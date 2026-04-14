//! GPU renderer implementation
//!
//! The main renderer that manages wgpu resources and executes render passes
//! for SDF primitives, glass effects, and text.
//!
//! ## A note on wasm32 + `Arc`
//!
//! On `wasm32-unknown-unknown` the wgpu API is single-threaded by design
//! (the WebGPU JavaScript interface lives on the main browser thread),
//! so `wgpu::Device` and `wgpu::Queue` are `!Send + !Sync`. Wrapping them
//! in `Arc` is still the right call — every other Blinc subsystem uses
//! `Arc<Device>` / `Arc<Queue>` to share GPU handles, and the
//! alternative (per-target `Rc` vs `Arc` aliases) would leak through
//! every storage site in `blinc_gpu`, `blinc_app::context`, the text
//! renderer, etc. Clippy's `arc_with_non_send_sync` lint catches the
//! theoretical footgun that an `Arc` of a `!Send` type can never
//! actually be sent across threads, but on wasm32 there are no other
//! threads to send to. The lint is `allow`ed at the module level for
//! that target only.

#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]

use std::collections::HashMap;
use std::sync::Arc;

use wgpu::util::DeviceExt;

use crate::gradient_texture::GradientTextureCache;
use crate::image::GpuImageInstance;
use crate::path::PathVertex;
use crate::primitives::{
    BlurUniforms, ColorMatrixUniforms, DropShadowUniforms, GlassType, GlassUniforms, GlowUniforms,
    GpuGlassPrimitive, GpuGlyph, GpuPrimitive, MaskImageUniforms, PathUniforms, PrimitiveBatch,
    Sdf3DUniform, SdfPipelineCategory, SdfVertexInstance, Uniforms, Viewport3D,
};
use crate::shaders::{
    BLUR_SHADER, COLOR_MATRIX_SHADER, COMPOSITE_SHADER, DROP_SHADOW_SHADER, GLASS_DT_SHADER,
    GLASS_SHADER, GLOW_SHADER, IMAGE_SHADER, LAYER_COMPOSITE_SHADER, MASK_IMAGE_SHADER,
    MESH_DT_SHADER, PATH_SHADER, SDF_3D_DT_SHADER, SDF_3D_SHADER, SDF_3D_VB_SHADER,
    SDF_CORE_DT_SHADER, SDF_CORE_SHADER, SDF_CORE_VB_SHADER, SDF_NOTCH_DT_SHADER, SDF_NOTCH_SHADER,
    SDF_NOTCH_VB_SHADER, SDF_SHADER, SDF_SHADOW_DT_SHADER, SDF_SHADOW_SHADER, SDF_SHADOW_VB_SHADER,
    SIMPLE_GLASS_DT_SHADER, SIMPLE_GLASS_SHADER, TEXT_DT_SHADER, TEXT_SHADER,
};

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
}

fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
}

fn device_required_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    // Default wgpu limits include `max_buffer_size = 256 MiB`.
    // This is conservative and may be smaller than what the hardware supports.
    //
    // If you want to raise this limit (e.g. for large path buffers), set:
    //   BLINC_WGPU_MAX_BUFFER_MB=512
    // The value is clamped to the adapter-supported maximum.
    let supported = adapter.limits();

    // On wasm32, use the adapter's own supported limits directly
    // instead of requesting wgpu defaults. Different browsers support
    // different subsets of WebGPU — Safari/Firefox may report 0 for
    // compute workgroups or storage buffer binding size. Requesting
    // any limit above what the adapter supports causes device creation
    // to fail. Using the adapter's limits verbatim is always safe.
    #[cfg(target_arch = "wasm32")]
    let mut limits = supported.clone();
    #[cfg(not(target_arch = "wasm32"))]
    let mut limits = wgpu::Limits::default();

    if let Some(mib) = env_u64("BLINC_WGPU_MAX_BUFFER_MB") {
        let requested = mib.saturating_mul(1024 * 1024);
        let clamped = requested.min(supported.max_buffer_size);
        limits.max_buffer_size = clamped;

        tracing::info!(
            "wgpu limits override: max_buffer_size={} MiB (requested {} MiB, supported {} MiB)",
            limits.max_buffer_size / (1024 * 1024),
            mib,
            supported.max_buffer_size / (1024 * 1024)
        );
    } else {
        tracing::debug!(
            "wgpu limits: max_buffer_size={} MiB (supported {} MiB)",
            limits.max_buffer_size / (1024 * 1024),
            supported.max_buffer_size / (1024 * 1024)
        );
    }

    limits
}

fn apply_renderer_config_overrides(
    mut config: RendererConfig,
    required_limits: &wgpu::Limits,
) -> RendererConfig {
    // Allow raising internal buffer capacities at startup.
    // These do NOT change hardware capabilities; they just size our storage buffers.
    //
    // Env:
    // - BLINC_GPU_MAX_PRIMITIVES=20000
    // - BLINC_GPU_MAX_GLYPHS=50000
    // - BLINC_GPU_MAX_GLASS_PRIMITIVES=1000
    if let Some(v) = env_usize("BLINC_GPU_MAX_PRIMITIVES") {
        config.max_primitives = v;
    }
    if let Some(v) = env_usize("BLINC_GPU_MAX_GLYPHS") {
        config.max_glyphs = v;
    }
    if let Some(v) = env_usize("BLINC_GPU_MAX_GLASS_PRIMITIVES") {
        config.max_glass_primitives = v;
    }

    // Clamp to required limits so device creation + bind sizes stay valid.
    let prim_cap = (required_limits.max_storage_buffer_binding_size as u64
        / std::mem::size_of::<GpuPrimitive>() as u64)
        .max(1) as usize;
    let glyph_cap = (required_limits.max_storage_buffer_binding_size as u64
        / std::mem::size_of::<GpuGlyph>() as u64)
        .max(1) as usize;
    let glass_cap = (required_limits.max_storage_buffer_binding_size as u64
        / std::mem::size_of::<GpuGlassPrimitive>() as u64)
        .max(1) as usize;

    config.max_primitives = config.max_primitives.clamp(1, prim_cap);
    config.max_glyphs = config.max_glyphs.clamp(1, glyph_cap);
    config.max_glass_primitives = config.max_glass_primitives.clamp(1, glass_cap);

    config
}

fn log_renderer_config(config: &RendererConfig) {
    tracing::info!(
        "gpu config: max_primitives={}, max_glyphs={}, max_glass_primitives={}, sample_count={}",
        config.max_primitives,
        config.max_glyphs,
        config.max_glass_primitives,
        config.sample_count
    );
}

/// Error type for renderer operations
#[derive(Debug)]
pub enum RendererError {
    /// Failed to request GPU adapter
    AdapterNotFound,
    /// Failed to request GPU device
    DeviceError(wgpu::RequestDeviceError),
    /// Failed to create surface
    SurfaceError(wgpu::CreateSurfaceError),
    /// Shader compilation error
    ShaderError(String),
}

impl std::fmt::Display for RendererError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RendererError::AdapterNotFound => write!(f, "No suitable GPU adapter found"),
            RendererError::DeviceError(e) => write!(f, "Failed to request GPU device: {}", e),
            RendererError::SurfaceError(e) => write!(f, "Failed to create surface: {}", e),
            RendererError::ShaderError(e) => write!(f, "Shader compilation error: {}", e),
        }
    }
}

impl std::error::Error for RendererError {}

/// Configuration for creating a renderer
#[derive(Clone, Debug)]
pub struct RendererConfig {
    /// Maximum number of primitives per batch
    pub max_primitives: usize,
    /// Maximum number of glass primitives per batch
    pub max_glass_primitives: usize,
    /// Maximum number of glyphs per batch
    pub max_glyphs: usize,
    /// Enable MSAA (sample count)
    pub sample_count: u32,
    /// Preferred texture format (None = use surface preferred)
    pub texture_format: Option<wgpu::TextureFormat>,
    /// Enable unified text/SDF rendering (renders text as SDF primitives in same pass)
    ///
    /// When enabled, text glyphs are converted to SDF primitives and rendered
    /// in the same GPU pass as other shapes. This ensures consistent transform
    /// timing during animations, preventing visual lag when parent containers
    /// have motion transforms applied.
    ///
    /// Default: true (unified rendering for consistent animations)
    pub unified_text_rendering: bool,
    /// GPU texture memory budget in bytes.
    ///
    /// When total tracked texture memory exceeds this budget, the renderer
    /// evicts least-recently-used textures from caches. Set to 0 to disable.
    ///
    /// Default: 128 MB. Override with `BLINC_GPU_MEMORY_BUDGET_MB` env var.
    pub gpu_memory_budget: u64,
}

impl Default for RendererConfig {
    fn default() -> Self {
        let budget_mb: u64 = std::env::var("BLINC_GPU_MEMORY_BUDGET_MB")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(128);

        Self {
            // Conservative defaults for low memory footprint
            // Buffers are re-created if scenes exceed these limits, so no hard cap
            max_primitives: 1_000,    // ~192 KB — handles complex UI screens
            max_glass_primitives: 32, // ~8 KB
            max_glyphs: 4_000,        // ~256 KB — handles full-screen text content
            sample_count: 1,
            texture_format: None,
            unified_text_rendering: true, // Enabled for consistent transforms during animations
            gpu_memory_budget: budget_mb * 1024 * 1024,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// GPU Memory Budget & Eviction
// ─────────────────────────────────────────────────────────────────────────────

/// Tracks GPU texture memory usage across all caches and enforces a budget.
pub struct GpuMemoryBudget {
    /// Maximum allowed texture memory in bytes (0 = unlimited)
    budget: u64,
    /// Memory used by mask image cache
    mask_image_bytes: u64,
    /// Memory used by mesh textures (transient, per-frame)
    mesh_texture_bytes: u64,
    /// Number of eviction passes performed
    eviction_count: u64,
}

impl GpuMemoryBudget {
    pub fn new(budget: u64) -> Self {
        Self {
            budget,
            mask_image_bytes: 0,
            mesh_texture_bytes: 0,
            eviction_count: 0,
        }
    }

    /// Report current total tracked memory across all sources.
    pub fn total_tracked_bytes(&self, layer_cache_bytes: u64) -> u64 {
        layer_cache_bytes + self.mask_image_bytes + self.mesh_texture_bytes
    }

    /// Check if we're over budget.
    pub fn is_over_budget(&self, layer_cache_bytes: u64) -> bool {
        self.budget > 0 && self.total_tracked_bytes(layer_cache_bytes) > self.budget
    }

    /// Track a mask image being added to the cache.
    pub fn track_mask_image(&mut self, width: u32, height: u32) {
        self.mask_image_bytes += (width as u64) * (height as u64) * 4;
    }

    /// Track a mask image being removed from the cache.
    pub fn untrack_mask_image(&mut self, width: u32, height: u32) {
        let bytes = (width as u64) * (height as u64) * 4;
        self.mask_image_bytes = self.mask_image_bytes.saturating_sub(bytes);
    }

    /// Reset per-frame transient tracking (mesh textures, etc.)
    pub fn reset_transient(&mut self) {
        self.mesh_texture_bytes = 0;
    }

    /// Get the memory budget in bytes.
    pub fn budget(&self) -> u64 {
        self.budget
    }

    /// Get number of eviction passes performed.
    pub fn eviction_count(&self) -> u64 {
        self.eviction_count
    }

    /// Increment eviction counter.
    pub fn record_eviction(&mut self) {
        self.eviction_count += 1;
    }
}

/// Render pipelines for different primitive types
struct Pipelines {
    /// Pipeline for SDF primitives (rects, circles, etc.) — monolithic fallback (deprecated)
    #[allow(dead_code)]
    sdf: wgpu::RenderPipeline,
    /// Pipeline for SDF primitives rendering on top of existing content (1x sampled) — monolithic fallback (deprecated)
    #[allow(dead_code)]
    sdf_overlay: wgpu::RenderPipeline,
    /// Split SDF pipeline: core shapes (Rect, Circle, Ellipse)
    sdf_core: wgpu::RenderPipeline,
    /// Split SDF pipeline: shadow shapes (Shadow, InnerShadow, CircleShadow, CircleInnerShadow)
    sdf_shadow: wgpu::RenderPipeline,
    /// Split SDF pipeline: 3D raymarched shapes
    sdf_3d: wgpu::RenderPipeline,
    /// Split SDF pipeline: notch shapes
    sdf_notch: wgpu::RenderPipeline,
    /// Split SDF overlay pipeline: core shapes (1x sampled)
    sdf_core_overlay: wgpu::RenderPipeline,
    /// Split SDF overlay pipeline: shadow shapes (1x sampled)
    sdf_shadow_overlay: wgpu::RenderPipeline,
    /// Split SDF overlay pipeline: 3D raymarched shapes (1x sampled)
    sdf_3d_overlay: wgpu::RenderPipeline,
    /// Split SDF overlay pipeline: notch shapes (1x sampled)
    sdf_notch_overlay: wgpu::RenderPipeline,
    /// Pipeline for text rendering (MSAA)
    #[allow(dead_code)]
    text: wgpu::RenderPipeline,
    /// Pipeline for text rendering on top of existing content (1x sampled)
    text_overlay: wgpu::RenderPipeline,
    /// Pipeline for final compositing (MSAA)
    #[allow(dead_code)]
    composite: wgpu::RenderPipeline,
    /// Pipeline for final compositing (1x sampled, for overlay blending)
    composite_overlay: wgpu::RenderPipeline,
    /// Pipeline for tessellated path rendering
    path: wgpu::RenderPipeline,
    /// Pipeline for tessellated path overlay (1x sampled)
    path_overlay: wgpu::RenderPipeline,
    /// Pipeline for layer composition (blend modes)
    layer_composite: wgpu::RenderPipeline,
}

/// Effect pipelines lazily created on first use to reduce GPU memory for simple apps
struct EffectPipelines {
    /// Pipeline for Kawase blur effect
    blur: Option<wgpu::RenderPipeline>,
    /// Pipeline for color matrix transformation
    color_matrix: Option<wgpu::RenderPipeline>,
    /// Pipeline for drop shadow effect
    drop_shadow: Option<wgpu::RenderPipeline>,
    /// Pipeline for glow effect
    glow: Option<wgpu::RenderPipeline>,
    /// Pipeline for mask image effect
    mask_image: Option<wgpu::RenderPipeline>,
    /// Pipeline for glass/vibrancy effects (liquid glass with refraction)
    glass: Option<wgpu::RenderPipeline>,
    /// Pipeline for simple frosted glass (pure blur, no refraction)
    simple_glass: Option<wgpu::RenderPipeline>,
}

/// Cached MSAA pipelines for dynamic sample counts
struct MsaaPipelines {
    /// SDF pipeline for this sample count (monolithic fallback, deprecated)
    #[allow(dead_code)]
    sdf: wgpu::RenderPipeline,
    /// Split SDF MSAA pipeline: core shapes
    sdf_core: wgpu::RenderPipeline,
    /// Split SDF MSAA pipeline: shadow shapes
    sdf_shadow: wgpu::RenderPipeline,
    /// Split SDF MSAA pipeline: 3D raymarched shapes
    sdf_3d: wgpu::RenderPipeline,
    /// Split SDF MSAA pipeline: notch shapes
    sdf_notch: wgpu::RenderPipeline,
    /// Path pipeline for this sample count
    path: wgpu::RenderPipeline,
    /// Sample count these pipelines were created for
    sample_count: u32,
}

/// GPU buffers for rendering
struct Buffers {
    /// Uniform buffer for viewport size
    uniforms: wgpu::Buffer,
    /// Storage buffer for SDF primitives
    primitives: wgpu::Buffer,
    /// Storage buffer for glass primitives
    glass_primitives: wgpu::Buffer,
    /// Uniform buffer for glass shader
    glass_uniforms: wgpu::Buffer,
    /// Storage buffer for text glyphs
    #[allow(dead_code)]
    glyphs: wgpu::Buffer,
    /// Uniform buffer for path rendering
    path_uniforms: wgpu::Buffer,
    /// Vertex buffer for path geometry (dynamic, recreated as needed)
    path_vertices: Option<wgpu::Buffer>,
    /// Index buffer for path geometry (dynamic, recreated as needed)
    path_indices: Option<wgpu::Buffer>,
    /// Pre-allocated uniform buffers for multi-pass blur (one per pass, max 8) — lazily created
    blur_uniforms_pool: Option<Vec<wgpu::Buffer>>,
    /// Cached uniform buffer for drop shadow effect — lazily created
    drop_shadow_uniforms: Option<wgpu::Buffer>,
    /// Cached uniform buffer for glow effect — lazily created
    glow_uniforms: Option<wgpu::Buffer>,
    /// Cached uniform buffer for color matrix effect — lazily created
    color_matrix_uniforms: Option<wgpu::Buffer>,
    /// Storage buffer for auxiliary per-primitive data (group shapes, polygon clips)
    aux_data: wgpu::Buffer,
    /// Instance vertex buffer for VERTEX_STORAGE fallback (WebGL2).
    /// Created/resized on demand when the adapter lacks storage buffers in
    /// vertex shaders.
    sdf_vertex_instances: Option<wgpu::Buffer>,
    /// Data texture for primitive data (WebGL2 fallback when no storage buffers).
    /// Width = 23 texels (one per vec4 field of GpuPrimitive), height = max_primitives.
    /// Format: Rgba32Float.
    prim_data_texture: Option<wgpu::Texture>,
    prim_data_view: Option<wgpu::TextureView>,
    /// Data texture for auxiliary data (WebGL2 fallback when no storage buffers).
    /// Width = 1024 texels, height grows on demand. Format: Rgba32Float.
    aux_data_texture: Option<wgpu::Texture>,
    aux_data_view: Option<wgpu::TextureView>,
    /// Current height of the aux data texture (for resize detection)
    aux_data_texture_height: u32,
    /// Data texture for glyph data (WebGL2 fallback).
    /// Width = 6 texels (one per vec4 field of GpuGlyph), height = max_glyphs.
    glyph_data_texture: Option<wgpu::Texture>,
    glyph_data_view: Option<wgpu::TextureView>,
}

/// Bind groups for shader resources
struct BindGroups {
    /// Bind group for SDF pipeline
    sdf: wgpu::BindGroup,
    /// Bind group for glass pipeline (needs backdrop texture)
    glass: Option<wgpu::BindGroup>,
    /// Bind group for path pipeline
    path: wgpu::BindGroup,
}

/// Cached MSAA textures and resources for overlay rendering
struct CachedMsaaTextures {
    msaa_texture: wgpu::Texture,
    msaa_view: wgpu::TextureView,
    resolve_texture: wgpu::Texture,
    resolve_view: wgpu::TextureView,
    width: u32,
    height: u32,
    sample_count: u32,
    /// Sampler for compositing (reused across frames)
    sampler: wgpu::Sampler,
    /// Uniform buffer for compositing (reused across frames)
    composite_uniform_buffer: wgpu::Buffer,
    /// Bind group for compositing (recreated when textures change)
    composite_bind_group: wgpu::BindGroup,
}

/// Cached glass resources to avoid per-frame allocations
struct CachedGlassResources {
    /// Sampler for backdrop texture (reused across frames)
    sampler: wgpu::Sampler,
    /// Cached bind group (valid when backdrop texture hasn't changed)
    bind_group: Option<wgpu::BindGroup>,
    /// Width/height when bind group was created (for invalidation)
    bind_group_size: (u32, u32),
}

/// Cached text resources to avoid per-frame allocations
struct CachedTextResources {
    /// Cached bind group (valid when atlas texture view hasn't changed)
    bind_group: wgpu::BindGroup,
    /// Pointer to grayscale atlas view when bind group was created (for invalidation)
    atlas_view_ptr: *const wgpu::TextureView,
    /// Pointer to color atlas view when bind group was created (for invalidation)
    color_atlas_view_ptr: *const wgpu::TextureView,
}

/// Active glyph atlas pointers for SDF bind group (set per-frame).
///
/// When CSS-transformed text is present, the real glyph atlas textures are bound
/// into `self.bind_groups.sdf` instead of the placeholder textures. These pointers
/// track the currently-bound atlas views so that `rebind_sdf_bind_group()` (called
/// during aux buffer resize) can recreate the bind group with the real atlas.
///
/// SAFETY: Pointers are valid for the duration of a frame — they point to TextureViews
/// owned by the text context, which outlives all render calls within a frame.
struct ActiveGlyphAtlas {
    atlas_view_ptr: *const wgpu::TextureView,
    color_atlas_view_ptr: *const wgpu::TextureView,
}

/// Cached resources for SDF 3D raymarching viewports
struct Sdf3DResources {
    /// Bind group layout for SDF 3D uniforms
    bind_group_layout: wgpu::BindGroupLayout,
    /// Uniform buffer for SDF 3D uniforms
    uniform_buffer: wgpu::Buffer,
    /// Bind group for SDF 3D uniforms
    bind_group: wgpu::BindGroup,
    /// Cached pipelines keyed by shader hash
    pipeline_cache: HashMap<u64, wgpu::RenderPipeline>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Layer Texture Management
// ─────────────────────────────────────────────────────────────────────────────

/// A texture used for offscreen layer rendering
///
/// Layer textures are used for rendering layers to offscreen targets,
/// enabling layer composition with blend modes and effects.
pub struct LayerTexture {
    /// The GPU texture for color data
    pub texture: wgpu::Texture,
    /// View into the texture for rendering
    pub view: wgpu::TextureView,
    /// Size of the texture in pixels (width, height)
    pub size: (u32, u32),
    /// Whether this texture has an associated depth buffer
    pub has_depth: bool,
    /// Optional depth texture view (for 3D content)
    pub depth_view: Option<wgpu::TextureView>,
    /// Optional depth texture (kept alive for the view)
    depth_texture: Option<wgpu::Texture>,
}

impl LayerTexture {
    /// Create a new layer texture with the given size
    pub fn new(
        device: &wgpu::Device,
        size: (u32, u32),
        format: wgpu::TextureFormat,
        with_depth: bool,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("layer_texture"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let (depth_texture, depth_view) = if with_depth {
            let depth_tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("layer_depth_texture"),
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let depth_view = depth_tex.create_view(&wgpu::TextureViewDescriptor::default());
            (Some(depth_tex), Some(depth_view))
        } else {
            (None, None)
        };

        Self {
            texture,
            view,
            size,
            has_depth: with_depth,
            depth_view,
            depth_texture,
        }
    }

    /// Check if this texture matches the requested size
    pub fn matches_size(&self, size: (u32, u32)) -> bool {
        self.size == size
    }
}

/// Size bucket for texture pooling
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureSizeBucket {
    Small,  // <= 128
    Medium, // <= 256
    Large,  // <= 512
    XLarge, // > 512 (not pooled by default)
}

impl TextureSizeBucket {
    /// Get the bucket for a given size
    fn from_size(size: (u32, u32)) -> Self {
        let max_dim = size.0.max(size.1);
        if max_dim <= 128 {
            Self::Small
        } else if max_dim <= 256 {
            Self::Medium
        } else if max_dim <= 512 {
            Self::Large
        } else {
            Self::XLarge
        }
    }

    /// Get the maximum size for this bucket (for rounding up)
    fn max_size(&self) -> u32 {
        match self {
            Self::Small => 128,
            Self::Medium => 256,
            Self::Large => 512,
            Self::XLarge => u32::MAX,
        }
    }
}

/// Statistics for texture cache performance monitoring
#[derive(Debug, Default, Clone)]
pub struct TextureCacheStats {
    /// Number of cache hits (texture reused from pool)
    pub hits: u64,
    /// Number of cache misses (new texture allocated)
    pub misses: u64,
    /// Number of textures currently in pool
    pub pool_count: usize,
    /// Estimated memory in pool (bytes)
    pub pool_memory_bytes: u64,
    /// Number of named textures
    pub named_count: usize,
    /// Estimated memory in named textures (bytes)
    pub named_memory_bytes: u64,
}

impl TextureCacheStats {
    /// Total estimated memory usage
    pub fn total_memory_bytes(&self) -> u64 {
        self.pool_memory_bytes + self.named_memory_bytes
    }

    /// Cache hit rate (0.0 - 1.0)
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// Cache for managing layer textures with size-bucketed pooling
///
/// Implements texture pooling to avoid frequent allocations during rendering.
/// Textures are acquired for layer rendering and released back to the pool
/// when no longer needed. Uses size buckets for more efficient reuse.
pub struct LayerTextureCache {
    /// Map of layer IDs to their dedicated textures
    named_textures: std::collections::HashMap<blinc_core::LayerId, LayerTexture>,
    /// Size-bucketed pools for efficient texture reuse
    pool_small: Vec<LayerTexture>, // <= 128
    pool_medium: Vec<LayerTexture>, // <= 256
    pool_large: Vec<LayerTexture>,  // <= 512
    pool_xlarge: Vec<LayerTexture>, // > 512
    /// Texture format used for all layer textures
    format: wgpu::TextureFormat,
    /// Maximum textures per bucket
    max_per_bucket: usize,
    /// Cache statistics
    stats: TextureCacheStats,
}

impl LayerTextureCache {
    /// Create a new layer texture cache
    pub fn new(format: wgpu::TextureFormat) -> Self {
        Self {
            named_textures: std::collections::HashMap::new(),
            pool_small: Vec::with_capacity(2),
            pool_medium: Vec::with_capacity(2),
            pool_large: Vec::with_capacity(2),
            pool_xlarge: Vec::with_capacity(2),
            format,
            max_per_bucket: 2,
            stats: TextureCacheStats::default(),
        }
    }

    /// Estimate memory usage of a texture in bytes (RGBA8 = 4 bytes per pixel)
    fn estimate_texture_bytes(size: (u32, u32), has_depth: bool) -> u64 {
        let color_bytes = (size.0 as u64) * (size.1 as u64) * 4;
        let depth_bytes = if has_depth {
            (size.0 as u64) * (size.1 as u64) * 4 // Depth32Float = 4 bytes
        } else {
            0
        };
        color_bytes + depth_bytes
    }

    /// Get the appropriate pool for a bucket
    fn get_pool(&self, bucket: TextureSizeBucket) -> &Vec<LayerTexture> {
        match bucket {
            TextureSizeBucket::Small => &self.pool_small,
            TextureSizeBucket::Medium => &self.pool_medium,
            TextureSizeBucket::Large => &self.pool_large,
            TextureSizeBucket::XLarge => &self.pool_xlarge,
        }
    }

    /// Get mutable pool for a bucket
    fn get_pool_mut(&mut self, bucket: TextureSizeBucket) -> &mut Vec<LayerTexture> {
        match bucket {
            TextureSizeBucket::Small => &mut self.pool_small,
            TextureSizeBucket::Medium => &mut self.pool_medium,
            TextureSizeBucket::Large => &mut self.pool_large,
            TextureSizeBucket::XLarge => &mut self.pool_xlarge,
        }
    }

    /// Acquire a texture of at least the given size
    ///
    /// First checks the pool for a matching texture, otherwise creates a new one.
    /// Textures may be larger than requested (rounded up to bucket size).
    pub fn acquire(
        &mut self,
        device: &wgpu::Device,
        size: (u32, u32),
        with_depth: bool,
    ) -> LayerTexture {
        let bucket = TextureSizeBucket::from_size(size);

        // Helper to find a matching texture in a pool
        fn find_matching(
            pool: &[LayerTexture],
            size: (u32, u32),
            with_depth: bool,
        ) -> Option<usize> {
            pool.iter()
                .position(|t| t.size.0 >= size.0 && t.size.1 >= size.1 && t.has_depth == with_depth)
        }

        // Try to find in primary bucket
        let primary_pool = match bucket {
            TextureSizeBucket::Small => &self.pool_small,
            TextureSizeBucket::Medium => &self.pool_medium,
            TextureSizeBucket::Large => &self.pool_large,
            TextureSizeBucket::XLarge => &self.pool_xlarge,
        };
        let found_in_primary = find_matching(primary_pool, size, with_depth);

        if let Some(index) = found_in_primary {
            self.stats.hits += 1;
            let texture = match bucket {
                TextureSizeBucket::Small => self.pool_small.swap_remove(index),
                TextureSizeBucket::Medium => self.pool_medium.swap_remove(index),
                TextureSizeBucket::Large => self.pool_large.swap_remove(index),
                TextureSizeBucket::XLarge => self.pool_xlarge.swap_remove(index),
            };
            self.update_pool_stats();
            return texture;
        }

        // Try larger buckets as fallback
        let found_in_larger = match bucket {
            TextureSizeBucket::Small => find_matching(&self.pool_medium, size, with_depth)
                .map(|i| (TextureSizeBucket::Medium, i))
                .or_else(|| {
                    find_matching(&self.pool_large, size, with_depth)
                        .map(|i| (TextureSizeBucket::Large, i))
                }),
            TextureSizeBucket::Medium => find_matching(&self.pool_large, size, with_depth)
                .map(|i| (TextureSizeBucket::Large, i)),
            _ => None,
        };

        if let Some((larger_bucket, index)) = found_in_larger {
            self.stats.hits += 1;
            let texture = match larger_bucket {
                TextureSizeBucket::Medium => self.pool_medium.swap_remove(index),
                TextureSizeBucket::Large => self.pool_large.swap_remove(index),
                _ => unreachable!(),
            };
            self.update_pool_stats();
            return texture;
        }

        // No suitable texture in pool, create a new one
        self.stats.misses += 1;

        // Round up for better future reuse
        let rounded_size = if bucket == TextureSizeBucket::XLarge {
            // Round XLarge to 64px increments for better cache reuse
            let w = size.0.div_ceil(64) * 64;
            let h = size.1.div_ceil(64) * 64;
            (w, h)
        } else {
            let bucket_max = bucket.max_size();
            (size.0.max(bucket_max), size.1.max(bucket_max))
        };

        LayerTexture::new(device, rounded_size, self.format, with_depth)
    }

    /// Release a texture back to the pool
    ///
    /// If the pool bucket is full or the texture is too large, it's dropped.
    pub fn release(&mut self, texture: LayerTexture) {
        let bucket = TextureSizeBucket::from_size(texture.size);
        let max = self.max_per_bucket;

        let pool = match bucket {
            TextureSizeBucket::Small => &mut self.pool_small,
            TextureSizeBucket::Medium => &mut self.pool_medium,
            TextureSizeBucket::Large => &mut self.pool_large,
            TextureSizeBucket::XLarge => &mut self.pool_xlarge,
        };

        if pool.len() < max {
            pool.push(texture);
            self.update_pool_stats();
        }
        // Otherwise let the texture be dropped
    }

    /// Update pool statistics
    fn update_pool_stats(&mut self) {
        let mut count = 0;
        let mut bytes = 0u64;

        for pool in [
            &self.pool_small,
            &self.pool_medium,
            &self.pool_large,
            &self.pool_xlarge,
        ] {
            for t in pool {
                count += 1;
                bytes += Self::estimate_texture_bytes(t.size, t.has_depth);
            }
        }

        self.stats.pool_count = count;
        self.stats.pool_memory_bytes = bytes;
    }

    /// Clear oversized textures from the pool
    ///
    /// Call this at frame start to evict any large textures that accumulated.
    pub fn evict_oversized(&mut self) {
        // Trim pools that are over capacity
        while self.pool_small.len() > self.max_per_bucket {
            self.pool_small.pop();
        }
        while self.pool_medium.len() > self.max_per_bucket {
            self.pool_medium.pop();
        }
        while self.pool_large.len() > self.max_per_bucket {
            self.pool_large.pop();
        }
        while self.pool_xlarge.len() > self.max_per_bucket {
            self.pool_xlarge.pop();
        }
        self.update_pool_stats();
    }

    /// Evict pooled textures until memory usage drops below `target_bytes`.
    ///
    /// Evicts largest textures first (XLarge → Large → Medium → Small).
    /// Returns the number of bytes freed.
    pub fn evict_to_budget(&mut self, target_bytes: u64) -> u64 {
        let mut freed = 0u64;
        let pools = [
            TextureSizeBucket::XLarge,
            TextureSizeBucket::Large,
            TextureSizeBucket::Medium,
            TextureSizeBucket::Small,
        ];

        for bucket in pools {
            while self.stats.pool_memory_bytes > target_bytes {
                let pool = self.get_pool_mut(bucket);
                if let Some(tex) = pool.pop() {
                    let bytes = Self::estimate_texture_bytes(tex.size, tex.has_depth);
                    freed += bytes;
                    self.update_pool_stats();
                } else {
                    break;
                }
            }
        }
        freed
    }

    /// Store a texture with a layer ID for later retrieval
    pub fn store(&mut self, id: blinc_core::LayerId, texture: LayerTexture) {
        self.named_textures.insert(id, texture);
        self.update_named_stats();
    }

    /// Get a reference to a named layer's texture
    pub fn get(&self, id: &blinc_core::LayerId) -> Option<&LayerTexture> {
        self.named_textures.get(id)
    }

    /// Remove and return a named layer's texture
    pub fn remove(&mut self, id: &blinc_core::LayerId) -> Option<LayerTexture> {
        let result = self.named_textures.remove(id);
        self.update_named_stats();
        result
    }

    /// Update named texture statistics
    fn update_named_stats(&mut self) {
        let mut bytes = 0u64;
        for t in self.named_textures.values() {
            bytes += Self::estimate_texture_bytes(t.size, t.has_depth);
        }
        self.stats.named_count = self.named_textures.len();
        self.stats.named_memory_bytes = bytes;
    }

    /// Clear all named textures (releases them to pool or drops them)
    pub fn clear_named(&mut self) {
        let textures: Vec<_> = self.named_textures.drain().map(|(_, t)| t).collect();
        for texture in textures {
            self.release(texture);
        }
        self.update_named_stats();
    }

    /// Clear the entire cache including pool
    pub fn clear_all(&mut self) {
        self.named_textures.clear();
        self.pool_small.clear();
        self.pool_medium.clear();
        self.pool_large.clear();
        self.pool_xlarge.clear();
        self.stats = TextureCacheStats::default();
    }

    /// Get the total number of textures in all pools
    pub fn pool_size(&self) -> usize {
        self.pool_small.len()
            + self.pool_medium.len()
            + self.pool_large.len()
            + self.pool_xlarge.len()
    }

    /// Get the number of named textures
    pub fn named_count(&self) -> usize {
        self.named_textures.len()
    }

    /// Get current cache statistics
    pub fn stats(&self) -> &TextureCacheStats {
        &self.stats
    }

    /// Reset cache statistics (call at start of profiling)
    pub fn reset_stats(&mut self) {
        self.stats.hits = 0;
        self.stats.misses = 0;
        self.update_pool_stats();
        self.update_named_stats();
    }
}

/// Primitive range boundaries for split SDF pipeline dispatch.
///
/// After sorting primitives by `SdfPipelineCategory`, each category
/// occupies a contiguous range in the GPU buffer. Text primitives are
/// tracked here for completeness but rendered by the separate text pipeline.
#[derive(Clone, Default)]
struct SdfPrimitiveRanges {
    core: std::ops::Range<u32>,
    shadow: std::ops::Range<u32>,
    sdf_3d: std::ops::Range<u32>,
    notch: std::ops::Range<u32>,
    text: std::ops::Range<u32>,
}

/// The GPU renderer using wgpu
///
/// This is the main rendering engine that:
/// - Manages wgpu device, queue, and surface
/// - Creates and manages render pipelines for different primitive types
/// - Batches primitives for efficient GPU rendering
/// - Executes render passes
pub struct GpuRenderer {
    /// wgpu instance
    #[allow(dead_code)]
    instance: wgpu::Instance,
    /// GPU adapter
    #[allow(dead_code)]
    adapter: wgpu::Adapter,
    /// GPU device
    device: Arc<wgpu::Device>,
    /// Command queue
    queue: Arc<wgpu::Queue>,
    /// Render pipelines
    pipelines: Pipelines,
    /// Effect pipelines (lazily created on first use)
    effect_pipelines: EffectPipelines,
    /// Cached MSAA pipelines for overlay rendering
    msaa_pipelines: Option<MsaaPipelines>,
    /// GPU buffers
    buffers: Buffers,
    /// Bind groups
    bind_groups: BindGroups,
    /// Bind group layouts
    bind_group_layouts: BindGroupLayouts,
    /// Current viewport size
    viewport_size: (u32, u32),
    /// Saved viewport size during offscreen rendering (for restore_viewport)
    saved_viewport_size: Option<(u32, u32)>,
    /// Renderer configuration
    config: RendererConfig,
    /// Current frame time (for animations)
    time: f32,
    /// Resolved texture format used by pipelines
    texture_format: wgpu::TextureFormat,
    /// Lazily-created image pipeline and resources
    image_pipeline: Option<ImagePipeline>,
    /// Lazily-created mesh rendering pipeline
    mesh_pipeline: Option<MeshPipeline>,
    /// User-registered custom render passes
    custom_passes: crate::custom_pass::CustomPassManager,
    /// GPU texture memory budget and tracking
    memory_budget: GpuMemoryBudget,
    /// Cached MSAA textures for overlay rendering (avoids per-frame allocation)
    cached_msaa: Option<CachedMsaaTextures>,
    /// Cached glass resources (avoids per-frame allocation)
    cached_glass: Option<CachedGlassResources>,
    /// Cached text resources (avoids per-frame allocation)
    cached_text: Option<CachedTextResources>,
    /// Placeholder glyph atlas texture view (1x1 transparent) for SDF bind group
    placeholder_glyph_atlas_view: wgpu::TextureView,
    /// Placeholder color glyph atlas texture view (1x1 transparent) for SDF bind group
    placeholder_color_glyph_atlas_view: wgpu::TextureView,
    /// Sampler for glyph atlas textures
    glyph_sampler: wgpu::Sampler,
    /// Active glyph atlas pointers — when set, `self.bind_groups.sdf` uses real atlas
    active_glyph_atlas: Option<ActiveGlyphAtlas>,
    /// Gradient texture cache for multi-stop gradient support on paths
    gradient_texture_cache: GradientTextureCache,
    /// Placeholder image texture (1x1 white) for path bind group when no image is used
    placeholder_path_image_view: wgpu::TextureView,
    /// Sampler for path image textures
    path_image_sampler: wgpu::Sampler,
    /// Layer texture cache for offscreen rendering and composition
    layer_texture_cache: LayerTextureCache,
    /// Cached resources for SDF 3D raymarching viewports (lazily initialized)
    sdf_3d_resources: Option<Sdf3DResources>,
    /// Cached particle systems for GPU particle rendering (keyed by hash of emitter config)
    particle_systems: std::collections::HashMap<u64, crate::particles::ParticleSystemGpu>,
    /// Cache of loaded mask images by URL/path
    mask_image_cache: HashMap<String, crate::image::GpuImage>,
    /// Dummy 1x1 texture view for blend mode dest binding when not needed (Normal mode)
    dummy_blend_dest_view: wgpu::TextureView,
    /// Dummy 1x1 texture for blend mode dest (needed for copy_texture_to_texture)
    dummy_blend_dest_texture: wgpu::Texture,
    /// Current render target texture pointer for blend mode two-pass compositing.
    /// Set via `set_blend_target()` before rendering, cleared after.
    /// Safety: Only valid during an active render frame.
    blend_target_ptr: Option<*const wgpu::Texture>,
    /// Cached @flow GPU pipelines (compiled lazily from FlowGraph → WGSL)
    flow_pipeline_cache: crate::flow_pipeline::FlowPipelineCache,
    /// Staging texture for scene capture (used by flow shaders with sample_scene()).
    /// Lazily created/resized to match the render target.
    scene_copy_texture: Option<(wgpu::Texture, wgpu::TextureView, u32, u32)>,
    /// Whether the GPU adapter supports storage buffers in vertex shaders.
    /// When `false`, SDF pipelines use an instance-stepped vertex buffer
    /// fallback (WebGL2 path).
    has_vertex_storage: bool,
    /// Whether the GPU adapter supports storage buffers at all
    /// (i.e. `max_storage_buffers_per_shader_stage > 0`).
    /// When `false`, the renderer uses data textures (Rgba32Float) to pass
    /// primitive and auxiliary data to fragment shaders instead of storage
    /// buffers. This is the Tier 3 / WebGL2 fallback path.
    has_storage_buffers: bool,
}

/// Image rendering pipeline (created lazily on first image render)
struct ImagePipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    instance_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
}

/// Shadow map resolution (square)
const SHADOW_MAP_SIZE: u32 = 2048;

/// Maximum number of distinct meshes whose GPU buffers / textures we
/// keep warm between frames. When exceeded, the FIFO eviction policy
/// drops the oldest entry. Sized conservatively — 128 fits every
/// asset in the workspace examples, scales to large editor scenes,
/// and caps worst-case GPU memory at ~`128 × per-mesh-footprint`.
const MESH_CACHE_CAPACITY: usize = 128;

/// Per-mesh cached vertex + index GPU buffers.
struct MeshBufferCacheEntry {
    vertex: wgpu::Buffer,
    index: wgpu::Buffer,
    index_count: u32,
}

/// Lazily-created 3D mesh rendering pipeline
struct MeshPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    material_buffer: wgpu::Buffer,
    /// Default 1x1 white texture (used when material has no texture)
    default_texture: crate::image::GpuImage,
    /// Default flat normal map (128, 128, 255 = tangent-space up)
    default_normal_map: crate::image::GpuImage,
    /// Default black displacement map (no displacement)
    default_displacement: crate::image::GpuImage,
    /// Default 1x1 white metallic-roughness texture. Bound when the
    /// material has no MR texture; multiplying scalar metallic ×
    /// roughness by (1,1,1,1) produces the scalar-only path.
    default_metallic_roughness: crate::image::GpuImage,
    /// Default 1x1 white emissive texture. Bound when the material
    /// has no emissive texture; the shader gates on the
    /// `has_emissive_texture` flag so the default is only read for
    /// layout validation.
    default_emissive: crate::image::GpuImage,
    /// Default 1x1 white occlusion texture. Same rationale as the
    /// other defaults.
    default_occlusion: crate::image::GpuImage,
    sampler: wgpu::Sampler,
    /// Storage buffer for joint matrices (skeletal animation, max 256 joints).
    /// Used when `has_storage_buffers` is true.
    joint_buffer: wgpu::Buffer,
    /// Data-texture fallback for joint matrices (WebGL2 DT mode).
    /// Width=4 (one texel per mat4 row), height=num_joints, RGBA32Float.
    joint_data_texture: Option<wgpu::Texture>,
    joint_data_view: Option<wgpu::TextureView>,
    /// Depth buffer for the main mesh pass. Separate from the shadow
    /// map — this one is sized to match the frame target so back faces
    /// and interior geometry z-test against front faces. Lazily
    /// created on the first render_mesh_data call and recreated when
    /// the viewport size changes.
    main_depth: Option<wgpu::Texture>,
    main_depth_view: Option<wgpu::TextureView>,
    main_depth_size: (u32, u32),
    /// Shadow depth pass pipeline
    shadow_pipeline: wgpu::RenderPipeline,
    shadow_bind_group_layout: wgpu::BindGroupLayout,
    shadow_uniform_buffer: wgpu::Buffer,
    /// 2048x2048 Depth32Float shadow map texture
    shadow_map: wgpu::Texture,
    shadow_view: wgpu::TextureView,
    /// Comparison sampler for PCF shadow sampling
    shadow_sampler: wgpu::Sampler,
    /// Procedural environment cubemap for IBL reflections. Generated
    /// once at pipeline init — a smooth sky gradient that gives metals
    /// and glass surfaces ambient reflections proportional to roughness.
    env_cubemap: wgpu::Texture,
    env_cubemap_view: wgpu::TextureView,
    env_sampler: wgpu::Sampler,
    /// Per-mesh vertex + index buffer cache. Keyed by the raw pointer
    /// of the mesh's `Arc<MeshData>`. Without this, `render_mesh_data`
    /// builds fresh `wgpu::Buffer`s every frame from the mesh's vertex
    /// / index `Vec`s — for a 39-mesh scene like buster_drone that's
    /// ~1.8 GB of vertex uploads per frame.
    ///
    /// Guarded by `cached_mesh_buffer_keys` for FIFO eviction so a
    /// long-running application that streams distinct meshes doesn't
    /// leak GPU memory. See `MESH_CACHE_CAPACITY`.
    cached_mesh_buffers: std::collections::HashMap<usize, MeshBufferCacheEntry>,
    cached_mesh_buffer_keys: std::collections::VecDeque<usize>,
    /// GPU texture cache, keyed by the raw pointer of the source
    /// `TextureData`'s `Arc<[u8]>` pixel buffer. Materials that reference
    /// the same underlying image share a single `GpuImage` instead of
    /// each mesh creating its own — critical because otherwise a
    /// 39-mesh asset with ~10 unique textures would upload ~195 GPU
    /// textures (GB of VRAM) instead of 10 (~few hundred MB).
    ///
    /// The companion `cached_gpu_image_keys` deque tracks insertion
    /// order for FIFO eviction at `MESH_CACHE_CAPACITY`.
    cached_gpu_images: std::collections::HashMap<usize, crate::image::GpuImage>,
    cached_gpu_image_keys: std::collections::VecDeque<usize>,
    /// Skybox pipeline — renders the environment cubemap as a
    /// background behind the mesh. Shares the cubemap texture/sampler
    /// but has its own bind group layout (camera vectors + cubemap).
    skybox_pipeline: wgpu::RenderPipeline,
    skybox_bind_group_layout: wgpu::BindGroupLayout,
    skybox_uniform_buffer: wgpu::Buffer,
    /// HDR intermediate texture (`Rgba16Float`). Meshes render here
    /// instead of the `Bgra8Unorm` framebuffer so specular + emissive
    /// values above 1.0 accumulate without clipping. The tonemap pass
    /// reads this and writes the tonemapped result to the frame target.
    hdr_texture: Option<wgpu::Texture>,
    hdr_view: Option<wgpu::TextureView>,
    hdr_size: (u32, u32),
    /// Fullscreen ACES tonemap pipeline + resources.
    tonemap_pipeline: wgpu::RenderPipeline,
    tonemap_bind_group_layout: wgpu::BindGroupLayout,
    tonemap_sampler: wgpu::Sampler,
    /// Bloom pipeline — shared for threshold-downsample and Kawase blur.
    bloom_pipeline: wgpu::RenderPipeline,
    bloom_bind_group_layout: wgpu::BindGroupLayout,
    bloom_uniform_buffer: wgpu::Buffer,
    /// Two half-res Rgba16Float ping-pong textures for bloom blur.
    bloom_a: Option<wgpu::Texture>,
    bloom_a_view: Option<wgpu::TextureView>,
    bloom_b: Option<wgpu::Texture>,
    bloom_b_view: Option<wgpu::TextureView>,
    bloom_size: (u32, u32),
}

struct BindGroupLayouts {
    sdf: wgpu::BindGroupLayout,
    glass: wgpu::BindGroupLayout,
    #[allow(dead_code)]
    text: wgpu::BindGroupLayout,
    #[allow(dead_code)]
    composite: wgpu::BindGroupLayout,
    path: wgpu::BindGroupLayout,
    /// Layout for layer composition shader
    layer_composite: wgpu::BindGroupLayout,
    /// Layout for blur effect shader
    blur: wgpu::BindGroupLayout,
    /// Layout for color matrix effect shader
    color_matrix: wgpu::BindGroupLayout,
    /// Layout for drop shadow effect shader
    drop_shadow: wgpu::BindGroupLayout,
    /// Layout for glow effect shader
    glow: wgpu::BindGroupLayout,
    /// Layout for mask image effect shader
    mask_image: wgpu::BindGroupLayout,
}

impl GpuRenderer {
    /// Whether the GPU adapter supports storage buffers.
    pub fn has_storage_buffers(&self) -> bool {
        self.has_storage_buffers
    }

    /// Get the preferred backend for the current platform
    ///
    /// Using the primary backend instead of all backends reduces memory usage
    /// by avoiding initialization of multiple GPU driver stacks.
    fn preferred_backends() -> wgpu::Backends {
        #[cfg(target_os = "macos")]
        {
            wgpu::Backends::METAL
        }
        #[cfg(target_os = "windows")]
        {
            wgpu::Backends::DX12
        }
        #[cfg(target_os = "linux")]
        {
            wgpu::Backends::VULKAN
        }
        #[cfg(target_arch = "wasm32")]
        {
            wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL
        }
        #[cfg(not(any(
            target_os = "macos",
            target_os = "windows",
            target_os = "linux",
            target_arch = "wasm32"
        )))]
        {
            wgpu::Backends::PRIMARY
        }
    }

    /// Safely write primitives to buffer, truncating if necessary to prevent overflow
    fn write_primitives_safe(&self, primitives: &[GpuPrimitive]) {
        if primitives.is_empty() {
            return;
        }
        let max_primitives = self.config.max_primitives;
        let primitives_to_write = if primitives.len() > max_primitives {
            tracing::warn!(
                "Primitive count {} exceeds buffer capacity {}, truncating",
                primitives.len(),
                max_primitives
            );
            &primitives[..max_primitives]
        } else {
            primitives
        };

        if !self.has_storage_buffers {
            // DT mode: upload to data texture instead of storage buffer.
            // Each GpuPrimitive is 23 × vec4<f32> = 23 RGBA32F texels in a row.
            if let Some(ref tex) = self.buffers.prim_data_texture {
                let bytes = bytemuck::cast_slice::<GpuPrimitive, u8>(primitives_to_write);
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        // 23 texels × 16 bytes per RGBA32F texel = 368 bytes per row
                        bytes_per_row: Some(23 * 16),
                        rows_per_image: None,
                    },
                    wgpu::Extent3d {
                        width: 23,
                        height: primitives_to_write.len() as u32,
                        depth_or_array_layers: 1,
                    },
                );
            }
        } else {
            // Tier 1/2: write to storage buffer as before
            self.queue.write_buffer(
                &self.buffers.primitives,
                0,
                bytemuck::cast_slice(primitives_to_write),
            );
        }
    }

    /// Sort primitives by `SdfPipelineCategory` and compute contiguous ranges.
    ///
    /// Returns a new sorted `Vec` and the corresponding `SdfPrimitiveRanges`.
    /// Text primitives are included in the sorted output (and tracked in ranges)
    /// but should NOT be drawn by the split SDF pipelines — they use the separate
    /// text pipeline.
    fn sort_primitives_by_category(
        primitives: &[GpuPrimitive],
    ) -> (Vec<GpuPrimitive>, SdfPrimitiveRanges) {
        if primitives.is_empty() {
            return (Vec::new(), SdfPrimitiveRanges::default());
        }
        let mut sorted: Vec<GpuPrimitive> = primitives.to_vec();
        sorted.sort_by_key(|p| p.pipeline_category());

        let mut ranges = SdfPrimitiveRanges::default();
        let mut i = 0u32;
        let len = sorted.len() as u32;
        while i < len {
            let cat = sorted[i as usize].pipeline_category();
            let start = i;
            while i < len && sorted[i as usize].pipeline_category() == cat {
                i += 1;
            }
            let range = start..i;
            match cat {
                SdfPipelineCategory::Core => ranges.core = range,
                SdfPipelineCategory::Shadow => ranges.shadow = range,
                SdfPipelineCategory::Sdf3D => ranges.sdf_3d = range,
                SdfPipelineCategory::Notch => ranges.notch = range,
                SdfPipelineCategory::Text => ranges.text = range,
            }
        }
        (sorted, ranges)
    }

    /// Sort primitives, upload to the GPU buffer (with safety truncation), and return ranges.
    ///
    /// When `has_vertex_storage` is `false`, also builds and uploads the
    /// `SdfVertexInstance` buffer used by the VB fallback shaders.
    fn upload_sorted_primitives(&mut self, primitives: &[GpuPrimitive]) -> SdfPrimitiveRanges {
        if primitives.is_empty() {
            return SdfPrimitiveRanges::default();
        }
        // Don't sort — preserve original z-order. Just scan for which
        // categories are present so we know which pipelines to activate.
        // Each split shader discards non-matching prim_types in fs_main.
        let mut ranges = SdfPrimitiveRanges::default();
        let len = primitives.len() as u32;
        let full = 0..len;
        for p in primitives {
            match p.pipeline_category() {
                SdfPipelineCategory::Core => {
                    if ranges.core.is_empty() {
                        ranges.core = full.clone();
                    }
                }
                SdfPipelineCategory::Shadow => {
                    if ranges.shadow.is_empty() {
                        ranges.shadow = full.clone();
                    }
                }
                SdfPipelineCategory::Sdf3D => {
                    if ranges.sdf_3d.is_empty() {
                        ranges.sdf_3d = full.clone();
                    }
                }
                SdfPipelineCategory::Notch => {
                    if ranges.notch.is_empty() {
                        ranges.notch = full.clone();
                    }
                }
                SdfPipelineCategory::Text => {
                    if ranges.text.is_empty() {
                        ranges.text = full.clone();
                    }
                }
            }
        }
        self.write_primitives_safe(primitives);

        // VERTEX_STORAGE fallback: build instance data and upload to VB
        if !self.has_vertex_storage {
            let instances: Vec<SdfVertexInstance> = primitives
                .iter()
                .map(SdfVertexInstance::from_primitive)
                .collect();
            let bytes = bytemuck::cast_slice::<SdfVertexInstance, u8>(&instances);
            let needed = bytes.len() as u64;

            // Create or resize the vertex buffer if necessary
            let needs_new_buffer = match &self.buffers.sdf_vertex_instances {
                Some(buf) => buf.size() < needed,
                None => true,
            };
            if needs_new_buffer {
                self.buffers.sdf_vertex_instances =
                    Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("SDF Vertex Instances (VB Fallback)"),
                        size: needed,
                        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }));
            }
            if let Some(buf) = &self.buffers.sdf_vertex_instances {
                self.queue.write_buffer(buf, 0, bytes);
            }
        }

        ranges
    }

    /// Returns the SDF vertex instance buffer for the VB fallback path,
    /// or `None` when VERTEX_STORAGE is supported.
    fn sdf_vb_buffer(&self) -> Option<&wgpu::Buffer> {
        if self.has_vertex_storage {
            None
        } else {
            self.buffers.sdf_vertex_instances.as_ref()
        }
    }

    /// Issue draw calls for split SDF pipelines using pre-computed ranges.
    ///
    /// The bind group must already be set on the render pass before calling this.
    /// If `overlay` is true, the overlay pipeline variants are used (1x sampled).
    /// When `vb_buffer` is `Some`, the instance vertex buffer is bound at slot 0
    /// before each pipeline draw (VERTEX_STORAGE fallback path).
    fn draw_split_sdf<'a>(
        render_pass: &mut wgpu::RenderPass<'a>,
        pipelines: &'a Pipelines,
        ranges: &SdfPrimitiveRanges,
        overlay: bool,
        vb_buffer: Option<&'a wgpu::Buffer>,
    ) {
        if let Some(buf) = vb_buffer {
            render_pass.set_vertex_buffer(0, buf.slice(..));
        }
        // Draw the full primitive range through each pipeline that has
        // matching primitives. Each split shader's fs_main checks prim_type
        // and discards non-matching instances. This preserves z-order
        // between categories (e.g., a notch parent at z=5 and its circle
        // child at z=6 render in the correct order).
        //
        // The total range spans from the lowest to highest instance index
        // across all non-empty categories.
        let total_start = [&ranges.core, &ranges.shadow, &ranges.sdf_3d, &ranges.notch]
            .iter()
            .filter(|r| !r.is_empty())
            .map(|r| r.start)
            .min();
        let total_end = [&ranges.core, &ranges.shadow, &ranges.sdf_3d, &ranges.notch]
            .iter()
            .filter(|r| !r.is_empty())
            .map(|r| r.end)
            .max();
        let full_range = match (total_start, total_end) {
            (Some(s), Some(e)) => s..e,
            _ => return,
        };

        // Draw order: Shadow → Notch → 3D → Core (back to front).
        // Shadows are backgrounds, notches are containers, core shapes
        // (rects/circles) are foreground. This ensures children drawn
        // by the core pipeline appear on top of notch parents.
        if !ranges.shadow.is_empty() {
            if overlay {
                render_pass.set_pipeline(&pipelines.sdf_shadow_overlay);
            } else {
                render_pass.set_pipeline(&pipelines.sdf_shadow);
            }
            render_pass.draw(0..6, full_range.clone());
        }
        if !ranges.notch.is_empty() {
            if overlay {
                render_pass.set_pipeline(&pipelines.sdf_notch_overlay);
            } else {
                render_pass.set_pipeline(&pipelines.sdf_notch);
            }
            render_pass.draw(0..6, full_range.clone());
        }
        if !ranges.sdf_3d.is_empty() {
            if overlay {
                render_pass.set_pipeline(&pipelines.sdf_3d_overlay);
            } else {
                render_pass.set_pipeline(&pipelines.sdf_3d);
            }
            render_pass.draw(0..6, full_range.clone());
        }
        if !ranges.core.is_empty() {
            if overlay {
                render_pass.set_pipeline(&pipelines.sdf_core_overlay);
            } else {
                render_pass.set_pipeline(&pipelines.sdf_core);
            }
            render_pass.draw(0..6, full_range.clone());
        }
        // Note: text range is NOT drawn here — text uses the separate text pipeline
    }

    /// Issue draw calls for split SDF pipelines using MSAA pipeline variants.
    ///
    /// Used by `render_overlay_msaa` where a specific sample count is in play.
    /// When `vb_buffer` is `Some`, the instance vertex buffer is bound at slot 0
    /// (VERTEX_STORAGE fallback path).
    fn draw_split_sdf_msaa<'a>(
        render_pass: &mut wgpu::RenderPass<'a>,
        msaa: &'a MsaaPipelines,
        ranges: &SdfPrimitiveRanges,
        vb_buffer: Option<&'a wgpu::Buffer>,
    ) {
        if let Some(buf) = vb_buffer {
            render_pass.set_vertex_buffer(0, buf.slice(..));
        }
        // Full range for z-order preservation (same approach as draw_split_sdf)
        let total_start = [&ranges.core, &ranges.shadow, &ranges.sdf_3d, &ranges.notch]
            .iter()
            .filter(|r| !r.is_empty())
            .map(|r| r.start)
            .min();
        let total_end = [&ranges.core, &ranges.shadow, &ranges.sdf_3d, &ranges.notch]
            .iter()
            .filter(|r| !r.is_empty())
            .map(|r| r.end)
            .max();
        let full_range = match (total_start, total_end) {
            (Some(s), Some(e)) => s..e,
            _ => return,
        };
        // Back-to-front: Shadow → Notch → 3D → Core
        if !ranges.shadow.is_empty() {
            render_pass.set_pipeline(&msaa.sdf_shadow);
            render_pass.draw(0..6, full_range.clone());
        }
        if !ranges.notch.is_empty() {
            render_pass.set_pipeline(&msaa.sdf_notch);
            render_pass.draw(0..6, full_range.clone());
        }
        if !ranges.sdf_3d.is_empty() {
            render_pass.set_pipeline(&msaa.sdf_3d);
            render_pass.draw(0..6, full_range.clone());
        }
        if !ranges.core.is_empty() {
            render_pass.set_pipeline(&msaa.sdf_core);
            render_pass.draw(0..6, full_range.clone());
        }
    }

    /// Create a new renderer without a surface (for headless rendering)
    pub async fn new(config: RendererConfig) -> Result<Self, RendererError> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: Self::preferred_backends(),
            ..Default::default()
        });

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|_| RendererError::AdapterNotFound)?;

        let required_limits = device_required_limits(&adapter);
        let config = apply_renderer_config_overrides(config, &required_limits);
        log_renderer_config(&config);

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Blinc GPU Device"),
                required_features: wgpu::Features::empty(),
                required_limits,
                // MemoryUsage hint tells the driver to prefer lower memory over performance.
                // This helps reduce RSS on integrated GPUs (Apple Silicon) where GPU memory
                // is shared with CPU and counts against process memory.
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(RendererError::DeviceError)?;

        let device = Arc::new(device);
        let queue = Arc::new(queue);

        // Default texture format for headless
        let texture_format = config
            .texture_format
            .unwrap_or(wgpu::TextureFormat::Bgra8UnormSrgb);

        Self::create_renderer(
            instance,
            adapter,
            device,
            queue,
            texture_format,
            config,
            (800, 600),
        )
    }

    /// Create a new renderer with a window surface
    pub async fn with_surface<W>(
        window: Arc<W>,
        config: RendererConfig,
    ) -> Result<(Self, wgpu::Surface<'static>), RendererError>
    where
        W: raw_window_handle::HasWindowHandle
            + raw_window_handle::HasDisplayHandle
            + Send
            + Sync
            + 'static,
    {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: Self::preferred_backends(),
            ..Default::default()
        });

        let surface = instance
            .create_surface(window.clone())
            .map_err(RendererError::SurfaceError)?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|_| RendererError::AdapterNotFound)?;

        let required_limits = device_required_limits(&adapter);
        let config = apply_renderer_config_overrides(config, &required_limits);
        log_renderer_config(&config);

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Blinc GPU Device"),
                required_features: wgpu::Features::empty(),
                required_limits,
                // MemoryUsage hint tells the driver to prefer lower memory over performance.
                // This helps reduce RSS on integrated GPUs (Apple Silicon) where GPU memory
                // is shared with CPU and counts against process memory.
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(RendererError::DeviceError)?;

        let device = Arc::new(device);
        let queue = Arc::new(queue);

        let surface_caps = surface.get_capabilities(&adapter);
        tracing::debug!("Surface capabilities - formats: {:?}", surface_caps.formats);
        tracing::debug!(
            "Surface capabilities - alpha modes: {:?}",
            surface_caps.alpha_modes
        );

        // Select texture format based on platform
        let texture_format = config.texture_format.unwrap_or_else(|| {
            // On macOS, prefer non-sRGB format to avoid automatic gamma correction
            // which causes colors to appear washed out. Other platforms may behave
            // differently, so we use sRGB there for now.
            #[cfg(target_os = "macos")]
            {
                surface_caps
                    .formats
                    .iter()
                    .find(|f| !f.is_srgb())
                    .copied()
                    .unwrap_or(surface_caps.formats[0])
            }
            #[cfg(not(target_os = "macos"))]
            {
                // On WebGL2 (GL adapter without storage buffers), prefer non-sRGB
                // to avoid double gamma correction — shaders output sRGB-encoded
                // colors directly, and an sRGB surface would apply gamma again.
                let prefer_non_srgb = adapter.limits().max_storage_buffers_per_shader_stage == 0;
                if prefer_non_srgb {
                    surface_caps
                        .formats
                        .iter()
                        .find(|f| !f.is_srgb())
                        .copied()
                        .unwrap_or(surface_caps.formats[0])
                } else {
                    surface_caps
                        .formats
                        .iter()
                        .find(|f| f.is_srgb())
                        .copied()
                        .unwrap_or(surface_caps.formats[0])
                }
            }
        });
        tracing::info!(
            "Selected texture format: {:?} (sRGB: {})",
            texture_format,
            texture_format.is_srgb()
        );

        let renderer = Self::create_renderer(
            instance,
            adapter,
            device,
            queue,
            texture_format,
            config,
            (800, 600),
        )?;

        Ok((renderer, surface))
    }

    /// Create a new renderer with a `<canvas>` element on `wasm32`.
    ///
    /// Mirrors [`Self::with_surface`] but takes a
    /// [`web_sys::HtmlCanvasElement`] instead of a raw-window-handle
    /// type, because `HtmlCanvasElement` doesn't (and can't) implement
    /// `HasWindowHandle` / `HasDisplayHandle` — the browser exposes its
    /// surface through `wgpu::SurfaceTarget::Canvas` instead.
    ///
    /// The texture format is selected from the browser-reported surface
    /// capabilities, preferring an sRGB format. WebGPU's canonical
    /// preferred format on Chrome is `Bgra8UnormSrgb`, but Safari
    /// Technology Preview reports only `Rgba8UnormSrgb` — the
    /// `find(is_srgb)` lookup handles both.
    ///
    /// # Browser availability
    ///
    /// Requires WebGPU (Chrome ≥ 113, Edge ≥ 113, Safari Technology
    /// Preview, Firefox Nightly with the WebGPU flag). The `web` feature
    /// also enables the `webgl` backend so wgpu can fall back to WebGL2
    /// where WebGPU isn't available, but the fallback path will reject
    /// some Blinc shader features (storage buffers in particular).
    #[cfg(target_arch = "wasm32")]
    pub async fn with_canvas(
        canvas: web_sys::HtmlCanvasElement,
        config: RendererConfig,
    ) -> Result<(Self, wgpu::Surface<'static>), RendererError> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: Self::preferred_backends(),
            ..Default::default()
        });

        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
            .map_err(RendererError::SurfaceError)?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|_| RendererError::AdapterNotFound)?;

        // Check that the adapter supports storage buffers in vertex
        // shaders — Blinc's SDF pipeline requires this (the primitives
        // buffer is `var<storage, read>` accessed from both vertex and
        // fragment stages). Skip on wasm32: the WebGPU spec guarantees
        // storage buffer support, but wgpu's downlevel report can be
        // wrong when the GL fallback adapter is selected (WebGL2 lacks
        // VERTEX_STORAGE, producing a false negative even though the
        // browser's WebGPU backend supports it).
        #[cfg(not(target_arch = "wasm32"))]
        {
            let downlevel = adapter.get_downlevel_capabilities();
            if !downlevel
                .flags
                .contains(wgpu::DownlevelFlags::VERTEX_STORAGE)
            {
                return Err(RendererError::ShaderError(
                    "GPU adapter does not support storage buffers in vertex shaders \
                     (VERTEX_STORAGE). Blinc requires this feature for its SDF \
                     rendering pipeline."
                        .to_string(),
                ));
            }
        }

        let required_limits = device_required_limits(&adapter);
        let config = apply_renderer_config_overrides(config, &required_limits);
        log_renderer_config(&config);

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Blinc GPU Device (Web)"),
                required_features: wgpu::Features::empty(),
                required_limits,
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(RendererError::DeviceError)?;

        let device = Arc::new(device);
        let queue = Arc::new(queue);

        let surface_caps = surface.get_capabilities(&adapter);
        tracing::debug!(
            "Web surface capabilities - formats: {:?}",
            surface_caps.formats
        );
        tracing::debug!(
            "Web surface capabilities - alpha modes: {:?}",
            surface_caps.alpha_modes
        );

        // On WebGPU (Chrome/Safari), prefer sRGB — the browser pipeline
        // expects sRGB output. On WebGL2 (GL adapter, no storage buffers),
        // prefer non-sRGB — shaders output sRGB-encoded colors directly,
        // and an sRGB surface would apply gamma encoding again (washed out).
        let is_gl_adapter = adapter.limits().max_storage_buffers_per_shader_stage == 0;
        let texture_format = config.texture_format.unwrap_or_else(|| {
            if is_gl_adapter {
                surface_caps
                    .formats
                    .iter()
                    .find(|f| !f.is_srgb())
                    .copied()
                    .unwrap_or(surface_caps.formats[0])
            } else {
                surface_caps
                    .formats
                    .iter()
                    .find(|f| f.is_srgb())
                    .copied()
                    .unwrap_or(surface_caps.formats[0])
            }
        });
        tracing::info!(
            "Web surface texture format: {:?} (sRGB: {}, GL adapter: {})",
            texture_format,
            texture_format.is_srgb(),
            is_gl_adapter
        );

        let renderer = Self::create_renderer(
            instance,
            adapter,
            device,
            queue,
            texture_format,
            config,
            (800, 600),
        )?;

        Ok((renderer, surface))
    }

    /// Create a new renderer with an existing wgpu instance and surface
    ///
    /// This is useful for platforms like Android where the surface is created
    /// from a native window handle before the renderer is initialized.
    pub async fn with_instance_and_surface(
        instance: wgpu::Instance,
        surface: &wgpu::Surface<'_>,
        config: RendererConfig,
    ) -> Result<Self, RendererError> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|_| RendererError::AdapterNotFound)?;

        let required_limits = device_required_limits(&adapter);
        let config = apply_renderer_config_overrides(config, &required_limits);
        log_renderer_config(&config);

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Blinc GPU Device"),
                required_features: wgpu::Features::empty(),
                required_limits,
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(RendererError::DeviceError)?;

        let device = Arc::new(device);
        let queue = Arc::new(queue);

        let surface_caps = surface.get_capabilities(&adapter);
        tracing::debug!("Surface capabilities - formats: {:?}", surface_caps.formats);

        // Select texture format based on platform
        let texture_format = config.texture_format.unwrap_or_else(|| {
            // On Android, prefer non-sRGB format to match macOS behavior
            // Using sRGB causes colors to appear washed out because the GPU
            // applies automatic gamma correction
            surface_caps
                .formats
                .iter()
                .find(|f| !f.is_srgb())
                .copied()
                .unwrap_or(surface_caps.formats[0])
        });
        tracing::info!("Surface formats available: {:?}", surface_caps.formats);
        tracing::info!("Selected texture format: {:?}", texture_format);

        Self::create_renderer(
            instance,
            adapter,
            device,
            queue,
            texture_format,
            config,
            (800, 600),
        )
    }

    fn create_renderer(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        texture_format: wgpu::TextureFormat,
        mut config: RendererConfig,
        viewport_size: (u32, u32),
    ) -> Result<Self, RendererError> {
        // Check if the adapter supports storage buffers in vertex shaders
        let has_vertex_storage = adapter
            .get_downlevel_capabilities()
            .flags
            .contains(wgpu::DownlevelFlags::VERTEX_STORAGE);
        if !has_vertex_storage {
            tracing::info!("VERTEX_STORAGE not supported — using instance vertex buffer fallback");
        }

        // Check if the adapter supports storage buffers at all (Tier 3 / DT fallback)
        let has_storage_buffers = adapter.limits().max_storage_buffers_per_shader_stage > 0;
        if !has_storage_buffers {
            tracing::info!("No storage buffer support — using data texture fallback (WebGL2 mode)");
            // When there are no storage buffers, max_storage_buffer_binding_size is 0,
            // so apply_renderer_config_overrides clamped max_primitives/max_glyphs to 1.
            // Re-apply sensible defaults clamped by texture dimension limits instead.
            let tex_max = adapter.limits().max_texture_dimension_2d as usize;
            let defaults = RendererConfig::default();
            // Use env overrides if present, otherwise fall back to defaults
            config.max_primitives = env_usize("BLINC_GPU_MAX_PRIMITIVES")
                .unwrap_or(defaults.max_primitives)
                .clamp(1, tex_max);
            config.max_glyphs = env_usize("BLINC_GPU_MAX_GLYPHS")
                .unwrap_or(defaults.max_glyphs)
                .clamp(1, tex_max);
            log_renderer_config(&config);
        }

        // Create bind group layouts
        let bind_group_layouts = Self::create_bind_group_layouts_with_flags(
            &device,
            has_vertex_storage,
            has_storage_buffers,
        );

        // Create shaders

        let text_source = if has_storage_buffers {
            TEXT_SHADER
        } else {
            TEXT_DT_SHADER
        };
        let text_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Text Shader"),
            source: wgpu::ShaderSource::Wgsl(text_source.into()),
        });

        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Composite Shader"),
            source: wgpu::ShaderSource::Wgsl(COMPOSITE_SHADER.into()),
        });

        let path_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Path Shader"),
            source: wgpu::ShaderSource::Wgsl(PATH_SHADER.into()),
        });

        let layer_composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Layer Composite Shader"),
            source: wgpu::ShaderSource::Wgsl(LAYER_COMPOSITE_SHADER.into()),
        });

        // Split SDF shaders — specialized pipelines for each primitive category.
        // Three tiers:
        //   Tier 1 (full): Storage buffers in VS + FS — sdf_*.wgsl
        //   Tier 2 (VB):   No VS storage, FS storage works — sdf_*_vb.wgsl + vertex buffer
        //   Tier 3 (DT):   No storage at all (WebGL2) — sdf_*_dt.wgsl + VB + data textures
        let (sdf_core_source, sdf_shadow_source, sdf_3d_source, sdf_notch_source) =
            if !has_storage_buffers {
                // Tier 3: Data texture fallback (no storage buffers at all)
                (
                    SDF_CORE_DT_SHADER,
                    SDF_SHADOW_DT_SHADER,
                    SDF_3D_DT_SHADER,
                    SDF_NOTCH_DT_SHADER,
                )
            } else if !has_vertex_storage {
                // Tier 2: VB fallback (no VS storage, FS storage works)
                (
                    SDF_CORE_VB_SHADER,
                    SDF_SHADOW_VB_SHADER,
                    SDF_3D_VB_SHADER,
                    SDF_NOTCH_VB_SHADER,
                )
            } else {
                // Tier 1: Full storage buffer support
                (
                    SDF_CORE_SHADER,
                    SDF_SHADOW_SHADER,
                    SDF_3D_SHADER,
                    SDF_NOTCH_SHADER,
                )
            };

        // The monolithic SDF_SHADER is no longer compiled — split pipelines
        // handle all primitive types. Use core shader as stand-in for the
        // dead-code monolithic pipeline fields in the Pipelines struct.
        let sdf_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("SDF Shader (stand-in — split pipelines active)"),
            source: wgpu::ShaderSource::Wgsl(sdf_core_source.into()),
        });

        let sdf_core_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("SDF Core Shader"),
            source: wgpu::ShaderSource::Wgsl(sdf_core_source.into()),
        });
        let sdf_shadow_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("SDF Shadow Shader"),
            source: wgpu::ShaderSource::Wgsl(sdf_shadow_source.into()),
        });
        let sdf_3d_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("SDF 3D Shader"),
            source: wgpu::ShaderSource::Wgsl(sdf_3d_source.into()),
        });
        let sdf_notch_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("SDF Notch Shader"),
            source: wgpu::ShaderSource::Wgsl(sdf_notch_source.into()),
        });

        // Create pipelines (core only — effect pipelines are lazy)
        let pipelines = Self::create_pipelines(
            &device,
            &bind_group_layouts,
            &sdf_shader,
            &sdf_core_shader,
            &sdf_shadow_shader,
            &sdf_3d_shader,
            &sdf_notch_shader,
            &text_shader,
            &composite_shader,
            &path_shader,
            &layer_composite_shader,
            texture_format,
            config.sample_count,
            has_vertex_storage,
        );

        // Create buffers (storage buffers always created; DT textures added when needed)
        let buffers = Self::create_buffers(&device, &config, has_storage_buffers);

        // Create placeholder glyph atlas textures (1x1 transparent)
        // These are used when no text is rendered, satisfying the bind group layout
        let placeholder_glyph_atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Placeholder Glyph Atlas"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm, // Grayscale for regular glyphs
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let placeholder_glyph_atlas_view =
            placeholder_glyph_atlas.create_view(&wgpu::TextureViewDescriptor::default());

        let placeholder_color_glyph_atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Placeholder Color Glyph Atlas"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb, // RGBA for color emoji
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let placeholder_color_glyph_atlas_view =
            placeholder_color_glyph_atlas.create_view(&wgpu::TextureViewDescriptor::default());

        // Create sampler for glyph atlases
        let glyph_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Glyph Atlas Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // Create gradient texture cache for multi-stop gradients on paths
        let gradient_texture_cache = GradientTextureCache::new(&device, &queue);

        // Create placeholder image texture for paths (1x1 white)
        let placeholder_path_image = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Placeholder Path Image"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // Initialize with white pixel
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &placeholder_path_image,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255u8, 255, 255, 255], // White pixel
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let placeholder_path_image_view =
            placeholder_path_image.create_view(&wgpu::TextureViewDescriptor::default());

        // Create sampler for path image textures
        let path_image_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Path Image Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // Create 1x1 dummy texture for blend mode dest binding (Normal mode doesn't read it)
        let dummy_blend_dest = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Dummy Blend Dest"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &dummy_blend_dest,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0u8, 0, 0, 0], // Transparent pixel
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let dummy_blend_dest_view =
            dummy_blend_dest.create_view(&wgpu::TextureViewDescriptor::default());

        // Create initial bind groups
        let bind_groups = Self::create_bind_groups(
            &device,
            &bind_group_layouts,
            &buffers,
            &placeholder_glyph_atlas_view,
            &placeholder_color_glyph_atlas_view,
            &glyph_sampler,
            &gradient_texture_cache,
            &placeholder_path_image_view,
            &path_image_sampler,
            has_storage_buffers,
        );

        let flow_pipeline_cache =
            crate::flow_pipeline::FlowPipelineCache::new(device.clone(), texture_format);

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            pipelines,
            effect_pipelines: EffectPipelines {
                blur: None,
                color_matrix: None,
                drop_shadow: None,
                glow: None,
                mask_image: None,
                glass: None,
                simple_glass: None,
            },
            msaa_pipelines: None,
            buffers,
            bind_groups,
            bind_group_layouts,
            viewport_size,
            saved_viewport_size: None,
            memory_budget: GpuMemoryBudget::new(config.gpu_memory_budget),
            config,
            time: 0.0,
            texture_format,
            image_pipeline: None,
            mesh_pipeline: None,
            custom_passes: crate::custom_pass::CustomPassManager::new(),
            cached_msaa: None,
            cached_glass: None,
            cached_text: None,
            placeholder_glyph_atlas_view,
            placeholder_color_glyph_atlas_view,
            glyph_sampler,
            active_glyph_atlas: None,
            gradient_texture_cache,
            placeholder_path_image_view,
            path_image_sampler,
            layer_texture_cache: LayerTextureCache::new(texture_format),
            sdf_3d_resources: None,
            particle_systems: std::collections::HashMap::new(),
            mask_image_cache: HashMap::new(),
            dummy_blend_dest_view,
            dummy_blend_dest_texture: dummy_blend_dest,
            blend_target_ptr: None,
            flow_pipeline_cache,
            scene_copy_texture: None,
            has_vertex_storage,
            has_storage_buffers,
        })
    }

    fn create_bind_group_layouts_with_flags(
        device: &wgpu::Device,
        has_vertex_storage: bool,
        has_storage_buffers: bool,
    ) -> BindGroupLayouts {
        // When VERTEX_STORAGE is available, the primitives storage buffer
        // is visible to both vertex and fragment stages. Otherwise, only
        // the fragment stage reads it — the vertex shader gets its data
        // from an instance-stepped vertex buffer instead.
        let primitives_visibility = if has_vertex_storage {
            wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT
        } else {
            wgpu::ShaderStages::FRAGMENT
        };

        // Binding 1 & 5: Storage buffers normally; data textures when no storage support
        let binding_1_entry = if has_storage_buffers {
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: primitives_visibility,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }
        } else {
            // DT mode: primitive data comes from an Rgba32Float texture.
            // VERTEX | FRAGMENT visibility needed because WGSL module-scope
            // bindings are validated against all entry points in the module,
            // even if only fs_main reads the texture. Texture bindings don't
            // require VERTEX_STORAGE (that flag only applies to storage buffers).
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }
        };

        let binding_5_entry = if has_storage_buffers {
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }
        } else {
            // DT mode: aux data comes from an Rgba32Float texture
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }
        };

        // SDF bind group layout (includes glyph atlas for unified text rendering)
        let sdf = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("SDF Bind Group Layout"),
            entries: &[
                // Uniforms
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Binding 1: Primitives (storage buffer or data texture)
                binding_1_entry,
                // Glyph atlas texture (grayscale text)
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Glyph sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Color glyph atlas texture (emoji)
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Binding 5: Auxiliary data (storage buffer or data texture)
                binding_5_entry,
            ],
        });

        // Glass bind group layout
        let glass = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Glass Bind Group Layout"),
            entries: &[
                // Uniforms
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Glass primitives: storage buffer (normal) or texture (WebGL2 DT fallback).
                // VERTEX | FRAGMENT in both modes — DT shader declares binding at module
                // scope, wgpu validates against all entry points.
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: if has_storage_buffers {
                        wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        }
                    } else {
                        wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        }
                    },
                    count: None,
                },
                // Backdrop texture
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Backdrop sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        // Text bind group layout
        let text = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Text Bind Group Layout"),
            entries: &[
                // Uniforms
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Glyphs: storage buffer (normal) or texture (WebGL2 DT fallback)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: if has_storage_buffers {
                        wgpu::ShaderStages::VERTEX
                    } else {
                        // DT mode: TEXT_DT_SHADER reads glyph_data texture in vs_main
                        wgpu::ShaderStages::VERTEX
                    },
                    ty: if has_storage_buffers {
                        wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        }
                    } else {
                        wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        }
                    },
                    count: None,
                },
                // Glyph atlas texture
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Glyph atlas sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Color glyph atlas texture (for emoji)
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        // Composite bind group layout
        let composite = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Composite Bind Group Layout"),
            entries: &[
                // Uniforms
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Source texture
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Source sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        // Path bind group layout (uniforms + gradient texture + image texture + backdrop for glass)
        let path = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Path Bind Group Layout"),
            entries: &[
                // Uniforms (viewport_size, transform, opacity, clip, glass params, etc.)
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Gradient texture (1D texture for multi-stop gradients)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D1,
                        multisampled: false,
                    },
                    count: None,
                },
                // Gradient sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Image texture (2D texture for image brush)
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Image sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Backdrop texture (2D texture for glass effect)
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Backdrop sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        // Layer composite bind group layout (for compositing offscreen layers)
        let layer_composite = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Layer Composite Bind Group Layout"),
            entries: &[
                // Uniforms (source_rect, dest_rect, viewport_size, opacity, blend_mode)
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Layer texture (source)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Layer sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Destination texture (for blend modes)
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Destination sampler (for blend modes)
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        // Blur effect bind group layout
        let blur = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Blur Effect Bind Group Layout"),
            entries: &[
                // BlurUniforms
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Input texture
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Input sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        // Color matrix effect bind group layout
        let color_matrix = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Color Matrix Effect Bind Group Layout"),
            entries: &[
                // ColorMatrixUniforms
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Input texture
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Input sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        // Drop shadow effect bind group layout
        let drop_shadow = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Drop Shadow Effect Bind Group Layout"),
            entries: &[
                // DropShadowUniforms
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Blurred input texture (for shadow alpha)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Input sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Original (unblurred) texture (for compositing)
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        // Glow effect bind group layout
        let glow = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Glow Effect Bind Group Layout"),
            entries: &[
                // GlowUniforms
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Source texture
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Input sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        // Mask image effect bind group layout
        let mask_image = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Mask Image Effect Bind Group Layout"),
            entries: &[
                // MaskUniforms
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Input (element) texture
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Input sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Mask texture
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Mask sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        BindGroupLayouts {
            sdf,
            glass,
            text,
            composite,
            path,
            layer_composite,
            blur,
            color_matrix,
            drop_shadow,
            glow,
            mask_image,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn create_pipelines(
        device: &wgpu::Device,
        layouts: &BindGroupLayouts,
        sdf_shader: &wgpu::ShaderModule,
        sdf_core_shader: &wgpu::ShaderModule,
        sdf_shadow_shader: &wgpu::ShaderModule,
        sdf_3d_shader: &wgpu::ShaderModule,
        sdf_notch_shader: &wgpu::ShaderModule,
        text_shader: &wgpu::ShaderModule,
        composite_shader: &wgpu::ShaderModule,
        path_shader: &wgpu::ShaderModule,
        layer_composite_shader: &wgpu::ShaderModule,
        texture_format: wgpu::TextureFormat,
        sample_count: u32,
        has_vertex_storage: bool,
    ) -> Pipelines {
        let blend_state = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };

        let color_targets = &[Some(wgpu::ColorTargetState {
            format: texture_format,
            blend: Some(blend_state),
            write_mask: wgpu::ColorWrites::ALL,
        })];

        let primitive_state = wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        };

        let multisample_state = wgpu::MultisampleState {
            count: sample_count,
            mask: !0,
            alpha_to_coverage_enabled: false,
        };

        // SDF pipeline
        let sdf_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("SDF Pipeline Layout"),
            bind_group_layouts: &[&layouts.sdf],
            push_constant_ranges: &[],
        });

        // When VERTEX_STORAGE is unavailable, SDF vertex shaders read from an
        // instance-stepped vertex buffer instead of the storage buffer.
        let sdf_vb_buffers: &[wgpu::VertexBufferLayout<'_>] = if has_vertex_storage {
            &[]
        } else {
            &[SdfVertexInstance::LAYOUT]
        };

        let sdf = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("SDF Pipeline"),
            layout: Some(&sdf_layout),
            vertex: wgpu::VertexState {
                module: sdf_shader,
                entry_point: Some("vs_main"),
                buffers: sdf_vb_buffers,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: sdf_shader,
                entry_point: Some("fs_main"),
                targets: color_targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: primitive_state,
            depth_stencil: None,
            multisample: multisample_state,
            multiview: None,
            cache: None,
        });

        // Overlay pipelines use sample_count=1 for rendering on resolved textures
        let overlay_multisample_state = wgpu::MultisampleState {
            count: 1,
            mask: !0,
            alpha_to_coverage_enabled: false,
        };

        let sdf_overlay = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("SDF Overlay Pipeline"),
            layout: Some(&sdf_layout),
            vertex: wgpu::VertexState {
                module: sdf_shader,
                entry_point: Some("vs_main"),
                buffers: sdf_vb_buffers,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: sdf_shader,
                entry_point: Some("fs_main"),
                targets: color_targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: primitive_state,
            depth_stencil: None,
            multisample: overlay_multisample_state,
            multiview: None,
            cache: None,
        });

        // --- Split SDF pipelines (share sdf_layout, same blend/primitive state) ---

        // Helper closure to create an SDF pipeline pair (MSAA + overlay) from a shader module
        let make_sdf_pipeline_pair = |shader: &wgpu::ShaderModule,
                                      label: &str|
         -> (wgpu::RenderPipeline, wgpu::RenderPipeline) {
            let msaa = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&sdf_layout),
                vertex: wgpu::VertexState {
                    module: shader,
                    entry_point: Some("vs_main"),
                    buffers: sdf_vb_buffers,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: shader,
                    entry_point: Some("fs_main"),
                    targets: color_targets,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: primitive_state,
                depth_stencil: None,
                multisample: multisample_state,
                multiview: None,
                cache: None,
            });
            let overlay_label = format!("{label} Overlay");
            let overlay = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&overlay_label),
                layout: Some(&sdf_layout),
                vertex: wgpu::VertexState {
                    module: shader,
                    entry_point: Some("vs_main"),
                    buffers: sdf_vb_buffers,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: shader,
                    entry_point: Some("fs_main"),
                    targets: color_targets,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: primitive_state,
                depth_stencil: None,
                multisample: overlay_multisample_state,
                multiview: None,
                cache: None,
            });
            (msaa, overlay)
        };

        let (sdf_core, sdf_core_overlay) =
            make_sdf_pipeline_pair(sdf_core_shader, "SDF Core Pipeline");
        let (sdf_shadow, sdf_shadow_overlay) =
            make_sdf_pipeline_pair(sdf_shadow_shader, "SDF Shadow Pipeline");
        let (sdf_3d, sdf_3d_overlay) = make_sdf_pipeline_pair(sdf_3d_shader, "SDF 3D Pipeline");
        let (sdf_notch, sdf_notch_overlay) =
            make_sdf_pipeline_pair(sdf_notch_shader, "SDF Notch Pipeline");

        // Text pipeline
        let text_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Text Pipeline Layout"),
            bind_group_layouts: &[&layouts.text],
            push_constant_ranges: &[],
        });

        // TEXT_DT_SHADER has its own vs_main that reads from a glyph data texture
        // (no VB instance attributes needed — unlike SDF DT which uses VB + DT).
        let text_vb_buffers: &[wgpu::VertexBufferLayout<'_>] = &[];

        let text = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Text Pipeline"),
            layout: Some(&text_layout),
            vertex: wgpu::VertexState {
                module: text_shader,
                entry_point: Some("vs_main"),
                buffers: text_vb_buffers,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: text_shader,
                entry_point: Some("fs_main"),
                targets: color_targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: primitive_state,
            depth_stencil: None,
            multisample: multisample_state,
            multiview: None,
            cache: None,
        });

        // Text overlay pipeline - uses sample_count=1 for rendering on resolved textures
        let text_overlay = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Text Overlay Pipeline"),
            layout: Some(&text_layout),
            vertex: wgpu::VertexState {
                module: text_shader,
                entry_point: Some("vs_main"),
                buffers: text_vb_buffers,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: text_shader,
                entry_point: Some("fs_main"),
                targets: color_targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: primitive_state,
            depth_stencil: None,
            multisample: overlay_multisample_state,
            multiview: None,
            cache: None,
        });

        // Composite pipeline
        let composite_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Composite Pipeline Layout"),
            bind_group_layouts: &[&layouts.composite],
            push_constant_ranges: &[],
        });

        let composite = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Composite Pipeline"),
            layout: Some(&composite_layout),
            vertex: wgpu::VertexState {
                module: composite_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: composite_shader,
                entry_point: Some("fs_main"),
                targets: color_targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: primitive_state,
            depth_stencil: None,
            multisample: multisample_state,
            multiview: None,
            cache: None,
        });

        // Composite overlay pipeline - single-sampled for blending onto resolved textures
        let composite_overlay = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Composite Overlay Pipeline"),
            layout: Some(&composite_layout),
            vertex: wgpu::VertexState {
                module: composite_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: composite_shader,
                entry_point: Some("fs_main"),
                targets: color_targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: primitive_state,
            depth_stencil: None,
            multisample: overlay_multisample_state,
            multiview: None,
            cache: None,
        });

        // Path pipeline - uses vertex buffers for tessellated geometry
        let path_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Path Pipeline Layout"),
            bind_group_layouts: &[&layouts.path],
            push_constant_ranges: &[],
        });

        // Vertex buffer layout for PathVertex
        // PathVertex layout (80 bytes total):
        //   position: [f32; 2]       - 8 bytes, offset 0
        //   color: [f32; 4]          - 16 bytes, offset 8
        //   end_color: [f32; 4]      - 16 bytes, offset 24
        //   uv: [f32; 2]             - 8 bytes, offset 40
        //   gradient_params: [f32;4] - 16 bytes, offset 48
        //   gradient_type: u32       - 4 bytes, offset 64
        //   edge_distance: f32       - 4 bytes, offset 68
        //   clip_bounds: [f32;4]     - 16 bytes, offset 72
        //   clip_radius: [f32;4]     - 16 bytes, offset 88
        //   clip_type: u32           - 4 bytes, offset 104
        //   _padding: [u32; 3]       - 12 bytes, offset 108
        let path_vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<PathVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                // position: vec2<f32>
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                },
                // color: vec4<f32>
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 8,
                    shader_location: 1,
                },
                // end_color: vec4<f32>
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 24,
                    shader_location: 2,
                },
                // uv: vec2<f32>
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 40,
                    shader_location: 3,
                },
                // gradient_params: vec4<f32>
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 48,
                    shader_location: 4,
                },
                // gradient_type: u32
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Uint32,
                    offset: 64,
                    shader_location: 5,
                },
                // edge_distance: f32 (for anti-aliasing)
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32,
                    offset: 68,
                    shader_location: 6,
                },
                // clip_bounds: vec4<f32>
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 72,
                    shader_location: 7,
                },
                // clip_radius: vec4<f32>
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 88,
                    shader_location: 8,
                },
                // clip_type: u32
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Uint32,
                    offset: 104,
                    shader_location: 9,
                },
            ],
        };

        let path = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Path Pipeline"),
            layout: Some(&path_layout),
            vertex: wgpu::VertexState {
                module: path_shader,
                entry_point: Some("vs_main"),
                buffers: std::slice::from_ref(&path_vertex_layout),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: path_shader,
                entry_point: Some("fs_main"),
                targets: color_targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: primitive_state,
            depth_stencil: None,
            multisample: multisample_state,
            multiview: None,
            cache: None,
        });

        // Path overlay pipeline - uses sample_count=1 for rendering on resolved textures
        let path_overlay = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Path Overlay Pipeline"),
            layout: Some(&path_layout),
            vertex: wgpu::VertexState {
                module: path_shader,
                entry_point: Some("vs_main"),
                buffers: &[path_vertex_layout],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: path_shader,
                entry_point: Some("fs_main"),
                targets: color_targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: primitive_state,
            depth_stencil: None,
            multisample: overlay_multisample_state,
            multiview: None,
            cache: None,
        });

        // Layer composite pipeline - for compositing offscreen layers with blend modes
        let layer_composite_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Layer Composite Pipeline Layout"),
                bind_group_layouts: &[&layouts.layer_composite],
                push_constant_ranges: &[],
            });

        // Use premultiplied alpha blending for layer composition
        let premultiplied_blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };

        let layer_composite = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Layer Composite Pipeline"),
            layout: Some(&layer_composite_layout),
            vertex: wgpu::VertexState {
                module: layer_composite_shader,
                entry_point: Some("vs_main"),
                buffers: &[], // No vertex buffers - quad generated in shader
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: layer_composite_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: texture_format,
                    blend: Some(premultiplied_blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: overlay_multisample_state, // 1x sampled - layers are resolved
            multiview: None,
            cache: None,
        });

        Pipelines {
            sdf,
            sdf_overlay,
            sdf_core,
            sdf_shadow,
            sdf_3d,
            sdf_notch,
            sdf_core_overlay,
            sdf_shadow_overlay,
            sdf_3d_overlay,
            sdf_notch_overlay,
            text,
            text_overlay,
            composite,
            composite_overlay,
            path,
            path_overlay,
            layer_composite,
        }
    }

    fn create_buffers(
        device: &wgpu::Device,
        config: &RendererConfig,
        has_storage_buffers: bool,
    ) -> Buffers {
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Uniforms Buffer"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Storage buffers are always created (even in DT mode they are needed by
        // non-SDF pipelines like glass). In DT mode the SDF bind group uses data
        // textures instead, but these buffers remain for other uses.
        let primitives = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Primitives Buffer"),
            size: (std::mem::size_of::<GpuPrimitive>() * config.max_primitives) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let glass_primitives = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Glass Primitives Buffer"),
            size: (std::mem::size_of::<GpuGlassPrimitive>() * config.max_glass_primitives) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let glass_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Glass Uniforms Buffer"),
            size: std::mem::size_of::<GlassUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let glyphs = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Glyphs Buffer"),
            size: (std::mem::size_of::<GpuGlyph>() * config.max_glyphs) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let path_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Path Uniforms Buffer"),
            size: std::mem::size_of::<PathUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Auxiliary data buffer for variable-length per-primitive data
        // Initial size: 1 vec4 (minimum for valid binding, will be recreated if needed)
        let aux_data = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Aux Data Buffer"),
            size: 16, // 1 vec4<f32> minimum
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Create data textures for DT (Tier 3) fallback when no storage buffers
        let (
            prim_data_texture,
            prim_data_view,
            aux_data_texture,
            aux_data_view,
            aux_data_texture_height,
            glyph_data_texture,
            glyph_data_view,
        ) = if !has_storage_buffers {
            // Primitive data texture: width=23 (one texel per vec4 field), height=max_primitives
            let prim_tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Primitive Data Texture"),
                size: wgpu::Extent3d {
                    width: 23,
                    height: config.max_primitives as u32,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let prim_view = prim_tex.create_view(&wgpu::TextureViewDescriptor::default());

            // Aux data texture: width=1024, height=1 initially (resized on demand)
            let aux_tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Aux Data Texture"),
                size: wgpu::Extent3d {
                    width: 1024,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let aux_view = aux_tex.create_view(&wgpu::TextureViewDescriptor::default());

            // Glyph data texture: width=6 (one texel per vec4 field), height=max_glyphs
            let glyph_tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Glyph Data Texture"),
                size: wgpu::Extent3d {
                    width: 6,
                    height: config.max_glyphs as u32,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let glyph_view = glyph_tex.create_view(&wgpu::TextureViewDescriptor::default());

            (
                Some(prim_tex),
                Some(prim_view),
                Some(aux_tex),
                Some(aux_view),
                1u32,
                Some(glyph_tex),
                Some(glyph_view),
            )
        } else {
            (None, None, None, None, 0u32, None, None)
        };

        Buffers {
            uniforms,
            primitives,
            glass_primitives,
            glass_uniforms,
            glyphs,
            path_uniforms,
            path_vertices: None,
            path_indices: None,
            blur_uniforms_pool: None,
            drop_shadow_uniforms: None,
            glow_uniforms: None,
            color_matrix_uniforms: None,
            aux_data,
            sdf_vertex_instances: None,
            prim_data_texture,
            prim_data_view,
            aux_data_texture,
            aux_data_view,
            aux_data_texture_height,
            glyph_data_texture,
            glyph_data_view,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn create_bind_groups(
        device: &wgpu::Device,
        layouts: &BindGroupLayouts,
        buffers: &Buffers,
        glyph_atlas_view: &wgpu::TextureView,
        color_glyph_atlas_view: &wgpu::TextureView,
        glyph_sampler: &wgpu::Sampler,
        gradient_texture_cache: &GradientTextureCache,
        path_image_view: &wgpu::TextureView,
        path_image_sampler: &wgpu::Sampler,
        has_storage_buffers: bool,
    ) -> BindGroups {
        // Binding 1: primitives (storage buffer or data texture)
        let binding_1 = if has_storage_buffers {
            wgpu::BindGroupEntry {
                binding: 1,
                resource: buffers.primitives.as_entire_binding(),
            }
        } else {
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(
                    buffers
                        .prim_data_view
                        .as_ref()
                        .expect("DT mode requires prim_data_view"),
                ),
            }
        };

        // Binding 5: aux data (storage buffer or data texture)
        let binding_5 = if has_storage_buffers {
            wgpu::BindGroupEntry {
                binding: 5,
                resource: buffers.aux_data.as_entire_binding(),
            }
        } else {
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(
                    buffers
                        .aux_data_view
                        .as_ref()
                        .expect("DT mode requires aux_data_view"),
                ),
            }
        };

        let sdf = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("SDF Bind Group"),
            layout: &layouts.sdf,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffers.uniforms.as_entire_binding(),
                },
                binding_1,
                // Glyph atlas texture (binding 2)
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(glyph_atlas_view),
                },
                // Glyph sampler (binding 3)
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(glyph_sampler),
                },
                // Color glyph atlas texture (binding 4)
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(color_glyph_atlas_view),
                },
                binding_5,
            ],
        });

        // Path bind group (with gradient texture, image texture, and backdrop for glass)
        let path = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Path Bind Group"),
            layout: &layouts.path,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffers.path_uniforms.as_entire_binding(),
                },
                // Gradient texture (binding 1)
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&gradient_texture_cache.view),
                },
                // Gradient sampler (binding 2)
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&gradient_texture_cache.sampler),
                },
                // Image texture (binding 3)
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(path_image_view),
                },
                // Image sampler (binding 4)
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(path_image_sampler),
                },
                // Backdrop texture (binding 5) - uses placeholder, will be replaced when glass is enabled
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(path_image_view),
                },
                // Backdrop sampler (binding 6)
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::Sampler(path_image_sampler),
                },
            ],
        });

        // Glass bind group will be created when we have a backdrop texture
        BindGroups {
            sdf,
            glass: None,
            path,
        }
    }

    /// Create MSAA-specific pipelines for a given sample count
    fn create_msaa_pipelines(
        device: &wgpu::Device,
        layouts: &BindGroupLayouts,
        texture_format: wgpu::TextureFormat,
        sample_count: u32,
        has_vertex_storage: bool,
        has_storage_buffers: bool,
    ) -> MsaaPipelines {
        let blend_state = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };

        let color_targets = &[Some(wgpu::ColorTargetState {
            format: texture_format,
            blend: Some(blend_state),
            write_mask: wgpu::ColorWrites::ALL,
        })];

        let primitive_state = wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        };

        let multisample_state = wgpu::MultisampleState {
            count: sample_count,
            mask: !0,
            alpha_to_coverage_enabled: false,
        };

        // Monolithic stand-in (MSAA) — uses core shader to avoid compiling
        // the full SDF_SHADER which exceeds PowerVR's shader compiler limit.
        let msaa_core_source = if !has_storage_buffers {
            SDF_CORE_DT_SHADER
        } else if !has_vertex_storage {
            SDF_CORE_VB_SHADER
        } else {
            SDF_CORE_SHADER
        };
        let sdf_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("SDF Shader (MSAA stand-in)"),
            source: wgpu::ShaderSource::Wgsl(msaa_core_source.into()),
        });

        let sdf_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("SDF Pipeline Layout (MSAA)"),
            bind_group_layouts: &[&layouts.sdf],
            push_constant_ranges: &[],
        });

        let sdf = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("SDF Pipeline (MSAA)"),
            layout: Some(&sdf_layout),
            vertex: wgpu::VertexState {
                module: &sdf_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &sdf_shader,
                entry_point: Some("fs_main"),
                targets: color_targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: primitive_state,
            depth_stencil: None,
            multisample: multisample_state,
            multiview: None,
            cache: None,
        });

        // Split SDF shader modules (MSAA)
        let sdf_vb_buffers: &[wgpu::VertexBufferLayout<'_>] = if has_vertex_storage {
            &[]
        } else {
            &[SdfVertexInstance::LAYOUT]
        };
        let make_msaa_sdf_pipeline = |source: &str, label: &str| -> wgpu::RenderPipeline {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&sdf_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: sdf_vb_buffers,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: color_targets,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: primitive_state,
                depth_stencil: None,
                multisample: multisample_state,
                multiview: None,
                cache: None,
            })
        };

        let (msaa_core_src, msaa_shadow_src, msaa_3d_src, msaa_notch_src) = if !has_storage_buffers
        {
            (
                SDF_CORE_DT_SHADER,
                SDF_SHADOW_DT_SHADER,
                SDF_3D_DT_SHADER,
                SDF_NOTCH_DT_SHADER,
            )
        } else if !has_vertex_storage {
            (
                SDF_CORE_VB_SHADER,
                SDF_SHADOW_VB_SHADER,
                SDF_3D_VB_SHADER,
                SDF_NOTCH_VB_SHADER,
            )
        } else {
            (
                SDF_CORE_SHADER,
                SDF_SHADOW_SHADER,
                SDF_3D_SHADER,
                SDF_NOTCH_SHADER,
            )
        };

        let sdf_core = make_msaa_sdf_pipeline(msaa_core_src, "SDF Core Pipeline (MSAA)");
        let sdf_shadow = make_msaa_sdf_pipeline(msaa_shadow_src, "SDF Shadow Pipeline (MSAA)");
        let sdf_3d = make_msaa_sdf_pipeline(msaa_3d_src, "SDF 3D Pipeline (MSAA)");
        let sdf_notch = make_msaa_sdf_pipeline(msaa_notch_src, "SDF Notch Pipeline (MSAA)");

        // Create path shader
        let path_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Path Shader (MSAA)"),
            source: wgpu::ShaderSource::Wgsl(PATH_SHADER.into()),
        });

        let path_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Path Pipeline Layout (MSAA)"),
            bind_group_layouts: &[&layouts.path],
            push_constant_ranges: &[],
        });

        // PathVertex layout — see PathVertex struct in path.rs for offset rationale
        let path_vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<PathVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 8,
                    shader_location: 1,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 24,
                    shader_location: 2,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 40,
                    shader_location: 3,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 48,
                    shader_location: 4,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Uint32,
                    offset: 64,
                    shader_location: 5,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32,
                    offset: 68,
                    shader_location: 6,
                },
                // clip_bounds: vec4<f32>
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 72,
                    shader_location: 7,
                },
                // clip_radius: vec4<f32>
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 88,
                    shader_location: 8,
                },
                // clip_type: u32
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Uint32,
                    offset: 104,
                    shader_location: 9,
                },
            ],
        };

        let path = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Path Pipeline (MSAA)"),
            layout: Some(&path_layout),
            vertex: wgpu::VertexState {
                module: &path_shader,
                entry_point: Some("vs_main"),
                buffers: &[path_vertex_layout],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &path_shader,
                entry_point: Some("fs_main"),
                targets: color_targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: primitive_state,
            depth_stencil: None,
            multisample: multisample_state,
            multiview: None,
            cache: None,
        });

        MsaaPipelines {
            sdf,
            sdf_core,
            sdf_shadow,
            sdf_3d,
            sdf_notch,
            path,
            sample_count,
        }
    }

    /// Resize the viewport
    pub fn resize(&mut self, width: u32, height: u32) {
        self.viewport_size = (width, height);
    }

    /// Set the current render target texture for blend mode two-pass compositing.
    ///
    /// Must be called before `render_overlay()` when the batch may contain
    /// non-Normal blend modes. The texture must remain valid until
    /// `clear_blend_target()` is called.
    ///
    /// # Safety contract
    /// The caller guarantees the texture reference outlives the render frame.
    /// The pointer is only dereferenced within `blit_texture_to_target`.
    pub fn set_blend_target(&mut self, texture: &wgpu::Texture) {
        self.blend_target_ptr = Some(texture as *const wgpu::Texture);
    }

    /// Clear the blend target texture reference after rendering.
    pub fn clear_blend_target(&mut self) {
        self.blend_target_ptr = None;
    }

    /// Update the frame time (for animations)
    pub fn update_time(&mut self, time: f32) {
        self.time = time;
    }

    /// Get the wgpu device
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Get the wgpu device as Arc
    pub fn device_arc(&self) -> Arc<wgpu::Device> {
        self.device.clone()
    }

    /// Get the wgpu queue
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Get the wgpu queue as Arc
    pub fn queue_arc(&self) -> Arc<wgpu::Queue> {
        self.queue.clone()
    }

    /// Create a new surface for an additional window.
    ///
    /// Uses the existing wgpu Instance to create a surface that can be
    /// configured and rendered to using the shared device and queue.
    /// This is used for multi-window support.
    pub fn create_surface<W>(&self, window: Arc<W>) -> Result<wgpu::Surface<'static>, RendererError>
    where
        W: raw_window_handle::HasWindowHandle
            + raw_window_handle::HasDisplayHandle
            + Send
            + Sync
            + 'static,
    {
        self.instance
            .create_surface(wgpu::SurfaceTarget::from(window))
            .map_err(RendererError::SurfaceError)
    }

    /// Get the texture format used by this renderer's pipelines
    pub fn texture_format(&self) -> wgpu::TextureFormat {
        self.texture_format
    }

    /// Returns true if unified text/SDF rendering is enabled
    ///
    /// When enabled, text glyphs are converted to SDF primitives and rendered
    /// in the same GPU pass as other shapes, ensuring consistent transforms
    /// during animations.
    pub fn unified_text_rendering(&self) -> bool {
        self.config.unified_text_rendering
    }

    /// Poll the device to process completed GPU operations and free resources.
    /// Call this after frame rendering to prevent memory accumulation.
    pub fn poll(&self) {
        // wgpu 26: `Maintain::Wait` was renamed to `PollType::Wait`. Result
        // is a `Result<PollStatus, _>` rather than the old `MaintainResult`,
        // and we don't care about the precise status here — we just want
        // to block until the GPU is idle.
        let _ = self.device.poll(wgpu::PollType::Wait);
    }

    /// Bind real glyph atlas textures into the default SDF bind group.
    ///
    /// Call once per frame before any rendering when CSS-transformed text is present.
    /// This replaces the placeholder atlas with the real glyph atlas in
    /// `self.bind_groups.sdf`, so ALL render paths automatically get the atlas
    /// without needing to thread it through every method.
    ///
    /// Uses pointer comparison to avoid recreating the bind group when the atlas
    /// hasn't changed between frames.
    ///
    /// SAFETY: The raw pointers stored in `active_glyph_atlas` must remain valid
    /// for the duration of the frame. This is guaranteed because they point to
    /// TextureViews owned by the text context, which outlives all render calls.
    pub fn set_glyph_atlas(
        &mut self,
        atlas_view: &wgpu::TextureView,
        color_atlas_view: &wgpu::TextureView,
    ) {
        let atlas_ptr = atlas_view as *const wgpu::TextureView;
        let color_ptr = color_atlas_view as *const wgpu::TextureView;

        let need_rebuild = match &self.active_glyph_atlas {
            Some(active) => {
                active.atlas_view_ptr != atlas_ptr || active.color_atlas_view_ptr != color_ptr
            }
            None => true,
        };

        if need_rebuild {
            self.active_glyph_atlas = Some(ActiveGlyphAtlas {
                atlas_view_ptr: atlas_ptr,
                color_atlas_view_ptr: color_ptr,
            });
            self.rebind_sdf_bind_group();
        }
    }

    /// Get a mutable reference to the @flow pipeline cache
    pub fn flow_pipeline_cache(&mut self) -> &mut crate::flow_pipeline::FlowPipelineCache {
        &mut self.flow_pipeline_cache
    }

    /// Render a @flow fragment shader into a target texture.
    ///
    /// Compiles the flow on first use, updates uniforms, and draws a fullscreen quad.
    /// If `viewport` is Some([x, y, w, h]), the quad is scoped to that region in pixels.
    /// Returns false if the flow is not found or compilation failed.
    /// Ensure the scene copy texture exists, matches viewport size, and is up-to-date.
    ///
    /// Called once per frame before rendering any flows that use `sample_scene()`.
    /// Returns the scene texture view, or None if the copy failed.
    fn ensure_scene_copy(&mut self) -> Option<&wgpu::TextureView> {
        let (tw, th) = self.viewport_size;
        if tw == 0 || th == 0 {
            return None;
        }

        // Recreate texture on viewport resize
        let needs_recreate = match &self.scene_copy_texture {
            Some((_, _, w, h)) => *w != tw || *h != th,
            None => true,
        };
        if needs_recreate {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Flow Scene Copy Texture"),
                size: wgpu::Extent3d {
                    width: tw,
                    height: th,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.texture_format,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            self.scene_copy_texture = Some((tex, view, tw, th));
            // Scene texture changed — invalidate bind groups that reference it
            self.flow_pipeline_cache.invalidate_scene_bind_groups();
        }

        // Copy current render target → scene copy texture (single copy per frame)
        if let Some((scene_tex, _, _, _)) = &self.scene_copy_texture {
            if let Some(tex_ptr) = self.blend_target_ptr {
                let src_tex = unsafe { &*tex_ptr };
                let mut copy_encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Flow Scene Copy Encoder"),
                        });
                copy_encoder.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: src_tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: scene_tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: tw,
                        height: th,
                        depth_or_array_layers: 1,
                    },
                );
                self.queue.submit(std::iter::once(copy_encoder.finish()));
            }
        }

        self.scene_copy_texture.as_ref().map(|(_, v, _, _)| v)
    }

    pub fn render_flow(
        &mut self,
        target: &wgpu::TextureView,
        flow_name: &str,
        uniforms: &crate::flow_pipeline::FlowUniformData,
        viewport: Option<[f32; 4]>,
    ) -> bool {
        // If this flow uses sample_scene(), ensure scene copy is ready
        let scene_view_owned: Option<*const wgpu::TextureView> =
            if self.flow_pipeline_cache.needs_scene_texture(flow_name) {
                self.ensure_scene_copy().map(|v| v as *const _)
            } else {
                None
            };
        // SAFETY: scene_copy_texture is owned by self and lives for the duration of this call
        let scene_view = scene_view_owned.map(|ptr| unsafe { &*ptr });

        if !self
            .flow_pipeline_cache
            .prepare_render(&self.queue, flow_name, uniforms, scene_view)
        {
            return false;
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Flow Render Encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Flow Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load, // Preserve existing content
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Scope the flow quad to the element's bounds via viewport + scissor.
            // Clamp to render target bounds to avoid wgpu validation errors.
            if let Some([x, y, w, h]) = viewport {
                let (tw, th) = self.viewport_size;
                let tw = tw as f32;
                let th = th as f32;
                let cx = x.max(0.0);
                let cy = y.max(0.0);
                let cw = (w - (cx - x)).min(tw - cx).max(1.0);
                let ch = (h - (cy - y)).min(th - cy).max(1.0);
                if cx < tw && cy < th {
                    pass.set_viewport(cx, cy, cw, ch, 0.0, 1.0);
                    pass.set_scissor_rect(cx as u32, cy as u32, cw as u32, ch as u32);
                }
            }

            self.flow_pipeline_cache
                .render_fragment(&mut pass, flow_name);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        true
    }

    /// Render a batch of primitives to a texture view
    /// Render primitives with transparent background (default)
    pub fn render(&mut self, target: &wgpu::TextureView, batch: &PrimitiveBatch) {
        self.render_with_clear(target, batch, [0.0, 0.0, 0.0, 0.0]);
    }

    /// Render primitives at a specific viewport size (for reduced-resolution rendering)
    ///
    /// Used for glass backdrop rendering at half resolution.
    pub fn render_at_size(
        &mut self,
        target: &wgpu::TextureView,
        batch: &PrimitiveBatch,
        clear_color: [f64; 4],
        width: u32,
        height: u32,
    ) {
        // Temporarily override viewport size for this render
        let original_size = self.viewport_size;
        self.viewport_size = (width, height);
        self.render_with_clear(target, batch, clear_color);
        self.viewport_size = original_size;
    }

    /// Render primitives with a specified clear color
    ///
    /// # Arguments
    /// * `target` - The texture view to render to
    /// * `batch` - The primitive batch to render
    /// * `clear_color` - RGBA clear color (0.0-1.0 range)
    pub fn render_with_clear(
        &mut self,
        target: &wgpu::TextureView,
        batch: &PrimitiveBatch,
        clear_color: [f64; 4],
    ) {
        // Evict oversized textures from the pool at frame start
        // This prevents memory bloat from accumulated large textures
        self.layer_texture_cache.evict_oversized();

        // Check if we have layer commands with effects, blend modes, or 3D transforms
        let has_layer_effects = batch.layer_commands.iter().any(|entry| {
            if let crate::primitives::LayerCommand::Push { config } = &entry.command {
                !config.effects.is_empty()
                    || config.blend_mode != blinc_core::BlendMode::Normal
                    || config.transform_3d.is_some()
            } else {
                false
            }
        });

        tracing::trace!(
            "render_with_clear: {} primitives, {} layer commands, has_layer_effects={}",
            batch.primitives.len(),
            batch.layer_commands.len(),
            has_layer_effects
        );

        // If we have layer effects, use the layer-aware rendering path
        if has_layer_effects {
            self.render_with_layer_effects(target, batch, clear_color);
            return;
        }

        // Standard rendering (no layer effects)
        self.render_with_clear_simple(target, batch, clear_color);
    }

    /// Simple render with clear (no layer effect processing)
    /// Test whether a primitive's expanded bounds intersect the viewport.
    ///
    /// Returns `true` if the primitive might be visible and should be rendered.
    /// Conservative: accounts for shadow, border, and rotation expansion.
    /// Primitives with 3D perspective are always considered visible.
    #[inline]
    fn is_primitive_visible(&self, prim: &GpuPrimitive) -> bool {
        let vp_w = self.viewport_size.0 as f32;
        let vp_h = self.viewport_size.1 as f32;

        let px = prim.bounds[0];
        let py = prim.bounds[1];
        let pw = prim.bounds[2];
        let ph = prim.bounds[3];

        // Primitives with 3D perspective may project anywhere
        if prim.perspective[2] > 0.0 {
            return true;
        }

        // Account for shadow expansion (matches shader bounds computation)
        let shadow_blur = prim.shadow[2];
        let shadow_ox = prim.shadow[0].abs();
        let shadow_oy = prim.shadow[1].abs();
        let mut expand = shadow_blur * 3.0 + shadow_ox + shadow_oy;

        // Account for border (stroke) expansion
        expand += prim.border[0];

        // Account for rotation — rotated rects have larger AABB
        // rotation = [sin_rz, cos_rz, sin_ry, cos_ry], identity = [0, 1, 0, 1]
        let has_rotation = prim.rotation[0] != 0.0 || prim.rotation[2] != 0.0;
        if has_rotation {
            // Worst case AABB expansion for rotated rect: half-diagonal
            let half_diag = (pw * pw + ph * ph).sqrt() * 0.5;
            expand += half_diag;
        }

        // Non-identity local affine — be generous with expansion
        let has_affine = prim.local_affine[1] != 0.0 || prim.local_affine[2] != 0.0;
        if has_affine {
            let half_diag = (pw * pw + ph * ph).sqrt() * 0.5;
            expand += half_diag;
        }

        // AABB intersection with viewport [0, 0, vp_w, vp_h]
        let left = px - expand;
        let top = py - expand;
        let right = px + pw + expand;
        let bottom = py + ph + expand;

        right > 0.0 && bottom > 0.0 && left < vp_w && top < vp_h
    }

    /// Cull a slice of primitives, returning only those visible in the viewport.
    fn cull_primitives(&self, prims: &[GpuPrimitive]) -> Vec<GpuPrimitive> {
        prims
            .iter()
            .filter(|p| self.is_primitive_visible(p))
            .copied()
            .collect()
    }

    fn render_with_clear_simple(
        &mut self,
        target: &wgpu::TextureView,
        batch: &PrimitiveBatch,
        clear_color: [f64; 4],
    ) {
        // Update uniforms
        let uniforms = Uniforms {
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            _padding: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.buffers.uniforms, 0, bytemuck::bytes_of(&uniforms));

        // Cull off-screen primitives before GPU upload
        let visible_primitives = self.cull_primitives(&batch.primitives);

        // Sort primitives by pipeline category and upload
        let sdf_ranges = self.upload_sorted_primitives(&visible_primitives);

        // Update auxiliary data buffer (group shapes, polygon clips)
        // This may call rebind_sdf_bind_group() if the buffer needs resizing.
        // When active_glyph_atlas is set, rebind uses the real atlas automatically.
        self.update_aux_data_buffer(batch);

        // Update path buffers if we have path geometry
        let has_paths = !batch.paths.vertices.is_empty() && !batch.paths.indices.is_empty();
        if has_paths {
            self.update_path_buffers(batch);
        }

        // Create command encoder
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Blinc Render Encoder"),
            });

        // Begin render pass
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Blinc Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear_color[0],
                            g: clear_color[1],
                            b: clear_color[2],
                            a: clear_color[3],
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Render SDF primitives via split pipelines
            if !visible_primitives.is_empty() {
                render_pass.set_bind_group(0, &self.bind_groups.sdf, &[]);
                Self::draw_split_sdf(
                    &mut render_pass,
                    &self.pipelines,
                    &sdf_ranges,
                    false,
                    self.sdf_vb_buffer(),
                );
            }

            // Render paths
            if has_paths {
                if let (Some(vb), Some(ib)) =
                    (&self.buffers.path_vertices, &self.buffers.path_indices)
                {
                    render_pass.set_pipeline(&self.pipelines.path);
                    render_pass.set_bind_group(0, &self.bind_groups.path, &[]);
                    render_pass.set_vertex_buffer(0, vb.slice(..));
                    render_pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                    render_pass.draw_indexed(0..batch.paths.indices.len() as u32, 0, 0..1);
                }
            }
        }

        // Submit commands
        self.queue.submit(std::iter::once(encoder.finish()));

        // Render SDF 3D viewports (after main content, so they render on top)
        if !batch.viewports_3d.is_empty() {
            self.render_sdf_3d_viewports(target, &batch.viewports_3d);
        }

        // Render GPU particle viewports (after SDF viewports)
        if !batch.particle_viewports.is_empty() {
            self.render_particle_viewports(target, &batch.particle_viewports);
        }
    }

    /// Render with layer effect processing
    ///
    /// This implements a correct layer effect system:
    /// 1. Identify primitive ranges for effect layers
    /// 2. Render non-effect primitives to target (skipping those in effect layers)
    /// 3. For each effect layer, render to viewport-sized texture, apply effects, blit at position
    fn render_with_layer_effects(
        &mut self,
        target: &wgpu::TextureView,
        batch: &PrimitiveBatch,
        clear_color: [f64; 4],
    ) {
        use crate::primitives::LayerCommand;

        // Build list of effect layers with their primitive ranges
        let mut effect_layers: Vec<(usize, usize, blinc_core::LayerConfig)> = Vec::new();
        let mut layer_stack: Vec<(usize, blinc_core::LayerConfig)> = Vec::new();

        for entry in &batch.layer_commands {
            match &entry.command {
                LayerCommand::Push { config } => {
                    layer_stack.push((entry.primitive_index, config.clone()));
                }
                LayerCommand::Pop => {
                    if let Some((start_idx, config)) = layer_stack.pop() {
                        if !config.effects.is_empty()
                            || config.blend_mode != blinc_core::BlendMode::Normal
                            || config.transform_3d.is_some()
                        {
                            effect_layers.push((start_idx, entry.primitive_index, config));
                        }
                    }
                }
                LayerCommand::Sample { .. } => {}
            }
        }

        // If no effect layers, just render normally
        if effect_layers.is_empty() {
            self.render_with_clear_simple(target, batch, clear_color);
            return;
        }

        // Build set of primitive indices that belong to effect layers (to skip in first pass)
        let mut effect_primitives = std::collections::HashSet::new();
        for (start, end, _) in &effect_layers {
            for i in *start..*end {
                effect_primitives.insert(i);
            }
        }

        // First pass: render primitives that are NOT in effect layers
        self.render_primitives_excluding(target, batch, &effect_primitives, clear_color);
        drop(effect_primitives); // Free HashSet immediately - not needed after first pass

        // Process each effect layer
        for (start_idx, end_idx, config) in effect_layers {
            if start_idx >= end_idx || end_idx > batch.primitives.len() {
                continue;
            }

            // Config position/size are in local coordinates (relative to parent)
            // But primitives are at screen-space coordinates after transforms
            // We need to compute the actual bounding box from primitives
            let primitives = &batch.primitives[start_idx..end_idx];
            let (layer_pos, layer_size, layer_clip) = if primitives.is_empty() {
                // Fallback to config values if no primitives
                let pos = config.position.map(|p| (p.x, p.y)).unwrap_or((0.0, 0.0));
                let size = config
                    .size
                    .map(|s| (s.width, s.height))
                    .unwrap_or((self.viewport_size.0 as f32, self.viewport_size.1 as f32));
                (pos, size, None)
            } else {
                // Compute bounding box from primitives (which are in screen coordinates)
                let mut min_x = f32::MAX;
                let mut min_y = f32::MAX;
                let mut max_x = f32::MIN;
                let mut max_y = f32::MIN;
                // Extract clip bounds from the first primitive with a valid clip
                // All primitives in a layer should have the same clip (from scroll container)
                let mut clip: Option<([f32; 4], [f32; 4])> = None;
                for p in primitives {
                    let (px, py, pw, ph) = (p.bounds[0], p.bounds[1], p.bounds[2], p.bounds[3]);
                    min_x = min_x.min(px);
                    min_y = min_y.min(py);
                    max_x = max_x.max(px + pw);
                    max_y = max_y.max(py + ph);
                    // Check for valid clip bounds (not the default "no clip" values)
                    // Default is [-10000, -10000, 100000, 100000]
                    if clip.is_none() && p.clip_bounds[0] > -5000.0 && p.clip_bounds[2] < 90000.0 {
                        clip = Some((p.clip_bounds, p.clip_radius));
                    }
                }
                let width = (max_x - min_x).max(1.0);
                let height = (max_y - min_y).max(1.0);
                ((min_x, min_y), (width, height), clip)
            };

            // Skip layers that are entirely outside the viewport
            let vp_w = self.viewport_size.0 as f32;
            let vp_h = self.viewport_size.1 as f32;
            let is_visible = layer_pos.0 < vp_w
                && layer_pos.1 < vp_h
                && layer_pos.0 + layer_size.0 > 0.0
                && layer_pos.1 + layer_size.1 > 0.0
                && layer_size.0 > 0.0
                && layer_size.1 > 0.0;

            if !is_visible {
                continue;
            }

            // Calculate effect expansion (how much effects extend beyond original bounds)
            let effect_expansion = Self::calculate_effect_expansion(&config.effects);

            // Render layer primitives to a TIGHT texture (not viewport-sized!)
            // This significantly reduces memory usage and effect processing time
            // Returns both texture and content_size (which may differ from texture.size due to pool bucket rounding)
            let (layer_texture, content_size) = self.render_primitive_range_tight(
                batch,
                start_idx,
                end_idx,
                layer_pos,
                layer_size,
                effect_expansion,
            );

            // Use content_size for blitting (not layer_texture.size which may be larger)
            let tight_size = content_size;

            // Calculate the destination position and size for blitting
            // Don't clamp to 0 - allow negative positions for scrolled content
            // The blit function will handle off-screen portions correctly
            let expanded_pos = (
                layer_pos.0 - effect_expansion.0,
                layer_pos.1 - effect_expansion.1,
            );
            let expanded_size = (
                layer_size.0 + effect_expansion.0 + effect_expansion.2,
                layer_size.1 + effect_expansion.1 + effect_expansion.3,
            );

            // Skip texture copy when no effects - use layer_texture directly
            if config.effects.is_empty() {
                // Blit directly without effect processing (skip copy)
                self.blit_tight_texture_to_target(
                    &layer_texture.view,
                    tight_size,
                    target,
                    expanded_pos,
                    expanded_size,
                    config.opacity,
                    config.blend_mode,
                    layer_clip,
                    config.transform_3d,
                );
                self.layer_texture_cache.release(layer_texture);
            } else {
                // Apply effects to the tight texture
                let effected = self.apply_layer_effects(&layer_texture, &config.effects);
                self.layer_texture_cache.release(layer_texture);

                // Blit the effected texture back to target at the correct position
                // Pass through the clip bounds so effects don't bleed outside scroll containers
                self.blit_tight_texture_to_target(
                    &effected.view,
                    tight_size,
                    target,
                    expanded_pos,
                    expanded_size,
                    config.opacity,
                    config.blend_mode,
                    layer_clip,
                    config.transform_3d,
                );
                self.layer_texture_cache.release(effected);
            }
        }
    }

    /// Render primitives excluding those in the given set
    fn render_primitives_excluding(
        &mut self,
        target: &wgpu::TextureView,
        batch: &PrimitiveBatch,
        exclude: &std::collections::HashSet<usize>,
        clear_color: [f64; 4],
    ) {
        // If nothing to exclude, use simple path
        if exclude.is_empty() {
            self.render_with_clear_simple(target, batch, clear_color);
            return;
        }

        // Build list of primitives to render (excluding effect layers + off-screen)
        let included_primitives: Vec<GpuPrimitive> = batch
            .primitives
            .iter()
            .enumerate()
            .filter(|(i, p)| !exclude.contains(i) && self.is_primitive_visible(p))
            .map(|(_, p)| *p)
            .collect();

        if included_primitives.is_empty() && batch.paths.vertices.is_empty() {
            // Just clear the target
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Clear Encoder"),
                });
            {
                let _render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Clear Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: clear_color[0],
                                g: clear_color[1],
                                b: clear_color[2],
                                a: clear_color[3],
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
            }
            self.queue.submit(std::iter::once(encoder.finish()));
            return;
        }

        // Update uniforms
        let uniforms = Uniforms {
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            _padding: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.buffers.uniforms, 0, bytemuck::bytes_of(&uniforms));

        // Update auxiliary data buffer
        self.update_aux_data_buffer(batch);

        // Sort and upload filtered primitives
        let sdf_ranges = self.upload_sorted_primitives(&included_primitives);

        // Update path buffers if we have path geometry
        let has_paths = !batch.paths.vertices.is_empty() && !batch.paths.indices.is_empty();
        if has_paths {
            self.update_path_buffers(batch);
        }

        // Create command encoder
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Filtered Render Encoder"),
            });

        // Begin render pass
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Filtered Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear_color[0],
                            g: clear_color[1],
                            b: clear_color[2],
                            a: clear_color[3],
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Render SDF primitives via split pipelines (filtered)
            if !included_primitives.is_empty() {
                render_pass.set_bind_group(0, &self.bind_groups.sdf, &[]);
                Self::draw_split_sdf(
                    &mut render_pass,
                    &self.pipelines,
                    &sdf_ranges,
                    false,
                    self.sdf_vb_buffer(),
                );
            }

            // Render paths (always rendered - path filtering would be more complex)
            if has_paths {
                if let (Some(vb), Some(ib)) =
                    (&self.buffers.path_vertices, &self.buffers.path_indices)
                {
                    render_pass.set_pipeline(&self.pipelines.path);
                    render_pass.set_bind_group(0, &self.bind_groups.path, &[]);
                    render_pass.set_vertex_buffer(0, vb.slice(..));
                    render_pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                    render_pass.draw_indexed(0..batch.paths.indices.len() as u32, 0, 0..1);
                }
            }
        }

        // Submit commands
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Update auxiliary data buffer (for 3D group shapes, polygon clips, etc.)
    ///
    /// If the batch has aux_data, writes it to the GPU buffer, recreating the buffer
    /// and rebinding if it's too small.
    fn update_aux_data_buffer(&mut self, batch: &PrimitiveBatch) {
        if batch.aux_data.is_empty() {
            return;
        }

        if !self.has_storage_buffers {
            // DT mode: upload aux data to texture instead of storage buffer
            self.update_aux_data_texture(&batch.aux_data);
            return;
        }

        let data_size = (batch.aux_data.len() * std::mem::size_of::<[f32; 4]>()) as u64;
        let buffer_size = self.buffers.aux_data.size();

        // Recreate buffer if too small
        if data_size > buffer_size {
            self.buffers.aux_data = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Aux Data Buffer"),
                size: data_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            // Must recreate the SDF bind group since the buffer changed
            self.rebind_sdf_bind_group();
        }

        self.queue.write_buffer(
            &self.buffers.aux_data,
            0,
            bytemuck::cast_slice(&batch.aux_data),
        );
    }

    /// Upload auxiliary data to the DT fallback texture (Tier 3 / WebGL2).
    ///
    /// The texture has width=1024 and variable height. If the data exceeds
    /// the current texture capacity, the texture is recreated larger and
    /// the SDF bind group is rebound.
    fn update_aux_data_texture(&mut self, aux_data: &[[f32; 4]]) {
        const AUX_TEX_WIDTH: u32 = 1024;
        let count = aux_data.len() as u32;
        let needed_height = count.div_ceil(AUX_TEX_WIDTH).max(1);

        if needed_height > self.buffers.aux_data_texture_height {
            // Recreate the texture with more rows
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Aux Data Texture"),
                size: wgpu::Extent3d {
                    width: AUX_TEX_WIDTH,
                    height: needed_height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            self.buffers.aux_data_texture = Some(tex);
            self.buffers.aux_data_view = Some(view);
            self.buffers.aux_data_texture_height = needed_height;

            // Rebind since the texture changed
            self.rebind_sdf_bind_group();
        }

        if let Some(ref tex) = self.buffers.aux_data_texture {
            // Pad aux_data to full rows so write_texture gets a complete rectangle
            let total_texels = (AUX_TEX_WIDTH * needed_height) as usize;
            let mut padded = aux_data.to_vec();
            padded.resize(total_texels, [0.0f32; 4]);

            let bytes = bytemuck::cast_slice::<[f32; 4], u8>(&padded);
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(AUX_TEX_WIDTH * 16), // 1024 texels × 16 bytes
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: AUX_TEX_WIDTH,
                    height: needed_height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    /// Recreate the SDF bind group (needed when aux_data buffer is resized).
    ///
    /// Uses the real glyph atlas if `active_glyph_atlas` is set, otherwise
    /// falls back to placeholder textures.
    fn rebind_sdf_bind_group(&mut self) {
        // SAFETY: When active_glyph_atlas is Some, the pointers are valid for the
        // duration of the frame (they point to TextureViews owned by the text context).
        let (atlas_view, color_atlas_view): (&wgpu::TextureView, &wgpu::TextureView) =
            if let Some(active) = &self.active_glyph_atlas {
                unsafe { (&*active.atlas_view_ptr, &*active.color_atlas_view_ptr) }
            } else {
                (
                    &self.placeholder_glyph_atlas_view,
                    &self.placeholder_color_glyph_atlas_view,
                )
            };

        // Binding 1: primitives (storage buffer or data texture)
        let binding_1 = if self.has_storage_buffers {
            wgpu::BindGroupEntry {
                binding: 1,
                resource: self.buffers.primitives.as_entire_binding(),
            }
        } else {
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(
                    self.buffers
                        .prim_data_view
                        .as_ref()
                        .expect("DT mode requires prim_data_view"),
                ),
            }
        };

        // Binding 5: aux data (storage buffer or data texture)
        let binding_5 = if self.has_storage_buffers {
            wgpu::BindGroupEntry {
                binding: 5,
                resource: self.buffers.aux_data.as_entire_binding(),
            }
        } else {
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(
                    self.buffers
                        .aux_data_view
                        .as_ref()
                        .expect("DT mode requires aux_data_view"),
                ),
            }
        };

        self.bind_groups.sdf = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("SDF Bind Group (rebound)"),
            layout: &self.bind_group_layouts.sdf,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.buffers.uniforms.as_entire_binding(),
                },
                binding_1,
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.glyph_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(color_atlas_view),
                },
                binding_5,
            ],
        });
    }

    /// Update path vertex and index buffers
    fn update_path_buffers(&mut self, batch: &PrimitiveBatch) {
        // Upload gradient texture if needed for multi-stop gradients
        if batch.paths.use_gradient_texture {
            if let Some(ref stops) = batch.paths.gradient_stops {
                self.gradient_texture_cache.upload_stops(
                    &self.queue,
                    stops,
                    crate::gradient_texture::SpreadMode::Pad,
                );
            }
        }

        // Update path uniforms with clip data and brush metadata from batch
        let path_uniforms = PathUniforms {
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            clip_bounds: batch.paths.clip_bounds,
            clip_radius: batch.paths.clip_radius,
            clip_type: batch.paths.clip_type,
            use_gradient_texture: if batch.paths.use_gradient_texture {
                1
            } else {
                0
            },
            use_image_texture: if batch.paths.use_image_texture { 1 } else { 0 },
            use_glass_effect: if batch.paths.use_glass_effect { 1 } else { 0 },
            image_uv_bounds: batch.paths.image_uv_bounds,
            glass_params: batch.paths.glass_params,
            glass_tint: batch.paths.glass_tint,
            ..PathUniforms::default()
        };
        self.queue.write_buffer(
            &self.buffers.path_uniforms,
            0,
            bytemuck::bytes_of(&path_uniforms),
        );

        // Create or recreate vertex buffer if needed
        let vertex_size = (std::mem::size_of::<PathVertex>() * batch.paths.vertices.len()) as u64;
        let need_new_vertex_buffer = match &self.buffers.path_vertices {
            Some(buf) => buf.size() < vertex_size,
            None => true,
        };

        if need_new_vertex_buffer && vertex_size > 0 {
            self.buffers.path_vertices = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Path Vertex Buffer"),
                size: vertex_size,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }

        if let Some(vb) = &self.buffers.path_vertices {
            self.queue
                .write_buffer(vb, 0, bytemuck::cast_slice(&batch.paths.vertices));
        }

        // Create or recreate index buffer if needed
        let index_size = (std::mem::size_of::<u32>() * batch.paths.indices.len()) as u64;
        let need_new_index_buffer = match &self.buffers.path_indices {
            Some(buf) => buf.size() < index_size,
            None => true,
        };

        if need_new_index_buffer && index_size > 0 {
            self.buffers.path_indices = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Path Index Buffer"),
                size: index_size,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }

        if let Some(ib) = &self.buffers.path_indices {
            self.queue
                .write_buffer(ib, 0, bytemuck::cast_slice(&batch.paths.indices));
        }
    }

    /// Render primitives with MSAA (multi-sample anti-aliasing)
    ///
    /// # Arguments
    /// * `msaa_target` - The multisampled texture view to render to
    /// * `resolve_target` - The single-sampled texture view to resolve to
    /// * `batch` - The primitive batch to render
    /// * `clear_color` - RGBA clear color (0.0-1.0 range)
    pub fn render_msaa(
        &mut self,
        msaa_target: &wgpu::TextureView,
        resolve_target: &wgpu::TextureView,
        batch: &PrimitiveBatch,
        clear_color: [f64; 4],
    ) {
        // Update uniforms
        let uniforms = Uniforms {
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            _padding: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.buffers.uniforms, 0, bytemuck::bytes_of(&uniforms));

        // Sort and upload primitives
        let sdf_ranges = self.upload_sorted_primitives(&batch.primitives);

        // Update path buffers if we have path geometry
        let has_paths = !batch.paths.vertices.is_empty() && !batch.paths.indices.is_empty();
        if has_paths {
            self.update_path_buffers(batch);
        }

        // Create command encoder
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Blinc MSAA Render Encoder"),
            });

        // Begin render pass with MSAA resolve
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Blinc MSAA Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: msaa_target,
                    resolve_target: Some(resolve_target),
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear_color[0],
                            g: clear_color[1],
                            b: clear_color[2],
                            a: clear_color[3],
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Render SDF primitives via split pipelines
            if !batch.primitives.is_empty() {
                render_pass.set_bind_group(0, &self.bind_groups.sdf, &[]);
                Self::draw_split_sdf(
                    &mut render_pass,
                    &self.pipelines,
                    &sdf_ranges,
                    false,
                    self.sdf_vb_buffer(),
                );
            }

            // Render paths
            if has_paths {
                if let (Some(vb), Some(ib)) =
                    (&self.buffers.path_vertices, &self.buffers.path_indices)
                {
                    render_pass.set_pipeline(&self.pipelines.path);
                    render_pass.set_bind_group(0, &self.bind_groups.path, &[]);
                    render_pass.set_vertex_buffer(0, vb.slice(..));
                    render_pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                    render_pass.draw_indexed(0..batch.paths.indices.len() as u32, 0, 0..1);
                }
            }
        }

        // Submit commands
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Render glass primitives (requires backdrop texture)
    ///
    /// Splits primitives into simple (frosted) and liquid (refracted) glass,
    /// rendering each with the appropriate shader.
    pub fn render_glass(
        &mut self,
        target: &wgpu::TextureView,
        backdrop: &wgpu::TextureView,
        batch: &PrimitiveBatch,
    ) {
        if batch.glass_primitives.is_empty() || !self.has_storage_buffers {
            return;
        }
        self.ensure_glass_pipelines();

        // Split primitives: simple glass first, then liquid glass
        // This allows us to render each group with its respective pipeline
        let mut simple_primitives: Vec<GpuGlassPrimitive> = Vec::new();
        let mut liquid_primitives: Vec<GpuGlassPrimitive> = Vec::new();

        for prim in &batch.glass_primitives {
            if prim.type_info[0] == GlassType::Simple as u32 {
                simple_primitives.push(*prim);
            } else {
                liquid_primitives.push(*prim);
            }
        }

        let simple_count = simple_primitives.len();
        let liquid_count = liquid_primitives.len();

        if simple_count == 0 && liquid_count == 0 {
            return;
        }

        // Combine: simple primitives first, then liquid primitives
        let mut ordered_primitives = simple_primitives;
        ordered_primitives.extend(liquid_primitives);

        // Ensure glass resources are cached (sampler is reused across frames)
        let current_size = self.viewport_size;

        // Check if we need to create or recreate the cached glass resources
        let need_new_bind_group = match &self.cached_glass {
            None => true,
            Some(cached) => cached.bind_group.is_none() || cached.bind_group_size != current_size,
        };

        if self.cached_glass.is_none() {
            let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("Glass Backdrop Sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            });
            self.cached_glass = Some(CachedGlassResources {
                sampler,
                bind_group: None,
                bind_group_size: (0, 0),
            });
        }

        // Update glass uniforms
        let glass_uniforms = GlassUniforms {
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            time: self.time,
            _padding: 0.0,
        };
        self.queue.write_buffer(
            &self.buffers.glass_uniforms,
            0,
            bytemuck::bytes_of(&glass_uniforms),
        );

        // Update glass primitives buffer with ordered primitives
        self.queue.write_buffer(
            &self.buffers.glass_primitives,
            0,
            bytemuck::cast_slice(&ordered_primitives),
        );

        // Create or reuse glass bind group
        if need_new_bind_group {
            let cached_glass = self.cached_glass.as_ref().unwrap();
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Glass Bind Group"),
                layout: &self.bind_group_layouts.glass,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.buffers.glass_uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.buffers.glass_primitives.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(backdrop),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&cached_glass.sampler),
                    },
                ],
            });

            // Update cache
            if let Some(ref mut cached) = self.cached_glass {
                cached.bind_group = Some(bind_group);
                cached.bind_group_size = current_size;
            }
        }

        let glass_bind_group = self
            .cached_glass
            .as_ref()
            .unwrap()
            .bind_group
            .as_ref()
            .unwrap();

        // Create command encoder
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Blinc Glass Render Encoder"),
            });

        // Begin render pass (load existing content)
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Blinc Glass Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load, // Keep existing content
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Render simple glass primitives with the simple_glass pipeline
            if simple_count > 0 {
                render_pass.set_pipeline(self.effect_pipelines.simple_glass.as_ref().unwrap());
                render_pass.set_bind_group(0, glass_bind_group, &[]);
                render_pass.draw(0..6, 0..simple_count as u32);
            }

            // Render liquid glass primitives with the glass pipeline
            if liquid_count > 0 {
                render_pass.set_pipeline(self.effect_pipelines.glass.as_ref().unwrap());
                render_pass.set_bind_group(0, glass_bind_group, &[]);
                render_pass.draw(
                    0..6,
                    simple_count as u32..(simple_count + liquid_count) as u32,
                );
            }
        }

        // Submit commands
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Render primitives to a backdrop texture for glass blur sampling
    ///
    /// This renders the background primitives to a lower-resolution texture
    /// that glass primitives sample from for their blur effect.
    pub fn render_to_backdrop(
        &mut self,
        backdrop: &wgpu::TextureView,
        _backdrop_size: (u32, u32),
        batch: &PrimitiveBatch,
        has_backdrop_content: bool,
    ) {
        if batch.primitives.is_empty() {
            return;
        }

        // Use full viewport size for coordinate mapping, even though texture is smaller.
        // GPU automatically maps NDC space to the texture size, ensuring primitives
        // appear at correct relative positions for glass sampling.
        let main_uniforms = Uniforms {
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            _padding: [0.0; 2],
        };
        self.queue.write_buffer(
            &self.buffers.uniforms,
            0,
            bytemuck::bytes_of(&main_uniforms),
        );

        // Sort and upload primitives
        let sdf_ranges = self.upload_sorted_primitives(&batch.primitives);

        // Create command encoder
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Backdrop Render Encoder"),
            });

        // Render to backdrop texture
        {
            let backdrop_load = if has_backdrop_content {
                wgpu::LoadOp::Load
            } else {
                wgpu::LoadOp::Clear(wgpu::Color::BLACK)
            };
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Backdrop Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: backdrop,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: backdrop_load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_bind_group(0, &self.bind_groups.sdf, &[]);
            Self::draw_split_sdf(
                &mut render_pass,
                &self.pipelines,
                &sdf_ranges,
                false,
                self.sdf_vb_buffer(),
            );
        }

        // Submit commands
        self.queue.submit(std::iter::once(encoder.finish()));
        // Note: No need to restore uniforms since we're already using main_uniforms
    }

    /// Render glass frame with backdrop and glass primitives in a single encoder submission.
    /// This is more efficient than separate render calls as it reduces command buffer overhead.
    ///
    /// Performs:
    /// 1. Render background primitives to backdrop texture
    /// 2. Render background primitives to target
    /// 3. Render glass primitives with backdrop blur to target
    pub fn render_glass_frame(
        &mut self,
        target: &wgpu::TextureView,
        backdrop: &wgpu::TextureView,
        _backdrop_size: (u32, u32), // Not used - we render with full viewport coords
        batch: &PrimitiveBatch,
        has_backdrop_content: bool,
    ) {
        // Glass effects require storage buffers for per-frame primitive data.
        // On WebGL2 (no storage buffers), skip glass rendering — the glass DT
        // shader exists but needs a per-frame glass data texture + bind group
        // plumbing that isn't implemented yet.
        if !self.has_storage_buffers {
            return;
        }
        self.ensure_glass_pipelines();

        // Update uniforms for rendering (always use full viewport size)
        // The GPU maps NDC space to actual texture size automatically
        let main_uniforms = Uniforms {
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            _padding: [0.0; 2],
        };

        // Update auxiliary data buffer
        self.update_aux_data_buffer(batch);

        // Sort and upload primitives
        let sdf_ranges = self.upload_sorted_primitives(&batch.primitives);

        // Split glass primitives into simple and liquid for separate rendering
        let mut simple_primitives: Vec<GpuGlassPrimitive> = Vec::new();
        let mut liquid_primitives: Vec<GpuGlassPrimitive> = Vec::new();
        for prim in &batch.glass_primitives {
            if prim.type_info[0] == GlassType::Simple as u32 {
                simple_primitives.push(*prim);
            } else {
                liquid_primitives.push(*prim);
            }
        }
        let simple_count = simple_primitives.len();
        let liquid_count = liquid_primitives.len();

        // Combine: simple first, then liquid
        let mut ordered_glass_primitives = simple_primitives;
        ordered_glass_primitives.extend(liquid_primitives);

        // Update glass primitives buffer with ordered primitives
        if !ordered_glass_primitives.is_empty() {
            self.queue.write_buffer(
                &self.buffers.glass_primitives,
                0,
                bytemuck::cast_slice(&ordered_glass_primitives),
            );
        }

        // Update glass uniforms
        let glass_uniforms = GlassUniforms {
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            time: self.time,
            _padding: 0.0,
        };
        self.queue.write_buffer(
            &self.buffers.glass_uniforms,
            0,
            bytemuck::bytes_of(&glass_uniforms),
        );

        // Ensure glass bind group is cached
        let current_size = self.viewport_size;
        let need_new_bind_group = match &self.cached_glass {
            None => true,
            Some(cached) => cached.bind_group.is_none() || cached.bind_group_size != current_size,
        };

        if self.cached_glass.is_none() {
            let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("Glass Backdrop Sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            });
            self.cached_glass = Some(CachedGlassResources {
                sampler,
                bind_group: None,
                bind_group_size: (0, 0),
            });
        }

        if need_new_bind_group {
            let cached_glass = self.cached_glass.as_ref().unwrap();
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Glass Bind Group"),
                layout: &self.bind_group_layouts.glass,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.buffers.glass_uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.buffers.glass_primitives.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(backdrop),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&cached_glass.sampler),
                    },
                ],
            });
            if let Some(ref mut cached) = self.cached_glass {
                cached.bind_group = Some(bind_group);
                cached.bind_group_size = current_size;
            }
        }

        // Create single command encoder for entire frame
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Blinc Glass Frame Encoder"),
            });

        // Pass 1: Render background primitives to backdrop texture (at half resolution)
        // NOTE: We use main_uniforms (full viewport size) for coordinate mapping,
        // even though the texture is half resolution. The GPU automatically maps
        // NDC space to the texture size. This ensures primitives appear at correct
        // relative positions for glass sampling.
        {
            self.queue.write_buffer(
                &self.buffers.uniforms,
                0,
                bytemuck::bytes_of(&main_uniforms),
            );

            let backdrop_load = if has_backdrop_content {
                wgpu::LoadOp::Load
            } else {
                wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
            };
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Backdrop Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: backdrop,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: backdrop_load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            if !batch.primitives.is_empty() {
                render_pass.set_bind_group(0, &self.bind_groups.sdf, &[]);
                Self::draw_split_sdf(
                    &mut render_pass,
                    &self.pipelines,
                    &sdf_ranges,
                    false,
                    self.sdf_vb_buffer(),
                );
            }
        }

        // Pass 2: Render background primitives to target (at full resolution)
        {
            self.queue.write_buffer(
                &self.buffers.uniforms,
                0,
                bytemuck::bytes_of(&main_uniforms),
            );

            let target_load = if has_backdrop_content {
                wgpu::LoadOp::Load
            } else {
                wgpu::LoadOp::Clear(wgpu::Color::BLACK)
            };
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Target Background Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: target_load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            if !batch.primitives.is_empty() {
                render_pass.set_bind_group(0, &self.bind_groups.sdf, &[]);
                Self::draw_split_sdf(
                    &mut render_pass,
                    &self.pipelines,
                    &sdf_ranges,
                    false,
                    self.sdf_vb_buffer(),
                );
            }
        }

        // Pass 3: Render glass primitives with backdrop blur
        if simple_count > 0 || liquid_count > 0 {
            let glass_bind_group = self
                .cached_glass
                .as_ref()
                .unwrap()
                .bind_group
                .as_ref()
                .unwrap();

            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Glass Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Render simple glass primitives with simple_glass pipeline
            if simple_count > 0 {
                render_pass.set_pipeline(self.effect_pipelines.simple_glass.as_ref().unwrap());
                render_pass.set_bind_group(0, glass_bind_group, &[]);
                render_pass.draw(0..6, 0..simple_count as u32);
            }

            // Render liquid glass primitives with glass pipeline
            if liquid_count > 0 {
                render_pass.set_pipeline(self.effect_pipelines.glass.as_ref().unwrap());
                render_pass.set_bind_group(0, glass_bind_group, &[]);
                render_pass.draw(
                    0..6,
                    simple_count as u32..(simple_count + liquid_count) as u32,
                );
            }
        }

        // Submit background and glass passes first
        self.queue.submit(std::iter::once(encoder.finish()));

        // Pass 3b: Render nested glass primitives (glass inside glass)
        // These are glass elements that are children of other glass elements.
        // They render after parent glass, sampling from the same backdrop.
        if !batch.nested_glass_primitives.is_empty() {
            // Split nested glass into simple and liquid
            let mut nested_simple: Vec<GpuGlassPrimitive> = Vec::new();
            let mut nested_liquid: Vec<GpuGlassPrimitive> = Vec::new();
            for prim in &batch.nested_glass_primitives {
                if prim.type_info[0] == GlassType::Simple as u32 {
                    nested_simple.push(*prim);
                } else {
                    nested_liquid.push(*prim);
                }
            }
            let nested_simple_count = nested_simple.len();
            let nested_liquid_count = nested_liquid.len();

            // Combine: simple first, then liquid
            let mut ordered_nested = nested_simple;
            ordered_nested.extend(nested_liquid);

            // Upload nested glass primitives to buffer
            self.queue.write_buffer(
                &self.buffers.glass_primitives,
                0,
                bytemuck::cast_slice(&ordered_nested),
            );

            // Recreate bind group since glass_primitives buffer contents changed
            {
                let cached_glass = self.cached_glass.as_ref().unwrap();
                let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Nested Glass Bind Group"),
                    layout: &self.bind_group_layouts.glass,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self.buffers.glass_uniforms.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: self.buffers.glass_primitives.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(backdrop),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::Sampler(&cached_glass.sampler),
                        },
                    ],
                });
                if let Some(ref mut cached) = self.cached_glass {
                    cached.bind_group = Some(bind_group);
                }
            }

            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Blinc Nested Glass Encoder"),
                });

            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Nested Glass Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            let nested_bind_group = self
                .cached_glass
                .as_ref()
                .unwrap()
                .bind_group
                .as_ref()
                .unwrap();

            if nested_simple_count > 0 {
                render_pass.set_pipeline(self.effect_pipelines.simple_glass.as_ref().unwrap());
                render_pass.set_bind_group(0, nested_bind_group, &[]);
                render_pass.draw(0..6, 0..nested_simple_count as u32);
            }

            if nested_liquid_count > 0 {
                render_pass.set_pipeline(self.effect_pipelines.glass.as_ref().unwrap());
                render_pass.set_bind_group(0, nested_bind_group, &[]);
                render_pass.draw(
                    0..6,
                    nested_simple_count as u32..(nested_simple_count + nested_liquid_count) as u32,
                );
            }

            drop(render_pass);
            self.queue.submit(std::iter::once(encoder.finish()));
        }

        // Pass 4: Render foreground primitives (on top of glass)
        // This requires a separate submission because we need to overwrite the primitives buffer
        if !batch.foreground_primitives.is_empty() {
            // Sort and upload foreground primitives
            let fg_ranges = self.upload_sorted_primitives(&batch.foreground_primitives);

            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Blinc Foreground Encoder"),
                });

            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Foreground Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_bind_group(0, &self.bind_groups.sdf, &[]);
            Self::draw_split_sdf(
                &mut render_pass,
                &self.pipelines,
                &fg_ranges,
                false,
                self.sdf_vb_buffer(),
            );

            drop(render_pass);
            self.queue.submit(std::iter::once(encoder.finish()));
        }

        // Pass 5: Render paths (SVGs) on top of glass
        // Paths are tessellated geometry that need their own pipeline
        let has_paths = !batch.paths.vertices.is_empty() && !batch.paths.indices.is_empty();
        if has_paths {
            // Update path buffers (creates/resizes as needed)
            self.update_path_buffers(batch);

            // Render paths
            if let (Some(vb), Some(ib)) = (&self.buffers.path_vertices, &self.buffers.path_indices)
            {
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Blinc Glass Path Encoder"),
                        });

                let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Glass Path Render Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });

                // Use overlay path pipeline (1x sampled, no MSAA)
                render_pass.set_pipeline(&self.pipelines.path_overlay);
                render_pass.set_bind_group(0, &self.bind_groups.path, &[]);
                render_pass.set_vertex_buffer(0, vb.slice(..));
                render_pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                render_pass.draw_indexed(0..batch.paths.indices.len() as u32, 0, 0..1);

                drop(render_pass);
                self.queue.submit(std::iter::once(encoder.finish()));
            }
        }
    }

    /// Render primitives as an overlay on existing content (1x sampled)
    ///
    /// This uses the overlay pipeline which is configured for sample_count=1,
    /// making it suitable for rendering on top of already-resolved content
    /// (e.g., after glass effects have been applied).
    ///
    /// # Arguments
    /// * `target` - The single-sampled texture view to render to (existing content is preserved)
    /// * `batch` - The primitive batch to render
    pub fn render_overlay(&mut self, target: &wgpu::TextureView, batch: &PrimitiveBatch) {
        // Check if we have layer commands with effects or blend modes that need processing
        let has_layer_processing = batch.layer_commands.iter().any(|entry| {
            if let crate::primitives::LayerCommand::Push { config } = &entry.command {
                !config.effects.is_empty() || config.blend_mode != blinc_core::BlendMode::Normal
            } else {
                false
            }
        });

        // If we have layer effects or blend modes, use the layer-aware rendering path
        if has_layer_processing {
            self.render_overlay_with_layer_effects(target, batch);
            return;
        }

        // Standard overlay rendering (no layer effects)
        // Update uniforms
        let uniforms = Uniforms {
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            _padding: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.buffers.uniforms, 0, bytemuck::bytes_of(&uniforms));

        // Update auxiliary data buffer
        self.update_aux_data_buffer(batch);

        // Sort and upload primitives
        let sdf_ranges = self.upload_sorted_primitives(&batch.primitives);

        // Update path buffers if we have path geometry
        let has_paths = !batch.paths.vertices.is_empty() && !batch.paths.indices.is_empty();
        if has_paths {
            self.update_path_buffers(batch);
        }

        // Create command encoder
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Blinc Overlay Render Encoder"),
            });

        // Begin render pass (load existing content, don't clear)
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Blinc Overlay Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None, // No MSAA resolve needed for overlay
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load, // Keep existing content
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Render paths first (they're typically backgrounds)
            if has_paths {
                if let (Some(vb), Some(ib)) =
                    (&self.buffers.path_vertices, &self.buffers.path_indices)
                {
                    render_pass.set_pipeline(&self.pipelines.path_overlay);
                    render_pass.set_bind_group(0, &self.bind_groups.path, &[]);
                    render_pass.set_vertex_buffer(0, vb.slice(..));
                    render_pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                    render_pass.draw_indexed(0..batch.paths.indices.len() as u32, 0, 0..1);
                }
            }

            // Render SDF primitives using split overlay pipelines
            if !batch.primitives.is_empty() {
                render_pass.set_bind_group(0, &self.bind_groups.sdf, &[]);
                Self::draw_split_sdf(
                    &mut render_pass,
                    &self.pipelines,
                    &sdf_ranges,
                    true,
                    self.sdf_vb_buffer(),
                );
            }
        }

        // Submit commands
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Render overlay with layer effect/blend-mode processing
    ///
    /// Follows the same pattern as render_with_layer_effects:
    /// 1. Build list of layers that need processing (effects or blend modes)
    /// 2. Render non-layer primitives normally (overlay = LoadOp::Load)
    /// 3. For each layer, render to tight offscreen texture, apply effects, blit at position
    fn render_overlay_with_layer_effects(
        &mut self,
        target: &wgpu::TextureView,
        batch: &PrimitiveBatch,
    ) {
        use crate::primitives::LayerCommand;

        // Build list of layers with their primitive ranges
        let mut effect_layers: Vec<(usize, usize, blinc_core::LayerConfig)> = Vec::new();
        let mut layer_stack: Vec<(usize, blinc_core::LayerConfig)> = Vec::new();

        for entry in &batch.layer_commands {
            match &entry.command {
                LayerCommand::Push { config } => {
                    layer_stack.push((entry.primitive_index, config.clone()));
                }
                LayerCommand::Pop => {
                    if let Some((start_idx, config)) = layer_stack.pop() {
                        if !config.effects.is_empty()
                            || config.blend_mode != blinc_core::BlendMode::Normal
                            || config.transform_3d.is_some()
                        {
                            effect_layers.push((start_idx, entry.primitive_index, config));
                        }
                    }
                }
                LayerCommand::Sample { .. } => {}
            }
        }

        if effect_layers.is_empty() {
            self.render_overlay_simple(target, batch);
            return;
        }

        // Build set of primitive indices that belong to effect/blend layers (skip in first pass)
        let mut effect_primitives = std::collections::HashSet::new();
        for (start, end, _) in &effect_layers {
            for i in *start..*end {
                effect_primitives.insert(i);
            }
        }

        // First pass: render primitives NOT in effect/blend layers (overlay = Load)
        self.render_overlay_primitives_excluding(target, batch, &effect_primitives);
        drop(effect_primitives);

        // Process each effect/blend layer
        for (start_idx, end_idx, config) in effect_layers {
            if start_idx >= end_idx || end_idx > batch.primitives.len() {
                continue;
            }

            // Compute bounding box from primitives (screen coordinates)
            let primitives = &batch.primitives[start_idx..end_idx];
            let (layer_pos, layer_size, layer_clip) = if primitives.is_empty() {
                let pos = config.position.map(|p| (p.x, p.y)).unwrap_or((0.0, 0.0));
                let size = config
                    .size
                    .map(|s| (s.width, s.height))
                    .unwrap_or((self.viewport_size.0 as f32, self.viewport_size.1 as f32));
                (pos, size, None)
            } else {
                let mut min_x = f32::MAX;
                let mut min_y = f32::MAX;
                let mut max_x = f32::MIN;
                let mut max_y = f32::MIN;
                let mut clip: Option<([f32; 4], [f32; 4])> = None;
                for p in primitives {
                    let (px, py, pw, ph) = (p.bounds[0], p.bounds[1], p.bounds[2], p.bounds[3]);
                    min_x = min_x.min(px);
                    min_y = min_y.min(py);
                    max_x = max_x.max(px + pw);
                    max_y = max_y.max(py + ph);
                    if clip.is_none() && p.clip_bounds[0] > -5000.0 && p.clip_bounds[2] < 90000.0 {
                        clip = Some((p.clip_bounds, p.clip_radius));
                    }
                }
                let width = (max_x - min_x).max(1.0);
                let height = (max_y - min_y).max(1.0);
                ((min_x, min_y), (width, height), clip)
            };

            // Skip layers entirely outside the viewport
            let vp_w = self.viewport_size.0 as f32;
            let vp_h = self.viewport_size.1 as f32;
            let is_visible = layer_pos.0 < vp_w
                && layer_pos.1 < vp_h
                && layer_pos.0 + layer_size.0 > 0.0
                && layer_pos.1 + layer_size.1 > 0.0
                && layer_size.0 > 0.0
                && layer_size.1 > 0.0;

            if !is_visible {
                continue;
            }

            let effect_expansion = Self::calculate_effect_expansion(&config.effects);

            // Render layer primitives to tight texture with offset
            let (layer_texture, content_size) = self.render_primitive_range_tight(
                batch,
                start_idx,
                end_idx,
                layer_pos,
                layer_size,
                effect_expansion,
            );

            let tight_size = content_size;
            let expanded_pos = (
                layer_pos.0 - effect_expansion.0,
                layer_pos.1 - effect_expansion.1,
            );
            let expanded_size = (
                layer_size.0 + effect_expansion.0 + effect_expansion.2,
                layer_size.1 + effect_expansion.1 + effect_expansion.3,
            );

            if config.effects.is_empty() {
                // Blend-mode only: blit directly
                self.blit_tight_texture_to_target(
                    &layer_texture.view,
                    tight_size,
                    target,
                    expanded_pos,
                    expanded_size,
                    config.opacity,
                    config.blend_mode,
                    layer_clip,
                    config.transform_3d,
                );
                self.layer_texture_cache.release(layer_texture);
            } else {
                // Apply effects then blit
                let effected = self.apply_layer_effects(&layer_texture, &config.effects);
                self.layer_texture_cache.release(layer_texture);

                self.blit_tight_texture_to_target(
                    &effected.view,
                    tight_size,
                    target,
                    expanded_pos,
                    expanded_size,
                    config.opacity,
                    config.blend_mode,
                    layer_clip,
                    config.transform_3d,
                );
                self.layer_texture_cache.release(effected);
            }
        }
    }

    /// Render overlay primitives excluding those in the given set (LoadOp::Load)
    fn render_overlay_primitives_excluding(
        &mut self,
        target: &wgpu::TextureView,
        batch: &PrimitiveBatch,
        exclude: &std::collections::HashSet<usize>,
    ) {
        if exclude.is_empty() {
            self.render_overlay_simple(target, batch);
            return;
        }

        let included_primitives: Vec<GpuPrimitive> = batch
            .primitives
            .iter()
            .enumerate()
            .filter(|(i, _)| !exclude.contains(i))
            .map(|(_, p)| *p)
            .collect();

        // Update uniforms
        let uniforms = Uniforms {
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            _padding: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.buffers.uniforms, 0, bytemuck::bytes_of(&uniforms));

        // Update auxiliary data buffer
        self.update_aux_data_buffer(batch);

        // Update path buffers
        let has_paths = !batch.paths.vertices.is_empty() && !batch.paths.indices.is_empty();
        if has_paths {
            self.update_path_buffers(batch);
        }

        // Sort and upload filtered primitives
        let sdf_ranges = self.upload_sorted_primitives(&included_primitives);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Blinc Overlay Excluding Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Blinc Overlay Excluding Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Render paths first
            if has_paths {
                if let (Some(vb), Some(ib)) =
                    (&self.buffers.path_vertices, &self.buffers.path_indices)
                {
                    render_pass.set_pipeline(&self.pipelines.path_overlay);
                    render_pass.set_bind_group(0, &self.bind_groups.path, &[]);
                    render_pass.set_vertex_buffer(0, vb.slice(..));
                    render_pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                    render_pass.draw_indexed(0..batch.paths.indices.len() as u32, 0, 0..1);
                }
            }

            // Render filtered SDF primitives via split overlay pipelines
            if !included_primitives.is_empty() {
                render_pass.set_bind_group(0, &self.bind_groups.sdf, &[]);
                Self::draw_split_sdf(
                    &mut render_pass,
                    &self.pipelines,
                    &sdf_ranges,
                    true,
                    self.sdf_vb_buffer(),
                );
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Simple overlay render without layer effect processing
    fn render_overlay_simple(&mut self, target: &wgpu::TextureView, batch: &PrimitiveBatch) {
        // Update uniforms
        let uniforms = Uniforms {
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            _padding: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.buffers.uniforms, 0, bytemuck::bytes_of(&uniforms));

        // Sort and upload primitives
        let sdf_ranges = self.upload_sorted_primitives(&batch.primitives);

        // Update path buffers if we have path geometry
        let has_paths = !batch.paths.vertices.is_empty() && !batch.paths.indices.is_empty();
        if has_paths {
            self.update_path_buffers(batch);
        }

        // Create command encoder
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Blinc Overlay Simple Render Encoder"),
            });

        // Begin render pass (load existing content, don't clear)
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Blinc Overlay Simple Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Render paths first
            if has_paths {
                if let (Some(vb), Some(ib)) =
                    (&self.buffers.path_vertices, &self.buffers.path_indices)
                {
                    render_pass.set_pipeline(&self.pipelines.path_overlay);
                    render_pass.set_bind_group(0, &self.bind_groups.path, &[]);
                    render_pass.set_vertex_buffer(0, vb.slice(..));
                    render_pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                    render_pass.draw_indexed(0..batch.paths.indices.len() as u32, 0, 0..1);
                }
            }

            // Render SDF primitives via split overlay pipelines
            if !batch.primitives.is_empty() {
                render_pass.set_bind_group(0, &self.bind_groups.sdf, &[]);
                Self::draw_split_sdf(
                    &mut render_pass,
                    &self.pipelines,
                    &sdf_ranges,
                    true,
                    self.sdf_vb_buffer(),
                );
            }
        }

        // Submit commands
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Render a slice of primitives as overlay (LoadOp::Load, keeps existing content)
    ///
    /// This is used for interleaved z-layer rendering where primitives need
    /// to be rendered per-layer to properly interleave with text.
    /// Uses `self.bind_groups.sdf` which automatically includes the real glyph
    /// atlas when `set_glyph_atlas()` was called at the start of the frame.
    pub fn render_primitives_overlay(
        &mut self,
        target: &wgpu::TextureView,
        primitives: &[GpuPrimitive],
    ) {
        if primitives.is_empty() {
            return;
        }

        // Update uniforms
        let uniforms = Uniforms {
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            _padding: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.buffers.uniforms, 0, bytemuck::bytes_of(&uniforms));

        // Sort and upload primitives
        let sdf_ranges = self.upload_sorted_primitives(primitives);

        // Create command encoder
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Blinc Layer Primitives Encoder"),
            });

        // Begin render pass (load existing content)
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Blinc Layer Primitives Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Render SDF primitives via split overlay pipelines
            render_pass.set_bind_group(0, &self.bind_groups.sdf, &[]);
            Self::draw_split_sdf(
                &mut render_pass,
                &self.pipelines,
                &sdf_ranges,
                true,
                self.sdf_vb_buffer(),
            );
        }

        // Submit commands
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Render paths (tessellated geometry like SVGs) as an overlay
    ///
    /// This renders paths on top of existing content without clearing.
    /// Used for z-layered rendering where paths need to be rendered separately.
    pub fn render_paths_overlay(&mut self, target: &wgpu::TextureView, batch: &PrimitiveBatch) {
        let has_paths = !batch.paths.vertices.is_empty() && !batch.paths.indices.is_empty();
        if !has_paths {
            return;
        }

        // Update path buffers
        self.update_path_buffers(batch);

        // Render paths
        if let (Some(vb), Some(ib)) = (&self.buffers.path_vertices, &self.buffers.path_indices) {
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Blinc Paths Overlay Encoder"),
                });

            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Paths Overlay Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Use overlay path pipeline (1x sampled)
            render_pass.set_pipeline(&self.pipelines.path_overlay);
            render_pass.set_bind_group(0, &self.bind_groups.path, &[]);
            render_pass.set_vertex_buffer(0, vb.slice(..));
            render_pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
            render_pass.draw_indexed(0..batch.paths.indices.len() as u32, 0, 0..1);

            drop(render_pass);
            self.queue.submit(std::iter::once(encoder.finish()));
        }
    }

    /// Render SDF primitives with unified text rendering (text as primitives)
    ///
    /// This method renders SDF primitives including text glyphs in a single pass.
    /// Text primitives (PrimitiveType::Text) sample from the provided glyph atlases.
    /// Uses `set_glyph_atlas()` to bind the real atlas, then delegates to
    /// `render_primitives_overlay()`.
    pub fn render_primitives_overlay_with_glyphs(
        &mut self,
        target: &wgpu::TextureView,
        primitives: &[GpuPrimitive],
        atlas_view: &wgpu::TextureView,
        color_atlas_view: &wgpu::TextureView,
    ) {
        self.set_glyph_atlas(atlas_view, color_atlas_view);
        self.render_primitives_overlay(target, primitives);
    }

    /// Render overlay primitives with MSAA anti-aliasing
    ///
    /// This method renders paths/primitives to a temporary MSAA texture,
    /// resolves it, and then blends onto the target. This provides smooth
    /// edges for tessellated paths that don't have shader-based AA.
    ///
    /// # Arguments
    /// * `target` - The single-sampled texture view to render to (existing content is preserved)
    /// * `batch` - The primitive batch to render
    /// * `sample_count` - MSAA sample count (typically 4)
    pub fn render_overlay_msaa(
        &mut self,
        target: &wgpu::TextureView,
        batch: &PrimitiveBatch,
        sample_count: u32,
    ) {
        if batch.paths.vertices.is_empty() && batch.primitives.is_empty() {
            return;
        }

        // Ensure we have MSAA pipelines for this sample count
        let need_new_pipelines = match &self.msaa_pipelines {
            Some(p) => p.sample_count != sample_count,
            None => true,
        };
        if need_new_pipelines && sample_count > 1 {
            self.msaa_pipelines = Some(Self::create_msaa_pipelines(
                &self.device,
                &self.bind_group_layouts,
                self.texture_format,
                sample_count,
                self.has_vertex_storage,
                self.has_storage_buffers,
            ));
        }

        let (width, height) = self.viewport_size;

        // Check if we need to recreate cached MSAA textures
        let need_new_textures = match &self.cached_msaa {
            Some(cached) => {
                cached.width != width
                    || cached.height != height
                    || cached.sample_count != sample_count
            }
            None => true,
        };

        if need_new_textures {
            // Create MSAA texture for rendering
            let msaa_texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Overlay MSAA Texture"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count,
                dimension: wgpu::TextureDimension::D2,
                format: self.texture_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let msaa_view = msaa_texture.create_view(&wgpu::TextureViewDescriptor::default());

            // Create resolve texture
            let resolve_texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Overlay Resolve Texture"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.texture_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let resolve_view = resolve_texture.create_view(&wgpu::TextureViewDescriptor::default());

            // Create sampler (reused across frames)
            let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("Overlay Blend Sampler"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });

            // Create composite uniforms (opacity=1.0, blend_mode=normal)
            #[repr(C)]
            #[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
            struct CompositeUniforms {
                opacity: f32,
                blend_mode: u32,
                _padding: [f32; 2],
            }
            let composite_uniforms = CompositeUniforms {
                opacity: 1.0,
                blend_mode: 0,
                _padding: [0.0; 2],
            };
            let composite_uniform_buffer =
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Composite Uniforms Buffer"),
                        contents: bytemuck::bytes_of(&composite_uniforms),
                        usage: wgpu::BufferUsages::UNIFORM,
                    });

            // Create bind group for compositing
            let composite_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Overlay Composite Bind Group"),
                layout: &self.bind_group_layouts.composite,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: composite_uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&resolve_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });

            self.cached_msaa = Some(CachedMsaaTextures {
                msaa_texture,
                msaa_view,
                resolve_texture,
                resolve_view,
                width,
                height,
                sample_count,
                sampler,
                composite_uniform_buffer,
                composite_bind_group,
            });
        }

        // Update uniforms
        let uniforms = Uniforms {
            viewport_size: [width as f32, height as f32],
            _padding: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.buffers.uniforms, 0, bytemuck::bytes_of(&uniforms));

        // Sort and upload primitives
        let sdf_ranges = self.upload_sorted_primitives(&batch.primitives);

        // Update path buffers
        let has_paths = !batch.paths.vertices.is_empty() && !batch.paths.indices.is_empty();
        if has_paths {
            self.update_path_buffers(batch);
        }

        // Get references to the cached textures (after mutable borrows are done)
        let cached = self.cached_msaa.as_ref().unwrap();

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Overlay MSAA Render Encoder"),
            });

        // Pass 1: Render paths + SDF primitives to an offscreen texture.
        //
        // WebGPU spec: `resolveTarget` MUST be None when the color
        // attachment's sample_count is 1. Chrome/Dawn silently accept
        // a stray `resolve_target: Some(...)` on a single-sampled
        // view, but Safari/WebKit rejects the render pass entirely —
        // which is why every path draw (notches, SVG strokes, custom
        // paths) was invisible on Safari.
        //
        // Fix: when sample_count == 1, render directly into
        // `resolve_view` (a single-sampled texture with both
        // RENDER_ATTACHMENT and TEXTURE_BINDING usage) and pass
        // `resolve_target: None`. When sample_count > 1, use the
        // multisampled `msaa_view` with a resolve into `resolve_view`
        // as before.
        let (pass1_view, pass1_resolve, pass1_store) = if sample_count > 1 {
            // Multisampled: keep the resolved single-sample texture,
            // discard the MSAA texture (we only wanted its resolved
            // output, not the per-sample content).
            (
                &cached.msaa_view,
                Some(&cached.resolve_view),
                wgpu::StoreOp::Discard,
            )
        } else {
            // Single-sampled: render directly into the resolve_view
            // and keep the content so pass 2 can sample from it.
            (&cached.resolve_view, None, wgpu::StoreOp::Store)
        };
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Overlay MSAA Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: pass1_view,
                    resolve_target: pass1_resolve,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: pass1_store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Get the appropriate path pipeline for the sample count
            let path_pipeline = if sample_count > 1 {
                if let Some(ref msaa) = self.msaa_pipelines {
                    &msaa.path
                } else {
                    &self.pipelines.path
                }
            } else {
                &self.pipelines.path
            };

            // Render paths using MSAA pipeline
            if has_paths {
                if let (Some(vb), Some(ib)) =
                    (&self.buffers.path_vertices, &self.buffers.path_indices)
                {
                    render_pass.set_pipeline(path_pipeline);
                    render_pass.set_bind_group(0, &self.bind_groups.path, &[]);
                    render_pass.set_vertex_buffer(0, vb.slice(..));
                    render_pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                    render_pass.draw_indexed(0..batch.paths.indices.len() as u32, 0, 0..1);
                }
            }

            // Render SDF primitives using split MSAA pipelines
            if !batch.primitives.is_empty() {
                render_pass.set_bind_group(0, &self.bind_groups.sdf, &[]);
                if sample_count > 1 {
                    if let Some(ref msaa) = self.msaa_pipelines {
                        Self::draw_split_sdf_msaa(
                            &mut render_pass,
                            msaa,
                            &sdf_ranges,
                            self.sdf_vb_buffer(),
                        );
                    } else {
                        Self::draw_split_sdf(
                            &mut render_pass,
                            &self.pipelines,
                            &sdf_ranges,
                            false,
                            self.sdf_vb_buffer(),
                        );
                    }
                } else {
                    Self::draw_split_sdf(
                        &mut render_pass,
                        &self.pipelines,
                        &sdf_ranges,
                        false,
                        self.sdf_vb_buffer(),
                    );
                }
            }
        }

        // Pass 2: Blend resolved texture onto target using cached resources
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Overlay Blend Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load, // Keep existing content
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(&self.pipelines.composite_overlay);
            render_pass.set_bind_group(0, &cached.composite_bind_group, &[]);
            render_pass.draw(0..3, 0..1); // Fullscreen triangle
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Render only paths with MSAA anti-aliasing
    ///
    /// This is used when SDF primitives are rendered separately (unified rendering mode)
    /// but paths still need MSAA for smooth edges.
    pub fn render_paths_overlay_msaa(
        &mut self,
        target: &wgpu::TextureView,
        batch: &PrimitiveBatch,
        sample_count: u32,
    ) {
        if batch.paths.vertices.is_empty() || batch.paths.indices.is_empty() {
            return;
        }

        // Ensure we have MSAA pipelines for this sample count
        let need_new_pipelines = match &self.msaa_pipelines {
            Some(p) => p.sample_count != sample_count,
            None => true,
        };
        if need_new_pipelines && sample_count > 1 {
            self.msaa_pipelines = Some(Self::create_msaa_pipelines(
                &self.device,
                &self.bind_group_layouts,
                self.texture_format,
                sample_count,
                self.has_vertex_storage,
                self.has_storage_buffers,
            ));
        }

        let (width, height) = self.viewport_size;

        // Check if we need to recreate cached MSAA textures
        let need_new_textures = match &self.cached_msaa {
            Some(cached) => {
                cached.width != width
                    || cached.height != height
                    || cached.sample_count != sample_count
            }
            None => true,
        };

        if need_new_textures {
            // Create MSAA texture for rendering
            let msaa_texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Path MSAA Texture"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count,
                dimension: wgpu::TextureDimension::D2,
                format: self.texture_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let msaa_view = msaa_texture.create_view(&wgpu::TextureViewDescriptor::default());

            // Create resolve texture
            let resolve_texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Path Resolve Texture"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.texture_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let resolve_view = resolve_texture.create_view(&wgpu::TextureViewDescriptor::default());

            // Create sampler
            let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("Path Blend Sampler"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });

            // Create composite uniforms
            #[repr(C)]
            #[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
            struct CompositeUniforms {
                opacity: f32,
                blend_mode: u32,
                _padding: [f32; 2],
            }
            let composite_uniforms = CompositeUniforms {
                opacity: 1.0,
                blend_mode: 0,
                _padding: [0.0; 2],
            };
            let composite_uniform_buffer =
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Path Composite Uniforms Buffer"),
                        contents: bytemuck::bytes_of(&composite_uniforms),
                        usage: wgpu::BufferUsages::UNIFORM,
                    });

            // Create bind group for compositing
            let composite_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Path Composite Bind Group"),
                layout: &self.bind_group_layouts.composite,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: composite_uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&resolve_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });

            self.cached_msaa = Some(CachedMsaaTextures {
                msaa_texture,
                msaa_view,
                resolve_texture,
                resolve_view,
                width,
                height,
                sample_count,
                sampler,
                composite_uniform_buffer,
                composite_bind_group,
            });
        }

        // Update uniforms
        let uniforms = Uniforms {
            viewport_size: [width as f32, height as f32],
            _padding: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.buffers.uniforms, 0, bytemuck::bytes_of(&uniforms));

        // Update path buffers
        self.update_path_buffers(batch);

        // Get references to the cached textures
        let cached = self.cached_msaa.as_ref().unwrap();

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Path MSAA Render Encoder"),
            });

        // Pass 1: Render paths to an offscreen texture.
        //
        // See the longer comment in `render_overlay_msaa` — the key
        // constraint is that `resolveTarget` must be None when the
        // color attachment is single-sampled (WebGPU spec). Safari
        // enforces this; Chrome accepts a stray `Some(...)` silently.
        let (pass1_view, pass1_resolve, pass1_store) = if sample_count > 1 {
            (
                &cached.msaa_view,
                Some(&cached.resolve_view),
                wgpu::StoreOp::Discard,
            )
        } else {
            (&cached.resolve_view, None, wgpu::StoreOp::Store)
        };
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Path MSAA Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: pass1_view,
                    resolve_target: pass1_resolve,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: pass1_store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Get the appropriate pipeline for the sample count
            let path_pipeline = if sample_count > 1 {
                if let Some(ref msaa) = self.msaa_pipelines {
                    &msaa.path
                } else {
                    &self.pipelines.path
                }
            } else {
                &self.pipelines.path
            };

            if let (Some(vb), Some(ib)) = (&self.buffers.path_vertices, &self.buffers.path_indices)
            {
                render_pass.set_pipeline(path_pipeline);
                render_pass.set_bind_group(0, &self.bind_groups.path, &[]);
                render_pass.set_vertex_buffer(0, vb.slice(..));
                render_pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                render_pass.draw_indexed(0..batch.paths.indices.len() as u32, 0, 0..1);
            }
        }

        // Pass 2: Blend resolved texture onto target
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Path Blend Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(&self.pipelines.composite_overlay);
            render_pass.set_bind_group(0, &cached.composite_bind_group, &[]);
            render_pass.draw(0..3, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Render text glyphs with a provided atlas texture
    ///
    /// # Arguments
    /// * `target` - The texture view to render to
    /// * `glyphs` - The glyph instances to render
    /// * `atlas_view` - The grayscale glyph atlas texture view
    /// * `color_atlas_view` - The color (RGBA) glyph atlas texture view for emoji
    /// * `atlas_sampler` - The sampler for the atlases
    pub fn render_text(
        &mut self,
        target: &wgpu::TextureView,
        glyphs: &[GpuGlyph],
        atlas_view: &wgpu::TextureView,
        color_atlas_view: &wgpu::TextureView,
        atlas_sampler: &wgpu::Sampler,
    ) {
        if glyphs.is_empty() {
            return;
        }

        // Update uniforms
        let uniforms = Uniforms {
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            _padding: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.buffers.uniforms, 0, bytemuck::bytes_of(&uniforms));

        // Update glyphs: storage buffer or data texture
        if self.has_storage_buffers {
            self.queue
                .write_buffer(&self.buffers.glyphs, 0, bytemuck::cast_slice(glyphs));
        } else if let Some(ref tex) = self.buffers.glyph_data_texture {
            if !glyphs.is_empty() {
                let bytes = bytemuck::cast_slice(glyphs);
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(6 * 16), // 6 texels × 16 bytes per Rgba32Float = 96 bytes = sizeof(GpuGlyph)
                        rows_per_image: None,
                    },
                    wgpu::Extent3d {
                        width: 6,
                        height: glyphs.len() as u32,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }

        // Check if we need to recreate the text bind group
        // Invalidate if either atlas view pointer changed (texture was recreated)
        let atlas_view_ptr = atlas_view as *const wgpu::TextureView;
        let color_atlas_view_ptr = color_atlas_view as *const wgpu::TextureView;
        let need_new_bind_group = match &self.cached_text {
            Some(cached) => {
                cached.atlas_view_ptr != atlas_view_ptr
                    || cached.color_atlas_view_ptr != color_atlas_view_ptr
            }
            None => true,
        };

        if need_new_bind_group {
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Text Bind Group"),
                layout: &self.bind_group_layouts.text,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.buffers.uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: if self.has_storage_buffers {
                            self.buffers.glyphs.as_entire_binding()
                        } else {
                            wgpu::BindingResource::TextureView(
                                self.buffers.glyph_data_view.as_ref().unwrap(),
                            )
                        },
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(atlas_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(atlas_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(color_atlas_view),
                    },
                ],
            });
            self.cached_text = Some(CachedTextResources {
                bind_group,
                atlas_view_ptr,
                color_atlas_view_ptr,
            });
        }

        let text_bind_group = &self.cached_text.as_ref().unwrap().bind_group;

        // Create command encoder
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Blinc Text Render Encoder"),
            });

        // Begin render pass (load existing content)
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Blinc Text Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load, // Keep existing content
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Use text_overlay pipeline since we're rendering to 1x sampled texture
            render_pass.set_pipeline(&self.pipelines.text_overlay);
            render_pass.set_bind_group(0, text_bind_group, &[]);
            render_pass.draw(0..6, 0..glyphs.len() as u32);
        }

        // Submit commands
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Create the image rendering pipeline (lazily initialized)
    fn ensure_image_pipeline(&mut self) {
        if self.image_pipeline.is_some() {
            return;
        }

        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Image Shader"),
                source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(IMAGE_SHADER)),
            });

        // Bind group layout: uniforms, texture, sampler
        let bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("Image Bind Group Layout"),
                    entries: &[
                        // Uniforms (viewport size)
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::VERTEX,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        // Image texture
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        // Sampler
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });

        let pipeline_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Image Pipeline Layout"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

        // Blending for premultiplied alpha
        let blend_state = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };

        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Image Pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<GpuImageInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &[
                            // dst_rect
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 0,
                                shader_location: 0,
                            },
                            // src_uv
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 16,
                                shader_location: 1,
                            },
                            // tint
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 32,
                                shader_location: 2,
                            },
                            // params (border_radius, opacity, border_width, packed_border_color)
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 48,
                                shader_location: 3,
                            },
                            // clip_bounds (x, y, width, height)
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 64,
                                shader_location: 4,
                            },
                            // clip_radius (tl, tr, br, bl)
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 80,
                                shader_location: 5,
                            },
                            // filter_a (grayscale, invert, sepia, hue_rotate_rad)
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 96,
                                shader_location: 6,
                            },
                            // filter_b (brightness, contrast, saturate, unused)
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 112,
                                shader_location: 7,
                            },
                            // transform (a, b, c, d) - 2x2 affine matrix
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 128,
                                shader_location: 8,
                            },
                            // clip2_bounds (x, y, width, height) - secondary sharp clip
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 144,
                                shader_location: 9,
                            },
                            // mask_params (gradient geometry)
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 160,
                                shader_location: 10,
                            },
                            // mask_info (type, start_alpha, end_alpha, 0)
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 176,
                                shader_location: 11,
                            },
                        ],
                    }],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.texture_format,
                        blend: Some(blend_state),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            });

        // Create instance buffer (max 1000 images per batch)
        let instance_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Image Instance Buffer"),
            size: (std::mem::size_of::<GpuImageInstance>() * 1000) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Create sampler
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Image Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        self.image_pipeline = Some(ImagePipeline {
            pipeline,
            bind_group_layout,
            instance_buffer,
            sampler,
        });
    }

    /// Lazily create the blur effect pipeline and its uniform buffers
    fn ensure_blur_pipeline(&mut self) {
        if self.effect_pipelines.blur.is_some() {
            return;
        }

        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Blur Effect Shader"),
                source: wgpu::ShaderSource::Wgsl(BLUR_SHADER.into()),
            });

        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Blur Effect Pipeline Layout"),
                bind_group_layouts: &[&self.bind_group_layouts.blur],
                push_constant_ranges: &[],
            });

        let targets = &[Some(wgpu::ColorTargetState {
            format: self.texture_format,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        })];

        self.effect_pipelines.blur = Some(self.device.create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some("Blur Effect Pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_kawase_blur"),
                    targets,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            },
        ));

        // Also create the 8 uniform buffers for multi-pass blur
        if self.buffers.blur_uniforms_pool.is_none() {
            self.buffers.blur_uniforms_pool = Some(
                (0..8)
                    .map(|i| {
                        self.device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some(&format!("Blur Uniforms Pass {i}")),
                            size: std::mem::size_of::<BlurUniforms>() as u64,
                            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        })
                    })
                    .collect(),
            );
        }
    }

    /// Lazily create the color matrix effect pipeline and its uniform buffer
    fn ensure_color_matrix_pipeline(&mut self) {
        if self.effect_pipelines.color_matrix.is_some() {
            return;
        }

        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Color Matrix Effect Shader"),
                source: wgpu::ShaderSource::Wgsl(COLOR_MATRIX_SHADER.into()),
            });

        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Color Matrix Effect Pipeline Layout"),
                bind_group_layouts: &[&self.bind_group_layouts.color_matrix],
                push_constant_ranges: &[],
            });

        let targets = &[Some(wgpu::ColorTargetState {
            format: self.texture_format,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        })];

        self.effect_pipelines.color_matrix = Some(self.device.create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some("Color Matrix Effect Pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_color_matrix"),
                    targets,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            },
        ));

        if self.buffers.color_matrix_uniforms.is_none() {
            self.buffers.color_matrix_uniforms =
                Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Color Matrix Uniforms Buffer"),
                    size: std::mem::size_of::<ColorMatrixUniforms>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
        }
    }

    /// Lazily create the drop shadow effect pipeline and its uniform buffer
    fn ensure_drop_shadow_pipeline(&mut self) {
        if self.effect_pipelines.drop_shadow.is_some() {
            return;
        }

        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Drop Shadow Effect Shader"),
                source: wgpu::ShaderSource::Wgsl(DROP_SHADOW_SHADER.into()),
            });

        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Drop Shadow Effect Pipeline Layout"),
                bind_group_layouts: &[&self.bind_group_layouts.drop_shadow],
                push_constant_ranges: &[],
            });

        let targets = &[Some(wgpu::ColorTargetState {
            format: self.texture_format,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        })];

        self.effect_pipelines.drop_shadow = Some(self.device.create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some("Drop Shadow Effect Pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_drop_shadow"),
                    targets,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            },
        ));

        if self.buffers.drop_shadow_uniforms.is_none() {
            self.buffers.drop_shadow_uniforms =
                Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Drop Shadow Uniforms Buffer"),
                    size: std::mem::size_of::<DropShadowUniforms>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
        }
    }

    /// Lazily create the glow effect pipeline and its uniform buffer
    fn ensure_glow_pipeline(&mut self) {
        if self.effect_pipelines.glow.is_some() {
            return;
        }

        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Glow Effect Shader"),
                source: wgpu::ShaderSource::Wgsl(GLOW_SHADER.into()),
            });

        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Glow Effect Pipeline Layout"),
                bind_group_layouts: &[&self.bind_group_layouts.glow],
                push_constant_ranges: &[],
            });

        let targets = &[Some(wgpu::ColorTargetState {
            format: self.texture_format,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        })];

        self.effect_pipelines.glow = Some(self.device.create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some("Glow Effect Pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_glow"),
                    targets,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            },
        ));

        if self.buffers.glow_uniforms.is_none() {
            self.buffers.glow_uniforms = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Glow Uniforms Buffer"),
                size: std::mem::size_of::<GlowUniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
    }

    /// Lazily create the mask image effect pipeline
    fn ensure_mask_image_pipeline(&mut self) {
        if self.effect_pipelines.mask_image.is_some() {
            return;
        }

        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Mask Image Shader"),
                source: wgpu::ShaderSource::Wgsl(MASK_IMAGE_SHADER.into()),
            });

        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Mask Image Effect Pipeline Layout"),
                bind_group_layouts: &[&self.bind_group_layouts.mask_image],
                push_constant_ranges: &[],
            });

        let targets = &[Some(wgpu::ColorTargetState {
            format: self.texture_format,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        })];

        self.effect_pipelines.mask_image = Some(self.device.create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some("Mask Image Effect Pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_mask"),
                    targets,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            },
        ));
    }

    /// Lazily create both glass pipelines (liquid glass + simple frosted glass)
    fn ensure_glass_pipelines(&mut self) {
        if self.effect_pipelines.glass.is_some() {
            return;
        }

        let glass_source = if self.has_storage_buffers {
            GLASS_SHADER
        } else {
            GLASS_DT_SHADER
        };
        let simple_glass_source = if self.has_storage_buffers {
            SIMPLE_GLASS_SHADER
        } else {
            SIMPLE_GLASS_DT_SHADER
        };

        let glass_shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Glass Shader"),
                source: wgpu::ShaderSource::Wgsl(glass_source.into()),
            });

        let simple_glass_shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Simple Glass Shader"),
                source: wgpu::ShaderSource::Wgsl(simple_glass_source.into()),
            });

        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Glass Pipeline Layout"),
                bind_group_layouts: &[&self.bind_group_layouts.glass],
                push_constant_ranges: &[],
            });

        let blend_state = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };

        let color_targets = &[Some(wgpu::ColorTargetState {
            format: self.texture_format,
            blend: Some(blend_state),
            write_mask: wgpu::ColorWrites::ALL,
        })];

        self.effect_pipelines.glass = Some(self.device.create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some("Glass Pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &glass_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &glass_shader,
                    entry_point: Some("fs_main"),
                    targets: color_targets,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            },
        ));

        self.effect_pipelines.simple_glass = Some(self.device.create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some("Simple Glass Pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &simple_glass_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &simple_glass_shader,
                    entry_point: Some("fs_main"),
                    targets: color_targets,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            },
        ));
    }

    /// Clear a texture view to a solid color
    pub fn clear_target(&mut self, target: &wgpu::TextureView, color: wgpu::Color) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Clear Target Encoder"),
            });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Clear Target Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Render images to a texture view
    ///
    /// # Arguments
    /// * `target` - The target texture view to render to
    /// * `image_view` - The image texture view to sample from
    /// * `instances` - The image instances to render
    pub fn render_images(
        &mut self,
        target: &wgpu::TextureView,
        image_view: &wgpu::TextureView,
        instances: &[GpuImageInstance],
    ) {
        if instances.is_empty() {
            return;
        }

        // Ensure pipeline is created
        self.ensure_image_pipeline();

        let image_pipeline = self.image_pipeline.as_ref().unwrap();

        // Update uniforms
        let uniforms = Uniforms {
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            _padding: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.buffers.uniforms, 0, bytemuck::bytes_of(&uniforms));

        // Update instance buffer
        self.queue.write_buffer(
            &image_pipeline.instance_buffer,
            0,
            bytemuck::cast_slice(instances),
        );

        // Create bind group for this image
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Image Bind Group"),
            layout: &image_pipeline.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.buffers.uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(image_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&image_pipeline.sampler),
                },
            ],
        });

        // Create command encoder
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Image Render Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Image Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load, // Preserve existing content
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(&image_pipeline.pipeline);
            render_pass.set_bind_group(0, &bind_group, &[]);
            render_pass.set_vertex_buffer(0, image_pipeline.instance_buffer.slice(..));
            render_pass.draw(0..6, 0..instances.len() as u32);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Layer Texture Cache Accessors
    // ─────────────────────────────────────────────────────────────────────────

    /// Render dynamic RGBA images (video frames, camera preview, etc.)
    ///
    /// Uploads each image as a temporary GPU texture and renders it
    /// to the destination rect using the image pipeline.
    pub fn render_dynamic_images(
        &mut self,
        target: &wgpu::TextureView,
        images: &[crate::primitives::DynamicImage],
    ) {
        for img in images {
            if img.data.len() != (img.width * img.height * 4) as usize {
                continue; // Invalid RGBA data
            }

            // Create temporary GPU texture from RGBA data
            let gpu_image = crate::image::GpuImage::from_rgba(
                &self.device,
                &self.queue,
                &img.data,
                img.width,
                img.height,
                Some("dynamic_image"),
            );

            // Create an instance for the image pipeline
            let instance = GpuImageInstance::new(
                img.dest.x(),
                img.dest.y(),
                img.dest.width(),
                img.dest.height(),
            )
            .with_opacity(img.opacity)
            .with_border_radius(img.corner_radius);

            // Render using the existing image pipeline
            self.render_images(target, gpu_image.view(), &[instance]);
        }
    }

    /// Ensure the mesh rendering pipeline is created
    fn ensure_mesh_pipeline(&mut self) {
        if self.mesh_pipeline.is_some() {
            return;
        }

        // ── Main mesh shader ─────────────────────────────────────────────
        let shader_src = if self.has_storage_buffers {
            include_str!("shaders/mesh.wgsl")
        } else {
            MESH_DT_SHADER
        };
        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Mesh Shader"),
                source: wgpu::ShaderSource::Wgsl(shader_src.into()),
            });

        let bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("Mesh Bind Group Layout"),
                    entries: &[
                        // 0: Uniforms (view_proj, model, light_view_proj, camera, light, flags)
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        // 1: Material uniforms
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        // 2: Base color texture
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                multisampled: false,
                                view_dimension: wgpu::TextureViewDimension::D2,
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            },
                            count: None,
                        },
                        // 3: Texture sampler (shared for base color, normal, displacement)
                        wgpu::BindGroupLayoutEntry {
                            binding: 3,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                        // 4: Normal map texture
                        wgpu::BindGroupLayoutEntry {
                            binding: 4,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                multisampled: false,
                                view_dimension: wgpu::TextureViewDimension::D2,
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            },
                            count: None,
                        },
                        // 5: Shadow map (depth texture)
                        wgpu::BindGroupLayoutEntry {
                            binding: 5,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                multisampled: false,
                                view_dimension: wgpu::TextureViewDimension::D2,
                                sample_type: wgpu::TextureSampleType::Depth,
                            },
                            count: None,
                        },
                        // 6: Shadow comparison sampler
                        wgpu::BindGroupLayoutEntry {
                            binding: 6,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                            count: None,
                        },
                        // 7: Displacement / height map texture
                        wgpu::BindGroupLayoutEntry {
                            binding: 7,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                multisampled: false,
                                view_dimension: wgpu::TextureViewDimension::D2,
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            },
                            count: None,
                        },
                        // 8: Joint matrices — storage buffer (normal) or texture (WebGL2 DT)
                        wgpu::BindGroupLayoutEntry {
                            binding: 8,
                            visibility: wgpu::ShaderStages::VERTEX,
                            ty: if self.has_storage_buffers {
                                wgpu::BindingType::Buffer {
                                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                                    has_dynamic_offset: false,
                                    min_binding_size: None,
                                }
                            } else {
                                wgpu::BindingType::Texture {
                                    sample_type: wgpu::TextureSampleType::Float {
                                        filterable: false,
                                    },
                                    view_dimension: wgpu::TextureViewDimension::D2,
                                    multisampled: false,
                                }
                            },
                            count: None,
                        },
                        // 9: Metallic / roughness texture (glTF convention:
                        // metallic in .b, roughness in .g, multiplied per-texel
                        // by the scalar factors in MaterialUniforms).
                        wgpu::BindGroupLayoutEntry {
                            binding: 9,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                multisampled: false,
                                view_dimension: wgpu::TextureViewDimension::D2,
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            },
                            count: None,
                        },
                        // 10: Emissive texture — multiplied per-texel by the
                        // scalar `emissive` RGB in MaterialUniforms. Used for
                        // self-lit surfaces (HUD glyphs, LED panels, etc.).
                        wgpu::BindGroupLayoutEntry {
                            binding: 10,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                multisampled: false,
                                view_dimension: wgpu::TextureViewDimension::D2,
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            },
                            count: None,
                        },
                        // 11: Ambient occlusion texture — .r channel attenuates
                        // the ambient/indirect diffuse term. `occlusion_strength`
                        // in MaterialUniforms controls how much AO applies.
                        wgpu::BindGroupLayoutEntry {
                            binding: 11,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                multisampled: false,
                                view_dimension: wgpu::TextureViewDimension::D2,
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            },
                            count: None,
                        },
                        // 12: Environment cubemap for IBL reflections
                        wgpu::BindGroupLayoutEntry {
                            binding: 12,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                multisampled: false,
                                view_dimension: wgpu::TextureViewDimension::Cube,
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            },
                            count: None,
                        },
                        // 13: Environment cubemap sampler
                        wgpu::BindGroupLayoutEntry {
                            binding: 13,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });

        let pipeline_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Mesh Pipeline Layout"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

        let vertex_stride = std::mem::size_of::<blinc_core::draw::Vertex>() as u64;
        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: vertex_stride,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                // position: vec3<f32>
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x3,
                    offset: 0,
                    shader_location: 0,
                },
                // normal: vec3<f32>
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x3,
                    offset: 12,
                    shader_location: 1,
                },
                // uv: vec2<f32>
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 24,
                    shader_location: 2,
                },
                // color: vec4<f32>
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 32,
                    shader_location: 3,
                },
                // tangent: vec4<f32>
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 48,
                    shader_location: 4,
                },
                // joints: vec4<u32>
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Uint32x4,
                    offset: 64,
                    shader_location: 5,
                },
                // weights: vec4<f32>
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 80,
                    shader_location: 6,
                },
            ],
        };

        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Mesh Pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[vertex_layout],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    // Target the HDR intermediate (Rgba16Float), not the
                    // sRGB framebuffer — this preserves values above 1.0
                    // for specular and emissive. The tonemap pass later
                    // maps the HDR range down to the framebuffer's format.
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba16Float,
                        // Premultiplied-alpha blending: the shader
                        // premultiplies `reflected * alpha` but leaves
                        // `emissive` un-attenuated so self-emitted
                        // light is preserved through transparent
                        // surfaces. `BlendState::ALPHA_BLENDING` would
                        // scale the whole final color by alpha,
                        // killing emissive glows on any BLEND material
                        // whose base-color mask has low alpha (e.g.
                        // buster_drone's body decal region around the
                        // nose).
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    // Culling disabled — glTF allows per-material
                    // `doubleSided`, but the mesh pipeline currently
                    // can't switch cull state per-draw. Some assets
                    // author inward-facing winding on interior
                    // components (shells that are meant to be
                    // doubleSided) and cull_mode: Back makes those
                    // geometries silently invisible. Rendering
                    // backfaces too is marginally more expensive but
                    // correct for every asset; making it material-
                    // driven would need a second pipeline variant.
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: true,
                    depth_compare: wgpu::CompareFunction::Less,
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            });

        // 3x mat4 (view_proj + model + light_view_proj) + camera + light + flags = 320 bytes
        let uniform_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Mesh Uniforms"),
            size: 320,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Material uniform layout:
        //   base_color: vec4           16
        //   metallic_roughness: vec2    8
        //   emissive: vec3 + pad       16  (WGSL vec3 is 16-byte aligned)
        //   unlit: f32                  4
        //   has_mr_texture: f32         4
        //   has_emissive_texture: f32   4
        //   has_occlusion_texture: f32  4
        //   occlusion_strength: f32     4
        //   _pad: f32 * 3              12  (round up to 16-byte struct end)
        //                              ─
        //                              80 bytes total, safely rounded to 96.
        //
        // We size to 96 to leave headroom for one more vec4 without
        // having to resize the buffer when the next PBR input lands.
        let material_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Mesh Material"),
            size: 96,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Default 1x1 white texture for untextured meshes
        let default_texture = crate::image::GpuImage::from_rgba(
            &self.device,
            &self.queue,
            &[255, 255, 255, 255],
            1,
            1,
            Some("mesh_default_tex"),
        );

        // Default flat normal map (tangent-space up = 128,128,255)
        let default_normal_map = crate::image::GpuImage::from_rgba(
            &self.device,
            &self.queue,
            &[128, 128, 255, 255],
            1,
            1,
            Some("mesh_default_normal"),
        );

        // Default black displacement (no displacement)
        let default_displacement = crate::image::GpuImage::from_rgba(
            &self.device,
            &self.queue,
            &[0, 0, 0, 255],
            1,
            1,
            Some("mesh_default_displacement"),
        );

        // Default white metallic/roughness. Bound whenever the material
        // has no MR texture — the shader gates the sample on the
        // `has_metallic_roughness_texture` flag and uses only the scalar
        // factors in that branch, so the texture itself is only read for
        // bind-group layout validation, never sampled.
        let default_metallic_roughness = crate::image::GpuImage::from_rgba(
            &self.device,
            &self.queue,
            &[255, 255, 255, 255],
            1,
            1,
            Some("mesh_default_metallic_roughness"),
        );

        // Default white emissive texture — same gating as above.
        let default_emissive = crate::image::GpuImage::from_rgba(
            &self.device,
            &self.queue,
            &[255, 255, 255, 255],
            1,
            1,
            Some("mesh_default_emissive"),
        );

        // Default white occlusion texture — 1.0 means "no occlusion",
        // so even if a caller accidentally leaves the flag on the AO
        // term reduces to the multiplicative identity.
        let default_occlusion = crate::image::GpuImage::from_rgba(
            &self.device,
            &self.queue,
            &[255, 255, 255, 255],
            1,
            1,
            Some("mesh_default_occlusion"),
        );

        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Mesh Sampler"),
            // Trilinear filtering for PBR textures at distance. glTF
            // exporters frequently author one set of textures intended
            // to be used at multiple distances; without mipmap_filter
            // set, min_filter alone gives aliased highlights on
            // metallic-roughness maps viewed at steep angles.
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            // REPEAT wrap matches the glTF spec default and what
            // buster_drone's sampler explicitly requests (wrapS/T =
            // GL_REPEAT = 10497). ClampToEdge (the wgpu default) breaks
            // tiled terrain textures — their UVs often run past [0,1]
            // to tile — and also subtly breaks body meshes whose UV
            // shells straddle the 0/1 seam. The mesh loader no longer
            // clamps UVs to [0,1]; the sampler handles repeat natively.
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            ..Default::default()
        });

        // Joint matrices — identity matrix as default (no skinning)
        // Max 256 joints * 64 bytes per mat4x4 = 16384 bytes
        let identity: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let joint_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Mesh Joint Matrices"),
                contents: bytemuck::cast_slice(&identity),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            });

        // DT mode: create a RGBA32Float texture for joint matrices.
        // Width=4 (one texel per mat4 row), height=256 (max joints).
        // Initialised with the identity matrix at row 0.
        let (joint_data_texture, joint_data_view) = if !self.has_storage_buffers {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Mesh Joint Data Texture"),
                size: wgpu::Extent3d {
                    width: 4,
                    height: 256,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            // Upload identity matrix at row 0: 4 texels (one per mat4 row)
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&identity),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * 16), // 4 texels * 16 bytes per RGBA32F texel
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: 4,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            (Some(tex), Some(view))
        } else {
            (None, None)
        };

        // ── Shadow pipeline ──────────────────────────────────────────────
        let shadow_shader_src = include_str!("shaders/shadow.wgsl");
        let shadow_shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Shadow Shader"),
                source: wgpu::ShaderSource::Wgsl(shadow_shader_src.into()),
            });

        let shadow_bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("Shadow Bind Group Layout"),
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    }],
                });

        let shadow_pipeline_layout =
            self.device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("Shadow Pipeline Layout"),
                    bind_group_layouts: &[&shadow_bind_group_layout],
                    push_constant_ranges: &[],
                });

        // Shadow pass only needs position (first attribute), but uses same vertex buffer
        let shadow_vertex_layout = wgpu::VertexBufferLayout {
            array_stride: vertex_stride,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            }],
        };

        let shadow_pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Shadow Pipeline"),
                layout: Some(&shadow_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shadow_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[shadow_vertex_layout],
                    compilation_options: Default::default(),
                },
                fragment: None, // depth-only pass
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: Some(wgpu::Face::Front), // front-face culling reduces shadow acne
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: true,
                    depth_compare: wgpu::CompareFunction::LessEqual,
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState {
                        constant: 2,
                        slope_scale: 2.0,
                        clamp: 0.0,
                    },
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            });

        // light_view_proj + model = 128 bytes
        let shadow_uniform_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Shadow Uniforms"),
            size: 128,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Shadow map depth texture
        let shadow_map = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Shadow Map"),
            size: wgpu::Extent3d {
                width: SHADOW_MAP_SIZE,
                height: SHADOW_MAP_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let shadow_view = shadow_map.create_view(&wgpu::TextureViewDescriptor::default());

        // Comparison sampler for PCF shadow sampling
        let shadow_sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Shadow Comparison Sampler"),
            compare: Some(wgpu::CompareFunction::LessEqual),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // ── IBL environment cubemap ─────────────────────────────────────
        //
        // The cubemap starts as a neutral gray fallback (0.15, 0.15, 0.15
        // in f16). SceneKit3D provides the real studio environment via
        // `set_environment_cubemap` → `upload_environment_cubemap`, which
        // overwrites the fallback on the first frame that has 3D content.
        let env_size: u32 = 128;
        let env_mip_count = (env_size as f32).log2() as u32 + 1;
        let env_cubemap = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("IBL Environment Cubemap"),
            size: wgpu::Extent3d {
                width: env_size,
                height: env_size,
                depth_or_array_layers: 6,
            },
            mip_level_count: env_mip_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        // Fill every face × mip with neutral gray (f16 RGBA).
        // f32_to_f16(0.15) ≈ 0x3106, f32_to_f16(1.0) = 0x3C00.
        let gray_r = 0x3106u16; // ~0.15
        let gray_a = 0x3C00u16; // 1.0
        for face in 0..6u32 {
            for mip in 0..env_mip_count {
                let mip_size = (env_size >> mip).max(1);
                let texels = (mip_size * mip_size) as usize;
                let mut data = Vec::with_capacity(texels * 8);
                for _ in 0..texels {
                    data.extend_from_slice(&gray_r.to_le_bytes());
                    data.extend_from_slice(&gray_r.to_le_bytes());
                    data.extend_from_slice(&gray_r.to_le_bytes());
                    data.extend_from_slice(&gray_a.to_le_bytes());
                }
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &env_cubemap,
                        mip_level: mip,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: face,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(mip_size * 8),
                        rows_per_image: Some(mip_size),
                    },
                    wgpu::Extent3d {
                        width: mip_size,
                        height: mip_size,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }

        let env_cubemap_view = env_cubemap.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        });

        let env_sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("IBL Environment Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // ── Skybox pipeline ──────────────────────────────────────────────
        //
        // The skybox is a fixed screen-space gradient (not a sky dome),
        // so it has no bindings — no camera, no cubemap. Empty layout.
        let skybox_bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("Skybox Bind Group Layout"),
                    entries: &[],
                });

        let skybox_pipeline = crate::custom_pass::create_fullscreen_pipeline(
            &self.device,
            "Skybox Pipeline",
            include_str!("shaders/skybox.wgsl"),
            wgpu::TextureFormat::Rgba16Float,
            &skybox_bind_group_layout,
        );

        let skybox_uniform_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Skybox Uniforms"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ── Tonemap pipeline ────────────────────────────────────────────
        let tonemap_bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("Tonemap Bind Group Layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                multisampled: false,
                                view_dimension: wgpu::TextureViewDimension::D2,
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                multisampled: false,
                                view_dimension: wgpu::TextureViewDimension::D2,
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            },
                            count: None,
                        },
                    ],
                });

        let tonemap_pipeline = crate::custom_pass::create_fullscreen_pipeline(
            &self.device,
            "Tonemap Pipeline",
            include_str!("shaders/tonemap.wgsl"),
            self.texture_format,
            &tonemap_bind_group_layout,
        );

        let tonemap_sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Tonemap Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // ── Bloom pipeline ──────────────────────────────────────────────
        let bloom_bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("Bloom Bind Group Layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                multisampled: false,
                                view_dimension: wgpu::TextureViewDimension::D2,
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });

        let bloom_pipeline = crate::custom_pass::create_fullscreen_pipeline(
            &self.device,
            "Bloom Pipeline",
            include_str!("shaders/bloom.wgsl"),
            wgpu::TextureFormat::Rgba16Float,
            &bloom_bind_group_layout,
        );

        let bloom_uniform_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Bloom Uniforms"),
            size: 16, // vec2 texel_size + f32 threshold + f32 mode
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        self.mesh_pipeline = Some(MeshPipeline {
            pipeline,
            bind_group_layout,
            uniform_buffer,
            material_buffer,
            default_texture,
            default_normal_map,
            default_displacement,
            default_metallic_roughness,
            default_emissive,
            default_occlusion,
            sampler,
            joint_buffer,
            joint_data_texture,
            joint_data_view,
            main_depth: None,
            main_depth_view: None,
            main_depth_size: (0, 0),
            shadow_pipeline,
            shadow_bind_group_layout,
            shadow_uniform_buffer,
            shadow_map,
            shadow_view,
            shadow_sampler,
            env_cubemap,
            env_cubemap_view,
            env_sampler,
            cached_mesh_buffers: std::collections::HashMap::new(),
            cached_mesh_buffer_keys: std::collections::VecDeque::new(),
            cached_gpu_images: std::collections::HashMap::new(),
            cached_gpu_image_keys: std::collections::VecDeque::new(),
            skybox_pipeline,
            skybox_bind_group_layout,
            skybox_uniform_buffer,
            hdr_texture: None,
            hdr_view: None,
            hdr_size: (0, 0),
            tonemap_pipeline,
            tonemap_bind_group_layout,
            tonemap_sampler,
            bloom_pipeline,
            bloom_bind_group_layout,
            bloom_uniform_buffer,
            bloom_a: None,
            bloom_a_view: None,
            bloom_b: None,
            bloom_b_view: None,
            bloom_size: (0, 0),
        });
    }

    /// Upload externally-generated cubemap face/mip data into the mesh
    /// pipeline's environment cubemap texture. Requires `ensure_mesh_pipeline`
    /// to have been called first (the texture must already exist).
    pub fn upload_environment_cubemap(&mut self, data: &blinc_core::layer::CubemapData) {
        let mp = match self.mesh_pipeline.as_mut() {
            Some(mp) => mp,
            None => return,
        };

        let expected = (6 * data.mip_count) as usize;
        if data.faces.len() != expected {
            tracing::warn!(
                "upload_environment_cubemap: expected {} face entries, got {}",
                expected,
                data.faces.len()
            );
            return;
        }

        // Recreate the cubemap texture if the incoming size differs
        // from the current one (e.g. procedural 128 → HDRI 256).
        let current_size = mp.env_cubemap.size();
        if current_size.width != data.size || current_size.height != data.size {
            let env_mip_count = (data.size as f32).log2() as u32 + 1;
            let new_tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("IBL Environment Cubemap"),
                size: wgpu::Extent3d {
                    width: data.size,
                    height: data.size,
                    depth_or_array_layers: 6,
                },
                mip_level_count: env_mip_count,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            mp.env_cubemap_view = new_tex.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::Cube),
                ..Default::default()
            });
            mp.env_cubemap = new_tex;
        }

        for face in 0..6u32 {
            for mip in 0..data.mip_count {
                let mip_size = (data.size >> mip).max(1);
                let idx = (face * data.mip_count + mip) as usize;
                let face_data = &data.faces[idx];
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &mp.env_cubemap,
                        mip_level: mip,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: face,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    face_data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(mip_size * 8),
                        rows_per_image: Some(mip_size),
                    },
                    wgpu::Extent3d {
                        width: mip_size,
                        height: mip_size,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
    }

    /// Render a mesh's shadow depth into the global shadow map.
    ///
    /// Must be called for every shadow-casting mesh in the frame BEFORE
    /// any main-pass draw runs, so that when [`Self::render_mesh_data_batched`]
    /// samples the shadow map it sees the fully populated scene depth.
    ///
    /// `shadow_batch_index` controls the shadow map's depth load op:
    /// - `0` → `Clear(1.0)` (reset the whole map for the new frame)
    /// - `>0` → `Load` (accumulate this mesh's depth alongside earlier
    ///   shadow casters)
    ///
    /// Caller responsibility: only invoke this for meshes whose material
    /// has `casts_shadows == true`, and count them consecutively starting
    /// from zero. The receiver-side `receives_shadows` flag is handled
    /// entirely in `render_mesh_data_batched` via the `light_view_proj`
    /// parameter.
    #[allow(clippy::too_many_arguments)]
    pub fn render_mesh_shadow_pass(
        &mut self,
        mesh: &std::sync::Arc<blinc_core::draw::MeshData>,
        transform: &[f32; 16],
        light_view_proj: &[f32; 16],
        shadow_batch_index: usize,
    ) {
        if mesh.vertices.is_empty() || mesh.indices.is_empty() {
            return;
        }
        self.ensure_mesh_pipeline();

        // Reuse the same Arc<MeshData>-keyed vertex/index cache the
        // main pass uses. If the main pass hasn't uploaded this mesh's
        // buffers yet, populate the cache here; subsequent main-pass
        // calls will hit the cache for free.
        let mesh_ptr = std::sync::Arc::as_ptr(mesh) as usize;
        let needs_buffers = !self
            .mesh_pipeline
            .as_ref()
            .unwrap()
            .cached_mesh_buffers
            .contains_key(&mesh_ptr);
        if needs_buffers {
            let vertex_data: &[u8] = unsafe {
                std::slice::from_raw_parts(
                    mesh.vertices.as_ptr() as *const u8,
                    mesh.vertices.len() * std::mem::size_of::<blinc_core::draw::Vertex>(),
                )
            };
            let vertex_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Mesh Vertices (cached)"),
                    contents: vertex_data,
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let index_data: &[u8] = bytemuck::cast_slice(&mesh.indices);
            let index_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Mesh Indices (cached)"),
                    contents: index_data,
                    usage: wgpu::BufferUsages::INDEX,
                });
            let entry = MeshBufferCacheEntry {
                vertex: vertex_buffer,
                index: index_buffer,
                index_count: mesh.indices.len() as u32,
            };
            let mp_mut = self.mesh_pipeline.as_mut().unwrap();
            mp_mut.cached_mesh_buffers.insert(mesh_ptr, entry);
            mp_mut.cached_mesh_buffer_keys.push_back(mesh_ptr);
            while mp_mut.cached_mesh_buffer_keys.len() > MESH_CACHE_CAPACITY {
                if let Some(old_key) = mp_mut.cached_mesh_buffer_keys.pop_front() {
                    mp_mut.cached_mesh_buffers.remove(&old_key);
                }
            }
        }
        let (vertex_buffer, index_buffer, index_count) = {
            let mp = self.mesh_pipeline.as_ref().unwrap();
            let entry = mp
                .cached_mesh_buffers
                .get(&mesh_ptr)
                .expect("mesh buffer cache entry must exist after insert");
            (entry.vertex.clone(), entry.index.clone(), entry.index_count)
        };

        #[repr(C)]
        #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
        struct ShadowUniforms {
            light_view_proj: [f32; 16],
            model: [f32; 16],
        }

        let shadow_uniforms = ShadowUniforms {
            light_view_proj: *light_view_proj,
            model: *transform,
        };

        let mp = self.mesh_pipeline.as_ref().unwrap();
        self.queue.write_buffer(
            &mp.shadow_uniform_buffer,
            0,
            bytemuck::bytes_of(&shadow_uniforms),
        );

        let shadow_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Shadow Bind Group"),
            layout: &mp.shadow_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: mp.shadow_uniform_buffer.as_entire_binding(),
            }],
        });

        // First shadow caster in the frame clears the map; subsequent
        // casters load so their depth accumulates with earlier draws.
        let load_op = if shadow_batch_index == 0 {
            wgpu::LoadOp::Clear(1.0)
        } else {
            wgpu::LoadOp::Load
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Shadow Pass"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Shadow Depth Pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &mp.shadow_view,
                    depth_ops: Some(wgpu::Operations {
                        load: load_op,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });

            pass.set_pipeline(&mp.shadow_pipeline);
            pass.set_bind_group(0, &shadow_bind_group, &[]);
            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..index_count, 0, 0..1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Render mesh data with shadow mapping, normal mapping, and parallax displacement.
    ///
    /// When `light_view_proj` is provided AND the mesh material has
    /// `receives_shadows == true`, the main fragment shader samples the
    /// shadow map produced by prior [`Self::render_mesh_shadow_pass`]
    /// calls in the same frame. Passing `Some(...)` here without having
    /// populated the shadow map leaves the depth compare sampling a
    /// cleared/stale texture.
    #[allow(clippy::too_many_arguments)]
    pub fn render_mesh_data(
        &mut self,
        target: &wgpu::TextureView,
        mesh: &std::sync::Arc<blinc_core::draw::MeshData>,
        transform: &[f32; 16],
        view_proj: &[f32; 16],
        camera_pos: [f32; 3],
        light_dir: [f32; 3],
        light_intensity: f32,
        light_view_proj: Option<&[f32; 16]>,
        viewport: Option<[f32; 4]>,
    ) {
        // Defer to the batch-aware variant; single-mesh calls behave
        // exactly as before (clear + skybox on first, tonemap on last).
        self.render_mesh_data_batched(
            target,
            mesh,
            transform,
            view_proj,
            camera_pos,
            light_dir,
            light_intensity,
            light_view_proj,
            viewport,
            0,
            1,
        );
    }

    /// Batch-aware version of [`Self::render_mesh_data`].
    ///
    /// `batch_index` is the 0-based position of this mesh within the
    /// current frame's mesh batch; `batch_count` is the batch size.
    ///
    /// - When `batch_index == 0`, the HDR intermediate is cleared and
    ///   the skybox pass runs. Subsequent calls preserve the HDR so
    ///   every mesh in the batch accumulates into it.
    /// - When `batch_index + 1 == batch_count` (last mesh), the
    ///   bloom + tonemap passes run, writing the composited HDR to
    ///   the frame target.
    ///
    /// Without this batching, the skybox pass clears HDR on every
    /// single-mesh call — causing multi-mesh scenes (e.g. a 39-mesh
    /// glTF asset) to show only the *last* mesh drawn because each
    /// tonemap pass reads HDR with only that mesh's contribution.
    #[allow(clippy::too_many_arguments)]
    pub fn render_mesh_data_batched(
        &mut self,
        target: &wgpu::TextureView,
        mesh: &std::sync::Arc<blinc_core::draw::MeshData>,
        transform: &[f32; 16],
        view_proj: &[f32; 16],
        camera_pos: [f32; 3],
        light_dir: [f32; 3],
        light_intensity: f32,
        light_view_proj: Option<&[f32; 16]>,
        viewport: Option<[f32; 4]>,
        batch_index: usize,
        batch_count: usize,
    ) {
        if mesh.vertices.is_empty() || mesh.indices.is_empty() {
            return;
        }
        let is_first = batch_index == 0;
        let is_last = batch_index + 1 >= batch_count.max(1);

        self.ensure_mesh_pipeline();

        // ── Vertex + index buffer cache ──────────────────────────────────
        //
        // Keyed by the `Arc<MeshData>` raw pointer. Without this, every
        // `draw_mesh_data` call uploads fresh vertex / index buffers —
        // a 39-mesh asset like buster_drone would push ~1.8 GB through
        // the wgpu queue per frame, pinning the render thread at ~1fps.
        //
        // FIFO-evicted at `MESH_CACHE_CAPACITY`. Scenes that recycle
        // the same Arcs each frame (the normal case) hit the cache on
        // every call after the first; streaming-mesh apps evict their
        // oldest entry and pay a re-upload for it on next touch.
        let mesh_ptr = std::sync::Arc::as_ptr(mesh) as usize;
        let needs_buffers = !self
            .mesh_pipeline
            .as_ref()
            .unwrap()
            .cached_mesh_buffers
            .contains_key(&mesh_ptr);
        if needs_buffers {
            // Safety: `Vertex` is `#[repr(C)]` with only f32/u32 fields
            // so a raw reinterpret to `&[u8]` is well-defined.
            let vertex_data: &[u8] = unsafe {
                std::slice::from_raw_parts(
                    mesh.vertices.as_ptr() as *const u8,
                    mesh.vertices.len() * std::mem::size_of::<blinc_core::draw::Vertex>(),
                )
            };
            let vertex_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Mesh Vertices (cached)"),
                    contents: vertex_data,
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let index_data: &[u8] = bytemuck::cast_slice(&mesh.indices);
            let index_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Mesh Indices (cached)"),
                    contents: index_data,
                    usage: wgpu::BufferUsages::INDEX,
                });
            let entry = MeshBufferCacheEntry {
                vertex: vertex_buffer,
                index: index_buffer,
                index_count: mesh.indices.len() as u32,
            };
            let mp_mut = self.mesh_pipeline.as_mut().unwrap();
            mp_mut.cached_mesh_buffers.insert(mesh_ptr, entry);
            mp_mut.cached_mesh_buffer_keys.push_back(mesh_ptr);
            // Cap the cache — drop the oldest entry (FIFO) when full.
            // An LRU policy would need a touch-on-hit step; FIFO is
            // good enough for the common case where every mesh in a
            // scene is drawn every frame (all entries are equally
            // hot).
            while mp_mut.cached_mesh_buffer_keys.len() > MESH_CACHE_CAPACITY {
                if let Some(old_key) = mp_mut.cached_mesh_buffer_keys.pop_front() {
                    mp_mut.cached_mesh_buffers.remove(&old_key);
                }
            }
        }

        // Pull cloned buffer handles out of the cache so the rest of
        // the function owns its references independently of any
        // further mutable borrows of `self.mesh_pipeline`. `wgpu::Buffer`
        // is reference-counted internally, so `.clone()` is cheap.
        let (vertex_buffer, index_buffer, index_count) = {
            let mp = self.mesh_pipeline.as_ref().unwrap();
            let entry = mp
                .cached_mesh_buffers
                .get(&mesh_ptr)
                .expect("mesh buffer cache entry must exist after insert");
            (entry.vertex.clone(), entry.index.clone(), entry.index_count)
        };

        // Shadow map population happens BEFORE this call via
        // `render_mesh_shadow_pass`. Here we only sample it; the map is
        // populated-or-not based on whether the caller passed a light
        // matrix, and each mesh opts into receiving shadows via its
        // own `receives_shadows` flag (caster status is irrelevant for
        // receiving).
        let shadow_map_populated = light_view_proj.is_some();

        // ── Upload main uniforms ─────────────────────────────────────────
        let identity_mat: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];

        #[repr(C)]
        #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
        struct MeshUniforms {
            view_proj: [f32; 16],
            model: [f32; 16],
            light_view_proj: [f32; 16],
            camera_pos: [f32; 3],
            _pad: f32,
            light_dir: [f32; 3],
            light_intensity: f32,
            viewport_size: [f32; 2],
            has_texture: f32,
            has_normal_map: f32,
            shadow_enabled: f32,
            displacement_scale: f32,
            normal_scale: f32,
            has_skinning: f32,
        }

        let has_texture = if mesh.material.base_color_texture.is_some() {
            1.0_f32
        } else {
            0.0
        };
        let has_normal_map = if mesh.material.normal_map.is_some() {
            1.0_f32
        } else {
            0.0
        };
        let displacement_scale = if mesh.material.displacement_map.is_some() {
            mesh.material.displacement_scale
        } else {
            0.0
        };

        let uniforms = MeshUniforms {
            view_proj: *view_proj,
            model: *transform,
            light_view_proj: light_view_proj.copied().unwrap_or(identity_mat),
            camera_pos,
            _pad: 0.0,
            light_dir,
            light_intensity,
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            has_texture,
            has_normal_map,
            shadow_enabled: if shadow_map_populated && mesh.material.receives_shadows {
                1.0
            } else {
                0.0
            },
            displacement_scale,
            normal_scale: mesh.material.normal_scale,
            has_skinning: if mesh.skin.is_some() { 1.0 } else { 0.0 },
        };

        let mp = self.mesh_pipeline.as_ref().unwrap();
        self.queue
            .write_buffer(&mp.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        // ── Upload material ──────────────────────────────────────────────
        //
        // Layout must match `MaterialUniforms` in mesh.wgsl. WGSL's
        // std140-like alignment puts the vec3 on a 16-byte boundary, so
        // `emissive` is followed by 4 bytes of implicit padding before
        // `unlit` — we represent that explicitly with `emissive_pad` to
        // keep the bytemuck Pod layout matching the shader's struct.
        #[repr(C)]
        #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
        struct MaterialGpu {
            base_color: [f32; 4],
            metallic_roughness: [f32; 2],
            _pad_mr: [f32; 2],
            emissive: [f32; 3],
            unlit: f32,
            has_metallic_roughness_texture: f32,
            has_emissive_texture: f32,
            has_occlusion_texture: f32,
            occlusion_strength: f32,
            // Alpha mode is encoded as a float so WGSL's uniform layout
            // rules don't need a bool type (which isn't POD anyway).
            // Matches shader `MaterialUniforms.alpha_mode`.
            alpha_mode: f32,
            _pad_am: [f32; 3],
        }

        let alpha_mode = match mesh.material.alpha_mode {
            blinc_core::draw::AlphaMode::Opaque => 0.0,
            blinc_core::draw::AlphaMode::Mask => 1.0,
            blinc_core::draw::AlphaMode::Blend => 2.0,
        };
        let mat = MaterialGpu {
            base_color: mesh.material.base_color,
            metallic_roughness: [mesh.material.metallic, mesh.material.roughness],
            _pad_mr: [0.0; 2],
            emissive: mesh.material.emissive,
            unlit: if mesh.material.unlit { 1.0 } else { 0.0 },
            has_metallic_roughness_texture: if mesh.material.metallic_roughness_texture.is_some() {
                1.0
            } else {
                0.0
            },
            has_emissive_texture: if mesh.material.emissive_texture.is_some() {
                1.0
            } else {
                0.0
            },
            has_occlusion_texture: if mesh.material.occlusion_texture.is_some() {
                1.0
            } else {
                0.0
            },
            occlusion_strength: mesh.material.occlusion_strength,
            alpha_mode,
            _pad_am: [0.0; 3],
        };
        self.queue
            .write_buffer(&mp.material_buffer, 0, bytemuck::bytes_of(&mat));

        // ── Upload textures — cache by source `Arc<[u8]>` pointer ─────────
        //
        // Multiple materials typically reference the same underlying
        // image (glTF explicitly reuses images across primitives —
        // buster_drone has 39 meshes but only 10 distinct textures).
        // Keying by `TextureData.rgba.as_ptr()` means each unique image
        // becomes exactly one `GpuImage`, regardless of how many
        // materials point at it. FIFO-evicted at `MESH_CACHE_CAPACITY`.
        let texture_slots: [(&Option<blinc_core::TextureData>, &str); 6] = [
            (&mesh.material.base_color_texture, "mesh_base_color_tex"),
            (&mesh.material.normal_map, "mesh_normal_map"),
            (&mesh.material.displacement_map, "mesh_displacement_map"),
            (
                &mesh.material.metallic_roughness_texture,
                "mesh_metallic_roughness_tex",
            ),
            (&mesh.material.emissive_texture, "mesh_emissive_tex"),
            (&mesh.material.occlusion_texture, "mesh_occlusion_tex"),
        ];
        for (td_opt, label) in &texture_slots {
            let Some(td) = td_opt.as_ref() else { continue };
            let key = td.rgba.as_ptr() as usize;
            let already_cached = self
                .mesh_pipeline
                .as_ref()
                .unwrap()
                .cached_gpu_images
                .contains_key(&key);
            if already_cached {
                continue;
            }
            let img = crate::image::GpuImage::from_rgba(
                &self.device,
                &self.queue,
                &td.rgba,
                td.width,
                td.height,
                Some(*label),
            );
            let mp_mut = self.mesh_pipeline.as_mut().unwrap();
            mp_mut.cached_gpu_images.insert(key, img);
            mp_mut.cached_gpu_image_keys.push_back(key);
            while mp_mut.cached_gpu_image_keys.len() > MESH_CACHE_CAPACITY {
                if let Some(old_key) = mp_mut.cached_gpu_image_keys.pop_front() {
                    mp_mut.cached_gpu_images.remove(&old_key);
                }
            }
        }

        let mp = self.mesh_pipeline.as_ref().unwrap();
        // Look up the `GpuImage` for each material slot, falling back to
        // the default placeholder texture when the slot is empty.
        let lookup = |td_opt: &Option<blinc_core::TextureData>| -> Option<&crate::image::GpuImage> {
            td_opt
                .as_ref()
                .and_then(|td| mp.cached_gpu_images.get(&(td.rgba.as_ptr() as usize)))
        };
        let base_view = lookup(&mesh.material.base_color_texture)
            .map_or_else(|| mp.default_texture.view(), |t| t.view());
        let normal_view = lookup(&mesh.material.normal_map)
            .map_or_else(|| mp.default_normal_map.view(), |t| t.view());
        let displacement_view = lookup(&mesh.material.displacement_map)
            .map_or_else(|| mp.default_displacement.view(), |t| t.view());
        let metallic_roughness_view = lookup(&mesh.material.metallic_roughness_texture)
            .map_or_else(|| mp.default_metallic_roughness.view(), |t| t.view());
        let emissive_view = lookup(&mesh.material.emissive_texture)
            .map_or_else(|| mp.default_emissive.view(), |t| t.view());
        let occlusion_view = lookup(&mesh.material.occlusion_texture)
            .map_or_else(|| mp.default_occlusion.view(), |t| t.view());

        // ── Upload joint matrices (skeletal animation) ───────────────────
        //
        // DT mode: upload to joint_data_texture (RGBA32Float, width=4,
        // height=num_joints). Storage-buffer mode: create a temporary
        // buffer or reuse the default one.
        let skin_joint_buf = if self.has_storage_buffers {
            mesh.skin.as_ref().map(|skin| {
                let count = skin.joint_matrices.len().min(256);
                let data: &[u8] = bytemuck::cast_slice(&skin.joint_matrices[..count]);
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Mesh Joint Matrices"),
                        contents: data,
                        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    })
            })
        } else {
            // DT mode: upload joint matrices to the data texture
            if let Some(skin) = mesh.skin.as_ref() {
                let count = skin.joint_matrices.len().min(256);
                let data: &[u8] = bytemuck::cast_slice(&skin.joint_matrices[..count]);
                if let Some(ref tex) = mp.joint_data_texture {
                    self.queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: tex,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        data,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(4 * 16), // 4 texels * 16 bytes
                            rows_per_image: None,
                        },
                        wgpu::Extent3d {
                            width: 4,
                            height: count as u32,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }
            None
        };
        let joint_buffer_ref = skin_joint_buf.as_ref().unwrap_or(&mp.joint_buffer);

        // Binding 8: joint matrices — storage buffer or data texture
        let joint_binding_8 = if self.has_storage_buffers {
            wgpu::BindGroupEntry {
                binding: 8,
                resource: joint_buffer_ref.as_entire_binding(),
            }
        } else {
            // DT mode: bind the joint data texture view
            wgpu::BindGroupEntry {
                binding: 8,
                resource: wgpu::BindingResource::TextureView(
                    mp.joint_data_view
                        .as_ref()
                        .expect("joint_data_view in DT mode"),
                ),
            }
        };

        // ── Create bind group ────────────────────────────────────────────
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Mesh Bind Group"),
            layout: &mp.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: mp.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: mp.material_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(base_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&mp.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(normal_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&mp.shadow_view),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::Sampler(&mp.shadow_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(displacement_view),
                },
                joint_binding_8,
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: wgpu::BindingResource::TextureView(metallic_roughness_view),
                },
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::TextureView(emissive_view),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::TextureView(occlusion_view),
                },
                wgpu::BindGroupEntry {
                    binding: 12,
                    resource: wgpu::BindingResource::TextureView(&mp.env_cubemap_view),
                },
                wgpu::BindGroupEntry {
                    binding: 13,
                    resource: wgpu::BindingResource::Sampler(&mp.env_sampler),
                },
            ],
        });

        // ── Main depth buffer ────────────────────────────────────────────
        //
        // Lazily (re)allocate a depth texture matching the current
        // viewport. Cached on the MeshPipeline so successive frames
        // reuse it; resized when the viewport changes. Without this,
        // the main mesh pass has no depth test and back faces draw
        // over front faces in mesh submission order.
        //
        // NOTE: we re-borrow `self.mesh_pipeline` here via a mutable
        // path because the earlier `let mp = self.mesh_pipeline.as_ref()`
        // borrow ended at the bind_group creation above.
        // ── Lazily (re)allocate depth + HDR textures ─────────────────────
        let (viewport_w, viewport_h) = self.viewport_size;
        {
            let mp_mut = self.mesh_pipeline.as_mut().unwrap();
            let size = (viewport_w.max(1), viewport_h.max(1));
            if mp_mut.main_depth_size != size || mp_mut.main_depth.is_none() {
                let depth = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("Mesh Main Depth"),
                    size: wgpu::Extent3d {
                        width: size.0,
                        height: size.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Depth32Float,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
                mp_mut.main_depth_view = Some(depth.create_view(&Default::default()));
                mp_mut.main_depth = Some(depth);
                mp_mut.main_depth_size = size;
            }
            if mp_mut.hdr_size != size || mp_mut.hdr_texture.is_none() {
                let hdr = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("Mesh HDR Intermediate"),
                    size: wgpu::Extent3d {
                        width: size.0,
                        height: size.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                mp_mut.hdr_view = Some(hdr.create_view(&Default::default()));
                mp_mut.hdr_texture = Some(hdr);
                mp_mut.hdr_size = size;
            }
            // Bloom ping-pong textures at half resolution
            let bloom_w = (size.0 / 2).max(1);
            let bloom_h = (size.1 / 2).max(1);
            if mp_mut.bloom_size != (bloom_w, bloom_h) || mp_mut.bloom_a.is_none() {
                let bloom_desc = wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d {
                        width: bloom_w,
                        height: bloom_h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                };
                let a = self.device.create_texture(&bloom_desc);
                let b = self.device.create_texture(&bloom_desc);
                mp_mut.bloom_a_view = Some(a.create_view(&Default::default()));
                mp_mut.bloom_b_view = Some(b.create_view(&Default::default()));
                mp_mut.bloom_a = Some(a);
                mp_mut.bloom_b = Some(b);
                mp_mut.bloom_size = (bloom_w, bloom_h);
            }
        }

        // ── Skybox bind group (empty — shader is screen-space only) ─────
        let mp = self.mesh_pipeline.as_ref().unwrap();
        let skybox_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Skybox Bind Group"),
            layout: &mp.skybox_bind_group_layout,
            entries: &[],
        });

        // ── HDR pass: skybox + mesh → Rgba16Float intermediate ──────────
        let depth_view = mp
            .main_depth_view
            .as_ref()
            .expect("main_depth_view populated above");
        let hdr_view = mp.hdr_view.as_ref().expect("hdr_view populated above");
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Mesh Render"),
            });

        // Sub-pass A: skybox → HDR (no depth attachment — the skybox
        // pipeline was created without depth_stencil so it can't share
        // a render pass with the mesh pipeline that has depth enabled).
        //
        // Only the first mesh in a batch runs the skybox pass; it also
        // owns the `LoadOp::Clear` that resets the HDR intermediate
        // for the whole frame. Later meshes in the same batch skip
        // both so their contributions accumulate in HDR instead of
        // being wiped away.
        if is_first {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Skybox Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: hdr_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });
            pass.set_pipeline(&mp.skybox_pipeline);
            pass.set_bind_group(0, &skybox_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        // Sub-pass: Scene3D custom passes → HDR (between skybox and mesh).
        // Submit the skybox encoder first, then run Scene3D passes
        // (which create their own encoders), then re-borrow mp for mesh.
        self.queue.submit(std::iter::once(encoder.finish()));

        // Scene3D custom passes also need to run once per frame, not
        // per mesh — otherwise a 3D grid/lines pass would re-render
        // its geometry every mesh, stacking overdraw. Gate on
        // `is_first` for the same reason as the skybox.
        if is_first {
            let hdr_view_ptr = self
                .mesh_pipeline
                .as_ref()
                .unwrap()
                .hdr_view
                .as_ref()
                .unwrap() as *const wgpu::TextureView;
            let inv_vp = mat4_inverse_flat(view_proj);
            // SAFETY: hdr_view lives on MeshPipeline which is owned by
            // self. execute_scene3d_passes doesn't modify MeshPipeline's
            // hdr_view — it only accesses custom_passes.
            let hdr_ref = unsafe { &*hdr_view_ptr };
            self.execute_scene3d_passes(hdr_ref, 1.0, view_proj, &inv_vp, camera_pos, viewport);
        }

        // Re-create encoder + re-borrow mp for remaining passes
        let mp = self.mesh_pipeline.as_ref().unwrap();
        let depth_view = mp.main_depth_view.as_ref().unwrap();
        let hdr_view = mp.hdr_view.as_ref().unwrap();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Mesh Render (continued)"),
            });

        // Sub-pass B: mesh → HDR. Depth is `Clear(1.0)` only on the
        // first mesh of the batch — subsequent meshes `Load` so the
        // accumulated depth rejects fragments occluded by earlier
        // draws within the same frame.
        let depth_load_op = if is_first {
            wgpu::LoadOp::Clear(1.0)
        } else {
            wgpu::LoadOp::Load
        };
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Mesh HDR Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: hdr_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: depth_load_op,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });

            pass.set_pipeline(&mp.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..index_count, 0, 0..1);
        }

        // ── Bloom + tonemap passes ─────────────────────────────────────
        //
        // Only the LAST mesh in a batch composites HDR into the frame
        // target. Earlier meshes accumulate into HDR with the mesh
        // pass above and return so their contributions stay in HDR
        // until the final pass tonemaps the full scene.
        //
        // Pass B1: threshold + downsample HDR → bloom_a (half-res)
        // Pass B2: Kawase blur bloom_a → bloom_b
        // Pass B3: Kawase blur bloom_b → bloom_a (wider)
        if is_last {
            let bloom_a_view = mp.bloom_a_view.as_ref().unwrap();
            let bloom_b_view = mp.bloom_b_view.as_ref().unwrap();
            let (bloom_w, bloom_h) = mp.bloom_size;
            let bloom_texel = [1.0 / bloom_w as f32, 1.0 / bloom_h as f32];

            #[repr(C)]
            #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
            struct BloomUniforms {
                texel_size: [f32; 2],
                threshold: f32,
                mode: f32,
            }

            // B1: threshold+downsample HDR → bloom_a
            {
                let uniforms = BloomUniforms {
                    texel_size: bloom_texel,
                    threshold: 0.8,
                    mode: 0.0,
                };
                self.queue
                    .write_buffer(&mp.bloom_uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
                let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Bloom Threshold"),
                    layout: &mp.bloom_bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: mp.bloom_uniform_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(hdr_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&mp.tonemap_sampler),
                        },
                    ],
                });
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Bloom Threshold"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: bloom_a_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    ..Default::default()
                });
                pass.set_pipeline(&mp.bloom_pipeline);
                pass.set_bind_group(0, &bg, &[]);
                pass.draw(0..3, 0..1);
            }

            // B2: blur bloom_a → bloom_b
            {
                let uniforms = BloomUniforms {
                    texel_size: bloom_texel,
                    threshold: 0.0,
                    mode: 1.0,
                };
                self.queue
                    .write_buffer(&mp.bloom_uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
                let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Bloom Blur 1"),
                    layout: &mp.bloom_bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: mp.bloom_uniform_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(bloom_a_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&mp.tonemap_sampler),
                        },
                    ],
                });
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Bloom Blur 1"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: bloom_b_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    ..Default::default()
                });
                pass.set_pipeline(&mp.bloom_pipeline);
                pass.set_bind_group(0, &bg, &[]);
                pass.draw(0..3, 0..1);
            }

            // B3: blur bloom_b → bloom_a (second iteration for wider glow)
            {
                let uniforms = BloomUniforms {
                    texel_size: bloom_texel,
                    threshold: 0.0,
                    mode: 1.0,
                };
                self.queue
                    .write_buffer(&mp.bloom_uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
                let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Bloom Blur 2"),
                    layout: &mp.bloom_bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: mp.bloom_uniform_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(bloom_b_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&mp.tonemap_sampler),
                        },
                    ],
                });
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Bloom Blur 2"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: bloom_a_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    ..Default::default()
                });
                pass.set_pipeline(&mp.bloom_pipeline);
                pass.set_bind_group(0, &bg, &[]);
                pass.draw(0..3, 0..1);
            }

            // Pass 2: tonemap HDR → framebuffer (ACES filmic + sRGB gamma)
            //
            // The tonemap pass reads the Rgba16Float intermediate and the
            // bloom result, composites them, and writes tonemapped sRGB to
            // the caller's frame target. Viewport/scissor are set here (not
            // on the mesh pass) so the tonemap fullscreen triangle only
            // writes within the canvas bounds while the mesh pass renders at
            // full HDR resolution for correct edge sampling.
            {
                let tonemap_bind_group =
                    self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("Tonemap Bind Group"),
                        layout: &mp.tonemap_bind_group_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(hdr_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(&mp.tonemap_sampler),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::TextureView(bloom_a_view),
                            },
                        ],
                    });

                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Tonemap Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    ..Default::default()
                });

                pass.set_pipeline(&mp.tonemap_pipeline);

                if let Some([vx, vy, vw, vh]) = viewport {
                    pass.set_viewport(vx, vy, vw.max(1.0), vh.max(1.0), 0.0, 1.0);
                    pass.set_scissor_rect(
                        vx.max(0.0) as u32,
                        vy.max(0.0) as u32,
                        vw.max(1.0) as u32,
                        vh.max(1.0) as u32,
                    );
                }

                pass.set_bind_group(0, &tonemap_bind_group, &[]);
                pass.draw(0..3, 0..1); // fullscreen triangle
            }
        } // end `if is_last` — bloom + tonemap run only on final mesh

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    // ─── Custom Render Pass API ────────────────────────────────────────────

    /// Register a custom render pass.
    ///
    /// The pass will be initialized immediately and executed each frame
    /// at the stage returned by `pass.stage()`.
    pub fn register_custom_pass(
        &mut self,
        mut pass: Box<dyn crate::custom_pass::CustomRenderPass>,
    ) {
        pass.initialize(&self.device, &self.queue, self.texture_format);
        self.custom_passes.register(pass);
    }

    /// Remove a custom render pass by label.
    pub fn remove_custom_pass(&mut self, label: &str) -> bool {
        self.custom_passes.remove(label)
    }

    /// Execute all custom passes for a given stage.
    pub fn execute_custom_passes(
        &mut self,
        stage: crate::custom_pass::RenderStage,
        target: &wgpu::TextureView,
        scale_factor: f64,
    ) {
        if !self.custom_passes.has_passes(stage) {
            return;
        }
        let ctx = crate::custom_pass::RenderPassContext {
            device: &self.device,
            queue: &self.queue,
            target,
            viewport_width: self.viewport_size.0,
            viewport_height: self.viewport_size.1,
            texture_format: self.texture_format,
            scale_factor,
            view_proj: None,
            inv_view_proj: None,
            camera_pos: None,
            viewport: None,
        };
        self.custom_passes.execute_stage(stage, &ctx);
    }

    /// Execute Scene3D custom passes with camera context.
    pub fn execute_scene3d_passes(
        &mut self,
        target: &wgpu::TextureView,
        scale_factor: f64,
        view_proj: &[f32; 16],
        inv_view_proj: &[f32; 16],
        camera_pos: [f32; 3],
        viewport: Option<[f32; 4]>,
    ) {
        let stage = crate::custom_pass::RenderStage::Scene3D;
        if !self.custom_passes.has_passes(stage) {
            return;
        }
        let ctx = crate::custom_pass::RenderPassContext {
            device: &self.device,
            queue: &self.queue,
            target,
            viewport_width: self.viewport_size.0,
            viewport_height: self.viewport_size.1,
            texture_format: self.texture_format,
            scale_factor,
            view_proj: Some(*view_proj),
            inv_view_proj: Some(*inv_view_proj),
            camera_pos: Some(camera_pos),
            viewport,
        };
        self.custom_passes.execute_stage(stage, &ctx);
    }

    /// Notify custom passes of a viewport resize.
    pub fn resize_custom_passes(&mut self, width: u32, height: u32) {
        self.custom_passes.resize(&self.device, width, height);
    }

    // ─── GPU Memory Budget ─────────────────────────────────────────────────

    /// Enforce the GPU memory budget by evicting cached textures.
    ///
    /// Call once per frame (e.g., at frame start) to keep memory in check.
    /// Evicts largest pooled textures first, then trims mask image cache
    /// if still over budget.
    pub fn enforce_memory_budget(&mut self) {
        self.memory_budget.reset_transient();

        if self.memory_budget.budget() == 0 {
            return; // unlimited
        }

        let layer_bytes = self.layer_texture_cache.stats().total_memory_bytes();
        if !self.memory_budget.is_over_budget(layer_bytes) {
            return;
        }

        // Phase 1: evict pooled layer textures (largest first)
        let target = self
            .memory_budget
            .budget()
            .saturating_sub(self.memory_budget.mask_image_bytes);
        let freed = self.layer_texture_cache.evict_to_budget(target);
        if freed > 0 {
            self.memory_budget.record_eviction();
        }

        // Phase 2: if still over, trim mask image cache (drop oldest entries)
        let layer_bytes = self.layer_texture_cache.stats().total_memory_bytes();
        if self.memory_budget.is_over_budget(layer_bytes) && !self.mask_image_cache.is_empty() {
            // Remove one entry at a time until under budget
            let keys: Vec<String> = self.mask_image_cache.keys().cloned().collect();
            for key in keys {
                if !self
                    .memory_budget
                    .is_over_budget(self.layer_texture_cache.stats().total_memory_bytes())
                {
                    break;
                }
                if let Some(img) = self.mask_image_cache.remove(&key) {
                    let (w, h) = img.dimensions();
                    self.memory_budget.untrack_mask_image(w, h);
                    self.memory_budget.record_eviction();
                }
            }
        }
    }

    /// Get the current GPU memory budget tracker.
    pub fn memory_budget(&self) -> &GpuMemoryBudget {
        &self.memory_budget
    }

    /// Get estimated total GPU texture memory usage in bytes.
    pub fn estimated_texture_memory(&self) -> u64 {
        let layer_bytes = self.layer_texture_cache.stats().total_memory_bytes();
        self.memory_budget.total_tracked_bytes(layer_bytes)
    }

    /// Get a reference to the layer texture cache
    pub fn layer_texture_cache(&self) -> &LayerTextureCache {
        &self.layer_texture_cache
    }

    /// Get a mutable reference to the layer texture cache
    pub fn layer_texture_cache_mut(&mut self) -> &mut LayerTextureCache {
        &mut self.layer_texture_cache
    }

    /// Acquire a layer texture from the cache
    ///
    /// If a matching texture exists in the pool, it will be reused.
    /// Otherwise, a new texture will be created.
    pub fn acquire_layer_texture(&mut self, size: (u32, u32), with_depth: bool) -> LayerTexture {
        self.layer_texture_cache
            .acquire(&self.device, size, with_depth)
    }

    /// Release a layer texture back to the cache pool
    pub fn release_layer_texture(&mut self, texture: LayerTexture) {
        self.layer_texture_cache.release(texture);
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // Layer Composition
    // ─────────────────────────────────────────────────────────────────────────────

    /// Create a bind group for layer composition
    fn create_layer_composite_bind_group(
        &self,
        uniform_buffer: &wgpu::Buffer,
        layer_view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        self.create_layer_composite_bind_group_with_dest(
            uniform_buffer,
            layer_view,
            sampler,
            &self.dummy_blend_dest_view,
            sampler,
        )
    }

    fn create_layer_composite_bind_group_with_dest(
        &self,
        uniform_buffer: &wgpu::Buffer,
        layer_view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
        dest_view: &wgpu::TextureView,
        dest_sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Layer Composite Bind Group"),
            layout: &self.bind_group_layouts.layer_composite,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(layer_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(dest_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(dest_sampler),
                },
            ],
        })
    }

    /// Composite a layer texture onto a target
    ///
    /// Uses the LAYER_COMPOSITE_SHADER to blend the layer onto the target
    /// with the specified blend mode and opacity.
    #[allow(clippy::too_many_arguments)]
    pub fn composite_layer(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        layer: &LayerTexture,
        dest_x: f32,
        dest_y: f32,
        opacity: f32,
        blend_mode: blinc_core::BlendMode,
    ) {
        // Create uniform buffer for this composition
        let uniforms = crate::primitives::LayerCompositeUniforms::new(
            layer.size,
            dest_x,
            dest_y,
            (self.viewport_size.0 as f32, self.viewport_size.1 as f32),
            opacity,
            blend_mode,
        );

        let uniform_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Layer Composite Uniforms"),
                contents: bytemuck::bytes_of(&uniforms),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        // Create sampler
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Layer Composite Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // Create bind group
        let bind_group =
            self.create_layer_composite_bind_group(&uniform_buffer, &layer.view, &sampler);

        // Create render pass and draw
        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Layer Composite Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load, // Preserve existing content
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        render_pass.set_pipeline(&self.pipelines.layer_composite);
        render_pass.set_bind_group(0, &bind_group, &[]);
        render_pass.draw(0..6, 0..1); // 6 vertices for quad (2 triangles)
    }

    /// Composite a layer with source/dest rectangle mapping
    ///
    /// Allows sampling a sub-region of the layer texture and placing it
    /// at a specific destination in the target.
    #[allow(clippy::too_many_arguments)]
    pub fn composite_layer_region(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        layer: &LayerTexture,
        source_rect: blinc_core::Rect,
        dest_rect: blinc_core::Rect,
        opacity: f32,
        blend_mode: blinc_core::BlendMode,
    ) {
        // Convert source rect to normalized UV coordinates
        let layer_w = layer.size.0 as f32;
        let layer_h = layer.size.1 as f32;
        let source_uv = [
            source_rect.x() / layer_w,
            source_rect.y() / layer_h,
            source_rect.width() / layer_w,
            source_rect.height() / layer_h,
        ];

        let uniforms = crate::primitives::LayerCompositeUniforms::with_source_rect(
            source_uv,
            [
                dest_rect.x(),
                dest_rect.y(),
                dest_rect.width(),
                dest_rect.height(),
            ],
            (self.viewport_size.0 as f32, self.viewport_size.1 as f32),
            opacity,
            blend_mode,
        );

        let uniform_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Layer Composite Uniforms"),
                contents: bytemuck::bytes_of(&uniforms),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Layer Composite Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bind_group =
            self.create_layer_composite_bind_group(&uniform_buffer, &layer.view, &sampler);

        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Layer Composite Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        render_pass.set_pipeline(&self.pipelines.layer_composite);
        render_pass.set_bind_group(0, &bind_group, &[]);
        render_pass.draw(0..6, 0..1);
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // Effect Application Methods
    // ─────────────────────────────────────────────────────────────────────────────

    /// Apply a single Kawase blur pass
    ///
    /// Renders from `input` to `output` using the blur shader with the specified
    /// radius and iteration index.
    ///
    /// `blur_alpha`: if true, blurs both RGB and alpha (for soft shadow edges);
    ///               if false, preserves alpha while blurring RGB (for element blur)
    /// Apply multi-pass Kawase blur, batched into a single GPU submission.
    ///
    /// Uses ping-pong rendering between two textures. All passes share one
    /// command encoder for minimal GPU synchronization overhead.
    ///
    /// `blur_alpha`: if true, blurs both RGB and alpha (for soft shadow edges);
    ///               if false, preserves alpha while blurring RGB (for element blur)
    ///
    /// Returns the final output texture (caller should release temp textures).
    pub fn apply_blur_with_alpha(
        &mut self,
        input: &LayerTexture,
        radius: f32,
        passes: u32,
        blur_alpha: bool,
    ) -> LayerTexture {
        self.ensure_blur_pipeline();

        if passes == 0 {
            // No blur needed, return a copy
            let output = self
                .layer_texture_cache
                .acquire(&self.device, input.size, false);
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Blur Copy Encoder"),
                });
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &input.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &output.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: input.size.0,
                    height: input.size.1,
                    depth_or_array_layers: 1,
                },
            );
            self.queue.submit(std::iter::once(encoder.finish()));
            return output;
        }

        let size = input.size;
        let blur_alpha_u32: u32 = if blur_alpha { 1 } else { 0 };

        // Write per-pass uniforms to pre-allocated buffer pool (no allocation)
        let blur_pool = self.buffers.blur_uniforms_pool.as_ref().unwrap();
        for i in 0..passes {
            self.queue.write_buffer(
                &blur_pool[i as usize],
                0,
                bytemuck::bytes_of(&BlurUniforms {
                    texel_size: [1.0 / size.0 as f32, 1.0 / size.1 as f32],
                    radius,
                    iteration: i,
                    blur_alpha: blur_alpha_u32,
                    _pad1: 0.0,
                    _pad2: 0.0,
                    _pad3: 0.0,
                }),
            );
        }

        // For ping-pong we need two temp textures
        let temp_a = self.layer_texture_cache.acquire(&self.device, size, false);
        let temp_b = self.layer_texture_cache.acquire(&self.device, size, false);

        // Pre-create bind groups: pass 0 reads input, subsequent passes alternate temp_a/temp_b
        let bind_groups: Vec<wgpu::BindGroup> = (0..passes)
            .map(|i| {
                let input_view = if i == 0 {
                    &input.view
                } else if i % 2 == 1 {
                    &temp_a.view
                } else {
                    &temp_b.view
                };
                let blur_pool = self.buffers.blur_uniforms_pool.as_ref().unwrap();
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Blur Effect Bind Group"),
                    layout: &self.bind_group_layouts.blur,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: blur_pool[i as usize].as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(input_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&self.path_image_sampler),
                        },
                    ],
                })
            })
            .collect();

        // Single command encoder for all passes
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Blur Multi-Pass Encoder"),
            });

        for i in 0..passes {
            let output_view = if i % 2 == 0 {
                &temp_a.view
            } else {
                &temp_b.view
            };

            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Blur Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(self.effect_pipelines.blur.as_ref().unwrap());
            render_pass.set_bind_group(0, &bind_groups[i as usize], &[]);
            render_pass.draw(0..6, 0..1);
        }

        // Single GPU submission for all blur passes
        self.queue.submit(std::iter::once(encoder.finish()));

        // Determine which texture has the final blurred result
        let (result, unused) = if passes % 2 == 1 {
            (temp_a, temp_b)
        } else {
            (temp_b, temp_a)
        };
        self.layer_texture_cache.release(unused);

        result
    }

    /// Apply multi-pass Kawase blur (CSS filter blur)
    ///
    /// Blurs both RGB and alpha channels, producing soft edges.
    pub fn apply_blur(&mut self, input: &LayerTexture, radius: f32, passes: u32) -> LayerTexture {
        self.apply_blur_with_alpha(input, radius, passes, false)
    }

    /// Apply multi-pass Kawase blur (shadow blur - blurs alpha for soft edges)
    ///
    /// Used for drop shadow and glow effects where we need soft alpha falloff.
    pub fn apply_shadow_blur(
        &mut self,
        input: &LayerTexture,
        radius: f32,
        passes: u32,
    ) -> LayerTexture {
        self.apply_blur_with_alpha(input, radius, passes, true)
    }

    /// Apply color matrix transformation
    ///
    /// Transforms colors using a 4x5 matrix (4x4 matrix + offset column).
    /// Useful for grayscale, sepia, saturation, brightness, contrast, etc.
    pub fn apply_color_matrix(
        &mut self,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
        matrix: &[f32; 20],
    ) {
        self.ensure_color_matrix_pipeline();

        let uniforms = ColorMatrixUniforms::from_matrix(matrix);

        // Use cached buffer instead of creating per-pass
        let cm_buf = self.buffers.color_matrix_uniforms.as_ref().unwrap();
        self.queue
            .write_buffer(cm_buf, 0, bytemuck::bytes_of(&uniforms));

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Color Matrix Effect Bind Group"),
            layout: &self.bind_group_layouts.color_matrix,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: cm_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(input),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.path_image_sampler),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Color Matrix Pass Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Color Matrix Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(self.effect_pipelines.color_matrix.as_ref().unwrap());
            render_pass.set_bind_group(0, &bind_group, &[]);
            render_pass.draw(0..6, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Apply drop shadow effect
    ///
    /// Takes a pre-blurred texture (for shadow shape) and the original texture (for compositing).
    /// The blurred texture's alpha is used to create the shadow, which is then colored and
    /// composited behind the original content.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_drop_shadow(
        &mut self,
        blurred_input: &wgpu::TextureView,
        original_input: &wgpu::TextureView,
        output: &wgpu::TextureView,
        size: (u32, u32),
        offset: (f32, f32),
        blur_radius: f32,
        spread: f32,
        color: [f32; 4],
    ) {
        self.ensure_drop_shadow_pipeline();

        let uniforms = DropShadowUniforms {
            offset: [offset.0, offset.1],
            blur_radius,
            spread,
            color,
            texel_size: [1.0 / size.0 as f32, 1.0 / size.1 as f32],
            _pad: [0.0, 0.0],
        };

        // Use cached buffer instead of creating per-pass
        let ds_buf = self.buffers.drop_shadow_uniforms.as_ref().unwrap();
        self.queue
            .write_buffer(ds_buf, 0, bytemuck::bytes_of(&uniforms));

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Drop Shadow Effect Bind Group"),
            layout: &self.bind_group_layouts.drop_shadow,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ds_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(blurred_input),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.path_image_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(original_input),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Drop Shadow Pass Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Drop Shadow Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(self.effect_pipelines.drop_shadow.as_ref().unwrap());
            render_pass.set_bind_group(0, &bind_group, &[]);
            render_pass.draw(0..6, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Apply glow effect to a texture
    ///
    /// Creates a radial glow around the shape by finding distance to nearest opaque pixels
    /// and applying a smooth falloff based on blur and range parameters.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_glow(
        &mut self,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
        size: (u32, u32),
        color: [f32; 4],
        blur: f32,
        range: f32,
        opacity: f32,
    ) {
        self.ensure_glow_pipeline();

        let uniforms = GlowUniforms {
            color,
            blur,
            range,
            opacity,
            _pad0: 0.0,
            texel_size: [1.0 / size.0 as f32, 1.0 / size.1 as f32],
            _pad1: [0.0, 0.0],
        };

        // Use cached buffer instead of creating per-pass
        let glow_buf = self.buffers.glow_uniforms.as_ref().unwrap();
        self.queue
            .write_buffer(glow_buf, 0, bytemuck::bytes_of(&uniforms));

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Glow Effect Bind Group"),
            layout: &self.bind_group_layouts.glow,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: glow_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(input),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.path_image_sampler),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Glow Pass Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Glow Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(self.effect_pipelines.glow.as_ref().unwrap());
            render_pass.set_bind_group(0, &bind_group, &[]);
            render_pass.draw(0..6, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Helper to create common color matrices
    pub fn grayscale_matrix() -> [f32; 20] {
        // Luminance weights (ITU-R BT.709)
        let r = 0.2126;
        let g = 0.7152;
        let b = 0.0722;
        [
            r, g, b, 0.0, 0.0, r, g, b, 0.0, 0.0, r, g, b, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0,
        ]
    }

    /// Create sepia tone color matrix
    pub fn sepia_matrix() -> [f32; 20] {
        [
            0.393, 0.769, 0.189, 0.0, 0.0, 0.349, 0.686, 0.168, 0.0, 0.0, 0.272, 0.534, 0.131, 0.0,
            0.0, 0.0, 0.0, 0.0, 1.0, 0.0,
        ]
    }

    /// Create saturation adjustment matrix
    pub fn saturation_matrix(saturation: f32) -> [f32; 20] {
        let s = saturation;
        let r = 0.2126;
        let g = 0.7152;
        let b = 0.0722;
        let sr = (1.0 - s) * r;
        let sg = (1.0 - s) * g;
        let sb = (1.0 - s) * b;
        [
            sr + s,
            sg,
            sb,
            0.0,
            0.0,
            sr,
            sg + s,
            sb,
            0.0,
            0.0,
            sr,
            sg,
            sb + s,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
        ]
    }

    /// Create brightness adjustment matrix
    pub fn brightness_matrix(brightness: f32) -> [f32; 20] {
        let b = brightness - 1.0; // 0 = no change, positive = brighter
        [
            1.0, 0.0, 0.0, 0.0, b, 0.0, 1.0, 0.0, 0.0, b, 0.0, 0.0, 1.0, 0.0, b, 0.0, 0.0, 0.0,
            1.0, 0.0,
        ]
    }

    /// Create contrast adjustment matrix
    pub fn contrast_matrix(contrast: f32) -> [f32; 20] {
        let c = contrast;
        let t = (1.0 - c) / 2.0;
        [
            c, 0.0, 0.0, 0.0, t, 0.0, c, 0.0, 0.0, t, 0.0, 0.0, c, 0.0, t, 0.0, 0.0, 0.0, 1.0, 0.0,
        ]
    }

    /// Create invert color matrix
    pub fn invert_matrix() -> [f32; 20] {
        [
            -1.0, 0.0, 0.0, 0.0, 1.0, 0.0, -1.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 1.0, 0.0, 0.0,
            0.0, 1.0, 0.0,
        ]
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // Layer Command Processing
    // ─────────────────────────────────────────────────────────────────────────────

    /// Calculate how much layer effects extend beyond the original content bounds.
    ///
    /// Returns (left, top, right, bottom) expansion in pixels.
    /// Blur expands bounds so the soft-edge falloff has room to render.
    fn calculate_effect_expansion(effects: &[blinc_core::LayerEffect]) -> (f32, f32, f32, f32) {
        use blinc_core::LayerEffect;

        let mut left = 0.0f32;
        let mut top = 0.0f32;
        let mut right = 0.0f32;
        let mut bottom = 0.0f32;

        for effect in effects {
            match effect {
                LayerEffect::Blur { radius, .. } => {
                    // Blur softens edges, which extends beyond original bounds.
                    // ~2x radius covers the visible falloff of Kawase blur.
                    let expand = radius * 2.0;
                    left = left.max(expand);
                    top = top.max(expand);
                    right = right.max(expand);
                    bottom = bottom.max(expand);
                }
                LayerEffect::DropShadow {
                    offset_x,
                    offset_y,
                    blur,
                    spread,
                    ..
                } => {
                    // Shadow expands based on blur, spread, and offset
                    let blur_expand = blur * 2.0; // 2x blur radius is enough
                    let spread_expand = spread.max(0.0);
                    let total_expand = blur_expand + spread_expand;

                    // Left/top expansion: when offset is negative, shadow goes that direction
                    left = left.max(total_expand + (-offset_x).max(0.0));
                    top = top.max(total_expand + (-offset_y).max(0.0));
                    // Right/bottom expansion: when offset is positive, shadow goes that direction
                    right = right.max(total_expand + offset_x.max(0.0));
                    bottom = bottom.max(total_expand + offset_y.max(0.0));
                }
                LayerEffect::Glow { blur, range, .. } => {
                    // Glow expands equally in all directions
                    let expand = (blur + range) * 2.0; // Account for range
                    left = left.max(expand);
                    top = top.max(expand);
                    right = right.max(expand);
                    bottom = bottom.max(expand);
                }
                LayerEffect::ColorMatrix { .. } | LayerEffect::MaskImage { .. } => {
                    // These don't expand bounds
                }
            }
        }

        (left, top, right, bottom)
    }

    /// No-op: mask images must be pre-loaded via `load_mask_image_rgba()`.
    fn load_mask_image(&mut self, _url: &str) {
        // Mask images are loaded externally (in blinc_app context) and cached
        // via load_mask_image_rgba() before the render pass begins.
    }

    /// Pre-load a mask image from RGBA pixel data.
    /// Call this before rendering to ensure mask textures are available.
    pub fn load_mask_image_rgba(&mut self, url: &str, pixels: &[u8], width: u32, height: u32) {
        if self.mask_image_cache.contains_key(url) {
            return;
        }
        let gpu_img = crate::image::GpuImage::from_rgba(
            &self.device,
            &self.queue,
            pixels,
            width,
            height,
            Some(&format!("mask:{}", url)),
        );
        self.mask_image_cache.insert(url.to_string(), gpu_img);
    }

    /// Check if a mask image is already loaded in cache
    pub fn has_mask_image(&self, url: &str) -> bool {
        self.mask_image_cache.contains_key(url)
    }

    /// Apply mask image effect: multiplies element alpha by mask value
    fn apply_mask_image_effect(
        &mut self,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
        image_url: &str,
        mask_mode: u32,
    ) {
        self.ensure_mask_image_pipeline();
        let mask_img = match self.mask_image_cache.get(image_url) {
            Some(img) => img,
            None => return,
        };

        let uniforms = MaskImageUniforms {
            mask_mode,
            _pad: [0.0; 3],
        };

        let uniform_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Mask Image Uniforms"),
                contents: bytemuck::bytes_of(&uniforms),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Mask Image Effect Bind Group"),
            layout: &self.bind_group_layouts.mask_image,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(input),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.path_image_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(mask_img.view()),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.path_image_sampler),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Mask Image Pass Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Mask Image Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(self.effect_pipelines.mask_image.as_ref().unwrap());
            render_pass.set_bind_group(0, &bind_group, &[]);
            render_pass.draw(0..6, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Apply layer effects to a texture
    ///
    /// Processes a list of LayerEffects in order and returns the final result.
    /// The input texture is not modified; a new texture with effects applied is returned.
    pub fn apply_layer_effects(
        &mut self,
        input: &LayerTexture,
        effects: &[blinc_core::LayerEffect],
    ) -> LayerTexture {
        use blinc_core::LayerEffect;

        if effects.is_empty() {
            // No effects, just return a copy
            let output = self
                .layer_texture_cache
                .acquire(&self.device, input.size, false);
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Layer Effect Copy Encoder"),
                });
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &input.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &output.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: input.size.0,
                    height: input.size.1,
                    depth_or_array_layers: 1,
                },
            );
            self.queue.submit(std::iter::once(encoder.finish()));
            return output;
        }

        let size = input.size;
        // Track ownership: effects that produce a new texture pass ownership here.
        // We avoid a redundant copy by using the input directly for the first effect
        // and only copying when a non-blur effect needs a mutable working texture.
        let mut current: Option<LayerTexture> = None;

        for effect in effects {
            // Get the current working texture or the original input
            let working = current.as_ref().unwrap_or(input);

            match effect {
                LayerEffect::Blur { radius, quality: _ } => {
                    // Blur reads from working and produces a new texture (no copy needed)
                    let passes = ((*radius / 2.0).ceil().max(2.0) as u32).min(8);
                    let blurred = self.apply_blur(working, *radius, passes);
                    if let Some(prev) = current.take() {
                        self.layer_texture_cache.release(prev);
                    }
                    current = Some(blurred);
                }

                LayerEffect::DropShadow {
                    offset_x,
                    offset_y,
                    blur,
                    spread,
                    color,
                } => {
                    let temp = self.layer_texture_cache.acquire(&self.device, size, false);
                    self.apply_drop_shadow(
                        &working.view,
                        &working.view,
                        &temp.view,
                        size,
                        (*offset_x, *offset_y),
                        *blur,
                        *spread,
                        [color.r, color.g, color.b, color.a],
                    );
                    if let Some(prev) = current.take() {
                        self.layer_texture_cache.release(prev);
                    }
                    current = Some(temp);
                }

                LayerEffect::Glow {
                    color,
                    blur,
                    range,
                    opacity,
                } => {
                    let temp = self.layer_texture_cache.acquire(&self.device, size, false);
                    self.apply_glow(
                        &working.view,
                        &temp.view,
                        size,
                        [color.r, color.g, color.b, color.a],
                        *blur,
                        *range,
                        *opacity,
                    );
                    if let Some(prev) = current.take() {
                        self.layer_texture_cache.release(prev);
                    }
                    current = Some(temp);
                }

                LayerEffect::ColorMatrix { matrix } => {
                    let temp = self.layer_texture_cache.acquire(&self.device, size, false);
                    self.apply_color_matrix(&working.view, &temp.view, matrix);
                    if let Some(prev) = current.take() {
                        self.layer_texture_cache.release(prev);
                    }
                    current = Some(temp);
                }

                LayerEffect::MaskImage {
                    image_url,
                    mask_mode,
                } => {
                    // Load mask image if not cached
                    self.load_mask_image(image_url);
                    // Apply mask if the texture was loaded successfully
                    if self.mask_image_cache.contains_key(image_url.as_str()) {
                        let temp = self.layer_texture_cache.acquire(&self.device, size, false);
                        let mode_val = match mask_mode {
                            blinc_core::MaskMode::Alpha => 0u32,
                            blinc_core::MaskMode::Luminance => 1u32,
                        };
                        self.apply_mask_image_effect(
                            &working.view,
                            &temp.view,
                            image_url,
                            mode_val,
                        );
                        if let Some(prev) = current.take() {
                            self.layer_texture_cache.release(prev);
                        }
                        current = Some(temp);
                    }
                }
            }
        }

        // If no effect produced a new texture (shouldn't happen since effects is non-empty),
        // fall back to a copy
        current.unwrap_or_else(|| {
            let output = self
                .layer_texture_cache
                .acquire(&self.device, input.size, false);
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Layer Effect Fallback Copy"),
                });
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &input.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &output.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: input.size.0,
                    height: input.size.1,
                    depth_or_array_layers: 1,
                },
            );
            self.queue.submit(std::iter::once(encoder.finish()));
            output
        })
    }

    /// Composite two textures together
    ///
    /// Blends `top` over `bottom` using the specified blend mode and opacity.
    pub fn composite_textures(
        &mut self,
        bottom: &wgpu::TextureView,
        top: &wgpu::TextureView,
        output: &wgpu::TextureView,
        size: (u32, u32),
        blend_mode: blinc_core::BlendMode,
        opacity: f32,
    ) {
        use crate::primitives::CompositeUniforms;

        let uniforms = CompositeUniforms {
            opacity,
            blend_mode: blend_mode as u32,
            _padding: [0.0; 2],
        };

        let uniform_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Composite Uniforms Buffer"),
                contents: bytemuck::cast_slice(&[uniforms]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Composite Bind Group"),
            layout: &self.bind_group_layouts.composite,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(bottom),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(top),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.path_image_sampler),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Composite Pass Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Composite Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(&self.pipelines.composite);
            render_pass.set_bind_group(0, &bind_group, &[]);
            render_pass.draw(0..6, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Render a range of primitives to a target
    fn render_primitive_range(
        &mut self,
        target: &wgpu::TextureView,
        batch: &PrimitiveBatch,
        start_idx: usize,
        end_idx: usize,
        clear_color: [f64; 4],
    ) {
        if start_idx >= end_idx {
            return;
        }

        // Extract the primitive range
        let _primitive_count = end_idx - start_idx;
        let primitives = &batch.primitives[start_idx..end_idx];

        if primitives.is_empty() {
            return;
        }

        // Update uniforms
        let uniforms = Uniforms {
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            _padding: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.buffers.uniforms, 0, bytemuck::bytes_of(&uniforms));

        // Sort and upload primitive range
        let sdf_ranges = self.upload_sorted_primitives(primitives);

        // Create command encoder
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Primitive Range Render Encoder"),
            });

        // Begin render pass
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Primitive Range Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear_color[0],
                            g: clear_color[1],
                            b: clear_color[2],
                            a: clear_color[3],
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_bind_group(0, &self.bind_groups.sdf, &[]);
            Self::draw_split_sdf(
                &mut render_pass,
                &self.pipelines,
                &sdf_ranges,
                false,
                self.sdf_vb_buffer(),
            );
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Render a range of primitives to a tight-fit texture with offset
    ///
    /// This method renders primitives to a texture sized to fit the content,
    /// offsetting primitive positions so they start at (0,0).
    ///
    /// Returns the texture AND the actual content size (which may differ from
    /// texture.size due to pool bucket rounding).
    fn render_primitive_range_tight(
        &mut self,
        batch: &PrimitiveBatch,
        start_idx: usize,
        end_idx: usize,
        layer_pos: (f32, f32),
        layer_size: (f32, f32),
        effect_expansion: (f32, f32, f32, f32), // (left, top, right, bottom)
    ) -> (LayerTexture, (u32, u32)) {
        // Calculate tight texture size including effect expansion
        let texture_width = (layer_size.0 + effect_expansion.0 + effect_expansion.2)
            .ceil()
            .max(1.0) as u32;
        let texture_height = (layer_size.1 + effect_expansion.1 + effect_expansion.3)
            .ceil()
            .max(1.0) as u32;

        // Round up to reasonable sizes for cache efficiency (64px increments)
        let texture_width = (texture_width.div_ceil(64) * 64).min(self.viewport_size.0);
        let texture_height = (texture_height.div_ceil(64) * 64).min(self.viewport_size.1);

        // This is the actual content size (64px rounded), which may differ from
        // the texture returned by acquire() due to bucket rounding
        let content_size = (texture_width, texture_height);

        // Acquire a texture of at least the tight size
        let layer_texture = self
            .layer_texture_cache
            .acquire(&self.device, content_size, false);

        if start_idx >= end_idx {
            return (layer_texture, content_size);
        }

        // Extract primitives and offset their positions
        let primitives = &batch.primitives[start_idx..end_idx];
        if primitives.is_empty() {
            return (layer_texture, content_size);
        }

        // Offset primitives so content starts at (effect_expansion.left, effect_expansion.top)
        // This leaves room for effects on the left/top edges
        let offset_x = layer_pos.0 - effect_expansion.0;
        let offset_y = layer_pos.1 - effect_expansion.1;

        let offset_primitives: Vec<GpuPrimitive> = primitives
            .iter()
            .map(|p| {
                let mut op = *p;
                op.bounds[0] -= offset_x;
                op.bounds[1] -= offset_y;
                // Also offset clip bounds if they're valid (not the "no clip" default)
                // Default "no clip" is [-10000.0, -10000.0, 100000.0, 100000.0]
                // A real clip has x > -5000 AND width < 90000 (reasonable viewport sizes)
                let has_real_clip = op.clip_bounds[0] > -5000.0 && op.clip_bounds[2] < 90000.0;
                if has_real_clip {
                    op.clip_bounds[0] -= offset_x;
                    op.clip_bounds[1] -= offset_y;
                }
                op
            })
            .collect();

        // Update uniforms with content size (the viewport for this tight render)
        let uniforms = Uniforms {
            viewport_size: [content_size.0 as f32, content_size.1 as f32],
            _padding: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.buffers.uniforms, 0, bytemuck::bytes_of(&uniforms));

        // Sort and upload offset primitives
        let sdf_ranges = self.upload_sorted_primitives(&offset_primitives);
        drop(offset_primitives); // Free Vec immediately - data is now on GPU

        // Create command encoder
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Tight Render Encoder"),
            });

        // Begin render pass
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Tight Primitive Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &layer_texture.view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_bind_group(0, &self.bind_groups.sdf, &[]);
            Self::draw_split_sdf(
                &mut render_pass,
                &self.pipelines,
                &sdf_ranges,
                false,
                self.sdf_vb_buffer(),
            );
        }

        self.queue.submit(std::iter::once(encoder.finish()));

        // Restore viewport uniforms for subsequent operations
        let restore_uniforms = Uniforms {
            viewport_size: [self.viewport_size.0 as f32, self.viewport_size.1 as f32],
            _padding: [0.0; 2],
        };
        self.queue.write_buffer(
            &self.buffers.uniforms,
            0,
            bytemuck::bytes_of(&restore_uniforms),
        );

        (layer_texture, content_size)
    }

    /// Blit a tight texture to the target at the correct position
    #[allow(clippy::too_many_arguments)]
    pub fn blit_tight_texture_to_target(
        &mut self,
        source: &wgpu::TextureView,
        source_size: (u32, u32),
        target: &wgpu::TextureView,
        dest_pos: (f32, f32),
        dest_size: (f32, f32),
        opacity: f32,
        blend_mode: blinc_core::BlendMode,
        clip: Option<([f32; 4], [f32; 4])>, // (clip_bounds, clip_radius)
        transform_3d: Option<blinc_core::Transform3DParams>,
    ) {
        use crate::primitives::LayerCompositeUniforms;

        let vp_w = self.viewport_size.0 as f32;
        let vp_h = self.viewport_size.1 as f32;

        // For 3D perspective transforms, compute the expanded bounding box of the
        // perspective-distorted quad corners so the scissor rect is large enough.
        let (effective_dest_pos, effective_dest_size) = if let Some(ref t3d) = transform_3d {
            let cx = dest_pos.0 + dest_size.0 * 0.5;
            let cy = dest_pos.1 + dest_size.1 * 0.5;
            let hw = dest_size.0 * 0.5;
            let hh = dest_size.1 * 0.5;
            // Project all 4 corners through perspective and find AABB
            let corners = [(-hw, -hh), (hw, -hh), (-hw, hh), (hw, hh)];
            let mut min_x = f32::MAX;
            let mut min_y = f32::MAX;
            let mut max_x = f32::MIN;
            let mut max_y = f32::MIN;
            for (lx, ly) in corners {
                // Rotate Y
                let ry_x = lx * t3d.cos_ry;
                let ry_z = lx * t3d.sin_ry;
                // Rotate X
                let rx_y = ly * t3d.cos_rx - ry_z * t3d.sin_rx;
                let rx_z = ly * t3d.sin_rx + ry_z * t3d.cos_rx;
                // Perspective
                let w = (t3d.perspective_d + rx_z) / t3d.perspective_d;
                let sx = cx + ry_x / w;
                let sy = cy + rx_y / w;
                min_x = min_x.min(sx);
                min_y = min_y.min(sy);
                max_x = max_x.max(sx);
                max_y = max_y.max(sy);
            }
            ((min_x, min_y), (max_x - min_x, max_y - min_y))
        } else {
            (dest_pos, dest_size)
        };

        // Calculate the visible region by intersecting dest rect with viewport and clip bounds
        // Start with destination rect (possibly expanded for 3D)
        let mut vis_x0 = effective_dest_pos.0;
        let mut vis_y0 = effective_dest_pos.1;
        let mut vis_x1 = effective_dest_pos.0 + effective_dest_size.0;
        let mut vis_y1 = effective_dest_pos.1 + effective_dest_size.1;

        // Intersect with viewport
        vis_x0 = vis_x0.max(0.0);
        vis_y0 = vis_y0.max(0.0);
        vis_x1 = vis_x1.min(vp_w);
        vis_y1 = vis_y1.min(vp_h);

        // Intersect with clip bounds if provided
        let (clip_bounds, clip_radius, clip_type) = match clip {
            Some((bounds, radius)) => {
                // Intersect with clip bounds
                vis_x0 = vis_x0.max(bounds[0]);
                vis_y0 = vis_y0.max(bounds[1]);
                vis_x1 = vis_x1.min(bounds[0] + bounds[2]);
                vis_y1 = vis_y1.min(bounds[1] + bounds[3]);
                (bounds, radius, 1)
            }
            None => ([0.0, 0.0, vp_w, vp_h], [0.0; 4], 0),
        };

        // Check if anything is visible
        let vis_w = vis_x1 - vis_x0;
        let vis_h = vis_y1 - vis_y0;
        if vis_w <= 0.0 || vis_h <= 0.0 {
            return; // Nothing visible, skip rendering
        }

        // For 3D perspective, the shader handles UV mapping via the full dest_rect/source_rect;
        // we just need the scissor to be large enough. Use the full source rect.
        let (source_rect, dest_rect) = if transform_3d.is_some() {
            // Full source rect, original dest rect (shader applies perspective)
            let src_total_w = dest_size.0 / source_size.0 as f32;
            let src_total_h = dest_size.1 / source_size.1 as f32;
            (
                [0.0, 0.0, src_total_w.min(1.0), src_total_h.min(1.0)],
                [dest_pos.0, dest_pos.1, dest_size.0, dest_size.1],
            )
        } else {
            // Calculate source rect based on what portion is visible
            // Map visible region back to source texture coordinates
            let src_total_w = dest_size.0 / source_size.0 as f32;
            let src_total_h = dest_size.1 / source_size.1 as f32;

            // Calculate what portion of the dest rect is visible
            let vis_offset_x = vis_x0 - dest_pos.0;
            let vis_offset_y = vis_y0 - dest_pos.1;

            // Map to source texture coordinates
            let src_x0 = (vis_offset_x / dest_size.0) * src_total_w;
            let src_y0 = (vis_offset_y / dest_size.1) * src_total_h;
            let src_w = (vis_w / dest_size.0) * src_total_w;
            let src_h = (vis_h / dest_size.1) * src_total_h;

            (
                [
                    src_x0.min(1.0),
                    src_y0.min(1.0),
                    src_w.min(1.0),
                    src_h.min(1.0),
                ],
                [vis_x0, vis_y0, vis_w, vis_h],
            )
        };

        let (perspective_d, sin_rx, cos_rx, sin_ry, cos_ry) = if let Some(ref t3d) = transform_3d {
            (
                t3d.perspective_d,
                t3d.sin_rx,
                t3d.cos_rx,
                t3d.sin_ry,
                t3d.cos_ry,
            )
        } else {
            (0.0, 0.0, 1.0, 0.0, 1.0)
        };

        let uniforms = LayerCompositeUniforms {
            source_rect,
            dest_rect,
            viewport_size: [vp_w, vp_h],
            opacity,
            blend_mode: blend_mode as u32,
            clip_bounds,
            clip_radius,
            clip_type,
            perspective_d,
            sin_rx,
            cos_rx,
            sin_ry,
            cos_ry,
            _pad: [0.0; 2],
        };

        let uniform_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Tight Blit Uniforms Buffer"),
                contents: bytemuck::cast_slice(&[uniforms]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });

        let is_blend = blend_mode != blinc_core::BlendMode::Normal;

        // For non-Normal blend modes, snapshot the target so the shader can sample dest
        let dest_snapshot = if is_blend {
            if let Some(target_ptr) = self.blend_target_ptr {
                let target_texture = unsafe { &*target_ptr };
                let temp =
                    self.layer_texture_cache
                        .acquire(&self.device, self.viewport_size, false);

                let mut copy_encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Tight Blit Blend Dest Copy"),
                        });
                copy_encoder.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: target_texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &temp.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: self.viewport_size.0,
                        height: self.viewport_size.1,
                        depth_or_array_layers: 1,
                    },
                );
                self.queue.submit(std::iter::once(copy_encoder.finish()));
                Some(temp)
            } else {
                None
            }
        } else {
            None
        };

        let bind_group = if let Some(ref snapshot) = dest_snapshot {
            self.create_layer_composite_bind_group_with_dest(
                &uniform_buffer,
                source,
                &self.path_image_sampler,
                &snapshot.view,
                &self.path_image_sampler,
            )
        } else {
            self.create_layer_composite_bind_group(
                &uniform_buffer,
                source,
                &self.path_image_sampler,
            )
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Tight Blit Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Tight Blit Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Set scissor rect to the visible region (already intersected with clip bounds)
            let scissor_x = vis_x0.max(0.0) as u32;
            let scissor_y = vis_y0.max(0.0) as u32;
            let scissor_w = vis_w.max(1.0) as u32;
            let scissor_h = vis_h.max(1.0) as u32;

            render_pass.set_scissor_rect(scissor_x, scissor_y, scissor_w, scissor_h);
            render_pass.set_pipeline(&self.pipelines.layer_composite);
            render_pass.set_bind_group(0, &bind_group, &[]);
            render_pass.draw(0..6, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));

        if let Some(snapshot) = dest_snapshot {
            self.layer_texture_cache.release(snapshot);
        }
    }

    /// Override viewport size for offscreen rendering to a smaller texture.
    /// This swaps `self.viewport_size` so all render functions (text, images, SDF)
    /// use the offscreen size for NDC conversion. Must call `restore_viewport()` after.
    pub fn set_viewport_override(&mut self, size: (u32, u32)) {
        self.saved_viewport_size = Some(self.viewport_size);
        self.viewport_size = size;
    }

    /// Restore viewport size after offscreen rendering.
    pub fn restore_viewport(&mut self) {
        if let Some(saved) = self.saved_viewport_size.take() {
            self.viewport_size = saved;
        }
    }

    /// Blit a texture to the target with blending
    ///
    /// For non-Normal blend modes, copies the target to a temp texture first
    /// so the shader can read the destination for blend computation.
    fn blit_texture_to_target(
        &mut self,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        opacity: f32,
        blend_mode: blinc_core::BlendMode,
    ) {
        use crate::primitives::LayerCompositeUniforms;

        let is_blend = blend_mode != blinc_core::BlendMode::Normal;

        // For non-Normal blend modes, copy the target to a temp texture
        // so the shader can sample the destination
        let dest_snapshot = if is_blend {
            if let Some(target_ptr) = self.blend_target_ptr {
                // Safety: pointer is valid for the duration of the render frame
                let target_texture = unsafe { &*target_ptr };
                let temp =
                    self.layer_texture_cache
                        .acquire(&self.device, self.viewport_size, false);

                let mut copy_encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Blend Dest Copy Encoder"),
                        });
                copy_encoder.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: target_texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &temp.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: self.viewport_size.0,
                        height: self.viewport_size.1,
                        depth_or_array_layers: 1,
                    },
                );
                self.queue.submit(std::iter::once(copy_encoder.finish()));
                Some(temp)
            } else {
                // No target texture available — fall back to Normal blend
                None
            }
        } else {
            None
        };

        // Full viewport blit - source covers entire texture, dest covers entire viewport
        let vp_w = self.viewport_size.0 as f32;
        let vp_h = self.viewport_size.1 as f32;
        let effective_blend = if dest_snapshot.is_some() {
            blend_mode
        } else {
            blinc_core::BlendMode::Normal
        };
        let uniforms = LayerCompositeUniforms {
            source_rect: [0.0, 0.0, 1.0, 1.0], // Full texture (normalized)
            dest_rect: [0.0, 0.0, vp_w, vp_h],
            viewport_size: [vp_w, vp_h],
            opacity,
            blend_mode: effective_blend as u32,
            clip_bounds: [0.0, 0.0, vp_w, vp_h], // No clipping
            clip_radius: [0.0, 0.0, 0.0, 0.0],
            clip_type: 0,
            perspective_d: 0.0,
            sin_rx: 0.0,
            cos_rx: 1.0,
            sin_ry: 0.0,
            cos_ry: 1.0,
            _pad: [0.0; 2],
        };

        let uniform_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Blit Uniforms Buffer"),
                contents: bytemuck::cast_slice(&[uniforms]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });

        let bind_group = if let Some(ref snapshot) = dest_snapshot {
            self.create_layer_composite_bind_group_with_dest(
                &uniform_buffer,
                source,
                &self.path_image_sampler,
                &snapshot.view,
                &self.path_image_sampler,
            )
        } else {
            self.create_layer_composite_bind_group(
                &uniform_buffer,
                source,
                &self.path_image_sampler,
            )
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Blit Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Blit Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        // Load existing content - we're blending on top
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(&self.pipelines.layer_composite);
            render_pass.set_bind_group(0, &bind_group, &[]);
            render_pass.draw(0..6, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));

        // Release the dest snapshot texture
        if let Some(snapshot) = dest_snapshot {
            self.layer_texture_cache.release(snapshot);
        }
    }

    /// Blit a specific region from source texture to target at given position
    ///
    /// This is used for layer effects where we need to composite only the
    /// element's region back to the target at the correct position.
    fn blit_region_to_target(
        &mut self,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        position: (f32, f32),
        size: (f32, f32),
        opacity: f32,
        blend_mode: blinc_core::BlendMode,
    ) {
        self.blit_region_to_target_with_clip(
            source, target, position, size, opacity, blend_mode, None,
        )
    }

    /// Blit a specific region with optional clip
    #[allow(clippy::too_many_arguments)]
    fn blit_region_to_target_with_clip(
        &mut self,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        position: (f32, f32),
        size: (f32, f32),
        opacity: f32,
        blend_mode: blinc_core::BlendMode,
        clip: Option<([f32; 4], [f32; 4])>, // (bounds, radii)
    ) {
        use crate::primitives::LayerCompositeUniforms;

        let vp_w = self.viewport_size.0 as f32;
        let vp_h = self.viewport_size.1 as f32;

        // Source rect in normalized coordinates (0-1)
        // The source texture is viewport-sized, so we extract the element's region
        let source_rect = [
            position.0 / vp_w,
            position.1 / vp_h,
            size.0 / vp_w,
            size.1 / vp_h,
        ];

        // Dest rect in viewport pixel coordinates
        let dest_rect = [position.0, position.1, size.0, size.1];

        let mut uniforms = LayerCompositeUniforms {
            source_rect,
            dest_rect,
            viewport_size: [vp_w, vp_h],
            opacity,
            blend_mode: blend_mode as u32,
            clip_bounds: [0.0, 0.0, vp_w, vp_h],
            clip_radius: [0.0, 0.0, 0.0, 0.0],
            clip_type: 0,
            perspective_d: 0.0,
            sin_rx: 0.0,
            cos_rx: 1.0,
            sin_ry: 0.0,
            cos_ry: 1.0,
            _pad: [0.0; 2],
        };

        if let Some((bounds, radii)) = clip {
            uniforms.clip_bounds = bounds;
            uniforms.clip_radius = radii;
            uniforms.clip_type = 1;
        }

        let uniform_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Region Blit Uniforms Buffer"),
                contents: bytemuck::cast_slice(&[uniforms]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });

        let bind_group = self.create_layer_composite_bind_group(
            &uniform_buffer,
            source,
            &self.path_image_sampler,
        );

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Region Blit Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Region Blit Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Set scissor rect to only affect the element's region
            render_pass.set_scissor_rect(
                position.0.max(0.0) as u32,
                position.1.max(0.0) as u32,
                size.0.min(vp_w - position.0).max(1.0) as u32,
                size.1.min(vp_h - position.1).max(1.0) as u32,
            );

            render_pass.set_pipeline(&self.pipelines.layer_composite);
            render_pass.set_bind_group(0, &bind_group, &[]);
            render_pass.draw(0..6, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // SDF 3D Viewport Rendering
    // ─────────────────────────────────────────────────────────────────────────────

    /// Initialize SDF 3D resources lazily
    fn ensure_sdf_3d_resources(&mut self) {
        if self.sdf_3d_resources.is_some() {
            return;
        }

        // Create bind group layout for SDF 3D uniforms
        let bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("SDF 3D Bind Group Layout"),
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    }],
                });

        // Create uniform buffer
        let uniform_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("SDF 3D Uniform Buffer"),
            size: std::mem::size_of::<Sdf3DUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Create bind group
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("SDF 3D Bind Group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        self.sdf_3d_resources = Some(Sdf3DResources {
            bind_group_layout,
            uniform_buffer,
            bind_group,
            pipeline_cache: HashMap::new(),
        });
    }

    /// Get or create a render pipeline for an SDF 3D viewport
    fn get_or_create_sdf_3d_pipeline(&mut self, shader_wgsl: &str) -> u64 {
        self.ensure_sdf_3d_resources();

        // Hash the shader for caching
        let shader_hash = Self::hash_string(shader_wgsl);

        let resources = self.sdf_3d_resources.as_mut().unwrap();

        if !resources.pipeline_cache.contains_key(&shader_hash) {
            // Create shader module
            let shader_module = self
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("SDF 3D Raymarch Shader"),
                    source: wgpu::ShaderSource::Wgsl(shader_wgsl.into()),
                });

            // Create pipeline layout
            let pipeline_layout =
                self.device
                    .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("SDF 3D Pipeline Layout"),
                        bind_group_layouts: &[&resources.bind_group_layout],
                        push_constant_ranges: &[],
                    });

            // Create render pipeline
            let pipeline = self
                .device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("SDF 3D Raymarch Pipeline"),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &shader_module,
                        entry_point: Some("vs_main"),
                        buffers: &[],
                        compilation_options: Default::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader_module,
                        entry_point: Some("fs_main"),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: self.texture_format,
                            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                        compilation_options: Default::default(),
                    }),
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        strip_index_format: None,
                        front_face: wgpu::FrontFace::Ccw,
                        cull_mode: None,
                        polygon_mode: wgpu::PolygonMode::Fill,
                        unclipped_depth: false,
                        conservative: false,
                    },
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview: None,
                    cache: None,
                });

            resources.pipeline_cache.insert(shader_hash, pipeline);
        }

        shader_hash
    }

    /// Simple string hash for shader caching
    fn hash_string(s: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut hasher);
        hasher.finish()
    }

    /// Render SDF 3D viewports to the target
    pub fn render_sdf_3d_viewports(
        &mut self,
        target: &wgpu::TextureView,
        viewports: &[Viewport3D],
    ) {
        if viewports.is_empty() {
            return;
        }

        self.ensure_sdf_3d_resources();

        let (surface_width, surface_height) = self.viewport_size;

        for viewport in viewports {
            // The paint context already clipped to its clip stack, but we need to
            // further clamp to the render target bounds for wgpu validity.
            // If we need to clamp further, we must also adjust the UV offset/scale.
            let orig_x = viewport.bounds[0];
            let orig_y = viewport.bounds[1];
            let orig_w = viewport.bounds[2];
            let orig_h = viewport.bounds[3];

            // Clamp to render target bounds
            let x = orig_x.max(0.0);
            let y = orig_y.max(0.0);
            let right = (orig_x + orig_w).min(surface_width as f32);
            let bottom = (orig_y + orig_h).min(surface_height as f32);
            let w = (right - x).max(0.0);
            let h = (bottom - y).max(0.0);

            // Skip if viewport is fully outside the render target or has zero size
            if w <= 0.0 || h <= 0.0 {
                continue;
            }

            // Check if we needed to clamp further and adjust UV accordingly
            let mut uniforms = viewport.uniforms;
            if orig_w > 0.0 && orig_h > 0.0 {
                // Calculate additional UV adjustment for surface clamping
                // The paint context's UV maps the paint-clipped region to the original viewport.
                // If we clamp further here, we need to adjust those UVs.
                let extra_offset_x = (x - orig_x) / orig_w;
                let extra_offset_y = (y - orig_y) / orig_h;
                let extra_scale_x = w / orig_w;
                let extra_scale_y = h / orig_h;

                // Compose with existing UV transform: new_uv = old_offset + (extra_offset + uv * extra_scale) * old_scale
                // Which simplifies to: new_offset = old_offset + extra_offset * old_scale, new_scale = old_scale * extra_scale
                uniforms.uv_offset[0] += extra_offset_x * uniforms.uv_scale[0];
                uniforms.uv_offset[1] += extra_offset_y * uniforms.uv_scale[1];
                uniforms.uv_scale[0] *= extra_scale_x;
                uniforms.uv_scale[1] *= extra_scale_y;
            }

            // Get or create pipeline for this viewport's shader
            let shader_hash = self.get_or_create_sdf_3d_pipeline(&viewport.shader_wgsl);

            // Update uniforms with adjusted UV
            let resources = self.sdf_3d_resources.as_ref().unwrap();
            self.queue
                .write_buffer(&resources.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

            // Create command encoder
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("SDF 3D Render Encoder"),
                });

            // Render pass
            {
                let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("SDF 3D Render Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            // Don't clear - we're rendering on top of existing content
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });

                // Set viewport and scissor to the clamped bounds
                render_pass.set_viewport(x, y, w, h, 0.0, 1.0);
                render_pass.set_scissor_rect(x as u32, y as u32, w as u32, h as u32);

                let resources = self.sdf_3d_resources.as_ref().unwrap();
                let pipeline = resources.pipeline_cache.get(&shader_hash).unwrap();
                render_pass.set_pipeline(pipeline);
                render_pass.set_bind_group(0, &resources.bind_group, &[]);
                render_pass.draw(0..3, 0..1); // Fullscreen triangle
            }

            // Submit
            self.queue.submit(std::iter::once(encoder.finish()));
        }
    }

    /// Render GPU particle viewports
    pub fn render_particle_viewports(
        &mut self,
        target: &wgpu::TextureView,
        viewports: &[crate::primitives::ParticleViewport3D],
    ) {
        use crate::particles::{ParticleSystemGpu, ParticleViewport};
        use std::hash::{Hash, Hasher};

        if viewports.is_empty() {
            return;
        }

        // Particles require compute shaders (for the simulation pass) and
        // storage buffers (for the particle buffer). WebGL2 has neither,
        // so skip particle rendering entirely in DT/Tier-3 mode.
        if !self.has_storage_buffers {
            return;
        }

        // Use the actual texture format that was selected during renderer initialization
        let surface_format = self.texture_format;

        for (vp_index, vp) in viewports.iter().enumerate() {
            if !vp.playing {
                continue;
            }

            // Generate a stable hash key for this particle system based on emitter config
            // This allows us to reuse the same GPU buffers across frames
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            vp_index.hash(&mut hasher);
            vp.max_particles.hash(&mut hasher);
            // Hash emitter position components to differentiate systems at different positions
            (vp.emitter.position_shape[0].to_bits()).hash(&mut hasher);
            (vp.emitter.position_shape[1].to_bits()).hash(&mut hasher);
            (vp.emitter.position_shape[2].to_bits()).hash(&mut hasher);
            let system_key = hasher.finish();

            // Get or create the particle system
            let system = self.particle_systems.entry(system_key).or_insert_with(|| {
                ParticleSystemGpu::new(&self.device, surface_format, vp.max_particles)
            });

            // Convert ParticleViewport3D to ParticleViewport for the GPU system
            let particle_viewport = ParticleViewport {
                emitter: vp.emitter,
                forces: vp.forces.clone(),
                max_particles: vp.max_particles,
                camera_pos: vp.camera_pos,
                camera_target: vp.camera_target,
                camera_up: vp.camera_up,
                fov: vp.fov,
                time: vp.time,
                delta_time: vp.delta_time,
                bounds: vp.bounds,
                blend_mode: vp.blend_mode,
                playing: vp.playing,
            };

            // Create command encoder
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Particle Encoder"),
                });

            // Run compute pass to update particles
            system.update(&self.queue, &mut encoder, &particle_viewport);

            // Submit compute work first
            self.queue.submit(std::iter::once(encoder.finish()));

            // Create render encoder
            let mut render_encoder =
                self.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Particle Render Encoder"),
                    });

            // Render pass
            {
                let mut render_pass =
                    render_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Particle Render Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: target,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load, // Don't clear, draw on top
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });

                // Set viewport to the particle bounds
                render_pass.set_viewport(
                    vp.bounds[0],
                    vp.bounds[1],
                    vp.bounds[2],
                    vp.bounds[3],
                    0.0,
                    1.0,
                );

                // Render the particles
                system.render(&self.queue, &mut render_pass, &particle_viewport);
            }

            // Submit render work
            self.queue.submit(std::iter::once(render_encoder.finish()));
        }
    }
}

impl Default for GpuRenderer {
    fn default() -> Self {
        // Create a basic renderer synchronously using pollster
        pollster::block_on(Self::new(RendererConfig::default()))
            .expect("Failed to create default renderer")
    }
}

/// Inverse of a column-major 4×4 matrix (GLU-style cofactor expansion).
fn mat4_inverse_flat(m: &[f32; 16]) -> [f32; 16] {
    let mut inv = [0.0f32; 16];
    inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15]
        + m[9] * m[7] * m[14]
        + m[13] * m[6] * m[11]
        - m[13] * m[7] * m[10];
    inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15]
        - m[8] * m[7] * m[14]
        - m[12] * m[6] * m[11]
        + m[12] * m[7] * m[10];
    inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15]
        + m[8] * m[7] * m[13]
        + m[12] * m[5] * m[11]
        - m[12] * m[7] * m[9];
    inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14]
        - m[8] * m[6] * m[13]
        - m[12] * m[5] * m[10]
        + m[12] * m[6] * m[9];
    inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15]
        - m[9] * m[3] * m[14]
        - m[13] * m[2] * m[11]
        + m[13] * m[3] * m[10];
    inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15]
        + m[8] * m[3] * m[14]
        + m[12] * m[2] * m[11]
        - m[12] * m[3] * m[10];
    inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15]
        - m[8] * m[3] * m[13]
        - m[12] * m[1] * m[11]
        + m[12] * m[3] * m[9];
    inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14]
        + m[8] * m[2] * m[13]
        + m[12] * m[1] * m[10]
        - m[12] * m[2] * m[9];
    inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15]
        + m[5] * m[3] * m[14]
        + m[13] * m[2] * m[7]
        - m[13] * m[3] * m[6];
    inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15]
        - m[4] * m[3] * m[14]
        - m[12] * m[2] * m[7]
        + m[12] * m[3] * m[6];
    inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15]
        + m[4] * m[3] * m[13]
        + m[12] * m[1] * m[7]
        - m[12] * m[3] * m[5];
    inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14]
        - m[4] * m[2] * m[13]
        - m[12] * m[1] * m[6]
        + m[12] * m[2] * m[5];
    inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11]
        - m[5] * m[3] * m[10]
        - m[9] * m[2] * m[7]
        + m[9] * m[3] * m[6];
    inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11]
        + m[4] * m[3] * m[10]
        + m[8] * m[2] * m[7]
        - m[8] * m[3] * m[6];
    inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11]
        - m[4] * m[3] * m[9]
        - m[8] * m[1] * m[7]
        + m[8] * m[3] * m[5];
    inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10]
        + m[4] * m[2] * m[9]
        + m[8] * m[1] * m[6]
        - m[8] * m[2] * m[5];
    let det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12];
    if det.abs() < 1e-12 {
        return [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
    }
    let id = 1.0 / det;
    for v in &mut inv {
        *v *= id;
    }
    inv
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─────────────────────────────────────────────────────────────────────────────
    // LayerTextureCache Tests
    // ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn layer_texture_cache_initial_state() {
        let cache = LayerTextureCache::new(wgpu::TextureFormat::Bgra8Unorm);
        assert_eq!(cache.pool_size(), 0);
        assert_eq!(cache.named_count(), 0);
    }

    #[test]
    fn layer_texture_cache_clear_all() {
        let cache = LayerTextureCache::new(wgpu::TextureFormat::Bgra8Unorm);
        // Pool is empty, but clear_all should work without panic
        let mut cache = cache;
        cache.clear_all();
        assert_eq!(cache.pool_size(), 0);
        assert_eq!(cache.named_count(), 0);
    }

    #[test]
    fn layer_texture_cache_format_preserved() {
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let cache = LayerTextureCache::new(format);
        assert_eq!(cache.format, format);
    }

    #[test]
    fn layer_texture_matches_size() {
        // Test requires GPU, but we can test the matches_size logic
        // by creating a helper struct with known sizes
        struct FakeTexture {
            size: (u32, u32),
        }
        impl FakeTexture {
            fn matches_size(&self, size: (u32, u32)) -> bool {
                self.size == size
            }
        }

        let tex = FakeTexture { size: (800, 600) };
        assert!(tex.matches_size((800, 600)));
        assert!(!tex.matches_size((800, 601)));
        assert!(!tex.matches_size((801, 600)));
        assert!(!tex.matches_size((400, 300)));
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // GPU Integration Tests (require actual wgpu device)
    // ─────────────────────────────────────────────────────────────────────────────

    /// Helper to create a test wgpu device
    async fn create_test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .ok()?;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .ok()?;

        Some((device, queue))
    }

    /// Helper to create unique layer IDs for testing
    fn test_layer_id(id: u64) -> blinc_core::LayerId {
        blinc_core::LayerId::new(id)
    }

    #[test]
    fn layer_texture_cache_acquire_and_release() {
        pollster::block_on(async {
            let Some((device, _queue)) = create_test_device().await else {
                // Skip test if no GPU available
                return;
            };

            let mut cache = LayerTextureCache::new(wgpu::TextureFormat::Bgra8Unorm);

            // Acquire a texture
            let tex1 = cache.acquire(&device, (512, 512), false);
            assert_eq!(tex1.size, (512, 512));
            assert!(!tex1.has_depth);

            // Release it back to pool
            cache.release(tex1);
            assert_eq!(cache.pool_size(), 1);

            // Acquire again - should reuse from pool
            let tex2 = cache.acquire(&device, (512, 512), false);
            assert_eq!(tex2.size, (512, 512));
            assert_eq!(cache.pool_size(), 0); // Removed from pool

            // Acquire different size in different bucket - should create new
            // Note: Using 256x256 (Medium bucket) since XLarge (>512) is not pooled
            let tex3 = cache.acquire(&device, (256, 256), false);
            assert_eq!(tex3.size, (256, 256));
            assert_eq!(cache.pool_size(), 0);

            // Release both - tex2 goes to Large bucket, tex3 goes to Medium bucket
            cache.release(tex2);
            cache.release(tex3);
            assert_eq!(cache.pool_size(), 2);
        });
    }

    #[test]
    fn layer_texture_cache_named_textures() {
        pollster::block_on(async {
            let Some((device, _queue)) = create_test_device().await else {
                return;
            };

            let mut cache = LayerTextureCache::new(wgpu::TextureFormat::Bgra8Unorm);
            let layer_id = test_layer_id(1);

            // Store a named texture
            let tex = cache.acquire(&device, (256, 256), false);
            cache.store(layer_id, tex);
            assert_eq!(cache.named_count(), 1);

            // Get reference to it
            let retrieved = cache.get(&layer_id);
            assert!(retrieved.is_some());
            assert_eq!(retrieved.unwrap().size, (256, 256));

            // Remove it
            let removed = cache.remove(&layer_id);
            assert!(removed.is_some());
            assert_eq!(cache.named_count(), 0);

            // Release back to pool
            cache.release(removed.unwrap());
            assert_eq!(cache.pool_size(), 1);
        });
    }

    #[test]
    fn layer_texture_cache_clear_named_releases_to_pool() {
        pollster::block_on(async {
            let Some((device, _queue)) = create_test_device().await else {
                return;
            };

            let mut cache = LayerTextureCache::new(wgpu::TextureFormat::Bgra8Unorm);

            // Store several named textures
            for i in 0..3 {
                let tex = cache.acquire(&device, (128, 128), false);
                cache.store(test_layer_id(i + 100), tex);
            }
            assert_eq!(cache.named_count(), 3);
            assert_eq!(cache.pool_size(), 0);

            // Clear named - should release to pool (capped at max_per_bucket=2)
            cache.clear_named();
            assert_eq!(cache.named_count(), 0);
            assert_eq!(cache.pool_size(), 2);
        });
    }

    #[test]
    fn layer_texture_cache_pool_size_limit() {
        pollster::block_on(async {
            let Some((device, _queue)) = create_test_device().await else {
                return;
            };

            let mut cache = LayerTextureCache::new(wgpu::TextureFormat::Bgra8Unorm);
            // Default max_per_bucket is 4 (bucketed by size: Small/Medium/Large)

            // Acquire and release more than max_per_bucket textures in Small bucket (64x64)
            let mut textures = Vec::new();
            for _ in 0..8 {
                textures.push(cache.acquire(&device, (64, 64), false));
            }

            // Release all
            for tex in textures {
                cache.release(tex);
            }

            // Pool should be capped at max_per_bucket (2) for the Small bucket
            assert_eq!(cache.pool_size(), 2);
        });
    }

    #[test]
    fn layer_texture_with_depth() {
        pollster::block_on(async {
            let Some((device, _queue)) = create_test_device().await else {
                return;
            };

            let mut cache = LayerTextureCache::new(wgpu::TextureFormat::Bgra8Unorm);

            // Acquire texture with depth
            let tex_with_depth = cache.acquire(&device, (512, 512), true);
            assert!(tex_with_depth.has_depth);
            assert!(tex_with_depth.depth_view.is_some());

            // Acquire texture without depth
            let tex_no_depth = cache.acquire(&device, (512, 512), false);
            assert!(!tex_no_depth.has_depth);
            assert!(tex_no_depth.depth_view.is_none());

            // Release both
            cache.release(tex_with_depth);
            cache.release(tex_no_depth);
            assert_eq!(cache.pool_size(), 2);

            // Acquire with depth - should NOT get the one without depth
            let tex_reacquire = cache.acquire(&device, (512, 512), true);
            assert!(tex_reacquire.has_depth);
            assert_eq!(cache.pool_size(), 1); // The no-depth one remains
        });
    }

    #[test]
    fn layer_texture_reuse_larger() {
        pollster::block_on(async {
            let Some((device, _queue)) = create_test_device().await else {
                return;
            };

            let mut cache = LayerTextureCache::new(wgpu::TextureFormat::Bgra8Unorm);

            // Acquire and release a Large bucket texture (512x512)
            // Note: XLarge (>512) is not pooled, so we use 512x512
            let large_tex = cache.acquire(&device, (512, 512), false);
            cache.release(large_tex);
            assert_eq!(cache.pool_size(), 1);

            // Acquire smaller from Medium bucket - should still reuse from Large bucket
            let small_tex = cache.acquire(&device, (256, 256), false);
            // The actual size will be 512x512 (reused from Large pool)
            assert!(small_tex.size.0 >= 256 && small_tex.size.1 >= 256);
            assert_eq!(cache.pool_size(), 0);
        });
    }
}

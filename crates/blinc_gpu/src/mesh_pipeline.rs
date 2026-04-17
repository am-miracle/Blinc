//! 3D mesh rendering pipeline.
//!
//! Holds every resource the mesh pass needs — render pipeline, bind
//! group layouts, default texture fallbacks, IBL cubemap, shadow map,
//! HDR intermediate target, tonemap + bloom passes, and the per-mesh
//! GPU caches (vertex / index buffers, morph deltas, decoded
//! textures). Lazily created on first mesh draw via
//! `GpuRenderer::ensure_mesh_pipeline`.
//!
//! Separated from `renderer.rs` to keep that file centred on
//! frame-level orchestration. The actual `impl GpuRenderer` method
//! blocks that drive this pipeline still live in `renderer.rs` for
//! now — this module holds the *types*. A follow-up move can relocate
//! those method blocks here once the structural split has proven
//! itself.

/// Maximum number of distinct meshes whose GPU buffers / textures we
/// keep warm between frames. When exceeded, the FIFO eviction policy
/// drops the oldest entry. Sized conservatively — 128 fits every
/// asset in the workspace examples, scales to large editor scenes,
/// and caps worst-case GPU memory at ~`128 × per-mesh-footprint`.
pub(crate) const MESH_CACHE_CAPACITY: usize = 128;

/// Upper bound on morph targets per mesh. CuteGirl G1 (current
/// stress-test asset) tops out at 152 on its face mesh; 256 covers
/// that plus headroom for rigs with denser blend-shape sets.
/// Storage cost: `256 × 4 bytes = 1 KB` per draw for the weights
/// buffer, negligible.
pub(crate) const MAX_MORPH_TARGETS: usize = 256;

/// FIFO-evicted cache size for per-mesh morph-delta storage
/// buffers. A face mesh with 152 targets × 5 K verts × 6 floats is
/// ~18 MB; at 32 entries cached, worst-case GPU footprint is
/// ~576 MB. Big but bounded — scenes with dozens of morph meshes
/// are rare.
pub(crate) const MORPH_CACHE_CAPACITY: usize = 32;

/// Per-mesh cached vertex + index GPU buffers.
pub(crate) struct MeshBufferCacheEntry {
    pub(crate) vertex: wgpu::Buffer,
    pub(crate) index: wgpu::Buffer,
    pub(crate) index_count: u32,
}

/// Lazily-created 3D mesh rendering pipeline.
pub(crate) struct MeshPipeline {
    pub(crate) pipeline: wgpu::RenderPipeline,
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) uniform_buffer: wgpu::Buffer,
    pub(crate) material_buffer: wgpu::Buffer,
    /// Default 1x1 white texture (used when material has no texture).
    pub(crate) default_texture: crate::image::GpuImage,
    /// Default flat normal map (128, 128, 255 = tangent-space up).
    pub(crate) default_normal_map: crate::image::GpuImage,
    /// Default black displacement map (no displacement).
    pub(crate) default_displacement: crate::image::GpuImage,
    /// Default 1x1 white metallic-roughness texture. Bound when the
    /// material has no MR texture; multiplying scalar metallic ×
    /// roughness by (1,1,1,1) produces the scalar-only path.
    pub(crate) default_metallic_roughness: crate::image::GpuImage,
    /// Default 1x1 white emissive texture. Bound when the material
    /// has no emissive texture; the shader gates on the
    /// `has_emissive_texture` flag so the default is only read for
    /// layout validation.
    pub(crate) default_emissive: crate::image::GpuImage,
    /// Default 1x1 white occlusion texture. Same rationale as the
    /// other defaults.
    pub(crate) default_occlusion: crate::image::GpuImage,
    pub(crate) sampler: wgpu::Sampler,
    /// Storage buffer for joint matrices (skeletal animation, max 256 joints).
    /// Used when `has_storage_buffers` is true.
    pub(crate) joint_buffer: wgpu::Buffer,
    /// Data-texture fallback for joint matrices (WebGL2 DT mode).
    /// Width=4 (one texel per mat4 row), height=num_joints, RGBA32Float.
    pub(crate) joint_data_texture: Option<wgpu::Texture>,
    pub(crate) joint_data_view: Option<wgpu::TextureView>,
    /// Per-draw morph-weights storage buffer. Holds up to
    /// [`MAX_MORPH_TARGETS`] floats — the mesh's current weights are
    /// copied in each draw.
    pub(crate) morph_weights_buffer: wgpu::Buffer,
    /// Dummy delta buffer (one zeroed vec4 pair) for meshes without
    /// morph targets. Bound so the bind-group layout is satisfied;
    /// `morph_target_count = 0` in the uniform guarantees the shader
    /// never reads it.
    pub(crate) morph_deltas_dummy: wgpu::Buffer,
    /// Per-mesh cache of uploaded morph-delta storage buffers, keyed
    /// by the `Arc<MeshData>` raw pointer. Deltas are static after
    /// glTF parse so the upload happens once per mesh, reused across
    /// frames. FIFO-evicted at [`MORPH_CACHE_CAPACITY`].
    pub(crate) morph_deltas_cache: std::collections::VecDeque<(usize, wgpu::Buffer)>,
    /// Depth buffer for the main mesh pass.
    pub(crate) main_depth: Option<wgpu::Texture>,
    pub(crate) main_depth_view: Option<wgpu::TextureView>,
    pub(crate) main_depth_size: (u32, u32),
    /// Shadow depth pass pipeline.
    pub(crate) shadow_pipeline: wgpu::RenderPipeline,
    pub(crate) shadow_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) shadow_uniform_buffer: wgpu::Buffer,
    /// 2048x2048 Depth32Float shadow map texture.
    pub(crate) shadow_map: wgpu::Texture,
    pub(crate) shadow_view: wgpu::TextureView,
    pub(crate) shadow_sampler: wgpu::Sampler,
    /// Procedural environment cubemap for IBL reflections.
    pub(crate) env_cubemap: wgpu::Texture,
    pub(crate) env_cubemap_view: wgpu::TextureView,
    pub(crate) env_sampler: wgpu::Sampler,
    /// Per-mesh vertex + index buffer cache. Keyed by the raw pointer
    /// of the mesh's `Arc<MeshData>`. FIFO-evicted at
    /// `MESH_CACHE_CAPACITY` to bound GPU memory for streaming scenes.
    pub(crate) cached_mesh_buffers: std::collections::HashMap<usize, MeshBufferCacheEntry>,
    pub(crate) cached_mesh_buffer_keys: std::collections::VecDeque<usize>,
    /// GPU texture cache, keyed by `TextureData::cache_key()`.
    /// Materials that reference the same underlying image share a
    /// single `GpuImage`.
    pub(crate) cached_gpu_images: std::collections::HashMap<usize, crate::image::GpuImage>,
    pub(crate) cached_gpu_image_keys: std::collections::VecDeque<usize>,
    /// Skybox pipeline — renders the environment cubemap as a
    /// background behind the mesh. Shares the cubemap texture/sampler
    /// but has its own bind group layout (camera vectors + cubemap).
    pub(crate) skybox_pipeline: wgpu::RenderPipeline,
    pub(crate) skybox_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) skybox_uniform_buffer: wgpu::Buffer,
    /// HDR intermediate texture (`Rgba16Float`). Meshes render here
    /// instead of the final framebuffer so specular + emissive above
    /// 1.0 accumulate without clipping; tonemap pass reads it.
    pub(crate) hdr_texture: Option<wgpu::Texture>,
    pub(crate) hdr_view: Option<wgpu::TextureView>,
    pub(crate) hdr_size: (u32, u32),
    /// Fullscreen ACES tonemap pipeline + resources.
    pub(crate) tonemap_pipeline: wgpu::RenderPipeline,
    pub(crate) tonemap_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) tonemap_sampler: wgpu::Sampler,
    /// Bloom pipeline — shared for threshold-downsample and Kawase blur.
    pub(crate) bloom_pipeline: wgpu::RenderPipeline,
    pub(crate) bloom_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) bloom_uniform_buffer: wgpu::Buffer,
    /// Two half-res Rgba16Float ping-pong textures for bloom blur.
    pub(crate) bloom_a: Option<wgpu::Texture>,
    pub(crate) bloom_a_view: Option<wgpu::TextureView>,
    pub(crate) bloom_b: Option<wgpu::Texture>,
    pub(crate) bloom_b_view: Option<wgpu::TextureView>,
    pub(crate) bloom_size: (u32, u32),
}

//! Render context for blinc_app
//!
//! Wraps the GPU rendering pipeline with a clean API.

use blinc_core::{
    Brush, Color, CornerRadius, DrawCommand, DrawContext, DrawContextExt, Rect, Stroke,
};
use blinc_gpu::{
    FontRegistry, GenericFont as GpuGenericFont, GpuGlyph, GpuImage, GpuImageInstance,
    GpuPaintContext, GpuPrimitive, GpuRenderer, ImageRenderingContext, PendingMesh, PrimitiveBatch,
    TextAlignment, TextAnchor, TextRenderingContext,
};
use blinc_layout::div::{FontFamily, FontWeight, GenericFont, TextAlign, TextVerticalAlign};
use blinc_layout::prelude::*;
use blinc_layout::render_state::Overlay;
use blinc_layout::renderer::ElementType;
use blinc_svg::{RasterizedSvg, SvgDocument};
use lru::LruCache;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use crate::error::Result;
use crate::svg_atlas::SvgAtlas;

/// Maximum number of images to keep in cache (prevents unbounded memory growth).
///
/// Sized to comfortably hold the simultaneously-visible image set of typical
/// content-heavy views (galleries, emoji grids, chat backlogs). Going below
/// the visible-set size causes scroll-driven thrashing where currently-visible
/// images are evicted to make room for newly-loaded ones.
const IMAGE_CACHE_CAPACITY: usize = 256;

/// Maximum number of parsed SVG documents to cache
const SVG_CACHE_CAPACITY: usize = 128;

/// Decide whether an image entering the cache should be BC-compressed.
///
/// BC1/BC3's 4×4 block quantization is designed for large
/// photographic content where artifacts hide in detail. It's a
/// visibly bad fit for:
///
/// - **Emoji sprites** (`emoji://...`) — tiny color glyphs with
///   smooth gradients, sharp edges, and feathered alpha. Blocking
///   artifacts produce obvious banding and ringing.
/// - **Small icons / logos** — same failure mode at smaller
///   scale. Anywhere a designer cared about crispness is somewhere
///   BC will look wrong.
///
/// Gate compression behind a minimum dimension floor of 256 px
/// and a hard skip for the `emoji://` scheme. Large photographic
/// content (product shots, avatars at reasonable size, wallpaper)
/// still goes through BC and keeps the VRAM win; small UI sprites
/// stay lossless.
fn bc_eligible(source_uri: &str, width: u32, height: u32) -> bool {
    const MIN_DIM: u32 = 256;
    if source_uri.starts_with("emoji://") {
        return false;
    }
    // BC blocks are 4×4 — wgpu's `Device::create_texture` rejects
    // any texture whose dimensions aren't multiples of 4 for the
    // BC formats. Real photos (camera captures, album art, stock
    // images) frequently land on odd dimensions like 1920×1201.
    // Padding the source buffer + adjusting UVs would reclaim
    // these, but the 2D image pipeline samples `[0,1]` across the
    // texture with no slop — cheapest correct fix is to fall
    // through to Rgba8 when the image isn't block-aligned.
    if width % 4 != 0 || height % 4 != 0 {
        return false;
    }
    width >= MIN_DIM && height >= MIN_DIM
}

/// Intersect two axis-aligned clip rects [x, y, w, h], returning their overlap.
fn intersect_clip_rects(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let x1 = a[0].max(b[0]);
    let y1 = a[1].max(b[1]);
    let x2 = (a[0] + a[2]).min(b[0] + b[2]);
    let y2 = (a[1] + a[3]).min(b[1] + b[3]);
    [x1, y1, (x2 - x1).max(0.0), (y2 - y1).max(0.0)]
}

/// Merge a new clip rect with an optional existing one via intersection.
fn merge_scroll_clip(new_clip: [f32; 4], existing: Option<[f32; 4]>) -> Option<[f32; 4]> {
    match existing {
        Some(ex) => Some(intersect_clip_rects(new_clip, ex)),
        None => Some(new_clip),
    }
}

/// Compute effective clip for elements that support only a single clip rect (text, SVG).
/// Intersects primary clip and scroll clip so nested scroll containers are respected.
fn effective_single_clip(primary: Option<[f32; 4]>, scroll: Option<[f32; 4]>) -> Option<[f32; 4]> {
    match (primary, scroll) {
        (Some(c), Some(s)) => Some(intersect_clip_rects(c, s)),
        (c, s) => c.or(s),
    }
}

// Rasterized SVG textures are now packed into SvgAtlas (single shared GPU texture)

/// Internal render context that manages GPU resources and rendering
pub struct RenderContext {
    renderer: GpuRenderer,
    pub(crate) text_ctx: TextRenderingContext,
    image_ctx: ImageRenderingContext,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    sample_count: u32,
    // Single texture for glass backdrop (rendered to and sampled from)
    backdrop_texture: Option<CachedTexture>,
    // Cached MSAA texture for anti-aliased rendering
    msaa_texture: Option<CachedTexture>,
    // LRU cache for images (prevents unbounded memory growth)
    image_cache: LruCache<String, GpuImage>,
    // Tracks when each image first appeared in the cache (for fade-in animation)
    image_load_times: std::collections::HashMap<String, web_time::Instant>,
    // LRU cache for parsed SVG documents (avoids re-parsing)
    svg_cache: LruCache<u64, SvgDocument>,
    // Texture atlas for rasterized SVGs (single shared GPU texture, shelf-packed)
    svg_atlas: SvgAtlas,
    // Scratch buffers for per-frame allocations (reused to avoid allocations)
    scratch_glyphs: Vec<GpuGlyph>,
    scratch_texts: Vec<TextElement>,
    scratch_svgs: Vec<SvgElement>,
    scratch_images: Vec<ImageElement>,
    // Current cursor position in physical pixels (for @flow pointer input)
    cursor_pos: [f32; 2],
    // Whether the last render contained @flow shader elements (triggers continuous redraw)
    has_active_flows: bool,
    // Frame counter for periodic cache stats logging
    frame_count: u64,
    // Alpha value used when clearing the main render target. 1.0 for
    // opaque windows (the default); 0.0 when the window surface is
    // configured for transparent composition. Set via
    // [`Self::set_clear_alpha`] before each window's render — the
    // desktop runner updates it per-window so a mix of opaque and
    // transparent windows can share the same RenderContext.
    clear_alpha: f32,
}

struct CachedTexture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

/// Info about a 3D-transformed ancestor layer. When text/SVGs/images are inside a parent
/// with `perspective` + `rotate-x`/`rotate-y`, this info is used to render them to an
/// offscreen texture and blit with the same perspective transform.
#[derive(Clone, Debug)]
struct Transform3DLayerInfo {
    /// Node ID of the 3D-transformed ancestor (used as layer grouping key)
    node_id: LayoutNodeId,
    /// Screen-space bounds of the 3D layer [x, y, w, h] (DPI-scaled)
    layer_bounds: [f32; 4],
    /// Perspective transform parameters
    transform_3d: blinc_core::Transform3DParams,
    /// Layer opacity
    opacity: f32,
}

/// Text element data for rendering
#[derive(Clone)]
struct TextElement {
    content: String,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    font_size: f32,
    color: [f32; 4],
    align: TextAlign,
    weight: FontWeight,
    /// Whether to use italic style
    italic: bool,
    /// Vertical alignment within bounding box
    v_align: TextVerticalAlign,
    /// Clip bounds from parent scroll container (x, y, width, height)
    clip_bounds: Option<[f32; 4]>,
    /// Motion opacity inherited from parent motion container
    motion_opacity: f32,
    /// Whether to wrap text at container bounds
    wrap: bool,
    /// Line height multiplier
    line_height: f32,
    /// Measured width (before layout constraints) - used to determine if wrap is needed
    measured_width: f32,
    /// Font family category
    font_family: FontFamily,
    /// Word spacing in pixels (0.0 = normal)
    word_spacing: f32,
    /// Letter spacing in pixels (0.0 = normal)
    letter_spacing: f32,
    /// Z-index for rendering order (higher = on top)
    z_index: u32,
    /// Font ascender in pixels (distance from baseline to top)
    ascender: f32,
    /// Whether text has strikethrough decoration
    strikethrough: bool,
    /// Whether text has underline decoration
    underline: bool,
    /// CSS text-decoration-color override (RGBA)
    decoration_color: Option<[f32; 4]>,
    /// CSS text-decoration-thickness override in pixels
    decoration_thickness: Option<f32>,
    /// Inherited CSS transform from ancestor elements (full 6-element affine in layout coords)
    /// [a, b, c, d, tx, ty] where new_x = a*x + c*y + tx, new_y = b*x + d*y + ty
    css_affine: Option<[f32; 6]>,
    /// Text shadow (offset_x, offset_y, blur, color) from CSS text-shadow property
    text_shadow: Option<blinc_core::Shadow>,
    /// 3D layer info if this text is inside a perspective-transformed parent
    transform_3d_layer: Option<Transform3DLayerInfo>,
    /// Whether this text is inside a foreground-layer element (rendered after foreground primitives)
    is_foreground: bool,
}

/// Image element data for rendering
#[derive(Clone)]
struct ImageElement {
    source: String,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    object_fit: u8,
    object_position: [f32; 2],
    opacity: f32,
    border_radius: f32,
    tint: [f32; 4],
    /// Clip bounds from parent (x, y, width, height)
    clip_bounds: Option<[f32; 4]>,
    /// Clip corner radii (tl, tr, br, bl)
    clip_radius: [f32; 4],
    /// Which layer this image renders in
    layer: RenderLayer,
    /// Loading strategy: 0 = Eager (load immediately), 1 = Lazy (load when visible)
    loading_strategy: u8,
    /// Placeholder type: 0 = None, 1 = Color, 2 = Image, 3 = Skeleton
    placeholder_type: u8,
    /// Placeholder color [r, g, b, a]
    placeholder_color: [f32; 4],
    /// Placeholder image source (only used when placeholder_type == 2)
    placeholder_image: Option<String>,
    /// Fade-in duration in milliseconds (0 = no fade)
    fade_duration_ms: u32,
    /// Z-layer index for interleaved rendering with primitives
    z_index: u32,
    /// Border width (0 = no border)
    border_width: f32,
    /// Border color
    border_color: blinc_core::Color,
    /// CSS transform as 6-element affine [a, b, c, d, tx, ty] (None = no transform)
    css_affine: Option<[f32; 6]>,
    /// Drop shadow from CSS
    shadow: Option<blinc_core::Shadow>,
    /// CSS filter A (grayscale, invert, sepia, hue_rotate_rad) — identity = [0,0,0,0]
    filter_a: [f32; 4],
    /// CSS filter B (brightness, contrast, saturate, unused) — identity = [1,1,1,0]
    filter_b: [f32; 4],
    /// Secondary clip (scroll container boundary) — sharp rect, no radius.
    /// Kept separate from primary clip_bounds so rounded corners don't morph
    /// when the primary clip rect shrinks at scroll boundaries.
    scroll_clip: Option<[f32; 4]>,
    /// Mask gradient params: linear=(x1,y1,x2,y2), radial=(cx,cy,r,0) in OBB space
    mask_params: [f32; 4],
    /// Mask info: [mask_type, start_alpha, end_alpha, 0] (0=none, 1=linear, 2=radial)
    mask_info: [f32; 4],
    /// 3D layer info if this image is inside a perspective-transformed parent
    transform_3d_layer: Option<Transform3DLayerInfo>,
}

/// SVG element data for rendering
#[derive(Clone)]
struct SvgElement {
    source: Arc<str>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    /// Tint color to apply to SVG fill/stroke (from CSS `color`)
    tint: Option<blinc_core::Color>,
    /// CSS `fill` override for SVG
    fill: Option<blinc_core::Color>,
    /// CSS `stroke` override for SVG
    stroke: Option<blinc_core::Color>,
    /// CSS `stroke-width` override for SVG
    stroke_width: Option<f32>,
    /// CSS `stroke-dasharray` pattern for SVG
    stroke_dasharray: Option<Vec<f32>>,
    /// CSS `stroke-dashoffset` for SVG
    stroke_dashoffset: Option<f32>,
    /// SVG path `d` attribute data (for path morphing)
    svg_path_data: Option<String>,
    /// Clip bounds from parent scroll container (x, y, width, height)
    clip_bounds: Option<[f32; 4]>,
    /// Motion opacity inherited from parent motion container
    motion_opacity: f32,
    /// Inherited CSS transform from ancestor elements (full 6-element affine in layout coords)
    /// [a, b, c, d, tx, ty] where new_x = a*x + c*y + tx, new_y = b*x + d*y + ty
    css_affine: Option<[f32; 6]>,
    /// Per-SVG-tag style overrides from CSS tag-name selectors (e.g., `path { fill: red; }`)
    tag_overrides: std::collections::HashMap<String, blinc_layout::element::SvgTagStyle>,
    /// 3D layer info if this SVG is inside a perspective-transformed parent
    transform_3d_layer: Option<Transform3DLayerInfo>,
}

/// Flow shader element — an element with `flow: <name>` that renders via a custom GPU pipeline
#[derive(Clone)]
struct FlowElement {
    /// Name referencing a @flow DAG in the stylesheet
    flow_name: String,
    /// Direct FlowGraph (from `flow!` macro), bypasses stylesheet lookup
    flow_graph: Option<std::sync::Arc<blinc_core::FlowGraph>>,
    /// Bounds in physical pixels (DPI-scaled)
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    /// Z-layer for rendering order
    z_index: u32,
    /// Corner radius in physical pixels
    corner_radius: f32,
}

/// Debug bounds element for layout visualization
#[derive(Clone)]
struct DebugBoundsElement {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    /// Element type name for labeling
    element_type: String,
    /// Depth in the tree (for color coding)
    depth: u32,
}

impl RenderContext {
    /// Create a new render context
    pub(crate) fn new(
        renderer: GpuRenderer,
        text_ctx: TextRenderingContext,
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        sample_count: u32,
    ) -> Self {
        let image_ctx = ImageRenderingContext::new(device.clone(), queue.clone());
        let svg_atlas = SvgAtlas::new(&device);
        Self {
            renderer,
            text_ctx,
            image_ctx,
            device,
            queue,
            sample_count,
            backdrop_texture: None,
            msaa_texture: None,
            image_cache: LruCache::new(NonZeroUsize::new(IMAGE_CACHE_CAPACITY).unwrap()),
            image_load_times: std::collections::HashMap::new(),
            svg_cache: LruCache::new(NonZeroUsize::new(SVG_CACHE_CAPACITY).unwrap()),
            svg_atlas,
            scratch_glyphs: Vec::with_capacity(1024), // Pre-allocate for typical text
            scratch_texts: Vec::with_capacity(64),    // Pre-allocate for text elements
            scratch_svgs: Vec::with_capacity(32),     // Pre-allocate for SVG elements
            scratch_images: Vec::with_capacity(32),   // Pre-allocate for image elements
            cursor_pos: [0.0; 2],
            has_active_flows: false,
            frame_count: 0,
            clear_alpha: 1.0,
        }
    }

    /// Set the alpha component used when clearing the main render target.
    ///
    /// 1.0 (default) gives opaque clears — correct for regular windows
    /// and any surface configured with `CompositeAlphaMode::Opaque`.
    /// 0.0 is used for windows whose surface was created with a
    /// premultiplied/postmultiplied alpha mode so the OS compositor
    /// can see through to whatever is behind the window.
    pub fn set_clear_alpha(&mut self, alpha: f32) {
        self.clear_alpha = alpha;
    }

    /// Update the current cursor position in physical pixels (for @flow pointer input)
    /// Register a custom render pass with the GPU renderer.
    ///
    /// Scene3D-stage passes run inside the mesh HDR pipeline with
    /// camera context (view_proj, inv_view_proj, camera_pos) populated
    /// on `RenderPassContext`. PreRender/PostProcess stages run at
    /// their existing points in the frame.
    pub fn register_custom_pass(
        &mut self,
        pass: Box<dyn blinc_gpu::custom_pass::CustomRenderPass>,
    ) {
        self.renderer.register_custom_pass(pass);
    }

    pub fn set_cursor_position(&mut self, x: f32, y: f32) {
        self.cursor_pos = [x, y];
    }

    /// Whether the last render frame contained @flow shader elements.
    /// Used to trigger continuous redraws for animated flow shaders.
    pub fn has_active_flows(&self) -> bool {
        self.has_active_flows
    }

    /// Set the current render target texture for blend mode two-pass compositing.
    /// Must be called before rendering when the batch may use non-Normal blend modes.
    pub fn set_blend_target(&mut self, texture: &wgpu::Texture) {
        self.renderer.set_blend_target(texture);
    }

    /// Clear the blend target texture reference after rendering.
    pub fn clear_blend_target(&mut self) {
        self.renderer.clear_blend_target();
    }

    /// Load font data into the text rendering registry
    ///
    /// This adds fonts that will be available for text rendering.
    /// Returns the number of font faces loaded.
    pub fn load_font_data_to_registry(&mut self, data: Vec<u8>) -> usize {
        self.text_ctx.load_font_data_to_registry(data)
    }

    /// Render a layout tree to a texture view
    ///
    /// Handles everything automatically - glass, text, SVG, MSAA.
    pub fn render_tree(
        &mut self,
        tree: &RenderTree,
        width: u32,
        height: u32,
        target: &wgpu::TextureView,
    ) -> Result<()> {
        // Get scale factor for HiDPI rendering
        let scale_factor = tree.scale_factor();

        // Create paint contexts for each layer with text rendering support
        let mut bg_ctx =
            GpuPaintContext::with_text_context(width as f32, height as f32, &mut self.text_ctx);

        // Render layout layers (background and glass go to bg_ctx)
        tree.render_to_layer(&mut bg_ctx, RenderLayer::Background);
        tree.render_to_layer(&mut bg_ctx, RenderLayer::Glass);

        // Take the batch from bg_ctx before we can reuse text_ctx for fg_ctx
        let mut bg_batch = bg_ctx.take_batch();

        // Create foreground context with text rendering support
        let mut fg_ctx =
            GpuPaintContext::with_text_context(width as f32, height as f32, &mut self.text_ctx);
        tree.render_to_layer(&mut fg_ctx, RenderLayer::Foreground);

        // Take the batch from fg_ctx before reusing text_ctx for text elements
        let mut fg_batch = fg_ctx.take_batch();

        // Collect text, SVG, image, and flow elements
        let (texts, svgs, images, _flows) = self.collect_render_elements(tree);

        // Pre-load all images into cache before rendering
        self.preload_images(&images, width as f32, height as f32);

        // Prepare text glyphs
        let mut all_glyphs = Vec::new();
        let mut css_transformed_text_prims: Vec<GpuPrimitive> = Vec::new();
        for text in &texts {
            // Convert layout TextAlign to GPU TextAlignment
            let alignment = match text.align {
                TextAlign::Left => TextAlignment::Left,
                TextAlign::Center => TextAlignment::Center,
                TextAlign::Right => TextAlignment::Right,
            };

            // Vertical alignment:
            // - Center: Use TextAnchor::Center with y at vertical center of bounds.
            //   This ensures text appears visually centered (by cap-height) rather than
            //   mathematically centered by the full bounding box (which includes descenders).
            // - Top: Text is centered within its layout box (items_center works).
            // - Baseline: Position text so baseline aligns at the font's actual baseline.
            //   Using the actual ascender from font metrics ensures all fonts align by
            //   their true baseline regardless of font family.
            let (anchor, y_pos, use_layout_height) = match text.v_align {
                TextVerticalAlign::Center => {
                    (TextAnchor::Center, text.y + text.height / 2.0, false)
                }
                TextVerticalAlign::Top => (TextAnchor::Top, text.y, true),
                TextVerticalAlign::Baseline => {
                    // Use the actual font ascender for baseline positioning.
                    // This ensures each font aligns by its true baseline.
                    let baseline_y = text.y + text.ascender;
                    (TextAnchor::Baseline, baseline_y, false)
                }
            };

            // Determine wrap width: use clip bounds if available (parent constraint),
            // otherwise use the text element's own layout width
            let wrap_width = if text.wrap {
                if let Some(clip) = text.clip_bounds {
                    // clip[2] is the clip width - use it if smaller than text width
                    clip[2].min(text.width)
                } else {
                    text.width
                }
            } else {
                text.width
            };

            // Convert font family to GPU types
            let font_name = text.font_family.name.as_deref();
            let generic = to_gpu_generic_font(text.font_family.generic);
            let font_weight = text.weight.weight();

            // Only pass layout_height when we want centering within the box
            let layout_height = if use_layout_height {
                Some(text.height)
            } else {
                None
            };

            match self.text_ctx.prepare_text_with_style(
                &text.content,
                text.x,
                y_pos,
                text.font_size,
                text.color,
                anchor,
                alignment,
                Some(wrap_width),
                text.wrap,
                font_name,
                generic,
                font_weight,
                text.italic,
                layout_height,
                text.letter_spacing,
            ) {
                Ok(mut glyphs) => {
                    tracing::trace!(
                        "Prepared {} glyphs for text '{}' (font={:?}, generic={:?})",
                        glyphs.len(),
                        text.content,
                        font_name,
                        generic
                    );
                    // Apply clip bounds to all glyphs if the text element has clip bounds
                    if let Some(clip) = text.clip_bounds {
                        for glyph in &mut glyphs {
                            glyph.clip_bounds = clip;
                        }
                    }

                    if let Some(affine) = text.css_affine {
                        // CSS-transformed text: convert to SDF primitives with local_affine
                        let [a, b, c, d, tx, ty] = affine;
                        let tx_scaled = tx * scale_factor;
                        let ty_scaled = ty * scale_factor;
                        for glyph in &glyphs {
                            let gc_x = glyph.bounds[0] + glyph.bounds[2] / 2.0;
                            let gc_y = glyph.bounds[1] + glyph.bounds[3] / 2.0;
                            let new_gc_x = a * gc_x + c * gc_y + tx_scaled;
                            let new_gc_y = b * gc_x + d * gc_y + ty_scaled;
                            let mut prim = GpuPrimitive::from_glyph(glyph);
                            prim.bounds = [
                                new_gc_x - glyph.bounds[2] / 2.0,
                                new_gc_y - glyph.bounds[3] / 2.0,
                                glyph.bounds[2],
                                glyph.bounds[3],
                            ];
                            prim.local_affine = [a, b, c, d];
                            css_transformed_text_prims.push(prim);
                        }
                    } else {
                        all_glyphs.extend(glyphs);
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to prepare text '{}': {:?}", text.content, e);
                }
            }
        }

        tracing::trace!(
            "Text rendering: {} texts collected, {} total glyphs prepared",
            texts.len(),
            all_glyphs.len()
        );

        // SVGs are rendered as rasterized images (not tessellated paths) for better anti-aliasing
        // They will be rendered later via render_rasterized_svgs

        self.renderer.resize(width, height);

        // Bind the real glyph atlas to the SDF pipeline whenever
        // it's available — `set_glyph_atlas` no-ops on pointer
        // equality so calling each frame is free. CSS-transformed
        // text + canvas `draw_text` calls both land glyph-sourced
        // primitives in `bg_batch.primitives`, and the SDF pipeline
        // needs the real atlas bound to sample them; the old
        // "only when CSS text exists" guard silently swallowed
        // canvas text that reached this path any other way.
        if let (Some(atlas), Some(color_atlas)) =
            (self.text_ctx.atlas_view(), self.text_ctx.color_atlas_view())
        {
            if !css_transformed_text_prims.is_empty() {
                bg_batch.primitives.append(&mut css_transformed_text_prims);
            }
            self.renderer.set_glyph_atlas(atlas, color_atlas);
        }

        let has_glass = bg_batch.glass_count() > 0;

        // Only allocate glass textures when glass is actually used
        if has_glass {
            self.ensure_glass_textures(width, height);
        }
        let use_msaa_overlay = self.sample_count > 1;

        // Background layer uses SDF rendering (shader-based AA, no MSAA needed)
        // Foreground layer (SVGs as tessellated paths) uses MSAA for smooth edges

        if has_glass {
            // Split images by layer: background images go behind glass (get blurred),
            // glass/foreground images render on top of glass (not blurred)
            let (bg_images, fg_images): (Vec<_>, Vec<_>) = images
                .iter()
                .partition(|img| img.layer == RenderLayer::Background);

            // Pre-render background images to both backdrop and target so glass can blur them
            let has_bg_images = !bg_images.is_empty();
            if has_bg_images {
                // Take backdrop temporarily to avoid borrow conflict with render_images_ref(&mut self)
                let backdrop_tex = self.backdrop_texture.take().unwrap();
                self.renderer
                    .clear_target(&backdrop_tex.view, wgpu::Color::TRANSPARENT);
                self.renderer.clear_target(
                    target,
                    wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: self.clear_alpha as f64,
                    },
                );
                self.render_images_ref(&backdrop_tex.view, &bg_images);
                self.render_images_ref(target, &bg_images);
                self.backdrop_texture = Some(backdrop_tex);
            }

            // Glass path - batched rendering for reduced command buffer overhead:
            // Steps 1-3 are batched into a single encoder submission
            {
                let backdrop = self.backdrop_texture.as_ref().unwrap();
                self.renderer.render_glass_frame(
                    target,
                    &backdrop.view,
                    (backdrop.width, backdrop.height),
                    &bg_batch,
                    has_bg_images,
                );
            }

            // Render background paths with MSAA for smooth edges on curved shapes like notch
            // (render_glass_frame uses 1x sampled path rendering, so we need MSAA overlay)
            if use_msaa_overlay && bg_batch.has_paths() {
                self.renderer
                    .render_paths_overlay_msaa(target, &bg_batch, self.sample_count);
            }

            // Render remaining bg images to target (only if not already pre-rendered)
            if !has_bg_images {
                self.render_images_ref(target, &bg_images);
            }

            // Step 5: Render glass/foreground-layer images (on top of glass, NOT blurred)
            self.render_images_ref(target, &fg_images);

            // Step 5b: Render dynamic RGBA images (video frames, camera preview)
            if !bg_batch.dynamic_images.is_empty() {
                self.renderer
                    .render_dynamic_images(target, &bg_batch.dynamic_images);
            }
            if !fg_batch.dynamic_images.is_empty() {
                self.renderer
                    .render_dynamic_images(target, &fg_batch.dynamic_images);
            }

            // Step 6: Render foreground and text
            // Use batch-based rendering when layer effects are present
            let has_layer_effects = fg_batch.has_layer_effects();
            if has_layer_effects {
                // Layer effects require batch-based rendering to process layer commands
                fg_batch.convert_glyphs_to_primitives();
                if !fg_batch.is_empty() {
                    // Pre-load any mask images referenced by layer effects
                    self.preload_mask_images(&fg_batch);
                    self.renderer.render_overlay(target, &fg_batch);
                }
                // Render SVGs as rasterized images for high-quality anti-aliasing
                if !svgs.is_empty() {
                    self.render_rasterized_svgs(target, &svgs, scale_factor);
                }
            } else if self.renderer.unified_text_rendering() {
                // Unified rendering: combine text glyphs with foreground primitives.
                // See the simple-path branch below for the rationale on
                // extending `unified_primitives` with the local
                // `all_glyphs` — `get_unified_foreground_primitives()`
                // reads from `fg_batch.glyphs`, which is empty here.
                let mut unified_primitives = fg_batch.get_unified_foreground_primitives();
                for glyph in &all_glyphs {
                    unified_primitives.push(GpuPrimitive::from_glyph(glyph));
                }
                if !unified_primitives.is_empty() {
                    self.render_unified(target, &unified_primitives);
                }

                // Render paths with MSAA for smooth edges (paths are not included in unified primitives)
                if use_msaa_overlay && fg_batch.has_paths() {
                    self.renderer
                        .render_paths_overlay_msaa(target, &fg_batch, self.sample_count);
                }

                // Render SVGs as rasterized images for high-quality anti-aliasing
                if !svgs.is_empty() {
                    self.render_rasterized_svgs(target, &svgs, scale_factor);
                }
            } else {
                // Legacy rendering: separate foreground and text passes
                if !fg_batch.is_empty() {
                    if use_msaa_overlay {
                        self.renderer
                            .render_overlay_msaa(target, &fg_batch, self.sample_count);
                    } else {
                        self.renderer.render_overlay(target, &fg_batch);
                    }
                }

                // Step 7: Render text
                if !all_glyphs.is_empty() {
                    self.render_text(target, &all_glyphs);
                }

                // Render SVGs as rasterized images for high-quality anti-aliasing
                if !svgs.is_empty() {
                    self.render_rasterized_svgs(target, &svgs, scale_factor);
                }
            }

            // Step 8: Render text decorations (strikethrough, underline)
            let decorations_by_layer = generate_text_decoration_primitives_by_layer(&texts);
            for primitives in decorations_by_layer.values() {
                if !primitives.is_empty() {
                    self.render_unified(target, primitives);
                }
            }
        } else {
            // Simple path (no glass):
            // Background uses SDF rendering (no MSAA needed)
            // Foreground uses MSAA for smooth SVG edges

            // Render background directly to target. Alpha comes from
            // `clear_alpha` so transparent windows get a fully clear
            // surface (0.0) while opaque windows keep the historical
            // opaque black (1.0) clear.
            self.renderer.render_with_clear(
                target,
                &bg_batch,
                [0.0, 0.0, 0.0, self.clear_alpha as f64],
            );

            // Render background paths with MSAA for smooth edges on curved shapes like notch
            if use_msaa_overlay && bg_batch.has_paths() {
                self.renderer
                    .render_paths_overlay_msaa(target, &bg_batch, self.sample_count);
            }

            // Render images after background primitives
            self.render_images(target, &images, width as f32, height as f32, scale_factor);

            // Render foreground and text
            // Use batch-based rendering when layer effects are present to preserve
            // layer commands for effect processing
            let has_layer_effects = fg_batch.has_layer_effects();
            if has_layer_effects {
                // Layer effects require batch-based rendering to process layer commands
                // First convert glyphs to primitives so they're included in the batch
                fg_batch.convert_glyphs_to_primitives();

                // Use render_overlay which supports layer effect processing
                if !fg_batch.is_empty() {
                    self.renderer.render_overlay(target, &fg_batch);
                }
                // Render SVGs as rasterized images for high-quality anti-aliasing
                if !svgs.is_empty() {
                    self.render_rasterized_svgs(target, &svgs, scale_factor);
                }
            } else if self.renderer.unified_text_rendering() {
                // Unified rendering: combine text glyphs with foreground primitives
                // This ensures text and shapes transform together during animations.
                //
                // `get_unified_foreground_primitives()` reads from
                // `fg_batch.glyphs`, which is empty here — the glyph
                // preparation loop above writes into the local
                // `all_glyphs` vec, not into the batch. We have to
                // extend the unified primitive list with our local
                // glyphs ourselves, otherwise the unified path silently
                // drops every text element. (The `render_tree_with_motion`
                // variant doesn't hit this because it pushes glyphs
                // through a different intermediate buffer.)
                let mut unified_primitives = fg_batch.get_unified_foreground_primitives();
                for glyph in &all_glyphs {
                    unified_primitives.push(GpuPrimitive::from_glyph(glyph));
                }
                if !unified_primitives.is_empty() {
                    self.render_unified(target, &unified_primitives);
                }

                // Render paths with MSAA for smooth edges (paths are not included in unified primitives)
                if use_msaa_overlay && fg_batch.has_paths() {
                    self.renderer
                        .render_paths_overlay_msaa(target, &fg_batch, self.sample_count);
                }

                // Render SVGs as rasterized images for high-quality anti-aliasing
                if !svgs.is_empty() {
                    self.render_rasterized_svgs(target, &svgs, scale_factor);
                }
            } else {
                // Legacy rendering: separate foreground and text passes
                if !fg_batch.is_empty() {
                    if use_msaa_overlay {
                        self.renderer
                            .render_overlay_msaa(target, &fg_batch, self.sample_count);
                    } else {
                        self.renderer.render_overlay(target, &fg_batch);
                    }
                }

                // Render text
                if !all_glyphs.is_empty() {
                    self.render_text(target, &all_glyphs);
                }

                // Render SVGs as rasterized images for high-quality anti-aliasing
                if !svgs.is_empty() {
                    self.render_rasterized_svgs(target, &svgs, scale_factor);
                }
            }

            // Render text decorations (strikethrough, underline)
            let decorations_by_layer = generate_text_decoration_primitives_by_layer(&texts);
            for primitives in decorations_by_layer.values() {
                if !primitives.is_empty() {
                    self.render_unified(target, primitives);
                }
            }
        }

        // Return scratch buffers for reuse on next frame
        self.return_scratch_elements(texts, svgs, images);

        // Poll the device to free completed command buffers and prevent memory accumulation
        self.renderer.poll();

        Ok(())
    }

    /// Return element vectors to scratch pool for reuse
    #[inline]
    fn return_scratch_elements(
        &mut self,
        mut texts: Vec<TextElement>,
        mut svgs: Vec<SvgElement>,
        mut images: Vec<ImageElement>,
    ) {
        // Clear and keep capacity for reuse
        texts.clear();
        svgs.clear();
        images.clear();
        self.scratch_texts = texts;
        self.scratch_svgs = svgs;
        self.scratch_images = images;
    }

    /// Log cache statistics (called every 300 frames, ~5 seconds at 60fps).
    /// Visible at RUST_LOG=blinc_app=debug level.
    fn log_cache_stats(&mut self) {
        self.frame_count += 1;
        if self.frame_count % 300 != 1 {
            return;
        }
        let (aw, ah) = self.text_ctx.atlas_dimensions();
        let (caw, cah) = self.text_ctx.color_atlas_dimensions();
        let atlas_glyphs = self.text_ctx.atlas().glyph_count();
        let atlas_util = self.text_ctx.atlas().utilization();
        let color_glyphs = self.text_ctx.color_atlas().glyph_count();
        let color_util = self.text_ctx.color_atlas().utilization();
        let glyph_cache = self.text_ctx.glyph_cache_len();
        let glyph_cap = self.text_ctx.glyph_cache_capacity();
        let color_cache = self.text_ctx.color_glyph_cache_len();
        let color_cap = self.text_ctx.color_glyph_cache_capacity();
        let img_cache = self.image_cache.len();
        let svg_cache = self.svg_cache.len();
        let svg_atlas_entries = self.svg_atlas.entry_count();
        let svg_atlas_util = self.svg_atlas.utilization();
        let (svg_aw, svg_ah) = (self.svg_atlas.width(), self.svg_atlas.height());

        tracing::info!(
            "Cache stats [frame {}]: \
             atlas={}x{} ({} glyphs, {:.1}% used), \
             color_atlas={}x{} ({} glyphs, {:.1}% used), \
             glyph_lru={}/{}, color_glyph_lru={}/{}, \
             image={}/{}, svg_doc={}/{}, svg_atlas={}x{} ({} entries, {:.1}% used)",
            self.frame_count,
            aw,
            ah,
            atlas_glyphs,
            atlas_util * 100.0,
            caw,
            cah,
            color_glyphs,
            color_util * 100.0,
            glyph_cache,
            glyph_cap,
            color_cache,
            color_cap,
            img_cache,
            IMAGE_CACHE_CAPACITY,
            svg_cache,
            SVG_CACHE_CAPACITY,
            svg_aw,
            svg_ah,
            svg_atlas_entries,
            svg_atlas_util * 100.0,
        );
    }

    /// Ensure glass-related textures exist and are the right size.
    /// Only called when glass elements are present in the scene.
    ///
    /// We use a single texture for both rendering and sampling (backdrop_texture).
    /// The texture is rendered at half resolution to save memory (blur doesn't need full res).
    fn ensure_glass_textures(&mut self, width: u32, height: u32) {
        // Use the same texture format as the renderer's pipelines
        let format = self.renderer.texture_format();

        // Use half resolution for glass backdrop - blur effect doesn't need full resolution
        // This saves 75% of texture memory (e.g., 2.5MB -> 0.6MB for 900x700 window)
        let backdrop_width = (width / 2).max(1);
        let backdrop_height = (height / 2).max(1);

        let needs_backdrop = self
            .backdrop_texture
            .as_ref()
            .map(|t| t.width != backdrop_width || t.height != backdrop_height)
            .unwrap_or(true);

        if needs_backdrop {
            // Single texture that can be both rendered to AND sampled from
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Glass Backdrop"),
                size: wgpu::Extent3d {
                    width: backdrop_width,
                    height: backdrop_height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.backdrop_texture = Some(CachedTexture {
                texture,
                view,
                width: backdrop_width,
                height: backdrop_height,
            });
        }
    }

    /// Render text glyphs
    fn render_text(&mut self, target: &wgpu::TextureView, glyphs: &[GpuGlyph]) {
        if let (Some(atlas_view), Some(color_atlas_view)) =
            (self.text_ctx.atlas_view(), self.text_ctx.color_atlas_view())
        {
            self.renderer.render_text(
                target,
                glyphs,
                atlas_view,
                color_atlas_view,
                self.text_ctx.sampler(),
            );
        }
    }

    /// Render SDF primitives and text glyphs in a unified pass
    ///
    /// This ensures text and shapes transform together during animations,
    /// preventing visual lag when parent containers have motion transforms.
    ///
    /// **Glyph atlas binding is required.** The unified rendering path
    /// converts text glyphs into `GpuPrimitive`s with `prim_type =
    /// PRIM_TEXT`, which the SDF shader's `case PRIM_TEXT:` arm
    /// samples from `glyph_atlas` / `color_glyph_atlas`. The default
    /// SDF bind group has 1×1 placeholder textures bound to those
    /// slots — without explicitly binding the real atlases via
    /// `render_primitives_overlay_with_glyphs`, every text quad
    /// samples a transparent placeholder pixel and the text renders
    /// invisibly.
    ///
    /// `render_tree_with_motion` (the desktop / mobile path) handles
    /// this via the same call. Skipping it was a `render_tree`
    /// (headless / web) path bug — text was being correctly converted
    /// to primitives but the placeholder atlas was producing zero
    /// output for every glyph quad.
    fn render_unified(&mut self, target: &wgpu::TextureView, primitives: &[GpuPrimitive]) {
        if primitives.is_empty() {
            return;
        }

        // MSAA route: draw the SDF primitive stream (including the
        // mesh triangles tessellated from solid path fills) through the
        // MSAA-enabled split pipelines so path fills get the same
        // hardware-resolved smoothing that gradient and stroke paths
        // already receive via `render_paths_overlay_msaa`. Without
        // this, unified-text mode bypassed MSAA for every `PRIM_MESH`
        // primitive, leaving tessellated vector output visibly rougher
        // than the rasterized-SVG path.
        //
        // The glyph atlas bind group is already attached to
        // `self.bind_groups.sdf` via `set_glyph_atlas()` at the start
        // of the frame (the same way
        // `render_primitives_overlay_with_glyphs` uses it), so
        // `PRIM_TEXT` primitives in the same stream still sample the
        // real atlas here.
        if self.sample_count > 1 {
            self.renderer
                .render_primitives_overlay_msaa(target, primitives, self.sample_count);
            return;
        }

        if let (Some(atlas_view), Some(color_atlas_view)) =
            (self.text_ctx.atlas_view(), self.text_ctx.color_atlas_view())
        {
            self.renderer.render_primitives_overlay_with_glyphs(
                target,
                primitives,
                atlas_view,
                color_atlas_view,
            );
        } else {
            // No atlas available — fall back to plain primitive
            // rendering. Text quads will sample placeholder pixels
            // and render invisibly, but at least non-text primitives
            // still render.
            self.renderer.render_primitives_overlay(target, primitives);
        }
    }

    /// Render text decorations for a specific z-layer
    fn render_text_decorations_for_layer(
        &mut self,
        target: &wgpu::TextureView,
        decorations_by_layer: &std::collections::HashMap<u32, Vec<GpuPrimitive>>,
        z_layer: u32,
    ) {
        if let Some(primitives) = decorations_by_layer.get(&z_layer) {
            if !primitives.is_empty() {
                self.renderer.render_primitives_overlay(target, primitives);
            }
        }
    }

    /// Render debug visualization overlays for text elements
    ///
    /// When `BLINC_DEBUG=text` (or `1`, `all`, `true`) is set, this renders:
    /// - Cyan: Text bounding box outline
    /// - Magenta: Baseline position
    /// - Green: Top of bounding box (ascender reference)
    /// - Yellow: Bottom of bounding box (descender reference)
    fn render_text_debug(&mut self, target: &wgpu::TextureView, texts: &[TextElement]) {
        let debug_primitives = generate_text_debug_primitives(texts);
        if !debug_primitives.is_empty() {
            self.renderer
                .render_primitives_overlay(target, &debug_primitives);
        }
    }

    /// Render debug visualization overlays for all layout elements
    ///
    /// When `BLINC_DEBUG=layout` (or `all`) is set, this renders:
    /// - Semi-transparent colored rectangles for each element's bounding box
    /// - Colors cycle based on tree depth to distinguish nested elements
    fn render_layout_debug(&mut self, target: &wgpu::TextureView, tree: &RenderTree, scale: f32) {
        let debug_bounds = collect_debug_bounds(tree, scale);
        let debug_primitives = generate_layout_debug_primitives(&debug_bounds);
        if !debug_primitives.is_empty() {
            self.renderer
                .render_primitives_overlay(target, &debug_primitives);
        }
    }

    /// Render debug visualization for motion/animations
    ///
    /// When `BLINC_DEBUG=motion` (or `all`) is set, this renders:
    /// - Top-right corner overlay showing animation stats
    /// - Number of active visual animations, layout animations, etc.
    fn render_motion_debug(
        &mut self,
        target: &wgpu::TextureView,
        tree: &RenderTree,
        width: u32,
        _height: u32,
    ) {
        let stats = tree.debug_stats();
        let mut debug_primitives = Vec::new();

        // Background for the debug panel
        let panel_width = 200.0;
        let panel_height = 100.0;
        let panel_x = width as f32 - panel_width - 10.0;
        let panel_y = 10.0;

        // Semi-transparent dark background
        debug_primitives.push(
            GpuPrimitive::rect(panel_x, panel_y, panel_width, panel_height)
                .with_color(0.1, 0.1, 0.15, 0.85)
                .with_corner_radius(6.0),
        );

        // Status indicator - green if any animations active
        let has_active = stats.visual_animation_count > 0
            || stats.layout_animation_count > 0
            || stats.animated_bounds_count > 0;

        let (r, g, b, a) = if has_active {
            (0.2, 0.9, 0.3, 1.0) // Green when animating
        } else {
            (0.4, 0.4, 0.5, 1.0) // Gray when idle
        };

        debug_primitives.push(
            GpuPrimitive::rect(panel_x + 10.0, panel_y + 12.0, 10.0, 10.0)
                .with_color(r, g, b, a)
                .with_corner_radius(5.0),
        );

        // Visual bars showing animation counts
        let bar_x = panel_x + 12.0;
        let bar_width = panel_width - 24.0;
        let bar_height = 6.0;

        // Visual animations bar (cyan)
        let visual_ratio = (stats.visual_animation_count as f32).min(10.0) / 10.0;
        if visual_ratio > 0.0 {
            debug_primitives.push(
                GpuPrimitive::rect(bar_x, panel_y + 35.0, bar_width * visual_ratio, bar_height)
                    .with_color(0.0, 0.8, 0.9, 0.9)
                    .with_corner_radius(3.0),
            );
        }

        // Layout animations bar (magenta)
        let layout_ratio = (stats.layout_animation_count as f32).min(10.0) / 10.0;
        if layout_ratio > 0.0 {
            debug_primitives.push(
                GpuPrimitive::rect(bar_x, panel_y + 50.0, bar_width * layout_ratio, bar_height)
                    .with_color(0.9, 0.2, 0.8, 0.9)
                    .with_corner_radius(3.0),
            );
        }

        // Animated bounds bar (yellow)
        let bounds_ratio = (stats.animated_bounds_count as f32).min(50.0) / 50.0;
        if bounds_ratio > 0.0 {
            debug_primitives.push(
                GpuPrimitive::rect(bar_x, panel_y + 65.0, bar_width * bounds_ratio, bar_height)
                    .with_color(0.95, 0.85, 0.2, 0.9)
                    .with_corner_radius(3.0),
            );
        }

        // Scroll physics indicator (orange dots)
        let scroll_count = stats.scroll_physics_count.min(8);
        for i in 0..scroll_count {
            debug_primitives.push(
                GpuPrimitive::rect(bar_x + (i as f32 * 14.0), panel_y + 80.0, 8.0, 8.0)
                    .with_color(1.0, 0.6, 0.2, 0.9)
                    .with_corner_radius(4.0),
            );
        }

        if !debug_primitives.is_empty() {
            self.renderer
                .render_primitives_overlay(target, &debug_primitives);
        }
    }

    /// Render images to the backdrop texture (for images that should be blurred by glass)
    fn render_images_to_backdrop(&mut self, images: &[&ImageElement]) {
        let Some(ref backdrop) = self.backdrop_texture else {
            return;
        };
        // Create a new view to avoid borrow conflicts
        let target = backdrop
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.render_images_ref(&target, images);
    }

    /// Pre-load images into cache (call before rendering)
    ///
    /// Images with lazy loading strategy are only loaded when visible in the viewport.
    /// A buffer zone extends the viewport to preload images that are about to become visible.
    fn preload_images(
        &mut self,
        images: &[ImageElement],
        viewport_width: f32,
        viewport_height: f32,
    ) {
        // Buffer zone: load images that are within 100px of becoming visible
        const VISIBILITY_BUFFER: f32 = 100.0;

        // Eagerly load placeholder images for any lazy element with placeholder_type == 2
        // (so the placeholder is already in cache when we go to render it).
        // Use get() instead of contains() so cached placeholders are promoted to
        // MRU and survive eviction pressure from the main image puts below.
        for image in images {
            if image.placeholder_type == 2 {
                if let Some(ref placeholder_src) = image.placeholder_image {
                    if self.image_cache.get(placeholder_src).is_none() {
                        let source = blinc_image::ImageSource::from_uri(placeholder_src);
                        if let Ok(data) = blinc_image::ImageData::load(source) {
                            // `is_srgb = false` mirrors
                            // `create_image_labeled`'s existing
                            // behavior so we don't accidentally
                            // change sampling here — the 2D image
                            // pipeline treats bytes as linear.
                            let has_bc = self.renderer.has_texture_compression_bc()
                                && bc_eligible(placeholder_src, data.width(), data.height());
                            let gpu_image = self.image_ctx.create_image_maybe_compressed(
                                data.pixels(),
                                data.width(),
                                data.height(),
                                false,
                                has_bc,
                                placeholder_src,
                            );
                            self.image_cache.put(placeholder_src.clone(), gpu_image);
                        }
                    }
                }
            }
        }

        for image in images {
            // Use get() (not contains()) so the cache hit promotes the entry
            // to MRU. Without this, the LRU order is set entirely by insertion
            // order, and any new put() during scroll evicts the oldest visible
            // image first — which is exactly the row at the top of the viewport.
            // Promoting on hit during preload guarantees the eviction victims
            // are non-visible entries at the back of the cache.
            if self.image_cache.get(&image.source).is_some() {
                continue;
            }

            // Check if lazy loading is enabled (loading_strategy == 1)
            if image.loading_strategy == 1 {
                // If image has clip bounds from a scroll container, use those for visibility check
                // The clip bounds represent the visible area of the parent scroll container
                let is_visible = if let Some([clip_x, clip_y, clip_w, clip_h]) = image.clip_bounds {
                    // Check if image intersects with its clip region (+ buffer for prefetching)
                    let clip_left = clip_x - VISIBILITY_BUFFER;
                    let clip_top = clip_y - VISIBILITY_BUFFER;
                    let clip_right = clip_x + clip_w + VISIBILITY_BUFFER;
                    let clip_bottom = clip_y + clip_h + VISIBILITY_BUFFER;

                    let image_right = image.x + image.width;
                    let image_bottom = image.y + image.height;

                    image.x < clip_right
                        && image_right > clip_left
                        && image.y < clip_bottom
                        && image_bottom > clip_top
                } else {
                    // No clip bounds - check against viewport
                    let viewport_left = -VISIBILITY_BUFFER;
                    let viewport_top = -VISIBILITY_BUFFER;
                    let viewport_right = viewport_width + VISIBILITY_BUFFER;
                    let viewport_bottom = viewport_height + VISIBILITY_BUFFER;

                    let image_right = image.x + image.width;
                    let image_bottom = image.y + image.height;

                    image.x < viewport_right
                        && image_right > viewport_left
                        && image.y < viewport_bottom
                        && image_bottom > viewport_top
                };

                if !is_visible {
                    // Skip loading - image is not yet visible
                    continue;
                }
            }

            // Try to load the image - use from_uri to handle emoji://, data:, and file paths
            let source = blinc_image::ImageSource::from_uri(&image.source);
            let image_data = match blinc_image::ImageData::load(source) {
                Ok(data) => data,
                Err(e) => {
                    tracing::trace!("Failed to load image '{}': {:?}", image.source, e);
                    continue; // Skip images that fail to load
                }
            };

            // Create GPU texture — compress to BC1/BC3 when the
            // device supports it so the image cache's VRAM
            // footprint scales with asset count instead of blowing
            // past the LRU budget on 4K dashboards. `bc_eligible`
            // filters out cases where BC's 4×4-block quantization
            // would be visually unacceptable (emoji sprites, small
            // icons with smooth gradients).
            let has_bc = self.renderer.has_texture_compression_bc()
                && bc_eligible(&image.source, image_data.width(), image_data.height());
            let gpu_image = self.image_ctx.create_image_maybe_compressed(
                image_data.pixels(),
                image_data.width(),
                image_data.height(),
                false,
                has_bc,
                &image.source,
            );

            // LruCache::put evicts oldest entry if at capacity
            self.image_cache.put(image.source.clone(), gpu_image);
            // Record load time for fade-in animation
            self.image_load_times
                .insert(image.source.clone(), web_time::Instant::now());
        }
    }

    /// Pre-load mask images referenced in a primitive batch's layer effects
    fn preload_mask_images(&mut self, batch: &PrimitiveBatch) {
        use blinc_core::LayerEffect;
        for entry in &batch.layer_commands {
            if let blinc_gpu::primitives::LayerCommand::Push { config } = &entry.command {
                for effect in &config.effects {
                    if let LayerEffect::MaskImage { image_url, .. } = effect {
                        if self.renderer.has_mask_image(image_url) {
                            continue;
                        }
                        let source = blinc_image::ImageSource::from_uri(image_url);
                        if let Ok(data) = blinc_image::ImageData::load(source) {
                            self.renderer.load_mask_image_rgba(
                                image_url,
                                data.pixels(),
                                data.width(),
                                data.height(),
                            );
                        }
                    }
                }
            }
        }
    }

    /// Convert a CssFilter into filter_a/filter_b arrays for the image shader.
    /// Returns (filter_a, filter_b) where identity = ([0,0,0,0], [1,1,1,0]).
    /// Extract mask gradient params and info from a MaskImage gradient.
    /// Returns ([mask_params], [mask_info]) or zero arrays if not a gradient.
    fn mask_image_to_arrays(mask: Option<&blinc_core::MaskImage>) -> ([f32; 4], [f32; 4]) {
        match mask {
            Some(blinc_core::MaskImage::Gradient(gradient)) => match gradient {
                blinc_core::Gradient::Linear {
                    start, end, stops, ..
                } => {
                    let (sa, ea) = Self::extract_mask_alphas_from_stops(stops);
                    ([start.x, start.y, end.x, end.y], [1.0, sa, ea, 0.0])
                }
                blinc_core::Gradient::Radial {
                    center,
                    radius,
                    stops,
                    ..
                } => {
                    let (sa, ea) = Self::extract_mask_alphas_from_stops(stops);
                    ([center.x, center.y, *radius, 0.0], [2.0, sa, ea, 0.0])
                }
                blinc_core::Gradient::Conic { center, stops, .. } => {
                    let (sa, ea) = Self::extract_mask_alphas_from_stops(stops);
                    ([center.x, center.y, 0.5, 0.0], [2.0, sa, ea, 0.0])
                }
            },
            _ => ([0.0; 4], [0.0; 4]),
        }
    }

    fn extract_mask_alphas_from_stops(stops: &[blinc_core::GradientStop]) -> (f32, f32) {
        if stops.is_empty() {
            return (1.0, 0.0);
        }
        (stops[0].color.a, stops[stops.len() - 1].color.a)
    }

    fn css_filter_to_arrays(
        filter: &blinc_layout::element_style::CssFilter,
    ) -> ([f32; 4], [f32; 4]) {
        (
            [
                filter.grayscale,
                filter.invert,
                filter.sepia,
                filter.hue_rotate.to_radians(),
            ],
            [filter.brightness, filter.contrast, filter.saturate, 0.0],
        )
    }

    /// Transform clip bounds and radii by a CSS affine.
    /// When a parent div has a CSS transform (e.g. `scale(1.08)` on hover), the image
    /// clip must follow the same transform so the image fills the visually-scaled parent.
    fn transform_clip_by_affine(
        clip: [f32; 4],
        clip_radius: [f32; 4],
        affine: [f32; 6],
        scale_factor: f32,
    ) -> ([f32; 4], [f32; 4]) {
        let [a, b, c, d, tx, ty] = affine;
        let tx_s = tx * scale_factor;
        let ty_s = ty * scale_factor;
        // Transform clip center through the affine
        let ccx = clip[0] + clip[2] * 0.5;
        let ccy = clip[1] + clip[3] * 0.5;
        let new_cx = a * ccx + c * ccy + tx_s;
        let new_cy = b * ccx + d * ccy + ty_s;
        // Uniform scale for dimensions
        let s = (a * d - b * c).abs().sqrt().max(1e-6);
        let new_clip = [
            new_cx - clip[2] * s * 0.5,
            new_cy - clip[3] * s * 0.5,
            clip[2] * s,
            clip[3] * s,
        ];
        let new_radius = [
            clip_radius[0] * s,
            clip_radius[1] * s,
            clip_radius[2] * s,
            clip_radius[3] * s,
        ];
        (new_clip, new_radius)
    }

    /// Decompose a CSS affine [a,b,c,d,tx,ty] into position and 2x2 transform for image rendering.
    /// Input: original rect (already DPI-scaled), affine (layout coords), scale_factor.
    /// Returns: (draw_x, draw_y, draw_w, draw_h, transform_a, transform_b, transform_c, transform_d)
    /// The 2x2 matrix [a, b, c, d] is passed to the shader for full affine support (rotation, scale, skew).
    fn decompose_image_affine(
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        affine: [f32; 6],
        scale_factor: f32,
    ) -> (f32, f32, f32, f32, f32, f32, f32, f32) {
        let [a, b, c, d, tx, ty] = affine;
        // DPI-scale the translation components
        let tx_s = tx * scale_factor;
        let ty_s = ty * scale_factor;
        // Transform center through the affine (positions are already in screen space)
        let cx = x + w * 0.5;
        let cy = y + h * 0.5;
        let new_cx = a * cx + c * cy + tx_s;
        let new_cy = b * cx + d * cy + ty_s;
        // Pass original bounds — the 2x2 transform is applied in the shader around the center
        (new_cx - w * 0.5, new_cy - h * 0.5, w, h, a, b, c, d)
    }

    /// Render images to target (images must be preloaded first)
    fn render_images(
        &mut self,
        target: &wgpu::TextureView,
        images: &[ImageElement],
        viewport_width: f32,
        viewport_height: f32,
        scale_factor: f32,
    ) {
        use blinc_image::{calculate_fit_rects, src_rect_to_uv, ObjectFit, ObjectPosition};

        for image in images {
            // Get cached GPU image
            let gpu_image = self.image_cache.get(&image.source);

            // Compute fade-in opacity multiplier from load time + duration
            // Returns 1.0 if no fade configured or fade complete; <1.0 during fade
            let fade_factor = if image.fade_duration_ms > 0 && gpu_image.is_some() {
                if let Some(loaded_at) = self.image_load_times.get(&image.source) {
                    let elapsed_ms = loaded_at.elapsed().as_secs_f32() * 1000.0;
                    (elapsed_ms / image.fade_duration_ms as f32).clamp(0.0, 1.0)
                } else {
                    1.0
                }
            } else {
                1.0
            };
            if fade_factor < 1.0 {
                // Force continuous redraw while fade is in progress
                self.has_active_flows = true;
            }

            // If image is not loaded and has a placeholder, render placeholder
            if gpu_image.is_none() && image.placeholder_type != 0 {
                match image.placeholder_type {
                    // Type 1: Solid color
                    1 => {
                        let color = blinc_core::Color::rgba(
                            image.placeholder_color[0],
                            image.placeholder_color[1],
                            image.placeholder_color[2],
                            image.placeholder_color[3],
                        );
                        let mut ctx = GpuPaintContext::new(viewport_width, viewport_height);
                        let rect =
                            blinc_core::Rect::new(image.x, image.y, image.width, image.height);
                        ctx.fill_rounded_rect(
                            rect,
                            blinc_core::CornerRadius::uniform(image.border_radius),
                            color,
                        );
                        let batch = ctx.take_batch();
                        self.renderer.render_overlay(target, &batch);
                    }
                    // Type 2: Image placeholder (e.g., low-res thumbnail or blur hash)
                    2 => {
                        if let Some(ref placeholder_src) = image.placeholder_image {
                            if let Some(placeholder_gpu) = self.image_cache.get(placeholder_src) {
                                let (src_rect, dst_rect) = calculate_fit_rects(
                                    placeholder_gpu.width(),
                                    placeholder_gpu.height(),
                                    image.width,
                                    image.height,
                                    ObjectFit::Cover,
                                    ObjectPosition::new(0.5, 0.5),
                                );
                                let src_uv = src_rect_to_uv(
                                    src_rect,
                                    placeholder_gpu.width(),
                                    placeholder_gpu.height(),
                                );
                                let instance = GpuImageInstance::new(
                                    image.x + dst_rect[0],
                                    image.y + dst_rect[1],
                                    dst_rect[2],
                                    dst_rect[3],
                                )
                                .with_src_uv(src_uv[0], src_uv[1], src_uv[2], src_uv[3])
                                .with_border_radius(image.border_radius)
                                .with_opacity(image.opacity);
                                self.renderer.render_images(
                                    target,
                                    placeholder_gpu.view(),
                                    &[instance],
                                );
                            }
                        }
                    }
                    // Type 3: Skeleton shimmer (animated gradient sweep)
                    3 => {
                        let t =
                            self.frame_count.saturating_mul(16).rem_euclid(2400) as f32 / 2400.0;
                        let base_a = image.placeholder_color[3].max(0.4);
                        let base = blinc_core::Color::rgba(
                            image.placeholder_color[0],
                            image.placeholder_color[1],
                            image.placeholder_color[2],
                            base_a,
                        );
                        let highlight_a = (base_a + 0.25).min(1.0);
                        let highlight = blinc_core::Color::rgba(
                            (image.placeholder_color[0] + 0.15).min(1.0),
                            (image.placeholder_color[1] + 0.15).min(1.0),
                            (image.placeholder_color[2] + 0.15).min(1.0),
                            highlight_a,
                        );
                        let mut ctx = GpuPaintContext::new(viewport_width, viewport_height);
                        let rect =
                            blinc_core::Rect::new(image.x, image.y, image.width, image.height);
                        // Base background
                        ctx.fill_rounded_rect(
                            rect,
                            blinc_core::CornerRadius::uniform(image.border_radius),
                            base,
                        );
                        // Shimmer band — narrow vertical strip swept horizontally
                        let band_w = (image.width * 0.25).max(40.0);
                        let band_x = image.x + (image.width + band_w) * t - band_w;
                        let band_rect =
                            blinc_core::Rect::new(band_x, image.y, band_w, image.height);
                        ctx.fill_rounded_rect(
                            band_rect,
                            blinc_core::CornerRadius::uniform(image.border_radius),
                            highlight,
                        );
                        let batch = ctx.take_batch();
                        self.renderer.render_overlay(target, &batch);
                        // Mark frame as needing redraw for animation
                        self.has_active_flows = true;
                    }
                    _ => {}
                }
                continue;
            }

            let Some(gpu_image) = gpu_image else {
                continue; // Skip images that failed to load
            };

            // Convert object_fit byte to ObjectFit enum
            let object_fit = match image.object_fit {
                0 => ObjectFit::Cover,
                1 => ObjectFit::Contain,
                2 => ObjectFit::Fill,
                3 => ObjectFit::ScaleDown,
                4 => ObjectFit::None,
                _ => ObjectFit::Cover,
            };

            // Create ObjectPosition from array
            let object_position =
                ObjectPosition::new(image.object_position[0], image.object_position[1]);

            // Calculate fit rectangles
            let (src_rect, dst_rect) = calculate_fit_rects(
                gpu_image.width(),
                gpu_image.height(),
                image.width,
                image.height,
                object_fit,
                object_position,
            );

            // Convert src_rect to UV coordinates
            let src_uv = src_rect_to_uv(src_rect, gpu_image.width(), gpu_image.height());

            // Apply CSS affine transform if present
            let base_x = image.x + dst_rect[0];
            let base_y = image.y + dst_rect[1];
            let base_w = dst_rect[2];
            let base_h = dst_rect[3];

            let (draw_x, draw_y, draw_w, draw_h, ta, tb, tc, td) = if let Some(affine) =
                image.css_affine
            {
                Self::decompose_image_affine(base_x, base_y, base_w, base_h, affine, scale_factor)
            } else {
                (base_x, base_y, base_w, base_h, 1.0, 0.0, 0.0, 1.0)
            };

            // Pre-compute effective clip (transformed by CSS affine if present)
            let effective_clip = image.clip_bounds.map(|clip| {
                if let Some(affine) = image.css_affine {
                    Self::transform_clip_by_affine(clip, image.clip_radius, affine, scale_factor)
                } else {
                    (clip, image.clip_radius)
                }
            });

            // Render shadow before image if present
            if let Some(ref shadow) = image.shadow {
                let mut shadow_ctx = GpuPaintContext::new(viewport_width, viewport_height);
                // Push scroll/parent clip so shadow doesn't escape the container
                if let Some(clip) = image.clip_bounds {
                    shadow_ctx.push_clip(blinc_core::ClipShape::RoundedRect {
                        rect: blinc_core::Rect::new(clip[0], clip[1], clip[2], clip[3]),
                        corner_radius: blinc_core::CornerRadius {
                            top_left: image.clip_radius[0],
                            top_right: image.clip_radius[1],
                            bottom_right: image.clip_radius[2],
                            bottom_left: image.clip_radius[3],
                        },
                    });
                }
                let shadow_rect =
                    blinc_core::Rect::new(image.x, image.y, image.width, image.height);
                let shadow_radius = blinc_core::CornerRadius::uniform(image.border_radius);
                shadow_ctx.draw_shadow(shadow_rect, shadow_radius, *shadow);
                let shadow_batch = shadow_ctx.take_batch();
                self.renderer.render_overlay(target, &shadow_batch);
            }

            // Create GPU instance with proper positioning
            let mut instance = GpuImageInstance::new(draw_x, draw_y, draw_w, draw_h)
                .with_src_uv(src_uv[0], src_uv[1], src_uv[2], src_uv[3])
                .with_tint(image.tint[0], image.tint[1], image.tint[2], image.tint[3])
                .with_border_radius(image.border_radius)
                .with_opacity(image.opacity * fade_factor)
                .with_transform(ta, tb, tc, td)
                .with_filter(image.filter_a, image.filter_b);

            // Render border inside the image shader (same SDF, perfect transform alignment)
            if image.border_width > 0.0 {
                instance = instance.with_image_border(
                    image.border_width,
                    image.border_color.r,
                    image.border_color.g,
                    image.border_color.b,
                    image.border_color.a,
                );
            }

            // Apply mask gradient
            if image.mask_info[0] > 0.5 {
                instance.mask_params = image.mask_params;
                instance.mask_info = image.mask_info;
            }

            // Apply clip bounds (primary rounded clip)
            if let Some((clip, clip_r)) = effective_clip {
                instance = instance.with_clip_rounded_rect_corners(
                    clip[0], clip[1], clip[2], clip[3], clip_r[0], clip_r[1], clip_r[2], clip_r[3],
                );
            }
            // Apply secondary scroll clip (sharp rect)
            if let Some(sc) = image.scroll_clip {
                instance = instance.with_clip2_rect(sc[0], sc[1], sc[2], sc[3]);
            }

            // Render the image
            self.renderer
                .render_images(target, gpu_image.view(), &[instance]);
        }
    }

    /// Render images to target from references (images must be preloaded first)
    fn render_images_ref(&mut self, target: &wgpu::TextureView, images: &[&ImageElement]) {
        use blinc_image::{calculate_fit_rects, src_rect_to_uv, ObjectFit, ObjectPosition};

        for image in images {
            // Get cached GPU image
            let Some(gpu_image) = self.image_cache.get(&image.source) else {
                continue; // Skip images that failed to load
            };

            // Compute fade-in opacity multiplier
            let fade_factor = if image.fade_duration_ms > 0 {
                if let Some(loaded_at) = self.image_load_times.get(&image.source) {
                    let elapsed_ms = loaded_at.elapsed().as_secs_f32() * 1000.0;
                    (elapsed_ms / image.fade_duration_ms as f32).clamp(0.0, 1.0)
                } else {
                    1.0
                }
            } else {
                1.0
            };
            if fade_factor < 1.0 {
                self.has_active_flows = true;
            }

            // Convert object_fit byte to ObjectFit enum
            let object_fit = match image.object_fit {
                0 => ObjectFit::Cover,
                1 => ObjectFit::Contain,
                2 => ObjectFit::Fill,
                3 => ObjectFit::ScaleDown,
                4 => ObjectFit::None,
                _ => ObjectFit::Cover,
            };

            // Create ObjectPosition from array
            let object_position =
                ObjectPosition::new(image.object_position[0], image.object_position[1]);

            // Calculate fit rectangles
            let (src_rect, dst_rect) = calculate_fit_rects(
                gpu_image.width(),
                gpu_image.height(),
                image.width,
                image.height,
                object_fit,
                object_position,
            );

            // Convert src_rect to UV coordinates
            let src_uv = src_rect_to_uv(src_rect, gpu_image.width(), gpu_image.height());

            // Apply CSS affine transform if present
            let base_x = image.x + dst_rect[0];
            let base_y = image.y + dst_rect[1];
            let base_w = dst_rect[2];
            let base_h = dst_rect[3];

            // render_images_ref is called for backdrop images; no scale_factor available,
            // but affine translation is already in screen coords for backdrop path
            let (draw_x, draw_y, draw_w, draw_h, ta, tb, tc, td) =
                if let Some(affine) = image.css_affine {
                    Self::decompose_image_affine(base_x, base_y, base_w, base_h, affine, 1.0)
                } else {
                    (base_x, base_y, base_w, base_h, 1.0, 0.0, 0.0, 1.0)
                };

            // Pre-compute effective clip (transformed by CSS affine if present)
            let effective_clip = image.clip_bounds.map(|clip| {
                if let Some(affine) = image.css_affine {
                    Self::transform_clip_by_affine(clip, image.clip_radius, affine, 1.0)
                } else {
                    (clip, image.clip_radius)
                }
            });

            // Create GPU instance with proper positioning
            let mut instance = GpuImageInstance::new(draw_x, draw_y, draw_w, draw_h)
                .with_src_uv(src_uv[0], src_uv[1], src_uv[2], src_uv[3])
                .with_tint(image.tint[0], image.tint[1], image.tint[2], image.tint[3])
                .with_border_radius(image.border_radius)
                .with_opacity(image.opacity * fade_factor)
                .with_transform(ta, tb, tc, td)
                .with_filter(image.filter_a, image.filter_b);

            // Render border inside the image shader (same SDF, perfect transform alignment)
            if image.border_width > 0.0 {
                instance = instance.with_image_border(
                    image.border_width,
                    image.border_color.r,
                    image.border_color.g,
                    image.border_color.b,
                    image.border_color.a,
                );
            }

            // Apply mask gradient
            if image.mask_info[0] > 0.5 {
                instance.mask_params = image.mask_params;
                instance.mask_info = image.mask_info;
            }

            // Apply clip bounds (primary rounded clip)
            if let Some((clip, clip_r)) = effective_clip {
                instance = instance.with_clip_rounded_rect_corners(
                    clip[0], clip[1], clip[2], clip[3], clip_r[0], clip_r[1], clip_r[2], clip_r[3],
                );
            }
            // Apply secondary scroll clip (sharp rect)
            if let Some(sc) = image.scroll_clip {
                instance = instance.with_clip2_rect(sc[0], sc[1], sc[2], sc[3]);
            }

            // Render the image
            self.renderer
                .render_images(target, gpu_image.view(), &[instance]);
        }
    }

    /// Render an SVG element with clipping and opacity support
    fn render_svg_element(&mut self, ctx: &mut GpuPaintContext, svg: &SvgElement) {
        // Skip completely transparent SVGs
        if svg.motion_opacity <= 0.001 {
            return;
        }

        // Skip SVGs completely outside their clip bounds
        if let Some([clip_x, clip_y, clip_w, clip_h]) = svg.clip_bounds {
            let svg_right = svg.x + svg.width;
            let svg_bottom = svg.y + svg.height;
            let clip_right = clip_x + clip_w;
            let clip_bottom = clip_y + clip_h;

            // Check if SVG is completely outside clip bounds
            if svg.x >= clip_right
                || svg_right <= clip_x
                || svg.y >= clip_bottom
                || svg_bottom <= clip_y
            {
                return;
            }
        }

        // Hash the SVG source for cache lookup (faster than using string as key)
        let svg_hash = {
            let mut hasher = DefaultHasher::new();
            svg.source.hash(&mut hasher);
            hasher.finish()
        };

        // Try cache lookup first, parse only on miss
        let doc = if let Some(cached) = self.svg_cache.get(&svg_hash) {
            cached.clone()
        } else {
            let Ok(parsed) = SvgDocument::from_str(&svg.source) else {
                return;
            };
            self.svg_cache.put(svg_hash, parsed.clone());
            parsed
        };

        // Apply clipping if present
        if let Some([clip_x, clip_y, clip_w, clip_h]) = svg.clip_bounds {
            ctx.push_clip(blinc_core::ClipShape::rect(Rect::new(
                clip_x, clip_y, clip_w, clip_h,
            )));
        }

        // Apply opacity if not fully opaque
        if svg.motion_opacity < 1.0 {
            ctx.push_opacity(svg.motion_opacity);
        }

        // Render the SVG with optional CSS overrides
        let has_css_overrides = svg.tint.is_some()
            || svg.fill.is_some()
            || svg.stroke.is_some()
            || svg.stroke_width.is_some();
        if has_css_overrides {
            self.render_svg_with_overrides(
                ctx,
                &doc,
                svg.x,
                svg.y,
                svg.width,
                svg.height,
                svg.tint,
                svg.fill,
                svg.stroke,
                svg.stroke_width,
            );
        } else {
            doc.render_fit(ctx, Rect::new(svg.x, svg.y, svg.width, svg.height));
        }

        // Pop opacity if applied
        if svg.motion_opacity < 1.0 {
            ctx.pop_opacity();
        }

        // Pop clip if applied
        if svg.clip_bounds.is_some() {
            ctx.pop_clip();
        }
    }

    /// Render an SVG with CSS overrides for fill, stroke, stroke-width, and tint
    #[allow(clippy::too_many_arguments)]
    fn render_svg_with_overrides(
        &self,
        ctx: &mut GpuPaintContext,
        doc: &SvgDocument,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        tint: Option<blinc_core::Color>,
        fill: Option<blinc_core::Color>,
        stroke: Option<blinc_core::Color>,
        stroke_width: Option<f32>,
    ) {
        use blinc_svg::SvgDrawCommand;

        // Calculate scale to fit within bounds while maintaining aspect ratio
        let scale_x = width / doc.width;
        let scale_y = height / doc.height;
        let scale = scale_x.min(scale_y);

        // Center within bounds
        let scaled_width = doc.width * scale;
        let scaled_height = doc.height * scale;
        let offset_x = x + (width - scaled_width) / 2.0;
        let offset_y = y + (height - scaled_height) / 2.0;

        let commands = doc.commands();

        for cmd in commands {
            match cmd {
                SvgDrawCommand::FillPath { path, brush } => {
                    let scaled = scale_and_translate_path(&path, offset_x, offset_y, scale);
                    // Priority: fill > tint > original brush
                    let fill_brush = if let Some(f) = fill {
                        Brush::Solid(f)
                    } else if let Some(t) = tint {
                        Brush::Solid(t)
                    } else {
                        brush.clone()
                    };
                    ctx.fill_path(&scaled, fill_brush);
                }
                SvgDrawCommand::StrokePath {
                    path,
                    stroke: orig_stroke,
                    brush,
                } => {
                    let scaled = scale_and_translate_path(&path, offset_x, offset_y, scale);
                    // Apply stroke-width override or scale original
                    let sw = stroke_width.unwrap_or(orig_stroke.width) * scale;
                    let scaled_stroke = Stroke::new(sw)
                        .with_cap(orig_stroke.cap)
                        .with_join(orig_stroke.join);
                    // Priority: stroke > tint > original brush
                    let stroke_brush = if let Some(s) = stroke {
                        Brush::Solid(s)
                    } else if let Some(t) = tint {
                        Brush::Solid(t)
                    } else {
                        brush.clone()
                    };
                    ctx.stroke_path(&scaled, &scaled_stroke, stroke_brush);
                }
            }
        }
    }

    /// Render SVG elements using CPU rasterization for high-quality anti-aliased output
    ///
    /// This method rasterizes SVGs using resvg/tiny-skia and renders them as textures,
    /// providing much better anti-aliasing than tessellation-based path rendering.
    ///
    /// The `scale_factor` parameter is the display's DPI scale (e.g., 2.0 for Retina).
    /// SVGs are rasterized at physical pixel resolution for crisp rendering on HiDPI displays.
    fn render_rasterized_svgs(
        &mut self,
        target: &wgpu::TextureView,
        svgs: &[SvgElement],
        scale_factor: f32,
    ) {
        // Evict stale atlas entries from the previous frame BEFORE
        // the loop so every UV coordinate computed below stays valid
        // for the entire render pass. Doing this mid-loop (inside
        // `insert`) would repack surviving entries to new shelf
        // positions, invalidating UVs already pushed into the
        // instance buffer → visible blink on animated SVGs.
        self.svg_atlas.begin_frame(&self.device);

        // Collect all instances for a single batched draw call
        let mut instances: Vec<GpuImageInstance> = Vec::with_capacity(svgs.len());

        for svg in svgs {
            // Skip completely transparent SVGs
            if svg.motion_opacity <= 0.001 {
                continue;
            }

            // Skip SVGs completely outside their clip bounds
            if let Some([clip_x, clip_y, clip_w, clip_h]) = svg.clip_bounds {
                let svg_right = svg.x + svg.width;
                let svg_bottom = svg.y + svg.height;
                let clip_right = clip_x + clip_w;
                let clip_bottom = clip_y + clip_h;

                if svg.x >= clip_right
                    || svg_right <= clip_x
                    || svg.y >= clip_bottom
                    || svg_bottom <= clip_y
                {
                    continue;
                }
            }

            // Rasterize at physical pixel resolution.
            //
            // `svg.width` / `svg.height` come out of `collect_elements_recursive`
            // already multiplied by `tree.scale_factor()` (see the SVG branch
            // around line 3066: `base_width = bounds.width * scale`), so they
            // are in *physical* pixels, not logical pixels — the same units
            // the GPU draw quad will be sized in. Multiplying by `scale_factor`
            // a second time here used to rasterize each icon at 4× its drawn
            // area on Retina (9× on 3× DPR), bloating the SVG atlas
            // (`cn_demo` was hitting the 4096×4096 ceiling on the first frame
            // and burning ~134 MB of CPU+GPU memory between the two mirror
            // buffers in `svg_atlas.rs`). resvg already does sub-pixel AA at
            // the target resolution, so 1:1 physical-pixel rasterization is
            // sharp enough; if a future workload turns up edge cases that
            // need supersampling, gate it behind an explicit knob rather than
            // a silent multiply.
            let raster_width = (svg.width.ceil() as u32).max(1);
            let raster_height = (svg.height.ceil() as u32).max(1);

            // Detect tintable SVGs: simple currentColor icons that can use shader tinting
            // instead of CPU re-rasterization per color variant.
            // Tintable = has tint, no other overrides, source uses currentColor.
            let is_tintable = svg.tint.is_some()
                && svg.fill.is_none()
                && svg.stroke.is_none()
                && svg.stroke_width.is_none()
                && svg.stroke_dasharray.is_none()
                && svg.stroke_dashoffset.is_none()
                && svg.svg_path_data.is_none()
                && svg.tag_overrides.is_empty()
                && svg.source.contains("currentColor");

            // Compute cache key: hash of (svg_source, width, height, scale, tint, fill, stroke, stroke_width)
            // For tintable SVGs, exclude tint from hash so all color variants share one texture.
            let cache_key = {
                let mut hasher = DefaultHasher::new();
                svg.source.hash(&mut hasher);
                raster_width.hash(&mut hasher);
                raster_height.hash(&mut hasher);
                if is_tintable {
                    // Sentinel byte to distinguish from non-tintable hashes
                    255u8.hash(&mut hasher);
                } else if let Some(tint) = &svg.tint {
                    tint.r.to_bits().hash(&mut hasher);
                    tint.g.to_bits().hash(&mut hasher);
                    tint.b.to_bits().hash(&mut hasher);
                    tint.a.to_bits().hash(&mut hasher);
                }
                if let Some(fill) = &svg.fill {
                    1u8.hash(&mut hasher);
                    fill.r.to_bits().hash(&mut hasher);
                    fill.g.to_bits().hash(&mut hasher);
                    fill.b.to_bits().hash(&mut hasher);
                    fill.a.to_bits().hash(&mut hasher);
                }
                if let Some(stroke) = &svg.stroke {
                    2u8.hash(&mut hasher);
                    stroke.r.to_bits().hash(&mut hasher);
                    stroke.g.to_bits().hash(&mut hasher);
                    stroke.b.to_bits().hash(&mut hasher);
                    stroke.a.to_bits().hash(&mut hasher);
                }
                if let Some(sw) = &svg.stroke_width {
                    3u8.hash(&mut hasher);
                    sw.to_bits().hash(&mut hasher);
                }
                if let Some(ref da) = svg.stroke_dasharray {
                    4u8.hash(&mut hasher);
                    for v in da {
                        v.to_bits().hash(&mut hasher);
                    }
                }
                if let Some(offset) = &svg.stroke_dashoffset {
                    5u8.hash(&mut hasher);
                    offset.to_bits().hash(&mut hasher);
                }
                if let Some(ref path_data) = svg.svg_path_data {
                    6u8.hash(&mut hasher);
                    path_data.hash(&mut hasher);
                }
                // Hash per-tag style overrides
                if !svg.tag_overrides.is_empty() {
                    7u8.hash(&mut hasher);
                    // Sort keys for deterministic hashing
                    let mut keys: Vec<&String> = svg.tag_overrides.keys().collect();
                    keys.sort();
                    for key in keys {
                        key.hash(&mut hasher);
                        if let Some(ts) = svg.tag_overrides.get(key) {
                            if let Some(f) = &ts.fill {
                                for v in f {
                                    v.to_bits().hash(&mut hasher);
                                }
                            }
                            if let Some(s) = &ts.stroke {
                                for v in s {
                                    v.to_bits().hash(&mut hasher);
                                }
                            }
                            if let Some(sw) = &ts.stroke_width {
                                sw.to_bits().hash(&mut hasher);
                            }
                            if let Some(op) = &ts.opacity {
                                op.to_bits().hash(&mut hasher);
                            }
                        }
                    }
                }
                hasher.finish()
            };

            // Check atlas first — skip string manipulation entirely on cache hit
            if self.svg_atlas.get(cache_key).is_none() {
                // Cache miss: build SVG source with inline attribute overrides
                let has_overrides = svg.tint.is_some()
                    || svg.fill.is_some()
                    || svg.stroke.is_some()
                    || svg.stroke_width.is_some()
                    || svg.stroke_dasharray.is_some()
                    || svg.stroke_dashoffset.is_some()
                    || svg.svg_path_data.is_some()
                    || !svg.tag_overrides.is_empty();

                fn color_val(c: blinc_core::Color) -> String {
                    if c.a < 1.0 {
                        format!(
                            "rgba({},{},{},{})",
                            (c.r * 255.0) as u8,
                            (c.g * 255.0) as u8,
                            (c.b * 255.0) as u8,
                            c.a
                        )
                    } else {
                        format!(
                            "#{:02x}{:02x}{:02x}",
                            (c.r * 255.0) as u8,
                            (c.g * 255.0) as u8,
                            (c.b * 255.0) as u8
                        )
                    }
                }

                let effective_source = if has_overrides {
                    // Build attribute string to inject into the root <svg> tag
                    let mut svg_attrs = String::new();
                    if let Some(fill) = svg.fill {
                        svg_attrs.push_str(&format!(r#" fill="{}""#, color_val(fill)));
                    }
                    if let Some(stroke) = svg.stroke {
                        svg_attrs.push_str(&format!(r#" stroke="{}""#, color_val(stroke)));
                    }
                    if let Some(sw) = svg.stroke_width {
                        svg_attrs.push_str(&format!(r#" stroke-width="{}""#, sw));
                    }
                    if let Some(ref da) = svg.stroke_dasharray {
                        let da_str = da
                            .iter()
                            .map(|v| v.to_string())
                            .collect::<Vec<_>>()
                            .join(",");
                        svg_attrs.push_str(&format!(r#" stroke-dasharray="{}""#, da_str));
                    }
                    if let Some(offset) = svg.stroke_dashoffset {
                        svg_attrs.push_str(&format!(r#" stroke-dashoffset="{}""#, offset));
                    }

                    // Strip existing attribute from a tag region in the SVG string.
                    fn strip_attr(s: &mut String, tag_start: usize, tag_end: usize, attr: &str) {
                        let region = &s[tag_start..tag_end];
                        let attr_eq = format!("{}=", attr);
                        if let Some(attr_offset) = region.find(&attr_eq) {
                            let abs_attr = tag_start + attr_offset;
                            let after_eq = abs_attr + attr.len() + 1;
                            if after_eq < s.len() {
                                let quote = s.as_bytes()[after_eq];
                                if quote == b'"' || quote == b'\'' {
                                    if let Some(end_quote) = s[after_eq + 1..].find(quote as char) {
                                        let remove_end = after_eq + 1 + end_quote + 1;
                                        let remove_start =
                                            if abs_attr > 0 && s.as_bytes()[abs_attr - 1] == b' ' {
                                                abs_attr - 1
                                            } else {
                                                abs_attr
                                            };
                                        s.replace_range(remove_start..remove_end, "");
                                    }
                                }
                            }
                        }
                    }

                    let mut modified = String::from(&*svg.source);

                    // Strip existing attributes from the <svg> tag
                    if let Some(svg_close) = modified.find('>') {
                        if svg.stroke.is_some() {
                            strip_attr(&mut modified, 0, svg_close, "stroke");
                        }
                        if svg.fill.is_some() {
                            let svg_close = modified.find('>').unwrap_or(0);
                            strip_attr(&mut modified, 0, svg_close, "fill");
                        }
                        if svg.stroke_width.is_some() {
                            let svg_close = modified.find('>').unwrap_or(0);
                            strip_attr(&mut modified, 0, svg_close, "stroke-width");
                        }
                        if svg.stroke_dasharray.is_some() {
                            let svg_close = modified.find('>').unwrap_or(0);
                            strip_attr(&mut modified, 0, svg_close, "stroke-dasharray");
                        }
                        if svg.stroke_dashoffset.is_some() {
                            let svg_close = modified.find('>').unwrap_or(0);
                            strip_attr(&mut modified, 0, svg_close, "stroke-dashoffset");
                        }
                    }

                    // Insert new attributes into the opening <svg tag
                    if !svg_attrs.is_empty() {
                        if let Some(pos) = modified.find('>') {
                            let insert_pos = if pos > 0 && modified.as_bytes()[pos - 1] == b'/' {
                                pos - 1
                            } else {
                                pos
                            };
                            modified.insert_str(insert_pos, &svg_attrs);
                        }
                    }

                    // Override fill/stroke on individual shape elements
                    let shape_tags = [
                        "<path",
                        "<circle",
                        "<rect",
                        "<polygon",
                        "<line",
                        "<ellipse",
                        "<polyline",
                    ];
                    for tag in &shape_tags {
                        let tag_name = tag.trim_start_matches('<');
                        let tag_style = svg.tag_overrides.get(tag_name);

                        // Per-tag overrides take priority over global element-level overrides
                        let effective_fill: Option<blinc_core::Color> = tag_style
                            .and_then(|ts| ts.fill)
                            .map(|c| blinc_core::Color::rgba(c[0], c[1], c[2], c[3]))
                            .or(svg.fill);
                        let effective_stroke: Option<blinc_core::Color> = tag_style
                            .and_then(|ts| ts.stroke)
                            .map(|c| blinc_core::Color::rgba(c[0], c[1], c[2], c[3]))
                            .or(svg.stroke);
                        let effective_stroke_width: Option<f32> = tag_style
                            .and_then(|ts| ts.stroke_width)
                            .or(svg.stroke_width);
                        let effective_dasharray: Option<Vec<f32>> = tag_style
                            .and_then(|ts| ts.stroke_dasharray.clone())
                            .or_else(|| svg.stroke_dasharray.clone());
                        let effective_dashoffset: Option<f32> = tag_style
                            .and_then(|ts| ts.stroke_dashoffset)
                            .or(svg.stroke_dashoffset);
                        let effective_opacity: Option<f32> = tag_style.and_then(|ts| ts.opacity);

                        let mut search_from = 0;
                        while let Some(tag_start) = modified[search_from..].find(tag) {
                            let abs_tag = search_from + tag_start;
                            let abs_start = abs_tag + tag.len();
                            if let Some(close) = modified[abs_start..].find('>') {
                                let abs_close = abs_start + close;

                                if effective_stroke.is_some() {
                                    strip_attr(&mut modified, abs_tag, abs_close, "stroke-width");
                                    let new_close = abs_start
                                        + modified[abs_start..].find('>').unwrap_or(close);
                                    strip_attr(&mut modified, abs_tag, new_close, "stroke");
                                }
                                if effective_fill.is_some() {
                                    let new_close = abs_start
                                        + modified[abs_start..].find('>').unwrap_or(close);
                                    strip_attr(&mut modified, abs_tag, new_close, "fill");
                                }
                                if effective_stroke_width.is_some() {
                                    let new_close = abs_start
                                        + modified[abs_start..].find('>').unwrap_or(close);
                                    strip_attr(&mut modified, abs_tag, new_close, "stroke-width");
                                }
                                if effective_dasharray.is_some() {
                                    let new_close = abs_start
                                        + modified[abs_start..].find('>').unwrap_or(close);
                                    strip_attr(
                                        &mut modified,
                                        abs_tag,
                                        new_close,
                                        "stroke-dasharray",
                                    );
                                }
                                if effective_dashoffset.is_some() {
                                    let new_close = abs_start
                                        + modified[abs_start..].find('>').unwrap_or(close);
                                    strip_attr(
                                        &mut modified,
                                        abs_tag,
                                        new_close,
                                        "stroke-dashoffset",
                                    );
                                }
                                if effective_opacity.is_some() {
                                    let new_close = abs_start
                                        + modified[abs_start..].find('>').unwrap_or(close);
                                    strip_attr(&mut modified, abs_tag, new_close, "opacity");
                                }
                                if svg.svg_path_data.is_some() && *tag == "<path" {
                                    let new_close = abs_start
                                        + modified[abs_start..].find('>').unwrap_or(close);
                                    strip_attr(&mut modified, abs_tag, new_close, "d");
                                }

                                // Recompute close position after stripping
                                let abs_close =
                                    abs_start + modified[abs_start..].find('>').unwrap_or(0);
                                let is_self_close =
                                    abs_close > 0 && modified.as_bytes()[abs_close - 1] == b'/';
                                let insert_at = if is_self_close {
                                    abs_close - 1
                                } else {
                                    abs_close
                                };
                                let mut elem_attrs = String::new();
                                if let Some(fill) = effective_fill {
                                    elem_attrs.push_str(&format!(r#" fill="{}""#, color_val(fill)));
                                }
                                if let Some(stroke) = effective_stroke {
                                    elem_attrs
                                        .push_str(&format!(r#" stroke="{}""#, color_val(stroke)));
                                }
                                if let Some(sw) = effective_stroke_width {
                                    elem_attrs.push_str(&format!(r#" stroke-width="{}""#, sw));
                                }
                                if let Some(ref da) = effective_dasharray {
                                    let da_str = da
                                        .iter()
                                        .map(|v| v.to_string())
                                        .collect::<Vec<_>>()
                                        .join(",");
                                    elem_attrs
                                        .push_str(&format!(r#" stroke-dasharray="{}""#, da_str));
                                }
                                if let Some(offset) = effective_dashoffset {
                                    elem_attrs
                                        .push_str(&format!(r#" stroke-dashoffset="{}""#, offset));
                                }
                                if let Some(opacity) = effective_opacity {
                                    elem_attrs.push_str(&format!(r#" opacity="{}""#, opacity));
                                }
                                if let Some(ref path_data) = svg.svg_path_data {
                                    if *tag == "<path" {
                                        elem_attrs.push_str(&format!(r#" d="{}""#, path_data));
                                    }
                                }
                                modified.insert_str(insert_at, &elem_attrs);
                                search_from = insert_at + elem_attrs.len() + 1;
                            } else {
                                break;
                            }
                        }
                    }

                    std::borrow::Cow::Owned(modified)
                } else {
                    std::borrow::Cow::Borrowed(&*svg.source)
                };

                // Resolve currentColor references in SVG source.
                // For tintable SVGs: rasterize as white — color applied via shader tint.
                // For non-tintable: replace with actual tint color for CPU rasterization.
                // For SVGs that have a tint but no currentColor at all (e.g.
                // hard-coded `fill="white"`), the post-rasterize `apply_tint`
                // path below handles it instead.
                let has_current_color = effective_source.contains("currentColor");
                let needs_post_raster_tint =
                    !is_tintable && svg.tint.is_some() && !has_current_color;
                let final_source = if is_tintable {
                    std::borrow::Cow::Owned(effective_source.replace("currentColor", "#ffffff"))
                } else if let Some(tint) = svg.tint {
                    if has_current_color {
                        std::borrow::Cow::Owned(
                            effective_source.replace("currentColor", &color_val(tint)),
                        )
                    } else {
                        effective_source
                    }
                } else {
                    effective_source
                };

                let rasterized =
                    RasterizedSvg::from_str(&final_source, raster_width, raster_height);

                let mut rasterized = match rasterized {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("Failed to rasterize SVG: {}", e);
                        continue;
                    }
                };

                // When a tint color is set but the SVG source doesn't
                // use `currentColor` (e.g. hard-coded `fill="white"`),
                // the currentColor replacement above was a no-op and
                // the rasterized pixels still carry the original fill.
                // Apply the tint as a post-rasterization color replace:
                // every non-transparent pixel gets its RGB replaced
                // with the tint color while preserving the original
                // alpha. This makes `.color(Color::RED)` work on any
                // SVG regardless of how its fills are authored.
                if needs_post_raster_tint {
                    rasterized.apply_tint(svg.tint.unwrap());
                }

                // Insert into atlas (handles grow/clear internally)
                if self
                    .svg_atlas
                    .insert(
                        cache_key,
                        rasterized.width,
                        rasterized.height,
                        rasterized.data(),
                        &self.device,
                    )
                    .is_none()
                {
                    tracing::warn!(
                        "SVG atlas full, could not allocate {}x{}",
                        raster_width,
                        raster_height
                    );
                    continue;
                }
            }

            // Get the atlas region for this SVG
            let Some(region) = self.svg_atlas.get(cache_key) else {
                continue;
            };
            let src_uv = region.uv_bounds(self.svg_atlas.width(), self.svg_atlas.height());
            self.svg_atlas.mark_used(cache_key);

            // Apply CSS affine transform to SVG bounds if present.
            // Pass full 2x2 affine to shader for rotation, scale, and skew support.
            let (draw_x, draw_y, draw_w, draw_h, ta, tb, tc, td) =
                if let Some([a, b, c, d, tx, ty]) = svg.css_affine {
                    // DPI-scale the translation components
                    let tx_s = tx * scale_factor;
                    let ty_s = ty * scale_factor;

                    // Transform center through the affine (in screen space)
                    let cx = svg.x + svg.width * 0.5;
                    let cy = svg.y + svg.height * 0.5;
                    let new_cx = a * cx + c * cy + tx_s;
                    let new_cy = b * cx + d * cy + ty_s;

                    // Pass original bounds — the 2x2 transform is applied in the shader
                    (
                        new_cx - svg.width * 0.5,
                        new_cy - svg.height * 0.5,
                        svg.width,
                        svg.height,
                        a,
                        b,
                        c,
                        d,
                    )
                } else {
                    (svg.x, svg.y, svg.width, svg.height, 1.0, 0.0, 0.0, 1.0)
                };

            // Create instance with atlas UV coordinates
            let mut instance = GpuImageInstance::new(draw_x, draw_y, draw_w, draw_h)
                .with_src_uv(src_uv[0], src_uv[1], src_uv[2], src_uv[3])
                .with_opacity(svg.motion_opacity)
                .with_transform(ta, tb, tc, td);

            // For tintable SVGs, apply color via shader tint multiplication
            // (white texture * tint = correctly colored output)
            if is_tintable {
                if let Some(tint) = svg.tint {
                    instance = instance.with_tint(tint.r, tint.g, tint.b, tint.a);
                }
            }

            // Apply clip bounds if specified
            if let Some([clip_x, clip_y, clip_w, clip_h]) = svg.clip_bounds {
                instance = instance.with_clip_rect(clip_x, clip_y, clip_w, clip_h);
            }

            instances.push(instance);
        }

        // Upload atlas to GPU if dirty, then batch-render all SVG instances
        if !instances.is_empty() {
            self.svg_atlas.upload(&self.queue);
            self.renderer
                .render_images(target, self.svg_atlas.view(), &instances);
        }
    }

    /// Collect text, SVG, and image elements from the render tree
    fn collect_render_elements(
        &mut self,
        tree: &RenderTree,
    ) -> (
        Vec<TextElement>,
        Vec<SvgElement>,
        Vec<ImageElement>,
        Vec<FlowElement>,
    ) {
        self.collect_render_elements_with_state(tree, None)
    }

    /// Collect text, SVG, and image elements with motion state
    fn collect_render_elements_with_state(
        &mut self,
        tree: &RenderTree,
        render_state: Option<&blinc_layout::RenderState>,
    ) -> (
        Vec<TextElement>,
        Vec<SvgElement>,
        Vec<ImageElement>,
        Vec<FlowElement>,
    ) {
        // Reuse scratch buffers - take them, clear, populate, and return
        // On next call they'll be reallocated if not returned
        let mut texts = std::mem::take(&mut self.scratch_texts);
        let mut svgs = std::mem::take(&mut self.scratch_svgs);
        let mut images = std::mem::take(&mut self.scratch_images);
        let mut flows = Vec::new();
        texts.clear();
        svgs.clear();
        images.clear();

        // Get the scale factor from the tree for DPI scaling
        let scale = tree.scale_factor();

        if let Some(root) = tree.root() {
            let mut z_layer = 0u32;
            self.collect_elements_recursive(
                tree,
                root,
                (0.0, 0.0),
                false,      // inside_glass
                false,      // inside_foreground
                None,       // No initial clip bounds
                None,       // No initial clip radius
                1.0,        // Initial motion opacity
                (0.0, 0.0), // Initial motion translate offset
                (1.0, 1.0), // Initial motion scale
                None,       // No initial motion scale center
                render_state,
                scale,
                &mut z_layer,
                &mut texts,
                &mut svgs,
                &mut images,
                &mut flows,
                None, // No initial CSS transform
                1.0,  // Initial inherited CSS opacity
                None, // No parent node
                None, // No initial scroll clip
                None, // No 3D layer ancestor
            );
        }

        // Sort texts by z_index (z_layer) to ensure correct rendering order with primitives
        texts.sort_by_key(|t| t.z_index);

        (texts, svgs, images, flows)
    }

    #[allow(clippy::too_many_arguments, clippy::only_used_in_recursion)]
    fn collect_elements_recursive(
        &self,
        tree: &RenderTree,
        node: LayoutNodeId,
        parent_offset: (f32, f32),
        inside_glass: bool,
        inside_foreground: bool,
        current_clip: Option<[f32; 4]>,
        current_clip_radius: Option<[f32; 4]>,
        inherited_motion_opacity: f32,
        inherited_motion_translate: (f32, f32),
        inherited_motion_scale: (f32, f32),
        // Center point for motion scale (in layout coordinates, before DPI scaling)
        // When a parent has motion scale, children should scale around the parent's center
        inherited_motion_scale_center: Option<(f32, f32)>,
        render_state: Option<&blinc_layout::RenderState>,
        scale: f32,
        z_layer: &mut u32,
        texts: &mut Vec<TextElement>,
        svgs: &mut Vec<SvgElement>,
        images: &mut Vec<ImageElement>,
        flows: &mut Vec<FlowElement>,
        // Accumulated CSS transform from ancestors as a 6-element affine [a,b,c,d,tx,ty]
        // in layout coordinates. Maps pre-transform coords to post-transform visual coords.
        inherited_css_affine: Option<[f32; 6]>,
        // Accumulated CSS opacity from ancestors (compounds multiplicatively).
        // CSS `opacity` applies to the element and its entire visual subtree.
        inherited_css_opacity: f32,
        // Parent node ID for inheriting non-cascading CSS props (border, shadow, filter)
        // to child images that render separately from the SDF pipeline.
        parent_node: Option<LayoutNodeId>,
        // Scroll container clip — sharp rect kept separate from the primary rounded clip.
        // This prevents corner radius morphing when a rounded element (card) is partially
        // scrolled past a sharp scroll boundary.
        current_scroll_clip: Option<[f32; 4]>,
        // 3D layer info if inside a perspective-transformed ancestor.
        // Text/SVGs/images inside 3D layers are rendered to offscreen textures
        // and blitted with the same perspective transform.
        inside_3d_layer: Option<Transform3DLayerInfo>,
    ) {
        use blinc_layout::Material;

        // Use animated bounds if this node has layout animation, otherwise use layout bounds
        // This ensures children are positioned correctly during layout animation transitions
        let Some(bounds) = tree.get_render_bounds(node, parent_offset) else {
            return;
        };

        let abs_x = bounds.x;
        let abs_y = bounds.y;

        // Get motion values for this node from RenderState (entry/exit animations)
        let motion_values = render_state.and_then(|rs| {
            // Try stable motion first, then node-based
            if let Some(render_node) = tree.get_render_node(node) {
                if let Some(ref stable_key) = render_node.props.motion_stable_id {
                    return rs.get_stable_motion_values(stable_key);
                }
            }
            rs.get_motion_values(node)
        });

        // Get motion bindings from RenderTree (continuous AnimatedValue animations)
        // NOTE: binding_transform (translate) is NOT added to effective_motion_translate
        // because it's already included in new_offset for child positioning (see line ~1250).
        // Only RenderState motion values need to be inherited through effective_motion_translate.
        let binding_scale = tree.get_motion_scale(node);
        let binding_opacity = tree.get_motion_opacity(node);

        // Calculate motion opacity for this node (combine both sources)
        let node_motion_opacity = motion_values
            .and_then(|m| m.opacity)
            .unwrap_or_else(|| binding_opacity.unwrap_or(1.0));

        // Get motion translate for this node from RenderState only
        // (binding translate is handled via new_offset in recursive calls)
        let node_motion_translate = motion_values
            .map(|m| m.resolved_translate())
            .unwrap_or((0.0, 0.0));

        // Get motion scale for this node from RenderState
        let node_motion_scale = motion_values
            .map(|m| m.resolved_scale())
            .unwrap_or((1.0, 1.0));

        // Combine with binding scale
        let binding_scale_values = binding_scale.unwrap_or((1.0, 1.0));

        // Combine with inherited values
        // NOTE: effective_motion_translate only includes RenderState motion values,
        // NOT binding transforms (which are already in the position via new_offset)
        let effective_motion_opacity = inherited_motion_opacity * node_motion_opacity;
        let effective_motion_translate = (
            inherited_motion_translate.0 + node_motion_translate.0,
            inherited_motion_translate.1 + node_motion_translate.1,
        );
        // Scale compounds multiplicatively (including binding scale)
        let effective_motion_scale = (
            inherited_motion_scale.0 * node_motion_scale.0 * binding_scale_values.0,
            inherited_motion_scale.1 * node_motion_scale.1 * binding_scale_values.1,
        );

        // Determine the motion scale center for children
        // If this node has motion scale (from RenderState or binding), use its center as the scale center
        // Otherwise, inherit the parent's scale center
        let this_node_has_scale = (node_motion_scale.0 - 1.0).abs() > 0.001
            || (node_motion_scale.1 - 1.0).abs() > 0.001
            || (binding_scale_values.0 - 1.0).abs() > 0.001
            || (binding_scale_values.1 - 1.0).abs() > 0.001;

        let effective_motion_scale_center = if this_node_has_scale {
            // This node has motion scale - compute its center in absolute layout coordinates
            let center_x = abs_x + bounds.width / 2.0;
            let center_y = abs_y + bounds.height / 2.0;
            Some((center_x, center_y))
        } else {
            // No scale on this node - inherit the parent's scale center
            inherited_motion_scale_center
        };

        // Skip if completely transparent
        if effective_motion_opacity <= 0.001 {
            return;
        }

        // CSS visibility: hidden — skip rendering but preserve layout space
        if let Some(render_node) = tree.get_render_node(node) {
            if !render_node.props.visible {
                return;
            }
        }

        // Determine if this node is a glass element
        let is_glass = tree
            .get_render_node(node)
            .map(|n| matches!(n.props.material, Some(Material::Glass(_))))
            .unwrap_or(false);

        // Track if children should be considered inside glass
        let children_inside_glass = inside_glass || is_glass;

        // Track if we're inside a foreground-layer element
        let is_foreground_node = tree
            .get_render_node(node)
            .map(|n| n.props.layer == RenderLayer::Foreground)
            .unwrap_or(false);
        let children_inside_foreground = inside_foreground || is_foreground_node;

        // Check if this node clips its children (e.g., scroll containers)
        let clips_content = tree
            .get_render_node(node)
            .map(|n| n.props.clips_content)
            .unwrap_or(false);

        // Check if this node has an active layout animation (also needs clipping)
        // Layout animations need to clip children to animated bounds
        let has_layout_animation = tree.is_layout_animating(node);

        // Check if this is a Stack layer - if so, increment z_layer for proper z-ordering
        let is_stack_layer = tree
            .get_render_node(node)
            .map(|n| n.props.is_stack_layer)
            .unwrap_or(false);
        if is_stack_layer {
            *z_layer += 1;
        }

        // Apply CSS z-index to z_layer for stacking order
        let saved_z_layer = *z_layer;
        let node_z_index = tree
            .get_render_node(node)
            .map(|n| n.props.z_index)
            .unwrap_or(0);
        if node_z_index > 0 {
            *z_layer = node_z_index as u32;
        }

        // Update clip bounds for children if this node clips (either via clips_content or layout animation)
        // When a node clips, we INTERSECT its bounds with any existing clip
        // This ensures nested clipping works correctly (inner clips can't expand outer clips)
        let should_clip = clips_content || has_layout_animation;
        let (child_clip, child_clip_radius, child_scroll_clip) = if should_clip {
            // For layout animation, use animated bounds for clipping
            // This ensures content is clipped to the animating size during transition
            let clip_bounds = if has_layout_animation {
                // Get animated bounds - these are the interpolated bounds during animation
                tree.get_render_bounds(node, parent_offset)
                    .map(|b| [b.x, b.y, b.width, b.height])
                    .unwrap_or([abs_x, abs_y, bounds.width, bounds.height])
            } else {
                [abs_x, abs_y, bounds.width, bounds.height]
            };
            // Inset clip by border-width only.  Per CSS spec, overflow clips
            // at the padding box (inside border, but padding area is visible).
            // Padding affects layout positioning, not clipping.
            let bw = tree
                .get_render_node(node)
                .map(|n| n.props.border_width)
                .unwrap_or(0.0);
            let this_clip = [
                clip_bounds[0] + bw,
                clip_bounds[1] + bw,
                (clip_bounds[2] - bw * 2.0).max(0.0),
                (clip_bounds[3] - bw * 2.0).max(0.0),
            ];

            // Extract border radius from this node for rounded clipping.
            // Inner corner radius = max(outer_radius − border_width, 0)
            let this_clip_radius = tree.get_render_node(node).map(|n| {
                let r = &n.props.border_radius;
                [
                    (r.top_left - bw).max(0.0),
                    (r.top_right - bw).max(0.0),
                    (r.bottom_right - bw).max(0.0),
                    (r.bottom_left - bw).max(0.0),
                ]
            });

            let this_has_radius = this_clip_radius
                .map(|r| r.iter().any(|&v| v > 0.5))
                .unwrap_or(false);
            let parent_has_radius = current_clip_radius
                .map(|r| r.iter().any(|&v| v > 0.5))
                .unwrap_or(false);

            if let Some(parent_clip) = current_clip {
                if this_has_radius && !parent_has_radius {
                    // This node is rounded (card), parent is sharp (scroll container).
                    // Keep them separate to avoid SDF radius clamping/morphing.
                    // Primary clip = this node's rounded clip (full card bounds).
                    // Scroll clip = parent's sharp clip intersected with any existing scroll clip.
                    (
                        Some(this_clip),
                        this_clip_radius,
                        merge_scroll_clip(parent_clip, current_scroll_clip),
                    )
                } else if !this_has_radius && parent_has_radius {
                    // This node is sharp (scroll), parent is rounded (card).
                    // Keep parent as primary rounded clip, this as scroll clip
                    // intersected with any existing scroll clip.
                    (
                        current_clip,
                        current_clip_radius,
                        merge_scroll_clip(this_clip, current_scroll_clip),
                    )
                } else {
                    // Both have same kind of radius — intersect normally.
                    let x1 = parent_clip[0].max(this_clip[0]);
                    let y1 = parent_clip[1].max(this_clip[1]);
                    let parent_right = parent_clip[0] + parent_clip[2];
                    let parent_bottom = parent_clip[1] + parent_clip[3];
                    let this_right = this_clip[0] + this_clip[2];
                    let this_bottom = this_clip[1] + this_clip[3];
                    let x2 = parent_right.min(this_right);
                    let y2 = parent_bottom.min(this_bottom);
                    let w = (x2 - x1).max(0.0);
                    let h = (y2 - y1).max(0.0);
                    let clip = Some([x1, y1, w, h]);

                    let child_r = this_clip_radius.unwrap_or([0.0; 4]);
                    let parent_r = current_clip_radius.unwrap_or([0.0; 4]);
                    let radius = Some([
                        child_r[0].max(parent_r[0]),
                        child_r[1].max(parent_r[1]),
                        child_r[2].max(parent_r[2]),
                        child_r[3].max(parent_r[3]),
                    ]);

                    (clip, radius, current_scroll_clip)
                }
            } else {
                // No parent clip — this is the first clip level.
                if this_has_radius {
                    // Rounded clip becomes primary, scroll clip passes through.
                    (Some(this_clip), this_clip_radius, current_scroll_clip)
                } else {
                    // Sharp clip becomes scroll clip; intersect with existing scroll clip
                    // so nested sharp clips (scroll + stack wrapper) don't lose the outer boundary.
                    let new_scroll_clip = if let Some(existing) = current_scroll_clip {
                        let x1 = existing[0].max(this_clip[0]);
                        let y1 = existing[1].max(this_clip[1]);
                        let x2 = (existing[0] + existing[2]).min(this_clip[0] + this_clip[2]);
                        let y2 = (existing[1] + existing[3]).min(this_clip[1] + this_clip[3]);
                        [x1, y1, (x2 - x1).max(0.0), (y2 - y1).max(0.0)]
                    } else {
                        this_clip
                    };
                    (None, None, Some(new_scroll_clip))
                }
            }
        } else {
            (current_clip, current_clip_radius, current_scroll_clip)
        };

        // Compute this node's CSS affine: compose its own CSS transform with inherited.
        // This must happen BEFORE the element-type match block so that SVGs, text, and images
        // get their own transform applied (not just the parent's inherited transform).
        // NOTE: 3D rotations (rotate-x/rotate-y/perspective) are NOT included here — they
        // can't be accurately represented as a 2D affine (perspective is projective, not linear).
        // Proper 3D text compositing requires layer-based rendering (render to texture, then
        // apply 3D transform to the composite). For now, text stays flat under 3D parents.
        let node_css_affine = if let Some(render_node) = tree.get_render_node(node) {
            let has_non_identity = if let Some(blinc_core::Transform::Affine2D(affine)) =
                &render_node.props.transform
            {
                let [a, b, c, d, tx, ty] = affine.elements;
                !((a - 1.0).abs() < 0.0001
                    && b.abs() < 0.0001
                    && c.abs() < 0.0001
                    && (d - 1.0).abs() < 0.0001
                    && tx.abs() < 0.0001
                    && ty.abs() < 0.0001)
            } else {
                false
            };

            if has_non_identity {
                let affine = match &render_node.props.transform {
                    Some(blinc_core::Transform::Affine2D(a)) => a.elements,
                    _ => unreachable!(),
                };
                let [a, b, c, d, tx, ty] = affine;
                // Compute transform center in absolute layout coords
                let (cx, cy) = if let Some([ox_pct, oy_pct]) = render_node.props.transform_origin {
                    (
                        abs_x + bounds.width * ox_pct / 100.0,
                        abs_y + bounds.height * oy_pct / 100.0,
                    )
                } else {
                    (abs_x + bounds.width / 2.0, abs_y + bounds.height / 2.0)
                };
                // Build full 6-element affine: T(center) * [a,b,c,d,tx,ty] * T(-center)
                // = [a, b, c, d, cx*(1-a) - cy*c + tx, cy*(1-d) - cx*b + ty]
                let this_affine = [
                    a,
                    b,
                    c,
                    d,
                    cx * (1.0 - a) - cy * c + tx,
                    cy * (1.0 - d) - cx * b + ty,
                ];
                match inherited_css_affine {
                    Some(parent) => {
                        let [pa, pb, pc, pd, ptx, pty] = parent;
                        Some([
                            a * pa + c * pb,
                            b * pa + d * pb,
                            a * pc + c * pd,
                            b * pc + d * pd,
                            a * ptx + c * pty + this_affine[4],
                            b * ptx + d * pty + this_affine[5],
                        ])
                    }
                    None => Some(this_affine),
                }
            } else {
                inherited_css_affine
            }
        } else {
            inherited_css_affine
        };

        if let Some(render_node) = tree.get_render_node(node) {
            // Determine effective layer: children inside glass render in Foreground
            let effective_layer = if inside_glass && !is_glass {
                RenderLayer::Foreground
            } else if is_glass {
                RenderLayer::Glass
            } else {
                render_node.props.layer
            };

            match &render_node.element_type {
                ElementType::Text(text_data) => {
                    // Apply DPI scale factor FIRST to match shape rendering order
                    // In render_with_motion, DPI scale is pushed at root level before any other transforms
                    // So we must: scale base positions first, then apply motion transforms
                    let base_x = abs_x * scale;
                    let base_y = abs_y * scale;
                    let base_width = bounds.width * scale;
                    let base_height = bounds.height * scale;

                    // Scale motion translate by DPI factor (motion values are in layout coordinates)
                    let scaled_motion_tx = effective_motion_translate.0 * scale;
                    let scaled_motion_ty = effective_motion_translate.1 * scale;

                    // Apply motion scale and translation
                    // When there's a motion scale center (from parent Motion container),
                    // we must scale around THAT center, not the text element's own center.
                    // This matches how shapes are rendered - the scale transform is pushed
                    // at the Motion container level and affects all children relative to
                    // the container's center.
                    let (scaled_x, scaled_y, scaled_width, scaled_height) =
                        if let Some((motion_center_x, motion_center_y)) =
                            effective_motion_scale_center
                        {
                            // Scale position around the motion container's center (in DPI-scaled coordinates)
                            let motion_center_x_scaled = motion_center_x * scale;
                            let motion_center_y_scaled = motion_center_y * scale;

                            // Calculate position relative to motion center
                            let rel_x = base_x - motion_center_x_scaled;
                            let rel_y = base_y - motion_center_y_scaled;

                            // Apply scale to relative position and size
                            let scaled_rel_x = rel_x * effective_motion_scale.0;
                            let scaled_rel_y = rel_y * effective_motion_scale.1;
                            let scaled_w = base_width * effective_motion_scale.0;
                            let scaled_h = base_height * effective_motion_scale.1;

                            // Apply motion translation and convert back to absolute position
                            let final_x = motion_center_x_scaled + scaled_rel_x + scaled_motion_tx;
                            let final_y = motion_center_y_scaled + scaled_rel_y + scaled_motion_ty;

                            (final_x, final_y, scaled_w, scaled_h)
                        } else {
                            // No motion scale center - just apply translation (no scale effect)
                            let final_x = base_x + scaled_motion_tx;
                            let final_y = base_y + scaled_motion_ty;
                            (final_x, final_y, base_width, base_height)
                        };

                    // Use CSS-overridden font size if available (from stylesheet/animation/transition)
                    let base_font_size = render_node.props.font_size.unwrap_or(text_data.font_size);
                    let scaled_font_size = base_font_size * effective_motion_scale.1 * scale;
                    let scaled_measured_width =
                        text_data.measured_width * effective_motion_scale.0 * scale;

                    // Intersect primary clip with scroll clip — text only supports
                    // a single clip rect so we must merge both boundaries.
                    let effective_clip = effective_single_clip(current_clip, current_scroll_clip);
                    let scaled_clip = effective_clip
                        .map(|[cx, cy, cw, ch]| [cx * scale, cy * scale, cw * scale, ch * scale]);

                    // Log motion values if non-trivial (for debugging text/shape sync issues)
                    if effective_motion_translate.0.abs() > 0.1
                        || effective_motion_translate.1.abs() > 0.1
                        || (effective_motion_scale.0 - 1.0).abs() > 0.01
                        || (effective_motion_scale.1 - 1.0).abs() > 0.01
                    {
                        tracing::trace!(
                            "Text '{}': motion_translate=({:.1}, {:.1}), motion_scale=({:.2}, {:.2}), base=({:.1}, {:.1}), final=({:.1}, {:.1})",
                            text_data.content,
                            effective_motion_translate.0,
                            effective_motion_translate.1,
                            effective_motion_scale.0,
                            effective_motion_scale.1,
                            base_x,
                            base_y,
                            scaled_x,
                            scaled_y,
                        );
                    }
                    tracing::trace!(
                        "Text '{}': abs=({:.1}, {:.1}), size=({:.1}x{:.1}), font={:.1}, align={:?}, v_align={:?}, z_layer={}",
                        text_data.content,
                        scaled_x,
                        scaled_y,
                        scaled_width,
                        scaled_height,
                        scaled_font_size,
                        text_data.align,
                        text_data.v_align,
                        *z_layer
                    );

                    // Apply text-overflow: ellipsis truncation if needed.
                    // Check both text_data.wrap (set at build time) and render_node.props.white_space
                    // (set by CSS after build). CSS white-space: nowrap overrides the builder wrap setting.
                    let is_nowrap = !text_data.wrap
                        || matches!(
                            render_node.props.white_space,
                            Some(blinc_layout::element_style::WhiteSpace::Nowrap)
                                | Some(blinc_layout::element_style::WhiteSpace::Pre)
                        );
                    let content = if is_nowrap
                        && matches!(
                            render_node.props.text_overflow,
                            Some(blinc_layout::element_style::TextOverflow::Ellipsis)
                        )
                        && scaled_measured_width > scaled_width
                        && scaled_width > 0.0
                    {
                        // Measure with the same options used for layout
                        let mut options = blinc_layout::text_measure::TextLayoutOptions::new();
                        options.font_name = text_data.font_family.name.clone();
                        options.generic_font = text_data.font_family.generic;
                        options.font_weight =
                            match render_node.props.font_weight.unwrap_or(text_data.weight) {
                                FontWeight::Bold => 700,
                                FontWeight::Normal => 400,
                                FontWeight::Light => 300,
                                _ => 400,
                            };
                        options.letter_spacing = render_node
                            .props
                            .letter_spacing
                            .unwrap_or(text_data.letter_spacing);

                        // Measure "..." to know reserved width
                        let ellipsis = "\u{2026}";
                        let ellipsis_w = blinc_layout::text_measure::measure_text_with_options(
                            ellipsis,
                            scaled_font_size / scale,
                            &options,
                        )
                        .width
                            * scale;
                        let target_width = scaled_width - ellipsis_w;

                        if target_width > 0.0 {
                            // Binary search for the right truncation point
                            let chars: Vec<char> = text_data.content.chars().collect();
                            let mut lo = 0usize;
                            let mut hi = chars.len();
                            while lo < hi {
                                #[allow(clippy::manual_div_ceil)]
                                let mid = (lo + hi + 1) / 2;
                                let sub: String = chars[..mid].iter().collect();
                                let w = blinc_layout::text_measure::measure_text_with_options(
                                    &sub,
                                    scaled_font_size / scale,
                                    &options,
                                )
                                .width
                                    * scale;
                                if w <= target_width {
                                    lo = mid;
                                } else {
                                    hi = mid - 1;
                                }
                            }
                            let truncated: String = chars[..lo].iter().collect();
                            format!("{}{}", truncated.trim_end(), ellipsis)
                        } else {
                            ellipsis.to_string()
                        }
                    } else {
                        text_data.content.clone()
                    };

                    texts.push(TextElement {
                        content,
                        x: scaled_x,
                        y: scaled_y,
                        width: scaled_width,
                        height: scaled_height,
                        font_size: scaled_font_size,
                        color: render_node.props.text_color.unwrap_or(text_data.color),
                        align: text_data.align,
                        weight: render_node.props.font_weight.unwrap_or(text_data.weight),
                        italic: text_data.italic,
                        v_align: text_data.v_align,
                        clip_bounds: scaled_clip,
                        motion_opacity: effective_motion_opacity
                            * render_node.props.opacity
                            * inherited_css_opacity,
                        wrap: !is_nowrap && text_data.wrap,
                        line_height: text_data.line_height,
                        measured_width: scaled_measured_width,
                        font_family: text_data.font_family.clone(),
                        word_spacing: text_data.word_spacing,
                        letter_spacing: render_node
                            .props
                            .letter_spacing
                            .unwrap_or(text_data.letter_spacing),
                        z_index: *z_layer,
                        ascender: text_data.ascender * effective_motion_scale.1 * scale,
                        strikethrough: render_node.props.text_decoration.map_or(
                            text_data.strikethrough,
                            |td| {
                                matches!(
                                    td,
                                    blinc_layout::element_style::TextDecoration::LineThrough
                                )
                            },
                        ),
                        underline: render_node.props.text_decoration.map_or(
                            text_data.underline,
                            |td| {
                                matches!(td, blinc_layout::element_style::TextDecoration::Underline)
                            },
                        ),
                        decoration_color: render_node.props.text_decoration_color,
                        decoration_thickness: render_node.props.text_decoration_thickness,
                        css_affine: node_css_affine,
                        text_shadow: render_node.props.text_shadow,
                        transform_3d_layer: inside_3d_layer.clone(),
                        is_foreground: children_inside_foreground,
                    });
                }
                ElementType::Svg(svg_data) => {
                    // Apply DPI scale factor FIRST to match shape rendering order
                    let base_x = abs_x * scale;
                    let base_y = abs_y * scale;
                    let base_width = bounds.width * scale;
                    let base_height = bounds.height * scale;

                    // Scale motion translate by DPI factor
                    let scaled_motion_tx = effective_motion_translate.0 * scale;
                    let scaled_motion_ty = effective_motion_translate.1 * scale;

                    // Apply motion scale and translation (same logic as Text)
                    let (scaled_x, scaled_y, scaled_width, scaled_height) =
                        if let Some((motion_center_x, motion_center_y)) =
                            effective_motion_scale_center
                        {
                            let motion_center_x_scaled = motion_center_x * scale;
                            let motion_center_y_scaled = motion_center_y * scale;

                            let rel_x = base_x - motion_center_x_scaled;
                            let rel_y = base_y - motion_center_y_scaled;

                            let scaled_rel_x = rel_x * effective_motion_scale.0;
                            let scaled_rel_y = rel_y * effective_motion_scale.1;
                            let scaled_w = base_width * effective_motion_scale.0;
                            let scaled_h = base_height * effective_motion_scale.1;

                            let final_x = motion_center_x_scaled + scaled_rel_x + scaled_motion_tx;
                            let final_y = motion_center_y_scaled + scaled_rel_y + scaled_motion_ty;

                            (final_x, final_y, scaled_w, scaled_h)
                        } else {
                            let final_x = base_x + scaled_motion_tx;
                            let final_y = base_y + scaled_motion_ty;
                            (final_x, final_y, base_width, base_height)
                        };

                    // Intersect primary clip with scroll clip — text/SVG only support
                    // a single clip rect so we must merge both boundaries.
                    let effective_clip = effective_single_clip(current_clip, current_scroll_clip);
                    let scaled_clip = effective_clip
                        .map(|[cx, cy, cw, ch]| [cx * scale, cy * scale, cw * scale, ch * scale]);

                    // Tint resolves `currentColor` references in SVG source.
                    // CSS fill/stroke are explicit overrides injected as SVG attributes.
                    // Both can coexist: tint handles currentColor, CSS handles specifics.
                    svgs.push(SvgElement {
                        source: svg_data.source.clone(),
                        x: scaled_x,
                        y: scaled_y,
                        width: scaled_width,
                        height: scaled_height,
                        tint: svg_data.tint.or_else(|| {
                            render_node
                                .props
                                .text_color
                                .map(|c| blinc_core::Color::rgba(c[0], c[1], c[2], c[3]))
                        }),
                        fill: render_node
                            .props
                            .fill
                            .map(|c| blinc_core::Color::rgba(c[0], c[1], c[2], c[3]))
                            .or(svg_data.fill),
                        stroke: render_node
                            .props
                            .stroke
                            .map(|c| blinc_core::Color::rgba(c[0], c[1], c[2], c[3]))
                            .or(svg_data.stroke),
                        stroke_width: render_node.props.stroke_width.or(svg_data.stroke_width),
                        stroke_dasharray: render_node.props.stroke_dasharray.clone(),
                        stroke_dashoffset: render_node.props.stroke_dashoffset,
                        svg_path_data: render_node.props.svg_path_data.clone(),
                        clip_bounds: scaled_clip,
                        motion_opacity: effective_motion_opacity
                            * render_node.props.opacity
                            * inherited_css_opacity,
                        css_affine: node_css_affine,
                        tag_overrides: render_node.props.svg_tag_styles.clone(),
                        transform_3d_layer: inside_3d_layer.clone(),
                    });
                }
                ElementType::Image(image_data) => {
                    // Apply DPI scale factor to image positions and sizes
                    let scaled_clip = current_clip
                        .map(|[cx, cy, cw, ch]| [cx * scale, cy * scale, cw * scale, ch * scale]);

                    // Scale clip radius by DPI factor (radius values are in layout coordinates)
                    let scaled_clip_radius = current_clip_radius
                        .map(|[tl, tr, br, bl]| [tl * scale, tr * scale, br * scale, bl * scale])
                        .unwrap_or([0.0; 4]);

                    // Scale scroll clip by DPI factor
                    let scaled_scroll_clip = current_scroll_clip
                        .map(|[cx, cy, cw, ch]| [cx * scale, cy * scale, cw * scale, ch * scale]);

                    // Look up parent render props for CSS property inheritance.
                    // Images render via a separate pipeline and don't inherit parent CSS
                    // properties automatically — we must propagate them explicitly.
                    let parent_props = parent_node
                        .and_then(|pid| tree.get_render_node(pid))
                        .map(|pn| &pn.props);

                    // Opacity: own CSS opacity * inherited CSS opacity chain * builder * motion
                    let own_css_opacity = render_node.props.opacity;
                    let final_opacity = image_data.opacity
                        * own_css_opacity
                        * inherited_css_opacity
                        * effective_motion_opacity;

                    // Border-radius: prefer own CSS, then builder.
                    // Parent clip (now at content-box) handles corner rounding.
                    let own_br = render_node.props.border_radius.top_left;
                    let final_border_radius = if own_br > 0.0 {
                        own_br * scale
                    } else {
                        image_data.border_radius * scale
                    };

                    // Border: use image's own CSS border (parent border renders via SDF,
                    // visible because clip now insets by border-width)
                    let border_width = render_node.props.border_width * scale;
                    let border_color = render_node
                        .props
                        .border_color
                        .unwrap_or(blinc_core::Color::TRANSPARENT);

                    // Shadow: use image's own (parent shadow renders via SDF)
                    let shadow = render_node.props.shadow;

                    // Filter: prefer own, fall back to parent
                    let own_filter = &render_node.props.filter;
                    let parent_filter = parent_props.and_then(|p| p.filter.as_ref());
                    let effective_filter = own_filter.as_ref().or(parent_filter);
                    let filter_a = effective_filter
                        .map(|f| Self::css_filter_to_arrays(f).0)
                        .unwrap_or([0.0, 0.0, 0.0, 0.0]);
                    let filter_b = effective_filter
                        .map(|f| Self::css_filter_to_arrays(f).1)
                        .unwrap_or([1.0, 1.0, 1.0, 0.0]);

                    // object-fit / object-position: CSS overrides builder values
                    let final_object_fit = render_node
                        .props
                        .object_fit
                        .unwrap_or(image_data.object_fit);
                    let final_object_position = render_node
                        .props
                        .object_position
                        .unwrap_or(image_data.object_position);

                    // CSS overrides for lazy loading properties
                    let final_loading_strategy = render_node
                        .props
                        .loading_strategy
                        .unwrap_or(image_data.loading_strategy);
                    let final_placeholder_type = render_node
                        .props
                        .placeholder_type
                        .unwrap_or(image_data.placeholder_type);
                    let final_placeholder_color = render_node
                        .props
                        .placeholder_color
                        .unwrap_or(image_data.placeholder_color);
                    let final_placeholder_image = render_node
                        .props
                        .placeholder_image
                        .clone()
                        .or_else(|| image_data.placeholder_image.clone());
                    let final_fade_duration = render_node
                        .props
                        .fade_duration_ms
                        .unwrap_or(image_data.fade_duration_ms);

                    // Mask: prefer own, fall back to parent
                    let own_mask = render_node.props.mask_image.as_ref();
                    let parent_mask = parent_props.and_then(|p| p.mask_image.as_ref());
                    let effective_mask = own_mask.or(parent_mask);
                    let (mask_params, mask_info) = Self::mask_image_to_arrays(effective_mask);

                    images.push(ImageElement {
                        source: image_data.source.clone(),
                        x: abs_x * scale,
                        y: abs_y * scale,
                        width: bounds.width * scale,
                        height: bounds.height * scale,
                        object_fit: final_object_fit,
                        object_position: final_object_position,
                        opacity: final_opacity,
                        border_radius: final_border_radius,
                        tint: image_data.tint,
                        clip_bounds: scaled_clip,
                        clip_radius: scaled_clip_radius,
                        layer: effective_layer,
                        loading_strategy: final_loading_strategy,
                        placeholder_type: final_placeholder_type,
                        placeholder_color: final_placeholder_color,
                        placeholder_image: final_placeholder_image,
                        fade_duration_ms: final_fade_duration,
                        z_index: *z_layer,
                        border_width,
                        border_color,
                        css_affine: node_css_affine,
                        shadow,
                        filter_a,
                        filter_b,
                        scroll_clip: scaled_scroll_clip,
                        mask_params,
                        mask_info,
                        transform_3d_layer: inside_3d_layer.clone(),
                    });
                }
                // Canvas elements are rendered inline during tree traversal (in render_layer)
                ElementType::Canvas(_) => {}
                ElementType::Div => {
                    // Check if this div has a background image brush
                    if let Some(blinc_core::Brush::Image(ref img_brush)) =
                        render_node.props.background
                    {
                        let scaled_clip = current_clip.map(|[cx, cy, cw, ch]| {
                            [cx * scale, cy * scale, cw * scale, ch * scale]
                        });
                        let scaled_clip_radius = current_clip_radius
                            .map(|[tl, tr, br, bl]| {
                                [tl * scale, tr * scale, br * scale, bl * scale]
                            })
                            .unwrap_or([0.0; 4]);
                        let scaled_scroll_clip_bg = current_scroll_clip.map(|[cx, cy, cw, ch]| {
                            [cx * scale, cy * scale, cw * scale, ch * scale]
                        });

                        images.push(ImageElement {
                            source: img_brush.source.clone(),
                            x: abs_x * scale,
                            y: abs_y * scale,
                            width: bounds.width * scale,
                            height: bounds.height * scale,
                            object_fit: match img_brush.fit {
                                blinc_core::ImageFit::Cover => 0,
                                blinc_core::ImageFit::Contain => 1,
                                blinc_core::ImageFit::Fill => 2,
                                blinc_core::ImageFit::Tile => 0,
                            },
                            object_position: [img_brush.position.x, img_brush.position.y],
                            opacity: img_brush.opacity
                                * render_node.props.opacity
                                * inherited_css_opacity
                                * effective_motion_opacity,
                            border_radius: render_node.props.border_radius.top_left * scale,
                            tint: [
                                img_brush.tint.r,
                                img_brush.tint.g,
                                img_brush.tint.b,
                                img_brush.tint.a,
                            ],
                            clip_bounds: scaled_clip,
                            clip_radius: scaled_clip_radius,
                            layer: effective_layer,
                            loading_strategy: 0, // Eager
                            placeholder_type: 0, // None
                            placeholder_color: [0.0; 4],
                            placeholder_image: None,
                            fade_duration_ms: 0,
                            z_index: *z_layer,
                            border_width: 0.0,
                            border_color: blinc_core::Color::TRANSPARENT,
                            css_affine: node_css_affine,
                            shadow: render_node.props.shadow,
                            filter_a: render_node
                                .props
                                .filter
                                .as_ref()
                                .map(|f| Self::css_filter_to_arrays(f).0)
                                .unwrap_or([0.0, 0.0, 0.0, 0.0]),
                            filter_b: render_node
                                .props
                                .filter
                                .as_ref()
                                .map(|f| Self::css_filter_to_arrays(f).1)
                                .unwrap_or([1.0, 1.0, 1.0, 0.0]),
                            scroll_clip: scaled_scroll_clip_bg,
                            mask_params: {
                                let (mp, _) = Self::mask_image_to_arrays(
                                    render_node.props.mask_image.as_ref(),
                                );
                                mp
                            },
                            mask_info: {
                                let (_, mi) = Self::mask_image_to_arrays(
                                    render_node.props.mask_image.as_ref(),
                                );
                                mi
                            },
                            transform_3d_layer: inside_3d_layer.clone(),
                        });
                    }
                }
                // StyledText: render text with inline styling using multiple TextElements
                ElementType::StyledText(styled_data) => {
                    // Apply DPI scale factor first
                    let base_x = abs_x * scale;
                    let base_y = abs_y * scale;
                    let base_width = bounds.width * scale;
                    let base_height = bounds.height * scale;

                    // Scale motion translate by DPI factor
                    let scaled_motion_tx = effective_motion_translate.0 * scale;
                    let scaled_motion_ty = effective_motion_translate.1 * scale;

                    // Apply motion scale and translation (same logic as Text)
                    let (scaled_x, scaled_y, scaled_width, scaled_height) =
                        if let Some((motion_center_x, motion_center_y)) =
                            effective_motion_scale_center
                        {
                            let motion_center_x_scaled = motion_center_x * scale;
                            let motion_center_y_scaled = motion_center_y * scale;

                            let rel_x = base_x - motion_center_x_scaled;
                            let rel_y = base_y - motion_center_y_scaled;

                            let scaled_rel_x = rel_x * effective_motion_scale.0;
                            let scaled_rel_y = rel_y * effective_motion_scale.1;
                            let scaled_w = base_width * effective_motion_scale.0;
                            let scaled_h = base_height * effective_motion_scale.1;

                            let final_x = motion_center_x_scaled + scaled_rel_x + scaled_motion_tx;
                            let final_y = motion_center_y_scaled + scaled_rel_y + scaled_motion_ty;

                            (final_x, final_y, scaled_w, scaled_h)
                        } else {
                            let final_x = base_x + scaled_motion_tx;
                            let final_y = base_y + scaled_motion_ty;
                            (final_x, final_y, base_width, base_height)
                        };

                    // Use CSS-overridden font size if available (from stylesheet/animation/transition)
                    let base_styled_font_size =
                        render_node.props.font_size.unwrap_or(styled_data.font_size);
                    let scaled_font_size = base_styled_font_size * effective_motion_scale.1 * scale;
                    // Intersect primary clip with scroll clip for styled text
                    let effective_clip = effective_single_clip(current_clip, current_scroll_clip);
                    let scaled_clip = effective_clip
                        .map(|[cx, cy, cw, ch]| [cx * scale, cy * scale, cw * scale, ch * scale]);

                    // Build non-overlapping segments from potentially overlapping spans
                    // This handles nested tags like <span color="red"><b>text</b></span>
                    let content = &styled_data.content;
                    let content_len = content.len();

                    // Get default styles from element config
                    let default_bold = styled_data.weight == FontWeight::Bold;
                    let default_italic = styled_data.italic;

                    // Collect all boundary positions where style might change
                    let mut boundaries: Vec<usize> = vec![0, content_len];
                    for span in &styled_data.spans {
                        if span.start < content_len {
                            boundaries.push(span.start);
                        }
                        if span.end <= content_len {
                            boundaries.push(span.end);
                        }
                    }
                    boundaries.sort();
                    boundaries.dedup();

                    // Build segments between boundaries
                    #[allow(clippy::type_complexity)]
                    let mut segments: Vec<(
                        usize,
                        usize,
                        [f32; 4],
                        bool,
                        bool,
                        bool,
                        bool,
                    )> = Vec::new();

                    for window in boundaries.windows(2) {
                        let seg_start = window[0];
                        let seg_end = window[1];
                        if seg_start >= seg_end {
                            continue;
                        }

                        // Determine style for this segment by merging all overlapping spans
                        let mut color: Option<[f32; 4]> = None;
                        let mut bold = default_bold;
                        let mut italic = default_italic;
                        let mut underline = false;
                        let mut strikethrough = false;

                        for span in &styled_data.spans {
                            // Check if span overlaps this segment
                            if span.start <= seg_start && span.end >= seg_end {
                                // This span covers this segment - merge styles
                                if span.bold {
                                    bold = true;
                                }
                                if span.italic {
                                    italic = true;
                                }
                                if span.underline {
                                    underline = true;
                                }
                                if span.strikethrough {
                                    strikethrough = true;
                                }
                                // Use color if span has explicit color (not transparent)
                                if span.color[3] > 0.0 {
                                    color = Some(span.color);
                                }
                            }
                        }

                        // CSS text_color override takes precedence over span colors
                        let default_color = render_node
                            .props
                            .text_color
                            .unwrap_or(styled_data.default_color);
                        let final_color = color.unwrap_or(default_color);
                        segments.push((
                            seg_start,
                            seg_end,
                            final_color,
                            bold,
                            italic,
                            underline,
                            strikethrough,
                        ));
                    }

                    // Use consistent ascender from element for baseline alignment
                    let scaled_ascender = styled_data.ascender * scale;

                    // Calculate x offsets for each segment and push as TextElements
                    let mut x_offset = 0.0f32;
                    for (start, end, color, bold, italic, underline, strikethrough) in segments {
                        if start >= end || start >= content.len() {
                            continue;
                        }
                        let segment_text = &content[start..end.min(content.len())];
                        if segment_text.is_empty() {
                            continue;
                        }

                        // Measure segment width for positioning
                        let mut options = blinc_layout::text_measure::TextLayoutOptions::new();
                        options.font_name = styled_data.font_family.name.clone();
                        options.generic_font = styled_data.font_family.generic;
                        options.font_weight = if bold { 700 } else { 400 };
                        options.italic = italic;
                        let metrics = blinc_layout::text_measure::measure_text_with_options(
                            segment_text,
                            styled_data.font_size,
                            &options,
                        );
                        // Apply both DPI scale and motion scale to segment width
                        let segment_width = metrics.width * scale * effective_motion_scale.0;

                        texts.push(TextElement {
                            content: segment_text.to_string(),
                            x: scaled_x + x_offset,
                            y: scaled_y,
                            width: segment_width,
                            height: scaled_height,
                            font_size: scaled_font_size,
                            color,
                            align: TextAlign::Left, // Always left-align segments
                            weight: if bold {
                                FontWeight::Bold
                            } else {
                                FontWeight::Normal
                            },
                            italic,
                            v_align: styled_data.v_align,
                            clip_bounds: scaled_clip,
                            motion_opacity: effective_motion_opacity
                                * render_node.props.opacity
                                * inherited_css_opacity,
                            wrap: false, // Don't wrap individual segments
                            line_height: styled_data.line_height,
                            measured_width: segment_width,
                            font_family: styled_data.font_family.clone(),
                            word_spacing: 0.0,
                            letter_spacing: render_node.props.letter_spacing.unwrap_or(0.0),
                            z_index: *z_layer,
                            ascender: scaled_ascender * effective_motion_scale.1, // Scale ascender with motion
                            strikethrough,
                            underline,
                            decoration_color: render_node.props.text_decoration_color,
                            decoration_thickness: render_node.props.text_decoration_thickness,
                            css_affine: node_css_affine,
                            text_shadow: render_node.props.text_shadow,
                            transform_3d_layer: inside_3d_layer.clone(),
                            is_foreground: children_inside_foreground,
                        });

                        x_offset += segment_width;
                    }
                }
            }

            // Collect flow element if this node has a @flow shader reference.
            // Flow elements render via custom GPU pipelines instead of (or on top of) the SDF path.
            if let Some(ref flow_name) = render_node.props.flow {
                flows.push(FlowElement {
                    flow_name: flow_name.clone(),
                    flow_graph: render_node.props.flow_graph.clone(),
                    x: abs_x * scale,
                    y: abs_y * scale,
                    width: bounds.width * scale,
                    height: bounds.height * scale,
                    z_index: *z_layer,
                    corner_radius: render_node.props.border_radius.top_left * scale,
                });
            }
        }

        // Include scroll offset and motion offset when calculating child positions
        let scroll_offset = tree.get_scroll_offset(node);
        let static_motion_offset = tree
            .get_motion_transform(node)
            .map(|t| match t {
                blinc_core::Transform::Affine2D(a) => (a.elements[4], a.elements[5]),
                _ => (0.0, 0.0),
            })
            .unwrap_or((0.0, 0.0));

        let new_offset = (
            abs_x + scroll_offset.0 + static_motion_offset.0,
            abs_y + scroll_offset.1 + static_motion_offset.1,
        );

        // Compute inherited CSS opacity for children: compound this node's CSS opacity
        // CSS `opacity` applies to the element AND its visual subtree
        let child_css_opacity = if let Some(rn) = tree.get_render_node(node) {
            inherited_css_opacity * rn.props.opacity
        } else {
            inherited_css_opacity
        };

        // Detect 3D layer: if this node has rotate-x/rotate-y/perspective,
        // create a Transform3DLayerInfo for children to inherit.
        let child_3d_layer = if let Some(rn) = tree.get_render_node(node) {
            let has_3d = rn.props.rotate_x.is_some()
                || rn.props.rotate_y.is_some()
                || rn.props.perspective.is_some();
            if has_3d {
                let rx = rn.props.rotate_x.unwrap_or(0.0).to_radians();
                let ry = rn.props.rotate_y.unwrap_or(0.0).to_radians();
                let d = rn.props.perspective.unwrap_or(800.0) * scale;
                Some(Transform3DLayerInfo {
                    node_id: node,
                    layer_bounds: [
                        abs_x * scale,
                        abs_y * scale,
                        bounds.width * scale,
                        bounds.height * scale,
                    ],
                    transform_3d: blinc_core::Transform3DParams {
                        sin_rx: rx.sin(),
                        cos_rx: rx.cos(),
                        sin_ry: ry.sin(),
                        cos_ry: ry.cos(),
                        perspective_d: d,
                    },
                    opacity: rn.props.opacity,
                })
            } else {
                inside_3d_layer.clone()
            }
        } else {
            inside_3d_layer.clone()
        };

        for child_id in tree.layout().children(node) {
            self.collect_elements_recursive(
                tree,
                child_id,
                new_offset,
                children_inside_glass,
                children_inside_foreground,
                child_clip,
                child_clip_radius,
                effective_motion_opacity,
                effective_motion_translate,
                effective_motion_scale,
                effective_motion_scale_center,
                render_state,
                scale,
                z_layer,
                texts,
                svgs,
                images,
                flows,
                node_css_affine,
                child_css_opacity,
                Some(node), // pass current node as parent for children
                child_scroll_clip,
                child_3d_layer.clone(),
            );
        }

        // Restore z_layer after this subtree
        if node_z_index > 0 {
            *z_layer = saved_z_layer;
        }
    }

    /// Get device arc
    pub fn device(&self) -> &Arc<wgpu::Device> {
        &self.device
    }

    /// Get queue arc
    pub fn queue(&self) -> &Arc<wgpu::Queue> {
        &self.queue
    }

    /// Whether the GPU adapter supports storage buffers.
    /// False on WebGL2 (GL adapter) — the renderer uses data textures instead.
    pub fn has_storage_buffers(&self) -> bool {
        self.renderer.has_storage_buffers()
    }

    /// Get the shared font registry
    ///
    /// This can be used to share fonts between text measurement and rendering,
    /// ensuring consistent font loading and metrics.
    pub fn font_registry(&self) -> Arc<Mutex<FontRegistry>> {
        self.text_ctx.font_registry()
    }

    /// Get the texture format used by the renderer
    pub fn texture_format(&self) -> wgpu::TextureFormat {
        self.renderer.texture_format()
    }

    /// Create a new wgpu surface for an additional window (multi-window support)
    pub fn create_surface<W>(
        &self,
        window: Arc<W>,
    ) -> std::result::Result<wgpu::Surface<'static>, blinc_gpu::RendererError>
    where
        W: raw_window_handle::HasWindowHandle
            + raw_window_handle::HasDisplayHandle
            + Send
            + Sync
            + 'static,
    {
        self.renderer.create_surface(window)
    }

    /// Render a layout tree with dynamic render state overlays
    ///
    /// This method renders:
    /// 1. The stable RenderTree (element hierarchy and layout)
    /// 2. RenderState overlays (cursors, selections, focus rings)
    ///
    /// The RenderState overlays are drawn on top of the tree without requiring
    /// tree rebuilds. This enables smooth cursor blinking and animations.
    pub fn render_tree_with_state(
        &mut self,
        tree: &RenderTree,
        render_state: &blinc_layout::RenderState,
        width: u32,
        height: u32,
        target: &wgpu::TextureView,
    ) -> Result<()> {
        // First render the tree as normal
        self.render_tree(tree, width, height, target)?;

        // Then render overlays from RenderState
        self.render_overlays(render_state, width, height, target);

        Ok(())
    }

    /// Render a layout tree with motion animations from RenderState
    ///
    /// This method renders:
    /// 1. The RenderTree with motion animations applied (opacity, scale, translate)
    /// 2. RenderState overlays (cursors, selections, focus rings)
    ///
    /// Use this method when you have elements wrapped in motion() containers
    /// for enter/exit animations.
    pub fn render_tree_with_motion(
        &mut self,
        tree: &RenderTree,
        render_state: &blinc_layout::RenderState,
        width: u32,
        height: u32,
        target: &wgpu::TextureView,
    ) -> Result<()> {
        // Get scale factor for HiDPI rendering
        let scale_factor = tree.scale_factor();

        // Create a single paint context for all layers with text rendering support
        let mut ctx =
            GpuPaintContext::with_text_context(width as f32, height as f32, &mut self.text_ctx);

        // Render with motion animations applied (all layers to same context)
        tree.render_with_motion(&mut ctx, render_state);

        // Take the batch (mutable so CSS-transformed text primitives can be added)
        let mut batch = ctx.take_batch();

        // Take any 3D mesh draws captured via `ctx.draw_mesh_data(...)`
        // inside canvas callbacks. These are dispatched after all 2D
        // content lands so the mesh composites on top of the UI — see
        // the `render_mesh_data` dispatch loop near the end of this
        // function. Drained here (not at the dispatch site) so
        // `ctx` can drop right after `take_batch`/`take_pending_meshes`
        // and the rest of the frame runs without holding onto it.
        let pending_meshes = ctx.take_pending_meshes();

        // Collect text, SVG, image, and flow elements WITH motion state
        let (all_texts, all_svgs, all_images, flow_elements) =
            self.collect_render_elements_with_state(tree, Some(render_state));

        // Partition elements into normal (no 3D ancestor) and 3D-layer groups.
        // Elements inside a 3D-transformed parent need to be rendered to an offscreen
        // texture and blitted with the same perspective transform.
        let mut texts = Vec::new();
        let mut fg_texts = Vec::new();
        let mut layer_3d_texts: std::collections::HashMap<
            LayoutNodeId,
            (Transform3DLayerInfo, Vec<TextElement>),
        > = std::collections::HashMap::new();
        for text in all_texts {
            if let Some(ref info) = text.transform_3d_layer {
                layer_3d_texts
                    .entry(info.node_id)
                    .or_insert_with(|| (info.clone(), Vec::new()))
                    .1
                    .push(text);
            } else if text.is_foreground {
                fg_texts.push(text);
            } else {
                texts.push(text);
            }
        }

        let mut svgs = Vec::new();
        let mut layer_3d_svgs: std::collections::HashMap<LayoutNodeId, Vec<SvgElement>> =
            std::collections::HashMap::new();
        for svg in all_svgs {
            if let Some(ref info) = svg.transform_3d_layer {
                layer_3d_svgs.entry(info.node_id).or_default().push(svg);
            } else {
                svgs.push(svg);
            }
        }

        let mut images = Vec::new();
        let mut layer_3d_images: std::collections::HashMap<LayoutNodeId, Vec<ImageElement>> =
            std::collections::HashMap::new();
        for image in all_images {
            if let Some(ref info) = image.transform_3d_layer {
                layer_3d_images.entry(info.node_id).or_default().push(image);
            } else {
                images.push(image);
            }
        }

        // Collect unique 3D layer IDs for rendering
        let layer_3d_ids: Vec<LayoutNodeId> = layer_3d_texts.keys().cloned().collect();

        // Pre-load all images into cache before rendering (both normal and 3D-layer)
        self.preload_images(&images, width as f32, height as f32);
        for layer_imgs in layer_3d_images.values() {
            self.preload_images(layer_imgs, width as f32, height as f32);
        }

        // Prepare text glyphs with z_layer information
        // Store (z_layer, glyphs) to enable interleaved rendering
        let mut glyphs_by_layer: std::collections::BTreeMap<u32, Vec<GpuGlyph>> =
            std::collections::BTreeMap::new();
        let mut css_transformed_text_prims: Vec<GpuPrimitive> = Vec::new();
        for text in &texts {
            // Skip text that's completely outside its clip bounds (visibility culling)
            // This prevents loading emoji fonts for off-screen text in scroll containers
            if let Some([clip_x, clip_y, clip_w, clip_h]) = text.clip_bounds {
                let text_right = text.x + text.width;
                let text_bottom = text.y + text.height;
                let clip_right = clip_x + clip_w;
                let clip_bottom = clip_y + clip_h;

                // Check if text is completely outside clip bounds
                if text.x >= clip_right
                    || text_right <= clip_x
                    || text.y >= clip_bottom
                    || text_bottom <= clip_y
                {
                    // Text is not visible, skip rendering entirely
                    continue;
                }
            }

            let alignment = match text.align {
                TextAlign::Left => TextAlignment::Left,
                TextAlign::Center => TextAlignment::Center,
                TextAlign::Right => TextAlignment::Right,
            };

            // Apply motion opacity to text color
            let color = if text.motion_opacity < 1.0 {
                [
                    text.color[0],
                    text.color[1],
                    text.color[2],
                    text.color[3] * text.motion_opacity,
                ]
            } else {
                text.color
            };

            // Determine wrap width:
            // 1. If clip bounds exist and are smaller than measured width, use clip width
            //    (this handles scroll containers where layout width isn't constrained)
            // 2. Otherwise, if layout width is smaller than measured, use layout width
            // 3. Otherwise, don't wrap (text fits naturally)
            let effective_width = if let Some(clip) = text.clip_bounds {
                // Use clip width if it constrains the text
                clip[2].min(text.width)
            } else {
                text.width
            };

            // Wrap if effective width is significantly smaller than measured width
            let needs_wrap = text.wrap && effective_width < text.measured_width - 2.0;

            // Always pass width for alignment - the layout engine needs max_width
            // to calculate center/right alignment offsets
            let wrap_width = Some(text.width);

            // Convert font family to GPU types
            let font_name = text.font_family.name.as_deref();
            let generic = to_gpu_generic_font(text.font_family.generic);
            let font_weight = text.weight.weight();

            // Map vertical alignment to text anchor
            let (anchor, y_pos, use_layout_height) = match text.v_align {
                TextVerticalAlign::Center => {
                    (TextAnchor::Center, text.y + text.height / 2.0, false)
                }
                TextVerticalAlign::Top => (TextAnchor::Top, text.y, true),
                TextVerticalAlign::Baseline => {
                    let baseline_y = text.y + text.ascender;
                    (TextAnchor::Baseline, baseline_y, false)
                }
            };
            let layout_height = if use_layout_height {
                Some(text.height)
            } else {
                None
            };

            // Render text shadow first (behind text) if present
            if let Some(shadow) = &text.text_shadow {
                let shadow_color = [
                    shadow.color.r,
                    shadow.color.g,
                    shadow.color.b,
                    shadow.color.a * text.motion_opacity,
                ];
                let shadow_x = text.x + shadow.offset_x * scale_factor;
                let shadow_y = y_pos + shadow.offset_y * scale_factor;
                if let Ok(mut shadow_glyphs) = self.text_ctx.prepare_text_with_style(
                    &text.content,
                    shadow_x,
                    shadow_y,
                    text.font_size,
                    shadow_color,
                    anchor,
                    alignment,
                    wrap_width,
                    needs_wrap,
                    font_name,
                    generic,
                    font_weight,
                    text.italic,
                    layout_height,
                    text.letter_spacing,
                ) {
                    if let Some(clip) = text.clip_bounds {
                        for glyph in &mut shadow_glyphs {
                            glyph.clip_bounds = clip;
                        }
                    }
                    if let Some(affine) = text.css_affine {
                        let [a, b, c, d, tx, ty] = affine;
                        let tx_scaled = tx * scale_factor;
                        let ty_scaled = ty * scale_factor;
                        for glyph in &shadow_glyphs {
                            let gc_x = glyph.bounds[0] + glyph.bounds[2] / 2.0;
                            let gc_y = glyph.bounds[1] + glyph.bounds[3] / 2.0;
                            let new_gc_x = a * gc_x + c * gc_y + tx_scaled;
                            let new_gc_y = b * gc_x + d * gc_y + ty_scaled;
                            let mut prim = GpuPrimitive::from_glyph(glyph);
                            prim.bounds = [
                                new_gc_x - glyph.bounds[2] / 2.0,
                                new_gc_y - glyph.bounds[3] / 2.0,
                                glyph.bounds[2],
                                glyph.bounds[3],
                            ];
                            prim.local_affine = [a, b, c, d];
                            prim.set_z_layer(text.z_index);
                            css_transformed_text_prims.push(prim);
                        }
                    } else {
                        glyphs_by_layer
                            .entry(text.z_index)
                            .or_default()
                            .extend(shadow_glyphs);
                    }
                }
            }

            match self.text_ctx.prepare_text_with_style(
                &text.content,
                text.x,
                y_pos,
                text.font_size,
                color,
                anchor,
                alignment,
                wrap_width,
                needs_wrap,
                font_name,
                generic,
                font_weight,
                text.italic,
                layout_height,
                text.letter_spacing,
            ) {
                Ok(mut glyphs) => {
                    tracing::trace!(
                        "render_tree_with_motion: prepared {} glyphs for '{}' (font={:?})",
                        glyphs.len(),
                        text.content,
                        font_name
                    );
                    // Apply clip bounds if present
                    if let Some(clip) = text.clip_bounds {
                        for glyph in &mut glyphs {
                            glyph.clip_bounds = clip;
                        }
                    }

                    if let Some(affine) = text.css_affine {
                        // CSS-transformed text: convert glyphs to SDF primitives with local_affine
                        let [a, b, c, d, tx, ty] = affine;
                        let tx_scaled = tx * scale_factor;
                        let ty_scaled = ty * scale_factor;
                        for glyph in &glyphs {
                            // Transform glyph center through the affine
                            let gc_x = glyph.bounds[0] + glyph.bounds[2] / 2.0;
                            let gc_y = glyph.bounds[1] + glyph.bounds[3] / 2.0;
                            let new_gc_x = a * gc_x + c * gc_y + tx_scaled;
                            let new_gc_y = b * gc_x + d * gc_y + ty_scaled;
                            let mut prim = GpuPrimitive::from_glyph(glyph);
                            prim.bounds = [
                                new_gc_x - glyph.bounds[2] / 2.0,
                                new_gc_y - glyph.bounds[3] / 2.0,
                                glyph.bounds[2],
                                glyph.bounds[3],
                            ];
                            prim.local_affine = [a, b, c, d];
                            prim.set_z_layer(text.z_index);
                            css_transformed_text_prims.push(prim);
                        }
                    } else {
                        // Normal text: add to glyph pipeline
                        glyphs_by_layer
                            .entry(text.z_index)
                            .or_default()
                            .extend(glyphs);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "render_tree_with_motion: failed to prepare text '{}': {:?}",
                        text.content,
                        e
                    );
                }
            }
        }

        // Prepare foreground text glyphs (rendered after foreground primitives)
        let mut fg_glyphs: Vec<GpuGlyph> = Vec::new();
        for text in &fg_texts {
            if let Some([clip_x, clip_y, clip_w, clip_h]) = text.clip_bounds {
                let text_right = text.x + text.width;
                let text_bottom = text.y + text.height;
                let clip_right = clip_x + clip_w;
                let clip_bottom = clip_y + clip_h;
                if text.x >= clip_right
                    || text_right <= clip_x
                    || text.y >= clip_bottom
                    || text_bottom <= clip_y
                {
                    continue;
                }
            }

            let alignment = match text.align {
                TextAlign::Left => TextAlignment::Left,
                TextAlign::Center => TextAlignment::Center,
                TextAlign::Right => TextAlignment::Right,
            };

            let color = if text.motion_opacity < 1.0 {
                [
                    text.color[0],
                    text.color[1],
                    text.color[2],
                    text.color[3] * text.motion_opacity,
                ]
            } else {
                text.color
            };

            let effective_width = if let Some(clip) = text.clip_bounds {
                clip[2].min(text.width)
            } else {
                text.width
            };
            let needs_wrap = text.wrap && effective_width < text.measured_width - 2.0;
            let wrap_width = Some(text.width);
            let font_name = text.font_family.name.as_deref();
            let generic = to_gpu_generic_font(text.font_family.generic);
            let font_weight = text.weight.weight();

            let (anchor, y_pos, use_layout_height) = match text.v_align {
                TextVerticalAlign::Center => {
                    (TextAnchor::Center, text.y + text.height / 2.0, false)
                }
                TextVerticalAlign::Top => (TextAnchor::Top, text.y, true),
                TextVerticalAlign::Baseline => {
                    let baseline_y = text.y + text.ascender;
                    (TextAnchor::Baseline, baseline_y, false)
                }
            };
            let layout_height = if use_layout_height {
                Some(text.height)
            } else {
                None
            };

            if let Ok(mut glyphs) = self.text_ctx.prepare_text_with_style(
                &text.content,
                text.x,
                y_pos,
                text.font_size,
                color,
                anchor,
                alignment,
                wrap_width,
                needs_wrap,
                font_name,
                generic,
                font_weight,
                text.italic,
                layout_height,
                text.letter_spacing,
            ) {
                if let Some(clip) = text.clip_bounds {
                    for glyph in &mut glyphs {
                        glyph.clip_bounds = clip;
                    }
                }
                fg_glyphs.extend(glyphs);
            }
        }

        // Generate decoration primitives for foreground text once so the
        // three render paths below can each render them after their
        // `render_text(target, &fg_glyphs)` call. Without this, any
        // strikethrough / underline on a `.foreground()` element is
        // silently dropped.
        let fg_decorations_by_layer = generate_text_decoration_primitives_by_layer(&fg_texts);

        tracing::trace!(
            "render_tree_with_motion: {} texts, {} fg texts, {} z-layers with glyphs, {} css-transformed",
            texts.len(),
            fg_texts.len(),
            glyphs_by_layer.len(),
            css_transformed_text_prims.len()
        );

        // SVGs are rendered as rasterized images (not tessellated paths) for better anti-aliasing
        // They will be rendered later via render_rasterized_svgs

        self.renderer.resize(width, height);

        // Bind the real glyph atlas to the SDF pipeline whenever
        // it's available — `set_glyph_atlas` no-ops on pointer
        // equality so calling each frame is free. CSS-transformed
        // text + canvas `draw_text` calls both land glyph-sourced
        // primitives in `batch.primitives`, and the SDF pipeline
        // needs the real atlas bound to sample them; the old
        // "only when CSS text exists" guard silently swallowed
        // canvas text that reached this path any other way.
        if let (Some(atlas), Some(color_atlas)) =
            (self.text_ctx.atlas_view(), self.text_ctx.color_atlas_view())
        {
            if !css_transformed_text_prims.is_empty() {
                batch.primitives.append(&mut css_transformed_text_prims);
            }
            self.renderer.set_glyph_atlas(atlas, color_atlas);
        }

        let has_glass = batch.glass_count() > 0;
        let has_layer_effects_in_batch = batch.has_layer_effects();

        // Only allocate glass textures when glass is actually used
        if has_glass {
            self.ensure_glass_textures(width, height);
        }
        let use_msaa_overlay = self.sample_count > 1;

        if has_glass {
            // Glass path with layer effects support
            let (bg_images, fg_images): (Vec<_>, Vec<_>) = images
                .iter()
                .partition(|img| img.layer == RenderLayer::Background);

            // Pre-render background images to both backdrop and target so glass can blur them
            let has_bg_images = !bg_images.is_empty();
            if has_bg_images {
                let backdrop_tex = self.backdrop_texture.take().unwrap();
                self.renderer
                    .clear_target(&backdrop_tex.view, wgpu::Color::TRANSPARENT);
                self.renderer.clear_target(
                    target,
                    wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: self.clear_alpha as f64,
                    },
                );
                self.render_images_ref(&backdrop_tex.view, &bg_images);
                self.render_images_ref(target, &bg_images);
                self.backdrop_texture = Some(backdrop_tex);
            }

            if has_layer_effects_in_batch {
                // When we have layer effects, we need a more complex render path:
                // 1. Render backdrop for glass blur sampling (with pre-rendered images if any)
                // 2. Use render_with_clear which handles layer effects
                // 3. Render background images to target (after clear, before glass)
                // 4. Render glass primitives on top
                {
                    let backdrop = self.backdrop_texture.as_ref().unwrap();
                    self.renderer.render_to_backdrop(
                        &backdrop.view,
                        (backdrop.width, backdrop.height),
                        &batch,
                        has_bg_images,
                    );
                }

                // Then use render_with_clear which handles layer effects
                self.renderer.render_with_clear(
                    target,
                    &batch,
                    [0.0, 0.0, 0.0, self.clear_alpha as f64],
                );

                // Render dynamic images (video frames from draw_rgba_pixels)
                if !batch.dynamic_images.is_empty() {
                    self.renderer
                        .render_dynamic_images(target, &batch.dynamic_images);
                }

                // Render background images to target after clear (so they're visible behind glass)
                if has_bg_images {
                    self.render_images_ref(target, &bg_images);
                }

                // Finally render glass primitives on top
                if batch.glass_count() > 0 {
                    let backdrop = self.backdrop_texture.as_ref().unwrap();
                    self.renderer.render_glass(target, &backdrop.view, &batch);
                }
            } else {
                // No layer effects, use optimized glass frame rendering
                let backdrop = self.backdrop_texture.as_ref().unwrap();
                self.renderer.render_glass_frame(
                    target,
                    &backdrop.view,
                    (backdrop.width, backdrop.height),
                    &batch,
                    has_bg_images,
                );
            }

            // Render paths with MSAA for smooth edges on curved shapes like notch
            // (render_glass_frame uses 1x sampled path rendering)
            if use_msaa_overlay && batch.has_paths() {
                self.renderer
                    .render_paths_overlay_msaa(target, &batch, self.sample_count);
            }

            // Render remaining bg images (only if not already pre-rendered for glass)
            if !has_bg_images {
                self.render_images_ref(target, &bg_images);
            }
            self.render_images_ref(target, &fg_images);

            // Interleaved z-layer rendering for proper text z-ordering in glass path
            let max_z = batch.max_z_layer();
            let max_text_z = glyphs_by_layer.keys().cloned().max().unwrap_or(0);
            let decorations_by_layer = generate_text_decoration_primitives_by_layer(&texts);
            let max_decoration_z = decorations_by_layer.keys().cloned().max().unwrap_or(0);
            let max_glass_layer = max_z.max(max_text_z).max(max_decoration_z);

            // Render z=0 text first (before any z>0 primitives)
            {
                let mut scratch = std::mem::take(&mut self.scratch_glyphs);
                scratch.clear();
                if let Some(glyphs) = glyphs_by_layer.get(&0) {
                    scratch.extend_from_slice(glyphs);
                }
                if !scratch.is_empty() {
                    self.render_text(target, &scratch);
                }
                self.scratch_glyphs = scratch;
            }
            self.render_text_decorations_for_layer(target, &decorations_by_layer, 0);

            if max_glass_layer > 0 {
                let effect_indices = batch.effect_layer_indices();
                for z in 1..=max_glass_layer {
                    // Render primitives for this layer
                    let layer_primitives = if effect_indices.is_empty() {
                        batch.primitives_for_layer(z)
                    } else {
                        batch.primitives_for_layer_excluding_effects(z, &effect_indices)
                    };
                    if !layer_primitives.is_empty() {
                        self.renderer
                            .render_primitives_overlay(target, &layer_primitives);
                    }

                    // Render text for this layer (interleaved for proper z-order)
                    {
                        let mut scratch = std::mem::take(&mut self.scratch_glyphs);
                        scratch.clear();
                        if let Some(glyphs) = glyphs_by_layer.get(&z) {
                            scratch.extend_from_slice(glyphs);
                        }
                        if !scratch.is_empty() {
                            self.render_text(target, &scratch);
                        }
                        self.scratch_glyphs = scratch;
                    }
                    self.render_text_decorations_for_layer(target, &decorations_by_layer, z);
                }
            }

            // Render SVGs as rasterized images for high-quality anti-aliasing
            if !svgs.is_empty() {
                self.render_rasterized_svgs(target, &svgs, scale_factor);
            }

            // Render foreground text (inside foreground-layer elements, after everything else)
            if !fg_glyphs.is_empty() {
                self.render_text(target, &fg_glyphs);
            }
            // Render foreground text decorations (strikethrough / underline)
            // for every z-layer present in the foreground decoration index.
            for &z in fg_decorations_by_layer.keys() {
                self.render_text_decorations_for_layer(target, &fg_decorations_by_layer, z);
            }
        } else {
            // Simple path (no glass)
            // Pre-generate text decorations grouped by layer for interleaved rendering
            let decorations_by_layer = generate_text_decoration_primitives_by_layer(&texts);

            let max_z = batch.max_z_layer();
            let max_text_z = glyphs_by_layer.keys().cloned().max().unwrap_or(0);
            let max_decoration_z = decorations_by_layer.keys().cloned().max().unwrap_or(0);
            let max_layer = max_z.max(max_text_z).max(max_decoration_z);
            let has_layer_effects = batch.has_layer_effects();

            if max_layer > 0 && !has_layer_effects {
                // Interleaved z-layer rendering for proper Stack z-ordering
                // Group images by z_index for interleaved rendering
                let mut images_by_layer: std::collections::BTreeMap<u32, Vec<&ImageElement>> =
                    std::collections::BTreeMap::new();
                for img in &images {
                    images_by_layer.entry(img.z_index).or_default().push(img);
                }
                let max_image_z = images_by_layer.keys().cloned().max().unwrap_or(0);
                let max_layer = max_layer.max(max_image_z);

                // First pass: render z_layer=0 primitives with clear
                let z0_primitives = batch.primitives_for_layer(0);
                // Create a temporary batch for z=0 (include paths - they don't have z-layer support)
                let mut z0_batch = PrimitiveBatch::new();
                z0_batch.primitives = z0_primitives;
                z0_batch.paths = batch.paths.clone();
                self.renderer.render_with_clear(
                    target,
                    &z0_batch,
                    [0.0, 0.0, 0.0, self.clear_alpha as f64],
                );

                // Render dynamic images (video frames)
                if !batch.dynamic_images.is_empty() {
                    self.renderer
                        .render_dynamic_images(target, &batch.dynamic_images);
                }

                // Render paths with MSAA for smooth edges on curved shapes like notch
                if use_msaa_overlay && z0_batch.has_paths() {
                    self.renderer
                        .render_paths_overlay_msaa(target, &z0_batch, self.sample_count);
                }

                // Render z=0 images
                if let Some(z0_images) = images_by_layer.get(&0) {
                    self.render_images_ref(target, z0_images);
                }

                // Render z=0 text (must render before z=1 primitives for proper z-ordering)
                if let Some(glyphs) = glyphs_by_layer.get(&0) {
                    if !glyphs.is_empty() {
                        self.render_text(target, glyphs);
                    }
                }
                self.render_text_decorations_for_layer(target, &decorations_by_layer, 0);

                // Render subsequent layers interleaved (primitives, images, text per layer)
                for z in 1..=max_layer {
                    // Render primitives for this layer. MSAA route for
                    // silhouette smoothing on stacked vector content
                    // (scroll overlays, stacked components, etc.). The
                    // z=0 batch still flows through `render_with_clear`
                    // below — wiring that one up takes a different
                    // helper because it fuses clearing with drawing.
                    let layer_primitives = batch.primitives_for_layer(z);
                    if !layer_primitives.is_empty() {
                        if use_msaa_overlay {
                            self.renderer.render_primitives_overlay_msaa(
                                target,
                                &layer_primitives,
                                self.sample_count,
                            );
                        } else {
                            self.renderer
                                .render_primitives_overlay(target, &layer_primitives);
                        }
                    }

                    // Render images for this layer
                    if let Some(layer_images) = images_by_layer.get(&z) {
                        self.render_images_ref(target, layer_images);
                    }

                    // Render text for this layer (interleaved with primitives for proper z-order)
                    if let Some(glyphs) = glyphs_by_layer.get(&z) {
                        if !glyphs.is_empty() {
                            self.render_text(target, glyphs);
                        }
                    }
                    self.render_text_decorations_for_layer(target, &decorations_by_layer, z);
                }

                // Render SVGs as rasterized images for high-quality anti-aliasing
                if !svgs.is_empty() {
                    self.render_rasterized_svgs(target, &svgs, scale_factor);
                }

                // Render foreground primitives (e.g. borders on top)
                if !batch.foreground_primitives.is_empty() {
                    self.renderer
                        .render_primitives_overlay(target, &batch.foreground_primitives);
                }

                // Render foreground text (inside foreground-layer elements, after foreground primitives)
                if !fg_glyphs.is_empty() {
                    self.render_text(target, &fg_glyphs);
                }
                for &z in fg_decorations_by_layer.keys() {
                    self.render_text_decorations_for_layer(target, &fg_decorations_by_layer, z);
                }
            } else {
                // Fast path: render the full batch through
                // `render_with_clear`, which dispatches into
                // `render_with_layer_effects` when the batch carries
                // any `LayerCommand::Push { effects: !empty }` and
                // falls through to the simple SDF path otherwise.
                //
                // This branch used to flip between
                // `render_overlay_msaa` (when no layer effects) and
                // `render_with_clear` (when effects were present),
                // which gave mesh primitives hardware-coverage AA on
                // the no-effect frames. The flip ran per frame, so an
                // animated effect — e.g. ruffle's `breathe-blur`
                // sweeping radius 0 → 32 → 0 — toggled the rendering
                // path twice per cycle. Mesh primitives' silhouette AA
                // shifts subtly between hardware-MSAA and the SDF
                // shader's barycentric ramp, and the toggle reads as
                // a flash on the affected node. Pinning the path to
                // `render_with_clear` keeps the visual stable, and
                // the path overlay below still applies hardware MSAA
                // to actual `Path` geometry. Mesh primitives lose the
                // hardware-coverage refinement on no-effect frames,
                // but the shader-side edge AA they fall back to is
                // already 1 px wide and is the same path that runs
                // when effects DO exist — no per-frame discontinuity.
                self.renderer.render_with_clear(
                    target,
                    &batch,
                    [0.0, 0.0, 0.0, self.clear_alpha as f64],
                );

                // Render dynamic images (video frames)
                if !batch.dynamic_images.is_empty() {
                    self.renderer
                        .render_dynamic_images(target, &batch.dynamic_images);
                }

                // Path overlay MSAA. The main pass above is always
                // single-sampled now, so this runs whenever MSAA is
                // configured — no more conditional skip for the
                // (previously) MSAA main path.
                if batch.has_paths() && self.sample_count > 1 {
                    self.renderer
                        .render_paths_overlay_msaa(target, &batch, self.sample_count);
                }

                self.render_images(target, &images, width as f32, height as f32, scale_factor);

                // Render foreground primitives (e.g. borders on top)
                if !batch.foreground_primitives.is_empty() {
                    self.renderer
                        .render_primitives_overlay(target, &batch.foreground_primitives);
                }

                // Render SVGs as rasterized images for high-quality anti-aliasing
                if !svgs.is_empty() {
                    self.render_rasterized_svgs(target, &svgs, scale_factor);
                }

                // Interleaved z-layer rendering for proper text z-ordering
                // Render z=0 text before any z>0 primitive overlays
                if let Some(glyphs) = glyphs_by_layer.get(&0) {
                    if !glyphs.is_empty() {
                        self.render_text(target, glyphs);
                    }
                }
                self.render_text_decorations_for_layer(target, &decorations_by_layer, 0);

                if max_layer > 0 {
                    let effect_indices = batch.effect_layer_indices();
                    for z in 1..=max_layer {
                        // Render primitives for this z-layer
                        let layer_primitives = if effect_indices.is_empty() {
                            batch.primitives_for_layer(z)
                        } else {
                            batch.primitives_for_layer_excluding_effects(z, &effect_indices)
                        };
                        if !layer_primitives.is_empty() {
                            self.renderer
                                .render_primitives_overlay(target, &layer_primitives);
                        }

                        // Render text for this z-layer (interleaved for proper z-order)
                        if let Some(glyphs) = glyphs_by_layer.get(&z) {
                            if !glyphs.is_empty() {
                                self.render_text(target, glyphs);
                            }
                        }
                        self.render_text_decorations_for_layer(target, &decorations_by_layer, z);
                    }
                }

                // Render foreground text (inside foreground-layer elements, after all z-layers)
                if !fg_glyphs.is_empty() {
                    self.render_text(target, &fg_glyphs);
                }
                for &z in fg_decorations_by_layer.keys() {
                    self.render_text_decorations_for_layer(target, &fg_decorations_by_layer, z);
                }
            }
        }

        // Render 3D-layer text/SVGs/images: for each 3D layer group, render to an
        // offscreen texture and blit with the same perspective transform as the parent.
        for layer_id in &layer_3d_ids {
            if let Some((info, layer_texts)) = layer_3d_texts.get(layer_id) {
                let layer_svgs_vec = layer_3d_svgs.get(layer_id);
                let layer_images_vec = layer_3d_images.get(layer_id);
                self.render_3d_layer_elements(
                    target,
                    info,
                    layer_texts,
                    layer_svgs_vec.map(|v| v.as_slice()).unwrap_or(&[]),
                    layer_images_vec.map(|v| v.as_slice()).unwrap_or(&[]),
                    scale_factor,
                );
            }
        }

        // Render @flow shader elements on top of their SDF base
        self.has_active_flows = !flow_elements.is_empty();
        if !flow_elements.is_empty() {
            let stylesheet = tree.stylesheet();

            // Use monotonic time for smooth animation
            static START_TIME: std::sync::OnceLock<web_time::Instant> = std::sync::OnceLock::new();
            let start = START_TIME.get_or_init(web_time::Instant::now);
            let elapsed_secs = start.elapsed().as_secs_f32();

            for flow_el in &flow_elements {
                // Resolve FlowGraph: direct graph first, then stylesheet lookup
                let graph = flow_el
                    .flow_graph
                    .as_deref()
                    .or_else(|| stylesheet.and_then(|s| s.get_flow(&flow_el.flow_name)));

                if let Some(graph) = graph {
                    // Compile on first use (no-op if already cached)
                    if let Err(e) = self.renderer.flow_pipeline_cache().compile(graph) {
                        tracing::warn!("@flow '{}' compile error: {}", flow_el.flow_name, e);
                        continue;
                    }

                    let uniforms = blinc_gpu::FlowUniformData {
                        viewport_size: [width as f32, height as f32],
                        time: elapsed_secs,
                        frame_index: 0.0, // TODO: track frame counter
                        element_bounds: [flow_el.x, flow_el.y, flow_el.width, flow_el.height],
                        pointer: [
                            (self.cursor_pos[0] - flow_el.x) / flow_el.width.max(1.0),
                            (self.cursor_pos[1] - flow_el.y) / flow_el.height.max(1.0),
                        ],
                        corner_radius: flow_el.corner_radius,
                        _padding: 0.0,
                    };

                    let viewport = [flow_el.x, flow_el.y, flow_el.width, flow_el.height];
                    if !self.renderer.render_flow(
                        target,
                        &flow_el.flow_name,
                        &uniforms,
                        Some(viewport),
                    ) {
                        tracing::warn!("@flow '{}' render failed", flow_el.flow_name);
                    }
                }
            }
        }

        // Poll the device to free completed command buffers
        self.renderer.poll();

        // Dispatch 3D mesh draws captured during `tree.render_with_motion`.
        // Each `PendingMesh` carries a snapshot of the camera and lights
        // active when `canvas(|ctx, bounds| ctx.draw_mesh_data(...))` fired,
        // so the mesh pipeline renders at the correct pose even if the
        // closure's camera was transient. View-projection is computed
        // from the captured camera + the actual frame viewport so aspect
        // stays correct under window resizes.
        //
        // MVP scope: meshes render to the full frame target (no scissor
        // to the canvas bounds yet), composite on top of the 2D UI, and
        // sit under `render_overlays` so overlay panels still clip
        // cleanly over them. Per-canvas viewport clipping is a
        // follow-up once the first end-to-end demo proves the path.
        if !pending_meshes.is_empty() {
            dispatch_pending_meshes(&mut self.renderer, target, width, height, &pending_meshes);
        }

        // Render overlays from RenderState
        self.render_overlays(render_state, width, height, target);

        // Render debug visualization if enabled (BLINC_DEBUG=text|layout|all)
        let debug = DebugMode::from_env();
        if debug.text {
            self.render_text_debug(target, &texts);
        }
        if debug.layout {
            let scale = tree.scale_factor();
            self.render_layout_debug(target, tree, scale);
        }
        if debug.motion {
            self.render_motion_debug(target, tree, width, height);
        }

        // Return scratch buffers for reuse on next frame
        self.return_scratch_elements(texts, svgs, images);

        // Periodic cache stats (every ~5s at 60fps)
        self.log_cache_stats();

        Ok(())
    }

    /// Render 3D-layer text/SVGs/images to an offscreen texture and blit with perspective.
    ///
    /// Elements inside a parent with `perspective` + `rotate-x`/`rotate-y` need to be
    /// rendered to a temporary offscreen texture and then blitted with the same perspective
    /// transform so they visually tilt with their parent's 3D transform.
    fn render_3d_layer_elements(
        &mut self,
        target: &wgpu::TextureView,
        info: &Transform3DLayerInfo,
        texts: &[TextElement],
        svgs: &[SvgElement],
        images: &[ImageElement],
        scale_factor: f32,
    ) {
        let [lx, ly, lw, lh] = info.layer_bounds;
        if lw <= 0.0 || lh <= 0.0 {
            return;
        }

        let tex_w = (lw.ceil() as u32).max(1);
        let tex_h = (lh.ceil() as u32).max(1);

        // Acquire offscreen texture
        let layer_tex = self.renderer.acquire_layer_texture((tex_w, tex_h), false);
        self.renderer
            .clear_target(&layer_tex.view, wgpu::Color::TRANSPARENT);

        // Set viewport to offscreen texture size
        self.renderer.set_viewport_override((tex_w, tex_h));

        // Render offset text glyphs
        if !texts.is_empty() {
            let mut layer_glyphs: Vec<GpuGlyph> = Vec::new();
            for text in texts {
                let alignment = match text.align {
                    TextAlign::Left => TextAlignment::Left,
                    TextAlign::Center => TextAlignment::Center,
                    TextAlign::Right => TextAlignment::Right,
                };

                let color = if text.motion_opacity < 1.0 {
                    [
                        text.color[0],
                        text.color[1],
                        text.color[2],
                        text.color[3] * text.motion_opacity,
                    ]
                } else {
                    text.color
                };

                let effective_width = if let Some(clip) = text.clip_bounds {
                    clip[2].min(text.width)
                } else {
                    text.width
                };
                let needs_wrap = text.wrap && effective_width < text.measured_width - 2.0;
                let wrap_width = Some(text.width);
                let font_name = text.font_family.name.as_deref();
                let generic = to_gpu_generic_font(text.font_family.generic);
                let font_weight = text.weight.weight();

                let (anchor, y_pos, use_layout_height) = match text.v_align {
                    TextVerticalAlign::Center => {
                        (TextAnchor::Center, text.y + text.height / 2.0, false)
                    }
                    TextVerticalAlign::Top => (TextAnchor::Top, text.y, true),
                    TextVerticalAlign::Baseline => {
                        let baseline_y = text.y + text.ascender;
                        (TextAnchor::Baseline, baseline_y, false)
                    }
                };
                let layout_height = if use_layout_height {
                    Some(text.height)
                } else {
                    None
                };

                if let Ok(mut glyphs) = self.text_ctx.prepare_text_with_style(
                    &text.content,
                    text.x - lx,
                    y_pos - ly,
                    text.font_size,
                    color,
                    anchor,
                    alignment,
                    wrap_width,
                    needs_wrap,
                    font_name,
                    generic,
                    font_weight,
                    text.italic,
                    layout_height,
                    text.letter_spacing,
                ) {
                    // Offset clip bounds to layer-local coords
                    if let Some(clip) = text.clip_bounds {
                        for glyph in &mut glyphs {
                            glyph.clip_bounds = [clip[0] - lx, clip[1] - ly, clip[2], clip[3]];
                        }
                    }
                    layer_glyphs.extend(glyphs);
                }
            }

            if !layer_glyphs.is_empty() {
                self.render_text(&layer_tex.view, &layer_glyphs);
            }
        }

        // Render offset images (mutate in place — we own these from partition)
        if !images.is_empty() {
            let mut offset_images = images.to_vec();
            for img in &mut offset_images {
                img.x -= lx;
                img.y -= ly;
                if let Some(ref mut clip) = img.clip_bounds {
                    clip[0] -= lx;
                    clip[1] -= ly;
                }
                if let Some(ref mut scroll) = img.scroll_clip {
                    scroll[0] -= lx;
                    scroll[1] -= ly;
                }
            }
            self.render_images(&layer_tex.view, &offset_images, lw, lh, scale_factor);
        }

        // Render offset SVGs (mutate in place — we own these from partition)
        if !svgs.is_empty() {
            let mut offset_svgs = svgs.to_vec();
            for svg in &mut offset_svgs {
                svg.x -= lx;
                svg.y -= ly;
                if let Some(ref mut clip) = svg.clip_bounds {
                    clip[0] -= lx;
                    clip[1] -= ly;
                }
            }
            self.render_rasterized_svgs(&layer_tex.view, &offset_svgs, scale_factor);
        }

        // Restore viewport
        self.renderer.restore_viewport();

        // Blit with perspective transform
        self.renderer.blit_tight_texture_to_target(
            &layer_tex.view,
            (tex_w, tex_h),
            target,
            (lx, ly),
            (lw, lh),
            info.opacity,
            blinc_core::BlendMode::Normal,
            None,
            Some(info.transform_3d),
        );

        self.renderer.release_layer_texture(layer_tex);
    }

    /// Render a tree on top of existing content (no clear)
    ///
    /// This is used for overlay trees (modals, toasts, dialogs) that render
    /// on top of the main UI without clearing it.
    pub fn render_overlay_tree_with_motion(
        &mut self,
        tree: &RenderTree,
        render_state: &blinc_layout::RenderState,
        width: u32,
        height: u32,
        target: &wgpu::TextureView,
    ) -> Result<()> {
        // Get scale factor for HiDPI rendering
        let scale_factor = tree.scale_factor();

        // Create a single paint context for all layers with text rendering support
        let mut ctx =
            GpuPaintContext::with_text_context(width as f32, height as f32, &mut self.text_ctx);

        // Render with motion animations applied (all layers to same context)
        tree.render_with_motion(&mut ctx, render_state);

        // Take the batch (mutable so CSS-transformed text primitives can be added)
        let mut batch = ctx.take_batch();

        // Collect text, SVG, image, and flow elements WITH motion state
        let (texts, svgs, images, _flows) =
            self.collect_render_elements_with_state(tree, Some(render_state));

        // Pre-load all images into cache before rendering
        self.preload_images(&images, width as f32, height as f32);

        // Prepare text glyphs with z_layer information
        let mut glyphs_by_layer: std::collections::BTreeMap<u32, Vec<GpuGlyph>> =
            std::collections::BTreeMap::new();
        let mut css_transformed_text_prims: Vec<GpuPrimitive> = Vec::new();
        for text in &texts {
            let alignment = match text.align {
                TextAlign::Left => TextAlignment::Left,
                TextAlign::Center => TextAlignment::Center,
                TextAlign::Right => TextAlignment::Right,
            };

            // Apply motion opacity to text color
            let color = if text.motion_opacity < 1.0 {
                [
                    text.color[0],
                    text.color[1],
                    text.color[2],
                    text.color[3] * text.motion_opacity,
                ]
            } else {
                text.color
            };

            // Determine wrap width
            let effective_width = if let Some(clip) = text.clip_bounds {
                clip[2].min(text.width)
            } else {
                text.width
            };

            let needs_wrap = text.wrap && effective_width < text.measured_width - 2.0;
            let wrap_width = Some(text.width);
            let font_name = text.font_family.name.as_deref();
            let generic = to_gpu_generic_font(text.font_family.generic);
            let font_weight = text.weight.weight();

            let (anchor, y_pos, use_layout_height) = match text.v_align {
                TextVerticalAlign::Center => {
                    (TextAnchor::Center, text.y + text.height / 2.0, false)
                }
                TextVerticalAlign::Top => (TextAnchor::Top, text.y, true),
                TextVerticalAlign::Baseline => {
                    let baseline_y = text.y + text.ascender;
                    (TextAnchor::Baseline, baseline_y, false)
                }
            };
            let layout_height = if use_layout_height {
                Some(text.height)
            } else {
                None
            };

            if let Ok(glyphs) = self.text_ctx.prepare_text_with_style(
                &text.content,
                text.x,
                y_pos,
                text.font_size,
                color,
                anchor,
                alignment,
                wrap_width,
                needs_wrap,
                font_name,
                generic,
                font_weight,
                text.italic,
                layout_height,
                text.letter_spacing,
            ) {
                let mut glyphs = glyphs;
                if let Some(clip) = text.clip_bounds {
                    for glyph in &mut glyphs {
                        glyph.clip_bounds = clip;
                    }
                }

                if let Some(affine) = text.css_affine {
                    // CSS-transformed text: convert to SDF primitives with local_affine
                    // Pushed into fg_batch.primitives to render in the main SDF pass
                    let [a, b, c, d, tx, ty] = affine;
                    let tx_scaled = tx * scale_factor;
                    let ty_scaled = ty * scale_factor;
                    for glyph in &glyphs {
                        let gc_x = glyph.bounds[0] + glyph.bounds[2] / 2.0;
                        let gc_y = glyph.bounds[1] + glyph.bounds[3] / 2.0;
                        let new_gc_x = a * gc_x + c * gc_y + tx_scaled;
                        let new_gc_y = b * gc_x + d * gc_y + ty_scaled;
                        let mut prim = GpuPrimitive::from_glyph(glyph);
                        prim.bounds = [
                            new_gc_x - glyph.bounds[2] / 2.0,
                            new_gc_y - glyph.bounds[3] / 2.0,
                            glyph.bounds[2],
                            glyph.bounds[3],
                        ];
                        prim.local_affine = [a, b, c, d];
                        prim.set_z_layer(text.z_index);
                        css_transformed_text_prims.push(prim);
                    }
                } else {
                    glyphs_by_layer
                        .entry(text.z_index)
                        .or_default()
                        .extend(glyphs);
                }
            }
        }

        // SVGs are rendered as rasterized images (not tessellated paths) for better anti-aliasing
        // They will be rendered later via render_rasterized_svgs

        self.renderer.resize(width, height);

        // Bind the real glyph atlas to the SDF pipeline whenever
        // it's available. See the same block in `render_tree` for
        // the rationale — canvas `draw_text` calls route glyph
        // primitives through `batch.primitives`, which need the
        // real atlas bound for the PRIM_TEXT shader path.
        if let (Some(atlas), Some(color_atlas)) =
            (self.text_ctx.atlas_view(), self.text_ctx.color_atlas_view())
        {
            if !css_transformed_text_prims.is_empty() {
                batch.primitives.append(&mut css_transformed_text_prims);
            }
            self.renderer.set_glyph_atlas(atlas, color_atlas);
        }

        // For overlay rendering, we DON'T have glass effects (overlays are simple)
        // Render primitives without clearing (LoadOp::Load)
        let max_z = batch.max_z_layer();
        let max_text_z = glyphs_by_layer.keys().cloned().max().unwrap_or(0);
        let max_layer = max_z.max(max_text_z);

        tracing::trace!(
            "render_overlay_tree: {} primitives, {} text layers, max_layer={}",
            batch.primitives.len(),
            glyphs_by_layer.len(),
            max_layer
        );

        // Render all layers using overlay mode (no clear)
        for z in 0..=max_layer {
            let layer_primitives = batch.primitives_for_layer(z);
            if !layer_primitives.is_empty() {
                tracing::trace!(
                    "render_overlay_tree: rendering {} primitives at z={}",
                    layer_primitives.len(),
                    z
                );
                self.renderer
                    .render_primitives_overlay(target, &layer_primitives);
            }

            if let Some(glyphs) = glyphs_by_layer.get(&z) {
                if !glyphs.is_empty() {
                    tracing::trace!(
                        "render_overlay_tree: rendering {} glyphs at z={}",
                        glyphs.len(),
                        z
                    );
                    self.render_text(target, glyphs);
                }
            }
        }

        // Images render on top
        self.render_images(target, &images, width as f32, height as f32, scale_factor);

        // Render foreground primitives (e.g. borders on top)
        if !batch.foreground_primitives.is_empty() {
            self.renderer
                .render_primitives_overlay(target, &batch.foreground_primitives);
        }

        // Poll the device to free completed command buffers
        self.renderer.poll();

        // Render layout debug for overlay tree if enabled
        let debug = DebugMode::from_env();
        if debug.layout {
            let scale = tree.scale_factor();
            self.render_layout_debug(target, tree, scale);
        }
        if debug.motion {
            self.render_motion_debug(target, tree, width, height);
        }

        // Return scratch buffers for reuse on next frame
        self.return_scratch_elements(texts, svgs, images);

        Ok(())
    }

    /// Render overlays from RenderState (cursors, selections, focus rings)
    fn render_overlays(
        &mut self,
        render_state: &blinc_layout::RenderState,
        width: u32,
        height: u32,
        target: &wgpu::TextureView,
    ) {
        let overlays = render_state.overlays();
        if overlays.is_empty() {
            return;
        }

        // Create a paint context for overlays
        let mut overlay_ctx = GpuPaintContext::new(width as f32, height as f32);

        for overlay in overlays {
            match overlay {
                Overlay::Cursor {
                    position,
                    size,
                    color,
                    opacity,
                } => {
                    if *opacity > 0.0 {
                        // Apply opacity to cursor color
                        let cursor_color =
                            Color::rgba(color.r, color.g, color.b, color.a * opacity);
                        overlay_ctx.execute_command(&DrawCommand::FillRect {
                            rect: Rect::new(position.0, position.1, size.0, size.1),
                            corner_radius: CornerRadius::default(),
                            brush: Brush::Solid(cursor_color),
                        });
                    }
                }
                Overlay::Selection { rects: _, color: _ } => {
                    // TODO: Re-enable for real-time text selection
                    // Disabled for now to avoid blue mask issue after modal close
                }
                Overlay::FocusRing {
                    position,
                    size,
                    radius,
                    color,
                    thickness,
                } => {
                    overlay_ctx.execute_command(&DrawCommand::StrokeRect {
                        rect: Rect::new(position.0, position.1, size.0, size.1),
                        corner_radius: CornerRadius::uniform(*radius),
                        stroke: Stroke::new(*thickness),
                        brush: Brush::Solid(*color),
                    });
                }
            }
        }

        // Render overlays as an overlay pass (on top of existing content)
        let overlay_batch = overlay_ctx.take_batch();
        if !overlay_batch.is_empty() {
            self.renderer.render_overlay(target, &overlay_batch);
        }
    }
}

/// Convert layout's GenericFont to GPU's GenericFont
fn to_gpu_generic_font(generic: GenericFont) -> GpuGenericFont {
    match generic {
        GenericFont::System => GpuGenericFont::System,
        GenericFont::Monospace => GpuGenericFont::Monospace,
        GenericFont::Serif => GpuGenericFont::Serif,
        GenericFont::SansSerif => GpuGenericFont::SansSerif,
    }
}

/// Debug mode flags for visual debugging
///
/// Set environment variable `BLINC_DEBUG` to enable debug visualization:
/// - `text`: Show text bounding boxes and baselines
/// - `layout`: Show all element bounding boxes (useful for debugging hit-testing)
/// - `motion`: Show active animation stats overlay
/// - `all` or `1` or `true`: Show all debug visualizations
#[derive(Clone, Copy)]
pub struct DebugMode {
    /// Show text bounding boxes and baseline indicators
    pub text: bool,
    /// Show all element bounding boxes
    pub layout: bool,
    /// Show motion/animation debug info
    pub motion: bool,
}

impl DebugMode {
    /// Check environment variable and return debug mode configuration
    pub fn from_env() -> Self {
        let debug_value = std::env::var("BLINC_DEBUG")
            .map(|v| v.to_lowercase())
            .unwrap_or_default();

        let all = debug_value == "all" || debug_value == "1" || debug_value == "true";
        let text = all || debug_value == "text";
        let layout = all || debug_value == "layout";
        let motion = all || debug_value == "motion";

        Self {
            text,
            layout,
            motion,
        }
    }

    /// Check if any debug mode is enabled
    pub fn any_enabled(&self) -> bool {
        self.text || self.layout || self.motion
    }
}

/// Generate text decoration primitives (strikethrough and underline) grouped by z-layer
///
/// Creates decoration lines for text elements that have:
/// - strikethrough: horizontal line through the middle of the text
/// - underline: horizontal line below the text baseline
///
/// Returns a HashMap of z_index -> primitives for interleaved rendering with text
fn generate_text_decoration_primitives_by_layer(
    texts: &[TextElement],
) -> std::collections::HashMap<u32, Vec<GpuPrimitive>> {
    let mut primitives_by_layer: std::collections::HashMap<u32, Vec<GpuPrimitive>> =
        std::collections::HashMap::new();

    for text in texts {
        if !text.strikethrough && !text.underline {
            continue;
        }

        // Calculate text width for decorations
        let decoration_width = if text.wrap && text.measured_width > text.width {
            text.width
        } else {
            text.measured_width.min(text.width)
        };

        // Skip if there's no meaningful width
        if decoration_width <= 0.0 {
            continue;
        }

        // Line thickness: use CSS text-decoration-thickness if set, else scale with font size
        let line_thickness = text
            .decoration_thickness
            .unwrap_or_else(|| (text.font_size / 14.0).clamp(1.0, 3.0));

        // Decoration color: use CSS text-decoration-color if set, else use text color
        let dec_color = text.decoration_color.unwrap_or(text.color);

        let layer_primitives = primitives_by_layer.entry(text.z_index).or_default();

        // Calculate the actual baseline Y position based on vertical alignment
        // This must match the text rendering logic to position decorations correctly
        //
        // glyph_extent = ascender - descender (where descender is negative)
        // Typical descender is about -20% of ascender, so glyph_extent ≈ ascender * 1.2
        let descender_approx = -text.ascender * 0.2;
        let glyph_extent = text.ascender - descender_approx;

        let baseline_y = match text.v_align {
            TextVerticalAlign::Center => {
                // GPU: y_pos = text.y + text.height / 2.0, then y_offset = y_pos - glyph_extent / 2.0
                // Glyph top is at: text.y + text.height/2 - glyph_extent/2
                // Baseline is at: glyph_top + ascender
                let glyph_top = text.y + text.height / 2.0 - glyph_extent / 2.0;
                glyph_top + text.ascender
            }
            TextVerticalAlign::Top => {
                // GPU: y_pos = text.y, y_offset = y + (layout_height - glyph_extent) / 2.0
                // Glyph top is at: text.y + (text.height - glyph_extent) / 2.0
                // Baseline is at: glyph_top + ascender
                let glyph_top = text.y + (text.height - glyph_extent) / 2.0;
                glyph_top + text.ascender
            }
            TextVerticalAlign::Baseline => {
                // GPU: y_pos = text.y + ascender, y_offset = y_pos - ascender = text.y
                // Glyph top is at: text.y
                // Baseline is at: text.y + ascender
                text.y + text.ascender
            }
        };

        // Strikethrough: draw line through the center of lowercase letters (x-height center)
        if text.strikethrough {
            // x-height is typically ~50% of ascender, center of x-height is ~25% above baseline
            let strikethrough_y = baseline_y - text.ascender * 0.35;
            let mut strike_rect = GpuPrimitive::rect(
                text.x,
                strikethrough_y - line_thickness / 2.0,
                decoration_width,
                line_thickness,
            )
            .with_color(dec_color[0], dec_color[1], dec_color[2], dec_color[3]);

            // Apply clip bounds from text element if present
            if let Some(clip) = text.clip_bounds {
                strike_rect = strike_rect.with_clip_rect(clip[0], clip[1], clip[2], clip[3]);
            }
            layer_primitives.push(strike_rect);
        }

        // Underline: draw line just below the baseline (at text bottom)
        if text.underline {
            // Underline position: just below baseline, snapping to text bottom
            let underline_y = baseline_y + text.ascender * 0.05;
            let mut underline_rect = GpuPrimitive::rect(
                text.x,
                underline_y - line_thickness / 2.0,
                decoration_width,
                line_thickness,
            )
            .with_color(dec_color[0], dec_color[1], dec_color[2], dec_color[3]);

            // Apply clip bounds from text element if present
            if let Some(clip) = text.clip_bounds {
                underline_rect = underline_rect.with_clip_rect(clip[0], clip[1], clip[2], clip[3]);
            }
            layer_primitives.push(underline_rect);
        }
    }

    primitives_by_layer
}

/// Generate debug primitives for text elements
///
/// Creates visual overlays showing:
/// - Bounding box outline (cyan)
/// - Baseline position (magenta line)
/// - Ascender line (green, at top of bounding box)
/// - Descender line (yellow, at bottom of bounding box)
fn generate_text_debug_primitives(texts: &[TextElement]) -> Vec<GpuPrimitive> {
    let mut primitives = Vec::new();

    for text in texts {
        // Determine the actual text width for debug visualization:
        // - For non-wrapped text: use measured_width (actual rendered text width)
        // - For wrapped text: use layout width (container constrains the text)
        let debug_width = if text.wrap && text.measured_width > text.width {
            // Text is wrapping - use container width
            text.width
        } else {
            // Single line - use actual measured width (clamped to layout width)
            text.measured_width.min(text.width)
        };

        // Bounding box outline (cyan, semi-transparent)
        let bbox = GpuPrimitive::rect(text.x, text.y, debug_width, text.height)
            .with_color(0.0, 0.0, 0.0, 0.0) // Transparent fill
            .with_border(1.0, 0.0, 1.0, 1.0, 0.7); // Cyan border
        primitives.push(bbox);

        // Baseline indicator (magenta horizontal line)
        // The baseline is at y + ascender
        let baseline_y = text.y + text.ascender;
        let baseline = GpuPrimitive::rect(text.x, baseline_y - 0.5, debug_width, 1.0)
            .with_color(1.0, 0.0, 1.0, 0.6); // Magenta
        primitives.push(baseline);

        // Ascender line indicator (green, at top of text)
        // For v_baseline texts, this shows where the ascender sits
        let ascender_line = GpuPrimitive::rect(text.x, text.y - 0.5, debug_width, 1.0)
            .with_color(0.0, 1.0, 0.0, 0.4); // Green, more transparent
        primitives.push(ascender_line);

        // Descender line (yellow, at bottom of bounding box)
        let descender_y = text.y + text.height;
        let descender_line = GpuPrimitive::rect(text.x, descender_y - 0.5, debug_width, 1.0)
            .with_color(1.0, 1.0, 0.0, 0.4); // Yellow
        primitives.push(descender_line);
    }

    primitives
}

/// Collect all element bounds from the render tree for debug visualization
fn collect_debug_bounds(tree: &RenderTree, scale: f32) -> Vec<DebugBoundsElement> {
    let mut bounds = Vec::new();

    if let Some(root) = tree.root() {
        collect_debug_bounds_recursive(tree, root, (0.0, 0.0), 0, scale, &mut bounds);
    }

    bounds
}

/// Recursively collect bounds from all nodes
fn collect_debug_bounds_recursive(
    tree: &RenderTree,
    node: LayoutNodeId,
    parent_offset: (f32, f32),
    depth: u32,
    scale: f32,
    bounds: &mut Vec<DebugBoundsElement>,
) {
    use blinc_layout::renderer::ElementType;

    let Some(node_bounds) = tree.layout().get_bounds(node, parent_offset) else {
        return;
    };

    // Determine element type name
    let element_type = tree
        .get_render_node(node)
        .map(|n| match &n.element_type {
            ElementType::Div => "Div".to_string(),
            ElementType::Text(_) => "Text".to_string(),
            ElementType::StyledText(_) => "StyledText".to_string(),
            ElementType::Image(_) => "Image".to_string(),
            ElementType::Svg(_) => "Svg".to_string(),
            ElementType::Canvas(_) => "Canvas".to_string(),
        })
        .unwrap_or_else(|| "Unknown".to_string());

    // Add this element's bounds (with DPI scaling)
    bounds.push(DebugBoundsElement {
        x: node_bounds.x * scale,
        y: node_bounds.y * scale,
        width: node_bounds.width * scale,
        height: node_bounds.height * scale,
        element_type,
        depth,
    });

    // Get scroll offset for this node (scroll containers offset their children)
    let scroll_offset = tree.get_scroll_offset(node);

    // Calculate new offset for children (including scroll offset)
    let new_offset = (
        node_bounds.x + scroll_offset.0,
        node_bounds.y + scroll_offset.1,
    );

    // Recurse into children
    for child in tree.layout().children(node) {
        collect_debug_bounds_recursive(tree, child, new_offset, depth + 1, scale, bounds);
    }
}

/// Generate debug primitives for layout element bounds
///
/// Creates visual overlays showing:
/// - Colored outlines for each element's bounding box
/// - Colors cycle based on tree depth (red, green, blue, yellow, cyan, magenta)
fn generate_layout_debug_primitives(bounds: &[DebugBoundsElement]) -> Vec<GpuPrimitive> {
    let mut primitives = Vec::new();

    // Color palette for different depths (cycling)
    let colors: [(f32, f32, f32); 6] = [
        (1.0, 0.3, 0.3), // Red
        (0.3, 1.0, 0.3), // Green
        (0.3, 0.3, 1.0), // Blue
        (1.0, 1.0, 0.3), // Yellow
        (0.3, 1.0, 1.0), // Cyan
        (1.0, 0.3, 1.0), // Magenta
    ];

    for elem in bounds {
        // Skip very small elements (likely invisible)
        if elem.width < 1.0 || elem.height < 1.0 {
            continue;
        }

        let (r, g, b) = colors[(elem.depth as usize) % colors.len()];
        let alpha = 0.5; // Semi-transparent outline

        // Draw outline only (transparent fill with colored border)
        let rect = GpuPrimitive::rect(elem.x, elem.y, elem.width, elem.height)
            .with_color(0.0, 0.0, 0.0, 0.0) // Transparent fill
            .with_border(1.0, r, g, b, alpha); // Colored border

        primitives.push(rect);
    }

    primitives
}

/// Scale and translate a path for SVG rendering with tint
fn scale_and_translate_path(
    path: &blinc_core::Path,
    x: f32,
    y: f32,
    scale: f32,
) -> blinc_core::Path {
    use blinc_core::{PathCommand, Point, Vec2};

    if scale == 1.0 && x == 0.0 && y == 0.0 {
        return path.clone();
    }

    let transform_point = |p: Point| -> Point { Point::new(p.x * scale + x, p.y * scale + y) };

    let new_commands: Vec<PathCommand> = path
        .commands()
        .iter()
        .map(|cmd| match cmd {
            PathCommand::MoveTo(p) => PathCommand::MoveTo(transform_point(*p)),
            PathCommand::LineTo(p) => PathCommand::LineTo(transform_point(*p)),
            PathCommand::QuadTo { control, end } => PathCommand::QuadTo {
                control: transform_point(*control),
                end: transform_point(*end),
            },
            PathCommand::CubicTo {
                control1,
                control2,
                end,
            } => PathCommand::CubicTo {
                control1: transform_point(*control1),
                control2: transform_point(*control2),
                end: transform_point(*end),
            },
            PathCommand::ArcTo {
                radii,
                rotation,
                large_arc,
                sweep,
                end,
            } => PathCommand::ArcTo {
                radii: Vec2::new(radii.x * scale, radii.y * scale),
                rotation: *rotation,
                large_arc: *large_arc,
                sweep: *sweep,
                end: transform_point(*end),
            },
            PathCommand::Close => PathCommand::Close,
        })
        .collect();

    blinc_core::Path::from_commands(new_commands)
}

// ─────────────────────────────────────────────────────────────────────────────
// 3D mesh dispatch
// ─────────────────────────────────────────────────────────────────────────────

/// Dispatch every `PendingMesh` captured by `GpuPaintContext` to
/// `GpuRenderer::render_mesh_data` against the frame target.
///
/// Computes a view-projection matrix from each pending mesh's captured
/// `Camera` against the current viewport size (so aspect stays correct
/// under window resizes) and extracts the first `Light::Directional`
/// for the mesh pipeline's sun light. Other light types are ignored
/// for now — the mesh pipeline only takes a single directional input,
/// and widening that is follow-up work tracked alongside per-canvas
/// viewport clipping.
///
/// If a mesh's camera is `Camera::default()` the pose is identity /
/// zero-eye which produces an invisible frame; demos should always
/// `ctx.set_camera(&cam)` before calling `ctx.draw_mesh_data`. A
/// `tracing::warn!` surfaces the silent-empty case to avoid
/// head-scratching during demo authoring.
fn dispatch_pending_meshes(
    renderer: &mut GpuRenderer,
    target: &wgpu::TextureView,
    width: u32,
    height: u32,
    meshes: &[PendingMesh],
) {
    if meshes.is_empty() {
        return;
    }
    let aspect = if height > 0 {
        width as f32 / height as f32
    } else {
        1.0
    };

    // Dedupe environment cubemap uploads by `Arc` identity. Without
    // this, multi-mesh scenes pay a full cubemap re-upload per
    // primitive — 39 meshes × 6 faces × ~9 mips = ~2000 redundant
    // `queue.write_texture` calls per frame on assets like
    // buster_drone, which dominates frame time. Every `PendingMesh`
    // from the same `SceneKit3D` shares the same `Arc<CubemapData>`,
    // so a cheap `Arc::ptr_eq` is enough to skip.
    let mut last_env: Option<std::sync::Arc<blinc_core::layer::CubemapData>> = None;

    // ── Shadow frustum (first directional light, first scene) ──────────
    //
    // Compute ONE light_view_proj for the whole mesh batch. Using the
    // first scene's camera target as a focus point and scaling the
    // orthographic frustum off the camera distance keeps the shadow
    // map sized to roughly what the viewer can see. Good enough for
    // the single-scene case (the common case); multi-scene frames
    // with divergent targets would want per-scene shadow maps, which
    // is out of scope here.
    let first = &meshes[0];
    let (light_dir, _light_intensity_0) = first_directional_light(&first.lights);
    let light_view_proj = compute_shadow_matrix(&first.camera, light_dir);

    // ── Phase 1: shadow depth pass for every caster ────────────────────
    //
    // All shadow-caster meshes write into the single shadow_map before
    // ANY main pass samples it. The first caster clears, the rest
    // load. Non-casters skip the pass entirely.
    let mut shadow_index: usize = 0;
    for pending in meshes {
        if !pending.mesh.material.casts_shadows {
            continue;
        }
        let model = mat4_to_array(&pending.transform);
        renderer.render_mesh_shadow_pass(&pending.mesh, &model, &light_view_proj, shadow_index);
        shadow_index += 1;
    }
    // If no casters were drawn, skip shadow sampling entirely so the
    // main pass doesn't read a stale / uninitialized shadow map.
    let shadow_matrix: Option<&[f32; 16]> = if shadow_index > 0 {
        Some(&light_view_proj)
    } else {
        None
    };

    // ── Phase 2: main HDR/tonemap passes for every mesh ────────────────
    //
    // Sort OPAQUE + MASK before BLEND before dispatch. Weighted-blended
    // OIT (the renderer's transparency path) needs every opaque fragment
    // to have written its depth before any BLEND fragment runs its own
    // depth test; otherwise a BLEND mesh drawn mid-scene accumulates at
    // pixels a later-drawn opaque mesh would have occluded, and the OIT
    // composite washes those opaque pixels out.
    //
    // Callers submitting `draw_mesh_data(...)` in scene-graph order
    // (the most common case — e.g. gltf_animation_demo, cutegirl,
    // strangler) would otherwise need to sort manually at each call
    // site. Doing it once here, at the single dispatch seam, keeps
    // every asset correct without surfacing the OIT ordering invariant
    // as user-facing knowledge.
    //
    // `sort_by_key` is stable so within a mode the caller's submission
    // order is preserved — matters when a caller is already doing
    // back-to-front sorting inside the BLEND group.
    let mut ordered: Vec<&PendingMesh> = meshes.iter().collect();
    ordered.sort_by_key(|p| {
        matches!(
            p.mesh.material.alpha_mode,
            blinc_core::draw::AlphaMode::Blend
        ) as u8
    });

    let batch_count = ordered.len();
    for (batch_index, &pending) in ordered.iter().enumerate() {
        // Upload the environment cubemap only when its `Arc` identity
        // changes. The renderer's texture is overwritten on each real
        // upload, so only the last distinct environment matters.
        if let Some(ref env) = pending.env_cubemap {
            let is_new = last_env
                .as_ref()
                .map(|prev| !std::sync::Arc::ptr_eq(prev, env))
                .unwrap_or(true);
            if is_new {
                renderer.upload_environment_cubemap(env);
                last_env = Some(env.clone());
            }
        }

        // Use the canvas viewport aspect when available so the
        // perspective projection matches the clipped region, not the
        // full frame. Falls back to the frame aspect for full-viewport
        // mesh draws (no canvas wrapper).
        let vp_aspect = pending
            .viewport
            .map(|[_, _, w, h]| if h > 0.0 { w / h } else { 1.0 })
            .unwrap_or(aspect);
        let view_proj = camera_view_proj(&pending.camera, vp_aspect);
        let _inv_view_proj = mat4_inverse_flat(&view_proj);
        let camera_pos = [
            pending.camera.position.x,
            pending.camera.position.y,
            pending.camera.position.z,
        ];
        let lights = collect_directional_lights(&pending.lights);
        let model = mat4_to_array(&pending.transform);

        renderer.render_mesh_data_batched(
            target,
            &pending.mesh,
            &model,
            &view_proj,
            camera_pos,
            &lights,
            shadow_matrix,
            pending.viewport,
            batch_index,
            batch_count,
        );
    }
}

/// Build a view × projection matrix for a directional light illuminating
/// the scene the given camera is looking at.
///
/// This is a deliberately simple fit: we center the orthographic frustum
/// on the camera target and scale it off the camera distance. The result
/// covers roughly what the viewer can see, which is enough for a
/// single-drone / single-object scene (the main case today). A more
/// ambitious implementation would fit the frustum to the scene's actual
/// world-space AABB (or to the view frustum intersected with the scene
/// bounds), plus use cascaded shadow maps for landscapes.
///
/// Up-vector selection avoids the degenerate case where `light_dir` is
/// parallel to world-Y (makes the look-at cross product collapse).
fn compute_shadow_matrix(camera: &blinc_core::Camera, light_dir: [f32; 3]) -> [f32; 16] {
    let target = camera.target;
    let eye = camera.position;
    let dx = eye.x - target.x;
    let dy = eye.y - target.y;
    let dz = eye.z - target.z;
    let cam_dist = (dx * dx + dy * dy + dz * dz).sqrt().max(1.0);

    // Ortho frustum half-extent — ~1.0× camera distance covers the
    // full scene for typical framings (frame_camera multiplies scene
    // diagonal by 1.1, so camera distance ≈ scene diagonal).
    let half = cam_dist;

    // Light sits on the opposite side of the target from its direction,
    // far enough back that the near plane doesn't clip the scene.
    let ld = blinc_core::Vec3::new(light_dir[0], light_dir[1], light_dir[2]).normalize();
    let light_pos = blinc_core::Vec3::new(
        target.x - ld.x * cam_dist * 2.0,
        target.y - ld.y * cam_dist * 2.0,
        target.z - ld.z * cam_dist * 2.0,
    );

    // Pick an up-vector not parallel to the light direction.
    let up = if ld.y.abs() > 0.95 {
        blinc_core::Vec3::new(0.0, 0.0, 1.0)
    } else {
        blinc_core::Vec3::new(0.0, 1.0, 0.0)
    };

    let view = mat4_look_at(light_pos, target, up);
    let proj = mat4_orthographic_rh(-half, half, -half, half, 0.1, cam_dist * 4.0);
    mat4_mul_flat(&proj, &view)
}

/// Build a view × projection matrix for the captured `Camera`.
///
/// Right-handed coordinate system, +Y up. Matches the convention the
/// mesh shader expects (see `crates/blinc_gpu/src/shaders/mesh.wgsl`).
///
/// For `CameraProjection::Perspective`, the stored `aspect` field on
/// the projection is overridden by the frame's actual aspect so the
/// scene doesn't stretch on resize — the stored value is just a
/// fallback default from `Camera::perspective`.
fn camera_view_proj(camera: &blinc_core::Camera, frame_aspect: f32) -> [f32; 16] {
    let view = mat4_look_at(camera.position, camera.target, camera.up);
    let proj = match camera.projection {
        blinc_core::CameraProjection::Perspective {
            fov_y, near, far, ..
        } => mat4_perspective_rh(fov_y, frame_aspect, near, far),
        blinc_core::CameraProjection::Orthographic {
            left,
            right,
            bottom,
            top,
            near,
            far,
        } => mat4_orthographic_rh(left, right, bottom, top, near, far),
    };
    mat4_mul_flat(&proj, &view)
}

/// Extract the first `Light::Directional` from the snapshot, returning
/// a normalized direction vector and scalar intensity. Falls back to a
/// soft top-down fill if none is present so the demo never renders
/// pitch-black.
fn first_directional_light(lights: &[blinc_core::Light]) -> ([f32; 3], f32) {
    for light in lights {
        if let blinc_core::Light::Directional {
            direction,
            intensity,
            ..
        } = light
        {
            let d = direction.normalize();
            return ([d.x, d.y, d.z], *intensity);
        }
    }
    ([0.0, -1.0, 0.3], 0.8)
}

/// Collect every directional light for the mesh shader's multi-light
/// array. Capped at the shader's `MAX_DIR_LIGHTS`; if the scene has
/// no directional lights we hand back a single soft top-down default
/// so the mesh never renders pitch-black.
fn collect_directional_lights(lights: &[blinc_core::Light]) -> Vec<blinc_gpu::DirectionalLight> {
    let max = blinc_gpu::MAX_DIR_LIGHTS;
    let mut out: Vec<blinc_gpu::DirectionalLight> = Vec::with_capacity(max);
    for light in lights {
        if out.len() >= max {
            break;
        }
        if let blinc_core::Light::Directional {
            direction,
            color,
            intensity,
            ..
        } = light
        {
            let d = direction.normalize();
            out.push(blinc_gpu::DirectionalLight {
                direction: [d.x, d.y, d.z],
                intensity: *intensity,
                color: [color.r, color.g, color.b],
            });
        }
    }
    if out.is_empty() {
        out.push(blinc_gpu::DirectionalLight::DEFAULT);
    }
    out
}

/// Flatten a column-major `Mat4` to the `[f32; 16]` layout
/// `GpuRenderer::render_mesh_data` expects.
fn mat4_to_array(m: &blinc_core::Mat4) -> [f32; 16] {
    let mut out = [0.0f32; 16];
    for col in 0..4 {
        for row in 0..4 {
            out[col * 4 + row] = m.cols[col][row];
        }
    }
    out
}

/// Multiply two flat column-major 4×4 matrices (`a * b`), returning a
/// `[f32; 16]` in the same layout. Used to compose `proj * view` after
/// both are computed in `Mat4`/array form.
fn mat4_mul_flat(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.0f32; 16];
    for col in 0..4 {
        for row in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[k * 4 + row] * b[col * 4 + k];
            }
            out[col * 4 + row] = s;
        }
    }
    out
}

/// Right-handed look-at view matrix. Produces `[f32; 16]` directly
/// (column-major) for the downstream multiply.
fn mat4_look_at(
    eye: blinc_core::Vec3,
    target: blinc_core::Vec3,
    up: blinc_core::Vec3,
) -> [f32; 16] {
    let f = blinc_core::Vec3::new(target.x - eye.x, target.y - eye.y, target.z - eye.z).normalize();
    let r = f.cross(up).normalize();
    let u = r.cross(f);
    let tx = -(r.x * eye.x + r.y * eye.y + r.z * eye.z);
    let ty = -(u.x * eye.x + u.y * eye.y + u.z * eye.z);
    let tz = f.x * eye.x + f.y * eye.y + f.z * eye.z;
    // Column-major: col0 = [r.x, u.x, -f.x, 0], col1 = [r.y, u.y, -f.y, 0], ...
    [
        r.x, u.x, -f.x, 0.0, r.y, u.y, -f.y, 0.0, r.z, u.z, -f.z, 0.0, tx, ty, tz, 1.0,
    ]
}

/// Right-handed perspective projection. Maps view-space Z in `[-far, -near]`
/// to clip-space depth `[0, 1]` (wgpu convention). `fov_y` is radians.
fn mat4_perspective_rh(fov_y: f32, aspect: f32, near: f32, far: f32) -> [f32; 16] {
    let f = 1.0 / (fov_y * 0.5).tan();
    let nf = 1.0 / (near - far);
    [
        f / aspect,
        0.0,
        0.0,
        0.0,
        0.0,
        f,
        0.0,
        0.0,
        0.0,
        0.0,
        far * nf,
        -1.0,
        0.0,
        0.0,
        far * near * nf,
        0.0,
    ]
}

/// Right-handed orthographic projection. Uses the same clip-space
/// depth range `[0, 1]` as the perspective variant so the mesh shader
/// can stay agnostic of the projection choice.
fn mat4_orthographic_rh(
    left: f32,
    right: f32,
    bottom: f32,
    top: f32,
    near: f32,
    far: f32,
) -> [f32; 16] {
    let rl = 1.0 / (right - left);
    let tb = 1.0 / (top - bottom);
    let fnn = 1.0 / (far - near);
    [
        2.0 * rl,
        0.0,
        0.0,
        0.0,
        0.0,
        2.0 * tb,
        0.0,
        0.0,
        0.0,
        0.0,
        -fnn,
        0.0,
        -(right + left) * rl,
        -(top + bottom) * tb,
        -near * fnn,
        1.0,
    ]
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

#![allow(unused, dead_code, deprecated)]
//! Blinc GPU Renderer
//!
//! SDF-based GPU rendering using wgpu.
//!
//! # Features
//!
//! - **SDF Primitives**: Rounded rectangles, circles, ellipses with anti-aliasing
//! - **Shadows**: Gaussian blur shadows via error function approximation
//! - **Gradients**: Linear and radial gradient fills
//! - **Glass/Vibrancy**: Backdrop blur effects for frosted glass UI (Apple-style)
//! - **Text**: SDF-based text rendering with glyph atlases
//! - **Compositing**: Layer blending with various blend modes
//! - **Backbuffer**: Double/triple buffering for WASM and glass effects
//! - **Paint Context**: GPU-backed DrawContext implementation
//! - **Path Rendering**: Vector path tessellation via lyon

pub mod backbuffer;
pub mod custom_pass;
pub mod flow_codegen;
pub mod flow_pipeline;
pub mod gradient_texture;
pub mod image;
pub mod mesh_pipeline;
pub mod paint;
pub mod particles;
pub mod path;
pub mod primitives;
pub mod renderer;
pub mod shaders;
pub mod text;

pub use backbuffer::{Backbuffer, BackbufferConfig, FrameContext};
pub use custom_pass::{
    create_buffer, create_compute_pipeline, create_fullscreen_pipeline, BindGroupBuilder,
    ComputeDispatch, CustomRenderPass, PostProcessChain, PostProcessEffect, RenderPassContext,
    RenderStage,
};
pub use gradient_texture::{GradientTextureCache, RasterizedGradient, GRADIENT_TEXTURE_WIDTH};
pub use image::{GpuImage, GpuImageInstance, ImageRenderingContext};
pub use paint::{GpuPaintContext, PendingMesh};
pub use path::{
    extract_brush_info, tessellate_fill, tessellate_stroke, PathBrushInfo, PathBrushType,
    PathVertex, TessellatedPath,
};
pub use primitives::{
    BlurUniforms, ClipType, ColorMatrixUniforms, CompositeUniforms, DropShadowUniforms, FillType,
    GlassType, GlassUniforms, GlowUniforms, GpuGlassPrimitive, GpuGlyph, GpuPrimitive,
    LayerCommand, LayerCommandEntry, LayerCompositeUniforms, ParticleViewport3D, PathBatch,
    PathUniforms, PrimitiveBatch, PrimitiveType, Uniforms, NOTCH_CORNER_CONCAVE,
    NOTCH_CORNER_SHARP_OR_CONVEX, NOTCH_MOD_BULGE, NOTCH_MOD_CUT, NOTCH_MOD_NONE, NOTCH_MOD_PEAK,
    NOTCH_MOD_SCOOP,
};
pub use renderer::{
    GpuMemoryBudget, GpuRenderer, LayerTexture, LayerTextureCache, RendererConfig, RendererError,
    TextureCacheStats,
};
pub use shaders::{
    BLUR_SHADER, COLOR_MATRIX_SHADER, COMPOSITE_SHADER, DROP_SHADOW_SHADER, GLASS_SHADER,
    GLOW_SHADER, IMAGE_SHADER, LAYER_COMPOSITE_SHADER, PATH_SHADER, SDF_3D_SHADER, SDF_CORE_SHADER,
    SDF_NOTCH_SHADER, SDF_SHADER, SDF_SHADOW_SHADER, SIMPLE_GLASS_SHADER, TEXT_SHADER,
};
pub use text::TextRenderingContext;

// Particle system exports
pub use particles::{
    GpuEmitter, GpuForce, GpuParticle, GpuRenderUniforms, GpuSimulationUniforms, ParticleManager,
    ParticleSystemGpu, ParticleViewport, PARTICLE_COMPUTE_SHADER, PARTICLE_RENDER_SHADER,
};

// Flow DAG → WGSL codegen + GPU pipeline cache
pub use flow_codegen::flow_to_wgsl;
pub use flow_pipeline::{FlowPipelineCache, FlowUniformData};

// Re-export text types for convenience
pub use blinc_text::{ColorSpan, FontRegistry, GenericFont, TextAlignment, TextAnchor};

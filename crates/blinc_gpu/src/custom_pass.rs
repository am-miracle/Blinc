//! Custom Render Pass API
//!
//! Allows users to inject their own GPU render passes into the Blinc pipeline.
//!
//! # Example
//!
//! ```ignore
//! use blinc_gpu::custom_pass::*;
//!
//! struct MyPostEffect {
//!     pipeline: Option<wgpu::RenderPipeline>,
//! }
//!
//! impl CustomRenderPass for MyPostEffect {
//!     fn label(&self) -> &str { "my_post_effect" }
//!     fn stage(&self) -> RenderStage { RenderStage::PostProcess }
//!
//!     fn initialize(&mut self, device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) {
//!         // Create your pipeline, bind groups, etc.
//!     }
//!
//!     fn render(&mut self, ctx: &RenderPassContext) {
//!         let mut encoder = ctx.device.create_command_encoder(&Default::default());
//!         // ... your render pass ...
//!         ctx.queue.submit(std::iter::once(encoder.finish()));
//!     }
//! }
//! ```

/// Stage in the rendering pipeline where a custom pass executes
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RenderStage {
    /// Before the main UI rendering.
    /// Good for: skyboxes, 3D scene backgrounds, clearing with custom colors.
    PreRender,
    /// After all UI rendering.
    /// Good for: screen-space effects, overlays, debug visualizations, tone mapping.
    PostProcess,
}

/// Context provided to custom render passes each frame
pub struct RenderPassContext<'a> {
    /// GPU device for creating resources
    pub device: &'a wgpu::Device,
    /// GPU queue for submitting commands and uploading data
    pub queue: &'a wgpu::Queue,
    /// Current render target (the framebuffer or offscreen texture)
    pub target: &'a wgpu::TextureView,
    /// Viewport width in physical pixels
    pub viewport_width: u32,
    /// Viewport height in physical pixels
    pub viewport_height: u32,
    /// Surface texture format
    pub texture_format: wgpu::TextureFormat,
    /// Display scale factor (DPI)
    pub scale_factor: f64,
}

/// Trait for user-defined render passes.
///
/// Implement this to inject custom GPU rendering into the Blinc pipeline.
/// The pass is initialized once, then `render()` is called each frame.
///
/// Passes are executed in registration order within their stage.
pub trait CustomRenderPass: Send {
    /// Human-readable label for debugging and profiling
    fn label(&self) -> &str;

    /// Which stage of the pipeline this pass runs in
    fn stage(&self) -> RenderStage;

    /// Create GPU resources (pipelines, buffers, textures).
    ///
    /// Called once after registration, before the first `render()` call.
    fn initialize(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
    );

    /// Execute the render pass for this frame.
    ///
    /// Create a command encoder, begin your render pass(es), and submit.
    /// The `target` view is the current framebuffer — use `LoadOp::Load`
    /// to preserve existing content, or `LoadOp::Clear` to overwrite.
    fn render(&mut self, ctx: &RenderPassContext);

    /// Called when the viewport is resized.
    ///
    /// Override this to recreate size-dependent resources (e.g., render targets).
    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {}

    /// Whether this pass is currently enabled.
    ///
    /// Disabled passes are skipped without removing them from the pipeline.
    fn enabled(&self) -> bool {
        true
    }
}

/// Manages registered custom render passes
pub(crate) struct CustomPassManager {
    passes: Vec<Box<dyn CustomRenderPass>>,
}

impl CustomPassManager {
    pub fn new() -> Self {
        Self { passes: Vec::new() }
    }

    /// Register a new custom render pass. It will be initialized on next frame.
    pub fn register(&mut self, pass: Box<dyn CustomRenderPass>) {
        self.passes.push(pass);
    }

    /// Initialize any uninitialized passes
    pub fn initialize_pending(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
    ) {
        // All passes are initialized when registered — this is called once
        // after each registration batch
        for pass in &mut self.passes {
            pass.initialize(device, queue, format);
        }
    }

    /// Execute all enabled passes for the given stage
    pub fn execute_stage(&mut self, stage: RenderStage, ctx: &RenderPassContext) {
        for pass in &mut self.passes {
            if pass.stage() == stage && pass.enabled() {
                pass.render(ctx);
            }
        }
    }

    /// Notify all passes of a resize
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        for pass in &mut self.passes {
            pass.resize(device, width, height);
        }
    }

    /// Check if any passes exist for a given stage
    pub fn has_passes(&self, stage: RenderStage) -> bool {
        self.passes
            .iter()
            .any(|p| p.stage() == stage && p.enabled())
    }

    /// Remove a pass by label. Returns true if found and removed.
    pub fn remove(&mut self, label: &str) -> bool {
        let len_before = self.passes.len();
        self.passes.retain(|p| p.label() != label);
        self.passes.len() < len_before
    }
}

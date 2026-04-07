//! Web platform runner — `wasm32-unknown-unknown` only.
//!
//! Sibling of [`crate::windowed`] / [`crate::android`] / [`crate::ios`]
//! (and the Fuchsia stub) that owns the per-frame loop and browser
//! event wiring. The frame loop drives the same 5-phase pipeline the
//! desktop runner uses; only the *driver* differs:
//!
//! - **desktop**: winit `Frame::AboutToWait` → render → `request_redraw`
//! - **android**: native_activity `MainEvent::RequestRedraw` → render
//! - **ios**: `CADisplayLink` callback → render
//! - **web**: `window.requestAnimationFrame` → render → schedule next
//!
//! ## Phase 3a scope
//!
//! This commit lands the *construction* path only — [`WebApp::new`]
//! locates the canvas, builds the [`crate::app::BlincApp`] via the new
//! [`crate::app::BlincApp::with_canvas`] async constructor, builds a
//! [`crate::windowed::WindowedContext`] via the new
//! [`crate::windowed::WindowedContext::new_web`] sibling, and stores
//! everything ready to be driven by a frame loop. The actual
//! `requestAnimationFrame` driver and DOM event wiring land in Phase 3b.
//!
//! Splitting the runner this way means each commit individually
//! compiles, type-checks, and lints clean — and any trait-bound
//! surprises in Phase 3b (we expect `Send`/`!Send` mismatches around
//! `Closure::<dyn FnMut>` and the shared state) are isolated to the
//! follow-up.

use std::sync::{atomic::AtomicBool, Arc, Mutex};

use blinc_animation::AnimationScheduler;
use blinc_core::context_state::HookState;
use blinc_core::reactive::ReactiveGraph;
use blinc_layout::selector::ElementRegistry;
use blinc_layout::widgets::overlay::overlay_manager;
use wasm_bindgen::JsCast;

use crate::app::BlincApp;
use crate::error::{BlincError, Result};
use crate::windowed::{
    RefDirtyFlag, SharedAnimationScheduler, SharedElementRegistry, SharedReactiveGraph,
    SharedReadyCallbacks, WindowedContext,
};

/// Top-level web runner.
///
/// Owns the canvas, the wgpu surface and surface configuration, the
/// shared [`BlincApp`], and the [`WindowedContext`] that the user-supplied
/// UI builder receives on each rebuild.
///
/// This struct is intentionally `!Send` — every browser API it touches
/// is single-threaded, and its sub-fields (`wgpu::Surface` on wasm32,
/// `web_sys::HtmlCanvasElement`) are `!Send` themselves.
pub struct WebApp {
    /// The HtmlCanvasElement we're rendering into. Held so we can
    /// re-read its size after a browser resize.
    canvas: web_sys::HtmlCanvasElement,
    /// Wgpu surface + its configured properties.
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    /// The Blinc application core (renderer + text + render context).
    blinc_app: BlincApp,
    /// User-facing window context. Same shape every other platform builds.
    ctx: WindowedContext,
    /// Last frame's logical width / height in CSS pixels. Used to
    /// detect resize events without having to query the canvas every
    /// frame.
    #[allow(dead_code)]
    last_logical_size: (f32, f32),
}

impl WebApp {
    /// Locate the `<canvas id="…">` in the DOM, set up its physical
    /// framebuffer to match the device pixel ratio, build the GPU
    /// renderer for it, and assemble a [`WebApp`] ready for a frame
    /// loop driver.
    ///
    /// Returns errors if:
    /// - There is no global `window` object (e.g. running in a worker)
    /// - There is no `document`
    /// - No element with `canvas_id` exists
    /// - The matched element isn't actually an `HtmlCanvasElement`
    /// - GPU initialization fails (no WebGPU support, adapter request fails…)
    ///
    /// On success, the canvas's framebuffer dimensions
    /// (`canvas.width` / `canvas.height`) are set to
    /// `client_width * dpr` × `client_height * dpr` so the GPU surface
    /// is sized to actual device pixels rather than CSS pixels.
    pub async fn new(canvas_id: &str) -> Result<Self> {
        // 1. Locate the canvas in the DOM.
        let window = web_sys::window().ok_or_else(|| {
            BlincError::Platform("WebApp::new called without a global `window` object".to_string())
        })?;
        let document = window.document().ok_or_else(|| {
            BlincError::Platform("WebApp::new called without a `document` object".to_string())
        })?;
        let canvas: web_sys::HtmlCanvasElement = document
            .get_element_by_id(canvas_id)
            .ok_or_else(|| {
                BlincError::Platform(format!("No element with id `{canvas_id}` in document"))
            })?
            .dyn_into()
            .map_err(|_| {
                BlincError::Platform(format!("Element `{canvas_id}` is not an HtmlCanvasElement"))
            })?;

        // 2. Read logical size + DPR, then set the framebuffer to the
        //    physical size before creating the GPU surface. This is
        //    the canonical "resize the canvas to match its CSS size"
        //    pattern from the wgpu web examples — without it, the
        //    canvas defaults to 300×150 regardless of CSS.
        let logical_width = canvas.client_width() as f32;
        let logical_height = canvas.client_height() as f32;
        let scale_factor = window.device_pixel_ratio();
        let physical_width = (logical_width * scale_factor as f32).round().max(1.0);
        let physical_height = (logical_height * scale_factor as f32).round().max(1.0);
        canvas.set_width(physical_width as u32);
        canvas.set_height(physical_height as u32);

        // 3. Build the GPU renderer from the canvas.
        let (blinc_app, surface) = BlincApp::with_canvas(canvas.clone(), None).await?;

        // 4. Configure the surface for the canvas's physical dimensions.
        let texture_format = blinc_app.texture_format();
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: texture_format,
            width: physical_width as u32,
            height: physical_height as u32,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };
        surface.configure(blinc_app.device(), &surface_config);

        // 5. Build the shared collaborator graph that every platform
        //    needs. These mirror what the desktop runner constructs in
        //    `WindowedApp::run` (windowed.rs ~line 2105) — we don't
        //    set a wake_callback or call `start_background()` because
        //    the wasm runner drives ticks synchronously from
        //    `requestAnimationFrame` (Phase 3b).
        let scheduler = AnimationScheduler::new();
        let animations: SharedAnimationScheduler = Arc::new(Mutex::new(scheduler));
        let ref_dirty_flag: RefDirtyFlag = Arc::new(AtomicBool::new(false));
        let reactive: SharedReactiveGraph = Arc::new(Mutex::new(ReactiveGraph::new()));
        let hooks = Arc::new(Mutex::new(HookState::new()));
        let overlay_mgr = overlay_manager();
        let element_registry: SharedElementRegistry = Arc::new(ElementRegistry::new());
        let ready_callbacks: SharedReadyCallbacks = Arc::new(Mutex::new(Vec::new()));

        let ctx = WindowedContext::new_web(
            logical_width,
            logical_height,
            scale_factor,
            physical_width,
            physical_height,
            true, // focused — Document.hasFocus() is true at startup; refreshed by visibility events later
            animations,
            ref_dirty_flag,
            reactive,
            hooks,
            overlay_mgr,
            element_registry,
            ready_callbacks,
        );

        Ok(Self {
            canvas,
            surface,
            surface_config,
            blinc_app,
            ctx,
            last_logical_size: (logical_width, logical_height),
        })
    }

    /// Borrow the canvas the runner is rendering into.
    pub fn canvas(&self) -> &web_sys::HtmlCanvasElement {
        &self.canvas
    }

    /// Borrow the underlying [`BlincApp`].
    pub fn blinc_app(&self) -> &BlincApp {
        &self.blinc_app
    }

    /// Borrow the [`WindowedContext`] the user's UI builder will
    /// receive on each rebuild.
    pub fn context(&self) -> &WindowedContext {
        &self.ctx
    }

    /// Mutable access to the [`WindowedContext`].
    pub fn context_mut(&mut self) -> &mut WindowedContext {
        &mut self.ctx
    }

    /// Borrow the wgpu surface.
    pub fn surface(&self) -> &wgpu::Surface<'static> {
        &self.surface
    }

    /// Borrow the surface configuration. Phase 3b will mutate this on
    /// resize and call `surface.configure(...)` again.
    pub fn surface_config(&self) -> &wgpu::SurfaceConfiguration {
        &self.surface_config
    }
}

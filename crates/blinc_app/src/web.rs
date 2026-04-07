//! Web platform runner — `wasm32-unknown-unknown` only.
//!
//! Sibling of [`crate::windowed`] / [`crate::android`] / [`crate::ios`]
//! (and the Fuchsia stub) that owns the per-frame loop and browser
//! event wiring. The frame loop drives the same render pipeline the
//! desktop runner uses; only the *driver* differs:
//!
//! - **desktop**: winit `Frame::AboutToWait` → render → `request_redraw`
//! - **android**: native_activity `MainEvent::RequestRedraw` → render
//! - **ios**: `CADisplayLink` callback → render
//! - **web**: `window.requestAnimationFrame` → render → schedule next
//!
//! ## What's wired in this commit
//!
//! Phase 3a built the construction path. Phase 3b added
//! `AnimationScheduler::start_raf` so the scheduler owns rAF directly.
//! This commit (Phase 3c) wires the wake callback that actually renders
//! a frame: `WebApp::run(canvas_id, ui_builder)` builds everything,
//! installs a render closure as the scheduler's wake callback, enables
//! continuous redraw so the wake fires on every rAF tick, and returns
//! once the loop is wired. The rAF closure chain inside the scheduler
//! keeps running for the lifetime of the page.
//!
//! Phase 3d will add DOM event listeners so input events flow through
//! `EventRouter`. Phase 3e will add resize handling.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{atomic::AtomicBool, Arc, Mutex};

use blinc_animation::AnimationScheduler;
use blinc_core::context_state::HookState;
use blinc_core::reactive::ReactiveGraph;
use blinc_layout::div::Div;
use blinc_layout::renderer::RenderTree;
use blinc_layout::selector::ElementRegistry;
use blinc_layout::widgets::overlay::overlay_manager;
use wasm_bindgen::JsCast;

use crate::app::BlincApp;
use crate::error::{BlincError, Result};
use crate::windowed::{
    RefDirtyFlag, SharedAnimationScheduler, SharedElementRegistry, SharedReactiveGraph,
    SharedReadyCallbacks, WindowedContext,
};

/// User-supplied UI builder closure. Called once per rebuild with a
/// mutable reference to the runner's [`WindowedContext`]. Same shape
/// as `WindowBuilder` in [`crate::windowed`], minus the `Send` bound
/// (the web target is single-threaded).
type UiBuilder = Box<dyn FnMut(&mut WindowedContext) -> Div>;

/// Top-level web runner.
///
/// Owns the canvas, the wgpu surface and surface configuration, the
/// shared [`BlincApp`], the [`WindowedContext`] that the user-supplied
/// UI builder receives on each rebuild, and the cached render tree.
///
/// This struct is intentionally `!Send` — every browser API it touches
/// is single-threaded, and its sub-fields (`wgpu::Surface` on wasm32,
/// `web_sys::HtmlCanvasElement`) are `!Send` themselves.
pub struct WebApp {
    /// The HtmlCanvasElement we're rendering into. Held so we can
    /// re-read its size after a browser resize.
    #[allow(dead_code)]
    canvas: web_sys::HtmlCanvasElement,
    /// Wgpu surface + its configured properties.
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    /// The Blinc application core (renderer + text + render context).
    blinc_app: BlincApp,
    /// User-facing window context. Same shape every other platform builds.
    ctx: WindowedContext,
    /// User-supplied UI builder. Set via [`Self::set_ui_builder`] or
    /// [`Self::run`]. Called inside [`Self::run_one_frame`] when
    /// `needs_rebuild` is `true`.
    ui_builder: Option<UiBuilder>,
    /// Cached layout tree from the most recent rebuild. `None` until
    /// the first rebuild fires.
    current_tree: Option<RenderTree>,
    /// Whether the next frame needs to rebuild the tree before
    /// rendering. Set on initial setup, after a resize, or when the
    /// user explicitly requests a rebuild.
    needs_rebuild: bool,
    /// Last frame's logical width / height in CSS pixels. Used to
    /// detect resize events without having to query the canvas every
    /// frame (resize handling lands in Phase 3e).
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
        //    `WindowedApp::run` (windowed.rs ~line 2105). The scheduler
        //    is built fresh — its `start_raf()` driver gets kicked off
        //    in [`Self::start_frame_loop`] once the user has wired their
        //    rebuild + render callback.
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
            ui_builder: None,
            current_tree: None,
            needs_rebuild: true,
            last_logical_size: (logical_width, logical_height),
        })
    }

    /// Convenience all-in-one entry point: locate the canvas, build
    /// the runner, install the user's UI builder, wire a render
    /// closure as the scheduler's wake callback, enable continuous
    /// redraw, and start the rAF loop.
    ///
    /// Returns once the rAF chain is wired. The chain self-perpetuates
    /// from inside the browser, so the page keeps rendering after this
    /// future resolves.
    ///
    /// # Cycle / leak note
    ///
    /// This method intentionally creates an `Rc<RefCell<WebApp>>`
    /// cycle: the wake callback owns a clone of the `Rc`, the wake
    /// callback lives inside the scheduler, the scheduler lives inside
    /// the `WindowedContext`, and the context lives inside the
    /// `WebApp`. The cycle is what keeps everything alive past the
    /// return of this function. The browser tears it down on page
    /// unload, which is the expected lifecycle for a web app.
    pub async fn run<F>(canvas_id: &str, ui_builder: F) -> Result<()>
    where
        F: FnMut(&mut WindowedContext) -> Div + 'static,
    {
        let mut app = Self::new(canvas_id).await?;
        app.set_ui_builder(ui_builder);

        // Render the first frame synchronously so the canvas isn't
        // blank between `run().await` returning and the first rAF
        // tick (which can be ~16ms later, longer if the browser is
        // busy). Failures here are non-fatal — the next rAF tick will
        // try again.
        if let Err(e) = app.run_one_frame() {
            tracing::error!("WebApp::run initial frame failed: {e}");
        }

        // Wrap in Rc<RefCell<…>> so the wake closure can re-borrow
        // for each frame. The scheduler stores the wake callback as
        // `Arc<dyn Fn()>`; on wasm32 there's no `Send + Sync` bound,
        // so it can capture the `!Send` Rc.
        let app_rc = Rc::new(RefCell::new(app));
        let app_for_wake = Rc::clone(&app_rc);

        // The wake callback re-borrows the app and runs one frame.
        // `try_borrow_mut` (rather than `borrow_mut`) keeps us safe
        // if a future Phase 3d input handler is mid-mutation when the
        // rAF tick fires — we just skip the frame and try again next
        // tick rather than panicking on borrow conflict.
        let wake = move || {
            if let Ok(mut app) = app_for_wake.try_borrow_mut() {
                if let Err(e) = app.run_one_frame() {
                    tracing::error!("WebApp wake-frame failed: {e}");
                }
            }
        };

        // Install the wake callback and enable continuous redraw so
        // the wake fires on every rAF tick (not just when an animation
        // is active). For a UI runtime, "render every frame the
        // browser asks for" is the right default — see windowed.rs
        // for the equivalent on desktop.
        //
        // We clone the scheduler `Arc` rather than holding the
        // `RefCell` borrow open across the `Mutex::lock()` — the
        // `MutexGuard` temporary that `if let Ok(...) = ...` produces
        // outlives the `RefCell` borrow otherwise, and the borrow
        // checker correctly rejects the drop ordering.
        let scheduler_arc = Arc::clone(&app_rc.borrow().ctx.animations);
        if let Ok(mut scheduler) = scheduler_arc.lock() {
            scheduler.set_wake_callback(wake);
            scheduler.set_continuous_redraw(true);
        }

        // Kick off the rAF chain. From here on the browser drives
        // every frame; this future returns immediately and the runtime
        // drops it.
        if let Ok(scheduler) = scheduler_arc.lock() {
            scheduler.start_raf();
        }

        // Don't return `app_rc` — let the cycle keep it alive. (See
        // the function-level "Cycle / leak note" doc comment.)
        Ok(())
    }

    /// Install (or replace) the UI builder closure.
    ///
    /// Sets `needs_rebuild = true` so the next [`Self::run_one_frame`]
    /// call rebuilds the tree from the new builder.
    pub fn set_ui_builder<F>(&mut self, builder: F)
    where
        F: FnMut(&mut WindowedContext) -> Div + 'static,
    {
        self.ui_builder = Some(Box::new(builder));
        self.needs_rebuild = true;
    }

    /// Mark the tree as dirty so the next frame rebuilds it.
    pub fn request_rebuild(&mut self) {
        self.needs_rebuild = true;
    }

    /// Render exactly one frame: rebuild the tree if dirty, acquire
    /// the next surface texture, render through `BlincApp`, and
    /// present.
    ///
    /// Called from the scheduler's wake callback (driven by rAF) and
    /// once synchronously from [`Self::run`] to avoid a blank-canvas
    /// gap between init and the first rAF tick.
    ///
    /// Errors here do NOT abort the loop — the scheduler will call
    /// us again on the next tick. Phase 3d's input handlers will
    /// also call this directly to force a render after a click /
    /// keypress.
    pub fn run_one_frame(&mut self) -> Result<()> {
        // 1. Rebuild the tree if needed. Splits the borrow so we can
        //    pass &mut ctx to the user's builder while &mut self.ui_builder
        //    is also live.
        if self.needs_rebuild {
            let builder = match self.ui_builder.as_mut() {
                Some(b) => b,
                None => {
                    // No builder yet — nothing to render. Not an error;
                    // the user just hasn't called `set_ui_builder`.
                    return Ok(());
                }
            };
            let element = builder(&mut self.ctx);

            // Build a fresh render tree from the element. The shared
            // element registry is kept across rebuilds so id-based
            // queries stay stable.
            let registry = Arc::clone(self.ctx.element_registry());
            let mut tree = RenderTree::from_element_with_registry(&element, registry);
            tree.compute_layout(self.ctx.width, self.ctx.height);
            self.current_tree = Some(tree);
            self.needs_rebuild = false;
            self.ctx.rebuild_count = self.ctx.rebuild_count.saturating_add(1);
        }

        // 2. Render the tree to the next surface texture. If we don't
        //    have a tree yet (no builder set), bail out gracefully.
        let tree = match self.current_tree.as_ref() {
            Some(t) => t,
            None => return Ok(()),
        };

        let frame = self
            .surface
            .get_current_texture()
            .map_err(|e| BlincError::Render(format!("get_current_texture failed: {e}")))?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let physical_w = self.surface_config.width;
        let physical_h = self.surface_config.height;
        self.blinc_app
            .render_tree(tree, &view, physical_w, physical_h)?;

        frame.present();
        Ok(())
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

    /// Borrow the surface configuration. Phase 3e will mutate this on
    /// resize and call `surface.configure(...)` again.
    pub fn surface_config(&self) -> &wgpu::SurfaceConfiguration {
        &self.surface_config
    }

    /// Borrow the shared animation scheduler.
    ///
    /// Use this to install a wake callback before calling
    /// [`Self::start_frame_loop`] — the scheduler invokes the wake
    /// callback on every tick where animations are active OR
    /// continuous redraw is requested. The wake callback is what
    /// actually renders a frame; the scheduler doesn't know about
    /// wgpu surfaces.
    pub fn scheduler(&self) -> &crate::windowed::SharedAnimationScheduler {
        &self.ctx.animations
    }

    /// Hand control of the per-frame loop over to
    /// [`AnimationScheduler::start_raf`].
    ///
    /// This is the wasm32 sibling of the desktop event-loop pump.
    /// `start_raf` installs a `requestAnimationFrame` chain that ticks
    /// the scheduler once per browser frame and invokes the wake
    /// callback whenever there's something to render. Returning from
    /// this method DOES NOT mean the loop is over — the rAF closure
    /// chain self-perpetuates from inside the browser. Returning just
    /// means "the loop is wired up; the runtime can drop the
    /// constructing future".
    ///
    /// Wire your wake callback via [`Self::scheduler`] *before*
    /// calling this — once `start_raf` returns, the chain is already
    /// firing.
    ///
    /// Most apps should use [`Self::run`] instead — it does setup,
    /// wake-callback wiring, and `start_raf` in one call.
    pub fn start_frame_loop(&self) {
        if let Ok(scheduler) = self.ctx.animations.lock() {
            scheduler.start_raf();
        }
    }
}

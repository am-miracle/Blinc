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
use blinc_core::context_state::{BlincContextState, HookState};
use blinc_core::reactive::{ReactiveGraph, SignalId};
use blinc_layout::div::Div;
use blinc_layout::renderer::RenderTree;
use blinc_layout::selector::ElementRegistry;
use blinc_layout::widgets::overlay::overlay_manager;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

use crate::app::BlincApp;
use crate::error::{BlincError, Result};
use crate::windowed::{
    RefDirtyFlag, SharedAnimationScheduler, SharedElementRegistry, SharedReactiveGraph,
    SharedReadyCallbacks, WindowedContext,
};

/// Convert a [`blinc_platform::MouseButton`] (the wasm-side input
/// helper output) into the [`blinc_layout::event_router::MouseButton`]
/// the dispatch path consumes. Mirrors `convert_button` from the
/// desktop runner at `windowed.rs:2312`.
fn convert_layout_button(
    b: blinc_platform::MouseButton,
) -> blinc_layout::event_router::MouseButton {
    match b {
        blinc_platform::MouseButton::Left => blinc_layout::event_router::MouseButton::Left,
        blinc_platform::MouseButton::Right => blinc_layout::event_router::MouseButton::Right,
        blinc_platform::MouseButton::Middle => blinc_layout::event_router::MouseButton::Middle,
        blinc_platform::MouseButton::Back => blinc_layout::event_router::MouseButton::Back,
        blinc_platform::MouseButton::Forward => blinc_layout::event_router::MouseButton::Forward,
        blinc_platform::MouseButton::Other(n) => blinc_layout::event_router::MouseButton::Other(n),
    }
}

/// User-supplied UI builder closure. Called once per rebuild with a
/// mutable reference to the runner's [`WindowedContext`]. Same shape
/// as `WindowBuilder` in [`crate::windowed`], minus the `Send` bound
/// (the web target is single-threaded).
type UiBuilder = Box<dyn FnMut(&mut WindowedContext) -> Div>;

/// Milliseconds since the runner was first constructed. Backed by a
/// `web_time::Instant` so the clock is monotonic on both native
/// (where `web_time::Instant` re-exports `std::time::Instant`) and
/// wasm32 (where it wraps `performance.now()`).
///
/// Used as the `current_time` argument to
/// `RenderTree::tick_scroll_physics`. The epoch is per-app, not
/// absolute, but every consumer is comparing deltas so that's all
/// we need.
fn now_ms() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<web_time::Instant> = OnceLock::new();
    let start = START.get_or_init(web_time::Instant::now);
    start.elapsed().as_millis() as u64
}


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
    /// Whether the next frame needs to re-run the user's UI builder
    /// before rendering. Set when an event handler marks the tree
    /// dirty, when the user explicitly requests a rebuild, or when
    /// `take_needs_rebuild` flips the global widget rebuild flag.
    needs_rebuild: bool,
    /// Whether the next rebuild must bypass `incremental_update` and
    /// fall back to a full `from_element_with_registry`. Set by
    /// [`Self::handle_resize`] because viewport-size changes don't
    /// propagate cleanly through the incremental path — parent
    /// constraints have to be re-derived from scratch for the new
    /// dimensions or you get the old layout stretched into the new
    /// viewport. Mirrors `ws.needs_relayout` on the desktop runner
    /// at [`windowed.rs:3684`](crate::windowed).
    needs_full_rebuild: bool,
    /// Last frame's logical width / height in CSS pixels. Used by
    /// [`Self::handle_resize`] to short-circuit `window.resize` events
    /// that don't actually correspond to a canvas size change (devtools
    /// toggle, focus changes, …).
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

        // Initialize the global `BlincContextState` singleton with
        // this runner's reactive graph, hook state, and dirty flag —
        // exactly the same call the desktop runner makes at
        // [`windowed.rs:2114`](crate::windowed). Without this,
        // every component that reaches for `BlincContextState::get()`
        // (which is every `ctx.use_state*`, every `Stateful::on_state`
        // body, every `State::set`, every signal-driven rebuild
        // path, …) panics or no-ops because the singleton is
        // uninitialized. The previous web runner created the four
        // shared collaborators and stuffed them into `WindowedContext`
        // but never wired them into the global, so reactive state
        // worked through `ctx.*` directly but `Stateful` widgets
        // and the implicit-context APIs all silently failed.
        if !BlincContextState::is_initialized() {
            #[allow(clippy::type_complexity)]
            let stateful_callback: Arc<dyn Fn(&[SignalId]) + Send + Sync> =
                Arc::new(|signal_ids| {
                    blinc_layout::check_stateful_deps(signal_ids);
                });
            BlincContextState::init_with_callback(
                Arc::clone(&reactive),
                Arc::clone(&hooks),
                Arc::clone(&ref_dirty_flag),
                stateful_callback,
            );
        }
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
            needs_full_rebuild: false,
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
    /// Apps that need to load fonts, register CSS, or otherwise touch
    /// the runner before the first frame should use [`Self::run_with_setup`]
    /// instead — `run` is a thin wrapper that passes a no-op setup.
    pub async fn run<F>(canvas_id: &str, ui_builder: F) -> Result<()>
    where
        F: FnMut(&mut WindowedContext) -> Div + 'static,
    {
        Self::run_with_setup(canvas_id, |_| {}, ui_builder).await
    }

    /// Same as [`Self::run`], plus a synchronous `setup` callback that
    /// runs after the runner is constructed and before the first frame
    /// is rendered. This is the canonical place to:
    ///
    /// - Load bundled font bytes via [`Self::load_font_data`]. Required
    ///   for any text to render — the wasm32 init path skips system
    ///   font discovery (no filesystem) so the font registry starts
    ///   empty.
    /// - Register CSS via `app.context_mut().add_css(...)`.
    /// - Wire up any other one-shot config that touches the runner.
    ///
    /// The setup callback receives a `&mut WebApp` and runs exactly
    /// once. It cannot be `async`; if you need fetch-based asset
    /// loading, do it BEFORE calling `run_with_setup` and pass the
    /// fetched bytes through your closure's environment.
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
    pub async fn run_with_setup<S, F>(canvas_id: &str, setup: S, ui_builder: F) -> Result<()>
    where
        S: FnOnce(&mut Self),
        F: FnMut(&mut WindowedContext) -> Div + 'static,
    {
        let mut app = Self::new(canvas_id).await?;

        // Run user setup BEFORE installing the UI builder. This is
        // when fonts get loaded, CSS gets registered, etc. If setup
        // panics, we never reach the rAF loop and the browser's
        // panic-hook surfaces it in the console.
        setup(&mut app);

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

        // Install browser DOM event listeners. They share the same
        // `Rc<RefCell<WebApp>>` cycle as the wake callback. The
        // `try_borrow_mut` guard inside each handler dodges
        // reentrancy with the rAF wake callback (which holds its own
        // clone): if the rAF tick is mid-render when an event fires,
        // we drop that one event rather than panicking — the next
        // event will succeed.
        Self::install_input_listeners(Rc::clone(&app_rc))?;

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

    /// Load font data from a byte buffer into the underlying
    /// [`BlincApp`]'s font registry.
    ///
    /// Returns the number of font faces registered (a single TTF
    /// usually has one face; TTC collections have several). Call this
    /// from a [`Self::run_with_setup`] setup closure with bundled
    /// `include_bytes!(...)` data, or with bytes fetched via
    /// `WebAssetLoader::preload`.
    ///
    /// **You must call this for at least one font.** The wasm32 init
    /// path deliberately skips system font discovery (no filesystem),
    /// so the font registry starts empty. Without a registered font,
    /// every text element fails to shape glyphs and renders as nothing.
    /// This is symmetric with how `BlincApp::with_canvas` documents the
    /// font situation.
    pub fn load_font_data(&mut self, bytes: Vec<u8>) -> usize {
        self.blinc_app.load_font_data_to_registry(bytes)
    }

    /// Install browser DOM event listeners that route input through the
    /// shared [`WindowedContext::event_router`] and dispatch the
    /// resulting events through the cached render tree.
    ///
    /// This is the wasm32 sibling of the desktop runner's input pump
    /// (`windowed.rs:2326+`). Same contract:
    /// - Mouse coords arrive in CSS pixels (which are also logical
    ///   pixels for our purposes — the canvas's `client_width`/
    ///   `client_height` are CSS pixels, and the renderer's layout
    ///   thinks in logical pixels).
    /// - `EventRouter::on_mouse_*` returns a `Vec<(LayoutNodeId, u32)>`
    ///   of events that need to be dispatched through
    ///   `RenderTree::dispatch_event` to actually fire user handlers.
    /// - Keyboard events use the legacy DOM `keyCode` (8 = Backspace,
    ///   13 = Enter, 27 = Escape, 65-90 = A-Z, etc.) which is what the
    ///   `EventRouter::on_key_*` API takes — no enum conversion needed.
    ///
    /// Each closure captures an `Rc<RefCell<WebApp>>` clone and uses
    /// `try_borrow_mut` to dodge reentrancy with the rAF wake callback
    /// (which holds its own clone). If the borrow fails, the event is
    /// dropped — the next event of the same kind will succeed.
    /// `Closure::forget()` deliberately leaks each closure for the
    /// lifetime of the app, matching the rAF chain leak in
    /// [`AnimationScheduler::start_raf`].
    ///
    /// Keyboard listeners are installed on `document` rather than the
    /// canvas because canvases don't get keyboard focus without
    /// `tabindex` shenanigans, and `document` events are reliably
    /// delivered.
    fn install_input_listeners(app_rc: Rc<RefCell<Self>>) -> Result<()> {
        let window = web_sys::window().ok_or_else(|| {
            BlincError::Platform(
                "WebApp::install_input_listeners called without a global `window` object"
                    .to_string(),
            )
        })?;
        let document = window.document().ok_or_else(|| {
            BlincError::Platform(
                "WebApp::install_input_listeners called without a `document` object".to_string(),
            )
        })?;
        // Borrow once to get a clone of the canvas reference. The
        // canvas itself lives inside the WebApp, but `add_event_listener`
        // only needs the EventTarget for the lifetime of the call —
        // the closures we attach own the routing back into the WebApp.
        let canvas = app_rc.borrow().canvas.clone();

        // ----- mousemove -----
        {
            let app_rc = Rc::clone(&app_rc);
            let closure = Closure::<dyn FnMut(_)>::new(move |evt: web_sys::MouseEvent| {
                if let Ok(mut app) = app_rc.try_borrow_mut() {
                    let x = evt.offset_x() as f32;
                    let y = evt.offset_y() as f32;
                    Self::dispatch_mouse_move(&mut app, x, y);
                }
            });
            canvas
                .add_event_listener_with_callback("mousemove", closure.as_ref().unchecked_ref())
                .map_err(|e| {
                    BlincError::Platform(format!("add mousemove listener failed: {e:?}"))
                })?;
            closure.forget();
        }

        // ----- mousedown -----
        {
            let app_rc = Rc::clone(&app_rc);
            let closure = Closure::<dyn FnMut(_)>::new(move |evt: web_sys::MouseEvent| {
                if let Ok(mut app) = app_rc.try_borrow_mut() {
                    let x = evt.offset_x() as f32;
                    let y = evt.offset_y() as f32;
                    let button = blinc_platform_web::input::convert_mouse_button(evt.button());
                    Self::dispatch_mouse_down(&mut app, x, y, button);
                }
            });
            canvas
                .add_event_listener_with_callback("mousedown", closure.as_ref().unchecked_ref())
                .map_err(|e| {
                    BlincError::Platform(format!("add mousedown listener failed: {e:?}"))
                })?;
            closure.forget();
        }

        // ----- mouseup -----
        {
            let app_rc = Rc::clone(&app_rc);
            let closure = Closure::<dyn FnMut(_)>::new(move |evt: web_sys::MouseEvent| {
                if let Ok(mut app) = app_rc.try_borrow_mut() {
                    let x = evt.offset_x() as f32;
                    let y = evt.offset_y() as f32;
                    let button = blinc_platform_web::input::convert_mouse_button(evt.button());
                    Self::dispatch_mouse_up(&mut app, x, y, button);
                }
            });
            canvas
                .add_event_listener_with_callback("mouseup", closure.as_ref().unchecked_ref())
                .map_err(|e| BlincError::Platform(format!("add mouseup listener failed: {e:?}")))?;
            closure.forget();
        }

        // ----- wheel -----
        {
            let app_rc = Rc::clone(&app_rc);
            let closure = Closure::<dyn FnMut(_)>::new(move |evt: web_sys::WheelEvent| {
                // Prevent the page from scrolling under the canvas
                // when the user wheels over it. Apps that want page
                // scrolling can revisit this in a future config knob.
                evt.prevent_default();
                if let Ok(mut app) = app_rc.try_borrow_mut() {
                    // Normalise wheel delta to pixels. delta_mode 0 is
                    // pixels (most browsers); 1 is lines (Firefox
                    // legacy); 2 is pages.
                    let multiplier: f32 = match evt.delta_mode() {
                        0 => 1.0,            // pixels
                        1 => 16.0,           // line ≈ 16px
                        2 => app.ctx.height, // page = viewport height
                        _ => 1.0,
                    };
                    let dx = -(evt.delta_x() as f32) * multiplier;
                    let dy = -(evt.delta_y() as f32) * multiplier;
                    Self::dispatch_scroll(&mut app, dx, dy);
                }
            });
            canvas
                .add_event_listener_with_callback("wheel", closure.as_ref().unchecked_ref())
                .map_err(|e| BlincError::Platform(format!("add wheel listener failed: {e:?}")))?;
            closure.forget();
        }

        // ----- keydown (on document, not canvas) -----
        {
            let app_rc = Rc::clone(&app_rc);
            let closure = Closure::<dyn FnMut(_)>::new(move |evt: web_sys::KeyboardEvent| {
                if let Ok(mut app) = app_rc.try_borrow_mut() {
                    // The DOM `keyCode` attribute returns the legacy
                    // virtual-key code (8 = Backspace, 13 = Enter,
                    // 27 = Escape, 65-90 = A-Z, …). This matches the
                    // codes the desktop runner builds in
                    // `windowed.rs:3052` exactly, so the same widget
                    // key shortcuts work without translation.
                    let key_code = evt.key_code();
                    Self::dispatch_key_down(&mut app, key_code);

                    // For printable single-character keys, also
                    // dispatch TEXT_INPUT so editor widgets can
                    // observe the typed character. The `key()` value
                    // is the W3C key string ("a", "Hello", "Enter"…);
                    // we only forward single-character non-control
                    // values, and only when no Ctrl/Cmd is held
                    // (matches the desktop runner's behaviour).
                    let key_str = evt.key();
                    let mut chars = key_str.chars();
                    if let (Some(ch), None) = (chars.next(), chars.next()) {
                        if !ch.is_control() && !evt.ctrl_key() && !evt.meta_key() {
                            Self::dispatch_text_input(&mut app, ch);
                        }
                    }
                }
            });
            document
                .add_event_listener_with_callback("keydown", closure.as_ref().unchecked_ref())
                .map_err(|e| BlincError::Platform(format!("add keydown listener failed: {e:?}")))?;
            closure.forget();
        }

        // ----- keyup (on document) -----
        {
            let app_rc = Rc::clone(&app_rc);
            let closure = Closure::<dyn FnMut(_)>::new(move |evt: web_sys::KeyboardEvent| {
                if let Ok(mut app) = app_rc.try_borrow_mut() {
                    let key_code = evt.key_code();
                    Self::dispatch_key_up(&mut app, key_code);
                }
            });
            document
                .add_event_listener_with_callback("keyup", closure.as_ref().unchecked_ref())
                .map_err(|e| BlincError::Platform(format!("add keyup listener failed: {e:?}")))?;
            closure.forget();
        }

        // ----- resize (on window) -----
        //
        // `window.resize` fires for any viewport change — browser-window
        // resize, devtools toggle, fullscreen enter/exit, orientation
        // change. The actual diff against the previous canvas size lives
        // inside `handle_resize`, which bails when nothing changed.
        //
        // The listener has to attach to `window`, not `canvas`: a CSS
        // `width: 100%` canvas only sees its own dimensions change as a
        // side-effect of the window resizing, and there is no DOM event
        // for "an element's CSS-computed size changed" outside of
        // `ResizeObserver` (which we can adopt later if apps need to
        // react to non-window-driven layout shifts).
        {
            let app_rc = Rc::clone(&app_rc);
            let closure = Closure::<dyn FnMut(_)>::new(move |_evt: web_sys::Event| {
                if let Ok(mut app) = app_rc.try_borrow_mut() {
                    Self::handle_resize(&mut app);
                }
            });
            window
                .add_event_listener_with_callback("resize", closure.as_ref().unchecked_ref())
                .map_err(|e| BlincError::Platform(format!("add resize listener failed: {e:?}")))?;
            closure.forget();
        }

        Ok(())
    }

    // ===========================================================================
    // Per-event dispatch helpers
    // ===========================================================================
    //
    // Each helper takes a `&mut WebApp` (already borrowed mutably by
    // the calling closure) and runs the EventRouter call → dispatch
    // pending events through the cached render tree. Factored out so
    // every event-handler closure stays a one-liner.

    fn dispatch_mouse_move(app: &mut Self, x: f32, y: f32) {
        let tree = match app.current_tree.as_ref() {
            Some(t) => t,
            None => return,
        };
        let pending = app.ctx.event_router.on_mouse_move(tree, x, y);
        Self::dispatch_pending(app, pending);
    }

    fn dispatch_mouse_down(app: &mut Self, x: f32, y: f32, button: blinc_platform::MouseButton) {
        let tree = match app.current_tree.as_ref() {
            Some(t) => t,
            None => return,
        };
        let pending = app
            .ctx
            .event_router
            .on_mouse_down(tree, x, y, convert_layout_button(button));
        Self::dispatch_pending(app, pending);
    }

    fn dispatch_mouse_up(app: &mut Self, x: f32, y: f32, button: blinc_platform::MouseButton) {
        let tree = match app.current_tree.as_ref() {
            Some(t) => t,
            None => return,
        };
        let pending = app
            .ctx
            .event_router
            .on_mouse_up(tree, x, y, convert_layout_button(button));
        Self::dispatch_pending(app, pending);
    }

    fn dispatch_scroll(app: &mut Self, delta_x: f32, delta_y: f32) {
        // Hit-test under the cursor first (immutable borrow), then
        // walk the chain of scroll containers from leaf to root via
        // `dispatch_scroll_chain`. This is the same path the desktop
        // runner takes ([`windowed.rs:3327-3340`](crate::windowed))
        // and is what *actually moves* the scroll position — the
        // simpler `EventRouter::on_scroll` only emits a SCROLL bubble
        // event, it does not advance scroll physics.
        //
        // Note: the `scroll()` builder defaults to **bounce-disabled**
        // on wasm32 (see `widgets/scroll.rs::scroll`), so there is
        // intentionally no inline `on_gesture_end` here — without a
        // reliable `ScrollPhase::Ended` from the DOM there is no
        // safe way to fire bounce-back without producing either a
        // ~1s lag (wait for the OS-momentum tail to subside) or
        // visible wobble (restart the spring as each momentum wheel
        // re-overscrolls a settled `Idle` scroll). Web users that
        // actually want bounce can opt in via
        // `Scroll::with_config(ScrollConfig::default())`.
        let hit = {
            let tree = match app.current_tree.as_ref() {
                Some(t) => t,
                None => return,
            };
            app.ctx.event_router.on_scroll_nested(tree, delta_x, delta_y)
        };

        let Some(hit) = hit else {
            // Cursor isn't over any element — nothing to scroll. This
            // happens before the user has moved the mouse over the
            // canvas (mouse_position defaults to (0, 0)).
            return;
        };

        let (mx, my) = app.ctx.event_router.mouse_position();
        if let Some(tree) = app.current_tree.as_mut() {
            tree.dispatch_scroll_chain(hit.node, &hit.ancestors, mx, my, delta_x, delta_y);
        }
    }

    fn dispatch_key_down(app: &mut Self, key_code: u32) {
        if let Some((node, event_type)) = app.ctx.event_router.on_key_down(key_code) {
            let (mx, my) = app.ctx.event_router.mouse_position();
            if let Some(tree) = app.current_tree.as_mut() {
                tree.dispatch_event(node, event_type, mx, my);
            }
        }
    }

    fn dispatch_key_up(app: &mut Self, key_code: u32) {
        if let Some((node, event_type)) = app.ctx.event_router.on_key_up(key_code) {
            let (mx, my) = app.ctx.event_router.mouse_position();
            if let Some(tree) = app.current_tree.as_mut() {
                tree.dispatch_event(node, event_type, mx, my);
            }
        }
    }

    fn dispatch_text_input(app: &mut Self, ch: char) {
        if let Some((node, event_type)) = app.ctx.event_router.on_text_input(ch) {
            let (mx, my) = app.ctx.event_router.mouse_position();
            if let Some(tree) = app.current_tree.as_mut() {
                tree.dispatch_event(node, event_type, mx, my);
            }
        }
    }

    /// Dispatch a batch of pending events through the cached render
    /// tree. Mouse handlers all use this — the EventRouter returns a
    /// list of (node, event_type) pairs that the tree's handler
    /// registry needs to walk through individually.
    fn dispatch_pending(app: &mut Self, pending: Vec<(blinc_layout::tree::LayoutNodeId, u32)>) {
        if pending.is_empty() {
            return;
        }
        let (mx, my) = app.ctx.event_router.mouse_position();
        if let Some(tree) = app.current_tree.as_mut() {
            for (node, event_type) in pending {
                tree.dispatch_event(node, event_type, mx, my);
            }
        }
    }

    /// Re-read the canvas's CSS dimensions and `devicePixelRatio`,
    /// resize the GPU framebuffer + surface configuration to match,
    /// update the [`WindowedContext`] dimensions, and mark the tree
    /// for rebuild on the next frame.
    ///
    /// Called from the `resize` event handler installed by
    /// [`Self::install_input_listeners`]. Skips work entirely when
    /// the logical dimensions haven't actually changed since the last
    /// resize — `window.resize` fires for things like browser-tab
    /// activation and devtools-toggle that don't actually change the
    /// canvas size.
    ///
    /// Zero-size guards: a 0×0 canvas (which can happen during
    /// fullscreen transitions or before CSS layout settles) would
    /// produce a wgpu validation error from `surface.configure(...)`.
    /// We bail early in that case and wait for a real resize event
    /// to arrive.
    fn handle_resize(app: &mut Self) {
        let window = match web_sys::window() {
            Some(w) => w,
            None => return,
        };

        let logical_width = app.canvas.client_width() as f32;
        let logical_height = app.canvas.client_height() as f32;
        if logical_width <= 0.0 || logical_height <= 0.0 {
            // Canvas is currently zero-sized — typical during fullscreen
            // transitions or before initial layout. Skip until a real
            // resize event lands.
            return;
        }
        let scale_factor = window.device_pixel_ratio();

        // Skip if nothing actually changed. `window.resize` fires for
        // many non-resize events (devtools toggle, focus changes…).
        let (last_w, last_h) = app.last_logical_size;
        let last_dpr = app.ctx.scale_factor;
        if (last_w - logical_width).abs() < 0.5
            && (last_h - logical_height).abs() < 0.5
            && (last_dpr - scale_factor).abs() < 0.001
        {
            return;
        }

        let physical_width = (logical_width * scale_factor as f32).round().max(1.0);
        let physical_height = (logical_height * scale_factor as f32).round().max(1.0);

        // Resize the canvas's GPU framebuffer to match the new
        // physical dimensions. The CSS size is what the browser
        // already laid out for us; we have to push the matching
        // pixel size into the canvas's `width`/`height` attributes
        // before reconfiguring the wgpu surface.
        app.canvas.set_width(physical_width as u32);
        app.canvas.set_height(physical_height as u32);

        // Update surface config and re-configure. wgpu requires a
        // configure call any time the size changes, otherwise
        // `get_current_texture` returns `Outdated` on the next frame.
        app.surface_config.width = physical_width as u32;
        app.surface_config.height = physical_height as u32;
        app.surface
            .configure(app.blinc_app.device(), &app.surface_config);

        // Update WindowedContext dimensions so the user's UI builder
        // sees the new logical size on the next rebuild. The renderer
        // also reads `tree.scale_factor()` which we set per-frame in
        // `run_one_frame`, so changing the DPR mid-resize is handled
        // automatically by the next rebuild.
        app.ctx.width = logical_width;
        app.ctx.height = logical_height;
        app.ctx.scale_factor = scale_factor;
        app.ctx.physical_width = physical_width;
        app.ctx.physical_height = physical_height;
        app.last_logical_size = (logical_width, logical_height);

        // Force a rebuild so the layout pass uses the new viewport
        // dimensions on the next rAF tick. `needs_full_rebuild`
        // bypasses the `incremental_update` path because viewport-
        // size changes don't propagate parent constraints cleanly
        // through it — desktop does the same at
        // [`windowed.rs:3684`](crate::windowed).
        app.needs_rebuild = true;
        app.needs_full_rebuild = true;
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
        // 1. Tick scroll physics on the existing tree BEFORE any
        //    rebuild. This advances momentum / bounce / spring-back
        //    one step every rAF tick — without it, the wheel input
        //    moves the position once and then everything freezes
        //    because the physics never gets a chance to step. The
        //    desktop runner does the same at
        //    [`windowed.rs:3492`](crate::windowed). The current_time
        //    units are milliseconds since app start; `web_time`
        //    gives us a monotonic clock that works on both native
        //    and wasm32.
        let now = now_ms();
        if let Some(ref mut tree) = self.current_tree {
            tree.tick_scroll_physics(now);
        }

        // 2. Detect rebuild triggers. Mirrors the desktop runner's
        //    Phase 1 polling at [`windowed.rs:3500-3535`](crate::windowed)
        //    but trimmed to the subset that's wired up on wasm32. Each
        //    `if` is independent — the first `true` branch wins and
        //    the rest still execute (for the side effect of clearing
        //    their respective dirty flags).
        //
        //    - `tree.needs_rebuild()` catches widgets that called
        //      `dirty_tracker.mark_dirty(...)` from inside an event
        //      handler (Stateful::on_state, click handlers that mutate
        //      element state, etc.).
        //
        //    - `widgets::take_needs_rebuild()` catches the global
        //      `NEEDS_REBUILD` atomic that text widgets and the
        //      stateful registry flip when their internal state
        //      changes (text input focus, cursor movement, …).
        //
        //    Without this block, drag handlers and Stateful containers
        //    can fire all they want — the runner never re-evaluates
        //    the user's UI builder, so nothing visibly changes on
        //    screen.
        if let Some(ref tree) = self.current_tree {
            if tree.needs_rebuild() {
                self.needs_rebuild = true;
            }
        }
        if blinc_layout::widgets::take_needs_rebuild() {
            self.needs_rebuild = true;
        }
        // The reactive `State::set` path flips this atomic via the
        // `BlincContextState` singleton. Desktop polls it at
        // [`windowed.rs:3513`](crate::windowed) under the same name.
        // Without this poll, every `state.set(new_value)` call from
        // a click / drag handler would correctly mutate the state
        // cell but never trigger a tree rebuild, so the new value
        // would never make it onto the screen.
        if self
            .ctx
            .dirty_flag()
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.needs_rebuild = true;
        }

        // 3. Rebuild / incrementally update the tree if needed.
        //    Mirrors the desktop runner's flow at
        //    [`windowed.rs:3653-3795`](crate::windowed): on the first
        //    frame (no existing tree) we do a full build via
        //    `from_element_with_registry`; on subsequent dirty
        //    frames we hand the new element tree to
        //    `RenderTree::incremental_update`, which preserves all
        //    accumulated state (scroll_physics, scroll_offsets,
        //    node_states, motion_bindings, dirty tracker, …) and
        //    only rebuilds the subtrees whose hashes actually
        //    changed.
        //
        //    Splits the borrow so we can pass `&mut ctx` to the
        //    user's builder while `&mut self.ui_builder` is also
        //    live.
        if self.needs_rebuild {
            let builder = match self.ui_builder.as_mut() {
                Some(b) => b,
                None => {
                    // No builder yet — nothing to render. Not an error;
                    // the user just hasn't called `set_ui_builder`.
                    return Ok(());
                }
            };

            // Reset per-call-site index counters so `InstanceKey::new`
            // (and everything that builds on it — `scroll()`, the
            // stateful registry, the auto-persisted scroll-physics
            // store) can map a call site at the same source location
            // to the same key across rebuilds. Mirrors
            // `windowed.rs:3655` exactly. Without this, every rebuild
            // would assign fresh InstanceKeys → fresh physics →
            // scroll position resets on every resize.
            blinc_layout::reset_call_counters();
            // Same lifecycle hooks the desktop runner clears at
            // `windowed.rs:3657-3658` — stale Stateful base-prop
            // updaters and click-outside handlers from the previous
            // tree have to be dropped before the new builder runs.
            blinc_layout::clear_stateful_base_updaters();
            blinc_layout::click_outside::clear_click_outside_handlers();

            let element = builder(&mut self.ctx);

            // `needs_full_rebuild` is the resize escape hatch — see
            // `handle_resize`. Viewport-size changes don't propagate
            // parent constraints cleanly through `incremental_update`,
            // so we throw away the existing tree and build fresh.
            // Desktop does the same at
            // [`windowed.rs:3684-3738`](crate::windowed).
            if self.needs_full_rebuild {
                self.current_tree = None;
                self.needs_full_rebuild = false;
            }

            if let Some(ref mut existing_tree) = self.current_tree {
                // Incremental update path. The framework hashes the
                // new element tree against the stored
                // per-node hashes and applies the minimal possible
                // change set:
                //
                //   NoChanges      → nothing — early-out, render the
                //                    existing tree as-is.
                //   VisualOnly     → render-prop updates were applied
                //                    in place; no relayout needed.
                //   LayoutChanged  → render-prop updates applied in
                //                    place, but layout dimensions
                //                    moved → recompute layout.
                //   ChildrenChanged → subtrees were rebuilt in place,
                //                    layout must be recomputed.
                //
                // This is the same match the desktop runner does at
                // [`windowed.rs:3748-3795`](crate::windowed). Doing
                // a full `RenderTree::from_element_with_registry`
                // here instead would throw away all the live tree
                // state (scroll_physics, node_states, motion bindings,
                // dirty tracker, …) on every dirty trigger — that's
                // why scroll position used to snap back on click.
                use blinc_layout::UpdateResult;
                match existing_tree.incremental_update(&element) {
                    UpdateResult::NoChanges | UpdateResult::VisualOnly => {
                        // Nothing to relayout; render path picks up
                        // the in-place prop updates.
                    }
                    UpdateResult::LayoutChanged | UpdateResult::ChildrenChanged => {
                        existing_tree.compute_layout(self.ctx.width, self.ctx.height);
                    }
                }
                // Clear the dirty tracker now that we've consumed
                // its signal — without this, the next frame's
                // `tree.needs_rebuild()` poll would still return
                // `true` and we'd loop forever.
                existing_tree.clear_dirty();
            } else {
                // First-frame build path. No tree to update yet, so
                // construct a fresh one and wire all the per-tree
                // services (scheduler weak ref for scroll-bounce
                // springs, DPI scale, layout) before stashing it in
                // `current_tree`.
                let registry = Arc::clone(self.ctx.element_registry());
                let mut tree = RenderTree::from_element_with_registry(&element, registry);

                // Wire the AnimationScheduler weak ref into the tree.
                // Internally `set_animations` walks the existing
                // `scroll_physics` map and calls `set_scheduler` on
                // each entry, which is what gives the bounce-spring
                // path a live `Weak<Mutex<AnimationScheduler>>` to
                // upgrade inside `ScrollPhysics::tick`. The desktop
                // runner makes the same call at
                // [`windowed.rs:3700`](crate::windowed).
                tree.set_animations(&self.ctx.animations);

                // CRITICAL: tell the tree about the device pixel
                // ratio BEFORE computing layout. The renderer
                // multiplies layout coordinates by
                // `tree.scale_factor()` to convert logical→physical
                // pixels inside the GPU paint context. Without this
                // call, layout coords go straight to physical 1:1 —
                // on a Retina display the entire UI ends up rendered
                // into the top-left quadrant of a 2× canvas. Same
                // call as `windowed.rs:3706`.
                tree.set_scale_factor(self.ctx.scale_factor as f32);

                // Layout is computed in *logical* coordinates — that's
                // what the user's UI builder thinks in. The scale
                // factor above is what scales the result up to
                // physical pixels at render time.
                tree.compute_layout(self.ctx.width, self.ctx.height);

                self.current_tree = Some(tree);
            }

            self.needs_rebuild = false;
            self.ctx.rebuild_count = self.ctx.rebuild_count.saturating_add(1);
        }

        // 4. Render the tree to the next surface texture. If we don't
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

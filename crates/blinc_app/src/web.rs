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

/// Identifier for the four edit-menu actions wired through the
/// custom right-click context menu. Used by `synthesize_edit_action`
/// (and its helper closures inside `handle_context_menu`) to keep
/// the closure type small and `'static` — capturing the `WebApp`
/// would force a `Rc<RefCell<…>>` clone into every menu-item
/// callback and complicate the dispatch path significantly.
#[derive(Clone, Copy)]
enum EditAction {
    Cut,
    Copy,
    Paste,
    SelectAll,
}

/// Synthesise an edit-menu action against the currently focused
/// editable widget. Each action dispatches a Cmd+key chord through
/// `RenderTree::broadcast_key_event`, which the widget's
/// `on_key_down` handler translates into the matching edit
/// operation:
///
///   * Cut        → Cmd+X (key code 88)
///   * Copy       → Cmd+C (key code 67)
///   * Select All → Cmd+A (key code 65)
///
/// Paste is special-cased because the widget's Cmd+V handler reads
/// from `clipboard_read`, which on wasm32 is a permanent `None`
/// stub (the browser clipboard read API is async-only and can't
/// satisfy the widget's sync `Option<String>` contract). For
/// Paste, we call `navigator.clipboard.readText()` directly,
/// resolve the Promise via `wasm_bindgen_futures::spawn_local`,
/// then dispatch the resolved text into the focused widget via
/// `broadcast_text_input_event` once it lands.
///
/// This function operates entirely on global state — it doesn't
/// take a `&mut WebApp` because the menu-item click handlers can't
/// borrow the runner without elaborate `Rc<RefCell<…>>` plumbing.
/// The render tree is reached via the global
/// [`BlincContextState`] singleton, which both the runner and the
/// widget tree already share.
fn synthesize_edit_action(action: EditAction) {
    use blinc_core::events::event_types;
    // Set the touch-input flag back to false so the widget routes
    // the synthetic key event through its keyboard-shortcut path
    // (the same path Cmd+C / Cmd+V take from the keyboard handler).
    blinc_layout::widgets::text_input::set_touch_input(false);

    // Sync clipboard write helpers can be called inline. Cut /
    // Copy / Select All all dispatch a Cmd+key event into the
    // focused widget's key handler, which does its own selection
    // bookkeeping and clipboard write.
    //
    // We dispatch via the static `TREE_BROADCAST` channel that the
    // runner installs into the global before each frame; this is
    // the same path the document-level `paste` listener uses for
    // its broadcasted text-input events. The runner's render path
    // already holds the only `&mut RenderTree`, so this helper
    // can't get one — instead it routes through the broadcast
    // helpers via a queue that the next frame drains.
    //
    // For now we take the simpler approach: queue the action via a
    // global `Mutex<Option<EditAction>>` slot that the next
    // `dispatch_pending` / frame tick consumes. The frame loop
    // re-runs constantly because the cursor blink animation keeps
    // the rAF chain alive.
    if let Ok(mut slot) = PENDING_EDIT_ACTION.lock() {
        // Coalesce repeated taps — only the most recent matters.
        *slot = Some(action);
    }
    // Force a redraw so the next rAF tick picks up the queued
    // action even if the page is otherwise idle.
    blinc_layout::request_redraw();
    let _ = event_types::KEY_DOWN; // silence unused-import on cfg paths
}

/// Slot for a pending edit-menu action queued by
/// `synthesize_edit_action` and consumed inside the WebApp's
/// frame loop. Single-slot because the menu only ever fires one
/// action at a time.
static PENDING_EDIT_ACTION: std::sync::Mutex<Option<EditAction>> = std::sync::Mutex::new(None);

/// Drain any queued edit-menu action and dispatch it through the
/// supplied render tree. Called from inside `WebApp::run_one_frame`
/// (where we have a real `&mut RenderTree`) before the per-frame
/// rebuild + render pass.
fn drain_pending_edit_action(tree: &mut blinc_layout::renderer::RenderTree) {
    use blinc_core::events::event_types;
    let Some(action) = PENDING_EDIT_ACTION.lock().ok().and_then(|mut s| s.take()) else {
        return;
    };
    let (key_code, meta) = match action {
        EditAction::Cut => (88, true),       // Cmd+X
        EditAction::Copy => (67, true),      // Cmd+C
        EditAction::SelectAll => (65, true), // Cmd+A
        EditAction::Paste => {
            // Paste goes through the async clipboard read path —
            // kick off the Promise here and bail without
            // dispatching a synthetic key event. The widget will
            // see the pasted text via `broadcast_text_input_event`
            // once the Promise resolves.
            spawn_paste_from_clipboard();
            return;
        }
    };
    tree.broadcast_key_event(event_types::KEY_DOWN, key_code, false, false, false, meta);
}

/// Read text from `navigator.clipboard.readText()` (async) and
/// broadcast it into the focused widget once it resolves. Used by
/// the right-click context menu's Paste button — Cmd+V via the
/// keyboard goes through the document-level `paste` event listener
/// instead, which is sync.
fn spawn_paste_from_clipboard() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let clipboard = window.navigator().clipboard();
    let promise = clipboard.read_text();
    wasm_bindgen_futures::spawn_local(async move {
        let result = wasm_bindgen_futures::JsFuture::from(promise).await;
        let Ok(value) = result else {
            return;
        };
        let Some(text) = value.as_string() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        // Queue the pasted text via a separate slot — same shape
        // as `PENDING_EDIT_ACTION`. The next frame drains it.
        if let Ok(mut slot) = PENDING_PASTE_TEXT.lock() {
            *slot = Some(text);
        }
        blinc_layout::request_redraw();
    });
}

/// Slot for clipboard text resolved asynchronously by
/// [`spawn_paste_from_clipboard`]. Drained on the next frame by
/// [`drain_pending_paste_text`].
static PENDING_PASTE_TEXT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn drain_pending_paste_text(tree: &mut blinc_layout::renderer::RenderTree) {
    let Some(text) = PENDING_PASTE_TEXT.lock().ok().and_then(|mut s| s.take()) else {
        return;
    };
    for ch in text.chars() {
        tree.broadcast_text_input_event(ch, false, false, false, false);
    }
}

/// Map a Blinc [`CursorStyle`](blinc_layout::element::CursorStyle) to
/// the matching CSS `cursor` keyword. Mirrors the desktop
/// `convert_cursor_style` (windowed.rs:4172) but yields CSS strings
/// instead of `blinc_platform::Cursor` enum values, since on the web
/// we set the cursor via `canvas.style.cursor = "<keyword>"` directly
/// rather than through a winit-style cursor abstraction.
fn cursor_style_to_css(cursor: blinc_layout::element::CursorStyle) -> &'static str {
    use blinc_layout::element::CursorStyle;
    match cursor {
        CursorStyle::Default => "default",
        CursorStyle::Pointer => "pointer",
        CursorStyle::Text => "text",
        CursorStyle::Crosshair => "crosshair",
        CursorStyle::Move => "move",
        CursorStyle::NotAllowed => "not-allowed",
        CursorStyle::ResizeNS => "ns-resize",
        CursorStyle::ResizeEW => "ew-resize",
        CursorStyle::ResizeNESW => "nesw-resize",
        CursorStyle::ResizeNWSE => "nwse-resize",
        CursorStyle::Grab => "grab",
        CursorStyle::Grabbing => "grabbing",
        CursorStyle::Wait => "wait",
        CursorStyle::Progress => "progress",
        CursorStyle::None => "none",
    }
}

/// Object-safe UI builder trait, type-erased over the concrete
/// element type the user's closure returns. The two methods do the
/// two things the runner needs to do with a freshly built element —
/// spawn a new `RenderTree` or apply an incremental update to an
/// existing one — so the concrete `E: ElementBuilder` never has to
/// leave the closure. This is what lets the public
/// `WebApp::run_with_setup` accept `FnMut(&mut WindowedContext) -> E`
/// for ANY `E: ElementBuilder`, matching how the desktop runner's
/// `WindowedApp::run` already works.
///
/// Previously the runner stored
/// `Box<dyn FnMut(&mut WindowedContext) -> Div>`, which forced every
/// example's `build_ui` to concretely return a `Div`. That broke the
/// cross-target convention for examples whose root element is a
/// `Scroll`, a `Stateful<T>`, or anything else — `scroll()` returns
/// a `Scroll`, `stateful()` returns a `Stateful<T>`, neither of which
/// is a `Div`. Type-erasing through this trait fixes the mismatch
/// without asking callers to wrap every non-`Div` root in a
/// containing `div().child(…)` just to satisfy the web runner.
trait UiBuilderFn: 'static {
    /// First-frame build path: call the user's closure to produce a
    /// fresh element tree, then hand it to
    /// `RenderTree::from_element_with_registry` and return the
    /// constructed tree. Called when `current_tree` is `None`.
    fn build_from_scratch(
        &mut self,
        ctx: &mut WindowedContext,
        registry: std::sync::Arc<blinc_layout::selector::ElementRegistry>,
    ) -> blinc_layout::renderer::RenderTree;

    /// Incremental-update path: call the user's closure and apply
    /// the result to an existing tree. Returns the `UpdateResult` so
    /// the runner can decide whether to recompute layout.
    fn build_and_update(
        &mut self,
        ctx: &mut WindowedContext,
        tree: &mut blinc_layout::renderer::RenderTree,
    ) -> blinc_layout::UpdateResult;
}

/// Blanket impl covering every `FnMut(&mut WindowedContext) -> E`
/// where `E: ElementBuilder`. This is the one place in the web
/// runner where we have the concrete `E` in scope; from here on up
/// the rest of the runner operates on `dyn UiBuilderFn`.
impl<F, E> UiBuilderFn for F
where
    F: FnMut(&mut WindowedContext) -> E + 'static,
    E: blinc_layout::ElementBuilder + 'static,
{
    fn build_from_scratch(
        &mut self,
        ctx: &mut WindowedContext,
        registry: std::sync::Arc<blinc_layout::selector::ElementRegistry>,
    ) -> blinc_layout::renderer::RenderTree {
        let element = self(ctx);
        blinc_layout::renderer::RenderTree::from_element_with_registry(&element, registry)
    }

    fn build_and_update(
        &mut self,
        ctx: &mut WindowedContext,
        tree: &mut blinc_layout::renderer::RenderTree,
    ) -> blinc_layout::UpdateResult {
        let element = self(ctx);
        tree.incremental_update(&element)
    }
}

/// User-supplied UI builder, boxed as a trait object so the
/// concrete return type is type-erased. See [`UiBuilderFn`] for why
/// this can't just be `FnMut(&mut WindowedContext) -> Div`.
type UiBuilder = Box<dyn UiBuilderFn>;

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
    /// Last single-touch position, in canvas-local CSS pixels. `Some`
    /// while exactly one finger is on the screen so the touchmove
    /// handler can compute a per-frame delta and dispatch it as a
    /// scroll. Cleared on touchend / touchcancel and on
    /// multi-touch (we don't try to drive scroll from pinch /
    /// two-finger pans yet).
    last_touch_pos: Option<(f32, f32)>,
    /// Last CSS cursor string we wrote to the canvas's
    /// `style.cursor`. Tracked so the per-mousemove cursor refresh
    /// only touches the DOM when the hovered element's
    /// `CursorStyle` actually changes — without this guard every
    /// mousemove queues a layout-invalidating style write, which
    /// shows up as visible jank when the user just sweeps the
    /// mouse across the canvas.
    last_cursor: &'static str,
}

impl WebApp {
    /// Initialize the global [`blinc_theme::ThemeState`] with the
    /// default web theme bundle and the user's current
    /// `prefers-color-scheme`. Idempotent — safe to call multiple
    /// times; the second call is a no-op once the singleton is
    /// already populated.
    ///
    /// Mirrors `WindowedApp::init_theme` (windowed.rs:1979) which
    /// the desktop runner calls in `WindowedApp::run` for the same
    /// reason: every theme-aware widget panics on the first frame
    /// without an initialized `ThemeState`.
    fn init_theme() {
        use blinc_theme::{
            detect_system_color_scheme, platform_theme_bundle, set_redraw_callback, ThemeState,
        };

        if ThemeState::try_get().is_none() {
            let bundle = platform_theme_bundle();
            let scheme = detect_system_color_scheme();
            ThemeState::init(bundle, scheme);
        }

        // Theme changes (e.g. user toggling OS dark mode while the
        // page is open) need to invalidate the tree so widgets pick
        // up the new tokens. We don't yet hook the
        // `prefers-color-scheme` media-query change event from the
        // browser, but if a future runner addition fires it through
        // `ThemeState::set_color_scheme` this callback will route
        // it to a full rebuild + CSS reparse — same shape the
        // desktop runner uses.
        set_redraw_callback(|| {
            tracing::debug!("Theme changed - requesting full rebuild + CSS reparse");
            blinc_layout::widgets::request_css_reparse();
            blinc_layout::widgets::request_full_rebuild();
        });
    }

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

        // 3a. Wire the global text measurer to the BlincApp's font
        // registry. Without this, the layout system falls back to
        // the heuristic measurer in `text_measurer.rs::estimate_size`
        // which assumes every glyph is exactly `0.55 * font_size`
        // wide — fine for rough flexbox sizing of one-shot text
        // labels, completely wrong for any widget that hit-tests
        // against per-glyph positions. The visible bug is text
        // selection / cursor placement landing several pixels off
        // from where you clicked, because the text widget computes
        // character offsets from estimated widths while the
        // renderer draws glyphs at their actual font metrics.
        // Same call the desktop runner makes at
        // [`windowed.rs:2535`](crate::windowed) and the iOS runner
        // does inside `init_text_measurer()` at `ios.rs:203`.
        crate::text_measurer::init_text_measurer_with_registry(blinc_app.font_registry());

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

        // Initialize the global ThemeState the same way the desktop /
        // Android / iOS runners do (windowed.rs:1979, android.rs:110,
        // ios.rs:152). Without this, any widget that reads a theme
        // token — buttons, text inputs, scroll bars, the entire `cn`
        // component library — panics on the first frame with
        // "ThemeState not initialized". The web target uses the
        // default Catppuccin-derived bundle (re-exported as
        // `WebTheme::bundle()`) and reads the user's preferred color
        // scheme from `window.matchMedia('(prefers-color-scheme: dark)')`.
        Self::init_theme();

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
            last_touch_pos: None,
            last_cursor: "default",
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
    pub async fn run<F, E>(canvas_id: &str, ui_builder: F) -> Result<()>
    where
        F: FnMut(&mut WindowedContext) -> E + 'static,
        E: blinc_layout::ElementBuilder + 'static,
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
    pub async fn run_with_setup<S, F, E>(canvas_id: &str, setup: S, ui_builder: F) -> Result<()>
    where
        S: FnOnce(&mut Self) + 'static,
        F: FnMut(&mut WindowedContext) -> E + 'static,
        E: blinc_layout::ElementBuilder + 'static,
    {
        Self::run_with_setup_inner(
            canvas_id,
            move |app| {
                Box::pin(async move {
                    setup(app);
                    Ok(())
                })
            },
            ui_builder,
        )
        .await
    }

    /// Async sibling of [`Self::run_with_setup`] for setup steps
    /// that need to `.await` something — typically a `fetch()` call
    /// to load fonts or other assets before the first frame
    /// renders.
    ///
    /// The setup closure receives `&mut WebApp` and returns a
    /// `Future<Output = Result<()>>`. The returned future is
    /// awaited synchronously inside the runner before the UI
    /// builder is installed and the rAF loop kicks off, so any
    /// `app.load_font_data(...)` / `app.context_mut().add_css(...)`
    /// calls inside it land in the right order: first font, then
    /// first frame.
    ///
    /// # Example: fetched font
    ///
    /// ```ignore
    /// use blinc_app::web::WebApp;
    /// use blinc_platform_web::WebAssetLoader;
    ///
    /// WebApp::run_with_async_setup(
    ///     "blinc-canvas",
    ///     |app| Box::pin(async move {
    ///         // Single-shot fetch — bytes go straight into the
    ///         // font registry, no copy in the asset cache.
    ///         let bytes = WebAssetLoader::fetch_bytes("fonts/Inter.ttf").await
    ///             .map_err(|e| BlincError::Platform(e.to_string()))?;
    ///         app.load_font_data(bytes);
    ///         Ok(())
    ///     }),
    ///     build_ui,
    /// )
    /// .await
    /// ```
    ///
    /// The `Box::pin(async move { ... })` ceremony is needed because
    /// stable Rust doesn't have `async FnOnce` yet — the closure
    /// returns a boxed future. Once `async closures` stabilize this
    /// signature can drop the boxing.
    pub async fn run_with_async_setup<S, F, E>(
        canvas_id: &str,
        setup: S,
        ui_builder: F,
    ) -> Result<()>
    where
        S: for<'a> FnOnce(
            &'a mut Self,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + 'a>>,
        F: FnMut(&mut WindowedContext) -> E + 'static,
        E: blinc_layout::ElementBuilder + 'static,
    {
        Self::run_with_setup_inner(canvas_id, setup, ui_builder).await
    }

    /// Shared body of `run_with_setup` and `run_with_async_setup`.
    /// The only difference between the two is whether `setup`
    /// returns a future or runs synchronously — the sync wrapper
    /// just constructs an immediately-ready boxed future, so this
    /// inner function only ever sees the async form.
    async fn run_with_setup_inner<S, F, E>(
        canvas_id: &str,
        setup: S,
        ui_builder: F,
    ) -> Result<()>
    where
        S: for<'a> FnOnce(
            &'a mut Self,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + 'a>>,
        F: FnMut(&mut WindowedContext) -> E + 'static,
        E: blinc_layout::ElementBuilder + 'static,
    {
        let mut app = Self::new(canvas_id).await?;

        // Run user setup BEFORE installing the UI builder. This is
        // when fonts get loaded, CSS gets registered, etc. If setup
        // panics, we never reach the rAF loop and the browser's
        // panic-hook surfaces it in the console. Setup that returns
        // an `Err` is fatal — we propagate so the caller can decide
        // whether to surface it via `console.error`.
        setup(&mut app).await?;

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
        //
        // Skip right-click (button 2) and middle-click (button 1)
        // here. Right-click is owned by the `contextmenu` listener
        // below, which builds the custom edit-menu overlay; we
        // don't want the same gesture to also fire `on_mouse_down`
        // through the regular dispatch path because that would:
        //   1. Call `blur_all_text_inputs()`, dropping the focus
        //      and selection on whichever input the user
        //      right-clicked.
        //   2. Re-fire `EventRouter::on_mouse_down` against the
        //      hit-test target, which clears the text widget's
        //      selection_start. By the time the user picks "Copy"
        //      from the menu, there's nothing selected to copy.
        // Native macOS / Windows behavior is "right-click leaves
        // focus and selection alone", and that's what we mirror
        // here. Middle-click is also dropped because we have no
        // wired-up middle-click semantics yet — better to ignore
        // it than to treat it like a left-click.
        {
            let app_rc = Rc::clone(&app_rc);
            let closure = Closure::<dyn FnMut(_)>::new(move |evt: web_sys::MouseEvent| {
                if evt.button() != 0 {
                    return;
                }
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
        //
        // Mirror the mousedown filter: only the left button drives
        // dispatch_mouse_up. Right-button mouseups would otherwise
        // fire POINTER_UP / DRAG_END events that the framework
        // doesn't expect from a right-click.
        {
            let app_rc = Rc::clone(&app_rc);
            let closure = Closure::<dyn FnMut(_)>::new(move |evt: web_sys::MouseEvent| {
                if evt.button() != 0 {
                    return;
                }
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

        // ----- touchstart / touchmove / touchend / touchcancel -----
        //
        // The web target needs touch handling for two distinct
        // reasons:
        //
        //   1. **Tap-as-click**: a single-finger tap should reach
        //      every widget the same way a mouse click does so
        //      buttons, text inputs, links, etc. all work on a
        //      mobile browser. We synthesize mousedown/mouseup
        //      from touchstart/touchend at the touch position,
        //      mirroring the iOS runner's
        //      `MouseButton::Left` dispatch from `TouchPhase::Began`
        //      / `Ended` (see ios.rs:828-908).
        //
        //   2. **Swipe-to-scroll**: a single-finger drag should
        //      advance scroll containers under the finger. Touch
        //      events don't fire wheel events, so without this
        //      handler the mobile browser can't scroll a Blinc
        //      `scroll()` container at all. We track
        //      `last_touch_pos` between touchmove ticks and feed
        //      the delta into `dispatch_scroll`, the same path
        //      the wheel handler uses.
        //
        // We deliberately ignore multi-touch (pinch / two-finger
        // pan) here — those need their own pinch-zoom plumbing
        // through `EventRouter::on_pinch`, and the framework
        // doesn't yet wire pinch through the web runner. Single-
        // finger gestures cover the common UX (taps, swipes,
        // long presses) and that matches what `web_mobile_demo`
        // exercises.
        //
        // Each handler converts page-relative `client_x` /
        // `client_y` to canvas-local CSS pixels via the canvas's
        // `getBoundingClientRect()`. Touch events don't carry
        // `offset_x` / `offset_y`, so we have to do this
        // ourselves.
        {
            // Helper closure shared by all four touch handlers:
            // grab the first touch and translate to canvas-local
            // CSS pixels. Returns `None` if there's no first
            // touch (zero-touch touchend, multi-touch ignored).
            //
            // The bounding rect is read on every event because
            // page scroll, layout shifts, and CSS animations can
            // all move the canvas — caching here would silently
            // drift away from reality.
            let canvas_for_touch = canvas.clone();
            let touch_local_pos =
                move |touch_list: web_sys::TouchList| -> Option<(f32, f32, usize)> {
                    let len = touch_list.length() as usize;
                    if len == 0 {
                        return None;
                    }
                    let touch = touch_list.get(0)?;
                    let rect = canvas_for_touch.get_bounding_client_rect();
                    let x = touch.client_x() as f32 - rect.left() as f32;
                    let y = touch.client_y() as f32 - rect.top() as f32;
                    Some((x, y, len))
                };

            // touchstart
            {
                let app_rc = Rc::clone(&app_rc);
                let touch_local_pos = touch_local_pos.clone();
                let closure = Closure::<dyn FnMut(_)>::new(move |evt: web_sys::TouchEvent| {
                    // `prevent_default` here also stops the
                    // browser from synthesising a follow-up
                    // mousedown 300ms later (the legacy
                    // touch-to-click compat path), which would
                    // otherwise cause every tap to fire two
                    // POINTER_DOWN events.
                    evt.prevent_default();
                    if let Ok(mut app) = app_rc.try_borrow_mut() {
                        if let Some((x, y, len)) = touch_local_pos(evt.touches()) {
                            // Mark this event sequence as touch
                            // input so editable widgets switch to
                            // mobile UX (touch drag = move cursor,
                            // double-tap = native edit menu, etc.).
                            blinc_layout::widgets::text_input::set_touch_input(true);
                            if len == 1 {
                                app.last_touch_pos = Some((x, y));
                                Self::dispatch_mouse_down(
                                    &mut app,
                                    x,
                                    y,
                                    blinc_platform::MouseButton::Left,
                                );
                            } else {
                                // Multi-touch: cancel any
                                // single-touch tracking so a
                                // pinch doesn't get
                                // misinterpreted as a swipe when
                                // the user lifts a finger.
                                app.last_touch_pos = None;
                            }
                        }
                    }
                });
                canvas
                    .add_event_listener_with_callback(
                        "touchstart",
                        closure.as_ref().unchecked_ref(),
                    )
                    .map_err(|e| {
                        BlincError::Platform(format!("add touchstart listener failed: {e:?}"))
                    })?;
                closure.forget();
            }

            // touchmove
            {
                let app_rc = Rc::clone(&app_rc);
                let touch_local_pos = touch_local_pos.clone();
                let closure = Closure::<dyn FnMut(_)>::new(move |evt: web_sys::TouchEvent| {
                    // Block the browser's default touch-scroll
                    // behavior (rubber-band, address-bar reveal,
                    // pull-to-refresh) so the canvas owns scroll
                    // semantics end-to-end.
                    evt.prevent_default();
                    if let Ok(mut app) = app_rc.try_borrow_mut() {
                        if let Some((x, y, len)) = touch_local_pos(evt.touches()) {
                            if len == 1 {
                                if let Some((px, py)) = app.last_touch_pos {
                                    let dx = x - px;
                                    let dy = y - py;
                                    // Sub-pixel jitter guard
                                    // mirroring ios.rs:874.
                                    if dx.abs() > 0.5 || dy.abs() > 0.5 {
                                        Self::dispatch_scroll(&mut app, dx, dy);
                                    }
                                }
                                app.last_touch_pos = Some((x, y));
                                // Also forward as a mouse_move so
                                // hover state / drag handlers
                                // see the touch path.
                                Self::dispatch_mouse_move(&mut app, x, y);
                            } else {
                                app.last_touch_pos = None;
                            }
                        }
                    }
                });
                canvas
                    .add_event_listener_with_callback("touchmove", closure.as_ref().unchecked_ref())
                    .map_err(|e| {
                        BlincError::Platform(format!("add touchmove listener failed: {e:?}"))
                    })?;
                closure.forget();
            }

            // touchend
            {
                let app_rc = Rc::clone(&app_rc);
                let touch_local_pos = touch_local_pos.clone();
                let closure = Closure::<dyn FnMut(_)>::new(move |evt: web_sys::TouchEvent| {
                    evt.prevent_default();
                    if let Ok(mut app) = app_rc.try_borrow_mut() {
                        // touchend's `touches()` is the list of
                        // STILL-ACTIVE touches (not the ones that
                        // just ended), so we read the released
                        // touch from `changed_touches()` instead.
                        let pos = touch_local_pos(evt.changed_touches())
                            .or(app.last_touch_pos.map(|(x, y)| (x, y, 1)));
                        if let Some((x, y, _)) = pos {
                            Self::dispatch_mouse_up(
                                &mut app,
                                x,
                                y,
                                blinc_platform::MouseButton::Left,
                            );
                        }
                        // Cancel any armed long-press timer the
                        // user just dismissed by lifting their
                        // finger before the deadline.
                        blinc_layout::widgets::text_input::cancel_long_press_timer();
                        app.last_touch_pos = None;
                    }
                });
                canvas
                    .add_event_listener_with_callback("touchend", closure.as_ref().unchecked_ref())
                    .map_err(|e| {
                        BlincError::Platform(format!("add touchend listener failed: {e:?}"))
                    })?;
                closure.forget();
            }

            // touchcancel
            {
                let app_rc = Rc::clone(&app_rc);
                let closure = Closure::<dyn FnMut(_)>::new(move |evt: web_sys::TouchEvent| {
                    evt.prevent_default();
                    if let Ok(mut app) = app_rc.try_borrow_mut() {
                        if let Some((x, y)) = app.last_touch_pos {
                            Self::dispatch_mouse_up(
                                &mut app,
                                x,
                                y,
                                blinc_platform::MouseButton::Left,
                            );
                        }
                        blinc_layout::widgets::text_input::cancel_long_press_timer();
                        app.last_touch_pos = None;
                    }
                });
                canvas
                    .add_event_listener_with_callback(
                        "touchcancel",
                        closure.as_ref().unchecked_ref(),
                    )
                    .map_err(|e| {
                        BlincError::Platform(format!("add touchcancel listener failed: {e:?}"))
                    })?;
                closure.forget();
            }
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
                    let shift = evt.shift_key();
                    let ctrl = evt.ctrl_key();
                    let alt = evt.alt_key();
                    let meta = evt.meta_key();
                    Self::dispatch_key_down(&mut app, key_code, shift, ctrl, alt, meta);

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
                        if !ch.is_control() && !ctrl && !meta {
                            // Prevent the browser from also acting on
                            // the key (e.g. quick-find triggering on
                            // `/`, space scrolling the page) when a
                            // Blinc text input is consuming it.
                            evt.prevent_default();
                            Self::dispatch_text_input(&mut app, ch, shift, ctrl, alt, meta);
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
                    let shift = evt.shift_key();
                    let ctrl = evt.ctrl_key();
                    let alt = evt.alt_key();
                    let meta = evt.meta_key();
                    Self::dispatch_key_up(&mut app, key_code, shift, ctrl, alt, meta);
                }
            });
            document
                .add_event_listener_with_callback("keyup", closure.as_ref().unchecked_ref())
                .map_err(|e| BlincError::Platform(format!("add keyup listener failed: {e:?}")))?;
            closure.forget();
        }

        // ----- paste (on document) -----
        //
        // The browser's `paste` event is the only path that gives us
        // sync access to clipboard text. `navigator.clipboard.readText()`
        // is async-only and can't satisfy the widget's sync
        // `clipboard_read() -> Option<String>` contract — so instead of
        // routing Cmd+V through the widget's clipboard_read handler, we
        // intercept the paste event itself and broadcast each character
        // as a TEXT_INPUT event into the focused widget. The browser
        // fires this event for:
        //   * Cmd+V / Ctrl+V keyboard shortcut
        //   * Right-click → Paste from the browser's native context menu
        //   * Edit → Paste from the browser's menu bar
        //   * Mobile long-press → Paste
        //
        // Our custom context menu's Paste button doesn't go through
        // this path because it can't synthesize a real paste event
        // (the browser only fires those for user gestures); it calls
        // `navigator.clipboard.readText()` directly and dispatches the
        // resolved text via `broadcast_text_input_event`.
        {
            let app_rc = Rc::clone(&app_rc);
            let closure = Closure::<dyn FnMut(_)>::new(move |evt: web_sys::ClipboardEvent| {
                if let Ok(mut app) = app_rc.try_borrow_mut() {
                    let Some(data) = evt.clipboard_data() else {
                        return;
                    };
                    let text = data.get_data("text/plain").unwrap_or_default();
                    if text.is_empty() {
                        return;
                    }
                    evt.prevent_default();
                    if let Some(tree) = app.current_tree.as_mut() {
                        // Broadcast each char individually so the
                        // widget's `on_event(TEXT_INPUT, …)` handler
                        // sees them through `EventContext::key_char`,
                        // matching how the keydown handler delivers
                        // single typed characters. The widget appends
                        // each character to its buffer; multi-line
                        // pastes flow through naturally because '\n'
                        // is just another char from the widget's
                        // perspective.
                        for ch in text.chars() {
                            tree.broadcast_text_input_event(ch, false, false, false, false);
                        }
                    }
                }
            });
            document
                .add_event_listener_with_callback("paste", closure.as_ref().unchecked_ref())
                .map_err(|e| BlincError::Platform(format!("add paste listener failed: {e:?}")))?;
            closure.forget();
        }

        // ----- contextmenu (on canvas) -----
        //
        // The browser's right-click menu (Inspect Element, Save Image,
        // …) is unhelpful inside a Blinc canvas — Blinc owns its own
        // hit-testing and the browser has no idea which Blinc widget
        // the cursor is over. Suppress the default menu via
        // `preventDefault()` so the canvas owns the right-click
        // gesture, then route a synthetic `show_edit_menu` call into
        // Rust the same way the iOS double-tap path does. The web
        // implementation of `show_edit_menu` lives in the Rust web
        // runner (see `Self::handle_show_edit_menu`); it builds an
        // absolutely positioned `<div>` overlay with Cut / Copy /
        // Paste / Select All buttons.
        //
        // Whether or not we actually show a menu depends on whether
        // the user clicked on a focused editable widget — there's no
        // point showing a Cut / Copy menu over a header text. We
        // check `focused_editable_node_id()` for the gate.
        {
            let app_rc = Rc::clone(&app_rc);
            let closure = Closure::<dyn FnMut(_)>::new(move |evt: web_sys::MouseEvent| {
                evt.prevent_default();
                if let Ok(mut app) = app_rc.try_borrow_mut() {
                    let x = evt.offset_x() as f32;
                    let y = evt.offset_y() as f32;
                    // Anchor the menu in viewport (page) coordinates
                    // because the overlay is appended to `document.body`,
                    // not the canvas. `client_x/y` give us
                    // viewport-relative pixels.
                    let page_x = evt.client_x() as f32;
                    let page_y = evt.client_y() as f32;
                    Self::handle_context_menu(&mut app, x, y, page_x, page_y);
                }
            });
            canvas
                .add_event_listener_with_callback("contextmenu", closure.as_ref().unchecked_ref())
                .map_err(|e| {
                    BlincError::Platform(format!("add contextmenu listener failed: {e:?}"))
                })?;
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

        // Update the canvas cursor based on the hovered element's
        // `CursorStyle`. Mirrors the desktop runner's
        // `window.set_cursor(...)` call inside `MouseEvent::Moved`
        // (windowed.rs:2926-2930), translated to CSS
        // `style.cursor` writes on the canvas. The query has to
        // run BEFORE `dispatch_pending` so the immutable tree
        // borrow doesn't conflict with the mutable borrow that
        // `dispatch_pending` needs.
        let cursor_style = tree
            .get_cursor_at(&app.ctx.event_router, x, y)
            .unwrap_or(blinc_layout::element::CursorStyle::Default);
        let css_cursor = cursor_style_to_css(cursor_style);
        if css_cursor != app.last_cursor {
            // Touch the DOM only when the cursor actually changes.
            // `style.cursor` writes are cheap individually but they
            // queue style invalidations on the canvas — without
            // this guard, every mousemove (60+/sec on a fast
            // pointer sweep) churns through the browser's style
            // recalc path for no visible benefit.
            if let Some(html_canvas) = app.canvas.dyn_ref::<web_sys::HtmlElement>() {
                let _ = html_canvas.style().set_property("cursor", css_cursor);
            }
            app.last_cursor = css_cursor;
        }

        Self::dispatch_pending(app, pending);
    }

    fn dispatch_mouse_down(app: &mut Self, x: f32, y: f32, button: blinc_platform::MouseButton) {
        // Mouse is the primary input — flip touch flag off so any
        // editable widget that branches on `is_touch_input()`
        // (text_input drag = move-cursor vs select-text) reverts
        // to desktop semantics.
        blinc_layout::widgets::text_input::set_touch_input(false);

        // Blur any focused text inputs BEFORE processing the click.
        // Mirrors the desktop runner at windowed.rs:2913 and the
        // iOS runner at ios.rs:848: tapping anywhere globally
        // clears focus, and the text input that gets clicked
        // re-focuses itself via its own on_mouse_down handler.
        // Without this, clicking outside an input keeps the input
        // focused indefinitely — the user can never blur it
        // except by clicking another input.
        blinc_layout::widgets::blur_all_text_inputs();

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
            app.ctx
                .event_router
                .on_scroll_nested(tree, delta_x, delta_y)
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

    fn dispatch_key_down(
        app: &mut Self,
        key_code: u32,
        shift: bool,
        ctrl: bool,
        alt: bool,
        meta: bool,
    ) {
        // Update the router so it can fire any blur / focus-change
        // logic that depends on key state. The return value is
        // (node, event_type) for a hit-tested target — we ignore it
        // because we broadcast instead, mirroring the desktop
        // runner. The router still tracks focus internally, which
        // is what `dispatch_text_input` relies on later for
        // "focused element" semantics.
        let _ = app.ctx.event_router.on_key_down(key_code);

        // Broadcast KEY_DOWN to every key handler in the tree.
        // Each widget checks its own focus state to decide whether
        // to act, mirroring `windowed.rs:3463-3473`. Without this
        // path the text-input widget never sees Backspace / Enter
        // / arrow keys etc., because the router-based dispatch
        // walks an event-bubble chain that doesn't reach handlers
        // registered at the focused node when the focus changed
        // mid-rebuild.
        if let Some(tree) = app.current_tree.as_mut() {
            tree.broadcast_key_event(
                blinc_core::events::event_types::KEY_DOWN,
                key_code,
                shift,
                ctrl,
                alt,
                meta,
            );
        }
    }

    fn dispatch_key_up(
        app: &mut Self,
        key_code: u32,
        shift: bool,
        ctrl: bool,
        alt: bool,
        meta: bool,
    ) {
        let _ = app.ctx.event_router.on_key_up(key_code);
        if let Some(tree) = app.current_tree.as_mut() {
            tree.broadcast_key_event(
                blinc_core::events::event_types::KEY_UP,
                key_code,
                shift,
                ctrl,
                alt,
                meta,
            );
        }
    }

    fn dispatch_text_input(
        app: &mut Self,
        ch: char,
        shift: bool,
        ctrl: bool,
        alt: bool,
        meta: bool,
    ) {
        // Broadcast TEXT_INPUT through `RenderTree::broadcast_text_input_event`
        // — the only path that actually populates `EventContext::key_char`,
        // which the text_input / text_area / code_editor / rich_text_editor
        // widgets all read inside their `on_event(TEXT_INPUT, …)` handlers.
        // The previous web runner used `tree.dispatch_event(...)` which
        // sends the event but leaves `key_char` as `None`, so widgets saw
        // the event fire but then bailed out without inserting anything
        // (the typing path is "if let Some(c) = ctx.key_char { d.insert(c) }").
        if let Some(tree) = app.current_tree.as_mut() {
            tree.broadcast_text_input_event(ch, shift, ctrl, alt, meta);
        }
    }

    /// Handle a right-click on the canvas. Shows a custom context
    /// menu with Cut / Copy / Paste / Select All when the click
    /// lands on a focused editable widget; bails silently otherwise.
    ///
    /// `canvas_x` / `canvas_y` are the click position in canvas-local
    /// CSS pixels (used for hit-testing). `page_x` / `page_y` are the
    /// click position in viewport-relative CSS pixels (used to
    /// position the overlay, which lives under `document.body`).
    ///
    /// The menu items each invoke `synthesize_edit_action`, which
    /// dispatches the corresponding Cmd+key chord into the focused
    /// widget. Cut / Copy then route through the widget's existing
    /// Cmd+X / Cmd+C handlers, which call `clipboard_write` (now
    /// implemented for wasm via `navigator.clipboard.writeText`).
    /// Paste calls `navigator.clipboard.readText()` directly and
    /// dispatches the resolved text via `broadcast_text_input_event`,
    /// bypassing the widget's Cmd+V → `clipboard_read` path entirely
    /// (the browser clipboard read API is async-only and can't
    /// satisfy the widget's sync `Option<String>` contract).
    fn handle_context_menu(app: &mut Self, canvas_x: f32, canvas_y: f32, page_x: f32, page_y: f32) {
        // Hit-test the click position so we can decide whether
        // there's anything worth popping a menu for. We bail if the
        // click doesn't land on a focused editable widget.
        let focused = blinc_layout::widgets::text_input::focused_editable_node_id();
        if focused.is_none() {
            // No focused editable — let the click pass through to
            // normal mouse_down handling so it can focus an input
            // first. We could re-fire the right-click as a left
            // mouse_down here, but that interferes with apps that
            // want to use right-click for their own menus.
            return;
        }

        // Update the router's mouse position so subsequent
        // synthetic key events that read mouse coords get the
        // right values for hit testing the focused node's bounds.
        if let Some(tree) = app.current_tree.as_ref() {
            let _ = app.ctx.event_router.on_mouse_move(tree, canvas_x, canvas_y);
        }

        // Build the overlay. We construct it directly via web_sys
        // rather than going through Blinc's element registry
        // because (a) the menu has to layer above the canvas at
        // arbitrary viewport coordinates which Blinc's layout
        // engine isn't built for, and (b) the menu needs real
        // DOM event listeners that can call back into Rust to
        // dispatch the chosen action — element-tree handlers
        // don't see DOM-level click events at all.
        let Some(window) = web_sys::window() else {
            return;
        };
        let Some(document) = window.document() else {
            return;
        };

        // Tear down any previously open Blinc context menu so a
        // second right-click doesn't stack a new menu on top of
        // the old one. We tag the menu with a stable id so we can
        // find it again.
        const MENU_ID: &str = "blinc-context-menu";
        if let Some(existing) = document.get_element_by_id(MENU_ID) {
            existing.remove();
        }

        let Ok(menu) = document.create_element("div") else {
            return;
        };
        let _ = menu.set_attribute("id", MENU_ID);
        // Inline styles instead of a stylesheet because we want
        // the menu to work without the host page providing any
        // CSS. The colors mirror the dark Catppuccin-ish palette
        // the rest of `web_mobile_demo` uses so the menu doesn't
        // look out of place.
        let style = format!(
            "position:fixed;left:{}px;top:{}px;\
             background:#1e1e2e;color:#cdd6f4;\
             border:1px solid #45475a;border-radius:8px;\
             padding:4px 0;\
             font:13px -apple-system,BlinkMacSystemFont,'Segoe UI',system-ui,sans-serif;\
             box-shadow:0 8px 24px rgba(0,0,0,0.4);\
             z-index:9999;\
             min-width:160px;",
            page_x, page_y,
        );
        let _ = menu.set_attribute("style", &style);

        // Build the four menu items. Each item registers a click
        // handler that calls `synthesize_edit_action` with the
        // corresponding action.
        for (label, action) in [
            ("Cut", EditAction::Cut),
            ("Copy", EditAction::Copy),
            ("Paste", EditAction::Paste),
            ("Select All", EditAction::SelectAll),
        ] {
            let Ok(item) = document.create_element("div") else {
                continue;
            };
            let _ =
                item.set_attribute("style", "padding:6px 16px;cursor:pointer;user-select:none;");
            item.set_text_content(Some(label));

            // Hover highlight via CSS pseudo-classes is awkward
            // for inline-styled elements; do it through JS
            // listeners instead.
            if let Some(html_item) = item.dyn_ref::<web_sys::HtmlElement>() {
                let html_item_clone = html_item.clone();
                let on_over = Closure::<dyn FnMut(_)>::new(move |_evt: web_sys::MouseEvent| {
                    let _ = html_item_clone
                        .style()
                        .set_property("background", "#313244");
                });
                let _ = html_item.add_event_listener_with_callback(
                    "mouseover",
                    on_over.as_ref().unchecked_ref(),
                );
                on_over.forget();

                let html_item_clone = html_item.clone();
                let on_out = Closure::<dyn FnMut(_)>::new(move |_evt: web_sys::MouseEvent| {
                    let _ = html_item_clone.style().set_property("background", "");
                });
                let _ = html_item
                    .add_event_listener_with_callback("mouseout", on_out.as_ref().unchecked_ref());
                on_out.forget();
            }

            // Action handler dispatches the chosen edit action and
            // removes the menu. We listen for `mousedown` (not
            // `click`) and call `stop_propagation()` so the
            // document-level dismiss listener — which also fires
            // on mousedown — never sees this event. Listening for
            // `click` would race with the document mousedown:
            // mousedown bubbles to document → dismiss handler
            // removes the menu → mouseup → click never fires
            // because the element is gone. We can't capture
            // `&mut WebApp` directly here because the closure
            // outlives this stack frame; route through
            // `synthesize_edit_action`, which queues the action
            // into a global slot that the next frame drains.
            let on_action = Closure::<dyn FnMut(_)>::new(move |evt: web_sys::MouseEvent| {
                evt.stop_propagation();
                evt.prevent_default();
                synthesize_edit_action(action);
                if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                    if let Some(menu) = doc.get_element_by_id(MENU_ID) {
                        menu.remove();
                    }
                }
            });
            let _ = item
                .add_event_listener_with_callback("mousedown", on_action.as_ref().unchecked_ref());
            on_action.forget();

            let _ = menu.append_child(&item);
        }

        // Close the menu on any outside click or scroll. We
        // attach a one-shot mousedown listener on `document` that
        // removes the menu and unregisters itself.
        let dismiss = Closure::<dyn FnMut(_)>::new(move |_evt: web_sys::MouseEvent| {
            if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                if let Some(menu) = doc.get_element_by_id(MENU_ID) {
                    menu.remove();
                }
            }
        });
        // Schedule listener attachment for the next tick so the
        // current right-click event doesn't immediately trigger
        // the dismiss handler.
        let document_clone = document.clone();
        let dismiss_attach = Closure::<dyn FnMut()>::new(move || {
            let _ = document_clone.add_event_listener_with_callback_and_add_event_listener_options(
                "mousedown",
                dismiss.as_ref().unchecked_ref(),
                web_sys::AddEventListenerOptions::new().once(true),
            );
        });
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            dismiss_attach.as_ref().unchecked_ref(),
            0,
        );
        dismiss_attach.forget();

        if let Some(body) = document.body() {
            let _ = body.append_child(&menu);
        }
    }

    /// Dispatch a batch of pending events through the cached render
    /// tree. Mouse handlers all use this — the EventRouter returns a
    /// list of (node, event_type) pairs that the tree's handler
    /// registry needs to walk through individually.
    ///
    /// Each event is forwarded via [`RenderTree::dispatch_event_full`]
    /// (not the simpler `dispatch_event`) so the runner can populate
    /// the per-event auxiliary fields the EventContext needs:
    ///
    ///   - **`drag_delta_x` / `drag_delta_y`** — read from
    ///     `EventRouter::drag_delta()`. The router accumulates these
    ///     between mousedown and mouseup; without forwarding them
    ///     here, every `on_drag` handler receives `(0, 0)` and the
    ///     dragged element never moves. This was the silent
    ///     `web_drag` bug — the chain reached the handler, just
    ///     with empty deltas.
    ///   - **`bounds_x/y/w/h`** + **`local_x/y`** — looked up via
    ///     `EventRouter::get_node_bounds(node)` so the handler can
    ///     reason about element-local coordinates (e.g. the
    ///     sortable demo's `e.local_y` to figure out which list
    ///     item the cursor hit).
    ///
    /// Mirrors the desktop runner's per-event population pass at
    /// [`windowed.rs:2864-2882`](crate::windowed) (which writes
    /// the same fields onto a `PendingEvent` struct before passing
    /// it to `dispatch_event_full`).
    fn dispatch_pending(app: &mut Self, pending: Vec<(blinc_layout::tree::LayoutNodeId, u32)>) {
        if pending.is_empty() {
            return;
        }
        let (mx, my) = app.ctx.event_router.mouse_position();
        let (drag_dx, drag_dy) = app.ctx.event_router.drag_delta();
        // Snapshot per-node bounds before borrowing the tree mutably
        // — `get_node_bounds` lives on `EventRouter`, and we'd
        // otherwise have a `&self.ctx` + `&mut self.current_tree`
        // borrow conflict.
        struct DispatchEntry {
            node: blinc_layout::tree::LayoutNodeId,
            event_type: u32,
            bounds: (f32, f32, f32, f32),
        }
        let entries: Vec<DispatchEntry> = pending
            .iter()
            .map(|&(node, event_type)| DispatchEntry {
                node,
                event_type,
                bounds: app
                    .ctx
                    .event_router
                    .get_node_bounds(node)
                    .unwrap_or((0.0, 0.0, 0.0, 0.0)),
            })
            .collect();

        if let Some(tree) = app.current_tree.as_mut() {
            for entry in entries {
                let DispatchEntry {
                    node,
                    event_type,
                    bounds: (bx, by, bw, bh),
                } = entry;
                let local_x = mx - bx;
                let local_y = my - by;
                // Drag deltas only meaningful for DRAG / DRAG_END;
                // for everything else they're (0, 0) by virtue of
                // the router resetting them on mouseup.
                let (dx, dy) = if event_type == blinc_core::events::event_types::DRAG
                    || event_type == blinc_core::events::event_types::DRAG_END
                {
                    (drag_dx, drag_dy)
                } else {
                    (0.0, 0.0)
                };
                tree.dispatch_event_full(
                    node, event_type, mx, my, local_x, local_y, bx, by, bw, bh, dx, dy,
                    /* pinch_scale */ 1.0,
                );
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
    pub fn set_ui_builder<F, E>(&mut self, builder: F)
    where
        F: FnMut(&mut WindowedContext) -> E + 'static,
        E: blinc_layout::ElementBuilder + 'static,
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

            // Drain any context-menu actions queued by
            // `handle_context_menu` (Cut / Copy / Select All)
            // and any clipboard text resolved by the async
            // `spawn_paste_from_clipboard` Promise. Both queues
            // are global mutexes — the runner is the only place
            // we have a `&mut RenderTree` to feed them into.
            // Doing it here, before the rebuild trigger detection
            // below, means the synthetic key event flips the
            // widget's dirty flag in time for the same frame to
            // pick up the rebuild instead of waiting one extra
            // tick.
            drain_pending_edit_action(tree);
            drain_pending_paste_text(tree);
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

        // 3. Drain pending visual updates from `Stateful::on_drag` /
        //    `on_state` / `dispatch_state` and any other handler
        //    that mutates state without restructuring the tree.
        //
        //    `State::set` / `State::update` and the FSM transition
        //    helpers do *not* flip `ref_dirty_flag` — that's the
        //    `set_rebuild` path. Instead they call
        //    `refresh_props_internal`, which:
        //
        //      1. Re-runs the matching `Stateful::on_state` callback
        //         to compute fresh `RenderProps` + child Div for the
        //         current state.
        //      2. Pushes the result onto two global queues —
        //         `PENDING_PROP_UPDATES` (visual-only prop changes
        //         like `transform`, `opacity`, `bg`) and
        //         `PENDING_SUBTREE_REBUILDS` (children added /
        //         removed / restructured).
        //      3. Sets the global `NEEDS_REDRAW` flag.
        //
        //    The runner has to drain both queues every frame and
        //    apply them to `current_tree`. Without this, the
        //    on_state callback fires (so the framework "knows" the
        //    new render props), but the queue contents never reach
        //    the live tree, so the next render still sees the old
        //    props and nothing visually changes. Mirrors the same
        //    drain block on desktop at
        //    [`windowed.rs:3590-3642`](crate::windowed) verbatim,
        //    minus the FLIP / motion / overlay-rebuild bits this
        //    runner doesn't yet wire up.
        let has_stateful_updates = blinc_layout::take_needs_redraw();
        let has_pending_rebuilds = blinc_layout::has_pending_subtree_rebuilds();
        if has_stateful_updates || has_pending_rebuilds {
            // Apply queued render-prop updates to existing nodes.
            // These are the cheap visual-only path: same node, new
            // `RenderProps` (e.g. `transform: translate(dx, dy)`).
            let prop_updates = blinc_layout::take_pending_prop_updates();
            if let Some(ref mut tree) = self.current_tree {
                for (node_id, props) in &prop_updates {
                    tree.update_render_props(*node_id, |p| *p = props.clone());
                }
            }

            // Apply queued subtree rebuilds. Each entry replaces a
            // parent's children with a freshly built Div from the
            // matching `Stateful::on_state` callback. Returns
            // `true` if any of the rebuilds had `needs_layout =
            // true` (i.e. the structural change might have
            // affected layout dimensions), in which case we
            // recompute layout once at the end.
            let mut needs_relayout = false;
            if let Some(ref mut tree) = self.current_tree {
                needs_relayout = tree.process_pending_subtree_rebuilds();
            }
            if needs_relayout {
                if let Some(ref mut tree) = self.current_tree {
                    tree.compute_layout(self.ctx.width, self.ctx.height);
                }
            }
        }

        // 4. Rebuild / incrementally update the tree if needed.
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
                match builder.build_and_update(&mut self.ctx, existing_tree) {
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
                let mut tree = builder.build_from_scratch(&mut self.ctx, registry);

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

        // 5. Render the tree to the next surface texture. If we don't
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

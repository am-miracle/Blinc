//! "Setup + draw" sketch runner for immediate-mode creative coding.
//!
//! `sketch(key, s)` wraps a `Canvas` element with an animation loop that
//! calls `s.draw(ctx, t, dt)` every frame. Sketch state — including whatever
//! fields the user puts on their `Sketch` impl — persists across UI rebuilds
//! via `BlincContextState::use_state_keyed`, so hot-reload or layout changes
//! that rebuild the tree don't reset counters, particle systems, etc.
//!
//! ```ignore
//! use blinc_canvas_kit::prelude::*;
//! use blinc_core::layer::Color;
//!
//! struct Bouncer { x: f32, vx: f32 }
//!
//! impl Sketch for Bouncer {
//!     fn draw(&mut self, ctx: &mut SketchContext, _t: f32, dt: f32) {
//!         self.x += self.vx * dt;
//!         if self.x < 0.0 || self.x + 40.0 > ctx.width { self.vx = -self.vx; }
//!
//!         let mut p = ctx.painter();
//!         p.fill(Color::WHITE).no_stroke();
//!         p.rect(self.x, 100.0, 40.0, 40.0);
//!     }
//! }
//!
//! // Inside a Div tree:
//! sketch("bouncer", Bouncer { x: 0.0, vx: 200.0 })
//! ```
//!
//! # Animation
//!
//! Each `draw()` call ends by requesting another frame, so sketches run at
//! the host's redraw cadence (typically vsync on windowed platforms). There
//! is no opt-out today — if you want a static render, use a plain
//! `canvas(...)` element directly.

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use blinc_core::events::event_types;
use blinc_core::layer::Rect;
use blinc_core::use_state_keyed;
use blinc_core::DrawContext;
use blinc_layout::canvas::{canvas, CanvasBounds};
use blinc_layout::div::{div, Div};
use blinc_layout::event_handler::EventContext;
use blinc_layout::stateful::request_redraw;
use web_time::Instant;

use crate::painter::Painter2D;

/// A per-frame immediate-mode sketch. Implementors own their own state
/// and mutate it each frame inside `draw()`.
///
/// `Send + 'static` is required because the sketch lives behind an
/// `Arc<Mutex<...>>` stored in `BlincContextState` — Blinc's persistent
/// state bag is `Send` for platform-portable threading.
pub trait Sketch: Send + 'static {
    /// Called once before the first `draw()` on a fresh sketch. Use for
    /// GPU uploads, asset preloads, or one-shot layout. Default: no-op.
    fn setup(&mut self, _ctx: &mut SketchContext<'_>) {}

    /// Called every frame. `t` is seconds since the sketch started;
    /// `dt` is seconds since the previous frame.
    fn draw(&mut self, ctx: &mut SketchContext<'_>, t: f32, dt: f32);
}

/// Per-frame context passed into `Sketch::setup` / `Sketch::draw`.
///
/// Borrows the underlying `DrawContext` mutably, plus exposes the current
/// canvas size and frame counter. Build a [`Painter2D`] for stateful
/// Processing-style drawing, or drop to [`SketchContext::draw_context`] for
/// full `DrawContext` access (gradients, glass, clip stacks, 3D).
pub struct SketchContext<'a> {
    ctx: &'a mut dyn DrawContext,
    /// Width of the canvas in layout units.
    pub width: f32,
    /// Height of the canvas in layout units.
    pub height: f32,
    /// Frames drawn since `setup()` — `0` inside `setup`, `0` on the
    /// first `draw()`, incrementing thereafter.
    pub frame_count: u64,
}

impl<'a> SketchContext<'a> {
    /// Build a stateful immediate-mode drawing wrapper.
    ///
    /// The returned [`Painter2D`] holds a mutable borrow of the underlying
    /// `DrawContext` for its lifetime. Drop it before calling
    /// [`SketchContext::draw_context`] again.
    pub fn painter(&mut self) -> Painter2D<'_> {
        Painter2D::new(self.ctx)
    }

    /// Access the raw `DrawContext` for features not covered by
    /// `Painter2D` — gradients, glass, clip stacks, text, images, 3D.
    pub fn draw_context(&mut self) -> &mut dyn DrawContext {
        self.ctx
    }

    /// Render a [`Player`] into `rect` at time `t`. Thin forwarder over
    /// [`Player::draw_at`] — provided so sketches reading a player out of
    /// `self` don't need to juggle a direct `self.logo.draw_at(ctx, ...)`
    /// borrow when the sketch also touches `ctx` elsewhere in the scope.
    pub fn play<P: Player + ?Sized>(&mut self, player: &mut P, rect: Rect, t: f32) {
        player.draw_at(self, rect, t);
    }
}

/// Build a `Div` running the given sketch at full width and height.
///
/// `key` scopes the sketch's persistent state. Pick a unique string per
/// sketch instance — state (including your `Sketch` impl's fields, the
/// frame counter, and the wall-clock start time) survives UI rebuilds
/// keyed on this string.
///
/// Wrap the returned `Div` in a sized container (`.w(...)`, `.h(...)`,
/// `.aspect_ratio(...)`) or a flex parent to control bounds.
pub fn sketch<S: Sketch>(key: &str, s: S) -> Div {
    let handle = use_state_keyed(&format!("{key}_sketch"), move || SketchHandle::new(s));

    div().w_full().h_full().child(
        canvas(move |ctx: &mut dyn DrawContext, bounds: CanvasBounds| {
            // `try_get` avoids the `T: Default` bound on `State::get`; the
            // handle is guaranteed to exist after `use_state_keyed`.
            if let Some(h) = handle.try_get() {
                h.tick(ctx, bounds);
            }
            // Keep the redraw chain alive so `t` / `dt` keep advancing.
            // Sketches are expected to animate; static sketches should
            // use `canvas(...)` directly instead.
            request_redraw();
        })
        .w_full()
        .h_full(),
    )
}

/// Persistent, clonable handle to a sketch's runtime state. Stored inside
/// `BlincContextState` via `use_state_keyed`; cheap to clone (one `Arc`).
#[derive(Clone)]
struct SketchHandle {
    inner: Arc<Mutex<SketchInner>>,
}

impl SketchHandle {
    fn new<S: Sketch>(s: S) -> Self {
        let now = Instant::now();
        Self {
            inner: Arc::new(Mutex::new(SketchInner {
                sketch: Box::new(s),
                start: now,
                last: now,
                frame_count: 0,
                did_setup: false,
            })),
        }
    }

    fn tick(&self, ctx: &mut dyn DrawContext, bounds: CanvasBounds) {
        let mut inner = self.inner.lock().expect("sketch state poisoned");
        inner.tick(ctx, bounds);
    }
}

struct SketchInner {
    sketch: Box<dyn Sketch>,
    start: Instant,
    last: Instant,
    frame_count: u64,
    did_setup: bool,
}

impl SketchInner {
    fn tick(&mut self, ctx: &mut dyn DrawContext, bounds: CanvasBounds) {
        let now = Instant::now();
        // First draw() sees dt = 0 rather than whatever elapsed between
        // SketchHandle construction and the first render — keeps motion
        // derived from `dt` from lurching on frame 1.
        let dt = if self.did_setup {
            now.duration_since(self.last).as_secs_f32()
        } else {
            0.0
        };
        let t = now.duration_since(self.start).as_secs_f32();
        self.last = now;

        let mut sctx = SketchContext {
            ctx,
            width: bounds.width,
            height: bounds.height,
            frame_count: self.frame_count,
        };

        if !self.did_setup {
            self.sketch.setup(&mut sctx);
            self.did_setup = true;
            // Setup may have initialized state that draw() depends on; if
            // it reset the clock somehow, the `t` we computed above is
            // still correct relative to `self.start`.
        }
        self.sketch.draw(&mut sctx, t, dt);
        self.frame_count += 1;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Player extension point
// ─────────────────────────────────────────────────────────────────────────────

/// Time-driven drawable loaded from external data — e.g. a Lottie file,
/// a Jitter scene, an exported timeline. Extensions implement this trait
/// to plug playable content into a [`Sketch`] without needing any
/// plugin registry or hook system; the sketch just holds the player as
/// a field and calls [`Player::draw_at`] (or [`SketchContext::play`])
/// where it wants.
///
/// # Contract
///
/// - `draw_at` is the only required method. Implementors are responsible
///   for parsing their own source format, caching GPU resources between
///   frames, and mapping the incoming `t` onto their own timeline
///   (looping, clamping, easing, etc.).
/// - Players are `Send + 'static` so they can live inside a sketch
///   stored in `BlincContextState::use_state_keyed`, which requires
///   cloneable-and-sendable state.
/// - Paused players should render their frozen pose regardless of the
///   `t` value passed to `draw_at`. The pause/seek state is internal to
///   the player; the host sketch controls it via [`Player::set_playing`]
///   and [`Player::seek`].
///
/// # Example
///
/// ```ignore
/// use blinc_canvas_kit::prelude::*;
/// use blinc_core::layer::Rect;
/// use blinc_lottie::LottiePlayer;
///
/// struct Hero { logo: LottiePlayer }
///
/// impl Sketch for Hero {
///     fn draw(&mut self, ctx: &mut SketchContext, t: f32, _dt: f32) {
///         ctx.play(&mut self.logo, Rect::new(40.0, 40.0, 200.0, 200.0), t);
///     }
/// }
/// ```
pub trait Player: Send + 'static {
    /// Total playback duration in seconds. `None` signals content that
    /// plays indefinitely (procedural, live, or user-controlled).
    fn duration(&self) -> Option<f32>;

    /// Render one frame at sketch time `t` into `rect`.
    ///
    /// The player is responsible for interpolating its own scene at
    /// time `t`, then dispatching draw calls into the provided
    /// `SketchContext`. Use `ctx.draw_context()` for raw access to the
    /// underlying `DrawContext` (paths, gradients, glass, images, 3D).
    fn draw_at(&mut self, ctx: &mut SketchContext<'_>, rect: Rect, t: f32);

    /// Seek internal playback to `t` seconds. Default: no-op — players
    /// that derive every frame's pose from the incoming `t` parameter
    /// don't need to override this.
    fn seek(&mut self, _t: f32) {
        let _ = _t;
    }

    /// Pause or resume playback. Paused players should render their
    /// frozen pose and ignore the `t` argument to `draw_at`. Default:
    /// no-op — a player that always renders from the caller's `t` has
    /// nothing to pause.
    fn set_playing(&mut self, _playing: bool) {
        let _ = _playing;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Canvas event bundle
// ─────────────────────────────────────────────────────────────────────────────

/// Attach a single callback to the bundle of input events that
/// canvas-style widgets typically care about — pointer down / up /
/// move, scroll, key down / up.
///
/// Intended for use on the `Div` returned by [`sketch`], or any `Div`
/// wrapping a canvas element. It bundles what would otherwise be six
/// separate `on_event` / `on_key_*` / `on_scroll` calls so downstream
/// code (e.g. `blinc_input::InputState::record`) can subscribe to the
/// whole input stream in one line:
///
/// ```ignore
/// use blinc_canvas_kit::sketch::{sketch, SketchEvents};
/// use blinc_input::InputState;
///
/// let input = InputState::new();
/// let i = input.clone();
/// sketch("demo", my_sketch).on_canvas_events(move |e| i.record(e))
/// ```
///
/// Scoped to the receiving `Div`'s subtree: pointer and scroll bubble
/// through every ancestor of the hit element, so the root `Div`
/// returned by [`sketch`] sees every canvas-directed pointer event.
/// Key events are subject to Blinc's focus routing — they reach this
/// `Div` after the first pointer-down inside its subtree, and may be
/// absorbed by a descendant that handles keys itself.
pub trait SketchEvents: Sized {
    /// Register `cb` to receive every pointer / scroll / key event
    /// that routes through this `Div`. Returns the modified `Div`.
    fn on_canvas_events<F>(self, cb: F) -> Self
    where
        F: Fn(&EventContext) + 'static;
}

impl SketchEvents for Div {
    fn on_canvas_events<F>(self, cb: F) -> Self
    where
        F: Fn(&EventContext) + 'static,
    {
        // `Rc` — single-threaded UI context matches Blinc's existing
        // `Canvas` / event-handler types (`EventCallback = Rc<dyn ...>`).
        let cb = Rc::new(cb);
        let f1 = cb.clone();
        let f2 = cb.clone();
        let f3 = cb.clone();
        let f4 = cb.clone();
        let f5 = cb.clone();
        let f6 = cb;
        self.on_event(event_types::POINTER_DOWN, move |e| f1(e))
            .on_event(event_types::POINTER_UP, move |e| f2(e))
            .on_event(event_types::POINTER_MOVE, move |e| f3(e))
            .on_scroll(move |e| f4(e))
            .on_key_down(move |e| f5(e))
            .on_key_up(move |e| f6(e))
    }
}


//! EXPERIMENTAL — hand-rolled Wayland `wl_surface::frame()` gate.
//!
//! Mirrors GPUI's Linux pattern: Blinc registers its own frame
//! callbacks via `wayland-client` directly on the window's surface,
//! and only allows `Surface::get_current_texture()` to run after the
//! compositor delivers `wl_callback::Event::Done`.
//!
//! winit 0.30 implements the same gating internally via
//! `Window::pre_present_notify`. On most compositors that's enough.
//! On some Mesa-driven Wayland setups (real user report: Intel HD
//! Graphics 620 + unknown compositor) the gating doesn't engage:
//! `Done` either never delivers to winit's calloop queue or arrives
//! at a stale token (the `calloop "non-existence source"` WARN).
//! Either failure mode leaves the next acquire blocking on the
//! 1s wgpu internal timeout — frozen UI from the user's point of
//! view.
//!
//! When this module is active (Linux + `wayland-frame-gate` feature
//! + a Wayland surface — anything else short-circuits to a no-op):
//!
//! * After every present, Blinc calls `arm_after_present()`. This
//!   sends a `wl_surface::frame()` request bound to OUR event
//!   queue and flips `frame_ready` to `false`.
//! * On every `RedrawRequested`, Blinc calls `dispatch_pending()`
//!   then checks `is_frame_ready()`. If the compositor hasn't
//!   delivered Done yet, the redraw is skipped and `request_redraw`
//!   re-arms — no blocking acquire is attempted.
//! * Blinc skips winit's `pre_present_notify` when the gate is
//!   active, to avoid two parallel `wl_surface::frame()`
//!   registrations racing each other.
//!
//! ## Why this can work alongside winit
//!
//! `wayland-client` 0.31 lets us share winit's wayland connection
//! via `Backend::from_foreign_display(*mut wl_display)`. The
//! resulting `Connection` doesn't own the socket — winit's read
//! loop continues to pull events from it as normal. We bind our
//! own `EventQueue` to that shared connection, and create
//! `wl_callback` proxies bound to OUR queue. Wayland's per-proxy
//! queue dispatch means our Done events arrive on our queue, not
//! winit's; pointer / keyboard / output events still go to winit.
//!
//! The wl_surface itself is created by winit. We wrap its raw
//! pointer (from `raw_window_handle::WaylandWindowHandle::surface`)
//! into an `ObjectId` and then a typed `WlSurface` proxy in our
//! connection. Sending `frame()` on it produces a new wl_callback
//! tracked by our queue.
//!
//! ## Failure modes
//!
//! * On a non-Wayland session (X11, Mir, headless): `try_new`
//!   returns `None`, gate is inert.
//! * If `Backend::from_foreign_display` rejects the pointer
//!   (compositor protocol mismatch, NULL display): returns `None`.
//! * If the surface pointer can't be wrapped: returns `None`.
//! * If a Done event never arrives (compositor-level bug):
//!   `is_frame_ready()` stays false forever. Caller MUST have a
//!   timeout or input-driven recovery — the windowed runner uses
//!   "any input event forces frame_ready true" to avoid permanent
//!   freezes.

#![cfg(all(feature = "wayland-frame-gate", target_os = "linux"))]

use std::ffi::c_void;
use std::sync::Mutex;

use wayland_backend::client::{Backend, ObjectId};
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};

/// Per-frame mutable state for the gate, owned by the event-queue
/// dispatcher. Accessed under `WaylandFrameGate::state` lock.
pub(crate) struct GateState {
    /// `true` when the compositor has delivered `Done` for the last
    /// armed `wl_surface::frame()` callback. Set to `false` by
    /// `arm_after_present`; flipped back to `true` by the dispatch
    /// impl below when a Done event arrives.
    frame_ready: bool,
    /// Monotonic counter of Done events received — telemetry for
    /// debugging "did Done ever fire?" without parsing logs.
    pub(crate) callbacks_received: u64,
    /// Wall-clock instant when the most recent
    /// `wl_surface::frame()` was armed. Used by the safety-valve in
    /// `is_frame_ready_or_timeout` to recover from a compositor
    /// that's stopped delivering Done events without permanently
    /// freezing the UI.
    armed_at: Option<std::time::Instant>,
}

impl Dispatch<WlCallback, ()> for GateState {
    fn event(
        state: &mut Self,
        _proxy: &WlCallback,
        event: wl_callback::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_callback::Event::Done { .. }) {
            state.frame_ready = true;
            state.callbacks_received = state.callbacks_received.wrapping_add(1);
        }
    }
}

/// Hand-rolled Wayland frame-callback gate. Construct via
/// [`Self::try_new_from_raw`]; lifecycle is documented at module
/// level.
pub struct WaylandFrameGate {
    #[allow(dead_code)] // kept alive so the connection stays valid for the queue
    conn: Connection,
    surface: WlSurface,
    queue: Mutex<EventQueue<GateState>>,
    state: Mutex<GateState>,
}

impl WaylandFrameGate {
    /// Attempt to construct a gate from raw Wayland pointers.
    ///
    /// `display_ptr` must be the `wl_display *` owned by winit (or
    /// the host event loop). `surface_ptr` must be the
    /// `wl_surface *` for the window we're gating frames on.
    /// Returns `None` if either pointer is null, the wayland-backend
    /// rejects the display, or the surface can't be wrapped as a
    /// proxy in our connection.
    ///
    /// Safety: callers must guarantee both pointers remain valid for
    /// the lifetime of the returned gate (winit owns them; the gate
    /// must be dropped before winit tears down the window). We don't
    /// take ownership of either — `Backend::from_foreign_display`
    /// shares the socket, and `ObjectId::from_ptr` borrows the
    /// surface.
    pub fn try_new_from_raw(display_ptr: *mut c_void, surface_ptr: *mut c_void) -> Option<Self> {
        if display_ptr.is_null() || surface_ptr.is_null() {
            return None;
        }

        // SAFETY: caller contract — display_ptr is a live wl_display
        // managed by winit's wayland-client / SCTK integration.
        // `from_foreign_display` doesn't try to close the socket on
        // Backend drop when given an external pointer.
        let backend = unsafe { Backend::from_foreign_display(display_ptr.cast()) };
        let conn = Connection::from_backend(backend);

        // Wrap winit's wl_surface pointer as an ObjectId in our
        // connection. `from_ptr` requires the interface to match —
        // wayland-backend validates by reading the proxy's interface
        // tag at the pointer.
        let surface_id =
            unsafe { ObjectId::from_ptr(WlSurface::interface(), surface_ptr.cast()).ok()? };
        let surface = WlSurface::from_id(&conn, surface_id).ok()?;

        let queue: EventQueue<GateState> = conn.new_event_queue();
        let state = GateState {
            // Start ready: the first frame is allowed before any
            // callback has been armed (no compositor signal needed
            // for frame zero).
            frame_ready: true,
            callbacks_received: 0,
            armed_at: None,
        };

        Some(Self {
            conn,
            surface,
            queue: Mutex::new(queue),
            state: Mutex::new(state),
        })
    }

    /// `true` if the compositor has delivered `wl_callback::Done`
    /// for the most recently armed frame callback (or no callback
    /// is currently armed — the initial state).
    pub fn is_frame_ready(&self) -> bool {
        // `expect`: lock is internally consistent; only this module
        // touches it and we never poison via panic-during-hold.
        self.state.lock().expect("gate state").frame_ready
    }

    /// `true` if [`Self::is_frame_ready`] would return `true`, OR if
    /// more than `timeout` has elapsed since the most recent
    /// `arm_after_present`. Safety valve against a permanently
    /// frozen UI when the compositor stops delivering `Done` —
    /// after the timeout, we proceed with the frame anyway and let
    /// the wgpu acquire path decide whether to render or skip.
    ///
    /// 100 ms is a sensible default ceiling: it's well above one
    /// vsync interval on every realistic monitor (16.7 ms @ 60 Hz),
    /// so a compositor delivering Done normally is never affected;
    /// but it caps the user-perceived freeze duration at one tenth
    /// of a second, which is below the human-perceptible
    /// "unresponsive" threshold.
    pub fn is_frame_ready_or_timeout(&self, timeout: std::time::Duration) -> bool {
        let state = self.state.lock().expect("gate state");
        if state.frame_ready {
            return true;
        }
        match state.armed_at {
            Some(armed) => armed.elapsed() >= timeout,
            None => true,
        }
    }

    /// Drain any pending events on our queue (notably
    /// `wl_callback::Done`). Cheap — typically zero events per call
    /// when nothing has arrived. Safe to call unconditionally each
    /// frame.
    pub fn dispatch_pending(&self) {
        let mut queue = self.queue.lock().expect("gate queue");
        let mut state = self.state.lock().expect("gate state");
        // `dispatch_pending` consumes events already routed into our
        // queue by winit's connection-level reader. We do NOT call
        // `roundtrip` or any blocking variant — winit owns the read
        // path and we just drain what's been delivered to us.
        let _ = queue.dispatch_pending(&mut *state);
    }

    /// Register a fresh `wl_surface::frame()` callback bound to our
    /// queue, and flip `frame_ready` to `false`. Call **immediately
    /// before** `frame.present()`.
    ///
    /// ## Why before, not after
    ///
    /// Per the Wayland protocol, `wl_surface::frame()` registers a
    /// callback that becomes armed by the **next**
    /// `wl_surface::commit()`. `wgpu::SurfaceTexture::present()`
    /// internally calls that commit. If we call `frame()` *after*
    /// present, our request is buffered for the commit AFTER this
    /// one — and if the render loop goes quiet (animations settle,
    /// no input pending), that next commit never happens. The
    /// compositor never delivers Done for our callback, and the
    /// gate sits permanently waiting. From the user's perspective:
    /// "UI is responsive for a few seconds then freezes" — the
    /// first frames work because continuous activity keeps
    /// commits flowing, then the quiet period traps us.
    ///
    /// Arming *before* present means our `frame()` request hits
    /// the wire (`conn.flush()`) before wgpu's commit, so the
    /// callback bundles with THIS commit and Done arrives on the
    /// next vsync. GPUI uses the same ordering on Linux.
    ///
    /// The next call to `is_frame_ready()` will return `false`
    /// until the compositor sends the corresponding `Done` event.
    pub fn arm_before_present(&self) {
        let queue = self.queue.lock().expect("gate queue");
        let qh = queue.handle();
        // Send the request. The returned callback proxy is bound to
        // our queue via `qh` — its `Done` event will arrive on our
        // dispatch path. We drop the proxy: the protocol keeps the
        // server-side object alive until `Done`, and the queue holds
        // the registration regardless of whether we retain a handle.
        let _callback = self.surface.frame(&qh, ());
        // Flush MUST happen before wgpu's commit. If the request
        // hits the wire AFTER the commit, the callback buffers for
        // the next commit (see the docs above for the freeze
        // pathology).
        let _ = self.conn.flush();
        drop(queue);
        let mut state = self.state.lock().expect("gate state");
        state.frame_ready = false;
        state.armed_at = Some(std::time::Instant::now());
    }

    /// Force-mark frame-ready. Used as the recovery path when an
    /// input event fires while we're waiting on a Done that hasn't
    /// arrived — protects against permanent freeze on
    /// compositor-side callback-delivery bugs (the
    /// `non-existence source` calloop-warn class).
    pub fn force_ready(&self) {
        self.state.lock().expect("gate state").frame_ready = true;
    }

    /// Telemetry — total Done events received over the gate's
    /// lifetime. `0` after init even if `is_frame_ready()` returns
    /// `true` (the initial-frame allowance doesn't count).
    pub fn callbacks_received(&self) -> u64 {
        self.state.lock().expect("gate state").callbacks_received
    }
}

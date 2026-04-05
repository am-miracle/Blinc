//! Pull-to-refresh widget using Stateful FSM
//!
//! Uses Blinc's `Stateful` container with FSM state transitions.
//! The FSM handles state changes automatically — the widget only
//! provides the `on_state` callback and event handlers.
//!
//! # Example
//!
//! ```ignore
//! use blinc_layout::widgets::pull_to_refresh::pull_to_refresh;
//!
//! pull_to_refresh(|| {
//!     println!("Refreshing...");
//! })
//! .threshold(60.0)
//! .w_full()
//! .h(400.0)
//! .into_div()
//! ```

use std::sync::{Arc, Mutex};

use blinc_core::Color;

use crate::div::{div, Div};
use crate::stateful::{SharedState, Stateful, StatefulInner};
use crate::text::text;

/// Pull-to-refresh FSM states
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum PullState {
    #[default]
    Idle,
    Pulling,
    Armed,
    Refreshing,
}

impl crate::stateful::StateTransitions for PullState {
    fn on_event(&self, event: u32) -> Option<Self> {
        use blinc_core::events::event_types;
        match (self, event) {
            (PullState::Idle, event_types::POINTER_DOWN) => Some(PullState::Pulling),
            (PullState::Pulling, event_types::POINTER_UP) => Some(PullState::Idle),
            (PullState::Armed, event_types::POINTER_UP) => Some(PullState::Refreshing),
            (PullState::Refreshing, event_types::POINTER_DOWN) => Some(PullState::Idle),
            _ => None,
        }
    }
}

/// Shared drag tracking (Send + Sync safe)
struct DragTracker {
    start_y: f32,
    offset: f32,
    threshold: f32,
    max_pull: f32,
}

/// Pull-to-refresh container builder
pub struct PullToRefresh {
    on_refresh: Arc<dyn Fn() + Send + Sync>,
    threshold: f32,
    max_pull: f32,
    inner: Div,
}

/// Create a pull-to-refresh container.
pub fn pull_to_refresh<F>(on_refresh: F) -> PullToRefresh
where
    F: Fn() + Send + Sync + 'static,
{
    PullToRefresh {
        on_refresh: Arc::new(on_refresh),
        threshold: 60.0,
        max_pull: 100.0,
        inner: div().overflow_clip(),
    }
}

impl PullToRefresh {
    pub fn threshold(mut self, px: f32) -> Self {
        self.threshold = px;
        self
    }
    pub fn max_pull(mut self, px: f32) -> Self {
        self.max_pull = px;
        self
    }
    pub fn w(mut self, v: f32) -> Self {
        self.inner = self.inner.w(v);
        self
    }
    pub fn h(mut self, v: f32) -> Self {
        self.inner = self.inner.h(v);
        self
    }
    pub fn w_full(mut self) -> Self {
        self.inner = self.inner.w_full();
        self
    }
    pub fn h_full(mut self) -> Self {
        self.inner = self.inner.h_full();
        self
    }
    pub fn bg(mut self, color: Color) -> Self {
        self.inner = self.inner.bg(color);
        self
    }
    pub fn rounded(mut self, r: f32) -> Self {
        self.inner = self.inner.rounded(r);
        self
    }
    pub fn id(mut self, id: &str) -> Self {
        self.inner = self.inner.id(id);
        self
    }
    pub fn class(mut self, class: &str) -> Self {
        self.inner = self.inner.class(class);
        self
    }

    /// Build into a Div
    pub fn into_div(self) -> Div {
        let tracker = Arc::new(Mutex::new(DragTracker {
            start_y: 0.0,
            offset: 0.0,
            threshold: self.threshold,
            max_pull: self.max_pull,
        }));

        let on_refresh = self.on_refresh;

        let shared_state: SharedState<PullState> =
            Arc::new(Mutex::new(StatefulInner::new(PullState::Idle)));

        // State callback: the Stateful system calls this when FSM state changes
        {
            let tracker_for_cb = Arc::clone(&tracker);
            let mut shared = shared_state.lock().unwrap();
            shared.state_callback =
                Some(Arc::new(move |state: &PullState, container: &mut Div| {
                    let t = tracker_for_cb.lock().unwrap();
                    let offset = t.offset;
                    let progress = (offset / t.threshold).min(1.0);

                    let (label, opacity, h) = match state {
                        PullState::Idle => ("", 0.0_f32, 0.0_f32),
                        PullState::Pulling => ("Pull to refresh", progress, offset.max(0.0)),
                        PullState::Armed => ("Release to refresh", 1.0, offset.max(0.0)),
                        PullState::Refreshing => ("Refreshing...", 1.0, 40.0),
                    };

                    let indicator = div()
                        .w_full()
                        .h(h)
                        .items_center()
                        .justify_center()
                        .opacity(opacity)
                        .child(
                            text(label)
                                .size(12.0)
                                .color(Color::rgba(0.5, 0.5, 0.6, 1.0)),
                        );

                    container.set_child(indicator);
                }));
            shared.needs_visual_update = true;
        }

        // Event handlers — the Stateful FSM handles state transitions.
        // We only track drag offset and fire the refresh callback.
        let tracker_down = Arc::clone(&tracker);
        let tracker_move = Arc::clone(&tracker);
        let tracker_up = Arc::clone(&tracker);
        let shared_for_move = Arc::clone(&shared_state);
        let shared_for_up = Arc::clone(&shared_state);

        let stateful = Stateful::with_shared_state(shared_state)
            .on_mouse_down(move |ctx| {
                let mut t = tracker_down.lock().unwrap();
                t.start_y = ctx.mouse_y;
                t.offset = 0.0;
                // FSM transition (Idle → Pulling) is handled by Stateful automatically
            })
            .on_mouse_move(move |ctx| {
                // Update drag offset during pull
                let mut t = tracker_move.lock().unwrap();
                let s = shared_for_move.lock().unwrap();
                if s.state != PullState::Pulling && s.state != PullState::Armed {
                    return;
                }
                let delta = (ctx.mouse_y - t.start_y).max(0.0).min(t.max_pull);
                t.offset = delta;

                // Transition between Pulling ↔ Armed based on threshold
                let armed = delta >= t.threshold;
                if armed && s.state == PullState::Pulling {
                    drop(s);
                    drop(t);
                    let mut s = shared_for_move.lock().unwrap();
                    s.state = PullState::Armed;
                    s.needs_visual_update = true;
                } else if !armed && s.state == PullState::Armed {
                    drop(s);
                    drop(t);
                    let mut s = shared_for_move.lock().unwrap();
                    s.state = PullState::Pulling;
                    s.needs_visual_update = true;
                }
            })
            .on_mouse_up(move |_ctx| {
                let was_armed = {
                    let s = shared_for_up.lock().unwrap();
                    s.state == PullState::Armed
                };
                if was_armed {
                    // Update offset for refreshing indicator position
                    let mut t = tracker_up.lock().unwrap();
                    t.offset = 40.0;
                    drop(t);
                    // FSM transition (Armed → Refreshing) handled by Stateful
                    // Fire the refresh callback
                    on_refresh();
                } else {
                    let mut t = tracker_up.lock().unwrap();
                    t.offset = 0.0;
                    // FSM transition (Pulling → Idle) handled by Stateful
                }
            });

        self.inner.child(stateful)
    }
}

impl From<PullToRefresh> for Div {
    fn from(ptr: PullToRefresh) -> Div {
        ptr.into_div()
    }
}

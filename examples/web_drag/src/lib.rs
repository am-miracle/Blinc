//! Blinc web example — drag gestures via Stateful + State + on_drag.
//!
//! Validates that the same drag patterns the native sortable demo
//! ([`crates/blinc_app/examples/sortable_demo.rs`](../../crates/blinc_app/examples/sortable_demo.rs))
//! uses work unchanged on wasm32. Three things have to be wired
//! correctly for this to render anything visible on click + drag:
//!
//! 1. **`BlincContextState` global initialised** — without this,
//!    `BlincContextState::get().use_state_keyed(...)` panics or
//!    no-ops, and `Stateful::on_state` closures never see the live
//!    state cell. The web runner now calls
//!    `init_with_callback` from `WebApp::new`, mirroring the
//!    desktop runner at [`windowed.rs:2114`](../../crates/blinc_app/src/windowed.rs#L2114).
//!
//! 2. **`ref_dirty_flag` polled per frame** — `State::set` flips
//!    this atomic via the singleton, but until the runner reads it
//!    and sets `needs_rebuild`, the state mutation never reaches
//!    the screen. The web runner now polls it inside Phase 1 of
//!    `run_one_frame`, mirroring [`windowed.rs:3513`](../../crates/blinc_app/src/windowed.rs#L3513).
//!
//! 3. **`incremental_update` instead of `from_element_with_registry`** —
//!    every dirty trigger that did a full rebuild used to throw
//!    away `scroll_physics`, `node_states`, the dirty tracker, and
//!    everything else. The runner now calls
//!    `tree.incremental_update(&element)` and only rebuilds the
//!    subtrees whose hashes changed.
//!
//! Once those three are in place, *the same code that runs on
//! desktop / Android / iOS* runs here. The widget code in this
//! file is intentionally a near-copy of the sortable_demo's
//! sortable list section, scaled down to a single draggable card.
//!
//! ## Build
//!
//! ```bash
//! cd examples/web_drag
//! wasm-pack build --target web --release
//! ```
//!
//! Then serve with `./serve.sh` and open `http://localhost:8000/`
//! in Chrome 113+ (or any browser with WebGPU enabled).

#![cfg(target_arch = "wasm32")]

use blinc_app::web::WebApp;
use blinc_app::windowed::WindowedContext;
use blinc_core::context_state::BlincContextState;
use blinc_core::reactive::State;
use blinc_core::{Color, Transform};
use blinc_layout::div::{div, Div};
use blinc_layout::stateful::{stateful_with_key, Stateful};
use blinc_layout::text::text;
use blinc_layout::FontWeight;
use wasm_bindgen::prelude::*;

/// Bundled font shared with `web_hello`. Browsers can't hand wgpu
/// their system fonts (those live in the compositor's 2D pipeline,
/// not in WebGPU), so the font bytes have to live on the wasm side.
const ARIAL_TTF: &[u8] = include_bytes!("../../web_hello/fonts/Arial.ttf");

/// FSM for the drag container. Identical shape to the
/// `DragFSM` in [`sortable_demo.rs`](../../crates/blinc_app/examples/sortable_demo.rs)
/// — `Idle` until the user mouses down, `Dragging` while a drag
/// gesture is in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
enum DragFSM {
    #[default]
    Idle,
    Dragging,
}

impl blinc_layout::stateful::StateTransitions for DragFSM {
    fn on_event(&self, event: u32) -> Option<Self> {
        use blinc_core::events::event_types;
        match (self, event) {
            (DragFSM::Idle, event_types::DRAG) => Some(DragFSM::Dragging),
            (DragFSM::Dragging, event_types::DRAG_END) => Some(DragFSM::Idle),
            (DragFSM::Dragging, event_types::POINTER_UP) => Some(DragFSM::Idle),
            _ => None,
        }
    }
}

/// Atomic state for the draggable card: position + dragging flag
/// change together via `State::update` so an intermediate frame
/// never sees one updated before the other.
#[derive(Clone, Debug, Default)]
struct CardState {
    /// Visual offset from the card's natural layout position. Reset
    /// to (0, 0) every drag end so the next drag starts fresh.
    offset_x: f32,
    offset_y: f32,
    /// Whether a drag is currently in progress. Drives the visual
    /// "lifted" treatment (raised z-index, slightly transparent).
    dragging: bool,
}

/// wasm-bindgen entry point. The `start` attribute makes this run
/// automatically when the browser loads the generated `.js` shim.
#[wasm_bindgen(start)]
pub fn _start() {
    console_error_panic_hook::set_once();

    tracing_wasm::set_as_global_default_with_config(
        tracing_wasm::WASMLayerConfigBuilder::new()
            .set_max_level(tracing::Level::INFO)
            .build(),
    );

    wasm_bindgen_futures::spawn_local(async {
        let result = WebApp::run_with_setup(
            "blinc-canvas",
            |app| {
                let faces = app.load_font_data(ARIAL_TTF.to_vec());
                web_sys::console::log_1(
                    &format!("blinc_web_drag: registered {faces} font face(s) from Arial.ttf")
                        .into(),
                );
            },
            build_ui,
        )
        .await;

        if let Err(e) = result {
            web_sys::console::error_1(
                &format!("blinc_web_drag: WebApp::run failed: {e}").into(),
            );
        }
    });
}

/// User UI builder. Re-invoked by the runner whenever
/// `tree.needs_rebuild()`, `take_needs_rebuild()`, or
/// `ref_dirty_flag` triggers a rebuild — including via `State::set`
/// from inside any of the drag handlers below.
fn build_ui(_ctx: &mut WindowedContext) -> Div {
    div()
        .w_full()
        .h_full()
        .bg(Color::rgba(0.07, 0.07, 0.10, 1.0))
        .flex_col()
        .items_center()
        .p(24.0)
        .gap(16.0)
        .child(
            text("Blinc · Drag gesture demo")
                .size(22.0)
                .color(Color::rgba(0.92, 0.92, 0.95, 1.0)),
        )
        .child(
            text("Drag the card around. Release to drop.")
                .size(13.0)
                .color(Color::rgba(0.65, 0.65, 0.72, 1.0)),
        )
        // Wrap the Stateful in a normal Div so the build_ui return
        // type is `Div` (which is what `WebApp::run` requires). The
        // native sortable demo does the same thing — Stateful is an
        // ElementBuilder, so it's a valid `.child(...)` argument
        // even though it isn't itself a Div.
        .child(draggable_card())
}

/// The draggable card. Single Stateful container that:
///
/// - Owns a `CardState` cell holding the visual offset and the
///   `dragging` flag, both keyed under `"web_drag_card"` so the
///   state survives tree rebuilds.
/// - Re-runs its `on_state` body whenever the cell's signal
///   changes (the `deps([...])` line wires the dependency).
/// - Captures the cell into the `on_mouse_down` / `on_drag` /
///   `on_drag_end` closures and mutates it via `state.update(...)`,
///   which flips `ref_dirty_flag` via the BlincContextState
///   singleton — the runner picks that up on the next frame and
///   rebuilds.
///
/// This is structurally identical to the `sortable_list_section`
/// in [`sortable_demo.rs`](../../crates/blinc_app/examples/sortable_demo.rs#L324),
/// minus the multi-item swap detection — it's just one card you
/// can move around.
fn draggable_card() -> Stateful<DragFSM> {
    let blinc = BlincContextState::get();
    let state: State<CardState> = blinc.use_state_keyed("web_drag_card", CardState::default);

    // Clones for each handler. The native sortable demo follows the
    // same pattern — every closure that captures the state cell
    // takes its own clone, so the borrow checker stays happy.
    let state_for_render = state.clone();
    let state_for_down = state.clone();
    let state_for_drag = state.clone();
    let state_for_end = state.clone();

    stateful_with_key::<DragFSM>("web-drag-card-container")
        .deps([state.signal_id()])
        .on_state(move |_ctx| {
            let s = state_for_render.get();

            let mut card = div()
                .w(220.0)
                .h(120.0)
                .bg(Color::rgba(0.32, 0.55, 0.92, 1.0))
                .rounded(16.0)
                .items_center()
                .justify_center()
                .child(
                    text("Drag me")
                        .size(20.0)
                        .weight(FontWeight::SemiBold)
                        .color(Color::WHITE),
                );

            // While dragging: lift visually via translate + opacity
            // dip + raised z-index. Same recipe as
            // `sortable_demo.rs:379-384`.
            if s.dragging {
                card = card
                    .transform(Transform::translate(s.offset_x, s.offset_y))
                    .opacity(0.85)
                    .z_index(100);
            }

            card
        })
        .on_mouse_down(move |_e| {
            // Mark the card as dragging. The visual offset starts
            // at (0, 0) — the EventRouter's drag tracker will feed
            // us cumulative deltas in `e.drag_delta_*` from the
            // mousedown anchor onward.
            state_for_down.update(|mut s| {
                s.dragging = true;
                s.offset_x = 0.0;
                s.offset_y = 0.0;
                s
            });
        })
        .on_drag(move |e| {
            // Update offset directly from the drag deltas the
            // EventRouter accumulates. These are computed
            // relative to the mousedown anchor, so they go to
            // (0, 0) at the start of each drag and grow as the
            // pointer moves.
            state_for_drag.update(|mut s| {
                s.offset_x = e.drag_delta_x;
                s.offset_y = e.drag_delta_y;
                s
            });
        })
        .on_drag_end(move |_e| {
            // Reset visual state on drop. The card animates back
            // to (0, 0) instantly because there's no transition on
            // it — for a smoother release, this is where you'd
            // start a spring on the offset.
            state_for_end.update(|mut s| {
                s.dragging = false;
                s.offset_x = 0.0;
                s.offset_y = 0.0;
                s
            });
        })
}

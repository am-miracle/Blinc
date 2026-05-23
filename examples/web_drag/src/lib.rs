//! Blinc web example — drag gestures via Stateful + State + on_drag.
//!
//! Validates that the same drag patterns the native sortable demo
//! ([`examples/blinc_app_examples/examples/sortable_demo.rs`](../../examples/blinc_app_examples/examples/sortable_demo.rs))
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
use blinc_layout::FontWeight;
use blinc_layout::div::{Div, div};
use blinc_layout::stateful::{Stateful, stateful_with_key};
use blinc_layout::text::text;
use wasm_bindgen::prelude::*;

/// Bundled font from `assets/fonts/` at the workspace root.
/// Browsers can't hand wgpu their system fonts, so the font bytes
/// are included in the wasm binary via `include_bytes!`.
const ARIAL_TTF: &[u8] = include_bytes!("../../../assets/fonts/Arial.ttf");

/// FSM for the drag container. Identical shape to the
/// `DragFSM` in [`sortable_demo.rs`](../../examples/blinc_app_examples/examples/sortable_demo.rs)
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

/// Visual offset of the card from its natural layout position.
/// Stored in a single tuple so a `State::update` rewrites both axes
/// atomically — the rebuild that follows always sees a consistent
/// pair instead of x-from-frame-N + y-from-frame-N+1.
///
/// We deliberately do *not* track an "is dragging" bool here. The
/// Stateful container already owns a `DragFSM` (`Idle` /
/// `Dragging`) that the framework transitions automatically as
/// DRAG / DRAG_END / POINTER_UP events fire — the on_state body
/// reads that via `ctx.state()` instead of duplicating the
/// dragging flag here.
type CardOffset = (f32, f32);

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
            web_sys::console::error_1(&format!("blinc_web_drag: WebApp::run failed: {e}").into());
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
/// - Holds its dragging-vs-idle status in the framework's built-in
///   `DragFSM` — `ctx.state()` inside the on_state body returns
///   the current FSM state, no parallel `bool` needed. The
///   framework transitions Idle → Dragging on the first DRAG
///   event and Dragging → Idle on DRAG_END / POINTER_UP via the
///   `StateTransitions` impl above.
///
/// - Holds the *visual offset* in a separate `State<CardOffset>`
///   cell, listed in `deps([offset.signal_id()])`. The offset
///   changes on every drag tick — much more frequently than the
///   FSM transition — so decoupling the two cells keeps the
///   re-render cadence right and matches the same split the
///   `sortable_list_section` in
///   [`sortable_demo.rs`](../../examples/blinc_app_examples/examples/sortable_demo.rs#L324)
///   uses (`State<SortListState>` for the items + `State<f32>`
///   for `drag_offset`).
///
/// - Each handler captures its own clone of the offset cell and
///   mutates it via `set` / `update`. Those mutations flip
///   `ref_dirty_flag` through the `BlincContextState` singleton,
///   the runner picks that up on the next frame, and the
///   incremental update path re-runs the on_state body with the
///   fresh values.
fn draggable_card() -> Stateful<DragFSM> {
    let blinc = BlincContextState::get();
    let offset: State<CardOffset> = blinc.use_state_keyed("web_drag_offset", || (0.0, 0.0));

    // Clones for each handler. The native sortable demo follows the
    // same pattern — every closure that captures the state cell
    // takes its own clone, so the borrow checker stays happy.
    let offset_for_drag = offset.clone();
    let offset_for_end = offset.clone();

    stateful_with_key::<DragFSM>("web-drag-card-container")
        .deps([offset.signal_id()])
        .on_state(move |ctx| {
            let (ox, oy) = offset.get();
            let dragging = matches!(ctx.state(), DragFSM::Dragging);

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
            if dragging {
                card = card
                    .transform(Transform::translate(ox, oy))
                    .opacity(0.85)
                    .z_index(100);
            }

            card
        })
        .on_drag(move |e| {
            // Update offset directly from the drag deltas the
            // EventRouter accumulates. These are computed
            // relative to the mousedown anchor, so they go to
            // (0, 0) at the start of each drag and grow as the
            // pointer moves. The DragFSM transition Idle →
            // Dragging happens automatically inside the
            // Stateful's `register_state_handlers` path on the
            // same DRAG event that fires us, so by the time the
            // next rebuild runs, `ctx.state()` is already
            // `DragFSM::Dragging`.
            offset_for_drag.set((e.drag_delta_x, e.drag_delta_y));
        })
        .on_drag_end(move |_e| {
            // Snap back to the layout position. The DragFSM
            // transition Dragging → Idle is fired by the framework
            // on DRAG_END, so we just need to reset our visual
            // offset and let the next rebuild render the card at
            // its layout position.
            offset_for_end.set((0.0, 0.0));
        })
}

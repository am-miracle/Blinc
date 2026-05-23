//! Sortable Demo
//!
//! Demonstrates drag-based interactions using FSM-driven stateful containers:
//! - Sortable list: drag items to reorder
//! - Swipe to delete: horizontal drag to dismiss items (stack overlay)
//! - Sortable grid: 3x3 drag-to-reorder grid
//!
//! Run with: cargo run -p blinc_app_examples --example sortable_demo

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;
use blinc_app::windowed::WindowedContext;
use blinc_core::Color;
use blinc_core::context_state::BlincContextState;
use blinc_core::events::event_types;
use blinc_core::reactive::State;

// ============================================================================
// FSM State Types
// ============================================================================

/// FSM for drag containers (sortable list / grid)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
enum DragFSM {
    #[default]
    Idle,
    Dragging,
}

impl StateTransitions for DragFSM {
    fn on_event(&self, event: u32) -> Option<Self> {
        match (self, event) {
            (DragFSM::Idle, event_types::DRAG) => Some(DragFSM::Dragging),
            (DragFSM::Dragging, event_types::DRAG_END) => Some(DragFSM::Idle),
            (DragFSM::Dragging, event_types::POINTER_UP) => Some(DragFSM::Idle),
            _ => None,
        }
    }
}

/// FSM for swipe-to-delete items
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
enum SwipeFSM {
    #[default]
    Idle,
    Swiping,
}

impl StateTransitions for SwipeFSM {
    fn on_event(&self, event: u32) -> Option<Self> {
        match (self, event) {
            (SwipeFSM::Idle, event_types::DRAG) => Some(SwipeFSM::Swiping),
            (SwipeFSM::Swiping, event_types::DRAG_END) => Some(SwipeFSM::Idle),
            (SwipeFSM::Swiping, event_types::POINTER_UP) => Some(SwipeFSM::Idle),
            _ => None,
        }
    }
}

/// No-op FSM for reactive-only containers (rebuilds on signal deps, no event transitions)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
struct PassiveFSM;

impl StateTransitions for PassiveFSM {
    fn on_event(&self, _event: u32) -> Option<Self> {
        None
    }
}

// ============================================================================
// Data Types
// ============================================================================

#[derive(Clone, Debug)]
struct ListItem {
    id: usize,
    label: String,
    color: Color,
}

/// Atomic state for sortable list: items + drag index change together
/// to prevent intermediate frames where drag_idx points at the wrong item.
#[derive(Clone, Debug, Default)]
struct SortListState {
    items: Vec<ListItem>,
    drag_idx: Option<usize>,
}

/// Atomic state for sortable grid: items + drag index change together.
#[derive(Clone, Debug, Default)]
struct SortGridState {
    items: Vec<ListItem>,
    drag_idx: Option<usize>,
}

const ITEM_COLORS: [Color; 8] = [
    Color {
        r: 0.23,
        g: 0.51,
        b: 0.96,
        a: 1.0,
    },
    Color {
        r: 0.13,
        g: 0.77,
        b: 0.37,
        a: 1.0,
    },
    Color {
        r: 0.66,
        g: 0.33,
        b: 0.97,
        a: 1.0,
    },
    Color {
        r: 0.98,
        g: 0.45,
        b: 0.09,
        a: 1.0,
    },
    Color {
        r: 0.93,
        g: 0.27,
        b: 0.27,
        a: 1.0,
    },
    Color {
        r: 0.08,
        g: 0.71,
        b: 0.83,
        a: 1.0,
    },
    Color {
        r: 0.85,
        g: 0.53,
        b: 0.05,
        a: 1.0,
    },
    Color {
        r: 0.55,
        g: 0.22,
        b: 0.80,
        a: 1.0,
    },
];

fn make_items(count: usize, prefix: &str) -> Vec<ListItem> {
    (0..count)
        .map(|i| ListItem {
            id: i,
            label: format!("{} {}", prefix, i + 1),
            color: ITEM_COLORS[i % ITEM_COLORS.len()],
        })
        .collect()
}

// Layout constants
const LIST_ITEM_H: f32 = 48.0;
const LIST_GAP: f32 = 8.0;
const LIST_STEP: f32 = LIST_ITEM_H + LIST_GAP;
const LIST_W: f32 = 400.0;

const DELETE_THRESHOLD: f32 = 120.0;

const GRID_CELL: f32 = 100.0;
const GRID_GAP: f32 = 12.0;
const GRID_STEP: f32 = GRID_CELL + GRID_GAP;
const GRID_COLS: i32 = 3;

// ============================================================================
// Main
// ============================================================================

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let config = WindowConfig {
        title: "Blinc Sortable Demo".to_string(),
        width: 900,
        height: 850,
        resizable: true,
        fullscreen: false,
        ..Default::default()
    };

    WindowedApp::run(config, build_ui)
}

const STYLESHEET: &str = r#"
    #header {
        width: 100%;
        height: 72px;
        background: #1e1e2e;
        border-bottom: 1px solid #333346;
        flex-direction: row;
        align-items: center;
        justify-content: center;
        gap: 16px;
    }

    .section {
        width: 100%;
        background: #1e1e2e;
        border: 1px solid #333346;
        border-radius: 12px;
        padding: 24px;
        flex-direction: column;
        gap: 16px;
    }

    .sort-list {
        flex-direction: column;
        gap: 8px;
        align-items: center;
    }

    .sort-item {
        width: 400px;
        height: 48px;
        border-radius: 12px;
        flex-direction: row;
        align-items: center;
        padding: 0px 0px 0px 12px;
        gap: 12px;
        transition: transform 200ms ease;
    }

    .delete-list {
        flex-direction: column;
        gap: 8px;
        align-items: center;
    }

    .delete-bg {
        width: 100%;
        height: 100%;
        border-radius: 12px;
        background: #d92626;
        flex-direction: row;
        align-items: center;
        padding: 0px 0px 0px 16px;
    }

    .delete-card {
        width: 100%;
        height: 100%;
        border-radius: 12px;
        flex-direction: row;
        align-items: center;
        padding: 0px 0px 0px 12px;
        gap: 12px;
    }

    .grid-container {
        width: 324px;
        flex-direction: row;
        flex-wrap: wrap;
        gap: 12px;
    }

    .grid-item {
        width: 100px;
        height: 100px;
        border-radius: 12px;
        flex-direction: column;
        justify-content: center;
        align-items: center;
        transition: transform 200ms ease;
    }
"#;

// ============================================================================
// Layout
// ============================================================================

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    if ctx.rebuild_count == 0 {
        ctx.add_css(STYLESHEET);
    }

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.07, 0.07, 0.11, 1.0))
        .flex_col()
        .child(
            div()
                .id("header")
                .child(
                    text("Sortable Demo")
                        .size(24.0)
                        .weight(FontWeight::Bold)
                        .color(Color::WHITE),
                )
                .child(
                    text("Drag interactions with stateful containers")
                        .size(14.0)
                        .color(Color::rgba(1.0, 1.0, 1.0, 0.5)),
                ),
        )
        .child(
            scroll().w_full().h(ctx.height - 72.0).child(
                div()
                    .w_full()
                    .flex_col()
                    .gap(32.0)
                    .p(6.0)
                    .child(sortable_list_section())
                    .child(swipe_to_delete_section())
                    .child(sortable_grid_section()),
            ),
        )
}

// ============================================================================
// Section 1: Sortable List
// ============================================================================

fn sortable_list_section() -> Div {
    let blinc = BlincContextState::get();
    let state: State<SortListState> = blinc.use_state_keyed("sort_state", || SortListState {
        items: make_items(7, "Item"),
        drag_idx: None,
    });
    let drag_offset: State<f32> = blinc.use_state_keyed("sort_drag_offset", || 0.0);
    let swap_adj: State<f32> = blinc.use_state_keyed("sort_swap_adj", || 0.0);

    // Clones for on_state
    let state_s = state.clone();
    let drag_offset_s = drag_offset.clone();

    // Clones for on_mouse_down
    let state_md = state.clone();
    let swap_adj_md = swap_adj.clone();

    // Clones for on_drag
    let state_d = state.clone();
    let drag_offset_d = drag_offset.clone();
    let swap_adj_d = swap_adj.clone();

    // Clones for on_drag_end
    let state_de = state.clone();
    let drag_offset_de = drag_offset.clone();
    let swap_adj_de = swap_adj.clone();

    let sort_container = stateful_with_key::<DragFSM>("sort-list-container")
        .deps([state.signal_id(), drag_offset.signal_id()])
        .on_state(move |_ctx| {
            let st = state_s.get();
            let offset = drag_offset_s.get();

            let children: Vec<Div> = st
                .items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let is_dragged = st.drag_idx == Some(i);
                    let mut d = div()
                        .id(format!("sort-{}", item.id))
                        .class("sort-item")
                        .bg(item.color)
                        .child(
                            text("\u{2261}")
                                .size(20.0)
                                .color(Color::rgba(1.0, 1.0, 1.0, 0.6)),
                        )
                        .child(
                            text(&item.label)
                                .size(16.0)
                                .weight(FontWeight::Medium)
                                .color(Color::WHITE),
                        );

                    if is_dragged {
                        d = d
                            .transform(Transform::translate(0.0, offset))
                            .z_index(100)
                            .opacity(0.85);
                    }
                    d
                })
                .collect();

            div().children(children)
        })
        .class("sort-list")
        .on_mouse_down(move |e| {
            let len = state_md.get().items.len();
            let idx = ((e.local_y / LIST_STEP).floor() as usize).min(len.saturating_sub(1));
            state_md.update(|mut s| {
                s.drag_idx = Some(idx);
                s
            });
            swap_adj_md.set(0.0);
        })
        .on_drag(move |e| {
            let st = state_d.get();
            if let Some(current) = st.drag_idx {
                let adj = swap_adj_d.get();
                let visual_offset = e.drag_delta_y - adj;
                let len = st.items.len();

                // Hysteresis: require 60% of a cell to swap, preventing
                // oscillation at the boundary (swap → reverse swap loop).
                let cells = visual_offset / LIST_STEP;
                let slots = (cells - 0.1 * cells.signum()).round() as i32;
                if slots != 0 {
                    let target = (current as i32 + slots).clamp(0, len as i32 - 1) as usize;
                    if target != current {
                        swap_adj_d.set(adj + (target as f32 - current as f32) * LIST_STEP);
                        // Set visual offset BEFORE atomic items+idx update so the
                        // structural rebuild callback reads the correct offset.
                        drag_offset_d.set(visual_offset);
                        // Atomic update: items + drag_idx change together in one
                        // signal, preventing intermediate frames where drag_idx
                        // points at the wrong item in a half-reordered array.
                        state_d.update(|mut s| {
                            let r = s.items.remove(current);
                            s.items.insert(target, r);
                            s.drag_idx = Some(target);
                            s
                        });
                        return;
                    }
                }
                drag_offset_d.set(visual_offset);
            }
        })
        .on_drag_end(move |_e| {
            state_de.update(|mut s| {
                s.drag_idx = None;
                s
            });
            drag_offset_de.set(0.0);
            swap_adj_de.set(0.0);
        });

    div()
        .class("section")
        .child(
            text("Sortable List")
                .size(20.0)
                .weight(FontWeight::SemiBold)
                .color(Color::WHITE),
        )
        .child(
            text("Drag items up and down to reorder.")
                .size(14.0)
                .color(Color::rgba(1.0, 1.0, 1.0, 0.5)),
        )
        .child(sort_container)
}

// ============================================================================
// Section 2: Swipe to Delete
// ============================================================================

fn swipe_to_delete_section() -> Div {
    let blinc = BlincContextState::get();
    let items: State<Vec<ListItem>> = blinc.use_state_keyed("del_items", || make_items(6, "Task"));

    let items_s = items.clone();

    // Wrap in reactive stateful so list rebuilds when items are removed
    let delete_list = stateful_with_key::<PassiveFSM>("delete-list-container")
        .deps([items.signal_id()])
        .on_state(move |_ctx| {
            let items_val = items_s.get();
            let children: Vec<_> = items_val
                .iter()
                .map(|item| build_swipe_item(item, items_s.clone()))
                .collect();
            div().children(children)
        })
        .class("delete-list");

    div()
        .class("section")
        .child(
            text("Swipe to Delete")
                .size(20.0)
                .weight(FontWeight::SemiBold)
                .color(Color::WHITE),
        )
        .child(
            text("Drag items right to reveal delete. Release past the threshold to remove.")
                .size(14.0)
                .color(Color::rgba(1.0, 1.0, 1.0, 0.5)),
        )
        .child(delete_list)
}

fn build_swipe_item(item: &ListItem, items: State<Vec<ListItem>>) -> Stateful<SwipeFSM> {
    let blinc = BlincContextState::get();
    let swipe_x: State<f32> = blinc.use_state_keyed(&format!("swipe_{}", item.id), || 0.0);
    let removing: State<bool> = blinc.use_state_keyed(&format!("removing_{}", item.id), || false);

    let item_clone = item.clone();

    // Clones for on_state
    let swipe_x_s = swipe_x.clone();
    let removing_s = removing.clone();

    // Clones for on_drag
    let swipe_x_d = swipe_x.clone();

    // Clones for on_drag_end
    let swipe_x_de = swipe_x.clone();
    let removing_de = removing.clone();

    stateful_with_key::<SwipeFSM>(&format!("del-{}", item.id))
        .deps([swipe_x.signal_id(), removing.signal_id()])
        .on_state(move |ctx: &StateContext<SwipeFSM>| {
            let sx = swipe_x_s.get();
            let is_removing = removing_s.get();

            let spring_target = if is_removing { LIST_W + 50.0 } else { sx };
            let spring_val = ctx.use_spring(
                "spring",
                spring_target,
                blinc_animation::SpringConfig::snappy(),
            );

            // Remove item after slide-out animation completes
            if is_removing && spring_val > LIST_W {
                let item_id = item_clone.id;
                let current = items.get();
                if current.iter().any(|it| it.id == item_id) {
                    items.update(|mut v| {
                        v.retain(|it| it.id != item_id);
                        v
                    });
                }
            }

            // let opacity: f32 =
            //     (1.0 - (spring_val / DELETE_THRESHOLD).min(1.0) * 0.5).max(0.3);

            div().child(
                stack()
                    .w(LIST_W)
                    .h(LIST_ITEM_H)
                    .child(
                        div().class("delete-bg").child(
                            text("DELETE")
                                .size(14.0)
                                .weight(FontWeight::Bold)
                                .color(Color::WHITE),
                        ),
                    )
                    .child(
                        div()
                            .class("delete-card")
                            .bg(item_clone.color)
                            .transform(Transform::translate(spring_val, 0.0))
                            .child(
                                text("\u{2194}")
                                    .size(16.0)
                                    .color(Color::rgba(1.0, 1.0, 1.0, 0.5)),
                            )
                            .child(
                                text(&item_clone.label)
                                    .size(16.0)
                                    .weight(FontWeight::Medium)
                                    .color(Color::WHITE),
                            ),
                    ),
            )
        })
        .on_drag(move |e| {
            swipe_x_d.set(e.drag_delta_x.max(0.0));
        })
        .on_drag_end(move |e| {
            if e.drag_delta_x > DELETE_THRESHOLD {
                removing_de.set(true);
            } else {
                swipe_x_de.set(0.0);
            }
        })
}

// ============================================================================
// Section 3: Sortable Grid (3x3)
// ============================================================================

fn sortable_grid_section() -> Div {
    let blinc = BlincContextState::get();
    let state: State<SortGridState> = blinc.use_state_keyed("grid_state", || SortGridState {
        items: make_items(9, "#"),
        drag_idx: None,
    });
    let drag_ox: State<f32> = blinc.use_state_keyed("grid_drag_ox", || 0.0);
    let drag_oy: State<f32> = blinc.use_state_keyed("grid_drag_oy", || 0.0);
    let swap_ax: State<f32> = blinc.use_state_keyed("grid_swap_ax", || 0.0);
    let swap_ay: State<f32> = blinc.use_state_keyed("grid_swap_ay", || 0.0);

    // Clones for on_state
    let state_s = state.clone();
    let drag_ox_s = drag_ox.clone();
    let drag_oy_s = drag_oy.clone();

    // Clones for on_mouse_down
    let state_md = state.clone();
    let swap_ax_md = swap_ax.clone();
    let swap_ay_md = swap_ay.clone();

    // Clones for on_drag
    let state_d = state.clone();
    let drag_ox_d = drag_ox.clone();
    let drag_oy_d = drag_oy.clone();
    let swap_ax_d = swap_ax.clone();
    let swap_ay_d = swap_ay.clone();

    // Clones for on_drag_end
    let state_de = state.clone();
    let drag_ox_de = drag_ox.clone();
    let drag_oy_de = drag_oy.clone();
    let swap_ax_de = swap_ax.clone();
    let swap_ay_de = swap_ay.clone();

    let grid = stateful_with_key::<DragFSM>("grid-container")
        .deps([state.signal_id(), drag_ox.signal_id(), drag_oy.signal_id()])
        .on_state(move |_ctx| {
            let st = state_s.get();
            let ox = drag_ox_s.get();
            let oy = drag_oy_s.get();

            let children: Vec<Div> = st
                .items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let is_dragged = st.drag_idx == Some(i);
                    let mut d = div()
                        .id(format!("grid-{}", item.id))
                        .class("grid-item")
                        .bg(item.color)
                        .child(
                            text(&item.label)
                                .size(20.0)
                                .weight(FontWeight::Bold)
                                .color(Color::WHITE),
                        );

                    if is_dragged {
                        d = d
                            .transform(Transform::translate(ox, oy))
                            .z_index(100)
                            .opacity(0.85);
                    }
                    d
                })
                .collect();

            div().children(children)
        })
        .class("grid-container")
        .on_mouse_down(move |e| {
            let len = state_md.get().items.len();
            let col = (e.local_x / GRID_STEP).floor() as i32;
            let row = (e.local_y / GRID_STEP).floor() as i32;
            let idx = ((row * GRID_COLS + col).max(0) as usize).min(len.saturating_sub(1));
            state_md.update(|mut s| {
                s.drag_idx = Some(idx);
                s
            });
            swap_ax_md.set(0.0);
            swap_ay_md.set(0.0);
        })
        .on_drag(move |e| {
            let st = state_d.get();
            if let Some(current) = st.drag_idx {
                let ax = swap_ax_d.get();
                let ay = swap_ay_d.get();
                let vx = e.drag_delta_x - ax;
                let vy = e.drag_delta_y - ay;
                let len = st.items.len();

                let current_col = current as i32 % GRID_COLS;
                let current_row = current as i32 / GRID_COLS;
                // Hysteresis: require 60% of a cell (not 50%) to prevent
                // oscillation when the dragged item sits right at the boundary.
                let cx = vx / GRID_STEP;
                let cy = vy / GRID_STEP;
                let col_offset = (cx - 0.1 * cx.signum()).round() as i32;
                let row_offset = (cy - 0.1 * cy.signum()).round() as i32;

                if col_offset != 0 || row_offset != 0 {
                    let target_col = (current_col + col_offset).clamp(0, GRID_COLS - 1);
                    let target_row =
                        (current_row + row_offset).clamp(0, (len as i32 - 1) / GRID_COLS);
                    let target =
                        (target_row * GRID_COLS + target_col).clamp(0, len as i32 - 1) as usize;

                    if target != current {
                        let dx = (target as i32 % GRID_COLS - current as i32 % GRID_COLS) as f32
                            * GRID_STEP;
                        let dy = (target as i32 / GRID_COLS - current as i32 / GRID_COLS) as f32
                            * GRID_STEP;
                        swap_ax_d.set(ax + dx);
                        swap_ay_d.set(ay + dy);
                        // Set visual offsets BEFORE the atomic items+idx update so
                        // the structural rebuild callback reads correct offsets.
                        drag_ox_d.set(vx);
                        drag_oy_d.set(vy);
                        // Atomic update: items + drag_idx change together in one
                        // signal, preventing intermediate frames where drag_idx
                        // points at the wrong item in a half-reordered array.
                        state_d.update(|mut s| {
                            let r = s.items.remove(current);
                            s.items.insert(target, r);
                            s.drag_idx = Some(target);
                            s
                        });
                        return;
                    }
                }
                drag_ox_d.set(vx);
                drag_oy_d.set(vy);
            }
        })
        .on_drag_end(move |_e| {
            state_de.update(|mut s| {
                s.drag_idx = None;
                s
            });
            drag_ox_de.set(0.0);
            drag_oy_de.set(0.0);
            swap_ax_de.set(0.0);
            swap_ay_de.set(0.0);
        });

    div()
        .class("section")
        .child(
            text("Sortable Grid")
                .size(20.0)
                .weight(FontWeight::SemiBold)
                .color(Color::WHITE),
        )
        .child(
            text("Drag items in the 3\u{00d7}3 grid to reorder.")
                .size(14.0)
                .color(Color::rgba(1.0, 1.0, 1.0, 0.5)),
        )
        .child(grid)
}

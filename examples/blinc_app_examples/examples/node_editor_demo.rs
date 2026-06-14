//! Node-editor demo — pre-wired graph with three node types, typed
//! ports, a group, and live drag-to-connect.
//!
//! Run with:
//! ```
//! cargo run -p blinc_app_examples --example node_editor_demo --features node-editor
//! ```
//!
//! What it shows:
//!
//! - **Metadata-driven**: nodes render from declarative
//!   `NodeTemplate`s. The editor never hardcodes shape — adding a new
//!   template adds a new node type.
//! - **Generic over port kind**: hosts impl `PortKind` for their own
//!   port-type enum. Here, [`DemoPort`] models `Number` / `String` /
//!   `Boolean`; the editor delegates compatibility to
//!   `DemoPort::compatible_with`.
//! - **Theme-aware**: chrome (background, header, border, badge) pulls
//!   from `ThemeState`. Switch theme bundles to recolor; squircle
//!   profile, shadows, typography, and spacing tokens flow through.
//! - **Group with badge**: two nodes are wrapped in a group with a
//!   status badge in the header.
//! - **Drag-to-connect**: drag from an output port to an input port;
//!   the validator accepts compatible kinds.
//! - **Pan + zoom + selection**: inherited from `blinc_canvas_kit`.

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_canvas_kit::prelude::*;
use blinc_core::layer::{Color, Point};
use blinc_node_editor::prelude::*;
use blinc_node_editor::{BadgeKind, Group, GroupId, StatusBadge};
use blinc_portal_ui::Sense;
use blinc_tabler_icons::outline;
use blinc_theme::{
    detect_system_color_scheme, themes::universal::HybridTheme, tokens::ColorToken, ThemeState,
};
use std::sync::{Arc, RwLock};

// Resolve a theme colour at build time, falling back to a sane
// default when ThemeState isn't initialised (e.g. unit-test builds).
fn token(t: ColorToken, fallback: Color) -> Color {
    ThemeState::try_get().map(|s| s.color(t)).unwrap_or(fallback)
}

// ─── Host-side port type ───────────────────────────────────────────

/// The host's port kind. Three semantic types, each with its own
/// accent colour the editor reads via `PortKind::accent`. Real hosts
/// would model their domain types here — reflow's `PortType`, ML
/// tensor dtypes, audio frame formats, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum DemoPort {
    Number,
    String,
    Boolean,
}

impl PortKind for DemoPort {
    fn compatible_with(&self, other: &Self) -> bool {
        // Strict: types must match exactly. Real hosts might allow
        // Number → String coercion etc.
        self == other
    }

    fn label(&self) -> String {
        match self {
            DemoPort::Number => "Number".into(),
            DemoPort::String => "String".into(),
            DemoPort::Boolean => "Bool".into(),
        }
    }

    fn accent(&self) -> Color {
        match self {
            DemoPort::Number => Color::rgb(0.40, 0.75, 1.00),
            DemoPort::String => Color::rgb(1.00, 0.70, 0.40),
            DemoPort::Boolean => Color::rgb(0.55, 0.85, 0.55),
        }
    }
}

// ─── Templates ─────────────────────────────────────────────────────

/// Build a node icon from a Tabler outline path constant.
///
/// Builds the SVG markup inline rather than going through
/// `to_svg_colored` so we can lower the `stroke-width` from
/// Tabler's default 2.0. Tabler authors paths in a 24×24 viewBox;
/// at our 16×16 display size the viewBox scales by 16/24 ≈ 0.667,
/// so a 2.0 stroke renders at ~1.33 CSS px — falls between pixel
/// boundaries and AA has to smear the edge across two rows, making
/// strokes read as thick + soft. Stroke `1.5` resolves to a clean
/// 1.0 CSS px (2 physical px on retina) which the rasterizer can
/// align to a pixel grid.
fn tabler_icon(path_data: &str) -> NodeIcon {
    let colour_hex = token_hex(ColorToken::TextPrimary, "#e8e8e8");
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="{colour_hex}" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">{path_data}</svg>"#
    );
    NodeIcon::from_svg_str(&svg).expect("valid SVG")
}

/// Resolve a theme colour to a `#rrggbb` hex string. Tabler's
/// `to_svg_colored` only accepts strings.
fn token_hex(t: ColorToken, fallback: &str) -> String {
    let Some(c) = ThemeState::try_get().map(|s| s.color(t)) else {
        return fallback.to_string();
    };
    let to_byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", to_byte(c.r), to_byte(c.g), to_byte(c.b))
}

/// Shared signals the portal widgets read + write. Same `Signal<T>`
/// reaches both the `filter` node's content slider AND the `formatter`
/// node's running toggle / label, demonstrating cross-portal reactive
/// state: setting either from outside the canvas (e.g. a sidebar
/// `cn::input`) repaints the node body immediately.
fn signals() -> &'static PortalSignals {
    use std::sync::OnceLock;
    static S: OnceLock<PortalSignals> = OnceLock::new();
    S.get_or_init(|| PortalSignals {
        threshold: blinc_core::reactive::signal::<f32>(0.5),
        running: blinc_core::reactive::signal::<bool>(false),
    })
}

struct PortalSignals {
    threshold: blinc_core::reactive::Signal<f32>,
    running: blinc_core::reactive::Signal<bool>,
}

fn build_templates() -> Vec<NodeTemplate<DemoPort>> {
    let source = NodeTemplate::<DemoPort>::new("source", "Source")
        .with_category("data")
        .with_subtitle("Emit values")
        .with_icon(tabler_icon(outline::DATABASE))
        .with_output(
            PortDesc::new("out_num", "value", Direction::Output, DemoPort::Number)
                .with_description("Stream of sampled numeric readings"),
        );

    let filter = NodeTemplate::<DemoPort>::new("filter", "Filter")
        .with_category("transform")
        .with_subtitle("Threshold gate")
        .with_icon(tabler_icon(outline::FILTER))
        .with_input(
            PortDesc::new("in_num", "input", Direction::Input, DemoPort::Number)
                .with_description("Value to test against the threshold"),
        )
        .with_input(
            PortDesc::new(
                "in_threshold",
                "threshold",
                Direction::Input,
                DemoPort::Number,
            )
            .with_description("Numeric cutoff — values above pass through"),
        )
        .with_output(
            PortDesc::new("out_pass", "pass?", Direction::Output, DemoPort::Boolean)
                .with_description("true when input >= threshold"),
        )
        // Portal content slot — 80px of immediate-mode UI under the
        // header. The slider edits the shared `threshold` signal;
        // any frame mutating it from anywhere repaints the canvas
        // via the portal-ui notifier hook.
        .with_content(80.0, |_node_id, ui| {
            let sigs = signals();
            ui.label(&format!("threshold = {:.2}", sigs.threshold.get()));
            ui.slider_signal(&sigs.threshold, 0.0..1.0);
        });

    let formatter = NodeTemplate::<DemoPort>::new("formatter", "Formatter")
        .with_category("transform")
        .with_subtitle("Number → String")
        .with_icon(tabler_icon(outline::TYPOGRAPHY))
        .with_input(
            PortDesc::new("in_num", "value", Direction::Input, DemoPort::Number)
                .with_description("Numeric value to format"),
        )
        .with_output(
            PortDesc::new("out_str", "text", Direction::Output, DemoPort::String)
                .with_description("Stringified value ready for display"),
        )
        // Portal content slot — proves cross-portal reactive state.
        // The label_signal here re-renders whenever the FILTER
        // node's slider edits `threshold`, even though that slider
        // lives in a different portal. Same signal, two readers.
        // The switch toggles a separate `running` signal; the
        // label below tracks the toggle.
        .with_content(110.0, |_node_id, ui| {
            let sigs = signals();
            ui.label(&format!("Mirrors threshold: {:.2}", sigs.threshold.get()));
            ui.horizontal(|ui| {
                ui.label("running");
                ui.switch_signal(&sigs.running);
            });
            ui.label(if sigs.running.get() { "● live" } else { "○ paused" });

            // Free-form painting — a moving sparkline driven by
            // the portal's monotonic clock. Proves
            // `allocate_painter` + per-frame animation. Sets
            // `request_animation` so the canvas keeps painting.
            let color = ui.style().accent;
            let (mut p, _) = ui.allocate_painter((180.0, 28.0), Sense::Hover);
            let t = p.time();
            let rect = p.rect();
            let n = 32;
            let mut path = blinc_core::draw::Path::new();
            for i in 0..n {
                let x = rect.x() + (i as f32 / (n - 1) as f32) * rect.width();
                let phase = t * 1.5 + i as f32 * 0.3;
                let y = rect.y() + rect.height() * (0.5 - phase.sin() * 0.35);
                if i == 0 {
                    path = path.move_to(x, y);
                } else {
                    path = path.line_to(x, y);
                }
            }
            use blinc_core::layer::Brush;
            let stroke = blinc_core::draw::Stroke::new(1.5);
            p.stroke_path(&path, &stroke, Brush::Solid(color));
            drop(p);
            ui.request_animation();
        });

    let sink = NodeTemplate::<DemoPort>::new("sink", "Sink")
        .with_category("data")
        .with_subtitle("Display")
        .with_icon(tabler_icon(outline::DEVICE_DESKTOP))
        .with_input(
            PortDesc::new("in_pass", "gate", Direction::Input, DemoPort::Boolean)
                .with_description("When false, the message is dropped"),
        )
        .with_input(
            PortDesc::new("in_str", "label", Direction::Input, DemoPort::String)
                .with_description("Text payload to render in the sink view"),
        );

    vec![source, filter, formatter, sink]
}

// ─── Initial graph ─────────────────────────────────────────────────

type Editor = NodeEditor<DemoPort, (), (), ()>;

fn initial_nodes() -> Vec<NodeInstance<()>> {
    vec![
        NodeInstance::new("src/1", "source", Point::new(80.0, 120.0))
            .with_size(180.0, 80.0)
            .with_badge(StatusBadge::success()),
        // `with_disabled(true)` demonstrates Tier 4.7 — the
        // renderer dims the body / icon / title via
        // `theme.node_disabled_alpha()` and downgrades every
        // incident edge (only `src/threshold → filter/in_threshold`
        // here) to the faded `Pending` style. Press `D` while a
        // node is selected to toggle the flag at runtime.
        NodeInstance::new("src/threshold", "source", Point::new(80.0, 280.0))
            .with_subtitle("Threshold const")
            .with_size(180.0, 80.0)
            .with_disabled(true),
        NodeInstance::new("filter/1", "filter", Point::new(360.0, 180.0))
            .with_size(200.0, 100.0)
            .with_badge(StatusBadge::running()),
        NodeInstance::new("fmt/1", "formatter", Point::new(360.0, 360.0))
            .with_size(200.0, 80.0),
        NodeInstance::new("sink/1", "sink", Point::new(660.0, 240.0))
            .with_size(200.0, 100.0)
            .with_badge(StatusBadge::info(3).with_tooltip("3 pending writes")),
    ]
}

fn initial_connections() -> Vec<Connection<()>> {
    vec![
        Connection::new(
            PortAddress::new("src/1".into(), "out_num"),
            PortAddress::new("filter/1".into(), "in_num"),
        )
        .with_state(ConnectionState::Running),
        Connection::new(
            PortAddress::new("src/threshold".into(), "out_num"),
            PortAddress::new("filter/1".into(), "in_threshold"),
        ),
        Connection::new(
            PortAddress::new("src/1".into(), "out_num"),
            PortAddress::new("fmt/1".into(), "in_num"),
        ),
        Connection::new(
            PortAddress::new("filter/1".into(), "out_pass"),
            PortAddress::new("sink/1".into(), "in_pass"),
        )
        .with_state(ConnectionState::Success),
        Connection::new(
            PortAddress::new("fmt/1".into(), "out_str"),
            PortAddress::new("sink/1".into(), "in_str"),
        ),
    ]
}

fn initial_groups() -> Vec<Group<()>> {
    vec![Group::<()>::new(GroupId::from("transforms"), "Transforms")
        .with_description("Filter + formatter")
        .with_description_placeholder("Enter a description")
        .add_member("filter/1")
        .add_member("fmt/1")
        .with_badge(
            StatusBadge {
                kind: BadgeKind::Running,
                count: Some(2),
                tooltip: Some("2 active operations".into()),
            },
        )]
}

// ─── Editor wiring ─────────────────────────────────────────────────

/// Host-side graph state. The editor stays a pure view; we hold the
/// authoritative copy of nodes / connections / groups here and
/// react to [`EditorEvent`]s by patching this state, then
/// re-syncing via granular commands.
#[derive(Clone, Default)]
struct HostGraph {
    nodes: Arc<RwLock<Vec<NodeInstance<()>>>>,
    connections: Arc<RwLock<Vec<Connection<()>>>>,
    groups: Arc<RwLock<Vec<Group<()>>>>,
}

fn build_editor() -> (Editor, HostGraph) {
    let editor: Editor = NodeEditor::new("node-editor-demo")
        .with_templates(build_templates())
        .with_background(
            // Denser, slightly chunkier dot grid: 28 px spacing (down
            // from the default 50) + 3 px dot size (up from the
            // default 2) reads as a fine workshop surface texture
            // without competing with the node chrome. Zoom-adaptive
            // still kicks in below 0.3× to drop every 5th dot for
            // legibility when fully zoomed out.
            CanvasBackground::dots(token(ColorToken::Border, Color::rgba(0.5, 0.5, 0.55, 0.65)))
                .with_spacing(28.0)
                .with_size(3.0)
                .with_zoom_adaptive(0.3, 5),
        );

    let host = HostGraph {
        nodes: Arc::new(RwLock::new(initial_nodes())),
        connections: Arc::new(RwLock::new(initial_connections())),
        groups: Arc::new(RwLock::new(initial_groups())),
    };

    // Initial sync.
    editor.set_graph(
        host.nodes.read().unwrap().clone(),
        host.connections.read().unwrap().clone(),
        host.groups.read().unwrap().clone(),
        Vec::new(),
    );

    // Validator — accept connections whose port kinds match. This is
    // the ONLY callback surface: validators answer mid-drag questions
    // the editor needs synchronously (preview-tint colour).
    let editor = editor.on_connect_request(|req| {
        if req.from_kind.compatible_with(req.to_kind) {
            ValidationOutcome::Accept
        } else {
            ValidationOutcome::Reject {
                reason: format!(
                    "kind mismatch: {} → {}",
                    req.from_kind.label(),
                    req.to_kind.label()
                ),
            }
        }
    });

    (editor, host)
}

/// Open a floating mini-toolbar at `anchor_screen` with Group +
/// Delete icon buttons targeting `node_ids`. Matches cn::popover's
/// surface design (SurfaceElevated bg, 1px border, theme radius,
/// shadow_lg, theme padding). Click-outside auto-dismisses via the
/// click_outside registry; clicking an action runs it then closes
/// the overlay.
fn open_multi_select_toolbar(
    editor: &Editor,
    host: &HostGraph,
    node_ids: Vec<NodeId>,
    anchor_screen: Point,
) {
    // Suppress the Group action when any selected node is already a
    // member of an existing group — re-grouping already-grouped
    // nodes would create overlapping memberships the editor doesn't
    // currently model. Delete stays available regardless.
    let can_group = {
        let groups = host.groups.read().unwrap();
        !node_ids.iter().any(|id| {
            groups
                .iter()
                .any(|g| g.members.iter().any(|m| m == id))
        })
    };
    use blinc_layout::click_outside;
    use blinc_layout::overlay_state::overlay_stack;
    use blinc_layout::widgets::overlay_stack::{OverlayBuilder, OverlayHandle};
    use std::sync::Mutex;

    let theme = ThemeState::get();
    let bg = theme.color(ColorToken::SurfaceElevated);
    let border = theme.color(ColorToken::Border);
    let radius = theme.radius(blinc_theme::tokens::RadiusToken::Lg);
    let padding = theme.spacing_value(blinc_theme::tokens::SpacingToken::Space2);

    // Reserve the overlay id so the click-outside registry can
    // bind to the content element's stable id BEFORE `show()`
    // returns. Mirrors cn::popover's pattern.
    let next_handle_id = overlay_stack()
        .lock()
        .ok()
        .map(|s| s.peek_next_handle_id())
        .unwrap_or(0);
    let toolbar_id = format!("ne-multi-toolbar-{next_handle_id}");
    let click_outside_key = format!("ne-multi-toolbar:{next_handle_id}");

    // Slot shared between the overlay's content closure and the
    // outer scope so action buttons can close the overlay via the
    // captured handle after `show()` returns.
    let handle_slot: Arc<Mutex<Option<OverlayHandle>>> = Arc::new(Mutex::new(None));

    let toolbar_id_for_content = toolbar_id.clone();
    let click_outside_key_for_close = click_outside_key.clone();

    let editor_for_group = editor.clone();
    let host_for_group = host.clone();
    let ids_for_group = node_ids.clone();
    let editor_for_delete = editor.clone();
    let host_for_delete = host.clone();
    let ids_for_delete = node_ids.clone();
    let editor_for_align = editor.clone();
    let ids_for_align = node_ids.clone();
    let slot_for_group = handle_slot.clone();
    let slot_for_delete = handle_slot.clone();
    // Distribute is only meaningful for 3+ nodes (two anchors + at
    // least one interior). Below that we keep the buttons rendered
    // but disabled so the layout stays stable between selections.
    let can_distribute = node_ids.len() >= 3;

    // Clamp the anchor so the whole toolbar stays inside the window.
    // The overlay system positions `AtPoint` overlays as absolute
    // `(left, top)` and doesn't auto-fit to the viewport — past the
    // window edge they clip off-screen. We don't know the exact
    // toolbar size until it lays out, so we estimate from the button
    // roster (icon-only ghost ≈ 32 px wide + 6 px gap, plus three
    // separators + outer padding). 480 × 60 covers the worst case
    // (with-Group + full align + full distribute + Delete) plus a
    // small margin. The 8 px window inset keeps the toolbar from
    // hugging the screen edge.
    let (window_w, window_h) = overlay_stack()
        .lock()
        .ok()
        .map(|s| s.viewport())
        .unwrap_or((f32::INFINITY, f32::INFINITY));
    let est_w = 480.0_f32;
    let est_h = 60.0_f32;
    let inset = 8.0_f32;
    let clamped_x = anchor_screen
        .x
        .clamp(inset, (window_w - est_w - inset).max(inset));
    let clamped_y = anchor_screen
        .y
        .clamp(inset, (window_h - est_h - inset).max(inset));

    let handle = OverlayBuilder::popover()
        .at(clamped_x, clamped_y)
        .on_close(move |_reason| {
            click_outside::unregister_click_outside(&click_outside_key_for_close);
        })
        .content(move || {
            let editor_g = editor_for_group.clone();
            let host_g = host_for_group.clone();
            let group_ids = ids_for_group.clone();
            let slot_g = slot_for_group.clone();
            let editor_d = editor_for_delete.clone();
            let host_d = host_for_delete.clone();
            let delete_ids = ids_for_delete.clone();
            let slot_d = slot_for_delete.clone();
            let editor_a = editor_for_align.clone();
            let ids_a = ids_for_align.clone();
            let error_color = theme.color(ColorToken::Error);

            // Builds a tooltip-wrapped icon-only ghost button bound
            // to either an align edge or a distribute axis.
            // Align + distribute buttons leave the popover open so
            // users can chain operations (canonical workflow:
            // align → distribute on the perpendicular axis); Group
            // + Delete dismiss as before.
            //
            // Uses `InstanceKey::explicit` keyed by the unique
            // label so each tooltip's stored overlay-handle state
            // survives popover re-renders. The default
            // `track_caller` key auto-indexes calls within a frame
            // (`mk_align(...)` invoked six times from the same
            // source line gets indices 0..5), and the indices can
            // drift across re-renders inside an overlay content
            // closure — drift orphans the stored handle and leaves
            // the tooltip unable to dismiss when hover-leaves.
            let mk_align = |edge: AlignEdge, icon: &'static str, label: &'static str| {
                let editor = editor_a.clone();
                let ids = ids_a.clone();
                let key = blinc_layout::key::InstanceKey::explicit(format!(
                    "ne-multi-toolbar-tooltip:{label}"
                ));
                let trigger = move || {
                    let editor = editor.clone();
                    let ids = ids.clone();
                    div().child(
                        blinc_cn::button("")
                            .variant(blinc_cn::ButtonVariant::Ghost)
                            .icon(icon)
                            .on_click(move |_| {
                                editor.align_nodes(&ids, edge);
                                tracing::info!(
                                    "align {:?} on {} nodes",
                                    edge,
                                    ids.len()
                                );
                            }),
                    )
                };
                blinc_cn::TooltipBuilder::with_key(trigger, key).text(label)
            };
            let mk_distribute =
                |axis: DistributeAxis, icon: &'static str, label: &'static str| {
                    let editor = editor_a.clone();
                    let ids = ids_a.clone();
                    let key = blinc_layout::key::InstanceKey::explicit(format!(
                        "ne-multi-toolbar-tooltip:{label}"
                    ));
                    let trigger = move || {
                        let editor = editor.clone();
                        let ids = ids.clone();
                        div().child(
                            blinc_cn::button("")
                                .variant(blinc_cn::ButtonVariant::Ghost)
                                .icon(icon)
                                .disabled(!can_distribute)
                                .on_click(move |_| {
                                    editor.distribute_nodes(&ids, axis);
                                    tracing::info!(
                                        "distribute {:?} on {} nodes",
                                        axis,
                                        ids.len()
                                    );
                                }),
                        )
                    };
                    blinc_cn::TooltipBuilder::with_key(trigger, key).text(label)
                };

            let mut row = div()
                .id(&toolbar_id_for_content)
                .flex_row()
                .items_center()
                .gap(6.0)
                .p_px(padding)
                .bg(bg)
                .border(1.0, border)
                .rounded(radius)
                .shadow_lg();
            if can_group {
                let group_key = blinc_layout::key::InstanceKey::explicit(
                    "ne-multi-toolbar-tooltip:group",
                );
                let group_trigger = move || {
                    let editor_g = editor_g.clone();
                    let host_g = host_g.clone();
                    let group_ids = group_ids.clone();
                    let slot_g = slot_g.clone();
                    div().child(
                        blinc_cn::button("")
                            .variant(blinc_cn::ButtonVariant::Ghost)
                            .icon(blinc_cn::prelude::icons::GROUP)
                            .on_click(move |_| {
                                    let new_id = GroupId::from(format!(
                                        "g-{}",
                                        web_time::SystemTime::now()
                                            .duration_since(web_time::UNIX_EPOCH)
                                            .map(|d| d.as_millis())
                                            .unwrap_or(0)
                                    ));
                                    let mut group =
                                        Group::<()>::new(new_id.clone(), "New Group")
                                            .with_description(
                                                "Group created from multi-select",
                                            )
                                            .with_description_placeholder(
                                                "Enter a description",
                                            );
                                    for nid in &group_ids {
                                        group = group.add_member(nid.as_str());
                                    }
                                    host_g.groups.write().unwrap().push(group.clone());
                                    editor_g.insert_group(group);
                                    editor_g.clear_selection();
                                    if let Some(h) = slot_g.lock().unwrap().as_ref() {
                                        h.close();
                                    }
                                    tracing::info!(
                                        "grouped {} nodes into {}",
                                        group_ids.len(),
                                        new_id.as_str()
                                    );
                                }),
                        )
                };
                row = row.child(
                    blinc_cn::TooltipBuilder::with_key(group_trigger, group_key)
                        .text("Group selection"),
                );
                row = row.child(blinc_cn::separator().vertical());
            }

            // Align group — six icon-only ghost buttons, matching
            // the standard design-tool layout (horizontal axis:
            // left / centre-x / right; vertical: top / middle / bottom).
            // Tabler's `LAYOUT_ALIGN_*` glyph family reads as align
            // affordances at every size.
            row = row
                .child(mk_align(AlignEdge::Left, outline::LAYOUT_ALIGN_LEFT, "Align left"))
                .child(mk_align(
                    AlignEdge::CenterX,
                    outline::LAYOUT_ALIGN_CENTER,
                    "Align centre (horizontal)",
                ))
                .child(mk_align(AlignEdge::Right, outline::LAYOUT_ALIGN_RIGHT, "Align right"))
                .child(mk_align(AlignEdge::Top, outline::LAYOUT_ALIGN_TOP, "Align top"))
                .child(mk_align(
                    AlignEdge::CenterY,
                    outline::LAYOUT_ALIGN_MIDDLE,
                    "Align middle (vertical)",
                ))
                .child(mk_align(AlignEdge::Bottom, outline::LAYOUT_ALIGN_BOTTOM, "Align bottom"))
                .child(blinc_cn::separator().vertical())
                .child(mk_distribute(
                    DistributeAxis::Horizontal,
                    outline::LAYOUT_DISTRIBUTE_HORIZONTAL,
                    "Distribute horizontally",
                ))
                .child(mk_distribute(
                    DistributeAxis::Vertical,
                    outline::LAYOUT_DISTRIBUTE_VERTICAL,
                    "Distribute vertically",
                ))
                .child(blinc_cn::separator().vertical());

            let delete_key = blinc_layout::key::InstanceKey::explicit(
                "ne-multi-toolbar-tooltip:delete",
            );
            let delete_trigger = move || {
                let host_d = host_d.clone();
                let editor_d = editor_d.clone();
                let delete_ids = delete_ids.clone();
                let slot_d = slot_d.clone();
                div().child(
                    blinc_cn::button("")
                        .variant(blinc_cn::ButtonVariant::Ghost)
                        .icon(blinc_cn::prelude::icons::TRASH)
                        .color(error_color)
                        .on_click(move |_| {
                            let id_set: std::collections::HashSet<_> =
                                delete_ids.iter().cloned().collect();
                            host_d
                                .nodes
                                .write()
                                .unwrap()
                                .retain(|n| !id_set.contains(&n.id));
                            host_d.connections.write().unwrap().retain(|c| {
                                !id_set.contains(&c.from.node)
                                    && !id_set.contains(&c.to.node)
                            });
                            for g in host_d.groups.write().unwrap().iter_mut() {
                                g.members.retain(|m| !id_set.contains(m));
                            }
                            for id in &delete_ids {
                                editor_d.remove_node(id);
                            }
                            editor_d.clear_selection();
                            if let Some(h) = slot_d.lock().unwrap().as_ref() {
                                h.close();
                            }
                            tracing::info!(
                                "deleted {} nodes via multi-select toolbar",
                                delete_ids.len()
                            );
                        }),
                )
            };
            row.child(
                blinc_cn::TooltipBuilder::with_key(delete_trigger, delete_key)
                    .text("Delete selection"),
            )
        })
        .show();

    // Stash the handle so action callbacks can close it.
    *handle_slot.lock().unwrap() = Some(handle);

    // Register click-outside dismissal — overlay closes the moment
    // the next mouse-down lands outside any element with id
    // == toolbar_id (the chrome rect). Mirrors cn::popover's
    // pattern; without this only Escape would dismiss.
    let handle_for_outside = handle;
    click_outside::register_click_outside(
        &click_outside_key,
        &toolbar_id,
        move || {
            handle_for_outside.close();
        },
    );
}

/// React to one [`EditorEvent`] by patching `host` and pushing the
/// matching granular command back at the editor. Centralises the
/// host-as-driver flow described in the roadmap.
fn handle_event(editor: &Editor, host: &HostGraph, evt: EditorEvent<DemoPort>) {
    match evt {
        EditorEvent::ConnectionAccepted(c) => {
            let conn = Connection::new(c.from.clone(), c.to.clone());
            host.connections.write().unwrap().push(conn.clone());
            editor.insert_connection(conn);
            tracing::info!(
                "connected {:?} -> {:?}",
                (c.from.node.as_str(), c.from.port.as_str()),
                (c.to.node.as_str(), c.to.port.as_str()),
            );
        }
        EditorEvent::NodeDragged { id, position } => {
            if let Some(n) = host.nodes.write().unwrap().iter_mut().find(|n| n.id == id) {
                n.position = position;
            }
            // Editor's drag handler already updated its internal copy
            // mid-drag — no need to re-push here. Host state is now in
            // sync for the next save/snapshot.
        }
        EditorEvent::DeleteConnectionRequested(id) => {
            // Confirm via a cn alert-dialog before pulling the edge
            // out of host state. Clones move into the on_confirm
            // closure so the dialog can fire its callback after the
            // event-handling call returns.
            let editor_for_confirm = editor.clone();
            let host_for_confirm = host.clone();
            blinc_cn::dialog()
                .title("Delete connection?")
                .description("This will remove the edge from the graph. The connected nodes stay in place.")
                .confirm_text("Delete")
                .cancel_text("Cancel")
                .confirm_destructive(true)
                .on_confirm(move || {
                    host_for_confirm.connections.write().unwrap().retain(|c| c.id != id);
                    editor_for_confirm.remove_connection(id);
                    tracing::info!("deleted connection {}", id.0);
                })
                .show();
        }
        EditorEvent::DeleteNodesRequested(ids) => {
            if ids.is_empty() {
                return;
            }
            let (title, description) = if ids.len() == 1 {
                (
                    "Delete node?".to_string(),
                    "This will remove the node and every connection attached to it."
                        .to_string(),
                )
            } else {
                (
                    format!("Delete {} nodes?", ids.len()),
                    "This will remove the selected nodes and every connection attached to them."
                        .to_string(),
                )
            };
            let editor_for_confirm = editor.clone();
            let host_for_confirm = host.clone();
            let ids_for_confirm = ids.clone();
            blinc_cn::dialog()
                .title(title)
                .description(description)
                .confirm_text("Delete")
                .cancel_text("Cancel")
                .confirm_destructive(true)
                .on_confirm(move || {
                    let id_set: std::collections::HashSet<_> =
                        ids_for_confirm.iter().cloned().collect();
                    host_for_confirm.nodes.write().unwrap().retain(|n| !id_set.contains(&n.id));
                    host_for_confirm
                        .connections
                        .write()
                        .unwrap()
                        .retain(|c| !id_set.contains(&c.from.node) && !id_set.contains(&c.to.node));
                    for g in host_for_confirm.groups.write().unwrap().iter_mut() {
                        g.members.retain(|m| !id_set.contains(m));
                    }
                    for id in &ids_for_confirm {
                        editor_for_confirm.remove_node(id);
                    }
                    tracing::info!("deleted {} node(s)", ids_for_confirm.len());
                })
                .show();
        }
        EditorEvent::AddToGroupRequested(req) => {
            // Add the node to the target group's member list +
            // mirror the change into the editor via the granular
            // `set_group_members` command.
            let updated = {
                let mut groups = host.groups.write().unwrap();
                groups
                    .iter_mut()
                    .find(|g| g.id == req.group)
                    .map(|g| {
                        if !g.members.contains(&req.node) {
                            g.members.push(req.node.clone());
                        }
                        g.members.clone()
                    })
            };
            if let Some(members) = updated {
                editor.set_group_members(&req.group, members);
                tracing::info!(
                    "added {} to group {}",
                    req.node.as_str(),
                    req.group.as_str()
                );
            }
        }
        EditorEvent::RemoveFromGroupRequested(req) => {
            let updated = {
                let mut groups = host.groups.write().unwrap();
                groups
                    .iter_mut()
                    .find(|g| g.id == req.group)
                    .map(|g| {
                        g.members.retain(|m| m != &req.node);
                        g.members.clone()
                    })
            };
            if let Some(members) = updated {
                editor.set_group_members(&req.group, members);
                tracing::info!(
                    "removed {} from group {} ({:?})",
                    req.node.as_str(),
                    req.group.as_str(),
                    req.source,
                );
            }
        }
        EditorEvent::ToggleCollapseRequested(req) => {
            // Apply directly — collapse/expand is a benign visual
            // toggle, no destructive intent. Host mirrors via the
            // editor's granular command so the renderer's slot
            // cache invalidates.
            if let Some(g) = host
                .groups
                .write()
                .unwrap()
                .iter_mut()
                .find(|g| g.id == req.group)
            {
                g.is_collapsed = req.collapsed;
            }
            editor.set_group_collapsed(&req.group, req.collapsed);
        }
        EditorEvent::DeleteGroupRequested(req) => {
            let editor_for_confirm = editor.clone();
            let host_for_confirm = host.clone();
            let group_id = req.group.clone();
            let name = host
                .groups
                .read()
                .unwrap()
                .iter()
                .find(|g| g.id == group_id)
                .map(|g| g.name.clone())
                .unwrap_or_else(|| group_id.as_str().to_string());
            blinc_cn::dialog()
                .title(format!("Delete group \"{name}\"?"))
                .description("The grouping is removed; member nodes and connections stay.")
                .confirm_text("Delete")
                .cancel_text("Cancel")
                .confirm_destructive(true)
                .on_confirm(move || {
                    host_for_confirm
                        .groups
                        .write()
                        .unwrap()
                        .retain(|g| g.id != group_id);
                    editor_for_confirm.remove_group(&group_id);
                    tracing::info!("deleted group {}", group_id.as_str());
                })
                .show();
        }
        EditorEvent::MultiSelectionSettled {
            node_ids,
            anchor_screen,
        } => {
            open_multi_select_toolbar(editor, host, node_ids, anchor_screen);
        }
        EditorEvent::SelectionCleared => {
            // Demo doesn't need to do anything — click-outside on
            // the overlay already dismisses it. Real hosts might
            // close inspector panels or update breadcrumbs here.
        }
        EditorEvent::EditGroupTitleRequested { group, current, anchor_screen } => {
            open_inline_text_editor(
                editor,
                host,
                group,
                current,
                anchor_screen,
                EditorField::Title,
            );
        }
        EditorEvent::EditGroupDescriptionRequested { group, current, anchor_screen } => {
            open_inline_text_editor(
                editor,
                host,
                group,
                current,
                anchor_screen,
                EditorField::Description,
            );
        }
        EditorEvent::ConnectionRejected { from, to, reason } => {
            // Pop a cn::toast banner with the validator's reason
            // (when supplied) so the user sees WHY the connection
            // didn't take instead of just the red preview line that
            // disappears on release. Falls back to a generic message
            // when the host's `on_connect_request` returned a
            // reason-less rejection.
            let description = if reason.trim().is_empty() {
                format!(
                    "{}:{} → {}:{}: incompatible port types",
                    from.node.as_str(),
                    from.port.as_str(),
                    to.node.as_str(),
                    to.port.as_str(),
                )
            } else {
                format!(
                    "{}:{} → {}:{}: {}",
                    from.node.as_str(),
                    from.port.as_str(),
                    to.node.as_str(),
                    to.port.as_str(),
                    reason,
                )
            };
            blinc_cn::toast_error("Connection rejected")
                .description(description)
                .duration_ms(5000)
                .show();
        }
        EditorEvent::CreateGroupRequested(_)
        | EditorEvent::EdgeClicked { .. }
        | EditorEvent::NodeClicked { .. }
        | EditorEvent::LayoutApplied(_) => {
            // Unhandled in this demo; real hosts dispatch to their
            // command palette / inspector / layout code.
        }
    }
}

/// Which group field a double-click is editing. Drives the editor
/// widget choice (single-line `cn::input` vs multi-line
/// `cn::textarea`) AND the save callback's field write.
#[derive(Clone, Copy)]
enum EditorField {
    Title,
    Description,
}

/// Open a floating inline editor anchored at the group field's
/// screen rect. cn::input for `Title`, cn::textarea for
/// `Description`. Enter (no shift) commits + closes; Esc closes
/// without saving; Shift+Enter inserts a newline inside the
/// textarea (default cn::textarea behaviour — we don't intercept
/// it). The save callback patches the host's `groups` Vec + calls
/// `editor.insert_group` to re-sync.
fn open_inline_text_editor(
    editor: &Editor,
    host: &HostGraph,
    group: GroupId,
    current: String,
    anchor_screen: blinc_core::layer::Rect,
    field: EditorField,
) {
    use blinc_core::context_state::BlincContextState;
    use blinc_layout::click_outside;
    use blinc_layout::overlay_state::overlay_stack;
    use blinc_layout::widgets::overlay_stack::{OverlayBuilder, OverlayHandle};
    use std::sync::Mutex;

    let theme = ThemeState::get();
    // Width-only sizing knobs — outer chrome is now provided by
    // the cn::input / cn::textarea widget itself (bg, border,
    // shadow). We only need the spacing token for our id'd
    // wrapper's padding allowance.
    let _ = (theme.color(ColorToken::SurfaceElevated), theme.color(ColorToken::Border));
    let padding = theme.spacing_value(blinc_theme::tokens::SpacingToken::Space2);

    // Estimate the popover footprint for window-bounds clamping
    // (same approach as the multi-select toolbar). Width matches
    // the anchor rect with a sensible minimum so a short title
    // doesn't collapse the editor to a sliver. Height differs by
    // field — textarea defaults taller.
    let estimated_w = anchor_screen.width().max(220.0) + padding * 2.0;
    let estimated_h = match field {
        EditorField::Title => 48.0,
        EditorField::Description => 110.0,
    };
    let (window_w, window_h) = overlay_stack()
        .lock()
        .ok()
        .map(|s| s.viewport())
        .unwrap_or((f32::INFINITY, f32::INFINITY));
    let inset = 8.0_f32;
    let clamped_x = anchor_screen
        .x()
        .clamp(inset, (window_w - estimated_w - inset).max(inset));
    let clamped_y = anchor_screen
        .y()
        .clamp(inset, (window_h - estimated_h - inset).max(inset));

    // Reserve the overlay id BEFORE `.show()` so the click-outside
    // registry can bind to the editor div's stable id immediately.
    let next_handle_id = overlay_stack()
        .lock()
        .ok()
        .map(|s| s.peek_next_handle_id())
        .unwrap_or(0);
    let editor_id = format!("ne-inline-text-{next_handle_id}");
    let click_outside_key = format!("ne-inline-text:{next_handle_id}");

    // Save / cancel both flow through the captured `OverlayHandle`
    // stored here. `show()` returns the handle after the overlay
    // is mounted; we stash it into this slot so the keyboard
    // shortcuts can close it.
    let handle_slot: Arc<Mutex<Option<OverlayHandle>>> = Arc::new(Mutex::new(None));

    // Per-editor shared state. cn::input + cn::textarea both want
    // their backing state cell created up-front so it survives the
    // overlay's re-renders. We prefill with `current` so the user
    // can edit in place rather than re-typing.
    let input_data = match field {
        EditorField::Title => {
            let data = blinc_layout::widgets::text_input::text_input_data();
            data.lock().unwrap().value = current.clone();
            Some(data)
        }
        EditorField::Description => None,
    };
    let area_state = match field {
        EditorField::Title => None,
        EditorField::Description => {
            let state = blinc_layout::widgets::text_area::text_area_state();
            {
                let mut s = state.lock().unwrap();
                s.lines = if current.is_empty() {
                    vec![String::new()]
                } else {
                    current.split('\n').map(String::from).collect()
                };
            }
            Some(state)
        }
    };

    // Auto-focus via the DEFERRED path. The synchronous
    // focus_text_input call we tried previously runs side-effects
    // (notify_continuous_redraw → wakes the scheduler) BEFORE the
    // popover content has been added to the tree, which on canvas-
    // backed hosts produced a ~4s "canvas broken / zoomed-out"
    // state until mouse-move. The deferred variants enqueue the
    // focus request; the windowed frame loop's pending-focus
    // drain (called after rebuild_overlay_subtree_if_dirty)
    // applies it on the frame the popover content actually mounts
    // — focus side-effects then land against a stable composition.
    if let Some(ref data) = input_data {
        blinc_layout::widgets::text_input::focus_text_input_deferred(data);
    }
    if let Some(ref state) = area_state {
        blinc_layout::widgets::text_area::focus_text_area_deferred(state);
    }

    // Build the save callback once — both Enter and (future) Save
    // buttons fan into it. Reads the current value from the
    // appropriate state cell, patches host.groups, and pushes the
    // updated group back through the editor's command surface.
    let editor_for_save = editor.clone();
    let host_for_save = host.clone();
    let group_for_save = group.clone();
    let input_for_save = input_data.clone();
    let area_for_save = area_state.clone();
    let slot_for_save = handle_slot.clone();
    let field_for_save = field;
    let save = Arc::new(move || {
        let new_value: Option<String> = match field_for_save {
            EditorField::Title => input_for_save
                .as_ref()
                .map(|d| d.lock().unwrap().value.clone()),
            EditorField::Description => area_for_save
                .as_ref()
                .map(|s| s.lock().unwrap().lines.join("\n")),
        };
        let Some(new_value) = new_value else {
            return;
        };
        // Skip the round-trip when the user pressed Enter without
        // changing anything — no need to bump graph_rev.
        let unchanged = {
            let groups = host_for_save.groups.read().unwrap();
            groups
                .iter()
                .find(|g| g.id == group_for_save)
                .map(|g| match field_for_save {
                    EditorField::Title => g.name == new_value,
                    EditorField::Description => {
                        g.description.as_deref().unwrap_or("") == new_value
                    }
                })
                .unwrap_or(false)
        };
        if !unchanged {
            // Patch host first, then sync the editor's copy.
            let updated = {
                let mut groups = host_for_save.groups.write().unwrap();
                if let Some(g) = groups.iter_mut().find(|g| g.id == group_for_save) {
                    match field_for_save {
                        EditorField::Title => g.name = new_value.clone(),
                        EditorField::Description => {
                            g.description = if new_value.is_empty() {
                                None
                            } else {
                                Some(new_value.clone())
                            };
                        }
                    }
                    Some(g.clone())
                } else {
                    None
                }
            };
            if let Some(g) = updated {
                editor_for_save.insert_group(g);
            }
            tracing::info!(
                "inline edit committed: group={} field={:?}",
                group_for_save.as_str(),
                std::any::type_name_of_val(&field_for_save),
            );
        }
        if let Some(h) = slot_for_save.lock().unwrap().as_ref() {
            h.close();
        }
    });

    let close_only = {
        let slot = handle_slot.clone();
        Arc::new(move || {
            if let Some(h) = slot.lock().unwrap().as_ref() {
                h.close();
            }
        })
    };

    // Build the overlay's content closure. Re-runs whenever the
    // overlay's reactive surface re-renders (rare for a focused
    // editor). Each render rebuilds the inner widget bound to the
    // same input_data / area_state, so the text + cursor survive.
    let editor_id_for_content = editor_id.clone();
    let click_outside_key_for_close = click_outside_key.clone();
    let save_for_content = save.clone();
    let close_only_for_content = close_only.clone();
    let input_for_content = input_data.clone();
    let area_for_content = area_state.clone();
    let field_for_content = field;
    let estimated_w_for_content = estimated_w;

    let handle = OverlayBuilder::popover()
        .at(clamped_x, clamped_y)
        .on_close(move |_reason| {
            click_outside::unregister_click_outside(&click_outside_key_for_close);
            // Release focus on the text input / textarea so the
            // global pointer pipeline doesn't keep routing
            // pointer-down events to the (now-hidden) widget for
            // text-selection drag — that route blocks the editor's
            // own drag handlers and makes a previously-edited
            // group undraggable until the next focus change. Pairs
            // with `focus_text_input` on open. Handles both
            // text_input AND text_area focus trackers.
            blinc_layout::widgets::text_input::blur_all_text_inputs();
        })
        .content(move || {
            let save = save_for_content.clone();
            let close_only = close_only_for_content.clone();
            // cn::input + cn::textarea ship their own bg + border
            // + rounded chrome. Wrapping them in an additional
            // bordered popover div doubles the visible container;
            // the outer scaffold here strips its own chrome (no
            // bg, no border, no shadow) and only carries the id
            // for click-outside + the on_key_down handler for
            // Enter / Esc. The cn widget paints the only visible
            // surface.
            let body: Div = match field_for_content {
                EditorField::Title => {
                    let data = input_for_content
                        .as_ref()
                        .expect("title editor missing input data")
                        .clone();
                    // Auto-focus already applied: open_inline_text_editor
                    // calls focus_text_input on `data` BEFORE this
                    // closure runs, so the cn::input Stateful built
                    // here initialises directly in Focused state —
                    // no mid-frame Idle→Focused transition, no
                    // cascading rebuild, no static-cache invalidation.
                    div()
                        .w(estimated_w_for_content - padding * 2.0)
                        .child(blinc_cn::input(&data).w(estimated_w_for_content - padding * 2.0))
                }
                EditorField::Description => {
                    let state = area_for_content
                        .as_ref()
                        .expect("description editor missing area state")
                        .clone();
                    // Sized via the parent div — cn::textarea
                    // doesn't currently expose explicit width on
                    // its builder.
                    div()
                        .w(estimated_w_for_content - padding * 2.0)
                        .h(80.0)
                        .child(blinc_cn::textarea(&state))
                }
            };
            let save_for_keys = save.clone();
            let close_for_keys = close_only.clone();
            let field_for_keys = field_for_content;
            div()
                .id(&editor_id_for_content)
                .child(body)
                .on_key_down(move |evt| {
                    let kc = blinc_core::events::KeyCode(evt.key_code);
                    if kc == blinc_core::events::KeyCode::ESCAPE {
                        (close_for_keys)();
                        return;
                    }
                    if kc == blinc_core::events::KeyCode::ENTER {
                        // Description: Shift+Enter passes through
                        // to the textarea's own key handling (which
                        // inserts a newline). Title: plain Enter
                        // commits unconditionally — single-line
                        // input.
                        let allow_newline = matches!(field_for_keys, EditorField::Description)
                            && evt.shift;
                        if !allow_newline {
                            (save_for_keys)();
                        }
                    }
                })
        })
        .show();

    *handle_slot.lock().unwrap() = Some(handle);

    let handle_for_outside = handle;
    click_outside::register_click_outside(&click_outside_key, &editor_id, move || {
        handle_for_outside.close();
    });
    let _ = BlincContextState::get;
}

// ─── UI scaffold ───────────────────────────────────────────────────

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder + use<> {
    let (mut editor, host) = build_editor();

    // Drain the editor's event queue whenever any event is pushed.
    // A zero-size `stateful_with_key` widget with `deps` on the
    // events signal re-fires its closure on every push, which is
    // when we drain + dispatch to host state.
    //
    // The drain widget renders no chrome — it's mounted only for its
    // reactive lifecycle.
    let drain_editor = editor.clone();
    let drain_host = host.clone();
    let evts_signal = editor.events_signal();
    let drainer = stateful_with_key::<NoState>("node-editor-event-drain")
        .deps([evts_signal])
        .on_state(move |_ctx| {
            for evt in drain_editor.drain_events() {
                handle_event(&drain_editor, &drain_host, evt);
            }
            div().w(0.0).h(0.0)
        });

    // Cull-stats HUD overlays the canvas top-left, rebuilding when
    // either the graph changes (nodes added / removed) or the
    // viewport pans / zooms. Reads `editor.last_render_stats()` —
    // populated at the end of every render frame — so the displayed
    // counts demonstrate Tier 9.1 frustum culling: pan / zoom out
    // and the `visible` numbers drop while `total` stays put.
    let hud_editor = editor.clone();
    let hud_deps = [editor.graph_signal(), editor.canvas_kit().viewport_signal()];
    let stats_hud = stateful_with_key::<NoState>("node-editor-cull-hud")
        .deps(hud_deps)
        .on_state(move |_ctx| {
            let s = hud_editor.last_render_stats();
            // `RenderLayer::Foreground` per known upstream U1: the
            // canvas closure's primitives paint OVER absolutely-
            // positioned siblings unless those siblings opt into the
            // foreground layer. Without this the HUD draws but stays
            // hidden beneath the editor's canvas surface.
            div()
                .absolute()
                .top(12.0)
                .left(12.0)
                .layer(RenderLayer::Foreground)
                .bg(Color::rgba(0.0, 0.0, 0.0, 0.55))
                .p(8.0)
                .gap(4.0)
                .flex_col()
                .child(
                    text(&format!(
                        "visible nodes: {} / {}",
                        s.visible_nodes, s.total_nodes
                    ))
                    .size(11.0)
                    .color(Color::rgb(0.92, 0.92, 0.95)),
                )
                .child(
                    text(&format!(
                        "visible edges: {} / {}",
                        s.visible_edges, s.total_edges
                    ))
                    .size(11.0)
                    .color(Color::rgb(0.92, 0.92, 0.95)),
                )
                .child(
                    text("press D to toggle disabled on selection")
                        .size(10.0)
                        .color(Color::rgb(0.65, 0.65, 0.70)),
                )
        });

    // No manual `.bg(...)` here — the editor's `element()` paints
    // its own workspace surface from the active theme, so the host
    // only needs to position it.
    div()
        .w(ctx.width)
        .h(ctx.height)
        .flex_col()
        .child(header_bar())
        .child(
            div()
                .flex_grow()
                .overflow_clip()
                .w_full()
                .relative()
                .child(editor.element())
                .child(drainer)
                .child(stats_hud),
        )
}

fn header_bar() -> Div {
    div()
        .w_full()
        .h(50.0)
        .bg(token(ColorToken::SurfaceElevated, Color::rgb(0.12, 0.12, 0.18)))
        .flex_row()
        .items_center()
        .justify_center()
        .gap(20.0)
        .child(
            text("Node Editor Demo")
                .size(22.0)
                .weight(FontWeight::Bold)
                .color(token(ColorToken::TextPrimary, Color::WHITE)),
        )
        .child(
            text("Drag from output ports → input ports. Scroll = zoom, drag background = pan.")
                .size(13.0)
                .color(token(ColorToken::TextSecondary, Color::rgb(0.55, 0.55, 0.65))),
        )
}

fn main() -> blinc_app::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = WindowConfig {
        title: "Node Editor Demo".to_string(),
        width: 1100,
        height: 720,
        // Cap animation-driven redraws at 30 fps. The edge-state
        // shimmer / pulse don't need vsync to read smoothly, and
        // without this the FpsAdapter ramps up to 120 fps under
        // continuous animation — pinning ~40% CPU in debug. A
        // fixed cap disables the adaptive ramp.
        animation_fps_cap: Some(30),
        ..Default::default()
    };

    // Use the Hybrid Universal bundle in dark mode — gives us the
    // ShapeTokens + multi-layer shadows the editor consumes.
    blinc_app::windowed::WindowedApp::run_with_theme(
        config,
        HybridTheme::bundle().with_css(blinc_cn::cn_styles::CN_STYLES),
        // detect_system_color_scheme(),
        ColorScheme::Dark,
        build_ui,
    )
}

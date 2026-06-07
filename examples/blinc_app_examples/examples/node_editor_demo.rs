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

fn build_templates() -> Vec<NodeTemplate<DemoPort>> {
    let source = NodeTemplate::<DemoPort>::new("source", "Source")
        .with_category("data")
        .with_subtitle("Emit values")
        .with_icon(tabler_icon(outline::DATABASE))
        .with_output(PortDesc::new(
            "out_num",
            "value",
            Direction::Output,
            DemoPort::Number,
        ));

    let filter = NodeTemplate::<DemoPort>::new("filter", "Filter")
        .with_category("transform")
        .with_subtitle("Threshold gate")
        .with_icon(tabler_icon(outline::FILTER))
        .with_input(PortDesc::new(
            "in_num",
            "input",
            Direction::Input,
            DemoPort::Number,
        ))
        .with_input(PortDesc::new(
            "in_threshold",
            "threshold",
            Direction::Input,
            DemoPort::Number,
        ))
        .with_output(PortDesc::new(
            "out_pass",
            "pass?",
            Direction::Output,
            DemoPort::Boolean,
        ));

    let formatter = NodeTemplate::<DemoPort>::new("formatter", "Formatter")
        .with_category("transform")
        .with_subtitle("Number → String")
        .with_icon(tabler_icon(outline::TYPOGRAPHY))
        .with_input(PortDesc::new(
            "in_num",
            "value",
            Direction::Input,
            DemoPort::Number,
        ))
        .with_output(PortDesc::new(
            "out_str",
            "text",
            Direction::Output,
            DemoPort::String,
        ));

    let sink = NodeTemplate::<DemoPort>::new("sink", "Sink")
        .with_category("data")
        .with_subtitle("Display")
        .with_icon(tabler_icon(outline::DEVICE_DESKTOP))
        .with_input(PortDesc::new(
            "in_pass",
            "gate",
            Direction::Input,
            DemoPort::Boolean,
        ))
        .with_input(PortDesc::new(
            "in_str",
            "label",
            Direction::Input,
            DemoPort::String,
        ));

    vec![source, filter, formatter, sink]
}

// ─── Initial graph ─────────────────────────────────────────────────

type Editor = NodeEditor<DemoPort, (), (), ()>;

fn initial_nodes() -> Vec<NodeInstance<()>> {
    vec![
        NodeInstance::new("src/1", "source", Point::new(80.0, 120.0))
            .with_size(180.0, 80.0)
            .with_badge(StatusBadge::success()),
        NodeInstance::new("src/threshold", "source", Point::new(80.0, 280.0))
            .with_subtitle("Threshold const")
            .with_size(180.0, 80.0),
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
            CanvasBackground::dots(token(ColorToken::Border, Color::rgba(0.5, 0.5, 0.55, 0.65)))
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
    let ids_for_delete = node_ids;
    let slot_for_group = handle_slot.clone();
    let slot_for_delete = handle_slot.clone();

    let handle = OverlayBuilder::popover()
        .at(anchor_screen.x, anchor_screen.y)
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
            let error_color = theme.color(ColorToken::Error);
            let mut row = div()
                .id(&toolbar_id_for_content)
                .flex_row()
                .gap(6.0)
                .p_px(padding)
                .bg(bg)
                .border(1.0, border)
                .rounded(radius)
                .shadow_lg();
            if can_group {
                row = row.child(
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
                                    .with_description("Group created from multi-select");
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
                );
            }
            row.child(
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
        EditorEvent::CreateGroupRequested(_)
        | EditorEvent::EdgeClicked { .. }
        | EditorEvent::NodeClicked { .. }
        | EditorEvent::LayoutApplied(_) => {
            // Unhandled in this demo; real hosts dispatch to their
            // command palette / inspector / layout code.
        }
    }
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
                .child(editor.element())
                .child(drainer),
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

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
        EditorEvent::CreateGroupRequested(_)
        | EditorEvent::AddToGroupRequested(_)
        | EditorEvent::RemoveFromGroupRequested(_)
        | EditorEvent::ToggleCollapseRequested(_)
        | EditorEvent::DeleteGroupRequested(_)
        | EditorEvent::DeleteConnectionRequested(_)
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

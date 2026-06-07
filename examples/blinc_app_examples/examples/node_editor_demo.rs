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
use blinc_tabler_icons::{outline, to_svg_colored};
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

/// Build a node icon from a Tabler outline path constant + the
/// title text colour. Colouring via `to_svg_colored` (rather than
/// the default black) makes the glyph read against the theme's
/// dark header band.
fn tabler_icon(path_data: &str) -> NodeIcon {
    let colour_hex = token_hex(ColorToken::TextPrimary, "#e8e8e8");
    NodeIcon::from_svg_str(&to_svg_colored(path_data, 16.0, &colour_hex))
        .expect("tabler emits valid SVG")
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

fn build_editor() -> Editor {
    let editor: Editor = NodeEditor::new("node-editor-demo")
        .with_templates(build_templates())
        .with_background(
            CanvasBackground::dots(token(ColorToken::Border, Color::rgba(0.5, 0.5, 0.55, 0.65)))
                .with_zoom_adaptive(0.3, 5),
        );

    // The host-managed graph state — owned by the demo. The editor
    // calls back into us via the request hooks; we mutate this and
    // re-sync via `editor.set_graph(...)`.
    let nodes = Arc::new(RwLock::new(initial_nodes()));
    let connections = Arc::new(RwLock::new(initial_connections()));
    let groups = Arc::new(RwLock::new(initial_groups()));

    // Initial sync.
    editor.set_graph(
        nodes.read().unwrap().clone(),
        connections.read().unwrap().clone(),
        groups.read().unwrap().clone(),
        Vec::new(),
    );

    // Validator — accept connections whose port kinds match.
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

    // Materialise accepted connections into the host model + re-sync.
    let nodes_c = nodes.clone();
    let connections_c = connections.clone();
    let groups_c = groups.clone();
    let editor = {
        let editor_inner = editor.clone();
        editor.on_connect_accepted(move |evt| {
            connections_c
                .write()
                .unwrap()
                .push(Connection::new(evt.from.clone(), evt.to.clone()));
            editor_inner.set_graph(
                nodes_c.read().unwrap().clone(),
                connections_c.read().unwrap().clone(),
                groups_c.read().unwrap().clone(),
                Vec::new(),
            );
            tracing::info!(
                "connected {:?} -> {:?}",
                (evt.from.node.as_str(), evt.from.port.as_str()),
                (evt.to.node.as_str(), evt.to.port.as_str()),
            );
        })
    };

    // Persist drag positions on the host side.
    let nodes_d = nodes.clone();
    let connections_d = connections.clone();
    let groups_d = groups.clone();
    let editor = {
        let editor_inner = editor.clone();
        editor.on_node_drag(move |id, new_pos| {
            let mut ns = nodes_d.write().unwrap();
            if let Some(n) = ns.iter_mut().find(|n| &n.id == id) {
                n.position = new_pos;
            }
            drop(ns);
            editor_inner.set_graph(
                nodes_d.read().unwrap().clone(),
                connections_d.read().unwrap().clone(),
                groups_d.read().unwrap().clone(),
                Vec::new(),
            );
        })
    };

    editor
}

// ─── UI scaffold ───────────────────────────────────────────────────

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder + use<> {
    let editor = build_editor();
    // No manual `.bg(...)` here — the editor's `element()` paints
    // its own workspace surface from the active theme, so the host
    // only needs to position it.
    div()
        .w(ctx.width)
        .h(ctx.height)
        .flex_col()
        .child(header_bar())
        .child(div().flex_grow().overflow_clip().w_full().child(editor.element()))
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

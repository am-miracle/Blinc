//! Canvas Kit Interactive Demo
//!
//! Demonstrates `blinc_canvas_kit` features:
//! - Pan (drag background) and zoom (scroll wheel) on an infinite canvas
//! - `kit.element()` builder with auto-wired event handlers
//! - `kit.handler()` for custom event wiring
//! - Hit testing via `kit.hit_rect()` inside draw callbacks
//! - Click, drag, and hover callbacks on canvas-drawn elements
//! - Viewport state HUD overlay
//!
//! Run with: cargo run -p blinc_app_examples --example canvas_kit_demo --features canvas

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_canvas_kit::prelude::*;
use blinc_core::draw::Stroke;
use blinc_core::{
    BlincContextState, Brush, Color, CornerRadius, DrawContext, Gradient, Point, Rect, State,
};
use std::f32::consts::PI;

// ─── Node Data ──────────────────────────────────────────────────────────────

/// (x, y, w, h, label_color, bg_color)
type NodeDef = (f32, f32, f32, f32, Color, Color);

const NODE_COUNT: usize = 6;

fn initial_nodes() -> [NodeDef; NODE_COUNT] {
    [
        (
            100.0,
            150.0,
            140.0,
            70.0,
            Color::rgba(0.4, 0.85, 1.0, 1.0),
            Color::rgba(0.15, 0.25, 0.35, 0.95),
        ),
        (
            350.0,
            80.0,
            140.0,
            70.0,
            Color::rgba(0.4, 1.0, 0.7, 1.0),
            Color::rgba(0.15, 0.3, 0.2, 0.95),
        ),
        (
            350.0,
            240.0,
            140.0,
            70.0,
            Color::rgba(1.0, 0.7, 0.4, 1.0),
            Color::rgba(0.35, 0.2, 0.1, 0.95),
        ),
        (
            600.0,
            160.0,
            140.0,
            70.0,
            Color::rgba(0.85, 0.5, 1.0, 1.0),
            Color::rgba(0.25, 0.15, 0.35, 0.95),
        ),
        (
            200.0,
            400.0,
            140.0,
            70.0,
            Color::rgba(1.0, 0.85, 0.4, 1.0),
            Color::rgba(0.35, 0.3, 0.1, 0.95),
        ),
        (
            500.0,
            380.0,
            140.0,
            70.0,
            Color::rgba(1.0, 0.4, 0.6, 1.0),
            Color::rgba(0.35, 0.1, 0.15, 0.95),
        ),
    ]
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = WindowConfig {
        title: "Canvas Kit Demo".to_string(),
        width: 900,
        height: 700,
        ..Default::default()
    };

    blinc_app::windowed::WindowedApp::run(config, build_ui)
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder + use<> {
    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.08, 0.08, 0.12, 1.0))
        .flex_col()
        .child(header_bar())
        .child(
            div()
                .flex_row()
                .flex_grow()
                .w_full()
                .child(builder_demo_panel())
                .child(handler_demo_panel()),
        )
}

// ─── Header ──────────────────────────────────────────────────────────────────

fn header_bar() -> Div {
    div()
        .w_full()
        .h(50.0)
        .bg(Color::rgba(0.12, 0.12, 0.18, 1.0))
        .flex_row()
        .items_center()
        .justify_center()
        .gap(20.0)
        .child(
            text("Canvas Kit Demo")
                .size(22.0)
                .weight(FontWeight::Bold)
                .color(Color::WHITE),
        )
        .child(
            text("Drag nodes to move, scroll to zoom, drag background to pan")
                .size(13.0)
                .color(Color::rgba(0.5, 0.5, 0.6, 1.0)),
        )
}

// ─── Demo 1: Builder API — Draggable Node Graph ─────────────────────────────

fn builder_demo_panel() -> Div {
    let mut kit = CanvasKit::new("builder_demo");
    kit.set_background(CanvasBackground::dots().with_zoom_adaptive(0.3, 5));

    // Persistent node positions — [[x, y, w, h]; NODE_COUNT]
    let bctx = BlincContextState::get();
    let nodes_state: State<Vec<[f32; 4]>> = bctx.use_state_keyed("builder_nodes", || {
        initial_nodes()
            .iter()
            .map(|(x, y, w, h, _, _)| [*x, *y, *w, *h])
            .collect()
    });

    // Wire up callbacks
    let nodes_drag = nodes_state.clone();
    kit.on_element_drag(move |evt| {
        let mut nodes = nodes_drag.get();
        // Parse node index from region ID like "node_0"
        if let Some(idx) = evt
            .region_id
            .strip_prefix("node_")
            .and_then(|s| s.parse::<usize>().ok())
        {
            if idx < nodes.len() {
                nodes[idx][0] += evt.content_delta.x;
                nodes[idx][1] += evt.content_delta.y;
                nodes_drag.set(nodes);
            }
        }
    });

    kit.on_element_click(move |evt| {
        if let Some(ref id) = evt.region_id {
            tracing::info!(
                "Clicked {id} at content ({:.0}, {:.0})",
                evt.content_point.x,
                evt.content_point.y
            );
        }
    });

    kit.on_element_hover(move |evt| match &evt.region_id {
        Some(id) => tracing::debug!("Hover → {id}"),
        None => tracing::debug!("Hover → background"),
    });

    let kit_hud = kit.clone();
    let kit_reset = kit.clone();
    let kit_interaction = kit.clone();
    let nodes_render = nodes_state.clone();
    let nodes_reset = nodes_state.clone();

    div()
        .flex_grow()
        .flex_col()
        .m(6.0)
        .rounded(12.0)
        .bg(Color::rgba(0.1, 0.1, 0.15, 1.0))
        .overflow_clip()
        .child(panel_title("kit.element() — Draggable Node Graph"))
        .child(
            div()
                .flex_grow()
                .w_full()
                .overflow_clip()
                .child(kit.element(move |ctx, _bounds| {
                    draw_interactive_node_graph(ctx, &kit_interaction, &nodes_render);
                }))
                .child(viewport_hud(kit_hud, "builder_hud"))
                .child(reset_button(kit_reset, nodes_reset, "reset_button_1")),
        )
}

// ─── Demo 2: Handler API — Interactive Shapes ───────────────────────────────

fn handler_demo_panel() -> Div {
    let mut kit = CanvasKit::new("handler_demo");
    kit.set_background(
        CanvasBackground::grid()
            .with_spacing(40.0)
            .with_zoom_adaptive(0.3, 5),
    );

    kit.on_element_click(move |evt| {
        if let Some(ref id) = evt.region_id {
            tracing::info!("Shape clicked: {id}");
        }
    });

    kit.on_element_hover(move |evt| match &evt.region_id {
        Some(id) => tracing::debug!("Shape hover → {id}"),
        None => tracing::debug!("Shape hover → background"),
    });

    let kit_hud = kit.clone();
    let kit_reset = kit.clone();
    let kit_render = kit.clone();

    div()
        .flex_grow()
        .flex_col()
        .m(6.0)
        .rounded(12.0)
        .bg(Color::rgba(0.1, 0.1, 0.15, 1.0))
        .overflow_clip()
        .child(panel_title("kit.element() — Shape Canvas"))
        .child(
            div()
                .flex_grow()
                .w_full()
                .overflow_clip()
                .child(kit.element(move |ctx, _bounds| {
                    draw_interactive_shapes(ctx, &kit_render);
                }))
                .child(viewport_hud(kit_hud, "handler_hud"))
                .child(reset_button_simple(kit_reset, "reset_button_2")),
        )
}

// ─── Drawing: Interactive Node Graph ────────────────────────────────────────

fn draw_interactive_node_graph(
    ctx: &mut dyn DrawContext,
    kit: &CanvasKit,
    nodes_state: &State<Vec<[f32; 4]>>,
) {
    let defs = initial_nodes();
    let positions = nodes_state.get();
    let interaction = kit.interaction();

    // Connections: (from_node, to_node)
    let connections = [(0, 1), (0, 2), (1, 3), (2, 3), (4, 5), (2, 5)];

    // Draw connections first (behind nodes)
    for (from, to) in &connections {
        let fp = &positions[*from];
        let tp = &positions[*to];

        let start_x = fp[0] + fp[2]; // x + w
        let start_y = fp[1] + fp[3] / 2.0; // y + h/2
        let end_x = tp[0];
        let end_y = tp[1] + tp[3] / 2.0;

        let steps = 20;
        for i in 0..steps {
            let t = i as f32 / steps as f32;
            let t_next = (i + 1) as f32 / steps as f32;
            let mid_x = (start_x + end_x) / 2.0;
            let x1 = bezier_cubic(t, start_x, mid_x, mid_x, end_x);
            let y1 = bezier_cubic(t, start_y, start_y, end_y, end_y);
            let x2 = bezier_cubic(t_next, start_x, mid_x, mid_x, end_x);
            let y2 = bezier_cubic(t_next, start_y, start_y, end_y, end_y);

            ctx.fill_rect(
                Rect::new(
                    x1.min(x2),
                    y1.min(y2),
                    (x2 - x1).abs().max(2.0),
                    (y2 - y1).abs().max(2.0),
                ),
                CornerRadius::uniform(1.0),
                Brush::Solid(Color::rgba(0.4, 0.5, 0.6, 0.4 + 0.3 * t)),
            );
        }

        // Connection endpoints (ports)
        let port_r = 5.0;
        ctx.fill_rect(
            Rect::new(
                start_x - port_r,
                start_y - port_r,
                port_r * 2.0,
                port_r * 2.0,
            ),
            CornerRadius::uniform(port_r),
            Brush::Solid(Color::rgba(0.5, 0.7, 0.9, 1.0)),
        );
        ctx.fill_rect(
            Rect::new(end_x - port_r, end_y - port_r, port_r * 2.0, port_r * 2.0),
            CornerRadius::uniform(port_r),
            Brush::Solid(Color::rgba(0.5, 0.7, 0.9, 1.0)),
        );
    }

    // Draw nodes with hit regions
    for (i, (pos, def)) in positions.iter().zip(defs.iter()).enumerate() {
        let (x, y, w, h) = (pos[0], pos[1], pos[2], pos[3]);
        let (_, _, _, _, label_color, bg_color) = def;
        let node_id = format!("node_{i}");

        let is_hovered = interaction.hovered.as_deref() == Some(node_id.as_str());
        let is_active = interaction.active.as_deref() == Some(node_id.as_str());

        // Shadow (deeper when active)
        let shadow_offset = if is_active { 6.0 } else { 3.0 };
        let shadow_alpha = if is_active { 0.5 } else { 0.3 };
        ctx.fill_rect(
            Rect::new(x + shadow_offset, y + shadow_offset, w, h),
            CornerRadius::uniform(10.0),
            Brush::Solid(Color::rgba(0.0, 0.0, 0.0, shadow_alpha)),
        );

        // Node body — brighter on hover
        let hover_boost = if is_hovered || is_active { 0.08 } else { 0.0 };
        let body_color = Color::rgba(
            (bg_color.r + hover_boost).min(1.0),
            (bg_color.g + hover_boost).min(1.0),
            (bg_color.b + hover_boost).min(1.0),
            bg_color.a,
        );
        ctx.fill_rect(
            Rect::new(x, y, w, h),
            CornerRadius::uniform(10.0),
            Brush::Solid(body_color),
        );

        // Hover outline
        if is_hovered || is_active {
            let outline_alpha = if is_active { 0.6 } else { 0.3 };
            ctx.stroke_rect(
                Rect::new(x - 1.0, y - 1.0, w + 2.0, h + 2.0),
                CornerRadius::uniform(11.0),
                &Stroke::new(2.0),
                Brush::Solid(Color::rgba(
                    label_color.r,
                    label_color.g,
                    label_color.b,
                    outline_alpha,
                )),
            );
        }

        // Top bar
        ctx.fill_rect(
            Rect::new(x, y, w, 22.0),
            CornerRadius {
                top_left: 10.0,
                top_right: 10.0,
                bottom_left: 0.0,
                bottom_right: 0.0,
            },
            Brush::Solid(Color::rgba(
                label_color.r,
                label_color.g,
                label_color.b,
                0.2,
            )),
        );

        // Status indicator dot
        let dot_r = 4.0;
        ctx.fill_rect(
            Rect::new(x + 10.0, y + 7.0, dot_r * 2.0, dot_r * 2.0),
            CornerRadius::uniform(dot_r),
            Brush::Solid(*label_color),
        );

        // Content bars
        for row in 0..2 {
            let bar_y = y + 30.0 + row as f32 * 14.0;
            let bar_w = w - 30.0 - (row as f32 * 20.0);
            ctx.fill_rect(
                Rect::new(x + 12.0, bar_y, bar_w, 6.0),
                CornerRadius::uniform(3.0),
                Brush::Solid(Color::rgba(1.0, 1.0, 1.0, 0.1)),
            );
        }

        // Register hit region for this node
        let node_rect = Rect::new(x, y, w, h);
        kit.hit_rect(&node_id, node_rect);
    }
}

// ─── Drawing: Interactive Shapes ────────────────────────────────────────────

fn draw_interactive_shapes(ctx: &mut dyn DrawContext, kit: &CanvasKit) {
    let interaction = kit.interaction();

    // Circles
    let circles: &[(&str, f32, f32, f32, Color)] = &[
        (
            "circle_0",
            200.0,
            200.0,
            60.0,
            Color::rgba(0.3, 0.6, 1.0, 0.7),
        ),
        (
            "circle_1",
            400.0,
            150.0,
            40.0,
            Color::rgba(1.0, 0.4, 0.5, 0.7),
        ),
        (
            "circle_2",
            300.0,
            350.0,
            80.0,
            Color::rgba(0.3, 0.9, 0.5, 0.5),
        ),
        (
            "circle_3",
            550.0,
            300.0,
            50.0,
            Color::rgba(0.9, 0.7, 0.2, 0.6),
        ),
        (
            "circle_4",
            150.0,
            420.0,
            35.0,
            Color::rgba(0.7, 0.3, 1.0, 0.6),
        ),
    ];

    for (id, cx, cy, r, color) in circles {
        let is_hovered = interaction.hovered.as_deref() == Some(*id);

        // Shadow
        ctx.fill_rect(
            Rect::new(cx - r + 4.0, cy - r + 4.0, r * 2.0, r * 2.0),
            CornerRadius::uniform(*r),
            Brush::Solid(Color::rgba(0.0, 0.0, 0.0, 0.2)),
        );

        // Circle — boost alpha on hover
        let alpha = if is_hovered {
            (color.a + 0.3).min(1.0)
        } else {
            color.a
        };
        let draw_color = Color::rgba(color.r, color.g, color.b, alpha);
        ctx.fill_rect(
            Rect::new(cx - r, cy - r, r * 2.0, r * 2.0),
            CornerRadius::uniform(*r),
            Brush::Gradient(Gradient::linear(
                Point::new(cx - r, cy - r),
                Point::new(cx + r, cy + r),
                draw_color,
                Color::rgba(
                    draw_color.r * 0.6,
                    draw_color.g * 0.6,
                    draw_color.b * 0.6,
                    draw_color.a,
                ),
            )),
        );

        // Hover outline
        if is_hovered {
            ctx.stroke_rect(
                Rect::new(cx - r - 2.0, cy - r - 2.0, r * 2.0 + 4.0, r * 2.0 + 4.0),
                CornerRadius::uniform(r + 2.0),
                &Stroke::new(2.0),
                Brush::Solid(Color::rgba(1.0, 1.0, 1.0, 0.4)),
            );
        }

        // Hit region (bounding box of the circle)
        kit.hit_rect(*id, Rect::new(cx - r, cy - r, r * 2.0, r * 2.0));
    }

    // Rectangles
    let rects: &[(&str, f32, f32, f32, f32, Color)] = &[
        (
            "rect_0",
            80.0,
            100.0,
            100.0,
            60.0,
            Color::rgba(1.0, 0.6, 0.2, 0.6),
        ),
        (
            "rect_1",
            450.0,
            400.0,
            120.0,
            80.0,
            Color::rgba(0.2, 0.7, 0.9, 0.6),
        ),
        (
            "rect_2",
            600.0,
            100.0,
            80.0,
            120.0,
            Color::rgba(0.8, 0.3, 0.8, 0.6),
        ),
    ];

    for (id, x, y, w, h, color) in rects {
        let is_hovered = interaction.hovered.as_deref() == Some(*id);

        ctx.fill_rect(
            Rect::new(x + 3.0, y + 3.0, *w, *h),
            CornerRadius::uniform(8.0),
            Brush::Solid(Color::rgba(0.0, 0.0, 0.0, 0.2)),
        );

        let alpha = if is_hovered {
            (color.a + 0.3).min(1.0)
        } else {
            color.a
        };
        ctx.fill_rect(
            Rect::new(*x, *y, *w, *h),
            CornerRadius::uniform(8.0),
            Brush::Solid(Color::rgba(color.r, color.g, color.b, alpha)),
        );

        if is_hovered {
            ctx.stroke_rect(
                Rect::new(x - 2.0, y - 2.0, w + 4.0, h + 4.0),
                CornerRadius::uniform(10.0),
                &Stroke::new(2.0),
                Brush::Solid(Color::rgba(1.0, 1.0, 1.0, 0.4)),
            );
        }

        kit.hit_rect(*id, Rect::new(*x, *y, *w, *h));
    }

    // Star
    draw_star(
        ctx,
        350.0,
        280.0,
        45.0,
        20.0,
        5,
        Color::rgba(1.0, 0.9, 0.3, 0.8),
    );
    kit.hit_rect("star", Rect::new(350.0 - 45.0, 280.0 - 45.0, 90.0, 90.0));

    // Crosshair at origin
    let cross_color = Brush::Solid(Color::rgba(1.0, 0.3, 0.3, 0.5));
    ctx.fill_rect(
        Rect::new(-1.0, -20.0, 2.0, 40.0),
        0.0.into(),
        cross_color.clone(),
    );
    ctx.fill_rect(Rect::new(-20.0, -1.0, 40.0, 2.0), 0.0.into(), cross_color);
}

// ─── Drawing Helpers ────────────────────────────────────────────────────────

fn draw_star(
    ctx: &mut dyn DrawContext,
    cx: f32,
    cy: f32,
    outer_r: f32,
    _inner_r: f32,
    points: usize,
    color: Color,
) {
    for i in 0..points {
        let angle = (i as f32 / points as f32) * PI * 2.0 - PI / 2.0;
        let ox = cx + outer_r * angle.cos();
        let oy = cy + outer_r * angle.sin();
        let w = 6.0;

        ctx.fill_rect(
            Rect::new(
                cx.min(ox) - w / 2.0,
                cy.min(oy) - w / 2.0,
                (ox - cx).abs() + w,
                (oy - cy).abs() + w,
            ),
            CornerRadius::uniform(w / 2.0),
            Brush::Solid(color),
        );
    }

    ctx.fill_rect(
        Rect::new(cx - 8.0, cy - 8.0, 16.0, 16.0),
        CornerRadius::uniform(8.0),
        Brush::Solid(color),
    );
}

fn bezier_cubic(t: f32, p0: f32, p1: f32, p2: f32, p3: f32) -> f32 {
    let mt = 1.0 - t;
    mt * mt * mt * p0 + 3.0 * mt * mt * t * p1 + 3.0 * mt * t * t * p2 + t * t * t * p3
}

// ─── UI Overlays ─────────────────────────────────────────────────────────────

fn panel_title(label: &str) -> Div {
    div()
        .w_full()
        .h(32.0)
        .bg(Color::rgba(0.15, 0.15, 0.22, 1.0))
        .flex_row()
        .items_center()
        .px(12.0)
        .child(
            text(label)
                .size(13.0)
                .weight(FontWeight::SemiBold)
                .color(Color::rgba(0.7, 0.7, 0.8, 1.0)),
        )
}

fn viewport_hud(kit: CanvasKit, unique_key: &str) -> Stateful<NoState> {
    let signal = kit.viewport_signal();
    stateful_with_key::<NoState>(unique_key)
        .deps([signal])
        .on_state(move |_ctx| {
            let vp = kit.viewport();
            let zoom_pct = (vp.zoom * 100.0) as i32;

            div()
                .absolute()
                .left(8.0)
                .top(40.0)
                .bg(Color::rgba(0.0, 0.0, 0.0, 0.6))
                .rounded(6.0)
                .px(8.0)
                .py(4.0)
                .flex_col()
                .gap(2.0)
                .child(
                    text(format!("Zoom: {}%", zoom_pct))
                        .size(11.0)
                        .color(Color::rgba(0.6, 0.8, 1.0, 1.0)),
                )
                .child(
                    text(format!("Pan: ({:.0}, {:.0})", vp.pan_x, vp.pan_y))
                        .size(11.0)
                        .color(Color::rgba(0.6, 0.8, 1.0, 1.0)),
                )
        })
}

fn reset_button(
    kit: CanvasKit,
    nodes_state: State<Vec<[f32; 4]>>,
    unique_key: &str,
) -> Stateful<ButtonState> {
    stateful_with_key::<ButtonState>(unique_key).on_state(move |ctx| {
        let bg = match ctx.state() {
            ButtonState::Idle => Color::rgba(0.2, 0.2, 0.3, 0.8),
            ButtonState::Hovered => Color::rgba(0.3, 0.3, 0.45, 0.9),
            _ => Color::rgba(0.15, 0.15, 0.25, 0.8),
        };

        if let Some(evt) = ctx.event() {
            if evt.event_type == blinc_core::events::event_types::POINTER_UP {
                kit.update_viewport(|vp| vp.reset());
                // Also reset node positions
                let reset_positions: Vec<[f32; 4]> = initial_nodes()
                    .iter()
                    .map(|(x, y, w, h, _, _)| [*x, *y, *w, *h])
                    .collect();
                nodes_state.set(reset_positions);
            }
        }

        div()
            .absolute()
            .left(8.0)
            .top(120.0)
            .bg(bg)
            .rounded(6.0)
            .px(10.0)
            .py(4.0)
            .child(
                text("Reset")
                    .size(11.0)
                    .color(Color::rgba(0.8, 0.8, 0.9, 1.0)),
            )
    })
}

fn reset_button_simple(kit: CanvasKit, unique_key: &str) -> Stateful<ButtonState> {
    stateful_with_key::<ButtonState>(unique_key).on_state(move |ctx| {
        let bg = match ctx.state() {
            ButtonState::Idle => Color::rgba(0.2, 0.2, 0.3, 0.8),
            ButtonState::Hovered => Color::rgba(0.3, 0.3, 0.45, 0.9),
            _ => Color::rgba(0.15, 0.15, 0.25, 0.8),
        };

        if let Some(evt) = ctx.event() {
            if evt.event_type == blinc_core::events::event_types::POINTER_UP {
                kit.update_viewport(|vp| vp.reset());
            }
        }

        div()
            .absolute()
            .left(8.0)
            .top(120.0)
            .bg(bg)
            .rounded(6.0)
            .px(10.0)
            .py(4.0)
            .child(
                text("Reset")
                    .size(11.0)
                    .color(Color::rgba(0.8, 0.8, 0.9, 1.0)),
            )
    })
}

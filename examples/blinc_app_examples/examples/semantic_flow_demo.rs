//! Semantic @flow Demo
//!
//! Demonstrates the semantic step/chain/use system for @flow shaders.
//! Uses `step`, `chain`, and raw `node` syntax together to create a
//! layered noise visualization with pointer-reactive color ramping.
//!
//! The fourth card ("Plasma") uses the `flow!` macro to define a flow
//! shader entirely in Rust — no CSS strings needed.
//!
//! Run with: cargo run -p blinc_app_examples --example semantic_flow_demo

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;
use blinc_app::windowed::WindowedContext;
use blinc_core::{Color, Shadow};
use blinc_layout::flow;

const STYLESHEET: &str = r#"
    /* ====== Semantic flow: steps + chains + raw nodes ====== */

    /* Step demo: noise pattern with color ramp */
    @flow terrain {
        target: fragment;
        input uv: builtin(uv);
        input time: builtin(time);

        /* Semantic step — procedural noise */
        step noise: pattern-noise {
            scale: 4.0;
            detail: 6;
            animation: time * 0.3;
        }

        /* Raw node for contrast adjustment */
        node contrast = smoothstep(0.2, 0.8, noise);

        /* Semantic step — color mapping from scalar to palette */
        step palette: color-ramp {
            source: contrast;
            stops: #1a3a5a 0.0, #4488aa 0.35, #88cc66 0.55, #ccdd44 0.75, #ffffff 1.0;
        }

        output color = palette;
    }

    /* Chain demo: ripple effect with falloff */
    @flow ripple {
        target: fragment;
        input uv: builtin(uv);
        input time: builtin(time);

        chain effect:
            pattern-ripple(center: vec2(0.5, 0.5), density: 25.0, speed: 3.0)
            | adjust-falloff(radius: 0.5)
            ;

        /* Map scalar to blue-white gradient */
        step tint: color-ramp {
            source: effect;
            stops: #002244 0.0, #0066cc 0.4, #88ddff 0.7, #ffffff 1.0;
        }

        output color = tint;
    }

    /* Mixed demo: raw math + translucent step */
    @flow waves {
        target: fragment;
        input uv: builtin(uv);
        input time: builtin(time);

        /* Raw math for custom wave pattern */
        node cx = uv.x - 0.5;
        node cy = uv.y - 0.5;
        node dist = length(vec2(cx, cy));
        node wave1 = sin(dist * 30.0 - time * 2.0) * 0.5 + 0.5;
        node wave2 = sin(uv.x * 15.0 + time * 1.5) * 0.5 + 0.5;
        node combined = wave1 * 0.6 + wave2 * 0.4;

        /* Semantic color ramp with translucency */
        step colored: color-ramp {
            source: combined;
            opacity: 0.50;
            stops: #0c2b52 0.0, #5787da 0.2, #2c5ac7 0.4, #407dee 0.8, #d9eef2 1.0;
        }

        output color = colored;
    }

    /* ====== Card styling ====== */
    #terrain-card {
        flow: terrain;
        border-radius: 24px;
    }
    #ripple-card {
        flow: ripple;
        border-radius: 24px;
    }
    #waves-card {
        flow: waves;
        border-radius: 24px;
    }
"#;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let config = WindowConfig {
        title: "Semantic Flow Demo".to_string(),
        width: 1400,
        height: 600,
        resizable: true,
        ..Default::default()
    };

    WindowedApp::run(config, build_ui)
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    if ctx.rebuild_count == 0 {
        ctx.add_css(STYLESHEET);
    }

    let bg = Color::rgba(0.12, 0.12, 0.14, 1.0);
    // Scale cards to fit: 2 columns on narrow screens, up to 4 on wide.
    // Available width = viewport - padding (16px each side)
    let available = ctx.width - 32.0;
    let cols = if available >= 4.0 * 200.0 + 3.0 * 16.0 {
        4.0
    } else {
        2.0
    };
    let card_w = ((available - (cols - 1.0) * 16.0) / cols).min(280.0);
    let card_h = card_w;
    let label_color = Color::rgba(0.85, 0.85, 0.90, 1.0);
    let sub_color = Color::rgba(0.55, 0.55, 0.60, 1.0);
    let default_bg = Color::WHITE;

    // Define a flow shader using the flow! macro — pure Rust, no CSS strings
    let plasma = flow!(plasma, fragment, {
        input uv: builtin(uv);
        input time: builtin(time);
        node cx = uv.x - 0.5;
        node cy = uv.y - 0.5;
        node d = length(vec2(cx, cy));
        node w1 = sin(d * 25.0 - time * 3.0) * 0.5 + 0.5;
        node w2 = sin(uv.x * 15.0 + uv.y * 10.0 + time * 2.0) * 0.5 + 0.5;
        node blend = w1 * 0.6 + w2 * 0.4;
        node r = blend * 0.8 + 0.1;
        node g = sin(blend * 3.14159) * 0.6;
        node b = 1.0 - blend * 0.7;
        output color = vec4(r, g, b, 1.0);
    });

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(bg)
        .p(16.0)
        .items_center()
        .justify_center()
        .child(
            div()
                .flex_row()
                .flex_wrap()
                .gap(16.0)
                .items_center()
                .justify_center()
                .child(card(
                    "terrain-card",
                    "Terrain",
                    "step + node + step",
                    card_w,
                    card_h,
                    default_bg,
                    label_color,
                    sub_color,
                ))
                .child(card(
                    "ripple-card",
                    "Ripple",
                    "chain + step",
                    card_w,
                    card_h,
                    default_bg,
                    label_color,
                    sub_color,
                ))
                .child(card(
                    "waves-card",
                    "Waves",
                    "translucent",
                    card_w,
                    card_h,
                    Color::rgba(0.85, 0.55, 0.15, 1.0),
                    label_color,
                    sub_color,
                ))
                .child(flow_macro_card(
                    plasma,
                    "Plasma",
                    "flow! macro",
                    card_w,
                    card_h,
                    label_color,
                    sub_color,
                )),
        )
}

#[allow(clippy::too_many_arguments)]
fn card(
    id: &str,
    title: &str,
    subtitle: &str,
    w: f32,
    h: f32,
    bg_color: Color,
    label_color: Color,
    sub_color: Color,
) -> impl ElementBuilder {
    div()
        .flex_col()
        .items_center()
        .gap(12.0)
        .child(div().id(id).w(w).h(h).bg(bg_color).shadow(Shadow::new(
            0.0,
            4.0,
            20.0,
            Color::rgba(0.0, 0.0, 0.0, 0.4),
        )))
        .child(
            text(title)
                .size(20.0)
                .weight(FontWeight::Bold)
                .color(label_color),
        )
        .child(text(subtitle).size(14.0).color(sub_color))
}

/// Card that uses a flow! macro-defined shader — no CSS string, no stylesheet registration.
/// The FlowGraph is passed directly to the div via `.flow(graph)`.
fn flow_macro_card(
    graph: blinc_core::FlowGraph,
    title: &str,
    subtitle: &str,
    w: f32,
    h: f32,
    label_color: Color,
    sub_color: Color,
) -> impl ElementBuilder {
    div()
        .flex_col()
        .items_center()
        .gap(12.0)
        .child(
            div()
                .w(w)
                .h(h)
                .bg(Color::BLACK)
                .rounded(24.0)
                .flow(graph)
                .shadow(Shadow::new(0.0, 4.0, 20.0, Color::rgba(0.0, 0.0, 0.0, 0.4))),
        )
        .child(
            text(title)
                .size(20.0)
                .weight(FontWeight::Bold)
                .color(label_color),
        )
        .child(text(subtitle).size(14.0).color(sub_color))
}

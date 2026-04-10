//! @flow Shader Demo
//!
//! Demonstrates the @flow DAG-based shader system.
//! Custom GPU fragment shaders are defined in CSS via `@flow` blocks
//! and applied to elements with `flow: <name>`.
//!
//! Run with: cargo run -p blinc_app --example flow_demo --features windowed

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;
use blinc_app::windowed::WindowedContext;
use blinc_core::Color;

const STYLESHEET: &str = r#"
    /* ====== Ripple: animated concentric rings ====== */
    @flow ripple {
        target: fragment;
        input uv: builtin(uv);
        input time: builtin(time);

        node centered = uv - vec2(0.5, 0.5);
        node dist = length(centered);
        node wave = sin(dist * 30.0 - time * 4.0) * 0.5 + 0.5;
        node falloff = smoothstep(0.5, 0.0, dist);
        node intensity = wave * falloff;

        output color = vec4(intensity * 0.3, intensity * 0.6, intensity, intensity * 0.9);
    }

    /* ====== Plasma: classic plasma shader ====== */
    @flow plasma {
        target: fragment;
        input uv: builtin(uv);
        input time: builtin(time);

        node t = time * 0.8;
        node px = uv.x * 6.0;
        node py = uv.y * 6.0;
        node v1 = sin(px + t);
        node v2 = sin(py + t * 0.7);
        node v3 = sin(px + py + t * 0.5);
        node v4 = sin(length(vec2(px - 3.0, py - 3.0)) * 1.5 - t);
        node v = (v1 + v2 + v3 + v4) * 0.25 + 0.5;

        output color = vec4(
            sin(v * 3.14159) * 0.5 + 0.5,
            sin(v * 3.14159 + 2.094) * 0.5 + 0.5,
            sin(v * 3.14159 + 4.189) * 0.5 + 0.5,
            0.85
        );
    }

    /* ====== Gradient Pulse: breathing radial gradient ====== */
    @flow pulse {
        target: fragment;
        input uv: builtin(uv);
        input time: builtin(time);

        node centered = uv - vec2(0.5, 0.5);
        node dist = length(centered);
        node pulse_size = sin(time * 2.0) * 0.1 + 0.35;
        node edge = smoothstep(pulse_size + 0.05, pulse_size - 0.05, dist);

        output color = vec4(0.2 * edge, 0.8 * edge, 0.6 * edge, edge * 0.9);
    }

    /* ====== SDF Circle: distance field shape ====== */
    @flow sdf_circle {
        target: fragment;
        input uv: builtin(uv);
        input time: builtin(time);

        node p = uv * 2.0 - vec2(1.0, 1.0);
        node d = length(p) - 0.6;
        node ring = abs(d) - 0.02;
        node glow = smoothstep(0.05, 0.0, ring);
        node hue = time * 0.5;
        node r = sin(hue) * 0.5 + 0.5;
        node g = sin(hue + 2.094) * 0.5 + 0.5;
        node b = sin(hue + 4.189) * 0.5 + 0.5;

        output color = vec4(r * glow, g * glow, b * glow, glow);
    }

    #ripple-box { flow: ripple; border-radius: 16px; }
    #plasma-box { flow: plasma; border-radius: 16px; }
    #pulse-box  { flow: pulse;  border-radius: 16px; }
    #sdf-box    { flow: sdf_circle; border-radius: 16px; }
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
        title: "@flow Shader Demo".to_string(),
        width: 1000,
        height: 700,
        resizable: true,
        ..Default::default()
    };

    WindowedApp::run(config, build_ui)
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    if ctx.rebuild_count == 0 {
        ctx.add_css(STYLESHEET);
    }

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.05, 0.05, 0.08, 1.0))
        .flex_col()
        .items_center()
        .justify_center()
        .gap(20.0)
        .child(
            h1("@flow Shader Demo")
                .size(28.0)
                .weight(FontWeight::Bold)
                .color(Color::WHITE),
        )
        .child(
            text("Custom GPU shaders defined in CSS via @flow blocks")
                .size(14.0)
                .color(Color::rgba(0.6, 0.6, 0.7, 1.0)),
        )
        .child(
            scroll().h_full().w_full().child(
                div()
                    .flex_row()
                    .justify_center()
                    .gap_px(20.0)
                    .child(flow_card("ripple-box", "Ripple", "Concentric ring waves"))
                    .child(flow_card("plasma-box", "Plasma", "Classic plasma effect"))
                    .child(flow_card("pulse-box", "Pulse", "Breathing radial gradient"))
                    .child(flow_card("sdf-box", "SDF Circle", "Distance field ring")),
            ),
        )
}

fn flow_card(id: &str, title: &str, desc: &str) -> impl ElementBuilder {
    div()
        .flex_col()
        .items_center()
        .gap(8.0)
        .child(
            // The flow element — shader renders here
            div()
                .id(id)
                .w(200.0)
                .h(200.0)
                .bg(Color::rgba(0.1, 0.1, 0.15, 1.0)),
        )
        .child(
            h2(title)
                .size(16.0)
                .weight(FontWeight::Bold)
                .color(Color::WHITE),
        )
        .child(text(desc).size(12.0).color(Color::rgba(0.5, 0.5, 0.6, 1.0)))
}

//! Pointer Query Demo
//!
//! Demonstrates the CSS-driven continuous pointer query system.
//! All pointer-reactive effects are defined purely in CSS using
//! `calc(env(pointer-*))` expressions — no Rust pointer reads needed.
//!
//! The pointer query system binds cursor position to ANY numerical CSS property:
//!   opacity, corner-radius, border-width, rotate, and more.
//!
//! CSS properties used:
//!   pointer-space: self;        — enables pointer tracking
//!   pointer-origin: center;     — coordinate origin
//!   pointer-range: -1.0 1.0;   — output range
//!   pointer-smoothing: 0.08;    — exponential smoothing
//!   opacity: calc(env(pointer-*));               — hover fade
//!   border-radius: calc(env(pointer-*));         — dynamic corners
//!   border-width: calc(env(pointer-*));          — dynamic borders
//!   rotate: calc(env(pointer-*));                — subtle rotation
//!
//! Run with: cargo run -p blinc_app --example pointer_query_demo --features windowed

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_app::windowed::WindowedApp;
use blinc_core::Color;

const STYLESHEET: &str = r#"
    /* ====== Card 1: 3D Tilt ====== */
    /* Perspective rotate-x/y following cursor — true 3D card effect */
    #tilt-card {
        pointer-space: self;
        pointer-origin: center;
        pointer-range: -1.0 1.0;
        pointer-smoothing: 0.08;
        border-radius: 16px;
        background: #1e2438;
        perspective: 800px;
        rotate-y: calc(env(pointer-x) * env(pointer-inside) * 25deg);
        rotate-x: -calc(env(pointer-y) * env(pointer-inside) * -25deg);
    }

    /* ====== Card 2: Hover Reveal ====== */
    /* Fades from dim to full brightness on hover */
    #reveal-card {
        pointer-space: self;
        pointer-origin: center;
        pointer-range: -1.0 1.0;
        pointer-smoothing: 0.12;
        border-radius: 16px;
        background: #2a1a3e;
        opacity: calc(mix(0.3, 1.0, env(pointer-inside)));
    }

    /* ====== Card 3: Distance Fade ====== */
    /* Opacity increases as pointer approaches center */
    #distance-card {
        pointer-space: self;
        pointer-origin: center;
        pointer-range: -1.0 1.0;
        pointer-smoothing: 0.06;
        border-radius: 16px;
        background: #1a2e38;
        opacity: calc(smoothstep(1.8, 0.0, env(pointer-distance)));
    }

    /* ====== Card 4: Dynamic Corners ====== */
    /* Corner radius morphs continuously based on pointer proximity */
    #corners-card {
        pointer-space: self;
        pointer-origin: center;
        pointer-range: -1.0 1.0;
        pointer-smoothing: 0.08;
        border-radius: calc(mix(4, 48, smoothstep(1.4, 0.0, env(pointer-distance))) * 1px);
        background: #1e3828;
        opacity: calc(mix(0.4, 1.0, smoothstep(1.8, 0.0, env(pointer-distance))));
    }

    /* ====== Card 5: Border Glow ====== */
    /* Border grows as pointer approaches center */
    #border-card {
        pointer-space: self;
        pointer-origin: center;
        pointer-range: -1.0 1.0;
        pointer-smoothing: 0.06;
        border-radius: 16px;
        background: #1e2438;
        border-width: calc(mix(0, 4, smoothstep(1.4, 0.0, env(pointer-distance))) * 1px);
        border-color: #4488cc;
        opacity: calc(mix(0.3, 1.0, smoothstep(1.8, 0.0, env(pointer-distance))));
    }

    /* ====== Card 6: Subtle Rotation ====== */
    /* Card rotates gently following cursor x-position */
    #rotate-card {
        pointer-space: self;
        pointer-origin: center;
        pointer-range: -1.0 1.0;
        pointer-smoothing: 0.1;
        border-radius: 16px;
        background: #382a1e;
        rotate: calc(env(pointer-x) * env(pointer-inside) * 5deg);
        opacity: calc(mix(0.5, 1.0, env(pointer-inside)));
    }

    /* ====== Card 7: Combined Effects ====== */
    /* Multiple properties respond to cursor simultaneously */
    #combo-card {
        pointer-space: self;
        pointer-origin: center;
        pointer-range: -1.0 1.0;
        pointer-smoothing: 0.08;
        border-radius: calc(mix(8, 40, smoothstep(1.4, 0.0, env(pointer-distance))) * 1px);
        background: #2a1a2e;
        border-width: calc(mix(0, 3, smoothstep(1.2, 0.0, env(pointer-distance))) * 1px);
        border-color: #cc66aa;
        opacity: calc(smoothstep(1.6, 0.0, env(pointer-distance)));
        rotate: calc(env(pointer-x) * env(pointer-inside) * 3deg);
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
        title: "Pointer Query Demo".to_string(),
        width: 1200,
        height: 800,
        resizable: true,
        fullscreen: false,
        ..Default::default()
    };

    let mut css_loaded = false;

    WindowedApp::run(config, move |ctx| {
        if !css_loaded {
            ctx.add_css(STYLESHEET);
            css_loaded = true;
        }
        build_ui(ctx)
    })
}

/// Build the static UI layout. All pointer-reactive effects are CSS-driven.
pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.06, 0.06, 0.1, 1.0))
        .flex_col()
        .child(
            // Scrollable content container
            div()
                .w_full()
                .flex_grow()
                .flex_col()
                .overflow_y_scroll()
                .p(10.0)
                .gap(20.0)
                .child(
                    text("Pointer Query Demo")
                        .size(28.0)
                        .weight(FontWeight::Bold)
                        .color(Color::WHITE),
                )
                .child(
                    text("Move cursor over cards — all effects driven by CSS calc(env(pointer-*)) expressions")
                        .size(14.0)
                        .color(Color::rgba(0.6, 0.6, 0.7, 1.0)),
                )
                .child(
                    // Cards grid
                    div()
                        .w_full()
                        .flex_col()
                        .gap(20.0)
                        .child(
                            div()
                                .w_full()
                                .flex_row()
                                .gap(20.0)
                                .child(card("tilt-card", "3D Tilt", "perspective + rotate-x/y follow cursor"))
                                .child(card("reveal-card", "Hover Reveal", "opacity: mix(0.3, 1.0, pointer-inside)"))
                                .child(card("distance-card", "Distance Fade", "opacity: smoothstep(1.8, 0, distance)")),
                        )
                        .child(
                            div()
                                .w_full()
                                .flex_row()
                                .gap(20.0)
                                .child(card("corners-card", "Dynamic Corners", "border-radius: mix(4, 48, smoothstep(distance))"))
                                .child(card("border-card", "Border Glow", "border-width: mix(0, 4, smoothstep(distance))"))
                                .child(card("rotate-card", "Subtle Rotation", "rotate: pointer-x * 5deg")),
                        )
                        .child(
                            div()
                                .w_full()
                                .flex_row()
                                .gap(20.0)
                                .child(card("combo-card", "Combined", "radius + border + opacity + rotate")),
                        ),
                ),
        )
}

/// Create a card div with an ID (CSS handles all dynamic styling).
fn card(id: &str, title: &str, description: &str) -> Div {
    div()
        .id(id)
        .h(300.0)
        .w(300.0)
        .flex_col()
        .p(20.0)
        .gap(10.0)
        .child(
            text(title)
                .size(20.0)
                .weight(FontWeight::Bold)
                .color(Color::WHITE),
        )
        .child(
            text(description)
                .size(13.0)
                .color(Color::rgba(0.7, 0.8, 0.95, 1.0)),
        )
        .child(
            text(format!("#{}", id))
                .size(11.0)
                .color(Color::rgba(0.4, 0.45, 0.55, 1.0)),
        )
}

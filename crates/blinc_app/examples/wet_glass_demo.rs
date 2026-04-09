//! Wet Glass Demo
//!
//! Procedural wet-window effect with real light refraction through water drops.
//! Uses `sample_scene()` to read the background and distort it through
//! procedural Worley-noise drops, streaks, and condensation fog.
//!
//! Run with: cargo run -p blinc_app --example wet_glass_demo --features windowed

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_app::windowed::WindowedApp;
use blinc_core::Color;

const STYLESHEET: &str = r#"
    /* ====== Wet glass with real refraction ======

       Semantic step version — uses pattern-worley, effect-refract,
       effect-frost, effect-specular, and effect-fog steps to compose
       a wet-window effect with real light refraction through water drops.

       Physical model:
       - Condensation fog: semi-transparent haze (effect-fog)
       - Water drops: convex lenses that refract background (effect-refract + sample_scene)
       - Running streaks: vertical trails cleared by gravity
       - Specular highlights: bright points on curved water surfaces (effect-specular)
    */

    @flow wetglass {
        target: fragment;
        input uv: builtin(uv);
        input time: builtin(time);
        input resolution: builtin(resolution);

        /* ── Moisture field (evolves slowly) ── */
        step mist: pattern-noise { scale: 3.0; detail: 5; animation: time * 0.02; }
        node grav = smoothstep(0.0, 1.0, uv.y);
        node moist = mist * (0.35 + grav * 0.65);

        /* ── Animated wetness UVs — aspect-corrected gravity scroll ── */
        step uv1: transform-wet { speed: 0.001; }
        step uv2: transform-wet { speed: 0.0015; offset: vec2(0.38, 0.21); }
        step uv3: transform-wet { speed: 0.002;  offset: vec2(0.17, 0.63); }
        step uvs: transform-wet { speed: 0.025;  x_scale: 3.0; y_scale: 0.5; }

        /* ── Drop layers at different scales ── */
        step drops1: pattern-worley {
            uv: uv1; scale: 7.0; threshold: 0.22; edge: 0.05;
            mask: step(0.30, moist);
        }
        step drops2: pattern-worley {
            uv: uv2; scale: 12.0; threshold: 0.18; edge: 0.04;
            mask: step(0.20, moist);
        }
        step drops3: pattern-worley {
            uv: uv3; scale: 20.0; threshold: 0.13; edge: 0.03;
            mask: step(0.12, moist);
        }
        step streaks: pattern-worley {
            uv: uvs; scale: 2.0; threshold: 0.12; edge: 0.03;
            mask: step(0.28, moist) * grav;
        }

        /* Combine and sharpen to binary mask */
        node drops_raw = clamp(drops1 + drops2 * 0.6 + drops3 * 0.3
                               + streaks * 0.5, 0.0, 1.0);
        node drops = smoothstep(0.05, 0.4, drops_raw);

        /* ── Refraction: noise-based UV distortion INSIDE drops ── */
        step lens: effect-frost { strength: 0.025; mask: drops; scale: 12.0; }

        /* ── Directional light: tight specular glints on drop edges ── */
        step highlight: effect-light {
            sources: drops1, drops2, drops3, streaks;
            weights: 1.0, 0.6, 0.3, 0.5;
            angle: 225.0;
            shininess: 64.0;
            intensity: 0.25;
            mask: drops;
        }

        /* ── Scene: sample with refracted UVs where drops are ── */
        node scene = sample_scene(uv + lens);

        /* ── Composite ──
           - Fog areas (no drops): light condensation haze
           - Drop areas: clear refracted windows + subtle specular glint
        */
        node fog = (1.0 - drops) * (0.12 + mist * 0.05);
        node out_r = scene.x * (1.0 - fog) + fog + highlight;
        node out_g = scene.y * (1.0 - fog) + fog + highlight;
        node out_b = scene.z * (1.0 - fog) + fog + highlight;

        output color = vec4(out_r, out_g, out_b, 0.97);
    }

    #glass-layer {
        flow: wetglass;
        border-radius: 16px;
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
        title: "Wet Glass Demo".to_string(),
        width: 900,
        height: 650,
        resizable: true,
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

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    let w = ctx.width;
    let h = ctx.height;

    div()
        .w(w)
        .h(h)
        .bg(Color::rgba(0.08, 0.08, 0.10, 1.0))
        .items_center()
        .justify_center()
        .child(
            // Container with relative positioning for overlay stacking
            div()
                .w(800.0)
                .h(550.0)
                .relative()
                .rounded(16.0)
                // Background image
                .child(
                    img(
                        "crates/blinc_app/examples/assets/stormy-plains-illuminated-stockcake.webp",
                    )
                    .size(800.0, 550.0)
                    .cover()
                    .rounded(16.0),
                )
                // Glass overlay with rain drops flow shader
                .child(
                    div()
                        .id("glass-layer")
                        .w(800.0)
                        .h(550.0)
                        .absolute()
                        .top(0.0)
                        .left(0.0)
                        .foreground(),
                )
                // Label
                .child(
                    div()
                        .absolute()
                        .bottom(16.0)
                        .left(24.0)
                        .foreground()
                        .child(
                            text("Wet Glass")
                                .size(28.0)
                                .weight(FontWeight::Bold)
                                .color(Color::rgba(1.0, 1.0, 1.0, 0.85)),
                        )
                        .child(
                            text("transform-wet + pattern-worley + effect-frost + effect-light")
                                .size(14.0)
                                .color(Color::rgba(1.0, 1.0, 1.0, 0.55)),
                        ),
                ),
        )
}

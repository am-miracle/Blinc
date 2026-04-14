//! SVG Animation Demo
//!
//! Demonstrates SVG animation capabilities:
//! - CSS transforms on SVG elements (rotate, scale)
//! - Fill/stroke color animation via @keyframes
//! - stroke-dasharray/dashoffset line-drawing effect
//! - Path morphing (d-attribute animation)
//! - Hamburger Menus: 9 food-themed icons (morph, dash, pulse, orbit)
//!
//! Run with: cargo run -p blinc_app_examples --example svg_animation_demo --features windowed

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;
use blinc_app::windowed::WindowedContext;
use blinc_core::Color;
use blinc_theme::{ColorToken, ThemeState};

/// Simple star SVG for transform/color demos
const STAR_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <path d="M50,5 L61,35 L95,35 L68,57 L79,90 L50,70 L21,90 L32,57 L5,35 L39,35 Z" fill="#fbbf24" stroke="#f59e0b" stroke-width="2"/>
</svg>"##;

/// Circle SVG for line-drawing demo
const CIRCLE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <circle cx="50" cy="50" r="40" fill="none" stroke="#3b82f6" stroke-width="3"/>
</svg>"##;

/// Square path SVG for path morphing demo
const MORPH_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <path d="M20,20 L80,20 L80,80 L50,80 L20,80 Z" fill="#8b5cf6" stroke="#7c3aed" stroke-width="2"/>
</svg>"##;

/// Checkmark SVG for line-drawing
const CHECK_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <path d="M20,55 L40,75 L80,25" fill="none" stroke="#10b981" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/>
</svg>"##;

/// Mixed SVG with paths and circles for tag-name targeting demo
const MIXED_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <circle cx="50" cy="50" r="40" fill="#e2e8f0" stroke="#94a3b8" stroke-width="2"/>
  <path d="M30,50 L45,65 L70,35" fill="none" stroke="#475569" stroke-width="4" stroke-linecap="round" stroke-linejoin="round"/>
</svg>"##;

/// SVG with rect and circle for selective targeting
const SHAPES_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <rect x="15" y="15" width="70" height="70" rx="8" fill="#dbeafe" stroke="#3b82f6" stroke-width="2"/>
  <circle cx="50" cy="50" r="20" fill="#bfdbfe" stroke="#2563eb" stroke-width="2"/>
</svg>"##;

// ── Hamburger Menu SVGs ──────────────────────────────────────────────────────
// All icons use viewBox="0 0 100 100", single <path>, stroke-linecap="round"
// Pink/magenta (#e91e63) matching the SVGator demo aesthetic

/// Hamburger: 3 equal horizontal lines
const HAMBURGER_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <path d="M20,30 L80,30 M20,50 L80,50 M20,70 L80,70" fill="none" stroke="#e91e63" stroke-width="8" stroke-linecap="round"/>
</svg>"##;

/// Cake: 3 lines + cherry dot at top-right
const CAKE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <path d="M20,35 L80,35 M20,52 L80,52 M20,69 L80,69 M74,20 L74.01,20" fill="none" stroke="#e91e63" stroke-width="7" stroke-linecap="round"/>
</svg>"##;

/// Kebab: 3 vertical dots
const KEBAB_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <path d="M50,25 L50.01,25 M50,50 L50.01,50 M50,75 L50.01,75" fill="none" stroke="#e91e63" stroke-width="10" stroke-linecap="round"/>
</svg>"##;

/// Cheeseburger: 3 curved lines (bun shape)
const CHEESEBURGER_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <path d="M20,30 C40,22 60,22 80,30 M20,50 C40,50 60,50 80,50 M20,70 C40,78 60,78 80,70" fill="none" stroke="#e91e63" stroke-width="7" stroke-linecap="round"/>
</svg>"##;

/// Meatballs: 3 horizontal dots
const MEATBALLS_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <path d="M25,50 L25.01,50 M50,50 L50.01,50 M75,50 L75.01,50" fill="none" stroke="#e91e63" stroke-width="10" stroke-linecap="round"/>
</svg>"##;

/// Strawberry: 3 tapered lines (top shortest, bottom longest)
const STRAWBERRY_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <path d="M35,30 L65,30 M28,50 L72,50 M20,70 L80,70" fill="none" stroke="#e91e63" stroke-width="7" stroke-linecap="round"/>
</svg>"##;

/// Candy Box: 3x3 dot grid
const CANDY_BOX_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <path d="M30,30 L30.01,30 M50,30 L50.01,30 M70,30 L70.01,30 M30,50 L30.01,50 M50,50 L50.01,50 M70,50 L70.01,50 M30,70 L30.01,70 M50,70 L50.01,70 M70,70 L70.01,70" fill="none" stroke="#e91e63" stroke-width="6" stroke-linecap="round"/>
</svg>"##;

/// Hot Dog: 3 lines (bun shape — short/long/short)
const HOT_DOG_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <path d="M32,30 L68,30 M22,50 L78,50 M32,70 L68,70" fill="none" stroke="#e91e63" stroke-width="8" stroke-linecap="round"/>
</svg>"##;

/// Bento: 3x3 grid of short thick lines (rounded rectangles)
const BENTO_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="100" height="100">
  <path d="M26,30 L38,30 M44,30 L56,30 M62,30 L74,30 M26,50 L38,50 M44,50 L56,50 M62,50 L74,50 M26,70 L38,70 M44,70 L56,70 M62,70 L74,70" fill="none" stroke="#e91e63" stroke-width="8" stroke-linecap="round"/>
</svg>"##;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let config = WindowConfig {
        title: "SVG Animation Demo".to_string(),
        width: 1100,
        height: 800,
        resizable: true,
        fullscreen: false,
        ..Default::default()
    };

    WindowedApp::run(config, build_ui)
}

const SVG_CSS: &str = r#"
/* CSS Transform Animation */
@keyframes spin {
    0%   { transform: rotate(0deg); }
    100% { transform: rotate(360deg); }
}
#spin-svg {
    animation: spin 3s linear infinite;
}

@keyframes pulse-scale {
    0%   { transform: scale(1.0); }
    50%  { transform: scale(1.3); }
    100% { transform: scale(1.0); }
}
#pulse-svg {
    animation: pulse-scale 2s ease-in-out infinite;
}

/* Fill/Stroke Color Animation */
@keyframes color-cycle {
    0%   { fill: #ef4444; stroke: #dc2626; }
    33%  { fill: #3b82f6; stroke: #2563eb; }
    66%  { fill: #10b981; stroke: #059669; }
    100% { fill: #ef4444; stroke: #dc2626; }
}
#color-svg {
    animation: color-cycle 4s ease-in-out infinite;
}

@keyframes glow-stroke {
    0%   { stroke: #fbbf24; stroke-width: 2; }
    50%  { stroke: #f43f5e; stroke-width: 5; }
    100% { stroke: #fbbf24; stroke-width: 2; }
}
#glow-svg {
    animation: glow-stroke 2s ease-in-out infinite;
}

/* Stroke Dash Animation (Line Drawing) */
@keyframes draw-circle {
    0%   { stroke-dashoffset: 251; }
    100% { stroke-dashoffset: 0; }
}
#draw-circle-svg {
    stroke-dasharray: 251;
    animation: draw-circle 3s ease-in-out infinite alternate;
}

@keyframes draw-check {
    0%   { stroke-dashoffset: 100; }
    100% { stroke-dashoffset: 0; }
}
#draw-check-svg {
    stroke-dasharray: 100;
    animation: draw-check 2s ease-out infinite alternate;
}

/* Path Morphing — both shapes must have the same number of segments */
@keyframes morph-shape {
    0%   { d: path("M20,20 L80,20 L80,80 L50,80 L20,80 Z"); }
    50%  { d: path("M50,10 L90,40 L75,85 L25,85 L10,40 Z"); }
    100% { d: path("M20,20 L80,20 L80,80 L50,80 L20,80 Z"); }
}
#morph-svg {
    animation: morph-shape 3s ease-in-out infinite;
}

/* Hover transitions */
#hover-fill-svg {
    fill: #6366f1;
    transition: fill 0.3s ease;
}
#hover-fill-svg:hover {
    fill: #f43f5e;
}

#hover-stroke-svg {
    stroke: #64748b;
    stroke-width: 2;
    transition: stroke 0.3s ease, stroke-width 0.3s ease;
}
#hover-stroke-svg:hover {
    stroke: #f59e0b;
    stroke-width: 5;
}

/* Tag-name CSS selectors — target specific SVG sub-element types */
#tag-check-svg path {
    stroke: #8b5cf6;
    stroke-width: 5;
}
#tag-check-svg circle {
    fill: #f3e8ff;
    stroke: #a78bfa;
}

#tag-shapes-svg rect {
    fill: #fef3c7;
    stroke: #f59e0b;
    stroke-width: 3;
}
#tag-shapes-svg circle {
    fill: #fed7aa;
    stroke: #ea580c;
    stroke-width: 2;
}

/* ── Hamburger Menu Animations ────────────────────────────────────────── */

/* 1. Hamburger: 3 lines → X (path morph) */
@keyframes hamburger-morph {
    0%   { d: path("M20,30 L80,30 M20,50 L80,50 M20,70 L80,70"); }
    100% { d: path("M26,26 L74,74 M50,50 L50,50 M26,74 L74,26"); }
}
#hamburger-classic {
    animation: hamburger-morph 1.5s ease-in-out infinite alternate;
}

/* 2. Cake: 3 lines + cherry → X (path morph, cherry vanishes) */
@keyframes cake-morph {
    0%   { d: path("M20,35 L80,35 M20,52 L80,52 M20,69 L80,69 M74,20 L74.01,20"); }
    100% { d: path("M26,26 L74,74 M50,50 L50,50 M26,74 L74,26 M50,50 L50,50"); }
}
#hamburger-cake {
    animation: cake-morph 1.5s ease-in-out infinite alternate;
}

/* 3. Kebab: staggered dot pulsation (wave pattern via path morph) */
@keyframes kebab-stagger {
    0%   { d: path("M46,25 L54,25 M50,50 L50.01,50 M50,75 L50.01,75"); }
    33%  { d: path("M50,25 L50.01,25 M46,50 L54,50 M50,75 L50.01,75"); }
    66%  { d: path("M50,25 L50.01,25 M50,50 L50.01,50 M46,75 L54,75"); }
    100% { d: path("M46,25 L54,25 M50,50 L50.01,50 M50,75 L50.01,75"); }
}
#hamburger-kebab {
    animation: kebab-stagger 1.2s ease-in-out infinite;
}

/* 4. Cheeseburger: curved bun lines → X (cubic path morph) */
@keyframes cheese-morph {
    0%   { d: path("M20,30 C40,22 60,22 80,30 M20,50 C40,50 60,50 80,50 M20,70 C40,78 60,78 80,70"); }
    100% { d: path("M26,26 C42,42 58,58 74,74 M50,50 C50,50 50,50 50,50 M26,74 C42,58 58,42 74,26"); }
}
#hamburger-cheese {
    animation: cheese-morph 1.5s ease-in-out infinite alternate;
}

/* 5. Meatballs: 2 outer dots orbit around center (multi-stop path morph) */
@keyframes meatball-orbit {
    0%   { d: path("M25,50 L25.01,50 M50,50 L50.01,50 M75,50 L75.01,50"); }
    25%  { d: path("M50,25 L50.01,25 M50,50 L50.01,50 M50,75 L50.01,75"); }
    50%  { d: path("M75,50 L75.01,50 M50,50 L50.01,50 M25,50 L25.01,50"); }
    75%  { d: path("M50,75 L50.01,75 M50,50 L50.01,50 M50,25 L50.01,25"); }
    100% { d: path("M25,50 L25.01,50 M50,50 L50.01,50 M75,50 L75.01,50"); }
}
#hamburger-meatball {
    animation: meatball-orbit 2s linear infinite;
}

/* 6. Strawberry: tapered lines → X (path morph) */
@keyframes strawberry-morph {
    0%   { d: path("M35,30 L65,30 M28,50 L72,50 M20,70 L80,70"); }
    100% { d: path("M28,28 L72,72 M50,50 L50,50 M28,72 L72,28"); }
}
#hamburger-strawberry {
    animation: strawberry-morph 1.5s ease-in-out infinite alternate;
}

/* 7. Candy Box: 3x3 dot grid → dots rearrange to X pattern (path morph) */
@keyframes candy-morph {
    0%   { d: path("M30,30 L30.01,30 M50,30 L50.01,30 M70,30 L70.01,30 M30,50 L30.01,50 M50,50 L50.01,50 M70,50 L70.01,50 M30,70 L30.01,70 M50,70 L50.01,70 M70,70 L70.01,70"); }
    100% { d: path("M26,26 L26.01,26 M62,38 L62.01,38 M74,26 L74.01,26 M38,38 L38.01,38 M50,50 L50.01,50 M62,62 L62.01,62 M26,74 L26.01,74 M38,62 L38.01,62 M74,74 L74.01,74"); }
}
#hamburger-candy {
    animation: candy-morph 1.5s ease-in-out infinite alternate;
}

/* 8. Hot Dog: 3 bun-shaped lines → X (path morph) */
@keyframes hotdog-morph {
    0%   { d: path("M32,30 L68,30 M22,50 L78,50 M32,70 L68,70"); }
    100% { d: path("M26,26 L74,74 M50,50 L50,50 M26,74 L74,26"); }
}
#hamburger-hotdog {
    animation: hotdog-morph 1.5s ease-in-out infinite alternate;
}

/* 9. Bento: 3x3 short line grid → dots rearrange to X pattern (path morph) */
@keyframes bento-morph {
    0%   { d: path("M26,30 L38,30 M44,30 L56,30 M62,30 L74,30 M26,50 L38,50 M44,50 L56,50 M62,50 L74,50 M26,70 L38,70 M44,70 L56,70 M62,70 L74,70"); }
    100% { d: path("M26,26 L26.01,26 M62,38 L62.01,38 M74,26 L74.01,26 M38,38 L38.01,38 M50,50 L50.01,50 M62,62 L62.01,62 M26,74 L26.01,74 M38,62 L38.01,62 M74,74 L74.01,74"); }
}
#hamburger-bento {
    animation: bento-morph 1.5s ease-in-out infinite alternate;
}
"#;

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    // Register CSS on the first frame. `add_css` is idempotent for
    // identical sources (the stylesheet deduplicates), but guard with
    // `rebuild_count` to avoid re-parsing every frame.
    if ctx.rebuild_count == 0 {
        ctx.add_css(SVG_CSS);
    }

    let theme = ThemeState::get();
    let bg = theme.color(ColorToken::Background);

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(bg)
        .flex_col()
        .child(header())
        .child(
            scroll().w_full().h(ctx.height - 60.0).child(
                div()
                    .w_full()
                    .p(24.0)
                    .flex_col()
                    .gap(32.0)
                    .child(transform_section())
                    .child(color_section())
                    .child(line_drawing_section())
                    .child(morph_section())
                    .child(hover_section())
                    .child(tag_selector_section())
                    .child(hamburger_section()),
            ),
        )
}

fn header() -> Div {
    let theme = ThemeState::get();

    div()
        .w_full()
        .h(60.0)
        .bg(theme.color(ColorToken::Surface))
        .flex_row()
        .items_center()
        .px(24.0)
        .child(
            text("SVG Animation Demo")
                .size(24.0)
                .weight(FontWeight::Bold)
                .color(theme.color(ColorToken::TextPrimary)),
        )
}

fn section_card(title: &str, subtitle: &str) -> Div {
    let theme = ThemeState::get();

    div()
        .w_full()
        .bg(theme.color(ColorToken::Surface))
        .rounded(12.0)
        .p(20.0)
        .flex_col()
        .gap(16.0)
        .child(
            div()
                .flex_col()
                .gap(4.0)
                .child(
                    text(title)
                        .size(18.0)
                        .weight(FontWeight::SemiBold)
                        .color(theme.color(ColorToken::TextPrimary)),
                )
                .child(
                    text(subtitle)
                        .size(13.0)
                        .color(theme.color(ColorToken::TextSecondary)),
                ),
        )
}

fn demo_cell(label: &str, svg_content: &str, id: &str, size: f32) -> Div {
    let theme = ThemeState::get();

    div()
        .flex_col()
        .items_center()
        .gap(8.0)
        .child(
            div()
                .w(size + 20.0)
                .h(size + 20.0)
                .rounded(8.0)
                .bg(Color::rgba(0.0, 0.0, 0.0, 0.2))
                .flex()
                .items_center()
                .justify_center()
                .child(svg(svg_content).square(size).id(id)),
        )
        .child(
            text(label)
                .size(12.0)
                .color(theme.color(ColorToken::TextSecondary)),
        )
}

/// CSS transforms on SVGs
fn transform_section() -> Div {
    section_card(
        "CSS Transforms",
        "rotate() and scale() animations applied directly to SVG elements",
    )
    .child(
        div()
            .flex_row()
            .gap(48.0)
            .justify_center()
            .child(demo_cell("Spin (rotate)", STAR_SVG, "spin-svg", 80.0))
            .child(demo_cell("Pulse (scale)", STAR_SVG, "pulse-svg", 80.0)),
    )
}

/// Fill/stroke color animation
fn color_section() -> Div {
    section_card(
        "Fill & Stroke Animation",
        "fill and stroke colors animated via @keyframes",
    )
    .child(
        div()
            .flex_row()
            .gap(48.0)
            .justify_center()
            .child(demo_cell("Color Cycle", STAR_SVG, "color-svg", 80.0))
            .child(demo_cell("Glow Stroke", STAR_SVG, "glow-svg", 80.0)),
    )
}

/// Stroke dash animation (line drawing)
fn line_drawing_section() -> Div {
    section_card(
        "Line Drawing Effect",
        "stroke-dasharray + stroke-dashoffset animation for SVG line drawing",
    )
    .child(
        div()
            .flex_row()
            .gap(48.0)
            .justify_center()
            .child(demo_cell(
                "Draw Circle",
                CIRCLE_SVG,
                "draw-circle-svg",
                80.0,
            ))
            .child(demo_cell("Draw Check", CHECK_SVG, "draw-check-svg", 80.0)),
    )
}

/// Path morphing
fn morph_section() -> Div {
    section_card(
        "Path Morphing",
        "d: path() animation morphs between shapes via cubic bezier interpolation",
    )
    .child(div().flex_row().gap(48.0).justify_center().child(demo_cell(
        "Square \u{2194} Pentagon",
        MORPH_SVG,
        "morph-svg",
        80.0,
    )))
}

/// Hover transitions
fn hover_section() -> Div {
    section_card(
        "Hover Transitions",
        "CSS transitions on fill and stroke (hover to see effect)",
    )
    .child(
        div()
            .flex_row()
            .gap(48.0)
            .justify_center()
            .child(demo_cell(
                "Fill Transition",
                STAR_SVG,
                "hover-fill-svg",
                80.0,
            ))
            .child(demo_cell(
                "Stroke Transition",
                STAR_SVG,
                "hover-stroke-svg",
                80.0,
            )),
    )
}

/// Tag-name CSS selectors targeting SVG sub-elements
fn tag_selector_section() -> Div {
    section_card(
        "Tag-Name CSS Selectors",
        "CSS selectors like `path { }` and `circle { }` target specific SVG sub-element types",
    )
    .child(
        div()
            .flex_row()
            .gap(48.0)
            .justify_center()
            .child(demo_cell(
                "path=purple, circle=lilac",
                MIXED_SVG,
                "tag-check-svg",
                80.0,
            ))
            .child(demo_cell(
                "rect=amber, circle=orange",
                SHAPES_SVG,
                "tag-shapes-svg",
                80.0,
            )),
    )
}

/// Hamburger Menu Animations — 9 food-themed icons from SVGator demo
fn hamburger_section() -> Div {
    section_card(
        "Hamburger Menu Animations",
        "9 animated menu icons using path morphing, stroke-dash, transforms, and color transitions",
    )
    .child(
        div()
            .flex_row()
            .flex_wrap()
            .gap(32.0)
            .justify_center()
            .child(demo_cell(
                "Hamburger",
                HAMBURGER_SVG,
                "hamburger-classic",
                80.0,
            ))
            .child(demo_cell("Cake", CAKE_SVG, "hamburger-cake", 80.0))
            .child(demo_cell("Kebab", KEBAB_SVG, "hamburger-kebab", 80.0))
            .child(demo_cell(
                "Cheeseburger",
                CHEESEBURGER_SVG,
                "hamburger-cheese",
                80.0,
            ))
            .child(demo_cell(
                "Meatballs",
                MEATBALLS_SVG,
                "hamburger-meatball",
                80.0,
            ))
            .child(demo_cell(
                "Strawberry",
                STRAWBERRY_SVG,
                "hamburger-strawberry",
                80.0,
            ))
            .child(demo_cell(
                "Candy Box",
                CANDY_BOX_SVG,
                "hamburger-candy",
                80.0,
            ))
            .child(demo_cell("Hot Dog", HOT_DOG_SVG, "hamburger-hotdog", 80.0))
            .child(demo_cell("Bento", BENTO_SVG, "hamburger-bento", 80.0)),
    )
}

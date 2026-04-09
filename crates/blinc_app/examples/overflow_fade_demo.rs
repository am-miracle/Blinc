//! Overflow Fade Demo
//!
//! Demonstrates the `overflow-fade` CSS property which applies smooth alpha
//! fading at overflow clip edges instead of hard clipping.
//!
//! Supports:
//! - Uniform fade: `overflow-fade: 24px` (all edges)
//! - Vertical/horizontal: `overflow-fade: 24px 0px` (top/bottom only)
//! - Per-edge: `overflow-fade: 24px 0px 24px 0px`
//! - CSS transitions and @keyframes animation
//! - Works with scroll containers
//!
//! Run with: cargo run -p blinc_app --example overflow_fade_demo --features windowed

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_app::windowed::WindowedApp;
use blinc_core::Color;
use blinc_theme::{ColorToken, ThemeState};

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let config = WindowConfig {
        title: "Blinc Overflow Fade Demo".to_string(),
        width: 900,
        height: 750,
        resizable: true,
        fullscreen: false,
        ..Default::default()
    };

    let mut css_loaded = false;

    WindowedApp::run(config, move |ctx| {
        if !css_loaded {
            ctx.add_css(
                r#"
            /* Fade transition on hover */
            #fade-hover-container {
                overflow: clip;
                overflow-fade: 0px;
                border-radius: 12px;
                transition: overflow-fade 500ms ease;
            }
            #fade-hover-container:hover {
                overflow-fade: 32px;
            }

            /* Animated fade via @keyframes */
            @keyframes fade-breathe {
                0% { overflow-fade: 0px; }
                50% { overflow-fade: 40px; }
                100% { overflow-fade: 0px; }
            }
            #fade-anim-container {
                overflow: clip;
                border-radius: 12px;
                animation: fade-breathe 3000ms ease-in-out infinite;
            }
            "#,
            );
            css_loaded = true;
        }
        build_ui(ctx)
    })
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    let theme = ThemeState::get();
    let bg = theme.color(ColorToken::Background);

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(bg)
        .flex_col()
        .child(header())
        .child(
            scroll().w_full().h(ctx.height - 72.0).child(
                div()
                    .w_full()
                    .p(theme.spacing().space_6)
                    .flex_col()
                    .gap(theme.spacing().space_8)
                    .child(vertical_fade_section())
                    .child(horizontal_fade_section())
                    .child(per_edge_fade_section())
                    .child(scroll_fade_section())
                    .child(css_transition_section())
                    .child(css_animation_section()),
            ),
        )
}

fn header() -> impl ElementBuilder {
    let theme = ThemeState::get();
    let surface = theme.color(ColorToken::Surface);
    let text_primary = theme.color(ColorToken::TextPrimary);
    let text_secondary = theme.color(ColorToken::TextSecondary);
    let border = theme.color(ColorToken::Border);

    div()
        .w_full()
        .h(72.0)
        .bg(surface)
        .border_bottom(1.0, border)
        .flex_row()
        .items_center()
        .justify_center()
        .gap(16.0)
        .child(
            text("Overflow Fade")
                .size(24.0)
                .weight(FontWeight::Bold)
                .color(text_primary),
        )
        .child(
            text("Smooth alpha fade at clip edges")
                .size(14.0)
                .color(text_secondary),
        )
}

// ============================================================================
// Helpers
// ============================================================================

fn section_container() -> Div {
    let theme = ThemeState::get();
    let surface = theme.color(ColorToken::Surface);
    let border = theme.color(ColorToken::Border);

    div()
        .w_full()
        .bg(surface)
        .border(1.0, border)
        .rounded(12.0)
        .p(24.0)
        .flex_col()
        .gap(16.0)
}

fn section_title(title: &str) -> impl ElementBuilder {
    let theme = ThemeState::get();
    text(title)
        .size(20.0)
        .weight(FontWeight::SemiBold)
        .color(theme.color(ColorToken::TextPrimary))
}

fn section_desc(desc: &str) -> impl ElementBuilder {
    let theme = ThemeState::get();
    text(desc)
        .size(14.0)
        .color(theme.color(ColorToken::TextSecondary))
}

fn code_label(label: &str) -> impl ElementBuilder {
    inline_code(label).size(12.0)
}

/// Generate colored content rows for demonstrating clipping
fn color_rows(count: usize) -> Div {
    let colors = [
        Color::rgba(0.23, 0.51, 0.96, 1.0), // blue
        Color::rgba(0.13, 0.77, 0.37, 1.0), // green
        Color::rgba(0.66, 0.33, 0.97, 1.0), // purple
        Color::rgba(0.98, 0.45, 0.09, 1.0), // orange
        Color::rgba(0.93, 0.27, 0.27, 1.0), // red
        Color::rgba(0.08, 0.71, 0.83, 1.0), // cyan
        Color::rgba(0.85, 0.53, 0.05, 1.0), // amber
        Color::rgba(0.55, 0.22, 0.80, 1.0), // violet
    ];
    let mut container = div().w_full().flex_col().gap(4.0);
    for i in 0..count {
        let c = colors[i % colors.len()];
        container = container.child(
            div()
                .w_full()
                .h(36.0)
                .rounded(6.0)
                .bg(c)
                .flex_col()
                .justify_center()
                .items_center()
                .child(
                    text(format!("Item {}", i + 1))
                        .size(13.0)
                        .color(Color::WHITE),
                ),
        );
    }
    container
}

/// Helper to create a dark clip container with consistent styling
fn clip_container(w: f32, h: f32) -> Div {
    div()
        .w(w)
        .h(h)
        .overflow_clip()
        .rounded(8.0)
        .bg(Color::rgba(0.06, 0.09, 0.16, 1.0))
        .flex_col()
        .p(4.0)
}

// ============================================================================
// Vertical Fade
// ============================================================================

fn vertical_fade_section() -> impl ElementBuilder {
    section_container()
        .child(section_title("Vertical Fade"))
        .child(section_desc(
            "overflow-fade-y fades the top and bottom edges. Content scrolls under the fade smoothly.",
        ))
        .child(
            div()
                .w_full()
                .flex_row()
                .gap(24.0)
                .flex_wrap()
                // No fade (reference)
                .child(
                    div()
                        .flex_col()
                        .gap(8.0)
                        .child(code_label("No fade (hard clip)"))
                        .child(clip_container(200.0, 180.0).child(color_rows(8))),
                )
                // 16px fade
                .child(
                    div()
                        .flex_col()
                        .gap(8.0)
                        .child(code_label(".overflow_fade_y(16.0)"))
                        .child(
                            clip_container(200.0, 180.0)
                                .overflow_fade_y(16.0)
                                .child(color_rows(8)),
                        ),
                )
                // 32px fade
                .child(
                    div()
                        .flex_col()
                        .gap(8.0)
                        .child(code_label(".overflow_fade_y(32.0)"))
                        .child(
                            clip_container(200.0, 180.0)
                                .overflow_fade_y(32.0)
                                .child(color_rows(8)),
                        ),
                )
                // 48px fade
                .child(
                    div()
                        .flex_col()
                        .gap(8.0)
                        .child(code_label(".overflow_fade_y(48.0)"))
                        .child(
                            clip_container(200.0, 180.0)
                                .overflow_fade_y(48.0)
                                .child(color_rows(8)),
                        ),
                ),
        )
}

// ============================================================================
// Horizontal Fade
// ============================================================================

fn horizontal_fade_section() -> impl ElementBuilder {
    section_container()
        .child(section_title("Horizontal Fade"))
        .child(section_desc(
            "overflow-fade-x fades the left and right edges of a clip container.",
        ))
        .child(
            div()
                .flex_row()
                .gap(24.0)
                .flex_wrap()
                .child(
                    div()
                        .flex_col()
                        .gap(8.0)
                        .child(code_label(".overflow_fade_x(24.0)"))
                        .child(
                            div()
                                .w(280.0)
                                .h(60.0)
                                .overflow_clip()
                                .overflow_fade_x(24.0)
                                .rounded(8.0)
                                .bg(Color::rgba(0.06, 0.09, 0.16, 1.0))
                                .p(4.0)
                                .flex_row()
                                .gap(6.0)
                                .child(horizontal_chips(12)),
                        ),
                )
                .child(
                    div()
                        .flex_col()
                        .gap(8.0)
                        .child(code_label(".overflow_fade_x(48.0)"))
                        .child(
                            div()
                                .w(280.0)
                                .h(60.0)
                                .overflow_clip()
                                .overflow_fade_x(48.0)
                                .rounded(8.0)
                                .bg(Color::rgba(0.06, 0.09, 0.16, 1.0))
                                .p(4.0)
                                .flex_row()
                                .gap(6.0)
                                .child(horizontal_chips(12)),
                        ),
                ),
        )
}

fn horizontal_chips(count: usize) -> Div {
    let colors = [
        Color::rgba(0.23, 0.51, 0.96, 1.0),
        Color::rgba(0.13, 0.77, 0.37, 1.0),
        Color::rgba(0.66, 0.33, 0.97, 1.0),
        Color::rgba(0.98, 0.45, 0.09, 1.0),
        Color::rgba(0.93, 0.27, 0.27, 1.0),
        Color::rgba(0.08, 0.71, 0.83, 1.0),
    ];
    let mut row = div().flex_row().gap(6.0);
    for i in 0..count {
        let c = colors[i % colors.len()];
        row = row.child(
            div()
                .w(56.0)
                .h(36.0)
                .rounded(6.0)
                .bg(c)
                .flex_col()
                .justify_center()
                .items_center()
                .child(text(format!("{}", i + 1)).size(12.0).color(Color::WHITE)),
        );
    }
    row
}

// ============================================================================
// Per-Edge Fade
// ============================================================================

fn per_edge_fade_section() -> impl ElementBuilder {
    section_container()
        .child(section_title("Per-Edge Fade"))
        .child(section_desc(
            "Different fade distances per edge (top, right, bottom, left).",
        ))
        .child(
            div()
                .flex_row()
                .gap(24.0)
                .flex_wrap()
                // Bottom only
                .child(
                    div()
                        .flex_col()
                        .gap(8.0)
                        .child(code_label("edges(0, 0, 32, 0) — bottom only"))
                        .child(
                            clip_container(200.0, 160.0)
                                .overflow_fade_edges(0.0, 0.0, 32.0, 0.0)
                                .child(color_rows(8)),
                        ),
                )
                // Top only
                .child(
                    div()
                        .flex_col()
                        .gap(8.0)
                        .child(code_label("edges(32, 0, 0, 0) — top only"))
                        .child(
                            clip_container(200.0, 160.0)
                                .overflow_fade_edges(32.0, 0.0, 0.0, 0.0)
                                .child(color_rows(8)),
                        ),
                )
                // All different
                .child(
                    div()
                        .flex_col()
                        .gap(8.0)
                        .child(code_label("edges(8, 24, 40, 24) — asymmetric"))
                        .child(
                            clip_container(200.0, 160.0)
                                .overflow_fade_edges(8.0, 24.0, 40.0, 24.0)
                                .child(color_rows(8)),
                        ),
                ),
        )
}

// ============================================================================
// Scroll + Fade
// ============================================================================

fn scroll_fade_section() -> impl ElementBuilder {
    section_container()
        .child(section_title("Scroll Container with Fade"))
        .child(section_desc(
            "overflow-fade works with scroll containers. Scroll to see content fade in/out at edges.",
        ))
        .child(
            div()
                .flex_row()
                .gap(24.0)
                .flex_wrap()
                // Scroll with vertical fade
                .child(
                    div()
                        .flex_col()
                        .gap(8.0)
                        .child(code_label("scroll() + overflow_fade_y(24.0)"))
                        .child(
                            scroll()
                                .w(220.0)
                                .h(200.0)
                                .overflow_fade_y(24.0)
                                .rounded(8.0)
                                .bg(Color::rgba(0.06, 0.09, 0.16, 1.0))
                                .child(
                                    div().w_full().p(4.0).flex_col().child(color_rows(16)),
                                ),
                        ),
                )
                // Scroll with uniform fade
                .child(
                    div()
                        .flex_col()
                        .gap(8.0)
                        .child(code_label("scroll() + overflow_fade(32.0)"))
                        .child(
                            scroll()
                                .w(220.0)
                                .h(200.0)
                                .overflow_fade(32.0)
                                .rounded(8.0)
                                .bg(Color::rgba(0.06, 0.09, 0.16, 1.0))
                                .child(
                                    div().w_full().p(4.0).flex_col().child(color_rows(16)),
                                ),
                        ),
                ),
        )
}

// ============================================================================
// CSS Transition
// ============================================================================

fn css_transition_section() -> impl ElementBuilder {
    section_container()
        .child(section_title("CSS Transition"))
        .child(section_desc(
            "overflow-fade with CSS transitions. Hover to smoothly fade-in the clip edges.",
        ))
        .child(
            div()
                .flex_col()
                .gap(8.0)
                .child(code_label(
                    "#fade-hover-container { overflow-fade: 0px; transition: overflow-fade 500ms; } :hover { overflow-fade: 32px; }",
                ))
                .child(
                    div()
                        .id("fade-hover-container")
                        .w(300.0)
                        .h(180.0)
                        .bg(Color::rgba(0.06, 0.09, 0.16, 1.0))
                        .flex_col()
                        .p(4.0)
                        .child(color_rows(8)),
                ),
        )
}

// ============================================================================
// CSS Animation
// ============================================================================

fn css_animation_section() -> impl ElementBuilder {
    section_container()
        .child(section_title("CSS @keyframes Animation"))
        .child(section_desc(
            "overflow-fade animated via @keyframes. The fade distance breathes from 0 to 40px.",
        ))
        .child(
            div()
                .flex_col()
                .gap(8.0)
                .child(code_label(
                    "@keyframes fade-breathe { 0% { 0px } 50% { 40px } 100% { 0px } }",
                ))
                .child(
                    div()
                        .id("fade-anim-container")
                        .w(300.0)
                        .h(180.0)
                        .bg(Color::rgba(0.06, 0.09, 0.16, 1.0))
                        .flex_col()
                        .p(4.0)
                        .child(color_rows(8)),
                ),
        )
}

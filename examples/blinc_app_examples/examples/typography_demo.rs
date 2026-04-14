//! Typography Demo
//!
//! This example demonstrates typography helpers:
//! - Headings: h1-h6, heading()
//! - Inline text: b, span, small, label, muted, p, caption, inline_code
//! - Font families: system, monospace, serif, sans_serif, custom fonts
//!
//! For table examples, see `table_demo.rs`
//!
//! Run with: cargo run -p blinc_app_examples --example typography_demo --features windowed

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_core::Color;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let config = WindowConfig {
        title: "Typography Demo".to_string(),
        width: 900,
        height: 700,
        resizable: true,
        ..Default::default()
    };

    blinc_app::windowed::WindowedApp::run(config, build_ui)
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.08, 0.08, 0.1, 1.0))
        .flex_col()
        .gap(8.0)
        .p(24.0)
        .child(
            scroll()
                .w_full()
                .h_full()
                .direction(ScrollDirection::Vertical)
                .child(
                    div()
                        .w_full() // Constrain width to scroll viewport for text wrapping
                        .flex_col()
                        .gap(32.0)
                        .p(8.0)
                        .child(typography_section())
                        .child(text_decorations_section())
                        .child(blockquote_section())
                        .child(baseline_alignment_section())
                        .child(inline_text_section())
                        .child(font_family_section()),
                ),
        )
}

/// Demonstrates heading helpers h1-h6
fn typography_section() -> Div {
    div()
        .flex_col()
        .gap(12.0)
        .child(
            div()
                .flex_col()
                .gap(4.0)
                .child(h1("Typography Helpers").color(Color::WHITE))
                .child(muted("Semantic text elements with sensible defaults")),
        )
        .child(
            div()
                .bg(Color::rgba(0.12, 0.12, 0.15, 1.0))
                .rounded(8.0)
                .p(16.0)
                .flex_col()
                .gap(2.0)
                .child(h1("Heading 1 (32px, bold)").color(Color::WHITE))
                .child(h2("Heading 2 (24px, bold)").color(Color::WHITE))
                .child(h3("Heading 3 (20px, semibold)").color(Color::WHITE))
                .child(h4("Heading 4 (18px, semibold)").color(Color::WHITE))
                .child(h5("Heading 5 (16px, medium)").color(Color::WHITE))
                .child(h6("Heading 6 (14px, medium)").color(Color::WHITE)),
        )
        .child(
            div()
                .bg(Color::rgba(0.12, 0.12, 0.15, 1.0))
                .rounded(8.0)
                .p(16.0)
                .flex_col()
                .gap(8.0)
                .child(h4("Dynamic Heading Level").color(Color::WHITE))
                .child(
                    div()
                        .flex_row()
                        .items_baseline()
                        .border(1.5, Color::from_hex(0x66B2FF))
                        .gap(16.0)
                        .child(heading(1, "Level 1").color(Color::from_hex(0x66B2FF)))
                        .child(heading(3, "Level 3").color(Color::from_hex(0x66B2FF)))
                        .child(heading(5, "Level 5").color(Color::from_hex(0x66B2FF))),
                ),
        )
}

/// Demonstrates text decorations (strikethrough and underline)
fn text_decorations_section() -> Div {
    div()
        .w_full()
        .flex_col()
        .gap(4.0)
        .child(h2("Text Decorations").color(Color::WHITE))
        .child(
            div()
                .bg(Color::rgba(0.12, 0.12, 0.15, 1.0))
                .rounded(8.0)
                .p(16.0)
                .flex_col()
                .gap(4.0)
                // Strikethrough
                .child(label(".strikethrough():").color(Color::GRAY))
                .child(
                    div()
                        .flex_row()
                        .gap(16.0)
                        .items_baseline()
                        .child(text("Regular text").color(Color::WHITE))
                        .child(
                            text("Strikethrough text")
                                .strikethrough()
                                .color(Color::WHITE),
                        )
                        .child(text("More regular").color(Color::WHITE)),
                )
                // Underline
                .child(label(".underline():").color(Color::GRAY))
                .child(
                    div()
                        .flex_row()
                        .gap(16.0)
                        .items_baseline()
                        .child(text("Regular text").color(Color::WHITE))
                        .child(
                            text("Underlined text")
                                .underline()
                                .color(Color::from_hex(0x66B2FF)),
                        )
                        .child(text("More regular").color(Color::WHITE)),
                )
                // Both decorations
                .child(label("Combined decorations:").color(Color::GRAY))
                .child(
                    div().flex_row().gap(16.0).items_baseline().child(
                        text("Both decorations")
                            .strikethrough()
                            .underline()
                            .italic()
                            .color(Color::from_hex(0xFF6666)),
                    ),
                )
                // Different sizes
                .child(label("Different font sizes:").color(Color::GRAY))
                .child(
                    div()
                        .flex_row()
                        .gap(16.0)
                        .items_baseline()
                        .child(text("12px").size(12.0).strikethrough().color(Color::WHITE))
                        .child(text("16px").size(16.0).strikethrough().color(Color::WHITE))
                        .child(text("24px").size(24.0).strikethrough().color(Color::WHITE))
                        .child(text("32px").size(32.0).strikethrough().color(Color::WHITE)),
                ),
        )
}

/// Demonstrates blockquote-style borders
fn blockquote_section() -> Div {
    div()
        .w_full()
        .flex_col()
        .gap(12.0)
        .child(h2("Blockquote Borders").color(Color::WHITE))
        .child(
            div()
                .bg(Color::rgba(0.12, 0.12, 0.15, 1.0))
                .rounded(8.0)
                .p(16.0)
                .flex_col()
                .gap(16.0)
                // Left border only (blockquote style)
                .child(label(".border_left():").color(Color::GRAY))
                .child(
                    div()
                        .border_left(4.0, Color::from_hex(0x66B2FF))
                        .bg(Color::rgba(0.15, 0.15, 0.2, 1.0))
                        .p(12.0)
                        .child(
                            p("This is a blockquote with a left border. It uses border_left() to achieve the classic blockquote appearance.")
                                .color(Color::from_hex(0xCCCCCC)),
                        ),
                )
                // All individual borders
                .child(label(".border_left/right/top/bottom():").color(Color::GRAY))
                .child(
                    div()
                        .border_left(3.0, Color::from_hex(0xFF6666))
                        .border_right(3.0, Color::from_hex(0x66FF66))
                        .border_top(3.0, Color::from_hex(0x6666FF))
                        .border_bottom(3.0, Color::from_hex(0xFFFF66))
                        .bg(Color::rgba(0.15, 0.15, 0.2, 1.0))
                        .p(12.0)
                        .child(
                            p("Each border side can have different colors and widths.")
                                .color(Color::WHITE),
                        ),
                )
                // Horizontal borders only
                .child(label(".border_y():").color(Color::GRAY))
                .child(
                    div()
                        .border_y(2.0, Color::from_hex(0x66B2FF))
                        .bg(Color::rgba(0.15, 0.15, 0.2, 1.0))
                        .p(12.0)
                        .child(
                            p("Top and bottom borders using border_y()")
                                .color(Color::WHITE),
                        ),
                )
                // Vertical borders only
                .child(label(".border_x():").color(Color::GRAY))
                .child(
                    div()
                        .border_x(2.0, Color::from_hex(0x66FF99))
                        .bg(Color::rgba(0.15, 0.15, 0.2, 1.0))
                        .p(12.0)
                        .child(
                            p("Left and right borders using border_x()")
                                .color(Color::WHITE),
                        ),
                ),
        )
}

/// Demonstrates baseline alignment with varying text sizes
fn baseline_alignment_section() -> Div {
    div()
        .w_full()
        .flex_col()
        .gap(12.0)
        .child(h2("Baseline Alignment").color(Color::WHITE))
        .child(
            div()
                .bg(Color::rgba(0.12, 0.12, 0.15, 1.0))
                .rounded(8.0)
                .p(16.0)
                .flex_col()
                .gap(12.0)
                // Row with varying text sizes - all using v_baseline()
                .child(label("Texts with different sizes using .v_baseline():").color(Color::GRAY))
                .child(
                    div()
                        .flex_row()
                        .items_baseline()
                        .gap(8.0)
                        .border(1.0, Color::from_hex(0x4488FF))
                        .child(text("32px").size(32.0).v_baseline().color(Color::WHITE))
                        .child(
                            text("24px")
                                .size(24.0)
                                .v_baseline()
                                .color(Color::from_hex(0x66B2FF)),
                        )
                        .child(text("18px").size(18.0).v_baseline().color(Color::WHITE))
                        .child(
                            text("14px")
                                .size(14.0)
                                .v_baseline()
                                .color(Color::from_hex(0x66B2FF)),
                        )
                        .child(text("12px").size(12.0).v_baseline().color(Color::WHITE)),
                )
                // Row with mixed fonts - same size
                .child(
                    label("Mixed fonts at same size (14px) with .v_baseline():").color(Color::GRAY),
                )
                .child(
                    div()
                        .flex_row()
                        .items_baseline()
                        .gap(8.0)
                        .border(1.0, Color::from_hex(0x44FF88))
                        .child(text("System font").v_baseline().color(Color::WHITE))
                        .child(
                            text("Monospace")
                                .monospace()
                                .v_baseline()
                                .color(Color::from_hex(0x98C379)),
                        )
                        .child(text("Serif font").serif().v_baseline().color(Color::WHITE))
                        .child(
                            text("Sans-serif")
                                .sans_serif()
                                .v_baseline()
                                .color(Color::from_hex(0x98C379)),
                        ),
                )
                // Row WITHOUT v_baseline for comparison
                .child(
                    label("Same texts WITHOUT .v_baseline() (default Top alignment):")
                        .color(Color::GRAY),
                )
                .child(
                    div()
                        .flex_row()
                        .items_baseline()
                        .gap(8.0)
                        .border(1.0, Color::from_hex(0xFF4444))
                        .child(text("32px").size(32.0).color(Color::WHITE))
                        .child(text("24px").size(24.0).color(Color::from_hex(0xFF6666)))
                        .child(text("18px").size(18.0).color(Color::WHITE))
                        .child(text("14px").size(14.0).color(Color::from_hex(0xFF6666)))
                        .child(text("12px").size(12.0).color(Color::WHITE)),
                )
                .child(
                    label("Mixed fonts at same size (14px) without .v_baseline():")
                        .color(Color::GRAY),
                )
                .child(
                    div()
                        .flex_row()
                        .items_baseline()
                        .gap(8.0)
                        .border(1.0, Color::from_hex(0x44FF88))
                        .child(text("System font").color(Color::WHITE))
                        .child(
                            text("Monospace")
                                .monospace()
                                .color(Color::from_hex(0x98C379)),
                        )
                        .child(text("Serif font").serif().color(Color::WHITE))
                        .child(
                            text("Sans-serif")
                                .sans_serif()
                                .color(Color::from_hex(0x98C379)),
                        ),
                ),
        )
}

/// Demonstrates inline text helpers
fn inline_text_section() -> Div {
    div()
    .w_full()
        .flex_col()
        .gap(12.0)
        .child(h2("Inline Text Helpers").color(Color::WHITE))
        .child(
            div()
                .w_full() // Constrain width for text wrapping
                .bg(Color::rgba(0.12, 0.12, 0.15, 1.0))
                .rounded(8.0)
                .p(16.0)
                .flex_col()
                .gap(12.0)
                // Bold text
                .child(
                    div()
                        .flex_row()
                        .gap(4.0)
                        .items_baseline()
                        .child(label("b() / strong():").color(Color::GRAY))
                        .child(chained_text([
                            span("This is a").color(Color::WHITE),
                            b(" bold").color(Color::WHITE),
                            span(" statement.").color(Color::WHITE),
                        ])),
                )
                // Muted text
                .child(
                    div()
                        .flex_row()
                        .gap(4.0)
                        .items_baseline()
                        .child(label("muted():").color(Color::GRAY))
                        .child(muted("This is secondary/muted text")),
                )
                // Small text
                .child(
                    div()
                        .flex_row()
                        .gap(4.0)
                        .items_baseline()
                        .child(label("small():").color(Color::GRAY))
                        .child(small("This is small text (12px)").color(Color::WHITE)),
                )
                // Label
                .child(
                    div()
                        .flex_row()
                        .gap(4.0)
                        .items_baseline()
                        .child(label("label():").color(Color::GRAY))
                        .child(label("Form field label").color(Color::WHITE)),
                )
                // Caption
                .child(
                    div()
                        .flex_row()
                        .gap(4.0)
                        .items_baseline()
                        .child(label("caption():").color(Color::GRAY))
                        .child(caption("Figure 1: An image caption")),
                )
                // Paragraph
                .child(
                    div()
                        .flex_col()
                        .gap(4.0)
                        .w_full() // Allow text to wrap within container
                        .items_baseline()
                        .child(label("p():").color(Color::GRAY))
                        .child(
                            p("This is a paragraph with optimal line height (1.5) for readability. Paragraphs are styled at 16px with comfortable spacing for body text.")
                                .color(Color::WHITE),
                        ),
                )
                // Inline code
                .child(
                    div()
                        .flex_row()
                        .gap(4.0)
                        .items_baseline()
                        .child(label("inline_code():").color(Color::GRAY))
                        .child(chained_text([
                            span("Use ").color(Color::WHITE),
                            inline_code("div().flex_col()").color(Color::GRAY),
                            span(" for layouts").color(Color::WHITE)])),
                ),
        )
}

/// Demonstrates font family options
fn font_family_section() -> Div {
    div()
        .w_full()
        .flex_col()
        .gap(12.0)
        .child(h2("Font Families").color(Color::WHITE))
        .child(
            div()
                .w_full()
                .bg(Color::rgba(0.12, 0.12, 0.15, 1.0))
                .rounded(8.0)
                .p(16.0)
                .flex_col()
                .gap(8.0)
                // System (default)
                .child(
                    div()
                        .flex_row()
                        .gap(4.0)
                        .items_baseline()
                        .child(label("System (default):").color(Color::GRAY))
                        .child(
                            text("The quick brown fox jumps over the lazy dog").color(Color::WHITE),
                        ),
                )
                // Monospace
                .child(
                    div()
                        .flex_row()
                        .gap(4.0)
                        .items_baseline()
                        .child(label(".monospace():").color(Color::GRAY))
                        .child(
                            text("fn main() { println!(\"Hello\"); }")
                                .monospace()
                                .color(Color::from_hex(0x98C379)),
                        ),
                )
                // Serif
                .child(
                    div()
                        .flex_row()
                        .gap(4.0)
                        .items_baseline()
                        .child(label(".serif():").color(Color::GRAY))
                        .child(
                            text("The quick brown fox jumps over the lazy dog")
                                .serif()
                                .color(Color::WHITE),
                        ),
                )
                // Sans-serif
                .child(
                    div()
                        .flex_row()
                        .gap(4.0)
                        .items_baseline()
                        .child(label(".sans_serif():").color(Color::GRAY))
                        .child(
                            text("The quick brown fox jumps over the lazy dog")
                                .sans_serif()
                                .color(Color::WHITE),
                        ),
                )
                // Named font examples
                .child(
                    div()
                        .flex_col()
                        .gap(4.0)
                        .child(label("Named fonts with .font():").color(Color::GRAY))
                        .child(
                            div()
                                .flex_col()
                                .gap(4.0)
                                .child(
                                    text("Fira Code - fn main() { }")
                                        .font("Fira Code")
                                        .color(Color::from_hex(0xE5C07B)),
                                )
                                .child(
                                    text("Menlo - let x = 42;")
                                        .font("Menlo")
                                        .color(Color::from_hex(0x61AFEF)),
                                )
                                .child(
                                    text("SF Mono - const PI: f64 = 3.14;")
                                        .font("SF Mono")
                                        .color(Color::from_hex(0xC678DD)),
                                )
                                .child(
                                    text("Inter - Modern UI font")
                                        .font("Inter")
                                        .color(Color::WHITE),
                                ),
                        ),
                ),
        )
}

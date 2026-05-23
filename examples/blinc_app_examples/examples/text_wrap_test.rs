//! Text Wrapping Test Example
//!
//! no-web: diagnostic test, not a showcase demo — kept desktop-only
//! to avoid cluttering the web example gallery.
//!
//! Verifies that:
//! 1. Explicit \n characters render as line breaks
//! 2. Text auto-wraps when exceeding container width
//! 3. .no_wrap() keeps text on single line
//!
//! Run with: cargo run -p blinc_app_examples --example text_wrap_test

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_core::Color;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let config = WindowConfig {
        title: "Text Wrap Test".to_string(),
        width: 800,
        height: 600,
        resizable: true,
        ..Default::default()
    };

    blinc_app::windowed::WindowedApp::run(config, build_ui)
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder + use<> {
    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.1, 0.1, 0.15, 1.0))
        .flex_col()
        .p(40.0)
        .gap(24.0)
        .overflow_clip()
        // Title
        .child(
            text("Text Wrapping Test")
                .size(32.0)
                .weight(FontWeight::Bold)
                .color(Color::WHITE),
        )
        // Test 1: Explicit newlines
        .child(build_test_section(
            "Test 1: Explicit \\n Characters",
            text("Line 1\nLine 2\nLine 3")
                .size(16.0)
                .color(Color::WHITE),
        ))
        // Test 2: Auto word-wrap
        .child(build_test_section(
            "Test 2: Auto Word-Wrap (300px container)",
            text("This is a long paragraph that should automatically wrap at the container boundary when it exceeds the available width.")
                .size(16.0)
                .color(Color::WHITE),
        ))
        // Test 3: No wrap
        .child(build_test_section(
            "Test 3: No Wrap (should overflow)",
            text("This text has no_wrap() so it stays on a single line even when very long")
                .size(16.0)
                .no_wrap()
                .color(Color::WHITE),
        ))
        // Test 4: Combined - newlines with wrapping
        .child(build_test_section(
            "Test 4: Newlines + Word Wrap",
            text("Paragraph 1: This is the first paragraph with enough text to wrap.\n\nParagraph 2: Second paragraph after explicit newlines, also long enough to wrap at container bounds.")
                .size(16.0)
                .color(Color::WHITE),
        ))
}

fn build_test_section<C: ElementBuilder + 'static>(
    title: &str,
    content: C,
) -> impl ElementBuilder + use<C> {
    div()
        .flex_col()
        .gap(8.0)
        .child(
            text(title)
                .size(14.0)
                .weight(FontWeight::Medium)
                .color(Color::rgba(0.4, 0.8, 1.0, 1.0)),
        )
        .child(
            div()
                .w(300.0) // Fixed width to test wrapping
                .p(16.0)
                .bg(Color::rgba(0.2, 0.2, 0.25, 0.9))
                .rounded(8.0)
                .child(content),
        )
}

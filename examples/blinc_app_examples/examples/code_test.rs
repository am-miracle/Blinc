//! Minimal Code Element Test
//!
//! no-web: minimal diagnostic test — code_demo covers the same features
//! in the web gallery.
//!
//! Run with: cargo run -p blinc_app_examples --example code_test --features windowed

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_core::Color;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let config = WindowConfig {
        title: "Code Test".to_string(),
        width: 800,
        height: 600,
        resizable: true,
        ..Default::default()
    };

    blinc_app::windowed::WindowedApp::run(config, build_ui)
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    let simple_code = "fn main() {\n    println!(\"Hello\");\n}";

    // Debug: print what's being passed to code()
    println!("Code content: {:?}", simple_code);

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.1, 0.1, 0.15, 1.0))
        .flex_col()
        .gap(20.0)
        .p(32.0)
        // Regular text for comparison
        .child(
            text("Regular text works fine")
                .size(16.0)
                .color(Color::WHITE),
        )
        // Simple code without syntax highlighting
        .child(
            div()
                .flex_col()
                .gap(4.0)
                .child(
                    text("Simple code (no syntax):")
                        .size(14.0)
                        .color(Color::YELLOW),
                )
                .child(code(simple_code).font_size(14.0).w(400.0).h(100.0)),
        )
        // Code with syntax highlighting
        .child(
            div()
                .flex_col()
                .gap(4.0)
                .child(
                    text("With Rust highlighting:")
                        .size(14.0)
                        .color(Color::YELLOW),
                )
                .child(
                    code(simple_code)
                        .syntax(SyntaxConfig::new(RustHighlighter::new()))
                        .font_size(14.0)
                        .w(400.0)
                        .h(100.0),
                ),
        )
        // Code with line numbers
        .child(
            div()
                .flex_col()
                .gap(4.0)
                .child(text("With line numbers:").size(14.0).color(Color::YELLOW))
                .child(
                    code(simple_code)
                        .syntax(SyntaxConfig::new(RustHighlighter::new()))
                        .line_numbers(true)
                        .font_size(14.0)
                        .w(500.0)
                        .h(100.0),
                ),
        )
}

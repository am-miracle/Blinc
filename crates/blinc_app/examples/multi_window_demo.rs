//! Multi-Window Demo
//!
//! Demonstrates opening additional windows via ctx.open_window().
//! Click the button in the primary window to open a new window.
//!
//! Run with: cargo run -p blinc_app --example multi_window_demo

use blinc_app::prelude::*;
use blinc_app::windowed::{WindowedApp, WindowedContext};
use blinc_core::Color;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let config = WindowConfig {
        title: "Multi-Window Demo (Primary)".to_string(),
        width: 600,
        height: 400,
        resizable: true,
        ..Default::default()
    };

    WindowedApp::run(config, |ctx| build_primary_ui(ctx))
}

fn build_primary_ui(ctx: &WindowedContext) -> impl ElementBuilder {
    let window_count = ctx.use_state_keyed("win_count", || 1u32);

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.08, 0.08, 0.12, 1.0))
        .flex_col()
        .justify_center()
        .items_center()
        .gap_px(24.0)
        .child(
            text("Multi-Window Demo")
                .size(32.0)
                .color(Color::WHITE)
                .bold(),
        )
        .child(
            text("Click the button to open a new window")
                .size(16.0)
                .color(Color::rgba(0.6, 0.6, 0.7, 1.0)),
        )
        .child(
            text(format!("Windows opened: {}", window_count.get()))
                .size(14.0)
                .color(Color::rgba(0.5, 0.8, 1.0, 1.0)),
        )
        .child(
            div()
                .w(200.0)
                .h(44.0)
                .bg(Color::rgba(0.3, 0.5, 1.0, 1.0))
                .rounded(8.0)
                .cursor_pointer()
                .items_center()
                .justify_center()
                .child(
                    text("Open New Window")
                        .size(14.0)
                        .color(Color::WHITE)
                        .bold(),
                )
                .on_click(move |_ctx| {
                    let count = window_count.get() + 1;
                    window_count.set(count);

                    // Open a new window via the global function
                    let config = WindowConfig {
                        title: format!("Window #{}", count),
                        width: 400,
                        height: 300,
                        resizable: true,
                        ..Default::default()
                    };
                    blinc_app::windowed::open_window(config);
                }),
        )
}

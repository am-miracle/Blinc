//! Multi-Window Demo
//!
//! Demonstrates opening additional windows via open_window().
//! Each secondary window renders with its own title and dimensions.
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

    // Color palette for windows
    let colors = [
        ("Coral", Color::rgba(1.0, 0.4, 0.4, 1.0)),
        ("Teal", Color::rgba(0.2, 0.8, 0.7, 1.0)),
        ("Violet", Color::rgba(0.6, 0.4, 1.0, 1.0)),
        ("Gold", Color::rgba(1.0, 0.8, 0.2, 1.0)),
        ("Sky", Color::rgba(0.3, 0.7, 1.0, 1.0)),
    ];

    let mut root = div()
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
            text("Click buttons to open themed windows")
                .size(16.0)
                .color(Color::rgba(0.6, 0.6, 0.7, 1.0)),
        )
        .child(
            text(format!("Windows opened: {}", window_count.get()))
                .size(14.0)
                .color(Color::rgba(0.5, 0.8, 1.0, 1.0)),
        );

    // Create a row of colored buttons
    let mut button_row = div().flex_row().gap_px(12.0);

    for (name, color) in &colors {
        let label = name.to_string();
        let click_name = name.to_string();
        let color = *color;
        let wc = window_count.clone();

        button_row = button_row.child(
            div()
                .w(100.0)
                .h(40.0)
                .bg(color)
                .rounded(8.0)
                .cursor_pointer()
                .items_center()
                .justify_center()
                .child(text(&label).size(13.0).color(Color::WHITE).bold())
                .on_click(move |_ctx| {
                    let count = wc.get() + 1;
                    wc.set(count);

                    let config = WindowConfig {
                        title: format!("{} Window #{}", click_name, count),
                        width: 400,
                        height: 300,
                        resizable: true,
                        ..Default::default()
                    };
                    blinc_app::windowed::open_window(config);
                }),
        );
    }

    root = root.child(button_row);
    root
}

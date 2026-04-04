//! Multi-Window Demo
//!
//! Demonstrates:
//! - Opening themed secondary windows via open_window_with()
//! - Modal windows that block input to the primary window
//! - Custom title bars with drag regions
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
        width: 700,
        height: 450,
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

    // Row of colored window buttons
    let mut button_row = div().flex_row().gap_px(12.0);

    for (name, color) in &colors {
        let label = name.to_string();
        let click_name = name.to_string();
        let color = *color;
        let wc = window_count.clone();

        button_row = button_row.child(
            div()
                .w(90.0)
                .h(36.0)
                .bg(color)
                .rounded(8.0)
                .cursor_pointer()
                .items_center()
                .justify_center()
                .child(text(&label).size(12.0).color(Color::WHITE).bold())
                .on_click(move |_ctx| {
                    let count = wc.get() + 1;
                    wc.set(count);

                    let win_color = color;
                    let win_name = click_name.clone();
                    let config =
                        WindowConfig::new(format!("{} Window #{}", win_name, count)).size(400, 300);
                    blinc_app::windowed::open_window_with(config, move |ctx| {
                        themed_window_ui(ctx, &win_name, win_color)
                    });
                }),
        );
    }

    root = root.child(button_row);

    // Modal and frameless buttons row
    root = root.child(
        div()
            .flex_row()
            .gap_px(12.0)
            .child(
                div()
                    .w(180.0)
                    .h(36.0)
                    .bg(Color::rgba(0.8, 0.2, 0.2, 1.0))
                    .rounded(8.0)
                    .cursor_pointer()
                    .items_center()
                    .justify_center()
                    .child(text("Open Modal").size(12.0).color(Color::WHITE).bold())
                    .on_click(move |_ctx| {
                        let config = WindowConfig::new("Confirm Action")
                            .size(360, 200)
                            .center()
                            .resizable(false)
                            .modal();
                        blinc_app::windowed::open_window_with(config, modal_ui);
                    }),
            )
            .child(
                div()
                    .w(180.0)
                    .h(36.0)
                    .bg(Color::rgba(0.2, 0.2, 0.3, 1.0))
                    .border(1.0, Color::rgba(0.4, 0.4, 0.5, 1.0))
                    .rounded(8.0)
                    .cursor_pointer()
                    .items_center()
                    .justify_center()
                    .child(
                        text("Frameless Window")
                            .size(12.0)
                            .color(Color::WHITE)
                            .bold(),
                    )
                    .on_click(move |_ctx| {
                        let config = WindowConfig::new("")
                            .size(400, 280)
                            .center()
                            .decorations(false);
                        blinc_app::windowed::open_window_with(config, frameless_window_ui);
                    }),
            ),
    );

    root
}

/// Themed secondary window UI
fn themed_window_ui(ctx: &mut WindowedContext, name: &str, color: Color) -> Div {
    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.06, 0.06, 0.09, 1.0))
        .flex_col()
        .justify_center()
        .items_center()
        .gap_px(16.0)
        .child(
            text(format!("{} Window", name))
                .size(28.0)
                .color(color)
                .bold(),
        )
        .child(div().w(200.0).h(4.0).bg(color).rounded(2.0))
        .child(
            text(format!("{:.0} x {:.0}", ctx.width, ctx.height))
                .size(14.0)
                .color(Color::rgba(0.6, 0.6, 0.7, 1.0)),
        )
        .child(
            text("This window has its own UI builder!")
                .size(12.0)
                .color(Color::rgba(0.5, 0.5, 0.5, 1.0)),
        )
}

/// Modal dialog window UI
fn modal_ui(ctx: &mut WindowedContext) -> Div {
    let bg = Color::rgba(0.12, 0.12, 0.16, 1.0);
    let danger = Color::rgba(0.9, 0.3, 0.3, 1.0);
    let close_cb = ctx.close_callback();

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(bg)
        .flex_col()
        .justify_center()
        .items_center()
        .gap_px(20.0)
        // Icon / warning
        .child(text("Are you sure?").size(22.0).color(Color::WHITE).bold())
        .child(
            text("This action cannot be undone.")
                .size(14.0)
                .color(Color::rgba(0.6, 0.6, 0.7, 1.0)),
        )
        // Button row
        .child(
            div()
                .flex_row()
                .gap_px(12.0)
                .child(
                    div()
                        .w(120.0)
                        .h(36.0)
                        .bg(Color::rgba(0.2, 0.2, 0.25, 1.0))
                        .border(1.0, Color::rgba(0.3, 0.3, 0.35, 1.0))
                        .rounded(6.0)
                        .cursor_pointer()
                        .items_center()
                        .justify_center()
                        .child(text("Cancel").size(13.0).color(Color::WHITE))
                        .on_click({
                            let close = close_cb.clone();
                            move |_| close()
                        }),
                )
                .child(
                    div()
                        .w(120.0)
                        .h(36.0)
                        .bg(danger)
                        .rounded(6.0)
                        .cursor_pointer()
                        .items_center()
                        .justify_center()
                        .child(text("Delete").size(13.0).color(Color::WHITE).bold())
                        .on_click({
                            let close = close_cb.clone();
                            move |_| {
                                tracing::info!("Confirmed! (modal action taken)");
                                close();
                            }
                        }),
                ),
        )
        // Subtle hint
        .child(
            text("This is a modal window — try clicking the primary window")
                .size(10.0)
                .color(Color::rgba(0.4, 0.4, 0.45, 1.0)),
        )
}

/// Frameless window with custom title bar and drag region
fn frameless_window_ui(ctx: &mut WindowedContext) -> Div {
    let title_bar_color = Color::rgba(0.12, 0.12, 0.15, 1.0);
    let accent = Color::rgba(0.4, 0.7, 1.0, 1.0);
    let minimize_cb = ctx.minimize_callback();
    let maximize_cb = ctx.maximize_callback();
    let close_cb = ctx.close_callback();
    let drag_cb = ctx.drag_callback();

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.08, 0.08, 0.1, 1.0))
        .border(1.0, Color::rgba(0.25, 0.25, 0.3, 1.0))
        .flex_col()
        // Custom title bar: drag zone + control buttons as siblings (no bubbling)
        .child(
            div()
                .w_full()
                .h(36.0)
                .bg(title_bar_color)
                .flex_row()
                .items_center()
                .padding_x_px(12.0)
                // Drag zone (title text area, grows to fill)
                .child(
                    div()
                        .flex_grow()
                        .h_full()
                        .flex_row()
                        .items_center()
                        .cursor_pointer()
                        .on_mouse_down({
                            let drag = drag_cb;
                            move |_| drag()
                        })
                        .child(
                            text("Frameless Window")
                                .size(13.0)
                                .color(Color::rgba(0.7, 0.7, 0.8, 1.0)),
                        ),
                )
                // Window control buttons (sibling, not child of drag zone)
                .child(
                    div()
                        .flex_row()
                        .gap_px(8.0)
                        // Minimize
                        .child(
                            div()
                                .w(14.0)
                                .h(14.0)
                                .bg(Color::rgba(1.0, 0.8, 0.0, 0.8))
                                .rounded(7.0)
                                .cursor_pointer()
                                .on_click({
                                    let cb = minimize_cb;
                                    move |_| cb()
                                }),
                        )
                        // Maximize
                        .child(
                            div()
                                .w(14.0)
                                .h(14.0)
                                .bg(Color::rgba(0.2, 0.8, 0.2, 0.8))
                                .rounded(7.0)
                                .cursor_pointer()
                                .on_click({
                                    let cb = maximize_cb;
                                    move |_| cb()
                                }),
                        )
                        // Close
                        .child(
                            div()
                                .w(14.0)
                                .h(14.0)
                                .bg(Color::rgba(1.0, 0.3, 0.3, 0.8))
                                .rounded(7.0)
                                .cursor_pointer()
                                .on_click({
                                    let cb = close_cb;
                                    move |_| cb()
                                }),
                        ),
                ),
        )
        // Content
        .child(
            div()
                .flex_grow()
                .w_full()
                .flex_col()
                .justify_center()
                .items_center()
                .gap_px(12.0)
                .child(text("Custom Title Bar").size(24.0).color(accent).bold())
                .child(div().w(160.0).h(3.0).bg(accent).rounded(2.0))
                .child(
                    text("Drag the title bar to move this window")
                        .size(13.0)
                        .color(Color::rgba(0.5, 0.5, 0.6, 1.0)),
                )
                .child(
                    text("Use the traffic light buttons to minimize/maximize/close")
                        .size(11.0)
                        .color(Color::rgba(0.4, 0.4, 0.45, 1.0)),
                ),
        )
}

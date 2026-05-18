//! Canvas Element Demo
//!
//! This example demonstrates the canvas element for custom GPU drawing
//! within the layout system.
//!
//! Features demonstrated:
//! - Custom 2D drawing with DrawContext
//! - Canvas respects layout transforms and clipping
//! - Procedural graphics (animated shapes, patterns)
//! - Canvas for cursor/indicator rendering
//! - BlincComponent derive macro for type-safe animation hooks
//!
//! Run with: cargo run -p blinc_app_examples --example canvas_demo

use blinc_animation::SpringConfig;
use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_core::{
    Brush, Color, CornerRadius, DrawContext, Gradient, GradientStop, Point, Rect, TextAlign,
    TextBaseline, TextStyle,
};
use std::sync::Arc;

/// Component for the animated demo card.
/// The BlincComponent derive generates a unique compile-time key and
/// provides type-safe use_animated_value/use_animated_timeline methods.
#[derive(BlincComponent)]
struct AnimatedDemoCard;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = WindowConfig {
        title: "Canvas Element Demo".to_string(),
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
        .bg(Color::rgba(0.08, 0.08, 0.12, 1.0))
        .flex_col()
        .items_center()
        .gap_px(10.0)
        .p(10.0)
        // Title
        .child(text("Canvas Element Demo").size(28.0).color(Color::WHITE))
        .child(
            text("Custom GPU drawing within the layout system")
                .size(14.0)
                .color(Color::rgba(0.6, 0.6, 0.7, 1.0)),
        )
        // Demo grid
        .child(
            div()
                .w_fit()
                .h(ctx.height - 200.0)
                .flex_row()
                .flex_wrap()
                .overflow_y_scroll()
                .gap(10.0)
                .child(demo_card("Simple Rectangle", simple_rectangle_canvas()))
                .child(demo_card("Gradient Fill", gradient_canvas()))
                .child(demo_card("Nested Shapes", nested_shapes_canvas()))
                .child(demo_card("Custom Cursor", cursor_demo_canvas()))
                .child(demo_card("Progress Bar", progress_bar_canvas(0.65)))
                .child(demo_card("Color Palette", color_palette_canvas()))
                .child(demo_card("Canvas Text", text_in_canvas()))
                .child(animated_demo_card(ctx)),
        )
}

/// Wraps a canvas in a demo card with a title
fn demo_card(title: &'static str, canvas_element: Canvas) -> Div {
    div()
        .w(300.0)
        .p(16.0) // Uniform padding on all sides
        .flex_col()
        .justify_center()
        .items_center()
        .gap(8.0)
        .bg(Color::rgba(0.12, 0.12, 0.16, 1.0))
        .rounded(12.0)
        .overflow_clip() // Clip children to card bounds
        .child(
            text(title)
                .size(14.0)
                .color(Color::rgba(0.8, 0.8, 0.9, 1.0)),
        )
        .child(canvas_element)
}

/// Demo 1: Simple filled rectangle
fn simple_rectangle_canvas() -> Canvas {
    canvas(|ctx: &mut dyn DrawContext, bounds| {
        // Fill with a blue rectangle
        ctx.fill_rect(
            Rect::new(10.0, 10.0, bounds.width - 20.0, bounds.height - 20.0),
            CornerRadius::uniform(8.0),
            Brush::Solid(Color::rgba(0.3, 0.5, 0.9, 1.0)),
        );
    })
    .w(228.0)
    .h(120.0)
}

/// Demo 2: Gradient fill
fn gradient_canvas() -> Canvas {
    canvas(|ctx: &mut dyn DrawContext, bounds| {
        // Create a horizontal gradient
        let gradient = Brush::Gradient(Gradient::linear_with_stops(
            Point::new(0.0, bounds.height / 2.0),
            Point::new(bounds.width, bounds.height / 2.0),
            vec![
                GradientStop::new(0.0, Color::rgba(0.9, 0.2, 0.5, 1.0)),
                GradientStop::new(0.5, Color::rgba(0.9, 0.5, 0.2, 1.0)),
                GradientStop::new(1.0, Color::rgba(0.2, 0.8, 0.6, 1.0)),
            ],
        ));

        ctx.fill_rect(
            Rect::new(0.0, 0.0, bounds.width, bounds.height),
            CornerRadius::uniform(8.0),
            gradient,
        );
    })
    .w(228.0)
    .h(120.0)
}

/// Demo 3: Nested shapes
fn nested_shapes_canvas() -> Canvas {
    canvas(|ctx: &mut dyn DrawContext, bounds| {
        let cx = bounds.width / 2.0;
        let cy = bounds.height / 2.0;

        // Draw concentric rectangles
        let colors = [
            Color::rgba(0.2, 0.3, 0.8, 0.9),
            Color::rgba(0.3, 0.6, 0.9, 0.8),
            Color::rgba(0.4, 0.8, 0.9, 0.7),
            Color::rgba(0.6, 0.9, 0.8, 0.6),
        ];

        for (i, color) in colors.iter().enumerate() {
            let _offset = i as f32 * 12.0;
            let size = (4 - i) as f32 * 24.0;
            ctx.fill_rect(
                Rect::new(cx - size / 2.0, cy - size / 2.0, size, size),
                CornerRadius::uniform(4.0 + i as f32 * 2.0),
                Brush::Solid(*color),
            );
        }
    })
    .w(228.0)
    .h(120.0)
}

/// Demo 4: Custom cursor indicator (like in text inputs)
fn cursor_demo_canvas() -> Canvas {
    canvas(|ctx: &mut dyn DrawContext, bounds| {
        // Background
        ctx.fill_rect(
            Rect::new(0.0, 0.0, bounds.width, bounds.height),
            CornerRadius::uniform(6.0),
            Brush::Solid(Color::rgba(0.15, 0.15, 0.2, 1.0)),
        );

        // Simulated text (horizontal lines)
        let text_color = Color::rgba(0.7, 0.7, 0.8, 1.0);
        for i in 0..3 {
            let y = 20.0 + i as f32 * 25.0;
            let width = if i == 2 { 80.0 } else { 180.0 };
            ctx.fill_rect(
                Rect::new(15.0, y, width, 12.0),
                CornerRadius::uniform(2.0),
                Brush::Solid(text_color),
            );
        }

        // Blinking cursor (just draw it solid for demo)
        let cursor_x = 100.0;
        let cursor_y = 15.0;
        let cursor_height = bounds.height - 30.0;
        ctx.fill_rect(
            Rect::new(cursor_x, cursor_y, 2.0, cursor_height),
            CornerRadius::default(),
            Brush::Solid(Color::rgba(0.4, 0.6, 1.0, 1.0)),
        );
    })
    .w(228.0)
    .h(100.0)
}

/// Demo 5: Progress bar with custom styling
fn progress_bar_canvas(progress: f32) -> Canvas {
    canvas(move |ctx: &mut dyn DrawContext, bounds| {
        let bar_height = 20.0;
        let bar_y = (bounds.height - bar_height) / 2.0;
        let radius = CornerRadius::uniform(bar_height / 2.0);

        // Background track
        ctx.fill_rect(
            Rect::new(0.0, bar_y, bounds.width, bar_height),
            radius,
            Brush::Solid(Color::rgba(0.2, 0.2, 0.25, 1.0)),
        );

        // Progress fill with gradient
        let fill_width = bounds.width * progress.clamp(0.0, 1.0);
        if fill_width > 0.0 {
            let gradient = Brush::Gradient(Gradient::linear(
                Point::new(0.0, bar_y),
                Point::new(fill_width, bar_y),
                Color::rgba(0.4, 0.6, 1.0, 1.0),
                Color::rgba(0.6, 0.4, 1.0, 1.0),
            ));
            ctx.fill_rect(
                Rect::new(0.0, bar_y, fill_width, bar_height),
                radius,
                gradient,
            );
        }

        // Progress percentage indicator — small pill above the bar
        // with the percentage text centred inside it.
        let percent = (progress * 100.0) as i32;
        let bubble = Rect::new(bounds.width / 2.0 - 25.0, bar_y - 28.0, 50.0, 22.0);
        ctx.fill_rect(
            bubble,
            CornerRadius::uniform(4.0),
            Brush::Solid(Color::rgba(0.1, 0.1, 0.15, 0.9)),
        );

        // Anchor the text at the bubble's centre and render with
        // `Middle` + `Center` alignment so "65%" stays perfectly
        // centred regardless of the digit count.
        ctx.draw_text(
            &format!("{}%", percent),
            Point::new(
                bubble.x() + bubble.width() / 2.0,
                bubble.y() + bubble.height() / 2.0,
            ),
            &TextStyle::new(13.0)
                .with_color(Color::WHITE)
                .with_align(TextAlign::Center)
                .with_baseline(TextBaseline::Middle),
        );
    })
    .w(228.0)
    .h(80.0)
}

/// Demo 6: Color palette grid
fn color_palette_canvas() -> Canvas {
    canvas(|ctx: &mut dyn DrawContext, bounds| {
        let cols = 8;
        let rows = 3;
        let cell_w = bounds.width / cols as f32;
        let cell_h = bounds.height / rows as f32;
        let gap = 2.0;

        for row in 0..rows {
            for col in 0..cols {
                let hue = col as f32 / cols as f32;
                let sat = 1.0 - (row as f32 * 0.25);
                let val = 0.9 - (row as f32 * 0.15);

                // Convert HSV to RGB (simplified)
                let color = hsv_to_rgb(hue, sat, val);

                let x = col as f32 * cell_w + gap / 2.0;
                let y = row as f32 * cell_h + gap / 2.0;

                ctx.fill_rect(
                    Rect::new(x, y, cell_w - gap, cell_h - gap),
                    CornerRadius::uniform(3.0),
                    Brush::Solid(color),
                );
            }
        }
    })
    .w(228.0)
    .h(90.0)
}

/// Demo 7: Text rendering via `DrawContext::draw_text` from inside
/// a canvas callback. Exercises multiple sizes, colours, and
/// alignments so any regression in canvas-pushed text is immediately
/// visible.
///
/// Each `TextStyle` switches the baseline to `Top` so the `y`
/// coordinate represents the top of the glyph bounds — most intuitive
/// for layout-style positioning. The default `Alphabetic` baseline
/// (matching HTML5 Canvas) treats `y` as the text's baseline, which
/// is what typographers expect but trips up layout-style use. Both
/// conventions are supported; `with_baseline` flips between them.
fn text_in_canvas() -> Canvas {
    canvas(|ctx: &mut dyn DrawContext, bounds| {
        // Background plate so the text has obvious contrast.
        ctx.fill_rect(
            Rect::new(0.0, 0.0, bounds.width, bounds.height),
            CornerRadius::uniform(8.0),
            Brush::Solid(Color::rgba(0.1, 0.1, 0.15, 1.0)),
        );

        // Big heading — Top baseline so `y` is the top of the
        // text bounds.
        ctx.draw_text(
            "Hello canvas!",
            Point::new(12.0, 8.0),
            &TextStyle::new(18.0)
                .with_color(Color::WHITE)
                .with_baseline(TextBaseline::Top),
        );

        // Smaller subtitle with a different colour.
        ctx.draw_text(
            "draw_text inside a canvas callback",
            Point::new(12.0, 38.0),
            &TextStyle::new(11.0)
                .with_color(Color::rgba(0.55, 0.70, 0.95, 1.0))
                .with_baseline(TextBaseline::Top),
        );

        // Digit + punctuation glyphs — regression guard for the
        // "only alphabetic glyphs in atlas" case the progress-bar
        // originally missed.
        ctx.draw_text(
            "0123456789 · %$@",
            Point::new(12.0, 62.0),
            &TextStyle::new(12.0)
                .with_color(Color::rgba(0.9, 0.85, 0.5, 1.0))
                .with_baseline(TextBaseline::Top),
        );
    })
    .w(228.0)
    .h(90.0)
}

/// Simple HSV to RGB conversion
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Color {
    let c = v * s;
    let x = c * (1.0 - ((h * 6.0) % 2.0 - 1.0).abs());
    let m = v - c;

    let (r, g, b) = match (h * 6.0) as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };

    Color::rgba(r + m, g + m, b + m, 1.0)
}

/// Demo 7: Animated bouncing ball using AnimatedValue
///
/// Uses the built-in AnimatedValue wrapper which handles spring management.
/// With BlincComponent derive macro for type-safe animation hooks.
fn animated_demo_card(ctx: &WindowedContext) -> Div {
    // AnimatedValue persisted to context using component-based key
    // The BlincComponent derive macro generates a unique key from the type
    let ball_x = AnimatedDemoCard::use_animated_value(ctx, 20.0, SpringConfig::wobbly());

    let render_ball_x = Arc::clone(&ball_x);
    let click_ball_x = Arc::clone(&ball_x);

    div()
        .w(300.0)
        .p(16.0)
        .flex_col()
        .justify_center()
        .items_center()
        .gap(8.0)
        .bg(Color::rgba(0.12, 0.12, 0.16, 1.0))
        .rounded(12.0)
        .overflow_clip()
        .child(
            text("Animated (Click!)")
                .size(14.0)
                .color(Color::rgba(0.8, 0.8, 0.9, 1.0)),
        )
        .child(
            canvas(move |ctx: &mut dyn DrawContext, bounds| {
                // Get current animated value - AnimatedValue handles all the complexity
                let current_x = render_ball_x.lock().unwrap().get();

                // Draw track
                let track_y = bounds.height / 2.0;
                ctx.fill_rect(
                    Rect::new(10.0, track_y - 2.0, bounds.width - 20.0, 4.0),
                    CornerRadius::uniform(2.0),
                    Brush::Solid(Color::rgba(0.2, 0.2, 0.25, 1.0)),
                );

                // Draw bouncing ball
                let ball_size = 24.0;
                let ball_y = track_y - ball_size / 2.0;
                ctx.fill_rect(
                    Rect::new(current_x, ball_y, ball_size, ball_size),
                    CornerRadius::uniform(ball_size / 2.0),
                    Brush::Gradient(Gradient::linear(
                        Point::new(current_x, ball_y),
                        Point::new(current_x + ball_size, ball_size + ball_y),
                        Color::rgba(0.9, 0.4, 0.3, 1.0),
                        Color::rgba(0.9, 0.6, 0.2, 1.0),
                    )),
                );
            })
            .w(228.0)
            .h(80.0),
        )
        .on_click(move |_| {
            println!("Canvas clicked - toggling ball direction");
            // Toggle direction by checking current target
            let mut ball = click_ball_x.lock().unwrap();
            let current_target = ball.target();
            let new_target = if current_target < 100.0 { 194.0 } else { 20.0 };

            // Set new target - AnimatedValue handles the spring
            ball.set_target(new_target);
        })
}

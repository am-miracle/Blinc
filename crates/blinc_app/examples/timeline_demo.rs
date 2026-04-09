//! Timeline Animation Demo
//!
//! This example demonstrates timeline-based animations using the stateful API:
//! - Ping-pong animations using `use_keyframes` with fluent builder
//! - Multiple animated values with staggered delays
//! - Continuous looping animations with easing
//!
//! Run with: cargo run -p blinc_app --example timeline_demo --features windowed

use blinc_animation::Easing;
use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_core::Transform;
use blinc_layout::stateful::NoState;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let config = WindowConfig {
        title: "Timeline Animation Demo".to_string(),
        width: 800,
        height: 800,
        resizable: true,
        ..Default::default()
    };

    blinc_app::windowed::WindowedApp::run(config, build_ui)
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.06, 0.06, 0.1, 1.0))
        .flex_col()
        .items_center()
        .justify_center()
        .gap_px(60.0)
        // Title
        .child(
            text("Timeline Animation Demo")
                .size(42.0)
                .weight(FontWeight::Bold)
                .color(Color::WHITE),
        )
        // Subtitle
        .child(
            text("Watch the ping-pong animations!")
                .size(18.0)
                .color(Color::rgba(1.0, 1.0, 1.0, 0.6)),
        )
        // Animation showcase
        .child(
            div()
                .flex_row()
                .gap_px(40.0)
                .child(bouncing_ball())
                .child(pulsing_ring())
                .child(sliding_bars()),
        )
        // Description
        .child(
            div()
                .w(600.0)
                .p(20.0)
                .bg(Color::rgba(1.0, 1.0, 1.0, 0.05))
                .rounded(12.0)
                .child(
                    text("Bounce: click to toggle with spring physics.\nPulse: click to start/stop keyframe animation.\nStagger: Timeline with alternate mode and staggered offsets.")
                        .size(14.0)
                        .color(Color::rgba(1.0, 1.0, 1.0, 0.5))
                        .text_center(),
                ),
        )
}

/// A bouncing ball - click to toggle position with smooth spring animation
fn bouncing_ball() -> impl ElementBuilder {
    use blinc_animation::SpringConfig;
    use blinc_core::events::event_types;
    use blinc_layout::stateful::ButtonState;

    stateful::<ButtonState>().on_state(|ctx| {
        // Track whether ball is down (toggled on click)
        let is_down = ctx.use_signal("is_down", || false);

        // Handle click to toggle position
        if let Some(event) = ctx.event() {
            if event.event_type == event_types::POINTER_UP {
                is_down.update(|v| !v);
            }
        }

        // Spring animates smoothly to target position
        let target_y = if is_down.get() { 50.0 } else { 0.0 };
        let y_offset = ctx.use_spring("y", target_y, SpringConfig::wobbly());

        // Visual feedback for hover state
        let bg = match ctx.state() {
            ButtonState::Hovered | ButtonState::Pressed => Color::rgba(0.2, 0.2, 0.28, 1.0),
            _ => Color::rgba(0.15, 0.15, 0.2, 1.0),
        };

        div()
            .w(140.0)
            .h(180.0)
            .bg(bg)
            .rounded(16.0)
            .flex_col()
            .items_center()
            .p(8.0)
            .gap_px(8.0)
            .cursor_pointer()
            // Label
            .child(
                text("Bounce")
                    .size(14.0)
                    .weight(FontWeight::SemiBold)
                    .color(Color::rgba(1.0, 1.0, 1.0, 0.7))
                    .pointer_events_none(),
            )
            // Instruction
            .child(
                text("Click me!")
                    .size(11.0)
                    .color(Color::rgba(1.0, 1.0, 1.0, 0.4))
                    .pointer_events_none(),
            )
            // Ball container
            .child(
                div()
                    .w(100.0)
                    .h(80.0)
                    .flex_col()
                    .items_center()
                    .pointer_events_none()
                    // The bouncing ball
                    .child(
                        div()
                            .w(30.0)
                            .h(30.0)
                            .bg(Color::rgba(0.4, 0.8, 1.0, 1.0))
                            .rounded(15.0)
                            .transform(Transform::translate(0.0, y_offset)),
                    ),
            )
    })
}

/// A pulsing ring - click to start/stop continuous pulsation
fn pulsing_ring() -> impl ElementBuilder {
    use blinc_core::events::event_types;
    use blinc_layout::stateful::ButtonState;

    stateful::<ButtonState>().on_state(|ctx| {
        // Track whether animation is running
        let is_running = ctx.use_signal("running", || false);

        // Keyframe animation for scale: 0.8 -> 1.2 with ping-pong and easing
        let scale_anim = ctx.use_keyframes("scale", |k| {
            k.at(0, 0.8)
                .at(800, 1.2)
                .ease(Easing::EaseInOut)
                .ping_pong()
                .loop_infinite()
        });

        // Keyframe animation for opacity: 0.4 -> 1.0 with ping-pong and easing
        let opacity_anim = ctx.use_keyframes("opacity", |k| {
            k.at(0, 0.4)
                .at(800, 1.0)
                .ease(Easing::EaseInOut)
                .ping_pong()
                .loop_infinite()
        });

        // Handle click to toggle animation
        if let Some(event) = ctx.event() {
            if event.event_type == event_types::POINTER_UP {
                if is_running.get() {
                    scale_anim.stop();
                    opacity_anim.stop();
                    is_running.set(false);
                } else {
                    scale_anim.start();
                    opacity_anim.start();
                    is_running.set(true);
                }
            }
        }

        // Get current animated values
        let scale = scale_anim.get();
        let opacity = opacity_anim.get();

        let bg = match ctx.state() {
            ButtonState::Hovered | ButtonState::Pressed => Color::rgba(0.2, 0.2, 0.28, 1.0),
            _ => Color::rgba(0.15, 0.15, 0.2, 1.0),
        };

        div()
            .w(140.0)
            .h(180.0)
            .bg(bg)
            .rounded(16.0)
            .flex_col()
            .items_center()
            .p(8.0)
            .gap_px(8.0)
            .cursor_pointer()
            // Label
            .child(
                text("Pulse")
                    .size(14.0)
                    .weight(FontWeight::SemiBold)
                    .color(Color::rgba(1.0, 1.0, 1.0, 0.7))
                    .pointer_events_none(),
            )
            // Instruction
            .child(
                text("Click me!")
                    .size(11.0)
                    .color(Color::rgba(1.0, 1.0, 1.0, 0.4))
                    .pointer_events_none(),
            )
            // Ring container
            .child(
                div()
                    .w(100.0)
                    .h(80.0)
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .pointer_events_none()
                    // The pulsing ring
                    .child(
                        div()
                            .w(60.0)
                            .h(60.0)
                            .border(4.0, Color::rgba(1.0, 0.5, 0.3, opacity))
                            .rounded(30.0)
                            .transform(Transform::scale(scale, scale)),
                    ),
            )
    })
}

/// Sliding bars with staggered timing using Timeline
fn sliding_bars() -> impl ElementBuilder {
    stateful::<NoState>().on_state(|ctx| {
        // Use a single timeline with staggered entries
        // Timeline handles the coordination, ping-pong reverses at the timeline level
        // so stagger offset is maintained across all iterations
        let ((bar1_id, bar2_id, bar3_id), timeline) = ctx.use_timeline("bars", |t| {
            // Add three entries with staggered offsets
            // Each bar animates from 0 to 60 over 500ms with EaseInOut easing
            let bar1 = t.add_with_easing(0, 500, 0.0, 60.0, Easing::EaseInOut);
            let bar2 = t.add_with_easing(100, 500, 0.0, 60.0, Easing::EaseInOut);
            let bar3 = t.add_with_easing(200, 500, 0.0, 60.0, Easing::EaseInOut);

            // Enable alternate (ping-pong) mode and infinite looping
            t.set_alternate(true);
            t.set_loop(-1);
            t.start();

            (bar1, bar2, bar3)
        });

        // Get current positions from timeline
        let bar1_x = timeline.get(bar1_id).unwrap_or(0.0);
        let bar2_x = timeline.get(bar2_id).unwrap_or(0.0);
        let bar3_x = timeline.get(bar3_id).unwrap_or(0.0);

        div()
            .w(140.0)
            .h(180.0)
            .bg(Color::rgba(0.15, 0.15, 0.2, 1.0))
            .rounded(16.0)
            .flex_col()
            .items_center()
            .p(8.0)
            .gap_px(8.0)
            // Label
            .child(
                text("Stagger")
                    .size(14.0)
                    .weight(FontWeight::SemiBold)
                    .color(Color::rgba(1.0, 1.0, 1.0, 0.7)),
            )
            // Bars container
            .child(
                div()
                    .w(100.0)
                    .h(100.0)
                    .flex_col()
                    .items_start()
                    .justify_center()
                    .gap_px(12.0)
                    // Bar 1
                    .child(
                        div()
                            .w(30.0)
                            .h(12.0)
                            .bg(Color::rgba(0.3, 0.9, 0.5, 1.0))
                            .rounded(6.0)
                            .transform(Transform::translate(bar1_x, 0.0)),
                    )
                    // Bar 2
                    .child(
                        div()
                            .w(30.0)
                            .h(12.0)
                            .bg(Color::rgba(0.9, 0.7, 0.2, 1.0))
                            .rounded(6.0)
                            .transform(Transform::translate(bar2_x, 0.0)),
                    )
                    // Bar 3
                    .child(
                        div()
                            .w(30.0)
                            .h(12.0)
                            .bg(Color::rgba(0.9, 0.3, 0.5, 1.0))
                            .rounded(6.0)
                            .transform(Transform::translate(bar3_x, 0.0)),
                    ),
            )
    })
}

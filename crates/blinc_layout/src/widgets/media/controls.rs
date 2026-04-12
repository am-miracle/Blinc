//! Shared media playback controls
//!
//! Generic over any `Player` implementation (audio or video).

use std::rc::Rc;

use blinc_core::Color;
use blinc_media::Player;

use crate::div::{div, Div};
use crate::svg::svg;
use crate::text::text;

use super::format_time::format_time_ms;

const PLAY_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor"><path d="M8 5v14l11-7z"/></svg>"#;
const PAUSE_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor"><path d="M6 19h4V5H6v14zm8-14v14h4V5h-4z"/></svg>"#;

/// Shared media controls — play/pause, seek bar, time, volume.
///
/// CSS classes for styling:
/// - `.blinc-media-controls` — outer row
/// - `.blinc-media-play-btn` — play/pause button
/// - `.blinc-media-time` — time display
/// - `.blinc-media-live-badge` — LIVE indicator
/// - `.blinc-media-seek-track` — seek bar track
/// - `.blinc-media-seek-fill` — seek bar fill
/// - `.blinc-media-volume` — volume display
pub struct MediaControls {
    inner: Div,
}

impl MediaControls {
    pub fn new<P: Player + 'static>(player: Rc<P>) -> Self {
        let position = player.position_ms();
        let duration = player.duration_ms();
        let is_playing = player.is_playing();
        let volume = player.volume();
        let is_live = player.is_live();

        let play_icon = if is_playing { PAUSE_SVG } else { PLAY_SVG };
        let player_for_click = Rc::clone(&player);

        let mut row = div()
            .flex_row()
            .items_center()
            .gap_px(8.0)
            .p_px(4.0)
            .class("blinc-media-controls")
            .child(
                div()
                    .w(28.0)
                    .h(28.0)
                    .rounded(14.0)
                    .bg(Color::rgba(0.2, 0.2, 0.25, 1.0))
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .class("blinc-media-play-btn")
                    .child(svg(play_icon).square(14.0).color(Color::WHITE))
                    .on_click(move |_| {
                        if player_for_click.is_playing() {
                            player_for_click.pause();
                        } else {
                            player_for_click.play();
                        }
                    }),
            );

        if is_live {
            row = row.child(
                div()
                    .bg(Color::rgba(0.8, 0.2, 0.2, 1.0))
                    .rounded(3.0)
                    .p_px(4.0)
                    .class("blinc-media-live-badge")
                    .child(text("LIVE").size(9.0).color(Color::WHITE).bold()),
            );
        } else {
            row = row.child(
                text(format!(
                    "{} / {}",
                    format_time_ms(position),
                    format_time_ms(duration)
                ))
                .size(11.0)
                .color(Color::rgba(0.6, 0.6, 0.7, 1.0))
                .monospace()
                .class("blinc-media-time"),
            );
        }

        let progress = if duration > 0 {
            (position as f32 / duration as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };

        if !is_live {
            row = row.child(
                div()
                    .flex_grow()
                    .h(4.0)
                    .bg(Color::rgba(0.2, 0.2, 0.25, 1.0))
                    .rounded(2.0)
                    .overflow_clip()
                    .class("blinc-media-seek-track")
                    .child({
                        let mut fill = div()
                            .h(4.0)
                            .bg(Color::rgba(0.3, 0.6, 1.0, 1.0))
                            .rounded(2.0)
                            .class("blinc-media-seek-fill");
                        if progress > 0.0 {
                            // Use flex_grow as a proportion
                            fill = fill.flex_grow();
                        }
                        fill
                    }),
            );
        }

        row = row.child(
            text(format!("{}%", (volume * 100.0) as u32))
                .size(10.0)
                .color(Color::rgba(0.5, 0.5, 0.55, 1.0))
                .monospace()
                .class("blinc-media-volume"),
        );

        Self { inner: row }
    }

    pub fn class(mut self, class: &str) -> Self {
        self.inner = self.inner.class(class);
        self
    }

    pub fn into_div(self) -> Div {
        self.inner
    }
}

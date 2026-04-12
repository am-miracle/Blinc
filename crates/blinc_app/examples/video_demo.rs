//! Video Player Demo
//!
//! Demonstrates `blinc_media::VideoPlayer` with the `video_player` widget:
//! - MP4 file loading + H.264 decoding via OpenH264
//! - Play/pause/stop controls via `MediaControls`
//! - Frame display on a canvas element
//!
//! `VideoPlayer` captures the UI dirty flag at construction and sets it
//! when each decoded frame arrives. The frame loop picks this up and
//! repaints the canvas with the latest frame.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p blinc_app --example video_demo --features windowed
//! ```

use std::rc::Rc;
use std::sync::OnceLock;

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_core::Color;
use blinc_layout::widgets::media::video_player;
use blinc_media::VideoPlayer;

const VIDEO_PATH: &str = "crates/blinc_app/examples/assets/german-shepherd-hd_1920_1080_25fps.mp4";

static PLAYER: OnceLock<VideoPlayer> = OnceLock::new();

fn shared_player() -> VideoPlayer {
    PLAYER
        .get_or_init(|| {
            let p = VideoPlayer::new();
            p.load_file(VIDEO_PATH);
            p
        })
        .clone()
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let config = WindowConfig {
        title: "Video Player Demo".to_string(),
        width: 960,
        height: 640,
        resizable: true,
        ..Default::default()
    };

    blinc_app::windowed::WindowedApp::run(config, build_ui)
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    let player = Rc::new(shared_player());

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.05, 0.06, 0.10, 1.0))
        .flex_col()
        .child(
            div()
                .w_full()
                .h(48.0)
                .bg(Color::rgba(0.09, 0.10, 0.14, 1.0))
                .flex_row()
                .items_center()
                .justify_center()
                .child(
                    text("Video Player Demo")
                        .size(16.0)
                        .weight(FontWeight::SemiBold)
                        .color(Color::rgba(0.95, 0.95, 1.0, 1.0)),
                ),
        )
        .child(
            div()
                .flex_grow()
                .w_full()
                .items_center()
                .justify_center()
                .p(16.0)
                .child(
                    video_player(player)
                        .w_full()
                        .bg(Color::rgba(0.0, 0.0, 0.0, 1.0))
                        .rounded(8.0)
                        .into_div(),
                ),
        )
}

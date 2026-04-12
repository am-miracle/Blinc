//! Video Player Demo — minimal test
//!
//! Run with:
//! ```sh
//! cargo run -p blinc_app --example video_demo --features windowed
//! ```

use std::rc::Rc;
use std::sync::OnceLock;

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_core::{Brush, Color, DrawContext, Rect};
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

    blinc_app::windowed::WindowedApp::run(
        WindowConfig {
            title: "Video Player Demo".into(),
            width: 960,
            height: 640,
            ..Default::default()
        },
        build_ui,
    )
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    let player = shared_player();

    let player_for_canvas = player.clone();

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.05, 0.06, 0.10, 1.0))
        .flex_col()
        .items_center()
        .child(
            text("Video Player Demo")
                .size(18.0)
                .weight(FontWeight::Bold)
                .color(Color::WHITE),
        )
        .child(
            canvas(move |ctx: &mut dyn DrawContext, bounds| {
                if let Some(frame) = player_for_canvas.current_frame() {
                    let rgba = frame.as_rgba();
                    ctx.draw_rgba_pixels(
                        &rgba,
                        frame.width,
                        frame.height,
                        Rect::new(0.0, 0.0, bounds.width, bounds.height),
                    );
                } else {
                    // Draw red so we can see the canvas has size
                    ctx.fill_rect(
                        Rect::new(0.0, 0.0, bounds.width, bounds.height),
                        0.0.into(),
                        Brush::Solid(Color::rgba(0.5, 0.0, 0.0, 1.0)),
                    );
                }
            })
            .w(ctx.width - 32.0)
            .h(ctx.height - 100.0),
        )
}

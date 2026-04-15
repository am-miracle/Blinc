//! Video Player Demo
//!
//! Demonstrates the video_player widget with `blinc_media::VideoPlayer` instance and controls.
//!
//! Run with:
//! ```sh
//! cargo run -p blinc_app_examples --example video_demo --features windowed
//! ```

use std::rc::Rc;
use std::sync::OnceLock;

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_core::Color;
use blinc_layout::widgets::media::video_player;
use blinc_media::VideoPlayer;
#[cfg(target_arch = "wasm32")]
use blinc_platform::assets::load_asset;

const VIDEO_PATH: &str =
    "examples/blinc_app_examples/examples/assets/german-shepherd-hd_1920_1080_25fps.mp4";

static PLAYER: OnceLock<VideoPlayer> = OnceLock::new();

fn shared_player() -> VideoPlayer {
    PLAYER
        .get_or_init(|| {
            let p = VideoPlayer::new();

            #[cfg(not(target_arch = "wasm32"))]
            p.load_file(VIDEO_PATH);

            // On wasm the wrapper background-spawns `WebAssetLoader::
            // preload`, so `load_asset` may fail at `build_ui` time
            // with "asset not preloaded". Poll on a 100 ms retry loop
            // until the fetch lands and feed the bytes into the
            // player — `build_ui` returns immediately with the player
            // already wired into the widget tree, so controls appear
            // straight away and come alive the moment the source is
            // attached.
            #[cfg(target_arch = "wasm32")]
            {
                let player = p.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    loop {
                        if let Ok(bytes) = load_asset(VIDEO_PATH) {
                            player.load_bytes(bytes);
                            break;
                        }
                        sleep_ms(100).await;
                    }
                });
            }

            p
        })
        .clone()
}

#[cfg(target_arch = "wasm32")]
async fn sleep_ms(ms: u32) {
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;
    let promise = js_sys::Promise::new(&mut |resolve: js_sys::Function, _reject| {
        web_sys::window().and_then(|w| {
            w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32)
                .ok()
        });
    });
    let _ = JsFuture::from(promise).await;
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

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.05, 0.06, 0.10, 1.0))
        .flex_col()
        .items_center()
        .justify_center()
        .p_px(16.0)
        .child(
            video_player(Rc::new(player))
                .w(ctx.width - 32.0)
                .h(ctx.height - 32.0)
                .bg(Color::BLACK)
                .rounded(8.0)
                .into_div(),
        )
}

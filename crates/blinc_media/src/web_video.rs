//! Browser-native video playback via `<video>` element.
//!
//! Frame pixels are captured each tick via a hidden `<canvas>` + `getImageData`.

use crate::frame::Frame;
use std::cell::RefCell;
use std::collections::HashMap;

use wasm_bindgen::JsCast;
use web_sys::{
    Blob, BlobPropertyBag, CanvasRenderingContext2d, HtmlCanvasElement, HtmlVideoElement, Url,
};

struct WebVideoState {
    video: HtmlVideoElement,
    canvas: HtmlCanvasElement,
    ctx2d: CanvasRenderingContext2d,
    width: u32,
    height: u32,
    ready: bool,
}

thread_local! {
    static WEB_VIDEOS: RefCell<HashMap<u64, WebVideoState>> = RefCell::new(HashMap::new());
}

fn document() -> web_sys::Document {
    web_sys::window().unwrap().document().unwrap()
}

pub(crate) fn create(player_id: u64) {
    let video: HtmlVideoElement = document()
        .create_element("video")
        .unwrap()
        .dyn_into()
        .unwrap();
    video.set_attribute("playsinline", "").ok();
    video.set_attribute("preload", "auto").ok();
    video.style().set_property("display", "none").ok();
    document().body().unwrap().append_child(&video).ok();

    let canvas: HtmlCanvasElement = document()
        .create_element("canvas")
        .unwrap()
        .dyn_into()
        .unwrap();
    canvas.style().set_property("display", "none").ok();
    document().body().unwrap().append_child(&canvas).ok();

    let ctx2d: CanvasRenderingContext2d = canvas
        .get_context("2d")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();

    WEB_VIDEOS.with(|map| {
        map.borrow_mut().insert(
            player_id,
            WebVideoState {
                video,
                canvas,
                ctx2d,
                width: 0,
                height: 0,
                ready: false,
            },
        );
    });
}

pub(crate) fn load_bytes(player_id: u64, bytes: &[u8]) {
    WEB_VIDEOS.with(|map| {
        let map = map.borrow();
        let Some(state) = map.get(&player_id) else {
            return;
        };

        let array = js_sys::Uint8Array::new_with_length(bytes.len() as u32);
        array.copy_from(bytes);
        let parts = js_sys::Array::new();
        parts.push(&array.buffer());

        let mut opts = BlobPropertyBag::new();
        opts.type_("video/mp4");
        let blob = Blob::new_with_buffer_source_sequence_and_options(&parts, &opts).unwrap();
        let url = Url::create_object_url_with_blob(&blob).unwrap();

        state.video.set_src(&url);
        state.video.load();
    });
}

pub(crate) fn play(player_id: u64) {
    WEB_VIDEOS.with(|map| {
        let map = map.borrow();
        if let Some(state) = map.get(&player_id) {
            let _ = state.video.play();
        }
    });
}

pub(crate) fn pause(player_id: u64) {
    WEB_VIDEOS.with(|map| {
        let map = map.borrow();
        if let Some(state) = map.get(&player_id) {
            state.video.pause().ok();
        }
    });
}

pub(crate) fn seek(player_id: u64, position_ms: u64) {
    WEB_VIDEOS.with(|map| {
        let map = map.borrow();
        if let Some(state) = map.get(&player_id) {
            state.video.set_current_time(position_ms as f64 / 1000.0);
        }
    });
}

pub(crate) fn set_volume(player_id: u64, volume: f32) {
    WEB_VIDEOS.with(|map| {
        let map = map.borrow();
        if let Some(state) = map.get(&player_id) {
            state.video.set_volume(volume as f64);
        }
    });
}

pub(crate) fn duration_ms(player_id: u64) -> u64 {
    WEB_VIDEOS.with(|map| {
        let map = map.borrow();
        map.get(&player_id)
            .map(|s| (s.video.duration() * 1000.0) as u64)
            .unwrap_or(0)
    })
}

pub(crate) fn position_ms(player_id: u64) -> u64 {
    WEB_VIDEOS.with(|map| {
        let map = map.borrow();
        map.get(&player_id)
            .map(|s| (s.video.current_time() * 1000.0) as u64)
            .unwrap_or(0)
    })
}

pub(crate) fn is_paused(player_id: u64) -> bool {
    WEB_VIDEOS.with(|map| {
        let map = map.borrow();
        map.get(&player_id)
            .map(|s| s.video.paused())
            .unwrap_or(true)
    })
}

pub(crate) fn is_ended(player_id: u64) -> bool {
    WEB_VIDEOS.with(|map| {
        let map = map.borrow();
        map.get(&player_id)
            .map(|s| s.video.ended())
            .unwrap_or(false)
    })
}

/// Capture the current video frame as RGBA pixels.
/// Returns None if the video isn't ready or has no dimensions.
pub(crate) fn capture_frame(player_id: u64) -> Option<Frame> {
    WEB_VIDEOS.with(|map| {
        let mut map = map.borrow_mut();
        let state = map.get_mut(&player_id)?;

        let vw = state.video.video_width();
        let vh = state.video.video_height();
        if vw == 0 || vh == 0 {
            return None;
        }

        if !state.ready || state.width != vw || state.height != vh {
            state.canvas.set_width(vw);
            state.canvas.set_height(vh);
            state.width = vw;
            state.height = vh;
            state.ready = true;
        }

        state
            .ctx2d
            .draw_image_with_html_video_element(&state.video, 0.0, 0.0)
            .ok()?;

        let image_data = state
            .ctx2d
            .get_image_data(0.0, 0.0, vw as f64, vh as f64)
            .ok()?;

        let rgba = image_data.data().0;
        Some(Frame::from_rgba(rgba, vw, vh))
    })
}

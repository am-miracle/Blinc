//! Video decoding — RGBA frames from H.264 streams
//!
//! Desktop: OpenH264 (royalty-free, Cisco covers patents)
//! Mobile: platform decoders via native bridge
//!
//! # Example
//!
//! ```ignore
//! use blinc_media::video::VideoDecoder;
//!
//! let mut decoder = VideoDecoder::new();
//! if let Some(frame) = decoder.decode_nal(h264_packet) {
//!     canvas_render(frame.as_rgba(), frame.width, frame.height);
//! }
//! ```

use crate::frame::Frame;

/// Video playback state
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoState {
    Idle,
    Playing,
    Paused,
    Ended,
}

/// Re-export Frame as VideoFrame for convenience
pub type VideoFrame = Frame;

/// Video decoder — extracts RGBA frames from H.264 NAL units
pub struct VideoDecoder {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    decoder: Option<openh264::decoder::Decoder>,
    state: VideoState,
}

impl VideoDecoder {
    pub fn new() -> Self {
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            let decoder = openh264::decoder::Decoder::new().ok();
            Self {
                decoder,
                state: VideoState::Idle,
            }
        }

        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            Self {
                state: VideoState::Idle,
            }
        }
    }

    /// Decode a single H.264 NAL unit and return an RGBA frame if available
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    pub fn decode_nal(&mut self, nal_data: &[u8]) -> Option<Frame> {
        let decoder = self.decoder.as_mut()?;
        let yuv = decoder.decode(nal_data).ok()??;

        // UV dimensions are half of full dimensions in I420
        let (uv_w, uv_h) = yuv.dimensions_uv();
        let w = uv_w * 2;
        let h = uv_h * 2;
        let mut rgba = vec![0u8; w * h * 4];
        yuv.write_rgba8(&mut rgba);

        self.state = VideoState::Playing;
        Some(Frame::from_rgba(rgba, w as u32, h as u32))
    }

    #[cfg(any(target_os = "android", target_os = "ios"))]
    pub fn decode_nal(&mut self, _nal_data: &[u8]) -> Option<Frame> {
        tracing::warn!("Use native_stream for mobile video decoding");
        None
    }

    pub fn state(&self) -> VideoState {
        self.state
    }
}

impl Default for VideoDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Video player with playback controls
///
/// Wraps a decoder and provides play/pause/seek/stop.
/// Frames are delivered to a callback or polled via `current_frame()`.
///
/// Desktop: decodes locally via OpenH264
/// Mobile: delegates to platform player via native bridge
///
/// # Example
///
/// ```ignore
/// use blinc_media::video::VideoPlayer;
///
/// let player = VideoPlayer::new();
/// player.load("video.h264");
/// player.play();
/// player.set_volume(0.8);
///
/// // In render loop
/// if let Some(frame) = player.current_frame() {
///     canvas_render(&frame.as_rgba(), frame.width, frame.height);
/// }
/// ```
pub struct VideoPlayer {
    state: std::sync::Arc<std::sync::Mutex<VideoPlayerInner>>,
}

struct VideoPlayerInner {
    playback_state: VideoState,
    volume: f32,
    current_frame: Option<Frame>,
    source: Option<String>,
    position_ms: u64,
    duration_ms: u64,
}

impl VideoPlayer {
    pub fn new() -> Self {
        Self {
            state: std::sync::Arc::new(std::sync::Mutex::new(VideoPlayerInner {
                playback_state: VideoState::Idle,
                volume: 1.0,
                current_frame: None,
                source: None,
                position_ms: 0,
                duration_ms: 0,
            })),
        }
    }

    /// Load a video source
    pub fn load(&self, path: &str) {
        let mut inner = self.state.lock().unwrap();
        inner.source = Some(path.to_string());
        inner.playback_state = VideoState::Idle;
        inner.position_ms = 0;

        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            let _ = blinc_core::native_bridge::native_call::<(), _>(
                "video",
                "load",
                vec![blinc_core::native_bridge::NativeValue::String(
                    path.to_string(),
                )],
            );
        }
    }

    /// Start or resume playback
    pub fn play(&self) {
        let mut inner = self.state.lock().unwrap();
        inner.playback_state = VideoState::Playing;

        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            let _ = blinc_core::native_bridge::native_call::<(), _>("video", "play", ());
        }
    }

    /// Pause playback
    pub fn pause(&self) {
        let mut inner = self.state.lock().unwrap();
        inner.playback_state = VideoState::Paused;

        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            let _ = blinc_core::native_bridge::native_call::<(), _>("video", "pause", ());
        }
    }

    /// Stop playback and reset position
    pub fn stop(&self) {
        let mut inner = self.state.lock().unwrap();
        inner.playback_state = VideoState::Idle;
        inner.position_ms = 0;
        inner.current_frame = None;

        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            let _ = blinc_core::native_bridge::native_call::<(), _>("video", "stop", ());
        }
    }

    /// Seek to a position in milliseconds
    pub fn seek(&self, position_ms: u64) {
        let mut inner = self.state.lock().unwrap();
        inner.position_ms = position_ms;

        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            let _ = blinc_core::native_bridge::native_call::<(), _>(
                "video",
                "seek",
                vec![blinc_core::native_bridge::NativeValue::Int64(
                    position_ms as i64,
                )],
            );
        }
    }

    /// Set volume (0.0 to 1.0)
    pub fn set_volume(&self, volume: f32) {
        self.state.lock().unwrap().volume = volume.clamp(0.0, 1.0);

        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            let _ = blinc_core::native_bridge::native_call::<(), _>(
                "video",
                "set_volume",
                vec![blinc_core::native_bridge::NativeValue::Float32(volume)],
            );
        }
    }

    /// Get the current decoded frame
    pub fn current_frame(&self) -> Option<Frame> {
        self.state.lock().unwrap().current_frame.clone()
    }

    /// Push a decoded frame (called by decoder thread or native bridge)
    pub fn push_frame(&self, frame: Frame) {
        self.state.lock().unwrap().current_frame = Some(frame);
    }

    /// Get playback state
    pub fn playback_state(&self) -> VideoState {
        self.state.lock().unwrap().playback_state
    }

    /// Get current position in milliseconds
    pub fn position_ms(&self) -> u64 {
        self.state.lock().unwrap().position_ms
    }

    /// Get duration in milliseconds
    pub fn duration_ms(&self) -> u64 {
        self.state.lock().unwrap().duration_ms
    }

    /// Get volume
    pub fn volume(&self) -> f32 {
        self.state.lock().unwrap().volume
    }

    /// Check if playing
    pub fn is_playing(&self) -> bool {
        self.playback_state() == VideoState::Playing
    }
}

impl Default for VideoPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::player::Player for VideoPlayer {
    fn play(&self) {
        VideoPlayer::play(self);
    }
    fn pause(&self) {
        VideoPlayer::pause(self);
    }
    fn stop(&self) {
        VideoPlayer::stop(self);
    }
    fn seek(&self, position_ms: u64) {
        VideoPlayer::seek(self, position_ms);
    }
    fn position_ms(&self) -> u64 {
        VideoPlayer::position_ms(self)
    }
    fn duration_ms(&self) -> u64 {
        VideoPlayer::duration_ms(self)
    }
    fn volume(&self) -> f32 {
        VideoPlayer::volume(self)
    }
    fn set_volume(&self, volume: f32) {
        VideoPlayer::set_volume(self, volume);
    }
    fn is_playing(&self) -> bool {
        VideoPlayer::is_playing(self)
    }
}

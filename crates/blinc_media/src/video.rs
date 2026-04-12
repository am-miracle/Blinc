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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VideoState {
    #[default]
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
/// Cloning shares the same playback state — all clones see the same
/// frames, position, and play/pause state via the inner `Arc<Mutex>`.
#[derive(Clone)]
pub struct VideoPlayer {
    state: std::sync::Arc<std::sync::Mutex<VideoPlayerInner>>,
    /// Change counter signal. Incremented on ANY state change — new
    /// frame, play/pause, position update, volume change. UI widgets
    /// depend on this signal's ID via `Stateful::deps([player.signal()])`
    /// so only the video widget rebuilds, not the entire tree.
    change_counter: blinc_core::State<u64>,
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
    #[track_caller]
    pub fn new() -> Self {
        let loc = std::panic::Location::caller();
        let key = format!(
            "video_player:{}:{}:{}",
            loc.file(),
            loc.line(),
            loc.column()
        );
        let ctx = blinc_core::BlincContextState::get();
        let change_counter = ctx.use_state_keyed(&key, || 0u64);
        Self {
            state: std::sync::Arc::new(std::sync::Mutex::new(VideoPlayerInner {
                playback_state: VideoState::Idle,
                volume: 1.0,
                current_frame: None,
                source: None,
                position_ms: 0,
                duration_ms: 0,
            })),
            change_counter,
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
        self.state.lock().unwrap().playback_state = VideoState::Playing;
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            let _ = blinc_core::native_bridge::native_call::<(), _>("video", "play", ());
        }
        self.notify();
    }

    /// Pause playback
    pub fn pause(&self) {
        self.state.lock().unwrap().playback_state = VideoState::Paused;
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            let _ = blinc_core::native_bridge::native_call::<(), _>("video", "pause", ());
        }
        self.notify();
    }

    /// Stop playback and reset position
    pub fn stop(&self) {
        {
            let mut inner = self.state.lock().unwrap();
            inner.playback_state = VideoState::Idle;
            inner.position_ms = 0;
            inner.current_frame = None;
        }
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            let _ = blinc_core::native_bridge::native_call::<(), _>("video", "stop", ());
        }
        self.notify();
    }

    /// Seek to a position in milliseconds
    pub fn seek(&self, position_ms: u64) {
        self.state.lock().unwrap().position_ms = position_ms;
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
        self.notify();
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
        self.notify();
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

    /// Signal ID for state changes. Pass into a `Stateful` widget's
    /// `.deps([...])` list so it rebuilds on any player state change
    /// (new frame, play/pause, position, volume).
    pub fn signal(&self) -> blinc_core::SignalId {
        self.change_counter.signal_id()
    }

    fn notify(&self) {
        self.change_counter.update(|n| n + 1);
    }

    /// Load and play an MP4 file from disk.
    ///
    /// Spawns a background thread that demuxes the MP4 container,
    /// extracts H.264 NAL units from the video track, decodes each
    /// frame via OpenH264, and pushes the resulting RGBA frames at the
    /// video's native framerate. The `video_player` widget displays
    /// whatever `current_frame()` returns each render tick.
    ///
    /// Playback can be paused/resumed/stopped via the `Player` trait
    /// methods — the background thread checks `playback_state` each
    /// frame and sleeps when paused.
    ///
    /// # Panics
    ///
    /// Panics if the file can't be read or doesn't contain an H.264
    /// video track.
    /// Load and play an MP4 file from disk. Convenience wrapper around
    /// [`Self::load_bytes`] that reads the file first.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    pub fn load_file(&self, path: &str) {
        let bytes =
            std::fs::read(path).unwrap_or_else(|e| panic!("failed to read video: {path}: {e}"));
        self.load_bytes(bytes);
    }

    /// Load and play an MP4 from in-memory bytes.
    ///
    /// Works cross-platform — on wasm, fetch the MP4 via the asset
    /// loader or `fetch()` and pass the bytes here. On desktop, use
    /// [`Self::load_file`] for convenience or read the bytes yourself.
    ///
    /// Spawns a background thread that demuxes the container, extracts
    /// H.264 NAL units, decodes via OpenH264, and pushes RGBA frames
    /// at the video's native framerate.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    pub fn load_bytes(&self, bytes: Vec<u8>) {
        let file_size = bytes.len() as u64;

        // First pass: extract metadata + SPS/PPS from the header
        let mp4 = mp4::Mp4Reader::read_header(std::io::Cursor::new(&bytes), file_size)
            .unwrap_or_else(|e| panic!("failed to parse MP4: {e}"));

        let video_track_id = mp4
            .tracks()
            .values()
            .find(|t| t.media_type().ok() == Some(mp4::MediaType::H264))
            .map(|t| t.track_id())
            .unwrap_or_else(|| panic!("no H.264 video track found"));

        let track = &mp4.tracks()[&video_track_id];
        let sample_count = track.sample_count();
        let duration_ms = track.duration().as_millis() as u64;

        {
            let mut inner = self.state.lock().unwrap();
            inner.duration_ms = duration_ms;
            inner.playback_state = VideoState::Playing;
        }

        let mut parameter_sets: Vec<Vec<u8>> = Vec::new();
        if let Some(ref avc1) = track.trak.mdia.minf.stbl.stsd.avc1 {
            for sps in &avc1.avcc.sequence_parameter_sets {
                let mut nal = vec![0x00, 0x00, 0x00, 0x01];
                nal.extend_from_slice(&sps.bytes);
                parameter_sets.push(nal);
            }
            for pps in &avc1.avcc.picture_parameter_sets {
                let mut nal = vec![0x00, 0x00, 0x00, 0x01];
                nal.extend_from_slice(&pps.bytes);
                parameter_sets.push(nal);
            }
        }

        let state = self.state.clone();
        let change_counter = self.change_counter.clone();

        std::thread::spawn(move || {
            let mut decoder = VideoDecoder::new();

            tracing::info!(
                "video: starting decode thread, {} SPS/PPS sets, {} samples, {}ms duration",
                parameter_sets.len(),
                sample_count,
                duration_ms
            );

            for (i, ps) in parameter_sets.iter().enumerate() {
                tracing::debug!("video: feeding parameter set {} ({} bytes)", i, ps.len());
                decoder.decode_nal(ps);
            }

            // Second pass: re-parse from the same bytes for sample reading
            let mut mp4 = match mp4::Mp4Reader::read_header(std::io::Cursor::new(&bytes), file_size)
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("failed to re-parse MP4: {e}");
                    return;
                }
            };

            let frame_duration = if sample_count > 0 {
                std::time::Duration::from_millis(duration_ms / sample_count as u64)
            } else {
                std::time::Duration::from_millis(33)
            };

            for sample_idx in 1..=sample_count {
                loop {
                    let ps = state.lock().unwrap().playback_state;
                    match ps {
                        VideoState::Playing => break,
                        VideoState::Paused => {
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            continue;
                        }
                        VideoState::Idle | VideoState::Ended => return,
                    }
                }

                let sample = match mp4.read_sample(video_track_id, sample_idx) {
                    Ok(Some(s)) => s,
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!("failed to read sample {sample_idx}: {e}");
                        continue;
                    }
                };

                if sample_idx <= 3 {
                    tracing::info!(
                        "video: sample {} size={} bytes",
                        sample_idx,
                        sample.bytes.len()
                    );
                }

                // Convert length-prefixed NALUs to Annex B for OpenH264
                let sample_bytes = &sample.bytes;
                let mut offset = 0;
                while offset + 4 <= sample_bytes.len() {
                    let nal_len = u32::from_be_bytes([
                        sample_bytes[offset],
                        sample_bytes[offset + 1],
                        sample_bytes[offset + 2],
                        sample_bytes[offset + 3],
                    ]) as usize;
                    offset += 4;
                    if offset + nal_len > sample_bytes.len() {
                        break;
                    }
                    let mut annex_b = vec![0x00, 0x00, 0x00, 0x01];
                    annex_b.extend_from_slice(&sample_bytes[offset..offset + nal_len]);
                    offset += nal_len;

                    let result = decoder.decode_nal(&annex_b);
                    if sample_idx <= 3 {
                        tracing::info!(
                            "video: NAL {} bytes, nal_type={}, decoded={}",
                            annex_b.len(),
                            annex_b.get(4).map(|b| b & 0x1f).unwrap_or(0),
                            result.is_some()
                        );
                    }
                    if let Some(frame) = result {
                        let mut inner = state.lock().unwrap();
                        inner.current_frame = Some(frame);
                        inner.position_ms = (sample_idx as u64 * duration_ms) / sample_count as u64;
                        drop(inner);
                        change_counter.update(|n| n + 1);
                    }
                }

                std::thread::sleep(frame_duration);
            }

            state.lock().unwrap().playback_state = VideoState::Ended;
        });
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

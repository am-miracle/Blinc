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

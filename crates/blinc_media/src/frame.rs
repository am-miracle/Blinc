//! Platform-agnostic AV frame data utilities
//!
//! Video frames: color space conversion, scaling, pixel format handling.
//! Audio samples: format conversion, resampling, channel mixing.
//! Useful for building custom media tools, filters, or renderers.
//!
//! # Video Frames
//!
//! ```ignore
//! use blinc_media::frame::{Frame, PixelFormat};
//!
//! let frame = Frame::from_rgba(rgba_bytes, 640, 480);
//! let rgb = frame.to_rgb();
//! let small = frame.scale(320, 240);
//! let gray = frame.to_gray();
//! ```
//!
//! # Audio Samples
//!
//! ```ignore
//! use blinc_media::frame::{AudioSamples, SampleFormat};
//!
//! let samples = AudioSamples::from_f32(&pcm_data, 2, 44100);
//! let mono = samples.to_mono();
//! let resampled = samples.resample(48000);
//! let as_f32 = samples.as_f32();
//! ```

/// Pixel format
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// 4 bytes per pixel: R, G, B, A
    Rgba,
    /// 3 bytes per pixel: R, G, B
    Rgb,
    /// YUV 4:2:0 planar (Y plane + half-size U plane + half-size V plane)
    Yuv420,
    /// 4 bytes per pixel: B, G, R, A (common in platform APIs)
    Bgra,
    /// Single channel grayscale
    Gray,
}

impl PixelFormat {
    /// Bytes per pixel for packed formats (None for planar)
    pub fn bytes_per_pixel(&self) -> Option<usize> {
        match self {
            PixelFormat::Rgba | PixelFormat::Bgra => Some(4),
            PixelFormat::Rgb => Some(3),
            PixelFormat::Gray => Some(1),
            PixelFormat::Yuv420 => None, // Planar
        }
    }
}

/// A video/image frame with pixel data
#[derive(Clone)]
pub struct Frame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Presentation timestamp in milliseconds (0 for images)
    pub pts_ms: u64,
}

impl Frame {
    /// Create from RGBA pixel data
    pub fn from_rgba(data: Vec<u8>, width: u32, height: u32) -> Self {
        Self {
            data,
            width,
            height,
            format: PixelFormat::Rgba,
            pts_ms: 0,
        }
    }

    /// Create from RGB pixel data
    pub fn from_rgb(data: Vec<u8>, width: u32, height: u32) -> Self {
        Self {
            data,
            width,
            height,
            format: PixelFormat::Rgb,
            pts_ms: 0,
        }
    }

    /// Create from BGRA pixel data
    pub fn from_bgra(data: Vec<u8>, width: u32, height: u32) -> Self {
        Self {
            data,
            width,
            height,
            format: PixelFormat::Bgra,
            pts_ms: 0,
        }
    }

    /// Set presentation timestamp
    pub fn with_pts(mut self, pts_ms: u64) -> Self {
        self.pts_ms = pts_ms;
        self
    }

    /// Convert to RGBA format
    pub fn to_rgba(&self) -> Frame {
        match self.format {
            PixelFormat::Rgba => self.clone(),
            PixelFormat::Bgra => {
                let mut rgba = self.data.clone();
                for pixel in rgba.chunks_exact_mut(4) {
                    pixel.swap(0, 2); // B↔R
                }
                Frame::from_rgba(rgba, self.width, self.height).with_pts(self.pts_ms)
            }
            PixelFormat::Rgb => {
                let mut rgba = Vec::with_capacity((self.width * self.height * 4) as usize);
                for pixel in self.data.chunks_exact(3) {
                    rgba.extend_from_slice(pixel);
                    rgba.push(255);
                }
                Frame::from_rgba(rgba, self.width, self.height).with_pts(self.pts_ms)
            }
            PixelFormat::Gray => {
                let mut rgba = Vec::with_capacity((self.width * self.height * 4) as usize);
                for &g in &self.data {
                    rgba.extend_from_slice(&[g, g, g, 255]);
                }
                Frame::from_rgba(rgba, self.width, self.height).with_pts(self.pts_ms)
            }
            PixelFormat::Yuv420 => {
                let w = self.width as usize;
                let h = self.height as usize;
                let y_size = w * h;
                let uv_size = (w / 2) * (h / 2);
                let mut rgba = vec![0u8; w * h * 4];

                if self.data.len() >= y_size + uv_size * 2 {
                    let y_plane = &self.data[..y_size];
                    let u_plane = &self.data[y_size..y_size + uv_size];
                    let v_plane = &self.data[y_size + uv_size..];

                    for row in 0..h {
                        for col in 0..w {
                            let y = y_plane[row * w + col] as f32;
                            let u = u_plane[(row / 2) * (w / 2) + (col / 2)] as f32 - 128.0;
                            let v = v_plane[(row / 2) * (w / 2) + (col / 2)] as f32 - 128.0;

                            let px = (row * w + col) * 4;
                            rgba[px] = (y + 1.402 * v).clamp(0.0, 255.0) as u8;
                            rgba[px + 1] = (y - 0.344 * u - 0.714 * v).clamp(0.0, 255.0) as u8;
                            rgba[px + 2] = (y + 1.772 * u).clamp(0.0, 255.0) as u8;
                            rgba[px + 3] = 255;
                        }
                    }
                }
                Frame::from_rgba(rgba, self.width, self.height).with_pts(self.pts_ms)
            }
        }
    }

    /// Convert to RGB format
    pub fn to_rgb(&self) -> Frame {
        let rgba = self.to_rgba();
        let mut rgb = Vec::with_capacity((rgba.width * rgba.height * 3) as usize);
        for pixel in rgba.data.chunks_exact(4) {
            rgb.extend_from_slice(&pixel[..3]);
        }
        Frame {
            data: rgb,
            width: rgba.width,
            height: rgba.height,
            format: PixelFormat::Rgb,
            pts_ms: self.pts_ms,
        }
    }

    /// Convert to grayscale
    pub fn to_gray(&self) -> Frame {
        let rgba = self.to_rgba();
        let gray: Vec<u8> = rgba
            .data
            .chunks_exact(4)
            .map(|p| ((p[0] as u16 * 77 + p[1] as u16 * 150 + p[2] as u16 * 29) >> 8) as u8)
            .collect();
        Frame {
            data: gray,
            width: self.width,
            height: self.height,
            format: PixelFormat::Gray,
            pts_ms: self.pts_ms,
        }
    }

    /// Scale the frame using nearest-neighbor interpolation
    pub fn scale(&self, new_width: u32, new_height: u32) -> Frame {
        let rgba = self.to_rgba();
        let bpp = 4usize;
        let mut scaled = vec![0u8; (new_width * new_height) as usize * bpp];

        for y in 0..new_height {
            for x in 0..new_width {
                let src_x = (x as f32 * rgba.width as f32 / new_width as f32) as u32;
                let src_y = (y as f32 * rgba.height as f32 / new_height as f32) as u32;
                let src_idx = (src_y * rgba.width + src_x) as usize * bpp;
                let dst_idx = (y * new_width + x) as usize * bpp;
                if src_idx + bpp <= rgba.data.len() && dst_idx + bpp <= scaled.len() {
                    scaled[dst_idx..dst_idx + bpp]
                        .copy_from_slice(&rgba.data[src_idx..src_idx + bpp]);
                }
            }
        }

        Frame::from_rgba(scaled, new_width, new_height).with_pts(self.pts_ms)
    }

    /// Get RGBA data (converts if needed)
    pub fn as_rgba(&self) -> std::borrow::Cow<'_, [u8]> {
        if self.format == PixelFormat::Rgba {
            std::borrow::Cow::Borrowed(&self.data)
        } else {
            std::borrow::Cow::Owned(self.to_rgba().data)
        }
    }

    /// Total byte size of the pixel data
    pub fn byte_size(&self) -> usize {
        self.data.len()
    }
}

// ============================================================================
// Audio Samples
// ============================================================================

/// Audio sample format
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    /// 32-bit float (-1.0 to 1.0)
    F32,
    /// 16-bit signed integer
    I16,
    /// 8-bit unsigned integer
    U8,
}

/// A buffer of audio samples
#[derive(Clone)]
pub struct AudioSamples {
    pub data: Vec<u8>,
    pub format: SampleFormat,
    pub channels: u16,
    pub sample_rate: u32,
    pub pts_ms: u64,
}

impl AudioSamples {
    pub fn from_f32(samples: &[f32], channels: u16, sample_rate: u32) -> Self {
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        Self {
            data: bytes,
            format: SampleFormat::F32,
            channels,
            sample_rate,
            pts_ms: 0,
        }
    }

    pub fn from_i16(samples: &[i16], channels: u16, sample_rate: u32) -> Self {
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        Self {
            data: bytes,
            format: SampleFormat::I16,
            channels,
            sample_rate,
            pts_ms: 0,
        }
    }

    pub fn with_pts(mut self, pts_ms: u64) -> Self {
        self.pts_ms = pts_ms;
        self
    }

    /// Get samples as f32 (converts if needed)
    pub fn as_f32(&self) -> Vec<f32> {
        match self.format {
            SampleFormat::F32 => self
                .data
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect(),
            SampleFormat::I16 => self
                .data
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                .collect(),
            SampleFormat::U8 => self
                .data
                .iter()
                .map(|&b| (b as f32 - 128.0) / 128.0)
                .collect(),
        }
    }

    /// Samples per channel
    pub fn sample_count(&self) -> usize {
        let bps = match self.format {
            SampleFormat::F32 => 4,
            SampleFormat::I16 => 2,
            SampleFormat::U8 => 1,
        };
        self.data.len() / bps / self.channels as usize
    }

    /// Duration in milliseconds
    pub fn duration_ms(&self) -> f64 {
        self.sample_count() as f64 / self.sample_rate as f64 * 1000.0
    }

    /// Mix to mono
    pub fn to_mono(&self) -> Self {
        if self.channels == 1 {
            return self.clone();
        }
        let samples = self.as_f32();
        let ch = self.channels as usize;
        let mono: Vec<f32> = samples
            .chunks_exact(ch)
            .map(|frame| frame.iter().sum::<f32>() / ch as f32)
            .collect();
        Self::from_f32(&mono, 1, self.sample_rate).with_pts(self.pts_ms)
    }

    /// Resample to target rate (linear interpolation)
    pub fn resample(&self, target_rate: u32) -> Self {
        if target_rate == self.sample_rate {
            return self.clone();
        }
        let samples = self.as_f32();
        let ratio = self.sample_rate as f64 / target_rate as f64;
        let ch = self.channels as usize;
        let src_frames = samples.len() / ch;
        let dst_frames = (src_frames as f64 / ratio) as usize;
        let mut output = Vec::with_capacity(dst_frames * ch);
        for i in 0..dst_frames {
            let src_pos = i as f64 * ratio;
            let idx = src_pos as usize;
            let frac = (src_pos - idx as f64) as f32;
            for c in 0..ch {
                let s0 = samples.get(idx * ch + c).copied().unwrap_or(0.0);
                let s1 = samples.get((idx + 1) * ch + c).copied().unwrap_or(s0);
                output.push(s0 + (s1 - s0) * frac);
            }
        }
        Self::from_f32(&output, self.channels, target_rate).with_pts(self.pts_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rgba_roundtrip() {
        let rgba = vec![255, 0, 0, 255, 0, 255, 0, 255]; // 2 pixels
        let frame = Frame::from_rgba(rgba.clone(), 2, 1);
        assert_eq!(frame.to_rgba().data, rgba);
    }

    #[test]
    fn test_bgra_to_rgba() {
        let bgra = vec![0, 0, 255, 255]; // B=0, G=0, R=255, A=255
        let frame = Frame::from_bgra(bgra, 1, 1);
        let rgba = frame.to_rgba();
        assert_eq!(rgba.data, vec![255, 0, 0, 255]); // R=255, G=0, B=0, A=255
    }

    #[test]
    fn test_rgb_to_rgba() {
        let rgb = vec![100, 150, 200];
        let frame = Frame::from_rgb(rgb, 1, 1);
        let rgba = frame.to_rgba();
        assert_eq!(rgba.data, vec![100, 150, 200, 255]);
    }

    #[test]
    fn test_scale() {
        let rgba = [255, 0, 0, 255].repeat(4); // 2x2 red
        let frame = Frame::from_rgba(rgba, 2, 2);
        let scaled = frame.scale(4, 4);
        assert_eq!(scaled.width, 4);
        assert_eq!(scaled.height, 4);
        assert_eq!(scaled.data.len(), 4 * 4 * 4);
        // All pixels should still be red
        assert_eq!(scaled.data[0], 255);
        assert_eq!(scaled.data[1], 0);
    }

    #[test]
    fn test_grayscale() {
        let rgba = vec![255, 0, 0, 255]; // Pure red
        let frame = Frame::from_rgba(rgba, 1, 1);
        let gray = frame.to_gray();
        assert_eq!(gray.format, PixelFormat::Gray);
        assert_eq!(gray.data.len(), 1);
        // Red luminance ≈ 77/256 * 255 ≈ 76
        assert!(gray.data[0] > 70 && gray.data[0] < 85);
    }
}

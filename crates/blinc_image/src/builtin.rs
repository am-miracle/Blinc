//! Built-in [`ImageDecoder`] implementations backed by the upstream
//! `image` crate. Each decoder is gated behind its matching cargo
//! feature so apps that don't need a format don't pay its compile
//! cost.
//!
//! Each decoder calls `image::load_from_memory_with_format(...,
//! ImageFormat::X)` which only links the X-specific code path. A
//! decoder built with feature `png` therefore drops `gif`, `webp`,
//! `bmp`, `tiff`, `avif` from its dep tree even when other built-in
//! decoders are also present (the `image` crate gates each format
//! independently).
//!
//! The decoders rely on the `image` crate's auto-detection only as a
//! sanity check via [`image::guess_format`] in [`detect_format`];
//! actual decoding always pins to the explicit format.
//!
//! [`ImageDecoder`]: crate::decoder::ImageDecoder

#![allow(dead_code)] // built-ins are conditionally compiled per-feature

use image::{DynamicImage, GenericImageView};

use crate::decoder::{DecodedImage, ImageDecoder};
use crate::error::{ImageError, Result};
use crate::source::ImageFormat;

fn dynamic_to_decoded(img: DynamicImage) -> DecodedImage {
    let (width, height) = img.dimensions();
    let rgba = img.to_rgba8();
    DecodedImage {
        pixels: rgba.into_raw(),
        width,
        height,
    }
}

fn decode_with_format(bytes: &[u8], format: image::ImageFormat) -> Result<DecodedImage> {
    let img = image::load_from_memory_with_format(bytes, format)
        .map_err(|e| ImageError::Decode(format!("{format:?} decode failed: {e}")))?;
    Ok(dynamic_to_decoded(img))
}

/// Detect a format from magic bytes. Used by callers that hold raw
/// bytes without a format hint; cheap and side-effect-free.
pub fn detect_format(bytes: &[u8]) -> Option<ImageFormat> {
    let detected = image::guess_format(bytes).ok()?;
    match detected {
        image::ImageFormat::Png => Some(ImageFormat::Png),
        image::ImageFormat::Jpeg => Some(ImageFormat::Jpeg),
        image::ImageFormat::Gif => Some(ImageFormat::Gif),
        image::ImageFormat::WebP => Some(ImageFormat::WebP),
        image::ImageFormat::Bmp => Some(ImageFormat::Bmp),
        image::ImageFormat::Tiff => Some(ImageFormat::Tiff),
        image::ImageFormat::Avif => Some(ImageFormat::Avif),
        _ => None,
    }
}

#[cfg(feature = "png")]
pub struct PngDecoder;
#[cfg(feature = "png")]
impl ImageDecoder for PngDecoder {
    fn formats(&self) -> &[ImageFormat] {
        &[ImageFormat::Png]
    }
    fn decode(&self, bytes: &[u8]) -> Result<DecodedImage> {
        decode_with_format(bytes, image::ImageFormat::Png)
    }
}

#[cfg(feature = "jpeg")]
pub struct JpegDecoder;
#[cfg(feature = "jpeg")]
impl ImageDecoder for JpegDecoder {
    fn formats(&self) -> &[ImageFormat] {
        &[ImageFormat::Jpeg]
    }
    fn decode(&self, bytes: &[u8]) -> Result<DecodedImage> {
        decode_with_format(bytes, image::ImageFormat::Jpeg)
    }
}

#[cfg(feature = "gif")]
pub struct GifDecoder;
#[cfg(feature = "gif")]
impl ImageDecoder for GifDecoder {
    fn formats(&self) -> &[ImageFormat] {
        &[ImageFormat::Gif]
    }
    fn decode(&self, bytes: &[u8]) -> Result<DecodedImage> {
        decode_with_format(bytes, image::ImageFormat::Gif)
    }
}

#[cfg(feature = "webp")]
pub struct WebPDecoder;
#[cfg(feature = "webp")]
impl ImageDecoder for WebPDecoder {
    fn formats(&self) -> &[ImageFormat] {
        &[ImageFormat::WebP]
    }
    fn decode(&self, bytes: &[u8]) -> Result<DecodedImage> {
        decode_with_format(bytes, image::ImageFormat::WebP)
    }
}

#[cfg(feature = "bmp")]
pub struct BmpDecoder;
#[cfg(feature = "bmp")]
impl ImageDecoder for BmpDecoder {
    fn formats(&self) -> &[ImageFormat] {
        &[ImageFormat::Bmp]
    }
    fn decode(&self, bytes: &[u8]) -> Result<DecodedImage> {
        decode_with_format(bytes, image::ImageFormat::Bmp)
    }
}

#[cfg(feature = "tiff")]
pub struct TiffDecoder;
#[cfg(feature = "tiff")]
impl ImageDecoder for TiffDecoder {
    fn formats(&self) -> &[ImageFormat] {
        &[ImageFormat::Tiff]
    }
    fn decode(&self, bytes: &[u8]) -> Result<DecodedImage> {
        decode_with_format(bytes, image::ImageFormat::Tiff)
    }
}

#[cfg(feature = "avif")]
pub struct AvifDecoder;
#[cfg(feature = "avif")]
impl ImageDecoder for AvifDecoder {
    fn formats(&self) -> &[ImageFormat] {
        &[ImageFormat::Avif]
    }
    fn decode(&self, bytes: &[u8]) -> Result<DecodedImage> {
        decode_with_format(bytes, image::ImageFormat::Avif)
    }
}

//! Runtime BC1 / BC3 encoding for the 2D image path.
//!
//! Compiled only when the `bc-encode` cargo feature is enabled.
//! Produces [`TextureData`] values whose `format` is a BC variant,
//! ready for [`crate::image::GpuImage::from_compressed`].
//!
//! Mirrors the 3D pipeline's approach in `blinc_gltf::bc_encode`
//! without the mesh-specific slots (no BC4 occlusion, no BC5
//! normal). The 2D image widget cache only hands us plain RGBA
//! buffers — diffuse-style color data — so the decision tree
//! collapses to:
//!
//! - **effectively opaque** (every alpha ≥ 244) → BC1 (4 bpp)
//! - **has meaningful alpha** → BC3 (8 bpp)
//!
//! Encode cost: ~50-150 ms per 2K × 2K RGBA at load, same ballpark
//! as the mesh-texture pipeline. The caller decides where that
//! cost lives — the current integration runs inline in
//! `BlincContext::preload_images`, which is off the paint path.

use blinc_core::draw::{TextureData, TexturePixelFormat};

/// Cutoff for "effectively opaque" — any pixel with alpha below
/// this uses BC3. 244 / 255 ≈ 0.957, matching the same heuristic
/// the mesh material path uses for auto-demoting BLEND → MASK:
/// a handful of stray near-opaque pixels from PNG antialiasing
/// shouldn't force the whole texture into the 8-bpp BC3 tier.
const OPAQUE_ALPHA_CUTOFF: u8 = 244;

/// Zero-copy reinterpret a packed `[r, g, b, a, ...]` u8 slice
/// as `&[tbc::color::Rgba8]`.
///
/// # Safety
/// `tbc::color::Rgba8` is `#[repr(C)]` with four `u8` fields, so
/// its layout is identical to `[u8; 4]`. Caller guarantees the
/// input length is a multiple of 4 via the `debug_assert` in each
/// public function.
#[inline]
fn rgba8_view(pixels: &[u8]) -> &[tbc::color::Rgba8] {
    debug_assert_eq!(pixels.len() % 4, 0);
    unsafe {
        std::slice::from_raw_parts(
            pixels.as_ptr() as *const tbc::color::Rgba8,
            pixels.len() / 4,
        )
    }
}

/// Classify an RGBA8 buffer as "effectively opaque" (BC1-eligible)
/// or "has meaningful alpha" (needs BC3). Single sequential pass;
/// short-circuits on the first non-opaque pixel.
pub fn is_effectively_opaque(pixels: &[u8]) -> bool {
    debug_assert_eq!(pixels.len() % 4, 0);
    for chunk in pixels.chunks_exact(4) {
        if chunk[3] < OPAQUE_ALPHA_CUTOFF {
            return false;
        }
    }
    true
}

/// Encode an RGBA8 buffer as BC1 (4 bpp, sRGB or linear variant
/// decided at upload time by the caller's `CompressedColorSpace`).
/// Alpha is discarded — caller must have validated opacity via
/// [`is_effectively_opaque`] first.
pub fn encode_bc1(pixels: &[u8], width: u32, height: u32) -> TextureData {
    debug_assert_eq!(pixels.len(), (width as usize) * (height as usize) * 4);
    let bytes = tbc::encode_image_bc1_conv_u8(rgba8_view(pixels), width as usize, height as usize);
    TextureData::new_compressed(bytes, TexturePixelFormat::Bc1, width, height)
}

/// Encode an RGBA8 buffer as BC3 (8 bpp). Preserves the alpha
/// channel via the BC3 alpha block.
pub fn encode_bc3(pixels: &[u8], width: u32, height: u32) -> TextureData {
    debug_assert_eq!(pixels.len(), (width as usize) * (height as usize) * 4);
    let bytes = tbc::encode_image_bc3_conv_u8(rgba8_view(pixels), width as usize, height as usize);
    TextureData::new_compressed(bytes, TexturePixelFormat::Bc3, width, height)
}

/// Pick BC1 or BC3 based on the alpha profile and run the encode.
/// Convenience for the 2D image cache path which doesn't know the
/// texture's role (color / normal / etc.) and just wants the
/// smallest format that preserves data.
pub fn encode_auto(pixels: &[u8], width: u32, height: u32) -> TextureData {
    if is_effectively_opaque(pixels) {
        encode_bc1(pixels, width, height)
    } else {
        encode_bc3(pixels, width, height)
    }
}

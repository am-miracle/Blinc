//! Blinc Image
//!
//! Image loading and rendering for Blinc UI.
//!
//! # Features
//!
//! - Load images from file paths, URLs, and base64 data
//! - Pluggable per-format decoders (PNG, JPEG, GIF, WebP, BMP, TIFF,
//!   AVIF) gated by cargo features — only the ones an app needs get
//!   compiled in. See [`decoder`] for the [`ImageDecoder`] trait and
//!   [`DecoderRegistry`] used to plug in custom decoders.
//! - CSS-style object-fit options (cover, contain, fill, etc.)
//! - Image filters: grayscale, sepia, brightness, contrast, blur, etc.
//!
//! # Example
//!
//! ```ignore
//! use blinc_image::{ImageSource, ImageData, ObjectFit};
//!
//! // Load from file
//! let data = ImageData::load(ImageSource::File("image.png".into()))?;
//!
//! // Load from base64
//! let data = ImageData::load(ImageSource::Base64("iVBORw0KGgo...".into()))?;
//!
//! // Load from URL (requires "network" feature)
//! let data = ImageData::load_async(ImageSource::Url("https://example.com/image.png".into())).await?;
//! ```
//!
//! [`ImageDecoder`]: decoder::ImageDecoder
//! [`DecoderRegistry`]: decoder::DecoderRegistry

#[cfg(any(
    feature = "png",
    feature = "jpeg",
    feature = "gif",
    feature = "webp",
    feature = "bmp",
    feature = "tiff",
    feature = "avif",
))]
mod builtin;
pub mod decoder;
mod error;
mod loader;
mod source;

pub use decoder::{DecodedImage, DecoderRegistry, ImageDecoder};
pub use error::{ImageError, Result};
pub use loader::ImageData;
pub use source::{ImageFormat, ImageSource};

// ============================================================================
// CSS-style Object Fit (equivalent to CSS object-fit)
// ============================================================================

/// How an image should fit within its container (CSS object-fit equivalent)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObjectFit {
    /// Fill the container completely, cropping if necessary (maintains aspect ratio)
    /// Equivalent to CSS `object-fit: cover`
    #[default]
    Cover,
    /// Fit entirely within the container (maintains aspect ratio, may letterbox)
    /// Equivalent to CSS `object-fit: contain`
    Contain,
    /// Stretch to fill the container (ignores aspect ratio)
    /// Equivalent to CSS `object-fit: fill`
    Fill,
    /// Scale down only if larger than container (maintains aspect ratio)
    /// Equivalent to CSS `object-fit: scale-down`
    ScaleDown,
    /// No scaling, display at original size
    /// Equivalent to CSS `object-fit: none`
    None,
}

/// Alias for ObjectFit for backward compatibility
pub type BoxFit = ObjectFit;

// ============================================================================
// CSS-style Object Position (equivalent to CSS object-position)
// ============================================================================

/// Image alignment within its container (CSS object-position equivalent)
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ObjectPosition {
    /// Horizontal alignment (0.0 = left, 0.5 = center, 1.0 = right)
    pub x: f32,
    /// Vertical alignment (0.0 = top, 0.5 = center, 1.0 = bottom)
    pub y: f32,
}

impl ObjectPosition {
    pub const TOP_LEFT: Self = Self { x: 0.0, y: 0.0 };
    pub const TOP_CENTER: Self = Self { x: 0.5, y: 0.0 };
    pub const TOP_RIGHT: Self = Self { x: 1.0, y: 0.0 };
    pub const CENTER_LEFT: Self = Self { x: 0.0, y: 0.5 };
    pub const CENTER: Self = Self { x: 0.5, y: 0.5 };
    pub const CENTER_RIGHT: Self = Self { x: 1.0, y: 0.5 };
    pub const BOTTOM_LEFT: Self = Self { x: 0.0, y: 1.0 };
    pub const BOTTOM_CENTER: Self = Self { x: 0.5, y: 1.0 };
    pub const BOTTOM_RIGHT: Self = Self { x: 1.0, y: 1.0 };

    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Alias for ObjectPosition for backward compatibility
pub type ImageAlignment = ObjectPosition;

// ============================================================================
// CSS-style Image Filters
// ============================================================================

/// Image filter effects (CSS filter equivalent)
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ImageFilter {
    /// Grayscale amount (0.0 = none, 1.0 = full grayscale)
    pub grayscale: f32,
    /// Sepia amount (0.0 = none, 1.0 = full sepia)
    pub sepia: f32,
    /// Brightness multiplier (1.0 = normal, 0.0 = black, 2.0 = double brightness)
    pub brightness: f32,
    /// Contrast multiplier (1.0 = normal, 0.0 = no contrast)
    pub contrast: f32,
    /// Saturation multiplier (1.0 = normal, 0.0 = grayscale, 2.0 = double saturation)
    pub saturate: f32,
    /// Hue rotation in degrees (0-360)
    pub hue_rotate: f32,
    /// Invert amount (0.0 = none, 1.0 = full invert)
    pub invert: f32,
    /// Blur radius in pixels (0.0 = no blur)
    pub blur: f32,
}

impl ImageFilter {
    /// Create a filter with no effects
    pub fn none() -> Self {
        Self {
            brightness: 1.0,
            contrast: 1.0,
            saturate: 1.0,
            ..Default::default()
        }
    }

    /// Apply grayscale effect
    pub fn grayscale(mut self, amount: f32) -> Self {
        self.grayscale = amount.clamp(0.0, 1.0);
        self
    }

    /// Apply sepia effect
    pub fn sepia(mut self, amount: f32) -> Self {
        self.sepia = amount.clamp(0.0, 1.0);
        self
    }

    /// Adjust brightness
    pub fn brightness(mut self, amount: f32) -> Self {
        self.brightness = amount.max(0.0);
        self
    }

    /// Adjust contrast
    pub fn contrast(mut self, amount: f32) -> Self {
        self.contrast = amount.max(0.0);
        self
    }

    /// Adjust saturation
    pub fn saturate(mut self, amount: f32) -> Self {
        self.saturate = amount.max(0.0);
        self
    }

    /// Rotate hue
    pub fn hue_rotate(mut self, degrees: f32) -> Self {
        self.hue_rotate = degrees % 360.0;
        self
    }

    /// Invert colors
    pub fn invert(mut self, amount: f32) -> Self {
        self.invert = amount.clamp(0.0, 1.0);
        self
    }

    /// Apply blur
    pub fn blur(mut self, radius: f32) -> Self {
        self.blur = radius.max(0.0);
        self
    }
}

// ============================================================================
// Image Rendering Style
// ============================================================================

/// Complete image styling options
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ImageStyle {
    /// How the image fits within its container
    pub object_fit: ObjectFit,
    /// Position of the image within its container
    pub object_position: ObjectPosition,
    /// Opacity (0.0 = transparent, 1.0 = opaque)
    pub opacity: f32,
    /// Border radius for rounded corners
    pub border_radius: f32,
    /// Image filter effects
    pub filter: ImageFilter,
    /// Tint color (multiplied with image colors) [r, g, b, a]
    pub tint: [f32; 4],
}

impl ImageStyle {
    /// Create a new image style with defaults
    pub fn new() -> Self {
        Self {
            object_fit: ObjectFit::default(),
            object_position: ObjectPosition::CENTER,
            opacity: 1.0,
            border_radius: 0.0,
            filter: ImageFilter::none(),
            tint: [1.0, 1.0, 1.0, 1.0],
        }
    }

    /// Set object-fit mode
    pub fn fit(mut self, fit: ObjectFit) -> Self {
        self.object_fit = fit;
        self
    }

    /// Set object-position
    pub fn position(mut self, position: ObjectPosition) -> Self {
        self.object_position = position;
        self
    }

    /// Set opacity
    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }

    /// Set border radius for rounded corners
    pub fn rounded(mut self, radius: f32) -> Self {
        self.border_radius = radius;
        self
    }

    /// Set image filter
    pub fn filter(mut self, filter: ImageFilter) -> Self {
        self.filter = filter;
        self
    }

    /// Set tint color
    pub fn tint(mut self, r: f32, g: f32, b: f32, a: f32) -> Self {
        self.tint = [r, g, b, a];
        self
    }
}

// ============================================================================
// Fit Calculation
// ============================================================================

/// Calculate the source and destination rectangles for rendering an image
/// with a given object-fit mode.
///
/// Returns (src_rect, dst_rect) where:
/// - src_rect: The portion of the source image to sample (x, y, width, height)
/// - dst_rect: Where to place the image in the container (x, y, width, height)
pub fn calculate_fit_rects(
    image_width: u32,
    image_height: u32,
    container_width: f32,
    container_height: f32,
    fit: ObjectFit,
    position: ObjectPosition,
) -> ([f32; 4], [f32; 4]) {
    let img_w = image_width as f32;
    let img_h = image_height as f32;

    match fit {
        ObjectFit::Fill => {
            // Stretch to fill - use entire source, fill entire container
            (
                [0.0, 0.0, img_w, img_h],
                [0.0, 0.0, container_width, container_height],
            )
        }

        ObjectFit::Contain => {
            // Fit within container, maintaining aspect ratio
            let scale = (container_width / img_w).min(container_height / img_h);
            let dst_w = img_w * scale;
            let dst_h = img_h * scale;

            // Align within container
            let dst_x = (container_width - dst_w) * position.x;
            let dst_y = (container_height - dst_h) * position.y;

            ([0.0, 0.0, img_w, img_h], [dst_x, dst_y, dst_w, dst_h])
        }

        ObjectFit::Cover => {
            // Fill container, cropping if necessary
            let scale = (container_width / img_w).max(container_height / img_h);
            let src_w = container_width / scale;
            let src_h = container_height / scale;

            // Align crop region within source image
            let src_x = (img_w - src_w) * position.x;
            let src_y = (img_h - src_h) * position.y;

            (
                [src_x, src_y, src_w, src_h],
                [0.0, 0.0, container_width, container_height],
            )
        }

        ObjectFit::ScaleDown => {
            // Like contain, but only scale down, never up
            let scale = (container_width / img_w)
                .min(container_height / img_h)
                .min(1.0);
            let dst_w = img_w * scale;
            let dst_h = img_h * scale;

            let dst_x = (container_width - dst_w) * position.x;
            let dst_y = (container_height - dst_h) * position.y;

            ([0.0, 0.0, img_w, img_h], [dst_x, dst_y, dst_w, dst_h])
        }

        ObjectFit::None => {
            // No scaling, display at original size
            let dst_x = (container_width - img_w) * position.x;
            let dst_y = (container_height - img_h) * position.y;

            ([0.0, 0.0, img_w, img_h], [dst_x, dst_y, img_w, img_h])
        }
    }
}

/// Convert source rectangle to UV coordinates (0-1 range)
pub fn src_rect_to_uv(src_rect: [f32; 4], image_width: u32, image_height: u32) -> [f32; 4] {
    let img_w = image_width as f32;
    let img_h = image_height as f32;
    [
        src_rect[0] / img_w,                 // u_min
        src_rect[1] / img_h,                 // v_min
        (src_rect[0] + src_rect[2]) / img_w, // u_max
        (src_rect[1] + src_rect[3]) / img_h, // v_max
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_object_fit_contain() {
        // 100x50 image in 200x200 container
        let (src, dst) = calculate_fit_rects(
            100,
            50,
            200.0,
            200.0,
            ObjectFit::Contain,
            ObjectPosition::CENTER,
        );

        assert_eq!(src, [0.0, 0.0, 100.0, 50.0]);
        // Scale is 2.0 (limited by height ratio 200/50=4, width ratio 200/100=2)
        // dst width = 100 * 2 = 200, dst height = 50 * 2 = 100
        // centered: x = 0, y = (200 - 100) * 0.5 = 50
        assert_eq!(dst, [0.0, 50.0, 200.0, 100.0]);
    }

    #[test]
    fn test_object_fit_cover() {
        // 100x50 image in 200x200 container
        let (src, dst) = calculate_fit_rects(
            100,
            50,
            200.0,
            200.0,
            ObjectFit::Cover,
            ObjectPosition::CENTER,
        );

        // Scale is 4.0 (200/50 to fill height)
        // src_w = 200/4 = 50, src_h = 200/4 = 50
        // src_x = (100-50) * 0.5 = 25
        assert_eq!(src, [25.0, 0.0, 50.0, 50.0]);
        assert_eq!(dst, [0.0, 0.0, 200.0, 200.0]);
    }

    #[test]
    fn test_object_fit_fill() {
        let (src, dst) = calculate_fit_rects(
            100,
            50,
            200.0,
            200.0,
            ObjectFit::Fill,
            ObjectPosition::CENTER,
        );

        assert_eq!(src, [0.0, 0.0, 100.0, 50.0]);
        assert_eq!(dst, [0.0, 0.0, 200.0, 200.0]);
    }

    #[test]
    fn test_src_rect_to_uv() {
        let src_rect = [25.0, 0.0, 50.0, 50.0];
        let uv = src_rect_to_uv(src_rect, 100, 50);

        assert_eq!(uv[0], 0.25); // u_min
        assert_eq!(uv[1], 0.0); // v_min
        assert_eq!(uv[2], 0.75); // u_max
        assert_eq!(uv[3], 1.0); // v_max
    }

    #[test]
    fn test_image_filter_chain() {
        let filter = ImageFilter::none().grayscale(0.5).brightness(1.2).blur(5.0);

        assert_eq!(filter.grayscale, 0.5);
        assert_eq!(filter.brightness, 1.2);
        assert_eq!(filter.blur, 5.0);
    }
}

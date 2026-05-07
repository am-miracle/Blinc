//! Image source types

use std::path::PathBuf;

/// Source of an image
#[derive(Debug, Clone)]
pub enum ImageSource {
    /// Load from a file path
    File(PathBuf),

    /// Load from a URL (requires "network" feature)
    Url(String),

    /// Load from base64-encoded data
    /// Can optionally include data URI prefix (e.g., "data:image/png;base64,...")
    Base64(String),

    /// Load from raw bytes with format hint
    Bytes {
        data: Vec<u8>,
        format: Option<ImageFormat>,
    },

    /// Load an emoji character as an image
    /// Format: "emoji://😀" or "emoji://😀?size=64"
    Emoji {
        /// The emoji character or string
        emoji: String,
        /// Rendering size in pixels (default 64)
        size: f32,
    },

    /// Pre-decoded RGBA image data (already in memory)
    Rgba {
        /// RGBA pixel data (4 bytes per pixel)
        data: Vec<u8>,
        /// Width in pixels
        width: u32,
        /// Height in pixels
        height: u32,
    },
}

impl ImageSource {
    /// Create a file source
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File(path.into())
    }

    /// Create a URL source
    pub fn url(url: impl Into<String>) -> Self {
        Self::Url(url.into())
    }

    /// Create a base64 source
    pub fn base64(data: impl Into<String>) -> Self {
        Self::Base64(data.into())
    }

    /// Create a bytes source
    pub fn bytes(data: Vec<u8>) -> Self {
        Self::Bytes { data, format: None }
    }

    /// Create a bytes source with format hint
    pub fn bytes_with_format(data: Vec<u8>, format: ImageFormat) -> Self {
        Self::Bytes {
            data,
            format: Some(format),
        }
    }

    /// Create an emoji source
    ///
    /// # Arguments
    /// * `emoji` - The emoji character or string (e.g., "😀", "🇺🇸")
    /// * `size` - Rendering size in pixels
    pub fn emoji(emoji: impl Into<String>, size: f32) -> Self {
        Self::Emoji {
            emoji: emoji.into(),
            size,
        }
    }

    /// Create an RGBA source from pre-decoded pixel data
    ///
    /// # Arguments
    /// * `data` - RGBA pixel data (4 bytes per pixel, in order)
    /// * `width` - Width in pixels
    /// * `height` - Height in pixels
    pub fn rgba(data: Vec<u8>, width: u32, height: u32) -> Self {
        Self::Rgba {
            data,
            width,
            height,
        }
    }

    /// Parse a resource URI string into an ImageSource
    ///
    /// Supported formats:
    /// - `file:///path/to/image.png` - File path
    /// - `http://...` or `https://...` - URL
    /// - `data:image/png;base64,...` - Data URI with base64
    /// - `emoji://😀` or `emoji://😀?size=64` - Emoji as image
    /// - `/path/to/image.png` - Treated as file path
    pub fn from_uri(uri: &str) -> Self {
        if uri.starts_with("data:") {
            // Data URI
            Self::Base64(uri.to_string())
        } else if uri.starts_with("http://") || uri.starts_with("https://") {
            // URL
            Self::Url(uri.to_string())
        } else if uri.starts_with("emoji://") {
            // Emoji URI: emoji://😀 or emoji://😀?size=64
            let rest = uri.strip_prefix("emoji://").unwrap_or("");
            let (emoji, size) = if let Some(idx) = rest.find('?') {
                let emoji_part = &rest[..idx];
                let query = &rest[idx + 1..];
                // Parse size from query string (e.g., "size=64")
                let size = query
                    .split('&')
                    .find_map(|param| {
                        let mut parts = param.splitn(2, '=');
                        if parts.next() == Some("size") {
                            parts.next().and_then(|v| v.parse::<f32>().ok())
                        } else {
                            None
                        }
                    })
                    .unwrap_or(64.0);
                (emoji_part.to_string(), size)
            } else {
                (rest.to_string(), 64.0)
            };
            Self::Emoji { emoji, size }
        } else if uri.starts_with("file://") {
            // File URI
            let path = uri.strip_prefix("file://").unwrap_or(uri);
            Self::File(PathBuf::from(path))
        } else {
            // Assume file path
            Self::File(PathBuf::from(uri))
        }
    }
}

impl From<&str> for ImageSource {
    fn from(s: &str) -> Self {
        Self::from_uri(s)
    }
}

impl From<String> for ImageSource {
    fn from(s: String) -> Self {
        Self::from_uri(&s)
    }
}

impl From<PathBuf> for ImageSource {
    fn from(path: PathBuf) -> Self {
        Self::File(path)
    }
}

impl From<&std::path::Path> for ImageSource {
    fn from(path: &std::path::Path) -> Self {
        Self::File(path.to_path_buf())
    }
}

/// Image format hint
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    WebP,
    Bmp,
    Tiff,
    Avif,
}

impl ImageFormat {
    /// Detect format from file extension
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "gif" => Some(Self::Gif),
            "webp" => Some(Self::WebP),
            "bmp" => Some(Self::Bmp),
            "tif" | "tiff" => Some(Self::Tiff),
            "avif" => Some(Self::Avif),
            _ => None,
        }
    }

    /// Detect format from MIME type
    pub fn from_mime(mime: &str) -> Option<Self> {
        match mime {
            "image/png" => Some(Self::Png),
            "image/jpeg" | "image/jpg" => Some(Self::Jpeg),
            "image/gif" => Some(Self::Gif),
            "image/webp" => Some(Self::WebP),
            "image/bmp" => Some(Self::Bmp),
            "image/tiff" => Some(Self::Tiff),
            "image/avif" => Some(Self::Avif),
            _ => None,
        }
    }
}

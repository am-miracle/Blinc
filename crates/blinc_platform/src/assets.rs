//! Cross-platform asset loading
//!
//! This module provides platform-agnostic asset loading for resources
//! like images, fonts, and data files that may be stored differently
//! on each platform:
//!
//! - **Desktop**: Regular filesystem paths
//! - **Android**: APK assets via AssetManager
//! - **iOS**: App bundle resources (planned)
//! - **Web**: HTTP fetch from server (planned)
//!
//! # Example
//!
//! ```ignore
//! use blinc_platform::assets::{AssetLoader, AssetPath};
//!
//! // Create platform-specific loader
//! let loader = create_asset_loader();
//!
//! // Load an asset - path is interpreted per-platform
//! let data = loader.load("images/logo.png")?;
//! ```

use crate::error::{PlatformError, Result};
use std::path::Path;

/// Asset path that can be resolved differently per platform
///
/// On desktop, this is typically relative to the executable or a resource directory.
/// On Android, this refers to files in the APK's assets folder.
/// On iOS, this refers to files in the app bundle.
#[derive(Debug, Clone)]
pub enum AssetPath {
    /// Path relative to asset root (platform-interpreted)
    /// - Desktop: relative to executable or resource dir
    /// - Android: relative to assets/ in APK
    /// - iOS: relative to app bundle
    Relative(String),

    /// Absolute filesystem path (desktop only)
    /// Falls back to relative on mobile platforms
    Absolute(String),

    /// Embedded asset by name (for compile-time embedded resources)
    Embedded(&'static str),
}

impl AssetPath {
    /// Create a relative asset path
    pub fn relative(path: impl Into<String>) -> Self {
        Self::Relative(path.into())
    }

    /// Create an absolute filesystem path
    pub fn absolute(path: impl Into<String>) -> Self {
        Self::Absolute(path.into())
    }

    /// Create an embedded asset reference
    pub const fn embedded(name: &'static str) -> Self {
        Self::Embedded(name)
    }
}

impl<S: Into<String>> From<S> for AssetPath {
    fn from(s: S) -> Self {
        let s = s.into();
        // Detect if this looks like an absolute path
        if s.starts_with('/') || (cfg!(windows) && s.chars().nth(1) == Some(':')) {
            Self::Absolute(s)
        } else {
            Self::Relative(s)
        }
    }
}

/// Platform-agnostic asset loader trait
///
/// Each platform implements this trait to provide asset loading
/// that works with that platform's storage mechanisms.
pub trait AssetLoader: Send + Sync {
    /// Load an asset as raw bytes
    ///
    /// # Arguments
    /// * `path` - Asset path (relative to assets root or absolute)
    ///
    /// # Returns
    /// * `Ok(Vec<u8>)` - Asset data
    /// * `Err(PlatformError)` - If asset cannot be loaded
    fn load(&self, path: &AssetPath) -> Result<Vec<u8>>;

    /// Check if an asset exists
    fn exists(&self, path: &AssetPath) -> bool;

    /// Load an asset as a UTF-8 string
    fn load_string(&self, path: &AssetPath) -> Result<String> {
        let bytes = self.load(path)?;
        String::from_utf8(bytes)
            .map_err(|e| PlatformError::AssetLoad(format!("Invalid UTF-8: {}", e)))
    }

    /// Get the platform name for this loader
    fn platform_name(&self) -> &'static str;

    /// Return a URL the platform's media stack can consume directly,
    /// bypassing the preload cache and any full-body byte fetch.
    ///
    /// For asset types that decode from the full byte buffer (PNG,
    /// JPEG, glTF, fonts), callers should keep using [`Self::load`]
    /// — there's no partial-bytes decode path. This exists for media
    /// types where the platform's own streaming pipeline is better
    /// than anything we could build on top of raw bytes:
    ///
    /// - `<video>` elements on web do HTTP range requests,
    ///   progressive buffering, seek-ahead, and media-source decoding
    ///   without us preloading the whole file first.
    /// - `<audio>` elements do the same.
    /// - Native file URLs (`file:///absolute/path`) let the host OS
    ///   mmap or stream the file on demand.
    ///
    /// Returns `None` when the loader can't produce a consumable URL
    /// for the path (e.g. embedded/bundled assets on mobile with no
    /// intermediate file, or an absent remote path the wasm loader
    /// doesn't know how to resolve). Callers should then fall back
    /// to the byte-based [`Self::load`] + `URL.createObjectURL(blob)`
    /// path.
    fn asset_url(&self, _path: &AssetPath) -> Option<String> {
        None
    }

    /// Whether the loader has finished any background fetches it was
    /// asked to perform. Callers use this to distinguish "asset
    /// genuinely missing" from "asset still in flight":
    ///
    /// - Desktop / embedded / filesystem loaders return `true`
    ///   unconditionally — they never fetch asynchronously, so
    ///   [`Self::load`] is already authoritative.
    /// - The web loader returns `false` until
    ///   `PreloadProgress::is_complete()` flips, then `true`.
    ///
    /// Intended for callers with a retry loop over [`Self::load`]:
    /// if `load` errors while `preload_settled()` is `false`, keep
    /// retrying; if it errors *after* `preload_settled()` is `true`,
    /// the asset isn't coming and it's safe to fall through to a
    /// placeholder (e.g. `blinc_gltf` substituting a 1×1 white
    /// texture for a 404'd diffuse map).
    fn preload_settled(&self) -> bool {
        true
    }
}

/// Default filesystem-based asset loader for desktop platforms
///
/// This loader reads assets directly from the filesystem.
/// It supports both relative paths (resolved from the current directory
/// or a configured base path) and absolute paths.
#[derive(Debug, Clone)]
pub struct FilesystemAssetLoader {
    /// Base directory for relative paths
    base_path: Option<std::path::PathBuf>,
}

impl FilesystemAssetLoader {
    /// Create a new filesystem loader with no base path
    /// (relative paths resolved from current directory)
    pub fn new() -> Self {
        Self { base_path: None }
    }

    /// Create a filesystem loader with a base path for relative assets
    pub fn with_base_path(base: impl AsRef<Path>) -> Self {
        Self {
            base_path: Some(base.as_ref().to_path_buf()),
        }
    }

    /// Set the base path for relative assets
    pub fn set_base_path(&mut self, base: impl AsRef<Path>) {
        self.base_path = Some(base.as_ref().to_path_buf());
    }

    fn resolve_path(&self, path: &AssetPath) -> std::path::PathBuf {
        match path {
            AssetPath::Relative(rel) => {
                if let Some(ref base) = self.base_path {
                    base.join(rel)
                } else {
                    std::path::PathBuf::from(rel)
                }
            }
            AssetPath::Absolute(abs) => std::path::PathBuf::from(abs),
            AssetPath::Embedded(name) => {
                // Embedded assets not supported in filesystem loader
                // Try as relative path
                if let Some(ref base) = self.base_path {
                    base.join(name)
                } else {
                    std::path::PathBuf::from(*name)
                }
            }
        }
    }
}

impl Default for FilesystemAssetLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetLoader for FilesystemAssetLoader {
    fn load(&self, path: &AssetPath) -> Result<Vec<u8>> {
        let resolved = self.resolve_path(path);
        std::fs::read(&resolved).map_err(|e| {
            PlatformError::AssetLoad(format!("Failed to load '{}': {}", resolved.display(), e))
        })
    }

    fn exists(&self, path: &AssetPath) -> bool {
        let resolved = self.resolve_path(path);
        resolved.exists()
    }

    fn platform_name(&self) -> &'static str {
        "filesystem"
    }

    /// Emit a `file://<absolute-path>` URL that native media
    /// pipelines (AVFoundation, FFmpeg, winit's platform media APIs
    /// via downstream crates) can open directly. Uses
    /// `canonicalize()` so relative paths fed through
    /// `resolve_path` still come out as absolute `file://` URLs.
    /// Returns `None` when the file doesn't exist on disk.
    fn asset_url(&self, path: &AssetPath) -> Option<String> {
        let resolved = self.resolve_path(path);
        let canonical = resolved.canonicalize().ok()?;
        let s = canonical.to_string_lossy();
        // Windows: `\\?\C:\path` → `file:///C:/path`
        // macOS/Linux: `/path` → `file:///path`
        #[cfg(target_os = "windows")]
        let fixed = s.strip_prefix(r"\\?\").unwrap_or(&s).replace('\\', "/");
        #[cfg(not(target_os = "windows"))]
        let fixed = s.to_string();
        Some(if fixed.starts_with('/') {
            format!("file://{fixed}")
        } else {
            format!("file:///{fixed}")
        })
    }
}

/// Global asset loader instance
///
/// This is set by the platform during initialization and provides
/// a way for libraries like blinc_image to load assets without
/// needing direct platform knowledge.
static GLOBAL_LOADER: std::sync::OnceLock<Box<dyn AssetLoader>> = std::sync::OnceLock::new();

/// Set the global asset loader
///
/// This should be called once during platform initialization.
/// Returns an error if a loader was already set.
pub fn set_global_asset_loader(loader: Box<dyn AssetLoader>) -> Result<()> {
    GLOBAL_LOADER.set(loader).map_err(|_| {
        PlatformError::InitFailed("Global asset loader already initialized".to_string())
    })
}

/// Get a reference to the global asset loader
///
/// Returns None if no loader has been set yet.
pub fn global_asset_loader() -> Option<&'static dyn AssetLoader> {
    GLOBAL_LOADER.get().map(|b| b.as_ref())
}

/// Load an asset using the global loader
///
/// This is the simplest way to load assets in a cross-platform manner.
///
/// # Example
///
/// ```ignore
/// let image_data = blinc_platform::assets::load_asset("images/logo.png")?;
/// ```
pub fn load_asset(path: impl Into<AssetPath>) -> Result<Vec<u8>> {
    let loader = global_asset_loader()
        .ok_or_else(|| PlatformError::AssetLoad("No asset loader configured".to_string()))?;
    loader.load(&path.into())
}

/// Check if an asset exists using the global loader
pub fn asset_exists(path: impl Into<AssetPath>) -> bool {
    global_asset_loader()
        .map(|l| l.exists(&path.into()))
        .unwrap_or(false)
}

/// Load an asset as a string using the global loader
pub fn load_asset_string(path: impl Into<AssetPath>) -> Result<String> {
    let loader = global_asset_loader()
        .ok_or_else(|| PlatformError::AssetLoad("No asset loader configured".to_string()))?;
    loader.load_string(&path.into())
}

/// Get a URL the platform's media stack can consume directly,
/// bypassing any byte-level preload. Thin wrapper over the global
/// loader's [`AssetLoader::asset_url`] — see that method for when
/// it returns `None` (embedded assets, absent remote paths).
///
/// Intended for streaming media: hand the returned URL to
/// [`blinc_media::VideoPlayer::load_url`] or an `<audio>` element.
pub fn asset_url(path: impl Into<AssetPath>) -> Option<String> {
    global_asset_loader().and_then(|l| l.asset_url(&path.into()))
}

/// Whether the global loader has finished all pending background
/// fetches. Wrapper over [`AssetLoader::preload_settled`]; returns
/// `true` when no loader is configured (nothing to wait on).
///
/// See the trait method for how callers use this to gate
/// placeholder-fallback logic during a retry loop.
pub fn preload_settled() -> bool {
    global_asset_loader()
        .map(|l| l.preload_settled())
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_filesystem_loader() {
        // Create a temp file
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("blinc_test_asset.txt");
        let mut f = std::fs::File::create(&test_file).unwrap();
        f.write_all(b"Hello, Blinc!").unwrap();

        // Test loading with absolute path
        let loader = FilesystemAssetLoader::new();
        let path = AssetPath::Absolute(test_file.to_string_lossy().to_string());
        let data = loader.load(&path).unwrap();
        assert_eq!(data, b"Hello, Blinc!");

        // Test exists
        assert!(loader.exists(&path));

        // Cleanup
        std::fs::remove_file(test_file).unwrap();
    }

    #[test]
    fn test_asset_path_from_string() {
        let relative: AssetPath = "images/logo.png".into();
        assert!(matches!(relative, AssetPath::Relative(_)));

        let absolute: AssetPath = "/absolute/path.png".into();
        assert!(matches!(absolute, AssetPath::Absolute(_)));
    }
}

//! Platform-native themes
//!
//! Each platform has its own native look and feel:
//! - macOS: Aqua/Big Sur design language
//! - Windows: Fluent Design System
//! - Linux: Adwaita (GNOME)
//! - iOS: iOS Human Interface Guidelines
//! - Android: Material You
//! - Web: default Catppuccin-derived [`crate::themes::BlincTheme`]

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "ios")]
pub mod ios;

#[cfg(target_os = "android")]
pub mod android;

#[cfg(target_arch = "wasm32")]
pub mod web;

use crate::theme::ThemeBundle;

/// Get the appropriate theme bundle for the current platform
pub fn platform_theme_bundle() -> ThemeBundle {
    #[cfg(target_os = "macos")]
    {
        return macos::MacOSTheme::bundle();
    }

    #[cfg(target_os = "windows")]
    {
        return windows::WindowsTheme::bundle();
    }

    #[cfg(target_os = "linux")]
    {
        return linux::LinuxTheme::bundle();
    }

    #[cfg(target_os = "ios")]
    {
        return ios::IOSTheme::bundle();
    }

    #[cfg(target_os = "android")]
    {
        return android::AndroidTheme::bundle();
    }

    #[cfg(target_arch = "wasm32")]
    {
        return web::WebTheme::bundle();
    }

    // Fallback for other platforms
    #[allow(unreachable_code)]
    {
        crate::themes::BlincTheme::bundle()
    }
}

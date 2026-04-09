//! Platform detection for color scheme and system preferences

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "ios")]
mod ios;

#[cfg(target_os = "android")]
mod android;

use crate::theme::ColorScheme;

/// Supported platforms
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    MacOS,
    Windows,
    Linux,
    IOS,
    Android,
    Web,
    Unknown,
}

impl Platform {
    /// Get the current platform
    pub fn current() -> Self {
        #[cfg(target_os = "macos")]
        return Platform::MacOS;

        #[cfg(target_os = "windows")]
        return Platform::Windows;

        #[cfg(target_os = "linux")]
        return Platform::Linux;

        #[cfg(target_os = "ios")]
        return Platform::IOS;

        #[cfg(target_os = "android")]
        return Platform::Android;

        #[cfg(target_arch = "wasm32")]
        return Platform::Web;

        #[cfg(not(any(
            target_os = "macos",
            target_os = "windows",
            target_os = "linux",
            target_os = "ios",
            target_os = "android",
            target_arch = "wasm32"
        )))]
        Platform::Unknown
    }
}

/// Detect the system color scheme
pub fn detect_system_color_scheme() -> ColorScheme {
    #[cfg(target_os = "macos")]
    {
        macos::detect_color_scheme()
    }

    #[cfg(target_os = "windows")]
    {
        windows::detect_color_scheme()
    }

    #[cfg(target_os = "linux")]
    {
        linux::detect_color_scheme()
    }

    #[cfg(target_os = "ios")]
    {
        ios::detect_color_scheme()
    }

    #[cfg(target_os = "android")]
    {
        android::detect_color_scheme()
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "windows",
        target_os = "linux",
        target_os = "ios",
        target_os = "android"
    )))]
    {
        ColorScheme::Light
    }
}

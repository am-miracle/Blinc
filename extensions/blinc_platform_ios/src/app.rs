//! iOS application integration
//!
//! Provides the iOS platform implementation and entry point.

use crate::event_loop::IOSEventLoop;
use crate::window::IOSWindow;
use blinc_platform::{Platform, PlatformError};

#[cfg(target_os = "ios")]
use objc2_foundation::MainThreadMarker;
#[cfg(target_os = "ios")]
use objc2_ui_kit::{UIApplication, UIScreen, UIWindowScene};

#[cfg(target_os = "ios")]
use tracing::info;

/// iOS platform implementation
pub struct IOSPlatform {
    /// Display scale factor
    #[cfg(target_os = "ios")]
    scale_factor: f64,
    #[cfg(not(target_os = "ios"))]
    _private: (),
}

#[cfg(target_os = "ios")]
impl IOSPlatform {
    /// Get the main screen's scale factor
    fn get_screen_scale() -> f64 {
        // On iOS, UIScreen::mainScreen() requires MainThreadMarker.
        // We assume this is called from the main thread.
        //
        // `mainScreen` is technically deprecated in modern iOS (Apple
        // recommends `view.window.windowScene.screen` instead) but at
        // platform-init time we don't have a view / window / scene to
        // hand — those don't exist until the UIApplicationDelegate
        // boots up. The deprecated form continues to work for
        // single-screen apps, which is the only configuration Blinc's
        // iOS runner currently supports.
        #[allow(deprecated)]
        let mtm = MainThreadMarker::new().expect("Must be called from main thread");
        #[allow(deprecated)]
        let screen = UIScreen::mainScreen(mtm);
        screen.scale()
    }
}

impl Platform for IOSPlatform {
    type Window = IOSWindow;
    type EventLoop = IOSEventLoop;

    #[cfg(target_os = "ios")]
    fn new() -> Result<Self, PlatformError> {
        let scale_factor = Self::get_screen_scale();
        info!(
            "iOS platform initialized with scale factor: {}",
            scale_factor
        );
        Ok(Self { scale_factor })
    }

    #[cfg(not(target_os = "ios"))]
    fn new() -> Result<Self, PlatformError> {
        Err(PlatformError::Unsupported(
            "iOS platform only available on iOS".to_string(),
        ))
    }

    fn create_event_loop(&self) -> Result<Self::EventLoop, PlatformError> {
        Ok(IOSEventLoop::new())
    }

    fn name(&self) -> &'static str {
        "ios"
    }

    #[cfg(target_os = "ios")]
    fn scale_factor(&self) -> f64 {
        self.scale_factor
    }

    #[cfg(not(target_os = "ios"))]
    fn scale_factor(&self) -> f64 {
        1.0
    }
}

/// iOS main entry point
///
/// This function is called by the iOS app delegate when the application launches.
/// It initializes the Blinc runtime and sets up the Metal rendering context.
///
/// Note: On iOS, the actual event loop is managed by UIKit's RunLoop.
/// This function sets up the necessary infrastructure for Blinc to integrate
/// with the iOS lifecycle.
#[cfg(target_os = "ios")]
pub fn ios_main() {
    info!("iOS main entry point called");

    // Note: The actual application lifecycle is managed by UIApplicationDelegate
    // and UIKit. This function serves as the Rust-side initialization point.
    //
    // A typical iOS app using Blinc would:
    // 1. Create a UIWindow with a UIViewController
    // 2. Set up a CAMetalLayer on the view
    // 3. Create a CADisplayLink for frame callbacks
    // 4. Route touch events to Blinc's input system
    //
    // See IOSApp::run() in blinc_app for the full integration.
}

/// Placeholder for non-iOS builds
#[cfg(not(target_os = "ios"))]
pub fn ios_main() {
    // Placeholder - iOS main is only called on iOS
}

/// Get the display scale factor for the main screen
#[cfg(target_os = "ios")]
pub fn get_display_scale() -> f64 {
    IOSPlatform::get_screen_scale()
}

/// Placeholder for non-iOS builds
#[cfg(not(target_os = "ios"))]
pub fn get_display_scale() -> f64 {
    1.0
}

/// Check if the system is in dark mode
#[cfg(target_os = "ios")]
pub fn is_dark_mode() -> bool {
    // TODO: Implement proper dark mode detection using UITraitCollection
    // For now, default to light mode
    false
}

/// Placeholder for non-iOS builds
#[cfg(not(target_os = "ios"))]
pub fn is_dark_mode() -> bool {
    false
}

/// Get the safe area insets for the key window.
///
/// Returns `(top, right, bottom, left)` in logical points, matching the
/// [`Window::safe_area_insets`](blinc_platform::Window::safe_area_insets)
/// contract. Reads `UIWindow.safeAreaInsets` from the first key window of
/// the first foreground-active `UIWindowScene` — the same lookup
/// `BlincNativeBridge.swift` uses for its `device.has_notch` handler.
///
/// Must be called from the main thread. Returns zeros if no scene has a
/// key window yet (e.g. before `application(_:didFinishLaunchingWithOptions:)`
/// completes).
#[cfg(target_os = "ios")]
pub fn get_safe_area_insets() -> (f32, f32, f32, f32) {
    let Some(mtm) = MainThreadMarker::new() else {
        return (0.0, 0.0, 0.0, 0.0);
    };

    let app = UIApplication::sharedApplication(mtm);
    let scenes = app.connectedScenes();

    // Pick the key window from the first scene that has one; otherwise
    // fall back to the first window of the first scene — mirrors the
    // Swift template's `?? first` pattern for pre-first-responder
    // launches.
    let mut insets: Option<objc2_ui_kit::UIEdgeInsets> = None;
    'scenes: for scene in scenes.iter() {
        let Ok(window_scene) = scene.downcast::<UIWindowScene>() else {
            continue;
        };
        let windows = window_scene.windows();
        let mut first_insets: Option<objc2_ui_kit::UIEdgeInsets> = None;
        for window in windows.iter() {
            if first_insets.is_none() {
                first_insets = Some(window.safeAreaInsets());
            }
            if window.isKeyWindow() {
                insets = Some(window.safeAreaInsets());
                break 'scenes;
            }
        }
        if let Some(fallback) = first_insets {
            insets = Some(fallback);
            break;
        }
    }

    let Some(insets) = insets else {
        return (0.0, 0.0, 0.0, 0.0);
    };

    // UIEdgeInsets is (top, left, bottom, right); the blinc Window trait
    // exposes (top, right, bottom, left). Reorder here so every call site
    // sees the same tuple shape.
    (
        insets.top as f32,
        insets.right as f32,
        insets.bottom as f32,
        insets.left as f32,
    )
}

/// Placeholder for non-iOS builds
#[cfg(not(target_os = "ios"))]
pub fn get_safe_area_insets() -> (f32, f32, f32, f32) {
    (0.0, 0.0, 0.0, 0.0)
}

/// iOS system font paths
///
/// These are the common font locations on iOS. Note that different iOS versions
/// and simulator vs device may have fonts at different paths. The fonts in the
/// Core directory are the most reliable across different iOS versions.
pub fn system_font_paths() -> &'static [&'static str] {
    &[
        // iOS system fonts - Core directory (most reliable)
        "/System/Library/Fonts/Core/SFUI.ttf", // SF UI (system font)
        "/System/Library/Fonts/Core/SFUIMono.ttf", // SF Mono
        "/System/Library/Fonts/Core/SFUIItalic.ttf", // SF Italic
        "/System/Library/Fonts/Core/Helvetica.ttc", // Helvetica
        "/System/Library/Fonts/Core/HelveticaNeue.ttc", // Helvetica Neue
        "/System/Library/Fonts/Core/Avenir.ttc", // Avenir
        "/System/Library/Fonts/Core/AvenirNext.ttc", // Avenir Next
        "/System/Library/Fonts/Core/Courier.ttc", // Courier
        "/System/Library/Fonts/Core/CourierNew.ttf", // Courier New
        // CoreUI fonts
        "/System/Library/Fonts/CoreUI/Menlo.ttc", // Menlo (monospace)
        "/System/Library/Fonts/CoreUI/SFUIRounded.ttf", // SF Rounded
        // CoreAddition fonts
        "/System/Library/Fonts/CoreAddition/Georgia.ttf",
        "/System/Library/Fonts/CoreAddition/Arial.ttf",
        "/System/Library/Fonts/CoreAddition/ArialBold.ttf",
        "/System/Library/Fonts/CoreAddition/Verdana.ttf",
        "/System/Library/Fonts/CoreAddition/TimesNewRomanPS.ttf",
    ]
}

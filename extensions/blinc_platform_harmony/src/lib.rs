//! Blinc HarmonyOS Platform
//!
//! XComponent integration and Vulkan/GLES rendering for HarmonyOS.
//!
//! This crate implements the `blinc_platform` traits for HarmonyOS,
//! providing touch input, lifecycle management, and window handling
//! via XComponent and N-API.
//!
//! # Architecture
//!
//! HarmonyOS uses ArkUI for native development. Blinc integrates through:
//!
//! - **XComponent** for native rendering surface
//! - **N-API** for ArkTS/JavaScript interop
//! - **Vulkan** or **OpenGL ES** for GPU rendering
//! - **XComponent touch callbacks** for input
//!
//! # Usage
//!
//! ```ignore
//! use blinc_app::harmony::HarmonyApp;
//!
//! // In your XComponent's OnSurfaceCreated callback
//! HarmonyApp::run_with_xcomponent(xcomponent, |ctx| {
//!     div()
//!         .w(ctx.width).h(ctx.height)
//!         .bg([0.1, 0.1, 0.15, 1.0])
//!         .flex_center()
//!         .child(text("Hello HarmonyOS!").size(48.0))
//! }).unwrap();
//! ```
//!
//! # Building for HarmonyOS
//!
//! Requires DevEco Studio and OHOS SDK:
//!
//! ```bash
//! # Build with hvigorw
//! hvigorw assembleHap
//! ```

pub mod app;
pub mod assets;
pub mod event_loop;
pub mod input;
pub mod napi_bridge;
pub mod window;

// Re-export public types
pub use app::HarmonyPlatform;
pub use assets::HarmonyAssetLoader;
pub use event_loop::{HarmonyEventLoop, HarmonyWakeProxy};
pub use input::{Touch, TouchPhase, convert_touch};
pub use window::HarmonyWindow;

use blinc_platform::PlatformError;

// Convenience constructor for non-HarmonyOS builds
impl HarmonyPlatform {
    /// Create a placeholder platform (for cross-compilation checks)
    #[cfg(not(target_os = "ohos"))]
    pub fn with_placeholder() -> Result<Self, PlatformError> {
        Err(PlatformError::Unsupported(
            "HarmonyOS platform only available on HarmonyOS".to_string(),
        ))
    }
}

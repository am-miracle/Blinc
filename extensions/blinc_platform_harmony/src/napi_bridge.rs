//! N-API bridge for HarmonyOS
//!
//! Registers native module for ArkTS interop.
//!
//! # Architecture
//!
//! HarmonyOS uses N-API (similar to Node.js) for native module integration.
//! This module provides:
//!
//! 1. Native module registration
//! 2. XComponent lifecycle callbacks
//! 3. Render loop integration
//!
//! # ArkTS Usage
//!
//! ```typescript
//! import blinc from 'libblinc_platform_harmony.so'
//!
//! @Component
//! struct BlincView {
//!   private context: number = 0
//!
//!   build() {
//!     XComponent({
//!       id: 'blinc_view',
//!       type: 'surface',
//!       libraryname: 'blinc_platform_harmony'
//!     })
//!     .onLoad((context) => {
//!       this.context = blinc.init(context)
//!     })
//!     .onDestroy(() => {
//!       blinc.destroy(this.context)
//!     })
//!   }
//! }
//! ```

use std::ffi::c_void;

/// XComponent callback functions
///
/// These are registered via OH_NativeXComponent_RegisterCallback
#[derive(Default)]
pub struct XComponentCallbacks {
    /// Called when surface is created
    pub on_surface_created: Option<extern "C" fn(component: *mut c_void, window: *mut c_void)>,
    /// Called when surface is changed (resized)
    pub on_surface_changed: Option<extern "C" fn(component: *mut c_void, window: *mut c_void)>,
    /// Called when surface is destroyed
    pub on_surface_destroyed: Option<extern "C" fn(component: *mut c_void, window: *mut c_void)>,
    /// Called to dispatch touch events
    pub dispatch_touch_event: Option<extern "C" fn(component: *mut c_void, window: *mut c_void)>,
}

/// N-API module initialization
///
/// This is called by the HarmonyOS runtime when loading the native module.
/// Registers the module with N-API and sets up XComponent callbacks.
///
/// # Safety
///
/// This function is called by the HarmonyOS N-API runtime.
#[unsafe(no_mangle)]
pub extern "C" fn napi_register_module() -> *mut c_void {
    tracing::info!("blinc_platform_harmony: N-API module registration");

    // TODO: Implement actual N-API registration
    // This would use napi-rs macros in the actual implementation:
    //
    // #[napi]
    // fn init(xcomponent_id: String) -> Result<i64, napi::Error> {
    //     // Create render context
    // }
    //
    // #[napi]
    // fn render_frame(handle: i64) -> Result<(), napi::Error> {
    //     // Render frame
    // }
    //
    // #[napi]
    // fn destroy(handle: i64) -> Result<(), napi::Error> {
    //     // Cleanup
    // }

    std::ptr::null_mut()
}

/// XComponent entry point
///
/// Called by HarmonyOS when the XComponent is loaded.
/// This is the native entry point specified in the XComponent's libraryname.
#[unsafe(no_mangle)]
pub extern "C" fn OH_NativeXComponent_Export() -> *mut c_void {
    tracing::info!("blinc_platform_harmony: XComponent export");

    // TODO: Return XComponent native interface
    // This would typically return a struct with callback function pointers

    std::ptr::null_mut()
}

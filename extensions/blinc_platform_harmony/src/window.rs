//! HarmonyOS window implementation
//!
//! Wraps XComponent's NativeWindow for rendering.

use blinc_platform::{Cursor, Window};

/// HarmonyOS window backed by XComponent's NativeWindow
pub struct HarmonyWindow {
    /// Window width in logical pixels
    width: u32,
    /// Window height in logical pixels
    height: u32,
    /// Display scale factor
    scale_factor: f64,
    /// Native window pointer (OHNativeWindow*)
    #[allow(dead_code)]
    native_window: *mut std::ffi::c_void,
}

impl HarmonyWindow {
    /// Create a new HarmonyOS window
    pub fn new(scale_factor: f64) -> Self {
        Self {
            width: 1080,
            height: 1920,
            scale_factor,
            native_window: std::ptr::null_mut(),
        }
    }

    /// Create with a native window from XComponent
    pub fn with_native_window(
        native_window: *mut std::ffi::c_void,
        width: u32,
        height: u32,
        scale_factor: f64,
    ) -> Self {
        Self {
            width,
            height,
            scale_factor,
            native_window,
        }
    }

    /// Update window size from XComponent callback
    pub fn update_size(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
    }

    /// Get the native window pointer for GPU surface creation
    pub fn native_window_ptr(&self) -> *mut std::ffi::c_void {
        self.native_window
    }
}

// SAFETY: HarmonyWindow is only accessed from the main thread
unsafe impl Send for HarmonyWindow {}
unsafe impl Sync for HarmonyWindow {}

impl Window for HarmonyWindow {
    fn id(&self) -> blinc_platform::WindowId {
        blinc_platform::WindowId::PRIMARY
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn logical_size(&self) -> (f32, f32) {
        (
            self.width as f32 / self.scale_factor as f32,
            self.height as f32 / self.scale_factor as f32,
        )
    }

    fn scale_factor(&self) -> f64 {
        self.scale_factor
    }

    fn set_title(&self, _title: &str) {
        // HarmonyOS apps use system-level titles, not window titles
    }

    fn set_cursor(&self, _cursor: Cursor) {
        // HarmonyOS is touch-only; no cursor support
    }

    fn request_redraw(&self) {
        // TODO: Signal XComponent to request next frame
        // This would use OH_NativeXComponent_RequestRefresh or similar
    }

    fn is_focused(&self) -> bool {
        // TODO: Track focus state from XComponent lifecycle
        true
    }

    fn is_visible(&self) -> bool {
        // TODO: Track visibility from lifecycle events
        true
    }
}

//! [`blinc_platform::Window`] implementation for an HtmlCanvasElement.
//!
//! On wasm32 this wraps a real `web_sys::HtmlCanvasElement`. On other
//! hosts (so the crate still compiles inside `cargo check` from a
//! desktop dev box) it's a placeholder type that always returns the
//! "unsupported" error path on the methods that would normally call
//! into JS.
//!
//! ## Why a stub on native?
//!
//! The other extension crates (Android, iOS, Harmony) all ship a
//! placeholder constructor (`with_placeholder`) so the workspace can be
//! checked from any host. We do the same — `cargo check -p
//! blinc_platform_web` on macOS should work without dragging
//! `wasm-bindgen` into the desktop dep tree.

use blinc_platform::{Cursor, Window, WindowId};

/// Wrapper around a `web_sys::HtmlCanvasElement` that satisfies the
/// [`blinc_platform::Window`] contract.
///
/// The window's *logical* size is read from `canvas.client_width()` /
/// `client_height()` (CSS pixels), and its *physical* size is read from
/// `canvas.width()` / `canvas.height()` (the GPU framebuffer size). The
/// device pixel ratio (`window.devicePixelRatio`) is captured at
/// construction time and refreshed on each resize.
#[derive(Debug)]
pub struct WebWindow {
    #[cfg(target_arch = "wasm32")]
    canvas: web_sys::HtmlCanvasElement,
    /// Logical width in CSS pixels.
    logical_width: f32,
    /// Logical height in CSS pixels.
    logical_height: f32,
    /// Device pixel ratio (`window.devicePixelRatio`).
    scale_factor: f64,
}

// SAFETY: WebWindow is only ever accessed from the browser main thread.
// `web_sys::HtmlCanvasElement` is `!Send` because the JS interface is
// single-threaded; we manually opt in to `Send + Sync` so the type can
// satisfy the `blinc_platform::Window: Send` bound. The harmony
// extension uses the same pattern for its `*mut c_void` resource
// manager handle.
unsafe impl Send for WebWindow {}
unsafe impl Sync for WebWindow {}

impl WebWindow {
    /// Create a `WebWindow` from a canvas element. Reads the canvas's
    /// current CSS size and the window's `devicePixelRatio` once;
    /// callers should call [`refresh_dimensions`] after a resize.
    #[cfg(target_arch = "wasm32")]
    pub fn from_canvas(canvas: web_sys::HtmlCanvasElement) -> Self {
        let logical_width = canvas.client_width() as f32;
        let logical_height = canvas.client_height() as f32;
        let scale_factor = web_sys::window()
            .map(|w| w.device_pixel_ratio())
            .unwrap_or(1.0);
        Self {
            canvas,
            logical_width,
            logical_height,
            scale_factor,
        }
    }

    /// Cross-host placeholder constructor — only useful for `cargo
    /// check` on a non-wasm host. The returned window has zero
    /// dimensions and won't respond to any meaningful operation.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn placeholder() -> Self {
        Self {
            logical_width: 0.0,
            logical_height: 0.0,
            scale_factor: 1.0,
        }
    }

    /// Re-read the canvas size and DPR. Call after the user resizes
    /// the browser window or the canvas's CSS dimensions change.
    #[cfg(target_arch = "wasm32")]
    pub fn refresh_dimensions(&mut self) {
        self.logical_width = self.canvas.client_width() as f32;
        self.logical_height = self.canvas.client_height() as f32;
        if let Some(w) = web_sys::window() {
            self.scale_factor = w.device_pixel_ratio();
        }
    }

    /// Resize the canvas's framebuffer (the *physical* size). Call
    /// this after the canvas's CSS size changes so the GPU surface
    /// stays correctly sized.
    #[cfg(target_arch = "wasm32")]
    pub fn resize_to_logical(&mut self, logical_width: f32, logical_height: f32) {
        self.logical_width = logical_width;
        self.logical_height = logical_height;
        let physical_w = (logical_width * self.scale_factor as f32).round() as u32;
        let physical_h = (logical_height * self.scale_factor as f32).round() as u32;
        self.canvas.set_width(physical_w);
        self.canvas.set_height(physical_h);
    }

    /// Borrow the underlying canvas element. Wasm32 only.
    #[cfg(target_arch = "wasm32")]
    pub fn canvas(&self) -> &web_sys::HtmlCanvasElement {
        &self.canvas
    }
}

impl Window for WebWindow {
    fn id(&self) -> WindowId {
        // The web target is always single-canvas; PRIMARY is the right
        // (and only) value.
        WindowId::PRIMARY
    }

    fn size(&self) -> (u32, u32) {
        let physical_w = (self.logical_width * self.scale_factor as f32).round() as u32;
        let physical_h = (self.logical_height * self.scale_factor as f32).round() as u32;
        (physical_w, physical_h)
    }

    fn logical_size(&self) -> (f32, f32) {
        (self.logical_width, self.logical_height)
    }

    fn scale_factor(&self) -> f64 {
        self.scale_factor
    }

    fn set_title(&self, title: &str) {
        // Document title rather than canvas title — that's what users
        // actually see in the browser tab.
        #[cfg(target_arch = "wasm32")]
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            doc.set_title(title);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = title;
    }

    fn set_cursor(&self, cursor: Cursor) {
        #[cfg(target_arch = "wasm32")]
        {
            let css = match cursor {
                Cursor::Default => "default",
                Cursor::Pointer => "pointer",
                Cursor::Text => "text",
                Cursor::Crosshair => "crosshair",
                Cursor::Move => "move",
                Cursor::NotAllowed => "not-allowed",
                Cursor::ResizeNS => "ns-resize",
                Cursor::ResizeEW => "ew-resize",
                Cursor::ResizeNESW => "nesw-resize",
                Cursor::ResizeNWSE => "nwse-resize",
                Cursor::Grab => "grab",
                Cursor::Grabbing => "grabbing",
                Cursor::Wait => "wait",
                Cursor::Progress => "progress",
                Cursor::None => "none",
            };
            let _ = self.canvas.style().set_property("cursor", css);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = cursor;
    }

    fn request_redraw(&self) {
        // No-op: the web runner drives redraws through
        // `requestAnimationFrame`, not via per-window redraw requests.
        // The next `rAF` callback will do the work.
    }

    fn is_focused(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            // The browser doesn't expose per-canvas focus directly;
            // approximate using `document.hasFocus()`.
            web_sys::window()
                .and_then(|w| w.document())
                .map(|d| d.has_focus().unwrap_or(true))
                .unwrap_or(true)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            true
        }
    }

    fn is_visible(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            // `document.visibilityState` is the canonical visibility
            // signal in the browser.
            web_sys::window()
                .and_then(|w| w.document())
                .map(|d| d.visibility_state() == web_sys::VisibilityState::Visible)
                .unwrap_or(true)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            false
        }
    }

    // The other Window trait methods (set_position, drag_window,
    // minimize, maximize, close) all default to no-op on the web —
    // a `<canvas>` doesn't own a window.

    fn safe_area_insets(&self) -> (f32, f32, f32, f32) {
        // CSS env(safe-area-inset-*) is the modern way to read these
        // for web apps installed as PWAs. v1 returns zero — PWA-aware
        // safe areas are a follow-up once iOS Safari WebGPU stabilises.
        (0.0, 0.0, 0.0, 0.0)
    }
}

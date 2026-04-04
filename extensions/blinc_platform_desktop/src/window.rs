//! Desktop window implementation using winit

use blinc_platform::{Cursor, Window, WindowConfig};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use winit::dpi::LogicalSize;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window as WinitWindow, WindowAttributes};

/// Desktop window wrapping a winit window
pub struct DesktopWindow {
    window: Arc<WinitWindow>,
    focused: AtomicBool,
}

impl DesktopWindow {
    /// Create a new desktop window
    pub fn new(
        event_loop: &ActiveEventLoop,
        config: &WindowConfig,
    ) -> Result<Self, winit::error::OsError> {
        let mut attrs = WindowAttributes::default()
            .with_title(&config.title)
            .with_inner_size(LogicalSize::new(config.width, config.height))
            .with_resizable(config.resizable)
            .with_decorations(config.decorations)
            .with_transparent(config.transparent);

        if config.fullscreen {
            attrs = attrs.with_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
        }
        if let Some((min_w, min_h)) = config.min_size {
            attrs = attrs.with_min_inner_size(LogicalSize::new(min_w, min_h));
        }
        if let Some((max_w, max_h)) = config.max_size {
            attrs = attrs.with_max_inner_size(LogicalSize::new(max_w, max_h));
        }
        if let Some((x, y)) = config.position {
            attrs = attrs.with_position(winit::dpi::LogicalPosition::new(x, y));
        }

        let window = event_loop.create_window(attrs)?;

        // Center window on screen if requested
        if config.center {
            if let Some(monitor) = window.current_monitor() {
                let screen = monitor.size();
                let win_size = window.outer_size();
                let x = (screen.width.saturating_sub(win_size.width)) / 2;
                let y = (screen.height.saturating_sub(win_size.height)) / 2;
                window.set_outer_position(winit::dpi::PhysicalPosition::new(x as i32, y as i32));
            }
        }

        // Enable IME (Input Method Editor) for international text input
        window.set_ime_allowed(true);

        Ok(Self {
            window: Arc::new(window),
            focused: AtomicBool::new(true),
        })
    }

    /// Get the underlying winit window
    pub fn winit_window(&self) -> &WinitWindow {
        &self.window
    }

    /// Get an Arc to the winit window
    pub fn winit_window_arc(&self) -> Arc<WinitWindow> {
        Arc::clone(&self.window)
    }

    /// Set focus state (called by event loop)
    pub(crate) fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::Relaxed);
    }
}

impl Window for DesktopWindow {
    fn id(&self) -> blinc_platform::WindowId {
        // Use winit's WindowId hash as our platform-agnostic ID
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.window.id().hash(&mut hasher);
        blinc_platform::WindowId(hasher.finish())
    }

    fn size(&self) -> (u32, u32) {
        let size = self.window.inner_size();
        (size.width, size.height)
    }

    fn logical_size(&self) -> (f32, f32) {
        let size = self.window.inner_size();
        let scale = self.window.scale_factor();
        (
            (size.width as f64 / scale) as f32,
            (size.height as f64 / scale) as f32,
        )
    }

    fn scale_factor(&self) -> f64 {
        self.window.scale_factor()
    }

    fn set_title(&self, title: &str) {
        self.window.set_title(title);
    }

    fn set_cursor(&self, cursor: Cursor) {
        use winit::window::CursorIcon;
        let icon = match cursor {
            Cursor::Default => CursorIcon::Default,
            Cursor::Pointer => CursorIcon::Pointer,
            Cursor::Text => CursorIcon::Text,
            Cursor::Crosshair => CursorIcon::Crosshair,
            Cursor::Move => CursorIcon::Move,
            Cursor::NotAllowed => CursorIcon::NotAllowed,
            Cursor::ResizeNS => CursorIcon::NsResize,
            Cursor::ResizeEW => CursorIcon::EwResize,
            Cursor::ResizeNESW => CursorIcon::NeswResize,
            Cursor::ResizeNWSE => CursorIcon::NwseResize,
            Cursor::Grab => CursorIcon::Grab,
            Cursor::Grabbing => CursorIcon::Grabbing,
            Cursor::Wait => CursorIcon::Wait,
            Cursor::Progress => CursorIcon::Progress,
            Cursor::None => {
                self.window.set_cursor_visible(false);
                return;
            }
        };
        self.window.set_cursor_visible(true);
        self.window.set_cursor(icon);
    }

    fn request_redraw(&self) {
        self.window.request_redraw();
    }

    fn is_focused(&self) -> bool {
        self.focused.load(Ordering::Relaxed)
    }

    fn is_visible(&self) -> bool {
        self.window.is_visible().unwrap_or(true)
    }

    fn set_position(&self, x: i32, y: i32) {
        self.window
            .set_outer_position(winit::dpi::LogicalPosition::new(x, y));
    }

    fn center_on_screen(&self) {
        if let Some(monitor) = self.window.current_monitor() {
            let screen = monitor.size();
            let win_size = self.window.outer_size();
            let x = (screen.width.saturating_sub(win_size.width)) / 2;
            let y = (screen.height.saturating_sub(win_size.height)) / 2;
            self.window
                .set_outer_position(winit::dpi::PhysicalPosition::new(x as i32, y as i32));
        }
    }

    fn set_size(&self, width: u32, height: u32) {
        let _ = self
            .window
            .request_inner_size(LogicalSize::new(width, height));
    }

    fn drag_window(&self) {
        let _ = self.window.drag_window();
    }

    fn minimize(&self) {
        self.window.set_minimized(true);
    }

    fn maximize(&self) {
        self.window.set_maximized(!self.window.is_maximized());
    }

    fn close(&self) {
        // winit doesn't have a close method — we request the event loop to close
        // by not requesting redraws. The actual close is handled by the event loop
        // when it receives CloseRequested.
        self.window.set_visible(false);
    }
}

// Safety: Window operations are thread-safe via winit's internal synchronization
unsafe impl Send for DesktopWindow {}
unsafe impl Sync for DesktopWindow {}

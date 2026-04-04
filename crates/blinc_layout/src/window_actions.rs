//! Window action callbacks for layout elements.
//!
//! These are registered by the platform runner and callable from element
//! event handlers (e.g., `.drag_region()` on Div for custom title bars).

use std::sync::OnceLock;

type ActionCallback = Box<dyn Fn() + Send + Sync>;

static DRAG_WINDOW: OnceLock<ActionCallback> = OnceLock::new();
static MINIMIZE_WINDOW: OnceLock<ActionCallback> = OnceLock::new();
static MAXIMIZE_WINDOW: OnceLock<ActionCallback> = OnceLock::new();
static CLOSE_WINDOW: OnceLock<ActionCallback> = OnceLock::new();

/// Register the drag_window callback (called by the platform runner at init)
pub fn set_drag_window_callback(f: impl Fn() + Send + Sync + 'static) {
    let _ = DRAG_WINDOW.set(Box::new(f));
}

/// Register the minimize callback
pub fn set_minimize_callback(f: impl Fn() + Send + Sync + 'static) {
    let _ = MINIMIZE_WINDOW.set(Box::new(f));
}

/// Register the maximize callback
pub fn set_maximize_callback(f: impl Fn() + Send + Sync + 'static) {
    let _ = MAXIMIZE_WINDOW.set(Box::new(f));
}

/// Register the close callback
pub fn set_close_callback(f: impl Fn() + Send + Sync + 'static) {
    let _ = CLOSE_WINDOW.set(Box::new(f));
}

/// Start a window drag operation (for custom title bars)
pub fn drag_window() {
    if let Some(f) = DRAG_WINDOW.get() {
        f();
    }
}

/// Minimize the window
pub fn minimize_window() {
    if let Some(f) = MINIMIZE_WINDOW.get() {
        f();
    }
}

/// Maximize/restore the window
pub fn maximize_window() {
    if let Some(f) = MAXIMIZE_WINDOW.get() {
        f();
    }
}

/// Close the window
pub fn close_window() {
    if let Some(f) = CLOSE_WINDOW.get() {
        f();
    }
}

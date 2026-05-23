//! HarmonyOS event loop implementation
//!
//! Event-driven via XComponent callbacks.

use blinc_platform::{ControlFlow, Event, EventLoop, PlatformError};

use crate::window::HarmonyWindow;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Wake proxy for HarmonyOS event loop
///
/// Use this to request a redraw from a background animation thread.
#[derive(Clone)]
pub struct HarmonyWakeProxy {
    wake_requested: Arc<AtomicBool>,
}

impl HarmonyWakeProxy {
    /// Create a new wake proxy
    pub fn new() -> Self {
        Self {
            wake_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Wake up the event loop
    pub fn wake(&self) {
        self.wake_requested.store(true, Ordering::SeqCst);
        // TODO: Post message to main thread via HarmonyOS APIs
    }

    /// Check if a wake was requested and clear the flag
    pub fn take_wake_request(&self) -> bool {
        self.wake_requested.swap(false, Ordering::SeqCst)
    }
}

impl Default for HarmonyWakeProxy {
    fn default() -> Self {
        Self::new()
    }
}

/// HarmonyOS event loop
///
/// Unlike desktop platforms, HarmonyOS doesn't have a blocking event loop.
/// Events come through XComponent callbacks:
/// - OnSurfaceCreated / OnSurfaceDestroyed
/// - OnSurfaceChanged (resize)
/// - DispatchTouchEvent
pub struct HarmonyEventLoop {
    wake_proxy: HarmonyWakeProxy,
}

impl HarmonyEventLoop {
    /// Create a new HarmonyOS event loop
    pub fn new() -> Self {
        Self {
            wake_proxy: HarmonyWakeProxy::new(),
        }
    }

    /// Get a wake proxy for animation threads
    pub fn wake_proxy(&self) -> HarmonyWakeProxy {
        self.wake_proxy.clone()
    }
}

impl Default for HarmonyEventLoop {
    fn default() -> Self {
        Self::new()
    }
}

impl EventLoop for HarmonyEventLoop {
    type Window = HarmonyWindow;

    fn run<F>(self, mut _handler: F) -> Result<(), PlatformError>
    where
        F: FnMut(Event, &Self::Window) -> ControlFlow + 'static,
    {
        // HarmonyOS event loop is managed by the framework
        // Events are delivered through XComponent callbacks
        // This method is a placeholder for trait compatibility

        tracing::info!("HarmonyOS event loop - events delivered via XComponent callbacks");

        Err(PlatformError::Unsupported(
            "HarmonyOS event loop is callback-based. Use XComponent callbacks instead.".to_string(),
        ))
    }
}

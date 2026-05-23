//! Global keyboard shortcuts
//!
//! Register system-wide hotkeys that trigger even when the app is not focused.
//!
//! # Example
//!
//! ```ignore
//! use blinc_app::hotkey::{GlobalHotkey, HotKeyModifiers};
//!
//! let hotkey = GlobalHotkey::new("Ctrl+Shift+P", || {
//!     println!("Global shortcut triggered!");
//! });
//! // hotkey stays active until dropped
//! ```

use std::sync::Arc;

/// A registered global hotkey. Active until dropped.
pub struct GlobalHotkey {
    #[cfg(feature = "global-hotkey")]
    _manager: global_hotkey::GlobalHotKeyManager,
    #[cfg(feature = "global-hotkey")]
    _hotkey: global_hotkey::hotkey::HotKey,
    #[cfg(not(feature = "global-hotkey"))]
    _phantom: (),
}

impl GlobalHotkey {
    /// Register a global hotkey from an accelerator string.
    ///
    /// Accelerator format: `"Ctrl+Shift+P"`, `"Cmd+Q"`, `"Alt+F4"`, etc.
    /// The callback fires on a background thread.
    pub fn new(accelerator: &str, callback: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        #[cfg(feature = "global-hotkey")]
        {
            use global_hotkey::GlobalHotKeyManager;
            use global_hotkey::hotkey::HotKey;

            let hotkey: HotKey = match accelerator.parse() {
                Ok(hk) => hk,
                Err(e) => {
                    tracing::error!("Invalid hotkey '{}': {:?}", accelerator, e);
                    return None;
                }
            };

            let manager = match GlobalHotKeyManager::new() {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!("Failed to create hotkey manager: {:?}", e);
                    return None;
                }
            };

            if let Err(e) = manager.register(hotkey) {
                tracing::error!("Failed to register hotkey '{}': {:?}", accelerator, e);
                return None;
            }

            let hotkey_id = hotkey.id();
            let callback = Arc::new(callback);

            std::thread::spawn(move || {
                let receiver = global_hotkey::GlobalHotKeyEvent::receiver();
                while let Ok(event) = receiver.recv() {
                    if event.id == hotkey_id {
                        callback();
                    }
                }
            });

            Some(Self {
                _manager: manager,
                _hotkey: hotkey,
            })
        }

        #[cfg(not(feature = "global-hotkey"))]
        {
            let _ = (accelerator, callback);
            tracing::warn!("Global hotkeys not available (global-hotkey feature not enabled)");
            None
        }
    }
}

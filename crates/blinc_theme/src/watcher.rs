//! System color scheme watcher
//!
//! Periodically checks the system's color scheme and automatically updates
//! the theme when it changes. This is useful for apps that want to follow
//! the system's light/dark mode preference automatically.
//!
//! # Usage
//!
//! ```rust,ignore
//! use blinc_theme::watcher::SystemSchemeWatcher;
//!
//! // Start watching with default interval (1 second)
//! let watcher = SystemSchemeWatcher::start();
//!
//! // Or with a custom interval
//! let watcher = SystemSchemeWatcher::start_with_interval(Duration::from_secs(5));
//!
//! // The watcher runs in a background thread and automatically updates
//! // ThemeState when the system color scheme changes.
//!
//! // Stop watching when done (or let it drop)
//! watcher.stop();
//! ```

use crate::platform::detect_system_color_scheme;
use crate::state::ThemeState;
use crate::theme::ColorScheme;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Default polling interval for system scheme detection
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Watches for system color scheme changes and updates the theme automatically.
///
/// The watcher runs in a background thread and polls the system's color scheme
/// at a configurable interval. When a change is detected, it updates `ThemeState`
/// which triggers an animated transition to the new color scheme.
///
/// # Thread Safety
///
/// The watcher is designed to be safe for use in multi-threaded applications.
/// It uses atomic operations for the stop signal and all theme updates go through
/// the thread-safe `ThemeState` API.
pub struct SystemSchemeWatcher {
    /// Signal to stop the watcher thread
    stop_signal: Arc<AtomicBool>,
    /// Handle to the background thread
    thread_handle: Option<JoinHandle<()>>,
}

impl SystemSchemeWatcher {
    /// Start watching for system color scheme changes with the default interval (1 second).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let watcher = SystemSchemeWatcher::start();
    /// // Watcher runs in background...
    /// // Drop or call stop() when done
    /// ```
    pub fn start() -> Self {
        Self::start_with_interval(DEFAULT_POLL_INTERVAL)
    }

    /// Start watching for system color scheme changes with a custom interval.
    ///
    /// # Arguments
    ///
    /// * `interval` - How often to check the system color scheme
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Check every 5 seconds
    /// let watcher = SystemSchemeWatcher::start_with_interval(Duration::from_secs(5));
    /// ```
    pub fn start_with_interval(interval: Duration) -> Self {
        let stop_signal = Arc::new(AtomicBool::new(false));
        let stop_signal_clone = Arc::clone(&stop_signal);

        let thread_handle = thread::Builder::new()
            .name("blinc-scheme-watcher".to_string())
            .spawn(move || {
                Self::watch_loop(stop_signal_clone, interval);
            })
            .expect("Failed to spawn scheme watcher thread");

        Self {
            stop_signal,
            thread_handle: Some(thread_handle),
        }
    }

    /// Stop watching for system color scheme changes.
    ///
    /// This method signals the background thread to stop and waits for it to finish.
    /// It's safe to call multiple times.
    pub fn stop(&mut self) {
        self.stop_signal.store(true, Ordering::SeqCst);

        if let Some(handle) = self.thread_handle.take() {
            // Wait for the thread to finish
            let _ = handle.join();
        }
    }

    /// Check if the watcher is still running.
    pub fn is_running(&self) -> bool {
        !self.stop_signal.load(Ordering::SeqCst)
            && self
                .thread_handle
                .as_ref()
                .map(|h| !h.is_finished())
                .unwrap_or(false)
    }

    /// The main watch loop that runs in the background thread.
    fn watch_loop(stop_signal: Arc<AtomicBool>, interval: Duration) {
        let mut last_scheme: Option<ColorScheme> = None;

        tracing::debug!("System scheme watcher started (interval: {:?})", interval);

        while !stop_signal.load(Ordering::SeqCst) {
            // Detect current system scheme
            let current_scheme = detect_system_color_scheme();

            // Check if it changed
            if last_scheme != Some(current_scheme) {
                if let Some(previous) = last_scheme {
                    tracing::info!(
                        "System color scheme changed: {:?} -> {:?}",
                        previous,
                        current_scheme
                    );

                    // Update theme state (this triggers animated transition)
                    if let Some(theme) = ThemeState::try_get() {
                        theme.set_scheme(current_scheme);
                    }
                } else {
                    tracing::debug!("Initial system color scheme detected: {:?}", current_scheme);
                }

                last_scheme = Some(current_scheme);
            }

            // Sleep until next check
            thread::sleep(interval);
        }

        tracing::debug!("System scheme watcher stopped");
    }
}

impl Drop for SystemSchemeWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Configuration for the system scheme watcher.
#[derive(Clone, Debug)]
pub struct WatcherConfig {
    /// How often to poll for system scheme changes
    pub poll_interval: Duration,
    /// Whether to start the watcher automatically
    pub auto_start: bool,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            poll_interval: DEFAULT_POLL_INTERVAL,
            auto_start: true,
        }
    }
}

impl WatcherConfig {
    /// Create a new watcher configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the polling interval.
    pub fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Set whether to auto-start the watcher.
    pub fn auto_start(mut self, enabled: bool) -> Self {
        self.auto_start = enabled;
        self
    }

    /// Build and optionally start the watcher.
    pub fn build(self) -> Option<SystemSchemeWatcher> {
        if self.auto_start {
            Some(SystemSchemeWatcher::start_with_interval(self.poll_interval))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_watcher_config_default() {
        let config = WatcherConfig::default();
        assert_eq!(config.poll_interval, DEFAULT_POLL_INTERVAL);
        assert!(config.auto_start);
    }

    #[test]
    fn test_watcher_config_builder() {
        let config = WatcherConfig::new()
            .poll_interval(Duration::from_secs(5))
            .auto_start(false);

        assert_eq!(config.poll_interval, Duration::from_secs(5));
        assert!(!config.auto_start);
    }
}

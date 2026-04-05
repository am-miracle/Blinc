//! Native OS notifications
//!
//! Send desktop notifications that appear in the system notification center.
//!
//! # Example
//!
//! ```ignore
//! use blinc_app::notify::Notification;
//!
//! Notification::new("Download Complete")
//!     .body("Your file has been saved.")
//!     .show();
//! ```

/// A desktop notification builder
pub struct Notification {
    summary: String,
    body: Option<String>,
    #[allow(dead_code)]
    icon: Option<String>,
}

impl Notification {
    /// Create a new notification with a title/summary
    pub fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            body: None,
            icon: None,
        }
    }

    /// Set the notification body text
    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Set the notification icon name (freedesktop icon name on Linux)
    pub fn icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Show the notification. Returns true if sent successfully.
    pub fn show(self) -> bool {
        #[cfg(feature = "notify-rust")]
        {
            let mut n = notify_rust::Notification::new();
            n.summary(&self.summary);
            if let Some(ref body) = self.body {
                n.body(body);
            }
            n.show().is_ok()
        }

        #[cfg(not(feature = "notify-rust"))]
        {
            tracing::warn!("Notifications not available (notify-rust feature not enabled)");
            false
        }
    }
}

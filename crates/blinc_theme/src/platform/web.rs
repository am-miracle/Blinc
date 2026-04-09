//! Web (browser) color scheme detection
//!
//! Reads the user's `prefers-color-scheme` media query via
//! [`web_sys::Window::match_media`]. Falls back to
//! [`ColorScheme::Light`] if anything in the chain is missing
//! (no `window`, the browser doesn't expose `matchMedia`, the
//! query string is malformed, etc.) so the framework still
//! initialises cleanly in non-DOM contexts like web workers.

use crate::theme::ColorScheme;

/// Detect the current browser color scheme via
/// `window.matchMedia('(prefers-color-scheme: dark)')`. Returns
/// [`ColorScheme::Light`] for any failure path.
pub fn detect_color_scheme() -> ColorScheme {
    let Some(window) = web_sys::window() else {
        return ColorScheme::Light;
    };
    match window.match_media("(prefers-color-scheme: dark)") {
        Ok(Some(mql)) if mql.matches() => ColorScheme::Dark,
        _ => ColorScheme::Light,
    }
}

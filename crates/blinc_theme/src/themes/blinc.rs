//! `BlincTheme` — the framework's default theme.
//!
//! Now an alias for [`super::universal::HybridTheme`] — the Universal
//! HID Hybrid synthesis. The earlier Catppuccin-derived palette has
//! been retired; Hybrid replaces it as Blinc's canonical look so
//! every consumer (call sites, platform-theme wrappers, examples,
//! tests) gets the new visual identity without a rename.
//!
//! Use this alias for the framework default; reach for
//! [`super::universal::RestrainedTheme`] /
//! [`super::universal::ExpressiveTheme`] when you want the Apple- or
//! Material-leaning variant explicitly.

pub use super::universal::HybridTheme as BlincTheme;

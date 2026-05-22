//! Universal HID theme set — three directional variants that
//! synthesise Apple HIG and Material 3 principles into a single
//! cross-platform design. Hybrid is the recommended default
//! fallback when no platform-matched bundle exists, replacing the
//! Catppuccin-based [`BlincTheme`](super::BlincTheme) (which
//! remains in the codebase as an opt-in named theme).
//!
//! The three variants sit on the restraint↔expressiveness axis:
//!
//! - [`RestrainedTheme`] — Apple-leaning. Quiet motion, tight
//!   curves, single-layer ambient shadows. For productivity tools
//!   where the chrome should fade into the background.
//! - [`HybridTheme`] — true 50/50, the recommended default.
//!   Mixed-radii (tight on inputs, bold on surfaces), dual-layer
//!   shadows, adaptive motion (quick for state, springy for nav).
//! - [`ExpressiveTheme`] — Material-leaning. Bolder radii, longer
//!   spring-y motion, tonal accent surfaces. For consumer apps
//!   that want personality.
//!
//! All three opt into Blinc's existing squircle pipeline (see
//! [`ShapeTokens`](crate::ShapeTokens)) for corners ≥ a per-variant
//! threshold; smaller corners and `radius_full` (avatars, switch
//! thumbs) stay as true circles. Existing themes that don't
//! override [`Theme::shape`](crate::Theme::shape) keep their
//! circular-arc behaviour via the trait's default impl.

pub mod expressive;
pub mod hybrid;
pub mod restrained;

pub use expressive::ExpressiveTheme;
pub use hybrid::HybridTheme;
pub use restrained::RestrainedTheme;

/// The recommended default theme for any context where no platform
/// theme is available and no explicit theme has been selected.
///
/// Aliased to [`HybridTheme`] — its mixed-radii ladder and adaptive
/// motion read as native enough on every platform that defaulting
/// to it doesn't pick a stylistic fight with the host OS.
pub type DefaultTheme = HybridTheme;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Theme;

    #[test]
    fn restrained_more_squircle_than_hybrid_more_than_expressive() {
        // Restrained (smoothing 0.65, exp 4.4) should have the most
        // pronounced squircle; Expressive (smoothing 0.20, exp 2.6)
        // the least. The smoothing × exponent combination determines
        // the effective n; assert the ordering is preserved.
        let r = RestrainedTheme::light();
        let h = HybridTheme::light();
        let e = ExpressiveTheme::light();
        let rn = r.shape().effective_corner_n();
        let hn = h.shape().effective_corner_n();
        let en = e.shape().effective_corner_n();
        assert!(rn > hn, "restrained {} > hybrid {}", rn, hn);
        assert!(hn > en, "hybrid {} > expressive {}", hn, en);
        // All three are > 1.0 (above the "circle" baseline).
        assert!(en > 1.0, "expressive n must be > 1.0, got {}", en);
    }

    #[test]
    fn bundle_carries_light_and_dark() {
        let b = HybridTheme::bundle();
        assert_eq!(b.name, "Universal · Hybrid");
        assert_eq!(b.light.color_scheme(), crate::ColorScheme::Light);
        assert_eq!(b.dark.color_scheme(), crate::ColorScheme::Dark);
    }

    #[test]
    fn universal_themes_opt_in_to_squircle() {
        // The three variants override the trait default — their
        // `shape()` must NOT be the default "off" state.
        assert!(!RestrainedTheme::light().shape().is_off());
        assert!(!HybridTheme::light().shape().is_off());
        assert!(!ExpressiveTheme::light().shape().is_off());
    }

    #[test]
    fn blinc_theme_aliases_hybrid() {
        // `BlincTheme` is now a type alias for `HybridTheme` — the
        // framework's canonical default. Squircle is opted-in.
        assert!(!crate::BlincTheme::light().shape().is_off());
        assert_eq!(crate::BlincTheme::light().name(), "Universal · Hybrid");
    }
}

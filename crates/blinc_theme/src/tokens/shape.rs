//! Continuous-curvature corner shape (squircle / superellipse) tokens.
//!
//! Sits alongside [`RadiusTokens`](super::RadiusTokens) in the
//! [`Theme`](crate::Theme) trait. Where `RadiusTokens` answers
//! *"how big is the corner"*, `ShapeTokens` answers *"what curve
//! does the corner follow"*. The two are independent — the radius
//! sets the corner's reach, the shape sets its profile.
//!
//! ## Default is "off"
//!
//! `ShapeTokens::default()` is the no-op configuration (circular
//! arcs everywhere, threshold above all practical radii), so
//! existing themes (Catppuccin, the per-platform bundles) keep
//! their current behaviour after the field is added to their
//! struct. Only themes that explicitly construct a non-default
//! `ShapeTokens` opt into squircle rendering.
//!
//! ## Squircle math
//!
//! A squircle is a [superellipse](https://en.wikipedia.org/wiki/Superellipse)
//! `|x/r|^N + |y/r|^N = 1`, with `N` controlling how square-ish the
//! corner is. `N = 2` is a circle; `N = 4` is the classic "Apple-style"
//! continuous-curvature corner; larger values approach a true square.
//!
//! - [`corner_exponent`](ShapeTokens::corner_exponent) — the target N.
//! - [`corner_smoothing`](ShapeTokens::corner_smoothing) — a 0..1 dial
//!   that interpolates between a plain circle (0.0) and the full
//!   superellipse at `corner_exponent` (1.0). Useful for picking a
//!   "considered HID" middle ground without committing to the full
//!   exponent. The effective exponent is
//!   `2 + (corner_exponent - 2) * corner_smoothing`.
//! - [`smoothing_threshold`](ShapeTokens::smoothing_threshold) — corners
//!   below this radius fall back to a circular arc, since the
//!   squircle difference is imperceptible at small sizes.

/// Semantic shape token keys for dynamic access.
#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq)]
pub enum ShapeToken {
    CornerSmoothing,
    CornerExponent,
    SmoothingThreshold,
}

/// Complete set of corner-shape tokens.
///
/// See the module-level documentation for the math and the
/// "default is off" semantics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeTokens {
    /// 0.0 = circular arc; 1.0 = full superellipse at
    /// `corner_exponent`. Values 0.4..0.7 read as "considered HID."
    pub corner_smoothing: f32,

    /// Math-space superellipse exponent `N` for
    /// `|x/r|^N + |y/r|^N = 1`. `2.0` = circle; `~3.3` is the
    /// perceptual sweet spot used by the Hybrid Universal variant;
    /// `~4.4` is Apple-ish; `> 6` looks square.
    pub corner_exponent: f32,

    /// Below this radius (in logical pixels) the renderer falls
    /// back to a circular arc — squircle smoothing is imperceptible
    /// at small sizes and just wastes path complexity. Per-corner:
    /// asymmetric radii where only some corners exceed the
    /// threshold mix circular + squircle in the same element.
    pub smoothing_threshold: f32,
}

impl ShapeTokens {
    /// Get a numeric token value by key.
    pub fn get(&self, token: ShapeToken) -> f32 {
        match token {
            ShapeToken::CornerSmoothing => self.corner_smoothing,
            ShapeToken::CornerExponent => self.corner_exponent,
            ShapeToken::SmoothingThreshold => self.smoothing_threshold,
        }
    }

    /// True when this set is the default "off / circular"
    /// configuration. The paint walker uses this as a fast
    /// short-circuit to skip the substitution entirely on themes
    /// that don't opt in.
    pub fn is_off(&self) -> bool {
        self.corner_smoothing <= 0.001 || !self.smoothing_threshold.is_finite()
    }

    /// Effective math-space superellipse exponent, interpolated
    /// between a circle (`2.0`) and `corner_exponent` by
    /// `corner_smoothing`. Clamped so values < 2 don't produce a
    /// concave corner (use `blinc_core::CornerShape::SCOOP` for
    /// that case via an explicit per-element override).
    pub fn effective_math_exponent(&self) -> f32 {
        2.0 + (self.corner_exponent.max(2.0) - 2.0) * self.corner_smoothing.clamp(0.0, 1.0)
    }

    /// Effective `n` in Blinc's GPU shape-field encoding (the field
    /// consumed by `sd_shaped_rect` as `p_exp = 2^|n|`).
    ///
    /// - `n = 1.0` → circle (the default `CornerShape::ROUND`).
    /// - `n = 2.0` → standard squircle (`CornerShape::SQUIRCLE`).
    /// - Larger n approaches a sharp square.
    ///
    /// The paint walker reads this and stamps it on each corner
    /// that passes the threshold check.
    pub fn effective_corner_n(&self) -> f32 {
        // log2(2) = 1 (circle), log2(4) = 2 (classic squircle),
        // log2(2.5) ≈ 1.32 (Hybrid sweet spot).
        self.effective_math_exponent().log2().max(0.0)
    }

    /// Linear interpolation between two shape token sets. Useful
    /// for animated theme transitions.
    pub fn lerp(from: &Self, to: &Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        Self {
            corner_smoothing: from.corner_smoothing
                + (to.corner_smoothing - from.corner_smoothing) * t,
            corner_exponent: from.corner_exponent + (to.corner_exponent - from.corner_exponent) * t,
            // `f32::INFINITY` doesn't lerp meaningfully — if either
            // end is infinite, hold whichever finite one we have.
            smoothing_threshold: match (
                from.smoothing_threshold.is_finite(),
                to.smoothing_threshold.is_finite(),
            ) {
                (true, true) => {
                    from.smoothing_threshold
                        + (to.smoothing_threshold - from.smoothing_threshold) * t
                }
                (true, false) => from.smoothing_threshold,
                (false, true) => to.smoothing_threshold,
                (false, false) => f32::INFINITY,
            },
        }
    }
}

impl Default for ShapeTokens {
    /// Off — circular arcs everywhere. See the module-level
    /// documentation for the rationale.
    fn default() -> Self {
        Self {
            corner_smoothing: 0.0,
            corner_exponent: 2.0,
            smoothing_threshold: f32::INFINITY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_off() {
        assert!(ShapeTokens::default().is_off());
        assert_eq!(ShapeTokens::default().effective_corner_n(), 1.0); // log2(2)
    }

    #[test]
    fn hybrid_sweet_spot() {
        // Hybrid: smoothing 0.40, exponent 3.3
        let t = ShapeTokens {
            corner_smoothing: 0.40,
            corner_exponent: 3.3,
            smoothing_threshold: 12.0,
        };
        assert!(!t.is_off());
        // effective math exponent = 2 + (3.3 - 2) * 0.40 = 2.52
        let n = t.effective_corner_n();
        assert!((n - 2.52_f32.log2()).abs() < 0.001, "got {}", n);
    }

    #[test]
    fn restrained_more_squircle_than_expressive() {
        let restrained = ShapeTokens {
            corner_smoothing: 0.65,
            corner_exponent: 4.4,
            smoothing_threshold: 12.0,
        };
        let expressive = ShapeTokens {
            corner_smoothing: 0.20,
            corner_exponent: 2.6,
            smoothing_threshold: 16.0,
        };
        // Restrained is the Apple-leaning variant — its squircle should
        // be more pronounced than the Material-leaning Expressive.
        assert!(restrained.effective_corner_n() > expressive.effective_corner_n());
    }
}

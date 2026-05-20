//! Small render-side helpers used across the painter walkers.
//!
//! - `extract_mask_alphas` — pulls start/end alpha (or luminance ×
//!   alpha when `luminance == true`) from a `Vec<GradientStop>` for
//!   the CSS `mask-image: linear-gradient(...)` flow.
//! - `has_glass` — reports whether the tree contains any
//!   `Material::Glass` nodes; lets the platform layer decide whether
//!   to spin up the dual-context layered render path.
//! - `apply_opacity_to_brush` — `motion_opacity` and similar
//!   per-frame multipliers fold into solid / blur brushes by adjusting
//!   their alpha; gradient and image brushes pass through unchanged
//!   (TODO in the existing code base).

use blinc_core::{Brush, Color, CornerRadius, CornerShape, GradientStop};
use blinc_theme::ShapeTokens;

use crate::element::Material;

use super::super::RenderTree;

impl RenderTree {
    /// Extract start and end alpha values from gradient stops for mask gradient
    pub(crate) fn extract_mask_alphas(stops: &[GradientStop], luminance: bool) -> (f32, f32) {
        if stops.is_empty() {
            return (1.0, 0.0);
        }
        let first = &stops[0].color;
        let last = &stops[stops.len() - 1].color;
        if luminance {
            // Luminance mode: use perceived luminance * alpha
            let lum_first = (0.2126 * first.r + 0.7152 * first.g + 0.0722 * first.b) * first.a;
            let lum_last = (0.2126 * last.r + 0.7152 * last.g + 0.0722 * last.b) * last.a;
            (lum_first, lum_last)
        } else {
            // Alpha mode: use color's alpha channel directly
            (first.a, last.a)
        }
    }

    /// Check if this tree contains any glass elements
    pub fn has_glass(&self) -> bool {
        self.render_nodes
            .values()
            .any(|node| matches!(node.props.material, Some(Material::Glass(_))))
    }

    /// Transform a local-space axis-aligned `(0, 0, width, height)`
    /// rectangle through a 6-element affine `[a, b, c, d, tx, ty]`
    /// and scale by `dpi`, returning the screen-space AABB as
    /// `[min_x, min_y, max_x, max_y]`.
    ///
    /// Used by the Compositor v2 walker to compute each
    /// `DynamicRegion`'s `screen_aabb` for dispatch-scissor and
    /// (Phase 4) damage-rect rebuild. Always returns a
    /// conservative (axis-aligned) bound — for rotated affines this
    /// expands to enclose all four corners after transformation,
    /// which is exactly what a scissor / damage rect needs.
    ///
    /// Affine convention matches the rest of the renderer:
    /// `new_x = a*x + c*y + tx`, `new_y = b*x + d*y + ty`.
    pub(crate) fn affine_screen_aabb(
        affine: &[f32; 6],
        width: f32,
        height: f32,
        dpi: f32,
    ) -> [f32; 4] {
        let [a, b, c, d, tx, ty] = *affine;
        let corners = [(0.0, 0.0), (width, 0.0), (0.0, height), (width, height)];
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for (x, y) in corners {
            let nx = a * x + c * y + tx;
            let ny = b * x + d * y + ty;
            if nx < min_x {
                min_x = nx;
            }
            if ny < min_y {
                min_y = ny;
            }
            if nx > max_x {
                max_x = nx;
            }
            if ny > max_y {
                max_y = ny;
            }
        }
        let dpi = dpi.max(1.0);
        [min_x * dpi, min_y * dpi, max_x * dpi, max_y * dpi]
    }
}

/// Substitute the theme's effective squircle `n` on any corner
/// whose radius passes the threshold check, when the element has
/// no explicit per-element `corner_shape` override.
///
/// Decision flow (top-level then per corner):
///
/// - **Explicit override.** If `explicit` is not the default
///   `CornerShape::ROUND`, return it unchanged — user-provided
///   `.squircle()` / `.bevel()` / CSS `corner-shape` wins over the
///   theme.
/// - **Theme off.** If the active theme returns the default
///   [`ShapeTokens`] (smoothing 0 or threshold = infinity), keep
///   circular corners — existing themes (platform bundles,
///   Catppuccin BlincTheme) all sit here via the trait's default
///   `fn shape()` impl.
/// - **Pill / circle.** If every corner's radius is at least half
///   the element's shorter side, the rendered shape is a pill
///   (or true circle when width == height). Spinners, avatars,
///   switch thumbs, badge dots all fall here — applying a
///   squircle `n` would deform their natural curve. The whole
///   element stays at `n = 1.0`.
/// - **Radius_full** (theme token marking "fully circular"). Same
///   intent as the pill case via a different signal: when a
///   corner's radius matches the theme's `radius_full` (within
///   1%), it must render as a true circle even if the element
///   isn't sized to be a pill.
/// - **Below threshold.** Squircle smoothing is imperceptible at
///   small radii and only wastes path complexity. Below
///   `theme_shape.smoothing_threshold` the corner falls back to
///   `n = 1.0`.
/// - **Otherwise.** Stamp the theme's
///   [`effective_corner_n`](ShapeTokens::effective_corner_n) on
///   that corner.
///
/// The per-corner evaluation handles asymmetric radii — a card
/// with `rounded_t_lg().rounded_b_sm()` keeps the bottom corners
/// circular even when the top ones get the squircle treatment.
pub fn resolve_corner_shape(
    explicit: CornerShape,
    radius: CornerRadius,
    bounds: (f32, f32),
    theme_shape: &ShapeTokens,
    radius_full: f32,
    shape_locked: bool,
) -> CornerShape {
    if shape_locked || !explicit.is_round() {
        // `shape_locked` is the explicit opt-out for floating
        // overlay widgets that want circular corners regardless of
        // theme. `!is_round()` covers user overrides via .squircle()
        // / .bevel() / CSS `corner-shape:` — both keep precedence
        // over the theme.
        return explicit;
    }
    if theme_shape.is_off() {
        return CornerShape::ROUND;
    }
    // Pill / circle short-circuit. The GPU rounded-rect SDF clamps
    // each corner radius to `min(half_width, half_height)`, which
    // means once every corner radius meets that ceiling the visible
    // shape is two semicircles joined by a straight edge (a pill;
    // or a circle when width == height). Substituting a squircle
    // `n` on such an element makes the natural curve "wobble"
    // (visible in cn::spinner where width = height and radius =
    // width / 2). Skip the substitution entirely.
    let (w, h) = bounds;
    let half_short = (w.min(h)) * 0.5;
    let is_pill = half_short > 0.0
        && radius.top_left >= half_short - 0.5
        && radius.top_right >= half_short - 0.5
        && radius.bottom_right >= half_short - 0.5
        && radius.bottom_left >= half_short - 0.5;
    if is_pill {
        return CornerShape::ROUND;
    }
    let n = theme_shape.effective_corner_n();
    let threshold = theme_shape.smoothing_threshold;
    // 99% of radius_full catches the legacy "explicit fully-round"
    // pattern (`radius_full = 9999` matched against any large
    // radius that intends "fully circular") without false-positives
    // for legitimately large but still rectangular sheets. Mostly
    // redundant with the pill short-circuit above on sized
    // elements, but kept as a belt-and-braces guard for paths that
    // can't measure bounds.
    let full_cutoff = radius_full * 0.99;
    let nf = |r: f32| -> f32 {
        if r >= full_cutoff || r < threshold {
            1.0
        } else {
            n
        }
    };
    CornerShape::new(
        nf(radius.top_left),
        nf(radius.top_right),
        nf(radius.bottom_right),
        nf(radius.bottom_left),
    )
}

#[cfg(test)]
mod resolve_corner_shape_tests {
    use super::*;

    fn hybrid_shape() -> ShapeTokens {
        ShapeTokens {
            corner_smoothing: 0.40,
            corner_exponent: 3.3,
            smoothing_threshold: 12.0,
        }
    }

    // Generous default bounds — 200×100 large enough to avoid the
    // pill short-circuit on the test cases that aren't specifically
    // about pills.
    const REC: (f32, f32) = (200.0, 100.0);

    #[test]
    fn explicit_override_wins_over_theme() {
        let resolved = resolve_corner_shape(
            CornerShape::BEVEL,
            CornerRadius::uniform(20.0),
            REC,
            &hybrid_shape(),
            9999.0,
            false,
        );
        assert_eq!(resolved, CornerShape::BEVEL);
    }

    #[test]
    fn theme_off_keeps_circular() {
        let resolved = resolve_corner_shape(
            CornerShape::ROUND,
            CornerRadius::uniform(20.0),
            REC,
            &ShapeTokens::default(),
            9999.0,
            false,
        );
        assert_eq!(resolved, CornerShape::ROUND);
    }

    #[test]
    fn radius_full_stays_true_circle() {
        let resolved = resolve_corner_shape(
            CornerShape::ROUND,
            CornerRadius::uniform(9999.0),
            REC,
            &hybrid_shape(),
            9999.0,
            false,
        );
        assert_eq!(resolved, CornerShape::ROUND);
    }

    #[test]
    fn shape_locked_keeps_explicit_round() {
        // Floating-overlay opt-out: when a widget sets
        // `corner_shape_locked = true` (e.g. via `.lock_corner_shape()`),
        // the resolver returns the explicit shape unchanged even if
        // it's the default ROUND. Popovers / dropdowns / select
        // menus use this to keep their corners circular under any
        // theme.
        let resolved = resolve_corner_shape(
            CornerShape::ROUND,
            CornerRadius::uniform(20.0),
            REC,
            &hybrid_shape(),
            9999.0,
            true, // shape_locked
        );
        assert_eq!(resolved, CornerShape::ROUND);
    }

    #[test]
    fn pill_stays_true_round() {
        // Spinner / capsule pattern: square element with radius =
        // half-side. The visible shape is a circle; substituting
        // squircle `n` would wobble it. Resolver must keep ROUND.
        let resolved = resolve_corner_shape(
            CornerShape::ROUND,
            CornerRadius::uniform(16.0),
            (32.0, 32.0),
            &hybrid_shape(),
            9999.0,
            false,
        );
        assert_eq!(resolved, CornerShape::ROUND);
    }

    #[test]
    fn horizontal_pill_stays_round() {
        // Wide pill: 200×40 with radius 20 = half the shorter side.
        // The rendered shape is two semicircles joined by a flat
        // edge; substituting squircle would deform the half-circles.
        let resolved = resolve_corner_shape(
            CornerShape::ROUND,
            CornerRadius::uniform(20.0),
            (200.0, 40.0),
            &hybrid_shape(),
            9999.0,
            false,
        );
        assert_eq!(resolved, CornerShape::ROUND);
    }

    #[test]
    fn near_pill_with_one_smaller_corner_takes_theme() {
        // Three corners at the pill ceiling but one corner pinned to
        // a smaller radius — not a clean pill, so the theme applies.
        let resolved = resolve_corner_shape(
            CornerShape::ROUND,
            CornerRadius::new(20.0, 20.0, 20.0, 4.0),
            (40.0, 40.0),
            &hybrid_shape(),
            9999.0,
            false,
        );
        // The 4.0 corner is below threshold (12.0) → stays round;
        // the three pill-ceiling corners take the theme n.
        let n = hybrid_shape().effective_corner_n();
        assert!((resolved.bottom_left - 1.0).abs() < 0.001);
        assert!((resolved.top_left - n).abs() < 0.001);
    }

    #[test]
    fn below_threshold_per_corner_stays_circular() {
        let resolved = resolve_corner_shape(
            CornerShape::ROUND,
            CornerRadius::new(8.0, 16.0, 16.0, 8.0),
            REC,
            &hybrid_shape(),
            9999.0,
            false,
        );
        let n = hybrid_shape().effective_corner_n();
        assert!((resolved.top_left - 1.0).abs() < 0.001);
        assert!((resolved.bottom_left - 1.0).abs() < 0.001);
        assert!((resolved.top_right - n).abs() < 0.001);
        assert!((resolved.bottom_right - n).abs() < 0.001);
    }

    #[test]
    fn above_threshold_applies_theme_n() {
        let resolved = resolve_corner_shape(
            CornerShape::ROUND,
            CornerRadius::uniform(20.0),
            REC,
            &hybrid_shape(),
            9999.0,
            false,
        );
        let n = hybrid_shape().effective_corner_n();
        assert!((resolved.top_left - n).abs() < 0.001);
        assert!((resolved.top_right - n).abs() < 0.001);
        assert!((resolved.bottom_right - n).abs() < 0.001);
        assert!((resolved.bottom_left - n).abs() < 0.001);
        assert!(n > 1.0 && n < 2.0, "got n = {}", n);
    }
}

/// Apply opacity to a brush by modifying its alpha component
pub(crate) fn apply_opacity_to_brush(brush: &Brush, opacity: f32) -> Brush {
    match brush {
        Brush::Solid(color) => {
            Brush::Solid(Color::rgba(color.r, color.g, color.b, color.a * opacity))
        }
        Brush::Gradient(gradient) => {
            // For gradients, we'd need to modify both start and end colors
            // For now, just return the gradient as-is
            // TODO: Apply opacity to gradient stops
            Brush::Gradient(gradient.clone())
        }
        Brush::Glass(glass) => {
            // Glass already has its own opacity handling
            Brush::Glass(*glass)
        }
        Brush::Image(image) => {
            // Image brushes - return as-is for now
            // TODO: Apply opacity to image brush
            Brush::Image(image.clone())
        }
        Brush::Blur(blur) => {
            // Blur with adjusted opacity
            let mut blur_adjusted = *blur;
            blur_adjusted.opacity *= opacity;
            Brush::Blur(blur_adjusted)
        }
    }
}

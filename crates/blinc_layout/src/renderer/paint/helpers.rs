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

use blinc_core::{Brush, Color, GradientStop};

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

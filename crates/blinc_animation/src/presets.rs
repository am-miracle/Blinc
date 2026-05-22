//! Animation presets for common entry/exit animations
//!
//! Pre-built animations that can be used with `motion()` containers
//! or directly on elements.

use crate::easing::Easing;
use crate::keyframe::{KeyframeProperties, MultiKeyframeAnimation};

/// Pre-built animation presets for common patterns
pub struct AnimationPreset;

impl AnimationPreset {
    // ========================================================================
    // Fade animations
    // ========================================================================

    /// Fade in from transparent to opaque
    pub fn fade_in(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(0.0, KeyframeProperties::opacity(0.0), Easing::Linear)
            .keyframe(1.0, KeyframeProperties::opacity(1.0), Easing::EaseOut)
    }

    /// Fade out from opaque to transparent
    pub fn fade_out(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(0.0, KeyframeProperties::opacity(1.0), Easing::Linear)
            .keyframe(1.0, KeyframeProperties::opacity(0.0), Easing::EaseIn)
    }

    // ========================================================================
    // Scale animations
    // ========================================================================

    /// Scale in from small to full size with fade
    pub fn scale_in(duration_ms: u32) -> MultiKeyframeAnimation {
        Self::scale_in_with(duration_ms, Easing::EaseOutCubic)
    }

    /// Scale in with a caller-supplied final-keyframe easing. Lets
    /// theme-aware callers thread a semantic curve (e.g.
    /// `ease_sheet` / `ease_state`) through the preset without
    /// rolling their own keyframe construction.
    pub fn scale_in_with(duration_ms: u32, easing: Easing) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale(0.0)
                    .with_opacity(0.0),
                Easing::Linear,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale(1.0)
                    .with_opacity(1.0),
                easing,
            )
    }

    /// Scale out from full size to small with fade
    pub fn scale_out(duration_ms: u32) -> MultiKeyframeAnimation {
        Self::scale_out_with(duration_ms, Easing::EaseInCubic)
    }

    /// Scale out with a caller-supplied final-keyframe easing.
    pub fn scale_out_with(duration_ms: u32, easing: Easing) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale(1.0)
                    .with_opacity(1.0),
                Easing::Linear,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale(0.0)
                    .with_opacity(0.0),
                easing,
            )
    }

    /// Pop in with slight overshoot
    pub fn pop_in(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale(0.0)
                    .with_opacity(0.0),
                Easing::Linear,
            )
            .keyframe(
                0.7,
                KeyframeProperties::default()
                    .with_scale(1.1)
                    .with_opacity(1.0),
                Easing::EaseOut,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale(1.0)
                    .with_opacity(1.0),
                Easing::EaseInOut,
            )
    }

    // ========================================================================
    // Slide animations
    // ========================================================================

    /// Slide in from an arbitrary `(dx, dy)` offset with a caller-
    /// supplied final-keyframe easing. The directional helpers
    /// (`slide_in_left` etc.) call this with their respective offsets
    /// and `Easing::EaseOutCubic`; theme-aware callers pass their
    /// semantic curve directly (e.g. `ease_sheet`).
    pub fn slide_in_with(
        duration_ms: u32,
        dx: f32,
        dy: f32,
        easing: Easing,
    ) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_translate(dx, dy)
                    .with_opacity(0.0),
                Easing::Linear,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_translate(0.0, 0.0)
                    .with_opacity(1.0),
                easing,
            )
    }

    /// Slide out to an arbitrary `(dx, dy)` offset with a caller-
    /// supplied final-keyframe easing — companion to [`slide_in_with`].
    pub fn slide_out_with(
        duration_ms: u32,
        dx: f32,
        dy: f32,
        easing: Easing,
    ) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_translate(0.0, 0.0)
                    .with_opacity(1.0),
                Easing::Linear,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_translate(dx, dy)
                    .with_opacity(0.0),
                easing,
            )
    }

    /// Slide in from the left
    pub fn slide_in_left(duration_ms: u32, distance: f32) -> MultiKeyframeAnimation {
        Self::slide_in_with(duration_ms, -distance, 0.0, Easing::EaseOutCubic)
    }

    /// Slide in from the right
    pub fn slide_in_right(duration_ms: u32, distance: f32) -> MultiKeyframeAnimation {
        Self::slide_in_with(duration_ms, distance, 0.0, Easing::EaseOutCubic)
    }

    /// Slide in from the top
    pub fn slide_in_top(duration_ms: u32, distance: f32) -> MultiKeyframeAnimation {
        Self::slide_in_with(duration_ms, 0.0, -distance, Easing::EaseOutCubic)
    }

    /// Slide in from the bottom
    pub fn slide_in_bottom(duration_ms: u32, distance: f32) -> MultiKeyframeAnimation {
        Self::slide_in_with(duration_ms, 0.0, distance, Easing::EaseOutCubic)
    }

    /// Slide out to the left
    pub fn slide_out_left(duration_ms: u32, distance: f32) -> MultiKeyframeAnimation {
        Self::slide_out_with(duration_ms, -distance, 0.0, Easing::EaseInCubic)
    }

    /// Slide out to the right
    pub fn slide_out_right(duration_ms: u32, distance: f32) -> MultiKeyframeAnimation {
        Self::slide_out_with(duration_ms, distance, 0.0, Easing::EaseInCubic)
    }

    /// Slide out to the top
    pub fn slide_out_top(duration_ms: u32, distance: f32) -> MultiKeyframeAnimation {
        Self::slide_out_with(duration_ms, 0.0, -distance, Easing::EaseInCubic)
    }

    /// Slide out to the bottom
    pub fn slide_out_bottom(duration_ms: u32, distance: f32) -> MultiKeyframeAnimation {
        Self::slide_out_with(duration_ms, 0.0, distance, Easing::EaseInCubic)
    }

    // ========================================================================
    // Bounce animations
    // ========================================================================

    /// Bounce in with overshoot effect
    pub fn bounce_in(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale(0.0)
                    .with_opacity(0.0),
                Easing::Linear,
            )
            .keyframe(
                0.5,
                KeyframeProperties::default()
                    .with_scale(1.15)
                    .with_opacity(1.0),
                Easing::EaseOut,
            )
            .keyframe(
                0.75,
                KeyframeProperties::default()
                    .with_scale(0.95)
                    .with_opacity(1.0),
                Easing::EaseInOut,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale(1.0)
                    .with_opacity(1.0),
                Easing::EaseOut,
            )
    }

    /// Bounce out with squash effect
    pub fn bounce_out(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale(1.0)
                    .with_opacity(1.0),
                Easing::Linear,
            )
            .keyframe(
                0.25,
                KeyframeProperties::default()
                    .with_scale(1.1)
                    .with_opacity(1.0),
                Easing::EaseOut,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale(0.0)
                    .with_opacity(0.0),
                Easing::EaseIn,
            )
    }

    // ========================================================================
    // Special effect animations
    // ========================================================================

    /// Shake horizontally (for error feedback)
    pub fn shake(duration_ms: u32, intensity: f32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(0.0, KeyframeProperties::translate(0.0, 0.0), Easing::Linear)
            .keyframe(
                0.1,
                KeyframeProperties::translate(-intensity, 0.0),
                Easing::EaseOut,
            )
            .keyframe(
                0.3,
                KeyframeProperties::translate(intensity, 0.0),
                Easing::EaseInOut,
            )
            .keyframe(
                0.5,
                KeyframeProperties::translate(-intensity * 0.8, 0.0),
                Easing::EaseInOut,
            )
            .keyframe(
                0.7,
                KeyframeProperties::translate(intensity * 0.6, 0.0),
                Easing::EaseInOut,
            )
            .keyframe(
                0.9,
                KeyframeProperties::translate(-intensity * 0.3, 0.0),
                Easing::EaseInOut,
            )
            .keyframe(
                1.0,
                KeyframeProperties::translate(0.0, 0.0),
                Easing::EaseOut,
            )
    }

    /// Pulse (scale up and down)
    pub fn pulse(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(0.0, KeyframeProperties::scale(1.0), Easing::Linear)
            .keyframe(0.5, KeyframeProperties::scale(1.1), Easing::EaseInOut)
            .keyframe(1.0, KeyframeProperties::scale(1.0), Easing::EaseInOut)
    }

    /// Spin rotation (full 360 degrees)
    pub fn spin(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(0.0, KeyframeProperties::rotation(0.0), Easing::Linear)
            .keyframe(1.0, KeyframeProperties::rotation(360.0), Easing::Linear)
    }

    /// Flip in on X axis
    pub fn flip_in_x(duration_ms: u32) -> MultiKeyframeAnimation {
        // Note: This is a simplified flip using scale_y as a stand-in
        // True 3D flip would require perspective transforms
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale_xy(1.0, 0.0)
                    .with_opacity(0.0),
                Easing::Linear,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale_xy(1.0, 1.0)
                    .with_opacity(1.0),
                Easing::EaseOutCubic,
            )
    }

    /// Flip in on Y axis
    pub fn flip_in_y(duration_ms: u32) -> MultiKeyframeAnimation {
        // Note: This is a simplified flip using scale_x as a stand-in
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale_xy(0.0, 1.0)
                    .with_opacity(0.0),
                Easing::Linear,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale_xy(1.0, 1.0)
                    .with_opacity(1.0),
                Easing::EaseOutCubic,
            )
    }

    /// Zoom in with slight rotation
    pub fn zoom_in_rotate(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale(0.3)
                    .with_rotate(-15.0)
                    .with_opacity(0.0),
                Easing::Linear,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale(1.0)
                    .with_rotate(0.0)
                    .with_opacity(1.0),
                Easing::EaseOutCubic,
            )
    }

    /// Drop in from above with bounce
    pub fn drop_in(duration_ms: u32, drop_distance: f32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_translate(0.0, -drop_distance)
                    .with_opacity(0.0),
                Easing::Linear,
            )
            .keyframe(
                0.6,
                KeyframeProperties::default()
                    .with_translate(0.0, 10.0)
                    .with_opacity(1.0),
                Easing::EaseIn,
            )
            .keyframe(
                0.8,
                KeyframeProperties::default()
                    .with_translate(0.0, -5.0)
                    .with_opacity(1.0),
                Easing::EaseOut,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_translate(0.0, 0.0)
                    .with_opacity(1.0),
                Easing::EaseInOut,
            )
    }

    // ========================================================================
    // Dialog/Modal animations (subtle scale + fade)
    // ========================================================================

    /// Dialog enter animation - subtle scale up from 95% with fade
    ///
    /// More appropriate for modals/dialogs than `scale_in` which starts from 0%.
    pub fn dialog_in(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale(0.95)
                    .with_opacity(0.0),
                Easing::Linear,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale(1.0)
                    .with_opacity(1.0),
                Easing::EaseOutCubic,
            )
    }

    /// Dialog exit animation - subtle scale down to 95% with fade
    pub fn dialog_out(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale(1.0)
                    .with_opacity(1.0),
                Easing::Linear,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale(0.95)
                    .with_opacity(0.0),
                Easing::EaseInCubic,
            )
    }

    /// Gentle grow enter animation - very subtle scale (99% to 100%) with fade
    ///
    /// Minimal scale change to reduce visual distortion while still providing
    /// a sense of the element "growing" into place. Good for content with text
    /// where scale artifacts are more noticeable.
    pub fn grow_in(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale(0.99)
                    .with_opacity(0.0),
                Easing::Linear,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale(1.0)
                    .with_opacity(1.0),
                Easing::EaseOutCubic,
            )
    }

    /// Gentle grow exit animation - very subtle scale with fade
    pub fn grow_out(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale(1.0)
                    .with_opacity(1.0),
                Easing::Linear,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale(0.99)
                    .with_opacity(0.0),
                Easing::EaseInCubic,
            )
    }

    /// Expand enter animation - grows from slightly smaller with a subtle "pop"
    ///
    /// Similar to grow_in but with a slight overshoot for a more dynamic feel.
    /// Scale goes from 98% → 100.5% → 100%.
    pub fn expand_in(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale(0.98)
                    .with_opacity(0.0),
                Easing::Linear,
            )
            .keyframe(
                0.7,
                KeyframeProperties::default()
                    .with_scale(1.005)
                    .with_opacity(1.0),
                Easing::EaseOut,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale(1.0)
                    .with_opacity(1.0),
                Easing::EaseInOut,
            )
    }

    /// Expand exit animation - shrinks slightly with fade
    pub fn expand_out(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale(1.0)
                    .with_opacity(1.0),
                Easing::Linear,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale(0.98)
                    .with_opacity(0.0),
                Easing::EaseInCubic,
            )
    }

    // ========================================================================
    // Dropdown/Menu animations (slide + fade, faster than dialogs)
    // ========================================================================

    /// Dropdown/menu enter animation - slide down with fade
    ///
    /// Appropriate for dropdowns, context menus, and other popup menus.
    /// Uses a slight downward slide for a natural "dropping" feel.
    pub fn dropdown_in(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale(0.98)
                    .with_opacity(0.0)
                    .with_translate(0.0, -4.0),
                Easing::Linear,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale(1.0)
                    .with_opacity(1.0)
                    .with_translate(0.0, 0.0),
                Easing::EaseOutCubic,
            )
    }

    /// Dropdown/menu exit animation - slide up with fade
    pub fn dropdown_out(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale(1.0)
                    .with_opacity(1.0)
                    .with_translate(0.0, 0.0),
                Easing::Linear,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale(0.98)
                    .with_opacity(0.0)
                    .with_translate(0.0, -4.0),
                Easing::EaseInCubic,
            )
    }

    /// Context menu enter animation - subtle scale from origin with fade
    ///
    /// Similar to dropdown but uses scale from a corner origin feel.
    pub fn context_menu_in(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale(0.95)
                    .with_opacity(0.0),
                Easing::Linear,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale(1.0)
                    .with_opacity(1.0),
                Easing::EaseOutCubic,
            )
    }

    /// Context menu exit animation - subtle scale with fade
    pub fn context_menu_out(duration_ms: u32) -> MultiKeyframeAnimation {
        MultiKeyframeAnimation::new(duration_ms)
            .keyframe(
                0.0,
                KeyframeProperties::default()
                    .with_scale(1.0)
                    .with_opacity(1.0),
                Easing::Linear,
            )
            .keyframe(
                1.0,
                KeyframeProperties::default()
                    .with_scale(0.95)
                    .with_opacity(0.0),
                Easing::EaseInCubic,
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fade_in_animation() {
        let mut anim = AnimationPreset::fade_in(300);
        anim.start();

        // At start
        let props = anim.current_properties();
        assert!((props.resolved_opacity() - 0.0).abs() < 0.01);

        // At end
        anim.tick(300.0);
        let props = anim.current_properties();
        assert!((props.resolved_opacity() - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_scale_in_animation() {
        let mut anim = AnimationPreset::scale_in(400);
        anim.start();

        // At start
        let props = anim.current_properties();
        let (sx, sy) = props.resolved_scale();
        assert!((sx - 0.0).abs() < 0.01);
        assert!((sy - 0.0).abs() < 0.01);

        // At end
        anim.tick(400.0);
        let props = anim.current_properties();
        let (sx, sy) = props.resolved_scale();
        assert!((sx - 1.0).abs() < 0.01);
        assert!((sy - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_slide_in_left_animation() {
        let mut anim = AnimationPreset::slide_in_left(300, 50.0);
        anim.start();

        // At start
        let props = anim.current_properties();
        let (tx, ty) = props.resolved_translate();
        assert!((tx - (-50.0)).abs() < 0.01);
        assert!((ty - 0.0).abs() < 0.01);

        // At end
        anim.tick(300.0);
        let props = anim.current_properties();
        let (tx, ty) = props.resolved_translate();
        assert!((tx - 0.0).abs() < 0.01);
        assert!((ty - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_bounce_in_has_overshoot() {
        let mut anim = AnimationPreset::bounce_in(400);
        anim.start();

        // At 50% should be at overshoot (scale > 1.0)
        anim.tick(200.0);
        let props = anim.current_properties();
        let (sx, _) = props.resolved_scale();
        assert!(sx > 1.0, "Should overshoot at 50%");
    }

    #[test]
    fn test_shake_returns_to_origin() {
        let mut anim = AnimationPreset::shake(300, 10.0);
        anim.start();

        // At end should be back at origin
        anim.tick(300.0);
        let props = anim.current_properties();
        let (tx, ty) = props.resolved_translate();
        assert!((tx - 0.0).abs() < 0.01);
        assert!((ty - 0.0).abs() < 0.01);
    }
}

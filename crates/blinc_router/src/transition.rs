//! Page transition configuration
//!
//! Transitions use Blinc's animation system — `MultiKeyframeAnimation`
//! for preset effects, `SpringConfig` for physics-based custom transitions.

use blinc_animation::{keyframe::MultiKeyframeAnimation, AnimationPreset, SpringConfig};

/// Page transition configuration.
///
/// Defines how a page animates in (enter) and out (exit).
/// Use presets for common patterns or provide custom animations.
#[derive(Clone, Debug)]
pub struct PageTransition {
    /// Animation when this page enters the stack
    pub enter: MultiKeyframeAnimation,
    /// Animation when this page exits the stack
    pub exit: MultiKeyframeAnimation,
    /// Optional spring config for physics-based transitions
    pub spring: Option<SpringConfig>,
}

impl PageTransition {
    /// Custom transition with explicit enter/exit animations
    pub fn new(enter: MultiKeyframeAnimation, exit: MultiKeyframeAnimation) -> Self {
        Self {
            enter,
            exit,
            spring: None,
        }
    }

    /// iOS-style: slide in from right, slide out to right
    pub fn slide() -> Self {
        Self::new(
            AnimationPreset::slide_in_right(300, 0.0),
            AnimationPreset::slide_out_right(300, 0.0),
        )
    }

    /// Crossfade between pages
    pub fn fade() -> Self {
        Self::new(
            AnimationPreset::fade_in(200),
            AnimationPreset::fade_out(200),
        )
    }

    /// Modal: slide up from bottom, dismiss down
    pub fn modal() -> Self {
        Self::new(
            AnimationPreset::slide_in_bottom(300, 0.0),
            AnimationPreset::slide_out_bottom(300, 0.0),
        )
    }

    /// Scale + fade
    pub fn scale() -> Self {
        Self::new(
            AnimationPreset::scale_in(250),
            AnimationPreset::scale_out(200),
        )
    }

    /// No animation — instant swap
    pub fn none() -> Self {
        Self::new(AnimationPreset::fade_in(0), AnimationPreset::fade_out(0))
    }

    /// Override with spring physics for interruptible transitions
    pub fn with_spring(mut self, spring: SpringConfig) -> Self {
        self.spring = Some(spring);
        self
    }
}

impl Default for PageTransition {
    fn default() -> Self {
        Self::slide()
    }
}

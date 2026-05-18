//! Spinner component for loading indicators
//!
//! A circular loading indicator that uses motion + clip-path to render a
//! rotating 180° arc on top of a static track ring. Two primitives total
//! (track + arc), so the static cache holds the track and only the arc's
//! single motion-bound primitive is dispatched per frame.
//!
//! # Example
//!
//! ```ignore
//! use blinc_cn::prelude::*;
//! use blinc_animation::AnimationContextExt;
//!
//! fn loading_view(ctx: &impl AnimationContext) -> impl ElementBuilder {
//!     let timeline = ctx.use_animated_timeline();
//!     cn::spinner(timeline)
//! }
//!
//! fn custom_spinner(ctx: &impl AnimationContext) -> impl ElementBuilder {
//!     let timeline = ctx.use_animated_timeline();
//!     cn::spinner(timeline)
//!         .size(SpinnerSize::Large)
//!         .color(Color::BLUE)
//!         .track_color(Color::rgba(0.0, 0.0, 1.0, 0.2))
//! }
//!
//! fn slow_spinner(ctx: &impl AnimationContext) -> impl ElementBuilder {
//!     let timeline = ctx.use_animated_timeline();
//!     cn::spinner(timeline)
//!         .duration_ms(2000)
//! }
//! ```

use blinc_animation::SharedAnimatedTimeline;
use blinc_core::{ClipLength, ClipPath, Color};
use blinc_layout::div::{Div, ElementTypeId};
use blinc_layout::element::RenderProps;
use blinc_layout::prelude::*;
use blinc_layout::tree::{LayoutNodeId, LayoutTree};
use blinc_theme::{ColorToken, ThemeState};
use std::cell::OnceCell;
use std::sync::Arc;
use taffy::Style;

/// Spinner size variants
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpinnerSize {
    /// Small spinner (16px)
    Small,
    /// Medium spinner (24px)
    #[default]
    Medium,
    /// Large spinner (32px)
    Large,
}

impl SpinnerSize {
    fn diameter(&self) -> f32 {
        match self {
            SpinnerSize::Small => 16.0,
            SpinnerSize::Medium => 24.0,
            SpinnerSize::Large => 32.0,
        }
    }

    fn border_width(&self) -> f32 {
        match self {
            SpinnerSize::Small => 2.0,
            SpinnerSize::Medium => 2.5,
            SpinnerSize::Large => 3.0,
        }
    }
}

/// Animated spinner component for loading indicators
///
/// Renders as a static track ring + a motion-bound arc div whose visible
/// region is masked by a clip-path polygon to 180°. The arc's rotation
/// comes from a timeline-driven `motion().rotate_timeline()`; under the
/// compositor's dynamic-batch routing the static cache never invalidates
/// while the spinner spins.
pub struct Spinner {
    timeline: SharedAnimatedTimeline,
    size: SpinnerSize,
    color: Option<Color>,
    track_color: Option<Color>,
    duration_ms: u32,
    classes: Vec<Arc<str>>,
    user_id: Option<String>,
    /// Lazily built composed Div. Initialized on first ElementBuilder
    /// method call (after all chained config methods).
    built: OnceCell<Div>,
}

impl Spinner {
    /// Create a new spinning spinner
    ///
    /// The timeline is configured for infinite rotation on first build.
    pub fn new(timeline: SharedAnimatedTimeline) -> Self {
        Self {
            timeline,
            size: SpinnerSize::default(),
            color: None,
            track_color: None,
            duration_ms: 1000,
            classes: Vec::new(),
            user_id: None,
            built: OnceCell::new(),
        }
    }

    /// Set the spinner size
    pub fn size(mut self, size: SpinnerSize) -> Self {
        self.size = size;
        self
    }

    /// Set the spinner color (the rotating arc)
    pub fn color(mut self, color: impl Into<Color>) -> Self {
        self.color = Some(color.into());
        self
    }

    /// Set the track color (the static background circle)
    pub fn track_color(mut self, color: impl Into<Color>) -> Self {
        self.track_color = Some(color.into());
        self
    }

    /// Set the rotation duration in milliseconds (default: 1000ms)
    pub fn duration_ms(mut self, duration: u32) -> Self {
        self.duration_ms = duration;
        self
    }

    /// Add a CSS class for selector matching
    pub fn class(mut self, name: impl AsRef<str>) -> Self {
        self.classes.push(blinc_core::intern::intern(name.as_ref()));
        self
    }

    /// Set the element ID for CSS selector matching
    pub fn id(mut self, id: &str) -> Self {
        self.user_id = Some(id.to_string());
        self
    }

    fn get_or_build(&self) -> &Div {
        self.built.get_or_init(|| self.build_inner())
    }

    fn build_inner(&self) -> Div {
        let theme = ThemeState::get();
        let diameter = self.size.diameter();
        let border_width = self.size.border_width();
        let half = diameter / 2.0;
        let spinner_color = self
            .color
            .unwrap_or_else(|| theme.color(ColorToken::Primary));
        let track_color = self
            .track_color
            .unwrap_or_else(|| theme.color(ColorToken::Border));

        // Configure timeline once (idempotent: subsequent calls return
        // the existing entry id rather than re-adding entries).
        let entry_id = self.timeline.lock().unwrap().configure(|t| {
            let id = t.add(0, self.duration_ms, 0.0, 360.0);
            t.set_loop(-1);
            t.start();
            id
        });

        // Polygon mask: rectangle covering the left half. The right half
        // of the ring is masked out, leaving a 180° visible arc.
        // Vertices in element-local coords; they rotate with motion's
        // timeline rotation thanks to the shader's `sp - bounds.xy`
        // polygon test.
        let polygon = ClipPath::Polygon {
            points: vec![
                (ClipLength::Px(0.0), ClipLength::Px(0.0)),
                (ClipLength::Px(half), ClipLength::Px(0.0)),
                (ClipLength::Px(half), ClipLength::Px(diameter)),
                (ClipLength::Px(0.0), ClipLength::Px(diameter)),
            ],
        };

        // Track ring — static, lives in the static cache.
        let track = div()
            .class("cn-spinner__track")
            .absolute()
            .inset(0.0)
            .w(diameter)
            .h(diameter)
            .rounded(half)
            .border(border_width, track_color);

        // Arc ring — motion-bound, rotates via timeline. Polygon clip
        // masks the top-right quadrant to leave a 270° visible arc.
        let arc = motion()
            .rotate_timeline(self.timeline.clone(), entry_id)
            .child(
                div()
                    .class("cn-spinner__arc")
                    .w(diameter)
                    .h(diameter)
                    .rounded(half)
                    .border(border_width, spinner_color)
                    .clip_path(polygon),
            );

        let arc_layer = div().absolute().inset(0.0).w(diameter).h(diameter).child(arc);

        // Container. Total size includes border-width padding on each
        // side so the ring's stroke isn't clipped by the parent layout.
        let total = diameter + border_width;
        let mut spinner = div()
            .class("cn-spinner")
            .w(total)
            .h(total)
            .relative()
            .child(track)
            .child(arc_layer);

        for c in &self.classes {
            spinner = spinner.class(c.as_ref());
        }
        if let Some(ref id) = self.user_id {
            spinner = spinner.id(id);
        }

        spinner
    }
}

impl ElementBuilder for Spinner {
    fn build(&self, tree: &mut LayoutTree) -> LayoutNodeId {
        self.get_or_build().build(tree)
    }

    fn render_props(&self) -> RenderProps {
        self.get_or_build().render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        self.get_or_build().children_builders()
    }

    fn element_type_id(&self) -> ElementTypeId {
        ElementTypeId::Div
    }

    fn layout_style(&self) -> Option<&Style> {
        self.get_or_build().layout_style()
    }

    fn element_classes(&self) -> &[Arc<str>] {
        self.get_or_build().element_classes()
    }

    fn element_id(&self) -> Option<&str> {
        self.get_or_build().element_id()
    }
}

/// Create an animated spinner loading indicator
///
/// Takes an `AnimatedTimeline` from the animation context. The timeline
/// is automatically configured for infinite rotation.
///
/// # Example
///
/// ```ignore
/// use blinc_cn::prelude::*;
/// use blinc_animation::AnimationContextExt;
///
/// fn loading(ctx: &impl AnimationContext) -> impl ElementBuilder {
///     let timeline = ctx.use_animated_timeline();
///     cn::spinner(timeline)
/// }
/// ```
pub fn spinner(timeline: SharedAnimatedTimeline) -> Spinner {
    Spinner::new(timeline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spinner_size_values() {
        assert_eq!(SpinnerSize::Small.diameter(), 16.0);
        assert_eq!(SpinnerSize::Medium.diameter(), 24.0);
        assert_eq!(SpinnerSize::Large.diameter(), 32.0);
    }

    #[test]
    fn test_spinner_border_widths() {
        assert_eq!(SpinnerSize::Small.border_width(), 2.0);
        assert_eq!(SpinnerSize::Medium.border_width(), 2.5);
        assert_eq!(SpinnerSize::Large.border_width(), 3.0);
    }
}

//! Spinner component for loading indicators
//!
//! A circular loading indicator that spins continuously using shape primitives
//! plus a single rotation binding. The renderer's compositor fast path
//! patches the rotation in place each frame — no canvas closure, no
//! per-frame walker pass.
//!
//! # Example
//!
//! ```ignore
//! use blinc_cn::prelude::*;
//! use blinc_animation::AnimationContextExt;
//!
//! // Create a spinning loader
//! fn loading_view(ctx: &impl AnimationContext) -> impl ElementBuilder {
//!     let timeline = ctx.use_animated_timeline();
//!     cn::spinner(timeline)
//! }
//!
//! // Custom size and colors
//! fn custom_spinner(ctx: &impl AnimationContext) -> impl ElementBuilder {
//!     let timeline = ctx.use_animated_timeline();
//!     cn::spinner(timeline)
//!         .size(SpinnerSize::Large)
//!         .color(Color::BLUE)
//!         .track_color(Color::rgba(0.0, 0.0, 1.0, 0.2))
//! }
//!
//! // Custom rotation duration (slower spin)
//! fn slow_spinner(ctx: &impl AnimationContext) -> impl ElementBuilder {
//!     let timeline = ctx.use_animated_timeline();
//!     cn::spinner(timeline)
//!         .duration_ms(2000) // 2 seconds per rotation
//! }
//! ```

use blinc_animation::SharedAnimatedTimeline;
use blinc_core::Color;
use blinc_layout::div::{Div, ElementTypeId};
use blinc_layout::motion::motion;
use blinc_layout::prelude::*;
use blinc_theme::{ColorToken, ThemeState};
use std::cell::OnceCell;
use std::f32::consts::PI;

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

/// Inner config for the spinner, separated so it can be cloned into
/// the lazy-built Div without mutating the parent builder.
#[derive(Clone)]
struct SpinnerConfig {
    timeline: SharedAnimatedTimeline,
    size: SpinnerSize,
    color: Option<Color>,
    track_color: Option<Color>,
    duration_ms: u32,
    classes: Vec<std::sync::Arc<str>>,
    user_id: Option<String>,
}

/// Animated spinner component for loading indicators
///
/// Built from a static track of rect segments plus a rotation-bound arc.
/// The compositor fast path patches the arc's rotation in place each
/// frame, so the surrounding tree stays cached and the walker never has
/// to repaint anything except the rotating segments.
///
/// Uses the same lazy `OnceCell` pattern as
/// [`AnimatedProgress`] — builder methods accumulate config and the
/// inner `Div` only materialises on first `ElementBuilder` access.
/// That keeps `children_builders()` consistent with what `build()`
/// actually constructs (so the renderer's
/// `collect_render_props_boxed` doesn't see a layout-children /
/// builder-children mismatch and walk the wrong thing).
pub struct Spinner {
    config: SpinnerConfig,
    built: OnceCell<Div>,
}

impl Spinner {
    /// Create a new spinning spinner
    ///
    /// The timeline will be configured for infinite rotation on first render.
    pub fn new(timeline: SharedAnimatedTimeline) -> Self {
        Self {
            config: SpinnerConfig {
                timeline,
                size: SpinnerSize::default(),
                color: None,
                track_color: None,
                duration_ms: 1000, // 1 second per rotation
                classes: Vec::new(),
                user_id: None,
            },
            built: OnceCell::new(),
        }
    }

    /// Set the spinner size
    pub fn size(mut self, size: SpinnerSize) -> Self {
        self.config.size = size;
        self
    }

    /// Set the spinner color (the spinning arc)
    pub fn color(mut self, color: impl Into<Color>) -> Self {
        self.config.color = Some(color.into());
        self
    }

    /// Set the track color (the background circle)
    pub fn track_color(mut self, color: impl Into<Color>) -> Self {
        self.config.track_color = Some(color.into());
        self
    }

    /// Set the rotation duration in milliseconds (default: 1000ms)
    ///
    /// Lower values = faster spin, higher values = slower spin.
    pub fn duration_ms(mut self, duration: u32) -> Self {
        self.config.duration_ms = duration;
        self
    }

    /// Add a CSS class for selector matching
    pub fn class(mut self, name: impl AsRef<str>) -> Self {
        self.config
            .classes
            .push(blinc_core::intern::intern(name.as_ref()));
        self
    }

    /// Set the element ID for CSS selector matching
    pub fn id(mut self, id: &str) -> Self {
        self.config.user_id = Some(id.to_string());
        self
    }

    /// Lazy-initialise the inner Div. Each builder method updates the
    /// config and the next ElementBuilder access materialises the
    /// final tree against the latest values. Subsequent calls return
    /// the cached Div so build / children_builders / render_props all
    /// see the same structure.
    fn get_or_build(&self) -> &Div {
        self.built.get_or_init(|| build_spinner_div(&self.config))
    }
}

/// Build the full spinner Div: static track behind a rotation-bound arc.
fn build_spinner_div(cfg: &SpinnerConfig) -> Div {
    let theme = ThemeState::get();
    let diameter = cfg.size.diameter();
    let border_width = cfg.size.border_width();
    let spinner_color = cfg
        .color
        .unwrap_or_else(|| theme.color(ColorToken::Primary));
    let track_color = cfg
        .track_color
        .unwrap_or_else(|| theme.color(ColorToken::Border));

    // Match the old canvas's padding: total layout box is
    // `diameter + border_width * 2` so an off-axis arc segment
    // doesn't get clipped at the edge.
    let total_size = diameter + border_width * 2.0;
    let cx = total_size / 2.0;
    let cy = total_size / 2.0;
    let radius = (diameter - border_width) / 2.0;

    // Configure the timeline on first access (the `configure`
    // closure short-circuits on subsequent calls — re-runs of the
    // builder return the same `entry_id` instead of re-adding a
    // duplicate keyframe track).
    let entry_id = cfg.timeline.lock().unwrap().configure(|t| {
        let id = t.add(0, cfg.duration_ms, 0.0, 360.0);
        t.set_loop(-1); // Infinite loop
        t.start();
        id
    });

    // Static track ring. Each segment is an axis-aligned rect rotated
    // into place via per-element `rotate(angle)` — these never
    // re-paint, the compositor caches them on the first walk and
    // keeps them across every fast-path frame.
    let mut track = div()
        .absolute()
        .top(0.0)
        .left(0.0)
        .w(total_size)
        .h(total_size);
    let track_segments = 32usize;
    for i in 0..track_segments {
        let t1 = i as f32 / track_segments as f32;
        let t2 = (i + 1) as f32 / track_segments as f32;
        let a1 = t1 * 2.0 * PI;
        let a2 = t2 * 2.0 * PI;
        let x1 = cx + radius * a1.cos();
        let y1 = cy + radius * a1.sin();
        let x2 = cx + radius * a2.cos();
        let y2 = cy + radius * a2.sin();
        let dx = x2 - x1;
        let dy = y2 - y1;
        let len = (dx * dx + dy * dy).sqrt();
        let mid_x = (x1 + x2) * 0.5;
        let mid_y = (y1 + y2) * 0.5;
        let angle = dy.atan2(dx);
        let seg = div()
            .absolute()
            .left(mid_x - (len + border_width) * 0.5)
            .top(mid_y - border_width * 0.5)
            .w(len + border_width)
            .h(border_width)
            .rounded(border_width / 2.0)
            .bg(track_color)
            .rotate(angle);
        track = track.child(seg);
    }

    // Spinning arc — 24 segments laid out across a 270° span, alpha
    // fading from 0.3 at the tail to 1.0 at the head. The segments
    // themselves are static; the whole arc rotates as a unit via the
    // motion wrapper, so the fast path only ever patches one rotation
    // binding to drive the visual.
    let arc_length = PI * 1.5;
    let segments = 24usize;
    let mut arc = div()
        .absolute()
        .top(0.0)
        .left(0.0)
        .w(total_size)
        .h(total_size);
    for i in 0..segments {
        let t1 = i as f32 / segments as f32;
        let t2 = (i + 1) as f32 / segments as f32;
        let a1 = t1 * arc_length;
        let a2 = t2 * arc_length;
        let x1 = cx + radius * a1.cos();
        let y1 = cy + radius * a1.sin();
        let x2 = cx + radius * a2.cos();
        let y2 = cy + radius * a2.sin();
        let dx = x2 - x1;
        let dy = y2 - y1;
        let len = (dx * dx + dy * dy).sqrt();
        let mid_x = (x1 + x2) * 0.5;
        let mid_y = (y1 + y2) * 0.5;
        let angle = dy.atan2(dx);

        // Trail fades from 0.3 (back) to 1.0 (leading edge).
        let alpha = 0.3 + 0.7 * t1;
        let color_with_alpha =
            Color::rgba(spinner_color.r, spinner_color.g, spinner_color.b, alpha);

        let seg = div()
            .absolute()
            .left(mid_x - (len + border_width) * 0.5)
            .top(mid_y - border_width * 0.5)
            .w(len + border_width)
            .h(border_width)
            .rounded(border_width / 2.0)
            .bg(color_with_alpha)
            .rotate(angle);
        arc = arc.child(seg);
    }

    // Wrap the arc in a motion container bound to the rotation
    // timeline. `MotionBindings::rotation_timeline` flows through the
    // renderer as `Transform::translate(c, c) · R(θ) ·
    // Transform::translate(-c, -c)` on each paint, and the
    // compositor fast path patches the resulting rotation delta into
    // every painted child primitive in place — exactly the path
    // `apply_binding_deltas` now handles natively.
    let spinning = motion()
        .rotate_timeline(cfg.timeline.clone(), entry_id)
        .child(arc);

    // Container holds both layers. `relative` so the absolutely-
    // positioned children resolve against this box.
    let mut container = div()
        .relative()
        .w(total_size)
        .h(total_size)
        .child(track)
        .child(spinning);
    for class in &cfg.classes {
        container = container.class(class.as_ref());
    }
    if let Some(id) = &cfg.user_id {
        container = container.id(id);
    }
    container
}

impl ElementBuilder for Spinner {
    fn build(&self, tree: &mut blinc_layout::tree::LayoutTree) -> blinc_layout::tree::LayoutNodeId {
        self.get_or_build().build(tree)
    }

    fn render_props(&self) -> blinc_layout::element::RenderProps {
        self.get_or_build().render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        self.get_or_build().children_builders()
    }

    fn element_type_id(&self) -> ElementTypeId {
        self.get_or_build().element_type_id()
    }

    fn layout_style(&self) -> Option<&taffy::Style> {
        self.get_or_build().layout_style()
    }

    fn element_classes(&self) -> &[std::sync::Arc<str>] {
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

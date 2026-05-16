//! Spinner component for loading indicators
//!
//! A circular loading indicator that spins continuously using two
//! SDF primitives — a static ring (track) and a polygon-clipped
//! arc rotating via a motion-bound subtree. The arc lives in the
//! compositor's motion-subtree overlay: each frame the renderer
//! re-walks ONLY the arc's tiny subtree with the current rotation
//! and dispatches a single primitive on top of the cached
//! surrounding tree. No canvas closure, no per-segment dots, no
//! walking of surrounding elements during animation.
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
//! ```

use blinc_animation::SharedAnimatedTimeline;
use blinc_core::layer::{ClipLength, ClipPath};
use blinc_core::Color;
use blinc_layout::div::{Div, ElementTypeId};
use blinc_layout::motion::motion;
use blinc_layout::prelude::*;
use blinc_theme::{ColorToken, ThemeState};
use std::cell::OnceCell;

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

/// Animated spinner component.
///
/// Renders as two SDF primitives:
/// 1. A static ring (full circle outline) — lives in the static-
///    layer cache, never re-renders.
/// 2. A polygon-clipped arc (270° of the same ring) — wrapped in
///    a `motion` with `rotate_timeline`. Excluded from the static
///    cache by the compositor's motion-subtree overlay; re-walked
///    each frame with the current rotation, costing one primitive
///    dispatch per frame.
pub struct Spinner {
    config: SpinnerConfig,
    built: OnceCell<Div>,
}

impl Spinner {
    pub fn new(timeline: SharedAnimatedTimeline) -> Self {
        Self {
            config: SpinnerConfig {
                timeline,
                size: SpinnerSize::default(),
                color: None,
                track_color: None,
                duration_ms: 1000,
                classes: Vec::new(),
                user_id: None,
            },
            built: OnceCell::new(),
        }
    }

    pub fn size(mut self, size: SpinnerSize) -> Self {
        self.config.size = size;
        self
    }

    pub fn color(mut self, color: impl Into<Color>) -> Self {
        self.config.color = Some(color.into());
        self
    }

    pub fn track_color(mut self, color: impl Into<Color>) -> Self {
        self.config.track_color = Some(color.into());
        self
    }

    pub fn duration_ms(mut self, duration: u32) -> Self {
        self.config.duration_ms = duration;
        self
    }

    pub fn class(mut self, name: impl AsRef<str>) -> Self {
        self.config
            .classes
            .push(blinc_core::intern::intern(name.as_ref()));
        self
    }

    pub fn id(mut self, id: &str) -> Self {
        self.config.user_id = Some(id.to_string());
        self
    }

    fn get_or_build(&self) -> &Div {
        self.built.get_or_init(|| build_spinner_div(&self.config))
    }
}

/// Build the spinner: container + static track + motion-wrapped
/// polygon-clipped arc.
///
/// The static track is one SDF primitive (rounded square + border
/// → circle outline). The arc is the same ring with a polygon
/// clip-path masking it to a 270° sweep — also one SDF primitive,
/// but wrapped in a `motion` whose `rotate_timeline` spins it.
///
/// During steady-state animation the compositor:
///   - Blits the static cache (which contains everything EXCEPT
///     the arc subtree — the track is included, the rest of the
///     UI is included).
///   - Re-walks the arc's motion subtree (2 nodes: motion + arc)
///     with the current rotation timeline value.
///   - Dispatches the single arc primitive on top of the blit.
///
/// Cost per frame: 1 primitive dispatch + 2-node walk per spinner.
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

    let total_size = diameter + border_width * 2.0;
    let cx = total_size / 2.0;
    let cy = total_size / 2.0;

    let entry_id = cfg.timeline.lock().unwrap().configure(|t| {
        let id = t.add(0, cfg.duration_ms, 0.0, 360.0);
        t.set_loop(-1);
        t.start();
        id
    });

    // Static track ring — one SDF primitive (rounded + border).
    let track = div()
        .absolute()
        .top(0.0)
        .left(0.0)
        .w(total_size)
        .h(total_size)
        .rounded(total_size / 2.0)
        .border(border_width, track_color);

    // Arc ring with polygon clip-path. The polygon traces:
    //   (0,0) → (cx,0) → (cx,cy) → (size,cy) → (size,size) → (0,size)
    // which encloses everything EXCEPT the top-right quadrant —
    // applied to the circular ring this exposes a 270° arc with
    // the 90° gap in the top-right.
    //
    // NOT marked `.absolute()` so it contributes to the motion
    // wrapper's intrinsic size (auto-height). Without that, the
    // motion's `height: Auto` collapses to 0 and rotation pivots
    // at top-centre instead of geometric centre.
    let arc = div()
        .w(total_size)
        .h(total_size)
        .rounded(total_size / 2.0)
        .border(border_width, spinner_color)
        .clip_path(ClipPath::Polygon {
            points: vec![
                (ClipLength::Px(0.0), ClipLength::Px(0.0)),
                (ClipLength::Px(cx), ClipLength::Px(0.0)),
                (ClipLength::Px(cx), ClipLength::Px(cy)),
                (ClipLength::Px(total_size), ClipLength::Px(cy)),
                (ClipLength::Px(total_size), ClipLength::Px(total_size)),
                (ClipLength::Px(0.0), ClipLength::Px(total_size)),
            ],
        });

    // Motion wrapper drives the rotation via the looping timeline.
    // `rotate_timeline` registers the binding as a
    // `MotionBindings::rotation_timeline` whose `is_playing()` is
    // always true, so the compositor's
    // `should_skip_for_motion_overlay` check detects it and excludes
    // the subtree from the static-layer cache — the overlay pass
    // re-walks just this motion subtree (2 nodes) every frame.
    let spinning_motion = motion()
        .rotate_timeline(cfg.timeline.clone(), entry_id)
        .child(arc);

    // Wrap the motion in an absolutely-positioned div so it
    // overlays the track at the same screen coords. The motion
    // wrapper sizes itself to its content (the arc div, which has
    // explicit `total_size × total_size`), so rotation pivots at
    // its geometric centre.
    let spinning_overlay = div()
        .absolute()
        .top(0.0)
        .left(0.0)
        .w(total_size)
        .h(total_size)
        .child(spinning_motion);

    let mut container = div()
        .relative()
        .w(total_size)
        .h(total_size)
        .child(track)
        .child(spinning_overlay);

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

//! Progress component - visual indicator of completion status
//!
//! A themed horizontal progress bar following shadcn/ui patterns.
//! Supports both static values and smooth spring-animated updates.
//!
//! # Example
//!
//! ```ignore
//! use blinc_cn::prelude::*;
//!
//! // Basic progress bar at 75%
//! cn::progress(75.0)
//!
//! // With custom size
//! cn::progress(50.0)
//!     .size(ProgressSize::Large)
//!
//! // Fixed width progress bar
//! cn::progress(30.0)
//!     .w(300.0)
//!
//! // Custom colors
//! cn::progress(100.0)
//!     .indicator_color(Color::GREEN)
//!     .track_color(Color::rgba(0.0, 1.0, 0.0, 0.2))
//! ```
//!
//! # Animated Progress
//!
//! For smooth spring-animated progress updates, use `progress_animated`:
//!
//! ```ignore
//! use blinc_cn::prelude::*;
//! use blinc_animation::prelude::*;
//!
//! fn loading_bar(ctx: &impl AnimationContext) -> impl ElementBuilder {
//!     let width = 300.0;
//!     // Create animated value (0 to width pixel range)
//!     let progress_anim = ctx.use_animated_value(0.0);
//!
//!     // Later, update progress smoothly (in pixels):
//!     // progress_anim.lock().unwrap().set_target(width * 0.75); // 75%
//!
//!     cn::progress_animated(progress_anim)
//!         .size(ProgressSize::Large)
//!         .w(width)
//! }
//! ```

use blinc_animation::SharedAnimatedValue;
use blinc_core::Color;
use blinc_layout::prelude::*;
use blinc_theme::{ColorToken, RadiusToken, ThemeState};

/// Progress bar size variants
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProgressSize {
    /// Small progress bar (4px height)
    Small,
    /// Medium progress bar (8px height)
    #[default]
    Medium,
    /// Large progress bar (12px height)
    Large,
}

impl ProgressSize {
    fn height(&self) -> f32 {
        match self {
            ProgressSize::Small => 4.0,
            ProgressSize::Medium => 8.0,
            ProgressSize::Large => 12.0,
        }
    }
}

/// Configuration for building a Progress bar
#[derive(Clone)]
struct ProgressConfig {
    /// Progress value from 0.0 to 100.0
    value: f32,
    size: ProgressSize,
    width: f32,
    indicator_color: Option<Color>,
    track_color: Option<Color>,
    corner_radius: Option<f32>,
}

impl ProgressConfig {
    fn new(value: f32) -> Self {
        Self {
            value: value.clamp(0.0, 100.0),
            size: ProgressSize::default(),
            width: 200.0, // Default width
            indicator_color: None,
            track_color: None,
            corner_radius: None,
        }
    }
}

/// Styled Progress bar component
pub struct Progress {
    inner: Div,
}

impl Progress {
    fn from_config(config: ProgressConfig) -> Self {
        let theme = ThemeState::get();

        let height = config.size.height();
        let radius = config
            .corner_radius
            .unwrap_or_else(|| theme.radius(RadiusToken::Full));

        let indicator_color = config
            .indicator_color
            .unwrap_or_else(|| theme.color(ColorToken::Primary));
        let track_color = config
            .track_color
            .unwrap_or_else(|| theme.color(ColorToken::Secondary));

        // Calculate fill width in pixels
        let fill_ratio = config.value / 100.0;
        let fill_width = config.width * fill_ratio;

        let size_class = match config.size {
            ProgressSize::Small => "cn-progress--sm",
            ProgressSize::Medium => "cn-progress--md",
            ProgressSize::Large => "cn-progress--lg",
        };

        // Build the indicator (filled portion) - absolutely positioned
        let indicator = div()
            .class("cn-progress-bar")
            .absolute()
            .left(0.0)
            .top(0.0)
            .w(fill_width)
            .h(height)
            .rounded(radius)
            .bg(indicator_color);

        // Track container with overflow clipping
        let track = div()
            .class("cn-progress")
            .class(size_class)
            .w(config.width)
            .h(height)
            .rounded(radius)
            .bg(track_color)
            .overflow_clip()
            .relative()
            .child(indicator);

        Self { inner: track }
    }

    /// Add a CSS class for selector matching
    pub fn class(mut self, name: impl AsRef<str>) -> Self {
        self.inner = self.inner.class(name);
        self
    }

    /// Set the element ID for CSS selector matching
    pub fn id(mut self, id: &str) -> Self {
        self.inner = self.inner.id(id);
        self
    }
}

impl ElementBuilder for Progress {
    fn build(&self, tree: &mut blinc_layout::tree::LayoutTree) -> blinc_layout::tree::LayoutNodeId {
        self.inner.build(tree)
    }

    fn render_props(&self) -> blinc_layout::element::RenderProps {
        self.inner.render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        self.inner.children_builders()
    }

    fn element_type_id(&self) -> blinc_layout::div::ElementTypeId {
        self.inner.element_type_id()
    }

    fn layout_style(&self) -> Option<&taffy::Style> {
        self.inner.layout_style()
    }

    fn element_classes(&self) -> &[std::sync::Arc<str>] {
        self.inner.element_classes()
    }
}

/// Builder for creating Progress components with fluent API
pub struct ProgressBuilder {
    config: ProgressConfig,
    built: std::cell::OnceCell<Progress>,
}

impl ProgressBuilder {
    /// Create a new progress builder with the given value (0-100)
    pub fn new(value: f32) -> Self {
        Self {
            config: ProgressConfig::new(value),
            built: std::cell::OnceCell::new(),
        }
    }

    /// Get or build the inner Progress
    fn get_or_build(&self) -> &Progress {
        self.built
            .get_or_init(|| Progress::from_config(self.config.clone()))
    }

    /// Set the progress bar size
    pub fn size(mut self, size: ProgressSize) -> Self {
        self.config.size = size;
        self
    }

    /// Set the width in pixels
    pub fn w(mut self, width: f32) -> Self {
        self.config.width = width;
        self
    }

    /// Set the indicator (filled portion) color
    pub fn indicator_color(mut self, color: impl Into<Color>) -> Self {
        self.config.indicator_color = Some(color.into());
        self
    }

    /// Set the track (background) color
    pub fn track_color(mut self, color: impl Into<Color>) -> Self {
        self.config.track_color = Some(color.into());
        self
    }

    /// Set the corner radius
    pub fn rounded(mut self, radius: f32) -> Self {
        self.config.corner_radius = Some(radius);
        self
    }

    /// Build the final Progress component
    pub fn build_component(self) -> Progress {
        Progress::from_config(self.config)
    }
}

impl ElementBuilder for ProgressBuilder {
    fn build(&self, tree: &mut blinc_layout::tree::LayoutTree) -> blinc_layout::tree::LayoutNodeId {
        self.get_or_build().build(tree)
    }

    fn render_props(&self) -> blinc_layout::element::RenderProps {
        self.get_or_build().render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        self.get_or_build().children_builders()
    }

    fn element_type_id(&self) -> blinc_layout::div::ElementTypeId {
        self.get_or_build().element_type_id()
    }

    fn layout_style(&self) -> Option<&taffy::Style> {
        self.get_or_build().layout_style()
    }

    fn element_classes(&self) -> &[std::sync::Arc<str>] {
        self.get_or_build().element_classes()
    }
}

/// Create a progress bar with the given value (0-100)
///
/// # Example
///
/// ```ignore
/// use blinc_cn::prelude::*;
///
/// // 75% complete
/// cn::progress(75.0)
///
/// // With size and width
/// cn::progress(50.0)
///     .size(ProgressSize::Large)
///     .w(300.0)
/// ```
pub fn progress(value: f32) -> ProgressBuilder {
    ProgressBuilder::new(value)
}

// ============================================================================
// Animated Progress
// ============================================================================

/// Configuration for animated progress bar
#[derive(Clone)]
struct AnimatedProgressConfig {
    /// Animated value (0.0 to 1.0 range)
    value: SharedAnimatedValue,
    size: ProgressSize,
    width: f32,
    indicator_color: Option<Color>,
    track_color: Option<Color>,
    corner_radius: Option<f32>,
}

impl AnimatedProgressConfig {
    fn new(value: SharedAnimatedValue) -> Self {
        Self {
            value,
            size: ProgressSize::default(),
            width: 200.0,
            indicator_color: None,
            track_color: None,
            corner_radius: None,
        }
    }
}

/// Animated progress bar component with spring physics
pub struct AnimatedProgress {
    inner: Div,
}

impl AnimatedProgress {
    fn from_config(config: AnimatedProgressConfig) -> Self {
        let theme = ThemeState::get();

        let height = config.size.height();
        let width = config.width;
        let radius = config
            .corner_radius
            .unwrap_or_else(|| theme.radius(RadiusToken::Full));

        let indicator_color = config
            .indicator_color
            .unwrap_or_else(|| theme.color(ColorToken::Primary));
        let track_color = config
            .track_color
            .unwrap_or_else(|| theme.color(ColorToken::Secondary));

        // Progress bar approach: position indicator at left edge, use translate_x to show fill
        // At translate_x = 0: indicator fully hidden (positioned at -width)
        // At translate_x = width: indicator fully visible (positioned at 0)
        // For 75%: translate_x = width * 0.75 = 225, indicator left edge at -75, right edge at 225

        // The indicator bar itself - full width, will be clipped by track
        let indicator = div().w(width).h(height).rounded(radius).bg(indicator_color);

        // Motion wrapper that translates the indicator
        // The animated value goes from 0 (empty) to width (full)
        let animated_indicator = motion().translate_x(config.value.clone()).child(indicator);

        // Position wrapper at -width so at translate_x=0, nothing is visible
        // At translate_x=225 (75%), indicator spans from -75 to +225 (225px visible)
        let positioned_wrapper = div()
            .absolute()
            .left(-width)
            .top(0.0)
            .w(width)
            .h(height)
            .child(animated_indicator);

        // Track container with overflow clipping
        let track = div()
            .w(width)
            .h(height)
            .rounded(radius)
            .bg(track_color)
            .overflow_clip()
            .relative()
            .child(positioned_wrapper);

        Self { inner: track }
    }
}

impl ElementBuilder for AnimatedProgress {
    fn build(&self, tree: &mut blinc_layout::tree::LayoutTree) -> blinc_layout::tree::LayoutNodeId {
        self.inner.build(tree)
    }

    fn render_props(&self) -> blinc_layout::element::RenderProps {
        self.inner.render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        self.inner.children_builders()
    }

    fn element_type_id(&self) -> blinc_layout::div::ElementTypeId {
        self.inner.element_type_id()
    }

    fn layout_style(&self) -> Option<&taffy::Style> {
        self.inner.layout_style()
    }

    fn element_classes(&self) -> &[std::sync::Arc<str>] {
        self.inner.element_classes()
    }
}

/// Builder for animated progress bar
pub struct AnimatedProgressBuilder {
    config: AnimatedProgressConfig,
    built: std::cell::OnceCell<AnimatedProgress>,
}

impl AnimatedProgressBuilder {
    /// Create a new animated progress builder
    pub fn new(value: SharedAnimatedValue) -> Self {
        Self {
            config: AnimatedProgressConfig::new(value),
            built: std::cell::OnceCell::new(),
        }
    }

    fn get_or_build(&self) -> &AnimatedProgress {
        self.built
            .get_or_init(|| AnimatedProgress::from_config(self.config.clone()))
    }

    /// Set the progress bar size
    pub fn size(mut self, size: ProgressSize) -> Self {
        self.config.size = size;
        self
    }

    /// Set the width in pixels
    pub fn w(mut self, width: f32) -> Self {
        self.config.width = width;
        self
    }

    /// Set the indicator (filled portion) color
    pub fn indicator_color(mut self, color: impl Into<Color>) -> Self {
        self.config.indicator_color = Some(color.into());
        self
    }

    /// Set the track (background) color
    pub fn track_color(mut self, color: impl Into<Color>) -> Self {
        self.config.track_color = Some(color.into());
        self
    }

    /// Set the corner radius
    pub fn rounded(mut self, radius: f32) -> Self {
        self.config.corner_radius = Some(radius);
        self
    }
}

impl ElementBuilder for AnimatedProgressBuilder {
    fn build(&self, tree: &mut blinc_layout::tree::LayoutTree) -> blinc_layout::tree::LayoutNodeId {
        self.get_or_build().build(tree)
    }

    fn render_props(&self) -> blinc_layout::element::RenderProps {
        self.get_or_build().render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        self.get_or_build().children_builders()
    }

    fn element_type_id(&self) -> blinc_layout::div::ElementTypeId {
        self.get_or_build().element_type_id()
    }

    fn layout_style(&self) -> Option<&taffy::Style> {
        self.get_or_build().layout_style()
    }

    fn element_classes(&self) -> &[std::sync::Arc<str>] {
        self.get_or_build().element_classes()
    }
}

/// Create an animated progress bar with spring physics
///
/// Takes a `SharedAnimatedValue` representing the fill width in pixels.
/// Animate from 0 to the track width (set via `.w()`) for 0% to 100%.
///
/// # Example
///
/// ```ignore
/// use blinc_cn::prelude::*;
/// use blinc_animation::prelude::*;
///
/// fn loading_indicator(ctx: &impl AnimationContext) -> impl ElementBuilder {
///     let width = 250.0;
///     let progress = ctx.use_animated_value(0.0);
///
///     // Update progress (in pixels, 0 to width)
///     // progress.lock().unwrap().set_target(width * 0.5); // 50%
///
///     cn::progress_animated(progress)
///         .size(ProgressSize::Medium)
///         .w(width)
/// }
/// ```
pub fn progress_animated(value: SharedAnimatedValue) -> AnimatedProgressBuilder {
    AnimatedProgressBuilder::new(value.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_theme() {
        let _ = ThemeState::try_get().unwrap_or_else(|| {
            ThemeState::init_default();
            ThemeState::get()
        });
    }

    #[test]
    fn test_progress_size_heights() {
        assert_eq!(ProgressSize::Small.height(), 4.0);
        assert_eq!(ProgressSize::Medium.height(), 8.0);
        assert_eq!(ProgressSize::Large.height(), 12.0);
    }

    #[test]
    fn test_progress_value_clamping() {
        let config = ProgressConfig::new(150.0);
        assert_eq!(config.value, 100.0);

        let config = ProgressConfig::new(-10.0);
        assert_eq!(config.value, 0.0);

        let config = ProgressConfig::new(50.0);
        assert_eq!(config.value, 50.0);
    }

    #[test]
    fn test_progress_builder() {
        init_theme();
        let pb = ProgressBuilder::new(75.0)
            .size(ProgressSize::Large)
            .w(300.0);

        assert_eq!(pb.config.value, 75.0);
        assert_eq!(pb.config.size, ProgressSize::Large);
        assert_eq!(pb.config.width, 300.0);
    }

    #[test]
    fn test_progress_fill_calculation() {
        init_theme();
        // At 50%, with 200px width, fill should be 100px
        let config = ProgressConfig::new(50.0);
        assert_eq!(config.width * (config.value / 100.0), 100.0);
    }
}

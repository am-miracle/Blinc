//! Tooltip component - lightweight informational text on hover
//!
//! A styled tooltip that appears when hovering over a trigger element.
//! Designed for simple text labels, not rich content (use HoverCard for that).
//!
//! # Example
//!
//! ```ignore
//! use blinc_cn::prelude::*;
//!
//! fn build_ui(ctx: &WindowedContext) -> impl ElementBuilder {
//!     // Basic tooltip
//!     cn::tooltip(|| cn::button("Hover me"))
//!         .text("This is a tooltip")
//!
//!     // Positioned to the right
//!     cn::tooltip(|| cn::button("Settings"))
//!         .text("Open settings panel")
//!         .side(TooltipSide::Right)
//!
//!     // With custom delays
//!     cn::tooltip(|| text("Help"))
//!         .text("Click for more info")
//!         .open_delay_ms(200)
//!         .close_delay_ms(0)
//! }
//! ```

use std::cell::OnceCell;
use std::sync::Arc;

use blinc_animation::AnimationPreset;
use blinc_core::context_state::BlincContextState;
use blinc_core::State;
use blinc_layout::div::ElementTypeId;
use blinc_layout::element::RenderProps;
use blinc_layout::overlay_state::get_overlay_manager;
use blinc_layout::prelude::*;
use blinc_layout::tree::{LayoutNodeId, LayoutTree};
use blinc_layout::widgets::overlay::{AnchorDirection, OverlayHandle, OverlayManagerExt};
use blinc_theme::{ColorToken, RadiusToken, ThemeState};

use blinc_layout::InstanceKey;

/// Side where the tooltip appears relative to the trigger
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TooltipSide {
    /// Above the trigger (default)
    #[default]
    Top,
    /// Below the trigger
    Bottom,
    /// To the right of the trigger
    Right,
    /// To the left of the trigger
    Left,
}

/// Alignment of the tooltip relative to the trigger
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TooltipAlign {
    /// Align to start of trigger
    Start,
    /// Center with trigger (default)
    #[default]
    Center,
    /// Align to end of trigger
    End,
}

/// Trigger builder function type for tooltip trigger
type TriggerBuilderFn = Arc<dyn Fn() -> Div + Send + Sync>;

/// Builder for tooltip component
pub struct TooltipBuilder {
    /// Trigger content (the element that triggers the tooltip)
    trigger: TriggerBuilderFn,
    /// Text to show in the tooltip
    text: Option<String>,
    /// Side where the tooltip appears
    side: TooltipSide,
    /// Alignment relative to trigger
    align: TooltipAlign,
    /// Delay before opening (ms)
    open_delay_ms: u32,
    /// Delay before closing (ms)
    close_delay_ms: u32,
    /// Offset from trigger (pixels)
    offset: f32,
    /// Unique instance key
    key: InstanceKey,
    /// User-added CSS classes
    classes: Vec<std::sync::Arc<str>>,
    /// User-set element ID
    user_id: Option<String>,
    /// Built component cache
    built: OnceCell<Tooltip>,
}

impl std::fmt::Debug for TooltipBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TooltipBuilder")
            .field("text", &self.text)
            .field("side", &self.side)
            .field("align", &self.align)
            .field("open_delay_ms", &self.open_delay_ms)
            .field("close_delay_ms", &self.close_delay_ms)
            .field("offset", &self.offset)
            .finish()
    }
}

impl TooltipBuilder {
    /// Create a new tooltip builder with a trigger builder function and a pre-created key
    pub fn with_key<F>(trigger_fn: F, key: InstanceKey) -> Self
    where
        F: Fn() -> Div + Send + Sync + 'static,
    {
        Self {
            trigger: Arc::new(trigger_fn),
            text: None,
            side: TooltipSide::Top,
            align: TooltipAlign::Center,
            open_delay_ms: 400, // Default 400ms delay before showing
            close_delay_ms: 0,  // Default 0ms delay - hide immediately
            offset: 6.0,
            key,
            classes: Vec::new(),
            user_id: None,
            built: OnceCell::new(),
        }
    }

    /// Set the text to display in the tooltip
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// Set the side where the tooltip appears
    pub fn side(mut self, side: TooltipSide) -> Self {
        self.side = side;
        self
    }

    /// Set the alignment relative to the trigger
    pub fn align(mut self, align: TooltipAlign) -> Self {
        self.align = align;
        self
    }

    /// Set the delay before opening (in milliseconds)
    pub fn open_delay_ms(mut self, delay: u32) -> Self {
        self.open_delay_ms = delay;
        self
    }

    /// Set the delay before closing (in milliseconds)
    pub fn close_delay_ms(mut self, delay: u32) -> Self {
        self.close_delay_ms = delay;
        self
    }

    /// Set the offset from the trigger (in pixels)
    pub fn offset(mut self, offset: f32) -> Self {
        self.offset = offset;
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

    /// Get or build the component
    fn get_or_build(&self) -> &Tooltip {
        self.built.get_or_init(|| self.build_component())
    }

    /// Build the tooltip component
    fn build_component(&self) -> Tooltip {
        let _theme = ThemeState::get();

        // Create state for tracking overlay handle
        let overlay_handle_state: State<Option<u64>> =
            BlincContextState::get().use_state_keyed(&self.key.derive("handle"), || None);

        // Clone values for closures
        let side = self.side;
        let align = self.align;
        let offset = self.offset;
        let tooltip_text = self.text.clone();
        let trigger_builder = self.trigger.clone();
        // Use the instance key to create a unique motion key for this tooltip
        let motion_key_str = format!("tooltip_{}", self.key.get());

        // Build trigger with hover handlers
        let overlay_handle_for_show = overlay_handle_state.clone();
        let overlay_handle_for_trigger_leave = overlay_handle_state.clone();
        let overlay_handle_for_trigger_enter = overlay_handle_state.clone();
        let tooltip_text_for_show = tooltip_text.clone();
        let motion_key_for_trigger = motion_key_str.clone();

        // Build the trigger element with hover detection
        let trigger_content = (trigger_builder)();

        let trigger = div()
            .w_fit()
            .align_self_start() // Prevent stretching in flex containers
            .child(trigger_content)
            .on_hover_enter(move |ctx| {
                // Build the full motion key
                let full_motion_key = format!("motion:{}:child:0", motion_key_for_trigger);

                // First, check if we have an existing overlay that's pending close or closing
                if let Some(handle_id) = overlay_handle_for_trigger_enter.get() {
                    let mgr = get_overlay_manager();
                    let handle = OverlayHandle::from_raw(handle_id);

                    // If overlay is visible, cancel any pending close
                    if mgr.is_visible(handle) {
                        if mgr.is_pending_close(handle) {
                            mgr.hover_enter(handle);
                        }
                        // Also cancel any exit animation
                        let motion = blinc_layout::selector::query_motion(&full_motion_key);
                        if motion.is_exiting() {
                            mgr.cancel_close(handle);
                            motion.cancel_exit();
                        }
                        return;
                    }
                    // Our overlay was closed externally, clear our state
                    overlay_handle_for_trigger_enter.set(None);
                }

                // Check if motion is already animating (entering) to prevent restart jitter
                let motion = blinc_layout::selector::query_motion(&full_motion_key);
                if motion.is_animating() && !motion.is_exiting() {
                    return;
                }

                // Get bounds for positioning
                let trigger_x = ctx.bounds_x;
                let trigger_y = ctx.bounds_y;
                let trigger_w = ctx.bounds_width;
                let trigger_h = ctx.bounds_height;

                // Calculate position based on side and alignment
                let (x, y) = calculate_tooltip_position(
                    trigger_x, trigger_y, trigger_w, trigger_h, side, align, offset,
                );

                // Show the tooltip content
                if let Some(ref text) = tooltip_text_for_show {
                    let text_clone = text.clone();
                    let overlay_handle_for_content = overlay_handle_for_show.clone();

                    let handle = show_tooltip_overlay(
                        x,
                        y,
                        side,
                        text_clone,
                        overlay_handle_for_content,
                        motion_key_for_trigger.clone(),
                    );

                    overlay_handle_for_show.set(Some(handle.id()));
                }
            })
            .on_hover_leave(move |_| {
                // Start close immediately or with delay
                if let Some(handle_id) = overlay_handle_for_trigger_leave.get() {
                    let mgr = get_overlay_manager();
                    let handle = OverlayHandle::from_raw(handle_id);

                    // Only start close if overlay is visible and in Open state
                    if mgr.is_visible(handle) && !mgr.is_pending_close(handle) {
                        mgr.hover_leave(handle);
                    }
                }
            });

        // Apply user classes and id
        let mut inner = trigger;
        for c in &self.classes {
            inner = inner.class(c);
        }
        if let Some(ref id) = self.user_id {
            inner = inner.id(id);
        }

        Tooltip { inner }
    }
}

/// Calculate position for tooltip based on trigger bounds
fn calculate_tooltip_position(
    trigger_x: f32,
    trigger_y: f32,
    trigger_w: f32,
    trigger_h: f32,
    side: TooltipSide,
    align: TooltipAlign,
    offset: f32,
) -> (f32, f32) {
    // Estimate tooltip width for alignment calculations
    // Tooltips are typically small, so use a smaller estimate than hover cards
    let tooltip_width_estimate = 100.0;

    match side {
        TooltipSide::Top => {
            // Position above trigger - use trigger_y - offset as the bottom anchor point
            // The overlay content will be positioned to align its bottom edge here
            let y = trigger_y - (offset * 6.0);
            let x = match align {
                TooltipAlign::Start => trigger_x,
                TooltipAlign::Center => trigger_x + (trigger_w - tooltip_width_estimate) / 2.0,
                TooltipAlign::End => trigger_x + trigger_w - tooltip_width_estimate,
            };
            (x.max(0.0), y.max(0.0))
        }
        TooltipSide::Bottom => {
            // Position below trigger
            let y = trigger_y + trigger_h + offset;
            let x = match align {
                TooltipAlign::Start => trigger_x,
                TooltipAlign::Center => trigger_x + (trigger_w - tooltip_width_estimate) / 2.0,
                TooltipAlign::End => trigger_x + trigger_w - tooltip_width_estimate,
            };
            (x.max(0.0), y)
        }
        TooltipSide::Right => {
            // Position to the right of trigger
            let x = trigger_x + trigger_w + offset;
            let y = match align {
                TooltipAlign::Start => trigger_y,
                TooltipAlign::Center => trigger_y,
                TooltipAlign::End => trigger_y,
            };
            (x, y)
        }
        TooltipSide::Left => {
            // Position to the left of trigger
            let x = trigger_x - tooltip_width_estimate - offset;
            let y = match align {
                TooltipAlign::Start => trigger_y,
                TooltipAlign::Center => trigger_y,
                TooltipAlign::End => trigger_y,
            };
            (x.max(0.0), y)
        }
    }
}

/// Show the tooltip overlay
fn show_tooltip_overlay(
    x: f32,
    y: f32,
    side: TooltipSide,
    tooltip_text: String,
    overlay_handle_state: State<Option<u64>>,
    motion_key: String,
) -> OverlayHandle {
    let theme = ThemeState::get();
    // Use inverted colors for tooltip (dark bg on light theme, light bg on dark theme)
    let bg = theme.color(ColorToken::TooltipBackground);
    let text_color = theme.color(ColorToken::TooltipText);
    let radius = theme.radius(RadiusToken::Sm);
    // padding from CSS: .cn-tooltip { padding: 6px 12px; }

    let mgr = get_overlay_manager();

    // Close any existing tooltip overlays before opening a new one
    mgr.close_all_of(blinc_layout::widgets::overlay::OverlayKind::Tooltip);

    // Clone state and key for closures
    let motion_key_for_content = motion_key.clone();

    // Use hover_card() which is a TRANSIENT overlay (both hover_card and tooltip use same behavior)
    let motion_key_with_child = format!("{}:child:0", motion_key);

    // Convert TooltipSide to AnchorDirection for correct positioning
    let anchor_dir = match side {
        TooltipSide::Top => AnchorDirection::Top,
        TooltipSide::Bottom => AnchorDirection::Bottom,
        TooltipSide::Left => AnchorDirection::Left,
        TooltipSide::Right => AnchorDirection::Right,
    };

    // Use hover_card() builder - it creates a transient overlay with OverlayKind::Tooltip
    mgr.hover_card()
        .at(x, y)
        .anchor_direction(anchor_dir)
        .motion_key(&motion_key_with_child)
        .follows_scroll(true)
        .content(move || {
            // padding from CSS: .cn-tooltip { padding: 6px 12px; }
            let tooltip_content = div()
                .class("cn-tooltip")
                .flex_row()
                .items_center()
                .bg(bg)
                .rounded(radius)
                .lock_corner_shape()
                .shadow_sm()
                .child(text(&tooltip_text).size(12.0).color(text_color).no_wrap());

            // Wrap in motion for enter/exit animations
            div().child(
                blinc_layout::motion::motion_derived(&motion_key_for_content)
                    .enter_animation(AnimationPreset::fade_in(100))
                    .exit_animation(AnimationPreset::fade_out(75))
                    .child(tooltip_content),
            )
        })
        .on_close({
            let overlay_handle = overlay_handle_state.clone();
            move || {
                overlay_handle.set(None);
            }
        })
        .show()
}

/// Built tooltip component
pub struct Tooltip {
    inner: Div,
}

impl std::ops::Deref for Tooltip {
    type Target = Div;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl std::ops::DerefMut for Tooltip {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl ElementBuilder for TooltipBuilder {
    fn build(&self, tree: &mut LayoutTree) -> LayoutNodeId {
        self.get_or_build().inner.build(tree)
    }

    fn render_props(&self) -> RenderProps {
        self.get_or_build().inner.render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        self.get_or_build().inner.children_builders()
    }

    fn element_type_id(&self) -> ElementTypeId {
        self.get_or_build().inner.element_type_id()
    }

    fn event_handlers(&self) -> Option<&blinc_layout::event_handler::EventHandlers> {
        ElementBuilder::event_handlers(&self.get_or_build().inner)
    }

    fn layout_style(&self) -> Option<&taffy::Style> {
        self.get_or_build().inner.layout_style()
    }

    fn element_classes(&self) -> &[std::sync::Arc<str>] {
        self.get_or_build().inner.element_classes()
    }

    fn element_id(&self) -> Option<&str> {
        self.get_or_build().inner.element_id()
    }
}

impl ElementBuilder for Tooltip {
    fn build(&self, tree: &mut LayoutTree) -> LayoutNodeId {
        self.inner.build(tree)
    }

    fn render_props(&self) -> RenderProps {
        self.inner.render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        self.inner.children_builders()
    }

    fn element_type_id(&self) -> ElementTypeId {
        self.inner.element_type_id()
    }

    fn event_handlers(&self) -> Option<&blinc_layout::event_handler::EventHandlers> {
        ElementBuilder::event_handlers(&self.inner)
    }

    fn layout_style(&self) -> Option<&taffy::Style> {
        self.inner.layout_style()
    }

    fn element_classes(&self) -> &[std::sync::Arc<str>] {
        self.inner.element_classes()
    }

    fn element_id(&self) -> Option<&str> {
        self.inner.element_id()
    }
}

/// Create a tooltip component with a trigger
///
/// The tooltip appears when the user hovers over the trigger element.
///
/// # Example
///
/// ```ignore
/// cn::tooltip(|| cn::button("Hover me"))
///     .text("This is a helpful tooltip")
/// ```
#[track_caller]
pub fn tooltip<F>(trigger_fn: F) -> TooltipBuilder
where
    F: Fn() -> Div + Send + Sync + 'static,
{
    // Create the key here so it captures the caller's location, not TooltipBuilder's
    let key = InstanceKey::new("tooltip");
    TooltipBuilder::with_key(trigger_fn, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tooltip_position_top() {
        let (x, y) = calculate_tooltip_position(
            100.0,
            50.0,
            80.0,
            30.0,
            TooltipSide::Top,
            TooltipAlign::Center,
            6.0,
        );
        // y uses offset * 6.0 multiplier for top positioning
        assert_eq!(y, 50.0 - (6.0 * 6.0)); // 14.0
                                           // x should be centered (tooltip_width_estimate = 100.0)
        assert_eq!(x, 100.0 + (80.0 - 100.0) / 2.0); // 90.0
    }

    #[test]
    fn test_tooltip_position_bottom() {
        let (x, y) = calculate_tooltip_position(
            100.0,
            50.0,
            80.0,
            30.0,
            TooltipSide::Bottom,
            TooltipAlign::Start,
            6.0,
        );
        assert_eq!(x, 100.0);
        assert_eq!(y, 50.0 + 30.0 + 6.0); // trigger_y + trigger_h + offset
    }

    #[test]
    fn test_tooltip_position_right() {
        let (x, y) = calculate_tooltip_position(
            100.0,
            50.0,
            80.0,
            30.0,
            TooltipSide::Right,
            TooltipAlign::Center,
            6.0,
        );
        assert_eq!(x, 100.0 + 80.0 + 6.0); // trigger_x + trigger_w + offset
        assert_eq!(y, 50.0); // y aligns with trigger_y for right side
    }
}

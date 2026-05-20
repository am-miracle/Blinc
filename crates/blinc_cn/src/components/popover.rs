//! Popover component - click-triggered floating content
//!
//! A styled overlay that appears when clicking a trigger element.
//! Unlike dropdown menus, popovers can contain any content.
//! Unlike hover cards, popovers are click-triggered and dismissed by clicking outside.
//!
//! # Example
//!
//! ```ignore
//! use blinc_cn::prelude::*;
//!
//! fn build_ui(ctx: &WindowedContext) -> impl ElementBuilder {
//!     // Basic popover with button trigger
//!     cn::popover(|| cn::button("Open"))
//!         .content(|| {
//!             div().flex_col().gap(8.0).children([
//!                 text("Popover Content").size(16.0),
//!                 cn::button("Action"),
//!             ])
//!         })
//!
//!     // Positioned to the right
//!     cn::popover(|| text("Click me"))
//!         .side(PopoverSide::Right)
//!         .content(|| text("Content on the right"))
//!
//!     // With custom alignment
//!     cn::popover(|| cn::button("Settings"))
//!         .side(PopoverSide::Bottom)
//!         .align(PopoverAlign::End)
//!         .content(|| settings_panel())
//! }
//! ```

use std::cell::OnceCell;
use std::sync::Arc;

use blinc_animation::AnimationPreset;
use blinc_core::context_state::BlincContextState;
use blinc_core::State;
use blinc_layout::div::ElementTypeId;
use blinc_layout::element::{ElementBounds, RenderProps};
use blinc_layout::motion::motion_derived;
use blinc_layout::overlay_state::get_overlay_manager;
use blinc_layout::prelude::*;
use blinc_layout::stateful::{stateful_with_key, ButtonState};
use blinc_layout::tree::{LayoutNodeId, LayoutTree};
use blinc_layout::widgets::overlay::{AnchorDirection, OverlayHandle, OverlayManagerExt};
use blinc_layout::{selector, InstanceKey};
use blinc_theme::{ColorToken, RadiusToken, SpacingToken, ThemeState};

/// Side where the popover appears relative to the trigger
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PopoverSide {
    /// Above the trigger
    Top,
    /// Below the trigger (default)
    #[default]
    Bottom,
    /// To the right of the trigger
    Right,
    /// To the left of the trigger
    Left,
}

/// Alignment of the popover relative to the trigger
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PopoverAlign {
    /// Align to start of trigger
    #[default]
    Start,
    /// Center with trigger
    Center,
    /// Align to end of trigger
    End,
}

/// Content builder function type for popover content
type ContentBuilderFn = Arc<dyn Fn() -> Div + Send + Sync>;

/// Trigger builder function type for popover trigger
type TriggerBuilderFn = Arc<dyn Fn(bool) -> Div + Send + Sync>;

/// Builder for popover component
pub struct PopoverBuilder {
    /// Trigger builder (receives open state)
    trigger: TriggerBuilderFn,
    /// Content to show in the popover
    content: Option<ContentBuilderFn>,
    /// Side where the popover appears
    side: PopoverSide,
    /// Alignment relative to trigger
    align: PopoverAlign,
    /// Offset from trigger (pixels)
    offset: f32,
    /// Unique instance key
    key: InstanceKey,
    /// User-added CSS classes
    classes: Vec<std::sync::Arc<str>>,
    /// User-set element ID
    user_id: Option<String>,
    /// Built component cache
    built: OnceCell<Popover>,
}

impl std::fmt::Debug for PopoverBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PopoverBuilder")
            .field("side", &self.side)
            .field("align", &self.align)
            .field("offset", &self.offset)
            .finish()
    }
}

impl PopoverBuilder {
    /// Create a new popover builder with a trigger builder function
    ///
    /// The trigger builder receives a boolean indicating if the popover is open.
    #[track_caller]
    pub fn new<F>(trigger_fn: F) -> Self
    where
        F: Fn(bool) -> Div + Send + Sync + 'static,
    {
        Self {
            trigger: Arc::new(trigger_fn),
            content: None,
            side: PopoverSide::Bottom,
            align: PopoverAlign::Start,
            offset: 4.0,
            key: InstanceKey::new("popover"),
            classes: Vec::new(),
            user_id: None,
            built: OnceCell::new(),
        }
    }

    /// Create with a pre-created key
    pub fn with_key<F>(trigger_fn: F, key: InstanceKey) -> Self
    where
        F: Fn(bool) -> Div + Send + Sync + 'static,
    {
        Self {
            trigger: Arc::new(trigger_fn),
            content: None,
            side: PopoverSide::Bottom,
            align: PopoverAlign::Start,
            offset: 4.0,
            key,
            classes: Vec::new(),
            user_id: None,
            built: OnceCell::new(),
        }
    }

    /// Set the content to display in the popover
    pub fn content<F>(mut self, content_fn: F) -> Self
    where
        F: Fn() -> Div + Send + Sync + 'static,
    {
        self.content = Some(Arc::new(content_fn));
        self
    }

    /// Set the side where the popover appears
    pub fn side(mut self, side: PopoverSide) -> Self {
        self.side = side;
        self
    }

    /// Set the alignment relative to the trigger
    pub fn align(mut self, align: PopoverAlign) -> Self {
        self.align = align;
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
    fn get_or_build(&self) -> &Popover {
        self.built.get_or_init(|| self.build_component())
    }

    /// Build the popover component
    fn build_component(&self) -> Popover {
        // Create open state
        let open_state: State<bool> =
            BlincContextState::get().use_state_keyed(self.key.get(), || false);

        // Store overlay handle ID
        let overlay_handle_state: State<Option<u64>> =
            BlincContextState::get().use_state_keyed(&self.key.derive("handle"), || None);

        // Clone values for closures
        let side = self.side;
        let align = self.align;
        let offset = self.offset;
        let content_builder = self.content.clone();
        let trigger_builder = self.trigger.clone();
        let motion_key_str = format!("popover_{}", self.key.get());
        let button_key = self.key.derive("button");

        // Clone states for closures
        let open_state_for_trigger = open_state.clone();
        let open_state_for_click = open_state.clone();
        let overlay_handle_for_click = overlay_handle_state.clone();
        let overlay_handle_for_show = overlay_handle_state.clone();
        let content_builder_for_show = content_builder.clone();

        // Build trigger with click handler
        let trigger = stateful_with_key::<ButtonState>(&button_key)
            .deps([open_state.signal_id()])
            .on_state(move |_ctx| {
                let is_open = open_state_for_trigger.get();

                // Build trigger content
                let trigger_content = (trigger_builder)(is_open);

                div().w_fit().cursor_pointer().child(trigger_content)
            })
            .on_click(move |ctx| {
                // Use bounds directly from EventContext
                let bounds = ElementBounds {
                    x: ctx.bounds_x,
                    y: ctx.bounds_y,
                    width: ctx.bounds_width,
                    height: ctx.bounds_height,
                };

                let is_open = open_state_for_click.get();
                if is_open {
                    // Close the popover
                    if let Some(handle_id) = overlay_handle_for_click.get() {
                        let mgr = get_overlay_manager();
                        let handle = OverlayHandle::from_raw(handle_id);

                        // Check if overlay is already closing or pending close
                        if mgr.is_closing(handle) || mgr.is_pending_close(handle) {
                            return;
                        }

                        mgr.close(handle);
                    }
                } else {
                    // Calculate position based on trigger bounds
                    let (x, y) = calculate_popover_position(&bounds, side, align, offset);

                    // Show the popover
                    if let Some(ref content_fn) = content_builder_for_show {
                        let handle = show_popover_overlay(
                            x,
                            y,
                            side,
                            Arc::clone(content_fn),
                            overlay_handle_for_show.clone(),
                            open_state_for_click.clone(),
                            motion_key_str.clone(),
                        );

                        overlay_handle_for_show.set(Some(handle.id()));
                        open_state_for_click.set(true);
                    }
                }
            });

        let mut inner = trigger;
        for c in &self.classes {
            inner = inner.class(c);
        }
        if let Some(ref id) = self.user_id {
            inner = inner.id(id);
        }
        Popover { inner }
    }
}

/// Calculate popover position based on trigger bounds
///
/// For Top/Left positioning, we position at the trigger edge and let content
/// extend naturally. The offset provides spacing from the trigger.
fn calculate_popover_position(
    bounds: &ElementBounds,
    side: PopoverSide,
    align: PopoverAlign,
    offset: f32,
) -> (f32, f32) {
    match side {
        PopoverSide::Top => {
            // Position just above trigger - with AnchorDirection::Top, y is the BOTTOM edge
            // So y = trigger.y - offset puts the popover bottom just above the trigger top
            let y = bounds.y - bounds.height - offset * 4.0;
            let x = match align {
                PopoverAlign::Start => bounds.x,
                PopoverAlign::Center => bounds.x + bounds.width / 2.0,
                PopoverAlign::End => bounds.x + bounds.width,
            };
            (x.max(0.0), y.max(0.0))
        }
        PopoverSide::Bottom => {
            // Position below trigger - content extends downward
            let y = bounds.y + bounds.height + offset;
            let x = match align {
                PopoverAlign::Start => bounds.x,
                PopoverAlign::Center => bounds.x + bounds.width / 2.0,
                PopoverAlign::End => bounds.x + bounds.width,
            };
            (x.max(0.0), y)
        }
        PopoverSide::Right => {
            // Position to the right of trigger
            let x = bounds.x + bounds.width + offset;
            let y = match align {
                PopoverAlign::Start => bounds.y,
                PopoverAlign::Center => bounds.y + bounds.height / 2.0,
                PopoverAlign::End => bounds.y + bounds.height,
            };
            (x, y.max(0.0))
        }
        PopoverSide::Left => {
            // Position to the left of trigger - content extends leftward
            let x = bounds.x - offset;
            let y = match align {
                PopoverAlign::Start => bounds.y,
                PopoverAlign::Center => bounds.y + bounds.height / 2.0,
                PopoverAlign::End => bounds.y + bounds.height,
            };
            (x.max(0.0), y.max(0.0))
        }
    }
}

/// Show the popover overlay
fn show_popover_overlay(
    x: f32,
    y: f32,
    side: PopoverSide,
    content_fn: ContentBuilderFn,
    overlay_handle_state: State<Option<u64>>,
    open_state: State<bool>,
    motion_key: String,
) -> OverlayHandle {
    let theme = ThemeState::get();
    let bg = theme.color(ColorToken::SurfaceElevated);
    let border = theme.color(ColorToken::Border);
    let radius = theme.radius(RadiusToken::Lg);
    let padding = theme.spacing_value(SpacingToken::Space4);

    let mgr = get_overlay_manager();

    // Clone states for closures
    let handle_state_for_close = overlay_handle_state.clone();
    let handle_state_for_ready = overlay_handle_state.clone();
    let open_state_for_dismiss = open_state.clone();
    let motion_key_for_content = motion_key.clone();

    // Use dropdown() which creates transparent backdrop with dismiss_on_click
    // The motion key includes ":child:0" suffix because animation is on child of wrapper
    let motion_key_with_child = format!("{}:child:0", motion_key);

    // Map PopoverSide to AnchorDirection for correct positioning
    let anchor_dir = match side {
        PopoverSide::Top => AnchorDirection::Top,
        PopoverSide::Bottom => AnchorDirection::Bottom,
        PopoverSide::Left => AnchorDirection::Left,
        PopoverSide::Right => AnchorDirection::Right,
    };

    // Use hover_card() which has no backdrop, allowing scroll events to pass through
    // Disable hover-leave dismiss since popover is click-triggered
    // Enable click-outside dismiss for click-to-close behavior
    // Enable follows_scroll so popover moves with its trigger when scrolling
    // Disable auto-dismiss - popover stays open until user closes it
    mgr.hover_card()
        .at(x, y)
        .anchor_direction(anchor_dir)
        .dismiss_on_hover_leave(false)
        .dismiss_on_click_outside(true) // Dismiss when clicking outside content
        .dismiss_on_escape(true)
        .follows_scroll(true) // Follow scroll to stay attached to trigger
        .auto_dismiss(None) // Stay open indefinitely
        .close_delay(None) // No close delay
        .motion_key(&motion_key_with_child)
        .on_close(move || {
            open_state_for_dismiss.set(false);
            handle_state_for_close.set(None);
        })
        .content(move || {
            let user_content = (content_fn)();

            // Generate unique ID for hit-test size tracking
            let popover_id = format!("popover-{}", motion_key_for_content);

            // Register on_ready callback to capture content size for accurate hit-testing
            let handle_state_for_on_ready = handle_state_for_ready.clone();
            if let Some(handle) = selector::query(&popover_id) {
                handle.on_ready(move |bounds| {
                    if let Some(handle_id) = handle_state_for_on_ready.get() {
                        let mgr = get_overlay_manager();
                        let overlay_handle = OverlayHandle::from_raw(handle_id);
                        mgr.set_content_size(overlay_handle, bounds.width, bounds.height);
                    }
                });
            }

            // Styled popover container. `lock_corner_shape` keeps the
            // panel's corners circular even when a theme advertises a
            // squircle exponent — overlay chrome reads cleanest with
            // the platform-default rounded-rect shape, not the
            // theme's hero curve.
            let popover_content = div()
                .class("cn-popover-content")
                .id(&popover_id)
                .flex_col()
                .bg(bg)
                .border(1.0, border)
                .rounded(radius)
                .lock_corner_shape()
                .p_px(padding)
                .shadow_lg()
                .min_w(150.0)
                .overflow_clip()
                .child(user_content);

            // Wrap in motion for enter/exit animations
            div().w_fit().h_fit().child(
                motion_derived(&motion_key_for_content)
                    .enter_animation(AnimationPreset::grow_in(150))
                    .exit_animation(AnimationPreset::grow_out(100))
                    .child(popover_content),
            )
        })
        .show()
}

/// Built popover component
pub struct Popover {
    inner: blinc_layout::stateful::Stateful<ButtonState>,
}

impl std::fmt::Debug for Popover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Popover").finish()
    }
}

impl ElementBuilder for PopoverBuilder {
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

    fn layout_style(&self) -> Option<&taffy::Style> {
        self.get_or_build().inner.layout_style()
    }

    fn event_handlers(&self) -> Option<&blinc_layout::event_handler::EventHandlers> {
        self.get_or_build().inner.event_handlers()
    }

    fn element_classes(&self) -> &[std::sync::Arc<str>] {
        self.get_or_build().inner.element_classes()
    }

    fn element_id(&self) -> Option<&str> {
        self.get_or_build().inner.element_id()
    }
}

impl ElementBuilder for Popover {
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

    fn layout_style(&self) -> Option<&taffy::Style> {
        self.inner.layout_style()
    }

    fn event_handlers(&self) -> Option<&blinc_layout::event_handler::EventHandlers> {
        self.inner.event_handlers()
    }
}

/// Create a popover component with a trigger
///
/// The trigger builder receives a boolean indicating if the popover is open,
/// allowing the trigger to visually respond to the open state.
///
/// # Example
///
/// ```ignore
/// cn::popover(|open| {
///     cn::button(if open { "Close" } else { "Open" })
/// })
/// .content(|| {
///     div().flex_col().gap(8.0).children([
///         text("Popover Title").size(16.0),
///         text("Some content here."),
///     ])
/// })
/// ```
#[track_caller]
pub fn popover<F>(trigger_fn: F) -> PopoverBuilder
where
    F: Fn(bool) -> Div + Send + Sync + 'static,
{
    PopoverBuilder::new(trigger_fn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_popover_position_bottom() {
        let bounds = ElementBounds {
            x: 100.0,
            y: 50.0,
            width: 80.0,
            height: 32.0,
        };
        let (x, y) =
            calculate_popover_position(&bounds, PopoverSide::Bottom, PopoverAlign::Start, 4.0);
        assert_eq!(x, 100.0);
        assert_eq!(y, 86.0); // 50 + 32 + 4
    }

    #[test]
    fn test_popover_position_right() {
        let bounds = ElementBounds {
            x: 100.0,
            y: 50.0,
            width: 80.0,
            height: 32.0,
        };
        let (x, y) =
            calculate_popover_position(&bounds, PopoverSide::Right, PopoverAlign::Start, 8.0);
        assert_eq!(x, 188.0); // 100 + 80 + 8
        assert_eq!(y, 50.0);
    }

    #[test]
    fn test_popover_position_top() {
        let bounds = ElementBounds {
            x: 100.0,
            y: 100.0,
            width: 80.0,
            height: 32.0,
        };
        let (x, y) =
            calculate_popover_position(&bounds, PopoverSide::Top, PopoverAlign::Start, 4.0);
        assert_eq!(x, 100.0);
        // y = trigger.y - trigger.height - offset * 4.0 = 100 - 32 - 16 = 52
        // This is where the popover's BOTTOM edge will be (via anchor_direction)
        assert_eq!(y, 52.0);
    }

    #[test]
    fn test_popover_position_center_align() {
        let bounds = ElementBounds {
            x: 100.0,
            y: 50.0,
            width: 80.0,
            height: 32.0,
        };
        let (x, _y) =
            calculate_popover_position(&bounds, PopoverSide::Bottom, PopoverAlign::Center, 4.0);
        // x = 100 + 80/2 = 140 (center of trigger)
        assert_eq!(x, 140.0);
    }
}

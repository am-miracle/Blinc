//! Toggle — a button-shaped binary on/off control.
//!
//! Visually a button that stays "pressed in" while `on`. Distinct from
//! [`switch`](super::super::super::widgets) (sliding track + thumb) and
//! [`checkbox`](super::checkbox) (small box with checkmark). The
//! canonical use is icon-only formatting toggles in a toolbar — bold /
//! italic / underline — or a single-key feature flag in settings.
//!
//! Defaults to theme tokens for every visual value (colours from
//! [`ColorToken`], radius from [`RadiusToken`], spacing from
//! [`SpacingToken`], typography from [`TypographyTokens`]) so a bare
//! `toggle(&state)` against any active theme bundle looks right out
//! of the box. `blinc_cn::toggle` layers shadcn-flavoured surface
//! styling on top via `.cn-toggle` classes.
//!
//! # Example
//!
//! ```ignore
//! let bold_on = ctx.use_state_keyed("bold", || false);
//! toggle(&bold_on)
//!     .label("B")
//!     .on_change(|on| println!("bold: {}", on))
//! ```

use std::sync::Arc;

use blinc_core::{Color, State};
use blinc_theme::{ColorToken, RadiusToken, SpacingToken, ThemeState};

use crate::css_parser::{ElementState, Stylesheet, active_stylesheet};
use crate::div::{ElementBuilder, div};
use crate::element::RenderProps;
use crate::element_style::ElementStyle;
use crate::key::InstanceKey;
use crate::stateful::{ButtonState, stateful_with_key};
use crate::svg::svg;
use crate::text::text;
use crate::tree::{LayoutNodeId, LayoutTree};

/// Toggle configuration.
///
/// Colour fields are `Option<Color>` — when `None`, defaults resolve
/// fresh from the theme inside the Stateful callback so theme swaps
/// re-paint without rebuilding.
#[derive(Clone)]
pub struct ToggleConfig {
    /// Reactive on/off state.
    on: State<bool>,
    /// Whether the toggle is disabled.
    pub disabled: bool,
    /// Optional text label.
    pub label: Option<String>,
    /// Optional SVG icon markup (rendered before the label).
    pub icon: Option<String>,
    /// Element height in logical px.
    pub height: f32,
    /// Horizontal padding (each side).
    pub padding_x: f32,
    /// Gap between icon and label.
    pub gap: f32,
    /// Icon size in logical px (defaults to ~0.5 × height).
    pub icon_size: Option<f32>,
    /// Label font size in logical px.
    pub label_font_size: Option<f32>,
    /// Corner radius (None = `RadiusToken::Default`).
    pub corner_radius: Option<f32>,
    /// Border width when `on` or in outline mode.
    pub border_width: f32,
    /// Whether the idle (off) state shows a border.
    pub bordered_off: bool,
    /// Background when off (None = transparent).
    pub off_bg: Option<Color>,
    /// Background when on (None = theme `Secondary`).
    pub on_bg: Option<Color>,
    /// Text/icon colour when off (None = theme `TextSecondary`).
    pub off_fg: Option<Color>,
    /// Text/icon colour when on (None = theme `TextPrimary`).
    pub on_fg: Option<Color>,
    /// Border colour (None = theme `BorderSecondary`).
    pub border_color: Option<Color>,
    /// Disabled opacity multiplier.
    pub disabled_opacity: f32,
    /// CSS element ID for stylesheet matching.
    pub css_element_id: Option<String>,
    /// CSS class names.
    pub css_classes: Vec<Arc<str>>,
    /// Change handler.
    pub on_change: Option<Arc<dyn Fn(bool) + Send + Sync>>,
}

impl ToggleConfig {
    fn new(on: State<bool>) -> Self {
        Self {
            on,
            disabled: false,
            label: None,
            icon: None,
            height: 36.0,
            padding_x: 12.0,
            gap: 6.0,
            icon_size: None,
            label_font_size: None,
            corner_radius: None,
            border_width: 1.0,
            bordered_off: false,
            off_bg: None,
            on_bg: None,
            off_fg: None,
            on_fg: None,
            border_color: None,
            disabled_opacity: 0.5,
            css_element_id: None,
            css_classes: Vec::new(),
            on_change: None,
        }
    }
}

/// Theme-resolved colours, with hover-tint variants applied at paint time.
struct ResolvedColors {
    off_bg: Color,
    on_bg: Color,
    off_fg: Color,
    on_fg: Color,
    border_color: Color,
}

impl ResolvedColors {
    fn from_config(config: &ToggleConfig, theme: &ThemeState) -> Self {
        // Off: transparent surface so the toggle reads as a ghost button
        //      affordance (shadcn's `<Toggle>` baseline). The label/icon
        //      itself carries the affordance — text colour is `TextPrimary`
        //      (not Secondary) so it's clearly readable at rest.
        // On:  a SUBTLE accent fill (8 % of the active foreground over
        //      the surface), matching shadcn's `bg-accent`. Avoids the
        //      "solid dark button" misread the previous `Secondary` token
        //      produced — the on-state needs to feel like an emphasis, not
        //      a different button entirely.
        let on_bg_default = mix(
            theme.color(ColorToken::Background),
            theme.color(ColorToken::TextPrimary),
            0.10,
        );
        Self {
            off_bg: config.off_bg.unwrap_or(Color::TRANSPARENT),
            on_bg: config.on_bg.unwrap_or(on_bg_default),
            off_fg: config
                .off_fg
                .unwrap_or_else(|| theme.color(ColorToken::TextPrimary)),
            on_fg: config
                .on_fg
                .unwrap_or_else(|| theme.color(ColorToken::TextPrimary)),
            border_color: config
                .border_color
                .unwrap_or_else(|| theme.color(ColorToken::BorderSecondary)),
        }
    }
}

/// Mix `top` over `bottom` by `amount` (a poor-man's overlay blend
/// good enough for the small hover-tint shift the toggle uses).
fn mix(bottom: Color, top: Color, amount: f32) -> Color {
    let a = amount.clamp(0.0, 1.0);
    Color::rgba(
        bottom.r * (1.0 - a) + top.r * a,
        bottom.g * (1.0 - a) + top.g * a,
        bottom.b * (1.0 - a) + top.b * a,
        bottom.a.max(top.a),
    )
}

/// CSS-override pipeline: layered base → :checked → :hover → :disabled,
/// matching the precedence the `cn::toggle` stylesheet expects. `:checked`
/// is what shadcn / aria use for the on-state, so toggle reuses
/// `ElementState::Checked` rather than introducing a new pseudo-state.
#[allow(clippy::too_many_arguments)]
fn apply_css_overrides(
    colors: &mut ResolvedColors,
    cfg: &mut ToggleConfig,
    stylesheet: &Stylesheet,
    element_id: Option<&str>,
    css_classes: &[Arc<str>],
    is_on: bool,
    is_hovered: bool,
    is_disabled: bool,
) {
    // 1. Class-based base + :state styles (lower priority than id-based).
    for class in css_classes {
        if let Some(base) = stylesheet.get_class(class) {
            apply_style(colors, cfg, base, is_on);
        }
        if is_on {
            if let Some(s) = stylesheet.get_class_with_state(class, ElementState::Checked) {
                apply_style(colors, cfg, s, is_on);
            }
        }
        if is_hovered {
            if let Some(s) = stylesheet.get_class_with_state(class, ElementState::Hover) {
                apply_style(colors, cfg, s, is_on);
            }
        }
        if is_disabled {
            if let Some(s) = stylesheet.get_class_with_state(class, ElementState::Disabled) {
                apply_style(colors, cfg, s, is_on);
            }
        }
    }
    // 2. ID-based overrides win over class.
    if let Some(element_id) = element_id {
        if let Some(base) = stylesheet.get(element_id) {
            apply_style(colors, cfg, base, is_on);
        }
        if is_on {
            if let Some(s) = stylesheet.get_with_state(element_id, ElementState::Checked) {
                apply_style(colors, cfg, s, is_on);
            }
        }
        if is_hovered {
            if let Some(s) = stylesheet.get_with_state(element_id, ElementState::Hover) {
                apply_style(colors, cfg, s, is_on);
            }
        }
        if is_disabled {
            if let Some(s) = stylesheet.get_with_state(element_id, ElementState::Disabled) {
                apply_style(colors, cfg, s, is_on);
            }
        }
    }
}

fn apply_style(
    colors: &mut ResolvedColors,
    cfg: &mut ToggleConfig,
    style: &ElementStyle,
    is_on: bool,
) {
    if let Some(blinc_core::Brush::Solid(color)) = style.background {
        if is_on {
            colors.on_bg = color;
        } else {
            colors.off_bg = color;
        }
    }
    if let Some(color) = style.border_color {
        colors.border_color = color;
    }
    if let Some(w) = style.border_width {
        cfg.border_width = w;
    }
    if let Some(cr) = style.corner_radius {
        cfg.corner_radius = Some(cr.top_left);
    }
    if let Some(opacity) = style.opacity {
        cfg.disabled_opacity = opacity;
    }
    if let Some(color) = style.text_color {
        if is_on {
            colors.on_fg = color;
        } else {
            colors.off_fg = color;
        }
    }
    if let Some(size) = style.font_size {
        cfg.label_font_size = Some(size);
    }
}

/// The fully-built Toggle (a Stateful Div).
pub struct Toggle {
    inner: crate::div::Div,
}

impl Toggle {
    fn with_config(instance_key: &InstanceKey, config: ToggleConfig) -> Self {
        let on_state = config.on.clone();
        let on_state_for_click = config.on.clone();
        let on_change = config.on_change.clone();
        let disabled = config.disabled;
        let css_element_id = config.css_element_id.clone();
        let css_classes = config.css_classes.clone();

        let key = instance_key.get().to_string();

        let mut toggle_el = stateful_with_key::<ButtonState>(&key)
            .deps([on_state.signal_id()])
            .on_state(move |ctx| {
                let button_state = ctx.state();
                let is_hovered =
                    matches!(button_state, ButtonState::Hovered | ButtonState::Pressed);
                let is_pressed = matches!(button_state, ButtonState::Pressed);
                let is_on = on_state.get();
                let is_disabled = config.disabled;

                let theme = ThemeState::get();
                let mut colors = ResolvedColors::from_config(&config, theme);
                let mut cfg = config.clone();

                if let Some(stylesheet) = active_stylesheet() {
                    apply_css_overrides(
                        &mut colors,
                        &mut cfg,
                        &stylesheet,
                        css_element_id.as_deref(),
                        &css_classes,
                        is_on,
                        is_hovered,
                        is_disabled,
                    );
                }

                // Resolve token defaults that weren't pinned by config.
                let radius = cfg
                    .corner_radius
                    .unwrap_or_else(|| theme.radius(RadiusToken::Default));
                let icon_size = cfg.icon_size.unwrap_or(cfg.height * 0.5);
                let font_size = cfg.label_font_size.unwrap_or(theme.typography().text_sm);

                // Background:
                //   off  → transparent (or off_bg) + faint hover wash
                //   on   → on_bg, mildly darkened on hover for affordance
                //   pressed → mild darken regardless of on/off so the click
                //             registers visually before the bool flips.
                let bg = if is_on {
                    if is_pressed && !is_disabled {
                        mix(colors.on_bg, theme.color(ColorToken::TextPrimary), 0.08)
                    } else if is_hovered && !is_disabled {
                        mix(colors.on_bg, theme.color(ColorToken::TextPrimary), 0.04)
                    } else {
                        colors.on_bg
                    }
                } else if is_pressed && !is_disabled {
                    mix(colors.off_bg, theme.color(ColorToken::TextPrimary), 0.10)
                } else if is_hovered && !is_disabled {
                    mix(colors.off_bg, theme.color(ColorToken::TextPrimary), 0.05)
                } else {
                    colors.off_bg
                };

                let fg = if is_on { colors.on_fg } else { colors.off_fg };

                let mut body = div()
                    .h(cfg.height)
                    .padding_x(crate::units::px(cfg.padding_x))
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .gap(cfg.gap)
                    .bg(bg)
                    .rounded(radius);

                // Border: only the outline variant (`bordered_off=true`)
                // draws a border, and it draws in both off and on states.
                // The default variant relies on the icon/label + bg-accent
                // overlay to communicate state (matching shadcn's
                // `<Toggle>`); adding a border-on-on for the default would
                // make it look like the outline variant.
                if cfg.bordered_off && cfg.border_width > 0.0 {
                    body = body.border(cfg.border_width, colors.border_color);
                }

                if let Some(ref icon_svg) = cfg.icon {
                    body = body.child(
                        svg(icon_svg)
                            .size(icon_size, icon_size)
                            .color(fg)
                            .internal(),
                    );
                }
                if let Some(ref label_text) = cfg.label {
                    body = body.child(text(label_text).size(font_size).color(fg));
                }

                if is_disabled {
                    body = body.opacity(cfg.disabled_opacity);
                }

                body
            });

        toggle_el = toggle_el.on_click(move |_| {
            if disabled {
                return;
            }
            let next = !on_state_for_click.get();
            on_state_for_click.set(next);
            if let Some(ref handler) = on_change {
                handler(next);
            }
        });

        // Wrap in a slim container so the cursor + classes attach
        // cleanly regardless of the inner Stateful.
        let theme = ThemeState::get();
        let inner = div()
            .h_fit()
            .w_fit()
            .gap(theme.spacing_value(SpacingToken::Space2))
            .cursor_pointer()
            .child(toggle_el);

        Self { inner }
    }
}

impl ElementBuilder for Toggle {
    fn build(&self, tree: &mut LayoutTree) -> LayoutNodeId {
        self.inner.build(tree)
    }

    fn render_props(&self) -> RenderProps {
        self.inner.render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        self.inner.children_builders()
    }

    fn element_type_id(&self) -> crate::div::ElementTypeId {
        self.inner.element_type_id()
    }

    fn semantic_type_name(&self) -> Option<&'static str> {
        Some("toggle")
    }

    fn layout_style(&self) -> Option<&taffy::Style> {
        self.inner.layout_style()
    }

    fn event_handlers(&self) -> Option<&crate::event_handler::EventHandlers> {
        Some(self.inner.event_handlers())
    }

    fn element_id(&self) -> Option<&str> {
        self.inner.element_id()
    }

    fn element_classes(&self) -> &[Arc<str>] {
        self.inner.element_classes()
    }
}

/// Lazy builder for [`Toggle`]. Config accumulates via fluent methods,
/// the Toggle itself materialises on first `ElementBuilder` access.
pub struct ToggleBuilder {
    key: InstanceKey,
    config: ToggleConfig,
    built: std::cell::OnceCell<Toggle>,
}

impl ToggleBuilder {
    #[track_caller]
    pub fn new(on: &State<bool>) -> Self {
        Self {
            key: InstanceKey::new("toggle"),
            config: ToggleConfig::new(on.clone()),
            built: std::cell::OnceCell::new(),
        }
    }

    fn get_or_build(&self) -> &Toggle {
        self.built
            .get_or_init(|| Toggle::with_config(&self.key, self.config.clone()))
    }

    pub fn id(mut self, id: &str) -> Self {
        self.config.css_element_id = Some(id.to_string());
        self
    }

    pub fn class(mut self, name: impl AsRef<str>) -> Self {
        self.config
            .css_classes
            .push(blinc_core::intern::intern(name.as_ref()));
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.config.label = Some(label.into());
        self
    }

    pub fn icon(mut self, svg_markup: impl Into<String>) -> Self {
        self.config.icon = Some(svg_markup.into());
        self
    }

    pub fn height(mut self, h: f32) -> Self {
        self.config.height = h;
        self
    }

    pub fn padding_x(mut self, px: f32) -> Self {
        self.config.padding_x = px;
        self
    }

    pub fn icon_size(mut self, px: f32) -> Self {
        self.config.icon_size = Some(px);
        self
    }

    pub fn label_font_size(mut self, px: f32) -> Self {
        self.config.label_font_size = Some(px);
        self
    }

    pub fn rounded(mut self, radius: f32) -> Self {
        self.config.corner_radius = Some(radius);
        self
    }

    pub fn border_width(mut self, w: f32) -> Self {
        self.config.border_width = w;
        self
    }

    /// Show a border in the off (idle) state. Off by default — outline
    /// variants in cn opt in for the shadcn-style boxed look.
    pub fn bordered_off(mut self, yes: bool) -> Self {
        self.config.bordered_off = yes;
        self
    }

    pub fn off_bg(mut self, color: impl Into<Color>) -> Self {
        self.config.off_bg = Some(color.into());
        self
    }

    pub fn on_bg(mut self, color: impl Into<Color>) -> Self {
        self.config.on_bg = Some(color.into());
        self
    }

    pub fn off_fg(mut self, color: impl Into<Color>) -> Self {
        self.config.off_fg = Some(color.into());
        self
    }

    pub fn on_fg(mut self, color: impl Into<Color>) -> Self {
        self.config.on_fg = Some(color.into());
        self
    }

    pub fn border_color(mut self, color: impl Into<Color>) -> Self {
        self.config.border_color = Some(color.into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.config.disabled = disabled;
        self
    }

    pub fn on_change<F>(mut self, handler: F) -> Self
    where
        F: Fn(bool) + Send + Sync + 'static,
    {
        self.config.on_change = Some(Arc::new(handler));
        self
    }
}

impl ElementBuilder for ToggleBuilder {
    fn build(&self, tree: &mut LayoutTree) -> LayoutNodeId {
        self.get_or_build().build(tree)
    }

    fn render_props(&self) -> RenderProps {
        self.get_or_build().render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        self.get_or_build().children_builders()
    }

    fn element_type_id(&self) -> crate::div::ElementTypeId {
        self.get_or_build().element_type_id()
    }

    fn semantic_type_name(&self) -> Option<&'static str> {
        Some("toggle")
    }

    fn layout_style(&self) -> Option<&taffy::Style> {
        self.get_or_build().layout_style()
    }

    fn event_handlers(&self) -> Option<&crate::event_handler::EventHandlers> {
        self.get_or_build().event_handlers()
    }

    fn element_id(&self) -> Option<&str> {
        self.get_or_build().element_id()
    }

    fn element_classes(&self) -> &[Arc<str>] {
        self.get_or_build().element_classes()
    }
}

/// Create a toggle bound to a reactive `State<bool>`.
///
/// ```ignore
/// let bold = ctx.use_state_keyed("bold", || false);
/// toggle(&bold).label("B").on_change(|on| /* … */)
/// ```
#[track_caller]
pub fn toggle(on: &State<bool>) -> ToggleBuilder {
    ToggleBuilder::new(on)
}

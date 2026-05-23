//! NumberInput — themed wrapper around [`blinc_layout::widgets::number_input`](mod@blinc_layout::widgets::number_input).
//!
//! Adds the cn surface (`.cn-input`-shaped focus ring, hover bg, the
//! `.cn-number-input` class hook for downstream override) plus a `+` /
//! `−` stepper pair flanking the field. All parse / clamp / format work
//! lives in the layout widget; the cn layer contributes look + the
//! stepper affordance.
//!
//! # Example
//!
//! ```ignore
//! use blinc_cn::prelude::*;
//!
//! fn build_ui(ctx: &WindowedContext) -> impl ElementBuilder {
//!     let qty = ctx.use_state_keyed("qty", || 1.0);
//!     cn::number_input(&qty).min(0.0).max(99.0).step(1.0).precision(0)
//! }
//! ```

use blinc_core::State;
use blinc_layout::div::ElementBuilder;
use blinc_layout::prelude::*;
use blinc_layout::tree::{LayoutNodeId, LayoutTree};
use blinc_layout::widgets::number_input as layout_number_input;
use blinc_theme::{ColorToken, RadiusToken, ThemeState};
use std::sync::Arc;

use crate::components::input::InputSize;

/// Internal config for the cn number input.
#[allow(clippy::type_complexity)]
struct Config {
    state: State<f64>,
    min: Option<f64>,
    max: Option<f64>,
    step: f64,
    precision: usize,
    size: InputSize,
    placeholder: Option<String>,
    disabled: bool,
    width: Option<f32>,
    on_change: Option<Arc<dyn Fn(f64) + Send + Sync>>,
}

/// Lazy builder for the cn number input.
pub struct NumberInputBuilder {
    config: Config,
    built: std::cell::OnceCell<NumberInput>,
}

pub struct NumberInput {
    inner: Div,
}

impl NumberInputBuilder {
    #[track_caller]
    pub fn new(state: &State<f64>) -> Self {
        Self {
            config: Config {
                state: state.clone(),
                min: None,
                max: None,
                step: 1.0,
                precision: 2,
                size: InputSize::Medium,
                placeholder: None,
                disabled: false,
                width: None,
                on_change: None,
            },
            built: std::cell::OnceCell::new(),
        }
    }

    fn get_or_build(&self) -> &NumberInput {
        self.built
            .get_or_init(|| NumberInput::from_config(self.clone_config()))
    }

    fn clone_config(&self) -> Config {
        Config {
            state: self.config.state.clone(),
            min: self.config.min,
            max: self.config.max,
            step: self.config.step,
            precision: self.config.precision,
            size: self.config.size,
            placeholder: self.config.placeholder.clone(),
            disabled: self.config.disabled,
            width: self.config.width,
            on_change: self.config.on_change.clone(),
        }
    }

    pub fn min(mut self, min: f64) -> Self {
        self.config.min = Some(min);
        self
    }

    pub fn max(mut self, max: f64) -> Self {
        self.config.max = Some(max);
        self
    }

    pub fn step(mut self, step: f64) -> Self {
        self.config.step = step;
        self
    }

    pub fn precision(mut self, precision: usize) -> Self {
        self.config.precision = precision;
        self
    }

    pub fn size(mut self, size: InputSize) -> Self {
        self.config.size = size;
        self
    }

    pub fn placeholder(mut self, text: impl Into<String>) -> Self {
        self.config.placeholder = Some(text.into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.config.disabled = disabled;
        self
    }

    pub fn w(mut self, px: f32) -> Self {
        self.config.width = Some(px);
        self
    }

    pub fn on_change<F>(mut self, handler: F) -> Self
    where
        F: Fn(f64) + Send + Sync + 'static,
    {
        self.config.on_change = Some(Arc::new(handler));
        self
    }
}

impl NumberInput {
    fn from_config(config: Config) -> Self {
        let theme = ThemeState::get();
        let typography = theme.typography();
        let height = config.size.height(theme);
        let font_size = config.size.font_size(&typography);
        let radius = theme.radius(RadiusToken::Default);
        let border_color = theme.color(ColorToken::BorderSecondary);
        let text_secondary = theme.color(ColorToken::TextSecondary);

        // Stepper button width tuned so two buttons + the field share
        // the row cleanly. Roughly `height` so the buttons read as
        // square-ish at the matching size.
        let button_w = height;
        let total_w = config.width.unwrap_or(140.0);
        let field_w = (total_w - button_w * 2.0).max(40.0);

        // Layout-level number input does the parse / clamp / format
        // pipeline. We hand it the field width + sizing only — the
        // class hooks below (`.cn-number-input`, `.cn-input`) let cn
        // styling cascade through `text_input`'s existing class-based
        // overrides.
        let mut field = layout_number_input::number_input(&config.state)
            .min(f64::NEG_INFINITY)
            .max(f64::INFINITY)
            .step(config.step)
            .precision(config.precision)
            .w(field_w)
            .h(height)
            .class("cn-number-input")
            .class("cn-input")
            .disabled(config.disabled);

        if let Some(min) = config.min {
            field = field.min(min);
        }
        if let Some(max) = config.max {
            field = field.max(max);
        }
        if let Some(ref placeholder) = config.placeholder {
            field = field.placeholder(placeholder.clone());
        }
        if let Some(ref cb) = config.on_change {
            let cb = cb.clone();
            field = field.on_change(move |v| cb(v));
        }

        // `+` / `−` buttons — read state, clamp, set. Same logic as
        // `layout_number_input::step_up` / `step_down` but inlined so
        // we don't need to expose the layout config publicly here.
        let make_stepper = |label: &'static str, delta: f64| {
            let state = config.state.clone();
            let min = config.min;
            let max = config.max;
            let step = config.step;
            let precision = config.precision;
            let disabled = config.disabled;
            let on_change = config.on_change.clone();

            div()
                .w(button_w)
                .h(height)
                .flex_row()
                .items_center()
                .justify_center()
                .bg(theme.color(ColorToken::Background))
                .border(1.0, border_color)
                .cursor_pointer()
                .child(text(label).size(font_size).color(text_secondary))
                .on_click(move |_| {
                    if disabled {
                        return;
                    }
                    let next = clamp(state.get() + delta * step, min, max);
                    let _ = precision; // formatting handled by the layout widget on next paint
                    state.set(next);
                    if let Some(ref cb) = on_change {
                        cb(next);
                    }
                })
        };

        // Container shapes a single rounded rectangle out of the three
        // children. Outer rounding via `overflow_clip` + `rounded` so
        // the leftmost / rightmost buttons inherit the radius corners.
        let inner = div()
            .w(total_w)
            .h(height)
            .flex_row()
            .items_center()
            .rounded(radius)
            .overflow_clip()
            .class("cn-number-input-group")
            .child(make_stepper("−", -1.0))
            .child(field)
            .child(make_stepper("+", 1.0));

        Self { inner }
    }
}

fn clamp(value: f64, min: Option<f64>, max: Option<f64>) -> f64 {
    let v = if let Some(lo) = min {
        value.max(lo)
    } else {
        value
    };
    if let Some(hi) = max { v.min(hi) } else { v }
}

impl ElementBuilder for NumberInput {
    fn build(&self, tree: &mut LayoutTree) -> LayoutNodeId {
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

    fn semantic_type_name(&self) -> Option<&'static str> {
        Some("number_input")
    }

    fn layout_style(&self) -> Option<&taffy::Style> {
        self.inner.layout_style()
    }

    fn event_handlers(&self) -> Option<&blinc_layout::event_handler::EventHandlers> {
        ElementBuilder::event_handlers(&self.inner)
    }

    fn element_id(&self) -> Option<&str> {
        self.inner.element_id()
    }

    fn element_classes(&self) -> &[Arc<str>] {
        self.inner.element_classes()
    }
}

impl ElementBuilder for NumberInputBuilder {
    fn build(&self, tree: &mut LayoutTree) -> LayoutNodeId {
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

    fn semantic_type_name(&self) -> Option<&'static str> {
        Some("number_input")
    }

    fn layout_style(&self) -> Option<&taffy::Style> {
        self.get_or_build().layout_style()
    }

    fn event_handlers(&self) -> Option<&blinc_layout::event_handler::EventHandlers> {
        self.get_or_build().event_handlers()
    }

    fn element_id(&self) -> Option<&str> {
        self.get_or_build().element_id()
    }

    fn element_classes(&self) -> &[Arc<str>] {
        self.get_or_build().element_classes()
    }
}

/// Create a cn-styled number input bound to a [`State<f64>`].
#[track_caller]
pub fn number_input(state: &State<f64>) -> NumberInputBuilder {
    NumberInputBuilder::new(state)
}

//! Shared CSS override resolution for stateful blinc_cn components.
//!
//! Components that use `Stateful` with `on_state` must manually resolve CSS
//! class styles because their visual tree is rebuilt inside callbacks.
//! This module provides a shared helper for that resolution.

use blinc_core::{Brush, Color};
use blinc_layout::css_parser::{ElementState, Stylesheet, active_stylesheet};
use blinc_layout::element_style::ElementStyle;

/// Resolved CSS overrides for a component.
///
/// Each field is `Some` only if the CSS stylesheet defines it.
/// Components should use these as overrides on top of their theme defaults.
#[derive(Clone, Debug, Default)]
pub struct CnStyleOverrides {
    pub background: Option<Color>,
    pub border_color: Option<Color>,
    pub text_color: Option<Color>,
    pub accent_color: Option<Color>,
    pub opacity: Option<f32>,
    pub corner_radius: Option<f32>,
}

impl CnStyleOverrides {
    /// Apply a single `ElementStyle` layer onto these overrides.
    ///
    /// Later calls take precedence (cascade order).
    pub fn apply(&mut self, style: &ElementStyle) {
        if let Some(Brush::Solid(color)) = style.background {
            self.background = Some(color);
        }
        if let Some(color) = style.border_color {
            self.border_color = Some(color);
        }
        if let Some(color) = style.text_color {
            self.text_color = Some(color);
        }
        if let Some(color) = style.accent_color {
            self.accent_color = Some(color);
        }
        if let Some(o) = style.opacity {
            self.opacity = Some(o);
        }
        if let Some(cr) = style.corner_radius {
            self.corner_radius = Some(cr.top_left);
        }
    }
}

/// Resolve CSS class overrides for a component, cascading state pseudo-classes.
///
/// Cascade order (lowest → highest priority):
/// 1. Base class (`.cn-button--primary`)
/// 2. `:hover` (if hovered)
/// 3. `:active` (if pressed)
/// 4. Checked class (`.cn-checkbox--checked`, if checked)
/// 5. Disabled class (`.cn-button--disabled`, if disabled)
///
/// # Arguments
///
/// * `class_name` — The primary CSS class (e.g., `"cn-button--primary"`)
/// * `is_hovered` — Whether the element is currently hovered
/// * `is_pressed` — Whether the element is currently pressed/active
/// * `checked_class` — Optional class to apply when checked (e.g., `"cn-checkbox--checked"`)
/// * `disabled_class` — Optional class to apply when disabled (e.g., `"cn-button--disabled"`)
pub fn resolve_class_overrides(
    class_name: &str,
    is_hovered: bool,
    is_pressed: bool,
    checked_class: Option<&str>,
    disabled_class: Option<&str>,
) -> CnStyleOverrides {
    let mut overrides = CnStyleOverrides::default();

    let stylesheet = match active_stylesheet() {
        Some(s) => s,
        None => return overrides,
    };

    // 1. Base class
    if let Some(style) = stylesheet.get_class(class_name) {
        overrides.apply(style);
    }

    // 2. :hover
    if is_hovered {
        if let Some(style) = stylesheet.get_class_with_state(class_name, ElementState::Hover) {
            overrides.apply(style);
        }
    }

    // 3. :active
    if is_pressed {
        if let Some(style) = stylesheet.get_class_with_state(class_name, ElementState::Active) {
            overrides.apply(style);
        }
    }

    // 4. Checked class (separate class, not a pseudo-class on base)
    if let Some(checked) = checked_class {
        if let Some(style) = stylesheet.get_class(checked) {
            overrides.apply(style);
        }
    }

    // 5. Disabled class (separate class, not a pseudo-class on base)
    if let Some(disabled) = disabled_class {
        if let Some(style) = stylesheet.get_class(disabled) {
            overrides.apply(style);
        }
    }

    overrides
}

/// Resolve CSS overrides for multiple classes, merging them in order.
///
/// Useful for components with multiple classes (e.g., `.cn-button .cn-button--primary .cn-button--sm`).
/// Each class is resolved independently and merged left-to-right.
pub fn resolve_multi_class_overrides(
    classes: &[&str],
    is_hovered: bool,
    is_pressed: bool,
    extra_classes: &[&str],
) -> CnStyleOverrides {
    let mut overrides = CnStyleOverrides::default();

    let stylesheet = match active_stylesheet() {
        Some(s) => s,
        None => return overrides,
    };

    // Apply base + state for each class
    for class in classes {
        if let Some(style) = stylesheet.get_class(class) {
            overrides.apply(style);
        }
        if is_hovered {
            if let Some(style) = stylesheet.get_class_with_state(class, ElementState::Hover) {
                overrides.apply(style);
            }
        }
        if is_pressed {
            if let Some(style) = stylesheet.get_class_with_state(class, ElementState::Active) {
                overrides.apply(style);
            }
        }
    }

    // Apply extra classes (checked, disabled, etc.)
    for class in extra_classes {
        if let Some(style) = stylesheet.get_class(class) {
            overrides.apply(style);
        }
    }

    overrides
}

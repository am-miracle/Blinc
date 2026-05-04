//! Badge component for status indicators
//!
//! Small labeled indicators for status, counts, or categories.
//!
//! # Example
//!
//! ```ignore
//! use blinc_cn::prelude::*;
//!
//! // Default badge
//! cn::badge("New")
//!
//! // Variant badges
//! cn::badge("Success").variant(BadgeVariant::Success)
//! cn::badge("Warning").variant(BadgeVariant::Warning)
//! cn::badge("Error").variant(BadgeVariant::Destructive)
//!
//! // Outline badge
//! cn::badge("Draft").variant(BadgeVariant::Outline)
//! ```

use std::ops::{Deref, DerefMut};

use blinc_layout::div::{Div, ElementBuilder, ElementTypeId};
use blinc_layout::prelude::*;
use blinc_theme::ThemeState;

/// Badge visual variants
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BadgeVariant {
    /// Default badge - primary color
    #[default]
    Default,
    /// Secondary badge - muted
    Secondary,
    /// Success badge - green
    Success,
    /// Warning badge - yellow/orange
    Warning,
    /// Destructive badge - red
    Destructive,
    /// Outline badge - border only
    Outline,
}

// All variant visuals (background, color, border) defined in CSS:
// .cn-badge--default, .cn-badge--secondary, .cn-badge--success, etc.

/// Badge component for status indicators
///
/// Implements `Deref` to `Div` for full customization.
pub struct Badge {
    inner: Div,
    label: String,
    variant: BadgeVariant,
}

impl Badge {
    /// Create a new badge with text
    pub fn new(label: impl Into<String>) -> Self {
        Self::with_variant(label, BadgeVariant::default())
    }

    fn with_variant(label: impl Into<String>, variant: BadgeVariant) -> Self {
        let label = label.into();

        let variant_class = match variant {
            BadgeVariant::Default => "cn-badge--default",
            BadgeVariant::Secondary => "cn-badge--secondary",
            BadgeVariant::Success => "cn-badge--success",
            BadgeVariant::Warning => "cn-badge--warning",
            BadgeVariant::Destructive => "cn-badge--destructive",
            BadgeVariant::Outline => "cn-badge--outline",
        };

        // All visual props from CSS: .cn-badge + .cn-badge--{variant}
        let badge = div()
            .class("cn-badge")
            .class(variant_class)
            .items_center()
            .justify_center()
            .child(text(&label).medium());

        Self {
            inner: badge,
            label,
            variant,
        }
    }

    /// Set the badge variant
    pub fn variant(self, variant: BadgeVariant) -> Self {
        Self::with_variant(self.label, variant)
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

impl Deref for Badge {
    type Target = Div;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Badge {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl ElementBuilder for Badge {
    fn build(&self, tree: &mut blinc_layout::tree::LayoutTree) -> blinc_layout::tree::LayoutNodeId {
        self.inner.build(tree)
    }

    fn render_props(&self) -> blinc_layout::element::RenderProps {
        self.inner.render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        self.inner.children_builders()
    }

    fn event_handlers(&self) -> Option<&blinc_layout::event_handler::EventHandlers> {
        ElementBuilder::event_handlers(&self.inner)
    }

    fn layout_style(&self) -> Option<&taffy::Style> {
        ElementBuilder::layout_style(&self.inner)
    }

    fn element_type_id(&self) -> ElementTypeId {
        ElementBuilder::element_type_id(&self.inner)
    }

    fn element_classes(&self) -> &[std::sync::Arc<str>] {
        self.inner.element_classes()
    }
}

/// Create a badge with text
///
/// # Example
///
/// ```ignore
/// use blinc_cn::prelude::*;
///
/// cn::badge("New")
///     .variant(BadgeVariant::Success)
/// ```
pub fn badge(label: impl Into<String>) -> Badge {
    Badge::new(label)
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
    fn test_badge_default() {
        init_theme();
        let _ = badge("Test");
    }

    #[test]
    fn test_badge_variants() {
        init_theme();
        let _ = badge("Default").variant(BadgeVariant::Default);
        let _ = badge("Secondary").variant(BadgeVariant::Secondary);
        let _ = badge("Success").variant(BadgeVariant::Success);
        let _ = badge("Warning").variant(BadgeVariant::Warning);
        let _ = badge("Error").variant(BadgeVariant::Destructive);
        let _ = badge("Outline").variant(BadgeVariant::Outline);
    }
}

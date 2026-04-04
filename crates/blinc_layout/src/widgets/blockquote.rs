//! Blockquote widget for quoted content
//!
//! A container for quoted text with a distinctive left border and background,
//! equivalent to `<blockquote>` in HTML.
//!
//! # Example
//!
//! ```ignore
//! use blinc_layout::prelude::*;
//!
//! blockquote()
//!     .child(p("To be or not to be, that is the question."))
//!     .child(p("— William Shakespeare"))
//! ```

use blinc_core::Color;
use blinc_theme::{ColorToken, ThemeState};

use crate::div::{div, Div, ElementBuilder};
use crate::element::RenderProps;
use crate::tree::{LayoutNodeId, LayoutTree};

/// Configuration for blockquote styling
#[derive(Clone, Debug)]
pub struct BlockquoteConfig {
    /// Left border color
    pub border_color: Color,
    /// Left border width in pixels
    pub border_width: f32,
    /// Background color (subtle)
    pub bg_color: Color,
    /// Padding inside the blockquote
    pub padding: f32,
    /// Vertical margin
    pub margin_y: f32,
}

impl Default for BlockquoteConfig {
    fn default() -> Self {
        let theme = ThemeState::get();
        Self {
            border_color: theme.color(ColorToken::Border),
            border_width: 4.0,
            bg_color: theme.color(ColorToken::SurfaceOverlay),
            padding: 4.0,
            margin_y: 2.0,
        }
    }
}

/// A blockquote container widget
///
/// Uses a simple container with background, left padding to simulate a left border,
/// and children added directly.
pub struct Blockquote {
    /// The content container with all styling and children
    inner: Div,
    css_element_id: Option<String>,
    css_classes: Vec<String>,
}

impl Blockquote {
    /// Create a new blockquote with default styling
    pub fn new() -> Self {
        Self::with_config(BlockquoteConfig::default())
    }

    /// Create a blockquote with custom configuration
    pub fn with_config(config: BlockquoteConfig) -> Self {
        let inner = div()
            .flex_col()
            .w_full()
            .my(config.margin_y)
            .bg(config.bg_color)
            .border_left(config.border_width, config.border_color)
            .p(config.padding);

        Self {
            inner,
            css_element_id: None,
            css_classes: Vec::new(),
        }
    }

    /// Add a child element to the blockquote content area
    pub fn child(mut self, child: impl ElementBuilder + 'static) -> Self {
        self.inner = self.inner.child(child);
        self
    }

    /// Set the element ID for CSS selector targeting
    pub fn id(mut self, id: &str) -> Self {
        self.css_element_id = Some(id.to_string());
        self
    }

    /// Add a CSS class for selector matching
    pub fn class(mut self, name: &str) -> Self {
        self.css_classes.push(name.to_string());
        self
    }
}

impl Default for Blockquote {
    fn default() -> Self {
        Self::new()
    }
}

impl ElementBuilder for Blockquote {
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
        crate::div::ElementTypeId::Div
    }

    fn semantic_type_name(&self) -> Option<&'static str> {
        Some("blockquote")
    }

    fn element_id(&self) -> Option<&str> {
        self.css_element_id.as_deref()
    }

    fn element_classes(&self) -> &[String] {
        &self.css_classes
    }
}

/// Create a blockquote container
///
/// # Example
///
/// ```ignore
/// blockquote()
///     .child(p("A famous quote here"))
/// ```
pub fn blockquote() -> Blockquote {
    Blockquote::new()
}

/// Create a blockquote with custom configuration
pub fn blockquote_with_config(config: BlockquoteConfig) -> Blockquote {
    Blockquote::with_config(config)
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
    fn test_blockquote_creates_container() {
        init_theme();
        let mut tree = LayoutTree::new();
        let bq = blockquote();
        bq.build(&mut tree);
        assert!(!tree.is_empty());
    }

    #[test]
    fn test_blockquote_with_child() {
        init_theme();
        let mut tree = LayoutTree::new();
        let bq = blockquote().child(div());
        bq.build(&mut tree);
        assert!(tree.len() > 1);
    }
}

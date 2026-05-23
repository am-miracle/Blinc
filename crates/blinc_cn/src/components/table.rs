//! Table — themed wrapper around `blinc_layout::widgets::table`.
//!
//! The layout widget already paints the section backgrounds, cell padding,
//! and text colours from theme tokens; `cn::table` contributes the
//! shadcn-style surface CSS (rounded outer border, hover row bg, selected
//! row tint, footer top-border + medium font-weight) and a small caption
//! helper.
//!
//! # Example
//!
//! ```ignore
//! use blinc_cn::prelude::*;
//!
//! cn::table()
//!     .w_full()
//!     .child(cn::table_header().child(
//!         cn::table_row()
//!             .child(cn::table_head("Name"))
//!             .child(cn::table_head("Status"))
//!             .child(cn::table_head("Amount")),
//!     ))
//!     .child(cn::table_body()
//!         .child(cn::table_row()
//!             .child(cn::table_cell().child(text("Alice")))
//!             .child(cn::table_cell().child(text("Active")))
//!             .child(cn::table_cell().child(text("$1,200"))))
//!         .child(cn::table_row().selected(true)
//!             .child(cn::table_cell().child(text("Bob")))
//!             .child(cn::table_cell().child(text("Pending")))
//!             .child(cn::table_cell().child(text("$840")))))
//!     .child(cn::table_caption("Recent transactions"))
//! ```

use std::ops::{Deref, DerefMut};

use blinc_layout::div::{Div, ElementBuilder, ElementTypeId};
use blinc_layout::prelude::*;
use blinc_layout::widgets::table as layout_table;
use blinc_theme::{ColorToken, SpacingToken, ThemeState};

// ============================================================================
// Sections
// ============================================================================

/// Outer table container.
///
/// Returns the layout widget's `table()` div with the `.cn-table` class
/// added. The cn stylesheet paints the outer border + rounded radius;
/// section backgrounds (header / footer) come from the layout widget's
/// theme-token setters.
pub fn table() -> Div {
    layout_table::table().class("cn-table")
}

/// Header section. Wraps `layout::thead()` and adds the
/// `.cn-table-header` class. The layout widget already paints
/// `--surface-overlay` as the header bg.
pub fn table_header() -> Div {
    layout_table::thead().class("cn-table-header")
}

/// Body section. Wraps `layout::tbody()` and adds the `.cn-table-body`
/// class. The class powers a `:last-child` rule that strips the bottom
/// row's border so it sits flush with the outer radius.
pub fn table_body() -> Div {
    layout_table::tbody().class("cn-table-body")
}

/// Footer section. Wraps `layout::tfoot()` and adds the
/// `.cn-table-footer` class. The cn stylesheet adds the top-border +
/// medium font-weight; the bg comes from the layout widget.
pub fn table_footer() -> Div {
    layout_table::tfoot().class("cn-table-footer")
}

// ============================================================================
// Row (with selected state)
// ============================================================================

/// Table row.
///
/// `selected(true)` adds the `.cn-table-row--selected` class which the
/// cn stylesheet paints with `--selection`. For reactive selection,
/// wrap the row in `Stateful` and toggle by rebuilding.
pub struct TableRow {
    inner: Div,
}

impl TableRow {
    fn new() -> Self {
        Self {
            inner: layout_table::tr().class("cn-table-row"),
        }
    }

    /// Mark this row as selected — adds `.cn-table-row--selected`.
    pub fn selected(mut self, selected: bool) -> Self {
        if selected {
            self.inner = self.inner.class("cn-table-row--selected");
        }
        self
    }

    /// Add a child element (typically a `cn::table_head` / `cn::table_cell`).
    pub fn child(mut self, child: impl ElementBuilder + 'static) -> Self {
        self.inner = self.inner.child(child);
        self
    }

    /// Add a boxed child element.
    pub fn child_box(mut self, child: Box<dyn ElementBuilder>) -> Self {
        self.inner = self.inner.child_box(child);
        self
    }

    /// Convert into the underlying `Div` for advanced customisation.
    pub fn into_div(self) -> Div {
        self.inner
    }
}

impl Default for TableRow {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for TableRow {
    type Target = Div;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for TableRow {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl ElementBuilder for TableRow {
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

    fn element_id(&self) -> Option<&str> {
        self.inner.element_id()
    }
}

/// Build a new table row.
pub fn table_row() -> TableRow {
    TableRow::new()
}

// ============================================================================
// Cells
// ============================================================================

/// Header cell. Builds on `layout::cell()` (empty TableCell with the
/// theme-token padding / flex-1 distribution) and supplies a
/// medium-weight, muted-foreground text child for shadcn parity.
pub fn table_head(content: impl Into<String>) -> layout_table::TableCell {
    let theme = ThemeState::get();
    layout_table::cell().class("cn-table-head").child(
        text(content)
            .size(theme.typography().text_sm)
            .medium()
            .color(theme.color(ColorToken::TextSecondary)),
    )
}

/// Data cell. Wraps `layout::cell()` with the `.cn-table-cell` class —
/// caller supplies the content child.
pub fn table_cell() -> layout_table::TableCell {
    layout_table::cell().class("cn-table-cell")
}

// ============================================================================
// Caption
// ============================================================================

/// Small muted caption below the table.
pub fn table_caption(content: impl Into<String>) -> Div {
    let theme = ThemeState::get();
    let pad = theme.spacing_value(SpacingToken::Space3);
    div()
        .class("cn-table-caption")
        .flex_row()
        .w_full()
        .justify_center()
        .padding_y_px(pad)
        .child(
            text(content)
                .size(theme.typography().text_sm)
                .color(theme.color(ColorToken::TextSecondary)),
        )
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
    fn build_simple_table() {
        init_theme();
        let _ = table()
            .child(table_header().child(table_row().child(table_head("Name"))))
            .child(table_body().child(table_row().child(table_cell().child(text("Alice")))));
    }

    #[test]
    fn selected_row_carries_modifier_class() {
        init_theme();
        let row = table_row().selected(true);
        let classes = row.element_classes();
        assert!(
            classes
                .iter()
                .any(|c| c.as_ref() == "cn-table-row--selected"),
            "selected row missing modifier class: {classes:?}"
        );
    }

    #[test]
    fn caption_builds() {
        init_theme();
        let _ = table_caption("Caption text");
    }
}

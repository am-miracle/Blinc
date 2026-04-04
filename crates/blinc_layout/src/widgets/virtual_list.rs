//! Virtualized list widget
//!
//! Renders only the visible items in a scrollable list, enabling efficient
//! display of 10K+ items without creating DOM nodes for every item.
//!
//! # Example
//!
//! ```ignore
//! use blinc_layout::prelude::*;
//! use blinc_layout::widgets::virtual_list::virtual_list;
//!
//! let items: Vec<String> = (0..10_000).map(|i| format!("Item {}", i)).collect();
//!
//! virtual_list(items.len(), 32.0, move |index| {
//!     div()
//!         .h(32.0)
//!         .w_full()
//!         .padding_x_px(12.0)
//!         .items_center()
//!         .child(text(&items[index]).size(14.0).color(Color::WHITE))
//! })
//! .w_full()
//! .h(400.0)
//! ```

use std::sync::Arc;

use crate::div::{div, Div};
use crate::widgets::scroll::ScrollDirection;

/// Builder function that creates a Div for a given item index
pub type ItemBuilder = Arc<dyn Fn(usize) -> Div + Send + Sync>;

/// A virtualized list that only renders visible items.
///
/// Use `virtual_list(count, item_height, builder)` to create one.
pub struct VirtualList {
    /// Total number of items
    item_count: usize,
    /// Height of each item in pixels (fixed)
    item_height: f32,
    /// Function to build a Div for a given index
    item_builder: ItemBuilder,
    /// Inner div for layout props (width, height, bg, etc.)
    inner: Div,
    /// Overscan: extra items to render above/below viewport
    overscan: usize,
}

/// Create a virtualized list.
///
/// - `item_count`: total number of items
/// - `item_height`: fixed height per item in pixels
/// - `builder`: closure that creates a Div for each visible item index
pub fn virtual_list<F>(item_count: usize, item_height: f32, builder: F) -> VirtualList
where
    F: Fn(usize) -> Div + Send + Sync + 'static,
{
    VirtualList {
        item_count,
        item_height,
        item_builder: Arc::new(builder),
        inner: div().overflow_clip(),
        overscan: 3,
    }
}

impl VirtualList {
    /// Set number of extra items to render above/below the viewport (default: 3)
    pub fn overscan(mut self, n: usize) -> Self {
        self.overscan = n;
        self
    }

    // Forward common Div methods
    pub fn w(mut self, v: f32) -> Self {
        self.inner = self.inner.w(v);
        self
    }
    pub fn h(mut self, v: f32) -> Self {
        self.inner = self.inner.h(v);
        self
    }
    pub fn w_full(mut self) -> Self {
        self.inner = self.inner.w_full();
        self
    }
    pub fn h_full(mut self) -> Self {
        self.inner = self.inner.h_full();
        self
    }
    pub fn bg(mut self, color: blinc_core::Color) -> Self {
        self.inner = self.inner.bg(color);
        self
    }
    pub fn rounded(mut self, r: f32) -> Self {
        self.inner = self.inner.rounded(r);
        self
    }
    pub fn border(mut self, width: f32, color: blinc_core::Color) -> Self {
        self.inner = self.inner.border(width, color);
        self
    }
    pub fn p(mut self, v: f32) -> Self {
        self.inner = self.inner.p(v);
        self
    }
    pub fn id(mut self, id: &str) -> Self {
        self.inner = self.inner.id(id);
        self
    }
    pub fn class(mut self, class: &str) -> Self {
        self.inner = self.inner.class(class);
        self
    }
}

impl VirtualList {
    /// Build the virtual list into a Div that can be used as a child element.
    ///
    /// This constructs the scroll container with visible items and spacers.
    pub fn into_div(self) -> Div {
        let total_height = self.item_count as f32 * self.item_height;

        // Get viewport height from the inner div's style (or default)
        let viewport_height = {
            use crate::div::ElementBuilder as _;
            self.inner
                .layout_style()
                .and_then(|s| match s.size.height {
                    taffy::Dimension::Length(h) => Some(h),
                    _ => None,
                })
                .unwrap_or(400.0)
        };

        // For the initial render, show items starting from index 0
        let visible_count =
            (viewport_height / self.item_height).ceil() as usize + self.overscan * 2;
        let end_idx = visible_count.min(self.item_count);

        // Build content: visible items + bottom spacer
        let mut content = div().flex_col().w_full();
        for i in 0..end_idx {
            content = content.child((self.item_builder)(i));
        }
        if end_idx < self.item_count {
            let bottom_space = (self.item_count - end_idx) as f32 * self.item_height;
            content = content.child(div().h(bottom_space).w_full());
        }

        // Wrap in scroll container
        let scroll = crate::widgets::scroll::scroll()
            .direction(ScrollDirection::Vertical)
            .w_full()
            .h(viewport_height)
            .child(content);

        self.inner.child(scroll)
    }
}

impl From<VirtualList> for Div {
    fn from(vl: VirtualList) -> Div {
        vl.into_div()
    }
}

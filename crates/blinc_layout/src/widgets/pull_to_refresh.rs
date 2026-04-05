//! Pull-to-refresh widget
//!
//! A scroll container that triggers a refresh callback when the user
//! pulls down past a threshold. Includes a loading indicator and
//! spring-animated content offset.
//!
//! # Example
//!
//! ```ignore
//! use blinc_layout::widgets::pull_to_refresh::pull_to_refresh;
//!
//! pull_to_refresh(|| {
//!     // Called when user pulls past threshold and releases
//!     println!("Refreshing...");
//! })
//! .child(
//!     div().flex_col()
//!         .child(text("Item 1"))
//!         .child(text("Item 2"))
//! )
//! .w_full()
//! .h(400.0)
//! ```

use std::sync::Arc;

use crate::div::{div, Div};

/// Pull-to-refresh container builder
pub struct PullToRefresh {
    /// Callback invoked when refresh is triggered
    on_refresh: Arc<dyn Fn() + Send + Sync>,
    /// Inner div for layout (width, height, etc.)
    inner: Div,
    /// Content to wrap in the pull container
    content: Option<Div>,
    /// Distance in pixels to pull before refresh triggers
    threshold: f32,
    /// Maximum pull distance
    max_pull: f32,
    /// Loading indicator text
    loading_text: String,
}

/// Create a pull-to-refresh container.
///
/// The callback fires when the user pulls down past the threshold and releases.
pub fn pull_to_refresh<F>(on_refresh: F) -> PullToRefresh
where
    F: Fn() + Send + Sync + 'static,
{
    PullToRefresh {
        on_refresh: Arc::new(on_refresh),
        inner: div().overflow_clip(),
        content: None,
        threshold: 60.0,
        max_pull: 100.0,
        loading_text: "Release to refresh".to_string(),
    }
}

impl PullToRefresh {
    /// Set the child content
    pub fn child(mut self, content: Div) -> Self {
        self.content = Some(content);
        self
    }

    /// Set the pull threshold distance in pixels (default: 60)
    pub fn threshold(mut self, px: f32) -> Self {
        self.threshold = px;
        self
    }

    /// Set the maximum pull distance (default: 100)
    pub fn max_pull(mut self, px: f32) -> Self {
        self.max_pull = px;
        self
    }

    /// Set the loading indicator text
    pub fn loading_text(mut self, text: impl Into<String>) -> Self {
        self.loading_text = text.into();
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
    pub fn id(mut self, id: &str) -> Self {
        self.inner = self.inner.id(id);
        self
    }
    pub fn class(mut self, class: &str) -> Self {
        self.inner = self.inner.class(class);
        self
    }

    /// Build into a Div
    pub fn into_div(self) -> Div {
        use blinc_core::Color;
        use std::sync::Mutex;

        let threshold = self.threshold;
        let max_pull = self.max_pull;
        let on_refresh = self.on_refresh;
        let loading_text = self.loading_text;

        // Shared state for drag tracking
        let drag_start_y: Arc<Mutex<f32>> = Arc::new(Mutex::new(0.0));
        let pull_offset: Arc<Mutex<f32>> = Arc::new(Mutex::new(0.0));
        let is_dragging: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
        let is_armed: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

        let ds_down = Arc::clone(&drag_start_y);
        let dragging_down = Arc::clone(&is_dragging);

        let ds_drag = Arc::clone(&drag_start_y);
        let po_drag = Arc::clone(&pull_offset);
        let dragging_drag = Arc::clone(&is_dragging);
        let armed_drag = Arc::clone(&is_armed);

        let po_up = Arc::clone(&pull_offset);
        let dragging_up = Arc::clone(&is_dragging);
        let armed_up = Arc::clone(&is_armed);

        // Indicator shown above content
        let indicator = div()
            .w_full()
            .h(40.0)
            .items_center()
            .justify_center()
            .child(
                crate::text::text(&loading_text)
                    .size(12.0)
                    .color(Color::rgba(0.5, 0.5, 0.6, 1.0)),
            );

        // Build the pull container
        let content = self.content.unwrap_or_else(div);

        let container = self
            .inner
            .flex_col()
            .relative()
            .child(indicator)
            .child(content)
            .on_mouse_down(move |ctx| {
                *ds_down.lock().unwrap() = ctx.mouse_y;
                *dragging_down.lock().unwrap() = true;
            })
            .on_drag(move |ctx| {
                if !*dragging_drag.lock().unwrap() {
                    return;
                }
                let start = *ds_drag.lock().unwrap();
                let delta = (ctx.mouse_y - start).max(0.0).min(max_pull);
                *po_drag.lock().unwrap() = delta;
                *armed_drag.lock().unwrap() = delta >= threshold;
            })
            .on_mouse_up(move |_ctx| {
                let was_armed = *armed_up.lock().unwrap();
                *dragging_up.lock().unwrap() = false;
                *po_up.lock().unwrap() = 0.0;
                *armed_up.lock().unwrap() = false;

                if was_armed {
                    on_refresh();
                }
            });

        container
    }
}

impl From<PullToRefresh> for Div {
    fn from(ptr: PullToRefresh) -> Div {
        ptr.into_div()
    }
}

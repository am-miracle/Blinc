//! Tree Panel - Element tree with diff visualization

use std::cell::OnceCell;

use blinc_cn::components::input::{InputSize, input};
use blinc_cn::components::separator::separator;
use blinc_cn::components::tree::{TreeNodeDiff, tree_view};
use blinc_layout::div::{Div, ElementBuilder};
use blinc_layout::element::RenderProps;
use blinc_layout::event_handler::EventHandlers;
use blinc_layout::prelude::*;
use blinc_layout::tree::{LayoutNodeId, LayoutTree};
use blinc_layout::widgets::text_input::text_input_data;
use blinc_recorder::TreeSnapshot;
use blinc_theme::{ColorToken, ThemeState};

use crate::theme::DebuggerTokens;

/// State for the tree panel
#[derive(Default)]
pub struct TreePanelState {
    pub selected_id: Option<String>,
    pub expanded_ids: Vec<String>,
    pub filter_text: String,
}

struct TreePanelConfig {
    has_snapshot: bool,
}

struct BuiltTreePanel {
    inner: Div,
}

impl BuiltTreePanel {
    fn from_config(config: &TreePanelConfig) -> Self {
        let theme = ThemeState::get();

        let inner = div()
            .w(DebuggerTokens::TREE_PANEL_WIDTH)
            .h_full()
            .bg(theme.color(ColorToken::SurfaceElevated))
            .flex_col()
            .child(Self::header())
            .child(separator())
            .child(Self::search_bar())
            .child(Self::tree_content(config.has_snapshot))
            .child(separator());

        BuiltTreePanel { inner }
    }

    fn header() -> Div {
        let theme = ThemeState::get();
        div()
            .h(44.0)
            .px(12.0)
            .py(2.0)
            .flex_row()
            .items_center()
            .child(
                text("Element Tree")
                    .size(13.0)
                    .color(theme.color(ColorToken::TextPrimary))
                    .weight(blinc_layout::div::FontWeight::SemiBold),
            )
    }

    fn search_bar() -> Div {
        let search_data = text_input_data();
        div().px(8.0).py(6.0).child(
            input(&search_data)
                .placeholder("Search...")
                .size(InputSize::Small),
        )
    }

    fn tree_content(has_snapshot: bool) -> Scroll {
        let content = if has_snapshot {
            Self::render_tree_placeholder()
        } else {
            Self::render_empty_state()
        };

        scroll().flex_grow().child(content)
    }

    fn render_tree_placeholder() -> Div {
        div().child(
            tree_view()
                .indent(12.0)
                .node("root", "root", |n| {
                    n.expanded()
                        .child("header", "header", |c| c)
                        .child("main", "main", |c| {
                            c.expanded()
                                .diff(TreeNodeDiff::Modified)
                                .child("sidebar", "sidebar", |c| c)
                                .child("content", "content", |c| c.diff(TreeNodeDiff::Added))
                        })
                        .child("footer", "footer", |c| c)
                })
                .on_select(|_key| {}),
        )
    }

    fn render_empty_state() -> Div {
        let theme = ThemeState::get();
        div()
            .w_full()
            .h_full()
            .items_center()
            .justify_center()
            .child(
                text("No recording loaded")
                    .size(13.0)
                    .color(theme.color(ColorToken::TextTertiary)),
            )
    }
}

pub struct TreePanel {
    config: TreePanelConfig,
    built: OnceCell<BuiltTreePanel>,
}

impl TreePanel {
    pub fn new(_snapshot: Option<&TreeSnapshot>, _state: &TreePanelState) -> Self {
        Self {
            config: TreePanelConfig {
                has_snapshot: _snapshot.is_some(),
            },
            built: OnceCell::new(),
        }
    }

    fn get_or_build(&self) -> &BuiltTreePanel {
        self.built
            .get_or_init(|| BuiltTreePanel::from_config(&self.config))
    }

    pub fn build(self) -> Div {
        BuiltTreePanel::from_config(&self.config).inner
    }
}

impl ElementBuilder for TreePanel {
    fn build(&self, tree: &mut LayoutTree) -> LayoutNodeId {
        self.get_or_build().inner.build(tree)
    }

    fn render_props(&self) -> RenderProps {
        self.get_or_build().inner.render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        self.get_or_build().inner.children_builders()
    }

    fn event_handlers(&self) -> Option<&EventHandlers> {
        let handlers = self.get_or_build().inner.event_handlers();
        if handlers.is_empty() {
            None
        } else {
            Some(handlers)
        }
    }
}

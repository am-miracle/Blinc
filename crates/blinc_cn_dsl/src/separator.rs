//! `cn.Separator` — divider line.

use std::cell::OnceCell;

use blinc_dsl_core::extern_widget;
use blinc_layout::div::ElementBuilder;

/// `cn.Separator(orientation?)` — divider line.
///
/// Props (DSL surface):
/// - `orientation: string` — `"horizontal"` (default) or `"vertical"`.
///
/// `blinc_cn::Separator` also exposes layout-shape builders (`.w()`,
/// `.h()`, `.m{,x,y,t,b,l,r}()`, `.bg()`, `.opacity()`) — those
/// overlap with the DSL's universal `Div(...)` styling surface and
/// should ride through that path rather than as per-widget props.
#[extern_widget(namespace = "cn", name = "Separator")]
pub struct CnSeparator {
    pub orientation: String,
    /// Lazy-constructed cn widget. Same caching rationale as
    /// `CnButton::built`.
    #[skip]
    built: OnceCell<blinc_cn::Separator>,
}

impl CnSeparator {
    fn get_or_build(&self) -> &blinc_cn::Separator {
        self.built.get_or_init(|| self.to_cn_widget())
    }

    fn to_cn_widget(&self) -> blinc_cn::Separator {
        match self.orientation.as_str() {
            "vertical" => blinc_cn::Separator::new().vertical(),
            "" | "horizontal" => blinc_cn::Separator::new(),
            other => {
                tracing::warn!(
                    orientation = %other,
                    "cn.Separator: unknown orientation — falling back to `horizontal`",
                );
                blinc_cn::Separator::new()
            }
        }
    }
}

impl ElementBuilder for CnSeparator {
    fn build(&self, tree: &mut blinc_layout::LayoutTree) -> blinc_layout::LayoutNodeId {
        self.get_or_build().build(tree)
    }
    fn render_props(&self) -> blinc_layout::RenderProps {
        self.get_or_build().render_props()
    }
    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        self.get_or_build().children_builders()
    }
}

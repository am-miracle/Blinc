//! `cn.Skeleton` — placeholder block while content loads.

use std::cell::OnceCell;

use blinc_dsl_core::extern_widget;
use blinc_layout::div::ElementBuilder;

/// `cn.Skeleton(w?, h?, rounded?, circle_size?)` — placeholder
/// block while content loads.
///
/// Props (DSL surface):
/// - `w: f64` — width in pixels. Zero means "use cn default
///   (160px)". Set together with `h` for a custom rectangle.
/// - `h: f64` — height in pixels. Same zero-as-default semantics.
/// - `rounded: f64` — corner radius. Zero means "use cn default
///   (4px)".
/// - `circle_size: f64` — when non-zero, builds a circular
///   skeleton (`cn::skeleton_circle(size)`) of that diameter and
///   ignores `w` / `h` / `rounded`.
///
/// Shimmer animation isn't part of this prop surface — `cn::Skeleton`
/// promotes to `AnimatedSkeleton` via `.shimmer(timeline)`, which
/// takes a `SharedAnimatedTimeline` that the DSL doesn't expose
/// today. Static skeletons cover the loading-state case for now.
#[extern_widget(namespace = "cn", name = "Skeleton")]
pub struct CnSkeleton {
    pub w: f64,
    pub h: f64,
    pub rounded: f64,
    pub circle_size: f64,
    /// Lazy-constructed cn widget. Same caching rationale as
    /// `CnButton::built`.
    #[skip]
    built: OnceCell<blinc_cn::Skeleton>,
}

impl CnSkeleton {
    fn get_or_build(&self) -> &blinc_cn::Skeleton {
        self.built.get_or_init(|| self.to_cn_widget())
    }

    fn to_cn_widget(&self) -> blinc_cn::Skeleton {
        if self.circle_size > 0.0 {
            // Circle takes priority — its own dimensions; `w`/`h`/
            // `rounded` are silently ignored to keep the call shape
            // small. The doc above flags this.
            return blinc_cn::Skeleton::circle(self.circle_size as f32);
        }
        let mut s = blinc_cn::Skeleton::new();
        if self.w > 0.0 {
            s = s.w(self.w as f32);
        }
        if self.h > 0.0 {
            s = s.h(self.h as f32);
        }
        if self.rounded > 0.0 {
            s = s.rounded(self.rounded as f32);
        }
        s
    }
}

impl ElementBuilder for CnSkeleton {
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

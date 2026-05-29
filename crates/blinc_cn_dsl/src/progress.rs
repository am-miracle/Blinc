//! `cn.Progress` — value bar.

use std::cell::OnceCell;

use blinc_dsl_core::extern_widget;
use blinc_layout::div::ElementBuilder;

/// `cn.Progress(value, size?, width?, indicator_color?, track_color?, rounded?)`
/// — value bar.
///
/// Props (DSL surface):
/// - `value: f64` — fill ratio in `[0.0, 1.0]`. Out-of-range values
///   get clamped at render time on the cn side.
/// - `size: string` — `"small"`, `"medium"` (default), or `"large"`.
/// - `width: f64` — bar width in pixels. Zero (default) means "use
///   cn default of 200px".
/// - `indicator_color: string` / `track_color: string` — hex colour
///   overrides per [`crate::color::parse_color_prop`].
/// - `rounded: f64` — corner radius override. Zero means "use cn
///   default" (the size-derived value).
///
/// Reactive bindings (`cn.Progress(value = my_signal)`) aren't wired
/// in this surface — `value` is a literal `f64`. The cn-side
/// `IntoReactive<f32>` shape supports binding to `&State<f32>` but
/// signal-bound colour / numeric props need the same overlay-pass
/// integration as built-in widgets; tracked separately.
#[extern_widget(namespace = "cn", name = "Progress")]
pub struct CnProgress {
    pub value: f64,
    pub size: String,
    pub width: f64,
    pub indicator_color: String,
    pub track_color: String,
    pub rounded: f64,
    /// Lazy-constructed cn widget. Same caching rationale as
    /// `CnButton::built`. We cache the FINAL `Progress` (via
    /// `.build_component()`) rather than `ProgressBuilder` because
    /// the builder type isn't re-exported at `blinc_cn::` crate
    /// root; `Progress` is.
    #[skip]
    built: OnceCell<blinc_cn::Progress>,
}

impl CnProgress {
    fn get_or_build(&self) -> &blinc_cn::Progress {
        self.built
            .get_or_init(|| self.to_cn_builder().build_component())
    }

    fn to_cn_builder(&self) -> blinc_cn::components::progress::ProgressBuilder {
        let size = match self.size.as_str() {
            "small" => blinc_cn::ProgressSize::Small,
            "large" => blinc_cn::ProgressSize::Large,
            "" | "medium" => blinc_cn::ProgressSize::Medium,
            other => {
                tracing::warn!(
                    size = %other,
                    "cn.Progress: unknown size — falling back to `medium`",
                );
                blinc_cn::ProgressSize::Medium
            }
        };
        let mut b = blinc_cn::progress(self.value as f32).size(size);
        if self.width > 0.0 {
            b = b.w(self.width as f32);
        }
        if let Some(c) =
            crate::color::parse_color_prop("cn.Progress", "indicator_color", &self.indicator_color)
        {
            b = b.indicator_color(c);
        }
        if let Some(c) =
            crate::color::parse_color_prop("cn.Progress", "track_color", &self.track_color)
        {
            b = b.track_color(c);
        }
        if self.rounded > 0.0 {
            b = b.rounded(self.rounded as f32);
        }
        b
    }
}

impl ElementBuilder for CnProgress {
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

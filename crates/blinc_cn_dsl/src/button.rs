//! `cn.Button` — single-line action button.

use std::cell::OnceCell;

use blinc_dsl_core::extern_widget;
use blinc_layout::div::ElementBuilder;

/// `cn.Button(label, variant?, size?, disabled?, icon?, icon_position?, icon_size?, on_click?)`
/// — a single-line action button.
///
/// Props (DSL surface):
/// - `label: string` — the button text. Positional or named.
/// - `variant: string` — `"primary"` (default), `"secondary"`,
///   `"destructive"`, `"outline"`, `"ghost"`, or `"link"`. Unknown
///   values fall back to `"primary"`.
/// - `size: string` — `"small"`, `"medium"` (default), or `"large"`.
///   Unknown values fall back to `"medium"`.
/// - `disabled: bool` — false by default.
/// - `icon: string` — icon name / SVG-path. Empty string means no icon.
/// - `icon_position: string` — `"start"` (default) or `"end"`. Ignored
///   when `icon` is empty.
/// - `icon_size: f64` — pixel size override. Zero means "derive from
///   button size" (the cn-side default).
/// - `on_click: || => unit` — DSL closure invoked on click. Mirrors
///   the existing `Div(on_click = ||{ … })` shape: zero-arg, fires
///   for side effects (signal writes, FSM triggers). The cn-side
///   `EventContext` is discarded — the closure runs as if it had
///   been written `||{ … }` on a plain Div.
///
/// `color` (Color override) is intentionally deferred — color FFI
/// needs a string-hex / packed-i32 design that's shared across every
/// cn widget with a colour prop. Tracked separately.
#[extern_widget(namespace = "cn", name = "Button")]
pub struct CnButton {
    pub label: String,
    pub variant: String,
    pub size: String,
    pub disabled: bool,
    pub icon: String,
    pub icon_position: String,
    pub icon_size: f64,
    /// Closure handle minted by Zyntax's `CreateClosure` → `func_addr`.
    /// Zero when the user omitted `on_click`. See [`CnButton::to_cn_builder`]
    /// for the transmute + dispatch.
    pub on_click: i64,
    /// Lazy-constructed cn-side builder. Cached so the `ElementBuilder`
    /// trait methods can delegate to a stable reference instead of
    /// rebuilding per call — and so `children_builders()` can hand
    /// back the cn widget's own slice rather than an empty stub. Not
    /// part of the FFI surface; the macro skips it and the thunk-side
    /// constructor fills the field via `Default::default()`.
    #[skip]
    built: OnceCell<blinc_cn::ButtonBuilder>,
}

impl CnButton {
    /// Lazy-build the cn-side widget once, then hand back a stable
    /// reference. Mirrors the OnceCell pattern cn's own
    /// `ButtonBuilder::get_or_build` uses.
    fn get_or_build(&self) -> &blinc_cn::ButtonBuilder {
        self.built.get_or_init(|| self.to_cn_builder())
    }

    fn to_cn_builder(&self) -> blinc_cn::ButtonBuilder {
        let variant = match self.variant.as_str() {
            "secondary" => blinc_cn::ButtonVariant::Secondary,
            "destructive" => blinc_cn::ButtonVariant::Destructive,
            "outline" => blinc_cn::ButtonVariant::Outline,
            "ghost" => blinc_cn::ButtonVariant::Ghost,
            "link" => blinc_cn::ButtonVariant::Link,
            // Empty string ("variant not supplied") OR explicit "primary"
            // OR an unknown value all resolve to Primary. Unknown values
            // get a tracing::warn so misspelled enum strings surface in
            // logs without breaking the build.
            "" | "primary" => blinc_cn::ButtonVariant::Primary,
            other => {
                tracing::warn!(
                    variant = %other,
                    "cn.Button: unknown variant — falling back to `primary`",
                );
                blinc_cn::ButtonVariant::Primary
            }
        };
        let size = match self.size.as_str() {
            "small" => blinc_cn::ButtonSize::Small,
            "large" => blinc_cn::ButtonSize::Large,
            "" | "medium" => blinc_cn::ButtonSize::Medium,
            other => {
                tracing::warn!(
                    size = %other,
                    "cn.Button: unknown size — falling back to `medium`",
                );
                blinc_cn::ButtonSize::Medium
            }
        };
        let mut b = blinc_cn::button(self.label.clone())
            .variant(variant)
            .size(size)
            .disabled(self.disabled);
        if !self.icon.is_empty() {
            b = b.icon(self.icon.clone());
            // `icon_position` only meaningful when an icon is present.
            let pos = match self.icon_position.as_str() {
                "end" => blinc_cn::IconPosition::End,
                "" | "start" => blinc_cn::IconPosition::Start,
                other => {
                    tracing::warn!(
                        icon_position = %other,
                        "cn.Button: unknown icon_position — falling back to `start`",
                    );
                    blinc_cn::IconPosition::Start
                }
            };
            b = b.icon_position(pos);
            if self.icon_size > 0.0 {
                b = b.icon_size(self.icon_size as f32);
            }
        }
        if self.on_click != 0 {
            // Mirrors `blinc_div_view`: Zyntax mints a zero-arg
            // `extern "C" fn()` for the DSL closure and hands it across
            // as `i64`. cn::button's on_click handler takes
            // `&EventContext`; the substrate-level DSL closure shape
            // doesn't carry that context yet, so we discard it and
            // fire the closure with no args. Signal writes inside the
            // closure route through the existing `__signal_set_*`
            // host externs as usual.
            let click_ptr = self.on_click;
            b = b.on_click(move |_ctx| {
                type ClosureFn = extern "C" fn();
                let func: ClosureFn = unsafe { std::mem::transmute(click_ptr) };
                func();
            });
        }
        b
    }
}

impl ElementBuilder for CnButton {
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

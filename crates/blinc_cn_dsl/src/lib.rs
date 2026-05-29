//! DSL bindings for the `blinc_cn` widget pack.
//!
//! Exposes shadcn-style components to the Blinc DSL under the `cn.*`
//! namespace:
//!
//! ```dsl,ignore
//! view {
//!     cn.Button("Save", variant = "primary")
//! }
//! ```
//!
//! Each wrapper struct in this crate uses
//! `#[extern_widget(namespace = "cn", name = "<Name>")]` to register
//! itself under the qualified DSL name. The grammar's namespaced
//! component-call rule routes `cn.<Name>(...)` to the matching
//! wrapper.
//!
//! ## Adoption
//!
//! ```ignore
//! let dsl = BlincDsl::new()?;
//! blinc_cn_dsl::register_all(&dsl);
//! dsl.compile_source(src, file)?;
//! ```
//!
//! `register_all` registers every widget this crate exposes. Pick
//! a focused subset via the per-category helpers
//! (`register_basics`, …) when binary size matters or you only need
//! a slice.
//!
//! ## What's exposed today
//!
//! - `cn.Button` — scalar props only (label, variant, size, disabled).
//!   Closure props (`on_click`) follow in a separate step alongside
//!   a one-arg-closure FFI pattern.
//!
//! The rest of the `blinc_cn` catalog (Card, Dialog, Combobox, …)
//! lands incrementally — leaf widgets first, container widgets after
//! the children-block FFI path proves itself end-to-end.

use blinc_dsl_core::{BlincDsl, BlincDslResult, extern_widget};
use blinc_layout::div::ElementBuilder;

// =====================================================================
// cn.Button
// =====================================================================

/// `cn.Button(label, variant?, size?, disabled?)` — a single-line
/// action button.
///
/// Props (DSL surface):
/// - `label: string` — the button text. Positional or named.
/// - `variant: string` — `"primary"` (default), `"secondary"`,
///   `"destructive"`, `"outline"`, `"ghost"`, or `"link"`. Unknown
///   values fall back to `"primary"`.
/// - `size: string` — `"small"`, `"medium"` (default), or `"large"`.
///   Unknown values fall back to `"medium"`.
/// - `disabled: bool` — false by default.
///
/// `on_click` is intentionally absent in this first cut — closure
/// marshalling lands together with the broader one-arg-closure FFI
/// pattern (see `blinc_cn_dsl` crate docs).
#[extern_widget(namespace = "cn", name = "Button")]
pub struct CnButton {
    pub label: String,
    pub variant: String,
    pub size: String,
    pub disabled: bool,
}

impl CnButton {
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
        blinc_cn::button(self.label.clone())
            .variant(variant)
            .size(size)
            .disabled(self.disabled)
    }
}

impl ElementBuilder for CnButton {
    fn build(&self, tree: &mut blinc_layout::LayoutTree) -> blinc_layout::LayoutNodeId {
        self.to_cn_builder().build(tree)
    }

    fn render_props(&self) -> blinc_layout::RenderProps {
        self.to_cn_builder().render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        // cn.Button is a leaf — no children. The Box-of-builder
        // returned by `to_cn_builder` owns its own internals but it
        // doesn't expose children for the outer tree to iterate.
        &[]
    }
}

// =====================================================================
// Registration helpers
// =====================================================================

/// Register every `cn.*` widget this crate exposes with the supplied
/// `BlincDsl`. Call once after `BlincDsl::new()`, before
/// `compile_source`.
///
/// Returns the first registration error if one occurs; subsequent
/// widgets are not attempted on failure. The error type is
/// [`blinc_dsl_core::BlincDslError`] from the underlying
/// `register_extern_widget` call.
pub fn register_all(dsl: &BlincDsl) -> BlincDslResult<()> {
    register_basics(dsl)?;
    Ok(())
}

/// Register the leaf-widget basics — `cn.Button`, and incrementally
/// the other scalar-prop widgets as they're wrapped. Stays callable
/// independently when an app wants buttons + badges + labels but not
/// the heavier surface (Dialog / Combobox / etc.).
pub fn register_basics(dsl: &BlincDsl) -> BlincDslResult<()> {
    dsl.register_extern_widget::<CnButton>()?;
    Ok(())
}

//! DSL bindings for the `blinc_cn` widget pack.
//!
//! Exposes shadcn-style components to the Blinc DSL under the `cn.*`
//! namespace:
//!
//! ```dsl,ignore
//! view {
//!     cn.Button("Save", variant = "primary", on_click = || {
//!         saved.set(true)
//!     })
//! }
//! ```
//!
//! Each widget wrapper lives in its own module and uses
//! `#[extern_widget(namespace = "cn", name = "<Name>")]` to register
//! under the qualified DSL name. The grammar's namespaced
//! component-call rule routes `cn.<Name>(...)` to the matching
//! wrapper.
//!
//! ## Adoption
//!
//! ```ignore
//! let dsl = BlincDsl::new()?;
//! blinc_cn_dsl::register_all(&dsl)?;
//! dsl.compile_source(src, file)?;
//! ```
//!
//! `register_all` registers every widget this crate exposes. Pick a
//! focused subset via the per-category helpers ([`register_basics`])
//! when binary size matters or you only want a slice.
//!
//! ## What's exposed
//!
//! Leaf widgets shipping today:
//! - [`button`] — `cn.Button`, with `on_click` closure prop.
//! - [`badge`] — `cn.Badge`.
//! - [`alert`] — `cn.Alert`.
//! - [`label`] — `cn.Label`.
//! - [`separator`] — `cn.Separator`.
//! - [`spinner`] — `cn.Spinner`.
//!
//! Container widgets:
//! - [`card`] — `cn.Card { children… }`. Body block flows through
//!   the macro's existing `#[children]` plumbing.
//!
//! Heavier container surface (`Dialog`, `Combobox`, `Tabs`, `Drawer`,
//! `Table`, …) lands incrementally as each widget's prop / slot
//! shape gets wired.

pub mod alert;
pub mod badge;
pub mod button;
pub mod card;
pub mod label;
pub mod separator;
pub mod spinner;

// Internal — shared helpers used by per-widget modules. Not
// re-exported; widgets pull what they need via `crate::color::…`.
pub(crate) mod color;

pub use alert::CnAlert;
pub use badge::CnBadge;
pub use button::CnButton;
pub use card::CnCard;
pub use label::CnLabel;
pub use separator::CnSeparator;
pub use spinner::CnSpinner;

use blinc_dsl_core::{BlincDsl, BlincDslResult};

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

/// Register the leaf-widget basics — every `cn.*` wrapper currently
/// shipped by this crate. Stays callable independently so an app that
/// adds heavier container widgets later can pick categories instead
/// of always paying for the full surface.
pub fn register_basics(dsl: &BlincDsl) -> BlincDslResult<()> {
    dsl.register_extern_widget::<CnButton>()?;
    dsl.register_extern_widget::<CnBadge>()?;
    dsl.register_extern_widget::<CnAlert>()?;
    dsl.register_extern_widget::<CnLabel>()?;
    dsl.register_extern_widget::<CnSeparator>()?;
    dsl.register_extern_widget::<CnSpinner>()?;
    dsl.register_extern_widget::<CnCard>()?;
    Ok(())
}

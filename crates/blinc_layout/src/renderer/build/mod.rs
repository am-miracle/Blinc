//! Tree construction + incremental update on `RenderTree`.
//!
//! Build-side concerns split across submodules:
//!
//! - [`text`] — text-property inheritance from parent → child
//!   (`inherit_text_props_from_parent`) and `TextData` materialisation
//!   (`build_text_data`).
//! - [`element_type`] — `determine_element_type` /
//!   `_boxed` projection from an `ElementBuilder` to the
//!   `ElementType` enum stored on every render node.
//!
//! Other build-side concerns (collect_render_props,
//! analyze_changes / update_render_props_in_place, subtree rebuild
//! flow) still live in `renderer/mod.rs` and will migrate here.

pub mod diff;
pub mod element_type;
pub mod subtree;
pub mod text;

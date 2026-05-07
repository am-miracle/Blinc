//! Stylesheet matching + application on `RenderTree`.
//!
//! The stylesheet pass is layered:
//!
//! - [`selectors`] — selector matching engine: `compound_matches`,
//!   `complex_selector_matches`, `selector_specificity`, plus the
//!   per-frame `apply_complex_selector_styles` /
//!   `apply_svg_tag_styles` driver passes that consume them.
//! - [`apply`] — `ElementStyle` → `RenderProps` / `taffy::Style`
//!   projection (`apply_element_style_to_props`,
//!   `apply_element_style_to_taffy`), keyframe-property snapshots for
//!   transition detection, and `clip-path` resolution.
//!
//! Other parts of the stylesheet flow (base-style apply, state-style
//! apply, layout-property override into taffy, ID-style getters)
//! still live in `renderer/mod.rs` and will migrate here as the
//! refactor progresses.

pub mod apply;
pub mod base;
pub mod selectors;

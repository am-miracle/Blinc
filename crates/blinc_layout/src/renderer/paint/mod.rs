//! Painting + render-walker machinery on `RenderTree`.
//!
//! Build-side concerns (tree creation, change analysis, subtree
//! rebuilds) live in [`crate::renderer::build`] —
//! this module is everything that walks the populated `RenderTree`
//! and emits draw calls or collects artefacts.
//!
//! Submodules:
//!
//! - [`collect`] — non-painting walkers that gather per-node
//!   artefacts into flat `Vec`s (`text_elements`, `svg_elements`,
//!   the deprecated `collect_glass_panels`).
//!
//! Other paint-side concerns (the `render` / `render_layer*` /
//! `render_to` / `render_layer_with_motion` walkers) still live in
//! `renderer/mod.rs` and will migrate here.

pub mod collect;

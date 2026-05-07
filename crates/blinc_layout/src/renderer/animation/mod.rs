//! Animation systems on `RenderTree`.
//!
//! Three loosely-coupled subsystems live here:
//!
//! - [`flip`] — FLIP-style position transitions for elements whose
//!   layout bounds changed across a subtree rebuild. Keyed by stable
//!   element id so they survive node-id churn.
//! - (TODO) `visual` — `animate_bounds`-driven offsets layered on
//!   top of taffy bounds without modifying the layout tree itself.
//! - (TODO) `css` — keyframe / transition machinery that interprets
//!   CSS `animation:` and `transition:` declarations, with a
//!   companion store on `RenderTree`.
//!
//! Methods in each submodule are extra `impl RenderTree` blocks; the
//! struct definition + fields stay in `renderer/mod.rs`.

pub mod flip;

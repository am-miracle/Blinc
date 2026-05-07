//! Cross-tree state transfers used during incremental rebuilds.
//!
//! When the runtime rebuilds the render tree (e.g. after a Stateful
//! dispatch produces a new structural shape), it constructs a fresh
//! `RenderTree` and then `transfer_*_from` walks the previous tree
//! to preserve scroll positions, scroll physics, viewport-cull
//! opt-ins, and per-node state storage. Without these, every rebuild
//! would scroll back to the top, drop momentum, and destroy widget
//! state — visibly broken on every Stateful interaction.
//!
//! Kept tiny and pure so the call sites in the build / runner code
//! stay unambiguous: each method copies one HashMap field by node
//! id, with no side effects on the source tree.

use std::sync::Arc;

use super::RenderTree;

impl RenderTree {
    /// Transfer scroll offsets from another tree (preserves scroll position across rebuilds)
    pub fn transfer_scroll_offsets_from(&mut self, other: &RenderTree) {
        for (node_id, offset) in &other.scroll_offsets {
            self.scroll_offsets.insert(*node_id, *offset);
        }
    }

    /// Transfer scroll physics from another tree (preserves scroll physics across rebuilds)
    pub fn transfer_scroll_physics_from(&mut self, other: &RenderTree) {
        for (node_id, physics) in &other.scroll_physics {
            self.scroll_physics.insert(*node_id, physics.clone());
        }
        for node_id in &other.viewport_cull_scrolls {
            self.viewport_cull_scrolls.insert(*node_id);
        }
    }

    /// Transfer node states from another tree
    ///
    /// This preserves state across rebuilds by copying the state storage
    /// from the old tree to the new one.
    pub fn transfer_states_from(&mut self, other: &RenderTree) {
        for (node_id, state) in &other.node_states {
            self.node_states.insert(*node_id, Arc::clone(state));
        }
    }
}

//! Cursor-style queries on `RenderTree`.
//!
//! Exposes three methods used by the windowed app's mouse-move
//! pipeline: `get_cursor` for a single-node lookup, `get_cursor_at`
//! for hit-tested topmost-or-ancestor lookup, and
//! `has_any_cursor_style` as a cheap "is any cursor styling present
//! at all?" predicate that lets the runner skip per-move cursor
//! resolution on UIs that never customise it.

use crate::tree::LayoutNodeId;

use super::RenderTree;

impl RenderTree {
    /// Get the cursor style for a node
    ///
    /// Returns the cursor style if set on this node, None if not set.
    pub fn get_cursor(&self, node: LayoutNodeId) -> Option<crate::element::CursorStyle> {
        self.render_nodes.get(&node).and_then(|n| n.props.cursor)
    }

    /// Whether any node in the tree has a non-default cursor style.
    ///
    /// Lets the windowed app skip the per-mouse-move cursor hit_test
    /// entirely on UIs that don't customise the cursor anywhere — the
    /// `hello_blinc` baseline now stays at near-zero CPU even during a
    /// continuous drag because we no longer hit_test + syscall per move.
    /// Bounded O(N) over render nodes with early exit on first match.
    pub fn has_any_cursor_style(&self) -> bool {
        self.render_nodes.values().any(|n| n.props.cursor.is_some())
    }

    /// Get the cursor style for the topmost hovered element at a point
    ///
    /// Walks up the ancestor chain starting from the topmost element,
    /// returning the first cursor style found. This allows child elements
    /// to override parent cursor styles.
    pub fn get_cursor_at(
        &self,
        router: &crate::event_router::EventRouter,
        x: f32,
        y: f32,
    ) -> Option<crate::element::CursorStyle> {
        // Hit test to find topmost element
        let hit = router.hit_test(self, x, y)?;

        // Check the hit node first
        if let Some(cursor) = self.get_cursor(hit.node) {
            return Some(cursor);
        }

        // Walk up ancestors (from leaf towards root) to find first cursor
        // Ancestors are stored from root to leaf, so iterate in reverse
        for &ancestor in hit.ancestors.iter().rev() {
            if let Some(cursor) = self.get_cursor(ancestor) {
                return Some(cursor);
            }
        }

        None
    }

    /// Cursor resolution that reuses the ancestor chain cached by
    /// the most recent `on_mouse_move`. Identical semantics to
    /// [`Self::get_cursor_at`] (leaf-wins via reverse walk) but
    /// skips the second hit_test, which was the dominant cost in
    /// the mouse-move pipeline once primitive emission was gated
    /// elsewhere — a Magic Mouse's ~120 Hz move rate × cn_demo's
    /// tree depth × HashMap-allocating recursion ate ~10 % CPU on
    /// its own.
    ///
    /// Returns `None` when the last hit test missed (cursor outside
    /// any node) or no ancestor in the chain carries a `cursor:`
    /// style.
    pub fn get_cursor_for_last_hit(
        &self,
        router: &crate::event_router::EventRouter,
    ) -> Option<crate::element::CursorStyle> {
        // Chain is stored root → leaf. Iterate in reverse so the
        // deepest hovered node wins — child overrides parent.
        for &node in router.last_hit_chain().iter().rev() {
            if let Some(cursor) = self.get_cursor(node) {
                return Some(cursor);
            }
        }
        None
    }
}

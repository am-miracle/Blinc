//! Subtree rebuild + maintenance flow.
//!
//! Five concerns:
//!
//! - **Hash-driven rebuild**: `rebuild_changed_subtrees` /
//!   `rebuild_changed_subtrees_boxed` walk the tree comparing the
//!   stored `DivHash` per node against the rebuilt element tree.
//!   When a node's children count changes, they hand off to
//!   `rebuild_children_in_place`; otherwise they recurse to refine
//!   the diff.
//! - **Top-level subtree replace**: `rebuild_children` is the
//!   user-facing entry point that wipes a node's children, builds
//!   the new one from an `ElementBuilder`, registers parent/index
//!   metadata, and re-applies the stylesheet base styles for the
//!   subtree.
//! - **Removal**: `remove_subtree_nodes` walks a subtree DFS and
//!   deletes the per-node `RenderTree` state (render_nodes, hashes,
//!   bounds caches, transitions, scroll/handler/storage records).
//!   Used by every rebuild/replace path that needs to clean up
//!   before re-adding.
//! - **Stateful-driven rebuilds**: `process_pending_subtree_rebuilds`
//!   drains the queue stamped by `crate::stateful` whenever a
//!   `Stateful` widget's deps change, dispatches each entry to either
//!   the props-only fast path or a structural rebuild, and re-applies
//!   stylesheet base styles for the rebuilt subtrees.
//! - **Props-only update**: `update_subtree_props_recursive` /
//!   `update_subtree_props_from_builder` carry through a state
//!   change when the layout shape didn't change — re-derive
//!   `RenderProps` from the new builder, write back, recurse.

use crate::diff::DivHash;
use crate::div::ElementBuilder;
use crate::tree::LayoutNodeId;

use super::super::RenderTree;

impl RenderTree {
    /// Rebuild subtrees for nodes with changed children
    ///
    /// This walks the tree comparing stored hashes with the new element tree.
    /// When it finds a node whose children have changed (different count),
    /// it rebuilds that subtree in place.
    pub(crate) fn rebuild_changed_subtrees<E: ElementBuilder>(
        &mut self,
        element: &E,
        node_id: LayoutNodeId,
    ) {
        let child_node_ids = self.layout_tree.children(node_id);
        let child_builders = element.children_builders();

        // Check if children count changed - rebuild children of this node
        if child_node_ids.len() != child_builders.len() {
            self.rebuild_children_in_place(node_id, child_builders);
            return;
        }

        // Same child count - check each child for deeper changes
        for (child_builder, &child_node_id) in child_builders.iter().zip(child_node_ids.iter()) {
            // Get stored hash for this child
            if let Some(&(_, stored_tree_hash)) = self.node_hashes.get(&child_node_id) {
                let new_tree_hash = DivHash::compute_element_tree(child_builder.as_ref());
                if stored_tree_hash != new_tree_hash {
                    // Child's subtree changed - check if it's the child count or deeper changes
                    let child_children_count = self.layout_tree.children(child_node_id).len();
                    let new_children_count = child_builder.children_builders().len();

                    if child_children_count != new_children_count {
                        // This child's children changed - rebuild its children
                        self.rebuild_children_in_place(
                            child_node_id,
                            child_builder.children_builders(),
                        );
                    } else {
                        // Recurse to find deeper changes
                        self.rebuild_changed_subtrees_boxed(child_builder.as_ref(), child_node_id);
                    }
                }
            }
        }
    }

    /// Rebuild subtrees for boxed element builder
    pub(crate) fn rebuild_changed_subtrees_boxed(
        &mut self,
        element: &dyn ElementBuilder,
        node_id: LayoutNodeId,
    ) {
        let child_node_ids = self.layout_tree.children(node_id);
        let child_builders = element.children_builders();

        if child_node_ids.len() != child_builders.len() {
            self.rebuild_children_in_place(node_id, child_builders);
            return;
        }

        for (child_builder, &child_node_id) in child_builders.iter().zip(child_node_ids.iter()) {
            if let Some(&(_, stored_tree_hash)) = self.node_hashes.get(&child_node_id) {
                let new_tree_hash = DivHash::compute_element_tree(child_builder.as_ref());
                if stored_tree_hash != new_tree_hash {
                    let child_children_count = self.layout_tree.children(child_node_id).len();
                    let new_children_count = child_builder.children_builders().len();

                    if child_children_count != new_children_count {
                        self.rebuild_children_in_place(
                            child_node_id,
                            child_builder.children_builders(),
                        );
                    } else {
                        self.rebuild_changed_subtrees_boxed(child_builder.as_ref(), child_node_id);
                    }
                }
            }
        }
    }

    /// Rebuild only the children of a specific node
    ///
    /// This is used for incremental updates when a stateful element's
    /// dependencies change. Instead of rebuilding the entire tree,
    /// we only rebuild the affected subtree.
    ///
    /// # Arguments
    /// * `parent_id` - The node whose children should be rebuilt
    /// * `new_child` - The new child element builder
    ///
    /// # Returns
    /// The ID of the new child node
    pub fn rebuild_children<E: ElementBuilder>(
        &mut self,
        parent_id: LayoutNodeId,
        new_child: &E,
    ) -> LayoutNodeId {
        // The rebuild registers fresh handlers + applies stylesheet
        // base styles for the new subtree. Invalidate the
        // bare-mouse-move pipeline cache so the next mouse-move
        // re-derives the early-return predicate.
        self.invalidate_mouse_move_pipeline_cache();

        // 1. Remove old children from layout tree and render nodes
        let old_children = self.layout_tree.children(parent_id);
        for child_id in &old_children {
            self.remove_subtree_nodes(*child_id);
        }
        self.layout_tree.clear_children(parent_id);

        // 2. Build the new child element into the layout tree
        let new_child_id = new_child.build(&mut self.layout_tree);

        // 3. Add the new child to the parent
        self.layout_tree.add_child(parent_id, new_child_id);

        // 4. Pre-register element ids for the new subtree BEFORE
        // mint so `widget_key` is consistent across mint passes —
        // see `build_element` for the full rationale.
        self.register_element_ids_walk(new_child, new_child_id);

        // 5. Mint stable ids over the updated tree BEFORE collect.
        // Handler registration in collect_render_props (Phase 3)
        // looks up `self.stable_id_or_warn(node_id)` — without this
        // mint, the new subtree's nodes have no stable id yet and
        // the warn fires per node.
        self.build_generation = self.build_generation.wrapping_add(1);
        self.mint_stable_ids_walk();

        // 5. Collect render props for the new subtree, then run the
        // standard post-build housekeeping.
        self.collect_render_props(new_child, new_child_id);
        self.auto_fill_animation_stable_keys();
        self.sweep_stale_handlers();

        new_child_id
    }

    /// Remove render nodes for a subtree (but don't touch layout tree)
    pub(crate) fn remove_subtree_nodes(&mut self, node_id: LayoutNodeId) {
        // Remove children first
        let children = self.layout_tree.children(node_id);
        for child_id in children {
            self.remove_subtree_nodes(child_id);
        }

        // Remove this node's render data. Handler registry removal
        // uses the stable id (mapping looked up before we drop the
        // mapping below) so the registry stays in sync.
        let stable_for_remove = self.stable_id(node_id);
        self.render_nodes.swap_remove(&node_id);
        if let Some(stable) = stable_for_remove {
            self.handler_registry.remove(stable);
        }
        self.node_states.remove(&node_id);
        self.scroll_offsets.remove(&node_id);
        self.scroll_physics.remove(&node_id);
        self.scroll_refs.remove(&node_id);
        // Unregister from element registry (removes by node_id)
        self.element_registry.unregister(node_id);
        // Remove layout animation config (but keep stable-key animations running)
        self.layout_animation_configs.remove(&node_id);
        self.layout_animations.remove(&node_id);
        self.previous_bounds.remove(&node_id);

        // Remove CSS state tracking data (prevents accumulation across rebuilds)
        self.base_styles.remove(&node_id);
        self.base_taffy_styles.remove(&node_id);
        self.node_hashes.remove(&node_id);
        self.layout_bounds_storages.remove(&node_id);
        self.animated_render_bounds.remove(&node_id);
        self.motion_bindings.remove(&node_id);
        self.hover_css_animations.remove(&node_id);
        self.complex_state_affected.remove(&node_id);

        // Remove CSS animations/transitions for this node from the
        // shared store. The store is now stable-keyed (Phase 5), so
        // we look up the stable id before dropping the mapping
        // below. Skipping this when there's no mapping is safe — a
        // node without a stable id never had a chance to register
        // CSS animations through the stable-keyed path.
        if let Some(stable) = self.stable_id(node_id) {
            if let Ok(mut store) = self.css_anim_store.lock() {
                store.animations.remove(&stable);
                store.transitions.remove(&stable);
            }
        }

        // Drop the stable-id mapping for this layout node. The
        // post-rebuild `mint_stable_ids_walk` will repopulate
        // mappings for surviving / new nodes; this prevents stale
        // entries from lingering between the remove call and the
        // re-mint (mostly defensive — the mint walk would overwrite
        // anyway, but removed-and-not-re-added nodes would otherwise
        // hold a forwarding entry to a freed slotmap key).
        if let Some(stable) = self.layout_to_stable.remove(&node_id) {
            self.stable_to_layout.remove(&stable);
        }
    }

    /// Process all pending subtree rebuilds
    ///
    /// This is called by the windowed app after processing events.
    /// It applies queued child rebuilds without rebuilding the entire tree.
    /// Process pending subtree rebuilds
    ///
    /// Returns true if any rebuild requires layout recomputation.
    /// Visual-only rebuilds (hover/press) return false.
    ///
    /// Processes only rebuilds for nodes that still exist in this tree.
    /// Rebuilds for nodes removed by an earlier pending rebuild are stale and
    /// are dropped; otherwise the global queue stays non-empty forever and the
    /// app keeps requesting redraws while idle.
    pub fn process_pending_subtree_rebuilds(&mut self) -> bool {
        let pending = crate::stateful::take_pending_subtree_rebuilds();
        if pending.is_empty() {
            return false;
        }

        // Subtree rebuilds register/unregister handlers and may apply
        // CSS overrides that change cursor styles. Invalidate the
        // bare-mouse-move pipeline cache so the next mouse-move
        // re-derives the early-return predicate.
        self.invalidate_mouse_move_pipeline_cache();

        tracing::debug!("Processing {} pending subtree rebuilds", pending.len());

        let mut needs_layout = false;
        let mut stale_rebuilds = 0usize;
        let mut superseded_rebuilds = 0usize;
        let structural_rebuilds_by_node: std::collections::HashMap<LayoutNodeId, usize> = pending
            .iter()
            .enumerate()
            .filter_map(|(idx, rebuild)| rebuild.needs_layout.then_some((rebuild.parent_id, idx)))
            .collect();

        for (idx, rebuild) in pending.into_iter().enumerate() {
            // Skip stale rebuilds. This can happen when multiple statefuls queue
            // work in one input cycle and a parent subtree rebuild removes a child
            // that also queued its own hover/press refresh.
            if !self.layout_tree.node_exists(rebuild.parent_id) {
                tracing::debug!(
                    "Subtree rebuild: node {:?} no longer exists, dropping stale rebuild",
                    rebuild.parent_id
                );
                stale_rebuilds += 1;
                continue;
            }

            // Drop work that will be overwritten by a pending structural rebuild.
            // Navigation clicks often queue button visual state updates and an
            // outlet replacement in the same event turn. Processing descendant
            // updates first is wasted work, and on slower Linux machines that can
            // make a simple route change feel sticky.
            if let Some(&structural_idx) = structural_rebuilds_by_node.get(&rebuild.parent_id) {
                if idx < structural_idx {
                    tracing::debug!(
                        "Subtree rebuild: node {:?} superseded by later structural rebuild",
                        rebuild.parent_id
                    );
                    superseded_rebuilds += 1;
                    continue;
                }
            }
            if self
                .layout_tree
                .ancestors(rebuild.parent_id)
                .iter()
                .any(|ancestor| structural_rebuilds_by_node.contains_key(ancestor))
            {
                tracing::debug!(
                    "Subtree rebuild: node {:?} superseded by pending ancestor rebuild",
                    rebuild.parent_id
                );
                superseded_rebuilds += 1;
                continue;
            }

            tracing::debug!(
                "Subtree rebuild: processing node {:?}, needs_layout={}",
                rebuild.parent_id,
                rebuild.needs_layout
            );
            if rebuild.needs_layout {
                // Full structural rebuild - remove old children and build new ones
                needs_layout = true;

                // Update the parent node's own render props AND layout style
                // This is critical for overlay layer where size changes from 0x0 to full viewport
                if let Some(render_node) = self.render_nodes.get_mut(&rebuild.parent_id) {
                    let mut new_props = rebuild.new_child.render_props();
                    new_props.node_id = Some(rebuild.parent_id);
                    new_props.motion = render_node.props.motion.clone();
                    render_node.props = new_props;
                }
                // Update parent node's CSS class registrations so that
                // apply_stylesheet_base_styles_for_subtree matches the current
                // classes (e.g., cn-checkbox--checked added/removed on toggle).
                let parent_classes = rebuild.new_child.element_classes();
                if !parent_classes.is_empty() {
                    self.element_registry
                        .register_classes(rebuild.parent_id, parent_classes.to_vec());
                } else {
                    self.element_registry.clear_classes(rebuild.parent_id);
                }
                self.base_styles.remove(&rebuild.parent_id);
                // Also update the taffy layout style (width, height, padding, etc.)
                if let Some(style) = rebuild.new_child.layout_style() {
                    self.layout_tree.set_style(rebuild.parent_id, style.clone());
                }

                // Re-register scroll_physics and event_handlers for the parent node.
                // Without this, Stateful containers with overflow_y_scroll() lose their
                // scroll state during rebuilds because only children get collect_render_props_boxed.
                if let Some(physics) = rebuild.new_child.scroll_physics() {
                    if let Some(scheduler) = self.animations.upgrade() {
                        physics.lock().unwrap().set_scheduler(&scheduler);
                    }
                    self.scroll_physics.insert(rebuild.parent_id, physics);
                    if rebuild.new_child.viewport_cull() {
                        self.viewport_cull_scrolls.insert(rebuild.parent_id);
                    }
                }
                {
                    let handlers = rebuild.new_child.event_handlers();
                    if !handlers.is_empty() {
                        let stable_id = self.stable_id_or_warn(rebuild.parent_id);
                        self.handler_registry.register(stable_id, handlers.clone());
                    }
                }

                // Always remove old children first (even if new children is empty)
                // This fixes the bug where SVG checkmarks would persist after unchecking
                let old_children = self.layout_tree.children(rebuild.parent_id);
                for child_id in &old_children {
                    self.remove_subtree_nodes(*child_id);
                }
                self.layout_tree.clear_children(rebuild.parent_id);

                // Two-phase: build all new layout nodes first, then
                // mint stable ids once, then collect. Collect must
                // run with stable ids available so handlers /
                // physics / motion bindings register against stable
                // keys — otherwise the registry entries don't
                // survive the next rebuild.
                let children = rebuild.new_child.children_builders();
                let mut built: Vec<LayoutNodeId> = Vec::with_capacity(children.len());
                for child in children {
                    let child_id = child.build(&mut self.layout_tree);
                    self.layout_tree.add_child(rebuild.parent_id, child_id);
                    built.push(child_id);
                }

                // Pre-register element ids for the new subtree
                // BEFORE mint so `widget_key` is read consistently
                // on every mint pass. Without this, mint derives
                // each `.id()`'d descendant's stable id with
                // `widget_key=None` the first time (registry not
                // yet populated) but `widget_key=Some(...)` on
                // every subsequent mint — descendants' stable ids
                // shift and previously-registered handlers go
                // orphaned. See `build_element` for the same fix
                // at initial build.
                for (child, child_id) in children.iter().zip(built.iter()) {
                    self.register_element_ids_walk(child.as_ref(), *child_id);
                }

                // Mint stable ids over the now-complete tree before
                // collect runs (collect inserts handlers etc.).
                self.build_generation = self.build_generation.wrapping_add(1);
                self.mint_stable_ids_walk();

                for (child, child_id) in children.iter().zip(built.iter()) {
                    self.collect_render_props_boxed(child.as_ref(), *child_id);
                }

                self.auto_fill_animation_stable_keys();
                self.sweep_stale_handlers();

                // Apply CSS base styles (class/complex selectors) to new subtree nodes.
                // collect_render_props_boxed only applies #id styles; class-based
                // styles are applied by apply_stylesheet_base_styles() which only
                // runs at full tree creation. Without this, new children from
                // stateful rebuilds lose CSS class styles (border-radius, etc.).
                self.apply_stylesheet_base_styles_for_subtree(rebuild.parent_id);
            } else {
                // Visual-only update - just update render props of existing children
                // Don't remove/rebuild, just walk the tree and update props
                self.update_subtree_props_recursive(rebuild.parent_id, &rebuild.new_child);
            }
        }

        if stale_rebuilds > 0 {
            tracing::debug!("Dropped {} stale subtree rebuild(s)", stale_rebuilds);
        }
        if superseded_rebuilds > 0 {
            tracing::debug!(
                "Dropped {} superseded subtree rebuild(s)",
                superseded_rebuilds
            );
        }

        // Mint already ran per subtree-rebuild above, between layout
        // build and collect. No additional walk needed here.

        needs_layout
    }

    /// Recursively update render props for existing children without rebuilding
    ///
    /// This walks the existing layout tree children alongside the new element definition
    /// and updates render props for matching nodes (by position in child order).
    fn update_subtree_props_recursive(
        &mut self,
        parent_id: LayoutNodeId,
        new_element: &crate::div::Div,
    ) {
        self.update_subtree_props_from_builder(parent_id, new_element);
    }

    /// Update subtree props from a generic ElementBuilder (for recursion)
    ///
    /// Uses full replacement (not merge) so that properties cleared back to defaults
    /// (e.g. transform removed on drag end) are properly reflected. Preserves node_id
    /// and motion which are not set by builders.
    fn update_subtree_props_from_builder(
        &mut self,
        parent_id: LayoutNodeId,
        new_element: &dyn crate::div::ElementBuilder,
    ) {
        let existing_children = self.layout_tree.children(parent_id);
        let new_children = new_element.children_builders();

        for (i, child_id) in existing_children.iter().enumerate() {
            if let Some(new_child) = new_children.get(i) {
                // Full replace of visual props, preserving node_id and motion
                let mut new_props = new_child.render_props();
                if let Some(render_node) = self.render_nodes.get_mut(child_id) {
                    new_props.node_id = render_node.props.node_id;
                    new_props.motion = render_node.props.motion.clone();
                    render_node.props = new_props;
                    // Also update element_type (SVG tint, text content, image data, etc.)
                    // Without this, visual-only rebuilds leave stale element data.
                    render_node.element_type =
                        Self::determine_element_type_boxed(new_child.as_ref());
                }

                // Re-register event handlers from the new element builder.
                // During visual-only rebuilds the tree structure doesn't change,
                // but callbacks may capture new closure state that needs updating.
                if let Some(handlers) = new_child.event_handlers() {
                    let stable_id = self.stable_id_or_warn(*child_id);
                    self.handler_registry.register(stable_id, handlers.clone());
                }

                // Update CSS class registrations so apply_stylesheet_base_styles_for_subtree
                // uses the current classes (not stale ones from the previous build).
                // Without this, adding/removing classes (e.g. cn-sidebar-item--active)
                // wouldn't take effect during visual-only rebuilds.
                let new_classes = new_child.element_classes();
                let old_classes = self.element_registry.get_classes(*child_id);
                let classes_changed = new_classes != old_classes.as_deref().unwrap_or(&[]);
                if !new_classes.is_empty() {
                    self.element_registry
                        .register_classes(*child_id, new_classes.to_vec());
                } else {
                    self.element_registry.clear_classes(*child_id);
                }
                // Invalidate base_styles cache when classes change so that
                // apply_complex_selector_styles resets to the correct base
                // (e.g., node gaining --active needs its new base to include that)
                if classes_changed {
                    self.base_styles.remove(child_id);
                }

                // Recursively update grandchildren
                if !new_child.children_builders().is_empty() {
                    self.update_subtree_props_from_builder(*child_id, new_child.as_ref());
                }
            }
        }

        // Re-apply CSS base styles since the full replace cleared them
        self.apply_stylesheet_base_styles_for_subtree(parent_id);
    }
}

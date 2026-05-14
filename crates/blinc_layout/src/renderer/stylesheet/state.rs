//! State-driven stylesheet application: per-node and per-frame.
//!
//! Three driver methods:
//!
//! - `apply_state_styles` — single-node entry point. Resets the node
//!   to its `base_styles` snapshot, re-applies the base CSS rule, and
//!   layers `:hover` / `:active` / `:focus` rules in precedence order.
//!   Captures a before-snapshot when a `transition:` exists so the
//!   diff against the after-snapshot can start CSS transitions.
//!   Mirrors the same flow into the taffy `Style` for layout
//!   transitions.
//! - `apply_stylesheet_state_styles` — per-frame fan-out. For every
//!   registered element, reads its current `is_hovered` / `is_pressed`
//!   / `is_focused` from the `EventRouter`, calls `apply_state_styles`,
//!   and starts/stops the matching `:hover`-scoped CSS animation. Then
//!   delegates to `apply_complex_selector_styles` and
//!   `apply_svg_tag_styles` for class- and SVG-tag rules.
//! - `apply_pointer_styles` — evaluates `calc(env(pointer-x), ...)`
//!   dynamic properties using the live `PointerQueryState`. Resets the
//!   transform back to base before reapplying transform-derived
//!   dynamics so successive frames don't compound.
//!
//! `css_has_visible_transitions` lives here too because the "any
//! state still in flight?" check is what gates the frame-loop redraw
//! after this pass writes new transitions.

use std::collections::HashSet;

use crate::css_parser::ElementState;
use crate::tree::LayoutNodeId;

use super::super::RenderTree;

impl RenderTree {
    /// Apply state-specific styles from the stylesheet to a node.
    ///
    /// This is called when a node's interaction state changes (hover, pressed, focused).
    /// It looks up the node's string ID and applies any matching state styles.
    ///
    /// # Arguments
    /// * `node_id` - The node whose state changed
    /// * `hovered` - Whether the node is currently hovered
    /// * `pressed` - Whether the node is currently pressed
    /// * `focused` - Whether the node is currently focused
    ///
    /// # Returns
    /// `true` if styles were applied, `false` if no stylesheet or no matching styles
    pub fn apply_state_styles(
        &mut self,
        node_id: LayoutNodeId,
        hovered: bool,
        pressed: bool,
        focused: bool,
    ) -> bool {
        // Early return if no stylesheet
        let stylesheet = match &self.stylesheet {
            Some(s) => s.clone(),
            None => return false,
        };

        // Look up the node's string ID from the registry
        let element_id = match self.element_registry.get_id(node_id) {
            Some(id) => id,
            None => return false, // Node has no ID, can't apply stylesheet styles
        };

        // Get or store base style for this node
        if !self.base_styles.contains_key(&node_id) {
            if let Some(render_node) = self.render_nodes.get(&node_id) {
                self.base_styles.insert(node_id, render_node.props.clone());
            }
        }
        // Get or store base taffy style for layout transition support
        if !self.base_taffy_styles.contains_key(&node_id) {
            if let Some(style) = self.layout_tree.get_style(node_id) {
                self.base_taffy_styles.insert(node_id, style);
            }
        }

        // Start with base style
        let base_props = match self.base_styles.get(&node_id) {
            Some(props) => props.clone(),
            None => return false,
        };

        // Check if this element has transitions defined
        let transition_set = stylesheet
            .get(&element_id)
            .and_then(|s| s.transition.clone());

        // Snapshot before-props for transition detection (visual + layout).
        // Uses snapshot_before_keyframe_properties to avoid QR decomposition
        // drift on transform fields when an active transition exists.
        let before_kp = if transition_set.is_some() {
            self.snapshot_before_keyframe_properties(node_id)
        } else {
            None
        };

        // Apply state-specific styles in order of precedence
        let mut applied = false;
        let render_node = match self.render_nodes.get_mut(&node_id) {
            Some(node) => node,
            None => return false,
        };

        // Reset to base style first
        render_node.props = base_props;

        // Reset taffy style to base before applying state layout overrides
        if let Some(base_taffy) = self.base_taffy_styles.get(&node_id) {
            self.layout_tree.set_style(node_id, base_taffy.clone());
        }

        // Apply base stylesheet style (if any)
        if let Some(base_style) = stylesheet.get(&element_id) {
            Self::apply_element_style_to_props(&mut render_node.props, base_style);
            if base_style.has_layout_props() {
                if let Some(mut taffy_style) = self.layout_tree.get_style(node_id) {
                    Self::apply_element_style_to_taffy(&mut taffy_style, base_style);
                    self.layout_tree.set_style(node_id, taffy_style);
                }
            }
            applied = true;
        }

        // Apply hover style
        if hovered {
            if let Some(hover_style) = stylesheet.get_with_state(&element_id, ElementState::Hover) {
                Self::apply_element_style_to_props(&mut render_node.props, hover_style);
                if hover_style.has_layout_props() {
                    if let Some(mut taffy_style) = self.layout_tree.get_style(node_id) {
                        Self::apply_element_style_to_taffy(&mut taffy_style, hover_style);
                        self.layout_tree.set_style(node_id, taffy_style);
                    }
                }
                applied = true;
            }
        }

        // Apply active/pressed style (takes precedence over hover)
        if pressed {
            if let Some(active_style) = stylesheet.get_with_state(&element_id, ElementState::Active)
            {
                Self::apply_element_style_to_props(&mut render_node.props, active_style);
                if active_style.has_layout_props() {
                    if let Some(mut taffy_style) = self.layout_tree.get_style(node_id) {
                        Self::apply_element_style_to_taffy(&mut taffy_style, active_style);
                        self.layout_tree.set_style(node_id, taffy_style);
                    }
                }
                applied = true;
            }
        }

        // Apply focus style
        if focused {
            if let Some(focus_style) = stylesheet.get_with_state(&element_id, ElementState::Focus) {
                Self::apply_element_style_to_props(&mut render_node.props, focus_style);
                if focus_style.has_layout_props() {
                    if let Some(mut taffy_style) = self.layout_tree.get_style(node_id) {
                        Self::apply_element_style_to_taffy(&mut taffy_style, focus_style);
                        self.layout_tree.set_style(node_id, taffy_style);
                    }
                }
                applied = true;
            }
        }

        // Detect and start transitions for changed properties (visual + layout)
        if let (Some(before_kp), Some(transition_set)) = (before_kp, transition_set) {
            if let Some(after_kp) = self.snapshot_keyframe_properties(node_id) {
                self.detect_and_start_transitions(node_id, &before_kp, &after_kp, &transition_set);
            }
        }

        applied
    }

    /// Visibility-gated counterpart of `css_transitions_empty`.
    /// Returns `true` when there's at least one transition whose
    /// target node was painted in the most recent frame, i.e. the
    /// chain should keep firing for it.
    ///
    /// `painted` is the `LayoutNodeId` set the paint walker
    /// produced; the store is keyed by `StableNodeId` (Phase 5),
    /// so we translate via `layout_to_stable` before the
    /// containment check.
    pub fn css_has_visible_transitions(&self, painted: &HashSet<LayoutNodeId>) -> bool {
        let painted_stable: HashSet<crate::tree::StableNodeId> =
            painted.iter().filter_map(|n| self.stable_id(*n)).collect();
        let store = self.css_anim_store.lock().unwrap();
        store.transitions.keys().any(|s| painted_stable.contains(s))
    }

    /// Apply stylesheet state styles based on EventRouter state.
    ///
    /// This should be called after mouse events to update styles for nodes
    /// whose interaction state has changed. It applies `:hover`, `:active`,
    /// and `:focus` styles from the stylesheet.
    ///
    /// # Arguments
    /// * `router` - The event router containing current interaction state
    ///
    /// # Returns
    /// `true` if any styles were applied, `false` otherwise
    pub fn apply_stylesheet_state_styles(
        &mut self,
        router: &crate::event_router::EventRouter,
    ) -> bool {
        // Early return if no stylesheet
        if self.stylesheet.is_none() {
            return false;
        }

        // State-style apply can change `props.cursor` via `:hover`/
        // `:active`/`:focus` rules, so the bare-mouse-move cache may
        // be stale. Invalidate eagerly — the cache recomputes on the
        // next read.
        self.invalidate_mouse_move_pipeline_cache();

        let mut any_applied = false;

        // Get all registered element IDs and their node IDs
        let registered_ids: Vec<(String, LayoutNodeId)> = self
            .element_registry
            .all_ids()
            .into_iter()
            .filter_map(|id| self.element_registry.get(&id).map(|node_id| (id, node_id)))
            .collect();

        // Apply state styles for each registered element
        for (element_id, node_id) in registered_ids {
            // Check if this element has any state styles in the stylesheet
            if !self.has_state_styles(node_id) {
                continue;
            }

            // Get current interaction state from router. `is_pressed`
            // is keyed by stable id so it survives rebuilds; resolve
            // the layout id to a stable id before asking.
            let hovered = router.is_hovered(node_id);
            let pressed = self
                .stable_id(node_id)
                .map(|s| router.is_pressed(s))
                .unwrap_or(false);
            let focused = router.is_focused(node_id);

            // Apply state styles
            if self.apply_state_styles(node_id, hovered, pressed, focused) {
                any_applied = true;
                tracing::trace!(
                    "Applied stylesheet state styles to #{}: hovered={}, pressed={}, focused={}",
                    element_id,
                    hovered,
                    pressed,
                    focused
                );
            }

            // Trigger/stop hover CSS animations
            let stylesheet = self.stylesheet.as_ref().unwrap();
            if hovered && !self.hover_css_animations.contains(&node_id) {
                let has_hover_anim = stylesheet
                    .get_with_state(&element_id, ElementState::Hover)
                    .and_then(|s| s.animation.as_ref())
                    .is_some();
                if has_hover_anim {
                    self.start_css_animation_for_state(node_id, ElementState::Hover);
                    self.hover_css_animations.insert(node_id);
                    any_applied = true;
                }
            } else if !hovered && self.hover_css_animations.remove(&node_id) {
                // Hover left — remove hover animation if no base animation exists
                let base_has_anim = stylesheet
                    .get(&element_id)
                    .and_then(|s| s.animation.as_ref())
                    .is_some();
                if base_has_anim {
                    self.start_css_animation_for_element(node_id);
                } else if let Some(stable) = self.stable_id(node_id) {
                    self.css_anim_store
                        .lock()
                        .unwrap()
                        .animations
                        .remove(&stable);
                }
                any_applied = true;
            }
        }

        // Apply complex selector rules (class selectors, descendant/child combinators, etc.)
        if self.apply_complex_selector_styles(router) {
            any_applied = true;
        }

        // Apply SVG tag-name CSS rules (e.g., `path { fill: red; }`)
        if self.apply_svg_tag_styles(router) {
            any_applied = true;
        }

        any_applied
    }

    /// Evaluate dynamic `calc(env(...))` properties for pointer-tracked elements.
    ///
    /// Called per-frame after `apply_stylesheet_state_styles()` and before rendering.
    /// For each element in the pointer query, collects dynamic properties from the
    /// active stylesheet entries (base + hover/active/focus) and evaluates them with
    /// the current pointer state, writing results directly to RenderProps.
    pub fn apply_pointer_styles(
        &mut self,
        pointer_query: &crate::pointer_query::PointerQueryState,
        router: &crate::event_router::EventRouter,
    ) {
        let stylesheet = match &self.stylesheet {
            Some(s) => s.clone(),
            None => return,
        };

        for (element_id, pointer_state) in pointer_query.iter() {
            let node_id = match self.element_registry.get(element_id) {
                Some(id) => id,
                None => continue,
            };

            // Build CalcContext with pointer env vars
            let mut ctx = crate::calc::CalcContext::default();
            for name in &[
                "pointer-x",
                "pointer-y",
                "pointer-vx",
                "pointer-vy",
                "pointer-speed",
                "pointer-distance",
                "pointer-angle",
                "pointer-inside",
                "pointer-active",
                "pointer-pressure",
                "pointer-touch-count",
                "pointer-hover-duration",
            ] {
                if let Some(val) = pointer_state.resolve_env(name) {
                    ctx.env_vars.insert(name.to_string(), val);
                }
            }

            // Collect dynamic properties from applicable stylesheet entries
            // (base first, then state overrides in precedence order)
            let mut dynamic_props: Vec<&crate::element_style::DynamicProperty> = Vec::new();

            // Base style
            if let Some(base) = stylesheet.get(element_id) {
                if let Some(ref dps) = base.dynamic_properties {
                    dynamic_props.extend(dps.iter());
                }
            }

            // Hover state (overrides base)
            if router.is_hovered(node_id) {
                if let Some(hover) = stylesheet.get_with_state(element_id, ElementState::Hover) {
                    if let Some(ref dps) = hover.dynamic_properties {
                        dynamic_props.extend(dps.iter());
                    }
                }
            }

            // Active/pressed state (overrides hover) — router stores
            // pressed by stable id; resolve the layout id first.
            let pressed_here = self
                .stable_id(node_id)
                .map(|s| router.is_pressed(s))
                .unwrap_or(false);
            if pressed_here {
                if let Some(active) = stylesheet.get_with_state(element_id, ElementState::Active) {
                    if let Some(ref dps) = active.dynamic_properties {
                        dynamic_props.extend(dps.iter());
                    }
                }
            }

            // Focus state
            if router.is_focused(node_id) {
                if let Some(focus) = stylesheet.get_with_state(element_id, ElementState::Focus) {
                    if let Some(ref dps) = focus.dynamic_properties {
                        dynamic_props.extend(dps.iter());
                    }
                }
            }

            if dynamic_props.is_empty() {
                continue;
            }

            // Evaluate and apply to RenderProps
            if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                // If any dynamic properties are transform-related (SkewX, SkewY, Rotate, etc.),
                // reset props.transform to its base value first to prevent frame-compounding.
                // Without this, compose_affine() would accumulate onto the previous frame's
                // dynamic transform, causing exponential growth.
                let has_transform_dynamics = dynamic_props.iter().any(|dp| dp.is_transform());
                if has_transform_dynamics {
                    let base_transform = self
                        .base_styles
                        .get(&node_id)
                        .and_then(|base| base.transform.clone());
                    render_node.props.transform = base_transform;
                }

                for dp in &dynamic_props {
                    dp.apply(&mut render_node.props, &ctx);
                }
            }
        }
    }
}

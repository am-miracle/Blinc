//! Apply base (non-state) stylesheet styles to a tree or subtree.
//!
//! Two driver methods plus a small helper:
//!
//! - `apply_stylesheet_base_styles` — runs once after the stylesheet
//!   is set on a freshly built tree. Walks complex rules in
//!   ascending-specificity order (type < class < id-shaped chains),
//!   then applies simple `#id` rules last so they win, then handles
//!   SVG tag rules and propagates inherited text properties from
//!   parent to child.
//! - `apply_stylesheet_base_styles_for_subtree` — same flow but
//!   restricted to a subtree, called by `process_pending_subtree_rebuilds`
//!   so newly-built children pick up class- and id-based base styles
//!   that `collect_render_props_boxed` only resolves for `#id`.
//! - `collect_subtree_ids` — DFS into the layout tree to gather all
//!   descendant node ids; private to this file.
//!
//! Both passes also eagerly seed `base_styles` for nodes matching a
//! state-rule class so the lazy save inside
//! `apply_complex_selector_styles` doesn't capture
//! Stateful-rebuilt-into-hover props as the "base".

use std::collections::{HashMap, HashSet};

use crate::tree::LayoutNodeId;

use super::super::{ElementType, RenderTree};

impl RenderTree {
    /// Apply base stylesheet styles to all registered elements.
    ///
    /// This must be called after `set_stylesheet_arc()` when the stylesheet
    /// was set AFTER tree construction. During tree build, `collect_render_props`
    /// applies base styles if the stylesheet is already set. But when the stylesheet
    /// is set after `from_element_with_registry()`, the base styles (background,
    /// border-radius, opacity, etc.) are missing. This method fixes that by
    /// iterating all registered elements and applying their base CSS styles.
    pub fn apply_stylesheet_base_styles(&mut self) {
        let stylesheet = match &self.stylesheet {
            Some(s) => s.clone(),
            None => return,
        };

        // Base-style apply can change `props.cursor` on any node, so
        // invalidate the bare-mouse-move pipeline cache to force a
        // recompute on next read.
        self.invalidate_mouse_move_pipeline_cache();

        // CSS specificity order: type(0,0,1) < class(0,1,0) < id(1,0,0)
        // Apply complex base rules FIRST (lower specificity: type, class selectors)
        // sorted by ascending specificity so higher-specificity rules overwrite lower.
        // Then apply simple ID rules LAST (highest specificity, always override).
        let complex_rules = stylesheet.complex_rules();
        if !complex_rules.is_empty() {
            let empty_set = HashSet::new();

            // Collect non-state rules and sort by specificity (ascending)
            let mut base_rules: Vec<&(
                crate::css_parser::ComplexSelector,
                crate::element_style::ElementStyle,
            )> = complex_rules
                .iter()
                .filter(|(selector, _)| !selector.has_state())
                .collect();
            base_rules.sort_by_key(|(selector, _)| Self::selector_specificity(selector));

            // Build inverted class index once (single lock acquisition) for O(1) lookups
            let class_to_nodes = self.element_registry.class_to_nodes_index();

            for (selector, style) in base_rules {
                // Fast path: simple `.class` selectors use inverted index — O(matched_nodes)
                if let Some(class_name) = selector.simple_class_name() {
                    if let Some(node_ids) = class_to_nodes.get(class_name) {
                        for &node_id in node_ids {
                            if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                                Self::apply_element_style_to_props(&mut render_node.props, style);
                            }
                        }
                    }
                    continue;
                }

                // Slow path: complex selectors (combinators, structural pseudos) — O(all_nodes)
                let all_node_ids: Vec<LayoutNodeId> = self.render_nodes.keys().copied().collect();
                for &node_id in &all_node_ids {
                    if self
                        .complex_selector_matches(selector, node_id, &empty_set, &empty_set, None)
                    {
                        if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                            Self::apply_element_style_to_props(&mut render_node.props, style);
                        }
                    }
                }
            }

            // Eagerly save base_styles for nodes matching classes that also have
            // state rules (:hover, :active, :focus). This prevents the lazy save
            // in apply_complex_selector_styles() from capturing contaminated props
            // (e.g. inline hover backgrounds set by Stateful component rebuilds).
            // Only save if not already present — this function runs for the entire
            // tree and nodes outside a rebuild may still carry hover/active styles.
            let state_class_names: HashSet<&str> = complex_rules
                .iter()
                .filter(|(sel, _)| sel.has_state())
                .filter_map(|(sel, _)| sel.class_name_with_state())
                .collect();
            for class_name in &state_class_names {
                if let Some(node_ids) = class_to_nodes.get(*class_name) {
                    for &node_id in node_ids {
                        if !self.base_styles.contains_key(&node_id) {
                            if let Some(render_node) = self.render_nodes.get(&node_id) {
                                self.base_styles.insert(node_id, render_node.props.clone());
                            }
                        }
                    }
                }
            }
        }

        // Apply simple ID rules LAST — #id has highest specificity and overrides
        // type/class selectors applied above.
        let registered_ids: Vec<(String, LayoutNodeId)> = self
            .element_registry
            .all_ids()
            .into_iter()
            .filter_map(|id| self.element_registry.get(&id).map(|node_id| (id, node_id)))
            .collect();

        for (element_id, node_id) in &registered_ids {
            if let Some(base_style) = stylesheet.get(element_id) {
                if let Some(render_node) = self.render_nodes.get_mut(node_id) {
                    Self::apply_element_style_to_props(&mut render_node.props, base_style);
                }
            }
        }

        // Sync CSS text-align into baked TextData for text nodes.
        // text-align may have been set by CSS above but TextData was built before CSS.
        for render_node in self.render_nodes.values_mut() {
            if let Some(ta) = render_node.props.text_align {
                if let ElementType::Text(ref mut text_data) = render_node.element_type {
                    text_data.align = ta;
                }
            }
        }

        // Update Stateful base_render_props with CSS-applied values.
        // This ensures that state changes (hover, press) start from CSS-enhanced
        // base props, preserving CSS overrides like border-radius across state changes.
        for (&node_id, render_node) in &self.render_nodes {
            if crate::stateful::has_stateful_base_updater(node_id) {
                crate::stateful::update_stateful_base_props(node_id, render_node.props.clone());
            }
        }

        // Apply base (non-state) SVG tag-name rules to SVG nodes
        let svg_tag_rules = stylesheet.svg_tag_rules();
        if !svg_tag_rules.is_empty() {
            let svg_nodes: Vec<LayoutNodeId> = self
                .render_nodes
                .keys()
                .copied()
                .filter(|&nid| self.element_registry.get_element_type(nid) == Some("svg"))
                .collect();

            for &svg_node in &svg_nodes {
                let mut tag_styles: HashMap<String, crate::element::SvgTagStyle> = HashMap::new();
                for &(tag_name, ancestor_segments, style) in &svg_tag_rules {
                    // Skip state-dependent rules (handled by apply_svg_tag_styles)
                    let has_state = ancestor_segments.iter().any(|(c, _)| c.has_state());
                    if has_state {
                        continue;
                    }
                    let matches = if ancestor_segments.is_empty() {
                        true
                    } else {
                        // Check if ancestor segments match the SVG node's chain
                        let last_idx = ancestor_segments.len() - 1;
                        let (last_compound, _) = &ancestor_segments[last_idx];
                        if !self.compound_matches(last_compound, svg_node, false, false, false) {
                            false
                        } else if ancestor_segments.len() == 1 {
                            true
                        } else {
                            let mut current_node = svg_node;
                            let mut all_matched = true;
                            for i in (0..last_idx).rev() {
                                let (compound, combinator) = &ancestor_segments[i];
                                let combinator =
                                    combinator.unwrap_or(crate::css_parser::Combinator::Descendant);
                                match combinator {
                                    crate::css_parser::Combinator::Child => {
                                        match self.element_registry.get_parent(current_node) {
                                            Some(parent) => {
                                                if !self.compound_matches(
                                                    compound, parent, false, false, false,
                                                ) {
                                                    all_matched = false;
                                                    break;
                                                }
                                                current_node = parent;
                                            }
                                            None => {
                                                all_matched = false;
                                                break;
                                            }
                                        }
                                    }
                                    crate::css_parser::Combinator::Descendant => {
                                        let ancestors =
                                            self.element_registry.ancestors(current_node);
                                        let mut found = false;
                                        for ancestor in &ancestors {
                                            if self.compound_matches(
                                                compound, *ancestor, false, false, false,
                                            ) {
                                                current_node = *ancestor;
                                                found = true;
                                                break;
                                            }
                                        }
                                        if !found {
                                            all_matched = false;
                                            break;
                                        }
                                    }
                                    _ => {
                                        all_matched = false;
                                        break;
                                    }
                                }
                            }
                            all_matched
                        }
                    };
                    if matches {
                        let entry = tag_styles.entry(tag_name.to_string()).or_default();
                        if let Some(fill) = style.fill {
                            entry.fill = Some([fill.r, fill.g, fill.b, fill.a]);
                        }
                        if let Some(stroke) = style.stroke {
                            entry.stroke = Some([stroke.r, stroke.g, stroke.b, stroke.a]);
                        }
                        if let Some(sw) = style.stroke_width {
                            entry.stroke_width = Some(sw);
                        }
                        if let Some(ref da) = style.stroke_dasharray {
                            entry.stroke_dasharray = Some(da.clone());
                        }
                        if let Some(offset) = style.stroke_dashoffset {
                            entry.stroke_dashoffset = Some(offset);
                        }
                        if let Some(opacity) = style.opacity {
                            entry.opacity = Some(opacity);
                        }
                    }
                }
                if !tag_styles.is_empty() {
                    if let Some(render_node) = self.render_nodes.get_mut(&svg_node) {
                        render_node.props.svg_tag_styles = tag_styles;
                    }
                }
            }
        }

        // Post-pass: propagate inherited text properties (text-decoration, white-space,
        // text-overflow, text-align) from parent to child nodes. This must run AFTER all
        // CSS styles are applied above, because during initial tree construction the
        // stylesheet wasn't set yet and inherit_text_props_from_parent found no parent values.
        let all_node_ids: Vec<LayoutNodeId> = self.render_nodes.keys().copied().collect();
        for node_id in all_node_ids {
            let parent_id = match self.element_registry.get_parent(node_id) {
                Some(id) => id,
                None => continue,
            };
            // Read parent text props (need separate borrow)
            let parent_text_props = self.render_nodes.get(&parent_id).map(|n| {
                (
                    n.props.text_decoration,
                    n.props.text_decoration_color,
                    n.props.text_decoration_thickness,
                    n.props.white_space,
                    n.props.text_overflow,
                    n.props.text_color,
                    n.props.text_align,
                    n.props.fill,
                    n.props.stroke,
                    n.props.stroke_width,
                )
            });
            if let Some((td, td_color, td_thick, ws, to, tc, ta, fill, stroke, stroke_w)) =
                parent_text_props
            {
                if let Some(node) = self.render_nodes.get_mut(&node_id) {
                    if node.props.text_decoration.is_none() {
                        node.props.text_decoration = td;
                    }
                    if node.props.text_decoration_color.is_none() {
                        node.props.text_decoration_color = td_color;
                    }
                    if node.props.text_decoration_thickness.is_none() {
                        node.props.text_decoration_thickness = td_thick;
                    }
                    if node.props.white_space.is_none() {
                        node.props.white_space = ws;
                    }
                    if node.props.text_overflow.is_none() {
                        node.props.text_overflow = to;
                    }
                    if node.props.text_color.is_none() {
                        node.props.text_color = tc;
                    }
                    if node.props.text_align.is_none() {
                        if let Some(ta) = ta {
                            node.props.text_align = Some(ta);
                            // Also update baked TextData.align so rendering uses the
                            // inherited value (TextData is built before CSS post-pass)
                            if let ElementType::Text(ref mut text_data) = node.element_type {
                                text_data.align = ta;
                            }
                        }
                    }
                    // SVG fill/stroke (CSS spec: inherited in SVG)
                    if node.props.fill.is_none() {
                        node.props.fill = fill;
                    }
                    if node.props.stroke.is_none() {
                        node.props.stroke = stroke;
                    }
                    if node.props.stroke_width.is_none() {
                        node.props.stroke_width = stroke_w;
                    }
                }
            }
        }
    }

    /// Apply CSS base styles (class and ID selectors) to a subtree after rebuild.
    ///
    /// Called after `process_pending_subtree_rebuilds` builds new child nodes.
    /// `collect_render_props_boxed` only applies `#id` styles inline; class-based
    /// selectors (`.sort-item`, `.grid-item`, etc.) are resolved by
    /// `apply_stylesheet_base_styles()` which only runs at full tree creation.
    /// This method fills that gap for incrementally rebuilt subtrees.
    pub(crate) fn apply_stylesheet_base_styles_for_subtree(&mut self, parent_id: LayoutNodeId) {
        let stylesheet = match &self.stylesheet {
            Some(s) => s.clone(),
            None => return,
        };

        // Collect all node IDs in the subtree (parent + descendants)
        let mut subtree_nodes = Vec::new();
        self.collect_subtree_ids(parent_id, &mut subtree_nodes);

        if subtree_nodes.is_empty() {
            return;
        }

        // Apply complex base rules (class selectors, combinators) — lower specificity first
        let complex_rules = stylesheet.complex_rules();
        if !complex_rules.is_empty() {
            let empty_set = HashSet::new();

            let mut base_rules: Vec<&(
                crate::css_parser::ComplexSelector,
                crate::element_style::ElementStyle,
            )> = complex_rules
                .iter()
                .filter(|(selector, _)| !selector.has_state())
                .collect();
            base_rules.sort_by_key(|(selector, _)| Self::selector_specificity(selector));

            // Build inverted class index for the subtree nodes
            let class_to_nodes = self.element_registry.class_to_nodes_index();
            // Filter to only subtree nodes for simple class lookups
            let subtree_set: HashSet<LayoutNodeId> = subtree_nodes.iter().copied().collect();

            for (selector, style) in base_rules {
                // Fast path: simple `.class` selectors use inverted index
                if let Some(class_name) = selector.simple_class_name() {
                    if let Some(node_ids) = class_to_nodes.get(class_name) {
                        for &node_id in node_ids {
                            if subtree_set.contains(&node_id) {
                                if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                                    Self::apply_element_style_to_props(
                                        &mut render_node.props,
                                        style,
                                    );
                                }
                            }
                        }
                    }
                    continue;
                }

                // Slow path: complex selectors need full matching
                for &node_id in &subtree_nodes {
                    if self
                        .complex_selector_matches(selector, node_id, &empty_set, &empty_set, None)
                    {
                        if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                            Self::apply_element_style_to_props(&mut render_node.props, style);
                        }
                    }
                }
            }

            // Eagerly save base_styles for subtree nodes matching classes that
            // also have :hover/:active/:focus state rules. This prevents the
            // lazy save in apply_complex_selector_styles() from capturing
            // contaminated props set by Stateful component rebuilds.
            let state_class_names: HashSet<&str> = complex_rules
                .iter()
                .filter(|(sel, _)| sel.has_state())
                .filter_map(|(sel, _)| sel.class_name_with_state())
                .collect();
            for class_name in &state_class_names {
                if let Some(node_ids) = class_to_nodes.get(*class_name) {
                    for &node_id in node_ids {
                        if subtree_set.contains(&node_id) {
                            if let Some(render_node) = self.render_nodes.get(&node_id) {
                                self.base_styles.insert(node_id, render_node.props.clone());
                            }
                        }
                    }
                }
            }
        }

        // Apply simple ID rules (highest specificity, overrides class selectors)
        for &node_id in &subtree_nodes {
            if let Some(element_id) = self.element_registry.get_id(node_id) {
                if let Some(base_style) = stylesheet.get(&element_id) {
                    if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                        Self::apply_element_style_to_props(&mut render_node.props, base_style);
                    }
                }
            }
        }

        // Update Stateful base_render_props for subtree nodes with CSS-applied values
        for &node_id in &subtree_nodes {
            if crate::stateful::has_stateful_base_updater(node_id) {
                if let Some(render_node) = self.render_nodes.get(&node_id) {
                    crate::stateful::update_stateful_base_props(node_id, render_node.props.clone());
                }
            }
        }

        // Propagate inherited text properties (color, text-decoration, etc.)
        // from parent to child nodes within the rebuilt subtree.
        for &node_id in &subtree_nodes {
            let parent_id = match self.element_registry.get_parent(node_id) {
                Some(id) => id,
                None => continue,
            };
            let parent_text_props = self.render_nodes.get(&parent_id).map(|n| {
                (
                    n.props.text_decoration,
                    n.props.text_decoration_color,
                    n.props.text_decoration_thickness,
                    n.props.white_space,
                    n.props.text_overflow,
                    n.props.text_color,
                    n.props.text_align,
                )
            });
            if let Some((td, td_color, td_thick, ws, to, tc, ta)) = parent_text_props {
                if let Some(node) = self.render_nodes.get_mut(&node_id) {
                    if node.props.text_decoration.is_none() {
                        node.props.text_decoration = td;
                    }
                    if node.props.text_decoration_color.is_none() {
                        node.props.text_decoration_color = td_color;
                    }
                    if node.props.text_decoration_thickness.is_none() {
                        node.props.text_decoration_thickness = td_thick;
                    }
                    if node.props.white_space.is_none() {
                        node.props.white_space = ws;
                    }
                    if node.props.text_overflow.is_none() {
                        node.props.text_overflow = to;
                    }
                    if node.props.text_color.is_none() {
                        node.props.text_color = tc;
                    }
                    if node.props.text_align.is_none() {
                        if let Some(ta) = ta {
                            node.props.text_align = Some(ta);
                            if let ElementType::Text(ref mut text_data) = node.element_type {
                                text_data.align = ta;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Collect all node IDs in a subtree (the node itself + all descendants).
    fn collect_subtree_ids(&self, node_id: LayoutNodeId, out: &mut Vec<LayoutNodeId>) {
        out.push(node_id);
        for child_id in self.layout_tree.children(node_id) {
            self.collect_subtree_ids(child_id, out);
        }
    }
}

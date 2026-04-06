//! `rich_text_editor(state, theme, content_width)` — interactive editor.
//!
//! Phase 3: focus, click-to-place cursor, arrow-key navigation (with shift
//! to extend selection), blinking visual cursor. **No editing yet** — keys
//! that produce characters are ignored. Phase 4 wires those up.
//!
//! The factory wraps the read-only renderer in a `Stateful` whose rebuild
//! is triggered by a private `version: State<u32>` signal that the click
//! and key handlers bump. Each rebuild also recomputes the line geometry
//! index so cursor positioning is always against the current document.

use std::sync::Arc;

use blinc_core::context_state::BlincContextState;
use blinc_core::events::event_types;
use blinc_core::Color;
use blinc_core::State;

use crate::div::{div, Div, ElementBuilder};
use crate::stateful::{stateful_with_key, NoState};

use super::cursor::{step_backward, step_forward, DocPosition};
use super::render::{compute_line_geometry, render_document, RichTextTheme};
use super::state::RichTextState;

/// Build an interactive rich text editor element.
///
/// `state` is an externally-owned `RichTextState` that survives across
/// rebuilds. `theme` controls visual style. `content_width` is the
/// pixel width of the column the document will be rendered into — same
/// caveat as [`render_document`].
///
/// The returned element is a `Stateful` so it can be embedded anywhere
/// a normal element fits. Use a parent with explicit width (e.g. inside
/// a centered column) to control the editor's footprint.
pub fn rich_text_editor(
    state: &RichTextState,
    theme: RichTextTheme,
    content_width: f32,
) -> impl ElementBuilder {
    // Per-editor signal that bumps on every state mutation. The Stateful
    // rebuilds whenever this signal changes, which triggers a fresh
    // line-index computation and a new cursor overlay.
    let blinc = BlincContextState::get();
    let version_key = format!("rich_text_editor:{:p}", Arc::as_ptr(state));
    let version: State<u32> = blinc.use_state_keyed(&version_key, || 0);
    let stateful_key = format!("rich_text_editor_root:{:p}", Arc::as_ptr(state));

    // Pre-populate the line index so the very first frame can already
    // hit-test clicks before any state mutation has happened.
    {
        if let Ok(mut data) = state.lock() {
            let geometry = compute_line_geometry(&data.document, &theme, content_width);
            data.set_line_index(geometry);
        }
    }

    // Clones for the various closures
    let state_for_render = Arc::clone(state);
    let state_for_click = Arc::clone(state);
    let state_for_drag = Arc::clone(state);
    let state_for_key = Arc::clone(state);
    let theme_for_render = theme.clone();
    let version_for_click = version.clone();
    let version_for_drag = version.clone();
    let version_for_key = version.clone();

    stateful_with_key::<NoState>(&stateful_key)
        .deps([version.signal_id()])
        .on_state(move |_ctx| {
            // Re-walk geometry against the (possibly mutated) document.
            let mut data = state_for_render.lock().unwrap();
            let geometry = compute_line_geometry(&data.document, &theme_for_render, content_width);
            data.set_line_index(geometry);

            let doc_tree = render_document(&data.document, &theme_for_render, content_width);

            // Cursor + selection overlay (only when focused).
            let mut overlay_children: Vec<Div> = Vec::new();
            if data.focused {
                if let Some((cx, cy, ch)) = data.cursor_geometry() {
                    overlay_children.push(cursor_div(cx, cy, ch, theme_for_render.text));
                }
                // Selection rectangles — one per visual line in the
                // selected range. Phase 3 ships this; Phase 4 will reuse
                // it for actual edit operations.
                if let Some(sel) = data.selection {
                    overlay_children.extend(selection_rects(&data, sel, &theme_for_render));
                }
            }

            // Wrap the document in a relative-positioned container so
            // absolute children (cursor / selection) are positioned
            // against the editor's content rect, not the window.
            let mut root = div().w_full().relative().child(doc_tree);
            for child in overlay_children {
                root = root.child(child);
            }
            root
        })
        .w_full()
        .on_mouse_down(move |ctx| {
            let mut data = state_for_click.lock().unwrap();
            data.focused = true;
            if let Some(pos) = data.position_from_click(ctx.local_x, ctx.local_y) {
                let extend = ctx.shift;
                data.move_cursor(pos, extend);
            }
            drop(data);
            version_for_click.set(version_for_click.get().wrapping_add(1));
        })
        .on_drag(move |ctx| {
            let mut data = state_for_drag.lock().unwrap();
            if !data.focused {
                return;
            }
            // Drag extends selection from anchor to current pointer.
            if let Some(pos) = data.position_from_click(ctx.local_x, ctx.local_y) {
                data.move_cursor(pos, true);
            }
            drop(data);
            version_for_drag.set(version_for_drag.get().wrapping_add(1));
        })
        .on_key_down(move |ctx| {
            let mut data = state_for_key.lock().unwrap();
            if !data.focused {
                return;
            }

            let extend = ctx.shift;
            let mut moved = false;
            match ctx.key_code {
                // Left
                37 => {
                    if let Some(pos) = step_backward(&data.document, data.cursor) {
                        data.move_cursor(pos, extend);
                        moved = true;
                    }
                }
                // Right
                39 => {
                    if let Some(pos) = step_forward(&data.document, data.cursor) {
                        data.move_cursor(pos, extend);
                        moved = true;
                    }
                }
                // Up
                38 => {
                    if let Some(pos) = move_vertical(&data, -1) {
                        data.move_cursor(pos, extend);
                        moved = true;
                    }
                }
                // Down
                40 => {
                    if let Some(pos) = move_vertical(&data, 1) {
                        data.move_cursor(pos, extend);
                        moved = true;
                    }
                }
                // Home — start of current line
                36 => {
                    let pos = home_of(&data);
                    data.move_cursor(pos, extend);
                    moved = true;
                }
                // End — end of current line
                35 => {
                    let pos = end_of(&data);
                    data.move_cursor(pos, extend);
                    moved = true;
                }
                // Escape — blur
                27 => {
                    data.focused = false;
                    moved = true;
                }
                _ => {}
            }
            // Trigger rebuild if anything moved.
            // FOCUS event ensures the stateful FSM stays focused too.
            let _ = event_types::FOCUS;
            drop(data);
            if moved {
                version_for_key.set(version_for_key.get().wrapping_add(1));
            }
        })
}

/// Build the absolute-positioned cursor div at `(x, y)` with height `h`.
fn cursor_div(x: f32, y: f32, h: f32, color: Color) -> Div {
    div().absolute().left(x).top(y).w(2.0).h(h).bg(color)
}

/// Build absolute-positioned selection rectangles for the given selection.
/// One rect per visual line that the selection covers, sized to the
/// width of the selected text on that line.
fn selection_rects(
    data: &super::state::RichTextData,
    sel: super::cursor::Selection,
    theme: &RichTextTheme,
) -> Vec<Div> {
    let (start, end) = sel.ordered();
    let mut rects = Vec::new();
    let highlight = Color::rgba(theme.text.r, theme.text.g, theme.text.b, 0.18);
    for g in &data.line_index {
        // Determine the [start_col_in_line .. end_col_in_line) range for
        // this visual line in the selection.
        let line_chars = g.text.chars().count();
        let line_end_col = g.start.col + line_chars;
        let on_block = g.start.block;
        let on_line = g.start.line;

        // Skip lines fully outside the selection.
        let after_start = (on_block, on_line, line_end_col) >= (start.block, start.line, start.col);
        let before_end = (on_block, on_line, g.start.col) <= (end.block, end.line, end.col);
        if !(after_start && before_end) {
            continue;
        }

        // Compute clamped local columns within this visual line.
        let line_start_pos = (on_block, on_line, g.start.col);
        let line_end_pos = (on_block, on_line, line_end_col);
        let sel_start_pos = (start.block, start.line, start.col);
        let sel_end_pos = (end.block, end.line, end.col);

        let sx_global = sel_start_pos.max(line_start_pos);
        let ex_global = sel_end_pos.min(line_end_pos);
        if sx_global >= ex_global {
            continue;
        }
        let local_start = sx_global.2 - g.start.col;
        let local_end = ex_global.2 - g.start.col;

        // Pixel offsets via measurement.
        let prefix: String = g.text.chars().take(local_start).collect();
        let mid: String = g
            .text
            .chars()
            .skip(local_start)
            .take(local_end - local_start)
            .collect();
        let prefix_w = super::state::measure_width(&prefix, g.font_size, g.weight, g.italic);
        let mid_w = super::state::measure_width(&mid, g.font_size, g.weight, g.italic);
        if mid_w <= 0.0 {
            continue;
        }
        rects.push(
            div()
                .absolute()
                .left(g.x + prefix_w)
                .top(g.y)
                .w(mid_w)
                .h(g.height)
                .bg(highlight),
        );
    }
    rects
}

/// Move the cursor up (-1) or down (+1) one visual line by stepping in
/// the line-geometry index. Preserves the cursor's column where possible.
fn move_vertical(data: &super::state::RichTextData, dir: i32) -> Option<DocPosition> {
    let cursor = data.cursor;
    // Find the cursor's current visual line in the index.
    let current_idx = data.line_index.iter().position(|g| {
        if g.start.block != cursor.block || g.start.line != cursor.line {
            return false;
        }
        let line_chars = g.text.chars().count();
        let end_col = g.start.col + line_chars;
        cursor.col >= g.start.col && cursor.col <= end_col
    })?;
    let target_idx = if dir < 0 {
        current_idx.checked_sub(1)?
    } else {
        let n = current_idx + 1;
        if n >= data.line_index.len() {
            return None;
        }
        n
    };
    let g = &data.line_index[target_idx];
    let local_col = cursor
        .col
        .saturating_sub(data.line_index[current_idx].start.col);
    let target_chars = g.text.chars().count();
    let new_col = g.start.col + local_col.min(target_chars);
    Some(DocPosition::new(g.start.block, g.start.line, new_col))
}

/// Position at the start of the cursor's current visual line.
fn home_of(data: &super::state::RichTextData) -> DocPosition {
    let cursor = data.cursor;
    for g in &data.line_index {
        if g.start.block == cursor.block && g.start.line == cursor.line {
            let line_chars = g.text.chars().count();
            let end_col = g.start.col + line_chars;
            if cursor.col >= g.start.col && cursor.col <= end_col {
                return DocPosition::new(g.start.block, g.start.line, g.start.col);
            }
        }
    }
    cursor
}

/// Position at the end of the cursor's current visual line.
fn end_of(data: &super::state::RichTextData) -> DocPosition {
    let cursor = data.cursor;
    for g in &data.line_index {
        if g.start.block == cursor.block && g.start.line == cursor.line {
            let line_chars = g.text.chars().count();
            let end_col = g.start.col + line_chars;
            if cursor.col >= g.start.col && cursor.col <= end_col {
                return DocPosition::new(g.start.block, g.start.line, end_col);
            }
        }
    }
    cursor
}

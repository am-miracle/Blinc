//! `rich_text_editor(state, theme, content_width)` — interactive editor.
//!
//! Phases 3 + 3.5 + 4 in this file:
//!
//! - Click to place cursor, drag to select, arrow / Home / End nav.
//! - Canvas-based blinking visual cursor (no rebuilds for the blink).
//! - Typing inserts characters at the cursor with the current
//!   `ActiveFormat`; Backspace / Delete / Enter / Shift+Enter all work
//!   as expected. Undo / redo are wired to Cmd-Z / Cmd-Shift-Z and use
//!   a snapshot stack capped at 200 entries.
//!
//! The factory wraps the renderer in a `Stateful` whose rebuild is
//! triggered by a private `version: State<u32>` signal that the click
//! and key handlers bump after every mutation. The Stateful's `on_state`
//! callback re-walks the line geometry index against the current
//! document and emits the cursor / selection overlay.

use std::sync::Arc;

use blinc_core::context_state::BlincContextState;
use blinc_core::{Brush, Color, CornerRadius, DrawContext, Rect, State};

use crate::canvas::{canvas, Canvas, CanvasBounds};
use crate::div::{div, Div, ElementBuilder};
use crate::stateful::{stateful_with_key, NoState};
use crate::widgets::cursor::SharedCursorState;

use super::cursor::{step_backward, step_forward, DocPosition};
use super::edit::{
    delete_backward, delete_forward, delete_selection, insert_char, insert_text, soft_break,
    split_block,
};
use super::format::{apply_mark_to_selection, Mark};
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
    let state_for_text = Arc::clone(state);
    let theme_for_render = theme.clone();
    let version_for_render = version.clone();
    let version_for_click = version.clone();
    let version_for_drag = version.clone();
    let version_for_key = version.clone();
    let version_for_text = version.clone();

    stateful_with_key::<NoState>(&stateful_key)
        .deps([version.signal_id()])
        .on_state(move |_ctx| {
            // Re-walk geometry against the (possibly mutated) document.
            let mut data = state_for_render.lock().unwrap();
            let geometry = compute_line_geometry(&data.document, &theme_for_render, content_width);
            data.set_line_index(geometry);

            // Sync cursor blink state's visibility with focus.
            data.set_cursor_visible(data.focused);

            // Compute the editor's content height from the line index so
            // the cursor canvas can be sized to span every visual line.
            // Without this the canvas defaults to its `h(1.0)` and the
            // cursor bar gets vertically clipped to a 1px dot. We add
            // a small bottom slack so the cursor is visible at the very
            // last line.
            let content_height = data
                .line_index
                .iter()
                .map(|g| g.y + g.height)
                .fold(0.0_f32, f32::max)
                .max(theme_for_render.font_size * theme_for_render.line_height);

            let doc_tree = render_document(&data.document, &theme_for_render, content_width);

            // Selection rectangles (one per visual line). The cursor
            // canvas overlays the entire content rect and is responsible
            // for its own blink animation.
            let mut overlay_children: Vec<Div> = Vec::new();
            if data.focused {
                if let Some(sel) = data.selection {
                    if !sel.is_empty() {
                        overlay_children.extend(selection_rects(&data, sel, &theme_for_render));
                    }
                }
            }

            // Wrap the document in a relative-positioned container so
            // absolute children (cursor / selection) are positioned
            // against the editor's content rect, not the window.
            //
            // The text cursor (`cursor_text`) is set on this inner Div
            // — not on the outer Stateful — because every rebuild
            // replaces the inner Div's RenderProps. Setting it on the
            // outer Stateful would only stick until the first state
            // mutation, after which the rebuilt inner div reverts to
            // the default arrow.
            let mut root = div().w_full().relative().cursor_text().child(doc_tree);
            for child in overlay_children {
                root = root.child(child);
            }
            // Cursor canvas spans the entire content rect and reads its
            // (x, y) position from the state on each redraw.
            root = root.child(cursor_overlay_canvas(
                Arc::clone(&state_for_render),
                data.cursor_state.clone(),
                theme_for_render.text,
                content_width,
                content_height,
            ));

            // Floating context toolbar — only shows when the editor is
            // focused and has a non-empty selection.
            drop(data);
            if let Some(toolbar) = super::toolbar::selection_toolbar(
                &state_for_render,
                &version_for_render,
                &theme_for_render,
            ) {
                root = root.child(toolbar);
            }
            root
        })
        .w_full()
        .on_mouse_down(move |ctx| {
            let mut data = state_for_click.lock().unwrap();
            data.set_focus(true);
            data.editor_bounds = (
                ctx.bounds_x,
                ctx.bounds_y,
                ctx.bounds_width,
                ctx.bounds_height,
            );

            // Detect double-click within the standard 400ms window.
            let now = std::time::Instant::now();
            let is_double = data
                .last_click_time
                .map(|t| now.duration_since(t).as_millis() < 400)
                .unwrap_or(false);
            data.last_click_time = Some(now);

            if let Some(pos) = data.position_from_click(ctx.local_x, ctx.local_y) {
                if is_double {
                    // Double-click — select the word under the cursor.
                    if let Some(line) = data
                        .document
                        .blocks
                        .get(pos.block)
                        .and_then(|b| b.lines.get(pos.line))
                    {
                        let (start_col, end_col) =
                            crate::widgets::text_edit::word_at_position(&line.text, pos.col);
                        data.cursor = super::cursor::DocPosition::new(pos.block, pos.line, end_col);
                        data.selection = Some(super::cursor::Selection {
                            anchor: super::cursor::DocPosition::new(pos.block, pos.line, start_col),
                            head: super::cursor::DocPosition::new(pos.block, pos.line, end_col),
                        });
                        data.reset_cursor_blink();
                    }
                } else {
                    let extend = ctx.shift;
                    data.move_cursor(pos, extend);
                }
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
        .on_text_input(move |ctx| {
            // Printable characters arrive here (post-IME). The
            // on_key_down handler intentionally doesn't insert anything
            // for typed characters; it only handles editing keys and
            // Cmd-shortcuts.
            let ch: char = match ctx.key_char {
                Some(c) => c,
                None => return,
            };
            // Skip control chars — those come via on_key_down.
            if ch.is_control() {
                return;
            }
            let mut data = state_for_text.lock().unwrap();
            if !data.focused {
                return;
            }
            // If the link prompt is open, route the keystroke into the
            // URL draft instead of inserting into the document.
            if let super::state::PickerState::Link { ref mut draft } = data.picker {
                draft.push(ch);
                drop(data);
                version_for_text.set(version_for_text.get().wrapping_add(1));
                return;
            }
            // If there's a selection, replace it first.
            let mut pos = data.cursor;
            if let Some(sel) = data.selection.take() {
                pos = delete_selection(&mut data.document, sel);
            }
            data.push_undo();
            let fmt = data.active_format.clone();
            let new_pos = insert_char(&mut data.document, pos, ch, &fmt);
            data.set_cursor(new_pos);
            drop(data);
            version_for_text.set(version_for_text.get().wrapping_add(1));
        })
        .on_key_down(move |ctx| {
            let mut data = state_for_key.lock().unwrap();
            if !data.focused {
                return;
            }

            // Link-prompt key handling — Backspace edits the draft,
            // Enter commits, Esc cancels. Other keys fall through so
            // arrow keys still navigate the document underneath.
            if matches!(data.picker, super::state::PickerState::Link { .. }) {
                match ctx.key_code {
                    8 => {
                        // Backspace — pop the last character from the draft
                        if let super::state::PickerState::Link { ref mut draft } = data.picker {
                            draft.pop();
                            drop(data);
                            version_for_key.set(version_for_key.get().wrapping_add(1));
                            return;
                        }
                    }
                    13 => {
                        // Enter — commit the link
                        if let super::state::PickerState::Link { draft } = data.picker.clone() {
                            if !draft.is_empty() {
                                if let Some(sel) = data.selection {
                                    if !sel.is_empty() {
                                        data.push_undo();
                                        super::format::apply_mark_to_selection(
                                            &mut data.document,
                                            sel,
                                            super::format::Mark::Link(Some(draft)),
                                        );
                                    }
                                }
                            }
                            data.picker = super::state::PickerState::None;
                            drop(data);
                            version_for_key.set(version_for_key.get().wrapping_add(1));
                            return;
                        }
                    }
                    27 => {
                        // Esc — cancel the prompt
                        data.picker = super::state::PickerState::None;
                        drop(data);
                        version_for_key.set(version_for_key.get().wrapping_add(1));
                        return;
                    }
                    _ => {}
                }
            }

            let mod_key = ctx.meta || ctx.ctrl;
            let extend = ctx.shift;
            let mut changed = false;

            // Modifier shortcuts first
            if mod_key {
                match ctx.key_code {
                    // Cmd+Z — undo
                    90 if !ctx.shift => {
                        if data.undo() {
                            changed = true;
                        }
                    }
                    // Cmd+Shift+Z — redo
                    90 if ctx.shift => {
                        if data.redo() {
                            changed = true;
                        }
                    }
                    // Cmd+A — select all
                    65 => {
                        let last_block = data.document.blocks.len().saturating_sub(1);
                        let last_line = data.document.blocks[last_block]
                            .lines
                            .len()
                            .saturating_sub(1);
                        let last_col = data.document.blocks[last_block].lines[last_line]
                            .text
                            .chars()
                            .count();
                        data.selection = Some(super::cursor::Selection {
                            anchor: DocPosition::ZERO,
                            head: DocPosition::new(last_block, last_line, last_col),
                        });
                        data.cursor = DocPosition::new(last_block, last_line, last_col);
                        data.reset_cursor_blink();
                        changed = true;
                    }
                    // Cmd+B — bold
                    66 => {
                        if toggle_mark(&mut data, Mark::Bold) {
                            changed = true;
                        }
                    }
                    // Cmd+I — italic
                    73 => {
                        if toggle_mark(&mut data, Mark::Italic) {
                            changed = true;
                        }
                    }
                    // Cmd+U — underline
                    85 => {
                        if toggle_mark(&mut data, Mark::Underline) {
                            changed = true;
                        }
                    }
                    // Cmd+Shift+X — strikethrough
                    88 if ctx.shift => {
                        if toggle_mark(&mut data, Mark::Strikethrough) {
                            changed = true;
                        }
                    }
                    // Cmd+E — inline code
                    69 => {
                        if toggle_mark(&mut data, Mark::Code) {
                            changed = true;
                        }
                    }
                    _ => {}
                }
                if changed {
                    drop(data);
                    version_for_key.set(version_for_key.get().wrapping_add(1));
                    return;
                }
            }

            match ctx.key_code {
                // Left
                37 => {
                    if mod_key {
                        // Cmd+Left jumps to start of line
                        let pos = home_of(&data);
                        data.move_cursor(pos, extend);
                        changed = true;
                    } else if let Some(pos) = step_backward(&data.document, data.cursor) {
                        data.move_cursor(pos, extend);
                        changed = true;
                    }
                }
                // Right
                39 => {
                    if mod_key {
                        let pos = end_of(&data);
                        data.move_cursor(pos, extend);
                        changed = true;
                    } else if let Some(pos) = step_forward(&data.document, data.cursor) {
                        data.move_cursor(pos, extend);
                        changed = true;
                    }
                }
                // Up
                38 => {
                    if let Some(pos) = move_vertical(&data, -1) {
                        data.move_cursor(pos, extend);
                        changed = true;
                    }
                }
                // Down
                40 => {
                    if let Some(pos) = move_vertical(&data, 1) {
                        data.move_cursor(pos, extend);
                        changed = true;
                    }
                }
                // Home
                36 => {
                    let pos = home_of(&data);
                    data.move_cursor(pos, extend);
                    changed = true;
                }
                // End
                35 => {
                    let pos = end_of(&data);
                    data.move_cursor(pos, extend);
                    changed = true;
                }
                // Backspace
                8 => {
                    let pos = data.cursor;
                    let sel = data.selection.take();
                    data.push_undo();
                    let new_pos = if let Some(s) = sel {
                        delete_selection(&mut data.document, s)
                    } else {
                        delete_backward(&mut data.document, pos)
                    };
                    data.set_cursor(new_pos);
                    changed = true;
                }
                // Delete (forward)
                127 => {
                    let pos = data.cursor;
                    let sel = data.selection.take();
                    data.push_undo();
                    let new_pos = if let Some(s) = sel {
                        delete_selection(&mut data.document, s)
                    } else {
                        delete_forward(&mut data.document, pos)
                    };
                    data.set_cursor(new_pos);
                    changed = true;
                }
                // Enter — Shift+Enter is a soft break, plain Enter splits the block
                13 => {
                    // Replace selection first
                    let mut pos = data.cursor;
                    if let Some(s) = data.selection.take() {
                        pos = delete_selection(&mut data.document, s);
                    }
                    data.push_undo();
                    let new_pos = if ctx.shift {
                        soft_break(&mut data.document, pos)
                    } else {
                        split_block(&mut data.document, pos)
                    };
                    data.set_cursor(new_pos);
                    changed = true;
                }
                // Escape — blur
                27 => {
                    data.set_focus(false);
                    changed = true;
                }
                _ => {}
            }

            drop(data);
            if changed {
                version_for_key.set(version_for_key.get().wrapping_add(1));
            }
        })
}

/// Canvas overlay that draws a blinking cursor at the current document
/// position. The closure reads the cursor geometry from `state` on each
/// frame, so when the editor's content rect is animating (or the cursor
/// blink advances) we don't need a tree rebuild — the canvas redraws
/// itself in place.
fn cursor_overlay_canvas(
    state: RichTextState,
    cursor_state: SharedCursorState,
    color: Color,
    width: f32,
    height: f32,
) -> Canvas {
    canvas(move |ctx: &mut dyn DrawContext, _bounds: CanvasBounds| {
        // Compute opacity from blink state.
        let (opacity, visible) = match cursor_state.lock() {
            Ok(s) => (s.current_opacity(), s.visible),
            Err(_) => (1.0, false),
        };
        if !visible || opacity < 0.01 {
            return;
        }

        let Ok(data) = state.lock() else {
            return;
        };
        if !data.focused {
            return;
        }
        let Some((x, y, h)) = data.cursor_geometry() else {
            return;
        };

        let c = Color::rgba(color.r, color.g, color.b, color.a * opacity);
        ctx.fill_rect(
            Rect::new(x, y, 2.0, h),
            CornerRadius::default(),
            Brush::Solid(c),
        );
    })
    .absolute()
    .left(0.0)
    .top(0.0)
    .w(width)
    .h(height.max(1.0))
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

/// Toggle an inline `mark`.
///
/// - With a non-empty selection, applies the toggle to the selected
///   range via [`apply_mark_to_selection`] and pushes an undo entry.
/// - With no selection, flips the corresponding flag on the cursor's
///   `ActiveFormat` so the next typed character carries the mark.
///   This matches every other rich editor's "click bold then start
///   typing" behaviour.
///
/// Returns `true` if anything changed (selection mark applied OR
/// active-format flag flipped), so the caller knows whether to bump
/// the rebuild signal.
fn toggle_mark(data: &mut super::state::RichTextData, mark: Mark) -> bool {
    if let Some(sel) = data.selection {
        if !sel.is_empty() {
            data.push_undo();
            let changed = apply_mark_to_selection(&mut data.document, sel, mark.clone());
            // Refresh the active format from the cursor location so the
            // toolbar (Phase 7) sees the new state.
            data.active_format =
                super::cursor::ActiveFormat::from_position(&data.document, data.cursor);
            data.reset_cursor_blink();
            return changed;
        }
    }

    // No selection — flip the active format flag in place.
    let fmt = &mut data.active_format;
    match mark {
        Mark::Bold => fmt.bold = !fmt.bold,
        Mark::Italic => fmt.italic = !fmt.italic,
        Mark::Underline => fmt.underline = !fmt.underline,
        Mark::Strikethrough => fmt.strikethrough = !fmt.strikethrough,
        Mark::Code => fmt.code = !fmt.code,
        Mark::Color(c) => fmt.color = Some(c),
        Mark::Link(url) => fmt.link = url,
    }
    data.reset_cursor_blink();
    true
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

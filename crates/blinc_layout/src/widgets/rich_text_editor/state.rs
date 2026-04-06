//! Editor state — document, cursor, selection, focus, and the visual line
//! index used for hit-testing and cursor positioning.
//!
//! `RichTextState` is the externally-visible handle. Like `TextInputData`
//! it's an `Arc<Mutex<…>>` so it survives across UI rebuilds. Phase 3 only
//! reads/writes the cursor and selection — Phase 4 will add the edit ops
//! that mutate the document.

use std::sync::{Arc, Mutex};

use crate::div::FontWeight;
use crate::styled_text::StyledLine;

use super::cursor::{ActiveFormat, DocPosition, Selection};
use super::document::RichDocument;

/// Geometry for a single visual line in the rendered document.
///
/// Built by the renderer at frame-build time and stored on the editor
/// state. Click handling and cursor positioning both walk this index.
///
/// One source `StyledLine` may produce many `LineGeometry` entries (one
/// per pre-wrapped chunk). The `source_line` field tells the click
/// handler which character it's looking at *inside the source line*, so
/// it can recover the byte/col offset back to the original document.
#[derive(Clone, Debug)]
pub struct LineGeometry {
    /// Document position of the *first* character in this visual line.
    /// Cursor placement on this line is `(block, line, col)` for cols
    /// `0..=visible_chars`.
    pub start: DocPosition,
    /// X offset of the line within the editor's content rect (px).
    /// Lists/quotes/indents add to this; plain paragraphs use 0.
    pub x: f32,
    /// Y top of the line within the editor's content rect (px).
    pub y: f32,
    /// Pixel width allocated for this line.
    pub width: f32,
    /// Pixel height of one line (font_size * line_height).
    pub height: f32,
    /// The wrapped chunk text (suitable for cursor x measurement).
    pub text: String,
    /// Font size (px) used for measurement.
    pub font_size: f32,
    /// Font weight used for measurement (drives bold-vs-normal width).
    pub weight: FontWeight,
    /// Italic flag used for measurement.
    pub italic: bool,
}

impl LineGeometry {
    /// True if `(local_x, local_y)` falls inside this line's rect.
    pub fn contains(&self, local_x: f32, local_y: f32) -> bool {
        local_y >= self.y
            && local_y < self.y + self.height
            && local_x >= self.x
            && local_x < self.x + self.width.max(1.0)
    }

    /// True if `local_y` falls inside this line's vertical band, ignoring
    /// horizontal position. Used by the click handler so clicking past
    /// the right edge of a short line still selects its end column.
    pub fn contains_y(&self, local_y: f32) -> bool {
        local_y >= self.y && local_y < self.y + self.height
    }
}

/// Editor data that survives across UI rebuilds.
///
/// Held inside `RichTextState = Arc<Mutex<RichTextData>>`. Public fields
/// are read-only outside Phase 4 — use the helper methods to mutate.
#[derive(Debug)]
pub struct RichTextData {
    /// The document being edited.
    pub document: RichDocument,
    /// Current cursor position. Always clamped to a valid location.
    pub cursor: DocPosition,
    /// Optional selection. When set, `head == cursor` and `anchor` is
    /// the other end.
    pub selection: Option<Selection>,
    /// Active formatting that will be applied to the next typed character.
    pub active_format: ActiveFormat,
    /// Focus flag — set on first mouse-down inside the editor.
    pub focused: bool,
    /// Visual line geometry index, populated by the renderer each frame.
    pub line_index: Vec<LineGeometry>,
}

impl Default for RichTextData {
    fn default() -> Self {
        Self::new(RichDocument::new())
    }
}

impl RichTextData {
    pub fn new(document: RichDocument) -> Self {
        Self {
            document,
            cursor: DocPosition::ZERO,
            selection: None,
            active_format: ActiveFormat::default(),
            focused: false,
            line_index: Vec::new(),
        }
    }

    /// Replace the line index. Called by the renderer at the end of each
    /// build pass.
    pub fn set_line_index(&mut self, index: Vec<LineGeometry>) {
        self.line_index = index;
    }

    /// Set the cursor position (clamped to valid bounds).
    pub fn set_cursor(&mut self, pos: DocPosition) {
        let clamped = pos.clamp(&self.document);
        self.cursor = clamped;
        self.active_format = ActiveFormat::from_position(&self.document, clamped);
    }

    /// Move the cursor and update the selection head if `extend` is true.
    pub fn move_cursor(&mut self, pos: DocPosition, extend: bool) {
        let clamped = pos.clamp(&self.document);
        if extend {
            // Establish or extend selection from the previous cursor.
            let anchor = self.selection.map(|s| s.anchor).unwrap_or(self.cursor);
            self.selection = Some(Selection {
                anchor,
                head: clamped,
            });
        } else {
            self.selection = None;
        }
        self.cursor = clamped;
        self.active_format = ActiveFormat::from_position(&self.document, clamped);
    }

    /// Find the first `LineGeometry` whose vertical band contains `local_y`.
    /// Falls back to the closest line above (or the very last line) if no
    /// band hits exactly — clicking past the bottom of the document still
    /// places the cursor.
    pub fn line_at_y(&self, local_y: f32) -> Option<&LineGeometry> {
        if self.line_index.is_empty() {
            return None;
        }
        // Direct hit
        if let Some(g) = self.line_index.iter().find(|g| g.contains_y(local_y)) {
            return Some(g);
        }
        // Above the first line — snap to first
        if local_y < self.line_index[0].y {
            return Some(&self.line_index[0]);
        }
        // Below everything — snap to last
        self.line_index.last()
    }

    /// Walk the line index for the line whose `start` matches `(block, line)`
    /// and which contains `col`. Returns the relative `(x, y)` of the
    /// cursor within the editor's content rect, plus the line height.
    ///
    /// Used by the cursor overlay renderer.
    pub fn cursor_geometry(&self) -> Option<(f32, f32, f32)> {
        let cursor = self.cursor;
        // Find the visual line whose start lies on the same source line
        // and whose char range contains `cursor.col`. Each visual line
        // covers `[start.col .. start.col + chars_in(text))`.
        let mut chosen: Option<&LineGeometry> = None;
        for g in &self.line_index {
            if g.start.block == cursor.block && g.start.line == cursor.line {
                let line_chars = g.text.chars().count();
                let line_end_col = g.start.col + line_chars;
                if cursor.col >= g.start.col && cursor.col <= line_end_col {
                    chosen = Some(g);
                    break;
                }
                // Cursor past the end of this visual chunk — keep the
                // last matching one as a fallback.
                if cursor.col > line_end_col {
                    chosen = Some(g);
                }
            }
        }
        let g = chosen?;
        let local_col = cursor.col.saturating_sub(g.start.col);
        let prefix = take_chars(&g.text, local_col);
        let prefix_width = measure_width(&prefix, g.font_size, g.weight, g.italic);
        Some((g.x + prefix_width, g.y, g.height))
    }

    /// Convert a click at `(local_x, local_y)` to a `DocPosition` and
    /// return it. Snaps to the nearest line if no line is directly under
    /// the click.
    pub fn position_from_click(&self, local_x: f32, local_y: f32) -> Option<DocPosition> {
        let g = self.line_at_y(local_y)?.clone();
        let inside_x = (local_x - g.x).max(0.0);
        let col_in_line = column_at_x(&g.text, inside_x, g.font_size, g.weight, g.italic);
        Some(DocPosition::new(
            g.start.block,
            g.start.line,
            g.start.col + col_in_line,
        ))
    }
}

/// Shared handle to editor state.
pub type RichTextState = Arc<Mutex<RichTextData>>;

/// Convenience: create a new shared state from a document.
pub fn rich_text_state(document: RichDocument) -> RichTextState {
    Arc::new(Mutex::new(RichTextData::new(document)))
}

// =====================================================================
// Helpers — text measurement / character math
// =====================================================================

fn take_chars(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// Measure the pixel width of `text` at the given font properties.
pub(crate) fn measure_width(text: &str, font_size: f32, weight: FontWeight, italic: bool) -> f32 {
    let mut options = crate::text_measure::TextLayoutOptions::new();
    options.font_weight = weight.weight();
    options.italic = italic;
    crate::text_measure::measure_text_with_options(text, font_size, &options).width
}

/// Find the character column inside `text` whose left edge is closest to
/// `target_x` (in pixels, measured from the line's left edge). The
/// returned column is in `0..=text.chars().count()` so that clicking past
/// the end of a line places the cursor at the end.
pub(crate) fn column_at_x(
    text: &str,
    target_x: f32,
    font_size: f32,
    weight: FontWeight,
    italic: bool,
) -> usize {
    if target_x <= 0.0 || text.is_empty() {
        return 0;
    }
    // Linear scan — fine for editor lines, which are short. We bisect
    // each character's left/right edge against the target and pick the
    // closer side.
    let mut prev_width = 0.0;
    let mut col = 0;
    for (i, _ch) in text.char_indices() {
        let upto = &text[..i];
        let after_idx = next_char_index(text, i);
        let upto_inclusive = &text[..after_idx];
        let w_before = measure_width(upto, font_size, weight, italic);
        let w_after = measure_width(upto_inclusive, font_size, weight, italic);
        let mid = (w_before + w_after) * 0.5;
        if target_x < mid {
            return col;
        }
        prev_width = w_after;
        col += 1;
        if w_after >= target_x && col > 0 {
            // Already past target — return the col that puts the cursor
            // before this character.
            // Actually we already advanced; bail and return col directly.
            return col;
        }
    }
    let _ = prev_width; // explicit drop, silences "unused"
    text.chars().count()
}

fn next_char_index(text: &str, byte_idx: usize) -> usize {
    text[byte_idx..]
        .char_indices()
        .nth(1)
        .map(|(i, _)| byte_idx + i)
        .unwrap_or(text.len())
}

/// Used by the line index to source the relevant `StyledLine` for a
/// click. The renderer registers entries with their source `StyledLine`
/// reference but stores only the wrapped text in `LineGeometry`; this
/// helper exists for tests that want to reconstruct geometry.
pub(crate) fn synth_line(text: &str, color: blinc_core::Color) -> StyledLine {
    StyledLine::plain(text, color)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::rich_text_editor::document::Block;
    use blinc_core::Color;

    fn sample_state() -> RichTextData {
        let doc = RichDocument::from_blocks(vec![
            Block::paragraph("hello world", Color::WHITE),
            Block::paragraph("second block", Color::WHITE),
        ]);
        let mut state = RichTextData::new(doc);
        // Synthesize a tiny line index — two single-line blocks.
        state.set_line_index(vec![
            LineGeometry {
                start: DocPosition::new(0, 0, 0),
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 20.0,
                text: "hello world".to_string(),
                font_size: 14.0,
                weight: FontWeight::Normal,
                italic: false,
            },
            LineGeometry {
                start: DocPosition::new(1, 0, 0),
                x: 0.0,
                y: 24.0,
                width: 200.0,
                height: 20.0,
                text: "second block".to_string(),
                font_size: 14.0,
                weight: FontWeight::Normal,
                italic: false,
            },
        ]);
        state
    }

    #[test]
    fn click_inside_first_line_finds_block_zero() {
        let state = sample_state();
        let pos = state.position_from_click(40.0, 5.0).unwrap();
        assert_eq!(pos.block, 0);
        assert_eq!(pos.line, 0);
    }

    #[test]
    fn click_in_second_line_finds_block_one() {
        let state = sample_state();
        let pos = state.position_from_click(40.0, 30.0).unwrap();
        assert_eq!(pos.block, 1);
    }

    #[test]
    fn click_above_first_line_snaps_to_start() {
        let state = sample_state();
        let pos = state.position_from_click(40.0, -100.0).unwrap();
        assert_eq!(pos.block, 0);
    }

    #[test]
    fn click_below_last_line_snaps_to_end() {
        let state = sample_state();
        let pos = state.position_from_click(40.0, 9999.0).unwrap();
        assert_eq!(pos.block, 1);
    }

    #[test]
    fn click_at_x_zero_returns_col_zero() {
        let state = sample_state();
        let pos = state.position_from_click(0.0, 5.0).unwrap();
        assert_eq!(pos.col, 0);
    }

    #[test]
    fn click_past_right_edge_returns_end_col() {
        let state = sample_state();
        let pos = state.position_from_click(10000.0, 5.0).unwrap();
        // "hello world" is 11 chars
        assert_eq!(pos.col, 11);
    }

    #[test]
    fn move_cursor_extends_selection_when_requested() {
        let mut state = sample_state();
        state.set_cursor(DocPosition::new(0, 0, 0));
        state.move_cursor(DocPosition::new(0, 0, 5), true);
        assert!(state.selection.is_some());
        let sel = state.selection.unwrap();
        assert_eq!(sel.anchor, DocPosition::new(0, 0, 0));
        assert_eq!(sel.head, DocPosition::new(0, 0, 5));
        // Subsequent extend keeps the same anchor
        state.move_cursor(DocPosition::new(0, 0, 8), true);
        let sel = state.selection.unwrap();
        assert_eq!(sel.anchor, DocPosition::new(0, 0, 0));
        assert_eq!(sel.head, DocPosition::new(0, 0, 8));
    }

    #[test]
    fn move_cursor_clears_selection_when_not_extending() {
        let mut state = sample_state();
        state.move_cursor(DocPosition::new(0, 0, 5), true);
        assert!(state.selection.is_some());
        state.move_cursor(DocPosition::new(0, 0, 7), false);
        assert!(state.selection.is_none());
    }

    #[test]
    fn cursor_geometry_returns_position_for_known_line() {
        let mut state = sample_state();
        state.set_cursor(DocPosition::new(0, 0, 5));
        let (x, y, h) = state.cursor_geometry().unwrap();
        assert!(x > 0.0);
        assert_eq!(y, 0.0);
        assert!(h > 0.0);
    }

    #[test]
    fn synth_line_helper_used_for_test_round_trip() {
        let _l = synth_line("a", Color::WHITE);
    }
}

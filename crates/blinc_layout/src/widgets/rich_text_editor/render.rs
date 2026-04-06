//! Read-only renderer for `RichDocument`.
//!
//! This phase produces a static element tree from a document — paragraphs,
//! headings, list items, quotes, dividers — using the existing `RichText`
//! element for inline span rendering. No cursor, no selection, no editing.
//!
//! Editing-aware rendering (cursor overlay, selection rects, click → cursor)
//! is added in Phase 3.
//!
//! # Theme
//!
//! Colors and font sizes are passed in via [`RichTextTheme`] so the renderer
//! stays decoupled from the application theme system. A reasonable default
//! is provided ([`RichTextTheme::dark`] / [`RichTextTheme::light`]).

use super::document::{Block, BlockKind, RichDocument};
use crate::div::{div, Div, FontWeight};
use crate::rich_text::{rich_text_styled, RichText};
use crate::styled_text::StyledText;
use blinc_core::Color;

/// Visual configuration for the rich text renderer.
///
/// All colors and sizes are explicit so the editor never reaches into a
/// global theme — pass a different `RichTextTheme` per editor instance to
/// theme it however you like.
#[derive(Clone, Debug)]
pub struct RichTextTheme {
    /// Default text color (paragraphs, list items).
    pub text: Color,
    /// Muted color used for blockquote text and list bullets.
    pub muted: Color,
    /// Color of the blockquote left border.
    pub quote_border: Color,
    /// Color of the divider line.
    pub divider: Color,
    /// Base font size for paragraph text (px).
    pub font_size: f32,
    /// Heading sizes for levels 1..=6 in px.
    pub heading_sizes: [f32; 6],
    /// Vertical spacing between blocks (px).
    pub block_gap: f32,
    /// Horizontal indent step for nested list items (px).
    pub indent_step: f32,
    /// Width of the blockquote left border (px).
    pub quote_border_width: f32,
    /// Width of the marker column for bullet/numbered list items (px).
    pub bullet_column_width: f32,
    /// Line height multiplier (1.0 = font_size, 1.5 = font_size * 1.5).
    /// Used both for spacing between visual lines and for cursor height.
    pub line_height: f32,
}

impl RichTextTheme {
    /// Reasonable defaults for a dark UI.
    pub fn dark() -> Self {
        Self {
            text: Color::rgba(0.92, 0.92, 0.95, 1.0),
            muted: Color::rgba(0.62, 0.62, 0.7, 1.0),
            quote_border: Color::rgba(0.35, 0.35, 0.42, 1.0),
            divider: Color::rgba(0.25, 0.25, 0.32, 1.0),
            font_size: 15.0,
            heading_sizes: [30.0, 24.0, 20.0, 18.0, 16.0, 15.0],
            block_gap: 8.0,
            indent_step: 24.0,
            quote_border_width: 3.0,
            bullet_column_width: 18.0,
            line_height: 1.45,
        }
    }

    /// Reasonable defaults for a light UI.
    pub fn light() -> Self {
        Self {
            text: Color::rgba(0.10, 0.10, 0.13, 1.0),
            muted: Color::rgba(0.42, 0.42, 0.5, 1.0),
            quote_border: Color::rgba(0.78, 0.78, 0.84, 1.0),
            divider: Color::rgba(0.88, 0.88, 0.92, 1.0),
            font_size: 15.0,
            heading_sizes: [30.0, 24.0, 20.0, 18.0, 16.0, 15.0],
            block_gap: 8.0,
            indent_step: 24.0,
            quote_border_width: 3.0,
            bullet_column_width: 18.0,
            line_height: 1.45,
        }
    }

    fn heading_size(&self, level: u8) -> f32 {
        let i = (level.clamp(1, 6) - 1) as usize;
        self.heading_sizes[i]
    }
}

impl Default for RichTextTheme {
    fn default() -> Self {
        Self::dark()
    }
}

/// Build a flat element tree representing `doc` using `theme`.
///
/// `content_width` is the pixel width of the column the document will be
/// rendered into. It's required because [`RichText`] needs an explicit
/// `wrap_to_width` to compute multi-line layouts — without it, long lines
/// silently overflow to the right of the container. Pass the inner width
/// of whatever wrapping div hosts the editor (i.e. width minus padding).
///
/// All spacing in the returned tree is expressed in raw pixels via `_px`
/// builders so that the renderer is independent of the global 4-unit
/// spacing scale used by the rest of the layout API.
pub fn render_document(doc: &RichDocument, theme: &RichTextTheme, content_width: f32) -> Div {
    let mut root = div().w_full().flex_col().gap_px(theme.block_gap);
    for (i, block) in doc.blocks.iter().enumerate() {
        root = root.child(render_block(doc, i, block, theme, content_width));
    }
    root
}

/// Build the element tree for a single block.
///
/// `index` is the block's position in `doc` — used for computing numbered
/// list ordinals. `content_width` is the available pixel width *outside*
/// any indent: each block subtracts its own indent before passing the
/// remainder down to its inline content.
fn render_block(
    doc: &RichDocument,
    index: usize,
    block: &Block,
    theme: &RichTextTheme,
    content_width: f32,
) -> Div {
    let indent_px = (block.indent as f32) * theme.indent_step;
    let inner_width = (content_width - indent_px).max(0.0);

    match &block.kind {
        BlockKind::Paragraph => indented_block(
            paragraph_div(
                block,
                theme,
                theme.font_size,
                FontWeight::Normal,
                false,
                inner_width,
            ),
            indent_px,
        ),
        BlockKind::Heading(level) => indented_block(
            paragraph_div(
                block,
                theme,
                theme.heading_size(*level),
                FontWeight::Bold,
                false,
                inner_width,
            ),
            indent_px,
        ),
        BlockKind::BulletItem => {
            list_item_div(block, theme, "•".to_string(), indent_px, inner_width)
        }
        BlockKind::NumberedItem => {
            let ordinal = doc.numbered_ordinal(index).unwrap_or(1);
            list_item_div(
                block,
                theme,
                format!("{}.", ordinal),
                indent_px,
                inner_width,
            )
        }
        BlockKind::Quote => quote_div(block, theme, indent_px, inner_width),
        BlockKind::Divider => indented_block(div().w_full().h(1.0).bg(theme.divider), indent_px),
    }
}

/// Wrap `inner` in a parent div whose left padding equals `indent_px`. Done
/// via a wrapper because there's no per-side `_px` padding builder; using
/// `pl(units)` would re-introduce the 4× scale bug.
fn indented_block(inner: Div, indent_px: f32) -> Div {
    if indent_px <= 0.5 {
        return inner.w_full();
    }
    div()
        .w_full()
        .flex_row()
        .child(div().w(indent_px))
        .child(inner.w_full())
}

/// Build a paragraph-style div containing one `RichText` per visual line.
///
/// Each source `StyledLine` is pre-wrapped to `line_width` via
/// [`super::wrap::wrap_styled_line`] and the resulting chunks are emitted
/// as separate children. This works around the styled-text render path
/// not supporting word wrap inside a single element.
fn paragraph_div(
    block: &Block,
    theme: &RichTextTheme,
    font_size: f32,
    weight: FontWeight,
    italic: bool,
    line_width: f32,
) -> Div {
    let mut col = div().w_full().flex_col();
    for source_line in &block.lines {
        let visual_lines = if line_width > 0.0 {
            super::wrap::wrap_styled_line(source_line, line_width, font_size, weight, italic)
        } else {
            vec![source_line.clone()]
        };
        for line in &visual_lines {
            col = col.child(render_line(
                line, theme, font_size, weight, italic, line_width,
            ));
        }
    }
    col
}

/// Build a list-item row: marker on the left, content on the right.
fn list_item_div(
    block: &Block,
    theme: &RichTextTheme,
    marker: String,
    indent_px: f32,
    inner_width: f32,
) -> Div {
    let marker_text = rich_text_styled(StyledText::plain(&marker, theme.muted))
        .size(theme.font_size)
        .default_color(theme.muted);
    // The marker column eats `bullet_column_width + 8` (gap) of the inner width.
    let line_width = (inner_width - theme.bullet_column_width - 8.0).max(0.0);

    let row = div()
        .w_full()
        .flex_row()
        .gap_px(8.0)
        .child(div().w(theme.bullet_column_width).child(marker_text))
        .child(paragraph_div(
            block,
            theme,
            theme.font_size,
            FontWeight::Normal,
            false,
            line_width,
        ));
    indented_block(row, indent_px)
}

/// Build a blockquote div with a left border and muted italic text.
fn quote_div(block: &Block, theme: &RichTextTheme, indent_px: f32, inner_width: f32) -> Div {
    // Border + gap consume `quote_border_width + 12` of the inner width.
    let line_width = (inner_width - theme.quote_border_width - 12.0).max(0.0);
    let inner = paragraph_div(
        block,
        theme,
        theme.font_size,
        FontWeight::Normal,
        true,
        line_width,
    );
    let row = div()
        .w_full()
        .flex_row()
        .gap_px(12.0)
        .child(div().w(theme.quote_border_width).bg(theme.quote_border))
        .child(inner);
    indented_block(row, indent_px)
}

/// Render a single (already pre-wrapped) styled line into a `RichText`.
/// The block-level font size, weight, and italic are applied as defaults;
/// spans inside the line still carry their own marks via `StyledText`.
fn render_line(
    line: &crate::styled_text::StyledLine,
    theme: &RichTextTheme,
    font_size: f32,
    weight: FontWeight,
    italic: bool,
    _line_width: f32,
) -> RichText {
    let styled = StyledText::from_lines(vec![line.clone()]);
    let mut rt = rich_text_styled(styled)
        .size(font_size)
        .weight(weight)
        .default_color(theme.text)
        .line_height(theme.line_height);
    if italic {
        rt = rt.italic();
    }
    rt
}

// =============================================================================
// Geometry walker — produces the LineGeometry index used by hit-testing
// =============================================================================

use super::cursor::DocPosition;
use super::state::LineGeometry;

/// Compute the line-geometry index for `doc` at the given `content_width`,
/// matching the layout that [`render_document`] produces. Walks every
/// block, applies the same indent / wrap / list-marker accounting, and
/// emits one `LineGeometry` per visual line.
///
/// The returned y-coordinates are relative to the editor's content rect
/// (the same rect whose origin matches the renderer tree). The editor's
/// click handler can use them directly with `local_x` / `local_y` from
/// the mouse-event context.
pub fn compute_line_geometry(
    doc: &RichDocument,
    theme: &RichTextTheme,
    content_width: f32,
) -> Vec<LineGeometry> {
    let mut out = Vec::new();
    let mut y = 0.0f32;
    for (block_idx, block) in doc.blocks.iter().enumerate() {
        let indent_px = (block.indent as f32) * theme.indent_step;
        let inner_width = (content_width - indent_px).max(0.0);

        match &block.kind {
            BlockKind::Divider => {
                // Divider has no inline content; it occupies 1px + the
                // implicit block_gap below. Skip it for cursor purposes.
                y += 1.0;
            }
            BlockKind::Paragraph => {
                emit_paragraph_geometry(
                    &mut out,
                    block_idx,
                    block,
                    indent_px,
                    inner_width,
                    theme.font_size,
                    FontWeight::Normal,
                    false,
                    theme,
                    &mut y,
                );
            }
            BlockKind::Heading(level) => {
                emit_paragraph_geometry(
                    &mut out,
                    block_idx,
                    block,
                    indent_px,
                    inner_width,
                    theme.heading_size(*level),
                    FontWeight::Bold,
                    false,
                    theme,
                    &mut y,
                );
            }
            BlockKind::BulletItem | BlockKind::NumberedItem => {
                let marker_total = theme.bullet_column_width + 8.0;
                let line_x = indent_px + marker_total;
                let line_w = (inner_width - marker_total).max(0.0);
                emit_paragraph_geometry(
                    &mut out,
                    block_idx,
                    block,
                    line_x,
                    line_w,
                    theme.font_size,
                    FontWeight::Normal,
                    false,
                    theme,
                    &mut y,
                );
            }
            BlockKind::Quote => {
                let border_total = theme.quote_border_width + 12.0;
                let line_x = indent_px + border_total;
                let line_w = (inner_width - border_total).max(0.0);
                emit_paragraph_geometry(
                    &mut out,
                    block_idx,
                    block,
                    line_x,
                    line_w,
                    theme.font_size,
                    FontWeight::Normal,
                    true,
                    theme,
                    &mut y,
                );
            }
        }
        // Block gap (matches gap_px on the root flex_col).
        y += theme.block_gap;
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn emit_paragraph_geometry(
    out: &mut Vec<LineGeometry>,
    block_idx: usize,
    block: &Block,
    line_x: f32,
    line_width: f32,
    font_size: f32,
    weight: FontWeight,
    italic: bool,
    theme: &RichTextTheme,
    y: &mut f32,
) {
    let line_height_px = font_size * theme.line_height;
    for (source_line_idx, source_line) in block.lines.iter().enumerate() {
        let visual_lines = if line_width > 0.0 {
            super::wrap::wrap_styled_line_with_offsets(
                source_line,
                line_width,
                font_size,
                weight,
                italic,
            )
        } else {
            vec![super::wrap::WrappedLine {
                line: source_line.clone(),
                source_start_col: 0,
                source_end_col: source_line.text.chars().count(),
            }]
        };
        // Make sure an empty source line still emits one zero-width
        // geometry so the cursor can sit on it.
        let to_emit: Vec<_> = if visual_lines.is_empty() {
            vec![super::wrap::WrappedLine {
                line: source_line.clone(),
                source_start_col: 0,
                source_end_col: 0,
            }]
        } else {
            visual_lines
        };
        for visual in to_emit {
            out.push(LineGeometry {
                start: DocPosition::new(block_idx, source_line_idx, visual.source_start_col),
                x: line_x,
                y: *y,
                width: line_width,
                height: line_height_px,
                text: visual.line.text,
                font_size,
                weight,
                italic,
            });
            *y += line_height_px;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::styled_text::TextSpan;

    fn sample_doc() -> RichDocument {
        let mut p = Block::paragraph("Hello, world", Color::WHITE);
        // Make "Hello" bold via a span override
        p.lines[0].spans = vec![
            TextSpan::new(0, 5, Color::WHITE, true),
            TextSpan::colored(5, 12, Color::WHITE),
        ];
        RichDocument::from_blocks(vec![
            Block::heading(1, "Demo", Color::WHITE),
            p,
            Block::bullet("first", Color::WHITE),
            Block::bullet("second", Color::WHITE),
            Block::numbered("alpha", Color::WHITE),
            Block::numbered("beta", Color::WHITE),
            Block::quote("a wise quote", Color::WHITE),
            Block::divider(),
            Block::paragraph("after the divider", Color::WHITE),
        ])
    }

    #[test]
    fn renders_without_panicking() {
        let doc = sample_doc();
        let theme = RichTextTheme::dark();
        let _root = render_document(&doc, &theme, 720.0);
    }

    #[test]
    fn empty_document_renders() {
        let doc = RichDocument::new();
        let theme = RichTextTheme::dark();
        let _root = render_document(&doc, &theme, 720.0);
    }
}

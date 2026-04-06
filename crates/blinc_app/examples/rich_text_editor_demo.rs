//! Rich Text Editor Demo (Phase 2: read-only renderer)
//!
//! Builds a hand-authored `RichDocument` and renders it through the
//! editor's read-only renderer. Demonstrates every block kind and inline
//! mark currently supported by the editor model:
//!
//! - Headings (H1–H3)
//! - Paragraphs with mixed bold / italic / underline / strikethrough /
//!   inline-code / colored / linked spans
//! - Bullet and numbered lists, including nested lists via `indent`
//! - Block quote
//! - Horizontal divider
//!
//! Editing, cursor, selection, and the toolbar all land in later phases —
//! this is purely a render-only smoke test for Phase 1+2.
//!
//! Run with: `cargo run -p blinc_app --example rich_text_editor_demo --features windowed`

use blinc_app::prelude::*;
use blinc_app::windowed::{WindowedApp, WindowedContext};
use blinc_core::context_state::BlincContextState;
use blinc_core::Color;
use blinc_layout::styled_text::{StyledLine, TextSpan};
use blinc_layout::widgets::rich_text_editor::{
    document::{Block, BlockKind, RichDocument},
    editor::rich_text_editor,
    render::RichTextTheme,
    state::{rich_text_state, RichTextState},
};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let config = WindowConfig {
        title: "Blinc Rich Text Editor Demo".to_string(),
        width: 900,
        height: 800,
        resizable: true,
        fullscreen: false,
        ..Default::default()
    };

    WindowedApp::run(config, |ctx| build_ui(ctx))
}

fn build_ui(ctx: &WindowedContext) -> impl ElementBuilder {
    let theme = RichTextTheme::dark();

    // Compute the explicit pixel width of the column the document renders
    // into. RichText needs this so each line wraps to the correct width
    // instead of overflowing the container on the right.
    const HORIZONTAL_PADDING: f32 = 32.0;
    const MAX_COLUMN_WIDTH: f32 = 720.0;
    let column_width = (ctx.width - 2.0 * HORIZONTAL_PADDING).min(MAX_COLUMN_WIDTH);

    // Persist the editor state across rebuilds via the context state store.
    let blinc = BlincContextState::get();
    let state_signal: blinc_core::State<Option<RichTextState>> =
        blinc.use_state_keyed("rich_text_editor_demo", || None);
    let state: RichTextState = match state_signal.get() {
        Some(s) => s,
        None => {
            let s = rich_text_state(sample_document(&theme));
            state_signal.set(Some(s.clone()));
            s
        }
    };

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.07, 0.07, 0.10, 1.0))
        .flex_col()
        .child(
            // Header bar — every spacing here is in raw pixels via _px helpers
            // because the rest of the layout API uses 4-unit spacing.
            div()
                .w_full()
                .h(56.0)
                .padding_x_px(32.0)
                .bg(Color::rgba(0.11, 0.11, 0.15, 1.0))
                .border_bottom(1.0, Color::rgba(0.20, 0.20, 0.26, 1.0))
                .flex_row()
                .items_center()
                .gap_px(16.0)
                .child(
                    text("Rich Text Editor")
                        .size(18.0)
                        .weight(FontWeight::Bold)
                        .color(Color::WHITE),
                )
                .child(
                    text("Phase 4 · editing + undo")
                        .size(12.0)
                        .color(Color::rgba(0.55, 0.55, 0.65, 1.0)),
                ),
        )
        .child(
            // Document area inside a scroll container.
            // Outer flex_row + justify_center centers a fixed-width column.
            scroll().w_full().h(ctx.height - 56.0).child(
                div()
                    .w_full()
                    .flex_row()
                    .justify_center()
                    .padding_x_px(HORIZONTAL_PADDING)
                    .padding_y_px(32.0)
                    .child(div().w(column_width).child(rich_text_editor(
                        &state,
                        theme.clone(),
                        column_width,
                    ))),
            ),
        )
}

// =============================================================================
// Sample document
// =============================================================================

fn sample_document(theme: &RichTextTheme) -> RichDocument {
    let text_color = theme.text;

    RichDocument::from_blocks(vec![
        Block::heading(1, "The Rich Text Editor", text_color),
        Block::paragraph(
            "A WYSIWYG editor for styled prose, built on the existing styled-text \
             primitives. This page is a hand-built RichDocument rendered through \
             the read-only renderer — no editing yet.",
            text_color,
        ),
        Block::heading(2, "Inline marks", text_color),
        // A paragraph showcasing every supported inline mark
        Block {
            kind: BlockKind::Paragraph,
            indent: 0,
            lines: vec![inline_marks_line(text_color)],
        },
        Block::heading(2, "Lists", text_color),
        Block::heading(3, "Bullet list", text_color),
        Block::bullet("first item — flat list, no indent", text_color),
        Block::bullet("second item — also at depth 0", text_color),
        nested_bullet("nested item at depth 1", text_color, 1),
        nested_bullet("another nested item", text_color, 1),
        nested_bullet("even deeper at depth 2", text_color, 2),
        Block::bullet("back to depth 0", text_color),
        Block::heading(3, "Numbered list", text_color),
        Block::numbered("ordinals are computed at render time", text_color),
        Block::numbered("they restart after a non-numbered block", text_color),
        Block::numbered("…so reordering items needs no bookkeeping", text_color),
        Block::paragraph("(plain paragraph breaks the run)", theme.muted),
        Block::numbered("a fresh run begins again at 1", text_color),
        Block::numbered("then 2", text_color),
        Block::heading(2, "Block quote", text_color),
        Block::quote(
            "The best way to predict the future is to invent it. — Alan Kay",
            text_color,
        ),
        Block::divider(),
        Block::heading(2, "Mixed prose", text_color),
        Block {
            kind: BlockKind::Paragraph,
            indent: 0,
            lines: vec![mixed_prose_line(text_color)],
        },
        Block::paragraph(
            "Phase 3 adds cursor, click-to-place, and selection. Phase 4 adds \
             actual editing. Phase 7 ships a toolbar. Until then, this page is \
             a smoke test that the model and renderer agree.",
            theme.muted,
        ),
    ])
}

// =============================================================================
// Helpers for spans
// =============================================================================

/// "Bold, italic, underline, strikethrough, inline code, colored, link"
fn inline_marks_line(default: Color) -> StyledLine {
    let segments: &[(&str, SpanFmt)] = &[
        ("Inline marks: ", SpanFmt::EMPTY),
        ("bold", SpanFmt::EMPTY.bold()),
        (", ", SpanFmt::EMPTY),
        ("italic", SpanFmt::EMPTY.italic()),
        (", ", SpanFmt::EMPTY),
        ("underline", SpanFmt::EMPTY.underline()),
        (", ", SpanFmt::EMPTY),
        ("strikethrough", SpanFmt::EMPTY.strikethrough()),
        (", ", SpanFmt::EMPTY),
        ("inline code", SpanFmt::EMPTY.code()),
        (", ", SpanFmt::EMPTY),
        (
            "colored",
            SpanFmt::EMPTY.color(Color::rgba(0.40, 0.78, 1.0, 1.0)),
        ),
        (", and a ", SpanFmt::EMPTY),
        (
            "hyperlink",
            SpanFmt::EMPTY
                .color(Color::rgba(0.45, 0.78, 1.0, 1.0))
                .underline()
                .link("https://example.com"),
        ),
        (".", SpanFmt::EMPTY),
    ];
    line_from_segments(default, segments)
}

/// A second showcase line that mixes several marks at once.
fn mixed_prose_line(default: Color) -> StyledLine {
    let segments: &[(&str, SpanFmt)] = &[
        ("You can ", SpanFmt::EMPTY),
        ("combine ", SpanFmt::EMPTY.bold().italic()),
        ("multiple marks ", SpanFmt::EMPTY.bold().underline()),
        ("on a single ", SpanFmt::EMPTY),
        (
            "run",
            SpanFmt::EMPTY
                .italic()
                .color(Color::rgba(1.0, 0.65, 0.42, 1.0)),
        ),
        (", or ", SpanFmt::EMPTY),
        (
            "strike them out",
            SpanFmt::EMPTY
                .strikethrough()
                .color(Color::rgba(0.55, 0.55, 0.65, 1.0)),
        ),
        (" entirely. Even ", SpanFmt::EMPTY),
        (
            "code chips",
            SpanFmt::EMPTY
                .code()
                .color(Color::rgba(0.93, 0.85, 0.55, 1.0)),
        ),
        (" sit happily inline.", SpanFmt::EMPTY),
    ];
    line_from_segments(default, segments)
}

/// Build a `StyledLine` from a sequence of (text, format) pairs. The
/// resulting line has one `TextSpan` per segment with byte ranges
/// computed automatically.
fn line_from_segments(default: Color, segments: &[(&str, SpanFmt)]) -> StyledLine {
    let mut text = String::new();
    let mut spans = Vec::with_capacity(segments.len());
    for (chunk, fmt) in segments {
        let start = text.len();
        text.push_str(chunk);
        let end = text.len();
        spans.push(fmt.into_span(start, end, default));
    }
    StyledLine { text, spans }
}

#[derive(Clone, Copy)]
struct SpanFmt {
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    code: bool,
    color: Option<Color>,
    link: Option<&'static str>,
}

impl SpanFmt {
    const EMPTY: Self = Self {
        bold: false,
        italic: false,
        underline: false,
        strikethrough: false,
        code: false,
        color: None,
        link: None,
    };

    const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
    const fn italic(mut self) -> Self {
        self.italic = true;
        self
    }
    const fn underline(mut self) -> Self {
        self.underline = true;
        self
    }
    const fn strikethrough(mut self) -> Self {
        self.strikethrough = true;
        self
    }
    const fn code(mut self) -> Self {
        self.code = true;
        self
    }
    const fn color(mut self, c: Color) -> Self {
        self.color = Some(c);
        self
    }
    const fn link(mut self, url: &'static str) -> Self {
        self.link = Some(url);
        self
    }

    fn into_span(self, start: usize, end: usize, default: Color) -> TextSpan {
        TextSpan {
            start,
            end,
            color: self.color.unwrap_or(default),
            bold: self.bold,
            italic: self.italic,
            underline: self.underline,
            strikethrough: self.strikethrough,
            code: self.code,
            link_url: self.link.map(String::from),
            token_type: None,
        }
    }
}

fn nested_bullet(text: &str, color: Color, indent: u8) -> Block {
    Block {
        kind: BlockKind::BulletItem,
        lines: vec![StyledLine::plain(text, color)],
        indent,
    }
}

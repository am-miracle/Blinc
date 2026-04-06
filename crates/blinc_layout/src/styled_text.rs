//! Styled text with multiple color spans
//!
//! This module provides structures for representing text with multiple styling spans,
//! enabling syntax highlighting and rich text rendering.
//!
//! # Example
//!
//! ```ignore
//! use blinc_layout::styled_text::{StyledText, TextSpan};
//! use blinc_core::Color;
//!
//! // Create styled text manually
//! let styled = StyledText::from_lines(vec![
//!     StyledLine {
//!         text: "fn main() {".to_string(),
//!         spans: vec![
//!             TextSpan::new(0, 2, Color::BLUE, true),   // "fn" keyword
//!             TextSpan::new(3, 7, Color::YELLOW, false), // "main" function name
//!         ],
//!     },
//! ]);
//! ```

use blinc_core::Color;

use crate::syntax::TokenType;

/// A span of styled text within a line
#[derive(Clone, Debug, PartialEq)]
pub struct TextSpan {
    /// Start byte index in the line
    pub start: usize,
    /// End byte index in the line (exclusive)
    pub end: usize,
    /// Text color
    pub color: Color,
    /// Whether text is bold
    pub bold: bool,
    /// Whether text is italic
    pub italic: bool,
    /// Whether text has underline decoration
    pub underline: bool,
    /// Whether text has strikethrough decoration
    pub strikethrough: bool,
    /// Whether the run is inline code (renders in monospace with a code chip background)
    pub code: bool,
    /// Optional link URL (for clickable text spans)
    pub link_url: Option<String>,
    /// Token type (for intellisense callbacks)
    pub token_type: Option<TokenType>,
}

impl TextSpan {
    /// Create a new text span
    pub fn new(start: usize, end: usize, color: Color, bold: bool) -> Self {
        Self {
            start,
            end,
            color,
            bold,
            italic: false,
            underline: false,
            strikethrough: false,
            code: false,
            link_url: None,
            token_type: None,
        }
    }

    /// Create a span with just color (not bold)
    pub fn colored(start: usize, end: usize, color: Color) -> Self {
        Self::new(start, end, color, false)
    }

    /// Set the token type for this span
    pub fn with_token_type(mut self, token_type: TokenType) -> Self {
        self.token_type = Some(token_type);
        self
    }

    /// Set italic style
    pub fn with_italic(mut self, italic: bool) -> Self {
        self.italic = italic;
        self
    }

    /// Set underline decoration
    pub fn with_underline(mut self, underline: bool) -> Self {
        self.underline = underline;
        self
    }

    /// Set strikethrough decoration
    pub fn with_strikethrough(mut self, strikethrough: bool) -> Self {
        self.strikethrough = strikethrough;
        self
    }

    /// Mark this span as inline code (monospace with chip background)
    pub fn with_code(mut self, code: bool) -> Self {
        self.code = code;
        self
    }

    // =========================================================================
    // Fluent setters — flag-flipping shorthands for builder chains
    // =========================================================================

    /// Mark this span as bold (no-arg shorthand for `with_bold(true)`).
    pub fn bold_on(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Mark this span as italic (no-arg shorthand).
    pub fn italic_on(mut self) -> Self {
        self.italic = true;
        self
    }

    /// Mark this span as underlined (no-arg shorthand).
    pub fn underline_on(mut self) -> Self {
        self.underline = true;
        self
    }

    /// Mark this span as strikethrough (no-arg shorthand).
    pub fn strikethrough_on(mut self) -> Self {
        self.strikethrough = true;
        self
    }

    /// Mark this span as inline code (no-arg shorthand).
    pub fn code_on(mut self) -> Self {
        self.code = true;
        self
    }

    /// Set this span's color.
    pub fn with_color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    /// Set link URL for clickable span
    pub fn with_link(mut self, url: impl Into<String>) -> Self {
        self.link_url = Some(url.into());
        self
    }

    /// Create an italic span
    pub fn italic(start: usize, end: usize, color: Color) -> Self {
        Self::new(start, end, color, false).with_italic(true)
    }

    /// Create a bold italic span
    pub fn bold_italic(start: usize, end: usize, color: Color) -> Self {
        Self::new(start, end, color, true).with_italic(true)
    }

    /// Create a link span (underlined by default)
    pub fn link(start: usize, end: usize, color: Color, url: impl Into<String>) -> Self {
        Self::new(start, end, color, false)
            .with_underline(true)
            .with_link(url)
    }
}

/// A line with styled spans
#[derive(Clone, Debug, PartialEq)]
pub struct StyledLine {
    /// The raw text content
    pub text: String,
    /// Style spans for this line (must cover entire line, sorted by start position)
    pub spans: Vec<TextSpan>,
}

impl StyledLine {
    /// Create a new styled line
    pub fn new(text: impl Into<String>, spans: Vec<TextSpan>) -> Self {
        Self {
            text: text.into(),
            spans,
        }
    }

    /// Create a line with a single color for all text
    pub fn plain(text: impl Into<String>, color: Color) -> Self {
        let text = text.into();
        let len = text.len();
        Self {
            spans: vec![TextSpan::colored(0, len, color)],
            text,
        }
    }

    /// Build a `StyledLine` from a sequence of segments where each
    /// segment is `(text, span_template)`.
    ///
    /// The byte ranges of each emitted span are computed automatically
    /// by appending the segments in order, so callers don't have to
    /// track offsets manually. The `default_color` is applied to any
    /// span whose template carries a transparent color (the conventional
    /// "no explicit color" sentinel — see [`SpanTemplate::default()`]).
    ///
    /// # Example
    /// ```ignore
    /// use blinc_layout::styled_text::{StyledLine, SpanTemplate};
    /// use blinc_core::Color;
    ///
    /// let line = StyledLine::from_segments(Color::WHITE, &[
    ///     ("Hello, ", SpanTemplate::default()),
    ///     ("world", SpanTemplate::default().bold()),
    ///     ("!", SpanTemplate::default()),
    /// ]);
    /// ```
    pub fn from_segments(default_color: Color, segments: &[(&str, SpanTemplate)]) -> Self {
        let mut text = String::new();
        let mut spans = Vec::with_capacity(segments.len());
        for (chunk, template) in segments {
            let start = text.len();
            text.push_str(chunk);
            let end = text.len();
            spans.push(template.into_span(start, end, default_color));
        }
        Self { text, spans }
    }
}

/// A reusable inline-format template for [`StyledLine::from_segments`].
///
/// Builds onto a default-colored span with chainable flag setters. The
/// emitted [`TextSpan`] inherits `default_color` from `from_segments`
/// unless `color()` was set explicitly.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpanTemplate {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub code: bool,
    pub color: Option<Color>,
    pub link: Option<String>,
}

impl SpanTemplate {
    /// A new template with no marks set. Equivalent to `Self::default()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the span as bold.
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Mark the span as italic.
    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    /// Mark the span as underlined.
    pub fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    /// Mark the span as strikethrough.
    pub fn strikethrough(mut self) -> Self {
        self.strikethrough = true;
        self
    }

    /// Mark the span as inline code (monospace + chip background).
    pub fn code(mut self) -> Self {
        self.code = true;
        self
    }

    /// Override the span's color.
    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    /// Attach a link URL to the span (also implicitly underlines it
    /// when consumers respect the standard "links are underlined"
    /// convention — `into_span` does not force the underline so callers
    /// can opt out).
    pub fn link(mut self, url: impl Into<String>) -> Self {
        self.link = Some(url.into());
        self
    }

    /// Materialize this template into a [`TextSpan`] over byte range
    /// `[start, end)`. The span's color falls back to `default_color`
    /// when the template didn't override it.
    pub fn into_span(&self, start: usize, end: usize, default_color: Color) -> TextSpan {
        TextSpan {
            start,
            end,
            color: self.color.unwrap_or(default_color),
            bold: self.bold,
            italic: self.italic,
            underline: self.underline,
            strikethrough: self.strikethrough,
            code: self.code,
            link_url: self.link.clone(),
            token_type: None,
        }
    }
}

/// Complete styled text with multiple lines
#[derive(Clone, Debug, Default)]
pub struct StyledText {
    /// All lines with their styles
    pub lines: Vec<StyledLine>,
}

impl StyledText {
    /// Create empty styled text
    pub fn new() -> Self {
        Self::default()
    }

    /// Create from pre-built lines
    pub fn from_lines(lines: Vec<StyledLine>) -> Self {
        Self { lines }
    }

    /// Create from plain text with a single color
    pub fn plain(text: &str, color: Color) -> Self {
        let lines = text
            .lines()
            .map(|line| StyledLine::plain(line, color))
            .collect();
        Self { lines }
    }

    /// Get the total number of lines
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Get the raw text content (without styling)
    pub fn raw_text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plain_text() {
        let styled = StyledText::plain("Hello\nWorld", Color::WHITE);
        assert_eq!(styled.line_count(), 2);
        assert_eq!(styled.lines[0].text, "Hello");
        assert_eq!(styled.lines[1].text, "World");
        assert_eq!(styled.lines[0].spans.len(), 1);
        assert_eq!(styled.lines[0].spans[0].start, 0);
        assert_eq!(styled.lines[0].spans[0].end, 5);
    }

    #[test]
    fn test_raw_text() {
        let styled = StyledText::plain("Line 1\nLine 2\nLine 3", Color::WHITE);
        assert_eq!(styled.raw_text(), "Line 1\nLine 2\nLine 3");
    }

    #[test]
    fn test_styled_line() {
        let line = StyledLine::new(
            "fn main()",
            vec![
                TextSpan::new(0, 2, Color::BLUE, true),
                TextSpan::colored(3, 7, Color::YELLOW),
            ],
        );
        assert_eq!(line.text, "fn main()");
        assert_eq!(line.spans.len(), 2);
        assert!(line.spans[0].bold);
        assert!(!line.spans[1].bold);
    }
}

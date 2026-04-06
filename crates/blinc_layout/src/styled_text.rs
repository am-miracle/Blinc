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

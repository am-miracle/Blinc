//! Syntax highlighting for code elements
//!
//! This module provides a trait-based syntax highlighting system that uses regex
//! patterns to match tokens and apply colors.
//!
//! # Example
//!
//! ```ignore
//! use blinc_layout::syntax::{SyntaxHighlighter, TokenRule, SyntaxConfig};
//! use blinc_core::Color;
//!
//! struct MyHighlighter {
//!     rules: Vec<TokenRule>,
//! }
//!
//! impl SyntaxHighlighter for MyHighlighter {
//!     fn token_rules(&self) -> &[TokenRule] {
//!         &self.rules
//!     }
//! }
//!
//! // Use with code element
//! code("let x = 42;")
//!     .config(SyntaxConfig::new(MyHighlighter { rules: vec![...] }))
//! ```

use blinc_core::Color;
use regex::Regex;

use crate::styled_text::{StyledLine, StyledText, TextSpan};

/// Token type identifier for callbacks
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TokenType {
    /// A keyword (fn, let, if, etc.)
    Keyword,
    /// A string literal
    String,
    /// A comment
    Comment,
    /// A number literal
    Number,
    /// A type name
    Type,
    /// A function name
    Function,
    /// A variable name
    Variable,
    /// A macro
    Macro,
    /// An operator
    Operator,
    /// A lifetime (Rust-specific)
    Lifetime,
    /// Custom token type with a name
    Custom(String),
}

impl TokenType {
    /// Create a custom token type
    pub fn custom(name: impl Into<String>) -> Self {
        TokenType::Custom(name.into())
    }
}

/// Information about a token hit (for intellisense callbacks)
#[derive(Clone, Debug)]
pub struct TokenHit {
    /// The matched token text
    pub text: String,
    /// The token type
    pub token_type: TokenType,
    /// Line number (0-based)
    pub line: usize,
    /// Start column (0-based, in characters)
    pub start_column: usize,
    /// End column (0-based, exclusive)
    pub end_column: usize,
}

impl TokenHit {
    /// Create a new token hit
    pub fn new(
        text: impl Into<String>,
        token_type: TokenType,
        line: usize,
        start_column: usize,
        end_column: usize,
    ) -> Self {
        Self {
            text: text.into(),
            token_type,
            line,
            start_column,
            end_column,
        }
    }
}

/// A single token style rule with regex pattern
#[derive(Clone)]
pub struct TokenRule {
    /// Compiled regex pattern to match
    pattern: Regex,
    /// Color to apply to matched tokens
    pub color: Color,
    /// Whether text should be bold
    pub bold: bool,
    /// Token type for identification in callbacks
    pub token_type: TokenType,
}

impl TokenRule {
    /// Create a new token rule with a regex pattern
    ///
    /// # Panics
    /// Panics if the regex pattern is invalid
    pub fn new(pattern: &str, color: Color) -> Self {
        Self {
            pattern: Regex::new(pattern).expect("Invalid regex pattern"),
            color,
            bold: false,
            token_type: TokenType::Custom("unknown".to_string()),
        }
    }

    /// Try to create a token rule, returning None if pattern is invalid
    pub fn try_new(pattern: &str, color: Color) -> Option<Self> {
        Regex::new(pattern).ok().map(|pattern| Self {
            pattern,
            color,
            bold: false,
            token_type: TokenType::Custom("unknown".to_string()),
        })
    }

    /// Set the token type for this rule
    pub fn token_type(mut self, token_type: TokenType) -> Self {
        self.token_type = token_type;
        self
    }

    /// Make this token bold
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Get the regex pattern
    pub fn pattern(&self) -> &Regex {
        &self.pattern
    }
}

impl std::fmt::Debug for TokenRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenRule")
            .field("pattern", &self.pattern.as_str())
            .field("color", &self.color)
            .field("bold", &self.bold)
            .finish()
    }
}

/// Trait for syntax highlighting implementations
///
/// Implement this trait to create custom syntax highlighters for different
/// languages or formats. The highlighter provides regex-based token rules
/// that are applied in order (first match wins for overlapping regions).
pub trait SyntaxHighlighter: Send + Sync {
    /// Get the list of token rules in priority order (first match wins)
    fn token_rules(&self) -> &[TokenRule];

    /// Default text color for non-matched text
    fn default_color(&self) -> Color {
        Color::rgba(0.9, 0.9, 0.9, 1.0)
    }

    /// Background color for the code block
    fn background_color(&self) -> Color {
        Color::rgba(0.12, 0.12, 0.14, 1.0)
    }

    /// Line number color (dimmer than text)
    fn line_number_color(&self) -> Color {
        Color::rgba(0.45, 0.45, 0.5, 1.0)
    }

    /// Apply highlighting to text, returning styled text
    fn highlight(&self, text: &str) -> StyledText {
        let default_color = self.default_color();
        let rules = self.token_rules();

        // Handle empty string specially - .lines() returns empty iterator for ""
        if text.is_empty() {
            return StyledText::from_lines(vec![highlight_line("", rules, default_color)]);
        }

        let lines = text
            .lines()
            .map(|line| highlight_line(line, rules, default_color))
            .collect();

        StyledText::from_lines(lines)
    }
}

/// Configuration for syntax highlighting passed to code elements
pub struct SyntaxConfig {
    highlighter: Box<dyn SyntaxHighlighter>,
}

impl SyntaxConfig {
    /// Create a new syntax config with the given highlighter
    pub fn new(highlighter: impl SyntaxHighlighter + 'static) -> Self {
        Self {
            highlighter: Box::new(highlighter),
        }
    }

    /// Get a reference to the highlighter
    pub fn highlighter(&self) -> &dyn SyntaxHighlighter {
        self.highlighter.as_ref()
    }

    /// Convert the config into an Arc-wrapped highlighter
    pub fn into_arc(self) -> std::sync::Arc<dyn SyntaxHighlighter> {
        std::sync::Arc::from(self.highlighter)
    }
}

/// A match found by a token rule
#[derive(Debug, Clone)]
struct TokenMatch {
    start: usize,
    end: usize,
    color: Color,
    bold: bool,
    token_type: TokenType,
}

/// Highlight a single line of text using the given rules
fn highlight_line(line: &str, rules: &[TokenRule], default_color: Color) -> StyledLine {
    if line.is_empty() {
        return StyledLine::new(line, vec![]);
    }

    // Find all matches from all rules
    let mut matches: Vec<TokenMatch> = Vec::new();

    for rule in rules {
        for m in rule.pattern.find_iter(line) {
            matches.push(TokenMatch {
                start: m.start(),
                end: m.end(),
                color: rule.color,
                bold: rule.bold,
                token_type: rule.token_type.clone(),
            });
        }
    }

    // Sort by start position (earlier first), then by rule priority (earlier in rules list)
    // Since we iterate rules in order and push matches, matches from earlier rules come first
    matches.sort_by_key(|m| m.start);

    // Build non-overlapping spans (first match wins for overlapping regions)
    let mut spans: Vec<TextSpan> = Vec::new();
    let mut current_pos = 0;

    for m in matches {
        // Skip if this match overlaps with already processed region
        if m.start < current_pos {
            continue;
        }

        // Add default-colored span for gap before this match
        if m.start > current_pos {
            spans.push(TextSpan::new(current_pos, m.start, default_color, false));
        }

        // Add the matched span with token type
        let mut span = TextSpan::new(m.start, m.end, m.color, m.bold);
        span.token_type = Some(m.token_type.clone());
        spans.push(span);
        current_pos = m.end;
    }

    // Add remaining text in default color
    if current_pos < line.len() {
        spans.push(TextSpan::new(current_pos, line.len(), default_color, false));
    }

    StyledLine::new(line, spans)
}

// ============================================================================
// Built-in Highlighters
// ============================================================================

/// A simple highlighter with no rules (plain text)
pub struct PlainHighlighter {
    text_color: Color,
    bg_color: Color,
}

impl PlainHighlighter {
    /// Create a new plain highlighter with default colors
    pub fn new() -> Self {
        Self {
            text_color: Color::rgba(0.9, 0.9, 0.9, 1.0),
            bg_color: Color::rgba(0.12, 0.12, 0.14, 1.0),
        }
    }

    /// Set the text color
    pub fn text_color(mut self, color: Color) -> Self {
        self.text_color = color;
        self
    }

    /// Set the background color
    pub fn background(mut self, color: Color) -> Self {
        self.bg_color = color;
        self
    }
}

impl Default for PlainHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl SyntaxHighlighter for PlainHighlighter {
    fn token_rules(&self) -> &[TokenRule] {
        &[]
    }

    fn default_color(&self) -> Color {
        self.text_color
    }

    fn background_color(&self) -> Color {
        self.bg_color
    }
}

/// A basic Rust syntax highlighter
pub struct RustHighlighter {
    rules: Vec<TokenRule>,
    bg_color: Color,
}

impl RustHighlighter {
    /// Create a new Rust highlighter with default colors
    pub fn new() -> Self {
        // Color palette (VS Code Dark+ inspired)
        let keyword_color = Color::rgba(0.77, 0.56, 0.82, 1.0); // Purple
        let string_color = Color::rgba(0.81, 0.54, 0.44, 1.0); // Orange/brown
        let comment_color = Color::rgba(0.42, 0.54, 0.35, 1.0); // Green
        let number_color = Color::rgba(0.71, 0.82, 0.57, 1.0); // Light green
        let function_color = Color::rgba(0.86, 0.82, 0.65, 1.0); // Yellow
        let type_color = Color::rgba(0.31, 0.76, 0.77, 1.0); // Cyan
        let macro_color = Color::rgba(0.31, 0.76, 0.77, 1.0); // Cyan
        let lifetime_color = Color::rgba(0.77, 0.56, 0.82, 1.0); // Purple

        let rules = vec![
            // Comments (must be early to capture before other rules)
            TokenRule::new(r"//.*$", comment_color).token_type(TokenType::Comment),
            // Strings (double-quoted)
            TokenRule::new(r#""[^"]*""#, string_color).token_type(TokenType::String),
            // Characters (single quoted, simplified)
            TokenRule::new(r"'[^']{1,2}'", string_color).token_type(TokenType::String),
            // Lifetimes
            TokenRule::new(r"'[a-zA-Z_][a-zA-Z0-9_]*", lifetime_color).token_type(TokenType::Lifetime),
            // Macros
            TokenRule::new(r"\b[a-z_][a-zA-Z0-9_]*!", macro_color).token_type(TokenType::Macro),
            // Keywords
            TokenRule::new(
                r"\b(fn|let|mut|const|static|pub|use|mod|struct|enum|trait|impl|for|while|loop|if|else|match|return|break|continue|async|await|move|ref|self|Self|super|crate|where|type|dyn|unsafe|extern)\b",
                keyword_color,
            ).bold().token_type(TokenType::Keyword),
            // Types (PascalCase)
            TokenRule::new(r"\b[A-Z][a-zA-Z0-9_]*\b", type_color).token_type(TokenType::Type),
            // Numbers
            TokenRule::new(r"\b\d+(\.\d+)?([eE][+-]?\d+)?[fiu]?(8|16|32|64|128|size)?\b", number_color).token_type(TokenType::Number),
            TokenRule::new(r"\b0x[0-9a-fA-F_]+\b", number_color).token_type(TokenType::Number),
            TokenRule::new(r"\b0b[01_]+\b", number_color).token_type(TokenType::Number),
            TokenRule::new(r"\b0o[0-7_]+\b", number_color).token_type(TokenType::Number),
            // Function calls
            TokenRule::new(r"\b([a-z_][a-zA-Z0-9_]*)\s*\(", function_color).token_type(TokenType::Function),
        ];

        Self {
            rules,
            bg_color: Color::rgba(0.12, 0.12, 0.14, 1.0),
        }
    }
}

impl Default for RustHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl SyntaxHighlighter for RustHighlighter {
    fn token_rules(&self) -> &[TokenRule] {
        &self.rules
    }

    fn background_color(&self) -> Color {
        self.bg_color
    }
}

/// A basic JSON syntax highlighter
pub struct JsonHighlighter {
    rules: Vec<TokenRule>,
}

impl JsonHighlighter {
    /// Create a new JSON highlighter
    pub fn new() -> Self {
        let string_color = Color::rgba(0.81, 0.54, 0.44, 1.0); // Orange/brown
        let number_color = Color::rgba(0.71, 0.82, 0.57, 1.0); // Light green
        let keyword_color = Color::rgba(0.77, 0.56, 0.82, 1.0); // Purple
        let key_color = Color::rgba(0.61, 0.78, 0.92, 1.0); // Light blue

        let rules = vec![
            // Keys (strings followed by colon)
            TokenRule::new(r#""[^"]*"\s*:"#, key_color),
            // Strings
            TokenRule::new(r#""(?:[^"\\]|\\.)*""#, string_color),
            // Numbers
            TokenRule::new(r"-?\b\d+(\.\d+)?([eE][+-]?\d+)?\b", number_color),
            // Keywords
            TokenRule::new(r"\b(true|false|null)\b", keyword_color),
        ];

        Self { rules }
    }
}

impl Default for JsonHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl SyntaxHighlighter for JsonHighlighter {
    fn token_rules(&self) -> &[TokenRule] {
        &self.rules
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plain_highlighter() {
        let highlighter = PlainHighlighter::new();
        let styled = highlighter.highlight("Hello, World!");

        assert_eq!(styled.line_count(), 1);
        assert_eq!(styled.lines[0].spans.len(), 1);
        assert_eq!(styled.lines[0].spans[0].start, 0);
        assert_eq!(styled.lines[0].spans[0].end, 13);
    }

    #[test]
    fn test_rust_keywords() {
        let highlighter = RustHighlighter::new();
        let styled = highlighter.highlight("fn main() {}");

        assert_eq!(styled.line_count(), 1);
        // Should have spans for: "fn", " ", "main", "()", " ", "{}"
        assert!(!styled.lines[0].spans.is_empty());
    }

    #[test]
    fn test_multiline() {
        let highlighter = RustHighlighter::new();
        let code = "fn main() {\n    println!(\"Hello\");\n}";
        let styled = highlighter.highlight(code);

        assert_eq!(styled.line_count(), 3);
    }

    #[test]
    fn test_json_highlighter() {
        let highlighter = JsonHighlighter::new();
        let styled = highlighter.highlight(r#"{"key": "value", "num": 42}"#);

        assert_eq!(styled.line_count(), 1);
        assert!(!styled.lines[0].spans.is_empty());
    }

    #[test]
    fn test_empty_line() {
        let highlighter = PlainHighlighter::new();
        let styled = highlighter.highlight("");

        assert_eq!(styled.line_count(), 1);
        assert!(styled.lines[0].spans.is_empty());
    }
}

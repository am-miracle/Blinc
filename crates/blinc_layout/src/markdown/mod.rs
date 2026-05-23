//! Markdown rendering support for blinc_layout
//!
//! This module provides markdown parsing and rendering to blinc layout elements.
//! It uses pulldown-cmark for CommonMark + GFM extension parsing.
//!
//! # Example
//!
//! ```ignore
//! use blinc_layout::prelude::*;
//! use blinc_layout::markdown::markdown;
//!
//! // Simple markdown rendering
//! let content = markdown(r#"
//! # Hello World
//!
//! This is **bold** and *italic* text.
//!
//! - List item 1
//! - List item 2
//! "#);
//!
//! // Use in your layout
//! div().flex_col().child(content)
//! ```

mod config;
mod renderer;

pub use config::MarkdownConfig;
pub use renderer::{MarkdownRenderer, markdown, markdown_light, markdown_with_config};

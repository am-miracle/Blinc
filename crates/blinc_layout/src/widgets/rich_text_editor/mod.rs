//! Rich text editor — WYSIWYG styled-prose editing.
//!
//! Composes on top of [`crate::styled_text`] (`StyledLine` / `TextSpan`) for
//! its inline content representation, the existing `Stateful` FSM for focus
//! state, and the same clipboard helpers used by `code_editor`.
//!
//! # Layout
//!
//! - [`document`] — `RichDocument`, `Block`, `BlockKind` (the data model).
//! - [`cursor`] — `DocPosition`, `Selection`, `ActiveFormat` and basic
//!   navigation helpers.
//!
//! Later phases will add:
//! - `edit` — pure functions over `&mut RichDocument` for insert / delete /
//!   format / split / merge.
//! - `state` — `RichTextState` (`Arc<Mutex<…>>`) carrying the document,
//!   cursor, selection, undo/redo stacks, and focus state.
//! - `render` — the `ElementBuilder` impl that paints the document.
//! - `shortcuts` — keyboard → edit-op dispatch.
//! - `toolbar` — composable toolbar widget.
//!
//! Public API surface is intentionally small for the MVP, but every edit
//! op is a free function so users can build their own toolbars and key
//! handlers without going through a sealed setter API.

pub mod block_ops;
pub mod cursor;
pub mod document;
pub mod edit;
pub mod editor;
pub mod format;
pub mod markdown;
pub mod render;
pub mod state;
pub mod toolbar;
pub mod wrap;

pub use block_ops::{
    indent_blocks, insert_divider_after, outdent_blocks, set_block_kind, toggle_block_kind,
};
pub use cursor::{ActiveFormat, DocPosition, Selection};
pub use document::{Block, BlockKind, RichDocument};
pub use edit::{
    delete_backward, delete_forward, delete_selection, insert_char, insert_text, soft_break,
    split_block,
};
pub use editor::rich_text_editor;
pub use format::{apply_mark_to_selection, Mark};
pub use render::{compute_line_geometry, render_document, RichTextTheme};
pub use state::{
    rich_text_state, LineGeometry, PickerState, RichTextData, RichTextState, RunGeometry, UndoEntry,
};
pub use toolbar::selection_toolbar;
pub use wrap::{wrap_styled_line, WrappedLine};

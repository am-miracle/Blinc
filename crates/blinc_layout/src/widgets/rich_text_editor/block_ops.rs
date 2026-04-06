//! Block-level transformations on `RichDocument`.
//!
//! These are pure functions over `&mut RichDocument` (mirroring the
//! style of `edit.rs` and `format.rs`) that change the *kind* or
//! *indent* of one or more blocks. Cursor / selection / undo are the
//! editor's job — these ops don't touch them.
//!
//! Block kind ops support a *toggle* convention: applying a kind that
//! every block in the range already has reverts those blocks to
//! plain `Paragraph`. Otherwise the kind is set on every block in
//! the range, regardless of what they were before. This matches the
//! "click bold while everything is bold → unbold" pattern from
//! inline marks.

use super::cursor::Selection;
use super::document::{Block, BlockKind, RichDocument};

// =============================================================================
// Public API
// =============================================================================

/// Set every block touched by `range` to `kind`.
///
/// `range` is a `Selection`; if collapsed, only the block containing
/// the cursor is affected. Returns `true` if any block actually
/// changed (kind differed before).
///
/// For toggle behaviour (revert to `Paragraph` when every block in
/// the range already has the requested kind), use [`toggle_block_kind`].
pub fn set_block_kind(doc: &mut RichDocument, range: Selection, kind: BlockKind) -> bool {
    let (start, end) = range.ordered();
    let mut changed = false;
    for idx in start.block..=end.block.min(doc.blocks.len().saturating_sub(1)) {
        let Some(block) = doc.blocks.get_mut(idx) else {
            break;
        };
        if block.kind != kind {
            block.kind = kind.clone();
            // Headings have no indent; clear it so they don't visually
            // hang from a previous list level.
            if matches!(kind, BlockKind::Heading(_)) {
                block.indent = 0;
            }
            changed = true;
        }
    }
    changed
}

/// Toggle every block touched by `range` to `kind`.
///
/// If every block in `range` already has `kind`, all of them revert
/// to `BlockKind::Paragraph`. Otherwise every block in `range` is
/// set to `kind` (whatever its previous kind was).
pub fn toggle_block_kind(doc: &mut RichDocument, range: Selection, kind: BlockKind) -> bool {
    let (start, end) = range.ordered();
    let mut all_already = true;
    let mut any = false;
    for idx in start.block..=end.block.min(doc.blocks.len().saturating_sub(1)) {
        if let Some(block) = doc.blocks.get(idx) {
            any = true;
            if !block_kind_eq(&block.kind, &kind) {
                all_already = false;
            }
        }
    }
    if !any {
        return false;
    }
    let target = if all_already {
        BlockKind::Paragraph
    } else {
        kind
    };
    set_block_kind(doc, range, target)
}

/// Increase the `indent` of every block touched by `range` by one.
///
/// Headings, dividers, and quotes are skipped — only paragraphs and
/// list items support indent. Indent is capped at `u8::MAX`.
pub fn indent_blocks(doc: &mut RichDocument, range: Selection) -> bool {
    let (start, end) = range.ordered();
    let mut changed = false;
    for idx in start.block..=end.block.min(doc.blocks.len().saturating_sub(1)) {
        let Some(block) = doc.blocks.get_mut(idx) else {
            break;
        };
        if !block_supports_indent(&block.kind) {
            continue;
        }
        let next = block.indent.saturating_add(1);
        if next != block.indent {
            block.indent = next;
            changed = true;
        }
    }
    changed
}

/// Decrease the `indent` of every block touched by `range` by one.
/// Stops at 0 (no underflow).
pub fn outdent_blocks(doc: &mut RichDocument, range: Selection) -> bool {
    let (start, end) = range.ordered();
    let mut changed = false;
    for idx in start.block..=end.block.min(doc.blocks.len().saturating_sub(1)) {
        let Some(block) = doc.blocks.get_mut(idx) else {
            break;
        };
        if !block_supports_indent(&block.kind) {
            continue;
        }
        if block.indent > 0 {
            block.indent -= 1;
            changed = true;
        }
    }
    changed
}

/// Insert a new horizontal divider block immediately after `block_idx`,
/// followed by a fresh empty paragraph (so the user has a place for
/// the cursor to land after the divider).
///
/// Returns the index of the new paragraph block.
pub fn insert_divider_after(doc: &mut RichDocument, block_idx: usize) -> usize {
    let insert_at = (block_idx + 1).min(doc.blocks.len());
    doc.blocks.insert(insert_at, Block::divider());
    let paragraph_at = insert_at + 1;
    doc.blocks.insert(paragraph_at, Block::paragraph_empty());
    paragraph_at
}

// =============================================================================
// Internals
// =============================================================================

fn block_supports_indent(kind: &BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::Paragraph | BlockKind::BulletItem | BlockKind::NumberedItem
    )
}

fn block_kind_eq(a: &BlockKind, b: &BlockKind) -> bool {
    match (a, b) {
        (BlockKind::Paragraph, BlockKind::Paragraph) => true,
        (BlockKind::Heading(x), BlockKind::Heading(y)) => x == y,
        (BlockKind::BulletItem, BlockKind::BulletItem) => true,
        (BlockKind::NumberedItem, BlockKind::NumberedItem) => true,
        (BlockKind::Quote, BlockKind::Quote) => true,
        (BlockKind::Divider, BlockKind::Divider) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::cursor::DocPosition;
    use super::*;
    use blinc_core::Color;

    fn doc(blocks: Vec<Block>) -> RichDocument {
        RichDocument::from_blocks(blocks)
    }

    fn sel(b1: usize, b2: usize) -> Selection {
        Selection {
            anchor: DocPosition::new(b1, 0, 0),
            head: DocPosition::new(b2, 0, 0),
        }
    }

    #[test]
    fn set_kind_changes_one_block() {
        let mut d = doc(vec![Block::paragraph("hi", Color::WHITE)]);
        assert!(set_block_kind(&mut d, sel(0, 0), BlockKind::Heading(2)));
        assert_eq!(d.blocks[0].kind, BlockKind::Heading(2));
    }

    #[test]
    fn set_kind_no_op_when_already_matches() {
        let mut d = doc(vec![Block::heading(1, "hi", Color::WHITE)]);
        assert!(!set_block_kind(&mut d, sel(0, 0), BlockKind::Heading(1)));
    }

    #[test]
    fn set_kind_clears_indent_for_heading() {
        let mut blocks = vec![Block::bullet("hi", Color::WHITE)];
        blocks[0].indent = 3;
        let mut d = doc(blocks);
        set_block_kind(&mut d, sel(0, 0), BlockKind::Heading(1));
        assert_eq!(d.blocks[0].indent, 0);
    }

    #[test]
    fn toggle_kind_reverts_to_paragraph_when_all_match() {
        let mut d = doc(vec![
            Block::bullet("a", Color::WHITE),
            Block::bullet("b", Color::WHITE),
        ]);
        toggle_block_kind(&mut d, sel(0, 1), BlockKind::BulletItem);
        assert_eq!(d.blocks[0].kind, BlockKind::Paragraph);
        assert_eq!(d.blocks[1].kind, BlockKind::Paragraph);
    }

    #[test]
    fn toggle_kind_sets_when_some_dont_match() {
        let mut d = doc(vec![
            Block::bullet("a", Color::WHITE),
            Block::paragraph("b", Color::WHITE),
        ]);
        toggle_block_kind(&mut d, sel(0, 1), BlockKind::BulletItem);
        assert_eq!(d.blocks[0].kind, BlockKind::BulletItem);
        assert_eq!(d.blocks[1].kind, BlockKind::BulletItem);
    }

    #[test]
    fn indent_increments_paragraph_and_bullets() {
        let mut d = doc(vec![
            Block::paragraph("a", Color::WHITE),
            Block::bullet("b", Color::WHITE),
            Block::heading(1, "c", Color::WHITE),
        ]);
        indent_blocks(&mut d, sel(0, 2));
        assert_eq!(d.blocks[0].indent, 1);
        assert_eq!(d.blocks[1].indent, 1);
        // Heading is skipped
        assert_eq!(d.blocks[2].indent, 0);
    }

    #[test]
    fn outdent_floors_at_zero() {
        let mut d = doc(vec![Block::paragraph("a", Color::WHITE)]);
        let changed = outdent_blocks(&mut d, sel(0, 0));
        assert!(!changed);
        assert_eq!(d.blocks[0].indent, 0);
    }

    #[test]
    fn outdent_walks_back_one_level() {
        let mut blocks = vec![Block::bullet("a", Color::WHITE)];
        blocks[0].indent = 2;
        let mut d = doc(blocks);
        outdent_blocks(&mut d, sel(0, 0));
        assert_eq!(d.blocks[0].indent, 1);
    }

    #[test]
    fn insert_divider_after_inserts_two_blocks() {
        let mut d = doc(vec![
            Block::paragraph("a", Color::WHITE),
            Block::paragraph("b", Color::WHITE),
        ]);
        let landing = insert_divider_after(&mut d, 0);
        assert_eq!(d.blocks.len(), 4);
        assert_eq!(d.blocks[1].kind, BlockKind::Divider);
        assert_eq!(d.blocks[2].kind, BlockKind::Paragraph);
        assert!(d.blocks[2].is_empty());
        assert_eq!(landing, 2);
        // Original blocks shift / preserve as expected
        assert_eq!(d.blocks[0].lines[0].text, "a");
        assert_eq!(d.blocks[3].lines[0].text, "b");
    }

    #[test]
    fn toggle_quote_sets_then_clears() {
        let mut d = doc(vec![Block::paragraph("hi", Color::WHITE)]);
        toggle_block_kind(&mut d, sel(0, 0), BlockKind::Quote);
        assert_eq!(d.blocks[0].kind, BlockKind::Quote);
        toggle_block_kind(&mut d, sel(0, 0), BlockKind::Quote);
        assert_eq!(d.blocks[0].kind, BlockKind::Paragraph);
    }
}

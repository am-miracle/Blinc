//! Scene operation buffer — the declarative output stream that
//! a DSL view body produces during a single render pass.
//!
//! Builtins called from compiled view code (`$Blinc$text`,
//! `$Blinc$text_int`, future widget primitives) push [`DslOp`]
//! entries onto the per-thread buffer; the embed API drains
//! the buffer after the call returns and hands the ops to
//! whichever consumer turns them into a real UI (a layout
//! tree, an HTML diff, a server-side serialisation, etc.).
//!
//! Lives in `blinc_runtime` rather than `blinc_dsl_core` because:
//!
//! - **Backend-agnostic.** Both the JIT path (Zyntax+Cranelift)
//!   and the future AOT path (Zyntax+LLVM) emit the same scene
//!   ops from the same DSL primitives. Moving the buffer here
//!   means widget code that *consumes* ops (or future host
//!   primitives that *produce* them) doesn't have to drag in
//!   the DSL compiler.
//! - **Single contention point.** Any extension crate that
//!   wants to add a primitive (`paragraph`, `image`, `button`)
//!   pushes to the same thread-local; no per-extension buffer
//!   shuffling.
//!
//! ## Threading
//!
//! Buffer is thread-local. JIT calls run synchronously on the
//! caller thread, so a builtin's push and the embed's drain
//! always pair up on the same `Vec`. Multi-threaded consumers
//! each see their own buffer, which is the right semantics for
//! "render this view on this thread".
//!
//! ## Adding new op variants
//!
//! Variants are intentionally narrow today — `Text` and
//! `IntText` cover the prototype's `text("...")` and
//! `text(N)` builtins. Real ops (containers, layout modifiers,
//! event-handler bindings, etc.) land alongside the grammar
//! expansion as new primitives surface. Each variant is one
//! constructor + the matching `$Blinc$<name>` builtin that
//! pushes it.

use std::cell::RefCell;

/// The flex axis a container's children flow along.
///
/// `Row` lays out horizontally; `Column` vertically. `Stack`
/// overlays children at the same position (z-axis composition).
/// Mirrors what `blinc_layout::Div` supports under the hood —
/// the DSL grammar's container primitives lower to one of these
/// when they land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContainerKind {
    /// Default container — no specific flex axis. Children
    /// stack vertically the same way `div()` defaults to.
    Box,
    /// Horizontal flex.
    Row,
    /// Vertical flex.
    Column,
    /// Z-stacked overlay.
    Stack,
}

/// Direction a flex spacer expands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// One declarative draw op emitted by a DSL view body.
///
/// The host drains the buffer after each `render_view` /
/// `render_component` call and turns the ops into a Blinc
/// element tree (or whichever consumer-specific shape the
/// embedder needs).
///
/// ## Two op shapes
///
/// 1. **Tree-via-markers.** [`Self::OpenContainer`] starts a
///    nesting boundary; the matching [`Self::CloseContainer`]
///    closes it. Ops emitted between the pair are children of
///    that container. Consumers reconstruct the tree by
///    walking the linear op stream and tracking depth — same
///    flat-with-markers encoding the DSL's `__slot_open__` /
///    `__slot_close__` markers already use elsewhere in the
///    pipeline.
///
/// 2. **Flat primitives.** [`Self::Text`], [`Self::IntText`],
///    [`Self::Spacer`] — no children, fully described by the
///    variant's payload. Most leaf nodes in a UI fit here.
///
/// New primitives that the grammar grows to support land here
/// in lockstep with the matching `$Blinc$<name>` builtin in
/// the JIT crate. Variants are additive — never remove or
/// rename. If a variant's payload needs to evolve, version it
/// (`SpacerV2 { ... }`) and migrate over time.
#[derive(Debug, Clone, PartialEq)]
pub enum DslOp {
    /// `text("literal")` — a single text node carrying a string.
    Text(String),

    /// `text(N)` — a single text node carrying an integer. The
    /// host stringifies on render. Distinct variant from `Text`
    /// so downstream consumers can format integers differently
    /// (alignment, locale, etc.) if they want.
    IntText(i32),

    /// Open a container scope. Every op pushed after this and
    /// before the matching [`Self::CloseContainer`] is a child
    /// of the container.
    ///
    /// Containers nest — `Open(Box) → Open(Row) → Text → Close → Close`
    /// is a Box with a single Row child that has one Text leaf.
    /// Consumers track an open-depth counter to reconstruct the
    /// tree.
    OpenContainer(ContainerKind),

    /// Close the most recently opened container scope. Matched
    /// pairs with [`Self::OpenContainer`] — the consumer's
    /// depth counter decrements on each `Close`.
    CloseContainer,

    /// Flex spacer — a sized empty box that pushes adjacent
    /// content along `axis`. `size` is in DPI-independent units
    /// (matches `blinc_layout`'s `Div::w` / `Div::h` semantics).
    Spacer { axis: Axis, size: f32 },
}

thread_local! {
    /// Per-thread scene buffer. Builtins push, the embed API
    /// drains via [`take_scene_ops`].
    static SCENE_BUFFER: RefCell<Vec<DslOp>> = const { RefCell::new(Vec::new()) };
}

/// Append an op to the current thread's scene buffer.
/// Builtins that produce visual output call this; everyone
/// else stays away.
pub fn push(op: DslOp) {
    SCENE_BUFFER.with(|b| b.borrow_mut().push(op));
}

/// Drain and return everything pushed onto the scene buffer
/// since the last call. Called by the embed API after
/// `runtime.call(...)` returns to hand the ops to the
/// consumer.
pub fn take() -> Vec<DslOp> {
    SCENE_BUFFER.with(|b| std::mem::take(&mut *b.borrow_mut()))
}

/// Reset the scene buffer without returning its contents.
/// Used by tests that want a clean slate; production code
/// should prefer [`take`] (cheap to drop the result if
/// uninterested) so a future producer that runs before
/// `take` doesn't trip on stale state.
pub fn clear() {
    SCENE_BUFFER.with(|b| b.borrow_mut().clear());
}

/// Validate that the buffer's open / close marker pairs are
/// balanced — every [`DslOp::OpenContainer`] has a matching
/// [`DslOp::CloseContainer`] at the same depth.
///
/// Returns the maximum nesting depth observed on success.
/// Returns `Err` with the index of the first violating op when
/// the stream is malformed:
///
/// - A `CloseContainer` with no matching open.
/// - The stream ends with unclosed opens.
///
/// Useful for consumers that want to fail fast on a corrupt
/// op stream, or for tests pinning the producer's invariants.
pub fn validate_balanced(ops: &[DslOp]) -> Result<usize, SceneValidationError> {
    let mut depth: usize = 0;
    let mut max_depth = 0usize;
    for (i, op) in ops.iter().enumerate() {
        match op {
            DslOp::OpenContainer(_) => {
                depth += 1;
                if depth > max_depth {
                    max_depth = depth;
                }
            }
            DslOp::CloseContainer => {
                if depth == 0 {
                    return Err(SceneValidationError::UnmatchedClose { index: i });
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    if depth != 0 {
        Err(SceneValidationError::UnclosedOpen { remaining: depth })
    } else {
        Ok(max_depth)
    }
}

/// Errors produced by [`validate_balanced`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneValidationError {
    /// A `CloseContainer` was encountered with no matching
    /// open. `index` is the position in the op stream.
    UnmatchedClose { index: usize },
    /// The op stream ended with `remaining` containers still
    /// open.
    UnclosedOpen { remaining: usize },
}

impl std::fmt::Display for SceneValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SceneValidationError::UnmatchedClose { index } => {
                write!(f, "unmatched CloseContainer at op index {index}")
            }
            SceneValidationError::UnclosedOpen { remaining } => {
                write!(f, "{remaining} container(s) left open at end of stream")
            }
        }
    }
}

impl std::error::Error for SceneValidationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_take_round_trip() {
        clear();
        push(DslOp::Text("hello".into()));
        push(DslOp::IntText(42));
        let ops = take();
        assert_eq!(ops, vec![DslOp::Text("hello".into()), DslOp::IntText(42)]);
        // Buffer drained — second take returns empty.
        assert!(take().is_empty());
    }

    #[test]
    fn clear_wipes_buffer() {
        clear();
        push(DslOp::Text("a".into()));
        push(DslOp::Text("b".into()));
        clear();
        assert!(take().is_empty());
    }

    /// A balanced open/close pair with one child reports
    /// depth = 1.
    #[test]
    fn validate_balanced_one_level() {
        let ops = vec![
            DslOp::OpenContainer(ContainerKind::Box),
            DslOp::Text("child".into()),
            DslOp::CloseContainer,
        ];
        assert_eq!(validate_balanced(&ops), Ok(1));
    }

    /// Nested open/close pairs report the deepest level.
    #[test]
    fn validate_balanced_nested() {
        let ops = vec![
            DslOp::OpenContainer(ContainerKind::Box),
            DslOp::OpenContainer(ContainerKind::Row),
            DslOp::OpenContainer(ContainerKind::Column),
            DslOp::Text("deep".into()),
            DslOp::CloseContainer,
            DslOp::CloseContainer,
            DslOp::CloseContainer,
        ];
        assert_eq!(validate_balanced(&ops), Ok(3));
    }

    /// Multiple siblings at the top level: depth = 1, not 2.
    #[test]
    fn validate_balanced_siblings_not_nested() {
        let ops = vec![
            DslOp::OpenContainer(ContainerKind::Box),
            DslOp::CloseContainer,
            DslOp::OpenContainer(ContainerKind::Row),
            DslOp::CloseContainer,
        ];
        assert_eq!(validate_balanced(&ops), Ok(1));
    }

    /// A stray Close with no preceding Open errors out and
    /// reports the violating index.
    #[test]
    fn validate_balanced_unmatched_close() {
        let ops = vec![DslOp::Text("a".into()), DslOp::CloseContainer];
        assert_eq!(
            validate_balanced(&ops),
            Err(SceneValidationError::UnmatchedClose { index: 1 })
        );
    }

    /// Unclosed opens error out and report how many are left
    /// hanging.
    #[test]
    fn validate_balanced_unclosed_opens() {
        let ops = vec![
            DslOp::OpenContainer(ContainerKind::Box),
            DslOp::OpenContainer(ContainerKind::Row),
        ];
        assert_eq!(
            validate_balanced(&ops),
            Err(SceneValidationError::UnclosedOpen { remaining: 2 })
        );
    }

    /// Spacer ops are flat — they don't affect depth.
    #[test]
    fn validate_balanced_spacer_neutral() {
        let ops = vec![
            DslOp::Spacer {
                axis: Axis::Vertical,
                size: 16.0,
            },
            DslOp::OpenContainer(ContainerKind::Box),
            DslOp::Spacer {
                axis: Axis::Horizontal,
                size: 8.0,
            },
            DslOp::CloseContainer,
        ];
        assert_eq!(validate_balanced(&ops), Ok(1));
    }
}

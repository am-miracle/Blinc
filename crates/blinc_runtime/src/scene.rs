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

/// One declarative draw op emitted by a DSL view body.
///
/// The host drains the buffer after each `render_view` /
/// `render_component` call and turns the ops into a Blinc
/// element tree (or whichever consumer-specific shape the
/// embedder needs).
#[derive(Debug, Clone, PartialEq)]
pub enum DslOp {
    /// `text("literal")` — a single text node carrying a string.
    Text(String),
    /// `text(N)` — a single text node carrying an integer. The
    /// host stringifies on render. Distinct variant from `Text`
    /// so downstream consumers can format integers differently
    /// (alignment, locale, etc.) if they want.
    IntText(i32),
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
}

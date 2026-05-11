//! Per-thread signal tables.
//!
//! "Signal" is the DSL term for a named runtime cell that user
//! code reads from and the host writes into — the typical use
//! is bridging widget state into FSM tick guards (a scroll
//! position, a progress percentage, etc.). The DSL surface is
//! `signal <name>: <T>` + `<name>.get()` inside guard
//! expressions; the DSL pipeline rewrites those into extern
//! calls (`__signal_get_i32` / `__signal_get_f64`) that the
//! runtime resolves by name.
//!
//! Lives in `blinc_runtime` rather than `blinc_dsl_core` because:
//!
//! - **Backend-agnostic.** Both the JIT path (Zyntax+Cranelift)
//!   and the future AOT path (Zyntax+LLVM) compile the same DSL
//!   surface into calls against the same `__signal_get_*`
//!   externs. The storage shape doesn't change between
//!   backends, so the table belongs in the substrate everyone
//!   shares.
//! - **Widget integration.** A widget that wants to feed a
//!   value into a DSL tick guard (e.g. a `ScrollView` writing
//!   its current position into a `progress` signal so a Loader
//!   FSM can react) needs to call `set_*` from widget code. If
//!   the table lived in `blinc_dsl_core`, the widget crate
//!   would have to depend on the DSL compiler — a heavy cycle.
//!
//! ## Threading
//!
//! Tables are thread-local. Zyntax JIT calls run synchronously
//! on the caller thread, so host-side state populated before a
//! call is visible inside the call. Cross-thread signal sharing
//! is not supported by this layer; embedders that need it
//! should layer their own `Mutex<HashMap>` on top and update
//! via the `set_*` API from the worker thread that's about to
//! issue a call.
//!
//! ## Extensibility
//!
//! Each primitive type that the DSL allows for a `signal` decl
//! has its own table (i32, f64 today). Adding a new type means
//! a new thread-local table + accessor pair here, plus a
//! matching extern + `@builtin` alias in `blinc_dsl_core`.
//! String-typed signals are deferred until the broader Blinc
//! string-return ABI is settled.

use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    static I32_TABLE: RefCell<HashMap<String, i32>> = RefCell::new(HashMap::new());
    static F64_TABLE: RefCell<HashMap<String, f64>> = RefCell::new(HashMap::new());
    static STR_TABLE: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

// =====================================================================
// i32 signals
// =====================================================================

/// Set the current value of an i32-typed signal. Subsequent
/// DSL reads of `<name>.get()` (after the
/// `resolve_signal_calls` pass rewrites them to
/// `__signal_get_i32("<name>")`) see this value.
pub fn set_i32(name: &str, value: i32) {
    I32_TABLE.with(|t| {
        t.borrow_mut().insert(name.to_string(), value);
    });
}

/// Read the current value of an i32-typed signal. Returns
/// `None` when the signal hasn't been set this thread.
pub fn get_i32(name: &str) -> Option<i32> {
    I32_TABLE.with(|t| t.borrow().get(name).copied())
}

/// Read with a default of `0` when the signal hasn't been
/// set. This matches the surface the JIT extern presents to
/// DSL code — guards never see an `Option`, just an `i32`, so
/// the unset case has to resolve to *some* concrete value.
/// Embedders that need a different default should seed the
/// table via `set_i32` before the first JIT call.
pub fn get_i32_or_default(name: &str) -> i32 {
    get_i32(name).unwrap_or(0)
}

// =====================================================================
// f64 signals
// =====================================================================

/// f64 mirror of [`set_i32`]. Same semantics, separate table.
pub fn set_f64(name: &str, value: f64) {
    F64_TABLE.with(|t| {
        t.borrow_mut().insert(name.to_string(), value);
    });
}

/// f64 mirror of [`get_i32`].
pub fn get_f64(name: &str) -> Option<f64> {
    F64_TABLE.with(|t| t.borrow().get(name).copied())
}

/// f64 mirror of [`get_i32_or_default`]. Returns `0.0` when
/// the signal hasn't been set.
pub fn get_f64_or_default(name: &str) -> f64 {
    get_f64(name).unwrap_or(0.0)
}

// =====================================================================
// String signals
// =====================================================================

/// Set the current value of a string-typed signal.
///
/// Stored as an owned `String`. Reads return clones, so the
/// embedder is free to mutate / replace the stored value
/// without disturbing in-flight reads.
pub fn set_str(name: &str, value: impl Into<String>) {
    STR_TABLE.with(|t| {
        t.borrow_mut().insert(name.to_string(), value.into());
    });
}

/// Read the current value of a string-typed signal. Returns
/// `None` when the signal hasn't been set this thread.
pub fn get_str(name: &str) -> Option<String> {
    STR_TABLE.with(|t| t.borrow().get(name).cloned())
}

/// Read with a default of `""` when the signal hasn't been
/// set. Matches the surface the JIT extern presents to DSL
/// code — string signals never see an `Option`, just a
/// `String`, so the unset case has to resolve to *some*
/// concrete value. Embedders that need a non-empty default
/// should seed the table via [`set_str`] before the first
/// JIT call.
pub fn get_str_or_default(name: &str) -> String {
    get_str(name).unwrap_or_default()
}

/// Clear all signal tables on the calling thread. Tests reach
/// for this to start from a clean slate; production code
/// typically doesn't need it (signals naturally persist across
/// JIT calls within a thread's lifetime).
pub fn clear_all() {
    I32_TABLE.with(|t| t.borrow_mut().clear());
    F64_TABLE.with(|t| t.borrow_mut().clear());
    STR_TABLE.with(|t| t.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip a value through `set_i32` / `get_i32`.
    #[test]
    fn i32_round_trip() {
        clear_all();
        assert_eq!(get_i32("count"), None);
        set_i32("count", 42);
        assert_eq!(get_i32("count"), Some(42));
        set_i32("count", -7);
        assert_eq!(get_i32("count"), Some(-7));
    }

    /// `get_i32_or_default` falls back to `0` when unset and
    /// returns the stored value otherwise.
    #[test]
    fn i32_default_when_unset() {
        clear_all();
        assert_eq!(get_i32_or_default("missing"), 0);
        set_i32("present", 99);
        assert_eq!(get_i32_or_default("present"), 99);
    }

    /// f64 mirror — round-trip + default. Separate table from
    /// i32, so the same name in both tables is independent.
    #[test]
    fn f64_round_trip_and_default() {
        clear_all();
        assert_eq!(get_f64("progress"), None);
        assert_eq!(get_f64_or_default("progress"), 0.0);

        set_f64("progress", 0.75);
        assert_eq!(get_f64("progress"), Some(0.75));
        assert_eq!(get_f64_or_default("progress"), 0.75);

        // The i32 table doesn't see the f64 write.
        set_i32("progress", 100);
        assert_eq!(get_i32("progress"), Some(100));
        assert_eq!(get_f64("progress"), Some(0.75));
    }

    /// String mirror — round-trip + default. Separate table.
    #[test]
    fn str_round_trip_and_default() {
        clear_all();
        assert_eq!(get_str("title"), None);
        assert_eq!(get_str_or_default("title"), "");

        set_str("title", "hello");
        assert_eq!(get_str("title").as_deref(), Some("hello"));
        assert_eq!(get_str_or_default("title"), "hello");

        // Different table from i32/f64 — no aliasing.
        set_i32("title", 42);
        assert_eq!(get_str("title").as_deref(), Some("hello"));
        assert_eq!(get_i32("title"), Some(42));
    }

    /// `clear_all` wipes all three tables.
    #[test]
    fn clear_all_wipes_all_tables() {
        set_i32("a", 1);
        set_f64("b", 2.0);
        set_str("c", "three");
        clear_all();
        assert_eq!(get_i32("a"), None);
        assert_eq!(get_f64("b"), None);
        assert_eq!(get_str("c"), None);
    }
}

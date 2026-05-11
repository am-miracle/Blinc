//! Per-thread signal table.
//!
//! "Signal" is the DSL term for a named runtime cell that user
//! code reads from and the host writes into — the typical use
//! is bridging widget state into FSM tick guards (a scroll
//! position, a progress percentage, etc.). The DSL surface is
//! `signal <name>: <T>` + `<name>.get()` inside guard
//! expressions; the DSL pipeline rewrites those into extern
//! calls (`__signal_get_i32` / `__signal_get_f64` /
//! `__signal_get_string`) that the runtime resolves by name.
//!
//! Storage is a single thread-local `HashMap<String,
//! crate::value::Value>` — values are stored in their full
//! [`Value`] form so the table naturally handles complex types
//! (structs, enums, arrays, optionals, ...) the moment the JIT
//! / AOT pipeline grows externs that return them. Today the
//! externs use i32 / f64 / string; the typed `set_*` / `get_*`
//! accessors below are thin wrappers over the underlying
//! `Value`-keyed store.
//!
//! Lives in `blinc_runtime` rather than `blinc_dsl_core`
//! because:
//!
//! - **Backend-agnostic.** Both the JIT path (Zyntax+Cranelift)
//!   and the future AOT path (Zyntax+LLVM) compile the same DSL
//!   surface into calls against the same `__signal_get_*`
//!   externs. The storage shape doesn't change between
//!   backends, so the table belongs in the substrate everyone
//!   shares.
//!
//! - **Widget integration.** A widget that wants to feed a
//!   value into a DSL tick guard (e.g. a `ScrollView` writing
//!   its current position into a `progress` signal so a Loader
//!   FSM can react) needs to call `set_*` from widget code. If
//!   the table lived in `blinc_dsl_core`, the widget crate
//!   would have to depend on the DSL compiler — a heavy cycle.
//!
//! ## Threading
//!
//! The table is thread-local. Zyntax JIT calls run
//! synchronously on the caller thread, so host-side state
//! populated before a call is visible inside the call.
//! Cross-thread signal sharing is not supported by this layer;
//! embedders that need it should layer their own
//! `Mutex<HashMap>` on top and update via the `set_*` API from
//! the worker thread that's about to issue a call.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::value::Value;

thread_local! {
    /// The underlying `Value`-keyed table. All typed accessors
    /// route through this single map so the substrate has one
    /// place to clear / introspect / hot-reload from.
    static TABLE: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

// =====================================================================
// Generic Value accessors
// =====================================================================
//
// These work in `Value` directly — for complex types (structs,
// enums, arrays) callers use these and pattern-match on the
// returned `Value`.

/// Set a signal to an arbitrary [`Value`]. Replaces any prior
/// entry for `name`.
pub fn set(name: &str, value: Value) {
    TABLE.with(|t| {
        t.borrow_mut().insert(name.to_string(), value);
    });
}

/// Read the current value of a signal. Returns `None` when
/// the signal hasn't been set on this thread.
pub fn get(name: &str) -> Option<Value> {
    TABLE.with(|t| t.borrow().get(name).cloned())
}

/// Clear the signal table on the calling thread. Tests reach
/// for this to start from a clean slate; production code
/// typically doesn't need it (signals naturally persist
/// across JIT calls within a thread's lifetime).
pub fn clear_all() {
    TABLE.with(|t| t.borrow_mut().clear());
}

// =====================================================================
// i32 typed accessors
// =====================================================================
//
// Wrap the `Value`-keyed table. JIT externs and most host
// callers work in primitive types, so the typed accessors
// stay the ergonomic surface.

/// Set the current value of an i32-typed signal. Stored
/// internally as `Value::Int(i64)`; readers using `get_i32`
/// truncate back.
pub fn set_i32(name: &str, value: i32) {
    set(name, Value::from_i32(value));
}

/// Read the current value of an i32-typed signal. Returns
/// `None` when the signal hasn't been set, or when the stored
/// value isn't actually an int (e.g. the host put a struct in
/// there — the JIT extern would have produced garbage if it
/// reached this case).
pub fn get_i32(name: &str) -> Option<i32> {
    get(name).and_then(|v| v.as_i32())
}

/// Read with a default of `0` when the signal hasn't been
/// set. Matches the surface the JIT extern presents to DSL
/// code — guards never see an `Option`, just an `i32`, so the
/// unset case has to resolve to *some* concrete value.
/// Embedders that need a different default should seed the
/// table via `set_i32` before the first JIT call.
pub fn get_i32_or_default(name: &str) -> i32 {
    get_i32(name).unwrap_or(0)
}

// =====================================================================
// f64 typed accessors
// =====================================================================

/// f64 mirror of [`set_i32`]. Stored as `Value::Float(f64)`.
pub fn set_f64(name: &str, value: f64) {
    set(name, Value::from_f64(value));
}

/// f64 mirror of [`get_i32`].
pub fn get_f64(name: &str) -> Option<f64> {
    get(name).and_then(|v| v.as_f64())
}

/// f64 mirror of [`get_i32_or_default`]. Returns `0.0` when
/// the signal hasn't been set.
pub fn get_f64_or_default(name: &str) -> f64 {
    get_f64(name).unwrap_or(0.0)
}

// =====================================================================
// String typed accessors
// =====================================================================

/// Set the current value of a string-typed signal. Stored as
/// `Value::String(owned)`.
pub fn set_str(name: &str, value: impl Into<String>) {
    set(name, Value::from_string(value));
}

/// Read the current value of a string-typed signal. Returns
/// `None` when unset (or when the stored value isn't a
/// string).
pub fn get_str(name: &str) -> Option<String> {
    match get(name) {
        Some(Value::String(s)) => Some(s),
        _ => None,
    }
}

/// Read with a default of `""` when the signal hasn't been
/// set. Matches the surface the JIT extern presents to DSL
/// code.
pub fn get_str_or_default(name: &str) -> String {
    get_str(name).unwrap_or_default()
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

    /// f64 mirror — round-trip + default. Single table now,
    /// but the typed accessors keep i32 and f64 readers
    /// independent: a name written as f64 reads back via
    /// `get_f64`, not `get_i32`.
    #[test]
    fn f64_round_trip_and_default() {
        clear_all();
        assert_eq!(get_f64("progress"), None);
        assert_eq!(get_f64_or_default("progress"), 0.0);

        set_f64("progress", 0.75);
        assert_eq!(get_f64("progress"), Some(0.75));
        assert_eq!(get_f64_or_default("progress"), 0.75);

        // Writing i32 to the same name OVERWRITES the f64
        // (single table). `get_f64` then returns None
        // because the stored Value is now `Int`, not `Float`.
        set_i32("progress", 100);
        assert_eq!(get_i32("progress"), Some(100));
        assert_eq!(get_f64("progress"), None);
    }

    /// String round-trip + default.
    #[test]
    fn str_round_trip_and_default() {
        clear_all();
        assert_eq!(get_str("title"), None);
        assert_eq!(get_str_or_default("title"), "");

        set_str("title", "hello");
        assert_eq!(get_str("title").as_deref(), Some("hello"));
        assert_eq!(get_str_or_default("title"), "hello");
    }

    /// Complex types — set a struct, retrieve it via the
    /// generic `get`. Proves the substrate's single-table
    /// design supports complex types end-to-end.
    #[test]
    fn complex_value_round_trip() {
        clear_all();
        let point = Value::Struct {
            type_name: "Point".into(),
            fields: std::collections::HashMap::from([
                ("x".to_string(), Value::Int(3)),
                ("y".to_string(), Value::Int(4)),
            ]),
        };
        set("origin", point.clone());
        let read_back = get("origin").unwrap();
        assert_eq!(read_back, point);

        // Typed accessors return `None` for a struct value —
        // they only succeed when the stored shape matches.
        assert_eq!(get_i32("origin"), None);
        assert_eq!(get_str("origin"), None);
    }

    /// `clear_all` wipes everything regardless of type.
    #[test]
    fn clear_all_wipes_all_tables() {
        set_i32("a", 1);
        set_f64("b", 2.0);
        set_str("c", "three");
        set("d", Value::Array(vec![Value::Int(1), Value::Int(2)]));
        clear_all();
        assert_eq!(get_i32("a"), None);
        assert_eq!(get_f64("b"), None);
        assert_eq!(get_str("c"), None);
        assert_eq!(get("d"), None);
    }
}

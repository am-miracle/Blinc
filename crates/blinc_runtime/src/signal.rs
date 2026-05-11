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
//! ZyntaxValue>`. Both JIT and AOT-compiled DSL code produces
//! `ZyntaxValue` natively (it's Zyntax's canonical runtime
//! value representation), so the substrate stores it directly
//! — no parallel value enum. Complex types (structs, enums,
//! arrays, optionals, generics) flow through unchanged the
//! moment the JIT / AOT externs grow to handle them.
//!
//! Lives in `blinc_runtime` rather than `blinc_dsl_core`
//! because both backends share this storage; a widget that
//! wants to feed a value into a DSL tick guard reaches for
//! `blinc_runtime::signal::set_*` without depending on the
//! DSL compiler.
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

use zyntax_embed::ZyntaxValue;

thread_local! {
    /// The underlying `ZyntaxValue`-keyed table. All typed
    /// accessors route through this single map so the substrate
    /// has one place to clear / introspect / hot-reload from.
    static TABLE: RefCell<HashMap<String, ZyntaxValue>> = RefCell::new(HashMap::new());
}

// =====================================================================
// Generic ZyntaxValue accessors
// =====================================================================
//
// These work in `ZyntaxValue` directly — for complex types
// (structs, enums, arrays) callers use these and pattern-match
// on the returned `ZyntaxValue`.

/// Set a signal to an arbitrary [`ZyntaxValue`]. Replaces any
/// prior entry for `name`.
pub fn set(name: &str, value: ZyntaxValue) {
    TABLE.with(|t| {
        t.borrow_mut().insert(name.to_string(), value);
    });
}

/// Read the current value of a signal. Returns `None` when
/// the signal hasn't been set on this thread.
pub fn get(name: &str) -> Option<ZyntaxValue> {
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
// Wrap the generic table. JIT externs and most host callers
// work in primitive types, so the typed accessors stay the
// ergonomic surface. Each accessor pattern-matches on the
// matching `ZyntaxValue` variant — `Int(i64)` for the integer
// signals, `Float(f64)` for floats, `String(String)` for
// strings.

/// Set the current value of an i32-typed signal. Stored
/// internally as `ZyntaxValue::Int(i64)`; readers using
/// `get_i32` truncate back.
pub fn set_i32(name: &str, value: i32) {
    set(name, ZyntaxValue::Int(value as i64));
}

/// Read the current value of an i32-typed signal. Returns
/// `None` when the signal hasn't been set, or when the stored
/// value isn't actually an int (e.g. the host put a struct in
/// there).
pub fn get_i32(name: &str) -> Option<i32> {
    match get(name) {
        Some(ZyntaxValue::Int(n)) => i32::try_from(n).ok(),
        _ => None,
    }
}

/// Read with a default of `0` when the signal hasn't been
/// set. Matches the surface the JIT extern presents to DSL
/// code.
pub fn get_i32_or_default(name: &str) -> i32 {
    get_i32(name).unwrap_or(0)
}

// =====================================================================
// f64 typed accessors
// =====================================================================

/// f64 mirror of [`set_i32`].
pub fn set_f64(name: &str, value: f64) {
    set(name, ZyntaxValue::Float(value));
}

/// f64 mirror of [`get_i32`].
pub fn get_f64(name: &str) -> Option<f64> {
    match get(name) {
        Some(ZyntaxValue::Float(n)) => Some(n),
        _ => None,
    }
}

/// f64 mirror of [`get_i32_or_default`].
pub fn get_f64_or_default(name: &str) -> f64 {
    get_f64(name).unwrap_or(0.0)
}

// =====================================================================
// String typed accessors
// =====================================================================

/// Set the current value of a string-typed signal. Stored as
/// `ZyntaxValue::String(owned)`.
pub fn set_str(name: &str, value: impl Into<String>) {
    set(name, ZyntaxValue::String(value.into()));
}

/// Read the current value of a string-typed signal. Returns
/// `None` when unset (or when the stored value isn't a
/// string).
pub fn get_str(name: &str) -> Option<String> {
    match get(name) {
        Some(ZyntaxValue::String(s)) => Some(s),
        _ => None,
    }
}

/// Read with a default of `""` when the signal hasn't been
/// set.
pub fn get_str_or_default(name: &str) -> String {
    get_str(name).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i32_round_trip() {
        clear_all();
        assert_eq!(get_i32("count"), None);
        set_i32("count", 42);
        assert_eq!(get_i32("count"), Some(42));
        set_i32("count", -7);
        assert_eq!(get_i32("count"), Some(-7));
    }

    #[test]
    fn i32_default_when_unset() {
        clear_all();
        assert_eq!(get_i32_or_default("missing"), 0);
        set_i32("present", 99);
        assert_eq!(get_i32_or_default("present"), 99);
    }

    /// f64 round-trip — single table, but typed readers stay
    /// type-strict: a name written as f64 reads back via
    /// `get_f64`, not `get_i32`. Overwriting with i32 makes
    /// `get_f64` return None (stored shape no longer matches).
    #[test]
    fn f64_round_trip_and_default() {
        clear_all();
        assert_eq!(get_f64("progress"), None);
        assert_eq!(get_f64_or_default("progress"), 0.0);

        set_f64("progress", 0.75);
        assert_eq!(get_f64("progress"), Some(0.75));
        assert_eq!(get_f64_or_default("progress"), 0.75);

        set_i32("progress", 100);
        assert_eq!(get_i32("progress"), Some(100));
        assert_eq!(get_f64("progress"), None);
    }

    #[test]
    fn str_round_trip_and_default() {
        clear_all();
        assert_eq!(get_str("title"), None);
        assert_eq!(get_str_or_default("title"), "");

        set_str("title", "hello");
        assert_eq!(get_str("title").as_deref(), Some("hello"));
        assert_eq!(get_str_or_default("title"), "hello");
    }

    /// Complex shape — Struct flows through the substrate
    /// without normalising. Proves the single-table design
    /// holds for arbitrary `ZyntaxValue` shapes.
    #[test]
    fn complex_value_round_trip() {
        clear_all();
        let point = ZyntaxValue::Struct {
            type_name: "Point".into(),
            fields: HashMap::from([
                ("x".to_string(), ZyntaxValue::Int(3)),
                ("y".to_string(), ZyntaxValue::Int(4)),
            ]),
        };
        set("origin", point.clone());
        let read_back = get("origin").unwrap();
        assert_eq!(read_back, point);

        // Typed accessors return None for a struct value.
        assert_eq!(get_i32("origin"), None);
        assert_eq!(get_str("origin"), None);
    }

    #[test]
    fn clear_all_wipes_table() {
        set_i32("a", 1);
        set_f64("b", 2.0);
        set_str("c", "three");
        set(
            "d",
            ZyntaxValue::Array(vec![ZyntaxValue::Int(1), ZyntaxValue::Int(2)]),
        );
        clear_all();
        assert_eq!(get_i32("a"), None);
        assert_eq!(get_f64("b"), None);
        assert_eq!(get_str("c"), None);
        assert_eq!(get("d"), None);
    }
}

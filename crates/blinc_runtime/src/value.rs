//! Runtime value representation for substrate-stored data.
//!
//! `Value` is the universal shape the substrate (signal
//! tables, future event payloads, devtools-side
//! introspection) uses to hold values across the JIT/AOT
//! boundary. It mirrors `zyntax_embed::ZyntaxValue` so the JIT
//! backend can translate freely with no information loss, but
//! lives here (not in `zyntax_embed`) so the substrate doesn't
//! pull in Cranelift / the JIT runtime.
//!
//! ## Why a separate enum instead of using `ZyntaxValue`
//!
//! `ZyntaxValue` is defined in `zyntax_embed`, which depends on
//! `zyntax_compiler` and Cranelift. An AOT-only app linking
//! `blinc_runtime` + a future AOT codegen crate should not need
//! Cranelift in its dependency graph. Mirroring the shape
//! keeps the substrate JIT-free; the JIT backend provides
//! `From<ZyntaxValue> for Value` (and the inverse) as
//! conversion glue at its own boundary.
//!
//! The shape is intentionally close-to-identical to
//! `ZyntaxValue` so the translation is mechanical — variant
//! names, payload shapes, ownership semantics all match.
//!
//! ## Adding variants
//!
//! New variants are additive — never remove or rename. If a
//! shape needs to evolve, version it (`StructV2 { ... }`) and
//! migrate over time. Same rule [`super::scene::DslOp`] uses.

use std::collections::HashMap;

/// A runtime value flowing through the substrate.
///
/// Mirrors `zyntax_embed::ZyntaxValue`'s shape so the JIT
/// backend can `From`-translate cleanly, but the type lives
/// here so AOT-only apps don't drag the JIT runtime in.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Void / unit — no payload.
    Void,

    /// Null. Distinct from `Void`: null is "I have a value
    /// slot and it's empty" (an optional that's None at
    /// runtime), void is "I don't have a value at all".
    Null,

    Bool(bool),

    /// Signed integer. Held as `i64` for headroom — narrower
    /// DSL types (`i32`) downcast lossily on the way out; the
    /// substrate doesn't track the original bit width.
    Int(i64),

    /// Unsigned integer. Same headroom rationale as `Int`.
    UInt(u64),

    /// Floating point. Held as `f64`; `f32` values lose
    /// precision on widening but that's the existing DSL
    /// convention (see the f64-only TypedLiteral::Float).
    Float(f64),

    /// String. Owned `String`, UTF-8.
    String(String),

    /// Array — sequence of values. Variants inside need not
    /// be homogeneous (the substrate doesn't enforce
    /// element-type uniformity), though consumers usually
    /// expect they are.
    Array(Vec<Value>),

    /// Map — string-keyed dictionary. Limit to string keys
    /// for symmetry with `ZyntaxValue::Map`; richer key types
    /// fold into [`Value::Struct`] if needed.
    Map(HashMap<String, Value>),

    /// Struct — named-fields aggregate. `type_name` is the
    /// user-facing struct type name (e.g. `"Counter"` for a
    /// `component Counter { ... }`); `fields` are
    /// alphabetical / declaration-order at the writer's
    /// discretion (the substrate preserves whatever insertion
    /// order the producer used since `HashMap` doesn't, but
    /// consumers shouldn't depend on iteration order).
    Struct {
        type_name: String,
        fields: HashMap<String, Value>,
    },

    /// Enum variant. `type_name` identifies the enum,
    /// `variant` the case; `data` carries the variant payload
    /// (None for unit variants).
    Enum {
        type_name: String,
        variant: String,
        data: Option<Box<Value>>,
    },

    /// Optional value. `Box<Option<Value>>` matches
    /// `ZyntaxValue::Optional` exactly — slightly awkward
    /// shape but keeps round-tripping cost-free.
    Optional(Box<Option<Value>>),

    /// Tuple — anonymous positional aggregate.
    Tuple(Vec<Value>),
}

impl Value {
    /// User-readable type name. Useful for diagnostic
    /// messages, devtools panels, and the
    /// "unknown variant" fallback path when consumers want
    /// to log what they couldn't handle.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Void => "void",
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::UInt(_) => "uint",
            Value::Float(_) => "float",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Map(_) => "map",
            Value::Struct { .. } => "struct",
            Value::Enum { .. } => "enum",
            Value::Optional(_) => "optional",
            Value::Tuple(_) => "tuple",
        }
    }
}

// =====================================================================
// Convenience constructors / extractors for primitive types
// =====================================================================
//
// The signal externs and most embedder-facing call sites work
// in primitive types — they shouldn't have to `match` on the
// `Value` enum for the common cases. These helpers cover the
// happy path; consumers that need richer logic pattern-match
// directly.

impl Value {
    /// Construct a primitive Value from an `i32`. Widens to
    /// `i64` internally — `as_i32` undoes the widening with a
    /// range check.
    pub fn from_i32(n: i32) -> Self {
        Value::Int(n as i64)
    }

    /// Construct a primitive Value from an `f64`.
    pub fn from_f64(n: f64) -> Self {
        Value::Float(n)
    }

    /// Construct a primitive Value from a string (anything
    /// convertible to `String`).
    pub fn from_string(s: impl Into<String>) -> Self {
        Value::String(s.into())
    }

    /// Try to interpret as `i32`. Returns `None` if the value
    /// isn't an integer, or if the stored `i64` doesn't fit in
    /// `i32` (overflow / underflow). DSL signal values use
    /// `i32` natively — the truncation case is defensive.
    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Value::Int(n) => i32::try_from(*n).ok(),
            _ => None,
        }
    }

    /// Try to interpret as `f64`. Returns `None` if the value
    /// isn't a float.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(n) => Some(*n),
            _ => None,
        }
    }

    /// Try to interpret as a `&str`. Returns `None` if the
    /// value isn't a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `type_name` returns the lowercase user-readable label
    /// for each variant.
    #[test]
    fn type_name_matches_variant() {
        assert_eq!(Value::Void.type_name(), "void");
        assert_eq!(Value::Null.type_name(), "null");
        assert_eq!(Value::Bool(true).type_name(), "bool");
        assert_eq!(Value::Int(42).type_name(), "int");
        assert_eq!(Value::Float(0.5).type_name(), "float");
        assert_eq!(Value::String("hi".into()).type_name(), "string");
        assert_eq!(Value::Array(vec![]).type_name(), "array");
        assert_eq!(Value::Tuple(vec![]).type_name(), "tuple");
    }

    /// Primitive constructors / extractors round-trip
    /// cleanly.
    #[test]
    fn primitive_round_trip() {
        let v = Value::from_i32(42);
        assert_eq!(v.as_i32(), Some(42));
        assert_eq!(v.as_f64(), None);

        let v = Value::from_f64(0.75);
        assert_eq!(v.as_f64(), Some(0.75));
        assert_eq!(v.as_i32(), None);

        let v = Value::from_string("hello");
        assert_eq!(v.as_str(), Some("hello"));
    }

    /// `as_i32` rejects out-of-range `i64` values (defensive
    /// — the JIT path never stores oversized values today,
    /// but consumers shouldn't have to trust that).
    #[test]
    fn as_i32_rejects_overflow() {
        let huge = Value::Int(i64::MAX);
        assert_eq!(huge.as_i32(), None);
    }

    /// Complex shapes — struct + enum + optional. Pin that
    /// the substrate stores them without normalising.
    #[test]
    fn complex_value_shapes() {
        let s = Value::Struct {
            type_name: "Point".into(),
            fields: HashMap::from([
                ("x".to_string(), Value::Int(3)),
                ("y".to_string(), Value::Int(4)),
            ]),
        };
        let Value::Struct { type_name, fields } = &s else {
            panic!()
        };
        assert_eq!(type_name, "Point");
        assert_eq!(fields.get("x").and_then(Value::as_i32), Some(3));

        let some = Value::Optional(Box::new(Some(Value::Int(7))));
        let Value::Optional(inner) = &some else {
            panic!()
        };
        match inner.as_ref() {
            Some(Value::Int(n)) => assert_eq!(*n, 7),
            other => panic!("expected Some(Int(7)), got {other:?}"),
        }

        let none = Value::Optional(Box::new(None));
        let Value::Optional(inner) = &none else {
            panic!()
        };
        assert!(inner.is_none());

        let variant = Value::Enum {
            type_name: "LoaderState".into(),
            variant: "Loading".into(),
            data: None,
        };
        let Value::Enum { variant, data, .. } = &variant else {
            panic!()
        };
        assert_eq!(variant, "Loading");
        assert!(data.is_none());
    }
}

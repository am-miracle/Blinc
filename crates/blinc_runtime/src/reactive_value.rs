//! `Reactive<T>` — the first typed DSL value at the FFI boundary.
//!
//! The DSL has three reactive shapes that any consumer (cn widgets,
//! built-in Div / Text, user components, third-party widget packs)
//! can accept on a single prop:
//!
//! * **Literal** — `cn.Foo(prop = 0.5)` or `Div(w = 120.0)`. Static
//!   value baked into the call site.
//! * **Signal-bound** — `cn.Foo(prop = my_signal)`. Live-bound to a
//!   `signal foo: T` declaration; updates patch the property channel
//!   on `signal.set(...)`.
//! * **Computed** — `cn.Foo(prop = computed { … } : T)`. Live-bound
//!   to a derived value that auto-tracks the signals it reads inside
//!   the closure.
//!
//! `Reactive<T>` is the union the host extern thunk sees after
//! decoding the two-slot FFI shape (`tag: i32`, `payload: i64`)
//! synthesised by `lower_reactive_args` at the DSL call site. The
//! wrapper consumes the typed enum and routes it to the cn-side
//! `IntoReactive<T>` channel, or any other binding-capable surface.
//!
//! ## Layering
//!
//! Lives in `blinc_runtime` because it's shared between JIT
//! (`blinc_dsl_core`) and the AOT path (future `blinc_dsl_aot`).
//! Anything `blinc_core::reactive`-aware can use it; widget packs
//! don't need a parallel definition.
//!
//! ## What's NOT here (yet)
//!
//! * `Reactive<String>` — string payloads need ZRTL-pointer
//!   ownership semantics across the FFI; lands in a focused
//!   follow-up.
//! * Multi-typed dispatch via a single Reactive over `Any` — out of
//!   scope. The macro generates per-`T` decoders.
//! * Effect-style props — `effect { … }` stays a side-effect-only
//!   surface (no value channel); the existing
//!   `__blinc_effect__` host extern handles it.

use blinc_core::reactive::{Computed, Signal, SignalId};

// Tag values used at the wire-format FFI boundary. The lowering
// pass writes one of these into the `tag` slot per Reactive prop;
// the host extern thunk pattern-matches on the value to pick the
// payload interpretation.
//
// Sentinel choices are deliberate:
// * `0 = Literal` — matches the macro's `Default::default()` for
//   any `i32` field, so an un-supplied Reactive prop falls through
//   as a literal default without special handling.
// * `1` and `2` follow naturally; new tags appended as needed.
pub const REACTIVE_TAG_LITERAL: i32 = 0;
pub const REACTIVE_TAG_SIGNAL: i32 = 1;
pub const REACTIVE_TAG_COMPUTED: i32 = 2;

/// Typed reactive value the host extern thunk decodes for any DSL
/// arg whose prop slot was declared `Reactive<T>` on the Rust side.
///
/// Pattern-match on this in widget wrappers; the cn-side
/// `IntoReactive<T>` channel accepts every variant via the matching
/// adapter (the wrapper's `to_cn_builder` glue).
///
/// `Clone` is derived; `Debug` is skipped because `Computed<T>`
/// holds an `Arc<Mutex<ReactiveGraph>>` that doesn't impl Debug.
/// Callers debug-print the literal value or the underlying ids
/// directly via the per-variant accessors.
#[derive(Clone)]
pub enum Reactive<T: Clone + Send + 'static> {
    Literal(T),
    Signal(Signal<T>),
    Computed(Computed<T>),
}

impl<T: Clone + Send + 'static> Reactive<T> {
    /// Lookup the current value if the binding resolves. Falls back
    /// to the supplied default for unresolvable signal / derived
    /// handles (the graph has been reset, the slotmap key was
    /// reclaimed, etc.). Useful for widgets that want a synchronous
    /// snapshot at build time rather than the cn-side
    /// `IntoReactive<T>` channel.
    pub fn get_or_else(&self, fallback: T) -> T {
        match self {
            Self::Literal(v) => v.clone(),
            Self::Signal(s) => s.try_get().unwrap_or(fallback),
            Self::Computed(c) => c.try_get().unwrap_or(fallback),
        }
    }
}

// =====================================================================
// FFI decoders — one per concrete `T`.
// =====================================================================
//
// Wire format: a `Reactive<T>` prop lands at the FFI thunk as two
// scalar slots, `tag: i32` and `payload: i64`. The tag picks the
// payload interpretation; the per-`T` decoder reconstructs the
// typed enum.
//
// For literals:
// * `i32`  — payload's low 32 bits.
// * `f64`  — `f64::from_bits(payload as u64)`.
// * `bool` — `payload != 0`.

impl Reactive<i32> {
    pub fn decode_ffi(tag: i32, payload: i64) -> Self {
        match tag {
            REACTIVE_TAG_SIGNAL => {
                Self::Signal(Signal::from_id(SignalId::from_raw(payload as u64)))
            }
            REACTIVE_TAG_COMPUTED => Self::Computed(Computed::from_id(
                blinc_core::reactive::DerivedId::from_raw(payload as u64),
            )),
            _ => Self::Literal(payload as i32),
        }
    }
}

impl Reactive<f64> {
    pub fn decode_ffi(tag: i32, payload: i64) -> Self {
        match tag {
            REACTIVE_TAG_SIGNAL => {
                Self::Signal(Signal::from_id(SignalId::from_raw(payload as u64)))
            }
            REACTIVE_TAG_COMPUTED => Self::Computed(Computed::from_id(
                blinc_core::reactive::DerivedId::from_raw(payload as u64),
            )),
            _ => Self::Literal(f64::from_bits(payload as u64)),
        }
    }
}

impl Reactive<bool> {
    pub fn decode_ffi(tag: i32, payload: i64) -> Self {
        match tag {
            REACTIVE_TAG_SIGNAL => {
                Self::Signal(Signal::from_id(SignalId::from_raw(payload as u64)))
            }
            REACTIVE_TAG_COMPUTED => Self::Computed(Computed::from_id(
                blinc_core::reactive::DerivedId::from_raw(payload as u64),
            )),
            _ => Self::Literal(payload != 0),
        }
    }
}

// =====================================================================
// Default impl — required by the `#[extern_widget]` macro's
// `#[skip]` field handling: any field excluded from the FFI gets
// `Default::default()` in the generated constructor. Reactive<T>
// for primitive T defaults to `Literal(T::default())` so the macro
// can synthesize an unsupplied reactive prop slot without panic.
// =====================================================================

impl<T: Clone + Send + Default + 'static> Default for Reactive<T> {
    fn default() -> Self {
        Self::Literal(T::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_i32_roundtrip() {
        let r = Reactive::<i32>::decode_ffi(REACTIVE_TAG_LITERAL, 42);
        assert!(matches!(r, Reactive::Literal(42)));
    }

    #[test]
    fn literal_f64_roundtrip() {
        let payload = f64::to_bits(2.5) as i64;
        let r = Reactive::<f64>::decode_ffi(REACTIVE_TAG_LITERAL, payload);
        let Reactive::Literal(v) = r else {
            panic!("expected Literal");
        };
        assert!((v - 2.5).abs() < f64::EPSILON);
    }

    #[test]
    fn literal_bool_roundtrip() {
        assert!(matches!(
            Reactive::<bool>::decode_ffi(REACTIVE_TAG_LITERAL, 0),
            Reactive::Literal(false)
        ));
        assert!(matches!(
            Reactive::<bool>::decode_ffi(REACTIVE_TAG_LITERAL, 1),
            Reactive::Literal(true)
        ));
    }

    #[test]
    fn unknown_tag_falls_back_to_literal() {
        // Defensive: a future tag value reaching an older binary
        // shouldn't crash — it lands in the Literal branch with
        // whatever the payload happened to be.
        let r = Reactive::<i32>::decode_ffi(99, 7);
        assert!(matches!(r, Reactive::Literal(7)));
    }

    #[test]
    fn signal_tag_constructs_handle() {
        // Build a fresh signal, get its id, decode it back through
        // the wire format, confirm the round-trip preserves the id.
        let s = blinc_core::reactive::signal(123_i32);
        let raw = s.id().to_raw() as i64;
        let r = Reactive::<i32>::decode_ffi(REACTIVE_TAG_SIGNAL, raw);
        let Reactive::Signal(sig) = r else {
            panic!("expected Signal");
        };
        assert_eq!(sig.id(), s.id());
        // Sanity: the reconstructed handle can read the current value.
        assert_eq!(sig.try_get(), Some(123));
    }
}

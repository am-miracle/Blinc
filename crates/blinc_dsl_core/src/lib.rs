//! Blinc DSL core — Zyntax-embedded grammar, runtime engine, and host
//! glue.
//!
//! # Status
//!
//! Risk-reduction prototype. Covers the `view { text("...") }` slice
//! end-to-end so we can validate Zyntax integration before scaling up
//! to the full grammar. See ROADMAP §3.9 for the phasing.
//!
//! # Pipeline
//!
//! ```text
//! .blinc source
//!     |
//!     v
//! [Grammar2::from_source(BLINC_GRAMMAR)] -> TypedProgram
//!     |
//!     v
//! [ZyntaxRuntime::compile_typed_program]  -> HirModule (cached)
//!     |
//!     v
//! [runtime.call::<()>("render_view", &[])]
//!     |
//!     v
//! [host scene buffer drained via take_scene_ops()]
//!     |
//!     v
//! [ElementBuilder tree handed to Blinc renderer]
//! ```
//!
//! Builtins are registered statically (`runtime.register_function`) —
//! no `.zrtl` plugin discovery from disk. Blinc's set of builtins is
//! fixed at link time, which matches the zynml "patterns to NOT copy"
//! recommendation in our research notes.

use std::cell::RefCell;
use std::path::Path;
use std::sync::{Arc, Mutex};

use thiserror::Error;
use zyntax_embed::{
    Grammar2, Grammar2Error, NativeSignature, NativeType, RuntimeError, TypeTag, ZrtlSigFlags,
    ZrtlSymbolSig, ZyntaxRuntime, ZyntaxValue,
};

/// Mirror of `zyntax_compiler::zrtl::MAX_PARAMS` (16). Not re-exported
/// from `zyntax_embed`, so we redeclare locally; the value is part of
/// the wire ABI for `ZrtlSymbolSig` and won't change without a major
/// version bump on the embed crate.
const ZRTL_MAX_PARAMS: usize = 16;
use zyntax_typed_ast::type_registry::{PrimitiveType, Type};
use zyntax_typed_ast::{typed_node, Span, TypedProgram, TypedStatement};

/// The embedded Blinc DSL grammar source.
///
/// Baked at compile time so apps don't ship a `.zyn` file separately.
/// Mirrors the zynml `ZYNML_GRAMMAR` pattern (zynml/src/lib.rs:77 in
/// the Zyntax sibling repo).
pub const BLINC_GRAMMAR: &str = include_str!("../grammar/blinc.zyn");

// =====================================================================
// Scene buffer — host-owned op stream populated by DSL builtins
// =====================================================================

/// One declarative draw op emitted by the DSL during a `render_view`
/// call. The host drains the buffer after each invocation and turns
/// it into a Blinc element tree.
///
/// Kept intentionally narrow for the prototype. Real ops (containers,
/// layout modifiers, event handlers, etc.) land alongside the grammar
/// expansion in phase 2 of the prototype.
#[derive(Debug, Clone)]
pub enum DslOp {
    /// `text("literal")` — a single text node carrying a string.
    Text(String),
    /// `int_text(N)` — a single text node carrying an integer. The
    /// host stringifies on render. Distinct variant from `Text` so
    /// downstream consumers can format integers differently
    /// (alignment, locale, etc.) if they want.
    IntText(i32),
}

thread_local! {
    /// Per-thread scene buffer. Builtins push, the embed API drains.
    /// Thread-local because Zyntax invocations are synchronous on the
    /// caller thread; multi-threaded callers would each see their own
    /// buffer, which is the right semantics for "render this view".
    static SCENE_BUFFER: RefCell<Vec<DslOp>> = const { RefCell::new(Vec::new()) };

    /// Per-thread i32-signal table. Populated by `BlincDsl::set_signal_i32`,
    /// read by the `blinc_signal_get_i32` extern when DSL code calls
    /// `<name>.get()` on an i32 signal (after `resolve_signal_calls`
    /// has rewritten it to `__signal_get_i32("<name>")`).
    ///
    /// Thread-local for the same reason as `SCENE_BUFFER` — Zyntax
    /// JIT calls run synchronously on the caller's thread, so
    /// host-side state populated before a call is visible inside
    /// the call. Cross-thread signal sharing is not supported by
    /// this layer; embedders that need it should layer their own
    /// `Mutex<HashMap>` on top and update via `set_signal_i32`
    /// from the worker thread that's about to issue a call.
    static SIGNAL_TABLE_I32: RefCell<std::collections::HashMap<String, i32>> =
        RefCell::new(std::collections::HashMap::new());

    /// Per-thread f64-signal table. Same shape as `SIGNAL_TABLE_I32`
    /// but for `signal <name>: f64` declarations. Read by
    /// `blinc_signal_get_f64`, populated by `set_signal_f64`.
    /// Useful for guards like `progress.get() >= 1.0` where the
    /// signal value drives a Harel-style data transition.
    static SIGNAL_TABLE_F64: RefCell<std::collections::HashMap<String, f64>> =
        RefCell::new(std::collections::HashMap::new());
}

/// Drain and return everything pushed onto the scene buffer since the
/// last call. Called by the embed API after `runtime.call(...)` returns.
pub fn take_scene_ops() -> Vec<DslOp> {
    SCENE_BUFFER.with(|b| std::mem::take(&mut *b.borrow_mut()))
}

// =====================================================================
// Builtins
// =====================================================================

/// `$Blinc$text` — the only builtin in the prototype slice.
///
/// Cranelift passes Zyntax `string` arguments as a pointer into the
/// length-prefixed `ZyntaxString` layout: `[i32 length][utf8 bytes...]`
/// (see `zyntax_embed::string` and the `ZyntaxString::HEADER_SIZE`
/// constant — 4 bytes). We pull the length, read the bytes, and strip
/// the surrounding quotes the grammar preserved (`string_literal`'s
/// `text()` capture returns `"hello"` whole, not `hello`).
///
/// # Safety
///
/// Called by Zyntax's JIT through a function pointer registered via
/// [`ZyntaxRuntime::register_function`]. The runtime guarantees the
/// argument shape matches the registered signature (one
/// `NativeType::Ptr`); any deviation is a Zyntax bug, not ours.
extern "C" fn blinc_text(s_ptr: *const i32) {
    if s_ptr.is_null() {
        tracing::warn!("$Blinc$text called with null pointer");
        return;
    }

    // SAFETY: the runtime guarantees `s_ptr` points at a Zyntax
    // length-prefixed UTF-8 buffer when the signature is `Ptr` for a
    // string parameter.
    let raw = unsafe {
        let len = std::ptr::read_unaligned(s_ptr) as usize;
        let body = (s_ptr as *const u8).add(std::mem::size_of::<i32>());
        let bytes = std::slice::from_raw_parts(body, len);
        std::str::from_utf8(bytes).unwrap_or("<invalid utf-8>")
    };

    // The grammar's `string_literal` capture preserves the surrounding
    // quotes (`text()` returns `"hello"` not `hello`). Strip them
    // before the host sees the value.
    let stripped = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);

    SCENE_BUFFER.with(|b| b.borrow_mut().push(DslOp::Text(stripped.to_string())));
}

/// `__signal_get_i32` — host implementation of the i32 signal
/// accessor synthesised by the `resolve_signal_calls` pass.
///
/// The pass rewrites every `<name>.get()` (where `<name>` is an
/// i32 signal declared via `signal <name>: i32`) into
/// `__signal_get_i32("<name>")`. At JIT time this Rust function
/// runs, looks the name up in the per-thread `SIGNAL_TABLE_I32`,
/// and returns the current value (or `0` when the signal hasn't
/// been set, which mirrors Default::default for i32 — embedders
/// that want a different fallback can call `set_signal_i32`
/// during `BlincDsl::new` to seed defaults).
///
/// # Safety
///
/// Same contract as [`blinc_text`]. The runtime guarantees
/// `name_ptr` points at a Zyntax length-prefixed UTF-8 buffer
/// when the registered signature has `String` for the parameter.
extern "C" fn blinc_signal_get_i32(name_ptr: *const i32) -> i32 {
    if name_ptr.is_null() {
        tracing::warn!("__signal_get_i32 called with null name pointer");
        return 0;
    }

    // SAFETY: Zyntax guarantees the length-prefixed Zyntax-string
    // layout when the registered parameter type is String.
    let name = unsafe {
        let len = std::ptr::read_unaligned(name_ptr) as usize;
        let body = (name_ptr as *const u8).add(std::mem::size_of::<i32>());
        let bytes = std::slice::from_raw_parts(body, len);
        std::str::from_utf8(bytes).unwrap_or("<invalid utf-8>")
    };

    // The signal-name string literal generated by the rewrite is
    // already unquoted (StringLiteral construction strips the
    // quotes — interpreter.rs:553). Defensive trim for parity with
    // blinc_text in case the synthesis changes upstream.
    let stripped = name
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(name);

    SIGNAL_TABLE_I32.with(|t| t.borrow().get(stripped).copied().unwrap_or(0))
}

/// `__signal_get_f64` — host implementation of the f64 signal
/// accessor. Mirrors `blinc_signal_get_i32` in shape; the only
/// differences are the lookup table and the return type.
/// Default fallback is `0.0` for unset signals.
///
/// # Safety
///
/// Same contract as [`blinc_signal_get_i32`]. The runtime
/// guarantees `name_ptr` points at a Zyntax length-prefixed
/// UTF-8 buffer.
extern "C" fn blinc_signal_get_f64(name_ptr: *const i32) -> f64 {
    if name_ptr.is_null() {
        tracing::warn!("__signal_get_f64 called with null name pointer");
        return 0.0;
    }

    let name = unsafe {
        let len = std::ptr::read_unaligned(name_ptr) as usize;
        let body = (name_ptr as *const u8).add(std::mem::size_of::<i32>());
        let bytes = std::slice::from_raw_parts(body, len);
        std::str::from_utf8(bytes).unwrap_or("<invalid utf-8>")
    };
    let stripped = name
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(name);

    SIGNAL_TABLE_F64.with(|t| t.borrow().get(stripped).copied().unwrap_or(0.0))
}

/// `$Blinc$text_int` — integer arm of `text(...)`. Pushes an integer
/// onto the scene buffer.
///
/// Probes the i32 ABI through Cranelift's JIT: the host receives an
/// actual `i32` register, not a length-prefixed pointer. Matches what
/// the grammar's `integer` terminal lowers to via `parse_int(text())`.
///
/// # Safety
///
/// Same contract as [`blinc_text`]. The runtime guarantees the
/// argument shape matches the registered `NativeType::I32`.
extern "C" fn blinc_text_int(n: i32) {
    SCENE_BUFFER.with(|b| b.borrow_mut().push(DslOp::IntText(n)));
}

// =====================================================================
// Errors
// =====================================================================

/// Top-level error type for the embed API. Wraps Zyntax's own error
/// types so callers see one taxonomy. Each variant carries the
/// underlying Zyntax error so `Display` / `source()` still surfaces
/// the original diagnostic with file:line:col spans where Zyntax
/// produced them.
#[derive(Debug, Error)]
pub enum BlincDslError {
    /// `Grammar2::from_source(BLINC_GRAMMAR)` failed. This is a Blinc
    /// bug — the grammar is baked at compile time, so any compile-
    /// failure is on us, not the user.
    #[error("blinc grammar compile failed: {0}")]
    Grammar(#[from] Grammar2Error),

    /// `runtime.compile_typed_program(...)` failed — the user's
    /// `.blinc` source has a parse / type / lowering error. The
    /// inner string carries the diagnostic with a file:line span.
    #[error("blinc compile error: {0}")]
    Compile(String),

    /// `runtime.call::<T>(...)` failed at execution time.
    #[error("blinc runtime error: {0}")]
    Runtime(#[from] RuntimeError),

    /// Reading the source file off disk failed.
    #[error("blinc source io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type BlincDslResult<T> = std::result::Result<T, BlincDslError>;

// =====================================================================
// FSM registry
// =====================================================================
//
// Module-aware identity for fsms compiled by the DSL. Keys combine
// the Zyntax module name (currently always `"main"` — Zyntax compiles
// every source into a single module today, see
// `zyntax_embed/src/runtime.rs:1373`) with the FSM's `TypeId` from
// the program's `type_registry`. When Zyntax surfaces real per-source
// modules later, the same key shape extends without breaking changes:
// `("foo", TypeId)` and `("bar", TypeId)` for same-named fsms in
// different modules can coexist.
//
// Why not bare-string keys: two fsms named `Loader` in different
// modules would collide on a string key. `InternedString` doesn't help
// either — `InternedString::new_global("Loader")` returns the same
// handle process-wide regardless of source.
//
// Why not `HirId`: `HirId` is generated during HIR lowering after
// compilation, isn't accessible at parse time, and is opaque
// (`Uuid`-backed) — it works for runtime symbol lookup but not as a
// stable identity exposed at the DSL surface.

/// Identity of an fsm in the global registry: the Zyntax module the
/// fsm is compiled in plus its `TypeId` within that module's type
/// registry. Stable within a process run (TypeIds come from a
/// process-global atomic counter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FsmId {
    /// The Zyntax module name the fsm lives in. Today this is always
    /// `"main"` because Zyntax compiles each source into a single
    /// module. Future per-source / per-package modules will surface
    /// distinct values here without changing the rest of the registry
    /// API.
    pub module: zyntax_typed_ast::InternedString,
    /// The fsm's enum `TypeId`, looked up from the program's
    /// `type_registry` after `compile_source`. Used as the registry
    /// key for fast lookup; user-facing `dispatch::<Loader>(event)`
    /// resolves to this id via Zyntax's type machinery.
    pub type_id: zyntax_typed_ast::type_registry::TypeId,
}

/// Tick-driven guard transition record. The original DSL guard
/// expression (the `<expr>` after `when`) is lifted into a
/// stand-alone top-level function during the post-parse pass so
/// it survives compile and is callable at dispatch time as an
/// ordinary Zyntax-compiled symbol. `guard_fn` carries the
/// generated function's symbol name; resolving it through
/// `runtime.call::<bool>(name, &[])` evaluates the guard with
/// current signal values.
///
/// Why a generated function rather than carrying the AST node
/// directly: Zyntax's runtime exposes `call(symbol, args)` for
/// invocation, but no general-purpose "evaluate this AST" entry
/// point. Wrapping the expression in a function gives us a
/// callable symbol with no extra runtime infrastructure. The
/// generated name is `__fsm_tick_guard_<FsmName>_<idx>__`, where
/// `<idx>` is the guard's position in the fsm's declaration order
/// (deterministic, stable across runs of the same source).
#[derive(Debug, Clone)]
pub struct TickGuard {
    pub from: zyntax_typed_ast::InternedString,
    pub to: zyntax_typed_ast::InternedString,
    /// Synthesised guard-function symbol name. `None` only when
    /// the parse-time pass couldn't extract an expression (a
    /// malformed `__fsm_tick__` marker). In normal flow this is
    /// always `Some`.
    pub guard_fn: Option<zyntax_typed_ast::InternedString>,
}

/// One event-driven transition. Names match the DSL surface:
/// `on <from>.<event> -> <to>`.
#[derive(Debug, Clone)]
pub struct EventTransition {
    pub from: zyntax_typed_ast::InternedString,
    pub event: zyntax_typed_ast::InternedString,
    pub to: zyntax_typed_ast::InternedString,
}

/// The runtime definition of an fsm — populated by the host when
/// the fsm's `__fsm_meta__` body executes (each marker call inside
/// mutates the entry). Owned by the `FsmRegistry` keyed by `FsmId`.
#[derive(Debug, Clone, Default)]
pub struct FsmDefinition {
    /// Initial state name (variant of the fsm's state enum).
    pub initial: Option<zyntax_typed_ast::InternedString>,
    /// Event-driven transitions in declaration order. Same order as
    /// the source so dispatch can iterate match-arm-style.
    pub transitions: Vec<EventTransition>,
    /// Tick-driven guards in declaration order. Currently the guard
    /// expression isn't carried — see `TickGuard` doc.
    pub tick_guards: Vec<TickGuard>,
    /// Bare fsm name (from the begin marker), useful for diagnostic
    /// messages. The authoritative identity is `FsmId`.
    pub name: Option<zyntax_typed_ast::InternedString>,
}

impl FsmDefinition {
    /// Resolve an event-driven transition. Returns the target
    /// state's name (variant of the fsm's state enum) when there's
    /// a transition matching `(from = current, event = event)`, or
    /// `None` if no rule applies.
    ///
    /// Linear scan in declaration order — match-arm-style. The
    /// first matching rule wins. Authors who want priority semantics
    /// rely on declaration order in the source; the post-parse pass
    /// preserves it (`populate_fsm_registry_pass` walks the AST in
    /// statement order).
    ///
    /// `&str` arguments rather than `InternedString` because the
    /// dispatch caller is typically holding either bare runtime
    /// strings (from a host event channel) or compile-time literals
    /// — converting at the boundary keeps the call site terse. The
    /// implementation interns once for comparison.
    pub fn step_event(
        &self,
        current: &str,
        event: &str,
    ) -> Option<zyntax_typed_ast::InternedString> {
        let current_i = zyntax_typed_ast::InternedString::new_global(current);
        let event_i = zyntax_typed_ast::InternedString::new_global(event);
        self.transitions
            .iter()
            .find(|t| t.from == current_i && t.event == event_i)
            .map(|t| t.to)
    }
}

/// Process-wide registry of fsm definitions populated as the host
/// runs each fsm's `__fsm_meta__` method. Lookup is by `FsmId`;
/// dispatch sites resolve the id from the user-facing fsm enum
/// type.
#[derive(Debug, Default)]
pub struct FsmRegistry {
    fsms: std::collections::HashMap<FsmId, FsmDefinition>,
}

impl FsmRegistry {
    /// Create an empty registry. Most callers should prefer the
    /// process-wide `global_fsm_registry()` accessor — the registry
    /// is host-managed singleton state, not something an embedder
    /// typically wants to instantiate per-instance.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert / update an fsm definition. Replaces any existing
    /// entry with the same `FsmId` — used when a hot-reload re-runs
    /// `__fsm_meta__` for an already-known fsm.
    pub fn upsert(&mut self, id: FsmId, def: FsmDefinition) {
        self.fsms.insert(id, def);
    }

    /// Look up an fsm by id. Returns `None` if no `__fsm_meta__`
    /// has been run for this `(module, type_id)` pair.
    pub fn get(&self, id: &FsmId) -> Option<&FsmDefinition> {
        self.fsms.get(id)
    }

    /// Mutable lookup. Used internally by the marker builtins to
    /// append transitions to the top-of-stack fsm's definition.
    pub fn get_mut(&mut self, id: &FsmId) -> Option<&mut FsmDefinition> {
        self.fsms.get_mut(id)
    }

    /// Number of registered fsms. Useful for tests and diagnostics.
    pub fn len(&self) -> usize {
        self.fsms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fsms.is_empty()
    }

    /// Iterate over all registered (id, definition) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&FsmId, &FsmDefinition)> {
        self.fsms.iter()
    }

    /// Remove an fsm from the registry. Used during hot-reload when
    /// a fsm decl gets removed from a source file — the host must
    /// drop the stale entry so dispatch fails loudly rather than
    /// silently using a definition for a type that no longer exists.
    pub fn remove(&mut self, id: &FsmId) -> Option<FsmDefinition> {
        self.fsms.remove(id)
    }

    /// Find an fsm by its source-level name within a given module.
    /// Returns `None` if no fsm of that name has been registered for
    /// the module. Useful as the entry point for callers that don't
    /// have an `FsmId` in hand — e.g. user code in Rust that holds
    /// the fsm name as a string after parsing the DSL source.
    ///
    /// Linear scan; for typical app sizes (handful of fsms per
    /// module) this is fine. If the registry grows past dozens of
    /// fsms per module a name → FsmId secondary index is the
    /// natural next step.
    pub fn find_by_name(
        &self,
        module: zyntax_typed_ast::InternedString,
        name: &str,
    ) -> Option<FsmId> {
        let needle = zyntax_typed_ast::InternedString::new_global(name);
        self.fsms
            .iter()
            .find(|(id, def)| id.module == module && def.name == Some(needle))
            .map(|(id, _)| *id)
    }

    /// Convenience: look up an fsm by id and resolve a transition
    /// in one call. Returns the target state's name when
    /// `(current, event)` matches a registered transition, `None`
    /// otherwise (no fsm registered, or no matching rule).
    ///
    /// Equivalent to `self.get(id).and_then(|d| d.step_event(...))`
    /// but lets callers avoid the explicit `get` lookup at the
    /// dispatch site.
    pub fn step_event(
        &self,
        id: &FsmId,
        current: &str,
        event: &str,
    ) -> Option<zyntax_typed_ast::InternedString> {
        self.get(id).and_then(|d| d.step_event(current, event))
    }
}

/// A live instance of a DSL-defined fsm — pairs an `FsmId` with the
/// current state name. Holds enough state for an embedder to drive
/// transitions without committing to a particular widget integration:
/// dispatch events / ticks, read the current state, reset to initial.
///
/// This is the dependency-free bridge to Blinc's Stateful pattern.
/// Wrapping an `FsmInstance` inside a `Stateful<S>` impl is one
/// integration shape, but it's not the only one — `FsmInstance`
/// works equally well as a field in any reactive container or
/// widget closure that needs string-keyed state.
///
/// # State representation
///
/// Current state is held as `InternedString` of the variant name
/// (e.g. `"Idle"`, `"Loading"`). This keeps the bridge dynamic —
/// no compile-time mapping between the DSL fsm's variants and a
/// user-defined Rust enum is required. Embedders that want a
/// strongly-typed enum on top can wrap with their own conversion
/// shim; for prototype use cases, the string-keyed shape is the
/// short path to "wire UI → DSL fsm".
///
/// # Example
///
/// ```ignore
/// let dsl = BlincDsl::new()?;
/// dsl.compile_source(/* fsm Loader { ... } */, "loader.blinc")?;
///
/// let mut loader = FsmInstance::new(&dsl, "main", "Loader")?;
/// assert_eq!(loader.current(), "Idle");
///
/// // Wire to a button click:
/// button("Start").on_click(move |_| {
///     loader.dispatch_event(&dsl, "Start");
///     // loader.current() → "Loading"
/// });
/// ```
#[derive(Debug, Clone)]
pub struct FsmInstance {
    /// Identity of the fsm definition this instance follows.
    /// Resolved via `FsmRegistry::find_by_name` at construction
    /// time so subsequent dispatches don't pay the lookup cost.
    pub id: FsmId,
    /// Current state name. Mutated in place by `dispatch_event` /
    /// `tick` when a transition fires.
    pub current: zyntax_typed_ast::InternedString,
}

impl FsmInstance {
    /// Create a new instance pinned to a fsm registered in the
    /// global registry. The instance starts in the fsm's declared
    /// initial state. Returns `None` if no fsm of the given name
    /// is registered for the module, or if the fsm was registered
    /// without an initial state.
    pub fn new(_dsl: &BlincDsl, module: &str, fsm_name: &str) -> Option<Self> {
        let module_i = zyntax_typed_ast::InternedString::new_global(module);
        let id = with_fsm_registry(|r| r.find_by_name(module_i, fsm_name))?;
        let initial = with_fsm_registry(|r| r.get(&id).and_then(|d| d.initial))?;
        Some(Self {
            id,
            current: initial,
        })
    }

    /// Current state name as a borrowed `&str`. Resolves from the
    /// instance's stored `InternedString`.
    pub fn current(&self) -> String {
        self.current.resolve_global().unwrap_or_default()
    }

    /// Dispatch an event by name. Returns `true` if a transition
    /// fired (and `current` has been updated to the new state),
    /// `false` if no rule matched. Mirrors the
    /// `StateTransitions::on_event` shape Blinc widgets expect:
    /// "did anything change?" maps to "should the UI rebuild?".
    pub fn dispatch_event(&mut self, _dsl: &BlincDsl, event: &str) -> bool {
        let current_str = self.current();
        let next = with_fsm_registry(|r| r.step_event(&self.id, &current_str, event));
        if let Some(to) = next {
            self.current = to;
            true
        } else {
            false
        }
    }

    /// Tick. Walks the registered tick-guards via `BlincDsl::step_tick`
    /// (which JIT-evaluates each guard), updates `current` if any
    /// fires, returns whether a transition happened. Errors from
    /// the JIT call propagate.
    pub fn tick(&mut self, dsl: &BlincDsl) -> BlincDslResult<bool> {
        let current_str = self.current();
        let next = dsl.step_tick(&self.id, &current_str)?;
        if let Some(to) = next {
            self.current = to;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Reset to the fsm's initial state. Useful when a higher-level
    /// flow restarts a sub-state-machine.
    pub fn reset(&mut self) {
        if let Some(initial) = with_fsm_registry(|r| r.get(&self.id).and_then(|d| d.initial)) {
            self.current = initial;
        }
    }
}

/// Process-wide fsm registry. Host marker builtins (registered in a
/// follow-up commit) read and mutate this through the `with_*`
/// accessors below. Stored as `OnceLock<Mutex<FsmRegistry>>` so
/// embedders that build multiple `BlincDsl` instances in the same
/// process share one consistent view.
static GLOBAL_FSM_REGISTRY: std::sync::OnceLock<std::sync::Mutex<FsmRegistry>> =
    std::sync::OnceLock::new();

fn fsm_registry_lock() -> std::sync::MutexGuard<'static, FsmRegistry> {
    GLOBAL_FSM_REGISTRY
        .get_or_init(|| std::sync::Mutex::new(FsmRegistry::new()))
        .lock()
        .expect("BlincDsl global FsmRegistry mutex poisoned")
}

/// Run a closure with shared access to the global fsm registry.
/// Use this from dispatch sites that need to resolve a fsm
/// definition by `FsmId`.
pub fn with_fsm_registry<R>(f: impl FnOnce(&FsmRegistry) -> R) -> R {
    let guard = fsm_registry_lock();
    f(&guard)
}

/// Run a closure with mutable access. Used internally by the
/// marker builtins; embedders shouldn't normally need this — the
/// registry is populated automatically when source compiles.
pub fn with_fsm_registry_mut<R>(f: impl FnOnce(&mut FsmRegistry) -> R) -> R {
    let mut guard = fsm_registry_lock();
    f(&mut guard)
}

// =====================================================================
// FSM dispatch synthesis (post-parse)
// =====================================================================

/// Recognition heuristic for `signal <name>: <T>` decls. The
/// grammar action's `Function` constructor in
/// `zyn_peg/src/runtime2/interpreter.rs:898` hardcodes
/// `link_name: None`, so we can't tag signal decls with a marker
/// link_name from the action — instead we identify them by shape:
/// extern function, no body, no parameters, primitive return,
/// and `link_name: None`. The latter discriminates against host
/// builtins auto-injected by `inject_builtin_externs`
/// (zyntax_embed/src/grammar2.rs:280) which always set
/// `link_name: Some(<target_symbol>)`.
fn is_signal_decl(func: &zyntax_typed_ast::typed_ast::TypedFunction) -> bool {
    func.is_external
        && func.body.is_none()
        && func.params.is_empty()
        && func.link_name.is_none()
        && matches!(func.return_type, Type::Primitive(_))
}

/// Resolve DSL signal references. Walks the program for
/// `signal <name>: <T>` declarations (recognised by `is_signal_decl`
/// above), records (name, type), then rewrites every
/// `<name>.get()` method call into a host-extern call
/// `__signal_get_<T>("<name>")`. The signal extern decls are
/// stripped before the program reaches Zyntax's compile path so
/// the bare-name extern doesn't collide with anything at link
/// time.
///
/// Why this shape:
///
///   * One host extern per primitive type (`__signal_get_i32`,
///     `__signal_get_string`, etc.) keeps the host registration
///     cost O(types), not O(signals). Adding a new signal is
///     pure DSL — no host code.
///   * `<name>.get()` syntax matches how DSL authors think about
///     signals (`State<T>::get()` is the standard accessor).
///     Internally we route through name + type discrimination at
///     the boundary; the DSL surface stays uniform.
///   * Stripping the signal decls before compile avoids two
///     problems: (a) the marker `link_name` would fail to link,
///     and (b) the bare-named extern (e.g. `count`) might shadow
///     other top-level callables.
///
/// Currently only primitive return types (`i32`, `string`) are
/// handled — adding more is one match arm here plus a host extern.
/// Struct returns route through Zyntax's Dyn Boxed machinery and
/// don't need this pass at all (the DSL author writes
/// `user.get().age` and Zyntax codegens the field load directly).
fn resolve_signal_calls(program: &mut TypedProgram) {
    use std::collections::HashMap;
    use zyntax_typed_ast::typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedLiteral};
    use zyntax_typed_ast::InternedString;

    // -----------------------------------------------------------------
    // Phase 1: collect signal name → return type. Walk extern fns
    // tagged with the magic link_name marker. The pass doesn't
    // require signals be declared before use (PEG already orders
    // top_level_item alternates), but we collect them all up front
    // anyway so the rewrite walker can see signals declared after
    // their first usage too.
    // -----------------------------------------------------------------
    let mut signals: HashMap<InternedString, Type> = HashMap::new();
    for decl in &program.declarations {
        let TypedDeclaration::Function(func) = &decl.node else {
            continue;
        };
        if !is_signal_decl(func) {
            continue;
        }
        signals.insert(func.name, func.return_type.clone());
    }

    if signals.is_empty() {
        return;
    }

    // -----------------------------------------------------------------
    // Phase 2: walk every expression in the program and rewrite
    // `<sig_name>.get()` (a `TypedExpression::MethodCall` on a
    // `Variable` receiver) into a `__signal_get_<T>("<name>")`
    // host-extern call.
    // -----------------------------------------------------------------
    fn typed_signal_extern_name(ty: &Type) -> Option<&'static str> {
        match ty {
            Type::Primitive(PrimitiveType::I32) => Some("__signal_get_i32"),
            Type::Primitive(PrimitiveType::F64) => Some("__signal_get_f64"),
            Type::Primitive(PrimitiveType::String) => Some("__signal_get_string"),
            // Add a match arm + a matching host builtin to extend.
            _ => None,
        }
    }

    fn rewrite_expr(
        expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>,
        signals: &HashMap<InternedString, Type>,
    ) {
        // Recursively rewrite children FIRST so nested signal calls
        // (e.g. `text(count.get())`) get the rewrite applied to the
        // inner expression before we look at the outer.
        match &mut expr.node {
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, signals);
                rewrite_expr(&mut b.right, signals);
            }
            TypedExpression::Unary(u) => {
                rewrite_expr(&mut u.operand, signals);
            }
            TypedExpression::Call(c) => {
                rewrite_expr(&mut c.callee, signals);
                for a in &mut c.positional_args {
                    rewrite_expr(a, signals);
                }
            }
            TypedExpression::Field(f) => {
                rewrite_expr(&mut f.object, signals);
            }
            TypedExpression::Index(idx) => {
                rewrite_expr(&mut idx.object, signals);
                rewrite_expr(&mut idx.index, signals);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for item in items {
                    rewrite_expr(item, signals);
                }
            }
            TypedExpression::MethodCall(mc) => {
                rewrite_expr(&mut mc.receiver, signals);
                for a in &mut mc.positional_args {
                    rewrite_expr(a, signals);
                }
            }
            TypedExpression::Block(block) => {
                rewrite_block(block, signals);
            }
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition, signals);
                rewrite_expr(&mut if_expr.then_branch, signals);
                rewrite_expr(&mut if_expr.else_branch, signals);
            }
            // Other variants (Literal, Variable, etc.) have no
            // rewritable children at this layer.
            _ => {}
        }

        // Now check the current node: is it a method call of the
        // shape `<sig_name>.get()` we should rewrite?
        if let TypedExpression::MethodCall(mc) = &expr.node {
            let TypedExpression::Variable(receiver_name) = &mc.receiver.node else {
                return;
            };
            let Some(sig_ty) = signals.get(receiver_name) else {
                return;
            };
            if mc.method.resolve_global().as_deref() != Some("get") {
                return;
            }
            if !mc.positional_args.is_empty() {
                return;
            }

            let Some(extern_name) = typed_signal_extern_name(sig_ty) else {
                // Unsupported signal type — leave the method call
                // alone. The compile path will surface the
                // unresolved symbol if it's actually used.
                return;
            };

            let call = TypedExpression::Call(TypedCall {
                callee: Box::new(zyntax_typed_ast::TypedNode::new(
                    TypedExpression::Variable(InternedString::new_global(extern_name)),
                    Type::Unknown,
                    expr.span,
                )),
                positional_args: vec![zyntax_typed_ast::TypedNode::new(
                    TypedExpression::Literal(TypedLiteral::String(*receiver_name)),
                    Type::Primitive(PrimitiveType::String),
                    expr.span,
                )],
                named_args: vec![],
                type_args: vec![],
            });
            expr.node = call;
            expr.ty = sig_ty.clone();
        }
    }

    fn rewrite_block(
        block: &mut zyntax_typed_ast::typed_ast::TypedBlock,
        signals: &HashMap<InternedString, Type>,
    ) {
        for stmt in &mut block.statements {
            rewrite_stmt(stmt, signals);
        }
    }

    fn rewrite_stmt(
        stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>,
        signals: &HashMap<InternedString, Type>,
    ) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, signals),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, signals);
                }
            }
            TypedStatement::Return(Some(e)) => rewrite_expr(e, signals),
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, signals);
                rewrite_block(&mut if_stmt.then_block, signals);
                if let Some(else_block) = &mut if_stmt.else_block {
                    rewrite_block(else_block, signals);
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition, signals);
                rewrite_block(&mut w.body, signals);
            }
            TypedStatement::Block(b) => rewrite_block(b, signals),
            // Other variants don't carry expressions we'd rewrite
            // for the prototype slice.
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        let TypedDeclaration::Function(func) = &mut decl.node else {
            continue;
        };
        if let Some(body) = &mut func.body {
            rewrite_block(body, &signals);
        }
    }
    // Also walk impl-block methods.
    for decl in &mut program.declarations {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };
        for method in &mut imp.methods {
            if let Some(body) = &mut method.body {
                rewrite_block(body, &signals);
            }
        }
    }

    // -----------------------------------------------------------------
    // Phase 3: strip the signal-marker decls. They were just
    // metadata-carriers; the rewrite path replaces every usage
    // with calls to host-registered builtins.
    // -----------------------------------------------------------------
    program.declarations.retain(|decl| {
        let TypedDeclaration::Function(func) = &decl.node else {
            return true;
        };
        !is_signal_decl(func)
    });
}

/// Wrap every fsm's `__fsm_meta__` body with `__fsm_begin__("FsmName")`
/// at the front and `__fsm_end__()` at the back. The host registers
/// these markers as builtins that push/pop a "current FSM" name on
/// a stack, so the `__fsm_initial__` / `__fsm_transition__` calls
/// inside the body know which fsm they're configuring.
///
/// Why a post-parse pass and not grammar action: same reason the
/// `<FSM>Event` synthesis is post-parse — the action language can't
/// string-concat or reach into the surrounding rule's bindings to
/// pull the FSM name into a child rule's emit. Building the wrapper
/// stmts in Rust is simpler.
///
/// The pass is idempotent in the sense that it only fires on
/// `__fsm_meta__` methods (synthesised by the `fsm` grammar rule);
/// programs without an fsm decl pass through untouched.
fn inject_fsm_context_markers(program: &mut TypedProgram) {
    use zyntax_typed_ast::typed_ast::{
        TypedCall, TypedDeclaration, TypedExpression, TypedLiteral, TypedStatement,
    };
    use zyntax_typed_ast::{InternedString, TypedNode};

    fn make_marker_call(callee: &str, str_args: &[&str]) -> TypedNode<TypedStatement> {
        let args: Vec<TypedNode<TypedExpression>> = str_args
            .iter()
            .map(|s| {
                TypedNode::new(
                    TypedExpression::Literal(TypedLiteral::String(InternedString::new_global(s))),
                    Type::Primitive(PrimitiveType::String),
                    Span::default(),
                )
            })
            .collect();

        let call = TypedExpression::Call(TypedCall {
            callee: Box::new(TypedNode::new(
                TypedExpression::Variable(InternedString::new_global(callee)),
                Type::Unknown,
                Span::default(),
            )),
            positional_args: args,
            named_args: vec![],
            type_args: vec![],
        });

        TypedNode::new(
            TypedStatement::Expression(Box::new(TypedNode::new(
                call,
                Type::Primitive(PrimitiveType::Unit),
                Span::default(),
            ))),
            Type::Primitive(PrimitiveType::Unit),
            Span::default(),
        )
    }

    for decl in &mut program.declarations {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };
        let Some(fsm_name) = imp.trait_name.resolve_global() else {
            continue;
        };

        for method in &mut imp.methods {
            if method.name.resolve_global().as_deref() != Some("__fsm_meta__") {
                continue;
            }
            let Some(body) = method.body.as_mut() else {
                continue;
            };

            // Skip if begin marker already present — defensive
            // against double-application if this pass is ever run
            // twice on the same program.
            let already_wrapped = body
                .statements
                .first()
                .map(|s| {
                    let TypedStatement::Expression(e) = &s.node else {
                        return false;
                    };
                    let TypedExpression::Call(c) = &e.node else {
                        return false;
                    };
                    let TypedExpression::Variable(callee) = &c.callee.node else {
                        return false;
                    };
                    callee.resolve_global().as_deref() == Some("__fsm_begin__")
                })
                .unwrap_or(false);
            if already_wrapped {
                continue;
            }

            let begin = make_marker_call("__fsm_begin__", &[&fsm_name]);
            let end = make_marker_call("__fsm_end__", &[]);
            body.statements.insert(0, begin);
            body.statements.push(end);
        }
    }
}

/// Walk a parsed `TypedProgram`, populate the global `FsmRegistry`
/// from each fsm's `__fsm_meta__` body, and strip the meta method so
/// Zyntax doesn't have to compile the (now-redundant) marker calls.
///
/// Three phases:
///
///   1. **Scan**: walk `Impl` decls looking for `__fsm_meta__`,
///      collect each fsm's name + parsed metadata (initial state,
///      event transitions, tick guards) into a buffer.
///
///   2. **Pin TypeIds**: for each fsm we found, mint a `TypeId` via
///      `TypeId::next()` and set the matching Enum decl's `ty` to
///      `Type::Named { id, ... }`. Zyntax's compile path
///      (`runtime.rs:1307-1368`) checks `decl_node.ty` first when
///      registering enum types — if it's `Type::Named`, the embedded
///      id wins; otherwise Zyntax mints its own. By pinning the id
///      here we guarantee the registry's `(module, TypeId)` key
///      matches whatever Zyntax sees later. Then we insert a
///      placeholder `TypeDefinition` ourselves so the
///      `get_type_by_name(...).is_none()` guard at runtime.rs:1308
///      short-circuits — Zyntax skips re-registering and our id is
///      authoritative.
///
///   3. **Strip**: remove `__fsm_meta__` from each fsm's Impl. The
///      marker callees (`__fsm_begin__`, `__fsm_initial__`, etc.)
///      have no extern decls in the program, so leaving the body in
///      place would type-fail. We've already extracted everything
///      the registry needs from those markers — the compiled
///      program doesn't need to call them.
///
/// Why direct AST walking instead of host-builtin marker invocation:
/// the chosen design (begin/end markers + eager population at
/// compile time) maps naturally onto walking the AST in Rust. We
/// already have the marker call shapes in `TypedExpression::Call`
/// form; running them through the JIT just to mutate a host-side
/// HashMap is a long detour for the same result.
fn populate_fsm_registry_pass(
    program: &mut TypedProgram,
    module: zyntax_typed_ast::InternedString,
) {
    use zyntax_typed_ast::type_registry::{
        TypeDefinition, TypeId, TypeKind, VariantDef, VariantFields, Visibility,
    };
    use zyntax_typed_ast::typed_ast::{
        TypedDeclaration, TypedExpression, TypedLiteral, TypedVariantFields,
    };
    use zyntax_typed_ast::InternedString;

    // -----------------------------------------------------------------
    // Phase 1: scan. Collect (fsm_name, FsmDefinition) tuples without
    // mutating program declarations. Tick-guard expressions are
    // captured here for lifting into stand-alone functions in
    // phase 2.5 (so they survive `__fsm_meta__` stripping).
    // -----------------------------------------------------------------
    let mut found: Vec<(InternedString, FsmDefinition)> = Vec::new();
    let mut guards_to_lift: Vec<(
        InternedString,
        zyntax_typed_ast::TypedNode<zyntax_typed_ast::TypedExpression>,
    )> = Vec::new();

    for decl in &program.declarations {
        let TypedDeclaration::Impl(imp) = &decl.node else {
            continue;
        };
        let Some(meta) = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
        else {
            continue;
        };
        let Some(body) = meta.body.as_ref() else {
            continue;
        };

        let fsm_name = imp.trait_name;
        let mut def = FsmDefinition {
            name: Some(fsm_name),
            ..Default::default()
        };

        for stmt_node in &body.statements {
            let TypedStatement::Expression(expr_node) = &stmt_node.node else {
                continue;
            };
            let TypedExpression::Call(call) = &expr_node.node else {
                continue;
            };
            let TypedExpression::Variable(callee_id) = &call.callee.node else {
                continue;
            };
            let callee = callee_id.resolve_global().unwrap_or_default();

            // Helper: pull a string-literal arg at index `idx`.
            let str_arg = |idx: usize| -> Option<InternedString> {
                call.positional_args.get(idx).and_then(|a| {
                    if let TypedExpression::Literal(TypedLiteral::String(s)) = &a.node {
                        Some(*s)
                    } else {
                        None
                    }
                })
            };

            match callee.as_str() {
                "__fsm_initial__" => {
                    if let Some(state) = str_arg(0) {
                        def.initial = Some(state);
                    }
                }
                "__fsm_transition__" => {
                    if let (Some(from), Some(event), Some(to)) =
                        (str_arg(0), str_arg(1), str_arg(2))
                    {
                        def.transitions.push(EventTransition { from, event, to });
                    }
                }
                "__fsm_tick__" => {
                    // arg 0 = from, arg 1 = guard expr, arg 2 = to.
                    // We lift the guard into a stand-alone function
                    // `__fsm_tick_guard_<FsmName>_<idx>__()` so it
                    // survives the `__fsm_meta__` strip and is
                    // callable as an ordinary Zyntax symbol at
                    // dispatch time.
                    if let (Some(from), Some(to)) = (str_arg(0), str_arg(2)) {
                        let idx = def.tick_guards.len();
                        let fsm_name_str = fsm_name.resolve_global().unwrap_or_default();
                        let guard_fn_name = format!("__fsm_tick_guard_{fsm_name_str}_{idx}__");
                        let guard_fn = InternedString::new_global(&guard_fn_name);

                        // Capture the guard expression (cloned to
                        // escape the read borrow on `program`) for
                        // lifting in the next phase.
                        if let Some(expr_node) = call.positional_args.get(1) {
                            guards_to_lift.push((guard_fn, expr_node.clone()));
                        }

                        def.tick_guards.push(TickGuard {
                            from,
                            to,
                            guard_fn: Some(guard_fn),
                        });
                    }
                }
                _ => {} // skip __fsm_begin__, __fsm_end__, anything else.
            }
        }

        found.push((fsm_name, def));
    }

    // -----------------------------------------------------------------
    // Phase 2: pin TypeIds + populate the registry. Pre-register the
    // type so Zyntax's compile path takes the "name already known"
    // short-circuit and respects our id.
    // -----------------------------------------------------------------
    for (fsm_name, def) in &found {
        let type_id = TypeId::next();

        // Pin `decl.ty` to Type::Named { id: our_id } on the matching
        // enum decl. Zyntax's enum-registration check at
        // runtime.rs:1313 reads exactly this field.
        let named_ty = program.type_registry.make_type(type_id, Vec::new());
        for decl in &mut program.declarations {
            let TypedDeclaration::Enum(enum_decl) = &decl.node else {
                continue;
            };
            if enum_decl.name == *fsm_name {
                decl.ty = named_ty.clone();
                break;
            }
        }

        // Pre-register the type so the get_type_by_name(...).is_none()
        // check at runtime.rs:1308 short-circuits and Zyntax doesn't
        // double-register with a fresh TypeId. We synthesise a
        // TypeDefinition mirroring what Zyntax would build for an
        // Enum declaration; downstream uses of TypeRegistry consume
        // this shape directly.
        if let Some(enum_decl) = program.declarations.iter().find_map(|d| match &d.node {
            TypedDeclaration::Enum(e) if e.name == *fsm_name => Some(e),
            _ => None,
        }) {
            let variants: Vec<VariantDef> = enum_decl
                .variants
                .iter()
                .enumerate()
                .map(|(i, v)| VariantDef {
                    name: v.name,
                    fields: match &v.fields {
                        TypedVariantFields::Unit => VariantFields::Unit,
                        TypedVariantFields::Tuple(types) => VariantFields::Tuple(types.clone()),
                        TypedVariantFields::Named(_) => VariantFields::Unit,
                    },
                    discriminant: Some(i as i64),
                    span: v.span,
                })
                .collect();

            let type_def = TypeDefinition {
                id: type_id,
                name: enum_decl.name,
                kind: TypeKind::Enum { variants },
                type_params: Vec::new(),
                constraints: Vec::new(),
                fields: Vec::new(),
                methods: Vec::new(),
                constructors: Vec::new(),
                metadata: Default::default(),
                span: enum_decl.span,
            };
            let _: TypeId = program.type_registry.register_type(type_def);
            let _ = Visibility::Public; // silence unused-import in case the type_registry-vis path changes upstream
        }

        let id = FsmId { module, type_id };
        with_fsm_registry_mut(|r| r.upsert(id, def.clone()));
    }

    // -----------------------------------------------------------------
    // Phase 2.5: lift each captured tick-guard expression into a
    // stand-alone top-level function. The function returns `i32`
    // (1 if the guard fires, 0 otherwise); the host's `step_tick`
    // tests `!= 0` to decide whether to transition. Body shape:
    //
    //     fn __fsm_tick_guard_<Fsm>_<idx>__() -> i32 {
    //         if <guard expr> { return 1 }
    //         return 0
    //     }
    //
    // Why i32 instead of bool: bool-return ABI marshaling through
    // `runtime.call::<bool>` is untested upstream
    // (`grep -rn 'call::<bool>'` hits zero across the Zyntax tree)
    // and triggers a misaligned-pointer panic in
    // `zyntax_compiler/zrtl.rs:416` during return-value
    // type-meta lookup. Using i32 with a 1/0 convention is a
    // tested ABI and keeps the lifting logic local to this pass.
    //
    // The lifted functions are appended after phase 2 so they
    // inherit any registered TypeIds in scope.
    // -----------------------------------------------------------------
    use zyntax_typed_ast::typed_ast::{TypedFunction, TypedIf};
    for (fn_name, guard_expr) in guards_to_lift {
        let i32_ty = Type::Primitive(PrimitiveType::I32);

        // `return 1` — the then-branch's only statement.
        let return_one = zyntax_typed_ast::TypedNode::new(
            TypedStatement::Return(Some(Box::new(zyntax_typed_ast::TypedNode::new(
                TypedExpression::Literal(zyntax_typed_ast::typed_ast::TypedLiteral::Integer(1)),
                i32_ty.clone(),
                Span::default(),
            )))),
            i32_ty.clone(),
            Span::default(),
        );

        let then_block = zyntax_typed_ast::typed_ast::TypedBlock {
            statements: vec![return_one],
            span: Span::default(),
        };

        // `if <guard> { return 1 }` — no else branch.
        let if_stmt = zyntax_typed_ast::TypedNode::new(
            TypedStatement::If(TypedIf {
                condition: Box::new(guard_expr),
                then_block,
                else_block: None,
                span: Span::default(),
            }),
            Type::Primitive(PrimitiveType::Unit),
            Span::default(),
        );

        // `return 0` — the body's trailing fallthrough.
        let return_zero = zyntax_typed_ast::TypedNode::new(
            TypedStatement::Return(Some(Box::new(zyntax_typed_ast::TypedNode::new(
                TypedExpression::Literal(zyntax_typed_ast::typed_ast::TypedLiteral::Integer(0)),
                i32_ty.clone(),
                Span::default(),
            )))),
            i32_ty.clone(),
            Span::default(),
        );

        let body = zyntax_typed_ast::typed_ast::TypedBlock {
            statements: vec![if_stmt, return_zero],
            span: Span::default(),
        };

        let func = TypedFunction {
            name: fn_name,
            return_type: i32_ty.clone(),
            body: Some(body),
            ..Default::default()
        };
        let decl_node = zyntax_typed_ast::TypedNode::new(
            TypedDeclaration::Function(func),
            Type::Unknown,
            Span::default(),
        );
        program.declarations.push(decl_node);
    }

    // -----------------------------------------------------------------
    // Phase 3: strip `__fsm_meta__` so the compile path doesn't have
    // to resolve the marker callees. The Impl may end up empty (the
    // fsm grammar emits __fsm_meta__ as the impl's only method) —
    // an empty inherent impl is benign.
    // -----------------------------------------------------------------
    for decl in &mut program.declarations {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };
        imp.methods
            .retain(|m| m.name.resolve_global().as_deref() != Some("__fsm_meta__"));
    }
}

/// Walk a parsed `TypedProgram` and synthesize a sibling `<FSM>Event`
/// enum for every fsm declaration that has at least one
/// `__fsm_transition__` marker. The synthesized enum's variants are
/// the unique event names referenced by the FSM's transitions, in
/// declaration order.
///
/// # Why a post-parse pass and not grammar action
///
/// Two reasons: (a) the grammar action language doesn't support
/// string concatenation, so we can't build the `<FSM>Event` name
/// from the FSM's own name at parse time; (b) deduplication of
/// event names across many transitions is naturally Rust code,
/// not grammar action code.
///
/// # Runtime contract
///
/// The synthesized enum is the bridge between user-facing event
/// names (`Start`, `Reset`) and `StateTransitions::on_event(event:
/// u32)`. A downstream codegen pass turns the enum into a Rust
/// `#[repr(u32)]` enum + `From<Event> for u32`, then user code
/// dispatches via `loader.dispatch(LoaderEvent::Start.into())`.
/// Tick transitions (`__fsm_tick__`) are guard-driven and don't
/// have user-facing event names, so they never appear in the
/// synthesized event enum.
fn synthesize_fsm_event_enums(program: &mut TypedProgram) {
    use std::collections::HashSet;
    use zyntax_typed_ast::type_registry::Visibility;
    use zyntax_typed_ast::typed_ast::{
        TypedDeclaration, TypedEnum, TypedExpression, TypedLiteral, TypedVariant,
        TypedVariantFields,
    };
    use zyntax_typed_ast::{InternedString, TypedNode};

    let mut event_enums: Vec<TypedNode<TypedDeclaration>> = Vec::new();

    for decl in &program.declarations {
        let TypedDeclaration::Impl(imp) = &decl.node else {
            continue;
        };

        // Find the synthesised `__fsm_meta__` method.
        let Some(meta) = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
        else {
            continue;
        };
        let Some(body) = meta.body.as_ref() else {
            continue;
        };

        // Collect unique event names from `__fsm_transition__(_, event,
        // _)` markers, preserving declaration order so the runtime
        // discriminant assignment is stable.
        let mut events: Vec<InternedString> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        for stmt_node in &body.statements {
            let TypedStatement::Expression(expr_node) = &stmt_node.node else {
                continue;
            };
            let TypedExpression::Call(call) = &expr_node.node else {
                continue;
            };
            let TypedExpression::Variable(callee) = &call.callee.node else {
                continue;
            };
            if callee.resolve_global().as_deref() != Some("__fsm_transition__") {
                continue;
            }
            let Some(event_arg) = call.positional_args.get(1) else {
                continue;
            };
            let TypedExpression::Literal(TypedLiteral::String(name)) = &event_arg.node else {
                continue;
            };
            let key = name.resolve_global().unwrap_or_default();
            if !key.is_empty() && seen.insert(key) {
                events.push(*name);
            }
        }

        if events.is_empty() {
            // Tick-only fsm or no transitions at all — nothing to
            // synthesise. The state enum + `__fsm_meta__` already
            // carry everything the runtime needs.
            continue;
        }

        // Build the variants and the `<FSM>Event` enum name. Use
        // `trait_name` (an InternedString) rather than `for_type`
        // (a Type) — the grammar sets both to the FSM identifier
        // for inherent impls, but `trait_name` gives us the bare
        // name without unwrapping a Type::Named.
        let fsm_name = imp.trait_name.resolve_global().unwrap_or_default();
        let event_enum_name = InternedString::new_global(&format!("{fsm_name}Event"));

        let variants: Vec<TypedVariant> = events
            .into_iter()
            .map(|name| TypedVariant {
                name,
                fields: TypedVariantFields::Unit,
                discriminant: None,
                span: Span::default(),
            })
            .collect();

        let event_enum = TypedDeclaration::Enum(TypedEnum {
            name: event_enum_name,
            type_params: vec![],
            variants,
            visibility: Visibility::Public,
            span: Span::default(),
        });

        event_enums.push(TypedNode::new(event_enum, Type::Unknown, Span::default()));
    }

    // Append synthesised enums after all original declarations so
    // existing `find_map` lookups (which return the first matching
    // decl) keep returning the user-declared state enum / impl.
    program.declarations.extend(event_enums);
}

// =====================================================================
// Runtime engine
// =====================================================================

/// The Blinc DSL runtime. Owns the compiled grammar + the Zyntax
/// runtime engine + the loaded module table.
///
/// Construction is `O(grammar parse)` — measured during the
/// risk-reduction prototype. If it climbs past 50 ms, the plan
/// (ROADMAP §3.9) is to switch to a pre-compiled `.zpeg` baked in via
/// `include_bytes!`.
pub struct BlincDsl {
    grammar: Grammar2,
    // We wrap `ZyntaxRuntime` in `Arc<Mutex<_>>` because the
    // production API (Phase 2 of ROADMAP §3.9) hands the same handle
    // to the hot-reload watcher thread for `runtime.hot_reload(...)`
    // calls. The runtime doesn't currently impl `Send + Sync`
    // upstream (its Cranelift `JITModule` is the blocker), which
    // makes clippy's `arc_with_non_send_sync` fire on construction —
    // we silence it where the `Arc::new` call lives, with the
    // expectation that upstream will eventually add the impls.
    runtime: Arc<Mutex<ZyntaxRuntime>>,
}

impl BlincDsl {
    /// Build a fresh runtime with the embedded Blinc grammar and all
    /// host builtins pre-registered.
    pub fn new() -> BlincDslResult<Self> {
        // Parse the grammar first so any embedded-grammar bug fails
        // fast before we touch the runtime.
        let grammar = Grammar2::from_source(BLINC_GRAMMAR)?;

        // Single-tier classic runtime for the prototype. The full
        // plan (§3.5) wraps this in a `RuntimeEngine` enum that picks
        // between `ZyntaxRuntime` and `TieredRuntime` based on
        // dev/release profile, mirroring the zynml shape.
        let mut runtime = ZyntaxRuntime::new()
            .map_err(|e| BlincDslError::Compile(format!("runtime init: {e}")))?;

        // Order matters: builtins must be registered before any
        // module load so the JIT linker can resolve `$Blinc$*`
        // symbols when `compile_typed_program` runs. Same rule zynml
        // calls out at zynml/src/lib.rs:199-200, except we do it via
        // `register_function` (statically linked symbols) rather than
        // `load_plugin` (.zrtl from disk). With Grammar2 there's no
        // separate `register_grammar` step — `compile_typed_program`
        // takes the typed AST directly.
        register_builtins(&mut runtime);

        // `register_function` only updates the backend's accumulator.
        // The Cranelift JIT module was constructed at `ZyntaxRuntime::new`
        // time and doesn't see new symbols until rebuild. Plugin loaders
        // do this implicitly; for static `register_function` callers we
        // poke the same hook explicitly. Without this, the first
        // `compile_typed_program` for any module that calls a builtin
        // panics inside Cranelift with "can't resolve symbol".
        runtime
            .finalize_runtime_symbols()
            .map_err(|e| BlincDslError::Compile(format!("finalize symbols: {e}")))?;

        // ZyntaxRuntime isn't Send+Sync today (see field-level
        // comment on `BlincDsl::runtime`). The Arc<Mutex<_>> wrapper
        // is the production-shape handle we'll need once it is.
        #[allow(clippy::arc_with_non_send_sync)]
        let runtime = Arc::new(Mutex::new(runtime));

        Ok(Self { grammar, runtime })
    }

    /// Compile a `.blinc` source file. Returns the names of compiled
    /// functions, mirroring the zynml `load_module_file` shape so we
    /// can rely on the same hot-reload pivot later (`runtime.hot_reload`
    /// keyed by function name).
    pub fn compile_source(&self, source: &str, filename: &str) -> BlincDslResult<Vec<String>> {
        let mut runtime = self
            .runtime
            .lock()
            .expect("BlincDsl runtime mutex poisoned");

        // Parse to typed AST. We don't pass plugin signatures
        // because we don't load `.zrtl` plugins; instead we splice
        // extern declarations for our static builtins directly into
        // the parsed program below, mirroring what
        // `Grammar2::inject_builtin_externs` does for plugin
        // symbols. Any parse-time / type / call-site / arity errors
        // here come back with the span machinery Zyntax already
        // provides — we don't add a parallel check layer
        // (ROADMAP §3.6).
        // `parse_with_signatures` runs Zyntax's
        // `inject_builtin_externs` (grammar2.rs:198-315) using the
        // runtime's registered plugin signatures. We populate those
        // signatures in `register_builtins` via
        // `register_function_typed`, so every `@builtin` entry in our
        // grammar gets a properly-typed extern decl in the parsed
        // program. This is the path that makes `concat` /
        // `__fstring_format__` (used by f-string desugaring) plus
        // our direct symbols (`$Blinc$text`, `$Blinc$text_int`,
        // etc.) all type-check + JIT-link cleanly.
        //
        // We don't run our own `inject_builtin_externs` host pass
        // anymore — Zyntax's covers everything in the @builtin
        // table, and our table now lists every builtin we register.
        let mut typed_program = self
            .grammar
            .parse_with_signatures(source, filename, runtime.plugin_signatures())
            .map_err(|e| BlincDslError::Compile(e.to_string()))?;

        // Apply the same post-parse passes parse_to_typed_ast runs,
        // so fsm-bearing programs get marker injection + event-enum
        // synthesis before compilation. Mirrors parse_to_typed_ast
        // — keep these two paths in sync when adding new passes.
        inject_fsm_context_markers(&mut typed_program);
        synthesize_fsm_event_enums(&mut typed_program);
        resolve_signal_calls(&mut typed_program);

        // Eager registry population: walk fsm impls, pin TypeIds,
        // record metadata into the global FsmRegistry, then strip
        // `__fsm_meta__` so the compile path doesn't have to handle
        // the marker callees. The module is hardcoded to "main"
        // since Zyntax compiles every source into a single module
        // today — when per-source modules surface upstream, this is
        // the place to thread the real module name through.
        let module = zyntax_typed_ast::InternedString::new_global("main");
        populate_fsm_registry_pass(&mut typed_program, module);

        // Belt-and-suspenders: terminate user functions with an
        // explicit `Return(None)` so the body classifier can't infer
        // a value-bearing return from a single trailing `Expression`
        // statement.
        ensure_unit_return(&mut typed_program);

        let function_names = runtime
            .compile_typed_program(typed_program)
            .map_err(|e| BlincDslError::Compile(e.to_string()))?;

        Ok(function_names)
    }

    /// Compile a `.blinc` file off disk.
    pub fn compile_file(&self, path: &Path) -> BlincDslResult<Vec<String>> {
        let source = std::fs::read_to_string(path)?;
        let filename = path.to_string_lossy();
        self.compile_source(&source, &filename)
    }

    /// Parse `.blinc` source to a TypedAST without compiling or
    /// running. Exposed for tests + tooling that want to analyse
    /// the parsed shape (e.g. an LSP, a CI lint, or — at this
    /// prototype stage — assertion tests on grammar rules that
    /// don't need full JIT round-trip).
    ///
    /// The grammar's job is to produce TypedAST; the compiler's
    /// job is to compile it. This entry point exists so we can
    /// test the former in isolation when the latter isn't the
    /// concern.
    pub fn parse_to_typed_ast(&self, source: &str, filename: &str) -> BlincDslResult<TypedProgram> {
        let runtime = self
            .runtime
            .lock()
            .expect("BlincDsl runtime mutex poisoned");

        let mut program = self
            .grammar
            .parse_with_signatures(source, filename, runtime.plugin_signatures())
            .map_err(|e| BlincDslError::Compile(e.to_string()))?;

        // Post-parse FSM passes — both run regardless of whether
        // the source contains an fsm; they no-op on programs that
        // don't have `__fsm_meta__` impls.
        //
        //   1. Inject `__fsm_begin__("FsmName")` / `__fsm_end__()`
        //      around each `__fsm_meta__` body so the host knows
        //      which FSM owns each marker call when the body is
        //      executed by Zyntax (the markers are stateful — they
        //      manipulate a host-side context stack).
        //   2. Synthesise a `<FSM>Event` enum from the unique event
        //      names referenced by `__fsm_transition__` markers.
        //   3. Resolve `signal <name>: <T>` decls into rewrites of
        //      `<name>.get()` to `__signal_get_<T>("<name>")` host
        //      extern calls. Strips the signal-marker decls so the
        //      compile path doesn't see them.
        //
        // The marker calls inside `__fsm_meta__` are then ordinary
        // function calls; Zyntax compiles them like any other call,
        // and the host registers `__fsm_begin__` / `__fsm_end__` /
        // `__fsm_initial__` / `__fsm_transition__` as builtins. No
        // codegen pass — Zyntax owns compilation.
        inject_fsm_context_markers(&mut program);
        synthesize_fsm_event_enums(&mut program);
        resolve_signal_calls(&mut program);

        Ok(program)
    }

    /// Invoke the bare-form `render_view` entry point and drain the
    /// scene buffer.
    ///
    /// For programs of the shape `view { ... }` (no enclosing
    /// `component` block). Component-form programs compile to
    /// `<Name>$render_view` instead — use [`Self::render_component`]
    /// for those.
    pub fn render_view(&self) -> BlincDslResult<Vec<DslOp>> {
        self.render_named("render_view")
    }

    /// Invoke a named component's view and drain the scene buffer.
    ///
    /// For programs of the shape `component <Name> { view { ... } }`
    /// — the grammar emits a function whose symbol IS the component
    /// name (so `component Greeting { ... }` produces a function
    /// named `Greeting`). This is symmetric: pass the component
    /// name, get the ops it emitted. Multi-component files work
    /// because each component gets its own distinct symbol.
    pub fn render_component(&self, name: &str) -> BlincDslResult<Vec<DslOp>> {
        self.render_named(name)
    }

    fn render_named(&self, fn_name: &str) -> BlincDslResult<Vec<DslOp>> {
        let runtime = self
            .runtime
            .lock()
            .expect("BlincDsl runtime mutex poisoned");

        runtime.call::<()>(fn_name, &[])?;
        Ok(take_scene_ops())
    }

    /// Resolve a tick-driven transition. Walks the registered fsm's
    /// `tick_guards` in declaration order, evaluates each whose
    /// `from` matches `current` by JIT-calling its lifted guard
    /// function (`__fsm_tick_guard_<Fsm>_<idx>__`), and returns the
    /// `to`-state of the first guard that fires (returns `true`).
    /// Returns `None` if nothing fires — either no rules match the
    /// current state, or every matching rule's guard returns `false`.
    ///
    /// Match-arm semantics: the first true guard wins. Authors get
    /// priority by source order, the same way `step_event` resolves
    /// event-driven transitions.
    ///
    /// Why this lives on `BlincDsl` and not the registry directly:
    /// dispatch needs both the registry (to find the guard fn name)
    /// and the runtime (to JIT-call it). `BlincDsl` is the natural
    /// owner of both. Plumbing a `&ZyntaxRuntime` through the
    /// registry's API would force every caller to thread it through
    /// — pointless when the existing `BlincDsl` handle already has
    /// what we need.
    pub fn step_tick(
        &self,
        id: &FsmId,
        current: &str,
    ) -> BlincDslResult<Option<zyntax_typed_ast::InternedString>> {
        // Phase 1: snapshot matching (guard_fn, to) pairs and drop
        // the registry lock before reaching for the runtime, so
        // there's no chance of holding both locks at once.
        let candidates: Vec<(
            zyntax_typed_ast::InternedString,
            zyntax_typed_ast::InternedString,
        )> = with_fsm_registry(|r| {
            r.get(id)
                .map(|def| {
                    def.tick_guards
                        .iter()
                        .filter(|g| g.from.resolve_global().as_deref() == Some(current))
                        .filter_map(|g| g.guard_fn.map(|fn_name| (fn_name, g.to)))
                        .collect()
                })
                .unwrap_or_default()
        });

        if candidates.is_empty() {
            return Ok(None);
        }

        // Phase 2: evaluate in declaration order against the JIT.
        let runtime = self
            .runtime
            .lock()
            .expect("BlincDsl runtime mutex poisoned");

        // Explicit signature: zero args, i32 return. Bypasses the
        // type-meta machinery in `runtime.call`, which doesn't have
        // a registered TypeMeta for user-compiled functions and
        // panics with a misaligned-pointer dereference at
        // `zrtl.rs:416`. `call_function` takes the signature
        // directly and uses Cranelift's known ABI for i32 returns.
        let guard_sig = NativeSignature::new(&[], NativeType::I32);

        for (guard_fn, to) in candidates {
            let Some(name) = guard_fn.resolve_global() else {
                continue;
            };
            let result = runtime
                .call_function(&name, &[], &guard_sig)
                .map_err(|e| BlincDslError::Compile(e.to_string()))?;
            // Lifted guards return 1 if the guard fires, 0 otherwise.
            let fired = matches!(result, ZyntaxValue::Int(v) if v != 0);
            if fired {
                return Ok(Some(to));
            }
        }

        Ok(None)
    }

    /// Set the current value of an i32-typed signal in the
    /// per-thread signal table. Subsequent JIT calls into the
    /// program — including tick-guard evaluations via `step_tick`
    /// and view-body reads — will see the new value.
    ///
    /// Embedders typically wire this to a Blinc `State<i32>` change
    /// callback so DSL guards stay in sync with the host's reactive
    /// state without needing to flow through Zyntax: when the host
    /// signal fires, push the new value into the DSL table here,
    /// then invalidate / re-tick whatever subscribed to it.
    ///
    /// The signal name must match the DSL `signal <name>: i32`
    /// declaration. There's no validation against the FsmRegistry
    /// — unknown names just sit in the table doing nothing, the
    /// JIT lookup at `<name>.get()` time finds them, and that's
    /// the contract. Embedders can verify shape via
    /// `parse_to_typed_ast` if they want to surface typos earlier.
    pub fn set_signal_i32(&self, name: &str, value: i32) {
        SIGNAL_TABLE_I32.with(|t| {
            t.borrow_mut().insert(name.to_string(), value);
        });
    }

    /// Read the current value of an i32-typed signal. Returns
    /// `None` when the signal hasn't been set in this thread —
    /// distinct from "set to 0", which returns `Some(0)`. Useful
    /// for diagnostics; production dispatch goes through
    /// `step_tick` / `step_event` which read the table at JIT
    /// time.
    pub fn get_signal_i32(&self, name: &str) -> Option<i32> {
        SIGNAL_TABLE_I32.with(|t| t.borrow().get(name).copied())
    }

    /// Set the current value of an f64-typed signal. Same shape
    /// as `set_signal_i32` but for `signal <name>: f64`
    /// declarations. Useful for floating-point guards — progress
    /// fractions, timing values, normalised positions.
    pub fn set_signal_f64(&self, name: &str, value: f64) {
        SIGNAL_TABLE_F64.with(|t| {
            t.borrow_mut().insert(name.to_string(), value);
        });
    }

    /// Read the current value of an f64-typed signal. `None`
    /// when unset; `Some(0.0)` when explicitly seeded to zero.
    pub fn get_signal_f64(&self, name: &str) -> Option<f64> {
        SIGNAL_TABLE_F64.with(|t| t.borrow().get(name).copied())
    }
}

/// Builtin descriptor — pairs a DSL-visible symbol name with the
/// `extern "C"` function pointer Cranelift will dispatch to and a
/// signature for the type checker.
///
/// Two roles in one struct:
///
/// - **Runtime registration**: `name` + `ptr` + `arg_count` are the
///   inputs to [`ZyntaxRuntime::register_function`], which makes the
///   symbol resolvable at JIT link time.
/// - **Type-system injection**: `param_types` + `return_type` are
///   used to mint a [`TypedDeclaration::Function`] with `is_external:
///   true` (via [`TypedASTBuilder::extern_function`]) that we splice
///   into every parsed `.blinc` `TypedProgram` before
///   `compile_typed_program`. Without that splice the type inferencer
///   sees `text(...)` as `Type::Any`, the body classifier rewrites
///   `render_view`'s declared `Unit` return to `I64`, and the call
///   site reads register-junk as a boxed value (misaligned-pointer
///   panic). The path `inject_builtin_externs` takes for `.zrtl`
///   plugins is the model — see
///   `zyntax/crates/zyntax_embed/src/grammar2.rs:198-315`.
struct BuiltinDescriptor {
    /// The mangled symbol the DSL emits as the call target. Grammar
    /// rules lower directly to this — no `@builtin` alias indirection
    /// (we drop the alias because we only have one consumer per
    /// symbol in the static-link world).
    name: &'static str,
    /// Argument types, in order. Drives the extern decl's parameter
    /// list and the type checker's call-site validation.
    param_types: &'static [Type],
    /// Return type. `Type::Primitive(PrimitiveType::Unit)` for
    /// builtins that perform a side-effect on the host scene buffer.
    return_type: Type,
    /// `extern "C"` function pointer — the Rust impl that gets
    /// invoked at runtime. Cast to `*const u8` for `register_function`.
    ptr: *const u8,
}

// SAFETY: `BuiltinDescriptor` only stores function pointers and
// `'static` references. Function pointers in Rust are `Send + Sync`;
// we mark this explicitly so the `[BuiltinDescriptor]` table can be
// iterated from any thread without complaints.
unsafe impl Sync for BuiltinDescriptor {}

/// The complete set of host builtins for the prototype slice. Ordering
/// is irrelevant; the registration loop walks all of them.
fn builtins() -> Vec<BuiltinDescriptor> {
    vec![
        BuiltinDescriptor {
            name: "$Blinc$text",
            param_types: &[Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_text as *const u8,
        },
        BuiltinDescriptor {
            name: "$Blinc$text_int",
            param_types: &[Type::Primitive(PrimitiveType::I32)],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_text_int as *const u8,
        },
        BuiltinDescriptor {
            // Bridges DSL `<name>.get()` (rewritten by
            // `resolve_signal_calls` to `__signal_get_i32("<name>")`)
            // to the per-thread `SIGNAL_TABLE_I32`. The DSL surface
            // for setting values lives on `BlincDsl::set_signal_i32`.
            name: "__signal_get_i32",
            param_types: &[Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::I32),
            ptr: blinc_signal_get_i32 as *const u8,
        },
        BuiltinDescriptor {
            // f64 mirror of `__signal_get_i32`. Same DSL surface
            // (`<name>.get()`) routed by `resolve_signal_calls` to
            // this extern when the signal's declared type is `f64`.
            name: "__signal_get_f64",
            param_types: &[Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::F64),
            ptr: blinc_signal_get_f64 as *const u8,
        },
    ]
}

/// Project a Blinc-side typed-AST `Type` onto the wire-format
/// `TypeTag` Zyntax expects in a `ZrtlSymbolSig`.
///
/// Only the primitive types we actually use in builtins are
/// represented today. Adding a new variant here is the small,
/// localised change required when a new builtin needs a new param /
/// return type (e.g. when we add `$Blinc$div(...) -> NodeHandle`,
/// we'll mint a `TypeTag` for opaque host handles).
fn type_to_tag(ty: &Type) -> TypeTag {
    match ty {
        Type::Primitive(PrimitiveType::Unit) => TypeTag::VOID,
        Type::Primitive(PrimitiveType::String) => TypeTag::STRING,
        Type::Primitive(PrimitiveType::I32) => TypeTag::I32,
        Type::Primitive(PrimitiveType::I64) => TypeTag::I64,
        Type::Primitive(PrimitiveType::F64) => TypeTag::F64,
        // Add more as the builtin surface grows. Falling through to
        // VOID rather than guessing would silently break codegen, so
        // panic loudly to surface the gap during prototype iteration.
        _ => panic!(
            "blinc_dsl_core: no TypeTag mapping for {ty:?} \
             — extend `type_to_tag` in src/lib.rs when adding new \
             builtin parameter / return types"
        ),
    }
}

/// Build the ZRTL signature for a builtin. The sig is what's stored
/// in `backend.symbol_signatures` and consulted at call-site lowering
/// (see `zyntax/crates/compiler/src/cranelift_backend.rs:2719`).
fn descriptor_to_sig(b: &BuiltinDescriptor) -> ZrtlSymbolSig {
    assert!(
        b.param_types.len() <= ZRTL_MAX_PARAMS,
        "{}: parameter count {} exceeds ZRTL_MAX_PARAMS ({})",
        b.name,
        b.param_types.len(),
        ZRTL_MAX_PARAMS
    );

    let mut params = [TypeTag::VOID; ZRTL_MAX_PARAMS];
    for (i, ty) in b.param_types.iter().enumerate() {
        params[i] = type_to_tag(ty);
    }

    ZrtlSymbolSig {
        param_count: b.param_types.len() as u8,
        flags: ZrtlSigFlags::NONE,
        return_type: type_to_tag(&b.return_type),
        params,
    }
}

/// Register all `$Blinc$*` builtins on the given runtime, with full
/// signatures so call-site lowering matches the typed AST extern
/// declarations injected by [`inject_builtin_externs`]. Without the
/// signature path, the call site would default-guess `I64` returns
/// and platform-default calling conventions and collide with the
/// extern decl.
///
/// Builtins are static `extern "C"` fns — no plugin discovery, no
/// `.zrtl` files (zynml/src/lib.rs:201-219 is the pattern we
/// explicitly do NOT copy).
fn register_builtins(runtime: &mut ZyntaxRuntime) {
    for b in builtins() {
        let sig = descriptor_to_sig(&b);
        runtime.register_function_typed(b.name, b.ptr, sig);
    }
}

/// Append a `Return(None)` to the main function so the body
/// classifier can't promote a single trailing Expression into a
/// value-bearing return.
///
/// `body_returns_value` (lowering.rs:1610-1633) treats a body with a
/// single `Expression` statement as value-returning, even when the
/// declaration is `return_type: Type::Unit`. With the extern decls
/// from [`inject_builtin_externs`] in place the call's type is `Unit`
/// rather than `Any`, which avoids most of the damage, but adding an
/// explicit terminator removes any remaining ambiguity and is cheap.
fn ensure_unit_return(program: &mut TypedProgram) {
    use zyntax_typed_ast::TypedDeclaration;

    for decl in program.declarations.iter_mut() {
        if let TypedDeclaration::Function(func) = &mut decl.node {
            if func.is_external {
                continue;
            }
            if let Some(body) = func.body.as_mut() {
                let trailing_is_return = matches!(
                    body.statements.last().map(|s| &s.node),
                    Some(TypedStatement::Return(_))
                );
                if !trailing_is_return {
                    body.statements.push(typed_node(
                        TypedStatement::Return(None),
                        Type::Primitive(PrimitiveType::Unit),
                        Span::default(),
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: try to compile a `.blinc` source. Returns the
    /// stringified error on failure so tests can assert on the
    /// diagnostic content. Tests don't share a `BlincDsl` instance
    /// because compiling a broken module pokes runtime state we
    /// don't want to leak into a follow-up assertion.
    fn try_compile(source: &str, filename: &str) -> Result<Vec<String>, String> {
        let _ = tracing_subscriber::fmt::try_init();
        let dsl = BlincDsl::new().map_err(|e| e.to_string())?;
        dsl.compile_source(source, filename)
            .map_err(|e| e.to_string())
    }

    /// Smoke test for the prototype slice — round-trips a tiny
    /// `view { text("...") }` program through the full pipeline and
    /// checks the host saw the right scene op. If this fails, the
    /// integration is broken end-to-end and the rest of the plan is
    /// blocked on fixing it before phase-2 grammar expansion.
    #[test]
    fn round_trip_text_view() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(r#"view { text("Hello, Blinc DSL!") }"#, "smoke.blinc")
            .expect("compile");
        let ops = dsl.render_view().expect("render_view");

        assert_eq!(ops.len(), 1, "expected 1 op, got {ops:?}");
        match &ops[0] {
            DslOp::Text(s) => assert_eq!(s, "Hello, Blinc DSL!"),
            other => panic!("expected DslOp::Text, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Component (Class + Impl) parsing tests.
    //
    // Components lower to two TypedDeclarations: a Class for the
    // data shape, and an Impl for the methods. Same idiom as ml.zyn
    // structs + inherent impls.
    // -----------------------------------------------------------------

    /// `component Counter { count: i32, width: i32 }` parses to a
    /// `TypedDeclaration::Class` with two fields.
    #[test]
    fn parse_component_struct_only() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"component Counter { count: i32, width: i32 }"#,
                "struct_only.blinc",
            )
            .expect("parse");

        let class = program
            .declarations
            .iter()
            .find_map(|d| {
                if let zyntax_typed_ast::TypedDeclaration::Class(c) = &d.node {
                    Some(c)
                } else {
                    None
                }
            })
            .expect("expected at least one Class decl");

        assert_eq!(class.name.resolve_global().as_deref(), Some("Counter"));
        assert_eq!(class.fields.len(), 2, "expected 2 fields");
        assert_eq!(
            class.fields[0].name.resolve_global().as_deref(),
            Some("count")
        );
        assert_eq!(
            class.fields[1].name.resolve_global().as_deref(),
            Some("width")
        );
    }

    /// `impl Counter { fn view() { text("hi") } }` parses to a
    /// `TypedDeclaration::Impl`. The interpreter's Impl-walk
    /// (interpreter.rs:1017-1080) unwraps each function into a
    /// `TypedMethod` automatically.
    #[test]
    fn parse_impl_with_view() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(r#"impl Counter { fn view() { text("hi") } }"#, "impl.blinc")
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| {
                if let zyntax_typed_ast::TypedDeclaration::Impl(i) = &d.node {
                    Some(i)
                } else {
                    None
                }
            })
            .expect("expected an Impl decl");

        assert_eq!(
            impl_block.trait_name.resolve_global().as_deref(),
            Some("Counter")
        );
        assert_eq!(impl_block.methods.len(), 1, "expected 1 method (view)");
        assert_eq!(
            impl_block.methods[0].name.resolve_global().as_deref(),
            Some("view")
        );
    }

    // -----------------------------------------------------------------
    // Reactivity tests — `state` keyword wraps the field type in
    // `Type::Named { State, [T] }`. Plain fields stay as bare `T`.
    // The host walks Class.fields, treats State<...>-typed fields
    // as reactive, plain fields as data-only.
    // -----------------------------------------------------------------

    /// `state count: i32` lowers to a `TypedField` whose `ty` is
    /// `Type::Named { name: "State", type_args: [i32] }`. The
    /// State<...> wrapping is the AST-level reactivity marker.
    #[test]
    fn parse_state_field_wraps_type() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"component Counter { state count: i32 }"#,
                "state_field.blinc",
            )
            .expect("parse");

        let class = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Class(c) => Some(c),
                _ => None,
            })
            .expect("expected a Class");

        assert_eq!(class.fields.len(), 1);
        let count_field = &class.fields[0];
        assert_eq!(count_field.name.resolve_global().as_deref(), Some("count"));

        // The `state` keyword wraps the type in `Type::Named { State, [i32] }`.
        match &count_field.ty {
            zyntax_typed_ast::Type::Named { id, type_args, .. } => {
                let name_str = dsl.runtime.lock().ok().map(|_| ());
                // Type::Named's `id` references the program's type
                // registry; resolve it back to a name to verify
                // it's `State`. For now we accept any
                // Named-with-1-arg as state.
                let _ = name_str;
                assert_eq!(
                    type_args.len(),
                    1,
                    "expected one type arg (the inner type), got {type_args:?}"
                );
                // Inner arg should be the i32 primitive.
                match &type_args[0] {
                    zyntax_typed_ast::Type::Primitive(prim) => {
                        assert!(
                            matches!(prim, zyntax_typed_ast::PrimitiveType::I32),
                            "expected i32 inner, got {prim:?}"
                        );
                    }
                    other => panic!("expected primitive inner, got {other:?}"),
                }
                let _ = id;
            }
            other => panic!("state field should wrap type in Type::Named, got {other:?}"),
        }
    }

    /// Mixed reactive + plain fields with mixed types:
    /// `state count: i32, name: string`. State field wrapped in
    /// `Type::Named { State, [i32] }`; plain field stays as the
    /// bare primitive.
    ///
    /// Earlier this test mis-attributed a parse failure to a PEG
    /// ambiguity in `struct_field`'s alternates. The actual cause
    /// was that the grammar was emitting `Type::Primitive { name:
    /// intern("string") }` while Zyntax's
    /// `primitive_type_from_name` (interpreter.rs:2017) recognises
    /// only `str` / `String` for the string primitive (matching
    /// ml.zyn's `prim_str` at ml.zyn:1444). Construct-type fell
    /// through to "unknown primitive type" and the rule failed —
    /// looked like an alternate-ordering issue from the outside,
    /// but was a tag-name mismatch. Fixed by emitting
    /// `intern("str")` internally while keeping `string` as the
    /// user-facing DSL keyword.
    #[test]
    fn parse_mixed_state_and_plain_fields() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"component Profile { state count: i32, name: string }"#,
                "mixed_fields.blinc",
            )
            .expect("parse");

        let class = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Class(c) => Some(c),
                _ => None,
            })
            .expect("expected a Class");

        assert_eq!(class.fields.len(), 2);

        // Field 0: state count → wrapped Named (State<i32>).
        let count_field = &class.fields[0];
        assert_eq!(count_field.name.resolve_global().as_deref(), Some("count"));
        assert!(
            matches!(&count_field.ty, zyntax_typed_ast::Type::Named { .. }),
            "state field should be Type::Named (State<...>), got {:?}",
            count_field.ty
        );

        // Field 1: plain name → bare Primitive(String).
        let name_field = &class.fields[1];
        assert_eq!(name_field.name.resolve_global().as_deref(), Some("name"));
        assert!(
            matches!(
                &name_field.ty,
                zyntax_typed_ast::Type::Primitive(zyntax_typed_ast::PrimitiveType::String)
            ),
            "plain field should be Primitive(String), got {:?}",
            name_field.ty
        );
    }

    /// state+state in the same field list also parses cleanly.
    /// Pinning this so the false-positive "ambiguity" claim
    /// doesn't get reintroduced by anyone reading the earlier
    /// commit message.
    #[test]
    fn parse_two_state_fields_same_list() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"component Counter { state count: i32, state width: i32 }"#,
                "two_states.blinc",
            )
            .expect("parse");

        let class = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Class(c) => Some(c),
                _ => None,
            })
            .expect("expected a Class");

        assert_eq!(class.fields.len(), 2);
        for f in &class.fields {
            assert!(
                matches!(&f.ty, zyntax_typed_ast::Type::Named { .. }),
                "every field is `state`, so each ty should be Type::Named, got {:?}",
                f.ty
            );
        }
    }

    /// Optional split form: `component Name { fields }` + `impl Name
    /// { fn ... }` as separate top-level items. Supported alongside
    /// the folded form for users who want explicit decl-per-rule
    /// shape (or are programmatically generating `.blinc` source
    /// where one decl at a time is easier to emit).
    #[test]
    fn parse_component_split_form() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Counter { count: i32 }
                impl Counter {
                    fn view() { text("count") }
                }
                "#,
                "counter_split.blinc",
            )
            .expect("parse");

        let mut class_count = 0;
        let mut impl_count = 0;
        for decl in &program.declarations {
            match &decl.node {
                zyntax_typed_ast::TypedDeclaration::Class(_) => class_count += 1,
                zyntax_typed_ast::TypedDeclaration::Impl(_) => impl_count += 1,
                _ => {}
            }
        }
        assert_eq!(class_count, 1, "expected 1 Class decl");
        assert_eq!(impl_count, 1, "expected 1 Impl decl");
    }

    /// Folded `component { fields, view { ... }, fn handler() { ... } }`
    /// emits BOTH a Class and an Impl from one source-level block.
    /// Validates the inlined `concat_list([Class], [Impl])` shape
    /// in the grammar action — relies on the upstream
    /// `get_field_as_decl_list` flatten patch (#7) so the parent
    /// `top_level_items` collector unwraps the nested list.
    #[test]
    fn parse_component_folded() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                // `view { ... }` does the rendering. Handlers like
                // `on_click` mutate state / call other functions /
                // run general logic — they don't render. The body
                // is empty here because state-mutation syntax
                // (e.g. `count = count - 1`) isn't in the grammar
                // yet (next phase-2 slice). The test still
                // validates that the handler is recognised as a
                // method on the component's Impl.
                r#"
                component Counter {
                    count: i32
                    view { text("count") }
                    fn on_click() {}
                }
                "#,
                "counter_folded.blinc",
            )
            .expect("parse");

        // One source `component { ... }` block -> two TypedDeclarations.
        let class = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Class(c) => Some(c),
                _ => None,
            })
            .expect("expected a Class decl from the folded component");
        assert_eq!(class.name.resolve_global().as_deref(), Some("Counter"));
        assert_eq!(
            class.fields.len(),
            1,
            "expected one field (count) in folded component"
        );

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("expected an Impl decl from the folded component");
        assert_eq!(
            impl_block.trait_name.resolve_global().as_deref(),
            Some("Counter")
        );
        assert_eq!(
            impl_block.methods.len(),
            2,
            "expected view + on_click methods, got {:?}",
            impl_block
                .methods
                .iter()
                .map(|m| m.name.resolve_global())
                .collect::<Vec<_>>()
        );

        // view is first (prepended), on_click is second.
        assert_eq!(
            impl_block.methods[0].name.resolve_global().as_deref(),
            Some("view")
        );
        assert_eq!(
            impl_block.methods[1].name.resolve_global().as_deref(),
            Some("on_click")
        );
    }

    /// `text(N)` round-trip — probes the i32 ABI through Cranelift.
    /// Confirms (a) the integer terminal in the grammar lowers to a
    /// real `IntLiteral`, (b) PEG backtracks from the string variant
    /// of `text(...)` and matches the int variant, (c) Zyntax
    /// type-checks the call against `$Blinc$text_int`'s `(i32) ->
    /// ()` signature, (d) Cranelift passes the value as an actual
    /// i32 register, (e) the host receives it without ABI corruption.
    #[test]
    fn round_trip_text_int() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(r#"view { text(42) }"#, "int_smoke.blinc")
            .expect("compile");
        let ops = dsl.render_view().expect("render_view");

        assert_eq!(ops.len(), 1, "expected 1 op, got {ops:?}");
        match &ops[0] {
            DslOp::IntText(n) => assert_eq!(*n, 42),
            other => panic!("expected DslOp::IntText, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // F-string parsing tests — TypedAST shape only.
    //
    // At this prototype stage the grammar's job is to produce the
    // right TypedAST and we verify that. JIT round-trip + runtime
    // concat / format dispatch is the compiler's concern (Zyntax owns
    // the SSA backend; what shape its f-string handling takes for a
    // non-`println` caller is something we'll address when the
    // compiler integration matures, not here).
    // -----------------------------------------------------------------

    use zyntax_typed_ast::typed_ast::{TypedExpression, TypedLiteral};
    use zyntax_typed_ast::TypedDeclaration;

    /// Pull the body statements out of the parsed program's first
    /// non-extern function. Test-only helper.
    fn first_user_function_body(
        program: &TypedProgram,
    ) -> &[zyntax_typed_ast::TypedNode<TypedStatement>] {
        for decl in program.declarations.iter() {
            if let TypedDeclaration::Function(func) = &decl.node {
                if !func.is_external {
                    return func
                        .body
                        .as_ref()
                        .map(|b| b.statements.as_slice())
                        .unwrap_or(&[]);
                }
            }
        }
        panic!("no user function found in program")
    }

    /// `text(f"hello")` — single-part f-string with no
    /// interpolation. Zyntax's `fold_concat` short-circuits the
    /// single-part case (interpreter.rs:2365-2370) and returns the
    /// bare expression, so the parsed AST should look identical to
    /// `text("hello")`.
    #[test]
    fn parse_text_fstring_single_part_text() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(r#"view { text(f"hello") }"#, "fstr_single.blinc")
            .expect("parse");

        let stmts = first_user_function_body(&program);
        assert_eq!(stmts.len(), 1, "expected 1 stmt in body, got {stmts:?}");
        let TypedStatement::Expression(call_node) = &stmts[0].node else {
            panic!("expected Expression statement");
        };
        let TypedExpression::Call(call) = &call_node.node else {
            panic!("expected Call");
        };
        // text("hello") -> the only positional arg is a string literal.
        assert_eq!(call.positional_args.len(), 1);
        let TypedExpression::Literal(TypedLiteral::String(_)) = &call.positional_args[0].node
        else {
            panic!(
                "expected single string-literal arg, got {:?}",
                call.positional_args[0].node
            );
        };
    }

    /// `text(f"{42}")` — single interp part. fold_concat's
    /// short-circuit returns the bare `__fstring_format__(42)` call
    /// (interpreter.rs:2365-2370 + the wrapper from
    /// `f_string_interp`). We assert the AST has that one call as
    /// the arg of `text`.
    #[test]
    fn parse_text_fstring_single_part_interp() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(r#"view { text(f"{42}") }"#, "fstr_interp.blinc")
            .expect("parse");

        let stmts = first_user_function_body(&program);
        let TypedStatement::Expression(call_node) = &stmts[0].node else {
            panic!("expected Expression statement");
        };
        let TypedExpression::Call(text_call) = &call_node.node else {
            panic!("expected Call");
        };
        // text(__fstring_format__(42))
        assert_eq!(text_call.positional_args.len(), 1);
        let TypedExpression::Call(fmt_call) = &text_call.positional_args[0].node else {
            panic!(
                "expected nested __fstring_format__ call, got {:?}",
                text_call.positional_args[0].node
            );
        };
        let TypedExpression::Variable(name) = &fmt_call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            name.resolve_global().as_deref(),
            Some("__fstring_format__"),
            "expected __fstring_format__ wrapping the int arg"
        );
    }

    /// `text(f"answer: {42}!")` — multi-part f-string. fold_concat
    /// builds `__fstring__(text_lit, fmt_call_stripped, text_lit)`
    /// (interpreter.rs:2372-2410). We assert the AST has that
    /// shape: one `__fstring__` call with three positional args.
    #[test]
    fn parse_text_fstring_multi_part() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(r#"view { text(f"answer: {42}!") }"#, "fstr_multi.blinc")
            .expect("parse");

        let stmts = first_user_function_body(&program);
        let TypedStatement::Expression(call_node) = &stmts[0].node else {
            panic!("expected Expression statement");
        };
        let TypedExpression::Call(text_call) = &call_node.node else {
            panic!("expected Call");
        };
        assert_eq!(text_call.positional_args.len(), 1);
        let TypedExpression::Call(fstring_call) = &text_call.positional_args[0].node else {
            panic!(
                "expected nested __fstring__ call, got {:?}",
                text_call.positional_args[0].node
            );
        };
        let TypedExpression::Variable(name) = &fstring_call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            name.resolve_global().as_deref(),
            Some("__fstring__"),
            "expected fold_concat-emitted __fstring__ marker"
        );
        assert_eq!(
            fstring_call.positional_args.len(),
            3,
            "expected three parts (text, int, text), got {:?}",
            fstring_call.positional_args
        );
    }

    // -----------------------------------------------------------------
    // Expression-layer parsing tests — variable refs, binary
    // arithmetic, assignment statements. Phase-2 minimal slice.
    //
    // Same TypedAST-only verification approach as the f-string tests
    // — we assert the grammar produces the right shape and let the
    // compiler handle codegen.
    // -----------------------------------------------------------------

    /// `text(f"{count}")` — interpolating a bare variable reference.
    /// fold_concat short-circuits the single-part case and returns the
    /// `__fstring_format__(count)` call directly. We assert the call
    /// arg is a `TypedExpression::Variable`, proving variable refs
    /// flow through `f_string_expr → expr → primary_expr`.
    #[test]
    fn parse_fstring_variable_ref() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(r#"view { text(f"{count}") }"#, "fstr_var.blinc")
            .expect("parse");

        let stmts = first_user_function_body(&program);
        let TypedStatement::Expression(call_node) = &stmts[0].node else {
            panic!("expected Expression statement");
        };
        let TypedExpression::Call(text_call) = &call_node.node else {
            panic!("expected Call");
        };
        let TypedExpression::Call(fmt_call) = &text_call.positional_args[0].node else {
            panic!("expected nested __fstring_format__ call");
        };
        let TypedExpression::Variable(name) = &fmt_call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(name.resolve_global().as_deref(), Some("__fstring_format__"));
        // The arg of __fstring_format__ should now be a Variable("count")
        // rather than an integer literal.
        assert_eq!(fmt_call.positional_args.len(), 1);
        let TypedExpression::Variable(arg_name) = &fmt_call.positional_args[0].node else {
            panic!(
                "expected Variable arg, got {:?}",
                fmt_call.positional_args[0].node
            );
        };
        assert_eq!(arg_name.resolve_global().as_deref(), Some("count"));
    }

    /// `count = count + 1` inside a method body. Lowers to a
    /// `TypedStatement::Expression` wrapping `Binary(Variable, Assign,
    /// Binary(Variable, Add, IntLiteral))`. The Class's reactivity
    /// marker (`State<i32>`) is irrelevant here — at parse time
    /// assignment is just an Assign Binary, regardless of whether the
    /// target is reactive. The compiler / a later pass decides whether
    /// to lower to `count.set(count.get() + 1)`.
    #[test]
    fn parse_assignment_state_mutation() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Counter {
                    state count: i32
                    view { text(f"{count}") }
                    fn on_click() { count = count + 1 }
                }
                "#,
                "counter_assign.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("expected an Impl decl");

        // Find the on_click method.
        let on_click = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("on_click"))
            .expect("expected on_click method");

        let body = on_click.body.as_ref().expect("on_click should have a body");
        assert_eq!(
            body.statements.len(),
            1,
            "expected one assignment stmt, got {:?}",
            body.statements
        );

        // Statement: Expression(Binary(Variable("count"), Assign, ...)).
        let TypedStatement::Expression(expr_node) = &body.statements[0].node else {
            panic!("expected Expression stmt");
        };
        let TypedExpression::Binary(outer) = &expr_node.node else {
            panic!("expected outer Binary, got {:?}", expr_node.node);
        };
        assert!(
            matches!(outer.op, zyntax_typed_ast::BinaryOp::Assign),
            "outer op should be Assign, got {:?}",
            outer.op
        );

        // LHS: Variable("count").
        let TypedExpression::Variable(target) = &outer.left.node else {
            panic!("expected Variable target, got {:?}", outer.left.node);
        };
        assert_eq!(target.resolve_global().as_deref(), Some("count"));

        // RHS: Binary(Variable("count"), Add, IntLiteral(1)).
        let TypedExpression::Binary(rhs) = &outer.right.node else {
            panic!("expected RHS Binary, got {:?}", outer.right.node);
        };
        assert!(
            matches!(rhs.op, zyntax_typed_ast::BinaryOp::Add),
            "RHS op should be Add, got {:?}",
            rhs.op
        );
        let TypedExpression::Variable(lhs_var) = &rhs.left.node else {
            panic!("expected Variable on RHS LHS");
        };
        assert_eq!(lhs_var.resolve_global().as_deref(), Some("count"));
        let TypedExpression::Literal(TypedLiteral::Integer(n)) = &rhs.right.node else {
            panic!("expected IntLiteral on RHS RHS, got {:?}", rhs.right.node);
        };
        assert_eq!(*n, 1);
    }

    /// Multiplicative binds tighter than additive: `1 + 2 * 3` should
    /// parse as `Binary(1, Add, Binary(2, Mul, 3))`, not
    /// `Binary(Binary(1, Add, 2), Mul, 3)`. Pinning this so a future
    /// well-meaning grammar refactor doesn't accidentally flatten the
    /// precedence ladder.
    #[test]
    fn parse_arithmetic_precedence() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state x: i32
                    view {}
                    fn step() { x = 1 + 2 * 3 }
                }
                "#,
                "precedence.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("expected Impl");

        let step = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("step"))
            .expect("expected step method");
        let body = step.body.as_ref().expect("body");
        let TypedStatement::Expression(node) = &body.statements[0].node else {
            panic!("expected Expression stmt");
        };
        let TypedExpression::Binary(assign) = &node.node else {
            panic!("expected Binary");
        };
        // RHS is Add at the top with Mul nested on the right.
        let TypedExpression::Binary(add) = &assign.right.node else {
            panic!("RHS should be Binary(Add)");
        };
        assert!(
            matches!(add.op, zyntax_typed_ast::BinaryOp::Add),
            "top RHS should be Add, got {:?}",
            add.op
        );
        let TypedExpression::Literal(TypedLiteral::Integer(left_n)) = &add.left.node else {
            panic!("Add LHS should be IntLiteral, got {:?}", add.left.node);
        };
        assert_eq!(*left_n, 1);
        let TypedExpression::Binary(mul) = &add.right.node else {
            panic!("Add RHS should be Binary(Mul), got {:?}", add.right.node);
        };
        assert!(
            matches!(mul.op, zyntax_typed_ast::BinaryOp::Mul),
            "nested op should be Mul, got {:?}",
            mul.op
        );
    }

    /// Parens override precedence: `(1 + 2) * 3` should parse as
    /// `Binary(Binary(1, Add, 2), Mul, 3)`. Confirms `paren_expr`
    /// passes its inner expression straight through (no wrapper node)
    /// and feeds the multiplicative chain.
    #[test]
    fn parse_paren_grouping() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state x: i32
                    view {}
                    fn step() { x = (1 + 2) * 3 }
                }
                "#,
                "parens.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("step"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::Expression(node) = &body.statements[0].node else {
            panic!("expected Expression stmt");
        };
        let TypedExpression::Binary(assign) = &node.node else {
            panic!("expected assign");
        };
        // RHS top-level should be Mul; the LHS of Mul is the
        // (1 + 2) Add subtree.
        let TypedExpression::Binary(mul) = &assign.right.node else {
            panic!("RHS should be Binary, got {:?}", assign.right.node);
        };
        assert!(
            matches!(mul.op, zyntax_typed_ast::BinaryOp::Mul),
            "top RHS should be Mul, got {:?}",
            mul.op
        );
        let TypedExpression::Binary(add) = &mul.left.node else {
            panic!("Mul LHS should be Add subtree, got {:?}", mul.left.node);
        };
        assert!(matches!(add.op, zyntax_typed_ast::BinaryOp::Add));
    }

    /// `let derived = count + 1` — immutable local binding.
    /// Lowers to `TypedStatement::Let` with `mutability ==
    /// Immutable`, `ty == Type::Any` (no annotation, compiler
    /// infers), and an initializer Binary expression.
    ///
    /// Why we test this end-to-end: the grammar action reads
    /// `is_mutable: false` / `type_annotation: None`, but the
    /// interpreter's "Let" construction
    /// (runtime2/interpreter.rs:381-403) rewrites those into the
    /// real TypedLet shape. Easy to break by changing field names.
    #[test]
    fn parse_let_binding() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view {}
                    fn step() {
                        let derived = count + 1
                    }
                }
                "#,
                "let_binding.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("expected Impl");
        let body = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("step"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();

        assert_eq!(body.statements.len(), 1, "expected one let stmt");
        let TypedStatement::Let(let_node) = &body.statements[0].node else {
            panic!("expected Let, got {:?}", body.statements[0].node);
        };
        assert_eq!(let_node.name.resolve_global().as_deref(), Some("derived"));
        assert!(
            matches!(let_node.mutability, zyntax_typed_ast::Mutability::Immutable),
            "phase-2 let is immutable, got {:?}",
            let_node.mutability
        );
        let init = let_node
            .initializer
            .as_ref()
            .expect("let must have initializer");
        let TypedExpression::Binary(add) = &init.node else {
            panic!("expected Binary initializer, got {:?}", init.node);
        };
        assert!(matches!(add.op, zyntax_typed_ast::BinaryOp::Add));
    }

    /// `if count > 0 { text("positive") } else { text("zero") }` —
    /// conditional rendering. Asserts (a) `>` parses as a
    /// comparison Binary at the condition, (b) both branches
    /// produce TypedBlocks with the right number of statements,
    /// (c) the else_block is populated.
    #[test]
    fn parse_if_else_with_comparison() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view {
                        if count > 0 {
                            text("positive")
                        } else {
                            text("zero")
                        }
                    }
                }
                "#,
                "if_else.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let view = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();

        assert_eq!(view.statements.len(), 1);
        let TypedStatement::If(if_stmt) = &view.statements[0].node else {
            panic!("expected If, got {:?}", view.statements[0].node);
        };

        // Condition is `count > 0` — Binary with BinaryOp::Gt.
        let TypedExpression::Binary(cond) = &if_stmt.condition.node else {
            panic!("expected Binary condition");
        };
        assert!(
            matches!(cond.op, zyntax_typed_ast::BinaryOp::Gt),
            "expected Gt, got {:?}",
            cond.op
        );

        // then-branch has one text("positive") stmt.
        assert_eq!(if_stmt.then_block.statements.len(), 1);
        // else-branch present, also one stmt.
        let else_block = if_stmt.else_block.as_ref().expect("expected else branch");
        assert_eq!(else_block.statements.len(), 1);
    }

    /// Field separators are optional. Authors should be able to
    /// write fields one-per-line, comma-separated, or mixed —
    /// without the parser caring. Pinning all three shapes
    /// because the easiest way to regress this is to "tighten"
    /// `struct_field_tail` back to a required comma during a
    /// future refactor.
    #[test]
    fn parse_field_separators_optional() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        for (label, src) in [
            (
                "newline",
                "component A { state count: i32 state width: i32 view {} }",
            ),
            (
                "comma",
                "component A { state count: i32, state width: i32 view {} }",
            ),
            (
                "mixed",
                "component A { state count: i32, state width: i32\nstate height: i32 view {} }",
            ),
        ] {
            let program = dsl
                .parse_to_typed_ast(src, &format!("sep_{label}.blinc"))
                .unwrap_or_else(|e| panic!("parse failure for {label}: {e:?}"));
            let class = program
                .declarations
                .iter()
                .find_map(|d| match &d.node {
                    zyntax_typed_ast::TypedDeclaration::Class(c) => Some(c),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no Class for {label}"));
            let expected = if label == "mixed" { 3 } else { 2 };
            assert_eq!(
                class.fields.len(),
                expected,
                "{label}: expected {expected} fields"
            );
        }
    }

    /// `a && b` lowers to `Binary(_, And, _)`. Single AND case;
    /// the next test covers precedence with OR.
    #[test]
    fn parse_logical_and() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state x: i32
                    view {
                        if x > 0 && x < 100 { text("in range") }
                    }
                }
                "#,
                "logical_and.blinc",
            )
            .expect("parse");

        let imp = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::If(if_stmt) = &body.statements[0].node else {
            panic!("expected If");
        };
        let TypedExpression::Binary(top) = &if_stmt.condition.node else {
            panic!("expected Binary top condition");
        };
        assert!(
            matches!(top.op, zyntax_typed_ast::BinaryOp::And),
            "top op should be And, got {:?}",
            top.op
        );
        let TypedExpression::Binary(lhs) = &top.left.node else {
            panic!("LHS of And should be a comparison");
        };
        assert!(matches!(lhs.op, zyntax_typed_ast::BinaryOp::Gt));
        let TypedExpression::Binary(rhs) = &top.right.node else {
            panic!("RHS of And should be a comparison");
        };
        assert!(matches!(rhs.op, zyntax_typed_ast::BinaryOp::Lt));
    }

    /// `a || b && c` parses with AND binding tighter than OR:
    /// top op is Or, RHS is `Binary(b, And, c)`. Pinning the
    /// precedence ladder so a future grammar refactor can't flip
    /// `||` and `&&`.
    #[test]
    fn parse_logical_or_and_precedence() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state x: i32
                    view {
                        if x < 0 || x > 100 && x < 200 { text("either") }
                    }
                }
                "#,
                "logical_precedence.blinc",
            )
            .expect("parse");

        let imp = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::If(if_stmt) = &body.statements[0].node else {
            panic!("expected If");
        };

        let TypedExpression::Binary(top) = &if_stmt.condition.node else {
            panic!("expected Binary at top");
        };
        assert!(
            matches!(top.op, zyntax_typed_ast::BinaryOp::Or),
            "top op should be Or, got {:?}",
            top.op
        );
        let TypedExpression::Binary(rhs_and) = &top.right.node else {
            panic!("RHS of Or should be Binary(And), got {:?}", top.right.node);
        };
        assert!(
            matches!(rhs_and.op, zyntax_typed_ast::BinaryOp::And),
            "RHS top op should be And, got {:?}",
            rhs_and.op
        );
    }

    /// Method-call expressions parse via the postfix layer.
    /// `count.get()` lowers to TypedExpression::MethodCall with
    /// receiver=Variable("count"), method="get", no args. This is
    /// the shape state-field reads will take once the deps-list
    /// view + ViewCtx::get(N) → State<T> chain is wired.
    #[test]
    fn parse_method_call_no_args() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view {}
                    fn step() { let v = count.get() }
                }
                "#,
                "method_call.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("step"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::Let(let_node) = &body.statements[0].node else {
            panic!("expected Let");
        };
        let init = let_node.initializer.as_ref().unwrap();
        let TypedExpression::MethodCall(call) = &init.node else {
            panic!("expected MethodCall, got {:?}", init.node);
        };
        assert_eq!(call.method.resolve_global().as_deref(), Some("get"));
        assert_eq!(call.positional_args.len(), 0);
        let TypedExpression::Variable(receiver) = &call.receiver.node else {
            panic!("expected Variable receiver");
        };
        assert_eq!(receiver.resolve_global().as_deref(), Some("count"));
    }

    /// `ctx.get(0)` — method call with one positional arg.
    /// Confirms `call_args_list` parses comma-separated args and
    /// the integer literal flows through.
    #[test]
    fn parse_method_call_with_arg() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view {}
                    fn step() { let s = ctx.get(0) }
                }
                "#,
                "method_call_arg.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("step"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::Let(let_node) = &body.statements[0].node else {
            panic!("expected Let");
        };
        let init = let_node.initializer.as_ref().unwrap();
        let TypedExpression::MethodCall(call) = &init.node else {
            panic!("expected MethodCall, got {:?}", init.node);
        };
        assert_eq!(call.method.resolve_global().as_deref(), Some("get"));
        assert_eq!(call.positional_args.len(), 1);
        let TypedExpression::Literal(TypedLiteral::Integer(n)) = &call.positional_args[0].node
        else {
            panic!("expected IntLiteral arg");
        };
        assert_eq!(*n, 0);
    }

    /// Method calls compose with comparisons: `count.get() > 0`
    /// parses as Binary(MethodCall, Gt, IntLiteral). Pinning the
    /// precedence — postfix should bind tighter than binary
    /// operators, so the method-call subtree is on the LHS of the
    /// comparison rather than the comparison being inside the
    /// method's args.
    #[test]
    fn parse_method_call_in_condition() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view { if count.get() > 0 { text("pos") } }
                }
                "#,
                "mcall_in_cond.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let view = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::If(if_stmt) = &view.statements[0].node else {
            panic!("expected If");
        };
        let TypedExpression::Binary(cmp) = &if_stmt.condition.node else {
            panic!("expected Binary condition");
        };
        assert!(matches!(cmp.op, zyntax_typed_ast::BinaryOp::Gt));
        // LHS is the method call.
        let TypedExpression::MethodCall(_) = &cmp.left.node else {
            panic!("expected MethodCall on LHS, got {:?}", cmp.left.node);
        };
    }

    /// `view([state1]) {|ctx| stmts}` — explicit-deps closure form.
    /// Lowers to a function `view(ctx)` whose body starts with a
    /// synthesised `__view_deps__(state1)` marker call, followed
    /// by the user's stmts. Asserts (a) the function has one
    /// parameter named "ctx", (b) the first statement is the
    /// marker call carrying the right deps, (c) user stmts come
    /// after the marker.
    #[test]
    fn parse_view_with_deps() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Counter {
                    state count: i32
                    state width: i32
                    view([count, width]) {|ctx|
                        let c = ctx.get(0)
                        text("rendered")
                    }
                }
                "#,
                "view_deps.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("expected Impl");
        let view = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .expect("expected view method");

        // (a) one parameter named "ctx".
        assert_eq!(
            view.params.len(),
            1,
            "expected one param, got {:?}",
            view.params
        );
        assert_eq!(view.params[0].name.resolve_global().as_deref(), Some("ctx"));

        // (b) first body stmt is `__view_deps__(count, width)`.
        let body = view.body.as_ref().expect("view body");
        assert!(
            body.statements.len() >= 2,
            "expected >=2 stmts (marker + user code), got {}",
            body.statements.len()
        );
        let TypedStatement::Expression(marker_node) = &body.statements[0].node else {
            panic!("expected marker stmt to be Expression");
        };
        let TypedExpression::Call(marker) = &marker_node.node else {
            panic!("expected marker to be Call, got {:?}", marker_node.node);
        };
        let TypedExpression::Variable(callee_name) = &marker.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            callee_name.resolve_global().as_deref(),
            Some("__view_deps__"),
            "marker callee should be __view_deps__"
        );
        assert_eq!(
            marker.positional_args.len(),
            2,
            "expected two deps, got {:?}",
            marker.positional_args
        );

        // (c) marker args are Variable refs with the right names.
        for (i, expected) in ["count", "width"].iter().enumerate() {
            let TypedExpression::Variable(name) = &marker.positional_args[i].node else {
                panic!("expected Variable arg at {}", i);
            };
            assert_eq!(name.resolve_global().as_deref(), Some(*expected));
        }

        // (d) user statements follow the marker. body[1] should be
        // the `let c = ctx.get(0)` stmt.
        let TypedStatement::Let(_) = &body.statements[1].node else {
            panic!(
                "expected user `let` stmt after marker, got {:?}",
                body.statements[1].node
            );
        };
    }

    /// Plain `view { stmts }` still works alongside the deps form
    /// — `view_member` is `view_with_deps | view_simple`. The
    /// simple form should produce a parameterless function with
    /// no `__view_deps__` marker.
    #[test]
    fn parse_view_simple_still_works() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Counter {
                    state count: i32
                    view { text("hi") }
                }
                "#,
                "view_simple.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let view = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap();
        // No params on the simple form.
        assert_eq!(view.params.len(), 0);
        // No marker — first stmt is the user's text("hi"), not a
        // __view_deps__ call.
        let body = view.body.as_ref().unwrap();
        let TypedStatement::Expression(first) = &body.statements[0].node else {
            panic!("expected Expression stmt");
        };
        let TypedExpression::Call(call) = &first.node else {
            panic!("expected Call");
        };
        let TypedExpression::Variable(callee) = &call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_ne!(
            callee.resolve_global().as_deref(),
            Some("__view_deps__"),
            "simple view shouldn't carry the deps marker"
        );
    }

    /// `if a { ... } else if b { ... } else { ... }` lowers to a
    /// recursive nested-If shape: the outer If's else_block holds a
    /// single-statement block whose only statement is another If
    /// (with its own else_block holding the final block). Pinning
    /// the shape so a future grammar refactor doesn't break the
    /// chain wrapping convention.
    #[test]
    fn parse_else_if_chain() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view {
                        if count > 100 { text("big") }
                        else if count > 10 { text("medium") }
                        else { text("small") }
                    }
                }
                "#,
                "else_if_chain.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let view = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        assert_eq!(
            view.statements.len(),
            1,
            "view body holds the single outer If"
        );

        // Outer If: condition `count > 100`, then `text("big")`,
        // else_block is a single-stmt block wrapping the next If.
        let TypedStatement::If(outer) = &view.statements[0].node else {
            panic!("expected outer If");
        };
        let outer_else = outer.else_block.as_ref().expect("outer else");
        assert_eq!(
            outer_else.statements.len(),
            1,
            "else block should hold one statement (the chained If)"
        );

        // Chained If: condition `count > 10`, then `text("medium")`,
        // else_block is a one-stmt block with `text("small")`.
        let TypedStatement::If(chained) = &outer_else.statements[0].node else {
            panic!("expected chained If as the only stmt in outer else");
        };
        let TypedExpression::Binary(cmp) = &chained.condition.node else {
            panic!("expected chained condition to be Binary");
        };
        assert!(matches!(cmp.op, zyntax_typed_ast::BinaryOp::Gt));
        let TypedExpression::Literal(TypedLiteral::Integer(n)) = &cmp.right.node else {
            panic!("expected IntLit on RHS of chained condition");
        };
        assert_eq!(*n, 10);

        // Tail else.
        let tail_else = chained.else_block.as_ref().expect("chained else (tail)");
        assert_eq!(
            tail_else.statements.len(),
            1,
            "tail else holds text(\"small\")"
        );
    }

    /// 4-arm chain `if / else if / else if / else if / else` walks
    /// to nested depth 4. Pinning that PEG recursion in `if_stmt`
    /// doesn't bottom out at any chain length.
    #[test]
    fn parse_else_if_chain_deep() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view {
                        if count > 1000 { text("a") }
                        else if count > 100 { text("b") }
                        else if count > 10 { text("c") }
                        else if count > 1 { text("d") }
                        else { text("e") }
                    }
                }
                "#,
                "else_if_deep.blinc",
            )
            .expect("parse");

        let imp = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let view_body = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();

        // Walk down the chain — each level should hold a single
        // `If` in its else_block, with a final tail `else`.
        let mut depth = 0;
        let mut current = match &view_body.statements[0].node {
            TypedStatement::If(i) => i,
            _ => panic!("top of view body should be If"),
        };
        loop {
            depth += 1;
            let else_block = current
                .else_block
                .as_ref()
                .unwrap_or_else(|| panic!("level {depth}: expected an else"));
            // If the only stmt in else is another If, descend.
            if else_block.statements.len() == 1 {
                if let TypedStatement::If(next) = &else_block.statements[0].node {
                    current = next;
                    continue;
                }
            }
            break;
        }
        assert_eq!(depth, 4, "expected 4 chained Ifs before the tail else");
    }

    /// `else if` without trailing else: `if A { } else if B { }`.
    /// The chained inner If has `else_block: None`. Pinning the
    /// no-tail-else case so the recursion handles it correctly.
    #[test]
    fn parse_else_if_no_trailing_else() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view {
                        if count > 100 { text("a") }
                        else if count > 10 { text("b") }
                    }
                }
                "#,
                "else_if_no_tail.blinc",
            )
            .expect("parse");

        let imp = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();

        let TypedStatement::If(outer) = &body.statements[0].node else {
            panic!("expected outer If");
        };
        let outer_else = outer.else_block.as_ref().expect("outer else");
        let TypedStatement::If(chained) = &outer_else.statements[0].node else {
            panic!("expected chained If");
        };
        assert!(
            chained.else_block.is_none(),
            "chained If should have no else when source omits it"
        );
    }

    /// `if count > 0 { ... }` with no else — `else_block` is None.
    /// Pinning the simple form so a future "always emit empty
    /// else" refactor doesn't sneak in.
    #[test]
    fn parse_if_no_else() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view { if count > 0 { text("yes") } }
                }
                "#,
                "if_no_else.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let view = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::If(if_stmt) = &view.statements[0].node else {
            panic!("expected If");
        };
        assert!(
            if_stmt.else_block.is_none(),
            "no-else form should leave else_block None"
        );
    }

    // -----------------------------------------------------------------
    // FSM declaration tests — `fsm Name { state X, initial Y, on
    // X.Event -> Z }`. Verify the grammar emits the right two-decl
    // shape (Enum + Impl) and the metadata marker calls inside
    // `__fsm_meta__`.
    // -----------------------------------------------------------------

    /// `fsm Loader { ... }` emits BOTH a TypedDeclaration::Enum
    /// (states as variants) and a TypedDeclaration::Impl (carrying
    /// the metadata via `__fsm_meta__`). Pinning that the
    /// `concat_list` lowering shape stays right — easy to break by
    /// switching list helpers during a refactor.
    #[test]
    fn parse_fsm_emits_enum_and_impl() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Loader {
                    state Idle
                    state Loading
                    state Done
                    initial Idle
                    on Idle.Load -> Loading
                    on Loading.Finish -> Done
                }
                "#,
                "fsm_loader.blinc",
            )
            .expect("parse");

        let enum_decl = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Enum(e) => Some(e),
                _ => None,
            })
            .expect("expected Enum decl from fsm");
        assert_eq!(enum_decl.name.resolve_global().as_deref(), Some("Loader"));
        assert_eq!(
            enum_decl.variants.len(),
            3,
            "expected 3 variants (Idle, Loading, Done), got {:?}",
            enum_decl
                .variants
                .iter()
                .map(|v| v.name.resolve_global())
                .collect::<Vec<_>>()
        );
        // Variant names match.
        for (i, expected) in ["Idle", "Loading", "Done"].iter().enumerate() {
            assert_eq!(
                enum_decl.variants[i].name.resolve_global().as_deref(),
                Some(*expected),
                "variant {i}"
            );
        }

        // Inherent Impl with a single `__fsm_meta__` method.
        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("expected Impl decl from fsm");
        assert_eq!(
            impl_block.trait_name.resolve_global().as_deref(),
            Some("Loader")
        );
        assert_eq!(impl_block.methods.len(), 1, "expected one method");
        assert_eq!(
            impl_block.methods[0].name.resolve_global().as_deref(),
            Some("__fsm_meta__")
        );
    }

    /// `__fsm_meta__` body layout after both post-parse passes:
    ///
    ///     [0] __fsm_begin__("FsmName")
    ///     [1] __fsm_initial__("InitialState")
    ///     [2..n-1] __fsm_transition__ / __fsm_tick__ markers
    ///     [n-1] __fsm_end__()
    ///
    /// Begin/end wrap the body so the host knows which fsm owns
    /// the markers in between. This test pins (a) the begin
    /// marker is at body[0] with the right FSM name, (b) the
    /// initial marker is at body[1] with the right state name,
    /// (c) the end marker is at the last index.
    #[test]
    fn parse_fsm_initial_marker() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Toggle {
                    state On
                    state Off
                    initial Off
                    on Off.Click -> On
                    on On.Click -> Off
                }
                "#,
                "fsm_toggle.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let meta = &impl_block.methods[0];
        let body = meta.body.as_ref().expect("__fsm_meta__ body");

        // Helper to extract (callee, args) at a given index.
        let extract = |idx: usize| -> (String, Vec<String>) {
            let TypedStatement::Expression(node) = &body.statements[idx].node else {
                panic!("expected Expression stmt at [{idx}]");
            };
            let TypedExpression::Call(call) = &node.node else {
                panic!("expected Call at [{idx}]");
            };
            let TypedExpression::Variable(callee) = &call.callee.node else {
                panic!("expected Variable callee at [{idx}]");
            };
            let str_args = call
                .positional_args
                .iter()
                .filter_map(|a| {
                    if let TypedExpression::Literal(TypedLiteral::String(s)) = &a.node {
                        s.resolve_global()
                    } else {
                        None
                    }
                })
                .collect();
            (callee.resolve_global().unwrap_or_default(), str_args)
        };

        let (begin_callee, begin_args) = extract(0);
        assert_eq!(begin_callee, "__fsm_begin__");
        assert_eq!(begin_args, vec!["Toggle".to_string()]);

        let (initial_callee, initial_args) = extract(1);
        assert_eq!(initial_callee, "__fsm_initial__");
        assert_eq!(initial_args, vec!["Off".to_string()]);

        let last = body.statements.len() - 1;
        let (end_callee, end_args) = extract(last);
        assert_eq!(end_callee, "__fsm_end__");
        assert!(end_args.is_empty(), "__fsm_end__ takes no args");
    }

    /// Each `on State.Event -> Next` lowers to one
    /// `__fsm_transition__("State", "Event", "Next")` marker call.
    /// Verifies all three string args carry the right names and the
    /// markers appear after the initial-state marker (preserving
    /// declaration order so the runtime can interpret them
    /// sequentially).
    #[test]
    fn parse_fsm_transitions() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Loader {
                    state Idle
                    state Loading
                    state Done
                    initial Idle
                    on Idle.Load -> Loading
                    on Loading.Finish -> Done
                    on Done.Reset -> Idle
                }
                "#,
                "fsm_three_transitions.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block.methods[0].body.as_ref().unwrap();

        // begin + initial + 3 transitions + end = 6 stmts.
        assert_eq!(
            body.statements.len(),
            6,
            "expected begin + initial + 3 transitions + end, got {}",
            body.statements.len()
        );

        let expected_transitions = [
            ("Idle", "Load", "Loading"),
            ("Loading", "Finish", "Done"),
            ("Done", "Reset", "Idle"),
        ];

        for (i, (from, event, to)) in expected_transitions.iter().enumerate() {
            // Body layout: [0]=begin, [1]=initial, [2..]=transitions,
            // [last]=end. Transitions start at body[2].
            let stmt = &body.statements[i + 2].node;
            let TypedStatement::Expression(node) = stmt else {
                panic!("expected Expression stmt at index {}", i + 1);
            };
            let TypedExpression::Call(call) = &node.node else {
                panic!("expected Call at index {}", i + 1);
            };
            let TypedExpression::Variable(callee) = &call.callee.node else {
                panic!("expected Variable callee");
            };
            assert_eq!(
                callee.resolve_global().as_deref(),
                Some("__fsm_transition__"),
                "marker at {} should be __fsm_transition__",
                i + 1
            );
            assert_eq!(call.positional_args.len(), 3);
            for (j, expected) in [from, event, to].iter().enumerate() {
                let TypedExpression::Literal(TypedLiteral::String(s)) =
                    &call.positional_args[j].node
                else {
                    panic!(
                        "expected String arg at transition {} arg {}, got {:?}",
                        i, j, call.positional_args[j].node
                    );
                };
                assert_eq!(
                    s.resolve_global().as_deref(),
                    Some(**expected),
                    "transition {} arg {}: expected {}, got differently",
                    i,
                    j,
                    expected
                );
            }
        }
    }

    /// `tick From -> To when <expr>` — data-guarded transition.
    /// Lowers to a `__fsm_tick__("From", <guard expr>, "To")`
    /// marker call. The middle arg is the raw expression (NOT a
    /// string literal) because the runtime/compiler reads it to
    /// lower into the body of the `StateTransitions::on_tick`
    /// impl. Pinning all three arg shapes: from is a string lit,
    /// guard is a Binary (the expression survives parsing intact),
    /// to is a string lit.
    #[test]
    fn parse_fsm_tick_transition() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Loader {
                    state Loading
                    state Done
                    initial Loading
                    tick Loading -> Done when progress.get() > 100
                }
                "#,
                "fsm_tick.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block.methods[0].body.as_ref().unwrap();

        // begin + initial + tick + end = 4 stmts. Tick is at body[2].
        assert_eq!(body.statements.len(), 4);
        let TypedStatement::Expression(node) = &body.statements[2].node else {
            panic!("expected Expression stmt at body[2]");
        };
        let TypedExpression::Call(call) = &node.node else {
            panic!("expected Call");
        };
        let TypedExpression::Variable(callee) = &call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            callee.resolve_global().as_deref(),
            Some("__fsm_tick__"),
            "tick marker callee"
        );
        assert_eq!(call.positional_args.len(), 3, "expected (from, guard, to)");

        // arg 0: from = "Loading" string literal.
        let TypedExpression::Literal(TypedLiteral::String(from)) = &call.positional_args[0].node
        else {
            panic!("expected string literal arg 0");
        };
        assert_eq!(from.resolve_global().as_deref(), Some("Loading"));

        // arg 1: guard = Binary(MethodCall, Gt, IntLiteral(100)).
        let TypedExpression::Binary(bin) = &call.positional_args[1].node else {
            panic!(
                "expected Binary guard expression, got {:?}",
                call.positional_args[1].node
            );
        };
        assert!(
            matches!(bin.op, zyntax_typed_ast::BinaryOp::Gt),
            "guard top op should be Gt"
        );
        let TypedExpression::MethodCall(mc) = &bin.left.node else {
            panic!("guard LHS should be MethodCall");
        };
        assert_eq!(mc.method.resolve_global().as_deref(), Some("get"));
        let TypedExpression::Literal(TypedLiteral::Integer(n)) = &bin.right.node else {
            panic!("guard RHS should be IntLiteral");
        };
        assert_eq!(*n, 100);

        // arg 2: to = "Done" string literal.
        let TypedExpression::Literal(TypedLiteral::String(to)) = &call.positional_args[2].node
        else {
            panic!("expected string literal arg 2");
        };
        assert_eq!(to.resolve_global().as_deref(), Some("Done"));
    }

    /// Mixed event and tick transitions in the same fsm — both
    /// shapes coexist inside `__fsm_meta__`. Pinning declaration
    /// order survives so the runtime can interpret transitions
    /// sequentially (event-driven and data-guarded share priority,
    /// resolved by the order users wrote them).
    #[test]
    fn parse_fsm_mixed_event_and_tick() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Loader {
                    state Idle
                    state Loading
                    state Done
                    initial Idle
                    on Idle.Start -> Loading
                    tick Loading -> Done when progress.get() > 100
                    on Done.Reset -> Idle
                }
                "#,
                "fsm_mixed.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block.methods[0].body.as_ref().unwrap();

        // begin + initial + 3 transitions + end = 6.
        assert_eq!(body.statements.len(), 6);

        // Helper to read a marker callee name from a stmt.
        let callee_at = |idx: usize| -> String {
            let TypedStatement::Expression(node) = &body.statements[idx].node else {
                panic!("expected Expression at {idx}");
            };
            let TypedExpression::Call(call) = &node.node else {
                panic!("expected Call at {idx}");
            };
            let TypedExpression::Variable(callee) = &call.callee.node else {
                panic!("expected Variable callee at {idx}");
            };
            callee.resolve_global().unwrap_or_default()
        };

        assert_eq!(callee_at(0), "__fsm_begin__");
        assert_eq!(callee_at(1), "__fsm_initial__");
        assert_eq!(callee_at(2), "__fsm_transition__");
        assert_eq!(callee_at(3), "__fsm_tick__");
        assert_eq!(callee_at(4), "__fsm_transition__");
        assert_eq!(callee_at(5), "__fsm_end__");
    }

    /// `__fsm_meta__` body is wrapped with `__fsm_begin__("FsmName")`
    /// at the front and `__fsm_end__()` at the back so the host's
    /// stateful marker runtime knows which fsm scopes the markers
    /// in between. Pins the wrapping behaviour against future
    /// refactors that might split the `inject_fsm_context_markers`
    /// pass.
    #[test]
    fn parse_fsm_begin_end_wrapping() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Loader {
                    state Idle
                    state Loading
                    initial Idle
                    on Idle.Start -> Loading
                }
                "#,
                "fsm_begin_end.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block.methods[0].body.as_ref().unwrap();

        // Body[0] = __fsm_begin__("Loader")
        let TypedStatement::Expression(begin_node) = &body.statements[0].node else {
            panic!("expected Expression at body[0]");
        };
        let TypedExpression::Call(begin_call) = &begin_node.node else {
            panic!("expected Call at body[0]");
        };
        let TypedExpression::Variable(begin_callee) = &begin_call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            begin_callee.resolve_global().as_deref(),
            Some("__fsm_begin__")
        );
        let TypedExpression::Literal(TypedLiteral::String(name)) =
            &begin_call.positional_args[0].node
        else {
            panic!("expected string arg to __fsm_begin__");
        };
        assert_eq!(
            name.resolve_global().as_deref(),
            Some("Loader"),
            "__fsm_begin__ should carry the fsm's own name"
        );

        // Last stmt = __fsm_end__()
        let last = body.statements.len() - 1;
        let TypedStatement::Expression(end_node) = &body.statements[last].node else {
            panic!("expected Expression at last");
        };
        let TypedExpression::Call(end_call) = &end_node.node else {
            panic!("expected Call");
        };
        let TypedExpression::Variable(end_callee) = &end_call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(end_callee.resolve_global().as_deref(), Some("__fsm_end__"));
        assert!(
            end_call.positional_args.is_empty(),
            "__fsm_end__ takes no args"
        );
    }

    /// Post-parse synthesis: an fsm with event-driven transitions
    /// gets a sibling `<FSM>Event` enum appended to the program's
    /// declarations. Variants are the unique event names from the
    /// FSM's `__fsm_transition__` markers, in declaration order.
    /// Pinning that the bridge between user-facing event names
    /// (`Start`, `Reset`) and `StateTransitions::on_event(u32)` is
    /// emitted at parse time, ready for codegen.
    #[test]
    fn synthesize_event_enum_basic() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Loader {
                    state Idle
                    state Loading
                    state Done
                    initial Idle
                    on Idle.Start -> Loading
                    on Loading.Finish -> Done
                    on Done.Reset -> Idle
                }
                "#,
                "fsm_event_enum.blinc",
            )
            .expect("parse");

        // Two enums in the program: the state enum (Loader) and
        // the synthesised event enum (LoaderEvent).
        let enums: Vec<_> = program
            .declarations
            .iter()
            .filter_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Enum(e) => Some(e),
                _ => None,
            })
            .collect();
        assert_eq!(
            enums.len(),
            2,
            "expected state enum + event enum, got {}",
            enums.len()
        );

        let state_enum = enums[0];
        assert_eq!(state_enum.name.resolve_global().as_deref(), Some("Loader"));

        let event_enum = enums[1];
        assert_eq!(
            event_enum.name.resolve_global().as_deref(),
            Some("LoaderEvent"),
            "synthesised enum should be named <FSM>Event"
        );
        assert_eq!(
            event_enum.variants.len(),
            3,
            "expected 3 unique events (Start, Finish, Reset)"
        );
        for (i, expected) in ["Start", "Finish", "Reset"].iter().enumerate() {
            assert_eq!(
                event_enum.variants[i].name.resolve_global().as_deref(),
                Some(*expected),
                "variant {i} should be {expected} (declaration order preserved)"
            );
        }
    }

    /// Duplicate event names across transitions (e.g. `Click`
    /// reused on multiple from-states) get deduped — the event
    /// enum has at most one variant per unique name. Order is the
    /// first-seen position.
    #[test]
    fn synthesize_event_enum_dedup() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Toggle {
                    state On
                    state Off
                    initial Off
                    on Off.Click -> On
                    on On.Click -> Off
                }
                "#,
                "fsm_event_dedup.blinc",
            )
            .expect("parse");

        let event_enum = program
            .declarations
            .iter()
            .filter_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Enum(e) => Some(e),
                _ => None,
            })
            .find(|e| e.name.resolve_global().as_deref() == Some("ToggleEvent"))
            .expect("expected ToggleEvent enum");

        assert_eq!(
            event_enum.variants.len(),
            1,
            "duplicate `Click` events should dedup to one variant"
        );
        assert_eq!(
            event_enum.variants[0].name.resolve_global().as_deref(),
            Some("Click")
        );
    }

    /// Tick-only fsm has no `__fsm_transition__` markers and so
    /// gets no event enum synthesised. The state enum + impl are
    /// the only fsm-related decls. Confirms the pass doesn't emit
    /// an empty stub when there are no events.
    #[test]
    fn synthesize_no_event_enum_for_tick_only_fsm() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Progress {
                    state Loading
                    state Done
                    initial Loading
                    tick Loading -> Done when count.get() > 100
                }
                "#,
                "fsm_tick_only.blinc",
            )
            .expect("parse");

        let enums: Vec<_> = program
            .declarations
            .iter()
            .filter_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Enum(e) => Some(e),
                _ => None,
            })
            .collect();
        assert_eq!(
            enums.len(),
            1,
            "tick-only fsm should have only the state enum, got {} enums",
            enums.len()
        );
        assert_eq!(enums[0].name.resolve_global().as_deref(), Some("Progress"));
    }

    /// `fsm` with no transitions still parses — useful for the
    /// degenerate "states without yet-defined behaviour" stub. The
    /// body should have exactly the initial marker, no transitions.
    #[test]
    fn parse_fsm_no_transitions() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                "fsm Status { state Open state Closed initial Open }",
                "fsm_stub.blinc",
            )
            .expect("parse");

        let enum_decl = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Enum(e) => Some(e),
                _ => None,
            })
            .unwrap();
        assert_eq!(enum_decl.variants.len(), 2);

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block.methods[0].body.as_ref().unwrap();
        // begin + initial + end = 3.
        assert_eq!(
            body.statements.len(),
            3,
            "stub fsm body should be begin + initial + end"
        );
    }

    // -----------------------------------------------------------------
    // FsmRegistry data-structure tests. These verify the registry's
    // public API in isolation. The end-to-end "compile a fsm
    // source, see registry populate" path lands in the next commit
    // when the host marker builtins are wired up — for now we
    // exercise upsert/get/remove/iter directly so the API is pinned
    // before integration arrives.
    // -----------------------------------------------------------------

    use zyntax_typed_ast::type_registry::TypeId;
    use zyntax_typed_ast::InternedString;

    fn fid(module: &str, raw_id: u32) -> FsmId {
        FsmId {
            module: InternedString::new_global(module),
            type_id: TypeId::new(raw_id),
        }
    }

    fn intern(s: &str) -> InternedString {
        InternedString::new_global(s)
    }

    /// Distinct `FsmId`s (different modules, same TypeId) hash and
    /// compare distinctly so two same-named fsms in different
    /// modules don't collide. Same TypeId across modules can happen
    /// because TypeIds come from a process-global counter — pinning
    /// the (module, type_id) tuple semantics.
    #[test]
    fn fsm_id_disambiguates_by_module() {
        let a = fid("foo", 7);
        let b = fid("bar", 7);
        let c = fid("foo", 7);
        assert_ne!(a, b, "different modules → different ids");
        assert_eq!(a, c, "same (module, type_id) → equal");
    }

    /// Upsert + get round-trip with the expected shape, including
    /// the initial state, transitions, and tick guards. Exercises
    /// the bulk of the data API.
    #[test]
    fn fsm_registry_upsert_get() {
        let mut registry = FsmRegistry::new();
        let id = fid("main", 42);

        let def = FsmDefinition {
            initial: Some(intern("Idle")),
            transitions: vec![
                EventTransition {
                    from: intern("Idle"),
                    event: intern("Start"),
                    to: intern("Loading"),
                },
                EventTransition {
                    from: intern("Loading"),
                    event: intern("Done"),
                    to: intern("Success"),
                },
            ],
            tick_guards: vec![TickGuard {
                from: intern("Loading"),
                to: intern("TimedOut"),
                guard_fn: Some(intern("__fsm_tick_guard_Loader_0__")),
            }],
            name: Some(intern("Loader")),
        };

        registry.upsert(id, def.clone());
        let got = registry.get(&id).expect("inserted entry should exist");
        assert_eq!(got.initial, Some(intern("Idle")));
        assert_eq!(got.transitions.len(), 2);
        assert_eq!(got.transitions[1].event, intern("Done"));
        assert_eq!(got.tick_guards.len(), 1);
        assert_eq!(got.name, Some(intern("Loader")));
    }

    /// Re-inserting the same id replaces the entry (used by
    /// hot-reload paths). Pinning that the second upsert wins so
    /// stale state from prior runs doesn't leak.
    #[test]
    fn fsm_registry_upsert_replaces() {
        let mut registry = FsmRegistry::new();
        let id = fid("main", 1);

        registry.upsert(
            id,
            FsmDefinition {
                initial: Some(intern("V1")),
                ..FsmDefinition::default()
            },
        );
        registry.upsert(
            id,
            FsmDefinition {
                initial: Some(intern("V2")),
                ..FsmDefinition::default()
            },
        );

        let got = registry.get(&id).unwrap();
        assert_eq!(got.initial, Some(intern("V2")), "second upsert should win");
    }

    /// `remove` drops the entry and returns the prior value;
    /// subsequent `get` returns None. Mirrors the hot-reload-of-a-
    /// removed-fsm scenario where dispatch should fail loudly.
    #[test]
    fn fsm_registry_remove() {
        let mut registry = FsmRegistry::new();
        let id = fid("main", 5);
        registry.upsert(
            id,
            FsmDefinition {
                initial: Some(intern("S")),
                ..FsmDefinition::default()
            },
        );

        let removed = registry.remove(&id).expect("entry should exist");
        assert_eq!(removed.initial, Some(intern("S")));
        assert!(registry.get(&id).is_none(), "remove should drop the entry");
    }

    /// End-to-end: compiling a fsm-bearing program populates the
    /// global FsmRegistry. Pinning that the (module, TypeId) →
    /// FsmDefinition wiring works through compile_source. Each test
    /// scopes by a unique fsm name so the global registry doesn't
    /// collide across tests in the same process.
    #[test]
    fn compile_source_populates_fsm_registry() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        // Distinct fsm name per test to avoid stepping on the global
        // registry from concurrent test runs.
        dsl.compile_source(
            r#"
            fsm RegistryProbeA {
                state Idle
                state Running
                state Done
                initial Idle
                on Idle.Begin -> Running
                on Running.Finish -> Done
            }
            "#,
            "registry_probe_a.blinc",
        )
        .expect("compile");

        // Find the registry entry by name (we don't know the
        // assigned TypeId from outside).
        let module = InternedString::new_global("main");
        let probe = with_fsm_registry(|r| {
            r.iter()
                .find(|(id, def)| {
                    id.module == module
                        && def.name.and_then(|n| n.resolve_global()).as_deref()
                            == Some("RegistryProbeA")
                })
                .map(|(id, def)| (*id, def.clone()))
        });

        let (_id, def) = probe.expect("RegistryProbeA should be in the registry after compile");
        assert_eq!(
            def.initial.and_then(|n| n.resolve_global()).as_deref(),
            Some("Idle"),
            "initial state survived registry round-trip"
        );
        assert_eq!(
            def.transitions.len(),
            2,
            "expected two event-driven transitions"
        );
        assert_eq!(
            def.transitions[0].event.resolve_global().as_deref(),
            Some("Begin")
        );
        assert_eq!(
            def.transitions[1].event.resolve_global().as_deref(),
            Some("Finish")
        );
    }

    /// Each tick guard lifts into a stand-alone top-level
    /// function. The function name lands on the TickGuard so
    /// future dispatch can resolve it via `runtime.call::<bool>`.
    /// Pinning the naming convention
    /// (`__fsm_tick_guard_<Fsm>_<idx>__`) so a future refactor
    /// that changes the format breaks loudly here.
    #[test]
    fn compile_source_lifts_tick_guards_to_functions() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        // Two tick guards on the same fsm so we exercise the
        // index suffix — guard 0 and guard 1. Guards use bare
        // integer comparisons so the lifted bodies don't need
        // signal/symbol resolution we haven't wired up yet (the
        // shape we care about here is the lifting + naming, not
        // dispatch evaluation).
        let function_names = dsl
            .compile_source(
                r#"
                fsm GuardLiftProbe {
                    state Loading
                    state Done
                    state Failed
                    initial Loading
                    tick Loading -> Done when 1 > 0
                    tick Loading -> Failed when 1 < 0
                }
                "#,
                "guard_lift.blinc",
            )
            .expect("compile");

        // Both guard functions should appear in the compiled
        // function-name list returned from compile_source.
        assert!(
            function_names
                .iter()
                .any(|n| n == "__fsm_tick_guard_GuardLiftProbe_0__"),
            "expected guard 0 function in compiled symbols, got {:?}",
            function_names
        );
        assert!(
            function_names
                .iter()
                .any(|n| n == "__fsm_tick_guard_GuardLiftProbe_1__"),
            "expected guard 1 function in compiled symbols, got {:?}",
            function_names
        );

        // And the registry entry should reference both names on
        // its TickGuard records.
        let module = InternedString::new_global("main");
        let def = with_fsm_registry(|r| {
            r.iter()
                .find(|(id, def)| {
                    id.module == module
                        && def.name.and_then(|n| n.resolve_global()).as_deref()
                            == Some("GuardLiftProbe")
                })
                .map(|(_, def)| def.clone())
        })
        .expect("GuardLiftProbe should be registered");

        assert_eq!(def.tick_guards.len(), 2);
        assert_eq!(
            def.tick_guards[0]
                .guard_fn
                .and_then(|n| n.resolve_global())
                .as_deref(),
            Some("__fsm_tick_guard_GuardLiftProbe_0__")
        );
        assert_eq!(
            def.tick_guards[1]
                .guard_fn
                .and_then(|n| n.resolve_global())
                .as_deref(),
            Some("__fsm_tick_guard_GuardLiftProbe_1__")
        );
    }

    /// Tick guards land in the registry alongside event transitions.
    /// Pinning that the marker walker recognises `__fsm_tick__` and
    /// extracts the (from, to) pair (the guard expression itself is
    /// stripped for now — see `TickGuard` doc).
    #[test]
    fn compile_source_records_tick_guards_in_registry() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm RegistryProbeB {
                state Loading
                state Done
                initial Loading
                tick Loading -> Done when count.get() > 100
            }
            "#,
            "registry_probe_b.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let probe = with_fsm_registry(|r| {
            r.iter()
                .find(|(id, def)| {
                    id.module == module
                        && def.name.and_then(|n| n.resolve_global()).as_deref()
                            == Some("RegistryProbeB")
                })
                .map(|(_, def)| def.clone())
        });

        let def = probe.expect("RegistryProbeB should be in the registry");
        assert_eq!(def.tick_guards.len(), 1);
        assert_eq!(
            def.tick_guards[0].from.resolve_global().as_deref(),
            Some("Loading")
        );
        assert_eq!(
            def.tick_guards[0].to.resolve_global().as_deref(),
            Some("Done")
        );
        assert_eq!(
            def.transitions.len(),
            0,
            "tick-only fsm has no event transitions"
        );
    }

    /// Dispatch round-trip: compile a fsm, find it by name, walk a
    /// full transition cycle, verify each step lands on the
    /// expected state. End-to-end coverage of the dispatch API
    /// against a registry populated by `compile_source`.
    #[test]
    fn dispatch_round_trip() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm DispatchProbe {
                state Idle
                state Loading
                state Done
                initial Idle
                on Idle.Start -> Loading
                on Loading.Finish -> Done
                on Done.Reset -> Idle
            }
            "#,
            "dispatch_probe.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");

        // find_by_name resolves the (module, TypeId) from a bare
        // string at the boundary.
        let id = with_fsm_registry(|r| r.find_by_name(module, "DispatchProbe"))
            .expect("DispatchProbe should be in the registry");

        // Idle --Start--> Loading
        let next = with_fsm_registry(|r| r.step_event(&id, "Idle", "Start"));
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Loading")
        );

        // Loading --Finish--> Done
        let next = with_fsm_registry(|r| r.step_event(&id, "Loading", "Finish"));
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Done")
        );

        // Done --Reset--> Idle (full cycle)
        let next = with_fsm_registry(|r| r.step_event(&id, "Done", "Reset"));
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Idle")
        );
    }

    /// Non-matching transitions return `None`. Three failure modes
    /// covered: unknown event, wrong from-state, and unknown fsm
    /// id. Pinning these together ensures callers can use
    /// `Option::is_some` as a "transition is legal" predicate
    /// without false positives.
    #[test]
    fn dispatch_misses_return_none() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm DispatchMissProbe {
                state On
                state Off
                initial Off
                on Off.Click -> On
                on On.Click -> Off
            }
            "#,
            "dispatch_miss.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "DispatchMissProbe"))
            .expect("DispatchMissProbe should be in the registry");

        // (a) unknown event for the current state.
        let miss = with_fsm_registry(|r| r.step_event(&id, "Off", "DoesNotExist"));
        assert!(miss.is_none(), "unknown event should miss");

        // (b) right event but wrong from-state.
        //     Click is defined on Off and On but not on a state
        //     that doesn't exist — verify the from-match isn't
        //     loose.
        let miss = with_fsm_registry(|r| r.step_event(&id, "Nowhere", "Click"));
        assert!(miss.is_none(), "wrong from-state should miss");

        // (c) unknown fsm id (TypeId that doesn't correspond to a
        //     registered fsm).
        let phantom = FsmId {
            module,
            type_id: TypeId::new(u32::MAX),
        };
        let miss = with_fsm_registry(|r| r.step_event(&phantom, "Off", "Click"));
        assert!(miss.is_none(), "phantom fsm id should miss");
    }

    /// Tick dispatch fires when the lifted guard returns `true`.
    /// `1 > 0` always evaluates to `true`, so step_tick should
    /// return the to-state.
    #[test]
    fn step_tick_fires_when_guard_true() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm StepTickTrue {
                state Loading
                state Done
                initial Loading
                tick Loading -> Done when 1 > 0
            }
            "#,
            "step_tick_true.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "StepTickTrue"))
            .expect("StepTickTrue should be registered");

        let next = dsl.step_tick(&id, "Loading").expect("step_tick call");
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Done"),
            "guard `1 > 0` should fire and transition Loading → Done"
        );
    }

    /// Tick dispatch returns `None` when the lifted guard returns
    /// `false`. `1 < 0` is always false, so no transition.
    #[test]
    fn step_tick_no_transition_when_guard_false() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm StepTickFalse {
                state Loading
                state Done
                initial Loading
                tick Loading -> Done when 1 < 0
            }
            "#,
            "step_tick_false.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "StepTickFalse")).unwrap();

        let next = dsl.step_tick(&id, "Loading").expect("step_tick call");
        assert!(
            next.is_none(),
            "guard `1 < 0` should not fire — got {next:?}"
        );
    }

    /// Multiple guards from the same from-state: declaration order
    /// wins. The first guard whose expression returns true short-
    /// circuits the rest. Pin this against future refactors that
    /// might re-order guards or evaluate them in parallel.
    #[test]
    fn step_tick_first_true_guard_wins() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        // Two guards from Loading: the first is always true (so it
        // fires), the second would also be true (but never gets a
        // chance). Verifies "first" semantics — if step_tick eval'd
        // in arbitrary order or all-at-once, this would flake.
        dsl.compile_source(
            r#"
            fsm StepTickPriority {
                state Loading
                state Failed
                state Done
                initial Loading
                tick Loading -> Failed when 1 > 0
                tick Loading -> Done when 1 > 0
            }
            "#,
            "step_tick_priority.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "StepTickPriority")).unwrap();

        let next = dsl.step_tick(&id, "Loading").expect("step_tick call");
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Failed"),
            "first declared guard should fire (Loading → Failed), not Loading → Done"
        );
    }

    /// No tick guard matches the current from-state → `None`.
    /// Covers two related cases in one: the from-state has no
    /// guards at all (Done has none), and a state that doesn't
    /// exist as a from in any rule.
    #[test]
    fn step_tick_no_matching_from_state() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm StepTickNoMatch {
                state Loading
                state Done
                initial Loading
                tick Loading -> Done when 1 > 0
            }
            "#,
            "step_tick_no_match.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "StepTickNoMatch")).unwrap();

        // Done has no tick rules — should miss.
        let from_done = dsl.step_tick(&id, "Done").expect("step_tick");
        assert!(from_done.is_none(), "Done has no tick rules");

        // Phantom state name with no rules — should also miss.
        let from_phantom = dsl.step_tick(&id, "DoesNotExist").expect("step_tick");
        assert!(from_phantom.is_none(), "phantom from-state should miss");
    }

    // -----------------------------------------------------------------
    // Signal-resolved guard tests. The `signal <name>: <T>` decl
    // gets stripped before compile, and every `<name>.get()`
    // method call becomes a `__signal_get_<T>("<name>")` extern
    // call. Tests verify the rewrite shape on parsed AST — the
    // host-builtin side (registering `__signal_get_i32` and a
    // signal-value table) lands in a follow-up commit.
    // -----------------------------------------------------------------

    /// `count.get()` on an `i32` signal lowers to
    /// `__signal_get_i32("count")`. Verifies (a) the rewrite
    /// happened (the original MethodCall is gone), (b) the new
    /// callee is `__signal_get_i32`, (c) the signal name is
    /// preserved as a string literal arg.
    #[test]
    fn signal_get_rewrites_to_typed_extern_i32() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                signal count: i32
                fsm SignalProbeI32 {
                    state Idle
                    state Hot
                    initial Idle
                    tick Idle -> Hot when count.get() > 100
                }
                "#,
                "signal_i32.blinc",
            )
            .expect("parse");

        // The signal_decl extern should be stripped — no more
        // top-level Function named `count`.
        let has_signal_decl = program.declarations.iter().any(|d| {
            let zyntax_typed_ast::TypedDeclaration::Function(f) = &d.node else {
                return false;
            };
            f.name.resolve_global().as_deref() == Some("count")
        });
        assert!(
            !has_signal_decl,
            "signal-marker decl should be stripped before compile"
        );

        // The rewrite happens inside `__fsm_meta__`'s body — the
        // tick-guard expression is the second arg to a
        // `__fsm_tick__("from", <guard>, "to")` marker call. After
        // the rewrite, that <guard> is `Binary(Call(__signal_get_i32,
        // "count"), Gt, IntLit(100))`. Lifting into a top-level
        // function happens later, in compile_source's
        // populate_fsm_registry_pass — at parse_to_typed_ast level
        // the markers are still in place.
        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i)
                    if i.trait_name.resolve_global().as_deref() == Some("SignalProbeI32") =>
                {
                    Some(i)
                }
                _ => None,
            })
            .expect("SignalProbeI32 Impl");
        let meta = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
            .expect("__fsm_meta__ method");
        let body = meta.body.as_ref().expect("__fsm_meta__ body");

        // Walk body for the __fsm_tick__ marker; arg[1] is the
        // (now-rewritten) guard expression.
        let tick_call = body
            .statements
            .iter()
            .find_map(|s| {
                let TypedStatement::Expression(e) = &s.node else {
                    return None;
                };
                let TypedExpression::Call(c) = &e.node else {
                    return None;
                };
                let TypedExpression::Variable(callee) = &c.callee.node else {
                    return None;
                };
                if callee.resolve_global().as_deref() == Some("__fsm_tick__") {
                    Some(c)
                } else {
                    None
                }
            })
            .expect("__fsm_tick__ marker should exist");
        let guard = &tick_call.positional_args[1];
        let TypedExpression::Binary(cmp) = &guard.node else {
            panic!("guard should be Binary, got {:?}", guard.node);
        };
        let TypedExpression::Call(call) = &cmp.left.node else {
            panic!(
                "guard LHS should be Call after rewrite, got {:?}",
                cmp.left.node
            );
        };
        let TypedExpression::Variable(callee) = &call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            callee.resolve_global().as_deref(),
            Some("__signal_get_i32"),
            "signal call should rewrite to __signal_get_i32"
        );
        // Signal name preserved as the first string-literal arg.
        assert_eq!(call.positional_args.len(), 1);
        let TypedExpression::Literal(TypedLiteral::String(name)) = &call.positional_args[0].node
        else {
            panic!("expected string-literal name arg");
        };
        assert_eq!(name.resolve_global().as_deref(), Some("count"));
    }

    /// `name.get()` on a `string` signal lowers to
    /// `__signal_get_string("name")`. Pinning the per-type extern
    /// dispatch — same DSL surface, different host extern based
    /// on the signal's declared return type. The signal usage
    /// goes through a `let` initializer (which accepts any
    /// expression) since `text(...)` doesn't yet accept method
    /// calls — the rewrite walker handles let initializers.
    #[test]
    fn signal_get_rewrites_to_typed_extern_string() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                signal username: string
                component C {
                    state x: i32
                    view {}
                    fn step() { let s = username.get() }
                }
                "#,
                "signal_string.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("Impl block expected");
        let step = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("step"))
            .unwrap();
        let body = step.body.as_ref().unwrap();
        // body: let s = __signal_get_string("username")
        let TypedStatement::Let(let_node) = &body.statements[0].node else {
            panic!("expected Let stmt");
        };
        let init = let_node.initializer.as_ref().expect("let init");
        let TypedExpression::Call(sig_call) = &init.node else {
            panic!("let init should be Call after rewrite, got {:?}", init.node);
        };
        let TypedExpression::Variable(callee) = &sig_call.callee.node else {
            panic!("signal callee should be Variable");
        };
        assert_eq!(
            callee.resolve_global().as_deref(),
            Some("__signal_get_string"),
            "string-typed signal should rewrite to __signal_get_string"
        );
    }

    /// Multiple signals in the same program — each gets its
    /// rewrite based on its declared type. Verifies the pass
    /// builds a signal table from ALL signal_decl markers, not
    /// just the first one.
    #[test]
    fn multiple_signals_rewrite_independently() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                signal count: i32
                signal label: string
                fsm MultiSignalProbe {
                    state Idle
                    state Hot
                    initial Idle
                    tick Idle -> Hot when count.get() > 0
                }
                component C {
                    state x: i32
                    view {}
                    fn step() { let s = label.get() }
                }
                "#,
                "multi_signals.blinc",
            )
            .expect("parse");

        // Both signal-marker decls stripped.
        let strays: Vec<_> = program
            .declarations
            .iter()
            .filter_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Function(f) => {
                    let name = f.name.resolve_global();
                    if matches!(name.as_deref(), Some("count") | Some("label")) {
                        Some(name)
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect();
        assert!(
            strays.is_empty(),
            "signal markers should all be stripped, got strays: {strays:?}"
        );

        // Verify each signal has its expected extern in some call
        // somewhere. We just look at the program-wide presence of
        // both __signal_get_i32 and __signal_get_string callees.
        fn callee_exists(program: &TypedProgram, callee: &str) -> bool {
            fn walk_expr(e: &zyntax_typed_ast::TypedNode<TypedExpression>, callee: &str) -> bool {
                match &e.node {
                    TypedExpression::Call(c) => {
                        if let TypedExpression::Variable(name) = &c.callee.node {
                            if name.resolve_global().as_deref() == Some(callee) {
                                return true;
                            }
                        }
                        c.positional_args.iter().any(|a| walk_expr(a, callee))
                            || walk_expr(&c.callee, callee)
                    }
                    TypedExpression::Binary(b) => {
                        walk_expr(&b.left, callee) || walk_expr(&b.right, callee)
                    }
                    _ => false,
                }
            }
            fn walk_stmt(s: &zyntax_typed_ast::TypedNode<TypedStatement>, callee: &str) -> bool {
                match &s.node {
                    TypedStatement::Expression(e) => walk_expr(e, callee),
                    TypedStatement::Let(l) => l
                        .initializer
                        .as_ref()
                        .map(|init| walk_expr(init, callee))
                        .unwrap_or(false),
                    TypedStatement::If(i) => {
                        walk_expr(&i.condition, callee)
                            || i.then_block.statements.iter().any(|s| walk_stmt(s, callee))
                            || i.else_block
                                .as_ref()
                                .is_some_and(|b| b.statements.iter().any(|s| walk_stmt(s, callee)))
                    }
                    TypedStatement::Return(Some(e)) => walk_expr(e, callee),
                    _ => false,
                }
            }
            program.declarations.iter().any(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Function(f) => f
                    .body
                    .as_ref()
                    .map(|b| b.statements.iter().any(|s| walk_stmt(s, callee)))
                    .unwrap_or(false),
                zyntax_typed_ast::TypedDeclaration::Impl(i) => i.methods.iter().any(|m| {
                    m.body
                        .as_ref()
                        .map(|b| b.statements.iter().any(|s| walk_stmt(s, callee)))
                        .unwrap_or(false)
                }),
                _ => false,
            })
        }

        assert!(
            callee_exists(&program, "__signal_get_i32"),
            "i32 signal should produce __signal_get_i32 call"
        );
        assert!(
            callee_exists(&program, "__signal_get_string"),
            "string signal should produce __signal_get_string call"
        );
    }

    // -----------------------------------------------------------------
    // Host-machinery + end-to-end signal-guard tests. The DSL
    // surface (`signal <name>: i32`, `<name>.get()`) gets
    // rewritten to `__signal_get_i32("<name>")` calls; the host's
    // `blinc_signal_get_i32` reads from the per-thread
    // `SIGNAL_TABLE_I32`; `step_tick` JITs the rewritten guard
    // and dispatches based on the live signal value.
    // -----------------------------------------------------------------

    /// Round-trip: set signal → 200, compile a guard `> 100`,
    /// step_tick → fires (200 > 100). Pinning the host extern +
    /// table + JIT path stays connected. Uses a unique signal
    /// name so the per-thread table doesn't bleed across tests.
    #[test]
    fn signal_guard_fires_when_above_threshold() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.set_signal_i32("e2e_above", 200);
        dsl.compile_source(
            r#"
            signal e2e_above: i32
            fsm SignalGuardAbove {
                state Idle
                state Hot
                initial Idle
                tick Idle -> Hot when e2e_above.get() > 100
            }
            "#,
            "signal_e2e_above.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "SignalGuardAbove"))
            .expect("SignalGuardAbove should be registered");

        let next = dsl.step_tick(&id, "Idle").expect("step_tick");
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Hot"),
            "guard `e2e_above.get() > 100` (200) should fire"
        );
    }

    /// Same shape but signal value below threshold → no fire.
    /// Together with the above test, pins both branches of the
    /// guard against the live signal table.
    #[test]
    fn signal_guard_no_fire_when_below_threshold() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.set_signal_i32("e2e_below", 50);
        dsl.compile_source(
            r#"
            signal e2e_below: i32
            fsm SignalGuardBelow {
                state Idle
                state Hot
                initial Idle
                tick Idle -> Hot when e2e_below.get() > 100
            }
            "#,
            "signal_e2e_below.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "SignalGuardBelow")).unwrap();

        let next = dsl.step_tick(&id, "Idle").expect("step_tick");
        assert!(
            next.is_none(),
            "guard `e2e_below.get() > 100` (50) should not fire — got {next:?}"
        );
    }

    /// Mutating the signal between step_tick calls flips the
    /// guard's outcome. Pinning that the table is read at JIT
    /// time, not snapshotted at compile time — i.e. the guard
    /// expression dispatches to the live value, the way DSL
    /// authors expect from a "reactive signal" abstraction.
    #[test]
    fn signal_guard_reflects_updated_value() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.set_signal_i32("e2e_mut", 0);
        dsl.compile_source(
            r#"
            signal e2e_mut: i32
            fsm SignalGuardMut {
                state Idle
                state Hot
                initial Idle
                tick Idle -> Hot when e2e_mut.get() > 100
            }
            "#,
            "signal_e2e_mut.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "SignalGuardMut")).unwrap();

        // First tick: signal is 0 → guard misses.
        assert!(dsl.step_tick(&id, "Idle").unwrap().is_none());

        // Update signal to a value above threshold.
        dsl.set_signal_i32("e2e_mut", 999);

        // Second tick on the same compiled program: now fires.
        let next = dsl.step_tick(&id, "Idle").expect("step_tick");
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Hot"),
            "after raising the signal, the guard should fire"
        );
    }

    // -----------------------------------------------------------------
    // FsmInstance bridge tests. Verifies the dependency-free
    // bridge between the DSL fsm registry and host code:
    // construct → current() / dispatch_event() / tick() / reset().
    // No widget integration or Stateful coupling — just the live
    // string-keyed state that embedders can wrap however they want.
    // -----------------------------------------------------------------

    // -----------------------------------------------------------------
    // Float-literal + f64-signal tests. Verify the new
    // primary_expr alternate produces a FloatLiteral, the
    // `signal <name>: f64` decl routes `.get()` to
    // `__signal_get_f64`, and a guard like
    // `progress.get() >= 1.0` fires the JIT path correctly.
    // -----------------------------------------------------------------

    /// `1.0` parses as a `TypedLiteral::Float(f64)`. Pinning the
    /// new `float` rule against the existing `integer` rule
    /// (which would otherwise consume `1` as the longer prefix
    /// without the dot, breaking ambiguity).
    #[test]
    fn parse_float_literal() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state x: f64
                    view {}
                    fn step() { let p = 1.5 }
                }
                "#,
                "float_literal.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("step"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::Let(let_node) = &body.statements[0].node else {
            panic!("expected Let");
        };
        let init = let_node.initializer.as_ref().unwrap();
        let TypedExpression::Literal(TypedLiteral::Float(v)) = &init.node else {
            panic!("expected Float literal, got {:?}", init.node);
        };
        assert!((*v - 1.5_f64).abs() < f64::EPSILON, "expected 1.5, got {v}");
    }

    /// Negative float `-0.25` and scientific notation `1e3` both
    /// parse via the same `float` rule. Pinning both shapes so a
    /// future rule simplification doesn't regress.
    #[test]
    fn parse_float_literal_signed_and_scientific() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        for (label, src, expected) in [
            ("negative", "-0.25", -0.25_f64),
            ("scientific", "1e3", 1000.0_f64),
        ] {
            let program = dsl
                .parse_to_typed_ast(
                    &format!(
                        "component C {{ state x: f64 view {{}} fn step() {{ let p = {src} }} }}"
                    ),
                    &format!("float_{label}.blinc"),
                )
                .unwrap_or_else(|e| panic!("{label}: {e:?}"));
            let imp = program
                .declarations
                .iter()
                .find_map(|d| match &d.node {
                    zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                    _ => None,
                })
                .unwrap();
            let body = imp
                .methods
                .iter()
                .find(|m| m.name.resolve_global().as_deref() == Some("step"))
                .unwrap()
                .body
                .as_ref()
                .unwrap();
            let TypedStatement::Let(let_node) = &body.statements[0].node else {
                panic!("{label}: expected Let");
            };
            let init = let_node.initializer.as_ref().unwrap();
            let TypedExpression::Literal(TypedLiteral::Float(v)) = &init.node else {
                panic!("{label}: expected Float, got {:?}", init.node);
            };
            assert!(
                (*v - expected).abs() < 1e-9,
                "{label}: expected {expected}, got {v}"
            );
        }
    }

    /// `signal progress: f64` + `progress.get()` should rewrite
    /// to `__signal_get_f64("progress")`, mirroring the i32 path.
    #[test]
    fn signal_get_rewrites_to_typed_extern_f64() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                signal progress: f64
                fsm SignalProbeF64 {
                    state Loading
                    state Done
                    initial Loading
                    tick Loading -> Done when progress.get() >= 1.0
                }
                "#,
                "signal_f64.blinc",
            )
            .expect("parse");

        // Locate the __fsm_meta__ body on the SignalProbeF64 impl.
        let imp = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i)
                    if i.trait_name.resolve_global().as_deref() == Some("SignalProbeF64") =>
                {
                    Some(i)
                }
                _ => None,
            })
            .expect("SignalProbeF64 Impl");
        let meta = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
            .unwrap();
        let body = meta.body.as_ref().unwrap();

        // Find the __fsm_tick__ marker; arg[1] is the rewritten
        // guard expression `Binary(Call(__signal_get_f64, "progress"), Ge, FloatLit(1.0))`.
        let tick_call = body
            .statements
            .iter()
            .find_map(|s| {
                let TypedStatement::Expression(e) = &s.node else {
                    return None;
                };
                let TypedExpression::Call(c) = &e.node else {
                    return None;
                };
                let TypedExpression::Variable(callee) = &c.callee.node else {
                    return None;
                };
                (callee.resolve_global().as_deref() == Some("__fsm_tick__")).then_some(c)
            })
            .expect("__fsm_tick__ marker");

        let guard = &tick_call.positional_args[1];
        let TypedExpression::Binary(cmp) = &guard.node else {
            panic!("guard should be Binary, got {:?}", guard.node);
        };
        assert!(matches!(cmp.op, zyntax_typed_ast::BinaryOp::Ge));
        let TypedExpression::Call(sig_call) = &cmp.left.node else {
            panic!("LHS should be Call after rewrite");
        };
        let TypedExpression::Variable(callee) = &sig_call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            callee.resolve_global().as_deref(),
            Some("__signal_get_f64"),
            "f64 signal should rewrite to __signal_get_f64"
        );
        let TypedExpression::Literal(TypedLiteral::Float(v)) = &cmp.right.node else {
            panic!("RHS should be FloatLit, got {:?}", cmp.right.node);
        };
        assert!((*v - 1.0_f64).abs() < f64::EPSILON);
    }

    /// End-to-end: compile a fsm with a float-signal guard,
    /// move the signal across the threshold, watch step_tick
    /// fire. Verifies the i32-handling pattern extends cleanly
    /// to f64 — same dispatch, different table.
    #[test]
    fn float_signal_guard_fires_on_threshold() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.set_signal_f64("e2e_progress", 0.0);
        dsl.compile_source(
            r#"
            signal e2e_progress: f64
            fsm FloatGuardProbe {
                state Loading
                state Done
                initial Loading
                tick Loading -> Done when e2e_progress.get() >= 1.0
            }
            "#,
            "float_e2e.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "FloatGuardProbe"))
            .expect("FloatGuardProbe registered");

        // Below threshold → no transition.
        let next = dsl.step_tick(&id, "Loading").expect("step_tick");
        assert!(next.is_none(), "0.0 < 1.0, should not fire");

        // Cross the threshold.
        dsl.set_signal_f64("e2e_progress", 1.0);
        let next = dsl.step_tick(&id, "Loading").expect("step_tick");
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Done"),
            "1.0 >= 1.0, guard fires"
        );
    }

    /// Lifecycle: construct from a registered fsm, dispatch a
    /// sequence of events, watch `current()` follow each
    /// transition. End-to-end round-trip through the whole stack
    /// (parse → registry → instance → dispatch).
    #[test]
    fn fsm_instance_event_round_trip() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm InstanceProbeA {
                state Idle
                state Loading
                state Done
                initial Idle
                on Idle.Start -> Loading
                on Loading.Finish -> Done
                on Done.Reset -> Idle
            }
            "#,
            "instance_probe_a.blinc",
        )
        .expect("compile");

        let mut instance = FsmInstance::new(&dsl, "main", "InstanceProbeA")
            .expect("InstanceProbeA should construct");
        assert_eq!(instance.current(), "Idle", "starts in declared initial");

        // Idle --Start--> Loading
        let fired = instance.dispatch_event(&dsl, "Start");
        assert!(fired, "Idle.Start should transition");
        assert_eq!(instance.current(), "Loading");

        // Loading --Finish--> Done
        let fired = instance.dispatch_event(&dsl, "Finish");
        assert!(fired);
        assert_eq!(instance.current(), "Done");

        // Done --Reset--> Idle (full cycle)
        let fired = instance.dispatch_event(&dsl, "Reset");
        assert!(fired);
        assert_eq!(instance.current(), "Idle");
    }

    /// Misses: dispatch on an event that doesn't match the
    /// current from-state should leave `current()` unchanged
    /// and return false. Pinning that the instance's state
    /// can't drift on a no-op dispatch.
    #[test]
    fn fsm_instance_event_miss_keeps_current() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm InstanceProbeMiss {
                state Off
                state On
                initial Off
                on Off.Click -> On
                on On.Click -> Off
            }
            "#,
            "instance_probe_miss.blinc",
        )
        .expect("compile");

        let mut instance = FsmInstance::new(&dsl, "main", "InstanceProbeMiss").unwrap();
        assert_eq!(instance.current(), "Off");

        // Wrong event name from Off state → no transition.
        let fired = instance.dispatch_event(&dsl, "DoesNotExist");
        assert!(!fired);
        assert_eq!(instance.current(), "Off", "miss should leave state alone");
    }

    /// Tick + signal end-to-end through FsmInstance: live
    /// signal value drives the guard, instance updates when
    /// guard fires. Same plumbing as `signal_guard_*` tests but
    /// going through `FsmInstance::tick` instead of
    /// `BlincDsl::step_tick` directly — verifies the bridge
    /// composes with the JIT-evaluated guard path.
    #[test]
    fn fsm_instance_tick_with_signal_guard() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.set_signal_i32("instance_tick_count", 5);
        dsl.compile_source(
            r#"
            signal instance_tick_count: i32
            fsm InstanceProbeTick {
                state Cold
                state Hot
                initial Cold
                tick Cold -> Hot when instance_tick_count.get() > 100
            }
            "#,
            "instance_probe_tick.blinc",
        )
        .expect("compile");

        let mut instance = FsmInstance::new(&dsl, "main", "InstanceProbeTick").unwrap();

        // Signal below threshold → tick is a no-op.
        let fired = instance.tick(&dsl).expect("tick");
        assert!(!fired);
        assert_eq!(instance.current(), "Cold");

        // Raise the signal above threshold.
        dsl.set_signal_i32("instance_tick_count", 200);

        // Now tick fires, instance moves to Hot.
        let fired = instance.tick(&dsl).expect("tick");
        assert!(fired);
        assert_eq!(instance.current(), "Hot");
    }

    /// Reset returns the instance to its declared initial state.
    /// Pinning that reset() works from any current state, not
    /// only from the initial state.
    #[test]
    fn fsm_instance_reset() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm InstanceProbeReset {
                state Idle
                state Working
                initial Idle
                on Idle.Go -> Working
            }
            "#,
            "instance_probe_reset.blinc",
        )
        .expect("compile");

        let mut instance = FsmInstance::new(&dsl, "main", "InstanceProbeReset").unwrap();

        // Move off the initial state.
        instance.dispatch_event(&dsl, "Go");
        assert_eq!(instance.current(), "Working");

        // Reset.
        instance.reset();
        assert_eq!(
            instance.current(),
            "Idle",
            "reset should return to declared initial state"
        );
    }

    /// Construction fails cleanly when the fsm name doesn't
    /// match any registered fsm in the module. Pinning the
    /// "missing fsm → None" contract instead of e.g. panicking.
    #[test]
    fn fsm_instance_unknown_name_returns_none() {
        let dsl = BlincDsl::new().expect("runtime init");
        let attempt = FsmInstance::new(&dsl, "main", "DoesNotExistFsm");
        assert!(
            attempt.is_none(),
            "missing fsm should return None, not panic"
        );
    }

    /// Direct `FsmDefinition::step_event` works without going
    /// through the registry — useful for callers holding a
    /// borrowed FsmDefinition (e.g. iterating registry contents
    /// for diagnostics) and for unit-testing transition tables in
    /// isolation.
    #[test]
    fn fsm_definition_step_event_direct() {
        let def = FsmDefinition {
            initial: Some(intern("Idle")),
            transitions: vec![
                EventTransition {
                    from: intern("Idle"),
                    event: intern("Go"),
                    to: intern("Running"),
                },
                EventTransition {
                    from: intern("Running"),
                    event: intern("Stop"),
                    to: intern("Idle"),
                },
            ],
            ..FsmDefinition::default()
        };

        assert_eq!(
            def.step_event("Idle", "Go")
                .and_then(|n| n.resolve_global())
                .as_deref(),
            Some("Running")
        );
        assert_eq!(
            def.step_event("Running", "Stop")
                .and_then(|n| n.resolve_global())
                .as_deref(),
            Some("Idle")
        );
        assert!(def.step_event("Idle", "Stop").is_none());
        assert!(def.step_event("Done", "Go").is_none());
    }

    /// `with_fsm_registry` / `with_fsm_registry_mut` round-trip.
    /// Verifies the global accessors give shared access in both
    /// directions; if a future refactor switches the lock to
    /// something non-mutex-shaped these tests fail loudly.
    #[test]
    fn fsm_registry_global_accessors() {
        // Use a high TypeId so this test is unlikely to collide with
        // any other test that pokes the global registry.
        let id = fid("global_test_module", 9_999);

        with_fsm_registry_mut(|r| {
            r.upsert(
                id,
                FsmDefinition {
                    initial: Some(intern("Begin")),
                    ..FsmDefinition::default()
                },
            );
        });

        let initial = with_fsm_registry(|r| {
            r.get(&id)
                .and_then(|d| d.initial.and_then(|n| n.resolve_global()))
        });
        assert_eq!(initial.as_deref(), Some("Begin"));

        // Cleanup so other tests see a clean global state.
        with_fsm_registry_mut(|r| {
            r.remove(&id);
        });
    }

    /// Mixed-statement view exercises both `text(...)` arg shapes
    /// (string + integer) coexisting in the same compiled function
    /// and routing to distinct host builtins via the grammar's PEG
    /// alternate dispatch.
    #[test]
    fn round_trip_text_mixed_args() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(r#"view { text("answer:") text(42) }"#, "mixed_smoke.blinc")
            .expect("compile");
        let ops = dsl.render_view().expect("render_view");

        assert_eq!(ops.len(), 2, "expected 2 ops, got {ops:?}");
        match &ops[0] {
            DslOp::Text(s) => assert_eq!(s, "answer:"),
            other => panic!("expected DslOp::Text, got {other:?}"),
        }
        match &ops[1] {
            DslOp::IntText(n) => assert_eq!(*n, 42),
            other => panic!("expected DslOp::IntText, got {other:?}"),
        }
    }

    // =================================================================
    // Diagnostic-channel probes (ROADMAP §3.9 prototype goals)
    //
    // These exercise the failure modes we want to make sure surface
    // as actionable errors with file:line:col spans rather than panics
    // or generic strings. The assertions are deliberately lenient on
    // exact wording — Zyntax's diagnostic phrasing isn't stable yet —
    // but strict on (a) we got a `BlincDslError` (not a panic) and (b)
    // the string mentions the failing token / construct.
    //
    // If any of these regresses to a panic we'll catch it here before
    // it manifests as a poor user experience in `blinc dev`.
    // =================================================================

    /// **Parse error.** Source has a stray brace; the grammar can't
    /// match it. We expect a `BlincDslError::Compile` with the failing
    /// position called out.
    #[test]
    fn diag_parse_error_unmatched_brace() {
        let err = try_compile("view { text(\"hi\") } }", "parse_err.blinc")
            .expect_err("expected compile to fail on stray closing brace");
        let lower = err.to_lowercase();
        assert!(
            lower.contains("parse") || lower.contains("error") || lower.contains("expected"),
            "expected diagnostic to mention parse / error / expected; got: {err}"
        );
    }

    /// **Arity error.** `text()` with no argument violates the grammar
    /// rule `text_stmt = { "text" ~ "(" ~ s:string_literal ~ ")" }`.
    /// This is currently caught at parse time (not type-check time) —
    /// either is fine for the prototype, we just need a useful error.
    #[test]
    fn diag_arity_error_text_no_args() {
        let err = try_compile("view { text() }", "arity_err.blinc")
            .expect_err("expected compile to fail on text() with no args");
        let lower = err.to_lowercase();
        assert!(
            lower.contains("error") || lower.contains("expected") || lower.contains("parse"),
            "expected diagnostic to mention error / expected / parse; got: {err}"
        );
    }

    /// **Type error.** The grammar lets us emit a `text(...)` call with
    /// any expression as the arg, so we forge a typed-AST that calls
    /// `$Blinc$text` with an int literal. The injected extern decl
    /// expects `string`; Zyntax's type checker should catch the
    /// mismatch.
    ///
    /// We use `string_literal` in the grammar today, which won't
    /// produce ints — so this test exercises the type checker by
    /// wrapping the arg in something the grammar accepts but the
    /// types reject. Once the grammar grows expression nodes (phase 2)
    /// the test gets simpler. For now, keep it as a known-skip if the
    /// grammar can't produce the failing shape.
    ///
    /// **Rationale for keeping it:** when phase-2 expressions land
    /// this test will start exercising real type-mismatch paths
    /// without modification. It's pinned to the ZRTL signature
    /// boundary so as long as `$Blinc$text` declares
    /// `Type::Primitive(String)` the assertion shape stays correct.
    #[test]
    #[ignore = "phase 2 grammar will introduce expressions \
                that can be passed to text() — until then the grammar \
                only accepts string_literal so we can't construct a \
                type mismatch from source. Re-enable when grammar \
                supports e.g. integer literals as call args."]
    fn diag_type_error_text_with_int_literal() {
        let err = try_compile("view { text(42) }", "type_err.blinc")
            .expect_err("expected compile to fail on text(42)");
        let lower = err.to_lowercase();
        assert!(
            lower.contains("type") || lower.contains("expected") || lower.contains("string"),
            "expected diagnostic to mention type / expected / string; got: {err}"
        );
    }
}

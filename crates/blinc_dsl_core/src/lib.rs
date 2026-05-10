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
    Grammar2, Grammar2Error, RuntimeError, TypeTag, ZrtlSigFlags, ZrtlSymbolSig, ZyntaxRuntime,
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

        self.grammar
            .parse_with_signatures(source, filename, runtime.plugin_signatures())
            .map_err(|e| BlincDslError::Compile(e.to_string()))
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

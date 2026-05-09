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
use zyntax_typed_ast::typed_ast::ParameterKind;
use zyntax_typed_ast::{
    typed_node, CallingConvention, InternedString, Mutability, Span, TypedDeclaration,
    TypedFunction, TypedParameter, TypedProgram, TypedStatement, Visibility,
};

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
    /// `text("literal")` — a single text node.
    Text(String),
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
        let mut typed_program = self
            .grammar
            .parse_with_filename(source, filename)
            .map_err(|e| BlincDslError::Compile(e.to_string()))?;

        // Splice extern decls so type inference resolves the
        // `$Blinc$*` callees to their declared return / param types.
        // Without this step the body classifier rewrites the
        // synthesized `render_view` function's `Unit` return to a
        // dynamic value, producing a misaligned-pointer panic at
        // call time (see `BuiltinDescriptor`'s docs for the full
        // chain).
        inject_builtin_externs(&mut typed_program);

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

    /// Invoke the entry point and drain the scene buffer.
    ///
    /// The DSL's `view { ... }` block compiles to a `render_view()`
    /// function that pushes ops onto the host's scene buffer; this
    /// method calls it once and returns whatever the DSL emitted.
    pub fn render_view(&self) -> BlincDslResult<Vec<DslOp>> {
        let runtime = self
            .runtime
            .lock()
            .expect("BlincDsl runtime mutex poisoned");

        runtime.call::<()>("render_view", &[])?;
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
    vec![BuiltinDescriptor {
        name: "$Blinc$text",
        param_types: &[Type::Primitive(PrimitiveType::String)],
        return_type: Type::Primitive(PrimitiveType::Unit),
        ptr: blinc_text as *const u8,
    }]
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

/// Splice extern function declarations for every builtin into the
/// parsed `TypedProgram`.
///
/// The grammar's call sites lower to `TypedExpression::Variable`
/// callees naming the builtin symbol. Without an extern declaration
/// the type checker can't resolve the variable to a function and the
/// body classifier defaults the return type to `Any`, which then
/// overrides the function's declared `Type::Unit` return (see
/// `zyntax/crates/compiler/src/lowering.rs:1610-1664`). The
/// `inject_builtin_externs` path inside `Grammar2::parse_with_signatures`
/// does this for `.zrtl` plugin symbols; we mirror it for our static
/// builtins.
fn inject_builtin_externs(program: &mut TypedProgram) {
    let span = Span::default();

    for b in builtins() {
        let params: Vec<TypedParameter> = b
            .param_types
            .iter()
            .enumerate()
            .map(|(i, ty)| TypedParameter {
                name: InternedString::new_global(&format!("p{i}")),
                ty: ty.clone(),
                mutability: Mutability::Immutable,
                kind: ParameterKind::Regular,
                default_value: None,
                attributes: vec![],
                span,
            })
            .collect();

        // Build the extern decl directly rather than through
        // `TypedASTBuilder::extern_function`. The builder hard-codes
        // `CallingConvention::Cdecl` (typed_builder.rs:965) which the
        // compiler lowers to `hir::CallingConvention::C ->
        // CallConv::SystemV`. The call-site lowering at
        // `cranelift_backend.rs:2716` uses `module.make_signature()`
        // which yields the platform-default convention
        // (`AppleAarch64` on aarch64-apple-darwin). The mismatch
        // panics with `IncompatibleSignature` at codegen time.
        // `CallingConvention::Default` lowers to
        // `hir::CallingConvention::System`, which the backend then
        // honours as the platform default — matching the call site.
        let func = TypedFunction {
            name: InternedString::new_global(b.name),
            annotations: vec![],
            effects: vec![],
            type_params: vec![],
            params,
            return_type: b.return_type.clone(),
            body: None,
            visibility: Visibility::Public,
            is_async: false,
            is_pure: false,
            is_external: true,
            calling_convention: CallingConvention::Default,
            link_name: None,
        };

        program.declarations.push(typed_node(
            TypedDeclaration::Function(func),
            b.return_type.clone(),
            span,
        ));
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
        }
    }
}

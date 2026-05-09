# Upstream Zyntax patches required

`blinc_dsl_core` depends on a path-pinned local checkout of `zyntax_embed`
(see workspace `Cargo.toml`) that includes patches not yet upstream. We
need to PR these to the Zyntax repo before `blinc_dsl_core` can publish
or even build against a crates.io release.

Sibling repo: `/Users/amaterasu/Vibranium/zyntax`.

## 1. `ZyntaxRuntime::finalize_runtime_symbols`

**Where:** `crates/zyntax_embed/src/runtime.rs`, immediately after
`register_function`.

**What:** A small public method that rebuilds the JIT module so symbols
recorded via `register_function` become resolvable from
subsequently-compiled modules.

**Why:** `register_function` only updates the backend's accumulator —
the underlying Cranelift JITModule was constructed at
`ZyntaxRuntime::new()` time and only knows about symbols registered
before that. Plugin loaders (`load_plugin`,
`load_plugins_from_directory`) call `rebuild_with_accumulated_symbols`
internally to fix this; static-registration callers had no equivalent
public hook before this patch.

**Symptom without the patch:** `runtime.call(...)` fails inside
Cranelift with `can't resolve symbol $Foo$bar`.

## 2. `ZyntaxRuntime::register_function_typed`

**Where:** `crates/zyntax_embed/src/runtime.rs`, alongside
`register_function`.

**What:** Same shape as `register_function` plus a `ZrtlSymbolSig`
parameter. Stores the function pointer + arity, plus pushes the sig
into both `plugin_signatures` (consulted by
`Grammar2::parse_with_signatures` for `@builtin` extern injection) and
the backend's `symbol_signatures` table (consulted by
`cranelift_backend.rs:2719` at call-site lowering time so the codegen
signature matches the typed AST extern declaration).

**Why:** Without this, statically-registered builtins collide with
Zyntax-injected extern declarations on `IncompatibleSignature` because
the call site and the extern decl describe the symbol differently.
The plugin path (`load_plugin`) does the equivalent for `.zrtl`
symbols; this is the symmetric API for static linking, which is the
distribution shape Blinc DSL targets.

**Symptom without the patch:** `compile_typed_program` panics with
`Failed to declare symbol $Foo$bar: IncompatibleSignature(...)`.

## 3. Re-export `ZrtlSymbolSig`, `ZrtlSigFlags`, `RuntimeSymbolInfo`

**Where:** `crates/zyntax_embed/src/lib.rs`, the
`pub use zyntax_compiler::zrtl::{...}` block.

**What:** Add the three types to the re-export list so embed hosts can
construct sigs without pulling in `zyntax_compiler` directly.

**Why:** Without this, downstream crates either depend on
`zyntax_compiler` (heavy transitive surface) or can't construct the
signatures `register_function_typed` needs.

## Tracking

When upstreaming:

1. Open one PR per change for clean review.
2. After each lands and a release ships, switch
   `Cargo.toml`'s workspace dep on `zyntax_embed` from path-based to
   versioned, drop the relevant section from this file, and update
   the doc-comment in `lib.rs` that mentions the upstream caveats.
3. When all three are upstream, delete this file.

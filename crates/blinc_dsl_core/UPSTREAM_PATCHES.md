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

## 4. Generic `__fstring__` SSA lowering (not pin-coded to print)

**Where:** `crates/compiler/src/ssa.rs:3083+`, in
`TypedExpression::Call` translation. Add a generic intercept BEFORE
the existing print-family one.

**What:** When a `Call` expression's callee is `__fstring__`,
synthesize a heap-allocated formatted string by chaining
`$IO$string_concat($IO$format_dynamic(part1), …)` and return that
string as the call's value. Add a `translate_fstring_to_string`
helper next to the existing print-family code.

**Why:** Today the `__fstring__` intercept is hard-coded to
`println` / `print` / `eprintln` / `eprint` callers (ssa.rs:3093-3097).
For any other caller (`text(f"...")`, `let s = f"..."`, etc.) the
JIT tries to call a non-existent `__fstring__` symbol and segfaults.
Generalising fixes f-strings for embedded-DSL hosts without each
host having to write its own typed-AST desugar pass.

**Symptom without the patch:** Multi-part f-strings used outside
print/println segfault with `can't resolve symbol __fstring__`.

## 5. Grammar2 action `Block` expression (replaces v1 command chaining)

**Where:** Three files in `crates/zyn_peg/src/`:
- `grammar/ir.rs` — add `ExprIR::Block { bindings, result }` variant
- `grammar/parser.rs::parse_expr_atom` — recognise `{ let x = ...; let y = ...; expr }`
  syntax and produce the new variant
- `runtime2/interpreter.rs::eval_expr` — handle `Block` by pushing a
  new binding scope, evaluating each `(name, expr)` and binding the
  result, evaluating the final `result` expression, then popping
  the scope

**What:** Rust-native grammar-action block syntax that lets a single
rule emit multiple intermediate values and combine them into a
final result. e.g.:

```text
component_item = { ... } -> {
    let cls = TypedDeclaration::Class { name: intern(name), fields: states };
    let imp = TypedDeclaration::Impl { trait_name: intern(name), for_type: intern(name), items: [view_fn] };
    [cls, imp]
}
```

**Why:** The v1 (`runtime.rs`) grammar runtime supported this via
`RuleCommands { commands: Vec<AstCommand> }` and a JSON-shaped
chaining syntax (see `examples/zpeg_test/calc.zyn:14-21`). Grammar2
moved to a TypedAST-native action shape but didn't carry the
multi-emit capability across — every action returns a single
`ParsedValue`. For UI frameworks like Blinc where one source-level
construct (`component { state, view }`) naturally lowers to two
declarations (Class + Impl), the workaround is to split the source
syntax into two top-level items, which fights ergonomics. The
`Block` variant brings the v1 capability forward without bringing
back JSON.

**Symptom without the patch:** A single grammar rule can only
produce one TypedDeclaration; multi-decl emissions force source-
level splits. Specifically, `component <Name> { state ...  view
{ ... } }` cannot lower to `[Class, Impl]` from one `component_item`
rule today.

## 6. `TypedField` initializer in grammar interpreter

**Where:** `crates/zyn_peg/src/runtime2/interpreter.rs:1418-1439`,
`construct_field`.

**What:** Read an optional `init` field from the action and populate
`TypedField.initializer` with it (currently hardcoded to `None`).

**Why:** Without this we can't write `state count: i32 = 0` in DSLs
that use Class fields for state — initial values get silently
dropped at parse time. Workaround is to set defaults in an explicit
`fn init() { ... }` method on the impl block, which is verbose.

## Tracking

When upstreaming:

1. Open one PR per change for clean review.
2. After each lands and a release ships, switch
   `Cargo.toml`'s workspace dep on `zyntax_embed` from path-based to
   versioned, drop the relevant section from this file, and update
   the doc-comment in `lib.rs` that mentions the upstream caveats.
3. When all three are upstream, delete this file.

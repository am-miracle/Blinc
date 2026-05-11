//! Backend-agnostic view rendering.
//!
//! A [`ViewRenderer`] resolves a DSL-emitted view symbol (e.g.
//! `Counter$view` for an inherent-impl `view` method, or
//! `render_view` for a bare-form `view { ... }`) and runs it,
//! returning the scene ops the view produced.
//!
//! Lives in `blinc_runtime` so widget integration code can hold
//! an `Arc<dyn ViewRenderer>` without knowing whether the
//! backend is `blinc_dsl_core` (Zyntax+Cranelift, in-process
//! JIT) or a future AOT-only crate (Zyntax+LLVM, static link).
//! Both backends implement this trait the same way from the
//! widget side: name in, ops out.
//!
//! ## Ownership story
//!
//! Unlike [`super::fsm::GuardDispatcher`] (which lives in a
//! process-wide slot because FSM state names are global), view
//! renderers are typically held directly by the embed code that
//! constructed them — a top-level `App` struct, for example,
//! stores `Arc<dyn ViewRenderer>` alongside its widget tree.
//! There's no global slot here. Apps with two coexisting DSLs
//! (rare but legal) would hold two renderer Arcs without
//! contention.

use std::sync::Arc;

use crate::scene::DslOp;

/// Strategy for resolving a view symbol and running it.
///
/// Implementations:
///
/// - **`JitViewRenderer`** (in `blinc_dsl_core`) — wraps an
///   `Arc<Mutex<ZyntaxRuntime>>`, calls
///   `runtime.call::<()>(symbol, &[])` so the Cranelift JIT runs
///   the compiled view body, then drains
///   [`crate::scene::take`].
///
/// - **AOT renderer** (future) — wraps a static lookup table of
///   `extern "C" fn() -> ()` pointers produced at LLVM link
///   time, calls the pointer directly, drains the same scene
///   buffer.
///
/// Both produce identical [`DslOp`] streams from identical
/// `.blinc` sources. Widget code that consumes ops only depends
/// on this trait — neither backend needs to be linked at widget
/// compile time.
///
/// `Send + Sync` because renderers commonly live behind an
/// `Arc` shared across the UI thread + worker threads.
/// Backends whose underlying runtime is `!Send` (e.g. the JIT
/// path's `ZyntaxRuntime`) carry an `unsafe Send + Sync` impl
/// alongside a `Mutex` that serialises access — see the JIT
/// crate's `JitViewRenderer` for the safety argument.
pub trait ViewRenderer: Send + Sync + 'static {
    /// Run the view registered under `symbol` and return the
    /// scene ops it produced.
    ///
    /// `symbol` is the JIT-linker-visible name:
    ///
    /// - `"render_view"` for the bare-form `view { ... }` at
    ///   the top of a `.blinc` file.
    /// - `"<ComponentName>$view"` for an inherent-impl `view`
    ///   method inside a `component` block. The
    ///   `<ComponentName>$view` mangling is what Zyntax's
    ///   `lower_impl_block` emits for inherent-impl methods.
    ///
    /// The convenience helpers [`render_main`] and
    /// [`render_component`] construct these names so callers
    /// don't have to remember the mangling.
    fn render_named(&self, symbol: &str) -> Result<Vec<DslOp>, ViewRenderError>;
}

/// Errors a view-renderer might return. Kept simple — most
/// backend-specific failure detail folds into the `Backend`
/// variant's stringified payload.
#[derive(Debug, thiserror::Error)]
pub enum ViewRenderError {
    /// The symbol resolved to nothing the backend could call.
    /// Usually means the embedder's caller passed the wrong
    /// component name (typo, removed, never compiled).
    #[error("no view symbol `{0}` is registered")]
    NotFound(String),

    /// The backend ran the view but the call itself failed
    /// (panic in the JIT, link error in AOT, ABI mismatch,
    /// etc.). The string carries whatever diagnostic the
    /// backend produced.
    #[error("view-renderer backend error: {0}")]
    Backend(String),
}

/// Run the bare-form `view { ... }` at the top of the DSL
/// source. Equivalent to `renderer.render_named("render_view")`.
pub fn render_main(renderer: &Arc<dyn ViewRenderer>) -> Result<Vec<DslOp>, ViewRenderError> {
    renderer.render_named("render_view")
}

/// Run the view method of a named component. Constructs the
/// `<ComponentName>$view` mangling internally; callers pass
/// the user-visible name (e.g. `"Counter"`).
pub fn render_component(
    renderer: &Arc<dyn ViewRenderer>,
    name: &str,
) -> Result<Vec<DslOp>, ViewRenderError> {
    let symbol = format!("{name}$view");
    renderer.render_named(&symbol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// In-process test renderer that records every symbol
    /// called and emits a programmed scene-op set per call.
    /// Mirrors the shape of a real backend without dragging in
    /// any actual compile / JIT machinery.
    struct ScriptedRenderer {
        calls: Mutex<Vec<String>>,
        ops_for: Mutex<std::collections::HashMap<String, Vec<DslOp>>>,
    }

    impl ScriptedRenderer {
        fn new(scripts: &[(&str, Vec<DslOp>)]) -> Self {
            let mut map = std::collections::HashMap::new();
            for (sym, ops) in scripts {
                map.insert((*sym).to_string(), ops.clone());
            }
            Self {
                calls: Mutex::new(Vec::new()),
                ops_for: Mutex::new(map),
            }
        }
    }

    impl ViewRenderer for ScriptedRenderer {
        fn render_named(&self, symbol: &str) -> Result<Vec<DslOp>, ViewRenderError> {
            self.calls.lock().unwrap().push(symbol.to_string());
            self.ops_for
                .lock()
                .unwrap()
                .get(symbol)
                .cloned()
                .ok_or_else(|| ViewRenderError::NotFound(symbol.to_string()))
        }
    }

    /// `render_main` resolves to the `"render_view"` symbol.
    #[test]
    fn render_main_uses_render_view_symbol() {
        let renderer: Arc<dyn ViewRenderer> = Arc::new(ScriptedRenderer::new(&[(
            "render_view",
            vec![DslOp::Text("bare".into())],
        )]));
        let ops = render_main(&renderer).unwrap();
        assert_eq!(ops, vec![DslOp::Text("bare".into())]);
    }

    /// `render_component(name)` mangles to `<name>$view`.
    #[test]
    fn render_component_mangles_view_symbol() {
        let renderer: Arc<dyn ViewRenderer> = Arc::new(ScriptedRenderer::new(&[(
            "Counter$view",
            vec![DslOp::IntText(42)],
        )]));
        let ops = render_component(&renderer, "Counter").unwrap();
        assert_eq!(ops, vec![DslOp::IntText(42)]);
    }

    /// Unknown symbol surfaces as `ViewRenderError::NotFound`.
    #[test]
    fn unknown_symbol_returns_not_found() {
        let renderer: Arc<dyn ViewRenderer> = Arc::new(ScriptedRenderer::new(&[]));
        let err = render_component(&renderer, "Missing").unwrap_err();
        assert!(
            matches!(&err, ViewRenderError::NotFound(s) if s == "Missing$view"),
            "expected NotFound for `Missing$view`, got {err:?}"
        );
    }
}

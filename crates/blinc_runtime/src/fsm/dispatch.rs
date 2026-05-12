//! Backend-agnostic guard dispatch.
//!
//! Event-driven transitions resolve in pure Rust by reading the
//! [`super::registry::FsmRegistry`]. Tick-driven transitions can't
//! — they call user-authored guard expressions (lifted to
//! stand-alone functions by the DSL post-parse pass) that need
//! whichever compile backend produced them.
//!
//! This module is the boundary. A backend implements
//! [`GuardDispatcher::call_guard`] and registers an instance via
//! [`set_guard_dispatcher`]; the [`super::instance::FsmStateId`]
//! widget shim calls into the trait object without knowing
//! whether the actual call goes through Cranelift's JIT
//! (`runtime.call_function`) or a direct extern-C function
//! pointer (AOT-linked binary).
//!
//! The trait is intentionally small: each guard is a zero-arg
//! function returning `i32` (1 = fire, 0 = don't). This matches
//! how `blinc_dsl_core::populate_fsm_registry_pass` lifts
//! guard expressions today, and is trivially representable in
//! the AOT path's static symbol table.

use std::sync::{Arc, OnceLock};

/// Strategy for invoking a lifted-guard symbol.
///
/// Implementors:
/// - `JitGuardDispatcher` (in `blinc_dsl_core`) — wraps a
///   `BlincDsl` handle, dispatches via Zyntax's
///   `runtime.call_function` so Cranelift-compiled guards run
///   in-process.
/// - `AotGuardDispatcher` (future, in a per-app generated crate
///   or in `blinc_runtime::aot`) — wraps a static table of
///   `extern "C" fn() -> i32` pointers produced at LLVM link
///   time.
///
/// `Send + Sync` because the dispatcher lives behind an `Arc` in
/// a process-wide slot and may be called from any thread that
/// dispatches widget events.
pub trait GuardDispatcher: Send + Sync + 'static {
    /// Call the lifted guard function identified by `symbol`.
    ///
    /// Returns:
    /// - `Some(true)` — guard fired.
    /// - `Some(false)` — guard ran but didn't fire (typical
    ///   case for a tick where no condition is currently met).
    /// - `None` — the symbol couldn't be resolved or the call
    ///   itself errored. Treated as "guard didn't fire" by the
    ///   `FsmStateId::on_tick` walker; the registered tick loop
    ///   continues with the next guard.
    fn call_guard(&self, symbol: &str) -> Option<bool>;
}

/// Process-wide guard-dispatcher slot. Set once at app startup
/// by whichever backend is active; subsequent attempts replace
/// the previous dispatcher (last write wins) so hot-reload-style
/// re-bootstrap flows don't deadlock.
static GLOBAL_DISPATCHER: OnceLock<std::sync::RwLock<Option<Arc<dyn GuardDispatcher>>>> =
    OnceLock::new();

fn slot() -> &'static std::sync::RwLock<Option<Arc<dyn GuardDispatcher>>> {
    GLOBAL_DISPATCHER.get_or_init(|| std::sync::RwLock::new(None))
}

/// Install the process-wide guard dispatcher. Replaces any
/// previously-installed dispatcher.
///
/// Typically called once at app startup — the JIT path's
/// `BlincDsl::new()` does this, and the AOT path's generated
/// init function does it too. Apps that ship without any FSM
/// support can leave the slot empty; [`FsmStateId::on_tick`]
/// silently returns `None` in that case (no transitions fire).
pub fn set_guard_dispatcher(dispatcher: Arc<dyn GuardDispatcher>) {
    let mut guard = slot()
        .write()
        .expect("blinc_runtime::fsm::dispatch slot poisoned");
    *guard = Some(dispatcher);
}

/// Clear the currently-installed dispatcher. Used by tests
/// that want to verify the no-dispatcher fallback path.
pub fn clear_guard_dispatcher() {
    let mut guard = slot()
        .write()
        .expect("blinc_runtime::fsm::dispatch slot poisoned");
    *guard = None;
}

/// Call a guard via the currently-installed dispatcher. Returns
/// `None` if no dispatcher is installed (e.g. app has no FSM
/// support compiled in) or the dispatcher itself returned `None`.
pub(crate) fn call_guard(symbol: &str) -> Option<bool> {
    let guard = slot()
        .read()
        .expect("blinc_runtime::fsm::dispatch slot poisoned");
    let dispatcher = guard.as_ref()?;
    dispatcher.call_guard(symbol)
}

/// Test-only mutex that serializes every test toggling
/// `GUARD_DISPATCHER`. The dispatcher slot is process-wide, so
/// parallel tests in this crate (`dispatch::tests::*` and
/// `instance::tests::on_tick_*`) clobber each other without
/// serialization. Acquire at the top of any test that calls
/// `set_guard_dispatcher` / `clear_guard_dispatcher`.
#[cfg(test)]
pub(crate) static DISPATCHER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Simple test dispatcher that records every symbol called
    /// and returns a programmed result.
    struct ScriptedDispatcher {
        calls: Mutex<Vec<String>>,
        results: Mutex<std::collections::HashMap<String, bool>>,
    }

    impl ScriptedDispatcher {
        fn new(results: &[(&str, bool)]) -> Self {
            let mut map = std::collections::HashMap::new();
            for (sym, fire) in results {
                map.insert((*sym).to_string(), *fire);
            }
            Self {
                calls: Mutex::new(Vec::new()),
                results: Mutex::new(map),
            }
        }
    }

    impl GuardDispatcher for ScriptedDispatcher {
        fn call_guard(&self, symbol: &str) -> Option<bool> {
            self.calls.lock().unwrap().push(symbol.to_string());
            self.results.lock().unwrap().get(symbol).copied()
        }
    }

    /// With no dispatcher installed, `call_guard` returns `None`.
    /// This is the "FSM not compiled in" fallback.
    #[test]
    fn no_dispatcher_returns_none() {
        let _guard = DISPATCHER_TEST_LOCK.lock().unwrap();
        clear_guard_dispatcher();
        assert_eq!(call_guard("__fsm_tick_guard_Loader_0__"), None);
    }

    /// Installed dispatcher routes through to `call_guard` and
    /// records the symbol. Pins the trait wiring against
    /// accidental Box-vs-Arc confusion or wrong-arg-type churn.
    #[test]
    fn installed_dispatcher_is_invoked() {
        let _guard = DISPATCHER_TEST_LOCK.lock().unwrap();
        let scripted = Arc::new(ScriptedDispatcher::new(&[
            ("__fsm_tick_guard_Loader_0__", true),
            ("__fsm_tick_guard_Loader_1__", false),
        ]));
        set_guard_dispatcher(scripted.clone());

        assert_eq!(call_guard("__fsm_tick_guard_Loader_0__"), Some(true));
        assert_eq!(call_guard("__fsm_tick_guard_Loader_1__"), Some(false));
        assert_eq!(call_guard("__missing__"), None);

        let calls = scripted.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);

        // Restore the no-dispatcher state so other tests get a
        // clean slot regardless of execution order. (Cargo runs
        // tests in arbitrary order within the same binary.)
        clear_guard_dispatcher();
    }
}

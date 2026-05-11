//! Hot-reload hooks.
//!
//! **Hot-reload in Blinc is non-destructive.** The substrate
//! does not wipe its state when DSL source recompiles; the
//! JIT-side machinery handles in-place code replacement, and
//! the substrate's registries / signal tables / in-flight
//! widget trees survive across reloads.
//!
//! ## Why non-destructive
//!
//! Zyntax's tiered runtime is backed by [`beadie`] (see
//! `zyntax/crates/compiler/BEADIE_INTEGRATION.md`). When a
//! function gets recompiled — whether because the source
//! changed or because tier-up promoted it from baseline to
//! optimised code — beadie performs an atomic function-pointer
//! swap (`Bead::swap_compiled`) and, for long-running
//! invocations, **on-stack replacement** (OSR) transfers live
//! frames into the new code without unwinding.
//!
//! Practical consequences for the substrate:
//!
//! - **FSM registry stays.** State machines tied to a
//!   `Stateful<FsmStateId>` keep their current variant across
//!   reload. If the new source moves a state's transitions
//!   around, `FsmDefinition::register` (which replaces by
//!   name) updates the rules but doesn't touch the in-flight
//!   state. Widgets keep responding.
//!
//! - **Component registry stays.** Same story — the publisher
//!   re-registers with the new prop shape; the registry's
//!   replace-by-name semantics swap the definition in place.
//!   Any widget already holding a `ZyntaxValue` widget handle
//!   keeps that handle valid because the underlying compiled
//!   view function is updated via beadie, not deallocated.
//!
//! - **Signal tables stay.** User-facing state (scroll
//!   positions, form values, etc.) MUST survive reload —
//!   wiping them would be a UX regression every time the dev
//!   saves a file. The DSL surface for signals is "named
//!   storage cells"; reload doesn't change the names, just the
//!   code that reads them.
//!
//! - **Guard dispatcher stays.** It points at the runtime,
//!   not at any specific compiled function. Beadie's atomic
//!   swap means the dispatcher's `call_guard(symbol)` call
//!   picks up the new code automatically on the next
//!   invocation.
//!
//! - **View renderer stays.** Same shape — embed-side
//!   `Arc<dyn ViewRenderer>` holds a backend handle, not a
//!   compiled-code reference. Next `render_named` call routes
//!   through beadie to whatever code is currently installed.
//!
//! ## What reload actually does
//!
//! The runtime side: re-parse → re-compile → beadie swaps the
//! new function pointers into the bead table. Embedders kick
//! this off through whatever change-detection they wire up
//! (file watcher, IPC signal, etc.).
//!
//! The substrate side: nothing routine. Registries get updated
//! by the normal post-parse publishers (`publish_fsms_to_runtime_registry`,
//! `publish_components_to_runtime_registry`) that run on the
//! recompile path the same way they ran on the initial compile.
//! Replace-by-name semantics make this idempotent.
//!
//! ## When you DO want a hard reset
//!
//! There's still a place for destructive clears:
//!
//! - **Tests** that want to start from a known-empty slate
//!   between cases.
//! - **Embedders shutting down a DSL** entirely (switching
//!   to a different `.blinc` source, multi-tenant scenarios).
//! - **Recovery** from a crash where the substrate state may
//!   be corrupted.
//!
//! [`clear_all_destructive`] is the explicit-name function
//! for that. Routine reload should NEVER call it.
//!
//! [`beadie`]: https://docs.rs/beadie

use crate::{component, fsm, signal};

/// **Destructive** reset of every substrate's persistent state.
/// Wipes FSM + component registries, clears the guard
/// dispatcher slot, wipes signal tables.
///
/// **This is not the reload path.** Hot-reload in Blinc is
/// non-destructive — see the module-level docs for why.
/// Call this only from tests that need a known-empty slate,
/// or from embedders explicitly tearing down a DSL instance
/// (switching to a different source, shutting down, etc.).
///
/// Idempotent — calling twice is the same as once. Safe to
/// call from any thread, but embedders should serialise
/// against any in-flight render so the resets don't race a
/// concurrent registry read.
pub fn clear_all_destructive() {
    fsm::with_fsm_registry_mut(|r| r.clear());
    fsm::clear_guard_dispatcher();
    component::with_component_registry_mut(|r| r.clear());
    signal::clear_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn arc(s: &str) -> Arc<str> {
        Arc::from(s)
    }

    /// `clear_all_destructive` wipes every substrate. Plant
    /// something in each, call clear, observe everything is
    /// empty / absent. This is the test-only / shutdown path,
    /// NOT the reload path — see module docs.
    #[test]
    fn clear_all_destructive_wipes_every_substrate() {
        // Plant: fsm registry
        fsm::with_fsm_registry_mut(|r| {
            r.register(fsm::FsmDefinition {
                name: arc("ReloadTestFsm"),
                state_names: vec![arc("A")],
                initial_code: 0,
                ..Default::default()
            });
        });
        assert!(fsm::with_fsm_registry(|r| r
            .id_of("ReloadTestFsm")
            .is_some()));

        // Plant: guard dispatcher
        struct NopDispatcher;
        impl fsm::GuardDispatcher for NopDispatcher {
            fn call_guard(&self, _symbol: &str) -> Option<bool> {
                Some(false)
            }
        }
        fsm::set_guard_dispatcher(Arc::new(NopDispatcher));

        // Plant: component registry
        component::with_component_registry_mut(|r| {
            r.register(component::ComponentDefinition {
                name: arc("ReloadTestComponent"),
                view_symbol: arc("ReloadTestComponent$view"),
                props: vec![],
            });
        });
        assert!(component::with_component_registry(|r| r
            .id_of("ReloadTestComponent")
            .is_some()));

        // Plant: signal tables
        signal::set_i32("reload_test_count", 7);
        signal::set_f64("reload_test_progress", 0.5);
        assert_eq!(signal::get_i32("reload_test_count"), Some(7));
        assert_eq!(signal::get_f64("reload_test_progress"), Some(0.5));

        clear_all_destructive();

        assert!(
            fsm::with_fsm_registry(|r| r.id_of("ReloadTestFsm").is_none()),
            "fsm registry should be empty after clear"
        );
        assert!(
            component::with_component_registry(|r| r.id_of("ReloadTestComponent").is_none()),
            "component registry should be empty after clear"
        );
        assert_eq!(signal::get_i32("reload_test_count"), None);
        assert_eq!(signal::get_f64("reload_test_progress"), None);
    }

    /// `clear_all_destructive` is idempotent — second call
    /// sees the cleared state and does nothing harmful.
    #[test]
    fn clear_all_destructive_is_idempotent() {
        clear_all_destructive();
        clear_all_destructive();
        assert!(fsm::with_fsm_registry(|r| r.is_empty()));
        assert!(component::with_component_registry(|r| r.is_empty()));
    }

    /// Re-publishing an FSM definition by the same name
    /// replaces the rules in place — the FsmId stays stable,
    /// in-flight `FsmStateId`s keep working with the new
    /// transitions. This is the registry-side behaviour that
    /// makes non-destructive reload work: the publisher just
    /// calls `register` again with the new shape; widgets
    /// holding a `FsmStateId(fsm_id, variant)` see updated
    /// transitions on the next `on_event` / `on_tick` without
    /// any handoff.
    #[test]
    fn fsm_register_replaces_by_name_without_rotating_id() {
        clear_all_destructive();

        let first = fsm::with_fsm_registry_mut(|r| {
            r.register(fsm::FsmDefinition {
                name: arc("ReloadStableId"),
                state_names: vec![arc("Idle"), arc("Loading")],
                initial_code: 0,
                ..Default::default()
            })
        });

        // Reload — re-register with a different state set.
        let second = fsm::with_fsm_registry_mut(|r| {
            r.register(fsm::FsmDefinition {
                name: arc("ReloadStableId"),
                state_names: vec![arc("Idle"), arc("Loading"), arc("Done")],
                initial_code: 0,
                ..Default::default()
            })
        });

        assert_eq!(first, second, "FsmId stable across re-register by name");
        let def = fsm::with_fsm_registry(|r| r.get(first).cloned()).unwrap();
        assert_eq!(def.state_names.len(), 3, "definition reflects new shape");
    }
}

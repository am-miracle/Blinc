//! Hot-reload hooks.
//!
//! Resetting state when the DSL recompiles is necessarily a
//! cross-cutting concern — every substrate module has its own
//! persistent state (registries, dispatcher slot, scene
//! buffer, signal tables) that needs to drop in lockstep so a
//! re-published program doesn't see stale entries from the
//! previous compile.
//!
//! This module bundles the resets into one entry point so the
//! reload flow on either backend (`blinc_dsl_core` JIT, future
//! AOT crate) has a single thing to call. Embedders that want
//! finer-grained control can still reach for the individual
//! `clear` / `clear_all` functions on each substrate module.
//!
//! ## What gets reset
//!
//! [`reset_all`] clears (in this order):
//!
//! 1. **`fsm` registry** — drops every published FSM
//!    definition. State machines tied to a `Stateful<FsmStateId>`
//!    widget will fail to resolve their current variant after
//!    this; consumers should rebind their FsmStateId to a fresh
//!    one (typically via `FsmStateId::from_fsm_name`) after
//!    reload finishes re-publishing.
//! 2. **`fsm` guard dispatcher** — releases the JIT runtime
//!    handle the previous compile held. The next compile
//!    installs a fresh one. Without this clear, the dispatcher
//!    slot would briefly hold an `Arc` pointing at a now-
//!    discarded runtime, surfacing as `FunctionNotFound` for
//!    every guard call until reinstall.
//! 3. **`component` registry** — drops every published
//!    component definition. Same rebind-after-reload pattern as
//!    the FSM registry.
//! 4. **`signal` tables** — drops both i32 and f64 tables.
//!    Embedders that want signals to persist across reloads
//!    should snapshot before calling and re-seed after.
//! 5. **`scene` buffer** — drops any partially-built op
//!    stream. Reload should never happen mid-render, but the
//!    clear is cheap and avoids tail-end glitches.
//!
//! View renderers held by embedders as `Arc<dyn ViewRenderer>`
//! are NOT touched — they're owned outside the substrate.
//! Embedders typically swap them as a separate step (drop the
//! old, get a new one from the freshly-compiled backend).

use crate::{component, fsm, scene, signal};

/// Reset every substrate's persistent state. Call at the
/// start of a hot-reload cycle, before re-publishing.
///
/// Each substrate's reset is idempotent — calling `reset_all`
/// twice is the same as once. Safe to call from any thread,
/// though embedders should serialise reload with any in-flight
/// render so the resets don't race a concurrent push to the
/// scene buffer.
pub fn reset_all() {
    fsm::with_fsm_registry_mut(|r| r.clear());
    fsm::clear_guard_dispatcher();
    component::with_component_registry_mut(|r| r.clear());
    signal::clear_all();
    scene::clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn arc(s: &str) -> Arc<str> {
        Arc::from(s)
    }

    /// `reset_all` clears every substrate. Plant something in
    /// each, call reset, observe everything is empty / absent.
    #[test]
    fn reset_all_clears_every_substrate() {
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

        // Plant: scene buffer
        scene::push(scene::DslOp::Text("reload-leftover".into()));

        // Reset.
        reset_all();

        // Observe.
        assert!(
            fsm::with_fsm_registry(|r| r.id_of("ReloadTestFsm").is_none()),
            "fsm registry should be empty after reset"
        );
        assert!(
            component::with_component_registry(|r| r.id_of("ReloadTestComponent").is_none()),
            "component registry should be empty after reset"
        );
        assert_eq!(signal::get_i32("reload_test_count"), None);
        assert_eq!(signal::get_f64("reload_test_progress"), None);
        assert!(scene::take().is_empty());
    }

    /// `reset_all` is idempotent — second call sees the
    /// cleared state and does nothing harmful.
    #[test]
    fn reset_all_is_idempotent() {
        reset_all();
        reset_all();
        // Spot-check that nothing's left behind.
        assert!(fsm::with_fsm_registry(|r| r.is_empty()));
        assert!(component::with_component_registry(|r| r.is_empty()));
    }
}

//! `FsmStateId` — the `Copy + StateTransitions` widget-side
//! handle for a DSL-defined FSM's state.
//!
//! `blinc_layout::Stateful<S>` requires its state type to be
//! `Copy + Hash + ...`. We can't satisfy that with the
//! `Arc<str>`-keyed registry shape directly, so this module
//! defines a compact two-`u32` newtype that carries enough
//! identity (which FSM, which variant) to delegate every
//! transition through the registry.
//!
//! Event transitions resolve from the registry's HashMap in
//! pure Rust. Tick transitions route through the
//! [`super::dispatch::GuardDispatcher`] trait so the same shim
//! works for JIT-compiled and AOT-linked guards.

use blinc_layout::stateful::StateTransitions;

use super::dispatch::call_guard;
use super::registry::{with_fsm_registry, FsmId};

/// Widget-side handle for a DSL-defined FSM's current state.
///
/// Two fields:
/// - `fsm_id` — which FSM in the [`super::registry::FsmRegistry`].
/// - `variant` — the current state's variant code (index into the
///   FSM definition's `state_names` table).
///
/// `Copy` so it can live as the state slot inside a
/// `Stateful<FsmStateId>`. `Hash + Eq` so the widget framework's
/// internal change-detection comparisons stay cheap. `Debug` so
/// diagnostic logs print the `(fsm_id, variant)` tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FsmStateId {
    pub fsm_id: FsmId,
    pub variant: u32,
}

impl FsmStateId {
    /// Construct an `FsmStateId` directly from raw codes.
    /// Useful when the caller already knows the FSM id and the
    /// initial state code (the typical bootstrap path).
    pub const fn new(fsm_id: FsmId, variant: u32) -> Self {
        Self { fsm_id, variant }
    }

    /// Construct an `FsmStateId` from an FSM name. Looks up the
    /// id in the global registry; returns `None` when no FSM
    /// with that name has been published yet.
    ///
    /// The state starts at the FSM's declared initial state.
    /// Use [`Self::new`] when the caller needs to start the
    /// state machine at a non-initial variant.
    pub fn from_fsm_name(name: &str) -> Option<Self> {
        with_fsm_registry(|r| {
            let id = r.id_of(name)?;
            let def = r.get(id)?;
            Some(Self {
                fsm_id: id,
                variant: def.initial_code,
            })
        })
    }

    /// Resolve the current state's name from the registry.
    /// Returns `None` if the FSM was unregistered between
    /// construction and this call (shouldn't happen in steady
    /// state).
    pub fn state_name(&self) -> Option<std::sync::Arc<str>> {
        with_fsm_registry(|r| {
            r.get(self.fsm_id)
                .and_then(|def| def.state_name(self.variant).cloned())
        })
    }
}

impl StateTransitions for FsmStateId {
    fn on_event(&self, event: u32) -> Option<Self> {
        with_fsm_registry(|r| {
            let def = r.get(self.fsm_id)?;
            let to = def.step_event(self.variant, event)?;
            Some(Self {
                fsm_id: self.fsm_id,
                variant: to,
            })
        })
    }

    fn on_tick(&self) -> Option<Self> {
        // Snapshot the guard list before releasing the registry
        // lock — calling guards while holding the lock would
        // deadlock if a guard's own logic touches the registry
        // (it shouldn't, but defensive isolation here is cheap).
        let guards: Vec<(u32, std::sync::Arc<str>)> = with_fsm_registry(|r| {
            let def = r.get(self.fsm_id)?;
            Some(
                def.tick_guards
                    .iter()
                    .filter(|g| g.from_code == self.variant)
                    .map(|g| (g.to_code, g.guard_symbol.clone()))
                    .collect(),
            )
        })?;

        for (to_code, symbol) in guards {
            if call_guard(symbol.as_ref()) == Some(true) {
                return Some(Self {
                    fsm_id: self.fsm_id,
                    variant: to_code,
                });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsm::dispatch::{clear_guard_dispatcher, set_guard_dispatcher, GuardDispatcher};
    use crate::fsm::registry::{with_fsm_registry_mut, EventTransition, FsmDefinition, TickGuard};
    use std::sync::{Arc, Mutex};

    fn arc(s: &str) -> Arc<str> {
        Arc::from(s)
    }

    /// Test helper to publish a "Loader" FSM with Idle → Loading
    /// (on Start) → Done (on Finish), plus a single tick guard
    /// from Loading → Done.
    fn publish_loader() -> FsmId {
        with_fsm_registry_mut(|r| {
            r.register(FsmDefinition {
                name: arc("Loader_test"),
                state_names: vec![arc("Idle"), arc("Loading"), arc("Done")],
                event_names: vec![arc("Start"), arc("Finish")],
                initial_code: 0,
                transitions: vec![
                    EventTransition {
                        from_code: 0,
                        event_code: 0,
                        to_code: 1,
                    },
                    EventTransition {
                        from_code: 1,
                        event_code: 1,
                        to_code: 2,
                    },
                ],
                tick_guards: vec![TickGuard {
                    from_code: 1,
                    to_code: 2,
                    guard_symbol: arc("__fsm_tick_guard_Loader_test_0__"),
                }],
            })
        })
    }

    /// `on_event` dispatches via the registry — Idle + Start →
    /// Loading, no other transition matches.
    #[test]
    fn on_event_walks_registry_transitions() {
        let id = publish_loader();
        let s = FsmStateId::new(id, 0); // Idle

        // Start (event_code = 0) → Loading (variant = 1)
        let next = s.on_event(0).unwrap();
        assert_eq!(next.variant, 1);

        // From Loading, Start does nothing (no rule).
        assert_eq!(next.on_event(0), None);

        // From Loading, Finish → Done (variant = 2).
        let done = next.on_event(1).unwrap();
        assert_eq!(done.variant, 2);
    }

    /// `on_tick` consults the registered dispatcher — when the
    /// guard fires, the state transitions; otherwise stays put.
    #[test]
    fn on_tick_routes_through_dispatcher() {
        struct FixedDispatcher(bool);
        impl GuardDispatcher for FixedDispatcher {
            fn call_guard(&self, _symbol: &str) -> Option<bool> {
                Some(self.0)
            }
        }

        let id = publish_loader();
        let loading = FsmStateId::new(id, 1);

        // No dispatcher → no tick transition.
        clear_guard_dispatcher();
        assert_eq!(loading.on_tick(), None);

        // Dispatcher returns false → no tick transition.
        set_guard_dispatcher(Arc::new(FixedDispatcher(false)));
        assert_eq!(loading.on_tick(), None);

        // Dispatcher returns true → transition to Done.
        set_guard_dispatcher(Arc::new(FixedDispatcher(true)));
        let next = loading.on_tick().unwrap();
        assert_eq!(next.variant, 2);

        clear_guard_dispatcher();
    }

    /// `on_tick` only fires guards whose `from_code` matches the
    /// current state — a guard registered against a different
    /// variant doesn't accidentally trigger.
    #[test]
    fn on_tick_only_matches_current_variant() {
        struct AlwaysFire;
        impl GuardDispatcher for AlwaysFire {
            fn call_guard(&self, _symbol: &str) -> Option<bool> {
                Some(true)
            }
        }

        let id = publish_loader();
        // Idle (variant 0) has no tick guard — should stay None
        // even with an always-fire dispatcher installed.
        set_guard_dispatcher(Arc::new(AlwaysFire));
        let idle = FsmStateId::new(id, 0);
        assert_eq!(idle.on_tick(), None);
        clear_guard_dispatcher();
    }

    /// Multiple guards on the same state: first one that fires
    /// wins, in declaration order. Mirrors the DSL's
    /// match-arm-style first-match semantics.
    #[test]
    fn on_tick_first_fire_wins() {
        let id = with_fsm_registry_mut(|r| {
            r.register(FsmDefinition {
                name: arc("Priority_test"),
                state_names: vec![arc("A"), arc("B"), arc("C")],
                event_names: vec![],
                initial_code: 0,
                transitions: vec![],
                tick_guards: vec![
                    TickGuard {
                        from_code: 0,
                        to_code: 1,
                        guard_symbol: arc("guard_A_to_B"),
                    },
                    TickGuard {
                        from_code: 0,
                        to_code: 2,
                        guard_symbol: arc("guard_A_to_C"),
                    },
                ],
            })
        });

        struct PickSecond {
            count: Mutex<u32>,
        }
        impl GuardDispatcher for PickSecond {
            fn call_guard(&self, _symbol: &str) -> Option<bool> {
                let mut n = self.count.lock().unwrap();
                *n += 1;
                // First call: don't fire. Second call: fire.
                Some(*n == 2)
            }
        }

        set_guard_dispatcher(Arc::new(PickSecond {
            count: Mutex::new(0),
        }));
        let a = FsmStateId::new(id, 0);
        let next = a.on_tick().unwrap();
        assert_eq!(
            next.variant, 2,
            "second guard fired, so we should jump to C (variant 2)"
        );
        clear_guard_dispatcher();
    }

    /// `from_fsm_name` looks up by user-facing FSM name.
    #[test]
    fn from_fsm_name_resolves_initial_state() {
        let id = publish_loader();
        let s = FsmStateId::from_fsm_name("Loader_test").unwrap();
        assert_eq!(s.fsm_id, id);
        assert_eq!(s.variant, 0); // initial = Idle
        assert_eq!(
            FsmStateId::from_fsm_name("Nonexistent").map(|s| s.variant),
            None
        );
    }
}

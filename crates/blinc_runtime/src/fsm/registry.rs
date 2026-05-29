//! Runtime-agnostic FSM registry: pure data + process-wide
//! singleton accessors.
//!
//! No Zyntax types here — the registry holds plain `Arc<str>`
//! names and `u32` codes so that both the JIT path
//! (`blinc_dsl_core`, which intern-resolves to `Arc<str>` at
//! registration) and the AOT path (a future codegen that writes
//! the same `Arc<str>` literals into a generated registration
//! function) share one identical shape.
//!
//! Each FSM is registered as a [`FsmDefinition`] keyed by an
//! [`FsmId`]. State variants and event codes are pre-assigned
//! `u32` values at registration time so the per-instance
//! [`super::instance::FsmStateId`] (which must be `Copy`) can
//! identify a state with just two integers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Opaque process-wide FSM identifier.
///
/// Minted by [`FsmRegistry::register`] in registration order.
/// Stable across the lifetime of a process. Designed for the
/// `Stateful<S>` pattern where the state value must be `Copy`,
/// so we pack a `u32` instead of an `Arc<str>` even though the
/// canonical FSM name also lives in the [`FsmDefinition`] for
/// diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FsmId(pub u32);

/// One event-driven transition. Names map to user-facing DSL
/// surface (`on <from>.<event> -> <to>`); codes are the
/// registry-assigned integer ids the dispatch path consumes.
#[derive(Debug, Clone)]
pub struct EventTransition {
    /// Source state variant code (registered index in the FSM's
    /// state-name table). Matches the discriminant the
    /// `Stateful<FsmStateId>` widget sees in its current state.
    pub from_code: u32,
    /// Event code (registered index in the FSM's event-name
    /// table). Callers translate user-facing event names via
    /// [`FsmDefinition::event_code`] before dispatching.
    pub event_code: u32,
    /// Target state variant code.
    pub to_code: u32,
    /// Actions to execute, in source order, after the state
    /// advances. Captures the `{ count = count + 1 }` blocks
    /// declared on the DSL `on … -> …` transition.
    pub actions: Vec<TransitionAction>,
}

/// Side effect declared on a transition body in the DSL.
///
/// Two flavours:
/// - The "small deterministic set" — `SetI32` / `AddI32` —
///   covers literal-shape mutations the substrate can execute
///   without a JIT (legacy lowering path; both JIT and AOT can
///   emit these directly).
/// - `Symbol` — names a lifted top-level fn (typically
///   `__fsm_action_<Fsm>_<idx>__`) compiled into the JIT
///   module. Used for arbitrary action bodies like
///   `{ ctx.count = ctx.count + 1 }` that can't be reduced to
///   a single enum variant. Dispatched through
///   [`super::dispatch::GuardDispatcher::call_action`].
#[derive(Debug, Clone)]
pub enum TransitionAction {
    /// `<signal_name> = <int_literal>` — set the named i32
    /// signal to a constant.
    SetI32 { signal: Arc<str>, value: i32 },
    /// `<signal_name> = <signal_name> + <int_literal>` (or `-`)
    /// — add a constant delta to the named i32 signal.
    AddI32 { signal: Arc<str>, delta: i32 },
    /// Arbitrary JIT-resolved action. The body has been lifted
    /// to a top-level zero-arg `extern "C" fn()` whose symbol
    /// name lives here; the runtime dispatches it via
    /// [`super::dispatch::call_action`] which routes through
    /// the installed [`super::GuardDispatcher`].
    Symbol(Arc<str>),
}

/// One tick-driven (data-guarded) transition. The guard
/// function's symbol name is what backends resolve at dispatch
/// time — Cranelift's `call_function` for JIT, a plain
/// `extern "C" fn() -> i32` pointer for AOT. The runtime doesn't
/// care which.
#[derive(Debug, Clone)]
pub struct TickGuard {
    /// Source state variant code.
    pub from_code: u32,
    /// Target state variant code (taken if the guard fires).
    pub to_code: u32,
    /// Symbol name of the lifted guard function. The function
    /// takes zero args and returns an `i32` (1 = guard fires,
    /// 0 = doesn't). The i32 ABI mirrors how `blinc_dsl_core`
    /// emits its lifted guards today — see
    /// `populate_fsm_registry_pass` in that crate.
    pub guard_symbol: Arc<str>,
}

/// All the data needed to dispatch transitions for a single FSM.
///
/// `state_names` and `event_names` are the lookup tables that
/// translate user-facing strings into the integer codes
/// [`EventTransition`] / [`TickGuard`] reference. Indexing into
/// these arrays IS the code: `state_names[2]` is variant code
/// `2`.
#[derive(Debug, Clone, Default)]
pub struct FsmDefinition {
    /// User-visible FSM name (the DSL identifier, e.g.
    /// `"Loader"`). Used for diagnostics and for the runtime
    /// name → id lookup.
    pub name: Arc<str>,
    /// Initial state code (index into `state_names`).
    pub initial_code: u32,
    /// State variant names, indexed by variant code.
    pub state_names: Vec<Arc<str>>,
    /// Event names, indexed by event code.
    pub event_names: Vec<Arc<str>>,
    /// Event-driven transitions, in declaration order. The
    /// first matching rule wins — same match-arm semantics the
    /// DSL surface implies.
    pub transitions: Vec<EventTransition>,
    /// Tick-driven guards, in declaration order. Same
    /// first-match semantics as event transitions.
    pub tick_guards: Vec<TickGuard>,
}

/// High-bit-set offset added to every FSM event code so DSL-defined
/// FSM events can't collide with widget pointer-event codes (which
/// live in the low range — `POINTER_DOWN = 1`, …, `DRAG_END = 7`,
/// see [`blinc_core::events::event_types`]).
///
/// The widget-layer `Stateful` auto-registers `POINTER_DOWN` /
/// `POINTER_UP` / etc. handlers on every container and feeds their
/// numeric codes into `StatefulInner::dispatch` → `state.on_event`.
/// For builtin widget states (`ButtonState`, `ScrollState`) those
/// codes are exactly the transitions the state machine defines. For
/// DSL FSMs they're meaningless — but without an offset they
/// accidentally match the FSM's own sequentially-assigned event
/// codes (e.g. DSL `Reset` = 1 collides with `POINTER_DOWN` = 1, so
/// a click silently dispatches `Reset` on the FSM).
///
/// Bumping every FSM event code into the high range puts the two
/// namespaces in disjoint orbits: pointer events stay tiny, FSM
/// events live above `0x4000_0000`. `step_event` looks up by raw
/// code and won't match across namespaces.
pub const FSM_EVENT_CODE_OFFSET: u32 = 0x4000_0000;

impl FsmDefinition {
    /// Look up the variant code for a state name. Returns
    /// `None` when the name isn't registered.
    pub fn state_code(&self, name: &str) -> Option<u32> {
        self.state_names
            .iter()
            .position(|n| n.as_ref() == name)
            .map(|i| i as u32)
    }

    /// Look up the event code for an event name. Adds
    /// [`FSM_EVENT_CODE_OFFSET`] so the returned code can be passed
    /// directly to `inner.dispatch(...)` without colliding with
    /// widget pointer events.
    pub fn event_code(&self, name: &str) -> Option<u32> {
        self.event_names
            .iter()
            .position(|n| n.as_ref() == name)
            .map(|i| i as u32 + FSM_EVENT_CODE_OFFSET)
    }

    /// Reverse of [`Self::state_code`] for diagnostics. Returns
    /// `None` when `code` is out of range.
    pub fn state_name(&self, code: u32) -> Option<&Arc<str>> {
        self.state_names.get(code as usize)
    }

    /// Resolve an event-driven transition. First matching rule
    /// wins, same semantics as the DSL surface.
    pub fn step_event(&self, from: u32, event: u32) -> Option<u32> {
        self.transitions
            .iter()
            .find(|t| t.from_code == from && t.event_code == event)
            .map(|t| t.to_code)
    }
}

/// Process-wide registry. Keyed by [`FsmId`]; also carries a
/// name → id index for runtime lookup by FSM name.
#[derive(Debug, Default)]
pub struct FsmRegistry {
    defs: HashMap<FsmId, FsmDefinition>,
    name_index: HashMap<Arc<str>, FsmId>,
    next_id: u32,
}

impl FsmRegistry {
    /// Empty registry. Embedders normally interact with the
    /// process-wide singleton via [`with_fsm_registry`] /
    /// [`with_fsm_registry_mut`] rather than constructing one of
    /// these directly — but a fresh registry is useful for
    /// tests.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) an FSM definition. Returns the
    /// assigned [`FsmId`]. Replaces any prior entry with the
    /// same name — useful for hot-reload scenarios where the
    /// same DSL source gets recompiled.
    pub fn register(&mut self, def: FsmDefinition) -> FsmId {
        if let Some(&existing) = self.name_index.get(&def.name) {
            self.defs.insert(existing, def);
            return existing;
        }
        let id = FsmId(self.next_id);
        self.next_id += 1;
        self.name_index.insert(def.name.clone(), id);
        self.defs.insert(id, def);
        id
    }

    /// Look up by id. Returns `None` when nothing's registered.
    pub fn get(&self, id: FsmId) -> Option<&FsmDefinition> {
        self.defs.get(&id)
    }

    /// Resolve an FSM name to its [`FsmId`].
    pub fn id_of(&self, name: &str) -> Option<FsmId> {
        self.name_index.get(name).copied()
    }

    /// Total registered FSMs.
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    /// Whether the registry has no entries.
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// Clear all registrations. Used by hot-reload paths that
    /// want a clean slate before re-publishing.
    pub fn clear(&mut self) {
        self.defs.clear();
        self.name_index.clear();
        self.next_id = 0;
    }
}

/// Process-wide registry singleton. Reachable from both the JIT
/// publisher (`blinc_dsl_core` calls `with_fsm_registry_mut` to
/// publish freshly-parsed FSMs) and the widget consumer
/// (`FsmStateId::on_event` calls `with_fsm_registry` to resolve
/// transitions).
static GLOBAL_FSM_REGISTRY: OnceLock<Mutex<FsmRegistry>> = OnceLock::new();

fn lock() -> std::sync::MutexGuard<'static, FsmRegistry> {
    GLOBAL_FSM_REGISTRY
        .get_or_init(|| Mutex::new(FsmRegistry::new()))
        .lock()
        .expect("blinc_runtime::fsm::FsmRegistry mutex poisoned")
}

/// Read-only access to the global FSM registry.
pub fn with_fsm_registry<R>(f: impl FnOnce(&FsmRegistry) -> R) -> R {
    let guard = lock();
    f(&guard)
}

/// Mutable access to the global FSM registry. Used by the JIT /
/// AOT publishers when registering newly-discovered FSMs at
/// app startup.
pub fn with_fsm_registry_mut<R>(f: impl FnOnce(&mut FsmRegistry) -> R) -> R {
    let mut guard = lock();
    f(&mut guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arc(s: &str) -> Arc<str> {
        Arc::from(s)
    }

    /// Registering an FSM mints a fresh id; re-registering by
    /// the same name reuses the id (hot-reload semantics).
    #[test]
    fn register_assigns_ids_and_dedupes_by_name() {
        let mut r = FsmRegistry::new();
        let id_a = r.register(FsmDefinition {
            name: arc("Loader"),
            state_names: vec![arc("Idle")],
            initial_code: 0,
            ..Default::default()
        });
        let id_b = r.register(FsmDefinition {
            name: arc("Toggle"),
            state_names: vec![arc("Off")],
            initial_code: 0,
            ..Default::default()
        });
        assert_ne!(id_a, id_b);

        // Re-register `Loader` — should return the SAME id.
        let id_a_again = r.register(FsmDefinition {
            name: arc("Loader"),
            state_names: vec![arc("Idle"), arc("Loading"), arc("Done")],
            initial_code: 0,
            ..Default::default()
        });
        assert_eq!(id_a, id_a_again);
        assert_eq!(r.len(), 2);
        // And the latest definition won — state names are now 3.
        assert_eq!(r.get(id_a).unwrap().state_names.len(), 3);
    }

    /// `step_event` returns the target state code in declaration
    /// order; non-matching events return `None`.
    #[test]
    fn step_event_first_match_wins() {
        let def = FsmDefinition {
            name: arc("Loader"),
            state_names: vec![arc("Idle"), arc("Loading"), arc("Done")],
            event_names: vec![arc("Start"), arc("Finish")],
            initial_code: 0,
            transitions: vec![
                EventTransition {
                    from_code: 0,
                    event_code: 0,
                    to_code: 1,
                    actions: vec![],
                },
                EventTransition {
                    from_code: 1,
                    event_code: 1,
                    to_code: 2,
                    actions: vec![],
                },
            ],
            tick_guards: vec![],
        };

        // Idle + Start → Loading
        assert_eq!(def.step_event(0, 0), Some(1));
        // Loading + Finish → Done
        assert_eq!(def.step_event(1, 1), Some(2));
        // Idle + Finish — no rule, None.
        assert_eq!(def.step_event(0, 1), None);
        // Done + Start — also no rule (one-way).
        assert_eq!(def.step_event(2, 0), None);
    }

    /// `state_code` / `event_code` round-trip names through the
    /// lookup tables.
    #[test]
    fn name_to_code_round_trip() {
        let def = FsmDefinition {
            name: arc("Loader"),
            state_names: vec![arc("Idle"), arc("Loading")],
            event_names: vec![arc("Start")],
            initial_code: 0,
            ..Default::default()
        };
        assert_eq!(def.state_code("Idle"), Some(0));
        assert_eq!(def.state_code("Loading"), Some(1));
        assert_eq!(def.state_code("Nope"), None);
        // Event codes carry FSM_EVENT_CODE_OFFSET so they can't
        // collide with widget pointer-event codes — see the constant's
        // doc comment for the rationale.
        assert_eq!(def.event_code("Start"), Some(FSM_EVENT_CODE_OFFSET));
        assert_eq!(def.event_code("Nope"), None);
    }
}

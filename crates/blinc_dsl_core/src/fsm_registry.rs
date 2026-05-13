// =====================================================================
// FSM registry
// =====================================================================
//
// `(module, TypeId)` keys so same-named fsms in different modules don't collide.

use super::*;

/// Identity of an fsm in the global registry: Zyntax module + type-registry `TypeId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FsmId {
    /// Zyntax module name. Currently always `"main"`.
    pub module: zyntax_typed_ast::InternedString,
    /// The fsm's enum `TypeId` from the program's `type_registry`.
    pub type_id: zyntax_typed_ast::type_registry::TypeId,
}

/// Tick-driven guard. The guard expression is lifted into a top-level function
/// `__fsm_tick_guard_<FsmName>_<idx>__` so dispatch can call it as a normal symbol.
#[derive(Debug, Clone)]
pub struct TickGuard {
    pub from: zyntax_typed_ast::InternedString,
    pub to: zyntax_typed_ast::InternedString,
    /// Synthesised guard-function symbol name.
    pub guard_fn: Option<zyntax_typed_ast::InternedString>,
}

/// One event-driven transition: `on <from>.<event> -> <to> { <action>... }`.
#[derive(Debug, Clone)]
pub struct EventTransition {
    pub from: zyntax_typed_ast::InternedString,
    pub event: zyntax_typed_ast::InternedString,
    pub to: zyntax_typed_ast::InternedString,
    /// Actions in source order.
    pub actions: Vec<blinc_runtime::fsm::TransitionAction>,
}

/// Runtime definition of an fsm — populated by the `__fsm_meta__` body.
#[derive(Debug, Clone, Default)]
pub struct FsmDefinition {
    /// Initial state name.
    pub initial: Option<zyntax_typed_ast::InternedString>,
    /// Event-driven transitions in declaration order.
    pub transitions: Vec<EventTransition>,
    /// Tick-driven guards in declaration order.
    pub tick_guards: Vec<TickGuard>,
    /// Bare fsm name (for diagnostics; authoritative identity is `FsmId`).
    pub name: Option<zyntax_typed_ast::InternedString>,
}

impl FsmDefinition {
    /// Resolve an event-driven transition. First matching rule wins (declaration order).
    pub fn step_event(
        &self,
        current: &str,
        event: &str,
    ) -> Option<zyntax_typed_ast::InternedString> {
        let current_i = zyntax_typed_ast::InternedString::new_global(current);
        let event_i = zyntax_typed_ast::InternedString::new_global(event);
        self.transitions
            .iter()
            .find(|t| t.from == current_i && t.event == event_i)
            .map(|t| t.to)
    }
}

/// Process-wide registry of fsm definitions keyed by `FsmId`.
#[derive(Debug, Default)]
pub struct FsmRegistry {
    fsms: std::collections::HashMap<FsmId, FsmDefinition>,
}

impl FsmRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert/update an fsm definition.
    pub fn upsert(&mut self, id: FsmId, def: FsmDefinition) {
        self.fsms.insert(id, def);
    }

    pub fn get(&self, id: &FsmId) -> Option<&FsmDefinition> {
        self.fsms.get(id)
    }

    pub fn get_mut(&mut self, id: &FsmId) -> Option<&mut FsmDefinition> {
        self.fsms.get_mut(id)
    }

    pub fn len(&self) -> usize {
        self.fsms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fsms.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&FsmId, &FsmDefinition)> {
        self.fsms.iter()
    }

    /// Remove an fsm — used during hot-reload to drop stale entries.
    pub fn remove(&mut self, id: &FsmId) -> Option<FsmDefinition> {
        self.fsms.remove(id)
    }

    /// Find an fsm by source-level name within a module (linear scan).
    pub fn find_by_name(
        &self,
        module: zyntax_typed_ast::InternedString,
        name: &str,
    ) -> Option<FsmId> {
        let needle = zyntax_typed_ast::InternedString::new_global(name);
        self.fsms
            .iter()
            .find(|(id, def)| id.module == module && def.name == Some(needle))
            .map(|(id, _)| *id)
    }

    /// Lookup + transition in one call. `None` if no fsm registered or no rule matches.
    pub fn step_event(
        &self,
        id: &FsmId,
        current: &str,
        event: &str,
    ) -> Option<zyntax_typed_ast::InternedString> {
        self.get(id).and_then(|d| d.step_event(current, event))
    }
}

/// Live instance of a DSL-defined fsm — pairs an `FsmId` with current state name.
/// State is `InternedString` of the variant (dynamic, no compile-time enum mapping).
///
/// # Example
///
/// ```ignore
/// let mut loader = FsmInstance::new(&dsl, "main", "Loader")?;
/// loader.dispatch_event(&dsl, "Start");
/// ```
#[derive(Debug, Clone)]
pub struct FsmInstance {
    /// Identity of the fsm definition this instance follows.
    pub id: FsmId,
    /// Current state name (mutated by `dispatch_event` / `tick`).
    pub current: zyntax_typed_ast::InternedString,
}

impl FsmInstance {
    /// Create an instance starting in the fsm's declared initial state. `None` if
    /// the fsm isn't registered or has no initial state.
    pub fn new(_dsl: &BlincDsl, module: &str, fsm_name: &str) -> Option<Self> {
        let module_i = zyntax_typed_ast::InternedString::new_global(module);
        let id = with_fsm_registry(|r| r.find_by_name(module_i, fsm_name))?;
        let initial = with_fsm_registry(|r| r.get(&id).and_then(|d| d.initial))?;
        Some(Self {
            id,
            current: initial,
        })
    }

    /// Current state name as `String`.
    pub fn current(&self) -> String {
        self.current.resolve_global().unwrap_or_default()
    }

    /// Dispatch an event by name. Returns `true` if a transition fired.
    pub fn dispatch_event(&mut self, _dsl: &BlincDsl, event: &str) -> bool {
        let current_str = self.current();
        let next = with_fsm_registry(|r| r.step_event(&self.id, &current_str, event));
        if let Some(to) = next {
            self.current = to;
            true
        } else {
            false
        }
    }

    /// Tick. JIT-evaluates registered tick-guards; returns `true` if a transition fired.
    pub fn tick(&mut self, dsl: &BlincDsl) -> BlincDslResult<bool> {
        let current_str = self.current();
        let next = dsl.step_tick(&self.id, &current_str)?;
        if let Some(to) = next {
            self.current = to;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Reset to the fsm's initial state.
    pub fn reset(&mut self) {
        if let Some(initial) = with_fsm_registry(|r| r.get(&self.id).and_then(|d| d.initial)) {
            self.current = initial;
        }
    }
}

/// Process-wide fsm registry. Multiple `BlincDsl` instances share one view.
static GLOBAL_FSM_REGISTRY: std::sync::OnceLock<std::sync::Mutex<FsmRegistry>> =
    std::sync::OnceLock::new();

fn fsm_registry_lock() -> std::sync::MutexGuard<'static, FsmRegistry> {
    GLOBAL_FSM_REGISTRY
        .get_or_init(|| std::sync::Mutex::new(FsmRegistry::new()))
        .lock()
        .expect("BlincDsl global FsmRegistry mutex poisoned")
}

/// Run a closure with shared access to the global fsm registry.
pub fn with_fsm_registry<R>(f: impl FnOnce(&FsmRegistry) -> R) -> R {
    let guard = fsm_registry_lock();
    f(&guard)
}

/// Run a closure with mutable registry access. Used internally by marker builtins.
pub fn with_fsm_registry_mut<R>(f: impl FnOnce(&mut FsmRegistry) -> R) -> R {
    let mut guard = fsm_registry_lock();
    f(&mut guard)
}

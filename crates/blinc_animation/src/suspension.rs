//! Animation suspension scopes
//!
//! Tracks which animations belong to which scope (route, component).
//! When a scope is suspended, its animations stop ticking.
//! When resumed, they continue from where they left off.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::scheduler::SpringId;

/// Scope identifier
pub type ScopeId = u64;

/// Global scope registry
static SCOPES: Mutex<Option<ScopeRegistry>> = Mutex::new(None);

struct ScopeRegistry {
    /// Springs registered to each scope
    spring_scopes: HashMap<ScopeId, Vec<SpringId>>,
    /// The currently active scope (springs created go here)
    active_scope: Option<ScopeId>,
    /// Next scope ID
    next_id: u64,
}

impl ScopeRegistry {
    fn new() -> Self {
        Self {
            spring_scopes: HashMap::new(),
            active_scope: None,
            next_id: 1,
        }
    }
}

fn with_registry<R>(f: impl FnOnce(&mut ScopeRegistry) -> R) -> R {
    let mut guard = SCOPES.lock().unwrap();
    let registry = guard.get_or_insert_with(ScopeRegistry::new);
    f(registry)
}

/// Create a new suspension scope and return its ID
pub fn create_scope() -> ScopeId {
    with_registry(|r| {
        let id = r.next_id;
        r.next_id += 1;
        r.spring_scopes.insert(id, Vec::new());
        id
    })
}

/// Set the active scope — new springs will be registered to this scope
pub fn enter_scope(scope: ScopeId) {
    with_registry(|r| {
        r.active_scope = Some(scope);
    });
}

/// Clear the active scope
pub fn exit_scope() {
    with_registry(|r| {
        r.active_scope = None;
    });
}

/// Get the currently active scope (called by AnimatedValue when registering a spring)
pub fn current_scope() -> Option<ScopeId> {
    with_registry(|r| r.active_scope)
}

/// Register a spring to the current scope
pub fn register_spring(spring_id: SpringId) {
    with_registry(|r| {
        if let Some(scope) = r.active_scope {
            r.spring_scopes.entry(scope).or_default().push(spring_id);
        }
    });
}

/// Suspend all animations in a scope
pub fn suspend_scope(scope: ScopeId, handle: &crate::SchedulerHandle) {
    let springs = with_registry(|r| r.spring_scopes.get(&scope).cloned().unwrap_or_default());
    for id in springs {
        handle.pause_spring(id);
    }
}

/// Resume all animations in a scope
pub fn resume_scope(scope: ScopeId, handle: &crate::SchedulerHandle) {
    let springs = with_registry(|r| r.spring_scopes.get(&scope).cloned().unwrap_or_default());
    for id in springs {
        handle.resume_spring(id);
    }
}

/// Remove a scope and unregister its springs
pub fn remove_scope(scope: ScopeId) {
    with_registry(|r| {
        r.spring_scopes.remove(&scope);
    });
}

/// Unregister a spring from whatever scope it belongs to (called on AnimatedValue drop)
pub fn unregister_spring(spring_id: SpringId) {
    with_registry(|r| {
        for springs in r.spring_scopes.values_mut() {
            springs.retain(|&id| id != spring_id);
        }
    });
}

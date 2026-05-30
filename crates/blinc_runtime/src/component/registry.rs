//! Process-wide component registry. Mirror of
//! [`crate::fsm::registry::FsmRegistry`] — same singleton +
//! accessor shape, different payload.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use super::definition::ComponentDefinition;

/// Opaque process-wide component identifier. Minted in
/// registration order; stable across the process lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ComponentId(pub u32);

/// All registered components. Lookup is by id (fast) or by
/// user-facing name (a name-index sits alongside the id-keyed
/// map so the slow-path-by-name stays O(1) without iterating).
#[derive(Debug, Default)]
pub struct ComponentRegistry {
    defs: HashMap<ComponentId, ComponentDefinition>,
    name_index: HashMap<Arc<str>, ComponentId>,
    next_id: u32,
}

impl ComponentRegistry {
    /// Empty registry. Embedders normally interact with the
    /// process-wide singleton via [`with_component_registry`]
    /// / [`with_component_registry_mut`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) a component definition. Returns the
    /// assigned [`ComponentId`]. Replaces any prior entry with
    /// the same name — same hot-reload-friendly semantics as
    /// [`crate::fsm::FsmRegistry::register`].
    pub fn register(&mut self, def: ComponentDefinition) -> ComponentId {
        if let Some(&existing) = self.name_index.get(&def.name) {
            self.defs.insert(existing, def);
            return existing;
        }
        let id = ComponentId(self.next_id);
        self.next_id += 1;
        self.name_index.insert(def.name.clone(), id);
        self.defs.insert(id, def);
        id
    }

    /// Look up by id.
    pub fn get(&self, id: ComponentId) -> Option<&ComponentDefinition> {
        self.defs.get(&id)
    }

    /// Resolve a user-facing component name to its id.
    pub fn id_of(&self, name: &str) -> Option<ComponentId> {
        self.name_index.get(name).copied()
    }

    /// Look up by name. Convenience wrapper over `id_of` +
    /// `get`; the common "do I have a definition for X?" shape.
    pub fn get_by_name(&self, name: &str) -> Option<&ComponentDefinition> {
        let id = self.id_of(name)?;
        self.get(id)
    }

    /// Iterate over all `(id, definition)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (ComponentId, &ComponentDefinition)> {
        self.defs.iter().map(|(id, def)| (*id, def))
    }

    /// Total registered components.
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

static GLOBAL_COMPONENT_REGISTRY: OnceLock<Mutex<ComponentRegistry>> = OnceLock::new();

fn lock() -> std::sync::MutexGuard<'static, ComponentRegistry> {
    GLOBAL_COMPONENT_REGISTRY
        .get_or_init(|| Mutex::new(ComponentRegistry::new()))
        .lock()
        .expect("blinc_runtime::component::ComponentRegistry mutex poisoned")
}

/// Read-only access to the global component registry.
pub fn with_component_registry<R>(f: impl FnOnce(&ComponentRegistry) -> R) -> R {
    let guard = lock();
    f(&guard)
}

/// Mutable access to the global component registry. Used by
/// JIT / AOT publishers when registering freshly-compiled
/// components at startup.
pub fn with_component_registry_mut<R>(f: impl FnOnce(&mut ComponentRegistry) -> R) -> R {
    let mut guard = lock();
    f(&mut guard)
}

#[cfg(test)]
mod tests {
    use super::super::definition::{PropDef, Type};
    use super::*;
    use zyntax_typed_ast::type_registry::PrimitiveType;

    fn arc(s: &str) -> Arc<str> {
        Arc::from(s)
    }

    fn counter() -> ComponentDefinition {
        ComponentDefinition {
            name: arc("Counter"),
            view_symbol: arc("Counter$view"),
            props: vec![PropDef {
                name: arc("initial"),
                ty: Type::Primitive(PrimitiveType::I32),
                reactive_inner: None,
            }],
        }
    }

    /// Registration assigns fresh ids; re-registering by name
    /// reuses the existing id (hot-reload semantics).
    #[test]
    fn register_assigns_ids_and_dedupes_by_name() {
        let mut r = ComponentRegistry::new();
        let id_a = r.register(counter());
        let id_b = r.register(ComponentDefinition {
            name: arc("Other"),
            view_symbol: arc("Other$view"),
            props: vec![],
        });
        assert_ne!(id_a, id_b);

        // Re-register Counter with a different prop shape.
        let updated = ComponentDefinition {
            name: arc("Counter"),
            view_symbol: arc("Counter$view"),
            props: vec![
                PropDef {
                    name: arc("initial"),
                    ty: Type::Primitive(PrimitiveType::I32),
                    reactive_inner: None,
                },
                PropDef {
                    name: arc("step"),
                    ty: Type::Primitive(PrimitiveType::I32),
                    reactive_inner: None,
                },
            ],
        };
        let id_a_again = r.register(updated);
        assert_eq!(id_a, id_a_again);
        // Replaced — Counter now has 2 props, not 1.
        assert_eq!(r.get(id_a).unwrap().prop_count(), 2);
    }

    /// Name lookup round-trips both via the dedicated method
    /// and via `id_of` + `get`.
    #[test]
    fn name_lookup_round_trip() {
        let mut r = ComponentRegistry::new();
        let id = r.register(counter());

        let by_name = r.get_by_name("Counter").unwrap();
        assert_eq!(by_name.view_symbol.as_ref(), "Counter$view");

        let by_id = r.get(r.id_of("Counter").unwrap()).unwrap();
        assert_eq!(by_id.name.as_ref(), "Counter");

        assert!(r.id_of("Missing").is_none());
        assert!(r.get_by_name("Missing").is_none());
        // get out-of-range id returns None
        assert!(r.get(ComponentId(99)).is_none());
        let _ = id; // suppress unused
    }
}

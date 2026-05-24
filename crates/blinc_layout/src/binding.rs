//! Signal-bound property binding registry — Phase 2 of the unified
//! property channel ([[project-reactive-architecture-v2]]).
//!
//! This is the substrate for `.bg(&state)`-style call sites: when a
//! signal value changes, the registry walks subscribers and queues
//! partial property updates via the same channel CSS animations,
//! transitions, and stateful refreshes already use ([`PropertyId`] +
//! [`SideEffects`] + the closure-form `queue_prop_update_partial`).
//!
//! # Architecture
//!
//! The registry lives in `blinc_layout` because it needs `LayoutNodeId`
//! and the queue API; `blinc_core` exposes a [global notifier hook][1]
//! that fires on every `State<T>::set`. The registry installs itself as
//! that notifier on first access — no init order coupling for platform
//! runners.
//!
//! [1]: blinc_core::reactive::set_property_binding_notifier
//!
//! # Lifecycle
//!
//! - **Register**: `Div::build` calls [`PropertyBindingRegistry::register`]
//!   for every reactive modifier on the element. The registration carries
//!   the target `LayoutNodeId`, the `PropertyId`, the `State<T>` (cloned)
//!   for current-value reads, and a closure that knows how to write the
//!   value into the right `RenderProps` field.
//! - **Fire**: `State<T>::set` triggers the global notifier → registry
//!   walks subscribers for that signal id → each subscriber reads the
//!   latest value and pushes a [`PartialPropertyUpdate`][crate::stateful::PartialPropertyUpdate]
//!   onto the unified queue. Platform runners drain the queue next frame.
//! - **Cleanup**: `remove_subtree_nodes` calls
//!   [`PropertyBindingRegistry::unregister_node`] for every removed
//!   layout id. Bindings for nodes that no longer exist are evicted, so
//!   stale subscribers can't fire on the next signal change.
//!
//! # API (this is the foundation — builder integration lands in P2.2)
//!
//! Phase 2.1 ships the registry + `IntoReactive` trait. Phase 2.2 wires
//! the trait into `Div` builder methods so `.bg(&state)` actually
//! registers. Phase 2.3+ rolls it across visual / layout properties.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use blinc_core::reactive::SignalId;

use crate::element::RenderProps;
use crate::property::{PropertyId, SideEffects};
use crate::stateful::queue_prop_update_partial;
use crate::tree::LayoutNodeId;

/// Reads the current value out of a signal's reactive graph and packages
/// it in a [`BoundValue`]. Re-invoked on every fire (not cached).
pub type ReadFn = Arc<dyn Fn() -> Option<BoundValue> + Send + Sync>;

/// Writes a [`BoundValue`] into the right `RenderProps` field. Built
/// once at `Bound` construction; reused on every fire.
pub type WriteFn = Arc<dyn Fn(&mut RenderProps, &BoundValue) + Send + Sync>;

/// A reactive value source — what a `IntoReactive<T>` impl resolves to.
///
/// Builder methods examine this variant: `Const` becomes an immediate
/// `RenderProps` write at build time; `Bound` schedules a registry
/// registration after the node's `LayoutNodeId` is minted.
pub enum Reactive<T> {
    /// Eager value — no subscription, written directly into the element
    /// state at build time.
    Const(T),
    /// Signal-bound — subscribe to the signal at build time, write the
    /// value via the supplied closure on every signal change.
    ///
    /// The closure takes `&mut RenderProps` and the new value, and is
    /// what gets handed to [`queue_prop_update_partial`] when the signal
    /// fires. Type-erased so the registry can store heterogeneous
    /// subscribers in one collection.
    Bound {
        /// The reactive source the binding subscribes to.
        signal_id: SignalId,
        /// Reads the current value from the reactive graph.
        read: ReadFn,
        /// Writes a [`BoundValue`] into the right `RenderProps` field.
        write: WriteFn,
    },
}

/// Type-erased value carrier for reactive bindings.
///
/// `Reactive::Bound` stores the read + write closures as type-erased
/// (`Arc<dyn Fn>`); they shuttle values through this `Any`-backed
/// wrapper rather than the concrete `T`. The downcast happens inside
/// the `write` closure where the concrete type is known at the original
/// `IntoReactive` construction site.
pub struct BoundValue(pub Box<dyn std::any::Any + Send + Sync>);

impl BoundValue {
    pub fn new<T: Send + Sync + 'static>(v: T) -> Self {
        Self(Box::new(v))
    }

    /// Downcast to a reference of the inner type. Returns `None` on
    /// type mismatch (programmer error — would mean a binding's
    /// write closure doesn't match its read closure's type).
    pub fn downcast_ref<T: 'static>(&self) -> Option<&T> {
        self.0.downcast_ref::<T>()
    }
}

/// Trait that lets `Div` builder methods accept both eager `T` values
/// and signal-bound `&State<T>` references at the same call site.
///
/// Implemented in this crate for `T` (eager) and for `&State<T>`
/// (signal-bound) — the two impls coexist via blanket-less specialisation
/// (each impl targets a distinct type, no overlap).
///
/// # Example (post-P2.2 surface)
///
/// ```ignore
/// // Eager — compiles to direct RenderProps write at build time
/// div().bg(Color::RED)
///
/// // Signal-bound — registers a subscription; fires on state.set()
/// let bg = State::new(...);
/// div().bg(&bg)
/// ```
pub trait IntoReactive<T> {
    fn into_reactive(self) -> Reactive<T>;
}

// Eager: any value of T is a Const reactive.
impl<T> IntoReactive<T> for T {
    fn into_reactive(self) -> Reactive<T> {
        Reactive::Const(self)
    }
}

// =========================================================================
// Registry
// =========================================================================

struct Subscriber {
    node_id: LayoutNodeId,
    property: PropertyId,
    read: ReadFn,
    write: WriteFn,
}

/// Process-global registry of signal-bound property subscribers.
pub struct PropertyBindingRegistry {
    /// `signal_id → list of subscribers waiting on that signal`.
    bindings: HashMap<SignalId, Vec<Subscriber>>,
    /// `node_id → list of signal_ids that node subscribed to`.
    /// Used by `unregister_node` to evict in O(per-binding-on-that-node)
    /// time instead of scanning every signal's subscriber list.
    by_node: HashMap<LayoutNodeId, Vec<SignalId>>,
}

impl PropertyBindingRegistry {
    fn new() -> Self {
        Self {
            bindings: HashMap::new(),
            by_node: HashMap::new(),
        }
    }

    /// Register a signal-bound subscription. Called by `Div::build` (or
    /// the equivalent collection pass) for every reactive modifier on
    /// the element, after the `LayoutNodeId` has been minted.
    ///
    /// `read` reads the current value from the signal's reactive graph;
    /// `write` writes that value into the right `RenderProps` field.
    /// Both are stored as `Arc<dyn Fn>` so the registry can hold
    /// heterogeneous subscribers in one table.
    pub fn register(
        &mut self,
        signal_id: SignalId,
        node_id: LayoutNodeId,
        property: PropertyId,
        read: ReadFn,
        write: WriteFn,
    ) {
        self.bindings.entry(signal_id).or_default().push(Subscriber {
            node_id,
            property,
            read,
            write,
        });
        self.by_node.entry(node_id).or_default().push(signal_id);
    }

    /// Evict every subscription belonging to the given node. Called by
    /// `remove_subtree_nodes` so stale subscribers can't fire on the
    /// next signal change after a structural rebuild dropped the node.
    pub fn unregister_node(&mut self, node_id: LayoutNodeId) {
        let Some(signal_ids) = self.by_node.remove(&node_id) else {
            return;
        };
        for sig_id in signal_ids {
            if let Some(subs) = self.bindings.get_mut(&sig_id) {
                subs.retain(|s| s.node_id != node_id);
                if subs.is_empty() {
                    self.bindings.remove(&sig_id);
                }
            }
        }
    }

    /// Walk every subscriber waiting on `signal_id` and queue a partial
    /// property update for each. Called by the global notifier from
    /// `blinc_core::reactive::set_property_binding_notifier` whenever a
    /// `State<T>::set` (or `update`) fires.
    pub fn fire(&self, signal_id: SignalId) {
        let Some(subs) = self.bindings.get(&signal_id) else {
            return;
        };
        for sub in subs {
            let Some(value) = (sub.read)() else {
                continue;
            };
            let write = Arc::clone(&sub.write);
            queue_prop_update_partial(
                sub.node_id,
                sub.property,
                sub.property.side_effects(),
                move |props| write(props, &value),
            );
        }
    }

    /// Number of unique signals currently subscribed to. For diagnostics
    /// / tests.
    pub fn signal_count(&self) -> usize {
        self.bindings.len()
    }

    /// Number of subscribers for `signal_id`. For diagnostics / tests.
    pub fn subscriber_count(&self, signal_id: SignalId) -> usize {
        self.bindings.get(&signal_id).map(|v| v.len()).unwrap_or(0)
    }

    /// Drop every binding. Test-only: each test creates a fresh
    /// `ReactiveGraph` whose signal-id counter restarts at 0, so test
    /// runs share signal ids and would observe each other's subscribers
    /// without an explicit reset.
    #[cfg(test)]
    pub fn clear_for_tests(&mut self) {
        self.bindings.clear();
        self.by_node.clear();
    }
}

/// Process-global registry instance. The first call to `global()`
/// installs the notifier hook in `blinc_core::reactive` — subsequent
/// `State<T>::set` calls feed into `fire` automatically.
#[allow(clippy::incompatible_msrv)]
static REGISTRY: LazyLock<Mutex<PropertyBindingRegistry>> = LazyLock::new(|| {
    blinc_core::reactive::set_property_binding_notifier(|signal_id| {
        // Lock the registry on each fire; brief — we drain to the
        // partial-update queue and release. Drain happens on a different
        // thread (platform runner main thread) so contention is minimal.
        if let Ok(reg) = REGISTRY.lock() {
            reg.fire(signal_id);
        }
    });
    Mutex::new(PropertyBindingRegistry::new())
});

/// Access the global registry. Lazy-initialises + installs the core
/// notifier hook on first call.
pub fn with_registry<R>(f: impl FnOnce(&mut PropertyBindingRegistry) -> R) -> R {
    let mut reg = REGISTRY.lock().unwrap();
    f(&mut reg)
}

/// Convenience: register a signal-bound subscription using a typed
/// `State<T>` + a typed writer. Wraps the boilerplate of constructing
/// the type-erased `read` + `write` closures.
///
/// This is the API `Div` builder methods will call internally in P2.2+.
pub fn register_typed<T>(
    signal_id: SignalId,
    node_id: LayoutNodeId,
    property: PropertyId,
    state: blinc_core::reactive::State<T>,
    write: impl Fn(&mut RenderProps, T) + Send + Sync + 'static,
) where
    T: Clone + Send + Sync + 'static,
{
    let write = Arc::new(write);
    let state_for_read = state.clone();
    let read: ReadFn = Arc::new(move || state_for_read.try_get().map(BoundValue::new));
    let write_dyn: WriteFn = {
        let write = Arc::clone(&write);
        Arc::new(move |props: &mut RenderProps, val: &BoundValue| {
            if let Some(v) = val.downcast_ref::<T>() {
                write(props, v.clone());
            }
        })
    };
    with_registry(|reg| reg.register(signal_id, node_id, property, read, write_dyn));
}

/// Convenience: unregister all bindings for a node. Called by
/// `remove_subtree_nodes` in the renderer cleanup path.
pub fn unregister_node(node_id: LayoutNodeId) {
    with_registry(|reg| reg.unregister_node(node_id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use blinc_core::reactive::{ReactiveGraph, State};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Serialise the binding tests — they share the process-global
    /// registry, and each `fresh_state` call rebuilds a `ReactiveGraph`
    /// whose signal-id counter restarts at 0. Without serialisation,
    /// two parallel tests both register subscribers for `SignalId(0)`
    /// and one test's `set()` fires the other's binding.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Acquire the lock + reset the registry to a clean state. Returns
    /// a guard so the lock is held for the test body's duration.
    fn lock_and_reset() -> std::sync::MutexGuard<'static, ()> {
        // Poisoning on a previous test's panic shouldn't kill subsequent
        // tests — recover from the poison so the suite keeps running.
        let guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        with_registry(|r| r.clear_for_tests());
        let _ = crate::stateful::take_pending_partial_prop_updates();
        guard
    }

    /// Build a fresh `State<T>` over a private ReactiveGraph. Avoids
    /// the BlincContextState global init (which is one-shot and can't
    /// be reused across tests). Also touches the registry so the
    /// LazyLock fires + installs the core notifier before the test
    /// interacts with `State::set`.
    fn fresh_state<T: Clone + Send + 'static>(initial: T) -> State<T> {
        with_registry(|_| {});
        let graph: Arc<Mutex<ReactiveGraph>> = Arc::new(Mutex::new(ReactiveGraph::new()));
        let signal = graph.lock().unwrap().create_signal(initial);
        let dirty: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
        State::new(signal, graph, dirty)
    }

    fn mint_node(tree: &mut crate::tree::LayoutTree) -> LayoutNodeId {
        tree.create_node(taffy::Style::default())
    }

    #[test]
    fn register_then_fire_queues_partial_update() {
        let _guard = lock_and_reset();
        let state = fresh_state::<i32>(0);

        let mut tree = crate::tree::LayoutTree::new();
        let node_id = mint_node(&mut tree);

        let fire_count = Arc::new(AtomicUsize::new(0));
        let fire_count_for_write = Arc::clone(&fire_count);
        register_typed(
            state.signal_id(),
            node_id,
            PropertyId::Background,
            state.clone(),
            move |_props, _v: i32| {
                fire_count_for_write.fetch_add(1, Ordering::SeqCst);
            },
        );

        // Set the signal — should fire one binding → one queued update.
        state.set(42);

        let updates = crate::stateful::take_pending_partial_prop_updates();
        assert_eq!(updates.len(), 1, "exactly one partial update queued");
        assert_eq!(updates[0].node_id, node_id);
        assert_eq!(updates[0].property, PropertyId::Background);

        // Apply it to a RenderProps to invoke the writer.
        let mut props = RenderProps::default();
        (updates.into_iter().next().unwrap().write)(&mut props);
        assert_eq!(fire_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unregister_node_stops_firing() {
        let _guard = lock_and_reset();
        let state = fresh_state::<i32>(0);

        let mut tree = crate::tree::LayoutTree::new();
        let node_id = mint_node(&mut tree);

        register_typed(
            state.signal_id(),
            node_id,
            PropertyId::Opacity,
            state.clone(),
            |_p, _v: i32| {},
        );

        // Sanity: registered.
        assert_eq!(with_registry(|r| r.subscriber_count(state.signal_id())), 1);

        unregister_node(node_id);

        // After unregister: no subscribers, fire is a no-op.
        assert_eq!(with_registry(|r| r.subscriber_count(state.signal_id())), 0);

        state.set(99);
        let updates = crate::stateful::take_pending_partial_prop_updates();
        assert!(
            updates.is_empty(),
            "unregistered binding must not fire (got {} updates)",
            updates.len()
        );
    }

    #[test]
    fn multiple_subscribers_one_signal() {
        let _guard = lock_and_reset();
        let state = fresh_state::<i32>(0);

        let mut tree = crate::tree::LayoutTree::new();
        let n1 = mint_node(&mut tree);
        let n2 = mint_node(&mut tree);
        let n3 = mint_node(&mut tree);

        for nid in [n1, n2, n3] {
            register_typed(
                state.signal_id(),
                nid,
                PropertyId::Background,
                state.clone(),
                |_p, _v: i32| {},
            );
        }

        state.set(7);
        let updates = crate::stateful::take_pending_partial_prop_updates();
        assert_eq!(updates.len(), 3, "all three subscribers fired");
        assert!(updates.iter().any(|u| u.node_id == n1));
        assert!(updates.iter().any(|u| u.node_id == n2));
        assert!(updates.iter().any(|u| u.node_id == n3));
    }

    #[test]
    fn unregister_removes_only_targeted_node() {
        let _guard = lock_and_reset();
        let state = fresh_state::<i32>(0);

        let mut tree = crate::tree::LayoutTree::new();
        let keep = mint_node(&mut tree);
        let drop_node = mint_node(&mut tree);

        register_typed(
            state.signal_id(),
            keep,
            PropertyId::Background,
            state.clone(),
            |_p, _v: i32| {},
        );
        register_typed(
            state.signal_id(),
            drop_node,
            PropertyId::Background,
            state.clone(),
            |_p, _v: i32| {},
        );

        unregister_node(drop_node);

        state.set(1);
        let updates = crate::stateful::take_pending_partial_prop_updates();
        assert_eq!(updates.len(), 1, "only the surviving subscriber fired");
        assert_eq!(updates[0].node_id, keep);
    }

    #[test]
    fn into_reactive_const_path() {
        // No registry interaction; doesn't need the lock.
        let r = 42_i32.into_reactive();
        match r {
            Reactive::Const(v) => assert_eq!(v, 42),
            Reactive::Bound { .. } => panic!("expected Const"),
        }
    }
}

//! Per-thread default `FsmInstance` substrate, backed by
//! `blinc_layout::stateful::SharedState<FsmStateId>`.
//!
//! Storage is the exact widget-state cell `Stateful<S>` already
//! consumes — `Arc<Mutex<StatefulInner<FsmStateId>>>`. Widgets
//! that want to follow the FSM call
//! `Stateful::<FsmStateId>::with_shared_state(default_state("Foo"))`
//! and read state through normal `Stateful` callbacks. The DSL
//! `Div(on_click = "Foo.Event")` extern dispatches against the
//! same cell. No parallel substrate layer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use blinc_layout::stateful::{request_redraw, use_fsm_keyed, SharedState};

use super::instance::FsmStateId;
use super::registry::with_fsm_registry;

pub use super::registry::TransitionAction;

fn state_key(fsm_name: &str) -> String {
    format!("__fsm:state:{fsm_name}")
}

/// `SharedState<FsmStateId>` for `fsm_name`'s default instance.
/// `None` if `BlincContextState` isn't initialised or the FSM
/// isn't registered.
///
/// Pass the returned handle to
/// `Stateful::<FsmStateId>::with_shared_state(...)` to bind a
/// widget to this FSM's current state — the widget's
/// `on_state` callback re-runs whenever the state advances.
pub fn default_state(fsm_name: &str) -> Option<SharedState<FsmStateId>> {
    if !blinc_core::context_state::BlincContextState::is_initialized() {
        return None;
    }
    let initial = FsmStateId::from_fsm_name(fsm_name)?;
    Some(use_fsm_keyed::<_, FsmStateId>(
        &state_key(fsm_name),
        initial,
    ))
}

/// Current state name. Reads through the `SharedState` when
/// present, otherwise resolves through the fallback code table
/// + the registry's name lookup.
pub fn current_state_name(fsm_name: &str) -> Option<Arc<str>> {
    if let Some(shared) = default_state(fsm_name) {
        return shared
            .lock()
            .ok()
            .and_then(|inner| inner.state.state_name());
    }
    let code = current_state_code(fsm_name)?;
    with_fsm_registry(|r| {
        let id = r.id_of(fsm_name)?;
        r.get(id)?.state_name(code).cloned()
    })
}

/// Current state code. Same fallback shape as
/// [`current_state_name`].
pub fn current_state_code(fsm_name: &str) -> Option<u32> {
    if let Some(shared) = default_state(fsm_name) {
        return shared.lock().ok().map(|inner| inner.state.variant);
    }
    FALLBACK_CODES.with(|m| {
        if let Some(&c) = m.borrow().get(fsm_name) {
            return Some(c);
        }
        let initial = with_fsm_registry(|r| {
            r.id_of(fsm_name)
                .and_then(|id| r.get(id))
                .map(|d| d.initial_code)
        })?;
        m.borrow_mut().insert(Arc::from(fsm_name), initial);
        Some(initial)
    })
}

/// Reset to the registered initial state. No-op if the FSM
/// isn't registered. Transition actions / effects do NOT fire
/// — reset is a host operation.
pub fn reset_default(fsm_name: &str) -> Option<Arc<str>> {
    let initial = FsmStateId::from_fsm_name(fsm_name)?;
    let name = initial.state_name()?;
    if let Some(shared) = default_state(fsm_name) {
        if let Ok(mut inner) = shared.lock() {
            inner.state = initial;
        }
        request_redraw();
    } else {
        FALLBACK_CODES.with(|m| {
            m.borrow_mut().insert(Arc::from(fsm_name), initial.variant);
        });
    }
    Some(name)
}

/// Dispatch `event_name` against `fsm_name`'s default instance.
///
/// Returns `Some((from_name, to_name))` when a registered
/// transition fires. On success the `SharedState` advances
/// (and any bound `Stateful<FsmStateId>` widget gets refreshed
/// on the next frame via `needs_visual_update` +
/// `request_redraw`), every transition action runs (currently
/// `i32` signal writes), and every callback registered via
/// [`register_transition_effect`] fires in order.
pub fn dispatch_default(fsm_name: &str, event_name: &str) -> Option<(Arc<str>, Arc<str>)> {
    let event_code = with_fsm_registry(|r| {
        let id = r.id_of(fsm_name)?;
        r.get(id)?.event_code(event_name)
    })?;

    let (from_name, to_name, actions) = if let Some(shared) = default_state(fsm_name) {
        let (from_state, to_state, actions) = {
            let mut inner = shared.lock().ok()?;
            let from = inner.state;
            let actions = with_fsm_registry(|r| {
                let def = r.get(from.fsm_id)?;
                def.transitions
                    .iter()
                    .find(|t| t.from_code == from.variant && t.event_code == event_code)
                    .map(|t| t.actions.clone())
            })
            .unwrap_or_default();
            if !inner.dispatch(event_code) {
                return None;
            }
            (from, inner.state, actions)
        };
        request_redraw();
        let from_name = from_state.state_name()?;
        let to_name = to_state.state_name()?;
        (from_name, to_name, actions)
    } else {
        let current_code = current_state_code(fsm_name)?;
        let (from_name, to_name, next_code, actions) = with_fsm_registry(|r| {
            let id = r.id_of(fsm_name)?;
            let def = r.get(id)?;
            let transition = def
                .transitions
                .iter()
                .find(|t| t.from_code == current_code && t.event_code == event_code)?;
            let from = def.state_name(transition.from_code).cloned()?;
            let to = def.state_name(transition.to_code).cloned()?;
            Some((from, to, transition.to_code, transition.actions.clone()))
        })?;
        FALLBACK_CODES.with(|m| {
            m.borrow_mut().insert(Arc::from(fsm_name), next_code);
        });
        (from_name, to_name, actions)
    };

    for action in &actions {
        execute_action(action);
    }
    EFFECTS.with(|e| {
        if let Some(list) = e.borrow().get(fsm_name) {
            for cb in list {
                cb(&from_name, event_name, &to_name);
            }
        }
    });
    let triggered_path = format!("{from_name}.{event_name}");
    let matched: Vec<Arc<TransitionSubscriber>> = SUBSCRIBERS.with(|s| {
        s.borrow()
            .get(fsm_name)
            .map(|v| {
                v.iter()
                    .filter(|(p, _)| p.as_str() == triggered_path.as_str())
                    .map(|(_, cb)| cb.clone())
                    .collect()
            })
            .unwrap_or_default()
    });
    for cb in matched {
        cb();
    }

    Some((from_name, to_name))
}

fn execute_action(action: &TransitionAction) {
    match action {
        TransitionAction::SetI32 { signal, value } => {
            crate::signal::set_i32(signal, *value);
        }
        TransitionAction::AddI32 { signal, delta } => {
            let current = crate::signal::get_i32_or_default(signal);
            crate::signal::set_i32(signal, current + delta);
        }
    }
}

thread_local! {
    /// Per-thread fallback state code, used when
    /// `BlincContextState` isn't initialised.
    static FALLBACK_CODES: RefCell<HashMap<Arc<str>, u32>> = RefCell::new(HashMap::new());
    static EFFECTS: RefCell<HashMap<String, Vec<TransitionEffect>>> = RefCell::new(HashMap::new());
    /// Per-FSM list of `(path, callback)` subscribers registered
    /// from DSL `init { … }` blocks via
    /// `<Fsm>.subscribe("From.Event", || { … })`. Each callback
    /// fires after a successful default-instance transition whose
    /// `"From.Event"` path equals the registered filter. The
    /// closure is wrapped in `Arc` so [`dispatch_default`] can snapshot
    /// the matching subset without holding the borrow across user
    /// code.
    static SUBSCRIBERS: RefCell<SubscriberMap> = RefCell::new(HashMap::new());
}

/// Callback signature for host-side effects registered via
/// [`register_transition_effect`]. Args are
/// `(from_state, event, to_state)`.
pub type TransitionEffect = Box<dyn Fn(&str, &str, &str) + 'static>;

/// Callback signature for DSL-registered FSM subscribers. The
/// closure takes no args — `register_subscriber` already filters
/// on the registered `"From.Event"` path, so by the time the
/// callback fires the path is known to match. Registered from a
/// DSL `init { ... }` block via
/// `<Fsm>.subscribe("From.Event", || { … })` — see
/// [`register_subscriber`].
pub type TransitionSubscriber = dyn Fn() + 'static;

/// Per-thread subscriber table: FSM name → list of
/// `(path filter, callback)` pairs. Aliased so the `thread_local!`
/// + lookup sites don't repeat the deeply-nested generics.
type SubscriberMap = HashMap<String, Vec<(String, Arc<TransitionSubscriber>)>>;

/// Register a DSL-side subscriber that fires after each successful
/// default-instance transition whose triggered `"From.Event"` path
/// equals `path`. Distinct from [`register_transition_effect`]:
/// subscribers are path-filtered and zero-arg, intended to be
/// driven by `__fsm_subscribe__` host-extern calls emitted from
/// DSL `init { ... }` blocks. Effects are intended for host-side
/// integrations and receive the full `(from, event, to)` triple.
pub fn register_subscriber(fsm_name: &str, path: &str, cb: impl Fn() + 'static) {
    SUBSCRIBERS.with(|s| {
        s.borrow_mut()
            .entry(fsm_name.to_string())
            .or_default()
            .push((path.to_string(), Arc::new(cb)));
    });
}

/// Register a callback to run after each successful default-
/// instance transition for `fsm_name`. Reserved for side
/// effects that can't be expressed in DSL (logging, network,
/// etc.) — signal writes belong in transition actions.
pub fn register_transition_effect(fsm_name: &str, cb: impl Fn(&str, &str, &str) + 'static) {
    EFFECTS.with(|e| {
        e.borrow_mut()
            .entry(fsm_name.to_string())
            .or_default()
            .push(Box::new(cb));
    });
}

/// Test helper.
pub fn clear_all() {
    FALLBACK_CODES.with(|m| m.borrow_mut().clear());
    EFFECTS.with(|e| e.borrow_mut().clear());
    SUBSCRIBERS.with(|s| s.borrow_mut().clear());
}

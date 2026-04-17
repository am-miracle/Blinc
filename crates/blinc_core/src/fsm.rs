//! State Machine Runtime
//!
//! Implementation of Harel statecharts for widget interaction states.
//! Supports:
//! - Flat state machines
//! - Guards (conditional transitions)
//! - Entry/exit actions
//! - Transition actions
//!
//! Future: hierarchical and parallel states.
//!
//! # Not to be confused with `blinc_skeleton::fsm`
//!
//! Blinc ships a second, unrelated state-machine implementation in
//! `blinc_skeleton::fsm` — used for skeletal animation state graphs
//! (idle → walk → run → attack, with crossfade transitions). That
//! machine is time-driven (`tick(dt)`), conditions are polled each
//! frame against named parameters (`set_bool("moving", true)`), and
//! transitions blend two poses over a configurable duration.
//!
//! This machine is **event-driven** (`send(event_id)`),
//! instantaneous (no transition duration), and leans on user
//! `FnMut()` callbacks for entry / exit / action work. Reach for
//! this one on widget interaction; reach for `blinc_skeleton::fsm`
//! on character animation.

use rustc_hash::FxHashMap;
use slotmap::{new_key_type, SlotMap};
use smallvec::SmallVec;

new_key_type! {
    /// Unique identifier for a state machine instance
    pub struct FsmId;
}

/// Identifier for a state within a state machine
pub type StateId = u32;

/// Identifier for an event type
pub type EventId = u32;

/// A guard function that determines if a transition should occur
pub type Guard = Box<dyn Fn() -> bool + Send>;

/// An action function executed during transitions
pub type Action = Box<dyn FnMut() + Send>;

/// A transition in the state machine
pub struct Transition {
    pub from_state: StateId,
    pub event: EventId,
    pub to_state: StateId,
    pub guard: Option<Guard>,
    pub actions: SmallVec<[Action; 2]>,
}

impl Transition {
    /// Create a simple transition without guard or actions
    pub fn new(from: StateId, event: EventId, to: StateId) -> Self {
        Self {
            from_state: from,
            event,
            to_state: to,
            guard: None,
            actions: SmallVec::new(),
        }
    }

    /// Add a guard condition
    pub fn with_guard<F: Fn() -> bool + Send + 'static>(mut self, guard: F) -> Self {
        self.guard = Some(Box::new(guard));
        self
    }

    /// Add an action to execute during transition
    pub fn with_action<F: FnMut() + Send + 'static>(mut self, action: F) -> Self {
        self.actions.push(Box::new(action));
        self
    }
}

/// Builder for creating state machines
pub struct StateMachineBuilder {
    initial_state: StateId,
    transitions: Vec<Transition>,
    entry_callbacks: FxHashMap<StateId, Vec<Action>>,
    exit_callbacks: FxHashMap<StateId, Vec<Action>>,
}

impl StateMachineBuilder {
    pub fn new(initial_state: StateId) -> Self {
        Self {
            initial_state,
            transitions: Vec::new(),
            entry_callbacks: FxHashMap::default(),
            exit_callbacks: FxHashMap::default(),
        }
    }

    /// Add a transition
    pub fn transition(mut self, transition: Transition) -> Self {
        self.transitions.push(transition);
        self
    }

    /// Add a simple transition (from, event, to)
    pub fn on(mut self, from: StateId, event: EventId, to: StateId) -> Self {
        self.transitions.push(Transition::new(from, event, to));
        self
    }

    /// Add an entry action for a state
    pub fn on_enter<F: FnMut() + Send + 'static>(mut self, state: StateId, action: F) -> Self {
        self.entry_callbacks
            .entry(state)
            .or_default()
            .push(Box::new(action));
        self
    }

    /// Add an exit action for a state
    pub fn on_exit<F: FnMut() + Send + 'static>(mut self, state: StateId, action: F) -> Self {
        self.exit_callbacks
            .entry(state)
            .or_default()
            .push(Box::new(action));
        self
    }

    /// Build the state machine
    pub fn build(self) -> StateMachine {
        StateMachine {
            current_state: self.initial_state,
            transitions: self.transitions,
            entry_callbacks: self.entry_callbacks,
            exit_callbacks: self.exit_callbacks,
            history: Vec::new(),
        }
    }
}

/// A state machine instance
pub struct StateMachine {
    current_state: StateId,
    transitions: Vec<Transition>,
    entry_callbacks: FxHashMap<StateId, Vec<Action>>,
    exit_callbacks: FxHashMap<StateId, Vec<Action>>,
    /// History of state transitions (for debugging)
    history: Vec<(StateId, EventId, StateId)>,
}

impl StateMachine {
    /// Create a new state machine with an initial state and transitions
    pub fn new(initial_state: StateId, transitions: Vec<Transition>) -> Self {
        Self {
            current_state: initial_state,
            transitions,
            entry_callbacks: FxHashMap::default(),
            exit_callbacks: FxHashMap::default(),
            history: Vec::new(),
        }
    }

    /// Create a builder for a state machine
    pub fn builder(initial_state: StateId) -> StateMachineBuilder {
        StateMachineBuilder::new(initial_state)
    }

    /// Get the current state
    pub fn current_state(&self) -> StateId {
        self.current_state
    }

    /// Check if we're in a specific state
    pub fn is_in(&self, state: StateId) -> bool {
        self.current_state == state
    }

    /// Get transition history
    pub fn history(&self) -> &[(StateId, EventId, StateId)] {
        &self.history
    }

    /// Clear transition history
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// Check if an event can trigger a transition from current state
    pub fn can_send(&self, event: EventId) -> bool {
        let current = self.current_state;
        self.transitions.iter().any(|t| {
            t.from_state == current && t.event == event && {
                match &t.guard {
                    Some(guard) => guard(),
                    None => true,
                }
            }
        })
    }

    /// Send an event to the state machine, potentially triggering a transition
    pub fn send(&mut self, event: EventId) -> StateId {
        let current = self.current_state;

        // Find matching transition
        let transition_idx = self.transitions.iter().position(|t| {
            t.from_state == current && t.event == event && {
                match &t.guard {
                    Some(guard) => guard(),
                    None => true,
                }
            }
        });

        let Some(idx) = transition_idx else {
            return current;
        };

        // Get the target state before executing callbacks
        let to_state = self.transitions[idx].to_state;

        // Execute exit callbacks
        if let Some(callbacks) = self.exit_callbacks.get_mut(&current) {
            for callback in callbacks.iter_mut() {
                callback();
            }
        }

        // Execute transition actions
        for action in self.transitions[idx].actions.iter_mut() {
            action();
        }

        // Update state
        self.current_state = to_state;

        // Record history
        self.history.push((current, event, to_state));

        // Execute entry callbacks
        if let Some(callbacks) = self.entry_callbacks.get_mut(&to_state) {
            for callback in callbacks.iter_mut() {
                callback();
            }
        }

        to_state
    }

    /// Register an entry callback for a state
    pub fn on_enter<F: FnMut() + Send + 'static>(&mut self, state: StateId, callback: F) {
        self.entry_callbacks
            .entry(state)
            .or_default()
            .push(Box::new(callback));
    }

    /// Register an exit callback for a state
    pub fn on_exit<F: FnMut() + Send + 'static>(&mut self, state: StateId, callback: F) {
        self.exit_callbacks
            .entry(state)
            .or_default()
            .push(Box::new(callback));
    }
}

/// Runtime that manages all state machine instances
pub struct FsmRuntime {
    machines: SlotMap<FsmId, StateMachine>,
}

impl FsmRuntime {
    pub fn new() -> Self {
        Self {
            machines: SlotMap::with_key(),
        }
    }

    /// Create a state machine from a builder
    pub fn create(&mut self, machine: StateMachine) -> FsmId {
        self.machines.insert(machine)
    }

    /// Create a simple state machine with initial state and transitions
    pub fn create_simple(&mut self, initial_state: StateId, transitions: Vec<Transition>) -> FsmId {
        self.machines
            .insert(StateMachine::new(initial_state, transitions))
    }

    /// Get a reference to a state machine
    pub fn get(&self, id: FsmId) -> Option<&StateMachine> {
        self.machines.get(id)
    }

    /// Get a mutable reference to a state machine
    pub fn get_mut(&mut self, id: FsmId) -> Option<&mut StateMachine> {
        self.machines.get_mut(id)
    }

    /// Send an event to a state machine
    pub fn send(&mut self, id: FsmId, event: EventId) -> Option<StateId> {
        self.machines.get_mut(id).map(|fsm| fsm.send(event))
    }

    /// Get current state of a state machine
    pub fn current_state(&self, id: FsmId) -> Option<StateId> {
        self.machines.get(id).map(|fsm| fsm.current_state())
    }

    /// Remove a state machine
    pub fn remove(&mut self, id: FsmId) -> Option<StateMachine> {
        self.machines.remove(id)
    }

    /// Get the number of state machines
    pub fn len(&self) -> usize {
        self.machines.len()
    }

    /// Check if runtime has no state machines
    pub fn is_empty(&self) -> bool {
        self.machines.is_empty()
    }
}

impl Default for FsmRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    // State constants for tests
    const IDLE: StateId = 0;
    const HOVERED: StateId = 1;
    const PRESSED: StateId = 2;
    const DISABLED: StateId = 3;

    // Event constants for tests
    const POINTER_ENTER: EventId = 1;
    const POINTER_LEAVE: EventId = 2;
    const POINTER_DOWN: EventId = 3;
    const POINTER_UP: EventId = 4;
    const DISABLE: EventId = 5;

    #[test]
    fn test_simple_transitions() {
        let mut fsm = StateMachine::new(
            IDLE,
            vec![
                Transition::new(IDLE, POINTER_ENTER, HOVERED),
                Transition::new(HOVERED, POINTER_LEAVE, IDLE),
                Transition::new(HOVERED, POINTER_DOWN, PRESSED),
                Transition::new(PRESSED, POINTER_UP, HOVERED),
            ],
        );

        assert_eq!(fsm.current_state(), IDLE);

        fsm.send(POINTER_ENTER);
        assert_eq!(fsm.current_state(), HOVERED);

        fsm.send(POINTER_DOWN);
        assert_eq!(fsm.current_state(), PRESSED);

        fsm.send(POINTER_UP);
        assert_eq!(fsm.current_state(), HOVERED);

        fsm.send(POINTER_LEAVE);
        assert_eq!(fsm.current_state(), IDLE);
    }

    #[test]
    fn test_invalid_event_no_transition() {
        let mut fsm = StateMachine::new(IDLE, vec![Transition::new(IDLE, POINTER_ENTER, HOVERED)]);

        // POINTER_DOWN is not valid in IDLE state
        fsm.send(POINTER_DOWN);
        assert_eq!(fsm.current_state(), IDLE);
    }

    #[test]
    fn test_guard_conditions() {
        let enabled = Arc::new(Mutex::new(true));
        let enabled_clone = enabled.clone();

        let mut fsm = StateMachine::builder(IDLE)
            .transition(
                Transition::new(IDLE, POINTER_ENTER, HOVERED)
                    .with_guard(move || *enabled_clone.lock().unwrap()),
            )
            .build();

        // Guard passes - transition happens
        fsm.send(POINTER_ENTER);
        assert_eq!(fsm.current_state(), HOVERED);

        // Reset to IDLE (manually for test)
        fsm.current_state = IDLE;

        // Disable the guard
        *enabled.lock().unwrap() = false;

        // Guard fails - no transition
        fsm.send(POINTER_ENTER);
        assert_eq!(fsm.current_state(), IDLE);
    }

    #[test]
    fn test_entry_exit_callbacks() {
        let entry_count = Arc::new(Mutex::new(0));
        let exit_count = Arc::new(Mutex::new(0));

        let entry_clone = entry_count.clone();
        let exit_clone = exit_count.clone();

        let mut fsm = StateMachine::builder(IDLE)
            .on(IDLE, POINTER_ENTER, HOVERED)
            .on(HOVERED, POINTER_LEAVE, IDLE)
            .on_enter(HOVERED, move || {
                *entry_clone.lock().unwrap() += 1;
            })
            .on_exit(HOVERED, move || {
                *exit_clone.lock().unwrap() += 1;
            })
            .build();

        assert_eq!(*entry_count.lock().unwrap(), 0);
        assert_eq!(*exit_count.lock().unwrap(), 0);

        fsm.send(POINTER_ENTER);
        assert_eq!(*entry_count.lock().unwrap(), 1);
        assert_eq!(*exit_count.lock().unwrap(), 0);

        fsm.send(POINTER_LEAVE);
        assert_eq!(*entry_count.lock().unwrap(), 1);
        assert_eq!(*exit_count.lock().unwrap(), 1);

        fsm.send(POINTER_ENTER);
        assert_eq!(*entry_count.lock().unwrap(), 2);
    }

    #[test]
    fn test_transition_actions() {
        let action_count = Arc::new(Mutex::new(0));
        let action_clone = action_count.clone();

        let mut fsm = StateMachine::builder(IDLE)
            .transition(
                Transition::new(IDLE, POINTER_ENTER, HOVERED).with_action(move || {
                    *action_clone.lock().unwrap() += 1;
                }),
            )
            .build();

        fsm.send(POINTER_ENTER);
        assert_eq!(*action_count.lock().unwrap(), 1);
    }

    #[test]
    fn test_history() {
        let mut fsm = StateMachine::new(
            IDLE,
            vec![
                Transition::new(IDLE, POINTER_ENTER, HOVERED),
                Transition::new(HOVERED, POINTER_DOWN, PRESSED),
            ],
        );

        fsm.send(POINTER_ENTER);
        fsm.send(POINTER_DOWN);

        let history = fsm.history();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0], (IDLE, POINTER_ENTER, HOVERED));
        assert_eq!(history[1], (HOVERED, POINTER_DOWN, PRESSED));
    }

    #[test]
    fn test_can_send() {
        let fsm = StateMachine::new(IDLE, vec![Transition::new(IDLE, POINTER_ENTER, HOVERED)]);

        assert!(fsm.can_send(POINTER_ENTER));
        assert!(!fsm.can_send(POINTER_DOWN));
    }

    #[test]
    fn test_fsm_runtime() {
        let mut runtime = FsmRuntime::new();

        let fsm1 = runtime.create_simple(IDLE, vec![Transition::new(IDLE, POINTER_ENTER, HOVERED)]);

        let fsm2 = runtime.create_simple(IDLE, vec![Transition::new(IDLE, POINTER_DOWN, PRESSED)]);

        assert_eq!(runtime.len(), 2);

        runtime.send(fsm1, POINTER_ENTER);
        assert_eq!(runtime.current_state(fsm1), Some(HOVERED));
        assert_eq!(runtime.current_state(fsm2), Some(IDLE));

        runtime.remove(fsm1);
        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime.current_state(fsm1), None);
    }
}

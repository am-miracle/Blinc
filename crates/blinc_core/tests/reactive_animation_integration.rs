//! Integration tests for reactive signals + FSM + animation system
//!
//! These tests verify that:
//! - The reactive system properly drives animations
//! - Animation state changes trigger reactive updates
//! - FSM state transitions can trigger animation targets
//! - All three systems can work together in a widget-like scenario

use blinc_animation::{AnimationScheduler, Spring, SpringConfig};
use blinc_core::fsm::{StateMachine, Transition};
use blinc_core::reactive::ReactiveGraph;
use std::sync::{Arc, Mutex};

/// Test that reactive signals can drive spring animation targets
#[test]
fn test_signal_drives_spring_target() {
    let mut graph = ReactiveGraph::new();

    // Create a reactive signal for position
    let position = graph.create_signal(0.0f32);

    // Create a spring that will animate toward the signal value
    let mut spring = Spring::new(SpringConfig::stiff(), 0.0);

    // Initial state
    assert_eq!(graph.get(position), Some(0.0));
    assert_eq!(spring.target(), 0.0);

    // Update the signal - this would drive the animation in a real UI
    graph.set(position, 100.0);

    // In a real scenario, an effect would sync signal to spring
    spring.set_target(graph.get(position).unwrap_or(0.0));

    // Simulate animation frames
    for _ in 0..60 {
        spring.step(1.0 / 60.0);
    }

    // Spring should be near target
    assert!((spring.value() - 100.0).abs() < 5.0);
}

/// Test that effects can update spring targets on signal changes
#[test]
fn test_effect_updates_spring_on_signal_change() {
    let mut graph = ReactiveGraph::new();

    let target_value = graph.create_signal(0.0f32);
    let spring_target = Arc::new(Mutex::new(0.0f32));
    let spring_target_clone = spring_target.clone();

    // Create an effect that syncs signal to spring target
    let _effect = graph.create_effect(move |g| {
        let value = g.get(target_value).unwrap_or(0.0);
        *spring_target_clone.lock().unwrap() = value;
    });

    // Effect runs immediately with initial value
    assert_eq!(*spring_target.lock().unwrap(), 0.0);

    // Update signal - effect should sync to spring
    graph.set(target_value, 50.0);
    assert_eq!(*spring_target.lock().unwrap(), 50.0);

    // Another update
    graph.set(target_value, -25.0);
    assert_eq!(*spring_target.lock().unwrap(), -25.0);
}

/// Test that animation scheduler can work with reactive-driven springs
#[test]
fn test_scheduler_with_reactive_signals() {
    let mut graph = ReactiveGraph::new();
    let scheduler = AnimationScheduler::new();

    // Create signals for multiple animated properties
    let scale = graph.create_signal(1.0f32);
    let opacity = graph.create_signal(1.0f32);

    // Create springs and add to scheduler
    let scale_spring = Spring::new(SpringConfig::snappy(), 1.0);
    let opacity_spring = Spring::new(SpringConfig::gentle(), 1.0);

    let scale_id = scheduler.add_spring(scale_spring);
    let opacity_id = scheduler.add_spring(opacity_spring);

    // Initially no active animations (springs at target)
    assert!(!scheduler.has_active_animations());

    // Update signals (these would trigger effects in real usage)
    graph.set(scale, 1.5);
    graph.set(opacity, 0.5);

    // Sync signal values to springs
    scheduler.with_spring_mut(scale_id, |spring| {
        spring.set_target(graph.get(scale).unwrap_or(1.0));
    });
    scheduler.with_spring_mut(opacity_id, |spring| {
        spring.set_target(graph.get(opacity).unwrap_or(1.0));
    });

    // Now we have active animations
    assert!(scheduler.has_active_animations());

    // Simulate animation loop using the scheduler's iterator
    for _ in 0..120 {
        scheduler.springs_iter_mut().for_each(|_, spring| {
            spring.step(1.0 / 60.0);
        });
    }

    // Check springs have settled
    let scale_spring = scheduler.get_spring(scale_id).unwrap();
    let opacity_spring = scheduler.get_spring(opacity_id).unwrap();

    assert!(scale_spring.is_settled());
    assert!(opacity_spring.is_settled());
    assert!((scale_spring.value() - 1.5).abs() < 0.01);
    assert!((opacity_spring.value() - 0.5).abs() < 0.01);
}

/// Test batched signal updates don't cause animation jitter
#[test]
fn test_batched_updates_for_smooth_animations() {
    let mut graph = ReactiveGraph::new();
    let effect_count = Arc::new(Mutex::new(0));

    let x = graph.create_signal(0.0f32);
    let y = graph.create_signal(0.0f32);
    let scale = graph.create_signal(1.0f32);

    let effect_count_clone = effect_count.clone();
    let _effect = graph.create_effect(move |g| {
        // In real code, this would update spring targets
        let _x = g.get(x);
        let _y = g.get(y);
        let _scale = g.get(scale);
        *effect_count_clone.lock().unwrap() += 1;
    });

    // Initial effect run
    assert_eq!(*effect_count.lock().unwrap(), 1);

    // Without batching: 3 effect runs
    *effect_count.lock().unwrap() = 0;
    graph.set(x, 10.0);
    graph.set(y, 20.0);
    graph.set(scale, 2.0);
    assert_eq!(*effect_count.lock().unwrap(), 3);

    // With batching: 1 effect run (better for animation smoothness)
    *effect_count.lock().unwrap() = 0;
    graph.batch(|g| {
        g.set(x, 100.0);
        g.set(y, 200.0);
        g.set(scale, 0.5);
    });
    assert_eq!(*effect_count.lock().unwrap(), 1);
}

/// Test derived values for computed animation properties
#[test]
fn test_derived_animation_properties() {
    let mut graph = ReactiveGraph::new();

    // Base properties
    let hover_progress = graph.create_signal(0.0f32); // 0.0 = normal, 1.0 = hovered

    // Derived animation targets
    let target_scale = graph.create_derived(move |g| {
        let progress = g.get(hover_progress).unwrap_or(0.0);
        1.0 + progress * 0.2 // 1.0 -> 1.2 on hover
    });

    let target_shadow = graph.create_derived(move |g| {
        let progress = g.get(hover_progress).unwrap_or(0.0);
        4.0 + progress * 8.0 // 4px -> 12px shadow on hover
    });

    // Initial state
    assert_eq!(graph.get_derived(target_scale), Some(1.0));
    assert_eq!(graph.get_derived(target_shadow), Some(4.0));

    // Simulate hover (this could be driven by FSM)
    graph.set(hover_progress, 1.0);
    assert_eq!(graph.get_derived(target_scale), Some(1.2));
    assert_eq!(graph.get_derived(target_shadow), Some(12.0));

    // Partial hover (mid-transition)
    graph.set(hover_progress, 0.5);
    assert_eq!(graph.get_derived(target_scale), Some(1.1));
    assert_eq!(graph.get_derived(target_shadow), Some(8.0));
}

/// Test that spring value changes can trigger reactive updates
#[test]
fn test_spring_value_updates_signal() {
    let mut graph = ReactiveGraph::new();

    // Signal representing the current animated value
    let animated_value = graph.create_signal(0.0f32);
    let render_count = Arc::new(Mutex::new(0));

    // Effect that would trigger re-render when value changes
    let render_count_clone = render_count.clone();
    let _effect = graph.create_effect(move |g| {
        let _val = g.get(animated_value);
        *render_count_clone.lock().unwrap() += 1;
    });

    assert_eq!(*render_count.lock().unwrap(), 1); // Initial

    // Create spring
    let mut spring = Spring::new(SpringConfig::stiff(), 0.0);
    spring.set_target(100.0);

    // Simulate animation loop - each frame updates signal
    let mut last_value = 0.0f32;
    for _ in 0..30 {
        spring.step(1.0 / 60.0);

        // Only update signal if value changed significantly
        let current = spring.value();
        if (current - last_value).abs() > 0.1 {
            graph.set(animated_value, current);
            last_value = current;
        }
    }

    // Effect should have run multiple times as value animated
    assert!(*render_count.lock().unwrap() > 1);
}

/// Test interruptible animations with reactive signals
#[test]
fn test_interruptible_animation() {
    let mut graph = ReactiveGraph::new();

    let target = graph.create_signal(0.0f32);
    let mut spring = Spring::new(SpringConfig::stiff(), 0.0); // Use stiff for less overshoot

    // Start animation toward 100
    graph.set(target, 100.0);
    spring.set_target(graph.get(target).unwrap_or(0.0));

    // Animate for a bit (fewer frames to catch mid-flight)
    for _ in 0..10 {
        spring.step(1.0 / 60.0);
    }

    // Mid-flight - spring should be moving toward target
    assert!(
        spring.value() > 0.0,
        "Spring should have moved from initial position"
    );
    let mid_velocity = spring.velocity();
    assert!(mid_velocity > 0.0, "Spring should be moving forward");

    // Interrupt! New target
    graph.set(target, 0.0);
    spring.set_target(graph.get(target).unwrap_or(0.0));

    // Velocity should be preserved (animation continues with momentum)
    assert_eq!(spring.velocity(), mid_velocity);

    // Continue animation - spring may overshoot but will eventually settle
    for _ in 0..180 {
        spring.step(1.0 / 60.0);
    }

    // Should settle back to 0
    assert!(spring.is_settled());
    assert!((spring.value() - 0.0).abs() < 0.01);
}

// =============================================================================
// FSM + Animation Integration Tests
// =============================================================================

// Button state constants
const IDLE: u32 = 0;
const HOVERED: u32 = 1;
const PRESSED: u32 = 2;

// Event constants
const POINTER_ENTER: u32 = 1;
const POINTER_LEAVE: u32 = 2;
const POINTER_DOWN: u32 = 3;
const POINTER_UP: u32 = 4;

/// Test FSM state transitions driving animation targets
#[test]
fn test_fsm_drives_animation_targets() {
    // Create button FSM
    let mut fsm = StateMachine::new(
        IDLE,
        vec![
            Transition::new(IDLE, POINTER_ENTER, HOVERED),
            Transition::new(HOVERED, POINTER_LEAVE, IDLE),
            Transition::new(HOVERED, POINTER_DOWN, PRESSED),
            Transition::new(PRESSED, POINTER_UP, HOVERED),
        ],
    );

    // Animation properties for each state
    fn get_scale_for_state(state: u32) -> f32 {
        match state {
            IDLE => 1.0,
            HOVERED => 1.05,
            PRESSED => 0.95,
            _ => 1.0,
        }
    }

    let mut scale_spring = Spring::new(SpringConfig::stiff(), 1.0);

    // Initial state
    assert_eq!(fsm.current_state(), IDLE);
    assert_eq!(scale_spring.target(), 1.0);

    // Hover - should animate to 1.05
    fsm.send(POINTER_ENTER);
    scale_spring.set_target(get_scale_for_state(fsm.current_state()));
    assert_eq!(scale_spring.target(), 1.05);

    // Animate
    for _ in 0..60 {
        scale_spring.step(1.0 / 60.0);
    }
    assert!((scale_spring.value() - 1.05).abs() < 0.01);

    // Press - should animate to 0.95
    fsm.send(POINTER_DOWN);
    scale_spring.set_target(get_scale_for_state(fsm.current_state()));
    assert_eq!(scale_spring.target(), 0.95);

    for _ in 0..60 {
        scale_spring.step(1.0 / 60.0);
    }
    assert!((scale_spring.value() - 0.95).abs() < 0.01);
}

/// Test FSM callbacks triggering reactive signal updates
#[test]
fn test_fsm_callbacks_update_signals() {
    let mut graph = ReactiveGraph::new();

    // Reactive signal for hover state
    let is_hovered = graph.create_signal(false);
    let hover_count = Arc::new(Mutex::new(0));

    // Effect that tracks hover changes
    let hover_count_clone = hover_count.clone();
    let _effect = graph.create_effect(move |g| {
        let hovered = g.get(is_hovered).unwrap_or(false);
        if hovered {
            *hover_count_clone.lock().unwrap() += 1;
        }
    });

    // FSM with callbacks that update signals
    // Note: In real code, we'd use proper callback integration
    let mut fsm = StateMachine::new(
        IDLE,
        vec![
            Transition::new(IDLE, POINTER_ENTER, HOVERED),
            Transition::new(HOVERED, POINTER_LEAVE, IDLE),
        ],
    );

    // Simulate FSM transition and signal update
    fsm.send(POINTER_ENTER);
    graph.set(is_hovered, fsm.current_state() == HOVERED);

    assert_eq!(*hover_count.lock().unwrap(), 1);

    // Leave
    fsm.send(POINTER_LEAVE);
    graph.set(is_hovered, fsm.current_state() == HOVERED);

    // Count should still be 1 (effect ran but didn't increment because not hovered)
    assert_eq!(*hover_count.lock().unwrap(), 1);

    // Enter again
    fsm.send(POINTER_ENTER);
    graph.set(is_hovered, fsm.current_state() == HOVERED);

    assert_eq!(*hover_count.lock().unwrap(), 2);
}

/// Test complete widget integration: FSM -> Reactive -> Animation
#[test]
fn test_complete_widget_integration() {
    // This test simulates a complete button widget with:
    // - FSM for interaction states
    // - Reactive signals for derived properties
    // - Spring animations for smooth transitions

    let mut graph = ReactiveGraph::new();
    let scheduler = AnimationScheduler::new();

    // Reactive state
    let fsm_state = graph.create_signal(IDLE);

    // Derived animation targets based on FSM state
    let target_scale = graph.create_derived(move |g| match g.get(fsm_state).unwrap_or(IDLE) {
        IDLE => 1.0f32,
        HOVERED => 1.08,
        PRESSED => 0.95,
        _ => 1.0,
    });

    let target_brightness = graph.create_derived(move |g| match g.get(fsm_state).unwrap_or(IDLE) {
        IDLE => 1.0f32,
        HOVERED => 1.1,
        PRESSED => 0.9,
        _ => 1.0,
    });

    // Springs for animation
    let scale_spring = Spring::new(SpringConfig::snappy(), 1.0);
    let brightness_spring = Spring::new(SpringConfig::stiff(), 1.0);

    let scale_id = scheduler.add_spring(scale_spring);
    let brightness_id = scheduler.add_spring(brightness_spring);

    // FSM for interactions
    let mut fsm = StateMachine::new(
        IDLE,
        vec![
            Transition::new(IDLE, POINTER_ENTER, HOVERED),
            Transition::new(HOVERED, POINTER_LEAVE, IDLE),
            Transition::new(HOVERED, POINTER_DOWN, PRESSED),
            Transition::new(PRESSED, POINTER_UP, HOVERED),
        ],
    );

    // Simulate interaction sequence: hover -> press -> release -> leave

    // 1. Hover
    fsm.send(POINTER_ENTER);
    graph.set(fsm_state, fsm.current_state());

    // Sync derived values to springs
    scheduler.with_spring_mut(scale_id, |s| {
        s.set_target(graph.get_derived(target_scale).unwrap_or(1.0));
    });
    scheduler.with_spring_mut(brightness_id, |s| {
        s.set_target(graph.get_derived(target_brightness).unwrap_or(1.0));
    });

    // Animate
    for _ in 0..60 {
        scheduler.springs_iter_mut().for_each(|_, spring| {
            spring.step(1.0 / 60.0);
        });
    }

    let scale = scheduler.get_spring(scale_id).unwrap().value();
    let brightness = scheduler.get_spring(brightness_id).unwrap().value();
    assert!(
        (scale - 1.08).abs() < 0.02,
        "Scale should be ~1.08 on hover"
    );
    assert!(
        (brightness - 1.1).abs() < 0.02,
        "Brightness should be ~1.1 on hover"
    );

    // 2. Press
    fsm.send(POINTER_DOWN);
    graph.set(fsm_state, fsm.current_state());

    scheduler.with_spring_mut(scale_id, |s| {
        s.set_target(graph.get_derived(target_scale).unwrap_or(1.0));
    });
    scheduler.with_spring_mut(brightness_id, |s| {
        s.set_target(graph.get_derived(target_brightness).unwrap_or(1.0));
    });

    for _ in 0..60 {
        scheduler.springs_iter_mut().for_each(|_, spring| {
            spring.step(1.0 / 60.0);
        });
    }

    let scale = scheduler.get_spring(scale_id).unwrap().value();
    let brightness = scheduler.get_spring(brightness_id).unwrap().value();
    assert!(
        (scale - 0.95).abs() < 0.02,
        "Scale should be ~0.95 on press"
    );
    assert!(
        (brightness - 0.9).abs() < 0.02,
        "Brightness should be ~0.9 on press"
    );

    // 3. Release (back to hovered)
    fsm.send(POINTER_UP);
    graph.set(fsm_state, fsm.current_state());
    assert_eq!(fsm.current_state(), HOVERED);

    // 4. Leave (back to idle)
    fsm.send(POINTER_LEAVE);
    graph.set(fsm_state, fsm.current_state());

    scheduler.with_spring_mut(scale_id, |s| {
        s.set_target(graph.get_derived(target_scale).unwrap_or(1.0));
    });
    scheduler.with_spring_mut(brightness_id, |s| {
        s.set_target(graph.get_derived(target_brightness).unwrap_or(1.0));
    });

    for _ in 0..90 {
        scheduler.springs_iter_mut().for_each(|_, spring| {
            spring.step(1.0 / 60.0);
        });
    }

    let scale = scheduler.get_spring(scale_id).unwrap().value();
    let brightness = scheduler.get_spring(brightness_id).unwrap().value();
    assert!((scale - 1.0).abs() < 0.02, "Scale should return to 1.0");
    assert!(
        (brightness - 1.0).abs() < 0.02,
        "Brightness should return to 1.0"
    );
}

/// Test rapid state changes with interruptible animations
#[test]
fn test_rapid_state_changes() {
    let mut graph = ReactiveGraph::new();

    let state = graph.create_signal(IDLE);
    let mut scale_spring = Spring::new(SpringConfig::wobbly(), 1.0);

    let mut fsm = StateMachine::new(
        IDLE,
        vec![
            Transition::new(IDLE, POINTER_ENTER, HOVERED),
            Transition::new(HOVERED, POINTER_LEAVE, IDLE),
        ],
    );

    fn get_scale(s: u32) -> f32 {
        if s == HOVERED {
            1.1
        } else {
            1.0
        }
    }

    // Rapid hover/unhover sequence
    for _ in 0..5 {
        // Enter
        fsm.send(POINTER_ENTER);
        graph.set(state, fsm.current_state());
        scale_spring.set_target(get_scale(graph.get(state).unwrap_or(IDLE)));

        // Only a few frames before next change
        for _ in 0..5 {
            scale_spring.step(1.0 / 60.0);
        }

        // Leave
        fsm.send(POINTER_LEAVE);
        graph.set(state, fsm.current_state());
        scale_spring.set_target(get_scale(graph.get(state).unwrap_or(IDLE)));

        for _ in 0..5 {
            scale_spring.step(1.0 / 60.0);
        }
    }

    // Spring should still be stable (not explode due to rapid changes)
    assert!(scale_spring.value().is_finite());
    assert!(scale_spring.value() > 0.5);
    assert!(scale_spring.value() < 1.5);

    // Let it settle
    for _ in 0..120 {
        scale_spring.step(1.0 / 60.0);
    }

    // Should settle to idle scale
    assert!(scale_spring.is_settled());
    assert!((scale_spring.value() - 1.0).abs() < 0.01);
}

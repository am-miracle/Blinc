//! Gesture recognizers for touch and pointer interactions
//!
//! Provides high-level gesture detection on top of raw pointer events:
//! tap, long-press, swipe, and pull-to-refresh.
//!
//! # Example
//!
//! ```ignore
//! use blinc_layout::widgets::gesture::*;
//!
//! div()
//!     .on_tap(|ctx| println!("Tapped!"))
//!     .on_long_press(|ctx| println!("Long pressed!"))
//!     .on_swipe(|dir, ctx| match dir {
//!         SwipeDirection::Left => println!("Swiped left"),
//!         SwipeDirection::Right => println!("Swiped right"),
//!         _ => {}
//!     })
//! ```

use std::sync::{Arc, Mutex};

/// Swipe direction
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwipeDirection {
    Left,
    Right,
    Up,
    Down,
}

/// Internal state for gesture detection on a single element
#[derive(Clone)]
struct GestureState {
    /// Position where the pointer went down
    start_x: f32,
    start_y: f32,
    /// Timestamp of pointer down (ms)
    start_time: u64,
    /// Whether pointer is currently down
    is_down: bool,
    /// Whether the long-press timer has fired
    long_press_fired: bool,
}

impl Default for GestureState {
    fn default() -> Self {
        Self {
            start_x: 0.0,
            start_y: 0.0,
            start_time: 0,
            is_down: false,
            long_press_fired: false,
        }
    }
}

/// Minimum distance (px) for a swipe gesture
const SWIPE_THRESHOLD: f32 = 30.0;
/// Maximum distance (px) for a tap (must not move more than this)
const TAP_MAX_DISTANCE: f32 = 10.0;
/// Maximum duration (ms) for a tap
const TAP_MAX_DURATION: u64 = 300;
/// Duration (ms) before a long press fires
const LONG_PRESS_DURATION: u64 = 500;

fn elapsed_ms() -> u64 {
    static START: std::sync::OnceLock<web_time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(web_time::Instant::now)
        .elapsed()
        .as_millis() as u64
}

/// Extension trait adding gesture methods to Div
pub trait GestureExt: Sized {
    /// Register a tap handler (quick press and release without significant movement)
    fn on_tap<F>(self, handler: F) -> Self
    where
        F: Fn(&crate::event_handler::EventContext) + 'static;

    /// Register a long-press handler (press and hold for 500ms+)
    fn on_long_press<F>(self, handler: F) -> Self
    where
        F: Fn(&crate::event_handler::EventContext) + 'static;

    /// Register a swipe handler with direction detection
    fn on_swipe<F>(self, handler: F) -> Self
    where
        F: Fn(SwipeDirection, &crate::event_handler::EventContext) + 'static;
}

impl GestureExt for crate::div::Div {
    fn on_tap<F>(self, handler: F) -> Self
    where
        F: Fn(&crate::event_handler::EventContext) + 'static,
    {
        let state = Arc::new(Mutex::new(GestureState::default()));
        let state_down = Arc::clone(&state);
        let state_up = Arc::clone(&state);

        self.on_mouse_down(move |ctx| {
            let mut s = state_down.lock().unwrap();
            s.start_x = ctx.mouse_x;
            s.start_y = ctx.mouse_y;
            s.start_time = elapsed_ms();
            s.is_down = true;
        })
        .on_mouse_up(move |ctx| {
            let s = state_up.lock().unwrap();
            if !s.is_down {
                return;
            }
            let dx = (ctx.mouse_x - s.start_x).abs();
            let dy = (ctx.mouse_y - s.start_y).abs();
            let dt = elapsed_ms() - s.start_time;

            if dx < TAP_MAX_DISTANCE && dy < TAP_MAX_DISTANCE && dt < TAP_MAX_DURATION {
                handler(ctx);
            }
        })
    }

    fn on_long_press<F>(self, handler: F) -> Self
    where
        F: Fn(&crate::event_handler::EventContext) + 'static,
    {
        let state = Arc::new(Mutex::new(GestureState::default()));
        let state_down = Arc::clone(&state);
        let state_up = Arc::clone(&state);
        let handler = Arc::new(handler);

        self.on_mouse_down(move |ctx| {
            let mut s = state_down.lock().unwrap();
            s.start_x = ctx.mouse_x;
            s.start_y = ctx.mouse_y;
            s.start_time = elapsed_ms();
            s.is_down = true;
            s.long_press_fired = false;
        })
        .on_mouse_up(move |ctx| {
            let mut s = state_up.lock().unwrap();
            if !s.is_down {
                return;
            }
            let dx = (ctx.mouse_x - s.start_x).abs();
            let dy = (ctx.mouse_y - s.start_y).abs();
            let dt = elapsed_ms() - s.start_time;

            // Long press: held long enough without moving
            if dx < TAP_MAX_DISTANCE
                && dy < TAP_MAX_DISTANCE
                && dt >= LONG_PRESS_DURATION
                && !s.long_press_fired
            {
                s.long_press_fired = true;
                handler(ctx);
            }
            s.is_down = false;
        })
    }

    fn on_swipe<F>(self, handler: F) -> Self
    where
        F: Fn(SwipeDirection, &crate::event_handler::EventContext) + 'static,
    {
        let state = Arc::new(Mutex::new(GestureState::default()));
        let state_down = Arc::clone(&state);
        let state_up = Arc::clone(&state);

        self.on_mouse_down(move |ctx| {
            let mut s = state_down.lock().unwrap();
            s.start_x = ctx.mouse_x;
            s.start_y = ctx.mouse_y;
            s.start_time = elapsed_ms();
            s.is_down = true;
        })
        .on_mouse_up(move |ctx| {
            let s = state_up.lock().unwrap();
            if !s.is_down {
                return;
            }
            let dx = ctx.mouse_x - s.start_x;
            let dy = ctx.mouse_y - s.start_y;

            // Determine primary axis
            if dx.abs() > dy.abs() && dx.abs() > SWIPE_THRESHOLD {
                let dir = if dx > 0.0 {
                    SwipeDirection::Right
                } else {
                    SwipeDirection::Left
                };
                handler(dir, ctx);
            } else if dy.abs() > dx.abs() && dy.abs() > SWIPE_THRESHOLD {
                let dir = if dy > 0.0 {
                    SwipeDirection::Down
                } else {
                    SwipeDirection::Up
                };
                handler(dir, ctx);
            }
        })
    }
}

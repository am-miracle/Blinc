//! Platform-agnostic context trait for Blinc applications
//!
//! This module provides the `BlincContext` trait which abstracts platform-specific
//! context implementations like `WindowedContext`. This enables components and
//! component libraries (like `blinc_cn`) to be initialized without depending on
//! platform-specific code.
//!
//! # Architecture
//!
//! The context trait provides access to:
//! - **State Management**: `use_state`, `use_signal`, signals and derived values
//! - **Animations**: Access to the animation scheduler for spring/keyframe animations
//! - **Overlays**: Manager for modals, toasts, dropdowns, etc.
//! - **Refs**: Element references for programmatic control
//! - **Dirty Flag**: For triggering UI rebuilds
//!
//! # Example
//!
//! ```ignore
//! use blinc_core::context::BlincContext;
//!
//! fn my_component(ctx: &dyn BlincContext) -> impl ElementBuilder {
//!     let count = ctx.use_state_keyed("count", || 0);
//!
//!     div()
//!         .child(text(&format!("Count: {}", count.get())))
//!         .on_click({
//!             let count = count.clone();
//!             move |_| count.set(count.get() + 1)
//!         })
//! }
//! ```

use crate::reactive::{Derived, DirtyFlag, ReactiveGraph, Signal, State};

/// Platform-agnostic context trait for Blinc applications
///
/// This trait abstracts the platform-specific context (like `WindowedContext`)
/// and provides a common interface for:
/// - State management (signals, derived values, persistent state)
/// - Animation scheduling
/// - Overlay management
/// - Element references
///
/// # Thread Safety
///
/// Note that this trait does NOT require `Send + Sync`. The context is typically
/// owned by the main thread and accessed synchronously during UI builds. For
/// cross-thread access, use the shared handles like `SharedAnimationScheduler`,
/// `OverlayManager`, etc.
///
/// # Implementors
///
/// - `WindowedContext` (desktop/Android windowed apps)
/// - Future: `HeadlessContext` (testing), `WebContext` (WASM), etc.
pub trait BlincContext {
    // =========================================================================
    // Reactive State Management
    // =========================================================================

    /// Create a persistent state value that survives across UI rebuilds (keyed)
    ///
    /// This creates component-level state identified by a unique string key.
    /// Returns a `State<T>` with direct `.get()` and `.set()` methods.
    fn use_state_keyed<T, F>(&self, key: &str, init: F) -> State<T>
    where
        T: Clone + Send + 'static,
        F: FnOnce() -> T;

    /// Create a persistent signal that survives across UI rebuilds (keyed)
    ///
    /// Unlike `use_signal()` which creates a new signal each call, this method
    /// persists the signal using a unique string key.
    fn use_signal_keyed<T, F>(&self, key: &str, init: F) -> Signal<T>
    where
        T: Clone + Send + 'static,
        F: FnOnce() -> T;

    /// Create or retrieve a persistent reactive signal, auto-keyed
    /// by the caller's source location via `#[track_caller]`.
    /// Survives UI rebuilds.
    ///
    /// Equivalent to `use_signal_keyed(unique_call_site_key, ||
    /// initial)` — first call mints the signal, subsequent calls
    /// from the same line return the same handle.
    ///
    /// For loops or factories called multiple times from the same
    /// line, use [`Self::use_signal_keyed`] with an explicit per-
    /// instance key.
    #[track_caller]
    fn use_signal<T: Clone + Send + 'static>(&self, initial: T) -> Signal<T>;

    /// Get the current value of a signal
    fn get<T: Clone + 'static>(&self, signal: Signal<T>) -> Option<T>;

    /// Set the value of a signal, triggering reactive updates
    fn set<T: Send + 'static>(&self, signal: Signal<T>, value: T);

    /// Update a signal using a function
    fn update<T: Clone + Send + 'static, F: FnOnce(T) -> T>(&self, signal: Signal<T>, f: F);

    /// Create a derived (computed) value
    fn use_derived<T, F>(&self, compute: F) -> Derived<T>
    where
        T: Clone + Send + 'static,
        F: Fn(&ReactiveGraph) -> T + Send + 'static;

    /// Get the value of a derived computation
    fn get_derived<T: Clone + 'static>(&self, derived: Derived<T>) -> Option<T>;

    /// Batch multiple signal updates into a single reactive update
    fn batch<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut ReactiveGraph) -> R;

    // =========================================================================
    // Dirty Flag / Rebuild Triggering
    // =========================================================================

    /// Get the shared dirty flag for manual state management
    ///
    /// Use this when you want to create your own state types that trigger
    /// UI rebuilds when modified.
    fn dirty_flag(&self) -> DirtyFlag;

    /// Request a UI rebuild
    ///
    /// This is equivalent to setting the dirty flag to true.
    fn request_rebuild(&self);

    // =========================================================================
    // Window/Viewport Information
    // =========================================================================

    /// Get the current viewport width in logical pixels
    fn width(&self) -> f32;

    /// Get the current viewport height in logical pixels
    fn height(&self) -> f32;

    /// Get the current scale factor (physical / logical)
    fn scale_factor(&self) -> f64;
}

/// Extension trait for BlincContext with additional convenience methods
///
/// This trait provides higher-level APIs built on top of the core BlincContext trait.
pub trait BlincContextExt: BlincContext {
    /// Create a persistent state with automatic source-location key
    ///
    /// This is a convenience wrapper that uses `#[track_caller]` to automatically
    /// generate a unique key based on the call site.
    #[track_caller]
    fn use_state<T, F>(&self, init: F) -> State<T>
    where
        T: Clone + Send + 'static,
        F: FnOnce() -> T,
    {
        let location = std::panic::Location::caller();
        let key = format!(
            "{}:{}:{}",
            location.file(),
            location.line(),
            location.column()
        );
        self.use_state_keyed(&key, init)
    }

    /// **Deprecated.** [`BlincContext::use_signal`] is now itself
    /// auto-keyed via the track_caller attribute and persistent
    /// across rebuilds, so this extension method is redundant.
    /// Switch to `ctx.use_signal(init())` (eagerly evaluate the
    /// closure at the call site) or move to `use_signal_keyed` if
    /// you need the lazy-init form for a non-Default `T`.
    #[deprecated(
        since = "0.5.2",
        note = "use `use_signal(initial)` directly — it now auto-keys by caller location; pass `init()` if you previously deferred construction"
    )]
    #[track_caller]
    fn use_signal_auto<T, F>(&self, init: F) -> Signal<T>
    where
        T: Clone + Send + 'static,
        F: FnOnce() -> T,
    {
        let location = std::panic::Location::caller();
        let key = format!(
            "{}:{}:{}",
            location.file(),
            location.line(),
            location.column()
        );
        self.use_signal_keyed(&key, init)
    }
}

// Blanket implementation for all BlincContext implementors
impl<T: BlincContext + ?Sized> BlincContextExt for T {}

#[cfg(test)]
mod tests {
    // Tests for BlincContext trait are in integration tests
    // The trait is not dyn-compatible due to generic methods,
    // which is intentional - we use static dispatch for performance.
}

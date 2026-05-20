//! `ToastTray` — sibling type to `OverlayStack`.
//!
//! Notification queue, not a dismiss stack. Toasts are corner-stacked,
//! auto-dismiss after a duration, tap-to-dismiss, and don't participate in
//! input dispatch the way `OverlayStack` entries do. They render *above* the
//! overlay stack but in their own independent layer.
//!
//! The two types live in separate modules with separate singletons so they
//! can evolve independently. `blinc_app` composites the two layers when
//! building the final overlay layer.
//!
//! Animation, as with `OverlayStack`, is delegated to `motion()` / FLIP / CSS.
//! The tray observes `query_motion(toast.motion_key)` to know when an exiting
//! toast can be evicted; FLIP via `animate_bounds(::position().snappy())` on
//! the tray container handles smooth reorders when a toast in the middle
//! auto-dismisses.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use blinc_core::context_state::MotionAnimationState;
use blinc_core::BlincContextState;

use crate::div::Div;
use crate::widgets::overlay::Corner;

/// Stable id for the toast tray layer. See `OVERLAY_STACK_LAYER_ID` for the
/// subtree-rebuild rationale.
pub const TOAST_TRAY_LAYER_ID: &str = "__cn_toast_tray_layer__";

// =============================================================================
// ToastHandle
// =============================================================================

/// Stable id for a toast in the tray.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ToastHandle(u64);

impl ToastHandle {
    pub fn raw(&self) -> u64 {
        self.0
    }
}

// =============================================================================
// ToastEntry
// =============================================================================

pub struct ToastEntry {
    pub handle: ToastHandle,
    pub motion_key: String,
    pub spawned_at_ms: u64,
    /// Auto-dismiss after this many ms.
    pub auto_after_ms: u64,
    /// True once the dismiss has fired; rendered until motion exit completes.
    pub exiting: bool,
    /// Whether tapping the toast itself dismisses it.
    pub dismiss_on_click: bool,
    pub content_fn: Arc<dyn Fn() -> Div + Send + Sync>,
    pub on_close: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl ToastEntry {
    /// FSM stable key (with the "motion:" prefix that `motion_derived` adds
    /// internally). Same caveat as `OverlayEntry::motion_stable_key`.
    fn motion_stable_key(&self) -> String {
        format!("motion:{}", self.motion_key)
    }

    fn motion_done(&self) -> bool {
        let state = BlincContextState::try_get()
            .map(|ctx| ctx.query_motion(&self.motion_stable_key()))
            .unwrap_or(MotionAnimationState::NotFound);
        matches!(
            state,
            MotionAnimationState::Removed | MotionAnimationState::NotFound
        )
    }
}

// =============================================================================
// ToastTray
// =============================================================================

pub struct ToastTray {
    toasts: Vec<ToastEntry>,
    next_id: AtomicU64,
    corner: Corner,
    gap_px: f32,
    max_visible: usize,
    current_time_ms: u64,
    dirty: AtomicBool,
}

impl Default for ToastTray {
    fn default() -> Self {
        Self::new()
    }
}

impl ToastTray {
    pub fn new() -> Self {
        Self {
            toasts: Vec::new(),
            next_id: AtomicU64::new(1),
            corner: Corner::TopRight,
            gap_px: 8.0,
            max_visible: 5,
            current_time_ms: 0,
            dirty: AtomicBool::new(false),
        }
    }

    fn allocate_handle(&self) -> ToastHandle {
        ToastHandle(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Push a new toast onto the tray.
    pub fn push(&mut self, entry: ToastEntry) -> ToastHandle {
        let handle = entry.handle;
        self.toasts.push(entry);
        self.dirty.store(true, Ordering::Release);
        handle
    }

    /// Begin dismissing a specific toast. Toast stays rendered until its
    /// motion exit completes; eviction happens in `update()`.
    pub fn dismiss(&mut self, handle: ToastHandle) {
        if let Some(entry) = self.toasts.iter_mut().find(|e| e.handle == handle) {
            if entry.exiting {
                return;
            }
            entry.exiting = true;
            crate::queue_global_motion_exit_start(entry.motion_stable_key());
            if let Some(cb) = &entry.on_close {
                cb();
            }
            self.dirty.store(true, Ordering::Release);
        }
    }

    /// Dismiss every toast.
    pub fn dismiss_all(&mut self) {
        let handles: Vec<_> = self
            .toasts
            .iter()
            .filter(|e| !e.exiting)
            .map(|e| e.handle)
            .collect();
        for h in handles {
            self.dismiss(h);
        }
    }

    /// Per-frame tick. Auto-dismisses expired toasts; reaps exited ones.
    pub fn update(&mut self, current_time_ms: u64) {
        self.current_time_ms = current_time_ms;

        // 1. Auto-dismiss timers.
        let due: Vec<_> = self
            .toasts
            .iter()
            .filter(|e| !e.exiting)
            .filter(|e| {
                let due_at = e.spawned_at_ms.saturating_add(e.auto_after_ms);
                current_time_ms >= due_at
            })
            .map(|e| e.handle)
            .collect();
        for handle in due {
            self.dismiss(handle);
        }

        // 2. Reap exited toasts.
        let before = self.toasts.len();
        self.toasts.retain(|e| !(e.exiting && e.motion_done()));
        if self.toasts.len() != before {
            self.dirty.store(true, Ordering::Release);
        }
    }

    /// Click at viewport coords. Returns true if a toast absorbed the click.
    pub fn handle_click_at(
        &mut self,
        x: f32,
        y: f32,
        hit_test: &dyn Fn(&ToastEntry, f32, f32) -> bool,
    ) -> bool {
        let dismissables: Vec<_> = self
            .toasts
            .iter()
            .rev()
            .filter(|e| !e.exiting && e.dismiss_on_click && hit_test(e, x, y))
            .map(|e| e.handle)
            .collect();
        if dismissables.is_empty() {
            return false;
        }
        for h in dismissables {
            self.dismiss(h);
        }
        true
    }

    // ----- Inspect -----

    pub fn iter(&self) -> impl Iterator<Item = &ToastEntry> {
        self.toasts.iter()
    }

    pub fn contains(&self, handle: ToastHandle) -> bool {
        self.toasts.iter().any(|e| e.handle == handle)
    }

    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.toasts.len()
    }

    pub fn corner(&self) -> Corner {
        self.corner
    }

    pub fn gap_px(&self) -> f32 {
        self.gap_px
    }

    pub fn max_visible(&self) -> usize {
        self.max_visible
    }

    pub fn current_time_ms(&self) -> u64 {
        self.current_time_ms
    }

    pub fn has_animating(&self) -> bool {
        let Some(ctx) = BlincContextState::try_get() else {
            return false;
        };
        self.toasts
            .iter()
            .any(|e| ctx.query_motion(&e.motion_stable_key()).is_animating())
    }

    // ----- Render -----

    /// Build the toast layer. Each toast is wrapped in
    /// `motion_derived(motion_key)` so widgets can configure their own
    /// enter / exit. Toasts stack at the configured corner with `gap_px`
    /// between them.
    ///
    /// The tray container uses `animate_bounds(::position().snappy())`
    /// (FLIP) to smoothly reorder toasts when one in the middle
    /// auto-dismisses, instead of leaving a hole.
    pub fn build_tray_layer(&self, viewport: (f32, f32)) -> Div {
        use crate::motion::motion_derived;
        use crate::visual_animation::VisualAnimationConfig;

        // Zero-sized when empty so the tray container never blanket-absorbs
        // events from the main UI underneath. Matches OverlayStack pattern.
        if self.toasts.is_empty() || viewport.0 <= 0.0 || viewport.1 <= 0.0 {
            return Div::new()
                .id(TOAST_TRAY_LAYER_ID)
                .absolute()
                .top(0.0)
                .left(0.0)
                .w(0.0)
                .h(0.0)
                .stack_layer()
                .pointer_events_none();
        }

        // Container is full-viewport, transparent, with corner anchoring.
        // Each toast is positioned by flex-col + gap inside.
        const INSET: f32 = 16.0;
        let (anchor_top, anchor_left) = match self.corner {
            Corner::TopLeft => (Some(INSET), Some(INSET)),
            Corner::TopRight => (Some(INSET), Some(viewport.0 - INSET)),
            Corner::BottomLeft => (Some(viewport.1 - INSET), Some(INSET)),
            Corner::BottomRight => (Some(viewport.1 - INSET), Some(viewport.0 - INSET)),
        };

        let mut tray = Div::new()
            .id(TOAST_TRAY_LAYER_ID)
            .absolute()
            .flex_col()
            .gap(self.gap_px)
            .animate_bounds(
                VisualAnimationConfig::position()
                    .with_key("cn-toast-tray")
                    .snappy(),
            );
        if let Some(t) = anchor_top {
            tray = tray.top(t);
        }
        if let Some(l) = anchor_left {
            tray = tray.left(l);
        }

        // Emit toasts in insertion order; the corner positioning + flex_col
        // gap handles stacking. Each is wrapped in its own motion so the
        // widget's chosen enter/exit (typically slide-from-edge + fade)
        // runs naturally.
        for toast in self.toasts.iter().take(self.max_visible) {
            let content = (toast.content_fn)();
            tray = tray.child(motion_derived(&toast.motion_key).child(content));
        }

        tray
    }

    // ----- Config -----

    pub fn set_corner(&mut self, c: Corner) {
        self.corner = c;
        self.dirty.store(true, Ordering::Release);
    }

    pub fn set_gap_px(&mut self, gap: f32) {
        self.gap_px = gap;
    }

    pub fn set_max_visible(&mut self, max: usize) {
        self.max_visible = max;
    }

    // ----- Dirty flags -----

    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Acquire)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_toast(tray: &ToastTray, spawn_at: u64, auto_after: u64) -> ToastEntry {
        ToastEntry {
            handle: tray.allocate_handle(),
            motion_key: format!("toast-test:{}", tray.next_id.load(Ordering::Relaxed)),
            spawned_at_ms: spawn_at,
            auto_after_ms: auto_after,
            exiting: false,
            dismiss_on_click: true,
            content_fn: Arc::new(|| Div::new()),
            on_close: None,
        }
    }

    #[test]
    fn push_dismiss_basic() {
        let mut tray = ToastTray::new();
        let h = tray.push(dummy_toast(&tray, 0, 4_000));
        assert_eq!(tray.len(), 1);
        assert!(tray.contains(h));

        tray.dismiss(h);
        let entry = tray.iter().find(|e| e.handle == h).unwrap();
        assert!(entry.exiting);
    }

    #[test]
    fn auto_dismiss_via_update() {
        let mut tray = ToastTray::new();
        let h = tray.push(dummy_toast(&tray, 1_000, 4_000));

        tray.update(4_999);
        let e = tray.iter().find(|e| e.handle == h).unwrap();
        assert!(!e.exiting);

        // Tick past due-time. In tests there's no motion system, so
        // `motion_done()` returns true immediately — the toast is reaped on
        // the same update() call that fires auto-dismiss.
        tray.update(5_000);
        assert!(
            !tray.contains(h),
            "toast should be reaped on the same update() tick (instant eviction \
             when no motion system is wired)"
        );
    }

    #[test]
    fn dismiss_all_marks_every_toast() {
        let mut tray = ToastTray::new();
        let a = tray.push(dummy_toast(&tray, 0, 4_000));
        let b = tray.push(dummy_toast(&tray, 0, 4_000));
        let c = tray.push(dummy_toast(&tray, 0, 4_000));

        tray.dismiss_all();
        for h in [a, b, c] {
            let e = tray.iter().find(|e| e.handle == h).unwrap();
            assert!(e.exiting);
        }
    }

    #[test]
    fn click_dismisses_hit_toast_only() {
        let mut tray = ToastTray::new();
        let a = tray.push(dummy_toast(&tray, 0, 4_000));
        let b = tray.push(dummy_toast(&tray, 0, 4_000));

        let hit_b = move |t: &ToastEntry, _: f32, _: f32| -> bool { t.handle == b };
        let consumed = tray.handle_click_at(0.0, 0.0, &hit_b);
        assert!(consumed);

        let a_e = tray.iter().find(|e| e.handle == a).unwrap();
        let b_e = tray.iter().find(|e| e.handle == b).unwrap();
        assert!(!a_e.exiting);
        assert!(b_e.exiting);
    }

    #[test]
    fn click_without_hit_returns_false() {
        let mut tray = ToastTray::new();
        let _h = tray.push(dummy_toast(&tray, 0, 4_000));
        let no_hit = |_: &ToastEntry, _: f32, _: f32| -> bool { false };
        let consumed = tray.handle_click_at(0.0, 0.0, &no_hit);
        assert!(!consumed);
    }

    #[test]
    fn double_dismiss_is_idempotent() {
        let mut tray = ToastTray::new();
        let h = tray.push(dummy_toast(&tray, 0, 4_000));
        tray.dismiss(h);
        tray.dismiss(h);
        let e = tray.iter().find(|e| e.handle == h).unwrap();
        assert!(e.exiting);
    }
}

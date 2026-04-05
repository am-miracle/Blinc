//! System back button handler
//!
//! Register callbacks for the Android system back button / browser back.
//! Handlers are stacked — the most recently registered handler fires first.
//! If it returns `true`, the back event is consumed. If `false`, the next
//! handler in the stack is tried.
//!
//! # Example
//!
//! ```ignore
//! use blinc_layout::back_handler::{push_back_handler, pop_back_handler};
//!
//! // Push a back handler (e.g., when opening a modal)
//! let handle = push_back_handler(|| {
//!     close_modal();
//!     true // consumed — don't exit app
//! });
//!
//! // Pop when done (e.g., modal closed)
//! pop_back_handler(handle);
//! ```

use std::sync::Mutex;

type BackCallback = Box<dyn Fn() -> bool + Send + Sync>;

struct BackHandlerEntry {
    id: u64,
    callback: BackCallback,
}

static BACK_STACK: Mutex<Vec<BackHandlerEntry>> = Mutex::new(Vec::new());
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Handle returned by push_back_handler, used to remove the handler
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackHandlerHandle(u64);

/// Push a back handler onto the stack.
///
/// The callback should return `true` if it consumed the back event
/// (preventing further handlers or app exit), or `false` to pass
/// to the next handler.
pub fn push_back_handler<F>(callback: F) -> BackHandlerHandle
where
    F: Fn() -> bool + Send + Sync + 'static,
{
    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let entry = BackHandlerEntry {
        id,
        callback: Box::new(callback),
    };
    BACK_STACK.lock().unwrap().push(entry);
    BackHandlerHandle(id)
}

/// Remove a back handler by its handle
pub fn pop_back_handler(handle: BackHandlerHandle) {
    let mut stack = BACK_STACK.lock().unwrap();
    stack.retain(|e| e.id != handle.0);
}

/// Dispatch a system back event.
///
/// Tries handlers from top of stack (most recent first).
/// Returns `true` if any handler consumed the event.
/// Returns `false` if no handler consumed it (app should exit or navigate back).
pub fn dispatch_back() -> bool {
    let stack = BACK_STACK.lock().unwrap();
    for entry in stack.iter().rev() {
        if (entry.callback)() {
            return true;
        }
    }
    false
}

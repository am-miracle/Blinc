//! Hello Blinc — minimal idle-baseline reproducer.
//!
//! This is the exact app from
//! [issue #28](https://github.com/project-blinc/Blinc/issues/28) — a
//! single text label on a flat background, no animations, no input
//! handlers, no overlays, no scroll. The original report measured
//! ~25% CPU in debug / ~6% in release on Ubuntu 25.10 + Intel
//! HD Graphics 520, even when the app was unfocused and idle.
//!
//! Use this example as the canonical baseline for measuring idle
//! resource use across platforms and after framework changes.
//! Expected behavior with the current scheduler / redraw chain:
//!
//! - **CPU:** ~0% when idle and unfocused. The animation scheduler's
//!   bg thread parks on a Condvar and the main thread parks in the
//!   OS event loop; nothing wakes them until input arrives.
//! - **Idle wake-ups:** 0. None of the redraw signals (animation,
//!   overlay, css, motion, scroll, theme, cursor, pointer-query,
//!   flow) are active, so the main thread never re-requests a frame.
//! - **Memory:** dominated by GPU surfaces; scales with display
//!   resolution. On a 1366×768 panel (the issue reporter's
//!   resolution) the resident set should be in the 120–150 MB range.
//!
//! ```bash
//! cargo run -p blinc_app_examples --example hello_blinc --release
//! ```
//!
//! To capture diagnostic detail about which redraw signals (if any)
//! are firing, run with:
//!
//! ```bash
//! RUST_LOG=blinc_app::redraw_signals=trace cargo run \
//!     -p blinc_app_examples --example hello_blinc --release
//! ```
//!
//! Each frame that requests a redraw will log which of the nine
//! signals were `true`. On a quiet idle run, the trace should be
//! silent.
//!
//! ## Linux note: Wayland baseline overhead
//!
//! On a Linux/Wayland session (Ubuntu's default), winit itself has a
//! known idle-CPU baseline — see [winit#2690][1] — independent of
//! anything Blinc does. Bare wgpu apps measure ~5–8 % CPU at idle on
//! Wayland; the same app on X11 measures ~0 %. To confirm that the
//! framework-side scheduler is parked correctly (vs. the platform
//! adding overhead), force the X11 backend:
//!
//! ```bash
//! WINIT_UNIX_BACKEND=x11 cargo run \
//!     -p blinc_app_examples --example hello_blinc --release
//! ```
//!
//! [1]: https://github.com/rust-windowing/winit/issues/2690

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;

fn main() -> Result<()> {
    WindowedApp::run(WindowConfig::default(), |ctx| {
        div()
            .w(ctx.width)
            .h(ctx.height)
            .bg(Color::rgb(0.1, 0.1, 0.15))
            .justify_center()
            .items_center()
            .child(text("Hello Blinc!").size(48.0).color(Color::WHITE))
    })
}

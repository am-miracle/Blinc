//! Minimal idle-baseline reproducer (issue #28).
//!
//! ```bash
//! cargo run -p blinc_app_examples --example hello_blinc --release
//! ```
//!
//! To see which redraw signals (if any) fire on a focused-but-idle
//! window, run with:
//!
//! ```bash
//! RUST_LOG=blinc_app::redraw_signals=trace cargo run \
//!     -p blinc_app_examples --example hello_blinc --release
//! ```

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;

fn main() -> Result<()> {
    // Install a tracing subscriber that respects `RUST_LOG`. Without
    // this, the `RUST_LOG=blinc_app::redraw_signals=trace` recipe in
    // the docstring above produces no output (the events fire but
    // there's nothing listening). Defaults to the `warn` level when
    // `RUST_LOG` is unset, so a normal `cargo run` is still quiet.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

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

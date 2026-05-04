//! Minimal idle-baseline reproducer (issue #28).
//!
//! ```bash
//! cargo run -p blinc_app_examples --example hello_blinc --release
//! ```

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

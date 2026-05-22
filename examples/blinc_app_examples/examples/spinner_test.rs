//! no-web: Minimal spinner test — no scrolling, no nested layouts.
//!
//! ```bash
//! cargo run -p blinc_app_examples --example spinner_test --features cn
//! ```
//!
//! Renders the three spinner sizes at fixed center positions so we can
//! see exactly what the polygon-clipped arc looks like without scroll /
//! flex / theme complications.

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;
use blinc_cn::prelude::*;
use blinc_core::Color;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let config = WindowConfig {
        title: "Spinner Test".to_string(),
        width: 600,
        height: 300,
        ..Default::default()
    };

    WindowedApp::run(config, |ctx| {
       
        div()
            .w(ctx.width)
            .h(ctx.height)
            .bg(Color::rgb(0.1, 0.1, 0.15))
            .flex_row()
            .gap(40.0)
            .items_center()
            .justify_center()
            .child(cn::spinner().size(SpinnerSize::Small))
            .child(cn::spinner().size(SpinnerSize::Medium))
            .child(cn::spinner().size(SpinnerSize::Large))
    })
}

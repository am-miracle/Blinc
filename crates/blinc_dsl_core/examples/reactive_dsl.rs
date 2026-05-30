//! `cargo run -p blinc_dsl_core --example reactive_dsl`
//!
//! Single live panel exercising the signal-binding pipeline on
//! built-in `Div` styling props. Click `Step` to mutate `pct` /
//! `radius` / `hue` and watch the swatches react in place.

#![cfg(not(target_arch = "wasm32"))]

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;
use blinc_dsl_core::BlincDsl;

const SOURCE: &str = include_str!("reactive_dsl.blinc");

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(
                    "warn,blinc_runtime::fsm=debug,blinc_dsl_core=debug",
                )
            }),
        )
        .init();

    let dsl = BlincDsl::new().expect("BlincDsl::new");
    dsl.compile_source(SOURCE, "reactive_dsl.blinc")
        .expect("compile");

    WindowedApp::run(
        WindowConfig {
            title: "Blinc DSL — Reactive bindings".to_string(),
            width: 720,
            height: 560,
            resizable: true,
            ..Default::default()
        },
        move |ctx| {
            div()
                .w(ctx.width)
                .h(ctx.height)
                .bg(Color::rgb(0.04, 0.06, 0.1))
                .items_center()
                .justify_center()
                .child_box(dsl.view_widget())
        },
    )
}

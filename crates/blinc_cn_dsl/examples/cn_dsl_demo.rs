//! `cargo run -p blinc_cn_dsl --example cn_dsl_demo`
//!
//! Co-located cn.* + DSL signal-binding canary. Click `Step` and the
//! `cn.Progress` bar fills, the swatch background and opacity track
//! `pct` / `hue`, the second swatch's corner radius grows with
//! `radius`. All live — no `@stateful`, no `@fsm`.

#![cfg(not(target_arch = "wasm32"))]

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;
use blinc_dsl_core::BlincDsl;

const SOURCE: &str = include_str!("cn_dsl_demo.blinc");

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(
                    "warn,blinc_runtime::fsm=debug,blinc_dsl_core=debug,blinc_cn_dsl=debug",
                )
            }),
        )
        .init();

    let dsl = BlincDsl::new().expect("BlincDsl::new");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");
    dsl.compile_source(SOURCE, "cn_dsl_demo.blinc")
        .expect("compile");

    // `cn_bundle()` returns the platform-detected ThemeBundle
    // pre-loaded with `CN_STYLES`. `run_with_theme` installs it
    // into `ThemeState` before the windowed loop boots, so cn
    // widgets pull both their theme tokens (colours / spacing /
    // typography) and their shadcn-flavoured CSS from a single
    // bundle.
    WindowedApp::run_with_theme(
        WindowConfig {
            title: "Blinc DSL — cn.* + signal binding".to_string(),
            width: 720,
            height: 600,
            resizable: true,
            ..Default::default()
        },
        blinc_cn::cn_bundle(),
        blinc_theme::ColorScheme::Dark,
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

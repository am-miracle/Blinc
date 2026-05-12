//! `cargo run -p blinc_dsl_core --example counter_dsl`

#![cfg(not(target_arch = "wasm32"))]

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;
use blinc_dsl_core::BlincDsl;
use blinc_layout::syntax::{RustHighlighter, SyntaxConfig};
use blinc_layout::widgets::code;

const SOURCE: &str = include_str!("counter_dsl.blinc");

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let dsl = BlincDsl::new().expect("BlincDsl::new");
    dsl.compile_source(SOURCE, "counter_dsl.blinc")
        .expect("compile");

    WindowedApp::run(
        WindowConfig {
            title: "Blinc DSL — Counter".to_string(),
            width: 1200,
            height: 640,
            resizable: true,
            ..Default::default()
        },
        move |ctx| {
            let source_pane = code(SOURCE)
                .w_full()
                .h(ctx.height)
                .syntax(SyntaxConfig::new(RustHighlighter::new()))
                .font_size(13.0)
                .line_height(1.4)
                .padding(16.0);

            div()
                .w(ctx.width)
                .h(ctx.height)
                .bg(Color::rgb(0.05, 0.07, 0.11))
                .flex_row()
                .gap(1.0)
                .justify_between()
                .child(
                    div()
                        .overflow_scroll()
                        .w(ctx.width / 2.0)
                        .child(source_pane),
                )
                .child(
                    div()
                        .w(ctx.width / 2.0)
                        .h(ctx.height)
                        .items_center()
                        .justify_center()
                        .child_box(dsl.view_widget()),
                )
        },
    )
}

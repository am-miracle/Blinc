//! `cargo run -p blinc_dsl_core --example reactive_dsl`
//!
//! Live canary for the signal-binding pipeline on built-in `Div`
//! styling props (`opacity`, `corner_radius`, `bg`, `border_width`,
//! `border_color`). Click `Step` and watch the swatches update without
//! a subtree rebuild: the FSM action mutates the underlying signals
//! and the compositor's `apply_binding_deltas` patches the GPU
//! primitives in place.
//!
//! Source-on-the-left + live-view-on-the-right layout mirrors
//! `counter_dsl` so both demos share muscle memory.

#![cfg(not(target_arch = "wasm32"))]

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;
use blinc_dsl_core::BlincDsl;
use blinc_layout::syntax::{RustHighlighter, SyntaxConfig};
use blinc_layout::widgets::code;

const SOURCE: &str = include_str!("reactive_dsl.blinc");

fn main() -> Result<()> {
    // Default to verbose tracing so signal-binding paint paths
    // (`apply_binding_deltas`) and FSM dispatch are visible in the
    // terminal. Override with `RUST_LOG=...` for a quieter run.
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
            width: 1280,
            height: 800,
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
                        .h(ctx.height)
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

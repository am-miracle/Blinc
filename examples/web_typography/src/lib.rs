//! Web typography debug demo
//!
//! Direct port of `examples/blinc_app_examples/examples/typography_demo.rs` for
//! local wasm32 debugging. Run with:
//!
//! ```bash
//! cd examples/web_typography
//! wasm-pack build --target web --release
//! python3 -m http.server 8080
//! ```

#![cfg(target_arch = "wasm32")]

// The upstream example has `//!` inner doc comments which can't
// be included via `#[path = ...]` on a mod item. build.rs strips
// them and writes the result to $OUT_DIR/example.rs.
#[allow(dead_code, unused_imports, unused_variables, unused_mut)]
#[allow(clippy::all, clippy::pedantic)]
mod example {
    include!(concat!(env!("OUT_DIR"), "/example.rs"));
}

use blinc_app::web::WebApp;
use example::build_ui;
use wasm_bindgen::prelude::*;

const ARIAL_TTF: &[u8] = include_bytes!("../../../assets/fonts/Arial.ttf");
const FIRA_CODE_TTF: &[u8] = include_bytes!("../../../assets/fonts/FiraCode-Regular.ttf");
const JETBRAINS_MONO_TTF: &[u8] = include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf");

#[wasm_bindgen(start)]
pub fn _start() {
    console_error_panic_hook::set_once();

    tracing_wasm::set_as_global_default_with_config(
        tracing_wasm::WASMLayerConfigBuilder::new()
            .set_max_level(tracing::Level::DEBUG)
            .build(),
    );

    wasm_bindgen_futures::spawn_local(async {
        let result = WebApp::run_with_async_setup(
            "blinc-canvas",
            |app| {
                Box::pin(async move {
                    app.load_font_data(ARIAL_TTF.to_vec());
                    app.load_font_data(FIRA_CODE_TTF.to_vec());
                    app.load_font_data(JETBRAINS_MONO_TTF.to_vec());
                    Ok(())
                })
            },
            build_ui,
        )
        .await;

        if let Err(e) = result {
            web_sys::console::error_1(&format!("web_typography: WebApp::run failed: {e}").into());
        }
    });
}

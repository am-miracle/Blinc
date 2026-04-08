//! Smallest possible Blinc web app — "Hello, WebGPU!" centered on a
//! dark background.
//!
//! This is the canonical proof-of-life for the wasm32 target. There
//! are no input handlers, no animations, and no asset loading — just
//! the render path. If this draws on a real canvas in a real browser,
//! the entire wgpu / wasm-bindgen / requestAnimationFrame /
//! `WebApp::run` pipeline is alive end-to-end.
//!
//! ## Build
//!
//! ```bash
//! cd examples/web_hello
//! wasm-pack build --target web --release
//! ```
//!
//! Then serve the directory with any static HTTP server (e.g.
//! `python3 -m http.server`) and open `http://localhost:8000/` in
//! Chrome 113+ (or any browser with WebGPU enabled — see the README).
//!
//! ## Fonts
//!
//! Browser-provided fonts (system fonts, `@font-face` declarations,
//! the `FontFace` API) are NOT accessible from wgpu — they live in
//! the browser's compositor and 2D-canvas pipeline, not in the
//! WebGPU pipeline. Blinc needs the actual TTF/OTF bytes to feed
//! through `swash` for glyph rasterization. The two real options are:
//!
//! 1. **Bundle**: `include_bytes!("../fonts/Inter.ttf")` ships the
//!    font inside the wasm artifact. Simplest, but adds the font
//!    file size to the wasm bundle. This example uses option 1
//!    with a 755 KB Arial.ttf for proof-of-life.
//! 2. **Fetch**: pre-load via `WebAssetLoader::preload(&["fonts/Inter.ttf"])`
//!    before calling `WebApp::run`. This is the canonical pattern
//!    for real apps and is what Phase 6 of the rollout will document.
//!
//! ## What's deliberately missing
//!
//! - **Input** — Phase 3d wires DOM events through `EventRouter`.
//!   Until then, hovers and clicks don't reach widgets.
//! - **Resize** — Phase 3e re-reads the canvas size and reconfigures
//!   the surface. Until then, the canvas stays at whatever size CSS
//!   gave it at startup.

#![cfg(target_arch = "wasm32")]

use blinc_app::web::WebApp;
use blinc_app::windowed::WindowedContext;
use blinc_core::Color;
use blinc_layout::div::{div, Div};
use blinc_layout::text::text;
use wasm_bindgen::prelude::*;

/// Bundled font. Browsers can't hand wgpu their system fonts (those
/// live in the compositor's 2D pipeline, not in WebGPU), so the font
/// bytes have to live on the wasm side. We `include_bytes!` Arial here
/// for proof-of-life. Real apps should fetch fonts on demand via
/// `WebAssetLoader::preload` (Phase 6) instead of bundling them — at
/// 755 KB, Arial alone roughly doubles the wasm payload.
const ARIAL_TTF: &[u8] = include_bytes!("../fonts/Arial.ttf");

/// wasm-bindgen entry point. The `start` attribute makes this run
/// automatically when the browser loads the generated `.js` shim.
///
/// We schedule the actual `WebApp::run` future on the wasm-bindgen
/// futures executor instead of `.await`-ing it inline, because
/// `#[wasm_bindgen(start)]` functions can't return a `Future` directly
/// across the JS boundary on stable wasm-bindgen.
#[wasm_bindgen(start)]
pub fn _start() {
    // Install the panic hook so any Rust panic shows up in the browser
    // console with a stack trace instead of a useless `RuntimeError:
    // unreachable executed`.
    console_error_panic_hook::set_once();

    // Bridge `tracing::*` macros from the Rust crates into the browser
    // DevTools console. INFO level keeps the per-frame DEBUG lines from
    // the renderer / scheduler / text path out of the console — at
    // 60fps those drown the JS thread and hang the page. Bump to
    // DEBUG temporarily when chasing a specific bug.
    tracing_wasm::set_as_global_default_with_config(
        tracing_wasm::WASMLayerConfigBuilder::new()
            .set_max_level(tracing::Level::INFO)
            .build(),
    );

    wasm_bindgen_futures::spawn_local(async {
        // `run_with_setup` lets us touch the WebApp between init and
        // the first frame. We use the setup hook to register Arial
        // with the font registry — without this, every text element
        // shapes zero glyphs and renders nothing (the wasm32 init
        // path skips system font discovery because there's no
        // filesystem).
        let result = WebApp::run_with_setup(
            "blinc-canvas",
            |app| {
                let faces = app.load_font_data(ARIAL_TTF.to_vec());
                web_sys::console::log_1(
                    &format!("blinc_web_hello: registered {faces} font face(s) from Arial.ttf")
                        .into(),
                );
            },
            build_ui,
        )
        .await;

        if let Err(e) = result {
            // We don't have a tracing subscriber yet, so reach for
            // web-sys directly to put the error somewhere visible.
            web_sys::console::error_1(&format!("blinc_web_hello: WebApp::run failed: {e}").into());
        }
    });
}

/// User UI builder. Re-invoked by the runner whenever a rebuild is
/// requested (currently: just once at startup, since this example
/// has no state).
fn build_ui(_ctx: &mut WindowedContext) -> Div {
    div()
        .w_full()
        .h_full()
        .bg(Color::rgba(0.07, 0.07, 0.10, 1.0))
        .items_center()
        .justify_center()
        .child(
            text("Hello, WebGPU!")
                .size(32.0)
                .color(Color::rgba(0.92, 0.92, 0.95, 1.0)),
        )
}

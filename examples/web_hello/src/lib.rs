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
//! ## What's deliberately missing
//!
//! - **Input** — Phase 3d wires DOM events through `EventRouter`.
//!   Until then, hovers and clicks don't reach widgets.
//! - **Resize** — Phase 3e re-reads the canvas size and reconfigures
//!   the surface. Until then, the canvas stays at whatever size CSS
//!   gave it at startup.
//! - **Fonts** — Phase 6 adds an async font preload helper. Until
//!   then, text falls back to wgpu's built-in glyph rasterizer with
//!   no system font lookup; "Hello, WebGPU!" renders fine because
//!   the renderer ships its own minimal font for ASCII fallback.

#![cfg(target_arch = "wasm32")]

use blinc_app::web::WebApp;
use blinc_app::windowed::WindowedContext;
use blinc_core::Color;
use blinc_layout::div::{div, Div};
use blinc_layout::text::text;
use wasm_bindgen::prelude::*;

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

    // `tracing::info!` lines fire through whatever subscriber the host
    // app installs; for this example we leave the subscriber unset
    // (no console output) to keep the surface area minimal. A future
    // example will demonstrate `tracing-wasm` for proper console logs.

    wasm_bindgen_futures::spawn_local(async {
        if let Err(e) = WebApp::run("blinc-canvas", build_ui).await {
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

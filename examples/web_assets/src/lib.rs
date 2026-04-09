//! Blinc web example — fetch a font at runtime instead of bundling it.
//!
//! `web_hello` and `web_drag` both `include_bytes!("Arial.ttf")`,
//! which works but bakes ~755 KB of font data into the wasm
//! artifact. For real apps that ship more than one font (or several
//! large fonts), bundling becomes impractical fast — the wasm
//! download grows linearly with font count and the user pays for
//! every glyph the page might possibly need *before* the first
//! frame renders.
//!
//! This example uses `WebApp::run_with_async_setup` +
//! `WebAssetLoader::fetch_bytes` to fetch `fonts/Arial.ttf` over
//! HTTP at startup, then hand the bytes to `WebApp::load_font_data`.
//! The wasm artifact stays small; the font lives next to `index.html`
//! and is fetched once on the first page load (and then cached by the
//! browser like any other static asset).
//! (Plain code spans rather than intra-doc links because this crate
//! is `#![cfg(target_arch = "wasm32")]` — on a host doc build the
//! `WebApp` / `WebAssetLoader` imports are gated out and rustdoc
//! can't resolve the symbols.)
//!
//! ## Build
//!
//! ```bash
//! cd examples/web_assets
//! wasm-pack build --target web --release
//! ./serve.sh
//! # open http://localhost:8000/
//! ```
//!
//! ## What's deliberately missing
//!
//! - **Bundle-then-fetch fallback.** Real apps usually ship a tiny
//!   bundled fallback font so the first frame renders even before
//!   the fetched font lands. This example doesn't — it just shows
//!   blank text until the fetch resolves, which is fine for
//!   demonstrating the API. Apps that care about fonts-out
//!   (FOIT/FOUT) should bundle a system-ish fallback first via
//!   `app.load_font_data(BUNDLED_FALLBACK)` and *then* fetch the
//!   real font asynchronously.
//! - **Multi-font preload.** `WebAssetLoader::preload` takes an
//!   array of URLs and stores them all in the loader's cache. For
//!   N fonts you'd loop through them inside the async setup
//!   closure and call `app.load_font_data(...)` on each. This
//!   example fetches just one to keep the wiring readable.

#![cfg(target_arch = "wasm32")]

use blinc_app::web::WebApp;
use blinc_app::windowed::WindowedContext;
use blinc_app::BlincError;
use blinc_core::Color;
use blinc_layout::div::{div, Div};
use blinc_layout::text::text;
use blinc_platform_web::WebAssetLoader;
use wasm_bindgen::prelude::*;

/// URL of the font to fetch on startup. Resolved relative to the
/// page's origin (i.e. served from `examples/web_assets/fonts/`).
/// The browser caches this like any other static asset, so the
/// fetch only happens on a cold load.
const FONT_URL: &str = "fonts/Arial.ttf";

#[wasm_bindgen(start)]
pub fn _start() {
    console_error_panic_hook::set_once();

    tracing_wasm::set_as_global_default_with_config(
        tracing_wasm::WASMLayerConfigBuilder::new()
            .set_max_level(tracing::Level::INFO)
            .build(),
    );

    wasm_bindgen_futures::spawn_local(async {
        // `run_with_async_setup` is the canonical way to load
        // fetch-based assets before the first frame renders. The
        // setup closure runs once, awaits whatever it needs, and
        // returns. By the time the closure resolves, the font is
        // in the registry and the first rAF tick can shape glyphs.
        let result = WebApp::run_with_async_setup(
            "blinc-canvas",
            // The `Box::pin(async move { ... })` ceremony is the
            // stable-Rust workaround for the lack of `async
            // FnOnce`. Once async closures stabilize this drops
            // back to `|app| async move { ... }`.
            |app| {
                Box::pin(async move {
                    web_sys::console::log_1(
                        &format!("blinc_web_assets: fetching {FONT_URL}…").into(),
                    );

                    // `WebAssetLoader::fetch_bytes` is a one-shot
                    // helper that fetches a single URL and returns
                    // its bytes without keeping a copy in the
                    // loader cache. The font registry takes
                    // ownership of the bytes via
                    // `load_font_data`, so caching them on the
                    // loader side too would just double the memory.
                    let bytes = WebAssetLoader::fetch_bytes(FONT_URL)
                        .await
                        .map_err(|e| BlincError::Platform(e.to_string()))?;

                    let faces = app.load_font_data(bytes);
                    web_sys::console::log_1(
                        &format!(
                            "blinc_web_assets: registered {faces} font face(s) from {FONT_URL}"
                        )
                        .into(),
                    );
                    Ok(())
                })
            },
            build_ui,
        )
        .await;

        if let Err(e) = result {
            web_sys::console::error_1(&format!("blinc_web_assets: WebApp::run failed: {e}").into());
        }
    });
}

fn build_ui(_ctx: &mut WindowedContext) -> Div {
    div()
        .w_full()
        .h_full()
        .bg(Color::rgba(0.07, 0.07, 0.10, 1.0))
        .flex_col()
        .items_center()
        .justify_center()
        .gap_px(16.0)
        .child(
            text("Blinc · Fetched font")
                .size(28.0)
                .color(Color::rgba(0.92, 0.92, 0.95, 1.0)),
        )
        .child(
            text("This text is rendered with a font fetched at startup,")
                .size(14.0)
                .color(Color::rgba(0.65, 0.65, 0.72, 1.0)),
        )
        .child(
            text("not bundled inside the wasm artifact.")
                .size(14.0)
                .color(Color::rgba(0.65, 0.65, 0.72, 1.0)),
        )
}

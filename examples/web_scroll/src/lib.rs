//! Blinc web example — vertical scrolling list with click handlers.
//!
//! Builds on `web_hello` to exercise the parts of the runtime that
//! the bare "render one Div" example doesn't touch:
//!
//! - **Wheel events** route through `EventRouter::on_scroll` and drive
//!   the [`Scroll`] widget's physics, which produces continuous frame
//!   ticks via the animation scheduler. Proves the rAF loop ↔ scheduler
//!   ↔ scroll-physics chain end-to-end.
//! - **Click events** route through `EventRouter::on_mouse_down` and
//!   reach an `on_click` closure attached to each card. The closure
//!   just logs to the browser console for now (no state-driven
//!   rebuilds — that requires reactive-graph plumbing in the web
//!   runner that hasn't landed yet; see the README for the follow-up).
//! - **Hover** is implicit: the bounce / momentum tick keeps the rAF
//!   loop alive long enough to surface any console errors from the
//!   per-frame render path.
//!
//! ## Build
//!
//! ```bash
//! cd examples/web_scroll
//! wasm-pack build --target web --release
//! ```
//!
//! Then serve with `./serve.sh` and open `http://localhost:8000/` in
//! Chrome 113+ (or any browser with WebGPU enabled).
//!
//! ## What's deliberately missing
//!
//! - **State-driven rebuilds** (`ctx.use_state` → click handler → tree
//!   rebuild) need a Phase 1 detection pass in `WebApp::run_one_frame`
//!   that polls the same dirty flags `WindowedApp` polls
//!   ([`crates/blinc_app/src/windowed.rs:3500-3535`](../../crates/blinc_app/src/windowed.rs#L3500-L3535)).
//!   That's a small but multi-touchpoint change to `web.rs` that's not
//!   on Phase 5's critical path — the click event still *reaches* the
//!   handler today, just nothing visible changes when you click.
//! - **Touch input.** Phase 3d wired mouse / wheel / keyboard. Touch
//!   events come in a follow-up.
//! - **IME / clipboard.** Same — async-only DOM APIs that need their
//!   own layer in `web.rs`.

#![cfg(target_arch = "wasm32")]

use blinc_app::web::WebApp;
use blinc_app::windowed::WindowedContext;
use blinc_core::Color;
use blinc_layout::div::{div, Div};
use blinc_layout::prelude::scroll;
use blinc_layout::text::text;
use wasm_bindgen::prelude::*;

/// Bundled font. Browsers can't hand wgpu their system fonts (those
/// live in the compositor's 2D pipeline, not in WebGPU), so the font
/// bytes have to live on the wasm side. We share Arial with `web_hello`
/// for now — Phase 6 will introduce the asset-preload helper for
/// fonts that don't ship inside the wasm artifact.
const ARIAL_TTF: &[u8] = include_bytes!("../../web_hello/fonts/Arial.ttf");

/// wasm-bindgen entry point. The `start` attribute makes this run
/// automatically when the browser loads the generated `.js` shim.
#[wasm_bindgen(start)]
pub fn _start() {
    // Install the panic hook so any Rust panic shows up in the browser
    // console with a stack trace instead of a useless `RuntimeError:
    // unreachable executed`.
    console_error_panic_hook::set_once();

    // Bridge `tracing::*` macros from the Rust crates into the browser
    // DevTools console. INFO level keeps the per-frame DEBUG lines from
    // the renderer / scheduler / text path out of the console — at
    // 60fps those drown the JS thread and hang the page.
    tracing_wasm::set_as_global_default_with_config(
        tracing_wasm::WASMLayerConfigBuilder::new()
            .set_max_level(tracing::Level::INFO)
            .build(),
    );

    wasm_bindgen_futures::spawn_local(async {
        let result = WebApp::run_with_setup(
            "blinc-canvas",
            |app| {
                let faces = app.load_font_data(ARIAL_TTF.to_vec());
                web_sys::console::log_1(
                    &format!("blinc_web_scroll: registered {faces} font face(s) from Arial.ttf")
                        .into(),
                );
            },
            build_ui,
        )
        .await;

        if let Err(e) = result {
            web_sys::console::error_1(&format!("blinc_web_scroll: WebApp::run failed: {e}").into());
        }
    });
}

/// User UI builder. Re-invoked by the runner whenever a rebuild is
/// requested (currently: just the initial mount, since this example
/// has no reactive state — see the module docs).
fn build_ui(ctx: &mut WindowedContext) -> Div {
    // Card colors hand-picked for visible contrast on the dark
    // background. We loop over them with `idx % palette.len()` so the
    // list scales to any number of cards without having to enumerate
    // colors per row.
    let palette: [Color; 5] = [
        Color::rgba(0.32, 0.55, 0.92, 1.0), // blue
        Color::rgba(0.93, 0.45, 0.55, 1.0), // pink
        Color::rgba(0.40, 0.85, 0.55, 1.0), // green
        Color::rgba(0.95, 0.70, 0.30, 1.0), // amber
        Color::rgba(0.75, 0.45, 0.95, 1.0), // violet
    ];

    // 24 cards is enough that the scrollable content overflows the
    // viewport at any reasonable browser window size, so the user can
    // see the scroll happen.
    const CARD_COUNT: usize = 24;

    let mut content = div().w_full().flex_col().p(20.0).gap(12.0);

    for idx in 0..CARD_COUNT {
        let accent = palette[idx % palette.len()];
        let label = format!("Card {}", idx + 1);
        let card_index = idx + 1;

        content = content.child(
            div()
                .w_full()
                .h(80.0)
                .bg(Color::rgba(0.16, 0.16, 0.21, 1.0))
                .rounded(12.0)
                .p(16.0)
                .flex_row()
                .items_center()
                .gap(12.0)
                // Accent strip on the left edge — visually separates
                // cards and gives the wheel-driven scroll something
                // colorful to move past.
                .child(div().w(6.0).h(48.0).bg(accent).rounded(3.0))
                .child(
                    text(&label)
                        .size(20.0)
                        .color(Color::rgba(0.95, 0.95, 0.97, 1.0)),
                )
                // Logging-only click handler. Real state-driven UI
                // updates need the Phase 1 rebuild detection in
                // `WebApp::run_one_frame` (see module docs).
                .on_click(move |_| {
                    web_sys::console::log_1(
                        &format!("blinc_web_scroll: clicked card #{card_index}").into(),
                    );
                }),
        );
    }

    // Outer page wrapper: full-bleed dark background, the Scroll
    // widget filling the viewport with a small inset gutter so the
    // scroll edges are visually distinct from the page edges.
    div()
        .w_full()
        .h_full()
        .bg(Color::rgba(0.07, 0.07, 0.10, 1.0))
        .flex_col()
        .items_center()
        .p(16.0)
        .child(
            text("Blinc · Wheel-scroll demo")
                .size(22.0)
                .color(Color::rgba(0.92, 0.92, 0.95, 1.0)),
        )
        .child(
            text(
                "Scroll with the mouse wheel or trackpad. Click a card — see the browser console.",
            )
            .size(13.0)
            .color(Color::rgba(0.65, 0.65, 0.72, 1.0)),
        )
        .child(
            div()
                .w(ctx.width - 32.0)
                .h(ctx.height - 96.0)
                .rounded(16.0)
                .bg(Color::rgba(0.11, 0.11, 0.14, 1.0))
                .child(
                    scroll()
                        .w(ctx.width - 32.0)
                        .h(ctx.height - 96.0)
                        .rounded(16.0)
                        .bg(Color::rgba(0.11, 0.11, 0.14, 1.0))
                        .child(content)
                        .on_scroll(|e| {
                            // INFO so it shows up at the default
                            // tracing-wasm level. Drop to DEBUG once
                            // we've validated the wheel pipeline.
                            tracing::info!(
                                "blinc_web_scroll: scroll delta=({:.1}, {:.1})",
                                e.scroll_delta_x,
                                e.scroll_delta_y
                            );
                        }),
                ),
        )
}

//! Router navigation reproducer — regression test for GH #35.
//!
//! Pre-fix, `router.push` / `replace` / `back` / `forward` silently
//! no-op'd: the methods mutated `RouterInner.current_match` but never
//! flipped the global rebuild flag, so the `Div` returned by the
//! initial `outlet()` call stayed wired into the tree against the old
//! route. Clicking "Go to Counter" looked dead.
//!
//! Post-fix, each navigation method calls
//! `BlincContextState::request_rebuild()` (the same hook
//! `State::set_rebuild` uses), the next frame re-runs the UI builder,
//! and `outlet()` reads the new `current_match`.
//!
//! What you should see:
//! - Window opens on "Home Page" with a "Go to Counter" button.
//! - Clicking it swaps the view to "Counter Page" with a "Go to home"
//!   button that swaps back.
//! - Visiting an unmapped path (none wired in this demo, but the
//!   `not_found` view is registered) would land on the 404 view with
//!   a "Go Home" button that `replace`s back to `/`.
//!
//! Not certified on wasm32 — `blinc_cn` + the router's
//! `BlincContextState::request_rebuild` path haven't been exercised
//! together in a browser build. Desktop only for now.
//!
//! Run with:
//! ```bash
//! cargo run -p blinc_app_examples --example router_navigation --features cn
//! ```

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;
use blinc_cn::cn;
use blinc_router::{Route, RouteContext, RouterBuilder, use_router};

fn home_page(_ctx: RouteContext) -> Div {
    let router = use_router();

    div()
        .flex_col()
        .items_center()
        .gap_px(16.0)
        .child(cn::h1("Home Page").color(Color::WHITE))
        .child(cn::button("Go to Counter").on_click(move |_| router.push("/counter")))
}

fn counter_page(_ctx: RouteContext) -> Div {
    let router = use_router();

    div()
        .flex_col()
        .items_center()
        .gap_px(16.0)
        .child(cn::h1("Counter Page").color(Color::WHITE))
        .child(cn::button("Go to home").on_click(move |_| router.push("/")))
}

fn not_found(_ctx: RouteContext) -> Div {
    let router = use_router();

    div()
        .flex_col()
        .items_center()
        .gap_px(12.0)
        .child(cn::h1("404").color(Color::WHITE))
        .child(cn::muted("Page not found").color(Color::WHITE))
        .child(cn::button("Go Home").on_click(move |_| router.replace("/")))
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = WindowConfig {
        title: "Router Navigation — GH #35".to_string(),
        width: 400,
        height: 350,
        resizable: true,
        ..Default::default()
    };

    let router = RouterBuilder::new()
        .route(Route::new("/").name("home").view(home_page))
        .route(Route::new("/counter").name("counter").view(counter_page))
        .not_found(not_found)
        .initial("/")
        .build();

    WindowedApp::run(config, move |ctx| {
        if ctx.rebuild_count == 0 {
            ctx.add_css(blinc_cn::cn_styles::CN_STYLES);
        }
        div()
            .flex_col()
            .gap(2.0)
            .w(ctx.width)
            .h(ctx.height)
            .bg(Color::rgb(0.06, 0.06, 0.09))
            .justify_center()
            .items_center()
            .child(cn::h1("Playground App").color(Color::WHITE))
            .child(router.outlet())
    })
}

#[cfg(target_arch = "wasm32")]
fn main() {}

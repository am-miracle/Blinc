//! no-web: Transparent window — regression test for GH #34.
//!
//!
//!
//! Linux Mesa surfaces only expose `[Opaque, PreMultiplied]`; the
//! previous hardcoded `PostMultiplied` selection panicked on Linux
//! during `Surface::configure`. The fix queries the surface's actual
//! supported alpha modes (`windowed.rs::pick_alpha_mode`) and falls
//! back to `PreMultiplied` when `PostMultiplied` isn't there.
//!
//! What you should see when this runs:
//! - A normal window with a translucent rounded card.
//! - The desktop behind the window shows through the card's
//!   `rgba(_, _, _, 0.78)` background.
//! - No wgpu validation panic at startup.
//!
//! Run with: `cargo run -p blinc_app_examples --example transparent_window`

use blinc_app::prelude::*;
use blinc_app::windowed::{WindowedApp, WindowedContext};

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let config = WindowConfig {
        title: "Transparent Window — GH #34".to_string(),
        width: 480,
        height: 320,
        resizable: true,
        // Keep decorations on — the title bar gives users a drag
        // handle. `decorations: false` is supported but pairing it
        // with `transparent: true` removes every drag affordance on
        // macOS unless the app wires `Window::drag_window()` to a
        // mouse-down handler. Outside the scope of this regression
        // example.
        decorations: true,
        transparent: true,
        ..Default::default()
    };

    WindowedApp::run(config, build_ui)
}

#[cfg(target_arch = "wasm32")]
fn main() {}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    div()
        .w(ctx.width)
        .h(ctx.height)
        // Fully transparent root — the desktop shows through.
        .bg(Color::rgba(0.0, 0.0, 0.0, 0.0))
        .flex_col()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(360.0)
                .h(200.0)
                .bg(Color::rgba(0.08, 0.10, 0.16, 0.78))
                .rounded(20.0)
                .border(1.0, Color::rgba(1.0, 1.0, 1.0, 0.18))
                .flex_col()
                .items_center()
                .justify_center()
                .gap_px(12.0)
                .child(
                    text("Transparent window")
                        .color(Color::rgba(1.0, 1.0, 1.0, 0.95))
                        .size(22.0),
                )
                .child(
                    text("The desktop should bleed through the card background.")
                        .color(Color::rgba(1.0, 1.0, 1.0, 0.72))
                        .size(13.0),
                ),
        )
}

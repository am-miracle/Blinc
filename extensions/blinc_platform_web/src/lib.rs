//! Blinc Web Platform
//!
//! HtmlCanvasElement integration, browser event conversion, and
//! fetch-based asset loading for `wasm32-unknown-unknown`. The crate is
//! a sibling of [`blinc_platform_desktop`], [`blinc_platform_android`],
//! [`blinc_platform_ios`], and [`blinc_platform_harmony`] — it owns
//! everything that needs `wasm-bindgen` / `web-sys` so the rest of the
//! Blinc workspace stays free of JS bindings.
//!
//! # Architecture
//!
//! The web target is conceptually identical to the other platforms:
//!
//! - **Surface**: a `<canvas>` element passed to wgpu via
//!   `SurfaceTarget::Canvas`.
//! - **Input**: browser DOM events (`mousemove`, `mousedown`, …) converted
//!   to `blinc_platform::InputEvent` and dispatched through the same
//!   `EventRouter` desktop / mobile use.
//! - **Frame loop**: `window.requestAnimationFrame(...)` driving the same
//!   5-phase pipeline as `windowed.rs`.
//! - **Assets**: pre-loaded via `fetch()` into an in-memory cache,
//!   because `AssetLoader::load` is sync.
//!
//! On non-wasm hosts every type still exists (so `cargo check` from a
//! desktop box doesn't error), but the methods that touch `web-sys`
//! return [`PlatformError::Unsupported`].
//!
//! # Usage
//!
//! ```ignore
//! use blinc_app::web::WebApp;
//!
//! #[wasm_bindgen(start)]
//! pub async fn main() -> Result<(), wasm_bindgen::JsValue> {
//!     console_error_panic_hook::set_once();
//!     WebApp::run("blinc-canvas", |_ctx| {
//!         div()
//!             .w_full()
//!             .h_full()
//!             .bg(Color::rgba(0.07, 0.07, 0.10, 1.0))
//!             .items_center()
//!             .justify_center()
//!             .child(text("Hello, WebGPU!").size(24.0).color(Color::WHITE))
//!     })
//!     .await
//!     .map_err(|e| wasm_bindgen::JsValue::from_str(&format!("{e}")))
//! }
//! ```
//!
//! # Building
//!
//! ```bash
//! wasm-pack build examples/web_hello --target web --release
//! ```

#![allow(clippy::needless_lifetimes)]

pub mod assets;
pub mod input;
pub mod window;

#[cfg(target_arch = "wasm32")]
pub mod wasm_bindgen_glue;

// Public API
pub use assets::WebAssetLoader;
pub use input::{
    convert_key_from_dom, convert_mouse_button, convert_pointer_button, modifiers_from_keyboard,
    modifiers_from_mouse,
};
pub use window::WebWindow;

#[cfg(target_arch = "wasm32")]
pub use wasm_bindgen_glue::install_panic_hook;

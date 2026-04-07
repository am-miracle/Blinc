//! wasm-bindgen entry-point glue.
//!
//! Currently a single helper that installs the panic hook so panics
//! show up in the browser DevTools console with a stack trace.
//!
//! Apps don't need to call this directly — `WebApp::run` (in
//! `blinc_app::web`) calls it on entry. The function is exposed
//! publicly so users who skip the high-level runner and drive the
//! frame loop themselves can still get nice panic messages.

/// Install the wasm panic hook (idempotent).
///
/// Without this, a Rust panic shows up in the browser as a useless
/// `RuntimeError: unreachable executed`. With it, the panic message
/// and a JS-side stack trace land in the DevTools console.
pub fn install_panic_hook() {
    console_error_panic_hook::set_once();
}

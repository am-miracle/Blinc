//! `blinc_noto_emoji` — drop-in NotoColorEmoji fallback for
//! `blinc_text`.
//!
//! # What it does
//!
//! This crate bundles a ~142 KB subset of Google's NotoColorEmoji
//! (CBDT/CBLC color bitmap tables retained) and registers it with
//! [`blinc_text::global_font_registry`] at binary initialisation via
//! a [`ctor::ctor`] function. The practical effect: **adding this
//! crate to your `Cargo.toml` makes color emoji work on every
//! platform**, including wasm, without writing a single line of
//! code.
//!
//! ```toml
//! [dependencies]
//! blinc_noto_emoji = "0.5"
//! ```
//!
//! That's it. No `use blinc_noto_emoji` in your source, no
//! `init()` call, no script to run. The next `text("Hello 🚀")`
//! that `blinc_app` renders will find the bundled font via the
//! existing fallback chain in
//! [`blinc_text::FontRegistry::load_emoji_font`].
//!
//! # Naming convention — `blinc_<font>_emoji` add-ons
//!
//! This is the first entry in a family of optional font add-on
//! crates. The convention is `blinc_<font>_emoji`, each one:
//!
//! - Bundles a pre-subsetted font as a `const &[u8]`.
//! - Exposes a public `register()` function that pushes the
//!   bytes into `blinc_text::global_font_registry()` via
//!   `FontRegistry::load_font_data`.
//! - Declares a `#[ctor::ctor]` wrapper so adding the crate to
//!   `Cargo.toml` is the only user-visible step.
//!
//! Example future siblings:
//!
//! - `blinc_twemoji_emoji` — Twitter's open-source COLRv0 emoji
//! - `blinc_fluent_emoji` — Microsoft's Fluent Emoji (flat, 3D)
//! - `blinc_openmoji` — OpenMoji's CC-BY-SA set
//!
//! Applications can depend on exactly one add-on, and the choice
//! is a single-line `Cargo.toml` change.
//!
//! # Why a separate crate
//!
//! Emoji rendering is an opt-in concern. Many Blinc applications
//! never render an emoji and would pay a ~142 KB binary-size cost
//! for nothing if the font lived in `blinc_text` itself. Splitting
//! the bundle into a plugin crate:
//!
//! - Keeps `blinc_text` lean for apps that don't need emoji.
//! - Gives users a clean way to **swap** the bundled font (see the
//!   naming convention above) — just depend on a different plugin
//!   crate with the same auto-register shape.
//! - Encourages the "Blinc is a framework with composable
//!   plugins" direction the roadmap is heading.
//!
//! # Auto-registration details
//!
//! The [`ctor::ctor`] attribute marks a function to run during
//! binary init via platform-specific mechanisms:
//!
//! | Platform              | Mechanism                   |
//! |-----------------------|-----------------------------|
//! | Linux / Android / BSDs| `.init_array` section       |
//! | macOS / iOS           | `__DATA,__mod_init_func`    |
//! | Windows               | TLS callback                |
//! | wasm32                | `linker_init`               |
//!
//! Because the `ctor` function is marked `#[used]` internally, the
//! symbol is preserved through dead-code elimination even when the
//! user's source never names the `blinc_noto_emoji` crate — adding
//! the dependency to `Cargo.toml` is enough.
//!
//! # What gets loaded
//!
//! The bundled subset covers **90 codepoints**: Latin-1 Supplement
//! punctuation (`°`, `±`, `×`), General Punctuation (`–`, `—`, `…`),
//! Arrows (`←`, `↑`, `→`, `↓`, `↻`), Misc Technical (`⌘`, `⏳`),
//! Geometric Shapes (`▶`, `▼`), Miscellaneous Symbols (`☀`, `☕`,
//! `⛅`), Dingbats (`✅`, `✊`, `✌`, `✓`, `✨`, `❄`, `❌`, `❤`),
//! Emoticons (`😀`–`😆`, `🙌`), and the big
//! Miscellaneous Symbols and Pictographs / Supplemental Symbols
//! and Pictographs / Transport and Map Symbols blocks for common
//! faces, food, and animals — enough to cover the Blinc example
//! gallery and the average app's chrome.
//!
//! If your app uses codepoints outside that set, you have two
//! options:
//!
//! 1. Rebuild the subset with a superset of the codepoints — run
//!    `scripts/regen-emoji-subset.sh` in the Blinc repo against
//!    your own source tree, then publish a fork of this crate with
//!    the regenerated bundle.
//! 2. Wait for the lazy per-codepoint loader planned in ROADMAP
//!    section 4.4, which will fetch glyphs over the network on
//!    demand instead of relying on a pre-built subset.

use std::sync::Arc;

/// The bundled NotoColorEmoji subset as a static byte slice.
///
/// Exposed as `pub` so downstream code can, for example, pre-warm
/// a custom font registry before `blinc_text`'s global one is
/// ever touched, or embed the same bytes into a different
/// registry instance.
pub const NOTO_COLOR_EMOJI_SUBSET: &[u8] = include_bytes!("../assets/NotoColorEmoji-subset.ttf");

/// The family name the bundled font registers itself under inside
/// `blinc_text`'s fontdb. `blinc_text::FontRegistry::load_emoji_font`
/// already tries this name as part of its non-Apple fallback chain,
/// so the plugin's contribution is picked up automatically once
/// [`register`] has run.
pub const FAMILY_NAME: &str = "Noto Color Emoji";

/// Idempotent manual registration hook.
///
/// Called automatically by the [`auto_register`] `#[ctor]` function
/// — applications normally never need to call this themselves. It's
/// exposed `pub` as an escape hatch for:
///
/// - Test harnesses that spin up a fresh `FontRegistry` and want to
///   prime it deterministically.
/// - Applications that build their own `FontRegistry` instance
///   instead of using [`blinc_text::global_font_registry`].
///
/// Calling this multiple times is safe; `fontdb` de-duplicates
/// identical face data internally.
pub fn register() {
    let registry: Arc<std::sync::Mutex<blinc_text::FontRegistry>> =
        blinc_text::global_font_registry();
    let Ok(mut registry) = registry.lock() else {
        tracing::warn!(
            "blinc_noto_emoji: global font registry mutex poisoned; \
             skipping NotoColorEmoji subset registration"
        );
        return;
    };
    let loaded = registry.load_font_data(NOTO_COLOR_EMOJI_SUBSET.to_vec());
    if loaded == 0 {
        tracing::warn!(
            "blinc_noto_emoji: NotoColorEmoji subset bytes rejected by fontdb \
             (expected at least one face registered)"
        );
    } else {
        tracing::debug!(
            "blinc_noto_emoji: registered NotoColorEmoji subset ({} face{})",
            loaded,
            if loaded == 1 { "" } else { "s" }
        );
    }
}

// -------------------------------------------------------------------
// Auto-registration hooks
// -------------------------------------------------------------------
//
// `ctor` covers every native target (Linux, macOS, iOS, Android,
// Windows, BSDs). For wasm32 we use `#[wasm_bindgen(start)]` instead,
// which wasm-bindgen wires into the generated JS glue so the function
// runs when the wasm module is instantiated. Either way the net
// effect is the same: by the time the user's code runs, this crate
// has already registered its bundled font with
// `blinc_text::global_font_registry`.

/// Native binary-init hook. Runs before `main` via platform-
/// specific init-array / mod_init_func / TLS callback mechanisms.
/// `#[used]` (injected by the `ctor` attribute) keeps the linker
/// from stripping this function even when the user's source never
/// names the `blinc_noto_emoji` crate — adding the dependency to
/// `Cargo.toml` is enough.
#[cfg(not(target_arch = "wasm32"))]
#[ctor::ctor]
fn auto_register_native() {
    register();
}

/// Wasm module-init hook. `wasm-bindgen` collects every
/// `#[wasm_bindgen(start)]` function it finds in the crate graph
/// during `wasm-pack build` and runs each one from the generated
/// JS glue when the wasm module is instantiated — the wasm
/// equivalent of `#[ctor::ctor]`. Same auto-registration semantics
/// as the native hook above.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn auto_register_wasm() {
    register();
}

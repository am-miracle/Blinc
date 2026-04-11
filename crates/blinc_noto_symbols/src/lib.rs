//! `blinc_noto_symbols` — drop-in text/symbol fallback for
//! `blinc_text`.
//!
//! # What it does
//!
//! This crate bundles a single merged subset of Google's Noto Sans
//! Regular + Noto Sans Math Regular covering the non-emoji special
//! characters that `blinc_noto_emoji` does not carry: math
//! operators (`∇`, `∝`, `∑`), full-width arrows (`⇐`, `⇒`, `⇔`, `↵`),
//! currency symbols (`₹`, `₱`, `₩`), and the Latin-1 Supplement
//! punctuation block (`°`, `±`, `×`, `÷`). `pyftmerge` combines
//! the two source subsets into one `fontTools`-compatible face;
//! `scripts/rename-font-family.py` rewrites its `name` table so
//! `fontdb` registers it under the blinc-specific family name
//! "Blinc Noto Symbols" (see [`FAMILY_NAME`]), preventing
//! `blinc_text`'s generic sans-serif fallback chain from
//! accidentally picking the subset as the *primary* text font.
//!
//! The single public [`register`] function pushes the merged bytes
//! into [`blinc_text::global_font_registry`]. From there,
//! `blinc_text::FontRegistry::load_symbol_font` consults the
//! "Blinc Noto Symbols" family at the top of its platform-agnostic
//! fallback chain, so every missing-glyph lookup that the renderer
//! routes through the symbol font finds a hit.
//!
//! # How it gets called
//!
//! The `blinc_app` web runner (`crates/blinc_app/src/web.rs`)
//! calls [`register`] once, early in `WebApp::new`. Outside the
//! web runner (tests, custom runners, native apps that want the
//! same bundled font), [`register`] is a public function you can
//! call at any point after the process starts. It is safe to call
//! multiple times — `fontdb` de-duplicates identical face data
//! internally.
//!
//! # Why a single merged face
//!
//! `blinc_text::FontRegistry::load_symbol_font` returns *one*
//! font face to the renderer, which then consults that face for
//! every non-primary glyph. Shipping two independent subsets in
//! this crate would only let the renderer see one of them, so the
//! crate pre-merges NotoSans and NotoSansMath at build time via
//! `pyftmerge`. On cmap conflicts NotoSans wins (we list it first
//! in the merge), contributing Latin-1 / currency; NotoSansMath
//! fills in the math operators / double arrows NotoSans doesn't
//! own. The merged face inherits NotoSans's metrics (line height,
//! ascent/descent), which keeps shaping consistent for
//! Latin-adjacent fallback use.
//!
//! # Naming convention
//!
//! Sibling of [`blinc_noto_emoji`](https://crates.io/crates/blinc_noto_emoji)
//! in the `blinc_<font>_*` add-on family. Each crate in the family:
//!
//! - Bundles a pre-subsetted font as a `const &[u8]`.
//! - Exposes a public `register()` function that pushes the bytes
//!   into `blinc_text::global_font_registry()` via
//!   `FontRegistry::load_font_data`.
//! - Gets called from the platform runner(s) that want the font
//!   live, typically under a Cargo feature gate so apps that don't
//!   need the fallback don't pay the binary-size cost.
//!
//! # What gets loaded
//!
//! The bundled subset is generated from the codepoints the Blinc
//! workspace examples actually reference (via `tools/emoji-scan`),
//! with HTML-entity decoding so `&nabla;` / `&#8377;` literals in
//! source contribute their decoded Unicode codepoints to the
//! harvest. Only glyphs that the merged NotoSans + NotoSansMath
//! cmap can provide end up in the output — the rest fall through
//! to the emoji font bundled by `blinc_noto_emoji`. Current
//! coverage sits at ~85 codepoints in a ~13 KB file.
//!
//! If your app uses codepoints outside that set, rebuild the
//! subset locally:
//!
//! ```bash
//! ./scripts/regen-symbols-subset.sh /path/to/your/app/src
//! ```
//!
//! then publish a fork of this crate with the regenerated bundle.

/// The bundled, merged Noto Sans + Noto Sans Math subset as a
/// static byte slice.
///
/// Exposed as `pub` so downstream code can, for example, pre-warm
/// a custom font registry before `blinc_text`'s global one is
/// ever touched, or embed the same bytes into a different
/// registry instance.
pub const BLINC_NOTO_SYMBOLS_SUBSET: &[u8] =
    include_bytes!("../assets/BlincNotoSymbols-subset.ttf");

/// The family name the merged subset reports in its `name` table.
/// `blinc_text::FontRegistry::load_symbol_font` lists this name at
/// the top of its platform-agnostic fallback chain, so any
/// missing-glyph lookup that the renderer routes through the
/// symbol font finds the bundled subset before walking the
/// platform-specific entries.
pub const FAMILY_NAME: &str = "Blinc Noto Symbols";

/// Install the bundled merged subset into
/// [`blinc_text::global_font_registry`].
///
/// Safe to call multiple times — `fontdb` de-duplicates identical
/// face data internally, so a second call is a cheap no-op. The
/// platform runner typically invokes this once during init; tests
/// and custom runners can call it at any point before the first
/// text-shaping pass.
pub fn register() {
    let registry = blinc_text::global_font_registry();
    let Ok(mut registry) = registry.lock() else {
        tracing::warn!(
            "blinc_noto_symbols: global font registry mutex poisoned; skipping symbol registration"
        );
        return;
    };
    let loaded = registry.load_font_data(BLINC_NOTO_SYMBOLS_SUBSET.to_vec());
    if loaded == 0 {
        tracing::warn!(
            "blinc_noto_symbols: merged subset bytes rejected by fontdb \
             (expected at least one face registered)"
        );
    } else {
        tracing::debug!(
            "blinc_noto_symbols: registered Blinc Noto Symbols subset ({} face{})",
            loaded,
            if loaded == 1 { "" } else { "s" }
        );
    }
}

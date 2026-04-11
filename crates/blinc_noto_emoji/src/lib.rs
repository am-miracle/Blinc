//! `blinc_noto_emoji` — drop-in NotoColorEmoji fallback for
//! `blinc_text`.
//!
//! # What it does
//!
//! This crate bundles a ~142 KB subset of Google's NotoColorEmoji
//! (CBDT/CBLC color bitmap tables retained) and ships a single
//! [`register`] function that pushes those bytes into
//! [`blinc_text::global_font_registry`]. Once registered, the font
//! becomes visible to `blinc_text`'s existing non-Apple emoji
//! fallback chain inside `FontRegistry::load_emoji_font`, so any
//! subsequent `text("Hello 🚀")` finds a color glyph instead of
//! rendering `.notdef`.
//!
//! # How it gets called
//!
//! The `blinc_app` web runner (`crates/blinc_app/src/web.rs`) calls
//! [`register`] once, early in `WebApp::new`, before the first
//! `BlincApp::with_canvas` await point. End-users never touch this
//! crate directly — adding it to their `Cargo.toml` (via the
//! `blinc_app` `web` feature) is enough:
//!
//! ```toml
//! [dependencies]
//! blinc_app = { version = "0.5", features = ["web"] }
//! ```
//!
//! Outside the web runner (tests, custom runners, native apps that
//! want the same bundled font), [`register`] is a public function
//! you can call at any point after the process starts. It is safe
//! to call multiple times — `fontdb` de-duplicates identical face
//! data internally.
//!
//! # Naming convention — `blinc_<font>_emoji` add-ons
//!
//! This is the first entry in a family of optional font add-on
//! crates. The convention is `blinc_<font>_emoji`, each one:
//!
//! - Bundles a pre-subsetted font as a `const &[u8]`.
//! - Exposes a public `register()` function that pushes the bytes
//!   into `blinc_text::global_font_registry()` via
//!   `FontRegistry::load_font_data`.
//! - Gets called from the platform runner(s) that want the font
//!   live, typically under a Cargo feature gate so apps that don't
//!   need emoji don't pay the binary-size cost.
//!
//! Example future siblings:
//!
//! - `blinc_twemoji_emoji` — Twitter's open-source COLRv0 emoji
//! - `blinc_fluent_emoji` — Microsoft's Fluent Emoji (flat, 3D)
//! - `blinc_openmoji` — OpenMoji's CC-BY-SA set
//!
//! Applications can depend on exactly one add-on, and the choice is
//! a single-line `Cargo.toml` feature change.
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
//!   crate with the same `register` shape.
//! - Encourages the "Blinc is a framework with composable plugins"
//!   direction the roadmap is heading.
//!
//! # What gets loaded
//!
//! The bundled subset covers **90 codepoints**: Latin-1 Supplement
//! punctuation (`°`, `±`, `×`), General Punctuation (`–`, `—`, `…`),
//! Arrows (`←`, `↑`, `→`, `↓`, `↻`), Misc Technical (`⌘`, `⏳`),
//! Geometric Shapes (`▶`, `▼`), Miscellaneous Symbols (`☀`, `☕`,
//! `⛅`), Dingbats (`✅`, `✊`, `✌`, `✓`, `✨`, `❄`, `❌`, `❤`),
//! Emoticons (`😀`–`😆`, `🙌`), and the big
//! Miscellaneous Symbols and Pictographs / Supplemental Symbols and
//! Pictographs / Transport and Map Symbols blocks for common faces,
//! food, and animals — enough to cover the Blinc example gallery
//! and the average app's chrome.
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

/// Install the bundled NotoColorEmoji subset into
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
            "blinc_noto_emoji: global font registry mutex poisoned; skipping emoji registration"
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

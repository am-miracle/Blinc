# blinc-emoji-scan

AST-based scanner that walks Rust sources and emits the set of
non-ASCII codepoints used in string / char literals. Feeds the emoji
font subsetter so Blinc's bundled fallback emoji font only contains
glyphs the application actually uses.

## Why

Blinc's web target and non-Apple native targets don't ship with a
system emoji font, so text like `"🚀"` renders as a `.notdef` box. The
obvious fix is bundling a color-emoji font (e.g. NotoColorEmoji), but
the full font is ~10 MB — too heavy for a wasm binary where every
kilobyte matters.

`blinc-emoji-scan` harvests the codepoint set the application
*actually* uses at build time, and a downstream subsetter produces a
tiny subset font containing just those glyphs (~2–50 KB in practice).

## Why AST

A byte-level regex would also pick up non-ASCII characters in:

- Identifiers (contributor names, unicode variable names)
- Doc comments (`/// © 2026 ...`)
- Attribute arguments (`#[doc = "..."]`)
- File paths passed to macros
- Example test data that never reaches the renderer

None of those end up as glyphs in the UI, but they would inflate the
subset. Parsing with `syn` and visiting only `Lit::Str` / `Lit::Char`
nodes keeps the harvested set tight.

## Usage

```bash
# Print a human-readable report to stdout
cargo run -p blinc-emoji-scan -- examples/blinc_app_examples/examples

# Write a sorted codepoint list for a subsetter (pyftsubset,
# harfbuzz-subset, Typst's `subsetter` crate, etc.)
cargo run -p blinc-emoji-scan -- \
    --output target/emoji/codepoints.txt \
    crates
```

The `--output` format is one `U+XXXX` per line, sorted and
deduplicated — the format `pyftsubset --unicodes-file=` accepts
directly.

Multiple input paths are merged into one set.

## Pipeline

```
┌────────────────────────┐
│ Rust source files      │
│  (.rs)                 │
└──────────┬─────────────┘
           │
           ▼
┌────────────────────────┐
│ blinc-emoji-scan       │   AST walk via `syn`
│  (this tool)           │   Extracts Lit::Str / Lit::Char
└──────────┬─────────────┘
           │
           ▼
┌────────────────────────┐
│ codepoints.txt         │   One `U+XXXX` per line
└──────────┬─────────────┘
           │
           ▼
┌────────────────────────┐
│ Font subsetter         │   pyftsubset / hb-subset /
│  (pyftsubset, etc.)    │   Typst `subsetter`
└──────────┬─────────────┘
           │
           ▼
┌────────────────────────┐
│ noto-emoji-subset.ttf  │   ~2–50 KB
│  (bundled by blinc_text)│
└────────────────────────┘
```

## Known limitations

- **Format strings**: `format!("Uploaded {} files 🎉", n)` works
  because the literal fragment `"Uploaded {} files 🎉"` is captured.
  But `format!("Uploaded {} files {}", n, "🎉")` works too because
  both literals are visited. Only codepoints embedded in runtime
  values (e.g. JSON loaded from the network, user input) are missed.
- **Byte and C strings**: skipped. They can't end up in the text
  layout tree — they're always either ASCII or `\u{}` escapes that
  represent bytes, not glyphs.
- **Macro bodies**: parsed via `syn::Item::Macro` but the inner token
  stream isn't recursively literal-scanned, because macros expand to
  arbitrary trees the scanner can't interpret without running the
  expander. Literals passed as normal function arguments inside a
  macro body *are* caught (they're parsed as part of the macro's
  argument expression).
- **Runtime text**: genuine dynamic text (chat input, fetched feeds)
  can't be pre-scanned. For apps that need that, the long-term fix is
  the lazy per-codepoint loader planned in roadmap section 4.4.

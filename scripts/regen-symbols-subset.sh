#!/usr/bin/env bash
#
# Regenerate the Noto Sans / Noto Sans Math subsets bundled in
# `crates/blinc_noto_symbols/assets/` from the non-ASCII codepoints
# currently used in the workspace.
#
# Sibling of `scripts/regen-emoji-subset.sh` — same scanner, same
# subsetter, but sourced from three complementary monochrome text
# fonts instead of NotoColorEmoji:
#
#   - NotoSans-Regular         currency symbols, Latin-1 Supplement
#                              punctuation, basic Latin-adjacent
#                              glyphs.
#   - NotoSansMath-Regular     mathematical operators, double arrows,
#                              letterlike symbols, everything in the
#                              U+2200-U+22FF / U+21D0-U+21FF range
#                              that NotoSans doesn't carry.
#   - NotoSansSymbols2-Regular Dingbats (U+2700-U+27BF) including
#                              ✗ ✓ ✦ ✪ etc. — neither NotoSans nor
#                              NotoSansMath own this block, and
#                              NotoColorEmoji only covers a subset
#                              of its codepoints as color glyphs.
#
# The two bundled subsets are complementary: either font's cmap
# silently ignores codepoints it doesn't own, so we can feed the
# same scanner output to both subsetters and let each contribute
# the glyphs it has. `blinc_noto_symbols::register` loads both into
# the global font registry; `fontdb` handles the multi-font lookup
# at shaping time.
#
# Pipeline:
#   1. `blinc-emoji-scan` walks `crates/blinc_app/examples/*.rs`
#      (or the path passed as $1) and harvests every non-ASCII
#      codepoint that appears in a string or char literal — including
#      HTML entities decoded via `html_escape`, which is the form
#      most Blinc examples use for special characters.
#   2. `hb-subset` runs twice — once against NotoSans-Regular,
#      once against NotoSansMath-Regular — and writes two static
#      TTFs under the crate's `assets/` directory.
#
# Requirements:
#   - Rust toolchain (for the scanner)
#   - harfbuzz with `hb-subset` in PATH
#     (`brew install harfbuzz`, `apt install harfbuzz-utils`, etc.)
#   - curl (to download the NotoSans / NotoSansMath sources — ~1 MB)
#
# Run from the workspace root:
#   ./scripts/regen-symbols-subset.sh

set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$WORKSPACE_ROOT"

# Google Fonts hosts both font sources at stable paths in its
# `google/fonts` repo. NotoSans is a variable font (we pin to
# Regular during subsetting); NotoSansMath ships as a static TTF.
# The OFL 1.1 license text is shared between the two families and
# lives alongside NotoSans.
SANS_URL="https://raw.githubusercontent.com/google/fonts/main/ofl/notosans/NotoSans%5Bwdth%2Cwght%5D.ttf"
MATH_URL="https://raw.githubusercontent.com/google/fonts/main/ofl/notosansmath/NotoSansMath-Regular.ttf"
SYM2_URL="https://raw.githubusercontent.com/google/fonts/main/ofl/notosanssymbols2/NotoSansSymbols2-Regular.ttf"
LICENSE_URL="https://raw.githubusercontent.com/google/fonts/main/ofl/notosans/OFL.txt"

SANS_CACHE="$WORKSPACE_ROOT/target/noto-sans/NotoSans-VF.ttf"
MATH_CACHE="$WORKSPACE_ROOT/target/noto-sans/NotoSansMath-Regular.ttf"
SYM2_CACHE="$WORKSPACE_ROOT/target/noto-sans/NotoSansSymbols2-Regular.ttf"
LICENSE_CACHE="$WORKSPACE_ROOT/target/noto-sans/OFL.txt"
CODEPOINTS="$WORKSPACE_ROOT/target/noto-sans/codepoints.txt"

# Intermediate subset paths inside the target/ cache. We subset each
# source font separately, then merge the three into a single face
# that ships in the crate under the asset directory. Keeping the
# intermediates out of `crates/blinc_noto_symbols/assets/` avoids
# shipping redundant copies in the crate.
SANS_STAGE="$WORKSPACE_ROOT/target/noto-sans/NotoSans-subset.ttf"
MATH_STAGE="$WORKSPACE_ROOT/target/noto-sans/NotoSansMath-subset.ttf"
SYM2_STAGE="$WORKSPACE_ROOT/target/noto-sans/NotoSansSymbols2-subset.ttf"

# The single merged output. `blinc_text::FontRegistry::load_symbol_font`
# returns a single font face, so we can't ship the three subsets as
# independent files and expect all to be consulted — `pyftmerge`
# combines their cmaps into one face (NotoSans wins on conflicts
# since it's listed first, contributing Latin-1 / currency;
# NotoSansMath contributes math operators / double arrows;
# NotoSansSymbols2 contributes Dingbats NotoSans + Math don't own).
MERGED_OUT="$WORKSPACE_ROOT/crates/blinc_noto_symbols/assets/BlincNotoSymbols-subset.ttf"
LICENSE_OUT="$WORKSPACE_ROOT/crates/blinc_noto_symbols/assets/NotoSans-OFL.txt"

# The scan input. By default we harvest from every example in the
# workspace so the bundled subset covers the entire gallery. Pass a
# path as the first argument to scan something else (e.g. your own
# app's src tree).
SCAN_PATHS=("${1:-crates/blinc_app/examples}")

mkdir -p "$(dirname "$SANS_CACHE")" "$(dirname "$MERGED_OUT")"

command -v hb-subset >/dev/null 2>&1 || {
    echo "error: hb-subset not found in PATH" >&2
    echo "       install with \`brew install harfbuzz\` on macOS or" >&2
    echo "       \`apt install harfbuzz-utils\` on Debian/Ubuntu" >&2
    exit 1
}

# fontTools is used for both `pyftmerge` (merging the two subsets
# into a single face) and the `rename-font-family.py` helper
# (rewriting the merged font's `name` table). `pip install --user
# fonttools` on macOS drops the binaries in ~/Library/Python/3.9/bin
# and the modules in the matching site-packages directory; prepend
# both to PATH / PYTHONPATH so the rest of the script can pick them
# up without the user needing to export anything.
export PATH="${HOME}/Library/Python/3.9/bin:${PATH}"
export PYTHONPATH="${PYTHONPATH:-}:${HOME}/Library/Python/3.9/lib/python/site-packages"

command -v pyftmerge >/dev/null 2>&1 || {
    echo "error: pyftmerge not found in PATH" >&2
    echo "       install fontTools with \`pip install --user fonttools\`" >&2
    exit 1
}

if [[ ! -f "$SANS_CACHE" ]]; then
    echo "==> Downloading NotoSans variable font source (~600 KB)"
    curl -sL "$SANS_URL" -o "$SANS_CACHE"
fi

if [[ ! -f "$MATH_CACHE" ]]; then
    echo "==> Downloading NotoSansMath Regular source (~500 KB)"
    curl -sL "$MATH_URL" -o "$MATH_CACHE"
fi

if [[ ! -f "$SYM2_CACHE" ]]; then
    echo "==> Downloading NotoSansSymbols2 Regular source (~270 KB)"
    curl -sL "$SYM2_URL" -o "$SYM2_CACHE"
fi

if [[ ! -f "$LICENSE_CACHE" ]]; then
    echo "==> Downloading SIL OFL 1.1 license text"
    curl -sL "$LICENSE_URL" -o "$LICENSE_CACHE"
fi

echo "==> Harvesting codepoints from ${SCAN_PATHS[*]}"
cargo run -q -p blinc-emoji-scan -- \
    --output "$CODEPOINTS" \
    "${SCAN_PATHS[@]}"

CP_COUNT=$(wc -l <"$CODEPOINTS" | tr -d '[:space:]')
echo "==> Subsetting $CP_COUNT codepoints"

# --drop-tables removes layout tables (GSUB/GPOS/GDEF) that aren't
# needed for simple symbol rendering — Blinc's shaper treats these
# fallback glyphs as one-off lookups rather than running complex
# OpenType features on them. Saves ~20 KB per subset.
#
# --variations=... pins the NotoSans variable font to the Regular
# instance so the output ships as a static TTF rather than dragging
# the ~8 KB fvar / gvar tables into every binary that links this
# crate. NotoSansMath ships as a static TTF already so needs no
# `--variations` flag.
echo "==> Subsetting NotoSans (currency, Latin-1 punctuation)"
hb-subset "$SANS_CACHE" \
    --unicodes-file="$CODEPOINTS" \
    --output-file="$SANS_STAGE" \
    --variations=wght=400,wdth=100 \
    --drop-tables=GSUB,GPOS,GDEF

echo "==> Subsetting NotoSansMath (math operators, double arrows)"
# Drop the MATH/BASE/JSTF tables along with GSUB/GPOS/GDEF — MATH in
# particular breaks `pyftmerge` (fontTools has no merge logic for
# its `MathGlyphInfo` structure) and we only want the glyph outlines
# / cmap for fallback shaping, not the OpenType math layout features.
hb-subset "$MATH_CACHE" \
    --unicodes-file="$CODEPOINTS" \
    --output-file="$MATH_STAGE" \
    --drop-tables=GSUB,GPOS,GDEF,MATH,BASE,JSTF

echo "==> Subsetting NotoSansSymbols2 (Dingbats, misc arrows)"
# Drop vhea/vmtx along with GSUB/GPOS/GDEF — NotoSansSymbols2
# carries vertical metrics tables that NotoSans / NotoSansMath
# don't have, and `pyftmerge` chokes trying to merge a vhea field
# whose sibling tables don't exist. We never use vertical layout
# in Blinc, so dropping them is free.
hb-subset "$SYM2_CACHE" \
    --unicodes-file="$CODEPOINTS" \
    --output-file="$SYM2_STAGE" \
    --drop-tables=GSUB,GPOS,GDEF,vhea,vmtx

echo "==> Merging NotoSans + NotoSansMath + NotoSansSymbols2 → single face"
# pyftmerge combines the three cmap tables into a single face. When
# multiple inputs carry the same codepoint, the first input wins,
# so we list NotoSans first (currency / Latin-1), then NotoSansMath
# (math operators / double arrows NotoSans lacks), then
# NotoSansSymbols2 (Dingbats like ✗ that neither of the others
# own). The merged output inherits NotoSans's metrics (line height,
# ascent/descent), which keeps shaping consistent for
# Latin-adjacent fallback use.
(
    cd "$WORKSPACE_ROOT/target/noto-sans" && \
    pyftmerge \
        --output-file="$MERGED_OUT" \
        "$SANS_STAGE" \
        "$MATH_STAGE" \
        "$SYM2_STAGE"
)

# Rename the merged `name` table so fontdb registers the face as
# "Blinc Noto Symbols" instead of one of the upstream Noto names.
# Without this, `blinc_text`'s generic sans-serif fallback chain
# finds "Noto Sans" in the registry and picks the merged subset as
# the *primary* sans-serif font — which breaks all Latin text
# because the subset only carries the ~150 non-ASCII codepoints the
# scanner harvested. Using a blinc-specific name keeps the merged
# face out of that chain; `load_symbol_font` references the new
# name explicitly so shaping still picks it up for missing glyphs.
RENAME_PY="$WORKSPACE_ROOT/scripts/rename-font-family.py"

echo "==> Renaming merged family → Blinc Noto Symbols"
python3 "$RENAME_PY" "$MERGED_OUT" "$MERGED_OUT" "Blinc Noto Symbols"

cp "$LICENSE_CACHE" "$LICENSE_OUT"

MERGED_SIZE=$(stat -f%z "$MERGED_OUT" 2>/dev/null || stat -c%s "$MERGED_OUT")
MERGED_KB=$((MERGED_SIZE / 1024))

echo "==> Wrote $MERGED_OUT ($MERGED_KB KB)"
echo "==> Wrote $LICENSE_OUT (OFL 1.1)"
echo "    Review with \`git diff --stat crates/blinc_noto_symbols/assets/\` before committing."

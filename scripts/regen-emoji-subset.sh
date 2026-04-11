#!/usr/bin/env bash
#
# Regenerate `crates/blinc_text/assets/fonts/NotoColorEmoji-subset.ttf`
# from the codepoints currently used in the workspace.
#
# Pipeline:
#   1. `blinc-emoji-scan` walks `crates/blinc_app/examples/*.rs` and
#      harvests every non-ASCII codepoint that appears in a string or
#      char literal.
#   2. `hb-subset` takes the codepoint list + a full NotoColorEmoji
#      source and emits a tiny subset that retains only the glyphs we
#      need (plus the CBDT/CBLC color tables).
#
# Requirements:
#   - Rust toolchain (for the scanner)
#   - harfbuzz with `hb-subset` in PATH
#     (`brew install harfbuzz`, `apt install harfbuzz-utils`, etc.)
#   - curl (to download the NotoColorEmoji source — ~10 MB)
#
# Run from the workspace root:
#   ./scripts/regen-emoji-subset.sh

set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$WORKSPACE_ROOT"

SOURCE_URL="https://raw.githubusercontent.com/googlefonts/noto-emoji/main/fonts/NotoColorEmoji.ttf"
SOURCE_CACHE="$WORKSPACE_ROOT/target/noto-emoji/NotoColorEmoji.ttf"
CODEPOINTS="$WORKSPACE_ROOT/target/noto-emoji/codepoints.txt"
SUBSET_OUT="$WORKSPACE_ROOT/crates/blinc_noto_emoji/assets/NotoColorEmoji-subset.ttf"

# The scan input. By default we harvest from every example in the
# workspace so the bundled subset covers the entire gallery. Pass a
# path as the first argument to scan something else (e.g. your own
# app's src tree).
SCAN_PATHS=("${1:-crates/blinc_app/examples}")

mkdir -p "$(dirname "$SOURCE_CACHE")" "$(dirname "$SUBSET_OUT")"

command -v hb-subset >/dev/null 2>&1 || {
    echo "error: hb-subset not found in PATH" >&2
    echo "       install with \`brew install harfbuzz\` on macOS or" >&2
    echo "       \`apt install harfbuzz-utils\` on Debian/Ubuntu" >&2
    exit 1
}

if [[ ! -f "$SOURCE_CACHE" ]]; then
    echo "==> Downloading NotoColorEmoji source (~10 MB)"
    curl -sL "$SOURCE_URL" -o "$SOURCE_CACHE"
fi

echo "==> Harvesting codepoints from ${SCAN_PATHS[*]}"
cargo run -q -p blinc-emoji-scan -- \
    --output "$CODEPOINTS" \
    "${SCAN_PATHS[@]}"

CP_COUNT=$(wc -l <"$CODEPOINTS" | tr -d '[:space:]')
echo "==> Subsetting $CP_COUNT codepoints"

# --drop-tables removes layout tables (GSUB/GPOS/GDEF) that aren't used
# by Blinc's shaper for emoji rendering — they add ~10 KB each without
# changing visual output.
hb-subset "$SOURCE_CACHE" \
    --unicodes-file="$CODEPOINTS" \
    --output-file="$SUBSET_OUT" \
    --drop-tables=GSUB,GPOS,GDEF

SIZE=$(stat -f%z "$SUBSET_OUT" 2>/dev/null || stat -c%s "$SUBSET_OUT")
SIZE_KB=$((SIZE / 1024))

echo "==> Wrote $SUBSET_OUT ($SIZE_KB KB)"
echo "    Review with \`git diff --stat $SUBSET_OUT\` before committing."

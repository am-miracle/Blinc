#!/usr/bin/env bash
# Opt into local sibling-crate development.
#
# The committed Cargo.toml resolves blinc_canvas_kit / blinc_portal_ui /
# blinc_node_editor / blinc_game_kit from their published git pins, so a
# fresh clone builds every example with no setup. Run this script only
# when you want to EDIT one of those sibling crates from a local checkout:
# it clones any missing repos into `packages/*` (gitignored) and appends
# the local-dev `[patch]` block (scripts/local-dev-patch.toml) to
# Cargo.toml so the workspace resolves them to those local copies.
#
# Revert with `scripts/use-published-packages.sh` before committing.
# Idempotent: safe to re-run.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

PATCH_FILE="scripts/local-dev-patch.toml"
CARGO_TOML="Cargo.toml"
MARKER='^# Local-dev redirects for the /packages'

if [[ ! -f "$PATCH_FILE" ]]; then
    echo "error: $PATCH_FILE not found" >&2
    exit 1
fi

# Clone any missing sibling repos into packages/ (gitignored).
mkdir -p packages
for entry in \
    "blinc_canvas_kit|https://github.com/project-blinc/blinc_canvas_kit.git" \
    "blinc_portal_ui|https://github.com/project-blinc/blinc_portal_ui.git" \
    "blinc_node_editor|https://github.com/project-blinc/blinc_node_editor.git" \
    "blinc_game_kit|https://github.com/project-blinc/blinc_game_kit.git"; do
    name="${entry%%|*}"
    url="${entry##*|}"
    if [[ -d "packages/$name/.git" ]]; then
        echo "  $name: already checked out"
    else
        echo "  $name: cloning..."
        git clone --quiet "$url" "packages/$name"
    fi
done

# Append the [patch] block if it isn't already enabled.
if grep -qE "$MARKER" "$CARGO_TOML"; then
    echo "use-local-packages: [patch] block already present in $CARGO_TOML"
else
    printf '\n' >> "$CARGO_TOML"
    cat "$PATCH_FILE" >> "$CARGO_TOML"
    echo "use-local-packages: appended local-dev [patch] block to $CARGO_TOML"
fi

echo
echo "Local sibling crates are now active. Cargo.toml is modified LOCALLY —"
echo "do NOT commit it (CI's check-no-local-patch.sh guard will fail)."
echo "Revert with: scripts/use-published-packages.sh"

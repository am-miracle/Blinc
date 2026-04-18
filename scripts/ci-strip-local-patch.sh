#!/usr/bin/env bash
# Strip the local-dev `[patch."https://github.com/project-blinc/blinc_{gltf,skeleton,input}.git"]`
# block from Cargo.toml before running cargo on CI.
#
# The patches redirect those git deps to `packages/blinc_*/` which are
# gitignored — they exist only on developer machines with a local
# checkout. CI runners clone only Blinc itself, so cargo fails with
# `No such file or directory` when resolving the patch paths.
#
# Removes everything from the `# Local-dev redirects for the /packages/*`
# comment marker to EOF. The earlier `[patch."...project-blinc/Blinc.git"]`
# block (which redirects to in-repo `crates/*` and does exist on CI)
# is untouched.
#
# Idempotent: safe to run even if the block is already absent.

set -euo pipefail

CARGO_TOML="${1:-Cargo.toml}"

if [[ ! -f "$CARGO_TOML" ]]; then
    echo "error: $CARGO_TOML not found" >&2
    exit 1
fi

if ! grep -q '^# Local-dev redirects for the /packages' "$CARGO_TOML"; then
    echo "ci-strip-local-patch: no local-dev patch block found in $CARGO_TOML — nothing to do"
    exit 0
fi

# Portable across BSD (macOS) and GNU sed: write to a temp file then move.
tmp="$(mktemp)"
awk '/^# Local-dev redirects for the \/packages/ {exit} {print}' "$CARGO_TOML" > "$tmp"
mv "$tmp" "$CARGO_TOML"

echo "ci-strip-local-patch: stripped local-dev patch block from $CARGO_TOML"

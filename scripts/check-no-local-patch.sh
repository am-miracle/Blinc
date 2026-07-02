#!/usr/bin/env bash
# CI guard: fail if the committed Cargo.toml still contains the local-dev
# `[patch]` block that redirects the sibling crates to `packages/*`.
#
# That block must never be committed — `packages/*` is gitignored and
# absent on fresh clones, so its presence breaks `cargo` for every user
# at dependency-resolution time (before any example even compiles). The
# committed manifest must resolve those crates from their published git
# pins. Enable local checkouts with `scripts/use-local-packages.sh` and
# revert with `scripts/use-published-packages.sh` before committing.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CARGO_TOML="${1:-$REPO_ROOT/Cargo.toml}"

if grep -qE '^# Local-dev redirects for the /packages' "$CARGO_TOML"; then
    echo "::error::Cargo.toml contains the local-dev [patch] block for packages/*." >&2
    echo "Run scripts/use-published-packages.sh and re-commit — the committed" >&2
    echo "manifest must resolve sibling crates from their published git pins." >&2
    exit 1
fi

echo "check-no-local-patch: OK (no local-dev [patch] block in Cargo.toml)"

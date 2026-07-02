#!/usr/bin/env bash
# Revert to the published git pins for the sibling crates: removes the
# local-dev `[patch]` block from Cargo.toml (the inverse of
# `scripts/use-local-packages.sh`). Leaves the `packages/*` checkouts in
# place; only edits Cargo.toml. Idempotent.
#
# Thin alias for the CI strip script so the enable/disable pair reads
# symmetrically. Run this before committing Cargo.toml.

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
exec bash "$REPO_ROOT/scripts/ci-strip-local-patch.sh" "${1:-$REPO_ROOT/Cargo.toml}"

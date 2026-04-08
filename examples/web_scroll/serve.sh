#!/usr/bin/env bash
# Serve the example over HTTP and print the URL.
#
# Browsers won't import wasm modules from `file://`, so even though
# `index.html` is just a static page, it has to come from a real HTTP
# server. This script picks the first available static-file server on
# your system and runs it from this directory.
#
# Usage:
#   ./serve.sh           # default port 8000
#   ./serve.sh 3000      # custom port
#
# Run `wasm-pack build --target web --release` first so `pkg/` exists.

set -euo pipefail

cd "$(dirname "$0")"

PORT="${1:-8000}"

if [ ! -d pkg ]; then
  echo "pkg/ not found. Run \`wasm-pack build --target web --release\` first." >&2
  exit 64
fi

URL="http://localhost:${PORT}/"
echo "Serving $(pwd) on ${URL}"
echo "Press Ctrl-C to stop."
echo

if command -v python3 >/dev/null 2>&1; then
  exec python3 -m http.server "${PORT}"
elif command -v python >/dev/null 2>&1; then
  exec python -m http.server "${PORT}"
elif command -v ruby >/dev/null 2>&1; then
  exec ruby -run -e httpd . -p "${PORT}"
elif command -v npx >/dev/null 2>&1; then
  exec npx --yes http-server -p "${PORT}" -c-1 .
else
  echo "No static HTTP server found. Install one of:" >&2
  echo "  python3, python, ruby, or npx (Node.js)" >&2
  exit 127
fi

#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"
rm -f rust-beacon-builder.zip
zip -q rust-beacon-builder.zip config.json rust-beacon-builder.html rust-beacon-builder.css rust-beacon-builder.js server.js
echo "[ok] $ROOT/rust-beacon-builder.zip"

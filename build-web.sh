#!/usr/bin/env bash
# Builds the WebAssembly version of Lumen into ../public/webapp (served by the Next.js site at /app).
# Requirements: rustup target add wasm32-unknown-unknown ; cargo install wasm-bindgen-cli --version <version in Cargo.lock>
# Optional: WASM_OPT=1 runs binaryen wasm-opt — NOT the default: with this toolchain it breaks the externref table and the app fails to start in Chrome.
set -euo pipefail
cd "$(dirname "$0")"
OUT=../public/webapp
cargo build --lib --profile web --target wasm32-unknown-unknown
WASM="${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/web/lumen.wasm"
mkdir -p "$OUT"
wasm-bindgen --target web --no-typescript --out-dir "$OUT" --out-name lumen "$WASM"
BUILD=$(date -u +%Y%m%d%H%M%S)
sed "s/__BUILD__/$BUILD/g" web/index.html > "$OUT/index.html"
VERSION=$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
printf '{"version":"%s","built":"%s"}\n' "$VERSION" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$OUT/version.json"
if [ "${WASM_OPT:-0}" = "1" ] && command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -O3 --enable-bulk-memory --enable-sign-ext --enable-mutable-globals --enable-nontrapping-float-to-int --enable-multivalue --enable-reference-types "$OUT/lumen_bg.wasm" -o "$OUT/lumen_bg.wasm"
fi
ls -la "$OUT"

#!/usr/bin/env bash
# Builds the WebAssembly version of Lumen into ../public/webapp (served by the Next.js site at /app).
# Requirements: rustup target add wasm32-unknown-unknown ; wasm-bindgen-cli 0.2.130 (must match Cargo.lock)
# Optional: binaryen >= 116 `wasm-opt` for a smaller download (WASM_OPT=1).
set -euo pipefail
cd "$(dirname "$0")"
OUT=../public/webapp
cargo build --lib --release --target wasm32-unknown-unknown
WASM="${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/release/lumen.wasm"
mkdir -p "$OUT"
wasm-bindgen --target web --no-typescript --out-dir "$OUT" --out-name lumen "$WASM"
# wasm-opt is opt-in: binaryen < 116 miscompiles wasm-bindgen's externref table. Use WASM_OPT=1 with a recent binaryen.
if [ "${WASM_OPT:-0}" = "1" ] && command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -O3 --enable-bulk-memory --enable-sign-ext --enable-mutable-globals --enable-nontrapping-float-to-int --enable-multivalue --enable-reference-types "$OUT/lumen_bg.wasm" -o "$OUT/lumen_bg.wasm"
fi
ls -la "$OUT"

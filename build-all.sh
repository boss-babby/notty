#!/usr/bin/env bash
# Builds every deliverable into ../public: WebAssembly app, signed Android APK, Windows exe (+zip) and SHA256SUMS.
# Headless: needs rustup (wasm32-unknown-unknown, aarch64-linux-android, x86_64-pc-windows-gnu), wasm-bindgen CLI, JDK 17, Android SDK/NDK, mingw-w64.
set -euo pipefail
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/lt}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-3}"
export CARGO_JOBS="$CARGO_BUILD_JOBS"
step() { echo; echo "════ $* ════ $(date +%T)"; }
step "web (WebAssembly)";  bash build-web.sh
step "android (arm64 APK)"; bash build-android.sh
step "windows (x64 exe)";   bash build-windows.sh
step "done"

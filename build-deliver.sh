#!/usr/bin/env bash
# Headless delivery build: WebAssembly (public/webapp) -> Android APK -> Windows exe/zip -> source zip -> SHA256SUMS.
# Requirements: rustup targets wasm32-unknown-unknown / aarch64-linux-android / x86_64-pc-windows-gnu, wasm-bindgen CLI,
# JDK 17, Android SDK 34 + NDK r26, mingw-w64. Run ./setup-android-env.sh once (see README).
set -uo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")"
export JAVA_HOME="${JAVA_HOME:-$(dirname "$(dirname "$(readlink -f "$(command -v javac)")")")}"
JOBS="${CARGO_JOBS:-4}"
export CARGO_BUILD_JOBS="$JOBS"
STATUS=0
run() { echo "===== $* ($(date -u +%H:%M:%S))"; "$@" || { echo "FAILED: $*"; STATUS=1; }; }
run bash build-web.sh
run bash build-android.sh
run bash build-windows.sh
run bash make-source.sh
run bash make-sums.sh
echo "DELIVERY_DONE status=$STATUS $(date -u +%H:%M:%S)"

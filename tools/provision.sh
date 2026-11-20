#!/usr/bin/env bash
# Headless provisioning of everything needed to build Lumen for Windows, WebAssembly and Android (no GUI, no Android Studio). Idempotent.
set -uo pipefail
log() { echo "[$(date +%T)] $*"; }
export HOME="${HOME:-/home/user}"
if [ ! -x "$HOME/.cargo/bin/rustc" ]; then
  log "rustup"; curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
. "$HOME/.cargo/env"
rustup target add wasm32-unknown-unknown aarch64-linux-android x86_64-pc-windows-gnu
if ! command -v java >/dev/null || ! command -v x86_64-w64-mingw32-gcc >/dev/null; then
  log "apt"; sudo apt-get update -qq; sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq openjdk-17-jdk-headless gcc-mingw-w64-x86-64 build-essential pkg-config libssl-dev zip unzip wget curl
fi
if ! command -v wasm-bindgen >/dev/null; then
  log "wasm-bindgen"; V=$(grep -A1 'name = "wasm-bindgen"$' "$(dirname "$0")/../Cargo.lock" | grep version | cut -d'"' -f2)
  wget -q "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/$V/wasm-bindgen-$V-x86_64-unknown-linux-musl.tar.gz" -O /tmp/wb.tgz && tar xzf /tmp/wb.tgz -C /tmp && cp /tmp/wasm-bindgen-$V-x86_64-unknown-linux-musl/wasm-bindgen "$HOME/.cargo/bin/"
fi
export ANDROID_HOME="$HOME/android-sdk"
if [ ! -d "$ANDROID_HOME/ndk/26.1.10909125" ] || [ ! -d "$ANDROID_HOME/build-tools/34.0.0" ]; then
  log "android sdk"; mkdir -p "$ANDROID_HOME/cmdline-tools"
  wget -q https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip -O /tmp/cmd.zip && unzip -q -o /tmp/cmd.zip -d /tmp/cmdtmp
  rm -rf "$ANDROID_HOME/cmdline-tools/latest"; mv /tmp/cmdtmp/cmdline-tools "$ANDROID_HOME/cmdline-tools/latest"
  yes | "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" --licenses >/dev/null 2>&1
  "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" "platforms;android-34" "build-tools;34.0.0" "ndk;26.1.10909125" >/dev/null 2>&1
fi
log "provisioned"; rustc --version; java -version 2>&1 | head -1

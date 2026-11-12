#!/usr/bin/env bash
# Headless Android toolchain for building Lumen's APK on Debian/Ubuntu (no Android Studio, no GUI). Idempotent.
# Phase 1: system packages  Phase 2: SDK + NDK  Phase 3: Rust targets + cargo-apk  (Phase 4/5: ./build-android.sh)
set -euo pipefail

# Phase 1 — system & Java dependencies
sudo apt-get update
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y openjdk-17-jdk build-essential git wget unzip pkg-config libssl-dev zip

# Phase 2 — headless SDK + NDK
export ANDROID_HOME="$HOME/android-sdk"
mkdir -p "$ANDROID_HOME/cmdline-tools"
if [ ! -x "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" ]; then
  wget -q https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip -O /tmp/cmdline-tools.zip
  unzip -q -o /tmp/cmdline-tools.zip -d /tmp/cmdline-tools-unpacked
  rm -rf "$ANDROID_HOME/cmdline-tools/latest"
  mv /tmp/cmdline-tools-unpacked/cmdline-tools "$ANDROID_HOME/cmdline-tools/latest"
fi
PROFILE_LINES='export ANDROID_HOME=$HOME/android-sdk
export PATH=$PATH:$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools
export ANDROID_NDK_ROOT=$ANDROID_HOME/ndk/26.1.10909125'
grep -q 'ANDROID_NDK_ROOT' "$HOME/.bashrc" 2>/dev/null || printf '%s\n' "$PROFILE_LINES" >> "$HOME/.bashrc"
export PATH="$PATH:$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools"
yes | sdkmanager --licenses >/dev/null || true
sdkmanager "platform-tools" "platforms;android-34" "build-tools;34.0.0" "ndk;26.1.10909125"
export ANDROID_NDK_ROOT="$ANDROID_HOME/ndk/26.1.10909125"

# Phase 3 — Rust
command -v rustup >/dev/null || curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
export PATH="$HOME/.cargo/bin:$PATH"
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-apk --locked

echo "Done. Build the APK with:  ./build-android.sh   (or the plain NativeActivity variant: cargo apk build --release --target aarch64-linux-android)"

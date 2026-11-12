#!/usr/bin/env bash
# Builds the production Android APK (arm64-v8a, release, signed) -> ../public/downloads/Lumen-android-arm64.apk
#
# Why not plain `cargo apk build`?  cargo-apk can only package a pure NativeActivity without any Java. Lumen needs a small Java
# activity (android/java/com/lumen/app/LumenActivity.java) for the things the Android framework only offers through Java:
# exact window insets (status bar / cutout / navigation bar / keyboard), the soft-keyboard InputConnection, Storage-Access-Framework
# file dialogs, clipboard, notifications, printing and the Back gesture. So this script does what cargo-apk does and a bit more:
#   cargo build (NDK clang linker) -> javac -> d8 -> aapt2 -> zip -> zipalign -> apksigner
# `cargo apk build --release --target aarch64-linux-android` still works too (see [package.metadata.android] in Cargo.toml) and
# gives a plain NativeActivity APK without the Java features.
#
# Requirements (see README "Android build"): JDK 17, Android SDK (platforms;android-34, build-tools;34.0.0), NDK r26
#   (ANDROID_HOME, ANDROID_NDK_ROOT), `rustup target add aarch64-linux-android`.
set -euo pipefail
cd "$(dirname "$0")"

export ANDROID_HOME="${ANDROID_HOME:-$HOME/android-sdk}"
export ANDROID_NDK_ROOT="${ANDROID_NDK_ROOT:-$ANDROID_HOME/ndk/26.1.10909125}"
BT="$ANDROID_HOME/build-tools/34.0.0"
ANDROID_JAR="$ANDROID_HOME/platforms/android-34/android.jar"
API=26
TRIPLE=aarch64-linux-android
ABI=arm64-v8a
TC="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64"
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
OUT_DIR="${OUT_DIR:-../public/downloads}"
WORK="${ANDROID_WORK:-$TARGET_DIR/android-apk}"
APK_NAME="${APK_NAME:-Lumen-android-arm64.apk}"

VERSION_NAME=$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
IFS=. read -r MA MI PA <<<"$VERSION_NAME"
VERSION_CODE=$(( ${MA:-1} * 10000 + ${MI:-0} * 100 + ${PA:-0} ))

for f in "$BT/aapt2" "$BT/d8" "$BT/zipalign" "$BT/apksigner" "$ANDROID_JAR" "$TC/bin/${TRIPLE}${API}-clang"; do
  [ -e "$f" ] || { echo "missing: $f" >&2; exit 1; }
done

# 1. Rust -> liblumen.so ---------------------------------------------------------------------------------------------------------
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$TC/bin/${TRIPLE}${API}-clang"
export CC_aarch64_linux_android="$TC/bin/${TRIPLE}${API}-clang"
export CXX_aarch64_linux_android="$TC/bin/${TRIPLE}${API}-clang++"
export AR_aarch64_linux_android="$TC/bin/llvm-ar"
# 16 KB ELF segment alignment (required by Android 15+ devices with 16 KB pages; harmless elsewhere)
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="-C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384"
PROFILE="${PROFILE:-android}"
cargo build ${CARGO_JOBS:+-j$CARGO_JOBS} --profile "$PROFILE" --lib --target "$TRIPLE"
SO="$TARGET_DIR/$TRIPLE/$PROFILE/liblumen.so"
[ -f "$SO" ] || { echo "liblumen.so was not produced" >&2; exit 1; }

# 2. Java -> classes.dex ---------------------------------------------------------------------------------------------------------
rm -rf "$WORK"; mkdir -p "$WORK/classes" "$WORK/dex" "$WORK/stage/lib/$ABI"
javac -encoding UTF-8 -nowarn -source 8 -target 8 -bootclasspath "$ANDROID_JAR" -d "$WORK/classes" $(find android/java -name '*.java') 2>&1 | grep -v "^warning: \[options\]\|^Note:\|^[0-9]* warning" || true
CLASSES=$(find "$WORK/classes" -name '*.class')
[ -n "$CLASSES" ] || { echo "javac produced no classes" >&2; exit 1; }
"$BT/d8" --release --min-api "$API" --lib "$ANDROID_JAR" --output "$WORK/dex" $CLASSES

# 3. resources + manifest (aapt2) ------------------------------------------------------------------------------------------------
sed "s/@VERSION_CODE@/$VERSION_CODE/; s/@VERSION_NAME@/$VERSION_NAME/" android/AndroidManifest.xml > "$WORK/AndroidManifest.xml"
"$BT/aapt2" compile --dir android/res -o "$WORK/res.zip"
"$BT/aapt2" link -o "$WORK/base.apk" -I "$ANDROID_JAR" --manifest "$WORK/AndroidManifest.xml" \
  --min-sdk-version "$API" --target-sdk-version 34 --version-code "$VERSION_CODE" --version-name "$VERSION_NAME" \
  --auto-add-overlay "$WORK/res.zip"

# 4. add dex + native library -----------------------------------------------------------------------------------------------------
cp "$WORK/dex/classes.dex" "$WORK/stage/classes.dex"
cp "$SO" "$WORK/stage/lib/$ABI/liblumen.so"
(cd "$WORK/stage" && zip -q -X -r ../base.apk classes.dex lib)

# 5. align + sign -----------------------------------------------------------------------------------------------------------------
"$BT/zipalign" -f -p 4 "$WORK/base.apk" "$WORK/aligned.apk"
KS="${LUMEN_KEYSTORE:-android/lumen-release.jks}"
KS_PASS="${LUMEN_KEYSTORE_PASS:-lumen-release}"
if [ ! -f "$KS" ]; then
  echo "creating a new signing key at $KS (keep it: updates must be signed with the same key)"
  keytool -genkeypair -keystore "$KS" -storepass "$KS_PASS" -keypass "$KS_PASS" -alias lumen -keyalg RSA -keysize 4096 -validity 36500 \
    -dname "CN=Lumen, OU=Lumen, O=Lumen, C=US"
fi
mkdir -p "$OUT_DIR"
"$BT/apksigner" sign --ks "$KS" --ks-pass "pass:$KS_PASS" --key-pass "pass:$KS_PASS" --ks-key-alias lumen \
  --v1-signing-enabled true --v2-signing-enabled true --v3-signing-enabled true --v4-signing-enabled false --out "$OUT_DIR/$APK_NAME" "$WORK/aligned.apk"
"$BT/apksigner" verify --verbose "$OUT_DIR/$APK_NAME" | head -n 6
./make-sums.sh >/dev/null
echo "APK: $(readlink -f "$OUT_DIR/$APK_NAME")  ($(du -h "$OUT_DIR/$APK_NAME" | cut -f1), $(stat -c %s "$OUT_DIR/$APK_NAME") bytes)"

#!/usr/bin/env bash
# Regenerates ../public/downloads/SHA256SUMS.txt for every published artifact (Windows exe/zip, Android APK).
set -euo pipefail
cd "$(dirname "$0")/../public/downloads"
files=()
for f in Lumen-android-arm64.apk Lumen-windows-x64.exe Lumen-windows-x64.zip Lumen-source.zip; do [ -f "$f" ] && files+=("$f"); done
sha256sum "${files[@]}" > SHA256SUMS.txt
cat SHA256SUMS.txt

#!/usr/bin/env bash
# Packs the source of this repository (tracked + new files, without .git, build output and the unrelated archives in the root)
# into ../public/downloads/Lumen-source.zip so the website can offer the exact code the binaries were built from.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=public/downloads/Lumen-source.zip
mkdir -p public/downloads
rm -f "$OUT"
git -C lumen ls-files -co --exclude-standard | grep -v '^[^/]*\.zip$' | sed 's#^#lumen/#' | zip -q -9 -@ "$OUT"
ls -la "$OUT"

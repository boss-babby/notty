#!/usr/bin/env bash
# Packs the source of this repository (tracked + new files, without .git, build output and the archives in the root)
# into ../public/downloads/Lumen-source.zip so the website can offer the exact code the binaries were built from.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(dirname "$HERE")"
OUT="$ROOT/public/downloads/Lumen-source.zip"
mkdir -p "$(dirname "$OUT")"
rm -f "$OUT"
git -C "$HERE" ls-files -co --exclude-standard | grep -v '^[^/]*\.zip$' | sed "s#^#$(basename "$HERE")/#" > /tmp/lumen-source-files.txt
(cd "$ROOT" && zip -q -9 -@ "$OUT" < /tmp/lumen-source-files.txt)
ls -la "$OUT"

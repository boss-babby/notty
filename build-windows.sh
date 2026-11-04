#!/usr/bin/env bash
# Cross-compiles the Windows x64 executable (needs: rustup target add x86_64-pc-windows-gnu ; apt install gcc-mingw-w64-x86-64).
# On a Windows machine simply run:  cargo build --release
set -euo pipefail
cd "$(dirname "$0")"
cargo build --release --bin lumen --target x86_64-pc-windows-gnu
EXE="${CARGO_TARGET_DIR:-target}/x86_64-pc-windows-gnu/release/lumen.exe"
mkdir -p ../public/downloads
cp "$EXE" ../public/downloads/Lumen-windows-x64.exe
(cd ../public/downloads && rm -f Lumen-windows-x64.zip && zip -q Lumen-windows-x64.zip Lumen-windows-x64.exe && sha256sum Lumen-windows-x64.exe Lumen-windows-x64.zip > SHA256SUMS.txt)
ls -la ../public/downloads

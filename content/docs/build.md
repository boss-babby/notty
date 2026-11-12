# Build & deliver — Web (WASM), Android (APK), Windows (EXE)

Everything is Rust (egui). One code base, three targets, built **headless** (no Android Studio, no GUI) on Debian/Ubuntu:

```bash
cd rust
bash tools/fetch-assets.sh     # fonts, icons (not stored in git)
bash build-all.sh              # web → android → windows, outputs land in ../public
```

## Toolchain (what `build-all.sh` expects)

| Piece | Version used |
|---|---|
| Rust | stable (1.100) + targets `wasm32-unknown-unknown`, `aarch64-linux-android`, `x86_64-pc-windows-gnu` |
| wasm-bindgen CLI | 0.2.130 (must equal `Cargo.lock`) |
| JDK | OpenJDK 17 (javac → d8) |
| Android SDK | cmdline-tools 11076708, platform 34, build-tools 34.0.0, NDK 26.1.10909125 |
| Windows cross | `gcc-mingw-w64-x86-64` |

`setup-android-env.sh` installs the Android half; `tools/fetch-assets.sh` restores the binary assets (Inter, Lucide subset, Noto scripts, PWA icons).

## Web — `build-web.sh`
`cargo build --lib --profile web --target wasm32-unknown-unknown` → `wasm-bindgen --target web` → `public/webapp` (index, service worker, manifest, icons).
The site rewrites `/app` → `/webapp/index.html` (`next.config.ts`), `<base href="/webapp/">` keeps assets resolving.

## Android — `build-android.sh` (optimised)
1. `cargo build --profile android --lib --target aarch64-linux-android` — NDK r26 clang linker, **thin LTO, 4 codegen units, stripped, 16 KB page alignment** (Android 15+ devices).
2. `javac` + `d8` compile the small Java activity (`android/java`): window insets, soft-keyboard `InputConnection`, Storage Access Framework dialogs, clipboard, notifications, alarms, printing.
3. `aapt2 compile/link` the manifest and resources, add `classes.dex` + `lib/arm64-v8a/liblumen.so`, `zipalign -p 4`.
4. `apksigner` v1–v3 signature with `android/lumen-release.jks` (keep this key: updates must be signed with it).

Manifest highlights: `minSdk 26`, `targetSdk 34`, `largeHeap`, hardware acceleration, predictive-back opt-in, exact-alarm reminders that survive reboots, cleartext traffic allowed so the AI settings can reach an Ollama / LM Studio server on your LAN.

Mobile-first AI: on screens narrower than 720 pt the Lumen AI chat is a full-screen sheet, the composer rides on top of the keyboard, Back closes settings → chat → drawer → page, and tool cards are touch-sized.

## Windows — `build-windows.sh`
`cargo build --release --bin lumen --target x86_64-pc-windows-gnu` → `public/downloads/Lumen-windows-x64.exe` (+ `.zip`). WebView2 is used for live web previews (ships with Windows 10/11).

## Verifying
`public/downloads/SHA256SUMS.txt` lists every artifact. `apksigner verify --verbose Lumen-android-arm64.apk` must print *Verified using v2/v3 scheme: true*.
Tests: `cargo test --lib` (incl. 7 agent / MCP / UI-render tests).

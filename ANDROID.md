# Lumen for Android

One Rust codebase (egui) — packaged as a signed **arm64-v8a** APK with a small Java shell.

```
./setup-android-env.sh     # once: JDK 17, SDK 34, NDK r26, Rust targets, cargo-apk   (headless, no Android Studio)
./build-android.sh         # -> ../public/downloads/Lumen-android-arm64.apk  (served by the website at /downloads)
```

`build-android.sh` = `cargo build --release --lib --target aarch64-linux-android` (NDK clang linker, 16 KB ELF alignment) →
`javac` → `d8` → `aapt2` → `zipalign` → `apksigner` (v2 + v3). The plain `cargo apk build --release --target aarch64-linux-android`
(metadata in `Cargo.toml`) produces a Java-less NativeActivity APK in `target/aarch64-linux-android/release/apk/`; the production APK needs
the Java activity, which cargo-apk cannot compile.

## What the Java shell (`android/java/com/lumen/app/LumenActivity.java`) fixes

| Problem | Solution |
|---|---|
| Blurry text | The activity reports `DisplayMetrics.density`; `src/android.rs` forces egui's `pixels_per_point` to exactly that value, so glyphs are rasterised at the real device resolution (same as WebAssembly with `devicePixelRatio`) and the phone breakpoints see the same dp width as in Chrome. |
| Content under the status bar / navigation bar / cutout | Edge-to-edge window; exact `WindowInsets` are sent to Rust, which shrinks egui's `screen_rect`. System-bar icon colours follow the app theme (`setDarkBars`). |
| Text hidden behind the keyboard | The IME inset is part of the reported bottom inset → `screen_rect` ends on top of the keyboard; the editor scrolls the caret into view on every height change. |
| Soft keyboard | A 1×1 `View` owns a real `InputConnection` (commit / composing text, backspace, enter, arrows). Opens on a tap in a text field, closes ½ s after the last field lost focus. Hardware keyboards go through winit. |
| File dialogs | Storage Access Framework: `ACTION_OPEN_DOCUMENT` (images, covers, attachments, PDFs; ≤ 96 MB) and `ACTION_CREATE_DOCUMENT` (exports), results delivered through JNI. |
| Back | Back gesture/button: close drawer → close dialog/menu (Escape) → leave text field → go up one page → move app to background (never destroys it). Android 13+ uses `OnBackInvokedCallback`, older versions the winit key event. |
| Data | `LUMEN_DATA_DIR` / `HOME` / `TMPDIR` / time zone are exported by the activity (`vendor/eframe` file_storage reads `LUMEN_DATA_DIR`). |
| Clipboard, links, haptics, notifications, print | JNI bridge (`ClipboardManager`, `ACTION_VIEW`, `Vibrator`, `NotificationManager`, `PrintManager` → "Save as PDF"). |

Vendored patches: `vendor/eframe` (storage dir), `vendor/egui-winit` (no winit soft-keyboard calls on Android, Back key capture).

## Signing
`android/lumen-release.jks` (password `lumen-release`, override with `LUMEN_KEYSTORE` / `LUMEN_KEYSTORE_PASS`) is generated on first build.
Keep it: an update must be signed with the same key. For Play Store publishing create your own upload key.

## Text rendering fix (v8.0.1) — sliced / blurry glyphs

Symptom: letters with missing vertical slices, uneven spacing, soft edges (worst on Adreno / Mali phones).

Cause: `egui_glow`'s stock shaders declare `precision mediump float;`. On mobile GPUs `mediump` is a **16-bit float**. Vertex
positions above 1024 px and — worse — the glyph-atlas texture coordinates (`a_tc` / `v_tc`, an atlas up to 8192 texels wide) cannot be
represented exactly, so whole texel columns are skipped or duplicated while a glyph is sampled.

Fix:
* `vendor/egui_glow` (patched copy of 0.31.1, wired through `[patch.crates-io]`): vertex shader `precision highp float`, fragment shader
  `highp` whenever `GL_FRAGMENT_PRECISION_HIGH` is defined (always on GLES 3.0).
* `src/android.rs`: the texture side is capped to 4096 px (smaller atlas, less GPU memory) and the pixel density stays pinned to
  `DisplayMetrics.density`.

## Power / redraw behaviour

egui is immediate-mode, but eframe only runs a frame when something asks for one: an input event, `request_repaint*()` from an
animation / timer, or a Java callback. Nothing in Lumen repaints continuously while idle (caret blink 650 ms, reminder poll 10–30 s,
toast animation only while visible). `adb logcat -s Lumen` prints `frames drawn in the last 10s` so the behaviour can be checked on a
device: idle ≈ 0–2 frames per window, scrolling = one frame per display refresh, then it falls back to 0 again.
Build profile: `opt-level 3`, thin LTO, stripped, no MSAA / depth / stencil buffers, vsync on.

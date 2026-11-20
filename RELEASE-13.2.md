# Lumen 13.2 — colour emoji, canvas formatting, exclusive PDF reader, big-data engine
* Colour emoji in all text: `vendor/epaint/src/text/emoji.rs` + `assets/emoji-noto.pack` (3,778 Noto-style pictures, bundled, offline). Monochrome glyphs remain: font family `emoji-bw`.
* Canvas nodes render and edit inline formatting (`src/canvasfmt.rs`): format bar + Ctrl/⌘ B I U, Shift+S/H, E.
* PDF reader is exclusive (app.rs) and has touch/mouse draggable scroll bars (`src/pdfbar.rs`).
* `src/bigdata.rs`: worker-thread aggregation, revision cache, sampling >250k rows, windowed database table. WebAssembly runs finished jobs in idle frames (no threads without COOP/COEP + shared memory).
* Build everything: `bash tools/pipeline.sh` (provisions Rust, JDK, NDK, mingw, wasm-bindgen; then web → Windows → Android into ../public).
* Known: `tests::right_click_keeps_selection_and_opens_menu` (pixel-sweep layout test) fails in this environment; all other 216 tests pass.

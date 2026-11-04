# Lumen

A free, offline-first **Notion alternative written entirely in Rust** with [egui](https://github.com/emilk/egui) — no web engine, no JavaScript UI.
One codebase builds a native **Windows** app and a **WebAssembly** app for the browser.

## Features
**Editor** — text, H1–H3, to-do, bulleted/numbered lists, toggles, quotes, callouts, code with syntax colours + copy button, dividers,
live table of contents, links to pages. Tab / Shift+Tab nesting, children move with their parent when dragged, collapsible toggles,
text & background colours (9), inline **bold** / *italic* / `code` / ~~strike~~ (Ctrl+B / I / E / Shift+S; markers hide when the block loses focus),
slash menu with 19 items, Markdown shortcuts (`# `, `- `, `1. `, `[] `, `> ` toggle, `" ` quote, `| ` callout, ``` , `---`).

**Databases** — properties: text, number, select, multi-select, checkbox, date, URL. Views: **Table, Board (drag & drop), List, Gallery, Calendar**,
each with its own filters and sorts. Every row is a full page. Create from the sidebar or inline with `/table database`, `/board database` …

**Workspace** — page tree, favourites, sub-pages, backlinks, breadcrumbs, covers & icons, trash with restore, 8 templates, daily journal (Ctrl+J),
command palette with full-text search (Ctrl+K), focus mode (Ctrl+.), page width / text size, word count, light/dark + 6 accent colours.

**Data** — autosave (disk on desktop, localStorage on web); export page → Markdown, database → CSV, workspace → JSON backup;
import Markdown / CSV / JSON by pasting or dropping a file on the window. Optional password lock (salted, iterated SHA-256) with idle auto-lock.

## What's new (this release)
- **Fixed:** clicking an item in the `/` slash menu now applies it (the menu used egui's hover flag, which is unreliable on the frame where the text field loses focus; it now hit-tests by position). Covered by a headless regression test.
- **Fixed:** dialogs/pop-ups could look greyed-out or hidden because the modal backdrop could end up above the dialog; layers are now explicitly ordered every frame and pop-ups are kept fully on-screen.
- **Right-click everywhere** (`widgets::context_menu`): text (Cut / Copy / Paste / Select all / Bold / Italic / Strike / Code / Turn into / Duplicate / Move / Delete), blocks & gutter, sidebar pages & empty sidebar space, breadcrumbs, database rows & cards in every view, view tabs, and every text field. Right-clicks are intercepted in `raw_input_hook`, so they never collapse or move a text selection.
- **Selection toolbar** that floats above selected text (turn into, bold, italic, strike, code, colour).
- **Lucide icons** (embedded icon font, also registered as a fallback font so any text can contain icon glyphs) for blocks, menus, databases, palette, icon picker (Icons + Emoji tabs). Legacy emoji page icons are migrated automatically.
- **Performance:** blocks far outside the viewport are not laid out (height cache), table-of-contents data is only built when needed, the lock-screen animation is throttled to ~30 fps, toast timers repaint only when animating, no depth/stencil/MSAA buffers, window size/position persisted.
- Multi-line paste creates one block per line; real clipboard access for the context-menu *Paste* (native: arboard, web: async Clipboard API).

Not (yet) included: real-time collaboration, cloud sync, comments/history/publishing (need a server), formulas/relations/rollups, image & file uploads.

## Shortcuts
`Ctrl/Cmd+K` palette · `N` new page · `J` journal · `\` sidebar · `.` focus · `L` lock · `Shift+L` theme · `Shift+E` export · `/` all shortcuts

## Build
```bash
cargo run --release            # native (any OS)
cargo test --lib               # 20 tests: model, import/export, editor ops, headless render passes
./build-windows.sh             # Windows .exe from Linux (rustup target add x86_64-pc-windows-gnu; mingw-w64)
./build-web.sh                 # WebAssembly -> ../public/webapp  (wasm-bindgen-cli 0.2.130; uses --cfg web_sys_unstable_apis from .cargo/config.toml)
```
On Windows just run `cargo build --release` → `target\release\lumen.exe`.

Fonts: Inter (SIL OFL 1.1), see `assets/LICENSE.txt`.

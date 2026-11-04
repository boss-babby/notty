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

## What's new in 2.0
- **Images, files, bookmarks & embeds** (`/image`, `/video`, `/embed`, `/bookmark`, `/file`; paste a URL into an empty block; drop files on the window). Web build embeds live websites/videos/maps in real iframes.
- **Drag-select blocks** on empty space → floating toolbar (turn into, colour, indent, move, duplicate, copy, undo, delete) and keys (Del, Ctrl+C/X/D, Ctrl+A, Esc). Block-level **undo / redo**.
- **Covers** from a local file or an image URL, drag to reposition. **Icons**: searchable colour emoji (any pasted emoji works), Lucide icons, custom uploaded picture.
- **Selection toolbar fixed** and extended: underline, highlight, link (`__u__`, `==h==`, `[text](url)`).
- **Custom emoji fixed** (a per-frame migration was replacing them), **links open on Windows** (browser hook was disabled).
- See [`ANALYSIS.md`](ANALYSIS.md) for the Notion-parity analysis, root causes and roadmap.

Limits: no real-time collaboration / cloud sync / comments / history yet; web build stores local media in localStorage (~2.6 MB cap, use image links for more).

## What's new in 2.1
- **WYSIWYG inline editing** — markers (`**`, `==`, `__` …) are never shown while editing. Blocks are edited as plain text + per-character formats (`src/rich.rs`); the stored form stays Markdown-compatible. Typing inherits the neighbouring format, Ctrl+B/I/U/E/Shift+S/Shift+H toggle formats on a selection *or* for the next typed text, the selection toolbar highlights the active formats, and links are edited in a small popup.
- **Exact block copy / paste** (`src/clip.rs`) — copying blocks (with their children) or a formatted fragment keeps the exact data in an in-app clipboard next to the Markdown text; pasting restores the same blocks (kind, colours, indent, media, nesting) / formatting. Pasted Markdown becomes real blocks (headings, lists, to-dos, quotes, code).
- **Icons fixed** — the root cause: Inter v4 contains 764 private-use stylistic alternates that shadowed Lucide's private-use code points (U+E286 drew an accented *u*). They are stripped from the bundled Inter files and Lucide is subset to its private-use range.
- **No more overflow** — menu / slash-menu / context-menu labels, descriptions and shortcut hints are truncated with an ellipsis (`widgets::text_fit`).
- **Smooth scrolling** — eased wheel / touchpad scrolling on the page.
- **PDF reader** — `/pdf` (or any file block with a `.pdf`) renders pages inline with [hayro](https://github.com/LaurenzV/hayro) (pure Rust: same code on Windows and WASM), page navigation and zoom. MuPDF (AGPL, C) and pdfium (binary) were evaluated and rejected.

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

Fonts: Inter (SIL OFL 1.1), Lucide icons (ISC), Twemoji images (CC-BY 4.0). `assets/` must contain `Inter-Regular.otf`, `Inter-Bold.otf` (rsms/inter v4.0 `extras/otf`) and `lucide.ttf` (npm `lucide-static`).

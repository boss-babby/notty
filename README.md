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

## What's new in 3.0
- **PDF reader rebuilt** (`src/pdf.rs`) — a PDF block is now just a card (nothing parsed, rendered or resident). *Read* opens a full-window reader: continuous scroll, pinch / Ctrl+wheel zoom anchored under the pointer (25–600 %), fit width / page, page box, thumbnails, keyboard (PgUp/PgDn/Home/End/Space/Ctrl±), night mode, password prompt. Pages are rasterised as 512 px **tiles** only where visible, at exact screen resolution, with ≤12 ms of rendering per frame, an LRU texture cap (72 tiles on wasm / 140 native) and low-res previews while sharp tiles arrive. Closing the reader drops the document, hayro's caches and all textures. Attached files are no longer kept in RAM at all (`MediaStore::bytes` loads from disk / local storage on demand). Native builds catch panics inside the PDF engine so a damaged file cannot crash the app.
- **Formula / relation / rollup properties** (`src/formula.rs`) — Notion-style expressions (`prop("Price") * prop("Qty")`, `if`, `dateBetween`, `formatDate`, `.length()` …, ~70 functions), relations (also to the same database) and 14 rollup functions. Results are cached in rows so filters, sorts, CSV and every view just work.
- **Date ranges + Timeline view** (`src/timeline.rs`) — date properties can have an end date; the Timeline draws draggable / resizable bars, today marker, zoom levels, dependency arrows (red when violated), connector drag to link tasks and *Auto-schedule*. Calendar shows multi-day items.
- **Undo / redo + page history** (`src/history.rs`) — multi-level undo for every block operation, versioned snapshots per page (auto + named), change summary, restore (itself undoable).
- **Print-ready export** (`src/print.rs`) — A4 HTML with inlined images, databases as tables, timelines as SVG Gantt charts; opens the print dialog (Save as PDF).
- **Video blocks** — 1080p posters with fallback, aspect ratios, autoplay / mute / loop, start time, native `<video>` for direct files on the web build.
- **Responsive** — slide-over sidebar and compact top bar below 720 pt, horizontally scrolling tables / calendar, touch panning, mobile viewport + loading progress in `web/index.html`.
- **WebAssembly** — `build-web.sh` uses the size-optimised `web` profile (`opt-level = "s"`, thin LTO, `panic = "abort"`) and `wasm-opt -O3`.

## What's new in 4.0
- **PDF annotations** — highlights are *multiplied* into the page (true marker look, text keeps full contrast), pen strokes are smoothed, and drawings / notes can be **moved and resized** (drag the body or a corner handle).
- **Physics scrolling** (`smooth.rs`) — critically damped spring for wheel / touchpad (fixes the stepped Windows touchpad feel) and 1:1 finger panning with momentum.
- **Touch** (`touch.rs`) — tap = click, swipe = scroll with fling, **long press = drag / multi-select** (marquee, drag & drop, text selection), two fingers = pinch. Works in every view.
- **Databases** — resizable table columns (double-click a header edge to reset), responsive toolbar, stacked filter rows on phones, popups clamp to the screen.
- **Graph view** (`graph.rs`, Ctrl+G) — Obsidian-style map of pages, databases, `[[wiki links]]` and relations with local graph, search, hover focus.
- **Dashboard** (`dashboard.rs`, Ctrl+D) — KPIs, pipeline / revenue charts for any database with a Select (+ Number) property, task inbox, upcoming & overdue, activity.
- **Templates** — Recruiting pipeline, Content calendar, OKRs & goals (13 total). See `ANALYSIS.md` §v4.0 for the sector playbooks and roadmap.
- The Inter / Lucide font files (missing from the repository) are regenerated: Inter v4.0 without its private-use alternates, Lucide 0.577 subset to the private-use range.

## What's new in 5.2 — pages that behave like Notion pages
**Research summary (what makes a Notion page "interactive")** — a Notion page is a tree of blocks; the interactive parts are (1) *layout blocks*
(column lists, toggles, callouts, synced blocks), (2) *databases as blocks* — an inline database or a **linked view** of any database, each with its own
layout / filter / sort / group, and (3) *dashboards* = ordinary pages that combine linked views, charts and columns. Lumen already had (3) in 5.1;
5.2 adds (1) and (2) and fixes the missing time window.

- **Dashboard pages get the date window** — the *Today · Week · Month* chips and the full *time window* menu (Last 7/30/90 days, This quarter/year, Next 7/30 days …)
  are now in the toolbar of **page dashboards**, not just Home. The window is stored per page board (`Board.range`, `dashcfg::Dash::scope_range`) and applies to every widget without its own window.
- **Column layouts** (`/2 columns` … `/5 columns`, `editor::draw_columns`) — stored as plain blocks (`Columns` → `Column` → content, two indent levels deeper) so undo, copy / paste,
  history, export and multi-select keep working. Drag the divider to resize (double-click = equal widths), drop any block into any column, hover toolbar: add / remove column, equal widths, unwrap, delete.
  Columns stack automatically on narrow screens. `editor::normalize_layout` repairs the structure after every edit (stray blocks, empty or single columns, page ending inside a layout).
- **Linked database views** (`/linked view`, `BlockKind::DbView`) — pick any database; the block gets its own hidden `View` (`View::linked = block id`) with its own layout (table / board / list /
  gallery / calendar / timeline), filters and sorts, so *"tasks due this week"* can sit next to *"done"* on the same page while the source database is unchanged. Hidden views never show up as tabs of the database.
- **Relative date filters** — `is today`, `this week`, `this month`, `next 7 days`, `past 7 days`, `overdue` for every date property (date ranges count when they touch the window) and a *Quick date filters* row in the filter popup.
- Markdown export flattens columns, print export lays them out side by side, linked views print as tables.
- Tests: `cargo test --lib` → 101 tests (15 new: layout repair, column actions, drag into columns, merge guard, date filters, page time window, headless render of columns + linked views on desktop / tablet / phone).

## Shortcuts
`Ctrl/Cmd+K` palette · `N` new page · `J` journal · `\` sidebar · `.` focus · `L` lock · `Shift+L` theme · `Shift+E` export · `/` all shortcuts

## Build
```bash
cargo run --release            # native (any OS)
cargo test --lib               # 44 tests: model, import/export, editor ops, headless render passes
./build-windows.sh             # Windows .exe from Linux (rustup target add x86_64-pc-windows-gnu; mingw-w64)
./build-web.sh                 # WebAssembly -> ../public/webapp  (wasm-bindgen-cli 0.2.130; uses --cfg web_sys_unstable_apis from .cargo/config.toml)
```
On Windows just run `cargo build --release` → `target\release\lumen.exe`.

Fonts: Inter (SIL OFL 1.1), Lucide icons (ISC), Twemoji images (CC-BY 4.0). `assets/` must contain `Inter-Regular.otf`, `Inter-Bold.otf` (rsms/inter v4.0 `extras/otf`) and `lucide.ttf` (npm `lucide-static`).


## v2.1 — what changed (Gantt fixes, sharper + annotated PDF reader, responsive tables)

### 1. Gantt / timeline (`src/timeline.rs`, `src/database.rs`)
| Bug | Root cause | Fix |
|---|---|---|
| **App freezes the moment a bar is dragged / resized** | `resp.drag_delta()` (reads egui's input state) was called *inside* `ctx.data_mut(|d| …)`, which holds the context write-lock → the same thread tried to take the read-lock again: a **self-deadlock** | read the delta first, then lock. Regression test `gantt_drag_past_the_edge_stays_bounded_and_commits` |
| Chart widens forever while dragging towards the edge (memory blow-up, millions of painted day columns) | the visible day range was computed from the *live drag preview*; the bar stayed under the pointer, so `dx` grew every frame | range comes from committed dates only, is monotonic per view, `dx` and the preview are clamped to it, scroll offset compensates when the chart grows to the left |
| No way to reach dates outside the screen while dragging | – | edge auto-scroll while dragging a bar |
| Only visible columns should cost CPU | all `min_day..=max_day` columns were painted every frame | culled to the clip rectangle |
| Dependency cycles (A→B→A) possible | `AddDep` had no check | cycle-safe iterative check; self-dependency refused |
| Dates outside years 1000-9000 could corrupt the ISO text | no clamp | clamped |

Incomplete code found and finished: the repo shipped **without its fonts** (`assets/Inter-*.otf`, `lucide.ttf` – the build did not compile; they are now generated: Inter v4 without its private-use alternates, Lucide 1.29 subset to the private-use range, `TEXT_SELECT` re-mapped), and **without `web/index.html`** (the loader the web build script copies).

### 2. PDF reader (`src/pdf.rs`, `src/pdfnotes.rs`)
* **Sharpness** – tiles used to be rendered at a scale quantised to 1/64 and drawn into fractionally positioned rectangles, so below 100 % every tile was resampled (up to 5 % scale error). Pages are now snapped to whole physical pixels and tiles are rendered at the exact on-screen scale (1/4096 resolution): one texel = one pixel at every zoom level and DPI. A test checks the tile size equals the page's pixel size for 25 %-300 % on desktop and phone viewports.
* **Annotations** – highlight, underline, strike-through, sticky notes with comments, freehand pen, eraser, 5 colours, notes list with per-annotation comments, *Copy as Markdown*. Stored in the workspace (`Workspace::pdf_annots`, page-relative coordinates) so they survive restarts and every zoom level.
* **Open / close time** – parsing is deferred by one frame (the reader is on screen immediately), the open time is displayed, closing drops the engine, hayro caches, every texture and the remote download and asks for one more frame so GPU memory is released.
* **RAM** – the tile cache is capped by *bytes* (96 MB native / 40 MB web) instead of by tile count (up to 140 × 1 MB), trimmed to what is on screen 2.5 s after the last input, thumbnails of far pages are dropped; the idle app saves every 30 s instead of 4 s and wakes up every 30 s (10 s when a password lock is set).

### 3. Responsiveness
* Database table: names, headers, dates, formulas and chips are truncated with an ellipsis inside their own cell (no text spills into the neighbouring column), the name column shrinks on phones and the *OPEN* button stays visible on touch screens.
* PDF reader: toolbar buttons drop out in priority order on narrow screens (no overlap down to 320 px), the annotation bar wraps to two rows, the notes list becomes a full-screen sheet on phones.

### Build
```
cargo test --lib                                   # 50+ headless tests
WASM_OPT=0 ./build-web.sh                          # → ../public/webapp   (needs wasm-bindgen-cli 0.2.130)
./build-windows.sh                                 # → ../public/downloads (mingw-w64) or `cargo build --release` on Windows
```

## v3 — what changed in this round

| Area | Change |
|---|---|
| **Gantt links** | The ● connector is now permanent (faint when idle, bright near the bar) and lives in a strip right of the bar that counts as "hover", so it cannot disappear on the way to it. It supports drag-to-link *and* click-to-link (click ●, then click the dependent bar; Esc cancels). Root cause: the dot was only registered while the bar itself was hovered, and egui hit-tests against last frame's widgets. |
| **PDF text layer** (`src/pdftext.rs`) | hayro only rasterised pages, so there was no text. A recording `Device` now runs hayro's interpreter per page and stores every glyph (Unicode + exact box, incl. invisible OCR text) in page points. On top: drag-select, double-click word, triple-click line, Ctrl+C, floating toolbar (copy / highlight / underline / strike / comment / colour), text-anchored annotations (`quads` + `quote`, exported in the Markdown notes), Ctrl+F search with highlights. Extraction is lazy, LRU-capped (48 pages) and dropped when the reader closes. Touch devices pan with drag. |
| **Website preview** | Web: `/api/preview` proxy (Next.js) strips X-Frame-Options / CSP, injects `<base>`, sandboxes the page, reader-view fallback, SSRF guard. Windows: native WebView2 child window (`wry`), created when a preview starts and **dropped immediately when it stops** (max 2 alive). The PDF reader follows the same rule: engine, text cache and textures are released on close. |
| **Inline marks** | `rich::align_baselines` calibrates every font used in a block to the body baseline and row height; focused and unfocused blocks share the same geometry. Test: `marked_text_keeps_baseline_and_row_height` (8 sizes). |
| **CRM** | Templates *Sales CRM* (Contacts + Deals pipeline, relation, probability, weighted-forecast formula, board/table/calendar) and *Support tickets*. |
| **Build** | Fonts regenerated (see above). Windows: `cargo build --release --target x86_64-pc-windows-gnu` (mingw or `zig cc` as linker). Web: `build-web.sh` → `public/webapp`, served by Next.js at `/webapp/index.html`. |

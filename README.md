# Lumen 12.0 — Markdown tables, two-way relations, data safety, Android-optimised

* **Markdown everywhere** — GFM tables are parsed by *every* entry point (paste, import, AI tools, MCP, `append_markdown`) through one pre-pass
  (`src/mdtable.rs`): `| a | b |` + `|---|:-:|` becomes a real **Table block**; tab-separated spreadsheet cells paste as a table too; export (page, copy,
  print, history) writes GFM tables back. Table block: click-to-edit cells with inline Markdown, Tab / Shift+Tab / Enter navigation, add / remove
  row & column, sort, alignment, header row, *Copy Markdown* and **Convert to database** (header → properties with inferred types: number, checkbox,
  date, URL, select; the block becomes a linked database view). `/table` inserts a blank one.
* **Relations work in both directions** — `Workspace::relation_backlinks`: a row page lists every row (in any database) that points at it through a
  Relation property, grouped by database + property (assign a task to a person → the person's page shows the task). No second property to maintain.
* **Data safety on every build** — native/Android: atomic writes (temp file → fsync → rename), `app.ron.bak`, hourly rolling snapshots in `backups/`
  (72 kept), automatic recovery from the newest readable copy when the main file is damaged (the damaged file is kept as `app.corrupt-*.ron`).
  All builds: a "previous state" slot (≥ 10 min old), a once-per-upgrade copy of the previous version's data, unreadable state kept raw under its own
  key (never overwritten silently), and on the web `navigator.storage.persist()` so the browser will not evict the workspace.
* **Scrolling** — root cause of the laggy / stuck feel on laptops: egui treated Windows precision-touchpad deltas (fractional "lines") and fast
  touchpad flicks (> 8 px per event) like mouse-wheel notches and ran them through ~100 ms of exponential smoothing. `vendor/egui` now applies
  touchpad motion 1:1, only whole-line wheel notches are smoothed; the page scroller (`src/smooth.rs`) follows the fingers directly, measures their
  velocity and glides with real fling friction when they lift (OS inertia is respected: no double momentum).
* **Start-up memory** — nothing web-engine related starts with the app (WebView2 is created only while a *website embed* preview is open). The real
  costs were fonts: the 50+ MB system CJK fonts (Windows / Android) were read into RAM at start, and 19 embedded script fonts were registered.
  Script fonts are now **lazy** (`theme::ensure_scripts`): a font is loaded the first time text in that script is typed, pasted, imported or opened.
* **UI** — the four "New …" rows became one **New ▸** entry with a fly-out (hover or tap); the AI panel slides in / out (cubic ease, content keeps its
  width while it moves, the page does not re-wrap).
* **MCP from WebAssembly** — a browser tab cannot listen on a port, but it can *call* `127.0.0.1`. `lumen-mcp --relay 7979 --token … --origin <site>` is a
  loopback relay (127.0.0.1 only, bearer token, Origin + Host checks, CORS / Private-Network-Access for exactly one origin). Set the app's bridge URL to
  `http://127.0.0.1:7979`; desktop AI clients use `http://127.0.0.1:7979/mcp`. The website also hosts the same relay (`/api/mcp`, `/api/bridge/poll`).

Build everything: `./build-all.sh` (WebAssembly → `../public/webapp`, signed arm64 APK → `../public/downloads`, Windows x64 exe + zip).

---

# Lumen 11.0 — Agents: built-in AI, MCP server, Android-optimised build

* **Lumen AI** (`src/ai.rs`, `src/aiui.rs`) — an agent panel that builds pages, databases, dashboards, canvases, flows, automations and scripts through one tool registry (`src/agent_tools.rs`, `src/agent_build.rs`). Providers: Gemini, OpenAI, Claude, OpenRouter, Groq, Mistral, DeepSeek, xAI, Together, Fireworks, Cerebras, Ollama, LM Studio, custom OpenAI-compatible. Access levels, approvals, undo.
* **MCP server** (`src/mcp.rs`) — cloud bridge (`/api/mcp` on the site), local HTTP, and the headless `lumen-mcp` binary. See [MCP.md](MCP.md).
* **Knowledge base** (`ai-knowledge/*.md`) — read by the agent on demand; extend it with [AI.md](AI.md). Roadmap: [ROADMAP.md](ROADMAP.md).
* **Android** — `[profile.android]` (thin LTO, stripped), `./build-android.sh`; `./build-all.sh` builds web + APK + Windows into `../public`.

---

# Lumen 9.1 — every script, exact pinch-zoom, Android-first build

* **Complex-script text shaping** (`vendor/epaint`, a patched epaint 0.31.1 + [rustybuzz](https://crates.io/crates/rustybuzz) = HarfBuzz in Rust).
  egui draws one glyph per character, which is why Bangla / Hindi / Tamil … looked "broken" (no conjuncts, vowel signs on the wrong side,
  unjoined Arabic). Now every run of a complex script is shaped per word with the font that supplies it; a shaped cluster keeps one layout
  entry per character (cursor, selection, editing and wrapping are unchanged) and is drawn from up to four glyph quads per entry.
  Covered: Bengali, Devanagari, Gurmukhi, Gujarati, Oriya, Tamil, Telugu, Kannada, Malayalam, Sinhala, Thai, Lao, Khmer, Myanmar, Tibetan (system font),
  Arabic (also Persian, Urdu, Pashto …), Hebrew, Armenian, Georgian, Ethiopic, Cyrillic / Greek / Vietnamese (Inter) and CJK
  (web: lazily fetched Simplified-Chinese / Japanese / Korean subsets from `web/fonts`; Android and Windows: the system fonts).
* **Right-to-left**: rows are reordered visually (Unicode bidi in its simplest useful form): RTL runs are mirrored as a whole, numbers and Latin
  words inside them stay left-to-right, brackets are mirrored, and a paragraph that starts with a right-to-left letter is right-aligned.
  Test: `cargo test --lib shaping_`.
* **Automations screen**: the header and the tab strip are a fixed block above the scroll area (content can no longer be drawn over them), and the top bar
  no longer shows — or acts on — the page that is hidden behind the workspace.
* **PDF pinch zoom** is driven by the *measured* centre and spread of the raw fingers (`Event::Touch`) instead of egui's derived gesture: the page point
  between the fingers stays between the fingers while the fingers move, in both axes, at every zoom level.
* Build: `./build-web.sh` (copies the CJK subsets too), `./build-android.sh` (signed arm64 APK), `./build-windows.sh`. Outputs land in `../public`.

# Lumen 6.0 — Automations, recurring tasks, reminders

New in this release (details and the cloud roadmap in [AUTOMATION.md](AUTOMATION.md)):

* **Rules** — *when* (row added · property changes / becomes X · a date arrives ± N days at HH:MM · on a schedule · manual) *if* (the same filters
  database views use) *then* (set property / date, create row, move row to another database, rename, add to page, notify, duplicate, trash).
  11 one-click **recipes**, **Run now** for bulk application, an **activity log**, a master switch.
* **Buttons** — `/button` puts a one-click button anywhere; it acts on the database row it sits on. ⚙ / right-click / long-press edits it.
* **Recurring tasks** — repeat rules on any date property (daily, weekly on chosen weekdays, monthly, yearly; ends never / on a date / after N);
  completing a task rolls the date and resets the status, or keeps the record and spawns the next copy; "count from completion"; habits that roll by themselves.
* **Reminders** — on a row's date (moves with it) or standalone; time, lead time, repeat. **Windows toast notifications** on the desktop,
  **push notifications** in the browser (service worker, works in a background tab / installed PWA), quiet hours, inbox with unread badge.
* **Redesigned upload / embed card** — one design for photos, PDFs, files, embeds, bookmarks, page icons and covers: segmented *Upload | Link*,
  animated dashed drop zone that reacts to dragging, accepted-format chips, recognised-service chips, stacked layout on phones.
* Mobile: every new screen is touch-sized and works down to 320 px wide.
* Local time zone support (`GetTimeZoneInformation` on Windows, the browser's zone on the web) — "today" is now your today.

Build: `bash build-web.sh` → `../public/webapp` (served at `/app`), `bash build-windows.sh` → `../public/downloads`.
Tests: `cargo test --lib` (127 tests, 14 for the automation engine).

---

# Lumen

A free, offline-first **Notion alternative written entirely in Rust** with [egui](https://github.com/emilk/egui) — no web engine, no JavaScript UI.
One codebase builds a native **Windows** app and a **WebAssembly** app for the browser.


## Android app (new) — see [ANDROID.md](ANDROID.md)
Signed arm64 APK built by `./build-android.sh` (served at `/downloads`): exact-density text rendering (as crisp as WebAssembly), edge-to-edge window with
exact system-bar / cutout insets, content that stops on top of the keyboard, a real soft-keyboard `InputConnection`, native Storage-Access-Framework file
dialogs, system Back handling, clipboard, haptics, notifications and Android print / Save-as-PDF. `./setup-android-env.sh` installs the headless toolchain.

## Mobile update 3 — keyboard, headings, PWA (increment on 5.6)
* **Keyboard policy** (`vendor/eframe/src/web/kbd.rs`, wired in `app_runner.rs::touch_keyboard`). On touch devices the hidden `<input>` is focused **only by a deliberate tap inside the text field being edited** (short, motionless touch). Carets that appear by themselves — a new block, a widget added with `/widget`, a new page, Enter — never open the keyboard, so you can keep scrolling. A **swipe closes the keyboard** and is never followed by a re-open. A caret that stays **outside the visible area for > 1.1 s** (below the keyboard) closes it, which removes the open → scroll → close → open loop. After Android's *Back* hid the keyboard, tapping the field brings it back. Timing uses the browser's event timestamps, so a busy main thread cannot turn a swipe into a "hold". Mouse / desktop browsers keep the old behaviour.
* **Caret stays visible**: when the visible height changes (keyboard opens/closes, rotation) the block being edited scrolls its caret into view once (`editor.rs`).
* **Headings**: the page title is a wrapping, smaller heading on phones (30 → 26 → 22 pt for long titles; pasted line breaks become spaces) instead of one 40 pt line that ran off the screen. The top bar is 40 pt on short screens (landscape / keyboard up).
* **Canvas sizing guard** (`events.rs::get_display_size`): `devicePixelContentBoxSize` is cross-checked against `contentBoxSize × devicePixelRatio`; a browser or emulator that reports CSS pixels there no longer makes the whole UI dpr× too large.
* **PWA / native feel**: `manifest.webmanifest` (standalone, maskable icons), `sw.js` (offline, per-build cache), Apple touch icon, light/dark theme colours. Android: Chrome → *Install app*. iOS: Safari → *Share → Add to Home Screen*. The Next.js site serves the app at `/app` (and `/webapp`) and the Windows build at `/downloads`.
* **Tests**: 113 native tests (new: wrapping heading fits 320–430 pt, keyboard-policy contract). End-to-end browser test (Chromium device emulation, real CDP touch events, Pixel 7 + iPhone 14 profiles): tap opens the keyboard, typing doesn't blur, swipe closes it, no re-open while scrolling, no flicker/loop when the viewport shrinks.
* **Build**: `./build-web.sh` → `../public/webapp`; `./build-windows.sh` → `../public/downloads/Lumen-windows-x64.{exe,zip}` + `SHA256SUMS.txt`. Fonts: `assets/Inter-*.otf` (Inter 4.0 minus private-use glyphs) and `tools/build-fonts.py`.

## 5.6 — mobile release notes (increment on 5.5)
* **Touch is predictable again — the 2× canvas cap is removed.** 5.5 capped the canvas pixel density at 2× on touch devices. The canvas is sized in *device* pixels by the browser's `ResizeObserver`, while touch coordinates are CSS pixels, so on 3× phones the logical screen was 1.5× larger than the real one and **a tap landed on a different widget than the one under the finger**. `native_pixels_per_point()` (`vendor/eframe/src/web/mod.rs`) is the plain `devicePixelRatio` again, exactly like before 5.5. (The rest of 5.5 — keyboard, gestures, scrolling popups — is unchanged.)
* **Content size control** — ⋯ (top right) → **CONTENT SIZE**: `−` / `+` buttons, a slider (finger-friendly, claimed so it never scrolls the menu) and a `100 %` reset chip, 70 – 180 %, stored per page (`Page::zoom`, old backups load with 100 %). On a phone you can also **pinch with two fingers**. Only the *page content* scales — text, headings, the title, bullets, check boxes, callout icons, table of contents. Menus, drop-downs, pop-ups, the bottom toolbar, the top bar and the sidebar are never scaled.
* **Sidebar animation** — eased (cubic in-out, 0.34 s) on phones **and** Windows/desktop, and synchronised with the page: the page and the top bar are laid out *once* at the width they will have when the sidebar has finished (`frozen_layout` in `app.rs`) and slide along with the sidebar edge — no text re-wrapping on every frame. On phones the drawer now *pushes* the page (with a shadow and a dimming backdrop that closes it) instead of covering it.
* **Hover-only controls work on touch.**
  1. *Sticky hover*: a tap no longer ends the hover (`touch.rs` drops the `PointerGone` that follows a tap), so whatever a mouse reveals on hover stays visible after the tap and disappears on the next tap elsewhere. Tooltips are disabled on touch (they would hang on screen).
  2. *Always visible on touch* (`touch::reveal`): **Change cover / Reposition / Remove** (wrapping to a second row on narrow screens), **Add icon / Add cover**, sidebar row **＋ / ⋯** and the *close sidebar* chevron, **Copy** on code blocks, calendar day **＋**, database **Open** buttons, dashboard widget **⋯**.
* **Fonts and vendor data are rebuilt by the build** — `assets/*.otf|ttf` (see `tools/build-fonts.py`) and `vendor/eframe/data/icon.png` were never committed; a fresh clone did not compile. They are now restored (Inter 4.0 with private-use glyphs stripped, Lucide subset).
* **Tests** (111, all green): sticky hover (web *and* native event order), content size scaling / clamping / old-file loading, slider maths and a real pointer click on `+`, cover buttons visible on touch at 320 / 360 / 1000 pt, frozen sidebar layout.

## 5.5 — mobile release notes (increment on 5.4)
* **Keyboard no longer flickers.** The hidden `<input>` that receives typing used to follow the caret *outside* the visible area when the block being edited sat below the screen / keyboard; focusing an input outside the viewport makes the browser scroll or resize the page, which closed and re-opened the keyboard in a loop. It is now `position: fixed` and clamped to the visible area (`vendor/eframe/src/web/text_agent.rs`), the blur grace period is 0.7 s, the viewport meta has `interactive-widget=resizes-content`, and the editor scrolls the caret (plus 72 pt of margin for the toolbar) into view while typing.
* **Faster on phones.** The web canvas is capped at 2× pixel density on touch devices (`native_pixels_per_point`, 55 % fewer pixels than a 3× screen), the wasm profile is `opt-level = 2`, and popups that are taller than the screen scroll instead of running off it.
* **Tap-and-hold is easier and no longer fights selection.** Hold time 0.36 s, jitter tolerance 22 pt (was 10), swipe threshold 10 pt. *Hold + lift* = right-click menu (with *Select word / Select block / Select all*), *hold + move* = drag / selection, *swipe* = scroll. The ⋮⋮ handle of the active block opens the block menu with a single tap, so no gesture is ever required for it.
* **Page dashboards can be edited and removed** — visible *Edit* and *Remove* (tap twice) buttons in the dashboard header (the old *Remove* lived in a hidden menu).
* **Dashboard widgets inside pages** (`src/inlinew.rs`) — `/widget` places any of the 33 widgets as a block anywhere in a page (no full dashboard needed). Always-visible settings and ⋯ buttons: settings, replace, width, height, plot type, delete.
* **Page hierarchy popup** — tap the page title in the phone top bar (`hier_ui`): path chips, the whole page tree (expandable), *new sub-page here*.
* **Undo · redo · ⋯ are always visible on phones** (`topbar_narrow`, laid out from computed widths so nothing is pushed off-screen); search / favourite / dark mode / lock moved into the ⋯ menu.
* **Timeline on touch** — taller rows and bars, wide grab zones on both ends of a bar, claimed grab zones (a finger on a bar drags immediately instead of scrolling), a 40 pt ● link handle (drag onto a bar, or tap ● then tap a bar; *Cancel* button), pointer fallback so a lifting finger still completes a link.
* **Templates** — one column of full-width, finger-sized cards in a scrolling list with a close button on phones.

## 5.4 — mobile release notes
* **Scrolling never selects.** A swipe only scrolls. Root cause: browsers fire `pointerdown` *before* `touchstart`; eframe's web backend treated both as input, so the press reached the editor before `src/touch.rs` knew it was a finger and the scroll turned into a drag (marquee). `vendor/eframe` (patched 0.31.1, see `vendor/eframe/PATCHES.md`) now delivers a finger once and in order, and the gesture layer re-orders defensively (unit-tested).
* **Tap-and-hold.** Hold ≈ 0.45 s (a ring grows, the phone buzzes). *Lift* → right-click menu of whatever is under the finger (blocks, text, database rows/cards, sidebar). *Drag* → real drag starting at the press point (marquee / text selection / drag & drop).
* **Selection toolbar** wraps to the screen width (buttons are finger-sized on touch) and the text menu has *Select word* / *Select block* on touch.
* **Keyboard.** The hidden `<input>` that receives typing is no longer blurred/re-focused after every keystroke, on every tap, or on every frame in which the editor briefly had no focus (grace period), and `focus({preventScroll})` is used.
* **Gallery view** lays cards out in explicit rows (`gallery_grid`) — no more endless sideways growth.
* **Dashboard** toolbar / time-window chips / edit hint / quick-add / sub-pages wrap on phones (headless test `home_dashboard_fits_a_phone_screen`).
* Fonts (`assets/*.otf|ttf`) are regenerated by `tools/build-fonts.py` (Inter without private-use glyphs + the Lucide subset).

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

## 10.0 — Flow (node-based automation), Android release pipeline, website

* **Flow** — new page type (*New flow*): a data-flow graph with named multi-input / multi-output ports, branching, parallel HTTP, list nodes, a LumenScript
  node with the workspace API, dry-run, run report, templates, variables, schedule, JSON import / export. Mobile-first (tap-to-wire, pinch, bottom-sheet inspector).
  Design, node reference, safety limits and the roadmap: **[FLOW.md](FLOW.md)**. Code: `src/flow.rs` (engine + tests), `src/flowui.rs` (editor).
* **Binary assets restored** — a fresh clone did not compile (fonts / icons were never committed). Inter (private-use glyphs stripped), Noto scripts, the Lucide subset,
  PWA icons and CJK web subsets are regenerated by `tools/build-fonts.py`, `tools/make-icons.py` and the commands in `ANDROID.md`.
* **Website** (`../src`, Next.js) — landing + download page, `/app` (WebAssembly), `/downloads/*` (APK, EXE), `/api/flow-proxy` (CORS relay for the web build).
* Headless build recipe (Debian/Ubuntu, no GUI): `./setup-android-env.sh` → `bash build-web.sh` → `./build-android.sh` → `bash build-windows.sh`.

---

## v12 additions (this release)

* **AI agent UI redesign** — seamless, borderless panel (flat header, plain-text answers, one-line tool log, soft composer with model / access drop-downs). Settings are a two-column page: *Model* (provider + model drop-downs, key, refresh models from the server), *Behavior*, *On-device*, *Connections*.
* **Optimised requests** — stable prompt prefix (provider prompt caching, Anthropic `cache_control`), cached tool schemas, history windowing (90k chars cloud / 14k on-device), tiered tool-result truncation, retry with back-off on 429/5xx, token usage shown per session, compact mode (short prompt + 18 core tools) that is automatic for on-device models.
* **On-device AI** — Ollama, LM Studio, llama.cpp server and Jan are first-class providers (`127.0.0.1`), *Detect runtimes* lists the installed models. Android: run the runtime in Termux; cleartext loopback is allowed in the manifest.
* **`[[Page]]` links** work inside normal text (drawn as links, click opens or creates the page; stored verbatim so graph / search / AI see them).
* **Background schedules** — a flow set to “Run every N seconds” and scripts with “Repeat every N s” are driven by `flowbg.rs` on every frame, not by the page that is open. (Browser: hidden tabs are paused by the browser; Android: the process is frozen when the app leaves the screen; Windows: runs as long as the app is open.)
* **Live data demo** — flow node *Mock API stream* (`flow.rs::mock_json`), one-click demo (`demo.rs`, Ctrl+K → “Live data demo”) and the web page `/live` with the server-side stream `/api/demo/stream` (JSON or SSE).
* **Site backend** (Next.js, repo root) — `/api/ai-proxy`, `/api/flow-proxy`, `/api/preview`, `/api/hooks/<channel>` (Postgres), `/api/bridge/poll` + `/api/mcp` (MCP relay for external agents).

### Build (headless Linux)

```bash
bash tools/fetch-assets.sh      # fonts, icons (not stored in git); needs python3 + fonttools
bash build-web.sh               # -> ../public/webapp   (WebAssembly)
bash build-windows.sh           # -> ../public/downloads/Lumen-windows-x64.exe (+zip)
bash build-android.sh           # -> ../public/downloads/Lumen-android-arm64.apk
```

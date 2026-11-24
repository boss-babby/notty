# Lumen — plan for the next pass (after 15.0)

This file is the single hand-over document for the next session. It lists, per feature of the 15.0 wish list,

1. what **shipped** in 15.0 (and where the code is),
2. what is **good** about the current state and what is **not** (honest verdicts),
3. the exact remaining steps, ordered, with file / function names and a test to write,
4. the order in which to do it and what to watch for.

Conventions: *Done* = compiled for Windows, Android and WebAssembly, unit-tested where logic is pure. *Partial* = usable, known gaps below.
*Not started* = nothing in the tree yet — no stub, no dead code.

---

## 0. Status board

| # | Feature | 15.0 status | Where |
|---|---------|-------------|-------|
| 1 | Database rows open in a **right-hand side peek**, "Open as page" | **Done** (desktop / tablets ≥ 820 pt; phones open full pages) | `src/peek.rs`, `database.rs` (`A::Open`, `A::OpenPage`), `app.rs` |
| 2 | **PDF → note** ("→ Note" in the PDF panel) with deep links to page + box | **Done** | `src/extract.rs`, `pdfnotes.rs`, `pdf.rs` (`Request::start`), `editor.rs` (link click) |
| 3 | Full-vault **Markdown + YAML front-matter + assets/** export | **Done** | `src/vault.rs::export_zip`, `io.rs::page_markdown_with` |
| 4 | **Notion ZIP / Obsidian vault** importer | **Done** (pages, folders, CSV, pictures, front-matter) | `src/vault.rs::import_zip`, Import dialog → "ZIP vault" |
| 5 | **Argon2id** vault KDF, silent upgrade of old passwords | **Done** | `src/auth.rs`, `lock.rs` (`LockEvent::Upgraded`) |
| 6 | **Storage manager** (size breakdown, history pruning) | **Partial** — no hourly `backups/` exist in this tree yet | `vault.rs::storage_report/prune_history`, `app.rs::storage_ui` |
| 7 | OS keychain (Windows Credential Manager / Android Keystore) | **Not started** | see §6 |
| 8 | Visual page-history **side-by-side diff** | **Not started** (the existing dialog shows a change summary only) | see §7 |
| 9 | Multi-pane **split view**, workspace **tabs**, hover **link previews** | **Not started** | see §3 |
| 10 | **Synced blocks / `![[Page#Heading]]` transclusion** | **Not started** | see §4 |
| 11 | Template gallery: `/template`, "New from template" on databases | **Partial** — a gallery exists (13 built-ins); the slash command and the database button do not | see §5 |
| 12 | One-key **Daily Notes** | **Partial** — exists as `Ctrl+J` (`Workspace::open_today`); `Ctrl+D` is already the Dashboard | see §8 |
| 13 | Focus mode | **Done before 15.0** (`Ctrl+.`) | `app.rs::focus_mode` |
| 14 | Unified full-text search (pages + PDFs + canvas + big tables) | **Not started** | see §9 |
| 15 | Local AI / offline research desk | **Not started** | see §10 |
| 16 | Mobile page-menu restructure (MOBILE-UPDATE.md leftovers) | **Partial** — bigger rows + switches done; cards and typeface tiles open | see §2 |

Delivery items (all in this repo): `build-web.sh`, `build-android.sh`, `build-windows.sh`, `build-all.sh`, `setup-android-env.sh`;
the Next.js site serves `/app` (WebAssembly) and `/downloads/*` (APK, EXE, ZIP, SHA-256).

---

## 1. What is good / not good (verdicts)

**Good — keep and build on**

* **Side peek reuses the real editor** (`editor::show_page`): properties, blocks, comments, everything works in the panel with no
  second code path. The panel is a plain `egui::SidePanel`, so it composes with the AI panel and the sidebar.
* **PDF deep links are a tiny, stable scheme** (`lumenpdf://<media-id-hex>/<page>?r=x0,y0,x1,y1&a=<annot>`). They survive zoom,
  reflow and re-import because every coordinate is a page fraction; the note is plain text, so it exports to Markdown unchanged.
* **The vault ZIP code has no new dependency** (`miniz_oxide` was already in the tree) and is pure functions over `Workspace`, so it
  is fully unit-tested and identical on Windows / Android / WebAssembly.
* **Argon2id parameters (19 MiB, t = 2, p = 1)** are the OWASP minimum and run in ≈ 0.1–0.3 s on a phone; legacy hashes still open.

**Not good — fix soon**

* **The side peek shares `EditorState` with the page behind it.** Undo (`history::track`) only follows `ws.current`, so typing in the peek
  is not undoable and not snapshotted. Property pop-ups (`ed.db.popup`) are shared too: opening a select in the peek while the table
  is visible draws the popup from both call sites (harmless in release builds, an egui id-clash warning in debug).
* **The PDF reader is a modal, full-screen overlay**, so "PDF beside notes" is not possible yet: extraction closes the reader and
  opens the note. A deep link re-opens the reader. This is the main reason for §3.
* **`lumenpdf://` links only resolve for PDFs attached as `File` blocks.** PDFs on canvas nodes or in big tables are not found
  (the app shows a toast).
* **Vault import is lossy for rich content**: Notion callouts / toggles become plain Markdown quotes / bullets, Notion "database
  row" pages are imported twice (once as CSV row, once as page), attachments other than pictures are ignored.
* **Storage manager cannot show backups**: there is no `backups/` folder or hourly backup job in this tree (the wish list assumed
  one). Today only manual JSON backup + page history exist.
* **Mobile peek**: phones open full pages on purpose (a 400 pt panel would hide the table); Notion uses a bottom sheet there.
* **Argon2id is not wired to an OS key store**; the hash lives next to the workspace like before.

---

## 2. Mobile page menu leftovers (MOBILE-UPDATE.md)

Done in 15.0: 50 dp rows (60 dp with a subtitle), switch rows for *Small text*, *Full width*, *Favorite*, *Dark mode*
(`widgets::menu_switch`).

Still open:

1. **Rounded cards with hairline separators.** Wrap each group in `Frame::new().fill(pal.card).corner_radius(12)` — QUICK ACTIONS, PAGE
   STYLE, EXPORT, DANGER. Use `Frame::begin` / `end` so the existing `act = Some(n)` closure captures keep working.
   Replace the upper-case `section_label` rows by small muted headings *outside* the cards.
2. **Typeface tiles (Default / Serif / Mono).** Needs `Page::font: u8` (`#[serde(default)]`), a Serif and a Mono face in
   `assets/` + `theme::install_fonts`, and one family lookup in `theme::body()/bold()` used by the editor (`ed.scale` path in
   `editor.rs::show_page`). Ship **Noto Serif** and **JetBrains Mono** (OFL) — ≈ 300 KB each; subset to Latin to keep the APK small.
3. **Destructive row** "Move to Trash" in red at the bottom (`PalAct::Trash` exists; the row only needs `danger = true`).
4. **Available offline** switch → only meaningful once pages sync; keep hidden until §10.

Test: golden-image test is not available; add an `egui_kittest` snapshot only if the crate is already vendored — otherwise
manual check on a 360 × 800 pt emulator (`tools/phone-rig.sh`).

---

## 3. Split view, workspace tabs, hover previews (desktop first)

Design (smallest change that works):

* **State**: `app.rs::Tabs { list: Vec<Tab>, active: usize }` with `Tab { page: u64, split: Option<Pane> }` where
  `Pane::Page(u64) | Pane::Pdf(pdf::Request)`; persist the list in eframe storage (`Id::new("tabs")`), never in `Workspace`
  (it would sync to collaborators).
* **Tab bar**: 32 pt strip above the page in `update()` (skip in focus mode and on phones). Middle-click closes, Ctrl+W closes,
  Ctrl+Tab cycles, Ctrl+click on any link / sidebar row opens a tab. Drag to reorder (reuse `widgets` drag helper from the sidebar).
* **Split**: second `SidePanel::right("split")` with the same *animated width* code as `peek.rs` (copy the `peek_w` logic; make a
  shared `widgets::slide_panel` first — both files would shrink).
  * Page pane: `editor::show_page` with a **second `EditorState`** (`App::ed2`) so selection / menus never cross. This is the clean
    fix for the weakness described in §1 as well — give the peek its own `EditorState` too.
  * PDF pane: split `pdf::draw` into `draw_body(ui, rect, rd, …)` and the modal chrome; call `draw_body` with the pane rect. The
    reader state already lives in a thread-local (`READER`), so one reader at a time is enough for 15.x.
  * "PDF beside notes" becomes: *Extract → Note* keeps the reader open on the left, note on the right, deep links scroll the left
    pane instead of re-opening it (`Request::start` already carries page + annotation).
* **Hover previews**: in `editor.rs` the link hit-test (`link_hit`, `wiki:` prefix) already knows the hovered page title. After 350 ms
  of hover show an `Area` (Order::Tooltip) with title, icon and the first 6 text blocks (`Workspace::page`). Cache the
  rendered preview per (page, updated) in `ctx.data`. On touch: long-press instead of hover.

Risks: widget-id clashes between panes (scope every id with the pane number, see `database::SCOPE`), and `history::track` which
follows `ws.current` only (make it take a page list).
Tests: `tabs_persist_round_trip`, `closing_the_active_tab_selects_a_neighbour`.

---

## 4. Synced blocks and `![[Page#Heading]]` transclusion

Data model (`model.rs`): `BlockKind::Synced` with `link: Some(source_page)` and `media: source_block_id`.
Rules:

* **Synced block** — the *original* is an ordinary block flagged `synced_id = n` (new `#[serde(default)] u64` on `Block`); copies are
  `BlockKind::Synced { link: page, media: block }`. Rendering reads the original's children (same `indent` subtree) every frame; editing
  a copy edits the original in place (`ws.page_mut(src).blocks[i]`). "Unsync" copies the content into real blocks.
* **`![[Page#Heading]]`** — parsed in `rich.rs` as an inline *embed token*, rendered by the same code as a Synced block, source =
  the subtree under the first heading whose text equals `Heading` (case-insensitive). Without `#Heading` the whole page.
* Cycle guard: depth ≤ 4 and a visited set of (page, block).
* Collaboration: copies carry only ids, so the existing CRDT block ops already sync them.

Order: model + render (read-only) → editing through → `/synced` slash command and "Copy as synced block" in the block menu →
`![[…]]` autocomplete (reuse the `[[` suggestion popup in `editor.rs`).
Tests: edit-through, cycle guard, heading resolution, Markdown export writes the *resolved* text.

---

## 5. Template gallery

Existing: `model::TEMPLATES` (13 built-ins) + `app.rs::templates_ui` + `Workspace::add_template`.

1. `/template` in the slash menu (`editor.rs` slash list) → `Req::Templates`.
2. **User templates**: "Save page as template" in the ⋯ menu → `Workspace::user_templates: Vec<UserTemplate>` (`#[serde(default)]`,
   blocks + title + icon). Gallery shows them first with a delete button.
3. **Database templates**: `Database::templates: Vec<RowTemplate { name, props, blocks }>`; the "New ▾" split button (see Notion screenshot in the
   brief) lists them. `ws.add_row(db, preset)` already accepts presets — add the blocks copy.
4. Daily notes use template `#5` today; make the template configurable (`Workspace::daily_template: Option<u64>`).

---

## 6. OS keychain

* **Windows**: Credential Manager through `windows-sys` (`Win32_Security_Credentials`: `CredWriteW` / `CredReadW`). Store a random
  32-byte *wrapping key* under target `Lumen/<workspace-id>`; encrypt the Argon2id hash with it so a copied workspace file is useless
  without the user profile. Add a "Remember on this device" switch (stores the *derived key*, not the password) → unlock without typing
  on that machine, fall back to the password everywhere else.
* **Android**: `KeyStore` (`AndroidKeyStore`, AES-GCM key with `setUserAuthenticationRequired(true)` + `BiometricPrompt`) via a tiny
  `KeyKit.java` next to `LumenActivity.java` and two JNI calls in `android.rs` (follow the `clipboard` helper). Min SDK is 26 → fine.
* **Web**: no keychain; keep password only (WebAuthn PRF is the future option).
* Tests: the wrapping format is pure — unit-test seal/open with a fixed key; the OS calls stay behind `#[cfg]` and a `trait KeyStore`.

---

## 7. Page-history diff viewer

`history.rs::dialog` already lists versions with a summary. Add:

1. `diff_blocks(old: &[Block], new: &[Block]) -> Vec<Op>` — LCS on `(kind, text)` per block (Myers is overkill: pages are < 2 000
   blocks; use the O(n·m) table with a size cap and fall back to "changed" above 4 M cells). Inside a changed block a word-level diff.
2. Two-column view (stacked on phones): left = selected version, right = current page; red / green row backgrounds
   (`pal.danger` / accent at 12 % alpha), unchanged runs collapsed to "… 14 unchanged blocks".
3. "Restore this version" = existing `history::restore` (already undoable).
4. Backups: when hourly backups exist (`backups/YYYYMMDD-HH.json`, native + Android; IndexedDB on web) offer *"compare with backup …"*.
   Writing them is a 40-line job in `autosave.rs` (rotate: 24 hourly, 7 daily) — **do it first**, it also feeds §6 of the storage manager.

Tests: diff of identical pages is empty; one inserted block yields exactly one `Insert`; moved block = delete + insert.

---

## 8. Daily notes / focus mode

* Keep `Ctrl+J`; add **`Alt+D`** (and a sidebar "Today" row) because `Ctrl+D` belongs to the Dashboard and to "duplicate block" in
  the editor. Document in the shortcut sheet (`help_open`).
* Make the template selectable (§5.4) and add "Yesterday / Tomorrow" chevrons on journal pages (`Workspace::open_day(offset)`).
* Focus mode additionally: dim all blocks except the active one (`ed.focus` is known) and typewriter scrolling; remember the setting.

---

## 9. Unified search

* Build an incremental index on a worker (`std::thread` native / `wasm_bindgen_futures` chunks on web): page titles + block text,
  PDF text (`pdftext.rs` already extracts lines per page — index on first open and cache by media id), canvas node text
  (`canvas.rs` nodes), big-table cells (`bigtable.rs` has windowed reads; index only text columns, cap 200 k cells, flag
  "partial").
* Ranking: BM25-lite on lower-cased tokens, title ×3, recent ×1.2. Results grouped: Pages · PDFs (page number, deep link via
  `lumenpdf://`) · Canvas · Tables (row opens in the side peek).
* UI: extend `palette_items`; results stream in as the index fills.
Tests: tokenizer (CJK = per char), ranking order, PDF hit deep-link format.

## 10. Local AI

`ai.rs` / `aiui.rs` already talk to cloud providers and MCP. Next: an **on-device** option — `llama.cpp` through a tiny C ABI on Windows
and Android (arm64 NEON), model files in the media directory, never on web. Start with a 1–3 B instruct model for summarising a PDF
extraction note (§2 of this release) and for "ask this page"; keep the prompt builder in Rust (`agent_build.rs` has tool schemas).
Needs: model manager UI in the storage dialog (download / delete / size), a streaming decode loop on a worker thread, cancel button.

---

## 11. Suggested order for the next session

1. Hourly rotating backups (`autosave.rs`) → storage manager shows them (small, unblocks §6 + §7).
2. History diff viewer (§7) — pure logic first, UI second.
3. Own `EditorState` for peek (§1 weakness) → then `slide_panel` refactor → tabs + split view (§3).
4. Synced blocks (§4), then template gallery completion (§5).
5. Keychain (§6), unified search (§9), local AI (§10).
6. Mobile menu cards + typefaces (§2).

## 12. Release checklist (every pass)

```
cd notty
cargo fmt --check && cargo clippy --lib -- -D warnings   # fix new warnings only; the legacy tree still has ~100
cargo test --lib
./build-all.sh                                           # web + android + windows + source zip + SHA256SUMS
```
* Android: bump `version` in `Cargo.toml` (the version code is `major*10000 + minor*100 + patch`), keep `android/lumen-release.jks`.
* Web: never enable `WASM_OPT` (breaks the externref table with this toolchain, see `build-web.sh`).
* Site: `public/webapp` + `public/downloads` are served by the Next.js app (`/app`, `/downloads/...`); run `npm run build` there.

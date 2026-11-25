# Lumen 16.0 — tabs, unified search, history diff, hourly backups, synced blocks, device unlock

Platforms: **Windows x64** · **Android arm64 (APK, release-signed, 16 KB page aligned)** · **WebAssembly** (served at `/app`).
Everything below is the *same* Rust code on all three; the pure logic is unit-tested (`cargo test --lib`).

## What shipped in this pass

| # | NEXT-PASS item | State in 16.0 | Code |
|---|----------------|---------------|------|
| 1 | §11.1 Hourly rotating backups | **Done.** Every state write whose UTC hour differs from the last backup stores `backups/YYYYMMDD-HH.ron` (`$LUMEN_DATA_DIR/backups`, desktop: eframe storage dir, browser: `localStorage`, newest 3). Rotation keeps the newest 24 hourly files plus the newest file of each of the 7 older days; unparseable names are never touched. The storage manager shows count, size and the newest backup. | `backups.rs`, `app.rs::write_state`, `app.rs::storage_ui` |
| 2 | §7 Page-history side-by-side diff | **Done.** LCS over `(kind, indent, text)` with prefix/suffix trimming and a 4M-cell cap, word-level diff inside changed blocks, red/green rows, unchanged runs collapsed to “… N unchanged blocks”, two columns on desktop and stacked on phones. **Compare with backup …** restores or compares any hourly backup (also undoable). | `diff.rs`, `history.rs::dialog` |
| 3 | §3 Workspace tabs | **Done** (desktop / tablet). Tab bar above the page, active tab follows navigation, click / middle-click / Ctrl+W close, Ctrl+Tab and Ctrl+Shift+Tab cycle, drag to reorder, persisted in app storage (never synced), pruned when a page is deleted. Hidden in focus mode and on phones. | `tabs.rs`, `app.rs::tab_bar` |
| 4 | §4 Synced blocks + `![[Page#Heading]]` | **Done.** `BlockKind::Synced` renders live content of a page, a heading section or a block with its children; `![[…]]` typed in a block becomes a transclusion; *Copy as synced block* in the block menu and `/synced` paste a live mirror; *Unsync* copies the resolved blocks; cycles and depth > 4 end in a notice; Markdown export and printing write the resolved text. | `synced.rs`, `syncedui.rs`, `editor.rs`, `io.rs`, `print.rs` |
| 5 | §5 Template gallery completion | **Partial → mostly done.** `/template` opens the gallery; **Save page as template** stores the page body; user templates are listed first (open / use for daily notes / delete); the daily-note template is selectable and used by `open_day`. Database *row* templates remain open. | `usertpl.rs`, `editor.rs`, `app.rs::templates_ui` |
| 6 | §8 Daily notes / journal navigation | **Done.** `Ctrl+J` **and** `Alt+D` open today; the journal entry shows ‹ Yesterday · Today · Tomorrow › chevrons (40 dp rows on touch); the template is configurable; shortcuts are documented in the help sheet. | `usertpl.rs::open_day`, `app.rs` |
| 7 | §6 OS key store | **Done for Windows + Android, no key store on the web.** A random 32-byte device token is kept by the OS (Windows Credential Manager via `windows-sys`, Android Keystore AES-256-GCM via `KeyKit.java`) and the workspace stores only `seal(token, Argon2id hash)`; the lock screen unlocks automatically on that device. Palette: *Remember this device* / *Forget this device*. Sealing is pure ChaCha20-Poly1305 and unit-tested (wrong key, tampering, truncation, copied file). | `keychain.rs`, `auth.rs`, `lock.rs`, `KeyKit.java`, `LumenActivity.java` |
| 8 | §9 Unified search | **Done.** Index over page titles + block text, database rows, whiteboard node text and PDF pages (`lumenpdf://` deep links); BM25-lite, title ×3, recent ×1.2, last word as prefix, CJK per character, results grouped Page / PDF / Whiteboard / Table row and merged into the command palette. | `search.rs`, `app.rs::palette_items` |
| 9 | §2 Mobile page menu | **Partly done before this pass** (50/60 dp rows, switch rows). Rounded cards and typeface tiles are still open — see NEXT-PASS §2. | `widgets.rs` |
| 10 | §3 Split view, PDF-beside-notes, hover previews | **Not in 16.0.** Tabs and the diff/backup work were prioritised; the design is unchanged in NEXT-PASS §3. | — |
| 11 | §10 Local AI | **Not started** (needs a model runtime shipped per platform). Cloud AI + MCP stay as in 15.0. | — |

## Delivery (all reproducible, headless)

```
./setup-android-env.sh     # once: JDK 17, Android SDK 34 / NDK r26, Rust targets, wasm-bindgen
./build-deliver.sh         # web -> android -> windows -> source zip -> SHA256SUMS  (into ../public)
# or individually
./build-web.sh && ./build-android.sh && ./build-windows.sh && ./make-source.sh && ./make-sums.sh
```

Artefacts in `public/downloads` (served by the Next.js site through `/api/download/<file>`, counted in PostgreSQL):

| File | What |
|---|---|
| `Lumen-android-arm64.apk` | arm64-v8a, minSdk 26, targetSdk 34, release profile (thin LTO, stripped), 16 KB page aligned, signed v1+v2+v3 |
| `Lumen-windows-x64.exe` | portable x64 (mingw-w64 cross build, mimalloc) |
| `Lumen-windows-x64.zip` | the same executable, compressed |
| `Lumen-source.zip` | the exact source tree these builds came from |
| `SHA256SUMS.txt` | SHA-256 of every file above |

WebAssembly: `public/webapp` (`lumen.js`, `lumen_bg.wasm`, `index.html` with `<base href="/webapp/">`, manifest, service worker,
icons, `version.json`) — reachable at **`/app`** and `/webapp` through the rewrites in `next.config.ts`.

## Verification notes

* `cargo check --lib --tests`, `cargo test --lib` (new tests: `diff`, `backups`, `search`, `synced`, `usertpl`, `tabs`, `keychain`).
* `cargo clippy --lib -- -D warnings` is clean for the new modules; the legacy tree still carries its pre-existing warnings.
* Web: `npm run build` + the `/api/health`, `/api/download/*` routes; APK/EXE/WASM are linked from the landing page with sizes and
  checksums read from disk, and download counts come from PostgreSQL.

## Known limits (unchanged from 15.0 unless listed)

* The side peek still shares the page's `EditorState` (undo does not follow the peek); tabs are the workaround.
* `lumenpdf://` links resolve for PDFs attached as file blocks only.
* Notion callouts/toggles import as plain Markdown; unsupported attachments are skipped.
* Synced blocks are read-only in the copy (edit the original); nested depth is capped at 4.
* Android has no OS key store fallback for the web; “Remember this device” is Windows/Android only.

# Lumen data architecture — from "everything in RAM" to a disk-backed, budgeted, cloud-ready store

Status (13.3): the storage engine is integrated for **large datasets** (CSV ≥ 20 000 lines, or *New large dataset* in the command palette): rows live on disk
in `lumen-store`, the table is windowed, sort/search/aggregates run on workers, dashboards read the chunks directly, a memory governor owns the budgets.
Ordinary small databases are unchanged (still `Workspace.pages`). `RELEASE-13.3.md` lists what is done and what is not. Original analysis follows. All numbers below were measured in this repository (4 vCPU / 4 GB sandbox, release build).

## 1. Why start-up is heavy today (findings in the code)

| # | Finding (file) | Consequence |
|---|---|---|
| 1 | The whole workspace is **one serde blob**: `eframe::set_value(storage, STORAGE_KEY, &SavedRef{ws,…})` (`app.rs` `save`), loaded with `eframe::get_value::<Saved>` (`app.rs` `load_saved`). | Opening = read the entire file into a `String`, parse it into a `Vec<Page>`: peak RAM ≈ text + parsed tree + final structs. Saving serialises everything again. |
| 2 | `save()` keeps *copies* of the previous state (`PREV_KEY`, `UPGRADE_KEY`, hourly `backups/`) by cloning the old string. | The data exists 2–4× in memory during a save. |
| 3 | Rows **are `Page`s** (`Workspace.pages: Vec<Page>`). A `Page` carries blocks, history (versions), `Option<Database>`, canvas/flow, repeats, cover strings … and a `BTreeMap<u64, Val>` for the cells. | Every database row pays for fields only a few pages use. Measured lower bound below; the real `Page` is larger. |
| 4 | `Workspace::rows(db)` / `query_uncached` scan **all** pages; the old `query` cache hashed every value of every row *per frame*. | O(total pages) work per frame (the per-frame hash is already replaced by the revision key in `bigdata.rs`). |
| 5 | Charts/dashboards recomputed on the UI thread each frame (fixed in 13.2 by `bigdata.rs`: worker threads, revision cache, sampling). | UI stalls. |
| 6 | `Val::Text(String)`, `String` titles, `BTreeMap` per row: ~6 heap allocations per row. | Memory fragmentation, slow allocation at load, poor cache locality. |

### Measured: the same table held the way `Workspace` holds rows (title `String` + `BTreeMap<u64,Val>`; **a lower bound — `Page` has ~30 more fields**)

| rows | all in RAM (RSS) | disk-backed store: open | store RSS after open | store RSS during a 64 MB-budget session |
|---|---|---|---|---|
| 1 000 000 | **627 MB** (built in 0.65 s; + the parse of the file on top) | **0.06 ms** (index only, 245 chunks) | **1 MB** | 58 MB |
| 5 000 000 | **3 130 MB** — does not fit a phone, barely a laptop | **0.18 ms** (1 221 chunks) | **2 MB** | 71 MB |

On disk the table takes 52 B/row (1 M rows = 52 MB, 5 M = 265 MB) *before* compression.

## 2. Target architecture

```
                       ┌──────────────────────────── UI thread (egui, 60/120 fps) ────────────────────────────┐
                       │ draws only: visible window of rows, finished chart results, small metadata           │
                       └───────▲───────────────────────────▲─────────────────────────────────▲────────────────┘
                               │ window(start,n)           │ results (Arc<…>)                │ edits (upsert/delete)
                       ┌───────┴───────────────┐   ┌───────┴────────────────┐        ┌───────┴─────────────┐
                       │  RowStore facade      │   │  bigdata workers       │        │   WAL (append-only) │
                       │  (per database)       │   │  scan_num / scan_sel / │        │   + overlay (≤ 8 k) │
                       │  byte-budgeted LRU    │   │  aggregate (zone maps) │        └───────┬─────────────┘
                       └───────┬───────────────┘   └───────┬────────────────┘                │ checkpoint (COW chunks)
                               └───────────────┬───────────┘                                 │
                                        ┌──────▼─────────────────────────────────────────────▼──────┐
                                        │ Source trait: FileSource │ MemSource │ OPFS │ Object store  │
                                        └────────────────────────────────────────────────────────────┘
```

Principles
1. **Metadata in RAM, rows on disk.** Keep in memory only: page tree (titles, icons, parents), database schemas, views, and the *index* of each row table. A row's cells are fetched when needed.
2. **RAM is a setting, not a consequence.** One byte budget per platform (`Store::set_budget`): Android 48 MB, web 96 MB, desktop 256 MB; Android `onTrimMemory` → `set_budget(0)`.
3. **Immutable chunks + log.** Chunks of 4 096 rows are written once (copy-on-write), never edited in place; edits go to a checksummed WAL and are folded in by `checkpoint`. Crash-safe (torn tail ignored — tested), trivially snapshot-able (backup = copy the index + keep old chunks), and *exactly* the shape object storage and CDNs like.
4. **Columnar inside a chunk.** Aggregates and charts read one column of a chunk, not rows. Text is dictionary-coded, ids delta-coded, numbers dense.
5. **Zone maps.** Per chunk and numeric property: min / max / sum / count in the index. Unfiltered aggregates read **zero** chunks; filtered ones skip chunks that cannot match.
6. **Never block the UI.** All scans run on the `bigdata` worker pool (native/Android) or in idle slices (web, no threads without COOP/COEP + shared memory).
7. **Same engine, three storage back-ends** behind `Source` (file, memory, browser OPFS/IndexedDB) — and a fourth one for the cloud.

## 3. What was built and measured (`lumen-store`, 10 passing tests, std-only, no `unsafe`)

Components: chunk codec (+ checksum, + single-row decoder), index with zone maps, WAL with replay, overlay, copy-on-write checkpoint with chunk splitting, compaction, LRU over *compact encoded bytes* (≈ 5× more rows per MB than decoded rows), `window`, `get`, `aggregate_num`, `scan_num`, `scan_sel`, `scan_sel_values`, `Source` trait with `FileSource` (unix `pread` / windows `seek_read`) and `MemSource`.

| Operation (1 M rows → 5 M rows) | time | RAM |
|---|---|---|
| bulk load | 0.5 s → 2.9 s | peak 4–5 MB |
| open table | 0.06 ms → 0.18 ms | 1–2 MB |
| `count/sum/min/max` of a number column | **1.4 µs → 4.8 µs** (zone maps, 0 chunk reads) | – |
| filtered aggregate (`100 ≤ x ≤ 120`) | 21 ms → 104 ms | one chunk |
| group-by over a select column (full scan) | **22 ms → 117 ms** | 2 MB |
| numeric column scan | 27 ms → 133 ms | 2 MB |
| table scroll: 60 rows at the middle of the table | **3.6 ms → 3.2 ms** (≤ 2 chunk reads) | – |
| 10 000 random point reads (cold + warm, 64 MB cache) | 0.53 s → 1.4 s  (≈ 53 µs → 140 µs each) | cache ≤ budget |
| 1 000 durable edits (WAL, `write_all` per edit) | 2.6 ms → 3.9 ms | – |
| checkpoint after 1 000 edits scattered over *every* chunk (worst case) | 0.7 s → 0.8 s | – |

Reading it: scrolling and charts stop depending on table size. The all-in-RAM design needs 627 MB / 3.1 GB *before the first frame*; the store needs 1–2 MB and 0.2 ms.

Honest limits of the prototype (each has a planned fix, see §5): no compression yet; point reads decode a row by scanning ids/varints inside the chunk (a per-chunk sparse id index would make this O(1)); the checkpoint rewrites whole chunks (worst case above — in practice edits cluster); `Value` mirrors `Val` but the app does not use the store yet; no secondary indexes (sorting/filtering a column still scans it — 117 ms for 5 M rows, fine on a worker, not per-frame); the web `Source` is not written.

## 4. How loading becomes dynamic (the mechanism)

*Start-up*: read `workspace.meta` (page tree + schemas + views, a few hundred KB even for huge workspaces) → show UI. Row tables are opened lazily on first display: `Store::open` reads the index (≈ 100 B per chunk).
*Table view*: the scroll position → `window(first_visible_row, visible+margin)`; read-ahead of the next chunk in the scroll direction on a worker; at most 2–3 chunks live.
*Sort / filter*: a per-property **sorted run index** (`(value,id)` pairs, chunked like data) is built once on a worker the first time a view needs it and then reused; a filtered+sorted view = "list of row numbers", paged. Full-text search: trigram posting lists in segment files.
*Dashboards*: `bigdata` asks the store for `aggregate_num` / `scan_sel_values` instead of snapshotting pages: no snapshot memory at all, exact results (no sampling needed up to hundreds of millions of rows on desktop).
*Opening a row as a page*: the row's blocks (rich content) live in a separate "page body" table keyed by id, loaded only when the row is opened.
*Editing*: `ws.edit_row(id, |r| …)` → overlay + WAL append (µs) → UI reads see the overlay immediately; checkpoint on idle / every 8 192 edits / on suspend.
*Memory pressure*: one `MemoryGovernor` owns all budgets (row cache, image/tile cache of the PDF reader, galley/texture caches of egui, font groups) and shrinks them in priority order on `onTrimMemory`, `visibilitychange` or when RSS passes a watermark.

## 5. Phased migration (each phase ships on its own and is reversible)

**Phase 0 — quick wins inside today's model (1–2 weeks, no file-format change)**
* `Box` the rarely used `Page` fields (`history`, `db`, `canvas`, `flow`, `repeats`, covers) → a row `Page` shrinks by a large factor; measure with `size_of`.
* Replace `BTreeMap<u64,Val>` by a sorted `SmallVec<[(u32,Val);8]>`; `Val::Text` → `Arc<str>`/small-string; intern repeated texts (statuses, tags).
* Save without cloning: stream the old file to `backups/` with `fs::copy`/rename, never hold `old` in memory; incremental hourly snapshots.
* Swap the global allocator for **mimalloc** (native/Android): lower fragmentation, memory returned to the OS.
* Split `STORAGE_KEY` into `meta` and `pages` so that opening a workspace with few rows does not parse every page body.
* Replace `rows(db)` scans by a `HashMap<db, Vec<id>>` maintained incrementally.

**Phase 1 — row tables on `lumen-store` (3–5 weeks)**
* `trait Rows { window, get, upsert, delete, aggregate_num, scan_* }` in `model.rs`; `Workspace` keeps `HashMap<db_id, Store>` instead of `Page`s for `row == true`.
* One-time migration on first run: old `app.ron` untouched (it stays as the safety copy that `save()` already keeps), rows streamed into stores with `bulk_append` (5 M rows in 3 s).
* `database.rs` table/list/board use `window()`; `dashdata.rs` uses scans (the `Snap` code in `bigdata.rs` disappears); formulas/rollups evaluate per visible window plus lazily for aggregates.
* Acceptance: 5 M-row workspace starts with < 80 MB RSS, first frame < 300 ms, 60 fps table scroll, edit latency < 2 ms.

**Phase 2 — platform back-ends and governance (2–3 weeks)**
* Android: `FileSource` in `getFilesDir()`, `onTrimMemory` JNI hook → governor; optional `memmap2` for read-only chunk access.
* Web: `OpfsSource` — `FileSystemSyncAccessHandle` exists only in dedicated workers, so a small storage worker owns the files and answers chunk requests; the UI keeps a synchronous in-memory chunk cache fed by prefetch (reads that miss show a skeleton row, never block). Fallback: IndexedDB with one chunk per key.
* `lz4_flex` (pure Rust) per chunk: expected 2–3× smaller files and faster cold reads on slow flash; `Source` stays byte-oriented.
* Desktop: `rayon` for parallel chunk scans (not on wasm); `std::simd`/auto-vectorised loops for sum/min/max (already a straight pass over `chunks_exact(8)`).

**Phase 3 — cloud (when needed; design decisions that are free to make now)**
* Chunks are immutable → **content-addressed objects** (`sha256` → key in S3/R2/GCS). A table = a *manifest* (the index). Dedup between versions and users is automatic; clients fetch chunks with HTTP range/GET on demand and cache them in the same LRU (`Source` for HTTP).
* The WAL becomes the **operation log**: ops carry a hybrid logical clock + actor id; rows converge by per-cell last-writer-wins, blocks (rich text) by a sequence CRDT (Rust: `diamond-types`/`yrs`), deletes by tombstones. Offline-first: the device is always the source of truth for its own edits.
* **End-to-end encryption**: encrypt each chunk (XChaCha20-Poly1305) with a key derived from the user's password (the app already has a credential flow); the server only ever sees opaque chunks and manifests — a strong selling point (see `STRATEGY.md`).
* Server: Rust (`axum`), Postgres for accounts/manifests/permissions, object storage for chunks, WebSocket/SSE for op fan-out; presence and comments are small separate channels. Shared databases get per-database ACLs and an audit log.
* Cost model: storage ≈ US$0.015/GB-month on R2-class storage; 1 M rows ≈ 52 MB raw (~20 MB compressed) ⇒ under US$0.001/month per million rows per user: sync is nearly free to host, which makes a generous free tier affordable.

## 6. Other performance work (everywhere, not only data)
* **Frame budget**: a `FrameBudget` that cuts optional work (thumbnails, link previews, tile rendering) when a frame passes 8 ms; heavy work only on workers.
* **Caches with ceilings**: egui galley cache and glyph atlas size, PDF tile cache (exists — put under the governor), emoji atlas entries, decoded image LRU.
* **Start-up**: lazy script fonts (exists), lazy emoji pack decode (exists: one WebP per distinct emoji), defer AI/MCP/automation init until used, load page *bodies* on demand.
* **Wasm size** (24 MB today): `wasm-opt -Oz` once the externref issue is solved, split fonts/emoji into fetched packs with a service-worker cache, enable Brotli on the host.
* **Profiling in CI**: `cargo bench` on `lumen-store`, a headless frame-time test with a 1 M-row workspace, RSS watermark assertions; fail the build on regressions.

## 7. Risks
Data safety is the product: every phase keeps the old blob as a read-only safety copy; the migration is idempotent and resumable; chunk + index + WAL carry checksums; compaction is write-new-then-rename. Formula/rollup/relation semantics depend on cross-row reads — Phase 1 must route them through the facade and keep results equal to today's (property tests comparing the old in-memory path with the store path on random workspaces). Web storage quotas and eviction (`navigator.storage.persist()` already requested) need the user-visible "export a backup" path to stay prominent.

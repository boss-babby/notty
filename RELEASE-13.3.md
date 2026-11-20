# Lumen 13.3 — large datasets on disk, memory governor, chart reference/trend lines

## Implemented (all tested; see the unit tests in `src/bigtable.rs`, `src/bigdata.rs`, `lumen-store/src/lib.rs`)
* **`lumen-store`** (std-only crate, 11 tests): chunked columnar row store, zone maps, WAL + crash recovery, copy-on-write checkpoints, compaction, byte-budgeted LRU,
  single-row decode, aligned column decoders, lock-light shared scans for workers.
* **Large datasets** (`src/bigtable.rs`, `src/bigui.rs`):
  * a CSV with ≥ 20 000 lines is imported *streaming* (2 passes, never a `Vec<Vec<String>>`) straight into a store; `Command palette → New large dataset (sample)` creates 1 000 000 rows in ~1 s.
  * The page is a normal database page (schema, title); its rows are **not pages** and **not in RAM** — `Workspace.bigs[page] → store directory`.
  * Windowed table: only the visible rows are read (≈3 ms per scroll step); click a cell to edit (text / number / date / select), checkbox toggles, `+ Row`, delete, **sort by any column** and **search** run on a worker thread; **Export CSV** streams the table.
  * Dashboards read the store directly: number cards (zone maps, no I/O), bar/line/pie *group-by* (select, checkbox, date buckets, text), scatter — **exact**, off the UI thread, cached by dataset version, never sampled.
* **Memory governor** (`src/memgov.rs`): per-platform row-cache budget (Android 24 MB, web 48 MB, desktop 96 MB); Android `onTrimMemory` (JNI) and "in the background for 20 s" shrink caches and drop dashboard results; app-memory figure in the table's status line.
* **Phase-0 items done**: `Workspace::rows` cached per data revision (no O(pages) scan per call), mimalloc on Windows, pending edits of large tables flushed on save.
* **Charts**: goal reference line (widget goal) and a least-squares **trend line** (widget setting "Trend line") on bar charts.

## Not done (so nobody is surprised) — see `DATA-ARCHITECTURE.md` §5
* Ordinary databases (small ones) still live in `Workspace.pages`; shrinking `Page`, boxing its rare fields and splitting the saved blob are not done. Formulas / relations / rollups do not exist on large datasets.
* Filters of a dashboard widget and drill-down lists are not applied to large datasets; heat-map and list/board/gallery views are not available for them.
* Browser: large datasets are held in memory for the session (no OPFS/IndexedDB source yet); export CSV to keep them.
* Cloud sync, collaboration, marketplace, forms/publishing, iOS/macOS/Linux packages and the other items of `STRATEGY.md` are product programmes, not part of this release.

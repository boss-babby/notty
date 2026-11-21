# Lumen 13.7 — dashboards on the canvas, vector zoom, workspaces

* **Dashboard widgets on the canvas** (`src/canvasdash.rs`, `canvas::Kind::Dash`) — any widget of the dashboard catalogue (charts, numbers, tasks, timers,
  calendars …) can be placed on a board: *Widgets ▸ Dashboard widget…*. The node stores the id of a widget in the hidden widget store, so data sources,
  filters, the settings panel and drill-down are the same as on dashboards. A widget is a display until it is activated (double-click / Enter / ▶ in the
  selection toolbar); Esc or a click elsewhere hands the board back to the pointer. ⚙ opens the settings, the replace button picks another widget.
  Duplicating a node clones its widget.
* **Vector-style zoom** — the canvas no longer swaps cards for grey bars at 45 %. Text, icons, connector labels, group labels, paddings and corner radii
  are pure functions of the zoom, so nothing hides, clips or overlaps; the bar stand-in only appears when glyphs are below ≈ 1.6 px. Dashboard widgets are
  laid out once in design units and their shapes are scaled with `Shape::transform` (`Painter` range of the layer), which keeps charts and text in exact proportion.
* **Tables follow page zoom** — `tableblock::draw` multiplies text (through `theme::ContentScale`), cell padding, column widths and row heights by the page
  size; saved widths / heights stay in design units, so zooming never rewrites your table.
* **Multiple workspaces** (`src/workspaces.rs`, sidebar header menu) — create, switch, rename (the name field), delete. The open workspace keeps the classic
  storage key (all data-safety slots still apply); the others are stored as JSON under `lumen_ws_<id>` and swapped in memory, so a switch needs no storage access.
* **Site** — the Next.js site in the parent folder serves the WebAssembly build at `/app`, the signed arm64 APK and the Windows build at `/downloads`.

Build: `./build-all.sh` (needs `tools/provision.sh` once). Missing binary assets: `emoji-noto.pack` from `tools/build-emoji-pack.py`, `XITSMath-Regular.otf` from the XITS 1.302 release.

# Lumen 13.5 — Whiteboard (edgeless) release

## 1. Research: what AFFiNE's whiteboard ("edgeless mode") offers
Sources: AFFiNE docs / feature pages, the 0.7 "edgeless" launch notes, BlockSuite documentation, hands-on comparison articles.

| Area | AFFiNE capability |
|---|---|
| Canvas | infinite pan / zoom, page ⇄ edgeless toggle per document, zoom bar (fit, −, %, +), minimap-less navigation by frames |
| Shapes | rectangle, rounded rectangle, ellipse, triangle, diamond; fill, stroke colour / width, dash, corner radius, shadow, **scribbled (hand-drawn) style**, text inside shapes |
| Pens | brush, **highlighter**, **eraser**, colour + width |
| Connectors | straight / curved / orthogonal, arrow heads, labels, auto-attach to shape edges, shortest path |
| Text & notes | free canvas text, note blocks (rich text) that can be hidden "on page", sticky notes |
| Frames | named frames, **frame navigator panel**, drag to reorder, background colour, **presentation mode**, export a frame |
| Mind map | mind-map element with styles / layouts, auto layout |
| Embeds | linked docs as cards / embedded views, images, bookmarks, YouTube / Figma / GitHub embeds |
| Templates | flowchart, mind map, Kanban, … |
| Export | PNG, SVG, PDF, copy as image |
| Whiteboard embedded in a doc | "embed synced doc" shows the edgeless view inside another page; peek view with expand / open / copy link / info |
| Collaboration | real-time CRDT (BlockSuite / Yjs) |
| AI | make-it-real, mind map from text … |

## 2. Gap analysis (Lumen 13.4 → 13.5)
Lumen already had cards, text, to-dos, page cards, images, links, groups, pen, connectors, mind-map tree layout, templates, minimap,
presentation of groups, grid / snap / guides, Obsidian JSON interchange, undo, find. Missing and now **implemented**:

| Gap | Lumen 13.5 |
|---|---|
| Shapes | `Kind::Shape`: 9 geometries (rectangle, rounded, ellipse, diamond, triangle, hexagon, star, parallelogram, pill), text inside, 4 looks (tinted / outline / solid / **hand-drawn**), solid / dashed / dotted outline, colour, text size. Shape-accurate hit testing (an ellipse corner is not the ellipse). Shape tool (`S`): drag to draw, tap for a default size. |
| Highlighter + eraser | Pen tool has three modes (`P`, `Shift+P`, `E`): pen, translucent highlighter, eraser (removes whole strokes, one undo step). |
| Frames | Frame tool (`F`): drag a rectangle, auto-named *Frame N*; **frame navigator** (top-right): list, jump, present as slides. |
| Zoom bar | bottom-left: − / % (click = 100 %) / +; top-right: frames, fit, options. |
| Templates | + *Diagram (shapes)*, *Retrospective*, *Org chart*. |
| Export | **SVG** of the whole board: file download (Windows: Downloads folder, Android: Storage Access Framework, web: browser download) or clipboard. Whiteboard blocks print as inline SVG. |
| **Whiteboard inside a page** | `/whiteboard` block (`BlockKind::Whiteboard`): live thumbnail of the board in the document, *Edit board* opens the **peek view** — a full canvas editor in an overlay with info, copy SVG, export SVG, open as page, full screen, close. The board is an ordinary canvas page under the host page (sidebar, search, links), and several blocks can show one board. |
| Persistence / interchange | New node fields are `serde(default)` (old workspaces open unchanged); shapes round-trip through the Obsidian-compatible JSON (`lumen` extension fields). |

Competitive advantages over AFFiNE: native Rust renderer (WebAssembly, Android, Windows from one codebase, no Electron / WebView), relational
databases and dashboards live on the same board as cards, offline-first with atomic saves + rolling snapshots, built-in AI / MCP agent tools.

Still open (honest list): PNG / PDF export, shape rotation and corner-radius slider, shape shadows, real-time multi-user editing, "on-page" toggle for notes.

## 3. Mobile / Android
* One-finger swipe on empty board pans; shape / frame / pen tools take over the finger only while active (`touch::set_passthrough`).
* The dock keeps six 44 px buttons on phones (select, hand, pen, shape, frame, +); option panels (pen modes, shape palette) open above it and are 250–290 px wide.
* The zoom bar moves above the dock on narrow screens; the peek view uses the whole screen when it is narrow (margins shrink to 6 px) and has a full-screen toggle.
* Release APK: `[profile.android]` (opt-level 3, thin LTO, strip, no debug info), arm64-v8a only, 16 KB ELF alignment, signed v1–v3, zipaligned.

## 4. Code map
* `src/canvas.rs` — `Kind::Shape`, `geo_points`, `geo_contains`, `add_stroke`, 3 new templates, JSON.
* `src/canvasdraw.rs` — `draw_shape` (fan-filled meshes, sketch wobble, dashes), highlighter, `thumbnail`.
* `src/canvasui.rs` — tools (`Tool::Shape / Frame`, pen modes), dock + option panels, selection-toolbar shape controls, frames panel, zoom bar, eraser, SVG acts.
* `src/canvasexport.rs` — SVG exporter (+ 3 unit tests).
* `src/whiteboard.rs` — the page block and the peek view.

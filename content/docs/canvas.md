# Lumen 8.0 — Canvas

An infinite whiteboard (Obsidian Canvas / Miro-lite) that lives next to pages and databases in the sidebar: **New canvas** (sidebar, right-click menu, `Ctrl+K`).
A sample board (*Launch board*) ships with a fresh workspace.

## What you can put on a board
| Kind | What it is |
|---|---|
| **Card** | Markdown-lite text: `# ` headings, `- ` bullets, `> ` quotes, `[ ] ` check boxes (click to tick), `---` rules. Auto-grows with its text. |
| **Text** | Frameless title / label (3 sizes). |
| **To-do list** | Check list with a progress bar. Enter continues the list. |
| **Page** | Live card of any page, database or canvas of the workspace (icon, title, excerpt). Click ↗ or double-click to open. |
| **Image** | Upload, drop a file on the window, paste a picture address. |
| **Link** | Web bookmark. |
| **Group** | Labelled frame — moves its contents, and is one slide in presentation mode. |
| **Pen** | Free-hand ink, 8 colours, 3 widths. |

**Connectors** — drag from the dot on any side of a card to another card (or to empty space: a new connected card appears). Select a connector for colour,
curve / straight / elbow, dashed, arrow heads, label, reverse.

## Gestures
| | Desktop | Phone / tablet |
|---|---|---|
| Pan | wheel / trackpad, Space+drag, Hand tool (`H`), middle button | swipe on empty board |
| Zoom | `Ctrl`+wheel, pinch, `+` / `−`, `Shift+1` fit, `Shift+2` selection, `Ctrl+0` 100 % | pinch |
| Select | click, `Shift`+click, drag a box, `Ctrl+A` | tap, hold + drag a box |
| Move / resize | drag the card / its corner handles (guides + grid snapping, `Alt` = free) | drag the card / handles |
| Add | double-click, dock buttons, `N` `T` `L` `G`, right-click menu | double-tap, `+` in the dock |
| Mind map | `Tab` child, `Ctrl+Enter` sibling, *Arrange branches as a tree* | selection toolbar `⊕` |
| Menu | right-click | press and hold |

Also: undo / redo (`Ctrl+Z`, top-bar buttons), duplicate `Ctrl+D`, group `Ctrl+G`, copy / cut / paste (`Ctrl+C/X/V`, between canvases and as text),
lock, bring to front / send to back, align, distribute, tidy into a grid, find on canvas `Ctrl+F`, minimap, grid dots / lines / off, snap, **Present** (each group = a slide, ← →).

Templates: Mind map, Kanban board, SWOT, Flowchart, Weekly plan.

**Interchange** — *Copy as Obsidian JSON* puts an Obsidian-compatible `.canvas` document on the clipboard; pasting such JSON (from Obsidian or Lumen) imports it.
Lumen-only kinds travel in an extra `lumen` field and survive the round trip.

## Code map
* `src/canvas.rs` — model (serde, forward compatible), undo, geometry / connector routing, auto layout, templates, JSON import / export. 16 unit tests.
* `src/canvasdraw.rs` — painting with level of detail (hundreds of cards stay smooth), minimap.
* `src/canvasui.rs` — gestures, shortcuts, floating chrome, menus, popovers.
* Extending: add a variant to `canvas::Kind`, paint it in `canvasdraw::draw_node` — it is then selectable, movable, connectable and exportable.

## Bug fix: drop-downs inside pop-ups
**Symptom** — in a pop-up (e.g. the automation *Button* editor) a drop-down worked once; afterwards the dialog looked dimmed and every click only closed the list.

**Cause** — egui draws a `ComboBox` list as an `Order::Foreground` layer. `app::modal` (backdrop + dialog) was *also* `Foreground` and re-raised itself every frame.
egui keeps the previous relative order of all layers that ask to be on top in the same frame, so a list opened for the **second** time (its layer already existed, low
in the order) ended up under the dialog and under the dimming backdrop: unreachable, dimmed, and any click hit the backdrop. Only the very first opening worked because a
brand-new layer is pushed on top.

**Fix** — `widgets::HOST_ORDER` (`Order::Middle`) for everything that can host a drop-down (`modal`, `popup_area`, context menus, dashboard gallery / inspector). egui's own
pop-ups stay on `Foreground`, so they are above their host *by construction*; no per-frame raising heuristics. A drop-down that is still open when its dialog closes is closed too.
Regression test: `tests::dropdown_inside_a_dialog_works_every_time` (fails without the fix).

## Build
`bash build-web.sh` → `../public/webapp` (served at `/app`) · `bash build-windows.sh` → `../public/downloads/Lumen-windows-x64.{exe,zip}` · `cargo test --lib` (162 tests).
Fonts / icons that the original repository did not contain are rebuilt by `tools/build-fonts.py` (Inter 4.0 without private-use glyphs, Lucide subset); `web/icons/*` and `vendor/eframe/data/icon.png` are generated artwork.

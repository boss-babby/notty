# Lumen 11.1 — AI-agent UI fixes, `[[page]]` chips, big-data performance, memory

## AI agent UI

* **Crash fixed** (`src/aiui.rs::dropdown`): closing / choosing an entry of *any* drop-down (chat history, model, behaviour/“What may
  Lumen AI do?”, compact mode, provider, MCP access) called `ctx.input()` *inside* `ctx.data_mut()`. egui's context lock is not re-entrant, so
  the app deadlocked / panicked at that moment. The clock is now read before the data lock is taken. A whole-source scan found no other
  nested `Context` access of that kind.
* The chat panel also no longer indexes `threads[cur]` blindly (e.g. after “Delete all chats”).
* **Mobile layering fixed**: the full-screen chat sheet lives on `Order::Foreground`, drop-downs were on `Order::Middle` → they opened *behind*
  the sheet (chat and settings). Drop-downs of the AI UI now use `widgets::popup_area_on(.., Order::Tooltip, ..)`, the topmost layer.

## `[[Page]]` links (editor)

* A wiki link is rendered like a Notion page mention: a rounded, hoverable pill with a page icon and the page title; the brackets (and the
  `Target|` part of `[[Target|label]]`) are hidden. Hover highlights the pill, click opens the page. No more blue underlined hyperlink.
* While the caret is *inside* a link it turns into raw, editable text (`[[Title]]`); move away and it becomes a pill again.
* Typing `[[` opens a drop-down under the caret with matching pages (prefix matches first, “Create “…”” for new titles).
  ↑/↓ select, Enter/Tab insert, Esc dismisses, tap/click works on phones. The stored text stays `[[Title]]` (graph, search, AI unchanged).
* Code: `rich.rs` (`job_ex`, `wiki_chips`, `wiki_chip_shapes`), `editor.rs` (`wiki_trigger`, `wiki_sugs`, `insert_wiki`).

## Performance with thousands of rows / live streams

| Hot spot | Before | Now |
|---|---|---|
| `Workspace::page(id)` | linear scan → every row loop O(rows²) | validated id→index map, O(1) |
| `Workspace::query` (filters + sorts) | recomputed every frame | memoised by a fingerprint of view + rows (not used for formula/rollup/relation) |
| relation titles (`database::load_titles`) | cloned every title every frame | rebuilt only when a title changes |
| table / list view | built every row every frame | virtualised: only visible rows are built |
| dashboards | O(rows²) filters per widget per frame | O(rows) on top of the cached query |

## Memory (Windows)

Most of the ~150 MB seen in Task Manager at start-up is *not* Rust heap: it is the OpenGL driver, the font atlas, the embedded fonts being
paged in and start-up allocations that are never touched again. `main.rs` trims the working set 4 s, 20 s and 90 s after start
(`SetProcessWorkingSetSize`), so Task Manager shows what is really needed; pages fault back on demand. The release profile no longer
keeps debug info.

## Build outputs

`build-web.sh` → `public/webapp` (WebAssembly, served at `/app`), `build-windows.sh` → `public/downloads/Lumen-windows-x64.exe|zip`,
`build-android.sh` → `public/downloads/Lumen-android-arm64.apk`. Font/icon assets that are not in git are fetched by `tools/fetch-assets.sh`.

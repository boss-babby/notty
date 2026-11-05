# Lumen vs Notion — feature analysis & roadmap

*What Notion offers, what Lumen has today (v2.0), what is missing and what we can add beyond Notion.*
Legend: ✅ done · 🟡 partial · 🛠 next · 🗓 planned · 💡 idea

## 1. Notion feature inventory and Lumen status

### Editor / blocks
| Notion capability | Lumen | Notes |
|---|---|---|
| Text, H1–H3, bulleted / numbered / to-do / toggle lists | ✅ | Nesting with Tab / Shift+Tab, children travel with their parent |
| Quote, callout, divider, code block | ✅ | Code has syntax colouring + copy button |
| Table of contents, page links, sub-pages, backlinks | ✅ | |
| Slash menu, Markdown shortcuts | ✅ | New **Media** section: Image, Video, Audio, Embed, Web bookmark, File |
| Image block | ✅ **new** | Upload (Windows/Web dialog, drag & drop) or paste a link; drag the right edge or use 25/50/75/100 %; caption; replace; open original |
| Video / audio / website embed | ✅ **new** | YouTube, Vimeo, Loom, Spotify, Google Maps, Figma, CodePen, Google Docs are recognised. Web build: **live iframe** (click *Live*). Windows build: thumbnail card that opens the system browser |
| Bookmark (link card) | ✅ **new** | Favicon, host, URL |
| File attachment | ✅ **new** | Stored inside the workspace, *Save* exports it |
| Paste a URL → image / embed / bookmark | ✅ **new** | Paste into an empty text block |
| Inline marks: bold, italic, strike, code | ✅ | |
| Inline underline, highlight, link | ✅ **new** | `__u__`, `==h==`, `[text](url)`, Ctrl+U, Ctrl+Shift+H, toolbar |
| Floating selection toolbar | ✅ **fixed** | Clicks were swallowed (see §3) |
| Inline colour per selection | 🟡 | Colour is per block today → 🛠 per-span colour (`{c3|text}` markup) |
| Multi-block select (marquee), bulk actions | ✅ **new** | Drag on empty space (auto-scrolls), Shift/Ctrl-click the handle, Ctrl+A; toolbar: turn into, colour, indent/outdent, move up/down, duplicate, copy as Markdown, undo, delete; keys Del, Ctrl+C/X/D, Esc |
| Undo / redo of block operations | ✅ **new** | Ctrl+Z / Ctrl+Shift+Z while no text field is focused (typing keeps the text field's own undo) |
| Simple tables, columns | 🛠 | Highest-voted editor gap |
| Equation (LaTeX), synced blocks, buttons, breadcrumbs | 💡 | |
| @mentions (people, pages, dates), reminders | 🗓 | Pages/dates first |
| Comments / discussions | 🗓 | Needs identity → server |

### Pages & workspace
| Notion capability | Lumen | Notes |
|---|---|---|
| Page tree, favourites, trash, templates, daily journal, command palette | ✅ | |
| Page icon: emoji | ✅ **fixed** | Colour (Twemoji), searchable, paste/type *any* emoji |
| Page icon: custom image | ✅ **new** | Upload or link, shown in sidebar, breadcrumbs, links, gallery |
| Cover: gradients | ✅ | |
| Cover: image from disk or URL, reposition | ✅ **new** | Gallery / Upload / Link tabs, drag to reposition, shown on gallery cards |
| Page width / text size, focus mode | ✅ | |
| Page history & restore | 🗓 | Snapshots per page |
| Share / publish to web, permissions | 🗓 | Needs server (the Next.js backend in this repo is the natural home) |
| Import / export | ✅ | Markdown, CSV, JSON; HTML & PDF export 🛠 |
| Search | 🟡 | Title + text search; add filters & recent history 🛠 |

### Databases
| Notion capability | Lumen | Notes |
|---|---|---|
| Table / Board / List / Gallery / Calendar | ✅ | Gallery cards now show cover pictures |
| Typed properties (text, number, select, multi-select, checkbox, date, URL) | ✅ | |
| Filters & sorts per view | ✅ | |
| Timeline view, groups / sub-groups, formulas, relations, rollups, files & media, person, email, phone, created/edited time | 🗓 | Relations + rollups are the biggest databases gap |
| Linked database views, templates per database | 🗓 | |
| Database automations | 💡 | |

### Collaboration & platform
| Notion capability | Lumen | Notes |
|---|---|---|
| Real-time collaboration | 🗓 | CRDT (e.g. `yrs`) + WebSocket sync server |
| Cloud sync / accounts | 🗓 | Optional, self-hostable; local-first stays the default |
| Mobile apps | 💡 | egui compiles to Android/iOS targets |
| Public API, integrations, web clipper | 💡 | |
| AI assistant | 💡 | Local-model or bring-your-own-key |

## 2. Where Lumen can be *better* than Notion
1. **Free & offline-first, no account.** Notion needs an online account for almost everything.
2. **Native speed.** A single Rust binary (~15 MB) – no Electron, no 300 MB RAM idle.
3. **Privacy by default.** Data is local; optional password lock with idle auto-lock.
4. **Two targets from one codebase:** Windows .exe and WebAssembly, identical behaviour (the headless egui tests drive both).
5. **Hackable.** MIT licence; data model is plain serde JSON.
6. Ideas to leapfrog: Markdown/Obsidian vault folder sync, backlink graph view, local full-text index, plugin API (WASM), spaced-repetition blocks, per-page encryption.

## 3. Bugs found & fixed in this release
| Bug | Root cause | Fix / test |
|---|---|---|
| Selection toolbar buttons did nothing | On the *press* frame the text field surrenders focus and `TextEdit` returns `cursor_range = None`; the editor treated that as “selection cleared” and dismissed the toolbar before the click completed | Only a *collapsed* range dismisses the toolbar. Test `clicking_the_selection_toolbar_formats_text` presses every toolbar button |
| Custom emoji “not working” | `migrate_icons()` ran **every frame** and converted any emoji that had a Lucide twin into a Lucide glyph, so the Emoji tab looked identical to the Icons tab; emoji outside the tiny 36-item list had no picker and monochrome / missing glyphs | Migration runs once (`icons_v2`). New picker: 300+ searchable emoji, paste any emoji, colour rendering via Twemoji images with font fallback, custom picture tab. Test `media_blocks_and_covers_render` |
| Links never opened on the Windows build | eframe was built without its `webbrowser` feature, so `ctx.open_url` was a no-op | `platform::open_url` uses `webbrowser` natively and `ctx.open_url` on the web |
| Repository could not be built | `assets/*.otf/ttf` were not committed | Fonts fetched/bundled; Lucide codepoints verified against the font (198/198) |
| Drag on the page scrolled it instead of selecting | `ScrollArea` drag-to-scroll captured the mouse | Disabled for the page (mouse wheel / scrollbar unchanged); cover is only a drag surface while repositioning |

## 4. Architecture notes
* `media.rs` – content-addressed media store (`%APPDATA%\Lumen\media` on Windows, local storage on the web), image pipeline (decode → ≤1600 px → JPEG/PNG), embed URL detection, Twemoji URLs, base64.
* `mediablocks.rs` – image / embed / bookmark / file blocks. `multisel.rs` – marquee, bulk actions, undo/redo. `pickers.rs` – icon picker, cover picker, cover banner. `emoji.rs` – catalogue.
* `platform.rs` – file dialogs (`rfd`), downloads, URL opening, live `<iframe>` overlay (web).
* Web storage limit: browsers cap localStorage at ~5 MB, so local media is limited to ~2.6 MB there (links are unlimited). Desktop has no practical limit. IndexedDB storage is the planned fix 🛠.

## 5. Suggested next milestones
1. **Tables & columns** (editor parity, highest vote).
2. **Per-span colour**, `@` mentions, dates & reminders.
3. **Version history** (snapshot every N minutes, diff, restore).
4. **IndexedDB media store** on the web (remove the 2.6 MB cap) and lazy loading.
5. **Relations / rollups / formulas**, timeline view.
6. **Sync + collaboration**: `yrs` CRDT, WebSocket server in the companion Next.js backend, share links & publish-to-web.
7. **HTML/PDF export**, web clipper, public API, Android build.


## 4. v3.0 status update
| Area | Status | Notes |
|---|---|---|
| Formula / relation / rollup | ✅ | `formula.rs`; two-way relations and rollups of rollups are the next step |
| Timeline + dependencies | ✅ | `timeline.rs`; baseline vs actual, critical path 💡 |
| Page history, undo / redo | ✅ | `history.rs`; per-database undo 🛠 |
| Print / PDF export | ✅ | `print.rs`; true PDF writer (no browser dialog) 💡 |
| PDF reader | ✅ | tiled renderer, LRU, zero idle memory; text selection & search need a text-extraction API in hayro 🗓 |
| Video | ✅ | HLS / captions 💡 |
| Mobile / responsive | ✅ | native Android / iOS targets 💡 |
| Collaboration, cloud sync, comments, permissions | 🗓 | needs the Next.js/PostgreSQL backend |
| Synced blocks, equations, columns, buttons | 🗓 | |

---

# v4.0 — root causes, sector playbooks and roadmap

## 1. What was wrong and how it was fixed

| Report | Root cause | Fix (file) |
|---|---|---|
| PDF highlights look poor, text looks brown / washed out | Highlights were translucent rectangles painted *over* the page (`alpha-over`). A 38 % yellow over black text gives dark brown text and a milky page; adjacent line boxes overlapped and showed darker stripes. Real viewers use a **multiply** blend. | `pdfnotes::bake` multiplies the highlighter ink into the page raster of each 512 px tile (anti-aliased rounded boxes, line boxes separated so nothing is multiplied twice). The tile cache key carries a signature of the page's highlights, so edits re-render only that page and the old tiles stay on screen until the new ones are ready. Night mode tints instead of multiplying. Underline / strike use text-height-relative stroke widths with round caps; pen strokes are Chaikin-smoothed (`pdfnotes.rs`, `pdf.rs`). |
| Drawings could not be moved | Only click-to-select existed; a drag on a mark panned the page. | `Notes::drag_edit`: drag the body to move, corner handles to scale (ink, free marks, notes move). Clamped inside the page, never flips, covered by unit tests. |
| Windows touchpad scrolling not smooth (web is fine) | The browser feeds egui pre-smoothed, inertial deltas; on Windows a precision touchpad delivers bursts of tiny `WM_MOUSEWHEEL` steps. The old code eased with a *first-order* filter (`x += (t-x)·k`) whose **velocity jumps on every event** — visible as steps. | `smooth.rs`: wheel input moves a target, the view follows with a **critically damped spring** (closed form, stable at any frame time, no overshoot, continuous velocity). Unit tests check convergence at 30–240 Hz and velocity continuity. |
| Touch: selection fights scrolling | A finger is a mouse drag, so a drag always meant *marquee / text selection / card move* and the page could not scroll. | `touch.rs`, a gesture layer in front of egui (`raw_input_hook`): **tap** is replayed unchanged, **swipe** becomes wheel events (every scroll area pans natively, with momentum), **long press** (0.42 s) starts a real drag — marquee, multi-select (further long presses add blocks), drag & drop, text selection — and **two fingers** pass through (pinch zoom). Drawing tools request raw input. State machine is unit-tested. |
| Table: overlapping text, filter popup overflow | Toolbar tabs and the right-hand tools shared one row regardless of width; filter rows used fixed 112 + 104 + 130 px fields in a 440 px popup; columns had fixed widths. | Toolbar wraps (tabs scroll, tools get a second row, icon-only below 480 pt); filter rows stack on narrow screens; popups never exceed the screen; **resizable columns** (per view, double-click resets). Headless tests sweep clicks at 1280 / 700 / 390 / 320 pt. |

## 2. Research: what productivity users expect, by sector

Compared: Notion, Obsidian (+ Canvas, Dataview), Airtable, Coda, ClickUp, Monday, Capacities, Anytype, AppFlowy, AFFiNE, HubSpot / Pipedrive (CRM).

| Sector | Core jobs | What Lumen has now | Highest-value additions |
|---|---|---|---|
| **Sales / CRM** | pipeline, contacts, forecasts, follow-ups | Sales CRM template (contacts ↔ deals relation, weighted forecast formula), board / table / calendar views, **dashboard pipeline charts** | activity timeline per contact, forecast report by month / owner, e-mail + calendar sync, forms for lead capture, duplicate detection |
| **Project management** | tasks, roadmaps, sprints | tracker template, Timeline view with dependencies + auto-schedule, board, calendar, dashboard task inbox | grouped tables + sub-totals, sprints / burndown, recurring tasks, rules & buttons |
| **Support / Ops** | tickets, SLAs, inventory | tickets template (priority, assignee, due), dashboard by status | SLA timers, forms, automations (assign, escalate), inventory template with stock rollups |
| **Marketing / content** | calendar, approvals | content calendar template, calendar view | approvals, channel analytics, linked views in campaign pages |
| **HR / recruiting** | pipeline, interviews | recruiting template (stage, rating, next step) | scorecards, offer letters from templates, e-mail templates |
| **Strategy** | OKRs, reviews | OKR template (status, quarter, progress) charted on the dashboard | roll-up of key results to objectives, review cadences |
| **Research / study** | reading, PDFs, flashcards | PDF reader with highlights / notes / export, reading list, **graph view** | `[[` autocomplete, tags, spaced repetition cards from highlights, citations |
| **Personal knowledge (Obsidian-style)** | links, graph, daily notes | page links, backlinks, `[[wiki links]]` in the graph, daily journal, **graph view with local graph** | `[[` autocomplete + unlinked mentions, Canvas (infinite whiteboard), tags as graph clusters, vault import |
| **Teams** | share, comment, co-edit | local password lock, history | CRDT real-time sync (small Rust server), comments + mentions, share links, roles |

## 3. Cross-cutting platform capabilities (ranked)

1. **Linked views + grouped tables with aggregates** — makes every page a dashboard; low risk, builds on `Workspace::query`.
2. **Rules & buttons** (trigger → action on database rows) and **recurring tasks** — the biggest productivity multiplier after views.
3. **Forms** → database rows (lead capture, tickets, applications).
4. **Canvas** — Obsidian's second pillar; reuse the graph's camera / hit-testing code.
5. **`[[` autocomplete, unlinked mentions, tags** — turns the graph from a viewer into the way you link.
6. **Real-time sync + comments** — requires a server; keep local-first (CRDT), end-to-end encrypted.
7. **Import** from Notion exports and Obsidian vaults to remove switching cost.
8. **PWA / offline cache**, push reminders (web) and toast notifications (Windows).
9. **Local-first AI assistant** (summaries, Q&A over the workspace; on-device model, nothing leaves the machine).
10. **REST API / webhooks** for integrations.

All of these are listed, with votes, at `/roadmap` in the web project.

## 4. Shipped in 4.0

* PDF: multiply highlights, smooth ink, move / resize drawings.
* Physics scrolling (spring) and touch gesture layer.
* Responsive database toolbar / filter / sort, resizable table columns, popups clamp to the screen.
* **Graph view** (`graph.rs`) — nodes: pages, databases, rows; edges: page links, `[[wiki links]]`, relations, hierarchy; force layout that sleeps when settled; zoom / pan / drag; hover focus; search; local graph depth 1-5; orphans / hierarchy / rows toggles; info card with links; Ctrl+G.
* **Dashboard** (`dashboard.rs`) — KPIs, per-database charts, task inbox with tick-off, upcoming / overdue, recent, favourites, activity; Ctrl+D.
* Templates: Recruiting pipeline, Content calendar, OKRs & goals (13 in total).

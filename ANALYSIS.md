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

# Flow — Lumen's node-based data-flow editor (v10.0)

*A visual programming surface for APIs, data, logic and your workspace. Sidebar → **New flow** (also `Ctrl+K` → "New flow").*

Code map: `src/flow.rs` (model, node catalogue, engine, 10 unit tests) · `src/flowui.rs` (editor, gestures, inspector, palette) ·
`src/scripthost.rs::run_script_with` (LumenScript bridge: `emit()`, `emit_to()`, flow globals) · `web/…/api/flow-proxy` (CORS relay, Next.js side).

## 1. Research summary — what the good node systems have in common

| System | What it gets right | What Lumen takes from it |
|---|---|---|
| **Node-RED** | Messages flow through wires; function nodes for anything custom; debug sidebar | Wires carry data, *Inspect* node + per-node value preview, *Code* node |
| **n8n** | Branching (IF/Switch/Merge), error outputs, expressions `{{ }}`, "execute up to this node", pinned test data | `If / Switch / Merge`, `{{path}}` templates, **Run up to here**, *Manual input* as pinned data, HTTP **error port** |
| **Make / Zapier** | Mapping UI, scheduling, templates | Templates gallery, schedule (`Run every N s`), *Set fields* mapping |
| **Unreal Blueprints / Houdini / Blender nodes** | Typed multi-port nodes, pure data nodes, strong visual language, collapse/organize | Named multi-input / multi-output ports, colour per category, tidy layout, zoom-level of detail |
| **Obsidian Canvas / Miro** | Fluid infinite board, touch gestures | Same gestures as Lumen's canvas (pan, pinch, long-press / double-tap) |

**Design decisions that follow**

1. **A flat DAG, not sub-graphs.** Loops are the number-one reason visual programs become unreadable. List nodes (`Map`, `Filter`, `Sort`, `Aggregate`…) iterate
   for you; per-item logic is a one-line LumenScript expression. Wiring a cycle is refused at connect time and repaired on import.
2. **Inactive outputs instead of control-flow wires.** A node may leave an output *inactive*. A node whose connected inputs are all inactive is *skipped*, recursively.
   That single rule gives If / Switch / Filter / error handling / "optional" branches, and `Merge` re-joins them.
3. **Fan-out and fan-in are first class.** One output → many nodes. Many wires into one input → a list (ordered by source node).
4. **Async without blocking.** HTTP and Delay are *pending* steps polled once per frame (`Run::step`). All ready nodes start together, so independent
   requests run **in parallel**; the UI never freezes (native: `ehttp` threads, web: `fetch`).
5. **Safe by default.** *Test* = dry run on a throw-away copy of the workspace (nothing saved, nothing notified). Scripts keep their step / row / notification limits.
   A failing node marks only its downstream as *blocked*; independent branches finish.
6. **Data are JSON.** No type system to fight; nodes explain what they expected ("Map list needs a list but got text").
7. **Mobile is not an afterthought.** Tap an output dot, then an input dot (no precision dragging); 40 px hit targets on touch; the inspector is a bottom sheet;
   pinch / two-finger pan; the palette is a full-width sheet; everything can also be done with mouse + keyboard.

## 2. Using it

| Action | Desktop | Touch |
|---|---|---|
| Add a node | double-click / right-click the board, **Add node**, drag a wire into empty space | **+ Add node**, double-tap, drop a wire on empty space |
| Wire | drag output dot → input dot | tap output dot, then tap input dot (a hint appears) |
| Select / move | click, `Shift`+click, `Shift`+drag box, drag | tap, drag |
| Pan / zoom | wheel / trackpad, `Ctrl`+wheel, pinch | drag the board, pinch |
| Delete | `Delete` / `Backspace`; click a wire → trash button | select → *Delete* in the inspector, trash button on a selected wire |
| Duplicate / copy / paste | `Ctrl+D`, `Ctrl+C`, `Ctrl+V` (works between flows, as JSON text) | inspector → *Duplicate*; ⚙ → paste JSON |
| Undo / redo | `Ctrl+Z` / `Ctrl+Shift+Z` or the top bar | top bar |
| Run | **Run** (live) · **Test** (dry run) · inspector: *Run / Test up to here* | same |

Settings (⚙): templates, **variables** (`{{vars.base}}`, `vars.base` in scripts), **schedule** (re-run every N seconds *while the page is open*),
arrange left → right, copy flow as JSON, import pasted JSON.

## 3. Node reference (37)

* **Inputs** — Manual input (JSON payload), Text, Number, Date & time (`text`, `date`, `ms`), Random number
* **Logic** — If (`true` / `false`, 10 tests), Switch (3 cases + default), Merge (list / object / concat / first), Compare, Logic (and/or/xor/not), Delay
* **Data** — Get field (`value` / `missing`), Set fields (`key = {{template}}`), Text template, Map list, Filter list (`pass` / `rejected`), Sort list, Limit list,
  Aggregate (count, sum, avg, min, max, join, unique, first, last, flatten), Math, Split text, Parse JSON (`value` / `error`), To JSON text, SHA-256,
  **Code (LumenScript)** — inputs `a b c`, outputs `out out2 out3`, `emit(x)` / `emit_to("out2", x)`, full workspace API
* **Network** — HTTP request: method, URL, headers, body (objects → JSON), outputs `body` / `status` / `error` (network failure **or** status ≥ 400), optional server proxy on the web
* **Workspace** — Query database (filter, limit → `rows` / `count` / `first`), Add row (object or list), Update row (find by property), Create page
* **Outputs** — Notify, Copy to clipboard, Inspect (pass-through debug), Result (listed in the run report), Note (sticky note)

Templates: `{{input}}`, `{{a}}` / `{{b}}` (port names), `{{a.items[0].name}}`, `{{vars.x}}`, `{{today}}`. A field that is *exactly* one `{{path}}` keeps its JSON type.

## 4. Web specifics

Browsers enforce CORS. In the WebAssembly build, switch **Via server proxy** on in an HTTP node: the request goes to `POST /api/flow-proxy`, which only accepts same-origin
callers, `http(s)` to **public** addresses (DNS-resolved, private / loopback / link-local / metadata ranges refused, no redirects followed), 15 s timeout, 4 MB cap, 90 req/min.
Android and Windows builds call APIs directly (the Android manifest already declares `INTERNET`).

## 5. Roadmap — how this grows into "everything is a flow"

**Next (v10.x)** — *engine reaches the app*
* **Triggers as nodes**: *Row changed* / *Row added* (from `automation::Runtime` events), *Button block*, *App start*, *Webhook* (needs the headless runner below).
* **Sub-flows**: select nodes → *Make a node*; the sub-flow's `Manual input` / `Result` nodes become its ports. Reuse + versioning.
* **For-each with an inner flow** (bounded, parallelism N) and **Try / Catch** wrapper nodes for explicit error routing instead of the generic `blocked` state.
* **Credentials vault** (`{{secret.github}}`) stored encrypted in the workspace and never exported; OAuth device flow for Google / GitHub.
* **Pinned data**: freeze a node's last output as test data, so you can iterate on the downstream half without re-calling an API.
* **Typed ports (optional)**: colour wires by inferred JSON type, warn on obvious mismatches, schema preview from the last run.

**Then (v11)** — *a flow runs without the UI*
* **Headless runner**: `flow.rs` has no egui dependency; compile it into a small server binary (or run it in a Next.js route via WASM) to execute scheduled / webhook flows
  while no browser is open; run history table in Postgres; the website becomes the control plane (`/flows`, logs, retries, cron).
* **Flow as API**: expose a flow as `POST /f/<id>` (inputs = Manual input, outputs = Result) with auth tokens and rate limits.
* **Realtime collaboration** on a flow (CRDT on nodes / wires; the graph is already a flat list of small structs with stable ids).
* **Web client automation**: *Browser step* nodes (fetch + HTML extraction with CSS selectors, RSS, sitemap crawl with politeness limits), *Screenshot / PDF* via headless runner.
* **AI nodes**: *Prompt*, *Classify*, *Extract JSON with schema*, *Embed + search workspace*, with the provider key held in the vault; a "describe the flow you want" assistant that emits
  the exported JSON (the format is stable and versioned: `lumen_flow`).
* **Data nodes**: CSV / Excel parse, XML, regex, date math, group-by, join (two lists on a key), diff of two runs, cache with TTL, rate limiter, queue / batch.
* **Observability**: run history per node (inputs, outputs, timings), re-run from a failed node, diff between two runs, flame chart of timings.
* **Marketplace-free extensibility**: *Custom node* = LumenScript + a declared port list (name, ports, params), shareable as JSON and registered into the palette at runtime.

## 6. Limits & safety

| Limit | Value |
|---|---|
| Nodes per flow | 300 |
| HTTP response size / Delay | 4 MB / 60 s |
| Script steps per node | 400 000 (LumenScript budget) |
| Rows created / changes / notifications per node | 200 / 3 000 / 25 |
| Cycles | refused when wiring, dropped on import, checked before every run |

Tests: `cargo test --lib flow` — paths & templates, map/filter/aggregate, branch skipping + merge, fan-in + cycle refusal, error blocking, script multi-output + workspace access,
database round trip, dry-run isolation, timers, template validity + JSON round trip, repair of damaged flows.

---

## 10.1 — Flow grows up: connectors, pickers, examples, and the "dialog stays small" fix

### The dialog-size bug (all pop-ups, whole app)
**Symptom:** type in a search box (Flow node palette, command palette, menus …), the dialog shrinks to the few results — and then it *stays* that small
(or the inspector card opens clipped, "Method" cut in half) even after the search is cleared.

**Root cause:** egui lays the content of an `Area` out inside **the rectangle the area had on the previous frame**
(`vendor/egui/src/containers/area.rs`, `Prepared::content_ui`: `max_rect = state.rect()`). Every Lumen pop-up is an `Area` with a `ScrollArea { max_height }` inside.
When the previous frame was small, the `ScrollArea` only saw that small height as "available", clipped its content to it, and reported the small size again —
a fixed point it could never leave.

**Fix (global, one place):** egui 0.31.1 is vendored (`[patch.crates-io] egui = { path = "vendor/egui" }`). The width is still tied to the last frame (menus set their own),
but the height may grow up to the screen on every frame, and a change of the measured size requests one more frame so anchored sheets settle immediately.
Regression test: `tests::area_grows_back_after_it_was_small`.
Additionally the Flow palette now has a **fixed-height list** (the dialog keeps its shape while you type), resets the scroll position when the query changes,
shows a "no match" hint, supports category chips, Enter = first match, Esc = close.

### Dropdowns instead of typing
| Parameter kind | Used by | What you get |
|---|---|---|
| `PK::Db` | Query database, Add row / task, Update row / task | Dropdown of the databases of your workspace; a warning if the stored name no longer exists. New nodes and templates are pre-filled with a real database (prefers one called *Tasks*). |
| `PK::Prop` | Where property, Find by property | Dropdown of that database's properties with their type. |
| `PK::Fields` | Add row / task, Update row / task | `Property = value` rows: property dropdown, value editor that follows the type — select options as dropdown, checkbox true/false, date with a *Today* button, number, text; `＋ Add field`, `✕` remove. |
| `PK::Secret` | HTTP token, message target, inbox key | Masked text with Show / Hide. |

### New nodes
| Node | Category | Purpose |
|---|---|---|
| **Ready-made API** (`api`) | Network | GitHub repo / issues / releases, weather (Open-Meteo), Hacker News, Wikipedia, exchange rates, CoinGecko, jokes, JSONPlaceholder todos — pick from a list. |
| **HTTP request** (extended) | Network | + query builder (auto-encoded), authentication (bearer, basic, x-api-key, `api_key` query), retries (network errors, 5xx, 429), proxy on by default on the web. |
| **RSS / Atom feed** (`rss`) | Network | Feed → `[{title, link, date, summary}]`. |
| **Send message** (`message`) | Network | ntfy.sh, Slack, Discord, Teams, Telegram, generic JSON webhook. |
| **Cloud inbox (webhook in)** (`cloud_in`) | Inputs | Reads events that anything POSTed to `/api/hooks/<channel>` on the server. `events` is inactive when empty, so scheduled flows simply stop. |
| **Cloud inbox (send)** (`cloud_out`) | Network | Writes an event into a channel (flow → flow, device → device). |
| **CSV → rows** (`csv`) | Data | Quoted fields, `,` `;` tab, header row, numbers detected. |

### Templates (Flow ⋯ menu → Templates)
Open tasks digest · Public API → tasks · GitHub issues → tasks · Webhook inbox → tasks · News digest → phone push · Weather → push message · Price alert (If / Merge) ·
CSV → database — plus the four from 10.0.

### Server side (Next.js, `src/app/api`)
* `POST /api/flow-proxy` — CORS relay for the web build. Public hosts only (DNS resolved and checked, redirects re-checked), 4 MB / 20 s limits, per-IP rate limit.
* `POST|GET|DELETE /api/hooks/<channel>` — PostgreSQL-backed inbox (500 events per channel, 256 KB per event, optional channel key via `x-hook-key` / `?key=`).
* `GET /api/downloads` — the published APK / Windows files with sizes and SHA-256.

On Android and Windows the nodes call the server directly: fill in **Server** (e.g. `https://your-site.example`) in the Cloud inbox nodes; HTTP / API nodes need no proxy there.

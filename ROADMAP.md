# Lumen — release 11 notes & roadmap

This file lives next to the code and on the website (`/roadmap`). It is **not** part of the app UI.

## Shipped in 11.0 — "Agents"

| Area | What you get |
|---|---|
| **Lumen AI** | A built-in agent panel (side panel on desktop/tablet, full-screen sheet on phones). It builds pages, databases (with relations, rows, views), dashboards (30 widget kinds), canvases, flows, automations, reminders and LumenScripts, verifies its work and can undo a whole run. |
| **Providers** | Google Gemini, OpenAI, Anthropic Claude, OpenRouter, Groq, Mistral, DeepSeek, xAI Grok, Together, Fireworks, Cerebras, Ollama, LM Studio and any OpenAI-compatible server. Bring your own key; keys stay on the device (browser storage / app data). |
| **Access control** | Read only · Create & edit · Full access, plus "ask before every change". Destructive tools (delete, replace, run script/flow) need Full access. |
| **MCP server** | One protocol core, three transports: cloud bridge (`/api/mcp`, works for web & Android apps), local HTTP (`127.0.0.1`), and the headless `lumen-mcp` stdio binary for backup files. Tools, resources (`lumen://page/<id>`, `lumen://knowledge/<module>`) and prompts. |
| **One tool registry** | `src/agent_tools.rs` — 38 tools shared by the in-app agent and every external agent. New features register a tool + a knowledge module and both get it. See *AI integration guide*. |
| **Knowledge base** | 10 knowledge modules (`rust/ai-knowledge`) that the agent reads on demand, including architecture blueprints for CRMs, trackers, OKRs, wikis. |
| **Android** | Release profile `android` (thin LTO, 4 codegen units, stripped), 16 KB page alignment, cleartext allowed for LAN model servers (Ollama), phone-first AI sheet that respects the keyboard and Back gesture. Signed arm64 APK built headless (no Android Studio). |
| **Web** | WebAssembly build served at `/app` with a PWA manifest + service worker; `/api/ai-proxy` (allow-listed AI providers), `/api/flow-proxy` (SSRF-guarded), `/api/hooks/<channel>` cloud inbox. |
| **Windows** | Cross-compiled x64 `.exe` (+ zip, SHA-256 sums). |

## Next round — recommended upgrades (website only, never inside the app)

**1. Streaming answers & cancellation of in-flight tool plans.** Today a model answer arrives as one piece and is revealed with a typewriter effect. Server-sent events for OpenAI/Anthropic/Gemini would show text and tool plans as they are produced. Needs a streaming `ehttp` path (native) and `fetch` streams (web).

**2. Server-held workspace (optional sync).** The bridge relays calls to a live app, so agents cannot act while the app is closed. A per-user encrypted snapshot + CRDT sync on the site would let agents work offline-first and merge when the app returns; it also gives multi-device sync and real-time collaboration.

**3. OAuth for MCP.** Today agents use a bearer token. Claude/ChatGPT "custom connectors" prefer OAuth 2.1 (dynamic client registration). Add `/.well-known/oauth-*`, consent screen, scoped tokens (read / edit / full) and revocation list.

**4. Embeddings & workspace search for the agent.** A local vector index (small ONNX/WebGPU model or provider embeddings) behind a `semantic_search` tool, so agents find the right page in large workspaces without reading the tree.

**5. Agent memory & scheduled agents.** Per-workspace memory pages the agent maintains, plus "run this prompt every Monday 9:00" (needs the server scheduler of item 2) — weekly reviews, inbox triage, digest flows.

**6. Visual agent plans.** Show the agent's plan as a checklist block before it executes (editable by the user), with per-step approval.

**7. Better flow runtime for agents.** Streaming flow runs, step-through debugging tools (`flow_step`, `flow_inspect`) and AI nodes (Prompt, Classify, Extract JSON, Embed + search) that use the same provider keys through the vault.

**8. Rich blocks for agents.** Tables-as-blocks, columns, linked database views, buttons and inline widgets are in the app but not yet creatable through `append_blocks` (the markdown dialect has no syntax for them). Add `db_view`, `columns`, `button`, `widget` block specs.

**9. Native platform polish.** Android: foreground-service for the local MCP server and reminders, share-target ("Share to Lumen" → AI summarises), home-screen widget, split-screen/foldable layouts, Play-Store AAB with Play App Signing. Windows: MSIX installer with auto-update, system tray quick-capture, global hotkey for Lumen AI. iOS: WASM PWA today; a Rust/Metal build needs a Mac CI runner.

**10. Test & safety harness.** Golden-file tests that replay recorded model conversations against the tool layer (regression for tool schemas), a prompt-injection test-suite for page content, rate limits and an audit log page for MCP calls.

**11. Fonts for CJK on the web.** Ship the lazily fetched Simplified-Chinese / Japanese / Korean subsets (`web/fonts`) with the deployment (they are produced by `tools/build-fonts.py` and not committed).

## Known limits of 11.0

* The cloud bridge needs the app open (and unlocked) — by design, the workspace lives on your device.
* Browsers can only reach local model servers (Ollama / LM Studio) when CORS is enabled on them; native apps have no such limit.
* The AI answers once per request (no token streaming yet) and is bounded to 24 tool steps per message (configurable up to 48).
* `run_flow` from an agent runs offline nodes immediately; flows with network nodes must be run from the Flow page.

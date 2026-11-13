# Connect AI agents to Lumen (MCP)

Lumen speaks the **Model Context Protocol** (revisions 2024-11-05, 2025-03-26, 2025-06-18, Streamable HTTP with JSON responses).
Any MCP client — Claude Desktop, Claude Code, Cursor, Windsurf, VS Code, ChatGPT/OpenAI connectors, your own scripts — gets the same
tools as the built-in Lumen AI: create and edit pages, databases, dashboards, canvases, flows, automations and scripts.

## 1. Cloud bridge (recommended — works from anywhere)

1. Open Lumen (web app at `/app`, Android, or Windows) → **✨ Ask Lumen AI → ⚙ → Connect agents (MCP)**.
2. Choose what agents may do: *Read only*, *Create & edit* or *Full access*.
3. Switch on **Connect to the Lumen site**. (Android / Windows: enter this site's URL first.) The status dot turns green.
4. Copy the **token** and the **endpoint** `https://YOUR-SITE/api/mcp`.

The app keeps a long-poll open to the site; the site relays agent calls to the live app and returns the answers. The workspace never leaves your
device except for what a tool returns. Closed app ⇒ agents can still *connect* and list tools (the manifest is cached) but tool calls answer
"the app is offline".

### Client configuration

**Claude Code**
```bash
claude mcp add --transport http lumen https://YOUR-SITE/api/mcp --header "Authorization: Bearer lmn_…"
```

**Cursor / Windsurf / VS Code / Claude Desktop (remote servers)**
```json
{
  "mcpServers": {
    "lumen": {
      "url": "https://YOUR-SITE/api/mcp",
      "headers": { "Authorization": "Bearer lmn_…" }
    }
  }
}
```

**stdio-only clients** (older Claude Desktop):
```json
{ "mcpServers": { "lumen": { "command": "npx", "args": ["mcp-remote", "https://YOUR-SITE/api/mcp", "--header", "Authorization: Bearer lmn_…"] } } }
```

**OpenAI Responses API**
```json
{ "tools": [{ "type": "mcp", "server_label": "lumen", "server_url": "https://YOUR-SITE/api/mcp", "headers": { "Authorization": "Bearer lmn_…" }, "require_approval": "never" }] }
```

## 2. Local server (desktop / Android)

AI settings → Connect → **Run a local MCP server**: `http://127.0.0.1:7878/mcp` with the same bearer token. No internet involved.

## 3. Headless stdio server (`lumen-mcp`)

For a workspace *file* (Lumen → ⋯ → Back up workspace):
```bash
lumen-mcp --workspace backup.json --access edit      # read | edit | full ; --new creates the file
```
```json
{ "mcpServers": { "lumen": { "command": "lumen-mcp", "args": ["--workspace", "/path/backup.json"] } } }
```
Changes are written back atomically after every modifying call. Re-import the file in Lumen (Import → Restore backup) to see them.

## What agents get

* **Tools (38)** — `workspace_overview`, `read_page`, `search`, `create_page`, `append_markdown`, `create_database`, `add_rows`, `query_database`, `add_view`, `create_dashboard`, `add_widgets`, `create_canvas`, `create_flow`, `run_flow`, `create_rule`, `create_reminder`, `script_test`, `script_run`, `get_knowledge`, … The list is generated — `tools/list` is always current. Tools are filtered by the access level.
* **Resources** — `lumen://knowledge/<module>` (the guides the built-in agent reads) and `lumen://page/<id>` (JSON of any page).
* **Prompts** — `build_system`, `design_dashboard`, `weekly_review`, `clean_up`, `automate`.
* **Annotations** — every tool carries `readOnlyHint` / `destructiveHint`.

A good first message for an agent: *"Call workspace_overview, read the `overview` and `architect` knowledge modules, then build me a CRM."*

## Security model

* The bearer token is the only credential; the site stores only its SHA-256. **New token** in the app cuts off every old connection.
* Access levels are enforced inside the app, not on the site: a *Read only* token cannot create anything even if the site is compromised.
* Destructive tools (delete block/rule/widget, replace content, trash, run script/flow) need *Full access*.
* A locked workspace answers every call with "locked".
* The AI provider proxy (`/api/ai-proxy`) relays only to an allow-list of AI hosts and never stores keys or bodies; `/api/flow-proxy` blocks private / loopback addresses.

## Troubleshooting

| Symptom | Fix |
|---|---|
| 401 Unknown token | Switch the bridge on in the app first — the token registers on its first poll. |
| "The Lumen app is offline" | Open the app, unlock it, keep the tab/app in the foreground (mobile OSes pause background tabs). |
| Tools missing | Access level is *Read only*, or the app is older than 11.0. |
| Timeout on long jobs | Big builds are split into tool calls of seconds each; ask the agent to proceed step by step. |


## Loopback relay for the WebAssembly app (v12)

```
lumen-mcp --relay 7979 --token lmn_<token from AI → Connect> --origin https://your-site
```
* Listens on **127.0.0.1 only**. Every request needs `Authorization: Bearer <token>` (constant-time compare).
* A request with an `Origin` header must come from `--origin` or a loopback page; the `Host` header must be a loopback name (DNS-rebinding guard).
* Answers CORS and Chrome's *Private Network Access* preflight (`Access-Control-Allow-Private-Network`) for that single origin.
* Lumen (web, bridge on, bridge URL `http://127.0.0.1:7979`) long-polls `/api/bridge/poll`; an MCP client posts JSON-RPC to `http://127.0.0.1:7979/mcp`.
* The same protocol is served by the website (`/api/mcp`, `/api/bridge/poll`) for remote use.

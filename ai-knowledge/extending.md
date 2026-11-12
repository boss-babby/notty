# Extending the AI: how a new Lumen feature teaches the agent

The agent and the MCP server read from **one registry** (`src/agent_tools.rs`) — add a feature there once and both the in-app
assistant and every external agent (Claude, Cursor, ChatGPT connectors, scripts …) get it.

## Checklist for every new feature
1. **Model** — the feature stores its data in `Workspace` (serde, `#[serde(default)]`).
2. **Tool(s)** — register a tool at start-up:
   ```rust
   agent_tools::register_tool(
       ToolDef::new("create_timeline", "Create a …", Risk::Write, json!({ "type":"object", "properties": {…}, "required":[…] })),
       |ws, args| { …; Ok(json!({ "id": id })) });
   ```
   Rules: `Risk::Read` never mutates; `Risk::Write` adds/edits; `Risk::Destroy` deletes/overwrites/runs user code. Accept **names or ids**
   (helpers `resolve_page`, `resolve_prop`); return small JSON with ids so the agent can chain calls; return `Err("clear, actionable message")`.
3. **Knowledge** — drop `ai-knowledge/<module>.md` and register it:
   `agent_tools::register_knowledge("module", "Title", "one-line summary", include_str!("…"))`. Keep it: concepts → tool list → JSON examples → patterns → pitfalls.
   Dynamic parts (node catalogues, widget lists) are generated from the registries, never hand-copied.
4. **Read path** — extend `describe_page` (used by `read_page` / `workspace_overview`) so the agent can *see* the new object.
5. **Tests** — add a test in `src/tests.rs` that runs the tool on `Workspace::seed()` and checks `tool_defs()` contains it.
6. **Docs** — add the feature to `ROADMAP.md`'s "shipped" list and to the website docs.

## What the MCP server exposes automatically
* `tools/list` ← registry · `resources` ← pages (`lumen://page/<id>`) and knowledge (`lumen://knowledge/<module>`) · `prompts` ← `PROMPTS`.
* Because `tools/list` is generated, nothing in `mcp.rs` changes when you add a tool. The cloud bridge re-publishes the manifest on connect.

## Prompt registry
`agent_tools::PROMPTS` holds ready-made agent prompts (`build_system`, `design_dashboard`, `weekly_review`, …). Add yours for the feature.

## Versioning
Bump `AGENT_API_VERSION` when a tool's arguments change incompatibly; additive changes (new optional args) need no bump.

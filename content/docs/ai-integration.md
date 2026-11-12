# AI integration guide — teaching the agent new features

Lumen has **one** place where "what the AI can do" is defined: the tool registry in `rust/src/agent_tools.rs`
(plus `agent_build.rs` for dashboards, canvases, flows, automations and scripts). Both the in-app assistant (`ai.rs`/`aiui.rs`) and the
MCP server (`mcp.rs`) read from it. When you ship a feature, you ship its AI surface in the same pull request.

## Architecture in one picture

```
 in-app chat (aiui.rs) ──► ai.rs  agent loop ──┐
                                               ├──► agent_tools::call(ws, name, args, access) ──► Workspace
 external agent ──► /api/mcp ──► cloud bridge ─┤
 local HTTP / lumen-mcp (stdio) ──► mcp.rs ────┘
```

* `agent_tools.rs` – registry, access control (`Risk::Read/Write/Destroy` × `Access::ReadOnly/Edit/Full`), helpers (name→id resolution, markdown → blocks, value conversion), knowledge registry, prompt registry, page/database tools.
* `agent_build.rs` – dashboard / canvas / flow / automation / script tools.
* `ai.rs` – providers (OpenAI-compatible, Anthropic, Gemini), request builders, reply parsers, the loop (plan → tool calls → results → … ), approval, undo snapshot, chat threads.
* `aiui.rs` – chat panel, tool cards, markdown renderer, settings (providers, agent, MCP connections).
* `mcp.rs` – JSON-RPC core, local HTTP server, cloud bridge client, stdio runner.
* `ai-knowledge/*.md` – what the model reads on demand (`get_knowledge`). Also served as MCP resources.

## Checklist: add a feature to the AI

1. **Store data in `Workspace`** with `#[serde(default)]` so old backups keep loading.
2. **Register tools** — in `agent_build.rs::tools()` (or from your module at start-up with `register_tool`):
   ```rust
   (ToolDef::new("create_timeline", "One clear sentence the model can decide on.", Risk::Write,
        obj(json!({"title": s("…"), "items": arr(obj(json!({"name": s("…"), "start": s("YYYY-MM-DD")}), &["name"]), "…")}), &["title"])),
    tool_create_timeline),
   ```
   * `Risk::Read` – never mutates; `Write` – creates/edits; `Destroy` – deletes, overwrites, runs user code, performs network side effects.
   * Accept **names or ids** (`resolve_page`, `resolve_db`, `resolve_prop`). Validate everything *before* creating anything; on error leave no half-built objects.
   * Return small JSON with ids (`{"id": 12}`); tools named `create_*` that return `id` make the app navigate / offer an "Open" button.
   * Error strings are for the model: say what was wrong **and how to fix it** ("Unknown property “Due”. Properties: Name, Status").
3. **Write knowledge** — `rust/ai-knowledge/<module>.md` and add it to `agent_tools::knowledge()` (or `register_knowledge` at runtime). Structure: concepts → tool list with argument shapes → one JSON example → patterns → pitfalls. Generate catalogues (node types, widget kinds) from the code registries instead of copying them.
4. **Make it readable** — extend `describe_page` so `read_page` shows the new object (agents verify by reading).
5. **Humanise** — add a case to `ai::summarize_call` so the chat card says "Created timeline “Roadmap”" instead of the tool name.
6. **Prompts** — add one-click prompts to `agent_tools::PROMPTS` (they appear in MCP `prompts/list`).
7. **Test** — add to `src/agent_tests.rs`: run the tool on `Workspace::seed()`, assert the model changed, assert a bad call changes nothing, and that `tool_defs()` schemas stay valid (a test already walks every schema, including a Gemini-safety check).
8. **Docs** — mention it in `ROADMAP.md` ("shipped") and the website docs.

## Adding an AI provider

* OpenAI-compatible server → one line in `ai::PROVIDERS` (`id`, `name`, `Api::OpenAi`, base URL, suggested models, key page). Add its host to `HOSTS` in `src/app/api/ai-proxy/route.ts` if it must work from the browser.
* New wire format → add an `Api` variant, a branch in `build()` (messages + tools → request) and in `parse_reply()` (response → text + tool calls), and keep the provider's raw assistant turn in `Msg::raw` if it needs it replayed (Gemini thought signatures, Anthropic thinking blocks).
* Tool schemas are sanitised for the strictest provider (`clean_schema`): no type arrays, no empty objects; free-form objects travel as JSON strings and are decoded by `normalize_args` in `agent_tools::call`.

## Design rules that keep agents reliable

* **Few big tools beat many tiny ones.** `create_database` takes columns *and* rows; `create_dashboard` takes all widgets.
* **Idempotent reads, atomic writes.** Validate first; never leave partial objects.
* **Names over ids** where humans would use names; ids in every result so the next call can use them.
* **Every destructive path is Full-access only** and every agent run keeps a snapshot, so the user can undo a whole run.
* **Tool output is untrusted data** (a page can say "ignore your instructions"). The system prompt says so; keep results free of anything that looks like instructions.
* **Bump `AGENT_API_VERSION`** only for incompatible argument changes.

## Testing a connection by hand

```bash
curl -s https://YOUR-SITE/api/mcp -H "Authorization: Bearer lmn_…" -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"workspace_overview","arguments":{}}}'
```

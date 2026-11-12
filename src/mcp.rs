//! Model Context Protocol (MCP) server for Lumen.
//!
//! One protocol core ([`handle`]) and three ways to reach it:
//! 1. **Local HTTP** (desktop / Android): `http://127.0.0.1:<port>/mcp`, bearer token — for agents on the same machine.
//! 2. **Cloud bridge** (web, Android, desktop): the app long-polls `/api/bridge/poll` of the Lumen site; external agents talk to
//!    `https://<site>/api/mcp` with the same token and the site relays the calls to the live app. Works from anywhere.
//! 3. **stdio** (`lumen-mcp --workspace backup.json`): headless, for Claude Desktop / Cursor style launchers.
//!
//! All transports funnel into the UI thread (the workspace has one owner), so an agent edit appears on screen immediately.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};

use crate::agent_tools::{self as tools, Access, Risk};
use crate::auth::now_ms;
use crate::model::Workspace;

pub const PROTOCOL: &str = "2025-06-18";
const SUPPORTED: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct McpSettings {
    /// Local HTTP server (desktop & Android).
    #[serde(default)]
    pub local_on: bool,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Cloud bridge through the Lumen site.
    #[serde(default)]
    pub bridge_on: bool,
    /// Site URL (empty on the web build = this site).
    #[serde(default)]
    pub bridge_url: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub access: Access,
}

fn default_port() -> u16 {
    7878
}

impl Default for McpSettings {
    fn default() -> Self {
        McpSettings { local_on: false, port: 7878, bridge_on: false, bridge_url: String::new(), token: String::new(), access: Access::Edit }
    }
}

pub fn new_token() -> String {
    format!("lmn_{}", crate::auth::random_hex(20))
}

// ───────────────────────────── protocol core ─────────────────────────────

fn instructions() -> String {
    "Lumen is a Notion-style workspace (pages, databases, dashboards, canvases, flows, automations, scripts). \
     Start with `workspace_overview`, read the `overview` module with `get_knowledge`, read before you write, prefer few large calls, \
     and verify what you built with `read_page`."
        .into()
}

fn tool_json(d: &tools::ToolDef) -> Json {
    json!({
        "name": d.name,
        "title": d.name.replace('_', " "),
        "description": d.description,
        "inputSchema": d.schema,
        "annotations": {"readOnlyHint": d.risk == Risk::Read, "destructiveHint": d.risk == Risk::Destroy, "idempotentHint": false, "openWorldHint": false},
    })
}

/// The public face of the server (what `initialize`, `tools/list`, `prompts/list` return) — the cloud bridge caches it so agents can
/// connect even while the app is closed.
pub fn manifest(access: Access) -> Json {
    json!({
        "protocolVersion": PROTOCOL,
        "serverInfo": {"name": "lumen", "title": "Lumen workspace", "version": env!("CARGO_PKG_VERSION")},
        "instructions": instructions(),
        "access": access.key(),
        "tools": tools::tool_defs().iter().filter(|d| access.allows(d.risk)).map(tool_json).collect::<Vec<_>>(),
        "prompts": prompts_json(),
    })
}

fn prompts_json() -> Json {
    json!(tools::PROMPTS.iter().map(|(id, title, text)| json!({"name": id, "title": title, "description": title, "arguments": if text.contains("{goal}") { json!([{"name": "goal", "description": "What it is for", "required": false}]) } else { json!([]) }})).collect::<Vec<_>>())
}

fn ok(id: &Json, result: Json) -> Json {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn err(id: &Json, code: i64, msg: impl Into<String>) -> Json {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg.into()}})
}

fn page_uri(id: u64) -> String {
    format!("lumen://page/{id}")
}

/// Handles one JSON-RPC message. `None` = notification (no answer).
pub fn handle_one(ws: &mut Workspace, req: &Json, access: Access, locked: bool) -> Option<Json> {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));
    let Some(id) = id else { return None };
    if method.is_empty() {
        return Some(err(&id, -32600, "Invalid request"));
    }
    Some(match method {
        "initialize" => {
            let want = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOL);
            let v = if SUPPORTED.contains(&want) { want } else { PROTOCOL };
            let mut m = manifest(access);
            m["protocolVersion"] = json!(v);
            ok(&id, json!({
                "protocolVersion": v,
                "capabilities": {"tools": {"listChanged": false}, "resources": {"subscribe": false, "listChanged": false}, "prompts": {"listChanged": false}},
                "serverInfo": m["serverInfo"], "instructions": m["instructions"],
            }))
        }
        "ping" | "logging/setLevel" => ok(&id, json!({})),
        "tools/list" => ok(&id, json!({"tools": manifest(access)["tools"]})),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            if name.is_empty() {
                return Some(err(&id, -32602, "tools/call needs a tool name"));
            }
            if locked {
                return Some(ok(&id, json!({"content": [{"type": "text", "text": "The Lumen workspace is locked. Ask the user to unlock the app."}], "isError": true})));
            }
            let o = tools::call(ws, name, &args, access);
            ok(&id, json!({"content": [{"type": "text", "text": o.text}], "isError": !o.ok}))
        }
        "resources/list" => {
            let mut v: Vec<Json> = tools::knowledge().iter().map(|k| json!({"uri": format!("lumen://knowledge/{}", k.id), "name": k.id, "title": k.title, "description": k.summary, "mimeType": "text/markdown"})).collect();
            for p in ws.pages.iter().filter(|p| !p.trashed && !p.row).take(300) {
                v.push(json!({"uri": page_uri(p.id), "name": format!("page-{}", p.id), "title": if p.title.is_empty() { "Untitled".to_owned() } else { p.title.clone() }, "mimeType": "application/json"}));
            }
            ok(&id, json!({"resources": if locked { vec![] } else { v }}))
        }
        "resources/templates/list" => ok(&id, json!({"resourceTemplates": [
            {"uriTemplate": "lumen://page/{id}", "name": "page", "title": "Lumen page", "description": "Any page, database row, canvas or flow by id", "mimeType": "application/json"},
            {"uriTemplate": "lumen://knowledge/{module}", "name": "knowledge", "title": "Lumen knowledge module", "mimeType": "text/markdown"}]})),
        "resources/read" => {
            let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");
            if let Some(m) = uri.strip_prefix("lumen://knowledge/") {
                match tools::knowledge().into_iter().find(|k| k.id == m) {
                    Some(k) => ok(&id, json!({"contents": [{"uri": uri, "mimeType": "text/markdown", "text": k.body}]})),
                    None => err(&id, -32002, format!("No knowledge module {m}")),
                }
            } else if let Some(pid) = uri.strip_prefix("lumen://page/").and_then(|p| p.parse::<u64>().ok()) {
                if locked {
                    return Some(err(&id, -32002, "Workspace is locked"));
                }
                match tools::describe_page(ws, pid) {
                    Ok(j) => ok(&id, json!({"contents": [{"uri": uri, "mimeType": "application/json", "text": j.to_string()}]})),
                    Err(e) => err(&id, -32002, e),
                }
            } else {
                err(&id, -32002, format!("Unknown resource {uri}"))
            }
        }
        "prompts/list" => ok(&id, json!({"prompts": prompts_json()})),
        "prompts/get" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            match tools::PROMPTS.iter().find(|(n, _, _)| *n == name) {
                Some((_, title, text)) => {
                    let goal = params.get("arguments").and_then(|a| a.get("goal")).and_then(|g| g.as_str()).unwrap_or("my current needs");
                    ok(&id, json!({"description": title, "messages": [{"role": "user", "content": {"type": "text", "text": text.replace("{goal}", goal)}}]}))
                }
                None => err(&id, -32602, format!("Unknown prompt {name}")),
            }
        }
        _ => err(&id, -32601, format!("Method not found: {method}")),
    })
}

/// Handles a raw request body (a message or a batch). Returns the body to send back, `None` when nothing is to be sent (notifications).
pub fn handle(ws: &mut Workspace, body: &str, access: Access, locked: bool) -> Option<String> {
    let v: Json = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Some(err(&Json::Null, -32700, "Parse error").to_string()),
    };
    match v {
        Json::Array(list) => {
            let out: Vec<Json> = list.iter().filter_map(|r| handle_one(ws, r, access, locked)).collect();
            if out.is_empty() {
                None
            } else {
                Some(Json::Array(out).to_string())
            }
        }
        r => handle_one(ws, &r, access, locked).map(|j| j.to_string()),
    }
}

/// One-line description of a request for the activity log, and whether it succeeded.
fn summarize(body: &str, resp: &Option<String>) -> Option<(String, bool)> {
    let v: Json = serde_json::from_str(body).ok()?;
    let method = v.get("method")?.as_str()?;
    if matches!(method, "ping" | "initialize" | "tools/list" | "resources/list" | "prompts/list" | "resources/templates/list") || method.starts_with("notifications/") {
        return None;
    }
    let label = if method == "tools/call" { v["params"]["name"].as_str().unwrap_or("tool").to_owned() } else { method.to_owned() };
    let failed = resp.as_ref().and_then(|r| serde_json::from_str::<Json>(r).ok()).map_or(false, |r| r.get("error").is_some() || r["result"]["isError"].as_bool() == Some(true));
    Some((label, !failed))
}

// ───────────────────────────── runtime: transports ─────────────────────────────

struct Job {
    body: String,
    reply: std::sync::mpsc::Sender<Option<String>>,
}

type Queue = Arc<Mutex<VecDeque<Job>>>;

#[cfg(not(target_arch = "wasm32"))]
struct Local {
    stop: Arc<std::sync::atomic::AtomicBool>,
    port: u16,
    token: String,
    status: Arc<Mutex<String>>,
}

type Slot = Arc<Mutex<Option<Result<ehttp::Response, String>>>>;

#[derive(Default)]
struct Bridge {
    slot: Option<Slot>,
    started: f64,
    next_try: f64,
    results: Vec<Json>,
    sent_manifest: u64,
    connected: bool,
    status: String,
}

#[derive(Default)]
pub struct McpRuntime {
    queue: Queue,
    #[cfg(not(target_arch = "wasm32"))]
    local: Option<Local>,
    bridge: Bridge,
    /// (time ms, what, success) — newest last.
    pub activity: VecDeque<(f64, String, bool)>,
    pub served: u32,
}

impl McpRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn local_status(&self) -> String {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(l) = &self.local {
            return l.status.lock().map(|s| s.clone()).unwrap_or_default();
        }
        #[cfg(target_arch = "wasm32")]
        return "Not available in the browser — use the cloud bridge".into();
        #[cfg(not(target_arch = "wasm32"))]
        "Off".into()
    }

    pub fn bridge_status(&self) -> (bool, String) {
        (self.bridge.connected, if self.bridge.status.is_empty() { "Off".to_owned() } else { self.bridge.status.clone() })
    }

    fn log(&mut self, what: String, ok: bool) {
        self.activity.push_back((now_ms(), what, ok));
        while self.activity.len() > 40 {
            self.activity.pop_front();
        }
        self.served += 1;
    }

    /// Call once per frame. Returns true when the workspace was modified.
    pub fn tick(&mut self, ws: &mut Workspace, set: &McpSettings, locked: bool, ctx: &egui::Context) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        self.sync_local(set, ctx);
        let mut changed = false;
        // jobs from local connections
        for _ in 0..16 {
            let Some(job) = self.queue.lock().ok().and_then(|mut q| q.pop_front()) else { break };
            let (c, resp) = self.run(ws, &job.body, set.access, locked);
            changed |= c;
            let _ = job.reply.send(resp);
        }
        if set.bridge_on {
            changed |= self.poll_bridge(ws, set, locked, ctx);
        } else if self.bridge.slot.is_some() || self.bridge.connected {
            self.bridge = Bridge::default();
        }
        changed
    }

    /// True when something wants frequent frames (open sockets / long polls).
    pub fn wants_fast_repaint(&self, set: &McpSettings) -> bool {
        set.local_on || set.bridge_on
    }

    fn run(&mut self, ws: &mut Workspace, body: &str, access: Access, locked: bool) -> (bool, Option<String>) {
        let before = ws_stamp(ws);
        let resp = handle(ws, body, access, locked);
        if let Some((what, okk)) = summarize(body, &resp) {
            self.log(what, okk);
        }
        (ws_stamp(ws) != before, resp)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn sync_local(&mut self, set: &McpSettings, ctx: &egui::Context) {
        use std::sync::atomic::Ordering;
        let want = set.local_on;
        if let Some(l) = &self.local {
            if !want || l.port != set.port || l.token != set.token {
                l.stop.store(true, Ordering::SeqCst);
                self.local = None;
            }
        }
        if want && self.local.is_none() {
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let status = Arc::new(Mutex::new(String::from("Starting …")));
            serve_local(set.port, set.token.clone(), self.queue.clone(), stop.clone(), status.clone(), ctx.clone());
            self.local = Some(Local { stop, port: set.port, token: set.token.clone(), status });
        }
    }

    fn poll_bridge(&mut self, ws: &mut Workspace, set: &McpSettings, locked: bool, ctx: &egui::Context) -> bool {
        let now = now_ms();
        let mut changed = false;
        if let Some(slot) = self.bridge.slot.clone() {
            let done = slot.lock().ok().and_then(|mut g| g.take());
            match done {
                None => {
                    if now - self.bridge.started > 50_000.0 {
                        self.bridge.slot = None;
                        self.bridge.next_try = now + 3000.0;
                    } else {
                        return false;
                    }
                }
                Some(Ok(res)) => {
                    self.bridge.slot = None;
                    if res.status == 200 {
                        self.bridge.connected = true;
                        self.bridge.status = "Connected — agents can use this workspace".into();
                        let v: Json = serde_json::from_slice(&res.bytes).unwrap_or(Json::Null);
                        if let Some(reqs) = v.get("requests").and_then(|r| r.as_array()) {
                            for r in reqs {
                                let (Some(id), Some(payload)) = (r.get("id").cloned(), r.get("payload").and_then(|p| p.as_str())) else { continue };
                                let (c, resp) = self.run(ws, payload, set.access, locked);
                                changed |= c;
                                self.bridge.results.push(json!({"id": id, "response": resp}));
                            }
                        }
                        self.bridge.next_try = now;
                    } else {
                        self.bridge.connected = false;
                        self.bridge.status = match res.status {
                            401 | 403 => "The site rejected the token — generate a new one".to_owned(),
                            404 => "That server has no Lumen bridge (/api/bridge/poll)".to_owned(),
                            s => format!("Server answered HTTP {s}"),
                        };
                        self.bridge.next_try = now + 12_000.0;
                    }
                }
                Some(Err(e)) => {
                    self.bridge.slot = None;
                    self.bridge.connected = false;
                    self.bridge.status = format!("Offline: {}", e.chars().take(80).collect::<String>());
                    self.bridge.next_try = now + 8000.0;
                }
            }
        }
        if self.bridge.slot.is_none() && now >= self.bridge.next_try {
            let base = set.bridge_url.trim().trim_end_matches('/').to_owned();
            if base.is_empty() && !cfg!(target_arch = "wasm32") {
                self.bridge.status = "Enter your Lumen site URL".into();
                self.bridge.next_try = now + 2000.0;
                return changed;
            }
            if set.token.is_empty() {
                self.bridge.status = "Generate a token first".into();
                self.bridge.next_try = now + 2000.0;
                return changed;
            }
            let m = manifest(set.access);
            let mhash = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                m.to_string().hash(&mut h);
                h.finish()
            };
            let send_manifest = mhash != self.bridge.sent_manifest;
            let body = json!({"manifest": if send_manifest { m } else { Json::Null }, "results": std::mem::take(&mut self.bridge.results), "client": format!("lumen/{}", env!("CARGO_PKG_VERSION")), "locked": locked});
            let mut req = ehttp::Request::post(format!("{base}/api/bridge/poll"), body.to_string().into_bytes());
            req.headers.insert("Content-Type", "application/json");
            req.headers.insert("Authorization", format!("Bearer {}", set.token));
            if send_manifest {
                self.bridge.sent_manifest = mhash;
            }
            let slot: Slot = Arc::new(Mutex::new(None));
            let (s2, c2) = (slot.clone(), ctx.clone());
            ehttp::fetch(req, move |r| {
                if let Ok(mut g) = s2.lock() {
                    *g = Some(r);
                }
                c2.request_repaint();
            });
            self.bridge.slot = Some(slot);
            self.bridge.started = now;
            if self.bridge.status.is_empty() || self.bridge.status == "Off" {
                self.bridge.status = "Connecting …".into();
            }
        }
        changed
    }
}

/// A cheap fingerprint of "did anything change" (pages count + newest update + dashboards + automation sizes).
fn ws_stamp(ws: &Workspace) -> (usize, u64, usize, usize, usize, u64) {
    let newest = ws.pages.iter().map(|p| p.updated as u64).max().unwrap_or(0);
    (ws.pages.len(), newest, ws.dash.boards.iter().map(|b| b.widgets.len() + 1).sum(), ws.auto.rules.len() + ws.auto.reminders.len(), ws.auto.scripts.len(), ws.next_id)
}

#[cfg(not(target_arch = "wasm32"))]
fn serve_local(port: u16, token: String, queue: Queue, stop: Arc<std::sync::atomic::AtomicBool>, status: Arc<Mutex<String>>, ctx: egui::Context) {
    use std::sync::atomic::Ordering;
    std::thread::spawn(move || {
        let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => l,
            Err(e) => {
                if let Ok(mut s) = status.lock() {
                    *s = format!("Cannot listen on port {port}: {e}");
                }
                return;
            }
        };
        let _ = listener.set_nonblocking(true);
        if let Ok(mut s) = status.lock() {
            *s = format!("Listening on http://127.0.0.1:{port}/mcp");
        }
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let (q, t, c) = (queue.clone(), token.clone(), ctx.clone());
                    std::thread::spawn(move || {
                        let _ = serve_conn(stream, &t, q, c);
                    });
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(60)),
            }
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn serve_conn(mut s: std::net::TcpStream, token: &str, queue: Queue, ctx: egui::Context) -> std::io::Result<()> {
    use std::io::{Read, Write};
    let _ = s.set_nonblocking(false);
    s.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = s.read(&mut tmp)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        if buf.len() > 65_536 {
            return Ok(());
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let mut len = 0usize;
    let mut auth = String::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            match k.trim().to_lowercase().as_str() {
                "content-length" => len = v.trim().parse().unwrap_or(0),
                "authorization" => auth = v.trim().to_owned(),
                "x-lumen-token" => auth = format!("Bearer {}", v.trim()),
                _ => {}
            }
        }
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len.min(8_000_000) {
        let n = s.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    let reply = |s: &mut std::net::TcpStream, code: &str, ctype: &str, body: &str| -> std::io::Result<()> {
        write!(s, "HTTP/1.1 {code}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: authorization, content-type, mcp-protocol-version, mcp-session-id, x-lumen-token\r\nAccess-Control-Allow-Methods: POST, GET, OPTIONS\r\nConnection: close\r\n\r\n{body}", body.len())?;
        s.flush()
    };
    if method == "OPTIONS" {
        return reply(&mut s, "204 No Content", "text/plain", "");
    }
    if method == "GET" && (path == "/health" || path == "/") {
        return reply(&mut s, "200 OK", "application/json", &json!({"ok": true, "server": "lumen-mcp", "version": env!("CARGO_PKG_VERSION")}).to_string());
    }
    if !token.is_empty() && auth != format!("Bearer {token}") {
        return reply(&mut s, "401 Unauthorized", "application/json", &err(&Json::Null, -32001, "Missing or wrong bearer token").to_string());
    }
    if method != "POST" {
        return reply(&mut s, "405 Method Not Allowed", "text/plain", "POST JSON-RPC to /mcp");
    }
    let (tx, rx) = std::sync::mpsc::channel();
    if let Ok(mut q) = queue.lock() {
        q.push_back(Job { body: String::from_utf8_lossy(&body).to_string(), reply: tx });
    }
    ctx.request_repaint();
    match rx.recv_timeout(std::time::Duration::from_secs(90)) {
        Ok(Some(resp)) => reply(&mut s, "200 OK", "application/json", &resp),
        Ok(None) => reply(&mut s, "202 Accepted", "text/plain", ""),
        Err(_) => reply(&mut s, "504 Gateway Timeout", "application/json", &err(&Json::Null, -32000, "Lumen did not answer in time (is the app open and unlocked?)").to_string()),
    }
}

// ───────────────────────────── stdio (headless) ─────────────────────────────

/// `lumen-mcp --workspace backup.json [--access read|edit|full] [--new]` — speaks MCP over stdin/stdout and keeps the file up to date.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_cli(args: Vec<String>) -> i32 {
    use std::io::{BufRead, Write};
    let mut path = String::from("lumen-workspace.json");
    let mut access = Access::Edit;
    let mut create = false;
    let mut it = args.into_iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--workspace" | "-w" => path = it.next().unwrap_or(path),
            "--access" => access = Access::from_key(&it.next().unwrap_or_default()),
            "--new" => create = true,
            "--help" | "-h" => {
                eprintln!("lumen-mcp — Model Context Protocol server for a Lumen workspace file\n\nUSAGE: lumen-mcp --workspace <backup.json> [--access read|edit|full] [--new]\n\nExport the file from Lumen (⋯ → Backup). Speaks MCP (JSON-RPC, one message per line) on stdin/stdout.");
                return 0;
            }
            _ => {}
        }
    }
    let mut ws: Workspace = match std::fs::read_to_string(&path) {
        Ok(t) => match serde_json::from_str::<Workspace>(&t) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("lumen-mcp: {path} is not a Lumen backup: {e}");
                return 2;
            }
        },
        Err(_) if create => Workspace::seed(),
        Err(e) => {
            eprintln!("lumen-mcp: cannot read {path}: {e} (use --new to create it)");
            return 2;
        }
    };
    ws.migrate();
    eprintln!("lumen-mcp {} — {} pages, access: {}", env!("CARGO_PKG_VERSION"), ws.pages.len(), access.label());
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let before = ws_stamp(&ws);
        if let Some(resp) = handle(&mut ws, &line, access, false) {
            let _ = writeln!(out, "{resp}");
            let _ = out.flush();
        }
        if ws_stamp(&ws) != before {
            if let Ok(text) = serde_json::to_string(&ws) {
                let tmp = format!("{path}.tmp");
                if std::fs::write(&tmp, text).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            }
        }
    }
    0
}

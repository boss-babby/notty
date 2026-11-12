//! Flow — a node-based data-flow editor / automation engine (the "logic canvas").
//!
//! This module is the **model + engine** half (no egui widgets, fully unit tested). `flowui.rs` draws and edits it.
//!
//! ## Design
//! * A [`Flow`] is a directed acyclic graph. Every node has **named input and output ports**; any number of wires can leave an output,
//!   and several wires can enter one input (fan-in → the values arrive as a list).
//! * Data are plain JSON values ([`Json`]). Ports are untyped, nodes convert what they receive and say so in their error message.
//! * **Branching without loops**: a node can leave an output *inactive* (`None`). A node whose connected inputs are all inactive is
//!   *skipped*, and so is everything that only depends on it — that is how `If`, `Switch`, `Filter` and HTTP's `error` port route data.
//! * **Iteration without sub-graphs**: list nodes (`Map`, `Filter`, `Sort`, `Aggregate`…) work on whole lists, so a flow stays flat and
//!   readable. Per-item logic is a one-line LumenScript expression (`item`, `index`, `with`).
//! * **Async without threads in the model**: the engine ([`Run`]) is a state machine polled once per frame. HTTP requests (`ehttp`, native
//!   threads / browser `fetch`) and timers are *pending* nodes; independent branches run concurrently.
//! * **Safe by default**: every run can be a *dry run* on a throw-away copy of the workspace; LumenScript limits (steps, rows, notices)
//!   apply to every script node; the graph is validated for cycles before a run.
//!
//! Extension point: add a [`Def`] to [`DEFS`] and a branch to `exec` — the editor, palette, inspector, validation and export pick it up.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map};
use sha2::{Digest, Sha256};

use crate::automation::{NoticeKind, Output};
use crate::model::Workspace;
use crate::scripthost::{self, Extra, ScriptCtx};

pub type Json = serde_json::Value;

pub const MAX_NODES: usize = 300;
const MAX_HTTP_BYTES: usize = 4_000_000;
const MAX_DELAY_MS: f64 = 60_000.0;

// ───────────────────────────── catalogue ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cat {
    Input,
    Logic,
    Data,
    Network,
    Workspace,
    Output,
}

impl Cat {
    pub const ALL: [Cat; 6] = [Cat::Input, Cat::Logic, Cat::Data, Cat::Network, Cat::Workspace, Cat::Output];
    pub fn label(self) -> &'static str {
        match self {
            Cat::Input => "Inputs",
            Cat::Logic => "Logic",
            Cat::Data => "Data",
            Cat::Network => "Network",
            Cat::Workspace => "Workspace",
            Cat::Output => "Outputs",
        }
    }
    /// Accent colour (r, g, b).
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            Cat::Input => (46, 160, 110),
            Cat::Logic => (224, 140, 40),
            Cat::Data => (66, 133, 244),
            Cat::Network => (156, 92, 220),
            Cat::Workspace => (214, 72, 120),
            Cat::Output => (96, 108, 128),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PK {
    Text,
    Long,
    Num,
    Bool,
    Code,
    Choice(&'static [&'static str]),
    /// Dropdown of the databases in this workspace (stores the database's name).
    Db,
    /// Dropdown of the properties of the database chosen in the `db` parameter (plus `title`).
    Prop,
    /// Masked text (tokens, passwords, webhook URLs). Still stored in the flow, so use `{{vars.x}}` when you share flows.
    Secret,
    /// `Property = value` rows: property dropdown + a value editor that fits the property (select options, checkbox, date …).
    Fields,
}

pub struct P {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: PK,
    pub def: &'static str,
    pub hint: &'static str,
}

pub struct Def {
    pub key: &'static str,
    pub label: &'static str,
    pub cat: Cat,
    pub ins: &'static [&'static str],
    pub outs: &'static [&'static str],
    pub params: &'static [P],
    pub doc: &'static str,
}

const fn p(key: &'static str, label: &'static str, kind: PK, def: &'static str, hint: &'static str) -> P {
    P { key, label, kind, def, hint }
}

const TESTS: &[&str] = &["truthy", "falsy", "equals", "not equals", "greater than", "less than", "contains", "starts with", "is empty", "is not empty"];
const AGG: &[&str] = &["count", "sum", "avg", "min", "max", "join", "unique", "first", "last", "flatten"];
const METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE"];
const MATH: &[&str] = &["add", "subtract", "multiply", "divide", "modulo", "power", "min", "max", "round"];
const AUTH: &[&str] = &["none", "bearer token", "basic (user:password)", "x-api-key header", "api_key query"];
const SERVICES: &[&str] = &["JSONPlaceholder todos", "GitHub repository", "GitHub open issues", "GitHub releases", "Weather (Open-Meteo)", "Hacker News front page", "Wikipedia summary", "Exchange rates", "Crypto price (CoinGecko)", "Random joke"];
const MSG_SERVICES: &[&str] = &["ntfy.sh push", "Slack webhook", "Discord webhook", "Microsoft Teams webhook", "Telegram bot", "Generic JSON webhook"];
const LOGIC: &[&str] = &["and", "or", "xor", "not"];

pub static DEFS: &[Def] = &[
    Def { key: "manual", label: "Manual input", cat: Cat::Input, ins: &[], outs: &["out"], params: &[p("payload", "Payload (JSON)", PK::Long, "{\"hello\": \"world\"}", "Any JSON value. Plain text works too.")], doc: "Starts a flow with a value you type in. Use it for tests and for flows you run with the ▶ button." },
    Def { key: "text", label: "Text", cat: Cat::Input, ins: &[], outs: &["out"], params: &[p("value", "Text", PK::Long, "", "{{vars.name}} inserts a flow variable.")], doc: "A piece of text. {{vars.x}} inserts a variable of this flow." },
    Def { key: "number", label: "Number", cat: Cat::Input, ins: &[], outs: &["out"], params: &[p("value", "Value", PK::Num, "0", "")], doc: "A constant number." },
    Def { key: "now", label: "Date & time", cat: Cat::Input, ins: &[], outs: &["text", "date", "ms"], params: &[p("days", "Offset (days)", PK::Num, "0", "Negative = past")], doc: "Local date and time at the moment of the run: `YYYY-MM-DD HH:MM`, `YYYY-MM-DD` and the Unix time in milliseconds." },
    Def { key: "random", label: "Random number", cat: Cat::Input, ins: &[], outs: &["out"], params: &[p("min", "Minimum", PK::Num, "0", ""), p("max", "Maximum", PK::Num, "100", ""), p("int", "Whole numbers", PK::Bool, "true", "")], doc: "A random number between minimum and maximum." },
    Def { key: "if", label: "If", cat: Cat::Logic, ins: &["value", "other"], outs: &["true", "false"], params: &[p("test", "Test", PK::Choice(TESTS), "truthy", ""), p("rhs", "Compare with", PK::Text, "", "Used when 'other' is not connected")], doc: "Routes the value to **true** or **false**. The other branch stays inactive and everything behind it is skipped." },
    Def { key: "switch", label: "Switch", cat: Cat::Logic, ins: &["value"], outs: &["case 1", "case 2", "case 3", "default"], params: &[p("c1", "Case 1 equals", PK::Text, "", ""), p("c2", "Case 2 equals", PK::Text, "", ""), p("c3", "Case 3 equals", PK::Text, "", "")], doc: "Sends the value to the first output whose case matches (text comparison), otherwise to **default**." },
    Def { key: "merge", label: "Merge", cat: Cat::Logic, ins: &["a", "b", "c", "d"], outs: &["out"], params: &[p("mode", "Mode", PK::Choice(&["list", "object", "concat", "first"]), "list", "list: [a, b …] · object: merge objects · concat: join lists · first: first active input")], doc: "Combines up to four branches. Inactive inputs are ignored, so Merge also re-joins the branches of an If." },
    Def { key: "compare", label: "Compare", cat: Cat::Logic, ins: &["a", "b"], outs: &["result"], params: &[p("test", "Test", PK::Choice(TESTS), "equals", ""), p("rhs", "b (if not connected)", PK::Text, "", "")], doc: "Outputs true / false." },
    Def { key: "logic", label: "Logic", cat: Cat::Logic, ins: &["a", "b"], outs: &["result"], params: &[p("op", "Operation", PK::Choice(LOGIC), "and", "")], doc: "AND / OR / XOR / NOT of two values (truthiness)." },
    Def { key: "delay", label: "Delay", cat: Cat::Logic, ins: &["value"], outs: &["out"], params: &[p("ms", "Milliseconds", PK::Num, "1000", "At most 60 000")], doc: "Waits, then passes the value on. Other branches keep running meanwhile." },
    Def { key: "get", label: "Get field", cat: Cat::Data, ins: &["value"], outs: &["value", "missing"], params: &[p("path", "Path", PK::Text, "data.items[0].name", "a.b[0].c")], doc: "Reads a field with a path. If it does not exist the original value leaves through **missing**." },
    Def { key: "set", label: "Set fields", cat: Cat::Data, ins: &["value"], outs: &["out"], params: &[p("fields", "Fields (key = value per line)", PK::Long, "title = {{input.name}}\ndone = false", "{{path}} alone keeps the type (number, list …); text around it makes text.")], doc: "Adds / replaces fields of an object (or builds a new one). Values can use {{templates}}." },
    Def { key: "template", label: "Text template", cat: Cat::Data, ins: &["a", "b"], outs: &["text"], params: &[p("text", "Template", PK::Long, "Hello {{a.name}}!", "{{a}} {{b.x}} {{input}} {{vars.name}}")], doc: "Builds text from the inputs." },
    Def { key: "map", label: "Map list", cat: Cat::Data, ins: &["items", "with"], outs: &["out"], params: &[p("expr", "Expression per item", PK::Code, "item.price * 2", "item, index, with")], doc: "Transforms every item of a list with a LumenScript expression, e.g. `{name: item.title, n: index}`." },
    Def { key: "filter", label: "Filter list", cat: Cat::Data, ins: &["items", "with"], outs: &["pass", "rejected"], params: &[p("expr", "Keep when", PK::Code, "item.price > 10", "item, index, with")], doc: "Splits a list in two: items for which the expression is true, and the rest." },
    Def { key: "sort", label: "Sort list", cat: Cat::Data, ins: &["items"], outs: &["out"], params: &[p("key", "Sort by field", PK::Text, "", "Empty = the items themselves"), p("order", "Order", PK::Choice(&["ascending", "descending"]), "ascending", "")], doc: "Sorts a list (numbers numerically, text case-insensitively)." },
    Def { key: "limit", label: "Limit list", cat: Cat::Data, ins: &["items"], outs: &["out"], params: &[p("n", "How many", PK::Num, "10", ""), p("from", "From", PK::Choice(&["start", "end"]), "start", "")], doc: "Keeps the first or last N items." },
    Def { key: "aggregate", label: "Aggregate", cat: Cat::Data, ins: &["items"], outs: &["out"], params: &[p("op", "Operation", PK::Choice(AGG), "count", ""), p("field", "Field", PK::Text, "", "Optional path inside every item"), p("sep", "Separator (join)", PK::Text, ", ", "")], doc: "Reduces a list to one value: count, sum, average, min, max, join, unique, first, last or flatten." },
    Def { key: "math", label: "Math", cat: Cat::Data, ins: &["a", "b"], outs: &["result"], params: &[p("op", "Operation", PK::Choice(MATH), "add", ""), p("b", "b (if not connected)", PK::Num, "1", "")], doc: "Arithmetic on two numbers." },
    Def { key: "split", label: "Split text", cat: Cat::Data, ins: &["text"], outs: &["list"], params: &[p("sep", "Separator", PK::Text, ",", "Empty = lines")], doc: "Splits text into a list (trimmed, empty parts removed)." },
    Def { key: "json_parse", label: "Parse JSON", cat: Cat::Data, ins: &["text"], outs: &["value", "error"], params: &[], doc: "Turns JSON text into data. Invalid text leaves through **error**." },
    Def { key: "json_text", label: "To JSON text", cat: Cat::Data, ins: &["value"], outs: &["text"], params: &[p("pretty", "Pretty print", PK::Bool, "true", "")], doc: "Turns data into JSON text." },
    Def { key: "hash", label: "SHA-256", cat: Cat::Data, ins: &["text"], outs: &["hex"], params: &[], doc: "SHA-256 of a text, as hexadecimal." },
    Def { key: "code", label: "Code (LumenScript)", cat: Cat::Data, ins: &["a", "b", "c"], outs: &["out", "out2", "out3"], params: &[p("code", "Script", PK::Code, "// a, b, c are the inputs; vars is a map\nemit(a)\n// emit_to(\"out2\", 42)", "Full workspace API: db(\"Tasks\").rows() …")], doc: "Runs a LumenScript with the inputs as variables. `emit(x)` writes to the first output, `emit_to(\"out2\", x)` to another one. Has access to the workspace API (db, notify …) — use Dry run to try it safely." },
    Def { key: "http", label: "HTTP request", cat: Cat::Network, ins: &["url", "body"], outs: &["body", "status", "error"], params: &[p("method", "Method", PK::Choice(METHODS), "GET", ""), p("url", "URL", PK::Text, "https://api.github.com/repos/rust-lang/rust", "{{vars.base}}/users — used when 'url' is not connected"), p("query", "Query (name = value per line)", PK::Long, "", "Added to the URL and encoded for you. {{templates}} allowed."), p("auth", "Authentication", PK::Choice(AUTH), "none", "Where the secret below goes."), p("token", "Token / key / user:password", PK::Secret, "", "Tip: store it as a flow variable and write {{vars.token}} here."), p("headers", "Headers (Name: value per line)", PK::Long, "Accept: application/json", ""), p("body", "Body", PK::Long, "", "Used when 'body' is not connected. Objects are sent as JSON."), p("retries", "Retries on failure", PK::Choice(&["0", "1", "2", "3"]), "0", "Network errors and HTTP 5xx / 429 are retried."), p("proxy", "Via server proxy (web, avoids CORS)", PK::Bool, "true", "Browsers block most cross-site requests. The proxy is the website's /api/flow-proxy. Ignored in the Android and Windows apps (they call the API directly).")], doc: "Calls any HTTP API. JSON answers are parsed into data. Success leaves through **body** and **status**, failures (network error or status ≥ 400) through **error**. Independent requests run in parallel." },
    Def { key: "api", label: "Ready-made API", cat: Cat::Network, ins: &["arg"], outs: &["body", "status", "error"], params: &[p("service", "Service", PK::Choice(SERVICES), "JSONPlaceholder todos", "Pick an API – no keys, no URLs to remember."), p("arg", "Argument", PK::Text, "5", "GitHub: owner/repo · Weather: lat,lon · Wikipedia: page title · Exchange rates: USD · Crypto: bitcoin · Todos: how many")], doc: "A catalogue of free public APIs (GitHub, weather, Hacker News, Wikipedia, exchange rates, crypto prices, demo todos …). Choose one, give it an argument, and the answer flows on as data." },
    Def { key: "rss", label: "RSS / Atom feed", cat: Cat::Network, ins: &["url"], outs: &["items", "title", "error"], params: &[p("url", "Feed URL", PK::Text, "https://hnrss.org/frontpage", "Used when 'url' is not connected"), p("limit", "Max items", PK::Num, "20", "")], doc: "Reads a news / blog feed into a list of `{title, link, date, summary}`. Combine with Filter, Map and Add row to build a reading list." },
    Def { key: "message", label: "Send message", cat: Cat::Network, ins: &["message"], outs: &["sent", "error"], params: &[p("service", "Service", PK::Choice(MSG_SERVICES), "ntfy.sh push", ""), p("target", "Webhook URL / topic / bot token", PK::Secret, "", "ntfy: a topic name · Slack, Discord, Teams, generic: the webhook URL · Telegram: the bot token"), p("chat", "Chat id / title", PK::Text, "", "Telegram: chat id · ntfy and generic: optional title"), p("text", "Message (if not connected)", PK::Long, "{{input}}", "{{templates}} allowed")], doc: "Pushes a message to your phone or team chat: ntfy.sh, Slack, Discord, Microsoft Teams, Telegram or any JSON webhook. Calls go through the server proxy on the web." },
    Def { key: "cloud_in", label: "Cloud inbox (webhook in)", cat: Cat::Input, ins: &[], outs: &["events", "first", "count", "error"], params: &[p("server", "Server", PK::Text, "", "Empty = the website you are on (web). Android / Windows: https://your-site.example"), p("channel", "Channel", PK::Text, "inbox", "Anything can POST to /api/hooks/<channel> – GitHub, Stripe, Zapier, curl, a shortcut on your phone …"), p("key", "Channel key (optional)", PK::Secret, "", "If set, senders must send the same key in the x-hook-key header"), p("consume", "Remove events after reading", PK::Bool, "true", ""), p("limit", "Max events", PK::Num, "50", "")], doc: "Collects what other services pushed to your cloud inbox since the last run. **events** stays inactive when nothing arrived, so the rest of the flow is skipped – perfect for scheduled flows (Flow menu → run every N seconds)." },
    Def { key: "cloud_out", label: "Cloud inbox (send)", cat: Cat::Network, ins: &["data"], outs: &["ok", "error"], params: &[p("server", "Server", PK::Text, "", "Empty = the website you are on (web)"), p("channel", "Channel", PK::Text, "inbox", ""), p("key", "Channel key (optional)", PK::Secret, "", "")], doc: "Stores the value as an event in a cloud channel so another flow, device or service can pick it up (the Android app and the web app share channels)." },
    Def { key: "csv", label: "CSV → rows", cat: Cat::Data, ins: &["text"], outs: &["rows", "headers"], params: &[p("delim", "Separator", PK::Choice(&["comma", "semicolon", "tab"]), "comma", ""), p("header", "First line is a header", PK::Bool, "true", "")], doc: "Turns CSV text into a list of objects (one per line)." },
    Def { key: "db_query", label: "Query database", cat: Cat::Workspace, ins: &[], outs: &["rows", "count", "first"], params: &[p("db", "Database", PK::Db, "", "Pick one of your databases"), p("prop", "Where property", PK::Prop, "", "Optional filter"), p("value", "equals", PK::Text, "", "{{templates}} allowed"), p("limit", "Limit", PK::Num, "200", "")], doc: "Reads rows of a database as a list of objects (property name → value, plus `title`)." },
    Def { key: "db_add", label: "Add row / task", cat: Cat::Workspace, ins: &["data"], outs: &["row"], params: &[p("db", "Database", PK::Db, "", "Pick one of your databases"), p("fields", "Fields to set", PK::Fields, "", "Written over the incoming object. {{input.x}} reads the incoming value; with a list each item is handled separately.")], doc: "Creates a row (for example a task) from an object (`title`, property names …) and/or the fields below. A list creates one row per item." },
    Def { key: "db_update", label: "Update row / task", cat: Cat::Workspace, ins: &["data"], outs: &["row", "missing"], params: &[p("db", "Database", PK::Db, "", "Pick one of your databases"), p("prop", "Find by property", PK::Prop, "title", ""), p("value", "equals", PK::Text, "{{input.title}}", ""), p("fields", "Fields to set", PK::Fields, "", "Merged with the incoming object")], doc: "Finds the first matching row and updates it with the incoming object and/or the fields below. Not found → **missing**." },

    Def { key: "db_query", label: "Query database", cat: Cat::Workspace, ins: &[], outs: &["rows", "count", "first"], params: &[p("db", "Database", PK::Text, "Tasks", "Name (case-insensitive)"), p("prop", "Where property", PK::Text, "", "Optional"), p("value", "equals", PK::Text, "", "{{templates}} allowed"), p("limit", "Limit", PK::Num, "200", "")], doc: "Reads rows of a database as a list of objects (property name → value, plus `title`)." },
    Def { key: "db_add", label: "Add row", cat: Cat::Workspace, ins: &["data"], outs: &["row"], params: &[p("db", "Database", PK::Text, "Tasks", "")], doc: "Creates a row from an object (`title`, property names …). A list creates one row per item." },
    Def { key: "db_update", label: "Update row", cat: Cat::Workspace, ins: &["data"], outs: &["row", "missing"], params: &[p("db", "Database", PK::Text, "Tasks", ""), p("prop", "Find by property", PK::Text, "title", ""), p("value", "equals", PK::Text, "{{input.title}}", "")], doc: "Finds the first matching row and updates it with the object. Not found → **missing**." },
    Def { key: "create_page", label: "Create page", cat: Cat::Workspace, ins: &["body"], outs: &["title"], params: &[p("title", "Title", PK::Text, "Flow result", "{{templates}} allowed")], doc: "Creates a page; the input (text, or JSON) becomes its body." },
    Def { key: "notify", label: "Notify", cat: Cat::Output, ins: &["message"], outs: &["out"], params: &[p("text", "Message (if not connected)", PK::Text, "Flow finished", "")], doc: "Sends a notification (toast, inbox, system notification)." },
    Def { key: "copy", label: "Copy to clipboard", cat: Cat::Output, ins: &["value"], outs: &[], params: &[], doc: "Copies the value (text or JSON) to the clipboard when the run finishes." },
    Def { key: "log", label: "Inspect", cat: Cat::Output, ins: &["value"], outs: &["value"], params: &[p("label", "Label", PK::Text, "", "")], doc: "Shows the value that flows through (and passes it on). Great for debugging." },
    Def { key: "output", label: "Result", cat: Cat::Output, ins: &["value"], outs: &[], params: &[p("label", "Label", PK::Text, "Result", "")], doc: "The end of a branch: its value is listed in the run report." },
    Def { key: "note", label: "Note", cat: Cat::Output, ins: &[], outs: &[], params: &[p("text", "Note", PK::Long, "Describe what this part does.", "")], doc: "A sticky note for people. It never runs." },
];

pub fn def(key: &str) -> Option<&'static Def> {
    DEFS.iter().find(|d| d.key == key)
}

// ───────────────────────────── model ─────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct FNode {
    pub id: u32,
    pub kind: String,
    pub x: f32,
    pub y: f32,
    /// Custom title ("" = the node type's name).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// Parameter values by key (missing = default).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Default)]
pub struct Wire {
    pub from: u32,
    pub fp: u8,
    pub to: u32,
    pub tp: u8,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Flow {
    #[serde(default)]
    pub nodes: Vec<FNode>,
    #[serde(default)]
    pub wires: Vec<Wire>,
    #[serde(default)]
    pub next: u32,
    #[serde(default)]
    pub pan: [f32; 2],
    #[serde(default = "one")]
    pub zoom: f32,
    /// Flow variables, available as `{{vars.name}}` and `vars.name` in scripts.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vars: BTreeMap<String, String>,
    /// Re-run every N seconds while the app is open (0 = off).
    #[serde(default)]
    pub every: u32,
}

fn one() -> f32 {
    1.0
}

impl Default for Flow {
    fn default() -> Self {
        Flow { nodes: vec![], wires: vec![], next: 1, pan: [60.0, 60.0], zoom: 1.0, vars: BTreeMap::new(), every: 0 }
    }
}

pub const NODE_W: f32 = 212.0;
pub const HEAD_H: f32 = 30.0;
pub const ROW_H: f32 = 24.0;
pub const PAD_B: f32 = 30.0;

impl FNode {
    pub fn def(&self) -> Option<&'static Def> {
        def(&self.kind)
    }
    pub fn title(&self) -> String {
        if !self.label.is_empty() {
            return self.label.clone();
        }
        self.def().map_or(self.kind.clone(), |d| d.label.to_owned())
    }
    pub fn param(&self, key: &str) -> String {
        if let Some(v) = self.params.get(key) {
            return v.clone();
        }
        self.def().and_then(|d| d.params.iter().find(|p| p.key == key)).map_or(String::new(), |p| p.def.to_owned())
    }
    pub fn height(&self) -> f32 {
        if self.kind == "note" {
            return 96.0;
        }
        let (i, o) = self.def().map_or((0, 0), |d| (d.ins.len(), d.outs.len()));
        HEAD_H + (i.max(o).max(1) as f32) * ROW_H + PAD_B
    }
    pub fn width(&self) -> f32 {
        if self.kind == "note" {
            230.0
        } else {
            NODE_W
        }
    }
    /// World position of a port (`out` = output side).
    pub fn port_pos(&self, idx: usize, out: bool) -> (f32, f32) {
        let x = if out { self.x + self.width() } else { self.x };
        (x, self.y + HEAD_H + ROW_H * (idx as f32 + 0.5))
    }
}

impl Flow {
    pub fn node(&self, id: u32) -> Option<&FNode> {
        self.nodes.iter().find(|n| n.id == id)
    }
    pub fn node_mut(&mut self, id: u32) -> Option<&mut FNode> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    pub fn add_node(&mut self, kind: &str, x: f32, y: f32) -> Option<u32> {
        if def(kind).is_none() || self.nodes.len() >= MAX_NODES {
            return None;
        }
        let id = self.next.max(self.nodes.iter().map(|n| n.id + 1).max().unwrap_or(1));
        self.next = id + 1;
        self.nodes.push(FNode { id, kind: kind.to_owned(), x, y, ..Default::default() });
        Some(id)
    }

    pub fn remove_node(&mut self, id: u32) {
        self.nodes.retain(|n| n.id != id);
        self.wires.retain(|w| w.from != id && w.to != id);
    }

    /// Does a path of wires lead from `a` to `b`?
    pub fn reaches(&self, a: u32, b: u32) -> bool {
        let mut seen = HashSet::new();
        let mut stack = vec![a];
        while let Some(n) = stack.pop() {
            if n == b {
                return true;
            }
            if seen.insert(n) {
                stack.extend(self.wires.iter().filter(|w| w.from == n).map(|w| w.to));
            }
        }
        false
    }

    pub fn can_connect(&self, from: u32, fp: usize, to: u32, tp: usize) -> Result<(), &'static str> {
        if from == to {
            return Err("A node cannot feed itself");
        }
        let (Some(a), Some(b)) = (self.node(from).and_then(|n| n.def()), self.node(to).and_then(|n| n.def())) else { return Err("Unknown node") };
        if fp >= a.outs.len() {
            return Err("No such output");
        }
        if tp >= b.ins.len() {
            return Err("No such input");
        }
        if self.wires.iter().any(|w| w.from == from && w.fp as usize == fp && w.to == to && w.tp as usize == tp) {
            return Err("Already connected");
        }
        if self.reaches(to, from) {
            return Err("That would create a loop");
        }
        Ok(())
    }

    pub fn connect(&mut self, from: u32, fp: usize, to: u32, tp: usize) -> Result<(), &'static str> {
        self.can_connect(from, fp, to, tp)?;
        self.wires.push(Wire { from, fp: fp as u8, to, tp: tp as u8 });
        Ok(())
    }

    /// Drops wires to ports that no longer exist / nodes that are gone, and breaks cycles in imported data.
    pub fn repair(&mut self) {
        let ids: HashSet<u32> = self.nodes.iter().map(|n| n.id).collect();
        let nodes = self.nodes.clone();
        self.wires.retain(|w| {
            ids.contains(&w.from)
                && ids.contains(&w.to)
                && w.from != w.to
                && nodes.iter().find(|n| n.id == w.from).and_then(|n| n.def()).map_or(false, |d| (w.fp as usize) < d.outs.len())
                && nodes.iter().find(|n| n.id == w.to).and_then(|n| n.def()).map_or(false, |d| (w.tp as usize) < d.ins.len())
        });
        let mut seen = HashSet::new();
        self.wires.retain(|w| seen.insert(*w));
        let all = std::mem::take(&mut self.wires);
        for w in all {
            if !self.reaches(w.to, w.from) {
                self.wires.push(w);
            }
        }
        for n in &mut self.nodes {
            if !n.x.is_finite() || !n.y.is_finite() {
                n.x = 0.0;
                n.y = 0.0;
            }
        }
        if !self.zoom.is_finite() || self.zoom < 0.1 || self.zoom > 3.0 {
            self.zoom = 1.0;
        }
        if !self.pan[0].is_finite() || !self.pan[1].is_finite() {
            self.pan = [60.0, 60.0];
        }
        self.next = self.next.max(self.nodes.iter().map(|n| n.id + 1).max().unwrap_or(1));
    }

    /// Problems that make a run pointless (shown in the toolbar).
    pub fn problems(&self) -> Vec<String> {
        let mut v = vec![];
        for n in &self.nodes {
            if n.def().is_none() {
                v.push(format!("Unknown node type “{}”", n.kind));
            }
        }
        if self.wires.iter().any(|w| self.reaches(w.to, w.from)) {
            v.push("The graph contains a loop".into());
        }
        if !self.nodes.iter().any(|n| n.kind != "note") {
            v.push("Add a node to begin".into());
        }
        v
    }

    pub fn to_json(&self) -> String {
        let mut v = serde_json::to_value(self).unwrap_or(Json::Null);
        if let Some(m) = v.as_object_mut() {
            m.insert("lumen_flow".into(), json!(1));
            m.remove("pan");
            m.remove("zoom");
        }
        serde_json::to_string_pretty(&v).unwrap_or_default()
    }

    /// Parses an exported flow; `None` when the text is not a Lumen flow.
    pub fn from_json(s: &str) -> Option<Flow> {
        let t = s.trim();
        if !t.starts_with('{') || !t.contains("lumen_flow") {
            return None;
        }
        let v: Json = serde_json::from_str(t).ok()?;
        v.get("lumen_flow")?;
        let mut f: Flow = serde_json::from_value(v).ok()?;
        f.nodes.truncate(MAX_NODES);
        f.repair();
        Some(f)
    }

    /// Adds the nodes of `other` (offset, new ids) — used by paste / templates. Returns the new ids.
    pub fn merge_in(&mut self, other: &Flow, dx: f32, dy: f32) -> Vec<u32> {
        let mut map = HashMap::new();
        let mut ids = vec![];
        for n in &other.nodes {
            if let Some(id) = self.add_node(&n.kind, n.x + dx, n.y + dy) {
                if let Some(m) = self.node_mut(id) {
                    m.label = n.label.clone();
                    m.params = n.params.clone();
                    m.disabled = n.disabled;
                }
                map.insert(n.id, id);
                ids.push(id);
            }
        }
        for w in &other.wires {
            if let (Some(&a), Some(&b)) = (map.get(&w.from), map.get(&w.to)) {
                let _ = self.connect(a, w.fp as usize, b, w.tp as usize);
            }
        }
        for (k, v) in &other.vars {
            self.vars.entry(k.clone()).or_insert_with(|| v.clone());
        }
        ids
    }

    /// Bounding box of all nodes in world units: (min x, min y, max x, max y).
    pub fn bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let mut it = self.nodes.iter();
        let f = it.next()?;
        let mut b = (f.x, f.y, f.x + f.width(), f.y + f.height());
        for n in it {
            b = (b.0.min(n.x), b.1.min(n.y), b.2.max(n.x + n.width()), b.3.max(n.y + n.height()));
        }
        Some(b)
    }

    /// Left-to-right layered layout (longest path from the sources).
    pub fn auto_layout(&mut self) {
        let mut depth: HashMap<u32, usize> = HashMap::new();
        for _ in 0..self.nodes.len() + 1 {
            let mut changed = false;
            for w in &self.wires {
                let d = depth.get(&w.from).copied().unwrap_or(0) + 1;
                if depth.get(&w.to).copied().unwrap_or(0) < d {
                    depth.insert(w.to, d);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut cols: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
        for n in &self.nodes {
            cols.entry(depth.get(&n.id).copied().unwrap_or(0)).or_default().push(n.id);
        }
        for (c, ids) in cols {
            let mut y = 0.0;
            for id in ids {
                if let Some(n) = self.node_mut(id) {
                    n.x = c as f32 * (NODE_W + 90.0);
                    n.y = y;
                    y += n.height() + 36.0;
                }
            }
        }
    }
}

// ───────────────────────────── templates ─────────────────────────────

pub struct FlowTemplate {
    pub name: &'static str,
    pub desc: &'static str,
    pub build: fn() -> Flow,
}

fn node(f: &mut Flow, kind: &str, x: f32, y: f32, params: &[(&str, &str)]) -> u32 {
    let id = f.add_node(kind, x, y).unwrap_or(0);
    if let Some(n) = f.node_mut(id) {
        for (k, v) in params {
            n.params.insert((*k).to_owned(), (*v).to_owned());
        }
    }
    id
}

fn tpl_api() -> Flow {
    let mut f = Flow::default();
    let h = node(&mut f, "http", 0.0, 0.0, &[("url", "https://api.github.com/repos/rust-lang/rust/releases?per_page=10")]);
    let m = node(&mut f, "map", 290.0, 0.0, &[("expr", "{name: item.name ?? item.tag_name, pre: item.prerelease}")]);
    let fl = node(&mut f, "filter", 580.0, 0.0, &[("expr", "!item.pre")]);
    let a = node(&mut f, "aggregate", 870.0, -60.0, &[("op", "count")]);
    let t = node(&mut f, "template", 870.0, 110.0, &[("text", "{{a}} stable Rust releases on the last page")]);
    let n = node(&mut f, "notify", 1160.0, 60.0, &[]);
    let r = node(&mut f, "output", 1160.0, 220.0, &[("label", "Stable releases")]);
    let _ = (f.connect(h, 0, m, 0), f.connect(m, 0, fl, 0), f.connect(fl, 0, a, 0), f.connect(a, 0, t, 0), f.connect(t, 0, n, 0), f.connect(fl, 0, r, 0));
    f
}

fn tpl_branch() -> Flow {
    let mut f = Flow::default();
    let r = node(&mut f, "random", 0.0, 40.0, &[("min", "0"), ("max", "100")]);
    let i = node(&mut f, "if", 260.0, 40.0, &[("test", "greater than"), ("rhs", "50")]);
    let a = node(&mut f, "template", 540.0, -40.0, &[("text", "{{a}} is high")]);
    let b = node(&mut f, "template", 540.0, 130.0, &[("text", "{{a}} is low")]);
    let m = node(&mut f, "merge", 820.0, 40.0, &[("mode", "first")]);
    let o = node(&mut f, "output", 1090.0, 40.0, &[]);
    let _ = (f.connect(r, 0, i, 0), f.connect(i, 0, a, 0), f.connect(i, 1, b, 0), f.connect(a, 0, m, 0), f.connect(b, 0, m, 1), f.connect(m, 0, o, 0));
    f
}

fn tpl_report() -> Flow {
    let mut f = Flow::default();
    let q = node(&mut f, "db_query", 0.0, 0.0, &[("db", "Tasks")]);
    let s = node(&mut f, "sort", 280.0, 0.0, &[("key", "title")]);
    let m = node(&mut f, "map", 560.0, 0.0, &[("expr", "\"- {item.title}\"")]);
    let g = node(&mut f, "aggregate", 840.0, 0.0, &[("op", "join"), ("sep", "\n")]);
    let c = node(&mut f, "create_page", 1120.0, 0.0, &[("title", "Task report")]);
    let _ = (f.connect(q, 0, s, 0), f.connect(s, 0, m, 0), f.connect(m, 0, g, 0), f.connect(g, 0, c, 0));
    f
}

fn tpl_parallel() -> Flow {
    let mut f = Flow::default();
    let a = node(&mut f, "http", 0.0, 0.0, &[("url", "https://api.github.com/repos/rust-lang/rust"), ("headers", "Accept: application/json")]);
    let b = node(&mut f, "http", 0.0, 230.0, &[("url", "https://api.github.com/repos/rust-lang/cargo")]);
    let ga = node(&mut f, "get", 290.0, 0.0, &[("path", "stargazers_count")]);
    let gb = node(&mut f, "get", 290.0, 230.0, &[("path", "stargazers_count")]);
    let m = node(&mut f, "merge", 570.0, 100.0, &[("mode", "list")]);
    let s = node(&mut f, "aggregate", 840.0, 100.0, &[("op", "sum")]);
    let o = node(&mut f, "output", 1110.0, 100.0, &[("label", "Stars (rust + cargo)")]);
    let _ = (f.connect(a, 0, ga, 0), f.connect(b, 0, gb, 0), f.connect(ga, 0, m, 0), f.connect(gb, 0, m, 1), f.connect(m, 0, s, 0), f.connect(s, 0, o, 0));
    f
}


fn tpl_digest() -> Flow {
    let mut f = Flow::default();
    let q = node(&mut f, "db_query", 0.0, 0.0, &[("db", "Tasks"), ("limit", "100")]);
    let fl = node(&mut f, "filter", 290.0, 0.0, &[("expr", "str(item.Status ?? \"\") != \"Done\"")]);
    let m = node(&mut f, "map", 580.0, 0.0, &[("expr", "\"• {item.title}\"")]);
    let g = node(&mut f, "aggregate", 870.0, 0.0, &[("op", "join"), ("sep", "\n")]);
    let n = node(&mut f, "notify", 1160.0, -60.0, &[]);
    let c = node(&mut f, "create_page", 1160.0, 90.0, &[("title", "Open tasks – {{today}}")]);
    let _ = (f.connect(q, 0, fl, 0), f.connect(fl, 0, m, 0), f.connect(m, 0, g, 0), f.connect(g, 0, n, 0), f.connect(g, 0, c, 0));
    f
}

fn tpl_todos() -> Flow {
    let mut f = Flow::default();
    let a = node(&mut f, "api", 0.0, 0.0, &[("service", "JSONPlaceholder todos"), ("arg", "8")]);
    let g = node(&mut f, "get", 290.0, 0.0, &[("path", ".")]);
    let m = node(&mut f, "map", 560.0, 0.0, &[("expr", "{title: item.title}")]);
    let d = node(&mut f, "db_add", 830.0, 0.0, &[("db", "Tasks")]);
    let n = node(&mut f, "notify", 1100.0, 0.0, &[("text", "Imported demo tasks")]);
    let _ = (f.connect(a, 0, g, 0), f.connect(g, 0, m, 0), f.connect(m, 0, d, 0), f.connect(d, 0, n, 0));
    f
}

fn tpl_issues() -> Flow {
    let mut f = Flow::default();
    let a = node(&mut f, "api", 0.0, 0.0, &[("service", "GitHub open issues"), ("arg", "rust-lang/rust")]);
    let l = node(&mut f, "limit", 290.0, 0.0, &[("n", "5")]);
    let m = node(&mut f, "map", 560.0, 0.0, &[("expr", "{title: \"#{item.number} {item.title}\"}")]);
    let d = node(&mut f, "db_add", 830.0, 0.0, &[("db", "Tasks")]);
    let _ = (f.connect(a, 0, l, 0), f.connect(l, 0, m, 0), f.connect(m, 0, d, 0));
    f
}

fn tpl_webhook() -> Flow {
    let mut f = Flow::default();
    let i = node(&mut f, "cloud_in", 0.0, 0.0, &[("channel", "tasks"), ("consume", "true")]);
    let d = node(&mut f, "db_add", 290.0, 0.0, &[("db", "Tasks"), ("fields", "title = {{input.title}}")]);
    let t = node(&mut f, "template", 580.0, 0.0, &[("text", "New task from the cloud: {{a.title}}")]);
    let n = node(&mut f, "notify", 860.0, 0.0, &[]);
    let _ = (f.connect(i, 0, d, 0), f.connect(d, 0, t, 0), f.connect(t, 0, n, 0));
    let note = node(&mut f, "note", 0.0, 200.0, &[("text", "POST {\"title\": \"Buy milk\"} to  /api/hooks/tasks  from any service. Set “Run every N seconds” in the Flow menu to poll automatically.")]);
    let _ = note;
    f
}

fn tpl_news() -> Flow {
    let mut f = Flow::default();
    let r = node(&mut f, "rss", 0.0, 0.0, &[("url", "https://hnrss.org/frontpage"), ("limit", "10")]);
    let l = node(&mut f, "limit", 290.0, 0.0, &[("n", "5")]);
    let m = node(&mut f, "map", 560.0, 0.0, &[("expr", "\"• {item.title}\\n  {item.link}\"")]);
    let g = node(&mut f, "aggregate", 840.0, 0.0, &[("op", "join"), ("sep", "\n")]);
    let s = node(&mut f, "message", 1120.0, -40.0, &[("service", "ntfy.sh push"), ("target", "my-lumen-news"), ("chat", "Daily digest")]);
    let o = node(&mut f, "output", 1120.0, 140.0, &[("label", "Digest")]);
    let _ = (f.connect(r, 0, l, 0), f.connect(l, 0, m, 0), f.connect(m, 0, g, 0), f.connect(g, 0, s, 0), f.connect(g, 0, o, 0));
    f
}

fn tpl_weather() -> Flow {
    let mut f = Flow::default();
    let a = node(&mut f, "api", 0.0, 0.0, &[("service", "Weather (Open-Meteo)"), ("arg", "52.52,13.41")]);
    let t = node(&mut f, "template", 290.0, 0.0, &[("text", "Now {{a.current.temperature_2m}}°C, wind {{a.current.wind_speed_10m}} km/h")]);
    let s = node(&mut f, "message", 580.0, 0.0, &[("service", "ntfy.sh push"), ("target", "my-lumen-weather")]);
    let o = node(&mut f, "output", 580.0, 170.0, &[("label", "Weather")]);
    let _ = (f.connect(a, 0, t, 0), f.connect(t, 0, s, 0), f.connect(t, 0, o, 0));
    f
}

fn tpl_price() -> Flow {
    let mut f = Flow::default();
    let a = node(&mut f, "api", 0.0, 0.0, &[("service", "Crypto price (CoinGecko)"), ("arg", "bitcoin")]);
    let g = node(&mut f, "get", 290.0, 0.0, &[("path", "bitcoin.usd")]);
    let i = node(&mut f, "if", 560.0, 0.0, &[("test", "greater than"), ("rhs", "50000")]);
    let t = node(&mut f, "template", 840.0, -50.0, &[("text", "Bitcoin is above $50,000: ${{a}}")]);
    let o = node(&mut f, "output", 840.0, 120.0, &[("label", "Below the limit")]);
    let n = node(&mut f, "notify", 1110.0, -50.0, &[]);
    let _ = (f.connect(a, 0, g, 0), f.connect(g, 0, i, 0), f.connect(i, 0, t, 0), f.connect(i, 1, o, 0), f.connect(t, 0, n, 0));
    f
}

fn tpl_csv() -> Flow {
    let mut f = Flow::default();
    let t = node(&mut f, "text", 0.0, 0.0, &[("value", "title,Priority\nWrite report,High\nCall the bank,Low\nBook flights,Medium")]);
    let c = node(&mut f, "csv", 290.0, 0.0, &[]);
    let d = node(&mut f, "db_add", 560.0, 0.0, &[("db", "Tasks")]);
    let n = node(&mut f, "notify", 830.0, 0.0, &[("text", "CSV imported")]);
    let _ = (f.connect(t, 0, c, 0), f.connect(c, 0, d, 0), f.connect(d, 0, n, 0));
    f
}

pub const TEMPLATES: &[FlowTemplate] = &[
    FlowTemplate { name: "Open tasks digest", desc: "Database → keep unfinished tasks → bullet list → notification and a new page.", build: tpl_digest },
    FlowTemplate { name: "Public API → tasks", desc: "Fetch demo todos from a ready-made API and add them to your Tasks database.", build: tpl_todos },
    FlowTemplate { name: "GitHub issues → tasks", desc: "Turn the newest open issues of a repository into tasks.", build: tpl_issues },
    FlowTemplate { name: "Webhook inbox → tasks", desc: "Anything that POSTs to your cloud inbox creates a task here (Android, web and Windows share it).", build: tpl_webhook },
    FlowTemplate { name: "News digest → phone push", desc: "RSS feed → top 5 → one message sent to your phone via ntfy.sh.", build: tpl_news },
    FlowTemplate { name: "Weather → push message", desc: "Ready-made weather API → sentence → push notification.", build: tpl_weather },
    FlowTemplate { name: "Price alert (If / Merge)", desc: "Crypto price API → compare → alert only when it is above your limit.", build: tpl_price },
    FlowTemplate { name: "CSV → database", desc: "Paste CSV text, parse it, and create one row per line.", build: tpl_csv },
    FlowTemplate { name: "API → list → notification", desc: "Call a public API, keep some items, count them and notify you.", build: tpl_api },
    FlowTemplate { name: "Branch on a condition", desc: "If / Merge: two different texts depending on a number, joined again afterwards.", build: tpl_branch },
    FlowTemplate { name: "Database → report page", desc: "Read a database, sort it, and write the result into a new page.", build: tpl_report },
    FlowTemplate { name: "Two APIs in parallel", desc: "Two requests run at the same time; Merge waits for both.", build: tpl_parallel },
];

// ───────────────────────────── JSON helpers ─────────────────────────────

pub fn text_of(j: &Json) -> String {
    match j {
        Json::Null => String::new(),
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Short single-line preview for node badges.
pub fn preview(j: &Json, max: usize) -> String {
    let s = match j {
        Json::String(s) => format!("“{}”", s.replace('\n', " ⏎ ")),
        Json::Array(a) => format!("list · {} items  {}", a.len(), j),
        other => other.to_string(),
    };
    if s.chars().count() > max {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    } else {
        s
    }
}

/// `a.b[0].c` — reads a path; `None` when something on the way is missing.
pub fn get_path<'a>(root: &'a Json, path: &str) -> Option<&'a Json> {
    let mut cur = root;
    let path = path.trim();
    if path.is_empty() || path == "." {
        return Some(cur);
    }
    let mut rest = path;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('.') {
            rest = r;
            continue;
        }
        if let Some(r) = rest.strip_prefix('[') {
            let end = r.find(']')?;
            let key = r[..end].trim().trim_matches(|c| c == '"' || c == '\'');
            cur = match cur {
                Json::Array(a) => {
                    let i: i64 = key.parse().ok()?;
                    let i = if i < 0 { a.len() as i64 + i } else { i };
                    a.get(usize::try_from(i).ok()?)?
                }
                Json::Object(m) => m.get(key)?,
                _ => return None,
            };
            rest = &r[end + 1..];
        } else {
            let end = rest.find(|c| c == '.' || c == '[').unwrap_or(rest.len());
            let key = &rest[..end];
            cur = match cur {
                Json::Object(m) => m.get(key)?,
                _ => return None,
            };
            rest = &rest[end..];
        }
    }
    Some(cur)
}

/// `{{ path }}` templating. A template that is exactly one `{{path}}` keeps the JSON type.
pub fn render_value(tpl: &str, ctx: &Json) -> Json {
    let t = tpl.trim();
    if t.starts_with("{{") && t.ends_with("}}") && t.matches("{{").count() == 1 {
        let p = t[2..t.len() - 2].trim();
        return get_path(ctx, p).cloned().unwrap_or(Json::Null);
    }
    Json::String(render(tpl, ctx))
}

pub fn render(tpl: &str, ctx: &Json) -> String {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        match after.find("}}") {
            Some(j) => {
                let path = after[..j].trim();
                if let Some(v) = get_path(ctx, path) {
                    out.push_str(&text_of(v));
                }
                rest = &after[j + 2..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn as_num(j: &Json) -> Option<f64> {
    match j {
        Json::Number(n) => n.as_f64(),
        Json::String(s) => s.trim().parse().ok(),
        Json::Bool(b) => Some(*b as u8 as f64),
        _ => None,
    }
}

fn num_json(n: f64) -> Json {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.0e15 {
        json!(n as i64)
    } else if n.is_finite() {
        json!(n)
    } else {
        Json::Null
    }
}

fn truthy(j: &Json) -> bool {
    match j {
        Json::Null => false,
        Json::Bool(b) => *b,
        Json::Number(n) => n.as_f64().map_or(false, |x| x != 0.0),
        Json::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn is_empty(j: &Json) -> bool {
    match j {
        Json::Null => true,
        Json::String(s) => s.trim().is_empty(),
        Json::Array(a) => a.is_empty(),
        Json::Object(m) => m.is_empty(),
        _ => false,
    }
}

fn cmp_json(a: &Json, b: &Json) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        (Json::Null, Json::Null) => Equal,
        (Json::Null, _) => Greater,
        (_, Json::Null) => Less,
        _ => match (as_num(a), as_num(b)) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Equal),
            _ => text_of(a).to_lowercase().cmp(&text_of(b).to_lowercase()),
        },
    }
}

fn loose_eq(a: &Json, b: &Json) -> bool {
    match (as_num(a), as_num(b)) {
        (Some(x), Some(y)) if !matches!(a, Json::Bool(_)) && !matches!(b, Json::Bool(_)) => x == y,
        _ => text_of(a) == text_of(b),
    }
}

pub fn run_test(test: &str, lhs: &Json, rhs: &Json) -> bool {
    match test {
        "truthy" => truthy(lhs),
        "falsy" => !truthy(lhs),
        "equals" => loose_eq(lhs, rhs),
        "not equals" => !loose_eq(lhs, rhs),
        "greater than" => !lhs.is_null() && cmp_json(lhs, rhs) == std::cmp::Ordering::Greater,
        "less than" => !lhs.is_null() && cmp_json(lhs, rhs) == std::cmp::Ordering::Less,
        "contains" => match lhs {
            Json::Array(a) => a.iter().any(|x| loose_eq(x, rhs)),
            Json::Object(m) => m.contains_key(&text_of(rhs)),
            other => text_of(other).to_lowercase().contains(&text_of(rhs).to_lowercase()),
        },
        "starts with" => text_of(lhs).to_lowercase().starts_with(&text_of(rhs).to_lowercase()),
        "is empty" => is_empty(lhs),
        "is not empty" => !is_empty(lhs),
        _ => false,
    }
}

fn to_script(j: &Json) -> crate::script::Value {
    use crate::script::Value;
    match j {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => Value::Num(n.as_f64().unwrap_or(0.0)),
        Json::String(s) => Value::str(s),
        Json::Array(a) => Value::list(a.iter().map(to_script).collect()),
        Json::Object(m) => Value::map(m.iter().map(|(k, v)| (k.clone(), to_script(v))).collect()),
    }
}

fn from_script(v: &crate::script::Value, depth: u8) -> Json {
    use crate::script::Value;
    if depth > 24 {
        return Json::Null;
    }
    match v {
        Value::Null | Value::Func(_) | Value::Native(_) => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Num(n) => num_json(*n),
        Value::Str(s) => Json::String(s.to_string()),
        Value::List(l) => Json::Array(l.borrow().iter().map(|x| from_script(x, depth + 1)).collect()),
        Value::Map(m) => Json::Object(m.borrow().iter().map(|(k, x)| (k.clone(), from_script(x, depth + 1))).collect::<Map<String, Json>>()),
        Value::Host(h) => json!(format!("{}#{}", v.type_name(), h.id)),
    }
}

// ───────────────────────────── engine ─────────────────────────────

#[derive(Clone, PartialEq, Debug)]
pub enum St {
    Idle,
    Running,
    /// Finished after N milliseconds.
    Done(u32),
    /// Not executed: all connected inputs were inactive.
    Skipped,
    /// Not executed: an upstream node failed.
    Blocked,
    Error(String),
}

struct HttpRes {
    status: u16,
    ok: bool,
    text: String,
}

type Shared = Arc<Mutex<Option<Result<HttpRes, String>>>>;

enum Pending {
    Http { slot: Shared, proxied: bool, post: &'static str, req: ehttp::Request, left: u8 },
    Timer { until: f64, out: Vec<Option<Json>> },
}

enum Step {
    Done(Vec<Option<Json>>),
    Wait(Pending),
}

struct Env<'a> {
    ws: &'a mut Workspace,
    name: &'a str,
    vars: &'a BTreeMap<String, String>,
    now: f64,
    out: &'a mut Output,
    copy: &'a mut Option<String>,
    log: &'a mut Vec<String>,
    seed: &'a mut u64,
}

/// One execution of a flow. Poll it with [`Run::step`] once per frame until [`Run::finished`].
pub struct Run {
    pub name: String,
    pub status: HashMap<u32, St>,
    pub values: HashMap<u32, Vec<Option<Json>>>,
    /// Lines of the run report.
    pub log: Vec<String>,
    pub results: Vec<(String, Json)>,
    pub finished: bool,
    pub dry: bool,
    pub started: f64,
    pub elapsed_ms: u32,
    pub output: Output,
    pub copy: Option<String>,
    nodes: Vec<FNode>,
    wires: Vec<Wire>,
    vars: BTreeMap<String, String>,
    set: HashSet<u32>,
    pending: Vec<(u32, Pending, f64)>,
    ws_copy: Option<Workspace>,
    seed: u64,
}

impl Run {
    /// `only`: run just this node and what it depends on.
    pub fn start(flow: &Flow, name: &str, only: Option<u32>, dry: bool, ws: &Workspace, now: f64) -> Run {
        let set: HashSet<u32> = match only {
            Some(t) => {
                let mut s = HashSet::new();
                let mut stack = vec![t];
                while let Some(n) = stack.pop() {
                    if s.insert(n) {
                        stack.extend(flow.wires.iter().filter(|w| w.to == n).map(|w| w.from));
                    }
                }
                s
            }
            None => flow.nodes.iter().map(|n| n.id).collect(),
        };
        let set: HashSet<u32> = set.into_iter().filter(|id| flow.node(*id).map_or(false, |n| n.kind != "note")).collect();
        let status = set.iter().map(|id| (*id, St::Idle)).collect();
        Run {
            name: name.to_owned(),
            status,
            values: HashMap::new(),
            log: vec![],
            results: vec![],
            finished: false,
            dry,
            started: now,
            elapsed_ms: 0,
            output: Output::default(),
            copy: None,
            nodes: flow.nodes.clone(),
            wires: flow.wires.clone(),
            vars: flow.vars.clone(),
            set,
            pending: vec![],
            ws_copy: if dry { Some(ws.clone()) } else { None },
            seed: (now as u64) ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    pub fn errors(&self) -> usize {
        self.status.values().filter(|s| matches!(s, St::Error(_))).count()
    }
    pub fn count_done(&self) -> usize {
        self.status.values().filter(|s| matches!(s, St::Done(_))).count()
    }
    /// Is something still waiting for the network / a timer (the UI keeps repainting)?
    pub fn waiting(&self) -> bool {
        !self.pending.is_empty()
    }

    fn terminal(s: &St) -> bool {
        matches!(s, St::Done(_) | St::Skipped | St::Blocked | St::Error(_))
    }

    /// (inputs, number of connected input ports, did an upstream node fail)
    fn gather(&self, id: u32, d: &Def) -> (Vec<Option<Json>>, usize, bool) {
        let mut ins = vec![None; d.ins.len()];
        let mut connected = 0;
        let mut any_error = false;
        for (i, slot) in ins.iter_mut().enumerate() {
            let mut ws: Vec<&Wire> = self.wires.iter().filter(|w| w.to == id && w.tp as usize == i && self.set.contains(&w.from)).collect();
            if ws.is_empty() {
                continue;
            }
            connected += 1;
            ws.sort_by_key(|w| (w.from, w.fp));
            let mut vals = vec![];
            for w in &ws {
                if matches!(self.status.get(&w.from), Some(St::Error(_)) | Some(St::Blocked)) {
                    any_error = true;
                }
                if let Some(Some(v)) = self.values.get(&w.from).and_then(|o| o.get(w.fp as usize)) {
                    vals.push(v.clone());
                }
            }
            *slot = match vals.len() {
                0 => None,
                1 if ws.len() == 1 => vals.pop(),
                _ => Some(Json::Array(vals)),
            };
        }
        (ins, connected, any_error)
    }

    /// Advances the run. `ws` is only touched when this is not a dry run.
    pub fn step(&mut self, ws: &mut Workspace, now: f64) {
        if self.finished {
            return;
        }
        let mut copy = self.ws_copy.take();
        // 1) poll what is in flight
        let mut still = vec![];
        for (id, mut pend, t0) in std::mem::take(&mut self.pending) {
            let res: Option<Result<Vec<Option<Json>>, String>> = match &mut pend {
                Pending::Timer { until, out } => (now >= *until).then(|| Ok(out.clone())),
                Pending::Http { slot, proxied, post, req, left } => {
                    let got = slot.lock().ok().and_then(|mut g| g.take());
                    match got {
                        None => None,
                        Some(r) => {
                            let failed = match &r {
                                Err(_) => true,
                                Ok(h) => !*proxied && (h.status >= 500 || h.status == 429),
                            };
                            if failed && *left > 0 {
                                *left -= 1;
                                *slot = launch(req.clone());
                                None
                            } else {
                                Some(r.and_then(|h| http_outputs(h, *proxied)).map(|o| shape(post, o)))
                            }
                        }
                    }
                }
            };
            match res {
                None => still.push((id, pend, t0)),
                Some(r) => self.finish_node(id, r, now - t0),
            }
        }
        self.pending = still;

        // 2) start everything that became ready (synchronous nodes complete immediately)
        let mut budget = 64;
        loop {
            let mut progressed = false;
            let mut ready: Vec<u32> = self
                .set
                .iter()
                .copied()
                .filter(|id| matches!(self.status.get(id), Some(St::Idle)))
                .filter(|id| self.wires.iter().filter(|w| w.to == *id && self.set.contains(&w.from)).all(|w| self.status.get(&w.from).map_or(true, Self::terminal)))
                .collect();
            ready.sort_unstable();
            for id in ready {
                if budget == 0 {
                    break;
                }
                budget -= 1;
                progressed = true;
                let Some(n) = self.nodes.iter().find(|n| n.id == id).cloned() else {
                    self.status.insert(id, St::Skipped);
                    continue;
                };
                let Some(d) = n.def() else {
                    self.status.insert(id, St::Error("unknown node type".into()));
                    continue;
                };
                let (ins, connected, upstream_failed) = self.gather(id, d);
                if n.disabled {
                    self.status.insert(id, St::Skipped);
                    continue;
                }
                if connected > 0 && ins.iter().all(|v| v.is_none()) {
                    self.status.insert(id, if upstream_failed { St::Blocked } else { St::Skipped });
                    continue;
                }
                let target: &mut Workspace = match copy.as_mut() {
                    Some(c) => c,
                    None => &mut *ws,
                };
                let mut env = Env { ws: target, name: &self.name, vars: &self.vars, now, out: &mut self.output, copy: &mut self.copy, log: &mut self.log, seed: &mut self.seed };
                let res = exec(&n, d, &ins, &mut env);
                match res {
                    Ok(Step::Done(o)) => self.finish_node(id, Ok(o), 0.0),
                    Ok(Step::Wait(p)) => {
                        self.status.insert(id, St::Running);
                        self.pending.push((id, p, now));
                    }
                    Err(e) => self.finish_node(id, Err(e), 0.0),
                }
            }
            if !progressed || budget == 0 {
                break;
            }
        }
        self.ws_copy = copy;
        self.elapsed_ms = (now - self.started).max(0.0) as u32;
        if self.pending.is_empty() && !self.status.values().any(|s| matches!(s, St::Idle | St::Running)) {
            self.finished = true;
            self.collect_results();
        }
    }

    fn finish_node(&mut self, id: u32, r: Result<Vec<Option<Json>>, String>, ms: f64) {
        match r {
            Ok(o) => {
                self.values.insert(id, o);
                self.status.insert(id, St::Done(ms.max(0.0) as u32));
            }
            Err(e) => {
                let title = self.nodes.iter().find(|n| n.id == id).map_or(String::new(), |n| n.title());
                self.log.push(format!("✖ {title}: {e}"));
                self.status.insert(id, St::Error(e));
            }
        }
    }

    fn collect_results(&mut self) {
        let mut ids: Vec<u32> = self.set.iter().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let Some(n) = self.nodes.iter().find(|n| n.id == id) else { continue };
            if n.kind == "output" {
                if let Some(Some(v)) = self.values.get(&id).and_then(|v| v.first()) {
                    self.results.push((n.param("label"), v.clone()));
                }
            }
        }
    }

    /// Stops a run (pending requests are simply ignored when they arrive).
    pub fn cancel(&mut self) {
        self.pending.clear();
        for s in self.status.values_mut() {
            if matches!(s, St::Idle | St::Running) {
                *s = St::Skipped;
            }
        }
        self.finished = true;
    }
}


// ───────────────────────────── connector helpers ─────────────────────────────

/// Percent-encoding for URL components.
pub fn pct(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

pub fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        o.push(T[(n >> 18) as usize & 63] as char);
        o.push(T[(n >> 12) as usize & 63] as char);
        o.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        o.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    o
}

fn add_query(url: &mut String, q: &[(String, String)]) {
    for (k, v) in q {
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(&pct(k));
        url.push('=');
        url.push_str(&pct(v));
    }
}

/// Base URL of the cloud server: empty = same origin (web); a bare host gets `https://`.
fn server_of(s: &str) -> String {
    let t = s.trim().trim_end_matches('/');
    if t.is_empty() || t.starts_with("http://") || t.starts_with("https://") {
        t.to_owned()
    } else {
        format!("https://{t}")
    }
}

fn preset_url(service: &str, arg: &str) -> Result<String, String> {
    let need = |what: &str| -> Result<String, String> {
        if arg.is_empty() {
            Err(format!("{service} needs an argument: {what}"))
        } else {
            Ok(arg.to_owned())
        }
    };
    Ok(match service {
        "GitHub repository" => format!("https://api.github.com/repos/{}", need("owner/repo, e.g. rust-lang/rust")?),
        "GitHub open issues" => format!("https://api.github.com/repos/{}/issues?state=open&per_page=30", need("owner/repo, e.g. rust-lang/rust")?),
        "GitHub releases" => format!("https://api.github.com/repos/{}/releases?per_page=10", need("owner/repo, e.g. rust-lang/rust")?),
        "Weather (Open-Meteo)" => {
            let a = if arg.is_empty() { "52.52,13.41".to_owned() } else { arg.to_owned() };
            let (la, lo) = a.split_once(',').ok_or("Weather needs “latitude,longitude”, e.g. 52.52,13.41")?;
            format!("https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}&current=temperature_2m,wind_speed_10m,weather_code&daily=temperature_2m_max,temperature_2m_min&timezone=auto", pct(la.trim()), pct(lo.trim()))
        }
        "Hacker News front page" => "https://hn.algolia.com/api/v1/search?tags=front_page".to_owned(),
        "Wikipedia summary" => format!("https://en.wikipedia.org/api/rest_v1/page/summary/{}", pct(&need("a page title, e.g. Rust (programming language)")?.replace(' ', "_"))),
        "Exchange rates" => format!("https://open.er-api.com/v6/latest/{}", pct(&(if arg.is_empty() { "USD".to_owned() } else { arg.to_uppercase() }))),
        "Crypto price (CoinGecko)" => format!("https://api.coingecko.com/api/v3/simple/price?ids={}&vs_currencies=usd,eur", pct(&(if arg.is_empty() { "bitcoin".to_owned() } else { arg.to_lowercase() }))),
        "Random joke" => "https://official-joke-api.appspot.com/random_joke".to_owned(),
        _ => format!("https://jsonplaceholder.typicode.com/todos?_limit={}", arg.parse::<u32>().unwrap_or(5).clamp(1, 50)),
    })
}

fn launch(req: ehttp::Request) -> Shared {
    let slot: Shared = Arc::new(Mutex::new(None));
    let s2 = slot.clone();
    ehttp::fetch(req, move |res| {
        let r = res.map(|r| {
            let mut bytes = r.bytes;
            bytes.truncate(MAX_HTTP_BYTES);
            HttpRes { status: r.status, ok: r.ok, text: String::from_utf8_lossy(&bytes).into_owned() }
        });
        if let Ok(mut g) = s2.lock() {
            *g = Some(r);
        }
    });
    slot
}

fn http_step(url: String, method: &str, headers: Vec<(String, String)>, body: String, proxy: bool, post: &'static str, retries: u8) -> Result<Step, String> {
    let url = url.trim().to_owned();
    // on the web an empty "Server" means "this website": /api/hooks/… is a same-origin URL
    let same_origin = cfg!(target_arch = "wasm32") && url.starts_with("/api/hooks/");
    if !(same_origin || url.starts_with("http://") || url.starts_with("https://")) {
        return Err(if url.starts_with("/api/hooks/") { "fill in the Server field (for example https://your-site.example) – only the web app can use the site it runs on".to_owned() } else { "the URL must start with http:// or https://".to_owned() });
    }
    let method = if method.is_empty() { "GET" } else { method };
    let proxied = cfg!(target_arch = "wasm32") && proxy && !url.contains("/api/hooks/");
    let req = if proxied {
        let payload = json!({"url": url, "method": method, "headers": headers.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(), "body": body});
        let mut r = ehttp::Request::post("/api/flow-proxy", payload.to_string().into_bytes());
        r.headers.insert("Content-Type", "application/json");
        r
    } else {
        let mut r = ehttp::Request::get(&url);
        r.method = method.to_owned();
        if !matches!(method, "GET" | "DELETE") || !body.is_empty() {
            r.body = body.into_bytes();
        }
        for (k, v) in &headers {
            r.headers.insert(k, v);
        }
        r
    };
    let slot = launch(req.clone());
    Ok(Step::Wait(Pending::Http { slot, proxied, post, req, left: retries }))
}

/// Post-processing of an HTTP answer for the connector nodes. `o` = [body, status, error].
fn shape(post: &str, o: Vec<Option<Json>>) -> Vec<Option<Json>> {
    let body = o.first().cloned().flatten();
    let err = o.get(2).cloned().flatten();
    match post {
        "msg" => vec![if err.is_none() { Some(body.unwrap_or(json!(true))) } else { None }, err],
        "inbox" => {
            if err.is_some() {
                return vec![None, None, None, err];
            }
            let evs: Vec<Json> = body
                .as_ref()
                .and_then(|b| b.get("events"))
                .and_then(|e| e.as_array())
                .map(|a| a.iter().map(|e| e.get("payload").cloned().unwrap_or_else(|| e.clone())).collect())
                .unwrap_or_default();
            let count = evs.len();
            vec![if count > 0 { Some(Json::Array(evs.clone())) } else { None }, evs.first().cloned(), Some(json!(count)), None]
        }
        p if p.starts_with("rss") => {
            if let Some(e) = err {
                return vec![None, None, Some(e)];
            }
            let text = body.as_ref().map(text_of).unwrap_or_default();
            let (title, items) = parse_feed(&text, if p == "rss20" { 20 } else { 200 });
            if items.is_empty() {
                return vec![None, None, Some(json!("no feed items found – is this an RSS / Atom URL?"))];
            }
            vec![Some(Json::Array(items)), Some(json!(title)), None]
        }
        _ => o,
    }
}

fn strip_tags(t: &str) -> String {
    let mut o = String::with_capacity(t.len());
    let mut in_tag = false;
    for c in t.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => o.push(c),
            _ => {}
        }
    }
    o
}

fn xml_unescape(s: &str) -> String {
    let t = s.trim();
    let t = t.strip_prefix("<![CDATA[").and_then(|x| x.strip_suffix("]]>")).unwrap_or(t);
    let t = strip_tags(t).replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&apos;", "'").replace("&nbsp;", " ").replace("&amp;", "&");
    // feeds often carry HTML as escaped text: strip that too
    strip_tags(&t).trim().to_owned()
}

fn xml_tag(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let mut from = 0;
    while let Some(i) = block[from..].find(&open) {
        let at = from + i;
        let after = &block[at + open.len()..];
        // must be exactly this tag (not <titleX>)
        if !after.starts_with(|c: char| c == '>' || c == ' ' || c == '/' || c == '\n' || c == '\r' || c == '\t') {
            from = at + open.len();
            continue;
        }
        let gt = after.find('>')?;
        if after[..gt].ends_with('/') {
            return None;
        }
        let rest = &after[gt + 1..];
        let end = rest.find(&format!("</{tag}>"))?;
        return Some(xml_unescape(&rest[..end]));
    }
    None
}

fn atom_link(block: &str) -> Option<String> {
    let mut from = 0;
    while let Some(i) = block[from..].find("<link") {
        let at = from + i;
        let end = block[at..].find('>')? + at;
        let tag = &block[at..end];
        if let Some(h) = tag.find("href=") {
            let v = &tag[h + 5..];
            let q = v.chars().next()?;
            if q == '"' || q == '\'' {
                let inner = &v[1..];
                return inner.find(q).map(|e| inner[..e].to_owned());
            }
        }
        from = end;
    }
    None
}

/// Minimal RSS 2.0 / Atom reader: (feed title, items).
pub fn parse_feed(xml: &str, limit: usize) -> (String, Vec<Json>) {
    let atom = xml.contains("<entry");
    let item_tag = if atom { "<entry" } else { "<item" };
    let head_end = xml.find(item_tag).unwrap_or(xml.len());
    let title = xml_tag(&xml[..head_end], "title").unwrap_or_default();
    let mut items = vec![];
    let mut rest = &xml[head_end..];
    while let Some(i) = rest.find(item_tag) {
        let after = &rest[i + item_tag.len()..];
        let close = if atom { "</entry>" } else { "</item>" };
        let Some(e) = after.find(close) else { break };
        let block = &after[..e];
        let link = if atom { atom_link(block) } else { xml_tag(block, "link").or_else(|| atom_link(block)) }.unwrap_or_default();
        let date = xml_tag(block, "pubDate").or_else(|| xml_tag(block, "published")).or_else(|| xml_tag(block, "updated")).or_else(|| xml_tag(block, "dc:date")).unwrap_or_default();
        let sum = xml_tag(block, "description").or_else(|| xml_tag(block, "summary")).or_else(|| xml_tag(block, "content")).unwrap_or_default();
        let mut sum: String = sum.chars().take(400).collect();
        if sum.chars().count() >= 400 {
            sum.push('…');
        }
        items.push(json!({"title": xml_tag(block, "title").unwrap_or_default(), "link": link, "date": date, "summary": sum}));
        if items.len() >= limit {
            break;
        }
        rest = &after[e + close.len()..];
    }
    (title, items)
}

pub fn parse_csv(text: &str, delim: char, header: bool) -> (Json, Json) {
    let mut rows: Vec<Vec<String>> = vec![];
    let (mut cur, mut field, mut quoted) = (vec![], String::new(), false);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' && field.is_empty() {
            quoted = true;
        } else if c == delim {
            cur.push(std::mem::take(&mut field));
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            cur.push(std::mem::take(&mut field));
            if cur.iter().any(|f| !f.is_empty()) {
                rows.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        } else {
            field.push(c);
        }
    }
    cur.push(field);
    if cur.iter().any(|f| !f.is_empty()) {
        rows.push(cur);
    }
    let conv = |s: &str| -> Json {
        match s.trim().parse::<f64>() {
            Ok(n) if !s.trim().is_empty() && n.is_finite() => num_json(n),
            _ => json!(s),
        }
    };
    let heads: Vec<String> = if header && !rows.is_empty() { rows.remove(0).into_iter().map(|h| h.trim().to_owned()).collect() } else { (0..rows.iter().map(|r| r.len()).max().unwrap_or(0)).map(|i| format!("col{}", i + 1)).collect() };
    let out: Vec<Json> = rows.iter().map(|r| Json::Object(heads.iter().enumerate().map(|(i, h)| (h.clone(), conv(r.get(i).map_or("", |s| s.as_str())))).collect::<Map<String, Json>>())).collect();
    (Json::Array(out), json!(heads))
}

/// `Name = value` lines → object (values are templates, a lone `{{x}}` keeps its type).
pub fn fields_obj(spec: &str, ctx: &Json) -> Map<String, Json> {
    let mut m = Map::new();
    for line in spec.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let k = k.trim();
        if !k.is_empty() {
            m.insert(k.to_owned(), render_value(v.trim(), ctx));
        }
    }
    m
}

fn merge_fields(base: Json, spec: &str, ctx: &Json) -> Json {
    let mut c = ctx.clone();
    if let Some(m) = c.as_object_mut() {
        m.insert("input".into(), base.clone());
        m.insert("item".into(), base.clone());
    }
    let extra = fields_obj(spec, &c);
    let mut m = match base {
        Json::Object(m) => m,
        Json::Null => Map::new(),
        other => {
            let mut m = Map::new();
            m.insert("title".into(), json!(text_of(&other)));
            m
        }
    };
    m.extend(extra);
    Json::Object(m)
}

fn http_outputs(h: HttpRes, proxied: bool) -> Result<Vec<Option<Json>>, String> {
    let (status, ok, text) = if proxied {
        let v: Json = serde_json::from_str(&h.text).map_err(|_| format!("the proxy answered with something unexpected (HTTP {})", h.status))?;
        if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
            return Ok(vec![None, None, Some(json!(e))]);
        }
        (v.get("status").and_then(|s| s.as_u64()).unwrap_or(0) as u16, v.get("ok").and_then(|s| s.as_bool()).unwrap_or(false), v.get("body").and_then(|b| b.as_str()).unwrap_or("").to_owned())
    } else {
        (h.status, h.ok, h.text)
    };
    let body = serde_json::from_str::<Json>(&text).unwrap_or(Json::String(text));
    if ok && status < 400 {
        Ok(vec![Some(body), Some(json!(status)), None])
    } else {
        Ok(vec![None, Some(json!(status)), Some(json!({"status": status, "body": body}))])
    }
}

fn run_code(env: &mut Env, code: &str, globals: Vec<(&str, Json)>, outs: &[&str]) -> Result<Vec<Option<Json>>, String> {
    let mut extra = Extra::default();
    for (k, v) in globals {
        extra.globals.insert(k.to_owned(), to_script(&v));
    }
    extra.globals.insert("vars".into(), to_script(&Json::Object(env.vars.iter().map(|(k, v)| (k.clone(), json!(v))).collect())));
    let ctx = ScriptCtx { row: None, page: None, db: 0, source: format!("Flow · {}", env.name), kind: NoticeKind::System, now: env.now };
    let mut follow = vec![];
    let rep = scripthost::run_script_with(&mut *env.ws, code, &ctx, &mut *env.out, &mut follow, Some(&mut extra));
    for l in rep.output {
        env.log.push(format!("  {l}"));
    }
    if let Some(e) = rep.error {
        return Err(e.to_string());
    }
    let mut res = vec![None; outs.len()];
    for (port, v) in &extra.emitted {
        let idx = if port.is_empty() { 0 } else { outs.iter().position(|o| o == port).ok_or_else(|| format!("emit_to: no output called “{port}” (outputs: {})", outs.join(", ")))? };
        if idx < res.len() {
            res[idx] = Some(from_script(v, 0));
        }
    }
    Ok(res)
}

fn ctx_json(d: &Def, ins: &[Option<Json>], vars: &BTreeMap<String, String>) -> Json {
    let mut m = Map::new();
    for (i, name) in d.ins.iter().enumerate() {
        m.insert((*name).to_owned(), ins.get(i).cloned().flatten().unwrap_or(Json::Null));
    }
    m.insert("input".into(), ins.iter().flatten().next().cloned().unwrap_or(Json::Null));
    m.insert("vars".into(), Json::Object(vars.iter().map(|(k, v)| (k.clone(), json!(v))).collect()));
    let (y, mo, da) = crate::dates::civil_from_days(crate::dates::now_local_min().div_euclid(1440));
    m.insert("today".into(), json!(crate::dates::iso(y, mo, da)));
    Json::Object(m)
}

fn rand01(seed: &mut u64) -> f64 {
    // xorshift64*
    let mut x = (*seed).max(1);
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *seed = x;
    (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
}

fn list_of<'a>(v: &'a Option<Json>, what: &str) -> Result<&'a Vec<Json>, String> {
    match v {
        Some(Json::Array(a)) => Ok(a),
        Some(other) => Err(format!("{what} needs a list but got {}", kind_name(other))),
        None => Err(format!("{what} needs a list but nothing is connected")),
    }
}

fn kind_name(j: &Json) -> &'static str {
    match j {
        Json::Null => "nothing",
        Json::Bool(_) => "a boolean",
        Json::Number(_) => "a number",
        Json::String(_) => "text",
        Json::Array(_) => "a list",
        Json::Object(_) => "an object",
    }
}

fn exec(n: &FNode, d: &'static Def, ins: &[Option<Json>], env: &mut Env) -> Result<Step, String> {
    let input = ins.first().cloned().flatten();
    let ctx = ctx_json(d, ins, env.vars);
    let tp = |k: &str| render(&n.param(k), &ctx);
    let num = |k: &str| -> f64 { n.param(k).trim().parse().unwrap_or(0.0) };
    let done = |v: Vec<Option<Json>>| -> Result<Step, String> { Ok(Step::Done(v)) };
    match d.key {
        "manual" => {
            let t = n.param("payload");
            done(vec![Some(serde_json::from_str(t.trim()).unwrap_or(Json::String(t)))])
        }
        "text" => done(vec![Some(Json::String(tp("value")))]),
        "number" => done(vec![Some(num_json(num("value")))]),
        "now" => {
            let lm = crate::dates::now_local_min() + (num("days") * 1440.0) as i64;
            let day = lm.div_euclid(1440);
            let min = lm.rem_euclid(1440) as u16;
            let date = crate::dates::iso_of_day(day);
            done(vec![Some(json!(format!("{date} {}", crate::dates::hhmm(min)))), Some(json!(date)), Some(json!(env.now + num("days") * 86_400_000.0))])
        }
        "random" => {
            let (lo, hi) = (num("min"), num("max"));
            let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
            let r = rand01(env.seed);
            let v = if n.param("int") == "true" { (lo.ceil() + (r * (hi.floor() - lo.ceil() + 1.0)).floor()).min(hi) } else { lo + r * (hi - lo) };
            done(vec![Some(num_json(v))])
        }
        "if" => {
            let v = input.clone().unwrap_or(Json::Null);
            let rhs = ins.get(1).cloned().flatten().unwrap_or_else(|| Json::String(tp("rhs")));
            let r = run_test(&n.param("test"), &v, &rhs);
            done(if r { vec![Some(v), None] } else { vec![None, Some(v)] })
        }
        "switch" => {
            let v = input.unwrap_or(Json::Null);
            let t = text_of(&v);
            let mut out = vec![None; 4];
            let hit = ["c1", "c2", "c3"].iter().position(|k| {
                let c = tp(k);
                !c.is_empty() && (c == t || (as_num(&json!(c)).is_some() && as_num(&json!(c)) == as_num(&v)))
            });
            out[hit.unwrap_or(3)] = Some(v);
            done(out)
        }
        "merge" => {
            let act: Vec<Json> = ins.iter().flatten().cloned().collect();
            let v = match n.param("mode").as_str() {
                "object" => {
                    let mut m = Map::new();
                    for (i, x) in ins.iter().enumerate() {
                        match x {
                            Some(Json::Object(o)) => m.extend(o.clone()),
                            Some(other) => {
                                m.insert(d.ins[i].to_owned(), other.clone());
                            }
                            None => {}
                        }
                    }
                    Json::Object(m)
                }
                "concat" => Json::Array(act.into_iter().flat_map(|x| if let Json::Array(a) = x { a } else { vec![x] }).collect()),
                "first" => act.into_iter().next().unwrap_or(Json::Null),
                _ => Json::Array(act),
            };
            done(vec![Some(v)])
        }
        "compare" => {
            let a = ins.first().cloned().flatten().unwrap_or(Json::Null);
            let b = ins.get(1).cloned().flatten().unwrap_or_else(|| Json::String(tp("rhs")));
            done(vec![Some(json!(run_test(&n.param("test"), &a, &b)))])
        }
        "logic" => {
            let a = ins.first().cloned().flatten().map_or(false, |x| truthy(&x));
            let b = ins.get(1).cloned().flatten().map_or(false, |x| truthy(&x));
            let r = match n.param("op").as_str() {
                "or" => a || b,
                "xor" => a != b,
                "not" => !a,
                _ => a && b,
            };
            done(vec![Some(json!(r))])
        }
        "delay" => {
            let ms = num("ms").clamp(0.0, MAX_DELAY_MS);
            Ok(Step::Wait(Pending::Timer { until: env.now + ms, out: vec![input] }))
        }
        "get" => {
            let v = input.unwrap_or(Json::Null);
            let found = get_path(&v, &tp("path")).cloned();
            done(match found {
                Some(x) => vec![Some(x), None],
                None => vec![None, Some(v)],
            })
        }
        "set" => {
            let mut base = match input {
                Some(Json::Object(m)) => m,
                Some(other) => {
                    let mut m = Map::new();
                    m.insert("value".into(), other);
                    m
                }
                None => Map::new(),
            };
            for line in n.param("fields").lines() {
                let Some((k, v)) = line.split_once('=') else { continue };
                let k = k.trim();
                if !k.is_empty() {
                    base.insert(k.to_owned(), render_value(v.trim(), &ctx));
                }
            }
            done(vec![Some(Json::Object(base))])
        }
        "template" => done(vec![Some(Json::String(tp("text")))]),
        "map" | "filter" => {
            let items = list_of(&input, d.label)?.clone();
            let expr = n.param("expr");
            let code = if d.key == "map" {
                format!("let __r = []\nfor __i, item in input {{\n let index = __i\n __r.push(({expr}))\n}}\nemit(__r)")
            } else {
                format!("let __r = []\nlet __x = []\nfor __i, item in input {{\n let index = __i\n if ({expr}) {{ __r.push(item) }} else {{ __x.push(item) }}\n}}\nemit(__r)\nemit_to(\"rejected\", __x)")
            };
            let r = run_code(env, &code, vec![("input", Json::Array(items)), ("with", ins.get(1).cloned().flatten().unwrap_or(Json::Null))], d.outs)?;
            done(r)
        }
        "sort" => {
            let mut items = list_of(&input, d.label)?.clone();
            let key = tp("key");
            items.sort_by(|a, b| cmp_json(get_path(a, &key).unwrap_or(&Json::Null), get_path(b, &key).unwrap_or(&Json::Null)));
            if n.param("order") == "descending" {
                items.reverse();
            }
            done(vec![Some(Json::Array(items))])
        }
        "limit" => {
            let items = list_of(&input, d.label)?;
            let k = num("n").max(0.0) as usize;
            let out: Vec<Json> = if n.param("from") == "end" { items.iter().rev().take(k).rev().cloned().collect() } else { items.iter().take(k).cloned().collect() };
            done(vec![Some(Json::Array(out))])
        }
        "aggregate" => {
            let items = list_of(&input, d.label)?;
            let field = tp("field");
            let vals: Vec<Json> = items.iter().map(|x| if field.is_empty() { x.clone() } else { get_path(x, &field).cloned().unwrap_or(Json::Null) }).collect();
            let nums: Vec<f64> = vals.iter().filter_map(as_num).collect();
            let v = match n.param("op").as_str() {
                "count" => json!(vals.len()),
                "sum" => num_json(nums.iter().sum()),
                "avg" => num_json(if nums.is_empty() { 0.0 } else { nums.iter().sum::<f64>() / nums.len() as f64 }),
                "min" => nums.iter().copied().fold(None, |a: Option<f64>, x| Some(a.map_or(x, |y| y.min(x)))).map_or(Json::Null, num_json),
                "max" => nums.iter().copied().fold(None, |a: Option<f64>, x| Some(a.map_or(x, |y| y.max(x)))).map_or(Json::Null, num_json),
                "join" => json!(vals.iter().map(text_of).collect::<Vec<_>>().join(&n.param("sep"))),
                "unique" => {
                    let mut seen = HashSet::new();
                    Json::Array(vals.into_iter().filter(|x| seen.insert(x.to_string())).collect())
                }
                "first" => vals.first().cloned().unwrap_or(Json::Null),
                "last" => vals.last().cloned().unwrap_or(Json::Null),
                "flatten" => Json::Array(vals.into_iter().flat_map(|x| if let Json::Array(a) = x { a } else { vec![x] }).collect()),
                _ => Json::Null,
            };
            done(vec![Some(v)])
        }
        "math" => {
            let a = ins.first().cloned().flatten().and_then(|x| as_num(&x)).ok_or("math: input a is not a number")?;
            let b = match ins.get(1).cloned().flatten() {
                Some(x) => as_num(&x).ok_or("math: input b is not a number")?,
                None => num("b"),
            };
            let r = match n.param("op").as_str() {
                "subtract" => a - b,
                "multiply" => a * b,
                "divide" => {
                    if b == 0.0 {
                        return Err("division by zero".into());
                    }
                    a / b
                }
                "modulo" => {
                    if b == 0.0 {
                        return Err("modulo by zero".into());
                    }
                    a % b
                }
                "power" => a.powf(b),
                "min" => a.min(b),
                "max" => a.max(b),
                "round" => {
                    let f = 10f64.powi(b.clamp(0.0, 12.0) as i32);
                    (a * f).round() / f
                }
                _ => a + b,
            };
            done(vec![Some(num_json(r))])
        }
        "split" => {
            let t = text_of(&input.unwrap_or(Json::Null));
            let sep = n.param("sep");
            let parts: Vec<Json> = if sep.is_empty() { t.lines().map(|s| json!(s.trim())).collect() } else { t.split(sep.as_str()).map(|s| json!(s.trim())).collect() };
            done(vec![Some(Json::Array(parts.into_iter().filter(|p| p != "").collect()))])
        }
        "json_parse" => {
            let t = text_of(&input.unwrap_or(Json::Null));
            done(match serde_json::from_str::<Json>(&t) {
                Ok(v) => vec![Some(v), None],
                Err(e) => vec![None, Some(json!(e.to_string()))],
            })
        }
        "json_text" => {
            let v = input.unwrap_or(Json::Null);
            let s = if n.param("pretty") == "true" { serde_json::to_string_pretty(&v) } else { serde_json::to_string(&v) };
            done(vec![Some(json!(s.unwrap_or_default()))])
        }
        "hash" => {
            let t = text_of(&input.unwrap_or(Json::Null));
            let h = Sha256::digest(t.as_bytes());
            done(vec![Some(json!(h.iter().map(|b| format!("{b:02x}")).collect::<String>()))])
        }
        "code" => {
            let g = vec![("a", ins.first().cloned().flatten().unwrap_or(Json::Null)), ("b", ins.get(1).cloned().flatten().unwrap_or(Json::Null)), ("c", ins.get(2).cloned().flatten().unwrap_or(Json::Null))];
            done(run_code(env, &n.param("code"), g, d.outs)?)
        }
        "http" => {
            let url = match ins.first().cloned().flatten() {
                Some(u) => text_of(&u),
                None => tp("url"),
            };
            let mut url = url.trim().to_owned();
            let body = match ins.get(1).cloned().flatten() {
                Some(Json::String(s)) => s,
                Some(other) => other.to_string(),
                None => tp("body"),
            };
            let mut headers: Vec<(String, String)> = n
                .param("headers")
                .lines()
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_owned(), render(v.trim(), &ctx)))
                .filter(|(k, _)| !k.is_empty())
                .collect();
            let query: Vec<(String, String)> = n.param("query").lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim().to_owned(), render(v.trim(), &ctx))).filter(|(k, _)| !k.is_empty()).collect();
            add_query(&mut url, &query);
            let token = tp("token");
            if !token.is_empty() {
                match n.param("auth").as_str() {
                    "bearer token" => headers.push(("Authorization".into(), format!("Bearer {token}"))),
                    "basic (user:password)" => headers.push(("Authorization".into(), format!("Basic {}", b64(token.as_bytes())))),
                    "x-api-key header" => headers.push(("X-API-Key".into(), token)),
                    "api_key query" => add_query(&mut url, &[("api_key".to_owned(), token)]),
                    _ => {}
                }
            }
            let json_body = body.trim_start().starts_with('{') || body.trim_start().starts_with('[');
            if json_body && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
                headers.push(("Content-Type".into(), "application/json".into()));
            }
            let retries = n.param("retries").trim().parse::<u8>().unwrap_or(0).min(3);
            http_step(url, &n.param("method"), headers, body, n.param("proxy") != "false", "", retries)
        }
        "api" => {
            let arg = match ins.first().cloned().flatten() {
                Some(a) => text_of(&a),
                None => tp("arg"),
            };
            let url = preset_url(&n.param("service"), arg.trim())?;
            http_step(url, "GET", vec![("Accept".into(), "application/json".into())], String::new(), true, "", 1)
        }
        "rss" => {
            let url = match ins.first().cloned().flatten() {
                Some(u) => text_of(&u),
                None => tp("url"),
            };
            let limit = num("limit").clamp(1.0, 200.0) as usize;
            http_step(url.trim().to_owned(), "GET", vec![("Accept".into(), "application/rss+xml, application/atom+xml, application/xml, text/xml, */*".into())], String::new(), true, if limit <= 20 { "rss20" } else { "rss" }, 1)
        }
        "message" => {
            let msg = match ins.first().cloned().flatten() {
                Some(v) => text_of(&v),
                None => tp("text"),
            };
            if msg.trim().is_empty() {
                return Err("there is nothing to send – connect a message or fill the Message field".into());
            }
            let target = tp("target");
            let target = target.trim();
            if target.is_empty() {
                return Err("fill in the webhook URL / topic / bot token".into());
            }
            let extra = tp("chat");
            let json_h = vec![("Content-Type".to_owned(), "application/json".to_owned())];
            match n.param("service").as_str() {
                "Slack webhook" => http_step(target.to_owned(), "POST", json_h, json!({"text": msg}).to_string(), true, "msg", 1),
                "Discord webhook" => http_step(target.to_owned(), "POST", json_h, json!({"content": msg}).to_string(), true, "msg", 1),
                "Microsoft Teams webhook" => http_step(target.to_owned(), "POST", json_h, json!({"text": msg}).to_string(), true, "msg", 1),
                "Telegram bot" => {
                    if extra.trim().is_empty() {
                        return Err("Telegram needs the chat id".into());
                    }
                    http_step(format!("https://api.telegram.org/bot{target}/sendMessage"), "POST", json_h, json!({"chat_id": extra.trim(), "text": msg}).to_string(), true, "msg", 1)
                }
                "Generic JSON webhook" => http_step(target.to_owned(), "POST", json_h, json!({"title": extra, "text": msg}).to_string(), true, "msg", 1),
                _ => {
                    let mut h = vec![("Content-Type".to_owned(), "text/plain; charset=utf-8".to_owned())];
                    if !extra.trim().is_empty() {
                        h.push(("Title".into(), extra.trim().to_owned()));
                    }
                    http_step(format!("https://ntfy.sh/{}", pct(target)), "POST", h, msg, true, "msg", 1)
                }
            }
        }
        "cloud_in" => {
            let ch = tp("channel");
            if ch.trim().is_empty() {
                return Err("choose a channel name".into());
            }
            let limit = num("limit").clamp(1.0, 200.0) as u32;
            let url = format!("{}/api/hooks/{}?limit={limit}&consume={}", server_of(&tp("server")), pct(ch.trim()), if n.param("consume") == "false" { 0 } else { 1 });
            let key = tp("key");
            let mut h = vec![("Accept".to_owned(), "application/json".to_owned())];
            if !key.trim().is_empty() {
                h.push(("x-hook-key".into(), key.trim().to_owned()));
            }
            http_step(url, "GET", h, String::new(), false, "inbox", 1)
        }
        "cloud_out" => {
            let ch = tp("channel");
            if ch.trim().is_empty() {
                return Err("choose a channel name".into());
            }
            let url = format!("{}/api/hooks/{}", server_of(&tp("server")), pct(ch.trim()));
            let key = tp("key");
            let mut h = vec![("Content-Type".to_owned(), "application/json".to_owned())];
            if !key.trim().is_empty() {
                h.push(("x-hook-key".into(), key.trim().to_owned()));
            }
            let body = input.clone().unwrap_or(Json::Null).to_string();
            http_step(url, "POST", h, body, false, "msg", 1)
        }
        "csv" => {
            let t = text_of(&input.unwrap_or(Json::Null));
            let delim = match n.param("delim").as_str() {
                "semicolon" => ';',
                "tab" => '\t',
                _ => ',',
            };
            let (rows, heads) = parse_csv(&t, delim, n.param("header") != "false");
            done(vec![Some(rows), Some(heads)])
        }
        "db_query" => {
            let limit = num("limit").clamp(1.0, 2000.0);
            let code = "let t = db(db_name)\nlet out = []\nfor r in t.rows() {\n let m = r.props()\n m.set(\"title\", r.title)\n if prop_name != \"\" && str(m.get(prop_name, \"\")) != match_value { continue }\n out.push(m)\n if len(out) >= max_rows { break }\n}\nemit(out)\nemit_to(\"count\", len(out))\nemit_to(\"first\", len(out) > 0 ? out[0] : null)";
            done(run_code(env, code, vec![("db_name", json!(tp("db"))), ("prop_name", json!(tp("prop"))), ("match_value", json!(tp("value"))), ("max_rows", json!(limit))], d.outs)?)
        }
        "db_add" => {
            let mut data = input.unwrap_or(Json::Null);
            let spec = n.param("fields");
            if !spec.trim().is_empty() {
                data = match data {
                    Json::Array(a) => Json::Array(a.into_iter().map(|x| merge_fields(x, &spec, &ctx)).collect()),
                    other => merge_fields(other, &spec, &ctx),
                };
            }
            if matches!(&data, Json::Null) {
                return Err("nothing to add – connect data or set at least one field".into());
            }
            let code = "let t = db(db_name)\nlet made = []\nlet items = type(data) == \"list\" ? data : [data]\nfor it in items { made.push(t.add(type(it) == \"map\" ? it : {title: str(it)}).props()) }\nemit(type(data) == \"list\" ? made : made[0])";
            done(run_code(env, code, vec![("db_name", json!(tp("db"))), ("data", data)], d.outs)?)
        }
        "db_update" => {
            let mut data = input.unwrap_or(Json::Null);
            let spec = n.param("fields");
            if !spec.trim().is_empty() {
                data = merge_fields(if data.is_object() { data } else { Json::Null }, &spec, &ctx);
            }
            if !data.is_object() {
                return Err("Update row needs an object (property → value)".into());
            }
            let code = "let r = db(db_name).find(prop_name, match_value)\nif r == null { emit_to(\"missing\", data) } else {\n r.update(data)\n emit(r.props())\n}";
            done(run_code(env, code, vec![("db_name", json!(tp("db"))), ("prop_name", json!(tp("prop"))), ("match_value", json!(tp("value"))), ("data", data)], d.outs)?)
        }
        "create_page" => {
            let body = match input {
                Some(Json::String(s)) => s,
                Some(Json::Null) | None => String::new(),
                Some(other) => serde_json::to_string_pretty(&other).unwrap_or_default(),
            };
            done(run_code(env, "create_page(title, body)\nemit(title)", vec![("title", json!(tp("title"))), ("body", json!(body))], d.outs)?)
        }
        "notify" => {
            let msg = match input {
                Some(v) => text_of(&v),
                None => tp("text"),
            };
            done(run_code(env, "notify(msg)\nemit(msg)", vec![("msg", json!(msg))], d.outs)?)
        }
        "copy" => {
            *env.copy = Some(match input {
                Some(Json::String(s)) => s,
                Some(other) => serde_json::to_string_pretty(&other).unwrap_or_default(),
                None => String::new(),
            });
            done(vec![])
        }
        "log" => {
            let v = input.unwrap_or(Json::Null);
            let label = n.param("label");
            env.log.push(format!("• {}{}", if label.is_empty() { String::new() } else { format!("{label}: ") }, preview(&v, 220)));
            done(vec![Some(v)])
        }
        // the value is kept (index 0 beyond the declared outputs) so the run report can list it
        "output" => done(vec![input]),
        _ => Err("this node type has no implementation".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(f: &Flow, ws: &mut Workspace) -> Run {
        let mut r = Run::start(f, "test", None, false, ws, 1000.0);
        for i in 0..2000 {
            r.step(ws, 1000.0 + i as f64 * 20.0);
            if r.finished {
                break;
            }
        }
        assert!(r.finished, "flow did not finish");
        r
    }

    #[test]
    fn paths_and_templates() {
        let v = json!({"a": {"b": [10, {"c": "x"}]}, "n": 5});
        assert_eq!(get_path(&v, "a.b[1].c"), Some(&json!("x")));
        assert_eq!(get_path(&v, "a.b[-2]"), Some(&json!(10)));
        assert_eq!(get_path(&v, "a.z"), None);
        assert_eq!(render("n={{ n }} c={{a.b[1].c}} {{nope}}!", &v), "n=5 c=x !");
        assert_eq!(render_value("{{n}}", &v), json!(5));
        assert_eq!(render_value("v{{n}}", &v), json!("v5"));
    }

    #[test]
    fn data_flows_through_map_filter_aggregate() {
        let mut ws = Workspace::seed();
        let mut f = Flow::default();
        let m = node(&mut f, "manual", 0.0, 0.0, &[("payload", "[{\"p\": 5}, {\"p\": 15}, {\"p\": 25}]")]);
        let fl = node(&mut f, "filter", 0.0, 0.0, &[("expr", "item.p > 10")]);
        let mp = node(&mut f, "map", 0.0, 0.0, &[("expr", "item.p * 2")]);
        let ag = node(&mut f, "aggregate", 0.0, 0.0, &[("op", "sum")]);
        let rej = node(&mut f, "aggregate", 0.0, 0.0, &[("op", "count")]);
        f.connect(m, 0, fl, 0).unwrap();
        f.connect(fl, 0, mp, 0).unwrap();
        f.connect(mp, 0, ag, 0).unwrap();
        f.connect(fl, 1, rej, 0).unwrap();
        let r = run(&f, &mut ws);
        assert_eq!(r.errors(), 0, "{:?}", r.log);
        assert_eq!(r.values[&ag][0], Some(json!(80)));
        assert_eq!(r.values[&rej][0], Some(json!(1)));
    }

    #[test]
    fn branches_skip_and_merge_rejoins() {
        let mut ws = Workspace::seed();
        let mut f = Flow::default();
        let n = node(&mut f, "number", 0.0, 0.0, &[("value", "7")]);
        let i = node(&mut f, "if", 0.0, 0.0, &[("test", "greater than"), ("rhs", "10")]);
        let hi = node(&mut f, "template", 0.0, 0.0, &[("text", "high")]);
        let lo = node(&mut f, "template", 0.0, 0.0, &[("text", "low {{a}}")]);
        let mg = node(&mut f, "merge", 0.0, 0.0, &[("mode", "first")]);
        f.connect(n, 0, i, 0).unwrap();
        f.connect(i, 0, hi, 0).unwrap();
        f.connect(i, 1, lo, 0).unwrap();
        f.connect(hi, 0, mg, 0).unwrap();
        f.connect(lo, 0, mg, 1).unwrap();
        let r = run(&f, &mut ws);
        assert_eq!(r.status[&hi], St::Skipped);
        assert_eq!(r.values[&mg][0], Some(json!("low 7")));
    }

    #[test]
    fn fan_in_collects_a_list_and_loops_are_refused() {
        let mut ws = Workspace::seed();
        let mut f = Flow::default();
        let a = node(&mut f, "number", 0.0, 0.0, &[("value", "1")]);
        let b = node(&mut f, "number", 0.0, 0.0, &[("value", "2")]);
        let s = node(&mut f, "aggregate", 0.0, 0.0, &[("op", "sum")]);
        f.connect(a, 0, s, 0).unwrap();
        f.connect(b, 0, s, 0).unwrap();
        let r = run(&f, &mut ws);
        assert_eq!(r.values[&s][0], Some(json!(3)));
        let x = node(&mut f, "math", 0.0, 0.0, &[]);
        let y = node(&mut f, "math", 0.0, 0.0, &[]);
        f.connect(x, 0, y, 0).unwrap();
        assert!(f.connect(y, 0, x, 0).is_err(), "cycle must be refused");
    }

    #[test]
    fn errors_block_downstream_only() {
        let mut ws = Workspace::seed();
        let mut f = Flow::default();
        let a = node(&mut f, "number", 0.0, 0.0, &[("value", "1")]);
        let bad = node(&mut f, "math", 0.0, 0.0, &[("op", "divide"), ("b", "0")]);
        let after = node(&mut f, "log", 0.0, 0.0, &[]);
        let side = node(&mut f, "log", 0.0, 0.0, &[]);
        f.connect(a, 0, bad, 0).unwrap();
        f.connect(bad, 0, after, 0).unwrap();
        f.connect(a, 0, side, 0).unwrap();
        let r = run(&f, &mut ws);
        assert!(matches!(r.status[&bad], St::Error(_)));
        assert_eq!(r.status[&after], St::Blocked);
        assert!(matches!(r.status[&side], St::Done(_)));
    }

    #[test]
    fn code_node_has_multiple_outputs_and_workspace_access() {
        let mut ws = Workspace::seed();
        let db = ws.add_database(None, crate::model::ViewKind::Table);
        ws.page_mut(db).unwrap().title = "Tasks".into();
        let before = ws.rows(db).len();
        let mut f = Flow::default();
        let m = node(&mut f, "manual", 0.0, 0.0, &[("payload", "{\"title\": \"From flow\"}")]);
        let c = node(&mut f, "code", 0.0, 0.0, &[("code", "emit(a.title.upper())\nemit_to(\"out2\", [1, 2, 3])")]);
        let add = node(&mut f, "db_add", 0.0, 0.0, &[("db", "Tasks")]);
        f.connect(m, 0, c, 0).unwrap();
        f.connect(m, 0, add, 0).unwrap();
        let r = run(&f, &mut ws);
        assert_eq!(r.errors(), 0, "{:?}", r.log);
        assert_eq!(r.values[&c][0], Some(json!("FROM FLOW")));
        assert_eq!(r.values[&c][1], Some(json!([1, 2, 3])));
        assert_eq!(ws.rows(db).len(), before + 1);
        // dry runs never touch the workspace
        let mut dry = Run::start(&f, "t", None, true, &ws, 0.0);
        for i in 0..100 {
            dry.step(&mut ws, i as f64);
        }
        assert_eq!(dry.errors(), 0, "{:?}", dry.log);
        assert_eq!(ws.rows(db).len(), before + 1);
    }

    #[test]
    fn query_database_returns_rows() {
        let mut ws = Workspace::seed();
        let db = ws.add_database(None, crate::model::ViewKind::Table);
        ws.page_mut(db).unwrap().title = "Tasks".into();
        let mut f = Flow::default();
        let m = node(&mut f, "manual", 0.0, 0.0, &[("payload", "[{\"title\": \"a\"}, {\"title\": \"b\"}]")]);
        let add = node(&mut f, "db_add", 0.0, 0.0, &[("db", "Tasks")]);
        f.connect(m, 0, add, 0).unwrap();
        let r0 = run(&f, &mut ws);
        assert_eq!(r0.errors(), 0, "{:?}", r0.log);
        let mut g = Flow::default();
        let q = node(&mut g, "db_query", 0.0, 0.0, &[("db", "tasks")]);
        let r = run(&g, &mut ws);
        assert_eq!(r.errors(), 0, "{:?}", r.log);
        let rows = r.values[&q][0].clone().unwrap();
        assert!(rows.as_array().unwrap().iter().any(|x| x["title"] == "b"), "{rows}");
        assert!(r.values[&q][1].clone().unwrap().as_i64().unwrap() >= 2);
    }

    #[test]
    fn connectors_parse_and_encode() {
        assert_eq!(pct("a b/ü"), "a%20b%2F%C3%BC");
        assert_eq!(b64(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(b64(b"ab"), "YWI=");
        let mut u = "https://x.test/p".to_owned();
        add_query(&mut u, &[("q".into(), "a b".into()), ("n".into(), "1".into())]);
        assert_eq!(u, "https://x.test/p?q=a%20b&n=1");
        assert_eq!(server_of("example.com/"), "https://example.com");
        assert_eq!(server_of(""), "");
        let (rows, heads) = parse_csv("name;n\n\"a;b\";2\nc;3\n", ';', true);
        assert_eq!(heads, json!(["name", "n"]));
        assert_eq!(rows, json!([{"name": "a;b", "n": 2}, {"name": "c", "n": 3}]));
        let rss = "<rss><channel><title>Feed</title><item><title><![CDATA[One &amp; two]]></title><link>https://a.test/1</link><description>&lt;p&gt;hi&lt;/p&gt;</description></item><item><title>Two</title><link>https://a.test/2</link></item></channel></rss>";
        let (t, items) = parse_feed(rss, 10);
        assert_eq!(t, "Feed");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["title"], "One & two");
        assert_eq!(items[0]["summary"], "hi");
        let atom = "<feed><title>A</title><entry><title>E1</title><link href=\"https://e.test/1\"/><updated>2025-01-01</updated></entry></feed>";
        let (_, it) = parse_feed(atom, 10);
        assert_eq!(it[0]["link"], "https://e.test/1");
        assert!(preset_url("Weather (Open-Meteo)", "oops").is_err());
        assert!(preset_url("GitHub repository", "").is_err());
        assert!(preset_url("Exchange rates", "usd").unwrap().ends_with("/USD"));
    }

    #[test]
    fn inbox_and_message_outputs_are_shaped() {
        let body = json!({"events": [{"id": 1, "payload": {"title": "x"}}, {"id": 2, "payload": "y"}]});
        let o = shape("inbox", vec![Some(body), Some(json!(200)), None]);
        assert_eq!(o[0], Some(json!([{"title": "x"}, "y"])));
        assert_eq!(o[1], Some(json!({"title": "x"})));
        assert_eq!(o[2], Some(json!(2)));
        let empty = shape("inbox", vec![Some(json!({"events": []})), Some(json!(200)), None]);
        assert!(empty[0].is_none() && empty[1].is_none());
        assert_eq!(empty[2], Some(json!(0)));
        let bad = shape("msg", vec![None, Some(json!(500)), Some(json!("boom"))]);
        assert!(bad[0].is_none() && bad[1].is_some());
    }

    #[test]
    fn add_row_merges_fields_over_the_input() {
        let mut ws = Workspace::seed();
        let db = ws.add_database(None, crate::model::ViewKind::Table);
        ws.page_mut(db).unwrap().title = "Tasks".into();
        let mut f = Flow::default();
        let m = node(&mut f, "manual", 0.0, 0.0, &[("payload", "[{\"title\": \"a\"}, {\"title\": \"b\"}]")]);
        let add = node(&mut f, "db_add", 0.0, 0.0, &[("db", "Tasks"), ("fields", "title = Task: {{input.title}}")]);
        f.connect(m, 0, add, 0).unwrap();
        let r = run(&f, &mut ws);
        assert_eq!(r.errors(), 0, "{:?}", r.log);
        let mut g = Flow::default();
        let q = node(&mut g, "db_query", 0.0, 0.0, &[("db", "Tasks")]);
        let r = run(&g, &mut ws);
        let rows = r.values[&q][0].clone().unwrap();
        assert!(rows.as_array().unwrap().iter().any(|x| x["title"] == "Task: b"), "{rows}");
    }

    #[test]
    fn db_nodes_accept_dropdown_kinds() {
        for d in DEFS {
            for pp in d.params {
                if matches!(pp.kind, PK::Db | PK::Prop | PK::Fields) {
                    assert!(d.cat == Cat::Workspace, "{} uses a database dropdown outside the workspace category", d.key);
                }
            }
        }
        assert!(def("api").is_some() && def("message").is_some() && def("cloud_in").is_some() && def("rss").is_some() && def("csv").is_some());
    }

    #[test]
    fn export_import_and_templates_are_valid() {
        for t in TEMPLATES {
            let f = (t.build)();
            assert!(f.problems().is_empty(), "{}: {:?}", t.name, f.problems());
            assert!(!f.wires.is_empty());
            let back = Flow::from_json(&f.to_json()).expect("round trip");
            assert_eq!(back.nodes.len(), f.nodes.len());
            assert_eq!(back.wires.len(), f.wires.len());
        }
        assert!(Flow::from_json("hello").is_none());
        assert!(Flow::from_json("{\"a\":1}").is_none());
    }

    #[test]
    fn timers_wait_without_blocking() {
        let mut ws = Workspace::seed();
        let mut f = Flow::default();
        let a = node(&mut f, "number", 0.0, 0.0, &[("value", "9")]);
        let d = node(&mut f, "delay", 0.0, 0.0, &[("ms", "500")]);
        f.connect(a, 0, d, 0).unwrap();
        let mut r = Run::start(&f, "t", None, false, &ws, 0.0);
        r.step(&mut ws, 0.0);
        assert!(!r.finished && r.waiting());
        r.step(&mut ws, 600.0);
        assert!(r.finished);
        assert_eq!(r.values[&d][0], Some(json!(9)));
    }

    #[test]
    fn repair_drops_dangling_and_cyclic_wires() {
        let mut f = Flow::default();
        let a = node(&mut f, "math", 0.0, 0.0, &[]);
        let b = node(&mut f, "math", 0.0, 0.0, &[]);
        f.wires.push(Wire { from: a, fp: 0, to: b, tp: 0 });
        f.wires.push(Wire { from: b, fp: 0, to: a, tp: 0 });
        f.wires.push(Wire { from: a, fp: 9, to: b, tp: 0 });
        f.wires.push(Wire { from: 77, fp: 0, to: b, tp: 0 });
        f.repair();
        assert_eq!(f.wires.len(), 1);
    }
}

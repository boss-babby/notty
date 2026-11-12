//! The agent tool layer — **one registry** shared by the in-app AI assistant (`ai.rs`) and the MCP server (`mcp.rs`).
//!
//! Everything here works on a plain `&mut Workspace` (no UI, no egui state), so it also runs headless (`lumen-mcp`).
//! To teach the agent a new feature: `register_tool(..)` + `register_knowledge(..)` (see `ai-knowledge/extending.md`).

use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value as Json};

use crate::auth::now_ms;
use crate::dates;
use crate::model::*;

pub const AGENT_API_VERSION: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Risk {
    /// Never changes anything.
    Read,
    /// Adds or edits content.
    Write,
    /// Deletes, overwrites or runs user code.
    Destroy,
}

/// How much an agent may do.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum Access {
    ReadOnly,
    #[default]
    Edit,
    Full,
}

impl Access {
    pub fn allows(self, r: Risk) -> bool {
        match self {
            Access::ReadOnly => r == Risk::Read,
            Access::Edit => r != Risk::Destroy,
            Access::Full => true,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Access::ReadOnly => "Read only",
            Access::Edit => "Create & edit",
            Access::Full => "Full access",
        }
    }
    pub fn hint(self) -> &'static str {
        match self {
            Access::ReadOnly => "Can look at everything, can't change anything.",
            Access::Edit => "Can create and edit pages, databases, dashboards … but never delete or run scripts.",
            Access::Full => "Can do everything, including deleting, replacing and running scripts and flows.",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Access::ReadOnly => "read",
            Access::Edit => "edit",
            Access::Full => "full",
        }
    }
    pub fn from_key(s: &str) -> Access {
        match s {
            "read" => Access::ReadOnly,
            "full" => Access::Full,
            _ => Access::Edit,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub risk: Risk,
    /// JSON schema of the arguments.
    pub schema: Json,
}

impl ToolDef {
    pub fn new(name: &str, description: &str, risk: Risk, schema: Json) -> ToolDef {
        ToolDef { name: name.into(), description: description.into(), risk, schema }
    }
}

pub type Handler = fn(&mut Workspace, &Json) -> Result<Json, String>;

/// Result of one tool call.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub ok: bool,
    /// What the model / the external agent sees (JSON text or an error message).
    pub text: String,
    /// The workspace was modified.
    pub changed: bool,
    /// Page the UI should offer to open.
    pub open: Option<u64>,
}

// ───────────────────────────── registry ─────────────────────────────

static EXTRA_TOOLS: Mutex<Vec<(ToolDef, Handler)>> = Mutex::new(Vec::new());
static BUILTIN: OnceLock<Vec<(ToolDef, Handler)>> = OnceLock::new();

/// Adds (or replaces) a tool. Call once at start-up from the feature that owns it.
#[allow(dead_code)]
pub fn register_tool(def: ToolDef, handler: Handler) {
    if let Ok(mut g) = EXTRA_TOOLS.lock() {
        g.retain(|(d, _)| d.name != def.name);
        g.push((def, handler));
    }
}

fn builtin() -> &'static Vec<(ToolDef, Handler)> {
    BUILTIN.get_or_init(|| {
        let mut v = core_tools();
        v.extend(crate::agent_build::tools());
        v
    })
}

fn all_tools() -> Vec<(ToolDef, Handler)> {
    let mut v: Vec<(ToolDef, Handler)> = builtin().clone();
    if let Ok(g) = EXTRA_TOOLS.lock() {
        for (d, h) in g.iter() {
            v.retain(|(x, _)| x.name != d.name);
            v.push((d.clone(), *h));
        }
    }
    v
}

pub fn tool_defs() -> Vec<ToolDef> {
    all_tools().into_iter().map(|(d, _)| d).collect()
}

pub fn tool_def(name: &str) -> Option<ToolDef> {
    all_tools().into_iter().find(|(d, _)| d.name == name).map(|(d, _)| d)
}

/// Runs a tool. The caller decides *whether* it may run (`access`); this function enforces it as well.
pub fn call(ws: &mut Workspace, name: &str, args: &Json, access: Access) -> Outcome {
    let Some((def, handler)) = all_tools().into_iter().find(|(d, _)| d.name == name) else {
        return Outcome { ok: false, text: format!("Unknown tool “{name}”. Call tools/list (or look at the tool list you were given)."), ..Default::default() };
    };
    if !access.allows(def.risk) {
        return Outcome {
            ok: false,
            text: format!("Not allowed: “{name}” is a {} action and the current access level is “{}”. Ask the user to raise it in Settings → AI → Access.", if def.risk == Risk::Destroy { "destructive" } else { "write" }, access.label()),
            ..Default::default()
        };
    }
    let before = if def.risk == Risk::Read { None } else { Some((ws.current, ws.pages.len())) };
    let empty = json!({});
    let args = if args.is_null() { &empty } else { args };
    let args = &normalize_args(args);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(ws, args)));
    let mut out = match r {
        Ok(Ok(v)) => {
            let open = v.get("id").and_then(|i| i.as_u64()).filter(|id| ws.page(*id).is_some() && def.risk != Risk::Read && name.starts_with("create_"));
            Outcome { ok: true, text: truncate(&v.to_string(), 24_000), changed: def.risk != Risk::Read, open }
        }
        Ok(Err(e)) => Outcome { ok: false, text: e, ..Default::default() },
        Err(_) => Outcome { ok: false, text: format!("The tool “{name}” crashed on these arguments — check the argument types and try again."), ..Default::default() },
    };
    if let Some((cur, _)) = before {
        // creating pages must never yank the user away from the page they are looking at
        if name != "open_page" {
            ws.current = cur;
        }
        if out.changed {
            let _ = crate::formula::recompute(ws);
        }
    }
    if !out.ok {
        out.changed = false;
    }
    out
}

/// Models sometimes send objects / arrays as JSON *strings* (Gemini schemas cannot express free-form objects) — decode them.
fn normalize_args(a: &Json) -> Json {
    fn fix(v: &Json) -> Json {
        match v {
            Json::String(s) => {
                let t = s.trim();
                if (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']')) {
                    if let Ok(p) = serde_json::from_str::<Json>(t) {
                        return fix(&p);
                    }
                }
                v.clone()
            }
            Json::Array(a) => Json::Array(a.iter().map(fix).collect()),
            Json::Object(m) => Json::Object(m.iter().map(|(k, x)| (k.clone(), fix(x))).collect()),
            o => o.clone(),
        }
    }
    fix(a)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated, {} more bytes — narrow the request]", &s[..end], s.len() - end)
}

// ───────────────────────────── knowledge ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Knowledge {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub body: String,
}

static EXTRA_KNOW: Mutex<Vec<Knowledge>> = Mutex::new(Vec::new());

/// Adds a knowledge module the agent can read with `get_knowledge`.
#[allow(dead_code)]
pub fn register_knowledge(id: &str, title: &str, summary: &str, body: &'static str) {
    if let Ok(mut g) = EXTRA_KNOW.lock() {
        g.retain(|k| k.id != id);
        g.push(Knowledge { id: id.into(), title: title.into(), summary: summary.into(), body: body.into() });
    }
}

pub fn knowledge() -> Vec<Knowledge> {
    let k = |id: &str, title: &str, summary: &str, body: &str| Knowledge { id: id.into(), title: title.into(), summary: summary.into(), body: body.into() };
    let mut v = vec![
        k("overview", "Start here", "Object model, working loop, safety rules", include_str!("../ai-knowledge/00-overview.md")),
        k("pages", "Pages & blocks", "Markdown dialect, block types, page tools", include_str!("../ai-knowledge/pages.md")),
        k("databases", "Databases", "Property types, values, views, filters, rows", include_str!("../ai-knowledge/databases.md")),
        k("dashboards", "Dashboards", "Widget kinds and the widget spec, layout recipes", include_str!("../ai-knowledge/dashboards.md")),
        k("canvas", "Canvas", "Whiteboard nodes, edges and layout", include_str!("../ai-knowledge/canvas.md")),
        k("flows", "Flows", "Node pipelines, ports, wires and patterns", include_str!("../ai-knowledge/flows.md")),
        k("automations", "Automations", "Rules, triggers, actions, reminders", include_str!("../ai-knowledge/automations.md")),
        k("scripting", "LumenScript", "The scripting language and complete host API", &scripting_doc()),
        k("architect", "System architecture", "Blueprints for CRMs, trackers, wikis, OKRs …", include_str!("../ai-knowledge/architect.md")),
        k("extending", "Extending the AI", "How new features register tools and knowledge", include_str!("../ai-knowledge/extending.md")),
    ];
    if let Ok(g) = EXTRA_KNOW.lock() {
        for e in g.iter() {
            v.retain(|x| x.id != e.id);
            v.push(e.clone());
        }
    }
    v
}

fn scripting_doc() -> String {
    format!(
        "# LumenScript\nA small sandboxed language. Generate code, then `script_check` (syntax), `script_test` (dry run on a copy: lists what it would change), and only then `script_run`.\nLimits: 400k steps, 200 rows created, 3000 changes, 25 notifications per run. No files / network / imports.\n\n{}",
        crate::scripthost::api_markdown()
    )
}

/// Ready-made prompts (MCP `prompts/*`, and the quick actions of the in-app assistant).
pub const PROMPTS: &[(&str, &str, &str)] = &[
    ("build_system", "Build a complete system", "Design and build a complete workspace system for: {goal}. Follow the `architect` knowledge module: hub page, databases with relations and example rows, views, an overview dashboard, automations and a short playbook. Tell me the plan first, then build it, verify it and summarise."),
    ("design_dashboard", "Design a dashboard", "Look at my databases and create a dashboard page for: {goal}. Use KPIs, charts and a table, 12-column layout, readable titles."),
    ("weekly_review", "Weekly review", "Review my workspace: open tasks, overdue items, recently edited pages. Create a 'Weekly review' page with what happened, what is at risk and the three most important next actions."),
    ("clean_up", "Tidy the workspace", "Find duplicated, empty or untitled pages and propose (do not execute) a cleanup plan as a to-do list on a new page."),
    ("automate", "Automate something", "Create the automation needed for: {goal}. Prefer rules; use a script only when rules cannot do it. Test scripts with a dry run first."),
];

// ───────────────────────────── helpers ─────────────────────────────

pub fn s(desc: &str) -> Json {
    json!({"type": "string", "description": desc})
}
pub fn n(desc: &str) -> Json {
    json!({"type": "number", "description": desc})
}
pub fn b(desc: &str) -> Json {
    json!({"type": "boolean", "description": desc})
}
pub fn any(desc: &str) -> Json {
    json!({"description": desc})
}
pub fn arr(items: Json, desc: &str) -> Json {
    json!({"type": "array", "items": items, "description": desc})
}
pub fn obj(props: Json, required: &[&str]) -> Json {
    json!({"type": "object", "properties": props, "required": required})
}
/// A page reference: id or title.
pub fn page_ref(desc: &str) -> Json {
    json!({"type": ["integer", "string"], "description": format!("{desc} (page id, or its exact title)")})
}

pub fn str_of<'a>(a: &'a Json, k: &str) -> Option<&'a str> {
    a.get(k).and_then(|v| v.as_str()).map(|s| s.trim()).filter(|s| !s.is_empty())
}
pub fn bool_of(a: &Json, k: &str) -> Option<bool> {
    a.get(k).and_then(|v| v.as_bool())
}
pub fn arr_of<'a>(a: &'a Json, k: &str) -> Vec<&'a Json> {
    a.get(k).and_then(|v| v.as_array()).map(|v| v.iter().collect()).unwrap_or_default()
}

fn id_of(v: &Json) -> Option<u64> {
    match v {
        Json::Number(n) => n.as_u64().or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)),
        Json::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
}

/// Resolves a page by id or title (case-insensitive; documents before rows; trashed pages last).
pub fn resolve_page_str(ws: &Workspace, v: &Json) -> Result<u64, String> {
    if let Some(id) = id_of(v) {
        return if ws.page(id).is_some() { Ok(id) } else { Err(format!("No page with id {id}. Use list_pages or search to find ids.")) };
    }
    let Some(t) = v.as_str().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()) else {
        return Err("A page id or title is required.".into());
    };
    let mut hits: Vec<&Page> = ws.pages.iter().filter(|p| p.title.trim().to_lowercase() == t).collect();
    hits.sort_by_key(|p| (p.trashed, p.row));
    match hits.first() {
        Some(p) => Ok(p.id),
        None => {
            let near: Vec<String> = ws.pages.iter().filter(|p| !p.trashed && p.title.to_lowercase().contains(&t)).take(5).map(|p| format!("{} (id {})", p.title, p.id)).collect();
            Err(format!("No page titled “{}”.{}", v.as_str().unwrap_or(""), if near.is_empty() { String::new() } else { format!(" Similar: {}", near.join(", ")) }))
        }
    }
}

pub fn resolve_page(ws: &Workspace, a: &Json, key: &str) -> Result<u64, String> {
    match a.get(key) {
        Some(v) if !v.is_null() => resolve_page_str(ws, v),
        _ => Err(format!("Missing argument “{key}”.")),
    }
}

pub fn resolve_db(ws: &Workspace, v: &Json) -> Result<u64, String> {
    let id = resolve_page_str(ws, v)?;
    if ws.db(id).is_some() {
        Ok(id)
    } else {
        Err(format!("Page {id} (“{}”) is not a database.", ws.page(id).map(|p| p.title.as_str()).unwrap_or("")))
    }
}

pub fn resolve_db_arg(ws: &Workspace, a: &Json, key: &str) -> Result<u64, String> {
    match a.get(key) {
        Some(v) if !v.is_null() => resolve_db(ws, v),
        _ => Err(format!("Missing argument “{key}” (a database id or title).")),
    }
}

/// Property by name (case-insensitive) or id; `title` / `name` / 0 = the title column.
pub fn resolve_prop(ws: &Workspace, db: u64, v: &Json) -> Option<u64> {
    let d = ws.db(db)?;
    if let Some(id) = id_of(v) {
        if id == 0 || d.prop(id).is_some() {
            return Some(id);
        }
    }
    let name = v.as_str()?.trim().to_lowercase();
    if name == "title" || name == "name" {
        return Some(0);
    }
    d.props.iter().find(|p| p.name.to_lowercase() == name).map(|p| p.id)
}

fn prop_names(ws: &Workspace, db: u64) -> String {
    ws.db(db).map(|d| d.props.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(", ")).unwrap_or_default()
}

const BLOCK_TYPES: &[(&str, BlockKind)] = &[
    ("text", BlockKind::Text),
    ("h1", BlockKind::H1),
    ("h2", BlockKind::H2),
    ("h3", BlockKind::H3),
    ("todo", BlockKind::Todo),
    ("bullet", BlockKind::Bullet),
    ("numbered", BlockKind::Numbered),
    ("toggle", BlockKind::Toggle),
    ("quote", BlockKind::Quote),
    ("callout", BlockKind::Callout),
    ("code", BlockKind::Code),
    ("divider", BlockKind::Divider),
    ("toc", BlockKind::Toc),
    ("page_link", BlockKind::PageLink),
    ("image", BlockKind::Image),
    ("embed", BlockKind::Embed),
    ("bookmark", BlockKind::Bookmark),
];

fn kind_name(k: BlockKind) -> &'static str {
    BLOCK_TYPES.iter().find(|(_, x)| *x == k).map(|(n, _)| *n).unwrap_or(match k {
        BlockKind::File => "file",
        BlockKind::Columns => "columns",
        BlockKind::Column => "column",
        BlockKind::DbView => "db_view",
        BlockKind::Widget => "widget",
        BlockKind::Button => "button",
        _ => "text",
    })
}

fn parse_kind(s: &str) -> Option<BlockKind> {
    let s = s.to_lowercase().replace([' ', '-'], "_");
    BLOCK_TYPES.iter().find(|(n, _)| *n == s).map(|(_, k)| *k)
}

fn page_kind_name(k: PageKind) -> &'static str {
    match k {
        PageKind::Doc | PageKind::Board => "doc",
        PageKind::Database => "database",
        PageKind::Canvas => "canvas",
        PageKind::Flow => "flow",
    }
}

fn prop_kind_name(k: PropKind) -> &'static str {
    match k {
        PropKind::Text => "text",
        PropKind::Number => "number",
        PropKind::Select => "select",
        PropKind::MultiSelect => "multi_select",
        PropKind::Checkbox => "checkbox",
        PropKind::Date => "date",
        PropKind::Url => "url",
        PropKind::Formula => "formula",
        PropKind::Relation => "relation",
        PropKind::Rollup => "rollup",
    }
}

fn parse_prop_kind(s: &str) -> Option<PropKind> {
    Some(match s.to_lowercase().replace([' ', '-'], "_").as_str() {
        "text" | "string" | "title" => PropKind::Text,
        "number" | "numeric" | "int" | "float" | "currency" | "percent" => PropKind::Number,
        "select" | "status" | "single_select" => PropKind::Select,
        "multi_select" | "multiselect" | "tags" | "multi" => PropKind::MultiSelect,
        "checkbox" | "bool" | "boolean" | "check" => PropKind::Checkbox,
        "date" | "datetime" | "time" => PropKind::Date,
        "url" | "link" | "email" | "phone" => PropKind::Url,
        "formula" => PropKind::Formula,
        "relation" => PropKind::Relation,
        _ => return None,
    })
}

fn view_kind(s: &str) -> Option<ViewKind> {
    Some(match s.to_lowercase().as_str() {
        "table" => ViewKind::Table,
        "board" | "kanban" => ViewKind::Board,
        "list" => ViewKind::List,
        "gallery" => ViewKind::Gallery,
        "calendar" => ViewKind::Calendar,
        "timeline" | "gantt" => ViewKind::Timeline,
        _ => return None,
    })
}

fn filter_op(s: &str) -> Option<FilterOp> {
    Some(match s.to_lowercase().replace([' ', '-'], "_").as_str() {
        "contains" => FilterOp::Contains,
        "is" | "equals" | "eq" | "=" => FilterOp::Is,
        "is_not" | "not_equals" | "ne" | "!=" => FilterOp::IsNot,
        "gt" | ">" | "after" => FilterOp::Gt,
        "lt" | "<" | "before" => FilterOp::Lt,
        "is_empty" | "empty" => FilterOp::IsEmpty,
        "not_empty" | "is_not_empty" => FilterOp::NotEmpty,
        "checked" => FilterOp::Checked,
        "unchecked" => FilterOp::Unchecked,
        "today" => FilterOp::Today,
        "this_week" => FilterOp::ThisWeek,
        "this_month" => FilterOp::ThisMonth,
        "next_7" | "next7" => FilterOp::Next7,
        "past_7" | "past7" => FilterOp::Past7,
        "overdue" => FilterOp::Overdue,
        _ => return None,
    })
}

/// Parses a list of `{prop, op, value}` into view filters.
pub fn parse_filters(ws: &Workspace, db: u64, list: &[&Json]) -> Result<Vec<Filter>, String> {
    let mut out = vec![];
    for f in list {
        let pv = f.get("prop").cloned().unwrap_or(json!("title"));
        let prop = resolve_prop(ws, db, &pv).ok_or_else(|| format!("Unknown property {pv} (properties: {}).", prop_names(ws, db)))?;
        let ops = str_of(f, "op").unwrap_or("contains");
        let op = filter_op(ops).ok_or_else(|| format!("Unknown filter op “{ops}”."))?;
        let value = match f.get("value") {
            Some(Json::String(s)) => s.clone(),
            Some(Json::Null) | None => String::new(),
            Some(v) => v.to_string(),
        };
        out.push(Filter { prop, op, value });
    }
    Ok(out)
}

// ───────────────────────────── markdown → blocks ─────────────────────────────

/// Converts the Lumen markdown dialect into blocks. With `title_from_h1` the first `# ` line is returned as the title instead.
pub fn md_blocks(ws: &mut Workspace, md: &str, title_from_h1: bool) -> (Option<String>, Vec<Block>) {
    let mut blocks: Vec<Block> = vec![];
    let mut title: Option<String> = None;
    let mut in_code = false;
    let mut code = String::new();
    for raw in md.lines() {
        let line = raw.trim_end();
        if line.trim_start().starts_with("```") {
            if in_code {
                let b = ws.blk(BlockKind::Code, code.trim_end_matches('\n'), 0);
                blocks.push(b);
                code.clear();
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            code.push_str(line);
            code.push('\n');
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let lead: usize = line.chars().take_while(|c| *c == ' ' || *c == '\t').map(|c| if c == '\t' { 2 } else { 1 }).sum();
        let ind = (lead / 2).min(6) as u8;
        let t = line.trim_start();
        let num = {
            let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
            if digits > 0 && t[digits..].starts_with(". ") {
                Some(digits + 2)
            } else {
                None
            }
        };
        let mut url = String::new();
        let mut link = None;
        let (kind, text, checked, indent): (BlockKind, String, bool, u8) = if let Some(r) = t.strip_prefix("#### ").or_else(|| t.strip_prefix("### ")) {
            (BlockKind::H3, r.into(), false, 0)
        } else if let Some(r) = t.strip_prefix("## ") {
            (BlockKind::H2, r.into(), false, 0)
        } else if let Some(r) = t.strip_prefix("# ") {
            if title_from_h1 && title.is_none() && blocks.is_empty() {
                title = Some(r.trim().to_owned());
                continue;
            }
            (BlockKind::H1, r.into(), false, 0)
        } else if t == "---" || t == "***" || t == "___" {
            (BlockKind::Divider, String::new(), false, 0)
        } else if t.eq_ignore_ascii_case("[toc]") {
            (BlockKind::Toc, String::new(), false, 0)
        } else if let Some(r) = t.strip_prefix("- [ ] ").or_else(|| t.strip_prefix("* [ ] ")) {
            (BlockKind::Todo, r.into(), false, ind)
        } else if let Some(r) = t.strip_prefix("- [x] ").or_else(|| t.strip_prefix("- [X] ")).or_else(|| t.strip_prefix("* [x] ")) {
            (BlockKind::Todo, r.into(), true, ind)
        } else if let Some(r) = t.strip_prefix("+ ") {
            (BlockKind::Toggle, r.into(), false, ind)
        } else if let Some(r) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            (BlockKind::Bullet, r.into(), false, ind)
        } else if let Some(k) = num {
            (BlockKind::Numbered, t[k..].into(), false, ind)
        } else if let Some(r) = t.strip_prefix("!> ") {
            (BlockKind::Callout, r.into(), false, 0)
        } else if let Some(r) = t.strip_prefix("> ") {
            (BlockKind::Quote, r.into(), false, 0)
        } else if t.starts_with("![") && t.contains("](") && t.ends_with(')') {
            url = t[t.find("](").unwrap_or(0) + 2..t.len() - 1].to_owned();
            (BlockKind::Image, String::new(), false, 0)
        } else if t.starts_with('<') && t.ends_with('>') && t.contains("://") {
            url = t[1..t.len() - 1].to_owned();
            (BlockKind::Bookmark, String::new(), false, 0)
        } else if t.starts_with("[[") && t.ends_with("]]") && !t[2..t.len() - 2].contains("]]") {
            let name = t[2..t.len() - 2].trim().to_owned();
            match resolve_page_str(ws, &json!(name)) {
                Ok(id) => {
                    link = Some(id);
                    (BlockKind::PageLink, String::new(), false, 0)
                }
                Err(_) => (BlockKind::Text, t.into(), false, 0),
            }
        } else {
            (BlockKind::Text, t.into(), false, 0)
        };
        let mut blk = ws.blk(kind, &text, indent);
        blk.checked = checked;
        blk.url = url;
        blk.link = link;
        blocks.push(blk);
    }
    if in_code && !code.is_empty() {
        let b = ws.blk(BlockKind::Code, code.trim_end_matches('\n'), 0);
        blocks.push(b);
    }
    (title, blocks)
}

// ───────────────────────────── describing things ─────────────────────────────

fn clip(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

pub fn row_json(ws: &Workspace, db: u64, row: u64) -> Json {
    let Some(p) = ws.page(row) else { return Json::Null };
    let mut vals = Map::new();
    if let Some(d) = ws.db(db) {
        for pr in &d.props {
            if let Some(v) = p.props.get(&pr.id) {
                let t = d.val_text(pr.id, v);
                if !t.is_empty() {
                    vals.insert(pr.name.clone(), match v {
                        Val::Num(x) => json!(x),
                        Val::Check(c) => json!(c),
                        Val::Multi(m) if pr.kind == PropKind::MultiSelect => json!(m.iter().filter_map(|o| d.opt(pr.id, *o)).map(|o| o.name.clone()).collect::<Vec<_>>()),
                        _ => json!(t),
                    });
                }
            }
        }
    }
    json!({"id": row, "title": p.title, "values": vals})
}

pub fn describe_db(ws: &Workspace, id: u64, sample: usize) -> Json {
    let Some(d) = ws.db(id) else { return Json::Null };
    let props: Vec<Json> = d
        .props
        .iter()
        .map(|p| {
            let mut j = json!({"id": p.id, "name": p.name, "type": prop_kind_name(p.kind)});
            if !p.opts.is_empty() {
                j["options"] = json!(p.opts.iter().map(|o| o.name.clone()).collect::<Vec<_>>());
            }
            if p.kind == PropKind::Formula {
                j["formula"] = json!(p.cfg.formula);
            }
            if p.kind == PropKind::Relation {
                j["relation_to"] = json!(p.cfg.relation);
            }
            j
        })
        .collect();
    let views: Vec<Json> = d.views.iter().filter(|v| v.linked == 0).map(|v| json!({"name": v.name, "kind": format!("{:?}", v.kind).to_lowercase()})).collect();
    let rows = ws.rows(id);
    let sample_rows: Vec<Json> = rows.iter().take(sample).map(|r| row_json(ws, id, *r)).collect();
    json!({"properties": props, "views": views, "row_count": rows.len(), "rows": sample_rows})
}

pub fn describe_page(ws: &Workspace, id: u64) -> Result<Json, String> {
    let p = ws.page(id).ok_or_else(|| format!("No page with id {id}."))?;
    let mut j = json!({
        "id": p.id, "title": p.title, "icon": p.icon, "kind": page_kind_name(p.kind), "parent": p.parent,
        "is_row": p.row, "favorite": p.favorite, "trashed": p.trashed,
        "children": ws.children(Some(id)).into_iter().filter_map(|c| ws.page(c).map(|x| json!({"id": x.id, "title": x.title, "kind": page_kind_name(x.kind)}))).collect::<Vec<_>>(),
    });
    if p.row {
        if let Some(db) = p.parent {
            j["database"] = json!(db);
            j["values"] = row_json(ws, db, id)["values"].clone();
        }
    }
    match p.kind {
        PageKind::Database => j["database_info"] = describe_db(ws, id, 12),
        PageKind::Canvas => {
            if let Some(c) = &p.canvas {
                let nodes: Vec<Json> = c.nodes.iter().take(200).map(|n| json!({"id": n.id, "kind": format!("{:?}", n.kind).to_lowercase(), "text": clip(&n.text, 120), "x": n.x.round(), "y": n.y.round(), "w": n.w.round(), "h": n.h.round()})).collect();
                j["canvas"] = json!({"nodes": nodes, "edges": c.edges.iter().map(|e| json!({"from": e.from, "to": e.to, "label": e.label})).collect::<Vec<_>>()});
            }
        }
        PageKind::Flow => {
            if let Some(f) = &p.flow {
                let nodes: Vec<Json> = f.nodes.iter().map(|n| json!({"id": n.id, "kind": n.kind, "label": n.label, "params": n.params})).collect();
                let wires: Vec<Json> = f
                    .wires
                    .iter()
                    .map(|w| {
                        let fo = f.node(w.from).and_then(|n| n.def()).and_then(|d| d.outs.get(w.fp as usize).copied()).unwrap_or("");
                        let ti = f.node(w.to).and_then(|n| n.def()).and_then(|d| d.ins.get(w.tp as usize).copied()).unwrap_or("");
                        json!({"from": w.from, "from_port": fo, "to": w.to, "to_port": ti})
                    })
                    .collect();
                j["flow"] = json!({"nodes": nodes, "wires": wires, "vars": f.vars, "every_seconds": f.every, "problems": f.problems()});
            }
        }
        _ => {}
    }
    if p.kind == PageKind::Doc || p.kind == PageKind::Board {
        let blocks: Vec<Json> = p
            .blocks
            .iter()
            .take(400)
            .map(|b| {
                let mut x = json!({"id": b.id, "type": kind_name(b.kind), "text": clip(&b.text, 600)});
                if b.indent > 0 {
                    x["indent"] = json!(b.indent);
                }
                if b.kind == BlockKind::Todo {
                    x["checked"] = json!(b.checked);
                }
                if !b.url.is_empty() {
                    x["url"] = json!(b.url);
                }
                if let Some(l) = b.link {
                    x["link"] = json!(l);
                }
                x
            })
            .collect();
        j["blocks"] = json!(blocks);
        if p.blocks.len() > 400 {
            j["blocks_truncated"] = json!(p.blocks.len());
        }
    }
    if let Some(bid) = ws.dash.page_board(id) {
        if let Some(b) = ws.dash.boards.iter().find(|b| b.id == bid) {
            j["dashboard"] = json!({"widgets": b.widgets.iter().map(|w| {
                let kind = crate::dashcfg::plot_of(w).unwrap_or_else(|| w.kind.clone());
                json!({"id": w.id, "kind": kind, "title": w.title, "span": w.span, "rows": w.rows, "database": w.cfg.db})
            }).collect::<Vec<_>>()});
        }
    }
    Ok(j)
}

fn tree(ws: &Workspace, parent: Option<u64>, depth: usize, budget: &mut usize) -> Vec<Json> {
    let mut out = vec![];
    for id in ws.children(parent) {
        let Some(p) = ws.page(id) else { continue };
        if p.trashed || p.row {
            continue;
        }
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let mut j = json!({"id": id, "title": p.title, "icon": p.icon, "kind": page_kind_name(p.kind)});
        if p.kind == PageKind::Database {
            j["rows"] = json!(ws.rows(id).len());
        }
        if depth < 4 {
            let ch = tree(ws, Some(id), depth + 1, budget);
            if !ch.is_empty() {
                j["children"] = json!(ch);
            }
        }
        out.push(j);
    }
    out
}

// ───────────────────────────── values ─────────────────────────────

pub(crate) fn ensure_opt(ws: &mut Workspace, db: u64, prop: u64, name: &str) -> u64 {
    if let Some(id) = ws.opt_id(db, prop, name).or_else(|| {
        let low = name.to_lowercase();
        ws.db(db)?.prop(prop)?.opts.iter().find(|o| o.name.to_lowercase() == low).map(|o| o.id)
    }) {
        return id;
    }
    let oid = ws.id();
    if let Some(p) = ws.db_mut(db).and_then(|d| d.props.iter_mut().find(|p| p.id == prop)) {
        let color = p.opts.len() % 9;
        p.opts.push(Opt { id: oid, name: name.to_owned(), color });
    }
    oid
}

fn date_text(v: &str) -> Result<String, String> {
    let one = |x: &str| dates::parse_one(x).map(dates::day_of);
    let v = v.trim();
    let parts: Vec<&str> = v.split(['/', '→']).map(|x| x.trim()).collect();
    match parts.as_slice() {
        [a] => one(a).map(|_| (*a).to_owned()).ok_or_else(|| format!("“{v}” is not an ISO date (YYYY-MM-DD).")),
        [a, b] => match (one(a), one(b)) {
            (Some(x), Some(y)) => Ok(dates::span_value(x, y)),
            _ => Err(format!("“{v}” is not an ISO date range (YYYY-MM-DD/YYYY-MM-DD).")),
        },
        _ => Err(format!("“{v}” is not an ISO date.")),
    }
}

pub fn to_val(ws: &mut Workspace, db: u64, prop: u64, v: &Json) -> Result<Option<Val>, String> {
    let (kind, rel) = match ws.db(db).and_then(|d| d.prop(prop)) {
        Some(p) => (p.kind, p.cfg.relation),
        None => return Ok(None),
    };
    if v.is_null() {
        return Ok(None);
    }
    let text = |v: &Json| -> String {
        match v {
            Json::String(s) => s.clone(),
            Json::Null => String::new(),
            o => o.to_string().trim_matches('"').to_owned(),
        }
    };
    Ok(match kind {
        PropKind::Text | PropKind::Url => Some(Val::Text(text(v))),
        PropKind::Number => match v {
            Json::Number(n) => n.as_f64().map(Val::Num),
            Json::String(s) => {
                let c: String = s.chars().filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
                Some(Val::Num(c.parse::<f64>().map_err(|_| format!("“{s}” is not a number."))?))
            }
            Json::Bool(b) => Some(Val::Num(*b as u8 as f64)),
            _ => None,
        },
        PropKind::Checkbox => Some(Val::Check(match v {
            Json::Bool(b) => *b,
            Json::String(s) => matches!(s.to_lowercase().as_str(), "true" | "yes" | "1" | "done" | "checked"),
            Json::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
            _ => false,
        })),
        PropKind::Date => {
            let t = text(v);
            if t.is_empty() {
                None
            } else {
                Some(Val::Text(date_text(&t)?))
            }
        }
        PropKind::Select => {
            let t = text(v);
            if t.trim().is_empty() {
                None
            } else {
                Some(Val::Sel(ensure_opt(ws, db, prop, t.trim())))
            }
        }
        PropKind::MultiSelect => {
            let names: Vec<String> = match v {
                Json::Array(a) => a.iter().map(text).collect(),
                o => text(o).split(',').map(|x| x.trim().to_owned()).collect(),
            };
            let ids: Vec<u64> = names.iter().filter(|n| !n.is_empty()).map(|n| ensure_opt(ws, db, prop, n)).collect();
            if ids.is_empty() {
                None
            } else {
                Some(Val::Multi(ids))
            }
        }
        PropKind::Relation => {
            let target = if rel != 0 { rel } else { db };
            let items: Vec<&Json> = match v {
                Json::Array(a) => a.iter().collect(),
                o => vec![o],
            };
            let mut ids = vec![];
            for it in items {
                let found = if let Some(id) = id_of(it) {
                    ws.page(id).filter(|p| p.row).map(|p| p.id)
                } else {
                    let t = text(it).to_lowercase();
                    ws.rows(target).into_iter().find(|r| ws.page(*r).map_or(false, |p| p.title.to_lowercase() == t))
                };
                match found {
                    Some(id) => ids.push(id),
                    None => return Err(format!("Relation target {it} not found in database {target}. Create that row first.")),
                }
            }
            if ids.is_empty() {
                None
            } else {
                Some(Val::Multi(ids))
            }
        }
        PropKind::Formula | PropKind::Rollup => None,
    })
}

fn set_values(ws: &mut Workspace, db: u64, row: u64, values: &Json) -> Result<(), String> {
    let Some(m) = values.as_object() else { return Ok(()) };
    for (k, v) in m {
        let Some(prop) = resolve_prop(ws, db, &json!(k)) else {
            return Err(format!("Unknown property “{k}”. Properties of this database: {}. Add it first with add_property.", prop_names(ws, db)));
        };
        if prop == 0 {
            if let Some(t) = v.as_str() {
                if let Some(p) = ws.page_mut(row) {
                    p.title = t.to_owned();
                }
            }
            continue;
        }
        let val = to_val(ws, db, prop, v)?;
        if let Some(p) = ws.page_mut(row) {
            match val {
                Some(x) => {
                    p.props.insert(prop, x);
                }
                None => {
                    p.props.remove(&prop);
                }
            }
        }
    }
    Ok(())
}

pub fn add_rows_to(ws: &mut Workspace, db: u64, rows: &[&Json]) -> Result<Vec<u64>, String> {
    let mut ids = vec![];
    for (i, r) in rows.iter().take(200).enumerate() {
        let id = ws.add_row(db, None);
        ids.push(id);
        let title = match r {
            Json::String(s) => s.clone(),
            _ => str_of(r, "title").or_else(|| str_of(r, "name")).unwrap_or("").to_owned(),
        };
        if let Some(p) = ws.page_mut(id) {
            p.title = title;
            p.updated = now_ms();
        }
        if let Some(v) = r.get("values") {
            set_values(ws, db, id, v).map_err(|e| format!("row {}: {e}", i + 1))?;
        }
        if let Some(body) = str_of(r, "body") {
            let (_, blocks) = md_blocks(ws, body, false);
            if !blocks.is_empty() {
                if let Some(p) = ws.page_mut(id) {
                    p.blocks = blocks;
                }
            }
        }
    }
    Ok(ids)
}

// ───────────────────────────── core tools ─────────────────────────────

fn core_tools() -> Vec<(ToolDef, Handler)> {
    use Risk::*;
    let t = ToolDef::new;
    let blocks_schema = arr(obj(json!({"type": s("text|h1|h2|h3|todo|bullet|numbered|toggle|quote|callout|code|divider|toc|image|bookmark|embed"), "text": s("Text with inline markdown"), "checked": b("to-dos"), "indent": n("0-6"), "url": s("image/bookmark/embed")}), &["type"]), "Blocks to add");
    vec![
        (t("workspace_overview", "Orientation: workspace name, today's date, counts, the page the user has open, and the page tree (documents, databases, canvases, flows). Call this first.", Read, obj(json!({}), &[])), tool_overview),
        (t("list_pages", "List pages (title, kind, parent). Rows of databases are excluded unless include_rows is true.", Read, obj(json!({"parent": page_ref("Only children of this page"), "kind": s("doc|database|canvas|flow"), "include_rows": b("Include database rows"), "include_trashed": b("Include trashed pages")}), &[])), tool_list_pages),
        (t("search", "Full-text search over titles and text (pages, rows, blocks). Returns ids and snippets.", Read, obj(json!({"query": s("Words to look for"), "limit": n("Max results (default 20)")}), &["query"])), tool_search),
        (t("read_page", "Read one page completely: blocks with ids, database schema + rows, canvas nodes, flow nodes/wires/problems, dashboard widgets.", Read, obj(json!({"page": page_ref("The page")}), &["page"])), tool_read_page),
        (t("get_knowledge", "Read the Lumen knowledge base. Call with no module to list modules; then read the one you need (overview, pages, databases, dashboards, canvas, flows, automations, scripting, architect, extending, …).", Read, obj(json!({"module": s("Module id")}), &[])), tool_knowledge),
        (t("create_page", "Create a document page. `markdown` becomes the body (see knowledge: pages). Returns the new id.", Write, obj(json!({"title": s("Page title (optional if markdown starts with '# Title')"), "parent": page_ref("Parent page (omit = top level)"), "icon": s("One emoji"), "markdown": s("Body in Lumen markdown"), "favorite": b("Pin to favorites")}), &[])), tool_create_page),
        (t("append_markdown", "Add markdown content to the end of a page (or after a given block).", Write, obj(json!({"page": page_ref("Target page"), "markdown": s("Content"), "after_block": n("Insert after this block id")}), &["page", "markdown"])), tool_append_markdown),
        (t("append_blocks", "Add structured blocks to a page.", Write, obj(json!({"page": page_ref("Target page"), "blocks": blocks_schema, "after_block": n("Insert after this block id")}), &["page", "blocks"])), tool_append_blocks),
        (t("edit_block", "Change the text, type or checked state of one block.", Write, obj(json!({"page": page_ref("Page"), "block": n("Block id"), "text": s("New text"), "type": s("New block type"), "checked": b("To-do state")}), &["page", "block"])), tool_edit_block),
        (t("delete_block", "Remove a block from a page.", Destroy, obj(json!({"page": page_ref("Page"), "block": n("Block id")}), &["page", "block"])), tool_delete_block),
        (t("replace_page_content", "Replace the entire body of a page with markdown (a history version is kept).", Destroy, obj(json!({"page": page_ref("Page"), "markdown": s("New body")}), &["page", "markdown"])), tool_replace_content),
        (t("update_page", "Rename, re-icon, move or favorite a page.", Write, obj(json!({"page": page_ref("Page"), "title": s("New title"), "icon": s("Emoji"), "parent": page_ref("New parent, or 0 for top level"), "favorite": b("Favorite"), "full_width": b("Wide layout")}), &["page"])), tool_update_page),
        (t("open_page", "Ask the app to show a page to the user.", Write, obj(json!({"page": page_ref("Page")}), &["page"])), tool_open_page),
        (t("trash_page", "Move a page (and its sub-pages) to the trash. Recoverable.", Destroy, obj(json!({"page": page_ref("Page or database row")}), &["page"])), tool_trash),
        (t("restore_page", "Restore a page from the trash.", Write, obj(json!({"page": page_ref("Page")}), &["page"])), tool_restore),
        (t("create_database", "Create a database with typed properties, optional rows and a first view. Starts empty (no default columns).", Write, obj(json!({
            "title": s("Plural noun, e.g. Tasks"), "parent": page_ref("Parent"), "icon": s("Emoji"),
            "view": s("table|board|list|gallery|calendar|timeline"),
            "properties": arr(obj(json!({"name": s("Column name"), "type": s("text|number|select|multi_select|checkbox|date|url|relation|formula"), "options": arr(s("option"), "select options"), "formula": s("formula expression"), "relation_to": page_ref("relation target database (default: itself)")}), &["name", "type"]), "Columns (the title column always exists)"),
            "rows": arr(obj(json!({"title": s("Row title"), "values": json!({"type": "object", "description": "property name → value"}), "body": s("markdown body")}), &["title"]), "Initial rows"),
        }), &["title"])), tool_create_database),
        (t("add_property", "Add a column to a database.", Write, obj(json!({"database": page_ref("Database"), "name": s("Name"), "type": s("Property type"), "options": arr(s("option"), "select options"), "formula": s("formula"), "relation_to": page_ref("relation target")}), &["database", "name", "type"])), tool_add_property),
        (t("add_rows", "Add rows (max 200 per call). values: {PropertyName: value}.", Write, obj(json!({"database": page_ref("Database"), "rows": arr(obj(json!({"title": s("Title"), "values": json!({"type": "object"}), "body": s("markdown body")}), &["title"]), "Rows")}), &["database", "rows"])), tool_add_rows),
        (t("update_row", "Change a row's title and/or property values.", Write, obj(json!({"row": page_ref("Row page"), "title": s("New title"), "values": json!({"type": "object", "description": "property name → value (null clears)"})}), &["row"])), tool_update_row),
        (t("query_database", "Read rows of a database, optionally filtered. where: {Property: text} (case-insensitive match).", Read, obj(json!({"database": page_ref("Database"), "where": json!({"type": "object"}), "search": s("text anywhere in the row"), "limit": n("default 50, max 200")}), &["database"])), tool_query_db),
        (t("add_view", "Add a view (table, board, list, gallery, calendar, timeline) to a database.", Write, obj(json!({"database": page_ref("Database"), "kind": s("view type"), "name": s("Tab name"), "group_by": s("Board: property to group by"), "filters": arr(obj(json!({"prop": s("property"), "op": s("filter op"), "value": s("value")}), &["prop"]), "Filters"), "sorts": arr(obj(json!({"prop": s("property"), "asc": b("ascending")}), &["prop"]), "Sorts")}), &["database", "kind"])), tool_add_view),
    ]
}

fn tool_overview(ws: &mut Workspace, _a: &Json) -> Result<Json, String> {
    let live = ws.pages.iter().filter(|p| !p.trashed);
    let (mut docs, mut dbs, mut canvases, mut flows, mut rows) = (0, 0, 0, 0, 0);
    for p in live {
        if p.row {
            rows += 1;
            continue;
        }
        match p.kind {
            PageKind::Database => dbs += 1,
            PageKind::Canvas => canvases += 1,
            PageKind::Flow => flows += 1,
            _ => docs += 1,
        }
    }
    let mut budget = 150usize;
    let current = ws.current.and_then(|c| ws.page(c)).map(|p| json!({"id": p.id, "title": p.title, "kind": page_kind_name(p.kind)}));
    Ok(json!({
        "workspace": ws.name, "today": dates::today_iso(), "agent_api_version": AGENT_API_VERSION,
        "counts": {"documents": docs, "databases": dbs, "database_rows": rows, "canvases": canvases, "flows": flows, "dashboards": ws.dash.boards.len(), "rules": ws.auto.rules.len(), "scripts": ws.auto.scripts.len(), "reminders": ws.auto.reminders.len()},
        "open_page": current,
        "pages": tree(ws, None, 0, &mut budget),
        "knowledge_modules": knowledge().iter().map(|k| k.id.clone()).collect::<Vec<_>>(),
    }))
}

fn tool_list_pages(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let parent = match a.get("parent") {
        Some(v) if !v.is_null() && id_of(v) != Some(0) => Some(resolve_page_str(ws, v)?),
        _ => None,
    };
    let kind = str_of(a, "kind");
    let rows = bool_of(a, "include_rows").unwrap_or(false);
    let trashed = bool_of(a, "include_trashed").unwrap_or(false);
    let list: Vec<Json> = ws
        .pages
        .iter()
        .filter(|p| (trashed || !p.trashed) && (rows || !p.row) && parent.map_or(true, |x| p.parent == Some(x)) && kind.map_or(true, |k| page_kind_name(p.kind) == k))
        .take(300)
        .map(|p| json!({"id": p.id, "title": p.title, "icon": p.icon, "kind": page_kind_name(p.kind), "parent": p.parent, "is_row": p.row}))
        .collect();
    Ok(json!({"count": list.len(), "pages": list}))
}

fn tool_search(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let q = str_of(a, "query").ok_or("query is required")?.to_lowercase();
    let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).min(100) as usize;
    let mut out = vec![];
    for p in ws.pages.iter().filter(|p| !p.trashed) {
        let in_title = p.title.to_lowercase().contains(&q);
        let hit = p.blocks.iter().find(|b| b.text.to_lowercase().contains(&q));
        if in_title || hit.is_some() {
            out.push(json!({"id": p.id, "title": p.title, "kind": page_kind_name(p.kind), "is_row": p.row, "snippet": hit.map(|b| clip(&crate::rich::strip(&b.text), 140))}));
            if out.len() >= limit {
                break;
            }
        }
    }
    Ok(json!({"count": out.len(), "results": out}))
}

fn tool_read_page(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let id = resolve_page(ws, a, "page")?;
    describe_page(ws, id)
}

fn tool_knowledge(_ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let all = knowledge();
    match str_of(a, "module") {
        None => Ok(json!({"modules": all.iter().map(|k| json!({"id": k.id, "title": k.title, "summary": k.summary})).collect::<Vec<_>>(), "hint": "Call get_knowledge with {module} to read one."})),
        Some(m) => {
            let m = m.to_lowercase();
            match all.iter().find(|k| k.id == m) {
                Some(k) => Ok(json!({"id": k.id, "title": k.title, "content": k.body})),
                None => Err(format!("No module “{m}”. Available: {}", all.iter().map(|k| k.id.as_str()).collect::<Vec<_>>().join(", "))),
            }
        }
    }
}

pub(crate) fn parent_arg(ws: &Workspace, a: &Json) -> Result<Option<u64>, String> {
    match a.get("parent") {
        Some(v) if !v.is_null() && id_of(v) != Some(0) && v.as_str().map_or(true, |s| !s.trim().is_empty()) => Ok(Some(resolve_page_str(ws, v)?)),
        _ => Ok(None),
    }
}

pub(crate) fn finish_new(ws: &mut Workspace, id: u64, a: &Json, default_icon: &str) {
    let icon = str_of(a, "icon").map(|s| s.to_owned());
    let title = str_of(a, "title").map(|s| s.to_owned());
    let fav = bool_of(a, "favorite");
    if let Some(p) = ws.page_mut(id) {
        if let Some(t) = title {
            p.title = t;
        }
        match icon {
            Some(i) => p.icon = i,
            None if p.icon.is_empty() => p.icon = default_icon.to_owned(),
            None => {}
        }
        if let Some(f) = fav {
            p.favorite = f;
        }
        p.updated = now_ms();
    }
}

fn tool_create_page(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let parent = parent_arg(ws, a)?;
    let md = str_of(a, "markdown").unwrap_or("");
    let (h1, blocks) = md_blocks(ws, md, str_of(a, "title").is_none());
    let id = ws.add_page(parent, PageKind::Doc);
    finish_new(ws, id, a, "📄");
    if let Some(t) = h1 {
        if let Some(p) = ws.page_mut(id) {
            p.title = t;
        }
    }
    if !blocks.is_empty() {
        if let Some(p) = ws.page_mut(id) {
            p.blocks = blocks;
        }
    }
    let title = ws.page(id).map(|p| p.title.clone()).unwrap_or_default();
    if title.is_empty() {
        if let Some(p) = ws.page_mut(id) {
            p.title = "Untitled".into();
        }
    }
    Ok(json!({"id": id, "title": ws.page(id).map(|p| p.title.clone()), "blocks": ws.page(id).map(|p| p.blocks.len())}))
}

fn doc_page(ws: &Workspace, a: &Json) -> Result<u64, String> {
    let id = resolve_page(ws, a, "page")?;
    match ws.page(id).map(|p| p.kind) {
        Some(PageKind::Doc) | Some(PageKind::Board) => Ok(id),
        _ => Err(format!("Page {id} is not a document page (kind {}). Use the tool for that kind.", ws.page(id).map(|p| page_kind_name(p.kind)).unwrap_or("?"))),
    }
}

fn insert_blocks(ws: &mut Workspace, id: u64, blocks: Vec<Block>, after: Option<u64>) -> usize {
    let n = blocks.len();
    if let Some(p) = ws.page_mut(id) {
        // a lone empty text block (a fresh page) is replaced
        if p.blocks.len() == 1 && p.blocks[0].kind == BlockKind::Text && p.blocks[0].text.is_empty() && after.is_none() {
            p.blocks.clear();
        }
        let at = after.and_then(|b| p.blocks.iter().position(|x| x.id == b)).map(|i| i + 1).unwrap_or(p.blocks.len());
        for (k, b) in blocks.into_iter().enumerate() {
            p.blocks.insert(at + k, b);
        }
        p.updated = now_ms();
    }
    n
}

fn tool_append_markdown(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let id = doc_page(ws, a)?;
    let (_, blocks) = md_blocks(ws, str_of(a, "markdown").ok_or("markdown is required")?, false);
    let after = a.get("after_block").and_then(|v| v.as_u64());
    let n = insert_blocks(ws, id, blocks, after);
    Ok(json!({"id": id, "added_blocks": n}))
}

fn tool_append_blocks(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let id = doc_page(ws, a)?;
    let mut blocks = vec![];
    for (i, spec) in arr_of(a, "blocks").into_iter().enumerate() {
        let ty = str_of(spec, "type").unwrap_or("text");
        let kind = parse_kind(ty).ok_or_else(|| format!("block {}: unknown type “{ty}”", i + 1))?;
        let indent = spec.get("indent").and_then(|v| v.as_u64()).unwrap_or(0).min(6) as u8;
        let mut blk = ws.blk(kind, spec.get("text").and_then(|v| v.as_str()).unwrap_or(""), indent);
        blk.checked = bool_of(spec, "checked").unwrap_or(false);
        blk.url = str_of(spec, "url").unwrap_or("").to_owned();
        if let Some(l) = spec.get("link") {
            blk.link = id_of(l);
        }
        blocks.push(blk);
    }
    let after = a.get("after_block").and_then(|v| v.as_u64());
    let n = insert_blocks(ws, id, blocks, after);
    Ok(json!({"id": id, "added_blocks": n}))
}

fn tool_edit_block(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let id = doc_page(ws, a)?;
    let bid = a.get("block").and_then(id_of).ok_or("block (id) is required")?;
    let kind = match str_of(a, "type") {
        Some(t) => Some(parse_kind(t).ok_or_else(|| format!("unknown block type “{t}”"))?),
        None => None,
    };
    let p = ws.page_mut(id).ok_or("page vanished")?;
    let blk = p.blocks.iter_mut().find(|b| b.id == bid).ok_or_else(|| format!("No block {bid} on page {id}. read_page shows block ids."))?;
    if let Some(t) = a.get("text").and_then(|v| v.as_str()) {
        blk.text = t.to_owned();
    }
    if let Some(k) = kind {
        blk.kind = k;
    }
    if let Some(c) = bool_of(a, "checked") {
        blk.checked = c;
    }
    p.updated = now_ms();
    Ok(json!({"id": id, "block": bid}))
}

fn tool_delete_block(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let id = doc_page(ws, a)?;
    let bid = a.get("block").and_then(id_of).ok_or("block (id) is required")?;
    let p = ws.page_mut(id).ok_or("page vanished")?;
    let before = p.blocks.len();
    p.blocks.retain(|b| b.id != bid);
    if p.blocks.len() == before {
        return Err(format!("No block {bid} on page {id}."));
    }
    p.updated = now_ms();
    Ok(json!({"id": id, "deleted": bid}))
}

fn tool_replace_content(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let id = doc_page(ws, a)?;
    let (_, mut blocks) = md_blocks(ws, str_of(a, "markdown").unwrap_or(""), false);
    if blocks.is_empty() {
        blocks.push(ws.blk(BlockKind::Text, "", 0));
    }
    if let Some(p) = ws.page_mut(id) {
        crate::history::snapshot(p, false, "Before AI rewrite");
        p.blocks = blocks;
        p.updated = now_ms();
    }
    Ok(json!({"id": id}))
}

fn tool_update_page(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let id = resolve_page(ws, a, "page")?;
    if let Some(v) = a.get("parent") {
        if !v.is_null() {
            let np = if id_of(v) == Some(0) { None } else { Some(resolve_page_str(ws, v)?) };
            if let Some(x) = np {
                if x == id || ws.descendants(id).contains(&x) {
                    return Err("A page cannot be moved into itself or one of its sub-pages.".into());
                }
            }
            if let Some(p) = ws.page_mut(id) {
                p.parent = np;
            }
        }
    }
    let title = str_of(a, "title").map(|s| s.to_owned());
    let icon = str_of(a, "icon").map(|s| s.to_owned());
    let (fav, wide) = (bool_of(a, "favorite"), bool_of(a, "full_width"));
    if let Some(p) = ws.page_mut(id) {
        if let Some(t) = title {
            p.title = t;
        }
        if let Some(i) = icon {
            p.icon = i;
        }
        if let Some(f) = fav {
            p.favorite = f;
        }
        if let Some(w) = wide {
            p.full_width = w;
        }
        p.updated = now_ms();
    }
    Ok(json!({"id": id}))
}

fn tool_open_page(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let id = resolve_page(ws, a, "page")?;
    if ws.page(id).map_or(true, |p| p.trashed) {
        return Err("That page is in the trash.".into());
    }
    ws.current = Some(id);
    Ok(json!({"id": id, "opened": true}))
}

fn tool_trash(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let id = resolve_page(ws, a, "page")?;
    ws.set_trashed(id, true);
    Ok(json!({"id": id, "trashed": true}))
}

fn tool_restore(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let id = resolve_page(ws, a, "page")?;
    ws.set_trashed(id, false);
    Ok(json!({"id": id, "restored": true}))
}

fn add_prop_from(ws: &mut Workspace, db: u64, spec: &Json) -> Result<u64, String> {
    let name = str_of(spec, "name").ok_or("a property needs a name")?;
    let ty = str_of(spec, "type").unwrap_or("text");
    let kind = parse_prop_kind(ty).ok_or_else(|| format!("Unknown property type “{ty}”. Use text, number, select, multi_select, checkbox, date, url, relation or formula."))?;
    let opts: Vec<String> = arr_of(spec, "options").into_iter().filter_map(|o| o.as_str().map(|s| s.to_owned())).collect();
    let refs: Vec<&str> = opts.iter().map(|s| s.as_str()).collect();
    let rel = if kind == PropKind::Relation {
        Some(match spec.get("relation_to") {
            Some(v) if !v.is_null() => resolve_db(ws, v)?,
            _ => db,
        })
    } else {
        None
    };
    let pid = ws.add_prop(db, name, kind, &refs);
    if let Some(p) = ws.db_mut(db).and_then(|d| d.props.iter_mut().find(|p| p.id == pid)) {
        if let Some(r) = rel {
            p.cfg.relation = r;
        }
        if kind == PropKind::Formula {
            p.cfg.formula = str_of(spec, "formula").unwrap_or("").to_owned();
        }
    }
    Ok(pid)
}

fn tool_create_database(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let parent = parent_arg(ws, a)?;
    let vk = match str_of(a, "view") {
        Some(v) => view_kind(v).ok_or_else(|| format!("Unknown view “{v}”."))?,
        None => ViewKind::Table,
    };
    let id = ws.add_database(parent, vk);
    ws.pages.retain(|p| !(p.row && p.parent == Some(id)));
    if let Some(d) = ws.db_mut(id) {
        d.props.clear();
    }
    finish_new(ws, id, a, "📁");
    if ws.page(id).map_or(true, |p| p.title.is_empty()) {
        if let Some(p) = ws.page_mut(id) {
            p.title = "Database".into();
        }
    }
    let mut result = (|| -> Result<Vec<u64>, String> {
        for spec in arr_of(a, "properties") {
            add_prop_from(ws, id, spec)?;
        }
        let rows = arr_of(a, "rows");
        add_rows_to(ws, id, &rows)
    })();
    if let Err(e) = &mut result {
        // never leave a half-built database behind
        ws.delete_forever(id);
        return Err(format!("{e} (nothing was created)"));
    }
    let _ = crate::formula::recompute(ws);
    Ok(json!({"id": id, "title": ws.page(id).map(|p| p.title.clone()), "database": describe_db(ws, id, 3)}))
}

fn tool_add_property(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let db = resolve_db_arg(ws, a, "database")?;
    let pid = add_prop_from(ws, db, a)?;
    Ok(json!({"database": db, "property": pid}))
}

fn tool_add_rows(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let db = resolve_db_arg(ws, a, "database")?;
    let rows = arr_of(a, "rows");
    if rows.is_empty() {
        return Err("rows is empty".into());
    }
    let ids = add_rows_to(ws, db, &rows)?;
    Ok(json!({"database": db, "row_ids": ids}))
}

fn tool_update_row(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let id = resolve_page(ws, a, "row")?;
    let db = ws.page(id).filter(|p| p.row).and_then(|p| p.parent).ok_or("That page is not a database row.")?;
    if let Some(t) = str_of(a, "title") {
        if let Some(p) = ws.page_mut(id) {
            p.title = t.to_owned();
        }
    }
    if let Some(v) = a.get("values") {
        set_values(ws, db, id, v)?;
    }
    ws.touch(id);
    Ok(json!({"row": id, "now": row_json(ws, db, id)}))
}

fn tool_query_db(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let db = resolve_db_arg(ws, a, "database")?;
    let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(50).min(200) as usize;
    let search = str_of(a, "search").map(|s| s.to_lowercase());
    let mut conds: Vec<(u64, String)> = vec![];
    if let Some(m) = a.get("where").and_then(|w| w.as_object()) {
        for (k, v) in m {
            let p = resolve_prop(ws, db, &json!(k)).ok_or_else(|| format!("Unknown property “{k}” (properties: {}).", prop_names(ws, db)))?;
            conds.push((p, match v {
                Json::String(s) => s.to_lowercase(),
                o => o.to_string().to_lowercase(),
            }));
        }
    }
    let d = ws.db(db).ok_or("not a database")?;
    let matched: Vec<u64> = ws
        .rows(db)
        .into_iter()
        .filter(|r| {
            let Some(p) = ws.page(*r) else { return false };
            let cell = |prop: u64| -> String {
                if prop == 0 {
                    p.title.to_lowercase()
                } else {
                    p.props.get(&prop).map(|v| d.val_text(prop, v).to_lowercase()).unwrap_or_default()
                }
            };
            conds.iter().all(|(prop, want)| {
                let got = cell(*prop);
                got == *want || got.contains(want.as_str())
            }) && search.as_ref().map_or(true, |q| {
                p.title.to_lowercase().contains(q) || d.props.iter().any(|pr| cell(pr.id).contains(q.as_str()))
            })
        })
        .collect();
    let total = matched.len();
    Ok(json!({"database": db, "matched": total, "rows": matched.into_iter().take(limit).map(|r| row_json(ws, db, r)).collect::<Vec<_>>()}))
}

fn tool_add_view(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let db = resolve_db_arg(ws, a, "database")?;
    let k = str_of(a, "kind").ok_or("kind is required")?;
    let kind = view_kind(k).ok_or_else(|| format!("Unknown view kind “{k}”."))?;
    let group = match str_of(a, "group_by") {
        Some(g) => Some(resolve_prop(ws, db, &json!(g)).ok_or_else(|| format!("Unknown property “{g}” (properties: {}).", prop_names(ws, db)))?),
        None => None,
    };
    let filters = parse_filters(ws, db, &arr_of(a, "filters"))?;
    let mut sorts = vec![];
    for sp in arr_of(a, "sorts") {
        let pv = sp.get("prop").cloned().unwrap_or(json!("title"));
        let prop = resolve_prop(ws, db, &pv).ok_or_else(|| format!("Unknown sort property {pv}."))?;
        sorts.push(Sort { prop, asc: bool_of(sp, "asc").unwrap_or(true) });
    }
    let vid = ws.add_view(db, kind);
    if let Some(v) = ws.db_mut(db).and_then(|d| d.views.iter_mut().find(|v| v.id == vid)) {
        if let Some(n) = str_of(a, "name") {
            v.name = n.to_owned();
        }
        if let Some(g) = group {
            v.group = g;
        }
        v.filters = filters;
        v.sorts = sorts;
    }
    Ok(json!({"database": db, "view": vid}))
}

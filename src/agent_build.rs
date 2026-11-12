//! Agent tools for the "big" objects: dashboards, canvases, flows, automations, scripts. Registered by `agent_tools::builtin()`.

use serde_json::{json, Value as Json};

use crate::agent_tools::*;
use crate::auth::now_ms;
use crate::automation::{self as au, Action, ChangeTo, Freq, Reminder, Repeat, Rule, Script, Setter, Trigger};
use crate::dashcfg::{self, Agg, Bucket, Cfg, Range, Widget};
use crate::dashwidgets as dw;
use crate::dates;
use crate::flow::{self, Flow};
use crate::model::*;

type Tool = (ToolDef, Handler);

pub fn tools() -> Vec<Tool> {
    use Risk::*;
    let t = ToolDef::new;
    let widget = obj(
        json!({
            "kind": s("Widget kind — see list_widget_kinds (bar, line, pie, number, table, board, tasks, notes …)"),
            "title": s("Heading"), "span": n("Grid columns 1-12"), "rows": n("Height 1-8"),
            "database": page_ref("Data source"), "group": s("Property to group by"), "stack": s("Second grouping (stacked)"), "value": s("Number property to aggregate"),
            "agg": s("count|filled|unique|sum|avg|min|max|median|percent"), "bucket": s("day|week|month|quarter|year"), "date": s("Date property"),
            "range": s("all|today|last7|last30|last90|last365|this_week|this_month|this_quarter|this_year|next7|next30"),
            "x": s("scatter x property"), "y": s("scatter y property"), "goal": n("Target for gauges/KPIs"), "limit": n("Max categories/rows"),
            "cols": arr(s("property"), "Table/list columns"), "sort": s("Sort property"), "asc": b("Ascending"),
            "filters": arr(obj(json!({"prop": s("property"), "op": s("op"), "value": s("value")}), &["prop"]), "Filters"),
            "text": s("notes widget text"), "items": arr(s("item"), "checklist items"), "target": s("countdown date YYYY-MM-DD"),
            "pages": arr(page_ref("page"), "quicklinks/subpages/pagepreview pages"), "palette": n("0-5"),
        }),
        &["kind"],
    );
    let node = obj(json!({"id": s("Local id used by edges"), "kind": s("note|text|todo|page|link|image|group"), "text": s("Markdown / label"), "x": n("x"), "y": n("y"), "w": n("width"), "h": n("height"), "color": s("1-6"), "url": s("link/image url"), "page": page_ref("page card target")}), &["kind"]);
    let edge = obj(json!({"from": s("local node id"), "to": s("local node id"), "label": s("Edge label"), "dashed": b("Dashed line")}), &["from", "to"]);
    let fnode = obj(json!({"id": s("Local id used by wires"), "kind": s("Node type from list_flow_nodes"), "label": s("Custom title"), "params": json!({"type": "object", "description": "parameter key → string value"})}), &["id", "kind"]);
    let wire = obj(json!({"from": s("local node id"), "to": s("local node id"), "from_port": s("output port name or index"), "to_port": s("input port name or index")}), &["from", "to"]);
    let action = json!({"type": "object", "description": "{type: set|create_row|move_row|rename|append|notify|duplicate|trash|script, …} see knowledge: automations"});
    vec![
        (t("create_dashboard", "Create a page that is a dashboard: a 12-column grid of widgets (KPIs, charts, tables, boards …). One call creates the page and all widgets.", Write, obj(json!({"title": s("Dashboard title"), "parent": page_ref("Parent"), "icon": s("Emoji"), "intro_markdown": s("Optional text above the widgets"), "widgets": arr(widget.clone(), "Widgets in reading order")}), &["title", "widgets"])), tool_create_dashboard),
        (t("add_widgets", "Add widgets to the dashboard of a page (the page becomes a dashboard if it is not one yet).", Write, obj(json!({"page": page_ref("Page"), "widgets": arr(widget, "Widgets")}), &["page", "widgets"])), tool_add_widgets),
        (t("remove_widget", "Remove one widget from a page's dashboard.", Destroy, obj(json!({"page": page_ref("Page"), "widget": n("Widget id from read_page")}), &["page", "widget"])), tool_remove_widget),
        (t("list_widget_kinds", "All dashboard widget kinds with name, description and default size.", Read, obj(json!({}), &[])), tool_widget_kinds),
        (t("create_canvas", "Create a whiteboard page with nodes and connections (mind maps, diagrams, roadmaps).", Write, obj(json!({"title": s("Title"), "parent": page_ref("Parent"), "icon": s("Emoji"), "nodes": arr(node.clone(), "Nodes"), "edges": arr(edge.clone(), "Edges")}), &["title"])), tool_create_canvas),
        (t("canvas_add", "Add nodes and edges to an existing canvas page.", Write, obj(json!({"page": page_ref("Canvas page"), "nodes": arr(node, "Nodes"), "edges": arr(edge, "Edges")}), &["page"])), tool_canvas_add),
        (t("list_flow_nodes", "The catalogue of flow node types with ports, parameters and documentation. Call before create_flow.", Read, obj(json!({"category": s("Optional filter: Input, Logic, Data, Network, Workspace, Output")}), &[])), tool_flow_nodes),
        (t("create_flow", "Create a flow page (node graph) from nodes and wires. Layout is automatic.", Write, obj(json!({"title": s("Title"), "parent": page_ref("Parent"), "icon": s("Emoji"), "nodes": arr(fnode, "Nodes"), "wires": arr(wire, "Wires"), "vars": json!({"type": "object", "description": "flow variables name → text"}), "every_seconds": n("Re-run interval while the app is open (0 = off)")}), &["title", "nodes"])), tool_create_flow),
        (t("flow_problems", "Validate a flow page: unknown nodes, loops, empty graph.", Read, obj(json!({"page": page_ref("Flow page")}), &["page"])), tool_flow_problems),
        (t("run_flow", "Run a flow page. dry=true (default) simulates on a copy of the workspace. Flows with network nodes need the Flow page open in the app.", Destroy, obj(json!({"page": page_ref("Flow page"), "dry": b("Simulate only (default true)")}), &["page"])), tool_run_flow),
        (t("list_automations", "Rules, reminders and saved scripts.", Read, obj(json!({}), &[])), tool_list_automations),
        (t("create_rule", "Create an automation rule (trigger → conditions → actions). See knowledge: automations.", Write, obj(json!({"name": s("Rule name"), "database": page_ref("Database the rule watches (optional for schedules)"), "trigger": any("\"row_added\" | \"manual\" | {type:'prop_changed',prop,to?} | {type:'date_reached',prop,offset_days,time} | {type:'schedule',every,interval,time,weekdays}"), "conditions": arr(obj(json!({"prop": s("property"), "op": s("op"), "value": s("value")}), &["prop"]), "Only when all match"), "actions": arr(action, "Actions in order")}), &["name", "trigger", "actions"])), tool_create_rule),
        (t("set_rule_enabled", "Turn a rule on or off.", Write, obj(json!({"rule": n("Rule id"), "on": b("Enabled")}), &["rule", "on"])), tool_rule_enabled),
        (t("delete_rule", "Delete a rule.", Destroy, obj(json!({"rule": n("Rule id")}), &["rule"])), tool_delete_rule),
        (t("create_reminder", "Create a standalone reminder (notification at a date and time).", Write, obj(json!({"message": s("Text"), "date": s("YYYY-MM-DD"), "time": s("HH:MM (default 09:00)"), "page": page_ref("Page it belongs to"), "lead_minutes": n("Fire this many minutes early")}), &["message", "date"])), tool_create_reminder),
        (t("script_check", "Syntax-check LumenScript code without running it.", Read, obj(json!({"code": s("Script source")}), &["code"])), tool_script_check),
        (t("script_test", "Dry-run LumenScript on a throw-away copy of the workspace; reports output, errors and what it WOULD change.", Read, obj(json!({"code": s("Script source")}), &["code"])), tool_script_test),
        (t("script_run", "Run LumenScript on the real workspace. Always script_test first.", Destroy, obj(json!({"code": s("Script source")}), &["code"])), tool_script_run),
        (t("save_script", "Save a script to the Automations → Scripts library.", Write, obj(json!({"name": s("Name"), "code": s("Script source")}), &["name", "code"])), tool_save_script),
    ]
}

// ───────────────────────────── dashboards ─────────────────────────────

const DATA_KINDS: &[&str] = &["number", "gauge", "bar", "hbar", "stacked", "line", "area", "pie", "donut", "funnel", "radar", "histogram", "scatter", "heatmap", "chart", "table", "list", "board", "calendar", "progress", "quickadd"];

fn parse_agg(s: &str) -> Option<Agg> {
    Some(match s.to_lowercase().as_str() {
        "count" => Agg::Count,
        "filled" => Agg::Filled,
        "unique" => Agg::Unique,
        "sum" => Agg::Sum,
        "avg" | "average" | "mean" => Agg::Avg,
        "min" => Agg::Min,
        "max" => Agg::Max,
        "median" => Agg::Median,
        "percent" | "percentage" => Agg::Percent,
        _ => return None,
    })
}

fn parse_bucket(s: &str) -> Option<Bucket> {
    Some(match s.to_lowercase().as_str() {
        "day" => Bucket::Day,
        "week" => Bucket::Week,
        "month" => Bucket::Month,
        "quarter" => Bucket::Quarter,
        "year" => Bucket::Year,
        _ => return None,
    })
}

fn parse_range(s: &str) -> Option<Range> {
    Some(match s.to_lowercase().replace([' ', '-'], "_").as_str() {
        "all" => Range::All,
        "today" => Range::Today,
        "last7" | "last_7" | "last_7_days" => Range::Last7,
        "last30" | "last_30" | "last_30_days" => Range::Last30,
        "last90" | "last_90" => Range::Last90,
        "last365" | "last_365" | "last_year" => Range::Last365,
        "this_week" => Range::ThisWeek,
        "this_month" => Range::ThisMonth,
        "this_quarter" => Range::ThisQuarter,
        "this_year" => Range::ThisYear,
        "next7" | "next_7" => Range::Next7,
        "next30" | "next_30" => Range::Next30,
        _ => return None,
    })
}

fn make_widget(ws: &Workspace, spec: &Json, default_db: Option<u64>) -> Result<Widget, String> {
    let kind = str_of(spec, "kind").ok_or("every widget needs a kind (see list_widget_kinds)")?.to_lowercase();
    let def = dw::def(&kind);
    if def.is_none() && !dashcfg::PLOT_KINDS.contains(&kind.as_str()) && kind != "chart" {
        let all: Vec<String> = dw::all().iter().map(|d| d.kind.to_owned()).collect();
        return Err(format!("Unknown widget kind “{kind}”. Available: {}", all.join(", ")));
    }
    let (dspan, drows) = def.map(|d| (d.span, d.rows)).unwrap_or((6, 3));
    let mut cfg = Cfg::default();
    let db = match spec.get("database") {
        Some(v) if !v.is_null() => Some(resolve_db(ws, v)?),
        _ if DATA_KINDS.contains(&kind.as_str()) => default_db.or_else(|| dashcfg::best_db(ws)),
        _ => None,
    };
    if let Some(db) = db {
        dashcfg::autofill(ws, &mut cfg, db);
        let prop = |key: &str| -> Result<Option<u64>, String> {
            match spec.get(key) {
                Some(v) if !v.is_null() => resolve_prop(ws, db, v).map(Some).ok_or_else(|| format!("Widget “{kind}”: unknown property {v} for “{key}”.")),
                _ => Ok(None),
            }
        };
        if let Some(p) = prop("group")? {
            cfg.group = p;
        }
        if let Some(p) = prop("stack")? {
            cfg.stack = p;
        }
        if let Some(p) = prop("value")? {
            cfg.value = p;
            if spec.get("agg").is_none() && p != 0 {
                cfg.agg = Agg::Sum;
            }
        }
        if let Some(p) = prop("date")? {
            cfg.date = p;
        }
        if let Some(p) = prop("x")? {
            cfg.x = p;
        }
        if let Some(p) = prop("y")? {
            cfg.y = p;
        }
        if let Some(p) = prop("sort")? {
            cfg.sort = p;
        }
        let cols: Vec<u64> = arr_of(spec, "cols").into_iter().filter_map(|c| resolve_prop(ws, db, c)).collect();
        if !cols.is_empty() {
            cfg.cols = cols;
        }
        cfg.filters = parse_filters(ws, db, &arr_of(spec, "filters"))?;
    }
    if let Some(a) = str_of(spec, "agg") {
        cfg.agg = parse_agg(a).ok_or_else(|| format!("Unknown agg “{a}”."))?;
    }
    if let Some(bk) = str_of(spec, "bucket") {
        cfg.bucket = parse_bucket(bk).ok_or_else(|| format!("Unknown bucket “{bk}”."))?;
    }
    if let Some(r) = str_of(spec, "range") {
        cfg.range = parse_range(r).ok_or_else(|| format!("Unknown range “{r}”."))?;
    }
    if let Some(g) = spec.get("goal").and_then(|v| v.as_f64()) {
        cfg.goal = g;
    }
    if let Some(l) = spec.get("limit").and_then(|v| v.as_u64()) {
        cfg.limit = l.min(500) as u32;
    }
    if let Some(a) = bool_of(spec, "asc") {
        cfg.asc = a;
    }
    if let Some(p) = spec.get("palette").and_then(|v| v.as_u64()) {
        cfg.palette = p.min(5) as u8;
    }
    if let Some(t) = spec.get("text").and_then(|v| v.as_str()) {
        cfg.text = t.to_owned();
    }
    if let Some(t) = str_of(spec, "target") {
        cfg.target = t.to_owned();
    }
    cfg.items = arr_of(spec, "items").into_iter().filter_map(|i| i.as_str()).map(|t| dashcfg::Item { text: t.to_owned(), done: false }).collect();
    cfg.pages = arr_of(spec, "pages").into_iter().filter_map(|p| resolve_page_str(ws, p).ok()).collect();
    let span = spec.get("span").and_then(|v| v.as_u64()).map(|v| v.clamp(1, 12) as u8).unwrap_or(dspan);
    let rows = spec.get("rows").and_then(|v| v.as_u64()).map(|v| v.clamp(1, 8) as u8).unwrap_or(drows);
    let mut w = dashcfg::new_widget(&kind, span, rows, cfg);
    w.title = str_of(spec, "title").unwrap_or("").to_owned();
    Ok(w)
}

fn tool_create_dashboard(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let parent = parent_arg(ws, a)?;
    let specs = arr_of(a, "widgets");
    if specs.is_empty() {
        return Err("widgets is empty — a dashboard needs at least one widget.".into());
    }
    // validate everything first: nothing is created when a widget spec is wrong
    let mut widgets = vec![];
    for (i, sp) in specs.iter().enumerate() {
        widgets.push(make_widget(ws, sp, None).map_err(|e| format!("widget {}: {e}", i + 1))?);
    }
    let id = ws.add_page(parent, PageKind::Doc);
    finish_new(ws, id, a, "📊");
    if let Some(md) = str_of(a, "intro_markdown") {
        let (_, blocks) = md_blocks(ws, md, false);
        if !blocks.is_empty() {
            if let Some(p) = ws.page_mut(id) {
                p.blocks = blocks;
            }
        }
    }
    let title = ws.page(id).map(|p| p.title.clone()).unwrap_or_default();
    let board = ws.dash.ensure_page_board(id, &title);
    let mut ids = vec![];
    for w in widgets {
        ids.push(ws.dash.add_widget_to(board, w));
    }
    Ok(json!({"id": id, "title": title, "widget_ids": ids}))
}

fn tool_add_widgets(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let id = resolve_page(ws, a, "page")?;
    let default_db = ws.db(id).map(|_| id);
    let mut widgets = vec![];
    for (i, sp) in arr_of(a, "widgets").iter().enumerate() {
        widgets.push(make_widget(ws, sp, default_db).map_err(|e| format!("widget {}: {e}", i + 1))?);
    }
    let title = ws.page(id).map(|p| p.title.clone()).unwrap_or_default();
    let board = ws.dash.ensure_page_board(id, &title);
    let ids: Vec<u64> = widgets.into_iter().map(|w| ws.dash.add_widget_to(board, w)).collect();
    Ok(json!({"page": id, "widget_ids": ids}))
}

fn tool_remove_widget(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let id = resolve_page(ws, a, "page")?;
    let wid = a.get("widget").and_then(|v| v.as_u64()).ok_or("widget id required")?;
    let board = ws.dash.page_board(id).ok_or("That page has no dashboard.")?;
    let b = ws.dash.boards.iter_mut().find(|b| b.id == board).ok_or("board missing")?;
    let before = b.widgets.len();
    b.widgets.retain(|w| w.id != wid);
    if b.widgets.len() == before {
        return Err(format!("No widget {wid} on that dashboard."));
    }
    Ok(json!({"page": id, "removed": wid}))
}

fn tool_widget_kinds(_ws: &mut Workspace, _a: &Json) -> Result<Json, String> {
    let list: Vec<Json> = dw::all().iter().map(|d| json!({"kind": d.kind, "name": d.name, "description": d.desc, "default_span": d.span, "default_rows": d.rows})).collect();
    Ok(json!({"widgets": list, "note": "bar, hbar, stacked, line, area, pie, donut, funnel, radar, histogram, scatter, heatmap, gauge and number are plot types of the Chart widget."}))
}

// ───────────────────────────── canvas ─────────────────────────────

fn canvas_json(ws: &Workspace, a: &Json) -> Result<(String, usize), String> {
    let nodes = arr_of(a, "nodes");
    let mut out = vec![];
    let (mut minx, mut miny, mut maxx, mut maxy) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (i, nd) in nodes.iter().enumerate() {
        let kind = str_of(nd, "kind").unwrap_or("note").to_lowercase();
        let (ty, lumen) = match kind.as_str() {
            "note" | "card" => ("text", "Note"),
            "text" | "label" => ("text", "Text"),
            "todo" | "checklist" => ("text", "Todo"),
            "page" => ("text", "Page"),
            "link" | "bookmark" => ("link", "Link"),
            "image" => ("file", "Image"),
            "group" | "frame" => ("group", "Group"),
            other => return Err(format!("node {}: unknown kind “{other}” (note, text, todo, page, link, image, group).", i + 1)),
        };
        let id = str_of(nd, "id").map(|s| s.to_owned()).unwrap_or_else(|| format!("n{i}"));
        let x = nd.get("x").and_then(|v| v.as_f64()).unwrap_or(((i % 3) * 300) as f64);
        let y = nd.get("y").and_then(|v| v.as_f64()).unwrap_or(((i / 3) * 190) as f64);
        let w = nd.get("w").or_else(|| nd.get("width")).and_then(|v| v.as_f64()).unwrap_or(if lumen == "Text" { 220.0 } else { 260.0 });
        let h = nd.get("h").or_else(|| nd.get("height")).and_then(|v| v.as_f64()).unwrap_or(if lumen == "Text" { 48.0 } else { 140.0 });
        minx = minx.min(x);
        miny = miny.min(y);
        maxx = maxx.max(x + w);
        maxy = maxy.max(y + h);
        let text = nd.get("text").or_else(|| nd.get("label")).and_then(|v| v.as_str()).unwrap_or("");
        let mut j = json!({"id": id, "type": ty, "x": x, "y": y, "width": w, "height": h, "lumen": lumen, "text": text});
        if ty == "group" {
            j["label"] = json!(text);
        }
        if let Some(u) = str_of(nd, "url") {
            j["url"] = json!(u);
            if ty == "file" {
                j["file"] = json!(u);
            }
        }
        if let Some(c) = nd.get("color").map(|c| c.to_string().trim_matches('"').to_owned()).filter(|c| matches!(c.as_str(), "1" | "2" | "3" | "4" | "5" | "6")) {
            j["color"] = json!(c);
        }
        if lumen == "Page" {
            if let Some(p) = nd.get("page") {
                j["page"] = json!(resolve_page_str(ws, p)?);
            }
        }
        out.push(j);
    }
    let mut edges = vec![];
    for e in arr_of(a, "edges") {
        let (Some(f), Some(t)) = (e.get("from").map(|v| v.to_string().trim_matches('"').to_owned()), e.get("to").map(|v| v.to_string().trim_matches('"').to_owned())) else { continue };
        let mut j = json!({"id": format!("e{}", edges.len()), "fromNode": f, "toNode": t});
        if let Some(l) = str_of(e, "label") {
            j["label"] = json!(l);
        }
        if bool_of(e, "dashed") == Some(true) {
            j["lumenDashed"] = json!(true);
        }
        edges.push(j);
    }
    let cx = if nodes.is_empty() { 0.0 } else { (minx + maxx) / 2.0 };
    let cy = if nodes.is_empty() { 0.0 } else { (miny + maxy) / 2.0 };
    Ok((json!({"nodes": out, "edges": edges, "cx": cx, "cy": cy}).to_string(), nodes.len()))
}

fn apply_canvas(ws: &mut Workspace, page: u64, a: &Json) -> Result<usize, String> {
    let (text, count) = canvas_json(ws, a)?;
    let v: Json = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let at = egui::pos2(v["cx"].as_f64().unwrap_or(0.0) as f32, v["cy"].as_f64().unwrap_or(0.0) as f32);
    let doc = json!({"nodes": v["nodes"], "edges": v["edges"]}).to_string();
    let c = ws.page_mut(page).and_then(|p| p.canvas.as_mut()).ok_or("That page is not a canvas.")?;
    c.checkpoint();
    let ids = c.import_json(&doc, at);
    if ids.len() != count {
        return Err("The canvas could not import those nodes.".into());
    }
    Ok(ids.len())
}

fn tool_create_canvas(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let parent = parent_arg(ws, a)?;
    canvas_json(ws, a)?; // validate first
    let id = ws.add_canvas(parent);
    finish_new(ws, id, a, crate::lucide::NETWORK);
    let r = if arr_of(a, "nodes").is_empty() { Ok(0) } else { apply_canvas(ws, id, a) };
    match r {
        Ok(n) => Ok(json!({"id": id, "nodes": n})),
        Err(e) => {
            ws.delete_forever(id);
            Err(e)
        }
    }
}

fn tool_canvas_add(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let id = resolve_page(ws, a, "page")?;
    let n = apply_canvas(ws, id, a)?;
    ws.touch(id);
    Ok(json!({"page": id, "added_nodes": n}))
}

// ───────────────────────────── flows ─────────────────────────────

fn tool_flow_nodes(_ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let cat = str_of(a, "category").map(|s| s.to_lowercase());
    let list: Vec<Json> = flow::DEFS
        .iter()
        .filter(|d| cat.as_ref().map_or(true, |c| d.cat.label().to_lowercase() == *c))
        .map(|d| {
            let params: Vec<Json> = d
                .params
                .iter()
                .map(|p| {
                    let mut j = json!({"key": p.key, "label": p.label, "default": p.def});
                    if !p.hint.is_empty() {
                        j["hint"] = json!(p.hint);
                    }
                    if let flow::PK::Choice(c) = p.kind {
                        j["options"] = json!(c);
                    }
                    j
                })
                .collect();
            json!({"kind": d.key, "label": d.label, "category": d.cat.label(), "inputs": d.ins, "outputs": d.outs, "params": params, "doc": d.doc})
        })
        .collect();
    Ok(json!({"nodes": list, "tips": "Params are strings. {{input.field}}, {{vars.name}} templates work in text params. Ports are wired by name."}))
}

fn port(names: &[&str], v: Option<&Json>, what: &str, kind: &str) -> Result<usize, String> {
    match v {
        None | Some(Json::Null) => Ok(0),
        Some(Json::Number(n)) => Ok(n.as_u64().unwrap_or(0) as usize),
        Some(Json::String(s)) => names.iter().position(|n| n.eq_ignore_ascii_case(s)).or_else(|| s.parse::<usize>().ok()).ok_or_else(|| format!("Node type “{kind}” has no {what} port “{s}” (ports: {}).", names.join(", "))),
        _ => Ok(0),
    }
}

fn build_flow(a: &Json) -> Result<Flow, String> {
    let mut f = Flow::default();
    let mut local: std::collections::HashMap<String, u32> = Default::default();
    for (i, nd) in arr_of(a, "nodes").iter().enumerate() {
        let kind = str_of(nd, "kind").ok_or_else(|| format!("node {}: kind is required", i + 1))?;
        let nid = f.add_node(kind, 0.0, 0.0).ok_or_else(|| format!("node {}: unknown node type “{kind}”. Call list_flow_nodes.", i + 1))?;
        let key = str_of(nd, "id").map(|s| s.to_owned()).unwrap_or_else(|| format!("n{i}"));
        local.insert(key, nid);
        if let Some(n) = f.node_mut(nid) {
            if let Some(l) = str_of(nd, "label") {
                n.label = l.to_owned();
            }
            if let Some(m) = nd.get("params").and_then(|p| p.as_object()) {
                for (k, v) in m {
                    n.params.insert(k.clone(), match v {
                        Json::String(s) => s.clone(),
                        o => o.to_string(),
                    });
                }
            }
        }
    }
    for (i, w) in arr_of(a, "wires").iter().enumerate() {
        let name = |k: &str| w.get(k).map(|v| v.to_string().trim_matches('"').to_owned()).unwrap_or_default();
        let (fl, tl) = (name("from"), name("to"));
        let (Some(&from), Some(&to)) = (local.get(&fl), local.get(&tl)) else {
            return Err(format!("wire {}: unknown node id “{fl}” or “{tl}” (ids: {}).", i + 1, local.keys().cloned().collect::<Vec<_>>().join(", ")));
        };
        let fk = f.node(from).map(|n| n.kind.clone()).unwrap_or_default();
        let tk = f.node(to).map(|n| n.kind.clone()).unwrap_or_default();
        let outs: Vec<&str> = flow::def(&fk).map(|d| d.outs.to_vec()).unwrap_or_default();
        let ins: Vec<&str> = flow::def(&tk).map(|d| d.ins.to_vec()).unwrap_or_default();
        let fp = port(&outs, w.get("from_port"), "output", &fk)?;
        let tp = port(&ins, w.get("to_port"), "input", &tk)?;
        f.connect(from, fp, to, tp).map_err(|e| format!("wire {} ({fl} → {tl}): {e}", i + 1))?;
    }
    if let Some(m) = a.get("vars").and_then(|v| v.as_object()) {
        for (k, v) in m {
            f.vars.insert(k.clone(), v.as_str().map(|s| s.to_owned()).unwrap_or_else(|| v.to_string()));
        }
    }
    f.every = a.get("every_seconds").and_then(|v| v.as_u64()).unwrap_or(0).min(86_400) as u32;
    f.auto_layout();
    Ok(f)
}

fn tool_create_flow(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let parent = parent_arg(ws, a)?;
    let f = build_flow(a)?;
    let problems = f.problems();
    let id = ws.add_flow(parent);
    finish_new(ws, id, a, crate::lucide::ZAP);
    if let Some(p) = ws.page_mut(id) {
        p.flow = Some(f);
    }
    Ok(json!({"id": id, "problems": problems}))
}

fn flow_of<'a>(ws: &'a Workspace, a: &Json) -> Result<(u64, &'a Flow), String> {
    let id = resolve_page(ws, a, "page")?;
    let f = ws.page(id).and_then(|p| p.flow.as_ref()).ok_or("That page is not a flow.")?;
    Ok((id, f))
}

fn tool_flow_problems(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let (id, f) = flow_of(ws, a)?;
    Ok(json!({"page": id, "problems": f.problems(), "nodes": f.nodes.len(), "wires": f.wires.len()}))
}

fn tool_run_flow(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let (id, f) = flow_of(ws, a)?;
    let f = f.clone();
    let dry = bool_of(a, "dry").unwrap_or(true);
    let name = ws.page(id).map(|p| p.title.clone()).unwrap_or_default();
    let problems = f.problems();
    if !problems.is_empty() {
        return Err(format!("The flow has problems: {}", problems.join("; ")));
    }
    let mut run = flow::Run::start(&f, &name, None, dry, ws, now_ms());
    let t0 = now_ms();
    let mut guard = 0;
    while !run.finished && guard < 20_000 {
        run.step(ws, now_ms());
        guard += 1;
        if !run.finished && run.waiting() {
            if cfg!(target_arch = "wasm32") || now_ms() - t0 > 6000.0 {
                break;
            }
            #[cfg(not(target_arch = "wasm32"))]
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    }
    let unfinished = !run.finished;
    if unfinished {
        run.cancel();
    }
    Ok(json!({
        "page": id, "dry": dry, "finished": !unfinished, "errors": run.errors(), "nodes_done": run.count_done(),
        "results": run.results.iter().map(|(k, v)| json!({"label": k, "value": v})).collect::<Vec<_>>(),
        "log": run.log,
        "note": if unfinished { "The flow was waiting for the network. Open the flow page in the app and press Run to execute network nodes." } else { "" },
    }))
}

// ───────────────────────────── automations ─────────────────────────────

fn tool_list_automations(ws: &mut Workspace, _a: &Json) -> Result<Json, String> {
    let rules: Vec<Json> = ws.auto.rules.iter().map(|r| json!({"id": r.id, "name": r.name, "enabled": r.on, "database": r.db, "trigger": r.trigger.kind_label(), "actions": r.actions.iter().map(|a| a.kind_label()).collect::<Vec<_>>(), "runs": r.runs})).collect();
    let rem: Vec<Json> = ws.auto.reminders.iter().map(|r| json!({"id": r.id, "message": r.message, "date": dates::iso_of_day(r.day), "time": dates::hhmm(r.minute), "done": r.done, "page": r.page})).collect();
    let scripts: Vec<Json> = ws.auto.scripts.iter().map(|s| json!({"id": s.id, "name": s.name, "runs": s.runs})).collect();
    Ok(json!({"master_switch_on": ws.auto.settings.rules_on, "rules": rules, "reminders": rem, "scripts": scripts}))
}

fn setter_for(ws: &mut Workspace, db: u64, prop: u64, v: &Json) -> Result<Setter, String> {
    if let Some(sv) = v.as_str() {
        let t = sv.trim().to_lowercase();
        if t == "today" {
            return Ok(Setter::Today);
        }
        if t == "clear" || t == "empty" {
            return Ok(Setter::Clear);
        }
        if let Some(days) = t.strip_suffix('d').and_then(|x| x.parse::<i32>().ok()).filter(|_| t.starts_with('+') || t.starts_with('-')) {
            return Ok(Setter::InDays(days));
        }
    }
    let kind = ws.db(db).map(|d| d.kind_of(prop)).unwrap_or(PropKind::Text);
    let text = match v {
        Json::String(s) => s.clone(),
        o => o.to_string().trim_matches('"').to_owned(),
    };
    Ok(match kind {
        PropKind::Select => Setter::Sel(ensure_opt(ws, db, prop, &text)),
        PropKind::MultiSelect => Setter::AddOpt(ensure_opt(ws, db, prop, &text)),
        PropKind::Checkbox => Setter::Check(matches!(text.to_lowercase().as_str(), "true" | "yes" | "1" | "checked" | "done")),
        PropKind::Number => Setter::Num(text.parse().map_err(|_| format!("“{text}” is not a number."))?),
        _ => Setter::Text(text),
    })
}

fn parse_action(ws: &mut Workspace, db: u64, a: &Json) -> Result<Action, String> {
    let ty = str_of(a, "type").ok_or("an action needs a type")?.to_lowercase();
    let find_prop = |ws: &Workspace, d: u64, v: Option<&Json>| -> Result<u64, String> {
        let v = v.ok_or("this action needs a prop")?;
        resolve_prop(ws, d, v).filter(|p| *p != 0).ok_or_else(|| format!("Unknown property {v}."))
    };
    Ok(match ty.as_str() {
        "set" => {
            let prop = find_prop(ws, db, a.get("prop"))?;
            let value = setter_for(ws, db, prop, a.get("value").unwrap_or(&Json::Null))?;
            Action::Set { prop, value }
        }
        "create_row" => {
            let target = match a.get("database") {
                Some(v) if !v.is_null() => resolve_db(ws, v)?,
                _ => db,
            };
            let mut sets = vec![];
            if let Some(m) = a.get("values").and_then(|v| v.as_object()) {
                for (k, v) in m {
                    let p = find_prop(ws, target, Some(&json!(k)))?;
                    sets.push((p, setter_for(ws, target, p, v)?));
                }
            }
            Action::CreateRow { db: target, title: str_of(a, "title").unwrap_or("{title}").to_owned(), sets }
        }
        "move_row" => Action::MoveRow { db: resolve_db_arg(ws, a, "database")? },
        "rename" => Action::Rename { title: str_of(a, "title").ok_or("rename needs a title")?.to_owned() },
        "append" => Action::Append { text: str_of(a, "text").ok_or("append needs text")?.to_owned(), todo: bool_of(a, "todo").unwrap_or(false) },
        "notify" => Action::Notify { message: str_of(a, "message").ok_or("notify needs a message")?.to_owned() },
        "duplicate" => Action::Duplicate,
        "trash" => Action::Trash,
        "script" => {
            let code = str_of(a, "code").ok_or("script needs code")?;
            crate::script::check(code).map_err(|e| format!("script: {e}"))?;
            Action::Script { code: code.to_owned() }
        }
        other => return Err(format!("Unknown action type “{other}” (set, create_row, move_row, rename, append, notify, duplicate, trash, script).")),
    })
}

fn weekday_mask(list: &[&Json]) -> u8 {
    let mut m = 0u8;
    for d in list {
        let s = d.as_str().unwrap_or("").to_lowercase();
        let i = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"].iter().position(|x| s.starts_with(x));
        if let Some(i) = i {
            m |= 1 << i;
        } else if let Some(n) = d.as_u64().filter(|n| (1..=7).contains(n)) {
            m |= 1 << (n - 1);
        }
    }
    m
}

fn parse_trigger(ws: &mut Workspace, db: u64, t: &Json) -> Result<Trigger, String> {
    let ty = match t {
        Json::String(s) => s.to_lowercase(),
        o => str_of(o, "type").ok_or("trigger needs a type")?.to_lowercase(),
    };
    let prop = |ws: &Workspace| -> Result<u64, String> {
        let v = t.get("prop").ok_or("this trigger needs a prop")?;
        resolve_prop(ws, db, v).filter(|p| *p != 0).ok_or_else(|| format!("Unknown property {v}."))
    };
    let time = |t: &Json| -> u16 { str_of(t, "time").and_then(dates::parse_hhmm).unwrap_or(9 * 60) };
    Ok(match ty.as_str() {
        "row_added" => Trigger::RowAdded,
        "manual" => Trigger::Manual,
        "prop_changed" => {
            let p = prop(ws)?;
            let kind = ws.db(db).map(|d| d.kind_of(p)).unwrap_or(PropKind::Text);
            let to = match t.get("to") {
                None | Some(Json::Null) => ChangeTo::Any,
                Some(v) => {
                    let text = v.as_str().map(|s| s.to_owned()).unwrap_or_else(|| v.to_string());
                    match kind {
                        PropKind::Select => ChangeTo::Sel(ensure_opt(ws, db, p, &text)),
                        PropKind::MultiSelect => ChangeTo::Has(ensure_opt(ws, db, p, &text)),
                        PropKind::Checkbox => ChangeTo::Check(matches!(text.to_lowercase().as_str(), "true" | "yes" | "1" | "checked")),
                        _ => ChangeTo::Any,
                    }
                }
            };
            Trigger::PropChanged { prop: p, to }
        }
        "date_reached" => Trigger::DateReached { prop: prop(ws)?, offset_days: t.get("offset_days").and_then(|v| v.as_i64()).unwrap_or(0) as i32, minute: time(t) },
        "schedule" => {
            let every = str_of(t, "every").unwrap_or("daily").to_lowercase();
            let freq = match every.as_str() {
                "daily" | "day" => Freq::Daily,
                "weekly" | "week" => Freq::Weekly,
                "monthly" | "month" => Freq::Monthly,
                "yearly" | "year" => Freq::Yearly,
                o => return Err(format!("Unknown schedule “{o}” (daily, weekly, monthly, yearly).")),
            };
            let repeat = Repeat { freq, every: t.get("interval").and_then(|v| v.as_u64()).unwrap_or(1).clamp(1, 365) as u32, weekdays: weekday_mask(&arr_of(t, "weekdays")), ..Repeat::default() };
            let start_day = str_of(t, "start").and_then(dates::parse_one).map(dates::day_of).unwrap_or_else(dates::today_day);
            Trigger::Schedule { repeat, start_day, minute: time(t) }
        }
        other => return Err(format!("Unknown trigger “{other}” (row_added, manual, prop_changed, date_reached, schedule).")),
    })
}

fn tool_create_rule(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let name = str_of(a, "name").ok_or("name is required")?.to_owned();
    let db = match a.get("database") {
        Some(v) if !v.is_null() => resolve_db(ws, v)?,
        _ => 0,
    };
    let trigger = parse_trigger(ws, db, a.get("trigger").ok_or("trigger is required")?)?;
    if db == 0 && !matches!(trigger, Trigger::Schedule { .. }) {
        return Err("This trigger needs a database.".into());
    }
    let mut actions = vec![];
    for (i, act) in arr_of(a, "actions").into_iter().enumerate() {
        actions.push(parse_action(ws, db, act).map_err(|e| format!("action {}: {e}", i + 1))?);
    }
    if actions.is_empty() {
        return Err("actions is empty".into());
    }
    let conds = if db != 0 { parse_filters(ws, db, &arr_of(a, "conditions"))? } else { vec![] };
    let id = ws.id();
    let mut rule = Rule::new(id, &name, db, trigger, actions);
    rule.conds = conds;
    ws.auto.rules.push(rule);
    Ok(json!({"rule": id, "name": name}))
}

fn tool_rule_enabled(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let id = a.get("rule").and_then(|v| v.as_u64()).ok_or("rule id required")?;
    let on = bool_of(a, "on").unwrap_or(true);
    let r = ws.auto.rules.iter_mut().find(|r| r.id == id).ok_or_else(|| format!("No rule {id}."))?;
    r.on = on;
    Ok(json!({"rule": id, "enabled": on}))
}

fn tool_delete_rule(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let id = a.get("rule").and_then(|v| v.as_u64()).ok_or("rule id required")?;
    let before = ws.auto.rules.len();
    ws.auto.rules.retain(|r| r.id != id);
    if ws.auto.rules.len() == before {
        return Err(format!("No rule {id}."));
    }
    Ok(json!({"deleted": id}))
}

fn tool_create_reminder(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let message = str_of(a, "message").ok_or("message required")?.to_owned();
    let day = str_of(a, "date").and_then(dates::parse_one).map(dates::day_of).ok_or("date must be YYYY-MM-DD")?;
    let minute = str_of(a, "time").and_then(dates::parse_hhmm).unwrap_or(9 * 60);
    let page = match a.get("page") {
        Some(v) if !v.is_null() => resolve_page_str(ws, v)?,
        _ => 0,
    };
    let lead = a.get("lead_minutes").and_then(|v| v.as_u64()).unwrap_or(ws.auto.settings.default_lead as u64) as u32;
    let id = ws.id();
    ws.auto.reminders.push(Reminder { id, page, prop: 0, day, minute, lead_min: lead, message, repeat: None, fired_for: 0, done: false });
    Ok(json!({"reminder": id}))
}

// ───────────────────────────── scripts ─────────────────────────────

fn code_of(a: &Json) -> Result<&str, String> {
    str_of(a, "code").ok_or_else(|| "code is required".to_owned())
}

fn tool_script_check(_ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    match crate::script::check(code_of(a)?) {
        Ok(()) => Ok(json!({"ok": true})),
        Err(e) => Ok(json!({"ok": false, "error": e.to_string(), "line": e.line, "column": e.col})),
    }
}

fn ctx() -> crate::scripthost::ScriptCtx {
    crate::scripthost::ScriptCtx { source: "AI agent".into(), now: now_ms(), ..Default::default() }
}

fn report_json(r: crate::scripthost::Report) -> Json {
    json!({"ok": r.error.is_none(), "error": r.error.map(|e| e.to_string()), "output": r.output, "steps": r.steps, "effects": r.effects})
}

fn tool_script_test(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    Ok(report_json(crate::scripthost::dry_run(ws, code_of(a)?, &ctx())))
}

fn tool_script_run(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let code = code_of(a)?.to_owned();
    crate::script::check(&code).map_err(|e| format!("Syntax error: {e}"))?;
    let mut out = au::Output::default();
    let mut follow = vec![];
    let rep = crate::scripthost::run_script(ws, &code, &ctx(), &mut out, &mut follow);
    Ok(report_json(rep))
}

fn tool_save_script(ws: &mut Workspace, a: &Json) -> Result<Json, String> {
    let code = code_of(a)?.to_owned();
    crate::script::check(&code).map_err(|e| format!("Syntax error: {e}"))?;
    let name = str_of(a, "name").ok_or("name required")?.to_owned();
    let id = ws.id();
    ws.auto.scripts.push(Script { id, name, code, runs: 0, last_run: 0.0 });
    Ok(json!({"script": id}))
}

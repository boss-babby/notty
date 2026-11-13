//! One-click "live data" demo: a mock API stream → flow (every second, in the background) → database → live dashboard.
//!
//! Everything is built with the same tool registry the AI agent uses, so the demo doubles as a worked example for agents.

use serde_json::{json, Value as Json};

use crate::agent_tools::{self as tools, Access};
use crate::model::Workspace;

pub const DB_NAME: &str = "Live Telemetry";

const FEED_SCRIPT: &str = "// one record per second from the mock API node (a); a ring buffer of 60 rows keeps the workspace small\nlet s = a.time.slice(6, 8).to_num()\nlet d = db(\"Live Telemetry\")\nlet vals = {title: a.time, Time: a.time, Slot: s, Temperature: a.temperature, Humidity: a.humidity, Pressure: a.pressure, Battery: a.battery, Status: a.status}\nlet hit = d.find(\"Slot\", s)\nif hit == null { d.add(vals) } else { hit.update(vals) }\nemit(a)";

fn id_of(text: &str) -> Option<u64> {
    serde_json::from_str::<Json>(text).ok().and_then(|v| v["id"].as_u64())
}

fn run(ws: &mut Workspace, tool: &str, args: Json) -> Result<u64, String> {
    let o = tools::call(ws, tool, &args, Access::Full);
    if !o.ok {
        return Err(format!("{tool}: {}", o.text));
    }
    Ok(id_of(&o.text).unwrap_or(0))
}

/// Creates (or re-creates) the demo. Returns the page of the dashboard.
pub fn build(ws: &mut Workspace) -> Result<u64, String> {
    // an older copy of the demo is replaced, not duplicated
    let old: Vec<u64> = ws.pages.iter().filter(|p| !p.trashed && !p.row && (p.title == DB_NAME || p.title == "Live feed (mock API, every second)" || p.title == "Live dashboard")).map(|p| p.id).collect();
    for id in old {
        if let Some(p) = ws.page_mut(id) {
            p.trashed = true;
        }
    }
    run(
        ws,
        "create_database",
        json!({
            "title": DB_NAME, "icon": "📡", "view": "table",
            "properties": [
                {"name": "Time", "type": "text"}, {"name": "Slot", "type": "number"}, {"name": "Temperature", "type": "number"},
                {"name": "Humidity", "type": "number"}, {"name": "Pressure", "type": "number"}, {"name": "Battery", "type": "number"},
                {"name": "Status", "type": "select", "options": ["ok", "warn"]}
            ]
        }),
    )?;
    run(
        ws,
        "create_flow",
        json!({
            "title": "Live feed (mock API, every second)", "icon": "⚡", "every_seconds": 1,
            "nodes": [
                {"id": "api", "kind": "mock_api", "label": "Mock sensor API", "params": {"preset": "Sensor telemetry"}},
                {"id": "store", "kind": "code", "label": "Store latest 60 readings", "params": {"code": FEED_SCRIPT}}
            ],
            "wires": [{"from": "api", "to": "store", "to_port": "a"}]
        }),
    )?;
    let d = DB_NAME;
    let kpi = |t: &str, v: &str, agg: &str, span: u32| json!({"kind": "number", "title": t, "span": span, "rows": 1, "database": d, "value": v, "agg": agg});
    let dash = run(
        ws,
        "create_dashboard",
        json!({
            "title": "Live dashboard", "icon": "📈",
            "intro_markdown": "A mock API stream feeds this dashboard **every second** from a flow that keeps running in the background — switch pages and come back, the numbers keep moving. Open the flow “Live feed” to change the stream, or replace the *Mock API* node with an *HTTP request* to use a real API.",
            "widgets": [
                kpi("Avg temperature °C", "Temperature", "avg", 3), kpi("Avg humidity %", "Humidity", "avg", 3),
                kpi("Peak pressure hPa", "Pressure", "max", 3), kpi("Lowest battery %", "Battery", "min", 3),
                {"kind": "line", "title": "Temperature (last 60 s)", "span": 6, "rows": 3, "database": d, "group": "Time", "value": "Temperature", "agg": "avg", "limit": 60},
                {"kind": "area", "title": "Humidity (last 60 s)", "span": 6, "rows": 3, "database": d, "group": "Time", "value": "Humidity", "agg": "avg", "limit": 60},
                {"kind": "donut", "title": "Status", "span": 4, "rows": 3, "database": d, "group": "Status", "agg": "count"},
                {"kind": "bar", "title": "Battery by reading", "span": 8, "rows": 3, "database": d, "group": "Time", "value": "Battery", "agg": "avg", "limit": 30},
                {"kind": "table", "title": "Latest readings", "span": 12, "rows": 4, "database": d, "cols": ["Time", "Temperature", "Humidity", "Pressure", "Status"], "sort": "Time", "asc": false}
            ]
        }),
    )?;
    ws.touch(dash);
    Ok(dash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_builds_and_the_feed_script_runs() {
        let mut ws = Workspace::seed();
        let dash = build(&mut ws).expect("demo builds");
        assert!(ws.page(dash).is_some());
        let flow_page = ws.pages.iter().find(|p| p.flow.is_some() && p.title.starts_with("Live feed")).expect("flow page");
        let f = flow_page.flow.clone().unwrap();
        assert_eq!(f.every, 1);
        assert!(f.problems().is_empty(), "{:?}", f.problems());
        // run the flow a few "seconds" and check the ring buffer
        let name = flow_page.title.clone();
        for sec in 0..3 {
            let now = 1_700_000_000_000.0 + sec as f64 * 1000.0;
            let mut run = crate::flow::Run::start(&f, &name, None, false, &ws, now);
            let mut n = 0;
            while !run.finished && n < 200 {
                run.step(&mut ws, now);
                n += 1;
            }
            assert!(run.finished && run.errors() == 0, "run {sec}: {:?}", run.log);
        }
        let db = ws.pages.iter().find(|p| p.title == DB_NAME).unwrap().id;
        assert_eq!(ws.rows(db).len(), 3);
    }
}

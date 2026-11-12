//! Tests of the agent tool layer, the MCP protocol core and the provider adapters.
use serde_json::{json, Value as Json};

use crate::agent_tools::{call, tool_defs, Access};
use crate::mcp;
use crate::model::Workspace;

fn run(ws: &mut Workspace, name: &str, args: Json) -> Json {
    let o = call(ws, name, &args, Access::Full);
    assert!(o.ok, "{name} failed: {}", o.text);
    serde_json::from_str(&o.text).unwrap_or(Json::Null)
}

#[test]
fn agent_builds_a_whole_system() {
    let mut ws = Workspace::seed();
    let before = ws.pages.len();
    let page = run(&mut ws, "create_page", json!({"title": "Hub", "icon": "🏠", "markdown": "# ignored\n## Goals\n- [ ] one\n- two\n!> note\n[toc]\n```rust\nlet a = 1;\n```"}));
    let hub = page["id"].as_u64().unwrap();
    assert_eq!(ws.page(hub).unwrap().title, "Hub");
    assert!(ws.page(hub).unwrap().blocks.len() >= 5);

    let db = run(&mut ws, "create_database", json!({
        "title": "Tasks", "parent": hub, "view": "board",
        "properties": [{"name": "Status", "type": "select", "options": ["Todo", "Doing", "Done"]}, {"name": "Due", "type": "date"}, {"name": "Hours", "type": "number"}, {"name": "Tags", "type": "multi_select"}],
        "rows": [{"title": "Write spec", "values": {"Status": "Doing", "Due": "2030-01-15", "Hours": 3, "Tags": ["a", "b"]}}, {"title": "Ship", "values": {"Status": "New option"}}]
    }));
    let dbid = db["id"].as_u64().unwrap();
    assert_eq!(ws.rows(dbid).len(), 2);
    assert_eq!(ws.db(dbid).unwrap().props.len(), 4);
    let q = run(&mut ws, "query_database", json!({"database": "Tasks", "where": {"Status": "doing"}}));
    assert_eq!(q["matched"], 1);
    run(&mut ws, "add_rows", json!({"database": dbid, "rows": [{"title": "Third", "values": {"Due": "2030-02-01/2030-02-09"}}]}));
    run(&mut ws, "add_view", json!({"database": dbid, "kind": "calendar", "filters": [{"prop": "Status", "op": "is_not", "value": "Done"}]}));

    // validation: bad property must not create anything
    let n = ws.pages.len();
    let o = call(&mut ws, "create_database", &json!({"title": "Bad", "rows": [{"title": "x", "values": {"Nope": 1}}]}), Access::Full);
    assert!(!o.ok);
    assert_eq!(ws.pages.len(), n, "failed create_database must leave nothing behind");

    let dash = run(&mut ws, "create_dashboard", json!({"title": "Overview", "parent": hub, "widgets": [
        {"kind": "number", "title": "Open", "database": "Tasks", "span": 3, "rows": 1},
        {"kind": "bar", "title": "By status", "database": "Tasks", "group": "Status", "span": 6},
        {"kind": "table", "database": "Tasks", "cols": ["Status", "Due"], "span": 12},
        {"kind": "notes", "text": "hello", "span": 3}]}));
    let did = dash["id"].as_u64().unwrap();
    assert_eq!(ws.dash.page_board(did).map(|b| ws.dash.boards.iter().find(|x| x.id == b).unwrap().widgets.len()), Some(4));
    let bad = call(&mut ws, "create_dashboard", &json!({"title": "X", "widgets": [{"kind": "nonsense"}]}), Access::Full);
    assert!(!bad.ok && bad.text.contains("Unknown widget kind"));

    let cv = run(&mut ws, "create_canvas", json!({"title": "Map", "nodes": [{"id": "a", "kind": "note", "text": "Root", "x": 0, "y": 0}, {"id": "b", "kind": "note", "text": "Child", "x": 400, "y": 0, "color": "4"}], "edges": [{"from": "a", "to": "b", "label": "leads to"}]}));
    let c = ws.page(cv["id"].as_u64().unwrap()).unwrap().canvas.as_ref().unwrap();
    assert_eq!((c.nodes.len(), c.edges.len()), (2, 1));

    let fl = run(&mut ws, "create_flow", json!({"title": "Flow", "nodes": [{"id": "n", "kind": "now"}, {"id": "o", "kind": "output", "params": {"label": "Time"}}], "wires": [{"from": "n", "to": "o"}]}));
    assert_eq!(fl["problems"].as_array().unwrap().len(), 0);
    let r = run(&mut ws, "run_flow", json!({"page": fl["id"], "dry": true}));
    assert_eq!(r["finished"], true);
    assert!(!call(&mut ws, "create_flow", &json!({"title": "F", "nodes": [{"id": "x", "kind": "does_not_exist"}]}), Access::Full).ok);

    run(&mut ws, "create_rule", json!({"name": "Stamp", "database": "Tasks", "trigger": {"type": "prop_changed", "prop": "Status", "to": "Done"}, "actions": [{"type": "set", "prop": "Due", "value": "today"}, {"type": "notify", "message": "Done!"}]}));
    run(&mut ws, "create_rule", json!({"name": "Daily", "trigger": {"type": "schedule", "every": "weekly", "time": "08:30", "weekdays": ["mon", "fri"]}, "actions": [{"type": "notify", "message": "Hi"}]}));
    assert_eq!(ws.auto.rules.len(), 2);
    run(&mut ws, "create_reminder", json!({"message": "Call", "date": "2030-03-03", "time": "10:15"}));

    let t = run(&mut ws, "script_test", json!({"code": "let n = 0\nfor r in db(\"Tasks\").rows() { n += 1 }\nprint(n)"}));
    assert_eq!(t["ok"], true);
    assert_eq!(run(&mut ws, "script_check", json!({"code": "let = "}))["ok"], false);

    let ov = run(&mut ws, "workspace_overview", json!({}));
    assert!(ov["counts"]["databases"].as_u64().unwrap() >= 1);
    let rp = run(&mut ws, "read_page", json!({"page": "Tasks"}));
    assert_eq!(rp["kind"], "database");
    assert!(ws.pages.len() > before);
    run(&mut ws, "trash_page", json!({"page": hub}));
    assert!(ws.page(hub).unwrap().trashed);
}

#[test]
fn access_levels_are_enforced() {
    let mut ws = Workspace::seed();
    assert!(!call(&mut ws, "create_page", &json!({"title": "x"}), Access::ReadOnly).ok);
    assert!(call(&mut ws, "create_page", &json!({"title": "x"}), Access::Edit).ok);
    let id = ws.pages.last().unwrap().id;
    assert!(!call(&mut ws, "trash_page", &json!({"page": id}), Access::Edit).ok);
    assert!(call(&mut ws, "trash_page", &json!({"page": id}), Access::Full).ok);
    assert!(call(&mut ws, "workspace_overview", &json!({}), Access::ReadOnly).ok);
}

#[test]
fn every_tool_has_a_valid_schema_and_knowledge_exists() {
    let defs = tool_defs();
    assert!(defs.len() >= 35, "{}", defs.len());
    for d in &defs {
        assert_eq!(d.schema["type"], "object", "{}", d.name);
        assert!(!d.description.is_empty());
    }
    let mut ws = Workspace::seed();
    let k = run(&mut ws, "get_knowledge", json!({}));
    assert!(k["modules"].as_array().unwrap().len() >= 10);
    let s = run(&mut ws, "get_knowledge", json!({"module": "scripting"}));
    assert!(s["content"].as_str().unwrap().contains("LumenScript"));
    // every documented tool name in the knowledge base exists
    for m in crate::agent_tools::knowledge() {
        for w in m.body.split('`') {
            if w.starts_with("create_") || w.starts_with("add_") || w.starts_with("list_") {
                let name: String = w.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if name.len() > 5 && w.len() == name.len() {
                    assert!(defs.iter().any(|d| d.name == name), "knowledge `{}` mentions unknown tool {name}", m.id);
                }
            }
        }
    }
}

#[test]
fn mcp_protocol_core() {
    let mut ws = Workspace::seed();
    let init = mcp::handle(&mut ws, r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#, Access::Edit, false).unwrap();
    let v: Json = serde_json::from_str(&init).unwrap();
    assert_eq!(v["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(v["result"]["serverInfo"]["name"], "lumen");
    assert!(mcp::handle(&mut ws, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, Access::Edit, false).is_none());
    let list: Json = serde_json::from_str(&mcp::handle(&mut ws, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#, Access::ReadOnly, false).unwrap()).unwrap();
    let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"read_page") && !names.contains(&"create_page"));
    let call_r: Json = serde_json::from_str(&mcp::handle(&mut ws, r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"create_page","arguments":{"title":"From MCP","markdown":"hi"}}}"#, Access::Edit, false).unwrap()).unwrap();
    assert_eq!(call_r["result"]["isError"], false);
    assert!(ws.pages.iter().any(|p| p.title == "From MCP"));
    let locked: Json = serde_json::from_str(&mcp::handle(&mut ws, r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"workspace_overview"}}"#, Access::Edit, true).unwrap()).unwrap();
    assert_eq!(locked["result"]["isError"], true);
    let res: Json = serde_json::from_str(&mcp::handle(&mut ws, r#"{"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":"lumen://knowledge/overview"}}"#, Access::Edit, false).unwrap()).unwrap();
    assert!(res["result"]["contents"][0]["text"].as_str().unwrap().contains("Lumen"));
    let batch = mcp::handle(&mut ws, r#"[{"jsonrpc":"2.0","id":6,"method":"ping"},{"jsonrpc":"2.0","id":7,"method":"prompts/list"}]"#, Access::Edit, false).unwrap();
    assert_eq!(serde_json::from_str::<Json>(&batch).unwrap().as_array().unwrap().len(), 2);
    let unknown: Json = serde_json::from_str(&mcp::handle(&mut ws, r#"{"jsonrpc":"2.0","id":8,"method":"nope"}"#, Access::Edit, false).unwrap()).unwrap();
    assert_eq!(unknown["error"]["code"], -32601);
    assert!(mcp::handle(&mut ws, "not json", Access::Edit, false).unwrap().contains("-32700"));
}

#[test]
fn extension_registry_works() {
    use crate::agent_tools::{register_knowledge, register_tool, Risk, ToolDef};
    register_tool(ToolDef::new("test_extension_tool", "x", Risk::Read, json!({"type": "object", "properties": {}})), |_, _| Ok(json!({"hello": "world"})));
    register_knowledge("test_ext", "Test", "t", "# body");
    let mut ws = Workspace::seed();
    assert_eq!(run(&mut ws, "test_extension_tool", json!({}))["hello"], "world");
    assert_eq!(run(&mut ws, "get_knowledge", json!({"module": "test_ext"}))["content"], "# body");
}

/// Renders the whole AI UI (chat in every state, settings tabs, narrow sheet) headlessly — catches panics / layout asserts.
#[test]
fn ai_ui_renders_without_panics() {
    use crate::ai::{AiHost, Call, Msg, Role, SettingsTab, ToolRes};
    let ctx = egui::Context::default();
    crate::theme::setup_fonts(&ctx);
    let pal = crate::theme::Palette::mix(0.0);
    let mut ws = Workspace::seed();
    let mut ai = AiHost::default();
    ai.st.mcp.token = "lmn_abcdef0123456789".into();
    let frame = |ctx: &egui::Context, ai: &mut AiHost, ws: &mut Workspace, w: f32| {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, 800.0))), ..Default::default() };
        let _ = ctx.run(input, |ctx| {
            if w < 720.0 {
                crate::aiui::fullscreen(ctx, ai, ws, &pal);
            } else {
                egui::SidePanel::right("t").default_width(400.0).show(ctx, |ui| {
                    crate::aiui::panel(ui, ctx, ai, ws, &pal);
                });
            }
            crate::aiui::settings(ctx, ai, ws, &pal);
        });
    };
    for w in [1100.0, 380.0] {
        frame(&ctx, &mut ai, &mut ws, w);
        ai.thread();
        let t = &mut ai.st.threads[0];
        t.msgs.push(Msg { role: Role::User, text: "Build me a CRM".into(), calls: vec![], results: vec![], raw: None, raw_api: String::new(), error: false });
        t.msgs.push(Msg { role: Role::Assistant, text: "# Plan\n- **one** `two`\n1. first\n> quote\n```\ncode\n```\n| a | b |\n[link](https://x.y)".into(), calls: vec![Call { id: "1".into(), name: "create_page".into(), args: json!({"title": "Hub"}) }], results: vec![], raw: None, raw_api: String::new(), error: false });
        t.msgs.push(Msg { role: Role::Tool, text: String::new(), calls: vec![], results: vec![ToolRes { call_id: "1".into(), name: "create_page".into(), ok: true, text: "{\"id\":1}".into(), open: Some(1) }], raw: None, raw_api: String::new(), error: false });
        t.msgs.push(Msg { role: Role::Assistant, text: "Invalid key (HTTP 401)".into(), calls: vec![], results: vec![], raw: None, raw_api: String::new(), error: true });
        for _ in 0..3 {
            frame(&ctx, &mut ai, &mut ws, w);
        }
        ai.rt.history = true;
        frame(&ctx, &mut ai, &mut ws, w);
        ai.rt.history = false;
        for tab in [SettingsTab::Providers, SettingsTab::Agent, SettingsTab::Connect] {
            ai.rt.settings = Some(tab);
            for p in ["gemini", "custom", "ollama"] {
                ai.rt.sel_provider = p.into();
                frame(&ctx, &mut ai, &mut ws, w);
            }
        }
        ai.rt.settings = None;
        ai.rt.phase = crate::ai::Phase::Approval;
        ai.rt.pending = vec![Call { id: "2".into(), name: "create_database".into(), args: json!({"title": "T", "rows": [1, 2]}) }];
        frame(&ctx, &mut ai, &mut ws, w);
        ai.st.threads.clear();
        ai.rt.phase = crate::ai::Phase::Idle;
        frame(&ctx, &mut ai, &mut ws, w);
    }
}

#[test]
fn provider_requests_are_well_formed() {
    use crate::ai::{AiHost, Msg, Role};
    let ws = Workspace::seed();
    let mut ai = AiHost::default();
    ai.st.cfg_mut("gemini").key = "k".into();
    ai.thread().msgs.push(Msg { role: Role::User, text: "hi".into(), calls: vec![], results: vec![], raw: None, raw_api: String::new(), error: false });
    let _ = &ws;
    // schemas must be Gemini-safe: no type arrays, no empty object properties
    for d in tool_defs() {
        fn walk(v: &Json, name: &str, gemini: bool) {
            if let Json::Object(m) = v {
                if gemini {
                    assert!(!m.get("type").map_or(false, |t| t.is_array()), "{name}: type array");
                }
                for (_, x) in m {
                    walk(x, name, gemini);
                }
            }
        }
        walk(&crate::ai::schema_for_test(&d.schema), &d.name, true);
    }
}

/// End-to-end agent loop against a scripted fake provider (OpenAI, Anthropic and Gemini wire formats).
#[test]
fn agent_loop_runs_against_fake_providers() {
    use crate::ai::{AiHost, Phase, Role};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    for (pid, api) in [("openai", "openai"), ("anthropic", "anthropic"), ("gemini", "gemini")] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let seen2 = seen.clone();
        let handle = std::thread::spawn(move || {
            for n in 0..2 {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = vec![];
                let mut tmp = [0u8; 8192];
                let (head_end, len) = loop {
                    let k = s.read(&mut tmp).unwrap();
                    buf.extend_from_slice(&tmp[..k]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
                        let len = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
                        break (p + 4, len);
                    }
                };
                while buf.len() < head_end + len {
                    let k = s.read(&mut tmp).unwrap();
                    buf.extend_from_slice(&tmp[..k]);
                }
                seen2.lock().unwrap().push(String::from_utf8_lossy(&buf).to_string());
                let args = json!({"title": "Mock page", "markdown": "# ignored\nhello"});
                let reply = match (api, n) {
                    ("openai", 0) => json!({"choices": [{"message": {"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "create_page", "arguments": args.to_string()}}]}}]}),
                    ("openai", _) => json!({"choices": [{"message": {"role": "assistant", "content": "All done."}}]}),
                    ("anthropic", 0) => json!({"type": "message", "content": [{"type": "text", "text": "Creating."}, {"type": "tool_use", "id": "tu1", "name": "create_page", "input": args}]}),
                    ("anthropic", _) => json!({"type": "message", "content": [{"type": "text", "text": "All done."}]}),
                    (_, 0) => json!({"candidates": [{"content": {"role": "model", "parts": [{"functionCall": {"name": "create_page", "args": args}, "thoughtSignature": "sig"}]}}]}),
                    _ => json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "All done."}]}}]}),
                };
                let body = reply.to_string();
                write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
        });

        let ctx = egui::Context::default();
        let mut ws = Workspace::seed();
        let mut ai = AiHost::default();
        ai.st.active = pid.into();
        ai.st.cfg_mut(pid).key = "test-key".into();
        ai.st.cfg_mut(pid).base = format!("http://127.0.0.1:{port}");
        ai.send(&ws, &ctx, "make a page");
        assert_eq!(ai.rt.phase, Phase::Waiting);
        let t0 = std::time::Instant::now();
        while ai.busy() && t0.elapsed().as_secs() < 20 {
            ai.tick(&mut ws, &ctx, false);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        handle.join().unwrap();
        assert!(!ai.busy(), "{pid}: loop did not finish");
        assert!(ws.pages.iter().any(|p| p.title == "Mock page"), "{pid}: page not created");
        let t = ai.thread_ref().unwrap();
        let roles: Vec<Role> = t.msgs.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool, Role::Assistant], "{pid}");
        assert_eq!(t.msgs[3].text, "All done.");
        assert!(t.msgs[2].results[0].ok);
        let reqs = seen.lock().unwrap().clone();
        let first = reqs[0].to_lowercase();
        match api {
            "openai" => assert!(first.contains("authorization: bearer test-key") && first.contains("/chat/completions")),
            "anthropic" => assert!(first.contains("x-api-key: test-key") && first.contains("anthropic-version") && first.contains("/messages")),
            _ => assert!(first.contains("x-goog-api-key: test-key") && first.contains(":generatecontent")),
        }
        assert!(first.contains("create_page") && first.contains("workspace_overview"), "{pid}: tools missing from request");
        let second = reqs[1].as_str();
        match api {
            "openai" => assert!(second.contains("tool_call_id")),
            "anthropic" => assert!(second.contains("tool_result") && second.contains("tool_use_id")),
            _ => assert!(second.contains("functionResponse") && second.contains("thoughtSignature"), "gemini must replay the raw parts"),
        }
        // undo restores the workspace
        assert!(ai.rt.snapshot.is_some());
        assert!(ai.undo(&mut ws));
        assert!(!ws.pages.iter().any(|p| p.title == "Mock page"), "{pid}: undo failed");
    }
}

#[test]
fn approval_flow_and_error_paths() {
    use crate::ai::{AiHost, Phase};
    let ctx = egui::Context::default();
    let mut ws = Workspace::seed();
    let mut ai = AiHost::default();
    // no key → settings opens, nothing is sent
    ai.send(&ws, &ctx, "hello");
    assert_eq!(ai.rt.phase, Phase::Idle);
    assert!(ai.rt.settings.is_some());
    // unreachable provider → an error message in the chat, loop ends
    ai.rt.settings = None;
    ai.st.active = "custom".into();
    ai.st.cfg_mut("custom").base = "http://127.0.0.1:9".into();
    ai.st.cfg_mut("custom").model = "m".into();
    ai.send(&ws, &ctx, "hello");
    let t0 = std::time::Instant::now();
    while ai.busy() && t0.elapsed().as_secs() < 20 {
        ai.tick(&mut ws, &ctx, false);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let last = ai.thread_ref().unwrap().msgs.last().unwrap();
    assert!(last.error, "expected an error message, got {:?}", last.text);
}

//! Cross-module tests: data model, import/export and headless render passes over every page and view type.

use egui::{Id, pos2, vec2, Event, Modifiers, PointerButton, RawInput, Rect};

use crate::dates;
use crate::editor::{self, EditorState};
use crate::io;
use crate::model::*;
use crate::theme::Palette;

#[test]
fn calendar_math() {
    assert_eq!(dates::days_from_civil(1970, 1, 1), 0);
    assert_eq!(dates::civil_from_days(0), (1970, 1, 1));
    assert_eq!(dates::civil_from_days(dates::days_from_civil(2024, 2, 29)), (2024, 2, 29));
    assert_eq!(dates::weekday(2024, 1, 1), 0, "2024-01-01 was a Monday");
    assert_eq!(dates::weekday(2026, 11, 4), 2, "2026-11-04 is a Wednesday");
    assert_eq!(dates::days_in_month(2024, 2), 29);
    assert_eq!(dates::days_in_month(2100, 2), 28);
    assert_eq!(dates::parse("2025-13-01"), None);
    assert_eq!(dates::parse("2025-02-30"), None);
    assert_eq!(dates::pretty("2025-03-07"), "Mar 7, 2025");
}

#[test]
fn seed_is_consistent() {
    let ws = Workspace::seed();
    assert!(ws.current.and_then(|c| ws.page(c)).is_some());
    let mut ids: Vec<u64> = ws.pages.iter().map(|p| p.id).collect();
    let n = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), n, "page ids must be unique");
    let road = ws.pages.iter().find(|p| p.title == "Roadmap").unwrap();
    assert_eq!(road.kind, PageKind::Database);
    assert_eq!(ws.rows(road.id).len(), 7);
    // rows never show up in the sidebar tree
    assert!(ws.children(None).iter().all(|id| !ws.page(*id).unwrap().row));
}

#[test]
fn database_filters_and_sorts() {
    let mut ws = Workspace::seed();
    let road = ws.pages.iter().find(|p| p.title == "Roadmap").unwrap().id;
    let d = ws.db(road).unwrap().clone();
    let status = d.first_of(PropKind::Select).unwrap();
    let prio = d.props.iter().find(|p| p.name == "Priority").unwrap().id;
    let vi = 0;
    // filter: status is "Done"
    ws.db_mut(road).unwrap().views[vi].filters.push(Filter { prop: status, op: FilterOp::Is, value: "Done".into() });
    assert_eq!(ws.query(road, vi).len(), 3);
    // plus title contains "block"
    ws.db_mut(road).unwrap().views[vi].filters.push(Filter { prop: 0, op: FilterOp::Contains, value: "block".into() });
    assert_eq!(ws.query(road, vi).len(), 1);
    // incomplete filters are ignored
    ws.db_mut(road).unwrap().views[vi].filters = vec![Filter { prop: 0, op: FilterOp::Contains, value: String::new() }];
    assert_eq!(ws.query(road, vi).len(), 7);
    // sort by title descending
    ws.db_mut(road).unwrap().views[vi].filters.clear();
    ws.db_mut(road).unwrap().views[vi].sorts = vec![Sort { prop: 0, asc: false }];
    let titles: Vec<String> = ws.query(road, vi).iter().map(|r| ws.page(*r).unwrap().title.clone()).collect();
    let mut sorted = titles.clone();
    sorted.sort_by_key(|t| t.to_lowercase());
    sorted.reverse();
    assert_eq!(titles, sorted);
    // sort by select
    ws.db_mut(road).unwrap().views[vi].sorts = vec![Sort { prop: prio, asc: true }];
    assert_eq!(ws.query(road, vi).len(), 7);
    // empty / not-empty
    ws.db_mut(road).unwrap().views[vi].sorts.clear();
    let tags = d.first_of(PropKind::MultiSelect).unwrap();
    ws.db_mut(road).unwrap().views[vi].filters = vec![Filter { prop: tags, op: FilterOp::NotEmpty, value: String::new() }];
    assert_eq!(ws.query(road, vi).len(), 3);
}

#[test]
fn trash_restore_and_delete() {
    let mut ws = Workspace::seed();
    let road = ws.pages.iter().find(|p| p.title == "Roadmap").unwrap().id;
    let row = ws.rows(road)[0];
    ws.set_trashed(row, true);
    assert_eq!(ws.rows(road).len(), 6);
    ws.set_trashed(row, false);
    assert_eq!(ws.rows(road).len(), 7);
    // trashing a database trashes its rows; restoring brings them back
    ws.set_trashed(road, true);
    assert!(ws.rows(road).is_empty());
    ws.set_trashed(road, false);
    assert_eq!(ws.rows(road).len(), 7);
    // duplicate copies rows with fresh ids
    let copy = ws.duplicate(road).unwrap();
    assert_eq!(ws.rows(copy).len(), 7);
    ws.delete_forever(copy);
    assert!(ws.page(copy).is_none() && ws.rows(copy).is_empty());
}

#[test]
fn backlinks_and_dangling_links() {
    let mut ws = Workspace::seed();
    let a = ws.add_page(None, PageKind::Doc);
    let b = ws.add_page(None, PageKind::Doc);
    let mut blk = ws.blk(BlockKind::PageLink, "", 0);
    blk.link = Some(b);
    ws.page_mut(a).unwrap().blocks.push(blk);
    assert_eq!(ws.backlinks(b), vec![a]);
    ws.delete_forever(b);
    assert!(ws.page(a).unwrap().blocks.iter().all(|x| x.link.is_none()));
}

#[test]
fn markdown_roundtrip() {
    let mut ws = Workspace::seed();
    let md = "# Plan\n\nIntro paragraph\n\n## Steps\n- [ ] first\n- [x] second\n  - nested bullet\n1. one\n2. two\n> wise words\n\n```\nlet x = 1;\n```\n---\n";
    let id = io::import_markdown(&mut ws, "fallback", md, None);
    let p = ws.page(id).unwrap();
    assert_eq!(p.title, "Plan");
    let kinds: Vec<BlockKind> = p.blocks.iter().map(|b| b.kind).collect();
    assert_eq!(
        kinds,
        vec![
            BlockKind::Text,
            BlockKind::H2,
            BlockKind::Todo,
            BlockKind::Todo,
            BlockKind::Bullet,
            BlockKind::Numbered,
            BlockKind::Numbered,
            BlockKind::Quote,
            BlockKind::Code,
            BlockKind::Divider
        ]
    );
    assert!(p.blocks[3].checked && !p.blocks[2].checked);
    assert_eq!(p.blocks[4].indent, 1);
    assert_eq!(p.blocks[8].text, "let x = 1;");
    let out = io::page_markdown(&ws, id);
    assert!(out.starts_with("# Plan"));
    assert!(out.contains("- [x] second"));
    assert!(out.contains("  - nested bullet"));
    assert!(out.contains("2. two"));
    assert!(out.contains("```\nlet x = 1;\n```"));
}

#[test]
fn csv_import_infers_types_and_exports() {
    let mut ws = Workspace::seed();
    let csv = "Name,Qty,Done,Due,Category,Notes\n\"Apples, red\",3,yes,2025-01-02,Fruit,\"say \"\"hi\"\"\"\nBananas,5,no,2025-01-03,Fruit,\nCarrot,2.5,yes,2025-01-04,Veg,x\nDates,9,no,2025-02-01,Fruit,y\nEggplant,1,no,2025-02-02,Veg,z\nFig,7,yes,2025-02-03,Fruit,w\n";
    let id = io::import_csv(&mut ws, "Shop", csv, None).unwrap();
    let d = ws.db(id).unwrap();
    let kinds: Vec<PropKind> = d.props.iter().map(|p| p.kind).collect();
    assert_eq!(kinds, vec![PropKind::Number, PropKind::Checkbox, PropKind::Date, PropKind::Select, PropKind::Text]);
    assert_eq!(ws.rows(id).len(), 6);
    let first = ws.page(ws.rows(id)[0]).unwrap();
    assert_eq!(first.title, "Apples, red");
    let out = io::db_csv(&ws, id);
    assert!(out.starts_with("Name,Qty,Done,Due,Category,Notes\n\"Apples, red\",3,Yes,"));
    assert!(out.contains("\"say \"\"hi\"\"\""));
    assert!(io::import_csv(&mut ws, "bad", "just one line", None).is_none());
}

#[test]
fn legacy_board_migrates_to_database() {
    let mut ws = Workspace::seed();
    let id = ws.id();
    let mut p = ws.page(ws.current.unwrap()).unwrap().clone();
    p.id = id;
    p.title = "Old board".into();
    p.kind = PageKind::Board;
    p.db = None;
    p.blocks.clear();
    p.columns = vec![
        Column { id: 900, title: "Todo".into(), color: 0, cards: vec![Card { id: 901, text: "one".into() }, Card { id: 902, text: "two".into() }] },
        Column { id: 903, title: "Done".into(), color: 2, cards: vec![Card { id: 904, text: "three".into() }] },
    ];
    ws.pages.push(p);
    ws.migrate();
    assert_eq!(ws.page(id).unwrap().kind, PageKind::Database);
    assert_eq!(ws.rows(id).len(), 3);
    let d = ws.db(id).unwrap();
    assert_eq!(d.views[0].kind, ViewKind::Board);
    assert_eq!(d.props[0].opts.len(), 2);
    // migrate is idempotent
    ws.migrate();
    assert_eq!(ws.rows(id).len(), 3);
}

#[test]
fn workspace_serde_roundtrip() {
    let ws = Workspace::seed();
    let json = serde_json::to_string(&ws).unwrap();
    let back: Workspace = serde_json::from_str(&json).unwrap();
    assert_eq!(back.pages.len(), ws.pages.len());
    let roadmap = back.pages.iter().find(|p| p.title == "Roadmap").unwrap();
    assert_eq!(back.rows(roadmap.id).len(), 7);
    // an old save (no new fields) still loads
    let old = r#"{"name":"W","pages":[{"id":1,"parent":null,"title":"T","icon":"","cover":null,"kind":"Doc","blocks":[{"id":2,"kind":"Text","text":"hi"}],"favorite":false,"trashed":false,"expanded":false,"updated":0.0}],"next_id":2,"current":1,"dark":false,"sidebar_open":true}"#;
    let w: Workspace = serde_json::from_str(old).unwrap();
    assert_eq!(w.pages[0].blocks[0].indent, 0);
    assert_eq!(w.accent, 0);
}

#[test]
fn templates_all_build() {
    let mut ws = Workspace::seed();
    for i in 0..TEMPLATES.len() {
        let id = ws.add_template(i, None);
        let p = ws.page(id).unwrap();
        assert_eq!(p.title, TEMPLATES[i].0);
        assert_eq!(ws.current, Some(id));
    }
    ws.open_today();
    let cur = ws.current.unwrap();
    assert_eq!(ws.page(cur).unwrap().title, dates::long_today());
    let again = {
        ws.open_today();
        ws.current.unwrap()
    };
    assert_eq!(cur, again, "today's entry is reused");
}

// ───────────── headless rendering ─────────────

fn test_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    crate::theme::setup_fonts(&ctx);
    let _ = ctx.run(RawInput::default(), |_| {});
    ctx
}

fn run_frame(ctx: &egui::Context, ws: &mut Workspace, id: u64, ed: &mut EditorState, t: f64, events: Vec<Event>) {
    let mut input = RawInput::default();
    input.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 820.0)));
    input.time = Some(t);
    input.events = events;
    let pal = Palette::light();
    let _ = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                editor::show_page(ui, ws, id, ed, &pal);
            });
        });
    });
}

#[test]
fn renders_every_page_and_view_without_panicking() {
    let ctx = test_ctx();
    let mut ws = Workspace::seed();
    for i in 0..TEMPLATES.len() {
        ws.add_template(i, None);
    }
    let mut ed = EditorState::default();
    let ids: Vec<u64> = ws.pages.iter().filter(|p| !p.trashed).map(|p| p.id).collect();
    let mut t = 0.0;
    for id in ids {
        let views = ws.page(id).and_then(|p| p.db.as_ref()).map(|d| d.views.len()).unwrap_or(1);
        for v in 0..views {
            if let Some(d) = ws.db_mut(id) {
                d.active = v;
            }
            for _ in 0..3 {
                t += 0.05;
                let ev = vec![Event::PointerMoved(pos2(400.0, 300.0))];
                run_frame(&ctx, &mut ws, id, &mut ed, t, ev);
            }
        }
    }
    // rows (with property panel) render too
    let road = ws.pages.iter().find(|p| p.title == "Roadmap").unwrap().id;
    for r in ws.rows(road) {
        run_frame(&ctx, &mut ws, r, &mut ed, t + 1.0, vec![]);
    }
}

#[test]
fn clicking_around_a_database_does_not_panic() {
    let ctx = test_ctx();
    let mut ws = Workspace::seed();
    let road = ws.pages.iter().find(|p| p.title == "Roadmap").unwrap().id;
    let mut ed = EditorState::default();
    let mut t = 0.0;
    for v in 0..3 {
        ws.db_mut(road).unwrap().active = v;
        // sweep clicks across the page to hit tabs, cells, cards and popups
        for y in (60..780).step_by(40) {
            for x in (80..1240).step_by(120) {
                t += 0.02;
                let p = pos2(x as f32, y as f32);
                let ev = vec![
                    Event::PointerMoved(p),
                    Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE },
                    Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE },
                ];
                run_frame(&ctx, &mut ws, road, &mut ed, t, ev);
            }
        }
    }
    assert!(ws.db(road).is_some());
}

#[test]
fn typing_in_the_editor_does_not_panic() {
    let ctx = test_ctx();
    let mut ws = Workspace::seed();
    let id = ws.current.unwrap();
    let mut ed = EditorState::default();
    let first = ws.page(id).unwrap().blocks.iter().find(|b| b.kind.has_text()).unwrap().id;
    ed.focus = Some((first, 0));
    let mut t = 0.0;
    let mut frames: Vec<Vec<Event>> = vec![vec![]];
    for ch in ["h", "i", "/", "t", "o"] {
        frames.push(vec![Event::Text(ch.into())]);
    }
    for k in [egui::Key::Enter, egui::Key::Tab, egui::Key::ArrowDown, egui::Key::Backspace] {
        frames.push(vec![Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
    }
    frames.push(vec![Event::Key { key: egui::Key::B, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND }]);
    for f in frames {
        t += 0.05;
        run_frame(&ctx, &mut ws, id, &mut ed, t, f);
        run_frame(&ctx, &mut ws, id, &mut ed, t + 0.01, vec![]);
    }
}

fn key(k: egui::Key, m: Modifiers) -> Event {
    Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: m }
}

/// Reproduces the "slash menu items do nothing when clicked" bug: type `/`, hover an item, press and release.
#[test]
fn clicking_a_slash_menu_item_applies_it() {
    let mut worked = 0;
    for (px, py) in [(360.0f32, 430.0f32), (380.0, 470.0), (380.0, 520.0), (380.0, 560.0), (380.0, 610.0)] {
        let ctx = test_ctx();
        let mut ws = Workspace::seed();
        let id = ws.current.unwrap();
        let mut ed = EditorState::default();
        // use an empty page with a single block so the layout is predictable
        let b = ws.blk(BlockKind::Text, "", 0);
        ws.page_mut(id).unwrap().blocks = vec![b];
        let bid = ws.page(id).unwrap().blocks[0].id;
        ed.focus = Some((bid, 0));
        let mut t = 0.0;
        for ev in [vec![], vec![Event::Text("/".into())], vec![], vec![], vec![], vec![]] {
            t += 0.05;
            run_frame(&ctx, &mut ws, id, &mut ed, t, ev);
        }
        assert_eq!(ws.page(id).unwrap().blocks[0].text, "/");
        eprintln!("menu rect: {:?} focus={:?}", ctx.memory(|m| m.area_rect(Id::new("slash_menu"))), ctx.memory(|m| m.focused()));
        let p = pos2(px, py);
        for ev in [
            vec![Event::PointerMoved(p)],
            vec![],
            vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }],
            vec![],
            vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }],
            vec![],
            vec![],
        ] {
            t += 0.05;
            run_frame(&ctx, &mut ws, id, &mut ed, t, ev);
        }
        let blk = &ws.page(id).unwrap().blocks[0];
        eprintln!("click at {px},{py}: kind={:?} text={:?} blocks={}", blk.kind, blk.text, ws.page(id).unwrap().blocks.len());
        if blk.text != "/" {
            worked += 1;
        }
    }
    assert!(worked >= 3, "clicking slash menu items must apply them ({worked}/5)");
}

/// Right-click must never collapse an existing text selection, and must open the context menu.
#[test]
fn right_click_keeps_selection_and_opens_menu() {
    let ctx = test_ctx();
    let mut ws = Workspace::seed();
    let id = ws.current.unwrap();
    let mut ed = EditorState::default();
    let b = ws.blk(BlockKind::Text, "hello world", 0);
    ws.page_mut(id).unwrap().blocks = vec![b];
    let bid = ws.page(id).unwrap().blocks[0].id;
    ed.focus = Some((bid, 0));
    let mut t = 0.0;
    let mut frame = |ctx: &egui::Context, ws: &mut Workspace, ed: &mut EditorState, evs: Vec<Event>, rc: bool, p: egui::Pos2| {
        t += 0.05;
        let mut input = RawInput::default();
        input.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 820.0)));
        input.time = Some(t);
        input.events = evs;
        if rc {
            input.events.push(Event::PointerButton { pos: p, button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE });
            input.events.push(Event::PointerButton { pos: p, button: PointerButton::Secondary, pressed: false, modifiers: Modifiers::NONE });
        }
        crate::widgets::filter_raw_input(ctx, &mut input);
        let pal = Palette::light();
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    editor::show_page(ui, ws, id, ed, &pal);
                });
            });
        });
        crate::widgets::end_frame(ctx);
    };
    let origin = pos2(0.0, 0.0);
    frame(&ctx, &mut ws, &mut ed, vec![], false, origin);
    // select all text in the block
    frame(&ctx, &mut ws, &mut ed, vec![key(egui::Key::A, Modifiers::COMMAND)], false, origin);
    frame(&ctx, &mut ws, &mut ed, vec![], false, origin);
    let tid = egui::Id::new(("blk", bid));
    let sel = crate::widgets::selection(&ctx, tid).expect("selection");
    assert_eq!(sel, (0, 11), "ctrl+a selects everything");
    // right-click somewhere over the text (sweep until the menu opens)
    let mut opened = false;
    for x in (310..500).step_by(20) {
        for y in (352..372).step_by(6) {
            let p = pos2(x as f32, y as f32);
            frame(&ctx, &mut ws, &mut ed, vec![Event::PointerMoved(p)], false, origin);
            frame(&ctx, &mut ws, &mut ed, vec![], true, p);
            if ed.menu.is_some() {
                opened = true;
            }
            frame(&ctx, &mut ws, &mut ed, vec![], false, origin);
            if opened {
                break;
            }
        }
        if opened {
            break;
        }
    }
    assert!(opened, "right-click opens the editor context menu");
    assert_eq!(crate::widgets::selection(&ctx, tid), Some((0, 11)), "selection survives a right-click");
}

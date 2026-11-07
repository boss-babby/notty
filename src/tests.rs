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
            egui::ScrollArea::vertical().drag_to_scroll(false).show(ui, |ui| {
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
                egui::ScrollArea::vertical().drag_to_scroll(false).show(ui, |ui| {
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

/// Regression: clicking a button of the floating selection toolbar must format the selected text.
#[test]
fn clicking_the_selection_toolbar_formats_text() {
    let ctx = test_ctx();
    let mut ws = Workspace::seed();
    let id = ws.current.unwrap();
    let mut ed = EditorState::default();
    let b = ws.blk(BlockKind::Text, "hello world", 0);
    ws.page_mut(id).unwrap().blocks = vec![b];
    let bid = ws.page(id).unwrap().blocks[0].id;
    ed.focus = Some((bid, 0));
    let mut t = 0.0;
    let mut frame = |ctx: &egui::Context, ws: &mut Workspace, ed: &mut EditorState, evs: Vec<Event>| {
        t += 0.05;
        let mut input = RawInput::default();
        input.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 820.0)));
        input.time = Some(t);
        input.events = evs;
        crate::widgets::filter_raw_input(ctx, &mut input);
        let pal = Palette::light();
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::ScrollArea::vertical().drag_to_scroll(false).show(ui, |ui| {
                    editor::show_page(ui, ws, id, ed, &pal);
                });
            });
        });
        crate::widgets::end_frame(ctx);
    };
    for _ in 0..3 {
        frame(&ctx, &mut ws, &mut ed, vec![]);
    }
    frame(&ctx, &mut ws, &mut ed, vec![key(egui::Key::A, Modifiers::COMMAND)]);
    for _ in 0..8 {
        frame(&ctx, &mut ws, &mut ed, vec![]);
    }
    assert!(ed.fmt_bar.is_some(), "toolbar must appear for a selection");
    let rect = ctx.memory(|m| m.area_rect(Id::new("fmt_bar"))).expect("toolbar area");
    eprintln!("bar rect {rect:?}");
    // click each button position along the bar until the text changes
    let mut applied = false;
    let mut x = rect.left() + 10.0;
    while x < rect.right() - 6.0 && !applied {
        let p = pos2(x, rect.center().y);
        for ev in [
            vec![Event::PointerMoved(p)],
            vec![],
            vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }],
            vec![],
            vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }],
            vec![],
            vec![],
        ] {
            frame(&ctx, &mut ws, &mut ed, ev);
        }
        let txt = ws.page(id).unwrap().blocks[0].text.clone();
        eprintln!("x={x} text={txt:?} bar={}", ed.fmt_bar.is_some());
        if txt != "hello world" {
            applied = true;
        } else {
            // that button did not format (e.g. it opened the colour menu): reset, re-select and try the next one
            ed.block_menu = None;
            ed.menu = None;
            ed.focus = Some((bid, 0));
            frame(&ctx, &mut ws, &mut ed, vec![]);
            frame(&ctx, &mut ws, &mut ed, vec![key(egui::Key::A, Modifiers::COMMAND)]);
            for _ in 0..6 {
                frame(&ctx, &mut ws, &mut ed, vec![]);
            }
        }
        x += 27.0;
    }
    assert!(applied, "clicking a toolbar button must change the text");
}

fn frame_with(ctx: &egui::Context, ws: &mut Workspace, id: u64, ed: &mut EditorState, t: &mut f64, evs: Vec<Event>) {
    *t += 0.05;
    let mut input = RawInput::default();
    input.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 820.0)));
    input.time = Some(*t);
    input.events = evs;
    crate::widgets::filter_raw_input(ctx, &mut input);
    let pal = Palette::light();
    let _ = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().drag_to_scroll(false).show(ui, |ui| {
                editor::show_page(ui, ws, id, ed, &pal);
            });
        });
    });
    crate::widgets::end_frame(ctx);
    crate::history::track(ws, id, ed, *t);
}

fn ptr(p: egui::Pos2, pressed: bool) -> Event {
    Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
}

/// Dragging a rectangle on the empty page area selects the blocks it touches; Delete removes them; Ctrl+Z restores them.
#[test]
fn marquee_selects_blocks_and_delete_undo_work() {
    let ctx = test_ctx();
    let mut ws = Workspace::seed();
    let id = ws.current.unwrap();
    let mut ed = EditorState::default();
    let blocks: Vec<Block> = (0..6).map(|i| ws.blk(BlockKind::Text, &format!("line {i}"), 0)).collect();
    ws.page_mut(id).unwrap().blocks = blocks;
    let mut t = 0.0;
    for _ in 0..4 {
        frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![]);
    }
    // start in the empty left margin, drag down-right across the text column
    let (a, b) = (pos2(90.0, 60.0), pos2(520.0, 700.0));
    frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![Event::PointerMoved(a)]);
    frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![ptr(a, true)]);
    for k in 1..=6 {
        let p = a + (b - a) * (k as f32 / 6.0);
        frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![Event::PointerMoved(p)]);
    }
    frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![ptr(b, false)]);
    frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![]);
    eprintln!("selected: {:?}", ed.sel);
    assert!(ed.sel.len() >= 2, "drag selection must select several blocks, got {}", ed.sel.len());
    let n_sel = ed.sel.len();
    // the toolbar is shown
    assert!(ctx.memory(|m| m.area_rect(Id::new("sel_bar"))).is_some(), "selection toolbar appears");
    // delete
    frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![key(egui::Key::Delete, Modifiers::NONE)]);
    frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![]);
    let left = ws.page(id).unwrap().blocks.len();
    assert_eq!(left, (6 - n_sel).max(1), "selected blocks are deleted");
    // undo
    frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![key(egui::Key::Z, Modifiers::COMMAND)]);
    frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![]);
    assert_eq!(ws.page(id).unwrap().blocks.len(), 6, "undo restores the deleted blocks");
}

#[test]
fn media_blocks_and_covers_render() {
    let ctx = test_ctx();
    egui_extras::install_image_loaders(&ctx);
    let mut ws = Workspace::seed();
    let id = ws.current.unwrap();
    let mut ed = EditorState::default();
    let mut blocks = vec![];
    for (k, url) in [
        (BlockKind::Image, ""),
        (BlockKind::Image, "https://example.invalid/pic.png"),
        (BlockKind::Embed, ""),
        (BlockKind::Embed, "https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
        (BlockKind::Embed, "https://example.com/page"),
        (BlockKind::Bookmark, "https://example.com/some/long/path?x=1"),
        (BlockKind::File, "https://example.com/report.pdf"),
        (BlockKind::File, ""),
    ] {
        let mut b = ws.blk(k, "", 0);
        b.url = url.into();
        blocks.push(b);
    }
    {
        let p = ws.page_mut(id).unwrap();
        p.blocks = blocks;
        p.cover_img = "https://example.invalid/cover.jpg".into();
        p.icon = "img:https://example.invalid/icon.png".into();
    }
    let mut t = 0.0;
    for _ in 0..6 {
        frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![]);
    }
    // an emoji icon chosen by the user must survive the legacy-icon migration
    ws.page_mut(id).unwrap().icon = "🦄".into();
    ws.icons_v2 = true;
    ws.migrate_icons();
    assert_eq!(ws.page(id).unwrap().icon, "🦄");
    // open both pickers
    ed.icon_picker = Some(pos2(300.0, 200.0));
    ed.cover_picker = Some(pos2(300.0, 200.0));
    for _ in 0..4 {
        frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![]);
    }
    // old workspaces without the new fields still load
    let json = serde_json::to_string(&ws).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    for p in v["pages"].as_array_mut().unwrap() {
        p.as_object_mut().unwrap().remove("cover_img");
        for b in p["blocks"].as_array_mut().unwrap() {
            b.as_object_mut().unwrap().remove("url");
        }
    }
    let back: Workspace = serde_json::from_value(v).unwrap();
    assert_eq!(back.pages.len(), ws.pages.len());
}

#[test]
fn inline_marks_underline_highlight_link_roundtrip_text() {
    let spans = editor::rich_job("a ==hi== __u__ [site](https://x.io) b", &egui::FontId::proportional(14.0), egui::Color32::BLACK, &Palette::light(), true);
    assert_eq!(spans.text, "a hi u site b", "markers are hidden when the block is not focused");
}


/// Regression: dragging a Gantt bar towards (and past) the edge of the chart used to widen the chart every frame while the bar
/// stayed under the pointer, `dx` grew without bound and the app froze. The chart must stay bounded and the drop must commit.
#[test]
fn gantt_drag_past_the_edge_stays_bounded_and_commits() {
    use std::collections::HashMap;
    let ctx = test_ctx();
    let mut ws = Workspace::seed();
    let (id, vi) = ws
        .pages
        .iter()
        .filter(|p| !p.trashed)
        .find_map(|p| p.db.as_ref().and_then(|d| d.views.iter().position(|v| v.kind == ViewKind::Timeline).map(|i| (p.id, i))))
        .expect("a timeline view");
    ws.db_mut(id).unwrap().active = vi;
    let dp = {
        let d = ws.db(id).unwrap();
        d.date_prop(&d.views[vi]).unwrap()
    };
    let mut ed = EditorState::default();
    let mut t = 0.0;
    for _ in 0..4 {
        frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![Event::PointerMoved(pos2(300.0, 300.0))]);
    }
    let bars: HashMap<u64, Rect> = ctx.data(|d| d.get_temp(Id::new(("tl_bars_test", id)))).expect("bars were laid out");
    let mut vis: Vec<(u64, Rect)> = bars.into_iter().filter(|(_, r)| r.center().x > 420.0 && r.center().x < 1000.0 && r.width() > 40.0 && r.center().y < 800.0).collect();
    vis.sort_by_key(|x| x.0);
    let (rid, r) = vis.first().copied().expect("a visible bar");
    let span_of = |ws: &Workspace| match ws.page(rid).unwrap().props.get(&dp) {
        Some(Val::Text(s)) => dates::span(s).unwrap(),
        _ => panic!("scheduled row expected"),
    };
    let (s0, e0) = span_of(&ws);
    let start = pos2(r.center().x, r.center().y);
    let started = std::time::Instant::now();
    frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![Event::PointerMoved(start), ptr(start, true)]);
    let far = pos2(1270.0, start.y);
    frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![Event::PointerMoved(pos2(900.0, start.y))]);
    // hold the pointer at the right edge for a long time (auto-scroll keeps pushing the bar outwards)
    for _ in 0..400 {
        frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![Event::PointerMoved(far)]);
    }
    frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![ptr(far, false)]);
    for _ in 0..3 {
        frame_with(&ctx, &mut ws, id, &mut ed, &mut t, vec![]);
    }
    assert!(started.elapsed().as_secs() < 60, "dragging must not stall the frame loop");
    let (s1, e1) = span_of(&ws);
    assert!(s1 > s0, "the bar moved to the right ({s0} → {s1})");
    assert_eq!(e1 - s1, e0 - s0, "moving keeps the duration");
    let (a, b): (i64, i64) = ctx.data(|d| d.get_temp(Id::new(("tl_domain", id, ws.db(id).unwrap().views[vi].id)))).expect("domain");
    assert!(b - a < 2000, "the chart range stays bounded ({} days)", b - a);
    assert!(e1 <= b, "the bar stays inside the chart");
}

fn frame_sized(ctx: &egui::Context, size: egui::Vec2, t: f64, events: Vec<Event>, f: impl FnOnce(&mut egui::Ui)) {
    let mut f = Some(f);
    let mut input = RawInput::default();
    input.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), size));
    input.time = Some(t);
    input.events = events;
    let _ = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(f) = f.take() {
                f(ui);
            }
        });
    });
}

#[test]
fn graph_and_dashboard_render_on_desktop_and_phone_without_panicking() {
    let ctx = test_ctx();
    let mut ws = Workspace::seed();
    for i in 0..TEMPLATES.len() {
        ws.add_template(i, None);
    }
    // a page that links to another one
    let (a, b) = (ws.pages[0].id, ws.pages[1].id);
    let mut link = ws.blk(BlockKind::PageLink, "", 0);
    link.link = Some(b);
    ws.page_mut(a).unwrap().blocks.push(link);
    let pal = Palette::light();
    let mut t = 0.0;
    for size in [vec2(1280.0, 800.0), vec2(390.0, 780.0), vec2(320.0, 560.0)] {
        for _ in 0..4 {
            t += 0.05;
            frame_sized(&ctx, size, t, vec![Event::PointerMoved(pos2(100.0, 300.0))], |ui| {
                let _ = crate::dashboard::show(ui, &mut ws, &pal);
            });
        }
        for k in 0..60 {
            t += 0.05;
            let p = pos2(150.0 + k as f32 * 3.0, 300.0);
            let ev = if k == 10 {
                vec![Event::PointerMoved(p), Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]
            } else if k == 12 {
                vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]
            } else {
                vec![Event::PointerMoved(p), Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -3.0), modifiers: Modifiers::NONE }]
            };
            frame_sized(&ctx, size, t, ev, |ui| {
                let _ = crate::graph::show(ui, &ws, &pal);
            });
        }
    }
}

#[test]
fn database_toolbar_filters_and_resizable_columns_fit_a_phone() {
    let ctx = test_ctx();
    let mut ws = Workspace::seed();
    let road = ws.pages.iter().find(|p| p.title == "Roadmap").unwrap().id;
    let mut ed = EditorState::default();
    let pal = Palette::light();
    ws.db_mut(road).unwrap().active = 0;
    let mut t = 0.0;
    for w in [1280.0_f32, 700.0, 390.0, 320.0] {
        // open the filter and sort popups through the toolbar buttons by sweeping clicks over the header area
        for y in [70.0_f32, 100.0, 130.0] {
            for x in (10..w as i32).step_by(30) {
                t += 0.02;
                let p = pos2(x as f32, y);
                let evs = vec![
                    Event::PointerMoved(p),
                    Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE },
                    Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE },
                ];
                frame_sized(&ctx, vec2(w, 700.0), t, evs, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        crate::database::show(ui, &mut ws, road, &mut ed, &pal, (w - 24.0).max(200.0));
                    });
                });
            }
        }
    }
    // column widths are stored per view and survive JSON
    ws.db_mut(road).unwrap().views[0].widths.insert(0, 333.0);
    let s = serde_json::to_string(&ws).unwrap();
    let back: Workspace = serde_json::from_str(&s).unwrap();
    assert_eq!(back.db(road).unwrap().views[0].widths.get(&0), Some(&333.0));
}

// ───────────────────────────── v5.2: dashboard range, date filters, columns, linked views ─────────────────────────────

fn new_db(ws: &mut Workspace) -> u64 {
    ws.pages.iter().find(|p| p.db.as_ref().map_or(false, |d| d.first_of(PropKind::Date).is_some()) && !p.row).map(|p| p.id).expect("a database with a date property")
}

#[test]
fn relative_date_filters_match_today_week_and_overdue() {
    let mut ws = Workspace::seed();
    let db = new_db(&mut ws);
    let dp = ws.db(db).unwrap().first_of(PropKind::Date).unwrap();
    let today = dates::today_day();
    let mk = |ws: &mut Workspace, off: i64| {
        let r = ws.add_row(db, None);
        ws.page_mut(r).unwrap().props.insert(dp, Val::Text(dates::iso_of_day(today + off)));
        r
    };
    let (r_today, r_past, r_next3, r_far) = (mk(&mut ws, 0), mk(&mut ws, -3), mk(&mut ws, 3), mk(&mut ws, 400));
    let count = |ws: &Workspace, r: u64, op: FilterOp| {
        let d = ws.db(db).unwrap();
        row_matches(ws.page(r).unwrap(), d, &Filter { prop: dp, op, value: String::new() })
    };
    assert!(count(&ws, r_today, FilterOp::Today));
    assert!(!count(&ws, r_past, FilterOp::Today));
    assert!(count(&ws, r_past, FilterOp::Overdue));
    assert!(!count(&ws, r_today, FilterOp::Overdue));
    assert!(count(&ws, r_next3, FilterOp::Next7));
    assert!(!count(&ws, r_far, FilterOp::Next7));
    assert!(count(&ws, r_past, FilterOp::Past7));
    assert!(count(&ws, r_today, FilterOp::ThisWeek));
    assert!(!count(&ws, r_far, FilterOp::ThisMonth));
    // a filter on a view applies through the normal query
    let vi = 0;
    let before = ws.query(db, vi).len();
    ws.db_mut(db).unwrap().views[vi].filters.push(Filter { prop: dp, op: FilterOp::Today, value: String::new() });
    let after = ws.query(db, vi);
    assert!(after.contains(&r_today) && !after.contains(&r_far) && after.len() < before);
}

#[test]
fn page_dashboards_have_their_own_time_window() {
    let mut ws = Workspace::seed();
    let page = ws.pages.iter().find(|p| p.kind == PageKind::Doc && !p.row).unwrap().id;
    ws.dash.ensure_page_board(page, "Dashboard");
    let b = ws.dash.page_board(page).unwrap();
    assert_eq!(ws.dash.scope_range(b), crate::dashcfg::Range::All);
    ws.dash.set_scope_range(b, crate::dashcfg::Range::Today);
    assert_eq!(ws.dash.scope_range(b), crate::dashcfg::Range::Today);
    assert_eq!(ws.dash.scope_range(0), crate::dashcfg::Range::All, "Home keeps its own window");
    // the window survives a save / load
    let json = serde_json::to_string(&ws).unwrap();
    let back: Workspace = serde_json::from_str(&json).unwrap();
    assert_eq!(back.dash.scope_range(b), crate::dashcfg::Range::Today);
    // and renders (toolbar with the Today / Week / Month chips) on desktop and phone
    let ctx = test_ctx();
    let pal = Palette::light();
    for (k, size) in [vec2(1280.0, 800.0), vec2(390.0, 780.0)].into_iter().enumerate() {
        for f in 0..3 {
            frame_sized(&ctx, size, 0.1 * (k * 3 + f) as f64 + 0.1, vec![], |ui| {
                let _ = crate::dashboard::embedded(ui, &mut ws, &pal, b);
            });
        }
    }
}

#[test]
fn columns_and_linked_database_views_render_and_stay_independent() {
    let ctx = test_ctx();
    let mut ws = Workspace::seed();
    let db = new_db(&mut ws);
    let pid = ws.pages.iter().find(|p| p.kind == PageKind::Doc && !p.row).unwrap().id;
    let mut bl = vec![ws.blk(BlockKind::H1, "Layouts", 0), ws.blk(BlockKind::Columns, "", 0)];
    for t in ["left", "right", "third"] {
        bl.push(ws.blk(BlockKind::Column, "", 1));
        bl.push(ws.blk(BlockKind::Text, t, 2));
        bl.push(ws.blk(BlockKind::Bullet, "item", 2));
    }
    let mut dv = ws.blk(BlockKind::DbView, "", 0);
    dv.link = Some(db);
    let dvid = dv.id;
    bl.push(dv);
    bl.push(ws.blk(BlockKind::Text, "after", 0));
    ws.page_mut(pid).unwrap().blocks = bl;
    let tabs_before = ws.db(db).unwrap().tabs().len();
    let mut ed = EditorState::default();
    let mut t = 0.0;
    for (k, size) in [vec2(1280.0, 820.0), vec2(900.0, 700.0), vec2(390.0, 780.0)].into_iter().enumerate() {
        for f in 0..6 {
            t += 0.05;
            let mut input = RawInput::default();
            input.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), size));
            input.time = Some(t);
            input.events = vec![Event::PointerMoved(pos2(300.0 + 40.0 * f as f32, 220.0 + 20.0 * k as f32))];
            let pal = Palette::light();
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        editor::show_page(ui, &mut ws, pid, &mut ed, &pal);
                    });
                });
            });
        }
    }
    // the linked view exists, is hidden from the database's own tabs, and has its own filters
    let d = ws.db(db).unwrap();
    let vi = d.linked_index(dvid).expect("linked view was created");
    assert_eq!(d.tabs().len(), tabs_before);
    assert!(d.views[vi].linked == dvid);
    let dp = d.first_of(PropKind::Date).unwrap();
    let all = ws.query(db, vi).len();
    ws.db_mut(db).unwrap().views[vi].filters.push(Filter { prop: dp, op: FilterOp::Today, value: String::new() });
    assert!(ws.query(db, vi).len() <= all);
    assert_eq!(ws.query(db, ws.db(db).unwrap().active_tab()).len(), all, "the source view is untouched");
    // the layout is intact
    assert!(ws.page(pid).unwrap().blocks.iter().filter(|b| b.kind == BlockKind::Column).count() == 3);
    // deleting the block prunes its hidden view
    ws.page_mut(pid).unwrap().blocks.retain(|b| b.kind != BlockKind::DbView);
    ws.prune_linked_views();
    assert!(ws.db(db).unwrap().linked_index(dvid).is_none());
}

/// Gallery view: cards wrap into rows that always fit the width (it used to grow sideways in one endless row that could not be scrolled).
#[test]
fn gallery_wraps_into_rows_instead_of_growing_sideways() {
    let ctx = test_ctx();
    let mut ws = Workspace::seed();
    let db = ws.add_database(None, ViewKind::Gallery);
    for _ in 0..40 {
        ws.add_row(db, None);
    }
    assert_eq!(ws.db(db).unwrap().views[ws.db(db).unwrap().active_tab()].kind, ViewKind::Gallery);
    let mut ed = EditorState::default();
    let pal = Palette::light();
    let mut t = 0.0;
    for width in [320.0_f32, 390.0, 700.0, 1280.0, 2400.0] {
        let mut used = (0.0_f32, 0.0_f32, 0.0_f32);
        for _ in 0..3 {
            t += 0.05;
            frame_sized(&ctx, vec2(width, 3000.0), t, vec![], |ui| {
                let w = (ui.available_width() - 40.0).max(200.0);
                crate::database::show(ui, &mut ws, db, &mut ed, &pal, w);
                used = (ui.min_rect().width(), w, ui.min_rect().height());
            });
        }
        assert!(used.0 <= used.1 + 40.5, "{width}px: the gallery is {}px wide inside {}px", used.0, used.1);
        assert!(used.2 > 300.0, "{width}px: 40 cards need several rows (height {})", used.2);
    }
}

// ───────────────────────── mobile update 3 ─────────────────────────

/// Height the page content takes at a given content size (rendered headlessly on a desktop-sized screen).
fn page_height_at(zoom: f32) -> f32 {
    let ctx = test_ctx();
    let mut ws = Workspace::seed();
    let id = ws.current.unwrap();
    {
        let p = ws.page_mut(id).unwrap();
        p.zoom = zoom;
        p.blocks.clear();
    }
    for (k, txt) in [
        (BlockKind::H1, "A heading"),
        (BlockKind::Text, "Some body text that is long enough to wrap onto a second line when the content is made larger, and a third one, and so on and so on."),
        (BlockKind::Todo, "a to-do"),
        (BlockKind::Bullet, "a bullet"),
        (BlockKind::Numbered, "a number"),
        (BlockKind::Callout, "a callout"),
        (BlockKind::Quote, "a quote"),
    ] {
        let b = ws.blk(k, txt, 0);
        ws.page_mut(id).unwrap().blocks.push(b);
    }
    let mut ed = EditorState::default();
    let pal = Palette::light();
    let mut h = 0.0;
    for i in 0..4 {
        let mut input = RawInput::default();
        input.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 4000.0)));
        input.time = Some(i as f64 * 0.1);
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let out = egui::ScrollArea::vertical().drag_to_scroll(false).show(ui, |ui| {
                    editor::show_page(ui, &mut ws, id, &mut ed, &pal);
                });
                h = out.content_size.y;
            });
        });
    }
    h
}

#[test]
fn content_size_scales_the_page_and_is_clamped() {
    let (small, normal, big) = (page_height_at(0.7), page_height_at(1.0), page_height_at(1.8));
    assert!(small < normal && normal < big, "page height must follow the content size: {small} < {normal} < {big}");
    // damaged values never break the layout
    for bad in [f32::NAN, 0.0, -3.0, 50.0, f32::INFINITY] {
        let h = page_height_at(bad);
        assert!(h.is_finite() && h > 100.0 && h < 100_000.0, "zoom {bad} gave height {h}");
    }
    assert_eq!(clamp_zoom(f32::NAN), 1.0);
    assert_eq!(clamp_zoom(9.0), ZOOM_MAX);
    assert_eq!(clamp_zoom(0.1), ZOOM_MIN);
    // pages saved before this update have no `zoom`
    let ws = Workspace::seed();
    let mut v: serde_json::Value = serde_json::to_value(&ws).unwrap();
    for p in v["pages"].as_array_mut().unwrap() {
        p.as_object_mut().unwrap().remove("zoom");
    }
    let back: Workspace = serde_json::from_value(v).unwrap();
    assert!(back.pages.iter().all(|p| (p.zoom - 1.0).abs() < f32::EPSILON));
}

#[test]
fn size_control_steps_and_slider_mapping() {
    use crate::widgets::{step_zoom, zoom_from_t};
    assert_eq!(step_zoom(1.0, 1), 1.05);
    assert_eq!(step_zoom(1.0, -1), 0.95);
    assert_eq!(step_zoom(ZOOM_MAX, 1), ZOOM_MAX);
    assert_eq!(step_zoom(ZOOM_MIN, -1), ZOOM_MIN);
    let mut z = 1.0;
    for _ in 0..40 {
        z = step_zoom(z, 1);
    }
    assert_eq!(z, ZOOM_MAX);
    for _ in 0..80 {
        z = step_zoom(z, -1);
    }
    assert_eq!(z, ZOOM_MIN);
    assert_eq!(zoom_from_t(0.0), ZOOM_MIN);
    assert_eq!(zoom_from_t(1.0), ZOOM_MAX);
    assert_eq!(zoom_from_t(-5.0), ZOOM_MIN);
    let t100 = (1.0 - ZOOM_MIN) / (ZOOM_MAX - ZOOM_MIN);
    assert_eq!(zoom_from_t(t100), 1.0);
}

/// The widget renders and the +/− buttons change the stored value (clicked with a real pointer press / release).
#[test]
fn size_control_buttons_work_with_a_pointer() {
    let ctx = test_ctx();
    let pal = Palette::light();
    let mut zoom = 1.0_f32;
    let size = vec2(300.0, 100.0);
    let mut plus_center = None;
    // layout pass: find the + button (right part of the row)
    frame_sized(&ctx, size, 0.0, vec![], |ui| {
        let before = ui.cursor().min;
        crate::widgets::size_control(ui, &pal, &mut zoom);
        let w = ui.min_rect().width();
        plus_center = Some(pos2(before.x + w - 4.0 - 48.0 - 4.0 - 17.0, before.y + 17.0));
    });
    let p = plus_center.unwrap();
    for (i, ev) in [vec![Event::PointerMoved(p)], vec![ptr(p, true)], vec![ptr(p, false)], vec![]].into_iter().enumerate() {
        frame_sized(&ctx, size, 0.1 * (i + 1) as f64, ev, |ui| {
            crate::widgets::size_control(ui, &pal, &mut zoom);
        });
    }
    assert!(zoom > 1.0, "the + button must enlarge the content, zoom = {zoom}");
}

/// All the text the frame painted (to look for controls that must be visible).
fn painted_text(out: &egui::FullOutput) -> String {
    fn walk(s: &egui::epaint::Shape, acc: &mut String) {
        match s {
            egui::epaint::Shape::Text(t) => {
                acc.push_str(&t.galley.text());
                acc.push('\n');
            }
            egui::epaint::Shape::Vec(v) => v.iter().for_each(|x| walk(x, acc)),
            _ => {}
        }
    }
    let mut acc = String::new();
    for c in &out.shapes {
        walk(&c.shape, &mut acc);
    }
    acc
}

/// Cover buttons used to need a mouse hover and so never showed on a phone.
#[test]
fn cover_buttons_are_visible_on_touch_without_hover() {
    fn run(touchy: bool, width: f32) -> String {
        crate::touch::set_touchy(touchy);
        let ctx = test_ctx();
        let mut ws = Workspace::seed();
        let id = ws.current.unwrap();
        ws.page_mut(id).unwrap().cover = Some(2);
        let mut ed = EditorState::default();
        let pal = Palette::light();
        let mut text = String::new();
        for i in 0..8 {
            let mut input = RawInput::default();
            input.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 800.0)));
            input.time = Some(i as f64 * 0.1);
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    editor::show_page(ui, &mut ws, id, &mut ed, &pal);
                });
            });
            text = painted_text(&out);
        }
        crate::touch::set_touchy(false);
        text
    }
    let mouse = run(false, 1000.0);
    assert!(!mouse.contains("Change cover"), "with a mouse the buttons appear on hover only");
    for w in [320.0_f32, 360.0, 1000.0] {
        let t = run(true, w);
        assert!(t.contains("Change cover") && t.contains("Remove"), "{w}px touch: cover buttons missing in {t:?}");
    }
}

/// While the sidebar moves, the page is laid out at one fixed width (no re-wrapping per frame) and is clipped to its panel.
#[test]
fn frozen_layout_keeps_the_layout_width_constant() {
    let ctx = test_ctx();
    let mut widths = vec![];
    for (i, panel_w) in [1000.0_f32, 900.0, 800.0, 700.0].into_iter().enumerate() {
        let mut input = RawInput::default();
        input.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(panel_w, 600.0)));
        input.time = Some(i as f64 * 0.1);
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                crate::app::frozen_layout(ui, Some(700.0), |ui| widths.push(ui.available_width()));
            });
        });
    }
    assert!(widths.iter().all(|w| (*w - 700.0).abs() < 1.0), "{widths:?}");
}

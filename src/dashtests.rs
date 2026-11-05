//! Headless UI tests of the customisable dashboard: every widget renders, edit mode, drag & drop, resize, pinning, persistence.

use egui::{pos2, vec2, Event, Modifiers, PointerButton, RawInput, Rect};

use crate::model::{Workspace, TEMPLATES};
use crate::theme::Palette;

fn test_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    crate::theme::setup_fonts(&ctx);
    let _ = ctx.run(RawInput::default(), |_| {});
    ctx
}

fn dash_ws() -> Workspace {
    let mut ws = Workspace::seed();
    for i in 0..TEMPLATES.len() {
        ws.add_template(i, None);
    }
    ws
}

fn frame(ctx: &egui::Context, ws: &mut Workspace, pal: &Palette, size: egui::Vec2, t: &mut f64, events: Vec<Event>) -> crate::dashboard::Nav {
    *t += 0.05;
    let mut input = RawInput::default();
    input.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), size));
    input.time = Some(*t);
    input.events = events;
    let mut nav = None;
    let _ = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            nav = Some(crate::dashboard::show(ui, ws, pal));
        });
    });
    nav.unwrap()
}

fn btn(p: egui::Pos2, pressed: bool) -> Event {
    Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
}

#[test]
fn every_dashboard_widget_renders_edits_and_survives_clicks() {
    crate::dashboard::testing::reset();
    let ctx = test_ctx();
    let mut ws = dash_ws();
    let pal = Palette::light();
    crate::dashcfg::ensure_seeded(&mut ws);
    ws.dash.boards[0].widgets.clear();
    for d in crate::dashwidgets::all() {
        let id = crate::dashedit::create(&mut ws, d.kind, 0).unwrap_or_else(|| panic!("{} can be created", d.kind));
        assert!(ws.dash.widget(id).is_some());
    }
    let n = ws.dash.boards[0].widgets.len();
    assert!(n >= 30, "{n} widgets on the board");
    let mut t = 0.0;
    // 1. a tall viewport so that every widget is drawn; 2. click the middle of every widget
    for size in [vec2(1280.0, 6000.0), vec2(640.0, 9000.0), vec2(390.0, 12000.0)] {
        for _ in 0..4 {
            frame(&ctx, &mut ws, &pal, size, &mut t, vec![]);
        }
        let rects = crate::dashboard::testing::rects();
        assert_eq!(rects.len(), n);
        for (_, r) in rects {
            let p = r.center();
            frame(&ctx, &mut ws, &pal, size, &mut t, vec![Event::PointerMoved(p)]);
            frame(&ctx, &mut ws, &pal, size, &mut t, vec![btn(p, true)]);
            frame(&ctx, &mut ws, &pal, size, &mut t, vec![btn(p, false)]);
            frame(&ctx, &mut ws, &pal, size, &mut t, vec![]);
        }
    }
    // 3. customise mode with the inspector open for every widget, then the gallery
    let ids: Vec<u64> = ws.dash.boards[0].widgets.iter().map(|w| w.id).collect();
    for id in ids {
        crate::dashboard::testing::set(true, Some(id), false);
        for _ in 0..3 {
            frame(&ctx, &mut ws, &pal, vec2(1280.0, 6000.0), &mut t, vec![]);
        }
        let (edit, inspect, _, _) = crate::dashboard::testing::state();
        assert!(edit && inspect == Some(id), "inspector for widget {id} stays open");
    }
    crate::dashboard::testing::set(true, None, true);
    for size in [vec2(1000.0, 900.0), vec2(360.0, 640.0)] {
        for _ in 0..3 {
            frame(&ctx, &mut ws, &pal, size, &mut t, vec![]);
        }
    }
    let (_, _, gallery, _) = crate::dashboard::testing::state();
    assert!(gallery, "the gallery stays open");
    // 4. dark mode
    crate::dashboard::testing::set(false, None, false);
    for _ in 0..3 {
        frame(&ctx, &mut ws, &Palette::dark(), vec2(1280.0, 6000.0), &mut t, vec![]);
    }
}

#[test]
fn dashboard_drag_reorders_and_resize_changes_the_size() {
    crate::dashboard::testing::reset();
    let ctx = test_ctx();
    let mut ws = Workspace::seed();
    ws.dash = Default::default();
    let pal = Palette::light();
    let a = ws.dash.add_widget(crate::dashcfg::new_widget("notes", 6, 2, Default::default()));
    let b = ws.dash.add_widget(crate::dashcfg::new_widget("today", 6, 2, Default::default()));
    let c = ws.dash.add_widget(crate::dashcfg::new_widget("timer", 12, 2, Default::default()));
    crate::dashboard::testing::set(true, None, false);
    let size = vec2(1280.0, 1400.0);
    let mut t = 0.0;
    for _ in 0..4 {
        frame(&ctx, &mut ws, &pal, size, &mut t, vec![]);
    }
    let rect = |id: u64| crate::dashboard::testing::rects().into_iter().find(|r| r.0 == id).unwrap().1;
    let order = |ws: &Workspace| ws.dash.boards[0].widgets.iter().map(|w| w.id).collect::<Vec<_>>();
    assert_eq!(order(&ws), vec![a, b, c]);
    // drag the grip of the third widget onto the left half of the first one
    let grip = rect(c).min + vec2(22.0, 21.0);
    let target = rect(a).min + vec2(40.0, 60.0);
    frame(&ctx, &mut ws, &pal, size, &mut t, vec![Event::PointerMoved(grip)]);
    frame(&ctx, &mut ws, &pal, size, &mut t, vec![btn(grip, true)]);
    for k in 1..=6 {
        let p = grip + (target - grip) * (k as f32 / 6.0);
        frame(&ctx, &mut ws, &pal, size, &mut t, vec![Event::PointerMoved(p)]);
    }
    frame(&ctx, &mut ws, &pal, size, &mut t, vec![btn(target, false)]);
    frame(&ctx, &mut ws, &pal, size, &mut t, vec![]);
    assert_eq!(order(&ws), vec![c, a, b], "the dragged widget now comes first");
    // resize: pull the corner handle to the middle of the widget and 330 px down
    for _ in 0..3 {
        frame(&ctx, &mut ws, &pal, size, &mut t, vec![]);
    }
    let r = rect(c);
    let handle = r.right_bottom() - vec2(11.0, 11.0);
    let w0 = ws.dash.widget(c).unwrap().clone();
    assert_eq!((w0.span, w0.rows), (12, 2));
    let to = pos2(r.left() + r.width() * 0.5, r.top() + 330.0);
    frame(&ctx, &mut ws, &pal, size, &mut t, vec![Event::PointerMoved(handle)]);
    frame(&ctx, &mut ws, &pal, size, &mut t, vec![btn(handle, true)]);
    for k in 1..=5 {
        let p = handle + (to - handle) * (k as f32 / 5.0);
        frame(&ctx, &mut ws, &pal, size, &mut t, vec![Event::PointerMoved(p)]);
    }
    frame(&ctx, &mut ws, &pal, size, &mut t, vec![btn(to, false)]);
    frame(&ctx, &mut ws, &pal, size, &mut t, vec![]);
    let w = ws.dash.widget(c).unwrap();
    assert_eq!(w.span, 6, "half width after dragging to the middle");
    assert_eq!(w.rows, 3, "three rows after dragging 330 px down");
}

#[test]
fn pinning_from_a_database_and_persisting_the_dashboard() {
    let mut ws = dash_ws();
    crate::dashcfg::ensure_seeded(&mut ws);
    let road = ws.pages.iter().find(|p| p.title == "Roadmap").unwrap().id;
    let before = ws.dash.boards[0].widgets.len();
    let id = crate::dashcfg::pin(&mut ws, road, 0, "table");
    let w = ws.dash.widget(id).unwrap();
    assert_eq!(w.kind, "table");
    assert_eq!(w.cfg.db, road);
    assert_eq!(w.cfg.view, ws.db(road).unwrap().views[0].id, "the view is pinned with its filters");
    assert!(w.title.contains("Roadmap"));
    let p0 = ws.db(road).unwrap().props[0].id;
    let id2 = crate::dashcfg::pin_prop(&mut ws, road, p0, "donut");
    assert_eq!(ws.dash.widget(id2).unwrap().cfg.group, p0);
    assert_eq!(ws.dash.boards[0].widgets.len(), before + 2);
    // the whole workspace (with the dashboard) survives a save / load round trip
    let json = serde_json::to_string(&ws).unwrap();
    let back: Workspace = serde_json::from_str(&json).unwrap();
    assert_eq!(back.dash, ws.dash);
    // a workspace saved before dashboards existed gets the default board
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    v.as_object_mut().unwrap().remove("dash");
    let mut old: Workspace = serde_json::from_value(v).unwrap();
    assert!(old.dash.boards.is_empty());
    crate::dashcfg::ensure_seeded(&mut old);
    assert!(old.dash.boards[0].widgets.iter().any(|w| w.kind == "tasks"));
}

// ───────────────────────────── icons ─────────────────────────────

/// Every icon constant must exist in the embedded (subset) Lucide font — otherwise the UI shows an empty box.
#[test]
fn every_lucide_icon_exists_in_the_embedded_font() {
    let ctx = test_ctx();
    let lucide = egui::FontId::new(16.0, egui::FontFamily::Name("lucide".into()));
    let src = include_str!("lucide.rs");
    let mut n = 0;
    let mut missing = vec![];
    for line in src.lines().filter(|l| l.starts_with("pub const ") && l.contains("\\u{")) {
        let name = line.trim_start_matches("pub const ").split(':').next().unwrap_or("?");
        let hex = line.split("\\u{").nth(1).and_then(|r| r.split('}').next()).unwrap_or("0");
        let ch = char::from_u32(u32::from_str_radix(hex, 16).unwrap()).unwrap();
        n += 1;
        if !ctx.fonts(|f| f.has_glyph(&lucide, ch)) {
            missing.push(format!("{name} U+{hex}"));
        }
    }
    assert!(n > 250, "{n} icons found");
    assert!(missing.is_empty(), "icons without a glyph: {missing:?}");
}

/// The dashboard must not draw pictograms from the text font (they were missing → "icons not working"); it uses Lucide glyphs.
#[test]
fn dashboard_code_uses_icon_font_not_symbol_characters() {
    for (file, src) in [("dashboard.rs", include_str!("dashboard.rs")), ("dashedit.rs", include_str!("dashedit.rs")), ("dashwidgets.rs", include_str!("dashwidgets.rs"))] {
        for (n, line) in src.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            let code = code.split(" // ").next().unwrap_or("");
            for ch in code.chars() {
                let u = ch as u32;
                assert!(!((0x2190..=0x2BFF).contains(&u) || (0xFF00..=0xFFEF).contains(&u) || u >= 0x1F000), "{file}:{} uses symbol {ch:?} (U+{u:04X}) — use a Lucide glyph", n + 1);
            }
        }
    }
}

/// A Lucide glyph typed into ordinary text must resolve to Lucide (Inter's private-use alternates were stripped).
#[test]
fn icons_render_in_proportional_text() {
    let ctx = test_ctx();
    let f = egui::FontId::new(15.0, egui::FontFamily::Proportional);
    let b = crate::theme::bold(15.0);
    for g in [crate::lucide::LAYOUT_DASHBOARD, crate::lucide::ROCKET, crate::lucide::PLUS, crate::lucide::CHART_PIE, crate::lucide::FILE_PLUS, crate::lucide::TRENDING_UP] {
        let ch = g.chars().next().unwrap();
        assert!(ctx.fonts(|x| x.has_glyph(&f, ch)), "{g:?} in body text");
        assert!(ctx.fonts(|x| x.has_glyph(&b, ch)), "{g:?} in bold text");
    }
}

// ───────────────────────────── unified chart ─────────────────────────────

#[test]
fn plots_are_one_chart_widget_with_a_plot_type_setting() {
    use crate::dashcfg::{plot_of, Widget};
    crate::dashboard::testing::reset();
    let mut ws = dash_ws();
    crate::dashcfg::ensure_seeded(&mut ws);
    // the gallery offers a single Chart entry and no per-plot entries
    let gallery: Vec<&str> = crate::dashwidgets::all().into_iter().filter(|d| !crate::dashcfg::PLOT_KINDS.contains(&d.kind)).map(|d| d.kind).collect();
    assert!(gallery.contains(&"chart"));
    assert!(!gallery.iter().any(|k| ["bar", "pie", "line", "gauge"].contains(k)));
    // creating "chart" and creating a legacy plot kind both give a Chart widget
    let a = crate::dashedit::create(&mut ws, "chart", 0).unwrap();
    let b = crate::dashedit::create(&mut ws, "donut", 0).unwrap();
    assert_eq!(ws.dash.widget(a).unwrap().kind, "chart");
    assert_eq!(plot_of(ws.dash.widget(a).unwrap()).as_deref(), Some("bar"));
    assert_eq!(plot_of(ws.dash.widget(b).unwrap()).as_deref(), Some("donut"));
    // workspaces saved with one widget kind per plot are migrated
    let old = Widget { id: 9_999, kind: "radar".into(), title: String::new(), span: 4, rows: 3, cfg: Default::default() };
    ws.dash.boards[0].widgets.push(old);
    crate::dashcfg::ensure_seeded(&mut ws);
    let w = ws.dash.widget(9_999).unwrap();
    assert_eq!((w.kind.as_str(), plot_of(w).as_deref()), ("chart", Some("radar")));
    assert_eq!(crate::dashwidgets::def_for(w).unwrap().name, "Radar");
    // every plot type renders as a Chart widget, also after the plot type was switched in the settings
    let ctx = test_ctx();
    let pal = Palette::light();
    let mut t = 0.0;
    for k in crate::dashwidgets::CHART_KINDS {
        ws.dash.widget_mut(a).unwrap().cfg.extra.insert("plot".into(), k.to_string());
        for _ in 0..2 {
            frame(&ctx, &mut ws, &pal, vec2(1200.0, 900.0), &mut t, vec![]);
        }
    }
}

// ───────────────────────────── any page can be a dashboard ─────────────────────────────

#[test]
fn any_page_can_be_a_dashboard() {
    crate::dashboard::testing::reset();
    let mut ws = dash_ws();
    crate::dashcfg::ensure_seeded(&mut ws);
    let home_tabs = ws.dash.home_count();
    let pal = Palette::light();
    let ctx = test_ctx();
    let mut ed = crate::editor::EditorState::default();
    // an ordinary page becomes a dashboard and keeps its blocks
    let page = ws.add_page(None, crate::model::PageKind::Doc);
    let blocks = ws.page(page).unwrap().blocks.len();
    let board = ws.dash.ensure_page_board(page, "Dashboard");
    assert_eq!(ws.dash.ensure_page_board(page, "Dashboard"), board, "idempotent");
    assert_eq!(ws.dash.home_count(), home_tabs, "page boards are not Home tabs");
    crate::dashcfg::fill_board(&mut ws, board, "analytics");
    let id = crate::dashedit::create(&mut ws, "chart", board).unwrap();
    assert!(ws.dash.boards[ws.dash.board_index(board).unwrap()].widgets.iter().any(|w| w.id == id), "widget lands on the page board");
    assert!(ws.dash.board().unwrap().widgets.iter().all(|w| w.id != id), "…and not on the Home board");
    let mut t = 0.0;
    for _ in 0..3 {
        t += 0.05;
        let mut input = RawInput::default();
        input.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1100.0, 800.0)));
        input.time = Some(t);
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    crate::editor::show_page(ui, &mut ws, page, &mut ed, &pal);
                });
            });
        });
    }
    assert_eq!(ws.page(page).unwrap().blocks.len(), blocks);
    // a dashboard page created from the dashboard sits in the page tree and survives save / load
    let dp = crate::dashcfg::new_dashboard_page(&mut ws, Some(page), "starter");
    assert_eq!(ws.page(dp).unwrap().parent, Some(page));
    assert!(ws.dash.page_board(dp).is_some());
    let json = serde_json::to_string(&ws).unwrap();
    let back: Workspace = serde_json::from_str(&json).unwrap();
    assert_eq!(back.dash.page_board(dp), ws.dash.page_board(dp));
    // the board can be removed again without touching the page
    ws.dash.drop_page_board(dp);
    assert!(ws.dash.page_board(dp).is_none() && ws.page(dp).is_some());
}

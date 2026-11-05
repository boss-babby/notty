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
        let id = crate::dashedit::create(&mut ws, d.kind).unwrap_or_else(|| panic!("{} can be created", d.kind));
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

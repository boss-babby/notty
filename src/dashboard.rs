//! Home dashboard — one screen that answers "what needs me today?" for any kind of workspace (CRM, projects, studies, teams).
//!
//! Everything is computed live from the workspace; nothing has to be configured:
//! * KPI cards (pages, databases, records, open / done tasks, words written)
//! * **Pipeline & analytics** — for every database that has a *Select* property (stage, status, priority, owner…) a bar chart of
//!   records per option, and when the database also has a *Number* property (value, price, hours) the sum per option — a sales
//!   pipeline, a budget or a workload chart without any setup
//! * **My tasks** — open to-do blocks from every page, tick them off right here
//! * **Upcoming & overdue** — database records whose date falls in the next two weeks (or the last week)
//! * Recently edited, favourites and a 14-day activity chart

use egui::{pos2, vec2, Align2, Color32, CursorIcon, Id, RichText, ScrollArea, Sense, Stroke, Ui};

use crate::auth::now_ms;
use crate::dates;
use crate::graph::Out;
use crate::model::{BlockKind, PropKind, Val, Workspace};
use crate::theme::{bold, body, tag_color, Palette};
use crate::widgets::{text_fit, with_alpha};

enum Card {
    Tasks,
    Upcoming,
    Recent,
    Favorites,
    Activity,
    Chart(u64),
}

fn rows_of(ws: &Workspace, db: u64) -> Vec<u64> {
    ws.pages.iter().filter(|p| p.row && !p.trashed && p.parent == Some(db)).map(|p| p.id).collect()
}

fn card_frame(pal: &Palette) -> egui::Frame {
    egui::Frame::new().fill(pal.card).stroke(Stroke::new(1.0_f32, pal.border)).corner_radius(14).inner_margin(egui::Margin::same(14))
}

fn heading(ui: &mut Ui, pal: &Palette, title: &str, sub: &str) {
    ui.label(RichText::new(title).font(bold(15.0)).color(pal.text));
    if !sub.is_empty() {
        ui.label(RichText::new(sub).font(body(12.0)).color(pal.muted));
    }
    ui.add_space(8.0);
}

/// Horizontal bars: (label, value, colour, text shown at the right).
fn bars(ui: &mut Ui, pal: &Palette, items: &[(String, f64, Color32, String)]) {
    let max = items.iter().map(|i| i.1).fold(0.0_f64, f64::max).max(1e-9);
    let w = ui.available_width();
    let label_w = (w * 0.34).clamp(70.0, 150.0);
    for (name, v, col, txt) in items {
        let (r, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
        text_fit(ui.painter(), pos2(r.left(), r.center().y), Align2::LEFT_CENTER, name, body(13.0), pal.text, label_w - 8.0);
        let track = egui::Rect::from_min_max(pos2(r.left() + label_w, r.center().y - 7.0), pos2(r.right() - 58.0, r.center().y + 7.0));
        ui.painter().rect_filled(track, 7.0, with_alpha(pal.muted, 0.14));
        let fw = (track.width() * (*v / max) as f32).max(if *v > 0.0 { 6.0 } else { 0.0 });
        if fw > 0.0 {
            ui.painter().rect_filled(egui::Rect::from_min_size(track.min, vec2(fw, track.height())), 7.0, *col);
        }
        ui.painter().text(pos2(r.right(), r.center().y), Align2::RIGHT_CENTER, txt, bold(12.5), pal.text);
    }
}

fn fmt_num(v: f64) -> String {
    if v.abs() >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v.abs() >= 10_000.0 {
        format!("{:.0}k", v / 1000.0)
    } else if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

fn link_row(ui: &mut Ui, pal: &Palette, id: Id, left: &str, right: &str, right_col: Color32) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
    let resp = ui.interact(r, id, Sense::click()).union(resp);
    if resp.hovered() {
        ui.painter().rect_filled(r, 7.0, pal.hover);
    }
    let rw = if right.is_empty() { 0.0 } else { 96.0 };
    text_fit(ui.painter(), pos2(r.left() + 6.0, r.center().y), Align2::LEFT_CENTER, left, body(13.5), pal.text, r.width() - rw - 14.0);
    if !right.is_empty() {
        text_fit(ui.painter(), pos2(r.right() - 6.0, r.center().y), Align2::RIGHT_CENTER, right, body(12.0), right_col, rw);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

fn draw_card(ui: &mut Ui, ws: &mut Workspace, pal: &Palette, c: &Card, out: &mut Out) {
    card_frame(pal).show(ui, |ui| {
        ui.set_width(ui.available_width());
        match c {
            Card::Tasks => {
                let mut tasks: Vec<(u64, usize, String, String)> = vec![];
                for p in ws.pages.iter().filter(|p| !p.trashed) {
                    for (i, b) in p.blocks.iter().enumerate() {
                        if b.kind == BlockKind::Todo && !b.checked && !b.text.trim().is_empty() {
                            tasks.push((p.id, i, b.text.clone(), if p.title.is_empty() { "Untitled".into() } else { p.title.clone() }));
                        }
                    }
                }
                let total = tasks.len();
                heading(ui, pal, "My tasks", &format!("{total} open to-do{} across all pages", if total == 1 { "" } else { "s" }));
                if tasks.is_empty() {
                    ui.label(RichText::new("All clear — add to-dos with /todo on any page.").font(body(13.0)).color(pal.muted));
                }
                let mut tick: Option<(u64, usize)> = None;
                for (pid, bi, text, page) in tasks.iter().take(9) {
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
                    let cb = egui::Rect::from_center_size(pos2(r.left() + 12.0, r.center().y), vec2(18.0, 18.0));
                    let cresp = ui.interact(cb.expand(6.0), Id::new(("dash_tick", pid, bi)), Sense::click());
                    ui.painter().rect_stroke(cb, 5.0, Stroke::new(1.6_f32, if cresp.hovered() { pal.accent } else { pal.muted }), egui::StrokeKind::Inside);
                    if cresp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        tick = Some((*pid, *bi));
                    }
                    if resp.hovered() {
                        ui.painter().rect_filled(egui::Rect::from_min_max(pos2(r.left() + 28.0, r.top()), r.max), 7.0, pal.hover);
                    }
                    let pw = (r.width() * 0.3).clamp(60.0, 130.0);
                    text_fit(ui.painter(), pos2(r.left() + 34.0, r.center().y), Align2::LEFT_CENTER, text, body(13.5), pal.text, r.width() - pw - 44.0);
                    text_fit(ui.painter(), pos2(r.right() - 4.0, r.center().y), Align2::RIGHT_CENTER, page, body(11.5), pal.muted, pw);
                    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() && tick.is_none() {
                        out.open = Some(*pid);
                    }
                }
                if total > 9 {
                    ui.label(RichText::new(format!("+ {} more", total - 9)).font(body(12.0)).color(pal.muted));
                }
                if let Some((pid, bi)) = tick {
                    if let Some(p) = ws.page_mut(pid) {
                        if let Some(b) = p.blocks.get_mut(bi) {
                            b.checked = true;
                        }
                        p.updated = now_ms();
                    }
                }
            }
            Card::Upcoming => {
                let today = dates::today_day();
                let mut items: Vec<(i64, i64, u64, String, String)> = vec![];
                for db in ws.pages.iter().filter(|p| p.db.is_some() && !p.trashed) {
                    let Some(d) = db.db.as_ref() else { continue };
                    for prop in d.props.iter().filter(|p| p.kind == PropKind::Date) {
                        for rid in rows_of(ws, db.id) {
                            let Some(row) = ws.page(rid) else { continue };
                            if let Some(Val::Text(s)) = row.props.get(&prop.id) {
                                if let Some((a, b)) = dates::span(s) {
                                    if b >= today - 7 && a <= today + 14 {
                                        items.push((a, b, rid, if row.title.is_empty() { "Untitled".into() } else { row.title.clone() }, format!("{} · {}", db.title, prop.name)));
                                    }
                                }
                            }
                        }
                    }
                }
                items.sort_by_key(|i| i.0);
                heading(ui, pal, "Upcoming & overdue", "Records dated in the next 14 days or the last week");
                if items.is_empty() {
                    ui.label(RichText::new("Nothing scheduled. Give a database a Date property to see it here.").font(body(13.0)).color(pal.muted));
                }
                for (a, b, rid, title, src) in items.iter().take(8) {
                    let overdue = *b < today;
                    let label = if overdue { format!("{}d overdue", today - b) } else if *a <= today { "today".to_owned() } else if *a == today + 1 { "tomorrow".to_owned() } else { format!("in {}d", a - today) };
                    let col = if overdue { pal.danger } else if *a <= today { tag_color(3) } else { pal.muted };
                    if link_row(ui, pal, Id::new(("dash_up", rid, a)), title, &label, col) {
                        out.open = Some(*rid);
                    }
                    let _ = src;
                }
            }
            Card::Recent => {
                heading(ui, pal, "Recently edited", "");
                let mut pages: Vec<&crate::model::Page> = ws.pages.iter().filter(|p| !p.trashed).collect();
                pages.sort_by(|a, b| b.updated.partial_cmp(&a.updated).unwrap_or(std::cmp::Ordering::Equal));
                let list: Vec<(u64, String)> = pages.iter().take(7).map(|p| (p.id, if p.title.is_empty() { "Untitled".into() } else { p.title.clone() })).collect();
                for (id, t) in list {
                    if link_row(ui, pal, Id::new(("dash_rc", id)), &t, "", pal.muted) {
                        out.open = Some(id);
                    }
                }
            }
            Card::Favorites => {
                heading(ui, pal, "Favorites", "Star a page to pin it here");
                let list: Vec<(u64, String)> = ws.pages.iter().filter(|p| p.favorite && !p.trashed).take(8).map(|p| (p.id, if p.title.is_empty() { "Untitled".into() } else { p.title.clone() })).collect();
                if list.is_empty() {
                    ui.label(RichText::new("No favorites yet.").font(body(13.0)).color(pal.muted));
                }
                for (id, t) in list {
                    if link_row(ui, pal, Id::new(("dash_fv", id)), &t, "", pal.muted) {
                        out.open = Some(id);
                    }
                }
            }
            Card::Activity => {
                heading(ui, pal, "Activity", "Pages edited per day, last 14 days");
                let today = (now_ms() / 86_400_000.0).floor() as i64;
                let mut counts = [0usize; 14];
                for p in ws.pages.iter().filter(|p| !p.trashed) {
                    let d = (p.updated / 86_400_000.0).floor() as i64;
                    let ago = today - d;
                    if (0..14).contains(&ago) {
                        counts[13 - ago as usize] += 1;
                    }
                }
                let max = counts.iter().copied().max().unwrap_or(1).max(1) as f32;
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 84.0), Sense::hover());
                let bw = r.width() / 14.0;
                for (i, c) in counts.iter().enumerate() {
                    let h = (r.height() - 16.0) * (*c as f32 / max);
                    let b = egui::Rect::from_min_max(pos2(r.left() + i as f32 * bw + 3.0, r.bottom() - 14.0 - h.max(2.0)), pos2(r.left() + (i as f32 + 1.0) * bw - 3.0, r.bottom() - 14.0));
                    ui.painter().rect_filled(b, 4.0, if i == 13 { pal.accent } else { with_alpha(pal.accent, 0.45) });
                }
                ui.painter().text(pos2(r.left(), r.bottom()), Align2::LEFT_BOTTOM, "14 days ago", body(10.5), pal.muted);
                ui.painter().text(pos2(r.right(), r.bottom()), Align2::RIGHT_BOTTOM, "today", body(10.5), pal.muted);
            }
            Card::Chart(db_id) => {
                let Some(dbp) = ws.page(*db_id) else { return };
                let Some(db) = dbp.db.as_ref() else { return };
                let Some(sel) = db.props.iter().find(|p| p.kind == PropKind::Select) else { return };
                let num = db.props.iter().find(|p| p.kind == PropKind::Number);
                let rows = rows_of(ws, *db_id);
                heading(ui, pal, &format!("{}  {}", if dbp.icon.is_empty() { "" } else { crate::icons::icon_text(&dbp.icon) }, dbp.title).trim().to_owned(), &format!("{} records by {}", rows.len(), sel.name));
                let mut counts: Vec<(String, f64, Color32, f64)> = sel.opts.iter().map(|o| (o.name.clone(), 0.0, tag_color(o.color), 0.0)).collect();
                let mut none = (0.0, 0.0);
                for rid in &rows {
                    let Some(row) = ws.page(*rid) else { continue };
                    let n = num.and_then(|n| match row.props.get(&n.id) {
                        Some(Val::Num(v)) => Some(*v),
                        _ => None,
                    });
                    match row.props.get(&sel.id) {
                        Some(Val::Sel(oid)) => {
                            if let Some(i) = sel.opts.iter().position(|o| o.id == *oid) {
                                counts[i].1 += 1.0;
                                counts[i].3 += n.unwrap_or(0.0);
                            }
                        }
                        _ => {
                            none.0 += 1.0;
                            none.1 += n.unwrap_or(0.0);
                        }
                    }
                }
                if none.0 > 0.0 {
                    counts.push(("No value".into(), none.0, with_alpha(pal.muted, 0.6), none.1));
                }
                let items: Vec<(String, f64, Color32, String)> = counts.iter().map(|c| (c.0.clone(), c.1, c.2, fmt_num(c.1))).collect();
                bars(ui, pal, &items);
                if let Some(n) = num {
                    ui.add_space(8.0);
                    ui.label(RichText::new(format!("Sum of {}", n.name)).font(bold(12.5)).color(pal.muted));
                    ui.add_space(4.0);
                    let items: Vec<(String, f64, Color32, String)> = counts.iter().map(|c| (c.0.clone(), c.3, c.2, fmt_num(c.3))).collect();
                    bars(ui, pal, &items);
                    let total: f64 = counts.iter().map(|c| c.3).sum();
                    ui.add_space(6.0);
                    ui.label(RichText::new(format!("Total  {}", fmt_num(total))).font(bold(13.0)).color(pal.text));
                }
                if ui.add(egui::Button::new(RichText::new("Open database").font(body(12.5))).frame(false)).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    out.open = Some(*db_id);
                }
            }
        }
    });
    ui.add_space(12.0);
}

fn kpi(ui: &mut Ui, pal: &Palette, w: f32, label: &str, value: String, col: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(w, 74.0), Sense::hover());
    ui.painter().rect_filled(r, 14.0, pal.card);
    ui.painter().rect_stroke(r, 14.0, Stroke::new(1.0_f32, pal.border), egui::StrokeKind::Inside);
    ui.painter().rect_filled(egui::Rect::from_min_size(pos2(r.left() + 12.0, r.top() + 14.0), vec2(4.0, 22.0)), 2.0, col);
    ui.painter().text(pos2(r.left() + 26.0, r.top() + 26.0), Align2::LEFT_CENTER, value, bold(24.0), pal.text);
    text_fit(ui.painter(), pos2(r.left() + 14.0, r.bottom() - 16.0), Align2::LEFT_CENTER, label, body(12.5), pal.muted, w - 24.0);
}

fn action(ui: &mut Ui, pal: &Palette, text: &str) -> bool {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), body(13.0), pal.text);
    let (r, resp) = ui.allocate_exact_size(vec2(galley.size().x + 22.0, 30.0), Sense::click());
    ui.painter().rect_filled(r, 15.0, if resp.hovered() { pal.active } else { pal.hover });
    ui.painter().galley(pos2(r.left() + 11.0, r.center().y - galley.size().y / 2.0), galley, pal.text);
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

/// What the dashboard wants the app to do besides opening a page.
#[derive(Default)]
pub struct Nav {
    pub out: Out,
    pub new_page: bool,
    pub new_db: bool,
    pub journal: bool,
    pub graph: bool,
}

pub fn show(ui: &mut Ui, ws: &mut Workspace, pal: &Palette) -> Nav {
    let mut nav = Nav::default();
    let w = ui.available_width();
    let cols = ((w / 360.0).floor() as usize).clamp(1, 3);
    let pad = if w < 520.0 { 12.0 } else { 28.0 };
    let key = Id::new("dash_scroll");
    let sa = ScrollArea::vertical().id_salt("dash_scroll_area").auto_shrink([false, false]);
    let over = ui.rect_contains_pointer(ui.max_rect());
    crate::smooth::scroll_area(ui, key, sa, over, |ui| {
        ui.add_space(14.0);
        egui::Frame::new().inner_margin(egui::Margin::symmetric(pad as i8, 0)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("{}", if ws.name.is_empty() { "Home" } else { &ws.name })).font(bold(if w < 520.0 { 24.0 } else { 30.0 })).color(pal.text));
            ui.label(RichText::new(dates::long_today()).font(body(14.0)).color(pal.muted));
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                if action(ui, pal, "＋ New page") {
                    nav.new_page = true;
                }
                if action(ui, pal, "▦ New database") {
                    nav.new_db = true;
                }
                if action(ui, pal, "☀ Today's journal") {
                    nav.journal = true;
                }
                if action(ui, pal, "✦ Graph view") {
                    nav.graph = true;
                }
            });
            ui.add_space(14.0);

            // KPIs
            let live: Vec<&crate::model::Page> = ws.pages.iter().filter(|p| !p.trashed).collect();
            let pages = live.iter().filter(|p| !p.row && p.db.is_none()).count();
            let dbs = live.iter().filter(|p| p.db.is_some()).count();
            let records = live.iter().filter(|p| p.row).count();
            let (mut open_t, mut done_t, mut words) = (0usize, 0usize, 0usize);
            for p in &live {
                for b in &p.blocks {
                    match (b.kind, b.checked) {
                        (BlockKind::Todo, false) => open_t += 1,
                        (BlockKind::Todo, true) => done_t += 1,
                        _ => {}
                    }
                    words += b.text.split_whitespace().count();
                }
            }
            let kc = if w < 520.0 { 2 } else if w < 900.0 { 3 } else { 6 };
            let kw = ((w - pad * 2.0 - 8.0 * (kc as f32 - 1.0)) / kc as f32).floor();
            let stats: Vec<(&str, String, Color32)> = vec![
                ("Pages", pages.to_string(), pal.accent),
                ("Databases", dbs.to_string(), tag_color(3)),
                ("Records", records.to_string(), tag_color(2)),
                ("Open tasks", open_t.to_string(), tag_color(5)),
                ("Done tasks", done_t.to_string(), tag_color(2)),
                ("Words written", fmt_num(words as f64), tag_color(4)),
            ];
            for chunk in stats.chunks(kc) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
                    for (l, v, c) in chunk {
                        kpi(ui, pal, kw, l, v.clone(), *c);
                    }
                });
                ui.add_space(8.0);
            }
            ui.add_space(8.0);

            // cards, dealt into columns
            let mut cards: Vec<Card> = vec![Card::Tasks, Card::Upcoming];
            let chart_dbs: Vec<u64> = ws.pages.iter().filter(|p| !p.trashed && p.db.as_ref().map_or(false, |d| d.props.iter().any(|x| x.kind == PropKind::Select))).map(|p| p.id).take(4).collect();
            cards.extend(chart_dbs.into_iter().map(Card::Chart));
            cards.extend([Card::Recent, Card::Favorites, Card::Activity]);
            let colw = (ui.available_width() - 14.0 * (cols as f32 - 1.0)) / cols as f32;
            let mut buckets: Vec<Vec<&Card>> = (0..cols).map(|_| vec![]).collect();
            for (i, c) in cards.iter().enumerate() {
                buckets[i % cols].push(c);
            }
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing = vec2(14.0, 0.0);
                for b in buckets {
                    ui.vertical(|ui| {
                        ui.set_width(colw);
                        for c in b {
                            draw_card(ui, ws, pal, c, &mut nav.out);
                        }
                    });
                }
            });
            ui.add_space(30.0);
        });
    });
    nav
}

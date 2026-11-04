//! Timeline (Gantt) view: date-range bars you can drag (move) or resize from either end, plus dependency arrows between rows.
//!
//! * Drag the body of a bar to reschedule it, drag its left / right edge to change the start / end.
//! * Drag the round connector on the right end of a bar onto another bar to make that bar *depend on* the first one
//!   (arrows are red when the dependency is violated; “Auto-schedule” pushes dependants out of the way).
//! * Click an empty cell of an unscheduled row to give it a date, drag across cells to create a range.
//! * Scrolling: drag the background (touch / mouse) or use the scroll bar. The view opens centred on today.

use std::collections::HashMap;

use egui::{pos2, vec2, Align2, Color32, CursorIcon, Id, Pos2, Rect, ScrollArea, Sense, Shape, Stroke, Ui};

use crate::dates;
use crate::database::{pill, title_of, A};
use crate::model::*;
use crate::theme::{bold, body, lerp_color, tag_color, Palette};
use crate::widgets::with_alpha;

const ROW_H: f32 = 38.0;
const HEAD_H: f32 = 46.0;
const BAR_H: f32 = 24.0;
const ZOOMS: [f32; 5] = [9.0, 16.0, 28.0, 44.0, 66.0];

pub fn zoom_index(view: &View) -> usize {
    if view.zoom == 0 {
        2
    } else {
        (view.zoom as usize - 1).min(ZOOMS.len() - 1)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Move,
    Left,
    Right,
}

#[allow(clippy::too_many_arguments)]
pub fn timeline(ui: &mut Ui, ws: &Workspace, db_id: u64, db: &Database, vi: usize, rows: &[u64], pal: &Palette, w: f32, acts: &mut Vec<A>) {
    let view = &db.views[vi];
    let ctx = ui.ctx().clone();
    let Some(dp) = db.date_prop(view) else {
        ui.add_space(24.0);
        ui.label(egui::RichText::new("The timeline needs a Date property.").font(body(14.0)).color(pal.muted));
        ui.add_space(6.0);
        if pill(ui, pal, "+  Add a date property", false, true).clicked() {
            acts.push(A::AddProp(PropKind::Date));
        }
        return;
    };
    let dep = db.dep_prop(view, db_id);
    let zi = zoom_index(view);
    let dw = ZOOMS[zi];
    let today = dates::today_day();

    // toolbar
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        if pill(ui, pal, "−", false, false).on_hover_text("Zoom out").clicked() && zi > 0 {
            acts.push(A::SetZoom(vi, zi as u8));
        }
        if pill(ui, pal, "+", false, false).on_hover_text("Zoom in").clicked() && zi + 1 < ZOOMS.len() {
            acts.push(A::SetZoom(vi, zi as u8 + 2));
        }
        if pill(ui, pal, "Today", false, false).clicked() {
            ctx.data_mut(|d| d.insert_temp(Id::new(("tl_goto", db_id)), true));
        }
        if dep.is_none() {
            if pill(ui, pal, "Enable dependencies", false, false).on_hover_text("Adds a “Blocked by” relation to this database").clicked() {
                acts.push(A::AddDepProp);
            }
        } else if pill(ui, pal, "Auto-schedule", false, false).on_hover_text("Moves rows so that none starts before the rows it depends on have finished").clicked() {
            acts.push(A::Reschedule);
        }
        ui.label(egui::RichText::new(if dep.is_some() { "Drag bars to move · edges to resize · ● onto another bar to link" } else { "Drag bars to move · drag edges to resize" }).font(body(12.0)).color(pal.muted));
    });
    ui.add_space(4.0);

    // data
    struct Item {
        id: u64,
        title: String,
        span: Option<(i64, i64)>,
        color: Color32,
    }
    let status = db.group_prop(view);
    let mut items: Vec<Item> = vec![];
    for rid in rows {
        let Some(r) = ws.page(*rid) else { continue };
        let span = match r.props.get(&dp) {
            Some(Val::Text(s)) => dates::span(s),
            _ => None,
        };
        let color = status
            .and_then(|g| match r.props.get(&g) {
                Some(Val::Sel(o)) => db.opt(g, *o).map(|o| tag_color(o.color)),
                _ => None,
            })
            .unwrap_or(pal.accent);
        items.push(Item { id: *rid, title: title_of(r).to_owned(), span, color });
    }
    if items.is_empty() {
        ui.label(egui::RichText::new("No rows yet — add one with “New”.").font(body(13.0)).color(pal.muted));
        return;
    }

    // live drag previews (read before laying out so arrows follow the bars)
    let mut preview: HashMap<u64, (i64, i64)> = HashMap::new();
    for it in &items {
        if let Some((s, e)) = it.span {
            if let Some((mode, dx)) = ctx.data(|d| d.get_temp::<(Mode, f32)>(Id::new(("tl_drag", it.id)))) {
                let dd = (dx / dw).round() as i64;
                let (ns, ne) = match mode {
                    Mode::Move => (s + dd, e + dd),
                    Mode::Left => ((s + dd).min(e), e),
                    Mode::Right => (s, (e + dd).max(s)),
                };
                preview.insert(it.id, (ns, ne));
            }
        }
    }
    let span_of = |id: u64, it: &Item| -> Option<(i64, i64)> { preview.get(&id).copied().or(it.span) };
    let spans: Vec<(i64, i64)> = items.iter().filter_map(|i| span_of(i.id, i)).collect();
    let (lo, hi) = if spans.is_empty() { (today, today) } else { (spans.iter().map(|s| s.0).min().unwrap_or(today).min(today), spans.iter().map(|s| s.1).max().unwrap_or(today).max(today)) };
    let min_day = lo - 10;
    let max_day = (hi + 45).max(min_day + (w / dw) as i64 + 4);
    let n_days = (max_day - min_day + 1) as f32;
    let total_w = n_days * dw;
    let label_w = if w < 560.0 { 112.0 } else { 220.0 };
    let height = HEAD_H + items.len() as f32 * ROW_H;

    let (outer, _) = ui.allocate_exact_size(vec2(w, height + 14.0), Sense::hover());
    let label_rect = Rect::from_min_size(outer.min, vec2(label_w, height));
    let grid_outer = Rect::from_min_size(pos2(outer.left() + label_w, outer.top()), vec2(w - label_w, height + 14.0));

    // labels
    {
        let p = ui.painter();
        p.rect_filled(label_rect, 0.0, pal.bg);
        p.text(pos2(label_rect.left() + 8.0, label_rect.top() + HEAD_H - 14.0), Align2::LEFT_CENTER, "Name", bold(11.5), pal.muted);
        p.hline(label_rect.x_range(), label_rect.top() + HEAD_H, Stroke::new(1.0_f32, pal.border));
    }
    for (i, it) in items.iter().enumerate() {
        let r = Rect::from_min_size(pos2(label_rect.left(), label_rect.top() + HEAD_H + i as f32 * ROW_H), vec2(label_w, ROW_H));
        let resp = ui.interact(r, Id::new(("tl_label", it.id)), Sense::click());
        let t = ctx.animate_bool_with_time(resp.id, resp.hovered(), 0.1);
        ui.painter().rect_filled(r, 0.0, with_alpha(pal.hover, t * 0.7));
        ui.painter().hline(r.x_range(), r.bottom(), Stroke::new(1.0_f32, with_alpha(pal.border, 0.7)));
        let col = if it.span.is_some() { pal.text } else { pal.muted };
        crate::widgets::text_fit(ui.painter(), pos2(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, &it.title, bold(13.5), col, label_w - 16.0);
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            acts.push(A::Open(it.id));
        }
    }
    ui.painter().vline(label_rect.right(), label_rect.y_range(), Stroke::new(1.0_f32, pal.border));

    // grid
    let goto: bool = ctx.data(|d| d.get_temp(Id::new(("tl_goto", db_id)))).unwrap_or(false);
    let inited_id = Id::new(("tl_init", db_id, view.id));
    let inited: bool = ctx.data(|d| d.get_temp(inited_id)).unwrap_or(false);
    let mut sa = ScrollArea::horizontal().id_salt(("tl_scroll", db_id, view.id)).auto_shrink([false, false]);
    if goto || !inited {
        let off = ((today - min_day) as f32 * dw - (grid_outer.width() * 0.3)).max(0.0);
        sa = sa.horizontal_scroll_offset(off);
        ctx.data_mut(|d| {
            d.insert_temp(inited_id, true);
            d.insert_temp(Id::new(("tl_goto", db_id)), false);
        });
    }
    let mut bar_rects: HashMap<u64, Rect> = HashMap::new();
    let mut to_draw_links: Vec<(u64, Rect)> = vec![];
    let mut link_drop: Option<(u64, Pos2)> = None;
    let mut ui_child = ui.new_child(egui::UiBuilder::new().max_rect(grid_outer).layout(egui::Layout::top_down(egui::Align::Min)));
    sa.show(&mut ui_child, |ui| {
        let (content, _) = ui.allocate_exact_size(vec2(total_w, height), Sense::hover());
        let p = ui.painter().clone();
        let x_of = |d: i64| content.left() + (d - min_day) as f32 * dw;
        // header + weekend shading + grid lines
        for d in min_day..=max_day {
            let x = x_of(d);
            let (y, m, dd) = dates::civil_from_days(d);
            let wd = dates::weekday(y, m, dd);
            if wd >= 5 {
                p.rect_filled(Rect::from_min_size(pos2(x, content.top() + HEAD_H), vec2(dw, height - HEAD_H)), 0.0, with_alpha(pal.hover, 0.45));
            }
            if wd == 0 || dd == 1 {
                p.vline(x, (content.top() + 20.0)..=content.bottom(), Stroke::new(1.0_f32, with_alpha(pal.border, if dd == 1 { 0.9 } else { 0.45 })));
            }
            if dd == 1 || d == min_day {
                p.text(pos2(x + 6.0, content.top() + 10.0), Align2::LEFT_CENTER, format!("{} {}", &dates::MONTHS[m as usize - 1][..3], y), bold(11.5), pal.text);
            }
            if dw >= 24.0 {
                p.text(pos2(x + dw / 2.0, content.top() + 34.0), Align2::CENTER_CENTER, dd.to_string(), body(11.0), if wd >= 5 { pal.muted } else { pal.text });
            } else if wd == 0 && dw >= 9.0 {
                p.text(pos2(x + 2.0, content.top() + 34.0), Align2::LEFT_CENTER, dd.to_string(), body(10.5), pal.muted);
            }
        }
        p.hline(content.x_range(), content.top() + HEAD_H, Stroke::new(1.0_f32, pal.border));
        for i in 0..items.len() {
            let y = content.top() + HEAD_H + (i + 1) as f32 * ROW_H;
            p.hline(content.x_range(), y, Stroke::new(1.0_f32, with_alpha(pal.border, 0.7)));
        }
        // today marker
        let tx = x_of(today) + dw / 2.0;
        p.vline(tx, (content.top() + 20.0)..=content.bottom(), Stroke::new(1.5_f32, pal.accent));
        p.rect_filled(Rect::from_center_size(pos2(tx, content.top() + 34.0), vec2(if dw >= 24.0 { dw - 2.0 } else { 18.0 }, 16.0)), 8.0, pal.accent);
        p.text(pos2(tx, content.top() + 34.0), Align2::CENTER_CENTER, dates::civil_from_days(today).2.to_string(), bold(10.5), Color32::WHITE);

        // rows
        for (i, it) in items.iter().enumerate() {
            let row_top = content.top() + HEAD_H + i as f32 * ROW_H;
            let row_rect = Rect::from_min_size(pos2(content.left(), row_top), vec2(total_w, ROW_H));
            match span_of(it.id, it) {
                None => {
                    // unscheduled: click a day to schedule, drag to create a range
                    let resp = ui.interact(row_rect, Id::new(("tl_new", it.id)), Sense::click_and_drag());
                    let day_at = |pos: Pos2| min_day + ((pos.x - content.left()) / dw).floor() as i64;
                    if resp.hovered() || resp.dragged() {
                        if let Some(pos) = ctx.pointer_hover_pos() {
                            let d = day_at(pos);
                            let r = Rect::from_min_size(pos2(x_of(d), row_top + 6.0), vec2(dw, ROW_H - 12.0));
                            p.rect_filled(r, 5.0, with_alpha(it.color, 0.25));
                            if !resp.dragged() {
                                p.text(pos2(pos.x + 12.0, row_top + ROW_H / 2.0), Align2::LEFT_CENTER, "click to schedule", body(11.5), pal.muted);
                            }
                        }
                    }
                    if resp.drag_started() {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            ctx.data_mut(|d| d.insert_temp(Id::new(("tl_newstart", it.id)), day_at(pos)));
                        }
                    }
                    if resp.dragged() {
                        let s: Option<i64> = ctx.data(|d| d.get_temp(Id::new(("tl_newstart", it.id))));
                        if let (Some(s), Some(pos)) = (s, resp.interact_pointer_pos()) {
                            let e = day_at(pos);
                            let (a, b) = (s.min(e), s.max(e));
                            p.rect_filled(Rect::from_min_max(pos2(x_of(a), row_top + 7.0), pos2(x_of(b + 1), row_top + ROW_H - 7.0)), 6.0, with_alpha(it.color, 0.5));
                        }
                    }
                    if resp.drag_stopped() {
                        let s: Option<i64> = ctx.data(|d| d.get_temp(Id::new(("tl_newstart", it.id))));
                        if let (Some(s), Some(pos)) = (s, resp.interact_pointer_pos().or(ctx.pointer_hover_pos())) {
                            acts.push(A::SetVal(it.id, dp, Some(Val::Text(dates::span_value(s, day_at(pos))))));
                        }
                    } else if resp.clicked() {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            acts.push(A::SetVal(it.id, dp, Some(Val::Text(dates::iso_of_day(day_at(pos))))));
                        }
                    }
                }
                Some((s, e)) => {
                    let x0 = x_of(s);
                    let x1 = x_of(e + 1);
                    let r = Rect::from_min_max(pos2(x0 + 1.0, row_top + (ROW_H - BAR_H) / 2.0), pos2((x1 - 1.0).max(x0 + 8.0), row_top + (ROW_H + BAR_H) / 2.0));
                    bar_rects.insert(it.id, r);
                    let dragging = preview.contains_key(&it.id);
                    // hit zones
                    let grip = (r.width() * 0.3).clamp(3.0, 9.0);
                    let left = Rect::from_min_max(r.min, pos2(r.left() + grip, r.bottom()));
                    let right = Rect::from_min_max(pos2(r.right() - grip, r.top()), r.max);
                    let mid = Rect::from_min_max(pos2(left.right(), r.top()), pos2(right.left(), r.bottom()));
                    let mut hover = false;
                    for (zone, mode, cursor) in [(mid, Mode::Move, CursorIcon::Grab), (left, Mode::Left, CursorIcon::ResizeHorizontal), (right, Mode::Right, CursorIcon::ResizeHorizontal)] {
                        if zone.width() <= 0.0 {
                            continue;
                        }
                        let id = Id::new(("tl_bar", it.id, mode as u8 as usize + if mode == Mode::Move { 0 } else if mode == Mode::Left { 1 } else { 2 }));
                        let resp = ui.interact(zone, id, Sense::click_and_drag());
                        hover |= resp.hovered() || resp.dragged();
                        let key = Id::new(("tl_drag", it.id));
                        if resp.hovered() || resp.dragged() {
                            ui.ctx().set_cursor_icon(if resp.dragged() && mode == Mode::Move { CursorIcon::Grabbing } else { cursor });
                        }
                        if resp.drag_started() {
                            ctx.data_mut(|d| d.insert_temp(key, (mode, 0.0_f32)));
                        }
                        if resp.dragged() {
                            ctx.data_mut(|d| {
                                if let Some((m, dx)) = d.get_temp::<(Mode, f32)>(key) {
                                    d.insert_temp(key, (m, dx + resp.drag_delta().x));
                                }
                            });
                            ctx.request_repaint();
                        }
                        if resp.drag_stopped() {
                            if let Some((ns, ne)) = preview.get(&it.id) {
                                if Some((*ns, *ne)) != it.span {
                                    acts.push(A::SetVal(it.id, dp, Some(Val::Text(dates::span_value(*ns, *ne)))));
                                }
                            }
                            ctx.data_mut(|d| d.remove::<(Mode, f32)>(key));
                        } else if resp.clicked() && mode == Mode::Move {
                            acts.push(A::Open(it.id));
                        }
                        if let Some(pos) = crate::widgets::right_clicked(ui, zone) {
                            acts.push(A::RowMenu(it.id, pos));
                        }
                    }
                    let base = it.color;
                    p.rect_filled(r.translate(vec2(0.0, 2.0)), 7.0, Color32::from_black_alpha(if dragging { 40 } else { 14 }));
                    p.rect_filled(r, 7.0, lerp_color(base, pal.bg, if hover || dragging { 0.0 } else { 0.12 }));
                    p.rect_stroke(r, 7.0, Stroke::new(1.0_f32, base.gamma_multiply(0.6)), egui::StrokeKind::Inside);
                    if hover || dragging {
                        for gx in [r.left() + 3.5, r.right() - 3.5] {
                            p.rect_filled(Rect::from_center_size(pos2(gx, r.center().y), vec2(2.0, 10.0)), 1.0, Color32::from_white_alpha(200));
                        }
                    }
                    let (ps, pe) = preview.get(&it.id).copied().unwrap_or((s, e));
                    let label = if dragging { dates::pretty(&dates::span_value(ps, pe)) } else { it.title.clone() };
                    let txt_col = Color32::WHITE;
                    if r.width() > 34.0 {
                        crate::widgets::text_fit(&p.with_clip_rect(r), pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, &label, bold(12.0), txt_col, r.width() - 20.0);
                    } else if !dragging {
                        crate::widgets::text_fit(&p, pos2(r.right() + 6.0, r.center().y), Align2::LEFT_CENTER, &label, body(12.0), pal.text, 160.0);
                    }
                    if dragging {
                        let g = p.layout_no_wrap(label.clone(), bold(11.5), Color32::WHITE);
                        let tr = Rect::from_min_size(pos2(r.left(), r.top() - 24.0), g.size() + vec2(14.0, 8.0));
                        p.rect_filled(tr, 6.0, Color32::from_black_alpha(210));
                        p.galley(tr.min + vec2(7.0, 4.0), g, Color32::WHITE);
                    }
                    if dep.is_some() && (hover || ctx.data(|d| d.get_temp::<bool>(Id::new(("tl_link_active", it.id)))).unwrap_or(false)) {
                        to_draw_links.push((it.id, r));
                    }
                }
            }
        }

        // dependency arrows
        if let Some(dpid) = dep {
            let arrow = |from: Rect, to: Rect, bad: bool| {
                let col = if bad { pal.danger } else { lerp_color(pal.muted, pal.text, 0.35) };
                let st = Stroke::new(1.6_f32, col);
                let a = pos2(from.right(), from.center().y);
                let b = pos2(to.left(), to.center().y);
                let mut pts = vec![a];
                if b.x - a.x >= 22.0 {
                    pts.push(pos2(a.x + 10.0, a.y));
                    pts.push(pos2(a.x + 10.0, b.y));
                } else {
                    let mid_y = if b.y > a.y { from.bottom() + (ROW_H - BAR_H) / 2.0 } else { from.top() - (ROW_H - BAR_H) / 2.0 };
                    pts.push(pos2(a.x + 8.0, a.y));
                    pts.push(pos2(a.x + 8.0, mid_y));
                    pts.push(pos2(b.x - 12.0, mid_y));
                    pts.push(pos2(b.x - 12.0, b.y));
                }
                pts.push(pos2(b.x - 5.0, b.y));
                p.add(Shape::line(pts, st));
                p.add(Shape::convex_polygon(vec![pos2(b.x, b.y), pos2(b.x - 7.0, b.y - 4.5), pos2(b.x - 7.0, b.y + 4.5)], col, Stroke::NONE));
            };
            for it in &items {
                let Some(row) = ws.page(it.id) else { continue };
                let Some(Val::Multi(preds)) = row.props.get(&dpid) else { continue };
                let Some(to) = bar_rects.get(&it.id) else { continue };
                let my = span_of(it.id, it);
                for pid in preds {
                    let Some(pit) = items.iter().find(|x| x.id == *pid) else { continue };
                    let Some(from) = bar_rects.get(pid) else { continue };
                    let bad = match (span_of(*pid, pit), my) {
                        (Some(ps), Some(ms)) => ms.0 <= ps.1,
                        _ => false,
                    };
                    arrow(*from, *to, bad);
                }
            }
        }

        // connectors (drag to link)
        for (rid, r) in &to_draw_links {
            let c = pos2(r.right() + 9.0, r.center().y);
            let cr = Rect::from_center_size(c, vec2(18.0, 18.0));
            let resp = ui.interact(cr, Id::new(("tl_conn", *rid)), Sense::drag());
            let active = resp.dragged();
            ctx.data_mut(|d| d.insert_temp(Id::new(("tl_link_active", *rid)), active));
            p.circle_filled(c, if resp.hovered() || active { 6.0 } else { 4.5 }, pal.accent);
            p.circle_stroke(c, if resp.hovered() || active { 6.0 } else { 4.5 }, Stroke::new(1.5_f32, pal.bg));
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
            }
            if active {
                if let Some(pos) = ctx.pointer_hover_pos() {
                    p.add(Shape::line(vec![c, pos], Stroke::new(2.0_f32, pal.accent)));
                    for (oid, orect) in &bar_rects {
                        if oid != rid && orect.expand(2.0).contains(pos) {
                            p.rect_stroke(orect.expand(2.0), 8.0, Stroke::new(2.0_f32, pal.accent), egui::StrokeKind::Outside);
                        }
                    }
                }
            }
            if resp.drag_stopped() {
                if let Some(pos) = ctx.pointer_hover_pos() {
                    link_drop = Some((*rid, pos));
                }
            }
        }
    });
    if let Some((from, pos)) = link_drop {
        if let Some((to, _)) = bar_rects.iter().find(|(id, r)| **id != from && r.expand(2.0).contains(pos)) {
            acts.push(A::AddDep(*to, from));
        }
    }
    // unscheduled hint
    let unscheduled = items.iter().filter(|i| i.span.is_none()).count();
    if unscheduled > 0 {
        ui.label(egui::RichText::new(format!("{unscheduled} row(s) have no date — click or drag in their row to schedule them.")).font(body(12.0)).color(pal.muted));
    }
}

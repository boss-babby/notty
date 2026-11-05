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

const HEAD_H: f32 = 46.0;
const ZOOMS: [f32; 5] = [9.0, 16.0, 28.0, 44.0, 66.0];
/// Empty days kept before the first / after the last scheduled day (gives room to drag bars outwards).
const PAD_LEFT: i64 = 30;
const PAD_RIGHT: i64 = 60;
/// Auto-scroll while a bar is dragged near the edge of the grid.
const EDGE: f32 = 48.0;

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
    // Touch screens: taller rows / bars, wide grab zones around the ends of a bar and a big link handle. Every zone is *claimed*
    // (touch.rs) so a finger that lands on it drags immediately instead of being turned into a scroll swipe.
    let tch = crate::touch::touchy();
    let row_h: f32 = if tch { 50.0 } else { 38.0 };
    let bar_h: f32 = if tch { 32.0 } else { 24.0 };
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
        let linking: bool = ctx.data(|d| d.get_temp::<u64>(Id::new(("tl_pick", db_id)))).is_some();
        if linking {
            ui.label(egui::RichText::new("Now tap the task that should come after it").font(body(12.5)).color(pal.accent));
            if pill(ui, pal, "Cancel", false, false).clicked() {
                ctx.data_mut(|d| d.remove::<u64>(Id::new(("tl_pick", db_id))));
            }
        } else if tch {
            ui.label(egui::RichText::new(if dep.is_some() { "Drag a bar to move it · drag its ends to resize · drag ● onto another bar (or tap ●, then tap a bar) to link" } else { "Drag a bar to move it · drag its ends to resize" }).font(body(12.0)).color(pal.muted));
        } else
        {
        ui.label(egui::RichText::new(if dep.is_some() { "Drag bars to move · edges to resize · ● (drag, or click then click a bar) to link" } else { "Drag bars to move · drag edges to resize" }).font(body(12.0)).color(pal.muted));
        }
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

    // ── visible day range ──
    // The range comes from the *committed* spans only and never from a live drag preview. (The old code derived it from the
    // preview: dragging a bar past the end widened the grid, the bar stayed under the pointer, `dx` kept growing, the grid kept
    // widening … an endless feedback loop that ended in millions of painted day columns — the freeze / crash.)
    // The range is also kept monotonic per view so releasing a bar never makes the chart jump.
    let day_floor = dates::days_from_civil(1000, 1, 1);
    let day_ceil = dates::days_from_civil(9000, 12, 31);
    let committed: Vec<(i64, i64)> = items.iter().filter_map(|i| i.span).collect();
    let lo = committed.iter().map(|s| s.0).min().unwrap_or(today).min(today);
    let hi = committed.iter().map(|s| s.1).max().unwrap_or(today).max(today);
    let dom_id = Id::new(("tl_domain", db_id, view.id));
    let off_id = Id::new(("tl_off", db_id, view.id));
    let auto_id = Id::new(("tl_auto", db_id, view.id));
    let mut min_day = (lo - PAD_LEFT).max(day_floor);
    let mut max_day = (hi + PAD_RIGHT).min(day_ceil).max(min_day + 1);
    let mut grew_left = 0_i64;
    if let Some((a, b)) = ctx.data(|d| d.get_temp::<(i64, i64)>(dom_id)) {
        if a < min_day {
            min_day = a;
        } else {
            grew_left = a - min_day;
        }
        max_day = max_day.max(b);
    }
    max_day = max_day.max(min_day + (w / dw) as i64 + 4).min(day_ceil.max(min_day + 1));
    ctx.data_mut(|d| d.insert_temp(dom_id, (min_day, max_day)));
    let (lim_lo, lim_hi) = (min_day, max_day - 1);

    // live drag previews (read before laying out so arrows follow the bars); always clamped into the visible range
    let mut preview: HashMap<u64, (i64, i64)> = HashMap::new();
    for it in &items {
        if let Some((s, e)) = it.span {
            if let Some((mode, dx)) = ctx.data(|d| d.get_temp::<(Mode, f32)>(Id::new(("tl_drag", it.id)))) {
                let dd = if dx.is_finite() { (dx / dw).round() as i64 } else { 0 };
                let (ns, ne) = match mode {
                    Mode::Move => {
                        let dd = dd.max(lim_lo - s).min((lim_hi - e).max(lim_lo - s));
                        (s + dd, e + dd)
                    }
                    Mode::Left => ((s + dd).max(lim_lo).min(e), e),
                    Mode::Right => (s, (e + dd).max(s).min(lim_hi.max(s))),
                };
                preview.insert(it.id, (ns, ne));
            }
        }
    }
    let span_of = |id: u64, it: &Item| -> Option<(i64, i64)> { preview.get(&id).copied().or(it.span) };
    let n_days = (max_day - min_day + 1) as f32;
    let total_w = n_days * dw;
    let label_w = if w < 560.0 { 112.0 } else { 220.0 };
    let height = HEAD_H + items.len() as f32 * row_h;

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
        let r = Rect::from_min_size(pos2(label_rect.left(), label_rect.top() + HEAD_H + i as f32 * row_h), vec2(label_w, row_h));
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
    let last_off: f32 = ctx.data(|d| d.get_temp(off_id)).unwrap_or(0.0);
    let auto: f32 = ctx.data_mut(|d| {
        let v = d.get_temp::<f32>(auto_id).unwrap_or(0.0);
        d.remove::<f32>(auto_id);
        v
    });
    if goto || !inited {
        let off = ((today - min_day) as f32 * dw - (grid_outer.width() * 0.3)).max(0.0);
        sa = sa.horizontal_scroll_offset(off);
        ctx.data_mut(|d| {
            d.insert_temp(inited_id, true);
            d.insert_temp(Id::new(("tl_goto", db_id)), false);
        });
    } else if grew_left != 0 || auto != 0.0 {
        // keep the chart visually still when it grows to the left, and follow a bar dragged to the edge
        sa = sa.horizontal_scroll_offset((last_off + grew_left as f32 * dw + auto).max(0.0));
    }
    let mut bar_rects: HashMap<u64, Rect> = HashMap::new();
    let mut to_draw_links: Vec<(u64, Rect, bool)> = vec![];
    // click-to-link mode: click a ● (source), then click the bar that should depend on it
    let pick_id = Id::new(("tl_pick", db_id));
    if ctx.data(|d| d.get_temp::<u64>(pick_id)).is_some() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        ctx.data_mut(|d| d.remove::<u64>(pick_id));
    }
    let pick: Option<u64> = ctx.data(|d| d.get_temp(pick_id));
    let mut link_drop: Option<(u64, Pos2)> = None;
    let mut ui_child = ui.new_child(egui::UiBuilder::new().max_rect(grid_outer).layout(egui::Layout::top_down(egui::Align::Min)));
    let scroll_out = sa.show(&mut ui_child, |ui| {
        let (content, _) = ui.allocate_exact_size(vec2(total_w, height), Sense::hover());
        let p = ui.painter().clone();
        let x_of = |d: i64| content.left() + (d - min_day) as f32 * dw;
        // only the visible columns are painted (the grid can be thousands of days wide)
        let vis = ui.clip_rect();
        let d0 = (min_day + ((vis.left() - content.left()) / dw).floor() as i64 - 1).max(min_day);
        let d1 = (min_day + ((vis.right() - content.left()) / dw).ceil() as i64 + 1).min(max_day);
        // header + weekend shading + grid lines
        for d in d0..=d1 {
            let x = x_of(d);
            let (y, m, dd) = dates::civil_from_days(d);
            let wd = dates::weekday(y, m, dd);
            if wd >= 5 {
                p.rect_filled(Rect::from_min_size(pos2(x, content.top() + HEAD_H), vec2(dw, height - HEAD_H)), 0.0, with_alpha(pal.hover, 0.45));
            }
            if wd == 0 || dd == 1 {
                p.vline(x, (content.top() + 20.0)..=content.bottom(), Stroke::new(1.0_f32, with_alpha(pal.border, if dd == 1 { 0.9 } else { 0.45 })));
            }
            if dd == 1 || d == d0 {
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
            let y = content.top() + HEAD_H + (i + 1) as f32 * row_h;
            p.hline(content.x_range(), y, Stroke::new(1.0_f32, with_alpha(pal.border, 0.7)));
        }
        // today marker
        let tx = x_of(today) + dw / 2.0;
        p.vline(tx, (content.top() + 20.0)..=content.bottom(), Stroke::new(1.5_f32, pal.accent));
        p.rect_filled(Rect::from_center_size(pos2(tx, content.top() + 34.0), vec2(if dw >= 24.0 { dw - 2.0 } else { 18.0 }, 16.0)), 8.0, pal.accent);
        p.text(pos2(tx, content.top() + 34.0), Align2::CENTER_CENTER, dates::civil_from_days(today).2.to_string(), bold(10.5), Color32::WHITE);

        // rows
        for (i, it) in items.iter().enumerate() {
            let row_top = content.top() + HEAD_H + i as f32 * row_h;
            let row_rect = Rect::from_min_size(pos2(content.left(), row_top), vec2(total_w, row_h));
            match span_of(it.id, it) {
                None => {
                    // unscheduled: click a day to schedule, drag to create a range
                    let resp = ui.interact(row_rect, Id::new(("tl_new", it.id)), Sense::click_and_drag());
                    let day_at = |pos: Pos2| (min_day + ((pos.x - content.left()) / dw).floor() as i64).max(min_day).min(max_day - 1);
                    if resp.hovered() || resp.dragged() {
                        if let Some(pos) = ctx.pointer_hover_pos() {
                            let d = day_at(pos);
                            let r = Rect::from_min_size(pos2(x_of(d), row_top + 6.0), vec2(dw, row_h - 12.0));
                            p.rect_filled(r, 5.0, with_alpha(it.color, 0.25));
                            if !resp.dragged() {
                                p.text(pos2(pos.x + 12.0, row_top + row_h / 2.0), Align2::LEFT_CENTER, "click to schedule", body(11.5), pal.muted);
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
                            p.rect_filled(Rect::from_min_max(pos2(x_of(a), row_top + 7.0), pos2(x_of(b + 1), row_top + row_h - 7.0)), 6.0, with_alpha(it.color, 0.5));
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
                    let r = Rect::from_min_max(pos2(x0 + 1.0, row_top + (row_h - bar_h) / 2.0), pos2((x1 - 1.0).max(x0 + 8.0), row_top + (row_h + bar_h) / 2.0));
                    bar_rects.insert(it.id, r);
                    let dragging = preview.contains_key(&it.id);
                    // hit zones
                    let grip = if tch { (r.width() * 0.28).clamp(9.0, 18.0) } else { (r.width() * 0.3).clamp(3.0, 9.0) };
                    let (ext, vext) = if tch { (14.0, 6.0) } else { (0.0, 0.0) };
                    let left = Rect::from_min_max(pos2(r.left() - ext, r.top() - vext), pos2(r.left() + grip, r.bottom() + vext));
                    let right = Rect::from_min_max(pos2(r.right() - grip, r.top() - vext), pos2(r.right() + if tch { 8.0 } else { 0.0 }, r.bottom() + vext));
                    let mid = Rect::from_min_max(pos2(left.right(), r.top() - vext), pos2(right.left(), r.bottom() + vext));
                    let mut hover = false;
                    for (zone, mode, cursor) in [(mid, Mode::Move, CursorIcon::Grab), (left, Mode::Left, CursorIcon::ResizeHorizontal), (right, Mode::Right, CursorIcon::ResizeHorizontal)] {
                        if zone.width() <= 0.0 {
                            continue;
                        }
                        let id = Id::new(("tl_bar", it.id, mode as u8));
                        let resp = ui.interact(zone, id, Sense::click_and_drag());
                        crate::touch::claim(zone);
                        hover |= resp.hovered() || resp.dragged();
                        if let Some(from) = pick {
                            if from != it.id && resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
                            }
                            if from != it.id && resp.clicked() {
                                acts.push(A::AddDep(it.id, from));
                                ctx.data_mut(|d| d.remove::<u64>(pick_id));
                            }
                        }
                        let key = Id::new(("tl_drag", it.id));
                        if resp.hovered() || resp.dragged() {
                            ui.ctx().set_cursor_icon(if resp.dragged() && mode == Mode::Move { CursorIcon::Grabbing } else { cursor });
                        }
                        if resp.drag_started() {
                            ctx.data_mut(|d| d.insert_temp(key, (mode, 0.0_f32)));
                        }
                        if resp.dragged() {
                            // auto-scroll when the pointer is near either edge of the grid
                            let mut auto_dx = 0.0_f32;
                            if let Some(pos) = ctx.pointer_interact_pos() {
                                let speed = (ctx.input(|i| i.stable_dt).clamp(0.004, 0.05)) * 520.0;
                                if pos.x > grid_outer.right() - EDGE {
                                    auto_dx = speed * ((pos.x - (grid_outer.right() - EDGE)) / EDGE).clamp(0.2, 1.5);
                                } else if pos.x < grid_outer.left() + EDGE {
                                    auto_dx = -speed * (((grid_outer.left() + EDGE) - pos.x) / EDGE).clamp(0.2, 1.5);
                                }
                                if auto_dx < 0.0 && last_off <= 0.5 {
                                    auto_dx = 0.0;
                                }
                            }
                            let (s, e) = it.span.unwrap_or((s, e)); // limits are relative to the committed span
                            let (lo_px, hi_px) = match mode {
                                Mode::Move => ((lim_lo - s) as f32 * dw, (lim_hi - e) as f32 * dw),
                                Mode::Left => ((lim_lo - s) as f32 * dw, (e - s) as f32 * dw),
                                Mode::Right => ((s - e) as f32 * dw, (lim_hi - e) as f32 * dw),
                            };
                            let hi_px = hi_px.max(lo_px);
                            // NOTE: `resp.drag_delta()` reads the egui input state; calling it *inside* `ctx.data_mut` (which holds the
                            // context write-lock) dead-locked the whole app on the first dragged frame — the original freeze.
                            let ddx = resp.drag_delta().x;
                            ctx.data_mut(|d| {
                                if let Some((m, dx)) = d.get_temp::<(Mode, f32)>(key) {
                                    let step = ddx + auto_dx;
                                    let nx = if (dx + step).is_finite() { (dx + step).max(lo_px).min(hi_px) } else { 0.0 };
                                    d.insert_temp(key, (m, nx));
                                }
                                if auto_dx != 0.0 {
                                    d.insert_temp(auto_id, auto_dx);
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
                    if hover || dragging || tch {
                        for gx in [r.left() + 3.5 + if tch { 2.0 } else { 0.0 }, r.right() - 3.5 - if tch { 2.0 } else { 0.0 }] {
                            p.rect_filled(Rect::from_center_size(pos2(gx, r.center().y), vec2(if tch { 3.0 } else { 2.0 }, if tch { 16.0 } else { 10.0 })), 1.5, Color32::from_white_alpha(if tch { 230 } else { 200 }));
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
                    if dep.is_some() {
                        // The connector must stay put while the pointer travels from the bar to it: it is shown for the bar *and*
                        // for the strip right of the bar (where the ● sits), and a faint dot is always visible. (It used to appear
                        // only while the bar itself was hovered, so it vanished the moment the pointer left the bar to reach it.)
                        let near = ctx.pointer_hover_pos().map_or(false, |pp| Rect::from_min_max(pos2(r.left(), r.top() - 3.0), pos2(r.right() + 46.0, r.bottom() + 3.0)).contains(pp));
                        let active = ctx.data(|d| d.get_temp::<bool>(Id::new(("tl_link_active", it.id)))).unwrap_or(false) || pick == Some(it.id);
                        to_draw_links.push((it.id, r, hover || near || active));
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
                    let mid_y = if b.y > a.y { from.bottom() + (row_h - bar_h) / 2.0 } else { from.top() - (row_h - bar_h) / 2.0 };
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
        for (rid, r, hot) in &to_draw_links {
            let c = pos2(r.right() + if tch { 26.0 } else { 11.0 }, r.center().y);
            let cr = if tch { Rect::from_center_size(c, vec2(40.0, row_h)) } else { Rect::from_center_size(c, vec2(22.0, 26.0)) };
            let resp = ui.interact(cr, Id::new(("tl_conn", *rid)), Sense::click_and_drag());
            crate::touch::claim(cr);
            let active = resp.dragged();
            ctx.data_mut(|d| d.insert_temp(Id::new(("tl_link_active", *rid)), active));
            let picked = pick == Some(*rid);
            if resp.clicked() {
                ctx.data_mut(|d| if picked { d.remove::<u64>(pick_id) } else { d.insert_temp(pick_id, *rid) });
            }
            let big = resp.hovered() || active || picked;
            let col = if *hot || big { pal.accent } else { with_alpha(pal.accent, 0.38) };
            let rad = if tch { if big { 10.0 } else { 8.0 } } else if big { 6.5 } else if *hot { 5.0 } else { 3.5 };
            if picked {
                p.circle_stroke(c, rad + 3.5, Stroke::new(2.0_f32, with_alpha(pal.accent, 0.5)));
            }
            p.circle_filled(c, rad, col);
            p.circle_stroke(c, rad, Stroke::new(1.5_f32, pal.bg));
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
                resp.clone().on_hover_text("Drag onto another bar — or click, then click the bar that depends on this one");
            }
            if picked {
                if let Some(pos) = ctx.pointer_hover_pos() {
                    p.add(Shape::line(vec![c, pos], Stroke::new(1.5_f32, with_alpha(pal.accent, 0.8))));
                    for (oid, orect) in &bar_rects {
                        if oid != rid && orect.expand(2.0).contains(pos) {
                            p.rect_stroke(orect.expand(2.0), 8.0, Stroke::new(2.0_f32, pal.accent), egui::StrokeKind::Outside);
                        }
                    }
                }
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
                // a finger that lifts also takes the pointer with it, so fall back to the last known position
                if let Some(pos) = ctx.pointer_hover_pos().or(resp.interact_pointer_pos()).or(ctx.input(|i| i.pointer.latest_pos())) {
                    link_drop = Some((*rid, pos));
                }
            }
        }
    });
    ctx.data_mut(|d| d.insert_temp(off_id, scroll_out.state.offset.x));
    #[cfg(test)]
    ctx.data_mut(|d| d.insert_temp(Id::new(("tl_bars_test", db_id)), bar_rects.clone()));
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

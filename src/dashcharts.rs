//! Chart painters for dashboard widgets (egui painter only — identical on Windows and in the browser).
//!
//! Every chart is interactive: hovering highlights the element under the pointer and shows a tooltip, clicking returns the
//! element (`(series, category)`, a record id or a day) so the dashboard can open a drill-down list, and the legend toggles
//! series on and off.  Charts animate in once when they appear.

use std::collections::BTreeMap;

use egui::{pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, Id, LayerId, Mesh, Order, Painter, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};

use crate::dashdata::{fmt_agg, fmt_num, Dataset, Scatter};
use crate::dates;
use crate::theme::{body, bold, Palette};
use crate::widgets::{appear, text_fit, with_alpha};

fn c(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub const PALETTE_NAMES: [&str; 4] = ["Vivid", "Ocean", "Sunset", "Forest"];

pub fn series_color(palette: u8, i: usize) -> Color32 {
    const P: [[(u8, u8, u8); 10]; 4] = [
        [(35, 131, 226), (46, 160, 110), (217, 115, 13), (144, 101, 176), (212, 76, 71), (203, 145, 47), (193, 76, 138), (51, 170, 190), (120, 130, 150), (159, 107, 83)],
        [(35, 131, 226), (24, 170, 200), (70, 200, 170), (90, 110, 220), (130, 170, 240), (40, 100, 160), (100, 200, 220), (60, 140, 140), (150, 190, 200), (30, 70, 130)],
        [(232, 93, 76), (245, 150, 60), (250, 200, 70), (200, 80, 130), (150, 70, 160), (240, 120, 120), (190, 120, 60), (120, 60, 100), (250, 170, 130), (170, 60, 60)],
        [(46, 160, 110), (90, 180, 120), (140, 200, 100), (40, 120, 90), (180, 210, 120), (70, 150, 150), (110, 140, 80), (30, 90, 70), (160, 190, 160), (60, 100, 60)],
    ];
    let (r, g, b) = P[palette as usize % 4][i % 10];
    c(r, g, b)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Bar,
    HBar,
    Stacked,
    Line,
    Area,
    Pie,
    Donut,
    Funnel,
    Radar,
}

pub struct Style {
    pub kind: Kind,
    pub labels: bool,
    pub legend: bool,
    pub palette: u8,
}

// ───────────────────────────── helpers ─────────────────────────────

fn tw(p: &Painter, s: &str, size: f32) -> f32 {
    p.layout_no_wrap(s.to_owned(), body(size), Color32::WHITE).size().x
}

/// (top, step) of a "nice" axis that covers `max`, with at most ~5 ticks.
pub fn nice_scale(max: f64) -> (f64, f64) {
    if !(max > 0.0) || !max.is_finite() {
        return (1.0, 0.25);
    }
    let raw = max / 4.0;
    let e = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0].iter().map(|m| m * e).find(|s| raw <= *s).unwrap_or(10.0 * e);
    ((max / step).ceil() * step, step)
}

fn tip(ctx: &egui::Context, pal: &Palette, at: Pos2, title: &str, rows: &[(Color32, String, String)]) {
    let p = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("dash_tip")));
    let tf = bold(12.5);
    let rf = body(12.0);
    let title_g = if title.is_empty() { None } else { Some(p.layout_no_wrap(title.to_owned(), tf.clone(), pal.text)) };
    let mut w: f32 = title_g.as_ref().map_or(0.0, |g| g.size().x);
    let mut rg = vec![];
    for (col, name, val) in rows {
        let a = p.layout_no_wrap(name.clone(), rf.clone(), pal.muted);
        let b = p.layout_no_wrap(val.clone(), bold(12.0), pal.text);
        w = w.max(a.size().x + b.size().x + 34.0);
        rg.push((*col, a, b));
    }
    let h = title_g.as_ref().map_or(0.0, |g| g.size().y + 4.0) + rg.len() as f32 * 17.0 + 14.0;
    let screen = ctx.screen_rect();
    let size = vec2(w + 20.0, h);
    let mut pos = at + vec2(14.0, 14.0);
    if pos.x + size.x > screen.right() - 6.0 {
        pos.x = at.x - size.x - 14.0;
    }
    if pos.y + size.y > screen.bottom() - 6.0 {
        pos.y = at.y - size.y - 10.0;
    }
    let pos = pos2(pos.x.max(6.0), pos.y.max(6.0));
    let r = Rect::from_min_size(pos, size);
    p.rect_filled(r.translate(vec2(0.0, 3.0)).expand(1.0), 9.0, Color32::from_black_alpha(34));
    p.rect_filled(r, 9.0, pal.popup);
    p.rect_stroke(r, 9.0, Stroke::new(1.0_f32, pal.border), egui::StrokeKind::Inside);
    let mut y = r.top() + 8.0;
    if let Some(g) = title_g {
        let hh = g.size().y;
        p.galley(pos2(r.left() + 10.0, y), g, pal.text);
        y += hh + 4.0;
    }
    for (col, a, b) in rg {
        p.circle_filled(pos2(r.left() + 15.0, y + 8.0), 4.0, col);
        p.galley(pos2(r.left() + 25.0, y + 1.0), a, pal.muted);
        let bw = b.size().x;
        p.galley(pos2(r.right() - 10.0 - bw, y + 1.0), b, pal.text);
        y += 17.0;
    }
}

fn hand(ui: &Ui, on: bool) {
    if on {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
}

/// Legend (wrapping rows). Returns the height used. Toggles `hidden[i]` on click.
fn legend(ui: &mut Ui, pal: &Palette, id: Id, rect: Rect, entries: &[(String, Color32)], hidden: &mut [bool]) -> f32 {
    let p = ui.painter_at(rect);
    let (mut x, mut y) = (rect.left(), rect.top());
    let rowh = 20.0;
    // measure first so that the legend can be bottom-aligned
    let widths: Vec<f32> = entries.iter().map(|(n, _)| (tw(&p, n, 11.5) + 26.0).min(rect.width())).collect();
    let mut rows = 1;
    let mut cx = 0.0;
    for w in &widths {
        if cx + w > rect.width() && cx > 0.0 {
            rows += 1;
            cx = 0.0;
        }
        cx += w;
    }
    let total = rows as f32 * rowh;
    y = y.max(rect.bottom() - total);
    for (i, (name, col)) in entries.iter().enumerate() {
        let w = widths[i];
        if x + w > rect.right() + 0.5 && x > rect.left() {
            x = rect.left();
            y += rowh;
        }
        let r = Rect::from_min_size(pos2(x, y), vec2(w, rowh));
        let resp = ui.interact(r, id.with(("leg", i)), Sense::click());
        hand(ui, resp.hovered());
        if resp.hovered() {
            p.rect_filled(r, 6.0, pal.hover);
        }
        let off = hidden[i];
        p.circle_filled(pos2(r.left() + 9.0, r.center().y), 4.5, if off { with_alpha(pal.muted, 0.5) } else { *col });
        text_fit(&p, pos2(r.left() + 19.0, r.center().y), Align2::LEFT_CENTER, name, body(11.5), if off { with_alpha(pal.muted, 0.7) } else { pal.muted }, w - 22.0);
        if resp.clicked() {
            hidden[i] = !hidden[i];
        }
        x += w;
    }
    total + 4.0
}

fn mesh_quad(m: &mut Mesh, a: Pos2, b: Pos2, c_: Pos2, d: Pos2, col: Color32) {
    let i = m.vertices.len() as u32;
    for q in [a, b, c_, d] {
        m.colored_vertex(q, col);
    }
    m.add_triangle(i, i + 1, i + 2);
    m.add_triangle(i, i + 2, i + 3);
}

/// Filled ring sector between radii `r0` and `r1` (r0 = 0 gives a pie slice).
pub fn sector(p: &Painter, center: Pos2, r0: f32, r1: f32, a0: f32, a1: f32, col: Color32) {
    let steps = (((a1 - a0).abs() * r1) / 6.0).ceil().clamp(2.0, 120.0) as usize;
    let mut m = Mesh::default();
    let pt = |r: f32, a: f32| center + vec2(a.cos(), a.sin()) * r;
    if r0 <= 0.5 {
        m.colored_vertex(center, col);
        for i in 0..=steps {
            let a = a0 + (a1 - a0) * i as f32 / steps as f32;
            m.colored_vertex(pt(r1, a), col);
        }
        for i in 0..steps as u32 {
            m.add_triangle(0, i + 1, i + 2);
        }
    } else {
        for i in 0..=steps {
            let a = a0 + (a1 - a0) * i as f32 / steps as f32;
            m.colored_vertex(pt(r0, a), col);
            m.colored_vertex(pt(r1, a), col);
        }
        for i in 0..steps as u32 {
            let k = i * 2;
            m.add_triangle(k, k + 1, k + 2);
            m.add_triangle(k + 1, k + 3, k + 2);
        }
    }
    p.add(Shape::mesh(m));
}

fn fmt_v(v: f64, ds: &Dataset) -> String {
    fmt_agg(v, ds.percent)
}

// ───────────────────────────── main entry ─────────────────────────────

/// Draws a categorical chart. Returns the clicked `(series, category)`.
pub fn chart(ui: &mut Ui, pal: &Palette, id: Id, size: Vec2, ds: &Dataset, st: &Style) -> Option<(usize, usize)> {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let ctx = ui.ctx().clone();
    let p = ui.painter_at(rect);
    let t = appear(&ctx, id.with("in"), 0.6);
    let hid_id = id.with("hidden");
    let mut hidden: Vec<bool> = ctx.data(|d| d.get_temp(hid_id)).unwrap_or_default();

    let by_cat = matches!(st.kind, Kind::Pie | Kind::Donut | Kind::Funnel);
    let single = ds.series.len() == 1;
    let percat = single && !ds.dated && !matches!(st.kind, Kind::Line | Kind::Area | Kind::Radar);
    let cat_col = |i: usize| ds.colors.get(i).copied().flatten().unwrap_or_else(|| series_color(st.palette, i));
    let ser_col = |i: usize| ds.series[i].color.unwrap_or_else(|| series_color(st.palette, i));
    let entries: Vec<(String, Color32)> = if by_cat {
        ds.cats.iter().enumerate().map(|(i, n)| (n.clone(), cat_col(i))).collect()
    } else if !single {
        ds.series.iter().enumerate().map(|(i, s)| (s.name.clone(), ser_col(i))).collect()
    } else {
        vec![]
    };
    if hidden.len() != entries.len() {
        hidden = vec![false; entries.len()];
    }
    let mut plot = rect;
    if st.legend && !entries.is_empty() && entries.len() <= 24 {
        let max_h = (rect.height() * 0.4).max(24.0);
        let lr = Rect::from_min_max(pos2(rect.left(), (rect.bottom() - max_h).max(rect.top())), rect.max);
        let used = legend(ui, pal, id, lr, &entries, &mut hidden).min(max_h);
        plot.max.y -= used + 4.0;
    }
    ctx.data_mut(|d| d.insert_temp(hid_id, hidden.clone()));
    let show_s = |i: usize| !(!by_cat && !single && hidden.get(i).copied().unwrap_or(false));
    let show_c = |i: usize| !(by_cat && hidden.get(i).copied().unwrap_or(false));

    let hp = resp.hover_pos();
    let cp = if resp.clicked() { resp.interact_pointer_pos().or(hp) } else { None };
    let mut clicked: Option<(usize, usize)> = None;
    if plot.width() < 40.0 || plot.height() < 30.0 {
        return None;
    }
    let n = ds.cats.len();
    if n == 0 {
        return None;
    }

    match st.kind {
        // ───────────── vertical bars (grouped or stacked) ─────────────
        Kind::Bar | Kind::Stacked => {
            let stacked = st.kind == Kind::Stacked && !single;
            let vis: Vec<usize> = (0..ds.series.len()).filter(|i| show_s(*i)).collect();
            let top_of = |ci: usize| -> f64 {
                if stacked {
                    vis.iter().map(|s| ds.series[*s].vals[ci].max(0.0)).sum()
                } else {
                    vis.iter().map(|s| ds.series[*s].vals[ci]).fold(0.0, f64::max)
                }
            };
            let max = (0..n).map(top_of).fold(0.0_f64, f64::max);
            let min = vis.iter().flat_map(|s| ds.series[*s].vals.iter().copied()).fold(0.0_f64, f64::min);
            let (top, step) = nice_scale(max);
            let lo = if min < 0.0 { -((-min / step).ceil() * step) } else { 0.0 };
            let axis_w = tw(&p, &fmt_v(top, ds), 11.0).max(tw(&p, &fmt_v(lo, ds), 11.0)) + 10.0;
            let area = Rect::from_min_max(pos2(plot.left() + axis_w, plot.top() + if st.labels { 14.0 } else { 6.0 }), pos2(plot.right() - 4.0, plot.bottom() - 20.0));
            let y_of = |v: f64| area.bottom() - ((v - lo) / (top - lo)) as f32 * area.height();
            let mut v = lo;
            while v <= top + step * 0.01 {
                let y = y_of(v);
                p.hline(area.x_range(), y, Stroke::new(1.0_f32, with_alpha(pal.border, if v == 0.0 { 1.0 } else { 0.8 })));
                p.text(pos2(area.left() - 6.0, y), Align2::RIGHT_CENTER, fmt_v(v, ds), body(11.0), pal.muted);
                v += step;
            }
            let slot = area.width() / n as f32;
            let m = if stacked { 1 } else { vis.len().max(1) };
            let bw = (slot * 0.74 / m as f32).clamp(3.0, 56.0);
            let gw = bw * m as f32;
            let base = y_of(0.0);
            let slot_at = |x: f32| (((x - area.left()) / slot).floor() as isize).clamp(0, n as isize - 1) as usize;
            // geometry of one bar (series index in `vis` order)
            let bar_rect = |ci: usize, k: usize, s: usize| -> Rect {
                let x0 = area.left() + slot * ci as f32 + (slot - gw) / 2.0;
                if stacked {
                    let below: f64 = vis.iter().take_while(|x| **x != s).map(|x| ds.series[*x].vals[ci].max(0.0)).sum();
                    let v = ds.series[s].vals[ci].max(0.0);
                    Rect::from_min_max(pos2(x0, y_of(below + v)), pos2(x0 + bw, y_of(below)))
                } else {
                    let v = ds.series[s].vals[ci];
                    let (a, b) = (y_of(v.max(0.0)), y_of(v.min(0.0)));
                    Rect::from_min_max(pos2(x0 + bw * k as f32, a), pos2(x0 + bw * (k + 1) as f32, b.max(a)))
                }
            };
            let hit = |pos: Pos2| -> Option<(usize, usize)> {
                if !plot.contains(pos) || pos.x < area.left() || pos.y > area.bottom() + 16.0 {
                    return None;
                }
                let ci = slot_at(pos.x);
                if stacked {
                    let found = vis.iter().rev().find(|s| bar_rect(ci, 0, **s).expand2(vec2(4.0, 0.0)).contains(pos));
                    return Some((found.copied().or(vis.last().copied()).unwrap_or(0), ci));
                }
                let x0 = area.left() + slot * ci as f32 + (slot - gw) / 2.0;
                let k = (((pos.x - x0) / bw).floor() as isize).clamp(0, m as isize - 1) as usize;
                vis.get(k).map(|s| (*s, ci))
            };
            let hv = hp.and_then(hit);
            if let Some((_, hc)) = hv {
                let r = Rect::from_min_max(pos2(area.left() + slot * hc as f32, area.top()), pos2(area.left() + slot * (hc + 1) as f32, area.bottom()));
                p.rect_filled(r, 6.0, with_alpha(pal.muted, 0.09));
            }
            // x labels (every k-th when crowded)
            let longest = ds.cats.iter().map(|s| s.chars().count()).max().unwrap_or(1) as f32;
            let stride = ((longest.min(12.0) * 6.4 + 8.0) / slot).ceil().max(1.0) as usize;
            for (ci, name) in ds.cats.iter().enumerate() {
                if ci % stride == 0 {
                    text_fit(&p, pos2(area.left() + slot * (ci as f32 + 0.5), area.bottom() + 11.0), Align2::CENTER_CENTER, name, body(11.5), if hv.map_or(false, |h| h.1 == ci) { pal.text } else { pal.muted }, slot * stride as f32 - 4.0);
                }
            }
            for ci in 0..n {
                for (k, s) in vis.iter().enumerate() {
                    let r = bar_rect(ci, k, *s);
                    let col = if percat { cat_col(ci) } else if ds.series.len() == 1 { series_color(st.palette, 0) } else { ser_col(*s) };
                    let col = if hv == Some((*s, ci)) { col.gamma_multiply(1.0).linear_multiply(1.0) } else { col.gamma_multiply(0.88) };
                    let grow = Rect::from_min_max(pos2(r.left(), base - (base - r.top()) * t), pos2(r.right(), base - (base - r.bottom()) * t));
                    let hh = grow.height().abs();
                    if hh < 0.5 {
                        continue;
                    }
                    let rad = if stacked { 0 } else { (bw / 3.0).clamp(1.0, 5.0) as u8 };
                    p.rect_filled(grow, CornerRadius { nw: rad, ne: rad, sw: 0, se: 0 }, col);
                    if hv == Some((*s, ci)) {
                        p.rect_stroke(grow, CornerRadius { nw: rad, ne: rad, sw: 0, se: 0 }, Stroke::new(1.5_f32, with_alpha(pal.text, 0.35)), egui::StrokeKind::Outside);
                    }
                    if st.labels && !stacked && bw >= 20.0 && t > 0.95 {
                        let val = ds.series[*s].vals[ci];
                        if val != 0.0 {
                            p.text(pos2(r.center().x, r.top() - 8.0), Align2::CENTER_CENTER, fmt_v(val, ds), bold(11.0), pal.text);
                        }
                    }
                }
                if stacked && st.labels && t > 0.95 && slot > 34.0 {
                    let tot = top_of(ci);
                    if tot > 0.0 {
                        p.text(pos2(area.left() + slot * (ci as f32 + 0.5), y_of(tot) - 8.0), Align2::CENTER_CENTER, fmt_v(tot, ds), bold(11.0), pal.text);
                    }
                }
            }
            if let Some((s, ci)) = hv {
                hand(ui, true);
                let mut rows: Vec<(Color32, String, String)> = vec![];
                for (k, sx) in vis.iter().enumerate() {
                    let _ = k;
                    let col = if percat { cat_col(ci) } else { ser_col(*sx) };
                    rows.push((col, ds.series[*sx].name.clone(), fmt_v(ds.series[*sx].vals[ci], ds)));
                }
                if !stacked && vis.len() > 1 {
                    rows.retain(|r| r.1 == ds.series[s].name);
                }
                tip(&ctx, pal, hp.unwrap_or(rect.center()), &ds.cats[ci], &rows);
            }
            clicked = cp.and_then(hit);
        }

        // ───────────── horizontal bars ─────────────
        Kind::HBar => {
            let vis: Vec<usize> = (0..ds.series.len()).filter(|i| show_s(*i)).collect();
            let max = vis.iter().flat_map(|s| ds.series[*s].vals.iter().copied()).fold(0.0_f64, f64::max).max(1e-9);
            let longest = ds.cats.iter().map(|s| tw(&p, s, 12.5)).fold(0.0_f32, f32::max);
            let label_w = (longest + 10.0).clamp(54.0, plot.width() * 0.42);
            let val_w = vis.iter().flat_map(|s| ds.series[*s].vals.iter().copied()).map(|v| tw(&p, &fmt_v(v, ds), 11.5)).fold(0.0_f32, f32::max) + 8.0;
            let area = Rect::from_min_max(pos2(plot.left() + label_w, plot.top() + 2.0), pos2(plot.right() - if st.labels { val_w } else { 4.0 }, plot.bottom() - 2.0));
            let rowh = (area.height() / n as f32).clamp(14.0, 40.0).min(area.height() / n as f32);
            let m = vis.len().max(1);
            let bh = ((rowh * 0.7) / m as f32).clamp(3.0, 22.0);
            let row_at = |y: f32| (((y - area.top()) / rowh).floor() as isize).clamp(0, n as isize - 1) as usize;
            let hit = |pos: Pos2| -> Option<(usize, usize)> {
                if pos.x < plot.left() || pos.x > plot.right() || pos.y < area.top() || pos.y > area.top() + rowh * n as f32 {
                    return None;
                }
                let ci = row_at(pos.y);
                let y0 = area.top() + rowh * ci as f32 + (rowh - bh * m as f32) / 2.0;
                let k = (((pos.y - y0) / bh).floor() as isize).clamp(0, m as isize - 1) as usize;
                vis.get(k).map(|s| (*s, ci))
            };
            let hv = hp.and_then(hit);
            for ci in 0..n {
                let ry = area.top() + rowh * ci as f32;
                if hv.map_or(false, |h| h.1 == ci) {
                    p.rect_filled(Rect::from_min_size(pos2(plot.left(), ry), vec2(plot.width(), rowh)), 6.0, with_alpha(pal.muted, 0.09));
                }
                text_fit(&p, pos2(plot.left() + 4.0, ry + rowh / 2.0), Align2::LEFT_CENTER, &ds.cats[ci], body(12.5), pal.text, label_w - 10.0);
                for (k, s) in vis.iter().enumerate() {
                    let v = ds.series[*s].vals[ci];
                    let y0 = ry + (rowh - bh * m as f32) / 2.0 + bh * k as f32;
                    let track = Rect::from_min_size(pos2(area.left(), y0 + 1.0), vec2(area.width(), bh - 2.0));
                    p.rect_filled(track, 4.0, with_alpha(pal.muted, 0.1));
                    let w = (area.width() * (v / max).max(0.0) as f32 * t).max(if v > 0.0 { 3.0 } else { 0.0 });
                    let col = if percat { cat_col(ci) } else { ser_col(*s) };
                    if w > 0.0 {
                        p.rect_filled(Rect::from_min_size(track.min, vec2(w, track.height())), 4.0, if hv == Some((*s, ci)) { col } else { col.gamma_multiply(0.88) });
                    }
                    if st.labels && k == m - 1 {
                        p.text(pos2(plot.right() - 2.0, ry + rowh / 2.0), Align2::RIGHT_CENTER, fmt_v(v, ds), bold(11.5), pal.text);
                    }
                }
            }
            if let Some((s, ci)) = hv {
                hand(ui, true);
                tip(&ctx, pal, hp.unwrap_or(rect.center()), &ds.cats[ci], &[(if percat { cat_col(ci) } else { ser_col(s) }, ds.series[s].name.clone(), fmt_v(ds.series[s].vals[ci], ds))]);
            }
            clicked = cp.and_then(hit);
        }

        // ───────────── line / area ─────────────
        Kind::Line | Kind::Area => {
            let vis: Vec<usize> = (0..ds.series.len()).filter(|i| show_s(*i)).collect();
            let max = vis.iter().flat_map(|s| ds.series[*s].vals.iter().copied()).fold(0.0_f64, f64::max);
            let min = vis.iter().flat_map(|s| ds.series[*s].vals.iter().copied()).fold(0.0_f64, f64::min);
            let (top, step) = nice_scale(max);
            let lo = if min < 0.0 { -((-min / step).ceil() * step) } else { 0.0 };
            let axis_w = tw(&p, &fmt_v(top, ds), 11.0).max(tw(&p, &fmt_v(lo, ds), 11.0)) + 10.0;
            let area = Rect::from_min_max(pos2(plot.left() + axis_w + 6.0, plot.top() + 8.0), pos2(plot.right() - 10.0, plot.bottom() - 20.0));
            let y_of = |v: f64| area.bottom() - ((v - lo) / (top - lo)) as f32 * area.height();
            let mut v = lo;
            while v <= top + step * 0.01 {
                let y = y_of(v);
                p.hline(area.x_range(), y, Stroke::new(1.0_f32, with_alpha(pal.border, 0.9)));
                p.text(pos2(plot.left() + axis_w - 4.0, y), Align2::RIGHT_CENTER, fmt_v(v, ds), body(11.0), pal.muted);
                v += step;
            }
            let x_of = |i: usize| if n == 1 { area.center().x } else { area.left() + area.width() * i as f32 / (n - 1) as f32 };
            let nearest = |x: f32| -> usize {
                if n == 1 {
                    0
                } else {
                    (((x - area.left()) / area.width() * (n - 1) as f32).round() as isize).clamp(0, n as isize - 1) as usize
                }
            };
            let longest = ds.cats.iter().map(|s| s.chars().count()).max().unwrap_or(1) as f32;
            let slot = if n > 1 { area.width() / (n - 1) as f32 } else { area.width() };
            let stride = ((longest.min(12.0) * 6.4 + 10.0) / slot).ceil().max(1.0) as usize;
            let hv_c = hp.filter(|p| plot.contains(*p) && p.x >= area.left() - 8.0).map(|p| nearest(p.x));
            for (ci, name) in ds.cats.iter().enumerate() {
                if ci % stride == 0 {
                    text_fit(&p, pos2(x_of(ci), area.bottom() + 12.0), Align2::CENTER_CENTER, name, body(11.5), if hv_c == Some(ci) { pal.text } else { pal.muted }, slot * stride as f32 - 2.0);
                }
            }
            if let Some(ci) = hv_c {
                p.vline(x_of(ci), area.y_range(), Stroke::new(1.0_f32, with_alpha(pal.muted, 0.5)));
            }
            let base = y_of(0.0);
            let reveal = |y: f32| base + (y - base) * t;
            for s in &vis {
                let col = if ds.series.len() == 1 { series_color(st.palette, 0) } else { ser_col(*s) };
                let pts: Vec<Pos2> = (0..n).map(|i| pos2(x_of(i), reveal(y_of(ds.series[*s].vals[i])))).collect();
                if st.kind == Kind::Area && n > 1 {
                    let mut m = Mesh::default();
                    for w in pts.windows(2) {
                        mesh_quad(&mut m, w[0], w[1], pos2(w[1].x, base), pos2(w[0].x, base), with_alpha(col, if vis.len() > 1 { 0.16 } else { 0.22 }));
                    }
                    p.add(Shape::mesh(m));
                }
                if n > 1 {
                    p.add(Shape::line(pts.clone(), Stroke::new(2.2_f32, col)));
                }
                if n <= 30 || hv_c.is_some() {
                    for (i, q) in pts.iter().enumerate() {
                        let hot = hv_c == Some(i);
                        if n <= 30 || hot {
                            p.circle_filled(*q, if hot { 5.5 } else { 3.5 }, pal.bg);
                            p.circle_stroke(*q, if hot { 5.5 } else { 3.5 }, Stroke::new(2.0_f32, col));
                        }
                    }
                }
                if st.labels && n <= 12 && t > 0.95 {
                    for (i, q) in pts.iter().enumerate() {
                        if vis.len() == 1 {
                            p.text(*q - vec2(0.0, 12.0), Align2::CENTER_CENTER, fmt_v(ds.series[*s].vals[i], ds), body(10.5), pal.muted);
                        }
                    }
                }
            }
            if let Some(ci) = hv_c {
                hand(ui, true);
                let rows: Vec<(Color32, String, String)> = vis.iter().map(|s| (if ds.series.len() == 1 { series_color(st.palette, 0) } else { ser_col(*s) }, ds.series[*s].name.clone(), fmt_v(ds.series[*s].vals[ci], ds))).collect();
                tip(&ctx, pal, hp.unwrap_or(rect.center()), &ds.cats[ci], &rows);
            }
            if let (Some(pos), Some(ci)) = (cp, cp.filter(|p| plot.contains(*p)).map(|p| nearest(p.x))) {
                let best = vis.iter().min_by(|a, b| {
                    let da = (y_of(ds.series[**a].vals[ci]) - pos.y).abs();
                    let db = (y_of(ds.series[**b].vals[ci]) - pos.y).abs();
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                });
                clicked = best.map(|s| (*s, ci));
            }
        }

        // ───────────── pie / donut ─────────────
        Kind::Pie | Kind::Donut => {
            let vals: Vec<f64> = (0..n).map(|i| if show_c(i) { ds.cat_total(i).max(0.0) } else { 0.0 }).collect();
            let total: f64 = vals.iter().sum();
            let r = (plot.width().min(plot.height()) / 2.0 - 8.0).max(16.0);
            let center = plot.center();
            let hole = if st.kind == Kind::Donut { r * 0.6 } else { 0.0 };
            if total <= 0.0 {
                p.circle_stroke(center, r, Stroke::new(1.5_f32, pal.border));
                p.text(center, Align2::CENTER_CENTER, "No data", body(13.0), pal.muted);
            } else {
                // angles
                let mut a = -std::f32::consts::FRAC_PI_2;
                let mut spans = vec![];
                for v in &vals {
                    let sweep = (*v / total) as f32 * std::f32::consts::TAU;
                    spans.push((a, a + sweep));
                    a += sweep;
                }
                let hit = |pos: Pos2| -> Option<usize> {
                    let d = pos - center;
                    let dist = d.length();
                    if dist > r + 6.0 || dist < hole {
                        return None;
                    }
                    let mut ang = d.y.atan2(d.x);
                    if ang < -std::f32::consts::FRAC_PI_2 {
                        ang += std::f32::consts::TAU;
                    }
                    spans.iter().position(|(s, e)| ang >= *s && ang < *e && e > s)
                };
                let hv = hp.and_then(hit);
                for (i, (s, e)) in spans.iter().enumerate() {
                    if e - s <= 1e-4 {
                        continue;
                    }
                    let e_anim = s + (e - s) * t;
                    let mid = (s + e) / 2.0;
                    let off = if hv == Some(i) { 6.0 } else { 0.0 };
                    let cc = center + vec2(mid.cos(), mid.sin()) * off;
                    let gap = if spans.iter().filter(|(a, b)| b - a > 1e-4).count() > 1 { 0.012 } else { 0.0 };
                    sector(&p, cc, hole, r, s + gap, (e_anim - gap).max(s + gap + 0.001), cat_col(i));
                    let frac = vals[i] / total;
                    if st.labels && frac >= 0.07 && t > 0.95 {
                        let lr = if hole > 0.0 { (hole + r) / 2.0 } else { r * 0.62 };
                        p.text(cc + vec2(mid.cos(), mid.sin()) * lr, Align2::CENTER_CENTER, format!("{:.0}%", frac * 100.0), bold(11.5), Color32::WHITE);
                    }
                }
                if st.kind == Kind::Donut {
                    let (big, small) = match hv {
                        Some(i) => (fmt_v(ds.cat_total(i), ds), ds.cats[i].clone()),
                        None => (fmt_v(total, ds), "Total".to_owned()),
                    };
                    p.text(center - vec2(0.0, 7.0), Align2::CENTER_CENTER, big, bold((hole * 0.42).clamp(14.0, 26.0)), pal.text);
                    text_fit(&p, center + vec2(0.0, (hole * 0.3).clamp(10.0, 18.0)), Align2::CENTER_CENTER, &small, body(11.5), pal.muted, hole * 1.6);
                }
                if let Some(i) = hv {
                    hand(ui, true);
                    tip(&ctx, pal, hp.unwrap_or(center), &ds.cats[i], &[(cat_col(i), format!("{:.1}%", vals[i] / total * 100.0), fmt_v(ds.cat_total(i), ds))]);
                }
                clicked = cp.and_then(hit).map(|i| (0, i));
            }
        }

        // ───────────── funnel ─────────────
        Kind::Funnel => {
            let idx: Vec<usize> = (0..n).filter(|i| show_c(*i)).collect();
            let vals: Vec<f64> = idx.iter().map(|i| ds.cat_total(*i).max(0.0)).collect();
            let max = vals.iter().copied().fold(0.0_f64, f64::max).max(1e-9);
            let first = vals.first().copied().unwrap_or(1.0).max(1e-9);
            let rowh = (plot.height() / idx.len().max(1) as f32).clamp(18.0, 44.0);
            let gap = 3.0;
            let hit = |pos: Pos2| -> Option<usize> {
                if !plot.contains(pos) {
                    return None;
                }
                let k = ((pos.y - plot.top()) / rowh).floor() as isize;
                (k >= 0 && (k as usize) < idx.len()).then(|| idx[k as usize])
            };
            let hv = hp.and_then(hit);
            for (k, i) in idx.iter().enumerate() {
                let w = (plot.width() * (vals[k] / max) as f32 * t).max(8.0);
                let r = Rect::from_center_size(pos2(plot.center().x, plot.top() + rowh * k as f32 + rowh / 2.0), vec2(w, rowh - gap));
                let col = cat_col(*i);
                p.rect_filled(r, 5.0, if hv == Some(*i) { col } else { col.gamma_multiply(0.9) });
                let label = format!("{}  ·  {}  ({:.0}%)", ds.cats[*i], fmt_v(vals[k], ds), vals[k] / first * 100.0);
                let col_t = Color32::WHITE;
                if tw(&p, &label, 12.0) + 14.0 <= w {
                    p.text(r.center(), Align2::CENTER_CENTER, label, bold(12.0), col_t);
                } else {
                    text_fit(&p, pos2(plot.center().x, r.center().y), Align2::CENTER_CENTER, &format!("{}  {}", ds.cats[*i], fmt_v(vals[k], ds)), bold(11.5), pal.text, plot.width() - 8.0);
                }
            }
            if let Some(i) = hv {
                hand(ui, true);
                tip(&ctx, pal, hp.unwrap_or(rect.center()), &ds.cats[i], &[(cat_col(i), "of first stage".into(), format!("{:.0}%", ds.cat_total(i) / first * 100.0)), (cat_col(i), "value".into(), fmt_v(ds.cat_total(i), ds))]);
            }
            clicked = cp.and_then(hit).map(|i| (0, i));
        }

        // ───────────── radar ─────────────
        Kind::Radar => {
            let vis: Vec<usize> = (0..ds.series.len()).filter(|i| show_s(*i)).collect();
            let max = vis.iter().flat_map(|s| ds.series[*s].vals.iter().copied()).fold(0.0_f64, f64::max);
            let (top, _) = nice_scale(max);
            let center = plot.center();
            let r = (plot.width().min(plot.height()) / 2.0 - 26.0).max(20.0);
            let ang = |i: usize| -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * i as f32 / n.max(3) as f32;
            for ring in 1..=4 {
                let rr = r * ring as f32 / 4.0;
                let pts: Vec<Pos2> = (0..n.max(3)).map(|i| center + vec2(ang(i).cos(), ang(i).sin()) * rr).chain(std::iter::once(center + vec2(ang(0).cos(), ang(0).sin()) * rr)).collect();
                p.add(Shape::line(pts, Stroke::new(1.0_f32, with_alpha(pal.border, 0.9))));
            }
            for i in 0..n.max(3) {
                let d = vec2(ang(i).cos(), ang(i).sin());
                p.line_segment([center, center + d * r], Stroke::new(1.0_f32, with_alpha(pal.border, 0.9)));
                if let Some(name) = ds.cats.get(i) {
                    let al = if d.x > 0.3 { Align2::LEFT_CENTER } else if d.x < -0.3 { Align2::RIGHT_CENTER } else { Align2::CENTER_CENTER };
                    text_fit(&p, center + d * (r + 8.0) + vec2(0.0, if d.y > 0.5 { 6.0 } else if d.y < -0.5 { -6.0 } else { 0.0 }), al, name, body(11.0), pal.muted, 90.0);
                }
            }
            let pt = |i: usize, v: f64| center + vec2(ang(i).cos(), ang(i).sin()) * (r * (v / top) as f32 * t);
            let hit = |pos: Pos2| -> Option<(usize, usize)> {
                let mut best: Option<(f32, usize, usize)> = None;
                for s in &vis {
                    for i in 0..n {
                        let d = (pt(i, ds.series[*s].vals[i]) - pos).length();
                        if d < 14.0 && best.map_or(true, |b| d < b.0) {
                            best = Some((d, *s, i));
                        }
                    }
                }
                best.map(|b| (b.1, b.2))
            };
            let hv = hp.and_then(hit);
            for s in &vis {
                let col = if ds.series.len() == 1 { series_color(st.palette, 0) } else { ser_col(*s) };
                let mut pts: Vec<Pos2> = (0..n).map(|i| pt(i, ds.series[*s].vals[i])).collect();
                if n >= 3 {
                    // fill as a fan from the centre
                    let mut m = Mesh::default();
                    m.colored_vertex(center, with_alpha(col, 0.18));
                    for q in &pts {
                        m.colored_vertex(*q, with_alpha(col, 0.18));
                    }
                    for i in 0..n as u32 {
                        m.add_triangle(0, i + 1, (i + 1) % n as u32 + 1);
                    }
                    p.add(Shape::mesh(m));
                }
                pts.push(pts[0]);
                p.add(Shape::line(pts.clone(), Stroke::new(2.0_f32, col)));
                for q in pts.iter().take(n) {
                    p.circle_filled(*q, 3.0, col);
                }
            }
            if let Some((s, i)) = hv {
                hand(ui, true);
                tip(&ctx, pal, hp.unwrap_or(center), &ds.cats[i], &[(ser_col(s), ds.series[s].name.clone(), fmt_v(ds.series[s].vals[i], ds))]);
            }
            clicked = cp.and_then(hit);
        }
    }
    clicked
}

// ───────────────────────────── scatter ─────────────────────────────

/// Scatter plot; returns the clicked record.
#[allow(clippy::too_many_arguments)]
pub fn scatter(ui: &mut Ui, pal: &Palette, id: Id, size: Vec2, sc: &Scatter, palette: u8, xname: &str, yname: &str, title_of: &dyn Fn(u64) -> String) -> Option<u64> {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let ctx = ui.ctx().clone();
    let p = ui.painter_at(rect);
    let t = appear(&ctx, id.with("in"), 0.5);
    if sc.pts.is_empty() {
        p.text(rect.center(), Align2::CENTER_CENTER, "No records with both numbers", body(13.0), pal.muted);
        return None;
    }
    let maxx = sc.pts.iter().map(|q| q.0).fold(0.0_f64, f64::max);
    let maxy = sc.pts.iter().map(|q| q.1).fold(0.0_f64, f64::max);
    let minx = sc.pts.iter().map(|q| q.0).fold(0.0_f64, f64::min);
    let miny = sc.pts.iter().map(|q| q.1).fold(0.0_f64, f64::min);
    let (tx, sx) = nice_scale(maxx);
    let (ty, sy) = nice_scale(maxy);
    let lox = if minx < 0.0 { -((-minx / sx).ceil() * sx) } else { 0.0 };
    let loy = if miny < 0.0 { -((-miny / sy).ceil() * sy) } else { 0.0 };
    let axis_w = tw(&p, &fmt_num(ty), 11.0) + 10.0;
    let area = Rect::from_min_max(pos2(rect.left() + axis_w + 8.0, rect.top() + 8.0), pos2(rect.right() - 12.0, rect.bottom() - 34.0));
    if area.width() < 30.0 || area.height() < 20.0 {
        return None;
    }
    let xp = |v: f64| area.left() + ((v - lox) / (tx - lox)) as f32 * area.width();
    let yp = |v: f64| area.bottom() - ((v - loy) / (ty - loy)) as f32 * area.height();
    let mut v = loy;
    while v <= ty + sy * 0.01 {
        p.hline(area.x_range(), yp(v), Stroke::new(1.0_f32, with_alpha(pal.border, 0.9)));
        p.text(pos2(area.left() - 6.0, yp(v)), Align2::RIGHT_CENTER, fmt_num(v), body(11.0), pal.muted);
        v += sy;
    }
    let mut v = lox;
    while v <= tx + sx * 0.01 {
        p.vline(xp(v), area.y_range(), Stroke::new(1.0_f32, with_alpha(pal.border, 0.5)));
        p.text(pos2(xp(v), area.bottom() + 10.0), Align2::CENTER_CENTER, fmt_num(v), body(11.0), pal.muted);
        v += sx;
    }
    p.text(pos2(area.center().x, rect.bottom() - 8.0), Align2::CENTER_CENTER, xname, bold(11.5), pal.muted);
    p.text(pos2(rect.left() + 2.0, rect.top() + 2.0), Align2::LEFT_TOP, yname, bold(11.5), pal.muted);
    let col_of = |ci: usize| sc.cats.get(ci).and_then(|c| c.1).unwrap_or_else(|| series_color(palette, ci));
    let hp = resp.hover_pos();
    let near = |pos: Pos2| -> Option<usize> {
        let mut best: Option<(f32, usize)> = None;
        for (i, q) in sc.pts.iter().enumerate() {
            let d = (pos2(xp(q.0), yp(q.1)) - pos).length();
            if d < 12.0 && best.map_or(true, |b| d < b.0) {
                best = Some((d, i));
            }
        }
        best.map(|b| b.1)
    };
    let hv = hp.and_then(near);
    for (i, q) in sc.pts.iter().enumerate() {
        let c0 = pos2(xp(q.0), yp(q.1));
        let c0 = pos2(c0.x, area.bottom() + (c0.y - area.bottom()) * t);
        let hot = hv == Some(i);
        p.circle_filled(c0, if hot { 8.0 } else { 5.5 }, with_alpha(col_of(q.3), if hot { 0.95 } else { 0.7 }));
        p.circle_stroke(c0, if hot { 8.0 } else { 5.5 }, Stroke::new(1.2_f32, col_of(q.3)));
    }
    if let Some(i) = hv {
        hand(ui, true);
        let q = &sc.pts[i];
        tip(&ctx, pal, hp.unwrap_or(rect.center()), &title_of(q.2), &[(col_of(q.3), xname.to_owned(), fmt_num(q.0)), (col_of(q.3), yname.to_owned(), fmt_num(q.1))]);
    }
    if sc.cats.len() > 1 {
        let mut x = area.left();
        for (i, (name, _)) in sc.cats.iter().enumerate().take(8) {
            p.circle_filled(pos2(x + 4.0, rect.top() + 8.0), 4.0, col_of(i));
            let w = text_fit(&p, pos2(x + 12.0, rect.top() + 8.0), Align2::LEFT_CENTER, name, body(11.0), pal.muted, 80.0);
            x += w + 26.0;
        }
    }
    if resp.clicked() {
        return resp.interact_pointer_pos().or(hp).and_then(near).map(|i| sc.pts[i].2);
    }
    None
}

// ───────────────────────────── gauge ─────────────────────────────

/// Semi-circular gauge: `frac` of the goal reached (may exceed 1.0).
pub fn gauge(ui: &mut Ui, pal: &Palette, id: Id, size: Vec2, frac: f64, value: &str, caption: &str, col: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let t = appear(ui.ctx(), id.with("in"), 0.7);
    let p = ui.painter_at(rect);
    let r = (rect.width() / 2.0 - 8.0).min(rect.height() - 26.0).max(20.0);
    let center = pos2(rect.center().x, rect.top() + r + 8.0);
    let th = (r * 0.2).clamp(8.0, 22.0);
    use std::f32::consts::PI;
    sector(&p, center, r - th, r, PI, 2.0 * PI, with_alpha(pal.muted, 0.18));
    let f = (frac.clamp(0.0, 1.0) as f32) * t;
    if f > 0.002 {
        sector(&p, center, r - th, r, PI, PI + PI * f, if frac >= 1.0 { c(46, 160, 110) } else { col });
    }
    p.text(center - vec2(0.0, r * 0.28), Align2::CENTER_CENTER, value, bold((r * 0.34).clamp(16.0, 34.0)), pal.text);
    p.text(center + vec2(0.0, 8.0), Align2::CENTER_CENTER, format!("{:.0}% of goal", frac * 100.0), body(12.0), pal.muted);
    if !caption.is_empty() {
        text_fit(&p, pos2(rect.center().x, rect.bottom() - 6.0), Align2::CENTER_CENTER, caption, body(11.5), pal.muted, rect.width() - 8.0);
    }
    resp
}

// ───────────────────────────── heat-map ─────────────────────────────

/// GitHub-style activity calendar ending at `end_day`. Returns the clicked day.
pub fn heatmap(ui: &mut Ui, pal: &Palette, id: Id, size: Vec2, counts: &BTreeMap<i64, Vec<u64>>, end_day: i64, col: Color32) -> Option<i64> {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let ctx = ui.ctx().clone();
    let p = ui.painter_at(rect);
    let t = appear(&ctx, id.with("in"), 0.6);
    let left = 26.0;
    let top = 16.0;
    let gap = 3.0;
    let cell = ((rect.height() - top) / 7.0 - gap).clamp(7.0, 20.0);
    let weeks = (((rect.width() - left) / (cell + gap)).floor() as i64).clamp(4, 60);
    // the last column is the week of end_day (Monday first)
    let end_week = end_day - (end_day + 3).rem_euclid(7);
    let first_week = end_week - (weeks - 1) * 7;
    let max = counts.values().map(|v| v.len()).max().unwrap_or(1).max(1) as f32;
    let hp = resp.hover_pos();
    let pos_of = |day: i64| -> Rect {
        let w = (day - first_week).div_euclid(7);
        let d = (day - first_week).rem_euclid(7);
        Rect::from_min_size(pos2(rect.left() + left + w as f32 * (cell + gap), rect.top() + top + d as f32 * (cell + gap)), vec2(cell, cell))
    };
    let day_at = |pos: Pos2| -> Option<i64> {
        let w = ((pos.x - rect.left() - left) / (cell + gap)).floor() as i64;
        let d = ((pos.y - rect.top() - top) / (cell + gap)).floor() as i64;
        if w < 0 || w >= weeks || !(0..7).contains(&d) {
            return None;
        }
        let day = first_week + w * 7 + d;
        (day <= end_day).then_some(day)
    };
    for (i, name) in ["Mon", "Wed", "Fri"].iter().enumerate() {
        p.text(pos2(rect.left(), rect.top() + top + (i as f32 * 2.0) * (cell + gap) + cell / 2.0), Align2::LEFT_CENTER, name, body(10.0), pal.muted);
    }
    let hv = hp.and_then(day_at);
    let mut last_month = 0;
    for w in 0..weeks {
        let (_, m, _) = dates::civil_from_days(first_week + w * 7 + 6);
        if m != last_month {
            last_month = m;
            p.text(pos2(rect.left() + left + w as f32 * (cell + gap), rect.top() + 2.0), Align2::LEFT_TOP, &dates::MONTHS[m as usize - 1][..3], body(10.0), pal.muted);
        }
    }
    for day in first_week..=end_day {
        let r = pos_of(day);
        let n = counts.get(&day).map_or(0, |v| v.len());
        let fill = if n == 0 { with_alpha(pal.muted, 0.14) } else { with_alpha(col, (0.28 + 0.72 * (n as f32 / max)) * t) };
        p.rect_filled(r, 3.0, fill);
        if day == end_day {
            p.rect_stroke(r, 3.0, Stroke::new(1.5_f32, pal.text), egui::StrokeKind::Outside);
        }
        if hv == Some(day) {
            p.rect_stroke(r, 3.0, Stroke::new(1.5_f32, with_alpha(pal.text, 0.6)), egui::StrokeKind::Outside);
        }
    }
    if let Some(day) = hv {
        hand(ui, true);
        let n = counts.get(&day).map_or(0, |v| v.len());
        tip(&ctx, pal, hp.unwrap_or(rect.center()), &dates::pretty(&dates::iso_of_day(day)), &[(col, if n == 1 { "record".into() } else { "records".into() }, n.to_string())]);
    }
    if resp.clicked() {
        return resp.interact_pointer_pos().or(hp).and_then(day_at);
    }
    None
}

// ───────────────────────────── sparkline ─────────────────────────────

pub fn sparkline(p: &Painter, rect: Rect, vals: &[f64], col: Color32) {
    if vals.len() < 2 || rect.width() < 10.0 {
        return;
    }
    let (lo, hi) = vals.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, v| (a.0.min(*v), a.1.max(*v)));
    let span = (hi - lo).max(1e-9);
    let pts: Vec<Pos2> = vals.iter().enumerate().map(|(i, v)| pos2(rect.left() + rect.width() * i as f32 / (vals.len() - 1) as f32, rect.bottom() - 2.0 - ((*v - lo) / span) as f32 * (rect.height() - 4.0))).collect();
    let mut m = Mesh::default();
    for w in pts.windows(2) {
        mesh_quad(&mut m, w[0], w[1], pos2(w[1].x, rect.bottom()), pos2(w[0].x, rect.bottom()), with_alpha(col, 0.14));
    }
    p.add(Shape::mesh(m));
    p.add(Shape::line(pts.clone(), Stroke::new(1.8_f32, col)));
    if let Some(l) = pts.last() {
        p.circle_filled(*l, 3.0, col);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_scales_cover_the_data() {
        for max in [0.3, 1.0, 7.0, 99.0, 350.0, 12_345.0, 1.0e7] {
            let (top, step) = nice_scale(max);
            assert!(top >= max, "{max} -> {top}");
            let ticks = (top / step).round();
            assert!((1.0..=6.0).contains(&ticks), "{max}: {ticks} ticks");
        }
        assert_eq!(nice_scale(350.0), (400.0, 100.0));
        assert_eq!(nice_scale(0.0).0, 1.0);
    }
}

//! Windowed table for large datasets (`Workspace.bigs`): only the rows on screen exist in memory, they are read from the
//! disk-backed store as the scroll position moves (one or two chunks per scroll step, ~3 ms). Sorting and searching run on a
//! worker thread and produce a list of row ids; editing writes through the store's write-ahead log.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use egui::{pos2, vec2, Align2, Area, Color32, Id, Key, Margin, Order, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, TextEdit, Ui};
use lumen_store::{Row, Value};

use crate::bigtable::{self as bt, lock, Shared};
use crate::model::{Database, PropKind, Workspace};
use crate::theme::{body, bold, tag_color, Palette};
use crate::widgets::{ghost_button, text_fit, with_alpha};

const ROW_H: f32 = 34.0;
const HEAD_H: f32 = 38.0;

struct Edit {
    row: u64,
    prop: u64,
    rect: Rect,
    text: String,
    focus: bool,
    age: u32,
}

#[derive(Default)]
struct UiSt {
    sort: Option<(u64, bool)>,
    query: String,
    query_at: f64,
    view: Option<Arc<Vec<u64>>>,
    view_key: u64,
    job: Option<(u64, Arc<Mutex<Option<Vec<u64>>>>)>,
    gen: u64,
    edit: Option<Edit>,
    win: Option<(u64, Vec<Row>)>,
    note: String,
    to_end: bool,
}

thread_local! {
    static UI: RefCell<HashMap<u64, UiSt>> = RefCell::new(HashMap::new());
}

struct Col {
    prop: u64,
    name: String,
    kind: PropKind,
    w: f32,
}

fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn hash<T: Hash>(t: T) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

fn now() -> u64 {
    crate::auth::now_ms() as u64
}

/// Writes one cell (or the title) of one row through the store.
fn commit(st: &Shared, dir: &str, row_id: u64, prop: u64, val: Option<Value>, title: Option<String>) {
    {
        let mut g = lock(st);
        if let Ok(Some(mut r)) = g.get(row_id) {
            if prop == 0 {
                if let Some(t) = title {
                    r.title = t;
                }
            } else {
                r.props.retain(|p| p.0 != prop as u32);
                if let Some(v) = val {
                    r.props.push((prop as u32, v));
                    r.props.sort_by_key(|p| p.0);
                }
            }
            r.updated = now();
            let id = r.id;
            let _ = g.upsert(r);
            let _ = g.checkpoint();
            bt::note_write(dir, id);
        }
    }
    bt::bump_version(dir);
}

fn paint_cell(p: &egui::Painter, cr: Rect, c: &Col, row: &Row, db: &Database, pal: &Palette) {
    let pad = 10.0;
    let mid = cr.center().y;
    if c.prop == 0 {
        text_fit(
            p,
            pos2(cr.left() + pad, mid),
            Align2::LEFT_CENTER,
            &row.title,
            body(13.5),
            pal.text,
            cr.width() - pad * 2.0,
        );
        return;
    }
    let Some(v) = row.get(c.prop as u32) else {
        return;
    };
    match v {
        Value::Num(x) => {
            text_fit(
                p,
                pos2(cr.right() - pad, mid),
                Align2::RIGHT_CENTER,
                &crate::dashdata::fmt_num(*x),
                body(13.5),
                pal.text,
                cr.width() - pad * 2.0,
            );
        }
        Value::Sel(o) => {
            if let Some(opt) = db.opt(c.prop, *o) {
                let w = (opt.name.chars().count() as f32 * 7.2 + 18.0).min(cr.width() - 12.0);
                let r = Rect::from_min_size(pos2(cr.left() + 8.0, mid - 11.0), vec2(w, 22.0));
                p.rect_filled(r, 11.0, tag_color(opt.color).gamma_multiply(0.38));
                text_fit(p, pos2(r.left() + 9.0, mid), Align2::LEFT_CENTER, &opt.name, body(12.5), pal.text, w - 14.0);
            }
        }
        Value::Check(b) => {
            let r = Rect::from_center_size(pos2(cr.left() + 22.0, mid), vec2(17.0, 17.0));
            if *b {
                p.rect_filled(r, 4.0, pal.accent);
                let st = Stroke::new(2.0_f32, Color32::WHITE);
                p.line_segment([r.min + vec2(4.0, 8.5), r.min + vec2(7.2, 12.0)], st);
                p.line_segment([r.min + vec2(7.2, 12.0), r.min + vec2(13.0, 5.0)], st);
            } else {
                p.rect_stroke(r, 4.0, Stroke::new(1.4_f32, pal.muted), StrokeKind::Inside);
            }
        }
        other => {
            let t = bt::cell_text(db, c.prop as u32, other);
            text_fit(p, pos2(cr.left() + pad, mid), Align2::LEFT_CENTER, &t, body(13.5), pal.text, cr.width() - pad * 2.0);
        }
    }
}

/// The page body of a large dataset. Returns true when the workspace metadata changed (never: rows live in the store).
pub fn show(ui: &mut Ui, ws: &mut Workspace, id: u64, pal: &Palette, w: f32) -> bool {
    let Some(meta) = ws.bigs.get(&id).cloned() else {
        return false;
    };
    let Some(db) = ws.db(id).cloned() else {
        return false;
    };
    let ctx = ui.ctx().clone();
    let Some(st) = bt::open_store(&meta, false) else {
        ui.add_space(24.0);
        ui.label(
            RichText::new("This dataset is not available in this session. Browser data is kept in memory only — import the CSV again, or use the desktop / Android app for persistent large datasets.")
                .font(body(14.0))
                .color(pal.muted),
        );
        return false;
    };
    let title = ws.page(id).map(|p| p.title.clone()).unwrap_or_default();
    let mut s = UI.with(|m| m.borrow_mut().remove(&id)).unwrap_or_default();
    let t_now = ctx.input(|i| i.time);
    let total = lock(&st).len().unwrap_or(0);
    let touchy = crate::touch::touchy();

    // ── toolbar ──
    ui.horizontal_wrapped(|ui| {
        let te = TextEdit::singleline(&mut s.query).hint_text(format!("Search {} rows…", group(total))).desired_width((w * 0.4).clamp(150.0, 320.0)).margin(Margin::symmetric(8, 6));
        if ui.add(te).changed() {
            s.query_at = t_now;
        }
        if ghost_button(ui, pal, "+ Row").clicked() {
            let rid = {
                let mut g = lock(&st);
                let rid = bt::new_row_id(&mut g);
                let _ = g.upsert(Row { id: rid, title: "Untitled".into(), updated: now(), props: vec![] });
                let _ = g.checkpoint();
                bt::note_write(&meta.dir, rid);
                rid
            };
            let _ = rid;
            bt::bump_version(&meta.dir);
            s.gen += 1;
            s.to_end = true;
        }
        if ghost_button(ui, pal, "Export CSV").clicked() {
            match bt::export_csv(ws, id) {
                Some(csv) => s.note = crate::platform::save_download(&format!("{}.csv", if title.is_empty() { "dataset" } else { &title }), csv.as_bytes(), "text/csv"),
                None => s.note = "Export failed".into(),
            }
        }
    });
    let (cache_mb, disk_mb) = {
        let g = lock(&st);
        (g.cache_bytes() as f64 / 1048576.0, g.disk_bytes() as f64 / 1048576.0)
    };
    let rss = crate::memgov::rss_mb().map(|m| format!(" · app memory {m:.0} MB")).unwrap_or_default();
    let storage = if meta.volatile { "in memory (this browser session)" } else { "on disk" };
    ui.label(
        RichText::new(format!(
            "{} rows · {storage} {disk_mb:.1} MB · row cache {cache_mb:.1} MB{rss}{}",
            group(total),
            if s.note.is_empty() { String::new() } else { format!(" · {}", s.note) }
        ))
        .font(body(11.5))
        .color(pal.muted),
    );
    ui.add_space(4.0);

    // ── sorted / searched view (worker) ──
    let want_view = s.sort.is_some() || !s.query.trim().is_empty();
    let vkey = hash((s.sort, s.query.trim().to_lowercase(), s.gen, bt::version(&meta.dir)));
    if !want_view {
        s.view = None;
        s.view_key = 0;
        s.job = None;
    } else if s.view_key != vkey && s.job.as_ref().map_or(true, |j| j.0 != vkey) {
        if t_now - s.query_at > 0.25 {
            let slot: Arc<Mutex<Option<Vec<u64>>>> = Arc::new(Mutex::new(None));
            let (st2, db2, sort, q, slot2, ctx2) = (st.clone(), db.clone(), s.sort, s.query.clone(), slot.clone(), ctx.clone());
            crate::bigdata::run_bg(Box::new(move || {
                let r = bt::build_view(&st2, &db2, sort, &q, 2_000_000).unwrap_or_default();
                if let Ok(mut g) = slot2.lock() {
                    *g = Some(r);
                }
                ctx2.request_repaint();
            }));
            s.job = Some((vkey, slot));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(260));
        }
    }
    if let Some((k, slot)) = s.job.take() {
        let got = slot.lock().ok().and_then(|mut g| g.take());
        match got {
            Some(r) => {
                s.view = Some(Arc::new(r));
                s.view_key = k;
            }
            None => {
                s.job = Some((k, slot));
                ctx.request_repaint_after(std::time::Duration::from_millis(80));
            }
        }
    }
    let busy = want_view && s.view_key != vkey;
    let n = s.view.as_ref().map_or(total, |v| v.len() as u64);
    if want_view {
        ui.label(RichText::new(if busy { "Sorting / searching on a background thread…".to_owned() } else { format!("{} matching rows", group(n)) }).font(body(11.5)).color(pal.muted));
    }

    // ── columns ──
    let title_w = (w * 0.55).clamp(180.0, 300.0);
    let mut cols = vec![Col { prop: 0, name: "Name".into(), kind: PropKind::Text, w: title_w }];
    for p in &db.props {
        if matches!(p.kind, PropKind::Formula | PropKind::Relation | PropKind::Rollup) {
            continue;
        }
        let cw = match p.kind {
            PropKind::Number => 110.0,
            PropKind::Select => 150.0,
            PropKind::Checkbox => 84.0,
            PropKind::Date => 130.0,
            PropKind::MultiSelect => 180.0,
            _ => 220.0,
        };
        cols.push(Col { prop: p.id, name: p.name.clone(), kind: p.kind, w: cw });
    }
    let total_w = cols.iter().map(|c| c.w).sum::<f32>() + 48.0;
    let height = (ctx.screen_rect().height() - 250.0).clamp(260.0, 900.0);

    let mut sa = ScrollArea::both().id_salt(("bigtbl", id)).max_height(height).auto_shrink([false, false]);
    if std::mem::take(&mut s.to_end) {
        sa = sa.vertical_scroll_offset(1.0e9);
    }
    let mut clicked_header: Option<u64> = None;
    let mut start_edit: Option<Edit> = None;
    let mut toggle: Option<(u64, u64, bool)> = None;
    let mut delete: Option<u64> = None;
    sa.show_viewport(ui, |ui, vp| {
        let content_h = HEAD_H + n as f32 * ROW_H;
        let (content, _) = ui.allocate_exact_size(vec2(total_w.max(w), content_h.max(HEAD_H + ROW_H)), Sense::hover());
        let first = ((vp.top() - HEAD_H) / ROW_H).floor().max(0.0) as u64;
        let count = ((vp.height() / ROW_H).ceil() as u64 + 2).min(n.saturating_sub(first));
        let wkey = hash((first, count, s.gen, s.view_key, n, bt::version(&meta.dir)));
        if s.win.as_ref().map_or(true, |w| w.0 != wkey) {
            let rows: Vec<Row> = match &s.view {
                None => lock(&st).window(first, count as usize).unwrap_or_default(),
                Some(v) => {
                    let mut g = lock(&st);
                    v.iter().skip(first as usize).take(count as usize).filter_map(|id| g.get(*id).ok().flatten()).collect()
                }
            };
            s.win = Some((wkey, rows));
        }
        let painter = ui.painter().clone();
        if let Some((_, rows)) = &s.win {
            for (k, row) in rows.iter().enumerate() {
                let y = content.top() + HEAD_H + (first + k as u64) as f32 * ROW_H;
                let rr = Rect::from_min_size(pos2(content.left(), y), vec2(content.width(), ROW_H));
                if !ui.is_rect_visible(rr) {
                    continue;
                }
                let resp = ui.interact(rr, Id::new(("bigrow", id, row.id)), Sense::click());
                if resp.hovered() {
                    painter.rect_filled(rr, 0.0, with_alpha(pal.hover, 0.7));
                }
                painter.hline(rr.x_range(), rr.bottom(), Stroke::new(1.0_f32, pal.border));
                let mut x = rr.left();
                let mut rects = vec![];
                for c in &cols {
                    let cr = Rect::from_min_size(pos2(x, y), vec2(c.w, ROW_H));
                    paint_cell(&painter, cr, c, row, &db, pal);
                    rects.push(cr);
                    x += c.w;
                }
                let dr = Rect::from_min_size(pos2(x, y), vec2(48.0, ROW_H));
                if resp.hovered() || touchy {
                    crate::icons::glyph(&painter, dr.center(), 15.0, crate::lucide::X, pal.muted);
                }
                if resp.clicked() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        if dr.contains(pos) {
                            delete = Some(row.id);
                        } else if let Some((ci, cr)) = cols.iter().zip(&rects).enumerate().find(|(_, (_, r))| r.x_range().contains(pos.x)).map(|(i, (_, r))| (i, *r)) {
                            let c = &cols[ci];
                            match c.kind {
                                PropKind::Checkbox if c.prop != 0 => {
                                    let cur = matches!(row.get(c.prop as u32), Some(Value::Check(true)));
                                    toggle = Some((row.id, c.prop, !cur));
                                }
                                PropKind::MultiSelect => {}
                                _ => {
                                    let text = if c.prop == 0 {
                                        row.title.clone()
                                    } else {
                                        match row.get(c.prop as u32) {
                                            Some(Value::Num(x)) => format!("{x}"),
                                            Some(v) if c.kind != PropKind::Select => bt::cell_text(&db, c.prop as u32, v),
                                            _ => String::new(),
                                        }
                                    };
                                    start_edit = Some(Edit { row: row.id, prop: c.prop, rect: cr, text, focus: true, age: 0 });
                                }
                            }
                        }
                    }
                }
            }
        }
        // sticky header, drawn last
        let hr = Rect::from_min_size(pos2(content.left(), content.top() + vp.top()), vec2(content.width(), HEAD_H));
        painter.rect_filled(hr, 0.0, pal.bg);
        painter.hline(hr.x_range(), hr.bottom(), Stroke::new(1.0_f32, pal.border));
        let mut x = hr.left();
        for c in &cols {
            let cr = Rect::from_min_size(pos2(x, hr.top()), vec2(c.w, HEAD_H));
            let resp = ui.interact(cr, Id::new(("bighead", id, c.prop)), Sense::click());
            if resp.hovered() {
                painter.rect_filled(cr, 0.0, with_alpha(pal.hover, 0.8));
            }
            let arrow = match s.sort {
                Some((p, true)) if p == c.prop => " ▲",
                Some((p, false)) if p == c.prop => " ▼",
                _ => "",
            };
            text_fit(
                &painter,
                pos2(cr.left() + 10.0, cr.center().y),
                Align2::LEFT_CENTER,
                &format!("{}{arrow}", c.name),
                bold(12.5),
                if arrow.is_empty() { pal.muted } else { pal.accent },
                c.w - 16.0,
            );
            if resp.clicked() && !matches!(c.kind, PropKind::MultiSelect) {
                clicked_header = Some(c.prop);
            }
            x += c.w;
        }
    });

    if let Some(p) = clicked_header {
        s.sort = match s.sort {
            Some((q, true)) if q == p => Some((p, false)),
            Some((q, false)) if q == p => None,
            _ => Some((p, true)),
        };
    }
    if let Some((row, prop, v)) = toggle {
        commit(&st, &meta.dir, row, prop, Some(Value::Check(v)), None);
        s.gen += 1;
    }
    if let Some(r) = delete {
        {
            let mut g = lock(&st);
            let _ = g.delete(r);
            let _ = g.checkpoint();
            bt::note_delete(&meta.dir, r);
        }
        bt::bump_version(&meta.dir);
        s.gen += 1;
    }
    if start_edit.is_some() {
        s.edit = start_edit;
    }

    // ── cell editor ──
    if let Some(mut e) = s.edit.take() {
        let kind = if e.prop == 0 { PropKind::Text } else { db.kind_of(e.prop) };
        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        let mut result: Option<Option<Value>> = None;
        let mut title_out: Option<String> = None;
        let mut cancel = esc;
        let area = Area::new(Id::new(("bigedit", id))).order(Order::Foreground).fixed_pos(e.rect.min).constrain(true).show(&ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                if kind == PropKind::Select {
                    ui.set_min_width(e.rect.width().max(160.0));
                    if let Some(p) = db.prop(e.prop) {
                        for o in &p.opts {
                            if ui.button(RichText::new(&o.name).color(tag_color(o.color))).clicked() {
                                result = Some(Some(Value::Sel(o.id)));
                            }
                        }
                    }
                    if ui.button("Clear").clicked() {
                        result = Some(None);
                    }
                } else {
                    ui.set_min_width(e.rect.width().max(200.0));
                    let r = TextEdit::singleline(&mut e.text).id(Id::new(("bigedit_te", id))).desired_width(f32::INFINITY).show(ui);
                    if e.focus {
                        r.response.request_focus();
                        e.focus = false;
                    }
                    if r.response.lost_focus() && !esc {
                        let t = e.text.trim().to_owned();
                        if e.prop == 0 {
                            title_out = Some(t);
                            result = Some(None);
                        } else if t.is_empty() {
                            result = Some(None);
                        } else {
                            result = Some(match kind {
                                PropKind::Number => t.parse::<f64>().ok().map(Value::Num),
                                _ => Some(Value::Text(t)),
                            });
                        }
                    }
                }
            });
        });
        if e.age > 0 && ctx.input(|i| i.pointer.any_pressed()) && ctx.input(|i| i.pointer.interact_pos()).map_or(false, |p| !area.response.rect.contains(p)) {
            cancel = true;
        }
        e.age += 1;
        if let Some(v) = result {
            commit(&st, &meta.dir, e.row, e.prop, v, title_out);
            s.gen += 1;
        } else if !cancel {
            s.edit = Some(e);
        }
    }

    UI.with(|m| m.borrow_mut().insert(id, s));
    false
}

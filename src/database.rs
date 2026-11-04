//! Notion-style databases: table, board, list, gallery and calendar views with filters, sorts and typed properties.
//!
//! Rendering is read-only over the workspace; every edit is recorded as an [`A`]ction and applied afterwards.

use egui::{pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Frame, Id, Key, Layout, Margin, Order, Painter, Pos2, Rect, Response, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui};

use crate::auth::now_ms;
use crate::dates;
use crate::editor::{EditorState, GUTTER};
use crate::icons::{self, Icon};
use crate::model::*;
use crate::theme::{bold, body, lerp_color, tag_color, Palette, COVERS, TAG_NAMES};
use crate::widgets::{field, gradient_rect, icon_button, menu_item, popup_area, section_label, with_alpha};

const ROW_H: f32 = 34.0;
const TITLE_W: f32 = 280.0;

#[derive(Clone)]
enum Popup {
    Select { row: u64, prop: u64, pos: Pos2 },
    Date { row: u64, prop: u64, pos: Pos2, y: i32, m: u32 },
    Filter(Pos2),
    Sort(Pos2),
    Props(Pos2),
    PropMenu { prop: u64, pos: Pos2 },
    ViewMenu(Pos2),
    AddView(Pos2),
}

#[derive(Default)]
pub struct DbState {
    popup: Option<Popup>,
    fresh: bool,
    cal: Option<(i32, u32)>,
    drag: Option<u64>,
    new_opt: String,
    focus_row: Option<u64>,
}

enum A {
    SetVal(u64, u64, Option<Val>),
    SetTitle(u64, String),
    AddRow(Option<(u64, Val)>),
    DelRow(u64),
    Open(u64),
    SetActive(usize),
    AddView(ViewKind),
    DelView(usize),
    RenameView(usize, String),
    SetViewKind(usize, ViewKind),
    SetGroup(usize, u64),
    SetDateProp(usize, u64),
    AddFilter(usize),
    SetFilter(usize, usize, Filter),
    DelFilter(usize, usize),
    AddSort(usize),
    SetSort(usize, usize, Sort),
    DelSort(usize, usize),
    AddProp(PropKind),
    RenameProp(u64, String),
    SetPropKind(u64, PropKind),
    DelProp(u64),
    AddOpt(u64, String, Option<u64>),
    RenameOpt(u64, u64, String),
    RecolorOpt(u64, u64),
    DelOpt(u64, u64),
}

fn col_w(k: PropKind) -> f32 {
    match k {
        PropKind::Text => 190.0,
        PropKind::Number => 120.0,
        PropKind::Select => 170.0,
        PropKind::MultiSelect => 210.0,
        PropKind::Checkbox => 100.0,
        PropKind::Date => 150.0,
        PropKind::Url => 210.0,
    }
}

// ───────────────────────────── small widgets ─────────────────────────────

fn chip_paint(p: &Painter, left_center: Pos2, text: &str, color: usize, pal: &Palette) -> f32 {
    let col = tag_color(color);
    let g = p.layout_no_wrap(text.to_owned(), body(12.5), lerp_color(col, pal.text, 0.25));
    let w = g.size().x + 14.0;
    let r = Rect::from_min_size(pos2(left_center.x, left_center.y - 10.0), vec2(w, 20.0));
    p.rect_filled(r, 10.0, col.gamma_multiply(0.2));
    p.galley(pos2(r.left() + 7.0, r.center().y - g.size().y / 2.0), g, Color32::WHITE);
    w
}

fn chip_ui(ui: &mut Ui, pal: &Palette, text: &str, color: usize) {
    let g = ui.painter().layout_no_wrap(text.to_owned(), body(12.5), pal.text);
    let (r, _) = ui.allocate_exact_size(vec2(g.size().x + 14.0, 20.0), Sense::hover());
    chip_paint(ui.painter(), r.left_center(), text, color, pal);
}

fn pill(ui: &mut Ui, pal: &Palette, label: &str, active: bool, accent: bool) -> Response {
    let g = ui.painter().layout_no_wrap(label.to_owned(), body(13.5), pal.text);
    let (rect, resp) = ui.allocate_exact_size(g.size() + vec2(18.0, 12.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
    let (bg, col) = if accent {
        (lerp_color(pal.accent, Color32::from_rgb(20, 100, 190), t * 0.5), Color32::WHITE)
    } else if active {
        (pal.active, pal.text)
    } else {
        (with_alpha(pal.hover, t), lerp_color(pal.muted, pal.text, t))
    };
    ui.painter().rect_filled(rect, 6.0, bg);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, body(13.5), col);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn field_btn(ui: &mut Ui, pal: &Palette, label: &str, width: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 30.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
    ui.painter().rect_filled(rect, 7.0, lerp_color(pal.hover, pal.active, t));
    ui.painter().with_clip_rect(rect.shrink2(vec2(4.0, 0.0))).text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, label, body(13.5), pal.text);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn check_box(ui: &Ui, pal: &Palette, c: Pos2, on: bool) {
    let r = Rect::from_center_size(c, vec2(16.0, 16.0));
    if on {
        ui.painter().rect_filled(r, 4.0, pal.accent);
        ui.painter().add(egui::Shape::line(vec![pos2(c.x - 4.0, c.y), pos2(c.x - 1.0, c.y + 3.0), pos2(c.x + 4.5, c.y - 3.5)], Stroke::new(2.0_f32, Color32::WHITE)));
    } else {
        ui.painter().rect_stroke(r, 4.0, Stroke::new(1.5_f32, pal.muted), egui::StrokeKind::Inside);
    }
}

fn title_of(r: &Page) -> &str {
    if r.title.is_empty() {
        "Untitled"
    } else {
        &r.title
    }
}

// ───────────────────────────── entry points ─────────────────────────────

/// Renders the full database (views toolbar + active view). Returns `true` when data changed.
pub fn show(ui: &mut Ui, ws: &mut Workspace, id: u64, ed: &mut EditorState, pal: &Palette, w: f32) -> bool {
    let Some(db) = ws.db(id).cloned() else { return false };
    if db.views.is_empty() {
        ws.add_view(id, ViewKind::Table);
        return true;
    }
    let vi = db.active.min(db.views.len() - 1);
    let mut acts: Vec<A> = vec![];
    {
        let wsr: &Workspace = ws;
        let st = &mut ed.db;
        toolbar(ui, &db, vi, st, pal, w, &mut acts);
        ui.add_space(2.0);
        let (lr, _) = ui.allocate_exact_size(vec2(w, 1.0), Sense::hover());
        ui.painter().rect_filled(lr, 0.0, pal.border);
        ui.add_space(8.0);
        let rows = wsr.query(id, vi);
        let view = &db.views[vi];
        match view.kind {
            ViewKind::Table => table(ui, wsr, &db, &rows, st, pal, w, &mut acts),
            ViewKind::Board => board(ui, wsr, &db, view, &rows, st, pal, &mut acts),
            ViewKind::List => list(ui, wsr, &db, &rows, pal, w, &mut acts),
            ViewKind::Gallery => gallery(ui, wsr, &db, &rows, pal, w, &mut acts),
            ViewKind::Calendar => calendar(ui, wsr, &db, view, &rows, st, pal, w, &mut acts),
        }
        popups(ui, wsr, &db, vi, st, pal, &mut acts);
    }
    let changed = !acts.is_empty();
    for a in acts {
        apply(ws, id, a, ed);
    }
    changed
}

/// Property panel shown under the title of a database row page.
pub fn row_props(ui: &mut Ui, ws: &mut Workspace, row_id: u64, ed: &mut EditorState, pal: &Palette, w: f32) -> bool {
    let Some(db_id) = ws.page(row_id).and_then(|p| p.parent) else { return false };
    let Some(db) = ws.db(db_id).cloned() else { return false };
    let mut acts: Vec<A> = vec![];
    {
        let wsr: &Workspace = ws;
        let Some(row) = wsr.page(row_id) else { return false };
        let st = &mut ed.db;
        for prop in &db.props {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                ui.add_space(GUTTER);
                let (lr, _) = ui.allocate_exact_size(vec2(150.0, 32.0), Sense::hover());
                ui.painter().text(pos2(lr.left() + 2.0, lr.center().y), Align2::LEFT_CENTER, format!("{}  {}", prop.kind.glyph(), prop.name), body(14.0), pal.muted);
                let (cr, _) = ui.allocate_exact_size(vec2((w - 150.0).max(120.0), 32.0), Sense::hover());
                cell(ui, &db, row, prop, cr, pal, st, &mut acts);
            });
        }
        ui.add_space(6.0);
        let (lr, _) = ui.allocate_exact_size(vec2(w + GUTTER, 1.0), Sense::hover());
        ui.painter().rect_filled(Rect::from_min_max(pos2(lr.left() + GUTTER, lr.top()), lr.right_bottom()), 0.0, pal.border);
        ui.add_space(6.0);
        popups(ui, wsr, &db, db.active, st, pal, &mut acts);
    }
    let changed = !acts.is_empty();
    for a in acts {
        apply(ws, db_id, a, ed);
    }
    changed
}

// ───────────────────────────── toolbar ─────────────────────────────

fn toolbar(ui: &mut Ui, db: &Database, vi: usize, st: &mut DbState, pal: &Palette, w: f32, acts: &mut Vec<A>) {
    let view = &db.views[vi];
    ui.horizontal(|ui| {
        ui.set_width(w);
        ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
        for (i, v) in db.views.iter().enumerate() {
            let r = pill(ui, pal, &v.name, i == vi, false);
            if r.clicked() {
                if i == vi {
                    st.popup = Some(Popup::ViewMenu(r.rect.left_bottom() + vec2(0.0, 6.0)));
                    st.fresh = true;
                } else {
                    acts.push(A::SetActive(i));
                }
            }
        }
        let r = pill(ui, pal, "+", false, false).on_hover_text("Add a view");
        if r.clicked() {
            st.popup = Some(Popup::AddView(r.rect.left_bottom() + vec2(0.0, 6.0)));
            st.fresh = true;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
            if pill(ui, pal, "New", false, true).clicked() {
                acts.push(A::AddRow(None));
            }
            ui.add_space(6.0);
            let r = pill(ui, pal, "Properties", false, false);
            if r.clicked() {
                st.popup = Some(Popup::Props(pos2(r.rect.right() - 280.0, r.rect.bottom() + 6.0)));
                st.fresh = true;
            }
            let fl = if view.filters.is_empty() { "Filter".to_owned() } else { format!("Filter · {}", view.filters.len()) };
            let r = pill(ui, pal, &fl, !view.filters.is_empty(), false);
            if r.clicked() {
                st.popup = Some(Popup::Filter(pos2(r.rect.right() - 440.0, r.rect.bottom() + 6.0)));
                st.fresh = true;
            }
            let sl = if view.sorts.is_empty() { "Sort".to_owned() } else { format!("Sort · {}", view.sorts.len()) };
            let r = pill(ui, pal, &sl, !view.sorts.is_empty(), false);
            if r.clicked() {
                st.popup = Some(Popup::Sort(pos2(r.rect.right() - 340.0, r.rect.bottom() + 6.0)));
                st.fresh = true;
            }
        });
    });
    ui.add_space(4.0);
}

// ───────────────────────────── cells ─────────────────────────────

fn open_popup_at(st: &mut DbState, p: Popup) {
    st.popup = Some(p);
    st.fresh = true;
}

#[allow(clippy::too_many_arguments)]
fn cell(ui: &mut Ui, db: &Database, row: &Page, prop: &Prop, rect: Rect, pal: &Palette, st: &mut DbState, acts: &mut Vec<A>) {
    let ctx = ui.ctx().clone();
    let id = Id::new(("cell", row.id, prop.id));
    let val = row.props.get(&prop.id);
    let clip = ui.painter().with_clip_rect(rect);
    let inner = Rect::from_center_size(rect.center(), vec2(rect.width() - 16.0, 24.0));
    match prop.kind {
        PropKind::Text | PropKind::Url | PropKind::Number => {
            let initial = val.map(|v| db.val_text(prop.id, v)).unwrap_or_default();
            let focused = ctx.memory(|m| m.has_focus(id));
            let mut buf: String = if focused { ctx.data(|d| d.get_temp::<String>(id.with("buf"))).unwrap_or_else(|| initial.clone()) } else { initial };
            let extra = if prop.kind == PropKind::Url && !buf.is_empty() { 22.0 } else { 0.0 };
            let ir = Rect::from_min_size(inner.min, vec2(inner.width() - extra, inner.height()));
            let r = ui.put(
                ir,
                TextEdit::singleline(&mut buf)
                    .id(id)
                    .frame(false)
                    .font(body(14.0))
                    .desired_width(ir.width())
                    .margin(Margin::symmetric(0, 2))
                    .horizontal_align(if prop.kind == PropKind::Number { Align::RIGHT } else { Align::LEFT }),
            );
            if r.changed() {
                ctx.data_mut(|d| d.insert_temp(id.with("buf"), buf.clone()));
                let v = if buf.is_empty() {
                    None
                } else if prop.kind == PropKind::Number {
                    buf.trim().parse::<f64>().ok().map(Val::Num)
                } else {
                    Some(Val::Text(buf.clone()))
                };
                if prop.kind != PropKind::Number || buf.is_empty() || v.is_some() {
                    acts.push(A::SetVal(row.id, prop.id, v));
                }
            }
            if extra > 0.0 {
                let br = Rect::from_min_size(pos2(inner.right() - 20.0, inner.top()), vec2(20.0, 24.0));
                let resp = ui.interact(br, id.with("open"), Sense::click());
                clip.text(br.center(), Align2::CENTER_CENTER, "↗", body(14.0), if resp.hovered() { pal.accent } else { pal.muted });
                if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Open link").clicked() {
                    let url = if buf.starts_with("http") { buf.clone() } else { format!("https://{buf}") };
                    ctx.open_url(egui::OpenUrl::new_tab(url));
                }
            }
        }
        PropKind::Select | PropKind::MultiSelect => {
            let resp = ui.interact(rect, id, Sense::click());
            let mut x = rect.left() + 8.0;
            let ids: Vec<u64> = match val {
                Some(Val::Sel(o)) => vec![*o],
                Some(Val::Multi(m)) => m.clone(),
                _ => vec![],
            };
            for oid in ids {
                if let Some(o) = db.opt(prop.id, oid) {
                    if x > rect.right() - 20.0 {
                        break;
                    }
                    x += chip_paint(&clip, pos2(x, rect.center().y), &o.name, o.color, pal) + 5.0;
                }
            }
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                open_popup_at(st, Popup::Select { row: row.id, prop: prop.id, pos: rect.left_bottom() + vec2(0.0, 4.0) });
            }
        }
        PropKind::Checkbox => {
            let resp = ui.interact(rect, id, Sense::click());
            let on = matches!(val, Some(Val::Check(true)));
            check_box(ui, pal, pos2(rect.left() + 18.0, rect.center().y), on);
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                acts.push(A::SetVal(row.id, prop.id, Some(Val::Check(!on))));
            }
        }
        PropKind::Date => {
            let resp = ui.interact(rect, id, Sense::click());
            let txt = match val {
                Some(Val::Text(s)) if !s.is_empty() => dates::pretty(s),
                _ => String::new(),
            };
            if txt.is_empty() {
                if resp.hovered() {
                    clip.text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, "Pick a date", body(13.5), with_alpha(pal.muted, 0.7));
                }
            } else {
                clip.text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, txt, body(14.0), pal.text);
            }
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                let (y, m, _) = match val {
                    Some(Val::Text(s)) => dates::parse(s).unwrap_or_else(dates::today),
                    _ => dates::today(),
                };
                open_popup_at(st, Popup::Date { row: row.id, prop: prop.id, pos: rect.left_bottom() + vec2(0.0, 4.0), y, m });
            }
        }
    }
}

// ───────────────────────────── table ─────────────────────────────

#[allow(clippy::too_many_arguments)]
fn table(ui: &mut Ui, ws: &Workspace, db: &Database, rows: &[u64], st: &mut DbState, pal: &Palette, w: f32, acts: &mut Vec<A>) {
    let total: f32 = TITLE_W + db.props.iter().map(|p| col_w(p.kind)).sum::<f32>() + 44.0;
    ScrollArea::horizontal().id_salt("table_scroll").auto_shrink([false, true]).show(ui, |ui| {
        ui.set_min_width(w.max(total.min(w)));
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        // header
        let (hr, _) = ui.allocate_exact_size(vec2(total, 32.0), Sense::hover());
        let mut x = hr.left();
        ui.painter().text(pos2(x + 8.0, hr.center().y), Align2::LEFT_CENTER, "Aa  Name", body(13.0), pal.muted);
        x += TITLE_W;
        for p in &db.props {
            let cw = col_w(p.kind);
            let r = Rect::from_min_size(pos2(x, hr.top()), vec2(cw, 32.0));
            let resp = ui.interact(r, Id::new(("th", p.id)), Sense::click());
            let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
            ui.painter().rect_filled(r, 0.0, with_alpha(pal.hover, t));
            ui.painter().with_clip_rect(r).text(pos2(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, format!("{}  {}", p.kind.glyph(), p.name), body(13.0), lerp_color(pal.muted, pal.text, t));
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                open_popup_at(st, Popup::PropMenu { prop: p.id, pos: r.left_bottom() + vec2(0.0, 4.0) });
            }
            x += cw;
        }
        let ar = Rect::from_min_size(pos2(x, hr.top()), vec2(40.0, 32.0));
        let resp = ui.interact(ar, Id::new("th_add"), Sense::click());
        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
        icons::paint(ui.painter(), ar.center(), 14.0, Icon::Plus, lerp_color(pal.muted, pal.text, t));
        if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Add a property").clicked() {
            open_popup_at(st, Popup::Props(ar.left_bottom() + vec2(-200.0, 4.0)));
        }
        ui.painter().hline(hr.x_range(), hr.bottom(), Stroke::new(1.0_f32, pal.border));

        // rows
        for rid in rows {
            let Some(row) = ws.page(*rid) else { continue };
            let (rr, _) = ui.allocate_exact_size(vec2(total, ROW_H), Sense::hover());
            let hovered = ui.rect_contains_pointer(rr);
            let t = ui.ctx().animate_bool_with_time(Id::new(("rowh", row.id)), hovered, 0.1);
            ui.painter().rect_filled(rr, 0.0, with_alpha(pal.hover, t * 0.6));
            ui.painter().hline(rr.x_range(), rr.bottom(), Stroke::new(1.0_f32, pal.border));
            // title
            let tr = Rect::from_min_size(rr.min, vec2(TITLE_W, ROW_H));
            let tid = Id::new(("rt", row.id));
            if st.focus_row == Some(row.id) {
                ui.ctx().memory_mut(|m| m.request_focus(tid));
                st.focus_row = None;
            }
            let mut title = row.title.clone();
            let ir = Rect::from_center_size(pos2(tr.left() + 8.0 + (TITLE_W - 92.0) / 2.0, tr.center().y), vec2(TITLE_W - 92.0, 24.0));
            let r = ui.put(
                ir,
                TextEdit::singleline(&mut title)
                    .id(tid)
                    .frame(false)
                    .font(bold(14.5))
                    .desired_width(ir.width())
                    .margin(Margin::symmetric(0, 2))
                    .hint_text(RichText::new("Untitled").color(with_alpha(pal.muted, 0.6))),
            );
            if r.changed() {
                acts.push(A::SetTitle(row.id, title));
            }
            if t > 0.05 {
                let open = Rect::from_min_size(pos2(tr.right() - 74.0, tr.center().y - 11.0), vec2(46.0, 22.0));
                let resp = ui.interact(open, Id::new(("ro", row.id)), Sense::click());
                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                ui.painter().rect_filled(open, 5.0, with_alpha(lerp_color(pal.active, pal.border, h), t));
                ui.painter().text(open.center(), Align2::CENTER_CENTER, "OPEN", bold(10.5), with_alpha(pal.text, t));
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    acts.push(A::Open(row.id));
                }
                let del = Rect::from_min_size(pos2(tr.right() - 26.0, tr.center().y - 11.0), vec2(22.0, 22.0));
                let resp = ui.interact(del, Id::new(("rd", row.id)), Sense::click());
                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                icons::paint(ui.painter(), del.center(), 13.0, Icon::Trash, with_alpha(lerp_color(pal.muted, pal.danger, h), t));
                if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Delete row").clicked() {
                    acts.push(A::DelRow(row.id));
                }
            }
            // props
            let mut x = rr.left() + TITLE_W;
            for p in &db.props {
                let cw = col_w(p.kind);
                let cr = Rect::from_min_size(pos2(x, rr.top()), vec2(cw, ROW_H));
                ui.painter().vline(cr.left(), cr.y_range(), Stroke::new(1.0_f32, with_alpha(pal.border, 0.7)));
                cell(ui, db, row, p, cr, pal, st, acts);
                x += cw;
            }
        }
        // new row
        let (nr, resp) = ui.allocate_exact_size(vec2(total.min(w), 32.0), Sense::click());
        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
        ui.painter().rect_filled(nr, 0.0, with_alpha(pal.hover, t));
        icons::paint(ui.painter(), pos2(nr.left() + 16.0, nr.center().y), 13.0, Icon::Plus, lerp_color(pal.muted, pal.text, t));
        ui.painter().text(pos2(nr.left() + 32.0, nr.center().y), Align2::LEFT_CENTER, "New", body(14.0), lerp_color(pal.muted, pal.text, t));
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            acts.push(A::AddRow(None));
        }
        ui.add_space(6.0);
        ui.label(RichText::new(format!("COUNT  {}", rows.len())).font(bold(11.0)).color(pal.muted));
    });
}

// ───────────────────────────── board ─────────────────────────────

fn card_chips(ui: &mut Ui, db: &Database, row: &Page, skip: u64, pal: &Palette) {
    let mut any = false;
    for p in &db.props {
        if p.id == skip {
            continue;
        }
        any |= matches!(row.props.get(&p.id), Some(Val::Sel(_)) | Some(Val::Multi(_))) || matches!(row.props.get(&p.id), Some(Val::Text(s)) if p.kind == PropKind::Date && !s.is_empty());
    }
    if !any {
        return;
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        for p in &db.props {
            if p.id == skip {
                continue;
            }
            match (row.props.get(&p.id), p.kind) {
                (Some(Val::Sel(o)), _) => {
                    if let Some(o) = db.opt(p.id, *o) {
                        chip_ui(ui, pal, &o.name, o.color);
                    }
                }
                (Some(Val::Multi(m)), _) => {
                    for o in m.iter().filter_map(|o| db.opt(p.id, *o)) {
                        chip_ui(ui, pal, &o.name, o.color);
                    }
                }
                (Some(Val::Text(s)), PropKind::Date) if !s.is_empty() => {
                    ui.label(RichText::new(format!("▦ {}", dates::pretty(s))).font(body(12.0)).color(pal.muted));
                }
                _ => {}
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn board(ui: &mut Ui, ws: &Workspace, db: &Database, view: &View, rows: &[u64], st: &mut DbState, pal: &Palette, acts: &mut Vec<A>) {
    let Some(gp) = db.group_prop(view) else {
        ui.add_space(20.0);
        ui.label(RichText::new("A board groups pages by a Select property.").color(pal.muted));
        ui.add_space(6.0);
        if pill(ui, pal, "Add a Status property", false, true).clicked() {
            acts.push(A::AddProp(PropKind::Select));
        }
        return;
    };
    let Some(gprop) = db.prop(gp) else { return };
    let mut cols: Vec<(Option<u64>, String, usize)> = gprop.opts.iter().map(|o| (Some(o.id), o.name.clone(), o.color)).collect();
    cols.push((None, format!("No {}", gprop.name), 0));
    let mut col_rects: Vec<(Rect, Option<u64>)> = vec![];
    let pointer = ui.ctx().pointer_interact_pos();

    ScrollArea::horizontal().id_salt("board_scroll").auto_shrink([false, true]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(12.0, 0.0);
            for (oid, name, color) in &cols {
                let members: Vec<&Page> = rows
                    .iter()
                    .filter_map(|r| ws.page(*r))
                    .filter(|r| {
                        let v = r.props.get(&gp);
                        match oid {
                            Some(o) => matches!(v, Some(Val::Sel(x)) if x == o),
                            None => match v {
                                Some(Val::Sel(x)) => db.opt(gp, *x).is_none(),
                                _ => true,
                            },
                        }
                    })
                    .collect();
                let fr = Frame::new().fill(pal.sidebar).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(8)).show(ui, |ui| {
                    ui.set_width(264.0);
                    ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing = vec2(6.0, 8.0);
                    ui.horizontal(|ui| {
                        if oid.is_some() {
                            chip_ui(ui, pal, name, *color);
                        } else {
                            ui.label(RichText::new(name).font(bold(13.0)).color(pal.muted));
                        }
                        ui.label(RichText::new(members.len().to_string()).font(body(13.0)).color(pal.muted));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icon_button(ui, pal, Icon::Plus, 12.0, "New page in this group").clicked() {
                                acts.push(A::AddRow(oid.map(|o| (gp, Val::Sel(o)))));
                            }
                        });
                    });
                    for row in &members {
                        let dragging_me = st.drag == Some(row.id);
                        let prev = ui.ctx().data(|d| d.get_temp::<bool>(Id::new(("cardh", row.id)))).unwrap_or(false);
                        let t = ui.ctx().animate_bool_with_time(Id::new(("cardt", row.id)), prev, 0.12);
                        let f = Frame::new()
                            .fill(if dragging_me { with_alpha(pal.card, 0.4) } else { pal.card })
                            .stroke(Stroke::new(1.0_f32, lerp_color(pal.border, pal.accent.gamma_multiply(0.7), t * 0.6)))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::symmetric(10, 8))
                            .shadow(egui::Shadow { offset: [0, 2], blur: (2.0 + 8.0 * t) as u8, spread: 0, color: Color32::from_black_alpha((12.0 + 20.0 * t) as u8) })
                            .show(ui, |ui| {
                                ui.set_width(246.0);
                                ui.label(RichText::new(title_of(row)).font(bold(14.5)).color(if row.title.is_empty() { pal.muted } else { pal.text }));
                                card_chips(ui, db, row, gp, pal);
                            });
                        let resp = ui.interact(f.response.rect, Id::new(("card", row.id)), Sense::click_and_drag());
                        ui.ctx().data_mut(|d| d.insert_temp(Id::new(("cardh", row.id)), resp.hovered() || resp.dragged()));
                        if resp.drag_started() {
                            st.drag = Some(row.id);
                        }
                        if resp.hovered() || resp.dragged() {
                            ui.ctx().set_cursor_icon(if resp.dragged() { CursorIcon::Grabbing } else { CursorIcon::PointingHand });
                        }
                        if resp.clicked() {
                            acts.push(A::Open(row.id));
                        }
                    }
                    let (r, resp) = ui.allocate_exact_size(vec2(246.0, 28.0), Sense::click());
                    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                    ui.painter().rect_filled(r, 6.0, with_alpha(pal.active, h * 0.8));
                    icons::paint(ui.painter(), pos2(r.left() + 14.0, r.center().y), 12.0, Icon::Plus, lerp_color(pal.muted, pal.text, h));
                    ui.painter().text(pos2(r.left() + 28.0, r.center().y), Align2::LEFT_CENTER, "New", body(13.5), lerp_color(pal.muted, pal.text, h));
                    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        acts.push(A::AddRow(oid.map(|o| (gp, Val::Sel(o)))));
                    }
                    });
                });
                col_rects.push((fr.response.rect.expand2(vec2(6.0, 4000.0)), *oid));
            }
        });
    });

    // drag & drop between columns
    if let Some(did) = st.drag {
        ui.ctx().request_repaint();
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        let target = pointer.and_then(|p| col_rects.iter().find(|(r, _)| r.x_range().contains(p.x)).map(|(r, o)| (*r, *o)));
        if let Some(p) = pointer {
            if let Some(row) = ws.page(did) {
                egui::Area::new(Id::new("board_ghost")).order(Order::Tooltip).fixed_pos(p + vec2(10.0, 8.0)).interactable(false).show(ui.ctx(), |ui| {
                    Frame::new()
                        .fill(pal.card)
                        .stroke(Stroke::new(1.5_f32, pal.accent))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(12, 8))
                        .shadow(egui::Shadow { offset: [0, 8], blur: 20, spread: 0, color: Color32::from_black_alpha(70) })
                        .show(ui, |ui| {
                            ui.set_max_width(230.0);
                            ui.label(RichText::new(title_of(row)).font(bold(14.0)));
                        });
                });
            }
        }
        if let Some((r, _)) = target {
            let mut hl = Rect::from_min_max(pos2(r.left(), ui.min_rect().top()), pos2(r.right(), ui.min_rect().bottom()));
            hl.set_height(hl.height().min(2000.0));
            ui.painter().rect_stroke(hl.shrink(1.0), 10.0, Stroke::new(1.5_f32, with_alpha(pal.accent, 0.6)), egui::StrokeKind::Inside);
        }
        if !ui.input(|i| i.pointer.primary_down()) {
            if let Some((_, o)) = target {
                let current = ws.page(did).and_then(|r| r.props.get(&gp).cloned());
                let new = o.map(Val::Sel);
                if current != new {
                    acts.push(A::SetVal(did, gp, new));
                }
            }
            st.drag = None;
        }
    }
}

// ───────────────────────────── list / gallery ─────────────────────────────

fn list(ui: &mut Ui, ws: &Workspace, db: &Database, rows: &[u64], pal: &Palette, w: f32, acts: &mut Vec<A>) {
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    for rid in rows {
        let Some(row) = ws.page(*rid) else { continue };
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 36.0), Sense::click());
        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
        ui.painter().rect_filled(rect, 6.0, with_alpha(pal.hover, t));
        let icon = if row.icon.is_empty() { "📄" } else { &row.icon };
        ui.painter().text(pos2(rect.left() + 18.0, rect.center().y), Align2::CENTER_CENTER, icon, body(16.0), pal.muted);
        let g = ui.painter().layout_no_wrap(title_of(row).to_owned(), bold(14.5), if row.title.is_empty() { pal.muted } else { pal.text });
        let tw = g.size().x;
        ui.painter().galley(pos2(rect.left() + 38.0, rect.center().y - g.size().y / 2.0), g, pal.text);
        let mut x = rect.left() + 38.0 + tw + 14.0;
        let clip = ui.painter().with_clip_rect(rect);
        for p in &db.props {
            let ids: Vec<u64> = match row.props.get(&p.id) {
                Some(Val::Sel(o)) => vec![*o],
                Some(Val::Multi(m)) => m.clone(),
                _ => vec![],
            };
            for o in ids.iter().filter_map(|o| db.opt(p.id, *o)) {
                x += chip_paint(&clip, pos2(x, rect.center().y), &o.name, o.color, pal) + 5.0;
            }
            if let (Some(Val::Text(s)), PropKind::Date) = (row.props.get(&p.id), p.kind) {
                if !s.is_empty() {
                    let g = clip.layout_no_wrap(dates::pretty(s), body(12.5), pal.muted);
                    let gw = g.size().x;
                    clip.galley(pos2(x, rect.center().y - g.size().y / 2.0), g, pal.muted);
                    x += gw + 10.0;
                }
            }
        }
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            acts.push(A::Open(row.id));
        }
    }
    new_row_button(ui, pal, acts);
}

fn new_row_button(ui: &mut Ui, pal: &Palette, acts: &mut Vec<A>) {
    ui.add_space(4.0);
    if pill(ui, pal, "+  New", false, false).clicked() {
        acts.push(A::AddRow(None));
    }
}

fn gallery(ui: &mut Ui, ws: &Workspace, db: &Database, rows: &[u64], pal: &Palette, w: f32, acts: &mut Vec<A>) {
    let cw = 236.0;
    ui.horizontal_wrapped(|ui| {
        ui.set_width(w);
        ui.spacing_mut().item_spacing = vec2(14.0, 14.0);
        for rid in rows {
            let Some(row) = ws.page(*rid) else { continue };
            let prev = ui.ctx().data(|d| d.get_temp::<bool>(Id::new(("galh", row.id)))).unwrap_or(false);
            let t = ui.ctx().animate_bool_with_time(Id::new(("galt", row.id)), prev, 0.12);
            let f = Frame::new()
                .fill(pal.card)
                .stroke(Stroke::new(1.0_f32, lerp_color(pal.border, pal.accent.gamma_multiply(0.7), t * 0.6)))
                .corner_radius(CornerRadius::same(10))
                .shadow(egui::Shadow { offset: [0, 2], blur: (3.0 + 8.0 * t) as u8, spread: 0, color: Color32::from_black_alpha((12.0 + 18.0 * t) as u8) })
                .show(ui, |ui| {
                    ui.set_width(cw);
                    ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                    let (cr, _) = ui.allocate_exact_size(vec2(cw, 110.0), Sense::hover());
                    let (a, b) = COVERS[(row.cover.unwrap_or(row.id as usize)) % COVERS.len()];
                    let top_r = CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 };
                    ui.painter().rect_filled(cr, top_r, Color32::from_rgb(a.0, a.1, a.2));
                    gradient_rect(ui, Rect::from_min_max(cr.min + vec2(0.0, 10.0), cr.max), Color32::from_rgb(a.0, a.1, a.2), Color32::from_rgb(b.0, b.1, b.2));
                    if !row.icon.is_empty() {
                        ui.painter().text(cr.center(), Align2::CENTER_CENTER, &row.icon, body(34.0), pal.text);
                    }
                    Frame::new().inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
                        ui.set_width(cw - 24.0);
                        ui.label(RichText::new(title_of(row)).font(bold(14.5)).color(if row.title.is_empty() { pal.muted } else { pal.text }));
                        card_chips(ui, db, row, 0, pal);
                        for p in db.props.iter().filter(|p| matches!(p.kind, PropKind::Text | PropKind::Number)) {
                            if let Some(v) = row.props.get(&p.id) {
                                let s = db.val_text(p.id, v);
                                if !s.is_empty() {
                                    ui.label(RichText::new(format!("{}: {}", p.name, s)).font(body(12.0)).color(pal.muted));
                                }
                            }
                        }
                    });
                    });
                });
            let resp = ui.interact(f.response.rect, Id::new(("gal", row.id)), Sense::click());
            ui.ctx().data_mut(|d| d.insert_temp(Id::new(("galh", row.id)), resp.hovered()));
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                acts.push(A::Open(row.id));
            }
        }
        let (r, resp) = ui.allocate_exact_size(vec2(cw, 90.0), Sense::click());
        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
        ui.painter().rect_stroke(r, 10.0, Stroke::new(1.2_f32, lerp_color(pal.border, pal.accent, t)), egui::StrokeKind::Inside);
        ui.painter().text(r.center(), Align2::CENTER_CENTER, "+  New", body(14.0), lerp_color(pal.muted, pal.accent, t));
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            acts.push(A::AddRow(None));
        }
    });
}

// ───────────────────────────── calendar ─────────────────────────────

#[allow(clippy::too_many_arguments)]
fn calendar(ui: &mut Ui, ws: &Workspace, db: &Database, view: &View, rows: &[u64], st: &mut DbState, pal: &Palette, w: f32, acts: &mut Vec<A>) {
    let Some(dp) = db.date_prop(view) else {
        ui.add_space(20.0);
        ui.label(RichText::new("A calendar needs a Date property.").color(pal.muted));
        ui.add_space(6.0);
        if pill(ui, pal, "Add a Date property", false, true).clicked() {
            acts.push(A::AddProp(PropKind::Date));
        }
        return;
    };
    let (y, m) = st.cal.unwrap_or_else(|| {
        let (y, m, _) = dates::today();
        (y, m)
    });
    ui.horizontal(|ui| {
        if pill(ui, pal, "‹", false, false).clicked() {
            st.cal = Some(if m == 1 { (y - 1, 12) } else { (y, m - 1) });
        }
        if pill(ui, pal, "›", false, false).clicked() {
            st.cal = Some(if m == 12 { (y + 1, 1) } else { (y, m + 1) });
        }
        ui.label(RichText::new(format!("{} {}", dates::MONTHS[m as usize - 1], y)).font(bold(18.0)));
        if pill(ui, pal, "Today", false, false).clicked() {
            let (ty, tm, _) = dates::today();
            st.cal = Some((ty, tm));
        }
    });
    ui.add_space(8.0);
    let cw = (w / 7.0).floor().max(90.0);
    let ch = 118.0;
    let (hr, _) = ui.allocate_exact_size(vec2(cw * 7.0, 24.0), Sense::hover());
    for (i, d) in dates::WEEKDAYS.iter().enumerate() {
        ui.painter().text(pos2(hr.left() + cw * i as f32 + 8.0, hr.center().y), Align2::LEFT_CENTER, *d, bold(12.0), pal.muted);
    }
    let first = dates::weekday(y, m, 1) as i32;
    let dim = dates::days_in_month(y, m) as i32;
    let weeks = ((first + dim + 6) / 7) as usize;
    let (today_y, today_m, today_d) = dates::today();
    let origin = ui.cursor().min;
    let (grid, _) = ui.allocate_exact_size(vec2(cw * 7.0, ch * weeks as f32), Sense::hover());
    let _ = origin;
    for wk in 0..weeks {
        for dow in 0..7 {
            let day = wk as i32 * 7 + dow as i32 - first + 1;
            let cr = Rect::from_min_size(pos2(grid.left() + cw * dow as f32, grid.top() + ch * wk as f32), vec2(cw, ch));
            let in_month = day >= 1 && day <= dim;
            ui.painter().rect_stroke(cr, 0.0, Stroke::new(1.0_f32, pal.border), egui::StrokeKind::Inside);
            if !in_month {
                ui.painter().rect_filled(cr.shrink(1.0), 0.0, with_alpha(pal.sidebar, 0.6));
                continue;
            }
            let day = day as u32;
            let iso = dates::iso(y, m, day);
            let hovered = ui.rect_contains_pointer(cr);
            let is_today = (y, m, day) == (today_y, today_m, today_d);
            let dc = pos2(cr.left() + 16.0, cr.top() + 14.0);
            if is_today {
                ui.painter().circle_filled(dc, 11.0, pal.accent);
            }
            ui.painter().text(dc, Align2::CENTER_CENTER, day.to_string(), body(12.5), if is_today { Color32::WHITE } else { pal.text });
            if hovered {
                let pr = Rect::from_center_size(pos2(cr.right() - 16.0, cr.top() + 14.0), vec2(22.0, 22.0));
                let resp = ui.interact(pr, Id::new(("cal_add", day)), Sense::click());
                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                ui.painter().rect_filled(pr, 5.0, with_alpha(pal.active, h));
                icons::paint(ui.painter(), pr.center(), 12.0, Icon::Plus, lerp_color(pal.muted, pal.text, h));
                if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Add a page on this day").clicked() {
                    acts.push(A::AddRow(Some((dp, Val::Text(iso.clone())))));
                }
            }
            let items: Vec<&Page> = rows.iter().filter_map(|r| ws.page(*r)).filter(|r| matches!(r.props.get(&dp), Some(Val::Text(s)) if *s == iso)).collect();
            let clip = ui.painter().with_clip_rect(cr.shrink(1.0));
            for (k, it) in items.iter().take(3).enumerate() {
                let ir = Rect::from_min_size(pos2(cr.left() + 4.0, cr.top() + 30.0 + k as f32 * 25.0), vec2(cw - 8.0, 22.0));
                let resp = ui.interact(ir, Id::new(("cal_item", it.id)), Sense::click());
                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                let color = db.group_prop(view).and_then(|g| match it.props.get(&g) {
                    Some(Val::Sel(o)) => db.opt(g, *o).map(|o| o.color),
                    _ => None,
                });
                let base = color.map(tag_color).unwrap_or(pal.accent);
                clip.rect_filled(ir, 5.0, base.gamma_multiply(0.18 + 0.12 * h));
                clip.rect_filled(Rect::from_min_size(ir.min, vec2(3.0, ir.height())), 2.0, base);
                clip.with_clip_rect(ir).text(pos2(ir.left() + 8.0, ir.center().y), Align2::LEFT_CENTER, title_of(it), body(12.5), pal.text);
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    acts.push(A::Open(it.id));
                }
            }
            if items.len() > 3 {
                clip.text(pos2(cr.left() + 8.0, cr.top() + 30.0 + 3.0 * 25.0 + 4.0), Align2::LEFT_CENTER, format!("+{} more", items.len() - 3), body(11.5), pal.muted);
            }
        }
    }
    // rows without a date
    let undated: Vec<&Page> = rows.iter().filter_map(|r| ws.page(*r)).filter(|r| !matches!(r.props.get(&dp), Some(Val::Text(s)) if !s.is_empty())).collect();
    if !undated.is_empty() {
        ui.add_space(12.0);
        ui.label(RichText::new(format!("NO DATE  ·  {}", undated.len())).font(bold(11.0)).color(pal.muted));
        ui.horizontal_wrapped(|ui| {
            for r in undated {
                if pill(ui, pal, title_of(r), false, false).clicked() {
                    acts.push(A::Open(r.id));
                }
            }
        });
    }
}

// ───────────────────────────── popups ─────────────────────────────

fn rule(ui: &mut Ui, pal: &Palette) {
    ui.add_space(3.0);
    ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0_f32, pal.border));
    ui.add_space(4.0);
}

fn prop_choices(db: &Database) -> Vec<(u64, String, PropKind)> {
    let mut v = vec![(0, "Name".to_owned(), PropKind::Text)];
    v.extend(db.props.iter().map(|p| (p.id, p.name.clone(), p.kind)));
    v
}

fn popups(ui: &mut Ui, ws: &Workspace, db: &Database, vi: usize, st: &mut DbState, pal: &Palette, acts: &mut Vec<A>) {
    let Some(pop) = st.popup.clone() else { return };
    let ctx = ui.ctx().clone();
    let pid = Id::new("db_popup");
    let mut next: Option<Option<Popup>> = None;
    let view = &db.views[vi.min(db.views.len() - 1)];
    let vi = vi.min(db.views.len() - 1);
    let open = match pop {
        Popup::Filter(pos) => popup_area(&ctx, pid, pos, pal, &mut st.fresh, 440.0, |ui| {
            section_label(ui, pal, "FILTER");
            if view.filters.is_empty() {
                ui.label(RichText::new("   No filters yet — show only the pages that match.").font(body(13.0)).color(pal.muted));
            }
            let choices = prop_choices(db);
            for (fi, f) in view.filters.iter().enumerate() {
                let kind = db.kind_of(f.prop);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
                    let pname = choices.iter().find(|c| c.0 == f.prop).map(|c| c.1.clone()).unwrap_or_else(|| "Name".into());
                    if field_btn(ui, pal, &pname, 112.0).clicked() {
                        let pos = choices.iter().position(|c| c.0 == f.prop).unwrap_or(0);
                        let nx = &choices[(pos + 1) % choices.len()];
                        let op = FilterOp::for_kind(nx.2)[0];
                        acts.push(A::SetFilter(vi, fi, Filter { prop: nx.0, op, value: String::new() }));
                    }
                    if field_btn(ui, pal, f.op.label(kind), 104.0).clicked() {
                        let ops = FilterOp::for_kind(kind);
                        let pos = ops.iter().position(|o| *o == f.op).unwrap_or(0);
                        acts.push(A::SetFilter(vi, fi, Filter { op: ops[(pos + 1) % ops.len()], ..f.clone() }));
                    }
                    if f.op.needs_value() {
                        if matches!(kind, PropKind::Select | PropKind::MultiSelect) {
                            let opts = db.prop(f.prop).map(|p| p.opts.clone()).unwrap_or_default();
                            let label = if f.value.is_empty() { "Choose…".to_owned() } else { f.value.clone() };
                            if field_btn(ui, pal, &label, 130.0).clicked() && !opts.is_empty() {
                                let pos = opts.iter().position(|o| o.name == f.value).map(|p| p + 1).unwrap_or(0);
                                let name = opts[pos % opts.len()].name.clone();
                                acts.push(A::SetFilter(vi, fi, Filter { value: name, ..f.clone() }));
                            }
                        } else {
                            let mut v = f.value.clone();
                            let hint = if kind == PropKind::Date { "YYYY-MM-DD" } else { "Value" };
                            let r = field(ui, pal, pal.bg, &mut v, hint, false, 130.0, Id::new(("fv", vi, fi)));
                            if r.changed() {
                                acts.push(A::SetFilter(vi, fi, Filter { value: v, ..f.clone() }));
                            }
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, pal, Icon::Close, 11.0, "Remove filter").clicked() {
                            acts.push(A::DelFilter(vi, fi));
                        }
                    });
                });
                ui.add_space(4.0);
            }
            if menu_item(ui, pal, "+", "Add a filter", None, false, false).clicked() {
                acts.push(A::AddFilter(vi));
            }
        }),
        Popup::Sort(pos) => popup_area(&ctx, pid, pos, pal, &mut st.fresh, 340.0, |ui| {
            section_label(ui, pal, "SORT");
            if view.sorts.is_empty() {
                ui.label(RichText::new("   No sorts yet — order the pages any way you like.").font(body(13.0)).color(pal.muted));
            }
            let choices = prop_choices(db);
            for (si, s) in view.sorts.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
                    let pname = choices.iter().find(|c| c.0 == s.prop).map(|c| c.1.clone()).unwrap_or_else(|| "Name".into());
                    if field_btn(ui, pal, &pname, 140.0).clicked() {
                        let pos = choices.iter().position(|c| c.0 == s.prop).unwrap_or(0);
                        acts.push(A::SetSort(vi, si, Sort { prop: choices[(pos + 1) % choices.len()].0, asc: s.asc }));
                    }
                    if field_btn(ui, pal, if s.asc { "↑ Ascending" } else { "↓ Descending" }, 110.0).clicked() {
                        acts.push(A::SetSort(vi, si, Sort { prop: s.prop, asc: !s.asc }));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, pal, Icon::Close, 11.0, "Remove sort").clicked() {
                            acts.push(A::DelSort(vi, si));
                        }
                    });
                });
                ui.add_space(4.0);
            }
            if menu_item(ui, pal, "+", "Add a sort", None, false, false).clicked() {
                acts.push(A::AddSort(vi));
            }
        }),
        Popup::Props(pos) => popup_area(&ctx, pid, pos, pal, &mut st.fresh, 280.0, |ui| {
            section_label(ui, pal, "PROPERTIES");
            for p in &db.props {
                if menu_item(ui, pal, p.kind.glyph(), &p.name, None, false, false).clicked() {
                    next = Some(Some(Popup::PropMenu { prop: p.id, pos }));
                }
            }
            rule(ui, pal);
            section_label(ui, pal, "ADD A PROPERTY");
            for k in PropKind::ALL {
                if menu_item(ui, pal, k.glyph(), k.label(), None, false, false).clicked() {
                    acts.push(A::AddProp(k));
                }
            }
        }),
        Popup::PropMenu { prop, pos } => {
            let Some(p) = db.prop(prop) else {
                st.popup = None;
                return;
            };
            popup_area(&ctx, pid, pos, pal, &mut st.fresh, 270.0, |ui| {
                let mut name = p.name.clone();
                let r = field(ui, pal, pal.bg, &mut name, "Property name", false, 250.0, Id::new(("pn", prop)));
                if r.changed() {
                    acts.push(A::RenameProp(prop, name));
                }
                ui.add_space(4.0);
                section_label(ui, pal, "TYPE");
                for k in PropKind::ALL {
                    if menu_item(ui, pal, k.glyph(), k.label(), None, k == p.kind, false).clicked() && k != p.kind {
                        acts.push(A::SetPropKind(prop, k));
                    }
                }
                if matches!(p.kind, PropKind::Select | PropKind::MultiSelect) {
                    rule(ui, pal);
                    section_label(ui, pal, "OPTIONS");
                    for o in &p.opts {
                        ui.horizontal(|ui| {
                            let (r, resp) = ui.allocate_exact_size(vec2(22.0, 26.0), Sense::click());
                            ui.painter().circle_filled(r.center(), 6.5, tag_color(o.color));
                            if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(format!("{} — click to change colour", TAG_NAMES[o.color % 9])).clicked() {
                                acts.push(A::RecolorOpt(prop, o.id));
                            }
                            let mut n = o.name.clone();
                            let r = ui.add(TextEdit::singleline(&mut n).frame(false).desired_width(170.0).font(body(14.0)).id(Id::new(("on", o.id))));
                            if r.changed() {
                                acts.push(A::RenameOpt(prop, o.id, n));
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, pal, Icon::Close, 10.0, "Delete option").clicked() {
                                    acts.push(A::DelOpt(prop, o.id));
                                }
                            });
                        });
                    }
                    let r = field(ui, pal, pal.bg, &mut st.new_opt, "Add an option…", false, 250.0, Id::new(("no", prop)));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !st.new_opt.trim().is_empty() {
                        acts.push(A::AddOpt(prop, std::mem::take(&mut st.new_opt), None));
                        ui.ctx().memory_mut(|m| m.request_focus(Id::new(("no", prop))));
                    }
                }
                rule(ui, pal);
                if menu_item(ui, pal, "🗑", "Delete property", None, false, true).clicked() {
                    acts.push(A::DelProp(prop));
                    next = Some(None);
                }
            })
        }
        Popup::ViewMenu(pos) => popup_area(&ctx, pid, pos, pal, &mut st.fresh, 270.0, |ui| {
            let mut name = view.name.clone();
            let r = field(ui, pal, pal.bg, &mut name, "View name", false, 250.0, Id::new(("vn", view.id)));
            if r.changed() {
                acts.push(A::RenameView(vi, name));
            }
            ui.add_space(4.0);
            section_label(ui, pal, "LAYOUT");
            for k in ViewKind::ALL {
                if menu_item(ui, pal, k.glyph(), k.label(), None, k == view.kind, false).clicked() && k != view.kind {
                    acts.push(A::SetViewKind(vi, k));
                }
            }
            if view.kind == ViewKind::Board {
                let sels: Vec<&Prop> = db.props.iter().filter(|p| p.kind == PropKind::Select).collect();
                if !sels.is_empty() {
                    rule(ui, pal);
                    let cur = db.group_prop(view).unwrap_or(0);
                    let cname = db.prop(cur).map(|p| p.name.clone()).unwrap_or_default();
                    if menu_item(ui, pal, "▥", &format!("Group by: {cname}"), Some("Click to switch property"), false, false).clicked() {
                        let pos = sels.iter().position(|p| p.id == cur).unwrap_or(0);
                        acts.push(A::SetGroup(vi, sels[(pos + 1) % sels.len()].id));
                    }
                }
            }
            if view.kind == ViewKind::Calendar {
                let ds: Vec<&Prop> = db.props.iter().filter(|p| p.kind == PropKind::Date).collect();
                if !ds.is_empty() {
                    rule(ui, pal);
                    let cur = db.date_prop(view).unwrap_or(0);
                    let cname = db.prop(cur).map(|p| p.name.clone()).unwrap_or_default();
                    if menu_item(ui, pal, "▦", &format!("Show by: {cname}"), Some("Click to switch property"), false, false).clicked() {
                        let pos = ds.iter().position(|p| p.id == cur).unwrap_or(0);
                        acts.push(A::SetDateProp(vi, ds[(pos + 1) % ds.len()].id));
                    }
                }
            }
            if db.views.len() > 1 {
                rule(ui, pal);
                if menu_item(ui, pal, "🗑", "Delete view", None, false, true).clicked() {
                    acts.push(A::DelView(vi));
                    next = Some(None);
                }
            }
        }),
        Popup::AddView(pos) => popup_area(&ctx, pid, pos, pal, &mut st.fresh, 230.0, |ui| {
            section_label(ui, pal, "ADD A VIEW");
            for k in ViewKind::ALL {
                if menu_item(ui, pal, k.glyph(), k.label(), None, false, false).clicked() {
                    acts.push(A::AddView(k));
                    next = Some(None);
                }
            }
        }),
        Popup::Select { row, prop, pos } => {
            let Some(p) = db.prop(prop) else {
                st.popup = None;
                return;
            };
            let cur: Vec<u64> = match ws.page(row).and_then(|r| r.props.get(&prop)) {
                Some(Val::Sel(o)) => vec![*o],
                Some(Val::Multi(m)) => m.clone(),
                _ => vec![],
            };
            let multi = p.kind == PropKind::MultiSelect;
            popup_area(&ctx, pid, pos, pal, &mut st.fresh, 250.0, |ui| {
                let r = field(ui, pal, pal.bg, &mut st.new_opt, "Search or create…", false, 230.0, Id::new(("sel_q", prop)));
                if !r.has_focus() && st.new_opt.is_empty() {
                    r.request_focus();
                }
                let q = st.new_opt.trim().to_lowercase();
                ui.add_space(4.0);
                let mut exact = false;
                ScrollArea::vertical().max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
                    for o in p.opts.iter().filter(|o| q.is_empty() || o.name.to_lowercase().contains(&q)) {
                        exact |= o.name.to_lowercase() == q;
                        let (rr, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
                        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                        ui.painter().rect_filled(rr, 6.0, with_alpha(pal.hover, t));
                        chip_paint(ui.painter(), pos2(rr.left() + 8.0, rr.center().y), &o.name, o.color, pal);
                        if cur.contains(&o.id) {
                            ui.painter().text(pos2(rr.right() - 14.0, rr.center().y), Align2::CENTER_CENTER, "✓", bold(13.0), pal.accent);
                        }
                        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            if multi {
                                let mut m = cur.clone();
                                if let Some(i) = m.iter().position(|x| *x == o.id) {
                                    m.remove(i);
                                } else {
                                    m.push(o.id);
                                }
                                acts.push(A::SetVal(row, prop, if m.is_empty() { None } else { Some(Val::Multi(m)) }));
                            } else {
                                acts.push(A::SetVal(row, prop, Some(Val::Sel(o.id))));
                                next = Some(None);
                            }
                        }
                    }
                });
                if !q.is_empty() && !exact {
                    let label = format!("Create “{}”", st.new_opt.trim());
                    let enter = ui.input(|i| i.key_pressed(Key::Enter));
                    if menu_item(ui, pal, "+", &label, None, false, false).clicked() || enter {
                        acts.push(A::AddOpt(prop, st.new_opt.trim().to_owned(), Some(row)));
                        st.new_opt.clear();
                        if !multi {
                            next = Some(None);
                        }
                    }
                }
                if !cur.is_empty() {
                    rule(ui, pal);
                    if menu_item(ui, pal, "×", "Clear", None, false, false).clicked() {
                        acts.push(A::SetVal(row, prop, None));
                        next = Some(None);
                    }
                }
            })
        }
        Popup::Date { row, prop, pos, y, m } => {
            let cur = match ws.page(row).and_then(|r| r.props.get(&prop)) {
                Some(Val::Text(s)) => dates::parse(s),
                _ => None,
            };
            popup_area(&ctx, pid, pos, pal, &mut st.fresh, 260.0, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    ui.label(RichText::new(format!("{} {}", dates::MONTHS[m as usize - 1], y)).font(bold(14.0)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, pal, Icon::ChevronRight, 13.0, "Next month").clicked() {
                            let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
                            next = Some(Some(Popup::Date { row, prop, pos, y: ny, m: nm }));
                        }
                        if icon_button(ui, pal, Icon::ChevronLeft, 13.0, "Previous month").clicked() {
                            let (ny, nm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
                            next = Some(Some(Popup::Date { row, prop, pos, y: ny, m: nm }));
                        }
                    });
                });
                let cw = 36.0;
                let first = dates::weekday(y, m, 1);
                let dim = dates::days_in_month(y, m);
                let (grid, _) = ui.allocate_exact_size(vec2(cw * 7.0, 20.0 + 32.0 * 6.0), Sense::hover());
                for (i, d) in dates::WEEKDAYS.iter().enumerate() {
                    ui.painter().text(pos2(grid.left() + cw * i as f32 + cw / 2.0, grid.top() + 10.0), Align2::CENTER_CENTER, &d[..2], body(11.5), pal.muted);
                }
                let today = dates::today();
                for day in 1..=dim {
                    let idx = first + day - 1;
                    let (col, rw) = (idx % 7, idx / 7);
                    let r = Rect::from_center_size(pos2(grid.left() + cw * col as f32 + cw / 2.0, grid.top() + 20.0 + 32.0 * rw as f32 + 16.0), vec2(30.0, 28.0));
                    let resp = ui.interact(r, Id::new(("dp", day)), Sense::click());
                    let sel = cur == Some((y, m, day));
                    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.08);
                    if sel {
                        ui.painter().rect_filled(r, 14.0, pal.accent);
                    } else {
                        ui.painter().rect_filled(r, 14.0, with_alpha(pal.hover, t));
                        if today == (y, m, day) {
                            ui.painter().rect_stroke(r, 14.0, Stroke::new(1.3_f32, pal.accent), egui::StrokeKind::Inside);
                        }
                    }
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, day.to_string(), body(13.0), if sel { Color32::WHITE } else { pal.text });
                    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        acts.push(A::SetVal(row, prop, Some(Val::Text(dates::iso(y, m, day)))));
                        next = Some(None);
                    }
                }
                rule(ui, pal);
                ui.horizontal(|ui| {
                    if pill(ui, pal, "Today", false, false).clicked() {
                        acts.push(A::SetVal(row, prop, Some(Val::Text(dates::today_iso()))));
                        next = Some(None);
                    }
                    if cur.is_some() && pill(ui, pal, "Clear", false, false).clicked() {
                        acts.push(A::SetVal(row, prop, None));
                        next = Some(None);
                    }
                });
            })
        }
    };
    match next {
        Some(n) => {
            if n.is_some() {
                st.fresh = true;
            }
            st.popup = n;
        }
        None => {
            if !open {
                st.popup = None;
                st.new_opt.clear();
            }
        }
    }
}

// ───────────────────────────── apply ─────────────────────────────

fn apply(ws: &mut Workspace, db_id: u64, a: A, ed: &mut EditorState) {
    match a {
        A::SetVal(row, prop, v) => {
            if let Some(p) = ws.page_mut(row) {
                match v {
                    Some(v) => {
                        p.props.insert(prop, v);
                    }
                    None => {
                        p.props.remove(&prop);
                    }
                }
                p.updated = now_ms();
            }
        }
        A::SetTitle(row, t) => {
            if let Some(p) = ws.page_mut(row) {
                p.title = t;
                p.updated = now_ms();
            }
        }
        A::AddRow(preset) => {
            let r = ws.add_row(db_id, preset);
            ed.db.focus_row = Some(r);
        }
        A::DelRow(r) => ws.set_trashed(r, true),
        A::Open(r) => ed.nav = Some(r),
        A::SetActive(i) => {
            if let Some(d) = ws.db_mut(db_id) {
                d.active = i.min(d.views.len().saturating_sub(1));
            }
        }
        A::AddView(k) => {
            ws.add_view(db_id, k);
        }
        A::DelView(i) => {
            if let Some(d) = ws.db_mut(db_id) {
                if d.views.len() > 1 && i < d.views.len() {
                    d.views.remove(i);
                    d.active = d.active.min(d.views.len() - 1);
                }
            }
        }
        A::RenameView(i, n) => {
            if let Some(v) = ws.db_mut(db_id).and_then(|d| d.views.get_mut(i)) {
                v.name = n;
            }
        }
        A::SetViewKind(i, k) => {
            if let Some(v) = ws.db_mut(db_id).and_then(|d| d.views.get_mut(i)) {
                v.kind = k;
            }
        }
        A::SetGroup(i, p) => {
            if let Some(v) = ws.db_mut(db_id).and_then(|d| d.views.get_mut(i)) {
                v.group = p;
            }
        }
        A::SetDateProp(i, p) => {
            if let Some(v) = ws.db_mut(db_id).and_then(|d| d.views.get_mut(i)) {
                v.date = p;
            }
        }
        A::AddFilter(i) => {
            if let Some(v) = ws.db_mut(db_id).and_then(|d| d.views.get_mut(i)) {
                v.filters.push(Filter { prop: 0, op: FilterOp::Contains, value: String::new() });
            }
        }
        A::SetFilter(i, f, nf) => {
            if let Some(x) = ws.db_mut(db_id).and_then(|d| d.views.get_mut(i)).and_then(|v| v.filters.get_mut(f)) {
                *x = nf;
            }
        }
        A::DelFilter(i, f) => {
            if let Some(v) = ws.db_mut(db_id).and_then(|d| d.views.get_mut(i)) {
                if f < v.filters.len() {
                    v.filters.remove(f);
                }
            }
        }
        A::AddSort(i) => {
            if let Some(v) = ws.db_mut(db_id).and_then(|d| d.views.get_mut(i)) {
                v.sorts.push(Sort { prop: 0, asc: true });
            }
        }
        A::SetSort(i, s, ns) => {
            if let Some(x) = ws.db_mut(db_id).and_then(|d| d.views.get_mut(i)).and_then(|v| v.sorts.get_mut(s)) {
                *x = ns;
            }
        }
        A::DelSort(i, s) => {
            if let Some(v) = ws.db_mut(db_id).and_then(|d| d.views.get_mut(i)) {
                if s < v.sorts.len() {
                    v.sorts.remove(s);
                }
            }
        }
        A::AddProp(k) => {
            let n = ws.db(db_id).map(|d| d.props.iter().filter(|p| p.kind == k).count()).unwrap_or(0);
            let name = match (k, n) {
                (PropKind::Select, 0) => "Status".to_owned(),
                (PropKind::Date, 0) => "Date".to_owned(),
                (_, 0) => k.label().to_owned(),
                (_, n) => format!("{} {}", k.label(), n + 1),
            };
            let opts: &[&str] = if matches!(k, PropKind::Select | PropKind::MultiSelect) { &["Option 1", "Option 2"] } else { &[] };
            let pid = ws.add_prop(db_id, &name, k, opts);
            ed.db.popup = Some(Popup::PropMenu { prop: pid, pos: pos2(300.0, 160.0) });
            ed.db.fresh = true;
        }
        A::RenameProp(p, n) => {
            if let Some(x) = ws.db_mut(db_id).and_then(|d| d.props.iter_mut().find(|x| x.id == p)) {
                x.name = n;
            }
        }
        A::SetPropKind(p, k) => {
            let from = ws.db(db_id).and_then(|d| d.prop(p)).map(|x| x.kind);
            let Some(from) = from else { return };
            if let Some(x) = ws.db_mut(db_id).and_then(|d| d.props.iter_mut().find(|x| x.id == p)) {
                x.kind = k;
                if matches!(k, PropKind::Select | PropKind::MultiSelect) && x.opts.is_empty() {
                    x.opts.clear();
                }
            }
            for r in ws.pages.iter_mut().filter(|r| r.row && r.parent == Some(db_id)) {
                let Some(v) = r.props.remove(&p) else { continue };
                let nv = match (&v, from, k) {
                    (Val::Sel(o), PropKind::Select, PropKind::MultiSelect) => Some(Val::Multi(vec![*o])),
                    (Val::Multi(m), PropKind::MultiSelect, PropKind::Select) => m.first().map(|o| Val::Sel(*o)),
                    (Val::Text(s), PropKind::Text | PropKind::Url, PropKind::Text | PropKind::Url) => Some(Val::Text(s.clone())),
                    (Val::Text(s), PropKind::Text | PropKind::Url, PropKind::Date) => dates::parse(s).map(|_| Val::Text(s.clone())),
                    (Val::Text(s), PropKind::Text, PropKind::Number) => s.trim().parse::<f64>().ok().map(Val::Num),
                    (Val::Num(n), PropKind::Number, PropKind::Text) => Some(Val::Text(format!("{n}"))),
                    (Val::Text(s), PropKind::Date, PropKind::Text | PropKind::Url) => Some(Val::Text(s.clone())),
                    _ => None,
                };
                if let Some(nv) = nv {
                    r.props.insert(p, nv);
                }
            }
            if let Some(d) = ws.db_mut(db_id) {
                for v in d.views.iter_mut() {
                    v.filters.retain(|f| f.prop != p);
                }
            }
        }
        A::DelProp(p) => {
            if let Some(d) = ws.db_mut(db_id) {
                d.props.retain(|x| x.id != p);
                for v in d.views.iter_mut() {
                    v.filters.retain(|f| f.prop != p);
                    v.sorts.retain(|s| s.prop != p);
                    if v.group == p {
                        v.group = 0;
                    }
                    if v.date == p {
                        v.date = 0;
                    }
                }
            }
            for r in ws.pages.iter_mut().filter(|r| r.row && r.parent == Some(db_id)) {
                r.props.remove(&p);
            }
        }
        A::AddOpt(p, name, set_on) => {
            let oid = ws.id();
            let mut kind = PropKind::Select;
            if let Some(x) = ws.db_mut(db_id).and_then(|d| d.props.iter_mut().find(|x| x.id == p)) {
                kind = x.kind;
                let color = x.opts.len() % 9;
                x.opts.push(Opt { id: oid, name, color });
            }
            if let Some(row) = set_on {
                if let Some(r) = ws.page_mut(row) {
                    if kind == PropKind::MultiSelect {
                        let mut m = match r.props.get(&p) {
                            Some(Val::Multi(m)) => m.clone(),
                            _ => vec![],
                        };
                        m.push(oid);
                        r.props.insert(p, Val::Multi(m));
                    } else {
                        r.props.insert(p, Val::Sel(oid));
                    }
                }
            }
        }
        A::RenameOpt(p, o, n) => {
            if let Some(x) = ws.db_mut(db_id).and_then(|d| d.props.iter_mut().find(|x| x.id == p)).and_then(|x| x.opts.iter_mut().find(|x| x.id == o)) {
                x.name = n;
            }
        }
        A::RecolorOpt(p, o) => {
            if let Some(x) = ws.db_mut(db_id).and_then(|d| d.props.iter_mut().find(|x| x.id == p)).and_then(|x| x.opts.iter_mut().find(|x| x.id == o)) {
                x.color = (x.color + 1) % 9;
            }
        }
        A::DelOpt(p, o) => {
            if let Some(x) = ws.db_mut(db_id).and_then(|d| d.props.iter_mut().find(|x| x.id == p)) {
                x.opts.retain(|x| x.id != o);
            }
            for r in ws.pages.iter_mut().filter(|r| r.row && r.parent == Some(db_id)) {
                match r.props.get_mut(&p) {
                    Some(Val::Sel(x)) if *x == o => {
                        r.props.remove(&p);
                    }
                    Some(Val::Multi(m)) => m.retain(|x| *x != o),
                    _ => {}
                }
            }
        }
    }
}

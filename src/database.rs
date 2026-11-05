//! Notion-style databases: table, board, list, gallery and calendar views with filters, sorts and typed properties.
//!
//! Rendering is read-only over the workspace; every edit is recorded as an [`A`]ction and applied afterwards.

use crate::lucide as l;
use crate::widgets::{context_menu, right_clicked, CtxItem};
use egui::{pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Frame, Id, Key, Layout, Margin, Order, Painter, Pos2, Rect, Response, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui};

use crate::auth::now_ms;
use crate::dates;
use crate::editor::{EditorState, GUTTER};
use crate::icons::{self, Icon};
use crate::model::*;
use crate::platform;
use crate::theme::{bold, body, lerp_color, tag_color, Palette, COVERS, TAG_NAMES};
use crate::widgets::{field, gradient_rect, icon_button, menu_item, popup_area, section_label, with_alpha};

const ROW_H: f32 = 34.0;
const TITLE_W: f32 = 280.0;

#[derive(Clone)]
enum Popup {
    Select { row: u64, prop: u64, pos: Pos2 },
    Date { row: u64, prop: u64, pos: Pos2, y: i32, m: u32 },
    Relation { row: u64, prop: u64, pos: Pos2 },
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
    rmenu: Option<(u64, Pos2)>,
    rmenu_fresh: bool,
    date_range: bool,
}

thread_local! {
    /// Titles of all live database rows (for relation chips) — refreshed at the start of every database render.
    static TITLES: std::cell::RefCell<std::collections::HashMap<u64, String>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

fn load_titles(ws: &Workspace) {
    TITLES.with(|t| {
        let mut t = t.borrow_mut();
        t.clear();
        for p in ws.pages.iter().filter(|p| p.row && !p.trashed) {
            t.insert(p.id, if p.title.is_empty() { "Untitled".to_owned() } else { p.title.clone() });
        }
    });
}

fn rel_title(id: u64) -> Option<String> {
    TITLES.with(|t| t.borrow().get(&id).cloned())
}

fn is_date_text(s: &str) -> bool {
    (s.len() == 10 || s.len() == 21) && dates::parse(s).is_some()
}

pub(crate) enum A {
    RowMenu(u64, Pos2),
    DupRow(u64),
    SetVal(u64, u64, Option<Val>),
    SetTitle(u64, String),
    AddRow(Option<(u64, Val)>),
    DelRow(u64),
    Open(u64),
    SetActive(usize),
    /// View index, property id (0 = name column), width (0 = reset).
    SetColW(usize, u64, f32),
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
    SetFormula(u64, String),
    SetRelation(u64, u64),
    RollupRel(u64, u64),
    RollupProp(u64, u64),
    RollupFn(u64, RollupFn),
    SetDep(usize, u64),
    SetZoom(usize, u8),
    /// (row that depends, row it depends on) — toggles the link.
    AddDep(u64, u64),
    AddDepProp,
    Reschedule,
    /// Pin the view (index) of this database to the dashboard as a widget kind.
    Pin(usize, &'static str),
    /// Pin a chart / number of one property.
    PinProp(u64, &'static str),
}

/// True when `row` (transitively) depends on `target` through the dependency property `dp`. Iterative and cycle-safe.
fn depends_on(ws: &Workspace, dp: u64, row: u64, target: u64) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![row];
    while let Some(r) = stack.pop() {
        if r == target {
            return true;
        }
        if !seen.insert(r) {
            continue;
        }
        if let Some(Val::Multi(preds)) = ws.page(r).and_then(|p| p.props.get(&dp)) {
            stack.extend(preds.iter().copied());
        }
    }
    false
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
        PropKind::Formula => 170.0,
        PropKind::Relation => 210.0,
        PropKind::Rollup => 160.0,
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

/// Like `chip_paint`, but the label is cut with an ellipsis so the chip never grows wider than `max_w`.
fn chip_paint_fit(p: &Painter, left_center: Pos2, text: &str, color: usize, pal: &Palette, max_w: f32) -> f32 {
    let col = tag_color(color);
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), body(12.5), lerp_color(col, pal.text, 0.25));
    job.wrap = egui::text::TextWrapping::truncate_at_width((max_w - 14.0).max(8.0));
    let g = p.layout_job(job);
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

pub(crate) fn pill(ui: &mut Ui, pal: &Palette, label: &str, active: bool, accent: bool) -> Response {
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
    crate::widgets::text_fit(ui.painter(), pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, label, body(13.5), pal.text, (rect.width() - 18.0).max(10.0));
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

pub(crate) fn title_of(r: &Page) -> &str {
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
    load_titles(ws);
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
            ViewKind::Table => table(ui, wsr, &db, vi, &rows, st, pal, w, &mut acts),
            ViewKind::Board => board(ui, wsr, &db, view, &rows, st, pal, &mut acts),
            ViewKind::List => list(ui, wsr, &db, &rows, pal, w, &mut acts),
            ViewKind::Gallery => gallery(ui, wsr, &db, &rows, pal, w, &mut acts),
            ViewKind::Calendar => {
                ScrollArea::horizontal().id_salt("cal_scroll").auto_shrink([false, true]).show(ui, |ui| calendar(ui, wsr, &db, view, &rows, st, pal, w, &mut acts));
            }
            ViewKind::Timeline => crate::timeline::timeline(ui, wsr, id, &db, vi, &rows, pal, w, &mut acts),
        }
        popups(ui, wsr, id, &db, vi, st, pal, &mut acts);
        row_menu(ui, wsr, st, pal, &mut acts);
    }
    let changed = acts.iter().any(|a| !matches!(a, A::RowMenu(..)));
    for a in acts {
        apply(ws, id, a, ed);
    }
    changed
}

/// Right-click menu for a database row (works in every view).
fn row_menu(ui: &mut Ui, ws: &Workspace, st: &mut DbState, pal: &Palette, acts: &mut Vec<A>) {
    let Some((rid, pos)) = st.rmenu else { return };
    let Some(row) = ws.page(rid) else {
        st.rmenu = None;
        return;
    };
    let title = row.title.clone();
    let items = [
        CtxItem::new(l::FILE_TEXT, "Open as page"),
        CtxItem::new(l::COPY_PLUS, "Duplicate"),
        CtxItem::new(l::COPY, "Copy title").enabled(!title.is_empty()),
        CtxItem::new(l::TRASH_2, "Delete").danger().sep(),
    ];
    let (open, pick) = context_menu(ui.ctx(), Id::new("db_row_menu"), pos, pal, &mut st.rmenu_fresh, &items);
    match pick {
        Some(0) => acts.push(A::Open(rid)),
        Some(1) => acts.push(A::DupRow(rid)),
        Some(2) => ui.ctx().copy_text(title),
        Some(3) => acts.push(A::DelRow(rid)),
        _ => {}
    }
    if !open {
        st.rmenu = None;
    }
}

/// Property panel shown under the title of a database row page.
pub fn row_props(ui: &mut Ui, ws: &mut Workspace, row_id: u64, ed: &mut EditorState, pal: &Palette, w: f32) -> bool {
    let Some(db_id) = ws.page(row_id).and_then(|p| p.parent) else { return false };
    let Some(db) = ws.db(db_id).cloned() else { return false };
    let mut acts: Vec<A> = vec![];
    load_titles(ws);
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
        popups(ui, wsr, db_id, &db, db.active, st, pal, &mut acts);
    }
    let changed = !acts.is_empty();
    for a in acts {
        apply(ws, db_id, a, ed);
    }
    changed
}

// ───────────────────────────── toolbar ─────────────────────────────

fn views_ui(ui: &mut Ui, db: &Database, vi: usize, st: &mut DbState, pal: &Palette, acts: &mut Vec<A>) {
    ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
    for (i, v) in db.views.iter().enumerate() {
        let r = pill(ui, pal, &format!("{}  {}", v.kind.glyph(), v.name), i == vi, false);
        if let Some(p) = right_clicked(ui, r.rect) {
            if i != vi {
                acts.push(A::SetActive(i));
            }
            st.popup = Some(Popup::ViewMenu(p));
            st.fresh = true;
        }
        if r.clicked() {
            if i == vi {
                st.popup = Some(Popup::ViewMenu(r.rect.left_bottom() + vec2(0.0, 6.0)));
                st.fresh = true;
            } else {
                acts.push(A::SetActive(i));
            }
        }
    }
    let r = pill(ui, pal, l::PLUS, false, false).on_hover_text("Add a view");
    if r.clicked() {
        st.popup = Some(Popup::AddView(r.rect.left_bottom() + vec2(0.0, 6.0)));
        st.fresh = true;
    }
}

/// Filter / Sort / Properties / New (right to left). `short` = icons only (phones).
fn tools_ui(ui: &mut Ui, view: &View, st: &mut DbState, pal: &Palette, acts: &mut Vec<A>, short: bool) {
    ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
    let new_t = if short { l::PLUS.to_string() } else { format!("{}  New", l::PLUS) };
    if pill(ui, pal, &new_t, false, true).clicked() {
        acts.push(A::AddRow(None));
    }
    ui.add_space(6.0);
    let pt = if short { l::SLIDERS_HORIZONTAL.to_string() } else { format!("{}  Properties", l::SLIDERS_HORIZONTAL) };
    let r = pill(ui, pal, &pt, false, false);
    if r.clicked() {
        st.popup = Some(Popup::Props(pos2(r.rect.right() - 280.0, r.rect.bottom() + 6.0)));
        st.fresh = true;
    }
    let cnt = |n: usize| if n == 0 { String::new() } else { format!(" · {n}") };
    let fl = if short { format!("{}{}", l::FILTER, cnt(view.filters.len())) } else { format!("{}  Filter{}", l::FILTER, cnt(view.filters.len())) };
    let r = pill(ui, pal, &fl, !view.filters.is_empty(), false);
    if r.clicked() {
        st.popup = Some(Popup::Filter(pos2(r.rect.right() - 440.0, r.rect.bottom() + 6.0)));
        st.fresh = true;
    }
    let sl = if short { format!("{}{}", l::ARROW_UP_DOWN, cnt(view.sorts.len())) } else { format!("{}  Sort{}", l::ARROW_UP_DOWN, cnt(view.sorts.len())) };
    let r = pill(ui, pal, &sl, !view.sorts.is_empty(), false);
    if r.clicked() {
        st.popup = Some(Popup::Sort(pos2(r.rect.right() - 340.0, r.rect.bottom() + 6.0)));
        st.fresh = true;
    }
}

/// The view tabs and the tools never share a row unless both fit: otherwise the tabs scroll sideways on their own row and
/// the tools get a second row (this is what used to overlap on narrow windows and phones).
fn toolbar(ui: &mut Ui, db: &Database, vi: usize, st: &mut DbState, pal: &Palette, w: f32, acts: &mut Vec<A>) {
    let view = &db.views[vi];
    let tabs_w: f32 = db.views.iter().map(|v| 44.0 + v.name.chars().count() as f32 * 7.6).sum::<f32>() + 40.0;
    let short = w < 480.0;
    let tools_w = if short { 200.0 } else { 372.0 };
    if tabs_w + tools_w + 16.0 <= w {
        ui.horizontal(|ui| {
            ui.set_width(w);
            views_ui(ui, db, vi, st, pal, acts);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| tools_ui(ui, view, st, pal, acts, false));
        });
    } else {
        ScrollArea::horizontal().id_salt("db_tabs").auto_shrink([false, true]).max_width(w).show(ui, |ui| {
            ui.horizontal(|ui| views_ui(ui, db, vi, st, pal, acts));
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.set_width(w);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| tools_ui(ui, view, st, pal, acts, short));
        });
    }
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
            let fm = crate::widgets::field_menu(ui, pal, &r, &mut buf);
            if r.changed() || fm {
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
                clip.text(br.center(), Align2::CENTER_CENTER, l::EXTERNAL_LINK, body(14.0), if resp.hovered() { pal.accent } else { pal.muted });
                if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Open link").clicked() {
                    let url = if buf.starts_with("http") { buf.clone() } else { format!("https://{buf}") };
                    platform::open_url(&ctx, &url);
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
                    if x > rect.right() - 30.0 {
                        break;
                    }
                    x += chip_paint_fit(&clip, pos2(x, rect.center().y), &o.name, o.color, pal, rect.right() - 8.0 - x) + 5.0;
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
        PropKind::Formula | PropKind::Rollup => {
            let resp = ui.interact(rect, id, Sense::hover());
            let cy = rect.center().y;
            let mut err = false;
            let (txt, right, chk) = match val {
                Some(Val::Num(n)) => (crate::formula::fmt_num(*n), true, None),
                Some(Val::Text(s)) => {
                    err = s.starts_with('⚠');
                    (if is_date_text(s) { dates::pretty(s) } else { s.clone() }, false, None)
                }
                Some(Val::Check(b)) => (String::new(), false, Some(*b)),
                _ => (String::new(), false, None),
            };
            if let Some(b) = chk {
                check_box(ui, pal, pos2(rect.left() + 18.0, cy), b);
            } else if right {
                crate::widgets::text_fit(&clip, pos2(rect.right() - 10.0, cy), Align2::RIGHT_CENTER, &txt, body(14.0), pal.text, rect.width() - 20.0);
            } else {
                crate::widgets::text_fit(&clip, pos2(rect.left() + 10.0, cy), Align2::LEFT_CENTER, &txt, body(14.0), if err { pal.danger } else { pal.text }, rect.width() - 20.0);
            }
            let tip = if prop.kind == PropKind::Formula { format!("Formula: {}", prop.cfg.formula) } else { format!("Rollup · {}", prop.cfg.rollup_fn.label()) };
            resp.on_hover_text(tip);
        }
        PropKind::Relation => {
            let resp = ui.interact(rect, id, Sense::click());
            let ids: Vec<u64> = match val {
                Some(Val::Multi(m)) => m.clone(),
                _ => vec![],
            };
            let mut x = rect.left() + 8.0;
            let mut any = false;
            for rid in ids {
                if let Some(t) = rel_title(rid) {
                    if x > rect.right() - 30.0 {
                        break;
                    }
                    any = true;
                    x += chip_paint_fit(&clip, pos2(x, rect.center().y), &t, 7, pal, rect.right() - 8.0 - x) + 5.0;
                }
            }
            if !any && resp.hovered() {
                clip.text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, "Link rows", body(13.5), with_alpha(pal.muted, 0.7));
            }
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                open_popup_at(st, Popup::Relation { row: row.id, prop: prop.id, pos: rect.left_bottom() + vec2(0.0, 4.0) });
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
                crate::widgets::text_fit(&clip, pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, &txt, body(14.0), pal.text, rect.width() - 20.0);
            }
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                let (y, m, _) = match val {
                    Some(Val::Text(s)) => dates::parse(s).unwrap_or_else(dates::today),
                    _ => dates::today(),
                };
                st.date_range = matches!(val, Some(Val::Text(s)) if dates::parse_end(s).is_some());
                open_popup_at(st, Popup::Date { row: row.id, prop: prop.id, pos: rect.left_bottom() + vec2(0.0, 4.0), y, m });
            }
        }
    }
}

// ───────────────────────────── table ─────────────────────────────

#[allow(clippy::too_many_arguments)]
fn table(ui: &mut Ui, ws: &Workspace, db: &Database, vi: usize, rows: &[u64], st: &mut DbState, pal: &Palette, w: f32, acts: &mut Vec<A>) {
    // Phones: a narrower name column, the hover buttons shrink to "OPEN" only — nothing overlaps the title text.
    let compact = w < 560.0;
    let view = &db.views[vi];
    let title_w: f32 = view.widths.get(&0).copied().unwrap_or(if compact { (w * 0.5).clamp(150.0, 230.0) } else { TITLE_W }).clamp(130.0, 640.0);
    let btn_w: f32 = if compact { 62.0 } else { 92.0 };
    let cw_of = |p: &Prop| view.widths.get(&p.id).copied().unwrap_or_else(|| col_w(p.kind)).clamp(70.0, 700.0);
    let total: f32 = title_w + db.props.iter().map(|p| cw_of(p)).sum::<f32>() + 44.0;
    ScrollArea::horizontal().id_salt("table_scroll").auto_shrink([false, true]).show(ui, |ui| {
        ui.set_min_width(w.max(total.min(w)));
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        // header
        let (hr, _) = ui.allocate_exact_size(vec2(total, 32.0), Sense::hover());
        let mut x = hr.left();
        ui.painter().text(pos2(x + 8.0, hr.center().y), Align2::LEFT_CENTER, "Aa  Name", body(13.0), pal.muted);
        resize_handle(ui, pal, Rect::from_min_size(pos2(x + title_w - 6.0, hr.top()), vec2(12.0, 32.0)), Id::new(("thr", vi, 0u64)), title_w, vi, 0, acts);
        x += title_w;
        for p in &db.props {
            let cw = cw_of(p);
            let r = Rect::from_min_size(pos2(x, hr.top()), vec2(cw, 32.0));
            let resp = ui.interact(r, Id::new(("th", p.id)), Sense::click());
            let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
            ui.painter().rect_filled(r, 0.0, with_alpha(pal.hover, t));
            crate::widgets::text_fit(&ui.painter().with_clip_rect(r), pos2(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, &format!("{}  {}", p.kind.glyph(), p.name), body(13.0), lerp_color(pal.muted, pal.text, t), cw - 16.0);
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                open_popup_at(st, Popup::PropMenu { prop: p.id, pos: r.left_bottom() + vec2(0.0, 4.0) });
            }
            resize_handle(ui, pal, Rect::from_min_size(pos2(x + cw - 6.0, hr.top()), vec2(12.0, 32.0)), Id::new(("thr", vi, p.id)), cw, vi, p.id, acts);
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
            let t = if compact { 1.0 } else { t }; // touch screens have no hover: keep the OPEN button visible
            let tr = Rect::from_min_size(rr.min, vec2(title_w, ROW_H));
            let tid = Id::new(("rt", row.id));
            if st.focus_row == Some(row.id) {
                ui.ctx().memory_mut(|m| m.request_focus(tid));
                st.focus_row = None;
            }
            let mut title = row.title.clone();
            let ir = Rect::from_center_size(pos2(tr.left() + 8.0 + (title_w - btn_w - 8.0) / 2.0, tr.center().y), vec2(title_w - btn_w - 8.0, 24.0));
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
            let fm = crate::widgets::field_menu(ui, pal, &r, &mut title);
            if let Some(p) = right_clicked(ui, rr) {
                if !ir.contains(p) {
                    acts.push(A::RowMenu(row.id, p));
                }
            }
            if r.changed() || fm {
                acts.push(A::SetTitle(row.id, title));
            }
            if t > 0.05 {
                let open = Rect::from_min_size(pos2(tr.right() - if compact { 52.0 } else { 74.0 }, tr.center().y - 11.0), vec2(46.0, 22.0));
                let resp = ui.interact(open, Id::new(("ro", row.id)), Sense::click());
                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                ui.painter().rect_filled(open, 5.0, with_alpha(lerp_color(pal.active, pal.border, h), t));
                ui.painter().text(open.center(), Align2::CENTER_CENTER, "OPEN", bold(10.5), with_alpha(pal.text, t));
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    acts.push(A::Open(row.id));
                }
                let del = Rect::from_min_size(pos2(tr.right() - 26.0, tr.center().y - 11.0), vec2(if compact { 0.0 } else { 22.0 }, 22.0));
                let resp = ui.interact(del, Id::new(("rd", row.id)), if compact { Sense::hover() } else { Sense::click() });
                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                if !compact {
                    icons::paint(ui.painter(), del.center(), 13.0, Icon::Trash, with_alpha(lerp_color(pal.muted, pal.danger, h), t));
                }
                if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Delete row").clicked() {
                    acts.push(A::DelRow(row.id));
                }
            }
            // props
            let mut x = rr.left() + title_w;
            for p in &db.props {
                let cw = cw_of(p);
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
                    ui.label(RichText::new(format!("{} {}", l::CALENDAR_DAYS, dates::pretty(s))).font(body(12.0)).color(pal.muted));
                }
                _ => {}
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
/// Drag handle on the right edge of a header cell: resizes the column (stored per view; key 0 = the name column).
#[allow(clippy::too_many_arguments)]
fn resize_handle(ui: &mut Ui, pal: &Palette, r: Rect, id: Id, cur: f32, vi: usize, key: u64, acts: &mut Vec<A>) {
    let resp = ui.interact(r, id, Sense::click_and_drag());
    let active = resp.hovered() || resp.dragged();
    if active {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        let x = r.center().x;
        ui.painter().vline(x, r.y_range(), Stroke::new(2.0_f32, pal.accent));
    }
    if resp.dragged() {
        let dx = resp.drag_delta().x;
        if dx != 0.0 {
            acts.push(A::SetColW(vi, key, (cur + dx).clamp(70.0, 700.0)));
        }
    }
    if resp.double_clicked() {
        acts.push(A::SetColW(vi, key, 0.0));
    }
}

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
                        if let Some(p) = right_clicked(ui, f.response.rect) {
                            acts.push(A::RowMenu(row.id, p));
                        }
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
        let icon = if row.icon.is_empty() { l::FILE_TEXT } else { icons::icon_text(&row.icon) };
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
        if let Some(p) = right_clicked(ui, rect) {
            acts.push(A::RowMenu(row.id, p));
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
                    if !row.cover_img.is_empty() {
                        crate::icons::paint_cover(ui, cr, &crate::media::src_uri(&row.cover_img), row.cover_pos);
                    }
                    if !row.icon.is_empty() {
                        crate::icons::paint_page_icon(ui, cr.center(), &row.icon, 40.0, pal.text);
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
            if let Some(p) = right_clicked(ui, f.response.rect) {
                acts.push(A::RowMenu(row.id, p));
            }
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
        if pill(ui, pal, l::CHEVRON_LEFT, false, false).clicked() {
            st.cal = Some(if m == 1 { (y - 1, 12) } else { (y, m - 1) });
        }
        if pill(ui, pal, l::CHEVRON_RIGHT, false, false).clicked() {
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
            let day_n = dates::days_from_civil(y, m, day);
            let items: Vec<&Page> = rows.iter().filter_map(|r| ws.page(*r)).filter(|r| matches!(r.props.get(&dp), Some(Val::Text(s)) if dates::span(s).map_or(false, |(a, b)| (a..=b).contains(&day_n)))).collect();
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
                if let Some(p) = right_clicked(ui, ir) {
                    acts.push(A::RowMenu(it.id, p));
                }
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

#[allow(clippy::too_many_arguments)]
fn filter_value(ui: &mut Ui, db: &Database, pal: &Palette, f: &Filter, kind: PropKind, vi: usize, fi: usize, w: f32, acts: &mut Vec<A>) {
    if matches!(kind, PropKind::Select | PropKind::MultiSelect) {
        let opts = db.prop(f.prop).map(|p| p.opts.clone()).unwrap_or_default();
        let label = if f.value.is_empty() { "Choose…".to_owned() } else { f.value.clone() };
        if field_btn(ui, pal, &label, w).clicked() && !opts.is_empty() {
            let pos = opts.iter().position(|o| o.name == f.value).map(|p| p + 1).unwrap_or(0);
            let name = opts[pos % opts.len()].name.clone();
            acts.push(A::SetFilter(vi, fi, Filter { value: name, ..f.clone() }));
        }
    } else {
        let mut v = f.value.clone();
        let hint = if kind == PropKind::Date { "YYYY-MM-DD" } else { "Value" };
        let r = field(ui, pal, pal.bg, &mut v, hint, false, w, Id::new(("fv", vi, fi)));
        if r.changed() {
            acts.push(A::SetFilter(vi, fi, Filter { value: v, ..f.clone() }));
        }
    }
}

fn popups(ui: &mut Ui, ws: &Workspace, db_id: u64, db: &Database, vi: usize, st: &mut DbState, pal: &Palette, acts: &mut Vec<A>) {
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
                let avail = ui.available_width();
                let wide = avail >= 430.0;
                let half = ((avail - 30.0 - 8.0) * 0.5).max(70.0);
                let (pw, ow, vw) = if wide { (120.0, 108.0, (avail - 120.0 - 108.0 - 16.0 - 30.0).max(90.0)) } else { (half, half, (avail - 4.0).max(90.0)) };
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
                        let pname = choices.iter().find(|c| c.0 == f.prop).map(|c| c.1.clone()).unwrap_or_else(|| "Name".into());
                        if field_btn(ui, pal, &pname, pw).clicked() {
                            let pos = choices.iter().position(|c| c.0 == f.prop).unwrap_or(0);
                            let nx = &choices[(pos + 1) % choices.len()];
                            let op = FilterOp::for_kind(nx.2)[0];
                            acts.push(A::SetFilter(vi, fi, Filter { prop: nx.0, op, value: String::new() }));
                        }
                        if field_btn(ui, pal, f.op.label(kind), ow).clicked() {
                            let ops = FilterOp::for_kind(kind);
                            let pos = ops.iter().position(|o| *o == f.op).unwrap_or(0);
                            acts.push(A::SetFilter(vi, fi, Filter { op: ops[(pos + 1) % ops.len()], ..f.clone() }));
                        }
                        if wide && f.op.needs_value() {
                            filter_value(ui, db, pal, f, kind, vi, fi, vw, acts);
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icon_button(ui, pal, Icon::Close, 11.0, "Remove filter").clicked() {
                                acts.push(A::DelFilter(vi, fi));
                            }
                        });
                    });
                    if !wide && f.op.needs_value() {
                        ui.add_space(3.0);
                        ui.horizontal(|ui| filter_value(ui, db, pal, f, kind, vi, fi, vw, acts));
                    }
                });
                ui.add_space(6.0);
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
                    if field_btn(ui, pal, &pname, ((ui.available_width() - 30.0 - 8.0) * 0.55).max(80.0)).clicked() {
                        let pos = choices.iter().position(|c| c.0 == s.prop).unwrap_or(0);
                        acts.push(A::SetSort(vi, si, Sort { prop: choices[(pos + 1) % choices.len()].0, asc: s.asc }));
                    }
                    if field_btn(ui, pal, &format!("{} {}", if s.asc { l::ARROW_UP } else { l::ARROW_DOWN }, if s.asc { "Ascending" } else { "Descending" }), ((ui.available_width() - 30.0).max(80.0)).min(120.0)).clicked() {
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
                if menu_item(ui, pal, l::CHART_PIE, "Pin chart to dashboard", Some("Breakdown by this property"), false, false).clicked() {
                    acts.push(A::PinProp(prop, if matches!(p.kind, PropKind::Number | PropKind::Formula | PropKind::Rollup) { "bar" } else { "donut" }));
                    next = Some(None);
                }
                if matches!(p.kind, PropKind::Number | PropKind::Formula | PropKind::Rollup) && menu_item(ui, pal, l::HASH, "Pin total to dashboard", Some("Sum as a number card"), false, false).clicked() {
                    acts.push(A::PinProp(prop, "number"));
                    next = Some(None);
                }
                section_label(ui, pal, "TYPE");
                for k in PropKind::ALL {
                    if menu_item(ui, pal, k.glyph(), k.label(), None, k == p.kind, false).clicked() && k != p.kind {
                        acts.push(A::SetPropKind(prop, k));
                    }
                }
                match p.kind {
                    PropKind::Formula => {
                        rule(ui, pal);
                        section_label(ui, pal, "FORMULA");
                        let mut f = p.cfg.formula.clone();
                        let r = ui.add(
                            TextEdit::multiline(&mut f)
                                .id(Id::new(("fx", prop)))
                                .desired_rows(3)
                                .desired_width(250.0)
                                .font(egui::FontId::monospace(13.0))
                                .hint_text("prop(\"Price\") * prop(\"Qty\")"),
                        );
                        if r.changed() {
                            acts.push(A::SetFormula(prop, f.clone()));
                        }
                        let first = ws.rows(db_id).first().copied();
                        match first.map(|row| crate::formula::eval_str(ws, db_id, row, &f)) {
                            Some(Ok(v)) => {
                                ui.label(RichText::new(format!("  = {}", v.text())).font(body(13.0)).color(pal.accent));
                            }
                            Some(Err(e)) => {
                                ui.label(RichText::new(format!("  ⚠ {e}")).font(body(12.5)).color(pal.danger));
                            }
                            None => {}
                        }
                        ui.add_space(2.0);
                        egui::CollapsingHeader::new(RichText::new("Functions & operators").font(body(12.5)).color(pal.muted)).id_salt(("fxh", prop)).show(ui, |ui| {
                            ui.add(egui::Label::new(RichText::new(crate::formula::HELP).font(body(12.0)).color(pal.muted)).wrap());
                        });
                    }
                    PropKind::Relation => {
                        rule(ui, pal);
                        section_label(ui, pal, "RELATED DATABASE");
                        let dbs: Vec<(u64, String)> = ws.pages.iter().filter(|x| x.db.is_some() && !x.trashed).map(|x| (x.id, ws.display_title(x.id))).collect();
                        ScrollArea::vertical().max_height(150.0).id_salt(("reldbs", prop)).auto_shrink([false, true]).show(ui, |ui| {
                            for (did, name) in dbs {
                                let label = if did == db_id { format!("{name}  (this database)") } else { name };
                                if menu_item(ui, pal, l::TABLE, &label, None, p.cfg.relation == did, false).clicked() {
                                    acts.push(A::SetRelation(prop, did));
                                }
                            }
                        });
                        if p.cfg.relation == db_id {
                            ui.label(RichText::new("  Relating a database to itself gives you task dependencies on the Timeline view.").font(body(12.0)).color(pal.muted));
                        }
                    }
                    PropKind::Rollup => {
                        rule(ui, pal);
                        let rels: Vec<&Prop> = db.props.iter().filter(|x| x.kind == PropKind::Relation).collect();
                        if rels.is_empty() {
                            ui.label(RichText::new("   Add a Relation property first — a rollup summarises the linked rows.").font(body(12.5)).color(pal.muted));
                        } else {
                            section_label(ui, pal, "RELATION");
                            for rp in &rels {
                                if menu_item(ui, pal, l::ARROW_UP_RIGHT, &rp.name, None, p.cfg.rollup_rel == rp.id, false).clicked() {
                                    acts.push(A::RollupRel(prop, rp.id));
                                }
                            }
                            if let Some(tdb) = rels.iter().find(|x| x.id == p.cfg.rollup_rel).and_then(|x| ws.db(x.cfg.relation)) {
                                section_label(ui, pal, "PROPERTY");
                                ScrollArea::vertical().max_height(120.0).id_salt(("rlp", prop)).auto_shrink([false, true]).show(ui, |ui| {
                                    if menu_item(ui, pal, PropKind::Text.glyph(), "Name", None, p.cfg.rollup_prop == 0, false).clicked() {
                                        acts.push(A::RollupProp(prop, 0));
                                    }
                                    for tp in &tdb.props {
                                        if menu_item(ui, pal, tp.kind.glyph(), &tp.name, None, p.cfg.rollup_prop == tp.id, false).clicked() {
                                            acts.push(A::RollupProp(prop, tp.id));
                                        }
                                    }
                                });
                                section_label(ui, pal, "CALCULATE");
                                ScrollArea::vertical().max_height(150.0).id_salt(("rlf", prop)).auto_shrink([false, true]).show(ui, |ui| {
                                    for f in RollupFn::ALL {
                                        if menu_item(ui, pal, l::SIGMA, f.label(), None, p.cfg.rollup_fn == f, false).clicked() {
                                            acts.push(A::RollupFn(prop, f));
                                        }
                                    }
                                });
                            }
                        }
                    }
                    _ => {}
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
                if menu_item(ui, pal, l::TRASH_2, "Delete property", None, false, true).clicked() {
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
            rule(ui, pal);
            egui::CollapsingHeader::new(RichText::new(format!("{}  Pin to dashboard", l::LAYOUT_DASHBOARD)).font(body(13.5)).color(pal.text)).id_salt(("pin_dash", view.id)).show(ui, |ui| {
                ui.label(RichText::new("This view's filters and sorts come along.").font(body(11.5)).color(pal.muted));
                for (kind, glyph, name) in [
                    ("table", l::TABLE, "Table"),
                    ("list", l::LAYOUT_LIST, "List"),
                    ("board", l::KANBAN, "Board"),
                    ("calendar", l::CALENDAR_DAYS, "Calendar"),
                    ("number", l::HASH, "Number (count)"),
                    ("bar", l::CHART_COLUMN, "Bar chart"),
                    ("donut", l::CHART_PIE, "Donut chart"),
                    ("line", l::CHART_LINE, "Line chart"),
                ] {
                    if menu_item(ui, pal, glyph, name, None, false, false).clicked() {
                        acts.push(A::Pin(vi, kind));
                        next = Some(None);
                    }
                }
            });
            if view.kind == ViewKind::Board {
                let sels: Vec<&Prop> = db.props.iter().filter(|p| p.kind == PropKind::Select).collect();
                if !sels.is_empty() {
                    rule(ui, pal);
                    let cur = db.group_prop(view).unwrap_or(0);
                    let cname = db.prop(cur).map(|p| p.name.clone()).unwrap_or_default();
                    if menu_item(ui, pal, l::KANBAN, &format!("Group by: {cname}"), Some("Click to switch property"), false, false).clicked() {
                        let pos = sels.iter().position(|p| p.id == cur).unwrap_or(0);
                        acts.push(A::SetGroup(vi, sels[(pos + 1) % sels.len()].id));
                    }
                }
            }
            if view.kind == ViewKind::Timeline {
                let selfrels: Vec<&Prop> = db.props.iter().filter(|p| p.kind == PropKind::Relation && p.cfg.relation == db_id).collect();
                if !selfrels.is_empty() {
                    rule(ui, pal);
                    let cur = db.dep_prop(view, db_id).unwrap_or(0);
                    let cname = db.prop(cur).map(|p| p.name.clone()).unwrap_or_default();
                    if menu_item(ui, pal, l::ARROW_UP_RIGHT, &format!("Dependencies: {cname}"), Some("Click to switch property"), false, false).clicked() {
                        let pos = selfrels.iter().position(|p| p.id == cur).unwrap_or(0);
                        acts.push(A::SetDep(vi, selfrels[(pos + 1) % selfrels.len()].id));
                    }
                }
            }
            if view.kind == ViewKind::Calendar || view.kind == ViewKind::Timeline {
                let ds: Vec<&Prop> = db.props.iter().filter(|p| p.kind == PropKind::Date).collect();
                if !ds.is_empty() {
                    rule(ui, pal);
                    let cur = db.date_prop(view).unwrap_or(0);
                    let cname = db.prop(cur).map(|p| p.name.clone()).unwrap_or_default();
                    if menu_item(ui, pal, l::CALENDAR_DAYS, &format!("Show by: {cname}"), Some("Click to switch property"), false, false).clicked() {
                        let pos = ds.iter().position(|p| p.id == cur).unwrap_or(0);
                        acts.push(A::SetDateProp(vi, ds[(pos + 1) % ds.len()].id));
                    }
                }
            }
            if db.views.len() > 1 {
                rule(ui, pal);
                if menu_item(ui, pal, l::TRASH_2, "Delete view", None, false, true).clicked() {
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
                            ui.painter().text(pos2(rr.right() - 14.0, rr.center().y), Align2::CENTER_CENTER, l::CHECK, body(15.0), pal.accent);
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
            let curv = match ws.page(row).and_then(|r| r.props.get(&prop)) {
                Some(Val::Text(s)) => dates::span(s),
                _ => None,
            };
            let range_on = st.date_range;
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
                    let center = pos2(grid.left() + cw * col as f32 + cw / 2.0, grid.top() + 20.0 + 32.0 * rw as f32 + 16.0);
                    let r = Rect::from_center_size(center, vec2(30.0, 28.0));
                    let dn = dates::days_from_civil(y, m, day);
                    let (is_s, is_e, inside) = match curv {
                        Some((a, b)) => (dn == a, dn == b, dn > a && dn < b),
                        None => (false, false, false),
                    };
                    let resp = ui.interact(r, Id::new(("dp", day)), Sense::click());
                    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.08);
                    if inside || ((is_s || is_e) && curv.map_or(false, |(a, b)| a != b)) {
                        let band = Rect::from_min_max(pos2(if is_s { center.x } else { center.x - cw / 2.0 }, r.top()), pos2(if is_e { center.x } else { center.x + cw / 2.0 }, r.bottom()));
                        ui.painter().rect_filled(band, 0.0, with_alpha(pal.accent, 0.22));
                    }
                    if is_s || is_e {
                        ui.painter().rect_filled(r, 14.0, pal.accent);
                    } else {
                        ui.painter().rect_filled(r, 14.0, with_alpha(pal.hover, t));
                        if today == (y, m, day) {
                            ui.painter().rect_stroke(r, 14.0, Stroke::new(1.3_f32, pal.accent), egui::StrokeKind::Inside);
                        }
                    }
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, day.to_string(), body(13.0), if is_s || is_e { Color32::WHITE } else { pal.text });
                    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        let (nv, done) = match curv {
                            Some((a, b)) if range_on && a == b => ((a.min(dn), a.max(dn)), true),
                            _ => ((dn, dn), !range_on),
                        };
                        acts.push(A::SetVal(row, prop, Some(Val::Text(dates::span_value(nv.0, nv.1)))));
                        if done {
                            next = Some(None);
                        }
                    }
                }
                rule(ui, pal);
                ui.horizontal(|ui| {
                    if pill(ui, pal, "Today", false, false).clicked() {
                        acts.push(A::SetVal(row, prop, Some(Val::Text(dates::today_iso()))));
                        next = Some(None);
                    }
                    if pill(ui, pal, if range_on { "✓ End date" } else { "End date" }, range_on, false).on_hover_text("Turn the date into a range (shown as a bar on the timeline)").clicked() {
                        st.date_range = !range_on;
                        if range_on {
                            if let Some((a, _)) = curv {
                                acts.push(A::SetVal(row, prop, Some(Val::Text(dates::iso_of_day(a)))));
                            }
                        }
                    }
                    if curv.is_some() && pill(ui, pal, "Clear", false, false).clicked() {
                        acts.push(A::SetVal(row, prop, None));
                        next = Some(None);
                    }
                });
                if range_on {
                    ui.add_space(2.0);
                    ui.label(RichText::new(if curv.map_or(true, |(a, b)| a != b) { "  Click a day to start a new range" } else { "  Now click the end day" }).font(body(12.0)).color(pal.muted));
                }
            })
        }
        Popup::Relation { row, prop, pos } => {
            let Some(p) = db.prop(prop) else {
                st.popup = None;
                return;
            };
            let target = p.cfg.relation;
            let cur: Vec<u64> = match ws.page(row).and_then(|r| r.props.get(&prop)) {
                Some(Val::Multi(m)) => m.clone(),
                _ => vec![],
            };
            let target_rows: Vec<(u64, String)> = ws.rows(target).into_iter().filter(|r| *r != row).filter_map(|r| ws.page(r).map(|x| (r, title_of(x).to_owned()))).collect();
            let has_target = ws.db(target).is_some();
            popup_area(&ctx, pid, pos, pal, &mut st.fresh, 280.0, |ui| {
                if !has_target {
                    ui.label(RichText::new("   Choose the related database first.").font(body(13.0)).color(pal.muted));
                    if menu_item(ui, pal, l::SLIDERS_HORIZONTAL, "Open property settings", None, false, false).clicked() {
                        next = Some(Some(Popup::PropMenu { prop, pos }));
                    }
                    return;
                }
                let r = field(ui, pal, pal.bg, &mut st.new_opt, &format!("Link {}…", ws.display_title(target)), false, 260.0, Id::new(("rel_q", prop)));
                if !r.has_focus() && st.new_opt.is_empty() {
                    r.request_focus();
                }
                let q = st.new_opt.trim().to_lowercase();
                ui.add_space(4.0);
                ScrollArea::vertical().max_height(240.0).auto_shrink([false, true]).show(ui, |ui| {
                    if target_rows.is_empty() {
                        ui.label(RichText::new("   The related database has no other rows.").font(body(13.0)).color(pal.muted));
                    }
                    for (rid, title) in target_rows.iter().filter(|(_, t)| q.is_empty() || t.to_lowercase().contains(&q)) {
                        let (rr, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
                        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                        ui.painter().rect_filled(rr, 6.0, with_alpha(pal.hover, t));
                        let on = cur.contains(rid);
                        check_box(ui, pal, pos2(rr.left() + 16.0, rr.center().y), on);
                        crate::widgets::text_fit(ui.painter(), pos2(rr.left() + 34.0, rr.center().y), Align2::LEFT_CENTER, title, body(14.0), pal.text, rr.width() - 46.0);
                        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            let mut m = cur.clone();
                            if let Some(i) = m.iter().position(|x| x == rid) {
                                m.remove(i);
                            } else {
                                m.push(*rid);
                            }
                            acts.push(A::SetVal(row, prop, if m.is_empty() { None } else { Some(Val::Multi(m)) }));
                        }
                    }
                });
                if !cur.is_empty() {
                    rule(ui, pal);
                    if menu_item(ui, pal, "×", "Clear all", None, false, false).clicked() {
                        acts.push(A::SetVal(row, prop, None));
                        next = Some(None);
                    }
                }
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
        A::Pin(vi, kind) => {
            crate::dashcfg::pin(ws, db_id, vi, kind);
            crate::dashcfg::set_toast("Pinned to the dashboard (Ctrl+D)");
        }
        A::PinProp(prop, kind) => {
            crate::dashcfg::pin_prop(ws, db_id, prop, kind);
            crate::dashcfg::set_toast("Pinned to the dashboard (Ctrl+D)");
        }
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
        A::DupRow(r) => {
            ws.duplicate(r);
        }
        A::RowMenu(r, pos) => {
            ed.db.rmenu = Some((r, pos));
            ed.db.rmenu_fresh = true;
        }
        A::Open(r) => ed.nav = Some(r),
        A::SetColW(vi, key, w) => {
            if let Some(v) = ws.db_mut(db_id).and_then(|d| d.views.get_mut(vi)) {
                if w <= 0.0 {
                    v.widths.remove(&key);
                } else {
                    v.widths.insert(key, w);
                }
            }
        }
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
            if k == PropKind::Formula {
                if let Some(x) = ws.db_mut(db_id).and_then(|d| d.props.iter_mut().find(|x| x.id == pid)) {
                    x.cfg.formula = "prop(\"Name\")".into();
                }
            }
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
                    (Val::Text(s), PropKind::Date, PropKind::Text | PropKind::Url) => Some(Val::Text(dates::pretty(s))),
                    (Val::Num(n), PropKind::Number, PropKind::Formula) | (Val::Num(n), PropKind::Formula, PropKind::Number) => Some(Val::Num(*n)),
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
                    if v.dep == p {
                        v.dep = 0;
                    }
                }
            }
            for r in ws.pages.iter_mut().filter(|r| r.row && r.parent == Some(db_id)) {
                r.props.remove(&p);
            }
        }
        A::SetFormula(p, f) => {
            if let Some(x) = ws.db_mut(db_id).and_then(|d| d.props.iter_mut().find(|x| x.id == p)) {
                x.cfg.formula = f;
            }
        }
        A::SetRelation(p, target) => {
            if let Some(x) = ws.db_mut(db_id).and_then(|d| d.props.iter_mut().find(|x| x.id == p)) {
                if x.cfg.relation != target {
                    x.cfg.relation = target;
                    for r in ws.pages.iter_mut().filter(|r| r.row && r.parent == Some(db_id)) {
                        r.props.remove(&p);
                    }
                }
            }
        }
        A::RollupRel(p, rel) => {
            if let Some(x) = ws.db_mut(db_id).and_then(|d| d.props.iter_mut().find(|x| x.id == p)) {
                x.cfg.rollup_rel = rel;
                x.cfg.rollup_prop = 0;
            }
        }
        A::RollupProp(p, tp) => {
            if let Some(x) = ws.db_mut(db_id).and_then(|d| d.props.iter_mut().find(|x| x.id == p)) {
                x.cfg.rollup_prop = tp;
            }
        }
        A::RollupFn(p, f) => {
            if let Some(x) = ws.db_mut(db_id).and_then(|d| d.props.iter_mut().find(|x| x.id == p)) {
                x.cfg.rollup_fn = f;
            }
        }
        A::SetDep(i, p) => {
            if let Some(v) = ws.db_mut(db_id).and_then(|d| d.views.get_mut(i)) {
                v.dep = p;
            }
        }
        A::SetZoom(i, z) => {
            if let Some(v) = ws.db_mut(db_id).and_then(|d| d.views.get_mut(i)) {
                v.zoom = z.clamp(1, 5);
            }
        }
        A::AddDepProp => {
            let pid = ws.add_prop(db_id, "Blocked by", PropKind::Relation, &[]);
            if let Some(x) = ws.db_mut(db_id).and_then(|d| d.props.iter_mut().find(|x| x.id == pid)) {
                x.cfg.relation = db_id;
            }
        }
        A::AddDep(succ, pred) => {
            let Some(dp) = ws.db(db_id).and_then(|d| d.views.get(d.active).and_then(|v| d.dep_prop(v, db_id))) else { return };
            // a row can neither depend on itself nor (indirectly) on something that already depends on it
            let exists = matches!(ws.page(succ).and_then(|p| p.props.get(&dp)), Some(Val::Multi(m)) if m.contains(&pred));
            if !exists && (succ == pred || depends_on(ws, dp, pred, succ)) {
                return;
            }
            if let Some(r) = ws.page_mut(succ) {
                let mut m = match r.props.get(&dp) {
                    Some(Val::Multi(m)) => m.clone(),
                    _ => vec![],
                };
                if let Some(i) = m.iter().position(|x| *x == pred) {
                    m.remove(i);
                } else {
                    m.push(pred);
                }
                if m.is_empty() {
                    r.props.remove(&dp);
                } else {
                    r.props.insert(dp, Val::Multi(m));
                }
                r.updated = now_ms();
            }
        }
        A::Reschedule => {
            let Some((dp, depp)) = ws.db(db_id).and_then(|d| {
                let v = d.views.get(d.active)?;
                Some((d.date_prop(v)?, d.dep_prop(v, db_id)?))
            }) else {
                return;
            };
            let rows = ws.rows(db_id);
            for _ in 0..rows.len().max(1) {
                let mut moved = false;
                for rid in &rows {
                    let Some(r) = ws.page(*rid) else { continue };
                    let Some(Val::Multi(preds)) = r.props.get(&depp) else { continue };
                    let Some((s, e)) = (match r.props.get(&dp) {
                        Some(Val::Text(t)) => dates::span(t),
                        _ => None,
                    }) else {
                        continue;
                    };
                    let need = preds
                        .iter()
                        .filter_map(|p| ws.page(*p))
                        .filter_map(|p| match p.props.get(&dp) {
                            Some(Val::Text(t)) => dates::span(t).map(|x| x.1),
                            _ => None,
                        })
                        .max();
                    if let Some(end) = need {
                        if s <= end {
                            let shift = end + 1 - s;
                            if let Some(rm) = ws.page_mut(*rid) {
                                rm.props.insert(dp, Val::Text(dates::span_value(s + shift, e + shift)));
                                rm.updated = now_ms();
                                moved = true;
                            }
                        }
                    }
                }
                if !moved {
                    break;
                }
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

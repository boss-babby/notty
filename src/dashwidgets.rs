//! Widget registry and implementations for the dashboard.
//!
//! A widget type is one [`Def`]: a stable `kind` id, name / description / icon for the gallery, a default size, the list of
//! settings it understands ([`Opt`] — the inspector builds its form from this list) and a draw function.  The built-in types
//! live in [`DEFS`]; features added later call [`register`] once at start-up and get the gallery entry, the settings
//! inspector, drag & drop, resizing, persistence and "pin to dashboard" for free.  See `DASHBOARD.md`.

use std::cell::RefCell;

use egui::{pos2, vec2, Align2, Color32, CursorIcon, Id, Pos2, Rect, RichText, Sense, Stroke, Ui};

use crate::auth::now_ms;
use crate::dashboard::Nav;
use crate::dashcfg::{Agg, Bucket, Cat, Cfg, Item, Opt, Range, Widget};
use crate::dashcharts::{self as ch, Kind, Style};
use crate::dashdata::{self as dd, fmt_agg, fmt_full, Dataset, Ser};
use crate::dates;
use crate::lucide as l;
use crate::model::{BlockKind, Page, PageKind, PropKind, Val, Workspace};
use crate::theme::{bold, body, tag_color, Palette};
use crate::widgets::{text_fit, with_alpha};

/// A drill-down list requested by a click on a chart element, number or day.
pub struct Drill {
    pub title: String,
    pub rows: Vec<u64>,
    pub at: Pos2,
    /// True for the frame in which the popup opens (the click that opened it must not close it).
    pub fresh: bool,
}

/// Everything a draw function can touch.
pub struct Cx<'a> {
    pub ws: &'a mut Workspace,
    pub pal: &'a Palette,
    pub nav: &'a mut Nav,
    pub drill: &'a mut Option<Drill>,
    /// Dashboard-wide time window.
    pub global: Range,
    /// Id of the widget being drawn (stable key for per-widget UI state).
    pub wid: u64,
    /// Set to ask the dashboard to open the settings inspector for this widget.
    pub configure: bool,
}

pub type DrawFn = fn(&mut Cx, &mut Widget, &mut Ui);

#[derive(Clone, Copy)]
pub struct Def {
    pub kind: &'static str,
    pub name: &'static str,
    pub desc: &'static str,
    pub cat: Cat,
    pub glyph: &'static str,
    /// Default size on the 12-column grid.
    pub span: u8,
    pub rows: u8,
    /// Settings shown in the inspector.
    pub opts: &'static [Opt],
    pub draw: DrawFn,
}

thread_local! {
    static EXTRA: RefCell<Vec<Def>> = const { RefCell::new(Vec::new()) };
}

/// Registers a widget type added by another feature. Call once at start-up; re-registering a `kind` replaces it.
#[allow(dead_code)]
pub fn register(def: Def) {
    EXTRA.with(|e| {
        let mut e = e.borrow_mut();
        e.retain(|d| d.kind != def.kind);
        e.push(def);
    });
}

pub fn all() -> Vec<Def> {
    let mut v: Vec<Def> = DEFS.to_vec();
    EXTRA.with(|e| {
        for d in e.borrow().iter() {
            v.retain(|x| x.kind != d.kind);
            v.push(*d);
        }
    });
    v
}

pub fn def(kind: &str) -> Option<Def> {
    all().into_iter().find(|d| d.kind == kind)
}

/// Plot types of the unified Chart widget, in the order the plot picker shows them.
pub const CHART_KINDS: [&str; 14] = ["bar", "hbar", "stacked", "line", "area", "pie", "donut", "funnel", "radar", "histogram", "scatter", "heatmap", "gauge", "number"];

/// Every setting any plot type understands — the Chart widget shows the subset of its current plot type.
const CHART_OPTS: &[Opt] = &[Db, View, Filters, Group, Stack, Value, Agg, Bucket, Axes, Date, Range, Limit, Goal, Cumulative, HideEmpty, Labels, Legend, Palette];

/// The registry entry that describes (name, icon, settings, drawing of) widget `w`: for a Chart widget that is the entry of its plot type.
pub fn def_for(w: &Widget) -> Option<Def> {
    match crate::dashcfg::plot_of(w) {
        Some(k) => def(&k),
        None => def(&w.kind),
    }
}

/// Short name of a plot type for the plot picker.
pub fn plot_label(kind: &str) -> &'static str {
    match kind {
        "bar" => "Bar",
        "hbar" => "H-bar",
        "stacked" => "Stacked",
        "line" => "Line",
        "area" => "Area",
        "pie" => "Pie",
        "donut" => "Donut",
        "funnel" => "Funnel",
        "radar" => "Radar",
        "histogram" => "Histogram",
        "scatter" => "Scatter",
        "heatmap" => "Heat-map",
        "gauge" => "Gauge",
        "number" => "Number",
        _ => "Chart",
    }
}

fn draw_chart(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    if let Some(d) = def_for(w) {
        (d.draw)(cx, w, ui);
    }
}

use Opt::*;

const GROUPED: &[Opt] = &[Db, View, Filters, Group, Value, Agg, Bucket, Date, Range, Limit, HideEmpty, Labels, Legend, Palette];
const SERIES: &[Opt] = &[Db, View, Filters, Group, Stack, Value, Agg, Bucket, Date, Range, Limit, HideEmpty, Labels, Legend, Palette];
const TIMED: &[Opt] = &[Db, View, Filters, Group, Stack, Value, Agg, Bucket, Date, Range, Limit, Cumulative, Labels, Legend, Palette];

pub static DEFS: &[Def] = &[
    // ── workspace ──
    Def { kind: "stats", name: "Workspace stats", desc: "Pages, databases, records, open and done tasks, words written", cat: Cat::Overview, glyph: l::LAYOUT_DASHBOARD, span: 12, rows: 1, opts: &[], draw: draw_stats },
    Def { kind: "tasks", name: "My tasks", desc: "Open to-dos from every page — tick them off right here", cat: Cat::Overview, glyph: l::LIST_TODO, span: 6, rows: 3, opts: &[Limit], draw: draw_tasks },
    Def { kind: "upcoming", name: "Upcoming & overdue", desc: "Records dated in the next two weeks or the last week", cat: Cat::Overview, glyph: l::CALENDAR_CLOCK, span: 6, rows: 3, opts: &[Limit], draw: draw_upcoming },
    Def { kind: "recent", name: "Recently edited", desc: "The pages you worked on last", cat: Cat::Overview, glyph: l::HISTORY, span: 4, rows: 3, opts: &[Limit], draw: draw_recent },
    Def { kind: "favorites", name: "Favorites", desc: "Starred pages, one click away", cat: Cat::Overview, glyph: l::STAR, span: 4, rows: 3, opts: &[], draw: draw_favorites },
    Def { kind: "activity", name: "Activity", desc: "Pages edited per day, last 14 days — click a bar to see which", cat: Cat::Overview, glyph: l::CHART_COLUMN, span: 4, rows: 3, opts: &[Labels, Palette], draw: draw_activity },
    Def { kind: "subpages", name: "Pages", desc: "Browse the sub-pages of any page — and create new pages or dashboards right here", cat: Cat::Overview, glyph: l::FILES, span: 4, rows: 3, opts: &[Pages], draw: draw_subpages },
    // ── charts ──
    Def { kind: "chart", name: "Chart", desc: "One widget for every plot — bar, line, pie, funnel, radar, gauge, scatter … Pick the plot type in its settings", cat: Cat::Charts, glyph: l::CHART_COLUMN, span: 6, rows: 3, opts: CHART_OPTS, draw: draw_chart },
    Def { kind: "number", name: "Number", desc: "One big figure: count, sum, average … with goal and trend", cat: Cat::Charts, glyph: l::HASH, span: 3, rows: 1, opts: &[Db, View, Filters, Value, Agg, Date, Range, Goal], draw: draw_number },
    Def { kind: "gauge", name: "Gauge", desc: "Progress towards a goal on a dial", cat: Cat::Charts, glyph: l::GAUGE, span: 3, rows: 2, opts: &[Db, View, Filters, Value, Agg, Date, Range, Goal], draw: draw_gauge },
    Def { kind: "bar", name: "Bar chart", desc: "Compare groups side by side", cat: Cat::Charts, glyph: l::CHART_COLUMN, span: 6, rows: 3, opts: SERIES, draw: draw_bar },
    Def { kind: "hbar", name: "Horizontal bars", desc: "Ranked bars with long labels", cat: Cat::Charts, glyph: l::CHART_BAR, span: 6, rows: 3, opts: SERIES, draw: draw_hbar },
    Def { kind: "stacked", name: "Stacked bars", desc: "Totals split by a second property", cat: Cat::Charts, glyph: l::CHART_COLUMN_STACKED, span: 6, rows: 3, opts: SERIES, draw: draw_stacked },
    Def { kind: "line", name: "Line chart", desc: "Trends over time or across groups", cat: Cat::Charts, glyph: l::CHART_LINE, span: 6, rows: 3, opts: TIMED, draw: draw_line },
    Def { kind: "area", name: "Area chart", desc: "Filled trend with running totals", cat: Cat::Charts, glyph: l::CHART_AREA, span: 6, rows: 3, opts: TIMED, draw: draw_area },
    Def { kind: "pie", name: "Pie chart", desc: "Share of each group", cat: Cat::Charts, glyph: l::CHART_PIE, span: 4, rows: 3, opts: GROUPED, draw: draw_pie },
    Def { kind: "donut", name: "Donut chart", desc: "Share of each group with the total in the middle", cat: Cat::Charts, glyph: l::CIRCLE_DOT, span: 4, rows: 3, opts: GROUPED, draw: draw_donut },
    Def { kind: "funnel", name: "Funnel", desc: "Pipeline stages with conversion", cat: Cat::Charts, glyph: l::FUNNEL, span: 6, rows: 3, opts: GROUPED, draw: draw_funnel },
    Def { kind: "radar", name: "Radar", desc: "Compare several measures across categories", cat: Cat::Charts, glyph: l::RADAR, span: 4, rows: 3, opts: SERIES, draw: draw_radar },
    Def { kind: "histogram", name: "Histogram", desc: "Distribution of a number property", cat: Cat::Charts, glyph: l::CHART_NO_AXES_COLUMN, span: 6, rows: 3, opts: &[Db, View, Filters, Value, Date, Range, Limit, Labels, Palette], draw: draw_histogram },
    Def { kind: "scatter", name: "Scatter plot", desc: "Relate two numbers, coloured by group", cat: Cat::Charts, glyph: l::CHART_SCATTER, span: 6, rows: 3, opts: &[Db, View, Filters, Group, Axes, Date, Range, Palette], draw: draw_scatter },
    Def { kind: "heatmap", name: "Activity heat-map", desc: "Records per day, like a contribution calendar", cat: Cat::Charts, glyph: l::GRID_3X3, span: 8, rows: 2, opts: &[Db, View, Filters, Date, Range, Palette], draw: draw_heatmap },
    // ── records ──
    Def { kind: "table", name: "Table", desc: "A filtered, sortable view of a database", cat: Cat::Data, glyph: l::TABLE, span: 6, rows: 4, opts: &[Db, View, Filters, Cols, Sort, Date, Range], draw: draw_table },
    Def { kind: "list", name: "List", desc: "Compact list of records", cat: Cat::Data, glyph: l::LAYOUT_LIST, span: 4, rows: 3, opts: &[Db, View, Filters, Cols, Sort, Date, Range], draw: draw_list },
    Def { kind: "board", name: "Board", desc: "Kanban columns — drag cards to change their status", cat: Cat::Data, glyph: l::KANBAN, span: 12, rows: 4, opts: &[Db, View, Filters, Group, Date, Range], draw: draw_board },
    Def { kind: "calendar", name: "Calendar", desc: "Month view of dated records", cat: Cat::Data, glyph: l::CALENDAR_DAYS, span: 6, rows: 4, opts: &[Db, View, Filters, Date], draw: draw_calendar },
    Def { kind: "progress", name: "Progress bars", desc: "One bar per record against a goal", cat: Cat::Data, glyph: l::TARGET, span: 4, rows: 3, opts: &[Db, View, Filters, Value, Goal, Sort, Date, Range, Limit, Palette], draw: draw_progress },
    // ── personal ──
    Def { kind: "notes", name: "Sticky note", desc: "Free text that lives on the dashboard", cat: Cat::Personal, glyph: l::STICKY_NOTE, span: 3, rows: 3, opts: &[Text], draw: draw_notes },
    Def { kind: "checklist", name: "Checklist", desc: "Your own quick to-do list", cat: Cat::Personal, glyph: l::LIST_CHECKS, span: 3, rows: 3, opts: &[Items], draw: draw_checklist },
    Def { kind: "countdown", name: "Countdown", desc: "Days until a date that matters", cat: Cat::Personal, glyph: l::HOURGLASS, span: 3, rows: 2, opts: &[Target], draw: draw_countdown },
    Def { kind: "timer", name: "Focus timer", desc: "Pomodoro: 25 / 5 / 15 minutes", cat: Cat::Personal, glyph: l::TIMER, span: 3, rows: 2, opts: &[], draw: draw_timer },
    Def { kind: "today", name: "Today", desc: "Date, week and how far through the year you are", cat: Cat::Personal, glyph: l::SUN, span: 4, rows: 2, opts: &[], draw: draw_today },
    Def { kind: "quicklinks", name: "Quick links", desc: "Your most-used pages, pinned", cat: Cat::Personal, glyph: l::PIN, span: 4, rows: 3, opts: &[Pages], draw: draw_quicklinks },
    Def { kind: "pagepreview", name: "Page preview", desc: "The start of any page, right on the dashboard", cat: Cat::Personal, glyph: l::FILE_TEXT, span: 4, rows: 3, opts: &[Pages], draw: draw_pagepreview },
    Def { kind: "quickadd", name: "Quick add", desc: "Capture a record or a page in one line", cat: Cat::Personal, glyph: l::CIRCLE_PLUS, span: 4, rows: 2, opts: &[Db], draw: draw_quickadd },
];

// ───────────────────────────── small helpers ─────────────────────────────

const ROW_H: f32 = 30.0;

fn title_of(ws: &Workspace, id: u64) -> String {
    ws.page(id).map(|p| if p.title.is_empty() { "Untitled".to_owned() } else { p.title.clone() }).unwrap_or_else(|| "Untitled".into())
}

fn note(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.label(RichText::new(text).font(body(13.0)).color(pal.muted));
}

/// Centred hint in the free space of a widget, optionally with a "Configure" button. Returns true when the button was clicked.
fn empty(ui: &mut Ui, pal: &Palette, text: &str, button: Option<&str>) -> bool {
    let size = ui.available_size();
    let (r, _) = ui.allocate_exact_size(vec2(size.x, size.y.max(40.0)), Sense::hover());
    let p = ui.painter_at(r);
    let dy = if button.is_some() { 14.0 } else { 0.0 };
    text_fit(&p, pos2(r.center().x, r.center().y - dy), Align2::CENTER_CENTER, text, body(13.0), pal.muted, r.width() - 16.0);
    if let Some(b) = button {
        let br = Rect::from_center_size(pos2(r.center().x, r.center().y + 14.0), vec2(tw(&p, b) + 24.0, 28.0));
        let resp = ui.interact(br, ui.id().with(("empty_btn", b)), Sense::click());
        p.rect_filled(br, 14.0, if resp.hovered() { pal.active } else { pal.hover });
        p.text(br.center(), Align2::CENTER_CENTER, b, body(12.5), pal.text);
        return resp.on_hover_cursor(CursorIcon::PointingHand).clicked();
    }
    false
}

fn tw(p: &egui::Painter, s: &str) -> f32 {
    p.layout_no_wrap(s.to_owned(), body(12.5), Color32::WHITE).size().x
}

/// Checks that the widget has a database; otherwise shows the "choose a database" state.
fn ensure_db(cx: &mut Cx, w: &Widget, ui: &mut Ui) -> bool {
    if cx.ws.db(w.cfg.db).map_or(false, |_| cx.ws.page(w.cfg.db).map_or(false, |p| !p.trashed)) {
        return true;
    }
    let none = crate::dashcfg::databases(cx.ws).is_empty();
    let msg = if none { "Create a database to see data here" } else if w.cfg.db == 0 { "Choose a database" } else { "The source database is gone" };
    if empty(ui, cx.pal, msg, if none { None } else { Some("Choose database") }) {
        cx.configure = true;
    }
    false
}

fn link_row(ui: &mut Ui, pal: &Palette, id: Id, left: &str, right: &str, right_col: Color32) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H - 2.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, 7.0, pal.hover);
    }
    let rw = if right.is_empty() { 0.0 } else { (r.width() * 0.4).min(120.0) };
    text_fit(ui.painter(), pos2(r.left() + 6.0, r.center().y), Align2::LEFT_CENTER, left, body(13.5), pal.text, r.width() - rw - 16.0);
    if !right.is_empty() {
        text_fit(ui.painter(), pos2(r.right() - 6.0, r.center().y), Align2::RIGHT_CENTER, right, body(12.0), right_col, rw);
    }
    let _ = id;
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

fn fit_rows(ui: &Ui, reserve: f32, row_h: f32) -> usize {
    (((ui.available_height() - reserve) / row_h).floor() as isize).max(1) as usize
}

fn open_drill(cx: &mut Cx, title: String, rows: Vec<u64>, at: Pos2) {
    if rows.is_empty() {
        return;
    }
    *cx.drill = Some(Drill { title, rows, at, fresh: true });
}

fn pointer(ui: &Ui) -> Pos2 {
    ui.ctx().pointer_interact_pos().unwrap_or(ui.max_rect().center())
}

fn page_text(p: &Page) -> String {
    p.blocks.iter().map(|b| b.text.split_whitespace().count()).sum::<usize>().to_string()
}

// ───────────────────────────── workspace widgets ─────────────────────────────

fn draw_stats(cx: &mut Cx, _w: &mut Widget, ui: &mut Ui) {
    let ws = &*cx.ws;
    let live: Vec<&Page> = ws.pages.iter().filter(|p| !p.trashed).collect();
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
    let stats: Vec<(&str, String, Color32)> = vec![
        ("Pages", pages.to_string(), cx.pal.accent),
        ("Databases", dbs.to_string(), tag_color(4)),
        ("Records", records.to_string(), tag_color(2)),
        ("Open tasks", open_t.to_string(), tag_color(3)),
        ("Done tasks", done_t.to_string(), tag_color(2)),
        ("Words written", dd::fmt_num(words as f64), tag_color(5)),
    ];
    let w = ui.available_width();
    let h = ui.available_height();
    let cols = ((w / 130.0).floor() as usize).clamp(2, 6);
    let rows = stats.len().div_ceil(cols);
    let cw = w / cols as f32;
    let ch_ = (h / rows as f32).min(80.0);
    let (r, _) = ui.allocate_exact_size(vec2(w, ch_ * rows as f32), Sense::hover());
    let p = ui.painter_at(r);
    for (i, (name, val, col)) in stats.iter().enumerate() {
        let (cx_, cy_) = (i % cols, i / cols);
        let cell = Rect::from_min_size(pos2(r.left() + cw * cx_ as f32, r.top() + ch_ * cy_ as f32), vec2(cw, ch_));
        p.rect_filled(Rect::from_min_size(pos2(cell.left() + 2.0, cell.center().y - 18.0), vec2(4.0, 36.0)), 2.0, *col);
        p.text(pos2(cell.left() + 16.0, cell.center().y - 8.0), Align2::LEFT_CENTER, val, bold(24.0), cx.pal.text);
        text_fit(&p, pos2(cell.left() + 16.0, cell.center().y + 15.0), Align2::LEFT_CENTER, name, body(12.5), cx.pal.muted, cw - 22.0);
    }
}

fn draw_tasks(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    let mut tasks: Vec<(u64, usize, String, String)> = vec![];
    for p in cx.ws.pages.iter().filter(|p| !p.trashed) {
        for (i, b) in p.blocks.iter().enumerate() {
            if b.kind == BlockKind::Todo && !b.checked && !b.text.trim().is_empty() {
                tasks.push((p.id, i, b.text.clone(), if p.title.is_empty() { "Untitled".into() } else { p.title.clone() }));
            }
        }
    }
    let total = tasks.len();
    if tasks.is_empty() {
        note(ui, cx.pal, "All clear — add to-dos with /todo on any page.");
        return;
    }
    let cap = if w.cfg.limit > 0 { w.cfg.limit as usize } else { 99 };
    let fit = fit_rows(ui, 22.0, ROW_H).min(cap);
    let mut tick: Option<(u64, usize)> = None;
    for (pid, bi, text, page) in tasks.iter().take(fit) {
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
        let cb = Rect::from_center_size(pos2(r.left() + 12.0, r.center().y), vec2(17.0, 17.0));
        let cresp = ui.interact(cb.expand(6.0), Id::new(("dash_tick", pid, bi)), Sense::click());
        ui.painter().rect_stroke(cb, 5.0, Stroke::new(1.6_f32, if cresp.hovered() { cx.pal.accent } else { cx.pal.muted }), egui::StrokeKind::Inside);
        if cresp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            tick = Some((*pid, *bi));
        }
        if resp.hovered() {
            ui.painter().rect_filled(Rect::from_min_max(pos2(r.left() + 28.0, r.top()), r.max), 7.0, cx.pal.hover);
        }
        let pw = (r.width() * 0.3).clamp(60.0, 130.0);
        text_fit(ui.painter(), pos2(r.left() + 34.0, r.center().y), Align2::LEFT_CENTER, text, body(13.5), cx.pal.text, r.width() - pw - 44.0);
        text_fit(ui.painter(), pos2(r.right() - 4.0, r.center().y), Align2::RIGHT_CENTER, page, body(11.5), cx.pal.muted, pw);
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() && tick.is_none() {
            cx.nav.out.open = Some(*pid);
        }
    }
    if total > fit {
        note(ui, cx.pal, &format!("+ {} more", total - fit));
    }
    if let Some((pid, bi)) = tick {
        if let Some(p) = cx.ws.page_mut(pid) {
            if let Some(b) = p.blocks.get_mut(bi) {
                b.checked = true;
            }
            p.updated = now_ms();
        }
    }
}

fn draw_upcoming(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    let today = dates::today_day();
    let mut items: Vec<(i64, i64, u64, String)> = vec![];
    for db in cx.ws.pages.iter().filter(|p| p.db.is_some() && !p.trashed) {
        let Some(d) = db.db.as_ref() else { continue };
        for prop in d.props.iter().filter(|p| p.kind == PropKind::Date) {
            for rid in cx.ws.rows(db.id) {
                let Some(row) = cx.ws.page(rid) else { continue };
                if let Some(Val::Text(s)) = row.props.get(&prop.id) {
                    if let Some((a, b)) = dates::span(s) {
                        if b >= today - 7 && a <= today + 14 {
                            items.push((a, b, rid, title_of(cx.ws, rid)));
                        }
                    }
                }
            }
        }
    }
    items.sort_by_key(|i| i.0);
    if items.is_empty() {
        note(ui, cx.pal, "Nothing scheduled. Give a database a Date property to see it here.");
        return;
    }
    let cap = if w.cfg.limit > 0 { w.cfg.limit as usize } else { 99 };
    let fit = fit_rows(ui, 0.0, ROW_H - 2.0).min(cap);
    for (a, b, rid, title) in items.iter().take(fit) {
        let overdue = *b < today;
        let label = if overdue { format!("{}d overdue", today - b) } else if *a <= today { "today".to_owned() } else if *a == today + 1 { "tomorrow".to_owned() } else { format!("in {}d", a - today) };
        let col = if overdue { cx.pal.danger } else if *a <= today { tag_color(3) } else { cx.pal.muted };
        if link_row(ui, cx.pal, Id::new(("dash_up", rid, a)), title, &label, col) {
            cx.nav.out.open = Some(*rid);
        }
    }
}

fn draw_recent(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    let mut pages: Vec<&Page> = cx.ws.pages.iter().filter(|p| !p.trashed && !p.row).collect();
    pages.sort_by(|a, b| b.updated.partial_cmp(&a.updated).unwrap_or(std::cmp::Ordering::Equal));
    let cap = if w.cfg.limit > 0 { w.cfg.limit as usize } else { 99 };
    let fit = fit_rows(ui, 0.0, ROW_H - 2.0).min(cap);
    let list: Vec<(u64, String)> = pages.iter().take(fit).map(|p| (p.id, if p.title.is_empty() { "Untitled".into() } else { p.title.clone() })).collect();
    for (id, t) in list {
        if link_row(ui, cx.pal, Id::new(("dash_rc", id)), &t, "", cx.pal.muted) {
            cx.nav.out.open = Some(id);
        }
    }
}

fn draw_subpages(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    let parent = w.cfg.pages.first().copied().filter(|p| cx.ws.page(*p).map_or(false, |p| !p.trashed));
    let mut open: Option<u64> = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if pill(ui, cx.pal, l::FILE_PLUS, "New page", true) {
            let id = cx.ws.add_page(parent, PageKind::Doc);
            open = Some(id);
        }
        if pill(ui, cx.pal, l::LAYOUT_DASHBOARD, "New dashboard", false) {
            open = Some(crate::dashcfg::new_dashboard_page(cx.ws, parent, "starter"));
        }
    });
    ui.add_space(4.0);
    let ids = cx.ws.children(parent);
    if ids.is_empty() {
        note(ui, cx.pal, if parent.is_some() { "No sub-pages yet — create the first one." } else { "No pages yet — create the first one." });
    }
    let fit = fit_rows(ui, 0.0, ROW_H - 2.0);
    for id in ids.into_iter().take(fit) {
        let (title, icon, dash) = match cx.ws.page(id) {
            Some(p) => (title_of(cx.ws, id), p.icon.clone(), cx.ws.dash.page_board(id).is_some()),
            None => continue,
        };
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H - 2.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(r, 7.0, cx.pal.hover);
        }
        let icon = if icon.is_empty() { l::FILE_TEXT.to_owned() } else { icon };
        crate::icons::paint_page_icon(ui, pos2(r.left() + 16.0, r.center().y), &icon, 15.0, cx.pal.muted);
        let rw = if dash { 22.0 } else { 0.0 };
        text_fit(ui.painter(), pos2(r.left() + 32.0, r.center().y), Align2::LEFT_CENTER, &title, body(13.5), cx.pal.text, r.width() - 40.0 - rw);
        if dash {
            ui.painter().text(pos2(r.right() - 12.0, r.center().y), Align2::CENTER_CENTER, l::LAYOUT_DASHBOARD, body(13.0), cx.pal.accent);
        }
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            open = Some(id);
        }
    }
    if let Some(id) = open {
        cx.nav.out.open = Some(id);
    }
}

/// Small rounded button with a Lucide glyph (used inside widgets).
pub fn pill(ui: &mut Ui, pal: &Palette, glyph: &str, text: &str, primary: bool) -> bool {
    let font = if primary { bold(12.5) } else { body(12.5) };
    let g = ui.painter().layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE);
    let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 44.0, 28.0), Sense::click());
    let fill = if primary { if resp.hovered() { pal.accent.gamma_multiply(0.85) } else { pal.accent } } else if resp.hovered() { pal.active } else { pal.hover };
    let fg = if primary { Color32::WHITE } else { pal.text };
    ui.painter().rect_filled(r, 14.0, fill);
    ui.painter().text(pos2(r.left() + 16.0, r.center().y), Align2::CENTER_CENTER, glyph, body(14.0), fg);
    ui.painter().text(pos2(r.left() + 29.0, r.center().y), Align2::LEFT_CENTER, text, font, fg);
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

fn draw_favorites(cx: &mut Cx, _w: &mut Widget, ui: &mut Ui) {
    let fit = fit_rows(ui, 0.0, ROW_H - 2.0);
    let list: Vec<(u64, String)> = cx.ws.pages.iter().filter(|p| p.favorite && !p.trashed).take(fit).map(|p| (p.id, if p.title.is_empty() { "Untitled".into() } else { p.title.clone() })).collect();
    if list.is_empty() {
        note(ui, cx.pal, "No favorites yet — star a page to pin it here.");
    }
    for (id, t) in list {
        if link_row(ui, cx.pal, Id::new(("dash_fv", id)), &t, "", cx.pal.muted) {
            cx.nav.out.open = Some(id);
        }
    }
}

fn draw_activity(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    let today = (now_ms() / 86_400_000.0).floor() as i64;
    let mut ds = Dataset::default();
    let mut ser = Ser { name: "Pages edited".into(), ..Default::default() };
    for i in 0..14i64 {
        let day = today - 13 + i;
        let ids: Vec<u64> = cx.ws.pages.iter().filter(|p| !p.trashed && (p.updated / 86_400_000.0).floor() as i64 == day).map(|p| p.id).collect();
        let (_, m, d) = dates::civil_from_days(day);
        ds.cats.push(format!("{} {d}", &dates::MONTHS[m as usize - 1][..3]));
        ds.colors.push(None);
        ser.vals.push(ids.len() as f64);
        ser.rows.push(ids);
    }
    ds.series.push(ser);
    let size = ui.available_size();
    let style = Style { kind: Kind::Bar, labels: w.cfg.labels, legend: false, palette: w.cfg.palette };
    if let Some((s, c)) = ch::chart(ui, cx.pal, Id::new(("dash_chart", cx.wid)), size, &ds, &style) {
        let at = pointer(ui);
        open_drill(cx, format!("Edited on {}", ds.cats[c]), ds.series[s].rows[c].clone(), at);
    }
}

// ───────────────────────────── numbers ─────────────────────────────

fn agg_caption(cx: &Cx, w: &Widget) -> String {
    let c = &w.cfg;
    let prop = cx.ws.db(c.db).and_then(|d| d.prop(c.value)).map(|p| p.name.clone()).unwrap_or_default();
    let base = if c.agg == Agg::Count || c.value == 0 { "records".to_owned() } else { format!("{} of {}", c.agg.short().to_lowercase(), prop) };
    let r = dd::effective_range(c, cx.global);
    if r == Range::All {
        base
    } else {
        format!("{base} · {}", r.label().to_lowercase())
    }
}

fn draw_number(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    if !ensure_db(cx, w, ui) {
        return;
    }
    let n = dd::number(cx.ws, &w.cfg, cx.global);
    let size = ui.available_size();
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter_at(r);
    let col = ch::series_color(w.cfg.palette, 0);
    let pct = w.cfg.agg.is_percent();
    let big = if pct { fmt_agg(n.value, true) } else if n.value.abs() >= 1e9 { dd::fmt_num(n.value) } else { fmt_full(n.value) };
    let fs = (r.height() * 0.5).clamp(22.0, 44.0);
    p.text(pos2(r.left() + 2.0, r.top() + fs * 0.62), Align2::LEFT_CENTER, &big, bold(fs), cx.pal.text);
    text_fit(&p, pos2(r.left() + 2.0, r.top() + fs * 0.62 + fs * 0.62 + 4.0), Align2::LEFT_TOP, &agg_caption(cx, w), body(12.0), cx.pal.muted, r.width() - 4.0);
    // trend vs. previous period
    let mut y = r.top() + fs * 1.3 + 22.0;
    if let Some(prev) = n.prev {
        let (txt, up) = if prev == 0.0 { (if n.value > 0.0 { "new".to_owned() } else { "no change".to_owned() }, n.value > 0.0) } else { (format!("{:+.0}%", (n.value - prev) / prev.abs() * 100.0), n.value >= prev) };
        let col = if n.value == prev { cx.pal.muted } else if up { tag_color(2) } else { cx.pal.danger };
        let tx = format!("{} {txt} vs previous", if n.value == prev { l::MINUS } else if up { l::TRENDING_UP } else { l::TRENDING_DOWN });
        text_fit(&p, pos2(r.left() + 2.0, y), Align2::LEFT_TOP, &tx, bold(12.0), col, r.width() - 4.0);
        y += 18.0;
    }
    // goal progress
    if w.cfg.goal > 0.0 && y + 14.0 <= r.bottom() {
        let frac = (n.value / w.cfg.goal).clamp(0.0, 1.0) as f32;
        let track = Rect::from_min_size(pos2(r.left() + 2.0, y + 2.0), vec2(r.width() - 4.0, 8.0));
        p.rect_filled(track, 4.0, with_alpha(cx.pal.muted, 0.18));
        p.rect_filled(Rect::from_min_size(track.min, vec2(track.width() * frac, 8.0)), 4.0, if n.value >= w.cfg.goal { tag_color(2) } else { col });
        text_fit(&p, pos2(r.left() + 2.0, y + 14.0), Align2::LEFT_TOP, &format!("{:.0}% of goal {}", n.value / w.cfg.goal * 100.0, dd::fmt_num(w.cfg.goal)), body(11.5), cx.pal.muted, r.width());
    }
    // sparkline of the trend (weekly buckets) when the widget has a date property
    if w.cfg.date != 0 && r.width() > 230.0 {
        let cfg = Cfg { group: w.cfg.date, stack: 0, bucket: Bucket::Week, limit: 14, cumulative: false, hide_empty: false, ..w.cfg.clone() };
        let ds = dd::grouped(cx.ws, &cfg, cx.global);
        if let Some(s) = ds.series.first() {
            ch::sparkline(&p, Rect::from_min_max(pos2(r.right() - (r.width() * 0.42).min(180.0), r.top() + 6.0), pos2(r.right() - 2.0, (r.top() + 6.0 + fs * 1.4).min(r.bottom()))), &s.vals, col);
        }
    }
    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
        let at = pointer(ui);
        open_drill(cx, agg_caption(cx, w), n.rows, at);
    }
}

fn draw_gauge(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    if !ensure_db(cx, w, ui) {
        return;
    }
    let n = dd::number(cx.ws, &w.cfg, cx.global);
    let goal = if w.cfg.goal > 0.0 { w.cfg.goal } else if w.cfg.agg.is_percent() { 100.0 } else { 0.0 };
    let size = ui.available_size();
    let val = if w.cfg.agg.is_percent() { fmt_agg(n.value, true) } else { dd::fmt_num(n.value) };
    if goal <= 0.0 {
        let (r, _) = ui.allocate_exact_size(size, Sense::hover());
        let p = ui.painter_at(r);
        p.text(r.center() - vec2(0.0, 10.0), Align2::CENTER_CENTER, val, bold(30.0), cx.pal.text);
        if ui.put(Rect::from_center_size(r.center() + vec2(0.0, 24.0), vec2(150.0, 26.0)), egui::Button::new(RichText::new("Set a goal…").font(body(12.5)))).clicked() {
            cx.configure = true;
        }
        return;
    }
    let cap = agg_caption(cx, w);
    let resp = ch::gauge(ui, cx.pal, Id::new(("dash_gauge", cx.wid)), size, n.value / goal, &val, &format!("{cap} · goal {}", dd::fmt_num(goal)), ch::series_color(w.cfg.palette, 0));
    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
        let at = pointer(ui);
        open_drill(cx, cap, n.rows, at);
    }
}

// ───────────────────────────── charts ─────────────────────────────

fn chart_widget(cx: &mut Cx, w: &mut Widget, ui: &mut Ui, kind: Kind, histogram: bool) {
    if !ensure_db(cx, w, ui) {
        return;
    }
    let ds = if histogram { dd::histogram(cx.ws, &w.cfg, cx.global) } else { dd::grouped(cx.ws, &w.cfg, cx.global) };
    if ds.is_empty() {
        let none = dd::rows(cx.ws, &w.cfg, cx.global).is_empty();
        if empty(ui, cx.pal, if none { "No records match the filters" } else if histogram { "Pick a number property" } else { "Nothing to plot yet" }, Some("Configure")) {
            cx.configure = true;
        }
        return;
    }
    let size = ui.available_size();
    let style = Style { kind, labels: w.cfg.labels, legend: w.cfg.legend, palette: w.cfg.palette };
    if let Some((s, c)) = ch::chart(ui, cx.pal, Id::new(("dash_chart", cx.wid)), size, &ds, &style) {
        let at = pointer(ui);
        let rows = if matches!(kind, Kind::Pie | Kind::Donut | Kind::Funnel) { ds.cat_rows(c) } else { ds.series[s].rows[c].clone() };
        let title = if ds.series.len() > 1 && !matches!(kind, Kind::Pie | Kind::Donut | Kind::Funnel) { format!("{} · {}", ds.cats[c], ds.series[s].name) } else { ds.cats[c].clone() };
        open_drill(cx, title, rows, at);
    }
}

fn draw_bar(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    chart_widget(cx, w, ui, Kind::Bar, false)
}
fn draw_hbar(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    chart_widget(cx, w, ui, Kind::HBar, false)
}
fn draw_stacked(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    chart_widget(cx, w, ui, Kind::Stacked, false)
}
fn draw_line(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    chart_widget(cx, w, ui, Kind::Line, false)
}
fn draw_area(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    chart_widget(cx, w, ui, Kind::Area, false)
}
fn draw_pie(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    chart_widget(cx, w, ui, Kind::Pie, false)
}
fn draw_donut(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    chart_widget(cx, w, ui, Kind::Donut, false)
}
fn draw_funnel(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    chart_widget(cx, w, ui, Kind::Funnel, false)
}
fn draw_radar(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    chart_widget(cx, w, ui, Kind::Radar, false)
}
fn draw_histogram(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    chart_widget(cx, w, ui, Kind::Bar, true)
}

fn prop_name(ws: &Workspace, db: u64, prop: u64) -> String {
    ws.db(db).and_then(|d| d.prop(prop)).map(|p| p.name.clone()).unwrap_or_else(|| "—".into())
}

fn draw_scatter(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    if !ensure_db(cx, w, ui) {
        return;
    }
    if w.cfg.x == 0 || w.cfg.y == 0 {
        if empty(ui, cx.pal, "Choose the X and Y number properties", Some("Configure")) {
            cx.configure = true;
        }
        return;
    }
    let sc = dd::scatter(cx.ws, &w.cfg, cx.global);
    let size = ui.available_size();
    let (xn, yn) = (prop_name(cx.ws, w.cfg.db, w.cfg.x), prop_name(cx.ws, w.cfg.db, w.cfg.y));
    let ws: &Workspace = &*cx.ws;
    let clicked = ch::scatter(ui, cx.pal, Id::new(("dash_chart", cx.wid)), size, &sc, w.cfg.palette, &xn, &yn, &|id| title_of(ws, id));
    if let Some(id) = clicked {
        cx.nav.out.open = Some(id);
    }
}

fn draw_heatmap(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    let m = dd::heat(cx.ws, &w.cfg, cx.global);
    let size = ui.available_size();
    let col = ch::series_color(w.cfg.palette, 0);
    if let Some(day) = ch::heatmap(ui, cx.pal, Id::new(("dash_chart", cx.wid)), size, &m, dates::today_day(), col) {
        let at = pointer(ui);
        let rows = m.get(&day).cloned().unwrap_or_default();
        open_drill(cx, dates::pretty(&dates::iso_of_day(day)), rows, at);
    }
}

// ───────────────────────────── records ─────────────────────────────

/// Paging state kept in egui memory: page index per widget.
fn page_state(ui: &Ui, wid: u64, total: usize, per: usize) -> usize {
    let id = Id::new(("dash_page", wid));
    let pages = total.div_ceil(per.max(1)).max(1);
    let cur: usize = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(0);
    cur.min(pages - 1)
}

fn set_page(ui: &Ui, wid: u64, page: usize) {
    ui.ctx().data_mut(|d| d.insert_temp(Id::new(("dash_page", wid)), page));
}

fn pager(ui: &mut Ui, pal: &Palette, wid: u64, page: usize, per: usize, total: usize, db: u64, nav: &mut Nav) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    let p = ui.painter_at(r);
    let pages = total.div_ceil(per.max(1)).max(1);
    let a = page * per + 1;
    let b = ((page + 1) * per).min(total);
    p.text(pos2(r.left() + 4.0, r.center().y), Align2::LEFT_CENTER, if total == 0 { "No records".to_owned() } else { format!("{a}–{b} of {total}") }, body(12.0), pal.muted);
    let mut x = r.right();
    for (k, (glyph, tip, enabled)) in [(l::CHEVRON_RIGHT, "Next", page + 1 < pages), (l::CHEVRON_LEFT, "Previous", page > 0)].into_iter().enumerate() {
        let br = Rect::from_min_size(pos2(x - 26.0, r.top()), vec2(24.0, 24.0));
        x -= 26.0;
        let resp = ui.interact(br, Id::new(("dash_pg", wid, k)), Sense::click());
        if enabled && resp.hovered() {
            p.rect_filled(br, 6.0, pal.hover);
        }
        p.text(br.center(), Align2::CENTER_CENTER, glyph, body(14.0), if enabled { pal.text } else { with_alpha(pal.muted, 0.4) });
        if resp.on_hover_text(tip).on_hover_cursor(if enabled { CursorIcon::PointingHand } else { CursorIcon::Default }).clicked() && enabled {
            set_page(ui, wid, if k == 0 { page + 1 } else { page - 1 });
        }
    }
    if db != 0 {
        let lr = Rect::from_min_size(pos2(x - 86.0, r.top()), vec2(84.0, 24.0));
        let resp = ui.interact(lr, Id::new(("dash_open_db", wid)), Sense::click());
        p.text(lr.center(), Align2::CENTER_CENTER, "Open all", body(12.0), if resp.hovered() { pal.accent } else { pal.muted });
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            nav.out.open = Some(db);
        }
    }
}

struct ColInfo {
    id: u64,
    name: String,
    kind: PropKind,
}

fn columns(cx: &Cx, w: &Widget, max: usize) -> Vec<ColInfo> {
    let Some(d) = cx.ws.db(w.cfg.db) else { return vec![] };
    let ids: Vec<u64> = if w.cfg.cols.is_empty() { d.props.iter().take(4).map(|p| p.id).collect() } else { w.cfg.cols.clone() };
    ids.into_iter().filter_map(|id| d.prop(id).map(|p| ColInfo { id: p.id, name: p.name.clone(), kind: p.kind })).take(max).collect()
}

/// Paints one cell; returns the checkbox toggle requested by a click.
fn cell(ui: &mut Ui, cx: &Cx, db: u64, row: &Page, col: &ColInfo, r: Rect, toggle: &mut Option<(u64, u64, bool)>) {
    let pal = cx.pal;
    let p = ui.painter_at(r);
    let Some(d) = cx.ws.db(db) else { return };
    let val = row.props.get(&col.id);
    match (col.kind, val) {
        (PropKind::Checkbox, v) => {
            let on = matches!(v, Some(Val::Check(true)));
            let b = Rect::from_center_size(pos2(r.left() + 14.0, r.center().y), vec2(16.0, 16.0));
            let resp = ui.interact(b.expand(5.0), Id::new(("dash_chk", row.id, col.id)), Sense::click());
            p.rect_filled(b, 4.0, if on { pal.accent } else { Color32::TRANSPARENT });
            p.rect_stroke(b, 4.0, Stroke::new(1.5_f32, if on { pal.accent } else { pal.muted }), egui::StrokeKind::Inside);
            if on {
                p.text(b.center(), Align2::CENTER_CENTER, l::CHECK, body(12.0), Color32::WHITE);
            }
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                *toggle = Some((row.id, col.id, !on));
            }
        }
        (PropKind::Select, Some(Val::Sel(o))) => {
            if let Some(opt) = d.opt(col.id, *o) {
                let c = tag_color(opt.color);
                let tw_ = p.layout_no_wrap(opt.name.clone(), body(11.5), c).size().x + 14.0;
                let pr = Rect::from_min_size(pos2(r.left() + 2.0, r.center().y - 10.0), vec2(tw_.min(r.width() - 4.0), 20.0));
                p.rect_filled(pr, 10.0, with_alpha(c, 0.16));
                text_fit(&p, pr.center(), Align2::CENTER_CENTER, &opt.name, body(11.5), c, pr.width() - 10.0);
            }
        }
        (PropKind::Number, Some(v)) | (PropKind::Formula, Some(v)) | (PropKind::Rollup, Some(v)) if matches!(v, Val::Num(_)) => {
            text_fit(&p, pos2(r.right() - 6.0, r.center().y), Align2::RIGHT_CENTER, &d.val_text(col.id, v), body(13.0), pal.text, r.width() - 8.0);
        }
        (PropKind::Date, Some(Val::Text(s))) => {
            text_fit(&p, pos2(r.left() + 2.0, r.center().y), Align2::LEFT_CENTER, &dates::pretty(s), body(12.5), pal.muted, r.width() - 6.0);
        }
        (PropKind::Relation, Some(Val::Multi(m))) => {
            let t = m.iter().map(|id| title_of(cx.ws, *id)).collect::<Vec<_>>().join(", ");
            text_fit(&p, pos2(r.left() + 2.0, r.center().y), Align2::LEFT_CENTER, &t, body(12.5), pal.muted, r.width() - 6.0);
        }
        (_, Some(v)) => {
            text_fit(&p, pos2(r.left() + 2.0, r.center().y), Align2::LEFT_CENTER, &d.val_text(col.id, v), body(12.5), pal.text, r.width() - 6.0);
        }
        _ => {}
    }
}

fn apply_toggle(cx: &mut Cx, t: Option<(u64, u64, bool)>) {
    if let Some((row, prop, on)) = t {
        if let Some(p) = cx.ws.page_mut(row) {
            p.props.insert(prop, Val::Check(on));
            p.updated = now_ms();
        }
    }
}

fn draw_table(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    if !ensure_db(cx, w, ui) {
        return;
    }
    let ids = dd::sorted_rows(cx.ws, &w.cfg, cx.global);
    let width = ui.available_width();
    let cols = columns(cx, w, (((width - 140.0) / 90.0).floor().max(0.0)) as usize);
    let rowh = 30.0;
    let per = fit_rows(ui, 26.0 + 24.0, rowh);
    let page = page_state(ui, cx.wid, ids.len(), per);
    let name_w = if cols.is_empty() { width } else { (width * 0.34).clamp(120.0, 260.0).min(width - 90.0 * cols.len() as f32).max(100.0) };
    let col_w = if cols.is_empty() { 0.0 } else { (width - name_w) / cols.len() as f32 };
    let (hr, _) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
    let p = ui.painter_at(hr);
    p.hline(hr.x_range(), hr.bottom(), Stroke::new(1.0_f32, cx.pal.border));
    let mut head: Vec<(u64, String, Rect, bool)> = vec![(dd::TITLE_SORT, "Name".into(), Rect::from_min_size(hr.min, vec2(name_w, 26.0)), false)];
    for (i, c) in cols.iter().enumerate() {
        head.push((c.id, c.name.clone(), Rect::from_min_size(pos2(hr.left() + name_w + col_w * i as f32, hr.top()), vec2(col_w, 26.0)), c.kind == PropKind::Number));
    }
    for (id, name, r, right) in head {
        let resp = ui.interact(r, Id::new(("dash_th", cx.wid, id)), Sense::click());
        let active = w.cfg.sort == id;
        let label = if active { format!("{name} {}", if w.cfg.asc { l::ARROW_UP } else { l::ARROW_DOWN }) } else { name };
        let col = if active { cx.pal.text } else { cx.pal.muted };
        if resp.hovered() {
            p.rect_filled(r.shrink2(vec2(0.0, 2.0)), 5.0, cx.pal.hover);
        }
        if right {
            text_fit(&p, pos2(r.right() - 6.0, r.center().y), Align2::RIGHT_CENTER, &label, bold(11.5), col, r.width() - 8.0);
        } else {
            text_fit(&p, pos2(r.left() + 4.0, r.center().y), Align2::LEFT_CENTER, &label, bold(11.5), col, r.width() - 8.0);
        }
        if resp.on_hover_text("Click to sort").on_hover_cursor(CursorIcon::PointingHand).clicked() {
            if active {
                w.cfg.asc = !w.cfg.asc;
            } else {
                w.cfg.sort = id;
                w.cfg.asc = true;
            }
            set_page(ui, cx.wid, 0);
        }
    }
    let mut toggle = None;
    for id in ids.iter().skip(page * per).take(per) {
        let Some(row) = cx.ws.page(*id) else { continue };
        let (r, resp) = ui.allocate_exact_size(vec2(width, rowh), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(r, 6.0, cx.pal.hover);
        }
        let pr = ui.painter_at(r);
        if !row.icon.is_empty() {
            pr.text(pos2(r.left() + 12.0, r.center().y), Align2::CENTER_CENTER, crate::icons::icon_text(&row.icon), body(14.0), cx.pal.text);
        }
        text_fit(&pr, pos2(r.left() + if row.icon.is_empty() { 4.0 } else { 26.0 }, r.center().y), Align2::LEFT_CENTER, &title_of(cx.ws, *id), bold(13.0), cx.pal.text, name_w - 30.0);
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            cx.nav.out.open = Some(*id);
        }
        for (i, c) in cols.iter().enumerate() {
            let cr = Rect::from_min_size(pos2(r.left() + name_w + col_w * i as f32, r.top()), vec2(col_w, rowh));
            cell(ui, cx, w.cfg.db, row, c, cr, &mut toggle);
        }
        ui.painter().hline(r.x_range(), r.bottom(), Stroke::new(1.0_f32, with_alpha(cx.pal.border, 0.6)));
    }
    apply_toggle(cx, toggle);
    pager(ui, cx.pal, cx.wid, page, per, ids.len(), w.cfg.db, cx.nav);
}

fn draw_list(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    if !ensure_db(cx, w, ui) {
        return;
    }
    let ids = dd::sorted_rows(cx.ws, &w.cfg, cx.global);
    let width = ui.available_width();
    let cols = columns(cx, w, 1);
    let per = fit_rows(ui, 24.0, ROW_H);
    let page = page_state(ui, cx.wid, ids.len(), per);
    let mut toggle = None;
    for id in ids.iter().skip(page * per).take(per) {
        let Some(row) = cx.ws.page(*id) else { continue };
        let (r, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(r, 7.0, cx.pal.hover);
        }
        let right_w = if cols.is_empty() { 0.0 } else { (width * 0.38).min(150.0) };
        let pr = ui.painter_at(r);
        text_fit(&pr, pos2(r.left() + 6.0, r.center().y), Align2::LEFT_CENTER, &format!("{} {}", if row.icon.is_empty() { String::new() } else { crate::icons::icon_text(&row.icon).to_owned() }, title_of(cx.ws, *id)).trim().to_owned(), body(13.5), cx.pal.text, width - right_w - 16.0);
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            cx.nav.out.open = Some(*id);
        }
        if let Some(c) = cols.first() {
            cell(ui, cx, w.cfg.db, row, c, Rect::from_min_size(pos2(r.right() - right_w, r.top()), vec2(right_w - 4.0, ROW_H)), &mut toggle);
        }
    }
    apply_toggle(cx, toggle);
    pager(ui, cx.pal, cx.wid, page, per, ids.len(), w.cfg.db, cx.nav);
}

fn draw_progress(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    if !ensure_db(cx, w, ui) {
        return;
    }
    if w.cfg.value == 0 {
        if empty(ui, cx.pal, "Choose the number property to track", Some("Configure")) {
            cx.configure = true;
        }
        return;
    }
    let ids = dd::sorted_rows(cx.ws, &w.cfg, cx.global);
    let goal = if w.cfg.goal > 0.0 { w.cfg.goal } else { 100.0 };
    let width = ui.available_width();
    let rowh = 38.0;
    let per = fit_rows(ui, 24.0, rowh);
    let page = page_state(ui, cx.wid, ids.len(), per);
    for id in ids.iter().skip(page * per).take(per) {
        let Some(row) = cx.ws.page(*id) else { continue };
        let v = dd::num_of(row, w.cfg.value).unwrap_or(0.0);
        let frac = (v / goal).clamp(0.0, 1.0) as f32;
        let (r, resp) = ui.allocate_exact_size(vec2(width, rowh), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(r, 7.0, cx.pal.hover);
        }
        let pr = ui.painter_at(r);
        text_fit(&pr, pos2(r.left() + 6.0, r.top() + 12.0), Align2::LEFT_CENTER, &title_of(cx.ws, *id), body(13.0), cx.pal.text, width - 70.0);
        pr.text(pos2(r.right() - 6.0, r.top() + 12.0), Align2::RIGHT_CENTER, format!("{}/{}", dd::fmt_num(v), dd::fmt_num(goal)), bold(11.5), cx.pal.muted);
        let track = Rect::from_min_size(pos2(r.left() + 6.0, r.bottom() - 13.0), vec2(width - 12.0, 7.0));
        pr.rect_filled(track, 3.5, with_alpha(cx.pal.muted, 0.18));
        pr.rect_filled(Rect::from_min_size(track.min, vec2(track.width() * frac, 7.0)), 3.5, if v >= goal { tag_color(2) } else { ch::series_color(w.cfg.palette, 0) });
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            cx.nav.out.open = Some(*id);
        }
    }
    pager(ui, cx.pal, cx.wid, page, per, ids.len(), w.cfg.db, cx.nav);
}

// ── board ──

struct BoardCol {
    opt: Option<u64>,
    name: String,
    color: Color32,
    rows: Vec<u64>,
}

fn draw_board(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    if !ensure_db(cx, w, ui) {
        return;
    }
    let (gp, opts): (u64, Vec<(u64, String, usize)>) = {
        let Some(d) = cx.ws.db(w.cfg.db) else { return };
        let Some(p) = d.prop(w.cfg.group).filter(|p| p.kind == PropKind::Select).or_else(|| d.props.iter().find(|p| p.kind == PropKind::Select)) else {
            drop(d);
            if empty(ui, cx.pal, "A board needs a Select property to make columns", Some("Configure")) {
                cx.configure = true;
            }
            return;
        };
        (p.id, p.opts.iter().map(|o| (o.id, o.name.clone(), o.color)).collect())
    };
    let ids = dd::rows(cx.ws, &w.cfg, cx.global);
    let mut cols: Vec<BoardCol> = opts.iter().map(|(id, n, c)| BoardCol { opt: Some(*id), name: n.clone(), color: tag_color(*c), rows: vec![] }).collect();
    let mut none = BoardCol { opt: None, name: "No value".into(), color: tag_color(0), rows: vec![] };
    for id in ids {
        match cx.ws.page(id).and_then(|r| r.props.get(&gp)) {
            Some(Val::Sel(o)) => match cols.iter_mut().find(|c| c.opt == Some(*o)) {
                Some(c) => c.rows.push(id),
                None => none.rows.push(id),
            },
            _ => none.rows.push(id),
        }
    }
    if !none.rows.is_empty() || cols.is_empty() {
        cols.push(none);
    }
    let avail = ui.available_size();
    let cw = (avail.x / cols.len() as f32).clamp(160.0, 280.0);
    let drag_id = Id::new(("dash_board_drag", cx.wid));
    let dragging: Option<u64> = ui.ctx().data(|d| d.get_temp(drag_id));
    let mut moved: Option<(u64, Option<u64>)> = None;
    let mut open: Option<u64> = None;
    let ptr = ui.ctx().pointer_interact_pos();
    let mut col_rects: Vec<Rect> = vec![];
    egui::ScrollArea::horizontal().id_salt(("dash_board_scroll", cx.wid)).auto_shrink([false, false]).max_height(avail.y).show(ui, |ui| {
        let (area, _) = ui.allocate_exact_size(vec2(cw * cols.len() as f32, avail.y - 12.0), Sense::hover());
        let p = ui.painter_at(area);
        for (ci, c) in cols.iter().enumerate() {
            let cr = Rect::from_min_size(pos2(area.left() + cw * ci as f32, area.top()), vec2(cw - 8.0, area.height()));
            col_rects.push(cr);
            let hot = dragging.is_some() && ptr.map_or(false, |q| cr.contains(q));
            p.rect_filled(cr, 10.0, with_alpha(cx.pal.muted, if hot { 0.2 } else { 0.07 }));
            if hot {
                p.rect_stroke(cr, 10.0, Stroke::new(1.5_f32, cx.pal.accent), egui::StrokeKind::Inside);
            }
            p.circle_filled(pos2(cr.left() + 14.0, cr.top() + 15.0), 4.5, c.color);
            text_fit(&p, pos2(cr.left() + 26.0, cr.top() + 15.0), Align2::LEFT_CENTER, &c.name, bold(12.5), cx.pal.text, cr.width() - 60.0);
            p.text(pos2(cr.right() - 10.0, cr.top() + 15.0), Align2::RIGHT_CENTER, c.rows.len().to_string(), body(12.0), cx.pal.muted);
            let cardh = 34.0;
            let fit = (((cr.height() - 36.0) / (cardh + 6.0)).floor() as usize).max(1);
            for (k, rid) in c.rows.iter().take(fit).enumerate() {
                let card = Rect::from_min_size(pos2(cr.left() + 6.0, cr.top() + 32.0 + k as f32 * (cardh + 6.0)), vec2(cr.width() - 12.0, cardh));
                let resp = ui.interact(card, Id::new(("dash_card", cx.wid, *rid)), Sense::click_and_drag());
                let lifted = dragging == Some(*rid);
                p.rect_filled(card, 8.0, if lifted { with_alpha(cx.pal.card, 0.4) } else { cx.pal.card });
                p.rect_stroke(card, 8.0, Stroke::new(1.0_f32, if resp.hovered() { cx.pal.accent } else { cx.pal.border }), egui::StrokeKind::Inside);
                text_fit(&p, pos2(card.left() + 10.0, card.center().y), Align2::LEFT_CENTER, &title_of(cx.ws, *rid), body(12.5), if lifted { cx.pal.muted } else { cx.pal.text }, card.width() - 18.0);
                if resp.drag_started() {
                    ui.ctx().data_mut(|d| d.insert_temp(drag_id, *rid));
                }
                if resp.on_hover_cursor(CursorIcon::Grab).clicked() {
                    open = Some(*rid);
                }
            }
            if c.rows.len() > fit {
                p.text(pos2(cr.center().x, cr.bottom() - 10.0), Align2::CENTER_CENTER, format!("+ {} more", c.rows.len() - fit), body(11.5), cx.pal.muted);
            }
        }
    });
    if let Some(rid) = dragging {
        if let Some(q) = ptr {
            // ghost card under the pointer
            let g = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("dash_ghost")));
            let r = Rect::from_center_size(q + vec2(0.0, 4.0), vec2(150.0, 30.0));
            g.rect_filled(r, 8.0, cx.pal.popup);
            g.rect_stroke(r, 8.0, Stroke::new(1.5_f32, cx.pal.accent), egui::StrokeKind::Inside);
            text_fit(&g, r.center(), Align2::CENTER_CENTER, &title_of(cx.ws, rid), body(12.5), cx.pal.text, 136.0);
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            ui.ctx().request_repaint();
        }
        if ui.ctx().input(|i| i.pointer.any_released()) || !ui.ctx().input(|i| i.pointer.any_down()) {
            ui.ctx().data_mut(|d| d.remove::<u64>(drag_id));
            if let Some(q) = ptr {
                if let Some(ci) = col_rects.iter().position(|r| r.contains(q)) {
                    let target = cols[ci].opt;
                    let from = cols.iter().find(|c| c.rows.contains(&rid)).and_then(|c| c.opt);
                    if target != from {
                        moved = Some((rid, target));
                    }
                }
            }
        }
    }
    if let Some((rid, target)) = moved {
        if let Some(p) = cx.ws.page_mut(rid) {
            match target {
                Some(o) => {
                    p.props.insert(gp, Val::Sel(o));
                }
                None => {
                    p.props.remove(&gp);
                }
            }
            p.updated = now_ms();
        }
    }
    if let Some(id) = open {
        if moved.is_none() {
            cx.nav.out.open = Some(id);
        }
    }
}

// ── calendar ──

fn draw_calendar(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    if !ensure_db(cx, w, ui) {
        return;
    }
    let date_prop = {
        let Some(d) = cx.ws.db(w.cfg.db) else { return };
        d.prop(w.cfg.date).filter(|p| p.kind == PropKind::Date).map(|p| p.id).or_else(|| d.first_of(PropKind::Date))
    };
    let Some(date_prop) = date_prop else {
        if empty(ui, cx.pal, "Add a Date property to this database to see it on a calendar", None) {}
        return;
    };
    let st_id = Id::new(("dash_cal", cx.wid));
    let (ty, tm, td) = dates::today();
    let (mut y, mut m): (i32, u32) = ui.ctx().data(|d| d.get_temp(st_id)).unwrap_or((ty, tm));
    let width = ui.available_width();
    // header
    let (hr, _) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
    let hp = ui.painter_at(hr);
    hp.text(pos2(hr.left() + 4.0, hr.center().y), Align2::LEFT_CENTER, format!("{} {y}", dates::MONTHS[m as usize - 1]), bold(14.0), cx.pal.text);
    let mut x = hr.right();
    for (k, (g, tip)) in [(l::CHEVRON_RIGHT, "Next month"), (l::CHEVRON_LEFT, "Previous month")].into_iter().enumerate() {
        let br = Rect::from_min_size(pos2(x - 28.0, hr.top()), vec2(26.0, 26.0));
        x -= 28.0;
        let resp = ui.interact(br, Id::new(("dash_calnav", cx.wid, k)), Sense::click());
        if resp.hovered() {
            hp.rect_filled(br, 6.0, cx.pal.hover);
        }
        hp.text(br.center(), Align2::CENTER_CENTER, g, body(14.0), cx.pal.text);
        if resp.on_hover_text(tip).on_hover_cursor(CursorIcon::PointingHand).clicked() {
            if k == 0 {
                (y, m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
            } else {
                (y, m) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
            }
        }
    }
    let tr = Rect::from_min_size(pos2(x - 52.0, hr.top()), vec2(50.0, 26.0));
    let resp = ui.interact(tr, Id::new(("dash_caltoday", cx.wid)), Sense::click());
    hp.rect_filled(tr, 13.0, if resp.hovered() { cx.pal.active } else { cx.pal.hover });
    hp.text(tr.center(), Align2::CENTER_CENTER, "Today", body(12.0), cx.pal.text);
    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
        (y, m) = (ty, tm);
    }
    ui.ctx().data_mut(|d| d.insert_temp(st_id, (y, m)));

    // records by day
    let first = dates::days_from_civil(y, m, 1);
    let dim = dates::days_in_month(y, m) as i64;
    let mut by_day: std::collections::BTreeMap<i64, Vec<u64>> = Default::default();
    let ids = dd::rows(cx.ws, &Cfg { date: date_prop, ..w.cfg.clone() }, Range::All);
    for id in ids {
        if let Some(Val::Text(s)) = cx.ws.page(id).and_then(|r| r.props.get(&date_prop)) {
            if let Some((a, b)) = dates::span(s) {
                for d in a.max(first)..=b.min(first + dim - 1) {
                    by_day.entry(d).or_default().push(id);
                }
            }
        }
    }
    let wd0 = dates::weekday(y, m, 1) as i64;
    let weeks = ((wd0 + dim) as f32 / 7.0).ceil() as usize;
    let avail_h = ui.available_height();
    let head_h = 18.0;
    let ch_ = ((avail_h - head_h) / weeks as f32).clamp(36.0, 120.0);
    let cw = width / 7.0;
    let (gr, _) = ui.allocate_exact_size(vec2(width, head_h + ch_ * weeks as f32), Sense::hover());
    let p = ui.painter_at(gr);
    for (i, n) in dates::WEEKDAYS.iter().enumerate() {
        p.text(pos2(gr.left() + cw * (i as f32 + 0.5), gr.top() + 9.0), Align2::CENTER_CENTER, n, body(11.0), cx.pal.muted);
    }
    for d in 0..dim {
        let slot = wd0 + d;
        let (cxi, cyi) = ((slot % 7) as f32, (slot / 7) as f32);
        let r = Rect::from_min_size(pos2(gr.left() + cw * cxi, gr.top() + head_h + ch_ * cyi), vec2(cw, ch_)).shrink(1.5);
        let day = first + d;
        let is_today = y == ty && m == tm && d as u32 + 1 == td;
        let items = by_day.get(&day);
        let resp = ui.interact(r, Id::new(("dash_calday", cx.wid, day)), Sense::click());
        p.rect_filled(r, 7.0, if resp.hovered() && items.is_some() { cx.pal.hover } else { with_alpha(cx.pal.muted, 0.05) });
        if is_today {
            p.rect_stroke(r, 7.0, Stroke::new(1.6_f32, cx.pal.accent), egui::StrokeKind::Inside);
        }
        p.text(pos2(r.left() + 6.0, r.top() + 8.0), Align2::LEFT_CENTER, (d + 1).to_string(), if is_today { bold(11.5) } else { body(11.5) }, if is_today { cx.pal.accent } else { cx.pal.muted });
        if let Some(items) = items {
            let lines = (((r.height() - 18.0) / 14.0).floor() as usize).max(0);
            for (k, id) in items.iter().take(lines.min(if ch_ < 56.0 { 1 } else { 3 })).enumerate() {
                let lr = Rect::from_min_size(pos2(r.left() + 3.0, r.top() + 18.0 + k as f32 * 14.0), vec2(r.width() - 6.0, 12.5));
                p.rect_filled(lr, 3.0, cx.pal.accent_soft);
                text_fit(&p, pos2(lr.left() + 4.0, lr.center().y), Align2::LEFT_CENTER, &title_of(cx.ws, *id), body(10.5), cx.pal.text, lr.width() - 6.0);
            }
            let shown = lines.min(if ch_ < 56.0 { 1 } else { 3 });
            if items.len() > shown {
                p.text(pos2(r.right() - 5.0, r.bottom() - 7.0), Align2::RIGHT_CENTER, format!("+{}", items.len() - shown), body(10.5), cx.pal.muted);
            }
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                let at = pointer(ui);
                open_drill(cx, dates::pretty(&dates::iso_of_day(day)), items.clone(), at);
            }
        }
    }
}

// ───────────────────────────── personal widgets ─────────────────────────────

fn draw_notes(_cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    let size = ui.available_size();
    ui.add_sized(size, egui::TextEdit::multiline(&mut w.cfg.text).frame(false).hint_text("Write something…").font(body(14.0)).desired_width(size.x));
}

fn draw_checklist(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    let done = w.cfg.items.iter().filter(|i| i.done).count();
    let total = w.cfg.items.len();
    let width = ui.available_width();
    if total > 0 {
        let (r, _) = ui.allocate_exact_size(vec2(width, 14.0), Sense::hover());
        let track = Rect::from_min_size(pos2(r.left() + 2.0, r.center().y - 3.0), vec2(r.width() - 44.0, 6.0));
        ui.painter().rect_filled(track, 3.0, with_alpha(cx.pal.muted, 0.2));
        ui.painter().rect_filled(Rect::from_min_size(track.min, vec2(track.width() * done as f32 / total as f32, 6.0)), 3.0, tag_color(2));
        ui.painter().text(pos2(r.right() - 2.0, r.center().y), Align2::RIGHT_CENTER, format!("{done}/{total}"), body(11.5), cx.pal.muted);
    }
    let mut remove: Option<usize> = None;
    let h = (ui.available_height() - 38.0).max(30.0);
    egui::ScrollArea::vertical().id_salt(("dash_check", cx.wid)).max_height(h).auto_shrink([false, false]).show(ui, |ui| {
        for (i, it) in w.cfg.items.iter_mut().enumerate() {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
            let cb = Rect::from_center_size(pos2(r.left() + 11.0, r.center().y), vec2(17.0, 17.0));
            let cr = ui.interact(cb.expand(5.0), Id::new(("dash_cl", cx.wid, i)), Sense::click());
            ui.painter().rect_filled(cb, 5.0, if it.done { cx.pal.accent } else { Color32::TRANSPARENT });
            ui.painter().rect_stroke(cb, 5.0, Stroke::new(1.6_f32, if it.done { cx.pal.accent } else { cx.pal.muted }), egui::StrokeKind::Inside);
            if it.done {
                ui.painter().text(cb.center(), Align2::CENTER_CENTER, l::CHECK, body(12.0), Color32::WHITE);
            }
            if cr.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                it.done = !it.done;
            }
            text_fit(ui.painter(), pos2(r.left() + 28.0, r.center().y), Align2::LEFT_CENTER, &it.text, body(13.5), if it.done { cx.pal.muted } else { cx.pal.text }, r.width() - 56.0);
            if it.done {
                let tw_ = ui.painter().layout_no_wrap(it.text.clone(), body(13.5), cx.pal.muted).size().x.min(r.width() - 56.0);
                ui.painter().hline(r.left() + 28.0..=r.left() + 28.0 + tw_, r.center().y, Stroke::new(1.0_f32, cx.pal.muted));
            }
            if resp.hovered() || ui.rect_contains_pointer(r) {
                let xr = Rect::from_center_size(pos2(r.right() - 12.0, r.center().y), vec2(22.0, 22.0));
                let xr_resp = ui.interact(xr, Id::new(("dash_clx", cx.wid, i)), Sense::click());
                ui.painter().text(xr.center(), Align2::CENTER_CENTER, l::X, body(13.0), if xr_resp.hovered() { cx.pal.danger } else { cx.pal.muted });
                if xr_resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    remove = Some(i);
                }
            }
        }
    });
    if let Some(i) = remove {
        w.cfg.items.remove(i);
    }
    // add field
    let nid = Id::new(("dash_cl_new", cx.wid));
    let mut text: String = ui.ctx().data(|d| d.get_temp(nid)).unwrap_or_default();
    let resp = ui.add(egui::TextEdit::singleline(&mut text).id(Id::new(("dash_cl_edit", cx.wid))).hint_text(format!("{} Add an item", l::PLUS)).desired_width(width).font(body(13.0)));
    if resp.lost_focus() && ui.ctx().input(|i| i.key_pressed(egui::Key::Enter)) && !text.trim().is_empty() {
        w.cfg.items.push(Item { text: text.trim().to_owned(), done: false });
        text.clear();
        resp.request_focus();
    }
    ui.ctx().data_mut(|d| d.insert_temp(nid, text));
}

fn draw_countdown(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    let width = ui.available_width();
    let days = dates::parse(&w.cfg.target).map(|t| dates::day_of(t) - dates::today_day());
    let (r, _) = ui.allocate_exact_size(vec2(width, (ui.available_height() - 34.0).max(40.0)), Sense::hover());
    let p = ui.painter_at(r);
    match days {
        Some(d) => {
            let (big, cap, col) = if d > 0 { (d.to_string(), if d == 1 { "day to go" } else { "days to go" }, cx.pal.text) } else if d == 0 { ("Today!".to_owned(), "it's the day", tag_color(2)) } else { ((-d).to_string(), if d == -1 { "day ago" } else { "days ago" }, cx.pal.muted) };
            p.text(pos2(r.center().x, r.center().y - 8.0), Align2::CENTER_CENTER, big, bold((r.height() * 0.5).clamp(24.0, 48.0)), col);
            p.text(pos2(r.center().x, r.center().y + (r.height() * 0.28).clamp(14.0, 28.0)), Align2::CENTER_CENTER, format!("{cap} · {}", dates::pretty(&w.cfg.target)), body(12.0), cx.pal.muted);
        }
        None => {
            p.text(r.center(), Align2::CENTER_CENTER, "Pick a date below  (YYYY-MM-DD)", body(12.5), cx.pal.muted);
        }
    }
    ui.add(egui::TextEdit::singleline(&mut w.cfg.target).hint_text("YYYY-MM-DD").desired_width(width).horizontal_align(egui::Align::Center).font(body(13.0)));
}

#[derive(Clone, Copy)]
struct TimerSt {
    mode: usize,
    running: bool,
    /// egui time at which the running timer started (adjusted for pauses).
    t0: f64,
    /// Seconds already elapsed when paused.
    done: f64,
}

const TIMER_MODES: [(&str, f64); 3] = [("Focus", 25.0 * 60.0), ("Break", 5.0 * 60.0), ("Long", 15.0 * 60.0)];

fn draw_timer(cx: &mut Cx, _w: &mut Widget, ui: &mut Ui) {
    let id = Id::new(("dash_timer", cx.wid));
    let now = ui.ctx().input(|i| i.time);
    let mut st: TimerSt = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(TimerSt { mode: 0, running: false, t0: 0.0, done: 0.0 });
    let total = TIMER_MODES[st.mode].1;
    let mut elapsed = if st.running { st.done + (now - st.t0) } else { st.done };
    if elapsed >= total {
        elapsed = total;
        st.running = false;
        st.done = total;
    }
    let left = (total - elapsed).max(0.0);
    let (r, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
    let p = ui.painter_at(r);
    let ring_r = (r.height() * 0.5 - 6.0).min(r.width() * 0.25).max(24.0);
    let center = pos2(r.left() + ring_r + 8.0, r.center().y);
    use std::f32::consts::{PI, TAU};
    ch::sector(&p, center, ring_r - 7.0, ring_r, 0.0, TAU, with_alpha(cx.pal.muted, 0.2));
    let frac = (elapsed / total) as f32;
    if frac > 0.002 {
        ch::sector(&p, center, ring_r - 7.0, ring_r, -PI / 2.0, -PI / 2.0 + TAU * frac.min(0.999), if st.mode == 0 { cx.pal.accent } else { tag_color(2) });
    }
    p.text(center, Align2::CENTER_CENTER, format!("{:02}:{:02}", (left / 60.0).floor() as u32, (left % 60.0).floor() as u32), bold((ring_r * 0.42).clamp(13.0, 22.0)), cx.pal.text);
    // controls
    let bx = center.x + ring_r + 14.0;
    let bw = (r.right() - bx).max(60.0);
    let mut y = r.top() + 4.0;
    for (i, (n, secs)) in TIMER_MODES.iter().enumerate() {
        let br = Rect::from_min_size(pos2(bx + (bw / 3.0) * i as f32, y), vec2(bw / 3.0 - 3.0, 22.0));
        let resp = ui.interact(br, Id::new(("dash_tm", cx.wid, i)), Sense::click());
        p.rect_filled(br, 11.0, if st.mode == i { cx.pal.accent_soft } else if resp.hovered() { cx.pal.hover } else { Color32::TRANSPARENT });
        p.text(br.center(), Align2::CENTER_CENTER, n, body(11.5), if st.mode == i { cx.pal.accent } else { cx.pal.muted });
        if resp.on_hover_text(format!("{} min", secs / 60.0)).on_hover_cursor(CursorIcon::PointingHand).clicked() {
            st = TimerSt { mode: i, running: false, t0: 0.0, done: 0.0 };
        }
    }
    y += 32.0;
    let finished = elapsed >= total;
    for (k, (label, primary)) in [(if finished { "Done" } else if st.running { "Pause" } else { "Start" }, true), ("Reset", false)].into_iter().enumerate() {
        let br = Rect::from_min_size(pos2(bx + (bw / 2.0) * k as f32, y), vec2(bw / 2.0 - 4.0, 28.0));
        let resp = ui.interact(br, Id::new(("dash_tb", cx.wid, k)), Sense::click());
        p.rect_filled(br, 14.0, if primary { if resp.hovered() { cx.pal.accent.gamma_multiply(0.85) } else { cx.pal.accent } } else if resp.hovered() { cx.pal.active } else { cx.pal.hover });
        p.text(br.center(), Align2::CENTER_CENTER, label, bold(12.0), if primary { Color32::WHITE } else { cx.pal.text });
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            if primary {
                if finished {
                    st.done = 0.0;
                    st.running = false;
                } else if st.running {
                    st.done += now - st.t0;
                    st.running = false;
                } else {
                    st.t0 = now;
                    st.running = true;
                }
            } else {
                st.running = false;
                st.done = 0.0;
            }
        }
    }
    if st.running {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, st));
}

fn draw_today(cx: &mut Cx, _w: &mut Widget, ui: &mut Ui) {
    let (y, m, d) = dates::today();
    let wd = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"][dates::weekday(y, m, d) as usize];
    let doy = dates::days_from_civil(y, m, d) - dates::days_from_civil(y, 1, 1) + 1;
    let diy = dates::days_from_civil(y + 1, 1, 1) - dates::days_from_civil(y, 1, 1);
    let (r, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
    let p = ui.painter_at(r);
    p.text(pos2(r.left() + 2.0, r.top() + 22.0), Align2::LEFT_CENTER, wd, bold((r.height() * 0.3).clamp(20.0, 30.0)), cx.pal.text);
    p.text(pos2(r.left() + 2.0, r.top() + 22.0 + (r.height() * 0.28).clamp(18.0, 28.0)), Align2::LEFT_CENTER, format!("{} {d}, {y}", dates::MONTHS[m as usize - 1]), body(14.0), cx.pal.muted);
    let track = Rect::from_min_size(pos2(r.left() + 2.0, r.bottom() - 22.0), vec2(r.width() - 4.0, 7.0));
    p.rect_filled(track, 3.5, with_alpha(cx.pal.muted, 0.2));
    p.rect_filled(Rect::from_min_size(track.min, vec2(track.width() * doy as f32 / diy as f32, 7.0)), 3.5, cx.pal.accent);
    p.text(pos2(r.left() + 2.0, r.bottom() - 6.0), Align2::LEFT_CENTER, format!("Day {doy} of {diy} · {:.0}% of {y}", doy as f32 / diy as f32 * 100.0), body(11.5), cx.pal.muted);
}

fn draw_quicklinks(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    let list: Vec<u64> = w.cfg.pages.iter().copied().filter(|id| cx.ws.page(*id).map_or(false, |p| !p.trashed)).collect();
    if list.is_empty() {
        if empty(ui, cx.pal, "Pin the pages you open most", Some("Choose pages")) {
            cx.configure = true;
        }
        return;
    }
    let fit = fit_rows(ui, 0.0, ROW_H - 2.0);
    for id in list.into_iter().take(fit) {
        let icon = cx.ws.page(id).map(|p| p.icon.clone()).unwrap_or_default();
        let t = title_of(cx.ws, id);
        let t = if icon.is_empty() { t } else { format!("{}  {t}", crate::icons::icon_text(&icon)) };
        if link_row(ui, cx.pal, Id::new(("dash_ql", id)), &t, "", cx.pal.muted) {
            cx.nav.out.open = Some(id);
        }
    }
}

fn draw_pagepreview(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    let Some(id) = w.cfg.pages.first().copied().filter(|id| cx.ws.page(*id).map_or(false, |p| !p.trashed)) else {
        if empty(ui, cx.pal, "Choose a page to preview", Some("Choose page")) {
            cx.configure = true;
        }
        return;
    };
    let Some(page) = cx.ws.page(id) else { return };
    let (r, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click());
    let p = ui.painter_at(r);
    let mut y = r.top() + 2.0;
    text_fit(&p, pos2(r.left() + 2.0, y + 11.0), Align2::LEFT_CENTER, &format!("{} {}", crate::icons::icon_text(&page.icon), title_of(cx.ws, id)).trim().to_owned(), bold(15.0), cx.pal.text, r.width() - 6.0);
    y += 30.0;
    if page.db.is_some() {
        p.text(pos2(r.left() + 2.0, y + 8.0), Align2::LEFT_CENTER, format!("Database · {} records", cx.ws.rows(id).len()), body(12.5), cx.pal.muted);
    }
    for b in page.blocks.iter().filter(|b| b.kind.has_text() && !b.text.trim().is_empty()) {
        if y + 18.0 > r.bottom() {
            p.text(pos2(r.left() + 2.0, r.bottom() - 6.0), Align2::LEFT_CENTER, "…", body(13.0), cx.pal.muted);
            break;
        }
        let (font, col) = match b.kind {
            BlockKind::H1 | BlockKind::H2 | BlockKind::H3 => (bold(14.0), cx.pal.text),
            _ => (body(13.0), cx.pal.muted),
        };
        let prefix = match b.kind {
            BlockKind::Bullet => "• ",
            BlockKind::Todo => if b.checked { "\u{e559} " } else { "\u{e167} " },
            _ => "",
        };
        let plain = crate::rich::strip(&b.text);
        let mut job = egui::text::LayoutJob::simple(format!("{prefix}{plain}"), font, col, r.width() - 6.0);
        job.wrap.max_rows = 2;
        let g = p.layout_job(job);
        let hh = g.size().y;
        p.galley(pos2(r.left() + 2.0, y), g, col);
        y += hh + 5.0;
    }
    if resp.on_hover_text("Open page").on_hover_cursor(CursorIcon::PointingHand).clicked() {
        cx.nav.out.open = Some(id);
    }
    let _ = page_text;
    let _ = PageKind::Doc;
}

fn draw_quickadd(cx: &mut Cx, w: &mut Widget, ui: &mut Ui) {
    let width = ui.available_width();
    let sid = Id::new(("dash_qa", cx.wid));
    let (mut text, flash): (String, f64) = ui.ctx().data(|d| d.get_temp(sid)).unwrap_or_default();
    let now = ui.ctx().input(|i| i.time);
    let db_ok = cx.ws.db(w.cfg.db).is_some() && cx.ws.page(w.cfg.db).map_or(false, |p| !p.trashed);
    let target = if db_ok { format!("Add a record to “{}”", title_of(cx.ws, w.cfg.db)) } else { "Add a new page".to_owned() };
    ui.label(RichText::new(target).font(body(12.0)).color(cx.pal.muted));
    let resp = ui.add(egui::TextEdit::singleline(&mut text).id(Id::new(("dash_qa_edit", cx.wid))).hint_text("Type a title and press Enter").desired_width(width).font(body(14.0)));
    let mut add = resp.lost_focus() && ui.ctx().input(|i| i.key_pressed(egui::Key::Enter));
    ui.horizontal(|ui| {
        if ui.add(egui::Button::new(RichText::new(format!("{} Add", l::PLUS)).font(bold(12.5)).color(Color32::WHITE)).fill(cx.pal.accent).corner_radius(14)).on_hover_cursor(CursorIcon::PointingHand).clicked() {
            add = true;
        }
        if db_ok && ui.add(egui::Button::new(RichText::new(format!("{} Page", l::FILE_PLUS)).font(body(12.5))).corner_radius(14)).on_hover_cursor(CursorIcon::PointingHand).clicked() {
            let id = cx.ws.add_page(None, PageKind::Doc);
            if let Some(p) = cx.ws.page_mut(id) {
                p.title = text.trim().to_owned();
            }
            cx.nav.out.open = Some(id);
            text.clear();
        }
        if now - flash < 2.5 {
            ui.label(RichText::new(format!("{} Added", l::CHECK)).font(bold(12.5)).color(tag_color(2)));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
        }
    });
    let mut flash = flash;
    if add && !text.trim().is_empty() {
        let id = if db_ok { cx.ws.add_row(w.cfg.db, None) } else { cx.ws.add_page(None, PageKind::Doc) };
        if let Some(p) = cx.ws.page_mut(id) {
            p.title = text.trim().to_owned();
            p.updated = now_ms();
        }
        text.clear();
        flash = now;
        resp.request_focus();
    }
    ui.ctx().data_mut(|d| d.insert_temp(sid, (text, flash)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_consistent() {
        let defs = all();
        let mut kinds: Vec<&str> = defs.iter().map(|d| d.kind).collect();
        kinds.sort_unstable();
        let n = kinds.len();
        kinds.dedup();
        assert_eq!(kinds.len(), n, "widget kinds are unique");
        assert!(n >= 30, "{n} widget types");
        for d in &defs {
            assert!((1..=12).contains(&d.span) && (1..=8).contains(&d.rows), "{} default size", d.kind);
            assert!(!d.name.is_empty() && !d.desc.is_empty());
        }
        for k in CHART_KINDS {
            assert!(def(k).is_some(), "{k} is registered");
        }
    }

    #[test]
    fn plugins_can_register() {
        fn nop(_: &mut Cx, _: &mut Widget, _: &mut Ui) {}
        register(Def { kind: "future", name: "Future", desc: "A later feature", cat: Cat::Data, glyph: l::ZAP, span: 4, rows: 2, opts: &[Db], draw: nop });
        assert!(def("future").is_some());
        register(Def { kind: "future", name: "Future 2", desc: "x", cat: Cat::Data, glyph: l::ZAP, span: 4, rows: 2, opts: &[], draw: nop });
        assert_eq!(all().iter().filter(|d| d.kind == "future").count(), 1);
        assert_eq!(def("future").unwrap().name, "Future 2");
    }
}

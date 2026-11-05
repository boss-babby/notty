//! Dashboard editing UI: the widget gallery ("Add widget") and the settings inspector (a docked panel with live preview).
//!
//! The inspector builds its form from `Def::opts`, so a widget type added later only lists the settings it understands.

use egui::{pos2, vec2, Align2, Area, Color32, CursorIcon, Id, Order, Rect, RichText, ScrollArea, Sense, Stroke, Ui};

use crate::dashcfg::{self as cfg, Agg, Bucket, Cat, Cfg, Opt, Range, Widget};
use crate::dashcharts::{series_color, PALETTE_NAMES};
use crate::dashdata as dd;
use crate::dashwidgets::{self as dw, Def};
use crate::lucide as l;
use crate::model::{Database, Filter, FilterOp, PropKind, Workspace};
use crate::theme::{bold, body, Palette};
use crate::widgets::{popup_frame, section_label, text_fit, with_alpha};

// ───────────────────────────── creating widgets ─────────────────────────────

/// Adds a widget of `kind` to board `board` (0 = the active Home board) with settings chosen from what the workspace contains.
/// `"chart"` (or any plot type such as `"line"`) creates the unified Chart widget with that plot type.
pub fn create(ws: &mut Workspace, kind: &str, board: u64) -> Option<u64> {
    let kind = if kind == "chart" { "bar" } else { kind };
    let d = dw::def(kind)?;
    let mut c = Cfg::default();
    if d.opts.contains(&Opt::Db) {
        if let Some(db) = cfg::best_db(ws) {
            cfg::autofill(ws, &mut c, db);
        }
    }
    tune(ws, &mut c, kind);
    match kind {
        "heatmap" => c.group = 0,
        "countdown" => c.target = dates_in(30),
        "pagepreview" | "quicklinks" => {
            c.pages = ws.pages.iter().filter(|p| !p.trashed && !p.row && p.favorite).map(|p| p.id).take(if kind == "pagepreview" { 1 } else { 5 }).collect();
            if c.pages.is_empty() {
                c.pages = ws.pages.iter().filter(|p| !p.trashed && !p.row).map(|p| p.id).take(if kind == "pagepreview" { 1 } else { 4 }).collect();
            }
        }
        "notes" => c.text = String::new(),
        _ => {}
    }
    Some(ws.dash.add_widget_to(board, cfg::new_widget(kind, d.span, d.rows, c)))
}

/// Gives the settings a plot type needs a sensible value (only fills what is still empty, so it is safe when the user switches plots).
pub fn tune(ws: &Workspace, c: &mut Cfg, plot: &str) {
    let props = ws.db(c.db).map(|x| x.props.clone()).unwrap_or_default();
    match plot {
        "line" | "area" => {
            if !props.iter().any(|p| p.id == c.group && p.kind == PropKind::Date) {
                if let Some(p) = props.iter().find(|p| p.kind == PropKind::Date) {
                    c.group = p.id;
                    c.bucket = Bucket::Month;
                }
            }
        }
        "stacked" => {
            if c.stack == 0 {
                if let Some(p) = props.iter().find(|p| matches!(p.kind, PropKind::Select | PropKind::MultiSelect) && p.id != c.group) {
                    c.stack = p.id;
                }
            }
        }
        "gauge" => {
            if c.goal <= 0.0 {
                let v = dd::number(ws, c, Range::All).value;
                c.goal = if c.agg.is_percent() { 100.0 } else { crate::dashcharts::nice_scale((v * 1.25).max(4.0)).0 };
            }
        }
        "progress" => {
            if c.goal <= 0.0 {
                c.goal = 100.0
            }
        }
        "scatter" => {
            if c.x == 0 && c.y == 0 {
                let nums: Vec<u64> = props.iter().filter(|p| numeric(p.kind)).map(|p| p.id).collect();
                if nums.len() >= 2 {
                    c.x = nums[0];
                    c.y = nums[1];
                }
            }
        }
        _ => {}
    }
}

fn dates_in(days: i64) -> String {
    crate::dates::iso_of_day(crate::dates::today_day() + days)
}

// ───────────────────────────── gallery ─────────────────────────────

#[derive(Default)]
pub struct Gallery {
    pub q: String,
    pub cat: Option<Cat>,
    /// Board that receives the widget (0 = the active Home board; otherwise the board of the page being edited).
    pub board: u64,
    pub opened: bool,
}

pub enum GalleryResult {
    Open,
    Close,
    Added(u64),
}

fn matches_query(d: &Def, q: &str) -> bool {
    let q = q.trim().to_lowercase();
    q.is_empty() || d.name.to_lowercase().contains(&q) || d.desc.to_lowercase().contains(&q) || d.kind.contains(&q)
}

pub fn gallery(ctx: &egui::Context, ws: &mut Workspace, pal: &Palette, g: &mut Gallery, _area: Rect) -> GalleryResult {
    let screen = ctx.screen_rect();
    let dw_ = 700.0f32.min(screen.width() - 20.0);
    let dh = (screen.height() - 60.0).min(600.0).max(260.0);
    let dialog = Rect::from_center_size(screen.center(), vec2(dw_, dh));
    let mut result = GalleryResult::Open;
    let t = crate::widgets::appear(ctx, Id::new("dash_gallery_in"), 0.18);
    let first = !g.opened;
    g.opened = true;
    // the individual plot types live inside the unified Chart widget
    let defs: Vec<Def> = dw::all().into_iter().filter(|d| !cfg::PLOT_KINDS.contains(&d.kind)).collect();
    Area::new(Id::new("dash_gallery")).order(Order::Foreground).fixed_pos(screen.min).constrain(false).show(ctx, |ui| {
        ui.set_clip_rect(screen);
        let (full, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
        ui.painter().rect_filled(full, 0.0, Color32::from_black_alpha((110.0 * t) as u8));
        let dr = dialog.translate(vec2(0.0, (1.0 - t) * 10.0));
        ui.painter().rect_filled(dr.translate(vec2(0.0, 6.0)).expand(2.0), 18.0, Color32::from_black_alpha(40));
        ui.painter().rect_filled(dr, 16.0, pal.popup);
        ui.painter().rect_stroke(dr, 16.0, Stroke::new(1.0_f32, pal.border), egui::StrokeKind::Inside);
        let inner = dr.shrink(18.0);
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(inner).id_salt("dash_gallery_ui").layout(egui::Layout::top_down(egui::Align::Min)));
        c.set_clip_rect(dr.intersect(screen));
        // header
        c.horizontal(|ui| {
            ui.label(RichText::new("Add a widget").font(bold(19.0)).color(pal.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (r, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(r, 14.0, pal.hover);
                }
                ui.painter().text(r.center(), Align2::CENTER_CENTER, l::X, body(16.0), pal.muted);
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    result = GalleryResult::Close;
                }
            });
        });
        c.label(RichText::new("Charts, numbers, tables and more — every widget is interactive and configurable.").font(body(12.5)).color(pal.muted));
        c.add_space(8.0);
        let sresp = c.add(egui::TextEdit::singleline(&mut g.q).id(Id::new("dash_gallery_q")).hint_text("Search widgets…").desired_width(inner.width()).font(body(14.0)));
        if first {
            sresp.request_focus();
        }
        c.add_space(8.0);
        c.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            let mut chip = |ui: &mut Ui, text: &str, on: bool| -> bool {
                let gl = ui.painter().layout_no_wrap(text.to_owned(), body(12.5), pal.text);
                let (r, resp) = ui.allocate_exact_size(vec2(gl.size().x + 20.0, 26.0), Sense::click());
                ui.painter().rect_filled(r, 13.0, if on { pal.accent_soft } else if resp.hovered() { pal.hover } else { with_alpha(pal.muted, 0.08) });
                ui.painter().text(r.center(), Align2::CENTER_CENTER, text, if on { bold(12.5) } else { body(12.5) }, if on { pal.accent } else { pal.text });
                resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
            };
            if chip(ui, "All", g.cat.is_none()) {
                g.cat = None;
            }
            for cat in Cat::ALL {
                if chip(ui, cat.label(), g.cat == Some(cat)) {
                    g.cat = Some(cat);
                }
            }
        });
        c.add_space(6.0);
        let list_h = (inner.bottom() - c.cursor().top() - 4.0).max(80.0);
        let mut pick: Option<&'static str> = None;
        ScrollArea::vertical().id_salt("dash_gallery_scroll").max_height(list_h).auto_shrink([false, false]).show(&mut c, |ui| {
            let w = ui.available_width();
            let ncols = if w >= 560.0 { 2 } else { 1 };
            let gap = 10.0;
            let cw = (w - gap * (ncols as f32 - 1.0)) / ncols as f32;
            let mut any = false;
            for cat in Cat::ALL {
                if g.cat.is_some() && g.cat != Some(cat) {
                    continue;
                }
                let items: Vec<&Def> = defs.iter().filter(|d| d.cat == cat && matches_query(d, &g.q)).collect();
                if items.is_empty() {
                    continue;
                }
                any = true;
                section_label(ui, pal, &cat.label().to_uppercase());
                for chunk in items.chunks(ncols) {
                    let (rr, _) = ui.allocate_exact_size(vec2(w, 74.0), Sense::hover());
                    for (ci, d) in chunk.iter().enumerate() {
                        let r = Rect::from_min_size(rr.min + vec2((cw + gap) * ci as f32, 0.0), vec2(cw, 74.0));
                        let resp = ui.interact(r, Id::new(("dash_gal_card", d.kind)), Sense::click());
                        let p = ui.painter();
                        p.rect_filled(r, 12.0, if resp.hovered() { pal.hover } else { with_alpha(pal.muted, 0.05) });
                        p.rect_stroke(r, 12.0, Stroke::new(1.0_f32, if resp.hovered() { pal.accent } else { pal.border }), egui::StrokeKind::Inside);
                        let ir = Rect::from_min_size(r.min + vec2(12.0, 14.0), vec2(46.0, 46.0));
                        p.rect_filled(ir, 12.0, pal.accent_soft);
                        p.text(ir.center(), Align2::CENTER_CENTER, d.glyph, body(22.0), pal.accent);
                        text_fit(p, pos2(r.left() + 70.0, r.top() + 20.0), Align2::LEFT_CENTER, d.name, bold(14.0), pal.text, cw - 82.0);
                        let mut job = egui::text::LayoutJob::simple(d.desc.to_owned(), body(12.0), pal.muted, cw - 84.0);
                        job.wrap.max_rows = 2;
                        let gl = p.layout_job(job);
                        p.galley(pos2(r.left() + 70.0, r.top() + 34.0), gl, pal.muted);
                        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            pick = Some(d.kind);
                        }
                    }
                    ui.add_space(gap);
                }
            }
            if !any {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("No widget matches your search").font(body(14.0)).color(pal.muted)));
            }
        });
        if let Some(kind) = pick {
            if let Some(id) = create(ws, kind, g.board) {
                result = GalleryResult::Added(id);
            }
        }
        if resp.clicked() && !ctx.pointer_interact_pos().map_or(false, |p| dr.contains(p)) {
            result = GalleryResult::Close;
        }
    });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        result = GalleryResult::Close;
    }
    if !matches!(result, GalleryResult::Open) {
        crate::widgets::reset_appear(ctx, Id::new("dash_gallery_in"));
    }
    result
}

// ───────────────────────────── inspector ─────────────────────────────

fn pick<T: Clone + PartialEq>(ui: &mut Ui, pal: &Palette, id: &str, label: &str, cur: &T, options: &[(T, String)]) -> Option<T> {
    ui.add_space(6.0);
    ui.label(RichText::new(label).font(body(12.0)).color(pal.muted));
    let text = options.iter().find(|o| &o.0 == cur).map(|o| o.1.clone()).unwrap_or_else(|| "—".to_owned());
    let mut chosen = None;
    egui::ComboBox::from_id_salt(("dash_pick", id)).selected_text(text).width(ui.available_width() - 6.0).show_ui(ui, |ui| {
        ui.set_min_width(220.0);
        for (v, name) in options {
            if ui.selectable_label(v == cur, name).clicked() {
                chosen = Some(v.clone());
            }
        }
    });
    chosen
}

fn props_where(d: &Database, f: impl Fn(PropKind) -> bool) -> Vec<(u64, String)> {
    d.props.iter().filter(|p| f(p.kind)).map(|p| (p.id, p.name.clone())).collect()
}

fn with_none(mut v: Vec<(u64, String)>, none: &str) -> Vec<(u64, String)> {
    v.insert(0, (0, none.to_owned()));
    v
}

fn numeric(k: PropKind) -> bool {
    matches!(k, PropKind::Number | PropKind::Formula | PropKind::Rollup | PropKind::Checkbox)
}

fn check(ui: &mut Ui, value: &mut bool, text: &str) {
    ui.add_space(2.0);
    ui.checkbox(value, text);
}

/// Docked settings panel for widget `id`. Returns false when it was closed.
pub fn inspector(ctx: &egui::Context, ws: &mut Workspace, pal: &Palette, id: u64, area: Rect) -> bool {
    let Some(orig) = ws.dash.widget(id).cloned() else { return false };
    let def = dw::def_for(&orig);
    let plot = cfg::plot_of(&orig);
    let opts: &[Opt] = def.as_ref().map_or(&[], |d| d.opts);
    let mut w: Widget = orig.clone();
    let mut keep = true;
    let mut remove = false;
    let width = 352.0f32.min(area.width() - 8.0);
    let h = area.height() - 8.0;
    let pos = pos2(area.right() - width - 4.0, area.top() + 4.0);
    let t = crate::widgets::appear(ctx, Id::new(("dash_insp_in", id)), 0.16);
    let db = ws.db(w.cfg.db).cloned();
    Area::new(Id::new("dash_inspector")).order(Order::Foreground).fixed_pos(pos + vec2((1.0 - t) * 24.0, 0.0)).constrain(false).show(ctx, |ui| {
        popup_frame(pal).inner_margin(egui::Margin::same(14)).show(ui, |ui| {
            ui.set_width(width - 28.0);
            ui.set_height(h - 28.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(if plot.is_some() { "Chart" } else { def.as_ref().map_or("Widget", |d| d.name) }).font(bold(16.0)).color(pal.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(r, 13.0, pal.hover);
                    }
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, l::X, body(15.0), pal.muted);
                    if resp.on_hover_text("Close settings").on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        keep = false;
                    }
                });
            });
            if let Some(d) = &def {
                ui.label(RichText::new(d.desc).font(body(12.0)).color(pal.muted));
            }
            ui.add_space(4.0);
            ScrollArea::vertical().id_salt("dash_insp_scroll").auto_shrink([false, false]).max_height(h - 100.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                // ── plot type (unified Chart widget) ──
                if let Some(cur) = &plot {
                    section_label(ui, pal, "PLOT TYPE");
                    let cols = 3usize;
                    let gap = 6.0;
                    let tw_ = (ui.available_width() - 6.0 - gap * (cols as f32 - 1.0)) / cols as f32;
                    for row in dw::CHART_KINDS.chunks(cols) {
                        let (rr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 50.0), Sense::hover());
                        for (ci, k) in row.iter().enumerate() {
                            let r = Rect::from_min_size(rr.min + vec2((tw_ + gap) * ci as f32, 0.0), vec2(tw_, 46.0));
                            let on = cur == k;
                            let resp = ui.interact(r, Id::new(("dash_plot", *k)), Sense::click());
                            let p = ui.painter();
                            p.rect_filled(r, 10.0, if on { pal.accent_soft } else if resp.hovered() { pal.hover } else { with_alpha(pal.muted, 0.07) });
                            if on {
                                p.rect_stroke(r, 10.0, Stroke::new(1.5_f32, pal.accent), egui::StrokeKind::Inside);
                            }
                            let g = dw::def(k).map_or(l::CHART_COLUMN, |d| d.glyph);
                            p.text(pos2(r.center().x, r.top() + 15.0), Align2::CENTER_CENTER, g, body(17.0), if on { pal.accent } else { pal.text });
                            p.text(pos2(r.center().x, r.bottom() - 11.0), Align2::CENTER_CENTER, dw::plot_label(k), if on { bold(11.5) } else { body(11.5) }, if on { pal.accent } else { pal.muted });
                            if resp.on_hover_text(dw::def(k).map_or("", |d| d.desc)).on_hover_cursor(CursorIcon::PointingHand).clicked() && !on {
                                w.cfg.extra.insert("plot".into(), (*k).to_owned());
                                tune(ws, &mut w.cfg, k);
                                if let Some(d) = dw::def(k) {
                                    w.span = d.span;
                                    w.rows = d.rows;
                                }
                            }
                        }
                    }
                }
                // ── title & size ──
                section_label(ui, pal, "GENERAL");
                ui.add(egui::TextEdit::singleline(&mut w.title).hint_text("Title (automatic)").desired_width(ui.available_width() - 6.0).font(body(14.0)));
                ui.add_space(6.0);
                ui.label(RichText::new("Width").font(body(12.0)).color(pal.muted));
                ui.horizontal_wrapped(|ui| {
                    for (n, s) in [("1/4", 3u8), ("1/3", 4), ("1/2", 6), ("2/3", 8), ("Full", 12)] {
                        if size_chip(ui, pal, n, w.span == s) {
                            w.span = s;
                        }
                    }
                });
                ui.label(RichText::new("Height").font(body(12.0)).color(pal.muted));
                ui.horizontal_wrapped(|ui| {
                    for (n, r) in [("XS", 1u8), ("S", 2), ("M", 3), ("L", 4), ("XL", 6), ("XXL", 8)] {
                        if size_chip(ui, pal, n, w.rows == r) {
                            w.rows = r;
                        }
                    }
                });

                // ── data source ──
                let has = |o: Opt| opts.contains(&o);
                if has(Opt::Db) {
                    section_label(ui, pal, "DATA");
                    let dbs: Vec<(u64, String)> = cfg::databases(ws).into_iter().map(|d| (d, ws.page(d).map(|p| if p.title.is_empty() { "Untitled".to_owned() } else { p.title.clone() }).unwrap_or_default())).collect();
                    if dbs.is_empty() {
                        ui.label(RichText::new("No databases yet — create one from the sidebar.").font(body(12.5)).color(pal.muted));
                    } else if let Some(nd) = pick(ui, pal, "db", "Database", &w.cfg.db, &dbs) {
                        if nd != w.cfg.db {
                            let keep_title = w.title.clone();
                            cfg::autofill(ws, &mut w.cfg, nd);
                            w.title = keep_title;
                        }
                    }
                }
                if let Some(d) = &db {
                    if has(Opt::View) {
                        let mut views: Vec<(u64, String)> = d.views.iter().map(|v| (v.id, format!("{}  ({})", v.name, v.kind.label()))).collect();
                        views.insert(0, (0, "All records".to_owned()));
                        if let Some(v) = pick(ui, pal, "view", "Saved view (its filters & sorts apply)", &w.cfg.view, &views) {
                            w.cfg.view = v;
                        }
                    }
                    if has(Opt::Filters) {
                        ui.add_space(8.0);
                        ui.label(RichText::new("Extra filters").font(body(12.0)).color(pal.muted));
                        let mut del = None;
                        let mut props = with_none(props_where(d, |_| true), "Title");
                        props[0].1 = "Title".into();
                        for (i, f) in w.cfg.filters.iter_mut().enumerate() {
                            egui::Frame::new().fill(with_alpha(pal.muted, 0.06)).corner_radius(8).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    let pname = props.iter().find(|p| p.0 == f.prop).map(|p| p.1.clone()).unwrap_or_else(|| "—".into());
                                    egui::ComboBox::from_id_salt(("dash_fp", id, i)).selected_text(pname).width(ui.available_width() - 34.0).show_ui(ui, |ui| {
                                        for (pid, n) in &props {
                                            if ui.selectable_label(f.prop == *pid, n).clicked() {
                                                f.prop = *pid;
                                                f.op = FilterOp::for_kind(d.kind_of(*pid)).first().copied().unwrap_or(FilterOp::Contains);
                                            }
                                        }
                                    });
                                    if ui.add(egui::Button::new(RichText::new(l::X).font(body(13.0)).color(pal.muted)).frame(false)).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                        del = Some(i);
                                    }
                                });
                                let kind = d.kind_of(f.prop);
                                ui.horizontal(|ui| {
                                    egui::ComboBox::from_id_salt(("dash_fo", id, i)).selected_text(f.op.label(kind)).width(110.0).show_ui(ui, |ui| {
                                        for op in FilterOp::for_kind(kind) {
                                            if ui.selectable_label(f.op == *op, op.label(kind)).clicked() {
                                                f.op = *op;
                                            }
                                        }
                                    });
                                    if f.op.needs_value() {
                                        ui.add(egui::TextEdit::singleline(&mut f.value).hint_text("value").desired_width(ui.available_width() - 4.0).font(body(13.0)));
                                    }
                                });
                            });
                            ui.add_space(4.0);
                        }
                        if let Some(i) = del {
                            w.cfg.filters.remove(i);
                        }
                        if ui.add(egui::Button::new(RichText::new(format!("{}  Add filter", l::PLUS)).font(body(12.5)).color(pal.accent)).frame(false)).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            w.cfg.filters.push(Filter { prop: 0, op: FilterOp::Contains, value: String::new() });
                        }
                    }

                    // ── grouping & measure ──
                    if has(Opt::Group) {
                        section_label(ui, pal, "ANALYSIS");
                        let groupable = with_none(props_where(d, |_| true), "Nothing (one group)");
                        if let Some(v) = pick(ui, pal, "group", if opts.contains(&Opt::Stack) { "Group by (X axis / categories)" } else { "Group by" }, &w.cfg.group, &groupable) {
                            w.cfg.group = v;
                        }
                        if has(Opt::Stack) {
                            if let Some(v) = pick(ui, pal, "stack", "Split into series by", &w.cfg.stack, &with_none(props_where(d, |_| true), "Nothing")) {
                                w.cfg.stack = v;
                            }
                        }
                        let date_axis = d.kind_of(w.cfg.group) == PropKind::Date && w.cfg.group != 0 || d.kind_of(w.cfg.stack) == PropKind::Date && w.cfg.stack != 0;
                        if has(Opt::Bucket) && date_axis {
                            let b: Vec<(Bucket, String)> = Bucket::ALL.iter().map(|b| (*b, b.label().to_owned())).collect();
                            if let Some(v) = pick(ui, pal, "bucket", "Date grouping", &w.cfg.bucket, &b) {
                                w.cfg.bucket = v;
                            }
                        }
                    } else if has(Opt::Value) || has(Opt::Agg) {
                        section_label(ui, pal, "ANALYSIS");
                    }
                    if has(Opt::Agg) {
                        let a: Vec<(Agg, String)> = Agg::ALL.iter().map(|a| (*a, a.label().to_owned())).collect();
                        if let Some(v) = pick(ui, pal, "agg", "Measure", &w.cfg.agg, &a) {
                            w.cfg.agg = v;
                            if !v.needs_prop() {
                                w.cfg.value = 0;
                            }
                        }
                    }
                    if has(Opt::Value) && (!has(Opt::Agg) || w.cfg.agg.needs_prop()) {
                        let any_kind = matches!(w.cfg.agg, Agg::Filled | Agg::Unique);
                        let cands = props_where(d, |k| any_kind || numeric(k));
                        if let Some(v) = pick(ui, pal, "value", if has(Opt::Agg) { "of property" } else { "Number property" }, &w.cfg.value, &with_none(cands, "—")) {
                            w.cfg.value = v;
                        }
                    }
                    if has(Opt::Axes) {
                        section_label(ui, pal, "AXES");
                        let nums = with_none(props_where(d, |k| numeric(k)), "—");
                        if let Some(v) = pick(ui, pal, "x", "X axis (number)", &w.cfg.x, &nums) {
                            w.cfg.x = v;
                        }
                        if let Some(v) = pick(ui, pal, "y", "Y axis (number)", &w.cfg.y, &nums) {
                            w.cfg.y = v;
                        }
                    }
                    if has(Opt::Date) {
                        let mut dates = props_where(d, |k| k == PropKind::Date);
                        dates.insert(0, (0, "Last edited".to_owned()));
                        if let Some(v) = pick(ui, pal, "date", "Date property (for the time window)", &w.cfg.date, &dates) {
                            w.cfg.date = v;
                        }
                    }
                    if has(Opt::Cols) {
                        section_label(ui, pal, "COLUMNS");
                        for p in &d.props {
                            let mut on = w.cfg.cols.contains(&p.id);
                            if ui.checkbox(&mut on, &p.name).changed() {
                                if on {
                                    w.cfg.cols.push(p.id);
                                } else {
                                    w.cfg.cols.retain(|c| *c != p.id);
                                }
                            }
                        }
                    }
                    if has(Opt::Sort) {
                        let mut s = props_where(d, |_| true);
                        s.insert(0, (0, "Saved view order".to_owned()));
                        s.insert(1, (dd::TITLE_SORT, "Title".to_owned()));
                        if let Some(v) = pick(ui, pal, "sort", "Sort by", &w.cfg.sort, &s) {
                            w.cfg.sort = v;
                            w.cfg.asc = true;
                        }
                        if w.cfg.sort != 0 {
                            check(ui, &mut w.cfg.asc, "Ascending");
                        }
                    }
                }
                if has(Opt::Range) {
                    let r: Vec<(Range, String)> = Range::ALL.iter().map(|r| (*r, r.label().to_owned())).collect();
                    if let Some(v) = pick(ui, pal, "range", "Time window", &w.cfg.range, &r) {
                        w.cfg.range = v;
                    }
                }
                if has(Opt::Limit) {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Show at most").font(body(12.5)).color(pal.muted));
                        ui.add(egui::DragValue::new(&mut w.cfg.limit).range(0..=60).speed(0.25).custom_formatter(|n, _| if n == 0.0 { "auto".to_owned() } else { format!("{n:.0}") }));
                    });
                }
                if has(Opt::Goal) {
                    ui.add_space(8.0);
                    let goal_speed = (w.cfg.goal.abs() * 0.01).max(0.5);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Goal").font(body(12.5)).color(pal.muted));
                        ui.add(egui::DragValue::new(&mut w.cfg.goal).range(0.0..=1.0e12).speed(goal_speed).custom_formatter(|n, _| if n == 0.0 { "none".to_owned() } else { dd::fmt_num(n) }));
                    });
                }

                // ── personal ──
                if has(Opt::Text) {
                    section_label(ui, pal, "TEXT");
                    ui.add(egui::TextEdit::multiline(&mut w.cfg.text).desired_rows(5).desired_width(ui.available_width() - 6.0).font(body(13.5)));
                }
                if has(Opt::Items) {
                    section_label(ui, pal, "ITEMS");
                    ui.label(RichText::new(format!("{} items — add and tick them on the widget.", w.cfg.items.len())).font(body(12.5)).color(pal.muted));
                    if w.cfg.items.iter().any(|i| i.done) && ui.button("Clear completed").clicked() {
                        w.cfg.items.retain(|i| !i.done);
                    }
                }
                if has(Opt::Target) {
                    section_label(ui, pal, "DATE");
                    ui.add(egui::TextEdit::singleline(&mut w.cfg.target).hint_text("YYYY-MM-DD").desired_width(ui.available_width() - 6.0).font(body(14.0)));
                }
                if has(Opt::Pages) {
                    section_label(ui, pal, if w.kind == "subpages" { "PARENT PAGE (EMPTY = TOP LEVEL)" } else if w.kind == "pagepreview" { "PAGE" } else { "PAGES" });
                    let mut del = None;
                    for (i, pid) in w.cfg.pages.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(ws.page(*pid).map(|p| if p.title.is_empty() { "Untitled".to_owned() } else { p.title.clone() }).unwrap_or_else(|| "(deleted)".into())).font(body(13.0)).color(pal.text));
                            if ui.add(egui::Button::new(RichText::new(l::X).font(body(12.0)).color(pal.muted)).frame(false)).clicked() {
                                del = Some(i);
                            }
                        });
                    }
                    if let Some(i) = del {
                        w.cfg.pages.remove(i);
                    }
                    let qid = Id::new(("dash_page_q", id));
                    let mut q: String = ctx.data(|d| d.get_temp(qid)).unwrap_or_default();
                    ui.add(egui::TextEdit::singleline(&mut q).hint_text("Search pages to add…").desired_width(ui.available_width() - 6.0).font(body(13.0)));
                    if !q.trim().is_empty() {
                        let ql = q.to_lowercase();
                        let found: Vec<(u64, String)> = ws.pages.iter().filter(|p| !p.trashed && !w.cfg.pages.contains(&p.id) && p.title.to_lowercase().contains(&ql)).take(6).map(|p| (p.id, if p.title.is_empty() { "Untitled".into() } else { p.title.clone() })).collect();
                        for (pid, name) in found {
                            if ui.add(egui::Button::new(RichText::new(format!("{}  {name}", l::PLUS)).font(body(13.0))).frame(false)).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                if w.kind == "pagepreview" || w.kind == "subpages" {
                                    w.cfg.pages = vec![pid];
                                } else {
                                    w.cfg.pages.push(pid);
                                }
                                q.clear();
                            }
                        }
                    }
                    ctx.data_mut(|d| d.insert_temp(qid, q));
                }

                // ── look ──
                if has(Opt::Labels) || has(Opt::Legend) || has(Opt::Palette) || has(Opt::Cumulative) || has(Opt::HideEmpty) {
                    section_label(ui, pal, "APPEARANCE");
                    if has(Opt::Labels) {
                        check(ui, &mut w.cfg.labels, "Show value labels");
                    }
                    if has(Opt::Legend) {
                        check(ui, &mut w.cfg.legend, "Show legend");
                    }
                    if has(Opt::Cumulative) {
                        check(ui, &mut w.cfg.cumulative, "Running total");
                    }
                    if has(Opt::HideEmpty) {
                        check(ui, &mut w.cfg.hide_empty, "Hide groups without records");
                    }
                    if has(Opt::Palette) {
                        ui.add_space(6.0);
                        ui.label(RichText::new("Colors").font(body(12.0)).color(pal.muted));
                        ui.horizontal(|ui| {
                            for (i, name) in PALETTE_NAMES.iter().enumerate() {
                                let (r, resp) = ui.allocate_exact_size(vec2(66.0, 30.0), Sense::click());
                                let on = w.cfg.palette as usize == i;
                                ui.painter().rect_filled(r, 8.0, if on { pal.accent_soft } else if resp.hovered() { pal.hover } else { with_alpha(pal.muted, 0.07) });
                                if on {
                                    ui.painter().rect_stroke(r, 8.0, Stroke::new(1.5_f32, pal.accent), egui::StrokeKind::Inside);
                                }
                                for k in 0..4 {
                                    ui.painter().circle_filled(pos2(r.left() + 12.0 + k as f32 * 14.0, r.center().y), 5.0, series_color(i as u8, k));
                                }
                                if resp.on_hover_text(*name).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                    w.cfg.palette = i as u8;
                                }
                            }
                        });
                    }
                }
                ui.add_space(14.0);
                if ui.add(egui::Button::new(RichText::new(format!("{}  Remove widget", l::TRASH_2)).font(body(13.0)).color(pal.danger)).frame(false)).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    remove = true;
                }
                ui.add_space(20.0);
            });
        });
    });
    if remove {
        ws.dash.remove_widget(id);
        return false;
    }
    if w != orig {
        if let Some(slot) = ws.dash.widget_mut(id) {
            *slot = w;
        }
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && ctx.memory(|m| m.focused()).is_none() {
        keep = false;
    }
    keep
}

fn size_chip(ui: &mut Ui, pal: &Palette, text: &str, on: bool) -> bool {
    let g = ui.painter().layout_no_wrap(text.to_owned(), body(12.0), pal.text);
    let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 18.0, 24.0), Sense::click());
    ui.painter().rect_filled(r, 12.0, if on { pal.accent_soft } else if resp.hovered() { pal.hover } else { with_alpha(pal.muted, 0.08) });
    ui.painter().text(r.center(), Align2::CENTER_CENTER, text, if on { bold(12.0) } else { body(12.0) }, if on { pal.accent } else { pal.text });
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

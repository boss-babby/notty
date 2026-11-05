//! Home dashboard — a customisable grid of widgets (boards / tabs, 12-column responsive layout).
//!
//! * `dashcfg.rs`   — saved model (boards, widgets, settings), pinning, presets, grid packing
//! * `dashdata.rs`  — data engine (filter → group → aggregate)
//! * `dashcharts.rs`— interactive chart painters
//! * `dashwidgets.rs` — widget registry and the 33 built-in widget types
//! * `dashedit.rs`  — widget gallery and settings inspector
//! * this file     — the shell: tabs, toolbar, grid layout, drag to reorder, drag to resize, menus, drill-down
//!
//! **Any page can be a dashboard.** The same grid is also drawn inside a page (`embedded`): the page keeps its title, icon,
//! cover and blocks and gains a widget grid — exactly how Notion dashboards are just pages. Home is merely the first page-like surface.
//!
//! Customise mode (button in the toolbar): drag a widget by its grip to move it, drag the corner handle to resize it, use
//! the gear for its settings, the cross to remove it. Outside customise mode widgets are purely interactive.

use std::cell::RefCell;

use egui::{pos2, vec2, Align2, Color32, CursorIcon, Id, LayerId, Order, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, Ui};

use crate::dashcfg::{self as cfg, Range, MAX_ROWS};
use crate::dashedit::{self, Gallery};
use crate::dashwidgets::{self as dw, Cx, Drill};
use crate::dates;
use crate::graph::Out;
use crate::lucide as l;
use crate::model::Workspace;
use crate::theme::{bold, body, Palette};
use crate::widgets::{menu_item, popup_area, section_label, text_fit, with_alpha};

/// Height of one grid row unit (a widget with `rows = 3` is `3 × UNIT + 2 × GAP` high).
pub const UNIT: f32 = 104.0;
pub const GAP: f32 = 14.0;
const HEADER_H: f32 = 34.0;

/// What the dashboard wants the app to do besides opening a page.
#[derive(Default)]
pub struct Nav {
    pub out: Out,
    pub new_page: bool,
    pub new_db: bool,
    pub journal: bool,
    pub graph: bool,
    /// The dashboard page was removed from the page it was embedded in.
    pub dropped: bool,
    pub toast: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Home,
    /// A board that belongs to a page (id of the board).
    Page(u64),
}

enum Menu {
    Widget(u64),
    Layouts(u64),
    Boards,
    BoardTab(usize),
    /// Time-window menu of the board with this id (0 = Home).
    Range(u64),
}

#[derive(Default)]
struct UiState {
    /// 0 = Home, otherwise the page board that was shown last (UI state resets when it changes).
    scope: u64,
    edit: bool,
    inspect: Option<u64>,
    gallery: Option<Gallery>,
    menu: Option<(Menu, Pos2)>,
    fresh: bool,
    drill: Option<Drill>,
    drag: Option<u64>,
    resize: Option<(u64, Pos2)>,
    rename: String,
    rects: Vec<(u64, Rect)>,
}

thread_local! {
    static UI: RefCell<UiState> = RefCell::new(UiState::default());
}

/// Opens the widget gallery (e.g. from a command-palette entry).
#[allow(dead_code)]
pub fn open_gallery() {
    UI.with(|u| {
        let mut st = u.borrow_mut();
        st.gallery = Some(Gallery::default());
        st.edit = true;
    });
}

fn gallery_for(scope: Scope) -> Gallery {
    Gallery { board: if let Scope::Page(b) = scope { b } else { 0 }, ..Gallery::default() }
}

fn card_frame(ui: &Ui, pal: &Palette, r: Rect, edit: bool, hot: bool, hover: bool) {
    let dark = pal.bg.r() < 100;
    // soft shadow, then the card itself
    let sh = Color32::from_black_alpha(if dark { 50 } else { 14 });
    ui.painter().rect_filled(r.translate(vec2(0.0, 2.0)).expand(0.5), 15.0, sh);
    ui.painter().rect_filled(r, 14.0, pal.card);
    let stroke = if hot {
        Stroke::new(2.0_f32, pal.accent)
    } else if edit {
        Stroke::new(1.2_f32, with_alpha(pal.accent, 0.55))
    } else if hover {
        Stroke::new(1.0_f32, with_alpha(pal.accent, 0.45))
    } else {
        Stroke::new(1.0_f32, pal.border)
    };
    ui.painter().rect_stroke(r, 14.0, stroke, egui::StrokeKind::Inside);
}

/// Pill button with a Lucide icon in front of the label (`glyph` may be empty).
fn action(ui: &mut Ui, pal: &Palette, glyph: &str, text: &str, primary: bool, active: bool) -> egui::Response {
    let font = if primary { bold(13.0) } else { body(13.0) };
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, Color32::WHITE);
    let lead = if glyph.is_empty() { 0.0 } else { 20.0 };
    let (r, resp) = ui.allocate_exact_size(vec2(galley.size().x + 24.0 + lead, 32.0), Sense::click());
    let fill = if primary {
        if resp.hovered() {
            pal.accent.gamma_multiply(0.85)
        } else {
            pal.accent
        }
    } else if active {
        pal.accent_soft
    } else if resp.hovered() {
        pal.active
    } else {
        pal.hover
    };
    ui.painter().rect_filled(r, 16.0, fill);
    let col = if primary { Color32::WHITE } else if active { pal.accent } else { pal.text };
    let mut x = r.left() + 12.0;
    if !glyph.is_empty() {
        ui.painter().text(pos2(x + 7.0, r.center().y), Align2::CENTER_CENTER, glyph, body(15.0), col);
        x += lead;
    }
    ui.painter().text(pos2(x, r.center().y), Align2::LEFT_CENTER, text, if primary { bold(13.0) } else { body(13.0) }, col);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Big quick-start card (icon tile, title, one-line hint).
fn quick_card(ui: &mut Ui, pal: &Palette, r: Rect, id: &str, glyph: &str, title: &str, hint: &str) -> bool {
    let resp = ui.interact(r, Id::new(("dash_quick", id)), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    ui.painter().rect_filled(r, 12.0, if t > 0.01 { crate::theme::lerp_color(pal.card, pal.hover, t) } else { pal.card });
    ui.painter().rect_stroke(r, 12.0, Stroke::new(1.0_f32, crate::theme::lerp_color(pal.border, pal.accent, t * 0.7)), egui::StrokeKind::Inside);
    let tile = Rect::from_min_size(r.min + vec2(10.0, (r.height() - 36.0) / 2.0), vec2(36.0, 36.0));
    ui.painter().rect_filled(tile, 10.0, pal.accent_soft);
    ui.painter().text(tile.center(), Align2::CENTER_CENTER, glyph, body(18.0), pal.accent);
    let tw_ = r.width() - 62.0;
    text_fit(ui.painter(), pos2(tile.right() + 10.0, r.center().y - 8.0), Align2::LEFT_CENTER, title, bold(13.5), pal.text, tw_);
    text_fit(ui.painter(), pos2(tile.right() + 10.0, r.center().y + 9.0), Align2::LEFT_CENTER, hint, body(11.5), pal.muted, tw_);
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

fn auto_title(ws: &Workspace, d: Option<&dw::Def>, w: &cfg::Widget) -> String {
    if !w.title.is_empty() {
        return w.title.clone();
    }
    let name = d.map_or("Unknown widget", |d| d.name);
    match ws.page(w.cfg.db).filter(|p| p.db.is_some() && d.map_or(false, |d| d.opts.contains(&cfg::Opt::Db))) {
        Some(p) if !p.title.is_empty() && d.map_or(false, |d| d.cat != cfg::Cat::Personal) => format!("{} · {}", p.title, name),
        _ => name.to_owned(),
    }
}

enum Act {
    Configure(u64),
    Duplicate(u64),
    Remove(u64),
    Size(u64, u8, u8),
    Plot(u64, &'static str),
    OpenDb(u64),
}

/// The Home dashboard (tabs of boards).
pub fn show(ui: &mut Ui, ws: &mut Workspace, pal: &Palette) -> Nav {
    run(ui, ws, pal, Scope::Home)
}

/// The widget grid of a **page that is a dashboard** (called by the page editor between the page header and its blocks).
pub fn embedded(ui: &mut Ui, ws: &mut Workspace, pal: &Palette, board: u64) -> Nav {
    run(ui, ws, pal, Scope::Page(board))
}

fn run(ui: &mut Ui, ws: &mut Workspace, pal: &Palette, scope: Scope) -> Nav {
    cfg::ensure_seeded(ws);
    let mut nav = Nav::default();
    UI.with(|u| {
        let mut st = u.borrow_mut();
        let key = if let Scope::Page(b) = scope { b } else { 0 };
        if st.scope != key {
            *st = UiState { scope: key, ..UiState::default() };
        }
        inner(ui, ws, pal, &mut st, &mut nav, scope);
    });
    nav
}

fn greeting_icon() -> &'static str {
    l::LAYOUT_DASHBOARD
}

/// Toolbar: Add widget · Customize · time window (Home) or layouts (page dashboards).
///
/// `flat`: the caller is a `horizontal_wrapped` row (phones). egui cannot wrap a nested `ui.horizontal`, so in that mode the time-window
/// chips are laid out directly in the caller's row and wrap with everything else instead of running off the screen.
fn toolbar(ui: &mut Ui, ws: &mut Workspace, pal: &Palette, st: &mut UiState, scope: Scope, flat: bool) {
    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
    if action(ui, pal, l::PLUS, "Add widget", true, false).clicked() {
        st.gallery = Some(gallery_for(scope));
        st.edit = true;
    }
    if action(ui, pal, if st.edit { l::CHECK } else { l::SLIDERS_HORIZONTAL }, if st.edit { "Done" } else { "Customize" }, false, st.edit).clicked() {
        st.edit = !st.edit;
        if !st.edit {
            st.inspect = None;
        }
    }
    // Time window — available on Home AND on every page dashboard: quick chips (All / Today / Week / Month) + a full menu.
    let bid = match scope {
        Scope::Home => 0,
        Scope::Page(b) => b,
    };
    let cur = ws.dash.scope_range(bid);
    // the chips are laid out left-to-right even when the toolbar itself is right-to-left
    let mut chips = |ui: &mut Ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for (r, t) in [(Range::All, "All"), (Range::Today, "Today"), (Range::ThisWeek, "Week"), (Range::ThisMonth, "Month")] {
                if chip(ui, pal, t, cur == r)
                    .on_hover_text(match r {
                        Range::All => "Show every record",
                        Range::Today => "Only today's records",
                        Range::ThisWeek => "Only this week's records",
                        _ => "Only this month's records",
                    })
                    .clicked()
                {
                    ws.dash.set_scope_range(bid, r);
                }
            }
            let other = !matches!(cur, Range::All | Range::Today | Range::ThisWeek | Range::ThisMonth);
            let label = if other { cur.label().to_owned() } else { "More".to_owned() };
            let r = action(ui, pal, l::CALENDAR_DAYS, &label, false, other);
            let c = r.clicked();
            if c {
                st.menu = Some((Menu::Range(bid), r.rect.left_bottom() + vec2(0.0, 6.0)));
                st.fresh = true;
            }
            r.on_hover_text("Time window for every widget on this dashboard that has none of its own");
            c
    };
    if flat {
        chips(ui);
    } else {
        ui.horizontal(|ui| chips(ui));
    }
    if let Scope::Page(b) = scope {
        let r = action(ui, pal, l::LAYOUT_TEMPLATE, "Layouts", false, false);
        if r.clicked() {
            st.menu = Some((Menu::Layouts(b), r.rect.left_bottom() + vec2(-120.0, 6.0)));
            st.fresh = true;
        }
        r.on_hover_text("Fill this dashboard with a ready-made set of widgets, or remove it");
    }
}

fn inner(ui: &mut Ui, ws: &mut Workspace, pal: &Palette, st: &mut UiState, nav: &mut Nav, scope: Scope) {
    let ctx = ui.ctx().clone();
    let w = ui.available_width();
    let home = scope == Scope::Home;
    let pad = if !home { 0.0 } else if w < 520.0 { 12.0 } else { 28.0 };
    let screen_rect = if home { ui.max_rect() } else { ctx.available_rect() };
    let key = Id::new("dash_scroll");
    let sa = ScrollArea::vertical().id_salt("dash_scroll_area").auto_shrink([false, false]);
    let over = ui.rect_contains_pointer(ui.max_rect()) && st.drag.is_none() && st.resize.is_none();
    let mut acts: Vec<Act> = vec![];
    let global = ws.dash.scope_range(match scope { Scope::Home => 0, Scope::Page(b) => b });
    st.rects.clear();
    // the board that is drawn
    let bidx: Option<usize> = match scope {
        Scope::Home => Some(ws.dash.active).filter(|i| *i < ws.dash.boards.len()),
        Scope::Page(b) => ws.dash.board_index(b),
    };
    let cur_board: u64 = bidx.map(|i| ws.dash.boards[i].id).unwrap_or(0);

    let mut draw_body = |ui: &mut Ui| {
        if home {
            ui.add_space(14.0);
        }
        egui::Frame::new().inner_margin(egui::Margin::symmetric(pad as i8, 0)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let gw0 = ui.available_width();
            if home {
                // ── hero ──
                ui.horizontal(|ui| {
                    let (tile, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
                    ui.painter().rect_filled(tile, 13.0, pal.accent);
                    ui.painter().text(tile.center(), Align2::CENTER_CENTER, greeting_icon(), body(22.0), Color32::WHITE);
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.add(egui::Label::new(RichText::new(if ws.name.is_empty() { "Home".to_owned() } else { ws.name.clone() }).font(bold(if w < 520.0 { 22.0 } else { 28.0 })).color(pal.text)).truncate());
                        ui.add(egui::Label::new(RichText::new(dates::long_today()).font(body(13.5)).color(pal.muted)).truncate());
                    });
                });
                ui.add_space(14.0);
                // ── quick start cards ──
                let cards: [(&str, &str, &str, &str); 5] = [
                    ("page", l::FILE_PLUS, "New page", "Write notes & docs"),
                    ("db", l::DATABASE, "New database", "Tables, boards, calendars"),
                    ("dash", l::LAYOUT_DASHBOARD, "New dashboard", "A page made of widgets"),
                    ("journal", l::SUN, "Today's journal", "Your daily page"),
                    ("graph", l::NETWORK, "Graph view", "See how pages connect"),
                ];
                let n = if gw0 >= 980.0 { 5 } else if gw0 >= 640.0 { 3 } else { 2 };
                let gap = 10.0;
                let cw_ = (gw0 - gap * (n as f32 - 1.0)) / n as f32;
                for chunk in cards.chunks(n) {
                    let (rr, _) = ui.allocate_exact_size(vec2(gw0, 56.0), Sense::hover());
                    for (i, (id, g, t, h)) in chunk.iter().enumerate() {
                        let r = Rect::from_min_size(rr.min + vec2((cw_ + gap) * i as f32, 0.0), vec2(cw_, 56.0));
                        if quick_card(ui, pal, r, id, g, t, h) {
                            match *id {
                                "page" => nav.new_page = true,
                                "db" => nav.new_db = true,
                                "dash" => nav.out.open = Some(cfg::new_dashboard_page(ws, None, "starter")),
                                "journal" => nav.journal = true,
                                _ => nav.graph = true,
                            }
                        }
                    }
                    ui.add_space(gap);
                }
                ui.add_space(6.0);

                // ── board tabs + toolbar (one row when there is room) ──
                let boards: Vec<(usize, String)> = ws.dash.boards.iter().enumerate().filter(|(_, b)| b.page == 0).map(|(i, b)| (i, b.name.clone())).collect();
                let active = ws.dash.active;
                let wide = gw0 >= 900.0;
                let mut tabs = |ui: &mut Ui, st: &mut UiState, ws: &mut Workspace| {
                    ui.spacing_mut().item_spacing = vec2(4.0, 6.0);
                    for (i, name) in &boards {
                        let on = *i == active;
                        let g = ui.painter().layout_no_wrap(name.clone(), if on { bold(13.5) } else { body(13.5) }, pal.text);
                        let (r, resp) = ui.allocate_exact_size(vec2((g.size().x + 28.0).min(220.0), 32.0), Sense::click());
                        ui.painter().rect_filled(r, 16.0, if on { pal.accent_soft } else if resp.hovered() { pal.hover } else { Color32::TRANSPARENT });
                        text_fit(ui.painter(), r.center(), Align2::CENTER_CENTER, name, if on { bold(13.5) } else { body(13.5) }, if on { pal.accent } else { pal.muted }, r.width() - 16.0);
                        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            if on {
                                st.rename = name.clone();
                                st.menu = Some((Menu::BoardTab(*i), r.left_bottom() + vec2(0.0, 6.0)));
                                st.fresh = true;
                            } else {
                                ws.dash.active = *i;
                            }
                        }
                    }
                    let (r, resp) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
                    ui.painter().rect_filled(r, 16.0, if resp.hovered() { pal.hover } else { Color32::TRANSPARENT });
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, l::PLUS, body(15.0), pal.muted);
                    if resp.on_hover_text("Add a board").on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        st.menu = Some((Menu::Boards, r.left_bottom() + vec2(0.0, 6.0)));
                        st.fresh = true;
                    }
                };
                if wide {
                    ui.horizontal(|ui| {
                        tabs(ui, st, ws);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| toolbar(ui, ws, pal, st, scope, false));
                    });
                } else {
                    ui.horizontal_wrapped(|ui| tabs(ui, st, ws));
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| toolbar(ui, ws, pal, st, scope, true));
                }
            } else {
                // ── page dashboard header ──
                ui.add_space(6.0);
                let count = bidx.map_or(0, |i| ws.dash.boards[i].widgets.len());
                let narrow = gw0 < 620.0;
                let mut head = |ui: &mut Ui| {
                    let (tile, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
                    ui.painter().rect_filled(tile, 8.0, pal.accent_soft);
                    ui.painter().text(tile.center(), Align2::CENTER_CENTER, l::LAYOUT_DASHBOARD, body(15.0), pal.accent);
                    ui.add_space(2.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new("Dashboard").font(bold(14.0)).color(pal.text));
                        ui.label(RichText::new(format!("{count} widget{}", if count == 1 { "" } else { "s" })).font(body(11.5)).color(pal.muted));
                    });
                };
                if narrow {
                    ui.horizontal(|ui| head(ui));
                    ui.horizontal_wrapped(|ui| toolbar(ui, ws, pal, st, scope, true));
                } else {
                    ui.horizontal(|ui| {
                        head(ui);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| toolbar(ui, ws, pal, st, scope, false));
                    });
                }
            }
            if st.edit {
                ui.add_space(8.0);
                egui::Frame::new().fill(pal.accent_soft).corner_radius(10).inner_margin(egui::Margin::symmetric(12, 7)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(l::SLIDERS_HORIZONTAL).font(body(14.0)).color(pal.accent));
                        ui.add(egui::Label::new(RichText::new(if gw0 < 520.0 { "Customize: grip = move · corner = resize · gear = settings" } else { "Customize: drag a widget by its grip to move it · drag the corner to resize · gear for settings" }).font(body(12.0)).color(pal.accent)).wrap());
                    });
                });
            }
            ui.add_space(14.0);

            // ── grid ──
            let gw = ui.available_width();
            let total: u8 = if gw >= 760.0 { 12 } else if gw >= 520.0 { 6 } else { 1 };
            let cw = (gw - GAP * (total as f32 - 1.0)) / total as f32;
            let widgets: Vec<cfg::Widget> = bidx.map(|i| ws.dash.boards[i].widgets.clone()).unwrap_or_default();
            if widgets.is_empty() {
                let (r, _) = ui.allocate_exact_size(vec2(gw, 250.0), Sense::hover());
                let p = ui.painter_at(r);
                p.rect_filled(r, 16.0, with_alpha(pal.muted, 0.04));
                p.rect_stroke(r, 16.0, Stroke::new(1.5_f32, pal.border), egui::StrokeKind::Inside);
                let tile = Rect::from_center_size(pos2(r.center().x, r.top() + 56.0), vec2(56.0, 56.0));
                p.rect_filled(tile, 16.0, pal.accent_soft);
                p.text(tile.center(), Align2::CENTER_CENTER, l::LAYOUT_DASHBOARD, body(28.0), pal.accent);
                p.text(pos2(r.center().x, r.top() + 106.0), Align2::CENTER_CENTER, if home { "This board is empty" } else { "This page is a dashboard — add its first widget" }, bold(17.0), pal.text);
                p.text(pos2(r.center().x, r.top() + 130.0), Align2::CENTER_CENTER, "Pin charts, numbers, tables, pages and more", body(13.0), pal.muted);
                let br = Rect::from_center_size(pos2(r.center().x - 76.0, r.top() + 176.0), vec2(136.0, 34.0));
                let resp = ui.interact(br, Id::new("dash_empty_add"), Sense::click());
                p.rect_filled(br, 17.0, if resp.hovered() { pal.accent.gamma_multiply(0.85) } else { pal.accent });
                p.text(br.center() + vec2(8.0, 0.0), Align2::CENTER_CENTER, "Add widget", bold(13.0), Color32::WHITE);
                p.text(pos2(br.left() + 19.0, br.center().y), Align2::CENTER_CENTER, l::PLUS, body(15.0), Color32::WHITE);
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    st.gallery = Some(gallery_for(scope));
                    st.edit = true;
                }
                let lr = Rect::from_center_size(pos2(r.center().x + 76.0, r.top() + 176.0), vec2(136.0, 34.0));
                let lresp = ui.interact(lr, Id::new("dash_empty_layout"), Sense::click());
                p.rect_filled(lr, 17.0, if lresp.hovered() { pal.active } else { pal.hover });
                p.text(lr.center() + vec2(8.0, 0.0), Align2::CENTER_CENTER, "Use a layout", body(13.0), pal.text);
                p.text(pos2(lr.left() + 19.0, lr.center().y), Align2::CENTER_CENTER, l::LAYOUT_TEMPLATE, body(15.0), pal.text);
                if lresp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    st.menu = Some((Menu::Layouts(cur_board), lr.left_bottom() + vec2(-60.0, 6.0)));
                    st.fresh = true;
                }
            } else {
                let spans: Vec<u8> = widgets.iter().map(|w| w.span).collect();
                let packed = cfg::pack(&spans, total);
                let nrows = packed.iter().map(|p| p.0).max().unwrap_or(0) + 1;
                let mut row_h = vec![0.0f32; nrows];
                for (i, p) in packed.iter().enumerate() {
                    let rows = widgets[i].rows.clamp(1, MAX_ROWS) as f32;
                    row_h[p.0] = row_h[p.0].max(rows * UNIT + (rows - 1.0) * GAP);
                }
                let mut row_y = vec![0.0f32; nrows];
                for r in 1..nrows {
                    row_y[r] = row_y[r - 1] + row_h[r - 1] + GAP;
                }
                let total_h = row_y[nrows - 1] + row_h[nrows - 1];
                let (grid, _) = ui.allocate_exact_size(vec2(gw, total_h + 20.0), Sense::hover());
                let rect_of = |i: usize| -> Rect {
                    let (r, x, s) = packed[i];
                    let wd = cw * s as f32 + GAP * (s as f32 - 1.0);
                    Rect::from_min_size(pos2(grid.left() + (cw + GAP) * x as f32, grid.top() + row_y[r]), vec2(wd, row_h[r]))
                };
                let ptr = ctx.pointer_interact_pos();
                for (i, wd) in widgets.iter().enumerate() {
                    let r = rect_of(i);
                    st.rects.push((wd.id, r));
                    if !ui.clip_rect().expand(200.0).intersects(r) {
                        continue;
                    }
                    let def = dw::def_for(wd);
                    let title = auto_title(ws, def.as_ref(), wd);
                    let selected = st.inspect == Some(wd.id);
                    let hover_card = ptr.map_or(false, |q| r.contains(q)) && st.drag.is_none();
                    card_frame(ui, pal, r, st.edit, selected, hover_card);
                    if st.drag == Some(wd.id) {
                        ui.painter().rect_filled(r, 14.0, with_alpha(pal.bg, 0.55));
                    }
                    // header
                    let hr = Rect::from_min_size(r.min + vec2(14.0, 8.0), vec2(r.width() - 28.0, HEADER_H - 8.0));
                    let mut tx = hr.left();
                    if st.edit {
                        let gr = Rect::from_min_size(pos2(hr.left() - 4.0, hr.top()), vec2(24.0, hr.height()));
                        let resp = ui.interact(gr, Id::new(("dash_grip", wd.id)), Sense::drag());
                        ui.painter().text(gr.center(), Align2::CENTER_CENTER, l::GRIP_VERTICAL, body(15.0), if resp.hovered() || st.drag == Some(wd.id) { pal.text } else { pal.muted });
                        if resp.hovered() {
                            ctx.set_cursor_icon(CursorIcon::Grab);
                        }
                        if resp.drag_started() {
                            st.drag = Some(wd.id);
                        }
                        tx += 22.0;
                    }
                    if let Some(d) = &def {
                        let chip = Rect::from_center_size(pos2(tx + 11.0, hr.center().y), vec2(22.0, 22.0));
                        ui.painter().rect_filled(chip, 7.0, pal.accent_soft);
                        ui.painter().text(chip.center(), Align2::CENTER_CENTER, d.glyph, body(13.0), pal.accent);
                        tx += 30.0;
                    }
                    let right_w = if st.edit { 62.0 } else if hover_card { 30.0 } else { 0.0 };
                    text_fit(ui.painter(), pos2(tx, hr.center().y), Align2::LEFT_CENTER, &title, bold(14.0), pal.text, (hr.right() - tx - right_w - 4.0).max(30.0));
                    // header buttons
                    let mut bx = hr.right();
                    if st.edit || hover_card || st.menu.as_ref().map_or(false, |m| matches!(m.0, Menu::Widget(id) if id == wd.id)) {
                        let br = Rect::from_min_size(pos2(bx - 28.0, hr.top() - 1.0), vec2(28.0, 26.0));
                        bx -= 30.0;
                        let resp = ui.interact(br, Id::new(("dash_more", wd.id)), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(br, 7.0, pal.hover);
                        }
                        ui.painter().text(br.center(), Align2::CENTER_CENTER, l::ELLIPSIS, body(15.0), pal.muted);
                        if resp.on_hover_text("Widget options").on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            st.menu = Some((Menu::Widget(wd.id), br.left_bottom() + vec2(-190.0, 4.0)));
                            st.fresh = true;
                        }
                    }
                    if st.edit {
                        let br = Rect::from_min_size(pos2(bx - 28.0, hr.top() - 1.0), vec2(28.0, 26.0));
                        let resp = ui.interact(br, Id::new(("dash_gear", wd.id)), Sense::click());
                        if resp.hovered() || selected {
                            ui.painter().rect_filled(br, 7.0, if selected { pal.accent_soft } else { pal.hover });
                        }
                        ui.painter().text(br.center(), Align2::CENTER_CENTER, l::SETTINGS, body(14.0), if selected { pal.accent } else { pal.muted });
                        if resp.on_hover_text("Settings").on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            st.inspect = if selected { None } else { Some(wd.id) };
                        }
                    }
                    // content
                    let content = Rect::from_min_max(pos2(r.left() + 14.0, r.top() + HEADER_H + 4.0), pos2(r.right() - 14.0, r.bottom() - 12.0));
                    if content.width() > 20.0 && content.height() > 16.0 {
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(content).id_salt(("dash_w", wd.id)).layout(egui::Layout::top_down(egui::Align::Min)));
                        child.set_clip_rect(content.intersect(ui.clip_rect()));
                        let mut copy = wd.clone();
                        {
                            let mut cx = Cx { ws, pal, nav, drill: &mut st.drill, global, wid: wd.id, configure: false };
                            match &def {
                                Some(d) => (d.draw)(&mut cx, &mut copy, &mut child),
                                None => {
                                    child.label(RichText::new(format!("Unknown widget type “{}”.\nIt was added by a newer version — it is kept and will show up again after updating.", wd.kind)).font(body(12.5)).color(pal.muted));
                                }
                            }
                            if cx.configure {
                                st.inspect = Some(wd.id);
                                st.edit = true;
                            }
                        }
                        #[cfg(test)]
                        {
                            let over = child.min_rect().right() - content.right();
                            if over > 1.0 {
                                testing::OVERFLOW.with(|o| o.borrow_mut().push((wd.kind.to_string(), over)));
                            }
                        }
                        if copy != *wd {
                            if let Some(slot) = ws.dash.widget_mut(wd.id) {
                                *slot = copy;
                            }
                        }
                    }
                    // resize handle
                    if st.edit {
                        let hr2 = Rect::from_min_size(r.right_bottom() - vec2(22.0, 22.0), vec2(22.0, 22.0));
                        let resp = ui.interact(hr2, Id::new(("dash_resize", wd.id)), Sense::drag());
                        let hot = resp.hovered() || st.resize.map_or(false, |x| x.0 == wd.id);
                        for k in 0..3 {
                            let o = 5.0 + k as f32 * 4.5;
                            ui.painter().line_segment([r.right_bottom() - vec2(o, 3.0), r.right_bottom() - vec2(3.0, o)], Stroke::new(1.6_f32, if hot { pal.accent } else { pal.muted }));
                        }
                        if resp.hovered() {
                            ctx.set_cursor_icon(CursorIcon::ResizeNwSe);
                        }
                        if resp.drag_started() {
                            st.resize = Some((wd.id, r.min));
                        }
                    }
                }

                // ── drag to move ──
                if let Some(id) = st.drag {
                    ctx.set_cursor_icon(CursorIcon::Grabbing);
                    ctx.request_repaint();
                    if let Some(q) = ptr {
                        let target = st.rects.iter().find(|(tid, r)| *tid != id && r.contains(q)).copied();
                        let title = widgets.iter().find(|w| w.id == id).map(|w| auto_title(ws, dw::def_for(w).as_ref(), w)).unwrap_or_default();
                        let g = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("dash_drag_ghost")));
                        let gr = Rect::from_center_size(q + vec2(10.0, 6.0), vec2(190.0, 34.0));
                        g.rect_filled(gr.translate(vec2(0.0, 3.0)), 10.0, Color32::from_black_alpha(40));
                        g.rect_filled(gr, 10.0, pal.popup);
                        g.rect_stroke(gr, 10.0, Stroke::new(1.5_f32, pal.accent), egui::StrokeKind::Inside);
                        text_fit(&g, gr.center(), Align2::CENTER_CENTER, &title, bold(13.0), pal.text, 170.0);
                        if let Some((tid, tr)) = target {
                            let after = q.x > tr.center().x;
                            let x = if after { tr.right() + GAP / 2.0 } else { tr.left() - GAP / 2.0 };
                            ui.painter().line_segment([pos2(x, tr.top()), pos2(x, tr.bottom())], Stroke::new(4.0_f32, pal.accent));
                            if !ctx.input(|i| i.pointer.any_down()) {
                                ws.dash.move_widget(id, tid, after);
                                st.drag = None;
                            }
                        }
                    }
                    if !ctx.input(|i| i.pointer.any_down()) {
                        st.drag = None;
                    }
                }
                // ── drag to resize ──
                if let Some((id, start)) = st.resize {
                    ctx.set_cursor_icon(CursorIcon::ResizeNwSe);
                    ctx.request_repaint();
                    if let Some(q) = ptr {
                        let cols = ((q.x - start.x + GAP) / (cw + GAP)).round().clamp(1.0, total as f32);
                        let span = ((cols * 12.0 / total as f32).round() as u8).clamp(1, 12);
                        let rows = (((q.y - start.y + GAP) / (UNIT + GAP)).round().clamp(1.0, MAX_ROWS as f32)) as u8;
                        if let Some(w) = ws.dash.widget_mut(id) {
                            w.span = span;
                            w.rows = rows;
                        }
                        let g = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("dash_size_tip")));
                        let gr = Rect::from_min_size(q + vec2(14.0, 14.0), vec2(64.0, 24.0));
                        g.rect_filled(gr, 12.0, pal.text);
                        g.text(gr.center(), Align2::CENTER_CENTER, format!("{span} × {rows}"), bold(12.0), pal.bg);
                    }
                    if !ctx.input(|i| i.pointer.any_down()) {
                        st.resize = None;
                    }
                }
            }
            ui.add_space(30.0);
        });
    };
    if home {
        let _out = crate::smooth::scroll_area(ui, key, sa, over, draw_body);
        #[cfg(test)]
        testing::CONTENT_W.with(|c| c.set((_out.content_size.x, _out.inner_rect.width())));
    } else {
        draw_body(ui);
    }

    // ───────────── overlays ─────────────
    // widget / board menus
    if let Some((menu, pos)) = st.menu.take() {
        let pid = Id::new("dash_menu");
        let mut keep;
        let mut close = false;
        match &menu {
            Menu::Widget(id) => {
                let wid = *id;
                let def = ws.dash.widget(wid).and_then(dw::def_for);
                let (db, span, rows) = ws.dash.widget(wid).map(|w| (w.cfg.db, w.span, w.rows)).unwrap_or((0, 4, 3));
                let plot = ws.dash.widget(wid).and_then(cfg::plot_of);
                keep = popup_area(&ctx, pid, pos, pal, &mut st.fresh, 230.0, |ui| {
                    if menu_item(ui, pal, l::SETTINGS, "Settings", None, false, false).clicked() {
                        acts.push(Act::Configure(wid));
                        close = true;
                    }
                    if menu_item(ui, pal, l::COPY, "Duplicate", None, false, false).clicked() {
                        acts.push(Act::Duplicate(wid));
                        close = true;
                    }
                    if db != 0 && ws.page(db).is_some() && menu_item(ui, pal, l::DATABASE, "Open source database", None, false, false).clicked() {
                        acts.push(Act::OpenDb(db));
                        close = true;
                    }
                    section_label(ui, pal, "WIDTH");
                    ui.horizontal_wrapped(|ui| {
                        for (n, s) in [("1/4", 3u8), ("1/3", 4), ("1/2", 6), ("2/3", 8), ("Full", 12)] {
                            if chip(ui, pal, n, span == s).clicked() {
                                acts.push(Act::Size(wid, s, rows));
                            }
                        }
                    });
                    section_label(ui, pal, "HEIGHT");
                    ui.horizontal_wrapped(|ui| {
                        for (n, r) in [("XS", 1u8), ("S", 2), ("M", 3), ("L", 4), ("XL", 6), ("XXL", 8)] {
                            if chip(ui, pal, n, rows == r).clicked() {
                                acts.push(Act::Size(wid, span, r));
                            }
                        }
                    });
                    if let Some(plot) = plot.clone() {
                        section_label(ui, pal, "PLOT TYPE");
                        ui.horizontal_wrapped(|ui| {
                            for k in dw::CHART_KINDS {
                                if let Some(d) = dw::def(k) {
                                    let resp = glyph_chip(ui, pal, d.glyph, plot == k).on_hover_text(dw::plot_label(k));
                                    if resp.clicked() {
                                        acts.push(Act::Plot(wid, k));
                                    }
                                }
                            }
                        });
                    }
                    let _ = &def;
                    ui.add_space(2.0);
                    if menu_item(ui, pal, l::TRASH_2, "Remove from dashboard", None, false, true).clicked() {
                        acts.push(Act::Remove(wid));
                        close = true;
                    }
                });
            }
            Menu::Layouts(bid) => {
                let bid = *bid;
                let page = ws.dash.board_index(bid).map_or(0, |i| ws.dash.boards[i].page);
                keep = popup_area(&ctx, pid, pos, pal, &mut st.fresh, 290.0, |ui| {
                    section_label(ui, pal, "ADD A LAYOUT");
                    for (key, glyph, name, hint) in [
                        ("overview", l::LAYOUT_DASHBOARD, "Overview", "Stats, tasks, deadlines, charts"),
                        ("analytics", l::CHART_PIE, "Analytics", "Numbers, charts and a table per database"),
                        ("personal", l::SUN, "Personal", "Today, timer, quick add, checklist, notes"),
                    ] {
                        if menu_item(ui, pal, glyph, name, Some(hint), false, false).clicked() {
                            cfg::fill_board(ws, bid, key);
                            close = true;
                        }
                    }
                    if page != 0 {
                        ui.add_space(3.0);
                        ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0_f32, pal.border));
                        ui.add_space(4.0);
                        if menu_item(ui, pal, l::TRASH_2, "Remove dashboard from this page", Some("The page and its text stay"), false, true).clicked() {
                            ws.dash.drop_page_board(page);
                            nav.dropped = true;
                            close = true;
                        }
                    }
                });
            }
            Menu::Boards => {
                keep = popup_area(&ctx, pid, pos, pal, &mut st.fresh, 270.0, |ui| {
                    section_label(ui, pal, "ADD A BOARD");
                    for (key, glyph, name, hint) in [
                        ("blank", l::PLUS, "Blank board", "Start from nothing"),
                        ("overview", l::LAYOUT_DASHBOARD, "Overview", "Stats, tasks, deadlines, charts"),
                        ("analytics", l::CHART_PIE, "Analytics", "Numbers, charts and a table per database"),
                        ("personal", l::SUN, "Personal", "Today, timer, quick add, checklist, notes"),
                    ] {
                        if menu_item(ui, pal, glyph, name, Some(hint), false, false).clicked() {
                            cfg::add_preset(ws, key);
                            close = true;
                        }
                    }
                });
            }
            Menu::BoardTab(i) => {
                let i = *i;
                keep = popup_area(&ctx, pid, pos, pal, &mut st.fresh, 250.0, |ui| {
                    section_label(ui, pal, "BOARD NAME");
                    let resp = ui.add(egui::TextEdit::singleline(&mut st.rename).desired_width(238.0).font(body(14.0)));
                    if resp.changed() {
                        if let Some(b) = ws.dash.boards.get_mut(i) {
                            b.name = st.rename.clone();
                        }
                    }
                    if resp.lost_focus() && ui.input(|k| k.key_pressed(egui::Key::Enter)) {
                        close = true;
                    }
                    ui.add_space(4.0);
                    if menu_item(ui, pal, l::ARROW_LEFT, "Move left", None, false, false).clicked() && i > 0 {
                        ws.dash.boards.swap(i, i - 1);
                        ws.dash.active = i - 1;
                        close = true;
                    }
                    if menu_item(ui, pal, l::ARROW_RIGHT, "Move right", None, false, false).clicked() && i + 1 < ws.dash.boards.len() {
                        ws.dash.boards.swap(i, i + 1);
                        ws.dash.active = i + 1;
                        close = true;
                    }
                    if ws.dash.boards.len() > 1 && menu_item(ui, pal, l::TRASH_2, "Delete board", None, false, true).clicked() {
                        ws.dash.remove_board(i);
                        close = true;
                    }
                });
            }
            Menu::Range(rb) => {
                keep = popup_area(&ctx, pid, pos, pal, &mut st.fresh, 230.0, |ui| {
                    section_label(ui, pal, "DASHBOARD TIME WINDOW");
                    for r in Range::ALL {
                        if menu_item(ui, pal, if r == Range::All { l::CLOCK } else { l::CALENDAR_DAYS }, r.label(), None, ws.dash.scope_range(*rb) == r, false).clicked() {
                            ws.dash.set_scope_range(*rb, r);
                            close = true;
                        }
                    }
                    ui.add_space(2.0);
                    ui.label(RichText::new("Applies to widgets that have a date property and no window of their own.").font(body(11.5)).color(pal.muted));
                });
            }
        }
        if keep && !close {
            st.menu = Some((menu, pos));
        }
    }

    // drill-down list
    if let Some(mut dr) = st.drill.take() {
        let pid = Id::new("dash_drill");
        let mut fresh = dr.fresh;
        let mut open: Option<u64> = None;
        let keep = popup_area(&ctx, pid, dr.at, pal, &mut fresh, 320.0, |ui| {
            ui.label(RichText::new(&dr.title).font(bold(14.0)).color(pal.text));
            ui.label(RichText::new(format!("{} record{}", dr.rows.len(), if dr.rows.len() == 1 { "" } else { "s" })).font(body(12.0)).color(pal.muted));
            ui.add_space(6.0);
            for id in dr.rows.iter().take(12) {
                let t = ws.page(*id).map(|p| if p.title.is_empty() { "Untitled".to_owned() } else { p.title.clone() }).unwrap_or_else(|| "Untitled".into());
                let parent = ws.page(*id).and_then(|p| p.parent).and_then(|d| ws.page(d)).map(|d| d.title.clone()).unwrap_or_default();
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(r, 7.0, pal.hover);
                }
                let rw = if parent.is_empty() { 0.0 } else { 100.0 };
                text_fit(ui.painter(), pos2(r.left() + 6.0, r.center().y), Align2::LEFT_CENTER, &t, body(13.5), pal.text, r.width() - rw - 14.0);
                if !parent.is_empty() {
                    text_fit(ui.painter(), pos2(r.right() - 6.0, r.center().y), Align2::RIGHT_CENTER, &parent, body(11.5), pal.muted, rw);
                }
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    open = Some(*id);
                }
            }
            if dr.rows.len() > 12 {
                ui.label(RichText::new(format!("+ {} more", dr.rows.len() - 12)).font(body(12.0)).color(pal.muted));
            }
        });
        dr.fresh = fresh;
        if let Some(id) = open {
            nav.out.open = Some(id);
        } else if keep {
            st.drill = Some(dr);
        }
    }

    // gallery + inspector
    if let Some(mut g) = st.gallery.take() {
        match dashedit::gallery(&ctx, ws, pal, &mut g, screen_rect) {
            dashedit::GalleryResult::Open => st.gallery = Some(g),
            dashedit::GalleryResult::Close => {}
            dashedit::GalleryResult::Added(id) => {
                st.inspect = Some(id);
                nav.toast = Some("Widget added — adjust it in the panel on the right".into());
            }
        }
    }
    if let Some(id) = st.inspect {
        if st.edit && ws.dash.widget(id).is_some() {
            if !dashedit::inspector(&ctx, ws, pal, id, screen_rect) {
                st.inspect = None;
            }
        } else {
            st.inspect = None;
        }
    }

    // widget actions
    for a in acts {
        match a {
            Act::Configure(id) => {
                st.edit = true;
                st.inspect = Some(id);
            }
            Act::Duplicate(id) => {
                ws.dash.duplicate_widget(id);
            }
            Act::Remove(id) => {
                ws.dash.remove_widget(id);
                if st.inspect == Some(id) {
                    st.inspect = None;
                }
                nav.toast = Some("Widget removed".into());
            }
            Act::Size(id, s, r) => {
                if let Some(w) = ws.dash.widget_mut(id) {
                    w.span = s;
                    w.rows = r;
                }
            }
            Act::Plot(id, k) => {
                if let Some(w) = ws.dash.widget_mut(id) {
                    w.kind = "chart".into();
                    w.cfg.extra.insert("plot".into(), k.to_owned());
                    let mut c = w.cfg.clone();
                    dashedit::tune(&*ws, &mut c, k);
                    if let Some(w) = ws.dash.widget_mut(id) {
                        w.cfg = c;
                    }
                }
            }
            Act::OpenDb(id) => nav.out.open = Some(id),
        }
    }
}

fn chip(ui: &mut Ui, pal: &Palette, text: &str, on: bool) -> egui::Response {
    let g = ui.painter().layout_no_wrap(text.to_owned(), body(12.0), pal.text);
    let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 18.0, 24.0), Sense::click());
    ui.painter().rect_filled(r, 12.0, if on { pal.accent_soft } else if resp.hovered() { pal.hover } else { with_alpha(pal.muted, 0.08) });
    ui.painter().text(r.center(), Align2::CENTER_CENTER, text, if on { bold(12.0) } else { body(12.0) }, if on { pal.accent } else { pal.text });
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn glyph_chip(ui: &mut Ui, pal: &Palette, glyph: &str, on: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(30.0, 28.0), Sense::click());
    ui.painter().rect_filled(r, 8.0, if on { pal.accent_soft } else if resp.hovered() { pal.hover } else { with_alpha(pal.muted, 0.08) });
    ui.painter().text(r.center(), Align2::CENTER_CENTER, glyph, body(15.0), if on { pal.accent } else { pal.text });
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Hooks for the headless UI tests.
#[cfg(test)]
pub mod testing {
    use super::*;

    pub fn set(edit: bool, inspect: Option<u64>, gallery: bool) {
        UI.with(|u| {
            let mut st = u.borrow_mut();
            st.edit = edit;
            st.inspect = inspect;
            st.gallery = if gallery { Some(Gallery::default()) } else { None };
        });
    }

    thread_local! {
        /// (widget kind, pixels its content ran past the right edge of its card) — filled while drawing.
        pub static OVERFLOW: std::cell::RefCell<Vec<(String, f32)>> = const { std::cell::RefCell::new(Vec::new()) };
        /// (content width, visible width) of the Home dashboard scroll area.
        pub static CONTENT_W: std::cell::Cell<(f32, f32)> = const { std::cell::Cell::new((0.0, 0.0)) };
    }

    pub fn rects() -> Vec<(u64, Rect)> {
        UI.with(|u| u.borrow().rects.clone())
    }

    pub fn state() -> (bool, Option<u64>, bool, bool) {
        UI.with(|u| {
            let st = u.borrow();
            (st.edit, st.inspect, st.gallery.is_some(), st.drill.is_some())
        })
    }

    pub fn reset() {
        UI.with(|u| *u.borrow_mut() = UiState::default());
    }
}

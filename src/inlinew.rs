//! Dashboard widgets as **page blocks** — put any dashboard widget (chart, number, task list, timer, table …) anywhere inside a page,
//! without turning the whole page into a dashboard.
//!
//! * A `BlockKind::Widget` block stores the id of its widget in `Block::media`. The widget itself lives on a hidden "store" board
//!   (`Board::page == STORE`), so the whole widget engine (33 widget types, settings inspector, time windows, drill-down) is reused unchanged.
//! * The card has an always-visible *settings* and *⋯* button (finger-sized), so nothing depends on hover or right-click:
//!   settings, replace, width, height, plot type, delete.
//! * `draw` paints one block, `overlays` shows the menu / gallery / inspector (called once per page frame).

use std::cell::RefCell;

use egui::{pos2, vec2, Align2, CursorIcon, Id, Margin, Pos2, Rect, RichText, Sense, Stroke, Ui};

use crate::dashboard::{self as dash, Nav, GAP, HEADER_H, UNIT};
use crate::dashcfg::{self as cfg, MAX_ROWS};
use crate::dashedit::{self, Gallery};
use crate::dashwidgets::{self as dw, Cx, Drill};
use crate::lucide as l;
use crate::model::{Block, Workspace};
use crate::theme::{bold, body, Palette};
use crate::widgets::{menu_item, popup_area, section_label, text_fit, with_alpha};

/// `Board::page` of the hidden board that stores the widgets of widget blocks.
pub const STORE: u64 = u64::MAX;

#[derive(Default)]
struct St {
    /// (widget id, block id, popup position)
    menu: Option<(u64, u64, Pos2)>,
    fresh: bool,
    inspect: Option<u64>,
    /// The widget gallery and the block that receives the chosen widget.
    gallery: Option<(Gallery, u64)>,
    drill: Option<Drill>,
    /// (block id, widget id): the gallery picked a widget for this block.
    assign: Option<(u64, u64)>,
    /// A widget that the user deleted from its menu: its block is removed on the next draw.
    delete: Option<u64>,
}

thread_local! {
    static ST: RefCell<St> = RefCell::new(St::default());
}

/// What `draw` reports back to the editor.
pub struct Out {
    pub changed: bool,
    /// The user deleted the widget: remove the block.
    pub remove: bool,
    pub nav: Nav,
}

/// Opens the widget gallery; the chosen widget is assigned to block `block`.
pub fn request_pick(ws: &mut Workspace, block: u64) {
    let board = ws.dash.ensure_page_board(STORE, "Widgets inside pages");
    ST.with(|s| s.borrow_mut().gallery = Some((Gallery { board, ..Gallery::default() }, block)));
}

/// Width of the card inside a column of `w` points (phones: always the full width).
fn card_width(w: f32, span: u8) -> f32 {
    if w < 520.0 {
        w
    } else {
        (w * span.clamp(3, 12) as f32 / 12.0).clamp(280.0_f32.min(w), w)
    }
}

/// Draws one widget block.
pub fn draw(ui: &mut Ui, ws: &mut Workspace, pal: &Palette, b: &mut Block, w: f32) -> Out {
    let mut out = Out { changed: false, remove: false, nav: Nav::default() };
    let (assigned, deleted) = ST.with(|s| {
        let mut s = s.borrow_mut();
        let a = match s.assign {
            Some((bid, wid)) if bid == b.id => {
                s.assign = None;
                Some(wid)
            }
            _ => None,
        };
        let d = match s.delete {
            Some(wid) if wid == b.media && wid != 0 => {
                s.delete = None;
                true
            }
            _ => false,
        };
        (a, d)
    });
    if deleted {
        ws.dash.remove_widget(b.media);
        b.media = 0;
        out.remove = true;
        out.changed = true;
        return out;
    }
    if let Some(wid) = assigned {
        if b.media != 0 && b.media != wid {
            ws.dash.remove_widget(b.media);
        }
        b.media = wid;
        out.changed = true;
    }
    let wd = if b.media != 0 { ws.dash.widget(b.media).cloned() } else { None };
    let Some(wd) = wd else {
        // nothing chosen yet (or the widget was removed in its settings): a placeholder with the two obvious actions
        let mut pick = false;
        egui::Frame::new().fill(with_alpha(pal.hover, 0.5)).stroke(Stroke::new(1.0, pal.border)).corner_radius(10.0).inner_margin(Margin::same(14)).show(ui, |ui| {
            ui.set_width((w - 30.0).max(120.0));
            ui.label(RichText::new(format!("{}  Dashboard widget", l::LAYOUT_DASHBOARD)).font(bold(15.0)).color(pal.text));
            ui.label(RichText::new("Charts, numbers, task lists, timers … any dashboard widget, right here in the page.").font(body(13.0)).color(pal.muted));
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                if crate::database::pill(ui, pal, "Choose a widget", false, true).clicked() {
                    pick = true;
                }
                if crate::database::pill(ui, pal, "Remove", false, false).clicked() {
                    out.remove = true;
                }
            });
        });
        if pick {
            request_pick(ws, b.id);
        }
        return out;
    };

    let rows = wd.rows.clamp(1, MAX_ROWS) as f32;
    let h = rows * UNIT + (rows - 1.0) * GAP;
    let cw = card_width(w, wd.span);
    let (slot, _) = ui.allocate_exact_size(vec2(w, h + 10.0), Sense::hover());
    let r = Rect::from_min_size(slot.min + vec2(0.0, 4.0), vec2(cw, h));
    if !ui.clip_rect().expand(300.0).intersects(r) {
        return out;
    }
    let def = dw::def_for(&wd);
    let title = dash::auto_title(ws, def.as_ref(), &wd);
    let selected = ST.with(|s| s.borrow().inspect == Some(wd.id) || s.borrow().menu.map_or(false, |m| m.0 == wd.id));
    dash::card_frame(ui, pal, r, false, selected, ui.rect_contains_pointer(r));

    // header: icon · title · settings · menu (both always visible — no hover / right-click needed on a phone)
    let hr = Rect::from_min_size(r.min + vec2(14.0, 6.0), vec2(r.width() - 28.0, HEADER_H - 4.0));
    let mut tx = hr.left();
    if let Some(d) = &def {
        let chip = Rect::from_center_size(pos2(tx + 11.0, hr.center().y), vec2(22.0, 22.0));
        ui.painter().rect_filled(chip, 7.0, pal.accent_soft);
        ui.painter().text(chip.center(), Align2::CENTER_CENTER, d.glyph, body(13.0), pal.accent);
        tx += 30.0;
    }
    let bw = if crate::touch::touchy() { 38.0 } else { 30.0 };
    let more = Rect::from_min_size(pos2(hr.right() - bw + 6.0, hr.center().y - 15.0), vec2(bw, 30.0));
    let gear = Rect::from_min_size(pos2(more.left() - bw, more.top()), vec2(bw, 30.0));
    text_fit(ui.painter(), pos2(tx, hr.center().y), Align2::LEFT_CENTER, &title, bold(14.0), pal.text, (gear.left() - tx - 6.0).max(30.0));
    for (rect, glyph, id, tip) in [(gear, l::SETTINGS, "iw_gear", "Widget settings"), (more, l::ELLIPSIS, "iw_more", "Widget options")] {
        let resp = ui.interact(rect, Id::new((id, wd.id)), Sense::click());
        if resp.hovered() || resp.is_pointer_button_down_on() {
            ui.painter().rect_filled(rect.shrink(2.0), 8.0, pal.hover);
        }
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, body(16.0), pal.muted);
        if resp.on_hover_text(tip).on_hover_cursor(CursorIcon::PointingHand).clicked() {
            ST.with(|s| {
                let mut s = s.borrow_mut();
                if id == "iw_gear" {
                    s.inspect = if s.inspect == Some(wd.id) { None } else { Some(wd.id) };
                } else {
                    s.menu = Some((wd.id, b.id, rect.left_bottom() + vec2(-200.0, 4.0)));
                    s.fresh = true;
                }
            });
        }
    }

    // content
    let content = Rect::from_min_max(pos2(r.left() + 14.0, r.top() + HEADER_H + 4.0), pos2(r.right() - 14.0, r.bottom() - 12.0));
    if content.width() > 20.0 && content.height() > 16.0 {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(content).id_salt(("iw_w", wd.id)).layout(egui::Layout::top_down(egui::Align::Min)));
        child.set_clip_rect(content.intersect(ui.clip_rect()));
        let mut copy = wd.clone();
        let global = ws.dash.range;
        ST.with(|s| {
            let mut s = s.borrow_mut();
            let configure;
            {
                let drill = &mut s.drill;
                let mut cx = Cx { ws, pal, nav: &mut out.nav, drill, global, wid: wd.id, configure: false };
                match &def {
                    Some(d) => (d.draw)(&mut cx, &mut copy, &mut child),
                    None => {
                        child.label(RichText::new(format!("Unknown widget type “{}”.", wd.kind)).font(body(12.5)).color(pal.muted));
                    }
                }
                configure = cx.configure;
            }
            if configure {
                s.inspect = Some(wd.id);
            }
        });
        if copy != wd {
            if let Some(slot) = ws.dash.widget_mut(wd.id) {
                *slot = copy;
            }
        }
    }
    out
}

enum Act {
    Configure(u64),
    Replace(u64),
    Delete(u64),
    Size(u64, u8, u8),
    Plot(u64, &'static str),
    OpenDb(u64),
}

/// Menu, gallery, settings panel and drill-down list of the widget blocks (call once per page frame).
pub fn overlays(ctx: &egui::Context, ws: &mut Workspace, pal: &Palette) -> Nav {
    let mut nav = Nav::default();
    let idle = ST.with(|s| {
        let s = s.borrow();
        s.menu.is_none() && s.gallery.is_none() && s.inspect.is_none() && s.drill.is_none()
    });
    if idle {
        return nav;
    }
    let mut acts: Vec<Act> = vec![];
    ST.with(|s| {
        let mut st = s.borrow_mut();
        if let Some((wid, bid, pos)) = st.menu.take() {
            let mut close = false;
            let (db, span, rows) = ws.dash.widget(wid).map(|w| (w.cfg.db, w.span, w.rows)).unwrap_or((0, 4, 3));
            let plot = ws.dash.widget(wid).and_then(cfg::plot_of);
            let mut fresh = st.fresh;
            let keep = popup_area(ctx, Id::new("iw_menu"), pos, pal, &mut fresh, 240.0, |ui| {
                if menu_item(ui, pal, l::SETTINGS, "Settings", Some("Data, filters, colours"), false, false).clicked() {
                    acts.push(Act::Configure(wid));
                    close = true;
                }
                if menu_item(ui, pal, l::REPLACE, "Replace widget", Some("Pick another one"), false, false).clicked() {
                    acts.push(Act::Replace(bid));
                    close = true;
                }
                if db != 0 && ws.page(db).is_some() && menu_item(ui, pal, l::DATABASE, "Open source database", None, false, false).clicked() {
                    acts.push(Act::OpenDb(db));
                    close = true;
                }
                section_label(ui, pal, "WIDTH");
                ui.horizontal_wrapped(|ui| {
                    for (n, sp) in [("1/3", 4u8), ("1/2", 6), ("2/3", 8), ("Full", 12)] {
                        if dash::chip(ui, pal, n, span == sp).clicked() {
                            acts.push(Act::Size(wid, sp, rows));
                        }
                    }
                });
                section_label(ui, pal, "HEIGHT");
                ui.horizontal_wrapped(|ui| {
                    for (n, r) in [("XS", 1u8), ("S", 2), ("M", 3), ("L", 4), ("XL", 6)] {
                        if dash::chip(ui, pal, n, rows == r).clicked() {
                            acts.push(Act::Size(wid, span, r));
                        }
                    }
                });
                if let Some(plot) = plot.clone() {
                    section_label(ui, pal, "PLOT TYPE");
                    ui.horizontal_wrapped(|ui| {
                        for k in dw::CHART_KINDS {
                            if let Some(d) = dw::def(k) {
                                if dash::glyph_chip(ui, pal, d.glyph, plot == k).on_hover_text(dw::plot_label(k)).clicked() {
                                    acts.push(Act::Plot(wid, k));
                                }
                            }
                        }
                    });
                }
                ui.add_space(2.0);
                if menu_item(ui, pal, l::TRASH_2, "Delete widget", Some("Removes it from the page"), false, true).clicked() {
                    acts.push(Act::Delete(wid));
                    close = true;
                }
            });
            st.fresh = fresh;
            if keep && !close {
                st.menu = Some((wid, bid, pos));
            }
        }
        // drill-down list behind a clicked bar / number
        let mut drill = st.drill.take();
        dash::drill_popup(ctx, ws, pal, &mut drill, &mut nav);
        st.drill = drill;

        if let Some((mut g, bid)) = st.gallery.take() {
            match dashedit::gallery(ctx, ws, pal, &mut g, ctx.screen_rect()) {
                dashedit::GalleryResult::Open => st.gallery = Some((g, bid)),
                dashedit::GalleryResult::Close => {}
                dashedit::GalleryResult::Added(id) => {
                    st.assign = Some((bid, id));
                    st.inspect = Some(id);
                    nav.toast = Some("Widget added — adjust it in the settings panel".into());
                }
            }
        }
        if let Some(id) = st.inspect {
            if ws.dash.widget(id).is_some() {
                if !dashedit::inspector(ctx, ws, pal, id, ctx.screen_rect()) {
                    st.inspect = None;
                }
            } else {
                st.inspect = None;
            }
        }
        for a in acts.drain(..) {
            match a {
                Act::Configure(id) => st.inspect = Some(id),
                Act::Replace(bid) => {
                    let board = ws.dash.ensure_page_board(STORE, "Widgets inside pages");
                    st.gallery = Some((Gallery { board, ..Gallery::default() }, bid));
                }
                Act::Delete(id) => {
                    st.delete = Some(id);
                    if st.inspect == Some(id) {
                        st.inspect = None;
                    }
                }
                Act::Size(id, sp, r) => {
                    if let Some(w) = ws.dash.widget_mut(id) {
                        w.span = sp;
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
        if st.delete.is_some() || st.assign.is_some() {
            ctx.request_repaint();
        }
    });
    nav
}

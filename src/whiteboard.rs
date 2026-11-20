//! Whiteboard block — an infinite, interactive board embedded in any page (the "edgeless" idea, but inside the document).
//!
//! * In the page the block shows a live thumbnail of the board (every shape, frame, card, connector and pen stroke, fitted into the block).
//! * A click (tap) opens the **peek view**: a large overlay with the full canvas editor — shapes, frames, highlighter, eraser, connectors,
//!   templates, zoom bar, frame navigator — plus a slim bar with *info*, *copy SVG*, *export SVG*, *open as page*, *full screen* and *close*.
//! * The board itself is an ordinary canvas page (a child of the page that hosts the block), so it also shows up in the sidebar, in search,
//!   in page links and in every export. Several blocks (or pages) can show the same board.
//!
//! Data: `Block::link` = the canvas page, `Block::size` = height step (0 normal, 1 tall, 2 huge).

use std::cell::RefCell;

use egui::{pos2, vec2, Align2, Color32, Context, Id, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder};

use crate::icons;
use crate::lucide as l;
use crate::model::{Block, PageKind, Workspace};
use crate::theme::{bold, body, Palette};
use crate::widgets::with_alpha;

const HEIGHTS: [f32; 3] = [300.0, 420.0, 560.0];

#[derive(Clone, Copy)]
struct Peek {
    board: u64,
    host: u64,
    full: bool,
    info: bool,
}

thread_local! {
    static PEEK: RefCell<Option<Peek>> = const { RefCell::new(None) };
}

/// Opens the peek view of `board` on top of the page `host`.
pub fn open_peek(ctx: &Context, board: u64, host: u64) {
    // the page behind must not keep the keyboard
    if let Some(id) = ctx.memory(|m| m.focused()) {
        ctx.memory_mut(|m| m.surrender_focus(id));
    }
    PEEK.with(|p| *p.borrow_mut() = Some(Peek { board, host, full: false, info: false }));
    ctx.request_repaint();
}

pub fn close_peek() {
    PEEK.with(|p| *p.borrow_mut() = None);
}

pub fn peek_open() -> bool {
    PEEK.with(|p| p.borrow().is_some())
}

/// What a whiteboard block asks the editor to do.
#[derive(Default)]
pub struct BlockOut {
    pub changed: bool,
    /// The block has no board yet: create one (a canvas page under the host page) and link it.
    pub create: bool,
    /// Open this board as a regular page.
    pub open: Option<u64>,
}

fn ibtn(ui: &mut Ui, pal: &Palette, rect: Rect, id: Id, glyph: &str, tip: &str, active: bool) -> bool {
    let resp = ui.interact(rect, id, Sense::click());
    if active {
        ui.painter().rect_filled(rect, 8.0, pal.accent_soft);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 8.0, pal.hover);
    }
    icons::glyph(ui.painter(), rect.center(), 16.0, glyph, if active { pal.accent } else { pal.text });
    resp.on_hover_text(tip).clicked()
}

fn board_stats(ws: &Workspace, board: u64) -> (usize, usize, usize) {
    ws.page(board).and_then(|p| p.canvas.as_ref()).map_or((0, 0, 0), |c| {
        let frames = c.nodes.iter().filter(|n| n.kind == crate::canvas::Kind::Group).count();
        let ink = c.nodes.iter().filter(|n| n.kind == crate::canvas::Kind::Ink).count();
        (c.nodes.len(), frames, ink)
    })
}

/// The block as it appears in the page.
pub fn block(ui: &mut Ui, ws: &Workspace, host: u64, b: &mut Block, pal: &Palette, w: f32) -> BlockOut {
    let mut out = BlockOut::default();
    let board = b.link.filter(|id| ws.page(*id).map_or(false, |p| p.kind == PageKind::Canvas && !p.trashed));
    let Some(board) = board else {
        egui::Frame::new().fill(with_alpha(pal.hover, 0.5)).stroke(Stroke::new(1.0, pal.border)).corner_radius(10.0).inner_margin(egui::Margin::same(14)).show(ui, |ui| {
            ui.set_width(w - 30.0);
            ui.label(egui::RichText::new(format!("{}  Whiteboard", l::NETWORK)).font(bold(15.0)).color(pal.text));
            ui.label(egui::RichText::new(if b.link.is_some() { "This whiteboard was deleted. Create a new one or pick another:" } else { "An infinite board inside this page: shapes, sticky notes, ink, frames, connectors." }).font(body(13.0)).color(pal.muted));
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                if crate::database::pill(ui, pal, &format!("{}  New whiteboard", l::PLUS), true, false).clicked() {
                    out.create = true;
                }
                for p in ws.pages.iter().filter(|p| p.kind == PageKind::Canvas && !p.trashed) {
                    if crate::database::pill(ui, pal, &format!("{}  {}", l::NETWORK, ws.display_title(p.id)), false, false).clicked() {
                        b.link = Some(p.id);
                        out.changed = true;
                    }
                }
            });
        });
        return out;
    };
    let title = ws.display_title(board);
    // header
    let (hr, _) = ui.allocate_exact_size(vec2(w, 32.0), Sense::hover());
    icons::glyph(ui.painter(), hr.left_center() + vec2(12.0, 0.0), 16.0, l::NETWORK, pal.muted);
    crate::widgets::text_fit(ui.painter(), hr.left_center() + vec2(28.0, 0.0), Align2::LEFT_CENTER, &title, bold(14.0), pal.text, (w - 190.0).max(40.0));
    let mut x = hr.right() - 4.0 - 30.0;
    let bid = b.id;
    for (k, g, tip) in [(0u8, l::X, "Remove from this page (the board is kept)"), (1, l::MOVE_VERTICAL, "Block height"), (2, l::EXTERNAL_LINK, "Open as a page"), (3, l::MAXIMIZE, "Edit here — expanded view")] {
        let r = Rect::from_min_size(pos2(x, hr.top() + 1.0), vec2(30.0, 30.0));
        if ibtn(ui, pal, r, Id::new(("wb_hdr", bid, k)), g, tip, false) {
            match k {
                0 => {
                    b.link = None;
                    out.changed = true;
                }
                1 => {
                    b.size = (b.size + 1) % 3;
                    out.changed = true;
                }
                2 => out.open = Some(board),
                _ => open_peek(ui.ctx(), board, host),
            }
        }
        x -= 32.0;
    }
    // live thumbnail
    let h = HEIGHTS[(b.size % 3) as usize];
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let inner = rect.shrink(1.5);
    if let Some(c) = ws.page(board).and_then(|p| p.canvas.as_ref()) {
        if ui.is_rect_visible(rect) {
            crate::canvasdraw::thumbnail(ui, ws, c, inner, pal);
        }
        if c.nodes.is_empty() {
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, "Empty whiteboard — click to start", body(14.0), pal.muted);
        }
    }
    // rounded frame (hides the square corners of the thumbnail background)
    let corner = Stroke::new(5.0_f32, pal.bg);
    ui.painter().rect_stroke(rect.expand(2.0), 12.0, corner, StrokeKind::Outside);
    ui.painter().rect_stroke(rect, 10.0, Stroke::new(1.0_f32, if resp.hovered() { pal.accent } else { pal.border }), StrokeKind::Inside);
    let chip = Rect::from_center_size(pos2(rect.right() - 62.0, rect.bottom() - 24.0), vec2(104.0, 30.0));
    ui.painter().rect_filled(chip, 15.0, with_alpha(pal.card, 0.94));
    ui.painter().rect_stroke(chip, 15.0, Stroke::new(1.0_f32, pal.border), StrokeKind::Inside);
    icons::glyph(ui.painter(), chip.left_center() + vec2(16.0, 0.0), 14.0, l::PENCIL, if resp.hovered() { pal.accent } else { pal.text });
    ui.painter().text(chip.left_center() + vec2(30.0, 0.0), Align2::LEFT_CENTER, "Edit board", body(12.5), pal.text);
    if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
        open_peek(ui.ctx(), board, host);
    }
    ui.add_space(4.0);
    out
}

/// The peek overlay; call once per frame from the page editor. Returns a page the user asked to open.
pub fn peek(ctx: &Context, ws: &mut Workspace, host: u64, pal: &Palette) -> Option<u64> {
    let mut pk = PEEK.with(|p| *p.borrow())?;
    if pk.host != host || ws.page(pk.board).map_or(true, |p| p.trashed || p.kind != PageKind::Canvas) {
        close_peek();
        return None;
    }
    let screen = ctx.screen_rect();
    let mut close = false;
    let mut open_page = None;
    egui::Area::new(Id::new("wb_peek")).order(crate::widgets::HOST_ORDER).fixed_pos(screen.min).constrain(false).show(ctx, |ui| {
        ui.set_clip_rect(screen);
        let backdrop = ui.allocate_rect(screen, Sense::click());
        ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(if pk.full { 255 } else { 165 }));
        let m = if pk.full { 0.0 } else { (screen.width() * 0.035).clamp(6.0, 48.0) };
        let bar_h = 42.0;
        let bar = Rect::from_min_size(screen.min + vec2(m, m.max(4.0)), vec2(screen.width() - 2.0 * m, bar_h));
        let panel = Rect::from_min_max(pos2(bar.left(), bar.bottom() + 4.0), pos2(screen.right() - m, screen.bottom() - m.max(0.0)));
        if backdrop.clicked() && ctx.input(|i| i.pointer.interact_pos()).map_or(false, |p| !panel.contains(p) && !bar.contains(p)) {
            close = true;
        }
        // top bar
        ui.painter().rect_filled(bar, if pk.full { 0.0 } else { 12.0 }, pal.card);
        let title = ws.display_title(pk.board);
        icons::glyph(ui.painter(), bar.left_center() + vec2(20.0, 0.0), 17.0, l::NETWORK, pal.accent);
        let narrow = bar.width() < 520.0;
        crate::widgets::text_fit(ui.painter(), bar.left_center() + vec2(40.0, 0.0), Align2::LEFT_CENTER, &title, bold(14.5), pal.text, (bar.width() - if narrow { 190.0 } else { 300.0 }).max(40.0));
        let mut x = bar.right() - 6.0 - 34.0;
        let by = bar.top() + 4.0;
        let btn = |ui: &mut Ui, x: &mut f32, k: u8, g: &str, tip: &str, active: bool| -> bool {
            let r = Rect::from_min_size(pos2(*x, by), vec2(34.0, 34.0));
            *x -= 36.0;
            ibtn(ui, pal, r, Id::new(("wb_peek_btn", k)), g, tip, active)
        };
        if btn(ui, &mut x, 0, l::X, "Close", false) {
            close = true;
        }
        if btn(ui, &mut x, 1, if pk.full { l::MINIMIZE } else { l::MAXIMIZE }, if pk.full { "Exit full screen" } else { "Full screen" }, pk.full) {
            pk.full = !pk.full;
        }
        if btn(ui, &mut x, 2, l::EXTERNAL_LINK, "Open as a page", false) {
            open_page = Some(pk.board);
        }
        if !narrow {
            let svg = |ws: &Workspace| ws.page(pk.board).and_then(|p| p.canvas.as_ref()).map(|c| crate::canvasexport::to_svg(c, ctx.style().visuals.dark_mode, &|id| ws.display_title(id)));
            if btn(ui, &mut x, 3, l::DOWNLOAD, "Export as SVG", false) {
                if let Some(s) = svg(ws) {
                    let stem: String = title.chars().map(|ch| if ch.is_alphanumeric() || ch == '-' || ch == '_' { ch } else { '-' }).collect();
                    let msg = crate::platform::save_download(&format!("{}.svg", if stem.trim_matches('-').is_empty() { "whiteboard" } else { stem.as_str() }), s.as_bytes(), "image/svg+xml");
                    crate::dashcfg::set_toast(&msg);
                }
            }
            if btn(ui, &mut x, 4, l::COPY, "Copy as SVG", false) {
                if let Some(s) = svg(ws) {
                    ctx.copy_text(s);
                    crate::dashcfg::set_toast("Board copied as SVG");
                }
            }
        }
        if btn(ui, &mut x, 5, l::INFO, "About this board", pk.info) {
            pk.info = !pk.info;
        }
        // board
        ui.painter().rect_filled(panel, if pk.full { 0.0 } else { 12.0 }, crate::canvasdraw::canvas_bg(pal));
        let child = ui.scope_builder(UiBuilder::new().max_rect(panel.shrink(1.0)), |ui| {
            ui.set_clip_rect(panel.shrink(1.0));
            crate::canvasui::show(ui, ws, pk.board, pal)
        });
        let co = child.inner;
        if let Some(t) = co.toast {
            crate::dashcfg::set_toast(&t);
        }
        if let Some(p) = co.open {
            open_page = Some(p);
        }
        ui.painter().rect_stroke(panel, if pk.full { 0.0 } else { 12.0 }, Stroke::new(1.0_f32, pal.border), StrokeKind::Inside);
        if pk.info {
            let (n, frames, ink) = board_stats(ws, pk.board);
            let r = Rect::from_min_size(pos2(bar.right() - 250.0, bar.bottom() + 12.0 + 40.0), vec2(240.0, 108.0));
            ui.painter().rect_filled(r, 12.0, pal.card);
            ui.painter().rect_stroke(r, 12.0, Stroke::new(1.0_f32, pal.border), StrokeKind::Inside);
            ui.painter().text(r.left_top() + vec2(14.0, 20.0), Align2::LEFT_CENTER, "Whiteboard", bold(13.5), pal.text);
            for (i, line) in [format!("{n} objects · {frames} frames · {ink} strokes"), "Shortcuts: V select · P pen".to_owned(), "S shape · F frame · E eraser".to_owned()].iter().enumerate() {
                ui.painter().text(r.left_top() + vec2(14.0, 44.0 + i as f32 * 20.0), Align2::LEFT_CENTER, line, body(12.5), pal.muted);
            }
        }
    });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !ctx.wants_keyboard_input() && pk.full {
        pk.full = false;
    }
    if close {
        close_peek();
    } else {
        PEEK.with(|p| *p.borrow_mut() = Some(pk));
    }
    if open_page.is_some() {
        close_peek();
    }
    open_page
}

//! PDF annotations: highlight, underline, strike-through, sticky notes (comments) and freehand ink.
//!
//! Annotations are stored per document in the workspace (`Workspace::pdf_annots`) with page-relative coordinates (0‥1), so they
//! stay exactly on the text at every zoom level and survive re-opening the file. The PDF engine (hayro) has no text-selection
//! API, so text marks are drawn like a marker pen: drag across the text. A drag that is only a few pixels tall snaps to a
//! comfortable line height. Every annotation can carry a comment which is edited in the side panel; *Copy* exports all notes as
//! Markdown.

use egui::{pos2, vec2, Align2, Color32, Context, CursorIcon, Id, Key, Painter, Pos2, Rect, Response, ScrollArea, Sense, Shape, Stroke, TextEdit, Ui};

use crate::auth::now_ms;
use crate::icons;
use crate::lucide as l;
use crate::model::{AnnotKind, PdfAnnot};
use crate::theme::{bold, body, Palette};
use crate::widgets::with_alpha;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Hand,
    Highlight,
    Underline,
    Strike,
    Note,
    Pen,
    Eraser,
}

impl Tool {
    const ALL: [(Tool, &'static str, &'static str); 7] = [
        (Tool::Hand, l::MOUSE_POINTER_2, "Select / scroll  (Esc)"),
        (Tool::Highlight, l::HIGHLIGHTER, "Highlight — drag across text"),
        (Tool::Underline, l::UNDERLINE, "Underline — drag along a line"),
        (Tool::Strike, l::STRIKETHROUGH, "Strike through — drag across a line"),
        (Tool::Note, l::STICKY_NOTE, "Note — click where it belongs"),
        (Tool::Pen, l::PEN_LINE, "Pen — draw freehand"),
        (Tool::Eraser, l::ERASER, "Eraser — click or drag over marks"),
    ];
}

pub const COLORS: [Color32; 5] = [
    Color32::from_rgb(255, 204, 0),
    Color32::from_rgb(52, 199, 89),
    Color32::from_rgb(10, 132, 255),
    Color32::from_rgb(255, 55, 95),
    Color32::from_rgb(255, 149, 0),
];

pub fn color_of(i: u8) -> Color32 {
    COLORS[(i as usize).min(COLORS.len() - 1)]
}

/// Highlighter inks (multiplied onto the page, like a real marker / Acrobat's "Multiply" highlight): light, saturated and
/// never darker than the paper, so the black text underneath keeps its full contrast.
const HL_RGB: [[u8; 3]; 5] = [[255, 230, 0], [128, 238, 140], [120, 196, 255], [255, 138, 176], [255, 184, 82]];

/// One highlighted rectangle (page fractions) baked into the page raster.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Mark {
    pub rect: [f32; 4],
    pub rgb: [u8; 3],
}

/// Multiplies (day) or tints (night mode) the highlighter ink into an RGBA tile. `tile` is the pixel origin of the tile inside the
/// page, `page` the pixel size of the whole page. Edges are anti-aliased with a signed-distance rounded box.
pub fn bake(rgba: &mut [u8], size: [usize; 2], tile: [u32; 2], page: [f32; 2], marks: &[Mark], night: bool) {
    let (iw, ih) = (size[0] as i32, size[1] as i32);
    for m in marks {
        let x0 = m.rect[0] * page[0] - tile[0] as f32;
        let x1 = m.rect[2] * page[0] - tile[0] as f32;
        let y0 = m.rect[1] * page[1] - tile[1] as f32;
        let y1 = m.rect[3] * page[1] - tile[1] as f32;
        if x1 <= 0.0 || y1 <= 0.0 || x0 >= iw as f32 || y0 >= ih as f32 || x1 - x0 < 0.5 || y1 - y0 < 0.5 {
            continue;
        }
        let (hx, hy) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let r = (hy * 0.28).clamp(0.4, 3.2).min(hx).min(hy);
        let (px0, px1) = ((x0.floor() as i32 - 1).max(0), (x1.ceil() as i32 + 1).min(iw));
        let (py0, py1) = ((y0.floor() as i32 - 1).max(0), (y1.ceil() as i32 + 1).min(ih));
        for py in py0..py1 {
            for px in px0..px1 {
                let qx = (px as f32 + 0.5 - cx).abs() - (hx - r);
                let qy = (py as f32 + 0.5 - cy).abs() - (hy - r);
                let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
                let cov = (0.5 - d).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let i = ((py * iw + px) * 4) as usize;
                for c in 0..3 {
                    let dst = rgba[i + c] as f32;
                    let ink = m.rgb[c] as f32;
                    let out = if night { dst + (ink - dst) * 0.40 * cov } else { dst * (1.0 - cov * (1.0 - ink / 255.0)) };
                    rgba[i + c] = out.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
}

/// Stops neighbouring line boxes from overlapping (an overlap would be multiplied twice and show as a darker stripe).
fn separate_lines(q: &mut [[f32; 4]]) {
    q.sort_by(|a, b| a[1].partial_cmp(&b[1]).unwrap_or(std::cmp::Ordering::Equal));
    for i in 1..q.len() {
        let (a, b) = (q[i - 1], q[i]);
        let x_overlap = a[0].max(b[0]) < a[2].min(b[2]);
        if x_overlap && b[1] < a[3] {
            let mid = (b[1] + a[3]) / 2.0;
            q[i - 1][3] = mid;
            q[i][1] = mid;
        }
    }
}

/// Smooths a hand-drawn stroke (Chaikin corner cutting) so pen ink looks like ink, not like a polyline.
fn chaikin(pts: &[Pos2], iters: usize) -> Vec<Pos2> {
    let mut v = pts.to_vec();
    for _ in 0..iters {
        if v.len() < 3 {
            break;
        }
        let mut n = Vec::with_capacity(v.len() * 2);
        n.push(v[0]);
        for w in v.windows(2) {
            n.push(w[0].lerp(w[1], 0.25));
            n.push(w[0].lerp(w[1], 0.75));
        }
        n.push(*v.last().unwrap());
        v = n;
    }
    v
}

fn movable(a: &PdfAnnot) -> bool {
    match a.kind {
        AnnotKind::Ink | AnnotKind::Note => true,
        _ => a.quads.is_empty(),
    }
}

fn resizable(a: &PdfAnnot) -> bool {
    a.kind != AnnotKind::Note && movable(a)
}

/// Handle corners (0 = top-left, 1 = top-right, 2 = bottom-right, 3 = bottom-left) in screen space.
fn handle_pos(a: &PdfAnnot, prect: Rect, h: u8) -> Pos2 {
    let b = bounds_of(a, prect).expand(4.0);
    match h {
        0 => b.left_top(),
        1 => b.right_top(),
        2 => b.right_bottom(),
        _ => b.left_bottom(),
    }
}

fn handle_at(a: &PdfAnnot, prect: Rect, pos: Pos2) -> Option<u8> {
    if !resizable(a) {
        return None;
    }
    (0..4u8).find(|h| (handle_pos(a, prect, *h) - pos).length() < 13.0)
}

#[derive(Clone)]
struct Edit {
    id: u64,
    orig: PdfAnnot,
    start: [f32; 2],
    handle: Option<u8>,
    page: usize,
}

fn raw_norm(prect: Rect, pos: Pos2) -> [f32; 2] {
    [(pos.x - prect.left()) / prect.width().max(1.0), (pos.y - prect.top()) / prect.height().max(1.0)]
}

/// The annotation after a move (`handle == None`) or a corner resize, computed from the original and the pointer travel `d`
/// (page fractions). Always stays inside the page.
fn edited(orig: &PdfAnnot, handle: Option<u8>, d: [f32; 2]) -> PdfAnnot {
    let mut a = orig.clone();
    let r = orig.rect;
    match handle {
        None => {
            let dx = d[0].clamp(-r[0].min(r[2]), 1.0 - r[0].max(r[2]));
            let dy = d[1].clamp(-r[1].min(r[3]), 1.0 - r[1].max(r[3]));
            a.rect = [r[0] + dx, r[1] + dy, r[2] + dx, r[3] + dy];
            for p in a.pts.iter_mut() {
                *p = [p[0] + dx, p[1] + dy];
            }
            for q in a.quads.iter_mut() {
                *q = [q[0] + dx, q[1] + dy, q[2] + dx, q[3] + dy];
            }
        }
        Some(h) => {
            let (ax, ay) = match h {
                0 => (r[2], r[3]),
                1 => (r[0], r[3]),
                2 => (r[0], r[1]),
                _ => (r[2], r[1]),
            };
            let (cx, cy) = match h {
                0 => (r[0], r[1]),
                1 => (r[2], r[1]),
                2 => (r[2], r[3]),
                _ => (r[0], r[3]),
            };
            let (nx, ny) = ((cx + d[0]).clamp(0.0, 1.0), (cy + d[1]).clamp(0.0, 1.0));
            let (w0, h0) = (cx - ax, cy - ay);
            let sx = if w0.abs() < 1e-4 { 1.0 } else { (((nx - ax) / w0).max(0.04)).min(60.0) };
            let sy = if h0.abs() < 1e-4 { 1.0 } else { (((ny - ay) / h0).max(0.04)).min(60.0) };
            let fx = |x: f32| (ax + (x - ax) * sx).clamp(0.0, 1.0);
            let fy = |y: f32| (ay + (y - ay) * sy).clamp(0.0, 1.0);
            a.rect = [fx(r[0]), fy(r[1]), fx(r[2]), fy(r[3])];
            for p in a.pts.iter_mut() {
                *p = [fx(p[0]), fy(p[1])];
            }
        }
    }
    a
}

struct Draft {
    page: usize,
    start: [f32; 2],
    cur: [f32; 2],
    pts: Vec<[f32; 2]>,
}

pub struct Notes {
    pub tool: Tool,
    pub color: u8,
    pub list: Vec<PdfAnnot>,
    draft: Option<Draft>,
    pub selected: Option<u64>,
    pub panel: bool,
    pub bar: bool,
    /// Scroll the panel to this annotation on the next frame.
    scroll_to: Option<u64>,
    /// Set whenever `list` changed; the reader copies it back into the workspace.
    pub dirty: bool,
    /// An annotation is being moved / resized with the pointer.
    edit: Option<Edit>,
}

impl Notes {
    pub fn new(list: Vec<PdfAnnot>) -> Notes {
        Notes { tool: Tool::Hand, color: 0, list, draft: None, selected: None, panel: false, bar: false, scroll_to: None, dirty: false, edit: None }
    }

    /// A drawing tool is armed (page dragging is then reserved for marking).
    pub fn capturing(&self) -> bool {
        self.bar && self.tool != Tool::Hand
    }

    fn next_id(&self) -> u64 {
        self.list.iter().map(|a| a.id).max().unwrap_or(0) + 1
    }

    pub fn remove(&mut self, id: u64) {
        let n = self.list.len();
        self.list.retain(|a| a.id != id);
        if self.list.len() != n {
            self.dirty = true;
        }
        if self.selected == Some(id) {
            self.selected = None;
        }
    }

    /// Highlight rectangles of a page, ready to be baked into its raster.
    pub fn marks_on(&self, page: usize) -> Vec<Mark> {
        let mut out = vec![];
        for a in self.on_page(page).filter(|a| a.kind == AnnotKind::Highlight) {
            let rgb = HL_RGB[(a.color as usize).min(HL_RGB.len() - 1)];
            let mut q: Vec<[f32; 4]> = if a.quads.is_empty() { vec![a.rect] } else { a.quads.clone() };
            separate_lines(&mut q);
            out.extend(q.into_iter().map(|rect| Mark { rect, rgb }));
        }
        out
    }

    /// Changes whenever the highlights of a page change (part of the tile cache key).
    pub fn marks_sig(&self, page: usize) -> u32 {
        let mut h: u32 = 2166136261;
        let mut mix = |v: u32| {
            h ^= v;
            h = h.wrapping_mul(16777619);
        };
        for a in self.on_page(page).filter(|a| a.kind == AnnotKind::Highlight) {
            mix(a.color as u32 + 1);
            if a.quads.is_empty() {
                a.rect.iter().for_each(|v| mix(v.to_bits()));
            }
            for q in &a.quads {
                q.iter().for_each(|v| mix(v.to_bits()));
            }
        }
        h
    }

    pub fn on_page(&self, page: usize) -> impl Iterator<Item = &PdfAnnot> {
        self.list.iter().filter(move |a| a.page as usize == page)
    }

    /// Keyboard: Delete removes the selected annotation, Esc leaves the current tool. Returns true when Esc was consumed.
    pub fn keys(&mut self, ctx: &Context, editing: bool) -> bool {
        if editing {
            return false;
        }
        let (del, esc) = ctx.input(|i| (i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace), i.key_pressed(Key::Escape)));
        if del {
            if let Some(id) = self.selected {
                self.remove(id);
            }
        }
        if esc && (self.tool != Tool::Hand || self.draft.is_some()) {
            self.tool = Tool::Hand;
            self.draft = None;
            return true;
        }
        false
    }

    // ───────────── painting ─────────────

    /// `vector_hl`: draw highlights as a translucent overlay (only needed when the page raster is not available, because
    /// committed highlights are normally multiplied into the tiles by [`bake`]).
    pub fn paint_page(&self, p: &Painter, prect: Rect, page: usize, night: bool, accent: Color32, vector_hl: bool) {
        let ink_w = (prect.width() / 612.0 * 2.0).clamp(1.4, 5.5);
        for a in self.on_page(page) {
            let col = color_of(a.color);
            let r = rect_of(a, prect);
            match a.kind {
                AnnotKind::Highlight if vector_hl => {
                    let rgb = HL_RGB[(a.color as usize).min(HL_RGB.len() - 1)];
                    let c = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                    let mut q: Vec<[f32; 4]> = if a.quads.is_empty() { vec![a.rect] } else { a.quads.clone() };
                    separate_lines(&mut q);
                    for q in q {
                        let qr = quad_rect(q, prect);
                        p.rect_filled(qr, (qr.height() * 0.2).min(3.0), with_alpha(c, if night { 0.4 } else { 0.55 }));
                    }
                }
                AnnotKind::Highlight => {}
                AnnotKind::Underline | AnnotKind::Strike => {
                    let quads: Vec<[f32; 4]> = if a.quads.is_empty() { vec![a.rect] } else { a.quads.clone() };
                    for q in &quads {
                        let qr = quad_rect(*q, prect);
                        let th = (qr.height() * 0.075).clamp(1.2, 3.2).max(if a.quads.is_empty() { ink_w } else { 0.0 });
                        let y = if a.quads.is_empty() { qr.center().y } else if a.kind == AnnotKind::Underline { qr.top() + qr.height() * 0.9 } else { qr.top() + qr.height() * 0.54 };
                        let (x0, x1) = (qr.left() + th * 0.5, (qr.right() - th * 0.5).max(qr.left() + th * 0.5));
                        p.line_segment([pos2(x0, y), pos2(x1, y)], Stroke::new(th, col));
                        p.circle_filled(pos2(x0, y), th * 0.5, col);
                        p.circle_filled(pos2(x1, y), th * 0.5, col);
                    }
                }
                AnnotKind::Ink => {
                    let raw: Vec<Pos2> = a.pts.iter().map(|q| pos_of(*q, prect)).collect();
                    paint_ink(p, &raw, ink_w, col);
                }
                AnnotKind::Note => {
                    let c = r.min;
                    let nr = Rect::from_center_size(c, vec2(26.0, 26.0));
                    p.rect_filled(nr.translate(vec2(0.0, 2.0)).expand(1.0), 8.0, Color32::from_black_alpha(46));
                    p.rect_filled(nr, 8.0, col);
                    p.rect_stroke(nr, 8.0, Stroke::new(1.0_f32, Color32::from_black_alpha(40)), egui::StrokeKind::Inside);
                    p.text(c, Align2::CENTER_CENTER, l::MESSAGE_SQUARE, icons::font(15.0), Color32::from_gray(30));
                }
            }
            if a.kind != AnnotKind::Note && !a.text.is_empty() {
                let c = pos2(r.right() + 2.0, r.top() - 2.0);
                p.circle_filled(c, 8.0, col);
                p.circle_stroke(c, 8.0, Stroke::new(1.0_f32, Color32::from_black_alpha(40)));
                p.text(c, Align2::CENTER_CENTER, l::MESSAGE_SQUARE, icons::font(10.0), Color32::from_gray(30));
            }
            if self.selected == Some(a.id) {
                let b = bounds_of(a, prect).expand(4.0);
                p.rect_stroke(b, 3.0, Stroke::new(1.5_f32, accent), egui::StrokeKind::Middle);
                if resizable(a) {
                    for h in 0..4u8 {
                        let c = handle_pos(a, prect, h);
                        p.circle_filled(c, 6.0, Color32::WHITE);
                        p.circle_stroke(c, 6.0, Stroke::new(2.0_f32, accent));
                    }
                }
            }
        }
        // the mark being drawn right now
        if let Some(d) = self.draft.as_ref().filter(|d| d.page == page) {
            let col = color_of(self.color);
            match self.tool {
                Tool::Highlight => {
                    let r = draft_rect(d, prect, self.tool);
                    let rgb = HL_RGB[(self.color as usize).min(HL_RGB.len() - 1)];
                    p.rect_filled(r, (r.height() * 0.2).min(3.0), with_alpha(Color32::from_rgb(rgb[0], rgb[1], rgb[2]), if night { 0.4 } else { 0.55 }));
                }
                Tool::Underline | Tool::Strike => {
                    let r = draft_rect(d, prect, self.tool);
                    p.line_segment([r.left_center(), r.right_center()], Stroke::new(2.4_f32, col));
                }
                Tool::Pen => {
                    let raw: Vec<Pos2> = d.pts.iter().map(|q| pos_of(*q, prect)).collect();
                    paint_ink(p, &raw, ink_w, col);
                }
                _ => {}
            }
        }
    }

    // ───────────── input ─────────────

    /// Moving / resizing of the selected (or pressed) annotation: drag the body to move it, drag a corner handle to scale it.
    /// Returns `true` while it owns the drag (the page must not pan or select text then).
    pub fn drag_edit(&mut self, ctx: &Context, resp: &Response, pages: &[(usize, Rect)]) -> bool {
        if self.capturing() {
            self.edit = None;
            return false;
        }
        let page_rect = |pg: usize| pages.iter().find(|(p, _)| *p == pg).map(|(_, r)| *r);
        if resp.drag_started() {
            if let Some(o) = ctx.input(|i| i.pointer.press_origin()) {
                let mut start: Option<(u64, Option<u8>)> = None;
                if let Some(a) = self.selected.and_then(|id| self.list.iter().find(|a| a.id == id)) {
                    if let Some(h) = page_rect(a.page as usize).and_then(|pr| handle_at(a, pr, o)) {
                        start = Some((a.id, Some(h)));
                    }
                }
                if start.is_none() {
                    if let Some(id) = self.hit(o, pages) {
                        if self.list.iter().any(|a| a.id == id && movable(a)) {
                            start = Some((id, None));
                        }
                    }
                }
                if let Some((id, handle)) = start {
                    if let Some(a) = self.list.iter().find(|a| a.id == id).cloned() {
                        if let Some(pr) = page_rect(a.page as usize) {
                            self.edit = Some(Edit { id, page: a.page as usize, orig: a, start: raw_norm(pr, o), handle });
                            self.selected = Some(id);
                        }
                    }
                }
            }
        }
        // hover cursor
        if self.edit.is_none() && resp.hovered() {
            if let (Some(h), Some(a)) = (ctx.pointer_hover_pos(), self.selected.and_then(|id| self.list.iter().find(|a| a.id == id))) {
                if let Some(pr) = page_rect(a.page as usize) {
                    match handle_at(a, pr, h) {
                        Some(0) | Some(2) => ctx.set_cursor_icon(CursorIcon::ResizeNwSe),
                        Some(_) => ctx.set_cursor_icon(CursorIcon::ResizeNeSw),
                        None => {
                            if movable(a) && bounds_of(a, pr).expand(4.0).contains(h) && self.hit(h, pages) == Some(a.id) {
                                ctx.set_cursor_icon(CursorIcon::Grab);
                            }
                        }
                    }
                }
            }
        }
        let Some(e) = self.edit.clone() else { return false };
        if resp.drag_stopped() || !ctx.input(|i| i.pointer.primary_down()) {
            self.edit = None;
            return false;
        }
        if let (Some(pos), Some(pr)) = (ctx.pointer_latest_pos(), page_rect(e.page)) {
            let n = raw_norm(pr, pos);
            let na = edited(&e.orig, e.handle, [n[0] - e.start[0], n[1] - e.start[1]]);
            if let Some(a) = self.list.iter_mut().find(|a| a.id == e.id) {
                if *a != na {
                    *a = na;
                    self.dirty = true;
                }
            }
            ctx.set_cursor_icon(match e.handle {
                Some(0) | Some(2) => CursorIcon::ResizeNwSe,
                Some(_) => CursorIcon::ResizeNeSw,
                None => CursorIcon::Grabbing,
            });
            ctx.request_repaint();
        }
        true
    }

    /// Handles pointer input on the page surface. `pages` are the on-screen rectangles of the visible pages.
    pub fn input(&mut self, ctx: &Context, resp: &Response, pages: &[(usize, Rect)]) {
        let pos = resp.interact_pointer_pos().or_else(|| ctx.pointer_hover_pos());
        let page_at = |pos: Pos2| pages.iter().find(|(_, r)| r.expand(2.0).contains(pos)).copied();
        if self.capturing() && resp.hovered() {
            ctx.set_cursor_icon(CursorIcon::Crosshair);
        }
        match self.tool {
            Tool::Highlight | Tool::Underline | Tool::Strike | Tool::Pen if self.capturing() => {
                if resp.drag_started() {
                    if let Some((pg, r)) = pos.and_then(page_at) {
                        let n = norm(r, pos.unwrap_or(r.center()));
                        self.draft = Some(Draft { page: pg, start: n, cur: n, pts: vec![n] });
                    }
                }
                if resp.dragged() {
                    let pen = self.tool == Tool::Pen;
                    if let (Some(d), Some(pos)) = (self.draft.as_mut(), pos) {
                        if let Some((_, r)) = pages.iter().find(|(pg, _)| *pg == d.page) {
                            let n = norm(*r, pos);
                            d.cur = n;
                            let last = d.pts.last().copied().unwrap_or(n);
                            let dpx = vec2((n[0] - last[0]) * r.width(), (n[1] - last[1]) * r.height()).length();
                            if pen && dpx > 1.5 && d.pts.len() < 4000 {
                                d.pts.push(n);
                            }
                        }
                    }
                    ctx.request_repaint();
                }
                if resp.drag_stopped() {
                    if let Some(d) = self.draft.take() {
                        if let Some((_, r)) = pages.iter().find(|(pg, _)| *pg == d.page) {
                            self.finish(d, *r);
                        }
                    }
                }
            }
            Tool::Note if self.capturing() => {
                if resp.clicked() {
                    if let Some((pg, r)) = pos.and_then(page_at) {
                        let n = norm(r, pos.unwrap_or(r.center()));
                        let id = self.next_id();
                        self.list.push(PdfAnnot { id, page: pg as u32, kind: AnnotKind::Note, rect: [n[0], n[1], n[0], n[1]], pts: vec![], color: self.color, text: String::new(), created: now_ms(), quads: vec![], quote: String::new() });
                        self.selected = Some(id);
                        self.panel = true;
                        self.scroll_to = Some(id);
                        self.dirty = true;
                        self.tool = Tool::Hand;
                    }
                }
            }
            Tool::Eraser if self.capturing() => {
                if resp.clicked() || resp.dragged() {
                    if let Some(pos) = pos {
                        if let Some(id) = self.hit(pos, pages) {
                            self.remove(id);
                        }
                    }
                }
            }
            _ => {
                if resp.clicked() {
                    match pos.and_then(|p| self.hit(p, pages)) {
                        Some(id) => {
                            self.selected = Some(id);
                            if self.bar {
                                self.panel = true;
                                self.scroll_to = Some(id);
                            }
                        }
                        None => self.selected = None,
                    }
                }
            }
        }
    }

    /// Creates one text mark per page piece of a selection (`quads` are page fractions per line). Returns the id of the first mark.
    pub fn add_text_marks(&mut self, kind: AnnotKind, pieces: &[crate::pdftext::Piece]) -> Option<u64> {
        let mut first = None;
        for pc in pieces {
            let (w, h) = (pc.w.max(1.0), pc.h.max(1.0));
            let quads: Vec<[f32; 4]> = pc.quads.iter().map(|q| [q[0] / w, q[1] / h, q[2] / w, q[3] / h]).collect();
            if quads.is_empty() {
                continue;
            }
            let rect = quads.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |r, q| [r[0].min(q[0]), r[1].min(q[1]), r[2].max(q[2]), r[3].max(q[3])]);
            let id = self.next_id();
            self.list.push(PdfAnnot { id, page: pc.page as u32, kind, rect, pts: vec![], color: self.color, text: String::new(), created: now_ms(), quads, quote: pc.text.clone() });
            first.get_or_insert(id);
        }
        if let Some(id) = first {
            self.selected = Some(id);
            self.dirty = true;
        }
        first
    }

    /// Opens the notes panel on an annotation so its comment can be typed.
    pub fn open_comment(&mut self, id: u64) {
        self.bar = true;
        self.panel = true;
        self.selected = Some(id);
        self.scroll_to = Some(id);
    }

    fn finish(&mut self, d: Draft, prect: Rect) {
        let (w, h) = (prect.width().max(1.0), prect.height().max(1.0));
        let (dx, dy) = ((d.cur[0] - d.start[0]).abs() * w, (d.cur[1] - d.start[1]).abs() * h);
        let id = self.next_id();
        let page = d.page as u32;
        let color = self.color;
        let mk = |kind, rect: [f32; 4], pts: Vec<[f32; 2]>| PdfAnnot { id, page, kind, rect, pts, color, text: String::new(), created: now_ms(), quads: vec![], quote: String::new() };
        let a = match self.tool {
            Tool::Highlight | Tool::Underline | Tool::Strike => {
                if dx < 5.0 && dy < 5.0 {
                    return;
                }
                let r = draft_rect(&d, prect, self.tool);
                let n = [(r.left() - prect.left()) / w, (r.top() - prect.top()) / h, (r.right() - prect.left()) / w, (r.bottom() - prect.top()) / h];
                let kind = match self.tool {
                    Tool::Highlight => AnnotKind::Highlight,
                    Tool::Underline => AnnotKind::Underline,
                    _ => AnnotKind::Strike,
                };
                mk(kind, n, vec![])
            }
            Tool::Pen => {
                if d.pts.len() < 2 {
                    return;
                }
                let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for q in &d.pts {
                    x0 = x0.min(q[0]);
                    y0 = y0.min(q[1]);
                    x1 = x1.max(q[0]);
                    y1 = y1.max(q[1]);
                }
                mk(AnnotKind::Ink, [x0, y0, x1, y1], d.pts.clone())
            }
            _ => return,
        };
        self.selected = Some(a.id);
        self.list.push(a);
        self.dirty = true;
    }

    /// Topmost annotation under the pointer.
    fn hit(&self, pos: Pos2, pages: &[(usize, Rect)]) -> Option<u64> {
        for a in self.list.iter().rev() {
            let Some((_, prect)) = pages.iter().find(|(pg, _)| *pg == a.page as usize) else { continue };
            let hit = match a.kind {
                AnnotKind::Ink => {
                    let pts: Vec<Pos2> = a.pts.iter().map(|q| pos_of(*q, *prect)).collect();
                    pts.windows(2).any(|w| dist_to_segment(pos, w[0], w[1]) < 7.0)
                }
                AnnotKind::Note => Rect::from_center_size(pos_of([a.rect[0], a.rect[1]], *prect), vec2(28.0, 28.0)).contains(pos),
                _ if !a.quads.is_empty() => a.quads.iter().any(|q| quad_rect(*q, *prect).expand(3.0).contains(pos)),
                _ => bounds_of(a, *prect).expand(4.0).contains(pos),
            };
            if hit {
                return Some(a.id);
            }
        }
        None
    }

    /// Markdown export of every annotation (page order).
    pub fn markdown(&self, title: &str) -> String {
        let mut v: Vec<&PdfAnnot> = self.list.iter().collect();
        v.sort_by(|a, b| (a.page, a.rect[1].to_bits()).cmp(&(b.page, b.rect[1].to_bits())));
        let mut out = format!("# Notes on {title}\n\n");
        for a in v {
            out.push_str(&format!("- **p. {}** · {}", a.page + 1, kind_name(a.kind)));
            if !a.quote.trim().is_empty() {
                out.push_str(&format!(": “{}”", a.quote.trim().replace('\n', " ")));
            }
            if !a.text.trim().is_empty() {
                out.push_str(&format!(" — {}", a.text.trim().replace('\n', " ")));
            }
            out.push('\n');
        }
        out
    }
}

fn kind_name(k: AnnotKind) -> &'static str {
    match k {
        AnnotKind::Highlight => "Highlight",
        AnnotKind::Underline => "Underline",
        AnnotKind::Strike => "Strike-through",
        AnnotKind::Note => "Note",
        AnnotKind::Ink => "Drawing",
    }
}

fn paint_ink(p: &Painter, raw: &[Pos2], w: f32, col: Color32) {
    match raw.len() {
        0 => {}
        1 => {
            p.circle_filled(raw[0], w * 0.5, col);
        }
        _ => {
            let pts = chaikin(raw, 2);
            p.add(Shape::line(pts, Stroke::new(w, col)));
            p.circle_filled(raw[0], w * 0.5, col);
            p.circle_filled(*raw.last().unwrap(), w * 0.5, col);
        }
    }
}

// ───────────── geometry helpers ─────────────

fn quad_rect(q: [f32; 4], prect: Rect) -> Rect {
    Rect::from_min_max(pos_of([q[0], q[1]], prect), pos_of([q[2], q[3]], prect))
}

fn norm(prect: Rect, pos: Pos2) -> [f32; 2] {
    [((pos.x - prect.left()) / prect.width().max(1.0)).clamp(0.0, 1.0), ((pos.y - prect.top()) / prect.height().max(1.0)).clamp(0.0, 1.0)]
}

fn pos_of(n: [f32; 2], prect: Rect) -> Pos2 {
    pos2(prect.left() + n[0] * prect.width(), prect.top() + n[1] * prect.height())
}

fn rect_of(a: &PdfAnnot, prect: Rect) -> Rect {
    Rect::from_min_max(pos_of([a.rect[0], a.rect[1]], prect), pos_of([a.rect[2], a.rect[3]], prect))
}

fn bounds_of(a: &PdfAnnot, prect: Rect) -> Rect {
    let r = rect_of(a, prect);
    match a.kind {
        AnnotKind::Underline | AnnotKind::Strike => Rect::from_min_max(pos2(r.left(), r.center().y - 4.0), pos2(r.right(), r.center().y + 4.0)),
        AnnotKind::Note => Rect::from_center_size(r.min, vec2(24.0, 24.0)),
        _ => r,
    }
}

/// Rectangle (screen space) a drag produces: thin drags snap to a line height; underline / strike use the mean height.
fn draft_rect(d: &Draft, prect: Rect, tool: Tool) -> Rect {
    let a = pos_of(d.start, prect);
    let b = pos_of(d.cur, prect);
    let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
    let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
    match tool {
        Tool::Highlight => {
            let line = (prect.height() / 792.0 * 13.0).clamp(9.0, 40.0);
            if y1 - y0 < line * 0.8 {
                let c = (y0 + y1) / 2.0;
                Rect::from_min_max(pos2(x0, c - line / 2.0), pos2(x1, c + line / 2.0))
            } else {
                Rect::from_min_max(pos2(x0, y0), pos2(x1, y1))
            }
        }
        _ => {
            let c = (a.y + b.y) / 2.0;
            Rect::from_min_max(pos2(x0, c), pos2(x1, c))
        }
    }
}

fn dist_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_sq();
    if len2 < 1e-6 {
        return (p - a).length();
    }
    let t = (((p - a).dot(ab)) / len2).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

// ───────────── toolbar ─────────────

pub fn bar_height(narrow: bool) -> f32 {
    if narrow {
        84.0
    } else {
        46.0
    }
}

fn small_button(ui: &mut Ui, pal: &Palette, rect: Rect, id: &str, glyph: &str, tip: &str, active: bool) -> bool {
    let resp = ui.interact(rect, Id::new(("pdfn", id)), Sense::click());
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
    let bg = if active { with_alpha(pal.accent, 0.9) } else { with_alpha(pal.active, h) };
    ui.painter().rect_filled(rect, 8.0, bg);
    let col = if active { Color32::WHITE } else { pal.text };
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, icons::font(16.0), col);
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip).clicked()
}

pub fn bar(ui: &mut Ui, pal: &Palette, rect: Rect, notes: &mut Notes, narrow: bool) {
    ui.painter().rect_filled(rect, 0.0, with_alpha(pal.bg, 0.97));
    ui.painter().hline(rect.x_range(), rect.bottom(), Stroke::new(1.0_f32, pal.border));
    let bs = if narrow { vec2(36.0, 34.0) } else { vec2(38.0, 34.0) };
    let mut x = rect.left() + 8.0;
    let y1 = rect.top() + 6.0;
    for (tool, glyph, tip) in Tool::ALL {
        let r = Rect::from_min_size(pos2(x, y1), bs);
        if small_button(ui, pal, r, tip, glyph, tip, notes.tool == tool) {
            notes.tool = tool;
        }
        x += bs.x + 3.0;
    }
    // colours (second row on narrow screens)
    let (mut cx, cy) = if narrow { (rect.left() + 22.0, rect.top() + 6.0 + bs.y + 8.0 + 17.0) } else { (x + 22.0, y1 + bs.y / 2.0) };
    for (i, c) in COLORS.iter().enumerate() {
        let r = Rect::from_center_size(pos2(cx, cy), vec2(26.0, 26.0));
        let resp = ui.interact(r, Id::new(("pdfn_col", i)), Sense::click());
        let sel = notes.color as usize == i;
        ui.painter().circle_filled(r.center(), if sel { 9.5 } else { 8.0 }, *c);
        if sel {
            ui.painter().circle_stroke(r.center(), 12.0, Stroke::new(2.0_f32, pal.text));
        }
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            notes.color = i as u8;
            if let Some(id) = notes.selected {
                if let Some(a) = notes.list.iter_mut().find(|a| a.id == id) {
                    a.color = i as u8;
                    notes.dirty = true;
                }
            }
        }
        cx += 30.0;
    }
    let lx = cx + 6.0;
    let r = Rect::from_min_size(pos2(lx, cy - bs.y / 2.0), vec2(112.0, bs.y));
    let resp = ui.interact(r, Id::new("pdfn_panel"), Sense::click());
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
    ui.painter().rect_filled(r, 8.0, if notes.panel { with_alpha(pal.accent, 0.9) } else { with_alpha(pal.active, h) });
    let col = if notes.panel { Color32::WHITE } else { pal.text };
    ui.painter().text(pos2(r.left() + 18.0, r.center().y), Align2::CENTER_CENTER, l::LIST, icons::font(15.0), col);
    ui.painter().text(pos2(r.left() + 34.0, r.center().y), Align2::LEFT_CENTER, format!("Notes · {}", notes.list.len()), body(13.0), col);
    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
        notes.panel = !notes.panel;
    }
}

// ───────────── side panel ─────────────

pub fn panel(ui: &mut Ui, ctx: &Context, pal: &Palette, rect: Rect, notes: &mut Notes, title: &str, goto_req: &mut Option<usize>) {
    ui.painter().rect_filled(rect, 0.0, pal.bg);
    ui.painter().vline(rect.left(), rect.y_range(), Stroke::new(1.0_f32, pal.border));
    let head = Rect::from_min_size(rect.min, vec2(rect.width(), 44.0));
    crate::widgets::text_fit(ui.painter(), pos2(head.left() + 14.0, head.center().y), Align2::LEFT_CENTER, &format!("Annotations ({})", notes.list.len()), bold(14.0), pal.text, (head.width() - 140.0).max(40.0));
    let copy = Rect::from_center_size(pos2(head.right() - 74.0, head.center().y), vec2(60.0, 28.0));
    let resp = ui.interact(copy, Id::new("pdfn_copy"), Sense::click());
    ui.painter().rect_filled(copy, 7.0, with_alpha(pal.active, if resp.hovered() { 1.0 } else { 0.6 }));
    ui.painter().text(copy.center(), Align2::CENTER_CENTER, "Copy", body(13.0), pal.text);
    if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Copy all annotations as Markdown").clicked() && !notes.list.is_empty() {
        ctx.copy_text(notes.markdown(title));
    }
    let close = Rect::from_center_size(pos2(head.right() - 24.0, head.center().y), vec2(28.0, 28.0));
    if small_button(ui, pal, close, "pdfn_close_panel", l::X, "Close the list", false) {
        notes.panel = false;
    }
    ui.painter().hline(head.x_range(), head.bottom(), Stroke::new(1.0_f32, pal.border));

    let body_rect = Rect::from_min_max(pos2(rect.left() + 8.0, head.bottom() + 6.0), pos2(rect.right() - 8.0, rect.bottom() - 6.0));
    if notes.list.is_empty() {
        crate::widgets::text_fit(ui.painter(), pos2(body_rect.left() + 6.0, body_rect.top() + 18.0), Align2::LEFT_CENTER, "Nothing marked yet.", body(13.5), pal.muted, body_rect.width() - 12.0);
        crate::widgets::text_fit(ui.painter(), pos2(body_rect.left() + 6.0, body_rect.top() + 40.0), Align2::LEFT_CENTER, "Pick a tool above and mark the page.", body(12.5), pal.muted, body_rect.width() - 12.0);
        return;
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body_rect).layout(egui::Layout::top_down(egui::Align::Min)));
    child.set_clip_rect(body_rect.intersect(ui.clip_rect()));
    let mut order: Vec<usize> = (0..notes.list.len()).collect();
    order.sort_by(|a, b| {
        let (x, y) = (&notes.list[*a], &notes.list[*b]);
        (x.page, x.rect[1].to_bits()).cmp(&(y.page, y.rect[1].to_bits()))
    });
    let mut delete: Option<u64> = None;
    let mut select: Option<(u64, usize)> = None;
    let scroll_to = notes.scroll_to.take();
    let mut changed = false;
    let selected = notes.selected;
    ScrollArea::vertical().id_salt("pdfn_list").auto_shrink([false, false]).show(&mut child, |ui| {
        ui.set_width(body_rect.width() - 8.0);
        for idx in order {
            let a = &mut notes.list[idx];
            let sel = selected == Some(a.id);
            let col = color_of(a.color);
            let frame = egui::Frame::new()
                .fill(if sel { with_alpha(pal.accent, 0.10) } else { with_alpha(pal.hover, 0.5) })
                .stroke(Stroke::new(if sel { 1.5_f32 } else { 1.0_f32 }, if sel { pal.accent } else { pal.border }))
                .corner_radius(9.0)
                .inner_margin(egui::Margin::same(8));
            let out = frame.show(ui, |ui| {
                ui.set_width(ui.available_width());
                let (hr, hresp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
                ui.painter().circle_filled(pos2(hr.left() + 7.0, hr.center().y), 6.0, col);
                crate::widgets::text_fit(ui.painter(), pos2(hr.left() + 20.0, hr.center().y), Align2::LEFT_CENTER, &format!("{} · page {}", kind_name(a.kind), a.page + 1), bold(12.5), pal.text, hr.width() - 52.0);
                let del = Rect::from_center_size(pos2(hr.right() - 11.0, hr.center().y), vec2(22.0, 22.0));
                let dresp = ui.interact(del, Id::new(("pdfn_del", a.id)), Sense::click());
                icons::paint(ui.painter(), del.center(), 13.0, icons::Icon::Trash, if dresp.hovered() { pal.danger } else { pal.muted });
                if dresp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Delete").clicked() {
                    delete = Some(a.id);
                }
                if hresp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    select = Some((a.id, a.page as usize));
                }
                if !a.quote.is_empty() {
                    let q = if a.quote.chars().count() > 180 { format!("{}…", a.quote.chars().take(180).collect::<String>()) } else { a.quote.clone() };
                    ui.add(egui::Label::new(egui::RichText::new(format!("“{q}”")).font(body(12.5)).italics().color(pal.muted)).wrap());
                }
                let te = TextEdit::multiline(&mut a.text).id(Id::new(("pdfn_text", a.id))).desired_rows(2).desired_width(f32::INFINITY).font(body(13.5)).hint_text("Add a comment…");
                let r = ui.add(te);
                if r.changed() {
                    changed = true;
                }
                if r.has_focus() && !sel {
                    select = Some((a.id, usize::MAX));
                }
            });
            if scroll_to == Some(a.id) {
                out.response.scroll_to_me(Some(egui::Align::Center));
            }
            ui.add_space(6.0);
        }
    });
    if changed {
        notes.dirty = true;
    }
    if let Some(id) = delete {
        notes.remove(id);
    }
    if let Some((id, page)) = select {
        notes.selected = Some(id);
        if page != usize::MAX {
            *goto_req = Some(page);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr() -> Rect {
        Rect::from_min_size(pos2(100.0, 50.0), vec2(600.0, 800.0))
    }

    #[test]
    fn coordinates_round_trip_at_any_zoom() {
        let n = norm(pr(), pos2(250.0, 450.0));
        assert!((n[0] - 0.25).abs() < 1e-6 && (n[1] - 0.5).abs() < 1e-6);
        let small = Rect::from_min_size(pos2(0.0, 0.0), vec2(150.0, 200.0));
        let p = pos_of(n, small);
        assert!((p.x - 37.5).abs() < 1e-4 && (p.y - 100.0).abs() < 1e-4);
        assert_eq!(norm(pr(), pos2(-50.0, 9999.0)), [0.0, 1.0]);
    }

    #[test]
    fn thin_highlight_drag_snaps_to_a_line_height() {
        let d = Draft { page: 0, start: [0.1, 0.5], cur: [0.6, 0.5], pts: vec![] };
        let r = draft_rect(&d, pr(), Tool::Highlight);
        assert!(r.height() >= 9.0 && r.width() > 100.0);
    }

    #[test]
    fn create_hit_test_erase_and_export() {
        let mut n = Notes::new(vec![]);
        n.bar = true;
        n.tool = Tool::Highlight;
        n.finish(Draft { page: 2, start: [0.1, 0.2], cur: [0.5, 0.22], pts: vec![] }, pr());
        assert_eq!(n.list.len(), 1);
        assert!(n.dirty);
        // a tiny accidental click creates nothing
        n.finish(Draft { page: 2, start: [0.1, 0.2], cur: [0.1, 0.2], pts: vec![] }, pr());
        assert_eq!(n.list.len(), 1);
        let pages = [(2usize, pr())];
        assert_eq!(n.hit(pos_of([0.3, 0.21], pr()), &pages), Some(n.list[0].id));
        assert_eq!(n.hit(pos_of([0.9, 0.9], pr()), &pages), None);
        n.list[0].text = "check this".into();
        let md = n.markdown("Doc");
        assert!(md.contains("p. 3") && md.contains("check this"));
        let id = n.list[0].id;
        n.remove(id);
        assert!(n.list.is_empty());
    }

    #[test]
    fn highlight_multiplies_so_black_text_stays_black() {
        // 20×10 tile: white paper with one black pixel; a yellow mark covers the whole tile
        let mut px = vec![255u8; 20 * 10 * 4];
        let black = (5 * 20 + 7) * 4;
        px[black..black + 3].copy_from_slice(&[0, 0, 0]);
        let m = Mark { rect: [0.0, 0.0, 1.0, 1.0], rgb: [255, 230, 0] };
        bake(&mut px, [20, 10], [0, 0], [20.0, 10.0], &[m], false);
        assert_eq!(&px[black..black + 3], &[0, 0, 0], "text keeps full contrast (alpha blending would have turned it brown)");
        let mid = (5 * 20 + 12) * 4;
        assert_eq!(&px[mid..mid + 3], &[255, 230, 0], "paper takes the ink colour");
        // outside the mark nothing changes, edges are anti-aliased
        let mut px = vec![255u8; 40 * 20 * 4];
        let m = Mark { rect: [0.25, 0.25, 0.75, 0.75], rgb: [255, 230, 0] };
        bake(&mut px, [40, 20], [0, 0], [40.0, 20.0], &[m], false);
        assert_eq!(&px[0..3], &[255, 255, 255]);
        let edge = (5 * 40 + 10) * 4; // rounded corner pixel
        assert!(px[edge + 2] < 255 && px[edge + 2] > 0, "partial coverage at the edge: {}", px[edge + 2]);
        // a tile in the middle of a bigger page sees the same pixels
        let mut a = vec![255u8; 40 * 20 * 4];
        let mut b = vec![255u8; 20 * 20 * 4];
        bake(&mut a, [40, 20], [0, 0], [40.0, 20.0], &[m], false);
        bake(&mut b, [20, 20], [20, 0], [40.0, 20.0], &[m], false);
        for y in 0..20usize {
            for x in 0..20usize {
                assert_eq!(a[(y * 40 + 20 + x) * 4..(y * 40 + 20 + x) * 4 + 3], b[(y * 20 + x) * 4..(y * 20 + x) * 4 + 3], "tile seam at {x},{y}");
            }
        }
    }

    #[test]
    fn overlapping_lines_are_separated_and_ink_moves_and_scales_inside_the_page() {
        let mut q = vec![[0.1, 0.10, 0.5, 0.16], [0.1, 0.15, 0.6, 0.22]];
        separate_lines(&mut q);
        assert!(q[0][3] <= q[1][1] + 1e-6, "no double-multiplied band");
        let ink = PdfAnnot { id: 1, page: 0, kind: AnnotKind::Ink, rect: [0.2, 0.2, 0.4, 0.4], pts: vec![[0.2, 0.2], [0.4, 0.4]], color: 0, text: String::new(), created: 0.0, quads: vec![], quote: String::new() };
        let m = edited(&ink, None, [0.1, -0.5]);
        assert!((m.rect[0] - 0.3).abs() < 1e-6 && m.rect[1].abs() < 1e-6, "clamped at the page edge: {:?}", m.rect);
        assert!((m.pts[1][0] - 0.5).abs() < 1e-6);
        // drag the bottom-right corner right/down: scales about the top-left corner
        let s = edited(&ink, Some(2), [0.2, 0.2]);
        assert!((s.rect[2] - 0.6).abs() < 1e-6 && (s.rect[0] - 0.2).abs() < 1e-6);
        assert!((s.pts[1][0] - 0.6).abs() < 1e-6);
        // it can never flip or vanish
        let t = edited(&ink, Some(2), [-5.0, -5.0]);
        assert!(t.rect[2] > t.rect[0] && t.rect[3] > t.rect[1]);
        assert!(movable(&ink) && resizable(&ink));
    }

    #[test]
    fn annotations_survive_json() {
        let a = PdfAnnot { id: 1, page: 0, kind: AnnotKind::Ink, rect: [0.0, 0.0, 1.0, 1.0], pts: vec![[0.0, 0.0], [1.0, 1.0]], color: 2, text: "x".into(), created: 1.0, quads: vec![[0.1, 0.1, 0.5, 0.15]], quote: "q".into() };
        let mut m = std::collections::BTreeMap::new();
        m.insert(42_u64, vec![a.clone()]);
        let s = serde_json::to_string(&m).unwrap();
        let back: std::collections::BTreeMap<u64, Vec<PdfAnnot>> = serde_json::from_str(&s).unwrap();
        assert_eq!(back[&42][0], a);
    }
}

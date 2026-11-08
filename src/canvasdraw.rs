//! Canvas drawing: grid, connectors, every node renderer and the minimap. Pure painting — gestures and chrome live in `canvasui.rs`.
//!
//! Level of detail: when the board is zoomed far out, cards degrade to coloured blocks with grey text bars, so a canvas with
//! hundreds of cards stays smooth in the browser and on phones. Only nodes that intersect the viewport are drawn.

use std::sync::Arc;

use egui::epaint::RectShape;
use egui::text::LayoutJob;
use egui::{pos2, vec2, Align2, Color32, FontId, Galley, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Ui};

use crate::canvas::{self as cv, Arrow, Canvas, Edge, Grid, Kind, Line, Node, View};
use crate::icons;
use crate::lucide as l;
use crate::model::{PageKind, Workspace};
use crate::theme::{bold, body, lerp_color, Palette};
use crate::widgets::{text_fit, with_alpha};

pub fn is_dark(pal: &Palette) -> bool {
    (pal.bg.r() as u32 + pal.bg.g() as u32 + pal.bg.b() as u32) < 300
}

pub fn canvas_bg(pal: &Palette) -> Color32 {
    if is_dark(pal) { pal.bg } else { pal.sidebar }
}

/// Strong accent colour of a colour index (neutral → the text colour).
pub fn accent(pal: &Palette, c: u8) -> Option<Color32> {
    cv::accent_rgb(c).map(|[r, g, b]| {
        let col = Color32::from_rgb(r, g, b);
        if is_dark(pal) { lerp_color(col, Color32::WHITE, 0.18) } else { col }
    })
}

/// Fill, border and optional accent of a card.
pub fn card_colors(pal: &Palette, c: u8) -> (Color32, Color32, Option<Color32>) {
    let dark = is_dark(pal);
    match accent(pal, c) {
        Some(a) => (lerp_color(pal.card, a, if dark { 0.20 } else { 0.10 }), lerp_color(pal.border, a, 0.55), Some(a)),
        None => (pal.card, if dark { lerp_color(pal.border, pal.muted, 0.35) } else { lerp_color(pal.border, pal.muted, 0.28) }, None),
    }
}

/// Everything a draw call needs.
pub struct Frame<'a> {
    pub p: &'a Painter,
    pub ui: &'a Ui,
    pub pal: &'a Palette,
    pub view: View,
    pub origin: Pos2,
    pub clip: Rect,
}

impl Frame<'_> {
    pub fn z(&self) -> f32 {
        self.view.zoom
    }
    pub fn s(&self, r: Rect) -> Rect {
        self.view.rect_to_screen(self.origin, r)
    }
    pub fn pt(&self, p: Pos2) -> Pos2 {
        self.view.to_screen(self.origin, p)
    }
}

#[derive(Default)]
pub struct NodeSt {
    pub selected: bool,
    pub editing: bool,
    pub hovered: bool,
    pub found: bool,
}

/// Things draw calls report back to the interaction code.
#[derive(Default)]
pub struct DrawOut {
    /// Node id → height it needs (auto-grow of cards / image aspect ratio).
    pub grow: Vec<(u64, f32)>,
    /// Clickable check boxes: screen rect, node, line index.
    pub checks: Vec<(Rect, u64, usize)>,
    /// Small "open" arrows (page / link cards): screen rect, node.
    pub opens: Vec<(Rect, u64)>,
    pub drawn: usize,
}

// ───────────────────────────── background ─────────────────────────────

pub fn grid(f: &Frame, g: Grid) {
    if g == Grid::None {
        return;
    }
    let z = f.z();
    let mut step = 24.0 * z;
    while step < 14.0 {
        step *= 2.0;
    }
    let c = f.clip;
    let x0 = c.left() + (f.origin.x - c.left() - f.view.x * z).rem_euclid(step) - if f.origin.x - f.view.x * z < c.left() { 0.0 } else { 0.0 };
    let y0 = c.top() + (f.origin.y - c.top() - f.view.y * z).rem_euclid(step);
    let col = with_alpha(if is_dark(f.pal) { f.pal.muted } else { lerp_color(f.pal.muted, f.pal.sidebar, 0.3) }, if g == Grid::Dots { 0.55 } else { 0.22 });
    match g {
        Grid::Dots => {
            let d = (1.7 * z.sqrt()).clamp(1.3, 2.4);
            let mut y = y0;
            while y < c.bottom() {
                let mut x = x0;
                while x < c.right() {
                    f.p.rect_filled(Rect::from_center_size(pos2(x, y), vec2(d, d)), 0.0, col);
                    x += step;
                }
                y += step;
            }
        }
        _ => {
            let st = Stroke::new(1.0_f32, col);
            let mut x = x0;
            while x < c.right() {
                f.p.line_segment([pos2(x, c.top()), pos2(x, c.bottom())], st);
                x += step;
            }
            let mut y = y0;
            while y < c.bottom() {
                f.p.line_segment([pos2(c.left(), y), pos2(c.right(), y)], st);
                y += step;
            }
        }
    }
}

// ───────────────────────────── connectors ─────────────────────────────

fn arrow_head(p: &Painter, tip: Pos2, dir: egui::Vec2, size: f32, color: Color32) {
    let d = if dir.length() < 1e-3 { vec2(1.0, 0.0) } else { dir.normalized() };
    let n = vec2(-d.y, d.x);
    let a = tip - d * size + n * size * 0.55;
    let b = tip - d * size - n * size * 0.55;
    p.add(Shape::convex_polygon(vec![tip, a, b], color, Stroke::NONE));
}

pub fn edge_color(pal: &Palette, e: &Edge) -> Color32 {
    accent(pal, e.color).unwrap_or_else(|| lerp_color(pal.muted, pal.text, if is_dark(pal) { 0.1 } else { 0.0 }))
}

pub fn draw_edge(f: &Frame, c: &Canvas, e: &Edge, selected: bool, hovered: bool) {
    let Some((pts, d_end, d_start)) = c.edge_route(e) else { return };
    let z = f.z();
    let spts: Vec<Pos2> = pts.iter().map(|p| f.pt(*p)).collect();
    let mut bb = Rect::from_min_max(spts[0], spts[0]);
    for p in &spts {
        bb.extend_with(*p);
    }
    if !bb.expand(40.0).intersects(f.clip) {
        return;
    }
    let col = edge_color(f.pal, e);
    let w = ((if selected { 2.4 } else if hovered { 2.0 } else { 1.6 }) * z.sqrt()).clamp(1.2, 3.4);
    if selected {
        f.p.add(Shape::line(spts.clone(), Stroke::new(w + 5.0, with_alpha(f.pal.accent, 0.18))));
    }
    let stroke = Stroke::new(w, if selected { f.pal.accent } else { col });
    if e.dashed {
        for s in Shape::dashed_line(&spts, stroke, 7.0 * z.sqrt().max(0.6), 5.0 * z.sqrt().max(0.6)) {
            f.p.add(s);
        }
    } else {
        f.p.add(Shape::line(spts.clone(), stroke));
    }
    let hs = (10.0 * z.sqrt()).clamp(6.0, 15.0);
    let hc = stroke.color;
    if e.arrow != Arrow::None {
        arrow_head(f.p, *spts.last().unwrap(), d_end, hs, hc);
    }
    if e.arrow == Arrow::Both {
        arrow_head(f.p, spts[0], d_start, hs, hc);
    }
    if !e.label.is_empty() && z > 0.3 {
        let mid = f.pt(cv::path_mid(&pts));
        let g = f.p.layout_no_wrap(e.label.clone(), body((12.5 * z).clamp(9.0, 22.0)), f.pal.text);
        let r = Rect::from_center_size(mid, g.size() + vec2(14.0, 8.0));
        f.p.rect_filled(r, r.height() / 2.0, f.pal.card);
        f.p.rect_stroke(r, r.height() / 2.0, Stroke::new(1.0_f32, if selected { f.pal.accent } else { f.pal.border }), StrokeKind::Inside);
        f.p.galley(r.center() - g.size() / 2.0, g, f.pal.text);
    }
}

// ───────────────────────────── lite markdown ─────────────────────────────

pub enum RowKind {
    Plain,
    Heading,
    Bullet,
    Check(bool),
    Quote,
    Rule,
}

pub struct MdRow {
    pub kind: RowKind,
    pub x: f32,
    pub y: f32,
    pub h: f32,
    pub line: usize,
    pub galley: Option<Arc<Galley>>,
}

pub struct MdLayout {
    pub rows: Vec<MdRow>,
    pub height: f32,
}

/// Lays out card text. All sizes are screen pixels (`z` already applied through `scale`).
pub fn layout_md(p: &Painter, text: &str, width: f32, px: f32, pal: &Palette, bold_all: bool) -> MdLayout {
    let mut rows = vec![];
    let mut y = 0.0;
    let gap = px * 0.28;
    let width = width.max(20.0);
    for (i, raw) in text.lines().enumerate() {
        let line = cv::parse_line(raw);
        let (kind, x, font, color, body_txt): (RowKind, f32, FontId, Color32, &str) = match line {
            Line::H(lvl, t) => (RowKind::Heading, 0.0, bold(px * [1.0, 1.52, 1.24, 1.07][lvl as usize]), pal.text, t),
            Line::Bullet(ind, t) => (RowKind::Bullet, px * 1.15 + ind as f32 * px * 1.0, if bold_all { bold(px) } else { body(px) }, pal.text, t),
            Line::Check(done, t) => (RowKind::Check(done), px * 1.55, body(px), if done { pal.muted } else { pal.text }, t),
            Line::Quote(t) => (RowKind::Quote, px * 0.95, body(px), pal.muted, t),
            Line::Rule => {
                rows.push(MdRow { kind: RowKind::Rule, x: 0.0, y, h: px * 0.8, line: i, galley: None });
                y += px * 0.8 + gap;
                continue;
            }
            Line::Blank => {
                y += px * 0.55;
                continue;
            }
            Line::Plain(t) => (RowKind::Plain, 0.0, if bold_all { bold(px) } else { body(px) }, pal.text, t),
        };
        let g = p.layout(if body_txt.is_empty() { " ".to_owned() } else { body_txt.to_owned() }, font, color, (width - x).max(10.0));
        let h = g.size().y.max(px * 1.2);
        rows.push(MdRow { kind, x, y, h, line: i, galley: Some(g) });
        y += h + gap;
    }
    MdLayout { rows, height: (y - gap).max(0.0) }
}

/// Paints a laid-out card body at `top_left`; check boxes are reported into `out.checks`.
pub fn paint_md(f: &Frame, lay: &MdLayout, top_left: Pos2, px: f32, accent_col: Color32, node: u64, out: &mut DrawOut) {
    for r in &lay.rows {
        let o = top_left + vec2(0.0, r.y);
        match r.kind {
            RowKind::Rule => {
                f.p.hline(top_left.x..=top_left.x + 400.0 * f.z(), o.y + r.h / 2.0, Stroke::new(1.0_f32, f.pal.border));
            }
            RowKind::Bullet => {
                f.p.circle_filled(pos2(o.x + r.x - px * 0.62, o.y + px * 0.62), (px * 0.11).max(1.2), f.pal.muted);
            }
            RowKind::Quote => {
                f.p.rect_filled(Rect::from_min_size(o + vec2(0.0, 1.0), vec2((px * 0.16).max(2.0), (r.h - 2.0).max(2.0))), 2.0, with_alpha(accent_col, 0.55));
            }
            RowKind::Check(done) => {
                let s = px * 1.05;
                let b = Rect::from_min_size(o + vec2(px * 0.1, (px * 1.2 - s) / 2.0 + 1.0), vec2(s, s));
                if done {
                    f.p.rect_filled(b, s * 0.28, accent_col);
                    let st = Stroke::new((px * 0.12).max(1.4), Color32::WHITE);
                    f.p.line_segment([b.min + vec2(s * 0.24, s * 0.52), b.min + vec2(s * 0.43, s * 0.72)], st);
                    f.p.line_segment([b.min + vec2(s * 0.43, s * 0.72), b.min + vec2(s * 0.78, s * 0.3)], st);
                } else {
                    f.p.rect_stroke(b, s * 0.28, Stroke::new(1.4_f32, f.pal.muted), StrokeKind::Inside);
                }
                out.checks.push((b.expand(px * 0.35), node, r.line));
            }
            _ => {}
        }
        if let Some(g) = &r.galley {
            let col = match r.kind {
                RowKind::Check(true) => f.pal.muted,
                RowKind::Quote => f.pal.muted,
                _ => f.pal.text,
            };
            let size = g.size();
            f.p.galley(o + vec2(r.x, 0.0), g.clone(), col);
            if matches!(r.kind, RowKind::Check(true)) && g.rows.len() == 1 {
                let y = o.y + size.y * 0.55;
                f.p.line_segment([pos2(o.x + r.x, y), pos2(o.x + r.x + g.rows[0].rect.width(), y)], Stroke::new(1.0_f32, f.pal.muted));
            }
        }
    }
}

fn text_bars(f: &Frame, r: Rect, n: usize, color: Color32) {
    let mut y = r.top() + r.height() * 0.22;
    for i in 0..n {
        let w = r.width() * (0.8 - 0.18 * (i % 3) as f32);
        f.p.rect_filled(Rect::from_min_size(pos2(r.left() + r.width() * 0.1, y), vec2(w.max(4.0), 2.5)), 1.0, color);
        y += 6.5;
        if y > r.bottom() - 6.0 {
            break;
        }
    }
}

// ───────────────────────────── nodes ─────────────────────────────

fn fake_shadow(f: &Frame, r: Rect, rad: f32, strong: bool) {
    let z = f.z();
    if z < 0.25 {
        return;
    }
    let a = if is_dark(f.pal) { 40 } else { 14 };
    f.p.rect_filled(r.translate(vec2(0.0, 2.5 * z)).expand(1.0), rad + 1.0, Color32::from_black_alpha(a));
    if strong {
        f.p.rect_filled(r.translate(vec2(0.0, 6.0 * z)).expand(3.0 * z), rad + 3.0, Color32::from_black_alpha(a / 2));
    }
}

/// Excerpt shown on a page card.
fn page_excerpt(ws: &Workspace, id: u64) -> (String, String, String, bool) {
    let Some(p) = ws.page(id) else { return (String::new(), "Page not found".into(), String::new(), false) };
    if p.trashed {
        return (p.icon.clone(), if p.title.is_empty() { "Untitled".into() } else { p.title.clone() }, "In the trash".into(), false);
    }
    let title = if p.title.is_empty() { "Untitled".to_owned() } else { p.title.clone() };
    let sub = match p.kind {
        PageKind::Database => format!("Database · {} rows", ws.rows(id).len()),
        PageKind::Canvas => format!("Canvas · {} items", p.canvas.as_ref().map_or(0, |c| c.nodes.len())),
        _ => {
            let mut s = String::new();
            for b in p.blocks.iter().filter(|b| !b.text.trim().is_empty()) {
                if !s.is_empty() {
                    s.push_str("  ");
                }
                s.push_str(b.text.trim());
                if s.chars().count() > 150 {
                    break;
                }
            }
            s.chars().take(150).collect()
        }
    };
    (p.icon.clone(), title, sub, true)
}

fn dashed_rect(f: &Frame, r: Rect, rad: f32, color: Color32) {
    let mut pts = vec![];
    let n = 6;
    for (c, a0) in [(pos2(r.right() - rad, r.top() + rad), -90.0_f32), (pos2(r.right() - rad, r.bottom() - rad), 0.0), (pos2(r.left() + rad, r.bottom() - rad), 90.0), (pos2(r.left() + rad, r.top() + rad), 180.0)] {
        for i in 0..=n {
            let a = (a0 + 90.0 * i as f32 / n as f32).to_radians();
            pts.push(pos2(c.x + rad * a.cos(), c.y + rad * a.sin()));
        }
    }
    pts.push(pts[0]);
    for s in Shape::dashed_line(&pts, Stroke::new(1.3_f32, color), 6.0, 5.0) {
        f.p.add(s);
    }
}

pub fn draw_node(f: &Frame, ws: &Workspace, n: &Node, st: &NodeSt, out: &mut DrawOut) {
    let r = f.s(n.rect());
    if !r.expand(24.0).intersects(f.clip) {
        return;
    }
    out.drawn += 1;
    let z = f.z();
    let pal = f.pal;
    let rad = (10.0 * z).clamp(2.0, 14.0);
    let (fill, border, acc) = card_colors(pal, n.color);
    let acc_col = acc.unwrap_or(pal.accent);
    let tiny = r.width() < 54.0 || z < 0.2;
    let px = 14.5 * z;
    let lod = px < 6.5;

    match n.kind {
        Kind::Group => {
            let tint = acc.map(|a| with_alpha(a, if is_dark(pal) { 0.10 } else { 0.07 })).unwrap_or_else(|| with_alpha(pal.muted, 0.10));
            f.p.add(RectShape::filled(r, rad + 2.0, tint));
            let bc = acc.map(|a| with_alpha(a, 0.5)).unwrap_or_else(|| with_alpha(pal.muted, 0.45));
            f.p.rect_stroke(r, rad + 2.0, Stroke::new(1.2_f32, bc), StrokeKind::Inside);
            let label = if n.text.is_empty() { "Group" } else { n.text.as_str() };
            if !st.editing {
                let fs = (13.5 * z).max(if z < 0.4 { 11.0 } else { 0.0 });
                let pill_h = (26.0 * z).max(fs * 1.7);
                let g = f.p.layout_no_wrap(label.to_owned(), bold(fs), acc.unwrap_or(pal.text));
                let pw = (g.size().x + 22.0 * z.max(0.6)).min(r.width() - 12.0 * z);
                let pill = Rect::from_min_size(r.min + vec2(10.0 * z, 9.0 * z), vec2(pw.max(24.0), pill_h));
                f.p.rect_filled(pill, pill_h / 2.0, acc.map(|a| with_alpha(a, if is_dark(pal) { 0.28 } else { 0.16 })).unwrap_or_else(|| with_alpha(pal.muted, 0.2)));
                text_fit(f.p, pill.left_center() + vec2(11.0 * z.max(0.6), 0.0), Align2::LEFT_CENTER, label, bold(fs), acc.unwrap_or(pal.text), (pill.width() - 18.0).max(8.0));
            }
        }
        Kind::Ink => {
            if n.pts.len() >= 2 {
                let pts: Vec<Pos2> = n.pts.iter().map(|p| f.pt(pos2(n.x + p[0], n.y + p[1]))).collect();
                let w = ((2.2 + n.size as f32 * 1.9) * z).clamp(1.0, 14.0);
                f.p.add(Shape::line(pts, Stroke::new(w, accent(pal, n.color).unwrap_or(pal.text))));
            }
        }
        Kind::Text => {
            if st.selected || st.hovered {
                f.p.rect_stroke(r, 6.0, Stroke::new(1.0_f32, with_alpha(pal.muted, 0.5)), StrokeKind::Inside);
            }
            if !st.editing {
                let scale = [1.25, 1.9, 2.8][n.size.min(2) as usize];
                if n.text.trim().is_empty() {
                    f.p.text(r.left_top() + vec2(4.0 * z, 4.0 * z), Align2::LEFT_TOP, "Text", bold(px * scale), with_alpha(pal.muted, 0.6));
                } else if !lod {
                    let lay = layout_md(f.p, &n.text, r.width() - 8.0 * z, px * scale, pal, true);
                    paint_md(f, &lay, r.left_top() + vec2(4.0 * z, 4.0 * z), px * scale, acc_col, n.id, out);
                    out.grow.push((n.id, lay.height / z + 10.0));
                } else {
                    text_bars(f, r, 1, with_alpha(pal.text, 0.45));
                }
            }
        }
        Kind::Note | Kind::Todo => {
            fake_shadow(f, r, rad, st.selected);
            f.p.rect_filled(r, rad, fill);
            f.p.rect_stroke(r, rad, Stroke::new(1.0_f32, border), StrokeKind::Inside);
            if let (Some(a), false) = (acc, tiny) {
                // slim colour tab on the left edge
                f.p.rect_filled(Rect::from_min_size(r.min + vec2(0.0, rad), vec2((3.0 * z).max(2.0), (r.height() - rad * 2.0).max(1.0))), 2.0, a);
            }
            if !st.editing {
                if lod {
                    text_bars(f, r, 4, with_alpha(pal.text, 0.28));
                } else {
                    let padx = 14.0 * z;
                    let pady = 12.0 * z;
                    let lay = layout_md(f.p, &n.text, r.width() - padx * 2.0, px, pal, false);
                    paint_md(f, &lay, r.left_top() + vec2(padx, pady), px, acc_col, n.id, out);
                    let extra = if n.kind == Kind::Todo { 10.0 } else { 0.0 };
                    out.grow.push((n.id, lay.height / z + 24.0 + extra));
                    if n.kind == Kind::Todo {
                        let (d, t) = cv::progress(&n.text);
                        if t > 0 {
                            let bar = Rect::from_min_size(pos2(r.left() + padx, r.bottom() - 11.0 * z), vec2(r.width() - padx * 2.0, (4.0 * z).max(2.0)));
                            f.p.rect_filled(bar, 3.0, with_alpha(pal.muted, 0.25));
                            f.p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * d as f32 / t as f32, bar.height())), 3.0, if d == t { accent(pal, 4).unwrap_or(acc_col) } else { acc_col });
                        }
                    }
                }
            } else if n.kind == Kind::Todo {
                // keep the card height honest while typing (checked in canvasui through the TextEdit galley)
            }
        }
        Kind::Page => {
            fake_shadow(f, r, rad, st.selected);
            let (icon, title, sub, ok) = n.page.map(|id| page_excerpt(ws, id)).unwrap_or((String::new(), String::new(), String::new(), false));
            if n.page.is_none() {
                f.p.rect_filled(r, rad, with_alpha(pal.card, 0.6));
                dashed_rect(f, r, rad, pal.muted);
                if !lod {
                    icons::glyph(f.p, r.center() + vec2(0.0, -10.0 * z), 20.0 * z, l::FILE_PLUS, pal.muted);
                    f.p.text(r.center() + vec2(0.0, 14.0 * z), Align2::CENTER_CENTER, "Choose a page", body(13.0 * z), pal.muted);
                }
            } else {
                f.p.rect_filled(r, rad, fill);
                f.p.rect_stroke(r, rad, Stroke::new(1.0_f32, border), StrokeKind::Inside);
                if lod || tiny {
                    text_bars(f, r, 2, with_alpha(pal.text, 0.28));
                } else {
                    let pad = 14.0 * z;
                    let ic = pos2(r.left() + pad + 11.0 * z, r.top() + pad + 11.0 * z);
                    if icon.is_empty() {
                        icons::glyph(f.p, ic, 20.0 * z, if ok { l::FILE_TEXT } else { l::CIRCLE_ALERT }, if ok { pal.muted } else { pal.danger });
                    } else {
                        icons::paint_page_icon(f.ui, ic, &icon, 22.0 * z, pal.text);
                    }
                    let tx = r.left() + pad + 30.0 * z;
                    let tw = (r.right() - tx - pad - 20.0 * z).max(20.0);
                    text_fit(f.p, pos2(tx, ic.y), Align2::LEFT_CENTER, &title, bold(15.0 * z), if ok { pal.text } else { pal.muted }, tw);
                    if !sub.is_empty() {
                        let mut job = LayoutJob::simple(sub, body(12.5 * z), pal.muted, r.width() - pad * 2.0);
                        job.wrap.max_rows = 2;
                        job.wrap.break_anywhere = false;
                        job.wrap.overflow_character = Some('…');
                        let g = f.p.layout_job(job);
                        f.p.galley(pos2(r.left() + pad, r.top() + pad + 28.0 * z), g, pal.muted);
                    }
                    if ok {
                        let b = Rect::from_center_size(pos2(r.right() - pad - 4.0 * z, ic.y), vec2(26.0 * z.max(0.8), 26.0 * z.max(0.8)));
                        icons::glyph(f.p, b.center(), 15.0 * z, l::ARROW_UP_RIGHT, if st.hovered || st.selected { pal.accent } else { with_alpha(pal.muted, 0.7) });
                        out.opens.push((b.expand(8.0), n.id));
                    }
                }
            }
        }
        Kind::Link => {
            fake_shadow(f, r, rad, st.selected);
            if n.url.is_empty() {
                f.p.rect_filled(r, rad, with_alpha(pal.card, 0.6));
                dashed_rect(f, r, rad, pal.muted);
                if !lod {
                    icons::glyph(f.p, r.center() + vec2(0.0, -10.0 * z), 20.0 * z, l::LINK, pal.muted);
                    f.p.text(r.center() + vec2(0.0, 14.0 * z), Align2::CENTER_CENTER, "Add a link", body(13.0 * z), pal.muted);
                }
            } else {
                f.p.rect_filled(r, rad, fill);
                f.p.rect_stroke(r, rad, Stroke::new(1.0_f32, border), StrokeKind::Inside);
                if lod || tiny {
                    text_bars(f, r, 2, with_alpha(pal.text, 0.28));
                } else {
                    let pad = 14.0 * z;
                    let tile = Rect::from_min_size(r.left_top() + vec2(pad, (r.height() - 40.0 * z) / 2.0), vec2(40.0 * z, 40.0 * z));
                    f.p.rect_filled(tile, 9.0 * z, acc.map(|a| with_alpha(a, 0.16)).unwrap_or(pal.accent_soft));
                    icons::glyph(f.p, tile.center(), 19.0 * z, l::GLOBE, acc_col);
                    let host = crate::media::host_of(&n.url);
                    let title = if n.text.trim().is_empty() { host.clone() } else { n.text.trim().to_owned() };
                    let tx = tile.right() + 12.0 * z;
                    let tw = (r.right() - tx - pad - 18.0 * z).max(20.0);
                    text_fit(f.p, pos2(tx, r.center().y - 9.0 * z), Align2::LEFT_CENTER, &title, bold(14.5 * z), pal.text, tw);
                    text_fit(f.p, pos2(tx, r.center().y + 10.0 * z), Align2::LEFT_CENTER, n.url.trim_start_matches("https://").trim_start_matches("http://"), body(12.0 * z), pal.muted, tw);
                    let b = Rect::from_center_size(pos2(r.right() - pad, r.top() + pad + 2.0 * z), vec2(24.0 * z.max(0.8), 24.0 * z.max(0.8)));
                    icons::glyph(f.p, b.center(), 14.0 * z, l::EXTERNAL_LINK, if st.hovered || st.selected { pal.accent } else { with_alpha(pal.muted, 0.7) });
                    out.opens.push((b.expand(8.0), n.id));
                }
            }
        }
        Kind::Image => {
            let uri = crate::media::src_uri(&n.url);
            if n.url.is_empty() {
                let rr = if r.height() < 8.0 { Rect::from_min_size(r.min, vec2(r.width(), r.width() * 0.6)) } else { r };
                f.p.rect_filled(rr, rad, with_alpha(pal.card, 0.6));
                dashed_rect(f, rr, rad, pal.muted);
                if !lod {
                    icons::glyph(f.p, rr.center() + vec2(0.0, -10.0 * z), 22.0 * z, l::IMAGE, pal.muted);
                    f.p.text(rr.center() + vec2(0.0, 14.0 * z), Align2::CENTER_CENTER, "Add an image", body(13.0 * z), pal.muted);
                }
                if n.h <= 0.5 {
                    out.grow.push((n.id, n.w * 0.6));
                }
            } else {
                let (state, size) = icons::image_size(f.ui, &uri, vec2(r.width().max(64.0), r.width().max(64.0)));
                if let (icons::ImgState::Ready, Some(s)) = (state, size) {
                    if n.h <= 0.5 && s.x > 0.0 {
                        out.grow.push((n.id, (n.w * s.y / s.x).clamp(30.0, 4000.0)));
                    }
                }
                let rr = if r.height() < 8.0 { Rect::from_min_size(r.min, vec2(r.width(), r.width() * 0.6)) } else { r };
                fake_shadow(f, rr, rad, st.selected);
                f.p.rect_filled(rr, rad, pal.hover);
                match icons::paint_image(f.ui, rr, &uri, rad) {
                    icons::ImgState::Loading => {
                        f.ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
                        icons::glyph(f.p, rr.center(), 22.0 * z, l::IMAGE, with_alpha(pal.muted, 0.6));
                    }
                    icons::ImgState::Failed => {
                        icons::glyph(f.p, rr.center() + vec2(0.0, -8.0 * z), 22.0 * z, l::IMAGE, pal.muted);
                        if !lod {
                            f.p.text(rr.center() + vec2(0.0, 16.0 * z), Align2::CENTER_CENTER, "Couldn't load this image", body(12.0 * z), pal.muted);
                        }
                    }
                    _ => {}
                }
                f.p.rect_stroke(rr, rad, Stroke::new(1.0_f32, with_alpha(pal.border, 0.9)), StrokeKind::Inside);
            }
        }
    }
    if n.locked && !tiny && z > 0.45 {
        icons::glyph(f.p, pos2(r.right() - 11.0, r.bottom() - 11.0), 12.0, l::LOCK, with_alpha(pal.muted, 0.8));
    }
    if st.found {
        f.p.rect_stroke(r.expand(3.0), rad + 3.0, Stroke::new(2.5_f32, accent(pal, 3).unwrap_or(pal.accent)), StrokeKind::Outside);
    }
}

// ───────────────────────────── minimap ─────────────────────────────

/// Draws the minimap into `rect`. Returns the world → minimap mapping `(scale, world_origin)`.
pub fn minimap(f: &Frame, c: &Canvas, rect: Rect, view_world: Rect) -> (f32, Pos2) {
    f.p.rect_filled(rect.expand(1.0), 10.0, Color32::from_black_alpha(if is_dark(f.pal) { 60 } else { 18 }));
    f.p.rect_filled(rect, 9.0, with_alpha(f.pal.card, 0.96));
    f.p.rect_stroke(rect, 9.0, Stroke::new(1.0_f32, f.pal.border), StrokeKind::Inside);
    let mut world = view_world;
    if let Some(b) = c.bounds(None) {
        world = world.union(b);
    }
    let world = world.expand(60.0);
    let inner = rect.shrink(6.0);
    let s = (inner.width() / world.width()).min(inner.height() / world.height()).max(1e-4);
    let off = inner.center() - world.center().to_vec2() * s;
    let map = |p: Pos2| pos2(off.x + p.x * s, off.y + p.y * s);
    let f2 = f.p.with_clip_rect(rect.shrink(1.0));
    for n in &c.nodes {
        let r = Rect::from_min_max(map(n.rect().min), map(n.rect().max));
        let col = match n.kind {
            Kind::Group => with_alpha(accent(f.pal, n.color).unwrap_or(f.pal.muted), 0.22),
            Kind::Ink => with_alpha(f.pal.text, 0.5),
            _ => accent(f.pal, n.color).map(|a| with_alpha(a, 0.8)).unwrap_or_else(|| with_alpha(f.pal.muted, 0.55)),
        };
        f2.rect_filled(Rect::from_min_size(r.min, vec2(r.width().max(1.5), r.height().max(1.5))), 1.5, col);
    }
    let vr = Rect::from_min_max(map(view_world.min), map(view_world.max));
    f2.rect_filled(vr, 3.0, with_alpha(f.pal.accent, 0.10));
    f2.rect_stroke(vr, 3.0, Stroke::new(1.4_f32, f.pal.accent), StrokeKind::Inside);
    (s, pos2(off.x, off.y))
}

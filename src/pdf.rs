//! Inline PDF reader (File blocks that hold a PDF).
//!
//! Rendering is done by [`hayro`], a pure-Rust PDF interpreter/rasteriser — no C library, so the very same code runs in the
//! Windows build and in the WebAssembly build. (We evaluated MuPDF — AGPL and a C build that is awkward on wasm — and
//! pdfium-render — needs a platform-specific binary. hayro needs neither.)
//! Pages are rasterised on demand at the on-screen resolution and cached as textures.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use egui::load::BytesPoll;
use egui::{pos2, vec2, Align2, ColorImage, Context, Id, Rect, Sense, Stroke, TextureHandle, TextureOptions, Ui};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::{RenderCache, RenderSettings};

use crate::theme::{body, lerp_color, Palette};
use crate::widgets::with_alpha;

pub fn looks_like_pdf(name: &str, url: &str) -> bool {
    let n = name.to_lowercase();
    let u = url.split(['?', '#']).next().unwrap_or("").to_lowercase();
    n.ends_with(".pdf") || u.ends_with(".pdf")
}

struct Doc {
    data: Arc<Vec<u8>>,
    /// Page sizes in PDF points.
    dims: Vec<(f32, f32)>,
}

enum Loaded {
    Ok(Doc),
    Err(String),
}

thread_local! {
    static DOCS: RefCell<HashMap<u64, Loaded>> = RefCell::new(HashMap::new());
    static TEX: RefCell<HashMap<(u64, usize, u32), TextureHandle>> = RefCell::new(HashMap::new());
}

fn open(data: &[u8]) -> Loaded {
    let arc = Arc::new(data.to_vec());
    match Pdf::new(arc.clone()) {
        Ok(pdf) => {
            let dims: Vec<(f32, f32)> = pdf.pages().iter().map(|p| p.render_dimensions()).collect();
            if dims.is_empty() {
                Loaded::Err("This PDF has no pages.".into())
            } else {
                Loaded::Ok(Doc { data: arc, dims })
            }
        }
        Err(e) => Loaded::Err(format!("Cannot read this PDF ({e:?}). Encrypted PDFs are not supported yet.")),
    }
}

/// Rasterises one page at `px_w` pixels wide.
pub fn render_page(data: &Arc<Vec<u8>>, page: usize, px_w: u32) -> Option<ColorImage> {
    let pdf = Pdf::new(data.clone()).ok()?;
    let pages = pdf.pages();
    let p = pages.get(page)?;
    let (w, _) = p.render_dimensions();
    let scale = (px_w as f32 / w.max(1.0)).clamp(0.2, 6.0);
    let pix = hayro::render(
        p,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &hayro::PixmapSettings { x_scale: scale, y_scale: scale, ..Default::default() },
    );
    let (iw, ih) = (pix.width() as usize, pix.height() as usize);
    let mut rgba = Vec::with_capacity(iw * ih * 4);
    for px in pix.data() {
        rgba.extend_from_slice(&[px.r, px.g, px.b, px.a]);
    }
    Some(ColorImage::from_rgba_premultiplied([iw, ih], &rgba))
}

fn key_of(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Draws the reader for the PDF at `uri` (a `media:` or http(s) source). Returns the height used.
pub fn viewer(ui: &mut Ui, ctx: &Context, bid: u64, uri: &str, title: &str, pal: &Palette, w: f32) {
    let key = key_of(uri);
    let state_id = Id::new(("pdf_state", bid));
    let (mut page, mut zoom): (usize, f32) = ctx.data(|d| d.get_temp(state_id)).unwrap_or((0, 1.0));

    // 1. bytes
    let bytes = match ctx.try_load_bytes(uri) {
        Ok(BytesPoll::Ready { bytes, .. }) => bytes,
        Ok(BytesPoll::Pending { .. }) => {
            status(ui, pal, w, "Loading PDF…");
            ctx.request_repaint();
            return;
        }
        Err(e) => {
            status(ui, pal, w, &format!("Could not load the PDF: {e}"));
            return;
        }
    };
    // 2. parse once
    let ready = DOCS.with(|d| {
        let mut d = d.borrow_mut();
        let ent = d.entry(key).or_insert_with(|| open(&bytes));
        match ent {
            Loaded::Ok(doc) => Ok((doc.data.clone(), doc.dims.clone())),
            Loaded::Err(e) => Err(e.clone()),
        }
    });
    let (data, dims) = match ready {
        Ok(v) => v,
        Err(e) => {
            status(ui, pal, w, &e);
            return;
        }
    };
    let n = dims.len();
    page = page.min(n - 1);

    // 3. toolbar
    let bar = ui.allocate_exact_size(vec2(w, 34.0), Sense::hover()).0;
    ui.painter().rect_filled(bar, 8.0, with_alpha(pal.hover, 0.7));
    let mut x = bar.left() + 10.0;
    let mut btn = |ui: &mut Ui, label: &str, enabled: bool, x: &mut f32| -> bool {
        let g = ui.painter().layout_no_wrap(label.to_owned(), body(13.0), pal.text);
        let r = Rect::from_min_size(pos2(*x, bar.top() + 5.0), vec2(g.size().x + 16.0, 24.0));
        *x = r.right() + 4.0;
        let resp = ui.interact(r, Id::new(("pdf_btn", bid, label)), if enabled { Sense::click() } else { Sense::hover() });
        let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() && enabled, 0.1);
        ui.painter().rect_filled(r, 6.0, with_alpha(lerp_color(pal.bg, pal.active, h), if enabled { 0.9 } else { 0.3 }));
        ui.painter().text(r.center(), Align2::CENTER_CENTER, label, body(13.0), with_alpha(pal.text, if enabled { 1.0 } else { 0.35 }));
        enabled && resp.clicked()
    };
    if btn(ui, "‹ Prev", page > 0, &mut x) {
        page -= 1;
    }
    let lbl = format!("{} / {}", page + 1, n);
    let g = ui.painter().layout_no_wrap(lbl, body(13.0), pal.text);
    ui.painter().galley(pos2(x + 6.0, bar.center().y - g.size().y / 2.0), g.clone(), pal.text);
    x += g.size().x + 16.0;
    if btn(ui, "Next ›", page + 1 < n, &mut x) {
        page += 1;
    }
    x += 10.0;
    if btn(ui, "–", zoom > 0.45, &mut x) {
        zoom = (zoom - 0.15).max(0.4);
    }
    let zl = format!("{:.0}%", zoom * 100.0);
    let zg = ui.painter().layout_no_wrap(zl, body(13.0), pal.muted);
    ui.painter().galley(pos2(x + 4.0, bar.center().y - zg.size().y / 2.0), zg.clone(), pal.muted);
    x += zg.size().x + 12.0;
    if btn(ui, "+", zoom < 1.0, &mut x) {
        zoom = (zoom + 0.15).min(1.0);
    }
    let tw = (bar.right() - x - 14.0).max(40.0);
    crate::widgets::text_fit(ui.painter(), pos2(bar.right() - 10.0, bar.center().y), Align2::RIGHT_CENTER, title, body(12.5), pal.muted, tw.min(260.0));

    // 4. page (rasterised at screen resolution, cached)
    let ppp = ctx.pixels_per_point();
    let shown_w = (w - 2.0) * zoom;
    let (pw, ph) = dims[page];
    let shown_h = shown_w * ph / pw.max(1.0);
    let px_w = (((shown_w * ppp) / 64.0).ceil() * 64.0).clamp(256.0, 2400.0) as u32;
    let tex = TEX.with(|t| {
        let mut t = t.borrow_mut();
        if let Some(h) = t.get(&(key, page, px_w)) {
            return Some(h.clone());
        }
        if t.len() > 10 {
            t.clear();
        }
        let img = render_page(&data, page, px_w)?;
        let h = ctx.load_texture(format!("pdf-{key}-{page}-{px_w}"), img, TextureOptions::LINEAR);
        t.insert((key, page, px_w), h.clone());
        Some(h)
    });
    let (area, _) = ui.allocate_exact_size(vec2(w, shown_h + 12.0), Sense::hover());
    let r = Rect::from_min_size(pos2(area.center().x - shown_w / 2.0, area.top() + 6.0), vec2(shown_w, shown_h));
    ui.painter().rect_filled(r.expand(1.0), 2.0, with_alpha(pal.border, 0.9));
    ui.painter().rect_filled(r, 0.0, egui::Color32::WHITE);
    match tex {
        Some(t) => {
            ui.painter().image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
        None => {
            ui.painter().text(r.center(), Align2::CENTER_CENTER, "This page could not be rendered", body(14.0), egui::Color32::GRAY);
        }
    }
    ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0_f32, with_alpha(pal.border, 0.6)), egui::StrokeKind::Outside);
    ctx.data_mut(|d| d.insert_temp(state_id, (page, zoom)));
}

fn status(ui: &mut Ui, pal: &Palette, w: f32, msg: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(w, 54.0), Sense::hover());
    ui.painter().rect_filled(r, 8.0, with_alpha(pal.hover, 0.5));
    crate::widgets::text_fit(ui.painter(), r.center(), Align2::CENTER_CENTER, msg, body(13.5), pal.muted, w - 24.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smallest valid one-page PDF with some text.
    fn tiny_pdf() -> Vec<u8> {
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
            { let c = "BT /F1 24 Tf 20 40 Td (Hello PDF) Tj ET"; format!("<< /Length {} >>\nstream\n{}\nendstream", c.len(), c) },
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offs = vec![];
        for (i, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out.extend(format!("{} 0 obj\n{}\nendobj\n", i + 1, o).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
        for o in offs {
            out.extend(format!("{:010} 00000 n \n", o).bytes());
        }
        out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF", objs.len() + 1, xref).bytes());
        out
    }

    #[test]
    fn renders_a_page() {
        let Loaded::Ok(doc) = open(&tiny_pdf()) else { panic!("pdf did not open") };
        assert_eq!(doc.dims.len(), 1);
        let img = render_page(&doc.data, 0, 400).expect("render");
        assert_eq!(img.size[0], 400);
        assert!(img.pixels.iter().any(|p| p.r() < 100), "text pixels expected");
        assert!(looks_like_pdf("a.PDF", "") && looks_like_pdf("x", "https://a/b.pdf?dl=1") && !looks_like_pdf("a.png", ""));
    }
}

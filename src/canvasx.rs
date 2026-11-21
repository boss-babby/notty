//! Rich canvas nodes: **embeds** (YouTube / Vimeo / any website), **PDF cards** (one-shot page picture), **files**, **database
//! windows** and **widgets** (counter, timer, pomodoro, progress, poll, clock, calendar, sheet) that are edited right on the board.
//!
//! Design rules
//! * Painting only (the gestures live in `canvasui.rs`): a draw call reports clickable spots through [`DrawOut::acts`]; the click
//!   handler hands them to [`act`], which mutates the node and returns an [`Fx`] for things only the app can do.
//! * Cheap by construction. A PDF card never keeps a parser or a tile cache: the chosen page (cover = first page, or the last page
//!   you turned to) is rasterised **once** into one small texture ([`crate::pdf::render_page_image`]), at most one per frame, and
//!   the textures live in a tiny LRU that is dropped when the board is left. Live web frames exist only for the (max. 3) embeds you
//!   switched on; every other embed is a poster picture.
//! * Widget state is plain JSON in [`Node::data`], so it is saved, copied, exported and undone like any other node property.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::{pos2, vec2, Align2, Color32, Context, Rect, Stroke, StrokeKind, TextureHandle, TextureOptions, Ui};
use serde_json::{json, Value};

use crate::canvas::{Canvas, Kind, Node};
use crate::canvasdraw::{card_colors, dashed_rect, fake_shadow, DrawOut, Frame, NodeSt};
use crate::lucide as l;
use crate::media::{self, MediaStore};
use crate::model::{PageKind, PropKind, Workspace};
use crate::platform::{self, EmbedRect};
use crate::theme::{body, bold, Palette};
use crate::widgets::{text_fit, with_alpha};
use crate::{dates, icons};

// ───────────────────────────── actions ─────────────────────────────

pub const A_OPEN: u16 = 1;
pub const A_LIVE: u16 = 2;
pub const A_PAGE: u16 = 3;
pub const A_EDIT: u16 = 5;
pub const A_INC: u16 = 10;
pub const A_RUN: u16 = 11;
pub const A_RESET: u16 = 12;
pub const A_VOTE: u16 = 13;
pub const A_PROG: u16 = 14;
pub const A_BAR: u16 = 15;
pub const A_MONTH: u16 = 16;
pub const A_DAY: u16 = 17;
pub const A_SKIP: u16 = 18;

/// What the app has to do after a click on a node (things the canvas code cannot do alone).
pub enum Fx {
    None,
    Page(u64),
    Url(String),
    Pdf { media: u64, url: String, title: String },
    File { media: u64, name: String },
    Edit(u64),
}

/// Widget catalogue: key, glyph, label.
pub const WIDGETS: [(&str, &str, &str); 8] = [
    ("counter", l::HASH, "Counter"),
    ("timer", l::TIMER, "Timer / stopwatch"),
    ("pomodoro", l::HOURGLASS, "Pomodoro"),
    ("progress", l::GAUGE, "Progress bar"),
    ("poll", l::CHART_BAR, "Quick poll"),
    ("clock", l::CLOCK, "Clock"),
    ("calendar", l::CALENDAR_DAYS, "Calendar / habit tracker"),
    ("sheet", l::TABLE, "Mini sheet"),
];

/// Initial state and size of a widget.
pub fn widget_defaults(key: &str) -> (Value, egui::Vec2) {
    match key {
        "timer" => (json!({"k": "timer", "t": "Timer", "mode": "down", "mins": 25, "acc": 0, "run": 0}), vec2(220.0, 150.0)),
        "pomodoro" => (json!({"k": "pomodoro", "t": "Pomodoro", "focus": 25, "brk": 5, "acc": 0, "run": 0}), vec2(240.0, 170.0)),
        "progress" => (json!({"k": "progress", "t": "Progress", "v": 40}), vec2(260.0, 108.0)),
        "poll" => (json!({"k": "poll", "t": "Quick poll", "o": ["Yes", "No", "Maybe"], "v": [0, 0, 0]}), vec2(260.0, 196.0)),
        "clock" => (json!({"k": "clock", "t": "", "tz": dates::tz_offset_min(), "h24": true, "secs": true}), vec2(220.0, 120.0)),
        "calendar" => (json!({"k": "calendar", "t": "Calendar", "marks": []}), vec2(300.0, 290.0)),
        "sheet" => (json!({"k": "sheet", "t": "Sheet", "c": [["Item", "Qty", "Price"], ["", "", ""], ["", "", ""]]}), vec2(360.0, 190.0)),
        _ => (json!({"k": "counter", "t": "Counter", "v": 0, "step": 1, "goal": 0}), vec2(220.0, 140.0)),
    }
}

/// Turns `n` into a widget of kind `key` (keeps position, resets state).
pub fn make_widget(n: &mut Node, key: &str, resize: bool) {
    let (v, size) = widget_defaults(key);
    n.kind = Kind::Widget;
    n.data = v.to_string();
    if resize {
        n.w = size.x;
        n.h = size.y;
    }
}

fn jd(n: &Node) -> Value {
    serde_json::from_str(&n.data).unwrap_or_else(|_| json!({}))
}
fn jf(v: &Value, k: &str, d: f64) -> f64 {
    v.get(k).and_then(|x| x.as_f64()).unwrap_or(d)
}
fn js(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_owned()
}
fn jb(v: &Value, k: &str, d: bool) -> bool {
    v.get(k).and_then(|x| x.as_bool()).unwrap_or(d)
}
fn jstrs(v: &Value, k: &str) -> Vec<String> {
    v.get(k).and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}
fn jnums(v: &Value, k: &str) -> Vec<f64> {
    v.get(k).and_then(|x| x.as_array()).map(|a| a.iter().map(|s| s.as_f64().unwrap_or(0.0)).collect()).unwrap_or_default()
}
fn jgrid(v: &Value) -> Vec<Vec<String>> {
    v.get("c")
        .and_then(|x| x.as_array())
        .map(|rows| rows.iter().map(|row| row.as_array().map(|c| c.iter().map(|s| s.as_str().unwrap_or("").to_owned()).collect()).unwrap_or_default()).collect())
        .unwrap_or_default()
}
fn wkind(n: &Node) -> String {
    let k = js(&jd(n), "k");
    if k.is_empty() {
        "counter".into()
    } else {
        k
    }
}

pub fn kind_glyph(n: &Node) -> &'static str {
    match n.kind {
        Kind::Embed => {
            let i = media::embed_info(&n.url);
            if i.service == "YouTube" {
                l::YOUTUBE
            } else if i.kind == media::EmbedKind::Video {
                l::VIDEO
            } else {
                l::GLOBE
            }
        }
        Kind::Pdf => l::FILE_TEXT,
        Kind::File => file_glyph(&n.text, &n.url),
        Kind::DbView => l::DATABASE,
        Kind::Widget => {
            let k = wkind(n);
            WIDGETS.iter().find(|w| w.0 == k).map_or(l::HASH, |w| w.1)
        }
        _ => l::FILE,
    }
}

fn file_glyph(name: &str, url: &str) -> &'static str {
    let s = if name.is_empty() { url } else { name }.to_lowercase();
    let ext = s.split(['?', '#']).next().unwrap_or("").rsplit('.').next().unwrap_or("").to_owned();
    match ext.as_str() {
        "pdf" => l::FILE_TEXT,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" => l::IMAGE,
        "mp3" | "wav" | "ogg" | "m4a" | "flac" => l::FILE_AUDIO,
        "mp4" | "mov" | "webm" | "mkv" => l::FILM,
        "zip" | "rar" | "7z" | "tar" | "gz" => l::ARCHIVE,
        "csv" | "xls" | "xlsx" | "ods" => l::FILE_SPREADSHEET,
        "json" => l::FILE_JSON,
        "rs" | "js" | "ts" | "py" | "html" | "css" | "java" | "c" | "cpp" | "go" => l::FILE_CODE,
        _ => l::FILE,
    }
}

fn file_ext(name: &str, url: &str) -> String {
    let s = if name.is_empty() { url } else { name };
    let e = s.split(['?', '#']).next().unwrap_or("").rsplit_once('.').map(|x| x.1.to_uppercase()).unwrap_or_default();
    if e.len() > 5 {
        String::new()
    } else {
        e
    }
}

// ───────────────────────────── caches (covers, live frames) ─────────────────────────────

struct Cover {
    tex: TextureHandle,
    pages: usize,
    used: u64,
}

#[derive(Default)]
struct Cache {
    covers: HashMap<(String, usize), Cover>,
    failed: HashSet<(String, usize)>,
    want: Vec<(String, usize)>,
    live: Vec<u64>,
    rects: (u64, Vec<EmbedRect>),
    last_draw: f64,
}

thread_local! {
    static C: RefCell<Cache> = RefCell::new(Cache::default());
}

#[cfg(any(target_arch = "wasm32", target_os = "android"))]
const MAX_COVERS: usize = 6;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
const MAX_COVERS: usize = 14;
const MAX_LIVE: usize = 3;

enum CoverState {
    Ready(egui::TextureId, egui::Vec2, usize),
    Pending,
    Failed,
}

fn cover(ctx: &Context, src: &str, page: usize) -> CoverState {
    let key = (src.to_owned(), page);
    let now = ctx.cumulative_pass_nr();
    let time = ctx.input(|i| i.time);
    C.with(|c| {
        let mut c = c.borrow_mut();
        c.last_draw = time;
        if let Some(cv) = c.covers.get_mut(&key) {
            cv.used = now;
            return CoverState::Ready(cv.tex.id(), cv.tex.size_vec2(), cv.pages);
        }
        if c.failed.contains(&key) {
            return CoverState::Failed;
        }
        if !c.want.contains(&key) {
            c.want.push(key);
        }
        CoverState::Pending
    })
}

/// Number of pages of a PDF card once its first picture exists.
fn known_pages(src: &str) -> Option<usize> {
    C.with(|c| c.borrow().covers.iter().find(|(k, _)| k.0 == src).map(|(_, v)| v.pages))
}

/// Renders at most one pending PDF picture. Called by the app once per frame. Frees every picture soon after the board is left.
pub fn service(ctx: &Context, media: &MediaStore) {
    let time = ctx.input(|i| i.time);
    let job = C.with(|c| {
        let mut c = c.borrow_mut();
        if !c.covers.is_empty() && time - c.last_draw > 3.0 {
            c.covers.clear();
        }
        if time - c.last_draw > 1.0 {
            c.want.clear();
            return None;
        }
        c.want.first().cloned()
    });
    let Some((src, page)) = job else { return };
    let remote = media::media_id(&src).is_none();
    let bytes: Option<Arc<[u8]>> = match media::media_id(&src) {
        Some(id) => media.bytes(id),
        None => {
            let url = media::normalize_url(&src);
            match ctx.try_load_bytes(&url) {
                Ok(egui::load::BytesPoll::Ready { bytes, .. }) => {
                    let b: Arc<[u8]> = Arc::from(bytes.as_ref());
                    Some(b)
                }
                Ok(egui::load::BytesPoll::Pending { .. }) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(200));
                    return;
                }
                Err(_) => None,
            }
        }
    };
    if remote {
        ctx.forget_image(&media::normalize_url(&src));
    }
    // a document that a collaborator is still sending: keep waiting instead of giving up on it
    if bytes.is_none() && !remote {
        C.with(|c| c.borrow_mut().want.retain(|k| !(k.0 == src && k.1 == page)));
        return;
    }
    let rendered = bytes.and_then(|b| crate::pdf::render_page_image(b, page, 900));
    let pass = ctx.cumulative_pass_nr();
    C.with(|c| {
        let mut c = c.borrow_mut();
        let key = (src.clone(), page);
        c.want.retain(|k| *k != key);
        match rendered {
            Some((img, pages)) => {
                let tex = ctx.load_texture(format!("pdfcover-{src}-{page}"), img, TextureOptions::LINEAR);
                c.covers.insert(key, Cover { tex, pages, used: pass });
                while c.covers.len() > MAX_COVERS {
                    let oldest = c.covers.iter().min_by_key(|(_, v)| v.used).map(|(k, _)| k.clone());
                    match oldest {
                        Some(k) => c.covers.remove(&k),
                        None => break,
                    };
                }
            }
            None => {
                c.failed.insert(key);
            }
        }
    });
    ctx.request_repaint();
}

/// Live frames of this pass (web iframes / Windows web views). Empty unless the board was painted in the current pass.
pub fn live_rects(ctx: &Context) -> Vec<EmbedRect> {
    let now = ctx.cumulative_pass_nr();
    C.with(|c| {
        let c = c.borrow();
        if c.rects.0 == now {
            c.rects.1.clone()
        } else {
            vec![]
        }
    })
}

fn begin_frame(ctx: &Context) {
    let now = ctx.cumulative_pass_nr();
    C.with(|c| {
        let mut c = c.borrow_mut();
        if c.rects.0 != now {
            c.rects = (now, vec![]);
        }
    });
}

fn is_live(id: u64) -> bool {
    C.with(|c| c.borrow().live.contains(&id))
}

fn toggle_live(id: u64) {
    C.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(i) = c.live.iter().position(|x| *x == id) {
            c.live.remove(i);
        } else {
            c.live.push(id);
            while c.live.len() > MAX_LIVE {
                c.live.remove(0);
            }
        }
    });
}

// ───────────────────────────── drawing ─────────────────────────────

fn hovered(f: &Frame, r: Rect) -> bool {
    f.ui.input(|i| i.pointer.hover_pos()).map_or(false, |p| r.contains(p))
}

#[allow(clippy::too_many_arguments)]
fn btn(f: &Frame, out: &mut DrawOut, id: u64, c: egui::Pos2, d: f32, g: &str, code: u16, arg: i32, tint: Color32, filled: bool) {
    let r = Rect::from_center_size(c, vec2(d, d));
    let hv = hovered(f, r.expand(3.0));
    if filled {
        f.p.circle_filled(c, d / 2.0, with_alpha(tint, if hv { 0.30 } else { 0.17 }));
    } else if hv {
        f.p.circle_filled(c, d / 2.0, with_alpha(f.pal.muted, 0.18));
    }
    icons::glyph(f.p, c, d * 0.52, g, tint);
    out.acts.push((r.expand(if crate::touch::touchy() { 10.0 } else { 4.0 }), id, code, arg));
}

fn fit(inner: Rect, size: egui::Vec2) -> Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return inner;
    }
    let s = (inner.width() / size.x).min(inner.height() / size.y);
    Rect::from_center_size(inner.center(), size * s)
}

#[allow(clippy::too_many_arguments)]
pub fn draw(f: &Frame, ws: &Workspace, n: &Node, st: &NodeSt, r: Rect, rad: f32, lod: bool, tiny: bool, out: &mut DrawOut) {
    let pal = f.pal;
    let z = f.z();
    begin_frame(f.ui.ctx());
    let (fill, border, acc) = card_colors(pal, n.color);
    let acc_col = acc.unwrap_or(pal.accent);
    let empty = match n.kind {
        Kind::Embed | Kind::Pdf | Kind::File => n.url.is_empty(),
        Kind::DbView => n.page.is_none(),
        _ => false,
    };
    fake_shadow(f, r, rad, st.selected);
    if empty {
        f.p.rect_filled(r, rad, with_alpha(pal.card, 0.6));
        dashed_rect(f, r, rad, pal.muted);
        if !lod {
            let (what, g) = match n.kind {
                Kind::Embed => ("Embed a video or website", l::GLOBE),
                Kind::Pdf => ("Add a PDF", l::FILE_TEXT),
                Kind::File => ("Attach a file", l::PAPERCLIP),
                _ => ("Pick a database", l::DATABASE),
            };
            icons::glyph(f.p, r.center() + vec2(0.0, -10.0 * z), 20.0 * z, g, pal.muted);
            f.p.text(r.center() + vec2(0.0, 14.0 * z), Align2::CENTER_CENTER, what, body(13.0 * z), pal.muted);
        }
        return;
    }
    f.p.rect_filled(r, rad, fill);
    f.p.rect_stroke(r, rad, Stroke::new(1.0_f32, border), StrokeKind::Inside);
    if lod || tiny {
        icons::glyph(f.p, r.center(), (r.height().min(r.width()) * 0.3).clamp(8.0, 40.0), kind_glyph(n), with_alpha(acc_col, 0.9));
        return;
    }
    match n.kind {
        Kind::Embed => draw_embed(f, n, r, out, acc_col),
        Kind::Pdf => draw_pdf(f, ws, n, r, rad, out, acc_col),
        Kind::File => draw_file(f, n, st, r, out, acc_col),
        Kind::DbView => draw_db(f, ws, n, st, r, out, acc_col),
        _ => draw_widget(f, n, st, r, out, acc_col),
    }
}

fn header(f: &Frame, r: Rect, h: f32) -> Rect {
    let head = Rect::from_min_size(r.min, vec2(r.width(), h));
    f.p.line_segment([pos2(head.left() + 1.0, head.bottom()), pos2(head.right() - 1.0, head.bottom())], Stroke::new(1.0_f32, with_alpha(f.pal.border, 0.8)));
    head
}

fn draw_embed(f: &Frame, n: &Node, r: Rect, out: &mut DrawOut, acc: Color32) {
    let pal = f.pal;
    let z = f.z();
    let info = media::embed_info(&n.url);
    let can_live = platform::live_preview_supported();
    let live = can_live && is_live(n.id);
    let head = header(f, r, 30.0 * z);
    icons::paint_image(f.ui, Rect::from_center_size(pos2(head.left() + 15.0 * z, head.center().y), vec2(14.0 * z, 14.0 * z)), &media::favicon_url(&info.host), 3.0 * z);
    let mut x = head.right() - 15.0 * z;
    btn(f, out, n.id, pos2(x, head.center().y), 22.0 * z, l::EXTERNAL_LINK, A_OPEN, 0, pal.muted, false);
    x -= 24.0 * z;
    if can_live {
        btn(f, out, n.id, pos2(x, head.center().y), 22.0 * z, if live { l::PAUSE } else { l::PLAY }, A_LIVE, 0, if live { acc } else { pal.muted }, live);
        x -= 24.0 * z;
    }
    let title = if n.text.trim().is_empty() { format!("{}  ·  {}", info.service, info.host) } else { n.text.trim().to_owned() };
    text_fit(f.p, pos2(head.left() + 28.0 * z, head.center().y), Align2::LEFT_CENTER, &title, bold(12.5 * z), pal.text, (x - head.left() - 30.0 * z).max(20.0));
    let bodyr = Rect::from_min_max(pos2(r.left() + 5.0 * z, head.bottom() + 5.0 * z), pos2(r.right() - 5.0 * z, r.bottom() - 5.0 * z));
    if live {
        f.p.rect_filled(bodyr, 6.0 * z, pal.hover);
        f.p.text(bodyr.center(), Align2::CENTER_CENTER, "Loading…", body(13.0 * z), pal.muted);
        let url = media::frame_url(&info, (n.num & 7) as u8, 0);
        C.with(|c| c.borrow_mut().rects.1.push(EmbedRect { id: (1 << 40) | n.id, url, rect: bodyr, video: info.file, opts: (n.num & 7) as u8, clip: f.clip }));
        return;
    }
    // poster
    f.p.rect_filled(bodyr, 6.0 * z, if info.thumb.is_some() { Color32::BLACK } else { pal.hover });
    let mut drawn = false;
    for th in [info.thumb.as_ref(), info.thumb_alt.as_ref()].into_iter().flatten() {
        let (s, size) = icons::image_size(f.ui, th, bodyr.size());
        if s == icons::ImgState::Ready {
            let pr = fit(bodyr, size.unwrap_or(vec2(16.0, 9.0)));
            icons::paint_image(f.ui, pr, th, 6.0 * z);
            drawn = true;
            break;
        }
        if s == icons::ImgState::Loading {
            f.ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
            break;
        }
    }
    let c = bodyr.center();
    if drawn || info.kind == media::EmbedKind::Video {
        let d = (46.0 * z).min(bodyr.height() * 0.5);
        f.p.circle_filled(c, d / 2.0, Color32::from_black_alpha(150));
        icons::glyph(f.p, c + vec2(1.5 * z, 0.0), d * 0.5, l::PLAY, Color32::WHITE);
    } else {
        icons::glyph(f.p, c + vec2(0.0, -16.0 * z), 30.0 * z, kind_glyph(n), if hovered(f, bodyr) { pal.accent } else { pal.muted });
        text_fit(f.p, c + vec2(0.0, 14.0 * z), Align2::CENTER_CENTER, &info.host, bold(15.0 * z), pal.text, bodyr.width() - 24.0 * z);
        text_fit(f.p, c + vec2(0.0, 34.0 * z), Align2::CENTER_CENTER, n.url.trim_start_matches("https://").trim_start_matches("http://"), body(11.5 * z), pal.muted, bodyr.width() - 24.0 * z);
    }
    let hint = if can_live { "Click to play / load live" } else { "Click to open in your browser" };
    f.p.text(pos2(c.x, bodyr.bottom() - 11.0 * z), Align2::CENTER_CENTER, hint, body(11.0 * z), if drawn { Color32::from_white_alpha(210) } else { pal.muted });
    out.acts.push((bodyr, n.id, if can_live { A_LIVE } else { A_OPEN }, 0));
}

/// Height (screen points at zoom 1) of the slim caption bar under a PDF card; the card's height is page height + this.
pub const PDF_CAP: f32 = 28.0;

/// Borderless PDF card: the page fills the card edge to edge (the card keeps the page's aspect ratio), the annotations made in the
/// reader are drawn on top of it, and a slim caption bar carries the file name, the page counter and first / previous / next.
fn draw_pdf(f: &Frame, ws: &Workspace, n: &Node, r: Rect, rad: f32, out: &mut DrawOut, acc: Color32) {
    let pal = f.pal;
    let z = f.z();
    let cap_h = PDF_CAP * z;
    let cap = Rect::from_min_max(pos2(r.left(), r.bottom() - cap_h), r.max);
    let area = Rect::from_min_max(r.min, pos2(r.right(), cap.top()));
    let page = n.num as usize;
    let cs = cover(f.ui.ctx(), &n.url, page);
    let mut pages = known_pages(&n.url);
    let r8 = rad.round().clamp(0.0, 40.0) as u8;
    match cs {
        CoverState::Ready(tex, size, p) => {
            pages = Some(p);
            let pr = fit(area, size);
            let top = (pr.top() - r.top()).abs() < 0.6;
            let cr = egui::CornerRadius { nw: if top && (pr.left() - r.left()).abs() < 0.6 { r8 } else { 0 }, ne: if top && (pr.right() - r.right()).abs() < 0.6 { r8 } else { 0 }, sw: 0, se: 0 };
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            f.p.add(egui::epaint::RectShape::filled(pr, cr, Color32::WHITE).with_texture(tex, uv));
            if size.x > 0.0 {
                // aspect lock: the card is exactly as tall as the page it shows (+ caption), so nothing is letter-boxed
                out.grow.push((n.id, n.w * size.y / size.x + PDF_CAP));
            }
            // annotations made in the reader (highlights, underlines, notes, drawings) stay visible on the card
            let key = media::media_id(&n.url).unwrap_or_else(|| crate::pdf::key_of(&n.url));
            if let Some(list) = ws.pdf_annots.get(&key) {
                let on: Vec<crate::model::PdfAnnot> = list.iter().filter(|a| a.page as usize == page).cloned().collect();
                if !on.is_empty() {
                    let notes = crate::pdfnotes::Notes::new(on);
                    let clipped = f.p.with_clip_rect(pr.intersect(f.p.clip_rect()));
                    notes.paint_page(&clipped, pr, page, crate::canvasdraw::is_dark(pal), acc, true);
                }
            }
        }
        CoverState::Pending => {
            f.ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
            f.p.rect_filled(area.shrink(1.0), egui::CornerRadius { nw: r8, ne: r8, sw: 0, se: 0 }, with_alpha(pal.hover, 0.8));
            f.p.text(area.center(), Align2::CENTER_CENTER, "Rendering page…", body(12.0 * z), pal.muted);
        }
        CoverState::Failed => {
            icons::glyph(f.p, area.center() + vec2(0.0, -10.0 * z), 26.0 * z, l::FILE_TEXT, pal.muted);
            f.p.text(area.center() + vec2(0.0, 14.0 * z), Align2::CENTER_CENTER, "No preview — tap to open", body(12.0 * z), pal.muted);
        }
    }
    out.acts.push((area, n.id, A_OPEN, 0));
    // caption bar
    f.p.line_segment([pos2(cap.left() + 1.0, cap.top()), pos2(cap.right() - 1.0, cap.top())], Stroke::new(1.0_f32, with_alpha(pal.border, 0.7)));
    let cy = cap.center().y;
    let total = pages.unwrap_or(0);
    let can_prev = page > 0;
    let can_next = total == 0 || page + 1 < total;
    let off = with_alpha(pal.muted, 0.4);
    let label = if total > 0 { format!("{} / {}", page + 1, total) } else { format!("{}", page + 1) };
    let g = f.p.layout_no_wrap(label, body(11.5 * z), pal.muted);
    let d = 22.0 * z;
    let x_next = r.right() - 13.0 * z;
    let right_txt = x_next - 12.0 * z;
    f.p.galley(pos2(right_txt - g.size().x, cy - g.size().y / 2.0), g.clone(), pal.muted);
    let x_prev = right_txt - g.size().x - 13.0 * z;
    let x_first = x_prev - 20.0 * z;
    btn(f, out, n.id, pos2(x_next, cy), d, l::CHEVRON_RIGHT, A_PAGE, 1, if can_next { acc } else { off }, false);
    btn(f, out, n.id, pos2(x_prev, cy), d, l::CHEVRON_LEFT, A_PAGE, -1, if can_prev { acc } else { off }, false);
    let first_tint = if can_prev { acc } else { off };
    btn(f, out, n.id, pos2(x_first, cy), d, l::CHEVRON_LEFT, A_PAGE, i32::MIN, first_tint, false);
    icons::glyph(f.p, pos2(x_first - 4.0 * z, cy), d * 0.52, l::CHEVRON_LEFT, first_tint);
    let name = if n.text.trim().is_empty() { "Document.pdf" } else { n.text.trim() };
    icons::glyph(f.p, pos2(cap.left() + 13.0 * z, cy), 13.0 * z, l::FILE_TEXT, Color32::from_rgb(214, 69, 65));
    text_fit(f.p, pos2(cap.left() + 26.0 * z, cy), Align2::LEFT_CENTER, name, bold(11.5 * z), pal.text, (x_first - 14.0 * z - (cap.left() + 26.0 * z)).max(0.0));
}

fn draw_file(f: &Frame, n: &Node, st: &NodeSt, r: Rect, out: &mut DrawOut, acc: Color32) {
    let pal = f.pal;
    let z = f.z();
    let pad = 14.0 * z;
    let tile = Rect::from_min_size(r.left_top() + vec2(pad, (r.height() - 42.0 * z) / 2.0), vec2(42.0 * z, 42.0 * z));
    f.p.rect_filled(tile, 9.0 * z, with_alpha(acc, 0.16));
    icons::glyph(f.p, tile.center(), 20.0 * z, file_glyph(&n.text, &n.url), acc);
    let name = if n.text.trim().is_empty() { media::host_of(&n.url) } else { n.text.trim().to_owned() };
    let tx = tile.right() + 12.0 * z;
    let tw = (r.right() - tx - pad - 20.0 * z).max(20.0);
    text_fit(f.p, pos2(tx, r.center().y - 9.0 * z), Align2::LEFT_CENTER, &name, bold(14.0 * z), pal.text, tw);
    let mut sub = file_ext(&n.text, &n.url);
    if n.num > 0 {
        if !sub.is_empty() {
            sub.push_str("  ·  ");
        }
        sub.push_str(&media::fmt_size(n.num as usize * 1024));
    }
    if sub.is_empty() {
        sub = "File".into();
    }
    text_fit(f.p, pos2(tx, r.center().y + 10.0 * z), Align2::LEFT_CENTER, &sub, body(12.0 * z), pal.muted, tw);
    let b = pos2(r.right() - pad, r.center().y);
    icons::glyph(f.p, b, 15.0 * z, l::DOWNLOAD, if st.hovered || st.selected { pal.accent } else { with_alpha(pal.muted, 0.7) });
    out.acts.push((r, n.id, A_OPEN, 0));
}

fn draw_db(f: &Frame, ws: &Workspace, n: &Node, st: &NodeSt, r: Rect, out: &mut DrawOut, acc: Color32) {
    let pal = f.pal;
    let z = f.z();
    let Some(dbid) = n.page else { return };
    let Some(p) = ws.page(dbid).filter(|p| !p.trashed && p.kind == PageKind::Database) else {
        f.p.text(r.center(), Align2::CENTER_CENTER, "Database not found", body(13.0 * z), pal.muted);
        return;
    };
    let head = header(f, r, 32.0 * z);
    if p.icon.is_empty() {
        icons::glyph(f.p, pos2(head.left() + 17.0 * z, head.center().y), 15.0 * z, l::DATABASE, acc);
    } else {
        icons::paint_page_icon(f.ui, pos2(head.left() + 17.0 * z, head.center().y), &p.icon, 16.0 * z, pal.text);
    }
    let rows = ws.rows(dbid);
    let title = if p.title.is_empty() { "Untitled".to_owned() } else { p.title.clone() };
    text_fit(f.p, pos2(head.left() + 32.0 * z, head.center().y), Align2::LEFT_CENTER, &format!("{title}  ·  {} rows", rows.len()), bold(13.0 * z), pal.text, head.width() - 70.0 * z);
    btn(f, out, n.id, pos2(head.right() - 16.0 * z, head.center().y), 22.0 * z, l::EXTERNAL_LINK, A_OPEN, 0, if st.hovered || st.selected { pal.accent } else { pal.muted }, false);
    let Some(db) = p.db.as_ref() else { return };
    let props: Vec<&crate::model::Prop> = db.props.iter().filter(|q| !matches!(q.kind, PropKind::Formula | PropKind::Relation | PropKind::Rollup)).take(4).collect();
    let cols = 1 + props.len();
    let inner = Rect::from_min_max(pos2(r.left() + 6.0 * z, head.bottom() + 4.0 * z), pos2(r.right() - 6.0 * z, r.bottom() - 6.0 * z));
    let rh = 25.0 * z;
    let cw = inner.width() / cols as f32;
    let clipped = f.p.with_clip_rect(inner.intersect(f.clip));
    let hr = Rect::from_min_size(inner.min, vec2(inner.width(), rh));
    clipped.rect_filled(hr, 4.0 * z, with_alpha(pal.muted, 0.10));
    for ci in 0..cols {
        let name = if ci == 0 { "Name" } else { props[ci - 1].name.as_str() };
        text_fit(&clipped, pos2(inner.left() + ci as f32 * cw + 6.0 * z, hr.center().y), Align2::LEFT_CENTER, name, bold(11.5 * z), pal.muted, cw - 10.0 * z);
    }
    let max_rows = (((inner.height() - rh) / rh).floor().max(0.0) as usize).min(60);
    let mut y = hr.bottom();
    for rid in rows.iter().take(max_rows) {
        let Some(row) = ws.page(*rid) else { continue };
        clipped.line_segment([pos2(inner.left(), y), pos2(inner.right(), y)], Stroke::new(1.0_f32, with_alpha(pal.border, 0.6)));
        let t = ws.display_title(*rid);
        text_fit(&clipped, pos2(inner.left() + 6.0 * z, y + rh / 2.0), Align2::LEFT_CENTER, &t, body(12.5 * z), pal.text, cw - 10.0 * z);
        for (ci, pr) in props.iter().enumerate() {
            let txt = crate::model::row_val(row, pr.id).map(|v| db.val_text(pr.id, &v)).unwrap_or_default();
            text_fit(&clipped, pos2(inner.left() + (ci + 1) as f32 * cw + 6.0 * z, y + rh / 2.0), Align2::LEFT_CENTER, &txt, body(12.5 * z), pal.text, cw - 10.0 * z);
        }
        y += rh;
    }
    if rows.len() > max_rows {
        clipped.text(pos2(inner.center().x, (y + rh / 2.0).min(inner.bottom() - 8.0 * z)), Align2::CENTER_CENTER, format!("+ {} more rows — open the database", rows.len() - max_rows), body(11.5 * z), pal.muted);
    } else if rows.is_empty() {
        clipped.text(pos2(inner.center().x, y + rh), Align2::CENTER_CENTER, "No rows yet", body(12.5 * z), pal.muted);
    }
    out.acts.push((inner, n.id, A_OPEN, 0));
}

// ───────────────────────────── widgets ─────────────────────────────

fn num_text(v: f64) -> String {
    if v.fract().abs() < 1e-9 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v:.2}")
    }
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTH_FULL: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// (HH:MM, SS, "Mon, 5 Jan 2026") for a timestamp already shifted into the target zone.
fn clock_text(ms: i64) -> (String, String, String) {
    let secs = ms.div_euclid(1000);
    let day = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = dates::civil_from_days(day);
    let wd = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][dates::weekday(y, m, d) as usize % 7];
    (format!("{:02}:{:02}", tod / 3600, tod % 3600 / 60), format!("{:02}", tod % 60), format!("{wd}, {d} {} {y}", MONTHS[(m as usize + 11) % 12]))
}

fn elapsed_ms(v: &Value, now: f64) -> f64 {
    let run = jf(v, "run", 0.0);
    jf(v, "acc", 0.0) + if run > 0.0 { (now - run).max(0.0) } else { 0.0 }
}

fn mmss(ms: f64) -> String {
    let s = (ms / 1000.0).ceil().max(0.0) as i64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

fn draw_widget(f: &Frame, n: &Node, st: &NodeSt, r: Rect, out: &mut DrawOut, acc: Color32) {
    let pal = f.pal;
    let z = f.z();
    let v = jd(n);
    let key = wkind(n);
    let now = crate::auth::now_ms();
    let title = js(&v, "t");
    let pad = 12.0 * z;
    let hv = st.hovered || st.selected;
    let gi = WIDGETS.iter().find(|w| w.0 == key).map_or(l::HASH, |w| w.1);
    let top = r.top() + 10.0 * z;
    if !title.is_empty() {
        icons::glyph(f.p, pos2(r.left() + pad + 6.0 * z, top + 8.0 * z), 13.0 * z, gi, acc);
        text_fit(f.p, pos2(r.left() + pad + 20.0 * z, top + 8.0 * z), Align2::LEFT_CENTER, &title, bold(12.5 * z), pal.muted, r.width() - pad * 2.0 - 46.0 * z);
    }
    btn(f, out, n.id, pos2(r.right() - pad - 6.0 * z, top + 8.0 * z), 22.0 * z, l::SETTINGS, A_EDIT, 0, if hv { pal.accent } else { with_alpha(pal.muted, 0.55) }, false);
    let body_r = Rect::from_min_max(pos2(r.left() + pad, top + 22.0 * z), pos2(r.right() - pad, r.bottom() - 10.0 * z));
    let c = body_r.center();
    match key.as_str() {
        "counter" => {
            let val = jf(&v, "v", 0.0);
            let goal = jf(&v, "goal", 0.0);
            let big = (body_r.height() * 0.5).clamp(18.0 * z, 46.0 * z);
            let cy = if goal > 0.0 { c.y - 6.0 * z } else { c.y };
            f.p.text(pos2(c.x, cy), Align2::CENTER_CENTER, num_text(val), bold(big), pal.text);
            let bd = (34.0 * z).min(body_r.height() * 0.5);
            btn(f, out, n.id, pos2(body_r.left() + bd * 0.6, cy), bd, l::MINUS, A_INC, -1, acc, true);
            btn(f, out, n.id, pos2(body_r.right() - bd * 0.6, cy), bd, l::PLUS, A_INC, 1, acc, true);
            if goal > 0.0 {
                let bar = Rect::from_min_size(pos2(body_r.left(), body_r.bottom() - 9.0 * z), vec2(body_r.width(), 6.0 * z));
                f.p.rect_filled(bar, 3.0 * z, with_alpha(pal.muted, 0.2));
                let fr = (val / goal).clamp(0.0, 1.0) as f32;
                f.p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * fr, bar.height())), 3.0 * z, acc);
                f.p.text(pos2(c.x, bar.top() - 7.0 * z), Align2::CENTER_CENTER, format!("of {}", num_text(goal)), body(11.0 * z), pal.muted);
            }
        }
        "timer" | "pomodoro" => {
            let el = elapsed_ms(&v, now);
            let running = jf(&v, "run", 0.0) > 0.0;
            let (text, sub, frac, done) = if key == "timer" {
                if js(&v, "mode") == "up" {
                    (mmss(el.floor()), if running { "running".to_owned() } else { "stopwatch".to_owned() }, None, false)
                } else {
                    let total = jf(&v, "mins", 25.0).max(0.1) * 60_000.0;
                    let rem = (total - el).max(0.0);
                    let sub = if rem <= 0.0 {
                        "time is up"
                    } else if running {
                        "counting down"
                    } else {
                        "paused"
                    };
                    (mmss(rem), sub.to_owned(), Some((el / total).clamp(0.0, 1.0) as f32), rem <= 0.0)
                }
            } else {
                let fm = jf(&v, "focus", 25.0).max(0.1) * 60_000.0;
                let bm = jf(&v, "brk", 5.0).max(0.1) * 60_000.0;
                let cyc = fm + bm;
                let pos = el % cyc;
                let sessions = (el / cyc).floor() as i64;
                if pos < fm {
                    (mmss(fm - pos), format!("Focus · {sessions} done"), Some((pos / fm) as f32), false)
                } else {
                    (mmss(cyc - pos), format!("Break · {sessions} done"), Some(((pos - fm) / bm) as f32), false)
                }
            };
            f.p.text(pos2(c.x, body_r.top() + body_r.height() * 0.30), Align2::CENTER_CENTER, text, bold((body_r.height() * 0.40).clamp(18.0 * z, 40.0 * z)), if done { acc } else { pal.text });
            f.p.text(pos2(c.x, body_r.top() + body_r.height() * 0.58), Align2::CENTER_CENTER, sub, body(11.5 * z), pal.muted);
            if let Some(fr) = frac {
                let bar = Rect::from_min_size(pos2(body_r.left(), body_r.top() + body_r.height() * 0.68), vec2(body_r.width(), 5.0 * z));
                f.p.rect_filled(bar, 2.5 * z, with_alpha(pal.muted, 0.2));
                f.p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * fr, bar.height())), 2.5 * z, acc);
            }
            let by = body_r.bottom() - 13.0 * z;
            let d = 28.0 * z;
            let n_btn = if key == "pomodoro" { 3.0 } else { 2.0 };
            let mut bx = c.x - (n_btn - 1.0) * (d + 8.0 * z) / 2.0;
            btn(f, out, n.id, pos2(bx, by), d, if running { l::PAUSE } else { l::PLAY }, A_RUN, 0, acc, true);
            bx += d + 8.0 * z;
            btn(f, out, n.id, pos2(bx, by), d, l::ROTATE_CCW, A_RESET, 0, pal.muted, false);
            if key == "pomodoro" {
                bx += d + 8.0 * z;
                btn(f, out, n.id, pos2(bx, by), d, l::ARROW_RIGHT, A_SKIP, 0, pal.muted, false);
            }
            if running {
                f.ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
            }
        }
        "progress" => {
            let val = jf(&v, "v", 0.0).clamp(0.0, 100.0);
            let ty = body_r.top() + body_r.height() * 0.28;
            f.p.text(pos2(body_r.left(), ty), Align2::LEFT_CENTER, format!("{}%", val.round() as i64), bold((body_r.height() * 0.46).clamp(16.0 * z, 34.0 * z)), pal.text);
            let bar = Rect::from_min_size(pos2(body_r.left() + 8.0 * z, body_r.bottom() - 24.0 * z), vec2(body_r.width() - 16.0 * z, 12.0 * z));
            f.p.rect_filled(bar, 6.0 * z, with_alpha(pal.muted, 0.2));
            let kx = bar.left() + bar.width() * val as f32 / 100.0;
            f.p.rect_filled(Rect::from_min_size(bar.min, vec2((kx - bar.left()).max(0.0), bar.height())), 6.0 * z, acc);
            f.p.circle_filled(pos2(kx, bar.center().y), 8.0 * z, Color32::WHITE);
            f.p.circle_stroke(pos2(kx, bar.center().y), 8.0 * z, Stroke::new(2.0_f32, acc));
            out.acts.push((bar.expand2(vec2(8.0 * z, 9.0 * z)), n.id, A_BAR, 0));
            btn(f, out, n.id, pos2(body_r.right() - 46.0 * z, ty), 26.0 * z, l::MINUS, A_PROG, -5, acc, true);
            btn(f, out, n.id, pos2(body_r.right() - 14.0 * z, ty), 26.0 * z, l::PLUS, A_PROG, 5, acc, true);
        }
        "poll" => {
            let opts = jstrs(&v, "o");
            let votes = jnums(&v, "v");
            let total: f64 = votes.iter().sum();
            let rows = opts.len().max(1) as f32;
            let rh = ((body_r.height() - 4.0 * z) / rows).min(34.0 * z);
            for (i, o) in opts.iter().enumerate() {
                let rr = Rect::from_min_size(pos2(body_r.left(), body_r.top() + 2.0 * z + i as f32 * rh), vec2(body_r.width(), rh - 5.0 * z));
                let cnt = votes.get(i).copied().unwrap_or(0.0);
                let share = if total > 0.0 { (cnt / total) as f32 } else { 0.0 };
                f.p.rect_filled(rr, 6.0 * z, with_alpha(pal.muted, if hovered(f, rr) { 0.2 } else { 0.12 }));
                f.p.rect_filled(Rect::from_min_size(rr.min, vec2(rr.width() * share, rr.height())), 6.0 * z, with_alpha(acc, 0.35));
                text_fit(f.p, pos2(rr.left() + 8.0 * z, rr.center().y), Align2::LEFT_CENTER, o, body(12.5 * z), pal.text, rr.width() - 70.0 * z);
                f.p.text(pos2(rr.right() - 8.0 * z, rr.center().y), Align2::RIGHT_CENTER, format!("{} · {}%", num_text(cnt), (share * 100.0).round() as i64), body(11.0 * z), pal.muted);
                out.acts.push((rr, n.id, A_VOTE, i as i32));
            }
        }
        "clock" => {
            let off = jf(&v, "tz", 0.0) as i64;
            let shifted = now as i64 + off * 60_000;
            let (hm, ss, date) = clock_text(shifted);
            let (hm, ampm) = if jb(&v, "h24", true) {
                (hm, String::new())
            } else {
                let h: i64 = hm[..2].parse().unwrap_or(0);
                (format!("{:02}{}", if h % 12 == 0 { 12 } else { h % 12 }, &hm[2..]), if h >= 12 { " PM".to_owned() } else { " AM".to_owned() })
            };
            let secs = jb(&v, "secs", true);
            let txt = if secs { format!("{hm}:{ss}{ampm}") } else { format!("{hm}{ampm}") };
            f.p.text(pos2(c.x, c.y - 6.0 * z), Align2::CENTER_CENTER, txt, bold((body_r.width() * 0.17).clamp(16.0 * z, 40.0 * z)), pal.text);
            f.p.text(pos2(c.x, c.y + body_r.height() * 0.27), Align2::CENTER_CENTER, date, body(11.5 * z), pal.muted);
            f.ui.ctx().request_repaint_after(std::time::Duration::from_millis(if secs { 500 } else { 5000 }));
        }
        "calendar" => draw_calendar(f, n, &v, body_r, out, acc),
        _ => draw_sheet(f, &v, body_r),
    }
}

fn cal_view(v: &Value) -> (i32, u32) {
    let (ty, tm, _) = dates::today();
    (jf(v, "y", ty as f64) as i32, (jf(v, "m", tm as f64) as u32).clamp(1, 12))
}

fn draw_calendar(f: &Frame, n: &Node, v: &Value, r: Rect, out: &mut DrawOut, acc: Color32) {
    let pal = f.pal;
    let z = f.z();
    let (y, m) = cal_view(v);
    let (ty, tm, td) = dates::today();
    let marks: HashSet<String> = jstrs(v, "marks").into_iter().collect();
    let hh = 24.0 * z;
    btn(f, out, n.id, pos2(r.left() + 10.0 * z, r.top() + hh / 2.0), 22.0 * z, l::CHEVRON_LEFT, A_MONTH, -1, pal.muted, false);
    btn(f, out, n.id, pos2(r.right() - 10.0 * z, r.top() + hh / 2.0), 22.0 * z, l::CHEVRON_RIGHT, A_MONTH, 1, pal.muted, false);
    f.p.text(pos2(r.center().x, r.top() + hh / 2.0), Align2::CENTER_CENTER, format!("{} {y}", MONTH_FULL[(m - 1) as usize]), bold(13.0 * z), pal.text);
    let gy = r.top() + hh + 18.0 * z;
    let cw = r.width() / 7.0;
    let ch = ((r.bottom() - gy) / 6.0).max(8.0);
    for (i, d) in ["M", "T", "W", "T", "F", "S", "S"].iter().enumerate() {
        f.p.text(pos2(r.left() + (i as f32 + 0.5) * cw, gy - 9.0 * z), Align2::CENTER_CENTER, d, body(10.5 * z), pal.muted);
    }
    let first = (dates::weekday(y, m, 1) + 6) % 7; // Monday first
    for d in 1..=dates::days_in_month(y, m) {
        let idx = first + d - 1;
        let (col, row) = ((idx % 7) as f32, (idx / 7) as f32);
        let cell = Rect::from_min_size(pos2(r.left() + col * cw, gy + row * ch), vec2(cw, ch)).shrink(1.5 * z);
        let marked = marks.contains(&dates::iso(y, m, d));
        let today = (y, m, d) == (ty, tm, td);
        let cc = cell.center();
        let rad = (cell.width().min(cell.height()) / 2.0 - 1.0).max(3.0);
        if marked {
            f.p.circle_filled(cc, rad, acc);
        } else if hovered(f, cell) {
            f.p.circle_filled(cc, rad, with_alpha(pal.muted, 0.16));
        }
        if today && !marked {
            f.p.circle_stroke(cc, rad, Stroke::new(1.6_f32, acc));
        }
        f.p.text(cc, Align2::CENTER_CENTER, d.to_string(), if today { bold(11.5 * z) } else { body(11.5 * z) }, if marked { Color32::WHITE } else { pal.text });
        out.acts.push((cell, n.id, A_DAY, d as i32));
    }
}

fn draw_sheet(f: &Frame, v: &Value, r: Rect) {
    let pal = f.pal;
    let z = f.z();
    let cells = jgrid(v);
    let nr = cells.len().max(1);
    let nc = cells.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
    let rh = (r.height() / nr as f32).min(28.0 * z);
    let cw = r.width() / nc as f32;
    let clipped = f.p.with_clip_rect(r.intersect(f.clip));
    for (ri, row) in cells.iter().enumerate() {
        let y = r.top() + ri as f32 * rh;
        if ri == 0 {
            clipped.rect_filled(Rect::from_min_size(pos2(r.left(), y), vec2(r.width(), rh)), 3.0 * z, with_alpha(pal.muted, 0.12));
        }
        clipped.line_segment([pos2(r.left(), y + rh), pos2(r.right(), y + rh)], Stroke::new(1.0_f32, with_alpha(pal.border, 0.9)));
        for (ci, t) in row.iter().enumerate() {
            let num = ri > 0 && t.trim().parse::<f64>().is_ok();
            let x = r.left() + ci as f32 * cw;
            if num {
                text_fit(&clipped, pos2(x + cw - 6.0 * z, y + rh / 2.0), Align2::RIGHT_CENTER, t, body(12.0 * z), pal.text, cw - 10.0 * z);
            } else {
                text_fit(&clipped, pos2(x + 6.0 * z, y + rh / 2.0), Align2::LEFT_CENTER, t, if ri == 0 { bold(12.0 * z) } else { body(12.0 * z) }, pal.text, cw - 10.0 * z);
            }
        }
    }
    for ci in 1..nc {
        let x = r.left() + ci as f32 * cw;
        clipped.line_segment([pos2(x, r.top()), pos2(x, r.top() + nr as f32 * rh)], Stroke::new(1.0_f32, with_alpha(pal.border, 0.6)));
    }
    let mut sums: Vec<Option<f64>> = vec![None; nc];
    for row in cells.iter().skip(1) {
        for (ci, t) in row.iter().enumerate() {
            if let Ok(x) = t.trim().parse::<f64>() {
                *sums[ci].get_or_insert(0.0) += x;
            }
        }
    }
    if sums.iter().any(|s| s.is_some()) && nr as f32 * rh + 22.0 * z <= r.height() + 2.0 {
        let y = r.top() + nr as f32 * rh + 10.0 * z;
        for (ci, s) in sums.iter().enumerate() {
            if let Some(s) = s {
                text_fit(&clipped, pos2(r.left() + (ci + 1) as f32 * cw - 6.0 * z, y), Align2::RIGHT_CENTER, &format!("Σ {}", num_text(*s)), bold(11.0 * z), pal.muted, cw - 8.0 * z);
            }
        }
    }
}

// ───────────────────────────── click handling ─────────────────────────────

/// Applies a click on a hot spot. `frac` is the horizontal position of the click inside the hot rectangle (0‥1).
pub fn act(c: &mut Canvas, id: u64, code: u16, arg: i32, frac: f32) -> Fx {
    let now = crate::auth::now_ms();
    let Some(n) = c.node(id).cloned() else { return Fx::None };
    match code {
        A_OPEN => {
            return match n.kind {
                Kind::DbView => n.page.map_or(Fx::None, Fx::Page),
                Kind::Pdf => Fx::Pdf { media: media::media_id(&n.url).unwrap_or(0), url: n.url.clone(), title: if n.text.is_empty() { "Document.pdf".into() } else { n.text.clone() } },
                Kind::File => match media::media_id(&n.url) {
                    Some(m) => Fx::File { media: m, name: if n.text.is_empty() { "file".into() } else { n.text.clone() } },
                    None => Fx::Url(media::normalize_url(&n.url)),
                },
                _ => Fx::Url(media::normalize_url(&n.url)),
            };
        }
        A_LIVE => {
            toggle_live(id);
            return Fx::None;
        }
        A_EDIT => return Fx::Edit(id),
        A_PAGE => {
            let known = known_pages(&n.url).unwrap_or(usize::MAX);
            if let Some(nn) = c.node_mut(id) {
                let p = if arg == i32::MIN { 0 } else { nn.num as i64 + arg as i64 };
                nn.num = p.clamp(0, known.saturating_sub(1).min(100_000) as i64) as u32;
            }
            c.rev += 1;
            return Fx::None;
        }
        _ => {}
    }
    if n.kind != Kind::Widget {
        return Fx::None;
    }
    let mut v = jd(&n);
    if !matches!(code, A_RUN | A_MONTH) {
        c.checkpoint();
    }
    match code {
        A_INC => {
            let step = jf(&v, "step", 1.0);
            v["v"] = json!(jf(&v, "v", 0.0) + step * arg as f64);
        }
        A_RUN => {
            let run = jf(&v, "run", 0.0);
            if run > 0.0 {
                v["acc"] = json!(jf(&v, "acc", 0.0) + (now - run).max(0.0));
                v["run"] = json!(0);
            } else {
                v["run"] = json!(now);
            }
        }
        A_RESET => {
            v["acc"] = json!(0);
            v["run"] = json!(0);
        }
        A_SKIP => {
            let (fm, bm) = (jf(&v, "focus", 25.0).max(0.1) * 60_000.0, jf(&v, "brk", 5.0).max(0.1) * 60_000.0);
            let el = elapsed_ms(&v, now);
            let cyc = fm + bm;
            let pos = el % cyc;
            let jump = if pos < fm { fm - pos } else { cyc - pos };
            v["acc"] = json!(jf(&v, "acc", 0.0) + jump + 1.0);
        }
        A_VOTE => {
            let mut votes = jnums(&v, "v");
            let i = arg.max(0) as usize;
            if votes.len() <= i {
                votes.resize(i + 1, 0.0);
            }
            votes[i] += 1.0;
            v["v"] = json!(votes);
        }
        A_PROG => v["v"] = json!((jf(&v, "v", 0.0) + arg as f64).clamp(0.0, 100.0)),
        A_BAR => v["v"] = json!(((frac as f64 * 100.0 / 5.0).round() * 5.0).clamp(0.0, 100.0)),
        A_MONTH => {
            let (mut y, m) = cal_view(&v);
            let mm = m as i32 - 1 + arg;
            y += mm.div_euclid(12);
            v["y"] = json!(y);
            v["m"] = json!(mm.rem_euclid(12) + 1);
        }
        A_DAY => {
            let (y, m) = cal_view(&v);
            let iso = dates::iso(y, m, arg.max(1) as u32);
            let mut marks = jstrs(&v, "marks");
            if let Some(i) = marks.iter().position(|s| *s == iso) {
                marks.remove(i);
            } else {
                marks.push(iso);
            }
            v["marks"] = json!(marks);
        }
        _ => {}
    }
    if let Some(nn) = c.node_mut(id) {
        nn.data = v.to_string();
    }
    c.rev += 1;
    Fx::None
}

// ───────────────────────────── editing UI ─────────────────────────────

fn text_field(ui: &mut Ui, pal: &Palette, v: &mut Value, k: &str, label: &str, changed: &mut bool) {
    let mut s = js(v, k);
    ui.label(egui::RichText::new(label).font(body(12.0)).color(pal.muted));
    if ui.add(egui::TextEdit::singleline(&mut s).desired_width(f32::INFINITY).font(body(14.0))).changed() {
        v[k] = json!(s);
        *changed = true;
    }
}

fn num_field(ui: &mut Ui, pal: &Palette, v: &mut Value, k: &str, label: &str, d: f64, range: std::ops::RangeInclusive<f64>, changed: &mut bool) {
    let mut x = jf(v, k, d);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).font(body(13.0)).color(pal.muted));
        if ui.add(egui::DragValue::new(&mut x).range(range).speed(0.5)).changed() {
            v[k] = json!(x);
            *changed = true;
        }
    });
}

/// Settings form of a widget (shown in a popover). Returns true when something changed.
pub fn edit_widget(ui: &mut Ui, pal: &Palette, n: &mut Node) -> bool {
    let mut v = jd(n);
    let mut key = wkind(n);
    let was = key.clone();
    let mut changed = false;
    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
    ui.label(egui::RichText::new("Widget").font(bold(12.0)).color(pal.muted));
    ui.horizontal_wrapped(|ui| {
        for (k, g, name) in WIDGETS {
            let on = key == k;
            let label = format!("{g}  {}", name.split(' ').next().unwrap_or(name));
            if ui.selectable_label(on, egui::RichText::new(label).font(body(13.0))).clicked() && !on {
                let t = js(&v, "t");
                let (mut nv, _) = widget_defaults(k);
                if !t.is_empty() && !js(&nv, "t").is_empty() {
                    nv["t"] = json!(t);
                }
                v = nv;
                key = k.to_owned();
                changed = true;
            }
        }
    });
    ui.add_space(2.0);
    match key.as_str() {
        "counter" => {
            text_field(ui, pal, &mut v, "t", "Label", &mut changed);
            num_field(ui, pal, &mut v, "v", "Value", 0.0, -1e9..=1e9, &mut changed);
            num_field(ui, pal, &mut v, "step", "Step", 1.0, 0.0..=1e6, &mut changed);
            num_field(ui, pal, &mut v, "goal", "Goal (0 = none)", 0.0, 0.0..=1e9, &mut changed);
        }
        "timer" => {
            text_field(ui, pal, &mut v, "t", "Label", &mut changed);
            let up = js(&v, "mode") == "up";
            ui.horizontal(|ui| {
                if ui.selectable_label(!up, "Countdown").clicked() && up {
                    v["mode"] = json!("down");
                    v["acc"] = json!(0);
                    v["run"] = json!(0);
                    changed = true;
                }
                if ui.selectable_label(up, "Stopwatch").clicked() && !up {
                    v["mode"] = json!("up");
                    v["acc"] = json!(0);
                    v["run"] = json!(0);
                    changed = true;
                }
            });
            if !up {
                num_field(ui, pal, &mut v, "mins", "Minutes", 25.0, 0.1..=1440.0, &mut changed);
            }
        }
        "pomodoro" => {
            text_field(ui, pal, &mut v, "t", "Label", &mut changed);
            num_field(ui, pal, &mut v, "focus", "Focus minutes", 25.0, 1.0..=180.0, &mut changed);
            num_field(ui, pal, &mut v, "brk", "Break minutes", 5.0, 1.0..=60.0, &mut changed);
        }
        "progress" => {
            text_field(ui, pal, &mut v, "t", "Label", &mut changed);
            num_field(ui, pal, &mut v, "v", "Percent", 0.0, 0.0..=100.0, &mut changed);
        }
        "poll" => {
            text_field(ui, pal, &mut v, "t", "Question", &mut changed);
            let mut opts = jstrs(&v, "o");
            let mut votes = jnums(&v, "v");
            votes.resize(opts.len(), 0.0);
            let mut del = None;
            for (i, o) in opts.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    if ui.add(egui::TextEdit::singleline(o).desired_width((ui.available_width() - 80.0).max(60.0)).font(body(14.0))).changed() {
                        changed = true;
                    }
                    ui.label(egui::RichText::new(format!("{} votes", num_text(votes[i]))).font(body(12.0)).color(pal.muted));
                    if ui.small_button("✕").clicked() {
                        del = Some(i);
                    }
                });
            }
            if let Some(i) = del {
                opts.remove(i);
                votes.remove(i);
                changed = true;
            }
            ui.horizontal(|ui| {
                if opts.len() < 12 && ui.button("+ Option").clicked() {
                    opts.push(format!("Option {}", opts.len() + 1));
                    votes.push(0.0);
                    changed = true;
                }
                if ui.button("Reset votes").clicked() {
                    votes.iter_mut().for_each(|x| *x = 0.0);
                    changed = true;
                }
            });
            v["o"] = json!(opts);
            v["v"] = json!(votes);
        }
        "clock" => {
            text_field(ui, pal, &mut v, "t", "Label (e.g. Tokyo)", &mut changed);
            let mut h = jf(&v, "tz", 0.0) / 60.0;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("UTC offset (hours)").font(body(13.0)).color(pal.muted));
                if ui.add(egui::DragValue::new(&mut h).range(-12.0..=14.0).speed(0.25)).changed() {
                    v["tz"] = json!((h * 60.0).round());
                    changed = true;
                }
            });
            let (mut h24, mut secs) = (jb(&v, "h24", true), jb(&v, "secs", true));
            ui.horizontal(|ui| {
                if ui.checkbox(&mut h24, "24-hour").changed() {
                    v["h24"] = json!(h24);
                    changed = true;
                }
                if ui.checkbox(&mut secs, "Seconds").changed() {
                    v["secs"] = json!(secs);
                    changed = true;
                }
            });
        }
        "calendar" => {
            text_field(ui, pal, &mut v, "t", "Title (e.g. Gym days)", &mut changed);
            ui.label(egui::RichText::new("Tap a day on the board to mark it — handy as a habit tracker.").font(body(12.0)).color(pal.muted));
            if ui.button("Clear all marks").clicked() {
                v["marks"] = json!([]);
                changed = true;
            }
        }
        _ => {
            text_field(ui, pal, &mut v, "t", "Title", &mut changed);
            let mut cells = jgrid(&v);
            if cells.is_empty() {
                cells.push(vec![String::new()]);
            }
            let nc = cells.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
            for row in cells.iter_mut() {
                row.resize(nc, String::new());
            }
            egui::ScrollArea::both().max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
                egui::Grid::new(("sheet_edit", n.id)).spacing([4.0, 4.0]).show(ui, |ui| {
                    for row in cells.iter_mut() {
                        for cell in row.iter_mut() {
                            if ui.add(egui::TextEdit::singleline(cell).desired_width(78.0).font(body(13.0))).changed() {
                                changed = true;
                            }
                        }
                        ui.end_row();
                    }
                });
            });
            ui.horizontal_wrapped(|ui| {
                if ui.button("+ Row").clicked() {
                    cells.push(vec![String::new(); nc]);
                    changed = true;
                }
                if ui.button("+ Column").clicked() {
                    cells.iter_mut().for_each(|r| r.push(String::new()));
                    changed = true;
                }
                if cells.len() > 1 && ui.button("− Row").clicked() {
                    cells.pop();
                    changed = true;
                }
                if nc > 1 && ui.button("− Column").clicked() {
                    cells.iter_mut().for_each(|r| {
                        r.pop();
                    });
                    changed = true;
                }
            });
            v["c"] = json!(cells);
        }
    }
    if changed {
        v["k"] = json!(key);
        n.data = v.to_string();
        if was != key {
            let (_, size) = widget_defaults(&key);
            n.w = size.x;
            n.h = size.y;
        }
    }
    changed
}

/// Text a node contributes to "find on canvas".
pub fn search_text(n: &Node) -> String {
    match n.kind {
        Kind::Widget => {
            let v = jd(n);
            let mut s = js(&v, "t");
            for o in jstrs(&v, "o") {
                s.push(' ');
                s.push_str(&o);
            }
            s
        }
        _ => format!("{} {}", n.text, n.url),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widget(c: &mut Canvas, key: &str) -> u64 {
        let id = c.add_node(Kind::Widget, egui::pos2(0.0, 0.0));
        make_widget(c.node_mut(id).unwrap(), key, true);
        id
    }

    #[test]
    fn counter_buttons_and_undo() {
        let mut c = Canvas::new();
        let id = widget(&mut c, "counter");
        for _ in 0..3 {
            act(&mut c, id, A_INC, 1, 0.0);
        }
        act(&mut c, id, A_INC, -1, 0.0);
        assert_eq!(jf(&jd(c.node(id).unwrap()), "v", -1.0), 2.0);
        assert!(c.undo());
        assert_eq!(jf(&jd(c.node(id).unwrap()), "v", -1.0), 2.0 + 1.0);
    }

    #[test]
    fn progress_bar_snaps_to_five_percent() {
        let mut c = Canvas::new();
        let id = widget(&mut c, "progress");
        act(&mut c, id, A_BAR, 0, 0.73);
        assert_eq!(jf(&jd(c.node(id).unwrap()), "v", -1.0), 75.0);
        act(&mut c, id, A_PROG, 50, 0.0);
        assert_eq!(jf(&jd(c.node(id).unwrap()), "v", -1.0), 100.0);
    }

    #[test]
    fn poll_votes_and_calendar_marks() {
        let mut c = Canvas::new();
        let p = widget(&mut c, "poll");
        act(&mut c, p, A_VOTE, 1, 0.0);
        act(&mut c, p, A_VOTE, 1, 0.0);
        assert_eq!(jnums(&jd(c.node(p).unwrap()), "v"), vec![0.0, 2.0, 0.0]);
        let k = widget(&mut c, "calendar");
        act(&mut c, k, A_DAY, 5, 0.0);
        assert_eq!(jstrs(&jd(c.node(k).unwrap()), "marks").len(), 1);
        act(&mut c, k, A_DAY, 5, 0.0);
        assert!(jstrs(&jd(c.node(k).unwrap()), "marks").is_empty());
        act(&mut c, k, A_MONTH, 13, 0.0);
        let (y0, _) = cal_view(&json!({}));
        assert!(cal_view(&jd(c.node(k).unwrap())).0 >= y0 + 1);
    }

    #[test]
    fn timer_runs_and_resets() {
        let mut c = Canvas::new();
        let id = widget(&mut c, "timer");
        act(&mut c, id, A_RUN, 0, 0.0);
        assert!(jf(&jd(c.node(id).unwrap()), "run", 0.0) > 0.0);
        act(&mut c, id, A_RUN, 0, 0.0);
        assert_eq!(jf(&jd(c.node(id).unwrap()), "run", 1.0), 0.0);
        act(&mut c, id, A_RESET, 0, 0.0);
        assert_eq!(jf(&jd(c.node(id).unwrap()), "acc", 1.0), 0.0);
    }

    #[test]
    fn rich_nodes_survive_the_json_round_trip() {
        let mut c = Canvas::new();
        let w = widget(&mut c, "sheet");
        let e = c.add_node(Kind::Embed, egui::pos2(300.0, 0.0));
        c.node_mut(e).unwrap().url = "https://youtu.be/dQw4w9WgXcQ".into();
        let p = c.add_node(Kind::Pdf, egui::pos2(600.0, 0.0));
        {
            let n = c.node_mut(p).unwrap();
            n.url = "media:7".into();
            n.num = 3;
            n.text = "paper.pdf".into();
        }
        let json = c.to_json(None);
        let mut o = Canvas::new();
        let ids = o.import_json(&json, egui::pos2(0.0, 0.0));
        assert_eq!(ids.len(), 3);
        assert!(o.nodes.iter().any(|n| n.kind == Kind::Widget && n.data.contains("\"sheet\"")));
        assert!(o.nodes.iter().any(|n| n.kind == Kind::Embed && n.url.contains("youtu.be")));
        assert!(o.nodes.iter().any(|n| n.kind == Kind::Pdf && n.num == 3 && n.url == "media:7"));
        let _ = w;
    }

    #[test]
    fn click_opens_documents() {
        let mut c = Canvas::new();
        let p = c.add_node(Kind::Pdf, egui::pos2(0.0, 0.0));
        c.node_mut(p).unwrap().url = "media:9".into();
        match act(&mut c, p, A_OPEN, 0, 0.0) {
            Fx::Pdf { media, .. } => assert_eq!(media, 9),
            _ => panic!("expected the PDF reader"),
        }
        let f = c.add_node(Kind::File, egui::pos2(0.0, 0.0));
        c.node_mut(f).unwrap().url = "https://example.com/a.zip".into();
        assert!(matches!(act(&mut c, f, A_OPEN, 0, 0.0), Fx::Url(_)));
    }
}

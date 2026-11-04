//! PDF reader.
//!
//! Design goals: **zero cost while no PDF is open**, smooth reading, and the same code on Windows and WebAssembly.
//!
//! * PDF blocks in a page are only a small card. Nothing is parsed, rendered or kept in memory for them. File bytes of attached
//!   documents are not even held in RAM (see `media.rs`); they are read from disk / local storage when the reader opens.
//! * Clicking *Read* opens a full-window reader: continuous scrolling, pinch / Ctrl+wheel zoom anchored under the pointer
//!   (25 % – 600 %), fit width / fit page, page jump, thumbnails, keyboard navigation, night mode, password prompt for
//!   encrypted files.
//! * Rendering is done with [`hayro`] (pure Rust). Pages are rasterised in 512 px **tiles**, only for the part of the page that is
//!   (almost) on screen, at the exact on-screen resolution. A small low-resolution preview per page and the tiles of the previous
//!   zoom level are shown while sharp tiles are being produced. At most a few milliseconds of rendering are spent per frame so
//!   the UI never freezes, and the texture cache is capped (LRU).
//! * Closing the reader drops the parsed document, hayro's caches and every texture (and asks for one more frame so the GPU
//!   copies are really released). While idle the tile cache shrinks to what is on screen.
//! * **Crisp at every zoom**: each page is snapped to whole physical pixels and tiles are rendered at the *exact* on-screen
//!   scale (1/4096 resolution) so one texel maps to one pixel — no resampling blur, also below 100 %.
//! * **Annotations** (`pdfnotes.rs`): highlight, underline, strike-through, sticky notes with comments, freehand pen, eraser;
//!   stored in the workspace per document and exportable as Markdown.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use egui::load::{BytesPoll, Bytes};
use egui::{
    pos2, vec2, Align2, Area, Color32, ColorImage, Context, CursorIcon, Id, Key, Order, Rect, ScrollArea, Sense, TextEdit,
    TextureHandle, TextureOptions, Ui, Vec2,
};
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::page::Page;
use hayro::hayro_syntax::LoadPdfError;
use hayro::hayro_syntax::Pdf;
use hayro::kurbo::Affine;
use hayro::vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};
use hayro::RenderSettings;

use crate::auth::now_ms;
use crate::icons;
use crate::lucide as l;
use crate::model::PdfAnnot;
use crate::pdfnotes::{self, Notes, Tool};
use crate::theme::{bold, body, Palette};
use crate::widgets::with_alpha;

pub fn looks_like_pdf(name: &str, url: &str) -> bool {
    let n = name.to_lowercase();
    let u = url.split(['?', '#']).next().unwrap_or("").to_lowercase();
    n.ends_with(".pdf") || u.ends_with(".pdf")
}

const TILE: u32 = 512;
const MARGIN: f32 = 16.0;
const GAP: f32 = 14.0;
const TOP_PAD: f32 = 14.0;
const TOOLBAR_H: f32 = 52.0;
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 6.0;
const THUMB_W: u32 = 240;
/// Render scale resolution: tiles are rendered at `qs / SCALE_Q` pixels per point (error < 0.01 % → no visible resampling).
const SCALE_Q: f32 = 4096.0;
/// Texture memory budget of the tile cache (bytes).
#[cfg(target_arch = "wasm32")]
const MAX_TILE_BYTES: usize = 40 << 20;
#[cfg(not(target_arch = "wasm32"))]
const MAX_TILE_BYTES: usize = 96 << 20;
/// After this long without input the cache is trimmed to the tiles that are on screen.
const IDLE_TRIM_MS: f64 = 2500.0;
const MAX_THUMBS: usize = 64;
/// Rendering budget per frame (milliseconds); at least one tile is always rendered.
const FRAME_BUDGET_MS: f64 = 12.0;

// ───────────────────────────── public request API ─────────────────────────────

pub enum Source {
    /// Bytes of an attachment stored in the workspace.
    Local(Arc<[u8]>),
    /// Linked PDF (downloaded through egui's HTTP loader; forgotten again on close).
    Remote(String),
}

/// "Open the reader for this document" — stored in the editor state, consumed by [`reader_ui`].
pub struct Request {
    pub key: u64,
    pub title: String,
    pub source: Option<Source>,
}

pub fn key_of(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

// ───────────────────────────── engine ─────────────────────────────

struct Buf(Arc<[u8]>);
impl AsRef<[u8]> for Buf {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Runs `f`, turning a panic inside the PDF library into `None` on native builds (WebAssembly aborts on panic).
fn guarded<T>(f: impl FnOnce() -> T) -> Option<T> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).ok()
    }
    #[cfg(target_arch = "wasm32")]
    {
        Some(f())
    }
}

enum OpenErr {
    Locked,
    Invalid(String),
}

/// A parsed document plus hayro's render cache.
///
/// SAFETY: `cache` borrows from `pdf` for `'static` (hayro ties both to one lifetime). `pdf` is boxed so its address never
/// changes, is never mutated, and the field order guarantees `cache` is dropped first.
struct Engine {
    cache: hayro::RenderCache<'static>,
    pdf: Box<Pdf>,
    dims: Arc<[(f32, f32)]>,
}

impl Engine {
    fn new(data: Arc<[u8]>, password: &str) -> Result<Engine, OpenErr> {
        let parsed = guarded(|| Pdf::new_with_password(Arc::new(Buf(data)), password)).ok_or_else(|| OpenErr::Invalid("The PDF library could not read this file.".into()))?;
        let pdf = match parsed {
            Ok(p) => Box::new(p),
            Err(LoadPdfError::Decryption(_)) => return Err(OpenErr::Locked),
            Err(_) => return Err(OpenErr::Invalid("This file is not a valid PDF (or is damaged).".into())),
        };
        let dims: Arc<[(f32, f32)]> = guarded(|| pdf.pages().iter().map(|p| p.render_dimensions()).map(|(w, h)| (w.max(1.0), h.max(1.0))).collect::<Vec<_>>())
            .map(Arc::from)
            .ok_or_else(|| OpenErr::Invalid("The page tree of this PDF is damaged.".into()))?;
        if dims.is_empty() {
            return Err(OpenErr::Invalid("This PDF has no pages.".into()));
        }
        Ok(Engine { cache: hayro::RenderCache::new(), pdf, dims })
    }

    fn page(&self, i: usize) -> Option<&'static Page<'static>> {
        let p = self.pdf.pages().get(i)?;
        // SAFETY: see the type documentation.
        Some(unsafe { std::mem::transmute::<&Page<'_>, &'static Page<'static>>(p) })
    }

    /// Rasterises the pixel rectangle `(x, y, w, h)` of page `i` rendered at `scale` pixels per PDF point.
    fn render(&self, i: usize, scale: f32, x: u32, y: u32, w: u32, h: u32, night: bool) -> Option<ColorImage> {
        let page = self.page(i)?;
        guarded(|| {
            let mut ctx = RenderContext::new(w as u16, h as u16);
            let transform = Affine::translate((-(x as f64), -(y as f64))) * Affine::scale(scale as f64) * page.initial_transform(true).to_kurbo();
            hayro::render_into(page, &self.cache, &InterpreterSettings::default(), &RenderSettings::default(), &mut ctx, transform);
            ctx.flush();
            let mut pix = Pixmap::new(ctx.width(), ctx.height());
            ctx.render_with(
                &mut pix,
                &mut Resources::default(),
                RasterizerSettings { target_init: TargetInit::Clear(hayro::vello_cpu::color::palette::css::WHITE), ..Default::default() },
            );
            let (iw, ih) = (pix.width() as usize, pix.height() as usize);
            let mut rgba = Vec::with_capacity(iw * ih * 4);
            for px in pix.data() {
                if night {
                    rgba.extend_from_slice(&[255 - px.r, 255 - px.g, 255 - px.b, 255]);
                } else {
                    rgba.extend_from_slice(&[px.r, px.g, px.b, px.a]);
                }
            }
            ColorImage::from_rgba_premultiplied([iw, ih], &rgba)
        })
    }
}

// ───────────────────────────── reader state ─────────────────────────────

enum Load {
    Fetch(String),
    /// Bytes are in memory, parsing starts on the next frame so the reader appears instantly.
    Opening(Arc<[u8]>),
    Locked { data: Arc<[u8]>, input: String, wrong: bool },
    Failed(String),
    Ready,
}

struct Tile {
    tex: TextureHandle,
    used: u64,
    bytes: usize,
}

/// (page, scale·64, tile x, tile y, night)
type TileKey = (usize, u32, u16, u16, bool);

struct Reader {
    key: u64,
    title: String,
    remote: Option<String>,
    load: Load,
    engine: Option<Engine>,
    zoom: f32,
    off: Vec2,
    vp: Rect,
    pending_off: Option<Vec2>,
    night: bool,
    thumbs: bool,
    tiles: HashMap<TileKey, Tile>,
    thumb_tex: HashMap<usize, Tile>,
    /// Most recent scale (·64) for which all visible tiles of a page were complete.
    best: HashMap<usize, u32>,
    failed: HashSet<(usize, u32)>,
    frame: u64,
    zoom_changed: f64,
    goto: String,
    page: usize,
    first_frame: bool,
    notes: Notes,
    /// Milliseconds it took to parse the document (shown in the title bar).
    open_ms: f64,
    /// Time of the last user input / render activity (for the idle trim).
    active_at: f64,
    trim_armed: bool,
}

thread_local! {
    static READER: RefCell<Option<Reader>> = const { RefCell::new(None) };
}

fn bytes_to_arc(b: Bytes) -> Arc<[u8]> {
    match b {
        Bytes::Shared(a) => a,
        Bytes::Static(s) => Arc::from(s),
    }
}

impl Reader {
    fn new(req: &mut Request, annots: Vec<PdfAnnot>) -> Reader {
        let (load, remote) = match req.source.take() {
            Some(Source::Remote(url)) => (Load::Fetch(url.clone()), Some(url)),
            Some(Source::Local(data)) => (Load::Opening(data), None),
            None => (Load::Failed("Nothing to show".into()), None),
        };
        let r = Reader {
            key: req.key,
            title: req.title.clone(),
            remote,
            load,
            engine: None,
            zoom: 1.0,
            off: Vec2::ZERO,
            vp: Rect::NOTHING,
            pending_off: None,
            night: false,
            thumbs: false,
            tiles: HashMap::new(),
            thumb_tex: HashMap::new(),
            best: HashMap::new(),
            failed: HashSet::new(),
            frame: 0,
            zoom_changed: 0.0,
            goto: String::new(),
            page: 0,
            first_frame: true,
            notes: Notes::new(annots),
            open_ms: 0.0,
            active_at: now_ms(),
            trim_armed: false,
        };
        r
    }

    fn try_open(&mut self, data: Arc<[u8]>, pw: &str) {
        let t0 = now_ms();
        match Engine::new(data.clone(), pw) {
            Ok(e) => {
                self.open_ms = now_ms() - t0;
                self.engine = Some(e);
                self.load = Load::Ready;
            }
            Err(OpenErr::Locked) => self.load = Load::Locked { data, input: String::new(), wrong: !pw.is_empty() },
            Err(OpenErr::Invalid(m)) => self.load = Load::Failed(m),
        }
    }

    fn mem_bytes(&self) -> usize {
        self.tiles.values().map(|t| t.bytes).sum::<usize>() + self.thumb_tex.values().map(|t| t.bytes).sum::<usize>()
    }
}

struct Lay {
    s: f32,
    ys: Vec<f32>,
    ws: Vec<f32>,
    hs: Vec<f32>,
    content_w: f32,
    content_h: f32,
}

fn layout(dims: &[(f32, f32)], zoom: f32, view_w: f32) -> Lay {
    let max_w = dims.iter().map(|d| d.0).fold(1.0, f32::max);
    let base = (view_w - 2.0 * MARGIN).max(80.0);
    let s = base * zoom / max_w;
    let mut ys = Vec::with_capacity(dims.len());
    let mut ws = Vec::with_capacity(dims.len());
    let mut hs = Vec::with_capacity(dims.len());
    let mut y = TOP_PAD;
    for (w, h) in dims {
        ys.push(y);
        ws.push(w * s);
        hs.push(h * s);
        y += h * s + GAP;
    }
    Lay { s, ys, ws, hs, content_w: (max_w * s + 2.0 * MARGIN).max(view_w), content_h: y + TOP_PAD }
}

fn page_at(lay: &Lay, y: f32) -> usize {
    lay.ys.partition_point(|py| *py <= y).saturating_sub(1).min(lay.ys.len().saturating_sub(1))
}

// ───────────────────────────── UI ─────────────────────────────

/// Draws the reader when a request is pending. Returns true while it is open (callers should hide web iframes etc.).
/// `annots` is the workspace's annotation store; changes made in the reader are written back immediately.
pub fn reader_ui(ctx: &Context, req: &mut Option<Request>, annots: &mut BTreeMap<u64, Vec<PdfAnnot>>, pal: &Palette) -> bool {
    let Some(r) = req.as_mut() else {
        close_reader(ctx);
        return false;
    };
    let keep = READER.with(|cell| {
        let mut g = cell.borrow_mut();
        if g.as_ref().map_or(true, |x| x.key != r.key) {
            if let Some(old) = g.take() {
                release(ctx, old);
            }
            *g = Some(Reader::new(r, annots.get(&r.key).cloned().unwrap_or_default()));
        }
        let rd = g.as_mut().expect("reader");
        let keep = draw(rd, ctx, pal);
        if rd.notes.dirty {
            rd.notes.dirty = false;
            if rd.notes.list.is_empty() {
                annots.remove(&rd.key);
            } else {
                annots.insert(rd.key, rd.notes.list.clone());
            }
        }
        keep
    });
    if !keep {
        *req = None;
        close_reader(ctx);
        return false;
    }
    true
}

/// Drops a reader: document, hayro caches, tile + preview textures and the remote download.
fn release(ctx: &Context, old: Reader) {
    if let Some(u) = &old.remote {
        ctx.forget_image(u);
    }
    drop(old);
    // textures are freed on the GPU when the next frame is finished
    ctx.request_repaint();
}

/// Releases the document, caches and textures.
pub fn close_reader(ctx: &Context) {
    let old = READER.with(|cell| cell.borrow_mut().take());
    if let Some(old) = old {
        release(ctx, old);
    }
}

fn tb_button(ui: &mut Ui, pal: &Palette, rect: Rect, id: &str, glyph: &str, tip: &str, enabled: bool, active: bool) -> bool {
    let resp = ui.interact(rect, Id::new(("pdf_tb", id)), if enabled { Sense::click() } else { Sense::hover() });
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() && enabled, 0.1);
    let bg = if active { with_alpha(pal.accent, 0.9) } else { with_alpha(pal.active, h) };
    ui.painter().rect_filled(rect, 8.0, bg);
    let col = if active { Color32::WHITE } else { with_alpha(pal.text, if enabled { 1.0 } else { 0.3 }) };
    if glyph.chars().count() <= 1 && !glyph.chars().next().map_or(false, |c| (0xE000..=0xF8FF).contains(&(c as u32))) {
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, bold(16.0), col);
    } else {
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, icons::font(16.0), col);
    }
    enabled && resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip).clicked()
}

fn draw(rd: &mut Reader, ctx: &Context, pal: &Palette) -> bool {
    rd.frame += 1;
    let screen = ctx.screen_rect();
    let narrow = screen.width() < 640.0;
    let tiny = screen.width() < 470.0;
    let mut keep = true;
    let mut zoom_req: Option<(f32, egui::Pos2)> = None;
    let mut goto_req: Option<usize> = None;
    let mut fit: Option<bool> = None;
    let mut scroll_by: Option<f32> = None;
    let editing = ctx.memory(|m| m.focused()).is_some();
    if ctx.input(|i| !i.events.is_empty() || i.pointer.velocity().length() > 0.0 || i.smooth_scroll_delta != Vec2::ZERO) {
        rd.active_at = now_ms();
        rd.trim_armed = false;
    }

    if let Load::Opening(d) = &rd.load {
        if rd.frame >= 2 {
            let d = d.clone();
            rd.try_open(d, "");
        }
        ctx.request_repaint();
    }

    if matches!(rd.load, Load::Fetch(_)) {
        if let Load::Fetch(uri) = &rd.load {
            let uri = uri.clone();
            match ctx.try_load_bytes(&uri) {
                Ok(BytesPoll::Ready { bytes, .. }) => rd.try_open(bytes_to_arc(bytes), ""),
                Ok(BytesPoll::Pending { .. }) => ctx.request_repaint(),
                Err(e) => rd.load = Load::Failed(format!("Could not load the PDF: {e}")),
            }
        }
    }

    let bg = if rd.night { Color32::from_rgb(24, 24, 26) } else { Color32::from_rgb(82, 86, 89) };
    Area::new(Id::new("pdf_reader")).order(Order::Foreground).fixed_pos(screen.min).constrain(false).show(ctx, |ui| {
        ui.set_clip_rect(screen);
        let (full, _) = ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
        ui.painter().rect_filled(full, 0.0, bg);
        let bar = Rect::from_min_size(full.min, vec2(full.width(), TOOLBAR_H));
        let body_rect = Rect::from_min_max(pos2(full.left(), bar.bottom()), full.max);

        // ── toolbar ──
        ui.painter().rect_filled(bar, 0.0, with_alpha(pal.bg, 0.97));
        ui.painter().hline(bar.x_range(), bar.bottom(), egui::Stroke::new(1.0_f32, pal.border));
        let n_pages = rd.engine.as_ref().map_or(0, |e| e.dims.len());
        let ready = rd.engine.is_some();
        let bsz = vec2(38.0, 36.0);
        let mut x = bar.left() + 8.0;
        let y = bar.top() + (TOOLBAR_H - bsz.y) / 2.0;
        let mut put = |x: &mut f32, w: f32| -> Rect {
            let r = Rect::from_min_size(pos2(*x, y), vec2(w, bsz.y));
            *x += w + 4.0;
            r
        };
        if tb_button(ui, pal, put(&mut x, bsz.x), "close", l::X, "Close  (Esc)", true, false) {
            keep = false;
        }
        if !narrow && ready && tb_button(ui, pal, put(&mut x, bsz.x), "thumbs", l::PANEL_LEFT, "Page thumbnails", true, rd.thumbs) {
            rd.thumbs = !rd.thumbs;
        }
        if ready && tb_button(ui, pal, put(&mut x, bsz.x), "annot", l::HIGHLIGHTER, "Annotate: highlight, underline, notes, pen", true, rd.notes.bar) {
            rd.notes.bar = !rd.notes.bar;
            if rd.notes.bar {
                rd.notes.panel = !narrow && !rd.notes.list.is_empty();
            } else {
                rd.notes.tool = Tool::Hand;
                rd.notes.panel = false;
            }
        }
        // right-hand side first (so the title can use the remaining space)
        let mut rx = bar.right() - 8.0;
        let mut put_r = |rx: &mut f32, w: f32| -> Rect {
            *rx -= w;
            let r = Rect::from_min_size(pos2(*rx, y), vec2(w, bsz.y));
            *rx -= 4.0;
            r
        };
        if ready {
            if !tiny && tb_button(ui, pal, put_r(&mut rx, bsz.x), "night", if rd.night { l::SUN } else { l::MOON }, "Night mode", true, rd.night) {
                rd.night = !rd.night;
                rd.tiles.clear();
                rd.thumb_tex.clear();
                rd.best.clear();
            }
            if !narrow {
                if tb_button(ui, pal, put_r(&mut rx, bsz.x), "fitp", l::MAXIMIZE, "Fit page", true, false) {
                    fit = Some(false);
                }
                if tb_button(ui, pal, put_r(&mut rx, bsz.x), "fitw", l::MOVE_HORIZONTAL, "Fit width", true, false) {
                    fit = Some(true);
                }
            }
            let anchor = pos2(body_rect.center().x, body_rect.top() + body_rect.height() * 0.4);
            if !tiny && tb_button(ui, pal, put_r(&mut rx, bsz.x), "zin", l::ZOOM_IN, "Zoom in  (Ctrl +)", rd.zoom < MAX_ZOOM - 0.01, false) {
                zoom_req = Some((rd.zoom * 1.25, anchor));
            }
            let lr = put_r(&mut rx, 54.0);
            ui.painter().text(lr.center(), Align2::CENTER_CENTER, format!("{:.0}%", rd.zoom * 100.0), body(13.5), pal.text);
            if ui.interact(lr, Id::new("pdf_zoom_lbl"), Sense::click()).on_hover_text("Click to reset to 100 %").clicked() {
                fit = Some(true);
            }
            if !tiny && tb_button(ui, pal, put_r(&mut rx, bsz.x), "zout", l::ZOOM_OUT, "Zoom out  (Ctrl −)", rd.zoom > MIN_ZOOM + 0.01, false) {
                zoom_req = Some((rd.zoom / 1.25, anchor));
            }
            // page controls
            if tb_button(ui, pal, put_r(&mut rx, bsz.x), "next", l::CHEVRON_DOWN, "Next page", rd.page + 1 < n_pages, false) {
                goto_req = Some(rd.page + 1);
            }
            let tot = put_r(&mut rx, if narrow { 34.0 } else { 44.0 });
            ui.painter().text(tot.center(), Align2::CENTER_CENTER, format!("/ {n_pages}"), body(13.0), pal.muted);
            let gr = put_r(&mut rx, 46.0);
            let gid = Id::new("pdf_goto");
            let focused = ctx.memory(|m| m.has_focus(gid));
            if !focused {
                rd.goto = (rd.page + 1).to_string();
            }
            let resp = ui.put(gr.shrink2(vec2(0.0, 4.0)), TextEdit::singleline(&mut rd.goto).id(gid).desired_width(gr.width()).horizontal_align(egui::Align::Center).font(body(14.0)));
            if resp.lost_focus() && ctx.input(|i| i.key_pressed(Key::Enter)) {
                if let Ok(n) = rd.goto.trim().parse::<usize>() {
                    goto_req = Some(n.clamp(1, n_pages.max(1)) - 1);
                }
            }
            if tb_button(ui, pal, put_r(&mut rx, bsz.x), "prev", l::CHEVRON_UP, "Previous page", rd.page > 0, false) {
                goto_req = Some(rd.page.saturating_sub(1));
            }
        }
        if !narrow {
            let avail = (rx - x - 12.0).max(40.0);
            let mem = rd.mem_bytes();
            let mut title = rd.title.clone();
            if rd.engine.is_some() {
                title.push_str(&format!("   ·   opened in {:.0} ms", rd.open_ms));
            }
            if mem > 0 {
                title.push_str(&format!("   ·   {:.0} MB cache", mem as f64 / 1_048_576.0));
            }
            crate::widgets::text_fit(ui.painter(), pos2(x + 8.0, bar.center().y), Align2::LEFT_CENTER, &title, bold(14.0), pal.text, avail);
        }

        // ── body ──
        match &mut rd.load {
            Load::Fetch(_) => {
                ui.painter().text(body_rect.center(), Align2::CENTER_CENTER, "Loading PDF…", body(16.0), Color32::from_gray(220));
                ctx.request_repaint();
            }
            Load::Opening(_) => {
                ui.painter().text(body_rect.center(), Align2::CENTER_CENTER, "Opening…", body(16.0), Color32::from_gray(220));
            }
            Load::Failed(m) => {
                ui.painter().text(body_rect.center() - vec2(0.0, 12.0), Align2::CENTER_CENTER, "⚠  Could not open this PDF", bold(18.0), Color32::from_gray(235));
                crate::widgets::text_fit(ui.painter(), body_rect.center() + vec2(0.0, 18.0), Align2::CENTER_CENTER, m, body(14.0), Color32::from_gray(190), body_rect.width() - 40.0);
            }
            Load::Locked { data, input, wrong } => {
                let card = Rect::from_center_size(body_rect.center(), vec2(340.0f32.min(body_rect.width() - 24.0), 168.0));
                ui.painter().rect_filled(card, 12.0, pal.bg);
                ui.painter().text(pos2(card.center().x, card.top() + 26.0), Align2::CENTER_CENTER, "This PDF is password protected", bold(15.0), pal.text);
                let field = Rect::from_min_size(pos2(card.left() + 20.0, card.top() + 54.0), vec2(card.width() - 40.0, 34.0));
                let resp = ui.put(field, TextEdit::singleline(input).password(true).hint_text("Password").desired_width(field.width()).font(body(15.0)));
                if !resp.has_focus() && input.is_empty() {
                    resp.request_focus();
                }
                if *wrong {
                    ui.painter().text(pos2(card.center().x, card.top() + 104.0), Align2::CENTER_CENTER, "Wrong password — try again", body(12.5), pal.danger);
                }
                let br = Rect::from_center_size(pos2(card.center().x, card.bottom() - 28.0), vec2(120.0, 34.0));
                ui.painter().rect_filled(br, 8.0, pal.accent);
                ui.painter().text(br.center(), Align2::CENTER_CENTER, "Unlock", bold(14.0), Color32::WHITE);
                let click = ui.interact(br, Id::new("pdf_unlock"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand).clicked();
                if click || (resp.lost_focus() && ctx.input(|i| i.key_pressed(Key::Enter))) {
                    let (d, pw) = (data.clone(), input.clone());
                    rd.try_open(d, &pw);
                }
            }
            Load::Ready => {
                let bar_h = if rd.notes.bar { pdfnotes::bar_height(narrow) } else { 0.0 };
                if rd.notes.bar {
                    pdfnotes::bar(ui, pal, Rect::from_min_size(body_rect.min, vec2(body_rect.width(), bar_h)), &mut rd.notes, narrow);
                }
                let area = Rect::from_min_max(pos2(body_rect.left(), body_rect.top() + bar_h), body_rect.max);
                let side = if rd.thumbs && !narrow { 132.0 } else { 0.0 };
                let panel_w = if rd.notes.panel && rd.notes.bar { if narrow { area.width() } else { 320.0f32.min(area.width() * 0.45) } } else { 0.0 };
                let view = Rect::from_min_max(pos2(area.left() + side, area.top()), pos2(if narrow { area.right() } else { area.right() - panel_w }, area.bottom()));
                if side > 0.0 {
                    thumbs_panel(ui, rd, ctx, pal, Rect::from_min_max(area.min, pos2(area.left() + side, area.bottom())), &mut goto_req);
                }
                // on phones the notes list replaces the page while it is open
                let list_only = narrow && panel_w > 0.0;
                if !list_only {
                    reading_view(ui, rd, ctx, view, editing, pal, &mut zoom_req, &mut goto_req, &mut fit, &mut scroll_by, &mut keep);
                }
                if panel_w > 0.0 {
                    let pr = Rect::from_min_max(pos2(area.right() - panel_w, area.top()), area.max);
                    let title = rd.title.clone();
                    pdfnotes::panel(ui, ctx, pal, pr, &mut rd.notes, &title, &mut goto_req);
                }
            }
        }
    });

    // ── apply zoom / navigation requests (computed above) ──
    if let Some(e) = rd.engine.as_ref() {
        let vw = rd.vp.width().max(200.0);
        let vh = rd.vp.height().max(200.0);
        if let Some(f) = fit {
            let max_w = e.dims.iter().map(|d| d.0).fold(1.0, f32::max);
            let target = if f {
                1.0
            } else {
                let (pw, ph) = e.dims[rd.page.min(e.dims.len() - 1)];
                let s_fit = (vh - 2.0 * GAP) / ph;
                (s_fit * max_w / (vw - 2.0 * MARGIN)).min((vw - 2.0 * MARGIN) / pw * max_w / (vw - 2.0 * MARGIN))
            };
            zoom_req = Some((target, pos2(rd.vp.center().x, rd.vp.top() + 4.0)));
            if !f {
                goto_req = Some(rd.page);
            }
        }
        if let Some((nz, anchor)) = zoom_req {
            let nz = nz.clamp(MIN_ZOOM, MAX_ZOOM);
            if (nz - rd.zoom).abs() > 1e-4 {
                let old = layout(&e.dims, rd.zoom, vw);
                let new = layout(&e.dims, nz, vw);
                let rel = anchor - rd.vp.min;
                let ay = rd.off.y + rel.y;
                let i = page_at(&old, ay);
                let local = ((ay - old.ys[i]) / old.s).clamp(-GAP, e.dims[i].1 + GAP);
                let ny = new.ys[i] + local * new.s - rel.y;
                let dx = rd.off.x + rel.x - old.content_w / 2.0;
                let nx = new.content_w / 2.0 + dx / old.s * new.s - rel.x;
                rd.zoom = nz;
                rd.pending_off = Some(vec2(nx.max(0.0), ny.max(0.0)));
                rd.zoom_changed = now_ms();
            }
        }
        if let Some(p) = goto_req {
            let lay = layout(&e.dims, rd.zoom, vw);
            let p = p.min(e.dims.len() - 1);
            rd.pending_off = Some(vec2(rd.pending_off.map_or(rd.off.x, |o| o.x), (lay.ys[p] - 8.0).max(0.0)));
        }
        if let Some(dy) = scroll_by {
            let base = rd.pending_off.unwrap_or(rd.off);
            rd.pending_off = Some(vec2(base.x, (base.y + dy).max(0.0)));
        }
    }
    keep
}

fn thumbs_panel(ui: &mut Ui, rd: &mut Reader, ctx: &Context, pal: &Palette, rect: Rect, goto_req: &mut Option<usize>) {
    let Some(e) = rd.engine.as_ref() else { return };
    ui.painter().rect_filled(rect, 0.0, with_alpha(pal.bg, 0.96));
    ui.painter().vline(rect.right(), rect.y_range(), egui::Stroke::new(1.0_f32, pal.border));
    let n = e.dims.len();
    let tw = rect.width() - 28.0;
    let row_h = (tw * e.dims[0].1 / e.dims[0].0).clamp(60.0, 220.0) + 30.0;
    let mut want: Vec<usize> = vec![];
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    let frame = rd.frame;
    ScrollArea::vertical().id_salt("pdf_thumbs").auto_shrink([false, false]).show_rows(&mut child, row_h, n, |ui, range| {
        for i in range {
            let (r, resp) = ui.allocate_exact_size(vec2(rect.width() - 8.0, row_h), Sense::click());
            let (pw, ph) = e.dims[i];
            let h = tw * ph / pw;
            let pr = Rect::from_center_size(pos2(r.center().x, r.top() + 8.0 + h.min(row_h - 30.0) / 2.0), vec2(h.min(row_h - 30.0) * pw / ph, h.min(row_h - 30.0)));
            let cur = i == rd.page;
            if cur {
                ui.painter().rect_filled(pr.expand(4.0), 4.0, with_alpha(pal.accent, 0.5));
            }
            ui.painter().rect_filled(pr, 1.0, if rd.night { Color32::from_gray(30) } else { Color32::WHITE });
            if let Some(t) = rd.thumb_tex.get_mut(&i) {
                t.used = frame;
                ui.painter().image(t.tex.id(), pr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            } else {
                want.push(i);
            }
            ui.painter().text(pos2(r.center().x, r.bottom() - 10.0), Align2::CENTER_CENTER, (i + 1).to_string(), body(12.0), if cur { pal.text } else { pal.muted });
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                *goto_req = Some(i);
            }
        }
    });
    if let Some(i) = want.first().copied() {
        make_thumb(rd, ctx, i);
        ctx.request_repaint();
    }
}

fn make_thumb(rd: &mut Reader, ctx: &Context, i: usize) {
    let Some(e) = rd.engine.as_ref() else { return };
    let (pw, ph) = e.dims[i];
    let scale = THUMB_W as f32 / pw;
    let (w, h) = (THUMB_W, ((ph * scale).ceil() as u32).clamp(8, 1200));
    let key = (i, 0);
    if rd.failed.contains(&key) {
        return;
    }
    match e.render(i, scale, 0, 0, w, h, rd.night) {
        Some(img) => {
            let bytes = img.pixels.len() * 4;
            let tex = ctx.load_texture(format!("pdf-th-{}-{i}", rd.key), img, TextureOptions::LINEAR);
            rd.thumb_tex.insert(i, Tile { tex, used: rd.frame, bytes });
            if rd.thumb_tex.len() > MAX_THUMBS {
                let frame = rd.frame;
                let mut v: Vec<(usize, u64)> = rd.thumb_tex.iter().filter(|(_, t)| t.used + 2 < frame).map(|(k, t)| (*k, t.used)).collect();
                v.sort_by_key(|x| x.1);
                for (k, _) in v.into_iter().take(rd.thumb_tex.len() - MAX_THUMBS) {
                    rd.thumb_tex.remove(&k);
                }
            }
        }
        None => {
            rd.failed.insert(key);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn reading_view(
    ui: &mut Ui,
    rd: &mut Reader,
    ctx: &Context,
    view: Rect,
    editing: bool,
    pal: &Palette,
    zoom_req: &mut Option<(f32, egui::Pos2)>,
    goto_req: &mut Option<usize>,
    fit: &mut Option<bool>,
    scroll_by: &mut Option<f32>,
    keep: &mut bool,
) {
    let ppp = ctx.pixels_per_point();
    let hovered = ctx.pointer_hover_pos().map_or(false, |p| view.contains(p));

    // input: pinch / Ctrl+wheel zoom, keys
    if hovered {
        let zd = ctx.input(|i| i.zoom_delta());
        if (zd - 1.0).abs() > 1e-4 {
            let anchor = ctx.input(|i| i.multi_touch().map(|t| t.start_pos)).or_else(|| ctx.pointer_hover_pos()).unwrap_or(view.center());
            *zoom_req = Some((zoom_req.map_or(rd.zoom, |z| z.0) * zd, anchor));
        }
    }
    if !editing {
        let anchor = pos2(view.center().x, view.top() + view.height() * 0.4);
        let vh = view.height();
        let (pgdn, pgup, home, end, down, up, zin, zout, z0, esc, space, shift) = ctx.input(|i| {
            (
                i.key_pressed(Key::PageDown),
                i.key_pressed(Key::PageUp),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::ArrowUp),
                i.modifiers.command && (i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals)),
                i.modifiers.command && i.key_pressed(Key::Minus),
                i.modifiers.command && i.key_pressed(Key::Num0),
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::Space),
                i.modifiers.shift,
            )
        });
        let esc_used = rd.notes.keys(ctx, editing);
        if esc && !esc_used {
            *keep = false;
        }
        if pgdn || (space && !shift) {
            *scroll_by = Some(vh * 0.9);
        } else if pgup || (space && shift) {
            *scroll_by = Some(-vh * 0.9);
        } else if down {
            *scroll_by = Some(70.0);
        } else if up {
            *scroll_by = Some(-70.0);
        }
        if home {
            *goto_req = Some(0);
        }
        if end {
            *goto_req = Some(rd.engine.as_ref().map_or(0, |e| e.dims.len().saturating_sub(1)));
        }
        if zin {
            *zoom_req = Some((rd.zoom * 1.2, anchor));
        }
        if zout {
            *zoom_req = Some((rd.zoom / 1.2, anchor));
        }
        if z0 {
            *fit = Some(true);
        }
    }

    let Some(engine) = rd.engine.as_ref() else { return };
    let dims = engine.dims.clone();
    let lay = layout(&dims, rd.zoom, view.width());
    let night = rd.night;
    let frame = rd.frame;
    let now = now_ms();
    let debounce = now - rd.zoom_changed < 160.0;

    let mut sa = ScrollArea::both().id_salt("pdf_scroll").auto_shrink([false, false]).drag_to_scroll(!rd.notes.capturing());
    if let Some(o) = rd.pending_off.take() {
        sa = sa.scroll_offset(o);
    }
    if rd.first_frame {
        rd.first_frame = false;
    }
    let mut want: Vec<(f32, TileKey, usize, f32, u32, u32, u32, u32)> = vec![];
    let mut want_thumb: Vec<usize> = vec![];
    let mut completed: Vec<(usize, u32)> = vec![];
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(view).layout(egui::Layout::top_down(egui::Align::Min)));
    child.set_clip_rect(view.intersect(ui.clip_rect()));
    let out = sa.show_viewport(&mut child, |ui, viewport| {
        let capturing = rd.notes.capturing();
        let (content, resp) = ui.allocate_exact_size(vec2(lay.content_w, lay.content_h), if capturing { Sense::click_and_drag() } else { Sense::click() });
        if !capturing && resp.double_clicked() {
            *zoom_req = Some((if rd.zoom < 1.6 { 2.0 } else { 1.0 }, resp.interact_pointer_pos().unwrap_or(view.center())));
        }
        let p = ui.painter().clone();
        let vp_top = viewport.top() - 300.0;
        let vp_bot = viewport.bottom() + 300.0;
        let first = page_at(&lay, vp_top);
        let center_y = viewport.center().y;
        let cur = page_at(&lay, center_y);
        rd.page = cur;
        let rs = (lay.s * ppp).clamp(0.05, 12.0);
        let qs = ((rs * SCALE_Q).round() as u32).max(1);
        let rs_q = qs as f32 / SCALE_Q;
        let view_center = viewport.center();
        let snap = |v: f32| (v * ppp).round() / ppp;
        let accent = pal.accent;
        let mut shown: Vec<(usize, Rect)> = vec![];
        for i in first..dims.len() {
            if lay.ys[i] > vp_bot {
                break;
            }
            // The page is snapped to whole physical pixels and has exactly `wp × hp` of them, so every texel of a tile lands on
            // one screen pixel (no bilinear resampling = no blur, at any zoom level).
            let (wp, hp) = (((dims[i].0 * rs_q).round() as u32).max(1), ((dims[i].1 * rs_q).round() as u32).max(1));
            let x = (content.width() - lay.ws[i]) / 2.0;
            let origin = pos2(snap(content.min.x + x), snap(content.min.y + lay.ys[i]));
            let prect = Rect::from_min_size(origin, vec2(wp as f32 / ppp, hp as f32 / ppp));
            p.rect_filled(prect.translate(vec2(0.0, 2.0)).expand(1.0), 2.0, Color32::from_black_alpha(70));
            p.rect_filled(prect, 0.0, if night { Color32::from_gray(30) } else { Color32::WHITE });
            let page_vp = Rect::from_min_max(content.min + vec2(0.0, vp_top), content.min + vec2(content.width(), vp_bot));
            let vis = prect.intersect(page_vp);
            if vis.width() <= 0.0 || vis.height() <= 0.0 {
                continue;
            }
            shown.push((i, prect));
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            // 1. low-res preview
            if let Some(t) = rd.thumb_tex.get_mut(&i) {
                t.used = frame;
                p.image(t.tex.id(), prect, uv, Color32::WHITE);
            } else if prect.intersects(Rect::from_min_max(content.min + vec2(0.0, viewport.top()), content.min + vec2(content.width(), viewport.bottom()))) {
                want_thumb.push(i);
            }
            // 2. tiles of the previously completed zoom level (stretched) while sharp ones render
            let best = rd.best.get(&i).copied();
            if let Some(bq) = best.filter(|b| *b != qs) {
                let brs = bq as f32 / SCALE_Q;
                let f = rs_q / brs / ppp; // screen points per old pixel
                for (k, t) in rd.tiles.iter_mut().filter(|(k, _)| k.0 == i && k.1 == bq && k.4 == night) {
                    t.used = frame;
                    let (x0, y0) = (k.2 as f32 * TILE as f32 * f, k.3 as f32 * TILE as f32 * f);
                    let [tw, th] = t.tex.size();
                    let r = Rect::from_min_size(prect.min + vec2(x0, y0), vec2(tw as f32 * f, th as f32 * f));
                    p.image(t.tex.id(), r, uv, Color32::WHITE);
                }
            }
            // 3. tiles at the current scale
            let rx0 = ((vis.min.x - prect.min.x) * ppp).max(0.0);
            let rx1 = ((vis.max.x - prect.min.x) * ppp).min(wp as f32);
            let ry0 = ((vis.min.y - prect.min.y) * ppp).max(0.0);
            let ry1 = ((vis.max.y - prect.min.y) * ppp).min(hp as f32);
            if rx1 <= rx0 || ry1 <= ry0 {
                continue;
            }
            let (tx0, tx1) = ((rx0 / TILE as f32) as u32, ((rx1 - 1.0).max(0.0) / TILE as f32) as u32);
            let (ty0, ty1) = ((ry0 / TILE as f32) as u32, ((ry1 - 1.0).max(0.0) / TILE as f32) as u32);
            let mut total = 0;
            let mut have = 0;
            for ty in ty0..=ty1 {
                for tx in tx0..=tx1 {
                    let (px, py) = (tx * TILE, ty * TILE);
                    if px >= wp || py >= hp {
                        continue;
                    }
                    total += 1;
                    let key: TileKey = (i, qs, tx as u16, ty as u16, night);
                    let (tw, th) = (TILE.min(wp - px), TILE.min(hp - py));
                    let r = Rect::from_min_size(prect.min + vec2(px as f32 / ppp, py as f32 / ppp), vec2(tw as f32 / ppp, th as f32 / ppp));
                    if let Some(t) = rd.tiles.get_mut(&key) {
                        t.used = frame;
                        have += 1;
                        p.image(t.tex.id(), r, uv, Color32::WHITE);
                    } else if !rd.failed.contains(&(i, qs)) && (!debounce || best.is_none()) {
                        let d = (r.center() - view_center).length();
                        want.push((d, key, i, rs_q, px, py, tw, th));
                    }
                }
            }
            if have == total && total > 0 {
                completed.push((i, qs));
            }
            if rd.failed.contains(&(i, qs)) {
                p.text(prect.center(), Align2::CENTER_CENTER, "This page could not be rendered", body(14.0), Color32::GRAY);
            }
            // 4. annotations on top of the page
            rd.notes.paint_page(&p, prect, i, night, accent);
        }
        if capturing || rd.notes.bar || !rd.notes.list.is_empty() {
            rd.notes.input(ctx, &resp, &shown);
        }
    });
    rd.off = out.state.offset;
    rd.vp = out.inner_rect;

    // retire stale scales of completed pages
    for (i, qs) in completed {
        rd.best.insert(i, qs);
        let stale: Vec<TileKey> = rd.tiles.keys().filter(|k| k.0 == i && k.1 != qs).copied().collect();
        for k in stale {
            rd.tiles.remove(&k);
        }
    }

    // render within the frame budget (nearest tiles first, then previews)
    want.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let t0 = now_ms();
    let mut left = want.len();
    if !want.is_empty() {
        rd.active_at = now_ms();
        rd.trim_armed = false;
    }
    for (_, key, i, rs_q, px, py, tw, th) in want {
        if let Some(e) = rd.engine.as_ref() {
            match e.render(i, rs_q, px, py, tw, th, night) {
                Some(img) => {
                    let bytes = img.pixels.len() * 4;
                    let tex = ctx.load_texture(format!("pdf-{}-{i}-{}-{}", rd.key, key.2, key.3), img, TextureOptions::LINEAR);
                    rd.tiles.insert(key, Tile { tex, used: frame, bytes });
                }
                None => {
                    rd.failed.insert((i, key.1));
                }
            }
        }
        left -= 1;
        if now_ms() - t0 > FRAME_BUDGET_MS {
            break;
        }
    }
    if left == 0 && now_ms() - t0 < FRAME_BUDGET_MS {
        if let Some(i) = want_thumb.first().copied() {
            make_thumb(rd, ctx, i);
            left = want_thumb.len().saturating_sub(1);
        }
    }
    if left > 0 || debounce {
        ctx.request_repaint();
    }

    // LRU eviction by memory budget (never touches what was drawn this frame)
    let mut mem: usize = rd.tiles.values().map(|t| t.bytes).sum();
    if mem > MAX_TILE_BYTES {
        let mut v: Vec<(TileKey, u64, usize)> = rd.tiles.iter().filter(|(_, t)| t.used + 1 < frame).map(|(k, t)| (*k, t.used, t.bytes)).collect();
        v.sort_by_key(|x| x.1);
        for (k, _, b) in v {
            if mem <= MAX_TILE_BYTES {
                break;
            }
            rd.tiles.remove(&k);
            mem = mem.saturating_sub(b);
        }
    }
    // idle trim: a few seconds after the last input only what is on screen stays in memory
    let idle = now_ms() - rd.active_at;
    if idle >= IDLE_TRIM_MS && left == 0 {
        let before = rd.tiles.len() + rd.thumb_tex.len();
        rd.tiles.retain(|_, t| t.used + 1 >= frame);
        rd.thumb_tex.retain(|_, t| t.used + 1 >= frame || rd.thumbs);
        if rd.tiles.len() + rd.thumb_tex.len() != before {
            let tiles = &rd.tiles;
            rd.best.retain(|i, q| tiles.keys().any(|k| k.0 == *i && k.1 == *q));
            rd.failed.clear();
        }
        rd.trim_armed = true;
    } else if !rd.trim_armed && left == 0 {
        ctx.request_repaint_after(Duration::from_millis(IDLE_TRIM_MS as u64 + 250));
        rd.trim_armed = true;
    }
    // pages far away lose their previews / sharp tiles (keeps memory flat in long documents)
    if frame % 30 == 0 {
        let cur = rd.page;
        let far = |i: usize| i + 8 < cur || i > cur + 8;
        rd.thumb_tex.retain(|i, _| !far(*i) || rd.thumbs);
        rd.tiles.retain(|k, t| !far(k.0) || t.used + 120 > frame);
    }
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Smallest valid one-page PDF with some text.
    fn tiny_pdf() -> Vec<u8> {
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
            {
                let c = "BT /F1 24 Tf 20 40 Td (Hello PDF) Tj ET";
                format!("<< /Length {} >>\nstream\n{}\nendstream", c.len(), c)
            },
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
    fn tiles_render_and_match_full_page() {
        let data: Arc<[u8]> = tiny_pdf().into();
        let Ok(e) = Engine::new(data, "") else { panic!("pdf did not open") };
        assert_eq!(e.dims.len(), 1);
        assert_eq!(e.dims[0], (200.0, 100.0));
        // whole page at 2 px/pt and the same page as two 200×200 tiles must agree
        let full = e.render(0, 2.0, 0, 0, 400, 200, false).expect("render");
        assert_eq!(full.size, [400, 200]);
        assert!(full.pixels.iter().any(|p| p.r() < 100), "text pixels expected");
        let left = e.render(0, 2.0, 0, 0, 200, 200, false).expect("left tile");
        let right = e.render(0, 2.0, 200, 0, 200, 200, false).expect("right tile");
        let mismatches = (0..200usize)
            .flat_map(|y| (0..200usize).map(move |x| (x, y)))
            .filter(|(x, y)| {
                let a = full.pixels[y * 400 + x].r() as i32;
                let b = left.pixels[y * 200 + x].r() as i32;
                let c = full.pixels[y * 400 + x + 200].r() as i32;
                let d = right.pixels[y * 200 + x].r() as i32;
                (a - b).abs() > 24 || (c - d).abs() > 24
            })
            .count();
        assert!(mismatches < 40, "tiles must line up with the full render ({mismatches} differing pixels)");
        let night = e.render(0, 1.0, 0, 0, 200, 100, true).expect("night");
        assert!(night.pixels.iter().any(|p| p.r() > 200) && night.pixels[0].r() < 20, "night mode inverts");
    }

    #[test]
    fn rejects_garbage_without_crashing() {
        assert!(matches!(Engine::new(Arc::from(&b"definitely not a pdf"[..]), ""), Err(OpenErr::Invalid(_))));
        assert!(looks_like_pdf("a.PDF", "") && looks_like_pdf("x", "https://a/b.pdf?dl=1") && !looks_like_pdf("a.png", ""));
    }

    #[test]
    fn layout_is_monotonic_and_anchorable() {
        let dims = vec![(612.0, 792.0); 30];
        let a = layout(&dims, 1.0, 900.0);
        let b = layout(&dims, 2.0, 900.0);
        assert!(b.s > a.s * 1.99 && b.content_h > a.content_h);
        assert_eq!(page_at(&a, a.ys[7] + 5.0), 7);
        assert_eq!(page_at(&a, 0.0), 0);
        assert_eq!(page_at(&a, 1e9), 29);
    }

    fn run(ctx: &Context, req: &mut Option<Request>, annots: &mut BTreeMap<u64, Vec<PdfAnnot>>, t: &mut f64, size: Vec2, evs: Vec<egui::Event>) {
        *t += 0.05;
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), size));
        input.time = Some(*t);
        input.events = evs;
        let pal = crate::theme::Palette::light();
        let _ = ctx.run(input, |ctx| {
            reader_ui(ctx, req, annots, &pal);
        });
    }

    #[test]
    fn reader_is_pixel_exact_annotates_and_cleans_up() {
        let ctx = Context::default();
        crate::theme::setup_fonts(&ctx);
        let mut req = Some(Request { key: 7, title: "t.pdf".into(), source: Some(Source::Local(tiny_pdf().into())) });
        let mut annots = BTreeMap::new();
        let mut t = 0.0;
        for size in [vec2(1280.0, 800.0), vec2(360.0, 640.0)] {
            for zoom in [0.25_f32, 0.5, 0.8, 1.0, 3.0] {
                READER.with(|c| {
                    if let Some(r) = c.borrow_mut().as_mut() {
                        r.zoom = zoom;
                        r.zoom_changed = 0.0;
                    }
                });
                for _ in 0..8 {
                    run(&ctx, &mut req, &mut annots, &mut t, size, vec![]);
                }
                READER.with(|c| {
                    let g = c.borrow();
                    let r = g.as_ref().expect("reader is open");
                    assert!(r.engine.is_some(), "the document opened");
                    assert!(!r.tiles.is_empty(), "tiles rendered at {zoom} on {size:?}");
                    for (k, tile) in r.tiles.iter().filter(|(k, _)| k.2 == 0 && k.3 == 0) {
                        let rs = k.1 as f32 / SCALE_Q;
                        let (wp, hp) = (((200.0 * rs).round() as usize).max(1), ((100.0 * rs).round() as usize).max(1));
                        assert_eq!(tile.tex.size(), [wp.min(TILE as usize), hp.min(TILE as usize)], "tile is exactly the page's pixel size");
                    }
                });
            }
        }
        // annotate: arm the highlighter and drag across the page
        READER.with(|c| {
            let mut g = c.borrow_mut();
            let r = g.as_mut().unwrap();
            r.zoom = 1.0;
            r.notes.bar = true;
            r.notes.tool = Tool::Highlight;
        });
        let size = vec2(1280.0, 800.0);
        for _ in 0..4 {
            run(&ctx, &mut req, &mut annots, &mut t, size, vec![]);
        }
        let (a, b) = (pos2(500.0, 330.0), pos2(780.0, 335.0));
        let btn = |p, pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        run(&ctx, &mut req, &mut annots, &mut t, size, vec![egui::Event::PointerMoved(a)]);
        run(&ctx, &mut req, &mut annots, &mut t, size, vec![btn(a, true)]);
        run(&ctx, &mut req, &mut annots, &mut t, size, vec![egui::Event::PointerMoved(pos2(600.0, 332.0))]);
        run(&ctx, &mut req, &mut annots, &mut t, size, vec![egui::Event::PointerMoved(b)]);
        run(&ctx, &mut req, &mut annots, &mut t, size, vec![btn(b, false)]);
        for _ in 0..3 {
            run(&ctx, &mut req, &mut annots, &mut t, size, vec![]);
        }
        assert_eq!(annots.get(&7).map(|v| v.len()), Some(1), "the highlight was stored in the workspace store");
        // close: everything is released
        req = None;
        run(&ctx, &mut req, &mut annots, &mut t, size, vec![]);
        assert!(READER.with(|c| c.borrow().is_none()), "reader state is dropped on close");
        // re-open: annotations come back
        req = Some(Request { key: 7, title: "t.pdf".into(), source: Some(Source::Local(tiny_pdf().into())) });
        for _ in 0..4 {
            run(&ctx, &mut req, &mut annots, &mut t, size, vec![]);
        }
        assert_eq!(READER.with(|c| c.borrow().as_ref().map(|r| r.notes.list.len())), Some(1));
    }
}

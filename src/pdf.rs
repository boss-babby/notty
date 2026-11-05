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
    pos2, Pos2, vec2, Align2, Area, Color32, ColorImage, Context, CursorIcon, Id, Key, Order, Rect, ScrollArea, Sense, TextEdit,
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
use crate::model::AnnotKind;
use crate::pdfnotes::{self, Notes, Tool};
use crate::pdftext::{self, PageText, Sel};
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
/// Pages whose text layer stays cached.
const MAX_TEXT_PAGES: usize = 48;
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
    /// Interpreter cache used by the text layer (fonts are parsed once per document).
    tcache: hayro::hayro_interpret::InterpreterCache<'static>,
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
        Ok(Engine { cache: hayro::RenderCache::new(), tcache: hayro::hayro_interpret::InterpreterCache::new(), pdf, dims })
    }

    fn page(&self, i: usize) -> Option<&'static Page<'static>> {
        let p = self.pdf.pages().get(i)?;
        // SAFETY: see the type documentation.
        Some(unsafe { std::mem::transmute::<&Page<'_>, &'static Page<'static>>(p) })
    }

    /// Reads the text layer of page `i` (no rasterisation).
    fn extract(&self, i: usize) -> Option<PageText> {
        let page = self.page(i)?;
        guarded(|| pdftext::extract(page, &self.tcache))
    }

    /// Rasterises the pixel rectangle `(x, y, w, h)` of page `i` rendered at `scale` pixels per PDF point.
    fn render(&self, i: usize, scale: f32, x: u32, y: u32, w: u32, h: u32, night: bool) -> Option<ColorImage> {
        self.render_marked(i, scale, x, y, w, h, night, &[])
    }

    /// Like [`Engine::render`], with the page's highlights multiplied into the pixels (see `pdfnotes::bake`).
    #[allow(clippy::too_many_arguments)]
    fn render_marked(&self, i: usize, scale: f32, x: u32, y: u32, w: u32, h: u32, night: bool, marks: &[pdfnotes::Mark]) -> Option<ColorImage> {
        let page = self.page(i)?;
        let page_px = [(self.dims[i].0 * scale).round().max(1.0), (self.dims[i].1 * scale).round().max(1.0)];
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
            let mut rgba: Vec<u8> = Vec::with_capacity(iw * ih * 4);
            for px in pix.data() {
                if night {
                    rgba.extend_from_slice(&[255 - px.r, 255 - px.g, 255 - px.b, 255]);
                } else {
                    rgba.extend_from_slice(&[px.r, px.g, px.b, px.a]);
                }
            }
            if !marks.is_empty() {
                pdfnotes::bake(&mut rgba, [iw, ih], [x, y], page_px, marks, night);
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

/// (page, scale·4096, tile x, tile y, night, highlight signature of the page when the tile was rendered)
type TileKey = (usize, u32, u16, u16, bool, u32);

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
    /// Width of the reading view the current scroll offset belongs to (0 before the first frame).
    /// Opening / closing the thumbnail or notes panel changes it; the offset is then re-based (see `reflow_offset`).
    lay_w: f32,
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
    // ── text layer ──
    texts: HashMap<usize, (Arc<PageText>, u64)>,
    /// Pages whose text could not be read (scanned documents without OCR, damaged fonts).
    no_text: HashSet<usize>,
    sel: Option<Sel>,
    /// A text selection drag is in progress (started on text).
    text_drag: bool,
    /// The press started outside text: the drag pans the page.
    pan: bool,
    /// A touch screen is in use: dragging pans, text is selected with double tap / the Select tool.
    touch: bool,
    find: Option<Find>,
    find_open: bool,
    find_text: String,
    /// Screen rectangle of the last selected line (anchor for the floating toolbar).
    sel_anchor: Option<Rect>,
    #[cfg(test)]
    dbg_shown: Vec<(usize, Rect)>,
}

/// Search state: hits are collected page by page, a little each frame.
struct Find {
    needle: Vec<char>,
    next: usize,
    hits: Vec<Sel>,
    cur: usize,
    jump: bool,
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
            lay_w: 0.0,
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
            texts: HashMap::new(),
            no_text: HashSet::new(),
            sel: None,
            text_drag: false,
            pan: false,
            touch: false,
            find: None,
            find_open: false,
            find_text: String::new(),
            sel_anchor: None,
            #[cfg(test)]
            dbg_shown: vec![],
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

    /// Text of page `i`, extracted on first use.
    fn text_of(&mut self, i: usize) -> Option<Arc<PageText>> {
        if let Some((t, used)) = self.texts.get_mut(&i) {
            *used = self.frame;
            return Some(t.clone());
        }
        if self.no_text.contains(&i) {
            return None;
        }
        let t = self.engine.as_ref().and_then(|e| e.extract(i));
        match t {
            Some(t) => {
                let t = Arc::new(t);
                self.texts.insert(i, (t.clone(), self.frame));
                if self.texts.len() > MAX_TEXT_PAGES {
                    let keep = self.sel.map(|s| s.ordered());
                    let cur = self.page;
                    let mut v: Vec<(usize, u64)> = self.texts.iter().map(|(k, (_, u))| (*k, *u)).collect();
                    v.sort_by_key(|x| x.1);
                    for (k, _) in v.into_iter().take(self.texts.len() - MAX_TEXT_PAGES) {
                        let in_sel = keep.map_or(false, |(a, b)| k >= a.page && k <= b.page);
                        if !in_sel && k != cur && k != i {
                            self.texts.remove(&k);
                        }
                    }
                }
                Some(t)
            }
            None => {
                self.no_text.insert(i);
                None
            }
        }
    }

    fn selection_text(&mut self) -> String {
        let Some(sel) = self.sel else { return String::new() };
        sel.text(|p| self.text_of(p))
    }

    fn set_find(&mut self, q: &str) {
        let needle = pdftext::needle_of(q);
        self.find = if needle.is_empty() { None } else { Some(Find { needle, next: 0, hits: vec![], cur: 0, jump: true }) };
    }

    /// Scans a few pages per frame (extraction is cheap, nothing is rendered). Returns true while more pages remain.
    fn step_find(&mut self) -> bool {
        let Some(n) = self.engine.as_ref().map(|e| e.dims.len()) else { return false };
        let Some(mut f) = self.find.take() else { return false };
        let t0 = now_ms();
        while f.next < n && now_ms() - t0 < 6.0 {
            let i = f.next;
            f.next += 1;
            let known = self.texts.contains_key(&i);
            if let Some(t) = self.text_of(i) {
                f.hits.extend(t.find(i, &f.needle));
                if !known && (i + 8 < self.page || i > self.page + 8) {
                    self.texts.remove(&i); // keep the cache for what the user looks at, not for what the search passes by
                }
            }
        }
        let more = f.next < n;
        self.find = Some(f);
        more
    }

    fn mem_bytes(&self) -> usize {
        self.tiles.values().map(|t| t.bytes).sum::<usize>() + self.thumb_tex.values().map(|t| t.bytes).sum::<usize>()
    }
}

// ───────────────────────────── text layer: selection, marks, search ─────────────────────────────

impl Reader {
    fn page_hit(&self, pos: Pos2, shown: &[(usize, Rect)]) -> Option<(usize, Rect)> {
        shown.iter().find(|(_, r)| r.expand(GAP / 2.0).contains(pos)).copied()
    }

    /// Page point of a screen position (with the page text).
    fn to_pt(&self, page: usize, r: Rect, pos: Pos2) -> Option<(Arc<PageText>, f32, f32)> {
        let (t, _) = self.texts.get(&page)?;
        Some((t.clone(), (pos.x - r.left()) / r.width().max(1.0) * t.w, (pos.y - r.top()) / r.height().max(1.0) * t.h))
    }

    fn over_text_at(&self, pos: Pos2, shown: &[(usize, Rect)]) -> bool {
        let Some((pg, r)) = self.page_hit(pos, shown) else { return false };
        self.to_pt(pg, r, pos).map_or(false, |(t, x, y)| t.over_text(x, y, 3.0 * t.w / r.width().max(1.0)))
    }

    /// Caret for a pointer position; outside the pages the nearest page / line is used (so dragging past the text still extends).
    fn caret_from(&self, pos: Pos2, shown: &[(usize, Rect)]) -> Option<pdftext::Caret> {
        let (pg, r) = self.page_hit(pos, shown).or_else(|| {
            shown
                .iter()
                .min_by(|a, b| {
                    let d = |r: &Rect| if pos.y < r.top() { r.top() - pos.y } else if pos.y > r.bottom() { pos.y - r.bottom() } else { 0.0 };
                    d(&a.1).partial_cmp(&d(&b.1)).unwrap_or(std::cmp::Ordering::Equal)
                })
                .copied()
        })?;
        let (t, x, y) = self.to_pt(pg, r, pos)?;
        t.caret_at(pg, x, y, None)
    }

    /// Pointer handling of the text layer. Returns true while it owns the pointer (the drawing tools must then stay idle).
    fn text_input(&mut self, ctx: &Context, ui: &Ui, resp: &egui::Response, shown: &[(usize, Rect)], view: Rect) -> bool {
        let tool = self.notes.tool;
        if ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Touch { .. }))) {
            self.touch = true;
        }
        if !matches!(tool, Tool::Hand | Tool::Highlight | Tool::Underline | Tool::Strike) {
            self.text_drag = false;
            self.pan = false;
            return false;
        }
        let mark = match tool {
            Tool::Highlight => Some(AnnotKind::Highlight),
            Tool::Underline => Some(AnnotKind::Underline),
            Tool::Strike => Some(AnnotKind::Strike),
            _ => None,
        };
        let mut handled = self.text_drag;
        if resp.drag_started() {
            let origin = ctx.input(|i| i.pointer.press_origin());
            let on_text = origin.map_or(false, |o| self.over_text_at(o, shown));
            if on_text && !(self.touch && tool == Tool::Hand) {
                if let Some(c) = origin.and_then(|o| self.caret_from(o, shown)) {
                    self.sel = Some(Sel { a: c, b: c });
                    self.text_drag = true;
                    handled = true;
                }
            } else if tool == Tool::Hand {
                self.pan = true;
            }
        }
        if self.text_drag && resp.dragged() {
            if let Some(pos) = ctx.pointer_latest_pos() {
                if let Some(c) = self.caret_from(pos, shown) {
                    if let Some(s) = self.sel.as_mut() {
                        s.b = c;
                    }
                }
                // auto-scroll while the pointer is near the top / bottom edge
                if pos.y < view.top() + 36.0 {
                    ui.scroll_with_delta(vec2(0.0, 12.0));
                } else if pos.y > view.bottom() - 36.0 {
                    ui.scroll_with_delta(vec2(0.0, -12.0));
                }
            }
            ctx.request_repaint();
        }
        if self.pan && resp.dragged() {
            ui.scroll_with_delta(resp.drag_delta());
        }
        if resp.drag_stopped() {
            if self.text_drag {
                self.text_drag = false;
                handled = true;
                match self.sel {
                    Some(s) if s.is_empty() => self.sel = None,
                    Some(_) => {
                        if let Some(k) = mark {
                            self.mark_selection(k, false);
                        }
                    }
                    None => {}
                }
            }
            self.pan = false;
        }
        if resp.clicked() {
            self.sel = None;
        }
        if resp.double_clicked() || resp.triple_clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                if let Some((pg, r)) = self.page_hit(pos, shown) {
                    if let Some((t, x, y)) = self.to_pt(pg, r, pos) {
                        let s = if resp.triple_clicked() { t.line_at(pg, y) } else { t.word_at(pg, x, y) };
                        if let Some(s) = s.filter(|s| !s.is_empty()) {
                            self.sel = Some(s);
                            handled = true;
                            if let Some(k) = mark {
                                self.mark_selection(k, false);
                            }
                        }
                    }
                }
            }
        }
        handled
    }

    fn text_cursor(&self, ctx: &Context, resp: &egui::Response, shown: &[(usize, Rect)]) {
        let tool = self.notes.tool;
        if self.touch || !matches!(tool, Tool::Hand | Tool::Highlight | Tool::Underline | Tool::Strike) {
            return;
        }
        if resp.hovered() || self.text_drag {
            if let Some(h) = ctx.pointer_hover_pos() {
                if self.text_drag || self.over_text_at(h, shown) {
                    ctx.set_cursor_icon(CursorIcon::Text);
                }
            }
        }
    }

    /// Turns the current selection into annotations (one per page) and clears it.
    fn mark_selection(&mut self, kind: AnnotKind, comment: bool) {
        let Some(sel) = self.sel else { return };
        let pieces = sel.pieces(|p| self.text_of(p));
        if let Some(id) = self.notes.add_text_marks(kind, &pieces) {
            if comment {
                self.notes.open_comment(id);
            }
        }
        self.sel = None;
    }

    /// Selection and search-hit rectangles of page `i`.
    fn paint_text_marks(&mut self, p: &egui::Painter, prect: Rect, i: usize, accent: Color32) {
        let Some((pt, _)) = self.texts.get(&i) else { return };
        let pt = pt.clone();
        let k = prect.width() / pt.w.max(1.0);
        let to_screen = |q: &[f32; 4]| Rect::from_min_max(pos2(prect.left() + q[0] * k, prect.top() + q[1] * k), pos2(prect.left() + q[2] * k, prect.top() + q[3] * k));
        if let Some(f) = &self.find {
            let from = f.hits.partition_point(|h| h.a.page < i);
            for (n, h) in f.hits[from..].iter().take_while(|h| h.a.page == i).enumerate() {
                let Some(l) = pt.lines.get(h.a.line as usize) else { continue };
                let (a, b) = (h.a.ch as usize, (h.b.ch as usize).min(l.chars.len()));
                if a >= b {
                    continue;
                }
                let q = [l.chars[a].x0, l.y0, l.chars[b - 1].x1, l.y1];
                let cur = from + n == f.cur;
                p.rect_filled(to_screen(&q), 2.0, if cur { Color32::from_rgba_unmultiplied(255, 140, 0, 150) } else { Color32::from_rgba_unmultiplied(255, 214, 0, 110) });
            }
        }
        if let Some(sel) = self.sel.filter(|s| !s.is_empty()) {
            let (s, e) = sel.ordered();
            if i < s.page || i > e.page {
                return;
            }
            let only = pt.clone();
            let pieces = sel.pieces(|pg| if pg == i { Some(only.clone()) } else { None });
            for pc in pieces {
                for q in &pc.quads {
                    p.rect_filled(to_screen(q), 1.0, with_alpha(accent, 0.38));
                }
                if i == e.page {
                    if let Some(q) = pc.quads.last() {
                        self.sel_anchor = Some(to_screen(q));
                    }
                }
            }
        }
    }

    /// Floating toolbar next to a finished selection: copy, highlight, underline, strike-through, comment, colour.
    fn sel_toolbar(&mut self, ui: &mut Ui, pal: &Palette, view: Rect) {
        let (Some(anchor), Some(_)) = (self.sel_anchor, self.sel.filter(|s| !s.is_empty())) else { return };
        if self.text_drag {
            return;
        }
        let ctx = ui.ctx().clone();
        let bw = 34.0;
        let acts: [(&str, &str); 5] = [("copy", l::COPY), ("hl", l::HIGHLIGHTER), ("ul", l::UNDERLINE), ("st", l::STRIKETHROUGH), ("cm", l::MESSAGE_SQUARE)];
        let w = acts.len() as f32 * bw + 10.0 + pdfnotes::COLORS.len() as f32 * 20.0 + 14.0;
        let h = 40.0;
        let mut x = (anchor.right() - w / 2.0).clamp(view.left() + 6.0, (view.right() - w - 6.0).max(view.left() + 6.0));
        let mut y = anchor.bottom() + 10.0;
        if y + h > view.bottom() - 4.0 {
            y = (anchor.top() - h - 10.0).max(view.top() + 4.0);
        }
        x = x.max(view.left() + 4.0);
        let bar = Rect::from_min_size(pos2(x, y), vec2(w, h));
        ui.painter().rect_filled(bar.translate(vec2(0.0, 2.0)).expand(1.0), 12.0, Color32::from_black_alpha(60));
        ui.painter().rect_filled(bar, 12.0, pal.bg);
        ui.painter().rect_stroke(bar, 12.0, egui::Stroke::new(1.0_f32, pal.border), egui::StrokeKind::Inside);
        let mut cx = bar.left() + 5.0;
        let mut act: Option<&str> = None;
        for (id, glyph) in acts {
            let r = Rect::from_min_size(pos2(cx, bar.top() + 4.0), vec2(bw, h - 8.0));
            cx += bw;
            let resp = ui.interact(r, Id::new(("pdf_seltb", id)), Sense::click());
            let hv = ctx.animate_bool_with_time(resp.id, resp.hovered(), 0.08);
            ui.painter().rect_filled(r, 8.0, with_alpha(pal.active, hv));
            ui.painter().text(r.center(), Align2::CENTER_CENTER, glyph, icons::font(16.0), pal.text);
            let tip = match id {
                "copy" => "Copy  (Ctrl+C)",
                "hl" => "Highlight",
                "ul" => "Underline",
                "st" => "Strike through",
                _ => "Highlight and comment",
            };
            if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip).clicked() {
                act = Some(id);
            }
        }
        cx += 10.0;
        for (i, c) in pdfnotes::COLORS.iter().enumerate() {
            let r = Rect::from_center_size(pos2(cx + 10.0, bar.center().y), vec2(20.0, 20.0));
            cx += 20.0;
            let resp = ui.interact(r, Id::new(("pdf_selcol", i)), Sense::click());
            ui.painter().circle_filled(r.center(), if resp.hovered() { 7.5 } else { 6.5 }, *c);
            if self.notes.color as usize == i {
                ui.painter().circle_stroke(r.center(), 9.0, egui::Stroke::new(1.5_f32, pal.text));
            }
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                self.notes.color = i as u8;
            }
        }
        match act {
            Some("copy") => {
                let t = self.selection_text();
                ctx.copy_text(t);
                self.sel = None;
            }
            Some("hl") => self.mark_selection(AnnotKind::Highlight, false),
            Some("ul") => self.mark_selection(AnnotKind::Underline, false),
            Some("st") => self.mark_selection(AnnotKind::Strike, false),
            Some(_) => self.mark_selection(AnnotKind::Highlight, true),
            None => {}
        }
    }

    /// Find-in-document bar (Ctrl+F).
    fn find_bar(&mut self, ui: &mut Ui, ctx: &Context, pal: &Palette, view: Rect) {
        if !self.find_open {
            return;
        }
        let w = 330.0f32.min(view.width() - 16.0);
        let bar = Rect::from_min_size(pos2(view.right() - w - 22.0, view.top() + 10.0), vec2(w, 40.0));
        ui.painter().rect_filled(bar.translate(vec2(0.0, 2.0)).expand(1.0), 12.0, Color32::from_black_alpha(70));
        ui.painter().rect_filled(bar, 12.0, pal.bg);
        ui.painter().rect_stroke(bar, 12.0, egui::Stroke::new(1.0_f32, pal.border), egui::StrokeKind::Inside);
        let id = Id::new("pdf_find_input");
        let input = Rect::from_min_size(bar.min + vec2(10.0, 6.0), vec2(w - 150.0, 28.0));
        let mut q = self.find_text.clone();
        let resp = ui.put(input, TextEdit::singleline(&mut q).id(id).hint_text("Find in document").font(body(14.0)).desired_width(input.width()));
        if resp.changed() {
            self.find_text = q.clone();
            self.set_find(&q);
        }
        let (enter, shift, esc) = ctx.input(|i| (i.key_pressed(Key::Enter), i.modifiers.shift, i.key_pressed(Key::Escape)));
        let n = self.find.as_ref().map_or(0, |f| f.hits.len());
        let scanning = self.find.as_ref().map_or(false, |f| self.engine.as_ref().map_or(false, |e| f.next < e.dims.len()));
        let label = if self.find.is_none() {
            String::new()
        } else if n == 0 {
            if scanning { "…".into() } else { "No results".into() }
        } else {
            format!("{}/{}{}", self.find.as_ref().map_or(0, |f| f.cur + 1), n, if scanning { "+" } else { "" })
        };
        ui.painter().text(pos2(bar.right() - 112.0, bar.center().y), Align2::RIGHT_CENTER, label, body(12.5), pal.muted);
        let mut step: i32 = 0;
        if resp.lost_focus() && enter {
            step = if shift { -1 } else { 1 };
            resp.request_focus();
        }
        for (k, (glyph, tip, d)) in [(l::CHEVRON_UP, "Previous", -1), (l::CHEVRON_DOWN, "Next", 1), (l::X, "Close  (Esc)", 0)].into_iter().enumerate() {
            let r = Rect::from_min_size(pos2(bar.right() - 104.0 + k as f32 * 32.0 + 6.0, bar.top() + 6.0), vec2(28.0, 28.0));
            let rr = ui.interact(r, Id::new(("pdf_findb", k)), Sense::click());
            let hv = ctx.animate_bool_with_time(rr.id, rr.hovered(), 0.08);
            ui.painter().rect_filled(r, 7.0, with_alpha(pal.active, hv));
            ui.painter().text(r.center(), Align2::CENTER_CENTER, glyph, icons::font(14.0), pal.text);
            if rr.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip).clicked() {
                if d == 0 {
                    self.find_open = false;
                    self.find = None;
                } else {
                    step = d;
                }
            }
        }
        if esc && resp.has_focus() {
            self.find_open = false;
            self.find = None;
        }
        if step != 0 {
            if let Some(f) = self.find.as_mut().filter(|f| !f.hits.is_empty()) {
                let n = f.hits.len() as i32;
                f.cur = ((f.cur as i32 + step).rem_euclid(n)) as usize;
                f.jump = true;
            }
        }
        if self.find_open && !resp.has_focus() && self.find_text.is_empty() {
            // first frame after opening: focus the field
            ctx.memory_mut(|m| m.request_focus(id));
        }
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

/// Re-bases a scroll offset when the width of the reading view changes (a side panel opened or closed, the window was
/// resized). Page scale is derived from the view width, so the same pixel offset would point at a different page —
/// with a long document the view used to jump several pages. The point at the top-left of the view (page + position
/// inside the page, in PDF points) and the horizontal centre are kept instead.
fn reflow_offset(dims: &[(f32, f32)], zoom: f32, old_w: f32, new_w: f32, off: Vec2) -> Vec2 {
    if dims.is_empty() || old_w <= 0.0 || (old_w - new_w).abs() < 0.5 {
        return off;
    }
    let old = layout(dims, zoom, old_w);
    let new = layout(dims, zoom, new_w);
    let i = page_at(&old, off.y);
    let local = (off.y - old.ys[i]) / old.s;
    let y = new.ys[i] + local * new.s;
    let cx = (off.x + old_w / 2.0 - old.content_w / 2.0) / old.s;
    let x = new.content_w / 2.0 + cx * new.s - new_w / 2.0;
    vec2(x.max(0.0), y.max(0.0))
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
        let (copy, find_key) = ctx.input(|i| (i.events.iter().any(|e| matches!(e, egui::Event::Copy)), i.modifiers.command && i.key_pressed(Key::F)));
        if copy && rd.sel.is_some() {
            let t = rd.selection_text();
            if !t.is_empty() {
                ctx.copy_text(t);
            }
        }
        if find_key {
            rd.find_open = true;
            ctx.memory_mut(|m| m.request_focus(Id::new("pdf_find_input")));
        }
        let esc_used = rd.notes.keys(ctx, editing);
        if esc && !esc_used {
            if rd.sel.is_some() || rd.find_open {
                rd.sel = None;
                rd.find_open = false;
                rd.find = None;
            } else {
                *keep = false;
            }
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
    // A side panel opened / closed (or the window was resized): keep the same page in view instead of the same pixel offset.
    if rd.lay_w > 0.0 && (rd.lay_w - view.width()).abs() >= 0.5 {
        let base = rd.pending_off.unwrap_or(rd.off);
        rd.pending_off = Some(reflow_offset(&dims, rd.zoom, rd.lay_w, view.width(), base));
    }
    rd.lay_w = view.width();
    let lay = layout(&dims, rd.zoom, view.width());
    let night = rd.night;
    let frame = rd.frame;
    let now = now_ms();
    let debounce = now - rd.zoom_changed < 160.0;

    rd.sel_anchor = None;
    if rd.step_find() {
        ctx.request_repaint();
    }
    // jump to the current search hit
    if let Some(f) = rd.find.as_mut().filter(|f| f.jump && !f.hits.is_empty()) {
        f.jump = false;
        let h = f.hits[f.cur.min(f.hits.len() - 1)];
        let (pg, li) = (h.a.page.min(dims.len() - 1), h.a.line as usize);
        if let Some((t, _)) = rd.texts.get(&pg) {
            if let Some(l) = t.lines.get(li) {
                let y = lay.ys[pg] + l.y0 * lay.s - view.height() * 0.35;
                rd.pending_off = Some(vec2(rd.pending_off.map_or(rd.off.x, |o| o.x), y.max(0.0)));
            }
        }
    }
    let mut sa = ScrollArea::both().id_salt("pdf_scroll").auto_shrink([false, false]).drag_to_scroll(false);
    if let Some(o) = rd.pending_off.take() {
        sa = sa.scroll_offset(o);
    }
    if rd.first_frame {
        rd.first_frame = false;
    }
    let mut want: Vec<(f32, TileKey, usize, f32, u32, u32, u32, u32)> = vec![];
    let mut want_thumb: Vec<usize> = vec![];
    let mut completed: Vec<(usize, u32, u32)> = vec![];
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(view).layout(egui::Layout::top_down(egui::Align::Min)));
    child.set_clip_rect(view.intersect(ui.clip_rect()));
    let out = sa.show_viewport(&mut child, |ui, viewport| {
        let capturing = rd.notes.capturing();
        let (content, resp) = ui.allocate_exact_size(vec2(lay.content_w, lay.content_h), Sense::click_and_drag());
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
            let sig = rd.notes.marks_sig(i);
            if let Some(bq) = best {
                let brs = bq as f32 / SCALE_Q;
                let f = rs_q / brs / ppp; // screen points per old pixel
                // (tiles of the previous zoom level, or of the same level before a highlight changed, stay visible until the new ones are ready)
                for (k, t) in rd.tiles.iter_mut().filter(|(k, _)| k.0 == i && k.1 == bq && k.4 == night && (bq != qs || k.5 != sig)) {
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
                    let key: TileKey = (i, qs, tx as u16, ty as u16, night, sig);
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
                completed.push((i, qs, sig));
            }
            if rd.failed.contains(&(i, qs)) {
                p.text(prect.center(), Align2::CENTER_CENTER, "This page could not be rendered", body(14.0), Color32::GRAY);
            }
            // 4. annotations on top of the page
            rd.notes.paint_page(&p, prect, i, night, accent, rd.failed.iter().any(|(pg, _)| *pg == i));
            rd.paint_text_marks(&p, prect, i, accent);
        }
        // the text layer of the page in the middle of the screen is read first, one page per frame
        let missing: Vec<usize> = shown.iter().map(|s| s.0).filter(|i| !rd.texts.contains_key(i) && !rd.no_text.contains(i)).collect();
        if let Some(i) = missing.iter().copied().min_by_key(|i| i.abs_diff(cur)) {
            rd.text_of(i);
            if missing.len() > 1 {
                ctx.request_repaint();
            }
        }
        #[cfg(test)]
        {
            rd.dbg_shown = shown.clone();
        }
        // drawing tools get the raw finger (otherwise a swipe scrolls the page, see touch.rs)
        crate::touch::set_passthrough(capturing);
        let editing_mark = rd.notes.drag_edit(ctx, &resp, &shown);
        let handled = editing_mark || rd.text_input(ctx, ui, &resp, &shown, view);
        if !handled && (capturing || rd.notes.bar || !rd.notes.list.is_empty()) {
            rd.notes.input(ctx, &resp, &shown);
        }
        rd.text_cursor(ctx, &resp, &shown);
        if !capturing && resp.double_clicked() && rd.sel.is_none() {
            let at = resp.interact_pointer_pos().unwrap_or(view.center());
            if !rd.over_text_at(at, &shown) {
                *zoom_req = Some((if rd.zoom < 1.6 { 2.0 } else { 1.0 }, at));
            }
        }
    });
    rd.sel_toolbar(ui, pal, view);
    rd.find_bar(ui, ctx, pal, view);
    rd.off = out.state.offset;
    rd.vp = out.inner_rect;

    // retire stale scales of completed pages
    for (i, qs, sig) in completed {
        rd.best.insert(i, qs);
        let stale: Vec<TileKey> = rd.tiles.keys().filter(|k| k.0 == i && (k.1 != qs || k.5 != sig)).copied().collect();
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
            match e.render_marked(i, rs_q, px, py, tw, th, night, &rd.notes.marks_on(i)) {
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

    /// A PDF with `n` identical portrait pages.
    fn multi_pdf(n: usize) -> Vec<u8> {
        let mut objs: Vec<String> = vec![String::new(), String::new()];
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + i * 2)).collect();
        objs[0] = "<< /Type /Catalog /Pages 2 0 R >>".into();
        objs[1] = format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), n);
        for i in 0..n {
            let c = format!("BT /F1 24 Tf 20 100 Td (Page {}) Tj ET", i + 1);
            objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents {} 0 R /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> >>", 4 + i * 2));
            objs.push(format!("<< /Length {} >>\nstream\n{}\nendstream", c.len(), c));
        }
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
    fn reflow_keeps_the_same_page_when_the_view_width_changes() {
        let dims = vec![(595.0, 842.0); 40];
        for zoom in [0.5_f32, 1.0, 2.0] {
            for target in [0usize, 7, 19, 39] {
                let lay = layout(&dims, zoom, 1200.0);
                let off = vec2(0.0, lay.ys[target] + 40.0);
                for (a, b) in [(1200.0, 1068.0), (1068.0, 748.0), (1200.0, 380.0), (380.0, 1200.0)] {
                    let lay_a = layout(&dims, zoom, a);
                    let off_a = vec2(0.0, lay_a.ys[target] + 40.0 * lay_a.s / lay.s);
                    let o2 = reflow_offset(&dims, zoom, a, b, off_a);
                    let lay_b = layout(&dims, zoom, b);
                    assert_eq!(page_at(&lay_b, o2.y), target, "zoom {zoom}, page {target}, {a} -> {b}");
                    // the same relative position inside the page is preserved
                    let ra = (off_a.y - lay_a.ys[target]) / lay_a.s;
                    let rb = (o2.y - lay_b.ys[target]) / lay_b.s;
                    assert!((ra - rb).abs() < 0.01);
                }
                let _ = off;
            }
        }
    }

    #[test]
    fn opening_thumbnails_and_notes_does_not_change_the_page() {
        let ctx = Context::default();
        crate::theme::setup_fonts(&ctx);
        let mut req = Some(Request { key: 11, title: "long.pdf".into(), source: Some(Source::Local(multi_pdf(40).into())) });
        let mut annots = BTreeMap::new();
        let mut t = 0.0;
        let size = vec2(1280.0, 800.0);
        for _ in 0..6 {
            run(&ctx, &mut req, &mut annots, &mut t, size, vec![]);
        }
        // scroll to page 30
        READER.with(|c| {
            let mut g = c.borrow_mut();
            let r = g.as_mut().unwrap();
            let lay = layout(&r.engine.as_ref().unwrap().dims, r.zoom, r.lay_w);
            r.pending_off = Some(vec2(0.0, lay.ys[29] + 30.0));
        });
        for _ in 0..5 {
            run(&ctx, &mut req, &mut annots, &mut t, size, vec![]);
        }
        let page = || READER.with(|c| c.borrow().as_ref().unwrap().page);
        assert_eq!(page(), 29, "scrolled to page 30");
        let toggle = |f: &dyn Fn(&mut Reader)| READER.with(|c| f(c.borrow_mut().as_mut().unwrap()));
        let mut step = |n: usize| {
            for _ in 0..n {
                run(&ctx, &mut req, &mut annots, &mut t, size, vec![]);
            }
        };
        toggle(&|r| r.thumbs = true);
        step(5);
        assert_eq!(page(), 29, "thumbnail panel opened");
        toggle(&|r| {
            r.notes.bar = true;
            r.notes.panel = true;
        });
        step(5);
        assert_eq!(page(), 29, "notes panel opened");
        toggle(&|r| r.thumbs = false);
        step(5);
        assert_eq!(page(), 29, "thumbnail panel closed");
        toggle(&|r| {
            r.notes.panel = false;
            r.notes.bar = false;
        });
        step(5);
        assert_eq!(page(), 29, "notes panel closed");
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

    #[test]
    fn text_layer_reads_the_page() {
        let Ok(e) = Engine::new(tiny_pdf().into(), "") else { panic!("pdf did not open") };
        let t = e.extract(0).expect("text layer");
        assert_eq!(t.lines.len(), 1);
        let plain: String = t.lines[0].chars.iter().map(|c| c.ch).collect();
        assert_eq!(plain, "Hello PDF");
        let l = &t.lines[0];
        // the text starts at x = 20 pt and its baseline is 60 pt from the top of the 100 pt page
        assert!((l.chars[0].x0 - 20.0).abs() < 1.5, "x0 = {}", l.chars[0].x0);
        assert!(l.y0 < 60.0 && l.y1 > 60.0 && l.y1 - l.y0 < 30.0, "line box {} .. {}", l.y0, l.y1);
        assert!(l.chars.windows(2).all(|w| w[1].x0 >= w[0].x0), "characters are ordered left to right");
    }

    fn drag(ctx: &Context, req: &mut Option<Request>, annots: &mut BTreeMap<u64, Vec<PdfAnnot>>, t: &mut f64, size: Vec2, a: egui::Pos2, b: egui::Pos2) {
        let btn = |p, pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        run(ctx, req, annots, t, size, vec![egui::Event::PointerMoved(a)]);
        run(ctx, req, annots, t, size, vec![btn(a, true)]);
        run(ctx, req, annots, t, size, vec![egui::Event::PointerMoved(pos2((a.x + b.x) / 2.0, a.y))]);
        run(ctx, req, annots, t, size, vec![egui::Event::PointerMoved(b)]);
        run(ctx, req, annots, t, size, vec![btn(b, false)]);
        for _ in 0..2 {
            run(ctx, req, annots, t, size, vec![]);
        }
    }

    #[test]
    fn text_can_be_selected_copied_and_highlighted() {
        let ctx = Context::default();
        crate::theme::setup_fonts(&ctx);
        let mut req = Some(Request { key: 9, title: "t.pdf".into(), source: Some(Source::Local(tiny_pdf().into())) });
        let mut annots = BTreeMap::new();
        let mut t = 0.0;
        let size = vec2(1280.0, 800.0);
        for _ in 0..8 {
            run(&ctx, &mut req, &mut annots, &mut t, size, vec![]);
        }
        // screen position of the text line
        let geom = |end: bool| {
            READER.with(|c| {
                let g = c.borrow();
                let r = g.as_ref().unwrap();
                let (_, prect) = r.dbg_shown[0];
                let (pt, _) = r.texts.get(&0).expect("text layer extracted for the visible page");
                let k = prect.width() / pt.w;
                let l = &pt.lines[0];
                let y = prect.top() + (l.y0 + l.y1) / 2.0 * k;
                let x = if end { prect.left() + l.chars.last().unwrap().x1 * k + 2.0 } else { prect.left() + l.chars[0].x0 * k + 1.0 };
                pos2(x, y)
            })
        };
        // 1. plain drag with the default (select / scroll) tool selects the text
        let (a, b) = (geom(false), geom(true));
        drag(&ctx, &mut req, &mut annots, &mut t, size, a, b);
        let copied = READER.with(|c| c.borrow_mut().as_mut().unwrap().selection_text());
        assert_eq!(copied, "Hello PDF", "dragging over text selects it");
        // a click elsewhere clears the selection
        let blank = pos2(700.0, 200.0);
        let btn = |p, pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        run(&ctx, &mut req, &mut annots, &mut t, size, vec![egui::Event::PointerMoved(blank)]);
        run(&ctx, &mut req, &mut annots, &mut t, size, vec![btn(blank, true)]);
        run(&ctx, &mut req, &mut annots, &mut t, size, vec![btn(blank, false)]);
        assert!(READER.with(|c| c.borrow().as_ref().unwrap().sel.is_none()), "click clears the selection");
        // 2. with the highlighter armed, the same drag stores a text-anchored highlight
        READER.with(|c| {
            let mut g = c.borrow_mut();
            let r = g.as_mut().unwrap();
            r.notes.bar = true;
            r.notes.tool = Tool::Highlight;
        });
        for _ in 0..3 {
            run(&ctx, &mut req, &mut annots, &mut t, size, vec![]);
        }
        let (a, b) = (geom(false), geom(true));
        drag(&ctx, &mut req, &mut annots, &mut t, size, a, b);
        let list = annots.get(&9).cloned().unwrap_or_default();
        assert_eq!(list.len(), 1, "one highlight stored");
        assert_eq!(list[0].quote, "Hello PDF");
        assert_eq!(list[0].quads.len(), 1);
        let q = list[0].quads[0];
        assert!(q[0] > 0.05 && q[2] < 0.95 && q[1] < 0.6 && q[3] > 0.55, "quad hugs the text line: {q:?}");
        // 3. search finds the word
        READER.with(|c| {
            let mut g = c.borrow_mut();
            let r = g.as_mut().unwrap();
            r.set_find("pdf");
            while r.step_find() {}
            assert_eq!(r.find.as_ref().unwrap().hits.len(), 1);
        });
    }
}

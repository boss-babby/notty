//! Text layer of the PDF reader: extraction, hit testing, selection, copy and search.
//!
//! The renderer (hayro) only produces pixels, so a page has no selectable text by itself. This module runs hayro's content-stream
//! interpreter once per page with a *recording device*: every glyph that the page paints (also invisible OCR text) arrives with its
//! Unicode value and the exact transform that places it on the page. The glyphs are stored as lines of characters in **PDF points**
//! (origin top-left, unrotated page space), so the layer is independent of zoom level, device pixel ratio and tiling. The reader
//! converts pointer positions to page points, hit-tests them here and paints selection / search rectangles on top of the tiles.
//!
//! Design notes (scaling):
//! * Extraction is lazy (only pages that are on screen, or that a search / copy needs), costs no rasterisation and is cached in a
//!   small LRU owned by the reader; closing the reader drops everything.
//! * Everything here is pure data (no egui context, no hayro lifetimes), so it is trivially testable and can later move to a
//!   worker thread or feed a full-text index without changing the UI code.

use std::sync::Arc;

use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::{BfString, WritingMode};
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_interpret::{
    interpret_page, BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps, InterpreterCache, InterpreterSettings, SoftMask,
};
use hayro::hayro_syntax::page::Page;
use hayro::kurbo::{BezPath, Point, Rect as KRect, Shape};

/// Fraction of the em box above / below the baseline used for selection rectangles.
const ASCENT: f64 = 0.80;
const DESCENT: f64 = 0.20;

#[derive(Clone, Debug, PartialEq)]
pub struct TChar {
    pub ch: char,
    pub x0: f32,
    pub x1: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TLine {
    pub y0: f32,
    pub y1: f32,
    pub chars: Vec<TChar>,
}

/// All text of one page (points, y down).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageText {
    pub w: f32,
    pub h: f32,
    pub lines: Vec<TLine>,
}

/// A caret position: between characters of a line (`ch` may equal `chars.len()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Caret {
    pub page: usize,
    pub line: u32,
    pub ch: u32,
}

/// A text selection (anchor + focus, either order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sel {
    pub a: Caret,
    pub b: Caret,
}

/// The part of a selection that lies on one page.
#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    pub page: usize,
    pub w: f32,
    pub h: f32,
    /// One rectangle per line: `[x0, y0, x1, y1]` in points.
    pub quads: Vec<[f32; 4]>,
    pub text: String,
}

// ───────────────────────────── extraction ─────────────────────────────

struct G {
    ch: char,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    base: f32,
    size: f32,
}

#[derive(Default)]
struct Collector {
    glyphs: Vec<G>,
}

impl<'a> Device<'a> for Collector {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, mode: &DrawMode) {
        // `Fill + Stroke` modes arrive as two calls; the stroke call would duplicate every character.
        if matches!(mode, DrawMode::Stroke(_)) || matches!(run.writing_mode(), WritingMode::Vertical) {
            return;
        }
        let glyphs = run.glyphs();
        for (i, g) in glyphs.iter().enumerate() {
            let t = props.transform * g.transform();
            let [a, b, c, d, e, f] = t.as_coeffs();
            let sx = (a * a + b * b).sqrt();
            let size = (c * c + d * d).sqrt() * 1000.0;
            if !(sx > 1e-9) || !size.is_finite() || size < 0.5 {
                continue;
            }
            // horizontal advance in glyph units: distance to the next glyph origin along the baseline, else the outline width
            let mut adv = 0.0;
            if let Some(n) = glyphs.get(i + 1) {
                let [_, _, _, _, ne, nf] = (props.transform * n.transform()).as_coeffs();
                adv = ((ne - e) * a + (nf - f) * b) / (sx * sx);
            }
            if !(adv > 1.0) {
                adv = match &**g {
                    Glyph::Outline(o) => {
                        let bb = o.outline().bounding_box();
                        if bb.x1 > 20.0 {
                            bb.x1 + 40.0
                        } else {
                            280.0
                        }
                    }
                    _ => 500.0,
                };
            }
            let text: Vec<char> = match g.as_unicode() {
                Some(BfString::Char(ch)) => vec![ch],
                Some(BfString::String(s)) => s.chars().collect(),
                None => match char::from_u32(g.char_code()) {
                    Some(ch) if ch.is_ascii_graphic() || ch == ' ' => vec![ch],
                    _ => vec![],
                },
            };
            if text.is_empty() {
                continue;
            }
            // axis-aligned box of the em rectangle (descent … ascent) in page space
            let corners = [Point::new(0.0, -DESCENT * 1000.0), Point::new(adv, -DESCENT * 1000.0), Point::new(adv, ASCENT * 1000.0), Point::new(0.0, ASCENT * 1000.0)];
            let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for p in corners {
                let q = t * p;
                x0 = x0.min(q.x);
                y0 = y0.min(q.y);
                x1 = x1.max(q.x);
                y1 = y1.max(q.y);
            }
            if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
                continue;
            }
            let n = text.len() as f64;
            for (k, ch) in text.into_iter().enumerate() {
                if ch.is_control() && ch != '\t' {
                    continue;
                }
                let k = k as f64;
                self.glyphs.push(G {
                    ch: if ch == '\t' || ch == '\u{a0}' { ' ' } else { ch },
                    x0: (x0 + (x1 - x0) * k / n) as f32,
                    x1: (x0 + (x1 - x0) * (k + 1.0) / n) as f32,
                    y0: y0 as f32,
                    y1: y1 as f32,
                    base: f as f32,
                    size: size as f32,
                });
            }
        }
    }
}

/// Reads all text of `page`. `cache` is the interpreter cache of the document (fonts are parsed once).
pub fn extract<'a>(page: &'a Page<'a>, cache: &InterpreterCache<'a>) -> PageText {
    let (w, h) = page.render_dimensions();
    let t = page.initial_transform(true).to_kurbo();
    let mut ctx = Context::new(t, KRect::new(0.0, 0.0, w as f64, h as f64), cache, page.xref(), InterpreterSettings::default());
    let mut dev = Collector::default();
    interpret_page(page, &mut ctx, &mut dev);
    build(dev.glyphs, w, h)
}

fn build(glyphs: Vec<G>, w: f32, h: f32) -> PageText {
    struct Cur {
        base: f32,
        size: f32,
        line: TLine,
    }
    let mut lines: Vec<TLine> = vec![];
    let mut cur: Option<Cur> = None;
    let flush = |cur: &mut Option<Cur>, lines: &mut Vec<TLine>| {
        if let Some(c) = cur.take() {
            let mut l = c.line;
            while l.chars.last().map_or(false, |c| c.ch == ' ') {
                l.chars.pop();
            }
            if !l.chars.is_empty() {
                lines.push(l);
            }
        }
    };
    for g in glyphs {
        // glyphs far outside the page (clipped text, off-page tricks) are not selectable
        if g.x1 < -20.0 || g.x0 > w + 20.0 || g.y1 < -20.0 || g.y0 > h + 20.0 {
            continue;
        }
        let same = cur.as_ref().map_or(false, |c| {
            let last = c.line.chars.last().map_or(f32::MIN, |x| x.x1);
            (g.base - c.base).abs() < 0.45 * g.size.max(c.size) && g.x0 > last - 0.8 * g.size.max(c.size)
        });
        if !same {
            flush(&mut cur, &mut lines);
            cur = Some(Cur { base: g.base, size: g.size, line: TLine { y0: g.y0, y1: g.y1, chars: vec![] } });
        }
        let c = cur.as_mut().expect("line");
        if let Some(last) = c.line.chars.last().cloned() {
            let gap = g.x0 - last.x1;
            if gap > 0.18 * c.size && last.ch != ' ' && g.ch != ' ' {
                c.line.chars.push(TChar { ch: ' ', x0: last.x1, x1: g.x0 });
            }
        }
        c.line.y0 = c.line.y0.min(g.y0);
        c.line.y1 = c.line.y1.max(g.y1);
        c.size = c.size.max(g.size);
        c.line.chars.push(TChar { ch: g.ch, x0: g.x0, x1: g.x1.max(g.x0 + 0.1) });
    }
    flush(&mut cur, &mut lines);
    PageText { w, h, lines }
}

// ───────────────────────────── queries ─────────────────────────────

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '\'' || c == '-'
}

impl PageText {
    /// Nearest line to `y` and its vertical distance (0 when inside).
    fn nearest_line(&self, y: f32) -> Option<(usize, f32)> {
        self.lines
            .iter()
            .enumerate()
            .map(|(i, l)| (i, if y < l.y0 { l.y0 - y } else if y > l.y1 { y - l.y1 } else { 0.0 }))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// True when `(x, y)` lies on (or within `slack` points of) a character.
    pub fn over_text(&self, x: f32, y: f32, slack: f32) -> bool {
        self.lines.iter().any(|l| {
            y >= l.y0 - slack
                && y <= l.y1 + slack
                && l.chars.first().map_or(false, |f| x >= f.x0 - slack)
                && l.chars.last().map_or(false, |e| x <= e.x1 + slack)
        })
    }

    /// Caret nearest to `(x, y)`; with `max_dy = None` the nearest line is used however far away it is.
    pub fn caret_at(&self, page: usize, x: f32, y: f32, max_dy: Option<f32>) -> Option<Caret> {
        let (li, dy) = self.nearest_line(y)?;
        if max_dy.map_or(false, |m| dy > m) {
            return None;
        }
        let l = &self.lines[li];
        let ch = l.chars.iter().take_while(|c| (c.x0 + c.x1) * 0.5 < x).count();
        Some(Caret { page, line: li as u32, ch: ch as u32 })
    }

    /// Selection of the word under `(x, y)`.
    pub fn word_at(&self, page: usize, x: f32, y: f32) -> Option<Sel> {
        let c = self.caret_at(page, x, y, Some(4.0))?;
        let l = &self.lines[c.line as usize];
        let n = l.chars.len();
        if n == 0 {
            return None;
        }
        let mut i = (c.ch as usize).min(n - 1);
        // prefer the character under the pointer
        if i > 0 && x < l.chars[i].x0 {
            i -= 1;
        }
        let (mut a, mut b) = (i, i + 1);
        if word_char(l.chars[i].ch) {
            while a > 0 && word_char(l.chars[a - 1].ch) {
                a -= 1;
            }
            while b < n && word_char(l.chars[b].ch) {
                b += 1;
            }
        }
        Some(Sel { a: Caret { page, line: c.line, ch: a as u32 }, b: Caret { page, line: c.line, ch: b as u32 } })
    }

    pub fn line_at(&self, page: usize, y: f32) -> Option<Sel> {
        let (li, dy) = self.nearest_line(y)?;
        if dy > 4.0 {
            return None;
        }
        let n = self.lines[li].chars.len() as u32;
        Some(Sel { a: Caret { page, line: li as u32, ch: 0 }, b: Caret { page, line: li as u32, ch: n } })
    }

    /// Case-insensitive search; hits never span lines.
    pub fn find(&self, page: usize, needle: &[char]) -> Vec<Sel> {
        let mut out = vec![];
        if needle.is_empty() {
            return out;
        }
        for (li, l) in self.lines.iter().enumerate() {
            let hay: Vec<char> = l.chars.iter().map(|c| lower(c.ch)).collect();
            if hay.len() < needle.len() {
                continue;
            }
            let mut i = 0;
            while i + needle.len() <= hay.len() {
                if hay[i..i + needle.len()] == *needle {
                    out.push(Sel { a: Caret { page, line: li as u32, ch: i as u32 }, b: Caret { page, line: li as u32, ch: (i + needle.len()) as u32 } });
                    i += needle.len();
                } else {
                    i += 1;
                }
            }
        }
        out
    }
}

pub fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

pub fn needle_of(q: &str) -> Vec<char> {
    q.trim().chars().map(lower).collect()
}

impl Sel {
    pub fn ordered(&self) -> (Caret, Caret) {
        if self.a <= self.b {
            (self.a, self.b)
        } else {
            (self.b, self.a)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.a == self.b
    }

    /// The pieces of this selection per page. `get` returns the text of a page (extracting it when needed).
    pub fn pieces(&self, mut get: impl FnMut(usize) -> Option<Arc<PageText>>) -> Vec<Piece> {
        let (s, e) = self.ordered();
        let mut out = vec![];
        for page in s.page..=e.page {
            let Some(pt) = get(page) else { continue };
            let mut quads = vec![];
            let mut text = String::new();
            for (li, l) in pt.lines.iter().enumerate() {
                let li = li as u32;
                if (page, li) < (s.page, s.line) || (page, li) > (e.page, e.line) {
                    continue;
                }
                let from = if (page, li) == (s.page, s.line) { s.ch as usize } else { 0 };
                let to = if (page, li) == (e.page, e.line) { e.ch as usize } else { l.chars.len() };
                let (from, to) = (from.min(l.chars.len()), to.min(l.chars.len()));
                if from >= to {
                    continue;
                }
                quads.push([l.chars[from].x0, l.y0, l.chars[to - 1].x1, l.y1]);
                if !text.is_empty() {
                    // join wrapped lines: a trailing hyphen glues the word back together, otherwise a space
                    if text.ends_with('-') && text.chars().rev().nth(1).map_or(false, |c| c.is_alphabetic()) {
                        text.pop();
                    } else {
                        text.push(' ');
                    }
                }
                text.extend(l.chars[from..to].iter().map(|c| c.ch));
            }
            if !quads.is_empty() {
                out.push(Piece { page, w: pt.w, h: pt.h, quads, text });
            }
        }
        out
    }

    /// Selected text over all pages, as it would be copied.
    pub fn text(&self, get: impl FnMut(usize) -> Option<Arc<PageText>>) -> String {
        self.pieces(get).into_iter().map(|p| p.text).collect::<Vec<_>>().join("\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, x: f32, y: f32) -> TLine {
        TLine { y0: y, y1: y + 10.0, chars: text.chars().enumerate().map(|(i, ch)| TChar { ch, x0: x + i as f32 * 5.0, x1: x + i as f32 * 5.0 + 5.0 }).collect() }
    }

    fn sample() -> PageText {
        PageText { w: 200.0, h: 100.0, lines: vec![line("Hello big-world", 10.0, 10.0), line("second line", 10.0, 24.0)] }
    }

    #[test]
    fn hit_testing_words_and_lines() {
        let p = sample();
        assert!(p.over_text(30.0, 15.0, 2.0) && !p.over_text(30.0, 60.0, 2.0) && !p.over_text(150.0, 15.0, 2.0));
        let w = p.word_at(0, 13.0, 15.0).unwrap();
        assert_eq!((w.a.ch, w.b.ch), (0, 5), "Hello");
        let w = p.word_at(0, 55.0, 15.0).unwrap();
        assert_eq!((w.a.ch, w.b.ch), (6, 15), "hyphenated word stays whole");
        let l = p.line_at(0, 28.0).unwrap();
        assert_eq!((l.a.line, l.b.ch), (1, 11));
        let c = p.caret_at(0, 1000.0, 15.0, None).unwrap();
        assert_eq!((c.line, c.ch), (0, 15));
    }

    #[test]
    fn selection_spans_lines_and_copies() {
        let p = Arc::new(sample());
        let sel = Sel { a: Caret { page: 0, line: 0, ch: 6 }, b: Caret { page: 0, line: 1, ch: 6 } };
        let pieces = sel.pieces(|_| Some(p.clone()));
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].quads.len(), 2);
        assert_eq!(pieces[0].text, "big-world second");
        let rev = Sel { a: sel.b, b: sel.a };
        assert_eq!(rev.text(|_| Some(p.clone())), "big-world second");
    }

    #[test]
    fn search_is_case_insensitive() {
        let p = sample();
        let hits = p.find(3, &needle_of("LINE"));
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].a.page, hits[0].a.line, hits[0].a.ch, hits[0].b.ch), (3, 1, 7, 11));
        assert!(p.find(0, &needle_of("zzz")).is_empty());
    }
}

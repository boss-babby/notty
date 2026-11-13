//! Rich-text model used while *editing* a block.
//!
//! A block's text is persisted as lightweight Markdown (`**bold**`, `==highlight==` …) so that import / export / search keep
//! working. While editing, however, the user must never see those markers: the editor works on a [`Rich`] value — the plain
//! text plus one [`Fmt`] per character — and converts back to the marked-up string whenever something changes.
//! Special characters that occur in the plain text are escaped with a backslash, so parse → markup is lossless.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Stroke};

use crate::editor::parse_spans;
use crate::theme::{bold, tag_color, Palette};

pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;
pub const STRIKE: u8 = 4;
pub const CODE: u8 = 8;
pub const UNDERLINE: u8 = 16;
pub const HIGHLIGHT: u8 = 32;

/// Formatting of a single character. `link` is 0 for "no link", otherwise `index + 1` into [`Rich::links`].
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Fmt {
    pub flags: u8,
    pub link: u16,
}

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Rich {
    pub plain: String,
    pub fmts: Vec<Fmt>,
    pub links: Vec<String>,
}

/// Maps the markdown marker of the toolbar / shortcuts to a flag.
pub fn flag_for(mark: &str) -> u8 {
    match mark {
        "**" => BOLD,
        "*" => ITALIC,
        "~~" => STRIKE,
        "`" => CODE,
        "__" => UNDERLINE,
        "==" => HIGHLIGHT,
        _ => 0,
    }
}

pub fn has_marks(s: &str) -> bool {
    s.contains('*') || s.contains('`') || s.contains("~~") || s.contains("==") || s.contains("__") || s.contains("](") || s.contains('\\') || s.contains("[[")
}

/// `[[Page title]]` / `[[Page title|label]]` mentions in a plain text: `(byte range incl. brackets, page title)`.
pub fn wiki_spans(plain: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let mut out = vec![];
    let mut from = 0usize;
    while let Some(a) = plain[from..].find("[[") {
        let a = from + a;
        let Some(len) = plain[a + 2..].find("]]") else { break };
        let inner = &plain[a + 2..a + 2 + len];
        if inner.trim().is_empty() || inner.contains('\n') || inner.contains("[[") {
            from = a + 2;
            continue;
        }
        let title = inner.split('|').next().unwrap_or("").trim().to_owned();
        let end = a + 2 + len + 2;
        if !title.is_empty() {
            out.push((a..end, title));
        }
        from = end;
    }
    out
}

/// Plain text of a marked-up string (markers removed). Used for search, outlines, titles …
pub fn strip(markup: &str) -> String {
    if !has_marks(markup) {
        return markup.to_owned();
    }
    Rich::parse(markup).plain
}

impl Rich {
    pub fn plain(s: &str) -> Rich {
        Rich { plain: s.to_owned(), fmts: vec![Fmt::default(); s.chars().count()], links: vec![] }
    }

    pub fn parse(markup: &str) -> Rich {
        if !has_marks(markup) {
            return Rich::plain(markup);
        }
        let mut r = Rich::default();
        for sp in parse_spans(markup) {
            if sp.marker {
                continue;
            }
            let mut f = Fmt { flags: 0, link: 0 };
            for (on, bit) in [(sp.bold, BOLD), (sp.italic, ITALIC), (sp.strike, STRIKE), (sp.code, CODE), (sp.ul, UNDERLINE), (sp.hl, HIGHLIGHT)] {
                if on {
                    f.flags |= bit;
                }
            }
            if let Some(url) = &sp.link {
                f.link = r.link_id(url);
            }
            for _ in sp.text.chars() {
                r.fmts.push(f);
            }
            r.plain.push_str(&sp.text);
        }
        r
    }

    pub fn len(&self) -> usize {
        self.fmts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fmts.is_empty()
    }

    fn link_id(&mut self, url: &str) -> u16 {
        if let Some(p) = self.links.iter().position(|l| l == url) {
            return p as u16 + 1;
        }
        self.links.push(url.to_owned());
        self.links.len() as u16
    }

    pub fn link_at(&self, i: usize) -> Option<&str> {
        let f = self.fmts.get(i)?;
        if f.link == 0 {
            None
        } else {
            self.links.get(f.link as usize - 1).map(|s| s.as_str())
        }
    }

    /// Title of the `[[wiki link]]` under character `i`, if any.
    pub fn wiki_at(&self, i: usize) -> Option<String> {
        let b = self.byte(i);
        wiki_spans(&self.plain).into_iter().find(|(r, _)| r.contains(&b)).map(|(_, t)| t)
    }

    fn byte(&self, chars: usize) -> usize {
        self.plain.char_indices().nth(chars).map_or(self.plain.len(), |(b, _)| b)
    }

    /// Consecutive characters sharing one format: `(byte range, format)`.
    pub fn runs(&self) -> Vec<(std::ops::Range<usize>, Fmt)> {
        let mut out = vec![];
        let mut start = 0usize;
        let mut cur: Option<Fmt> = None;
        let mut pos = 0usize;
        for (ch, f) in self.plain.chars().zip(self.fmts.iter()) {
            if cur != Some(*f) {
                if let Some(c) = cur {
                    out.push((start..pos, c));
                }
                start = pos;
                cur = Some(*f);
            }
            pos += ch.len_utf8();
        }
        if let Some(c) = cur {
            out.push((start..pos, c));
        }
        out
    }

    /// Serialises to the persisted marked-up string.
    pub fn markup(&self) -> String {
        let mut out = String::with_capacity(self.plain.len() + 8);
        let mut open: u8 = 0;
        for (range, f) in self.runs() {
            let text = &self.plain[range];
            let linked = f.link != 0;
            // links cannot contain code
            let want = if linked { f.flags & !CODE } else { f.flags };
            for bit in [CODE, HIGHLIGHT, UNDERLINE, STRIKE, ITALIC, BOLD] {
                if open & bit != 0 && want & bit == 0 {
                    out.push_str(mark_of(bit));
                    open &= !bit;
                }
            }
            for bit in [BOLD, ITALIC, STRIKE, UNDERLINE, HIGHLIGHT, CODE] {
                if want & bit != 0 && open & bit == 0 {
                    out.push_str(mark_of(bit));
                    open |= bit;
                }
            }
            if linked {
                let url = self.links.get(f.link as usize - 1).map(|s| s.as_str()).unwrap_or("");
                out.push('[');
                escape_into(&mut out, text, true);
                out.push_str("](");
                out.push_str(&url.replace(' ', "%20").replace(')', "%29"));
                out.push(')');
            } else if want & CODE != 0 {
                out.push_str(text); // code spans are literal
            } else {
                escape_into(&mut out, text, false);
            }
        }
        for bit in [CODE, HIGHLIGHT, UNDERLINE, STRIKE, ITALIC, BOLD] {
            if open & bit != 0 {
                out.push_str(mark_of(bit));
            }
        }
        out
    }

    /// Re-synchronises the formats after the text was replaced by `new_plain` (typing, deleting, pasting …).
    /// New characters inherit the neighbouring format, `pending` (a toggle made with an empty selection) or the pasted format.
    pub fn adjust(&mut self, new_plain: &str, pending: Option<Fmt>, paste: Option<&Rich>) {
        if self.plain == new_plain {
            return;
        }
        let old: Vec<char> = self.plain.chars().collect();
        let new: Vec<char> = new_plain.chars().collect();
        let (m, k) = (old.len(), new.len());
        let mut p = 0;
        while p < m && p < k && old[p] == new[p] {
            p += 1;
        }
        let mut s = 0;
        while s < m - p && s < k - p && old[m - 1 - s] == new[k - 1 - s] {
            s += 1;
        }
        let ins = &new[p..k - s];
        let base = match pending {
            Some(f) => f,
            None => {
                let mut f = if p < m - s {
                    self.fmts[p]
                } else if p > 0 {
                    self.fmts[p - 1]
                } else {
                    self.fmts.get(m - s).copied().unwrap_or_default()
                };
                let keep_link = f.link != 0 && p > 0 && m - s < m && self.fmts[p - 1].link == f.link && self.fmts[m - s].link == f.link;
                if !keep_link {
                    f.link = 0;
                }
                f
            }
        };
        let mut fmts: Vec<Fmt> = self.fmts[..p].to_vec();
        let ins_str: String = ins.iter().collect();
        match paste {
            Some(pr) if !ins.is_empty() && pr.plain == ins_str => {
                for f in &pr.fmts {
                    let mut f = *f;
                    if f.link != 0 {
                        let url = pr.links[f.link as usize - 1].clone();
                        f.link = self.link_id(&url);
                    }
                    fmts.push(f);
                }
            }
            _ => fmts.extend(std::iter::repeat(base).take(ins.len())),
        }
        fmts.extend_from_slice(&self.fmts[m - s..]);
        self.plain = new_plain.to_owned();
        self.fmts = fmts;
    }

    /// Does every character in `a..b` have `flag`? (an empty range looks at the character before the caret).
    pub fn all_have(&self, a: usize, b: usize, flag: u8) -> bool {
        let (a, b) = (a.min(b), a.max(b));
        if a >= b {
            return a > 0 && self.fmts.get(a - 1).map_or(false, |f| f.flags & flag != 0);
        }
        self.fmts[a.min(self.len())..b.min(self.len())].iter().all(|f| f.flags & flag != 0)
    }

    pub fn toggle(&mut self, a: usize, b: usize, flag: u8) -> bool {
        let (a, b) = (a.min(b), a.max(b));
        let on = !self.all_have(a, b, flag);
        let n = self.len();
        for f in &mut self.fmts[a.min(n)..b.min(n)] {
            if on {
                f.flags |= flag;
            } else {
                f.flags &= !flag;
            }
        }
        on
    }

    pub fn set_link(&mut self, a: usize, b: usize, url: Option<&str>) {
        let id = url.map(|u| self.link_id(u)).unwrap_or(0);
        let (a, b) = (a.min(b), a.max(b));
        let n = self.len();
        for f in &mut self.fmts[a.min(n)..b.min(n)] {
            f.link = id;
        }
    }

    /// Link covering the whole range, if any.
    pub fn link_in(&self, a: usize, b: usize) -> Option<String> {
        let (a, b) = (a.min(b), a.max(b));
        let first = self.link_at(a)?.to_owned();
        (a..b.max(a + 1)).all(|i| self.link_at(i) == Some(first.as_str())).then_some(first)
    }

    pub fn slice(&self, a: usize, b: usize) -> Rich {
        let (a, b) = (a.min(b).min(self.len()), a.max(b).min(self.len()));
        let (ba, bb) = (self.byte(a), self.byte(b));
        Rich { plain: self.plain[ba..bb].to_owned(), fmts: self.fmts[a..b].to_vec(), links: self.links.clone() }
    }

    pub fn split(&self, at: usize) -> (Rich, Rich) {
        (self.slice(0, at), self.slice(at, self.len()))
    }

    pub fn append(&mut self, other: &Rich) {
        let map: Vec<u16> = other.links.iter().map(|u| self.link_id(u)).collect();
        self.plain.push_str(&other.plain);
        for f in &other.fmts {
            let mut f = *f;
            if f.link != 0 {
                f.link = map[f.link as usize - 1];
            }
            self.fmts.push(f);
        }
    }

    /// Replaces `a..b` by `with` (used by the context-menu paste).
    pub fn replace(&mut self, a: usize, b: usize, with: &Rich) {
        let (a, b) = (a.min(b).min(self.len()), a.max(b).min(self.len()));
        let mut out = self.slice(0, a);
        out.append(with);
        out.append(&self.slice(b, self.len()));
        *self = out;
    }

    /// Text layout of the rich text (what the user sees). The text of the job is exactly `self.plain`.
    pub fn job(&self, font: &FontId, color: Color32, pal: &Palette) -> LayoutJob {
        let mut job = LayoutJob::default();
        let wikis = wiki_spans(&self.plain);
        for (range, f) in self.runs() {
            // split the run at wiki-link boundaries so `[[Page]]` is drawn like a link
            let mut cuts = vec![range.start];
            for (w, _) in &wikis {
                for e in [w.start, w.end] {
                    if e > range.start && e < range.end {
                        cuts.push(e);
                    }
                }
            }
            cuts.push(range.end);
            cuts.sort_unstable();
            cuts.dedup();
            for seg in cuts.windows(2) {
                let (a, b) = (seg[0], seg[1]);
                let wiki = wikis.iter().any(|(w, _)| a >= w.start && b <= w.end);
                let is_link = f.link != 0 || wiki;
                let code = f.flags & CODE != 0;
                let fg = if is_link {
                    pal.accent
                } else if code {
                    tag_color(5)
                } else {
                    color
                };
                let fmt = TextFormat {
                    font_id: if code {
                        FontId::monospace(font.size * 0.88)
                    } else if f.flags & BOLD != 0 {
                        bold(font.size)
                    } else {
                        font.clone()
                    },
                    color: fg,
                    background: if f.flags & HIGHLIGHT != 0 {
                        Color32::from_rgba_unmultiplied(255, 208, 0, 100)
                    } else if code {
                        pal.hover
                    } else {
                        Color32::TRANSPARENT
                    },
                    italics: f.flags & ITALIC != 0,
                    strikethrough: if f.flags & STRIKE != 0 { Stroke::new(1.3_f32, color) } else { Stroke::NONE },
                    underline: if f.flags & UNDERLINE != 0 || is_link { Stroke::new(1.2_f32, fg) } else { Stroke::NONE },
                    ..Default::default()
                };
                job.append(&self.plain[a..b], 0.0, fmt);
            }
        }
        if job.sections.is_empty() {
            job.append("", 0.0, TextFormat::simple(font.clone(), color));
        }
        job
    }
}

fn mark_of(bit: u8) -> &'static str {
    match bit {
        BOLD => "**",
        ITALIC => "*",
        STRIKE => "~~",
        CODE => "`",
        UNDERLINE => "__",
        _ => "==",
    }
}

fn escape_into(out: &mut String, text: &str, in_link: bool) {
    let ch: Vec<char> = text.chars().collect();
    for (i, &c) in ch.iter().enumerate() {
        let doubled = |d: char| (i > 0 && ch[i - 1] == d) || (i + 1 < ch.len() && ch[i + 1] == d);
        let esc = match c {
            '\\' | '*' | '`' => true,
            // `[[wiki links]]` stay readable in the stored text (the graph, search and the AI look for them)
            '[' => !doubled('['),
            ']' => in_link,
            '_' | '~' | '=' => doubled(c),
            _ => false,
        };
        if esc {
            out.push('\\');
        }
        out.push(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(s: &str) {
        let r = Rich::parse(s);
        assert_eq!(r.markup(), s, "markup round trip of {s:?}");
        assert_eq!(Rich::parse(&r.markup()), r);
    }

    #[test]
    fn markup_round_trips() {
        for s in ["plain", "**bold** text", "a *it* b", "==hl== and __u__", "~~s~~", "x `code` y", "**[link](https://a.b)** z", "***both***", "a \\* b", "a \\_\\_b\\_\\_", "snake_case = 1"] {
            rt(s);
        }
    }

    #[test]
    fn markers_are_hidden_in_plain() {
        let r = Rich::parse("**bold** and ==hot== [x](https://y.z)");
        assert_eq!(r.plain, "bold and hot x");
        assert_eq!(r.fmts[0].flags, BOLD);
        assert_eq!(r.fmts[9].flags, HIGHLIGHT);
        assert_eq!(r.link_at(13), Some("https://y.z"));
    }

    #[test]
    fn literal_specials_survive() {
        let mut r = Rich::plain("2*3*4 and a__b__c");
        r.toggle(0, 1, BOLD);
        let again = Rich::parse(&r.markup());
        assert_eq!(again.plain, "2*3*4 and a__b__c");
        assert_eq!(again.fmts[0].flags, BOLD);
        assert!(again.fmts[1..].iter().all(|f| f.flags == 0));
    }

    #[test]
    fn typing_inherits_format() {
        let mut r = Rich::parse("**ab**cd");
        r.adjust("abXcd", None, None);
        assert_eq!(r.fmts[2].flags, BOLD); // typed at the end of a bold run stays bold
        r.adjust("abXcdY", None, None);
        assert_eq!(r.fmts[5].flags, 0);
        r.adjust("abXcdY!", Some(Fmt { flags: ITALIC, link: 0 }), None);
        assert_eq!(r.fmts[6].flags, ITALIC);
        assert_eq!(r.markup(), "**abX**cdY*!*");
    }

    #[test]
    fn toggle_split_merge() {
        let mut r = Rich::plain("hello world");
        assert!(r.toggle(0, 5, BOLD));
        assert!(r.all_have(0, 5, BOLD));
        let (l, rr) = r.split(3);
        assert_eq!(l.markup(), "**hel**");
        assert_eq!(rr.markup(), "**lo** world");
        let mut m = l.clone();
        m.append(&rr);
        assert_eq!(m.markup(), r.markup());
        assert!(!r.toggle(0, 5, BOLD));
    }
}


/// Gives every section of `job` the baseline of the base font and the row height of the base font.
///
/// **Why:** egui places each glyph at `ascent_of_its_font + valign · (row_height − line_height_of_its_font)`. Monospace (0.88 ×
/// size), bold and fallback fonts have different ascents / line heights than the body font, so as soon as a word was marked up
/// its glyphs sat a pixel or more higher (or lower) than the neighbouring text, and the row height — and with it the whole block —
/// changed. We measure the baseline and row height of every font used by the job with egui itself and give each section the
/// `line_height` that makes its baseline coincide with the body font's. Plain text (all sections in the base font) is untouched.
pub fn align_baselines(fonts: &egui::epaint::text::Fonts, job: &mut LayoutJob, base: &FontId) {
    let mut others: Vec<(FontId, f32)> = vec![];
    for sec in &job.sections {
        let f = &sec.format.font_id;
        if f != base && !others.iter().any(|(k, _)| k == f) {
            others.push((f.clone(), fonts.row_height(base)));
        }
    }
    if others.is_empty() {
        return;
    }
    // Empirical calibration: lay out one "x" per font exactly as the real job will, compare the baselines (egui rounds them to whole
    // pixels) and move each font's `line_height` by the whole-pixel error. Shifting by an integer keeps the rounding stable.
    for _ in 0..3 {
        let mut probe = LayoutJob::default();
        probe.append("x", 0.0, TextFormat::simple(base.clone(), Color32::WHITE));
        for (f, lh) in &others {
            probe.append("x", 0.0, TextFormat { font_id: f.clone(), line_height: Some(*lh), valign: egui::Align::BOTTOM, ..Default::default() });
        }
        let g = fonts.layout_job(probe);
        let ys: Vec<f32> = g.rows.first().map(|r| r.glyphs.iter().map(|x| x.pos.y).collect()).unwrap_or_default();
        if ys.len() != others.len() + 1 {
            return;
        }
        let mut done = true;
        for (i, (_, lh)) in others.iter_mut().enumerate() {
            let err = ys[i + 1] - ys[0];
            if err.abs() > 0.01 {
                *lh += err;
                done = false;
            }
        }
        if done {
            break;
        }
    }
    for sec in job.sections.iter_mut() {
        if let Some((_, lh)) = others.iter().find(|(k, _)| *k == sec.format.font_id) {
            sec.format.valign = egui::Align::BOTTOM;
            sec.format.line_height = Some(*lh);
        }
    }
}

#[cfg(test)]
mod baseline_tests {
    use super::*;
    use egui::{Context, FontFamily};

    /// Marking text up must neither move the baseline of any glyph nor change the height of the row.
    #[test]
    fn marked_text_keeps_baseline_and_row_height() {
        let ctx = Context::default();
        crate::theme::setup_fonts(&ctx);
        ctx.run(Default::default(), |_| {});
        for size in [13.0_f32, 14.0, 15.0, 16.0, 17.6, 20.0, 24.0, 30.0] {
            let base = FontId::new(size, FontFamily::Proportional);
            let build = |align: bool| {
                let mut job = LayoutJob::default();
                for (txt, f) in [("aaa", base.clone()), ("bbb", crate::theme::bold(size)), ("ccc", FontId::monospace(size * 0.88)), ("ddd", base.clone())] {
                    job.append(txt, 0.0, TextFormat { font_id: f, ..Default::default() });
                }
                ctx.fonts(|f| {
                    if align {
                        align_baselines(f, &mut job, &base);
                    }
                    f.layout_job(job)
                })
            };
            let plain = ctx.fonts(|f| f.layout_job(LayoutJob::single_section("aaa".into(), TextFormat::simple(base.clone(), Color32::WHITE))));
            let g = build(true);
            let ys: Vec<f32> = g.rows[0].glyphs.iter().map(|x| x.pos.y).collect();
            assert!(ys.iter().all(|y| (*y - ys[0]).abs() < 0.01), "size {size}: one baseline, got {ys:?}");
            assert!((g.rows[0].rect.height() - plain.rows[0].rect.height()).abs() < 0.01, "size {size}: row height {} vs plain {}", g.rows[0].rect.height(), plain.rows[0].rect.height());
        }
    }
}

#[cfg(test)]
mod wiki_tests {
    use super::*;

    #[test]
    fn wiki_links_inside_text_are_found_styled_and_survive_editing() {
        let text = "see [[Project plan]] and [[Notes|my notes]] later, [[ ]] and [[open";
        assert!(has_marks(text));
        let r = Rich::parse(text);
        assert_eq!(r.plain, text, "brackets stay visible and the text is unchanged");
        assert_eq!(r.markup(), text, "[[links]] are stored exactly as typed");
        let at = text.find("Project").unwrap();
        assert_eq!(r.wiki_at(at), Some("Project plan".to_owned()));
        assert_eq!(r.wiki_at(text.find("my notes").unwrap()), Some("Notes".to_owned()));
        assert_eq!(r.wiki_at(0), None);
        assert_eq!(wiki_spans(text).len(), 2);
        // bold + wiki link together
        let mut b = Rich::plain("a [[B]] c");
        b.toggle(0, 1, BOLD);
        assert_eq!(Rich::parse(&b.markup()).plain, "a [[B]] c");
        assert!(b.markup().contains("[[B]]"));
    }
}

//! PDF → note: one click turns every highlight, underline, sticky note and drawing of a document into a linked note.
//!
//! Every extracted passage carries a *deep link* (`lumenpdf://<doc>/<page>?r=x0,y0,x1,y1&a=<annotation>`). Clicking it in the note
//! re-opens the reader on that page with the annotation selected, so the note is never a dead copy of the source.
//!
//! The reader (`pdf.rs`) has no access to the workspace, so the button only *queues* an [`Extract`]; the app drains the queue once per
//! frame ([`take`]) and builds the page with [`build_note`].

use std::cell::RefCell;
use std::fmt::Write as _;

use crate::auth::now_ms;
use crate::dates;
use crate::model::{AnnotKind, Block, BlockKind, PageKind, PdfAnnot, Workspace};

pub const SCHEME: &str = "lumenpdf://";

/// A request from the reader: "turn the annotations of this document into a note".
pub struct Extract {
    pub key: u64,
    pub title: String,
    pub list: Vec<PdfAnnot>,
}

thread_local! {
    static PENDING: RefCell<Option<Extract>> = const { RefCell::new(None) };
}

pub fn queue(e: Extract) {
    PENDING.with(|p| *p.borrow_mut() = Some(e));
}

pub fn take() -> Option<Extract> {
    PENDING.with(|p| p.borrow_mut().take())
}

/// Where a deep link points.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub key: u64,
    /// Zero-based page.
    pub page: usize,
    pub rect: Option<[f32; 4]>,
    pub annot: u64,
}

/// Bounding box (page fractions) of any annotation kind.
pub fn bbox(a: &PdfAnnot) -> [f32; 4] {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    let mut add = |x0: f32, y0: f32, x1: f32, y1: f32| {
        b[0] = b[0].min(x0.min(x1));
        b[1] = b[1].min(y0.min(y1));
        b[2] = b[2].max(x0.max(x1));
        b[3] = b[3].max(y0.max(y1));
    };
    match a.kind {
        AnnotKind::Ink if !a.pts.is_empty() => a.pts.iter().for_each(|p| add(p[0], p[1], p[0], p[1])),
        _ if !a.quads.is_empty() => a.quads.iter().for_each(|q| add(q[0], q[1], q[2], q[3])),
        _ => add(a.rect[0], a.rect[1], a.rect[2], a.rect[3]),
    }
    b.map(|v| v.clamp(0.0, 1.0))
}

pub fn link(key: u64, a: &PdfAnnot) -> String {
    let b = bbox(a);
    format!(
        "{SCHEME}{key:x}/{}?r={:.4},{:.4},{:.4},{:.4}&a={}",
        a.page + 1,
        b[0],
        b[1],
        b[2],
        b[3],
        a.id
    )
}

pub fn page_link(key: u64, page: u32) -> String {
    format!("{SCHEME}{key:x}/{}", page + 1)
}

pub fn parse(url: &str) -> Option<Target> {
    let rest = url.strip_prefix(SCHEME)?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (key, page) = path.split_once('/')?;
    let key = u64::from_str_radix(key, 16).ok()?;
    let page = page.trim_end_matches('/').parse::<usize>().ok()?.max(1) - 1;
    let (mut rect, mut annot) = (None, 0);
    for kv in query.split('&') {
        match kv.split_once('=') {
            Some(("r", v)) => {
                let n: Vec<f32> = v.split(',').filter_map(|x| x.parse().ok()).collect();
                if n.len() == 4 {
                    rect = Some([n[0], n[1], n[2], n[3]]);
                }
            }
            Some(("a", v)) => annot = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    Some(Target { key, page, rect, annot })
}

/// Collapses whitespace and backslash-escapes the characters the inline parser would take for formatting.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, w) in s.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        for c in w.chars() {
            if matches!(c, '\\' | '*' | '_' | '~' | '=' | '`' | '[' | ']') {
                out.push('\\');
            }
            out.push(c);
        }
    }
    out
}

/// Builds the "Highlights — <title>" page (sorted by page, then by position) and returns its id. `parent` = where it is filed.
pub fn build_note(ws: &mut Workspace, ex: &Extract, parent: Option<u64>) -> u64 {
    let mut list: Vec<&PdfAnnot> = ex.list.iter().collect();
    list.sort_by(|a, b| (a.page, a.rect[1].to_bits(), a.id).cmp(&(b.page, b.rect[1].to_bits(), b.id)));

    let marks = list
        .iter()
        .filter(|a| matches!(a.kind, AnnotKind::Highlight | AnnotKind::Underline | AnnotKind::Strike))
        .count();
    let notes = list.iter().filter(|a| a.kind == AnnotKind::Note).count();
    let inks = list.iter().filter(|a| a.kind == AnnotKind::Ink).count();

    let mut blocks: Vec<Block> = Vec::with_capacity(list.len() * 2 + 4);
    let mut head = format!(
        "Extracted from [{}]({}) on {} — {marks} marked passage(s)",
        esc(&ex.title),
        page_link(ex.key, 0),
        dates::long_today()
    );
    if notes > 0 {
        let _ = write!(head, ", {notes} note(s)");
    }
    if inks > 0 {
        let _ = write!(head, ", {inks} drawing(s)");
    }
    head.push_str(". Click a page link to jump back to the exact spot.");
    blocks.push(ws.blk(BlockKind::Callout, &head, 0));

    if list.is_empty() {
        blocks.push(ws.blk(
            BlockKind::Text,
            "Nothing is marked in this document yet — highlight a passage and extract again.",
            0,
        ));
    }
    let mut cur_page = u32::MAX;
    for a in list {
        if a.page != cur_page {
            cur_page = a.page;
            let h = format!("[Page {}]({})", a.page + 1, page_link(ex.key, a.page));
            blocks.push(ws.blk(BlockKind::H3, &h, 0));
        }
        let url = link(ex.key, a);
        let back = format!("[↩ p. {}]({url})", a.page + 1);
        let quote = a.quote.trim();
        let comment = a.text.trim();
        match a.kind {
            AnnotKind::Highlight | AnnotKind::Underline | AnnotKind::Strike => {
                let body = if quote.is_empty() {
                    "(marked area without text)".to_owned()
                } else {
                    esc(quote)
                };
                blocks.push(ws.blk(BlockKind::Quote, &format!("{body} {back}"), 0));
                if !comment.is_empty() {
                    blocks.push(ws.blk(BlockKind::Bullet, &esc(comment), 1));
                }
            }
            AnnotKind::Note => {
                let body = if comment.is_empty() {
                    "(empty note)".to_owned()
                } else {
                    esc(comment)
                };
                blocks.push(ws.blk(BlockKind::Callout, &format!("💬 {body} {back}"), 0));
            }
            AnnotKind::Ink => {
                let extra = if comment.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", esc(comment))
                };
                blocks.push(ws.blk(BlockKind::Bullet, &format!("✏️ Drawing{extra} {back}"), 0));
            }
        }
    }

    let id = ws.add_page(parent, PageKind::Doc);
    if let Some(p) = ws.page_mut(id) {
        p.title = format!("Highlights — {}", ex.title.trim());
        p.icon = "🖍".into();
        p.blocks = blocks;
        p.updated = now_ms();
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annot(id: u64, page: u32, kind: AnnotKind, quote: &str) -> PdfAnnot {
        PdfAnnot {
            id,
            page,
            kind,
            rect: [0.1, 0.2, 0.6, 0.25],
            pts: vec![],
            color: 0,
            text: String::new(),
            created: 0.0,
            quads: vec![[0.1, 0.2, 0.6, 0.22], [0.1, 0.22, 0.4, 0.25]],
            quote: quote.into(),
        }
    }

    #[test]
    fn deep_links_round_trip() {
        let a = annot(7, 4, AnnotKind::Highlight, "x");
        let t = parse(&link(0xdead_beef, &a)).expect("parses");
        assert_eq!((t.key, t.page, t.annot), (0xdead_beef, 4, 7));
        let r = t.rect.expect("bbox");
        assert!((r[0] - 0.1).abs() < 1e-3 && (r[3] - 0.25).abs() < 1e-3);
        assert_eq!(parse(&page_link(1, 0)).map(|t| t.page), Some(0));
        assert!(parse("https://example.com").is_none());
        assert!(parse("lumenpdf://zz/1").is_none());
    }

    #[test]
    fn note_has_one_link_per_mark_and_escapes_markup() {
        let mut ws = Workspace::seed();
        let mut a = annot(1, 0, AnnotKind::Highlight, "a *bold* [claim]");
        a.text = "check this".into();
        let b = annot(2, 2, AnnotKind::Note, "");
        let ex = Extract {
            key: 9,
            title: "Paper".into(),
            list: vec![b, a],
        };
        let id = build_note(&mut ws, &ex, None);
        let p = ws.page(id).expect("page");
        let links = p.blocks.iter().filter(|b| b.text.contains("?r=")).count();
        assert_eq!(links, 2);
        assert!(p.blocks.iter().any(|b| b.text.contains("a \\*bold\\* \\[claim\\]")));
        let h: Vec<&str> = p
            .blocks
            .iter()
            .filter(|b| b.kind == BlockKind::H3)
            .map(|b| b.text.as_str())
            .collect();
        assert!(h[0].starts_with("[Page 1]") && h[1].starts_with("[Page 3]"));
    }
}

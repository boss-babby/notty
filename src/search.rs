//! Unified full-text search (NEXT-PASS §9): one inverted index over pages (title + blocks), database rows, whiteboard (canvas) node text
//! and — through [`Index::add_pdf`] — PDF pages. Ranking is BM25-lite on lower-cased tokens: title ×3, edited in the last week ×1.2,
//! every query word must match (the last one as a prefix, so results appear while typing). CJK text is tokenised per character.
//!
//! The index is rebuilt lazily when the workspace fingerprint changes ([`cached`]); building is a single pass over the text and costs
//! a few milliseconds per thousand pages.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::model::Workspace;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Page,
    /// A row of a database (opens in the side peek).
    Row,
    Canvas,
    Pdf,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Page => "Page",
            Kind::Row => "Table row",
            Kind::Canvas => "Whiteboard",
            Kind::Pdf => "PDF",
        }
    }
    fn group(self) -> u8 {
        match self {
            Kind::Page => 0,
            Kind::Pdf => 1,
            Kind::Canvas => 2,
            Kind::Row => 3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub page: u64,
    pub kind: Kind,
    pub score: f32,
    pub title: String,
    pub snippet: String,
    /// PDF hits: `lumenpdf://…` deep link.
    pub link: Option<String>,
}

struct Doc {
    page: u64,
    kind: Kind,
    title: String,
    text: String,
    updated: f64,
    len: u32,
    link: Option<String>,
}

#[derive(Default)]
pub struct Index {
    docs: Vec<Doc>,
    postings: HashMap<String, Vec<(u32, u32)>>,
    total_len: u64,
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF)
}

/// Lower-cased words; every CJK character is a token of its own.
pub fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if is_cjk(c) {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            out.push(c.to_string());
        } else if c.is_alphanumeric() {
            cur.extend(c.to_lowercase());
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `lumenpdf://<media-id-hex>/<page>` — same scheme as the PDF → note extraction (`page` is the 0-based page index).
pub fn pdf_link(media: u64, page: u32) -> String {
    crate::extract::page_link(media, page)
}

impl Index {
    pub fn len(&self) -> usize {
        self.docs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    fn push(
        &mut self,
        page: u64,
        kind: Kind,
        title: String,
        text: String,
        updated: f64,
        link: Option<String>,
    ) {
        let id = self.docs.len() as u32;
        let mut tf: HashMap<String, u32> = HashMap::new();
        let mut len = 0u32;
        for t in tokenize(&title) {
            *tf.entry(t).or_default() += 3;
            len += 3;
        }
        for t in tokenize(&text) {
            *tf.entry(t).or_default() += 1;
            len += 1;
        }
        for (term, n) in tf {
            self.postings.entry(term).or_default().push((id, n));
        }
        self.total_len += len as u64;
        self.docs.push(Doc {
            page,
            kind,
            title,
            text,
            updated,
            len: len.max(1),
            link,
        });
    }

    /// Indexes the text of a PDF, one document per page (`pages[0]` = page 1). Called by the reader when it has extracted a file.
    pub fn add_pdf(&mut self, media: u64, title: &str, pages: &[String]) {
        for (i, t) in pages.iter().enumerate() {
            if !t.trim().is_empty() {
                self.push(
                    media,
                    Kind::Pdf,
                    format!("{title} · p. {}", i + 1),
                    t.clone(),
                    0.0,
                    Some(pdf_link(media, i as u32)),
                );
            }
        }
    }

    pub fn build(ws: &Workspace) -> Index {
        let mut ix = Index::default();
        for p in ws.pages.iter().filter(|p| !p.trashed) {
            let mut text = String::new();
            for b in &p.blocks {
                if !b.text.is_empty() {
                    text.push_str(&crate::rich::strip(&b.text));
                    text.push('\n');
                }
            }
            let title = if p.title.is_empty() {
                "Untitled".to_owned()
            } else {
                p.title.clone()
            };
            ix.push(
                p.id,
                if p.row { Kind::Row } else { Kind::Page },
                title.clone(),
                text,
                p.updated,
                None,
            );
            if let Some(c) = &p.canvas {
                let t: Vec<&str> = c
                    .nodes
                    .iter()
                    .map(|n| n.text.as_str())
                    .filter(|t| !t.is_empty())
                    .collect();
                if !t.is_empty() {
                    ix.push(p.id, Kind::Canvas, title, t.join("\n"), p.updated, None);
                }
            }
        }
        ix
    }

    /// Ranked hits for `q` (grouped by kind in the order pages → PDFs → whiteboards → rows when scores tie; best first overall).
    pub fn query(&self, q: &str, now_ms: f64, limit: usize) -> Vec<Hit> {
        let words = tokenize(q);
        if words.is_empty() || self.docs.is_empty() {
            return vec![];
        }
        let n = self.docs.len() as f32;
        let avg = (self.total_len as f32 / n).max(1.0);
        let (k1, b) = (1.2f32, 0.75f32);
        let mut score: HashMap<u32, (f32, usize)> = HashMap::new();
        for (wi, w) in words.iter().enumerate() {
            let last = wi + 1 == words.len();
            let mut seen: HashMap<u32, f32> = HashMap::new();
            let mut add_term = |plist: &Vec<(u32, u32)>, boost: f32| {
                let idf = ((n - plist.len() as f32 + 0.5) / (plist.len() as f32 + 0.5) + 1.0).ln();
                for (d, tf) in plist {
                    let dl = self.docs[*d as usize].len as f32;
                    let tf = *tf as f32;
                    let s = idf * (tf * (k1 + 1.0)) / (tf + k1 * (1.0 - b + b * dl / avg)) * boost;
                    let e = seen.entry(*d).or_default();
                    *e = e.max(s);
                }
            };
            if let Some(p) = self.postings.get(w) {
                add_term(p, 1.0);
            }
            if last {
                let mut taken = 0;
                for (term, p) in &self.postings {
                    if term.len() > w.len() && term.starts_with(w.as_str()) {
                        add_term(p, 0.6);
                        taken += 1;
                        if taken >= 64 {
                            break;
                        }
                    }
                }
            }
            for (d, s) in seen {
                let e = score.entry(d).or_insert((0.0, 0));
                e.0 += s;
                e.1 += 1;
            }
        }
        let week = 7.0 * 86_400_000.0;
        let mut hits: Vec<Hit> = score
            .into_iter()
            .filter(|(_, (_, m))| *m == words.len())
            .map(|(d, (s, _))| {
                let doc = &self.docs[d as usize];
                let recent = if doc.updated > 0.0 && now_ms - doc.updated < week {
                    1.2
                } else {
                    1.0
                };
                Hit {
                    page: doc.page,
                    kind: doc.kind,
                    score: s * recent,
                    title: doc.title.clone(),
                    snippet: snippet(&doc.text, &words[0]),
                    link: doc.link.clone(),
                }
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.kind.group().cmp(&b.kind.group()))
                .then(a.page.cmp(&b.page))
        });
        hits.truncate(limit);
        hits
    }
}

fn snippet(text: &str, word: &str) -> String {
    let lower = text.to_lowercase();
    // `to_lowercase` can change byte lengths; fall back to the start when the offsets do not line up
    let Some(pos) = lower.find(word).filter(|_| lower.len() == text.len()) else {
        return text.lines().next().unwrap_or("").chars().take(70).collect();
    };
    let start = text[..pos].chars().count().saturating_sub(24);
    let s: String = text
        .chars()
        .skip(start)
        .take(80)
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    format!("{}{}", if start > 0 { "…" } else { "" }, s.trim())
}

fn fingerprint(ws: &Workspace) -> u64 {
    let mut h = ws.pages.len() as u64;
    for p in &ws.pages {
        h = h.rotate_left(5)
            ^ p.id.wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ p.updated.to_bits()
            ^ (p.trashed as u64);
    }
    h
}

thread_local! {
    static CACHE: RefCell<Option<(u64, Index)>> = const { RefCell::new(None) };
}

/// Query against a lazily rebuilt index of `ws`.
pub fn cached(ws: &Workspace, q: &str, now_ms: f64, limit: usize) -> Vec<Hit> {
    let fp = fingerprint(ws);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.as_ref().map(|(f, _)| *f) != Some(fp) {
            *c = Some((fp, Index::build(ws)));
        }
        c.as_ref()
            .map(|(_, ix)| ix.query(q, now_ms, limit))
            .unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BlockKind, PageKind};

    #[test]
    fn tokenizer_splits_words_and_cjk_per_character() {
        assert_eq!(tokenize("Hello, World-42!"), vec!["hello", "world", "42"]);
        assert_eq!(tokenize("东京tower"), vec!["东", "京", "tower"]);
        assert!(tokenize("  ,, ").is_empty());
    }

    fn ws_with(pages: &[(&str, &str)]) -> (Workspace, Vec<u64>) {
        let mut ws = Workspace::seed();
        ws.pages.clear();
        let mut ids = vec![];
        for (t, body) in pages {
            let id = ws.add_page(None, PageKind::Doc);
            let b = ws.blk(BlockKind::Text, body, 0);
            let p = ws.page_mut(id).unwrap();
            p.title = (*t).into();
            p.blocks = vec![b];
            ids.push(id);
        }
        (ws, ids)
    }

    #[test]
    fn title_matches_outrank_body_matches() {
        let (ws, ids) = ws_with(&[
            ("Groceries", "milk eggs"),
            (
                "Shopping",
                "buy groceries for the week and some more words to pad this out",
            ),
        ]);
        let hits = Index::build(&ws).query("groceries", 0.0, 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].page, ids[0]);
    }

    #[test]
    fn every_word_must_match_and_the_last_is_a_prefix() {
        let (ws, ids) = ws_with(&[
            ("Plan", "quarterly roadmap review"),
            ("Notes", "roadmap only"),
        ]);
        let ix = Index::build(&ws);
        let hits = ix.query("roadmap rev", 0.0, 10);
        assert_eq!(
            hits.iter().map(|h| h.page).collect::<Vec<_>>(),
            vec![ids[0]]
        );
        assert!(ix.query("zzz", 0.0, 10).is_empty());
        assert!(ix.query("", 0.0, 10).is_empty());
    }

    #[test]
    fn canvas_text_is_searchable() {
        let (mut ws, ids) = ws_with(&[("Board", "")]);
        let mut c = crate::canvas::Canvas::default();
        let mut n = crate::canvas::Node::default();
        n.text = "launch checklist".into();
        c.nodes.push(n);
        ws.page_mut(ids[0]).unwrap().canvas = Some(c);
        let hits = Index::build(&ws).query("checklist", 0.0, 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, Kind::Canvas);
    }

    #[test]
    fn pdf_hits_carry_a_deep_link() {
        let mut ix = Index::default();
        ix.add_pdf(
            0xAB,
            "Paper",
            &["intro text".into(), "".into(), "method of moments".into()],
        );
        let hits = ix.query("moments", 0.0, 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, Kind::Pdf);
        assert_eq!(hits[0].link.as_deref(), Some("lumenpdf://ab/3"));
        assert!(hits[0].title.ends_with("p. 3"));
    }

    #[test]
    fn recent_pages_get_a_boost() {
        let (mut ws, ids) = ws_with(&[("Alpha", "same words here"), ("Beta", "same words here")]);
        let now = 1.0e10;
        ws.page_mut(ids[1]).unwrap().updated = now - 1000.0;
        ws.page_mut(ids[0]).unwrap().updated = 1.0;
        let hits = Index::build(&ws).query("same words", now, 5);
        assert_eq!(hits[0].page, ids[1]);
    }
}

//! Synced blocks and `![[Page#Heading]]` transclusion (NEXT-PASS §4).
//!
//! A [`BlockKind::Synced`] block shows live content of another place:
//! * `link = Some(page)`, `media = block id` → the block with that id *and its children* (a "synced block");
//! * `link = Some(page)`, `media = 0`, `text = "Heading"` → the section under the first heading with that text;
//! * `link = Some(page)`, `media = 0`, empty text → the whole page;
//! * `link = None`, `text = "Page title#Heading"` → resolved by title (typed as `![[Page title#Heading]]`).
//!
//! Resolution follows nested transclusions up to [`MAX_DEPTH`] levels with a visited set, so cycles end in a notice instead of a loop.
//! Markdown export writes the *resolved* text. "Unsync" copies the resolved blocks into the page as ordinary blocks.

use crate::model::{Block, BlockKind, Workspace};

pub const MAX_DEPTH: usize = 4;

/// Result of resolving a synced block.
#[derive(Clone)]
pub enum Resolved {
    Blocks {
        page: u64,
        blocks: Vec<Block>,
    },
    /// The source does not exist (any more).
    Missing,
    /// The transclusion loops back to itself or nests deeper than [`MAX_DEPTH`].
    Cycle,
}

impl PartialEq for Resolved {
    fn eq(&self, o: &Self) -> bool {
        match (self, o) {
            (Resolved::Blocks { page: a, blocks: x }, Resolved::Blocks { page: b, blocks: y }) => {
                a == b && x.iter().map(|b| b.id).eq(y.iter().map(|b| b.id))
            }
            (Resolved::Missing, Resolved::Missing) | (Resolved::Cycle, Resolved::Cycle) => true,
            _ => false,
        }
    }
}

impl std::fmt::Debug for Resolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Resolved::Blocks { page, blocks } => {
                write!(f, "Blocks(page {page}, {} blocks)", blocks.len())
            }
            Resolved::Missing => f.write_str("Missing"),
            Resolved::Cycle => f.write_str("Cycle"),
        }
    }
}

/// Splits `Page title#Heading`.
pub fn split_ref(text: &str) -> (&str, &str) {
    match text.split_once('#') {
        Some((p, h)) => (p.trim(), h.trim()),
        None => (text.trim(), ""),
    }
}

/// `![[…]]` → the inner reference.
pub fn parse_embed(text: &str) -> Option<&str> {
    let t = text.trim();
    let inner = t.strip_prefix("![[")?.strip_suffix("]]")?;
    (!inner.is_empty() && !inner.contains("]]") && !inner.contains('\n')).then_some(inner)
}

fn page_by_title(ws: &Workspace, title: &str) -> Option<u64> {
    let t = title.to_lowercase();
    ws.pages
        .iter()
        .find(|p| !p.trashed && p.title.to_lowercase() == t)
        .map(|p| p.id)
}

/// End (exclusive) of the subtree that starts at `i` (deeper indent follows).
fn subtree_end(blocks: &[Block], i: usize) -> usize {
    let ind = blocks[i].indent;
    let mut j = i + 1;
    while j < blocks.len() && blocks[j].indent > ind {
        j += 1;
    }
    j
}

fn heading_level(k: BlockKind) -> Option<u8> {
    match k {
        BlockKind::H1 => Some(1),
        BlockKind::H2 => Some(2),
        BlockKind::H3 => Some(3),
        _ => None,
    }
}

/// The section under the first heading whose plain text equals `heading` (case-insensitive): everything up to the next heading of the
/// same or a higher level. The heading block itself is included.
pub fn section(blocks: &[Block], heading: &str) -> Option<Vec<Block>> {
    let want = heading.trim().to_lowercase();
    let i = blocks.iter().position(|b| {
        heading_level(b.kind).is_some() && crate::rich::strip(&b.text).trim().to_lowercase() == want
    })?;
    let lvl = heading_level(blocks[i].kind)?;
    let mut j = i + 1;
    while j < blocks.len() && !heading_level(blocks[j].kind).is_some_and(|l| l <= lvl) {
        j += 1;
    }
    Some(blocks[i..j].to_vec())
}

fn resolve_rec(ws: &Workspace, b: &Block, depth: usize, seen: &mut Vec<(u64, u64)>) -> Resolved {
    if depth > MAX_DEPTH {
        return Resolved::Cycle;
    }
    let (title_ref, heading) = if b.link.is_none() {
        split_ref(&b.text)
    } else {
        ("", b.text.trim())
    };
    let Some(src) = b
        .link
        .filter(|l| ws.page(*l).is_some_and(|p| !p.trashed))
        .or_else(|| {
            (b.link.is_none())
                .then(|| page_by_title(ws, title_ref))
                .flatten()
        })
    else {
        return Resolved::Missing;
    };
    let key = (src, b.media);
    if seen.contains(&key) {
        return Resolved::Cycle;
    }
    seen.push(key);
    let Some(page) = ws.page(src) else {
        return Resolved::Missing;
    };
    let picked: Vec<Block> = if b.media != 0 {
        match page.blocks.iter().position(|x| x.id == b.media) {
            Some(i) => page.blocks[i..subtree_end(&page.blocks, i)].to_vec(),
            None => return Resolved::Missing,
        }
    } else if !heading.is_empty() {
        match section(&page.blocks, heading) {
            Some(s) => s,
            None => return Resolved::Missing,
        }
    } else {
        page.blocks.clone()
    };
    // nested transclusions are expanded in place
    let mut out = Vec::with_capacity(picked.len());
    for x in picked {
        if x.kind == BlockKind::Synced {
            match resolve_rec(ws, &x, depth + 1, seen) {
                Resolved::Blocks { blocks, .. } => out.extend(blocks),
                other => return other,
            }
        } else {
            out.push(x);
        }
    }
    seen.pop();
    Resolved::Blocks {
        page: src,
        blocks: out,
    }
}

/// Resolves a synced block against the workspace.
pub fn resolve(ws: &Workspace, b: &Block) -> Resolved {
    let mut seen = Vec::new();
    resolve_rec(ws, b, 0, &mut seen)
}

/// A synced block that mirrors `block` (and its children) of `page`.
pub fn make_copy(ws: &mut Workspace, page: u64, block: u64) -> Option<Block> {
    ws.page(page)?.blocks.iter().find(|b| b.id == block)?;
    let mut b = ws.blk(BlockKind::Synced, "", 0);
    b.link = Some(page);
    b.media = block;
    Some(b)
}

/// Copies of the resolved content with fresh ids and the indent of the synced block (for "Unsync").
pub fn unsynced_blocks(ws: &mut Workspace, b: &Block) -> Vec<Block> {
    let Resolved::Blocks { blocks, .. } = resolve(ws, b) else {
        return vec![];
    };
    let base = blocks.iter().map(|x| x.indent).min().unwrap_or(0);
    blocks
        .into_iter()
        .map(|mut x| {
            x.id = ws.id();
            x.indent = (x.indent - base + b.indent).min(6);
            x
        })
        .collect()
}

/// One-line plain text per resolved block (Markdown export, search, print).
pub fn plain_lines(ws: &Workspace, b: &Block) -> Vec<String> {
    match resolve(ws, b) {
        Resolved::Blocks { blocks, .. } => blocks
            .iter()
            .map(|x| crate::history::plain_line(x))
            .collect(),
        Resolved::Missing => vec!["[synced content not found]".into()],
        Resolved::Cycle => vec!["[synced content loops back to itself]".into()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PageKind;

    fn page(ws: &mut Workspace, title: &str, lines: &[(BlockKind, &str, u8)]) -> u64 {
        let id = ws.add_page(None, PageKind::Doc);
        let blocks: Vec<Block> = lines.iter().map(|(k, t, i)| ws.blk(*k, t, *i)).collect();
        let p = ws.page_mut(id).unwrap();
        p.title = title.into();
        p.blocks = blocks;
        id
    }

    #[test]
    fn embed_syntax_and_heading_resolution() {
        assert_eq!(parse_embed("![[Plan#Goals]]"), Some("Plan#Goals"));
        assert_eq!(parse_embed("[[Plan]]"), None);
        assert_eq!(parse_embed("![[]]"), None);
        assert_eq!(split_ref("Plan # Goals"), ("Plan", "Goals"));
        let mut ws = Workspace::seed();
        let src = page(
            &mut ws,
            "Plan",
            &[
                (BlockKind::H1, "Intro", 0),
                (BlockKind::Text, "hello", 0),
                (BlockKind::H2, "Goals", 0),
                (BlockKind::Bullet, "ship", 0),
                (BlockKind::H2, "Risks", 0),
                (BlockKind::Text, "late", 0),
            ],
        );
        let mut b = ws.blk(BlockKind::Synced, "plan#goals", 0);
        b.link = None;
        match resolve(&ws, &b) {
            Resolved::Blocks { page, blocks } => {
                assert_eq!(page, src);
                assert_eq!(
                    blocks.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(),
                    vec!["Goals", "ship"]
                );
            }
            other => panic!("{other:?}"),
        }
        let whole = ws.blk(BlockKind::Synced, "Plan", 0);
        assert!(
            matches!(resolve(&ws, &whole), Resolved::Blocks { blocks, .. } if blocks.len() == 6)
        );
        let nope = ws.blk(BlockKind::Synced, "Plan#Nope", 0);
        assert_eq!(resolve(&ws, &nope), Resolved::Missing);
    }

    #[test]
    fn synced_copy_follows_edits_of_the_original() {
        let mut ws = Workspace::seed();
        let src = page(
            &mut ws,
            "A",
            &[
                (BlockKind::Callout, "old", 0),
                (BlockKind::Text, "child", 1),
                (BlockKind::Text, "other", 0),
            ],
        );
        let id = ws.page(src).unwrap().blocks[0].id;
        let copy = make_copy(&mut ws, src, id).unwrap();
        assert!(
            matches!(resolve(&ws, &copy), Resolved::Blocks { blocks, .. } if blocks.len() == 2)
        );
        ws.page_mut(src).unwrap().blocks[0].text = "new".into();
        match resolve(&ws, &copy) {
            Resolved::Blocks { blocks, .. } => assert_eq!(blocks[0].text, "new"),
            other => panic!("{other:?}"),
        }
        assert_eq!(plain_lines(&ws, &copy).len(), 2);
        let un = unsynced_blocks(&mut ws, &copy);
        assert_eq!(un.len(), 2);
        assert_ne!(un[0].id, id);
        ws.page_mut(src).unwrap().blocks.clear();
        assert_eq!(resolve(&ws, &copy), Resolved::Missing);
    }

    #[test]
    fn cycles_end_in_a_notice() {
        let mut ws = Workspace::seed();
        let a = page(&mut ws, "A", &[(BlockKind::Synced, "B", 0)]);
        let b = page(&mut ws, "B", &[(BlockKind::Synced, "A", 0)]);
        let _ = b;
        let blk = ws.page(a).unwrap().blocks[0].clone();
        assert_eq!(resolve(&ws, &blk), Resolved::Cycle);
        assert!(plain_lines(&ws, &blk)[0].contains("loops"));
    }
}

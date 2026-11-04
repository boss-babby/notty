//! In-app rich clipboard.
//!
//! The operating-system clipboard only carries text, so a copied block would come back as a Markdown "slab".
//! Whenever we copy blocks (or a formatted fragment of a block) we additionally remember the *exact* data here, together with the
//! text we put on the system clipboard. On paste, if the system clipboard text still matches, the exact blocks / formatting are
//! restored instead of the text.

use std::cell::RefCell;

use crate::model::Block;
use crate::rich::Rich;

#[derive(Clone)]
pub struct BlockClip {
    /// Text that was written to the system clipboard (Markdown); used to recognise our own clip.
    pub text: String,
    /// Blocks with indents normalised so that the shallowest one is 0.
    pub blocks: Vec<Block>,
}

thread_local! {
    static BLOCKS: RefCell<Option<BlockClip>> = const { RefCell::new(None) };
    static RICH: RefCell<Option<Rich>> = const { RefCell::new(None) };
}

fn norm(s: &str) -> String {
    s.replace("\r\n", "\n").trim().to_owned()
}

pub fn set_blocks(text: String, mut blocks: Vec<Block>) {
    let min = blocks.iter().map(|b| b.indent).min().unwrap_or(0);
    for b in &mut blocks {
        b.indent -= min;
    }
    BLOCKS.with(|c| *c.borrow_mut() = Some(BlockClip { text: norm(&text), blocks }));
    RICH.with(|c| *c.borrow_mut() = None);
}

/// The exact blocks behind a pasted text, if it is what we copied last.
pub fn blocks_for(pasted: &str) -> Option<Vec<Block>> {
    let p = norm(pasted);
    BLOCKS.with(|c| c.borrow().as_ref().filter(|b| b.text == p && !p.is_empty()).map(|b| b.blocks.clone()))
}

pub fn set_rich(fragment: Rich) {
    RICH.with(|c| *c.borrow_mut() = Some(fragment));
    BLOCKS.with(|c| *c.borrow_mut() = None);
}

/// The formatted fragment behind a pasted text, if it is what we copied last.
pub fn rich_for(pasted: &str) -> Option<Rich> {
    RICH.with(|c| c.borrow().as_ref().filter(|r| r.plain == pasted.replace("\r\n", "\n") && !pasted.is_empty()).cloned())
}

/// Parses pasted Markdown into blocks (ids are assigned on insertion). `None` when the text has no block-level Markdown.
pub fn markdown_blocks(md: &str) -> Option<Vec<Block>> {
    use crate::model::BlockKind as K;
    let mut out: Vec<Block> = vec![];
    let (mut in_code, mut code, mut structured) = (false, String::new(), false);
    for raw in md.replace("\r\n", "\n").lines() {
        let line = raw.trim_end();
        if line.trim_start().starts_with("```") {
            structured = true;
            if in_code {
                out.push(Block::new(0, K::Code, code.trim_end_matches('\n'), 0));
                code.clear();
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            code.push_str(line);
            code.push('\n');
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let lead = line.len() - line.trim_start().len();
        let ind = (lead / 2).min(6) as u8;
        let t = line.trim_start();
        let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
        let (kind, text, checked, indent): (K, &str, bool, u8) = if let Some(r) = t.strip_prefix("### ").or_else(|| t.strip_prefix("#### ")) {
            (K::H3, r, false, 0)
        } else if let Some(r) = t.strip_prefix("## ") {
            (K::H2, r, false, 0)
        } else if let Some(r) = t.strip_prefix("# ") {
            (K::H1, r, false, 0)
        } else if t == "---" || t == "***" || t == "___" {
            (K::Divider, "", false, 0)
        } else if let Some(r) = t.strip_prefix("- [ ] ").or_else(|| t.strip_prefix("* [ ] ")) {
            (K::Todo, r, false, ind)
        } else if let Some(r) = t.strip_prefix("- [x] ").or_else(|| t.strip_prefix("- [X] ")).or_else(|| t.strip_prefix("* [x] ")) {
            (K::Todo, r, true, ind)
        } else if let Some(r) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("+ ")) {
            (K::Bullet, r, false, ind)
        } else if digits > 0 && t[digits..].starts_with(". ") {
            (K::Numbered, &t[digits + 2..], false, ind)
        } else if let Some(r) = t.strip_prefix("> ") {
            (K::Quote, r, false, 0)
        } else {
            (K::Text, t, false, 0)
        };
        if kind != K::Text {
            structured = true;
        }
        let mut b = Block::new(0, kind, text, indent);
        b.checked = checked;
        out.push(b);
    }
    if in_code && !code.is_empty() {
        out.push(Block::new(0, K::Code, code.trim_end_matches('\n'), 0));
    }
    (structured && !out.is_empty()).then_some(out)
}

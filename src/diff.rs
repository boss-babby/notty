//! Block / word diff for the page-history viewer (NEXT-PASS §7).
//!
//! * [`diff_blocks`] — LCS over `(kind, indent, text)` keys. Common prefix / suffix are trimmed first, so a typical edit (one changed
//!   paragraph in a long page) costs almost nothing; the O(n·m) table is only built for the changed middle and is capped at
//!   [`MAX_CELLS`] (above that the middle is reported as "everything deleted, everything inserted").
//! * [`word_diff`] — token LCS inside one changed block (whitespace-separated words, whitespace kept).
//! * [`rows`] — groups ops for display: a delete directly followed by an insert becomes one *changed* row, long unchanged runs collapse
//!   into "… N unchanged blocks".
//! * [`view`] — the two-column (stacked on phones) renderer used by `history::dialog`.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, Frame, Margin, RichText, Ui};

use crate::history::plain_line;
use crate::model::{Block, BlockKind};
use crate::theme::{body, bold, Palette};
use crate::widgets::with_alpha;

/// Largest LCS table (cells) that is ever built.
pub const MAX_CELLS: usize = 4_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    /// `(old index, new index)` — identical block.
    Equal(usize, usize),
    /// Block of the old list that is gone.
    Delete(usize),
    /// Block of the new list that is new.
    Insert(usize),
}

fn key(b: &Block) -> String {
    format!("{}|{}|{}", b.kind.label(), b.indent, plain_line(b))
}

/// Generic LCS diff over comparable keys.
pub fn diff_keys<T: PartialEq>(old: &[T], new: &[T]) -> Vec<Op> {
    let mut pre = 0;
    while pre < old.len() && pre < new.len() && old[pre] == new[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < old.len() - pre
        && suf < new.len() - pre
        && old[old.len() - 1 - suf] == new[new.len() - 1 - suf]
    {
        suf += 1;
    }
    let a = &old[pre..old.len() - suf];
    let b = &new[pre..new.len() - suf];
    let mut ops: Vec<Op> = (0..pre).map(|i| Op::Equal(i, i)).collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 || n.saturating_mul(m) > MAX_CELLS {
        ops.extend((0..n).map(|i| Op::Delete(pre + i)));
        ops.extend((0..m).map(|j| Op::Insert(pre + j)));
    } else {
        // t[i][j] = LCS length of a[i..] and b[j..]
        let w = m + 1;
        let mut t = vec![0u32; (n + 1) * w];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                t[i * w + j] = if a[i] == b[j] {
                    t[(i + 1) * w + j + 1] + 1
                } else {
                    t[(i + 1) * w + j].max(t[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        let mut run_start = ops.len();
        while i < n || j < m {
            if i < n && j < m && a[i] == b[j] {
                // within a changed run: deletions first, then insertions (stable pairing for `rows`)
                ops[run_start..].sort_by_key(|o| matches!(o, Op::Insert(_)));
                ops.push(Op::Equal(pre + i, pre + j));
                run_start = ops.len();
                i += 1;
                j += 1;
            } else if j >= m || (i < n && t[(i + 1) * w + j] >= t[i * w + j + 1]) {
                ops.push(Op::Delete(pre + i));
                i += 1;
            } else {
                ops.push(Op::Insert(pre + j));
                j += 1;
            }
        }
        ops[run_start..].sort_by_key(|o| matches!(o, Op::Insert(_)));
    }
    for s in 0..suf {
        ops.push(Op::Equal(old.len() - suf + s, new.len() - suf + s));
    }
    ops
}

/// Block-level diff of two versions of a page.
pub fn diff_blocks(old: &[Block], new: &[Block]) -> Vec<Op> {
    let a: Vec<String> = old.iter().map(key).collect();
    let b: Vec<String> = new.iter().map(key).collect();
    diff_keys(&a, &b)
}

/// One run of text in a word diff.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Seg {
    pub text: String,
    pub changed: bool,
}

fn tokens(s: &str) -> Vec<&str> {
    let mut v = Vec::new();
    let mut start = 0;
    let mut prev_ws: Option<bool> = None;
    for (i, c) in s.char_indices() {
        let ws = c.is_whitespace();
        if prev_ws.is_some_and(|p| p != ws) {
            v.push(&s[start..i]);
            start = i;
        }
        prev_ws = Some(ws);
    }
    if start < s.len() {
        v.push(&s[start..]);
    }
    v
}

fn push_seg(v: &mut Vec<Seg>, text: &str, changed: bool) {
    match v.last_mut() {
        Some(l) if l.changed == changed => l.text.push_str(text),
        _ => v.push(Seg {
            text: text.to_owned(),
            changed,
        }),
    }
}

/// Word-level diff: `(old side, new side)`; `changed` segments are the words that were removed / added.
pub fn word_diff(old: &str, new: &str) -> (Vec<Seg>, Vec<Seg>) {
    let a = tokens(old);
    let b = tokens(new);
    let (mut l, mut r) = (Vec::new(), Vec::new());
    for op in diff_keys(&a, &b) {
        match op {
            Op::Equal(i, j) => {
                push_seg(&mut l, a[i], false);
                push_seg(&mut r, b[j], false);
            }
            Op::Delete(i) => push_seg(&mut l, a[i], true),
            Op::Insert(j) => push_seg(&mut r, b[j], true),
        }
    }
    (l, r)
}

/// Display rows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    Same(usize, usize),
    /// `n` unchanged blocks collapsed.
    Skip(usize),
    Del(usize),
    Ins(usize),
    /// A block that was edited: `(old, new)`.
    Chg(usize, usize),
}

/// Groups ops for display. Unchanged runs longer than `2 * ctx + 1` keep `ctx` blocks next to a change and collapse the rest.
pub fn rows(ops: &[Op], ctx: usize) -> Vec<Row> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < ops.len() {
        if let Op::Equal(..) = ops[i] {
            let s = i;
            while i < ops.len() && matches!(ops[i], Op::Equal(..)) {
                i += 1;
            }
            let (at_start, at_end) = (s == 0, i == ops.len());
            let run = i - s;
            let (head, tail) = (if at_start { 0 } else { ctx }, if at_end { 0 } else { ctx });
            let eq = |k: usize| match ops[k] {
                Op::Equal(a, b) => Row::Same(a, b),
                _ => Row::Skip(0),
            };
            if run <= head + tail + 1 {
                out.extend((s..i).map(eq));
            } else {
                out.extend((s..s + head).map(eq));
                out.push(Row::Skip(run - head - tail));
                out.extend((i - tail..i).map(eq));
            }
        } else {
            let ds = i;
            while i < ops.len() && matches!(ops[i], Op::Delete(_)) {
                i += 1;
            }
            let is = i;
            while i < ops.len() && matches!(ops[i], Op::Insert(_)) {
                i += 1;
            }
            let dels: Vec<usize> = ops[ds..is]
                .iter()
                .filter_map(|o| {
                    if let Op::Delete(x) = o {
                        Some(*x)
                    } else {
                        None
                    }
                })
                .collect();
            let inss: Vec<usize> = ops[is..i]
                .iter()
                .filter_map(|o| {
                    if let Op::Insert(x) = o {
                        Some(*x)
                    } else {
                        None
                    }
                })
                .collect();
            let paired = dels.len().min(inss.len());
            for k in 0..paired {
                out.push(Row::Chg(dels[k], inss[k]));
            }
            out.extend(dels[paired..].iter().map(|d| Row::Del(*d)));
            out.extend(inss[paired..].iter().map(|n| Row::Ins(*n)));
        }
    }
    out
}

/// `(inserted, deleted, edited)` block counts.
pub fn stats(rows: &[Row]) -> (usize, usize, usize) {
    rows.iter().fold((0, 0, 0), |(i, d, c), r| match r {
        Row::Ins(_) => (i + 1, d, c),
        Row::Del(_) => (i, d + 1, c),
        Row::Chg(..) => (i, d, c + 1),
        _ => (i, d, c),
    })
}

fn font_for(b: &Block) -> egui::FontId {
    match b.kind {
        BlockKind::H1 | BlockKind::H2 | BlockKind::H3 => bold(14.5),
        BlockKind::Code => egui::FontId::monospace(12.5),
        _ => body(13.0),
    }
}

fn job(b: &Block, segs: Option<&[Seg]>, tint: Color32, pal: &Palette) -> LayoutJob {
    let font = font_for(b);
    let mut j = LayoutJob::default();
    let line = plain_line(b);
    let fmt = |changed: bool| TextFormat {
        font_id: font.clone(),
        color: pal.text,
        background: if changed {
            with_alpha(tint, 0.45)
        } else {
            Color32::TRANSPARENT
        },
        ..Default::default()
    };
    match segs {
        Some(segs) => {
            for s in segs {
                j.append(&s.text, 0.0, fmt(s.changed));
            }
        }
        None => j.append(
            if line.trim().is_empty() { " " } else { &line },
            0.0,
            fmt(false),
        ),
    }
    j
}

fn cell(ui: &mut Ui, width: f32, fill: Color32, j: Option<LayoutJob>) {
    Frame::new()
        .fill(fill)
        .corner_radius(4.0)
        .inner_margin(Margin::symmetric(6, 3))
        .show(ui, |ui| {
            ui.set_width((width - 12.0).max(40.0));
            if let Some(j) = j {
                ui.add(egui::Label::new(j).wrap());
            } else {
                ui.label(" ");
            }
        });
}

/// Two columns (old | new) on wide screens, stacked on phones. Long unchanged runs collapse; at most 600 rows are drawn.
pub fn view(ui: &mut Ui, pal: &Palette, old: &[Block], new: &[Block], narrow: bool) {
    let ops = diff_blocks(old, new);
    let rs = rows(&ops, 2);
    let green = Color32::from_rgb(76, 175, 120);
    let del = with_alpha(pal.danger, 0.16);
    let ins = with_alpha(green, 0.18);
    let none = Color32::TRANSPARENT;
    let total = ui.available_width();
    let w = if narrow {
        total
    } else {
        ((total - 10.0) / 2.0).max(120.0)
    };
    if !narrow {
        ui.horizontal(|ui| {
            ui.add_sized(
                [w, 18.0],
                egui::Label::new(
                    RichText::new("Selected version")
                        .font(bold(12.0))
                        .color(pal.muted),
                ),
            );
            ui.add_sized(
                [w, 18.0],
                egui::Label::new(
                    RichText::new("Current page")
                        .font(bold(12.0))
                        .color(pal.muted),
                ),
            );
        });
    }
    for (n, r) in rs.iter().take(600).enumerate() {
        let skip_note = |ui: &mut Ui, k: usize| {
            ui.label(
                RichText::new(format!(
                    "… {k} unchanged block{}",
                    if k == 1 { "" } else { "s" }
                ))
                .font(body(12.0))
                .color(pal.muted),
            );
        };
        ui.push_id(n, |ui| {
            if let Row::Skip(k) = *r {
                skip_note(ui, k);
            } else if narrow {
                match *r {
                    Row::Same(a, _) => cell(ui, w, none, Some(job(&old[a], None, none, pal))),
                    Row::Del(a) => cell(ui, w, del, Some(job(&old[a], None, none, pal))),
                    Row::Ins(b) => cell(ui, w, ins, Some(job(&new[b], None, none, pal))),
                    Row::Chg(a, b) => {
                        let (l, rr) = word_diff(&plain_line(&old[a]), &plain_line(&new[b]));
                        cell(ui, w, del, Some(job(&old[a], Some(&l), pal.danger, pal)));
                        cell(ui, w, ins, Some(job(&new[b], Some(&rr), green, pal)));
                    }
                    Row::Skip(_) => {}
                }
            } else {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    match *r {
                        Row::Same(a, b) => {
                            cell(ui, w, none, Some(job(&old[a], None, none, pal)));
                            cell(ui, w, none, Some(job(&new[b], None, none, pal)));
                        }
                        Row::Del(a) => {
                            cell(ui, w, del, Some(job(&old[a], None, none, pal)));
                            cell(ui, w, none, None);
                        }
                        Row::Ins(b) => {
                            cell(ui, w, none, None);
                            cell(ui, w, ins, Some(job(&new[b], None, none, pal)));
                        }
                        Row::Chg(a, b) => {
                            let (l, rr) = word_diff(&plain_line(&old[a]), &plain_line(&new[b]));
                            cell(ui, w, del, Some(job(&old[a], Some(&l), pal.danger, pal)));
                            cell(ui, w, ins, Some(job(&new[b], Some(&rr), green, pal)));
                        }
                        Row::Skip(_) => {}
                    }
                });
            }
        });
        ui.add_space(2.0);
    }
    if rs.len() > 600 {
        ui.label(
            RichText::new(format!("… {} more rows not shown", rs.len() - 600))
                .font(body(12.0))
                .color(pal.muted),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Workspace;

    fn blocks(ws: &mut Workspace, lines: &[&str]) -> Vec<Block> {
        lines
            .iter()
            .map(|t| ws.blk(BlockKind::Text, t, 0))
            .collect()
    }

    #[test]
    fn identical_pages_have_no_changes() {
        let mut ws = Workspace::seed();
        let a = blocks(&mut ws, &["one", "two", "three"]);
        let ops = diff_blocks(&a, &a);
        assert!(ops.iter().all(|o| matches!(o, Op::Equal(..))));
        assert_eq!(stats(&rows(&ops, 2)), (0, 0, 0));
    }

    #[test]
    fn one_inserted_block_is_exactly_one_insert() {
        let mut ws = Workspace::seed();
        let a = blocks(&mut ws, &["one", "two", "three"]);
        let b = blocks(&mut ws, &["one", "two", "new", "three"]);
        let ops = diff_blocks(&a, &b);
        assert_eq!(ops.iter().filter(|o| matches!(o, Op::Insert(_))).count(), 1);
        assert_eq!(ops.iter().filter(|o| matches!(o, Op::Delete(_))).count(), 0);
        assert!(ops.contains(&Op::Insert(2)));
    }

    #[test]
    fn a_moved_block_is_a_delete_plus_an_insert() {
        let mut ws = Workspace::seed();
        let a = blocks(&mut ws, &["a", "b", "c", "d"]);
        let b = blocks(&mut ws, &["b", "c", "d", "a"]);
        let ops = diff_blocks(&a, &b);
        assert_eq!(ops.iter().filter(|o| matches!(o, Op::Insert(_))).count(), 1);
        assert_eq!(ops.iter().filter(|o| matches!(o, Op::Delete(_))).count(), 1);
    }

    #[test]
    fn edited_block_becomes_one_changed_row_with_word_diff() {
        let mut ws = Workspace::seed();
        let a = blocks(&mut ws, &["keep", "the quick brown fox", "end"]);
        let b = blocks(&mut ws, &["keep", "the slow brown fox", "end"]);
        let rs = rows(&diff_blocks(&a, &b), 2);
        assert_eq!(stats(&rs), (0, 0, 1));
        let (l, r) = word_diff("the quick brown fox", "the slow brown fox");
        assert!(l.iter().any(|s| s.changed && s.text == "quick"));
        assert!(r.iter().any(|s| s.changed && s.text == "slow"));
        assert_eq!(
            l.iter().map(|s| s.text.as_str()).collect::<String>(),
            "the quick brown fox"
        );
    }

    #[test]
    fn long_unchanged_runs_collapse() {
        let mut ws = Workspace::seed();
        let lines: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let a = blocks(&mut ws, &refs);
        let mut b = a.clone();
        b[20].text = "changed".into();
        let rs = rows(&diff_blocks(&a, &b), 2);
        assert!(rs.iter().any(|r| matches!(r, Row::Skip(k) if *k > 10)));
        assert!(rs.len() < 14);
    }

    #[test]
    fn huge_middles_fall_back_without_a_table() {
        let a: Vec<u32> = (0..3000).collect();
        let b: Vec<u32> = (10_000..13_000).collect();
        let ops = diff_keys(&a, &b);
        assert_eq!(ops.len(), 6000);
    }
}

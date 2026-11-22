//! Review of what other people changed — shown like a code review: red `−` lines for what was there, green `+` lines for what is
//! there now, with the changed words highlighted inside a changed line.
//!
//! * The log is filled by [`crate::collab::crdt::Replica::apply`] (remote operations only) while tracking is on: always on a host,
//!   and on a joiner after the first full sync — or from the very start when the joiner re-opened a workspace it had before
//!   (so the list is exactly "what happened while you were away").
//! * Memory: one entry per touched block (before/after text, each ≤ 8 KiB), at most [`MAX_CHANGES`] entries, later edits of the same
//!   block are folded into its entry (first `before`, last `after`). Nothing is kept for untouched content.
//! * [`diff`] is a small, allocation-conscious Myers-free LCS diff: common prefix/suffix are trimmed first (typing is almost always a
//!   one-hunk edit), the middle is an LCS over lines (≤ 400) or words (≤ 600) and falls back to "replace" beyond that.
use super::ui::{button, glyph_button};
use super::*;
use crate::lucide as l;
use crate::theme::{body, bold, Palette};
use crate::widgets::with_alpha;
use egui::{vec2, Align2, Color32, Context, Id, Sense};

pub const MAX_CHANGES: usize = 1000;
const MAX_TEXT: usize = 8 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Added,
    Edited,
    Removed,
}

#[derive(Clone, Debug)]
pub struct Change {
    pub kind: Kind,
    /// "Page", "Block", "Comment".
    pub what: &'static str,
    pub page: u64,
    pub block: u64,
    pub title: String,
    pub who: u32,
    pub at: u64,
    pub before: String,
    pub after: String,
}

#[derive(Default)]
pub struct Log {
    pub list: Vec<Change>,
    /// Entries that were dropped because the log is full.
    pub dropped: usize,
    /// Edits to canvas cards / edges / annotations (counted, not itemised).
    pub other: usize,
}

fn cut(s: &str) -> String {
    if s.len() <= MAX_TEXT {
        return s.to_owned();
    }
    let mut n = MAX_TEXT;
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    format!("{}…", &s[..n])
}

impl Log {
    pub fn clear(&mut self) {
        self.list.clear();
        self.dropped = 0;
        self.other = 0;
    }

    pub fn len(&self) -> usize {
        self.list.len() + self.other
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty() && self.other == 0
    }

    /// Records one remote change, folding it into an earlier entry of the same thing.
    pub fn push(&mut self, mut c: Change) {
        c.before = cut(&c.before);
        c.after = cut(&c.after);
        if let Some(i) = self.list.iter().rposition(|x| x.what == c.what && x.page == c.page && x.block == c.block) {
            let prev = &mut self.list[i];
            let kind = match (prev.kind, c.kind) {
                (Kind::Added, Kind::Removed) => {
                    self.list.remove(i); // created and deleted again: nothing to review
                    return;
                }
                (Kind::Added, _) => Kind::Added,
                (Kind::Removed, Kind::Added) | (Kind::Removed, Kind::Edited) => Kind::Edited,
                (Kind::Edited, Kind::Removed) => Kind::Removed,
                (_, k) => k,
            };
            prev.kind = kind;
            prev.after = c.after;
            prev.who = c.who;
            prev.at = c.at;
            if !c.title.is_empty() {
                prev.title = c.title;
            }
            if prev.kind == Kind::Edited && prev.before == prev.after {
                self.list.remove(i);
            }
            return;
        }
        if self.list.len() >= MAX_CHANGES {
            self.list.remove(0);
            self.dropped += 1;
        }
        self.list.push(c);
    }
}

// ───────────── diff ─────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tag {
    Same,
    Del,
    Ins,
}

/// Generic diff of two token slices. Returns (tag, token index in `a` for Same/Del or in `b` for Ins).
fn diff_tokens<T: PartialEq>(a: &[T], b: &[T], cap: usize) -> Vec<(Tag, usize)> {
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf] {
        suf += 1;
    }
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let mut out: Vec<(Tag, usize)> = (0..pre).map(|i| (Tag::Same, i)).collect();
    if am.len() > cap || bm.len() > cap || am.len().saturating_mul(bm.len()) > cap * cap {
        out.extend((0..am.len()).map(|i| (Tag::Del, pre + i)));
        out.extend((0..bm.len()).map(|i| (Tag::Ins, pre + i)));
    } else {
        // LCS table over the differing middle only
        let (n, m) = (am.len(), bm.len());
        let mut t = vec![0u16; (n + 1) * (m + 1)];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                t[i * (m + 1) + j] = if am[i] == bm[j] { t[(i + 1) * (m + 1) + j + 1] + 1 } else { t[(i + 1) * (m + 1) + j].max(t[i * (m + 1) + j + 1]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && am[i] == bm[j] {
                out.push((Tag::Same, pre + i));
                i += 1;
                j += 1;
            } else if j < m && (i == n || t[i * (m + 1) + j + 1] >= t[(i + 1) * (m + 1) + j]) {
                out.push((Tag::Ins, pre + j));
                j += 1;
            } else {
                out.push((Tag::Del, pre + i));
                i += 1;
            }
        }
    }
    out.extend((0..suf).map(|k| (Tag::Same, a.len() - suf + k)));
    out
}

/// Line diff: `(tag, line)`; for `Ins` the line comes from `b`, otherwise from `a`.
pub fn line_diff<'a>(a: &'a str, b: &'a str) -> Vec<(Tag, &'a str)> {
    let al: Vec<&str> = a.split('\n').collect();
    let bl: Vec<&str> = b.split('\n').collect();
    diff_tokens(&al, &bl, 400).into_iter().map(|(t, i)| if t == Tag::Ins { (t, bl[i]) } else { (t, al[i]) }).collect()
}

/// Splits into words and the white space between them (so the pieces concatenate to the original).
fn tokens(s: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut start = 0;
    let mut prev: Option<bool> = None;
    for (i, ch) in s.char_indices() {
        let ws = ch.is_whitespace();
        if prev.map_or(false, |p| p != ws) || (!ws && !ch.is_alphanumeric() && prev.is_some()) {
            out.push(&s[start..i]);
            start = i;
        }
        prev = Some(ws);
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Word-level segments of an edit: `(changed, text)` for the old line (deletions marked) and for the new line (insertions marked).
pub fn word_diff(a: &str, b: &str) -> (Vec<(bool, String)>, Vec<(bool, String)>) {
    let (ta, tb) = (tokens(a), tokens(b));
    let (mut old, mut new): (Vec<(bool, String)>, Vec<(bool, String)>) = (vec![], vec![]);
    for (t, i) in diff_tokens(&ta, &tb, 600) {
        match t {
            Tag::Same => {
                push_seg(&mut old, false, ta[i]);
                push_seg(&mut new, false, ta[i]);
            }
            Tag::Del => push_seg(&mut old, true, ta[i]),
            Tag::Ins => push_seg(&mut new, true, tb[i]),
        }
    }
    (old, new)
}

fn push_seg(v: &mut Vec<(bool, String)>, hl: bool, s: &str) {
    match v.last_mut() {
        Some((h, t)) if *h == hl => t.push_str(s),
        _ => v.push((hl, s.to_owned())),
    }
}

// ───────────── window ─────────────

fn job(pal: &Palette, segs: &[(bool, String)], hl: Color32, size: f32) -> egui::text::LayoutJob {
    let mut j = egui::text::LayoutJob::default();
    j.wrap.max_width = f32::INFINITY;
    for (changed, t) in segs {
        j.append(
            t,
            0.0,
            egui::TextFormat { font_id: body(size), color: pal.text, background: if *changed { hl } else { Color32::TRANSPARENT }, ..Default::default() },
        );
    }
    j
}

fn row(ui: &mut egui::Ui, pal: &Palette, sign: &str, bg: Color32, strong: Color32, segs: &[(bool, String)]) {
    egui::Frame::new().fill(bg).inner_margin(egui::Margin::symmetric(6, 2)).corner_radius(egui::CornerRadius::same(3)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            ui.label(egui::RichText::new(sign).font(egui::FontId::monospace(12.5)).color(strong));
            let mut j = job(pal, segs, with_alpha(strong, 0.35), 12.5);
            j.wrap.max_width = ui.available_width();
            ui.add(egui::Label::new(j).wrap());
        });
    });
}

fn paint_change(ui: &mut egui::Ui, pal: &Palette, ch: &Change) {
    let (red, green) = (Color32::from_rgb(224, 82, 82), Color32::from_rgb(64, 168, 96));
    let (bg_red, bg_green) = (with_alpha(red, 0.12), with_alpha(green, 0.12));
    let one = |s: &str| vec![(false, s.to_owned())];
    match ch.kind {
        Kind::Added => {
            for line in ch.after.split('\n') {
                row(ui, pal, "+", bg_green, green, &one(line));
            }
        }
        Kind::Removed => {
            for line in ch.before.split('\n') {
                row(ui, pal, "−", bg_red, red, &one(line));
            }
        }
        Kind::Edited => {
            let ops = line_diff(&ch.before, &ch.after);
            let mut i = 0;
            let mut shown = 0;
            while i < ops.len() && shown < 60 {
                match ops[i].0 {
                    Tag::Same => {
                        // only the unchanged lines next to a change are shown (like a unified diff with 1 line of context)
                        let near = (i > 0 && ops[i - 1].0 != Tag::Same) || ops.get(i + 1).map_or(false, |n| n.0 != Tag::Same);
                        if near {
                            row(ui, pal, " ", Color32::TRANSPARENT, pal.muted, &one(ops[i].1));
                            shown += 1;
                        }
                        i += 1;
                    }
                    _ => {
                        let s = i;
                        while i < ops.len() && ops[i].0 != Tag::Same {
                            i += 1;
                        }
                        let dels: Vec<&str> = ops[s..i].iter().filter(|o| o.0 == Tag::Del).map(|o| o.1).collect();
                        let inss: Vec<&str> = ops[s..i].iter().filter(|o| o.0 == Tag::Ins).map(|o| o.1).collect();
                        if dels.len() == 1 && inss.len() == 1 {
                            let (o, n) = word_diff(dels[0], inss[0]);
                            row(ui, pal, "−", bg_red, red, &o);
                            row(ui, pal, "+", bg_green, green, &n);
                            shown += 2;
                        } else {
                            for d in &dels {
                                row(ui, pal, "−", bg_red, red, &one(d));
                            }
                            for n in &inss {
                                row(ui, pal, "+", bg_green, green, &one(n));
                            }
                            shown += dels.len() + inss.len();
                        }
                    }
                }
            }
        }
    }
}

fn ago(at: u64) -> String {
    let s = now_ms().saturating_sub(at) / 1000;
    match s {
        0..=4 => "just now".into(),
        5..=59 => format!("{s}s ago"),
        60..=3599 => format!("{} min ago", s / 60),
        _ => format!("{} h ago", s / 3600),
    }
}

/// The review window. Returns the page the user asked to open.
pub fn window(ctx: &Context, pal: &Palette, c: &mut Collab) -> Option<u64> {
    let screen = ctx.screen_rect();
    let w = 460.0_f32.min(screen.width() - 16.0);
    let mut open = None;
    let mut close = false;
    let mut clear = false;
    let names: HashMap<u32, String> = c.roster.iter().map(|(p, _)| (p.id, p.name.clone())).collect();
    egui::Window::new("changes_win")
        .id(Id::new("changes_win"))
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::RIGHT_TOP, vec2(-8.0, 52.0))
        .frame(crate::widgets::popup_frame(pal).corner_radius(egui::CornerRadius::same(14)).inner_margin(egui::Margin::same(12)))
        .show(ctx, |ui| {
            ui.set_width(w - 24.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(l::HISTORY).font(body(15.0)).color(pal.accent));
                ui.label(egui::RichText::new("Changes").font(bold(15.0)).color(pal.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if glyph_button(ui, pal, l::X, "Close", None).clicked() {
                        close = true;
                    }
                    if !c.changes.is_empty() && button(ui, pal, "Mark all reviewed", 3, 132.0).clicked() {
                        clear = true;
                    }
                });
            });
            if c.changes.is_empty() {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(
                        "Nothing new. Edits other people make appear here as they happen — what was there (red) and what is there now (green).",
                    )
                    .font(body(12.5))
                    .color(pal.muted),
                );
                return;
            }
            let mut sub = format!("{} change{}", c.changes.list.len(), if c.changes.list.len() == 1 { "" } else { "s" });
            if c.changes.other > 0 {
                sub.push_str(&format!(" · {} on canvases / annotations", c.changes.other));
            }
            if c.changes.dropped > 0 {
                sub.push_str(&format!(" · {} older not shown", c.changes.dropped));
            }
            ui.label(egui::RichText::new(sub).font(body(12.0)).color(pal.muted));
            ui.add_space(4.0);
            let h = (screen.height() - 150.0).clamp(160.0, 560.0);
            egui::ScrollArea::vertical().id_salt("changes_list").max_height(h).auto_shrink([false, true]).show(ui, |ui| {
                // newest first, grouped by page
                let mut order: Vec<usize> = (0..c.changes.list.len()).collect();
                order.sort_by(|a, b| c.changes.list[*b].at.cmp(&c.changes.list[*a].at));
                let mut last_page = u64::MAX;
                for ix in order.into_iter().take(150) {
                    let ch = &c.changes.list[ix];
                    if ch.page != last_page {
                        last_page = ch.page;
                        ui.add_space(6.0);
                        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
                        ui.painter().text(
                            r.left_center(),
                            Align2::LEFT_CENTER,
                            format!("{}  {}", l::FILE_TEXT, if ch.title.is_empty() { "Untitled" } else { &ch.title }),
                            bold(13.0),
                            pal.text,
                        );
                        if resp.on_hover_text("Open this page").clicked() {
                            open = Some(ch.page);
                        }
                    }
                    let (mark, col) = match ch.kind {
                        Kind::Added => ("added", Color32::from_rgb(64, 168, 96)),
                        Kind::Edited => ("edited", Color32::from_rgb(235, 160, 40)),
                        Kind::Removed => ("removed", Color32::from_rgb(224, 82, 82)),
                    };
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("{} {mark}", ch.what)).font(bold(11.5)).color(col));
                        let who = names.get(&ch.who).cloned().unwrap_or_else(|| "Someone".into());
                        ui.label(egui::RichText::new(format!("{who} · {}", ago(ch.at))).font(body(11.5)).color(pal.muted));
                    });
                    paint_change(ui, pal, ch);
                    ui.add_space(4.0);
                }
            });
        });
    if clear {
        c.changes.clear();
    }
    if close {
        c.ui.changes = false;
    }
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_diff_marks_only_the_changed_line() {
        let d = line_diff("a\nb\nc", "a\nB\nc");
        assert_eq!(d.iter().filter(|x| x.0 == Tag::Del).map(|x| x.1).collect::<Vec<_>>(), vec!["b"]);
        assert_eq!(d.iter().filter(|x| x.0 == Tag::Ins).map(|x| x.1).collect::<Vec<_>>(), vec!["B"]);
        assert_eq!(d.iter().filter(|x| x.0 == Tag::Same).count(), 2);
    }

    #[test]
    fn word_diff_highlights_the_new_words() {
        let (old, new) = word_diff("the quick fox", "the slow brown fox");
        assert_eq!(old.iter().filter(|s| s.0).map(|s| s.1.as_str()).collect::<String>(), "quick");
        assert_eq!(new.iter().filter(|s| s.0).map(|s| s.1.as_str()).collect::<String>(), "slow brown");
        assert_eq!(new.iter().map(|s| s.1.as_str()).collect::<String>(), "the slow brown fox");
    }

    #[test]
    fn log_folds_edits_of_one_block() {
        let mk =
            |k, b: &str, a: &str| Change { kind: k, what: "Block", page: 1, block: 7, title: "P".into(), who: 2, at: 1, before: b.into(), after: a.into() };
        let mut log = Log::default();
        log.push(mk(Kind::Edited, "a", "ab"));
        log.push(mk(Kind::Edited, "ab", "abc"));
        assert_eq!(log.list.len(), 1);
        assert_eq!((log.list[0].before.as_str(), log.list[0].after.as_str()), ("a", "abc"));
        log.push(mk(Kind::Edited, "abc", "a")); // typed and reverted: nothing to review
        assert!(log.list.is_empty());
        log.push(mk(Kind::Added, "", "x"));
        log.push(mk(Kind::Removed, "x", ""));
        assert!(log.list.is_empty());
    }
}

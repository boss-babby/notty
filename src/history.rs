//! Undo / redo stack for every block operation and versioned page snapshots ("Page history").
//!
//! * [`track`] runs every frame for the open page. It fingerprints the blocks; when they changed it pushes the previous state on
//!   the undo stack (typing bursts within ~1.2 s are merged into one step). Undo / redo restore whole block lists, so *any*
//!   operation — typing, drag & drop, turn-into, colours, paste, delete, media, restore from history — is undoable.
//! * Auto snapshots are stored in [`Page::history`] after the page was idle for a while (or when you leave it); users can also
//!   save named versions manually, preview them with a change summary and restore them (restoring is itself undoable).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use egui::{pos2, vec2, Align, Align2, Context, CursorIcon, Id, Layout, RichText, ScrollArea, Sense, Stroke, Ui};

use crate::auth::now_ms;
use crate::dates;
use crate::editor::EditorState;
use crate::model::{Block, BlockKind, Page, Version, Workspace};
use crate::theme::{bold, body, lerp_color, Palette};
use crate::widgets::{field, ghost_button, primary_button, with_alpha};

const MAX_UNDO: usize = 300;
const MAX_AUTO: usize = 30;
const MAX_TOTAL: usize = 80;

fn hash_block(b: &Block, with_text: bool, h: &mut DefaultHasher) {
    b.id.hash(h);
    std::mem::discriminant(&b.kind).hash(h);
    if with_text {
        b.text.hash(h);
    }
    (b.checked, b.indent, b.collapsed, b.color, b.bg, b.link, b.size, b.media).hash(h);
    b.url.hash(h);
}

pub fn fingerprint(blocks: &[Block]) -> u64 {
    let mut h = DefaultHasher::new();
    blocks.len().hash(&mut h);
    for b in blocks {
        hash_block(b, true, &mut h);
    }
    h.finish()
}

/// Same as [`fingerprint`] but ignoring text — changes only on structural / attribute edits.
fn structure(blocks: &[Block]) -> u64 {
    let mut h = DefaultHasher::new();
    blocks.len().hash(&mut h);
    for b in blocks {
        hash_block(b, false, &mut h);
    }
    h.finish()
}

fn version_fp(title: &str, blocks: &[Block]) -> u64 {
    let mut h = DefaultHasher::new();
    title.hash(&mut h);
    fingerprint(blocks).hash(&mut h);
    h.finish()
}

fn sync(ed: &mut EditorState, page: &Page) {
    ed.hist_prev = page.blocks.clone();
    ed.hist_fp = fingerprint(&page.blocks);
    ed.hist_sig = structure(&page.blocks);
    ed.hist_last = 0.0;
}

pub fn can_undo(ed: &EditorState, page: u64) -> bool {
    ed.undo.iter().any(|(p, _)| *p == page)
}

pub fn can_redo(ed: &EditorState, page: u64) -> bool {
    ed.redo.iter().any(|(p, _)| *p == page)
}

pub fn undo(ed: &mut EditorState, page: &mut Page) -> bool {
    match ed.undo.iter().rposition(|(p, _)| *p == page.id) {
        Some(pos) => {
            let (_, blocks) = ed.undo.remove(pos);
            let cur = std::mem::replace(&mut page.blocks, blocks);
            ed.redo.push((page.id, cur));
            ed.sel.clear();
            sync(ed, page);
            true
        }
        None => false,
    }
}

pub fn redo(ed: &mut EditorState, page: &mut Page) -> bool {
    match ed.redo.iter().rposition(|(p, _)| *p == page.id) {
        Some(pos) => {
            let (_, blocks) = ed.redo.remove(pos);
            let cur = std::mem::replace(&mut page.blocks, blocks);
            ed.undo.push((page.id, cur));
            ed.sel.clear();
            sync(ed, page);
            true
        }
        None => false,
    }
}

/// Saves a snapshot (skipped when identical to the newest one). Returns true when a new version was stored.
pub fn snapshot(page: &mut Page, manual: bool, note: &str) -> bool {
    let fp = version_fp(&page.title, &page.blocks);
    if let Some(last) = page.history.last_mut() {
        if version_fp(&last.title, &last.blocks) == fp {
            if manual {
                last.manual = true;
                if !note.is_empty() {
                    last.note = note.to_owned();
                }
            }
            return false;
        }
    }
    page.history.push(Version { ts: now_ms(), title: page.title.clone(), blocks: page.blocks.clone(), manual, note: note.to_owned() });
    prune(page);
    true
}

fn prune(page: &mut Page) {
    while page.history.iter().filter(|v| !v.manual).count() > MAX_AUTO {
        if let Some(i) = page.history.iter().position(|v| !v.manual) {
            page.history.remove(i);
        }
    }
    while page.history.len() > MAX_TOTAL {
        page.history.remove(0);
    }
}

pub fn restore(page: &mut Page, idx: usize) -> bool {
    let Some(v) = page.history.get(idx).cloned() else { return false };
    snapshot(page, false, "Before restore");
    page.blocks = v.blocks;
    page.title = v.title;
    page.updated = now_ms();
    true
}

fn has_content(p: &Page) -> bool {
    p.blocks.iter().any(|b| !b.text.trim().is_empty() || !matches!(b.kind, BlockKind::Text))
}

/// Per-frame bookkeeping for the open page: undo stack + automatic snapshots.
pub fn track(ws: &mut Workspace, cur: u64, ed: &mut EditorState, now: f64) {
    if ed.hist_page != cur {
        if ed.hist_page != 0 && ed.hist_dirty != 0.0 {
            if let Some(p) = ws.page_mut(ed.hist_page) {
                snapshot(p, false, "");
            }
        }
        ed.hist_dirty = 0.0;
        ed.hist_page = cur;
        if let Some(p) = ws.page_mut(cur) {
            if p.history.is_empty() && has_content(p) {
                snapshot(p, false, "Before editing");
            }
            sync(ed, p);
        }
        return;
    }
    let Some(p) = ws.page_mut(cur) else { return };
    let fp = fingerprint(&p.blocks);
    if fp != ed.hist_fp {
        let sig = structure(&p.blocks);
        let burst = sig == ed.hist_sig && now - ed.hist_last < 1.2;
        if !burst {
            ed.undo.push((cur, std::mem::take(&mut ed.hist_prev)));
            ed.redo.clear();
            if ed.undo.len() > MAX_UNDO {
                ed.undo.remove(0);
            }
        }
        ed.hist_prev = p.blocks.clone();
        ed.hist_fp = fp;
        ed.hist_sig = sig;
        ed.hist_last = now;
        if ed.hist_dirty == 0.0 {
            ed.hist_dirty = now;
        }
    }
    if ed.hist_dirty != 0.0 && (now - ed.hist_last > 20.0 || now - ed.hist_dirty > 240.0) {
        snapshot(p, false, "");
        ed.hist_dirty = 0.0;
    }
}

// ───────────────────────────── history dialog ─────────────────────────────

#[derive(Default)]
pub struct HistState {
    pub sel: Option<usize>,
    pub note: String,
}

#[derive(Default)]
pub struct HistOut {
    pub close: bool,
    pub toast: Option<String>,
}

/// "Mar 5, 14:32" in the local time zone (browser time on the web, UTC on desktop).
pub fn stamp(ts_ms: f64) -> String {
    #[allow(unused_mut)]
    let mut ms = ts_ms;
    #[cfg(target_arch = "wasm32")]
    {
        ms -= js_sys::Date::new_0().get_timezone_offset() * 60_000.0;
    }
    let days = (ms / 86_400_000.0).floor() as i64;
    let (_, m, d) = dates::civil_from_days(days);
    let secs = ((ms - days as f64 * 86_400_000.0) / 1000.0) as u64;
    format!("{} {}, {:02}:{:02}", &dates::MONTHS[m as usize - 1][..3], d, secs / 3600, (secs / 60) % 60)
}

fn plain_line(b: &Block) -> String {
    let pad = "    ".repeat(b.indent as usize);
    let t = crate::rich::strip(&b.text);
    match b.kind {
        BlockKind::H1 => format!("{pad}{}", t.to_uppercase()),
        BlockKind::H2 | BlockKind::H3 => format!("{pad}{t}"),
        BlockKind::Todo => format!("{pad}{} {t}", if b.checked { "☑" } else { "☐" }),
        BlockKind::Bullet | BlockKind::Toggle => format!("{pad}•  {t}"),
        BlockKind::Numbered => format!("{pad}1.  {t}"),
        BlockKind::Quote => format!("{pad}“{t}”"),
        BlockKind::Callout => format!("{pad}💡 {t}"),
        BlockKind::Divider => format!("{pad}────────"),
        BlockKind::Image => format!("{pad}[image] {t}"),
        BlockKind::File => format!("{pad}[file] {t}"),
        BlockKind::Embed | BlockKind::Bookmark => format!("{pad}[link] {}", if t.is_empty() { &b.url } else { &t }),
        BlockKind::Toc => format!("{pad}[table of contents]"),
        BlockKind::PageLink => format!("{pad}[page link]"),
        BlockKind::Table => format!("{pad}[table] {}", t.replace([crate::mdtable::CELL, crate::mdtable::ROW], " ")),
        _ => format!("{pad}{t}"),
    }
}

/// (added, removed) block counts of `old` compared to `now` (multiset comparison of block text).
fn diff_counts(old: &[Block], now: &[Block]) -> (usize, usize) {
    let mut pool: Vec<String> = now.iter().map(plain_line).collect();
    let mut same = 0;
    for b in old {
        let l = plain_line(b);
        if let Some(i) = pool.iter().position(|x| *x == l) {
            pool.remove(i);
            same += 1;
        }
    }
    (old.len() - same, pool.len())
}

pub fn dialog(ctx: &Context, pal: &Palette, ws: &mut Workspace, cur: u64, st: &mut HistState, narrow: bool) -> HistOut {
    let mut out = HistOut::default();
    let Some(page) = ws.page(cur) else {
        out.close = true;
        return out;
    };
    let versions: Vec<Version> = page.history.clone();
    let now_blocks = page.blocks.clone();
    let title = if page.title.is_empty() { "Untitled".to_owned() } else { page.title.clone() };
    let mut act: Option<(u8, usize)> = None;
    let sel = st.sel.filter(|i| *i < versions.len()).or(if versions.is_empty() { None } else { Some(versions.len() - 1) });
    let open = crate::app::modal(ctx, Id::new("page_history"), pal, 780.0, 70.0, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.label(RichText::new("Page history").font(bold(20.0)));
            ui.label(RichText::new(format!("· {title}")).font(body(14.0)).color(pal.muted));
        });
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.label(RichText::new("Versions are saved automatically while you write. Restoring one keeps the current text as a new version — and you can still undo it.").font(body(12.5)).color(pal.muted));
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            field(ui, pal, pal.bg, &mut st.note, "Name this version (optional)", false, if narrow { 150.0 } else { 280.0 }, Id::new("hist_note"));
            if primary_button(ui, pal, "Save version", 120.0).clicked() {
                act = Some((0, 0));
            }
        });
        ui.add_space(8.0);
        let list_w = if narrow { ui.available_width() - 8.0 } else { 262.0 };
        let body_fn = |ui: &mut Ui, act: &mut Option<(u8, usize)>, st: &mut HistState| {
            ui.vertical(|ui| {
                ui.set_width(list_w);
                ScrollArea::vertical().id_salt("hist_list").max_height(if narrow { 150.0 } else { 380.0 }).auto_shrink([false, false]).show(ui, |ui| {
                    if versions.is_empty() {
                        ui.add_space(20.0);
                        ui.label(RichText::new("No versions yet. Edit the page and one will appear here.").font(body(13.0)).color(pal.muted));
                    }
                    for (i, v) in versions.iter().enumerate().rev() {
                        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 50.0), Sense::click());
                        let on = sel == Some(i);
                        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                        ui.painter().rect_filled(r, 8.0, if on { pal.active } else { with_alpha(pal.hover, t) });
                        let label = if v.manual && !v.note.is_empty() { v.note.clone() } else { stamp(v.ts) };
                        crate::widgets::text_fit(ui.painter(), pos2(r.left() + 12.0, r.top() + 17.0), Align2::LEFT_CENTER, &label, bold(13.5), pal.text, r.width() - 24.0);
                        let words: usize = v.blocks.iter().map(|b| b.text.split_whitespace().count()).sum();
                        let sub = format!("{}{} · {} words{}", if v.manual { "★ saved · " } else { "" }, if v.manual && !v.note.is_empty() { stamp(v.ts) } else { crate::app::ago(v.ts) }, words, if v.note == "Before restore" { " · before restore" } else { "" });
                        crate::widgets::text_fit(ui.painter(), pos2(r.left() + 12.0, r.top() + 35.0), Align2::LEFT_CENTER, &sub, body(12.0), pal.muted, r.width() - 24.0);
                        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            st.sel = Some(i);
                        }
                    }
                });
            });
            if narrow {
                ui.add_space(8.0);
            } else {
                ui.add_space(8.0);
                let (r, _) = ui.allocate_exact_size(vec2(1.0, 380.0), Sense::hover());
                ui.painter().rect_filled(r, 0.0, pal.border);
                ui.add_space(8.0);
            }
            ui.vertical(|ui| {
                let Some(i) = sel else { return };
                let v = &versions[i];
                let (added, removed) = diff_counts(&v.blocks, &now_blocks);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(stamp(v.ts)).font(bold(14.0)));
                    let summary = if added == 0 && removed == 0 { "identical to the current page".to_owned() } else { format!("{added} block(s) differ · {removed} only in the current page") };
                    ui.label(RichText::new(summary).font(body(12.5)).color(pal.muted));
                });
                ui.horizontal(|ui| {
                    if primary_button(ui, pal, "Restore this version", 170.0).clicked() {
                        *act = Some((1, i));
                    }
                    if ghost_button(ui, pal, "Delete").clicked() {
                        *act = Some((2, i));
                    }
                });
                ui.add_space(6.0);
                ScrollArea::vertical().id_salt("hist_prev").max_height(if narrow { 180.0 } else { 330.0 }).auto_shrink([false, false]).show(ui, |ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 0.0), Sense::hover());
                    let _ = r;
                    ui.label(RichText::new(if v.title.is_empty() { "Untitled".to_owned() } else { v.title.clone() }).font(bold(18.0)));
                    ui.add_space(4.0);
                    let mut pool: Vec<String> = now_blocks.iter().map(plain_line).collect();
                    for b in v.blocks.iter().take(300) {
                        let line = plain_line(b);
                        let in_now = if let Some(k) = pool.iter().position(|x| *x == line) {
                            pool.remove(k);
                            true
                        } else {
                            false
                        };
                        let mut rt = RichText::new(if line.trim().is_empty() { " ".to_owned() } else { line }).font(match b.kind {
                            BlockKind::H1 | BlockKind::H2 | BlockKind::H3 => bold(15.0),
                            BlockKind::Code => egui::FontId::monospace(12.5),
                            _ => body(13.5),
                        });
                        if !in_now {
                            rt = rt.background_color(with_alpha(lerp_color(pal.accent, pal.bg, 0.3), 0.28));
                        }
                        ui.add(egui::Label::new(rt).wrap());
                    }
                });
            });
        };
        if narrow {
            ui.vertical(|ui| body_fn(ui, &mut act, st));
        } else {
            ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                ui.add_space(8.0);
                body_fn(ui, &mut act, st)
            });
        }
        ui.add_space(6.0);
        let _ = Stroke::NONE;
    });
    if let Some((kind, i)) = act {
        if let Some(p) = ws.page_mut(cur) {
            match kind {
                0 => {
                    let note = std::mem::take(&mut st.note);
                    let note = note.trim().to_owned();
                    out.toast = Some(if snapshot(p, true, &note) { "Version saved".into() } else { "Marked the latest version as saved".into() });
                    st.sel = None;
                }
                1 => {
                    if restore(p, i) {
                        out.toast = Some("Version restored — Ctrl+Z undoes it".into());
                        out.close = true;
                    }
                }
                _ => {
                    if i < p.history.len() {
                        p.history.remove(i);
                        st.sel = None;
                    }
                }
            }
        }
    }
    if !open {
        out.close = true;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BlockKind, PageKind};

    #[test]
    fn undo_redo_and_versions() {
        let mut ws = Workspace::seed();
        let id = ws.add_page(None, PageKind::Doc);
        let mut ed = EditorState::default();
        track(&mut ws, id, &mut ed, 1.0);
        // typing burst → one undo step
        for (i, t) in ["h", "he", "hel"].iter().enumerate() {
            ws.page_mut(id).unwrap().blocks[0].text = (*t).into();
            track(&mut ws, id, &mut ed, 2.0 + i as f64 * 0.1);
        }
        assert_eq!(ed.undo.len(), 1);
        // structural change much later → a second step
        let nb = ws.blk(BlockKind::Bullet, "x", 0);
        ws.page_mut(id).unwrap().blocks.push(nb);
        track(&mut ws, id, &mut ed, 10.0);
        assert_eq!(ed.undo.len(), 2);
        assert!(undo(&mut ed, ws.page_mut(id).unwrap()));
        assert_eq!(ws.page(id).unwrap().blocks.len(), 1);
        track(&mut ws, id, &mut ed, 11.0);
        assert!(can_redo(&ed, id), "undo must not clear redo");
        assert!(redo(&mut ed, ws.page_mut(id).unwrap()));
        assert_eq!(ws.page(id).unwrap().blocks.len(), 2);
        assert!(undo(&mut ed, ws.page_mut(id).unwrap()) && undo(&mut ed, ws.page_mut(id).unwrap()));
        assert_eq!(ws.page(id).unwrap().blocks[0].text, "");
        // versions
        let p = ws.page_mut(id).unwrap();
        p.blocks[0].text = "v1".into();
        assert!(snapshot(p, true, "first"));
        assert!(!snapshot(p, false, ""));
        p.blocks[0].text = "v2".into();
        assert!(snapshot(p, false, ""));
        p.blocks[0].text = "v3".into();
        assert!(restore(p, 0));
        assert_eq!(p.blocks[0].text, "v1");
        assert!(p.history.len() >= 3, "restore keeps the replaced text as a version");
    }
}

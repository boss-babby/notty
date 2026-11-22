//! Comments and @mentions (COLLAB.md §7 item 10).
//!
//! A comment is an ordinary synced entity (`Key::Comment(page, id)`), so threads, replies, *resolve* and deletes travel through the
//! same CRDT as blocks — in a session, offline, and through the relay. The window lists the threads of the open page, can attach a
//! comment to the block the user is editing, completes `@names` from the people in the session and raises a toast when somebody
//! else mentions you.
use super::ui::{avatar, button, glyph_button};
use super::*;
use crate::lucide as l;
use crate::model::{Comment, Workspace};
use crate::theme::{bold, body, Palette};
use crate::widgets::with_alpha;
use egui::{pos2, vec2, Align2, Color32, Context, Id, Sense};

/// Does `text` contain `@first-name` of `name`?
pub fn mentions(text: &str, name: &str) -> bool {
    let first = name.split_whitespace().next().unwrap_or("").to_lowercase();
    if first.is_empty() {
        return false;
    }
    let t = text.to_lowercase();
    let needle = format!("@{first}");
    t.match_indices(&needle).any(|(i, _)| t[i + needle.len()..].chars().next().map_or(true, |c| !c.is_alphanumeric()))
}

fn ago(ts: f64) -> String {
    let d = (now_ms() as f64 / 1000.0 - ts).max(0.0);
    match d as u64 {
        0..=44 => "just now".into(),
        45..=3599 => format!("{} min", (d / 60.0).round() as u64),
        3600..=86_399 => format!("{} h", (d / 3600.0).round() as u64),
        _ => format!("{} d", (d / 86_400.0).round() as u64),
    }
}

fn snippet(s: &str, n: usize) -> String {
    let line = s.lines().next().unwrap_or("");
    if line.chars().count() > n {
        format!("{}…", line.chars().take(n).collect::<String>())
    } else {
        line.to_owned()
    }
}

/// Toast for new comments from other people (and a stronger one when they mention you). Runs every frame, cheap.
pub fn notify(c: &mut Collab, ws: &Workspace) {
    let mut fresh: Vec<&Comment> = vec![];
    for list in ws.comments.values() {
        for cm in list {
            if !c.ui.comment_known.contains(&cm.id) {
                fresh.push(cm);
            }
        }
    }
    if fresh.is_empty() {
        c.ui.comment_primed = true;
        return;
    }
    let primed = c.ui.comment_primed;
    let me = c.me.clone();
    let mut toast: Option<String> = None;
    let ids: Vec<u64> = fresh.iter().map(|x| x.id).collect();
    if primed {
        for cm in &fresh {
            if cm.author_id == me.id || cm.resolved {
                continue;
            }
            let msg = if mentions(&cm.text, &me.name) { format!("{} mentioned you: {}", cm.author, snippet(&cm.text, 60)) } else { format!("{} commented: {}", cm.author, snippet(&cm.text, 60)) };
            // a mention always wins over a plain comment
            if toast.is_none() || mentions(&cm.text, &me.name) {
                toast = Some(msg);
            }
        }
    }
    c.ui.comment_known.extend(ids);
    c.ui.comment_primed = true;
    if let Some(t) = toast {
        c.toast(t);
    }
}

fn mention_spans(ui: &mut egui::Ui, pal: &Palette, text: &str, size: f32) {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    let mut rest = text;
    while let Some(i) = rest.find('@') {
        let (a, b) = rest.split_at(i);
        let end = b[1..].find(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '-')).map_or(b.len(), |e| e + 1);
        job.append(a, 0.0, egui::TextFormat { font_id: body(size), color: pal.text, ..Default::default() });
        if end > 1 {
            job.append(&b[..end], 0.0, egui::TextFormat { font_id: bold(size), color: pal.accent, background: with_alpha(pal.accent, 0.12), ..Default::default() });
            rest = &b[end..];
        } else {
            job.append("@", 0.0, egui::TextFormat { font_id: body(size), color: pal.text, ..Default::default() });
            rest = &b[1..];
        }
    }
    job.append(rest, 0.0, egui::TextFormat { font_id: body(size), color: pal.text, ..Default::default() });
    ui.label(job);
}

pub fn window(ctx: &Context, pal: &Palette, c: &mut Collab, ws: &mut Workspace, current: Option<u64>) {
    let Some(pid) = current else { return };
    let screen = ctx.screen_rect();
    let w = 340.0_f32.min(screen.width() - 16.0);
    let list: Vec<Comment> = ws.comments.get(&pid).cloned().unwrap_or_default();
    let me = c.me.clone();
    let can_moderate = c.mode != Mode::Joined || c.role == Role::Editor;
    let viewer = c.mode != Mode::Off && c.role == Role::Viewer;
    let editing_block = c.local.block.filter(|b| ws.page(pid).map_or(false, |p| p.blocks.iter().any(|x| x.id == *b)));
    let names: Vec<String> = c.roster.iter().filter(|(p, _)| p.id != me.id).filter_map(|(p, _)| p.name.split_whitespace().next().map(|s| s.to_owned())).collect();

    let mut close = false;
    let mut resolve: Option<(u64, bool)> = None;
    let mut delete: Option<u64> = None;
    let mut submit = false;
    egui::Window::new("comments_win")
        .id(Id::new("comments_win"))
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::RIGHT_TOP, vec2(-8.0, 52.0))
        .frame(crate::widgets::popup_frame(pal).corner_radius(egui::CornerRadius::same(14)).inner_margin(egui::Margin::same(12)))
        .show(ctx, |ui| {
            ui.set_width(w - 24.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(l::MESSAGE_SQUARE).font(body(15.0)).color(pal.accent));
                ui.label(egui::RichText::new("Comments").font(bold(15.0)).color(pal.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if glyph_button(ui, pal, l::X, "Close", None).clicked() {
                        close = true;
                    }
                    let resolved_n = list.iter().filter(|x| x.resolved).count();
                    if resolved_n > 0 {
                        let lbl = if c.ui.comment_resolved { "Hide resolved" } else { "Resolved" };
                        if button(ui, pal, &format!("{lbl} ({resolved_n})"), 3, 112.0).clicked() {
                            c.ui.comment_resolved = !c.ui.comment_resolved;
                        }
                    }
                });
            });
            ui.add_space(6.0);
            let shown: Vec<&Comment> = list.iter().filter(|x| x.reply_to == 0 && (c.ui.comment_resolved || !x.resolved)).collect();
            if shown.is_empty() {
                ui.label(egui::RichText::new("No comments yet. Start the conversation — @mention a teammate to notify them.").font(body(12.5)).color(pal.muted));
            }
            egui::ScrollArea::vertical().max_height((screen.height() - 330.0).max(120.0)).auto_shrink([true, true]).show(ui, |ui| {
                for top in shown {
                    thread(ui, pal, c, ws, pid, top, &list, &me, can_moderate && !viewer, &mut resolve, &mut delete);
                    ui.add_space(6.0);
                }
            });
            if viewer {
                ui.label(egui::RichText::new("View only — you cannot comment.").font(body(12.0)).color(pal.muted));
                return;
            }
            ui.add_space(4.0);
            if c.ui.comment_reply != 0 {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Replying").font(body(12.0)).color(pal.accent));
                    if button(ui, pal, "Cancel", 3, 60.0).clicked() {
                        c.ui.comment_reply = 0;
                    }
                });
            } else if let Some(b) = editing_block {
                let txt = ws.page(pid).and_then(|p| p.blocks.iter().find(|x| x.id == b)).map(|x| snippet(&x.text, 28)).unwrap_or_default();
                ui.horizontal(|ui| {
                    let on = c.ui.comment_on_block;
                    if super::ui::switch_public(ui, pal, on).clicked() {
                        c.ui.comment_on_block = !on;
                    }
                    ui.label(egui::RichText::new(format!("On the block I'm editing: “{txt}”")).font(body(12.0)).color(pal.muted));
                });
            }
            let edit_id = Id::new("comment_draft_edit");
            let out = egui::TextEdit::multiline(&mut c.ui.comment_draft).id(edit_id).desired_rows(2).hint_text("Add a comment — @name mentions").desired_width(w - 24.0).font(body(13.5)).show(ui);
            // @name picker: follows the caret (not only the end of the text), matches the start of any word of a name, keyboard + tap
            let chars: Vec<char> = c.ui.comment_draft.chars().collect();
            let caret = out.cursor_range.map(|r| r.primary.ccursor.index).unwrap_or(chars.len()).min(chars.len());
            let mut at = None;
            let mut i = caret;
            while i > 0 {
                let ch = chars[i - 1];
                if ch == '@' {
                    if i == 1 || chars[i - 2].is_whitespace() {
                        at = Some(i - 1);
                    }
                    break;
                }
                if ch.is_whitespace() {
                    break;
                }
                i -= 1;
            }
            let focused = out.response.has_focus();
            if let (Some(at), true) = (at, focused || c.ui.comment_pick != usize::MAX) {
                let frag: String = chars[at + 1..caret].iter().collect::<String>().to_lowercase();
                let mut hits: Vec<&String> = names.iter().filter(|n| frag.is_empty() || n.to_lowercase().split_whitespace().any(|w| w.starts_with(&frag))).collect();
                hits.dedup();
                hits.truncate(6);
                if hits.is_empty() {
                    ui.label(egui::RichText::new(if names.is_empty() { "Nobody else is in this session yet — share the workspace to mention people." } else { "No one by that name." }).font(body(11.5)).color(pal.muted));
                } else {
                    if c.ui.comment_pick >= hits.len() {
                        c.ui.comment_pick = 0;
                    }
                    let (down, up, accept) = ui.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown), i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp), i.consume_key(egui::Modifiers::NONE, egui::Key::Tab) || i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)));
                    if down {
                        c.ui.comment_pick = (c.ui.comment_pick + 1) % hits.len();
                    }
                    if up {
                        c.ui.comment_pick = (c.ui.comment_pick + hits.len() - 1) % hits.len();
                    }
                    let mut chosen: Option<String> = if accept { Some(hits[c.ui.comment_pick].clone()) } else { None };
                    egui::Frame::new().fill(with_alpha(pal.hover, 0.6)).corner_radius(egui::CornerRadius::same(8)).inner_margin(egui::Margin::same(3)).show(ui, |ui| {
                        for (ix, n) in hits.iter().enumerate() {
                            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
                            if ix == c.ui.comment_pick || resp.hovered() {
                                ui.painter().rect_filled(r, 6.0, pal.accent_soft);
                            }
                            ui.painter().text(pos2(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, format!("@{n}"), body(13.0), pal.text);
                            if resp.clicked() {
                                chosen = Some((*n).clone());
                            }
                        }
                    });
                    if let Some(n) = chosen {
                        let mut s: String = chars[..at + 1].iter().collect();
                        s.push_str(&n);
                        s.push(' ');
                        let new_caret = s.chars().count();
                        s.extend(chars[caret..].iter());
                        c.ui.comment_draft = s;
                        c.ui.comment_pick = 0;
                        let mut st = egui::TextEdit::load_state(ui.ctx(), edit_id).unwrap_or_default();
                        st.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(new_caret))));
                        st.store(ui.ctx(), edit_id);
                        ui.ctx().memory_mut(|m| m.request_focus(edit_id));
                    }
                }
            } else {
                c.ui.comment_pick = 0;
            }
            let empty = c.ui.comment_draft.trim().is_empty();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{} chars", c.ui.comment_draft.chars().count())).font(body(11.5)).color(pal.muted));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled_ui(!empty, |ui| button(ui, pal, "Comment", 1, 86.0)).inner.clicked() {
                        submit = true;
                    }
                });
            });
        });

    if submit {
        let text: String = c.ui.comment_draft.trim().chars().take(4000).collect();
        if !text.is_empty() {
            let id = ws.id();
            let reply_to = c.ui.comment_reply;
            let block = if reply_to == 0 && c.ui.comment_on_block { editing_block.unwrap_or(0) } else { 0 };
            let cm = Comment { id, block, reply_to, author: me.name.clone(), author_id: me.id, text, ts: now_ms() as f64 / 1000.0, resolved: false };
            ws.comments.entry(pid).or_default().push(cm);
            c.ui.comment_known.insert(id);
            c.ui.comment_draft.clear();
            c.ui.comment_reply = 0;
            c.poke();
        }
    }
    if let Some((id, v)) = resolve {
        if let Some(cm) = ws.comments.get_mut(&pid).and_then(|l| l.iter_mut().find(|x| x.id == id)) {
            cm.resolved = v;
        }
        c.poke();
    }
    if let Some(id) = delete {
        if let Some(l) = ws.comments.get_mut(&pid) {
            l.retain(|x| x.id != id && x.reply_to != id);
            if l.is_empty() {
                ws.comments.remove(&pid);
            }
        }
        c.poke();
    }
    if close {
        c.ui.comments = false;
    }
}

#[allow(clippy::too_many_arguments)]
fn thread(ui: &mut egui::Ui, pal: &Palette, c: &mut Collab, ws: &Workspace, pid: u64, top: &Comment, all: &[Comment], me: &PeerInfo, can_edit: bool, resolve: &mut Option<(u64, bool)>, delete: &mut Option<u64>) {
    crate::widgets::popup_frame(pal).fill(with_alpha(pal.hover, if top.resolved { 0.25 } else { 0.55 })).corner_radius(egui::CornerRadius::same(10)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if top.block != 0 {
            if let Some(b) = ws.page(pid).and_then(|p| p.blocks.iter().find(|x| x.id == top.block)) {
                ui.label(egui::RichText::new(format!("▎{}", snippet(&b.text, 48))).font(body(11.5)).color(pal.muted));
            }
        }
        let mut one = |ui: &mut egui::Ui, cm: &Comment, is_top: bool| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
                let who = PeerInfo { id: cm.author_id, name: cm.author.clone(), device: String::new(), platform: String::new(), color: (cm.author_id % 8) as u8 };
                avatar(ui.painter(), r.center(), 11.0, &who, pal.popup);
                ui.label(egui::RichText::new(&cm.author).font(bold(12.5)).color(pal.text));
                ui.label(egui::RichText::new(ago(cm.ts)).font(body(11.5)).color(pal.muted));
                if cm.resolved && is_top {
                    ui.label(egui::RichText::new("resolved").font(body(11.5)).color(Color32::from_rgb(52, 168, 83)));
                }
            });
            mention_spans(ui, pal, &cm.text, 13.0);
        };
        one(ui, top, true);
        for r in all.iter().filter(|x| x.reply_to == top.id) {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.vertical(|ui| one(ui, r, false));
            });
            if can_edit && (r.author_id == me.id || c.mode != Mode::Joined) {
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    if button(ui, pal, "Delete", 3, 56.0).clicked() {
                        *delete = Some(r.id);
                    }
                });
            }
        }
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if can_edit {
                if button(ui, pal, "Reply", 3, 52.0).clicked() {
                    c.ui.comment_reply = top.id;
                }
                if button(ui, pal, if top.resolved { "Reopen" } else { "Resolve" }, 3, 64.0).clicked() {
                    *resolve = Some((top.id, !top.resolved));
                }
                if (top.author_id == me.id || c.mode != Mode::Joined) && button(ui, pal, "Delete", 3, 56.0).clicked() {
                    *delete = Some(top.id);
                }
            }
        });
        let _ = pos2(0.0, 0.0);
    });
}

#[cfg(test)]
mod tests {
    use super::mentions;

    #[test]
    fn mentions_match_first_names_only_at_word_ends() {
        assert!(mentions("ping @anna please", "Anna Lopez"));
        assert!(mentions("@Anna", "anna"));
        assert!(!mentions("email anna@example.com", "Bob"));
        assert!(!mentions("@annabel", "Anna"));
        assert!(!mentions("hello", ""));
    }
}

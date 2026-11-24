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

fn ui_first(n: &str) -> &str {
    n.split_whitespace().next().unwrap_or(n)
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

pub fn window(
    ctx: &Context,
    pal: &Palette,
    c: &mut Collab,
    ws: &mut Workspace,
    current: Option<u64>,
) {
    let Some(pid) = current else { return };
    let screen = ctx.screen_rect();
    let w = 340.0_f32.min(screen.width() - 16.0);
    let list: Vec<Comment> = ws.comments.get(&pid).cloned().unwrap_or_default();
    let me = c.me.clone();
    let can_moderate = c.mode != Mode::Joined || c.role == Role::Editor;
    let viewer = c.mode != Mode::Off && c.role == Role::Viewer;
    let editing_block = c.local.block.filter(|b| {
        ws.page(pid)
            .map_or(false, |p| p.blocks.iter().any(|x| x.id == *b))
    });
    // read receipt: this device is looking at the thread list of `pid` right now, up to the newest comment it contains
    if c.mode != Mode::Off {
        let newest = list
            .iter()
            .map(|x| (x.ts * 1000.0) as u64)
            .max()
            .unwrap_or(0);
        if newest > 0 && c.local.seen != Some((pid, newest)) {
            c.local.seen = Some((pid, newest));
        }
    }
    let peers: Vec<(PeerInfo, String)> = c
        .roster
        .iter()
        .filter(|(p, _)| p.id != me.id)
        .filter_map(|(p, _)| {
            p.name
                .split_whitespace()
                .next()
                .map(|first| (p.clone(), first.to_owned()))
        })
        .collect();

    let phone = screen.width() < 560.0;
    let win_w = if phone {
        (screen.width() - 20.0).max(260.0)
    } else {
        356.0_f32.min(screen.width() - 16.0)
    };
    let anchor = if phone {
        (Align2::CENTER_TOP, vec2(0.0, 50.0))
    } else {
        (Align2::RIGHT_TOP, vec2(-10.0, 52.0))
    };

    let mut close = false;
    let mut resolve: Option<(u64, bool)> = None;
    let mut delete: Option<u64> = None;
    let mut submit = false;
    egui::Window::new("comments_win")
        .id(Id::new("comments_win"))
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(anchor.0, anchor.1)
        .frame(
            crate::widgets::popup_frame(pal)
                .corner_radius(egui::CornerRadius::same(12))
                .inner_margin(egui::Margin::same(14)),
        )
        .show(ctx, |ui| {
            let inner_w = (win_w - 28.0).max(220.0);
            ui.set_width(inner_w);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            // Clean Notion header
            ui.horizontal(|ui| {
                crate::icons::glyph(
                    ui.painter(),
                    ui.cursor().left_center() + vec2(8.0, 0.0),
                    15.0,
                    l::MESSAGE_SQUARE,
                    pal.muted,
                );
                ui.add_space(20.0);
                ui.label(
                    egui::RichText::new("Comments")
                        .font(bold(14.5))
                        .color(pal.text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if glyph_button(ui, pal, l::X, "Close", None).clicked() {
                        close = true;
                    }
                    let resolved_n = list.iter().filter(|x| x.resolved).count();
                    if resolved_n > 0 {
                        let lbl = if c.ui.comment_resolved {
                            "Hide resolved"
                        } else {
                            "Resolved"
                        };
                        if button(ui, pal, &format!("{lbl} ({resolved_n})"), 3, 108.0).clicked() {
                            c.ui.comment_resolved = !c.ui.comment_resolved;
                        }
                    }
                });
            });
            let (hr, _) = ui.allocate_exact_size(vec2(inner_w, 1.0), Sense::hover());
            ui.painter().line_segment(
                [
                    pos2(hr.left(), hr.center().y),
                    pos2(hr.right(), hr.center().y),
                ],
                egui::Stroke::new(1.0_f32, pal.border),
            );

            let shown: Vec<&Comment> = list
                .iter()
                .filter(|x| x.reply_to == 0 && (c.ui.comment_resolved || !x.resolved))
                .collect();
            if shown.is_empty() {
                egui::Frame::new()
                    .fill(pal.callout)
                    .corner_radius(egui::CornerRadius::same(8))
                    .stroke(egui::Stroke::new(1.0_f32, pal.border))
                    .inner_margin(egui::Margin::symmetric(12, 14))
                    .show(ui, |ui| {
                        ui.set_width((inner_w - 24.0).max(160.0));
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new("No comments yet")
                                    .font(bold(12.5))
                                    .color(pal.text),
                            );
                            ui.add_space(2.0);
                            ui.label(
                                egui::RichText::new(if peers.is_empty() {
                                    "Leave a note on this page, or share to @mention teammates."
                                } else {
                                    "Start a thread — type @ to mention a teammate."
                                })
                                .font(body(11.5))
                                .color(pal.muted),
                            );
                        });
                    });
            } else {
                egui::ScrollArea::vertical()
                    .max_height((screen.height() - if phone { 320.0 } else { 340.0 }).max(120.0))
                    .auto_shrink([true, true])
                    .show(ui, |ui| {
                        for top in shown {
                            thread(
                                ui,
                                pal,
                                c,
                                ws,
                                pid,
                                top,
                                &list,
                                &me,
                                can_moderate && !viewer,
                                &mut resolve,
                                &mut delete,
                            );
                            ui.add_space(6.0);
                        }
                    });
            }
            if viewer {
                ui.label(
                    egui::RichText::new("View only — you cannot comment.")
                        .font(body(12.0))
                        .color(pal.muted),
                );
                return;
            }
            if c.ui.comment_reply != 0 {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Replying to thread")
                            .font(body(12.0))
                            .color(pal.accent),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if button(ui, pal, "Cancel", 3, 56.0).clicked() {
                            c.ui.comment_reply = 0;
                        }
                    });
                });
            } else if let Some(b) = editing_block {
                let txt = ws
                    .page(pid)
                    .and_then(|p| p.blocks.iter().find(|x| x.id == b))
                    .map(|x| snippet(&x.text, 26))
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    let on = c.ui.comment_on_block;
                    if super::ui::switch_public(ui, pal, on).clicked() {
                        c.ui.comment_on_block = !on;
                    }
                    ui.label(
                        egui::RichText::new(format!("Attach to block: “{txt}”"))
                            .font(body(11.5))
                            .color(pal.muted),
                    );
                });
            }

            // Notion-style padded composer box with subtle muted placeholder
            let edit_id = Id::new("comment_draft_edit");
            let mut out_opt = None;
            egui::Frame::new()
                .fill(pal.code_bg)
                .corner_radius(egui::CornerRadius::same(8))
                .stroke(egui::Stroke::new(1.0_f32, pal.border))
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width((inner_w - 20.0).max(160.0));
                    let out = egui::TextEdit::multiline(&mut c.ui.comment_draft)
                        .id(edit_id)
                        .desired_rows(2)
                        .frame(false)
                        .hint_text(
                            egui::RichText::new("Add a comment… type @ to mention")
                                .color(pal.muted),
                        )
                        .desired_width(f32::INFINITY)
                        .font(body(13.0))
                        .show(ui);
                    out_opt = Some(out);
                });
            let out = out_opt.unwrap();

            // @mention picker: follows the caret, shows connected teammate avatars + names + @handle
            let chars: Vec<char> = c.ui.comment_draft.chars().collect();
            let caret = out
                .cursor_range
                .map(|r| r.primary.ccursor.index)
                .unwrap_or(chars.len())
                .min(chars.len());
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
                let frag: String = chars[at + 1..caret]
                    .iter()
                    .collect::<String>()
                    .to_lowercase();
                let mut hits: Vec<&(PeerInfo, String)> = peers
                    .iter()
                    .filter(|(p, first)| {
                        frag.is_empty()
                            || first.to_lowercase().starts_with(&frag)
                            || p.name
                                .to_lowercase()
                                .split_whitespace()
                                .any(|w| w.starts_with(&frag))
                    })
                    .collect();
                hits.truncate(6);
                egui::Frame::new()
                    .fill(pal.callout)
                    .corner_radius(egui::CornerRadius::same(8))
                    .stroke(egui::Stroke::new(1.0_f32, pal.border))
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        ui.set_width((inner_w - 8.0).max(160.0));
                        if hits.is_empty() {
                            ui.label(
                                egui::RichText::new(if peers.is_empty() {
                                    "Share this workspace to mention connected teammates."
                                } else {
                                    "No matching teammate."
                                })
                                .font(body(11.5))
                                .color(pal.muted),
                            );
                        } else {
                            if c.ui.comment_pick >= hits.len() {
                                c.ui.comment_pick = 0;
                            }
                            let (down, up, accept) = ui.input_mut(|i| {
                                (
                                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                                    i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                                        || i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                                )
                            });
                            if down {
                                c.ui.comment_pick = (c.ui.comment_pick + 1) % hits.len();
                            }
                            if up {
                                c.ui.comment_pick =
                                    (c.ui.comment_pick + hits.len() - 1) % hits.len();
                            }
                            let mut chosen: Option<String> = if accept {
                                Some(hits[c.ui.comment_pick].1.clone())
                            } else {
                                None
                            };
                            for (ix, (peer, handle)) in hits.iter().enumerate() {
                                let (r, resp) = ui.allocate_exact_size(
                                    vec2(ui.available_width(), 30.0),
                                    Sense::click(),
                                );
                                let sel = ix == c.ui.comment_pick || resp.hovered();
                                if sel {
                                    ui.painter().rect_filled(r, 6.0, pal.hover);
                                }
                                avatar(
                                    ui.painter(),
                                    pos2(r.left() + 14.0, r.center().y),
                                    9.5,
                                    peer,
                                    if sel { pal.hover } else { pal.callout },
                                );
                                ui.painter().text(
                                    pos2(r.left() + 30.0, r.center().y),
                                    Align2::LEFT_CENTER,
                                    &peer.name,
                                    bold(12.5),
                                    pal.text,
                                );
                                ui.painter().text(
                                    pos2(r.right() - 8.0, r.center().y),
                                    Align2::RIGHT_CENTER,
                                    format!("@{handle}"),
                                    body(11.5),
                                    pal.muted,
                                );
                                if resp
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .clicked()
                                {
                                    chosen = Some(handle.clone());
                                }
                            }
                            if let Some(n) = chosen {
                                let mut s: String = chars[..at + 1].iter().collect();
                                s.push_str(&n);
                                s.push(' ');
                                let new_caret = s.chars().count();
                                s.extend(chars[caret..].iter());
                                c.ui.comment_draft = s;
                                c.ui.comment_pick = 0;
                                let mut st = egui::TextEdit::load_state(ui.ctx(), edit_id)
                                    .unwrap_or_default();
                                st.cursor.set_char_range(Some(egui::text::CCursorRange::one(
                                    egui::text::CCursor::new(new_caret),
                                )));
                                st.store(ui.ctx(), edit_id);
                                ui.ctx().memory_mut(|m| m.request_focus(edit_id));
                            }
                        }
                    });
            } else {
                c.ui.comment_pick = 0;
            }

            // Clean composer footer: quick `@ Mention` trigger on left, crisp `Comment` button on right (no `0 chars` clutter)
            let empty = c.ui.comment_draft.trim().is_empty();
            ui.horizontal(|ui| {
                let mention_lbl = if peers.is_empty() {
                    "@ Mention".to_owned()
                } else {
                    format!("@ Mention ({})", peers.len())
                };
                if button(
                    ui,
                    pal,
                    &mention_lbl,
                    0,
                    if peers.is_empty() { 82.0 } else { 102.0 },
                )
                .on_hover_text("Insert @ to mention a teammate in this session")
                .clicked()
                {
                    if !c.ui.comment_draft.is_empty() && !c.ui.comment_draft.ends_with(' ') {
                        c.ui.comment_draft.push(' ');
                    }
                    c.ui.comment_draft.push('@');
                    let new_caret = c.ui.comment_draft.chars().count();
                    let mut st = egui::TextEdit::load_state(ui.ctx(), edit_id).unwrap_or_default();
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::one(
                        egui::text::CCursor::new(new_caret),
                    )));
                    st.store(ui.ctx(), edit_id);
                    ui.ctx().memory_mut(|m| m.request_focus(edit_id));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled_ui(!empty, |ui| button(ui, pal, "Comment", 1, 86.0))
                        .inner
                        .clicked()
                    {
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
            let block = if reply_to == 0 && c.ui.comment_on_block {
                editing_block.unwrap_or(0)
            } else {
                0
            };
            let cm = Comment {
                id,
                block,
                reply_to,
                author: me.name.clone(),
                author_id: me.id,
                text,
                ts: now_ms() as f64 / 1000.0,
                resolved: false,
            };
            ws.comments.entry(pid).or_default().push(cm);
            c.ui.comment_known.insert(id);
            c.ui.comment_draft.clear();
            c.ui.comment_reply = 0;
            c.poke();
        }
    }
    if let Some((id, v)) = resolve {
        if let Some(cm) = ws
            .comments
            .get_mut(&pid)
            .and_then(|l| l.iter_mut().find(|x| x.id == id))
        {
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
fn thread(
    ui: &mut egui::Ui,
    pal: &Palette,
    c: &mut Collab,
    ws: &Workspace,
    pid: u64,
    top: &Comment,
    all: &[Comment],
    me: &PeerInfo,
    can_edit: bool,
    resolve: &mut Option<(u64, bool)>,
    delete: &mut Option<u64>,
) {
    crate::widgets::popup_frame(pal)
        .fill(with_alpha(
            pal.hover,
            if top.resolved { 0.25 } else { 0.55 },
        ))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if top.block != 0 {
                if let Some(b) = ws
                    .page(pid)
                    .and_then(|p| p.blocks.iter().find(|x| x.id == top.block))
                {
                    ui.label(
                        egui::RichText::new(format!("▎{}", snippet(&b.text, 48)))
                            .font(body(11.5))
                            .color(pal.muted),
                    );
                }
            }
            let one = |ui: &mut egui::Ui, cm: &Comment, is_top: bool| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
                    let who = PeerInfo {
                        id: cm.author_id,
                        name: cm.author.clone(),
                        device: String::new(),
                        platform: String::new(),
                        color: (cm.author_id % 8) as u8,
                    };
                    avatar(ui.painter(), r.center(), 11.0, &who, pal.popup);
                    ui.label(
                        egui::RichText::new(&cm.author)
                            .font(bold(12.5))
                            .color(pal.text),
                    );
                    ui.label(
                        egui::RichText::new(ago(cm.ts))
                            .font(body(11.5))
                            .color(pal.muted),
                    );
                    if cm.resolved && is_top {
                        ui.label(
                            egui::RichText::new("resolved")
                                .font(body(11.5))
                                .color(Color32::from_rgb(52, 168, 83)),
                        );
                    }
                });
                mention_spans(ui, pal, &cm.text, 13.0);
                // read receipt on my own comments: who has had this page's comments open since it was written
                if cm.author_id == me.id && c.mode != Mode::Off {
                    let at = (cm.ts * 1000.0) as u64;
                    let seen: Vec<&str> = c
                        .roster
                        .iter()
                        .filter(|(p, _)| {
                            p.id != me.id
                                && c.presence_of(p.id)
                                    .and_then(|pr| pr.seen)
                                    .map_or(false, |(pg, t)| pg == pid && t >= at)
                        })
                        .map(|(p, _)| ui_first(&p.name))
                        .collect();
                    if !seen.is_empty() {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} Seen by {}",
                                l::CHECK,
                                seen.join(", ")
                            ))
                            .font(body(10.5))
                            .color(pal.muted),
                        );
                    }
                }
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
                    if button(
                        ui,
                        pal,
                        if top.resolved { "Reopen" } else { "Resolve" },
                        3,
                        64.0,
                    )
                    .clicked()
                    {
                        *resolve = Some((top.id, !top.resolved));
                    }
                    if (top.author_id == me.id || c.mode != Mode::Joined)
                        && button(ui, pal, "Delete", 3, 56.0).clicked()
                    {
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

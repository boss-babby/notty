//! Drawing of [`BlockKind::Synced`] blocks: the live content of another page / section / block with a small header
//! (source, section picker, open original, unsync). See `synced.rs` for the resolution rules.

use egui::{pos2, vec2, Align2, Frame, Margin, RichText, Sense, Stroke, Ui};

use crate::editor::EditorState;
use crate::history::plain_line;
use crate::model::{Block, BlockKind, Workspace};
use crate::synced::{self, Resolved};
use crate::theme::{body, bold, Palette};
use crate::widgets::with_alpha;

#[derive(Default)]
pub struct Out {
    pub changed: bool,
    /// "Unsync" was clicked: the resolved content as ordinary blocks (fresh ids) that replace the synced block.
    pub unsync: Option<Vec<Block>>,
}

fn headings(ws: &Workspace, page: u64) -> Vec<String> {
    ws.page(page)
        .map(|p| {
            p.blocks
                .iter()
                .filter(|b| matches!(b.kind, BlockKind::H1 | BlockKind::H2 | BlockKind::H3))
                .map(|b| crate::rich::strip(&b.text))
                .filter(|t| !t.trim().is_empty())
                .collect()
        })
        .unwrap_or_default()
}

pub fn draw(
    ui: &mut Ui,
    ws: &mut Workspace,
    b: &mut Block,
    ed: &mut EditorState,
    pal: &Palette,
    w: f32,
    cur_page: u64,
) -> Out {
    let mut out = Out::default();
    // the page being edited is lent to the editor, so a block cannot mirror its own page
    let own = b.link == Some(cur_page);
    let resolved = if own {
        Resolved::Cycle
    } else {
        synced::resolve(ws, b)
    };
    let (src_title, src_page) = match (&resolved, b.link) {
        (Resolved::Blocks { page, .. }, _) => (ws.display_title(*page), Some(*page)),
        (_, Some(l)) => (ws.display_title(l), Some(l)),
        _ => (synced::split_ref(&b.text).0.to_owned(), None),
    };
    let section = if b.link.is_none() {
        synced::split_ref(&b.text).1.to_owned()
    } else {
        b.text.trim().to_owned()
    };
    let accent = pal.accent;
    let frame = Frame::new()
        .fill(with_alpha(accent, 0.06))
        .stroke(Stroke::new(1.0, with_alpha(accent, 0.35)))
        .corner_radius(8.0)
        .inner_margin(Margin::symmetric(12, 8));
    frame.show(ui, |ui| {
        ui.set_width((w - 26.0).max(120.0));
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 4.0);
            let mut head = format!(
                "{}  Synced from {}",
                crate::lucide::LINK_2,
                if src_title.is_empty() {
                    "…"
                } else {
                    &src_title
                }
            );
            if !section.is_empty() {
                head.push_str(&format!("  ›  {section}"));
            } else if b.media != 0 {
                head.push_str("  ›  block");
            }
            ui.label(RichText::new(head).font(bold(12.5)).color(accent));
            if let Some(src) = src_page.filter(|_| !own) {
                if ui
                    .small_button("Open original")
                    .on_hover_text("Edit the source — every synced copy updates")
                    .clicked()
                {
                    ed.nav = Some(src);
                }
                if b.media == 0 {
                    let hs = headings(ws, src);
                    if !hs.is_empty() {
                        ui.menu_button("Section", |ui| {
                            if ui.button("Whole page").clicked() {
                                b.text.clear();
                                out.changed = true;
                                ui.close_menu();
                            }
                            for h in hs {
                                if ui.button(&h).clicked() {
                                    b.text = h;
                                    out.changed = true;
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                }
            }
            if matches!(resolved, Resolved::Blocks { .. })
                && ui
                    .small_button("Unsync")
                    .on_hover_text("Turn this into ordinary blocks")
                    .clicked()
            {
                let blocks = synced::unsynced_blocks(ws, b);
                if !blocks.is_empty() {
                    out.unsync = Some(blocks);
                    out.changed = true;
                }
            }
        });
        ui.add_space(4.0);
        match &resolved {
            Resolved::Blocks { blocks, .. } => {
                let base = blocks.iter().map(|x| x.indent).min().unwrap_or(0);
                let mut num = 0u32;
                for x in blocks.iter().take(400) {
                    let pad = (x.indent.saturating_sub(base) as f32) * 20.0;
                    if x.kind == BlockKind::Numbered {
                        num += 1;
                    } else {
                        num = 0;
                    }
                    let mut line = plain_line(x);
                    if x.kind == BlockKind::Numbered {
                        line = line.replacen("1.", &format!("{num}."), 1);
                    }
                    ui.horizontal_top(|ui| {
                        ui.add_space(pad);
                        match x.kind {
                            BlockKind::Divider => {
                                let (r, _) = ui.allocate_exact_size(
                                    vec2(ui.available_width(), 8.0),
                                    Sense::hover(),
                                );
                                ui.painter().hline(
                                    r.x_range(),
                                    r.center().y,
                                    Stroke::new(1.0, pal.border),
                                );
                            }
                            BlockKind::Callout | BlockKind::Quote | BlockKind::Code => {
                                Frame::new()
                                    .fill(if x.kind == BlockKind::Code {
                                        pal.code_bg
                                    } else {
                                        pal.callout
                                    })
                                    .corner_radius(6.0)
                                    .inner_margin(Margin::symmetric(8, 4))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width() - 8.0);
                                        let f = if x.kind == BlockKind::Code {
                                            egui::FontId::monospace(13.0)
                                        } else {
                                            body(15.0)
                                        };
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(line).font(f).color(pal.text),
                                            )
                                            .wrap(),
                                        );
                                    });
                            }
                            k => {
                                let font = match k {
                                    BlockKind::H1 => bold(24.0),
                                    BlockKind::H2 => bold(19.0),
                                    BlockKind::H3 => bold(16.5),
                                    _ => body(15.5),
                                };
                                let shown = if matches!(k, BlockKind::H1) {
                                    crate::rich::strip(&x.text)
                                } else {
                                    line
                                };
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(shown).font(font).color(pal.text),
                                    )
                                    .wrap(),
                                );
                            }
                        }
                    });
                }
            }
            Resolved::Missing => {
                ui.label(
                    RichText::new(if b.link.is_none() && b.text.is_empty() {
                        "Choose a page to sync from…"
                    } else {
                        "The synced source was moved or deleted."
                    })
                    .font(body(13.5))
                    .color(pal.danger),
                );
                if b.link.is_none() && ui.small_button("Pick a page").clicked() {
                    ed.req = Some(crate::editor::Req::PickLink(b.id));
                }
            }
            Resolved::Cycle => {
                ui.label(
                    RichText::new(if own {
                        "A block cannot mirror its own page — pick another source."
                    } else {
                        "This synced content loops back to itself."
                    })
                    .font(body(13.5))
                    .color(pal.danger),
                );
            }
        }
    });
    let _ = (Align2::LEFT_TOP, pos2(0.0, 0.0));
    out
}

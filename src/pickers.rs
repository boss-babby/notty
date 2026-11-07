//! Page icon picker (emoji with search + any pasted emoji, Lucide icons, custom picture), cover picker (gradients, upload, link)
//! and the cover banner itself (picture or gradient, reposition by dragging).

use egui::{pos2, vec2, Align2, Color32, Context, CursorIcon, Id, Key, Margin, Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui};

use crate::editor::EditorState;
use crate::emoji;
use crate::icons::{self, ImgState};
use crate::lucide as l;
use crate::media;
use crate::model::Page;
use crate::platform::{self, PickTarget};
use crate::theme::{bold, body, lerp_color, Palette, COVERS};
use crate::widgets::{self, gradient_rect, menu_item, popup_area, section_label, with_alpha};

fn tabs(ui: &mut Ui, pal: &Palette, labels: &[&str], tab: &mut u8) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, label) in labels.iter().enumerate() {
            let g = ui.painter().layout_no_wrap((*label).to_owned(), bold(13.0), pal.text);
            let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 20.0, 28.0), Sense::click());
            let on = *tab as usize == i;
            let t = ui.ctx().animate_bool_with_time(resp.id, on, 0.15);
            let h = ui.ctx().animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.1);
            ui.painter().rect_filled(r, 6.0, with_alpha(pal.hover, h.max(t)));
            ui.painter().text(r.center(), Align2::CENTER_CENTER, label, bold(13.0), lerp_color(pal.muted, pal.text, t.max(h)));
            if on {
                ui.painter().hline(r.x_range().shrink(8.0), r.bottom() - 1.0, Stroke::new(2.0_f32, pal.accent));
            }
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                *tab = i as u8;
            }
        }
    });
}

fn cell(ui: &mut Ui, pal: &Palette, icon: &str, lucide: bool) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(38.0, 38.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
    ui.painter().rect_filled(r, 8.0, with_alpha(pal.hover, t));
    if lucide {
        ui.painter().text(r.center(), Align2::CENTER_CENTER, icon, icons::font(20.0 + 3.0 * t), lerp_color(pal.text, pal.accent, t));
    } else {
        icons::paint_page_icon(ui, r.center(), icon, 24.0 + 3.0 * t, pal.text);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

/// A text field with a button that submits on Enter too. Returns the submitted text.
fn link_field(ui: &mut Ui, pal: &Palette, key: Id, hint: &str, button: &str) -> Option<String> {
    let ctx = ui.ctx().clone();
    let mut buf: String = ctx.data(|d| d.get_temp(key)).unwrap_or_default();
    let mut out = None;
    ui.horizontal(|ui| {
        let r = ui.add(TextEdit::singleline(&mut buf).id(key.with("te")).hint_text(hint).desired_width(190.0).margin(Margin::symmetric(8, 7)));
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if (widgets::ghost_button(ui, pal, button).clicked() || enter) && !buf.trim().is_empty() {
            out = Some(media::normalize_url(&buf));
        }
    });
    if out.is_some() {
        buf.clear();
    }
    ctx.data_mut(|d| d.insert_temp(key, buf));
    out
}

pub fn icon_picker(ctx: &Context, page: &mut Page, ed: &mut EditorState, pal: &Palette) -> bool {
    let Some(pos) = ed.icon_picker else { return false };
    let mut changed = false;
    let mut chosen: Option<String> = None;
    let mut remove = false;
    let tab_id = Id::new("icon_tab");
    let q_id = Id::new("icon_q");
    let page_id = page.id;
    let cur_custom = page.icon.starts_with("img:");
    let open = popup_area(ctx, Id::new("icon_picker"), pos, pal, &mut ed.fresh, 344.0, |ui| {
        let mut tab: u8 = ui.ctx().data(|d| d.get_temp(tab_id)).unwrap_or(0);
        tabs(ui, pal, &["Emoji", "Icons", "Custom"], &mut tab);
        ui.ctx().data_mut(|d| d.insert_temp(tab_id, tab));
        ui.add_space(6.0);
        match tab {
            0 => {
                let mut q: String = ui.ctx().data(|d| d.get_temp(q_id)).unwrap_or_default();
                let r = ui.add(TextEdit::singleline(&mut q).id(q_id.with("te")).hint_text("Search, or type / paste any emoji…").desired_width(f32::INFINITY).margin(Margin::symmetric(8, 7)));
                let custom = emoji::first_emoji(&q);
                let results = if q.trim().is_empty() { vec![] } else { emoji::search(&q) };
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    if let Some(c) = custom.clone().or_else(|| results.first().map(|s| (*s).to_owned())) {
                        chosen = Some(c);
                    }
                }
                if let Some(c) = &custom {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let (rr, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                        icons::paint_page_icon(ui, rr.center(), c, 24.0, pal.text);
                        if widgets::ghost_button(ui, pal, "Use this emoji").clicked() {
                            chosen = Some(c.clone());
                        }
                    });
                }
                ui.ctx().data_mut(|d| d.insert_temp(q_id, q.clone()));
                ui.add_space(4.0);
                ScrollArea::vertical().max_height(250.0).auto_shrink([false, true]).show(ui, |ui| {
                    if q.trim().is_empty() {
                        for (name, _, list) in emoji::CATS {
                            section_label(ui, pal, &name.to_uppercase());
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
                                for (e, _) in list.iter() {
                                    if cell(ui, pal, e, false) {
                                        chosen = Some((*e).to_owned());
                                    }
                                }
                            });
                        }
                    } else if results.is_empty() && custom.is_none() {
                        ui.add_space(14.0);
                        ui.label(RichText::new("No emoji found. Paste any emoji into the field to use it.").font(body(13.0)).color(pal.muted));
                    } else {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
                            for e in results {
                                if cell(ui, pal, e, false) {
                                    chosen = Some(e.to_owned());
                                }
                            }
                        });
                    }
                });
            }
            1 => {
                ScrollArea::vertical().max_height(290.0).auto_shrink([false, true]).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
                        for g in l::PAGE_ICONS.iter() {
                            if cell(ui, pal, g, true) {
                                chosen = Some((*g).to_owned());
                            }
                        }
                    });
                });
            }
            _ => {
                section_label(ui, pal, "UPLOAD YOUR OWN ICON");
                if platform::can_pick_files() {
                    if widgets::primary_button(ui, pal, "Choose an image…", 200.0).clicked() {
                        platform::pick_file(ui.ctx(), PickTarget::Icon { page: page_id }, true);
                    }
                } else {
                    ui.label(RichText::new("Drop an image file onto the window, then choose “Use as icon”.").font(body(12.5)).color(pal.muted));
                }
                ui.add_space(8.0);
                section_label(ui, pal, "OR PASTE AN IMAGE LINK");
                if let Some(u) = link_field(ui, pal, Id::new("icon_link"), "https://…/logo.png", "Use link") {
                    chosen = Some(format!("img:{u}"));
                }
                ui.add_space(8.0);
                ui.label(RichText::new("Square PNG, JPG, GIF or WebP pictures work best. Custom icons appear everywhere a page icon does.").font(body(12.0)).color(pal.muted));
                if cur_custom {
                    ui.add_space(4.0);
                    ui.label(RichText::new("This page currently uses a custom icon.").font(body(12.0)).color(pal.accent));
                }
            }
        }
        ui.add_space(4.0);
        if menu_item(ui, pal, l::X, "Remove icon", None, false, false).clicked() {
            remove = true;
        }
    });
    let done = chosen.is_some() || remove;
    if let Some(e) = chosen {
        page.icon = e;
        changed = true;
    }
    if remove {
        page.icon.clear();
        changed = true;
    }
    if !open || done {
        ed.icon_picker = None;
        ctx.data_mut(|d| d.insert_temp(q_id, String::new()));
    }
    changed
}

pub fn cover_picker(ctx: &Context, page: &mut Page, ed: &mut EditorState, pal: &Palette) -> bool {
    let Some(pos) = ed.cover_picker else { return false };
    let mut changed = false;
    let mut chosen: Option<usize> = None;
    let mut link: Option<String> = None;
    let mut remove = false;
    let mut reposition = false;
    let tab_id = Id::new("cover_tab");
    let page_id = page.id;
    let cur_grad = page.cover;
    let has_cover = page.has_cover();
    let has_img = !page.cover_img.is_empty();
    let open = popup_area(ctx, Id::new("cover_picker"), pos, pal, &mut ed.fresh, 330.0, |ui| {
        let mut tab: u8 = ui.ctx().data(|d| d.get_temp(tab_id)).unwrap_or(0);
        tabs(ui, pal, &["Gallery", "Upload", "Link"], &mut tab);
        ui.ctx().data_mut(|d| d.insert_temp(tab_id, tab));
        ui.add_space(6.0);
        match tab {
            0 => {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                    for (i, (a, b)) in COVERS.iter().enumerate() {
                        let (r, resp) = ui.allocate_exact_size(vec2(60.0, 38.0), Sense::click());
                        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                        let rr = r.expand(t * 2.0);
                        gradient_rect(ui, rr, Color32::from_rgb(a.0, a.1, a.2), Color32::from_rgb(b.0, b.1, b.2));
                        if cur_grad == Some(i) && !has_img {
                            ui.painter().rect_stroke(rr, 2.0, Stroke::new(2.0_f32, pal.accent), egui::StrokeKind::Outside);
                        }
                        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            chosen = Some(i);
                        }
                    }
                });
            }
            1 => {
                section_label(ui, pal, "UPLOAD FROM THIS DEVICE");
                if platform::can_pick_files() {
                    if widgets::primary_button(ui, pal, "Choose an image…", 220.0).clicked() {
                        platform::pick_file(ui.ctx(), PickTarget::Cover { page: page_id }, true);
                    }
                    ui.add_space(6.0);
                    ui.label(RichText::new("PNG, JPG, GIF or WebP. Wide images (1500 × 600 or larger) look best.").font(body(12.0)).color(pal.muted));
                } else {
                    ui.label(RichText::new("Drag an image file onto the window, then choose “Set as cover”.").font(body(12.5)).color(pal.muted));
                }
            }
            _ => {
                section_label(ui, pal, "IMAGE LINK");
                link = link_field(ui, pal, Id::new("cover_link"), "https://…/photo.jpg", "Set cover");
                ui.add_space(4.0);
                ui.label(RichText::new("Works with any direct link to an image (right-click an image on the web → Copy image address).").font(body(12.0)).color(pal.muted));
            }
        }
        if has_cover {
            ui.add_space(6.0);
            if has_img && menu_item(ui, pal, l::MOVE_VERTICAL, "Reposition", None, false, false).clicked() {
                reposition = true;
            }
            if menu_item(ui, pal, l::TRASH_2, "Remove cover", None, false, true).clicked() {
                remove = true;
            }
        }
    });
    let done = chosen.is_some() || link.is_some() || remove || reposition;
    if let Some(i) = chosen {
        page.cover = Some(i);
        page.cover_img.clear();
        changed = true;
    }
    if let Some(u) = link {
        page.cover_img = u;
        page.cover_pos = 0.5;
        page.cover = None;
        changed = true;
    }
    if remove {
        page.cover = None;
        page.cover_img.clear();
        changed = true;
    }
    if reposition {
        ed.cover_reposition = true;
    }
    if !open || done {
        ed.cover_picker = None;
    }
    changed
}

/// The cover banner. Returns true when the page changed.
pub fn cover(ui: &mut Ui, page: &mut Page, ed: &mut EditorState, pal: &Palette, avail: f32) -> bool {
    let mut changed = false;
    if !page.has_cover() {
        return false;
    }
    let has_img = !page.cover_img.is_empty();
    let repos = ed.cover_reposition && has_img;
    // only a drag surface while repositioning, otherwise drags on the cover start the page selection rectangle
    let (rect, bg) = ui.allocate_exact_size(vec2(avail, 220.0), if repos { Sense::drag() } else { Sense::hover() });
    if has_img {
        let uri = media::src_uri(&page.cover_img);
        let (st, size) = icons::image_size(ui, &uri, rect.size());
        gradient_rect(ui, rect, pal.hover, pal.active);
        match st {
            ImgState::Ready => {
                icons::paint_cover(ui, rect, &uri, page.cover_pos);
                if repos && bg.dragged() {
                    if let Some(ts) = size {
                        let shown_h = (rect.width() * ts.y / ts.x).max(rect.height());
                        let excess = (shown_h - rect.height()).max(1.0);
                        page.cover_pos = (page.cover_pos - bg.drag_delta().y / excess).clamp(0.0, 1.0);
                        changed = true;
                    }
                }
            }
            ImgState::Loading => {
                ui.put(Rect::from_center_size(rect.center(), vec2(26.0, 26.0)), egui::Spinner::new());
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
            }
            ImgState::Failed => {
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, "Couldn't load the cover image", body(14.0), pal.muted);
            }
        }
    } else if let Some(ci) = page.cover {
        let (a, b) = COVERS[ci % COVERS.len()];
        gradient_rect(ui, rect, Color32::from_rgb(a.0, a.1, a.2), Color32::from_rgb(b.0, b.1, b.2));
    }
    if repos {
        ui.ctx().set_cursor_icon(CursorIcon::Grab);
        ui.painter().rect_filled(Rect::from_center_size(pos2(rect.center().x, rect.top() + 24.0), vec2(250.0, 28.0)), 14.0, Color32::from_black_alpha(150));
        ui.painter().text(pos2(rect.center().x, rect.top() + 24.0), Align2::CENTER_CENTER, "Drag the image to reposition", body(13.0), Color32::WHITE);
    }
    // phones have no hover: the buttons are always there (and wrap onto a second row when the banner is narrow)
    let hovered = crate::touch::reveal(ui.rect_contains_pointer(rect)) || repos;
    let t = ui.ctx().animate_bool_with_time(Id::new(("cover_h", page.id)), hovered, 0.15);
    if t > 0.01 {
        let labels: Vec<(&str, &str)> = if repos {
            vec![("cs", "Save position")]
        } else if has_img {
            vec![("cc", "Change cover"), ("cp", "Reposition"), ("cr", "Remove")]
        } else {
            vec![("cc", "Change cover"), ("cr", "Remove")]
        };
        let bh = if crate::touch::touchy() { 34.0 } else { 28.0 };
        let right = rect.right() - 12.0;
        let mut x = right;
        let mut y = rect.bottom() - bh - 14.0;
        for (id, label) in labels.iter().rev() {
            let gw = ui.painter().layout_no_wrap((*label).to_owned(), body(12.5), pal.text).size().x;
            let bw = gw + 22.0;
            if x - bw < rect.left() + 12.0 && x < right {
                // does not fit next to its neighbour: next row up
                x = right;
                y -= bh + 8.0;
            }
            let rr = Rect::from_min_size(pos2(x - bw, y), vec2(bw, bh));
            x = rr.left() - 8.0;
            let resp = ui.interact(rr, Id::new((*id, page.id)), Sense::click());
            let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() || resp.is_pointer_button_down_on(), 0.1);
            ui.painter().rect_filled(rr, 6.0, with_alpha(lerp_color(pal.bg, pal.hover, h), t * 0.92));
            ui.painter().text(rr.center(), Align2::CENTER_CENTER, label, body(12.5), with_alpha(pal.text, t));
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                match *id {
                    "cc" => {
                        ed.cover_picker = Some(rr.left_bottom() + vec2(-60.0, 6.0));
                        ed.fresh = true;
                    }
                    "cp" => ed.cover_reposition = true,
                    "cs" => ed.cover_reposition = false,
                    _ => {
                        page.cover = None;
                        page.cover_img.clear();
                        ed.cover_reposition = false;
                        changed = true;
                    }
                }
            }
        }
    }
    changed
}

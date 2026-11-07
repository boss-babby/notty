//! The "add something" card: upload a photo / PDF / file, paste a link or embed a service.
//!
//! One consistent design for every place that asks for media — image, file and PDF blocks, embeds, bookmarks, page icons and covers:
//! a card with an icon badge and a short explanation, a segmented *Upload | Link* switch, a dashed drop zone that lights up while a
//! file is dragged over the window, rounded link field with an inline action, and chips that show what is accepted.
//! Everything scales to phones: larger tap targets, stacked fields and no fixed widths wider than the screen.

use egui::{pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, Frame, Id, Key, Margin, Pos2, Rect, RichText, Sense, Shape, Stroke, TextEdit, Ui};

use crate::icons;
use crate::lucide as l;
use crate::media;
use crate::model::Block;
use crate::platform::{self, PickTarget};
use crate::theme::{bold, body, lerp_color, Palette};
use crate::widgets::{self, with_alpha};

/// What the card is asking for.
pub struct Spec<'a> {
    pub icon: &'a str,
    pub title: &'a str,
    pub subtitle: &'a str,
    /// `Some((images_only, accepted-format chips))` when a file can be uploaded.
    pub upload: Option<(bool, &'a [&'a str])>,
    pub link_hint: &'a str,
    pub link_button: &'a str,
    /// Services that are recognised when pasting a link (glyph, name).
    pub providers: &'a [(&'a str, &'a str)],
}

fn roomy() -> bool {
    crate::touch::touchy()
}

/// Closed rounded-rectangle outline as a point list (for dashed strokes).
fn rounded_path(r: Rect, rad: f32) -> Vec<Pos2> {
    let mut pts = Vec::with_capacity(40);
    let corners = [
        (pos2(r.right() - rad, r.top() + rad), -std::f32::consts::FRAC_PI_2),
        (pos2(r.right() - rad, r.bottom() - rad), 0.0),
        (pos2(r.left() + rad, r.bottom() - rad), std::f32::consts::FRAC_PI_2),
        (pos2(r.left() + rad, r.top() + rad), std::f32::consts::PI),
    ];
    for (c, a0) in corners {
        for i in 0..=6 {
            let a = a0 + (i as f32 / 6.0) * std::f32::consts::FRAC_PI_2;
            pts.push(pos2(c.x + rad * a.cos(), c.y + rad * a.sin()));
        }
    }
    pts.push(pts[0]);
    pts
}

/// Segmented switch ("Upload | Link"). Returns true when the selection changed.
pub fn segmented(ui: &mut Ui, pal: &Palette, id: Id, labels: &[&str], sel: &mut usize) -> bool {
    let ctx = ui.ctx().clone();
    let h = if roomy() { 36.0 } else { 30.0 };
    let widths: Vec<f32> = labels.iter().map(|s| ui.painter().layout_no_wrap((*s).to_owned(), bold(13.0), pal.text).size().x + 26.0).collect();
    let total: f32 = widths.iter().sum::<f32>() + 6.0;
    let (rect, _) = ui.allocate_exact_size(vec2(total, h), Sense::hover());
    ui.painter().rect_filled(rect, h / 2.0, pal.hover);
    let mut x = rect.left() + 3.0;
    let mut starts = vec![];
    for w in &widths {
        starts.push(x);
        x += w;
    }
    let cur_x = ctx.animate_value_with_time(id.with("pill_x"), starts[(*sel).min(starts.len() - 1)], 0.16);
    let cur_w = ctx.animate_value_with_time(id.with("pill_w"), widths[(*sel).min(widths.len() - 1)], 0.16);
    let pill = Rect::from_min_size(pos2(cur_x, rect.top() + 3.0), vec2(cur_w, h - 6.0));
    ui.painter().rect_filled(pill.translate(vec2(0.0, 1.0)), (h - 6.0) / 2.0, Color32::from_black_alpha(18));
    ui.painter().rect_filled(pill, (h - 6.0) / 2.0, pal.bg);
    let mut changed = false;
    for (i, label) in labels.iter().enumerate() {
        let r = Rect::from_min_size(pos2(starts[i], rect.top() + 3.0), vec2(widths[i], h - 6.0));
        let resp = ui.interact(r, id.with(i), Sense::click());
        let on = i == *sel;
        let hov = ctx.animate_bool_with_time(resp.id, resp.hovered(), 0.1);
        ui.painter().text(r.center(), Align2::CENTER_CENTER, label, bold(13.0), lerp_color(pal.muted, pal.text, if on { 1.0 } else { hov * 0.8 }));
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() && !on {
            *sel = i;
            changed = true;
        }
    }
    changed
}

/// A big dashed drop target. Returns true when it was clicked (open the file dialog).
#[allow(clippy::too_many_arguments)]
pub fn drop_zone(ui: &mut Ui, pal: &Palette, id: Id, w: f32, title: &str, sub: &str, chips: &[&str], clickable: bool) -> bool {
    let ctx = ui.ctx().clone();
    let h = if roomy() { 158.0 } else { 146.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), if clickable { Sense::click() } else { Sense::hover() });
    let dragging = ctx.input(|i| !i.raw.hovered_files.is_empty());
    let hov = ctx.animate_bool_with_time(id.with("hov"), (resp.hovered() && clickable) || dragging, 0.16);
    let down = clickable && resp.is_pointer_button_down_on();
    if dragging {
        ctx.request_repaint();
    }
    let p = ui.painter();
    let r = if down { rect.shrink(1.5) } else { rect };
    p.rect_filled(r, 14.0, pal.bg);
    p.rect_filled(r, 14.0, with_alpha(pal.accent, 0.035 + 0.10 * hov));
    let border = lerp_color(with_alpha(pal.muted, 0.55), pal.accent, hov);
    let offset = if dragging { (ctx.input(|i| i.time) as f32 * 24.0) % 12.0 } else { 0.0 };
    p.extend(Shape::dashed_line_with_offset(&rounded_path(r.shrink(0.75), 14.0), Stroke::new(1.4_f32 + hov * 0.4, border), &[6.0], &[5.0], offset));

    // icon badge
    let lift = -3.0 * hov;
    let c = pos2(r.center().x, r.top() + 36.0 + lift);
    p.circle_filled(c + vec2(0.0, 1.5), 24.0, Color32::from_black_alpha((14.0 + 10.0 * hov) as u8));
    p.circle_filled(c, 24.0, lerp_color(pal.hover, with_alpha(pal.accent, 0.18), hov));
    icons::glyph(p, c, 21.0, if dragging { l::ARROW_DOWN_FROM_LINE } else { l::UPLOAD }, lerp_color(pal.muted, pal.accent, 0.6 + 0.4 * hov));
    p.text(pos2(r.center().x, r.top() + 78.0), Align2::CENTER_CENTER, title, bold(14.5), if dragging { pal.accent } else { pal.text });
    p.text(pos2(r.center().x, r.top() + 98.0), Align2::CENTER_CENTER, sub, body(12.5), pal.muted);
    if !chips.is_empty() {
        let gal: Vec<_> = chips.iter().map(|c| p.layout_no_wrap((*c).to_owned(), bold(10.5), pal.muted)).collect();
        let total: f32 = gal.iter().map(|g| g.size().x + 14.0).sum::<f32>() + 5.0 * (gal.len() as f32 - 1.0);
        let mut x = r.center().x - total / 2.0;
        let y = r.top() + 122.0;
        for g in gal {
            let cr = Rect::from_min_size(pos2(x, y - 9.0), vec2(g.size().x + 14.0, 18.0));
            p.rect_filled(cr, 9.0, pal.hover);
            p.galley(pos2(cr.left() + 7.0, cr.center().y - g.size().y / 2.0), g, pal.muted);
            x = cr.right() + 5.0;
        }
    }
    clickable && resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

/// Rounded link field with an inline action button (stacked on narrow screens). Returns the normalised URL on submit.
pub fn url_field(ui: &mut Ui, pal: &Palette, key: Id, w: f32, hint: &str, button: &str) -> Option<String> {
    let ctx = ui.ctx().clone();
    let mut buf: String = ctx.data(|d| d.get_temp(key)).unwrap_or_default();
    let mut out = None;
    let stacked = w < 330.0;
    let bw = if stacked { w } else { (ui.painter().layout_no_wrap(button.to_owned(), bold(14.5), pal.text).size().x + 34.0).max(92.0) };
    let fw = if stacked { w } else { (w - bw - 8.0).max(80.0) };
    let mut go = false;
    let field = |ui: &mut Ui, buf: &mut String, go: &mut bool| {
        Frame::new().fill(pal.hover).stroke(Stroke::new(1.0_f32, pal.border)).corner_radius(CornerRadius::same(9)).inner_margin(Margin::symmetric(10, if roomy() { 8 } else { 3 })).show(ui, |ui| {
            ui.set_width(fw - 22.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(l::LINK_2).font(icons::font(15.0)).color(pal.muted));
                let r = ui.add(TextEdit::singleline(buf).id(key.with("te")).frame(false).hint_text(hint).desired_width(f32::INFINITY).font(body(14.0)).margin(Margin::symmetric(2, 6)));
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    *go = true;
                }
            });
        });
    };
    if stacked {
        field(ui, &mut buf, &mut go);
        ui.add_space(6.0);
        if widgets::primary_button(ui, pal, button, bw).clicked() {
            go = true;
        }
    } else {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            field(ui, &mut buf, &mut go);
            if widgets::primary_button(ui, pal, button, bw).clicked() {
                go = true;
            }
        });
    }
    if go && !buf.trim().is_empty() {
        out = Some(media::normalize_url(&buf));
        buf.clear();
    }
    ctx.data_mut(|d| d.insert_temp(key, buf));
    out
}

/// Row of small "recognised service" chips.
fn provider_row(ui: &mut Ui, pal: &Palette, items: &[(&str, &str)], w: f32) {
    if items.is_empty() {
        return;
    }
    ui.add_space(8.0);
    let gap = 6.0;
    let (mut x, mut y) = (0.0_f32, 0.0_f32);
    let row_h = 24.0;
    let mut placed: Vec<(Rect, usize)> = vec![];
    let sizes: Vec<f32> = items.iter().map(|(_, n)| ui.painter().layout_no_wrap((*n).to_owned(), body(12.0), pal.muted).size().x + 36.0).collect();
    for (i, sw) in sizes.iter().enumerate() {
        if x + sw > w && x > 0.0 {
            x = 0.0;
            y += row_h + gap;
        }
        placed.push((Rect::from_min_size(pos2(x, y), vec2(*sw, row_h)), i));
        x += sw + gap;
    }
    let total_h = y + row_h;
    let (area, _) = ui.allocate_exact_size(vec2(w, total_h), Sense::hover());
    for (r, i) in placed {
        let r = r.translate(area.min.to_vec2());
        ui.painter().rect_filled(r, 12.0, pal.hover);
        icons::glyph(ui.painter(), pos2(r.left() + 14.0, r.center().y), 12.5, items[i].0, pal.muted);
        ui.painter().text(pos2(r.left() + 27.0, r.center().y), Align2::LEFT_CENTER, items[i].1, body(12.0), pal.muted);
    }
}

fn badge(ui: &mut Ui, pal: &Palette, icon: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
    ui.painter().rect_filled(r, 11.0, with_alpha(pal.accent, 0.13));
    icons::glyph(ui.painter(), r.center(), 20.0, icon, pal.accent);
}

/// Upload + link body, shared by the block card and the icon / cover pickers. `target` receives the picked file.
/// Returns the submitted URL (already normalised).
pub fn body_ui(ui: &mut Ui, pal: &Palette, id: Id, spec: &Spec, target: Option<PickTarget>, w: f32, show_switch: bool) -> Option<String> {
    let ctx = ui.ctx().clone();
    let can_upload = spec.upload.is_some() && target.is_some();
    let tab_id = id.with("tab");
    let mut tab: usize = if can_upload { ctx.data(|d| d.get_temp(tab_id)).unwrap_or(0) } else { 1 };
    let mut out = None;
    if can_upload && show_switch {
        segmented(ui, pal, id.with("seg"), &["Upload", "Link"], &mut tab);
        ui.add_space(10.0);
    }
    if can_upload && tab == 0 {
        let (images_only, chips) = spec.upload.unwrap();
        let can_pick = platform::can_pick_files();
        let (title, sub) = if can_pick {
            (if roomy() { "Tap to choose a file" } else { "Click to upload" }, if roomy() { "from your phone or tablet" } else { "or drag & drop a file here" })
        } else {
            ("Drop a file here", "drag it onto this window")
        };
        if drop_zone(ui, pal, id.with("zone"), w, title, sub, chips, can_pick) {
            if let Some(t) = target {
                platform::pick_file(ui.ctx(), t, images_only);
            }
        }
        ui.add_space(8.0);
        ui.label(RichText::new(if images_only { "Pictures are resized to keep your workspace small." } else { "Files are stored inside your workspace — no account, no cloud." }).font(body(12.0)).color(pal.muted));
    } else {
        out = url_field(ui, pal, id.with("url"), w, spec.link_hint, spec.link_button);
        provider_row(ui, pal, spec.providers, w);
    }
    ctx.data_mut(|d| d.insert_temp(tab_id, tab));
    out
}

/// The full card shown in a media block that has no content yet. Returns true when the block changed.
pub fn block_card(ui: &mut Ui, b: &mut Block, spec: &Spec, target: Option<PickTarget>, pal: &Palette, w: f32) -> bool {
    let id = Id::new(("media_card", b.id));
    let mut url = None;
    let inner_w = (w - 2.0 * 18.0 - 2.0).max(150.0);
    Frame::new()
        .fill(pal.bg)
        .stroke(Stroke::new(1.0_f32, pal.border))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::same(18))
        .shadow(egui::epaint::Shadow { offset: [0, 2], blur: 10, spread: 0, color: Color32::from_black_alpha(14) })
        .show(ui, |ui| {
            ui.set_width(inner_w);
            ui.horizontal(|ui| {
                badge(ui, pal, spec.icon);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new(spec.title).font(bold(15.5)).color(pal.text));
                    ui.add(egui::Label::new(RichText::new(spec.subtitle).font(body(12.5)).color(pal.muted)).wrap());
                });
            });
            ui.add_space(14.0);
            url = body_ui(ui, pal, id, spec, target, inner_w, true);
        });
    if let Some(u) = url {
        b.url = u;
        b.media = 0;
        return true;
    }
    false
}

/// Compact variant for popovers (icon / cover pickers): no card chrome, drop zone + link field in one column.
pub fn popover_body(ui: &mut Ui, pal: &Palette, id: Id, spec: &Spec, target: Option<PickTarget>, w: f32) -> Option<String> {
    body_ui(ui, pal, id, spec, target, w, true)
}

#[allow(dead_code)]
pub fn section_hint(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(text).font(body(12.0)).color(pal.muted));
}

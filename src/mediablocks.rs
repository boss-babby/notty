//! Media blocks: images (upload or link, resizable, captions), website / video / map embeds (live iframe on the web build),
//! bookmarks (link cards) and file attachments.

use egui::{pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, Frame, Id, Key, Margin, Rect, RichText, Sense, Shape, Stroke, TextEdit, Ui};

use crate::editor::EditorState;
use crate::icons::{self, ImgState};
use crate::lucide as l;
use crate::media::{self, EmbedKind};
use crate::model::{Block, BlockKind};
use crate::platform::{self, EmbedRect, PickTarget};
use crate::theme::{bold, body, lerp_color, Palette};
use crate::widgets::{self, with_alpha};

pub fn draw(ui: &mut Ui, b: &mut Block, page_id: u64, ed: &mut EditorState, pal: &Palette, w: f32) -> bool {
    match b.kind {
        BlockKind::Image => image(ui, b, page_id, pal, w),
        BlockKind::Embed => embed(ui, b, ed, pal, w),
        BlockKind::Bookmark => bookmark(ui, b, pal, w),
        BlockKind::File => file(ui, b, page_id, ed, pal, w),
        _ => false,
    }
}

/// Small floating button; returns true when clicked.
fn chip(ui: &mut Ui, pal: &Palette, rect: Rect, id: Id, label: &str, alpha: f32, active: bool) -> bool {
    let resp = ui.interact(rect, id, Sense::click());
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
    let fill = if active { pal.accent } else { lerp_color(pal.bg, pal.hover, h) };
    ui.painter().rect_filled(rect, 6.0, with_alpha(fill, 0.95 * alpha));
    ui.painter().rect_stroke(rect, 6.0, Stroke::new(1.0_f32, with_alpha(pal.border, alpha)), egui::StrokeKind::Inside);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, body(12.5), with_alpha(if active { Color32::WHITE } else { pal.text }, alpha));
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked() && alpha > 0.5
}

/// Right-aligned row of chips inside `area` (top edge). Returns the index of the clicked chip.
fn chips(ui: &mut Ui, pal: &Palette, area: Rect, key: Id, items: &[(&str, bool)], alpha: f32) -> Option<usize> {
    let mut x = area.right() - 8.0;
    let mut hit = None;
    for (i, (label, active)) in items.iter().enumerate().rev() {
        let gw = ui.painter().layout_no_wrap((*label).to_owned(), body(12.5), pal.text).size().x;
        let r = Rect::from_min_size(pos2(x - gw - 16.0, area.top() + 6.0), vec2(gw + 16.0, 24.0));
        x = r.left() - 6.0;
        if chip(ui, pal, r, key.with(i), label, alpha, *active) {
            hit = Some(i);
        }
    }
    hit
}

use crate::uploader::{self, Spec};

const IMG_CHIPS: &[&str] = &["PNG", "JPG", "GIF", "WebP"];
const FILE_CHIPS: &[&str] = &["PDF", "DOCX", "XLSX", "ZIP", "MP3", "any file"];
const PROVIDERS: &[(&str, &str)] = &[
    (l::YOUTUBE, "YouTube"),
    (l::VIDEO, "Vimeo"),
    (l::MONITOR_PLAY, "Loom"),
    (l::MUSIC, "Spotify"),
    (l::FIGMA, "Figma"),
    (l::MAP, "Google Maps"),
    (l::CODE_XML, "CodePen"),
    (l::FILE_TEXT, "Google Docs"),
];

/// "Add a …" card: segmented Upload | Link switch, dashed drop zone, rounded link field (see `uploader.rs`).
fn placeholder(ui: &mut Ui, b: &mut Block, spec: &Spec, target: Option<PickTarget>, pal: &Palette, w: f32) -> bool {
    uploader::block_card(ui, b, spec, target, pal, w)
}

fn caption(ui: &mut Ui, b: &mut Block, pal: &Palette, width: f32, open_req: bool) -> bool {
    let cap_id = Id::new(("cap", b.id));
    let open_id = Id::new(("cap_open", b.id));
    let ctx = ui.ctx().clone();
    let mut open: bool = ctx.data(|d| d.get_temp(open_id)).unwrap_or(false);
    if open_req {
        open = true;
        ctx.memory_mut(|m| m.request_focus(cap_id));
    }
    let focused = ctx.memory(|m| m.has_focus(cap_id));
    if !open && !focused && b.text.is_empty() {
        ui.add_space(6.0);
        return false;
    }
    let r = ui.add(TextEdit::singleline(&mut b.text).id(cap_id).frame(false).font(body(13.5)).text_color(pal.muted).hint_text("Write a caption…").desired_width(width).margin(Margin::symmetric(0, 4)));
    if !r.has_focus() && b.text.is_empty() && !open_req {
        open = false;
    }
    ctx.data_mut(|d| d.insert_temp(open_id, open));
    r.changed()
}

// ───────────────────────────── image ─────────────────────────────

fn image(ui: &mut Ui, b: &mut Block, page_id: u64, pal: &Palette, w: f32) -> bool {
    let src = b.src();
    if src.is_empty() {
        let ph = Spec { icon: l::IMAGE, title: "Add an image", subtitle: "Upload a photo from your device or paste an image address.", upload: Some((true, IMG_CHIPS)), link_hint: "Paste an image link…", link_button: "Add image", providers: &[] };
        return placeholder(ui, b, &ph, Some(PickTarget::BlockImage { page: page_id, block: b.id }), pal, w);
    }
    let ctx = ui.ctx().clone();
    let mut changed = false;
    let uri = media::src_uri(&src);
    let frac = if b.size == 0 { 1.0 } else { (b.size as f32 / 100.0).clamp(0.1, 1.0) };
    let mw = (w * frac).max(60.0);
    let (st, size) = icons::image_size(ui, &uri, vec2(mw, mw));
    let ratio = size.map(|s| s.y / s.x.max(1.0)).unwrap_or(0.5);
    let h = if st == ImgState::Ready { (mw * ratio).clamp(24.0, 1200.0) } else { (mw * 0.5).clamp(90.0, 180.0) };
    let (rect, _) = ui.allocate_exact_size(vec2(mw, h), Sense::hover());
    match st {
        ImgState::Ready => {
            icons::paint_image(ui, rect, &uri, 6.0);
        }
        ImgState::Loading => {
            ui.painter().rect_filled(rect, 6.0, pal.hover);
            ui.put(Rect::from_center_size(rect.center(), vec2(26.0, 26.0)), egui::Spinner::new());
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
        ImgState::Failed => {
            ui.painter().rect_filled(rect, 6.0, pal.hover);
            icons::glyph(ui.painter(), rect.center() + vec2(0.0, -14.0), 26.0, l::IMAGE, pal.muted);
            ui.painter().text(rect.center() + vec2(0.0, 14.0), Align2::CENTER_CENTER, "Couldn't load this image", body(13.5), pal.muted);
            let g = ui.painter().layout(src.clone(), body(11.5), with_alpha(pal.muted, 0.7), rect.width() - 24.0);
            ui.painter().galley(pos2(rect.center().x - g.size().x / 2.0, rect.center().y + 28.0), g, pal.muted);
        }
    }
    let hovered = ui.rect_contains_pointer(rect);
    let t = ctx.animate_bool_with_time(Id::new(("img_h", b.id)), hovered, 0.12);
    let mut cap_req = false;
    if t > 0.01 {
        let mut items: Vec<(&str, bool)> = vec![("Replace", false), ("Caption", false), ("25%", b.size == 25), ("50%", b.size == 50), ("75%", b.size == 75), ("100%", b.size == 0 || b.size == 100)];
        if b.media == 0 {
            items.push(("Open", false));
        }
        if let Some(i) = chips(ui, pal, rect, Id::new(("img_c", b.id)), &items, t) {
            match items[i].0 {
                "Replace" => {
                    b.url.clear();
                    b.media = 0;
                    changed = true;
                }
                "Caption" => cap_req = true,
                "25%" => {
                    b.size = 25;
                    changed = true;
                }
                "50%" => {
                    b.size = 50;
                    changed = true;
                }
                "75%" => {
                    b.size = 75;
                    changed = true;
                }
                "100%" => {
                    b.size = 0;
                    changed = true;
                }
                _ => platform::open_url(&ctx, &b.url),
            }
        }
    }
    // drag handle on the right edge
    let hr = Rect::from_min_size(pos2(rect.right() - 12.0, rect.center().y - 26.0), vec2(16.0, 52.0));
    let hresp = ui.interact(hr, Id::new(("img_rs", b.id)), Sense::drag());
    if t > 0.01 || hresp.dragged() {
        let pill = Rect::from_center_size(pos2(rect.right() - 7.0, rect.center().y), vec2(5.0, 40.0));
        ui.painter().rect_filled(pill, 3.0, with_alpha(Color32::from_black_alpha(150), t.max(if hresp.dragged() { 1.0 } else { 0.0 })));
        ui.painter().rect_stroke(pill, 3.0, Stroke::new(1.0_f32, with_alpha(Color32::WHITE, 0.6 * t)), egui::StrokeKind::Inside);
    }
    if hresp.hovered() || hresp.dragged() {
        ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
    }
    if hresp.dragged() {
        if let Some(p) = hresp.interact_pointer_pos() {
            let pct = ((p.x - rect.left()) / w * 100.0).clamp(10.0, 100.0);
            b.size = if pct > 96.0 { 0 } else { pct.round() as u8 };
            changed = true;
        }
    }
    changed |= caption(ui, b, pal, mw, cap_req);
    changed
}

// ───────────────────────────── embed ─────────────────────────────

fn play_button(ui: &Ui, c: egui::Pos2, r: f32) {
    ui.painter().circle_filled(c, r, Color32::from_black_alpha(165));
    let k = r / 30.0;
    ui.painter().add(Shape::convex_polygon(vec![pos2(c.x - 8.0 * k, c.y - 12.0 * k), pos2(c.x - 8.0 * k, c.y + 12.0 * k), pos2(c.x + 13.0 * k, c.y)], Color32::WHITE, Stroke::NONE));
}

fn embed(ui: &mut Ui, b: &mut Block, ed: &mut EditorState, pal: &Palette, w: f32) -> bool {
    if b.url.is_empty() {
        let ph = Spec { icon: l::GLOBE, title: "Embed anything", subtitle: "Videos, maps, designs, documents or any website — live inside your page.", upload: None, link_hint: "Paste a link to embed…", link_button: "Embed", providers: PROVIDERS };
        return placeholder(ui, b, &ph, None, pal, w);
    }
    let ctx = ui.ctx().clone();
    let mut changed = false;
    let info = media::embed_info(&b.url);
    let web = platform::live_preview_supported();
    let live = web && b.checked;
    let h = match info.kind {
        EmbedKind::Video => {
            let ratio = media::ASPECTS[(b.size as usize).min(media::ASPECTS.len() - 1)].1;
            (w / ratio).clamp(160.0, 720.0)
        }
        EmbedKind::Audio => 152.0,
        _ => 440.0,
    };
    // header strip
    let (hr, _) = ui.allocate_exact_size(vec2(w, 32.0), Sense::hover());
    icons::paint_image(ui, Rect::from_center_size(pos2(hr.left() + 10.0, hr.center().y), vec2(16.0, 16.0)), &media::favicon_url(&info.host), 3.0);
    let title = format!("{}  ·  {}", info.service, info.host);
    ui.painter().text(pos2(hr.left() + 26.0, hr.center().y), Align2::LEFT_CENTER, title, body(13.0), pal.muted);
    let is_video = info.kind == EmbedKind::Video;
    let opts_open: bool = ctx.data(|d| d.get_temp(Id::new(("vid_opts", b.id)))).unwrap_or(false);
    let mut items: Vec<(&str, bool)> = if web { vec![(if live { "Stop" } else { "Live" }, live), ("Open", false), ("Replace", false)] } else { vec![("Open", false), ("Replace", false)] };
    if is_video {
        items.insert(0, ("Edit", opts_open));
    }
    let hr2 = Rect::from_min_max(pos2(hr.left(), hr.top() - 6.0), hr.right_bottom());
    if let Some(i) = chips(ui, pal, hr2, Id::new(("emb_c", b.id)), &items, 1.0) {
        match items[i].0 {
            "Live" | "Stop" => {
                b.checked = !b.checked;
                changed = true;
            }
            "Edit" => ctx.data_mut(|d| d.insert_temp(Id::new(("vid_opts", b.id)), !opts_open)),
            "Open" => platform::open_url(&ctx, &b.url),
            _ => {
                b.url.clear();
                b.checked = false;
                changed = true;
            }
        }
    }
    // video options: aspect ratio, playback flags, start time
    if is_video && opts_open {
        let (orow, _) = ui.allocate_exact_size(vec2(w, 36.0), Sense::hover());
        ui.painter().rect_filled(orow, 8.0, with_alpha(pal.hover, 0.6));
        let mut x = orow.left() + 8.0;
        let bid = b.id;
        let pill = |ui: &mut Ui, x: &mut f32, label: &str, on: bool, key: &str| -> bool {
            let gw = ui.painter().layout_no_wrap(label.to_owned(), body(12.5), pal.text).size().x;
            let r = Rect::from_min_size(pos2(*x, orow.top() + 6.0), vec2(gw + 16.0, 24.0));
            *x = r.right() + 5.0;
            chip(ui, pal, r, Id::new(("vo", bid, key)), label, 1.0, on)
        };
        for (i, (name, _)) in media::ASPECTS.iter().enumerate() {
            if pill(ui, &mut x, name, b.size as usize == i, name) {
                b.size = i as u8;
                changed = true;
            }
        }
        x += 8.0;
        for (bit, name) in [(1u8, "Autoplay"), (2, "Muted"), (4, "Loop")] {
            if pill(ui, &mut x, name, b.vopts & bit != 0, name) {
                b.vopts ^= bit;
                changed = true;
            }
        }
        x += 8.0;
        let sr = Rect::from_min_size(pos2(x, orow.top() + 6.0), vec2(96.0, 24.0));
        if sr.right() < orow.right() {
            ui.painter().text(pos2(sr.left(), sr.center().y), Align2::LEFT_CENTER, "Start", body(12.5), pal.muted);
            let mut t = if b.start == 0 { String::new() } else { format!("{}:{:02}", b.start / 60, b.start % 60) };
            let r = ui.put(Rect::from_min_size(pos2(sr.left() + 38.0, sr.top()), vec2(58.0, 24.0)), TextEdit::singleline(&mut t).hint_text("0:00").font(body(12.5)).desired_width(54.0));
            if r.changed() {
                let secs = match t.split_once(':') {
                    Some((m, s)) => m.trim().parse::<u32>().unwrap_or(0) * 60 + s.trim().parse::<u32>().unwrap_or(0),
                    None => t.trim().parse::<u32>().unwrap_or(0),
                };
                b.start = secs.min(86_400);
                changed = true;
            }
        }
        ui.add_space(4.0);
    }
    // body
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    if live {
        ui.painter().rect_filled(rect, 8.0, pal.hover);
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, "Loading…", body(14.0), pal.muted);
        ed.embed_rects.push(EmbedRect { id: b.id, url: media::frame_url(&info, b.vopts, b.start), rect, video: info.file, opts: b.vopts, clip: ui.clip_rect() });
    } else {
        let mut drawn = false;
        if let Some(th) = &info.thumb {
            let mut st = icons::paint_cover(ui, rect, th, 0.5);
            if st == ImgState::Failed {
                if let Some(alt) = &info.thumb_alt {
                    st = icons::paint_cover(ui, rect, alt, 0.5);
                }
            }
            drawn = st == ImgState::Ready;
            if drawn {
                ui.painter().rect_filled(rect, 8.0, Color32::from_black_alpha(55));
            }
        }
        if !drawn {
            ui.painter().rect_filled(rect, 8.0, pal.hover);
            ui.painter().rect_stroke(rect, 8.0, Stroke::new(1.0_f32, pal.border), egui::StrokeKind::Inside);
        }
        let glyph = match info.kind {
            EmbedKind::Video => l::VIDEO,
            EmbedKind::Audio => l::MUSIC,
            EmbedKind::Map => l::MAP,
            EmbedKind::Design => l::PANEL_TOP,
            EmbedKind::Web => l::GLOBE,
        };
        let hover = resp.hovered();
        if info.kind == EmbedKind::Video && (info.thumb.is_some() || info.file) {
            play_button(ui, rect.center(), if hover { 34.0 } else { 30.0 });
        } else {
            icons::glyph(ui.painter(), rect.center() + vec2(0.0, -26.0), 38.0, glyph, if hover { pal.accent } else { pal.muted });
            ui.painter().text(rect.center() + vec2(0.0, 12.0), Align2::CENTER_CENTER, &info.host, bold(17.0), pal.text);
            let g = ui.painter().layout(b.url.clone(), body(12.5), pal.muted, (rect.width() - 40.0).max(60.0));
            ui.painter().galley(pos2(rect.center().x - g.size().x / 2.0, rect.center().y + 30.0), g, pal.muted);
        }
        let hint = if web { "Click to load live preview" } else { "Click to open in your browser" };
        let col = if drawn { Color32::WHITE } else { pal.muted };
        ui.painter().text(pos2(rect.center().x, rect.bottom() - 16.0), Align2::CENTER_CENTER, hint, body(12.5), with_alpha(col, if hover { 1.0 } else { 0.8 }));
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            if web {
                b.checked = true;
                changed = true;
            } else {
                platform::open_url(&ctx, &b.url);
            }
        }
    }
    ui.add_space(4.0);
    changed
}

// ───────────────────────────── bookmark ─────────────────────────────

fn elide(ui: &Ui, text: &str, font: egui::FontId, color: Color32, width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple(text.to_owned(), font, color, width.max(20.0));
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    ui.painter().layout_job(job)
}

fn bookmark(ui: &mut Ui, b: &mut Block, pal: &Palette, w: f32) -> bool {
    if b.url.is_empty() {
        let ph = Spec { icon: l::BOOKMARK, title: "Create a web bookmark", subtitle: "Save a link as a clean card with its site name and address.", upload: None, link_hint: "Paste a link…", link_button: "Create", providers: &[] };
        return placeholder(ui, b, &ph, None, pal, w);
    }
    let ctx = ui.ctx().clone();
    let mut changed = false;
    let host = media::host_of(&b.url);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 70.0), Sense::click());
    let h = ctx.animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    ui.painter().rect_filled(rect, 8.0, with_alpha(pal.hover, 0.35 + 0.65 * h));
    ui.painter().rect_stroke(rect, 8.0, Stroke::new(1.0_f32, pal.border), egui::StrokeKind::Inside);
    let fav = Rect::from_center_size(pos2(rect.left() + 34.0, rect.center().y), vec2(30.0, 30.0));
    if icons::paint_image(ui, fav, &media::favicon_url(&host), 6.0) != ImgState::Ready {
        icons::glyph(ui.painter(), fav.center(), 22.0, l::GLOBE, pal.muted);
    }
    let tw = rect.width() - 100.0;
    let title = if b.text.trim().is_empty() { host.clone() } else { b.text.clone() };
    let g1 = elide(ui, &title, bold(15.0), pal.text, tw);
    ui.painter().galley(pos2(rect.left() + 62.0, rect.center().y - 22.0), g1, pal.text);
    let g2 = elide(ui, &b.url, body(12.5), pal.muted, tw);
    ui.painter().galley(pos2(rect.left() + 62.0, rect.center().y + 3.0), g2, pal.muted);
    icons::glyph(ui.painter(), pos2(rect.right() - 22.0, rect.center().y), 16.0, l::EXTERNAL_LINK, lerp_color(pal.muted, pal.accent, h));
    let t = ctx.animate_bool_with_time(Id::new(("bm_h", b.id)), ui.rect_contains_pointer(rect), 0.12);
    if t > 0.01 {
        let r = Rect::from_min_size(pos2(rect.right() - 108.0, rect.top() + 4.0), vec2(72.0, 22.0));
        if chip(ui, pal, r, Id::new(("bm_r", b.id)), "Replace", t, false) {
            b.url.clear();
            changed = true;
        }
    }
    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() && !b.url.is_empty() && !ui.rect_contains_pointer(Rect::from_min_size(pos2(rect.right() - 108.0, rect.top() + 4.0), vec2(72.0, 22.0))) {
        platform::open_url(&ctx, &b.url);
    }
    ui.add_space(4.0);
    changed
}

// ───────────────────────────── file ─────────────────────────────

fn file(ui: &mut Ui, b: &mut Block, page_id: u64, ed: &mut EditorState, pal: &Palette, w: f32) -> bool {
    if b.media == 0 && b.url.is_empty() {
        let ph = Spec { icon: l::PAPERCLIP, title: "Attach a file or PDF", subtitle: "PDFs open in the built-in reader with highlights and notes.", upload: Some((false, FILE_CHIPS)), link_hint: "Paste a link to a file…", link_button: "Add link", providers: &[] };
        return placeholder(ui, b, &ph, Some(PickTarget::BlockFile { page: page_id, block: b.id }), pal, w);
    }
    let ctx = ui.ctx().clone();
    let mut changed = false;
    let local = b.media != 0;
    let name = if !b.text.trim().is_empty() {
        b.text.clone()
    } else {
        b.url.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("File").to_owned()
    };
    let sub = if local { ed.media_sizes.get(&b.media).map(|s| media::fmt_size(*s)).unwrap_or_default() } else { media::host_of(&b.url) };
    let sub = if crate::pdf::looks_like_pdf(&name, &b.url) { format!("PDF · {sub} · opens in the reader") } else { sub };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 58.0), Sense::click());
    let h = ctx.animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    ui.painter().rect_filled(rect, 8.0, with_alpha(pal.hover, 0.35 + 0.65 * h));
    ui.painter().rect_stroke(rect, 8.0, Stroke::new(1.0_f32, pal.border), egui::StrokeKind::Inside);
    let pdf_card = crate::pdf::looks_like_pdf(&name, &b.url);
    icons::glyph(ui.painter(), pos2(rect.left() + 28.0, rect.center().y), 24.0, if pdf_card { l::BOOK_OPEN } else { l::PAPERCLIP }, pal.accent);
    let g1 = elide(ui, &name, bold(14.5), pal.text, rect.width() - 230.0);
    ui.painter().galley(pos2(rect.left() + 54.0, rect.center().y - 19.0), g1, pal.text);
    ui.painter().text(pos2(rect.left() + 54.0, rect.center().y + 9.0), Align2::LEFT_CENTER, sub, body(12.5), pal.muted);
    let is_pdf = crate::pdf::looks_like_pdf(&name, &b.url);
    let items: Vec<(&str, bool)> = if is_pdf { vec![("Read", true), (if local { "Save" } else { "Open" }, false), ("Replace", false)] } else { vec![(if local { "Save" } else { "Open" }, false), ("Replace", false)] };
    let area = Rect::from_min_max(pos2(rect.left(), rect.top() + 11.0), rect.right_bottom());
    let mut clicked_chip = false;
    if let Some(i) = chips(ui, pal, area, Id::new(("file_c", b.id)), &items, 1.0) {
        clicked_chip = true;
        if items[i].0 == "Read" {
            ed.pdf_ask = Some((b.media, b.url.clone(), name.clone()));
        } else if items[i].0 != "Replace" {
            if local {
                ed.dl_req = Some((b.media, name.clone()));
            } else {
                platform::open_url(&ctx, &b.url);
            }
        } else {
            b.url.clear();
            b.media = 0;
            b.text.clear();
            changed = true;
        }
    }
    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() && !clicked_chip {
        if is_pdf {
            ed.pdf_ask = Some((b.media, b.url.clone(), name.clone()));
        } else if local {
            ed.dl_req = Some((b.media, name.clone()));
        } else {
            platform::open_url(&ctx, &b.url);
        }
    }
    ui.add_space(4.0);
    changed
}

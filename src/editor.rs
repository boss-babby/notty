//! The Notion-style block editor: nesting, toggles, colours, inline formatting, slash menu, drag & drop.

use std::collections::HashMap;

use egui::{
    pos2,
    text::{CCursor, CCursorRange, LayoutJob, TextFormat},
    vec2, Align, Align2, Color32, Context, CornerRadius, CursorIcon, FontId, Frame, Id, Key, Margin, Modifiers, Order,
    Rect, RichText, ScrollArea, Sense, Shape, Stroke, TextEdit, Ui,
};

use crate::database::{self, DbState};
use crate::dates;
use crate::icons::{self, Icon};
use crate::model::{Block, BlockKind, Page, PageKind, ViewKind, Workspace, ICONS};
use crate::theme::{bold, body, lerp_color, tag_color, Palette, COVERS, TAG_NAMES};
use crate::widgets::{appear, gradient_rect, menu_item, popup_area, reset_appear, section_label, with_alpha};

pub const GUTTER: f32 = 56.0;
const INDENT: f32 = 28.0;

/// Requests the editor cannot fulfil itself (they need the whole workspace); handled by the app shell.
#[derive(Clone, Copy)]
pub enum Req {
    NewPage(u64),
    NewDb(u64, ViewKind),
    PickLink(u64),
}

#[derive(Default)]
pub struct EditorState {
    pub focus: Option<(u64, usize)>,
    pub slash_sel: usize,
    pub slash_moved: bool,
    pub block_menu: Option<(u64, egui::Pos2)>,
    pub dragging: Option<u64>,
    pub icon_picker: Option<egui::Pos2>,
    pub cover_picker: Option<egui::Pos2>,
    pub fresh: bool,
    pub req: Option<Req>,
    pub nav: Option<u64>,
    pub scroll_to: Option<u64>,
    pub scale: f32,
    pub db: DbState,
}

pub struct LinkInfo {
    pub icon: String,
    pub title: String,
}
type Links = HashMap<u64, LinkInfo>;

#[derive(Clone, Copy, PartialEq)]
enum Slash {
    Block(BlockKind),
    NewPage,
    NewDb(ViewKind),
    Link,
    Today,
}

#[derive(Clone, Copy)]
struct SlashItem {
    act: Slash,
    label: &'static str,
    hint: &'static str,
    glyph: &'static str,
    kw: &'static str,
    sec: &'static str,
}

fn slash_all() -> Vec<SlashItem> {
    let mut v: Vec<SlashItem> = BlockKind::TEXTY
        .iter()
        .map(|k| SlashItem { act: Slash::Block(*k), label: k.label(), hint: k.hint(), glyph: k.glyph(), kw: k.keywords(), sec: "BASIC BLOCKS" })
        .collect();
    v.push(SlashItem { act: Slash::Block(BlockKind::Divider), label: "Divider", hint: "Visually divide blocks.", glyph: "—", kw: "divider line hr separator", sec: "BASIC BLOCKS" });
    let adv = "ADVANCED";
    v.push(SlashItem { act: Slash::Block(BlockKind::Toc), label: "Table of contents", hint: "Jump links to every heading.", glyph: "≡", kw: "table of contents toc outline", sec: adv });
    v.push(SlashItem { act: Slash::NewPage, label: "Page", hint: "Embed a new sub-page inside this page.", glyph: "📄", kw: "page subpage new child", sec: adv });
    v.push(SlashItem { act: Slash::Link, label: "Link to page", hint: "Link to an existing page.", glyph: "↗", kw: "link page mention reference", sec: adv });
    v.push(SlashItem { act: Slash::Today, label: "Today's date", hint: "Insert the current date.", glyph: "▦", kw: "date today now time", sec: adv });
    let dbs = [
        (ViewKind::Table, "Table database", "Spreadsheet-style database."),
        (ViewKind::Board, "Board database", "Kanban board grouped by status."),
        (ViewKind::List, "List database", "A minimal list of pages."),
        (ViewKind::Gallery, "Gallery database", "Cards in a grid."),
        (ViewKind::Calendar, "Calendar database", "Plan items on a calendar."),
    ];
    for (k, l, h) in dbs {
        v.push(SlashItem { act: Slash::NewDb(k), label: l, hint: h, glyph: k.glyph(), kw: "database table board kanban list gallery calendar view", sec: "DATABASES" });
    }
    v
}

fn slash_items(query: &str) -> Vec<SlashItem> {
    let q = query.trim().to_lowercase();
    slash_all().into_iter().filter(|k| q.is_empty() || k.label.to_lowercase().contains(&q) || k.kw.contains(&q)).collect()
}

enum Act {
    Split(usize, usize),
    Merge(usize),
    Remove(usize),
    Convert(usize, BlockKind, bool),
    Strip(usize, BlockKind, usize),
    MoveTo(usize, usize),
    Dup(usize),
    InsertAfter(usize, BlockKind, String),
    FocusPrev(usize),
    FocusNext(usize),
    Indent(usize, i8),
    Special(usize, Slash),
    SetColor(usize, u8, bool),
}

fn slash_act(idx: usize, s: SlashItem) -> Act {
    match s.act {
        Slash::Block(k) => Act::Convert(idx, k, true),
        other => Act::Special(idx, other),
    }
}

fn byte_at(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map(|(i, _)| i).unwrap_or(s.len())
}

fn md_shortcut(text: &str, cursor: usize) -> Option<(BlockKind, usize)> {
    const RULES: [(&str, BlockKind); 11] = [
        ("### ", BlockKind::H3),
        ("## ", BlockKind::H2),
        ("# ", BlockKind::H1),
        ("- ", BlockKind::Bullet),
        ("* ", BlockKind::Bullet),
        ("[] ", BlockKind::Todo),
        ("1. ", BlockKind::Numbered),
        ("> ", BlockKind::Toggle),
        ("\" ", BlockKind::Quote),
        ("| ", BlockKind::Callout),
        ("```", BlockKind::Code),
    ];
    if text == "---" {
        return Some((BlockKind::Divider, 3));
    }
    if text.starts_with("[ ] ") && cursor == 4 {
        return Some((BlockKind::Todo, 4));
    }
    for (prefix, kind) in RULES {
        let n = prefix.chars().count();
        if text.starts_with(prefix) && cursor == n {
            return Some((kind, n));
        }
    }
    None
}

// ───────────────────────────── inline formatting ─────────────────────────────

struct Span {
    text: String,
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    marker: bool,
}

/// Splits `**bold**`, `*italic*`, `~~strike~~` and `` `code` `` into styled spans (markers are kept as spans).
fn parse_spans(s: &str) -> Vec<Span> {
    let ch: Vec<char> = s.chars().collect();
    let n = ch.len();
    let (mut bo, mut it, mut st, mut co) = (false, false, false, false);
    let mut out: Vec<Span> = vec![];
    let mut cur = String::new();
    let mut i = 0;
    let has_later = |from: usize, pat: &[char]| -> bool { (from..n.saturating_sub(pat.len() - 1)).any(|j| ch[j..j + pat.len()] == *pat) };
    macro_rules! flush {
        ($marker:expr) => {
            if !cur.is_empty() {
                out.push(Span { text: std::mem::take(&mut cur), bold: bo, italic: it, strike: st, code: co, marker: $marker });
            }
        };
    }
    while i < n {
        let c = ch[i];
        let two = i + 1 < n && ch[i + 1] == c;
        if co {
            if c == '`' {
                flush!(false);
                cur.push(c);
                flush!(true);
                co = false;
            } else {
                cur.push(c);
            }
            i += 1;
            continue;
        }
        if c == '`' && has_later(i + 1, &['`']) {
            flush!(false);
            cur.push(c);
            co = true;
            flush!(true);
            i += 1;
        } else if c == '*' && two && (bo || has_later(i + 2, &['*', '*'])) {
            flush!(false);
            cur.push_str("**");
            if bo {
                flush!(true);
                bo = false;
            } else {
                bo = true;
                flush!(true);
            }
            i += 2;
        } else if c == '~' && two && (st || has_later(i + 2, &['~', '~'])) {
            flush!(false);
            cur.push_str("~~");
            if st {
                flush!(true);
                st = false;
            } else {
                st = true;
                flush!(true);
            }
            i += 2;
        } else if c == '*' && !two && (it || (i + 1 < n && !ch[i + 1].is_whitespace() && has_later(i + 1, &['*']))) {
            flush!(false);
            cur.push(c);
            if it {
                flush!(true);
                it = false;
            } else {
                it = true;
                flush!(true);
            }
            i += 1;
        } else {
            cur.push(c);
            i += 1;
        }
    }
    flush!(false);
    out
}

fn has_marks(s: &str) -> bool {
    s.contains('*') || s.contains('`') || s.contains("~~")
}

pub fn rich_job(text: &str, font: &FontId, color: Color32, pal: &Palette, strip: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    for sp in parse_spans(text) {
        if strip && sp.marker {
            continue;
        }
        let fmt = TextFormat {
            font_id: if sp.code { FontId::monospace(font.size * 0.88) } else if sp.bold { bold(font.size) } else { font.clone() },
            color: if sp.marker { with_alpha(pal.muted, 0.75) } else if sp.code { tag_color(5) } else { color },
            background: if sp.code && !sp.marker { pal.hover } else { Color32::TRANSPARENT },
            italics: sp.italic,
            strikethrough: if sp.strike && !sp.marker { Stroke::new(1.3_f32, color) } else { Stroke::NONE },
            ..Default::default()
        };
        job.append(&sp.text, 0.0, fmt);
    }
    if job.sections.is_empty() {
        job.append("", 0.0, TextFormat::simple(font.clone(), color));
    }
    job
}

const KEYWORDS: &[&str] = &[
    "fn", "let", "mut", "pub", "use", "mod", "struct", "enum", "impl", "trait", "if", "else", "for", "while", "loop", "return", "match",
    "const", "static", "class", "def", "function", "var", "import", "from", "export", "async", "await", "true", "false", "self",
    "None", "null", "new", "in", "as", "where", "type", "int", "string", "void", "public", "private", "package", "break", "continue",
];

/// Tiny language-agnostic syntax highlighter for code blocks.
fn code_job(text: &str, font: &FontId, pal: &Palette) -> LayoutJob {
    let ch: Vec<char> = text.chars().collect();
    let n = ch.len();
    let mut job = LayoutJob::default();
    let mut push = |s: String, col: Color32| {
        if !s.is_empty() {
            job.append(&s, 0.0, TextFormat::simple(font.clone(), col));
        }
    };
    let mut i = 0;
    while i < n {
        let c = ch[i];
        let rest_starts = |p: &str| p.chars().enumerate().all(|(k, pc)| i + k < n && ch[i + k] == pc);
        if rest_starts("//") || (c == '#' && i + 1 < n && ch[i + 1] == ' ') {
            let mut j = i;
            while j < n && ch[j] != '\n' {
                j += 1;
            }
            push(ch[i..j].iter().collect(), pal.muted);
            i = j;
        } else if rest_starts("/*") {
            let mut j = i + 2;
            while j + 1 < n && !(ch[j] == '*' && ch[j + 1] == '/') {
                j += 1;
            }
            let j = (j + 2).min(n);
            push(ch[i..j].iter().collect(), pal.muted);
            i = j;
        } else if c == '"' || c == '`' {
            let mut j = i + 1;
            while j < n && ch[j] != c && ch[j] != '\n' {
                if ch[j] == '\\' {
                    j += 1;
                }
                j += 1;
            }
            let j = (j + 1).min(n);
            push(ch[i..j].iter().collect(), tag_color(2));
            i = j;
        } else if c.is_ascii_digit() {
            let mut j = i;
            while j < n && (ch[j].is_ascii_alphanumeric() || ch[j] == '.' || ch[j] == '_') {
                j += 1;
            }
            push(ch[i..j].iter().collect(), tag_color(3));
            i = j;
        } else if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < n && (ch[j].is_alphanumeric() || ch[j] == '_') {
                j += 1;
            }
            let w: String = ch[i..j].iter().collect();
            let col = if KEYWORDS.contains(&w.as_str()) {
                tag_color(4)
            } else if c.is_uppercase() {
                tag_color(1)
            } else if j < n && ch[j] == '(' {
                tag_color(6)
            } else {
                pal.text
            };
            push(w, col);
            i = j;
        } else {
            push(c.to_string(), pal.text);
            i += 1;
        }
    }
    if job.sections.is_empty() {
        job.append("", 0.0, TextFormat::simple(font.clone(), pal.text));
    }
    job
}

/// Wraps (or unwraps) the current selection in `mark`.
fn wrap_selection(ctx: &Context, id: Id, text: &mut String, mark: &str) {
    let mut st = TextEdit::load_state(ctx, id).unwrap_or_default();
    let Some(r) = st.cursor.char_range() else { return };
    let (a, b) = (r.primary.index.min(r.secondary.index), r.primary.index.max(r.secondary.index));
    let (ba, bb) = (byte_at(text, a), byte_at(text, b));
    let m = mark.chars().count();
    let (sa, sb);
    if a >= m && text[..ba].ends_with(mark) && text[bb..].starts_with(mark) {
        let inner = text[ba..bb].to_owned();
        let new = format!("{}{}{}", &text[..ba - mark.len()], inner, &text[bb + mark.len()..]);
        *text = new;
        sa = a - m;
        sb = b - m;
    } else {
        let inner = text[ba..bb].to_owned();
        text.replace_range(ba..bb, &format!("{mark}{inner}{mark}"));
        sa = a + m;
        sb = b + m;
    }
    st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(sa), CCursor::new(sb))));
    st.store(ctx, id);
}

fn hidden(bl: &[Block], j: usize) -> bool {
    let mut lvl = bl[j].indent;
    for k in (0..j).rev() {
        if bl[k].indent < lvl {
            if bl[k].kind == BlockKind::Toggle && bl[k].collapsed {
                return true;
            }
            lvl = bl[k].indent;
            if lvl == 0 {
                break;
            }
        }
    }
    false
}

/// Index one past the last child of block `i`.
fn subtree_end(bl: &[Block], i: usize) -> usize {
    let mut j = i + 1;
    while j < bl.len() && bl[j].indent > bl[i].indent {
        j += 1;
    }
    j
}

// ───────────────────────────── page body ─────────────────────────────

/// Renders a page (cover, header, body). Returns `true` if anything changed.
pub fn show_page(ui: &mut Ui, ws: &mut Workspace, id: u64, ed: &mut EditorState, pal: &Palette) -> bool {
    let Some(idx) = ws.pages.iter().position(|p| p.id == id) else { return false };
    let mut changed = false;
    let avail = ui.available_width();
    let (kind, is_row, full, small) = {
        let p = &ws.pages[idx];
        (p.kind, p.row, p.full_width, p.small_text)
    };
    ed.scale = if small { 0.9 } else { 1.0 };
    changed |= cover(ui, &mut ws.pages[idx], ed, pal, avail);

    let wide = kind == PageKind::Database || full;
    let col_w = if wide { (avail - 96.0).max(300.0) } else { (avail - 80.0).clamp(300.0, 900.0) };
    let left = if wide { 48.0 } else { ((avail - col_w) / 2.0).max(24.0) };
    let content_w = (col_w - GUTTER).max(200.0);

    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(left);
        ui.vertical(|ui| {
            ui.set_width(col_w);
            ui.spacing_mut().item_spacing.y = 0.0;
            changed |= header(ui, &mut ws.pages[idx], ed, pal, content_w);
            if is_row {
                changed |= database::row_props(ui, ws, id, ed, pal, content_w);
            }
            match kind {
                PageKind::Database => changed |= database::show(ui, ws, id, ed, pal, col_w),
                _ => {
                    let mut links: Links = HashMap::new();
                    for b in ws.pages[idx].blocks.iter().filter(|b| b.kind == BlockKind::PageLink) {
                        if let Some(t) = b.link.and_then(|l| ws.page(l)).filter(|t| !t.trashed) {
                            links.insert(t.id, LinkInfo { icon: t.icon.clone(), title: if t.title.is_empty() { "Untitled".into() } else { t.title.clone() } });
                        }
                    }
                    {
                        let Workspace { pages, next_id, .. } = &mut *ws;
                        changed |= blocks(ui, &mut pages[idx], next_id, ed, pal, content_w, &links);
                    }
                    backlinks(ui, ws, id, ed, pal, content_w);
                }
            }
        });
    });

    ui.add_space(80.0);
    {
        let Workspace { pages, next_id, .. } = &mut *ws;
        changed |= popups(ui, &mut pages[idx], next_id, ed, pal);
    }
    changed
}

fn backlinks(ui: &mut Ui, ws: &Workspace, id: u64, ed: &mut EditorState, pal: &Palette, w: f32) {
    let links = ws.backlinks(id);
    if links.is_empty() {
        return;
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(GUTTER);
        ui.label(RichText::new(format!("LINKED FROM  ·  {}", links.len())).font(bold(11.5)).color(pal.muted));
    });
    for l in links {
        let Some(p) = ws.page(l) else { continue };
        let (rect, resp) = ui.allocate_exact_size(vec2(w + GUTTER, 28.0), Sense::click());
        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
        let r = Rect::from_min_max(pos2(rect.left() + GUTTER - 6.0, rect.top()), rect.right_bottom());
        ui.painter().rect_filled(r, 6.0, with_alpha(pal.hover, t));
        let icon = if p.icon.is_empty() { "📄" } else { &p.icon };
        ui.painter().text(pos2(r.left() + 18.0, r.center().y), Align2::CENTER_CENTER, icon, body(15.0), pal.text);
        let title = if p.title.is_empty() { "Untitled" } else { &p.title };
        ui.painter().text(pos2(r.left() + 38.0, r.center().y), Align2::LEFT_CENTER, title, body(14.5), pal.text);
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            ed.nav = Some(l);
        }
    }
}

fn cover(ui: &mut Ui, page: &mut Page, ed: &mut EditorState, pal: &Palette, avail: f32) -> bool {
    let mut changed = false;
    let Some(ci) = page.cover else { return false };
    let (rect, _) = ui.allocate_exact_size(vec2(avail, 200.0), Sense::hover());
    let (a, b) = COVERS[ci % COVERS.len()];
    gradient_rect(ui, rect, Color32::from_rgb(a.0, a.1, a.2), Color32::from_rgb(b.0, b.1, b.2));
    let hovered = ui.rect_contains_pointer(rect);
    let t = ui.ctx().animate_bool_with_time(Id::new(("cover_h", page.id)), hovered, 0.15);
    if t > 0.01 {
        let r = Rect::from_min_size(pos2(rect.right() - 262.0, rect.bottom() - 42.0), vec2(120.0, 28.0));
        let r2 = Rect::from_min_size(pos2(rect.right() - 134.0, rect.bottom() - 42.0), vec2(110.0, 28.0));
        for (rr, label, id) in [(r, "Change cover", "cc"), (r2, "Remove", "cr")] {
            let resp = ui.interact(rr, Id::new((id, page.id)), Sense::click());
            let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
            ui.painter().rect_filled(rr, 6.0, with_alpha(lerp_color(pal.bg, pal.hover, h), t * 0.92));
            ui.painter().text(rr.center(), Align2::CENTER_CENTER, label, body(12.5), with_alpha(pal.text, t));
            if resp.clicked() {
                if id == "cc" {
                    ed.cover_picker = Some(rr.left_bottom() + vec2(0.0, 6.0));
                    ed.fresh = true;
                } else {
                    page.cover = None;
                    changed = true;
                }
            }
        }
    }
    changed
}

fn header(ui: &mut Ui, page: &mut Page, ed: &mut EditorState, pal: &Palette, content_w: f32) -> bool {
    let mut changed = false;
    let has_cover = page.cover.is_some();
    ui.add_space(if has_cover { 0.0 } else { 40.0 });

    let icon_h = 76.0;
    let (rect, _) = ui.allocate_exact_size(vec2(content_w + GUTTER, if has_cover { 44.0 } else { icon_h }), Sense::hover());
    let hdr_top = rect.top();
    if !page.icon.is_empty() {
        let size = 68.0;
        let top = if has_cover { hdr_top - 34.0 } else { hdr_top };
        let ir = Rect::from_min_size(pos2(rect.left() + GUTTER - 8.0, top), vec2(size + 16.0, size + 8.0));
        let resp = ui.interact(ir, Id::new(("icon", page.id)), Sense::click());
        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
        ui.painter().rect_filled(ir, 12.0, with_alpha(pal.hover, t));
        ui.painter().text(pos2(ir.center().x, ir.center().y + 2.0), Align2::CENTER_CENTER, &page.icon, body(54.0), pal.text);
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            ed.icon_picker = Some(ir.left_bottom() + vec2(0.0, 6.0));
            ed.fresh = true;
        }
    }

    let row_h = 30.0;
    let (row, _) = ui.allocate_exact_size(vec2(content_w + GUTTER, row_h), Sense::hover());
    let hov = ui.rect_contains_pointer(Rect::from_min_max(pos2(row.left(), hdr_top - 10.0), pos2(row.right(), row.bottom() + 56.0)));
    let t = ui.ctx().animate_bool_with_time(Id::new(("hdr_h", page.id)), hov, 0.15);
    let mut x = row.left() + GUTTER;
    let mut buttons: Vec<(&str, &str)> = vec![];
    if page.icon.is_empty() {
        buttons.push(("☺  Add icon", "ai"));
    }
    if page.cover.is_none() {
        buttons.push(("▭  Add cover", "ac"));
    }
    for (label, key) in buttons {
        let w = 100.0 + if key == "ac" { 6.0 } else { 0.0 };
        let r = Rect::from_min_size(pos2(x, row.top() + 2.0), vec2(w, 26.0));
        x += w + 6.0;
        if t < 0.02 {
            continue;
        }
        let resp = ui.interact(r, Id::new((key, page.id)), Sense::click());
        let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
        ui.painter().rect_filled(r, 6.0, with_alpha(pal.hover, h * t));
        ui.painter().text(r.center(), Align2::CENTER_CENTER, label, body(13.0), with_alpha(lerp_color(pal.muted, pal.text, h), t));
        if resp.clicked() {
            if key == "ai" {
                page.icon = "📄".into();
            } else {
                page.cover = Some((page.id as usize) % COVERS.len());
            }
            changed = true;
        }
    }

    ui.horizontal(|ui| {
        ui.add_space(GUTTER);
        let tid = Id::new(("title", page.id));
        let title_focused = ui.memory(|m| m.has_focus(tid));
        let enter = title_focused && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        let r = ui.add(
            TextEdit::singleline(&mut page.title)
                .id(tid)
                .frame(false)
                .font(bold(40.0))
                .hint_text(RichText::new("Untitled").font(bold(40.0)).color(with_alpha(pal.muted, 0.55)))
                .desired_width(content_w)
                .margin(Margin::symmetric(0, 4)),
        );
        changed |= r.changed();
        if enter && page.kind == PageKind::Doc {
            if let Some(b) = page.blocks.iter().find(|b| b.kind.has_text()) {
                ed.focus = Some((b.id, 0));
            }
        }
    });
    ui.add_space(8.0);
    changed
}

fn font_for(kind: BlockKind, scale: f32) -> FontId {
    let s = if scale <= 0.0 { 1.0 } else { scale };
    match kind {
        BlockKind::H1 => bold(30.0 * s),
        BlockKind::H2 => bold(24.0 * s),
        BlockKind::H3 => bold(19.0 * s),
        BlockKind::Code => FontId::monospace(13.5 * s),
        BlockKind::Quote => body(17.0 * s),
        _ => body(16.0 * s),
    }
}

fn blocks(ui: &mut Ui, page: &mut Page, next_id: &mut u64, ed: &mut EditorState, pal: &Palette, w: f32, links: &Links) -> bool {
    let mut changed = false;
    let n = page.blocks.len();
    let mut act: Option<Act> = None;
    let mut rects: Vec<(usize, Rect)> = Vec::with_capacity(n);
    let mut nums = [0usize; 8];
    let pointer = ui.ctx().pointer_hover_pos();
    let heads: Vec<(u64, u8, String)> = page
        .blocks
        .iter()
        .filter_map(|b| match b.kind {
            BlockKind::H1 => Some((b.id, 1, b.text.clone())),
            BlockKind::H2 => Some((b.id, 2, b.text.clone())),
            BlockKind::H3 => Some((b.id, 3, b.text.clone())),
            _ => None,
        })
        .collect();
    let mut visible = vec![true; n];
    let mut hide: Option<u8> = None;
    for i in 0..n {
        let b = &page.blocks[i];
        if let Some(h) = hide {
            if b.indent > h {
                visible[i] = false;
                continue;
            }
            hide = None;
        }
        if b.kind == BlockKind::Toggle && b.collapsed {
            hide = Some(b.indent);
        }
    }

    for i in 0..n {
        let kind = page.blocks[i].kind;
        let bid = page.blocks[i].id;
        let indent = page.blocks[i].indent.min(6);
        let ind = indent as usize;
        for d in nums.iter_mut().skip(ind + 1) {
            *d = 0;
        }
        if kind == BlockKind::Numbered {
            nums[ind] += 1;
        } else {
            nums[ind] = 0;
        }
        let num = nums[ind];
        if !visible[i] {
            continue;
        }
        if !rects.is_empty() {
            ui.add_space(match kind {
                BlockKind::H1 => 18.0,
                BlockKind::H2 => 12.0,
                BlockKind::H3 => 8.0,
                BlockKind::Callout | BlockKind::Code | BlockKind::Quote => 6.0,
                _ => 1.0,
            });
        }
        let ind_px = indent as f32 * INDENT;
        let bg_slot = ui.painter().add(Shape::Noop);
        let row = ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            ui.add_space(GUTTER + ind_px);
            ui.vertical(|ui| {
                ui.set_width(w - ind_px);
                changed |= draw_block(ui, &mut page.blocks[i], i, n, num, ed, pal, w - ind_px, &mut act, &heads, links);
            });
        });
        let rect = row.response.rect;
        rects.push((i, rect));
        let bgc = page.blocks[i].bg;
        if bgc > 0 && !matches!(kind, BlockKind::Divider | BlockKind::Callout | BlockKind::Code) {
            let r = Rect::from_min_max(pos2(rect.left() + GUTTER + ind_px - 6.0, rect.top()), pos2(rect.right() + 4.0, rect.bottom()));
            ui.painter().set(bg_slot, Shape::rect_filled(r, 4.0, tag_color(bgc as usize - 1).gamma_multiply(0.16)));
        }
        if ed.scroll_to == Some(bid) {
            ui.scroll_to_rect(rect, Some(Align::TOP));
            ed.scroll_to = None;
        }

        // gutter controls
        let hover_zone = Rect::from_min_max(pos2(rect.left(), rect.top() - 1.0), pos2(rect.right(), rect.bottom() + 1.0));
        let hovering = pointer.map_or(false, |p| hover_zone.contains(p)) && ed.dragging.is_none()
            || ed.dragging == Some(bid)
            || ed.block_menu.map_or(false, |(b, _)| b == bid);
        let t = ui.ctx().animate_bool_with_time(Id::new(("gutter", bid)), hovering, 0.12);
        if t > 0.02 {
            let y = rect.top()
                + match kind {
                    BlockKind::H1 => 8.0,
                    BlockKind::H2 => 5.0,
                    BlockKind::H3 => 3.0,
                    BlockKind::Callout => 11.0,
                    BlockKind::Code => 14.0,
                    _ => 1.0,
                };
            let plus = Rect::from_min_size(pos2(rect.left() + 6.0, y), vec2(22.0, 24.0));
            let grip = Rect::from_min_size(pos2(rect.left() + 29.0, y), vec2(20.0, 24.0));
            let pr = ui.interact(plus, Id::new(("plus", bid)), Sense::click());
            let gr = ui.interact(grip, Id::new(("grip", bid)), Sense::click_and_drag());
            for (r, resp, icon) in [(plus, &pr, Icon::Plus), (grip, &gr, Icon::Grip)] {
                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                ui.painter().rect_filled(r, 5.0, with_alpha(pal.hover, h * t));
                icons::paint(ui.painter(), r.center(), 15.0, icon, with_alpha(lerp_color(pal.muted, pal.text, h), t));
            }
            if pr.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Click to add a block below").clicked() {
                act = Some(Act::InsertAfter(i, BlockKind::Text, "/".into()));
            }
            if gr.drag_started() {
                ed.dragging = Some(bid);
            }
            if gr.clicked() {
                ed.block_menu = Some((bid, grip.left_bottom() + vec2(-4.0, 6.0)));
                ed.fresh = true;
            }
            if gr.hovered() || gr.dragged() {
                ui.ctx().set_cursor_icon(if gr.dragged() { CursorIcon::Grabbing } else { CursorIcon::Grab });
            }
        }
    }

    // drag & drop reorder (children travel with their parent)
    if let Some(did) = ed.dragging {
        ui.ctx().request_repaint();
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        if let (Some(p), Some(from), false) = (pointer, page.blocks.iter().position(|b| b.id == did), rects.is_empty()) {
            let slot = rects.iter().position(|(_, r)| p.y < r.center().y);
            let (target, y) = match slot {
                Some(s) => (rects[s].0, rects[s].1.top() - 1.0),
                None => (n, rects[rects.len() - 1].1.bottom() + 1.0),
            };
            let x0 = rects[0].1.left() + GUTTER;
            ui.painter().rect_filled(Rect::from_min_size(pos2(x0, y - 1.5), vec2(w, 3.0)), 2.0, pal.accent);
            if !ui.input(|i| i.pointer.primary_down()) {
                act = Some(Act::MoveTo(from, target));
                ed.dragging = None;
            }
        } else if !ui.input(|i| i.pointer.primary_down()) {
            ed.dragging = None;
        }
    }

    // click below the last block to append
    let (rest, resp) = ui.allocate_exact_size(vec2(w + GUTTER, 150.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(Id::new(("tail", page.id)), resp.hovered(), 0.15);
    ui.painter().text(pos2(rest.left() + GUTTER, rest.top() + 16.0), Align2::LEFT_CENTER, "Click to add a block", body(14.0), with_alpha(pal.muted, t * 0.8));
    if resp.clicked() && n > 0 {
        let last = &page.blocks[n - 1];
        if last.kind == BlockKind::Text && last.text.is_empty() {
            ed.focus = Some((last.id, 0));
        } else {
            act = Some(Act::InsertAfter(n - 1, BlockKind::Text, String::new()));
        }
    }

    if let Some(a) = act {
        apply(page, a, next_id, ed);
        changed = true;
        ui.ctx().request_repaint();
    }
    if let Some((fid, _)) = ed.focus {
        if !page.blocks.iter().any(|b| b.id == fid) {
            ed.focus = None;
        }
    }
    changed
}

fn new_block(next_id: &mut u64, kind: BlockKind, text: String, indent: u8) -> Block {
    *next_id += 1;
    Block { id: *next_id, kind, text, checked: false, indent, collapsed: false, color: 0, bg: 0, link: None }
}

fn apply(page: &mut Page, act: Act, next_id: &mut u64, ed: &mut EditorState) {
    let bl = &mut page.blocks;
    match act {
        Act::Split(i, at) => {
            let kind = bl[i].kind;
            let indent = bl[i].indent;
            let cut = byte_at(&bl[i].text, at);
            let right = bl[i].text.split_off(cut);
            let (nk, ni) = match kind {
                BlockKind::Todo | BlockKind::Bullet | BlockKind::Numbered => (kind, indent),
                BlockKind::Toggle if !bl[i].collapsed => (BlockKind::Text, (indent + 1).min(6)),
                _ => (BlockKind::Text, indent),
            };
            let mut nb = new_block(next_id, nk, right, ni);
            nb.color = bl[i].color;
            ed.focus = Some((nb.id, 0));
            // keep children attached to the new block when the split block is a list item
            bl.insert(i + 1, nb);
        }
        Act::Merge(i) => {
            if i == 0 {
                return;
            }
            if !bl[i - 1].kind.has_text() {
                bl.remove(i - 1);
                ed.focus = Some((bl[i - 1].id, 0));
            } else {
                let b = bl.remove(i);
                let prev = &mut bl[i - 1];
                let pos = prev.text.chars().count();
                prev.text.push_str(&b.text);
                ed.focus = Some((prev.id, pos));
            }
        }
        Act::Remove(i) => {
            bl.remove(i);
            if bl.is_empty() {
                bl.push(new_block(next_id, BlockKind::Text, String::new(), 0));
            }
            let j = i.saturating_sub(1).min(bl.len() - 1);
            ed.focus = Some((bl[j].id, usize::MAX));
        }
        Act::Convert(i, kind, clear) => {
            if clear {
                bl[i].text.clear();
            }
            bl[i].kind = kind;
            if !kind.has_text() {
                bl[i].text.clear();
                if kind != BlockKind::PageLink {
                    let ind = bl[i].indent;
                    let nb = new_block(next_id, BlockKind::Text, String::new(), ind);
                    ed.focus = Some((nb.id, 0));
                    bl.insert(i + 1, nb);
                }
            } else {
                ed.focus = Some((bl[i].id, usize::MAX));
            }
        }
        Act::Strip(i, kind, n) => {
            let cut = byte_at(&bl[i].text, n);
            bl[i].text = bl[i].text[cut..].to_owned();
            bl[i].kind = kind;
            if kind == BlockKind::Divider {
                bl[i].text.clear();
                let ind = bl[i].indent;
                let nb = new_block(next_id, BlockKind::Text, String::new(), ind);
                ed.focus = Some((nb.id, 0));
                bl.insert(i + 1, nb);
            } else {
                ed.focus = Some((bl[i].id, 0));
            }
        }
        Act::MoveTo(from, to) => {
            let end = subtree_end(bl, from);
            if to >= from && to <= end {
                return;
            }
            let moved: Vec<Block> = bl.drain(from..end).collect();
            let len = moved.len();
            let to = if to > from { to - len } else { to };
            let to = to.min(bl.len());
            for (k, b) in moved.into_iter().enumerate() {
                bl.insert(to + k, b);
            }
        }
        Act::Dup(i) => {
            let end = subtree_end(bl, i);
            let mut copies: Vec<Block> = bl[i..end].to_vec();
            for c in copies.iter_mut() {
                *next_id += 1;
                c.id = *next_id;
            }
            ed.focus = Some((copies[0].id, usize::MAX));
            for (k, c) in copies.into_iter().enumerate() {
                bl.insert(end + k, c);
            }
        }
        Act::InsertAfter(i, kind, text) => {
            let ind = bl[i].indent;
            let nb = new_block(next_id, kind, text, ind);
            ed.focus = Some((nb.id, usize::MAX));
            bl.insert(i + 1, nb);
        }
        Act::FocusPrev(i) => {
            let mut j = i;
            while j > 0 {
                j -= 1;
                if bl[j].kind.has_text() && !hidden(bl, j) {
                    ed.focus = Some((bl[j].id, usize::MAX));
                    break;
                }
            }
        }
        Act::FocusNext(i) => {
            let mut j = i + 1;
            while j < bl.len() {
                if bl[j].kind.has_text() && !hidden(bl, j) {
                    ed.focus = Some((bl[j].id, 0));
                    break;
                }
                j += 1;
            }
        }
        Act::Indent(i, d) => {
            let cur = bl[i].indent as i8;
            let max_allowed = if i == 0 { 0 } else { (bl[i - 1].indent as i8 + 1).min(6) };
            let new = if d > 0 { (cur + 1).min(max_allowed) } else { (cur - 1).max(0) };
            if new == cur {
                return;
            }
            let delta = new - cur;
            let end = subtree_end(bl, i);
            for b in bl[i..end].iter_mut() {
                b.indent = (b.indent as i8 + delta).clamp(0, 6) as u8;
            }
            ed.focus = Some((bl[i].id, usize::MAX));
        }
        Act::SetColor(i, c, is_bg) => {
            if is_bg {
                bl[i].bg = c;
            } else {
                bl[i].color = c;
            }
        }
        Act::Special(i, s) => match s {
            Slash::NewPage | Slash::Link | Slash::NewDb(_) => {
                let bid = bl[i].id;
                bl[i].kind = BlockKind::PageLink;
                bl[i].text.clear();
                bl[i].link = None;
                ed.req = Some(match s {
                    Slash::NewPage => Req::NewPage(bid),
                    Slash::NewDb(v) => Req::NewDb(bid, v),
                    _ => Req::PickLink(bid),
                });
                let ind = bl[i].indent;
                if i + 1 >= bl.len() {
                    let nb = new_block(next_id, BlockKind::Text, String::new(), ind);
                    bl.push(nb);
                }
            }
            Slash::Today => {
                bl[i].kind = BlockKind::Text;
                bl[i].text = dates::long_today();
                ed.focus = Some((bl[i].id, usize::MAX));
            }
            Slash::Block(k) => apply(page, Act::Convert(i, k, true), next_id, ed),
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_block(
    ui: &mut Ui,
    b: &mut Block,
    idx: usize,
    n: usize,
    num: usize,
    ed: &mut EditorState,
    pal: &Palette,
    w: f32,
    act: &mut Option<Act>,
    heads: &[(u64, u8, String)],
    links: &Links,
) -> bool {
    let mut changed = false;
    let font = font_for(b.kind, ed.scale);
    let base = if b.color > 0 { tag_color(b.color as usize - 1) } else { pal.text };
    match b.kind {
        BlockKind::Text | BlockKind::H1 | BlockKind::H2 | BlockKind::H3 => {
            let hint = match b.kind {
                BlockKind::H1 => "Heading 1",
                BlockKind::H2 => "Heading 2",
                BlockKind::H3 => "Heading 3",
                _ => "Type '/' for commands…",
            };
            changed |= text_edit(ui, b, idx, n, ed, pal, Tx { font, color: base, hint, width: w }, act);
        }
        BlockKind::Todo => {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let (r, resp) = ui.allocate_exact_size(vec2(28.0, 26.0), Sense::click());
                let c = pos2(r.left() + 10.0, r.top() + 13.5);
                let t = ui.ctx().animate_bool_with_time(Id::new(("chk", b.id)), b.checked, 0.18);
                let box_r = Rect::from_center_size(c, vec2(17.0, 17.0));
                ui.painter().rect_filled(box_r, 4.0, with_alpha(pal.accent, t));
                ui.painter().rect_stroke(box_r, 4.0, Stroke::new(1.6_f32, lerp_color(pal.text.gamma_multiply(0.8), pal.accent, t)), egui::StrokeKind::Inside);
                if t > 0.02 {
                    let s = t.min(1.0);
                    let p1 = pos2(c.x - 4.2, c.y + 0.2);
                    let p2 = pos2(c.x - 1.2, c.y + 3.2);
                    let p3 = pos2(c.x + 4.4, c.y - 3.2);
                    let mid = p1 + (p2 - p1) * (s * 2.0).min(1.0);
                    let mut pts = vec![p1, mid];
                    if s > 0.5 {
                        pts.push(p2 + (p3 - p2) * ((s - 0.5) * 2.0).min(1.0));
                    }
                    ui.painter().add(Shape::line(pts, Stroke::new(2.0_f32, Color32::WHITE)));
                }
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    b.checked = !b.checked;
                    changed = true;
                }
                let col = lerp_color(base, pal.muted, t);
                changed |= text_edit(ui, b, idx, n, ed, pal, Tx { font, color: col, hint: "To-do", width: w - 28.0 }, act);
            });
        }
        BlockKind::Bullet | BlockKind::Numbered => {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
                if b.kind == BlockKind::Bullet {
                    let c = pos2(r.left() + 10.0, r.top() + 13.5);
                    match b.indent % 3 {
                        0 => ui.painter().circle_filled(c, 3.0, base),
                        1 => ui.painter().circle_stroke(c, 3.0, Stroke::new(1.3_f32, base)),
                        _ => ui.painter().rect_filled(Rect::from_center_size(c, vec2(5.5, 5.5)), 1.0, base),
                    };
                } else {
                    let label = match b.indent % 3 {
                        0 => format!("{num}."),
                        1 => format!("{}.", (b'a' + ((num.max(1) - 1) % 26) as u8) as char),
                        _ => format!("{}.", roman(num)),
                    };
                    ui.painter().text(pos2(r.left() + 17.0, r.top() + 13.5), Align2::RIGHT_CENTER, label, body(16.0 * ed.scale.max(0.5)), base);
                }
                let hint = if b.kind == BlockKind::Bullet { "List" } else { "List item" };
                changed |= text_edit(ui, b, idx, n, ed, pal, Tx { font, color: base, hint, width: w - 26.0 }, act);
            });
        }
        BlockKind::Toggle => {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
                let t = ui.ctx().animate_bool_with_time(Id::new(("tgl", b.id)), !b.collapsed, 0.14);
                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                let rr = Rect::from_center_size(r.center() + vec2(-2.0, 0.0), vec2(22.0, 22.0));
                ui.painter().rect_filled(rr, 5.0, with_alpha(pal.hover, h));
                icons::chevron(ui.painter(), rr.center(), 13.0, t * std::f32::consts::FRAC_PI_2, lerp_color(pal.muted, pal.text, h));
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    b.collapsed = !b.collapsed;
                    changed = true;
                }
                changed |= text_edit(ui, b, idx, n, ed, pal, Tx { font, color: base, hint: "Toggle", width: w - 26.0 }, act);
            });
        }
        BlockKind::Quote => {
            let r = ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(14.0);
                changed |= text_edit(ui, b, idx, n, ed, pal, Tx { font, color: base, hint: "Empty quote", width: w - 14.0 }, act);
            });
            let rr = r.response.rect;
            ui.painter().rect_filled(Rect::from_min_size(rr.min + vec2(0.0, 3.0), vec2(3.0, rr.height() - 6.0)), 2.0, base);
        }
        BlockKind::Callout => {
            Frame::new().fill(pal.callout).corner_radius(CornerRadius::same(6)).inner_margin(Margin::symmetric(14, 12)).show(ui, |ui| {
                ui.set_width(w - 28.0);
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let (r, _) = ui.allocate_exact_size(vec2(34.0, 26.0), Sense::hover());
                    ui.painter().text(r.left_center() + vec2(2.0, 0.0), Align2::LEFT_CENTER, "💡", body(20.0), pal.text);
                    changed |= text_edit(ui, b, idx, n, ed, pal, Tx { font, color: base, hint: "Type something…", width: w - 28.0 - 34.0 }, act);
                });
            });
        }
        BlockKind::Code => {
            let fr = Frame::new()
                .fill(pal.code_bg)
                .stroke(Stroke::new(1.0_f32, pal.border))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::symmetric(16, 14))
                .show(ui, |ui| {
                    ui.set_width(w - 32.0);
                    changed |= text_edit(ui, b, idx, n, ed, pal, Tx { font, color: pal.text, hint: "Write some code…", width: w - 32.0 }, act);
                });
            let rr = fr.response.rect;
            if ui.rect_contains_pointer(rr) {
                let br = Rect::from_min_size(pos2(rr.right() - 58.0, rr.top() + 6.0), vec2(50.0, 22.0));
                let resp = ui.interact(br, Id::new(("copy", b.id)), Sense::click());
                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                ui.painter().rect_filled(br, 5.0, with_alpha(pal.active, 0.5 + 0.5 * h));
                ui.painter().text(br.center(), Align2::CENTER_CENTER, "Copy", body(12.0), pal.text);
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    ui.ctx().copy_text(b.text.clone());
                }
            }
        }
        BlockKind::Divider => {
            let (r, _) = ui.allocate_exact_size(vec2(w, 24.0), Sense::hover());
            ui.painter().line_segment([pos2(r.left(), r.center().y), pos2(r.right(), r.center().y)], Stroke::new(1.0_f32, pal.border.gamma_multiply(1.4)));
        }
        BlockKind::Toc => {
            ui.add_space(4.0);
            if heads.is_empty() {
                ui.label(RichText::new("Add headings to your page and they will appear here.").color(pal.muted).font(body(14.0)));
            }
            for (hid, lvl, text) in heads {
                let (r, resp) = ui.allocate_exact_size(vec2(w, 24.0), Sense::click());
                let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                let x = r.left() + (*lvl as f32 - 1.0) * 18.0;
                let label = if text.trim().is_empty() { "Untitled" } else { text.trim() };
                let col = lerp_color(pal.muted, pal.text, t);
                let gal = ui.painter().layout_no_wrap(label.to_owned(), body(14.5), col);
                let wd = gal.size().x;
                ui.painter().galley(pos2(x, r.center().y - gal.size().y / 2.0), gal, col);
                if t > 0.01 {
                    ui.painter().line_segment([pos2(x, r.center().y + 9.0), pos2(x + wd, r.center().y + 9.0)], Stroke::new(1.0_f32, with_alpha(col, t)));
                }
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    ed.scroll_to = Some(*hid);
                }
            }
            ui.add_space(4.0);
        }
        BlockKind::PageLink => {
            let info = b.link.and_then(|l| links.get(&l));
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 32.0), Sense::click());
            let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
            ui.painter().rect_filled(rect, 6.0, with_alpha(pal.hover, t));
            match (info, b.link) {
                (Some(i), _) => {
                    let icon = if i.icon.is_empty() { "📄" } else { &i.icon };
                    ui.painter().text(pos2(rect.left() + 16.0, rect.center().y), Align2::CENTER_CENTER, icon, body(17.0), pal.text);
                    let col = pal.text;
                    let g = ui.painter().layout_no_wrap(i.title.clone(), body(16.0 * ed.scale.max(0.5)), col);
                    let (tx, ty) = (rect.left() + 34.0, rect.center().y - g.size().y / 2.0);
                    let gw = g.size().x;
                    ui.painter().galley(pos2(tx, ty), g, col);
                    ui.painter().line_segment([pos2(tx, rect.center().y + 10.0), pos2(tx + gw, rect.center().y + 10.0)], Stroke::new(1.0_f32, with_alpha(col, 0.35)));
                }
                (None, None) => {
                    ui.painter().text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, "↗  Choose a page to link…", body(15.0), pal.muted);
                }
                (None, Some(_)) => {
                    ui.painter().text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, "⚠  This page was deleted", body(15.0), pal.danger);
                }
            }
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                match (info, b.link) {
                    (Some(_), Some(l)) => ed.nav = Some(l),
                    (None, None) => ed.req = Some(Req::PickLink(b.id)),
                    _ => {}
                }
            }
        }
    }
    changed
}

fn roman(mut n: usize) -> String {
    let t = [(10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i")];
    let mut s = String::new();
    for (v, r) in t {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

struct Tx<'a> {
    font: FontId,
    color: Color32,
    hint: &'a str,
    width: f32,
}

fn text_edit(ui: &mut Ui, b: &mut Block, idx: usize, n: usize, ed: &mut EditorState, pal: &Palette, tx: Tx, act: &mut Option<Act>) -> bool {
    let ctx = ui.ctx().clone();
    let id = Id::new(("blk", b.id));
    let nchars = b.text.chars().count();
    let Tx { font, color, hint, width } = tx;
    let focus_request = ed.focus.map_or(false, |(f, _)| f == b.id);

    // Unfocused blocks with inline formatting render the styled text without the markers.
    if !focus_request && b.kind != BlockKind::Code && has_marks(&b.text) && !ctx.memory(|m| m.has_focus(id)) {
        let mut job = rich_job(&b.text, &font, color, pal, true);
        job.wrap.max_width = width;
        let galley = ui.painter().layout_job(job);
        ui.add_space(3.0);
        let (rect, resp) = ui.allocate_exact_size(vec2(width, galley.size().y.max(font.size + 4.0)), Sense::click());
        ui.painter().galley(rect.min, galley, color);
        ui.add_space(3.0);
        let resp = resp.on_hover_cursor(CursorIcon::Text);
        if resp.clicked() {
            ed.focus = Some((b.id, usize::MAX));
        }
        return false;
    }

    if let Some((fid, pos)) = ed.focus {
        if fid == b.id {
            ctx.memory_mut(|m| m.request_focus(id));
            let mut st = TextEdit::load_state(&ctx, id).unwrap_or_default();
            st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(pos.min(nchars)))));
            st.store(&ctx, id);
            ed.focus = None;
            ed.slash_sel = 0;
        }
    }

    let focused = ctx.memory(|m| m.has_focus(id));
    let items = if focused && b.kind == BlockKind::Text && b.text.starts_with('/') && !b.text.contains('\n') { slash_items(&b.text[1..]) } else { vec![] };
    let slash_active = !items.is_empty();
    if slash_active {
        ed.slash_sel = ed.slash_sel.min(items.len() - 1);
    }

    if focused {
        // inline formatting shortcuts
        if b.kind != BlockKind::Code {
            let marks: [(Modifiers, Key, &str); 4] = [
                (Modifiers::COMMAND | Modifiers::SHIFT, Key::S, "~~"),
                (Modifiers::COMMAND, Key::B, "**"),
                (Modifiers::COMMAND, Key::I, "*"),
                (Modifiers::COMMAND, Key::E, "`"),
            ];
            for (m, k, mark) in marks {
                if ui.input_mut(|i| i.consume_key(m, k)) {
                    wrap_selection(&ctx, id, &mut b.text, mark);
                    ed.slash_sel = 0;
                }
            }
        }
        let range = TextEdit::load_state(&ctx, id).and_then(|s| s.cursor.char_range());
        let (cursor, collapsed) = range.map_or((nchars, true), |r| (r.primary.index, r.primary.index == r.secondary.index));
        if slash_active {
            let len = items.len();
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                ed.slash_sel = (ed.slash_sel + 1) % len;
                ed.slash_moved = true;
            }
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
                ed.slash_sel = (ed.slash_sel + len - 1) % len;
                ed.slash_moved = true;
            }
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab)) {
                *act = Some(slash_act(idx, items[ed.slash_sel]));
            }
        } else {
            if b.kind != BlockKind::Code {
                let shift = ui.input(|i| i.modifiers.shift);
                if !shift && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
                    let listy = matches!(b.kind, BlockKind::Todo | BlockKind::Bullet | BlockKind::Numbered | BlockKind::Quote | BlockKind::Callout | BlockKind::Toggle);
                    if b.text.is_empty() && listy {
                        *act = Some(if b.indent > 0 { Act::Indent(idx, -1) } else { Act::Convert(idx, BlockKind::Text, false) });
                    } else {
                        *act = Some(Act::Split(idx, cursor));
                    }
                }
                let tab = ui.input_mut(|i| {
                    let a = i.consume_key(Modifiers::NONE, Key::Tab);
                    let c = i.consume_key(Modifiers::SHIFT, Key::Tab);
                    a || c
                });
                if tab {
                    *act = Some(Act::Indent(idx, if shift { -1 } else { 1 }));
                }
            }
            if collapsed && cursor == 0 && ui.input(|i| i.key_pressed(Key::Backspace)) {
                ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace));
                if b.kind != BlockKind::Text {
                    *act = Some(Act::Convert(idx, BlockKind::Text, false));
                } else if b.indent > 0 {
                    *act = Some(Act::Indent(idx, -1));
                } else if idx > 0 {
                    *act = Some(Act::Merge(idx));
                }
            }
            if b.kind != BlockKind::Code {
                let (row, rows) = ctx.data(|d| d.get_temp::<(usize, usize)>(id.with("rows"))).unwrap_or((1, 1));
                if row == 0 && idx > 0 && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
                    *act = Some(Act::FocusPrev(idx));
                }
                if row + 1 >= rows && idx + 1 < n && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                    *act = Some(Act::FocusNext(idx));
                }
            }
        }
    }

    let hint_txt = if b.kind == BlockKind::Text && !focused { String::new() } else { hint.to_owned() };
    let is_code = b.kind == BlockKind::Code;
    let (pal2, font2) = (*pal, font.clone());
    let mut layouter = |ui: &Ui, s: &str, wrap: f32| {
        let mut job = if is_code { code_job(s, &font2, &pal2) } else { rich_job(s, &font2, color, &pal2, false) };
        job.wrap.max_width = wrap;
        ui.fonts(|f| f.layout_job(job))
    };
    let out = TextEdit::multiline(&mut b.text)
        .id(id)
        .frame(false)
        .font(font.clone())
        .text_color(color)
        .desired_width(width)
        .desired_rows(1)
        .lock_focus(true)
        .margin(Margin::symmetric(0, 3))
        .layouter(&mut layouter)
        .hint_text(RichText::new(hint_txt).font(font).color(with_alpha(pal.muted, 0.75)))
        .show(ui);

    let mut cursor_idx = 0;
    if let Some(cr) = out.cursor_range {
        cursor_idx = cr.primary.ccursor.index;
        let row = cr.primary.rcursor.row;
        let rows = out.galley.rows.len();
        ctx.data_mut(|d| d.insert_temp(id.with("rows"), (row, rows)));
    }

    if out.response.changed() && b.kind == BlockKind::Text && act.is_none() {
        if let Some((kind, n)) = md_shortcut(&b.text, cursor_idx) {
            *act = Some(Act::Strip(idx, kind, n));
        }
    }

    // slash command popup
    if slash_active {
        let pos = out.response.rect.left_bottom() + vec2(0.0, 6.0);
        let t = appear(&ctx, Id::new("slash_appear"), 0.14);
        let moved = std::mem::take(&mut ed.slash_moved);
        let sel = ed.slash_sel;
        let mut pick: Option<SlashItem> = None;
        let screen = ctx.screen_rect();
        let y = pos.y.min(screen.bottom() - 380.0).max(10.0);
        egui::Area::new(Id::new("slash_menu")).order(Order::Foreground).fixed_pos(pos2(pos.x, y + (1.0 - t) * -5.0)).show(&ctx, |ui| {
            ui.set_opacity(t);
            crate::widgets::popup_frame(pal).show(ui, |ui| {
                ui.set_width(310.0);
                ScrollArea::vertical().max_height(340.0).auto_shrink([false, true]).show(ui, |ui| {
                    let mut last_sec = "";
                    for (i, k) in items.iter().enumerate() {
                        if k.sec != last_sec {
                            section_label(ui, pal, k.sec);
                            last_sec = k.sec;
                        }
                        let r = menu_item(ui, pal, k.glyph, k.label, Some(k.hint), i == sel, false);
                        if i == sel && moved {
                            r.scroll_to_me(None);
                        }
                        if r.hovered() {
                            ed.slash_sel = i;
                        }
                        if r.hovered() && ui.input(|i| i.pointer.primary_pressed()) {
                            pick = Some(*k);
                        }
                    }
                });
            });
        });
        if let Some(k) = pick {
            *act = Some(slash_act(idx, k));
        }
    } else if focused || !ctx.memory(|m| m.focused().is_some()) {
        reset_appear(&ctx, Id::new("slash_appear"));
    }

    out.response.changed()
}

fn swatches(ui: &mut Ui, pal: &Palette, cur: u8, is_bg: bool, pick: &mut Option<u8>) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
        for c in 0..=9u8 {
            let (r, resp) = ui.allocate_exact_size(vec2(21.0, 21.0), Sense::click());
            let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
            let col = if c == 0 { pal.muted } else { tag_color(c as usize - 1) };
            if is_bg {
                let fill = if c == 0 { pal.hover } else { col.gamma_multiply(0.35) };
                ui.painter().rect_filled(r, 5.0, fill);
            } else {
                ui.painter().rect_filled(r, 5.0, with_alpha(pal.hover, 0.4 + 0.6 * t));
                ui.painter().text(r.center(), Align2::CENTER_CENTER, "A", bold(13.0), if c == 0 { pal.text } else { col });
            }
            if cur == c {
                ui.painter().rect_stroke(r, 5.0, Stroke::new(1.6_f32, pal.accent), egui::StrokeKind::Inside);
            }
            let name = if c == 0 { "Default" } else { TAG_NAMES[c as usize - 1] };
            if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(name).clicked() {
                *pick = Some(c);
            }
        }
    });
}

fn popups(ui: &mut Ui, page: &mut Page, next_id: &mut u64, ed: &mut EditorState, pal: &Palette) -> bool {
    let ctx = ui.ctx().clone();
    let mut changed = false;

    // block menu
    if let Some((bid, pos)) = ed.block_menu {
        let mut action: Option<Act> = None;
        let idx = page.blocks.iter().position(|b| b.id == bid);
        if let Some(idx) = idx {
            let cur_kind = page.blocks[idx].kind;
            let (cur_c, cur_bg) = (page.blocks[idx].color, page.blocks[idx].bg);
            let y = pos.y.min(ctx.screen_rect().bottom() - 480.0).max(10.0);
            let max_h = (ctx.screen_rect().bottom() - y - 20.0).clamp(240.0, 560.0);
            let open = popup_area(&ctx, Id::new("block_menu"), pos2(pos.x, y), pal, &mut ed.fresh, 250.0, |ui| {
                ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                    if cur_kind.has_text() {
                        section_label(ui, pal, "TURN INTO");
                        for k in BlockKind::TEXTY.iter() {
                            if menu_item(ui, pal, k.glyph(), k.label(), None, *k == cur_kind, false).clicked() {
                                action = Some(Act::Convert(idx, *k, false));
                            }
                        }
                        ui.add_space(4.0);
                        section_label(ui, pal, "COLOR");
                        let mut pick = None;
                        swatches(ui, pal, cur_c, false, &mut pick);
                        if let Some(c) = pick {
                            action = Some(Act::SetColor(idx, c, false));
                        }
                    }
                    section_label(ui, pal, "BACKGROUND");
                    let mut pick = None;
                    swatches(ui, pal, cur_bg, true, &mut pick);
                    if let Some(c) = pick {
                        action = Some(Act::SetColor(idx, c, true));
                    }
                    ui.add_space(4.0);
                    ui.painter().hline(ui.available_rect_before_wrap().x_range(), ui.cursor().top(), Stroke::new(1.0_f32, pal.border));
                    ui.add_space(5.0);
                    if menu_item(ui, pal, "⇥", "Indent", None, false, false).clicked() {
                        action = Some(Act::Indent(idx, 1));
                    }
                    if menu_item(ui, pal, "⇤", "Outdent", None, false, false).clicked() {
                        action = Some(Act::Indent(idx, -1));
                    }
                    if menu_item(ui, pal, "⎘", "Duplicate", None, false, false).clicked() {
                        action = Some(Act::Dup(idx));
                    }
                    if menu_item(ui, pal, "×", "Delete", None, false, true).clicked() {
                        action = Some(Act::Remove(idx));
                    }
                });
            });
            let close = action.is_some() && !matches!(action, Some(Act::SetColor(..)));
            if let Some(a) = action {
                apply(page, a, next_id, ed);
                changed = true;
            }
            if !open || close {
                ed.block_menu = None;
            }
        } else {
            ed.block_menu = None;
        }
    }

    // icon picker
    if let Some(pos) = ed.icon_picker {
        let mut chosen: Option<String> = None;
        let mut remove = false;
        let open = popup_area(&ctx, Id::new("icon_picker"), pos, pal, &mut ed.fresh, 268.0, |ui| {
            section_label(ui, pal, "ICON");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
                for e in ICONS {
                    let (r, resp) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::click());
                    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                    ui.painter().rect_filled(r, 8.0, with_alpha(pal.hover, t));
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, e, body(22.0 + 3.0 * t), pal.text);
                    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        chosen = Some(e.to_owned());
                    }
                }
            });
            ui.add_space(4.0);
            if menu_item(ui, pal, "×", "Remove icon", None, false, false).clicked() {
                remove = true;
            }
        });
        let chosen_flag = chosen.is_some();
        if let Some(e) = chosen {
            page.icon = e;
            changed = true;
        }
        if remove {
            page.icon.clear();
            changed = true;
        }
        if !open || chosen_flag || remove {
            ed.icon_picker = None;
        }
    }

    // cover picker
    if let Some(pos) = ed.cover_picker {
        let mut chosen: Option<usize> = None;
        let open = popup_area(&ctx, Id::new("cover_picker"), pos, pal, &mut ed.fresh, 268.0, |ui| {
            section_label(ui, pal, "GRADIENTS");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                for (i, (a, b)) in COVERS.iter().enumerate() {
                    let (r, resp) = ui.allocate_exact_size(vec2(60.0, 38.0), Sense::click());
                    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                    let rr = r.expand(t * 2.0);
                    gradient_rect(ui, rr, Color32::from_rgb(a.0, a.1, a.2), Color32::from_rgb(b.0, b.1, b.2));
                    if page.cover == Some(i) {
                        ui.painter().rect_stroke(rr, 2.0, Stroke::new(2.0_f32, pal.accent), egui::StrokeKind::Outside);
                    }
                    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        chosen = Some(i);
                    }
                }
            });
        });
        if let Some(i) = chosen {
            page.cover = Some(i);
            changed = true;
        }
        if !open || chosen.is_some() {
            ed.cover_picker = None;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blk(id: u64, kind: BlockKind, text: &str, indent: u8) -> Block {
        Block { id, kind, text: text.into(), checked: false, indent, collapsed: false, color: 0, bg: 0, link: None }
    }

    fn page(blocks: Vec<Block>) -> Page {
        let mut ws = Workspace::seed();
        let id = ws.add_page(None, PageKind::Doc);
        let mut p = ws.page(id).unwrap().clone();
        p.blocks = blocks;
        p
    }

    #[test]
    fn inline_spans() {
        let s = parse_spans("a **b** *c* ~~d~~ `e` f");
        let styled: Vec<(&str, bool, bool, bool, bool)> =
            s.iter().filter(|x| !x.marker).map(|x| (x.text.as_str(), x.bold, x.italic, x.strike, x.code)).collect();
        assert!(styled.contains(&("b", true, false, false, false)));
        assert!(styled.contains(&("c", false, true, false, false)));
        assert!(styled.contains(&("d", false, false, true, false)));
        assert!(styled.contains(&("e", false, false, false, true)));
        // text is preserved byte-for-byte (needed for cursor mapping)
        assert_eq!(s.iter().map(|x| x.text.as_str()).collect::<String>(), "a **b** *c* ~~d~~ `e` f");
        // unmatched markers stay literal
        assert!(parse_spans("2 * 3 = 6").iter().all(|x| !x.marker));
        assert!(parse_spans("**open").iter().all(|x| !x.marker));
    }

    #[test]
    fn markdown_shortcuts() {
        assert_eq!(md_shortcut("# ", 2), Some((BlockKind::H1, 2)));
        assert_eq!(md_shortcut("> ", 2), Some((BlockKind::Toggle, 2)));
        assert_eq!(md_shortcut("---", 3), Some((BlockKind::Divider, 3)));
        assert_eq!(md_shortcut("hello", 5), None);
    }

    #[test]
    fn indent_moves_children() {
        let mut p = page(vec![
            blk(1, BlockKind::Bullet, "a", 0),
            blk(2, BlockKind::Bullet, "b", 0),
            blk(3, BlockKind::Bullet, "b1", 1),
            blk(4, BlockKind::Bullet, "c", 0),
        ]);
        let mut next = 100;
        let mut ed = EditorState::default();
        apply(&mut p, Act::Indent(1, 1), &mut next, &mut ed);
        let ind: Vec<u8> = p.blocks.iter().map(|b| b.indent).collect();
        assert_eq!(ind, vec![0, 1, 2, 0]);
        // cannot indent deeper than previous + 1
        apply(&mut p, Act::Indent(1, 1), &mut next, &mut ed);
        assert_eq!(p.blocks[1].indent, 1);
        apply(&mut p, Act::Indent(1, -1), &mut next, &mut ed);
        assert_eq!(p.blocks.iter().map(|b| b.indent).collect::<Vec<_>>(), vec![0, 0, 1, 0]);
    }

    #[test]
    fn drag_moves_subtree_and_split_keeps_indent() {
        let mut p = page(vec![
            blk(1, BlockKind::Text, "a", 0),
            blk(2, BlockKind::Toggle, "t", 0),
            blk(3, BlockKind::Text, "child", 1),
            blk(4, BlockKind::Text, "z", 0),
        ]);
        let mut next = 100;
        let mut ed = EditorState::default();
        apply(&mut p, Act::MoveTo(1, 4), &mut next, &mut ed);
        assert_eq!(p.blocks.iter().map(|b| b.id).collect::<Vec<_>>(), vec![1, 4, 2, 3]);
        apply(&mut p, Act::MoveTo(2, 3), &mut next, &mut ed); // into own subtree: ignored
        assert_eq!(p.blocks.iter().map(|b| b.id).collect::<Vec<_>>(), vec![1, 4, 2, 3]);
        apply(&mut p, Act::Split(3, 2), &mut next, &mut ed);
        assert_eq!(p.blocks[4].text, "ild");
        assert_eq!(p.blocks[4].indent, 1);
        apply(&mut p, Act::Merge(4), &mut next, &mut ed);
        assert_eq!(p.blocks[3].text, "child");
    }

    #[test]
    fn collapsed_toggle_hides_children() {
        let mut bl = vec![blk(1, BlockKind::Toggle, "t", 0), blk(2, BlockKind::Text, "c", 1), blk(3, BlockKind::Text, "d", 0)];
        bl[0].collapsed = true;
        assert!(hidden(&bl, 1));
        assert!(!hidden(&bl, 2));
    }

    #[test]
    fn slash_menu_filters() {
        assert!(slash_items("").len() > 15);
        assert_eq!(slash_items("todo")[0].label, "To-do list");
        assert!(slash_items("kanban").iter().any(|i| i.label == "Board database"));
        assert!(slash_items("zzzzzz").is_empty());
    }

    #[test]
    fn special_blocks_request_workspace_actions() {
        let mut p = page(vec![blk(1, BlockKind::Text, "/page", 0)]);
        let mut next = 100;
        let mut ed = EditorState::default();
        apply(&mut p, Act::Special(0, Slash::NewPage), &mut next, &mut ed);
        assert!(matches!(ed.req, Some(Req::NewPage(1))));
        assert_eq!(p.blocks[0].kind, BlockKind::PageLink);
        assert_eq!(p.blocks.len(), 2, "a trailing text block is kept so the user can keep typing");
    }
}

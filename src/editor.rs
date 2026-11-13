//! The Notion-style block editor: nesting, toggles, colours, inline formatting, slash menu, drag & drop.

use crate::widgets::LayerRaise;
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
use crate::lucide as l;
use crate::media;
use crate::mediablocks;
use crate::multisel;
use crate::pickers;
use crate::rich::{self, Fmt, Rich};
use crate::clip;
use crate::platform;
use crate::widgets::{
    self, appear, context_menu, gradient_rect, menu_item, popup_area, replace_chars, reset_appear, right_clicked, section_label,
    slice_chars, with_alpha, CtxItem,
};

/// Width of the left gutter that holds the "+" / drag handle of a block. Phones get a slimmer one so text keeps its width.
pub fn gutter() -> f32 {
    if crate::touch::compact() {
        38.0
    } else {
        56.0
    }
}
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
    /// Set by the `/dashboard` command: the page gets a widget grid.
    pub want_dash: bool,
    /// Button blocks: the block whose actions should run (set by a click, consumed by the app) / whose editor should open.
    pub press_button: Option<u64>,
    pub edit_button: Option<u64>,
    /// Set by the `/widget` command: open the widget gallery for this (new) widget block.
    pub want_widget: Option<u64>,
    pub focus: Option<(u64, usize)>,
    /// The block the user is editing / last touched (touch devices keep its handle + toolbar visible; survives focus loss).
    pub active: Option<u64>,
    /// Phone toolbar: 0 = closed, 1 = "turn into", 2 = "insert".
    pub tool_menu: u8,
    /// Time until which the phone toolbar stays up although the text field lost focus (a tap on a toolbar button does that).
    pub bar_grace: f64,
    pub slash_sel: usize,
    pub slash_moved: bool,
    /// `[[` page suggestions: titles + icons of the workspace pages (filled on demand by `show_page`), selected row, dismissed trigger.
    pub pages: Vec<(String, String)>,
    pub want_pages: bool,
    pub link_sel: usize,
    pub link_dismiss: Option<(u64, usize)>,
    pub block_menu: Option<(u64, egui::Pos2)>,
    pub dragging: Option<u64>,
    pub icon_picker: Option<egui::Pos2>,
    pub cover_picker: Option<egui::Pos2>,
    pub fresh: bool,
    pub req: Option<Req>,
    pub nav: Option<u64>,
    /// A `[[Page title]]` that was clicked: the app opens the page (and creates it when it does not exist yet).
    pub wiki: Option<String>,
    pub scroll_to: Option<u64>,
    pub scale: f32,
    pub db: DbState,
    /// Database UI state of every linked-view block on the page (block id → state).
    pub dbv: HashMap<u64, DbState>,
    /// Right-click menu (text selection / block).
    pub menu: Option<EdMenu>,
    pub menu_fresh: bool,
    /// Set when a right-click was consumed this frame.
    pub rc_frame: bool,
    /// Re-focus a block and restore its selection (after a menu stole the focus).
    pub restore_sel: Option<(u64, usize, usize)>,
    pub paste_wait: Option<(u64, usize, usize)>,
    /// Floating "selection toolbar".
    pub fmt_bar: Option<FmtBar>,
    /// Measured block heights (id → (height, layout key)); lets us skip laying out off-screen blocks.
    pub heights: HashMap<u64, (f32, u64)>,
    /// Multi-block selection (ids) and the marquee drag state (start point in content space).
    pub sel: Vec<u64>,
    pub sel_base: Vec<u64>,
    /// Blocks were picked with a touch long-press (selection mode: the next long press adds to it).
    pub touch_sel: bool,
    pub marquee: Option<egui::Vec2>,
    /// 0 = none, 1 = "turn into" panel, 2 = colour panel of the selection toolbar.
    pub sel_menu: u8,
    pub want_undo: bool,
    pub undo: Vec<(u64, Vec<Block>)>,
    pub redo: Vec<(u64, Vec<Block>)>,
    /// PDF reader: "open this" request from a block (media id, url, title) and the active reader request.
    pub pdf_ask: Option<(u64, String, String)>,
    pub pdf_req: Option<crate::pdf::Request>,
    /// History tracking (see `history.rs`): page, previous blocks, fingerprints, last change, dirty-since.
    pub hist_page: u64,
    pub hist_prev: Vec<Block>,
    pub hist_fp: u64,
    pub hist_sig: u64,
    pub hist_last: f64,
    pub hist_dirty: f64,
    /// Live web embeds requested this frame (web build draws iframes over them).
    pub embed_rects: Vec<platform::EmbedRect>,
    /// File attachment the user wants saved: (media id, file name).
    pub dl_req: Option<(u64, String)>,
    pub media_sizes: HashMap<u64, usize>,
    pub cover_reposition: bool,
    pub cur_page: u64,
    /// Format toggled with an empty selection (block id, caret, format) — applies to the next typed text.
    pub pending: Option<(u64, usize, Fmt)>,
    /// Link editor popup opened from the selection toolbar.
    pub link_edit: Option<LinkEdit>,
}

pub struct LinkEdit {
    pub bid: u64,
    pub a: usize,
    pub b: usize,
    pub url: String,
    pub pos: egui::Pos2,
    pub fresh: bool,
}

#[derive(Clone, Copy)]
pub enum EdMenu {
    Text { bid: u64, a: usize, b: usize, pos: egui::Pos2 },
    Block { bid: u64, pos: egui::Pos2 },
}

#[derive(Clone, Copy)]
pub struct FmtBar {
    pub bid: u64,
    pub pos: egui::Pos2,
    pub a: usize,
    pub b: usize,
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
    /// Turn the page into a dashboard (adds the widget grid to the page).
    Dashboard,
    /// A column layout with this many columns.
    Columns(u8),
    /// A linked, filterable view of any database.
    DbView,
    /// A dashboard widget (chart, number, task list …) placed anywhere in the page.
    Widget,
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
    v.push(SlashItem { act: Slash::Block(BlockKind::Divider), label: "Divider", hint: "Visually divide blocks.", glyph: l::MINUS, kw: "divider line hr separator", sec: "BASIC BLOCKS" });
    let med = "MEDIA";
    for (k, label, hint, glyph, kw) in [
        (BlockKind::Image, "Image", "Upload a picture or paste an image link.", l::IMAGE, "image picture photo upload png jpg gif"),
        (BlockKind::Embed, "Video", "Embed a YouTube, Vimeo or Loom video.", l::VIDEO, "video youtube vimeo loom movie clip"),
        (BlockKind::Embed, "Audio", "Embed Spotify or any audio player.", l::MUSIC, "audio spotify music podcast sound"),
        (BlockKind::Embed, "Embed", "Embed any website, map, Figma file or document.", l::GLOBE, "embed website iframe map figma codepen docs"),
        (BlockKind::Bookmark, "Web bookmark", "Save a link as a visual card.", l::BOOKMARK, "bookmark link url web card"),
        (BlockKind::File, "PDF", "Upload a PDF and read it right inside the page.", l::FILE_TEXT, "pdf reader document read viewer paper"),
        (BlockKind::File, "File", "Attach a file to this page.", l::PAPERCLIP, "file attachment upload document"),
    ] {
        v.push(SlashItem { act: Slash::Block(k), label, hint, glyph, kw, sec: med });
    }
    for (n, label, glyph) in [(2u8, "2 columns", l::COLUMNS_2), (3, "3 columns", l::COLUMNS_3), (4, "4 columns", l::LAYOUT_GRID), (5, "5 columns", l::LAYOUT_GRID)] {
        v.push(SlashItem { act: Slash::Columns(n), label, hint: "Lay content out side by side.", glyph, kw: "column columns layout side by side split grid", sec: "LAYOUT" });
    }
    v.push(SlashItem { act: Slash::Block(BlockKind::Button), label: "Button", hint: "One click runs actions: set properties, create tasks, move rows, notify.", glyph: l::MOUSE_POINTER_2, kw: "button automation action click run rule trigger macro one-click", sec: "AUTOMATION" });
    let adv = "ADVANCED";
    v.push(SlashItem { act: Slash::Block(BlockKind::Toc), label: "Table of contents", hint: "Jump links to every heading.", glyph: l::TABLE_OF_CONTENTS, kw: "table of contents toc outline", sec: adv });
    v.push(SlashItem { act: Slash::NewPage, label: "Page", hint: "Embed a new sub-page inside this page.", glyph: l::FILE_PLUS, kw: "page subpage new child", sec: adv });
    v.push(SlashItem { act: Slash::Link, label: "Link to page", hint: "Link to an existing page.", glyph: l::LINK_2, kw: "link page mention reference", sec: adv });
    v.push(SlashItem { act: Slash::Dashboard, label: "Dashboard", hint: "Turn this page into a dashboard of charts, tables and widgets.", glyph: l::LAYOUT_DASHBOARD, kw: "dashboard widgets chart charts kpi board analytics overview", sec: adv });
    v.push(SlashItem { act: Slash::Widget, label: "Dashboard widget", hint: "Place a chart, number, task list or timer anywhere in the page.", glyph: l::CHART_PIE, kw: "widget chart kpi number gauge stats metric dashboard graph plot timer tasks", sec: adv });
    v.push(SlashItem { act: Slash::Today, label: "Today's date", hint: "Insert the current date.", glyph: l::CALENDAR_DAYS, kw: "date today now time", sec: adv });
    v.push(SlashItem { act: Slash::DbView, label: "Linked view of database", hint: "Show a filtered view of any database on this page.", glyph: l::TABLE_2, kw: "linked view database embed inline filter table board calendar today week", sec: "DATABASES" });
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
    /// A URL was pasted into an empty text block: turn it into an image / embed / bookmark.
    UrlBlock(usize),
    /// Paste the exact blocks that were copied (kind, indent, colours, media … preserved).
    PasteBlocks(usize, Vec<Block>),
    /// Move a block (with its children) and adopt the nesting level of its new place (used when it enters / leaves a column).
    MoveAs(usize, usize, u8),
    /// Column layouts: add a column after the column block, remove a column, equal widths, unwrap, delete.
    ColAdd(usize),
    ColDel(usize),
    ColEqual(usize),
    ColUnwrap(usize),
    ColDelete(usize),
    /// Insert an empty block after this one and turn *that* into a slash-menu item (phone toolbar "insert").
    InsertSlash(usize, SlashItem),
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

pub(crate) struct Span {
    pub(crate) text: String,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) strike: bool,
    pub(crate) code: bool,
    pub(crate) ul: bool,
    pub(crate) hl: bool,
    pub(crate) link: Option<String>,
    pub(crate) marker: bool,
}

fn is_escapable(c: char) -> bool {
    matches!(c, '\\' | '*' | '_' | '~' | '=' | '`' | '[' | ']')
}

/// Removes the backslash escapes of a marked-up string fragment.
pub(crate) fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' && it.peek().map_or(false, |n| is_escapable(*n)) {
            continue;
        }
        out.push(c);
    }
    out
}

/// Finds `[text](url)` starting at `i`; returns the indices of `]` and `)`.
fn find_link(ch: &[char], i: usize) -> Option<(usize, usize)> {
    let n = ch.len();
    let mut j = i + 1;
    while j < n && ch[j] != ']' {
        if ch[j] == '\\' && j + 1 < n && is_escapable(ch[j + 1]) {
            j += 2;
            continue;
        }
        if ch[j] == '[' || ch[j] == '\n' {
            return None;
        }
        j += 1;
    }
    if j >= n {
        return None;
    }
    if j == i + 1 || j + 2 >= n || ch[j + 1] != '(' {
        return None;
    }
    let mut k = j + 2;
    while k < n && ch[k] != ')' {
        if ch[k].is_whitespace() {
            return None;
        }
        k += 1;
    }
    if k >= n {
        return None;
    }
    Some((j, k))
}

/// Splits `**bold**`, `*italic*`, `__underline__`, `~~strike~~`, `==highlight==`, `` `code` `` and `[link](url)` into styled spans
/// (markers are kept as spans so the text is preserved byte-for-byte).
pub(crate) fn parse_spans(s: &str) -> Vec<Span> {
    let ch: Vec<char> = s.chars().collect();
    let n = ch.len();
    let (mut bo, mut it, mut st, mut co, mut ul, mut hl) = (false, false, false, false, false, false);
    let mut out: Vec<Span> = vec![];
    let mut cur = String::new();
    let mut i = 0;
    let has_later = |from: usize, pat: &[char]| -> bool { (from..n.saturating_sub(pat.len() - 1)).any(|j| ch[j..j + pat.len()] == *pat) };
    macro_rules! flush {
        ($marker:expr) => {
            if !cur.is_empty() {
                out.push(Span { text: std::mem::take(&mut cur), bold: bo, italic: it, strike: st, code: co, ul, hl, link: None, marker: $marker });
            }
        };
    }
    macro_rules! toggle2 {
        ($flag:ident, $mark:expr) => {{
            flush!(false);
            cur.push_str($mark);
            if $flag {
                flush!(true);
                $flag = false;
            } else {
                $flag = true;
                flush!(true);
            }
            i += 2;
        }};
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
        if c == '\\' && i + 1 < n && is_escapable(ch[i + 1]) {
            flush!(false);
            cur.push('\\');
            flush!(true);
            cur.push(ch[i + 1]);
            i += 2;
            continue;
        }
        let open_ok = |from: usize| from < n && !ch[from].is_whitespace();
        if c == '[' {
            if let Some((j, k)) = find_link(&ch, i) {
                flush!(false);
                cur.push('[');
                flush!(true);
                let inner: String = unescape(&ch[i + 1..j].iter().collect::<String>());
                out.push(Span { text: inner, bold: bo, italic: it, strike: st, code: false, ul, hl, link: Some(ch[j + 2..k].iter().collect()), marker: false });
                cur = ch[j..=k].iter().collect();
                flush!(true);
                i = k + 1;
                continue;
            }
        }
        if c == '`' && has_later(i + 1, &['`']) {
            flush!(false);
            cur.push(c);
            co = true;
            flush!(true);
            i += 1;
        } else if c == '*' && two && (bo || has_later(i + 2, &['*', '*'])) {
            toggle2!(bo, "**");
        } else if c == '~' && two && (st || has_later(i + 2, &['~', '~'])) {
            toggle2!(st, "~~");
        } else if c == '_' && two && (ul || (open_ok(i + 2) && has_later(i + 2, &['_', '_']))) {
            toggle2!(ul, "__");
        } else if c == '=' && two && (hl || (open_ok(i + 2) && has_later(i + 2, &['=', '=']))) {
            toggle2!(hl, "==");
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
    crate::rich::has_marks(s)
}

pub fn rich_job(text: &str, font: &FontId, color: Color32, pal: &Palette, strip: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    for sp in parse_spans(text) {
        if strip && sp.marker {
            continue;
        }
        let is_link = sp.link.is_some() && !sp.marker;
        let fg = if sp.marker {
            with_alpha(pal.muted, 0.75)
        } else if is_link {
            pal.accent
        } else if sp.code {
            tag_color(5)
        } else {
            color
        };
        let fmt = TextFormat {
            font_id: if sp.code { FontId::monospace(font.size * 0.88) } else if sp.bold { bold(font.size) } else { font.clone() },
            color: fg,
            background: if sp.hl && !sp.marker {
                Color32::from_rgba_unmultiplied(255, 208, 0, 100)
            } else if sp.code && !sp.marker {
                pal.hover
            } else {
                Color32::TRANSPARENT
            },
            italics: sp.italic,
            strikethrough: if sp.strike && !sp.marker { Stroke::new(1.3_f32, color) } else { Stroke::NONE },
            underline: if (sp.ul || is_link) && !sp.marker { Stroke::new(1.2_f32, fg) } else { Stroke::NONE },
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
    ed.rc_frame = false;
    ed.embed_rects.clear();
    ed.cur_page = id;
    if std::mem::take(&mut ed.want_pages) {
        ed.pages = ws.pages.iter().filter(|p| !p.trashed && !p.row && p.id != id).map(|p| (p.title.clone(), p.icon.clone())).collect();
        ed.pages.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    }
    let avail = ui.available_width();
    // two-finger pinch on the page = content size (the same value as the slider in the ⋯ menu)
    if let Some(d) = ui.input(|i| i.multi_touch().map(|m| m.zoom_delta)) {
        if d.is_finite() && (d - 1.0).abs() > 1e-4 && ed.pdf_req.is_none() {
            let p = &mut ws.pages[idx];
            p.zoom = crate::model::clamp_zoom(p.content_zoom() * d.clamp(0.9, 1.1));
            ui.ctx().request_repaint();
        }
    }
    let (kind, is_row, full, small, zoom) = {
        let p = &ws.pages[idx];
        (p.kind, p.row, p.full_width, p.small_text, p.content_zoom())
    };
    ed.scale = zoom * if small { 0.9 } else { 1.0 };
    // Page-wide drag surface (registered first = lowest): blank space starts the selection rectangle.
    let top = ui.cursor().min;
    let bg_rect = Rect::from_min_max(pos2(ui.max_rect().left(), top.y), pos2(ui.max_rect().right(), top.y + 30000.0));
    let bg = ui.interact(bg_rect, Id::new(("page_bg", id)), Sense::click_and_drag());
    changed |= pickers::cover(ui, &mut ws.pages[idx], ed, pal, avail);

    let dash_board = if kind == PageKind::Doc && !is_row { ws.dash.page_board(id) } else { None };
    let wide = kind == PageKind::Database || full || dash_board.is_some();
    // phones: use (almost) the whole width, no 300 pt minimum that would push the page off-screen
    let compact = avail < 640.0;
    let col_w = if compact { (avail - 16.0).max(200.0) } else if wide { (avail - 96.0).max(300.0) } else { (avail - 80.0).clamp(300.0, 900.0) };
    let left = if compact { 8.0 } else if wide { 48.0 } else { ((avail - col_w) / 2.0).max(24.0) };
    let content_w = (col_w - gutter()).max(if compact { 140.0 } else { 200.0 });

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
            if let Some(b) = dash_board {
                // this page is a dashboard: the widget grid sits between the header and the blocks
                let nav = crate::dashboard::embedded(ui, ws, pal, b);
                if let Some(o) = nav.out.open {
                    ed.nav = Some(o);
                }
                if let Some(t) = nav.toast {
                    crate::dashcfg::set_toast(&t);
                }
                ui.add_space(16.0);
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
                        // the page's own blocks are lent to the block renderer so that linked database views can use the whole workspace
                        let dv_before = ws.pages[idx].blocks.iter().filter(|b| b.kind == BlockKind::DbView).count();
                        let shell = ws.pages[idx].shell();
                        let mut page = std::mem::replace(&mut ws.pages[idx], shell);
                        changed |= blocks(ui, &mut page, ws, ed, pal, content_w, &links, &bg);
                        let dv_after = page.blocks.iter().filter(|b| b.kind == BlockKind::DbView).count();
                        ws.pages[idx] = page;
                        if dv_after < dv_before {
                            ws.prune_linked_views();
                        }
                    }
                    backlinks(ui, ws, id, ed, pal, content_w);
                }
            }
        });
    });

    if std::mem::take(&mut ed.want_dash) && kind == PageKind::Doc {
        ws.dash.ensure_page_board(id, "Dashboard");
        if let Some(p) = ws.pages.get_mut(idx) {
            p.full_width = true;
        }
        changed = true;
        ui.ctx().request_repaint();
    }
    if let Some(bid) = ed.want_widget.take() {
        crate::inlinew::request_pick(ws, bid);
        ui.ctx().request_repaint();
    }
    {
        // menu / gallery / settings panel of the dashboard widgets placed inside this page
        let o = crate::inlinew::overlays(ui.ctx(), ws, pal);
        if let Some(op) = o.out.open {
            ed.nav = Some(op);
        }
        if let Some(t) = o.toast {
            crate::dashcfg::set_toast(&t);
        }
    }
    ui.add_space(80.0);
    {
        let Workspace { pages, next_id, .. } = &mut *ws;
        changed |= popups(ui, &mut pages[idx], next_id, ed, pal);
        if changed {
            normalize_layout(&mut pages[idx].blocks, next_id);
        }
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
        ui.add_space(gutter());
        ui.label(RichText::new(format!("LINKED FROM  ·  {}", links.len())).font(bold(11.5)).color(pal.muted));
    });
    for l in links {
        let Some(p) = ws.page(l) else { continue };
        let (rect, resp) = ui.allocate_exact_size(vec2(w + gutter(), 28.0), Sense::click());
        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
        let r = Rect::from_min_max(pos2(rect.left() + gutter() - 6.0, rect.top()), rect.right_bottom());
        ui.painter().rect_filled(r, 6.0, with_alpha(pal.hover, t));
        let icon = if p.icon.is_empty() { l::FILE_TEXT } else { &p.icon };
        icons::paint_page_icon(ui, pos2(r.left() + 18.0, r.center().y), icon, 17.0, pal.text);
        let title = if p.title.is_empty() { "Untitled" } else { &p.title };
        ui.painter().text(pos2(r.left() + 38.0, r.center().y), Align2::LEFT_CENTER, title, body(14.5), pal.text);
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            ed.nav = Some(l);
        }
    }
}

fn header(ui: &mut Ui, page: &mut Page, ed: &mut EditorState, pal: &Palette, content_w: f32) -> bool {
    let mut changed = false;
    let has_cover = page.has_cover();
    ui.add_space(if has_cover { 0.0 } else { 40.0 });

    let icon_h = 76.0;
    let (rect, _) = ui.allocate_exact_size(vec2(content_w + gutter(), if has_cover { 44.0 } else { icon_h }), Sense::hover());
    let hdr_top = rect.top();
    if !page.icon.is_empty() {
        let size = 68.0;
        let top = if has_cover { hdr_top - 34.0 } else { hdr_top };
        let ir = Rect::from_min_size(pos2(rect.left() + gutter() - 8.0, top), vec2(size + 16.0, size + 8.0));
        let resp = ui.interact(ir, Id::new(("icon", page.id)), Sense::click());
        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
        ui.painter().rect_filled(ir, 12.0, with_alpha(pal.hover, t));
        icons::paint_page_icon(ui, pos2(ir.center().x, ir.center().y + 2.0), &page.icon, 60.0, pal.text);
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            ed.icon_picker = Some(ir.left_bottom() + vec2(0.0, 6.0));
            ed.fresh = true;
        }
    }

    let row_h = 30.0;
    let (row, _) = ui.allocate_exact_size(vec2(content_w + gutter(), row_h), Sense::hover());
    let hov = crate::touch::reveal(ui.rect_contains_pointer(Rect::from_min_max(pos2(row.left(), hdr_top - 10.0), pos2(row.right(), row.bottom() + 56.0))));
    let t = ui.ctx().animate_bool_with_time(Id::new(("hdr_h", page.id)), hov, 0.15);
    let mut x = row.left() + gutter();
    let mut buttons: Vec<(String, &str)> = vec![];
    if page.icon.is_empty() {
        buttons.push((format!("{}  Add icon", l::SMILE), "ai"));
    }
    if !page.has_cover() {
        buttons.push((format!("{}  Add cover", l::IMAGE), "ac"));
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
        ui.painter().text(r.center(), Align2::CENTER_CENTER, &label, body(13.0), with_alpha(lerp_color(pal.muted, pal.text, h), t));
        if resp.clicked() {
            if key == "ai" {
                page.icon = l::FILE_TEXT.into();
            } else {
                page.cover = Some((page.id as usize) % COVERS.len());
            }
            changed = true;
        }
    }

    ui.horizontal(|ui| {
        ui.add_space(gutter());
        let tid = Id::new(("title", page.id));
        let title_focused = ui.memory(|m| m.has_focus(tid));
        let enter = title_focused && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        // Phones: a smaller heading that WRAPS (a single 40 pt line is wider than the screen and could not be scrolled to the end).
        // The size also steps down for long titles. Desktop keeps the single 40 pt line.
        let phone = crate::touch::compact();
        let size = if phone {
            let n = page.title.chars().count();
            (if n > 48 { 22.0 } else if n > 26 { 26.0 } else { 30.0 }) * ed.scale.max(0.5)
        } else {
            40.0 * ed.scale.max(0.5)
        };
        let w = if phone { (content_w - 4.0).max(80.0) } else { content_w };
        let edit = if phone { TextEdit::multiline(&mut page.title).desired_rows(1) } else { TextEdit::singleline(&mut page.title) };
        let r = ui.add(
            edit.id(tid)
                .frame(false)
                .font(bold(size))
                .hint_text(RichText::new("Untitled").font(bold(size)).color(with_alpha(pal.muted, 0.55)))
                .desired_width(w)
                .margin(Margin::symmetric(0, 4)),
        );
        if phone && page.title.contains('\n') {
            // a title is one paragraph: pasted line breaks become spaces
            page.title = page.title.replace("\r\n", " ").replace('\n', " ");
            changed = true;
        }
        changed |= r.changed();
        changed |= widgets::field_menu(ui, pal, &r, &mut page.title);
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

/// Everything one render pass over the page's blocks shares (the blocks of a column are drawn by the same code as the page body).
struct Pass<'a> {
    ws: &'a mut Workspace,
    ed: &'a mut EditorState,
    pal: &'a Palette,
    links: &'a Links,
    heads: Vec<(u64, u8, String)>,
    visible: Vec<bool>,
    rects: Vec<(usize, Rect)>,
    /// Rectangles of the columns drawn this frame (index of the `Column` block).
    col_rects: Vec<(usize, Rect)>,
    nums: [usize; 8],
    act: Option<Act>,
    changed: bool,
    pointer: Option<egui::Pos2>,
}

/// Number of column layouts each block sits in (0 = the block is part of the page body).
fn col_depth(bl: &[Block]) -> Vec<u8> {
    let mut out = vec![0u8; bl.len()];
    let mut stack: Vec<u8> = vec![]; // indents of the open Columns blocks
    for (i, b) in bl.iter().enumerate() {
        while stack.last().map_or(false, |k| b.indent <= *k) {
            stack.pop();
        }
        out[i] = stack.len() as u8;
        if b.kind == BlockKind::Columns {
            stack.push(b.indent);
        }
    }
    out
}

fn blocks(ui: &mut Ui, page: &mut Page, ws: &mut Workspace, ed: &mut EditorState, pal: &Palette, w: f32, links: &Links, bg: &egui::Response) -> bool {
    let origin = ui.cursor().min;
    let n = page.blocks.len();
    let pointer = ui.ctx().pointer_hover_pos();
    let has_toc = page.blocks.iter().any(|b| b.kind == BlockKind::Toc);
    let heads: Vec<(u64, u8, String)> = page
        .blocks
        .iter()
        .filter(|_| has_toc)
        .filter_map(|b| match b.kind {
            BlockKind::H1 => Some((b.id, 1, rich::strip(&b.text))),
            BlockKind::H2 => Some((b.id, 2, rich::strip(&b.text))),
            BlockKind::H3 => Some((b.id, 3, rich::strip(&b.text))),
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

    let mut cx = Pass { ws, ed, pal, links, heads, visible, rects: Vec::with_capacity(n), col_rects: vec![], nums: [0; 8], act: None, changed: false, pointer };
    draw_range(ui, page, &mut cx, 0, n, 0, w);
    let Pass { ws, ed, rects, col_rects, mut act, mut changed, .. } = cx;

    // drag & drop reorder (children travel with their parent; blocks can be dropped into any column)
    if let Some(did) = ed.dragging {
        ui.ctx().request_repaint();
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        let from_pos = page.blocks.iter().position(|b| b.id == did);
        if let (Some(p), Some(from), false) = (pointer, from_pos, rects.is_empty()) {
            // long pages: dragging near the top / bottom edge scrolls (finger or mouse)
            {
                let clip = ui.clip_rect();
                let edge = if crate::touch::touchy() { 80.0 } else { 44.0 };
                if p.y < clip.top() + edge {
                    ui.scroll_with_delta(vec2(0.0, 5.0 + 15.0 * ((clip.top() + edge - p.y) / edge).clamp(0.0, 1.0)));
                } else if p.y > clip.bottom() - edge {
                    ui.scroll_with_delta(vec2(0.0, -5.0 - 15.0 * ((p.y - (clip.bottom() - edge)) / edge).clamp(0.0, 1.0)));
                }
            }
            let bl = &page.blocks;
            let depth = col_depth(bl);
            let from_end = subtree_end(bl, from);
            // (target index, line y, line x, line width, indent to adopt)
            let mut drop: Option<(usize, f32, f32, f32, Option<u8>)> = None;
            let into = col_rects.iter().find(|(c, r)| r.contains(p) && bl[from].kind != BlockKind::Columns && !(*c >= from && *c < from_end)).copied();
            if let Some((c, cr)) = into {
                let end = subtree_end(bl, c);
                let inside: Vec<&(usize, Rect)> = rects.iter().filter(|(i, _)| *i > c && *i < end).collect();
                let slot = inside.iter().find(|(_, r)| p.y < r.center().y);
                let (target, y, ind) = match slot {
                    Some((i, r)) => (*i, r.top() - 1.0, bl[*i].indent),
                    None => (end, inside.last().map_or(cr.top(), |(_, r)| r.bottom()) + 1.0, bl[c].indent + 1),
                };
                drop = Some((target, y, cr.left() + gutter(), (cr.width() - gutter()).max(40.0), Some(ind)));
            } else {
                let cands: Vec<&(usize, Rect)> = rects.iter().filter(|(i, _)| depth[*i] == 0).collect();
                if let (Some(first), Some(last)) = (cands.first(), cands.last()) {
                    let (target, y) = match cands.iter().find(|(_, r)| p.y < r.center().y) {
                        Some((i, r)) => (*i, r.top() - 1.0),
                        None => (n, last.1.bottom() + 1.0),
                    };
                    let ind = if target < n { bl[target].indent } else { 0 };
                    drop = Some((target, y, first.1.left() + gutter(), w, if depth[from] > 0 { Some(ind) } else { None }));
                }
            }
            if let Some((target, y, x0, lw, ind)) = drop {
                ui.painter().rect_filled(Rect::from_min_size(pos2(x0, y - 1.5), vec2(lw, 3.0)), 2.0, pal.accent);
                if !ui.input(|i| i.pointer.primary_down()) {
                    act = Some(match ind {
                        Some(ind) => Act::MoveAs(from, target, ind),
                        None => Act::MoveTo(from, target),
                    });
                    ed.dragging = None;
                }
            } else if !ui.input(|i| i.pointer.primary_down()) {
                ed.dragging = None;
            }
        } else if !ui.input(|i| i.pointer.primary_down()) {
            ed.dragging = None;
        }
    }

    if bg.clicked() {
        ed.active = None;
    }
    // click below the last block to append
    let (rest, _) = ui.allocate_exact_size(vec2(w + gutter(), 150.0), Sense::hover());
    let tail_hover = ui.rect_contains_pointer(rest);
    let t = ui.ctx().animate_bool_with_time(Id::new(("tail", page.id)), tail_hover, 0.15);
    ui.painter().text(pos2(rest.left() + gutter(), rest.top() + 16.0), Align2::LEFT_CENTER, "Click to add a block", body(14.0), with_alpha(pal.muted, t * 0.8));
    if bg.clicked() && tail_hover && n > 0 {
        let last = &page.blocks[n - 1];
        if last.kind == BlockKind::Text && last.text.is_empty() {
            ed.focus = Some((last.id, 0));
        } else {
            act = Some(Act::InsertAfter(n - 1, BlockKind::Text, String::new()));
        }
    }

    if multisel::handle(ui, page, &mut ws.next_id, ed, pal, bg, &rects, origin) {
        changed = true;
        act = None;
    }
    let mut from_bar = false;
    if act.is_none() {
        if let Some(a) = mobile_bar(ui, page, ed, pal) {
            act = Some(a);
            from_bar = true;
        }
    }
    if let Some(a) = act {
        if !matches!(a, Act::FocusPrev(_) | Act::FocusNext(_)) {
            multisel::push_undo(ed, page);
        }
        apply(page, a, &mut ws.next_id, ed);
        changed = true;
        if from_bar && ed.focus.is_none() {
            // the toolbar press took the focus away: give it back so the keyboard stays up
            if let Some(b) = ed.active {
                ed.focus = Some((b, usize::MAX));
            }
        }
        ui.ctx().request_repaint();
    }
    if let Some((fid, _)) = ed.focus {
        if !page.blocks.iter().any(|b| b.id == fid) {
            ed.focus = None;
        }
    }
    changed
}

/// Draws the blocks `from..to` of the page. `base` is the indent of the first nesting level (2 levels deeper inside every column).
#[allow(clippy::too_many_arguments)]
fn draw_range(ui: &mut Ui, page: &mut Page, cx: &mut Pass, from: usize, to: usize, base: u8, w: f32) {
    let pal = cx.pal;
    let n = page.blocks.len();
    let mut i = from;
    let mut drawn = false;
    while i < to {
        let cur = i;
        i += 1;
        let kind = page.blocks[cur].kind;
        let bid = page.blocks[cur].id;
        let indent = page.blocks[cur].indent.min(6);
        let ind = indent as usize;
        for d in cx.nums.iter_mut().skip(ind + 1) {
            *d = 0;
        }
        if kind == BlockKind::Numbered {
            cx.nums[ind] += 1;
        } else {
            cx.nums[ind] = 0;
        }
        let num = cx.nums[ind];
        if !cx.visible[cur] || kind == BlockKind::Column {
            continue;
        }
        if drawn {
            ui.add_space(match kind {
                BlockKind::H1 => 18.0,
                BlockKind::H2 => 12.0,
                BlockKind::H3 => 8.0,
                BlockKind::Callout | BlockKind::Code | BlockKind::Quote => 6.0,
                BlockKind::Columns | BlockKind::DbView | BlockKind::Widget => 8.0,
                _ => 1.0,
            });
        }
        drawn = true;
        if kind == BlockKind::Columns {
            let end = subtree_end(&page.blocks, cur).min(to);
            draw_columns(ui, page, cx, cur, end, base, w);
            i = end.max(i);
            continue;
        }
        let ind_px = indent.saturating_sub(base) as f32 * INDENT;
        // Performance: blocks far outside the viewport are not laid out at all — we just reserve their last known height.
        let key = {
            use std::hash::{Hash, Hasher};
            let b = &page.blocks[cur];
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (b.kind as u8, &b.text, b.indent, b.collapsed, b.checked, w.to_bits(), cx.ed.scale.to_bits(), cx.heads.len(), b.bg, b.size, b.url.len(), b.media).hash(&mut h);
            h.finish()
        };
        if cx.ed.dragging.is_none()
            && cx.ed.focus.map_or(true, |(f, _)| f != bid)
            && cx.ed.restore_sel.map_or(true, |r| r.0 != bid)
            && cx.ed.scroll_to != Some(bid)
            && kind != BlockKind::DbView
            && !ui.ctx().memory(|m| m.has_focus(Id::new(("blk", bid))))
        {
            if let Some(&(h, k)) = cx.ed.heights.get(&bid) {
                let clip = ui.clip_rect();
                let top = ui.cursor().top();
                if k == key && (top + h < clip.top() - 800.0 || top > clip.bottom() + 800.0) {
                    let (r, _) = ui.allocate_exact_size(vec2(w + gutter(), h), Sense::hover());
                    cx.rects.push((cur, r));
                    continue;
                }
            }
        }
        let bg_slot = ui.painter().add(Shape::Noop);
        let row = ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            ui.add_space(gutter() + ind_px);
            ui.vertical(|ui| {
                ui.set_width(w - ind_px);
                if kind == BlockKind::DbView {
                    cx.changed |= draw_dbview(ui, cx.ws, &mut page.blocks[cur], cx.ed, pal, w - ind_px);
                } else if kind == BlockKind::Widget {
                    let o = crate::inlinew::draw(ui, cx.ws, pal, &mut page.blocks[cur], w - ind_px);
                    cx.changed |= o.changed;
                    if o.remove {
                        cx.act = Some(Act::Remove(cur));
                    }
                    if let Some(op) = o.nav.out.open {
                        cx.ed.nav = Some(op);
                    }
                    if let Some(t) = o.nav.toast {
                        crate::dashcfg::set_toast(&t);
                    }
                } else {
                    cx.changed |= draw_block(ui, &mut page.blocks[cur], cur, n, num, cx.ed, pal, w - ind_px, &mut cx.act, &cx.heads, cx.links);
                }
            });
        });
        let rect = row.response.rect;
        if kind.has_text() && ui.ctx().memory(|m| m.has_focus(Id::new(("blk", bid)))) {
            cx.ed.active = Some(bid);
        }
        cx.rects.push((cur, rect));
        cx.ed.heights.insert(bid, (rect.height(), key));
        if !cx.ed.rc_frame {
            if let Some(p) = right_clicked(ui, rect) {
                cx.ed.menu = Some(EdMenu::Block { bid, pos: p });
                cx.ed.menu_fresh = true;
                cx.ed.rc_frame = true;
                cx.ed.block_menu = None;
            }
        }
        let bgc = page.blocks[cur].bg;
        if bgc > 0 && !matches!(kind, BlockKind::Divider | BlockKind::Callout | BlockKind::Code) {
            let r = Rect::from_min_max(pos2(rect.left() + gutter() + ind_px - 6.0, rect.top()), pos2(rect.right() + 4.0, rect.bottom()));
            ui.painter().set(bg_slot, Shape::rect_filled(r, 4.0, tag_color(bgc as usize - 1).gamma_multiply(0.16)));
        }
        if cx.ed.scroll_to == Some(bid) {
            ui.scroll_to_rect(rect, Some(Align::TOP));
            cx.ed.scroll_to = None;
        }

        // gutter controls
        let hover_zone = Rect::from_min_max(pos2(rect.left(), rect.top() - 1.0), pos2(rect.right(), rect.bottom() + 1.0));
        let hovering = cx.pointer.map_or(false, |p| hover_zone.contains(p)) && cx.ed.dragging.is_none()
            || cx.ed.dragging == Some(bid)
            || cx.ed.block_menu.map_or(false, |(b, _)| b == bid)
            || (crate::touch::touchy() && cx.ed.active == Some(bid) && cx.ed.sel.is_empty());
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
            let tiny = crate::touch::compact();
            let (plus, grip) = if tiny {
                // phones: one big handle (the "+" lives in the toolbar above the keyboard)
                (Rect::from_min_size(pos2(rect.left() + 1.0, y - 5.0), vec2(0.0, 0.0)), Rect::from_min_size(pos2(rect.left() + 1.0, y - 5.0), vec2(34.0, 34.0)))
            } else if crate::touch::touchy() {
                (Rect::from_min_size(pos2(rect.left() + 3.0, y - 4.0), vec2(26.0, 32.0)), Rect::from_min_size(pos2(rect.left() + 29.0, y - 4.0), vec2(26.0, 32.0)))
            } else {
                (Rect::from_min_size(pos2(rect.left() + 6.0, y), vec2(22.0, 24.0)), Rect::from_min_size(pos2(rect.left() + 29.0, y), vec2(20.0, 24.0)))
            };
            let pr = ui.interact(plus, Id::new(("plus", bid)), Sense::click());
            let gr = ui.interact(grip, Id::new(("grip", bid)), Sense::click_and_drag());
            crate::touch::claim(grip);
            for (r, resp, icon) in [(plus, &pr, Icon::Plus), (grip, &gr, Icon::Grip)] {
                if tiny && icon == Icon::Plus {
                    continue;
                }
                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() || resp.is_pointer_button_down_on(), 0.1);
                ui.painter().rect_filled(r, 5.0, with_alpha(pal.hover, h * t));
                icons::paint(ui.painter(), r.center(), if tiny { 18.0 } else { 15.0 }, icon, with_alpha(lerp_color(pal.muted, pal.text, h), t));
            }
            if !tiny && pr.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Click to add a block below").clicked() {
                cx.act = Some(Act::InsertAfter(cur, BlockKind::Text, "/".into()));
            }
            if gr.drag_started() {
                cx.ed.dragging = Some(bid);
            }
            if gr.clicked() {
                if ui.input(|i| i.modifiers.command || i.modifiers.shift) {
                    // Ctrl/Shift + click on the handle adds the block to the multi-selection
                    if let Some(pos) = cx.ed.sel.iter().position(|x| *x == bid) {
                        cx.ed.sel.remove(pos);
                    } else {
                        cx.ed.sel.push(bid);
                    }
                } else {
                    cx.ed.block_menu = Some((bid, grip.left_bottom() + vec2(-4.0, 6.0)));
                    cx.ed.fresh = true;
                }
            }
            if gr.hovered() || gr.dragged() {
                ui.ctx().set_cursor_icon(if gr.dragged() { CursorIcon::Grabbing } else { CursorIcon::Grab });
            }
        }
    }
}

/// Where `MoveTo` must put block `i` to swap it with its previous / next sibling (children travel with their parent).
fn sibling_target(bl: &[Block], i: usize, up: bool) -> Option<usize> {
    let ind = bl[i].indent;
    if matches!(bl[i].kind, BlockKind::Column | BlockKind::Columns) {
        return None;
    }
    if up {
        let mut j = i;
        while j > 0 {
            j -= 1;
            if bl[j].indent < ind {
                return None;
            }
            if bl[j].indent == ind {
                return Some(j);
            }
        }
        None
    } else {
        let end = subtree_end(bl, i);
        if end < bl.len() && bl[end].indent == ind && !matches!(bl[end].kind, BlockKind::Column) {
            Some(subtree_end(bl, end))
        } else {
            None
        }
    }
}

/// Phone / tablet editing toolbar: a strip above the keyboard (add, block type, indent, move, duplicate, undo, delete, hide keyboard)
/// plus the block-type bottom sheet. It scrolls sideways, so it can never run off a narrow screen.
fn mobile_bar(ui: &mut Ui, page: &mut Page, ed: &mut EditorState, pal: &Palette) -> Option<Act> {
    if !crate::touch::touchy() {
        return None;
    }
    let ctx = ui.ctx().clone();
    let bid = ed.active?;
    if !ed.sel.is_empty() || ed.marquee.is_some() || ed.dragging.is_some() || ed.menu.is_some() || ed.block_menu.is_some() {
        return None;
    }
    let idx = page.blocks.iter().position(|b| b.id == bid)?;
    let now = ctx.input(|i| i.time);
    let focused = ctx.memory(|m| m.focused().is_some());
    if focused {
        ed.bar_grace = now + 0.6;
    }
    if !(focused || ed.tool_menu != 0 || now < ed.bar_grace) {
        return None;
    }
    if !focused && ed.tool_menu == 0 {
        ctx.request_repaint_after(std::time::Duration::from_millis(650));
    }
    let screen = ctx.screen_rect();
    let bar_h = 48.0;
    let mut out: Option<Act> = None;
    let mut dismiss = false;
    let (can_up, can_down) = (sibling_target(&page.blocks, idx, true).is_some(), sibling_target(&page.blocks, idx, false).is_some());
    let can_undo = ed.undo.iter().any(|(p, _)| *p == page.id);
    let mut menu = ed.tool_menu;

    let bar = egui::Area::new(Id::new("mobile_bar")).order(Order::Foreground).fixed_pos(pos2(screen.left(), screen.bottom() - bar_h)).show(&ctx, |ui| {
        Frame::new().fill(pal.popup).stroke(Stroke::new(1.0_f32, pal.border)).show(ui, |ui| {
            ui.set_width(screen.width() - 2.0);
            ui.set_height(bar_h - 2.0);
            ScrollArea::horizontal().id_salt("mobile_bar_scroll").scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
                    ui.add_space(4.0);
                    if widgets::icon_button(ui, pal, Icon::Plus, 18.0, "Insert a block below").clicked() {
                        menu = if menu == 2 { 0 } else { 2 };
                    }
                    if widgets::icon_button(ui, pal, Icon::Type, 18.0, "Turn into").clicked() {
                        menu = if menu == 1 { 0 } else { 1 };
                    }
                    ui.add_space(6.0);
                    if widgets::icon_button(ui, pal, Icon::Outdent, 18.0, "Outdent").clicked() {
                        out = Some(Act::Indent(idx, -1));
                    }
                    if widgets::icon_button(ui, pal, Icon::Indent, 18.0, "Indent").clicked() {
                        out = Some(Act::Indent(idx, 1));
                    }
                    ui.add_space(6.0);
                    if ui.add_enabled_ui(can_up, |ui| widgets::icon_button(ui, pal, Icon::MoveUp, 18.0, "Move up")).inner.clicked() {
                        out = sibling_target(&page.blocks, idx, true).map(|t| Act::MoveTo(idx, t));
                    }
                    if ui.add_enabled_ui(can_down, |ui| widgets::icon_button(ui, pal, Icon::MoveDown, 18.0, "Move down")).inner.clicked() {
                        out = sibling_target(&page.blocks, idx, false).map(|t| Act::MoveTo(idx, t));
                    }
                    ui.add_space(6.0);
                    if widgets::icon_button(ui, pal, Icon::Duplicate, 18.0, "Duplicate").clicked() {
                        out = Some(Act::Dup(idx));
                    }
                    if ui.add_enabled_ui(can_undo, |ui| widgets::icon_button(ui, pal, Icon::Undo, 18.0, "Undo")).inner.clicked() {
                        ed.want_undo = true;
                    }
                    if widgets::icon_button(ui, pal, Icon::Trash, 18.0, "Delete block").clicked() {
                        out = Some(Act::Remove(idx));
                    }
                    ui.add_space(6.0);
                    if widgets::icon_button(ui, pal, Icon::ChevronDown, 18.0, "Hide keyboard").clicked() {
                        dismiss = true;
                    }
                    ui.add_space(4.0);
                });
            });
        });
    });
    ctx.raise_layer(bar.response.layer_id);
    let mut covered = bar.response.rect;

    if menu != 0 {
        let cur_kind = page.blocks[idx].kind;
        let w = (screen.width() - 16.0).min(520.0);
        let max_h = (screen.height() * 0.5).clamp(160.0, 380.0);
        let sheet = egui::Area::new(Id::new("mobile_sheet")).order(Order::Foreground).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -(bar_h + 6.0))).show(&ctx, |ui| {
            widgets::popup_frame(pal).show(ui, |ui| {
                ui.set_width(w - 12.0);
                ui.horizontal(|ui| {
                    for (m, label) in [(1u8, "Turn into"), (2u8, "Insert below")] {
                        let on = menu == m;
                        let (r, resp) = ui.allocate_exact_size(vec2((w - 16.0) / 2.0, 36.0), Sense::click());
                        ui.painter().rect_filled(r, 8.0, if on { pal.active } else { pal.hover.gamma_multiply(0.0) });
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, label, bold(13.5), if on { pal.text } else { pal.muted });
                        if resp.clicked() {
                            menu = m;
                        }
                    }
                });
                ui.add_space(4.0);
                ScrollArea::vertical().id_salt("mobile_sheet_scroll").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                    if menu == 1 {
                        for k in BlockKind::TEXTY.iter().copied().chain(std::iter::once(BlockKind::Divider)) {
                            if menu_item(ui, pal, k.glyph(), k.label(), None, k == cur_kind, false).clicked() {
                                out = Some(Act::Convert(idx, k, false));
                                menu = 0;
                            }
                        }
                    } else {
                        let mut sec = "";
                        for it in slash_all() {
                            if it.sec != sec {
                                sec = it.sec;
                                section_label(ui, pal, sec);
                            }
                            if menu_item(ui, pal, it.glyph, it.label, None, false, false).clicked() {
                                out = Some(Act::InsertSlash(idx, it));
                                menu = 0;
                            }
                        }
                    }
                });
            });
        });
        ctx.raise_layer(sheet.response.layer_id);
        covered = covered.union(sheet.response.rect);
        // a tap outside the sheet and the toolbar closes it
        if ctx.input(|i| i.pointer.any_pressed()) {
            if let Some(p) = ctx.pointer_interact_pos() {
                if !covered.contains(p) {
                    menu = 0;
                }
            }
        }
    }
    ed.tool_menu = menu;
    if dismiss {
        ed.tool_menu = 0;
        ed.active = None;
        ctx.memory_mut(|m| {
            if let Some(f) = m.focused() {
                m.surrender_focus(f);
            }
        });
    }
    out
}

/// Draws one column layout (the `Columns` block `i` and its `Column` children up to `end`) side by side.
/// Every column owns a gutter on its left, so each block keeps its "+" / drag handle; columns stack on narrow screens.
fn draw_columns(ui: &mut Ui, page: &mut Page, cx: &mut Pass, i: usize, end: usize, base: u8, w: f32) {
    let pal = cx.pal;
    let k = page.blocks[i].indent;
    let ind_px = k.saturating_sub(base) as f32 * INDENT;
    let total = w - ind_px + gutter();
    let mut cols: Vec<(usize, usize)> = vec![];
    let mut j = i + 1;
    while j < end {
        if page.blocks[j].kind == BlockKind::Column && page.blocks[j].indent == k + 1 {
            let e = subtree_end(&page.blocks, j).min(end);
            cols.push((j, e));
            j = e;
        } else {
            j += 1;
        }
    }
    let m = cols.len();
    if m == 0 {
        return;
    }
    let cw_all = total - gutter() * m as f32;
    let stacked = (cw_all / m as f32) < 150.0;
    // widths (permille weights; 0 = equal share)
    let wsum: f32 = cols.iter().map(|(c, _)| if page.blocks[*c].start == 0 { 1000.0 / m as f32 } else { page.blocks[*c].start as f32 }).sum::<f32>().max(1.0);
    let widths: Vec<f32> = cols.iter().map(|(c, _)| (if page.blocks[*c].start == 0 { 1000.0 / m as f32 } else { page.blocks[*c].start as f32 }) / wsum * cw_all).collect();
    let first_col_idx = cx.col_rects.len();
    let mut rights: Vec<f32> = vec![];
    let row = ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        ui.add_space(ind_px);
        if stacked {
            ui.vertical(|ui| {
                ui.set_width(total - ind_px);
                for (c, e) in cols.iter() {
                    draw_range(ui, page, cx, *c + 1, *e, k + 2, w - ind_px);
                    ui.add_space(6.0);
                }
            });
        } else {
            for (idx, (c, e)) in cols.iter().enumerate() {
                let cw = widths[idx];
                let r = ui.vertical(|ui| {
                    ui.set_width(gutter() + cw);
                    draw_range(ui, page, cx, *c + 1, *e, k + 2, cw);
                });
                let mut rr = r.response.rect;
                rr.set_width(gutter() + cw);
                cx.col_rects.push((*c, rr));
                rights.push(rr.right());
            }
        }
    });
    let rect = row.response.rect;
    let bid = page.blocks[i].id;

    // drag the divider between two columns to resize them
    if !stacked && cx.ed.dragging.is_none() {
        for idx in 1..m {
            let x = rights[idx - 1];
            let hr = Rect::from_min_max(pos2(x - 2.0, rect.top()), pos2(x + 8.0, rect.bottom()));
            let resp = ui.interact(hr, Id::new(("colsz", page.blocks[cols[idx].0].id)), Sense::drag());
            let hot = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() || resp.dragged(), 0.1);
            if hot > 0.01 {
                ui.painter().rect_filled(Rect::from_center_size(pos2(x + 3.0, rect.center().y), vec2(3.0, (rect.height() - 4.0).max(12.0))), 2.0, with_alpha(pal.accent, hot * 0.8));
            }
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            if resp.dragged() {
                let dx = resp.drag_delta().x.clamp(-(widths[idx - 1] - 90.0).max(0.0), (widths[idx] - 90.0).max(0.0));
                let mut nw = widths.clone();
                nw[idx - 1] += dx;
                nw[idx] -= dx;
                let s: f32 = nw.iter().sum::<f32>().max(1.0);
                for (q, (c, _)) in cols.iter().enumerate() {
                    page.blocks[*c].start = (nw[q] / s * 1000.0).round().max(1.0) as u32;
                }
                cx.changed = true;
            }
            if resp.double_clicked() {
                cx.act = Some(Act::ColEqual(i));
            }
        }
    }

    // hover toolbar: add / remove a column, equal widths, unwrap, delete layout
    let zone = Rect::from_min_max(pos2(rect.left(), rect.top() - 26.0), pos2(rect.right(), rect.bottom()));
    let hot = cx.pointer.map_or(false, |p| zone.contains(p)) && cx.ed.dragging.is_none() && cx.ed.marquee.is_none();
    let t = ui.ctx().animate_bool_with_time(Id::new(("colbar", bid)), hot, 0.12);
    if t > 0.02 {
        let hovered_col = cx.col_rects[first_col_idx..].iter().find(|(_, r)| cx.pointer.map_or(false, |p| r.x_range().contains(p.x))).map(|(c, _)| *c).unwrap_or(cols[m - 1].0);
        let items: [(&str, &str, Option<Act>); 5] = [
            (l::PLUS, if m < 6 { "Add a column after this one" } else { "Up to 6 columns" }, if m < 6 { Some(Act::ColAdd(hovered_col)) } else { None }),
            (l::MINUS, "Remove this column and its content", Some(Act::ColDel(hovered_col))),
            (l::BETWEEN_HORIZONTAL_START, "Equal column widths (or double-click a divider)", Some(Act::ColEqual(i))),
            (l::LAYOUT_LIST, "Unwrap — stack the columns, keep the content", Some(Act::ColUnwrap(i))),
            (l::TRASH_2, "Delete the whole layout", Some(Act::ColDelete(i))),
        ];
        let bw = 24.0;
        let x0 = rect.right() - bw * items.len() as f32 - 4.0;
        let y0 = rect.top() - 24.0;
        ui.painter().rect_filled(Rect::from_min_size(pos2(x0 - 3.0, y0 - 1.0), vec2(bw * items.len() as f32 + 6.0, 26.0)), 8.0, with_alpha(pal.popup, t));
        ui.painter().rect_stroke(Rect::from_min_size(pos2(x0 - 3.0, y0 - 1.0), vec2(bw * items.len() as f32 + 6.0, 26.0)), 8.0, Stroke::new(1.0, with_alpha(pal.border, t)), egui::StrokeKind::Inside);
        for (q, (glyph, tip, a)) in items.into_iter().enumerate() {
            let r = Rect::from_min_size(pos2(x0 + bw * q as f32, y0), vec2(bw, 24.0));
            let resp = ui.interact(r, Id::new(("colbtn", bid, q)), Sense::click());
            let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() && a.is_some(), 0.1);
            ui.painter().rect_filled(r.shrink(1.0), 6.0, with_alpha(pal.hover, h * t));
            let col = if q == 4 && h > 0.0 { pal.danger } else if a.is_some() { pal.text } else { pal.muted };
            ui.painter().text(r.center(), Align2::CENTER_CENTER, glyph, body(14.0), with_alpha(col, t));
            if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip).clicked() {
                if let Some(a) = a {
                    cx.act = Some(a);
                }
            }
        }
    }
}

/// A linked view of a database: the source stays unchanged, this block has its own layout, filters and sorts.
fn draw_dbview(ui: &mut Ui, ws: &mut Workspace, b: &mut Block, ed: &mut EditorState, pal: &Palette, w: f32) -> bool {
    let mut changed = false;
    let src = b.link.filter(|l| ws.page(*l).map_or(false, |p| p.db.is_some() && !p.trashed));
    let Some(db_id) = src else {
        // pick the source database
        egui::Frame::new().fill(with_alpha(pal.hover, 0.5)).stroke(Stroke::new(1.0, pal.border)).corner_radius(8.0).inner_margin(Margin::same(12)).show(ui, |ui| {
            ui.set_width(w - 26.0);
            ui.label(RichText::new(format!("{}  Linked view of a database", l::TABLE_2)).font(bold(15.0)).color(pal.text));
            ui.label(RichText::new(if b.link.is_some() { "The source database was deleted. Pick another one:" } else { "Pick a database. This block shows its own filtered view — the original stays untouched." }).font(body(13.0)).color(pal.muted));
            ui.add_space(6.0);
            let dbs: Vec<(u64, String)> = ws.pages.iter().filter(|p| p.db.is_some() && !p.trashed && !p.row).map(|p| (p.id, ws.display_title(p.id))).collect();
            if dbs.is_empty() {
                ui.label(RichText::new("No databases yet — create one with /table database or from the sidebar.").font(body(13.0)).color(pal.muted));
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                for (id, t) in dbs {
                    if database::pill(ui, pal, &format!("{}  {}", l::DATABASE, t), false, false).clicked() {
                        b.link = Some(id);
                        changed = true;
                    }
                }
            });
        });
        return changed;
    };
    let title = ws.display_title(db_id);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 0.0);
        let r = ui.add(egui::Label::new(RichText::new(format!("{}  {}", l::DATABASE, title)).font(bold(15.0)).color(pal.text)).sense(Sense::click()));
        if r.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Open the source database").clicked() {
            ed.nav = Some(db_id);
        }
        ui.label(RichText::new("linked view").font(body(11.5)).color(pal.muted));
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            if crate::widgets::icon_button(ui, pal, Icon::Close, 11.0, "Choose another database").clicked() {
                b.link = None;
                changed = true;
            }
        });
    });
    ui.add_space(2.0);
    if b.link.is_none() {
        return changed;
    }
    let Some(vi) = ws.ensure_linked_view(db_id, b.id, ViewKind::Table) else { return changed };
    let mut st = ed.dbv.remove(&b.id).unwrap_or_default();
    std::mem::swap(&mut ed.db, &mut st);
    changed |= database::show_linked(ui, ws, db_id, vi, b.id, ed, pal, w);
    std::mem::swap(&mut ed.db, &mut st);
    ed.dbv.insert(b.id, st);
    changed
}

/// Repairs the column structure after any edit: a `Columns` block holds `Column` blocks (one level deeper), every column holds at
/// least one block, a layout with fewer than two columns dissolves, and the page never ends inside a layout (so you can keep typing).
fn normalize_layout(bl: &mut Vec<Block>, next_id: &mut u64) {
    if !bl.iter().any(|b| matches!(b.kind, BlockKind::Columns | BlockKind::Column)) {
        return;
    }
    let mut guard = 0;
    'again: loop {
        guard += 1;
        if guard > 500 {
            break;
        }
        for i in 0..bl.len() {
            match bl[i].kind {
                BlockKind::Column => {
                    let ok = bl[i].indent > 0 && (0..i).rev().find(|j| bl[*j].indent < bl[i].indent).map_or(false, |j| bl[j].kind == BlockKind::Columns && bl[j].indent + 1 == bl[i].indent);
                    if !ok {
                        bl[i].kind = BlockKind::Text;
                        bl[i].text.clear();
                        bl[i].start = 0;
                        continue 'again;
                    }
                }
                BlockKind::Columns => {
                    let k = bl[i].indent;
                    if k > 4 {
                        bl[i].kind = BlockKind::Text;
                        bl[i].text.clear();
                        continue 'again;
                    }
                    let end = subtree_end(bl, i);
                    if end > i + 1 && bl[i + 1].indent > k + 1 {
                        let c = new_block(next_id, BlockKind::Column, String::new(), k + 1);
                        bl.insert(i + 1, c);
                        continue 'again;
                    }
                    for j in i + 1..end {
                        if bl[j].indent == k + 1 && bl[j].kind != BlockKind::Column {
                            let prev_is_col = (i + 1..j).rev().find(|q| bl[*q].indent == k + 1).map_or(false, |q| bl[q].kind == BlockKind::Column);
                            if prev_is_col {
                                let e = subtree_end(bl, j);
                                for b in bl[j..e].iter_mut() {
                                    b.indent = (b.indent + 1).min(6);
                                }
                            } else {
                                let c = new_block(next_id, BlockKind::Column, String::new(), k + 1);
                                bl.insert(j, c);
                            }
                            continue 'again;
                        }
                    }
                    let cols: Vec<usize> = (i + 1..end).filter(|q| bl[*q].indent == k + 1 && bl[*q].kind == BlockKind::Column).collect();
                    if cols.len() < 2 {
                        dissolve(bl, i);
                        continue 'again;
                    }
                    for c in cols {
                        if subtree_end(bl, c) == c + 1 {
                            let t = new_block(next_id, BlockKind::Text, String::new(), k + 2);
                            bl.insert(c + 1, t);
                            continue 'again;
                        }
                    }
                }
                _ => {}
            }
        }
        // the page must not end inside a layout
        if let Some(r) = (0..bl.len()).rev().find(|i| bl[*i].kind == BlockKind::Columns && subtree_end(bl, *i) == bl.len()) {
            let t = new_block(next_id, BlockKind::Text, String::new(), bl[r].indent);
            bl.push(t);
        }
        break;
    }
    if bl.is_empty() {
        bl.push(new_block(next_id, BlockKind::Text, String::new(), 0));
    }
}

/// Removes the `Columns` block `i` and its `Column` markers; the content stays, two levels shallower.
fn dissolve(bl: &mut Vec<Block>, i: usize) {
    let k = bl[i].indent;
    let end = subtree_end(bl, i);
    let mut keep: Vec<Block> = vec![];
    for b in bl[i + 1..end].iter() {
        if b.indent == k + 1 && b.kind == BlockKind::Column {
            continue;
        }
        let mut b = b.clone();
        b.indent = b.indent.saturating_sub(2);
        keep.push(b);
    }
    bl.splice(i..end, keep);
}

fn new_block(next_id: &mut u64, kind: BlockKind, text: String, indent: u8) -> Block {
    *next_id += 1;
    Block::new(*next_id, kind, &text, indent)
}

fn apply(page: &mut Page, act: Act, next_id: &mut u64, ed: &mut EditorState) {
    let bl = &mut page.blocks;
    match act {
        Act::Split(i, at) => {
            let kind = bl[i].kind;
            let indent = bl[i].indent;
            let (left, rightp) = Rich::parse(&bl[i].text).split(at);
            bl[i].text = left.markup();
            let right = rightp.markup();
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
            if matches!(bl[i - 1].kind, BlockKind::Column | BlockKind::Columns) || col_depth(bl)[i - 1] != col_depth(bl)[i] {
                // never merge across a column boundary
                ed.focus = Some((bl[i].id, 0));
                return;
            }
            if !bl[i - 1].kind.has_text() {
                bl.remove(i - 1);
                ed.focus = Some((bl[i - 1].id, 0));
            } else {
                let b = bl.remove(i);
                let prev = &mut bl[i - 1];
                let mut joined = Rich::parse(&prev.text);
                let pos = joined.len();
                joined.append(&Rich::parse(&b.text));
                prev.text = joined.markup();
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
                if kind == BlockKind::Button {
                    bl[i].text = "New button".into();
                    ed.edit_button = Some(bl[i].id);
                }
                let media_kind = matches!(kind, BlockKind::Image | BlockKind::Embed | BlockKind::Bookmark | BlockKind::File);
                if kind != BlockKind::PageLink && (!media_kind || i + 1 >= bl.len()) {
                    let ind = bl[i].indent;
                    let nb = new_block(next_id, BlockKind::Text, String::new(), ind);
                    if !media_kind {
                        ed.focus = Some((nb.id, 0));
                    }
                    bl.insert(i + 1, nb);
                }
            } else {
                ed.focus = Some((bl[i].id, usize::MAX));
            }
        }
        Act::Strip(i, kind, n) => {
            let whole = Rich::parse(&bl[i].text);
            bl[i].text = whole.slice(n, whole.len()).markup();
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
        Act::MoveAs(from, to, ind) => {
            let end = subtree_end(bl, from);
            if to >= from && to <= end {
                return;
            }
            let moved: Vec<Block> = bl.drain(from..end).collect();
            let len = moved.len();
            let to = if to > from { to - len } else { to };
            let to = to.min(bl.len());
            let delta = ind as i32 - moved[0].indent as i32;
            for (k, mut b) in moved.into_iter().enumerate() {
                b.indent = (b.indent as i32 + delta).clamp(0, 6) as u8;
                bl.insert(to + k, b);
            }
        }
        Act::ColAdd(c) => {
            let k = bl[c].indent;
            let end = subtree_end(bl, c);
            let col = new_block(next_id, BlockKind::Column, String::new(), k);
            let txt = new_block(next_id, BlockKind::Text, String::new(), k + 1);
            ed.focus = Some((txt.id, 0));
            bl.insert(end, txt);
            bl.insert(end, col);
            if let Some(p) = (0..c).rev().find(|j| bl[*j].kind == BlockKind::Columns && bl[*j].indent + 1 == k) {
                let pe = subtree_end(bl, p);
                for b in bl[p + 1..pe].iter_mut().filter(|b| b.kind == BlockKind::Column && b.indent == k) {
                    b.start = 0;
                }
            }
        }
        Act::ColDel(c) => {
            let end = subtree_end(bl, c);
            bl.drain(c..end);
        }
        Act::ColDelete(i) => {
            let end = subtree_end(bl, i);
            bl.drain(i..end);
            if bl.is_empty() {
                bl.push(new_block(next_id, BlockKind::Text, String::new(), 0));
            }
        }
        Act::ColEqual(i) => {
            let end = subtree_end(bl, i);
            let k = bl[i].indent;
            for b in bl[i + 1..end].iter_mut().filter(|b| b.kind == BlockKind::Column && b.indent == k + 1) {
                b.start = 0;
            }
        }
        Act::ColUnwrap(i) => dissolve(bl, i),
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
        Act::InsertSlash(i, item) => {
            let ind = bl[i].indent;
            let nb = new_block(next_id, BlockKind::Text, String::new(), ind);
            bl.insert(i + 1, nb);
            apply(page, slash_act(i + 1, item), next_id, ed);
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
            if matches!(bl[i].kind, BlockKind::Column | BlockKind::Columns) {
                return;
            }
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
        Act::UrlBlock(i) => {
            let url = Rich::parse(&bl[i].text).plain.trim().to_owned();
            let kind = if media::is_image_name(&url) {
                BlockKind::Image
            } else if media::embed_info(&url).service != "Website" {
                BlockKind::Embed
            } else {
                BlockKind::Bookmark
            };
            bl[i].kind = kind;
            bl[i].url = url;
            bl[i].text.clear();
            if i + 1 >= bl.len() {
                let ind = bl[i].indent;
                let nb = new_block(next_id, BlockKind::Text, String::new(), ind);
                ed.focus = Some((nb.id, 0));
                bl.push(nb);
            }
        }
        Act::PasteBlocks(i, blocks) => {
            if blocks.is_empty() {
                return;
            }
            let base = bl[i].indent;
            let replace = bl[i].kind.has_text() && bl[i].text.is_empty();
            let mut new = blocks;
            for nb in new.iter_mut() {
                *next_id += 1;
                nb.id = *next_id;
                nb.indent = (nb.indent + base).min(6);
            }
            let focus_last = new.last().filter(|b| b.kind.has_text()).map(|b| b.id);
            let at = if replace {
                bl.remove(i);
                i
            } else {
                subtree_end(bl, i)
            };
            for (k, nb) in new.into_iter().enumerate() {
                bl.insert(at + k, nb);
            }
            if let Some(id) = focus_last {
                ed.focus = Some((id, usize::MAX));
            }
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
            Slash::Columns(n) => {
                let ind = bl[i].indent.min(4);
                bl[i].indent = ind;
                bl[i].kind = BlockKind::Columns;
                bl[i].text.clear();
                let mut first = 0;
                let mut ins: Vec<Block> = vec![];
                for q in 0..n.clamp(2, 6) {
                    ins.push(new_block(next_id, BlockKind::Column, String::new(), ind + 1));
                    let t = new_block(next_id, BlockKind::Text, String::new(), ind + 2);
                    if q == 0 {
                        first = t.id;
                    }
                    ins.push(t);
                }
                ed.focus = Some((first, 0));
                bl.splice(i + 1..i + 1, ins);
            }
            Slash::DbView => {
                bl[i].kind = BlockKind::DbView;
                bl[i].text.clear();
                bl[i].link = None;
                if i + 1 >= bl.len() {
                    let ind = bl[i].indent;
                    let nb = new_block(next_id, BlockKind::Text, String::new(), ind);
                    bl.push(nb);
                }
            }
            Slash::Widget => {
                bl[i].kind = BlockKind::Widget;
                bl[i].text.clear();
                bl[i].media = 0;
                ed.want_widget = Some(bl[i].id);
                if i + 1 >= bl.len() {
                    let ind = bl[i].indent;
                    let nb = new_block(next_id, BlockKind::Text, String::new(), ind);
                    bl.push(nb);
                }
            }
            Slash::Dashboard => {
                bl[i].kind = BlockKind::Text;
                bl[i].text.clear();
                ed.want_dash = true;
                ed.focus = Some((bl[i].id, usize::MAX));
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
    // content size: every fixed dimension of a block (bullets, check boxes, icons) follows the text
    let s = ed.scale.max(0.5);
    let base = if b.color > 0 { tag_color(b.color as usize - 1) } else { pal.text };
    match b.kind {
        BlockKind::Button => {
            changed |= crate::autoui::button_block(ui, b, ed, pal, w);
        }
        BlockKind::Image | BlockKind::Embed | BlockKind::Bookmark | BlockKind::File => {
            let pid = ed.cur_page;
            changed |= mediablocks::draw(ui, b, pid, ed, pal, w);
        }
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
                let (r, resp) = ui.allocate_exact_size(vec2(28.0 * s, 26.0 * s), Sense::click());
                let c = pos2(r.left() + 10.0 * s, r.top() + 13.5 * s);
                let t = ui.ctx().animate_bool_with_time(Id::new(("chk", b.id)), b.checked, 0.18);
                let box_r = Rect::from_center_size(c, vec2(17.0 * s, 17.0 * s));
                ui.painter().rect_filled(box_r, 4.0 * s, with_alpha(pal.accent, t));
                ui.painter().rect_stroke(box_r, 4.0 * s, Stroke::new(1.6_f32, lerp_color(pal.text.gamma_multiply(0.8), pal.accent, t)), egui::StrokeKind::Inside);
                if t > 0.02 {
                    let s = t.min(1.0);
                    let p1 = pos2(c.x - 4.2 * s, c.y + 0.2 * s);
                    let p2 = pos2(c.x - 1.2 * s, c.y + 3.2 * s);
                    let p3 = pos2(c.x + 4.4 * s, c.y - 3.2 * s);
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
                changed |= text_edit(ui, b, idx, n, ed, pal, Tx { font, color: col, hint: "To-do", width: w - 28.0 * s }, act);
            });
        }
        BlockKind::Bullet | BlockKind::Numbered => {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let (r, _) = ui.allocate_exact_size(vec2(26.0 * s, 26.0 * s), Sense::hover());
                if b.kind == BlockKind::Bullet {
                    let c = pos2(r.left() + 10.0 * s, r.top() + 13.5 * s);
                    match b.indent % 3 {
                        0 => ui.painter().circle_filled(c, 3.0 * s, base),
                        1 => ui.painter().circle_stroke(c, 3.0 * s, Stroke::new(1.3_f32, base)),
                        _ => ui.painter().rect_filled(Rect::from_center_size(c, vec2(5.5 * s, 5.5 * s)), 1.0, base),
                    };
                } else {
                    let label = match b.indent % 3 {
                        0 => format!("{num}."),
                        1 => format!("{}.", (b'a' + ((num.max(1) - 1) % 26) as u8) as char),
                        _ => format!("{}.", roman(num)),
                    };
                    ui.painter().text(pos2(r.left() + 17.0 * s, r.top() + 13.5 * s), Align2::RIGHT_CENTER, label, body(16.0 * ed.scale.max(0.5)), base);
                }
                let hint = if b.kind == BlockKind::Bullet { "List" } else { "List item" };
                changed |= text_edit(ui, b, idx, n, ed, pal, Tx { font, color: base, hint, width: w - 26.0 * s }, act);
            });
        }
        BlockKind::Toggle => {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let (r, resp) = ui.allocate_exact_size(vec2(26.0 * s, 26.0 * s), Sense::click());
                let t = ui.ctx().animate_bool_with_time(Id::new(("tgl", b.id)), !b.collapsed, 0.14);
                let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                let rr = Rect::from_center_size(r.center() + vec2(-2.0, 0.0), vec2(22.0 * s, 22.0 * s));
                ui.painter().rect_filled(rr, 5.0, with_alpha(pal.hover, h));
                icons::chevron(ui.painter(), rr.center(), 13.0 * s, t * std::f32::consts::FRAC_PI_2, lerp_color(pal.muted, pal.text, h));
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    b.collapsed = !b.collapsed;
                    changed = true;
                }
                changed |= text_edit(ui, b, idx, n, ed, pal, Tx { font, color: base, hint: "Toggle", width: w - 26.0 * s }, act);
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
                    let (r, _) = ui.allocate_exact_size(vec2(34.0 * s, 26.0 * s), Sense::hover());
                    icons::glyph(ui.painter(), r.left_center() + vec2(12.0 * s, 0.0), 19.0 * s, l::LIGHTBULB, tag_color(3));
                    changed |= text_edit(ui, b, idx, n, ed, pal, Tx { font, color: base, hint: "Type something…", width: w - 28.0 - 34.0 * s }, act);
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
            if crate::touch::reveal(ui.rect_contains_pointer(rr)) {
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
                ui.label(RichText::new("Add headings to your page and they will appear here.").color(pal.muted).font(body(14.0 * s)));
            }
            for (hid, lvl, text) in heads {
                let (r, resp) = ui.allocate_exact_size(vec2(w, 24.0 * s), Sense::click());
                let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                let x = r.left() + (*lvl as f32 - 1.0) * 18.0;
                let label = if text.trim().is_empty() { "Untitled" } else { text.trim() };
                let col = lerp_color(pal.muted, pal.text, t);
                let gal = ui.painter().layout_no_wrap(label.to_owned(), body(14.5 * s), col);
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
        // laid out by `draw_range` / `draw_columns` / `draw_dbview`, never by this function
        BlockKind::Columns | BlockKind::Column | BlockKind::DbView | BlockKind::Widget => {}
        BlockKind::PageLink => {
            let info = b.link.and_then(|l| links.get(&l));
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 32.0), Sense::click());
            let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
            ui.painter().rect_filled(rect, 6.0, with_alpha(pal.hover, t));
            match (info, b.link) {
                (Some(i), _) => {
                    let icon = if i.icon.is_empty() { l::FILE_TEXT } else { &i.icon };
                    icons::paint_page_icon(ui, pos2(rect.left() + 16.0, rect.center().y), icon, 19.0, pal.text);
                    let col = pal.text;
                    let g = ui.painter().layout_no_wrap(i.title.clone(), body(16.0 * ed.scale.max(0.5)), col);
                    let (tx, ty) = (rect.left() + 34.0, rect.center().y - g.size().y / 2.0);
                    let gw = g.size().x;
                    ui.painter().galley(pos2(tx, ty), g, col);
                    ui.painter().line_segment([pos2(tx, rect.center().y + 10.0), pos2(tx + gw, rect.center().y + 10.0)], Stroke::new(1.0_f32, with_alpha(col, 0.35)));
                }
                (None, None) => {
                    ui.painter().text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, format!("{}  Choose a page to link…", l::LINK_2), body(15.0), pal.muted);
                }
                (None, Some(_)) => {
                    ui.painter().text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, format!("{}  This page was deleted", l::TRIANGLE_ALERT), body(15.0), pal.danger);
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
    let is_code = b.kind == BlockKind::Code;
    let mut rt = if is_code { Rich::plain(&b.text) } else { Rich::parse(&b.text) };
    let nchars = rt.len();
    let Tx { font, color, hint, width } = tx;
    let focus_request = ed.focus.map_or(false, |(f, _)| f == b.id) || ed.restore_sel.map_or(false, |r| r.0 == b.id);

    // Unfocused blocks with inline formatting render the styled text without the markers.
    if !focus_request && b.kind != BlockKind::Code && has_marks(&b.text) && !ctx.memory(|m| m.has_focus(id)) {
        let mut job = rt.job_ex(&font, color, pal, None);
        job.wrap.max_width = width;
        ui.fonts(|f| rich::align_baselines(f, &mut job, &font));
        let galley = ui.painter().layout_job(job);
        // Same geometry as the focused `TextEdit` below (3 px margin, at least one row of the body font) so that a block does not
        // move when it gains / loses focus or when text in it is marked up.
        let row_h = ui.fonts(|f| f.row_height(&font));
        let (outer, resp) = ui.allocate_exact_size(vec2(width, galley.size().y.max(row_h) + 6.0), Sense::click());
        let rect = Rect::from_min_size(outer.min + vec2(0.0, 3.0), vec2(width, outer.height() - 6.0));
        // `[[Page]]` links are hoverable chips (rounded pill + page icon), not blue hyperlinks
        let chips = rt.wiki_chips(None);
        let chip_hit = if chips.is_empty() { None } else { resp.hover_pos().or_else(|| resp.interact_pointer_pos()).and_then(|p| rich::wiki_chip_at(&galley, rect.min, &chips, p)) };
        if !chips.is_empty() {
            let shapes = ui.fonts(|f| rich::wiki_chip_shapes(f, &galley, rect.min, &chips, pal, if resp.hovered() { chip_hit } else { None }));
            for sh in shapes {
                ui.painter().add(sh);
            }
        }
        ui.painter().galley(rect.min, galley.clone(), color);
        // hovering a link shows a hand cursor; clicking it opens it
        let mut link_hit: Option<String> = None;
        if let Some(p) = resp.hover_pos().or_else(|| resp.interact_pointer_pos()) {
            let idx = galley.cursor_from_pos(p - rect.min).ccursor.index;
            // only a click *on* the text of a link counts (not the empty space after it)
            if p.x - rect.min.x <= galley.rect.max.x + 2.0 {
                link_hit = rt.link_at(idx).map(str::to_owned);
            }
        }
        if let Some(ci) = chip_hit {
            link_hit = Some(format!("wiki:{}", chips[ci].title));
        }
        if link_hit.is_some() && resp.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        let resp = resp.on_hover_cursor(CursorIcon::Text);
        if resp.clicked() {
            match link_hit {
                Some(url) if url.starts_with("wiki:") => ed.wiki = Some(url[5..].to_owned()),
                Some(url) => platform::open_url(&ctx, &url),
                None => ed.focus = Some((b.id, usize::MAX)),
            }
        }
        if !ed.rc_frame {
            if let Some(p) = right_clicked(ui, rect) {
                open_text_menu(ed, &ctx, id, b.id, nchars, p);
            }
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

    if let Some((rb, a, c)) = ed.restore_sel {
        if rb == b.id {
            ctx.memory_mut(|m| m.request_focus(id));
            let mut st = TextEdit::load_state(&ctx, id).unwrap_or_default();
            st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(a.min(nchars)), CCursor::new(c.min(nchars)))));
            st.store(&ctx, id);
            ed.restore_sel = None;
            ed.slash_sel = 0;
        }
    }

    let focused = ctx.memory(|m| m.has_focus(id));
    let items = if focused && b.kind == BlockKind::Text && rt.plain.starts_with('/') && !rt.plain.contains('\n') { slash_items(&rt.plain[1..]) } else { vec![] };
    let slash_active = !items.is_empty();
    if slash_active {
        ed.slash_sel = ed.slash_sel.min(items.len() - 1);
    }

    if focused {
        // inline formatting shortcuts
        if b.kind != BlockKind::Code {
            let marks: [(Modifiers, Key, &str); 6] = [
                (Modifiers::COMMAND | Modifiers::SHIFT, Key::S, "~~"),
                (Modifiers::COMMAND, Key::B, "**"),
                (Modifiers::COMMAND, Key::I, "*"),
                (Modifiers::COMMAND, Key::E, "`"),
                (Modifiers::COMMAND, Key::U, "__"),
                (Modifiers::COMMAND | Modifiers::SHIFT, Key::H, "=="),
            ];
            for (m, k, mark) in marks {
                if ui.input_mut(|i| i.consume_key(m, k)) {
                    let (sa, sb) = widgets::selection(&ctx, id).unwrap_or((nchars, nchars));
                    toggle_format(&mut rt, ed, b.id, sa, sb, rich::flag_for(mark));
                    b.text = rt.markup();
                    ed.slash_sel = 0;
                }
            }
            // Copy / Cut of a formatted fragment: remember the exact formatting for the next paste.
            let copying = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy | egui::Event::Cut)));
            if copying {
                if let Some((sa, sb)) = widgets::selection(&ctx, id) {
                    if sa != sb {
                        clip::set_rich(rt.slice(sa, sb));
                    }
                }
            }
            // Pasting blocks that were copied in this app: insert the exact blocks, not their Markdown.
            let pasted = ctx.input(|i| i.events.iter().find_map(|e| if let egui::Event::Paste(t) = e { Some(t.clone()) } else { None }));
            if let Some(t) = pasted {
                if let Some(blocks) = clip::blocks_for(&t) {
                    ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Paste(_))));
                    *act = Some(Act::PasteBlocks(idx, blocks));
                }
            }
        }
        let range = TextEdit::load_state(&ctx, id).and_then(|s| s.cursor.char_range());
        let (cursor, collapsed) = range.map_or((nchars, true), |r| (r.primary.index, r.primary.index == r.secondary.index));
        let link_trig = if b.kind != BlockKind::Code && !slash_active && collapsed { wiki_trigger(&rt.plain, cursor) } else { None };
        if link_trig.is_none() && ed.link_dismiss.map_or(false, |d| d.0 == b.id) {
            ed.link_dismiss = None;
        }
        ed.want_pages |= link_trig.is_some();
        let link_sugs = match &link_trig {
            Some((st, q)) if ed.link_dismiss != Some((b.id, *st)) => wiki_sugs(&ed.pages, q),
            _ => vec![],
        };
        let link_active = !link_sugs.is_empty();
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
        } else if link_active {
            let len = link_sugs.len();
            ed.link_sel = ed.link_sel.min(len - 1);
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                ed.link_sel = (ed.link_sel + 1) % len;
                ed.slash_moved = true;
            }
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
                ed.link_sel = (ed.link_sel + len - 1) % len;
                ed.slash_moved = true;
            }
            let start = link_trig.as_ref().map_or(0, |t| t.0);
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                ed.link_dismiss = Some((b.id, start));
            }
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab)) {
                let nc = insert_wiki(&mut rt, start, cursor, &link_sugs[ed.link_sel].title);
                b.text = rt.markup();
                ed.focus = Some((b.id, nc));
                ed.link_sel = 0;
                ctx.request_repaint();
            }
        } else {
            if b.kind != BlockKind::Code {
                let shift = ui.input(|i| i.modifiers.shift);
                if !shift && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
                    let listy = matches!(b.kind, BlockKind::Todo | BlockKind::Bullet | BlockKind::Numbered | BlockKind::Quote | BlockKind::Callout | BlockKind::Toggle);
                    if rt.is_empty() && listy {
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
    let (pal2, font2) = (*pal, font.clone());
    let pending_fmt = ed.pending.filter(|p| p.0 == b.id).map(|p| p.2);
    // caret of the previous frame: a `[[link]]` that contains it is shown as raw, editable text; the others are chips
    let caret_key = id.with("wl_caret");
    let caret_prev: Option<usize> = if focused { ctx.data(|d| d.get_temp(caret_key)) } else { None };
    let rt_old = rt.clone();
    let mut buf = rt.plain.clone();
    // The layouter shows the formatted text *without any markers*; while the text changes inside a frame it adapts the formats.
    let mut layouter = |ui: &Ui, s: &str, wrap: f32| {
        let mut job = if is_code {
            code_job(s, &font2, &pal2)
        } else if s == rt_old.plain {
            rt_old.job_ex(&font2, color, &pal2, caret_prev)
        } else {
            let mut r = rt_old.clone();
            r.adjust(s, pending_fmt, None);
            r.job_ex(&font2, color, &pal2, caret_prev)
        };
        job.wrap.max_width = wrap;
        ui.fonts(|f| {
            if !is_code {
                rich::align_baselines(f, &mut job, &font2);
            }
            f.layout_job(job)
        })
    };
    let chip_bg = ui.painter().add(egui::Shape::Noop);
    let out = TextEdit::multiline(&mut buf)
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

    if out.response.changed() {
        if is_code {
            b.text = buf.clone();
        } else {
            let paste = ctx.input(|i| i.events.iter().find_map(|e| if let egui::Event::Paste(t) = e { clip::rich_for(t) } else { None }));
            rt.adjust(&buf, pending_fmt, paste.as_ref());
            b.text = rt.markup();
            ed.pending = None;
        }
    }
    if let Some((pb, pc, _)) = ed.pending {
        // the caret moved away: the pending format no longer applies
        let cur = out.cursor_range.map(|c| c.primary.ccursor.index);
        if pb == b.id && cur.is_some() && cur != Some(pc) {
            ed.pending = None;
        }
    }

    if !is_code && rt.plain.contains("[[") {
        let chips = rt.wiki_chips(caret_prev);
        if !chips.is_empty() {
            let hover = ui.input(|i| i.pointer.hover_pos()).and_then(|p| rich::wiki_chip_at(&out.galley, out.galley_pos, &chips, p));
            let shapes = ui.fonts(|f| rich::wiki_chip_shapes(f, &out.galley, out.galley_pos, &chips, pal, hover));
            ui.painter().set(chip_bg, egui::Shape::Vec(shapes));
            if let Some(hi) = hover {
                ctx.set_cursor_icon(CursorIcon::PointingHand);
                if out.response.clicked() && !out.response.changed() {
                    ed.wiki = Some(chips[hi].title.clone());
                }
            }
        }
    }
    if focused {
        if let Some(cr) = out.cursor_range {
            let c = cr.primary.ccursor.index;
            if Some(c) != caret_prev {
                ctx.data_mut(|d| d.insert_temp(caret_key, c));
                if rt.plain.contains("[[") {
                    ctx.request_repaint();
                }
            }
        }
    }

    let mut cursor_idx = 0;
    if let Some(cr) = out.cursor_range {
        cursor_idx = cr.primary.ccursor.index;
        let row = cr.primary.rcursor.row;
        let rows = out.galley.rows.len();
        ctx.data_mut(|d| d.insert_temp(id.with("rows"), (row, rows)));
    }

    // Phones: keep the caret visible above the on-screen keyboard / the toolbar while typing in a block that sits low on the screen.
    // (The hidden text input follows the caret, see vendor/eframe text_agent.rs — it is clamped to the visible area.)
    if crate::touch::touchy() && focused && out.response.changed() {
        if let Some(cr) = out.cursor_range {
            let r = out.galley.pos_from_cursor(&cr.primary).translate(out.galley_pos.to_vec2());
            ui.scroll_to_rect(r.expand2(vec2(0.0, 72.0)), None);
        }
    }
    // Phones: when the visible height changes (the soft keyboard opens / closes, the phone is rotated) bring the caret of the block that is
    // being edited back into view once — otherwise a block in the lower half of the screen ends up below the keyboard. Only on a size
    // change, so it never fights a swipe that scrolls the page.
    if crate::touch::touchy() && focused {
        let h = ctx.screen_rect().height();
        let key = id.with("kb_view_h");
        let prev: Option<f32> = ctx.data(|d| d.get_temp(key));
        if prev.map_or(true, |p| (p - h).abs() > 1.0) {
            ctx.data_mut(|d| d.insert_temp(key, h));
            if let Some(cr) = out.cursor_range {
                let r = out.galley.pos_from_cursor(&cr.primary).translate(out.galley_pos.to_vec2());
                ui.scroll_to_rect(r.expand2(vec2(0.0, 72.0)), None);
            }
        }
    }

    if !ed.rc_frame {
        if let Some(p) = right_clicked(ui, out.response.rect) {
            open_text_menu(ed, &ctx, id, b.id, rt.len(), p);
        }
    }

    // floating selection toolbar
    if focused && b.kind != BlockKind::Code {
        // NOTE: `cursor_range` is `None` on the frame where a press *outside* the field (e.g. on the toolbar itself)
        // makes it surrender focus. That must NOT dismiss the toolbar, otherwise its buttons could never be clicked.
        match out.cursor_range {
            Some(cr) if cr.primary.ccursor.index != cr.secondary.ccursor.index => {
                let (pa, pb) = (cr.primary.ccursor.index, cr.secondary.ccursor.index);
                let lo = if pa < pb { cr.primary } else { cr.secondary };
                let r = out.galley.pos_from_cursor(&lo).translate(out.galley_pos.to_vec2());
                ed.fmt_bar = Some(FmtBar { bid: b.id, pos: r.left_top(), a: pa.min(pb), b: pa.max(pb) });
            }
            Some(_) => {
                if ed.fmt_bar.map_or(false, |f| f.bid == b.id) {
                    ed.fmt_bar = None;
                }
            }
            None => {}
        }
    }

    if out.response.changed()
        && b.kind == BlockKind::Text
        && act.is_none()
        && media::looks_like_url(&rt.plain)
        && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))))
    {
        *act = Some(Act::UrlBlock(idx));
    }
    if out.response.changed() && b.kind == BlockKind::Text && act.is_none() {
        if let Some((kind, n)) = md_shortcut(&rt.plain, cursor_idx) {
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
        let pressed = ctx.input(|i| i.pointer.primary_pressed());
        let area = egui::Area::new(Id::new("slash_menu")).order(Order::Foreground).fixed_pos(pos2(pos.x, y + (1.0 - t) * -5.0)).show(&ctx, |ui| {
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
                        // Hit-test by position (not by egui's hover flag): the text field loses focus on the very
                        // press that clicks an item, and the flag is unreliable on that frame.
                        let over = ui.rect_contains_pointer(r.rect) && ui.clip_rect().contains(r.rect.center());
                        if over {
                            ed.slash_sel = i;
                        }
                        if pressed && over {
                            pick = Some(*k);
                        }
                    }
                });
            });
        });
        ctx.raise_layer(area.response.layer_id);
        if let Some(k) = pick {
            *act = Some(slash_act(idx, k));
        }
    } else if focused || !ctx.memory(|m| m.focused().is_some()) {
        reset_appear(&ctx, Id::new("slash_appear"));
    }

    // `[[` page suggestions: a drop-down under the caret listing matching pages (and "Create …" for a new title)
    if focused && !slash_active && b.kind != BlockKind::Code {
        let caret = out.cursor_range.filter(|c| c.primary.ccursor.index == c.secondary.ccursor.index);
        let trig = caret.and_then(|c| wiki_trigger(&rt.plain, c.primary.ccursor.index));
        if let (Some(cr), Some((st, q))) = (caret, trig) {
            let sugs = if ed.link_dismiss == Some((b.id, st)) { vec![] } else { wiki_sugs(&ed.pages, &q) };
            ed.want_pages = true;
            if !sugs.is_empty() {
                let screen = ctx.screen_rect();
                let c_rect = out.galley.pos_from_cursor(&cr.primary).translate(out.galley_pos.to_vec2());
                let w = 300.0_f32.min(screen.width() - 16.0);
                let est_h = (sugs.len() as f32 * if crate::touch::touchy() { 40.0 } else { 30.0 } + 44.0).min(380.0);
                let below = c_rect.bottom() + 6.0;
                let y = if below + est_h > screen.bottom() - 6.0 && c_rect.top() - est_h - 6.0 > screen.top() { c_rect.top() - est_h - 6.0 } else { below.min(screen.bottom() - est_h - 6.0).max(6.0) };
                let x = c_rect.left().min(screen.right() - w - 8.0).max(8.0);
                let t = appear(&ctx, Id::new("wiki_appear"), 0.12);
                let moved = std::mem::take(&mut ed.slash_moved);
                ed.link_sel = ed.link_sel.min(sugs.len() - 1);
                let sel = ed.link_sel;
                let pressed = ctx.input(|i| i.pointer.primary_pressed());
                let mut pick: Option<usize> = None;
                let area = egui::Area::new(Id::new("wiki_menu")).order(Order::Foreground).fixed_pos(pos2(x, y + (1.0 - t) * -4.0)).show(&ctx, |ui| {
                    ui.set_opacity(t);
                    crate::widgets::popup_frame(pal).show(ui, |ui| {
                        ui.set_width(w);
                        section_label(ui, pal, "Link to page");
                        ScrollArea::vertical().id_salt("wiki_scroll").max_height(300.0).auto_shrink([false, true]).show(ui, |ui| {
                            for (i, sg) in sugs.iter().enumerate() {
                                let (lead, label) = if sg.create { (l::PLUS, format!("Create “{}”", sg.title)) } else { (if sg.icon.is_empty() { l::FILE_TEXT } else { sg.icon.as_str() }, sg.title.clone()) };
                                let r = menu_item(ui, pal, lead, &label, None, i == sel, false);
                                if i == sel && moved {
                                    r.scroll_to_me(None);
                                }
                                let over = ui.rect_contains_pointer(r.rect) && ui.clip_rect().contains(r.rect.center());
                                if over {
                                    ed.link_sel = i;
                                }
                                if pressed && over {
                                    pick = Some(i);
                                }
                            }
                        });
                    });
                });
                ctx.raise_layer(area.response.layer_id);
                if let Some(i) = pick {
                    let nc = insert_wiki(&mut rt, st, cr.primary.ccursor.index, &sugs[i].title);
                    b.text = rt.markup();
                    ed.focus = Some((b.id, nc));
                    ed.link_sel = 0;
                    ctx.request_repaint();
                }
            } else {
                reset_appear(&ctx, Id::new("wiki_appear"));
            }
        } else {
            reset_appear(&ctx, Id::new("wiki_appear"));
        }
    }

    out.response.changed()
}

/// The open `[[` before the caret: `(char index of the "[[", text typed after it)`; `None` when the caret is not inside an unclosed link.
fn wiki_trigger(plain: &str, cursor: usize) -> Option<(usize, String)> {
    let chars: Vec<char> = plain.chars().collect();
    let cur = cursor.min(chars.len());
    let mut i = cur;
    while i >= 2 && cur - i <= 60 {
        if chars[i - 1] == '[' && chars[i - 2] == '[' {
            let q: String = chars[i..cur].iter().collect();
            return if q.contains(['[', ']', '\n']) { None } else { Some((i - 2, q)) };
        }
        if chars[i - 1] == ']' || chars[i - 1] == '\n' {
            return None;
        }
        i -= 1;
    }
    None
}

struct WikiSug {
    title: String,
    icon: String,
    create: bool,
}

/// Pages matching what was typed after `[[` (prefix matches first), plus a "create" entry when the title does not exist yet.
fn wiki_sugs(pages: &[(String, String)], q: &str) -> Vec<WikiSug> {
    let ql = q.trim().to_lowercase();
    let mut hits: Vec<&(String, String)> = pages.iter().filter(|(t, _)| ql.is_empty() || t.to_lowercase().contains(&ql)).collect();
    hits.sort_by_key(|(t, _)| !t.to_lowercase().starts_with(&ql));
    let mut v: Vec<WikiSug> = hits.into_iter().take(8).map(|(t, i)| WikiSug { title: t.clone(), icon: i.clone(), create: false }).collect();
    if !ql.is_empty() && !pages.iter().any(|(t, _)| t.trim().to_lowercase() == ql) {
        v.push(WikiSug { title: q.trim().to_owned(), icon: String::new(), create: true });
    }
    v
}

/// Replaces the open `[[query` (and a `]]` the user already typed after the caret) with `[[Title]]`; returns the new caret.
fn insert_wiki(rt: &mut Rich, start: usize, cursor: usize, title: &str) -> usize {
    let mut end = cursor;
    if rt.plain.chars().skip(cursor).take(2).collect::<String>() == "]]" {
        end += 2;
    }
    let clean: String = title.chars().filter(|c| !matches!(c, '|' | '[' | ']' | '\n')).collect();
    let t = format!("[[{}]]", clean.trim());
    let n = t.chars().count();
    rt.replace(start, end, &Rich::plain(&t));
    start + n
}

/// Bold / italic / … on the selection, or — with an empty selection — on the text typed next.
fn toggle_format(rt: &mut Rich, ed: &mut EditorState, bid: u64, a: usize, b: usize, flag: u8) {
    let (a, b) = (a.min(b), a.max(b));
    if a != b {
        rt.toggle(a, b, flag);
        ed.pending = None;
    } else {
        let mut f = match ed.pending {
            Some((pb, pc, f)) if pb == bid && pc == a => f,
            _ => a.checked_sub(1).and_then(|i| rt.fmts.get(i).copied()).unwrap_or_default(),
        };
        f.link = 0;
        f.flags ^= flag;
        ed.pending = Some((bid, a, f));
    }
}

/// The word around char index `at` (or the closest one) as a `(start, end)` char range.
fn word_range(text: &str, at: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let is_w = |c: char| c.is_alphanumeric() || c == '_' || c == '\'' || c == '-';
    let mut i = at.min(chars.len().saturating_sub(1));
    if !is_w(chars[i]) {
        if i > 0 && is_w(chars[i - 1]) {
            i -= 1;
        } else if let Some(k) = (i..chars.len()).find(|&k| is_w(chars[k])) {
            i = k;
        } else {
            return (at.min(chars.len()), at.min(chars.len()));
        }
    }
    let (mut a, mut b) = (i, i + 1);
    while a > 0 && is_w(chars[a - 1]) {
        a -= 1;
    }
    while b < chars.len() && is_w(chars[b]) {
        b += 1;
    }
    (a, b)
}

fn open_text_menu(ed: &mut EditorState, ctx: &Context, id: Id, bid: u64, nchars: usize, p: egui::Pos2) {
    let (a, b) = widgets::selection(ctx, id).unwrap_or((nchars, nchars));
    let (a, b) = (a.min(nchars), b.min(nchars));
    ed.menu = Some(EdMenu::Text { bid, a, b, pos: p });
    ed.menu_fresh = true;
    ed.rc_frame = true;
    ed.block_menu = None;
    ed.fmt_bar = None;
}

#[derive(Clone, Copy, PartialEq)]
enum Cmd {
    Cut,
    Copy,
    Paste,
    SelAll,
    SelWord,
    SelBlock,
    Fmt(&'static str),
    Style,
    Dup,
    Del,
    Indent,
    Outdent,
    Up,
    Down,
    CopyBlock,
}

fn ed_menu(ui: &mut Ui, page: &mut Page, next_id: &mut u64, ed: &mut EditorState, pal: &Palette, m: EdMenu) -> bool {
    let ctx = ui.ctx().clone();
    let (bid, pos, sel) = match m {
        EdMenu::Text { bid, a, b, pos } => (bid, pos, Some((a, b))),
        EdMenu::Block { bid, pos } => (bid, pos, None),
    };
    let Some(idx) = page.blocks.iter().position(|b| b.id == bid) else {
        ed.menu = None;
        return false;
    };
    let kind = page.blocks[idx].kind;
    let has_text = kind.has_text();
    let has_sel = sel.map_or(false, |(a, b)| a != b);
    let mut list: Vec<(CtxItem, Cmd)> = vec![];
    if has_text {
        if sel.is_some() {
            list.push((CtxItem::new(l::SCISSORS, "Cut").hint("Ctrl+X").enabled(has_sel), Cmd::Cut));
            list.push((CtxItem::new(l::COPY, "Copy").hint("Ctrl+C").enabled(has_sel), Cmd::Copy));
            list.push((CtxItem::new(l::CLIPBOARD_PASTE, "Paste").hint("Ctrl+V"), Cmd::Paste));
            if !has_sel && crate::touch::touchy() {
                list.push((CtxItem::new(l::TEXT_SELECT, "Select word"), Cmd::SelWord));
            }
            list.push((CtxItem::new(l::TEXT_SELECT, "Select all").hint("Ctrl+A"), Cmd::SelAll));
            if kind != BlockKind::Code {
                list.push((CtxItem::new(l::BOLD, "Bold").hint("Ctrl+B").enabled(has_sel).sep(), Cmd::Fmt("**")));
                list.push((CtxItem::new(l::ITALIC, "Italic").hint("Ctrl+I").enabled(has_sel), Cmd::Fmt("*")));
                list.push((CtxItem::new(l::STRIKETHROUGH, "Strikethrough").hint("Ctrl+Shift+S").enabled(has_sel), Cmd::Fmt("~~")));
                list.push((CtxItem::new(l::CODE, "Inline code").hint("Ctrl+E").enabled(has_sel), Cmd::Fmt("`")));
            }
        } else {
            list.push((CtxItem::new(l::COPY, "Copy text"), Cmd::CopyBlock));
        }
    }
    list.push((CtxItem::new(l::REPLACE, if has_text { "Turn into & colour…" } else { "Style…" }).sep(), Cmd::Style));
    list.push((CtxItem::new(l::SQUARE_CHECK, "Select block"), Cmd::SelBlock));
    list.push((CtxItem::new(l::COPY_PLUS, "Duplicate").hint("Ctrl+D"), Cmd::Dup));
    list.push((CtxItem::new(l::ARROW_UP, "Move up").enabled(idx > 0), Cmd::Up));
    list.push((CtxItem::new(l::ARROW_DOWN, "Move down").enabled(subtree_end(&page.blocks, idx) < page.blocks.len()), Cmd::Down));
    list.push((CtxItem::new(l::INDENT_INCREASE, "Indent").hint("Tab").enabled(idx > 0), Cmd::Indent));
    list.push((CtxItem::new(l::INDENT_DECREASE, "Outdent").hint("Shift+Tab").enabled(page.blocks[idx].indent > 0), Cmd::Outdent));
    list.push((CtxItem::new(l::TRASH_2, "Delete").hint("Del").danger().sep(), Cmd::Del));
    let items: Vec<CtxItem> = list.iter().map(|x| x.0).collect();
    let (open, pick) = context_menu(&ctx, Id::new("ed_menu"), pos, pal, &mut ed.menu_fresh, &items);
    let mut changed = false;
    if let Some(pi) = pick {
        let tid = Id::new(("blk", bid));
        let (a, b) = sel.unwrap_or((0, 0));
        let mut rt = Rich::parse(&page.blocks[idx].text);
        let nchars = rt.len();
        match list[pi].1 {
            Cmd::Copy => {
                let frag = rt.slice(a, b);
                ctx.copy_text(frag.plain.clone());
                clip::set_rich(frag);
                ed.restore_sel = Some((bid, a, b));
            }
            Cmd::CopyBlock => {
                ctx.copy_text(rt.plain.clone());
                clip::set_blocks(rt.plain.clone(), vec![page.blocks[idx].clone()]);
            }
            Cmd::Cut => {
                let frag = rt.slice(a, b);
                ctx.copy_text(frag.plain.clone());
                clip::set_rich(frag);
                rt.replace(a, b, &Rich::default());
                page.blocks[idx].text = rt.markup();
                let at = a.min(b);
                ed.restore_sel = Some((bid, at, at));
                changed = true;
            }
            Cmd::Paste => {
                platform::request_paste(&ctx);
                ed.paste_wait = Some((bid, a, b));
                ed.restore_sel = Some((bid, a, b));
            }
            Cmd::SelAll => ed.restore_sel = Some((bid, 0, nchars)),
            Cmd::SelWord => {
                let (wa, wb) = word_range(&rt.plain, a.min(nchars));
                ed.restore_sel = Some((bid, wa, wb));
            }
            Cmd::SelBlock => {
                if !ed.sel.contains(&bid) {
                    ed.sel.push(bid);
                }
                ed.touch_sel = true;
                ed.focus = None;
                ed.active = None;
                ed.fmt_bar = None;
                ctx.memory_mut(|m| {
                    if let Some(f) = m.focused() {
                        m.surrender_focus(f);
                    }
                });
            }
            Cmd::Fmt(mark) => {
                rt.toggle(a, b, rich::flag_for(mark));
                page.blocks[idx].text = rt.markup();
                ed.restore_sel = Some((bid, a, b));
                let _ = tid;
                changed = true;
            }
            Cmd::Style => {
                ed.block_menu = Some((bid, pos));
                ed.fresh = true;
            }
            Cmd::Dup => {
                apply(page, Act::Dup(idx), next_id, ed);
                changed = true;
            }
            Cmd::Del => {
                apply(page, Act::Remove(idx), next_id, ed);
                changed = true;
            }
            Cmd::Indent => {
                apply(page, Act::Indent(idx, 1), next_id, ed);
                changed = true;
            }
            Cmd::Outdent => {
                apply(page, Act::Indent(idx, -1), next_id, ed);
                changed = true;
            }
            Cmd::Up => {
                apply(page, Act::MoveTo(idx, idx - 1), next_id, ed);
                changed = true;
            }
            Cmd::Down => {
                let j = subtree_end(&page.blocks, idx);
                let to = subtree_end(&page.blocks, j);
                apply(page, Act::MoveTo(idx, to), next_id, ed);
                changed = true;
            }
        }
    }
    if !open {
        ed.menu = None;
    }
    changed
}

/// Applies clipboard text that arrived (asynchronously on the web) after "Paste" was chosen from a menu.
fn finish_paste(page: &mut Page, next_id: &mut u64, ed: &mut EditorState) -> bool {
    let Some((bid, a, b)) = ed.paste_wait else { return false };
    let Some(clip) = platform::take_paste() else { return false };
    ed.paste_wait = None;
    let Some(idx) = page.blocks.iter().position(|x| x.id == bid) else { return false };
    let clip = clip.replace("\r\n", "\n");
    if clip.is_empty() {
        return false;
    }
    let (a, b) = (a.min(b), a.max(b));
    let is_code = page.blocks[idx].kind == BlockKind::Code;
    // 1. our own blocks come back as exactly the same blocks
    if !is_code {
        if let Some(blocks) = clip::blocks_for(&clip) {
            apply(page, Act::PasteBlocks(idx, blocks), next_id, ed);
            return true;
        }
    }
    if is_code {
        let at = replace_chars(&mut page.blocks[idx].text, a, b, &clip);
        ed.restore_sel = Some((bid, at, at));
        return true;
    }
    // 2. a formatted fragment copied inside the app keeps its formatting
    if !clip.contains('\n') {
        let frag = clip::rich_for(&clip).unwrap_or_else(|| Rich::plain(&clip));
        let mut rt = Rich::parse(&page.blocks[idx].text);
        rt.replace(a, b, &frag);
        page.blocks[idx].text = rt.markup();
        let at = a + frag.len();
        ed.restore_sel = Some((bid, at, at));
        return true;
    }
    // 3. Markdown from elsewhere becomes real blocks (headings, lists, to-dos, quotes, code …)
    if let Some(blocks) = clip::markdown_blocks(&clip) {
        apply(page, Act::PasteBlocks(idx, blocks), next_id, ed);
        return true;
    }
    // 4. plain multi-line text: one block per line (list items continue the list)
    let whole = Rich::parse(&page.blocks[idx].text);
    let (head, tail) = (whole.slice(0, a), whole.slice(b, whole.len()));
    let mut lines = clip.split('\n');
    let mut first = head;
    first.append(&Rich::plain(lines.next().unwrap_or("")));
    page.blocks[idx].text = first.markup();
    let rest: Vec<&str> = lines.collect();
    let (kind, indent) = (page.blocks[idx].kind, page.blocks[idx].indent);
    let nk = match kind {
        BlockKind::Todo | BlockKind::Bullet | BlockKind::Numbered => kind,
        _ => BlockKind::Text,
    };
    let mut at = idx + 1;
    let mut last = (bid, first.len());
    let n = rest.len();
    for (k, line) in rest.into_iter().enumerate() {
        let mut r = Rich::plain(line);
        let cur = r.len();
        if k + 1 == n {
            r.append(&tail);
        }
        let nb = new_block(next_id, nk, r.markup(), indent);
        last = (nb.id, cur);
        page.blocks.insert(at, nb);
        at += 1;
    }
    ed.focus = Some(last);
    true
}

/// Floating toolbar shown above a text selection (Notion style).
fn fmt_bar(ui: &mut Ui, page: &mut Page, ed: &mut EditorState, pal: &Palette) -> bool {
    let ctx = ui.ctx().clone();
    let appear_id = Id::new("fmt_appear");
    let Some(bar) = ed.fmt_bar else {
        reset_appear(&ctx, appear_id);
        return false;
    };
    let Some(idx) = page.blocks.iter().position(|b| b.id == bar.bid) else {
        ed.fmt_bar = None;
        return false;
    };
    let tid = Id::new(("blk", bar.bid));
    let rect_id = Id::new("fmt_rect");
    let prev: Option<Rect> = ctx.data(|d| d.get_temp(rect_id));
    let (down, released) = ctx.input(|i| (i.pointer.any_down(), i.pointer.any_released()));
    let ptr = ctx.pointer_interact_pos();
    let over = matches!((prev, ptr), (Some(r), Some(p)) if r.contains(p));
    let focused = ctx.memory(|m| m.has_focus(tid));
    if (!focused && !(over && (down || released))) || ed.menu.is_some() || ed.block_menu.is_some() {
        ed.fmt_bar = None;
        reset_appear(&ctx, appear_id);
        return false;
    }
    if down && !over {
        return false; // still dragging out a selection
    }
    let t = appear(&ctx, appear_id, 0.12);
    let screen = ctx.screen_rect();
    // Phones: the buttons are finger-sized (38 px), 9 of them do not fit a 360 px screen — the bar wraps onto a second row instead of
    // running off the edge.
    let btn: f32 = if crate::touch::touchy() { 38.0 } else { 27.0 };
    let full_w = 9.0 * btn + 2.0 * 9.0 + 12.0;
    let w = full_w.min((screen.width() - 16.0).max(120.0));
    let wrap = full_w > w;
    let h = if wrap { 2.0 * btn + 12.0 } else { 40.0 };
    let x = (bar.pos.x - 10.0).clamp(8.0, (screen.right() - w - 8.0).max(8.0));
    // on touch screens the finger / selection handles / keyboard hide the area under the selection: prefer the space above
    let above = bar.pos.y - h - 8.0 > screen.top() + 6.0;
    let y = if above { bar.pos.y - h - 8.0 } else { bar.pos.y + 32.0 };
    let mut clicked: Option<Cmd> = None;
    let mut link_clicked = false;
    let area = egui::Area::new(Id::new("fmt_bar")).order(Order::Foreground).fixed_pos(pos2(x, y + (1.0 - t) * 5.0)).show(&ctx, |ui| {
        ui.set_opacity(t);
        crate::widgets::popup_frame(pal).inner_margin(Margin::same(4)).corner_radius(CornerRadius::same(9)).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            if wrap {
                ui.set_width(w - 8.0);
            }
            let row = |ui: &mut Ui| {
                let sep = |ui: &mut Ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(9.0, 26.0), Sense::hover());
                    ui.painter().vline(r.center().x, r.y_range().shrink(4.0), Stroke::new(1.0_f32, pal.border));
                };
                if widgets::icon_button(ui, pal, Icon::Type, 15.0, "Turn into & colour").clicked() {
                    clicked = Some(Cmd::Style);
                }
                sep(ui);
                let state = Rich::parse(&page.blocks[idx].text);
                for (icon, mark, tip) in [
                    (Icon::Bold, "**", "Bold  Ctrl+B"),
                    (Icon::Italic, "*", "Italic  Ctrl+I"),
                    (Icon::Underline, "__", "Underline  Ctrl+U"),
                    (Icon::Strike, "~~", "Strikethrough  Ctrl+Shift+S"),
                    (Icon::Code, "`", "Inline code  Ctrl+E"),
                    (Icon::Highlight, "==", "Highlight  Ctrl+Shift+H"),
                ] {
                    let r = widgets::icon_button(ui, pal, icon, 15.0, tip);
                    if state.all_have(bar.a, bar.b, rich::flag_for(mark)) {
                        ui.painter().rect_filled(r.rect.shrink2(vec2(0.0, 1.0)), 6.0, with_alpha(pal.accent, 0.22));
                        ui.painter().hline(r.rect.x_range().shrink(7.0), r.rect.bottom() - 3.0, Stroke::new(2.0_f32, pal.accent));
                    }
                    if r.clicked() {
                        clicked = Some(Cmd::Fmt(mark));
                    }
                }
                let lr = widgets::icon_button(ui, pal, Icon::Link, 15.0, "Link");
                if state.link_in(bar.a, bar.b).is_some() {
                    ui.painter().rect_filled(lr.rect.shrink2(vec2(0.0, 1.0)), 6.0, with_alpha(pal.accent, 0.22));
                }
                if lr.clicked() {
                    link_clicked = true;
                }
                sep(ui);
                if widgets::icon_button(ui, pal, Icon::Palette, 15.0, "Colour & background").clicked() {
                    clicked = Some(Cmd::Style);
                }
            };
            if wrap {
                ui.horizontal_wrapped(row);
            } else {
                ui.horizontal(row);
            }
        });
    });
    ctx.raise_layer(area.response.layer_id);
    ctx.data_mut(|d| d.insert_temp(rect_id, area.response.rect));
    let mut changed = false;
    if link_clicked && clicked.is_none() {
        let existing = Rich::parse(&page.blocks[idx].text).link_in(bar.a, bar.b).unwrap_or_else(|| "https://".to_owned());
        ed.link_edit = Some(LinkEdit { bid: bar.bid, a: bar.a, b: bar.b, url: existing, pos: pos2(area.response.rect.left(), area.response.rect.bottom() + 6.0), fresh: true });
        ed.fmt_bar = None;
        return false;
    }
    match clicked {
        Some(Cmd::Fmt(mark)) => {
            let mut rt = Rich::parse(&page.blocks[idx].text);
            rt.toggle(bar.a, bar.b, rich::flag_for(mark));
            page.blocks[idx].text = rt.markup();
            ed.restore_sel = Some((bar.bid, bar.a, bar.b));
            changed = true;
        }
        Some(_) => {
            ed.restore_sel = Some((bar.bid, bar.a, bar.b));
            ed.block_menu = Some((bar.bid, pos2(area.response.rect.left(), area.response.rect.bottom() + 6.0)));
            ed.fresh = true;
            ed.fmt_bar = None;
        }
        None => {}
    }
    changed
}

pub fn link_popup(ui: &mut Ui, page: &mut Page, ed: &mut EditorState, pal: &Palette) -> bool {
    let Some(mut le) = ed.link_edit.take() else { return false };
    let ctx = ui.ctx().clone();
    let Some(idx) = page.blocks.iter().position(|b| b.id == le.bid) else { return false };
    let (mut apply_it, mut remove, mut close) = (false, false, false);
    let id = Id::new("link_url");
    let screen = ctx.screen_rect();
    let pos = pos2(le.pos.x.min(screen.right() - 340.0).max(8.0), le.pos.y.min(screen.bottom() - 90.0).max(8.0));
    let area = egui::Area::new(Id::new("link_popup")).order(Order::Foreground).fixed_pos(pos).show(&ctx, |ui| {
        crate::widgets::popup_frame(pal).inner_margin(Margin::same(8)).show(ui, |ui| {
            ui.set_width(318.0);
            ui.horizontal(|ui| {
                icons::paint(ui.painter(), ui.cursor().min + vec2(10.0, 12.0), 14.0, Icon::Link, pal.muted);
                ui.add_space(24.0);
                let r = ui.add(TextEdit::singleline(&mut le.url).id(id).hint_text("Paste or type a link").desired_width(200.0));
                if le.fresh {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    apply_it = true;
                }
                if ui.button("Apply").clicked() {
                    apply_it = true;
                }
                if ui.button("✕").on_hover_text("Remove link").clicked() {
                    remove = true;
                }
            });
        });
    });
    ctx.raise_layer(area.response.layer_id);
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        close = true;
    }
    if !le.fresh && ctx.input(|i| i.pointer.any_pressed()) && !ctx.pointer_interact_pos().map_or(false, |p| area.response.rect.contains(p)) {
        close = true;
    }
    le.fresh = false;
    let mut changed = false;
    if apply_it || remove {
        let mut rt = Rich::parse(&page.blocks[idx].text);
        let url = le.url.trim();
        if remove || url.is_empty() || url == "https://" {
            rt.set_link(le.a, le.b, None);
        } else {
            let full = if url.contains("://") || url.starts_with("mailto:") { url.to_owned() } else { format!("https://{url}") };
            rt.set_link(le.a, le.b, Some(&full));
        }
        page.blocks[idx].text = rt.markup();
        ed.restore_sel = Some((le.bid, le.b, le.b));
        changed = true;
        close = true;
    }
    if close {
        if !changed {
            ed.restore_sel = Some((le.bid, le.a, le.b));
        }
    } else {
        ed.link_edit = Some(le);
    }
    changed
}

pub fn swatches(ui: &mut Ui, pal: &Palette, cur: u8, is_bg: bool, pick: &mut Option<u8>) {
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

    changed |= finish_paste(page, next_id, ed);
    if let Some(m) = ed.menu {
        changed |= ed_menu(ui, page, next_id, ed, pal, m);
    }
    changed |= fmt_bar(ui, page, ed, pal);
    changed |= link_popup(ui, page, ed, pal);

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
                    if menu_item(ui, pal, l::INDENT_INCREASE, "Indent", None, false, false).clicked() {
                        action = Some(Act::Indent(idx, 1));
                    }
                    if menu_item(ui, pal, l::INDENT_DECREASE, "Outdent", None, false, false).clicked() {
                        action = Some(Act::Indent(idx, -1));
                    }
                    if menu_item(ui, pal, l::COPY_PLUS, "Duplicate", None, false, false).clicked() {
                        action = Some(Act::Dup(idx));
                    }
                    if menu_item(ui, pal, l::TRASH_2, "Delete", None, false, true).clicked() {
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

    changed |= pickers::icon_picker(&ctx, page, ed, pal);
    changed |= pickers::cover_picker(&ctx, page, ed, pal);
    changed |= multisel::bar(ui, page, next_id, ed, pal);
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blk(id: u64, kind: BlockKind, text: &str, indent: u8) -> Block {
        Block::new(id, kind, text, indent)
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
    fn wiki_trigger_finds_the_open_brackets() {
        assert_eq!(wiki_trigger("see [[Pro", 9), Some((4, "Pro".to_owned())));
        assert_eq!(wiki_trigger("[[", 2), Some((0, String::new())));
        assert_eq!(wiki_trigger("[[done]] and", 12), None, "a closed link is not a trigger");
        assert_eq!(wiki_trigger("plain text", 5), None);
        assert_eq!(wiki_trigger("[[a\nb", 6), None, "a trigger never spans lines");
    }

    #[test]
    fn wiki_suggestions_rank_and_offer_create() {
        let pages: Vec<(String, String)> = ["Project plan", "Roadmap", "My project"].iter().map(|t| (t.to_string(), String::new())).collect();
        let s = wiki_sugs(&pages, "proj");
        assert_eq!(s[0].title, "Project plan", "prefix matches first");
        assert!(s.iter().any(|x| x.title == "My project"));
        assert!(s.last().unwrap().create, "an unknown title offers Create");
        assert!(!wiki_sugs(&pages, "roadmap").iter().any(|x| x.create), "an existing title does not");
        assert_eq!(wiki_sugs(&pages, "").len(), 3);
    }

    #[test]
    fn inserting_a_wiki_link_replaces_the_trigger_and_typed_closers() {
        let mut rt = Rich::plain("see [[Pro]] now");
        let caret = insert_wiki(&mut rt, 4, 9, "Project plan");
        assert_eq!(rt.plain, "see [[Project plan]] now");
        assert_eq!(caret, 4 + "[[Project plan]]".chars().count());
        let mut rt = Rich::plain("[[x");
        insert_wiki(&mut rt, 0, 3, "A|B]]");
        assert_eq!(rt.plain, "[[AB]]", "separators are stripped from titles");
    }

    #[test]
    fn slash_menu_filters() {
        assert!(slash_items("").len() > 15);
        assert_eq!(slash_items("todo")[0].label, "To-do list");
        assert!(slash_items("kanban").iter().any(|i| i.label == "Board database"));
        assert!(slash_items("zzzzzz").is_empty());
    }

    fn kinds(p: &Page) -> Vec<(BlockKind, u8)> {
        p.blocks.iter().map(|b| (b.kind, b.indent)).collect()
    }

    #[test]
    fn slash_columns_builds_a_valid_layout() {
        let mut p = page(vec![blk(1, BlockKind::Text, "/columns", 0), blk(2, BlockKind::Text, "tail", 0)]);
        let mut next = 100;
        let mut ed = EditorState::default();
        apply(&mut p, Act::Special(0, Slash::Columns(3)), &mut next, &mut ed);
        normalize_layout(&mut p.blocks, &mut next);
        let k = kinds(&p);
        assert_eq!(k[0], (BlockKind::Columns, 0));
        assert_eq!(k.iter().filter(|x| **x == (BlockKind::Column, 1)).count(), 3);
        assert_eq!(k.iter().filter(|x| **x == (BlockKind::Text, 2)).count(), 3);
        assert_eq!(*k.last().unwrap(), (BlockKind::Text, 0));
        let before = kinds(&p);
        normalize_layout(&mut p.blocks, &mut next);
        assert_eq!(before, kinds(&p), "normalising twice changes nothing");
    }

    #[test]
    fn normalize_repairs_broken_layouts() {
        let mut next = 500;
        // a single column dissolves, its content moves back to the page body
        let mut p = page(vec![blk(1, BlockKind::Columns, "", 0), blk(2, BlockKind::Column, "", 1), blk(3, BlockKind::Text, "only", 2), blk(4, BlockKind::Text, "after", 0)]);
        normalize_layout(&mut p.blocks, &mut next);
        assert_eq!(kinds(&p), vec![(BlockKind::Text, 0), (BlockKind::Text, 0)]);
        // a stray block directly under Columns is pulled into the previous column
        let mut p = page(vec![blk(1, BlockKind::Columns, "", 0), blk(2, BlockKind::Column, "", 1), blk(3, BlockKind::Text, "a", 2), blk(4, BlockKind::Column, "", 1), blk(5, BlockKind::Text, "b", 2), blk(6, BlockKind::Text, "stray", 1), blk(7, BlockKind::Text, "end", 0)]);
        normalize_layout(&mut p.blocks, &mut next);
        assert_eq!(p.blocks[5].indent, 2);
        assert_eq!(p.blocks[5].text, "stray");
        // an empty column gets an empty block; an orphan Column becomes text; a page never ends inside a layout
        let mut p = page(vec![blk(1, BlockKind::Column, "", 1), blk(2, BlockKind::Columns, "", 0), blk(3, BlockKind::Column, "", 1), blk(4, BlockKind::Column, "", 1), blk(5, BlockKind::Text, "x", 2)]);
        normalize_layout(&mut p.blocks, &mut next);
        assert_eq!(p.blocks[0].kind, BlockKind::Text);
        let cols = p.blocks.iter().filter(|b| b.kind == BlockKind::Column).count();
        assert_eq!(cols, 2);
        assert!(p.blocks.iter().all(|b| b.kind != BlockKind::Column || b.indent == 1));
        assert_eq!(p.blocks.last().unwrap().indent, 0);
        assert_eq!(p.blocks.last().unwrap().kind, BlockKind::Text);
    }

    #[test]
    fn column_actions_add_remove_and_unwrap() {
        let mut p = page(vec![blk(1, BlockKind::Text, "/c", 0)]);
        let mut next = 100;
        let mut ed = EditorState::default();
        apply(&mut p, Act::Special(0, Slash::Columns(2)), &mut next, &mut ed);
        normalize_layout(&mut p.blocks, &mut next);
        let first_col = p.blocks.iter().position(|b| b.kind == BlockKind::Column).unwrap();
        apply(&mut p, Act::ColAdd(first_col), &mut next, &mut ed);
        normalize_layout(&mut p.blocks, &mut next);
        assert_eq!(p.blocks.iter().filter(|b| b.kind == BlockKind::Column).count(), 3);
        apply(&mut p, Act::ColDel(first_col), &mut next, &mut ed);
        normalize_layout(&mut p.blocks, &mut next);
        assert_eq!(p.blocks.iter().filter(|b| b.kind == BlockKind::Column).count(), 2);
        apply(&mut p, Act::ColUnwrap(0), &mut next, &mut ed);
        normalize_layout(&mut p.blocks, &mut next);
        assert!(p.blocks.iter().all(|b| !matches!(b.kind, BlockKind::Column | BlockKind::Columns)));
        assert!(p.blocks.iter().all(|b| b.indent == 0));
    }

    #[test]
    fn dragging_into_a_column_adopts_its_indent() {
        let mut p = page(vec![
            blk(1, BlockKind::Text, "move me", 0),
            blk(2, BlockKind::Columns, "", 0),
            blk(3, BlockKind::Column, "", 1),
            blk(4, BlockKind::Text, "a", 2),
            blk(5, BlockKind::Column, "", 1),
            blk(6, BlockKind::Text, "b", 2),
            blk(7, BlockKind::Text, "end", 0),
        ]);
        let mut next = 100;
        let mut ed = EditorState::default();
        apply(&mut p, Act::MoveAs(0, 5, 2), &mut next, &mut ed); // before "b"
        normalize_layout(&mut p.blocks, &mut next);
        let i = p.blocks.iter().position(|b| b.text == "move me").unwrap();
        assert_eq!(p.blocks[i].indent, 2);
        assert_eq!(p.blocks[i + 1].text, "b");
        assert_eq!(p.blocks[i - 1].kind, BlockKind::Column);
    }

    #[test]
    fn backspace_never_merges_across_a_column_boundary() {
        let mut p = page(vec![blk(1, BlockKind::Text, "before", 0), blk(2, BlockKind::Columns, "", 0), blk(3, BlockKind::Column, "", 1), blk(4, BlockKind::Text, "in column", 2), blk(5, BlockKind::Column, "", 1), blk(6, BlockKind::Text, "second", 2), blk(7, BlockKind::Text, "end", 0)]);
        let mut next = 100;
        let mut ed = EditorState::default();
        let n = p.blocks.len();
        apply(&mut p, Act::Merge(3), &mut next, &mut ed); // first block of column 1 vs. the Column marker
        apply(&mut p, Act::Merge(5), &mut next, &mut ed); // first block of column 2
        assert_eq!(p.blocks.len(), n);
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

//! Application shell: sidebar, top bar, command palette, dialogs, toasts and persistence.

use std::f32::consts::FRAC_PI_2;
use std::time::Duration;

use egui::{
    pos2, vec2, Align, Align2, Area, Color32, Context, CornerRadius, CursorIcon, Frame, Id, Key, Layout, Margin,
    Modifiers, Order, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui,
};
use serde::{Deserialize, Serialize};

use crate::auth::{self, now_ms, Credential};
use crate::editor::{self, EditorState, Req};
use crate::io as nio;
use crate::platform;
use crate::icons::{self, Icon};
use crate::lock::{LockEvent, LockScreen};
use crate::model::{Page, PageKind, Workspace, TEMPLATES};
use crate::theme::{self, bold, body, lerp_color, Palette, ACCENTS};
use crate::widgets::{
    appear, field, icon_button, menu_item, popup_area, primary_button, reset_appear, section_label, with_alpha,
};

const SIDEBAR_W: f32 = 264.0;
const STORAGE_KEY: &str = "lumen_v1";

#[derive(Serialize)]
struct SavedRef<'a> {
    ws: &'a Workspace,
    cred: &'a Option<Credential>,
    skipped: bool,
}

#[derive(Deserialize)]
struct Saved {
    ws: Workspace,
    cred: Option<Credential>,
    #[serde(default)]
    skipped: bool,
}

enum Ev {
    Open(u64),
    Toggle(u64),
    AddChild(u64),
    Menu(u64, Pos2),
}

#[derive(Clone, Copy)]
enum PalAct {
    Open(u64),
    NewPage,
    NewDb,
    Journal,
    Templates,
    Import,
    ExportMd,
    ExportCsv,
    Backup,
    Focus,
    Help,
    Theme,
    Lock,
    Sidebar,
    Trash,
}

struct PaletteState {
    query: String,
    sel: usize,
    focused: bool,
    /// When set, the palette is picking a page to link into this block.
    pick: Option<u64>,
}

#[derive(Default)]
struct ImportDialog {
    name: String,
    text: String,
}

#[derive(Default)]
struct SecurityDialog {
    cur: String,
    new: String,
    confirm: String,
    msg: Option<(String, bool)>,
}

struct Toast {
    msg: String,
    undo: Option<u64>,
    born: f64,
}

pub struct LumenApp {
    ws: Workspace,
    cred: Option<Credential>,
    skipped_setup: bool,
    locked: bool,
    lock: LockScreen,
    ed: EditorState,
    palette: Option<PaletteState>,
    row_menu: Option<(u64, Pos2)>,
    ws_menu: Option<Pos2>,
    trash_open: bool,
    security: Option<SecurityDialog>,
    fresh: bool,
    toasts: Vec<Toast>,
    last_theme_t: f32,
    theme_applied: bool,
    last_active: f64,
    shown_page: Option<u64>,
    focus_title: Option<u64>,
    import: Option<ImportDialog>,
    templates_open: bool,
    help_open: bool,
    page_menu: Option<Pos2>,
    focus_mode: bool,
    last_accent: usize,
}

impl LumenApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::setup_fonts(&cc.egui_ctx);
        let saved: Option<Saved> = cc.storage.and_then(|s| eframe::get_value(s, STORAGE_KEY));
        let (ws, cred, skipped) = match saved {
            Some(s) => (s.ws, s.cred, s.skipped),
            None => (Workspace::seed(), None, false),
        };
        let mut ws = ws;
        ws.migrate();
        if ws.current.map_or(true, |c| ws.page(c).is_none()) {
            ws.current = ws.pages.iter().find(|p| !p.trashed && !p.row).map(|p| p.id);
        }
        let locked = cred.is_some() || !skipped;
        Self {
            ws,
            cred,
            skipped_setup: skipped,
            locked,
            lock: LockScreen::new(),
            ed: EditorState::default(),
            palette: None,
            row_menu: None,
            ws_menu: None,
            trash_open: false,
            security: None,
            fresh: false,
            toasts: vec![],
            last_theme_t: -1.0,
            theme_applied: false,
            last_active: now_ms() / 1000.0,
            shown_page: None,
            focus_title: None,
            import: None,
            templates_open: false,
            help_open: false,
            page_menu: None,
            focus_mode: false,
            last_accent: usize::MAX,
        }
    }

    fn toast(&mut self, msg: impl Into<String>, undo: Option<u64>) {
        self.toasts.push(Toast { msg: msg.into(), undo, born: now_ms() / 1000.0 });
    }

    fn new_page(&mut self, parent: Option<u64>, kind: PageKind) {
        let id = self.ws.add_page(parent, kind);
        self.focus_title = Some(id);
    }

    fn link_block(&mut self, page: u64, bid: u64, target: u64) {
        if let Some(b) = self.ws.page_mut(page).and_then(|p| p.blocks.iter_mut().find(|b| b.id == bid)) {
            b.link = Some(target);
        }
    }

    fn handle_req(&mut self, req: Req, cur: u64) {
        match req {
            Req::NewPage(bid) => {
                let id = self.ws.add_page(Some(cur), PageKind::Doc);
                self.ws.current = Some(cur);
                self.link_block(cur, bid, id);
                self.toast("Sub-page created — click the link to open it", None);
            }
            Req::NewDb(bid, vk) => {
                let id = self.ws.add_database(Some(cur), vk);
                self.ws.current = Some(cur);
                self.link_block(cur, bid, id);
                self.toast("Database created — click the link to open it", None);
            }
            Req::PickLink(bid) => {
                self.palette = Some(PaletteState { query: String::new(), sel: 0, focused: false, pick: Some(bid) });
            }
        }
    }

    fn export_md(&mut self) {
        let Some(cur) = self.ws.current else { return };
        let md = nio::page_markdown(&self.ws, cur);
        let name = format!("{}.md", nio::file_safe(&self.ws.display_title(cur)));
        let msg = platform::save_download(&name, md.as_bytes(), "text/markdown");
        self.toast(msg, None);
    }

    fn export_csv(&mut self) {
        let Some(cur) = self.ws.current else { return };
        if self.ws.db(cur).is_none() {
            self.toast("Open a database to export it as CSV", None);
            return;
        }
        let csv = nio::db_csv(&self.ws, cur);
        let name = format!("{}.csv", nio::file_safe(&self.ws.display_title(cur)));
        let msg = platform::save_download(&name, csv.as_bytes(), "text/csv");
        self.toast(msg, None);
    }

    fn backup(&mut self) {
        match serde_json::to_vec_pretty(&self.ws) {
            Ok(b) => {
                let msg = platform::save_download("lumen-backup.json", &b, "application/json");
                self.toast(msg, None);
            }
            Err(_) => self.toast("Could not create backup", None),
        }
    }

    fn import_text(&mut self, name: &str, text: &str, csv: bool) {
        let name = name.trim();
        let name = if name.is_empty() { "Imported" } else { name };
        if csv {
            match nio::import_csv(&mut self.ws, name, text, None) {
                Some(id) => {
                    self.ws.current = Some(id);
                    self.toast("Imported as a database", None);
                }
                None => self.toast("That doesn't look like CSV (need a header and at least one row)", None),
            }
        } else {
            let id = nio::import_markdown(&mut self.ws, name, text, None);
            self.ws.current = Some(id);
            self.toast("Imported as a page", None);
        }
    }

    fn lock_now(&mut self) {
        if self.cred.is_some() {
            self.locked = true;
            self.lock.clear();
            self.palette = None;
            self.row_menu = None;
            self.ws_menu = None;
            self.security = None;
            self.trash_open = false;
        }
    }

    fn run(&mut self, a: PalAct) {
        match a {
            PalAct::Open(id) => self.ws.current = Some(id),
            PalAct::NewPage => self.new_page(None, PageKind::Doc),
            PalAct::NewDb => self.new_page(None, PageKind::Database),
            PalAct::Journal => self.ws.open_today(),
            PalAct::Templates => self.templates_open = true,
            PalAct::Import => self.import = Some(ImportDialog::default()),
            PalAct::ExportMd => self.export_md(),
            PalAct::ExportCsv => self.export_csv(),
            PalAct::Backup => self.backup(),
            PalAct::Help => self.help_open = true,
            PalAct::Focus => {
                self.focus_mode = !self.focus_mode;
                if self.focus_mode {
                    self.toast("Focus mode — press Ctrl+. to exit", None);
                }
            }
            PalAct::Theme => self.ws.dark = !self.ws.dark,
            PalAct::Lock => {
                if self.cred.is_some() {
                    self.lock_now();
                } else {
                    self.security = Some(SecurityDialog::default());
                }
            }
            PalAct::Sidebar => self.ws.sidebar_open = !self.ws.sidebar_open,
            PalAct::Trash => self.trash_open = true,
        }
    }

    // ───────────── sidebar ─────────────

    fn sidebar(&mut self, ui: &mut Ui, pal: &Palette) {
        let w = SIDEBAR_W - 16.0;
        let mut ev: Vec<Ev> = vec![];
        ui.spacing_mut().item_spacing = vec2(0.0, 1.0);
        ui.add_space(10.0);

        // workspace header
        let (_, hr) = ui.allocate_space(vec2(w, 36.0));
        let hresp = ui.interact(hr, Id::new("ws_header"), Sense::click());
        let ht = ui.ctx().animate_bool_with_time(hresp.id, ui.rect_contains_pointer(hr), 0.1);
        ui.painter().rect_filled(hr, 7.0, with_alpha(pal.hover, ht));
        let av = Rect::from_center_size(pos2(hr.left() + 20.0, hr.center().y), vec2(24.0, 24.0));
        ui.painter().rect_filled(av, 7.0, pal.accent);
        let letter = self.ws.name.chars().next().unwrap_or('L').to_uppercase().to_string();
        ui.painter().text(av.center(), Align2::CENTER_CENTER, letter, bold(13.0), Color32::WHITE);
        let name_clip = Rect::from_min_max(pos2(hr.left() + 40.0, hr.top()), pos2(hr.right() - 40.0, hr.bottom()));
        ui.painter().with_clip_rect(name_clip).text(
            pos2(hr.left() + 40.0, hr.center().y),
            Align2::LEFT_CENTER,
            &self.ws.name,
            bold(14.5),
            pal.text,
        );
        let cr = Rect::from_center_size(pos2(hr.right() - 18.0, hr.center().y), vec2(26.0, 26.0));
        let cresp = ui.interact(cr, Id::new("sb_collapse"), Sense::click());
        if ui.rect_contains_pointer(hr) {
            let h = ui.ctx().animate_bool_with_time(cresp.id, cresp.hovered(), 0.1);
            ui.painter().rect_filled(cr, 6.0, with_alpha(pal.active, h));
            icons::paint(ui.painter(), cr.center(), 15.0, Icon::ChevronLeft, pal.muted);
        }
        if cresp.on_hover_text("Close sidebar  (Ctrl+\\)").clicked() {
            self.ws.sidebar_open = false;
        } else if hresp.clicked() {
            self.ws_menu = Some(hr.left_bottom() + vec2(0.0, 4.0));
            self.fresh = true;
        }
        ui.add_space(8.0);

        // quick actions
        if simple_row(ui, pal, "sr_search", w, Icon::Search, "Search", Some("Ctrl K")) {
            self.palette = Some(PaletteState { query: String::new(), sel: 0, focused: false, pick: None });
        }
        if simple_row(ui, pal, "sr_new", w, Icon::Plus, "New page", None) {
            self.new_page(None, PageKind::Doc);
        }
        if simple_row(ui, pal, "sr_board", w, Icon::Board, "New database", None) {
            self.new_page(None, PageKind::Database);
        }
        if simple_row(ui, pal, "sr_journal", w, Icon::Page, "Today's journal", Some("Ctrl J")) {
            self.run(PalAct::Journal);
        }
        if simple_row(ui, pal, "sr_tpl", w, Icon::Duplicate, "Templates", None) {
            self.templates_open = true;
        }
        ui.add_space(8.0);

        let footer_h = 78.0;
        let h = (ui.available_height() - footer_h).max(80.0);
        ScrollArea::vertical().id_salt("side_scroll").max_height(h).auto_shrink([false, false]).show(ui, |ui| {
            ui.set_width(w);
            let favs: Vec<u64> = self.ws.pages.iter().filter(|p| p.favorite && !p.trashed).map(|p| p.id).collect();
            if !favs.is_empty() {
                section_label(ui, pal, "Favorites");
                for id in favs {
                    let Some(p) = self.ws.page(id) else { continue };
                    let out = page_row(ui, pal, Id::new(("fav", id)), w, 0, p, self.ws.current == Some(id), false);
                    if out.click {
                        ev.push(Ev::Open(id));
                    }
                    if let Some(pos) = out.more {
                        ev.push(Ev::Menu(id, pos));
                    }
                    if out.plus {
                        ev.push(Ev::AddChild(id));
                    }
                }
                ui.add_space(6.0);
            }
            section_label(ui, pal, "Pages");
            self.tree(ui, pal, w, None, 0, &mut ev);
            if self.ws.children(None).is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new("  No pages yet").color(pal.muted).font(body(13.0)));
            }
        });

        // footer
        ui.add_space(4.0);
        if simple_row(ui, pal, "sr_trash", w, Icon::Trash, "Trash", None) {
            self.trash_open = true;
        }
        let (lbl, ic) = if self.cred.is_some() { ("Lock workspace", Icon::Lock) } else { ("Set a password", Icon::Shield) };
        if simple_row(ui, pal, "sr_lock", w, ic, lbl, if self.cred.is_some() { Some("Ctrl L") } else { None }) {
            self.run(PalAct::Lock);
        }

        for e in ev {
            match e {
                Ev::Open(id) => self.ws.current = Some(id),
                Ev::Toggle(id) => {
                    if let Some(p) = self.ws.page_mut(id) {
                        p.expanded = !p.expanded;
                    }
                }
                Ev::AddChild(id) => self.new_page(Some(id), PageKind::Doc),
                Ev::Menu(id, pos) => {
                    self.row_menu = Some((id, pos));
                    self.fresh = true;
                }
            }
        }
    }

    fn tree(&self, ui: &mut Ui, pal: &Palette, w: f32, parent: Option<u64>, depth: usize, ev: &mut Vec<Ev>) {
        for id in self.ws.children(parent) {
            let Some(p) = self.ws.page(id) else { continue };
            let has = self.ws.has_children(id);
            let key = Id::new(("srow", id));
            let out = page_row(ui, pal, key, w, depth, p, self.ws.current == Some(id), has);
            if out.click {
                ev.push(Ev::Open(id));
            }
            if out.arrow {
                ev.push(Ev::Toggle(id));
            }
            if out.plus {
                ev.push(Ev::AddChild(id));
            }
            if let Some(pos) = out.more {
                ev.push(Ev::Menu(id, pos));
            }
            let t = ui.ctx().animate_bool_with_time(key.with("kids"), p.expanded && has, 0.18);
            if t > 0.0 {
                ui.scope(|ui| {
                    ui.set_opacity(t);
                    self.tree(ui, pal, w, Some(id), depth + 1, ev);
                });
            }
        }
    }

    // ───────────── top bar ─────────────

    fn topbar(&mut self, ui: &mut Ui, pal: &Palette) {
        ui.add_space(8.0);
        if icon_button(ui, pal, Icon::Menu, 16.0, "Toggle sidebar  (Ctrl+\\)").clicked() {
            self.ws.sidebar_open = !self.ws.sidebar_open;
        }
        let Some(cur) = self.ws.current else { return };
        let crumbs = self.ws.breadcrumb(cur);
        let n = crumbs.len();
        let mut nav: Option<u64> = None;
        for (i, id) in crumbs.iter().enumerate() {
            if let Some(p) = self.ws.page(*id) {
                let title = if p.title.is_empty() { "Untitled" } else { &p.title };
                if crumb(ui, pal, &p.icon, title, i + 1 == n).clicked() {
                    nav = Some(*id);
                }
                if i + 1 < n {
                    ui.label(RichText::new("/").color(with_alpha(pal.muted, 0.6)).font(body(14.0)));
                }
            }
        }
        if let Some(id) = nav {
            self.ws.current = Some(id);
        }
        let (updated, fav) = self.ws.page(cur).map(|p| (p.updated, p.favorite)).unwrap_or((0.0, false));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(10.0);
            let r = icon_button(ui, pal, Icon::Dots, 16.0, "Page options");
            if r.clicked() {
                self.page_menu = Some(r.rect.left_bottom() + vec2(-220.0, 6.0));
                self.fresh = true;
            }
            let (lock_icon, tip) = if self.cred.is_some() { (Icon::Lock, "Lock workspace  (Ctrl+L)") } else { (Icon::Shield, "Set a password") };
            if icon_button(ui, pal, lock_icon, 16.0, tip).clicked() {
                self.run(PalAct::Lock);
            }
            let (ti, tt) = if self.ws.dark { (Icon::Sun, "Switch to light mode") } else { (Icon::Moon, "Switch to dark mode") };
            if icon_button(ui, pal, ti, 16.0, tt).clicked() {
                self.ws.dark = !self.ws.dark;
            }
            if icon_button(ui, pal, Icon::Search, 16.0, "Search  (Ctrl+K)").clicked() {
                self.palette = Some(PaletteState { query: String::new(), sel: 0, focused: false, pick: None });
            }
            let (si, st) = if fav { (Icon::StarFilled, "Remove from favorites") } else { (Icon::Star, "Add to favorites") };
            let r = icon_button(ui, pal, si, 16.0, st);
            if r.clicked() {
                if let Some(p) = self.ws.page_mut(cur) {
                    p.favorite = !p.favorite;
                }
            }
            ui.add_space(6.0);
            ui.label(RichText::new(format!("Edited {}", ago(updated))).color(pal.muted).font(body(12.5)));
        });
    }

    // ───────────── overlays ─────────────

    fn palette_items(&self, q: &str) -> Vec<(PalAct, String, String, Option<String>)> {
        let q = q.trim().to_lowercase();
        let mut out = vec![];
        let title_of = |p: &Page| if p.title.is_empty() { "Untitled".to_owned() } else { p.title.clone() };
        let icon_of = |p: &Page| if p.icon.is_empty() { "▤".to_owned() } else { p.icon.clone() };
        if q.is_empty() {
            let mut pages: Vec<&Page> = self.ws.pages.iter().filter(|p| !p.trashed).collect();
            pages.sort_by(|a, b| b.updated.partial_cmp(&a.updated).unwrap_or(std::cmp::Ordering::Equal));
            for p in pages.into_iter().take(6) {
                out.push((PalAct::Open(p.id), icon_of(p), title_of(p), Some(format!("Edited {}", ago(p.updated)))));
            }
        } else {
            let mut scored: Vec<(i32, PalAct, String, String, Option<String>)> = vec![];
            for p in self.ws.pages.iter().filter(|p| !p.trashed) {
                let t = title_of(p);
                let tl = t.to_lowercase();
                if tl.starts_with(&q) {
                    scored.push((0, PalAct::Open(p.id), icon_of(p), t, None));
                } else if tl.contains(&q) {
                    scored.push((1, PalAct::Open(p.id), icon_of(p), t, None));
                } else {
                    let mut snippet = None;
                    for b in &p.blocks {
                        if let Some(pos) = b.text.to_lowercase().find(&q) {
                            let start = b.text[..pos].char_indices().rev().nth(20).map(|(i, _)| i).unwrap_or(0);
                            let s: String = b.text[start..].chars().take(70).collect();
                            snippet = Some(format!("…{s}"));
                            break;
                        }
                    }
                    if snippet.is_none() {
                        'o: for c in &p.columns {
                            for card in &c.cards {
                                if card.text.to_lowercase().contains(&q) {
                                    snippet = Some(format!("{} · {}", c.title, card.text));
                                    break 'o;
                                }
                            }
                        }
                    }
                    if let Some(s) = snippet {
                        scored.push((2, PalAct::Open(p.id), icon_of(p), t, Some(s)));
                    }
                }
            }
            scored.sort_by_key(|s| s.0);
            out.extend(scored.into_iter().take(8).map(|(_, a, i, t, s)| (a, i, t, s)));
        }
        let actions: [(PalAct, &str, &str, &str); 14] = [
            (PalAct::NewPage, "+", "New page", "Create a blank page"),
            (PalAct::NewDb, "▥", "New database", "Table, board, list, gallery or calendar"),
            (PalAct::Journal, "📖", "Today's journal", "Open or create today's entry  (Ctrl+J)"),
            (PalAct::Templates, "▦", "Browse templates", "Meeting notes, trackers, wiki…"),
            (PalAct::Import, "⇩", "Import…", "Markdown, CSV or a workspace backup"),
            (PalAct::ExportMd, "⇧", "Export page as Markdown", "Download the current page"),
            (PalAct::ExportCsv, "⇧", "Export database as CSV", "Download the current database"),
            (PalAct::Backup, "📦", "Back up workspace", "Download everything as JSON"),
            (PalAct::Focus, "◎", "Toggle focus mode", "Hide sidebar and top bar  (Ctrl+.)"),
            (PalAct::Theme, "◐", "Toggle theme", "Switch between light and dark"),
            (PalAct::Lock, "🔒", "Lock workspace", "Require your password again"),
            (PalAct::Sidebar, "◧", "Toggle sidebar", "Show or hide the sidebar"),
            (PalAct::Trash, "🗑", "Open trash", "Restore deleted pages"),
            (PalAct::Help, "⌨", "Keyboard shortcuts", "Everything you can do without the mouse  (Ctrl+/)"),
        ];
        for (a, i, t, s) in actions {
            if q.is_empty() || t.to_lowercase().contains(&q) {
                out.push((a, i.to_owned(), t.to_owned(), Some(s.to_owned())));
            }
        }
        out
    }

    fn palette_ui(&mut self, ctx: &Context, pal: &Palette) {
        let Some(mut st) = self.palette.take() else { return };
        let mut items = self.palette_items(&st.query);
        if st.pick.is_some() {
            items.retain(|i| matches!(i.0, PalAct::Open(_)));
        }
        let len = items.len();
        if len > 0 {
            st.sel = st.sel.min(len - 1);
        }
        let mut exec: Option<PalAct> = None;
        let st_pick = st.pick;
        if len > 0 {
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                st.sel = (st.sel + 1) % len;
            }
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
                st.sel = (st.sel + len - 1) % len;
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) && len > 0 {
            exec = Some(items[st.sel].0);
        }
        let open = modal(ctx, Id::new("palette"), pal, 600.0, 96.0, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                let (r, _) = ui.allocate_exact_size(vec2(20.0, 36.0), Sense::hover());
                icons::paint(ui.painter(), r.center(), 16.0, Icon::Search, pal.muted);
                let id = Id::new("pal_input");
                if !st.focused {
                    ui.ctx().memory_mut(|m| m.request_focus(id));
                    st.focused = true;
                }
                let before = st.query.clone();
                ui.add(
                    TextEdit::singleline(&mut st.query)
                        .id(id)
                        .frame(false)
                        .font(body(18.0))
                        .desired_width(520.0)
                        .margin(Margin::symmetric(4, 10))
                        .hint_text(RichText::new(if st.pick.is_some() { "Link to which page?" } else { "Search pages or run a command…" }).color(with_alpha(pal.muted, 0.8))),
                );
                if before != st.query {
                    st.sel = 0;
                }
            });
            let y = ui.cursor().top();
            ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0_f32, pal.border));
            ui.add_space(6.0);
            if items.is_empty() {
                ui.add_space(18.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("No results").color(pal.muted)));
                ui.add_space(18.0);
            }
            let mut last_page = true;
            ScrollArea::vertical().max_height(380.0).auto_shrink([false, true]).show(ui, |ui| {
                for (i, (act, lead, title, sub)) in items.iter().enumerate() {
                    if last_page && !matches!(act, PalAct::Open(_)) {
                        last_page = false;
                        section_label(ui, pal, "COMMANDS");
                    }
                    let r = menu_item(ui, pal, lead, title, sub.as_deref(), i == st.sel, false);
                    if r.hovered() && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                        st.sel = i;
                    }
                    if r.clicked() {
                        exec = Some(*act);
                    }
                }
            });
        });
        if open && exec.is_none() {
            self.palette = Some(st);
        } else {
            reset_appear(ctx, Id::new("palette").with("a"));
            if let Some(a) = exec {
                match (st_pick, a) {
                    (Some(bid), PalAct::Open(t)) => {
                        if let Some(cur) = self.ws.current {
                            self.link_block(cur, bid, t);
                        }
                    }
                    (Some(_), _) => {}
                    (None, a) => self.run(a),
                }
            }
        }
    }

    fn row_menu_ui(&mut self, ctx: &Context, pal: &Palette) {
        let Some((id, pos)) = self.row_menu else { return };
        let Some(page) = self.ws.page(id) else {
            self.row_menu = None;
            return;
        };
        let fav = page.favorite;
        let mut act: Option<u8> = None;
        let open = popup_area(ctx, Id::new("row_menu"), pos, pal, &mut self.fresh, 220.0, |ui| {
            if menu_item(ui, pal, if fav { "☆" } else { "★" }, if fav { "Remove from favorites" } else { "Add to favorites" }, None, false, false).clicked() {
                act = Some(0);
            }
            if menu_item(ui, pal, "⎘", "Duplicate", None, false, false).clicked() {
                act = Some(1);
            }
            if menu_item(ui, pal, "+", "Add sub-page", None, false, false).clicked() {
                act = Some(2);
            }
            ui.add_space(3.0);
            ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0_f32, pal.border));
            ui.add_space(4.0);
            if menu_item(ui, pal, "🗑", "Move to trash", None, false, true).clicked() {
                act = Some(3);
            }
        });
        if !open || act.is_some() {
            self.row_menu = None;
        }
        match act {
            Some(0) => {
                if let Some(p) = self.ws.page_mut(id) {
                    p.favorite = !p.favorite;
                }
            }
            Some(1) => {
                self.ws.duplicate(id);
            }
            Some(2) => self.new_page(Some(id), PageKind::Doc),
            Some(3) => {
                self.ws.set_trashed(id, true);
                self.toast("Moved to trash", Some(id));
            }
            _ => {}
        }
    }

    fn ws_menu_ui(&mut self, ctx: &Context, pal: &Palette) {
        let Some(pos) = self.ws_menu else { return };
        let mut act: Option<u8> = None;
        let has_pw = self.cred.is_some();
        let dark = self.ws.dark;
        let accent = self.ws.accent;
        let mut acc_pick: Option<usize> = None;
        let name = &mut self.ws.name;
        let open = popup_area(ctx, Id::new("ws_menu"), pos, pal, &mut self.fresh, 286.0, |ui| {
            section_label(ui, pal, "WORKSPACE");
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.add(TextEdit::singleline(name).font(bold(15.0)).frame(false).desired_width(220.0).margin(Margin::symmetric(0, 4)));
            });
            ui.add_space(3.0);
            ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0_f32, pal.border));
            ui.add_space(4.0);
            if menu_item(ui, pal, "◐", if dark { "Light mode" } else { "Dark mode" }, None, false, false).clicked() {
                act = Some(0);
            }
            if has_pw && menu_item(ui, pal, "🔒", "Lock now", None, false, false).clicked() {
                act = Some(1);
            }
            if menu_item(ui, pal, "🔑", if has_pw { "Change or remove password…" } else { "Set a password…" }, None, false, false).clicked() {
                act = Some(2);
            }
            if menu_item(ui, pal, "▦", "Templates…", None, false, false).clicked() {
                act = Some(4);
            }
            if menu_item(ui, pal, "⇩", "Import Markdown / CSV / backup…", None, false, false).clicked() {
                act = Some(5);
            }
            if menu_item(ui, pal, "📦", "Back up workspace (download JSON)", None, false, false).clicked() {
                act = Some(6);
            }
            if menu_item(ui, pal, "⎘", "Copy workspace as JSON", None, false, false).clicked() {
                act = Some(3);
            }
            if menu_item(ui, pal, "⌨", "Keyboard shortcuts", None, false, false).clicked() {
                act = Some(7);
            }
            ui.add_space(3.0);
            ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0_f32, pal.border));
            ui.add_space(4.0);
            section_label(ui, pal, "ACCENT COLOUR");
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                for (i, (nm, (r, g, b))) in ACCENTS.iter().enumerate() {
                    let (rc, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
                    let col = Color32::from_rgb(*r, *g, *b);
                    ui.painter().circle_filled(rc.center(), 9.0 + if resp.hovered() { 1.5 } else { 0.0 }, col);
                    if accent == i {
                        ui.painter().circle_stroke(rc.center(), 12.0, Stroke::new(1.6_f32, pal.text));
                    }
                    if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(*nm).clicked() {
                        acc_pick = Some(i);
                    }
                }
            });
        });
        if let Some(i) = acc_pick {
            self.ws.accent = i;
        }
        if !open || act.is_some() {
            self.ws_menu = None;
        }
        match act {
            Some(0) => self.ws.dark = !self.ws.dark,
            Some(1) => self.lock_now(),
            Some(2) => self.security = Some(SecurityDialog::default()),
            Some(3) => {
                if let Ok(json) = serde_json::to_string_pretty(&self.ws) {
                    ctx.copy_text(json);
                    self.toast("Workspace JSON copied to clipboard", None);
                }
            }
            Some(4) => self.templates_open = true,
            Some(5) => self.import = Some(ImportDialog::default()),
            Some(6) => self.backup(),
            Some(7) => self.help_open = true,
            _ => {}
        }
    }

    fn trash_ui(&mut self, ctx: &Context, pal: &Palette) {
        if !self.trash_open {
            return;
        }
        let trashed: Vec<(u64, String, String)> = self
            .ws
            .pages
            .iter()
            .filter(|p| p.trashed && !p.parent.and_then(|pp| self.ws.page(pp)).map_or(false, |pp| pp.trashed))
            .map(|p| (p.id, if p.title.is_empty() { "Untitled".into() } else { p.title.clone() }, p.icon.clone()))
            .collect();
        let mut restore: Option<u64> = None;
        let mut delete: Option<u64> = None;
        let mut empty = false;
        let open = modal(ctx, Id::new("trash"), pal, 480.0, 120.0, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new("Trash").font(bold(18.0)));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(8.0);
                    if !trashed.is_empty() && crate::widgets::ghost_button(ui, pal, "Empty trash").clicked() {
                        empty = true;
                    }
                });
            });
            ui.add_space(6.0);
            if trashed.is_empty() {
                ui.add_space(26.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("🗑").font(body(34.0)).color(pal.muted));
                    ui.label(RichText::new("Trash is empty").color(pal.muted));
                });
                ui.add_space(30.0);
            }
            ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
                for (id, title, icon) in &trashed {
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
                    let t = ui.ctx().animate_bool_with_time(Id::new(("trash_row", *id)), ui.rect_contains_pointer(r), 0.1);
                    ui.painter().rect_filled(r, 7.0, with_alpha(pal.hover, t));
                    ui.painter().text(pos2(r.left() + 20.0, r.center().y), Align2::CENTER_CENTER, if icon.is_empty() { "▤" } else { icon }, body(16.0), pal.text);
                    ui.painter().text(pos2(r.left() + 42.0, r.center().y), Align2::LEFT_CENTER, title, body(14.5), pal.text);
                    let _ = resp;
                    let rr = Rect::from_center_size(pos2(r.right() - 48.0, r.center().y), vec2(28.0, 28.0));
                    let dr = Rect::from_center_size(pos2(r.right() - 16.0, r.center().y), vec2(28.0, 28.0));
                    let a = ui.interact(rr, Id::new(("restore", *id)), Sense::click());
                    let b = ui.interact(dr, Id::new(("purge", *id)), Sense::click());
                    crate::widgets::paint_icon_button(ui, pal, rr, &a, Icon::Restore, 15.0);
                    crate::widgets::paint_icon_button(ui, pal, dr, &b, Icon::Close, 15.0);
                    if a.on_hover_text("Restore").clicked() {
                        restore = Some(*id);
                    }
                    if b.on_hover_text("Delete forever").clicked() {
                        delete = Some(*id);
                    }
                }
            });
            ui.add_space(6.0);
        });
        if let Some(id) = restore {
            self.ws.set_trashed(id, false);
            self.ws.current = Some(id);
            self.toast("Page restored", None);
        }
        if let Some(id) = delete {
            self.ws.delete_forever(id);
        }
        if empty {
            let ids: Vec<u64> = self.ws.pages.iter().filter(|p| p.trashed).map(|p| p.id).collect();
            for id in ids {
                self.ws.delete_forever(id);
            }
        }
        if !open {
            self.trash_open = false;
            reset_appear(ctx, Id::new("trash").with("a"));
        }
    }

    fn page_menu_ui(&mut self, ctx: &Context, pal: &Palette) {
        let Some(pos) = self.page_menu else { return };
        let Some(cur) = self.ws.current else {
            self.page_menu = None;
            return;
        };
        let Some(page) = self.ws.page(cur) else {
            self.page_menu = None;
            return;
        };
        let (small, full, is_db) = (page.small_text, page.full_width, page.db.is_some());
        let (words, chars) = (nio::word_count(page), nio::char_count(page));
        let mut act: Option<u8> = None;
        let open = popup_area(ctx, Id::new("page_menu"), pos, pal, &mut self.fresh, 262.0, |ui| {
            section_label(ui, pal, "PAGE STYLE");
            if menu_item(ui, pal, "Aa", if small { "Normal text size" } else { "Small text" }, None, false, false).clicked() {
                act = Some(0);
            }
            if menu_item(ui, pal, "↔", if full { "Standard page width" } else { "Full width" }, None, false, false).clicked() {
                act = Some(1);
            }
            ui.add_space(3.0);
            ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0_f32, pal.border));
            ui.add_space(4.0);
            if menu_item(ui, pal, "⎘", "Duplicate", None, false, false).clicked() {
                act = Some(2);
            }
            if menu_item(ui, pal, "⇧", "Export as Markdown", Some("Downloads a .md file"), false, false).clicked() {
                act = Some(3);
            }
            if menu_item(ui, pal, "⎘", "Copy as Markdown", None, false, false).clicked() {
                act = Some(4);
            }
            if is_db && menu_item(ui, pal, "⇧", "Export as CSV", Some("Downloads a .csv file"), false, false).clicked() {
                act = Some(5);
            }
            ui.add_space(3.0);
            ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0_f32, pal.border));
            ui.add_space(4.0);
            if !is_db {
                let mins = ((words as f32) / 220.0).ceil().max(1.0) as usize;
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(RichText::new(format!("{words} words · {chars} characters · ~{mins} min read")).font(body(12.0)).color(pal.muted));
                });
                ui.add_space(4.0);
            }
            if menu_item(ui, pal, "🗑", "Move to trash", None, false, true).clicked() {
                act = Some(6);
            }
        });
        if !open || act.is_some() {
            self.page_menu = None;
        }
        match act {
            Some(0) => {
                if let Some(p) = self.ws.page_mut(cur) {
                    p.small_text = !p.small_text;
                }
            }
            Some(1) => {
                if let Some(p) = self.ws.page_mut(cur) {
                    p.full_width = !p.full_width;
                }
            }
            Some(2) => {
                self.ws.duplicate(cur);
            }
            Some(3) => self.export_md(),
            Some(4) => {
                ctx.copy_text(nio::page_markdown(&self.ws, cur));
                self.toast("Markdown copied to clipboard", None);
            }
            Some(5) => self.export_csv(),
            Some(6) => {
                self.ws.set_trashed(cur, true);
                self.toast("Moved to trash", Some(cur));
            }
            _ => {}
        }
    }

    fn templates_ui(&mut self, ctx: &Context, pal: &Palette) {
        if !self.templates_open {
            return;
        }
        let mut pick: Option<usize> = None;
        let open = modal(ctx, Id::new("templates"), pal, 640.0, 90.0, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new("Templates").font(bold(20.0)));
            });
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new("Start from a ready-made page or database. Everything is editable.").font(body(13.5)).color(pal.muted));
            });
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
                for (i, (name, icon, desc)) in TEMPLATES.iter().enumerate() {
                    let (r, resp) = ui.allocate_exact_size(vec2(296.0, 74.0), Sense::click());
                    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
                    ui.painter().rect_filled(r, 10.0, with_alpha(pal.hover, 0.5 + 0.5 * t));
                    ui.painter().rect_stroke(r, 10.0, Stroke::new(1.2_f32, lerp_color(pal.border, pal.accent, t)), egui::StrokeKind::Inside);
                    ui.painter().text(pos2(r.left() + 30.0, r.center().y), Align2::CENTER_CENTER, *icon, body(28.0), pal.text);
                    ui.painter().text(pos2(r.left() + 62.0, r.center().y - 11.0), Align2::LEFT_CENTER, *name, bold(14.5), pal.text);
                    ui.painter().with_clip_rect(r.shrink(6.0)).text(pos2(r.left() + 62.0, r.center().y + 10.0), Align2::LEFT_CENTER, *desc, body(12.0), pal.muted);
                    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        pick = Some(i);
                    }
                }
            });
            ui.add_space(8.0);
        });
        if let Some(i) = pick {
            let id = self.ws.add_template(i, None);
            self.focus_title = Some(id);
            self.templates_open = false;
            reset_appear(ctx, Id::new("templates").with("a"));
        } else if !open {
            self.templates_open = false;
            reset_appear(ctx, Id::new("templates").with("a"));
        }
    }

    fn import_ui(&mut self, ctx: &Context, pal: &Palette) {
        let Some(mut dlg) = self.import.take() else { return };
        let mut action: Option<u8> = None;
        let open = modal(ctx, Id::new("import"), pal, 620.0, 80.0, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new("Import").font(bold(20.0)));
            });
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new("Paste Markdown, CSV or a Lumen backup — or just drop a file onto the window.").font(body(13.5)).color(pal.muted));
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                field(ui, pal, pal.bg, &mut dlg.name, "Name of the new page or database", false, 580.0, Id::new("imp_name"));
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                Frame::new().fill(pal.bg).stroke(Stroke::new(1.0_f32, pal.border)).corner_radius(CornerRadius::same(9)).inner_margin(Margin::same(10)).show(ui, |ui| {
                    ScrollArea::vertical().max_height(230.0).show(ui, |ui| {
                        ui.add(TextEdit::multiline(&mut dlg.text).desired_width(556.0).desired_rows(11).frame(false).font(egui::FontId::monospace(13.0)).hint_text("# My notes\n\n- paste your content here"));
                    });
                });
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                if primary_button(ui, pal, "Markdown → page", 190.0).clicked() {
                    action = Some(0);
                }
                if primary_button(ui, pal, "CSV → database", 170.0).clicked() {
                    action = Some(1);
                }
                if crate::widgets::ghost_button(ui, pal, "Restore backup (replaces all)").clicked() {
                    action = Some(2);
                }
            });
            ui.add_space(8.0);
        });
        let mut keep = open;
        match action {
            Some(0) | Some(1) if dlg.text.trim().is_empty() => self.toast("Paste some content first", None),
            Some(0) => {
                self.import_text(&dlg.name, &dlg.text, false);
                keep = false;
            }
            Some(1) => {
                self.import_text(&dlg.name, &dlg.text, true);
                keep = false;
            }
            Some(2) => match serde_json::from_str::<Workspace>(&dlg.text) {
                Ok(mut w) => {
                    w.migrate();
                    self.ws = w;
                    self.toast("Workspace restored from backup", None);
                    keep = false;
                }
                Err(_) => self.toast("That isn't a valid Lumen backup", None),
            },
            _ => {}
        }
        if keep {
            self.import = Some(dlg);
        } else {
            reset_appear(ctx, Id::new("import").with("a"));
        }
    }

    fn help_ui(&mut self, ctx: &Context, pal: &Palette) {
        if !self.help_open {
            return;
        }
        let groups: [(&str, &[(&str, &str)]); 3] = [
            (
                "GENERAL",
                &[
                    ("Ctrl K  /  Ctrl P", "Search & command palette"),
                    ("Ctrl N", "New page"),
                    ("Ctrl J", "Today's journal"),
                    ("Ctrl \\", "Toggle sidebar"),
                    ("Ctrl .", "Focus mode"),
                    ("Ctrl L", "Lock workspace"),
                    ("Ctrl Shift L", "Light / dark theme"),
                    ("Ctrl Shift E", "Export page as Markdown"),
                    ("Ctrl /", "This help"),
                ],
            ),
            (
                "EDITING",
                &[
                    ("/", "Insert any block or database"),
                    ("Enter  /  Shift Enter", "New block  /  line break"),
                    ("Tab  /  Shift Tab", "Nest  /  un-nest a block"),
                    ("Backspace at start", "Reset or merge block"),
                    ("Ctrl B  /  Ctrl I", "Bold  /  italic"),
                    ("Ctrl E", "Inline code"),
                    ("Ctrl Shift S", "Strikethrough"),
                    ("↑ ↓ at block edge", "Move between blocks"),
                ],
            ),
            (
                "MARKDOWN",
                &[
                    ("# , ## , ###", "Headings"),
                    ("- , * ", "Bulleted list"),
                    ("1. ", "Numbered list"),
                    ("[] ", "To-do"),
                    ("> ", "Toggle"),
                    ("\" ", "Quote"),
                    ("| ", "Callout"),
                    ("```", "Code block"),
                    ("---", "Divider"),
                ],
            ),
        ];
        let open = modal(ctx, Id::new("help"), pal, 760.0, 80.0, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new("Keyboard shortcuts").font(bold(20.0)));
            });
            ui.add_space(8.0);
            ui.horizontal_top(|ui| {
                for (title, rows) in groups.iter() {
                    ui.vertical(|ui| {
                        ui.set_width(238.0);
                        section_label(ui, pal, title);
                        for (k, d) in rows.iter() {
                            ui.horizontal(|ui| {
                                ui.add_space(6.0);
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    ui.label(RichText::new(*k).font(bold(12.5)).color(pal.accent));
                                    ui.label(RichText::new(*d).font(body(13.0)).color(pal.text));
                                    ui.add_space(5.0);
                                });
                            });
                        }
                    });
                }
            });
            ui.add_space(8.0);
        });
        if !open {
            self.help_open = false;
            reset_appear(ctx, Id::new("help").with("a"));
        }
    }

    fn security_ui(&mut self, ctx: &Context, pal: &Palette) {
        let Some(mut dlg) = self.security.take() else { return };
        #[derive(PartialEq)]
        enum A {
            None,
            Save,
            Remove,
        }
        let mut action = A::None;
        let has = self.cred.is_some();
        let open = modal(ctx, Id::new("security"), pal, 400.0, 130.0, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new(if has { "Password & security" } else { "Set a password" }).font(bold(18.0)));
            });
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(
                    RichText::new("Your workspace is locked with a salted, iterated SHA-256 hash stored only on this device.")
                        .color(pal.muted)
                        .font(body(12.5)),
                );
            });
            ui.add_space(2.0);
            let w = 372.0;
            if has {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    field(ui, pal, pal.bg, &mut dlg.cur, "Current password", true, w - 16.0, Id::new("sec_cur"));
                });
            }
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                field(ui, pal, pal.bg, &mut dlg.new, "New password", true, w - 16.0, Id::new("sec_new"));
            });
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                field(ui, pal, pal.bg, &mut dlg.confirm, "Confirm new password", true, w - 16.0, Id::new("sec_conf"));
            });
            if let Some((m, ok)) = &dlg.msg {
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    ui.label(RichText::new(m).color(if *ok { Color32::from_rgb(60, 180, 110) } else { pal.danger }).font(body(13.0)));
                });
            }
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                if primary_button(ui, pal, if has { "Update password" } else { "Set password" }, if has { 190.0 } else { w - 16.0 }).clicked() {
                    action = A::Save;
                }
                if has {
                    let (r, resp) = ui.allocate_exact_size(vec2(w - 16.0 - 190.0 - 8.0, 38.0), Sense::click());
                    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
                    ui.painter().rect_stroke(r, 8.0, Stroke::new(1.0_f32, lerp_color(pal.border, pal.danger, t)), egui::StrokeKind::Inside);
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, "Remove", body(14.0), lerp_color(pal.muted, pal.danger, t));
                    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        action = A::Remove;
                    }
                }
            });
            ui.add_space(8.0);
        });
        let mut keep = open;
        match action {
            A::Save => {
                let cur_ok = match &self.cred {
                    Some(c) => auth::verify(c, &dlg.cur),
                    None => true,
                };
                if !cur_ok {
                    dlg.msg = Some(("Current password is incorrect.".into(), false));
                } else if dlg.new.chars().count() < 4 {
                    dlg.msg = Some(("Use at least 4 characters.".into(), false));
                } else if dlg.new != dlg.confirm {
                    dlg.msg = Some(("New passwords don't match.".into(), false));
                } else {
                    self.cred = Some(auth::create(&dlg.new));
                    self.skipped_setup = false;
                    self.toast("Password saved", None);
                    keep = false;
                }
            }
            A::Remove => {
                let ok = self.cred.as_ref().map_or(false, |c| auth::verify(c, &dlg.cur));
                if ok {
                    self.cred = None;
                    self.skipped_setup = true;
                    self.toast("Password removed", None);
                    keep = false;
                } else {
                    dlg.msg = Some(("Enter your current password to remove it.".into(), false));
                }
            }
            A::None => {}
        }
        if keep {
            self.security = Some(dlg);
        } else {
            reset_appear(ctx, Id::new("security").with("a"));
        }
    }

    fn toasts_ui(&mut self, ctx: &Context, pal: &Palette) {
        let now = now_ms() / 1000.0;
        self.toasts.retain(|t| now - t.born < 4.6);
        let Some(t) = self.toasts.last() else { return };
        let age = (now - t.born) as f32;
        let a = (age / 0.25).min(1.0) * ((4.6 - age) / 0.4).clamp(0.0, 1.0);
        let a = crate::widgets::ease(a);
        let mut undo = None;
        let msg = t.msg.clone();
        let can_undo = t.undo;
        Area::new(Id::new("toast")).order(Order::Tooltip).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -28.0 + (1.0 - a) * 16.0)).show(ctx, |ui| {
            ui.set_opacity(a);
            Frame::new()
                .fill(Color32::from_rgb(38, 38, 42))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::symmetric(16, 10))
                .shadow(egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(80) })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&msg).color(Color32::WHITE).font(body(14.0)));
                        if let Some(id) = can_undo {
                            ui.add_space(8.0);
                            let r = ui.add(egui::Label::new(RichText::new("Undo").color(pal.accent).font(bold(14.0))).sense(Sense::click()));
                            if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                undo = Some(id);
                            }
                        }
                    });
                });
        });
        ctx.request_repaint();
        if let Some(id) = undo {
            self.ws.set_trashed(id, false);
            self.ws.current = Some(id);
            self.toasts.clear();
        }
    }
}

// ───────────── eframe glue ─────────────

impl eframe::App for LumenApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, STORAGE_KEY, &SavedRef { ws: &self.ws, cred: &self.cred, skipped: self.skipped_setup });
    }

    fn auto_save_interval(&self) -> Duration {
        Duration::from_secs(4)
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.ws.dark || self.locked { [0.098, 0.098, 0.098, 1.0] } else { [1.0, 1.0, 1.0, 1.0] }
    }

    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // theme (animated)
        let target = if self.locked || self.ws.dark { 1.0 } else { 0.0 };
        let t = ctx.animate_value_with_time(Id::new("theme_t"), target, 0.35);
        let pal = theme::with_accent(Palette::mix(t), self.ws.accent, t);
        if !self.theme_applied || (t - self.last_theme_t).abs() > 1e-4 || self.last_accent != self.ws.accent {
            theme::apply(ctx, &pal, t > 0.5);
            self.last_theme_t = t;
            self.last_accent = self.ws.accent;
            self.theme_applied = true;
        }

        // idle auto-lock (5 min)
        let now = now_ms() / 1000.0;
        if ctx.input(|i| !i.events.is_empty() || i.pointer.velocity().length() > 0.0) {
            self.last_active = now;
        }
        if !self.locked && self.cred.is_some() && now - self.last_active > 300.0 {
            self.lock_now();
        }
        ctx.request_repaint_after(Duration::from_secs(10));

        if self.locked {
            match self.lock.show(ctx, self.cred.as_ref()) {
                LockEvent::None => {}
                LockEvent::Unlocked => {
                    self.locked = false;
                    self.last_active = now;
                }
                LockEvent::Created(c) => {
                    self.cred = Some(c);
                    self.skipped_setup = false;
                    self.locked = false;
                    self.last_active = now;
                    self.toast("Password set — your workspace is protected", None);
                }
                LockEvent::Skipped => {
                    self.skipped_setup = true;
                    self.locked = false;
                }
                LockEvent::Reset => {
                    self.ws = Workspace::seed();
                    self.cred = None;
                    self.skipped_setup = false;
                    self.lock = LockScreen::new();
                }
            }
            return;
        }

        // global shortcuts
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::L)) {
            self.ws.dark = !self.ws.dark;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::K)) {
            self.palette = if self.palette.is_some() { None } else { Some(PaletteState { query: String::new(), sel: 0, focused: false, pick: None }) };
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::N)) {
            self.new_page(None, PageKind::Doc);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Backslash)) {
            self.ws.sidebar_open = !self.ws.sidebar_open;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::L)) {
            self.run(PalAct::Lock);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::J)) {
            self.run(PalAct::Journal);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Slash)) {
            self.help_open = !self.help_open;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Period)) {
            self.run(PalAct::Focus);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::E)) {
            self.export_md();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::P)) {
            self.palette = Some(PaletteState { query: String::new(), sel: 0, focused: false, pick: None });
        }

        // files dropped onto the window
        let dropped: Vec<egui::DroppedFile> = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            let name = platform::file_name(&f);
            let lower = name.to_lowercase();
            let Some(bytes) = platform::dropped_bytes(&f) else { continue };
            let text = String::from_utf8_lossy(&bytes).to_string();
            let stem = name.rsplit_once('.').map(|x| x.0.to_owned()).unwrap_or(name.clone());
            if lower.ends_with(".csv") || lower.ends_with(".tsv") {
                self.import_text(&stem, &text, true);
            } else if lower.ends_with(".json") {
                self.import = Some(ImportDialog { name: stem, text });
            } else {
                self.import_text(&stem, &text, false);
            }
        }

        // sidebar
        let st = ctx.animate_bool_with_time_and_easing(Id::new("sidebar_t"), self.ws.sidebar_open && !self.focus_mode, 0.28, egui::emath::easing::cubic_out);
        if st > 0.003 {
            egui::SidePanel::left("sidebar")
                .resizable(false)
                .exact_width(SIDEBAR_W * st)
                .frame(Frame::new().fill(pal.sidebar).inner_margin(Margin::symmetric(8, 0)))
                .show(ctx, |ui| {
                    ui.set_clip_rect(ui.max_rect().expand2(vec2(8.0, 0.0)).intersect(ui.clip_rect()));
                    ui.set_opacity(st.powf(1.5));
                    ui.set_min_width(SIDEBAR_W - 16.0);
                    self.sidebar(ui, &pal);
                    let r = ui.clip_rect();
                    ui.painter().vline(r.right(), r.y_range(), Stroke::new(1.0_f32, pal.border));
                });
        }

        if !self.focus_mode {
            egui::TopBottomPanel::top("topbar")
                .exact_height(46.0)
                .frame(Frame::new().fill(pal.bg))
                .show(ctx, |ui| {
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| self.topbar(ui, &pal));
                });
        }

        egui::CentralPanel::default().frame(Frame::new().fill(pal.bg)).show(ctx, |ui| {
            let Some(cur) = self.ws.current.filter(|id| self.ws.page(*id).map_or(false, |p| !p.trashed)) else {
                ui.add_space(ui.available_height() * 0.3);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("📄").font(body(46.0)).color(pal.muted));
                    ui.label(RichText::new("No page selected").font(bold(18.0)));
                    ui.add_space(6.0);
                    if primary_button(ui, &pal, "Create a page", 160.0).clicked() {
                        self.new_page(None, PageKind::Doc);
                    }
                });
                return;
            };

            if self.shown_page != Some(cur) {
                reset_appear(ctx, Id::new(("page_in", cur)));
                self.shown_page = Some(cur);
                self.ed.block_menu = None;
                self.ed.icon_picker = None;
                self.ed.cover_picker = None;
            }
            let a = appear(ctx, Id::new(("page_in", cur)), 0.28);
            if self.focus_title == Some(cur) {
                ctx.memory_mut(|m| m.request_focus(Id::new(("title", cur))));
                self.focus_title = None;
            }
            ui.add_space((1.0 - a) * 14.0);
            ui.set_opacity(a);

            let ws = &mut self.ws;
            let ed = &mut self.ed;
            let mut changed = false;
            ScrollArea::vertical().id_salt(("page_scroll", cur)).auto_shrink([false, false]).show(ui, |ui| {
                changed = editor::show_page(ui, ws, cur, ed, &pal);
            });
            if changed {
                ws.touch(cur);
            }
        });

        if let Some(id) = self.ed.nav.take() {
            if self.ws.page(id).map_or(false, |p| !p.trashed) {
                self.ws.current = Some(id);
            }
        }
        if let (Some(req), Some(cur)) = (self.ed.req.take(), self.ws.current) {
            self.handle_req(req, cur);
        }

        self.row_menu_ui(ctx, &pal);
        self.page_menu_ui(ctx, &pal);
        self.templates_ui(ctx, &pal);
        self.import_ui(ctx, &pal);
        self.help_ui(ctx, &pal);
        self.ws_menu_ui(ctx, &pal);
        self.palette_ui(ctx, &pal);
        self.trash_ui(ctx, &pal);
        self.security_ui(ctx, &pal);
        self.toasts_ui(ctx, &pal);
    }
}

// ───────────── helpers ─────────────

fn ago(ms: f64) -> String {
    let s = ((now_ms() - ms) / 1000.0).max(0.0) as u64;
    match s {
        0..=9 => "just now".into(),
        10..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

fn modal(ctx: &Context, id: Id, pal: &Palette, width: f32, top: f32, add: impl FnOnce(&mut Ui)) -> bool {
    let t = appear(ctx, id.with("a"), 0.2);
    let mut open = true;
    let screen = ctx.screen_rect();
    Area::new(id.with("backdrop")).order(Order::Foreground).fixed_pos(Pos2::ZERO).show(ctx, |ui| {
        let (r, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
        ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha((120.0 * t) as u8));
        if resp.clicked() {
            open = false;
        }
    });
    Area::new(id).order(Order::Foreground).anchor(Align2::CENTER_TOP, vec2(0.0, top + (1.0 - t) * 16.0)).show(ctx, |ui| {
        ui.set_opacity(t);
        crate::widgets::popup_frame(pal).corner_radius(CornerRadius::same(14)).inner_margin(Margin::same(8)).show(ui, |ui| {
            ui.set_width(width);
            add(ui);
        });
    });
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        open = false;
    }
    open
}

fn crumb(ui: &mut Ui, pal: &Palette, icon: &str, title: &str, last: bool) -> egui::Response {
    let col = if last { pal.text } else { pal.muted };
    let galley = ui.painter().layout_no_wrap(title.to_owned(), body(14.0), col);
    let iw = if icon.is_empty() { 0.0 } else { 22.0 };
    let w = (galley.size().x + iw + 16.0).min(260.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 28.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
    ui.painter().rect_filled(rect, 6.0, with_alpha(pal.hover, t));
    let mut x = rect.left() + 8.0;
    if !icon.is_empty() {
        ui.painter().text(pos2(x + 7.0, rect.center().y), Align2::CENTER_CENTER, icon, body(14.0), pal.text);
        x += iw;
    }
    let clip = Rect::from_min_max(pos2(x, rect.top()), pos2(rect.right() - 4.0, rect.bottom()));
    ui.painter().with_clip_rect(clip).text(pos2(x, rect.center().y), Align2::LEFT_CENTER, title, body(14.0), col);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn row_bg(ui: &Ui, pal: &Palette, key: Id, rect: Rect, selected: bool) -> (bool, f32) {
    let hovered = ui.rect_contains_pointer(rect);
    let t = ui.ctx().animate_bool_with_time(key.with("bg"), hovered, 0.1);
    let s = ui.ctx().animate_bool_with_time(key.with("sel"), selected, 0.15);
    if s > 0.0 {
        ui.painter().rect_filled(rect, 6.0, with_alpha(pal.active, s));
    }
    if t > 0.0 {
        ui.painter().rect_filled(rect, 6.0, with_alpha(pal.hover, t * (1.0 - s)));
    }
    (hovered, t)
}

fn simple_row(ui: &mut Ui, pal: &Palette, key: &str, w: f32, icon: Icon, label: &str, hint: Option<&str>) -> bool {
    let key = Id::new(key);
    let (_, rect) = ui.allocate_space(vec2(w, 28.0));
    let resp = ui.interact(rect, key, Sense::click());
    let (_, t) = row_bg(ui, pal, key, rect, false);
    let col = lerp_color(pal.muted, pal.text, 0.35 + 0.65 * t);
    icons::paint(ui.painter(), pos2(rect.left() + 20.0, rect.center().y), 15.0, icon, col);
    ui.painter().text(pos2(rect.left() + 38.0, rect.center().y), Align2::LEFT_CENTER, label, body(14.5), col);
    if let Some(h) = hint {
        ui.painter().text(pos2(rect.right() - 10.0, rect.center().y), Align2::RIGHT_CENTER, h, body(11.5), with_alpha(pal.muted, 0.8));
    }
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

struct RowOut {
    click: bool,
    arrow: bool,
    plus: bool,
    more: Option<Pos2>,
}

#[allow(clippy::too_many_arguments)]
fn page_row(ui: &mut Ui, pal: &Palette, key: Id, w: f32, depth: usize, page: &Page, selected: bool, has_children: bool) -> RowOut {
    let (_, rect) = ui.allocate_space(vec2(w, 28.0));
    let resp = ui.interact(rect, key, Sense::click());
    let (hovered, t) = row_bg(ui, pal, key, rect, selected);
    let x0 = rect.left() + 2.0 + depth as f32 * 14.0;
    let p = ui.painter();

    // disclosure arrow
    let arrow_rect = Rect::from_min_size(pos2(x0, rect.top() + 4.0), vec2(20.0, 20.0));
    let aresp = ui.interact(arrow_rect, key.with("arrow"), Sense::click());
    let et = ui.ctx().animate_bool_with_time(key.with("exp"), page.expanded, 0.18);
    if has_children {
        let ah = ui.ctx().animate_bool_with_time(key.with("ah"), aresp.hovered(), 0.1);
        p.rect_filled(arrow_rect, 4.0, with_alpha(pal.active, ah));
        icons::chevron(p, arrow_rect.center(), 13.0, et * FRAC_PI_2, lerp_color(pal.muted, pal.text, ah));
    }

    // icon & title
    let ic = pos2(x0 + 32.0, rect.center().y);
    if page.icon.is_empty() {
        icons::paint(p, ic, 15.0, if page.kind == PageKind::Database { Icon::Board } else { Icon::Page }, pal.muted);
    } else {
        p.text(ic, Align2::CENTER_CENTER, &page.icon, body(15.0), pal.text);
    }
    let label_right = rect.right() - 6.0 - 48.0 * t;
    let clip = Rect::from_min_max(pos2(x0 + 46.0, rect.top()), pos2(label_right.max(x0 + 60.0), rect.bottom()));
    let title = if page.title.is_empty() { "Untitled" } else { &page.title };
    let col = if page.title.is_empty() {
        pal.muted
    } else {
        lerp_color(pal.muted, pal.text, 0.55 + 0.45 * t.max(if selected { 1.0 } else { 0.0 }))
    };
    p.with_clip_rect(clip).text(pos2(x0 + 46.0, rect.center().y), Align2::LEFT_CENTER, title, body(14.5), col);

    // hover actions
    let mut out = RowOut { click: false, arrow: false, plus: false, more: None };
    if t > 0.02 && hovered {
        let more = Rect::from_center_size(pos2(rect.right() - 16.0, rect.center().y), vec2(22.0, 22.0));
        let plus = Rect::from_center_size(pos2(rect.right() - 40.0, rect.center().y), vec2(22.0, 22.0));
        let mr = ui.interact(more, key.with("more"), Sense::click());
        let pr = ui.interact(plus, key.with("plus"), Sense::click());
        crate::widgets::paint_icon_button(ui, pal, more, &mr, Icon::Dots, 15.0);
        crate::widgets::paint_icon_button(ui, pal, plus, &pr, Icon::Plus, 14.0);
        if mr.clicked() {
            out.more = Some(pos2(more.left(), more.bottom() + 4.0));
        }
        if pr.clicked() {
            out.plus = true;
        }
    }
    out.click = resp.on_hover_cursor(CursorIcon::PointingHand).clicked();
    out.arrow = aresp.clicked() && has_children;
    out
}

//! Application shell: sidebar, top bar, command palette, dialogs, toasts and persistence.

use crate::widgets::LayerRaise;
use crate::lucide as l;
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
use crate::media::{self, MediaStore};
use crate::model::{Block, BlockKind, Page, PageKind, Workspace, TEMPLATES};
use crate::platform::{PickTarget};
use crate::theme::{self, bold, body, lerp_color, Palette, ACCENTS};
use crate::widgets::{
    appear, context_menu, field, icon_button, menu_item, popup_area, primary_button, reset_appear, section_label, with_alpha, CtxItem,
};

const SIDEBAR_W: f32 = 264.0;
const STORAGE_KEY: &str = "lumen_v1";
/// Data safety (see `LumenApp::save` / `load_saved`): the state as it was before the newest save (refreshed at most every 10 minutes),
/// the state of the previous app version (kept once on every upgrade) and the raw text of a state that could not be read.
const PREV_KEY: &str = "lumen_v1_prev";
const PREV_AT_KEY: &str = "lumen_v1_prev_at";
const UPGRADE_KEY: &str = "lumen_v1_before_upgrade";
const UPGRADE_VER_KEY: &str = "lumen_v1_before_upgrade_version";
const VERSION_KEY: &str = "lumen_version";
const UNREADABLE_KEY: &str = "lumen_v1_unreadable";

/// Reads the workspace; when the newest state is damaged (or written by an incompatible version) the next older copy is used.
/// Returns the state and, when the primary copy existed but could not be read, its raw text so that it can be preserved.
fn load_saved(s: &dyn eframe::Storage) -> (Option<Saved>, Option<String>) {
    let mut rescued = None;
    for (i, key) in [STORAGE_KEY, PREV_KEY, UPGRADE_KEY].into_iter().enumerate() {
        let Some(raw) = s.get_string(key) else { continue };
        if raw.trim().is_empty() {
            continue;
        }
        match eframe::get_value::<Saved>(s, key) {
            Some(v) => {
                if i > 0 {
                    log::warn!("workspace restored from the backup slot {key}");
                }
                return (Some(v), rescued);
            }
            None => {
                if i == 0 {
                    rescued = Some(raw);
                }
            }
        }
    }
    (None, rescued)
}

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
    Blank(Pos2),
}

#[derive(Clone, Copy)]
enum PalAct {
    Open(u64),
    NewPage,
    NewDb,
    NewCanvas,
    NewFlow,
    LiveDemo,
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
    Print,
    History,
    Undo,
    Redo,
    Dashboard,
    Graph,
    Automations,
    NewReminder,
    Ai,
    AiSettings,
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
    side_menu: Option<Pos2>,
    ws_menu: Option<Pos2>,
    /// Raw text of a stored workspace that could not be parsed (kept under its own storage key on the next save).
    rescued: Option<String>,
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
    /// Page-hierarchy popup (tap the page title on a phone): open flag, expanded nodes, "expand the current path" done.
    hier: bool,
    hier_exp: std::collections::HashSet<u64>,
    hier_init: bool,
    focus_mode: bool,
    last_accent: usize,
    last_touch: bool,
    media: MediaStore,
    history: Option<crate::history::HistState>,
    last_calc: f64,
    calc_dirty: bool,
    prev_sidebar_page: Option<u64>,
    /// Automation engine state (change snapshot, rate limits) — never saved.
    auto_rt: crate::automation::Runtime,
    bg: crate::flowbg::Bg,
    last_auto: f64,
    last_push: f64,
    /// Lumen AI: provider settings, chats, the agent loop and the MCP transports (see `ai.rs`, `aiui.rs`, `mcp.rs`).
    ai: crate::ai::AiHost,
}

impl LumenApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        if crate::platform::coarse_pointer() {
            crate::touch::set_touchy(true);
        }
        theme::setup_fonts(&cc.egui_ctx);
        #[cfg(target_arch = "wasm32")]
        theme::load_web_fonts(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let (saved, rescued) = cc.storage.map(|s| load_saved(s)).unwrap_or((None, None));
        crate::platform::request_persistent_storage();
        let (ws, cred, skipped) = match saved {
            Some(s) => (s.ws, s.cred, s.skipped),
            None => (Workspace::seed(), None, false),
        };
        let mut ws = ws;
        ws.migrate();
        // fonts for the scripts that are really used in this workspace (the others are never loaded = less memory)
        for pg in &ws.pages {
            theme::ensure_scripts(&cc.egui_ctx, &pg.title);
            for b in pg.blocks.iter().filter(|b| b.kind.has_text()) {
                theme::ensure_scripts(&cc.egui_ctx, &b.text);
            }
            for v in pg.props.values() {
                if let crate::model::Val::Text(t) = v {
                    theme::ensure_scripts(&cc.egui_ctx, t);
                }
            }
        }
        if ws.current.map_or(true, |c| ws.page(c).is_none()) {
            ws.current = ws.pages.iter().find(|p| !p.trashed && !p.row).map(|p| p.id);
        }
        let locked = cred.is_some() || !skipped;
        let mut media = MediaStore::load(cc.storage);
        {
            let mut used = std::collections::HashSet::new();
            for p in &ws.pages {
                used.extend(media::media_id(&p.cover_img));
                used.extend(p.icon.strip_prefix("img:").and_then(media::media_id));
                used.extend(p.blocks.iter().filter(|b| b.media != 0).map(|b| b.media));
            }
            media.gc(&used);
            media.register(&cc.egui_ctx);
        }
        let ai = crate::ai::AiHost { st: cc.storage.and_then(|s| eframe::get_value::<crate::ai::AiState>(s, crate::ai::STORAGE_KEY)).unwrap_or_default(), rt: Default::default() };
        let mut ed = EditorState::default();
        ed.media_sizes = media.items.iter().map(|(k, v)| (*k, v.size)).collect();
        Self {
            ws,
            cred,
            skipped_setup: skipped,
            locked,
            lock: LockScreen::new(),
            ed,
            palette: None,
            row_menu: None,
            side_menu: None,
            ws_menu: None,
            trash_open: false,
            security: None,
            fresh: false,
            toasts: vec![],
            last_theme_t: -1.0,
            theme_applied: false,
            rescued,
            last_active: now_ms() / 1000.0,
            shown_page: None,
            focus_title: None,
            import: None,
            templates_open: false,
            help_open: false,
            page_menu: None,
            hier: false,
            hier_exp: Default::default(),
            hier_init: false,
            focus_mode: false,
            last_accent: usize::MAX,
            last_touch: false,
            media,
            history: None,
            last_calc: 0.0,
            calc_dirty: true,
            prev_sidebar_page: None,
            auto_rt: crate::automation::Runtime::new(),
            bg: crate::flowbg::Bg::default(),
            last_auto: 0.0,
            last_push: 0.0,
            ai,
        }
    }

    /// Stores a picked / dropped file and wires it into its target (block, cover or icon).
    fn set_media(&mut self, ctx: &Context, target: PickTarget, name: &str, bytes: Vec<u8>) {
        let as_image = !matches!(target, PickTarget::BlockFile { .. });
        let size_hint = bytes.len();
        match self.media.add(ctx, name, bytes, as_image) {
            Err(e) => self.toast(e, None),
            Ok(id) => {
                let size = self.media.get(id).map(|i| i.size).unwrap_or(size_hint);
                self.ed.media_sizes.insert(id, size);
                let page = match target {
                    PickTarget::ImportFile { .. } => return,
                    PickTarget::BlockImage { page, .. } | PickTarget::BlockFile { page, .. } | PickTarget::Cover { page } | PickTarget::Icon { page } | PickTarget::CanvasImage { page, .. } => page,
                };
                if let Some(p) = self.ws.page_mut(page) {
                    match target {
                        PickTarget::ImportFile { .. } => {}
                        PickTarget::BlockImage { block, .. } => {
                            if let Some(b) = p.blocks.iter_mut().find(|b| b.id == block) {
                                b.kind = BlockKind::Image;
                                b.media = id;
                                b.url.clear();
                            }
                        }
                        PickTarget::BlockFile { block, .. } => {
                            if let Some(b) = p.blocks.iter_mut().find(|b| b.id == block) {
                                b.kind = BlockKind::File;
                                b.media = id;
                                b.url.clear();
                                b.text = name.to_owned();
                            }
                        }
                        PickTarget::Cover { .. } => {
                            p.cover_img = format!("media:{id}");
                            p.cover = None;
                            p.cover_pos = 0.5;
                            self.ed.cover_picker = None;
                        }
                        PickTarget::Icon { .. } => {
                            p.icon = format!("img:media:{id}");
                            self.ed.icon_picker = None;
                        }
                        PickTarget::CanvasImage { node, .. } => {
                            if let Some(c) = p.canvas.as_mut() {
                                if let Some(n) = c.node_mut(node) {
                                    n.kind = crate::canvas::Kind::Image;
                                    n.url = format!("media:{id}");
                                    n.h = 0.0;
                                }
                                c.rev += 1;
                            }
                            crate::canvasui::close_popup();
                        }
                    }
                }
                self.ws.touch(page);
            }
        }
    }

    /// Appends an image / file block to the current page (used by drag & drop).
    fn add_media_block(&mut self, ctx: &Context, kind: BlockKind, name: &str, bytes: Vec<u8>) {
        let Some(cur) = self.ws.current else { return };
        if self.ws.page(cur).map_or(false, |p| p.kind == PageKind::Canvas) {
            if kind == BlockKind::Image {
                if let Some(node) = crate::canvasui::new_image_node(&mut self.ws, cur) {
                    self.set_media(ctx, PickTarget::CanvasImage { page: cur, node }, name, bytes);
                }
            } else {
                self.toast("Drop images onto a canvas to add them", None);
            }
            return;
        }
        let bid = self.ws.id();
        let tail_id = self.ws.id();
        let target = if kind == BlockKind::Image { PickTarget::BlockImage { page: cur, block: bid } } else { PickTarget::BlockFile { page: cur, block: bid } };
        if let Some(p) = self.ws.page_mut(cur) {
            if p.kind != PageKind::Doc {
                self.toast("Open a document page to add files", None);
                return;
            }
            let mut b = Block::new(bid, kind, "", 0);
            b.text = String::new();
            let empty_tail = p.blocks.last().map_or(false, |l| l.kind == BlockKind::Text && l.text.is_empty());
            let at = if empty_tail { p.blocks.len() - 1 } else { p.blocks.len() };
            p.blocks.insert(at, b);
            if !empty_tail {
                p.blocks.push(Block::new(tail_id, BlockKind::Text, "", 0));
            }
        }
        self.set_media(ctx, target, name, bytes);
        self.toast(if kind == BlockKind::Image { "Image added to the page" } else { "File attached" }, None);
    }

    /// Applies finished file dialogs, saves requested attachments.
    fn poll_media(&mut self, ctx: &Context) {
        if let Some((media_id, url, title)) = self.ed.pdf_ask.take() {
            let (key, source) = if media_id != 0 {
                (media_id, self.media.bytes(media_id).map(crate::pdf::Source::Local))
            } else {
                (crate::pdf::key_of(&url), Some(crate::pdf::Source::Remote(media::normalize_url(&url))))
            };
            match source {
                Some(s) => self.ed.pdf_req = Some(crate::pdf::Request { key, title, source: Some(s) }),
                None => self.toast("The attached file could not be read", None),
            }
        }
        for pk in platform::take_picked() {
            if let PickTarget::ImportFile { mode } = pk.target {
                self.import_file(mode, &pk.name, &pk.bytes);
                continue;
            }
            self.set_media(ctx, pk.target, &pk.name, pk.bytes);
        }
        if let Some((id, name)) = self.ed.dl_req.take() {
            if let Some(data) = self.media.bytes(id) {
                let msg = platform::save_download(&name, &data, "application/octet-stream");
                self.toast(msg, None);
            }
        }
    }

    /// Web: draws live iframes over embed blocks; Windows: native WebView2 child windows (hidden while a modal / palette is open).
    fn sync_embeds(&mut self, frame: &eframe::Frame) {
        let overlay = self.locked
            || self.ed.pdf_req.is_some()
            || self.history.is_some()
            || self.palette.is_some()
            || self.help_open
            || self.import.is_some()
            || self.templates_open
            || self.trash_open
            || self.security.is_some()
            || self.ed.menu.is_some()
            || self.ed.block_menu.is_some()
            || self.ed.icon_picker.is_some()
            || self.ed.cover_picker.is_some();
        if overlay {
            platform::sync_iframes(&[]);
            platform::sync_native_previews(frame, &[]);
        } else {
            platform::sync_iframes(&self.ed.embed_rects);
            platform::sync_native_previews(frame, &self.ed.embed_rects);
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

    /// A file chosen with the import dialog's "Choose file" buttons (Markdown → page, CSV → database, JSON backup).
    fn import_file(&mut self, mode: u8, file_name: &str, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes).trim_start_matches('\u{feff}').to_string();
        let stem = file_name.rsplit_once('.').map(|x| x.0.to_owned()).unwrap_or_else(|| file_name.to_owned());
        let typed = self.import.as_ref().map(|d| d.name.trim().to_owned()).unwrap_or_default();
        let name = if typed.is_empty() { stem } else { typed };
        if text.trim().is_empty() {
            self.toast("That file is empty", None);
            return;
        }
        match mode {
            2 => match serde_json::from_str::<Workspace>(&text) {
                Ok(mut w) => {
                    w.migrate();
                    self.ws = w;
                    self.auto_rt.resync(&self.ws);
                    self.import = None;
                    self.toast("Workspace restored from backup", None);
                }
                Err(_) => self.toast("That isn't a valid Lumen backup", None),
            },
            m => {
                self.import = None;
                self.import_text(&name, &text, m == 1);
            }
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

    fn print_page(&mut self) {
        let Some(cur) = self.ws.current else { return };
        let html = crate::print::page_html(&self.ws, &self.media, cur, !cfg!(target_arch = "wasm32"));
        let name = format!("{}-print.html", nio::file_safe(&self.ws.display_title(cur)));
        let msg = platform::open_html(&name, &html);
        self.toast(msg, None);
    }

    fn can_undo_redo(&self, cur: u64) -> (bool, bool) {
        if self.ws.page(cur).map_or(false, |p| p.kind == PageKind::Canvas) {
            crate::canvasui::can_undo_redo(&self.ws, cur)
        } else if self.ws.page(cur).map_or(false, |p| p.kind == PageKind::Flow) {
            crate::flowui::can_undo_redo(&self.ws, cur)
        } else {
            (crate::history::can_undo(&self.ed, cur), crate::history::can_redo(&self.ed, cur))
        }
    }

    fn undo_redo(&mut self, redo: bool) {
        let Some(cur) = self.ws.current else { return };
        if self.ws.page(cur).map_or(false, |p| p.kind == PageKind::Canvas) {
            if !crate::canvasui::undo_redo(&mut self.ws, cur, redo) {
                self.toast(if redo { "Nothing to redo" } else { "Nothing to undo" }, None);
            }
            return;
        }
        if self.ws.page(cur).map_or(false, |p| p.kind == PageKind::Flow) {
            if !crate::flowui::undo_redo(&mut self.ws, cur, redo) {
                self.toast(if redo { "Nothing to redo" } else { "Nothing to undo" }, None);
            }
            return;
        }
        let Some(p) = self.ws.page_mut(cur) else { return };
        let ok = if redo { crate::history::redo(&mut self.ed, p) } else { crate::history::undo(&mut self.ed, p) };
        if ok {
            p.updated = now_ms();
        } else {
            self.toast(if redo { "Nothing to redo" } else { "Nothing to undo" }, None);
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
            PalAct::NewCanvas => self.new_page(None, PageKind::Canvas),
            PalAct::NewFlow => self.new_page(None, PageKind::Flow),
            PalAct::LiveDemo => match crate::demo::build(&mut self.ws) {
                Ok(id) => {
                    crate::views::set(crate::views::Mode::Page, None);
                    self.ws.current = Some(id);
                    self.calc_dirty = true;
                    self.toast("Live demo started — a mock API feeds the dashboard every second, even when you switch pages", None);
                }
                Err(e) => self.toast(format!("Could not build the demo: {e}"), None),
            },
            PalAct::Journal => self.ws.open_today(),
            PalAct::Dashboard => crate::views::toggle(crate::views::Mode::Dashboard, self.ws.current),
            PalAct::Graph => crate::views::toggle(crate::views::Mode::Graph, self.ws.current),
            PalAct::Ai => {
                self.ai.rt.open = !self.ai.rt.open;
                self.ai.rt.focus_input = self.ai.rt.open;
            }
            PalAct::AiSettings => {
                self.ai.rt.open = true;
                self.ai.rt.settings = Some(crate::ai::SettingsTab::Providers);
            }
            PalAct::Automations => {
                crate::autoui::open_tab(crate::autoui::TAB_RULES);
                crate::views::toggle(crate::views::Mode::Automations, self.ws.current);
            }
            PalAct::NewReminder => {
                crate::autoui::open_tab(crate::autoui::TAB_REMINDERS);
                crate::views::set(crate::views::Mode::Automations, self.ws.current);
            }
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
            PalAct::Print => self.print_page(),
            PalAct::History => {
                if self.ws.current.is_some() {
                    self.history = Some(crate::history::HistState::default());
                }
            }
            PalAct::Undo => self.undo_redo(false),
            PalAct::Redo => self.undo_redo(true),
        }
    }

    // ───────────── sidebar ─────────────

    /// One concise "New" entry in the sidebar: hovering (or tapping) it opens a fly-out on its right with page · database · canvas · flow.
    fn new_flyout(&mut self, ui: &mut Ui, pal: &Palette, w: f32) {
        let key = Id::new("sr_new");
        let ctx = ui.ctx().clone();
        let (_, rect) = ui.allocate_space(vec2(w, 28.0));
        let resp = ui.interact(rect, key, Sense::click());
        let open_id = Id::new("new_fly_open");
        let last_id = Id::new("new_fly_last");
        let mut open: bool = ctx.data(|d| d.get_temp(open_id)).unwrap_or(false);
        let mut last: f64 = ctx.data(|d| d.get_temp(last_id)).unwrap_or(0.0);
        let now = ctx.input(|i| i.time);
        let touch = crate::touch::touchy();
        let (_, t) = row_bg(ui, pal, key, rect, open);
        let col = lerp_color(pal.muted, pal.text, 0.35 + 0.65 * t);
        icons::paint(ui.painter(), pos2(rect.left() + 20.0, rect.center().y), 15.0, Icon::Plus, col);
        ui.painter().text(pos2(rect.left() + 38.0, rect.center().y), Align2::LEFT_CENTER, "New", body(14.5), col);
        ui.painter().text(pos2(rect.right() - 12.0, rect.center().y), Align2::RIGHT_CENTER, "›", bold(17.0), col);
        let hov = resp.hovered();
        if resp.clicked() {
            open = !open;
        } else if hov && !touch {
            open = true;
        }
        if hov || (open && last == 0.0) {
            last = now;
        }
        if open {
            let a = ctx.animate_bool_with_time(Id::new("new_fly_t"), true, 0.12);
            let pos = pos2(rect.right() + 6.0, rect.top() - 6.0);
            let mut pick: Option<u8> = None;
            let area = Area::new(Id::new("new_fly")).order(Order::Foreground).fixed_pos(pos + vec2((1.0 - a) * -6.0, 0.0)).show(&ctx, |ui| {
                ui.set_opacity(a);
                crate::widgets::popup_frame(pal).show(ui, |ui| {
                    ui.set_width(236.0);
                    section_label(ui, pal, "CREATE");
                    if menu_item(ui, pal, l::FILE_PLUS, "Page", Some("Notes, docs, wikis — Markdown, tables, blocks"), false, false).clicked() {
                        pick = Some(0);
                    }
                    if menu_item(ui, pal, l::KANBAN, "Database", Some("Table, board, list, gallery, calendar, timeline"), false, false).clicked() {
                        pick = Some(1);
                    }
                    if menu_item(ui, pal, l::NETWORK, "Canvas", Some("Infinite whiteboard with connectors"), false, false).clicked() {
                        pick = Some(2);
                    }
                    if menu_item(ui, pal, l::ZAP, "Flow", Some("Node-based automation and data pipelines"), false, false).clicked() {
                        pick = Some(3);
                    }
                    ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top() + 2.0, Stroke::new(1.0_f32, pal.border));
                    ui.add_space(5.0);
                    if menu_item(ui, pal, l::LAYOUT_TEMPLATE, "From template…", None, false, false).clicked() {
                        pick = Some(4);
                    }
                    if menu_item(ui, pal, l::FILE_DOWN, "Import Markdown / CSV…", None, false, false).clicked() {
                        pick = Some(5);
                    }
                });
            });
            let over = ctx.pointer_hover_pos().map_or(false, |p| area.response.rect.expand(8.0).contains(p));
            if over || hov {
                last = now;
            }
            if (!touch && now - last > 0.28) || (ctx.input(|i| i.pointer.any_pressed()) && !over && !hov) {
                open = false;
            }
            if let Some(k) = pick {
                open = false;
                match k {
                    0 => self.new_page(None, PageKind::Doc),
                    1 => self.new_page(None, PageKind::Database),
                    2 => self.new_page(None, PageKind::Canvas),
                    3 => self.new_page(None, PageKind::Flow),
                    4 => self.templates_open = true,
                    _ => self.run(PalAct::Import),
                }
            }
            if open {
                ctx.request_repaint_after(std::time::Duration::from_millis(60));
            }
        }
        if !open {
            ctx.animate_bool_with_time(Id::new("new_fly_t"), false, 0.0);
            last = 0.0;
        }
        ctx.data_mut(|d| {
            d.insert_temp(open_id, open);
            d.insert_temp(last_id, last);
        });
    }

    /// Loads the font of any script that shows up in typed / pasted / IME text, or on the page being viewed (imports, AI, MCP, sync).
    fn load_script_fonts(&mut self, ctx: &Context) {
        let typed: Vec<String> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Text(t) | egui::Event::Paste(t) | egui::Event::Ime(egui::ImeEvent::Commit(t)) if !t.is_ascii() => Some(t.clone()),
                    _ => None,
                })
                .collect()
        });
        for t in typed {
            theme::ensure_scripts(ctx, &t);
        }
        let now = ctx.input(|i| i.time);
        let last: f64 = ctx.data(|d| d.get_temp(Id::new("script_scan_t"))).unwrap_or(-10.0);
        if now - last > 2.0 && !self.locked {
            ctx.data_mut(|d| d.insert_temp(Id::new("script_scan_t"), now));
            if let Some(p) = self.ws.current.and_then(|c| self.ws.page(c)) {
                theme::ensure_scripts(ctx, &p.title);
                for b in p.blocks.iter().filter(|b| b.kind.has_text()) {
                    theme::ensure_scripts(ctx, &b.text);
                }
            }
            if self.ai.rt.open {
                let texts: Vec<String> = self.ai.st.threads.iter().max_by(|a, b| a.updated.total_cmp(&b.updated)).map(|t| t.msgs.iter().rev().take(3).map(|m| m.text.clone()).collect()).unwrap_or_default();
                for t in texts {
                    theme::ensure_scripts(ctx, &t);
                }
            }
            ctx.request_repaint_after(Duration::from_secs(2));
        }
    }

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
        if crate::touch::reveal(ui.rect_contains_pointer(hr)) {
            let h = ui.ctx().animate_bool_with_time(cresp.id, cresp.hovered() || cresp.is_pointer_button_down_on(), 0.1);
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
        self.new_flyout(ui, pal, w);
        if simple_row(ui, pal, "sr_journal", w, Icon::Page, "Today's journal", Some("Ctrl J")) {
            self.run(PalAct::Journal);
        }
        if simple_row(ui, pal, "sr_tpl", w, Icon::Duplicate, "Templates", None) {
            self.templates_open = true;
        }
        if simple_row(ui, pal, "sr_home", w, Icon::Board, "Dashboard", Some("Ctrl D")) {
            self.run(PalAct::Dashboard);
        }
        if simple_row(ui, pal, "sr_graph", w, Icon::Link, "Graph view", Some("Ctrl G")) {
            self.run(PalAct::Graph);
        }
        let unread = self.ws.auto.unread();
        let badge = if unread > 0 { format!("{unread} new") } else { String::new() };
        if simple_row(ui, pal, "sr_auto", w, Icon::Zap, "Automations", if unread > 0 { Some(badge.as_str()) } else { Some("Ctrl ⇧ A") }) {
            self.run(PalAct::Automations);
        }
        if simple_row(ui, pal, "sr_ai", w, Icon::Sparkles, "Ask Lumen AI", Some("Ctrl ⇧ J")) {
            self.run(PalAct::Ai);
            if ui.ctx().screen_rect().width() < 720.0 {
                self.ws.sidebar_open = false;
            }
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
                    if let Some(pos) = out.more.or(out.rclick) {
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

        if !ev.iter().any(|e| matches!(e, Ev::Menu(..))) {
            if let Some(p) = crate::widgets::right_clicked(ui, ui.max_rect()) {
                ev.push(Ev::Blank(p));
            }
        }
        for e in ev {
            match e {
                Ev::Blank(pos) => {
                    self.side_menu = Some(pos);
                    self.row_menu = None;
                    self.fresh = true;
                }
                Ev::Open(id) => self.ws.current = Some(id),
                Ev::Toggle(id) => {
                    if let Some(p) = self.ws.page_mut(id) {
                        p.expanded = !p.expanded;
                    }
                }
                Ev::AddChild(id) => self.new_page(Some(id), PageKind::Doc),
                Ev::Menu(id, pos) => {
                    self.row_menu = Some((id, pos));
                    self.side_menu = None;
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
            if let Some(pos) = out.more.or(out.rclick) {
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
        // Automations is its own workspace: the bar must not show (or act on) the page that is hidden behind it.
        if crate::views::mode() == crate::views::Mode::Automations {
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(l::ZAP).font(body(15.0)).color(pal.accent)));
            ui.add(egui::Label::new(RichText::new("Automations").font(bold(15.0)).color(pal.text)).truncate());
            return;
        }
        let Some(cur) = self.ws.current else { return };
        let narrow = ui.ctx().screen_rect().width() < 720.0;
        if narrow {
            self.topbar_narrow(ui, pal, cur);
            return;
        }
        // room for the breadcrumb: the screen minus the menu button and the four right-hand buttons (phones: never overlap them)
        let btn = if crate::touch::touchy() { 38.0 } else { 28.0 };
        let crumb_w = if narrow { (ui.ctx().screen_rect().width() - 8.0 - btn * 5.0 - ui.spacing().item_spacing.x * 6.0 - 30.0).clamp(48.0, 260.0) } else { 260.0 };
        let crumbs = self.ws.breadcrumb(cur);
        let n = crumbs.len();
        let mut nav: Option<u64> = None;
        let mut crumb_menu: Option<(u64, Pos2)> = None;
        for (i, id) in crumbs.iter().enumerate() {
            if narrow && i + 1 < n {
                continue;
            }
            if let Some(p) = self.ws.page(*id) {
                let title = if p.title.is_empty() { "Untitled" } else { &p.title };
                let cr = crumb(ui, pal, &p.icon, title, i + 1 == n, crumb_w);
                if cr.clicked() {
                    nav = Some(*id);
                }
                if let Some(pos) = crate::widgets::right_clicked(ui, cr.rect) {
                    crumb_menu = Some((*id, pos));
                }
                if i + 1 < n {
                    ui.label(RichText::new("/").color(with_alpha(pal.muted, 0.6)).font(body(14.0)));
                }
            }
        }
        if let Some(id) = nav {
            self.ws.current = Some(id);
        }
        if let Some(m) = crumb_menu {
            self.row_menu = Some(m);
            self.fresh = true;
        }
        let (updated, fav) = self.ws.page(cur).map(|p| (p.updated, p.favorite)).unwrap_or((0.0, false));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(10.0);
            let r = icon_button(ui, pal, Icon::Dots, 16.0, "Page options");
            if r.clicked() {
                self.page_menu = Some(r.rect.left_bottom() + vec2(-220.0, 6.0));
                self.fresh = true;
            }
            let (can_u, can_r) = self.can_undo_redo(cur);
            if icon_button(ui, pal, Icon::Sparkles, 16.0, "Ask Lumen AI  (Ctrl+Shift+J)").clicked() {
                self.run(PalAct::Ai);
            }
            let unread = self.ws.auto.unread();
            let br = icon_button(ui, pal, Icon::Bell, 16.0, "Notifications & automations");
            if unread > 0 {
                let c = br.rect.right_top() + vec2(-6.0, 7.0);
                ui.painter().circle_filled(c, 7.5, pal.danger);
                ui.painter().text(c, Align2::CENTER_CENTER, if unread > 9 { "9+".to_owned() } else { unread.to_string() }, bold(9.5), Color32::WHITE);
            }
            if br.clicked() {
                crate::autoui::open_tab(if unread > 0 { crate::autoui::TAB_INBOX } else { crate::autoui::TAB_RULES });
                crate::views::toggle(crate::views::Mode::Automations, self.ws.current);
            }
            let (lock_icon, tip) = if self.cred.is_some() { (Icon::Lock, "Lock workspace  (Ctrl+L)") } else { (Icon::Shield, "Set a password") };
            if !narrow {
                if icon_button(ui, pal, lock_icon, 16.0, tip).clicked() {
                    self.run(PalAct::Lock);
                }
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
            if !narrow {
                if ui.add_enabled_ui(can_r, |ui| icon_button(ui, pal, Icon::Redo, 16.0, "Redo  (Ctrl+Shift+Z)")).inner.clicked() {
                    self.run(PalAct::Redo);
                }
                if ui.add_enabled_ui(can_u, |ui| icon_button(ui, pal, Icon::Undo, 16.0, "Undo  (Ctrl+Z)")).inner.clicked() {
                    self.run(PalAct::Undo);
                }
                if icon_button(ui, pal, Icon::History, 16.0, "Page history").clicked() {
                    self.run(PalAct::History);
                }
                ui.add_space(6.0);
                ui.label(RichText::new(format!("Edited {}", ago(updated))).color(pal.muted).font(body(12.5)));
            }
        });
    }

    // ───────────── overlays ─────────────

    fn palette_items(&self, q: &str) -> Vec<(PalAct, String, String, Option<String>)> {
        let q = q.trim().to_lowercase();
        let mut out = vec![];
        let title_of = |p: &Page| if p.title.is_empty() { "Untitled".to_owned() } else { p.title.clone() };
        let icon_of = |p: &Page| if p.icon.is_empty() { l::FILE_TEXT.to_owned() } else { icons::icon_text(&p.icon).to_owned() };
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
        let actions: [(PalAct, &str, &str, &str); 27] = [
            (PalAct::Ai, l::SPARKLES, "Ask Lumen AI", "Chat with the agent that can build pages, databases, dashboards and automations  (Ctrl+Shift+J)"),
            (PalAct::AiSettings, l::KEY_ROUND, "AI settings & agent connections", "API keys (Gemini, OpenAI, Claude, OpenRouter …), access level, MCP server"),
            (PalAct::Automations, l::ZAP, "Automations", "Rules, buttons, recurring tasks, reminders  (Ctrl+Shift+A)"),
            (PalAct::NewReminder, l::BELL, "New reminder", "Get a desktop / push notification at a set time"),
            (PalAct::Dashboard, l::LAYOUT_DASHBOARD, "Dashboard", "KPIs, pipeline charts, tasks and deadlines  (Ctrl+D)"),
            (PalAct::Graph, l::NETWORK, "Graph view", "Map of how your notes connect  (Ctrl+G)"),
            (PalAct::Undo, l::UNDO_2, "Undo", "Undo the last block change  (Ctrl+Z)"),
            (PalAct::Redo, l::REDO_2, "Redo", "Redo  (Ctrl+Shift+Z)"),
            (PalAct::History, l::HISTORY, "Page history", "Browse, name and restore earlier versions"),
            (PalAct::Print, l::PRINTER, "Print / Save as PDF", "Print-ready version with images and databases"),
            (PalAct::NewPage, l::PLUS, "New page", "Create a blank page"),
            (PalAct::NewDb, l::KANBAN, "New database", "Table, board, list, gallery or calendar"),
            (PalAct::NewCanvas, l::NETWORK, "New canvas", "Infinite whiteboard of cards, pages, images and connectors"),
            (PalAct::NewFlow, l::ZAP, "New flow", "Node-based automation: APIs, data, logic and your workspace, wired together"),
            (PalAct::LiveDemo, l::ZAP, "Live data demo", "A mock API stream → flow → database → dashboard that updates every second"),
            (PalAct::Journal, l::BOOK_OPEN, "Today's journal", "Open or create today's entry  (Ctrl+J)"),
            (PalAct::Templates, l::LAYOUT_TEMPLATE, "Browse templates", "Meeting notes, trackers, wiki…"),
            (PalAct::Import, l::FILE_DOWN, "Import…", "Markdown, CSV or a workspace backup"),
            (PalAct::ExportMd, l::FILE_UP, "Export page as Markdown", "Download the current page"),
            (PalAct::ExportCsv, l::FILE_SPREADSHEET, "Export database as CSV", "Download the current database"),
            (PalAct::Backup, l::PACKAGE, "Back up workspace", "Download everything as JSON"),
            (PalAct::Focus, l::FOCUS, "Toggle focus mode", "Hide sidebar and top bar  (Ctrl+.)"),
            (PalAct::Theme, l::SUN_MOON, "Toggle theme", "Switch between light and dark"),
            (PalAct::Lock, l::LOCK, "Lock workspace", "Require your password again"),
            (PalAct::Sidebar, l::PANEL_LEFT, "Toggle sidebar", "Show or hide the sidebar"),
            (PalAct::Trash, l::TRASH_2, "Open trash", "Restore deleted pages"),
            (PalAct::Help, l::KEYBOARD, "Keyboard shortcuts", "Everything you can do without the mouse  (Ctrl+/)"),
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
        let title = page.title.clone();
        let items = [
            CtxItem::new(l::FILE_TEXT, "Open"),
            CtxItem::new(l::STAR, if fav { "Remove from favorites" } else { "Add to favorites" }),
            CtxItem::new(l::PENCIL, "Rename").hint("F2"),
            CtxItem::new(l::COPY_PLUS, "Duplicate"),
            CtxItem::new(l::FILE_PLUS, "Add sub-page"),
            CtxItem::new(l::COPY, "Copy title").enabled(!title.is_empty()),
            CtxItem::new(l::TRASH_2, "Move to trash").danger().sep(),
        ];
        let (open, pick) = context_menu(ctx, Id::new("row_menu"), pos, pal, &mut self.fresh, &items);
        if !open {
            self.row_menu = None;
        }
        match pick {
            Some(0) => self.ws.current = Some(id),
            Some(1) => {
                if let Some(p) = self.ws.page_mut(id) {
                    p.favorite = !p.favorite;
                }
            }
            Some(2) => {
                self.ws.current = Some(id);
                self.focus_title = Some(id);
            }
            Some(3) => {
                self.ws.duplicate(id);
            }
            Some(4) => self.new_page(Some(id), PageKind::Doc),
            Some(5) => ctx.copy_text(title),
            Some(6) => {
                self.ws.set_trashed(id, true);
                self.toast("Moved to trash", Some(id));
            }
            _ => {}
        }
    }

    /// Right-click on empty sidebar space.
    fn side_menu_ui(&mut self, ctx: &Context, pal: &Palette) {
        let Some(pos) = self.side_menu else { return };
        let items = [
            CtxItem::new(l::FILE_PLUS, "New page").hint("Ctrl+N"),
            CtxItem::new(l::KANBAN, "New database"),
            CtxItem::new(l::NETWORK, "New canvas"),
            CtxItem::new(l::ZAP, "New flow"),
            CtxItem::new(l::BOOK_OPEN, "Today's journal").hint("Ctrl+J"),
            CtxItem::new(l::LAYOUT_TEMPLATE, "Templates…").sep(),
            CtxItem::new(l::FILE_DOWN, "Import…"),
            CtxItem::new(l::TRASH_2, "Open trash"),
        ];
        let (open, pick) = context_menu(ctx, Id::new("side_menu"), pos, pal, &mut self.fresh, &items);
        if !open {
            self.side_menu = None;
        }
        match pick {
            Some(0) => self.new_page(None, PageKind::Doc),
            Some(1) => self.new_page(None, PageKind::Database),
            Some(2) => self.new_page(None, PageKind::Canvas),
            Some(3) => self.new_page(None, PageKind::Flow),
            Some(4) => self.run(PalAct::Journal),
            Some(5) => self.templates_open = true,
            Some(6) => self.run(PalAct::Import),
            Some(7) => self.trash_open = true,
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
            if menu_item(ui, pal, l::SUN_MOON, if dark { "Light mode" } else { "Dark mode" }, None, false, false).clicked() {
                act = Some(0);
            }
            if has_pw && menu_item(ui, pal, l::LOCK, "Lock now", None, false, false).clicked() {
                act = Some(1);
            }
            if menu_item(ui, pal, l::KEY_ROUND, if has_pw { "Change or remove password…" } else { "Set a password…" }, None, false, false).clicked() {
                act = Some(2);
            }
            if menu_item(ui, pal, l::LAYOUT_TEMPLATE, "Templates…", None, false, false).clicked() {
                act = Some(4);
            }
            if menu_item(ui, pal, l::FILE_DOWN, "Import Markdown / CSV / backup…", None, false, false).clicked() {
                act = Some(5);
            }
            if menu_item(ui, pal, l::PACKAGE, "Back up workspace (download JSON)", None, false, false).clicked() {
                act = Some(6);
            }
            if menu_item(ui, pal, l::COPY, "Copy workspace as JSON", None, false, false).clicked() {
                act = Some(3);
            }
            if menu_item(ui, pal, l::KEYBOARD, "Keyboard shortcuts", None, false, false).clicked() {
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
            .map(|p| (p.id, if p.title.is_empty() { "Untitled".into() } else { p.title.clone() }, icons::icon_text(&p.icon).to_owned()))
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
                    ui.label(RichText::new(l::TRASH_2).font(body(34.0)).color(pal.muted));
                    ui.label(RichText::new("Trash is empty").color(pal.muted));
                });
                ui.add_space(30.0);
            }
            ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
                for (id, title, icon) in &trashed {
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
                    let t = ui.ctx().animate_bool_with_time(Id::new(("trash_row", *id)), ui.rect_contains_pointer(r), 0.1);
                    ui.painter().rect_filled(r, 7.0, with_alpha(pal.hover, t));
                    ui.painter().text(pos2(r.left() + 20.0, r.center().y), Align2::CENTER_CENTER, if icon.is_empty() { l::FILE_TEXT } else { icon }, body(16.0), pal.text);
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

    /// Phone top bar: ☰ · page title ▾ (opens the hierarchy popup) · undo · redo · ⋯ (page menu: search, favourite, theme, history …).
    /// Every button is laid out from a computed width, so nothing is ever pushed off the screen.
    fn topbar_narrow(&mut self, ui: &mut Ui, pal: &Palette, cur: u64) {
        let sw = ui.ctx().screen_rect().width();
        let sp = ui.spacing().item_spacing.x;
        let btn = 38.0;
        let title_w = (sw - 8.0 - btn * 6.0 - sp * 8.0 - 28.0).clamp(60.0, 520.0);
        if let Some(p) = self.ws.page(cur) {
            let title = if p.title.is_empty() { "Untitled".to_owned() } else { p.title.clone() };
            let icon = p.icon.clone();
            let cr = crumb(ui, pal, &icon, &title, true, title_w - 24.0);
            let (cr2, ch) = ui.allocate_exact_size(vec2(24.0, 36.0), Sense::click());
            ui.painter().text(cr2.center(), Align2::CENTER_CENTER, l::CHEVRON_DOWN, body(14.0), pal.muted);
            if cr.clicked() || ch.clicked() {
                self.hier = true;
                self.hier_init = false;
                reset_appear(ui.ctx(), Id::new("hier").with("a"));
            }
            if let Some(pos) = crate::widgets::right_clicked(ui, cr.rect) {
                self.row_menu = Some((cur, pos));
                self.fresh = true;
            }
        }
        let (can_u, can_r) = self.can_undo_redo(cur);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(4.0);
            let r = icon_button(ui, pal, Icon::Dots, 18.0, "Page options");
            if r.clicked() {
                self.page_menu = Some(r.rect.left_bottom() + vec2(-230.0, 6.0));
                self.fresh = true;
            }
            if icon_button(ui, pal, Icon::Sparkles, 18.0, "Ask Lumen AI").clicked() {
                self.run(PalAct::Ai);
            }
            let unread = self.ws.auto.unread();
            let br = icon_button(ui, pal, Icon::Bell, 18.0, "Notifications & automations");
            if unread > 0 {
                let c = br.rect.right_top() + vec2(-7.0, 8.0);
                ui.painter().circle_filled(c, 7.5, pal.danger);
                ui.painter().text(c, Align2::CENTER_CENTER, if unread > 9 { "9+".to_owned() } else { unread.to_string() }, bold(9.5), Color32::WHITE);
            }
            if br.clicked() {
                crate::autoui::open_tab(if unread > 0 { crate::autoui::TAB_INBOX } else { crate::autoui::TAB_RULES });
                crate::views::toggle(crate::views::Mode::Automations, self.ws.current);
            }
            if ui.add_enabled_ui(can_r, |ui| icon_button(ui, pal, Icon::Redo, 18.0, "Redo")).inner.clicked() {
                self.run(PalAct::Redo);
            }
            if ui.add_enabled_ui(can_u, |ui| icon_button(ui, pal, Icon::Undo, 18.0, "Undo")).inner.clicked() {
                self.run(PalAct::Undo);
            }
        });
    }

    /// The page hierarchy as a popup: the path to the current page as chips, and the whole page tree (expanded along the path).
    fn hier_ui(&mut self, ctx: &Context, pal: &Palette) {
        if !self.hier {
            return;
        }
        let Some(cur) = self.ws.current else {
            self.hier = false;
            return;
        };
        let path = self.ws.breadcrumb(cur);
        if !self.hier_init {
            self.hier_init = true;
            for id in &path {
                self.hier_exp.insert(*id);
            }
        }
        let mut go: Option<u64> = None;
        let mut tog: Option<u64> = None;
        let mut new_child = false;
        let max_h = (ctx.screen_rect().height() - 200.0).max(160.0);
        let open = {
            let ws = &self.ws;
            let exp = &self.hier_exp;
            modal(ctx, Id::new("hier"), pal, 460.0, if ctx.screen_rect().width() < 720.0 { 10.0 } else { 70.0 }, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(RichText::new("Where you are").font(bold(17.0)).color(pal.text));
                });
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(6.0);
                    ui.spacing_mut().item_spacing = vec2(4.0, 6.0);
                    for (i, id) in path.iter().enumerate() {
                        let Some(p) = ws.page(*id) else { continue };
                        let t = if p.title.is_empty() { "Untitled" } else { p.title.as_str() };
                        if crate::database::pill(ui, pal, t, *id == cur, false).clicked() {
                            go = Some(*id);
                        }
                        if i + 1 < path.len() {
                            ui.label(RichText::new(l::CHEVRON_RIGHT).font(body(13.0)).color(pal.muted));
                        }
                    }
                });
                ui.add_space(6.0);
                ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0_f32, pal.border));
                ui.add_space(4.0);
                egui::ScrollArea::vertical().id_salt("hier_scroll").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                    hier_rows(ui, pal, ws, exp, cur, None, 0, &mut go, &mut tog);
                });
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(6.0);
                    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                    if crate::database::pill(ui, pal, "+  New sub-page here", false, true).clicked() {
                        new_child = true;
                    }
                });
                ui.add_space(4.0);
            })
        };
        if let Some(id) = tog {
            if !self.hier_exp.remove(&id) {
                self.hier_exp.insert(id);
            }
        }
        if new_child {
            self.new_page(Some(cur), PageKind::Doc);
            self.hier = false;
            reset_appear(ctx, Id::new("hier").with("a"));
        } else if let Some(id) = go {
            self.ws.current = Some(id);
            self.hier = false;
            reset_appear(ctx, Id::new("hier").with("a"));
        } else if !open {
            self.hier = false;
            reset_appear(ctx, Id::new("hier").with("a"));
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
        let zoom0 = page.content_zoom();
        let mut zoom = zoom0;
        let has_dash = self.ws.dash.page_board(cur).is_some();
        let (words, chars) = (nio::word_count(page), nio::char_count(page));
        let mut act: Option<u8> = None;
        let (fav, dark, has_pw) = (page.favorite, self.ws.dark, self.cred.is_some());
        let phone = ctx.screen_rect().width() < 720.0;
        let open = popup_area(ctx, Id::new("page_menu"), pos, pal, &mut self.fresh, 262.0, |ui| {
            if phone {
                // the top bar of a phone only has room for undo / redo / this menu: everything else lives here
                section_label(ui, pal, "QUICK ACTIONS");
                if menu_item(ui, pal, l::SEARCH, "Search & commands", None, false, false).clicked() {
                    act = Some(10);
                }
                if menu_item(ui, pal, l::STAR, if fav { "Remove from favorites" } else { "Add to favorites" }, None, fav, false).clicked() {
                    act = Some(11);
                }
                if menu_item(ui, pal, if dark { l::SUN } else { l::MOON }, if dark { "Switch to light mode" } else { "Switch to dark mode" }, None, false, false).clicked() {
                    act = Some(12);
                }
                if menu_item(ui, pal, l::LOCK, if has_pw { "Lock workspace" } else { "Set a password" }, None, false, false).clicked() {
                    act = Some(13);
                }
                ui.add_space(3.0);
                ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0_f32, pal.border));
                ui.add_space(4.0);
            }
            section_label(ui, pal, "CONTENT SIZE");
            crate::widgets::size_control(ui, pal, &mut zoom);
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new("Resizes the page content only — menus, the sidebar and pop-ups keep their size.").font(body(11.5)).color(pal.muted));
            });
            ui.add_space(3.0);
            ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0_f32, pal.border));
            ui.add_space(4.0);
            section_label(ui, pal, "PAGE STYLE");

            if menu_item(ui, pal, l::TYPE, if small { "Normal text size" } else { "Small text" }, None, false, false).clicked() {
                act = Some(0);
            }
            if menu_item(ui, pal, l::MAXIMIZE, if full { "Standard page width" } else { "Full width" }, None, false, false).clicked() {
                act = Some(1);
            }
            if !is_db && menu_item(ui, pal, l::LAYOUT_DASHBOARD, if has_dash { "Remove dashboard" } else { "Make this page a dashboard" }, Some(if has_dash { "Keeps the page text" } else { "Adds a grid of widgets and charts" }), has_dash, false).clicked() {
                act = Some(9);
            }
            ui.add_space(3.0);
            ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0_f32, pal.border));
            ui.add_space(4.0);
            if menu_item(ui, pal, l::COPY_PLUS, "Duplicate", None, false, false).clicked() {
                act = Some(2);
            }
            if menu_item(ui, pal, l::FILE_UP, "Export as Markdown", Some("Downloads a .md file"), false, false).clicked() {
                act = Some(3);
            }
            if menu_item(ui, pal, l::COPY, "Copy as Markdown", None, false, false).clicked() {
                act = Some(4);
            }
            if is_db && menu_item(ui, pal, l::FILE_SPREADSHEET, "Export as CSV", Some("Downloads a .csv file"), false, false).clicked() {
                act = Some(5);
            }
            if menu_item(ui, pal, l::PRINTER, "Print / Save as PDF", Some("Images and databases included"), false, false).clicked() {
                act = Some(7);
            }
            if menu_item(ui, pal, l::HISTORY, "Page history", Some("Versions & restore"), false, false).clicked() {
                act = Some(8);
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
            if menu_item(ui, pal, l::TRASH_2, "Move to trash", None, false, true).clicked() {
                act = Some(6);
            }
        });
        if !open || act.is_some() {
            self.page_menu = None;
        }
        if (zoom - zoom0).abs() > 1e-4 {
            if let Some(p) = self.ws.page_mut(cur) {
                p.zoom = zoom;
            }
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
            Some(9) => {
                if has_dash {
                    self.ws.dash.drop_page_board(cur);
                    self.toast("Dashboard removed from this page", None);
                } else {
                    self.ws.dash.ensure_page_board(cur, "Dashboard");
                    if let Some(p) = self.ws.page_mut(cur) {
                        p.full_width = true;
                    }
                    self.toast("This page is now a dashboard — add widgets from the toolbar", None);
                }
            }
            Some(10) => self.palette = Some(PaletteState { query: String::new(), sel: 0, focused: false, pick: None }),
            Some(11) => {
                if let Some(p) = self.ws.page_mut(cur) {
                    p.favorite = !p.favorite;
                }
            }
            Some(12) => self.ws.dark = !self.ws.dark,
            Some(13) => self.run(PalAct::Lock),
            Some(3) => self.export_md(),
            Some(4) => {
                ctx.copy_text(nio::page_markdown(&self.ws, cur));
                self.toast("Markdown copied to clipboard", None);
            }
            Some(5) => self.export_csv(),
            Some(7) => self.print_page(),
            Some(8) => self.history = Some(crate::history::HistState::default()),
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
        let screen = ctx.screen_rect();
        let phone = screen.width() < 720.0;
        let max_h = (screen.height() - if phone { 130.0 } else { 220.0 }).max(180.0);
        let mut close = false;
        let open = modal(ctx, Id::new("templates"), pal, 640.0, if phone { 8.0 } else { 90.0 }, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new("Templates").font(bold(if phone { 18.0 } else { 20.0 })));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, pal, Icon::Close, 16.0, "Close").clicked() {
                        close = true;
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new("Start from a ready-made page or database. Everything is editable.").font(body(13.0)).color(pal.muted)).wrap());
            });
            ui.add_space(8.0);
            // Phones: one column of full-width, finger-sized cards in a scrolling list (the fixed 296 pt grid used to run off the screen).
            let avail = ui.available_width();
            let (cw, ch) = if phone { (avail - 6.0, 78.0) } else { (296.0, 74.0) };
            egui::ScrollArea::vertical().id_salt("templates_scroll").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
                    for (i, (name, icon, desc)) in TEMPLATES.iter().enumerate() {
                        let (r, resp) = ui.allocate_exact_size(vec2(cw, ch), Sense::click());
                        let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() || resp.is_pointer_button_down_on(), 0.12);
                        ui.painter().rect_filled(r, 10.0, with_alpha(pal.hover, 0.5 + 0.5 * t));
                        ui.painter().rect_stroke(r, 10.0, Stroke::new(1.2_f32, lerp_color(pal.border, pal.accent, t)), egui::StrokeKind::Inside);
                        ui.painter().text(pos2(r.left() + 30.0, r.center().y), Align2::CENTER_CENTER, *icon, body(28.0), pal.text);
                        let tx = r.left() + 62.0;
                        let tw = (r.right() - 12.0 - tx).max(60.0);
                        crate::widgets::text_fit(ui.painter(), pos2(tx, r.top() + 22.0), Align2::LEFT_CENTER, name, bold(14.5), pal.text, tw);
                        let mut job = egui::text::LayoutJob::simple(desc.to_string(), body(12.0), pal.muted, tw);
                        job.wrap.max_rows = 2;
                        let g = ui.painter().layout_job(job);
                        ui.painter().galley(pos2(tx, r.top() + 36.0), g, pal.muted);
                        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            pick = Some(i);
                        }
                    }
                });
            });
            ui.add_space(6.0);
        });
        if let Some(i) = pick {
            let id = self.ws.add_template(i, None);
            self.focus_title = Some(id);
            self.templates_open = false;
            reset_appear(ctx, Id::new("templates").with("a"));
        } else if !open || close {
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
                ui.label(RichText::new("Paste Markdown, CSV or a Lumen backup, choose a file from your device — or drop a file onto the window.").font(body(13.5)).color(pal.muted));
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
            if platform::can_pick_files() {
                // choose a file from the device instead of pasting
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(8.0);
                    ui.label(RichText::new("Or choose a file:").font(body(13.5)).color(pal.muted));
                    if crate::widgets::ghost_button(ui, pal, "Markdown / text file…").clicked() {
                        platform::pick_file(ui.ctx(), PickTarget::ImportFile { mode: 0 }, false);
                    }
                    if crate::widgets::ghost_button(ui, pal, "CSV / TSV file…").clicked() {
                        platform::pick_file(ui.ctx(), PickTarget::ImportFile { mode: 1 }, false);
                    }
                    if crate::widgets::ghost_button(ui, pal, "Backup (.json)…").clicked() {
                        platform::pick_file(ui.ctx(), PickTarget::ImportFile { mode: 2 }, false);
                    }
                });
                ui.add_space(8.0);
            }
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
                    self.auto_rt.resync(&self.ws);
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
        if age < 0.3 || age > 4.1 {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
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
        // ── data safety ──
        // 1. a state that could not be read is never thrown away: its raw text is kept under its own key
        if let Some(raw) = self.rescued.take() {
            storage.set_string(UNREADABLE_KEY, raw);
        }
        let old = storage.get_string(STORAGE_KEY).filter(|o| !o.trim().is_empty());
        if let Some(old) = old {
            // 2. first save after an app update: keep the previous version's data untouched in its own slot
            let ver = env!("CARGO_PKG_VERSION");
            if storage.get_string(VERSION_KEY).as_deref() != Some(ver) {
                if let Some(prev_ver) = storage.get_string(VERSION_KEY) {
                    storage.set_string(UPGRADE_KEY, old.clone());
                    storage.set_string(UPGRADE_VER_KEY, prev_ver);
                }
                storage.set_string(VERSION_KEY, ver.to_owned());
            }
            // 3. rolling "previous state" slot: at most one rotation every 10 minutes, so it is always a state from before the latest edits
            let now = now_ms();
            let last: f64 = storage.get_string(PREV_AT_KEY).and_then(|t| t.parse().ok()).unwrap_or(0.0);
            if now - last > 10.0 * 60.0 * 1000.0 {
                storage.set_string(PREV_KEY, old);
                storage.set_string(PREV_AT_KEY, format!("{now}"));
            }
        } else {
            storage.set_string(VERSION_KEY, env!("CARGO_PKG_VERSION").to_owned());
        }
        eframe::set_value(storage, STORAGE_KEY, &SavedRef { ws: &self.ws, cred: &self.cred, skipped: self.skipped_setup });
        self.media.save(storage);
        eframe::set_value(storage, crate::ai::STORAGE_KEY, &self.ai.st.for_save());
    }

    fn auto_save_interval(&self) -> Duration {
        // Serialising the whole workspace is the most expensive thing an idle app does: every 30 s is plenty
        // (eframe also saves when the window is closed).
        Duration::from_secs(30)
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.ws.dark || self.locked { [0.098, 0.098, 0.098, 1.0] } else { [1.0, 1.0, 1.0, 1.0] }
    }

    fn raw_input_hook(&mut self, ctx: &Context, raw_input: &mut egui::RawInput) {
        // Right-clicks are handled by us (context menus) and never reach egui's text fields.
        crate::widgets::filter_raw_input(ctx, raw_input);
        // Finger gestures: tap / swipe-to-scroll with fling / long-press to drag (touch.rs).
        crate::touch::on_raw_input(ctx, raw_input);
        // Android: exact pixel density, safe area (status bar / navigation bar / keyboard), soft-keyboard text and keys.
        #[cfg(target_os = "android")]
        crate::android::on_raw_input(ctx, raw_input);
    }

    fn update(&mut self, ctx: &Context, frame: &mut eframe::Frame) {
        self.ed.embed_rects.clear();
        self.load_script_fonts(ctx);
        #[cfg(target_os = "android")]
        self.android_back(ctx);
        self.frame(ctx);
        self.sync_embeds(frame);
        crate::touch::paint_hold(ctx);
        {
            let target = if self.locked || self.ws.dark { 1.0 } else { 0.0 };
            let t = ctx.animate_value_with_time(Id::new("theme_t"), target, 0.35);
            let pal = theme::with_accent(Palette::mix(t), self.ws.accent, t);
            crate::widgets::universal_text_menu(ctx, &pal);
        }
        crate::widgets::end_frame(ctx);
        #[cfg(target_os = "android")]
        crate::android::after_frame(ctx, self.ws.dark || self.locked);
    }
}

#[cfg(target_os = "android")]
impl LumenApp {
    /// The Android Back button / gesture, handled like a native app: close the drawer, close the top-most dialog or menu, leave the text
    /// field, go up one level in the page tree — and only then send the app to the background (never destroy it).
    fn android_back(&mut self, ctx: &Context) {
        if !crate::android::take_back() {
            return;
        }
        let narrow = ctx.screen_rect().width() < 720.0;
        if self.locked {
            crate::android::go_home();
            return;
        }
        if narrow && self.ws.sidebar_open {
            self.ws.sidebar_open = false;
            return;
        }
        if self.ai.rt.settings.is_some() {
            self.ai.rt.settings = None;
            return;
        }
        if self.ai.rt.open && narrow {
            self.ai.rt.open = false;
            return;
        }
        let overlay = self.history.is_some()
            || self.palette.is_some()
            || self.help_open
            || self.import.is_some()
            || self.templates_open
            || self.trash_open
            || self.security.is_some()
            || self.ed.pdf_req.is_some()
            || self.ed.menu.is_some()
            || self.ed.block_menu.is_some()
            || self.ed.icon_picker.is_some()
            || self.ed.cover_picker.is_some();
        if overlay || ctx.memory(|m| m.any_popup_open()) {
            crate::android::push_key(egui::Key::Escape);
            return;
        }
        if let Some(id) = ctx.memory(|m| m.focused()) {
            ctx.memory_mut(|m| m.surrender_focus(id));
            ctx.request_repaint();
            return;
        }
        if let Some(parent) = self.ws.current.and_then(|c| self.ws.page(c)).and_then(|p| p.parent) {
            if self.ws.page(parent).is_some_and(|p| !p.trashed) {
                self.ws.current = Some(parent);
                ctx.request_repaint();
                return;
            }
        }
        crate::android::go_home();
    }
}

impl LumenApp {
    /// Runs the automation engine (rules, recurring tasks, reminders) a few times per second — also while the workspace is locked.
    fn run_automation(&mut self, ctx: &Context) {
        let t = now_ms();
        if t - self.last_auto < 250.0 {
            return;
        }
        self.last_auto = t;
        let o = crate::automation::tick(&mut self.ws, &mut self.auto_rt, t, crate::dates::now_local_min());
        self.after_auto(ctx, o);
        // web: hand the reminders of the next 14 days to the page (own timers, even in a background tab); Android: to AlarmManager (fires while the app is closed)
        if t - self.last_push > 5000.0 {
            self.last_push = t;
            if self.ws.auto.settings.os_notify {
                if let Some(json) = crate::automation::push_schedule(&self.ws, &mut self.auto_rt, crate::dates::now_local_min(), 24 * 14) {
                    platform::schedule_push(&json);
                }
            }
        }
    }

    /// Toasts + system notifications for what the engine produced.
    fn after_auto(&mut self, ctx: &Context, o: crate::automation::Output) {
        if o.changed {
            self.calc_dirty = true;
            ctx.request_repaint();
        }
        let minute = crate::dates::now_local_min().rem_euclid(1440) as u16;
        let quiet = self.ws.auto.settings.is_quiet(minute);
        for n in o.notices {
            if !self.locked {
                self.toast(if n.body.is_empty() { n.title.clone() } else { format!("{} — {}", n.title, n.body) }, None);
            }
            if self.ws.auto.settings.os_notify && !quiet {
                if self.locked {
                    platform::notify_os("Lumen", "You have a new notification", &format!("lumen-n-{}", n.id));
                } else {
                    platform::notify_os(&n.title, &n.body, &format!("lumen-n-{}", n.id));
                }
            }
        }
    }

    /// Lumen AI: network answers, the agent loop, MCP requests — and what they ask of the app (open a page, toasts, formula refresh).
    fn run_ai(&mut self, ctx: &Context) {
        let changed = self.ai.tick(&mut self.ws, ctx, self.locked);
        if changed {
            self.calc_dirty = true;
            ctx.request_repaint();
        }
        if self.ai.rt.undone {
            self.ai.rt.undone = false;
            self.auto_rt.resync(&self.ws);
            self.calc_dirty = true;
        }
        if let Some(t) = self.ai.rt.toast.take() {
            self.toast(t, None);
        }
        if let Some(id) = self.ai.rt.open_req.take() {
            // wide screens follow the agent live; on phones the chat covers the page, so the card's Open button does it
            if ctx.screen_rect().width() >= 720.0 && !self.locked {
                crate::views::set(crate::views::Mode::Page, None);
                self.ws.current = Some(id);
            }
        }
        if self.ai.rt.mcp.wants_fast_repaint(&self.ai.st.mcp) {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn frame(&mut self, ctx: &Context) {
        self.run_automation(ctx);
        // repeating flows / scripts keep running whatever page is open
        {
            let t = now_ms();
            let shown_flow = self.ws.current.filter(|c| !self.locked && crate::views::mode() == crate::views::Mode::Page && self.ws.page(*c).map_or(false, |p| p.kind == PageKind::Flow));
            let o = self.bg.tick(&mut self.ws, ctx, t, shown_flow);
            if o.output.changed || !o.output.notices.is_empty() {
                self.after_auto(ctx, o.output);
            }
            for m in o.toasts {
                self.toast(m, None);
            }
        }
        self.run_ai(ctx);
        // theme (animated)
        let target = if self.locked || self.ws.dark { 1.0 } else { 0.0 };
        let t = ctx.animate_value_with_time(Id::new("theme_t"), target, 0.35);
        let pal = theme::with_accent(Palette::mix(t), self.ws.accent, t);
        let touchy = crate::touch::touchy();
        crate::touch::set_narrow(ctx.screen_rect().width() < 720.0);
        if !self.theme_applied || (t - self.last_theme_t).abs() > 1e-4 || self.last_accent != self.ws.accent || self.last_touch != touchy {
            self.last_touch = touchy;
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
        // Idle wake-ups: only the auto-lock timer needs one, and only when a password is set.
        let busy = !self.ws.auto.rules.is_empty() || self.ws.auto.reminders.iter().any(|r| !r.done);
        ctx.request_repaint_after(Duration::from_secs(if busy { 4 } else if self.cred.is_some() { 10 } else { 30 }));

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
                    self.auto_rt.resync(&self.ws);
                    self.cred = None;
                    self.skipped_setup = false;
                    self.lock = LockScreen::new();
                }
            }
            return;
        }

        self.ws.migrate_icons();
        self.poll_media(ctx);

        // global shortcuts (suspended while the PDF reader is open — it has its own keys)
        if self.ed.pdf_req.is_none() {
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::L)) {
            self.ws.dark = !self.ws.dark;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::J)) {
            self.run(PalAct::Ai);
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
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::G)) {
            self.run(PalAct::Graph);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::D)) && self.ed.sel.is_empty() {
            self.run(PalAct::Dashboard);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::A)) {
            self.run(PalAct::Automations);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::E)) {
            self.export_md();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::P)) {
            self.palette = Some(PaletteState { query: String::new(), sel: 0, focused: false, pick: None });
        }
        }

        // files dropped onto the window
        let dropped: Vec<egui::DroppedFile> = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            let name = platform::file_name(&f);
            let lower = name.to_lowercase();
            let Some(bytes) = platform::dropped_bytes(&f) else { continue };
            let stem = name.rsplit_once('.').map(|x| x.0.to_owned()).unwrap_or(name.clone());
            if media::is_image_name(&lower) {
                if let (Some(cur), true) = (self.ws.current, self.ed.cover_picker.is_some()) {
                    self.set_media(ctx, PickTarget::Cover { page: cur }, &name, bytes);
                } else if let (Some(cur), true) = (self.ws.current, self.ed.icon_picker.is_some()) {
                    self.set_media(ctx, PickTarget::Icon { page: cur }, &name, bytes);
                } else {
                    self.add_media_block(ctx, BlockKind::Image, &name, bytes);
                }
                continue;
            }
            let text = String::from_utf8_lossy(&bytes).to_string();
            if lower.ends_with(".csv") || lower.ends_with(".tsv") {
                self.import_text(&stem, &text, true);
            } else if lower.ends_with(".json") {
                self.import = Some(ImportDialog { name: stem, text });
            } else if lower.ends_with(".md") || lower.ends_with(".markdown") || lower.ends_with(".txt") {
                self.import_text(&stem, &text, false);
            } else {
                self.add_media_block(ctx, BlockKind::File, &name, bytes);
            }
        }

        // sidebar — a docked panel on wide screens, a slide-over drawer on phones / narrow windows
        let screen_w = ctx.screen_rect().width();
        let narrow = screen_w < 720.0;
        if !ctx.data(|d| d.get_temp::<bool>(Id::new("narrow_init")).unwrap_or(false)) {
            ctx.data_mut(|d| d.insert_temp(Id::new("narrow_init"), true));
            if narrow {
                self.ws.sidebar_open = false;
            }
        }
        if narrow && self.prev_sidebar_page != self.ws.current {
            self.ws.sidebar_open = false;
        }
        self.prev_sidebar_page = self.ws.current;
        // swipe from the left edge opens the drawer, swipe left closes it
        match crate::touch::take_swipe() {
            Some(crate::touch::Swipe::EdgeRight) if narrow && !self.ws.sidebar_open => self.ws.sidebar_open = true,
            Some(crate::touch::Swipe::Left) if narrow && self.ws.sidebar_open => self.ws.sidebar_open = false,
            _ => {}
        }
        let st = ctx.animate_bool_with_time_and_easing(Id::new("sidebar_t"), self.ws.sidebar_open && !self.focus_mode, 0.34, egui::emath::easing::cubic_in_out);
        // The page is laid out ONCE at the width it will have when the sidebar has finished moving and then simply slides along with the
        // sidebar edge (no text re-wrapping on every frame = no stutter). `None` = not animating: lay out at the real panel width.
        let frozen_w: Option<f32> = if narrow {
            (st > 0.003).then_some(screen_w)
        } else if st > 0.003 && st < 0.997 {
            Some(if self.ws.sidebar_open && !self.focus_mode { screen_w - SIDEBAR_W } else { screen_w })
        } else {
            None
        };
        let ai_target = self.ai.rt.open && !self.locked && ctx.screen_rect().width() >= 720.0;
        let ai_t = ctx.animate_bool_with_time_and_easing(Id::new("ai_slide_t"), ai_target, 0.32, egui::emath::easing::cubic_in_out);
        let ai_w: f32 = ctx.data(|d| d.get_temp::<f32>(Id::new("ai_panel_w"))).unwrap_or(400.0).clamp(320.0, 680.0);
        // the page is laid out once at the width it will have when the AI panel has finished sliding
        let frozen_w: Option<f32> = if frozen_w.is_none() && ai_t > 0.003 && ai_t < 0.997 {
            let side = if !narrow && self.ws.sidebar_open && !self.focus_mode { SIDEBAR_W } else { 0.0 };
            Some(screen_w - side - if ai_target { ai_w } else { 0.0 })
        } else {
            frozen_w
        };
        if narrow {
            if st > 0.003 {
                let sw = SIDEBAR_W.min(screen_w * 0.88);
                let screen = ctx.screen_rect();
                // the drawer pushes the page (and the top bar) to the right in step with its own edge
                egui::SidePanel::left("drawer_push").resizable(false).show_separator_line(false).exact_width(sw * st).frame(Frame::NONE).show(ctx, |_| {});
                let bd = Area::new(Id::new("drawer_backdrop")).order(Order::Middle).fixed_pos(screen.min).show(ctx, |ui| {
                    let (r, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                    ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha((110.0 * st) as u8));
                    if resp.clicked() {
                        self.ws.sidebar_open = false;
                    }
                });
                ctx.raise_layer(bd.response.layer_id);
                let dr = Area::new(Id::new("drawer")).order(Order::Middle).fixed_pos(pos2(screen.min.x - (1.0 - st) * sw, screen.min.y)).show(ctx, |ui| {
                    Frame::new().fill(pal.sidebar).inner_margin(Margin::symmetric(8, 0)).shadow(egui::epaint::Shadow { offset: [3, 0], blur: 20, spread: 0, color: Color32::from_black_alpha((70.0 * st) as u8) }).show(ui, |ui| {
                        ui.set_min_size(vec2(sw - 16.0, screen.height()));
                        ui.set_max_width(sw - 16.0);
                        self.sidebar(ui, &pal);
                    });
                });
                ctx.raise_layer(dr.response.layer_id);
            }
        } else if st > 0.003 {
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
            // Short screens (a phone in landscape, or a portrait phone with the keyboard up) get a slimmer bar so the page keeps its room.
            let bar_h = if crate::touch::touchy() && ctx.screen_rect().height() < 440.0 { 40.0 } else { 46.0 };
            egui::TopBottomPanel::top("topbar")
                .exact_height(bar_h)
                .frame(Frame::new().fill(pal.bg))
                .show(ctx, |ui| {
                    frozen_layout(ui, frozen_w, |ui| {
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| self.topbar(ui, &pal));
                    });
                });
        }

        // AI panel: slides in / out (the content keeps its width and is revealed from the right edge, so nothing re-wraps while it moves)
        if ai_t > 0.003 {
            let animating = ai_t < 0.997;
            let mut out = crate::aiui::PanelOut::default();
            let mut panel = egui::SidePanel::right("ai_panel").frame(Frame::new().fill(crate::aiui::panel_bg(&pal)).shadow(egui::epaint::Shadow { offset: [-4, 0], blur: 24, spread: 0, color: Color32::from_black_alpha((38.0 * (1.0 - ai_t * 0.6)) as u8) }));
            panel = if animating { panel.resizable(false).exact_width(ai_w * ai_t) } else { panel.resizable(true).default_width(ai_w).width_range(320.0..=680.0) };
            let shown = panel.show(ctx, |ui| {
                let r = ui.max_rect();
                let lw = if animating { ai_w } else { r.width() };
                let lr = Rect::from_min_size(pos2(r.right() - lw, r.top()), vec2(lw, r.height()));
                let clip = ui.clip_rect().intersect(r);
                ui.scope_builder(egui::UiBuilder::new().id_salt("ai_content").max_rect(lr), |ui| {
                    ui.set_clip_rect(clip);
                    out = crate::aiui::panel(ui, ctx, &mut self.ai, &mut self.ws, &pal);
                });
            });
            if !animating {
                ctx.data_mut(|d| d.insert_temp(Id::new("ai_panel_w"), shown.response.rect.width()));
            }
            if out.close {
                self.ai.rt.open = false;
            }
            if let Some(id) = out.open_page {
                crate::views::set(crate::views::Mode::Page, None);
                self.ws.current = Some(id);
            }
        }
        crate::views::sync(self.ws.current);
        if let Some(t) = crate::dashcfg::take_toast() {
            self.toast(t, None);
        }
        egui::CentralPanel::default().frame(Frame::new().fill(pal.bg)).show(ctx, |ui| frozen_layout(ui, frozen_w, |ui| {
            match crate::views::mode() {
                crate::views::Mode::Dashboard => {
                    let nav = crate::dashboard::show(ui, &mut self.ws, &pal);
                    if let Some(t) = nav.toast.clone() {
                        self.toast(t, None);
                    }
                    if nav.new_page {
                        crate::views::set(crate::views::Mode::Page, None);
                        self.new_page(None, PageKind::Doc);
                    } else if nav.new_db {
                        crate::views::set(crate::views::Mode::Page, None);
                        self.new_page(None, PageKind::Database);
                    } else if nav.journal {
                        crate::views::set(crate::views::Mode::Page, None);
                        self.ws.open_today();
                    } else if nav.graph {
                        crate::views::set(crate::views::Mode::Graph, self.ws.current);
                    }
                    if let Some(id) = nav.out.open {
                        crate::views::set(crate::views::Mode::Page, None);
                        self.ws.current = Some(id);
                    }
                    return;
                }
                crate::views::Mode::Graph => {
                    let out = crate::graph::show(ui, &self.ws, &pal);
                    if out.close {
                        crate::views::set(crate::views::Mode::Page, None);
                    }
                    if let Some(id) = out.open {
                        crate::views::set(crate::views::Mode::Page, None);
                        self.ws.current = Some(id);
                    }
                    return;
                }
                crate::views::Mode::Automations => {
                    let out = crate::autoui::show(ui, &mut self.ws, &mut self.auto_rt, &pal);
                    if out.close {
                        crate::views::set(crate::views::Mode::Page, None);
                    }
                    if let Some(id) = out.open {
                        crate::views::set(crate::views::Mode::Page, None);
                        self.ws.current = Some(id);
                    }
                    if let Some(t) = out.toast {
                        self.toast(t, None);
                    }
                    return;
                }
                crate::views::Mode::Page => {}
            }
            let Some(cur) = self.ws.current.filter(|id| self.ws.page(*id).map_or(false, |p| !p.trashed)) else {
                ui.add_space(ui.available_height() * 0.3);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(l::FILE_TEXT).font(body(46.0)).color(pal.muted));
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
            // canvas pages are a full-bleed whiteboard, not a scrolling document
            if self.ws.page(cur).map_or(false, |p| p.kind == PageKind::Canvas) {
                crate::canvasui::fix_once(&mut self.ws);
                let out = crate::canvasui::show(ui, &mut self.ws, cur, &pal);
                if let Some(id) = out.open {
                    if self.ws.page(id).map_or(false, |p| !p.trashed) {
                        self.ws.current = Some(id);
                    }
                }
                if let Some(t) = out.toast {
                    self.toast(t, None);
                }
                return;
            }
            if self.ws.page(cur).map_or(false, |p| p.kind == PageKind::Flow) {
                crate::flowui::fix_once(&mut self.ws);
                let out = crate::flowui::show(ui, &mut self.ws, cur, &pal);
                if let Some(o) = out.output {
                    self.after_auto(ctx, o);
                }
                if let Some(t) = out.toast {
                    self.toast(t, None);
                }
                return;
            }
            ui.add_space((1.0 - a) * 14.0);
            ui.set_opacity(a);

            let ws = &mut self.ws;
            let ed = &mut self.ed;
            let mut changed = false;
            // Physics scrolling (critically damped spring for wheel / touchpad, 1:1 + fling for fingers) — see smooth.rs / touch.rs.
            let over = ui.rect_contains_pointer(ui.max_rect()) && ed.menu.is_none() && ed.block_menu.is_none() && ed.link_edit.is_none();
            let sa = ScrollArea::vertical().id_salt(("page_scroll", cur)).drag_to_scroll(false).auto_shrink([false, false]);
            crate::smooth::scroll_area(ui, Id::new(("smooth_scroll", cur)), sa, over, |ui| {
                changed = editor::show_page(ui, ws, cur, ed, &pal);
            });
            if changed {
                ws.touch(cur);
            }
        }));

        // computed properties (formulas / rollups), undo stack and page versions
        let tnow = now_ms() / 1000.0;
        if ctx.input(|i| !i.events.is_empty()) || self.calc_dirty || tnow - self.last_calc > 1.5 {
            if tnow - self.last_calc > 0.12 || self.calc_dirty {
                if crate::formula::recompute(&mut self.ws) {
                    ctx.request_repaint();
                }
                self.last_calc = tnow;
                self.calc_dirty = false;
            }
        }
        if let Some(cur) = self.ws.current {
            crate::history::track(&mut self.ws, cur, &mut self.ed, tnow);
        }

        if let Some(id) = self.ed.nav.take() {
            if self.ws.page(id).map_or(false, |p| !p.trashed) {
                self.ws.current = Some(id);
            }
        }
        if let Some(title) = self.ed.wiki.take() {
            let key = title.trim().to_lowercase();
            let hit = self.ws.pages.iter().filter(|p| !p.trashed && p.title.trim().to_lowercase() == key).map(|p| (p.row, p.id)).min().map(|(_, id)| id);
            let id = hit.unwrap_or_else(|| {
                let id = self.ws.add_page(None, crate::model::PageKind::Doc);
                if let Some(p) = self.ws.page_mut(id) {
                    p.title = title.trim().to_owned();
                }
                id
            });
            crate::views::set(crate::views::Mode::Page, None);
            self.ws.current = Some(id);
        }
        if let (Some(req), Some(cur)) = (self.ed.req.take(), self.ws.current) {
            self.handle_req(req, cur);
        }

        // one-click buttons, the button editor and the repeat / remind popover of date properties
        if let (Some(bid), Some(cur)) = (self.ed.press_button.take(), self.ws.current) {
            let o = crate::automation::press_button(&mut self.ws, &mut self.auto_rt, cur, bid, now_ms());
            self.after_auto(ctx, o);
        }
        if let Some(bid) = self.ed.edit_button {
            let keep = match self.ws.current {
                Some(cur) => crate::autoui::button_editor(ctx, &mut self.ws, &mut self.auto_rt, cur, bid, &pal),
                None => false,
            };
            if !keep {
                self.ed.edit_button = None;
            }
        }
        if crate::autoui::row_popover(ctx, &mut self.ws, &mut self.auto_rt, &pal) {
            self.calc_dirty = true;
        }

        self.row_menu_ui(ctx, &pal);
        self.side_menu_ui(ctx, &pal);
        self.page_menu_ui(ctx, &pal);
        self.templates_ui(ctx, &pal);
        self.hier_ui(ctx, &pal);
        self.import_ui(ctx, &pal);
        self.help_ui(ctx, &pal);
        self.ws_menu_ui(ctx, &pal);
        if self.ai.rt.open && !self.locked && ctx.screen_rect().width() < 720.0 {
            let out = crate::aiui::fullscreen(ctx, &mut self.ai, &mut self.ws, &pal);
            if out.close {
                self.ai.rt.open = false;
                crate::widgets::reset_appear(ctx, Id::new("ai_sheet_a"));
            }
            if let Some(id) = out.open_page {
                crate::views::set(crate::views::Mode::Page, None);
                self.ws.current = Some(id);
                self.ai.rt.open = false;
                crate::widgets::reset_appear(ctx, Id::new("ai_sheet_a"));
            }
        }
        if !self.locked {
            crate::aiui::settings(ctx, &mut self.ai, &self.ws, &pal);
        }
        self.palette_ui(ctx, &pal);
        self.trash_ui(ctx, &pal);
        self.security_ui(ctx, &pal);
        self.history_ui(ctx, &pal);
        crate::pdf::reader_ui(ctx, &mut self.ed.pdf_req, &mut self.ws.pdf_annots, &pal);
        self.toasts_ui(ctx, &pal);
    }

    fn history_ui(&mut self, ctx: &Context, pal: &Palette) {
        let (Some(st), Some(cur)) = (self.history.as_mut(), self.ws.current) else {
            self.history = None;
            return;
        };
        let narrow = ctx.screen_rect().width() < 720.0;
        let out = crate::history::dialog(ctx, pal, &mut self.ws, cur, st, narrow);
        if let Some(t) = out.toast {
            self.toast(t, None);
        }
        if out.close {
            self.history = None;
            reset_appear(ctx, Id::new("page_history").with("a"));
        }
    }
}

// ───────────── helpers ─────────────

/// Runs `add` in a child `Ui` that is laid out at `w` (when given) instead of the width of the panel, starting at the panel's left edge, and
/// clipped to the panel. While the sidebar moves, the page / top bar therefore slide along with it instead of re-wrapping every frame.
pub(crate) fn frozen_layout<R>(ui: &mut Ui, w: Option<f32>, add: impl FnOnce(&mut Ui) -> R) -> R {
    let r = ui.max_rect();
    let lr = match w {
        Some(w) => Rect::from_min_size(r.min, vec2(w.max(160.0), r.height())),
        None => r,
    };
    let clip = ui.clip_rect().intersect(r);
    ui.scope_builder(egui::UiBuilder::new().max_rect(lr), |ui| {
        ui.set_clip_rect(clip);
        add(ui)
    })
    .inner
}

pub(crate) fn ago(ms: f64) -> String {
    let s = ((now_ms() - ms) / 1000.0).max(0.0) as u64;
    match s {
        0..=9 => "just now".into(),
        10..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

pub(crate) fn modal(ctx: &Context, id: Id, pal: &Palette, width: f32, top: f32, add: impl FnOnce(&mut Ui)) -> bool {
    let t = appear(ctx, id.with("a"), 0.2);
    let mut open = true;
    let screen = ctx.screen_rect();
    let bd = Area::new(id.with("backdrop")).order(crate::widgets::HOST_ORDER).fixed_pos(Pos2::ZERO).show(ctx, |ui| {
        let (r, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
        ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha((120.0 * t) as u8));
        if resp.clicked() {
            open = false;
        }
    });
    // The backdrop must always sit *below* the dialog, otherwise the dialog looks greyed-out/hidden.
    ctx.move_to_top(bd.response.layer_id);
    let md = Area::new(id).order(crate::widgets::HOST_ORDER).anchor(Align2::CENTER_TOP, vec2(0.0, top + (1.0 - t) * 16.0)).show(ctx, |ui| {
        ui.set_opacity(t);
        crate::widgets::popup_frame(pal).corner_radius(CornerRadius::same(14)).inner_margin(Margin::same(8)).show(ui, |ui| {
            ui.set_width(width.min((screen.width() - 32.0).max(280.0)));
            add(ui);
        });
    });
    ctx.move_to_top(md.response.layer_id);
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        open = false;
    }
    if !open {
        reset_appear(ctx, id.with("a"));
        // a drop-down that was open when the dialog closed must not stay "open" in egui's memory (it would block re-raising later)
        ctx.memory_mut(|m| m.close_popup());
    }
    open
}

#[allow(clippy::too_many_arguments)]
fn hier_rows(ui: &mut Ui, pal: &Palette, ws: &Workspace, exp: &std::collections::HashSet<u64>, cur: u64, parent: Option<u64>, depth: usize, go: &mut Option<u64>, tog: &mut Option<u64>) {
    for id in ws.children(parent) {
        let Some(p) = ws.page(id) else { continue };
        let has = ws.has_children(id);
        let open = exp.contains(&id);
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::hover());
        let row = ui.interact(rect, Id::new(("hier_row", id)), Sense::click());
        let sel = id == cur;
        let x0 = rect.left() + 4.0 + depth.min(8) as f32 * 18.0;
        let arrow = Rect::from_min_size(pos2(x0, rect.top()), vec2(34.0, rect.height()));
        let ar = ui.interact(arrow, Id::new(("hier_arrow", id)), Sense::click());
        if sel {
            ui.painter().rect_filled(rect.shrink2(vec2(2.0, 1.0)), 8.0, pal.accent_soft);
        } else if row.is_pointer_button_down_on() || row.hovered() {
            ui.painter().rect_filled(rect.shrink2(vec2(2.0, 1.0)), 8.0, pal.hover);
        }
        if has {
            ui.painter().text(arrow.center(), Align2::CENTER_CENTER, if open { l::CHEVRON_DOWN } else { l::CHEVRON_RIGHT }, body(15.0), pal.muted);
        }
        let ic = pos2(x0 + 44.0, rect.center().y);
        if p.icon.is_empty() {
            icons::paint(ui.painter(), ic, 16.0, match p.kind { PageKind::Database => Icon::Board, PageKind::Canvas => Icon::Canvas, PageKind::Flow => Icon::Flow, _ => Icon::Page }, pal.muted);
        } else {
            icons::paint_page_icon(ui, ic, &p.icon, 18.0, pal.text);
        }
        let title = if p.title.is_empty() { "Untitled" } else { p.title.as_str() };
        crate::widgets::text_fit(ui.painter(), pos2(x0 + 62.0, rect.center().y), Align2::LEFT_CENTER, title, if sel { bold(14.5) } else { body(14.5) }, if sel { pal.accent } else { pal.text }, (rect.right() - 10.0 - (x0 + 62.0)).max(40.0));
        if ar.clicked() && has {
            *tog = Some(id);
        } else if row.clicked() {
            *go = Some(id);
        }
        if has && open {
            hier_rows(ui, pal, ws, exp, cur, Some(id), depth + 1, go, tog);
        }
    }
}

fn crumb(ui: &mut Ui, pal: &Palette, icon: &str, title: &str, last: bool, max_w: f32) -> egui::Response {
    let col = if last { pal.text } else { pal.muted };
    let galley = ui.painter().layout_no_wrap(title.to_owned(), body(14.0), col);
    let iw = if icon.is_empty() { 0.0 } else { 22.0 };
    let w = (galley.size().x + iw + 16.0).min(max_w.max(48.0));
    let (rect, resp) = ui.allocate_exact_size(vec2(w, if crate::touch::touchy() { 36.0 } else { 28.0 }), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
    ui.painter().rect_filled(rect, 6.0, with_alpha(pal.hover, t));
    let mut x = rect.left() + 8.0;
    if !icon.is_empty() {
        icons::paint_page_icon(ui, pos2(x + 7.0, rect.center().y), icon, 16.0, pal.text);
        x += iw;
    }
    crate::widgets::text_fit(ui.painter(), pos2(x, rect.center().y), Align2::LEFT_CENTER, title, body(14.0), col, rect.right() - 6.0 - x);
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
    rclick: Option<Pos2>,
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
        icons::paint(p, ic, 15.0, match page.kind { PageKind::Database => Icon::Board, PageKind::Canvas => Icon::Canvas, PageKind::Flow => Icon::Flow, _ => Icon::Page }, pal.muted);
    } else {
        icons::paint_page_icon(ui, ic, &page.icon, 17.0, pal.text);
    }
    // a finger cannot hover: the + and ⋯ actions of a row are always there on touch screens
    let hovered = crate::touch::reveal(hovered);
    let t = if crate::touch::touchy() { 1.0 } else { t };
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
    let mut out = RowOut { click: false, arrow: false, plus: false, more: None, rclick: crate::widgets::right_clicked(ui, rect) };
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

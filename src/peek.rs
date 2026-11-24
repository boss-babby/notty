//! Side peek: a database row opens in a panel on the right (like Notion), with "Open as page" one click away.
//!
//! * Clicking a row of a table / board / list / gallery / calendar / timeline only *requests* a peek (`EditorState::peek`); the app
//!   decides — phones and the "always open as full page" preference go straight to the page.
//! * The panel slides in with the same easing as the AI panel; the page behind keeps its place (no navigation happens).
//! * Header: close · open as page (Ctrl+Enter) · previous / next row · breadcrumb · "always full page" toggle. Esc closes.
//! * The body is the very same editor as a full page (properties + content), so nothing is lost by peeking.

use egui::{pos2, vec2, Align, Context, Frame, Id, Key, Layout, Modifiers, Rect, ScrollArea, Stroke, Ui};

use crate::editor::{self, EditorState};
use crate::icons::Icon;
use crate::model::Workspace;
use crate::theme::{body, Palette};
use crate::widgets::{icon_button, text_fit, with_alpha};

/// Below this window width rows open as full pages (a 400 pt panel would leave no room for the database).
pub const MIN_SCREEN_W: f32 = 820.0;

#[derive(Default)]
pub struct Peek {
    /// The row that is peeked right now.
    pub id: Option<u64>,
    /// The row shown while the panel slides out again.
    last: u64,
    /// The page that was open when the peek started; navigating elsewhere closes the peek.
    origin: Option<u64>,
    /// User preference: always open rows as full pages.
    pub full_pages: bool,
}

#[derive(Default)]
struct Out {
    close: bool,
    full: bool,
    step: i32,
    toggle_pref: bool,
}

impl Peek {
    /// Opens `id` in the panel, or returns it when it has to be shown as a full page.
    pub fn request(&mut self, ws: &Workspace, id: u64, screen_w: f32) -> Option<u64> {
        if self.full_pages || screen_w < MIN_SCREEN_W {
            self.id = None;
            return Some(id);
        }
        if ws.page(id).map_or(true, |p| p.trashed) {
            return None;
        }
        if self.id.is_none() {
            self.origin = ws.current;
        }
        self.id = Some(id);
        None
    }

    pub fn close(&mut self) {
        self.id = None;
    }

    /// Draws the panel (call before the central panel). Returns a page that has to be opened in full.
    pub fn panel(&mut self, ctx: &Context, ws: &mut Workspace, ed: &mut EditorState, pal: &Palette, screen_w: f32) -> Option<u64> {
        // stale peeks: row deleted, window became narrow, user navigated to another page
        if let Some(id) = self.id {
            let gone = ws.page(id).map_or(true, |p| p.trashed);
            if gone || screen_w < MIN_SCREEN_W || ws.current != self.origin {
                self.id = None;
            }
        }
        let t = ctx.animate_bool_with_time_and_easing(Id::new("peek_t"), self.id.is_some(), 0.24, egui::emath::easing::cubic_out);
        if t <= 0.003 {
            return None;
        }
        let id = match self.id {
            Some(id) => {
                self.last = id;
                id
            }
            None if ws.page(self.last).is_some() => self.last,
            None => return None,
        };
        let animating = t < 0.997;
        let full_w: f32 = ctx
            .data(|d| d.get_temp::<f32>(Id::new("peek_w")))
            .unwrap_or((screen_w * 0.42).clamp(400.0, 720.0))
            .clamp(360.0, (screen_w * 0.7).max(361.0));

        let mut out = Out::default();
        let mut panel = egui::SidePanel::right("peek_panel").frame(Frame::new().fill(pal.bg).shadow(egui::epaint::Shadow {
            offset: [-4, 0],
            blur: 24,
            spread: 0,
            color: egui::Color32::from_black_alpha((34.0 * (1.0 - t * 0.5)) as u8),
        }));
        panel = if animating {
            panel.resizable(false).exact_width(full_w * t)
        } else {
            panel
                .resizable(true)
                .default_width(full_w)
                .width_range(360.0..=(screen_w * 0.7).max(361.0))
        };
        let shown = panel.show(ctx, |ui| {
            let r = ui.max_rect();
            let lw = if animating { full_w } else { r.width() };
            let lr = Rect::from_min_size(r.min, vec2(lw, r.height()));
            let clip = ui.clip_rect().intersect(r);
            ui.scope_builder(egui::UiBuilder::new().id_salt("peek_content").max_rect(lr), |ui| {
                ui.set_clip_rect(clip);
                ui.set_opacity((t * 1.4).min(1.0));
                out = content(ui, ws, ed, pal, id, self.full_pages);
            });
            ui.painter()
                .vline(r.left(), r.y_range(), Stroke::new(1.0_f32, with_alpha(pal.border, t)));
        });
        if !animating {
            ctx.data_mut(|d| d.insert_temp(Id::new("peek_w"), shown.response.rect.width()));
        }

        // keyboard: Ctrl+Enter = open as page, Esc = close (only when no text field / popup is using it)
        if self.id.is_some() {
            if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter)) {
                out.full = true;
            } else if ctx.memory(|m| m.focused().is_none()) && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                out.close = true;
            }
        }
        if out.toggle_pref {
            self.full_pages = !self.full_pages;
            if self.full_pages {
                out.full = true;
            }
        }
        if out.step != 0 {
            let parent = ws.page(id).and_then(|p| p.parent);
            let rows: Vec<u64> = ws
                .pages
                .iter()
                .filter(|p| p.row && !p.trashed && p.parent == parent)
                .map(|p| p.id)
                .collect();
            if let Some(i) = rows.iter().position(|r| *r == id) {
                let n = (i as i32 + out.step).clamp(0, rows.len() as i32 - 1) as usize;
                self.id = Some(rows[n]);
            }
        }
        if out.full {
            self.id = None;
            return Some(id);
        }
        if out.close {
            self.id = None;
        }
        if animating {
            ctx.request_repaint();
        }
        None
    }
}

fn content(ui: &mut Ui, ws: &mut Workspace, ed: &mut EditorState, pal: &Palette, id: u64, pref_full: bool) -> Out {
    let mut out = Out::default();
    let (title, parent_title) = {
        let p = ws.page(id);
        let t = p
            .map(|p| {
                if p.title.trim().is_empty() {
                    "Untitled".to_owned()
                } else {
                    p.title.clone()
                }
            })
            .unwrap_or_default();
        let pt = p
            .and_then(|p| p.parent)
            .and_then(|pid| ws.page(pid))
            .map(|d| d.title.clone())
            .unwrap_or_default();
        (t, pt)
    };
    // header bar
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 40.0), Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.add_space(6.0);
        if icon_button(ui, pal, Icon::ChevronRight, 15.0, "Close  (Esc)").clicked() {
            out.close = true;
        }
        if icon_button(ui, pal, Icon::External, 15.0, "Open as page  (Ctrl+Enter)").clicked() {
            out.full = true;
        }
        ui.add_space(6.0);
        if icon_button(ui, pal, Icon::Up, 15.0, "Previous row").clicked() {
            out.step = -1;
        }
        if icon_button(ui, pal, Icon::Down, 15.0, "Next row").clicked() {
            out.step = 1;
        }
        ui.add_space(8.0);
        let w = (ui.available_width() - 52.0).max(40.0);
        let (r, _) = ui.allocate_exact_size(vec2(w, 28.0), egui::Sense::hover());
        let crumb = if parent_title.trim().is_empty() {
            title.clone()
        } else {
            format!("{}  /  {}", parent_title.trim(), title)
        };
        text_fit(
            ui.painter(),
            pos2(r.left(), r.center().y),
            egui::Align2::LEFT_CENTER,
            &crumb,
            body(13.0),
            pal.muted,
            r.width(),
        );
        let tip = if pref_full {
            "Rows open as full pages — click to use the side peek again"
        } else {
            "Always open rows as full pages"
        };
        if icon_button(ui, pal, Icon::Panel, 15.0, tip).clicked() {
            out.toggle_pref = true;
        }
    });
    let r = ui.max_rect();
    ui.painter().hline(r.x_range(), ui.cursor().top(), Stroke::new(1.0_f32, pal.border));

    let over = ui.rect_contains_pointer(ui.available_rect_before_wrap());
    let sa = ScrollArea::vertical()
        .id_salt(("peek_scroll", id))
        .drag_to_scroll(false)
        .auto_shrink([false, false]);
    let mut changed = false;
    crate::smooth::scroll_area(ui, Id::new(("peek_smooth", id)), sa, over, |ui| {
        ui.add_space(8.0);
        changed = editor::show_page(ui, ws, id, ed, pal);
    });
    if changed {
        ws.touch(id);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phones_and_the_preference_open_full_pages() {
        let ws = Workspace::seed();
        let id = ws.pages.iter().find(|p| !p.trashed).map(|p| p.id).expect("a page");
        let mut pk = Peek::default();
        assert_eq!(pk.request(&ws, id, 400.0), Some(id));
        assert!(pk.id.is_none());
        assert_eq!(pk.request(&ws, id, 1200.0), None);
        assert!(pk.id.is_some());
        pk.close();
        pk.full_pages = true;
        assert_eq!(pk.request(&ws, id, 1200.0), Some(id));
    }
}

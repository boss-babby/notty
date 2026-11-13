//! The `Table` block: an editable grid whose cells render inline Markdown, scrolls sideways on phones, and can be turned into a database.
//!
//! Keyboard: click / tap a cell to edit, `Tab` / `Shift+Tab` move between cells (a new row appears after the last cell),
//! `Enter` moves down, `Esc` stops editing. The toolbar under the table adds / removes rows and columns, sorts, aligns, toggles the
//! header row, copies the table as Markdown and converts it to a database (`EditorState::table_to_db`, handled by the page editor).

use egui::{pos2, vec2, Align2, CursorIcon, Id, Key, Modifiers, Rect, ScrollArea, Sense, Stroke, TextEdit, Ui};

use crate::editor::{rich_job, EditorState};
use crate::mdtable;
use crate::model::Block;
use crate::theme::{bold, body, Palette};
use crate::widgets::with_alpha;

const PAD_X: f32 = 9.0;
const PAD_Y: f32 = 7.0;

#[derive(Clone, Copy, Default)]
struct Edit {
    cell: Option<(usize, usize)>,
    /// A cell was just chosen: focus its text field once.
    fresh: bool,
    /// Last column that was sorted, and the direction.
    sorted: Option<(usize, bool)>,
}

fn is_num(s: &str) -> Option<f64> {
    s.trim().trim_start_matches(['$', '€', '£']).trim_end_matches('%').replace(',', "").parse::<f64>().ok()
}

fn btn(ui: &mut Ui, pal: &Palette, id: Id, label: &str, on: bool, alpha: f32) -> bool {
    let font = body(12.5);
    let gw = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), pal.text).size().x;
    let (rect, _) = ui.allocate_exact_size(vec2(gw + 16.0, 24.0), Sense::hover());
    // While the toolbar is faded out (pointer away / no cell selected) the buttons are completely inert and invisible:
    // no background, no hover, no click, no cursor. (Previously the "on" buttons kept their accent fill at alpha 0.)
    if alpha < 0.02 {
        return false;
    }
    let live = alpha > 0.4;
    let resp = ui.interact(rect, id, if live { Sense::click() } else { Sense::hover() });
    let h = ui.ctx().animate_bool_with_time(id.with("h"), resp.hovered() && live, 0.1);
    let fill = if on { with_alpha(pal.accent, 0.16 * alpha) } else { with_alpha(pal.hover, h * alpha) };
    if fill.a() > 0 {
        ui.painter().rect_filled(rect, 6.0, fill);
    }
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, font, with_alpha(if on { pal.accent } else { pal.muted.lerp_to_gamma(pal.text, 0.4 + 0.6 * h) }, alpha));
    if live {
        resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
    } else {
        false
    }
}

/// Draws the table; returns true when the block changed.
pub fn draw(ui: &mut Ui, b: &mut Block, ed: &mut EditorState, pal: &Palette, w: f32) -> bool {
    let mut changed = false;
    let mut rows = mdtable::grid(b);
    let (nr, nc) = (rows.len(), rows[0].len());
    let al: Vec<char> = b.url.chars().chain(std::iter::repeat('l')).take(nc).collect();
    let key = Id::new(("tblstate", b.id));
    let mut st: Edit = ui.ctx().data(|d| d.get_temp(key)).unwrap_or_default();
    if let Some((r, c)) = st.cell {
        if r >= nr || c >= nc {
            st.cell = None;
        }
    }

    let colw = (w / nc as f32).clamp(124.0, 340.0).max(if nc as f32 * 124.0 < w { w / nc as f32 } else { 124.0 });
    let total_w = colw * nc as f32;
    let (f_body, f_head) = (body(14.5), bold(14.5));

    // row heights from the laid-out text (an editing cell is measured with its raw text)
    let mut heights = vec![0.0f32; nr];
    let mut galleys = Vec::with_capacity(nr);
    for (ri, row) in rows.iter().enumerate() {
        let mut gr = Vec::with_capacity(nc);
        for (ci, cell) in row.iter().enumerate() {
            let head = ri == 0 && b.checked;
            let font = if head { &f_head } else { &f_body };
            let mut job = rich_job(cell, font, pal.text, pal, true);
            job.wrap.max_width = colw - PAD_X * 2.0;
            if cell.is_empty() {
                job = rich_job(" ", font, pal.text, pal, true);
            }
            let g = ui.fonts(|f| f.layout_job(job));
            heights[ri] = heights[ri].max(g.size().y);
            gr.push(g);
            let _ = ci;
        }
        galleys.push(gr);
        heights[ri] = heights[ri].max(20.0) + PAD_Y * 2.0;
    }
    let total_h: f32 = heights.iter().sum();

    let mut table_rect = Rect::NOTHING;
    let mut next: Option<(usize, usize)> = st.cell;
    let mut add_row_after: Option<usize> = None;

    ScrollArea::horizontal().id_salt(("tblscroll", b.id)).auto_shrink([false, true]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(total_w.max(w), total_h + 2.0), Sense::hover());
        let rect = Rect::from_min_size(rect.min, vec2(total_w, total_h));
        table_rect = rect;
        let p = ui.painter().clone();
        p.rect_filled(rect, 4.0, with_alpha(pal.bg, 1.0));
        let mut y = rect.top();
        for ri in 0..nr {
            let h = heights[ri];
            let head = ri == 0 && b.checked;
            if head {
                p.rect_filled(Rect::from_min_size(pos2(rect.left(), y), vec2(total_w, h)), 0.0, pal.hover);
            }
            for ci in 0..nc {
                let cell = Rect::from_min_size(pos2(rect.left() + ci as f32 * colw, y), vec2(colw, h));
                let id = Id::new(("tcell", b.id, ri, ci));
                let editing = st.cell == Some((ri, ci));
                let resp = ui.interact(cell, id, Sense::click());
                if resp.hovered() && !editing {
                    p.rect_filled(cell, 0.0, with_alpha(pal.hover, 0.55));
                }
                if editing {
                    p.rect_stroke(cell.shrink(1.0), 2.0, Stroke::new(1.6_f32, pal.accent), egui::StrokeKind::Inside);
                    let eid = id.with("edit");
                    let enter = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
                    let tab = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab));
                    let back = ui.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Tab));
                    let esc = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
                    let font = if head { f_head.clone() } else { f_body.clone() };
                    let inner = cell.shrink2(vec2(PAD_X, PAD_Y));
                    let r = ui.put(inner, TextEdit::multiline(&mut rows[ri][ci]).id(eid).frame(false).desired_rows(1).desired_width(inner.width()).font(font).text_color(pal.text).margin(egui::Margin::ZERO));
                    if st.fresh {
                        ui.memory_mut(|m| m.request_focus(eid));
                        st.fresh = false;
                    }
                    if r.changed() {
                        for ch in ['\n', '\t', mdtable::CELL, mdtable::ROW, '\r'] {
                            if rows[ri][ci].contains(ch) {
                                rows[ri][ci] = rows[ri][ci].replace(ch, " ");
                            }
                        }
                        changed = true;
                    }
                    if tab || enter {
                        let (mut nr2, mut nc2) = (ri, ci);
                        if tab {
                            if ci + 1 < nc {
                                nc2 += 1;
                            } else {
                                nc2 = 0;
                                nr2 += 1;
                            }
                        } else {
                            nr2 += 1;
                        }
                        if nr2 >= nr {
                            add_row_after = Some(nr - 1);
                            nr2 = nr;
                        }
                        next = Some((nr2, nc2));
                        st.fresh = true;
                    } else if back {
                        next = Some(if ci > 0 { (ri, ci - 1) } else if ri > 0 { (ri - 1, nc - 1) } else { (0, 0) });
                        st.fresh = true;
                    } else if esc {
                        next = None;
                    }
                } else {
                    let g = galleys[ri][ci].clone();
                    let gw = g.size().x;
                    let x = match al[ci] {
                        'c' => cell.center().x - gw / 2.0,
                        'r' => cell.right() - PAD_X - gw,
                        _ => cell.left() + PAD_X,
                    };
                    p.with_clip_rect(cell).galley(pos2(x, cell.top() + PAD_Y), g, pal.text);
                    if resp.on_hover_cursor(CursorIcon::Text).clicked() {
                        next = Some((ri, ci));
                        st.fresh = true;
                    }
                }
            }
            y += h;
        }
        // grid lines
        let line = Stroke::new(1.0_f32, with_alpha(pal.border, 0.9));
        let mut y = rect.top();
        for h in std::iter::once(0.0).chain(heights.iter().copied()) {
            y += h;
            p.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], line);
        }
        p.line_segment([pos2(rect.left(), rect.top()), pos2(rect.right(), rect.top())], line);
        for ci in 0..=nc {
            let x = rect.left() + ci as f32 * colw;
            p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], line);
        }
    });

    // ── toolbar ──
    let active = st.cell.is_some() || next.is_some();
    let hov = ui.rect_contains_pointer(table_rect.expand2(vec2(8.0, 34.0)));
    let t = ui.ctx().animate_bool_with_time(key.with("bar"), crate::touch::reveal(hov) || active, 0.14);
    let (r, c) = next.or(st.cell).unwrap_or((nr - 1, nc - 1));
    let (r, c) = (r.min(nr - 1), c.min(nc - 1));
    let mut convert = false;
    ui.add_space(4.0);
    ScrollArea::horizontal().id_salt(("tblbar", b.id)).auto_shrink([false, true]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if btn(ui, pal, key.with("ar"), "+ Row", false, t) {
                rows.insert(r + 1, vec![String::new(); nc]);
                next = Some((r + 1, c));
                st.fresh = true;
                changed = true;
            }
            if btn(ui, pal, key.with("ac"), "+ Column", false, t) {
                for row in rows.iter_mut() {
                    row.insert(c + 1, String::new());
                }
                let mut a: Vec<char> = al.clone();
                a.insert(c + 1, 'l');
                b.url = a.into_iter().collect();
                changed = true;
            }
            if nr > 1 && btn(ui, pal, key.with("dr"), "− Row", false, t) {
                rows.remove(r);
                next = None;
                changed = true;
            }
            if nc > 1 && btn(ui, pal, key.with("dc"), "− Column", false, t) {
                for row in rows.iter_mut() {
                    row.remove(c);
                }
                let mut a: Vec<char> = al.clone();
                a.remove(c);
                b.url = a.into_iter().collect();
                next = None;
                changed = true;
            }
            if btn(ui, pal, key.with("al"), match al[c] { 'c' => "Align: center", 'r' => "Align: right", _ => "Align: left" }, false, t) {
                let mut a: Vec<char> = al.clone();
                a[c] = match al[c] { 'l' => 'c', 'c' => 'r', _ => 'l' };
                b.url = a.into_iter().collect();
                changed = true;
            }
            if nr > 2 && btn(ui, pal, key.with("so"), if st.sorted == Some((c, true)) { "Sort ↓" } else { "Sort ↑" }, false, t) {
                let asc = st.sorted != Some((c, true));
                let skip = usize::from(b.checked);
                let numeric = rows[skip..].iter().all(|r| r[c].trim().is_empty() || is_num(&r[c]).is_some());
                rows[skip..].sort_by(|x, y| {
                    let o = if numeric {
                        is_num(&x[c]).unwrap_or(f64::MIN).partial_cmp(&is_num(&y[c]).unwrap_or(f64::MIN)).unwrap_or(std::cmp::Ordering::Equal)
                    } else {
                        x[c].to_lowercase().cmp(&y[c].to_lowercase())
                    };
                    if asc { o } else { o.reverse() }
                });
                st.sorted = Some((c, asc));
                changed = true;
            }
            if btn(ui, pal, key.with("hd"), if b.checked { "Header row ✓" } else { "Header row" }, b.checked, t) {
                b.checked = !b.checked;
                changed = true;
            }
            if btn(ui, pal, key.with("cp"), "Copy Markdown", false, t) {
                let mut tmp = b.clone();
                mdtable::set_grid(&mut tmp, &rows);
                ui.ctx().copy_text(mdtable::to_markdown(&tmp));
            }
            if btn(ui, pal, key.with("db"), "Convert to database", true, t) {
                convert = true;
            }
        });
    });

    if let Some(after) = add_row_after {
        rows.insert(after + 1, vec![String::new(); nc]);
        changed = true;
    }
    st.cell = next;
    ui.ctx().data_mut(|d| d.insert_temp(key, st));
    if changed {
        mdtable::set_grid(b, &rows);
    }
    if convert {
        // the page editor owns the workspace: it performs the conversion after this frame's layout
        mdtable::set_grid(b, &rows);
        ed.table_to_db = Some(b.id);
        changed = true;
    }
    changed
}

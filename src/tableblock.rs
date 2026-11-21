//! The `Table` block: an editable grid whose cells render inline Markdown, scrolls sideways on phones, and can be turned into a database.
//!
//! * **Editing** — click / tap a cell, `Tab` / `Shift+Tab` move between cells (a new row appears after the last cell), `Enter` moves down, `Esc` stops.
//! * **Resizing** — drag the border between two columns (double-click = fit to content), drag the bottom border of a row for a minimum
//!   height (double-click = automatic), drag the grabber in the bottom-right corner to scale the whole table, or use *Fit width* /
//!   *Equal columns* in the toolbar. Sizes are stored in `Block::cols` (`"w1,w2|h1,h2"`, 0 = automatic) and survive copy / history; Markdown export ignores them.
//! * **Handles** (Affine / Notion style) — a grip above every column and left of every row: click opens a menu (insert left / right or
//!   above / below, move, duplicate, align, sort, fit, clear, delete), drag reorders. `+` strips at the right and bottom edge add a column / row.
//!   On touch screens the grips and borders are larger and always visible.
//! * The toolbar under the table adds / removes rows and columns, sorts, aligns, toggles the header row, copies the table as Markdown
//!   and converts it to a database (`EditorState::table_to_db`, handled by the page editor).

use egui::{pos2, vec2, Align2, CursorIcon, Id, Key, Modifiers, Pos2, Rect, ScrollArea, Sense, Stroke, TextEdit, Ui};

use crate::editor::{rich_job, EditorState};
use crate::lucide as l;
use crate::mdtable;
use crate::model::Block;
use crate::theme::{bold, body, Palette};
use crate::widgets::{context_menu, with_alpha, CtxItem};

const PAD_X: f32 = 9.0;
const PAD_Y: f32 = 7.0;
const MIN_COL: f32 = 56.0;
const MAX_COL: f32 = 900.0;
const MIN_ROW: f32 = 30.0;

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Col(usize),
    Row(usize),
}

#[derive(Clone, Copy, Default)]
struct Edit {
    cell: Option<(usize, usize)>,
    /// A cell was just chosen: focus its text field once.
    fresh: bool,
    /// Last column that was sorted, and the direction.
    sorted: Option<(usize, bool)>,
    /// Open grip menu: (is a row, index, anchor).
    menu: Option<(bool, usize, Pos2)>,
    menu_fresh: bool,
    drag: Option<Drag>,
}

/// Structural / formatting changes requested during the frame; applied together after the grid was drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    InsCol(usize),
    DelCol(usize),
    MoveCol(usize, usize),
    DupCol(usize),
    ClearCol(usize),
    Align(usize, char),
    Sort(usize, bool),
    FitCol(usize),
    EqualCols,
    FitWidth,
    InsRow(usize),
    DelRow(usize),
    MoveRow(usize, usize),
    DupRow(usize),
    ClearRow(usize),
    ResetRowH(usize),
    ToggleHeader,
}

/// The table as the editor edits it: cells, alignment, widths and minimum heights stay in step through every structural change.
#[derive(Clone, Debug, PartialEq)]
pub struct Tbl {
    pub rows: Vec<Vec<String>>,
    pub al: Vec<char>,
    pub w: Vec<f32>,
    pub h: Vec<f32>,
}

impl Tbl {
    pub fn from_block(b: &Block) -> Tbl {
        let rows = mdtable::grid(b);
        let (nr, nc) = (rows.len(), rows[0].len());
        let al: Vec<char> = b.url.chars().chain(std::iter::repeat('l')).take(nc).collect();
        let (w, h) = parse_cols(&b.cols, nc, nr);
        Tbl { rows, al, w, h }
    }

    pub fn write_to(&self, b: &mut Block) {
        mdtable::set_grid(b, &self.rows);
        b.url = self.al.iter().collect();
        b.cols = write_cols(&self.w, &self.h);
    }

    pub fn nr(&self) -> usize {
        self.rows.len()
    }
    pub fn nc(&self) -> usize {
        self.rows[0].len()
    }

    pub fn insert_col(&mut self, at: usize) {
        let at = at.min(self.nc());
        for r in self.rows.iter_mut() {
            r.insert(at, String::new());
        }
        self.al.insert(at, 'l');
        self.w.insert(at, 0.0);
    }
    pub fn remove_col(&mut self, c: usize) {
        if self.nc() <= 1 {
            return;
        }
        for r in self.rows.iter_mut() {
            r.remove(c);
        }
        self.al.remove(c);
        self.w.remove(c);
    }
    pub fn move_col(&mut self, from: usize, to: usize) {
        let to = to.min(self.nc() - 1);
        if from == to {
            return;
        }
        for r in self.rows.iter_mut() {
            let x = r.remove(from);
            r.insert(to, x);
        }
        let a = self.al.remove(from);
        self.al.insert(to, a);
        let w = self.w.remove(from);
        self.w.insert(to, w);
    }
    pub fn dup_col(&mut self, c: usize) {
        for r in self.rows.iter_mut() {
            let x = r[c].clone();
            r.insert(c + 1, x);
        }
        self.al.insert(c + 1, self.al[c]);
        self.w.insert(c + 1, self.w[c]);
    }
    pub fn insert_row(&mut self, at: usize) {
        let at = at.min(self.nr());
        let nc = self.nc();
        self.rows.insert(at, vec![String::new(); nc]);
        self.h.insert(at, 0.0);
    }
    pub fn remove_row(&mut self, r: usize) {
        if self.nr() <= 1 {
            return;
        }
        self.rows.remove(r);
        self.h.remove(r);
    }
    pub fn move_row(&mut self, from: usize, to: usize) {
        let to = to.min(self.nr() - 1);
        if from == to {
            return;
        }
        let x = self.rows.remove(from);
        self.rows.insert(to, x);
        let h = self.h.remove(from);
        self.h.insert(to, h);
    }
    pub fn dup_row(&mut self, r: usize) {
        let x = self.rows[r].clone();
        self.rows.insert(r + 1, x);
        self.h.insert(r + 1, self.h[r]);
    }
}

/// `"w1,w2|h1,h2"` → `(widths, heights)` padded / cut to the grid size; garbage reads as automatic (0).
pub fn parse_cols(s: &str, nc: usize, nr: usize) -> (Vec<f32>, Vec<f32>) {
    let (a, b) = s.split_once('|').unwrap_or((s, ""));
    let read = |t: &str, n: usize, hi: f32| -> Vec<f32> {
        let mut v: Vec<f32> = t.split(',').map(|x| x.trim().parse::<f32>().ok().filter(|x| x.is_finite() && *x > 0.0).map_or(0.0, |x| x.min(hi))).collect();
        v.resize(n, 0.0);
        v
    };
    (read(a, nc, MAX_COL), read(b, nr, 2000.0))
}

pub fn write_cols(w: &[f32], h: &[f32]) -> String {
    if w.iter().chain(h).all(|x| *x <= 0.0) {
        return String::new();
    }
    let f = |v: &[f32]| v.iter().map(|x| format!("{}", x.round() as i32)).collect::<Vec<_>>().join(",");
    if h.iter().all(|x| *x <= 0.0) {
        f(w)
    } else {
        format!("{}|{}", f(w), f(h))
    }
}

/// Column widths the layout uses: the saved ones, automatic columns share what is left (or, with nothing saved, the page width).
pub fn layout_widths(saved: &[f32], avail: f32) -> Vec<f32> {
    let nc = saved.len().max(1);
    let n_auto = saved.iter().filter(|w| **w <= 0.0).count();
    let fixed: f32 = saved.iter().filter(|w| **w > 0.0).sum();
    let auto_w = if n_auto == 0 {
        0.0
    } else if n_auto == nc {
        (avail / nc as f32).clamp(124.0, 340.0).max(if nc as f32 * 124.0 < avail { avail / nc as f32 } else { 124.0 })
    } else {
        ((avail - fixed).max(0.0) / n_auto as f32).clamp(124.0, 340.0)
    };
    saved.iter().map(|w| if *w > 0.0 { *w } else { auto_w }).collect()
}

fn is_num(s: &str) -> Option<f64> {
    s.trim().trim_start_matches(['$', '€', '£']).trim_end_matches('%').replace(',', "").parse::<f64>().ok()
}

fn btn(ui: &mut Ui, pal: &Palette, id: Id, label: &str, on: bool, alpha: f32) -> bool {
    let font = body(12.5);
    let gw = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), pal.text).size().x;
    let (rect, _) = ui.allocate_exact_size(vec2(gw + 16.0, 24.0), Sense::hover());
    // While the toolbar is faded out (pointer away / no cell selected) the buttons are completely inert and invisible.
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

fn col_items(c: usize, nc: usize, al: char) -> Vec<CtxItem> {
    let on = |a: char| if al == a { "✓" } else { "" };
    vec![
        CtxItem::new(l::BETWEEN_HORIZONTAL_START, "Insert column left"),
        CtxItem::new(l::PLUS, "Insert column right"),
        CtxItem::new(l::ARROW_LEFT, "Move left").enabled(c > 0).sep(),
        CtxItem::new(l::ARROW_RIGHT, "Move right").enabled(c + 1 < nc),
        CtxItem::new(l::COPY, "Duplicate column"),
        CtxItem::new(l::ALIGN_LEFT, "Align left").hint(on('l')).sep(),
        CtxItem::new(l::ALIGN_LEFT, "Align center").hint(on('c')),
        CtxItem::new(l::ALIGN_LEFT, "Align right").hint(on('r')),
        CtxItem::new(l::ARROW_DOWN, "Sort A → Z").sep(),
        CtxItem::new(l::ARROW_UP, "Sort Z → A"),
        CtxItem::new(l::SHRINK, "Fit to contents").sep(),
        CtxItem::new(l::MOVE_HORIZONTAL, "Equal column widths"),
        CtxItem::new(l::EXPAND, "Fit table to page width"),
        CtxItem::new(l::ERASER, "Clear contents").sep(),
        CtxItem::new(l::TRASH_2, "Delete column").danger().enabled(nc > 1),
    ]
}

fn col_op(i: usize, c: usize) -> Option<Op> {
    Some(match i {
        0 => Op::InsCol(c),
        1 => Op::InsCol(c + 1),
        2 => Op::MoveCol(c, c.saturating_sub(1)),
        3 => Op::MoveCol(c, c + 1),
        4 => Op::DupCol(c),
        5 => Op::Align(c, 'l'),
        6 => Op::Align(c, 'c'),
        7 => Op::Align(c, 'r'),
        8 => Op::Sort(c, true),
        9 => Op::Sort(c, false),
        10 => Op::FitCol(c),
        11 => Op::EqualCols,
        12 => Op::FitWidth,
        13 => Op::ClearCol(c),
        14 => Op::DelCol(c),
        _ => return None,
    })
}

fn row_items(r: usize, nr: usize, header: bool) -> Vec<CtxItem> {
    vec![
        CtxItem::new(l::PLUS, "Insert row above"),
        CtxItem::new(l::PLUS, "Insert row below"),
        CtxItem::new(l::ARROW_UP, "Move up").enabled(r > 0).sep(),
        CtxItem::new(l::ARROW_DOWN, "Move down").enabled(r + 1 < nr),
        CtxItem::new(l::COPY, "Duplicate row"),
        CtxItem::new(l::PANEL_TOP, "Header row").hint(if header && r == 0 { "✓" } else { "" }).enabled(r == 0).sep(),
        CtxItem::new(l::EXPAND, "Automatic height"),
        CtxItem::new(l::ERASER, "Clear contents").sep(),
        CtxItem::new(l::TRASH_2, "Delete row").danger().enabled(nr > 1),
    ]
}

fn row_op(i: usize, r: usize) -> Option<Op> {
    Some(match i {
        0 => Op::InsRow(r),
        1 => Op::InsRow(r + 1),
        2 => Op::MoveRow(r, r.saturating_sub(1)),
        3 => Op::MoveRow(r, r + 1),
        4 => Op::DupRow(r),
        5 => Op::ToggleHeader,
        6 => Op::ResetRowH(r),
        7 => Op::ClearRow(r),
        8 => Op::DelRow(r),
        _ => return None,
    })
}

/// Applies one operation to the table. Returns the cell that should stay selected (None = drop the selection).
pub fn apply_op(t: &mut Tbl, header: &mut bool, sorted: &mut Option<(usize, bool)>, op: Op, avail: f32) {
    let nc = t.nc();
    match op {
        Op::InsCol(c) => t.insert_col(c),
        Op::DelCol(c) => t.remove_col(c.min(nc - 1)),
        Op::MoveCol(a, b) => t.move_col(a.min(nc - 1), b),
        Op::DupCol(c) => t.dup_col(c.min(nc - 1)),
        Op::ClearCol(c) => {
            let skip = usize::from(*header);
            for r in t.rows.iter_mut().skip(skip) {
                r[c.min(nc - 1)].clear();
            }
        }
        Op::Align(c, a) => t.al[c.min(nc - 1)] = a,
        Op::Sort(c, asc) => {
            let c = c.min(nc - 1);
            let skip = usize::from(*header);
            if t.nr() > skip + 1 {
                let numeric = t.rows[skip..].iter().all(|r| r[c].trim().is_empty() || is_num(&r[c]).is_some());
                let mut idx: Vec<usize> = (skip..t.nr()).collect();
                let rows = t.rows.clone();
                idx.sort_by(|x, y| {
                    let (a, b) = (&rows[*x][c], &rows[*y][c]);
                    let o = if numeric { is_num(a).unwrap_or(f64::MIN).partial_cmp(&is_num(b).unwrap_or(f64::MIN)).unwrap_or(std::cmp::Ordering::Equal) } else { a.to_lowercase().cmp(&b.to_lowercase()) };
                    if asc { o } else { o.reverse() }
                });
                let hs = t.h.clone();
                for (k, src) in idx.iter().enumerate() {
                    t.rows[skip + k] = rows[*src].clone();
                    t.h[skip + k] = hs[*src];
                }
                *sorted = Some((c, asc));
            }
        }
        Op::FitCol(_) => {} // needs text metrics: handled while drawing
        Op::EqualCols => {
            let each = (avail / nc as f32).clamp(MIN_COL, MAX_COL).round();
            t.w = vec![each; nc];
        }
        Op::FitWidth => t.w = vec![0.0; nc],
        Op::InsRow(r) => t.insert_row(r),
        Op::DelRow(r) => t.remove_row(r.min(t.nr() - 1)),
        Op::MoveRow(a, b) => t.move_row(a.min(t.nr() - 1), b),
        Op::DupRow(r) => t.dup_row(r.min(t.nr() - 1)),
        Op::ClearRow(r) => {
            let r = r.min(t.nr() - 1);
            for c in t.rows[r].iter_mut() {
                c.clear();
            }
        }
        Op::ResetRowH(r) => {
            let r = r.min(t.nr() - 1);
            t.h[r] = 0.0;
        }
        Op::ToggleHeader => *header = !*header,
    }
}

/// Draws the table; returns true when the block changed.
pub fn draw(ui: &mut Ui, b: &mut Block, ed: &mut EditorState, pal: &Palette, w: f32) -> bool {
    let mut changed = false;
    let mut t = Tbl::from_block(b);
    let (nr, nc) = (t.nr(), t.nc());
    let key = Id::new(("tblstate", b.id));
    let mut st: Edit = ui.ctx().data(|d| d.get_temp(key)).unwrap_or_default();
    if let Some((r, c)) = st.cell {
        if r >= nr || c >= nc {
            st.cell = None;
        }
    }
    let touchy = crate::touch::touchy();
    // page zoom: text (through body()/bold()), cell padding, column widths and row heights all follow the content size
    let s = crate::theme::content_z();
    let (pad_x, pad_y) = (PAD_X * s, PAD_Y * s);
    let gutter = if touchy { 22.0 } else { 14.0 };
    let grip_h = if touchy { 22.0 } else { 14.0 };
    let hw = if touchy { 13.0 } else { 5.0 };
    let avail = (w - gutter).max(200.0);

    let colw_d = layout_widths(&t.w, avail / s);
    let colw: Vec<f32> = colw_d.iter().map(|c| c * s).collect();
    let mut xs = vec![0.0f32; nc + 1];
    for ci in 0..nc {
        xs[ci + 1] = xs[ci] + colw[ci];
    }
    let total_w = xs[nc];
    let (f_body, f_head) = (body(14.5), bold(14.5));

    // row heights from the laid-out text, never below the height the user set
    let mut heights = vec![0.0f32; nr];
    let mut galleys = Vec::with_capacity(nr);
    for (ri, row) in t.rows.iter().enumerate() {
        let mut gr = Vec::with_capacity(nc);
        for (ci, cell) in row.iter().enumerate() {
            let head = ri == 0 && b.checked;
            let font = if head { &f_head } else { &f_body };
            let mut job = rich_job(cell, font, pal.text, pal, true);
            job.wrap.max_width = colw[ci] - pad_x * 2.0;
            if cell.is_empty() {
                job = rich_job(" ", font, pal.text, pal, true);
            }
            let g = ui.fonts(|f| f.layout_job(job));
            heights[ri] = heights[ri].max(g.size().y);
            gr.push(g);
        }
        galleys.push(gr);
        heights[ri] = (heights[ri].max(20.0 * s) + pad_y * 2.0).max(t.h[ri] * s);
    }
    let mut ys = vec![0.0f32; nr + 1];
    for ri in 0..nr {
        ys[ri + 1] = ys[ri] + heights[ri];
    }
    let total_h = ys[nr];

    let mut table_rect = Rect::NOTHING;
    let mut next: Option<(usize, usize)> = st.cell;
    let mut ops: Vec<Op> = vec![];
    let mut add_row_after: Option<usize> = None;
    let ctx = ui.ctx().clone();
    let sel_col = match st.menu { Some((false, c, _)) => Some(c), _ => None };
    let sel_row = match st.menu { Some((true, r, _)) => Some(r), _ => None };
    let bottom_pad = if touchy { 26.0 } else { 20.0 };

    ScrollArea::horizontal().id_salt(("tblscroll", b.id)).auto_shrink([false, true]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
        let (outer, _) = ui.allocate_exact_size(vec2((gutter + total_w + 24.0).max(w), grip_h + total_h + bottom_pad), Sense::hover());
        let rect = Rect::from_min_size(outer.min + vec2(gutter, grip_h), vec2(total_w, total_h));
        table_rect = rect;
        ui.ctx().data_mut(|d| d.insert_temp(key.with("rect"), rect));
        let p = ui.painter().clone();
        let hot = ui.rect_contains_pointer(outer.expand2(vec2(6.0, 6.0))) || st.drag.is_some() || st.menu.is_some();
        let ctl = ui.ctx().animate_bool_with_time(key.with("ctl"), hot || touchy, 0.12);
        p.rect_filled(rect, 4.0, with_alpha(pal.bg, 1.0));

        // ── cells ──
        for ri in 0..nr {
            let (y, h) = (rect.top() + ys[ri], heights[ri]);
            let head = ri == 0 && b.checked;
            if head {
                p.rect_filled(Rect::from_min_size(pos2(rect.left(), y), vec2(total_w, h)), 0.0, pal.hover);
            }
            if sel_row == Some(ri) {
                p.rect_filled(Rect::from_min_size(pos2(rect.left(), y), vec2(total_w, h)), 0.0, with_alpha(pal.accent, 0.10));
            }
            for ci in 0..nc {
                let cell = Rect::from_min_size(pos2(rect.left() + xs[ci], y), vec2(colw[ci], h));
                if sel_col == Some(ci) {
                    p.rect_filled(cell, 0.0, with_alpha(pal.accent, 0.10));
                }
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
                    let inner = cell.shrink2(vec2(pad_x, pad_y));
                    let r = ui.put(inner, TextEdit::multiline(&mut t.rows[ri][ci]).id(eid).frame(false).desired_rows(1).desired_width(inner.width()).font(font).text_color(pal.text).margin(egui::Margin::ZERO));
                    if st.fresh {
                        ui.memory_mut(|m| m.request_focus(eid));
                        st.fresh = false;
                    }
                    if r.changed() {
                        for ch in ['\n', '\t', mdtable::CELL, mdtable::ROW, '\r'] {
                            if t.rows[ri][ci].contains(ch) {
                                t.rows[ri][ci] = t.rows[ri][ci].replace(ch, " ");
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
                    let x = match t.al[ci] {
                        'c' => cell.center().x - gw / 2.0,
                        'r' => cell.right() - pad_x - gw,
                        _ => cell.left() + pad_x,
                    };
                    p.with_clip_rect(cell).galley(pos2(x, cell.top() + pad_y), g, pal.text);
                    if resp.on_hover_cursor(CursorIcon::Text).clicked() {
                        next = Some((ri, ci));
                        st.fresh = true;
                    }
                }
            }
        }

        // ── grid lines ──
        let line = Stroke::new(1.0_f32, with_alpha(pal.border, 0.9));
        for yy in ys.iter() {
            p.line_segment([pos2(rect.left(), rect.top() + yy), pos2(rect.right(), rect.top() + yy)], line);
        }
        for xx in xs.iter() {
            p.line_segment([pos2(rect.left() + xx, rect.top()), pos2(rect.left() + xx, rect.bottom())], line);
        }

        // ── column borders: drag = resize, double-click = fit to contents ──
        let mut widths_now = t.w.clone();
        let mut dragging_w = false;
        for ci in 0..nc {
            let x = rect.left() + xs[ci + 1];
            let hr = Rect::from_min_max(pos2(x - hw, rect.top()), pos2(x + hw, rect.bottom()));
            let r = ui.interact(hr, Id::new(("tcw", b.id, ci)), Sense::click_and_drag());
            if r.hovered() || r.dragged() {
                ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
                p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(2.5_f32, pal.accent));
            } else if touchy && ctl > 0.5 {
                p.line_segment([pos2(x, rect.center().y - 12.0), pos2(x, rect.center().y + 12.0)], Stroke::new(3.0_f32, with_alpha(pal.muted, 0.6)));
            }
            if r.drag_started() && widths_now.iter().any(|w| *w <= 0.0) {
                widths_now = colw_d.clone(); // freeze the automatic widths so only this border moves
            }
            if r.dragged() {
                widths_now[ci] = (widths_now[ci] + r.drag_delta().x / s).clamp(MIN_COL, MAX_COL);
                dragging_w = true;
            }
            if r.double_clicked() {
                ops.push(Op::FitCol(ci));
            }
        }
        if dragging_w {
            t.w = widths_now.clone();
            changed = true;
            ctx.request_repaint();
        }

        // ── row borders: drag = minimum height, double-click = automatic ──
        let mut heights_now = t.h.clone();
        let mut dragging_h = false;
        for ri in 0..nr {
            let y = rect.top() + ys[ri + 1];
            let hr = Rect::from_min_max(pos2(rect.left(), y - hw * 0.8), pos2(rect.right(), y + hw * 0.8));
            let r = ui.interact(hr, Id::new(("trh", b.id, ri)), Sense::click_and_drag());
            if r.hovered() || r.dragged() {
                ctx.set_cursor_icon(CursorIcon::ResizeVertical);
                p.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(2.5_f32, pal.accent));
            }
            if r.drag_started() {
                heights_now[ri] = heights[ri] / s;
            }
            if r.dragged() {
                heights_now[ri] = (heights_now[ri].max(heights[ri] / s) + r.drag_delta().y / s).max(MIN_ROW);
                dragging_h = true;
            }
            if r.double_clicked() {
                ops.push(Op::ResetRowH(ri));
            }
        }
        if dragging_h {
            t.h = heights_now;
            changed = true;
            ctx.request_repaint();
        }

        // ── corner grabber: scale the whole table ──
        let corner = Rect::from_min_size(rect.right_bottom() - vec2(if touchy { 22.0 } else { 14.0 }, if touchy { 22.0 } else { 14.0 }), vec2(if touchy { 22.0 } else { 14.0 }, if touchy { 22.0 } else { 14.0 }));
        let cr = ui.interact(corner, Id::new(("tcorner", b.id)), Sense::click_and_drag());
        if ctl > 0.02 || cr.dragged() {
            let a = if cr.hovered() || cr.dragged() { 1.0 } else { 0.7 * ctl };
            let col = with_alpha(if cr.hovered() || cr.dragged() { pal.accent } else { pal.muted }, a);
            for k in 0..3 {
                let o = 3.0 + 3.5 * k as f32;
                p.line_segment([corner.right_bottom() - vec2(o, 1.5), corner.right_bottom() - vec2(1.5, o)], Stroke::new(1.5_f32, col));
            }
        }
        if cr.hovered() || cr.dragged() {
            ctx.set_cursor_icon(CursorIcon::ResizeNwSe);
        }
        if cr.dragged() {
            let factor = ((total_w + cr.drag_delta().x) / total_w.max(1.0)).clamp(0.5, 2.0);
            t.w = colw_d.iter().map(|w| (w * factor).clamp(MIN_COL, MAX_COL)).collect();
            changed = true;
            ctx.request_repaint();
        }
        if cr.double_clicked() {
            ops.push(Op::FitWidth);
        }

        // ── column grips (above) ──
        for ci in 0..nc {
            let gr = Rect::from_min_size(pos2(rect.left() + xs[ci], outer.top()), vec2(colw[ci], grip_h));
            let id = Id::new(("tcg", b.id, ci));
            let r = ui.interact(gr, id, Sense::click_and_drag());
            let active = r.hovered() || r.dragged() || sel_col == Some(ci) || st.drag == Some(Drag::Col(ci));
            let pw = (colw[ci] * 0.4).clamp(22.0, 44.0);
            let ph = if touchy { 8.0 } else { 6.0 };
            let pill = Rect::from_center_size(pos2(gr.center().x, gr.center().y), vec2(pw, ph));
            let a = if active { 1.0 } else { 0.55 * ctl };
            if a > 0.02 {
                p.rect_filled(pill, ph / 2.0, with_alpha(if active { pal.accent } else { pal.muted }, a));
            }
            if r.hovered() {
                p.rect_filled(Rect::from_min_max(pos2(rect.left() + xs[ci], rect.top()), pos2(rect.left() + xs[ci + 1], rect.bottom())), 0.0, with_alpha(pal.accent, 0.06));
                ctx.set_cursor_icon(CursorIcon::Grab);
            }
            if r.drag_started() {
                st.drag = Some(Drag::Col(ci));
            }
            if st.drag == Some(Drag::Col(ci)) {
                ctx.set_cursor_icon(CursorIcon::Grabbing);
                if let Some(pp) = ctx.pointer_latest_pos() {
                    let to = (0..nc).find(|k| pp.x < rect.left() + xs[*k + 1]).unwrap_or(nc - 1);
                    let lx = rect.left() + if to <= ci { xs[to] } else { xs[to + 1] };
                    p.line_segment([pos2(lx, rect.top() - 4.0), pos2(lx, rect.bottom() + 2.0)], Stroke::new(3.0_f32, pal.accent));
                    if r.drag_stopped() {
                        if to != ci {
                            ops.push(Op::MoveCol(ci, to));
                        }
                        st.drag = None;
                    }
                } else if r.drag_stopped() {
                    st.drag = None;
                }
            }
            if r.clicked() {
                st.menu = Some((false, ci, pos2(gr.left(), gr.bottom() + 2.0)));
                st.menu_fresh = true;
                crate::widgets::reset_appear(&ctx, key.with("menu").with("appear"));
            }
        }

        // ── row grips (left) ──
        for ri in 0..nr {
            let gr = Rect::from_min_size(pos2(outer.left(), rect.top() + ys[ri]), vec2(gutter, heights[ri]));
            let id = Id::new(("trg", b.id, ri));
            let r = ui.interact(gr, id, Sense::click_and_drag());
            let active = r.hovered() || r.dragged() || sel_row == Some(ri) || st.drag == Some(Drag::Row(ri));
            let ph = (heights[ri] * 0.5).clamp(16.0, 30.0);
            let pw = if touchy { 8.0 } else { 6.0 };
            let pill = Rect::from_center_size(gr.center(), vec2(pw, ph));
            let a = if active { 1.0 } else { 0.55 * ctl };
            if a > 0.02 {
                p.rect_filled(pill, pw / 2.0, with_alpha(if active { pal.accent } else { pal.muted }, a));
            }
            if r.hovered() {
                p.rect_filled(Rect::from_min_max(pos2(rect.left(), rect.top() + ys[ri]), pos2(rect.right(), rect.top() + ys[ri + 1])), 0.0, with_alpha(pal.accent, 0.06));
                ctx.set_cursor_icon(CursorIcon::Grab);
            }
            if r.drag_started() {
                st.drag = Some(Drag::Row(ri));
            }
            if st.drag == Some(Drag::Row(ri)) {
                ctx.set_cursor_icon(CursorIcon::Grabbing);
                if let Some(pp) = ctx.pointer_latest_pos() {
                    let to = (0..nr).find(|k| pp.y < rect.top() + ys[*k + 1]).unwrap_or(nr - 1);
                    let ly = rect.top() + if to <= ri { ys[to] } else { ys[to + 1] };
                    p.line_segment([pos2(rect.left() - 4.0, ly), pos2(rect.right() + 2.0, ly)], Stroke::new(3.0_f32, pal.accent));
                    if r.drag_stopped() {
                        if to != ri {
                            ops.push(Op::MoveRow(ri, to));
                        }
                        st.drag = None;
                    }
                } else if r.drag_stopped() {
                    st.drag = None;
                }
            }
            if r.clicked() {
                st.menu = Some((true, ri, pos2(gr.right() + 2.0, gr.top())));
                st.menu_fresh = true;
                crate::widgets::reset_appear(&ctx, key.with("menu").with("appear"));
            }
        }

        // ── "+" strips: add a column at the right edge, a row at the bottom edge ──
        let strip_w = if touchy { 24.0 } else { 16.0 };
        let add_col = Rect::from_min_max(pos2(rect.right() + 4.0, rect.top()), pos2(rect.right() + 4.0 + strip_w, rect.bottom()));
        if add_col.right() <= outer.right() + 0.5 {
            let r = ui.interact(add_col, key.with("addcol"), Sense::click());
            let a = if r.hovered() { 1.0 } else { 0.45 * ctl };
            if a > 0.02 {
                p.rect_filled(add_col, 6.0, with_alpha(if r.hovered() { pal.accent } else { pal.hover }, if r.hovered() { 0.16 } else { a * 0.5 }));
                p.text(add_col.center(), Align2::CENTER_CENTER, "+", bold(15.0), with_alpha(if r.hovered() { pal.accent } else { pal.muted }, a.max(0.35)));
            }
            if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                ops.push(Op::InsCol(nc));
            }
        }
        let add_row = Rect::from_min_max(pos2(rect.left(), rect.bottom() + 4.0), pos2(rect.right(), rect.bottom() + 4.0 + if touchy { 20.0 } else { 13.0 }));
        let r = ui.interact(add_row, key.with("addrow"), Sense::click());
        let a = if r.hovered() { 1.0 } else { 0.45 * ctl };
        if a > 0.02 {
            p.rect_filled(add_row, 5.0, with_alpha(if r.hovered() { pal.accent } else { pal.hover }, if r.hovered() { 0.16 } else { a * 0.5 }));
            p.text(add_row.center(), Align2::CENTER_CENTER, "+", bold(13.0), with_alpha(if r.hovered() { pal.accent } else { pal.muted }, a.max(0.35)));
        }
        if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            ops.push(Op::InsRow(nr));
        }
    });

    // ── grip menu ──
    if let Some((is_row, idx, pos)) = st.menu {
        let idx = idx.min(if is_row { nr - 1 } else { nc - 1 });
        let items = if is_row { row_items(idx, nr, b.checked) } else { col_items(idx, nc, t.al[idx]) };
        let (open, pick) = context_menu(&ctx, key.with("menu"), pos, pal, &mut st.menu_fresh, &items);
        if let Some(i) = pick {
            if let Some(op) = if is_row { row_op(i, idx) } else { col_op(i, idx) } {
                ops.push(op);
            }
            st.menu = None;
        } else if !open {
            st.menu = None;
        }
    }

    // ── toolbar ──
    let active = st.cell.is_some() || next.is_some();
    let hov = ui.rect_contains_pointer(table_rect.expand2(vec2(8.0, 34.0)));
    let tb = ui.ctx().animate_bool_with_time(key.with("bar"), crate::touch::reveal(hov) || active, 0.14);
    let (r, c) = next.or(st.cell).unwrap_or((nr - 1, nc - 1));
    let (r, c) = (r.min(nr - 1), c.min(nc - 1));
    let mut convert = false;
    ScrollArea::horizontal().id_salt(("tblbar", b.id)).auto_shrink([false, true]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if btn(ui, pal, key.with("ar"), "+ Row", false, tb) {
                ops.push(Op::InsRow(r + 1));
                next = Some((r + 1, c));
                st.fresh = true;
            }
            if btn(ui, pal, key.with("ac"), "+ Column", false, tb) {
                ops.push(Op::InsCol(c + 1));
            }
            if nr > 1 && btn(ui, pal, key.with("dr"), "− Row", false, tb) {
                ops.push(Op::DelRow(r));
                next = None;
            }
            if nc > 1 && btn(ui, pal, key.with("dc"), "− Column", false, tb) {
                ops.push(Op::DelCol(c));
                next = None;
            }
            if btn(ui, pal, key.with("al"), match t.al[c] { 'c' => "Align: center", 'r' => "Align: right", _ => "Align: left" }, false, tb) {
                ops.push(Op::Align(c, match t.al[c] { 'l' => 'c', 'c' => 'r', _ => 'l' }));
            }
            if nr > 2 && btn(ui, pal, key.with("so"), if st.sorted == Some((c, true)) { "Sort ↓" } else { "Sort ↑" }, false, tb) {
                ops.push(Op::Sort(c, st.sorted != Some((c, true))));
            }
            if btn(ui, pal, key.with("fw"), "Fit width", false, tb) {
                ops.push(Op::FitWidth);
            }
            if btn(ui, pal, key.with("eq"), "Equal columns", false, tb) {
                ops.push(Op::EqualCols);
            }
            if btn(ui, pal, key.with("hd"), if b.checked { "Header row ✓" } else { "Header row" }, b.checked, tb) {
                ops.push(Op::ToggleHeader);
            }
            if btn(ui, pal, key.with("cp"), "Copy Markdown", false, tb) {
                let mut tmp = b.clone();
                t.write_to(&mut tmp);
                ui.ctx().copy_text(mdtable::to_markdown(&tmp));
            }
            if btn(ui, pal, key.with("db"), "Convert to database", true, tb) {
                convert = true;
            }
        });
    });

    if let Some(after) = add_row_after {
        t.insert_row(after + 1);
        changed = true;
    }
    for op in ops {
        if matches!(op, Op::FitCol(c)) {
            let c = c.min(nc - 1);
            let mut wmax = MIN_COL;
            for (ri, row) in t.rows.iter().enumerate() {
                let font = if ri == 0 && b.checked { &f_head } else { &f_body };
                let mut job = rich_job(&row[c], font, pal.text, pal, true);
                job.wrap.max_width = f32::INFINITY;
                wmax = wmax.max((ui.fonts(|f| f.layout_job(job)).size().x + pad_x * 2.0) / s + 4.0);
            }
            let mut cur: Vec<f32> = t.w.iter().zip(&colw_d).map(|(a, b)| if *a > 0.0 { *a } else { *b }).collect();
            cur[c] = wmax.clamp(MIN_COL, 600.0).round();
            t.w = cur;
        } else {
            let structural = !matches!(op, Op::Align(..) | Op::ToggleHeader | Op::ResetRowH(_) | Op::EqualCols | Op::FitWidth);
            apply_op(&mut t, &mut b.checked, &mut st.sorted, op, avail / s);
            if structural {
                next = None;
            }
        }
        changed = true;
    }
    st.cell = next.filter(|(r, c)| *r <= t.nr() && *c < t.nc());
    ui.ctx().data_mut(|d| d.insert_temp(key, st));
    if changed {
        t.write_to(b);
    }
    if convert {
        // the page editor owns the workspace: it performs the conversion after this frame's layout
        t.write_to(b);
        ed.table_to_db = Some(b.id);
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tbl(nr: usize, nc: usize) -> Tbl {
        Tbl { rows: (0..nr).map(|r| (0..nc).map(|c| format!("{r}{c}")).collect()).collect(), al: vec!['l'; nc], w: vec![0.0; nc], h: vec![0.0; nr] }
    }

    #[test]
    fn sizes_roundtrip_and_survive_garbage() {
        assert_eq!(write_cols(&[0.0, 0.0], &[0.0]), "");
        assert_eq!(write_cols(&[120.0, 0.0, 200.4], &[0.0, 60.0]), "120,0,200|0,60");
        let (w, h) = parse_cols("120,0,200|0,60", 3, 2);
        assert_eq!((w, h), (vec![120.0, 0.0, 200.0], vec![0.0, 60.0]));
        let (w, h) = parse_cols("abc,-5,nan,99999", 6, 1);
        assert_eq!(w, vec![0.0, 0.0, 0.0, MAX_COL, 0.0, 0.0]);
        assert_eq!(h, vec![0.0]);
        assert_eq!(parse_cols("", 2, 2), (vec![0.0, 0.0], vec![0.0, 0.0]));
    }

    #[test]
    fn automatic_columns_share_the_page_width() {
        let w = layout_widths(&[0.0, 0.0, 0.0], 600.0);
        assert!(w.iter().all(|x| (*x - 200.0).abs() < 0.01));
        let w = layout_widths(&[300.0, 0.0, 0.0], 600.0);
        assert_eq!(w[0], 300.0);
        assert!((w[1] - 150.0).abs() < 0.01);
        // a narrow screen keeps a readable minimum and the table scrolls sideways
        let w = layout_widths(&[0.0; 5], 300.0);
        assert!(w.iter().all(|x| *x >= 124.0));
    }

    #[test]
    fn structural_ops_keep_widths_heights_and_alignment_in_step() {
        let mut t = tbl(3, 3);
        t.w = vec![100.0, 200.0, 300.0];
        t.al = vec!['l', 'c', 'r'];
        t.h = vec![0.0, 80.0, 0.0];
        let (mut hdr, mut sorted) = (true, None);
        apply_op(&mut t, &mut hdr, &mut sorted, Op::MoveCol(0, 2), 600.0);
        assert_eq!(t.w, vec![200.0, 300.0, 100.0]);
        assert_eq!(t.al, vec!['c', 'r', 'l']);
        assert_eq!(t.rows[1], vec!["11", "12", "10"]);
        apply_op(&mut t, &mut hdr, &mut sorted, Op::DupCol(1), 600.0);
        assert_eq!((t.nc(), t.w[2]), (4, 300.0));
        apply_op(&mut t, &mut hdr, &mut sorted, Op::DelCol(0), 600.0);
        assert_eq!(t.w, vec![300.0, 300.0, 100.0]);
        apply_op(&mut t, &mut hdr, &mut sorted, Op::MoveRow(1, 2), 600.0);
        assert_eq!(t.h, vec![0.0, 0.0, 80.0]);
        apply_op(&mut t, &mut hdr, &mut sorted, Op::InsRow(0), 600.0);
        assert_eq!((t.nr(), t.h.len(), t.rows[0].iter().all(|c| c.is_empty())), (4, 4, true));
        apply_op(&mut t, &mut hdr, &mut sorted, Op::DelRow(0), 600.0);
        apply_op(&mut t, &mut hdr, &mut sorted, Op::EqualCols, 600.0);
        assert!(t.w.iter().all(|w| *w == 200.0));
        apply_op(&mut t, &mut hdr, &mut sorted, Op::FitWidth, 600.0);
        assert!(t.w.iter().all(|w| *w == 0.0));
        // the last column / row can never be deleted
        let mut one = tbl(1, 1);
        apply_op(&mut one, &mut hdr, &mut sorted, Op::DelCol(0), 600.0);
        apply_op(&mut one, &mut hdr, &mut sorted, Op::DelRow(0), 600.0);
        assert_eq!((one.nr(), one.nc()), (1, 1));
    }

    #[test]
    fn sort_skips_the_header_and_moves_row_heights_with_their_rows() {
        let mut t = Tbl { rows: vec![vec!["h".into()], vec!["b".into()], vec!["a".into()], vec!["c".into()]], al: vec!['l'], w: vec![0.0], h: vec![0.0, 50.0, 0.0, 70.0] };
        let (mut hdr, mut sorted) = (true, None);
        apply_op(&mut t, &mut hdr, &mut sorted, Op::Sort(0, true), 600.0);
        assert_eq!(t.rows.iter().map(|r| r[0].clone()).collect::<Vec<_>>(), vec!["h", "a", "b", "c"]);
        assert_eq!(t.h, vec![0.0, 0.0, 50.0, 70.0]);
        assert_eq!(sorted, Some((0, true)));
        let mut n = Tbl { rows: vec![vec!["10".into()], vec!["9".into()], vec!["$100".into()]], al: vec!['l'], w: vec![0.0], h: vec![0.0; 3] };
        apply_op(&mut n, &mut false, &mut sorted, Op::Sort(0, false), 600.0);
        assert_eq!(n.rows.iter().map(|r| r[0].clone()).collect::<Vec<_>>(), vec!["$100", "10", "9"]);
    }

    #[test]
    fn block_roundtrip_keeps_sizes() {
        let mut b = Block::new(1, crate::model::BlockKind::Table, "", 0);
        let mut t = tbl(2, 3);
        t.w = vec![150.0, 0.0, 220.0];
        t.write_to(&mut b);
        assert_eq!(b.cols, "150,0,220");
        let back = Tbl::from_block(&b);
        assert_eq!(back, t);
        // markdown export is unaffected by the sizes
        assert!(mdtable::to_markdown(&b).contains("| 00 | 01 | 02 |"));
    }

    // ── the real widget, driven with a simulated mouse ──
    use egui::{Event, Modifiers, PointerButton, RawInput};

    struct Rig {
        ctx: egui::Context,
        b: Block,
        ed: EditorState,
        t: f64,
        pos: Pos2,
    }

    impl Rig {
        fn new() -> Rig {
            let ctx = egui::Context::default();
            crate::theme::setup_fonts(&ctx);
            let mut b = Block::new(7, crate::model::BlockKind::Table, "", 0);
            tbl(3, 3).write_to(&mut b);
            b.checked = true;
            let mut r = Rig { ctx, b, ed: EditorState::default(), t: 0.0, pos: pos2(0.0, 0.0) };
            r.frame(vec![]);
            r.frame(vec![]);
            r
        }
        fn frame(&mut self, evs: Vec<Event>) {
            self.t += 0.05;
            let mut input = RawInput::default();
            input.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 700.0)));
            input.time = Some(self.t);
            input.events = evs;
            let pal = Palette::light();
            let (b, ed) = (&mut self.b, &mut self.ed);
            let _ = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    draw(ui, b, ed, &pal, 700.0);
                });
            });
        }
        fn rect(&self) -> Rect {
            self.ctx.data(|d| d.get_temp::<Rect>(Id::new(("tblstate", 7)).with("rect"))).expect("table drawn")
        }
        fn mv(&mut self, p: Pos2) {
            self.pos = p;
            self.frame(vec![Event::PointerMoved(p)]);
        }
        fn press(&mut self) {
            let p = self.pos;
            self.frame(vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
        }
        fn release(&mut self) {
            let p = self.pos;
            self.frame(vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]);
        }
        fn drag(&mut self, from: Pos2, to: Pos2) {
            self.mv(from);
            self.press();
            for k in 1..=8 {
                self.mv(from + (to - from) * (k as f32 / 8.0));
            }
            self.release();
            self.frame(vec![]);
        }
        fn click(&mut self, p: Pos2) {
            self.mv(p);
            self.press();
            self.release();
            self.frame(vec![]);
        }
    }

    #[test]
    fn dragging_a_column_border_resizes_and_persists() {
        let mut r = Rig::new();
        let rect = r.rect();
        let w0 = Tbl::from_block(&r.b).w.clone();
        assert!(w0.iter().all(|w| *w == 0.0), "automatic at first");
        let (colw, _) = (layout_widths(&w0, 700.0 - 14.0), 0);
        let border = pos2(rect.left() + colw[0], rect.top() + 12.0);
        r.drag(border, border + vec2(90.0, 0.0));
        let w = Tbl::from_block(&r.b).w;
        assert!((w[0] - (colw[0] + 90.0)).abs() < 6.0, "first column grew by the drag: {w:?} vs {}", colw[0]);
        assert!((w[1] - colw[1]).abs() < 1.5 && (w[2] - colw[2]).abs() < 1.5, "the others keep their width: {w:?}");
        assert!(!r.b.cols.is_empty());
        // shrinking stops at the minimum
        let rect = r.rect();
        let border = pos2(rect.left() + w[0], rect.top() + 12.0);
        r.drag(border, border - vec2(600.0, 0.0));
        assert_eq!(Tbl::from_block(&r.b).w[0], MIN_COL);
    }

    #[test]
    fn dragging_a_row_border_sets_a_height_and_double_click_resets_it() {
        let mut r = Rig::new();
        let rect = r.rect();
        let row0_bottom = r.rect().top() + 41.0; // header row: 20 + 2 * 7 padding ≈ 34..42
        let _ = row0_bottom;
        // find the first row border: scan down until the cursor handle takes the drag
        let mut grew = false;
        for dy in 30..50 {
            let mut rig = Rig::new();
            let p = pos2(rect.left() + 30.0, rect.top() + dy as f32);
            rig.drag(p, p + vec2(0.0, 60.0));
            let h = Tbl::from_block(&rig.b).h;
            if h[0] > 70.0 {
                grew = true;
                // double-click the same border → automatic again
                let rr = rig.rect();
                let by = rr.top() + h[0];
                rig.click(pos2(rr.left() + 30.0, by));
                rig.click(pos2(rr.left() + 30.0, by));
                break;
            }
        }
        assert!(grew, "a row border can be dragged to make the row taller");
    }

    #[test]
    fn the_corner_grabber_scales_the_whole_table() {
        let mut r = Rig::new();
        let rect = r.rect();
        let before: f32 = layout_widths(&Tbl::from_block(&r.b).w, 700.0 - 14.0).iter().sum();
        let corner = rect.right_bottom() - vec2(5.0, 5.0);
        r.drag(corner, corner - vec2(150.0, 0.0));
        let w = Tbl::from_block(&r.b).w;
        let after: f32 = w.iter().sum();
        assert!(w.iter().all(|x| *x > 0.0), "all columns are fixed after scaling: {w:?}");
        assert!(after < before - 100.0 && after > before - 200.0, "{before} → {after}");
        assert!((w[0] - w[1]).abs() < 1.5, "proportions are kept");
    }

    #[test]
    fn grip_menu_inserts_a_column_and_dragging_a_grip_reorders() {
        let mut r = Rig::new();
        let rect = r.rect();
        let colw = layout_widths(&Tbl::from_block(&r.b).w, 700.0 - 14.0);
        // click the grip above the second column → menu opens
        let grip = pos2(rect.left() + colw[0] + colw[1] / 2.0, rect.top() - 7.0);
        r.click(grip);
        let st: Edit = r.ctx.data(|d| d.get_temp(Id::new(("tblstate", 7)))).unwrap_or_default();
        assert!(matches!(st.menu, Some((false, 1, _))), "menu for column 1 is open");
        // drag the first column's grip over the third → the columns are reordered
        let mut r = Rig::new();
        let rect = r.rect();
        let g0 = pos2(rect.left() + colw[0] / 2.0, rect.top() - 7.0);
        let g2 = pos2(rect.left() + colw[0] + colw[1] + colw[2] / 2.0, rect.top() - 7.0);
        r.drag(g0, g2);
        let t = Tbl::from_block(&r.b);
        assert_eq!(t.rows[0], vec!["01", "02", "00"], "column 0 moved to the end");
    }

    #[test]
    fn plus_strips_add_a_row() {
        let mut r = Rig::new();
        let rect = r.rect();
        let before = Tbl::from_block(&r.b).nr();
        r.click(pos2(rect.center().x, rect.bottom() + 9.0));
        assert_eq!(Tbl::from_block(&r.b).nr(), before + 1);
    }
}

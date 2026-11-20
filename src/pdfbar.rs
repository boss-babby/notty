//! Touch- and mouse-draggable scroll bars for the PDF reader.
//!
//! egui's own floating scroll bars are 4–8 px wide and cannot be grabbed with a finger (the PDF area also turns off
//! `drag_to_scroll`, the finger belongs to text selection / panning). This module draws a real bar:
//! * vertical bar on the right edge with a 40 px wide hit zone (an 18 px wide thumb while dragged), a page bubble ("12 / 80") while it is dragged;
//! * horizontal bar at the bottom when the page is zoomed wider than the screen;
//! * a press anywhere in the zone jumps the thumb under the finger (track tap), dragging scrolls 1:1 with the thumb;
//! * the zone is *claimed* from the touch gesture engine ([`crate::touch::claim`]) so a swipe starting on the bar never also scrolls the page,
//!   and the reader ignores presses that start on a bar ([`SBar::busy`]).
//!
//! Pure geometry lives in [`vbar`] / [`offset_for_thumb`] and is unit tested.

use egui::{pos2, vec2, Align2, Color32, Context, Painter, Pos2, Rect, Stroke, Vec2};

use crate::theme::{bold, Palette};

/// Width of the touch zone (points).
pub const ZONE: f32 = 40.0;
const MIN_THUMB: f32 = 56.0;

#[derive(Default, Clone, Copy, PartialEq)]
enum Drag {
    #[default]
    None,
    V,
    H,
}

#[derive(Default)]
pub struct SBar {
    drag: Drag,
    /// Distance between the press point and the thumb's leading edge.
    grab: f32,
    /// Last time (egui seconds) the offset changed or a bar was touched; the bars fade out afterwards.
    last_move: f64,
    last_off: Vec2,
    /// A bar owns the pointer this frame.
    pub busy: bool,
}

/// `(track, thumb)` of a bar along one axis: `view_len` visible of `content_len`, scrolled by `off`, in a track of `track_len` points.
/// Returns `(thumb_len, thumb_start)` relative to the track start, or `None` when nothing scrolls.
pub fn thumb_geometry(view_len: f32, content_len: f32, off: f32, track_len: f32) -> Option<(f32, f32)> {
    if content_len <= view_len + 1.0 || track_len < 20.0 {
        return None;
    }
    let max_off = content_len - view_len;
    let len = (track_len * view_len / content_len).clamp(MIN_THUMB.min(track_len * 0.5), track_len);
    let t = (off / max_off).clamp(0.0, 1.0);
    Some((len, t * (track_len - len)))
}

/// Scroll offset for a thumb whose leading edge is at `thumb_start` (relative to the track).
pub fn offset_for_thumb(view_len: f32, content_len: f32, track_len: f32, thumb_len: f32, thumb_start: f32) -> f32 {
    let room = (track_len - thumb_len).max(1.0);
    ((thumb_start / room).clamp(0.0, 1.0)) * (content_len - view_len).max(0.0)
}

fn v_track(view: Rect, has_h: bool) -> Rect {
    Rect::from_min_max(pos2(view.right() - ZONE, view.top() + 6.0), pos2(view.right(), view.bottom() - 6.0 - if has_h { 22.0 } else { 0.0 }))
}

fn h_track(view: Rect, has_v: bool) -> Rect {
    Rect::from_min_max(pos2(view.left() + 6.0, view.bottom() - 24.0), pos2(view.right() - 6.0 - if has_v { ZONE } else { 0.0 }, view.bottom()))
}

impl SBar {
    /// Handles input before the scroll area is laid out. Returns the offset to scroll to while a bar is dragged.
    pub fn input(&mut self, ctx: &Context, view: Rect, off: Vec2, content: Vec2) -> Option<Vec2> {
        self.busy = false;
        let has_v = content.y > view.height() + 1.0;
        let has_h = content.x > view.width() + 1.0;
        let now = ctx.input(|i| i.time);
        if (off - self.last_off).length() > 0.5 {
            self.last_off = off;
            self.last_move = now;
        }
        if !has_v && !has_h {
            self.drag = Drag::None;
            return None;
        }
        let vt = v_track(view, has_h);
        let ht = h_track(view, has_v);
        if has_v {
            crate::touch::claim(Rect::from_min_max(vt.min - vec2(0.0, 6.0), vt.max + vec2(0.0, 6.0)));
        }
        if has_h {
            crate::touch::claim(Rect::from_min_max(ht.min - vec2(6.0, 0.0), ht.max + vec2(6.0, 0.0)));
        }
        let (pressed, down, pos) = ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos()));
        if pressed {
            if let Some(p) = pos {
                if has_v && vt.contains(p) {
                    self.drag = Drag::V;
                    let (len, start) = thumb_geometry(view.height(), content.y, off.y, vt.height()).unwrap_or((MIN_THUMB, 0.0));
                    let thumb_top = vt.top() + start;
                    self.grab = if p.y >= thumb_top && p.y <= thumb_top + len { p.y - thumb_top } else { len / 2.0 };
                } else if has_h && ht.contains(p) {
                    self.drag = Drag::H;
                    let (len, start) = thumb_geometry(view.width(), content.x, off.x, ht.width()).unwrap_or((MIN_THUMB, 0.0));
                    let thumb_left = ht.left() + start;
                    self.grab = if p.x >= thumb_left && p.x <= thumb_left + len { p.x - thumb_left } else { len / 2.0 };
                }
            }
        }
        if !down {
            self.drag = Drag::None;
        }
        if self.drag == Drag::None {
            return None;
        }
        self.busy = true;
        self.last_move = now;
        ctx.request_repaint();
        let p = pos?;
        match self.drag {
            Drag::V => {
                let (len, _) = thumb_geometry(view.height(), content.y, off.y, vt.height())?;
                let start = p.y - self.grab - vt.top();
                Some(vec2(off.x, offset_for_thumb(view.height(), content.y, vt.height(), len, start)))
            }
            Drag::H => {
                let (len, _) = thumb_geometry(view.width(), content.x, off.x, ht.width())?;
                let start = p.x - self.grab - ht.left();
                Some(vec2(offset_for_thumb(view.width(), content.x, ht.width(), len, start), off.y))
            }
            Drag::None => None,
        }
    }

    /// Paints the bars (after the scroll area, so they sit above the pages).
    pub fn paint(&self, ctx: &Context, painter: &Painter, pal: &Palette, view: Rect, off: Vec2, content: Vec2, page: (usize, usize), night: bool) {
        let has_v = content.y > view.height() + 1.0;
        let has_h = content.x > view.width() + 1.0;
        if !has_v && !has_h {
            return;
        }
        let now = ctx.input(|i| i.time);
        let hover = ctx.pointer_hover_pos().map_or(false, |p| (has_v && v_track(view, has_h).expand(4.0).contains(p)) || (has_h && h_track(view, has_v).expand(4.0).contains(p)));
        let idle = (now - self.last_move) as f32;
        // visible while moving / touched / hovered, then fades over 0.4 s after 1.2 s of rest
        let base = if self.drag != Drag::None || hover { 1.0 } else { (1.0 - (idle - 1.2) / 0.4).clamp(0.0, 1.0) };
        let a = ctx.animate_value_with_time(egui::Id::new("pdf_sbar_a"), base, 0.12);
        if idle < 1.6 && self.drag == Drag::None && !hover {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
        if a <= 0.01 {
            return;
        }
        let ink = if night { Color32::from_gray(235) } else { Color32::WHITE };
        let draw = |track: Rect, vertical: bool, off: f32, view_len: f32, content_len: f32, active: bool| {
            let Some((len, start)) = thumb_geometry(view_len, content_len, off, if vertical { track.height() } else { track.width() }) else { return };
            let thick = if active { 12.0 } else { 6.0 };
            let (lane, thumb) = if vertical {
                let cx = track.right() - 8.0;
                (Rect::from_center_size(pos2(cx, track.center().y), vec2(thick, track.height())), Rect::from_min_size(pos2(cx - thick / 2.0, track.top() + start), vec2(thick, len)))
            } else {
                let cy = track.bottom() - 8.0;
                (Rect::from_center_size(pos2(track.center().x, cy), vec2(track.width(), thick)), Rect::from_min_size(pos2(track.left() + start, cy - thick / 2.0), vec2(len, thick)))
            };
            if active {
                painter.rect_filled(lane, thick / 2.0, Color32::from_black_alpha((70.0 * a) as u8));
            }
            let alpha = ((if active { 235.0 } else { 175.0 }) * a) as u8;
            painter.rect_filled(thumb.translate(vec2(0.0, 1.0)), thick / 2.0, Color32::from_black_alpha((60.0 * a) as u8));
            painter.rect_filled(thumb, thick / 2.0, Color32::from_rgba_unmultiplied(ink.r(), ink.g(), ink.b(), alpha));
            if active && vertical && page.1 > 0 {
                // page bubble next to the thumb
                let label = format!("{} / {}", page.0 + 1, page.1);
                let g = painter.layout_no_wrap(label, bold(15.0), pal.text);
                let size = g.size() + vec2(24.0, 14.0);
                let r = Rect::from_center_size(pos2(thumb.left() - 14.0 - size.x / 2.0, (thumb.center().y).clamp(view.top() + size.y / 2.0 + 4.0, view.bottom() - size.y / 2.0 - 4.0)), size);
                painter.rect_filled(r.translate(vec2(0.0, 2.0)), 12.0, Color32::from_black_alpha(80));
                painter.rect_filled(r, 12.0, pal.popup);
                painter.rect_stroke(r, 12.0, Stroke::new(1.0_f32, pal.border), egui::StrokeKind::Inside);
                painter.text(r.center(), Align2::CENTER_CENTER, format!("{} / {}", page.0 + 1, page.1), bold(15.0), pal.text);
            }
        };
        if has_v {
            draw(v_track(view, has_h), true, off.y, view.height(), content.y, self.drag == Drag::V);
        }
        if has_h {
            draw(h_track(view, has_v), false, off.x, view.width(), content.x, self.drag == Drag::H);
        }
    }
}

/// Is `p` on one of the bars' touch zones? (used by the reader to ignore presses that belong to a bar)
pub fn on_bar(view: Rect, content: Vec2, p: Pos2) -> bool {
    let has_v = content.y > view.height() + 1.0;
    let has_h = content.x > view.width() + 1.0;
    (has_v && v_track(view, has_h).contains(p)) || (has_h && h_track(view, has_v).contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_roundtrip() {
        // 800 of 8000 visible in a 600 track
        let (len, start0) = thumb_geometry(800.0, 8000.0, 0.0, 600.0).unwrap();
        assert!(len >= MIN_THUMB && len <= 600.0);
        assert_eq!(start0, 0.0);
        let (_, start_end) = thumb_geometry(800.0, 8000.0, 7200.0, 600.0).unwrap();
        assert!((start_end - (600.0 - len)).abs() < 0.01);
        let off = offset_for_thumb(800.0, 8000.0, 600.0, len, (600.0 - len) / 2.0);
        assert!((off - 3600.0).abs() < 1.0);
        assert!(thumb_geometry(800.0, 700.0, 0.0, 600.0).is_none());
    }

    #[test]
    fn huge_documents_keep_a_grabbable_thumb() {
        let (len, _) = thumb_geometry(800.0, 4_000_000.0, 10.0, 700.0).unwrap();
        assert!(len >= MIN_THUMB);
    }
}

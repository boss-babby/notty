//! Soft-keyboard policy for touch devices (Android / iOS browsers and installed PWAs).
//!
//! Rules (all of them exist because of a real bug report):
//!
//! 1. **Only a deliberate tap on a text field opens the keyboard.** A short, motionless touch whose end point lies inside the text
//!    field that is being edited arms the keyboard for [`TAP_WINDOW_SECS`]. Text that gets the caret by itself (a new block, a widget that
//!    was just added, a new page, a block that was focused by Enter …) never pops the keyboard up — the user can keep scrolling.
//! 2. **A swipe closes the keyboard.** A finger that moves like a scroll gesture dismisses the keyboard (like iOS' "dismiss on drag") and the
//!    swipe is *not* followed by a re-open.
//! 3. **A caret that is not visible closes the keyboard.** When the caret sits outside the visible part of the screen (below the keyboard,
//!    above the top bar …) for longer than [`HIDDEN_CARET_SECS`] the keyboard is dismissed. Re-opening it needs another tap, therefore
//!    the open → scroll → close → open loop is impossible.
//! 4. **Android "back" hides the keyboard but keeps the input focused**: a tap on the field then re-focuses the hidden input.
//!
//! The mouse / desktop behaviour is unchanged (see [`touch_mode`]).

use std::cell::Cell;

use super::now_sec;

/// A tap stays valid for this long (the editor may hand the caret over one or two frames after the release).
pub const TAP_WINDOW_SECS: f64 = 0.9;
/// The caret must be invisible for this long before the keyboard is closed (the viewport shrinks ~300 ms after the keyboard opens and
/// the editor scrolls the caret into view in the meantime).
pub const HIDDEN_CARET_SECS: f64 = 1.1;
/// A finger that travels further than this (egui points) is a swipe, not a tap.
const SLOP: f32 = 12.0;
/// A touch that lasts longer than this is a press-and-hold (context menu / drag), not a tap.
const TAP_MAX_SECS: f64 = 0.38;

thread_local! {
    static TOUCH_MODE: Cell<bool> = const { Cell::new(false) };
    static DOWN: Cell<Option<(f32, f32, f64)>> = const { Cell::new(None) };
    static MOVED: Cell<f32> = const { Cell::new(0.0) };
    static MULTI: Cell<bool> = const { Cell::new(false) };
    static SWIPED: Cell<bool> = const { Cell::new(false) };
    static TAP: Cell<Option<(f32, f32, f64)>> = const { Cell::new(None) };
    static DISMISS: Cell<bool> = const { Cell::new(false) };
    static HIDDEN_SINCE: Cell<Option<f64>> = const { Cell::new(None) };
    static MAX_VIEW_H: Cell<(f64, f64)> = const { Cell::new((0.0, 0.0)) };
}

/// `true` once a finger has been used (and until a real mouse is used again).
pub fn touch_mode() -> bool {
    TOUCH_MODE.with(|t| t.get())
}

pub fn set_touch_mode(on: bool) {
    TOUCH_MODE.with(|t| t.set(on));
}

/// The browser's own event time (immune to a busy main thread delaying the handler), in seconds on the `now_sec` clock.
pub fn event_time(ts_ms: f64) -> f64 {
    let n = now_sec();
    let t = ts_ms / 1000.0;
    if t <= 0.0 || (n - t).abs() > 5.0 { n } else { t.min(n) }
}

pub fn touch_start(pos: egui::Pos2, fingers: u32, t: f64) {
    set_touch_mode(true);
    if fingers > 1 {
        MULTI.with(|m| m.set(true));
    } else {
        MULTI.with(|m| m.set(false));
        SWIPED.with(|s| s.set(false));
        MOVED.with(|m| m.set(0.0));
        DOWN.with(|d| d.set(Some((pos.x, pos.y, t))));
    }
}

pub fn touch_move(pos: egui::Pos2, now: f64) {
    let Some((x, y, t0)) = DOWN.with(|d| d.get()) else { return };
    let dist = ((pos.x - x).powi(2) + (pos.y - y).powi(2)).sqrt();
    MOVED.with(|m| m.set(m.get().max(dist)));
    // A scroll swipe starts moving right away; a press-and-hold that then drags (selection / drag & drop) does not dismiss.
    if dist > SLOP && now - t0 < TAP_MAX_SECS && !SWIPED.with(|s| s.get()) {
        SWIPED.with(|s| s.set(true));
        DISMISS.with(|d| d.set(true));
    }
}

pub fn touch_end(pos: egui::Pos2, now: f64) {
    let down = DOWN.with(|d| d.take());
    let multi = MULTI.with(|m| m.get());
    if let Some((_, _, t0)) = down {
        let tap = !multi && !SWIPED.with(|s| s.get()) && MOVED.with(|m| m.get()) <= SLOP && now - t0 <= TAP_MAX_SECS;
        TAP.with(|t| t.set(if tap { Some((pos.x, pos.y, now_sec())) } else { None }));
    }
}

pub fn touch_cancel() {
    DOWN.with(|d| d.set(None));
    TAP.with(|t| t.set(None));
}

/// A swipe happened: close the keyboard (consumes the request).
pub fn take_dismiss() -> bool {
    DISMISS.with(|d| d.take())
}

/// The last tap is recent and ended inside `rect` (the text field that is being edited).
pub fn tap_in(rect: egui::Rect) -> bool {
    TAP.with(|t| match t.get() {
        Some((x, y, at)) => now_sec() - at <= TAP_WINDOW_SECS && rect.expand(8.0).contains(egui::pos2(x, y)),
        None => false,
    })
}

pub fn consume_tap() {
    TAP.with(|t| t.set(None));
}

/// Height of the visible page in CSS pixels (shrinks while the soft keyboard is up).
fn view_height() -> f64 {
    let Some(w) = web_sys::window() else { return 0.0 };
    let inner = w.inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
    let vv = w.visual_viewport().map(|v| v.height()).unwrap_or(inner);
    if vv > 0.0 { inner.min(vv) } else { inner }
}

/// Is the on-screen keyboard (probably) showing? The tallest viewport seen for the current width is the "no keyboard" height.
pub fn keyboard_visible() -> bool {
    let Some(w) = web_sys::window() else { return false };
    let width = w.inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
    let h = view_height();
    let (mw, mh) = MAX_VIEW_H.with(|m| m.get());
    let mh = if (mw - width).abs() > 1.0 { h } else { mh.max(h) };
    MAX_VIEW_H.with(|m| m.set((width, mh)));
    mh - h > 120.0
}

/// Tracks how long the caret has been outside `visible`. Returns `true` when the keyboard should be closed.
pub fn caret_hidden_too_long(caret: egui::Rect, visible: egui::Rect) -> bool {
    let c = caret.center();
    let seen = c.y >= visible.top() - 2.0 && c.y <= visible.bottom() + 2.0 && c.x >= visible.left() - 2.0 && c.x <= visible.right() + 2.0;
    if seen {
        HIDDEN_SINCE.with(|h| h.set(None));
        return false;
    }
    let now = now_sec();
    let since = HIDDEN_SINCE.with(|h| match h.get() {
        Some(t) => t,
        None => {
            h.set(Some(now));
            now
        }
    });
    if now - since > HIDDEN_CARET_SECS {
        HIDDEN_SINCE.with(|h| h.set(None));
        true
    } else {
        false
    }
}

pub fn reset_hidden() {
    HIDDEN_SINCE.with(|h| h.set(None));
}

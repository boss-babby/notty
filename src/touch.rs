//! Touch gestures for phones, tablets and touch screens (web + Windows).
//!
//! egui reports a finger as a mouse button + `Event::Touch`. That makes every drag a *drag* — it selects text, draws a marquee or
//! moves a card — so the page can never be scrolled with a finger. This module sits in front of egui (`App::raw_input_hook`) and
//! decides what a finger is doing **before** the widgets see it:
//!
//! * **tap** — released quickly without moving: the press / release is replayed to egui unchanged (click, focus, keyboard).
//! * **swipe** — moves further than a few pixels before the hold time: the pointer events are swallowed and replaced by mouse-wheel
//!   events, so *every* scroll area (page, tables, sidebar, PDF reader, graph) pans natively. When the finger lifts the content keeps
//!   gliding (fling with friction).
//! * **hold** — held still for [`LONG_PRESS`] seconds (a ring grows under the finger, the phone buzzes). The press is *still held back*:
//!   * lift the finger without moving → it is a **right-click**: [`Outcome::context`] / `widgets::inject_rclick` open the context menu
//!     of whatever is under the finger (block menu, text menu, database row menu, sidebar menu …);
//!   * move the finger → it is **hold-and-drag**: the press is delivered at the spot where the finger went down and the gesture becomes
//!     a normal drag (marquee selection, text selection, drag & drop). A plain swipe never does that — it scrolls.
//! * **two fingers** — always passed through untouched (pinch zoom, rotate).
//! * [`set_passthrough`] — a drawing tool (PDF pen) can ask for raw pointer events.
//!
//! The state machine is plain data (no egui context needed) so it is unit-tested below.

use std::cell::Cell;
use std::time::Duration;

use egui::{Context, Event, Id, Modifiers, MouseWheelUnit, Pos2, RawInput, TouchPhase, Vec2};

/// Seconds a finger must rest to count as a hold (right-click / start of a drag selection).
pub const LONG_PRESS: f64 = 0.36;
/// After this many seconds of resting the "hold" ring starts to grow.
const RING_AFTER: f64 = 0.12;
/// Movement (points) of a *held* finger that turns the hold into a drag.
const HOLD_SLOP: f32 = 22.0;
/// Movement (points) that turns a press into a swipe.
const SLOP: f32 = 10.0;
/// Fling friction (1/s) and the speed below which a fling stops (points/s).
const FRICTION: f32 = 3.1;
const MIN_FLING: f32 = 140.0;

thread_local! {
    static PASS: Cell<bool> = const { Cell::new(false) };
    /// A finger has been used (or the device reports a coarse pointer): bigger tap targets everywhere.
    static TOUCHY: Cell<bool> = const { Cell::new(false) };
    /// The window is phone-sized (set by the app shell every frame).
    static NARROW: Cell<bool> = const { Cell::new(false) };
    /// Zones where a finger must grab immediately (drag handles, resize grips) instead of scrolling the page.
    static CLAIMS_NOW: std::cell::RefCell<Vec<egui::Rect>> = const { std::cell::RefCell::new(Vec::new()) };
    static CLAIMS_PREV: std::cell::RefCell<Vec<egui::Rect>> = const { std::cell::RefCell::new(Vec::new()) };
    static SWIPE: Cell<Option<Swipe>> = const { Cell::new(None) };
}

/// A quick horizontal swipe recognised by the gesture layer (used to open / close the sidebar drawer).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Swipe {
    /// Started at the left screen edge and moved right.
    EdgeRight,
    Left,
    Right,
}

/// Registers a rectangle (this frame) in which a finger drags immediately: drag handles, column resizers, timeline bars.
pub fn claim(r: egui::Rect) {
    if !touchy() {
        return;
    }
    CLAIMS_NOW.with(|c| c.borrow_mut().push(r));
}

/// Bigger tap targets / spacing (touch screen in use).
pub fn touchy() -> bool {
    TOUCHY.with(|t| t.get())
}

pub fn set_touchy(on: bool) {
    TOUCHY.with(|t| t.set(on));
}

/// Phone-sized window: single-column layouts, compact gutters.
pub fn compact() -> bool {
    NARROW.with(|t| t.get())
}

pub fn set_narrow(on: bool) {
    NARROW.with(|t| t.set(on));
}

/// A horizontal swipe happened since the last call (consumes it).
pub fn take_swipe() -> Option<Swipe> {
    SWIPE.with(|s| s.take())
}

/// A tool that needs raw finger input (pen / highlighter) calls this every frame it is active.
pub fn set_passthrough(on: bool) {
    PASS.with(|p| p.set(on));
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Idle,
    Pending,
    /// The finger rested for `LONG_PRESS`: release = right-click, move = drag.
    Held,
    Scroll,
    Fling,
    Pass,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Lock {
    Free,
    Vertical,
    Horizontal,
}

pub struct Gestures {
    mode: Mode,
    lock: Lock,
    fingers: u32,
    start: Pos2,
    last: Pos2,
    t0: f64,
    last_t: f64,
    buf: Vec<Event>,
    vel: Vec2,
    /// A finger lifted from a swipe: its release / pointer-gone events must be dropped.
    drop_tail: bool,
    /// Distance travelled this frame (accumulated from `Touch::Move`).
    frame_delta: Vec2,
    /// A touch screen has been used at least once.
    pub seen: bool,
    /// Grab zones of the previous frame (see [`claim`]).
    pub claims: Vec<egui::Rect>,
}

impl Default for Gestures {
    fn default() -> Self {
        Gestures { mode: Mode::Idle, lock: Lock::Free, fingers: 0, start: Pos2::ZERO, last: Pos2::ZERO, t0: 0.0, last_t: 0.0, buf: vec![], vel: Vec2::ZERO, drop_tail: false, frame_delta: Vec2::ZERO, seen: false, claims: vec![] }
    }
}

/// What the gesture layer decided this frame (besides rewriting the events).
#[derive(Default, Debug, PartialEq)]
pub struct Outcome {
    /// A held finger started moving: the press was delivered at this (start) position and a real drag begins.
    pub long_press: Option<Pos2>,
    /// The finger reached the hold time (haptic feedback).
    pub held: Option<Pos2>,
    /// A held finger was lifted without moving: a right-click at this position.
    pub context: Option<Pos2>,
    pub direct: bool,
    pub swipe: Option<Swipe>,
}

impl Gestures {
    /// Web browsers (and some backends) report the pointer press of a finger *before* the touch event. The gesture layer must know a
    /// finger is down before it sees the press, so touch-start events are moved in front of the pointer events that precede them.
    fn touch_first(mut events: Vec<Event>) -> Vec<Event> {
        let Some(ts) = events.iter().position(|e| matches!(e, Event::Touch { phase: TouchPhase::Start, .. })) else { return events };
        if ts == 0 {
            return events;
        }
        let before: Vec<Event> = events.drain(..ts).collect();
        let (mut ptr, other): (Vec<Event>, Vec<Event>) = before.into_iter().partition(|e| matches!(e, Event::PointerMoved(_) | Event::PointerButton { pressed: true, .. }));
        // [other…, Touch…(start), pointer…, rest]
        let mut touches: Vec<Event> = vec![];
        while matches!(events.first(), Some(Event::Touch { phase: TouchPhase::Start, .. })) {
            touches.push(events.remove(0));
        }
        let mut out = other;
        out.append(&mut touches);
        out.append(&mut ptr);
        out.append(&mut events);
        out
    }

    /// `(finger position, progress 0..1)` while a finger rests and the hold ring should be drawn.
    pub fn hold_ring(&self, now: f64) -> Option<(Pos2, f32)> {
        match self.mode {
            Mode::Pending if now - self.t0 > RING_AFTER => Some((self.start, (((now - self.t0 - RING_AFTER) / (LONG_PRESS - RING_AFTER)) as f32).clamp(0.0, 0.98))),
            Mode::Held => Some((self.start, 1.0)),
            _ => None,
        }
    }

    pub fn is_scrolling(&self) -> bool {
        matches!(self.mode, Mode::Scroll | Mode::Fling)
    }

    /// Rewrites `events` (the raw input of one frame). `now` is the frame time in seconds, `passthrough` disables recognition.
    pub fn process(&mut self, now: f64, passthrough: bool, events: Vec<Event>) -> (Vec<Event>, Outcome) {
        let dt = ((now - self.last_t) as f32).clamp(0.0, 0.1);
        self.last_t = now;
        let mut out: Vec<Event> = Vec::with_capacity(events.len() + 4);
        let mut outcome = Outcome::default();
        self.frame_delta = Vec2::ZERO;
        let events = Self::touch_first(events);

        for ev in events {
            match ev {
                Event::Touch { phase, pos, .. } => {
                    self.seen = true;
                    match phase {
                        TouchPhase::Start => {
                            self.fingers += 1;
                            let claimed = self.fingers == 1 && self.claims.iter().any(|r| r.expand(4.0).contains(pos));
                            if self.fingers >= 2 || passthrough || claimed {
                                out.append(&mut self.buf);
                                self.mode = Mode::Pass;
                            } else {
                                self.mode = Mode::Pending;
                                self.start = pos;
                                self.last = pos;
                                self.t0 = now;
                                self.buf.clear();
                                self.vel = Vec2::ZERO;
                                self.lock = Lock::Free;
                                self.drop_tail = false;
                            }
                        }
                        TouchPhase::Move => {
                            if self.fingers == 1 {
                                if self.mode == Mode::Held && (pos - self.start).length() > HOLD_SLOP {
                                    // hold-and-drag: deliver the held press (at the spot where the finger went down), then follow it
                                    out.append(&mut self.buf);
                                    out.push(Event::PointerMoved(pos));
                                    self.mode = Mode::Pass;
                                    outcome.long_press = Some(self.start);
                                }
                                if self.mode == Mode::Pending && (pos - self.start).length() > SLOP {
                                    self.mode = Mode::Scroll;
                                    self.buf.clear();
                                    let d = pos - self.start;
                                    self.lock = if d.y.abs() > 2.0 * d.x.abs() {
                                        Lock::Vertical
                                    } else if d.x.abs() > 2.0 * d.y.abs() {
                                        Lock::Horizontal
                                    } else {
                                        Lock::Free
                                    };
                                    if d.x.abs() > 1.6 * d.y.abs() {
                                        outcome.swipe = Some(if d.x > 0.0 && self.start.x < 28.0 {
                                            Swipe::EdgeRight
                                        } else if d.x < 0.0 {
                                            Swipe::Left
                                        } else {
                                            Swipe::Right
                                        });
                                    }
                                }
                                if self.mode == Mode::Scroll {
                                    self.frame_delta += pos - self.last;
                                    self.last = pos;
                                }
                            }
                        }
                        TouchPhase::End | TouchPhase::Cancel => {
                            self.fingers = self.fingers.saturating_sub(1);
                            match self.mode {
                                Mode::Pending if self.fingers == 0 => {
                                    if phase == TouchPhase::End {
                                        out.append(&mut self.buf);
                                    }
                                    self.buf.clear();
                                    self.mode = Mode::Idle;
                                }
                                Mode::Held if self.fingers == 0 => {
                                    // held and lifted without moving = right-click; the press is never delivered
                                    self.buf.clear();
                                    self.mode = Mode::Idle;
                                    out.push(Event::PointerGone);
                                    if phase == TouchPhase::End {
                                        outcome.context = Some(self.start);
                                    }
                                }
                                Mode::Scroll if self.fingers == 0 => {
                                    self.drop_tail = true;
                                    self.mode = if phase == TouchPhase::End && self.vel.length() > MIN_FLING { Mode::Fling } else { Mode::Idle };
                                    if self.mode == Mode::Idle {
                                        // the pointer-gone that follows is dropped; hover must end now
                                        out.push(Event::PointerGone);
                                    }
                                }
                                Mode::Pass if self.fingers == 0 => self.mode = Mode::Idle,
                                _ => {}
                            }
                        }
                    }
                    out.push(ev);
                }
                Event::PointerMoved(_) | Event::PointerButton { .. } | Event::PointerGone => match self.mode {
                    Mode::Pending | Mode::Held => self.buf.push(ev),
                    Mode::Scroll => {}
                    _ => {
                        let tail = matches!(ev, Event::PointerGone | Event::PointerButton { pressed: false, .. });
                        if self.drop_tail && tail {
                            if matches!(ev, Event::PointerGone) {
                                self.drop_tail = false;
                            }
                        } else {
                            out.push(ev);
                        }
                    }
                },
                other => out.push(other),
            }
        }

        // hold: still no press delivered — lifting now is a right-click, moving is a drag (see the Touch Move arm)
        if self.mode == Mode::Pending && self.fingers == 1 && now - self.t0 >= LONG_PRESS {
            self.mode = Mode::Held;
            outcome.held = Some(self.start);
        }

        // swipe: pointer position (so the scroll area under the finger gets it) + wheel
        match self.mode {
            Mode::Scroll => {
                let mut d = self.frame_delta;
                match self.lock {
                    Lock::Vertical => d.x = 0.0,
                    Lock::Horizontal => d.y = 0.0,
                    Lock::Free => {}
                }
                if dt > 0.0 {
                    let inst = d / dt;
                    self.vel = self.vel * 0.55 + inst * 0.45;
                }
                if d != Vec2::ZERO {
                    out.push(Event::PointerMoved(self.last));
                    out.push(Event::MouseWheel { unit: MouseWheelUnit::Point, delta: d, modifiers: Modifiers::NONE });
                }
                outcome.direct = true;
            }
            Mode::Fling => {
                let mut v = self.vel;
                match self.lock {
                    Lock::Vertical => v.x = 0.0,
                    Lock::Horizontal => v.y = 0.0,
                    Lock::Free => {}
                }
                let d = v * dt;
                self.vel *= (-FRICTION * dt).exp();
                if self.vel.length() < MIN_FLING * 0.18 {
                    self.mode = Mode::Idle;
                    out.push(Event::PointerGone);
                } else if d != Vec2::ZERO {
                    out.push(Event::PointerMoved(self.last));
                    out.push(Event::MouseWheel { unit: MouseWheelUnit::Point, delta: d, modifiers: Modifiers::NONE });
                }
                outcome.direct = true;
            }
            _ => {}
        }
        (out, outcome)
    }
}

thread_local! {
    static G: std::cell::RefCell<Gestures> = std::cell::RefCell::new(Gestures::default());
}

/// Entry point used by `App::raw_input_hook`.
pub fn on_raw_input(ctx: &Context, raw: &mut RawInput) {
    G.with(|g| hook(&mut g.borrow_mut(), ctx, raw));
}

const LP_ID: &str = "touch_long_press";
const DIRECT_ID: &str = "touch_direct";
const RING_ID: &str = "touch_hold_ring";

/// Paints the "hold" feedback (a ring that grows under the finger while it rests). Call at the end of every frame.
pub fn paint_hold(ctx: &Context) {
    let Some((p, t)) = ctx.data(|d| d.get_temp::<(Pos2, f32)>(Id::new(RING_ID))) else { return };
    let accent = egui::Color32::from_rgb(35, 131, 226);
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("touch_hold_ring_layer")));
    let ease = 1.0 - (1.0 - t) * (1.0 - t);
    let r = 12.0 + 20.0 * ease;
    painter.circle_filled(p, r, accent.gamma_multiply(0.10 + 0.14 * ease));
    painter.circle_stroke(p, r, egui::Stroke::new(if t >= 1.0 { 2.5 } else { 1.8 }, accent.gamma_multiply(0.35 + 0.65 * ease)));
}

/// Call from `App::raw_input_hook`.
pub fn hook(g: &mut Gestures, ctx: &Context, raw: &mut RawInput) {
    let now = raw.time.unwrap_or(0.0);
    // grab zones registered during the previous frame
    CLAIMS_PREV.with(|prev| {
        CLAIMS_NOW.with(|now| {
            let mut now = now.borrow_mut();
            let mut prev = prev.borrow_mut();
            std::mem::swap(&mut *prev, &mut *now);
            now.clear();
            g.claims.clear();
            g.claims.extend(prev.iter().copied());
        })
    });
    let had_touch = raw.events.iter().any(|e| matches!(e, Event::Touch { .. }));
    // Nothing to do (and no cost) for mouse-only use.
    if !had_touch && !g.seen && g.mode == Mode::Idle {
        return;
    }
    if had_touch {
        mark_touch(ctx, true);
        set_touchy(true);
    }
    let events = std::mem::take(&mut raw.events);
    let pass = PASS.with(|p| p.get());
    let (events, outcome) = g.process(now, pass, events);
    raw.events = events;
    if let Some(p) = outcome.long_press {
        ctx.data_mut(|d| d.insert_temp(Id::new(LP_ID), p));
        ctx.request_repaint();
    }
    if outcome.held.is_some() {
        crate::platform::haptic(ctx, 18);
        ctx.request_repaint();
    }
    if let Some(p) = outcome.context {
        crate::widgets::inject_rclick(ctx, p);
    }
    let ring = g.hold_ring(now);
    ctx.data_mut(|d| match ring {
        Some(r) => d.insert_temp(Id::new(RING_ID), r),
        None => {
            d.remove::<(Pos2, f32)>(Id::new(RING_ID));
        }
    });
    if let Some(sw) = outcome.swipe {
        SWIPE.with(|s| s.set(Some(sw)));
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(Id::new(DIRECT_ID), outcome.direct));
    match g.mode {
        Mode::Pending if now - g.t0 > RING_AFTER => ctx.request_repaint(),
        Mode::Pending => ctx.request_repaint_after(Duration::from_millis((RING_AFTER * 1000.0) as u64 + 5)),
        Mode::Held => ctx.request_repaint(),
        Mode::Fling | Mode::Scroll => ctx.request_repaint(),
        _ => {}
    }
}

/// A finger finished a long press at this position since the last call (consumes the event).
pub fn take_long_press(ctx: &Context) -> Option<Pos2> {
    ctx.data_mut(|d| {
        let v = d.get_temp::<Pos2>(Id::new(LP_ID));
        d.remove::<Pos2>(Id::new(LP_ID));
        v
    })
}

/// A finger is panning right now: scroll containers should follow it 1:1 instead of easing.
pub fn direct(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(Id::new(DIRECT_ID))).unwrap_or(false)
}

/// A touch screen has been used in this session.
pub fn is_touch(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(Id::new("touch_seen"))).unwrap_or(false)
}

pub fn mark_touch(ctx: &Context, seen: bool) {
    if seen && !is_touch(ctx) {
        ctx.data_mut(|d| d.insert_temp(Id::new("touch_seen"), true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, PointerButton, TouchDeviceId, TouchId};

    fn touch(phase: TouchPhase, p: Pos2) -> Event {
        Event::Touch { device_id: TouchDeviceId(0), id: TouchId(1), phase, pos: p, force: None }
    }
    fn press(p: Pos2, pressed: bool) -> Event {
        Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
    }
    fn has_wheel(v: &[Event]) -> Option<Vec2> {
        v.iter().find_map(|e| if let Event::MouseWheel { delta, .. } = e { Some(*delta) } else { None })
    }

    #[test]
    fn tap_is_replayed_unchanged() {
        let mut g = Gestures::default();
        let p = pos2(50.0, 60.0);
        let (o, _) = g.process(0.0, false, vec![touch(TouchPhase::Start, p), Event::PointerMoved(p), press(p, true)]);
        assert!(!o.iter().any(|e| matches!(e, Event::PointerButton { .. })), "press is held back until we know");
        let (o, _) = g.process(0.1, false, vec![touch(TouchPhase::End, p), press(p, false), Event::PointerGone]);
        let presses = o.iter().filter(|e| matches!(e, Event::PointerButton { .. })).count();
        assert_eq!(presses, 2, "press + release delivered: {o:?}");
    }

    #[test]
    fn swipe_becomes_wheel_and_flings() {
        let mut g = Gestures::default();
        let p = pos2(100.0, 400.0);
        g.process(0.0, false, vec![touch(TouchPhase::Start, p), Event::PointerMoved(p), press(p, true)]);
        let mut y = 400.0;
        let mut total = 0.0;
        let mut t = 0.0;
        for _ in 0..8 {
            t += 0.016;
            y -= 20.0;
            let q = pos2(100.0, y);
            let (o, oc) = g.process(t, false, vec![touch(TouchPhase::Move, q), Event::PointerMoved(q)]);
            assert!(!o.iter().any(|e| matches!(e, Event::PointerButton { .. })));
            assert!(oc.direct);
            if let Some(d) = has_wheel(&o) {
                assert!(d.y < 0.0 && d.x == 0.0, "finger up = content up, axis locked");
                total += d.y;
            }
        }
        assert!(total < -100.0, "moved {total}");
        let q = pos2(100.0, y);
        let (o, _) = g.process(t + 0.016, false, vec![touch(TouchPhase::End, q), press(q, false), Event::PointerGone]);
        assert!(!o.iter().any(|e| matches!(e, Event::PointerButton { .. } | Event::PointerGone)), "release is swallowed");
        assert!(g.is_scrolling(), "fling continues");
        let (o, _) = g.process(t + 0.032, false, vec![]);
        assert!(has_wheel(&o).map_or(false, |d| d.y < 0.0));
        let mut stopped = false;
        for i in 0..400 {
            let (o, _) = g.process(t + 0.05 + i as f64 * 0.016, false, vec![]);
            if o.iter().any(|e| matches!(e, Event::PointerGone)) {
                stopped = true;
                break;
            }
        }
        assert!(stopped && !g.is_scrolling());
    }

    #[test]
    fn hold_then_drag_starts_a_real_drag_at_the_press_point() {
        let mut g = Gestures::default();
        let p = pos2(10.0, 10.0);
        g.process(0.0, false, vec![touch(TouchPhase::Start, p), Event::PointerMoved(p), press(p, true)]);
        let (o, oc) = g.process(0.1, false, vec![]);
        assert!(oc.held.is_none() && o.is_empty());
        let (o, oc) = g.process(LONG_PRESS + 0.05, false, vec![]);
        assert_eq!(oc.held, Some(p));
        assert!(!o.iter().any(|e| matches!(e, Event::PointerButton { .. })), "press is still held back while the finger rests");
        let q = pos2(80.0, 90.0);
        let (o, oc) = g.process(LONG_PRESS + 0.1, false, vec![touch(TouchPhase::Move, q), Event::PointerMoved(q)]);
        assert_eq!(oc.long_press, Some(p));
        assert!(o.iter().any(|e| matches!(e, Event::PointerButton { pressed: true, pos, .. } if *pos == p)), "press delivered at the start point");
        assert!(has_wheel(&o).is_none() && o.iter().any(|e| matches!(e, Event::PointerMoved(_))));
    }

    #[test]
    fn hold_and_release_is_a_right_click() {
        let mut g = Gestures::default();
        let p = pos2(40.0, 50.0);
        g.process(0.0, false, vec![touch(TouchPhase::Start, p), Event::PointerMoved(p), press(p, true)]);
        g.process(LONG_PRESS + 0.05, false, vec![]);
        let (o, oc) = g.process(LONG_PRESS + 0.2, false, vec![press(p, false), Event::PointerGone, touch(TouchPhase::End, p)]);
        assert_eq!(oc.context, Some(p));
        assert!(!o.iter().any(|e| matches!(e, Event::PointerButton { .. })), "no primary click is produced: {o:?}");
        assert!(!g.is_scrolling());
    }

    #[test]
    fn a_swipe_never_becomes_a_drag_even_when_the_press_arrives_first() {
        // browsers report the pointer press *before* the touch start
        let mut g = Gestures::default();
        let p = pos2(100.0, 400.0);
        let (o, _) = g.process(0.0, false, vec![press(p, true), touch(TouchPhase::Start, p)]);
        assert!(!o.iter().any(|e| matches!(e, Event::PointerButton { .. })), "press must be held back: {o:?}");
        let q = pos2(100.0, 360.0);
        let (o, oc) = g.process(0.05, false, vec![Event::PointerMoved(q), touch(TouchPhase::Move, q)]);
        assert!(oc.direct && has_wheel(&o).is_some());
        assert!(!o.iter().any(|e| matches!(e, Event::PointerButton { .. })));
        let (o, _) = g.process(0.1, false, vec![press(q, false), Event::PointerGone, touch(TouchPhase::End, q)]);
        assert!(!o.iter().any(|e| matches!(e, Event::PointerButton { .. })), "no release without a press");
    }

    #[test]
    fn two_fingers_and_passthrough_are_untouched() {
        let mut g = Gestures::default();
        let p = pos2(10.0, 10.0);
        let (o, _) = g.process(0.0, true, vec![touch(TouchPhase::Start, p), Event::PointerMoved(p), press(p, true)]);
        assert!(o.iter().any(|e| matches!(e, Event::PointerButton { .. })), "pen tool receives the finger");
        let mut g = Gestures::default();
        g.process(0.0, false, vec![touch(TouchPhase::Start, p), press(p, true)]);
        let (o, _) = g.process(0.05, false, vec![Event::Touch { device_id: TouchDeviceId(0), id: TouchId(2), phase: TouchPhase::Start, pos: pos2(60.0, 60.0), force: None }]);
        assert!(o.iter().any(|e| matches!(e, Event::PointerButton { pressed: true, .. })));
    }
}

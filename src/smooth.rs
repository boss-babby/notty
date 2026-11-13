//! Physics scrolling.
//!
//! Wheel and touchpad events arrive in uneven chunks (a Windows precision touchpad sends bursts of tiny deltas, a mouse wheel sends
//! big steps). Feeding them straight into a scroll offset — or into a first-order ease like `x += (target - x) * k` — makes the
//! *velocity* jump on every event, which is exactly what looks "steppy" on Windows. Here the offset follows the target with a
//! **critically damped spring** (a stable closed-form step for any frame time): velocity is continuous, there is no overshoot,
//! and the motion is identical at 30, 60 or 144 Hz. While a finger is panning (see `touch.rs`) the offset follows it 1:1.

use egui::{Context, Id, ScrollArea, Ui};

/// Seconds the spring needs to (nearly) reach the target. Smaller = snappier.
const SMOOTH_TIME: f32 = 0.115;

#[derive(Clone, Copy, Default, Debug)]
pub struct Spring {
    pub pos: f32,
    pub target: f32,
    pub vel: f32,
    actual: f32,
    max: f32,
    active: bool,
    init: bool,
    /// Touchpad physics: velocity (points / s) estimated from the finger, time of the last touchpad event, "a touchpad gesture is running".
    fling: f32,
    last_in: f64,
    pad_mode: bool,
    /// Number of events of the running touchpad gesture (a fling is only synthesised after a real, multi-event swipe).
    n_in: u32,
}

/// Fastest synthetic touchpad fling (points / s) and a wheel notch cap (points per frame): a defence against devices that report huge deltas.
const MAX_FLING: f32 = 2600.0;
const MAX_NOTCH: f32 = 420.0;
/// Browsers (Chrome / Safari / Firefox) generate the inertia of a touchpad swipe themselves (as more wheel events), so the web build never adds one.
const SYNTH_FLING: bool = !cfg!(target_arch = "wasm32");

/// One step of a critically damped spring towards `target` (Game Programming Gems 4, "SmoothDamp").
pub fn damp(pos: f32, vel: f32, target: f32, dt: f32) -> (f32, f32) {
    let w = 2.0 / SMOOTH_TIME;
    let x = w * dt;
    let e = 1.0 / (1.0 + x + 0.48 * x * x + 0.235 * x * x * x);
    let delta = pos - target;
    let tmp = (vel + w * delta) * dt;
    let nv = (vel - w * tmp) * e;
    let np = target + (delta + tmp) * e;
    (np, nv)
}

impl Spring {
    /// Call before the scroll area is shown. `wheel` is the vertical wheel delta (points, positive = content moves down).
    /// Returns the offset to force on the scroll area this frame, or `None` when it should scroll by itself.
    pub fn update(&mut self, ctx: &Context, wheel: f32, direct: bool) -> Option<f32> {
        if !self.init {
            self.init = true;
            self.max = f32::MAX;
        }
        if (self.actual - self.pos).abs() > 1.5 {
            // something else moved the view (scrollbar drag, scroll_to, content change)
            self.pos = self.actual;
            self.target = self.actual;
            self.vel = 0.0;
            self.fling = 0.0;
            self.pad_mode = false;
        }
        let now = ctx.input(|i| i.time);
        let dt = ctx.input(|i| i.unstable_dt).clamp(0.002, 0.05);
        let max = self.max.max(0.0);
        let pad = wheel != 0.0 && ctx.data(|d| d.get_temp::<bool>(Id::new("wheel_is_pad"))).unwrap_or(false);
        let mut gliding = false;
        if wheel != 0.0 {
            if direct {
                self.pos = (self.pos - wheel).clamp(0.0, max);
                self.target = self.pos;
                self.vel = 0.0;
                self.pad_mode = false;
            } else if pad {
                // Touchpad: the content follows the fingers 1:1 (no smoothing = no latency) while the finger velocity is tracked.
                self.pos = (self.pos - wheel).clamp(0.0, max);
                self.target = self.pos;
                self.vel = 0.0;
                let inst = (-wheel / dt).clamp(-MAX_FLING, MAX_FLING);
                if now - self.last_in > 0.12 || (self.fling != 0.0 && self.fling.signum() != inst.signum()) {
                    self.n_in = 0;
                    self.fling = 0.0;
                }
                self.n_in += 1;
                self.fling = if self.fling != 0.0 { self.fling * 0.55 + inst * 0.45 } else { inst };
                self.last_in = now;
                self.pad_mode = true;
            } else {
                // Mouse wheel notch: critically damped spring towards the new target.
                self.target = (self.target - wheel.clamp(-MAX_NOTCH, MAX_NOTCH)).clamp(0.0, max);
                self.pad_mode = false;
                self.fling = 0.0;
                self.n_in = 0;
            }
        } else if self.pad_mode && !direct {
            let idle = now - self.last_in;
            if idle <= 0.045 {
                // between two touchpad events of the same gesture: hold position
                gliding = true;
                ctx.request_repaint();
            } else if SYNTH_FLING && self.n_in >= 4 && self.fling.abs() > 80.0 {
                // The fingers lifted while still moving: keep gliding and slow down with the friction of a real touchpad fling
                // (the OS sends its own inertia on Windows / macOS — then the measured velocity is already ~0 and nothing is added).
                self.pos = (self.pos + self.fling * dt).clamp(0.0, max);
                self.target = self.pos;
                self.fling *= (-dt / 0.32).exp();
                if self.pos <= 0.0 || self.pos >= max {
                    self.fling = 0.0;
                }
                gliding = true;
                ctx.request_repaint();
            } else {
                self.pad_mode = false;
                self.fling = 0.0;
            }
        }
        let moving = !direct && !self.pad_mode && ((self.target - self.pos).abs() > 0.25 || self.vel.abs() > 2.0);
        if moving {
            let (p, v) = damp(self.pos, self.vel, self.target, dt);
            self.pos = p;
            self.vel = v;
            if (self.target - self.pos).abs() <= 0.25 && self.vel.abs() <= 2.0 {
                self.pos = self.target;
                self.vel = 0.0;
            } else {
                ctx.request_repaint();
            }
        }
        self.active = wheel != 0.0 || moving || gliding;
        if self.active {
            Some(self.pos)
        } else {
            None
        }
    }

    /// Call after the scroll area was shown with the offset it reports and its maximum offset.
    pub fn observe(&mut self, actual: f32, max: f32) {
        self.actual = actual;
        self.max = max;
        if !self.active {
            self.pos = actual;
            self.target = actual;
            self.vel = 0.0;
        } else {
            self.target = self.target.clamp(0.0, max.max(0.0));
        }
    }
}

/// Takes the vertical wheel / touchpad scroll of this frame away from egui (so the area does not scroll twice).
pub fn take_wheel(ctx: &Context, over: bool) -> f32 {
    if !over || ctx.input(|i| i.modifiers.command) {
        return 0.0;
    }
    let now = ctx.input(|i| i.time);
    let in_gesture = ctx.data(|d| d.get_temp::<f64>(Id::new("wheel_pad_until"))).map_or(false, |t| now < t);
    let (d, pad) = ctx.input_mut(|i| {
        let d = i.raw_scroll_delta.y;
        // Touchpad: small pixel deltas (or fractional lines). A mouse wheel notch is one BIG pixel delta (Chrome / Edge / Windows: 100-120,
        // scaled by display zoom) or a whole number of lines (Firefox / native) — it must never be mistaken for a swipe: treating one
        // 100 px notch as "finger velocity" used to fling the page ~2000 px (the "tiny scroll, huge jump" bug).
        let mut pad = false;
        let mut big = false;
        for e in &i.events {
            if let egui::Event::MouseWheel { unit, delta, .. } = e {
                match unit {
                    egui::MouseWheelUnit::Point => {
                        if delta.y.abs() < 40.0 {
                            pad = true;
                        } else {
                            big = true;
                        }
                    }
                    egui::MouseWheelUnit::Line => {
                        if delta.y.fract() != 0.0 {
                            pad = true;
                        }
                    }
                    egui::MouseWheelUnit::Page => {}
                }
            }
        }
        // big deltas inside a running touchpad gesture are still the touchpad (a fast flick)
        let pad = pad || (big && in_gesture);
        if d != 0.0 {
            i.raw_scroll_delta.y = 0.0;
            i.smooth_scroll_delta.y = 0.0;
        }
        (d, pad)
    });
    if d != 0.0 {
        ctx.data_mut(|dm| {
            dm.insert_temp(Id::new("wheel_is_pad"), pad);
            if pad {
                dm.insert_temp(Id::new("wheel_pad_until"), now + 0.18);
            }
        });
    }
    d
}

/// A vertical scroll area with spring physics (wheel / touchpad) and 1:1 finger panning.
pub fn scroll_area<R>(ui: &mut Ui, key: Id, area: ScrollArea, over: bool, add: impl FnOnce(&mut Ui) -> R) -> egui::scroll_area::ScrollAreaOutput<R> {
    let ctx = ui.ctx().clone();
    let mut sp: Spring = ctx.data(|d| d.get_temp(key)).unwrap_or_default();
    let wheel = take_wheel(&ctx, over);
    let direct = crate::touch::direct(&ctx);
    let force = sp.update(&ctx, wheel, direct);
    let mut area = area;
    if let Some(o) = force {
        area = area.vertical_scroll_offset(o);
    }
    let out = area.show(ui, add);
    let max = (out.content_size.y - out.inner_rect.height()).max(0.0);
    sp.observe(out.state.offset.y, max);
    ctx.data_mut(|d| d.insert_temp(key, sp));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_converges_without_overshoot_at_any_frame_rate() {
        for fps in [30.0_f32, 60.0, 144.0, 240.0] {
            let dt = 1.0 / fps;
            let (mut p, mut v) = (0.0_f32, 0.0_f32);
            let mut max_p = 0.0_f32;
            for _ in 0..(fps as usize * 2) {
                let (np, nv) = damp(p, v, 500.0, dt);
                p = np;
                v = nv;
                max_p = max_p.max(p);
            }
            assert!((p - 500.0).abs() < 0.5, "{fps} fps: {p}");
            assert!(max_p <= 500.5, "no overshoot at {fps} fps: {max_p}");
        }
    }

    #[test]
    fn velocity_is_continuous_when_the_target_jumps() {
        let (mut p, mut v) = (0.0_f32, 0.0_f32);
        let mut target = 100.0;
        let mut worst_jump = 0.0_f32;
        let mut prev = 0.0;
        for i in 0..90 {
            if i % 7 == 0 {
                target += 40.0;
            }
            let (np, nv) = damp(p, v, target, 1.0 / 60.0);
            worst_jump = worst_jump.max(((np - p) - prev).abs());
            prev = np - p;
            p = np;
            v = nv;
        }
        assert!(worst_jump < 8.0, "per-frame step changed by {worst_jump}px at most");
    }

    fn classify(unit: egui::MouseWheelUnit, dy: f32) -> (f32, bool) {
        let ctx = Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::MouseWheel { unit, delta: egui::vec2(0.0, dy), modifiers: Default::default() });
        let mut out = (0.0, false);
        let _ = ctx.run(raw, |ctx| {
            let d = take_wheel(ctx, true);
            out = (d, ctx.data(|dm| dm.get_temp::<bool>(Id::new("wheel_is_pad"))).unwrap_or(false));
        });
        out
    }

    #[test]
    fn one_mouse_wheel_notch_is_never_a_touchpad_swipe() {
        // Chrome / Edge: one notch = one pixel event of ~100; Firefox / native: whole lines
        for dy in [-100.0, -120.0, -125.0, -66.7] {
            let (d, pad) = classify(egui::MouseWheelUnit::Point, dy);
            assert!(d != 0.0 && !pad, "pixel notch {dy} was classified as a touchpad");
        }
        let (_, pad) = classify(egui::MouseWheelUnit::Line, -3.0);
        assert!(!pad);
        // a touchpad reports small pixel deltas
        let (_, pad) = classify(egui::MouseWheelUnit::Point, -7.0);
        assert!(pad);
    }
}

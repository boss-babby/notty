//! True circles. egui (and everything built on polylines) only knows straight segments, so a "circle" is a polygon — and a polygon with a
//! fixed number of corners shows its straight edges as soon as it is large on screen (a big screen, a 3× phone, a zoomed whiteboard card).
//! The number of segments therefore has to follow the size of the circle **in physical pixels**:
//!
//! ```text
//! sagitta s = R · (1 − cos(Δ/2))   →   Δ = 2 · acos(1 − s / R)       (s = largest allowed gap between chord and arc)
//! ```
//!
//! With `s` = 0.06 px the polygon deviates from the ideal circle by far less than one pixel, i.e. it is not distinguishable from a
//! true circle. `R` = logical radius × `pixels_per_point` × the zoom of the layer that the shape is painted in (the whiteboard paints
//! widget cards at 100 % and zooms them afterwards, see `canvasdash`). Edges are additionally feathered (anti-aliased) by the callers.

use std::cell::Cell;

/// Largest distance between a chord and the arc, in physical pixels.
const SAGITTA_PX: f32 = 0.06;

thread_local! {
    static PPP: Cell<f32> = const { Cell::new(1.0) };
    static ZOOM: Cell<f32> = const { Cell::new(1.0) };
}

/// Called once per frame with `ctx.pixels_per_point()`.
pub fn set_ppp(ppp: f32) {
    PPP.with(|p| p.set(ppp.clamp(0.5, 8.0)));
}

/// Zoom of the layer the following shapes are painted in (1.0 = painted at their final size).
pub fn set_zoom(z: f32) {
    ZOOM.with(|p| p.set(if z.is_finite() && z > 0.0 { z.clamp(0.01, 64.0) } else { 1.0 }));
}

pub fn zoom() -> f32 {
    ZOOM.with(|z| z.get())
}

/// Physical pixels per logical unit of the shape that is being painted right now.
pub fn px_scale() -> f32 {
    PPP.with(|p| p.get()) * ZOOM.with(|z| z.get())
}

/// Number of chords needed to approximate an arc of `sweep` radians and logical radius `radius` as a true circular arc.
pub fn arc_steps(radius: f32, sweep: f32) -> usize {
    arc_steps_at(radius, sweep, px_scale())
}

pub fn arc_steps_at(radius: f32, sweep: f32, px_per_unit: f32) -> usize {
    let r = (radius.abs() * px_per_unit).max(0.5);
    let step = 2.0 * (1.0 - (SAGITTA_PX / r).min(0.5)).acos();
    ((sweep.abs() / step).ceil() as usize).clamp(4, 2048)
}

/// Width of the anti-aliasing fringe in logical units (one physical pixel).
pub fn feather() -> f32 {
    1.0 / px_scale().max(0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn more_pixels_means_more_segments_and_never_a_visible_polygon() {
        let tau = std::f32::consts::TAU;
        let small = arc_steps_at(20.0, tau, 1.0);
        let big = arc_steps_at(20.0, tau, 4.0);
        assert!(big > small);
        for (r, k) in [(10.0_f32, 1.0_f32), (60.0, 3.0), (300.0, 2.0), (30.0, 12.0)] {
            let n = arc_steps_at(r, tau, k) as f32;
            let sag = r * k * (1.0 - (tau / n / 2.0).cos());
            assert!(sag <= 0.08, "radius {r} at {k}x deviates {sag}px");
        }
    }
}

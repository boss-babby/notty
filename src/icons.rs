//! Vector icons painted straight with egui shapes — no images, no icon fonts.

use egui::{pos2, vec2, Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum Icon {
    Menu,
    Plus,
    Dots,
    Search,
    Sun,
    Moon,
    Lock,
    Trash,
    Star,
    StarFilled,
    Close,
    ChevronLeft,
    ChevronRight,
    Page,
    Board,
    Grip,
    Duplicate,
    Restore,
    Shield,
}

fn pts(c: Pos2, k: f32, v: &[(f32, f32)]) -> Vec<Pos2> {
    v.iter().map(|(x, y)| pos2(c.x + x * k, c.y + y * k)).collect()
}

/// Draws a rotated chevron (`angle` = 0 points right, PI/2 points down).
pub fn chevron(p: &Painter, c: Pos2, size: f32, angle: f32, color: Color32) {
    let k = size / 16.0;
    let (s, co) = angle.sin_cos();
    let rot = |x: f32, y: f32| pos2(c.x + (x * co - y * s) * k, c.y + (x * s + y * co) * k);
    let line = vec![rot(-2.5, -4.5), rot(2.5, 0.0), rot(-2.5, 4.5)];
    p.add(Shape::line(line, Stroke::new(1.6_f32, color)));
}

pub fn paint(p: &Painter, c: Pos2, size: f32, icon: Icon, color: Color32) {
    let k = size / 16.0;
    let st = Stroke::new(1.5_f32, color);
    match icon {
        Icon::Menu => {
            for y in [-4.5f32, 0.0, 4.5] {
                p.line_segment([pos2(c.x - 6.5 * k, c.y + y * k), pos2(c.x + 6.5 * k, c.y + y * k)], st);
            }
        }
        Icon::Plus => {
            p.line_segment([pos2(c.x - 6.0 * k, c.y), pos2(c.x + 6.0 * k, c.y)], st);
            p.line_segment([pos2(c.x, c.y - 6.0 * k), pos2(c.x, c.y + 6.0 * k)], st);
        }
        Icon::Dots => {
            for x in [-5.0f32, 0.0, 5.0] {
                p.circle_filled(pos2(c.x + x * k, c.y), 1.4 * k.max(0.8), color);
            }
        }
        Icon::Search => {
            p.circle_stroke(pos2(c.x - 1.5 * k, c.y - 1.5 * k), 5.0 * k, st);
            p.line_segment([pos2(c.x + 2.2 * k, c.y + 2.2 * k), pos2(c.x + 6.5 * k, c.y + 6.5 * k)], st);
        }
        Icon::Sun => {
            p.circle_stroke(c, 3.3 * k, st);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let (s, co) = a.sin_cos();
                p.line_segment(
                    [pos2(c.x + co * 5.6 * k, c.y + s * 5.6 * k), pos2(c.x + co * 7.4 * k, c.y + s * 7.4 * k)],
                    st,
                );
            }
        }
        Icon::Moon => {
            let mut v: Vec<Pos2> = Vec::new();
            let r = 6.2 * k;
            let mut a = 13.0f32;
            while a <= 257.0 {
                let rad = a.to_radians();
                v.push(pos2(c.x + r * rad.cos(), c.y + r * rad.sin()));
                a += 8.0;
            }
            let b = pos2(c.x + 3.0 * k, c.y - 3.0 * k);
            let r2 = 5.2 * k;
            let mut a = 213.0f32;
            while a >= 57.0 {
                let rad = a.to_radians();
                v.push(pos2(b.x + r2 * rad.cos(), b.y + r2 * rad.sin()));
                a -= 8.0;
            }
            p.add(Shape::closed_line(v, st));
        }
        Icon::Lock => {
            let body = Rect::from_center_size(pos2(c.x, c.y + 2.5 * k), vec2(11.0 * k, 8.0 * k));
            p.rect_stroke(body, 2.0, st, StrokeKind::Inside);
            let mut v = Vec::new();
            let mut a = 180.0f32;
            while a <= 360.0 {
                let rad = a.to_radians();
                v.push(pos2(c.x + 3.4 * k * rad.cos(), c.y - 1.8 * k + 3.8 * k * rad.sin()));
                a += 15.0;
            }
            p.add(Shape::line(v, st));
            p.circle_filled(pos2(c.x, c.y + 2.5 * k), 1.1 * k, color);
        }
        Icon::Trash => {
            p.line_segment([pos2(c.x - 6.0 * k, c.y - 4.2 * k), pos2(c.x + 6.0 * k, c.y - 4.2 * k)], st);
            p.line_segment([pos2(c.x - 2.0 * k, c.y - 6.2 * k), pos2(c.x + 2.0 * k, c.y - 6.2 * k)], st);
            p.add(Shape::line(pts(c, k, &[(-4.6, -2.6), (-4.0, 6.4), (4.0, 6.4), (4.6, -2.6)]), st));
            p.line_segment([pos2(c.x - 1.6 * k, c.y - 0.6 * k), pos2(c.x - 1.6 * k, c.y + 4.0 * k)], st);
            p.line_segment([pos2(c.x + 1.6 * k, c.y - 0.6 * k), pos2(c.x + 1.6 * k, c.y + 4.0 * k)], st);
        }
        Icon::Star | Icon::StarFilled => {
            let mut v = Vec::new();
            for i in 0..10 {
                let r = if i % 2 == 0 { 7.0 } else { 3.0 } * k;
                let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
                v.push(pos2(c.x + r * a.cos(), c.y + 0.5 * k + r * a.sin()));
            }
            if icon == Icon::StarFilled {
                p.add(Shape::convex_polygon(v, color, st));
            } else {
                p.add(Shape::closed_line(v, st));
            }
        }
        Icon::Close => {
            p.line_segment([pos2(c.x - 4.5 * k, c.y - 4.5 * k), pos2(c.x + 4.5 * k, c.y + 4.5 * k)], st);
            p.line_segment([pos2(c.x - 4.5 * k, c.y + 4.5 * k), pos2(c.x + 4.5 * k, c.y - 4.5 * k)], st);
        }
        Icon::ChevronLeft => chevron(p, c, size, std::f32::consts::PI, color),
        Icon::ChevronRight => chevron(p, c, size, 0.0, color),
        Icon::Page => {
            let r = Rect::from_center_size(c, vec2(11.0 * k, 14.0 * k));
            p.rect_stroke(r, 2.0, st, StrokeKind::Inside);
            for y in [-2.5f32, 0.5, 3.5] {
                p.line_segment([pos2(c.x - 3.0 * k, c.y + y * k), pos2(c.x + 3.0 * k, c.y + y * k)], Stroke::new(1.2_f32, color));
            }
        }
        Icon::Board => {
            for (i, h) in [11.0f32, 7.0, 9.0].iter().enumerate() {
                let x = c.x + (i as f32 - 1.0) * 5.0 * k;
                let r = Rect::from_min_size(pos2(x - 1.8 * k, c.y - 6.0 * k), vec2(3.6 * k, h * k));
                p.rect_stroke(r, 1.0, Stroke::new(1.3_f32, color), StrokeKind::Inside);
            }
        }
        Icon::Grip => {
            for x in [-2.2f32, 2.2] {
                for y in [-4.5f32, 0.0, 4.5] {
                    p.circle_filled(pos2(c.x + x * k, c.y + y * k), 1.15 * k, color);
                }
            }
        }
        Icon::Duplicate => {
            let a = Rect::from_center_size(pos2(c.x + 1.5 * k, c.y + 1.5 * k), vec2(9.0 * k, 9.0 * k));
            let b = Rect::from_center_size(pos2(c.x - 1.5 * k, c.y - 1.5 * k), vec2(9.0 * k, 9.0 * k));
            p.rect_stroke(a, 2.0, st, StrokeKind::Inside);
            p.rect_stroke(b, 2.0, st, StrokeKind::Inside);
        }
        Icon::Restore => {
            let mut v = Vec::new();
            let mut a = -60.0f32;
            while a <= 220.0 {
                let rad = a.to_radians();
                v.push(pos2(c.x + 5.5 * k * rad.cos(), c.y + 5.5 * k * rad.sin()));
                a += 12.0;
            }
            p.add(Shape::line(v, st));
            p.add(Shape::line(pts(c, k, &[(3.6, -6.8), (2.8, -3.2), (6.4, -2.6)]), st));
        }
        Icon::Shield => {
            let v = pts(c, k, &[(0.0, -7.0), (6.0, -4.5), (5.4, 2.0), (0.0, 7.2), (-5.4, 2.0), (-6.0, -4.5)]);
            p.add(Shape::closed_line(v, st));
            p.add(Shape::line(pts(c, k, &[(-2.4, 0.2), (-0.6, 2.2), (2.8, -1.8)]), st));
        }
    }
}

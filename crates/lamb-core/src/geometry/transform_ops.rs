//! Point-level transforms shared by MOVE/COPY/ROTATE/MIRROR/SCALE/ARRAY. Port of
//! `geometry-primitives/src/transformOps.ts`.

use super::point::{Point2, add2, dot2, normalize2, sub2};

pub fn translate_point(p: Point2, dx: f64, dy: f64) -> Point2 {
    Point2 { x: p.x + dx, y: p.y + dy }
}

pub fn rotate_point_about(p: Point2, center: Point2, angle_rad: f64) -> Point2 {
    let (sin, cos) = angle_rad.sin_cos();
    let dx = p.x - center.x;
    let dy = p.y - center.y;
    Point2 { x: center.x + dx * cos - dy * sin, y: center.y + dx * sin + dy * cos }
}

pub fn scale_point_about(p: Point2, center: Point2, factor: f64) -> Point2 {
    Point2 { x: center.x + (p.x - center.x) * factor, y: center.y + (p.y - center.y) * factor }
}

/// Reflects `p` across the infinite line through (`line_a`, `line_b`).
pub fn mirror_point_across_line(p: Point2, line_a: Point2, line_b: Point2) -> Point2 {
    let dir = normalize2(sub2(line_b, line_a));
    let proj = dot2(sub2(p, line_a), dir);
    let closest = add2(line_a, Point2 { x: dir.x * proj, y: dir.y * proj });
    Point2 { x: 2.0 * closest.x - p.x, y: 2.0 * closest.y - p.y }
}

//! Simple-polygon triangulation (ear clipping, no holes) and the loop predicates that go with it.
//! Port of `geometry-primitives/src/triangulate.ts`.
//!
//! Collinearity is judged as a perpendicular DISTANCE (mm), never a raw cross-product magnitude.

use super::point::Point2;
use super::tolerance::TOLERANCE;

/// Positive when the loop winds counter-clockwise. First vertex is not repeated.
pub fn polygon_area_signed(lp: &[Point2]) -> f64 {
    let n = lp.len();
    let mut sum = 0.0;
    for i in 0..n {
        let a = lp[i];
        let b = lp[(i + 1) % n];
        sum += a.x * b.y - b.x * a.y;
    }
    sum / 2.0
}

/// Perpendicular distance of `c` from the infinite line a->b, signed by side.
fn side_distance(a: Point2, b: Point2, c: Point2) -> f64 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = dx.hypot(dy);
    if len == 0.0 {
        return 0.0;
    }
    (dx * (c.y - a.y) - dy * (c.x - a.x)) / len
}

/// Every turn the same way (collinear vertices ignored). Fewer than three vertices, or all
/// collinear, is NOT convex.
pub fn is_convex_loop(lp: &[Point2]) -> bool {
    let n = lp.len();
    if n < 3 {
        return false;
    }
    let mut sign = 0.0;
    for i in 0..n {
        let d = side_distance(lp[i], lp[(i + 1) % n], lp[(i + 2) % n]);
        if d.abs() <= TOLERANCE.coincidence_mm {
            continue;
        }
        let s = if d > 0.0 { 1.0 } else { -1.0 };
        if sign == 0.0 {
            sign = s;
        } else if s != sign {
            return false;
        }
    }
    sign != 0.0
}

/// Inside (or on an edge of) a CCW triangle.
fn point_in_triangle(p: Point2, a: Point2, b: Point2, c: Point2) -> bool {
    let eps = TOLERANCE.coincidence_mm;
    side_distance(a, b, p) >= -eps && side_distance(b, c, p) >= -eps && side_distance(c, a, p) >= -eps
}

/// Ear clipping over a simple polygon, no holes. Triangles are index triples into `lp`.
pub fn triangulate_loop(lp: &[Point2]) -> Vec<[usize; 3]> {
    let n = lp.len();
    if n < 3 {
        return Vec::new();
    }
    if n == 3 {
        return vec![[0, 1, 2]];
    }

    // Work CCW so "convex vertex" is a single sign test; indices stay original.
    let mut remaining: Vec<usize> = if polygon_area_signed(lp) >= 0.0 { (0..n).collect() } else { (0..n).rev().collect() };

    let mut tris = Vec::new();
    let mut guard = 0;
    while remaining.len() > 3 && guard < n * n {
        guard += 1;
        let m = remaining.len();
        let mut clipped = false;
        for i in 0..m {
            let i_prev = remaining[(i + m - 1) % m];
            let i_cur = remaining[i];
            let i_next = remaining[(i + 1) % m];
            let (a, b, c) = (lp[i_prev], lp[i_cur], lp[i_next]);

            if side_distance(a, b, c) <= TOLERANCE.coincidence_mm {
                continue; // reflex or collinear
            }
            let blocked = remaining.iter().any(|&idx| idx != i_prev && idx != i_cur && idx != i_next && point_in_triangle(lp[idx], a, b, c));
            if blocked {
                continue;
            }
            tris.push([i_prev, i_cur, i_next]);
            remaining.remove(i);
            clipped = true;
            break;
        }
        if !clipped {
            // No ear: self-intersecting or degenerate input. Clip a vertex anyway so this always
            // terminates.
            tris.push([remaining[m - 1], remaining[0], remaining[1]]);
            remaining.remove(0);
        }
    }

    if let [a, b, c] = remaining[..] {
        tris.push([a, b, c]);
    }
    tris
}

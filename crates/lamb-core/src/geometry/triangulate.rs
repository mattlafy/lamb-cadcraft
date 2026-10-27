//! Simple-polygon triangulation (ear clipping) and the loop predicates that go with it. Port of
//! `geometry-primitives/src/triangulate.ts`, plus holes: [`bridge_holes`] joins each hole to the
//! outline by a bridge into one keyhole loop that [`triangulate_loop`] clips like any other.
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

fn coincident(a: Point2, b: Point2) -> bool {
    (a.x - b.x).hypot(a.y - b.y) <= TOLERANCE.coincidence_mm
}

/// Even–odd containment of `p` in the closed loop `poly` (first vertex not repeated).
pub fn point_in_loop(poly: &[Point2], p: Point2) -> bool {
    let n = poly.len();
    let mut inside = false;
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a.y > p.y) != (b.y > p.y) {
            let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
            if p.x < x {
                inside = !inside;
            }
        }
    }
    inside
}

/// Ear clipping over a simple polygon, or a keyhole loop from [`bridge_holes`]: a vertex
/// coincident with one of the ear's own corners (the far side of a bridge) never blocks it.
/// Triangles are index triples into `lp`.
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
            let blocked = remaining.iter().any(|&idx| {
                let p = lp[idx];
                idx != i_prev
                    && idx != i_cur
                    && idx != i_next
                    && !coincident(p, a)
                    && !coincident(p, b)
                    && !coincident(p, c)
                    && point_in_triangle(p, a, b, c)
            });
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

/// Do segments p1-p2 and q1-q2 cross at a point interior to both? Touching at an end is not.
fn segments_cross(p1: Point2, p2: Point2, q1: Point2, q2: Point2) -> bool {
    let eps = TOLERANCE.coincidence_mm;
    let d1 = side_distance(q1, q2, p1);
    let d2 = side_distance(q1, q2, p2);
    let d3 = side_distance(p1, p2, q1);
    let d4 = side_distance(p1, p2, q2);
    ((d1 > eps && d2 < -eps) || (d1 < -eps && d2 > eps)) && ((d3 > eps && d4 < -eps) || (d3 < -eps && d4 > eps))
}

/// Does direction v -> m leave CCW vertex v (between `prev` and `next`) into the polygon?
fn in_cone(prev: Point2, v: Point2, next: Point2, m: Point2) -> bool {
    let left = |a: Point2, b: Point2, c: Point2| side_distance(a, b, c) > 0.0;
    let left_on = |a: Point2, b: Point2, c: Point2| side_distance(a, b, c) >= 0.0;
    if left_on(v, next, prev) { left(v, m, prev) && left(m, v, next) } else { !(left_on(v, m, next) && left_on(m, v, prev)) }
}

/// One loop from an outline and the holes inside it: the outline made CCW, each hole made CW and
/// spliced in by a bridge from its max-x vertex to the nearest outline vertex that sees it
/// (outline -> bridge -> hole -> back along the bridge). First vertex not repeated; the bridge
/// ends appear twice. Holes are assumed to lie inside `outer` and not to cross each other.
pub fn bridge_holes(outer: &[Point2], holes: &[Vec<Point2>]) -> Vec<Point2> {
    let mut merged: Vec<Point2> = outer.to_vec();
    if polygon_area_signed(&merged) < 0.0 {
        merged.reverse();
    }
    let mut pending: Vec<Vec<Point2>> = holes
        .iter()
        .filter(|h| h.len() >= 3)
        .map(|h| {
            let mut h = h.clone();
            if polygon_area_signed(&h) > 0.0 {
                h.reverse();
            }
            h
        })
        .collect();
    let max_x = |h: &Vec<Point2>| h.iter().fold(f64::NEG_INFINITY, |m, p| m.max(p.x));
    // Rightmost hole first, so no later bridge has to cross an earlier hole.
    pending.sort_by(|a, b| max_x(b).total_cmp(&max_x(a)));

    while !pending.is_empty() {
        let hole = pending.remove(0);
        let Some(mi) = (0..hole.len()).max_by(|&a, &b| hole[a].x.total_cmp(&hole[b].x)) else { continue };
        let m = hole[mi];
        let n = merged.len();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| {
            let da = (merged[a].x - m.x).hypot(merged[a].y - m.y);
            let db = (merged[b].x - m.x).hypot(merged[b].y - m.y);
            da.total_cmp(&db)
        });
        let edges_of = |lp: &[Point2]| (0..lp.len()).map(move |i| (lp[i], lp[(i + 1) % lp.len()])).collect::<Vec<_>>();
        let mut blockers = edges_of(&merged);
        blockers.extend(edges_of(&hole));
        for h in &pending {
            blockers.extend(edges_of(h));
        }
        let visible = |vi: usize| {
            let v = merged[vi];
            if !in_cone(merged[(vi + n - 1) % n], v, merged[(vi + 1) % n], m) {
                return false;
            }
            !blockers
                .iter()
                .any(|&(a, b)| !coincident(a, v) && !coincident(b, v) && !coincident(a, m) && !coincident(b, m) && segments_cross(v, m, a, b))
        };
        let vi = order.iter().copied().find(|&vi| visible(vi)).unwrap_or(order[0]);

        let v = merged[vi];
        let mut spliced = Vec::with_capacity(n + hole.len() + 2);
        spliced.extend_from_slice(&merged[..=vi]);
        spliced.extend((0..hole.len()).map(|k| hole[(mi + k) % hole.len()]));
        spliced.push(m);
        spliced.push(v);
        spliced.extend_from_slice(&merged[vi + 1..]);
        merged = spliced;
    }
    merged
}

#[cfg(test)]
mod hole_tests {
    use super::*;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Point2> {
        vec![Point2 { x: x0, y: y0 }, Point2 { x: x1, y: y0 }, Point2 { x: x1, y: y1 }, Point2 { x: x0, y: y1 }]
    }

    fn tri_area(lp: &[Point2]) -> f64 {
        triangulate_loop(lp).iter().map(|&[a, b, c]| polygon_area_signed(&[lp[a], lp[b], lp[c]]).abs()).sum()
    }

    #[test]
    fn a_bridged_ring_triangulates_to_exactly_the_ring_area() {
        let lp = bridge_holes(&rect(0.0, 0.0, 100.0, 100.0), &[rect(20.0, 20.0, 80.0, 80.0)]);
        assert_eq!(lp.len(), 4 + 4 + 2);
        assert!((tri_area(&lp) - (100.0 * 100.0 - 60.0 * 60.0)).abs() < 1e-6, "area {}", tri_area(&lp));
        assert!((polygon_area_signed(&lp) - (100.0 * 100.0 - 60.0 * 60.0)).abs() < 1e-6);
    }

    #[test]
    fn two_holes_and_a_clockwise_outline() {
        let mut outer = rect(0.0, 0.0, 300.0, 100.0);
        outer.reverse();
        let mut h2 = rect(160.0, 10.0, 290.0, 90.0);
        h2.reverse();
        let lp = bridge_holes(&outer, &[rect(10.0, 10.0, 140.0, 90.0), h2]);
        let want = 300.0 * 100.0 - 2.0 * 130.0 * 80.0;
        assert!((tri_area(&lp) - want).abs() < 1e-6, "area {}", tri_area(&lp));
    }

    #[test]
    fn point_in_loop_is_even_odd() {
        let sq = rect(0.0, 0.0, 10.0, 10.0);
        assert!(point_in_loop(&sq, Point2 { x: 5.0, y: 5.0 }));
        assert!(!point_in_loop(&sq, Point2 { x: 15.0, y: 5.0 }));
    }
}

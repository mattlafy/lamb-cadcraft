//! Circles of a given radius tangent to two full curves (lines / circles), and TTR's pick among
//! them. Port of `geometry-primitives/src/tangentCircle.ts`.

use serde::{Deserialize, Serialize};

use super::intersect::{infinite_line_intersect, intersect_line_with_boundary_circle};
use super::point::{Point2, add2, dist2, dot2, normalize2, scale2, sub2};
use super::tolerance::TOLERANCE;

/// T134: the full infinite line or full circle a curve sits on — never a segment's drawn extent.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ApparentCurve {
    Line { from: Point2, to: Point2 },
    Circle { center: Point2, radius: f64 },
}

/// Every point (0, 1, or 2) where two full circles cross.
pub fn circle_circle_intersect(c0: Point2, r0: f64, c1: Point2, r1: f64) -> Vec<Point2> {
    let d = dist2(c0, c1);
    if d < 1e-9 || d > r0 + r1 + 1e-6 || d < (r0 - r1).abs() - 1e-6 {
        return Vec::new();
    }
    let a = (r0 * r0 - r1 * r1 + d * d) / (2.0 * d);
    let h = (r0 * r0 - a * a).max(0.0).sqrt();
    let dir = scale2(sub2(c1, c0), 1.0 / d);
    let mid = add2(c0, scale2(dir, a));
    let perp = Point2 { x: -dir.y, y: dir.x };
    if h < 1e-9 { vec![mid] } else { vec![add2(mid, scale2(perp, h)), add2(mid, scale2(perp, -h))] }
}

/// `offset_line`'s result: a translated line.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Segment2 {
    pub from: Point2,
    pub to: Point2,
}

/// Translates an (infinite) line `distance` along its own left-hand normal.
pub fn offset_line(from: Point2, to: Point2, distance: f64) -> Segment2 {
    let dir = normalize2(sub2(to, from));
    let normal = Point2 { x: -dir.y, y: dir.x };
    let off = scale2(normal, distance);
    Segment2 { from: add2(from, off), to: add2(to, off) }
}

/// Which offset of its curve a tangent point was derived from. Line: which side of the line the
/// tangent circle's centre sits on. Circle: "external" (`R + r`) or "internal" (`|R - r|`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TangentSide {
    Left,
    Right,
    External,
    Internal,
}

/// A circle of a given radius tangent to both curves, with where it touches each and which offset
/// produced it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TangentCircle {
    pub center: Point2,
    pub radius: f64,
    pub tangent_a: Point2,
    pub tangent_b: Point2,
    pub side_a: TangentSide,
    pub side_b: TangentSide,
}

#[derive(Debug, Clone, Copy)]
enum OffsetShape {
    Line { point: Point2, dir: Point2 },
    Circle { center: Point2, radius: f64 },
}

#[derive(Debug, Clone, Copy)]
struct OffsetCandidate {
    side: TangentSide,
    shape: OffsetShape,
}

/// The 2 candidate offsets of `curve` at radius `r`.
fn offset_candidates_for(curve: &ApparentCurve, r: f64) -> [OffsetCandidate; 2] {
    match *curve {
        ApparentCurve::Line { from, to } => {
            let left = offset_line(from, to, r);
            let right = offset_line(from, to, -r);
            let dir = normalize2(sub2(to, from));
            [
                OffsetCandidate { side: TangentSide::Left, shape: OffsetShape::Line { point: left.from, dir } },
                OffsetCandidate { side: TangentSide::Right, shape: OffsetShape::Line { point: right.from, dir } },
            ]
        }
        ApparentCurve::Circle { center, radius } => [
            OffsetCandidate { side: TangentSide::External, shape: OffsetShape::Circle { center, radius: radius + r } },
            OffsetCandidate { side: TangentSide::Internal, shape: OffsetShape::Circle { center, radius: (radius - r).abs() } },
        ],
    }
}

fn intersect_offsets(a: &OffsetCandidate, b: &OffsetCandidate) -> Vec<Point2> {
    match (a.shape, b.shape) {
        (OffsetShape::Line { point: pa, dir: da }, OffsetShape::Line { point: pb, dir: db }) => {
            infinite_line_intersect(pa, add2(pa, da), pb, add2(pb, db)).map(|h| vec![h.point]).unwrap_or_default()
        }
        (OffsetShape::Line { point, dir }, OffsetShape::Circle { center, radius })
        | (OffsetShape::Circle { center, radius }, OffsetShape::Line { point, dir }) => {
            intersect_line_with_boundary_circle(point, add2(point, dir), center, radius)
        }
        (OffsetShape::Circle { center: c0, radius: r0 }, OffsetShape::Circle { center: c1, radius: r1 }) => circle_circle_intersect(c0, r0, c1, r1),
    }
}

/// The point where a radius-`r` tangent circle centred at `c` (built from offset `side`) actually
/// touches `curve`.
fn tangent_point_on(curve: &ApparentCurve, side: TangentSide, c: Point2, r: f64) -> Point2 {
    match *curve {
        ApparentCurve::Line { from, to } => {
            let dir = normalize2(sub2(to, from));
            let t = dot2(sub2(c, from), dir);
            add2(from, scale2(dir, t))
        }
        ApparentCurve::Circle { center, radius } => {
            let dir0 = normalize2(sub2(c, center));
            let toward = add2(center, scale2(dir0, radius));
            if side == TangentSide::External {
                return toward;
            }
            let away = add2(center, scale2(dir0, -radius));
            if r <= radius { toward } else { away }
        }
    }
}

/// Every circle of `radius` tangent to both curves. Generation only.
pub fn tangent_circles(a: &ApparentCurve, b: &ApparentCurve, radius: f64) -> Vec<TangentCircle> {
    let cand_a = offset_candidates_for(a, radius);
    let cand_b = offset_candidates_for(b, radius);
    let mut raw: Vec<(Point2, TangentSide, TangentSide)> = Vec::new();
    for ca in &cand_a {
        for cb in &cand_b {
            for center in intersect_offsets(ca, cb) {
                raw.push((center, ca.side, cb.side));
            }
        }
    }
    let mut deduped: Vec<(Point2, TangentSide, TangentSide)> = Vec::new();
    for cand in raw {
        if !deduped.iter().any(|d| dist2(d.0, cand.0) < TOLERANCE.coincidence_mm) {
            deduped.push(cand);
        }
    }
    deduped
        .into_iter()
        .map(|(center, side_a, side_b)| TangentCircle {
            center,
            radius,
            tangent_a: tangent_point_on(a, side_a, center, radius),
            tangent_b: tangent_point_on(b, side_b, center, radius),
            side_a,
            side_b,
        })
        .collect()
}

fn max_pairwise_dist(points: &[Point2]) -> f64 {
    let mut max = 0.0_f64;
    for (i, &p) in points.iter().enumerate() {
        for &q in &points[i + 1..] {
            max = max.max(dist2(p, q));
        }
    }
    max
}

/// Index of the first element minimising `key` (strict `<`, like the TS `reduce`).
fn argmin_by(len: usize, key: impl Fn(usize) -> f64) -> usize {
    let mut best = 0;
    for i in 1..len {
        if key(i) < key(best) {
            best = i;
        }
    }
    best
}

/// TTR's pick: the candidate whose tangent points are nearest the two points actually clicked.
/// 1. A candidate nearest-by-tangent-point for BOTH picks wins outright.
/// 2. Otherwise minimise the spread-normalised sum of both distances.
pub fn choose_tangent_circle(candidates: &[TangentCircle], pick_a: Point2, pick_b: Point2) -> Option<TangentCircle> {
    match candidates.len() {
        0 => return None,
        1 => return Some(candidates[0]),
        _ => {}
    }
    let n = candidates.len();
    let nearest_a = argmin_by(n, |i| dist2(pick_a, candidates[i].tangent_a));
    let nearest_b = argmin_by(n, |i| dist2(pick_b, candidates[i].tangent_b));
    if nearest_a == nearest_b {
        return Some(candidates[nearest_a]);
    }

    let spread_a = max_pairwise_dist(&candidates.iter().map(|c| c.tangent_a).collect::<Vec<_>>());
    let spread_b = max_pairwise_dist(&candidates.iter().map(|c| c.tangent_b).collect::<Vec<_>>());
    let normalized = |d: f64, spread: f64| if spread > TOLERANCE.coincidence_mm { d / spread } else { d };
    let score = |c: &TangentCircle| normalized(dist2(pick_a, c.tangent_a), spread_a) + normalized(dist2(pick_b, c.tangent_b), spread_b);
    Some(candidates[argmin_by(n, |i| score(&candidates[i]))])
}

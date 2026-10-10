//! Line / segment / arc intersections and crossing angles. Port of
//! `geometry-primitives/src/intersect.ts`.

use std::f64::consts::PI;

use serde::{Deserialize, Serialize};

use super::point::{Point2, add2, dot2, normalize2, scale2, sub2};
use super::polyline::{ResolvedSegment, arc_from_bulge, is_arc, point_at_param, tangent_at_param};
use super::tolerance::rad_to_deg;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntersectionHit {
    pub point: Point2,
    /// Param along the first segment/line supplied, 0..1 if it was a finite segment.
    pub t_a: f64,
    /// Param along the second segment/line supplied, 0..1 if it was a finite segment.
    pub t_b: f64,
}

/// Intersection of two infinite lines through (p1,p2) and (p3,p4), with each line's parameter
/// (can be outside 0..1). `None` if parallel.
pub fn infinite_line_intersect(p1: Point2, p2: Point2, p3: Point2, p4: Point2) -> Option<IntersectionHit> {
    let d1 = sub2(p2, p1);
    let d2 = sub2(p4, p3);
    let denom = d1.x * d2.y - d1.y * d2.x;
    if denom.abs() < 1e-12 {
        return None; // parallel
    }
    let diff = sub2(p3, p1);
    let t_a = (diff.x * d2.y - diff.y * d2.x) / denom;
    let t_b = (diff.x * d1.y - diff.y * d1.x) / denom;
    Some(IntersectionHit { point: add2(p1, scale2(d1, t_a)), t_a, t_b })
}

/// Intersection of two finite straight segments, or `None` if they don't cross within range.
pub fn segment_segment_straight(a1: Point2, a2: Point2, b1: Point2, b2: Point2, eps: f64) -> Option<IntersectionHit> {
    let hit = infinite_line_intersect(a1, a2, b1, b2)?;
    if hit.t_a < -eps || hit.t_a > 1.0 + eps || hit.t_b < -eps || hit.t_b > 1.0 + eps {
        return None;
    }
    Some(hit)
}

/// TRIM/EXTEND helper: intersects the INFINITE line through a straight subject segment against a
/// boundary's actual finite segment. `None` if parallel or the hit falls outside the boundary
/// segment's own extent. `t_a` (unclamped) says whether the hit is a trim or an extend.
pub fn intersect_line_with_boundary_segment(
    subject_from: Point2,
    subject_to: Point2,
    boundary_from: Point2,
    boundary_to: Point2,
    eps: f64,
) -> Option<IntersectionHit> {
    let hit = infinite_line_intersect(subject_from, subject_to, boundary_from, boundary_to)?;
    if hit.t_b < -eps || hit.t_b > 1.0 + eps {
        return None;
    }
    Some(hit)
}

/// TRIM/EXTEND boundary helper: every point where the INFINITE line through (p1,p2) crosses a full
/// circle — 0, 1 (tangent, reported twice), or 2 points.
pub fn intersect_line_with_boundary_circle(p1: Point2, p2: Point2, center: Point2, radius: f64) -> Vec<Point2> {
    let d = sub2(p2, p1);
    line_circle_intersect(p1, p2, center, radius).into_iter().map(|t| add2(p1, scale2(d, t))).collect()
}

/// Like [`intersect_line_with_boundary_circle`], but for a single ARC segment: hits outside the
/// arc's own angular span are filtered out.
pub fn intersect_line_with_boundary_arc(p1: Point2, p2: Point2, arc_from: Point2, arc_to: Point2, bulge: f64) -> Vec<Point2> {
    let Some(arc) = arc_from_bulge(arc_from, arc_to, bulge) else { return Vec::new() };
    let seg = ResolvedSegment { index: 0, from: arc_from, to: arc_to, bulge };
    intersect_line_with_boundary_circle(p1, p2, arc.center, arc.radius)
        .into_iter()
        .filter(|&pt| angle_param_on_arc(&seg, pt).is_some_and(|t| (-1e-6..=1.0 + 1e-6).contains(&t)))
        .collect()
}

/// Intersect an infinite line (p1,p2) against a circle, returning up to two params along the line.
fn line_circle_intersect(p1: Point2, p2: Point2, center: Point2, radius: f64) -> Vec<f64> {
    let d = sub2(p2, p1);
    let f = sub2(p1, center);
    let a = dot2(d, d);
    let b = 2.0 * dot2(f, d);
    let c = dot2(f, f) - radius * radius;
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 || a < 1e-12 {
        return Vec::new();
    }
    let sqrt_disc = disc.sqrt();
    vec![(-b - sqrt_disc) / (2.0 * a), (-b + sqrt_disc) / (2.0 * a)]
}

fn angle_param_on_arc(seg: &ResolvedSegment, point: Point2) -> Option<f64> {
    let arc = arc_from_bulge(seg.from, seg.to, seg.bulge)?;
    let angle = (point.y - arc.center.y).atan2(point.x - arc.center.x);
    let two_pi = PI * 2.0;
    let delta = ((angle - arc.start_angle) % two_pi + two_pi) % two_pi;
    let ccw_total = ((arc.end_angle - arc.start_angle) % two_pi + two_pi) % two_pi;
    if arc.ccw {
        Some(if ccw_total < 1e-12 { 0.0 } else { delta / ccw_total })
    } else {
        let delta = delta - two_pi;
        let total = ccw_total - two_pi;
        Some(if total.abs() < 1e-12 { 0.0 } else { delta / total })
    }
}

/// Intersects a finite segment (straight or arc) against an INFINITE line through
/// (line_from, line_to). `t_a` is the param along `seg` (clamped); `t_b` the param along the line
/// (unclamped).
pub fn intersect_segment_with_infinite_line(seg: &ResolvedSegment, line_from: Point2, line_to: Point2, eps: f64) -> Vec<IntersectionHit> {
    if !is_arc(seg.bulge) {
        return match infinite_line_intersect(seg.from, seg.to, line_from, line_to) {
            Some(hit) if !(hit.t_a < -eps || hit.t_a > 1.0 + eps) => vec![hit],
            _ => Vec::new(),
        };
    }

    let Some(arc) = arc_from_bulge(seg.from, seg.to, seg.bulge) else { return Vec::new() };
    let mut out = Vec::new();
    for t_b in line_circle_intersect(line_from, line_to, arc.center, arc.radius) {
        let point = add2(line_from, scale2(sub2(line_to, line_from), t_b));
        let Some(t_arc) = angle_param_on_arc(seg, point) else { continue };
        if t_arc < -eps || t_arc > 1.0 + eps {
            continue;
        }
        out.push(IntersectionHit { point, t_a: t_arc.clamp(0.0, 1.0), t_b });
    }
    out
}

/// General segment-vs-segment intersection: straight/straight, straight/arc, and arc/arc (arc/arc
/// falls back to sampling).
pub fn intersect_segments(a: &ResolvedSegment, b: &ResolvedSegment, eps: f64) -> Vec<IntersectionHit> {
    match (is_arc(a.bulge), is_arc(b.bulge)) {
        (false, false) => segment_segment_straight(a.from, a.to, b.from, b.to, eps).into_iter().collect(),
        (true, false) => {
            intersect_arc_line(a, b.from, b.to, eps).into_iter().map(|h| IntersectionHit { point: h.point, t_a: h.t_arc, t_b: h.t_line }).collect()
        }
        (false, true) => {
            intersect_arc_line(b, a.from, a.to, eps).into_iter().map(|h| IntersectionHit { point: h.point, t_a: h.t_line, t_b: h.t_arc }).collect()
        }
        (true, true) => {
            // arc/arc: sample one arc finely and test each sub-chord against the other arc.
            let samples = 64;
            let mut hits = Vec::new();
            let mut prev = point_at_param(a, 0.0);
            for i in 1..=samples {
                let t = f64::from(i) / f64::from(samples);
                let cur = point_at_param(a, t);
                for h in intersect_arc_line(b, prev, cur, eps) {
                    hits.push(IntersectionHit { point: h.point, t_a: (f64::from(i - 1) + h.t_line) / f64::from(samples), t_b: h.t_arc });
                }
                prev = cur;
            }
            hits
        }
    }
}

struct ArcLineHit {
    point: Point2,
    t_arc: f64,
    t_line: f64,
}

fn intersect_arc_line(arc_seg: &ResolvedSegment, p1: Point2, p2: Point2, eps: f64) -> Vec<ArcLineHit> {
    let Some(arc) = arc_from_bulge(arc_seg.from, arc_seg.to, arc_seg.bulge) else { return Vec::new() };
    let mut out = Vec::new();
    for t_line in line_circle_intersect(p1, p2, arc.center, arc.radius) {
        if t_line < -eps || t_line > 1.0 + eps {
            continue;
        }
        let point = add2(p1, scale2(sub2(p2, p1), t_line));
        let Some(t_arc) = angle_param_on_arc(arc_seg, point) else { continue };
        if t_arc < -eps || t_arc > 1.0 + eps {
            continue;
        }
        out.push(ArcLineHit { point, t_arc: t_arc.clamp(0.0, 1.0), t_line: t_line.clamp(0.0, 1.0) });
    }
    out
}

/// Crossing angle between two directions, folded into [0, 90] degrees (90 = perpendicular,
/// 0 = parallel).
pub fn crossing_angle_deg(dir_a: Point2, dir_b: Point2) -> f64 {
    let a = normalize2(dir_a);
    let b = normalize2(dir_b);
    let dot = dot2(a, b).clamp(-1.0, 1.0);
    rad_to_deg(dot.abs().acos())
}

pub fn tangent_at_hit(seg: &ResolvedSegment, t: f64) -> Point2 {
    tangent_at_param(seg, t)
}

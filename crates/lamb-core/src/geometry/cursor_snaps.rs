//! The three osnaps whose point depends on where the cursor (or the rubber-band's anchor) is:
//! nearest, perpendicular, apparent intersection. None of them filters on `within_extent` — that
//! is the canvas's decision. Port of `geometry-primitives/src/cursorSnaps.ts`.

use serde::{Deserialize, Serialize};

use super::fillet::{apparent_curve_intersections, curve_from_edge, param_along_edge};
use super::point::{Point2, add2, dist2, dot2, normalize2, scale2, sub2};
use super::polyline::{ResolvedSegment, arc_from_bulge, closest_point_on_segment, is_arc, segment_length};
use super::tolerance::TOLERANCE;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorSnapHit {
    pub point: Point2,
    /// The hit lies on the segment as DRAWN, not on its extension/full circle.
    pub within_extent: bool,
}

/// `TOLERANCE.coincidence_mm` expressed in the segment's own 0..1 parameter space.
fn param_tolerance(seg: &ResolvedSegment) -> f64 {
    let len = segment_length(seg);
    if len < 1e-9 { 1.0 } else { TOLERANCE.coincidence_mm / len }
}

fn within_param(seg: &ResolvedSegment, t: f64) -> bool {
    let eps = param_tolerance(seg);
    t >= -eps && t <= 1.0 + eps
}

/// Coincident hits within `TOLERANCE.coincidence_mm` collapse to one, keeping `within_extent` if
/// any of them had it.
fn collapse(hits: &[CursorSnapHit]) -> Vec<CursorSnapHit> {
    let mut out: Vec<CursorSnapHit> = Vec::new();
    for hit in hits {
        match out.iter_mut().find(|h| dist2(h.point, hit.point) <= TOLERANCE.coincidence_mm) {
            Some(existing) => existing.within_extent |= hit.within_extent,
            None => out.push(*hit),
        }
    }
    out
}

/// NEAREST — the closest point on the segment as drawn (always `within_extent`).
pub fn nearest_candidate(seg: &ResolvedSegment, cursor: Point2) -> CursorSnapHit {
    CursorSnapHit { point: closest_point_on_segment(seg, cursor).point, within_extent: true }
}

/// PERPENDICULAR — the foot(feet) of the perpendicular from `from` onto the segment. Unclamped for
/// a straight segment; an arc has up to two feet on its full circle.
pub fn perpendicular_candidates(seg: &ResolvedSegment, from: Point2) -> Vec<CursorSnapHit> {
    if !is_arc(seg.bulge) {
        let dir = sub2(seg.to, seg.from);
        let len2 = dot2(dir, dir);
        if len2 < 1e-12 {
            return Vec::new();
        }
        let t = dot2(sub2(from, seg.from), dir) / len2;
        let point = add2(seg.from, scale2(dir, t));
        return vec![CursorSnapHit { point, within_extent: within_param(seg, t) }];
    }

    let Some(arc) = arc_from_bulge(seg.from, seg.to, seg.bulge) else { return Vec::new() };
    let radial = sub2(from, arc.center);
    // `from` at the centre: every direction is perpendicular, so none is the snap.
    if dist2(from, arc.center) < 1e-9 {
        return Vec::new();
    }
    let unit = normalize2(radial);
    let hits: Vec<CursorSnapHit> = [1.0, -1.0]
        .iter()
        .map(|sign| {
            let point = add2(arc.center, scale2(unit, sign * arc.radius));
            let t = param_along_edge(seg.from, seg.to, seg.bulge, point);
            CursorSnapHit { point, within_extent: within_param(seg, t) }
        })
        .collect();
    collapse(&hits)
}

/// APPARENT INTERSECTION — where the two segments' full curves cross but the drawn edges do not.
/// A crossing within BOTH drawn extents is a real intersection and is dropped.
pub fn apparent_intersection_candidates(a: &ResolvedSegment, b: &ResolvedSegment) -> Vec<CursorSnapHit> {
    let curve_a = curve_from_edge(a.from, a.to, a.bulge);
    let curve_b = curve_from_edge(b.from, b.to, b.bulge);
    let mut hits: Vec<CursorSnapHit> = Vec::new();
    for point in apparent_curve_intersections(&curve_a, &curve_b) {
        let on_a = within_param(a, param_along_edge(a.from, a.to, a.bulge, point));
        let on_b = within_param(b, param_along_edge(b.from, b.to, b.bulge, point));
        if on_a && on_b {
            continue; // a real intersection — `intersect_segments` owns it
        }
        hits.push(CursorSnapHit { point, within_extent: on_a || on_b });
    }
    collapse(&hits)
}

//! Minimum distance between two polylines (T136, the QUERY command's `near` clause). Port of
//! `geometry-primitives/src/distance.ts`.

use super::intersect::segment_segment_straight;
use super::point::{Point2, dist2};
use super::polyline::{Polyline2, ResolvedSegment, closest_point_on_segment, is_arc, point_at_param, resolved_segments};

/// Straight segment `a` vs. straight segment `b`: exact closest approach. Crossing segments are
/// distance 0; otherwise the closest pair is one segment's endpoint against the other segment.
fn straight_segment_distance(a: &ResolvedSegment, b: &ResolvedSegment) -> f64 {
    if segment_segment_straight(a.from, a.to, b.from, b.to, 1e-6).is_some() {
        return 0.0;
    }
    closest_point_on_segment(b, a.from)
        .distance
        .min(closest_point_on_segment(b, a.to).distance)
        .min(closest_point_on_segment(a, b.from).distance)
        .min(closest_point_on_segment(a, b.to).distance)
}

/// Sample density for an arc-bearing pair; the error is bounded by
/// (segment length) / (2 * ARC_SAMPLES).
const ARC_SAMPLES: u32 = 64;

/// Points sampled evenly (by parameter) along one segment's sweep, endpoints included.
fn sample_segment_points(seg: &ResolvedSegment) -> impl Iterator<Item = Point2> + '_ {
    (0..=ARC_SAMPLES).map(move |i| point_at_param(seg, f64::from(i) / f64::from(ARC_SAMPLES)))
}

fn segment_distance(a: &ResolvedSegment, b: &ResolvedSegment) -> f64 {
    if !is_arc(a.bulge) && !is_arc(b.bulge) {
        return straight_segment_distance(a, b);
    }
    let mut min = f64::INFINITY;
    if is_arc(a.bulge) {
        for p in sample_segment_points(a) {
            min = min.min(closest_point_on_segment(b, p).distance);
        }
    }
    if is_arc(b.bulge) {
        for p in sample_segment_points(b) {
            min = min.min(closest_point_on_segment(a, p).distance);
        }
    }
    min
}

/// Minimum distance between two polylines — 0 if they cross or touch. Exact for straight×straight
/// pairs; sampled when either segment is an arc. Zero-segment polylines fall back to `start`.
pub fn polyline_distance(a: &Polyline2, b: &Polyline2) -> f64 {
    let segs_a = resolved_segments(a);
    let segs_b = resolved_segments(b);
    if segs_a.is_empty() && segs_b.is_empty() {
        return dist2(a.start, b.start);
    }
    if segs_a.is_empty() {
        return segs_b.iter().map(|sb| closest_point_on_segment(sb, a.start).distance).fold(f64::INFINITY, f64::min);
    }
    if segs_b.is_empty() {
        return segs_a.iter().map(|sa| closest_point_on_segment(sa, b.start).distance).fold(f64::INFINITY, f64::min);
    }
    let mut min = f64::INFINITY;
    for sa in &segs_a {
        for sb in &segs_b {
            let d = segment_distance(sa, sb);
            if d < min {
                min = d;
            }
        }
    }
    min
}

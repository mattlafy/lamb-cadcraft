//! OFFSET of a bulge polyline. Port of `geometry-primitives/src/offset.ts`.

use super::intersect::infinite_line_intersect;
use super::point::{Point2, add2, dist2, normalize2, scale2, sub2};
use super::polyline::{PolySegment, Polyline2, ResolvedSegment, arc_from_bulge, is_arc, resolved_segments};

/// Offsets a polyline by `distance` to one side: positive is LEFT of travel, negative right.
/// Corners between two STRAIGHT segments are mitered; a corner touching an arc is joined directly.
/// Degenerate (collapsed/inverted) arcs are dropped.
pub fn offset_polyline(poly: &Polyline2, distance: f64) -> Option<Polyline2> {
    let segs = resolved_segments(poly);
    if segs.is_empty() {
        return None;
    }

    let primitives: Vec<OffsetPrimitive> = segs.iter().filter_map(|seg| offset_segment_primitive(seg, distance)).collect();
    let n = primitives.len();
    if n == 0 {
        return None;
    }

    // A CLOSED polyline's shared start/end vertex is mitered like any other corner — but only if
    // every segment survived offsetting.
    let closable = poly.closed && n == segs.len();

    // corners[i] is where primitive i STARTS, mitered against its predecessor.
    let corners: Vec<Point2> = (0..n)
        .map(|i| {
            let cur = &primitives[i];
            if i == 0 && !closable {
                return cur.from;
            }
            let prev = &primitives[(i + n - 1) % n];
            corner_join(prev, cur).unwrap_or(cur.from)
        })
        .collect();

    let last_point = if closable { corners[0] } else { primitives[n - 1].to };
    let out_segments: Vec<PolySegment> = primitives
        .iter()
        .enumerate()
        .map(|(i, prim)| PolySegment { to: if i == n - 1 { last_point } else { corners[i + 1] }, bulge: prim.bulge, corner_at_to: None })
        .collect();

    Some(Polyline2 { start: corners[0], segments: out_segments, closed: poly.closed })
}

struct OffsetPrimitive {
    from: Point2,
    to: Point2,
    bulge: f64,
    /// Present for arcs.
    is_arc: bool,
}

fn offset_segment_primitive(seg: &ResolvedSegment, distance: f64) -> Option<OffsetPrimitive> {
    if !is_arc(seg.bulge) {
        let dir = normalize2(sub2(seg.to, seg.from));
        let normal = Point2 { x: -dir.y, y: dir.x }; // left of travel
        let offset = scale2(normal, distance);
        return Some(OffsetPrimitive { from: add2(seg.from, offset), to: add2(seg.to, offset), bulge: 0.0, is_arc: false });
    }

    let arc = arc_from_bulge(seg.from, seg.to, seg.bulge)?;
    // CCW arc: offsetting left shrinks the radius; CW: the opposite.
    let sign = if arc.ccw { -1.0 } else { 1.0 };
    let new_radius = arc.radius + sign * distance;
    if new_radius <= 1e-6 {
        return None; // degenerate/inverted
    }

    let from_dir = normalize2(sub2(seg.from, arc.center));
    let to_dir = normalize2(sub2(seg.to, arc.center));
    let from = add2(arc.center, scale2(from_dir, new_radius));
    let to = add2(arc.center, scale2(to_dir, new_radius));
    // Same included angle and direction, so the bulge is unchanged.
    Some(OffsetPrimitive { from, to, bulge: seg.bulge, is_arc: true })
}

fn corner_join(prev: &OffsetPrimitive, cur: &OffsetPrimitive) -> Option<Point2> {
    if !prev.is_arc && !cur.is_arc {
        return infinite_line_intersect(prev.from, prev.to, cur.from, cur.to).map(|hit| hit.point);
    }
    // At least one side is an arc: join directly without mitering.
    None
}

/// An open or closed straight polyline through `points`. TS leaves `start` undefined for no
/// points; Rust uses the origin.
pub fn polyline_from_points(points: &[Point2], closed: bool) -> Polyline2 {
    let start = points.first().copied().unwrap_or_default();
    let segments = points.iter().skip(1).map(|&to| PolySegment { to, bulge: 0.0, corner_at_to: None }).collect();
    Polyline2 { start, segments, closed }
}

pub fn chord_distance(a: Point2, b: Point2) -> f64 {
    dist2(a, b)
}

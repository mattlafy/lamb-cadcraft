//! Bulge polylines: segments, arcs, virtual corners, sliding and co-arc merging. Port of
//! `geometry-primitives/src/polyline.ts`.

use std::f64::consts::PI;

use serde::{Deserialize, Serialize};

use super::circle_fit::circumcircle;
use super::point::{Point2, add2, cross2, dist2, dot2, lerp2, normalize2, scale2, sub2};
use super::tolerance::TOLERANCE;

/// T9: a corner rounded/chamfered by FILLET/CHAMFER defaults to VIRTUAL — the vertex itself stays
/// an ordinary point; this tag just says "draw this corner rounded/chamfered with these params,
/// computed live from whatever its neighbors currently are."
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum VirtualCornerEntry {
    Fillet { radius: f64 },
    Chamfer { distance1: f64, distance2: f64 },
}

/// A polyline segment: straight, or an arc via the standard CAD "bulge" factor
/// (bulge = tan(includedAngle / 4); 0 = straight).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolySegment {
    pub to: Point2,
    pub bulge: f64,
    /// T58: the vertex at this segment's own `to` is filleted/chamfered. The tag rides the segment
    /// so every splice that reuses segments carries surviving corners for free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corner_at_to: Option<VirtualCornerEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Polyline2 {
    pub start: Point2,
    pub segments: Vec<PolySegment>,
    pub closed: bool,
}

/// The segment whose `to` is vertex `corner_index`, i.e. the one that owns that vertex's
/// `corner_at_to` tag — `corner_index - 1`, except that a CLOSED polyline's wrap corner (vertex 0)
/// rides the LAST segment. `None` when the vertex can't be a corner at all: an open polyline's two
/// free ends.
pub fn virtual_corner_segment_index(poly: &Polyline2, corner_index: usize) -> Option<usize> {
    let n = poly.segments.len();
    if corner_index == 0 {
        return if poly.closed && n >= 2 { Some(n - 1) } else { None };
    }
    if corner_index < n { Some(corner_index - 1) } else { None }
}

/// The fillet/chamfer tag on vertex `corner_index`, if it has one.
pub fn virtual_corner_at(poly: &Polyline2, corner_index: usize) -> Option<VirtualCornerEntry> {
    let seg_index = virtual_corner_segment_index(poly, corner_index)?;
    poly.segments.get(seg_index).and_then(|s| s.corner_at_to)
}

/// One tagged corner: the vertex index it describes and its tag.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CornerEntry {
    pub corner_index: usize,
    pub entry: VirtualCornerEntry,
}

/// Every tagged corner, in segment order, paired with the vertex index it describes.
pub fn virtual_corner_entries(poly: &Polyline2) -> Vec<CornerEntry> {
    let n = poly.segments.len();
    let mut out = Vec::new();
    for (i, seg) in poly.segments.iter().enumerate() {
        let Some(entry) = seg.corner_at_to else { continue };
        // The last segment ends at vertex 0 when closed, and at an open polyline's free end
        // otherwise — which is never a corner, so a stray tag there is ignored.
        let corner_index = if i == n - 1 { if poly.closed { Some(0) } else { None } } else { Some(i + 1) };
        if let Some(corner_index) = corner_index {
            out.push(CornerEntry { corner_index, entry });
        }
    }
    out
}

/// A segment with its start point resolved, addressable by index.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedSegment {
    pub index: usize,
    pub from: Point2,
    pub to: Point2,
    pub bulge: f64,
}

pub fn line2(from: Point2, to: Point2) -> Polyline2 {
    Polyline2 { start: from, segments: vec![PolySegment { to, bulge: 0.0, corner_at_to: None }], closed: false }
}

/// Walk a polyline's segments, pairing each with its resolved start point.
pub fn resolved_segments(poly: &Polyline2) -> Vec<ResolvedSegment> {
    let mut out = Vec::with_capacity(poly.segments.len());
    let mut from = poly.start;
    for (index, seg) in poly.segments.iter().enumerate() {
        out.push(ResolvedSegment { index, from, to: seg.to, bulge: seg.bulge });
        from = seg.to;
    }
    out
}

/// Same shape, walked in the opposite direction: new start = old end, segment order reverses,
/// each bulge negates. T58: new segment `k` ends at old vertex `n-1-k`, so it inherits whatever
/// tag described THAT vertex (tags shift by one relative to the segments).
pub fn reverse_polyline(poly: &Polyline2) -> Polyline2 {
    let resolved = resolved_segments(poly);
    let Some(last) = resolved.last() else { return poly.clone() };
    let segments =
        resolved.iter().rev().map(|seg| PolySegment { to: seg.from, bulge: -seg.bulge, corner_at_to: virtual_corner_at(poly, seg.index) }).collect();
    Polyline2 { start: last.to, segments, closed: poly.closed }
}

/// TS `isArc(seg: { bulge })`, taking the bulge itself.
pub fn is_arc(bulge: f64) -> bool {
    bulge.abs() > 1e-12
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArcGeometry {
    pub center: Point2,
    pub radius: f64,
    /// Radians, angle of `from` relative to center.
    pub start_angle: f64,
    /// Radians, angle of `to` relative to center.
    pub end_angle: f64,
    /// true = sweeps counterclockwise (increasing angle) from start to end.
    pub ccw: bool,
}

/// Convert a bulge-encoded arc to center/radius/angle form. bulge > 0 sweeps counterclockwise
/// from `from` to `to`; bulge < 0 clockwise. `None` for a straight segment (bulge ~ 0) or a
/// degenerate chord.
pub fn arc_from_bulge(from: Point2, to: Point2, bulge: f64) -> Option<ArcGeometry> {
    if bulge.abs() < 1e-12 {
        return None;
    }
    let chord = dist2(from, to);
    if chord < 1e-9 {
        return None;
    }
    let b = bulge;
    let signed_radius = (chord * (1.0 + b * b)) / (4.0 * b);
    let h = (chord * (1.0 - b * b)) / (4.0 * b);

    let dir = normalize2(sub2(to, from));
    let perp = Point2 { x: -dir.y, y: dir.x }; // rotate +90deg (CCW)
    let mid = lerp2(from, to, 0.5);
    let center = add2(mid, scale2(perp, h));

    let radius = signed_radius.abs();
    let start_angle = (from.y - center.y).atan2(from.x - center.x);
    let end_angle = (to.y - center.y).atan2(to.x - center.x);
    Some(ArcGeometry { center, radius, start_angle, end_angle, ccw: bulge > 0.0 })
}

/// The arc a segment describes, or `None` when it is straight. TS asserts `arcFromBulge(...)!`
/// after `isArc`; the only way that is null is a degenerate (< 1e-9) chord, where TS throws. The
/// Rust port treats that segment as straight instead.
fn segment_arc(seg: &ResolvedSegment) -> Option<ArcGeometry> {
    if !is_arc(seg.bulge) {
        return None;
    }
    arc_from_bulge(seg.from, seg.to, seg.bulge)
}

/// Normalize `to` to be reachable from `from` by sweeping in `ccw` direction.
pub fn sweep_angle(from: f64, to: f64, ccw: bool) -> f64 {
    let two_pi = PI * 2.0;
    let delta = ((to - from) % two_pi + two_pi) % two_pi;
    if ccw { delta } else { delta - two_pi }
}

/// `Math.max(0, Math.min(1, t))` (NaN stays NaN).
fn clamp01(t: f64) -> f64 {
    t.clamp(0.0, 1.0)
}

/// Point along a single segment at parameter t in [0, 1].
pub fn point_at_param(seg: &ResolvedSegment, t: f64) -> Point2 {
    let clamped = clamp01(t);
    let Some(arc) = segment_arc(seg) else { return lerp2(seg.from, seg.to, clamped) };
    let delta = sweep_angle(arc.start_angle, arc.end_angle, arc.ccw);
    let angle = arc.start_angle + delta * clamped;
    Point2 { x: arc.center.x + arc.radius * angle.cos(), y: arc.center.y + arc.radius * angle.sin() }
}

/// Unit tangent vector (direction of travel from `from` to `to`) at parameter t.
pub fn tangent_at_param(seg: &ResolvedSegment, t: f64) -> Point2 {
    let Some(arc) = segment_arc(seg) else { return normalize2(sub2(seg.to, seg.from)) };
    let delta = sweep_angle(arc.start_angle, arc.end_angle, arc.ccw);
    let angle = arc.start_angle + delta * clamp01(t);
    // Tangent is perpendicular to the radius, oriented by sweep direction.
    let radial = Point2 { x: angle.cos(), y: angle.sin() };
    let ccw_tangent = Point2 { x: -radial.y, y: radial.x };
    if delta >= 0.0 { ccw_tangent } else { scale2(ccw_tangent, -1.0) }
}

pub fn segment_length(seg: &ResolvedSegment) -> f64 {
    let Some(arc) = segment_arc(seg) else { return dist2(seg.from, seg.to) };
    let delta = sweep_angle(arc.start_angle, arc.end_angle, arc.ccw).abs();
    arc.radius * delta
}

pub fn polyline_length(poly: &Polyline2) -> f64 {
    resolved_segments(poly).iter().fold(0.0, |sum, seg| sum + segment_length(seg))
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BBox2 {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

const EMPTY_BBOX: BBox2 = BBox2 { min_x: f64::INFINITY, min_y: f64::INFINITY, max_x: f64::NEG_INFINITY, max_y: f64::NEG_INFINITY };

fn bbox_union(b: BBox2, o: BBox2) -> BBox2 {
    BBox2 { min_x: b.min_x.min(o.min_x), min_y: b.min_y.min(o.min_y), max_x: b.max_x.max(o.max_x), max_y: b.max_y.max(o.max_y) }
}

fn point_bbox(p: Point2) -> BBox2 {
    BBox2 { min_x: p.x, min_y: p.y, max_x: p.x, max_y: p.y }
}

pub fn segment_bbox(seg: &ResolvedSegment) -> BBox2 {
    if segment_arc(seg).is_none() {
        return BBox2 {
            min_x: seg.from.x.min(seg.to.x),
            min_y: seg.from.y.min(seg.to.y),
            max_x: seg.from.x.max(seg.to.x),
            max_y: seg.from.y.max(seg.to.y),
        };
    }
    // Conservative bbox: sample endpoints plus points along the sweep.
    let samples = 32;
    let mut b = bbox_union(bbox_union(EMPTY_BBOX, point_bbox(seg.from)), point_bbox(seg.to));
    for i in 0..=samples {
        b = bbox_union(b, point_bbox(point_at_param(seg, f64::from(i) / f64::from(samples))));
    }
    b
}

pub fn polyline_bbox(poly: &Polyline2) -> BBox2 {
    let segs = resolved_segments(poly);
    if segs.is_empty() {
        return point_bbox(poly.start);
    }
    segs.iter().fold(EMPTY_BBOX, |b, seg| bbox_union(b, segment_bbox(seg)))
}

/// `closest_point_on_segment`'s result.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClosestPoint {
    pub point: Point2,
    pub t: f64,
    pub distance: f64,
}

/// Closest point on a segment to `p`, plus the parameter t it occurs at.
pub fn closest_point_on_segment(seg: &ResolvedSegment, p: Point2) -> ClosestPoint {
    let Some(arc) = segment_arc(seg) else {
        let d = sub2(seg.to, seg.from);
        let len2 = d.x * d.x + d.y * d.y;
        let t = if len2 < 1e-12 { 0.0 } else { clamp01(((p.x - seg.from.x) * d.x + (p.y - seg.from.y) * d.y) / len2) };
        let point = lerp2(seg.from, seg.to, t);
        return ClosestPoint { point, t, distance: dist2(point, p) };
    };
    let angle_to_p = (p.y - arc.center.y).atan2(p.x - arc.center.x);
    // T82: both sweeps carry the arc's own direction, so the fraction along it is |rel| / |delta|.
    let delta = sweep_angle(arc.start_angle, arc.end_angle, arc.ccw);
    let total = delta.abs();
    let rel = sweep_angle(arc.start_angle, angle_to_p, arc.ccw).abs();
    let mut t = if total < 1e-12 { 0.0 } else { rel / total };
    if t > 1.0 {
        // The point is on the complementary arc; clamp to whichever end is nearer in angle.
        t = if rel - total <= PI * 2.0 - rel { 1.0 } else { 0.0 };
    }
    let point = point_at_param(seg, t);
    ClosestPoint { point, t, distance: dist2(point, p) }
}

// --------------------------------------------------- T108: ray-sliding --
// Moves one segment of a polyline while keeping every OTHER segment's shape non-deforming: each
// neighbour keeps its own direction and only changes length.

const RAY_PARALLEL_EPS: f64 = 1e-9;

/// Point on the infinite line through `anchor` with direction `dir` (unit) closest to `point`.
fn closest_point_on_infinite_line(anchor: Point2, dir: Point2, point: Point2) -> Point2 {
    add2(anchor, scale2(dir, dot2(sub2(point, anchor), dir)))
}

/// Where two infinite lines (each anchor + unit direction) cross, or `None` if parallel.
fn intersect_rays(anchor_a: Point2, dir_a: Point2, anchor_b: Point2, dir_b: Point2) -> Option<Point2> {
    let denom = dir_a.x * dir_b.y - dir_a.y * dir_b.x;
    if denom.abs() < RAY_PARALLEL_EPS {
        return None;
    }
    let diff = sub2(anchor_b, anchor_a);
    let t = (diff.x * dir_b.y - diff.y * dir_b.x) / denom;
    Some(add2(anchor_a, scale2(dir_a, t)))
}

/// Where one end of the moved segment lands after a translate by `delta`.
struct SlidEnd {
    /// The moved segment's new end.
    moved: Point2,
    /// LAMB T573: a straight neighbour collinear with the moved segment cannot absorb the part of
    /// the move across them, so the shared vertex is doubled: the neighbour's end lands here (on
    /// its own line) and a perpendicular connector joins it to `moved`. `None` when the neighbour
    /// simply ends at `moved`.
    neighbor: Option<Point2>,
}

/// Slides one endpoint of the moved segment to reconnect with `neighbor` after a translate by
/// `delta`, keeping `neighbor`'s own direction fixed. `shared_is_to` is true when `neighbor.to` is
/// the shared vertex. A `None` neighbour is an open end, which translates freely.
fn slide_endpoint(shared_original: Point2, neighbor: Option<&ResolvedSegment>, shared_is_to: bool, moved_dir: Point2, delta: Point2) -> SlidEnd {
    let target = add2(shared_original, delta);
    let Some(neighbor) = neighbor else { return SlidEnd { moved: target, neighbor: None } };

    let far = if shared_is_to { neighbor.from } else { neighbor.to };

    if is_arc(neighbor.bulge) {
        // A non-tangent arc neighbour (a tangent fillet is handled by `slide_fillet` before this
        // is reached) reconnects to the closest point on its CHORD's line.
        let shared = if shared_is_to { neighbor.to } else { neighbor.from };
        let chord_dir = normalize2(sub2(shared, far));
        return SlidEnd { moved: closest_point_on_infinite_line(far, chord_dir, target), neighbor: None };
    }

    let neighbor_dir = normalize2(sub2(neighbor.to, neighbor.from));
    let parallel = cross2(moved_dir, neighbor_dir).abs() < RAY_PARALLEL_EPS;
    if parallel {
        // The neighbour's ray can only absorb a slide ALONG the shared direction; the rest of the
        // move becomes a connector perpendicular to the pair.
        let along = add2(shared_original, scale2(moved_dir, dot2(delta, moved_dir)));
        if dist2(along, target) <= TOLERANCE.coincidence_mm {
            return SlidEnd { moved: along, neighbor: None };
        }
        return SlidEnd { moved: target, neighbor: Some(along) };
    }

    // Shift the moved segment's own infinite line by delta's perpendicular component, then
    // intersect that line with the neighbour's fixed ray.
    let perp = sub2(delta, scale2(moved_dir, dot2(delta, moved_dir)));
    let shifted_anchor = add2(shared_original, perp);
    let hit = intersect_rays(far, neighbor_dir, shifted_anchor, moved_dir);
    SlidEnd { moved: hit.unwrap_or(target), neighbor: None }
}

/// §4b: a tangent fillet beside the moved segment keeps its radius. `fillet` is the arc neighbour,
/// `beyond` the straight segment past it, `fillet_is_prev` says which side of the moved segment
/// (`moved_shared` is its vertex shared with the fillet). When the arc is tangent to both lines,
/// it translates by the shift of the two lines' intersection, so turn angle, radius and both
/// tangencies are unchanged. Returns the moved segment's new shared end and the new position of
/// the fillet's other end (the vertex it shares with `beyond`), or `None` when the arc is not a
/// tangent fillet (the chord rule applies).
fn slide_fillet(
    moved_shared: Point2,
    fillet: &ResolvedSegment,
    beyond: Option<&ResolvedSegment>,
    fillet_is_prev: bool,
    moved_dir: Point2,
    delta: Point2,
) -> Option<(Point2, Point2)> {
    let beyond = beyond.filter(|b| !is_arc(b.bulge))?;
    if !is_arc(fillet.bulge) {
        return None;
    }
    let beyond_dir = normalize2(sub2(beyond.to, beyond.from));
    // Tangent directions of the arc at its two ends, in travel order.
    let (at_moved, at_beyond) = if fillet_is_prev {
        (tangent_at_param(fillet, 1.0), tangent_at_param(fillet, 0.0))
    } else {
        (tangent_at_param(fillet, 0.0), tangent_at_param(fillet, 1.0))
    };
    let tol = TOLERANCE.angle_deg.to_radians();
    let aligned = |a: Point2, b: Point2| dot2(a, b) > 0.0 && cross2(a, b).abs() < tol.sin();
    if !aligned(at_moved, moved_dir) || !aligned(at_beyond, beyond_dir) {
        return None;
    }
    let beyond_shared = if fillet_is_prev { beyond.to } else { beyond.from };
    let corner = intersect_rays(moved_shared, moved_dir, beyond_shared, beyond_dir)?;
    let perp = sub2(delta, scale2(moved_dir, dot2(delta, moved_dir)));
    let shifted = intersect_rays(add2(moved_shared, perp), moved_dir, beyond_shared, beyond_dir)?;
    let v = sub2(shifted, corner);
    Some((add2(moved_shared, v), add2(beyond_shared, v)))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentSlideResult {
    pub poly: Polyline2,
    /// The component of `delta` an adjacent ray could not absorb, or `None` when the full delta
    /// was achieved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreachable: Option<Point2>,
    /// LAMB T573: indices, in `poly`, of the connector segments inserted where a collinear
    /// neighbour's shared vertex was doubled, ascending. An original segment moves up by one per
    /// inserted index at or below its running index (walk them ascending).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inserted: Vec<usize>,
}

/// T108: translate `poly`'s segment `segment_index` by `delta`, stretching (never deforming) its
/// neighbours to reconnect. An open end translates freely. A tangent fillet arc beside a straight
/// moved segment translates with the corner it rounds, keeping its radius and tangencies (§4b); a
/// non-tangent arc neighbour keeps the chord rule. A straight neighbour collinear with the moved
/// segment keeps its line and gets a perpendicular connector at the doubled vertex (T573,
/// [`SegmentSlideResult::inserted`]). `corner_at_to` tags are left untouched.
pub fn slide_segment_by_delta(poly: &Polyline2, segment_index: usize, delta: Point2) -> SegmentSlideResult {
    let resolved = resolved_segments(poly);
    let n = resolved.len();
    let Some(seg) = resolved.get(segment_index).copied() else {
        return SegmentSlideResult { poly: poly.clone(), unreachable: None, inserted: Vec::new() };
    };

    let wrap = poly.closed && n >= 2;
    let prev_index = if segment_index > 0 {
        Some(segment_index - 1)
    } else if wrap {
        Some(n - 1)
    } else {
        None
    };
    let next_index = if segment_index + 1 < n {
        Some(segment_index + 1)
    } else if wrap {
        Some(0)
    } else {
        None
    };
    let prev_seg = prev_index.filter(|&i| i != segment_index).and_then(|i| resolved.get(i));
    let next_seg = next_index.filter(|&i| i != segment_index).and_then(|i| resolved.get(i));

    // R0: an arc moved segment translates rigidly and uses its CHORD direction.
    let moved_dir = normalize2(sub2(seg.to, seg.from));

    // The segment past a neighbour (cyclically when closed), never the moved segment itself.
    let beyond = |i: usize, before: bool| -> Option<&ResolvedSegment> {
        let j = if before {
            if i > 0 {
                Some(i - 1)
            } else if wrap {
                Some(n - 1)
            } else {
                None
            }
        } else if i + 1 < n {
            Some(i + 1)
        } else if wrap {
            Some(0)
        } else {
            None
        };
        j.filter(|&j| j != segment_index && j != i).and_then(|j| resolved.get(j))
    };
    let straight_moved = !is_arc(seg.bulge);
    // A tangent fillet's far vertex moves too: (index of the segment ending there, new point).
    let mut fillet_vertices: Vec<(usize, Point2)> = Vec::new();

    let prev_fillet =
        prev_seg.filter(|_| straight_moved).and_then(|f| slide_fillet(seg.from, f, beyond(f.index, true), true, moved_dir, delta).map(|r| (f, r)));
    let from = match prev_fillet {
        Some((f, (shared, far))) => {
            // The fillet's `from` is the end of the segment before it.
            fillet_vertices.push((if f.index > 0 { f.index - 1 } else { n - 1 }, far));
            SlidEnd { moved: shared, neighbor: None }
        }
        None => slide_endpoint(seg.from, prev_seg, true, moved_dir, delta),
    };
    let next_fillet =
        next_seg.filter(|_| straight_moved).and_then(|f| slide_fillet(seg.to, f, beyond(f.index, false), false, moved_dir, delta).map(|r| (f, r)));
    let to = match next_fillet {
        Some((f, (shared, far))) => {
            fillet_vertices.push((f.index, far));
            SlidEnd { moved: shared, neighbor: None }
        }
        None => slide_endpoint(seg.to, next_seg, false, moved_dir, delta),
    };
    // The neighbours' new shared ends: the moved segment's own ends unless a vertex is doubled.
    let from_point = from.moved;
    let to_point = to.moved;
    let from_neighbor = from.neighbor.unwrap_or(from_point);
    let to_neighbor = to.neighbor.unwrap_or(to_point);

    let mut segments = poly.segments.clone();
    let mut start = poly.start;
    for (i, point) in fillet_vertices {
        if let Some(s) = segments.get_mut(i) {
            s.to = point;
        }
        if wrap && i == n - 1 {
            start = point;
        }
    }

    if segment_index == 0 {
        start = from_point;
        if wrap && let Some(s) = segments.get_mut(n - 1) {
            s.to = from_neighbor;
        }
    } else if let Some(s) = segments.get_mut(segment_index - 1) {
        s.to = from_neighbor;
    }

    if let Some(s) = segments.get_mut(segment_index) {
        s.to = to_point;
        s.bulge = seg.bulge;
    }
    if wrap && segment_index == n - 1 {
        start = to_neighbor;
    }

    // T573: insert the connectors at doubled vertices. One across a closed polyline's start vertex
    // is appended, so no existing index shifts; the others go beside the moved segment.
    let connector = |to: Point2| PolySegment { to, bulge: 0.0, corner_at_to: None };
    let mut appended: Vec<Point2> = Vec::new();
    // (insert before this ORIGINAL index, connector end), ascending by index.
    let mut beside: Vec<(usize, Point2)> = Vec::new();
    if from.neighbor.is_some() {
        if segment_index == 0 {
            appended.push(from_point);
        } else {
            beside.push((segment_index, from_point));
        }
    }
    if to.neighbor.is_some() {
        if segment_index == n - 1 {
            appended.push(to_neighbor);
        } else {
            beside.push((segment_index + 1, to_neighbor));
        }
    }
    let mut inserted: Vec<usize> = Vec::new();
    for (shift, &(k, point)) in beside.iter().enumerate() {
        // Each earlier connector pushed this original index up by one.
        segments.insert(k + shift, connector(point));
        inserted.push(k + shift);
    }
    for point in appended {
        inserted.push(segments.len());
        segments.push(connector(point));
    }

    SegmentSlideResult { poly: Polyline2 { start, segments, closed: poly.closed }, unreachable: None, inserted }
}

/// The bulge that makes an arc from `from` to `to` pass through `through`. Returns 0 (a straight
/// segment) when the three points are collinear.
pub fn bulge_from_through_point(from: Point2, to: Point2, through: Point2) -> f64 {
    let Some(circle) = circumcircle(from, to, through) else { return 0.0 };
    let center = circle.center;
    let start_angle = (from.y - center.y).atan2(from.x - center.x);
    let end_angle = (to.y - center.y).atan2(to.x - center.x);
    let through_angle = (through.y - center.y).atan2(through.x - center.x);

    let two_pi = PI * 2.0;
    let delta_ccw = ((end_angle - start_angle) % two_pi + two_pi) % two_pi;
    let through_ccw = ((through_angle - start_angle) % two_pi + two_pi) % two_pi;

    // If sweeping CCW from start reaches `through` before (or at) `end`, that's the arc that
    // contains it; otherwise `through` sits on the complementary (CW) arc.
    let delta = if through_ccw <= delta_ccw + 1e-9 { delta_ccw } else { delta_ccw - two_pi };
    (delta / 4.0).tan()
}

/// T102: adjacent segments that are two halves of one smooth run merge into one (same circle the
/// same way round, or collinear straights). Callers run it on the RESULT of a splice.
pub fn merge_co_arc_segments(poly: &Polyline2) -> Polyline2 {
    merge_co_arc_segments_with_map(poly).poly
}

/// Where source segment `i` ended up: which merged segment, and the sub-range of it that segment
/// became.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentMergeMap {
    pub new_index: usize,
    pub t0: f64,
    pub t1: f64,
}

/// `merge_co_arc_segments_with_map`'s result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergedPolyline {
    pub poly: Polyline2,
    pub map: Vec<SegmentMergeMap>,
}

/// The mapping half of [`merge_co_arc_segments`]: an anchor at param `p` on source segment `i`
/// lands at `t0 + p * (t1 - t0)` on merged segment `new_index`.
pub fn merge_co_arc_segments_with_map(poly: &Polyline2) -> MergedPolyline {
    let resolved = resolved_segments(poly);
    let n = resolved.len();
    let mut map: Vec<SegmentMergeMap> = Vec::with_capacity(n);
    if n == 0 {
        return MergedPolyline { poly: Polyline2 { segments: Vec::new(), ..poly.clone() }, map };
    }

    let angle_tol = (TOLERANCE.angle_deg * PI) / 180.0;
    let mut segments: Vec<PolySegment> = Vec::new();

    // Emit source segments [from, to_exclusive) as one segment, and record their sub-ranges in it.
    let mut flush = |from: usize, to_exclusive: usize, segments: &mut Vec<PolySegment>| {
        let new_index = segments.len();
        let (Some(first), Some(last)) = (poly.segments.get(from), poly.segments.get(to_exclusive - 1)) else { return };
        let mut bulge = first.bulge;
        if to_exclusive - from > 1 && is_arc(first.bulge) {
            bulge = (group_sweep(&resolved, from, to_exclusive) / 4.0).tan();
        }
        segments.push(PolySegment { to: last.to, bulge, corner_at_to: last.corner_at_to });

        // Parameter along the merged segment is proportional to arc length, and within a group
        // the radius is constant — so length shares out the range for straights and arcs alike.
        let lengths: Vec<f64> = resolved[from..to_exclusive].iter().map(segment_length).collect();
        let total: f64 = lengths.iter().sum();
        let mut acc = 0.0;
        for len in &lengths {
            let share = if total < 1e-12 { 0.0 } else { len / total };
            map.push(SegmentMergeMap { new_index, t0: acc, t1: acc + share });
            acc += share;
        }
        if let Some(m) = map.last_mut() {
            m.t1 = 1.0;
        }
    };

    let mut group_start = 0;
    for i in 1..n {
        if continues_group(poly, &resolved, group_start, i, angle_tol) {
            continue;
        }
        flush(group_start, i, &mut segments);
        group_start = i;
    }
    flush(group_start, n, &mut segments);
    // The wrap pair is deliberately never considered: on a circle merging it would destroy the
    // two-arc representation circle-ness is derived from.
    MergedPolyline { poly: Polyline2 { segments, ..poly.clone() }, map }
}

/// Total signed sweep of source segments [from, to_exclusive), which are all arcs.
fn group_sweep(resolved: &[ResolvedSegment], from: usize, to_exclusive: usize) -> f64 {
    resolved[from..to_exclusive]
        .iter()
        .filter_map(|s| arc_from_bulge(s.from, s.to, s.bulge))
        .map(|arc| sweep_angle(arc.start_angle, arc.end_angle, arc.ccw))
        .sum()
}

/// Does segment `i` continue the run that started at `group_start` — same line, or same circle
/// the same way round?
fn continues_group(poly: &Polyline2, resolved: &[ResolvedSegment], group_start: usize, i: usize, angle_tol: f64) -> bool {
    // A tagged vertex is a filleted or chamfered corner; merging it away would delete the fillet.
    if poly.segments.get(i - 1).is_some_and(|p| p.corner_at_to.is_some()) {
        return false;
    }
    let (Some(head), Some(next)) = (resolved.get(group_start), resolved.get(i)) else { return false };
    if is_arc(head.bulge) != is_arc(next.bulge) {
        return false;
    }

    if !is_arc(head.bulge) {
        let da = normalize2(sub2(head.to, head.from));
        let db = normalize2(sub2(next.to, next.from));
        return (da.x * db.y - da.y * db.x).atan2(da.x * db.x + da.y * db.y).abs() <= angle_tol;
    }

    let (Some(arc_a), Some(arc_b)) = (arc_from_bulge(head.from, head.to, head.bulge), arc_from_bulge(next.from, next.to, next.bulge)) else {
        return false;
    };
    if arc_a.ccw != arc_b.ccw {
        return false;
    }
    if dist2(arc_a.center, arc_b.center) > TOLERANCE.coincidence_mm {
        return false;
    }
    if (arc_a.radius - arc_b.radius).abs() > TOLERANCE.coincidence_mm {
        return false;
    }
    // A full turn's bulge is infinite: a whole circle's two semicircles must stay two.
    group_sweep(resolved, group_start, i + 1).abs() < PI * 2.0 - 1e-6
}

pub const GEOMETRY_TOLERANCE: f64 = TOLERANCE.coincidence_mm;

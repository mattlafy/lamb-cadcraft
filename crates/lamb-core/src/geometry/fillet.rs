//! FILLET / CHAMFER corner geometry: straight corners, arc-adjacent corners, ends-mode apparent
//! intersections and gap bridges, and live virtual corners. Port of
//! `geometry-primitives/src/fillet.ts`.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use serde::{Deserialize, Serialize};

use super::intersect::{infinite_line_intersect, intersect_line_with_boundary_circle};
use super::point::{Point2, add2, cross2, dist2, dot2, normalize2, scale2, sub2};
use super::polyline::{ArcGeometry, PolySegment, Polyline2, VirtualCornerEntry, arc_from_bulge, sweep_angle, virtual_corner_entries};
use super::tangent_circle::{ApparentCurve, circle_circle_intersect, tangent_circles};
use super::tolerance::{deg_to_rad, rad_to_deg};

/// JavaScript's `Math.sign`: -1, 0 or 1 (NaN stays NaN; unlike `f64::signum`, 0 maps to 0).
fn js_sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        x
    }
}

/// `Math.sign(x) || 1`: 0 and NaN become 1.
fn sign_or_one(x: f64) -> f64 {
    let s = js_sign(x);
    if s == 0.0 || s.is_nan() { 1.0 } else { s }
}

/// The bulge of the arc joining two edges whose tangent directions (each heading away from the
/// joint) are `dir_in`/`dir_out` — shared tail of the fillet solvers.
fn bulge_between(dir_in: Point2, dir_out: Point2) -> f64 {
    let path_in = scale2(dir_in, -1.0);
    let turns_ccw = cross2(path_in, dir_out) > 0.0;
    let cos_theta = dot2(dir_in, dir_out).clamp(-1.0, 1.0);
    let included_angle_deg = 180.0 - rad_to_deg(cos_theta.acos());
    let signed_included_deg = if turns_ccw { included_angle_deg } else { -included_angle_deg };
    (deg_to_rad(signed_included_deg) / 4.0).tan()
}

/// FILLET of a straight corner: the trim points, the arc's bulge and centre.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilletResult {
    /// New endpoint on the p0-p1 edge.
    pub trim_p0: Point2,
    /// New endpoint on the p1-p2 edge.
    pub trim_p2: Point2,
    /// Arc segment from `trim_p0` to `trim_p2`.
    pub bulge: f64,
    pub arc_center: Point2,
    pub radius: f64,
}

/// FILLET: replace the sharp corner at p1 (between edges p0-p1 and p1-p2) with a tangent arc of
/// the given radius. Straight-line corners only.
pub fn fillet_corner(p0: Point2, p1: Point2, p2: Point2, radius: f64) -> Option<FilletResult> {
    if radius < 0.0 {
        return None;
    }
    let d1 = normalize2(sub2(p0, p1));
    let d2 = normalize2(sub2(p2, p1));
    let cos_theta = dot2(d1, d2).clamp(-1.0, 1.0);
    let theta = cos_theta.acos();
    if theta < 1e-6 || theta > PI - 1e-6 {
        return None; // collinear or coincident edges
    }

    // T72: radius 0 is the square corner — the corner itself, always fits.
    if radius == 0.0 {
        return Some(FilletResult { trim_p0: p1, trim_p2: p1, bulge: 0.0, arc_center: p1, radius: 0.0 });
    }

    let dist = radius / (theta / 2.0).tan();
    let edge1_len = (p0.x - p1.x).hypot(p0.y - p1.y);
    let edge2_len = (p2.x - p1.x).hypot(p2.y - p1.y);
    if dist >= edge1_len || dist >= edge2_len {
        return None; // radius too large for these edges
    }

    let trim_p0 = add2(p1, scale2(d1, dist));
    let trim_p2 = add2(p1, scale2(d2, dist));

    let bisector = normalize2(Point2 { x: d1.x + d2.x, y: d1.y + d2.y });
    let center_dist = radius / (theta / 2.0).sin();
    let arc_center = add2(p1, scale2(bisector, center_dist));

    let path_in = normalize2(sub2(p1, p0));
    let path_out = normalize2(sub2(p2, p1));
    let turns_ccw = cross2(path_in, path_out) > 0.0;
    let included_angle_deg = 180.0 - rad_to_deg(theta);
    let signed_included_deg = if turns_ccw { included_angle_deg } else { -included_angle_deg };
    let bulge = (deg_to_rad(signed_included_deg) / 4.0).tan();

    Some(FilletResult { trim_p0, trim_p2, bulge, arc_center, radius })
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChamferResult {
    pub trim_p0: Point2,
    pub trim_p2: Point2,
}

/// CHAMFER: replace the corner at p1 with a straight cut at the given distances along each edge.
pub fn chamfer_corner(p0: Point2, p1: Point2, p2: Point2, dist1: f64, dist2: f64) -> Option<ChamferResult> {
    let edge1_len = (p0.x - p1.x).hypot(p0.y - p1.y);
    let edge2_len = (p2.x - p1.x).hypot(p2.y - p1.y);
    // T72: both distances 0 is the square corner; ONE of them 0 stays an error.
    if dist1 < 0.0 || dist2 < 0.0 {
        return None;
    }
    if dist1 == 0.0 || dist2 == 0.0 {
        return if dist1 == 0.0 && dist2 == 0.0 { Some(ChamferResult { trim_p0: p1, trim_p2: p1 }) } else { None };
    }
    if dist1 >= edge1_len || dist2 >= edge2_len {
        return None;
    }
    let d1 = normalize2(sub2(p0, p1));
    let d2 = normalize2(sub2(p2, p1));
    Some(ChamferResult { trim_p0: add2(p1, scale2(d1, dist1)), trim_p2: add2(p1, scale2(d2, dist2)) })
}

// -------------------------------------------------- T12: arc-adjacent corners --
// FILLET at a corner where one or both neighbouring segments are ARCS, via the standard
// offset-and-intersect tangent-circle method; each candidate centre is validated and the local one
// (same side as the corner's bisector, nearest the corner) wins.

/// One edge at a corner: a straight edge's direction away from the corner, or the arc it lies on
/// (oriented to start at the corner).
#[derive(Debug, Clone, Copy)]
enum ArcCornerEdge {
    Line { dir: Point2 },
    Arc(ArcGeometry),
}

fn arc_tangent_at(arc: &ArcGeometry, angle: f64) -> Point2 {
    let radial = Point2 { x: angle.cos(), y: angle.sin() };
    let ccw_tangent = Point2 { x: -radial.y, y: radial.x };
    if arc.ccw { ccw_tangent } else { scale2(ccw_tangent, -1.0) }
}

/// `far`/`bulge` describe the ORIGINAL segment touching `p1` (`reversed` = original direction was
/// far->p1); returns the edge shape plus the tangent direction of travel AT p1, heading away from
/// the corner into the edge. TS asserts the arc exists; a degenerate chord is treated as straight.
fn arc_corner_edge(p1: Point2, far: Point2, bulge: f64, reversed: bool) -> (ArcCornerEdge, Point2) {
    if bulge != 0.0
        && let Some(arc) = arc_from_bulge(p1, far, if reversed { -bulge } else { bulge })
    {
        return (ArcCornerEdge::Arc(arc), arc_tangent_at(&arc, arc.start_angle));
    }
    let dir = normalize2(sub2(far, p1));
    (ArcCornerEdge::Line { dir }, dir)
}

/// The edge as the self-contained `ApparentCurve` the shared solver takes; a line is anchored at
/// `anchor`.
fn edge_to_apparent_curve(anchor: Point2, edge: &ArcCornerEdge) -> ApparentCurve {
    match edge {
        ArcCornerEdge::Line { dir } => ApparentCurve::Line { from: anchor, to: add2(anchor, *dir) },
        ArcCornerEdge::Arc(arc) => ApparentCurve::Circle { center: arc.center, radius: arc.radius },
    }
}

/// Thin wrapper over the shared `tangent_circles`: just the candidate centres.
fn tangent_circle_centers(p1_a: Point2, p1_b: Point2, edge_a: &ArcCornerEdge, edge_b: &ArcCornerEdge, r: f64) -> Vec<Point2> {
    tangent_circles(&edge_to_apparent_curve(p1_a, edge_a), &edge_to_apparent_curve(p1_b, edge_b), r).into_iter().map(|c| c.center).collect()
}

/// Signed angular difference `to - from`, normalised to `(-π, π]` — the SHORTEST way around.
fn shortest_angle_diff(from: f64, to: f64) -> f64 {
    let two_pi = PI * 2.0;
    ((to - from + PI) % two_pi + two_pi) % two_pi - PI
}

/// Validates `c` as a tangent-circle centre against one edge, returning the tangent point and the
/// edge's own tangent direction there. `bound_near` also rejects a tangent point at or before `p1`
/// (corner mode); gap-bridge passes `false` so only the far end bounds it.
fn edge_tangent_point(p1: Point2, far: Point2, edge: &ArcCornerEdge, c: Point2, radius: f64, bound_near: bool) -> Option<(Point2, Point2)> {
    let eps = 1e-7;
    let arc = match edge {
        ArcCornerEdge::Line { dir } => {
            let t = dot2(sub2(c, p1), *dir);
            let edge_len = dist2(p1, far);
            if (bound_near && t <= eps) || t >= edge_len - eps {
                return None;
            }
            return Some((add2(p1, scale2(*dir, t)), *dir));
        }
        ArcCornerEdge::Arc(arc) => arc,
    };
    let dir0 = normalize2(sub2(c, arc.center));
    let toward = add2(arc.center, scale2(dir0, arc.radius));
    let away = add2(arc.center, scale2(dir0, -arc.radius));
    let point = if (dist2(c, toward) - radius).abs() <= (dist2(c, away) - radius).abs() { toward } else { away };
    let angle = (point.y - arc.center.y).atan2(point.x - arc.center.x);
    if bound_near {
        let total = sweep_angle(arc.start_angle, arc.end_angle, arc.ccw);
        let rel = sweep_angle(arc.start_angle, angle, arc.ccw);
        let out_of_range_near = if arc.ccw { rel <= eps } else { rel >= -eps };
        let out_of_range_far = if arc.ccw { rel >= total - eps } else { rel <= total + eps };
        if out_of_range_near || out_of_range_far {
            return None;
        }
    } else {
        // Gap-bridge (T83): reject only a candidate on far's own side of near that has gone at
        // least as far as far itself.
        let far_diff = shortest_angle_diff(arc.start_angle, arc.end_angle);
        let cand_diff = shortest_angle_diff(arc.start_angle, angle);
        let same_direction_as_far = js_sign(far_diff) == js_sign(cand_diff);
        if same_direction_as_far && cand_diff.abs() >= far_diff.abs() - eps {
            return None;
        }
    }
    Some((point, arc_tangent_at(arc, angle)))
}

/// Live FILLET geometry at a corner where at least one neighbouring segment is curved. Same
/// contract as `fillet_corner`: `None` if the radius doesn't fit or the edges meet
/// tangentially/collinearly.
pub fn resolve_arc_fillet_corner(p0: Point2, p1: Point2, p2: Point2, prev_bulge: f64, next_bulge: f64, radius: f64) -> Option<VirtualCornerResult> {
    if radius < 0.0 {
        return None;
    }
    let (in_edge, d1) = arc_corner_edge(p1, p0, prev_bulge, true);
    let (out_edge, d2) = arc_corner_edge(p1, p2, next_bulge, false);
    let cos_theta = dot2(d1, d2).clamp(-1.0, 1.0);
    let theta = cos_theta.acos();
    if theta < 1e-6 || theta > PI - 1e-6 {
        return None; // tangential/collinear: no real corner
    }
    // T72: radius 0 is the corner itself.
    if radius == 0.0 {
        return Some(VirtualCornerResult { trim_p0: p1, trim_p2: p1, bulge: 0.0, arc_center: Some(p1) });
    }
    let bisector = normalize2(Point2 { x: d1.x + d2.x, y: d1.y + d2.y });

    // (center, trim_in, trim_out, dir_in, dir_out, dist_to_p1)
    let mut best: Option<(Point2, Point2, Point2, Point2, Point2, f64)> = None;
    for c in tangent_circle_centers(p1, p1, &in_edge, &out_edge, radius) {
        if dot2(sub2(c, p1), bisector) <= 0.0 {
            continue;
        }
        let Some((trim_in, dir_in)) = edge_tangent_point(p1, p0, &in_edge, c, radius, true) else { continue };
        let Some((trim_out, dir_out)) = edge_tangent_point(p1, p2, &out_edge, c, radius, true) else { continue };
        let dist_to_p1 = dist2(c, p1);
        if best.is_none_or(|b| dist_to_p1 < b.5) {
            best = Some((c, trim_in, trim_out, dir_in, dir_out, dist_to_p1));
        }
    }
    let (center, trim_in, trim_out, dir_in, dir_out, _) = best?;
    Some(VirtualCornerResult { trim_p0: trim_in, trim_p2: trim_out, bulge: bulge_between(dir_in, dir_out), arc_center: Some(center) })
}

// ------------------------------------------------ T12: ends-mode arc support --

/// The full curve an edge sits on. TS asserts the arc exists; a degenerate chord comes back as a
/// line.
pub fn curve_from_edge(p0: Point2, p1: Point2, bulge: f64) -> ApparentCurve {
    if bulge != 0.0
        && let Some(arc) = arc_from_bulge(p0, p1, bulge)
    {
        return ApparentCurve::Circle { center: arc.center, radius: arc.radius };
    }
    ApparentCurve::Line { from: p0, to: p1 }
}

/// Every point where two edges' FULL curves cross — 0, 1 (tangent), or 2 points.
pub fn apparent_curve_intersections(a: &ApparentCurve, b: &ApparentCurve) -> Vec<Point2> {
    match (*a, *b) {
        (ApparentCurve::Line { from: a0, to: a1 }, ApparentCurve::Line { from: b0, to: b1 }) => {
            infinite_line_intersect(a0, a1, b0, b1).map(|h| vec![h.point]).unwrap_or_default()
        }
        (ApparentCurve::Line { from, to }, ApparentCurve::Circle { center, radius })
        | (ApparentCurve::Circle { center, radius }, ApparentCurve::Line { from, to }) => {
            intersect_line_with_boundary_circle(from, to, center, radius)
        }
        (ApparentCurve::Circle { center: c0, radius: r0 }, ApparentCurve::Circle { center: c1, radius: r1 }) => {
            circle_circle_intersect(c0, r0, c1, r1)
        }
    }
}

/// Scalar position of `point` along an edge's own p0->p1 direction: 0 at p0, 1 at p1, unclamped —
/// an arc sweeps the same rotational sense as its bulge.
pub fn param_along_edge(p0: Point2, p1: Point2, bulge: f64, point: Point2) -> f64 {
    let arc = if bulge != 0.0 { arc_from_bulge(p0, p1, bulge) } else { None };
    let Some(arc) = arc else {
        let dir = sub2(p1, p0);
        let len2 = dot2(dir, dir);
        return if len2 < 1e-12 { 0.0 } else { dot2(sub2(point, p0), dir) / len2 };
    };
    let total_sweep = sweep_angle(arc.start_angle, arc.end_angle, arc.ccw);
    if total_sweep.abs() < 1e-12 {
        return 0.0;
    }
    let angle = (point.y - arc.center.y).atan2(point.x - arc.center.x);
    sweep_angle(arc.start_angle, angle, arc.ccw) / total_sweep
}

/// The single point BOTH picked edges should meet at, from the apparent-intersection candidates
/// `pts`. One candidate wins outright; with two, each edge picks whichever is nearer its own click
/// (along `param_along_edge`), and disagreement is `None`.
#[allow(clippy::too_many_arguments)]
pub fn choose_shared_corner(
    pts: &[Point2],
    p0_a: Point2,
    p1_a: Point2,
    bulge_a: f64,
    click_a: Point2,
    p0_b: Point2,
    p1_b: Point2,
    bulge_b: f64,
    click_b: Point2,
) -> Option<Point2> {
    match pts.len() {
        0 => return None,
        1 => return Some(pts[0]),
        _ => {}
    }
    let nearest = |p0: Point2, p1: Point2, bulge: f64, click: Point2| {
        let click_param = param_along_edge(p0, p1, bulge, click);
        let off = |p: Point2| (param_along_edge(p0, p1, bulge, p) - click_param).abs();
        let mut best = 0;
        for (i, &p) in pts.iter().enumerate() {
            if off(p) < off(pts[best]) {
                best = i;
            }
        }
        best
    };
    let chosen_a = nearest(p0_a, p1_a, bulge_a, click_a);
    let chosen_b = nearest(p0_b, p1_b, bulge_b, click_b);
    if chosen_a == chosen_b { Some(pts[chosen_a]) } else { None }
}

/// Which side of an edge survives: `Before` keeps p0's side, `After` keeps p1's side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Keep {
    Before,
    After,
}

/// The portion you click on is the portion that survives, given the chosen corner's param `t`.
pub fn keep_for_param(t: f64, click_param: f64) -> Keep {
    if t <= 0.0 {
        return Keep::After;
    }
    if t >= 1.0 {
        return Keep::Before;
    }
    if click_param < t { Keep::Before } else { Keep::After }
}

/// One edge as picked in ends mode: its two endpoints and bulge, plus the world point clicked.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndsPick {
    pub p0: Point2,
    pub p1: Point2,
    pub bulge: f64,
    pub click: Point2,
}

/// `resolve_ends_keeps`'s result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndsKeeps {
    pub keep_a: Keep,
    pub keep_b: Keep,
}

/// Resolves BOTH picks' `keep` from where the two edges actually meet; with no shared corner each
/// click falls back to its own half of its own edge.
pub fn resolve_ends_keeps(a: &EndsPick, b: &EndsPick) -> EndsKeeps {
    let pts = apparent_curve_intersections(&curve_from_edge(a.p0, a.p1, a.bulge), &curve_from_edge(b.p0, b.p1, b.bulge));
    let corner = choose_shared_corner(&pts, a.p0, a.p1, a.bulge, a.click, b.p0, b.p1, b.bulge, b.click);
    let keep_for = |e: &EndsPick| {
        let click_param = param_along_edge(e.p0, e.p1, e.bulge, e.click);
        match corner {
            None => {
                if click_param < 0.5 {
                    Keep::Before
                } else {
                    Keep::After
                }
            }
            Some(corner) => keep_for_param(param_along_edge(e.p0, e.p1, e.bulge, corner), click_param),
        }
    };
    EndsKeeps { keep_a: keep_for(a), keep_b: keep_for(b) }
}

/// The bulge for an arc on a KNOWN circle whose endpoints have moved to `from`/`to`.
pub fn bulge_for_arc(center: Point2, ccw: bool, from: Point2, to: Point2) -> f64 {
    let start_angle = (from.y - center.y).atan2(from.x - center.x);
    let end_angle = (to.y - center.y).atan2(to.x - center.x);
    (sweep_angle(start_angle, end_angle, ccw) / 4.0).tan()
}

/// T12 ends-mode, 0-intersection case: a real arc spliced between the two edges' near ends.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GapBridgeResult {
    pub trim_near_a: Point2,
    pub trim_near_b: Point2,
    /// The bridging arc, `trim_near_a` -> `trim_near_b`.
    pub bulge: f64,
    pub arc_center: Point2,
}

/// One edge's side of a gap-bridge call: `near` is the end being replaced, `far` the fixed anchor,
/// `reversed` whether `near` is the segment's own `to`, `click` the world point picked.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GapBridgeSide {
    pub near: Point2,
    pub far: Point2,
    pub bulge: f64,
    pub reversed: bool,
    pub click: Point2,
}

/// T83: picks among up to 4 tangent circles by the summed perpendicular offset of both clicks from
/// the axis through the two near ends; a leftover tie within that family falls back to nearest.
pub fn resolve_gap_bridge(a: &GapBridgeSide, b: &GapBridgeSide, radius: f64) -> Option<GapBridgeResult> {
    if radius <= 0.0 {
        return None;
    }
    let (edge_a, _) = arc_corner_edge(a.near, a.far, a.bulge, a.reversed);
    let (edge_b, _) = arc_corner_edge(b.near, b.far, b.bulge, b.reversed);

    let ref_dir = normalize2(sub2(b.near, a.near));
    let axis_offset = |point: Point2, near: Point2| cross2(ref_dir, sub2(point, near));
    let want_side = sign_or_one(axis_offset(a.click, a.near) + axis_offset(b.click, b.near));

    struct Candidate {
        center: Point2,
        pt_a: Point2,
        pt_b: Point2,
        dir_a: Point2,
        dir_b: Point2,
        score: f64,
        side: f64,
    }
    let mut valid: Vec<Candidate> = Vec::new();
    for c in tangent_circle_centers(a.near, b.near, &edge_a, &edge_b, radius) {
        let Some((pt_a, dir_a)) = edge_tangent_point(a.near, a.far, &edge_a, c, radius, false) else { continue };
        let Some((pt_b, dir_b)) = edge_tangent_point(b.near, b.far, &edge_b, c, radius, false) else { continue };
        let score = dist2(a.near, pt_a) + dist2(b.near, pt_b);
        valid.push(Candidate { center: c, pt_a, pt_b, dir_a, dir_b, score, side: sign_or_one(axis_offset(c, a.near)) });
    }

    let matching: Vec<&Candidate> = valid.iter().filter(|c| c.side == want_side).collect();
    let pool: Vec<&Candidate> = if matching.is_empty() { valid.iter().collect() } else { matching };
    let mut best = *pool.first()?; // existence: no tangent circle of this radius fits at all
    for &c in &pool {
        if c.score < best.score {
            best = c;
        }
    }

    Some(GapBridgeResult { trim_near_a: best.pt_a, trim_near_b: best.pt_b, bulge: bulge_between(best.dir_a, best.dir_b), arc_center: best.center })
}

// ------------------------------------------------------- T9: virtual corners --

/// Normalised fillet-or-chamfer corner geometry, live-derived from a tagged vertex's current
/// neighbours. `arc_center` is only present for a fillet.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualCornerResult {
    pub trim_p0: Point2,
    pub trim_p2: Point2,
    pub bulge: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arc_center: Option<Point2>,
}

fn polyline_points(poly: &Polyline2) -> Vec<Point2> {
    std::iter::once(poly.start).chain(poly.segments.iter().map(|s| s.to)).collect()
}

fn insert_result_for_entry(entry: &VirtualCornerEntry, p0: Point2, p1: Point2, p2: Point2) -> Option<VirtualCornerResult> {
    match *entry {
        VirtualCornerEntry::Fillet { radius } => fillet_corner(p0, p1, p2, radius).map(|r| VirtualCornerResult {
            trim_p0: r.trim_p0,
            trim_p2: r.trim_p2,
            bulge: r.bulge,
            arc_center: Some(r.arc_center),
        }),
        VirtualCornerEntry::Chamfer { distance1, distance2 } => chamfer_corner(p0, p1, p2, distance1, distance2).map(|r| VirtualCornerResult {
            trim_p0: r.trim_p0,
            trim_p2: r.trim_p2,
            bulge: 0.0,
            arc_center: None,
        }),
    }
}

/// Live fillet/chamfer geometry for `poly`'s vertex `corner_index`, from its CURRENT neighbours —
/// `None` if that vertex isn't an interior (or closed-wrap) corner, a curved neighbour has a
/// chamfer, or the radius/distance doesn't fit.
pub fn resolve_virtual_corner(poly: &Polyline2, corner_index: usize, entry: &VirtualCornerEntry) -> Option<VirtualCornerResult> {
    let n = poly.segments.len();
    let wrap = poly.closed && corner_index == 0 && n >= 2;
    let points = polyline_points(poly);
    if !wrap && (corner_index < 1 || corner_index + 2 > points.len()) {
        return None;
    }
    let prev_seg = if wrap { poly.segments.get(n - 1)? } else { poly.segments.get(corner_index - 1)? };
    let next_seg = poly.segments.get(corner_index)?;
    let p0 = if wrap { *points.get(points.len() - 2)? } else { *points.get(corner_index - 1)? };
    let p1 = *points.get(corner_index)?;
    let p2 = *points.get(corner_index + 1)?;
    if prev_seg.bulge != 0.0 || next_seg.bulge != 0.0 {
        // T12: a curved neighbour only has live geometry for FILLET.
        let VirtualCornerEntry::Fillet { radius } = *entry else { return None };
        return resolve_arc_fillet_corner(p0, p1, p2, prev_seg.bulge, next_seg.bulge, radius);
    }
    insert_result_for_entry(entry, p0, p1, p2)
}

/// The polyline as it should be RENDERED/PICKED/SNAPPED: every tagged corner with a feasible result
/// has its sharp vertex swapped for a trim-point pair + virtual arc/chamfer segment. Each corner is
/// resolved independently against `poly`'s own pristine edges.
pub fn resolve_virtual_polyline(poly: &Polyline2) -> Polyline2 {
    let n = poly.segments.len();
    let mut results: BTreeMap<usize, VirtualCornerResult> = BTreeMap::new();
    for ce in virtual_corner_entries(poly) {
        if let Some(r) = resolve_virtual_corner(poly, ce.corner_index, &ce.entry) {
            results.insert(ce.corner_index, r);
        }
    }
    if results.is_empty() {
        return poly.clone();
    }

    let wrap_result = if poly.closed { results.get(&0).copied() } else { None };
    let new_start = wrap_result.map_or(poly.start, |r| r.trim_p2);
    let mut new_segments: Vec<PolySegment> = Vec::with_capacity(n + results.len());
    for (i, seg) in poly.segments.iter().enumerate() {
        let end_vertex = i + 1;
        let end_result = if end_vertex < n { results.get(&end_vertex).copied() } else { wrap_result };
        match end_result {
            Some(r) => {
                new_segments.push(PolySegment { to: r.trim_p0, bulge: seg.bulge, corner_at_to: None });
                new_segments.push(PolySegment { to: r.trim_p2, bulge: r.bulge, corner_at_to: None });
            }
            None => new_segments.push(*seg),
        }
    }
    Polyline2 { start: new_start, segments: new_segments, closed: poly.closed }
}

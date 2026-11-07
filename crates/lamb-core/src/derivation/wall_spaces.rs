//! The wall graph, fourth slice: **inside and outside** (LAMB's
//! `docs/07-architecture/cadcraft-integration.md` §8, phase 7). The graph's centrelines — runs plus
//! opening bridges, meeting at junction points — form a planar arrangement. Its bounded faces are
//! **spaces**; runs bordering the unbounded face are **exterior**. Computed on demand, never stored.
//!
//! Vertices are the junction points plus every run end that joined no junction (its centreline end
//! point). Edges are the runs and the bridges. Faces are traced over half-edges with the face on
//! their left: at a half-edge's head, `next` is the outgoing half-edge immediately clockwise from
//! its twin. Per connected component, the face with the smallest signed area is that component's
//! outer face → `Outside`; a tree of walls (a garden wall, a room with an undrawn gap) has a single
//! face, which is outer — a structure that doesn't close has no inside.
//!
//! Overrides, passed as [`SideParams`] until a later task stores them: an outside pick point turns
//! the smallest space containing it into `Outside` (a courtyard); a run with a face on an exterior
//! layer is `exterior` whatever its sides say. A structure that doesn't close (both sides of a run
//! `Outside`) takes a straight or arc run's exterior side from the nearest outside pick point beside
//! it ([`RunSides::open_outside_left`]).
//!
//! Not handled: a structure standing inside another's room (a separate component nested in a
//! space) — its outer face is reported `Outside` and the enclosing space does not know about it.

use std::cmp::Ordering;

use super::read::ProjectRead;
use super::wall_junctions::{RunEnd, WallGraph, run_ends};
use super::walls::RunCurve;
use crate::geometry::Point2;
use crate::geometry::point::{angle_of2, cross2, dist2, dot2, scale2, sub2};
use crate::geometry::tolerance::TOLERANCE;
use crate::geometry::triangulate::{point_in_loop, polygon_area_signed};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideKind {
    Outside,
    /// Index into [`WallSides::spaces`].
    Space(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunSides {
    /// The side to the left of the centreline's direction of travel.
    pub left: SideKind,
    pub right: SideKind,
    /// A side is `Outside`, or a face is on an exterior layer.
    pub exterior: bool,
    /// For a straight or arc run with both sides `Outside`: whether the nearest outside pick point
    /// beside it lies to the left. `None` when no point resolves it, and for every other run.
    /// A caller may also set it on any run to give that run an explicit side (the LAMB fork's
    /// detail-tag sides); [`outside_on_left`] reads it first.
    pub open_outside_left: Option<bool>,
}

/// Whether the run's exterior side is the left of its centreline's travel: an explicit side in
/// [`RunSides::open_outside_left`] first — the nearest outside point on an open run, or the side of
/// the detail tag governing the run (set by the caller after [`wall_sides`]) — else the single
/// `Outside` side. `None` when undecided.
pub fn outside_on_left(sides: &RunSides) -> Option<bool> {
    if let Some(left) = sides.open_outside_left {
        return Some(left);
    }
    match (sides.left, sides.right) {
        (SideKind::Outside, SideKind::Space(_)) => Some(true),
        (SideKind::Space(_), SideKind::Outside) => Some(false),
        _ => None,
    }
}

/// The side (left = `true`) of the straight centreline `from → to` holding the nearest of `points`
/// beside the run: projecting within its extent (± `TOLERANCE.grouping_mm`) and off its faces.
fn open_side(from: Point2, to: Point2, width: f64, points: &[Point2]) -> Option<bool> {
    let len = dist2(from, to);
    if len <= TOLERANCE.coincidence_mm {
        return None;
    }
    let dir = scale2(sub2(to, from), 1.0 / len);
    let g = TOLERANCE.grouping_mm;
    let mut best: Option<(f64, bool)> = None;
    for &q in points {
        let v = sub2(q, from);
        let t = dot2(v, dir);
        let c = cross2(dir, v);
        if t < -g || t > len + g || c.abs() <= width / 2.0 + TOLERANCE.coincidence_mm {
            continue;
        }
        if best.is_none_or(|(d, _)| c.abs() < d) {
            best = Some((c.abs(), c > 0.0));
        }
    }
    best.map(|(_, left)| left)
}

/// The side (left = `true`) of the arc centreline about `center` holding the nearest of `points`
/// beside the run: within its angular span (widened at each end by `TOLERANCE.grouping_mm` of arc
/// length) and off its faces. Left of a ccw arc's travel is toward the centre.
fn open_side_arc(center: Point2, radius: f64, start_angle: f64, sweep: f64, width: f64, points: &[Point2]) -> Option<bool> {
    if radius <= TOLERANCE.coincidence_mm || sweep.abs() <= f64::EPSILON {
        return None;
    }
    let tau = std::f64::consts::TAU;
    let w = TOLERANCE.grouping_mm / radius;
    let mut best: Option<(f64, bool)> = None;
    for &q in points {
        let r = dist2(q, center);
        let off = (r - radius).abs();
        if off <= width / 2.0 + TOLERANCE.coincidence_mm {
            continue;
        }
        // Angle from the start, measured in the direction of travel, in [0, 2π).
        let a = angle_of2(sub2(q, center));
        let d = (if sweep > 0.0 { a - start_angle } else { start_angle - a }).rem_euclid(tau);
        if d > sweep.abs() + w && d < tau - w {
            continue;
        }
        if best.is_none_or(|(b, _)| off < b) {
            best = Some((off, if sweep > 0.0 { r < radius } else { r > radius }));
        }
    }
    best.map(|(_, left)| left)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Space {
    /// Area enclosed by the centrelines (and bridges), mm².
    pub area_mm2: f64,
    /// Runs on its boundary, sorted, deduped.
    pub runs: Vec<usize>,
    /// The traced loop of centreline (and bridge) edges, in order, with the space on their left.
    pub boundary: Vec<BoundaryEdge>,
}

/// One edge of a [`Space`]'s boundary: a run's or a bridge's centreline, traversed with the space
/// on its left.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundaryEdge {
    /// Tail to head; an arc as its sampled points.
    pub points: Vec<Point2>,
    /// Half the wall's width: a run's `width / 2`, a bridge's that of its run `a`.
    pub half_width: f64,
    /// `Some(run)` for a run's edge, `None` for a bridge.
    pub run: Option<usize>,
}

/// The space's centreline loop as one polygon (the space on its left).
pub fn boundary_points(space: &Space) -> Vec<Point2> {
    let mut poly = Vec::new();
    for e in &space.boundary {
        for &q in &e.points {
            push_point(&mut poly, q);
        }
    }
    if poly.len() > 1 && dist2(poly[0], poly[poly.len() - 1]) <= TOLERANCE.coincidence_mm {
        poly.pop();
    }
    poly
}

/// Whether `pt` lies inside the space's centreline loop.
pub fn space_contains(space: &Space, pt: Point2) -> bool {
    point_in_loop(&boundary_points(space), pt)
}

/// The area inside the walls' inner faces, mm²: every boundary segment offset left (into the space)
/// by its edge's `half_width`, consecutive offset segments joined at their lines' intersection
/// (parallel neighbours joined straight), then the polygon's signed area. An arc is offset as its
/// sampled points. Zero for a boundary of fewer than three segments.
pub fn inner_area_mm2(space: &Space) -> f64 {
    let mut segs: Vec<(Point2, Point2, f64)> = Vec::new();
    for e in &space.boundary {
        for w in e.points.windows(2) {
            if dist2(w[0], w[1]) > TOLERANCE.coincidence_mm {
                segs.push((w[0], w[1], e.half_width));
            }
        }
    }
    if segs.len() < 3 {
        return 0.0;
    }
    let off: Vec<(Point2, Point2)> = segs
        .iter()
        .map(|&(a, b, h)| {
            let len = dist2(a, b);
            let n = Point2 { x: -(b.y - a.y) / len * h, y: (b.x - a.x) / len * h };
            (Point2 { x: a.x + n.x, y: a.y + n.y }, Point2 { x: b.x + n.x, y: b.y + n.y })
        })
        .collect();
    let sin_tol = TOLERANCE.angle_deg.to_radians().sin();
    let mut poly = Vec::new();
    for (i, &(a0, a1)) in off.iter().enumerate() {
        let (b0, b1) = off[(i + 1) % off.len()];
        let (da, db) = (sub2(a1, a0), sub2(b1, b0));
        let denom = cross2(da, db);
        if denom.abs() <= sin_tol * dist2(a0, a1) * dist2(b0, b1) {
            push_point(&mut poly, a1);
            push_point(&mut poly, b0);
        } else {
            let t = cross2(sub2(b0, a0), db) / denom;
            push_point(&mut poly, Point2 { x: a0.x + da.x * t, y: a0.y + da.y * t });
        }
    }
    polygon_area_signed(&poly)
}

#[derive(Debug, Clone, PartialEq)]
pub struct WallSides {
    /// Parallel to `graph.runs`.
    pub runs: Vec<RunSides>,
    /// In order of each space's smallest run index, then area.
    pub spaces: Vec<Space>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SideParams {
    /// Each marks the smallest space containing it as `Outside`.
    pub outside_points: Vec<Point2>,
    /// A run with either face entity on one of these layers is exterior.
    pub exterior_layer_ids: Vec<String>,
}

/// Arc sampling step, radians (1°).
const ARC_STEP: f64 = std::f64::consts::PI / 180.0;

fn cmp_f64(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

fn push_point(path: &mut Vec<Point2>, p: Point2) {
    if path.last().is_none_or(|q| dist2(*q, p) > TOLERANCE.coincidence_mm) {
        path.push(p);
    }
}

fn centreline_points(c: &RunCurve) -> Vec<Point2> {
    match *c {
        RunCurve::Line { from, to } => vec![from, to],
        RunCurve::Arc { center, radius, start_angle, sweep } => {
            let n = ((sweep.abs() / ARC_STEP).ceil() as usize).max(1);
            (0..=n)
                .map(|i| {
                    let a = start_angle + sweep * i as f64 / n as f64;
                    Point2 { x: center.x + radius * a.cos(), y: center.y + radius * a.sin() }
                })
                .collect()
        }
    }
}

struct HalfEdge {
    tail: usize,
    head: usize,
    /// Out-direction at `tail`, radians in `[0, 2π)`.
    dir: f64,
    /// Points from tail to head (inclusive).
    path: Vec<Point2>,
    /// `Some(run)` for a run's half-edge.
    run: Option<usize>,
    half_width: f64,
}

/// Each run's left and right side and whether it is exterior, plus the spaces. See the module doc.
pub fn wall_sides(p: &impl ProjectRead, graph: &WallGraph, params: &SideParams) -> WallSides {
    let runs = &graph.runs;
    let ends = run_ends(runs);
    let end_info = |run: usize, end: RunEnd| &ends[2 * run + usize::from(end == RunEnd::End)];

    // Vertices: junctions first, then each unjoined run end.
    let mut vpoint: Vec<Point2> = graph.junctions.iter().map(|j| j.point).collect();
    let mut vertex_of = vec![[usize::MAX; 2]; runs.len()];
    for (k, j) in graph.junctions.iter().enumerate() {
        for a in &j.arms {
            vertex_of[a.run][usize::from(a.end == RunEnd::End)] = k;
        }
    }
    for (r, slots) in vertex_of.iter_mut().enumerate() {
        for (s, slot) in slots.iter_mut().enumerate() {
            if *slot == usize::MAX {
                *slot = vpoint.len();
                vpoint.push(end_info(r, if s == 0 { RunEnd::Start } else { RunEnd::End }).point);
            }
        }
    }
    let vert = |(run, end): (usize, RunEnd)| vertex_of[run][usize::from(end == RunEnd::End)];
    let norm = |a: f64| a.rem_euclid(std::f64::consts::TAU);

    // Half-edges: 2k forward, 2k + 1 its twin.
    let mut half: Vec<HalfEdge> = Vec::new();
    for (r, run) in runs.iter().enumerate() {
        let (vs, ve) = (vertex_of[r][0], vertex_of[r][1]);
        let mut path = Vec::new();
        push_point(&mut path, vpoint[vs]);
        for q in centreline_points(&run.centreline) {
            push_point(&mut path, q);
        }
        push_point(&mut path, vpoint[ve]);
        let into = |end| angle_of2(scale2(end_info(r, end).outward, -1.0));
        let mut back = path.clone();
        back.reverse();
        let half_width = run.width / 2.0;
        half.push(HalfEdge { tail: vs, head: ve, dir: norm(into(RunEnd::Start)), path, run: Some(r), half_width });
        half.push(HalfEdge { tail: ve, head: vs, dir: norm(into(RunEnd::End)), path: back, run: Some(r), half_width });
    }
    for b in &graph.bridges {
        let (va, vb) = (vert(b.a), vert(b.b));
        let (pa, pb) = (vpoint[va], vpoint[vb]);
        let half_width = runs.get(b.a.0).map_or(0.0, |r| r.width / 2.0);
        half.push(HalfEdge { tail: va, head: vb, dir: norm(angle_of2(sub2(pb, pa))), path: vec![pa, pb], run: None, half_width });
        half.push(HalfEdge { tail: vb, head: va, dir: norm(angle_of2(sub2(pa, pb))), path: vec![pb, pa], run: None, half_width });
    }

    // Outgoing half-edges per vertex, CCW by direction.
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); vpoint.len()];
    for (h, e) in half.iter().enumerate() {
        out[e.tail].push(h);
    }
    let mut pos = vec![0usize; half.len()];
    for list in &mut out {
        list.sort_by(|&a, &b| cmp_f64(half[a].dir, half[b].dir).then(a.cmp(&b)));
        for (i, &h) in list.iter().enumerate() {
            pos[h] = i;
        }
    }
    let next = |h: usize| {
        let twin = h ^ 1;
        let list = &out[half[h].head];
        list[(pos[twin] + list.len() - 1) % list.len()]
    };

    // Connected components over vertices.
    let mut parent: Vec<usize> = (0..vpoint.len()).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for e in half.iter().step_by(2) {
        let (a, b) = (find(&mut parent, e.tail), find(&mut parent, e.head));
        if a != b {
            parent[a.max(b)] = a.min(b);
        }
    }

    // Faces.
    struct Face {
        poly: Vec<Point2>,
        area: f64,
        runs: Vec<usize>,
        component: usize,
        boundary: Vec<BoundaryEdge>,
    }
    let mut face_of = vec![usize::MAX; half.len()];
    let mut faces: Vec<Face> = Vec::new();
    for start in 0..half.len() {
        if face_of[start] != usize::MAX {
            continue;
        }
        let f = faces.len();
        let mut poly = Vec::new();
        let mut face_runs = Vec::new();
        let mut boundary = Vec::new();
        let mut h = start;
        while face_of[h] == usize::MAX {
            face_of[h] = f;
            boundary.push(BoundaryEdge { points: half[h].path.clone(), half_width: half[h].half_width, run: half[h].run });
            for &q in &half[h].path {
                push_point(&mut poly, q);
            }
            face_runs.extend(half[h].run);
            h = next(h);
        }
        if poly.len() > 1 && dist2(poly[0], poly[poly.len() - 1]) <= TOLERANCE.coincidence_mm {
            poly.pop();
        }
        face_runs.sort_unstable();
        face_runs.dedup();
        let area = polygon_area_signed(&poly);
        let component = find(&mut parent, half[start].tail);
        faces.push(Face { poly, area, runs: face_runs, component, boundary });
    }

    // Outer face per component: the smallest signed area (first on a tie).
    let mut outside = vec![false; faces.len()];
    let mut components: Vec<usize> = faces.iter().map(|f| f.component).collect();
    components.sort_unstable();
    components.dedup();
    for c in components {
        let outer = (0..faces.len()).filter(|&f| faces[f].component == c).min_by(|&a, &b| cmp_f64(faces[a].area, faces[b].area).then(a.cmp(&b)));
        if let Some(f) = outer {
            outside[f] = true;
        }
    }
    // Outside pick points: the smallest remaining space containing each.
    for &pt in &params.outside_points {
        let hit = (0..faces.len())
            .filter(|&f| !outside[f] && point_in_loop(&faces[f].poly, pt))
            .min_by(|&a, &b| cmp_f64(faces[a].area, faces[b].area).then(a.cmp(&b)));
        if let Some(f) = hit {
            outside[f] = true;
        }
    }

    // Number the spaces.
    let mut space_faces: Vec<usize> = (0..faces.len()).filter(|&f| !outside[f]).collect();
    space_faces.sort_by(|&a, &b| {
        let key = |f: usize| faces[f].runs.first().copied().unwrap_or(usize::MAX);
        key(a).cmp(&key(b)).then(cmp_f64(faces[a].area, faces[b].area)).then(a.cmp(&b))
    });
    let mut kind = vec![SideKind::Outside; faces.len()];
    for (i, &f) in space_faces.iter().enumerate() {
        kind[f] = SideKind::Space(i);
    }
    let spaces =
        space_faces.iter().map(|&f| Space { area_mm2: faces[f].area, runs: faces[f].runs.clone(), boundary: faces[f].boundary.clone() }).collect();

    let on_exterior_layer = |r: usize| {
        runs[r].faces.iter().any(|face| p.entity(face.from.entity_id).is_some_and(|e| params.exterior_layer_ids.iter().any(|l| l == e.layer_id)))
    };
    let run_sides = (0..runs.len())
        .map(|r| {
            let (left, right) = (kind[face_of[2 * r]], kind[face_of[2 * r + 1]]);
            let exterior = left == SideKind::Outside || right == SideKind::Outside || on_exterior_layer(r);
            let open_outside_left = match runs[r].centreline {
                RunCurve::Line { from, to } if left == SideKind::Outside && right == SideKind::Outside => {
                    open_side(from, to, runs[r].width, &params.outside_points)
                }
                RunCurve::Arc { center, radius, start_angle, sweep } if left == SideKind::Outside && right == SideKind::Outside => {
                    open_side_arc(center, radius, start_angle, sweep, runs[r].width, &params.outside_points)
                }
                _ => None,
            };
            RunSides { left, right, exterior, open_outside_left }
        })
        .collect();
    WallSides { runs: run_sides, spaces }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f64, y: f64) -> Point2 {
        Point2 { x, y }
    }

    /// A space traced CCW through `corners`, edge `i` (from corner `i`) `widths[i]` thick.
    fn space(corners: &[(f64, f64)], widths: &[f64]) -> Space {
        let n = corners.len();
        let boundary = (0..n)
            .map(|i| {
                let (a, b) = (corners[i], corners[(i + 1) % n]);
                BoundaryEdge { points: vec![pt(a.0, a.1), pt(b.0, b.1)], half_width: widths[i] / 2.0, run: Some(i) }
            })
            .collect();
        Space { area_mm2: 0.0, runs: (0..n).collect(), boundary }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-6 * b.abs().max(1.0)
    }

    #[test]
    fn a_rectangle_of_200_walls_is_measured_inside_its_faces() {
        let s = space(&[(0.0, 0.0), (5000.0, 0.0), (5000.0, 4000.0), (0.0, 4000.0)], &[200.0; 4]);
        assert!(close(inner_area_mm2(&s), 4800.0 * 3800.0), "{}", inner_area_mm2(&s));
        assert!(space_contains(&s, pt(2500.0, 2000.0)) && !space_contains(&s, pt(6000.0, 2000.0)));
    }

    #[test]
    fn one_300_wall_and_three_200_walls_each_take_their_own_half_width() {
        // South wall 300: inside y 150..3900, x 100..4900.
        let s = space(&[(0.0, 0.0), (5000.0, 0.0), (5000.0, 4000.0), (0.0, 4000.0)], &[300.0, 200.0, 200.0, 200.0]);
        assert!(close(inner_area_mm2(&s), 4800.0 * 3750.0), "{}", inner_area_mm2(&s));
    }

    #[test]
    fn an_l_shaped_room_is_measured_inside_its_faces() {
        // 6000 × 4000 less its 3000 × 2000 top-right corner, 200 walls: inside x 100..5900 up to
        // y 1900, then x 100..2900 from 1900 to 3900.
        let s = space(&[(0.0, 0.0), (6000.0, 0.0), (6000.0, 2000.0), (3000.0, 2000.0), (3000.0, 4000.0), (0.0, 4000.0)], &[200.0; 6]);
        let want = 5800.0 * 1800.0 + 2800.0 * 2000.0;
        assert!(close(inner_area_mm2(&s), want), "{} vs {want}", inner_area_mm2(&s));
    }

    #[test]
    fn a_collinear_split_edge_joins_straight() {
        let s = space(&[(0.0, 0.0), (2500.0, 0.0), (5000.0, 0.0), (5000.0, 4000.0), (0.0, 4000.0)], &[200.0; 5]);
        assert!(close(inner_area_mm2(&s), 4800.0 * 3800.0), "{}", inner_area_mm2(&s));
    }
}

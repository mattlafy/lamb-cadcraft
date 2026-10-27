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
//! `Outside`) takes a straight run's exterior side from the nearest outside pick point beside it
//! ([`RunSides::open_outside_left`]).
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
    /// For a straight run with both sides `Outside`: whether the nearest outside pick point beside
    /// it lies to the left. `None` when no point resolves it, and for every other run.
    pub open_outside_left: Option<bool>,
}

/// Whether the run's exterior side is the left of its centreline's travel: the single `Outside`
/// side, or for an open structure [`RunSides::open_outside_left`]. `None` when undecided.
pub fn outside_on_left(sides: &RunSides) -> Option<bool> {
    match (sides.left, sides.right) {
        (SideKind::Outside, SideKind::Space(_)) => Some(true),
        (SideKind::Space(_), SideKind::Outside) => Some(false),
        (SideKind::Outside, SideKind::Outside) => sides.open_outside_left,
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

#[derive(Debug, Clone, PartialEq)]
pub struct Space {
    /// Area enclosed by the centrelines (and bridges), mm².
    pub area_mm2: f64,
    /// Runs on its boundary, sorted, deduped.
    pub runs: Vec<usize>,
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
        half.push(HalfEdge { tail: vs, head: ve, dir: norm(into(RunEnd::Start)), path, run: Some(r) });
        half.push(HalfEdge { tail: ve, head: vs, dir: norm(into(RunEnd::End)), path: back, run: Some(r) });
    }
    for b in &graph.bridges {
        let (va, vb) = (vert(b.a), vert(b.b));
        let (pa, pb) = (vpoint[va], vpoint[vb]);
        half.push(HalfEdge { tail: va, head: vb, dir: norm(angle_of2(sub2(pb, pa))), path: vec![pa, pb], run: None });
        half.push(HalfEdge { tail: vb, head: va, dir: norm(angle_of2(sub2(pa, pb))), path: vec![pb, pa], run: None });
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
        let mut h = start;
        while face_of[h] == usize::MAX {
            face_of[h] = f;
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
        faces.push(Face { poly, area, runs: face_runs, component });
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
    let spaces = space_faces.iter().map(|&f| Space { area_mm2: faces[f].area, runs: faces[f].runs.clone() }).collect();

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
                _ => None,
            };
            RunSides { left, right, exterior, open_outside_left }
        })
        .collect();
    WallSides { runs: run_sides, spaces }
}

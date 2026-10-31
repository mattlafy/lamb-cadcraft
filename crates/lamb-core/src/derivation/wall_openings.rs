//! The wall graph, third slice: **openings bridge gaps** (LAMB's
//! `docs/07-architecture/cadcraft-integration.md` §8, phase 7). A gap between two facing free run
//! ends is bridged with an opening edge only where a drawn window or door spans it — an undrawn
//! gap stays two free ends. Computed on demand, never stored.
//!
//! Openings are recognised by layer only ([`WallRunParams::opening_layer_ids`]; role is ignored)
//! and grouped into connected clusters, so a door drawn as a separate leaf line and swing arc is one
//! opening. A cluster spans a gap when it comes within `TOLERANCE.grouping_mm` of both ends' caps.
//!
//! A gap is bridged between two straight free ends that face each other on one line, or between two
//! free ends of different arc runs on one circle whose outward tangents turn toward each other
//! around it (the short way, under half a turn). A straight end never pairs with an arc end.
//! Openings drawn as blocks are a later slice.

use std::cmp::Ordering;

use super::read::ProjectRead;
use super::wall_junctions::{EndInfo, RunEnd, run_ends};
use super::walls::{RunCurve, WallRun, WallRunParams};
use crate::geometry::Point2;
use crate::geometry::intersect::intersect_segments;
use crate::geometry::point::{add2, cross2, dist2, dot2, scale2, sub2};
use crate::geometry::polyline::{ResolvedSegment, closest_point_on_segment, resolved_segments};
use crate::geometry::tolerance::{TOLERANCE, deg_to_rad};
use crate::model::Id;

/// An opening edge closing the gap between two facing run ends.
#[derive(Debug, Clone, PartialEq)]
pub struct OpeningBridge {
    /// `a < b`.
    pub a: (usize, RunEnd),
    pub b: (usize, RunEnd),
    /// The two centreline end points: `a`'s, then `b`'s.
    pub from: Point2,
    pub to: Point2,
    /// The spanning opening cluster's entities, sorted.
    pub opening_entity_ids: Vec<Id>,
}

struct Cluster {
    ids: Vec<Id>,
    segments: Vec<ResolvedSegment>,
}

fn opening_clusters(p: &impl ProjectRead, view: Id, params: &WallRunParams) -> Vec<Cluster> {
    let mut ents: Vec<(Id, Vec<ResolvedSegment>)> = p
        .entities(view)
        .filter(|e| params.opening_layer_ids.iter().any(|id| id == e.layer_id))
        .filter_map(|e| e.polyline.as_ref().map(|poly| (e.id, resolved_segments(poly))))
        .filter(|(_, segs)| !segs.is_empty())
        .collect();
    ents.sort_by_key(|(id, _)| *id);

    let touches = |a: &[ResolvedSegment], b: &[ResolvedSegment]| {
        a.iter().flat_map(|s| [s.from, s.to]).any(|pt| b.iter().any(|t| closest_point_on_segment(t, pt).distance <= TOLERANCE.grouping_mm))
    };
    let mut parent: Vec<usize> = (0..ents.len()).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for i in 0..ents.len() {
        for j in i + 1..ents.len() {
            if touches(&ents[i].1, &ents[j].1) || touches(&ents[j].1, &ents[i].1) {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri != rj {
                    parent[ri.max(rj)] = ri.min(rj);
                }
            }
        }
    }
    let mut clusters: Vec<(usize, Cluster)> = Vec::new();
    for (i, (id, segs)) in ents.into_iter().enumerate() {
        let r = find(&mut parent, i);
        match clusters.iter_mut().find(|c| c.0 == r) {
            Some((_, c)) => {
                c.ids.push(id);
                c.segments.extend(segs);
            }
            None => clusters.push((r, Cluster { ids: vec![id], segments: segs })),
        }
    }
    clusters.into_iter().map(|(_, c)| c).collect()
}

/// Distance between a cluster and an end cap: 0 where they cross, else the closest approach of
/// cap endpoints to cluster segments and cluster segment endpoints to the cap.
fn cluster_cap_distance(c: &Cluster, cap: &ResolvedSegment) -> f64 {
    if c.segments.iter().any(|s| !intersect_segments(cap, s, 1e-9).is_empty()) {
        return 0.0;
    }
    let mut best = f64::INFINITY;
    for s in &c.segments {
        for q in [cap.from, cap.to] {
            best = best.min(closest_point_on_segment(s, q).distance);
        }
        for q in [s.from, s.to] {
            best = best.min(closest_point_on_segment(cap, q).distance);
        }
    }
    best
}

/// Two free arc ends face each other around one circle: centres and radii within `grouping_mm`,
/// `a`'s outward tangent turning toward `b` by more than `coincidence_mm / radius` and under π, and
/// `b`'s outward tangent turning the opposite way.
fn arcs_face(a: &EndInfo, b: &EndInfo, ca: Point2, ra: f64, cb: Point2, rb: f64) -> bool {
    if dist2(ca, cb) > TOLERANCE.grouping_mm || (ra - rb).abs() > TOLERANCE.grouping_mm || ra <= 0.0 {
        return false;
    }
    let sense = |e: &EndInfo, c: Point2| cross2(sub2(e.point, c), e.outward).signum();
    let (sa, sb) = (sense(a, ca), sense(b, ca));
    if sa == 0.0 || sb != -sa {
        return false;
    }
    let ang = |q: Point2| {
        let v = sub2(q, ca);
        v.y.atan2(v.x)
    };
    let two_pi = std::f64::consts::TAU;
    let turn = ((ang(b.point) - ang(a.point)) * sa).rem_euclid(two_pi);
    turn > TOLERANCE.coincidence_mm / ra && turn < std::f64::consts::PI
}

/// The opening bridges among `free_ends` (from [`super::wall_junctions::wall_junctions`] over
/// `runs`), sorted by `(a, b)`. Candidates are free ends of different straight runs whose outward
/// directions are antiparallel and which face each other on one line (lateral offset within
/// `grouping_mm`), or free ends of different arc runs facing each other around one circle
/// ([`arcs_face`]); a pair is bridged when one opening cluster comes within `grouping_mm` of both
/// end caps. Shortest gaps win; each end bridges at most once.
pub fn opening_bridges(
    p: &impl ProjectRead,
    view: Id,
    params: &WallRunParams,
    runs: &[WallRun],
    free_ends: &[(usize, RunEnd)],
) -> Vec<OpeningBridge> {
    let ends: Vec<_> = run_ends(runs).into_iter().filter(|e| free_ends.contains(&e.key)).collect();
    if ends.len() < 2 {
        return Vec::new();
    }
    let clusters = opening_clusters(p, view, params);
    if clusters.is_empty() {
        return Vec::new();
    }
    let cap = |e: &EndInfo| {
        let n = Point2 { x: -e.outward.y, y: e.outward.x };
        let h = 0.5 * e.width;
        ResolvedSegment { index: 0, from: add2(e.point, scale2(n, -h)), to: add2(e.point, scale2(n, h)), bulge: 0.0 }
    };
    let caps: Vec<ResolvedSegment> = ends.iter().map(cap).collect();
    let sin_tol = deg_to_rad(TOLERANCE.angle_deg).sin();

    let mut cands: Vec<(f64, usize, usize, usize)> = Vec::new(); // (gap, i, j, cluster)
    for (i, a) in ends.iter().enumerate() {
        for (j, b) in ends.iter().enumerate().skip(i + 1) {
            if a.key.0 == b.key.0 {
                continue;
            }
            let facing = match (&runs[a.key.0].centreline, &runs[b.key.0].centreline) {
                (RunCurve::Line { .. }, RunCurve::Line { .. }) => {
                    let d = sub2(b.point, a.point);
                    dot2(a.outward, b.outward) < 0.0
                        && cross2(a.outward, b.outward).abs() <= sin_tol
                        && dot2(a.outward, d) > TOLERANCE.coincidence_mm
                        && cross2(a.outward, d).abs() <= TOLERANCE.grouping_mm
                }
                (RunCurve::Arc { center: ca, radius: ra, .. }, RunCurve::Arc { center: cb, radius: rb, .. }) => arcs_face(a, b, *ca, *ra, *cb, *rb),
                _ => false,
            };
            if !facing {
                continue;
            }
            let spanning = clusters.iter().position(|c| {
                cluster_cap_distance(c, &caps[i]) <= TOLERANCE.grouping_mm && cluster_cap_distance(c, &caps[j]) <= TOLERANCE.grouping_mm
            });
            if let Some(k) = spanning {
                cands.push((dist2(a.point, b.point), i, j, k));
            }
        }
    }
    cands.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(Ordering::Equal).then((ends[x.1].key, ends[x.2].key).cmp(&(ends[y.1].key, ends[y.2].key))));

    let mut used = vec![false; ends.len()];
    let mut out = Vec::new();
    for (_, i, j, k) in cands {
        if used[i] || used[j] {
            continue;
        }
        used[i] = true;
        used[j] = true;
        let (a, b) = if ends[i].key < ends[j].key { (&ends[i], &ends[j]) } else { (&ends[j], &ends[i]) };
        let mut ids = clusters[k].ids.clone();
        ids.sort();
        out.push(OpeningBridge { a: a.key, b: b.key, from: a.point, to: b.point, opening_entity_ids: ids });
    }
    out.sort_by_key(|b| (b.a, b.b));
    out
}

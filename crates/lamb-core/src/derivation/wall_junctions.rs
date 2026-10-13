//! The wall graph, second slice: **junctions** (LAMB's `docs/07-architecture/cadcraft-integration.md`
//! §8, phase 7). Where run ends meet near one point they become one node with its arms and a
//! signature — the CCW gaps between the arms. An L, a T built from one straight face line and two
//! L-shaped ones, and an X each come out as a single node. Unjoined run ends are free ends, for the
//! opening-bridging slice. Computed on demand, never stored.
//!
//! A node is found from pairs of run ends: two ends of different runs whose extended centreline
//! tangents cross within `reach = half the wider run's width + TOLERANCE.grouping_mm` of both give a
//! candidate point; candidates cluster by single linkage. Then every run end joins the nearest node
//! within that node's reach that is not behind it — which is how a T's two collinear through-runs,
//! which never cross each other, join their stem's node.

use std::cmp::Ordering;

use super::read::ProjectRead;
use super::walls::{RunCurve, WallRun, WallRunParams, wall_runs};
use crate::geometry::Point2;
use crate::geometry::intersect::infinite_line_intersect;
use crate::geometry::point::{add2, angle_of2, cross2, dist2, dot2, normalize2, scale2, sub2};
use crate::geometry::tolerance::{TOLERANCE, deg_to_rad, rad_to_deg};
use crate::model::Id;

/// Which end of a run: `Start` is the centreline's `from` (or arc start), `End` its `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RunEnd {
    Start,
    End,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JunctionArm {
    /// Index into the runs the junctions were computed from.
    pub run: usize,
    pub end: RunEnd,
    /// Direction from the node into the run, degrees in `[0, 360)`.
    pub direction_deg: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Junction {
    pub point: Point2,
    /// In signature order: CCW by direction, rotated so `gaps_deg` is lexicographically smallest.
    pub arms: Vec<JunctionArm>,
    /// `gaps_deg[i]` is the CCW angle from `arms[i]` to the next arm (the last wraps to the first);
    /// they sum to 360. L → `[90, 270]`, T → `[90, 90, 180]`, X → `[90, 90, 90, 90]`.
    pub gaps_deg: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WallGraph {
    pub runs: Vec<WallRun>,
    pub junctions: Vec<Junction>,
    pub free_ends: Vec<(usize, RunEnd)>,
}

#[derive(Clone, Copy)]
struct EndInfo {
    key: (usize, RunEnd),
    point: Point2,
    /// Unit tangent pointing out of the run.
    outward: Point2,
    width: f64,
}

fn run_ends(runs: &[WallRun]) -> Vec<EndInfo> {
    let mut out = Vec::with_capacity(runs.len() * 2);
    for (i, r) in runs.iter().enumerate() {
        let (start, start_out, end, end_out) = match r.centreline {
            RunCurve::Line { from, to } => {
                let t = normalize2(sub2(to, from));
                (from, scale2(t, -1.0), to, t)
            }
            RunCurve::Arc { center, radius, start_angle, sweep } => {
                let at = |a: f64| add2(center, Point2 { x: radius * a.cos(), y: radius * a.sin() });
                // Unit tangent in the direction of travel at angle `a`.
                let s = sweep.signum();
                let tan = |a: f64| Point2 { x: -a.sin() * s, y: a.cos() * s };
                let end_angle = start_angle + sweep;
                (at(start_angle), scale2(tan(start_angle), -1.0), at(end_angle), tan(end_angle))
            }
        };
        out.push(EndInfo { key: (i, RunEnd::Start), point: start, outward: start_out, width: r.width });
        out.push(EndInfo { key: (i, RunEnd::End), point: end, outward: end_out, width: r.width });
    }
    out
}

fn not_behind(e: &EndInfo, p: Point2) -> bool {
    dot2(e.outward, sub2(p, e.point)) >= -TOLERANCE.grouping_mm
}

fn candidate(a: &EndInfo, b: &EndInfo) -> Option<(Point2, f64)> {
    let reach = 0.5 * a.width.max(b.width) + TOLERANCE.grouping_mm;
    if dist2(a.point, b.point) > 2.0 * reach {
        return None;
    }
    if cross2(a.outward, b.outward).abs() <= deg_to_rad(TOLERANCE.angle_deg).sin() {
        return None;
    }
    let hit = infinite_line_intersect(a.point, add2(a.point, a.outward), b.point, add2(b.point, b.outward))?;
    let p = hit.point;
    (dist2(p, a.point) <= reach && dist2(p, b.point) <= reach && not_behind(a, p) && not_behind(b, p)).then_some((p, reach))
}

/// Total order on `f64` for sorting finite values.
fn cmp_f64(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

fn signature(mut arms: Vec<JunctionArm>) -> (Vec<JunctionArm>, Vec<f64>) {
    arms.sort_by(|a, b| cmp_f64(a.direction_deg, b.direction_deg).then((a.run, a.end).cmp(&(b.run, b.end))));
    let n = arms.len();
    let gaps: Vec<f64> = (0..n)
        .map(|i| {
            let next = arms[(i + 1) % n].direction_deg;
            (next - arms[i].direction_deg).rem_euclid(360.0)
        })
        .collect();
    // Rounded to 0.1° (as integer tenths) to choose the rotation; the stored gaps stay exact.
    let tenths: Vec<i64> = gaps.iter().map(|g| (g * 10.0).round() as i64).collect();
    let tenths = &tenths;
    let rotated = |k: usize| (0..n).map(move |i| tenths[(i + k) % n]);
    let best = (0..n).min_by(|&a, &b| rotated(a).cmp(rotated(b))).unwrap_or(0);
    arms.rotate_left(best);
    let mut gaps = gaps;
    gaps.rotate_left(best);
    (arms, gaps)
}

/// The junctions among `runs`, sorted by each one's smallest `(run, end)` arm, and the run ends that
/// joined none, sorted.
pub fn wall_junctions(runs: &[WallRun]) -> (Vec<Junction>, Vec<(usize, RunEnd)>) {
    let ends = run_ends(runs);

    // Candidate points from pairs of ends of different runs.
    let mut cands: Vec<(Point2, f64)> = Vec::new();
    for (i, a) in ends.iter().enumerate() {
        for b in &ends[i + 1..] {
            if a.key.0 != b.key.0 {
                cands.extend(candidate(a, b));
            }
        }
    }

    // Single-linkage clustering (union–find) within grouping_mm.
    let mut parent: Vec<usize> = (0..cands.len()).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for i in 0..cands.len() {
        for j in i + 1..cands.len() {
            if dist2(cands[i].0, cands[j].0) <= TOLERANCE.grouping_mm {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri != rj {
                    parent[ri.max(rj)] = ri.min(rj);
                }
            }
        }
    }
    // Nodes in order of their root (first candidate), so the result does not depend on hash order.
    let mut nodes: Vec<(usize, Point2, f64, usize)> = Vec::new(); // (root, sum, reach, count)
    for (i, &(p, reach)) in cands.iter().enumerate() {
        let r = find(&mut parent, i);
        match nodes.iter_mut().find(|n| n.0 == r) {
            Some(n) => {
                n.1 = add2(n.1, p);
                n.2 = n.2.max(reach);
                n.3 += 1;
            }
            None => nodes.push((r, p, reach, 1)),
        }
    }
    let nodes: Vec<(Point2, f64)> = nodes.into_iter().map(|(_, sum, reach, n)| (scale2(sum, 1.0 / n as f64), reach)).collect();

    // Each end joins the nearest node within that node's reach that is not behind it.
    let mut arms: Vec<Vec<JunctionArm>> = vec![Vec::new(); nodes.len()];
    let mut free: Vec<(usize, RunEnd)> = Vec::new();
    for e in &ends {
        let best = nodes
            .iter()
            .enumerate()
            .filter(|(_, (p, reach))| dist2(*p, e.point) <= *reach && not_behind(e, *p))
            .min_by(|(ia, (pa, _)), (ib, (pb, _))| cmp_f64(dist2(*pa, e.point), dist2(*pb, e.point)).then(ia.cmp(ib)));
        match best {
            Some((k, _)) => {
                let dir = rad_to_deg(angle_of2(scale2(e.outward, -1.0))).rem_euclid(360.0);
                let dir = if dir >= 360.0 { 0.0 } else { dir };
                arms[k].push(JunctionArm { run: e.key.0, end: e.key.1, direction_deg: dir });
            }
            None => free.push(e.key),
        }
    }

    let mut junctions = Vec::new();
    for ((point, _), node_arms) in nodes.into_iter().zip(arms) {
        if node_arms.len() < 2 {
            free.extend(node_arms.iter().map(|a| (a.run, a.end)));
            continue;
        }
        let (arms, gaps_deg) = signature(node_arms);
        junctions.push(Junction { point, arms, gaps_deg });
    }
    let first = |j: &Junction| j.arms.iter().map(|a| (a.run, a.end)).min();
    junctions.sort_by_key(first);
    free.sort();
    (junctions, free)
}

/// [`wall_runs`] in `view`, then [`wall_junctions`] over them.
pub fn wall_graph(p: &impl ProjectRead, view: Id, params: &WallRunParams) -> WallGraph {
    let runs = wall_runs(p, view, params);
    let (junctions, free_ends) = wall_junctions(&runs);
    WallGraph { runs, junctions, free_ends }
}

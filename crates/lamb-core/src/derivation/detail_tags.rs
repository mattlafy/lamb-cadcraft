//! The wall graph, fifth slice: **greedy detail-tag resolution** for edge details (LAMB's
//! `docs/07-architecture/cadcraft-integration.md` §9, phase 8, LAMB T342). Given one view's wall
//! graph, its sides and the edge-detail tags placed on runs, says which detail each run takes — or
//! that it stays untyped. Computed on demand, never stored; tags as drawn entities, corner details,
//! sheet references and the sweep come later and consume this result.
//!
//! Resolution runs per **class** — `(exterior, width rounded to the mm)`: an exterior cladding
//! detail never lands on a partition, a 90 mm stud detail never on a 140 mm wall. Two runs are
//! neighbours when they share a junction or an opening bridge joins their ends (a door does not
//! break a wall's detail). Rules, in order:
//!
//! 1. A tagged run takes its tag ([`RunDetail::Tagged`]); two different details on one run →
//!    [`RunDetail::Conflict`].
//! 2. A class with exactly one distinct detail among its tags gives it to every untagged run of the
//!    class ([`RunDetail::Spread`]), connected or not.
//! 3. Otherwise each connected gap of untagged runs in the class graph collects the distinct details
//!    of the tagged class runs adjacent to it: exactly one → every run in the gap takes it
//!    ([`RunDetail::Gap`]); none or several → [`RunDetail::Untyped`]. A free end or a junction into
//!    another class bounds nothing, so a tag out to a dead end takes the stretch.
//! 4. Never guess: anything else is `Untyped`.
//!
//! The result depends only on the graph's structure, never on run or tag order.
//!
//! **Corner details** (LAMB T348) resolve by the same rules over junctions instead of runs
//! ([`resolve_corner_details`]). A junction's class ([`JunctionClass`]) is its signature — gap
//! angles rounded to whole degrees, each with whether it faces `Outside` — plus its arms' run
//! classes, so an L detail never lands on a T, an exterior L never on a partition L, and a
//! building's convex corner (outside gap 270) never shares a class with a reflex one (outside gap
//! 90). Two junctions are neighbours when a run, continued across opening bridges, joins them
//! ([`junction_neighbours`]). Straight-through N = 2 nodes are junctions like any other.

use std::collections::{BTreeMap, BTreeSet};

use super::wall_junctions::{RunEnd, WallGraph};
use super::wall_spaces::{SideKind, WallSides};

/// An edge-detail tag placed on one run: `run` indexes [`WallGraph::runs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeTag {
    pub detail: String,
    pub run: usize,
}

/// What one run's edge detail, or one junction's corner detail, resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunDetail {
    /// The run carries this tag itself.
    Tagged(String),
    /// The run's class has this as its only detail in the view.
    Spread(String),
    /// The run sits in a gap bounded by runs tagged only with this detail.
    Gap(String),
    /// The run carries two or more different tags.
    Conflict,
    /// Nothing decides it.
    Untyped,
}

/// The key resolution runs under: runs of different classes never share a detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RunClass {
    pub exterior: bool,
    pub width_mm: i64,
}

/// Each run's class: `exterior` from `sides`, the wall width rounded to the nearest mm.
pub fn run_classes(graph: &WallGraph, sides: &WallSides) -> Vec<RunClass> {
    graph
        .runs
        .iter()
        .enumerate()
        .map(|(i, r)| RunClass { exterior: sides.runs.get(i).is_some_and(|s| s.exterior), width_mm: r.width.round() as i64 })
        .collect()
}

/// Every run's neighbours — runs sharing a junction or joined by an opening bridge — sorted, no
/// self-loops.
pub fn run_neighbours(graph: &WallGraph) -> Vec<BTreeSet<usize>> {
    let n = graph.runs.len();
    let mut nb = vec![BTreeSet::new(); n];
    let mut join = |a: usize, b: usize| {
        if a != b && a < n && b < n {
            nb[a].insert(b);
            nb[b].insert(a);
        }
    };
    for j in &graph.junctions {
        for (k, x) in j.arms.iter().enumerate() {
            for y in &j.arms[k + 1..] {
                join(x.run, y.run);
            }
        }
    }
    for b in &graph.bridges {
        join(b.a.0, b.b.0);
    }
    nb
}

/// One [`RunDetail`] per run of `graph`, by the rules in the module docs. Tags naming a run that is
/// not in the graph are ignored.
pub fn resolve_edge_details(graph: &WallGraph, sides: &WallSides, tags: &[EdgeTag]) -> Vec<RunDetail> {
    let tags: Vec<(usize, &str)> = tags.iter().map(|t| (t.run, t.detail.as_str())).collect();
    resolve_greedy(&run_classes(graph, sides), &run_neighbours(graph), &tags)
}

/// A corner-detail tag placed on one junction: `junction` indexes [`WallGraph::junctions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CornerTag {
    pub detail: String,
    pub junction: usize,
}

/// The key corner resolution runs under: per gap in signature order, the CCW angle rounded to whole
/// degrees and whether that gap faces [`SideKind::Outside`] (canonically rotated, see
/// [`junction_classes`]), and the arms' [`RunClass`]es sorted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JunctionClass {
    pub gaps: Vec<(i64, bool)>,
    pub arms: Vec<RunClass>,
}

/// Each junction's class. The gap from arm i to arm i+1 lies on one side of arm i's run — its left
/// when the arm leaves from the run's `Start` (the arm points along the centreline), its right when
/// from the `End`. Of the gap list's rotations the lexicographically smallest is taken, so X and Y
/// signatures do not depend on which arm the signature happened to start from.
pub fn junction_classes(graph: &WallGraph, sides: &WallSides) -> Vec<JunctionClass> {
    let rc = run_classes(graph, sides);
    graph
        .junctions
        .iter()
        .map(|j| {
            let gaps: Vec<(i64, bool)> = j
                .arms
                .iter()
                .zip(&j.gaps_deg)
                .map(|(arm, g)| {
                    let outside = sides.runs.get(arm.run).is_some_and(|s| {
                        let side = match arm.end {
                            RunEnd::Start => s.left,
                            RunEnd::End => s.right,
                        };
                        side == SideKind::Outside
                    });
                    (g.round() as i64, outside)
                })
                .collect();
            let gaps = (0..gaps.len())
                .map(|k| {
                    let mut r = gaps.clone();
                    r.rotate_left(k);
                    r
                })
                .min()
                .unwrap_or_default();
            let mut arms: Vec<RunClass> = j.arms.iter().filter_map(|a| rc.get(a.run).copied()).collect();
            arms.sort();
            JunctionClass { gaps, arms }
        })
        .collect()
}

/// Every junction's neighbours, sorted, no self-loops: leave a junction along an arm's run to its
/// other end; an end in an opening bridge crosses to the bridged run end and continues along that
/// run; the first junction reached is a neighbour, a free end gives none.
pub fn junction_neighbours(graph: &WallGraph) -> Vec<BTreeSet<usize>> {
    let mut at: BTreeMap<(usize, RunEnd), usize> = BTreeMap::new();
    for (k, j) in graph.junctions.iter().enumerate() {
        for a in &j.arms {
            at.insert((a.run, a.end), k);
        }
    }
    let mut bridge: BTreeMap<(usize, RunEnd), (usize, RunEnd)> = BTreeMap::new();
    for b in &graph.bridges {
        bridge.insert(b.a, b.b);
        bridge.insert(b.b, b.a);
    }
    let other = |e: RunEnd| match e {
        RunEnd::Start => RunEnd::End,
        RunEnd::End => RunEnd::Start,
    };
    let mut nb = vec![BTreeSet::new(); graph.junctions.len()];
    for (k, j) in graph.junctions.iter().enumerate() {
        for a in &j.arms {
            let mut end = (a.run, other(a.end));
            let mut visited = BTreeSet::new();
            while visited.insert(end) {
                if let Some(&m) = at.get(&end) {
                    if m != k {
                        nb[k].insert(m);
                        nb[m].insert(k);
                    }
                    break;
                }
                match bridge.get(&end) {
                    Some(&(r, e)) => end = (r, other(e)),
                    None => break,
                }
            }
        }
    }
    nb
}

/// One [`RunDetail`] per junction of `graph`, by the module docs' rules over [`junction_classes`]
/// and [`junction_neighbours`]. Tags naming a junction that is not in the graph are ignored.
pub fn resolve_corner_details(graph: &WallGraph, sides: &WallSides, tags: &[CornerTag]) -> Vec<RunDetail> {
    let tags: Vec<(usize, &str)> = tags.iter().map(|t| (t.junction, t.detail.as_str())).collect();
    resolve_greedy(&junction_classes(graph, sides), &junction_neighbours(graph), &tags)
}

/// The greedy algorithm shared by edges and corners: `classes[i]` and `neighbours[i]` describe item
/// i (a run or a junction), `tags` are `(item, detail)`; items out of range are ignored.
fn resolve_greedy<K: Ord + Clone>(classes: &[K], neighbours: &[BTreeSet<usize>], tags: &[(usize, &str)]) -> Vec<RunDetail> {
    let n = classes.len();

    let mut item_tags: Vec<BTreeSet<&str>> = vec![BTreeSet::new(); n];
    for &(i, d) in tags.iter().filter(|t| t.0 < n) {
        item_tags[i].insert(d);
    }

    let mut out: Vec<RunDetail> = item_tags
        .iter()
        .map(|d| match (d.len(), only(d)) {
            (0, _) => RunDetail::Untyped,
            (_, Some(t)) => RunDetail::Tagged(t),
            _ => RunDetail::Conflict,
        })
        .collect();

    let mut class_details: BTreeMap<&K, BTreeSet<&str>> = BTreeMap::new();
    for (c, t) in classes.iter().zip(&item_tags) {
        class_details.entry(c).or_default().extend(t.iter().copied());
    }

    let mut seen = vec![false; n];
    for start in 0..n {
        if seen[start] || !item_tags[start].is_empty() {
            continue;
        }
        let c = &classes[start];
        if let Some(d) = only(&class_details[c]) {
            seen[start] = true;
            out[start] = RunDetail::Spread(d);
            continue;
        }
        // Flood the gap of untagged same-class items containing `start`, collecting the details of
        // the tagged same-class items bounding it.
        let mut gap = vec![start];
        let mut bound: BTreeSet<&str> = BTreeSet::new();
        seen[start] = true;
        let mut k = 0;
        while k < gap.len() {
            let r = gap[k];
            k += 1;
            for &m in neighbours.get(r).into_iter().flatten() {
                if m >= n || classes[m] != *c {
                    continue;
                }
                if !item_tags[m].is_empty() {
                    bound.extend(item_tags[m].iter().copied());
                } else if !seen[m] {
                    seen[m] = true;
                    gap.push(m);
                }
            }
        }
        let resolved = only(&bound).map_or(RunDetail::Untyped, RunDetail::Gap);
        for r in gap {
            out[r] = resolved.clone();
        }
    }
    out
}

/// The set's single member, if it has exactly one.
fn only(set: &BTreeSet<&str>) -> Option<String> {
    if set.len() == 1 { set.first().map(|d| (*d).to_string()) } else { None }
}

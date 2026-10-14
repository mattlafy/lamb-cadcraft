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

use std::collections::{BTreeMap, BTreeSet};

use super::wall_junctions::WallGraph;
use super::wall_spaces::WallSides;

/// An edge-detail tag placed on one run: `run` indexes [`WallGraph::runs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeTag {
    pub detail: String,
    pub run: usize,
}

/// What one run's edge detail resolved to.
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
    let n = graph.runs.len();
    let class = run_classes(graph, sides);
    let nb = run_neighbours(graph);

    let mut run_tags: Vec<BTreeSet<&str>> = vec![BTreeSet::new(); n];
    for t in tags.iter().filter(|t| t.run < n) {
        run_tags[t.run].insert(t.detail.as_str());
    }

    let mut out: Vec<RunDetail> = run_tags
        .iter()
        .map(|d| match (d.len(), only(d)) {
            (0, _) => RunDetail::Untyped,
            (_, Some(t)) => RunDetail::Tagged(t),
            _ => RunDetail::Conflict,
        })
        .collect();

    let mut class_details: BTreeMap<RunClass, BTreeSet<&str>> = BTreeMap::new();
    for i in 0..n {
        class_details.entry(class[i]).or_default().extend(run_tags[i].iter().copied());
    }

    let mut seen = vec![false; n];
    for start in 0..n {
        if seen[start] || !run_tags[start].is_empty() {
            continue;
        }
        let c = class[start];
        let details = &class_details[&c];
        if let Some(d) = only(details) {
            seen[start] = true;
            out[start] = RunDetail::Spread(d);
            continue;
        }
        // Flood the gap of untagged same-class runs containing `start`, collecting the details of
        // the tagged same-class runs bounding it.
        let mut gap = vec![start];
        let mut bound: BTreeSet<&str> = BTreeSet::new();
        seen[start] = true;
        let mut k = 0;
        while k < gap.len() {
            let r = gap[k];
            k += 1;
            for &m in &nb[r] {
                if class[m] != c {
                    continue;
                }
                if !run_tags[m].is_empty() {
                    bound.extend(run_tags[m].iter().copied());
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

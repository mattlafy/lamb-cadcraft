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
//!
//! **Opening details** (LAMB T485) resolve per opening bridge ([`resolve_opening_details`]) by rules
//! 1 and 2 only — tagged, then class spread — never by gaps. A bridge's class is its two bridged
//! runs' [`RunClass`]es, sorted, so a jamb detail for a 200 mm exterior wall never lands in a 90 mm
//! partition's door. The detail is drawn for one jamb and placed at both
//! (`assembly::place_opening`).
//!
//! **Opening ends** (LAMB T505): in a section an opening bridge's two ends are different
//! conditions — a head and a sill — so [`resolve_opening_end_details`] resolves each end as its own
//! item, classed by the bridge's class plus its [`EndLevel`]. A head tag spreads to heads, never to
//! a sill; a level bridge's two ends share one class.

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

/// An opening-detail tag placed on one opening bridge: `bridge` indexes [`WallGraph::bridges`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpeningTag {
    pub detail: String,
    pub bridge: usize,
}

/// One [`RunDetail`] per bridge of `graph`: rules 1 and 2 of the module docs over each bridge's
/// class — its two runs' [`RunClass`]es, sorted — with no neighbours, so gaps never decide it.
/// Tags naming a bridge that is not in the graph are ignored.
pub fn resolve_opening_details(graph: &WallGraph, sides: &WallSides, tags: &[OpeningTag]) -> Vec<RunDetail> {
    let classes = bridge_classes(graph, sides);
    let tags: Vec<(usize, &str)> = tags.iter().map(|t| (t.bridge, t.detail.as_str())).collect();
    resolve_greedy(&classes, &vec![BTreeSet::new(); classes.len()], &tags)
}

/// Each bridge's class: its two runs' [`RunClass`]es, sorted.
fn bridge_classes(graph: &WallGraph, sides: &WallSides) -> Vec<(Option<RunClass>, Option<RunClass>)> {
    let rc = run_classes(graph, sides);
    graph
        .bridges
        .iter()
        .map(|b| {
            let (x, y) = (rc.get(b.a.0).copied(), rc.get(b.b.0).copied());
            if x <= y { (x, y) } else { (y, x) }
        })
        .collect()
}

/// An opening-detail tag on one end of an opening bridge: `bridge` indexes [`WallGraph::bridges`],
/// `at_b` picks the `b` end (`bridge.to`) over the `a` end (`bridge.from`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpeningEndTag {
    pub detail: String,
    pub bridge: usize,
    pub at_b: bool,
}

/// Where an opening end sits against the bridge's other end: `Upper` (a head), `Lower` (a sill or
/// threshold) or `Level` (both ends at one height, e.g. a gap in a floor).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EndLevel {
    Upper,
    Lower,
    Level,
}

/// `[a-end, b-end]` per bridge of `graph`: rules 1 and 2 of the module docs over items `2*i` (a) and
/// `2*i+1` (b), each classed by its bridge's class and [`EndLevel`] from `upper[i]` (`Some(true)` =
/// a is higher, `Some(false)` = b is higher, `None` or missing = level). No neighbours. Tags naming
/// a bridge that is not in the graph are ignored.
pub fn resolve_opening_end_details(graph: &WallGraph, sides: &WallSides, tags: &[OpeningEndTag], upper: &[Option<bool>]) -> Vec<[RunDetail; 2]> {
    let bc = bridge_classes(graph, sides);
    let mut classes = Vec::with_capacity(bc.len() * 2);
    for (i, c) in bc.iter().enumerate() {
        let (a, b) = match upper.get(i).copied().flatten() {
            Some(true) => (EndLevel::Upper, EndLevel::Lower),
            Some(false) => (EndLevel::Lower, EndLevel::Upper),
            None => (EndLevel::Level, EndLevel::Level),
        };
        classes.push((*c, a));
        classes.push((*c, b));
    }
    let tags: Vec<(usize, &str)> =
        tags.iter().filter(|t| t.bridge < bc.len()).map(|t| (2 * t.bridge + usize::from(t.at_b), t.detail.as_str())).collect();
    let flat = resolve_greedy(&classes, &vec![BTreeSet::new(); classes.len()], &tags);
    flat.chunks(2).map(|c| [c[0].clone(), c[1].clone()]).collect()
}

/// The greedy algorithm shared by edges, corners and openings: `classes[i]` and `neighbours[i]` describe item
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derivation::wall_openings::OpeningBridge;
    use crate::derivation::wall_spaces::RunSides;
    use crate::derivation::walls::{RunCurve, RunFace, WallRun};
    use crate::geometry::Point2;
    use crate::model::{Id, LinkAnchor};

    fn run(width: f64) -> WallRun {
        let a = LinkAnchor { entity_id: Id(1), segment_index: 0, segment_param: 0.0 };
        let p = Point2 { x: 0.0, y: 0.0 };
        WallRun {
            view_id: Id(1),
            faces: [RunFace { from: a.clone(), to: a.clone() }, RunFace { from: a.clone(), to: a }],
            width,
            centreline: RunCurve::Line { from: p, to: Point2 { x: 1000.0, y: 0.0 } },
        }
    }

    /// Exterior runs of the given widths, bridged in pairs (0–1, 2–3, …).
    fn bridged(widths: &[f64]) -> (WallGraph, WallSides) {
        let p = Point2 { x: 0.0, y: 0.0 };
        let bridges = (0..widths.len() / 2)
            .map(|k| OpeningBridge { a: (2 * k, RunEnd::End), b: (2 * k + 1, RunEnd::Start), from: p, to: p, opening_entity_ids: vec![Id(k as u64)] })
            .collect();
        let graph = WallGraph { runs: widths.iter().map(|w| run(*w)).collect(), junctions: Vec::new(), free_ends: Vec::new(), bridges };
        let side = RunSides { left: SideKind::Space(0), right: SideKind::Outside, exterior: true, open_outside_left: None };
        (graph, WallSides { runs: vec![side; widths.len()], spaces: Vec::new() })
    }

    fn tag(detail: &str, bridge: usize) -> OpeningTag {
        OpeningTag { detail: detail.into(), bridge }
    }

    #[test]
    fn one_tagged_opening_spreads_to_its_class() {
        let (g, s) = bridged(&[200.0; 4]);
        assert_eq!(resolve_opening_details(&g, &s, &[tag("J1", 0)]), [RunDetail::Tagged("J1".into()), RunDetail::Spread("J1".into())]);
    }

    #[test]
    fn two_details_in_a_class_leave_the_third_untyped() {
        let (g, s) = bridged(&[200.0; 6]);
        assert_eq!(
            resolve_opening_details(&g, &s, &[tag("J1", 0), tag("J2", 1)]),
            [RunDetail::Tagged("J1".into()), RunDetail::Tagged("J2".into()), RunDetail::Untyped]
        );
    }

    #[test]
    fn two_tags_on_one_opening_conflict() {
        let (g, s) = bridged(&[200.0; 4]);
        assert_eq!(resolve_opening_details(&g, &s, &[tag("J1", 0), tag("J2", 0)]), [RunDetail::Conflict, RunDetail::Untyped]);
        // A tag on a bridge not in the graph is ignored.
        assert_eq!(resolve_opening_details(&g, &s, &[tag("J1", 7)]), [RunDetail::Untyped, RunDetail::Untyped]);
    }

    fn end(detail: &str, bridge: usize, at_b: bool) -> OpeningEndTag {
        OpeningEndTag { detail: detail.into(), bridge, at_b }
    }

    #[test]
    fn a_head_tag_spreads_to_heads_and_never_to_sills() {
        let (g, s) = bridged(&[200.0; 4]);
        // Bridge 0: a is the head; bridge 1: b is the head.
        let r = resolve_opening_end_details(&g, &s, &[end("H", 0, false)], &[Some(true), Some(false)]);
        assert_eq!(r, [[RunDetail::Tagged("H".into()), RunDetail::Untyped], [RunDetail::Untyped, RunDetail::Spread("H".into())]]);
        let r = resolve_opening_end_details(&g, &s, &[end("H", 0, false), end("S", 1, false)], &[Some(true), Some(false)]);
        assert_eq!(
            r,
            [[RunDetail::Tagged("H".into()), RunDetail::Spread("S".into())], [RunDetail::Tagged("S".into()), RunDetail::Spread("H".into())]]
        );
    }

    #[test]
    fn a_level_bridge_shares_one_tag_between_its_ends() {
        let (g, s) = bridged(&[200.0; 2]);
        let r = resolve_opening_end_details(&g, &s, &[end("F", 0, true)], &[None]);
        assert_eq!(r, [[RunDetail::Spread("F".into()), RunDetail::Tagged("F".into())]]);
    }

    #[test]
    fn two_tags_on_one_end_conflict() {
        let (g, s) = bridged(&[200.0; 2]);
        let r = resolve_opening_end_details(&g, &s, &[end("H", 0, false), end("X", 0, false)], &[Some(true)]);
        assert_eq!(r, [[RunDetail::Conflict, RunDetail::Untyped]]);
        assert_eq!(resolve_opening_end_details(&g, &s, &[end("H", 3, false)], &[Some(true)]), [[RunDetail::Untyped, RunDetail::Untyped]]);
    }

    #[test]
    fn an_opening_in_another_width_is_not_spread_to() {
        let (g, s) = bridged(&[200.0, 200.0, 90.0, 90.0, 200.0, 90.0]);
        assert_eq!(resolve_opening_details(&g, &s, &[tag("J1", 0)]), [RunDetail::Tagged("J1".into()), RunDetail::Untyped, RunDetail::Untyped]);
    }
}

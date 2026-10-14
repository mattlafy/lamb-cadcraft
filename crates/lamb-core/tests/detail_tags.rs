//! Greedy detail-tag resolution for edge details (`derivation::detail_tags`, LAMB T342), driven
//! through `wall_graph` + `wall_sides` + `resolve_edge_details`. Tags are placed on the run whose
//! centreline midpoint is at a given point, so a test never depends on run order.

#[path = "common/walls.rs"]
mod common;

use common::{VIEW, add_layer, on_layer, project, wall};
use lamb_core::derivation::detail_tags::{EdgeTag, RunDetail, resolve_edge_details};
use lamb_core::derivation::wall_junctions::{WallGraph, wall_graph};
use lamb_core::derivation::wall_spaces::{SideParams, wall_sides};
use lamb_core::derivation::walls::{RunCurve, WallRunParams};
use lamb_core::model::{AnyView, DrawingEntity, Project};

/// tan(π/8): the bulge of a quarter circle.
const QUARTER: f64 = 0.414_213_562_373_095_1;

use RunDetail::{Conflict, Gap, Spread, Tagged, Untyped};

fn graph(p: &Project) -> WallGraph {
    wall_graph(p, VIEW, &WallRunParams::defaults(&p.layers))
}

fn midpoint(c: &RunCurve) -> (f64, f64) {
    match *c {
        RunCurve::Line { from, to } => ((from.x + to.x) / 2.0, (from.y + to.y) / 2.0),
        RunCurve::Arc { center, radius, start_angle, sweep } => {
            let a = start_angle + sweep / 2.0;
            (center.x + radius * a.cos(), center.y + radius * a.sin())
        }
    }
}

/// The run whose centreline midpoint is at (x, y).
fn run_at(g: &WallGraph, x: f64, y: f64) -> usize {
    let d = |i: usize| {
        let (mx, my) = midpoint(&g.runs[i].centreline);
        (mx - x).hypot(my - y)
    };
    let i = (0..g.runs.len()).min_by(|&a, &b| d(a).total_cmp(&d(b))).expect("no runs");
    assert!(d(i) < 1.0, "no run at ({x}, {y}): {:?}", g.runs.iter().map(|r| midpoint(&r.centreline)).collect::<Vec<_>>());
    i
}

/// Resolves `tags` (detail, midpoint) on `p`; returns the graph and one detail per run.
fn resolve(p: &Project, tags: &[(&str, (f64, f64))]) -> (WallGraph, Vec<RunDetail>) {
    let g = graph(p);
    let s = wall_sides(p, &g, &SideParams::default());
    let tags: Vec<EdgeTag> = tags.iter().map(|&(d, (x, y))| EdgeTag { detail: d.into(), run: run_at(&g, x, y) }).collect();
    let r = resolve_edge_details(&g, &s, &tags);
    assert_eq!(r.len(), g.runs.len());
    (g, r)
}

/// The detail at the run whose midpoint is (x, y).
fn at(g: &WallGraph, r: &[RunDetail], x: f64, y: f64) -> RunDetail {
    r[run_at(g, x, y)].clone()
}

fn s(d: &str) -> String {
    d.into()
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)]
}

/// A closed 5000 × 4000 room at `dx`, 200 mm walls; centrelines x = dx+100, dx+4900, y = 100, 3900.
fn closed_room(id: u64, dx: f64) -> Vec<DrawingEntity> {
    vec![wall(id, &rect(dx, 0.0, dx + 5000.0, 4000.0)), wall(id + 1, &rect(dx + 200.0, 200.0, dx + 4800.0, 3800.0))]
}

/// Midpoints of `closed_room(_, dx)`'s bottom, right, top, left runs.
fn room_mids(dx: f64) -> [(f64, f64); 4] {
    [(dx + 2500.0, 100.0), (dx + 4900.0, 2000.0), (dx + 2500.0, 3900.0), (dx + 100.0, 2000.0)]
}

/// An L-shaped room; six runs r0..r5 in ring order with these midpoints.
const L_MIDS: [(f64, f64); 6] = [(3000.0, 100.0), (5900.0, 1500.0), (4400.0, 2900.0), (2900.0, 4400.0), (1500.0, 5900.0), (100.0, 3000.0)];

fn l_room() -> Vec<DrawingEntity> {
    vec![
        wall(1, &[(0.0, 0.0), (6000.0, 0.0), (6000.0, 3000.0), (3000.0, 3000.0), (3000.0, 6000.0), (0.0, 6000.0), (0.0, 0.0)]),
        wall(2, &[(200.0, 200.0), (5800.0, 200.0), (5800.0, 2800.0), (2800.0, 2800.0), (2800.0, 5800.0), (200.0, 5800.0), (200.0, 200.0)]),
    ]
}

/// A straight 4000 mm garden wall at `y` (faces y, y + 200); midpoint (2000, y + 100).
fn straight_wall(id: u64, y: f64) -> Vec<DrawingEntity> {
    vec![wall(id, &[(0.0, y), (4000.0, y)]), wall(id + 1, &[(0.0, y + 200.0), (4000.0, y + 200.0)])]
}

#[test]
fn one_tag_in_a_closed_room_spreads_to_the_other_three_runs() {
    let p = project(closed_room(1, 0.0));
    let m = room_mids(0.0);
    let (g, r) = resolve(&p, &[("A", m[0])]);
    assert_eq!(g.runs.len(), 4);
    assert_eq!(at(&g, &r, m[0].0, m[0].1), Tagged(s("A")));
    for &(x, y) in &m[1..] {
        assert_eq!(at(&g, &r, x, y), Spread(s("A")), "{r:?}");
    }
}

#[test]
fn one_tag_spreads_to_a_disconnected_room_of_the_same_class() {
    let mut e = closed_room(1, 0.0);
    e.extend(closed_room(3, 8000.0));
    let p = project(e);
    let (g, r) = resolve(&p, &[("A", room_mids(0.0)[1])]);
    assert_eq!(g.runs.len(), 8);
    assert_eq!(r.iter().filter(|d| **d == Tagged(s("A"))).count(), 1, "{r:?}");
    assert_eq!(r.iter().filter(|d| **d == Spread(s("A"))).count(), 7, "{r:?}");
}

#[test]
fn different_tags_on_opposite_runs_leave_the_sides_untyped() {
    let p = project(closed_room(1, 0.0));
    let m = room_mids(0.0);
    let (g, r) = resolve(&p, &[("A", m[0]), ("B", m[2])]);
    assert_eq!(at(&g, &r, m[0].0, m[0].1), Tagged(s("A")));
    assert_eq!(at(&g, &r, m[2].0, m[2].1), Tagged(s("B")));
    assert_eq!(at(&g, &r, m[1].0, m[1].1), Untyped);
    assert_eq!(at(&g, &r, m[3].0, m[3].1), Untyped);
}

#[test]
fn l_shaped_room_fills_only_a_gap_bounded_by_one_detail() {
    let p = project(l_room());
    let (g, r) = resolve(&p, &[("A", L_MIDS[0]), ("A", L_MIDS[2]), ("B", L_MIDS[4])]);
    assert_eq!(g.runs.len(), 6);
    let want = [Tagged(s("A")), Gap(s("A")), Tagged(s("A")), Untyped, Tagged(s("B")), Untyped];
    for (k, &(x, y)) in L_MIDS.iter().enumerate() {
        assert_eq!(at(&g, &r, x, y), want[k], "r{k}: {r:?}");
    }
}

#[test]
fn a_tag_out_to_a_free_end_takes_the_stretch() {
    // A U-shaped garden wall (runs left, bottom, right; free ends at y = 3000) and a straight
    // garden wall far above, so the class has two details and rule 3 decides.
    let mut e = vec![
        wall(1, &[(0.0, 3000.0), (0.0, 0.0), (4000.0, 0.0), (4000.0, 3000.0)]),
        wall(2, &[(200.0, 3000.0), (200.0, 200.0), (3800.0, 200.0), (3800.0, 3000.0)]),
    ];
    e.extend(straight_wall(3, 10000.0));
    let p = project(e);
    let (g, r) = resolve(&p, &[("A", (2000.0, 100.0)), ("B", (2000.0, 10100.0))]);
    assert_eq!(g.runs.len(), 4, "{:?}", g.runs);
    assert!(!g.free_ends.is_empty());
    assert_eq!(at(&g, &r, 100.0, 1600.0), Gap(s("A")), "{r:?}");
    assert_eq!(at(&g, &r, 3900.0, 1600.0), Gap(s("A")), "{r:?}");
    assert_eq!(at(&g, &r, 2000.0, 10100.0), Tagged(s("B")));
}

/// Two rooms 0..10000 × 0..4000 sharing an interior wall (centreline x = 5000).
fn two_rooms() -> Project {
    project(vec![
        wall(1, &rect(0.0, 0.0, 10000.0, 4000.0)),
        wall(2, &[(4900.0, 200.0), (200.0, 200.0), (200.0, 3800.0), (4900.0, 3800.0)]),
        wall(3, &[(5100.0, 200.0), (9800.0, 200.0), (9800.0, 3800.0), (5100.0, 3800.0)]),
        on_layer(4, "layer-wall-ext", &[(4900.0, 200.0), (4900.0, 3800.0)], &[]),
        on_layer(5, "layer-wall-ext", &[(5100.0, 200.0), (5100.0, 3800.0)], &[]),
    ])
}

#[test]
fn partition_and_exterior_tags_never_cross_classes() {
    let p = two_rooms();
    let part = (5000.0, 2000.0);
    let ext = (100.0, 2000.0);

    let (g, r) = resolve(&p, &[("P", part)]);
    let pi = run_at(&g, part.0, part.1);
    assert_eq!(r[pi], Tagged(s("P")));
    assert!(r.iter().enumerate().all(|(i, d)| i == pi || *d == Untyped), "{r:?}");

    let (_, r) = resolve(&p, &[("E", ext)]);
    assert_eq!(r[pi], Untyped, "{r:?}");
    assert!(r.iter().enumerate().all(|(i, d)| i == pi || matches!(d, Tagged(x) | Spread(x) if x == "E")), "{r:?}");

    let (_, r) = resolve(&p, &[("E", ext), ("P", part)]);
    assert_eq!(r[pi], Tagged(s("P")));
    assert!(r.iter().enumerate().all(|(i, d)| i == pi || matches!(d, Tagged(x) | Spread(x) if x == "E")), "{r:?}");
}

/// A straight garden wall with a 900 mm gap from x = 2000 to 2900, and — when `with_door` — a door
/// hinged at (2000, 200) spanning it; plus a straight wall far above.
fn wall_with_door(with_door: bool) -> Project {
    let mut p = project(Vec::new());
    let d = add_layer(&mut p, "DOOR");
    let mut e = vec![
        wall(1, &[(0.0, 0.0), (2000.0, 0.0)]),
        wall(2, &[(0.0, 200.0), (2000.0, 200.0)]),
        wall(3, &[(2900.0, 0.0), (5000.0, 0.0)]),
        wall(4, &[(2900.0, 200.0), (5000.0, 200.0)]),
    ];
    e.extend(straight_wall(5, 10000.0));
    if with_door {
        e.push(on_layer(20, &d, &[(2000.0, 200.0), (2000.0, 1100.0)], &[]));
        e.push(on_layer(21, &d, &[(2000.0, 1100.0), (2900.0, 200.0)], &[-QUARTER]));
    }
    if let AnyView::Plan(v) = &mut p.views[0] {
        v.entities = e;
    }
    p
}

#[test]
fn a_door_bridge_keeps_the_runs_either_side_adjacent() {
    let tags = [("A", (1000.0, 100.0)), ("B", (2000.0, 10100.0))];
    let p = wall_with_door(true);
    let (g, r) = resolve(&p, &tags);
    assert_eq!(g.bridges.len(), 1, "{g:?}");
    assert_eq!(at(&g, &r, 3950.0, 100.0), Gap(s("A")), "{r:?}");

    // Without the door the gap is undrawn: the far run is a stretch with no tagged neighbour.
    let p = wall_with_door(false);
    let (g, r) = resolve(&p, &tags);
    assert!(g.bridges.is_empty());
    assert_eq!(at(&g, &r, 3950.0, 100.0), Untyped, "{r:?}");

    // With a single detail in the class, the spread crosses too.
    let (g, r) = resolve(&wall_with_door(true), &tags[..1]);
    assert_eq!(at(&g, &r, 3950.0, 100.0), Spread(s("A")), "{r:?}");
}

/// The L room's resolution as (rounded midpoint, detail), sorted — comparable across run orders.
fn keyed(g: &WallGraph, r: &[RunDetail]) -> Vec<((i64, i64), RunDetail)> {
    let mut v: Vec<_> = g
        .runs
        .iter()
        .zip(r)
        .map(|(run, d)| {
            let (x, y) = midpoint(&run.centreline);
            ((x.round() as i64, y.round() as i64), d.clone())
        })
        .collect();
    v.sort_by_key(|a| a.0);
    v
}

#[test]
fn two_different_tags_on_one_run_conflict_and_order_does_not_matter() {
    let tags = [("A", L_MIDS[0]), ("B", L_MIDS[0]), ("A", L_MIDS[0]), ("A", L_MIDS[2]), ("C", L_MIDS[4])];
    let p = project(l_room());
    let (g, r) = resolve(&p, &tags);
    assert_eq!(at(&g, &r, L_MIDS[0].0, L_MIDS[0].1), Conflict);
    // r1 is bounded by the conflicting run (A, B) and r2 (A): two details, untyped.
    assert_eq!(at(&g, &r, L_MIDS[1].0, L_MIDS[1].1), Untyped);
    assert_eq!(at(&g, &r, L_MIDS[2].0, L_MIDS[2].1), Tagged(s("A")));
    let base = keyed(&g, &r);

    let mut q = p.clone();
    if let AnyView::Plan(v) = &mut q.views[0] {
        v.entities.reverse();
    }
    let mut rev = tags;
    rev.reverse();
    let (g2, r2) = resolve(&q, &rev);
    assert_eq!(keyed(&g2, &r2), base);
    let (g3, r3) = resolve(&p, &rev);
    assert_eq!(keyed(&g3, &r3), base);
}

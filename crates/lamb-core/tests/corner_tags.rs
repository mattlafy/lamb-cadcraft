//! Greedy detail-tag resolution for corner details (`derivation::detail_tags`, LAMB T348), driven
//! through `wall_graph` + `wall_sides` + `resolve_corner_details`. Tags are placed on the junction
//! nearest a given point, so a test never depends on junction order. The room builders mirror
//! `detail_tags.rs`'s, which T348 keeps unchanged.

#[path = "common/walls.rs"]
mod common;

use common::{VIEW, add_layer, on_layer, project, wall};
use lamb_core::derivation::detail_tags::{CornerTag, RunDetail, junction_neighbours, resolve_corner_details};
use lamb_core::derivation::wall_junctions::{WallGraph, wall_graph};
use lamb_core::derivation::wall_spaces::{SideParams, wall_sides};
use lamb_core::derivation::walls::WallRunParams;
use lamb_core::model::{AnyView, DrawingEntity, Project};

/// tan(π/8): the bulge of a quarter circle.
const QUARTER: f64 = 0.414_213_562_373_095_1;

use RunDetail::{Conflict, Gap, Spread, Tagged, Untyped};

fn graph(p: &Project) -> WallGraph {
    wall_graph(p, VIEW, &WallRunParams::defaults(&p.layers))
}

/// The junction at (x, y).
fn junction_at(g: &WallGraph, x: f64, y: f64) -> usize {
    let d = |i: usize| (g.junctions[i].point.x - x).hypot(g.junctions[i].point.y - y);
    let i = (0..g.junctions.len()).min_by(|&a, &b| d(a).total_cmp(&d(b))).expect("no junctions");
    assert!(d(i) < 1.0, "no junction at ({x}, {y}): {:?}", g.junctions.iter().map(|j| j.point).collect::<Vec<_>>());
    i
}

/// Resolves `tags` (detail, junction point) on `p`; returns the graph and one detail per junction.
fn resolve(p: &Project, tags: &[(&str, (f64, f64))]) -> (WallGraph, Vec<RunDetail>) {
    let g = graph(p);
    let s = wall_sides(p, &g, &SideParams::default());
    let tags: Vec<CornerTag> = tags.iter().map(|&(d, (x, y))| CornerTag { detail: d.into(), junction: junction_at(&g, x, y) }).collect();
    let r = resolve_corner_details(&g, &s, &tags);
    assert_eq!(r.len(), g.junctions.len());
    (g, r)
}

fn at(g: &WallGraph, r: &[RunDetail], (x, y): (f64, f64)) -> RunDetail {
    r[junction_at(g, x, y)].clone()
}

fn s(d: &str) -> String {
    d.into()
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)]
}

/// A closed 5000 × 4000 room at `dx`, 200 mm walls.
fn closed_room(id: u64, dx: f64) -> Vec<DrawingEntity> {
    vec![wall(id, &rect(dx, 0.0, dx + 5000.0, 4000.0)), wall(id + 1, &rect(dx + 200.0, 200.0, dx + 4800.0, 3800.0))]
}

/// `closed_room(_, dx)`'s corners: bottom-left, bottom-right, top-right, top-left.
fn room_corners(dx: f64) -> [(f64, f64); 4] {
    [(dx + 100.0, 100.0), (dx + 4900.0, 100.0), (dx + 4900.0, 3900.0), (dx + 100.0, 3900.0)]
}

/// An L-shaped room's five convex corners and its one reflex corner.
const L_CONVEX: [(f64, f64); 5] = [(100.0, 100.0), (5900.0, 100.0), (5900.0, 2900.0), (2900.0, 5900.0), (100.0, 5900.0)];
const L_REFLEX: (f64, f64) = (2900.0, 2900.0);

fn l_room() -> Vec<DrawingEntity> {
    vec![
        wall(1, &[(0.0, 0.0), (6000.0, 0.0), (6000.0, 3000.0), (3000.0, 3000.0), (3000.0, 6000.0), (0.0, 6000.0), (0.0, 0.0)]),
        wall(2, &[(200.0, 200.0), (5800.0, 200.0), (5800.0, 2800.0), (2800.0, 2800.0), (2800.0, 5800.0), (200.0, 5800.0), (200.0, 200.0)]),
    ]
}

#[test]
fn one_tag_in_a_rectangular_room_spreads_to_the_other_three_corners() {
    let p = project(closed_room(1, 0.0));
    let c = room_corners(0.0);
    let (g, r) = resolve(&p, &[("A", c[0])]);
    assert_eq!(g.junctions.len(), 4);
    assert_eq!(at(&g, &r, c[0]), Tagged(s("A")));
    for &q in &c[1..] {
        assert_eq!(at(&g, &r, q), Spread(s("A")), "{r:?}");
    }
}

#[test]
fn one_tag_spreads_to_a_disconnected_room_of_the_same_walls() {
    let mut e = closed_room(1, 0.0);
    e.extend(closed_room(3, 8000.0));
    let (g, r) = resolve(&project(e), &[("A", room_corners(8000.0)[2])]);
    assert_eq!(g.junctions.len(), 8);
    assert_eq!(r.iter().filter(|d| **d == Tagged(s("A"))).count(), 1, "{r:?}");
    assert_eq!(r.iter().filter(|d| **d == Spread(s("A"))).count(), 7, "{r:?}");
}

#[test]
fn l_room_convex_tag_spreads_to_convex_corners_only() {
    let p = project(l_room());
    let (g, r) = resolve(&p, &[("A", L_CONVEX[2])]);
    assert_eq!(g.junctions.len(), 6);
    assert_eq!(at(&g, &r, L_CONVEX[2]), Tagged(s("A")));
    for (k, &q) in L_CONVEX.iter().enumerate().filter(|&(k, _)| k != 2) {
        assert_eq!(at(&g, &r, q), Spread(s("A")), "convex {k}: {r:?}");
    }
    assert_eq!(at(&g, &r, L_REFLEX), Untyped, "{r:?}");

    // And the reflex corner's own tag stays on it.
    let (g, r) = resolve(&p, &[("A", L_CONVEX[0]), ("R", L_REFLEX)]);
    assert_eq!(at(&g, &r, L_REFLEX), Tagged(s("R")));
    assert!(L_CONVEX[1..].iter().all(|&q| at(&g, &r, q) == Spread(s("A"))), "{r:?}");
}

#[test]
fn different_tags_on_opposite_corners_leave_the_others_untyped() {
    let p = project(closed_room(1, 0.0));
    let c = room_corners(0.0);
    let (g, r) = resolve(&p, &[("A", c[0]), ("B", c[2])]);
    assert_eq!(at(&g, &r, c[0]), Tagged(s("A")));
    assert_eq!(at(&g, &r, c[2]), Tagged(s("B")));
    assert_eq!(at(&g, &r, c[1]), Untyped);
    assert_eq!(at(&g, &r, c[3]), Untyped);
}

/// Two rooms 0..10000 × 0..4000 sharing a partition (centreline x = 5000): four L corners and two
/// T junctions at (5000, 100) and (5000, 3900).
fn two_rooms() -> Project {
    project(vec![
        wall(1, &rect(0.0, 0.0, 10000.0, 4000.0)),
        wall(2, &[(4900.0, 200.0), (200.0, 200.0), (200.0, 3800.0), (4900.0, 3800.0)]),
        wall(3, &[(5100.0, 200.0), (9800.0, 200.0), (9800.0, 3800.0), (5100.0, 3800.0)]),
        on_layer(4, "layer-wall-ext", &[(4900.0, 200.0), (4900.0, 3800.0)], &[]),
        on_layer(5, "layer-wall-ext", &[(5100.0, 200.0), (5100.0, 3800.0)], &[]),
    ])
}

const TWO_ROOM_LS: [(f64, f64); 4] = [(100.0, 100.0), (9900.0, 100.0), (9900.0, 3900.0), (100.0, 3900.0)];
const TWO_ROOM_TS: [(f64, f64); 2] = [(5000.0, 100.0), (5000.0, 3900.0)];

#[test]
fn a_t_never_takes_an_l_detail_and_an_l_never_a_t_detail() {
    let p = two_rooms();
    let (g, r) = resolve(&p, &[("L", TWO_ROOM_LS[0])]);
    assert_eq!(g.junctions.len(), 6, "{:?}", g.junctions);
    assert!(TWO_ROOM_LS[1..].iter().all(|&q| at(&g, &r, q) == Spread(s("L"))), "{r:?}");
    assert!(TWO_ROOM_TS.iter().all(|&q| at(&g, &r, q) == Untyped), "{r:?}");

    let (g, r) = resolve(&p, &[("T", TWO_ROOM_TS[0])]);
    assert_eq!(at(&g, &r, TWO_ROOM_TS[1]), Spread(s("T")), "{r:?}");
    assert!(TWO_ROOM_LS.iter().all(|&q| at(&g, &r, q) == Untyped), "{r:?}");
}

/// A U-shaped garden wall at `dx` (left, bottom, right; free ends at y = 3000) whose bottom has a
/// 900 mm gap from dx+2000 to dx+2900 — spanned by a door hinged at (dx+2000, 200) when `door`.
/// Its two L corners are (dx+100, 100) and (dx+4900, 100).
fn u_wall(p: &mut Project, id: u64, dx: f64, door: bool) -> Vec<DrawingEntity> {
    let mut e = vec![
        wall(id, &[(dx, 3000.0), (dx, 0.0), (dx + 2000.0, 0.0)]),
        wall(id + 1, &[(dx + 200.0, 3000.0), (dx + 200.0, 200.0), (dx + 2000.0, 200.0)]),
        wall(id + 2, &[(dx + 2900.0, 0.0), (dx + 5000.0, 0.0), (dx + 5000.0, 3000.0)]),
        wall(id + 3, &[(dx + 2900.0, 200.0), (dx + 4800.0, 200.0), (dx + 4800.0, 3000.0)]),
    ];
    if door {
        let d = add_layer(p, "DOOR");
        e.push(on_layer(id + 4, &d, &[(dx + 2000.0, 200.0), (dx + 2000.0, 1100.0)], &[]));
        e.push(on_layer(id + 5, &d, &[(dx + 2000.0, 1100.0), (dx + 2900.0, 200.0)], &[-QUARTER]));
    }
    e
}

/// A door-bridged U at x = 0 (when `door`) and a plain U far to the right, so the class holds two
/// details once both are tagged and rule 3 decides.
fn u_walls(door: bool) -> Project {
    let mut p = project(Vec::new());
    let mut e = u_wall(&mut p, 1, 0.0, door);
    e.extend(u_wall(&mut p, 11, 20000.0, false));
    if let AnyView::Plan(v) = &mut p.views[0] {
        v.entities = e;
    }
    p
}

#[test]
fn a_door_bridged_run_keeps_its_end_corners_neighbours() {
    let tags = [("A", (100.0, 100.0)), ("B", (20100.0, 100.0))];
    let p = u_walls(true);
    let g = graph(&p);
    assert_eq!(g.bridges.len(), 1, "{g:?}");
    let (a, b) = (junction_at(&g, 100.0, 100.0), junction_at(&g, 4900.0, 100.0));
    assert!(junction_neighbours(&g)[a].contains(&b));
    let (g, r) = resolve(&p, &tags);
    assert_eq!(at(&g, &r, (4900.0, 100.0)), Gap(s("A")), "{r:?}");

    // Without the door the gap is undrawn: the far corner has no tagged neighbour.
    let p = u_walls(false);
    let g = graph(&p);
    assert!(g.bridges.is_empty());
    assert!(junction_neighbours(&g).iter().all(|n| n.is_empty()), "{:?}", junction_neighbours(&g));
    let (g, r) = resolve(&p, &tags);
    assert_eq!(at(&g, &r, (4900.0, 100.0)), Untyped, "{r:?}");
}

/// The resolution as (rounded junction point, detail), sorted — comparable across junction orders.
fn keyed(g: &WallGraph, r: &[RunDetail]) -> Vec<((i64, i64), RunDetail)> {
    let mut v: Vec<_> = g.junctions.iter().zip(r).map(|(j, d)| ((j.point.x.round() as i64, j.point.y.round() as i64), d.clone())).collect();
    v.sort_by_key(|a| a.0);
    v
}

#[test]
fn two_different_tags_on_one_junction_conflict_and_order_does_not_matter() {
    let tags = [("A", L_CONVEX[0]), ("B", L_CONVEX[0]), ("A", L_CONVEX[0]), ("A", L_CONVEX[2]), ("C", L_CONVEX[4])];
    let p = project(l_room());
    let (g, r) = resolve(&p, &tags);
    assert_eq!(at(&g, &r, L_CONVEX[0]), Conflict);
    // L_CONVEX[1] lies between the conflicting corner (A, B) and L_CONVEX[2] (A): untyped.
    assert_eq!(at(&g, &r, L_CONVEX[1]), Untyped, "{r:?}");
    assert_eq!(at(&g, &r, L_CONVEX[2]), Tagged(s("A")));
    assert_eq!(at(&g, &r, L_CONVEX[4]), Tagged(s("C")));
    // L_CONVEX[3] is bounded by L_CONVEX[4] (C) and the reflex corner, another class: C.
    assert_eq!(at(&g, &r, L_CONVEX[3]), Gap(s("C")), "{r:?}");
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

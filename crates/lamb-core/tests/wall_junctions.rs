//! Wall junctions (`derivation::wall_junctions`, LAMB T335): run ends meeting near one point become
//! one node with an arm signature.

#[path = "common/walls.rs"]
mod common;

use common::{VIEW, project, wall};
use lamb_core::derivation::wall_junctions::{Junction, RunEnd, WallGraph, wall_graph};
use lamb_core::derivation::walls::WallRunParams;
use lamb_core::geometry::Point2;
use lamb_core::model::DrawingEntity;

fn graph(entities: Vec<DrawingEntity>) -> WallGraph {
    let p = project(entities);
    wall_graph(&p, VIEW, &WallRunParams::defaults(&p.layers))
}

fn assert_gaps(j: &Junction, want: &[f64], tol: f64) {
    assert_eq!(j.gaps_deg.len(), want.len(), "{j:?}");
    for (g, w) in j.gaps_deg.iter().zip(want) {
        assert!((g - w).abs() <= tol, "gaps {:?} != {want:?}", j.gaps_deg);
    }
    assert_eq!(j.arms.len(), want.len());
}

fn near(p: Point2, x: f64, y: f64, tol: f64) -> bool {
    (p.x - x).hypot(p.y - y) <= tol
}

fn l_wall(dx: f64) -> Vec<DrawingEntity> {
    let id = if dx == 0.0 { 1 } else { 11 };
    vec![wall(id, &[(dx, 0.0), (dx + 2000.0, 0.0), (dx + 2000.0, 2000.0)]), wall(id + 1, &[(dx, 200.0), (dx + 1800.0, 200.0), (dx + 1800.0, 2000.0)])]
}

#[test]
fn l_corner_is_one_node() {
    let g = graph(l_wall(0.0));
    assert_eq!(g.runs.len(), 2, "{:?}", g.runs);
    assert_eq!(g.junctions.len(), 1, "{:?}", g.junctions);
    let j = &g.junctions[0];
    assert_gaps(j, &[90.0, 270.0], 1e-6);
    assert!(near(j.point, 1900.0, 100.0, 1.0), "{j:?}");
    assert_eq!(g.free_ends, vec![(0, RunEnd::Start), (1, RunEnd::End)]);
}

#[test]
fn t_from_one_straight_face_and_two_l_polylines_is_one_node() {
    let g = graph(vec![
        wall(1, &[(0.0, 0.0), (10000.0, 0.0)]),
        wall(2, &[(0.0, -200.0), (4950.0, -200.0), (4950.0, -5000.0)]),
        wall(3, &[(5050.0, -5000.0), (5050.0, -200.0), (10000.0, -200.0)]),
    ]);
    assert_eq!(g.runs.len(), 3, "{:?}", g.runs);
    assert_eq!(g.junctions.len(), 1, "{:?}", g.junctions);
    let j = &g.junctions[0];
    assert_gaps(j, &[90.0, 90.0, 180.0], 1e-6);
    assert!(near(j.point, 5000.0, -100.0, 1.0), "{j:?}");
    let mut runs: Vec<usize> = j.arms.iter().map(|a| a.run).collect();
    runs.sort();
    assert_eq!(runs, vec![0, 1, 2]);
    assert_eq!(g.free_ends.len(), 3, "{:?}", g.free_ends);
}

fn x_wall() -> Vec<DrawingEntity> {
    vec![
        wall(1, &[(100.0, 3000.0), (100.0, 100.0), (3000.0, 100.0)]),
        wall(2, &[(-3000.0, 100.0), (-100.0, 100.0), (-100.0, 3000.0)]),
        wall(3, &[(-100.0, -3000.0), (-100.0, -100.0), (-3000.0, -100.0)]),
        wall(4, &[(3000.0, -100.0), (100.0, -100.0), (100.0, -3000.0)]),
    ]
}

#[test]
fn x_from_four_l_polylines_is_one_node() {
    let g = graph(x_wall());
    assert_eq!(g.runs.len(), 4, "{:?}", g.runs);
    assert_eq!(g.junctions.len(), 1, "{:?}", g.junctions);
    assert_gaps(&g.junctions[0], &[90.0, 90.0, 90.0, 90.0], 1e-6);
    assert!(near(g.junctions[0].point, 0.0, 0.0, 1.0));
    assert_eq!(g.free_ends.len(), 4);
}

#[test]
fn a_100_degree_corner_has_gaps_100_and_260() {
    let a = 80f64.to_radians(); // the second leg's heading; the corner between the arms is 100°
    let (c, s) = (a.cos(), a.sin());
    let t = 200.0 * (1.0 - c) / s;
    let inner = (5000.0 - 200.0 * s + t * c, 200.0);
    let g = graph(vec![
        wall(1, &[(0.0, 0.0), (5000.0, 0.0), (5000.0 + 4000.0 * c, 4000.0 * s)]),
        wall(2, &[(0.0, 200.0), inner, (inner.0 + 4000.0 * c, inner.1 + 4000.0 * s)]),
    ]);
    assert_eq!(g.runs.len(), 2, "{:?}", g.runs);
    assert_eq!(g.junctions.len(), 1, "{:?}", g.junctions);
    assert_gaps(&g.junctions[0], &[100.0, 260.0], 0.1);
}

#[test]
fn u_from_two_u_polylines_is_two_nodes_and_two_free_ends() {
    let g = graph(vec![
        wall(1, &[(0.0, 4000.0), (0.0, 0.0), (5000.0, 0.0), (5000.0, 4000.0)]),
        wall(2, &[(200.0, 4000.0), (200.0, 200.0), (4800.0, 200.0), (4800.0, 4000.0)]),
    ]);
    assert_eq!(g.runs.len(), 3, "{:?}", g.runs);
    assert_eq!(g.junctions.len(), 2, "{:?}", g.junctions);
    for j in &g.junctions {
        assert_gaps(j, &[90.0, 270.0], 1e-6);
    }
    assert_eq!(g.free_ends.len(), 2, "{:?}", g.free_ends);
}

#[test]
fn a_lone_straight_wall_has_no_node_and_two_free_ends() {
    let g = graph(vec![wall(1, &[(0.0, 0.0), (5000.0, 0.0)]), wall(2, &[(0.0, 200.0), (5000.0, 200.0)])]);
    assert!(g.junctions.is_empty());
    assert_eq!(g.free_ends, vec![(0, RunEnd::Start), (0, RunEnd::End)]);
}

#[test]
fn collinear_runs_across_an_undrawn_door_do_not_join() {
    let g = graph(vec![
        wall(1, &[(0.0, 0.0), (3000.0, 0.0)]),
        wall(2, &[(0.0, 200.0), (3000.0, 200.0)]),
        wall(3, &[(3900.0, 0.0), (7000.0, 0.0)]),
        wall(4, &[(3900.0, 200.0), (7000.0, 200.0)]),
    ]);
    assert_eq!(g.runs.len(), 2, "{:?}", g.runs);
    assert!(g.junctions.is_empty(), "{:?}", g.junctions);
    assert_eq!(g.free_ends.len(), 4);
}

#[test]
fn two_ls_3000_apart_are_never_merged() {
    let mut e = l_wall(0.0);
    e.extend(l_wall(3000.0));
    let g = graph(e);
    assert_eq!(g.runs.len(), 4, "{:?}", g.runs);
    assert_eq!(g.junctions.len(), 2, "{:?}", g.junctions);
    assert!(near(g.junctions[0].point, 1900.0, 100.0, 1.0) && near(g.junctions[1].point, 4900.0, 100.0, 1.0), "{:?}", g.junctions);
    assert_eq!(g.free_ends.len(), 4);
}

#[test]
fn output_is_identical_with_entity_order_shuffled() {
    let first = graph(x_wall());
    let mut shuffled = x_wall();
    shuffled.reverse();
    shuffled.swap(0, 2);
    assert_eq!(first, graph(shuffled));
}

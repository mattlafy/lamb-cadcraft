//! Wall openings (`derivation::wall_openings`, LAMB T336): a gap between facing run ends is
//! bridged only where a drawn window or door spans it; tangent continuations join as N = 2 nodes.

#[path = "common/walls.rs"]
mod common;

use common::{VIEW, add_layer, on_layer, project, wall};
use lamb_core::derivation::wall_junctions::{RunEnd, WallGraph, wall_graph};
use lamb_core::derivation::walls::WallRunParams;
use lamb_core::model::{DrawingEntity, Project};

/// tan(π/8): the bulge of a quarter circle.
const QUARTER: f64 = 0.414_213_562_373_095_1;

fn graph_of(p: &Project) -> WallGraph {
    wall_graph(p, VIEW, &WallRunParams::defaults(&p.layers))
}

/// A project of `walls` plus opening layers WINDOW and DOOR, then `extra` built against them.
fn build(walls: Vec<DrawingEntity>, extra: impl FnOnce(&str, &str) -> Vec<DrawingEntity>) -> Project {
    let mut p = project(Vec::new());
    let window = add_layer(&mut p, "WINDOW");
    let door = add_layer(&mut p, "DOOR");
    let mut ents = walls;
    ents.extend(extra(&window, &door));
    if let lamb_core::model::AnyView::Plan(v) = &mut p.views[0] {
        v.entities = ents;
    }
    p
}

/// Two collinear 200 mm wall runs along x, a 900 mm gap from x = 2000 to 2900.
fn gapped_walls() -> Vec<DrawingEntity> {
    vec![
        wall(1, &[(0.0, 0.0), (2000.0, 0.0)]),
        wall(2, &[(0.0, 200.0), (2000.0, 200.0)]),
        wall(3, &[(2900.0, 0.0), (5000.0, 0.0)]),
        wall(4, &[(2900.0, 200.0), (5000.0, 200.0)]),
    ]
}

/// A door hinged at (2000, 200): leaf line out to (2000, 1100), swing arc back to (2900, 200).
fn door(layer: &str) -> Vec<DrawingEntity> {
    vec![on_layer(20, layer, &[(2000.0, 200.0), (2000.0, 1100.0)], &[]), on_layer(21, layer, &[(2000.0, 1100.0), (2900.0, 200.0)], &[-QUARTER])]
}

fn assert_one_bridge(g: &WallGraph, ids: &[u64]) {
    assert_eq!(g.runs.len(), 2, "{:?}", g.runs);
    assert_eq!(g.bridges.len(), 1, "{:?}", g.bridges);
    let b = &g.bridges[0];
    assert_eq!(b.opening_entity_ids.iter().map(|i| i.0).collect::<Vec<_>>(), ids);
    // Run 0 (faces 1/2) ends at x = 2000, run 1 (faces 3/4) starts at x = 2900.
    assert_eq!((b.a, b.b), ((0, RunEnd::End), (1, RunEnd::Start)));
    assert!((b.from.x - 2000.0).abs() < 1e-6 && (b.from.y - 100.0).abs() < 1e-6, "{b:?}");
    assert!((b.to.x - 2900.0).abs() < 1e-6 && (b.to.y - 100.0).abs() < 1e-6, "{b:?}");
    assert_eq!(g.free_ends, vec![(0, RunEnd::Start), (1, RunEnd::End)]);
}

#[test]
fn door_as_leaf_and_swing_bridges_the_gap() {
    let p = build(gapped_walls(), |_, d| door(d));
    assert_one_bridge(&graph_of(&p), &[20, 21]);
}

#[test]
fn window_polyline_bridges_only_on_an_opening_layer() {
    let win = |layer: &str| vec![on_layer(30, layer, &[(2000.0, 100.0), (2900.0, 100.0)], &[])];
    assert_one_bridge(&graph_of(&build(gapped_walls(), |w, _| win(w))), &[30]);

    let g = graph_of(&build(gapped_walls(), |_, _| win("layer-0")));
    assert!(g.bridges.is_empty(), "{:?}", g.bridges);
    assert_eq!(g.free_ends.len(), 4);

    let g = graph_of(&build(gapped_walls(), |_, _| Vec::new()));
    assert!(g.bridges.is_empty());
    assert_eq!(g.free_ends, vec![(0, RunEnd::Start), (0, RunEnd::End), (1, RunEnd::Start), (1, RunEnd::End)]);
}

#[test]
fn window_short_of_a_jamb_bridges_within_tolerance_only() {
    let short = |gap: f64| move |w: &str, _: &str| vec![on_layer(30, w, &[(2000.0 + gap, 100.0), (2900.0, 100.0)], &[])];
    assert_one_bridge(&graph_of(&build(gapped_walls(), short(30.0))), &[30]);
    let g = graph_of(&build(gapped_walls(), short(120.0)));
    assert!(g.bridges.is_empty(), "{:?}", g.bridges);
    assert_eq!(g.free_ends.len(), 4);
}

/// A 5000 × 4000 room, 200 mm walls, a 900 mm door in the bottom wall from x = 2000 to 2900.
fn room_with_door() -> Vec<DrawingEntity> {
    vec![
        wall(1, &[(2900.0, 0.0), (5000.0, 0.0), (5000.0, 4000.0), (0.0, 4000.0), (0.0, 0.0), (2000.0, 0.0)]),
        wall(2, &[(2900.0, 200.0), (4800.0, 200.0), (4800.0, 3800.0), (200.0, 3800.0), (200.0, 200.0), (2000.0, 200.0)]),
    ]
}

#[test]
fn closed_room_with_a_door() {
    let p = build(room_with_door(), |_, d| door(d));
    let g = graph_of(&p);
    assert_eq!(g.runs.len(), 5, "{:?}", g.runs);
    assert_eq!(g.junctions.len(), 4, "{:?}", g.junctions);
    assert_eq!(g.bridges.len(), 1, "{:?}", g.bridges);
    assert!(g.free_ends.is_empty(), "{:?}", g.free_ends);
}

fn assert_continuation(g: &WallGraph) {
    assert_eq!(g.runs.len(), 2, "{:?}", g.runs);
    assert_eq!(g.junctions.len(), 1, "{:?}", g.junctions);
    let j = &g.junctions[0];
    assert_eq!(j.arms.len(), 2);
    for gap in &j.gaps_deg {
        assert!((gap - 180.0).abs() <= 0.1, "{:?}", j.gaps_deg);
    }
    // Only the two outer ends are free: the continuation is a node, not a gap.
    assert_eq!(g.free_ends.len(), 2, "{:?}", g.free_ends);
    assert!(g.bridges.is_empty());
}

#[test]
fn straight_wall_continuing_tangentially_into_an_arc_is_one_node() {
    // Faces at y = 0 and y = 200 run to x = 2000, then turn CCW about (2000, 2000) up to x = 4000.
    let p = build(
        vec![
            on_layer(1, "layer-wall-ext", &[(0.0, 0.0), (2000.0, 0.0), (4000.0, 2000.0)], &[0.0, QUARTER]),
            on_layer(2, "layer-wall-ext", &[(0.0, 200.0), (2000.0, 200.0), (3800.0, 2000.0)], &[0.0, QUARTER]),
        ],
        |_, _| Vec::new(),
    );
    assert_continuation(&graph_of(&p));
}

#[test]
fn face_split_into_collinear_lines_is_one_node() {
    let p = build(
        vec![wall(1, &[(0.0, 0.0), (1000.0, 0.0)]), wall(2, &[(1000.0, 0.0), (2000.0, 0.0)]), wall(3, &[(0.0, 200.0), (2000.0, 200.0)])],
        |_, _| Vec::new(),
    );
    assert_continuation(&graph_of(&p));
}

#[test]
fn output_independent_of_entity_order() {
    let base = build(room_with_door(), |_, d| door(d));
    let mut shuffled = base.clone();
    if let lamb_core::model::AnyView::Plan(v) = &mut shuffled.views[0] {
        v.entities.reverse();
        v.entities.swap(0, 1);
    }
    assert_eq!(graph_of(&base), graph_of(&shuffled));
    let a = build(gapped_walls(), |_, d| door(d));
    let mut b = a.clone();
    if let lamb_core::model::AnyView::Plan(v) = &mut b.views[0] {
        v.entities.reverse();
    }
    assert_eq!(graph_of(&a), graph_of(&b));
}

/// A point at `r` mm and `deg` degrees about `c`.
fn polar(c: (f64, f64), r: f64, deg: f64) -> (f64, f64) {
    let a = deg.to_radians();
    (c.0 + r * a.cos(), c.1 + r * a.sin())
}

/// A 200 mm arc wall (faces at radius 5000 and 5200 about `c`) swept CCW from `from` to `to` degrees.
fn arc_wall(id: u64, c: (f64, f64), from: f64, to: f64) -> Vec<DrawingEntity> {
    let b = ((to - from).to_radians() / 4.0).tan();
    [5000.0, 5200.0]
        .iter()
        .enumerate()
        .map(|(k, r)| on_layer(id + k as u64, "layer-wall-ext", &[polar(c, *r, from), polar(c, *r, to)], &[b]))
        .collect()
}

#[test]
fn a_window_across_a_gap_in_an_arc_wall_bridges_it() {
    const O: (f64, f64) = (0.0, 0.0);
    let walls = || [arc_wall(1, O, 0.0, 40.0), arc_wall(3, O, 50.0, 90.0)].concat();
    // A window drawn along the centreline circle across the 10° gap.
    let win = |w: &str| vec![on_layer(30, w, &[polar(O, 5100.0, 40.0), polar(O, 5100.0, 50.0)], &[(10f64.to_radians() / 4.0).tan()])];
    let g = graph_of(&build(walls(), |w, _| win(w)));
    assert_eq!(g.runs.len(), 2, "{:?}", g.runs);
    assert_eq!(g.bridges.len(), 1, "{:?}", g.bridges);
    let b = &g.bridges[0];
    assert_eq!(b.opening_entity_ids.iter().map(|i| i.0).collect::<Vec<_>>(), vec![30]);
    let near = |p: lamb_core::geometry::Point2, q: (f64, f64)| (p.x - q.0).hypot(p.y - q.1) < 1e-6;
    let (e40, e50) = (polar(O, 5100.0, 40.0), polar(O, 5100.0, 50.0));
    assert!((near(b.from, e40) && near(b.to, e50)) || (near(b.from, e50) && near(b.to, e40)), "{b:?}");
    assert_eq!(g.free_ends.len(), 2, "{:?}", g.free_ends);

    // The same gap with no opening stays two free ends.
    let g = graph_of(&build(walls(), |_, _| Vec::new()));
    assert!(g.bridges.is_empty(), "{:?}", g.bridges);
    assert_eq!(g.free_ends.len(), 4);
}

#[test]
fn arc_ends_on_different_circles_or_facing_a_straight_end_are_not_bridged() {
    const O: (f64, f64) = (0.0, 0.0);
    // A window straight between the two centreline ends, spanning both caps.
    let win = |a: (f64, f64), b: (f64, f64)| move |w: &str, _: &str| vec![on_layer(30, w, &[a, b], &[])];

    // Second arc about a centre 300 mm away.
    let c2 = (300.0, 0.0);
    let p = build([arc_wall(1, O, 0.0, 40.0), arc_wall(3, c2, 50.0, 90.0)].concat(), win(polar(O, 5100.0, 40.0), polar(c2, 5100.0, 50.0)));
    let g = graph_of(&p);
    assert_eq!(g.runs.len(), 2, "{:?}", g.runs);
    assert!(g.bridges.is_empty(), "{:?}", g.bridges);

    // A straight wall leaving the 50° point along the circle's tangent.
    let t = (-50f64.to_radians().sin(), 50f64.to_radians().cos());
    let straight = |id: u64, r: f64| {
        let s = polar(O, r, 50.0);
        on_layer(id, "layer-wall-ext", &[s, (s.0 + 2000.0 * t.0, s.1 + 2000.0 * t.1)], &[])
    };
    let mut walls = arc_wall(1, O, 0.0, 40.0);
    walls.extend([straight(3, 5000.0), straight(4, 5200.0)]);
    let g = graph_of(&build(walls, win(polar(O, 5100.0, 40.0), polar(O, 5100.0, 50.0))));
    assert_eq!(g.runs.len(), 2, "{:?}", g.runs);
    assert!(g.bridges.is_empty(), "{:?}", g.bridges);
}

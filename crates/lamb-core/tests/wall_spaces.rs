//! Inside and outside (`derivation::wall_spaces`, LAMB T337): bounded faces of the wall graph's
//! centreline arrangement are spaces; runs bordering the unbounded face are exterior.

#[path = "common/walls.rs"]
mod common;

use std::f64::consts::PI;

use common::{VIEW, add_layer, on_layer, project, pt, wall};
use lamb_core::derivation::wall_junctions::wall_graph;
use lamb_core::derivation::wall_spaces::{SideKind, SideParams, WallSides, outside_on_left, wall_sides};
use lamb_core::derivation::walls::{RunCurve, WallRunParams};
use lamb_core::geometry::Point2;
use lamb_core::model::{AnyView, DrawingEntity, Project};

/// tan(π/8): the bulge of a quarter circle.
const QUARTER: f64 = 0.414_213_562_373_095_1;

fn sides(p: &Project, params: &SideParams) -> WallSides {
    let g = wall_graph(p, VIEW, &WallRunParams::defaults(&p.layers));
    let s = wall_sides(p, &g, params);
    assert_eq!(s.runs.len(), g.runs.len());
    s
}

fn sides_of(entities: Vec<DrawingEntity>) -> WallSides {
    sides(&project(entities), &SideParams::default())
}

/// A project holding `entities` plus a DOOR layer; `doors` builds door entities on it.
fn with_doors(walls: Vec<DrawingEntity>, doors: impl FnOnce(&str) -> Vec<DrawingEntity>) -> Project {
    let mut p = project(Vec::new());
    let door = add_layer(&mut p, "DOOR");
    let mut ents = walls;
    ents.extend(doors(&door));
    if let AnyView::Plan(v) = &mut p.views[0] {
        v.entities = ents;
    }
    p
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)]
}

/// A closed 5000 × 4000 room at `dx`, 200 mm walls; centreline rectangle 4800 × 3800.
fn closed_room(id: u64, dx: f64) -> Vec<DrawingEntity> {
    vec![wall(id, &rect(dx, 0.0, dx + 5000.0, 4000.0)), wall(id + 1, &rect(dx + 200.0, 200.0, dx + 4800.0, 3800.0))]
}

/// `closed_room` with a 900 mm door in the bottom wall from `dx + 2000` to `dx + 2900`.
fn room_with_door(id: u64, dx: f64) -> Vec<DrawingEntity> {
    let s = |pts: &[(f64, f64)]| pts.iter().map(|&(x, y)| (x + dx, y)).collect::<Vec<_>>();
    vec![
        wall(id, &s(&[(2900.0, 0.0), (5000.0, 0.0), (5000.0, 4000.0), (0.0, 4000.0), (0.0, 0.0), (2000.0, 0.0)])),
        wall(id + 1, &s(&[(2900.0, 200.0), (4800.0, 200.0), (4800.0, 3800.0), (200.0, 3800.0), (200.0, 200.0), (2000.0, 200.0)])),
    ]
}

/// A door hinged at (dx + 2000, 200): leaf and swing arc.
fn door(id: u64, layer: &str, dx: f64) -> Vec<DrawingEntity> {
    vec![
        on_layer(id, layer, &[(dx + 2000.0, 200.0), (dx + 2000.0, 1100.0)], &[]),
        on_layer(id + 1, layer, &[(dx + 2000.0, 1100.0), (dx + 2900.0, 200.0)], &[-QUARTER]),
    ]
}

/// Two rooms 0..10000 × 0..4000 sharing the wall at x = 4900..5100, drawn as its own two faces on
/// `shared_layer`.
fn two_rooms(shared_layer: &str) -> Vec<DrawingEntity> {
    vec![
        wall(1, &rect(0.0, 0.0, 10000.0, 4000.0)),
        wall(2, &[(4900.0, 200.0), (200.0, 200.0), (200.0, 3800.0), (4900.0, 3800.0)]),
        wall(3, &[(5100.0, 200.0), (9800.0, 200.0), (9800.0, 3800.0), (5100.0, 3800.0)]),
        on_layer(4, shared_layer, &[(4900.0, 200.0), (4900.0, 3800.0)], &[]),
        on_layer(5, shared_layer, &[(5100.0, 200.0), (5100.0, 3800.0)], &[]),
    ]
}

fn graph_runs(p: &Project) -> Vec<RunCurve> {
    wall_graph(p, VIEW, &WallRunParams::defaults(&p.layers)).runs.iter().map(|r| r.centreline).collect()
}

/// A point 300 mm off the middle of a straight centreline, to its left (`left = true`) or right.
fn beside(c: &RunCurve, left: bool) -> Point2 {
    let RunCurve::Line { from, to } = *c else { panic!("straight run expected: {c:?}") };
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let len = dx.hypot(dy);
    let (nx, ny) = (-dy / len, dx / len);
    let k = if left { 300.0 } else { -300.0 };
    pt((from.x + to.x) / 2.0 + nx * k, (from.y + to.y) / 2.0 + ny * k)
}

fn is_space(k: SideKind) -> bool {
    matches!(k, SideKind::Space(_))
}

#[test]
fn closed_rectangular_room_is_one_space_with_every_run_exterior() {
    let p = project(closed_room(1, 0.0));
    let s = sides(&p, &SideParams::default());
    assert_eq!(s.spaces.len(), 1, "{s:?}");
    let want = 4800.0 * 3800.0;
    assert!((s.spaces[0].area_mm2 - want).abs() <= want * 0.001, "{s:?}");
    assert_eq!(s.spaces[0].runs, vec![0, 1, 2, 3]);
    let curves = graph_runs(&p);
    assert_eq!(s.runs.len(), 4);
    for (r, c) in s.runs.iter().zip(&curves) {
        assert!(r.exterior, "{r:?}");
        assert_eq!([r.left, r.right].iter().filter(|k| **k == SideKind::Outside).count(), 1, "{r:?}");
        // The Outside side faces away from the room's centreline rectangle 100..4900 × 100..3900.
        let q = beside(c, r.left == SideKind::Outside);
        assert!(!(100.0..4900.0).contains(&q.x) || !(100.0..3900.0).contains(&q.y), "{r:?} {c:?} {q:?}");
    }
}

#[test]
fn two_rooms_sharing_a_wall() {
    let p = project(two_rooms("layer-wall-ext"));
    let s = sides(&p, &SideParams::default());
    assert_eq!(s.spaces.len(), 2, "{s:?}");
    let curves = graph_runs(&p);
    let shared: Vec<usize> = curves
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c, RunCurve::Line { from, to } if (from.x - 5000.0).abs() < 1e-6 && (to.x - 5000.0).abs() < 1e-6))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(shared.len(), 1, "{curves:?}");
    for (i, r) in s.runs.iter().enumerate() {
        if i == shared[0] {
            assert!(is_space(r.left) && is_space(r.right) && r.left != r.right && !r.exterior, "{r:?}");
        } else {
            assert!(r.exterior, "run {i}: {r:?}");
        }
    }
    // Centrelines 100..5000 and 5000..9900 by 100..3900.
    for sp in &s.spaces {
        let want = 4900.0 * 3800.0;
        assert!((sp.area_mm2 - want).abs() <= want * 0.001, "{sp:?}");
    }
}

#[test]
fn gate_terrace_two_rooms_with_doors_3000_apart() {
    let mut walls = room_with_door(1, 0.0);
    walls.extend(room_with_door(3, 8000.0));
    let p = with_doors(walls, |d| {
        let mut e = door(20, d, 0.0);
        e.extend(door(22, d, 8000.0));
        e
    });
    let s = sides(&p, &SideParams::default());
    assert_eq!(s.spaces.len(), 2, "{s:?}");
    assert_eq!(s.runs.len(), 10);
    for r in &s.runs {
        assert!(r.exterior, "{r:?}");
        assert!(is_space(r.left) != is_space(r.right), "{r:?}");
    }
    for sp in &s.spaces {
        let want = 4800.0 * 3800.0;
        assert!((sp.area_mm2 - want).abs() <= want * 0.001, "{sp:?}");
    }
}

/// An outer 12000 square ring, an inner courtyard ring (faces 4000/4200..7800/8000), and four cross
/// walls at the midpoints (faces 5900/6100) joining them: four L-shaped rooms and a courtyard.
fn courtyard() -> Vec<DrawingEntity> {
    let room = [(200.0, 200.0), (5900.0, 200.0), (5900.0, 4000.0), (4000.0, 4000.0), (4000.0, 5900.0), (200.0, 5900.0), (200.0, 200.0)];
    // Rotate by quarter turns about (6000, 6000).
    let rot = |k: usize, (x, y): (f64, f64)| (0..k).fold((x, y), |(x, y), _| (12000.0 - y, x));
    let mut e = vec![wall(1, &rect(0.0, 0.0, 12000.0, 12000.0)), wall(2, &rect(4200.0, 4200.0, 7800.0, 7800.0))];
    for k in 0..4 {
        e.push(wall(3 + k as u64, &room.iter().map(|&q| rot(k, q)).collect::<Vec<_>>()));
    }
    e
}

#[test]
fn gate_courtyard_is_a_space_until_picked_as_outside() {
    let p = project(courtyard());
    let g = wall_graph(&p, VIEW, &WallRunParams::defaults(&p.layers));
    assert_eq!((g.runs.len(), g.junctions.len()), (20, 16), "{g:?}");
    let in_courtyard = |c: &RunCurve| match *c {
        RunCurve::Line { from, to } => [from, to].iter().all(|q| (4000.0..=8000.0).contains(&q.x) && (4000.0..=8000.0).contains(&q.y)),
        RunCurve::Arc { .. } => false,
    };
    let court: Vec<usize> = (0..g.runs.len()).filter(|&i| in_courtyard(&g.runs[i].centreline)).collect();
    assert_eq!(court.len(), 8, "{:?}", g.runs);

    let s = wall_sides(&p, &g, &SideParams::default());
    assert_eq!(s.spaces.len(), 5, "{s:?}");
    let want = 3800.0 * 3800.0;
    assert!(s.spaces.iter().any(|sp| (sp.area_mm2 - want).abs() <= want * 0.001), "{s:?}");
    for (i, r) in s.runs.iter().enumerate() {
        assert_eq!(r.exterior, !court.contains(&i) && !is_cross(&g.runs[i].centreline), "run {i}: {r:?}");
    }

    let params = SideParams { outside_points: vec![pt(6000.0, 6000.0)], ..SideParams::default() };
    let s = wall_sides(&p, &g, &params);
    assert_eq!(s.spaces.len(), 4, "{s:?}");
    for (i, r) in s.runs.iter().enumerate() {
        if court.contains(&i) {
            assert!(r.exterior, "run {i}: {r:?}");
            assert_eq!([r.left, r.right].iter().filter(|k| **k == SideKind::Outside).count(), 1, "{r:?}");
            // The Outside side faces the courtyard centre.
            let q = beside(&g.runs[i].centreline, r.left == SideKind::Outside);
            assert!((4100.0..7900.0).contains(&q.x) && (4100.0..7900.0).contains(&q.y), "{r:?} {q:?}");
        }
    }
}

/// A cross wall's centreline lies on x = 6000 or y = 6000.
fn is_cross(c: &RunCurve) -> bool {
    match *c {
        RunCurve::Line { from, to } => {
            ((from.x - 6000.0).abs() < 1e-6 && (to.x - 6000.0).abs() < 1e-6) || ((from.y - 6000.0).abs() < 1e-6 && (to.y - 6000.0).abs() < 1e-6)
        }
        RunCurve::Arc { .. } => false,
    }
}

#[test]
fn lone_garden_wall_is_outside_on_both_sides() {
    let s = sides_of(vec![wall(1, &[(0.0, 0.0), (5000.0, 0.0)]), wall(2, &[(0.0, 200.0), (5000.0, 200.0)])]);
    assert!(s.spaces.is_empty(), "{s:?}");
    assert_eq!(s.runs.len(), 1);
    let r = &s.runs[0];
    assert_eq!((r.left, r.right, r.exterior), (SideKind::Outside, SideKind::Outside, true));
}

#[test]
fn open_run_takes_its_exterior_side_from_an_outside_point_beside_it() {
    let p = project(vec![wall(1, &[(0.0, 0.0), (5000.0, 0.0)]), wall(2, &[(0.0, 200.0), (5000.0, 200.0)])]);
    let c = graph_runs(&p)[0];
    let RunCurve::Line { from, to } = c else { panic!("{c:?}") };
    let one = |pts: Vec<Point2>| {
        let r = sides(&p, &SideParams { outside_points: pts, ..SideParams::default() }).runs[0].clone();
        assert_eq!((r.left, r.right, r.exterior), (SideKind::Outside, SideKind::Outside, true));
        (r.open_outside_left, outside_on_left(&r))
    };
    assert_eq!(one(vec![beside(&c, false)]), (Some(false), Some(false)));
    assert_eq!(one(vec![beside(&c, true)]), (Some(true), Some(true)));
    // The nearest point beside the run wins.
    let far_left = pt(2500.0 + (beside(&c, true).x - 2500.0) * 10.0, 100.0 + (beside(&c, true).y - 100.0) * 10.0);
    assert_eq!(one(vec![far_left, beside(&c, false)]), (Some(false), Some(false)));
    // Beyond the run's end, inside its faces, or no point at all: undecided.
    let beyond = pt(to.x + (to.x - from.x).signum() * 1000.0, 400.0);
    assert_eq!(one(vec![beyond]), (None, None));
    assert_eq!(one(vec![pt(2500.0, 150.0)]), (None, None));
    assert_eq!(one(Vec::new()), (None, None));
}

/// A lone semicircular garden wall about the origin (faces at radii 2000 and 2250, upper half),
/// drawn ccw, or cw when `reversed`.
fn semicircular_garden_wall(reversed: bool) -> Project {
    let arc = |id: u64, r: f64| {
        if reversed {
            on_layer(id, "layer-wall-ext", &[(-r, 0.0), (r, 0.0)], &[-1.0])
        } else {
            on_layer(id, "layer-wall-ext", &[(r, 0.0), (-r, 0.0)], &[1.0])
        }
    };
    project(vec![arc(1, 2250.0), arc(2, 2000.0)])
}

#[test]
fn open_arc_run_takes_its_exterior_side_from_an_outside_point_beside_it() {
    for reversed in [false, true] {
        let p = semicircular_garden_wall(reversed);
        let runs = graph_runs(&p);
        assert_eq!(runs.len(), 1, "{runs:?}");
        let RunCurve::Arc { sweep, .. } = runs[0] else { panic!("{:?}", runs[0]) };
        // Left of a ccw arc's travel is toward the centre.
        let inner_is_left = sweep > 0.0;
        let one = |pts: Vec<Point2>| {
            let r = sides(&p, &SideParams { outside_points: pts, ..SideParams::default() }).runs[0].clone();
            assert_eq!((r.left, r.right, r.exterior), (SideKind::Outside, SideKind::Outside, true));
            let got = r.open_outside_left;
            assert_eq!(got, outside_on_left(&r));
            got
        };
        let (outer, inner) = (pt(0.0, 3000.0), pt(500.0, 1000.0));
        assert_eq!(one(vec![outer]), Some(!inner_is_left), "reversed {reversed}");
        assert_eq!(one(vec![inner]), Some(inner_is_left), "reversed {reversed}");
        // The nearest point wins; the centre counts as inside.
        assert_eq!(one(vec![pt(0.0, 9000.0), inner]), Some(inner_is_left));
        assert_eq!(one(vec![pt(0.0, 0.0)]), Some(inner_is_left));
        // Past the arc's span, within its faces, or no point at all: undecided.
        assert_eq!(one(vec![pt(0.0, -3000.0)]), None);
        assert_eq!(one(vec![pt(0.0, 2125.0)]), None);
        assert_eq!(one(Vec::new()), None);
    }
}

#[test]
fn closed_room_sides_ignore_open_resolution() {
    let p = project(closed_room(1, 0.0));
    let plain = sides(&p, &SideParams::default());
    let with_point = sides(&p, &SideParams { outside_points: vec![pt(-1000.0, 2000.0)], ..SideParams::default() });
    assert_eq!(plain, with_point);
    for r in &plain.runs {
        assert_eq!(r.open_outside_left, None);
        assert_eq!(outside_on_left(r), Some(r.left == SideKind::Outside));
    }
}

#[test]
fn room_with_an_undrawn_gap_has_no_space() {
    let s = sides_of(room_with_door(1, 0.0));
    assert!(s.spaces.is_empty(), "{s:?}");
    assert_eq!(s.runs.len(), 5);
    for r in &s.runs {
        assert_eq!((r.left, r.right, r.exterior), (SideKind::Outside, SideKind::Outside, true));
    }
}

#[test]
fn interior_wall_on_an_exterior_layer_is_exterior_with_spaces_both_sides() {
    let p = project(two_rooms("layer-wall-int"));
    let shared = graph_runs(&p).iter().position(|c| matches!(c, RunCurve::Line { from, .. } if (from.x - 5000.0).abs() < 1e-6)).unwrap();
    let r = &sides(&p, &SideParams::default()).runs[shared];
    assert!(!r.exterior && is_space(r.left) && is_space(r.right), "{r:?}");
    let params = SideParams { exterior_layer_ids: vec!["layer-wall-int".into()], ..SideParams::default() };
    let s = sides(&p, &params);
    let r = &s.runs[shared];
    assert!(r.exterior && is_space(r.left) && is_space(r.right), "{r:?}");
    assert_eq!(s.spaces.len(), 2);
}

#[test]
fn room_with_a_semicircular_wall_is_one_space() {
    // A straight wall (faces y = 0, y = 200) and a semicircular wall about (2000, 200), centreline
    // radius 2000; the straight centreline y = 100 meets the arc's ends at (0, 100) and (4000, 100).
    let s = sides_of(vec![
        on_layer(1, "layer-wall-ext", &[(-100.0, 200.0), (-100.0, 0.0), (4100.0, 0.0), (4100.0, 200.0), (-100.0, 200.0)], &[0.0, 0.0, 0.0, 1.0]),
        on_layer(2, "layer-wall-ext", &[(100.0, 200.0), (3900.0, 200.0), (100.0, 200.0)], &[0.0, 1.0]),
    ]);
    assert_eq!(s.spaces.len(), 1, "{s:?}");
    assert_eq!(s.runs.len(), 2, "{s:?}");
    let want = 4000.0 * 100.0 + PI * 2000.0 * 2000.0 / 2.0;
    assert!((s.spaces[0].area_mm2 - want).abs() <= want * 0.005, "{} vs {want}", s.spaces[0].area_mm2);
    assert!(s.runs.iter().all(|r| r.exterior && is_space(r.left) != is_space(r.right)), "{s:?}");
}

#[test]
fn output_identical_with_entity_order_shuffled() {
    let check = |p: Project, params: SideParams| {
        let mut q = p.clone();
        if let AnyView::Plan(v) = &mut q.views[0] {
            v.entities.reverse();
            let n = v.entities.len();
            v.entities.swap(0, n / 2);
        }
        assert_eq!(sides(&p, &params), sides(&q, &params));
    };
    check(project(courtyard()), SideParams { outside_points: vec![pt(6000.0, 6000.0)], ..SideParams::default() });
    check(project(two_rooms("layer-wall-int")), SideParams { exterior_layer_ids: vec!["layer-wall-int".into()], ..SideParams::default() });
    let mut walls = room_with_door(1, 0.0);
    walls.extend(room_with_door(3, 8000.0));
    check(
        with_doors(walls, |d| {
            let mut e = door(20, d, 0.0);
            e.extend(door(22, d, 8000.0));
            e
        }),
        SideParams::default(),
    );
}

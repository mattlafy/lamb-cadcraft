//! Wall runs (`derivation::walls`, LAMB T334): paired cut face lines on wall layers become runs.

use std::f64::consts::PI;

use lamb_core::derivation::walls::{RunCurve, WallRun, WallRunParams, wall_runs};
use lamb_core::geometry::{Point2, PolySegment, Polyline2};
use lamb_core::model::{AnyView, DrawingEntity, EntityGeometry, Id, LineRole, PlanView, Project, empty_project};

const VIEW: Id = Id(100);
const EPS: f64 = 1e-6;

fn pt(x: f64, y: f64) -> Point2 {
    Point2 { x, y }
}

/// A polyline through `pts`; `bulges[i]` is segment i's bulge (missing = straight).
fn poly(pts: &[(f64, f64)], bulges: &[f64]) -> Polyline2 {
    Polyline2 {
        start: pt(pts[0].0, pts[0].1),
        segments: pts[1..]
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| PolySegment { to: pt(x, y), bulge: bulges.get(i).copied().unwrap_or(0.0), corner_at_to: None })
            .collect(),
        closed: false,
    }
}

fn ent(id: u64, layer: &str, role: LineRole, geometry: Polyline2) -> DrawingEntity {
    DrawingEntity {
        id: Id(id),
        view_id: VIEW,
        role,
        layer_id: layer.into(),
        geometry: EntityGeometry::Polyline { geometry },
        color_override: None,
        lineweight_override: None,
        ai_origin: None,
    }
}

fn wall(id: u64, pts: &[(f64, f64)]) -> DrawingEntity {
    ent(id, "layer-wall-ext", LineRole::Bylayer, poly(pts, &[]))
}

fn project(entities: Vec<DrawingEntity>) -> Project {
    let mut p = empty_project(None);
    p.views.push(AnyView::Plan(PlanView { id: VIEW, name: "plan".into(), entities, placement: None }));
    p
}

fn runs(entities: Vec<DrawingEntity>) -> Vec<WallRun> {
    let p = project(entities);
    wall_runs(&p, VIEW, &WallRunParams::defaults(&p.layers))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < EPS
}

/// Asserts width, both faces' `(entity, segment, from param, to param)` and a line centreline.
fn assert_line_run(r: &WallRun, width: f64, f0: (u64, usize, f64, f64), f1: (u64, usize, f64, f64), from: (f64, f64), to: (f64, f64)) {
    assert!(close(r.width, width), "width {} != {width}: {r:?}", r.width);
    for (face, (e, s, a, b)) in r.faces.iter().zip([f0, f1]) {
        assert_eq!((face.from.entity_id, face.from.segment_index), (Id(e), s), "{r:?}");
        assert_eq!((face.to.entity_id, face.to.segment_index), (Id(e), s), "{r:?}");
        assert!(close(face.from.segment_param, a) && close(face.to.segment_param, b), "params {face:?} != {a}..{b}");
    }
    let RunCurve::Line { from: cf, to: ct } = r.centreline else { panic!("not a line: {r:?}") };
    assert!(close(cf.x, from.0) && close(cf.y, from.1) && close(ct.x, to.0) && close(ct.y, to.1), "centreline {cf:?}->{ct:?}");
}

#[test]
fn straight_pair_gives_one_run_halfway() {
    let r = runs(vec![wall(1, &[(0.0, 0.0), (5000.0, 0.0)]), wall(2, &[(0.0, 200.0), (5000.0, 200.0)])]);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].view_id, VIEW);
    assert_line_run(&r[0], 200.0, (1, 0, 0.0, 1.0), (2, 0, 0.0, 1.0), (0.0, 100.0), (5000.0, 100.0));
}

#[test]
fn antiparallel_pair_gives_the_same_run_with_reversed_params() {
    let r = runs(vec![wall(1, &[(0.0, 0.0), (5000.0, 0.0)]), wall(2, &[(5000.0, 200.0), (0.0, 200.0)])]);
    assert_eq!(r.len(), 1);
    assert_line_run(&r[0], 200.0, (1, 0, 0.0, 1.0), (2, 0, 1.0, 0.0), (0.0, 100.0), (5000.0, 100.0));
}

#[test]
fn partial_overlap_spans_only_the_shared_length() {
    let r = runs(vec![wall(1, &[(0.0, 0.0), (5000.0, 0.0)]), wall(2, &[(1000.0, 200.0), (4000.0, 200.0)])]);
    assert_eq!(r.len(), 1);
    assert_line_run(&r[0], 200.0, (1, 0, 0.2, 0.8), (2, 0, 0.0, 1.0), (1000.0, 100.0), (4000.0, 100.0));
}

#[test]
fn l_wall_gives_one_run_per_leg_ending_at_the_inner_corner() {
    let r = runs(vec![wall(1, &[(0.0, 0.0), (5000.0, 0.0), (5000.0, 4000.0)]), wall(2, &[(0.0, 200.0), (4800.0, 200.0), (4800.0, 4000.0)])]);
    assert_eq!(r.len(), 2, "{r:?}");
    assert_line_run(&r[0], 200.0, (1, 0, 0.0, 0.96), (2, 0, 0.0, 1.0), (0.0, 100.0), (4800.0, 100.0));
    assert_line_run(&r[1], 200.0, (1, 1, 0.05, 1.0), (2, 1, 0.0, 1.0), (4900.0, 200.0), (4900.0, 4000.0));
}

fn t_wall() -> Vec<DrawingEntity> {
    vec![
        wall(1, &[(0.0, 0.0), (10000.0, 0.0)]),
        wall(2, &[(0.0, -200.0), (4900.0, -200.0), (4900.0, -5000.0)]),
        wall(3, &[(5100.0, -5000.0), (5100.0, -200.0), (10000.0, -200.0)]),
    ]
}

#[test]
fn t_wall_gives_two_through_runs_and_a_stem() {
    let r = runs(t_wall());
    assert_eq!(r.len(), 3, "{r:?}");
    assert_line_run(&r[0], 200.0, (1, 0, 0.0, 0.49), (2, 0, 0.0, 1.0), (0.0, -100.0), (4900.0, -100.0));
    assert_line_run(&r[1], 200.0, (1, 0, 0.51, 1.0), (3, 1, 0.0, 1.0), (5100.0, -100.0), (10000.0, -100.0));
    assert_line_run(&r[2], 200.0, (2, 1, 0.0, 1.0), (3, 0, 1.0, 0.0), (5000.0, -200.0), (5000.0, -5000.0));
}

#[test]
fn concentric_arcs_give_one_arc_run() {
    let b = (PI / 8.0).tan(); // 90 degrees
    let inner = ent(1, "layer-wall-int", LineRole::Bylayer, poly(&[(3000.0, 0.0), (0.0, 3000.0)], &[b]));
    // Outer drawn clockwise, so its params run 1 -> 0.
    let outer = ent(2, "layer-wall-int", LineRole::Cut, poly(&[(0.0, 3250.0), (3250.0, 0.0)], &[-b]));
    let r = runs(vec![inner, outer]);
    assert_eq!(r.len(), 1, "{r:?}");
    let run = &r[0];
    assert!(close(run.width, 250.0));
    let [f0, f1] = &run.faces;
    assert_eq!((f0.from.entity_id, f1.from.entity_id), (Id(1), Id(2)));
    assert!(close(f0.from.segment_param, 0.0) && close(f0.to.segment_param, 1.0), "{f0:?}");
    assert!(close(f1.from.segment_param, 1.0) && close(f1.to.segment_param, 0.0), "{f1:?}");
    let RunCurve::Arc { center, radius, start_angle, sweep } = run.centreline else { panic!("not an arc: {run:?}") };
    assert!(close(center.x, 0.0) && close(center.y, 0.0), "{center:?}");
    assert!(close(radius, 3125.0) && close(start_angle, 0.0) && close(sweep, PI / 2.0), "{run:?}");
}

#[test]
fn arc_never_pairs_with_a_line() {
    let b = (PI / 8.0).tan();
    let arc = ent(1, "layer-wall-ext", LineRole::Bylayer, poly(&[(3000.0, 0.0), (0.0, 3000.0)], &[b]));
    assert!(runs(vec![arc, wall(2, &[(3200.0, 0.0), (3200.0, 3000.0)])]).is_empty());
}

#[test]
fn room_faces_too_far_apart_give_none() {
    assert!(runs(vec![wall(1, &[(0.0, 0.0), (5000.0, 0.0)]), wall(2, &[(0.0, 2000.0), (5000.0, 2000.0)])]).is_empty());
}

#[test]
fn non_wall_layer_or_non_cut_role_gives_none() {
    let a = || poly(&[(0.0, 0.0), (5000.0, 0.0)], &[]);
    let b = || poly(&[(0.0, 200.0), (5000.0, 200.0)], &[]);
    assert!(runs(vec![ent(1, "layer-0", LineRole::Bylayer, a()), ent(2, "layer-0", LineRole::Bylayer, b())]).is_empty());
    assert!(runs(vec![ent(1, "layer-wall-ext", LineRole::Annotation, a()), ent(2, "layer-wall-ext", LineRole::Bylayer, b())]).is_empty());
}

#[test]
fn three_parallel_lines_pair_nearest_partners_only() {
    let r = runs(vec![wall(1, &[(0.0, 0.0), (5000.0, 0.0)]), wall(2, &[(0.0, 100.0), (5000.0, 100.0)]), wall(3, &[(0.0, 250.0), (5000.0, 250.0)])]);
    assert_eq!(r.len(), 2, "{r:?}");
    assert_line_run(&r[0], 100.0, (1, 0, 0.0, 1.0), (2, 0, 0.0, 1.0), (0.0, 50.0), (5000.0, 50.0));
    assert_line_run(&r[1], 150.0, (2, 0, 0.0, 1.0), (3, 0, 0.0, 1.0), (0.0, 175.0), (5000.0, 175.0));
}

#[test]
fn output_is_deterministic_across_calls_and_entity_order() {
    let p = project(t_wall());
    let params = WallRunParams::defaults(&p.layers);
    let first = wall_runs(&p, VIEW, &params);
    assert_eq!(first, wall_runs(&p, VIEW, &params));
    let mut shuffled = t_wall();
    shuffled.reverse();
    shuffled.swap(0, 1);
    assert_eq!(first, runs(shuffled));
}

#[test]
fn defaults_pick_wall_layers_case_insensitively() {
    let mut p = empty_project(None);
    p.layers[0].name = "wall-misc".into();
    let ids = WallRunParams::defaults(&p.layers).wall_layer_ids;
    assert_eq!(ids, vec!["layer-0", "layer-wall-ext", "layer-wall-int"]);
}

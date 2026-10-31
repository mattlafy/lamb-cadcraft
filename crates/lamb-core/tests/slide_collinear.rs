//! LAMB T573: a travelling segment whose straight neighbour is collinear with it (shares the
//! vertex and the heading) doubles the shared vertex — a connector perpendicular to the pair joins
//! the neighbour's unchanged line to the moved segment's new end (`slide_segment_by_delta`).

use lamb_core::geometry::Point2;
use lamb_core::geometry::polyline::{PolySegment, Polyline2, resolved_segments, slide_segment_by_delta};

fn p(x: f64, y: f64) -> Point2 {
    Point2 { x, y }
}

fn poly(pts: &[(f64, f64)], closed: bool) -> Polyline2 {
    let segments = pts[1..].iter().map(|&(x, y)| PolySegment { to: p(x, y), bulge: 0.0, corner_at_to: None }).collect();
    Polyline2 { start: p(pts[0].0, pts[0].1), segments, closed }
}

/// The polyline's vertices in order: start, then every segment's end.
fn vertices(poly: &Polyline2) -> Vec<(f64, f64)> {
    let segs = resolved_segments(poly);
    std::iter::once(poly.start).chain(segs.iter().map(|s| s.to)).map(|q| (q.x, q.y)).collect()
}

fn same(got: &[(f64, f64)], want: &[(f64, f64)]) {
    assert_eq!(got.len(), want.len(), "{got:?} != {want:?}");
    for (g, w) in got.iter().zip(want) {
        assert!((g.0 - w.0).abs() < 1e-6 && (g.1 - w.1).abs() < 1e-6, "{got:?} != {want:?}");
    }
}

#[test]
fn a_move_across_a_collinear_pair_inserts_a_connector() {
    let open = poly(&[(0.0, 0.0), (1000.0, 0.0), (2000.0, 0.0), (2000.0, 1000.0)], false);
    let out = slide_segment_by_delta(&open, 1, p(0.0, 100.0));
    same(&vertices(&out.poly), &[(0.0, 0.0), (1000.0, 0.0), (1000.0, 100.0), (2000.0, 100.0), (2000.0, 1000.0)]);
    assert_eq!(out.inserted, vec![1]);
    assert_eq!(out.unreachable, None);
    assert!(out.poly.segments.iter().all(|s| s.bulge == 0.0 && s.corner_at_to.is_none()));
}

#[test]
fn an_oblique_move_keeps_the_connector_perpendicular() {
    let open = poly(&[(0.0, 0.0), (1000.0, 0.0), (2000.0, 0.0), (2000.0, 1000.0)], false);
    let out = slide_segment_by_delta(&open, 1, p(50.0, 100.0));
    same(&vertices(&out.poly), &[(0.0, 0.0), (1050.0, 0.0), (1050.0, 100.0), (2000.0, 100.0), (2000.0, 1000.0)]);
    assert_eq!(out.inserted, vec![1]);
}

#[test]
fn a_move_along_the_pair_inserts_nothing() {
    let open = poly(&[(0.0, 0.0), (1000.0, 0.0), (2000.0, 0.0), (2000.0, 1000.0)], false);
    let out = slide_segment_by_delta(&open, 1, p(50.0, 0.0));
    same(&vertices(&out.poly), &[(0.0, 0.0), (1050.0, 0.0), (2000.0, 0.0), (2000.0, 1000.0)]);
    assert!(out.inserted.is_empty());
}

#[test]
fn a_collinear_next_neighbour_gets_its_connector_after_the_moved_segment() {
    let open = poly(&[(0.0, 1000.0), (0.0, 0.0), (1000.0, 0.0), (2000.0, 0.0)], false);
    let out = slide_segment_by_delta(&open, 1, p(0.0, 100.0));
    same(&vertices(&out.poly), &[(0.0, 1000.0), (0.0, 100.0), (1000.0, 100.0), (1000.0, 0.0), (2000.0, 0.0)]);
    assert_eq!(out.inserted, vec![2]);
}

#[test]
fn both_sides_collinear_insert_two_connectors() {
    let open = poly(&[(0.0, 0.0), (1000.0, 0.0), (2000.0, 0.0), (3000.0, 0.0)], false);
    let out = slide_segment_by_delta(&open, 1, p(0.0, 100.0));
    same(&vertices(&out.poly), &[(0.0, 0.0), (1000.0, 0.0), (1000.0, 100.0), (2000.0, 100.0), (2000.0, 0.0), (3000.0, 0.0)]);
    assert_eq!(out.inserted, vec![1, 3]);
}

#[test]
fn a_closed_pair_across_the_wrap_appends_the_connector() {
    // The last side (-1000,0)→(0,0) is collinear with segment 0 (0,0)→(1000,0) across the start.
    let closed = poly(&[(0.0, 0.0), (1000.0, 0.0), (1000.0, 1000.0), (-1000.0, 1000.0), (-1000.0, 0.0), (0.0, 0.0)], true);
    let n = closed.segments.len();
    let out = slide_segment_by_delta(&closed, 0, p(0.0, 100.0));
    assert_eq!(out.inserted, vec![n]);
    assert_eq!(out.poly.segments.len(), n + 1);
    same(&vertices(&out.poly), &[(0.0, 100.0), (1000.0, 100.0), (1000.0, 1000.0), (-1000.0, 1000.0), (-1000.0, 0.0), (0.0, 0.0), (0.0, 100.0)]);
}

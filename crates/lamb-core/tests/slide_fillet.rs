//! §4b: a tangent fillet arc beside a travelling segment keeps its radius and both tangencies
//! (`slide_segment_by_delta`, LAMB T565); a non-tangent arc neighbour keeps the chord rule.

use lamb_core::geometry::Point2;
use lamb_core::geometry::polyline::{
    PolySegment, Polyline2, ResolvedSegment, arc_from_bulge, resolved_segments, slide_segment_by_delta, tangent_at_param,
};

/// tan(π/8): the bulge of a quarter circle (counter-clockwise).
const QUARTER: f64 = 0.414_213_562_373_095_1;

fn p(x: f64, y: f64) -> Point2 {
    Point2 { x, y }
}

fn seg(x: f64, y: f64, bulge: f64) -> PolySegment {
    PolySegment { to: p(x, y), bulge, corner_at_to: None }
}

fn near(a: Point2, b: Point2) {
    assert!((a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6, "{a:?} != {b:?}");
}

fn radius(s: &ResolvedSegment) -> f64 {
    arc_from_bulge(s.from, s.to, s.bulge).expect("arc").radius
}

fn dir(s: &ResolvedSegment) -> Point2 {
    tangent_at_param(s, 0.0)
}

/// The arc `fillet` is tangent to `before` at its start and to `after` at its end.
fn assert_tangent(before: &ResolvedSegment, fillet: &ResolvedSegment, after: &ResolvedSegment) {
    near(before.to, fillet.from);
    near(fillet.to, after.from);
    near(tangent_at_param(fillet, 0.0), dir(before));
    near(tangent_at_param(fillet, 1.0), dir(after));
}

#[test]
fn open_l_fillet_translates_with_its_corner() {
    let poly =
        Polyline2 { start: p(0.0, 8000.0), segments: vec![seg(0.0, 500.0, 0.0), seg(500.0, 0.0, QUARTER), seg(10000.0, 0.0, 0.0)], closed: false };
    let before = resolved_segments(&poly);
    assert!((radius(&before[1]) - 500.0).abs() < 1e-6);

    let out = slide_segment_by_delta(&poly, 2, p(0.0, -1500.0));
    assert_eq!(out.unreachable, None);
    let r = resolved_segments(&out.poly);
    near(r[0].from, p(0.0, 8000.0));
    near(r[0].to, p(0.0, -1000.0));
    near(r[1].from, p(0.0, -1000.0));
    near(r[1].to, p(500.0, -1500.0));
    assert_eq!(r[1].bulge, QUARTER);
    assert!((radius(&r[1]) - 500.0).abs() < 1e-6);
    near(r[2].to, p(10000.0, -1500.0));
    assert_tangent(&r[0], &r[1], &r[2]);
}

/// A 4000 × 3000 rectangle, counter-clockwise from the bottom edge, every corner a 500 fillet.
fn filleted_rectangle() -> Polyline2 {
    Polyline2 {
        start: p(500.0, 0.0),
        segments: vec![
            seg(3500.0, 0.0, 0.0),
            seg(4000.0, 500.0, QUARTER),
            seg(4000.0, 2500.0, 0.0),
            seg(3500.0, 3000.0, QUARTER),
            seg(500.0, 3000.0, 0.0),
            seg(0.0, 2500.0, QUARTER),
            seg(0.0, 500.0, 0.0),
            seg(500.0, 0.0, QUARTER),
        ],
        closed: true,
    }
}

#[test]
fn closed_rectangle_side_moves_out_with_both_fillets() {
    let poly = filleted_rectangle();
    let before = resolved_segments(&poly);
    // Move the right side (segment 2) outward by 1000.
    let out = slide_segment_by_delta(&poly, 2, p(1000.0, 0.0));
    assert_eq!(out.unreachable, None);
    let r = resolved_segments(&out.poly);
    near(r[2].from, p(5000.0, 500.0));
    near(r[2].to, p(5000.0, 2500.0));
    for (b, f, a) in [(0, 1, 2), (2, 3, 4)] {
        assert_eq!(r[f].bulge, QUARTER);
        assert!((radius(&r[f]) - 500.0).abs() < 1e-6, "fillet {f}");
        assert_tangent(&r[b], &r[f], &r[a]);
    }
    near(r[0].to, p(4500.0, 0.0));
    near(r[4].from, p(4500.0, 3000.0));
    // The opposite side and its fillets are untouched.
    for i in [5, 6, 7] {
        assert_eq!(r[i], before[i]);
    }
    near(out.poly.start, p(500.0, 0.0));
    near(r[0].from, p(500.0, 0.0));
}

#[test]
fn non_tangent_arc_neighbour_keeps_the_chord_rule() {
    // The arc from (0,500) to (500,0) bulges the other way: not tangent to either line.
    let poly =
        Polyline2 { start: p(0.0, 8000.0), segments: vec![seg(0.0, 500.0, 0.0), seg(500.0, 0.0, -QUARTER), seg(10000.0, 0.0, 0.0)], closed: false };
    let out = slide_segment_by_delta(&poly, 2, p(0.0, -1500.0));
    let r = resolved_segments(&out.poly);
    // Chord rule: the shared end lands on the chord line x + y = 500, closest to (500, -1500).
    near(r[0].to, p(0.0, 500.0));
    near(r[1].from, p(0.0, 500.0));
    near(r[1].to, p(1250.0, -750.0));
    assert_eq!(r[1].bulge, -QUARTER);
    near(r[2].to, p(10000.0, -1500.0));
    assert_eq!(out.unreachable, None);
}

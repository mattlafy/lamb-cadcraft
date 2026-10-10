//! Circles as closed two-arc polylines, and the 2P / 3P circle fits. Port of
//! `geometry-primitives/src/circleFit.ts`.

use serde::{Deserialize, Serialize};

use super::point::{Point2, dist2};
use super::polyline::{PolySegment, Polyline2};
use super::tolerance::TOLERANCE;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircleFit {
    pub center: Point2,
    pub radius: f64,
}

/// T94: a circle IS a closed two-segment polyline, both bulges +1 — so every command that already
/// understands polylines understands a circle, and nothing has to carry a "this is a circle" flag.
/// This pair of functions is the only place that representation is known.
///
/// Two segments rather than one because a 360-degree bulge is infinite, and rather than four
/// because `resolved_segments` emits no implicit closing segment: the last `to` must land back on
/// `start`, which two semicircles over a shared diameter do.
pub fn circle_as_polyline(fit: &CircleFit) -> Polyline2 {
    let right = Point2 { x: fit.center.x + fit.radius, y: fit.center.y };
    let left = Point2 { x: fit.center.x - fit.radius, y: fit.center.y };
    Polyline2 {
        start: right,
        segments: vec![PolySegment { to: left, bulge: 1.0, corner_at_to: None }, PolySegment { to: right, bulge: 1.0, corner_at_to: None }],
        closed: true,
    }
}

/// Bulge magnitude may drift by rounding through a save/load; a semicircle's is exactly 1.
const UNIT_BULGE_TOLERANCE: f64 = 1e-6;

/// JavaScript's `Math.sign` (0 and NaN map to themselves, unlike `f64::signum`).
fn js_sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        x
    }
}

/// Circle-ness is DERIVED, never flagged: a closed polyline of exactly two same-signed unit-bulge
/// arcs over a shared diameter *is* a circle, whoever built it. Returns `None` for anything else.
pub fn polyline_as_circle(poly: &Polyline2) -> Option<CircleFit> {
    if !poly.closed {
        return None;
    }
    let [first, second] = poly.segments.as_slice() else { return None };
    if (first.bulge.abs() - 1.0).abs() > UNIT_BULGE_TOLERANCE {
        return None;
    }
    if (second.bulge.abs() - 1.0).abs() > UNIT_BULGE_TOLERANCE {
        return None;
    }
    if js_sign(first.bulge) != js_sign(second.bulge) {
        return None;
    }
    // The loop has to actually close, or the two arcs aren't over one diameter.
    if dist2(second.to, poly.start) > TOLERANCE.coincidence_mm {
        return None;
    }
    let diameter = dist2(poly.start, first.to);
    if diameter < TOLERANCE.coincidence_mm {
        return None;
    }
    Some(CircleFit { center: Point2 { x: (poly.start.x + first.to.x) / 2.0, y: (poly.start.y + first.to.y) / 2.0 }, radius: diameter / 2.0 })
}

/// CIRCLE's "2P" mode: the two clicked points are the ends of a diameter.
pub fn circle_from_diameter(a: Point2, b: Point2) -> CircleFit {
    CircleFit { center: Point2 { x: (a.x + b.x) / 2.0, y: (a.y + b.y) / 2.0 }, radius: dist2(a, b) / 2.0 }
}

/// CIRCLE's "3P" mode: the circumcircle through three points. `None` if they're (nearly) collinear.
pub fn circumcircle(a: Point2, b: Point2, c: Point2) -> Option<CircleFit> {
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    if d.abs() < 1e-9 {
        return None;
    }
    let a_sq = a.x * a.x + a.y * a.y;
    let b_sq = b.x * b.x + b.y * b.y;
    let c_sq = c.x * c.x + c.y * c.y;
    let ux = (a_sq * (b.y - c.y) + b_sq * (c.y - a.y) + c_sq * (a.y - b.y)) / d;
    let uy = (a_sq * (c.x - b.x) + b_sq * (a.x - c.x) + c_sq * (b.x - a.x)) / d;
    let center = Point2 { x: ux, y: uy };
    Some(CircleFit { center, radius: dist2(center, a) })
}

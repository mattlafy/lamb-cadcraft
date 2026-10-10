//! Tilt and the angle correction (TS `tilt.ts`): `tan(alpha) = sin(theta) / tan(phi)`, per LAMB's
//! `01-core-concept/authority-resolution-algorithm.md#tilt-and-the-angle-correction`.
//!
//! theta: plan crossing angle between the vertical view's cut line and the plan line (90 =
//! perpendicular). phi: the drawn line's elevation angle in the vertical view (90 = vertical).
//! alpha: the surface's true tilt away from plumb.

use crate::geometry::Point2;
use crate::geometry::intersect::crossing_angle_deg;
use crate::geometry::tolerance::{TOLERANCE, deg_to_rad, rad_to_deg};

/// Unsigned form, matching the literal formula and sanity-check table.
pub fn tilt_from_crossing(theta_deg: f64, phi_deg: f64) -> f64 {
    if theta_deg < TOLERANCE.marker_near_parallel_max_deg {
        return 0.0;
    }
    let tan_phi = deg_to_rad(phi_deg).tan();
    if tan_phi.abs() < 1e-9 {
        return 90.0;
    }
    let tan_alpha = deg_to_rad(theta_deg).sin() / tan_phi;
    rad_to_deg(tan_alpha.atan())
}

/// Signed tilt as synthesis uses it. `sin(theta)` is the signed cross of the plan tangent `p_hat`
/// (authored from->to) and the cut-line direction `c_hat`, both global XY; `tan(phi)` is the drawn
/// line's rise over run (`dy / dx`) in the vertical view's local coords. A positive alpha offsets
/// the surface towards `p_hat` rotated +90° (CCW) in plan.
pub fn signed_tilt_deg(p_hat: Point2, c_hat: Point2, dx: f64, dy: f64) -> f64 {
    let theta_unsigned = crossing_angle_deg(p_hat, c_hat);
    if theta_unsigned < TOLERANCE.marker_near_parallel_max_deg {
        return 0.0;
    }
    let sin_theta = p_hat.x * c_hat.y - p_hat.y * c_hat.x;
    if dx.abs() < 1e-9 {
        return 0.0; // vertical drawn line -> plumb
    }
    let tan_phi = dy / dx;
    if tan_phi.abs() < 1e-9 {
        // JS `Math.sign(sinTheta || 1)`: 0 (and NaN) count as 1.
        let s = if sin_theta == 0.0 || sin_theta.is_nan() { 1.0 } else { sin_theta.signum() };
        return 90.0 * s;
    }
    rad_to_deg((sin_theta / tan_phi).atan())
}

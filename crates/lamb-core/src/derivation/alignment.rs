//! Relates a vertical view's cut line to another view shown beside it (the reference pane): the
//! shared physical axis the two must be aligned against (TS `alignment.ts`).

use serde::{Deserialize, Serialize};

use super::projection::{plan_global_to_local_xy, plan_local_to_global_xy, vertical_basis};
use super::read::{ViewKind, ViewRef};
use crate::geometry::Point2;
use crate::geometry::point::{dot2, sub2};
use crate::geometry::transform2d::world_to_local_dir2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlignmentBasis {
    /// Global XY of the cut line's start (u = 0).
    #[serde(rename = "originXY")]
    pub origin_xy: Point2,
    /// Unit direction along the cut line, global XY (the u axis).
    pub c_hat: Point2,
    /// `c_hat` rotated +90°, global XY (the v axis).
    pub n_hat: Point2,
}

pub fn compute_alignment_basis(sec_view: &ViewRef<'_>, host_plan: &ViewRef<'_>) -> Result<AlignmentBasis, String> {
    let basis = vertical_basis(sec_view, host_plan)?;
    Ok(AlignmentBasis { origin_xy: basis.origin_xy, c_hat: basis.c_hat, n_hat: Point2 { x: -basis.c_hat.y, y: basis.c_hat.x } })
}

/// Never returns exactly 0, so a caller can invert by it.
const MIN_RATIO: f64 = 0.02;

/// How much of a unit step along `c_hat` shows as horizontal motion in `view`'s own local
/// rendering: always 1 for a non-plan or floating view (its local x is `u`), `cos θ` for a plan,
/// clamped away from zero.
pub fn local_direction_along_c_hat(view: &ViewRef<'_>, c_hat: Point2) -> f64 {
    let Some(t) = view.transform() else { return 1.0 };
    if view.kind != ViewKind::Plan {
        return 1.0;
    }
    let ratio = world_to_local_dir2(t, c_hat).x;
    if ratio.abs() < MIN_RATIO {
        return if ratio < 0.0 { -MIN_RATIO } else { MIN_RATIO };
    }
    ratio
}

/// A plan-local point in the cut-line-aligned frame: `u` along `c_hat`, `v` along `n_hat`.
pub fn plan_point_to_uv(plan_view: &ViewRef<'_>, basis: &AlignmentBasis, local: Point2) -> Result<Point2, String> {
    let g = plan_local_to_global_xy(plan_view, local)?;
    let d = sub2(g, basis.origin_xy);
    Ok(Point2 { x: dot2(d, basis.c_hat), y: dot2(d, basis.n_hat) })
}

/// Inverse of [`plan_point_to_uv`].
pub fn uv_to_plan_point(plan_view: &ViewRef<'_>, basis: &AlignmentBasis, uv: Point2) -> Result<Point2, String> {
    let g = Point2 {
        x: basis.origin_xy.x + uv.x * basis.c_hat.x + uv.y * basis.n_hat.x,
        y: basis.origin_xy.y + uv.x * basis.c_hat.y + uv.y * basis.n_hat.y,
    };
    plan_global_to_local_xy(plan_view, g)
}

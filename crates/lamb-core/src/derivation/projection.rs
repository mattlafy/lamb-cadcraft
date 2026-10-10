//! Placement math: a view's local 2D drafting space to and from project-global 3D (TS
//! `projection.ts`). A vertical view's cut line lives in its host view's space; only a PLAN host
//! gets real placement math, a vertical host falls back to taking the cut line as-is.

use serde::{Deserialize, Serialize};

use super::read::{ViewKind, ViewRef};
use crate::geometry::point::{dot2, normalize2, sub2};
use crate::geometry::transform2d::{local_to_world2, world_to_local2};
use crate::geometry::{IDENTITY_TRANSFORM, Point2, Point3, Transform2D};
use crate::model::Id;

fn floating(what: &str, view: &ViewRef<'_>) -> String {
    format!("{what} view {} is floating; has no global position", view.id.0)
}

pub fn plan_local_to_global_xy(view: &ViewRef<'_>, local: Point2) -> Result<Point2, String> {
    let t = view.transform().ok_or_else(|| floating("Plan", view))?;
    Ok(local_to_world2(t, local))
}

pub fn plan_global_to_local_xy(view: &ViewRef<'_>, global: Point2) -> Result<Point2, String> {
    let t = view.transform().ok_or_else(|| floating("Plan", view))?;
    Ok(world_to_local2(t, global))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerticalBasis {
    /// Global XY of the cut line's start point (u = 0).
    #[serde(rename = "originXY")]
    pub origin_xy: Point2,
    /// Unit direction of the cut line in global XY (u axis).
    pub c_hat: Point2,
    /// Total plan-length of the cut line, mm.
    pub length_mm: f64,
    /// The view's stored flip; read only by [`vertical_gaze_dir`].
    pub flipped: bool,
}

/// A vertical view's cut-plane basis in project-global XY, given its host view. A host that is not
/// a plan takes the cut line as-is.
pub fn vertical_basis(view: &ViewRef<'_>, host_view: &ViewRef<'_>) -> Result<VerticalBasis, String> {
    if view.placement.is_none() {
        return Err(floating("Vertical", view));
    }
    let cut_line = view.cut_line().ok_or_else(|| format!("Vertical view {} has no vertical placement", view.id.0))?;
    let seg = cut_line.segments.first().ok_or_else(|| format!("Vertical view {} has an empty cut line", view.id.0))?;
    let (start, end) = if host_view.kind == ViewKind::Plan {
        (plan_local_to_global_xy(host_view, cut_line.start)?, plan_local_to_global_xy(host_view, seg.to)?)
    } else {
        (cut_line.start, seg.to)
    };
    let c_hat = normalize2(sub2(end, start));
    let length_mm = (end.x - start.x).hypot(end.y - start.y);
    Ok(VerticalBasis { origin_xy: start, c_hat, length_mm, flipped: view.flipped() })
}

/// Position along a vertical view's cut line (`u`) and absolute height (`z`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Uz {
    pub u: f64,
    pub z: f64,
}

/// A vertical view's local 2D coords to (u, z). A floating view uses the identity transform.
pub fn vertical_local_to_uz(view: &ViewRef<'_>, local: Point2) -> Uz {
    let p = local_to_world2(view.transform().unwrap_or(&IDENTITY_TRANSFORM), local);
    Uz { u: p.x, z: p.y }
}

pub fn vertical_uz_to_local(view: &ViewRef<'_>, uz: Uz) -> Point2 {
    world_to_local2(view.transform().unwrap_or(&IDENTITY_TRANSFORM), Point2 { x: uz.u, y: uz.z })
}

/// Project-global 3D position of a point authored in a vertical view's local space.
pub fn vertical_local_to_global3(view: &ViewRef<'_>, host_view: &ViewRef<'_>, local: Point2) -> Result<Point3, String> {
    let basis = vertical_basis(view, host_view)?;
    let Uz { u, z } = vertical_local_to_uz(view, local);
    Ok(Point3 { x: basis.origin_xy.x + u * basis.c_hat.x, y: basis.origin_xy.y + u * basis.c_hat.y, z })
}

/// Signed distance along the cut line from its origin to a global XY point's projection.
pub fn project_to_u(basis: &VerticalBasis, global_xy: Point2) -> f64 {
    dot2(sub2(global_xy, basis.origin_xy), basis.c_hat)
}

/// Which way a section/elevation looks: `c_hat` rotated +90° (CCW), reversed when `flipped`.
pub fn vertical_gaze_dir(c_hat: Point2, flipped: bool) -> Point2 {
    let sign = if flipped { -1.0 } else { 1.0 };
    Point2 { x: sign * -c_hat.y, y: sign * c_hat.x }
}

/// The view's host as a placed plan; both click-to-view mappings require one.
fn placed_plan_host<'a>(view: &ViewRef<'_>, host_view: &ViewRef<'a>) -> Result<(Id, &'a Transform2D), String> {
    if view.placement.is_none() {
        return Err(floating("Vertical", view));
    }
    match (host_view.kind, host_view.transform()) {
        (ViewKind::Plan, Some(t)) => Ok((host_view.id, t)),
        _ => Err(format!("Vertical view {} must be hosted by a placed plan (host {} is not)", view.id.0, host_view.id.0)),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanPlacementThrough {
    /// Absolute Z of the clicked point: the new plan's FLOOR elevation, not its cut plane.
    pub floor_elevation_mm: f64,
    /// Taken from the vertical view's host plan.
    pub transform: Transform2D,
}

/// Where a new horizontal plan sits, given one point clicked in a vertical view.
pub fn plan_placement_through(view: &ViewRef<'_>, host_view: &ViewRef<'_>, local: Point2) -> Result<PlanPlacementThrough, String> {
    let (_, transform) = placed_plan_host(view, host_view)?;
    let Uz { z, .. } = vertical_local_to_uz(view, local);
    Ok(PlanPlacementThrough { floor_elevation_mm: z, transform: *transform })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerpendicularCutLine {
    /// The host plan the cut line is expressed in.
    pub host_view_id: Id,
    pub cut_from: Point2,
    pub cut_to: Point2,
}

/// A cut line perpendicular to `view` through the clicked point, `length_mm` long, in the host
/// plan's local coordinates, running along the basis's [`vertical_gaze_dir`] (so the view's flip
/// applies, as in TS).
pub fn perpendicular_cut_line_through(
    view: &ViewRef<'_>,
    host_view: &ViewRef<'_>,
    local: Point2,
    length_mm: f64,
) -> Result<PerpendicularCutLine, String> {
    let (host_view_id, _) = placed_plan_host(view, host_view)?;
    let basis = vertical_basis(view, host_view)?;
    let Uz { u, .. } = vertical_local_to_uz(view, local);
    let through = Point2 { x: basis.origin_xy.x + u * basis.c_hat.x, y: basis.origin_xy.y + u * basis.c_hat.y };
    let p_hat = vertical_gaze_dir(basis.c_hat, basis.flipped);
    let half = length_mm / 2.0;
    Ok(PerpendicularCutLine {
        host_view_id,
        cut_from: plan_global_to_local_xy(host_view, Point2 { x: through.x - half * p_hat.x, y: through.y - half * p_hat.y })?,
        cut_to: plan_global_to_local_xy(host_view, Point2 { x: through.x + half * p_hat.x, y: through.y + half * p_hat.y })?,
    })
}

//! Mapping a point between two views' own local 2D spaces (TS `underlay.ts`): the reference
//! underlay (T6, same-kind geometry drawn into the active canvas) and the linked pan/zoom lock
//! (T7). The one place that handles plan<->plan, vertical<->vertical and plan<->vertical pairings.

use serde::{Deserialize, Serialize};

use super::projection::{
    Uz, plan_global_to_local_xy, plan_local_to_global_xy, project_to_u, vertical_basis, vertical_local_to_global3, vertical_local_to_uz,
    vertical_uz_to_local,
};
use super::read::{ProjectRead, ViewKind, ViewRef};
use crate::geometry::Point2;
use crate::geometry::point::{cross2, dot2};
use crate::geometry::tolerance::{TOLERANCE, deg_to_rad};
use crate::model::Id;

/// `|cross2|` of two unit cut-line directions above this means "not parallel".
fn parallel_sin() -> f64 {
    deg_to_rad(TOLERANCE.angle_deg).sin()
}

fn placed(v: &ViewRef<'_>) -> bool {
    v.placement.is_some()
}

/// A placed vertical view's host, when that host is itself placed.
fn placed_host<'p, P: ProjectRead>(project: &'p P, view: &ViewRef<'_>) -> Option<ViewRef<'p>> {
    if view.kind != ViewKind::Vertical {
        return None;
    }
    let host = project.view(view.host_view_id()?)?;
    placed(&host).then_some(host)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnderlayCandidate {
    /// TS returns the whole view; look it up by id.
    pub view_id: Id,
    /// The candidate's cut line points opposite the active view's: it renders mirrored. Vertical
    /// views only.
    pub mirrored: bool,
}

/// Every OTHER placed view eligible as a reference underlay for `active_view_id`: same kind only,
/// and two vertical views only with parallel cut lines.
pub fn find_underlay_candidates(project: &impl ProjectRead, active_view_id: Id) -> Result<Vec<UnderlayCandidate>, String> {
    let Some(active) = project.view(active_view_id).filter(placed) else { return Ok(vec![]) };

    if active.kind == ViewKind::Plan {
        return Ok(project
            .views()
            .filter(|v| v.kind == ViewKind::Plan && v.id != active.id && placed(v))
            .map(|v| UnderlayCandidate { view_id: v.id, mirrored: false })
            .collect());
    }

    if active.kind != ViewKind::Vertical {
        return Ok(vec![]);
    }
    let Some(active_host) = placed_host(project, &active) else { return Ok(vec![]) };
    let basis_a = vertical_basis(&active, &active_host)?;

    let mut out = Vec::new();
    for v in project.views() {
        if v.kind != ViewKind::Vertical || v.id == active.id || !placed(&v) {
            continue;
        }
        let Some(host) = placed_host(project, &v) else { continue };
        let basis_b = vertical_basis(&v, &host)?;
        if cross2(basis_a.c_hat, basis_b.c_hat).abs() > parallel_sin() {
            continue; // not parallel: out of scope for now
        }
        out.push(UnderlayCandidate { view_id: v.id, mirrored: dot2(basis_a.c_hat, basis_b.c_hat) < 0.0 });
    }
    Ok(out)
}

/// Maps a point from `from_view`'s local space into `to_view`'s, or `None` if the pairing isn't
/// supported (unplaced, mixed kinds, or two vertical views with non-parallel cut lines).
pub fn map_view_local_point(
    project: &impl ProjectRead,
    from_view: &ViewRef<'_>,
    to_view: &ViewRef<'_>,
    from_local: Point2,
) -> Result<Option<Point2>, String> {
    if !placed(from_view) || !placed(to_view) {
        return Ok(None);
    }
    match (from_view.kind, to_view.kind) {
        (ViewKind::Plan, ViewKind::Plan) => Ok(Some(plan_global_to_local_xy(to_view, plan_local_to_global_xy(from_view, from_local)?)?)),
        (ViewKind::Vertical, ViewKind::Vertical) => {
            let (Some(from_host), Some(to_host)) = (placed_host(project, from_view), placed_host(project, to_view)) else { return Ok(None) };
            let basis_to = vertical_basis(to_view, &to_host)?;
            let basis_from = vertical_basis(from_view, &from_host)?;
            if cross2(basis_from.c_hat, basis_to.c_hat).abs() > parallel_sin() {
                return Ok(None); // non-parallel: not supported yet
            }
            let g3 = vertical_local_to_global3(from_view, &from_host, from_local)?;
            let u = project_to_u(&basis_to, Point2 { x: g3.x, y: g3.y });
            Ok(Some(vertical_uz_to_local(to_view, Uz { u, z: g3.z })))
        }
        _ => Ok(None),
    }
}

/// Vertical->plan is fully determined; plan->vertical also needs the vertical view's own previous
/// local point, for its z. Anything else falls through to [`map_view_local_point`].
pub fn map_plan_vertical_center(
    project: &impl ProjectRead,
    from_view: &ViewRef<'_>,
    from_local: Point2,
    to_view: &ViewRef<'_>,
    to_previous_local: Point2,
) -> Result<Option<Point2>, String> {
    if !placed(from_view) || !placed(to_view) {
        return Ok(None);
    }
    match (from_view.kind, to_view.kind) {
        (ViewKind::Vertical, ViewKind::Plan) => {
            let Some(host) = placed_host(project, from_view) else { return Ok(None) };
            let g3 = vertical_local_to_global3(from_view, &host, from_local)?;
            Ok(Some(plan_global_to_local_xy(to_view, Point2 { x: g3.x, y: g3.y })?))
        }
        (ViewKind::Plan, ViewKind::Vertical) => {
            let Some(host) = placed_host(project, to_view) else { return Ok(None) };
            let basis = vertical_basis(to_view, &host)?;
            let u = project_to_u(&basis, plan_local_to_global_xy(from_view, from_local)?);
            let z = vertical_local_to_uz(to_view, to_previous_local).z;
            Ok(Some(vertical_uz_to_local(to_view, Uz { u, z })))
        }
        _ => map_view_local_point(project, from_view, to_view, from_local),
    }
}

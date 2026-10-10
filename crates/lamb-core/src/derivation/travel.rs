//! T108 cross-view link travel (TS `travel.ts`): given a link and a delta applied to one anchor's
//! segment (in that anchor's view's local space), the PARTNER entity's new polyline: its crossing
//! segment translated by the correctly projected delta, its neighbours stretched to reconnect.
//! Pure. Source: LAMB's `docs/06-ui-ux/selection-and-link-editing.md` §4b.

use serde::{Deserialize, Serialize};

use super::projection::vertical_basis;
use super::read::{EntityRef, ProjectRead, ViewKind, ViewRef};
use crate::geometry::point::dot2;
use crate::geometry::polyline::slide_segment_by_delta;
use crate::geometry::transform2d::{local_to_world_dir2, world_to_local_dir2};
use crate::geometry::{Point2, Point3, Polyline2, Transform2D};
use crate::model::{Id, LinkAnchor, LinkRecord};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TravelResult {
    pub entity_id: Id,
    /// The partner's new polyline.
    pub geometry: Polyline2,
    /// The part of the delta ray-sliding could not absorb, in the PARTNER's local space. `None`
    /// when the full delta was achieved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreachable: Option<Point2>,
}

fn anchors_match(a: &LinkAnchor, b: &LinkAnchor) -> bool {
    a.entity_id == b.entity_id && a.segment_index == b.segment_index
}

/// `findEntityAndView`: the entity and the view whose entity list holds it.
fn entity_and_view<P: ProjectRead>(project: &P, id: Id) -> Option<(EntityRef<'_>, ViewRef<'_>)> {
    project.views().find_map(|v| project.entities(v.id).find(|e| e.id == id).map(|e| (e, v)))
}

fn plan_transform<'a>(view: &ViewRef<'a>) -> Result<&'a Transform2D, String> {
    view.transform().ok_or_else(|| format!("Plan view {} is floating; has no global position", view.id.0))
}

/// A vertical view's transform and its cut line's global direction.
fn vertical_frame<'a, P: ProjectRead>(project: &P, view: &ViewRef<'a>) -> Result<(&'a Transform2D, Point2), String> {
    if view.kind != ViewKind::Vertical {
        return Err(format!("View {} is neither plan nor vertical", view.id.0));
    }
    let t = view.transform().ok_or_else(|| format!("Vertical view {} is floating; has no global position", view.id.0))?;
    let host_id = view.host_view_id().ok_or_else(|| format!("Vertical view {} has no vertical placement", view.id.0))?;
    let host = project.view(host_id).ok_or_else(|| format!("Vertical view {}'s host {} not found", view.id.0, host_id.0))?;
    Ok((t, vertical_basis(view, &host)?.c_hat))
}

/// A view-local 2D delta as a project-global 3D one. A plan delta has dz = 0; a vertical's local
/// delta maps to (du, dz), and du carries into XY along the cut line.
fn moved_delta_to_global3<P: ProjectRead>(project: &P, view: &ViewRef<'_>, delta: Point2) -> Result<Point3, String> {
    if view.kind == ViewKind::Plan {
        let w = local_to_world_dir2(plan_transform(view)?, delta);
        return Ok(Point3 { x: w.x, y: w.y, z: 0.0 });
    }
    let (t, c_hat) = vertical_frame(project, view)?;
    let uz = local_to_world_dir2(t, delta);
    Ok(Point3 { x: c_hat.x * uz.x, y: c_hat.y * uz.x, z: uz.y })
}

/// The inverse of [`moved_delta_to_global3`]. A plan has no Z axis, so dz does not travel into it.
fn global3_to_local_delta<P: ProjectRead>(project: &P, view: &ViewRef<'_>, delta3: Point3) -> Result<Point2, String> {
    let world_xy = Point2 { x: delta3.x, y: delta3.y };
    if view.kind == ViewKind::Plan {
        return Ok(world_to_local_dir2(plan_transform(view)?, world_xy));
    }
    let (t, c_hat) = vertical_frame(project, view)?;
    Ok(world_to_local_dir2(t, Point2 { x: dot2(world_xy, c_hat), y: delta3.z }))
}

pub fn travel_segment_move(
    project: &impl ProjectRead,
    link: &LinkRecord,
    moved_anchor: &LinkAnchor,
    delta: Point2,
) -> Result<Option<TravelResult>, String> {
    let (mine, other) = if anchors_match(&link.a, moved_anchor) {
        (&link.a, &link.b)
    } else if anchors_match(&link.b, moved_anchor) {
        (&link.b, &link.a)
    } else {
        return Ok(None); // moved_anchor isn't either side of this link
    };

    let (Some((_, moved_view)), Some((partner, partner_view))) =
        (entity_and_view(project, mine.entity_id), entity_and_view(project, other.entity_id))
    else {
        return Ok(None);
    };
    let Some(partner_poly) = partner.polyline.as_deref() else { return Ok(None) };

    let global_delta = moved_delta_to_global3(project, &moved_view, delta)?;
    let partner_delta = global3_to_local_delta(project, &partner_view, global_delta)?;

    let slid = slide_segment_by_delta(partner_poly, other.segment_index, partner_delta);
    Ok(Some(TravelResult { entity_id: other.entity_id, geometry: slid.poly, unreachable: slid.unreachable }))
}

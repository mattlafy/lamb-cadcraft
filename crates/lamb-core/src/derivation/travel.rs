//! T108 cross-view link travel (TS `travel.ts`): given a link and a delta applied to one anchor's
//! segment (in that anchor's view's local space), the PARTNER entity's new polyline: its crossing
//! segment translated by the correctly projected delta, its neighbours stretched to reconnect.
//! Plan → vertical shifts the partner by the exact crossing distance along the cut, (d·n)/(c·n)
//! (LAMB T541); vertical → plan translates the plan partner by du along the cut, which moves its
//! crossing by exactly du whatever its angle.
//! Pure. Source: LAMB's `docs/06-ui-ux/selection-and-link-editing.md` §4b.

use serde::{Deserialize, Serialize};

use super::projection::vertical_basis;
use super::read::{EntityRef, ProjectRead, ViewKind, ViewRef};
use crate::geometry::point::dot2;
use crate::geometry::polyline::{resolved_segments, slide_segment_by_delta, tangent_at_param};
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

    let (Some((moved, moved_view)), Some((partner, partner_view))) =
        (entity_and_view(project, mine.entity_id), entity_and_view(project, other.entity_id))
    else {
        return Ok(None);
    };
    let Some(partner_poly) = partner.polyline.as_deref() else { return Ok(None) };

    let partner_delta = if moved_view.kind == ViewKind::Plan && partner_view.kind == ViewKind::Vertical {
        let Some(moved_poly) = moved.polyline.as_deref() else { return Ok(None) };
        let Some(seg) = resolved_segments(moved_poly).into_iter().nth(mine.segment_index) else { return Ok(None) };
        let t = plan_transform(&moved_view)?;
        let (vt, c_hat) = vertical_frame(project, &partner_view)?;
        // The crossing of a segment translated by `d` with the fixed cut line moves along the cut by
        // s = (d·n)/(c·n), n the segment's normal (for an arc, at the link's crossing). A rigid
        // translation keeps the direction, so reading it from the current drawing is exact.
        let dir = local_to_world_dir2(t, tangent_at_param(&seg, mine.segment_param));
        let n = Point2 { x: -dir.y, y: dir.x };
        let cn = dot2(c_hat, n);
        if cn.abs() < 1e-9 {
            return Ok(None); // parallel to the cut: they cannot cross
        }
        let d = local_to_world_dir2(t, delta);
        world_to_local_dir2(vt, Point2 { x: dot2(d, n) / cn, y: 0.0 })
    } else {
        let global_delta = moved_delta_to_global3(project, &moved_view, delta)?;
        global3_to_local_delta(project, &partner_view, global_delta)?
    };

    let slid = slide_segment_by_delta(partner_poly, other.segment_index, partner_delta);
    Ok(Some(TravelResult { entity_id: other.entity_id, geometry: slid.poly, unreachable: slid.unreachable }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::model::Project;
    use serde_json::json;

    /// A plan wall `wall` (one segment) linked to segment 1 of a section face crossing at u = 5000;
    /// the section's cut line runs global +X along y = 0, its transform identity (local x = u).
    fn project(wall: [[f64; 2]; 2]) -> Project {
        let poly = |start: [f64; 2], to: &[[f64; 2]]| {
            json!({"kind": "polyline", "geometry": {"closed": false, "start": {"x": start[0], "y": start[1]},
                "segments": to.iter().map(|p| json!({"bulge": 0, "to": {"x": p[0], "y": p[1]}})).collect::<Vec<_>>()}})
        };
        let ident = json!({"originX": 0, "originY": 0, "rotation": 0});
        serde_json::from_value(json!({
            "commandLog": [], "levels": [], "name": "T541", "patterns": {}, "schemaVersion": 3,
            "defaults": {"defaultCutHeightMm": 1000, "defaultFloorToFloorMm": 3000, "defaultSurfaceDepthMm": 2700,
                "defaultSurfaceHeightMm": 2700, "dimTextHeightPaperMm": 3, "plotScale": 50, "units": "mm"},
            "layers": [{"color": "#d0d0d0", "defaultRole": "cut", "id": "layer-0", "lineweight": 0.25, "locked": false, "name": "0", "visible": true}],
            "links": [{"a": {"entityId": 11, "segmentIndex": 0, "segmentParam": 0.5}, "b": {"entityId": 21, "segmentIndex": 1, "segmentParam": 0.5},
                "createdAt": "2026-01-01T00:00:00.000Z", "createdBy": {"kind": "user", "userId": "t"}, "id": 31, "origin": "automatic"}],
            "views": [
                {"id": 1, "kind": "plan", "name": "plan", "entities": [{"id": 11, "layerId": "layer-0", "role": "cut", "viewId": 1,
                    "geometry": poly(wall[0], &[wall[1]])}],
                 "placement": {"kind": "plan", "cutElevation": 1200, "levelId": 41, "transform": ident}},
                {"id": 2, "kind": "vertical", "intent": "section", "name": "sec", "entities": [{"id": 21, "layerId": "layer-0", "role": "cut", "viewId": 2,
                    "geometry": poly([4000.0, 0.0], &[[5000.0, 0.0], [5000.0, 3000.0], [6000.0, 3000.0]])}],
                 "placement": {"kind": "vertical", "hostViewId": 1, "transform": ident,
                    "cutLine": {"closed": false, "start": {"x": 0, "y": 0}, "segments": [{"bulge": 0, "to": {"x": 10000, "y": 0}}]}}}
            ]
        }))
        .unwrap()
    }

    /// The x of the partner's crossing segment (segment 1) after moving the wall by `delta`.
    fn moved_x(p: &Project, delta: Point2) -> Option<f64> {
        let link = &p.links[0];
        let r = travel_segment_move(p, link, &link.a, delta).unwrap()?;
        assert_eq!(r.entity_id, Id(21));
        assert!(r.unreachable.is_none(), "{r:?}");
        let seg = &resolved_segments(&r.geometry)[1];
        assert!((seg.from.x - seg.to.x).abs() < 1e-9);
        Some(seg.from.x)
    }

    #[test]
    fn an_oblique_wall_shifts_its_partner_by_the_true_crossing_distance() {
        // 45° wall through (5000, 0).
        let p = project([[0.0, -5000.0], [10000.0, 5000.0]]);
        let k = 100.0 / 2f64.sqrt();
        // 100 mm along its normal (-1, 1)/√2: the crossing moves −100·√2 along u.
        let x = moved_x(&p, Point2 { x: -k, y: k }).unwrap();
        assert!((x - (5000.0 - 141.421)).abs() < 0.1, "{x}");
        // 100 mm along its own direction: the crossing does not move.
        if let Some(x) = moved_x(&p, Point2 { x: k, y: k }) {
            assert!((x - 5000.0).abs() < 1e-6, "{x}");
        }
    }

    #[test]
    fn a_square_wall_shifts_its_partner_by_the_move() {
        let p = project([[5000.0, -5000.0], [5000.0, 5000.0]]);
        let x = moved_x(&p, Point2 { x: 100.0, y: 0.0 }).unwrap();
        assert!((x - 5100.0).abs() < 1e-6, "{x}");
        // Along itself: nothing moves.
        assert!((moved_x(&p, Point2 { x: 0.0, y: 100.0 }).unwrap() - 5000.0).abs() < 1e-6);
    }

    #[test]
    fn a_wall_parallel_to_the_cut_does_not_travel() {
        let p = project([[0.0, 0.0], [10000.0, 0.0]]);
        assert_eq!(moved_x(&p, Point2 { x: 0.0, y: 100.0 }), None);
    }
}

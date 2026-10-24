//! Intersection markers, cut-line overlays and a view's cut plane through the solids (TS
//! `markers.ts`). All derived per view, never written into a view's entities.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::js_to_fixed;
use super::projection::{
    Uz, VerticalBasis, plan_global_to_local_xy, plan_local_to_global_xy, project_to_u, vertical_basis, vertical_gaze_dir, vertical_local_to_uz,
    vertical_uz_to_local,
};
use super::read::{EntityRef, ProjectRead, ViewKind, ViewRef};
use super::resolve::{ProvenanceKind, resolve_seen_z};
use super::solids::SynthesizedSolid;
use super::synthesis::synthesize;
use crate::geometry::halfspace::{HalfSpace, section_convex_polyhedron};
use crate::geometry::intersect::{crossing_angle_deg, intersect_segment_with_infinite_line, intersect_segments, tangent_at_hit};
use crate::geometry::point::{normalize2, sub2};
use crate::geometry::polyline::{point_at_param, resolved_segments};
use crate::geometry::tolerance::TOLERANCE;
use crate::geometry::transform2d::{world_to_local_dir2, world_to_local2};
use crate::geometry::triangulate::polygon_area_signed;
use crate::geometry::{Point2, Point3, ResolvedSegment};
use crate::model::{DrawingLayer, Id, LineRole, Placement, resolve_entity_role};

/// TS `intersectSegmentWithInfiniteLine` / `intersectSegments` default.
const INTERSECT_EPS: f64 = 1e-6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkerShape {
    Circle,
    Ellipse2,
    Ellipse5,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Marker {
    /// `{view_id}:{entity_id}:{seg_index}:{t_a.toFixed(6)}`, ids printed with `Id`'s `Display`.
    pub id: String,
    pub view_id: Id,
    pub source_entity_id: Id,
    pub source_view_id: Id,
    pub source_segment_index: usize,
    pub source_segment_param: f64,
    /// Position in `view_id`'s local 2D space.
    pub position: Point2,
    pub theta_deg: f64,
    pub shape: MarkerShape,
    pub filled: bool,
    pub assumed_z: bool,
}

fn shape_for_theta(theta_deg: f64) -> MarkerShape {
    if theta_deg >= TOLERANCE.marker_circle_min_deg {
        MarkerShape::Circle
    } else if theta_deg < TOLERANCE.marker_near_parallel_max_deg {
        MarkerShape::Ellipse5
    } else {
        MarkerShape::Ellipse2
    }
}

fn marker_id(view_id: Id, entity_id: Id, seg_index: usize, t_a: f64) -> String {
    format!("{}:{}:{}:{}", view_id, entity_id, seg_index, js_to_fixed(t_a, 6))
}

fn plan_cut_elevation(view: &ViewRef<'_>) -> Option<f64> {
    match view.placement? {
        Placement::Plan { cut_elevation, .. } => Some(*cut_elevation),
        Placement::Vertical { .. } => None,
    }
}

/// `cut` or `seen` polylines: the lines that cross another view's plane.
fn crossing_role(doc: &impl ProjectRead, e: &EntityRef<'_>) -> Option<LineRole> {
    e.polyline.as_ref()?;
    match doc.role_of(e) {
        r @ (LineRole::Cut | LineRole::Seen) => Some(r),
        _ => None,
    }
}

/// A marker is filled when some link joins `entity` to an entity in `view` with its anchor on
/// `entity` within coincidence of the marker's source point `at` (in `entity`'s own view space),
/// whatever segment that anchor names — so a link on one segment of a closed loop's corner, or of
/// a line ending on a cut plane, fills the marker its neighbouring segment makes there too, in both
/// views (LAMB T514).
fn marker_filled(doc: &impl ProjectRead, entity: Id, segments: &[ResolvedSegment], view: Id, at: Point2) -> bool {
    doc.links().any(|l| {
        [(&l.a, &l.b), (&l.b, &l.a)].into_iter().any(|(mine, other)| {
            mine.entity_id == entity
                && doc.entity_view_id(other.entity_id) == Some(view)
                && segments.get(mine.segment_index).is_some_and(|s| {
                    let p = point_at_param(s, mine.segment_param);
                    (p.x - at.x).hypot(p.y - at.y) <= TOLERANCE.coincidence_mm
                })
        })
    })
}

/// Markers in a placed vertical view where every placed plan view's `cut`/`seen` lines cross its
/// cut line (taken as an infinite line in plan).
pub fn compute_markers_for_vertical_view(doc: &impl ProjectRead, view: &ViewRef<'_>) -> Result<Vec<Marker>, String> {
    let Some(transform) = view.transform() else { return Ok(Vec::new()) };
    let Some(host_view) = view.host_view_id().and_then(|h| doc.view(h)) else { return Ok(Vec::new()) };
    let basis = vertical_basis(view, &host_view)?;
    let cut_line_from = basis.origin_xy;
    let cut_line_to = Point2 { x: basis.origin_xy.x + basis.c_hat.x, y: basis.origin_xy.y + basis.c_hat.y };

    let mut markers = Vec::new();
    for other_view in doc.views() {
        if other_view.kind != ViewKind::Plan {
            continue;
        }
        let Some(cut_elevation) = plan_cut_elevation(&other_view) else { continue };
        for entity in doc.entities(other_view.id) {
            let Some(role) = crossing_role(doc, &entity) else { continue };
            let Some(poly) = entity.polyline.as_deref() else { continue };
            let segments = resolved_segments(poly);
            for seg in &segments {
                let global_seg = ResolvedSegment {
                    index: seg.index,
                    from: plan_local_to_global_xy(&other_view, seg.from)?,
                    to: plan_local_to_global_xy(&other_view, seg.to)?,
                    bulge: seg.bulge,
                };
                for hit in intersect_segment_with_infinite_line(&global_seg, cut_line_from, cut_line_to, INTERSECT_EPS) {
                    let theta_deg = crossing_angle_deg(tangent_at_hit(&global_seg, hit.t_a), basis.c_hat);
                    let (z, assumed_z) = if role == LineRole::Cut {
                        (cut_elevation, false)
                    } else {
                        let r = resolve_seen_z(doc, &entity, seg.index)?;
                        (r.z, r.provenance != ProvenanceKind::Authored)
                    };
                    let u = project_to_u(&basis, hit.point);
                    markers.push(Marker {
                        id: marker_id(view.id, entity.id, seg.index, hit.t_a),
                        view_id: view.id,
                        source_entity_id: entity.id,
                        source_view_id: other_view.id,
                        source_segment_index: seg.index,
                        source_segment_param: hit.t_a,
                        position: world_to_local2(transform, Point2 { x: u, y: z }),
                        theta_deg,
                        shape: shape_for_theta(theta_deg),
                        filled: marker_filled(doc, entity.id, &segments, view.id, point_at_param(seg, hit.t_a)),
                        assumed_z,
                    });
                }
            }
        }
    }
    Ok(markers)
}

/// Markers in a placed plan view where every placed vertical view's `cut`/`seen` lines cross the
/// plan's horizontal cut plane.
pub fn compute_markers_for_plan_view(doc: &impl ProjectRead, plan_view: &ViewRef<'_>) -> Result<Vec<Marker>, String> {
    let Some(cut_elevation) = plan_cut_elevation(plan_view) else { return Ok(Vec::new()) };
    let mut markers = Vec::new();
    for other_view in doc.views() {
        if other_view.kind != ViewKind::Vertical || other_view.placement.is_none() {
            continue;
        }
        let Some(host_view) = other_view.host_view_id().and_then(|h| doc.view(h)) else { continue };
        let basis = vertical_basis(&other_view, &host_view)?;

        for entity in doc.entities(other_view.id) {
            if crossing_role(doc, &entity).is_none() {
                continue;
            }
            let Some(poly) = entity.polyline.as_deref() else { continue };
            let segments = resolved_segments(poly);
            for seg in &segments {
                let from_uz = vertical_local_to_uz(&other_view, seg.from);
                let to_uz = vertical_local_to_uz(&other_view, seg.to);
                let seg_uz = ResolvedSegment {
                    index: seg.index,
                    from: Point2 { x: from_uz.u, y: from_uz.z },
                    to: Point2 { x: to_uz.u, y: to_uz.z },
                    bulge: seg.bulge,
                };
                // A horizontal test segment at the cut elevation, padded well past this segment's
                // own u-range so any crossing lands inside it.
                let u_min = from_uz.u.min(to_uz.u);
                let u_max = from_uz.u.max(to_uz.u);
                let pad = 1f64.max(u_max - u_min);
                let cut_test_seg = ResolvedSegment {
                    index: 0,
                    from: Point2 { x: u_min - pad, y: cut_elevation },
                    to: Point2 { x: u_max + pad, y: cut_elevation },
                    bulge: 0.0,
                };
                for hit in intersect_segments(&seg_uz, &cut_test_seg, INTERSECT_EPS) {
                    let theta_deg = crossing_angle_deg(tangent_at_hit(&seg_uz, hit.t_a), Point2 { x: 1.0, y: 0.0 });
                    let u = hit.point.x;
                    let global_xy = Point2 { x: basis.origin_xy.x + u * basis.c_hat.x, y: basis.origin_xy.y + u * basis.c_hat.y };
                    markers.push(Marker {
                        id: marker_id(plan_view.id, entity.id, seg.index, hit.t_a),
                        view_id: plan_view.id,
                        source_entity_id: entity.id,
                        source_view_id: other_view.id,
                        source_segment_index: seg.index,
                        source_segment_param: hit.t_a,
                        position: plan_global_to_local_xy(plan_view, global_xy)?,
                        theta_deg,
                        shape: shape_for_theta(theta_deg),
                        filled: marker_filled(doc, entity.id, &segments, plan_view.id, point_at_param(seg, hit.t_a)),
                        assumed_z: false,
                    });
                }
            }
        }
    }
    Ok(markers)
}

/// Dispatches on the view's kind; empty for an unknown or detail view.
pub fn compute_markers_for_view(doc: &impl ProjectRead, view_id: Id) -> Result<Vec<Marker>, String> {
    let Some(view) = doc.view(view_id) else { return Ok(Vec::new()) };
    match view.kind {
        ViewKind::Vertical => compute_markers_for_vertical_view(doc, &view),
        ViewKind::Plan => compute_markers_for_plan_view(doc, &view),
        ViewKind::Detail => Ok(Vec::new()),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CutLineOverlay {
    pub view_id: Id,
    pub source_view_id: Id,
    pub name: String,
    pub from: Point2,
    pub to: Point2,
    /// Unit vector in `view_id`'s local space: `vertical_gaze_dir` rotated by this view's placement.
    pub gaze_dir: Point2,
}

/// Every placed vertical view's cut line, projected into a placed plan view's local coords.
pub fn compute_cut_lines_for_view(doc: &impl ProjectRead, view_id: Id) -> Result<Vec<CutLineOverlay>, String> {
    let Some(view) = doc.view(view_id) else { return Ok(Vec::new()) };
    let Some(transform) = view.transform().filter(|_| view.kind == ViewKind::Plan) else { return Ok(Vec::new()) };

    let mut overlays = Vec::new();
    for other_view in doc.views() {
        if other_view.kind != ViewKind::Vertical || other_view.placement.is_none() {
            continue;
        }
        let Some(host_view) = other_view.host_view_id().and_then(|h| doc.view(h)) else { continue };
        if host_view.kind != ViewKind::Plan || host_view.placement.is_none() {
            continue;
        }
        let Some(cut_line) = other_view.cut_line() else { continue };
        let Some(seg) = cut_line.segments.first() else { continue };

        let from_global = plan_local_to_global_xy(&host_view, cut_line.start)?;
        let to_global = plan_local_to_global_xy(&host_view, seg.to)?;
        let gaze_global = vertical_gaze_dir(normalize2(sub2(to_global, from_global)), other_view.flipped());
        overlays.push(CutLineOverlay {
            view_id: view.id,
            source_view_id: other_view.id,
            name: other_view.name.to_owned(),
            from: plan_global_to_local_xy(&view, from_global)?,
            to: plan_global_to_local_xy(&view, to_global)?,
            gaze_dir: world_to_local_dir2(transform, gaze_global),
        });
    }
    Ok(overlays)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanCutLineOverlay {
    pub view_id: Id,
    pub source_view_id: Id,
    pub name: String,
    /// Y in the vertical view's local 2D space: the plan's cut elevation through its transform.
    pub local_y: f64,
}

/// Every placed plan view's cut elevation, projected into a placed vertical view's local Z.
pub fn compute_plan_cut_lines_for_view(doc: &impl ProjectRead, view_id: Id) -> Vec<PlanCutLineOverlay> {
    let Some(view) = doc.view(view_id) else { return Vec::new() };
    if view.kind != ViewKind::Vertical || view.placement.is_none() {
        return Vec::new();
    }
    doc.views()
        .filter(|v| v.kind == ViewKind::Plan)
        .filter_map(|other| {
            let z = plan_cut_elevation(&other)?;
            Some(PlanCutLineOverlay {
                view_id: view.id,
                source_view_id: other.id,
                name: other.name.to_owned(),
                local_y: vertical_uz_to_local(&view, Uz { u: 0.0, z }).y,
            })
        })
        .collect()
}

pub fn is_near_marker(marker: &Marker, point: Point2, tolerance_mm: f64) -> bool {
    (marker.position.x - point.x).hypot(marker.position.y - point.y) <= tolerance_mm
}

pub fn entity_role_participates_3d(layers: &[DrawingLayer], role: LineRole, layer_id: &str) -> bool {
    matches!(resolve_entity_role(layers, role, layer_id), LineRole::Cut | LineRole::Seen | LineRole::Datum)
}

// ------------------------------------------------------- a view's cut plane through the solids --

/// Where a placed view's cut plane slices a solid. Derived, never authored.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivedSolidCut {
    /// `{view_id}:solidcut:{n}`.
    pub id: String,
    pub view_id: Id,
    pub source_entity_ids: Vec<Id>,
    /// The solid's own provenance.
    pub provenance: ProvenanceKind,
    /// Closed convex loop in the view's local 2D space, first vertex not repeated, wound CCW.
    #[serde(rename = "loop")]
    pub loop_: Vec<Point2>,
}

/// Prefix of an `ai_origin.proposal_id` that a solid-cut sweep materialised.
pub const SOLID_CUT_PROPOSAL_PREFIX: &str = "solidcut:";

/// How a point on a view's cut plane maps back into the view's local 2D.
enum ToLocal<'a> {
    Plan(ViewRef<'a>),
    Vertical(ViewRef<'a>, VerticalBasis),
}

impl ToLocal<'_> {
    fn map(&self, p: Point3) -> Result<Point2, String> {
        match self {
            ToLocal::Plan(view) => plan_global_to_local_xy(view, Point2 { x: p.x, y: p.y }),
            ToLocal::Vertical(view, basis) => Ok(vertical_uz_to_local(view, Uz { u: project_to_u(basis, Point2 { x: p.x, y: p.y }), z: p.z })),
        }
    }
}

/// A placed view's cut plane in global 3D; `None` for a floating or detail view.
fn cut_plane_for_view<'a>(doc: &'a impl ProjectRead, view: ViewRef<'a>) -> Result<Option<(HalfSpace, ToLocal<'a>)>, String> {
    match view.kind {
        ViewKind::Plan => {
            let Some(d) = plan_cut_elevation(&view) else { return Ok(None) };
            Ok(Some((HalfSpace { n: Point3 { x: 0.0, y: 0.0, z: 1.0 }, d }, ToLocal::Plan(view))))
        }
        ViewKind::Vertical => {
            if view.placement.is_none() {
                return Ok(None);
            }
            let Some(host_view) = view.host_view_id().and_then(|h| doc.view(h)) else { return Ok(None) };
            if host_view.kind == ViewKind::Plan && host_view.placement.is_none() {
                return Ok(None);
            }
            let basis = vertical_basis(&view, &host_view)?;
            // The vertical plane containing the cut line; `flipped` never moves a plane.
            let n = Point3 { x: -basis.c_hat.y, y: basis.c_hat.x, z: 0.0 };
            let d = n.x * basis.origin_xy.x + n.y * basis.origin_xy.y;
            Ok(Some((HalfSpace { n, d }, ToLocal::Vertical(view, basis))))
        }
        ViewKind::Detail => Ok(None),
    }
}

/// Every solid this view's cut plane passes through, as a closed loop in the view's local 2D.
/// `solids` defaults to a full [`synthesize`].
pub fn compute_solid_cuts_for_view(doc: &impl ProjectRead, view_id: Id, solids: Option<&[SynthesizedSolid]>) -> Result<Vec<DerivedSolidCut>, String> {
    let Some(view) = doc.view(view_id) else { return Ok(Vec::new()) };
    let Some((plane, to_local)) = cut_plane_for_view(doc, view)? else { return Ok(Vec::new()) };

    let own_entity_ids: HashSet<Id> = doc.entities(view_id).map(|e| e.id).collect();
    let synthesized;
    let solids = match solids {
        Some(s) => s,
        None => {
            synthesized = synthesize(doc)?.solids;
            &synthesized[..]
        }
    };

    let mut cuts: Vec<DerivedSolidCut> = Vec::new();
    for solid in solids {
        // A view never derives its own shape back out of the solid it made.
        if solid.entity_ids.iter().any(|id| own_entity_ids.contains(id)) {
            continue;
        }
        // Nor does a solid built from a previous sweep's materialised cut entity.
        let from_sweep = solid
            .entity_ids
            .iter()
            .any(|id| doc.entity(*id).and_then(|e| e.ai_origin).is_some_and(|o| o.proposal_id.starts_with(SOLID_CUT_PROPOSAL_PREFIX)));
        if from_sweep {
            continue;
        }

        let loop3 = section_convex_polyhedron(&solid.faces, plane);
        if loop3.len() < 3 {
            continue;
        }
        let mut loop_ = loop3.into_iter().map(|p| to_local.map(p)).collect::<Result<Vec<_>, _>>()?;
        if polygon_area_signed(&loop_) < 0.0 {
            loop_.reverse();
        }
        cuts.push(DerivedSolidCut {
            id: format!("{}:solidcut:{}", view_id, cuts.len()),
            view_id,
            source_entity_ids: solid.entity_ids.clone(),
            provenance: solid.provenance,
            loop_,
        });
    }
    Ok(cuts)
}

#[cfg(test)]
mod floor_cut_tests {
    //! LAMB T516: a new plan cuts at its level's floor, so a section's wall lines drawn from the floor
    //! up end on the cut plane and link there, and an unlinked wall spans floor → floor + 2700.
    use super::*;
    use crate::geometry::transform2d::IDENTITY_TRANSFORM;
    use crate::geometry::{PolySegment, Polyline2};
    use crate::model::{
        AnyView, DEFAULT_CUT_HEIGHT_MM, DrawingEntity, EntityGeometry, Field, Level, PlanView, Project, Provenance, VerticalIntent, VerticalView,
        empty_project,
    };

    const PLAN: Id = Id(10);
    const SEC: Id = Id(20);
    const LEVEL: Id = Id(5);
    const FLOOR: f64 = 3000.0;

    fn pl(pts: &[(f64, f64)], closed: bool) -> Polyline2 {
        Polyline2 {
            start: Point2 { x: pts[0].0, y: pts[0].1 },
            segments: pts[1..].iter().map(|&(x, y)| PolySegment { to: Point2 { x, y }, bulge: 0.0, corner_at_to: None }).collect(),
            closed,
        }
    }

    fn ent(id: u64, view_id: Id, layer: &str, geometry: Polyline2) -> DrawingEntity {
        DrawingEntity {
            id: Id(id),
            view_id,
            role: LineRole::Cut,
            layer_id: layer.into(),
            geometry: EntityGeometry::Polyline { geometry },
            color_override: None,
            lineweight_override: None,
            ai_origin: None,
        }
    }

    /// A plan placed on a level at +3000 with the default cut height, and a section along plan
    /// y = 0 (identity transforms: section local = (u, z)).
    fn project(plan: Vec<DrawingEntity>, sec: Vec<DrawingEntity>) -> Project {
        let mut p = empty_project(None);
        p.levels.push(Level {
            id: LEVEL,
            name: "L1".into(),
            order: 1.0,
            floor_elevation: Field { value: FLOOR, provenance: Provenance::Defaulted { source_default_id: "test".into() } },
        });
        let cut_elevation = FLOOR + p.defaults.default_cut_height_mm;
        p.views.push(AnyView::Plan(PlanView {
            id: PLAN,
            name: "plan".into(),
            entities: plan,
            placement: Some(Placement::Plan { level_id: LEVEL, cut_elevation, transform: IDENTITY_TRANSFORM }),
        }));
        p.views.push(AnyView::Vertical(VerticalView {
            id: SEC,
            name: "A-A".into(),
            intent: VerticalIntent::Section,
            entities: sec,
            placement: Some(Placement::Vertical {
                cut_line: pl(&[(0.0, 0.0), (5000.0, 0.0)], false),
                host_view_id: PLAN,
                transform: IDENTITY_TRANSFORM,
                flipped: None,
            }),
        }));
        p
    }

    fn at(markers: &[Marker]) -> Vec<(f64, f64, usize)> {
        let mut v: Vec<_> = markers.iter().map(|m| (m.position.x, m.position.y, m.source_segment_index)).collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        v
    }

    #[test]
    fn the_default_cut_height_is_the_floor() {
        assert_eq!(DEFAULT_CUT_HEIGHT_MM, 0.0);
        assert_eq!(crate::model::default_project_defaults().default_cut_height_mm, 0.0);
    }

    #[test]
    fn a_plan_line_marks_the_section_on_the_floor_line() -> Result<(), String> {
        let p = project(vec![ent(1, PLAN, "layer-wall-ext", pl(&[(1000.0, -500.0), (1000.0, 500.0)], false))], vec![]);
        let m = compute_markers_for_view(&p, SEC)?;
        assert_eq!(at(&m), [(1000.0, FLOOR, 0)]);
        Ok(())
    }

    #[test]
    fn a_wall_line_from_the_floor_up_marks_the_plan_at_its_end() -> Result<(), String> {
        let up = ent(2, SEC, "layer-wall-ext", pl(&[(1000.0, FLOOR), (1000.0, FLOOR + 2700.0)], false));
        let down = ent(3, SEC, "layer-wall-ext", pl(&[(2000.0, FLOOR + 2700.0), (2000.0, FLOOR)], false));
        let p = project(vec![], vec![up, down]);
        let m = compute_markers_for_view(&p, PLAN)?;
        assert_eq!(at(&m), [(1000.0, 0.0, 0), (2000.0, 0.0, 0)], "{m:?}");
        assert_eq!(m.iter().find(|m| m.position.x == 1000.0).map(|m| m.source_segment_param), Some(0.0));
        assert_eq!(m.iter().find(|m| m.position.x == 2000.0).map(|m| m.source_segment_param), Some(1.0));
        Ok(())
    }

    #[test]
    fn a_loops_bottom_edge_in_the_cut_plane_makes_no_marker_of_its_own() -> Result<(), String> {
        let lp = ent(
            4,
            SEC,
            "layer-wall-ext",
            pl(&[(500.0, FLOOR), (1500.0, FLOOR), (1500.0, FLOOR + 2700.0), (500.0, FLOOR + 2700.0), (500.0, FLOOR)], true),
        );
        let p = project(vec![], vec![lp]);
        let m = compute_markers_for_view(&p, PLAN)?;
        // Only the two uprights, each where it ends on the floor; the bottom edge (segment 0) none.
        assert_eq!(at(&m), [(500.0, 0.0, 3), (1500.0, 0.0, 1)], "{m:?}");
        Ok(())
    }

    #[test]
    fn a_floor_wall_line_auto_links_and_both_markers_fill() -> Result<(), String> {
        let plan_line = ent(1, PLAN, "layer-wall-ext", pl(&[(1000.0, -500.0), (1000.0, 500.0)], false));
        let up = ent(2, SEC, "layer-wall-ext", pl(&[(1000.0, FLOOR), (1000.0, FLOOR + 2700.0)], false));
        let mut p = project(vec![plan_line], vec![up]);
        let mut n = 100u64;
        let mut mint = || {
            n += 1;
            Id(n)
        };
        let actor = crate::model::Actor::User { user_id: "t".into() };
        let e = p.entity(Id(2)).ok_or("no line")?;
        let links = super::super::linking::find_auto_links(&p, &e, &actor, "t", &mut mint)?;
        drop(e);
        assert_eq!(links.len(), 1, "{links:?}");
        p.links.extend(links);
        assert!(compute_markers_for_view(&p, PLAN)?.iter().all(|m| m.filled), "plan marker filled");
        assert!(compute_markers_for_view(&p, SEC)?.iter().all(|m| m.filled), "section marker filled");
        Ok(())
    }

    #[test]
    fn an_unlinked_wall_spans_floor_to_floor_plus_default_height() -> Result<(), String> {
        let wall = ent(1, PLAN, "layer-wall-ext", pl(&[(0.0, 1000.0), (4000.0, 1000.0), (4000.0, 3000.0), (0.0, 3000.0), (0.0, 1000.0)], true));
        let p = project(vec![wall], vec![]);
        let solids = synthesize(&p)?.solids;
        assert_eq!(solids.len(), 1);
        let zs: Vec<f64> = solids[0].faces.iter().flatten().map(|q| q.z).collect();
        let lo = zs.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = zs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert_eq!((lo, hi), (FLOOR, FLOOR + 2700.0));
        Ok(())
    }
}

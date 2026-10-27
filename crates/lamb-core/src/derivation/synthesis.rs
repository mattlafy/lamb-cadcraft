//! Full 3D synthesis (TS `synthesis.ts`): surfaces from `cut` lines, paths from `seen` lines, and
//! capped solids from closed `cut` loops, in every placed view. Full recompute, no caching.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::markers::SOLID_CUT_PROPOSAL_PREFIX;
use super::projection::plan_local_to_global_xy;
use super::read::{EntityRef, ProjectRead, ViewKind, ViewRef};
use super::resolve::{ProvenanceKind, ResolvedSpan, VerticalExtent, resolve_cut_segment, resolve_seen_z};
use super::solids::{SolidForm, SynthesizedSolid, intersect_solids, most_doubtful, prism_half_spaces, prism_with_holes, vertical_prism};
use crate::geometry::polyline::{is_arc, point_at_param, resolved_segments, tangent_at_param};
use crate::geometry::tolerance::{TOLERANCE, deg_to_rad};
use crate::geometry::triangulate::{bridge_holes, is_convex_loop, point_in_loop, polygon_area_signed};
use crate::geometry::{Point2, Point3, ResolvedSegment};
use crate::model::{Id, LineRole, Placement};

/// Always `Plane`: a surface is only emitted for a span that did not go into a solid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SurfaceForm {
    Plane,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthesizedSurface {
    pub entity_id: Id,
    pub segment_index: usize,
    pub span_index: usize,
    pub provenance: ProvenanceKind,
    /// Triangle list, project-global mm, 3 * n vertices.
    pub triangles: Vec<Point3>,
    pub tilt_deg: f64,
    pub form: SurfaceForm,
    pub layer_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthesizedPath {
    pub entity_id: Id,
    pub segment_index: usize,
    pub provenance: ProvenanceKind,
    pub points: Vec<Point3>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthesisResult {
    pub surfaces: Vec<SynthesizedSurface>,
    pub paths: Vec<SynthesizedPath>,
    pub solids: Vec<SynthesizedSolid>,
}

const SAMPLES_PER_SEGMENT: usize = 12;

fn rotate90ccw(v: Point2) -> Point2 {
    Point2 { x: -v.y, y: v.x }
}

/// A placed plan view's cut elevation.
fn plan_cut_elevation(view: &ViewRef<'_>) -> Option<f64> {
    match (view.kind, view.placement) {
        (ViewKind::Plan, Some(Placement::Plan { cut_elevation, .. })) => Some(*cut_elevation),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn path_points_3d(
    plan_view: &ViewRef<'_>,
    seg: &ResolvedSegment,
    from_t: f64,
    to_t: f64,
    contour_z: f64,
    bottom_z: f64,
    top_z: f64,
    tilt_deg: f64,
) -> Result<(Vec<Point3>, Vec<Point3>), String> {
    let rot = plan_view.transform().ok_or_else(|| format!("Plan view {} is floating", plan_view.id.0))?.rotation;
    let (sin, cos) = rot.sin_cos();
    let tan_alpha = deg_to_rad(tilt_deg).tan();

    let mut bottom = Vec::new();
    let mut top = Vec::new();
    // Always positive, so f64::round matches Math.round.
    let steps = ((SAMPLES_PER_SEGMENT as f64) * (to_t - from_t).max(0.05)).round().max(1.0);

    let bottom_offset = (bottom_z - contour_z) * tan_alpha;
    let top_offset = (top_z - contour_z) * tan_alpha;
    let mut i = 0.0;
    while i <= steps {
        let t = from_t + ((to_t - from_t) * i) / steps;
        let global_xy = plan_local_to_global_xy(plan_view, point_at_param(seg, t))?;
        let lt = tangent_at_param(seg, t);
        let perp = rotate90ccw(Point2 { x: lt.x * cos - lt.y * sin, y: lt.x * sin + lt.y * cos });
        bottom.push(Point3 { x: global_xy.x + perp.x * bottom_offset, y: global_xy.y + perp.y * bottom_offset, z: bottom_z });
        top.push(Point3 { x: global_xy.x + perp.x * top_offset, y: global_xy.y + perp.y * top_offset, z: top_z });
        i += 1.0;
    }
    Ok((bottom, top))
}

fn ribbon_triangles(bottom: &[Point3], top: &[Point3]) -> Vec<Point3> {
    let mut tris = Vec::new();
    for (b, t) in bottom.windows(2).zip(top.windows(2)) {
        let (b0, b1, t0, t1) = (b[0], b[1], t[0], t[1]);
        tris.extend([b0, b1, t1, b0, t1, t0]);
    }
    tris
}

/// Surfaces from `cut` lines and paths from `seen` lines in every placed plan view, plus solids
/// from closed `cut` loops in placed plan and vertical views.
pub fn synthesize(doc: &impl ProjectRead) -> Result<SynthesisResult, String> {
    let mut surfaces = Vec::new();
    let mut paths = Vec::new();

    // Solids first: an entity that closes into one emits no ribbons.
    let (plan_solids, conflicted_entity_ids) = plan_solid_records(doc)?;
    let vertical_solids = vertical_solid_records(doc)?;
    let solids = pair_solids(doc, &plan_solids, &vertical_solids);
    // Solids that survived pairing, not merely entities whose plan loop closed.
    let surviving_ids: HashSet<Id> = solids.iter().flat_map(|s| s.entity_ids.iter().copied()).collect();
    let closed_entity_ids: HashSet<Id> = plan_solids
        .iter()
        .filter(|r| surviving_ids.contains(&r.entity_id))
        .flat_map(|r| std::iter::once(r.entity_id).chain(r.hole_ids.iter().copied()))
        .collect();

    for view in doc.views() {
        let Some(contour_z) = plan_cut_elevation(&view) else { continue };

        for entity in doc.entities(view.id) {
            let Some(poly) = entity.polyline.as_deref() else { continue };
            let role = doc.role_of(&entity);

            if role == LineRole::Cut && !closed_entity_ids.contains(&entity.id) {
                // A loop that failed to close because its own spans disagree ghosts every segment.
                let forced_conflict = conflicted_entity_ids.contains(&entity.id);
                for seg in resolved_segments(poly) {
                    let spans = resolve_cut_segment(doc, &entity, seg.index)?;
                    for (span_index, span) in spans.iter().enumerate() {
                        if span.provenance == ProvenanceKind::NotThreeD {
                            continue;
                        }
                        let extent = plane_extent_for(doc, contour_z, span, &spans, span_index);
                        let tilt = span.tilt_deg.unwrap_or(0.0);
                        let (bottom, top) = path_points_3d(&view, &seg, span.from_t, span.to_t, contour_z, extent.bottom, extent.top, tilt)?;
                        surfaces.push(SynthesizedSurface {
                            entity_id: entity.id,
                            segment_index: seg.index,
                            span_index,
                            provenance: if forced_conflict { ProvenanceKind::Conflicted } else { span.provenance },
                            triangles: ribbon_triangles(&bottom, &top),
                            tilt_deg: tilt,
                            form: SurfaceForm::Plane,
                            layer_id: entity.layer_id.to_string(),
                        });
                    }
                }
            }

            if role == LineRole::Seen {
                for seg in resolved_segments(poly) {
                    let seen = resolve_seen_z(doc, &entity, seg.index)?;
                    let points = (0..=SAMPLES_PER_SEGMENT)
                        .map(|i| {
                            let g = plan_local_to_global_xy(&view, point_at_param(&seg, i as f64 / SAMPLES_PER_SEGMENT as f64))?;
                            Ok(Point3 { x: g.x, y: g.y, z: seen.z })
                        })
                        .collect::<Result<Vec<_>, String>>()?;
                    paths.push(SynthesizedPath { entity_id: entity.id, segment_index: seg.index, provenance: seen.provenance, points });
                }
            }
        }
    }

    Ok(SynthesisResult { surfaces, paths, solids })
}

// ------------------------------------------------------- closed loops -> solids --

struct SolidRecord {
    solid: SynthesizedSolid,
    /// The outline's entity.
    entity_id: Id,
    /// Entities of the loops subtracted from it (odd nesting depth, this outline their smallest
    /// container).
    hole_ids: Vec<Id>,
    /// Segment count of the source polyline: what a link's `segment_index` indexes.
    segment_count: usize,
    /// The loop in its own view's 2D space: what `is_convex_loop` is asked about.
    loop2: Vec<Point2>,
    base_loop: Vec<Point3>,
    axis: Point3,
    layer_id: String,
}

/// A closed polyline as a vertex loop, first vertex not repeated: a straight segment gives its
/// start point, a bulged one `SAMPLES_PER_SEGMENT` samples.
fn sample_closed_loop(segs: &[ResolvedSegment]) -> Vec<Point2> {
    let mut lp = Vec::new();
    for seg in segs {
        let steps = if is_arc(seg.bulge) { SAMPLES_PER_SEGMENT } else { 1 };
        for i in 0..steps {
            lp.push(point_at_param(seg, i as f64 / steps as f64));
        }
    }
    lp
}

fn closed_cut_loop(doc: &impl ProjectRead, entity: &EntityRef<'_>) -> Option<Vec<ResolvedSegment>> {
    let poly = entity.polyline.as_deref().filter(|p| p.closed)?;
    if doc.role_of(entity) != LineRole::Cut {
        return None;
    }
    // An unaccepted solid-cut proposal is a slice of an existing solid, not a solid source.
    if entity.ai_origin.is_some_and(|o| !o.accepted && o.proposal_id.starts_with(SOLID_CUT_PROPOSAL_PREFIX)) {
        return None;
    }
    let segs = resolved_segments(poly);
    (segs.len() >= 3).then_some(segs)
}

/// One closed `cut` loop of a view, sampled in that view's 2D space.
struct ClosedLoop<'a> {
    entity: EntityRef<'a>,
    segs: Vec<ResolvedSegment>,
    loop2: Vec<Point2>,
}

fn closed_loops<'a>(doc: &'a impl ProjectRead, view: Id) -> Vec<ClosedLoop<'a>> {
    doc.entities(view)
        .filter_map(|entity| {
            let segs = closed_cut_loop(doc, &entity)?;
            let loop2 = sample_closed_loop(&segs);
            (loop2.len() >= 3).then_some(ClosedLoop { entity, segs, loop2 })
        })
        .collect()
}

/// Loop `b` lies inside loop `a`: every vertex of `b` inside `a` by even–odd, and `b` smaller.
fn loop_inside(b: &[Point2], a: &[Point2]) -> bool {
    polygon_area_signed(b).abs() < polygon_area_signed(a).abs() && b.iter().all(|&p| point_in_loop(a, p))
}

/// Even/odd nesting (T540): per loop, its depth — how many loops of the same layer contain it —
/// and the smallest of those. Even depth is solid, odd depth a void inside its container.
fn nesting(loops: &[ClosedLoop<'_>]) -> Vec<(usize, Option<usize>)> {
    let area = |i: usize| polygon_area_signed(&loops[i].loop2).abs();
    loops
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let containers: Vec<usize> = loops
                .iter()
                .enumerate()
                .filter(|(j, a)| *j != i && a.entity.layer_id == b.entity.layer_id && loop_inside(&b.loop2, &a.loop2))
                .map(|(j, _)| j)
                .collect();
            let smallest = containers.iter().copied().min_by(|&x, &y| area(x).total_cmp(&area(y)));
            (containers.len(), smallest)
        })
        .collect()
}

/// The solid loops of a view: each even-depth loop's index and the indices of its holes.
fn solids_with_holes(loops: &[ClosedLoop<'_>]) -> Vec<(usize, Vec<usize>)> {
    let nest = nesting(loops);
    nest.iter()
        .enumerate()
        .filter(|(_, (depth, _))| depth % 2 == 0)
        .map(|(i, _)| (i, nest.iter().enumerate().filter(|(_, (d, parent))| d % 2 == 1 && *parent == Some(i)).map(|(j, _)| j).collect()))
        .collect()
}

/// A holed outline in 2D: the holes wound opposite to `outer`, and the cap loop — every hole
/// bridged in — wound like it.
fn holed_loops(outer: &[Point2], holes: &[&[Point2]]) -> (Vec<Vec<Point2>>, Vec<Point2>) {
    let ccw = polygon_area_signed(outer) >= 0.0;
    let holes: Vec<Vec<Point2>> = holes
        .iter()
        .map(|h| {
            let mut h = h.to_vec();
            if (polygon_area_signed(&h) >= 0.0) == ccw {
                h.reverse();
            }
            h
        })
        .collect();
    let mut cap = if holes.is_empty() { outer.to_vec() } else { bridge_holes(outer, &holes) };
    if !ccw && !holes.is_empty() {
        cap.reverse();
    }
    (holes, cap)
}

/// The outcome of trying to make one loop's spans agree on a single vertical extent. `conflict`
/// is true only when the loop's own spans genuinely disagree (not tilt, not a missing extent).
struct LoopExtentResult {
    extent: Option<VerticalExtent>,
    conflict: bool,
}

/// The whole loop's single vertical extent, or why it cannot close. The default extent is drawn
/// from the whole loop's spans, so a shape linked on one side takes that extent all the way round.
fn loop_extent(doc: &impl ProjectRead, contour_z: f64, spans: &[ResolvedSpan]) -> LoopExtentResult {
    let none = |conflict| LoopExtentResult { extent: None, conflict };
    if spans.is_empty() || spans.iter().any(|s| s.provenance == ProvenanceKind::NotThreeD) {
        return none(false);
    }
    if spans.iter().any(|s| s.tilt_deg.unwrap_or(0.0).abs() > TOLERANCE.angle_deg) {
        return none(false);
    }
    if spans.iter().any(|s| s.provenance == ProvenanceKind::Conflicted) {
        return none(true);
    }

    let mut extent: Option<VerticalExtent> = None;
    for (i, span) in spans.iter().enumerate() {
        let here = span.extent.unwrap_or_else(|| default_extent_for(doc, contour_z, spans, i));
        match extent {
            None => extent = Some(here),
            Some(e) => {
                if (here.bottom - e.bottom).abs() > TOLERANCE.height_agreement_mm || (here.top - e.top).abs() > TOLERANCE.height_agreement_mm {
                    return none(true);
                }
            }
        }
    }
    LoopExtentResult { extent, conflict: false }
}

/// Closed `cut` loops in placed plan views, capped top and bottom, minus the loops nested inside
/// them by even/odd parity.
fn plan_solid_records(doc: &impl ProjectRead) -> Result<(Vec<SolidRecord>, HashSet<Id>), String> {
    let mut out = Vec::new();
    let mut conflicted = HashSet::new();
    for view in doc.views() {
        let Some(contour_z) = plan_cut_elevation(&view) else { continue };
        let loops = closed_loops(doc, view.id);

        for (oi, hole_idx) in solids_with_holes(&loops) {
            let outline = &loops[oi];
            let entity = &outline.entity;
            let mut spans = Vec::new();
            for seg in &outline.segs {
                spans.extend(resolve_cut_segment(doc, entity, seg.index)?);
            }
            let result = loop_extent(doc, contour_z, &spans);
            if result.conflict {
                conflicted.insert(entity.id);
            }
            let Some(extent) = result.extent else { continue };

            // Hole spans only fold into provenance; a hole's own extent is not computed.
            let mut kinds: Vec<ProvenanceKind> = spans.iter().map(|s| s.provenance).collect();
            for &hi in &hole_idx {
                for seg in &loops[hi].segs {
                    kinds.extend(resolve_cut_segment(doc, &loops[hi].entity, seg.index)?.iter().map(|s| s.provenance));
                }
            }

            let hole2: Vec<&[Point2]> = hole_idx.iter().map(|&hi| loops[hi].loop2.as_slice()).collect();
            let (holes, cap) = holed_loops(&outline.loop2, &hole2);
            let to3 = |lp: &[Point2]| {
                lp.iter().map(|&p| plan_local_to_global_xy(&view, p).map(|g| Point3 { x: g.x, y: g.y, z: contour_z })).collect::<Result<Vec<_>, _>>()
            };
            let base_loop = to3(&outline.loop2)?;
            let holes3 = holes.iter().map(|h| to3(h)).collect::<Result<Vec<_>, _>>()?;
            let axis = Point3 { x: 0.0, y: 0.0, z: 1.0 };
            let faces = prism_with_holes(&base_loop, &holes3, &to3(&cap)?, axis, extent.bottom - contour_z, extent.top - contour_z);
            if faces.is_empty() {
                continue;
            }

            let hole_ids: Vec<Id> = hole_idx.iter().map(|&hi| loops[hi].entity.id).collect();
            out.push(SolidRecord {
                solid: SynthesizedSolid {
                    entity_ids: std::iter::once(entity.id).chain(hole_ids.iter().copied()).collect(),
                    provenance: most_doubtful(&kinds),
                    faces,
                    form: SolidForm::Shell,
                    layer_id: entity.layer_id.to_string(),
                },
                entity_id: entity.id,
                hole_ids,
                segment_count: outline.segs.len(),
                loop2: outline.loop2.clone(),
                base_loop,
                axis,
                layer_id: entity.layer_id.to_string(),
            });
        }
    }
    Ok((out, conflicted))
}

/// Closed `cut` loops in placed vertical views, extruded along the gaze direction, minus the loops
/// nested inside them by even/odd parity. Always `Defaulted`: nothing authored the depth.
fn vertical_solid_records(doc: &impl ProjectRead) -> Result<Vec<SolidRecord>, String> {
    let mut out = Vec::new();
    for view in doc.views() {
        if view.kind != ViewKind::Vertical {
            continue;
        }
        let Some(host_id) = view.host_view_id() else { continue };
        let Some(host_view) = doc.view(host_id) else { continue };
        if host_view.kind == ViewKind::Plan && host_view.placement.is_none() {
            continue; // basis would throw
        }
        let loops = closed_loops(doc, view.id);

        for (oi, hole_idx) in solids_with_holes(&loops) {
            let outline = &loops[oi];
            let hole2: Vec<&[Point2]> = hole_idx.iter().map(|&hi| loops[hi].loop2.as_slice()).collect();
            let (holes, cap) = holed_loops(&outline.loop2, &hole2);
            let prism = vertical_prism(&view, &host_view, &outline.loop2, &holes, &cap, doc.defaults().default_surface_depth_mm)?;
            if prism.faces.is_empty() {
                continue;
            }
            let entity = &outline.entity;
            let hole_ids: Vec<Id> = hole_idx.iter().map(|&hi| loops[hi].entity.id).collect();
            out.push(SolidRecord {
                solid: SynthesizedSolid {
                    entity_ids: std::iter::once(entity.id).chain(hole_ids.iter().copied()).collect(),
                    provenance: ProvenanceKind::Defaulted,
                    faces: prism.faces,
                    form: SolidForm::Shell,
                    layer_id: entity.layer_id.to_string(),
                },
                entity_id: entity.id,
                hole_ids,
                segment_count: outline.segs.len(),
                loop2: outline.loop2.clone(),
                base_loop: prism.base_loop,
                axis: prism.axis,
                layer_id: entity.layer_id.to_string(),
            });
        }
    }
    Ok(out)
}

/// Does any link join any segment of `entity_id` to `other_entity_id`?
fn entities_are_linked(doc: &impl ProjectRead, entity_id: Id, segment_count: usize, other_entity_id: Id) -> bool {
    (0..segment_count).any(|i| doc.links_for_segment(entity_id, i).iter().any(|l| l.other.entity_id == other_entity_id))
}

/// A plan solid linked to one or more convex vertical solids resolves to their intersection.
/// Unlinked shapes that merely overlap keep their own solids, as do concave or holed ones.
fn pair_solids(doc: &impl ProjectRead, plan_solids: &[SolidRecord], vertical_solids: &[SolidRecord]) -> Vec<SynthesizedSolid> {
    let mut consumed: HashSet<usize> = HashSet::new();
    let mut out = Vec::new();

    for plan in plan_solids {
        let mut current: Option<SynthesizedSolid> = None;
        let mut partners: Vec<usize> = Vec::new();

        for (vi, vert) in vertical_solids.iter().enumerate() {
            if !entities_are_linked(doc, plan.entity_id, plan.segment_count, vert.entity_id) {
                continue;
            }
            // Convex only: a concave or holed loop's half-space set is not the shape.
            if !is_convex_loop(&plan.loop2) || !is_convex_loop(&vert.loop2) || !plan.hole_ids.is_empty() || !vert.hole_ids.is_empty() {
                continue;
            }
            // The vertical prism contributes no depth caps.
            let Some(clipped) = intersect_solids(current.as_ref().unwrap_or(&plan.solid), &prism_half_spaces(&vert.base_loop, vert.axis, None))
            else {
                continue; // no overlap in space -> both solids stand
            };
            current = Some(clipped);
            partners.push(vi);
        }

        let Some(current) = current.filter(|_| !partners.is_empty()) else {
            out.push(plan.solid.clone());
            continue;
        };
        consumed.extend(partners.iter().copied());
        let partner_records: Vec<&SolidRecord> = partners.iter().filter_map(|&i| vertical_solids.get(i)).collect();
        let mut entity_ids = plan.solid.entity_ids.clone();
        entity_ids.extend(partner_records.iter().map(|p| p.entity_id));
        let mut kinds = vec![plan.solid.provenance];
        kinds.extend(partner_records.iter().map(|p| p.solid.provenance));
        out.push(SynthesizedSolid {
            entity_ids,
            provenance: most_doubtful(&kinds),
            faces: current.faces,
            form: SolidForm::Shell,
            layer_id: plan.layer_id.clone(),
        });
    }

    for (vi, vert) in vertical_solids.iter().enumerate() {
        if !consumed.contains(&vi) {
            out.push(vert.solid.clone());
        }
    }
    out
}

/// A flanking span's extent when any other span has one, else the project default height above
/// the contour.
fn default_extent_for(doc: &impl ProjectRead, contour_z: f64, all_spans: &[ResolvedSpan], span_index: usize) -> VerticalExtent {
    if let Some(e) = all_spans.iter().enumerate().find(|(i, s)| *i != span_index && s.extent.is_some()).and_then(|(_, s)| s.extent) {
        return e;
    }
    VerticalExtent { bottom: contour_z, top: contour_z + doc.defaults().default_surface_height_mm }
}

/// The extent a plane draws at: its own, else the union of a conflicted span's candidates, else
/// [`default_extent_for`].
fn plane_extent_for(doc: &impl ProjectRead, contour_z: f64, span: &ResolvedSpan, all_spans: &[ResolvedSpan], span_index: usize) -> VerticalExtent {
    if let Some(e) = span.extent {
        return e;
    }
    if span.provenance == ProvenanceKind::Conflicted
        && let Some(candidates) = span.conflict_candidates.as_ref().filter(|c| !c.is_empty())
    {
        return VerticalExtent {
            bottom: candidates.iter().fold(f64::INFINITY, |m, c| m.min(c.extent.bottom)),
            top: candidates.iter().fold(f64::NEG_INFINITY, |m, c| m.max(c.extent.top)),
        };
    }
    default_extent_for(doc, contour_z, all_spans, span_index)
}

#[cfg(test)]
mod nested_loop_tests {
    //! T540: closed cut loops nested on one layer synthesize by even/odd parity.
    use super::*;
    use crate::derivation::solids::solid_triangles;
    use crate::geometry::point::{cross3, sub3};
    use crate::geometry::transform2d::IDENTITY_TRANSFORM;
    use crate::geometry::{PolySegment, Polyline2};
    use crate::model::{AnyView, DrawingEntity, EntityGeometry, PlanView, Project, VerticalIntent, VerticalView, empty_project};

    const PLAN: Id = Id(100);
    const SECTION: Id = Id(200);

    fn closed_rect(w: f64, h: f64, cx: f64, cy: f64) -> Polyline2 {
        let (x0, y0, x1, y1) = (cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
        let to = |x, y| PolySegment { to: Point2 { x, y }, bulge: 0.0, corner_at_to: None };
        Polyline2 { start: Point2 { x: x0, y: y0 }, segments: vec![to(x1, y0), to(x1, y1), to(x0, y1), to(x0, y0)], closed: true }
    }

    fn ent(id: u64, view: Id, layer: &str, geometry: Polyline2) -> DrawingEntity {
        DrawingEntity {
            id: Id(id),
            view_id: view,
            role: LineRole::Cut,
            layer_id: layer.into(),
            geometry: EntityGeometry::Polyline { geometry },
            color_override: None,
            lineweight_override: None,
            ai_origin: None,
        }
    }

    fn plan_project(entities: Vec<DrawingEntity>) -> Project {
        let mut p = empty_project(None);
        let placement = Placement::Plan { level_id: Id(1), cut_elevation: 1000.0, transform: IDENTITY_TRANSFORM };
        p.views.push(AnyView::Plan(PlanView { id: PLAN, name: "plan".into(), entities, placement: Some(placement) }));
        p
    }

    /// Concentric rectangles on layer-0, outermost first, centred on (5000, 4000).
    fn concentric(n: usize) -> Project {
        plan_project(
            (0..n)
                .map(|k| ent(k as u64 + 1, PLAN, "layer-0", closed_rect(10000.0 - 400.0 * k as f64, 8000.0 - 400.0 * k as f64, 5000.0, 4000.0)))
                .collect(),
        )
    }

    fn top_cap_area(solid: &SynthesizedSolid) -> f64 {
        let tris = solid_triangles(solid);
        let top = tris.iter().fold(f64::NEG_INFINITY, |m, p| m.max(p.z));
        tris.chunks(3)
            .filter(|t| t.iter().all(|p| (p.z - top).abs() < 1e-6))
            .map(|t| ((t[1].x - t[0].x) * (t[2].y - t[0].y) - (t[2].x - t[0].x) * (t[1].y - t[0].y)).abs() / 2.0)
            .sum()
    }

    #[test]
    fn an_outline_and_its_inward_offset_make_one_wall_ring() {
        let solids = synthesize(&concentric(2)).expect("synth").solids;
        assert_eq!(solids.len(), 1);
        let ring = &solids[0];
        assert_eq!(ring.entity_ids, vec![Id(1), Id(2)]);
        let want = 10000.0 * 8000.0 - 9600.0 * 7600.0;
        assert!((top_cap_area(ring) - want).abs() <= 1.0, "top cap {}", top_cap_area(ring));

        // Hole walls: vertical quads on the inner rectangle, normals pointing into the void.
        let centre = Point3 { x: 5000.0, y: 4000.0, z: 0.0 };
        let on_inner = |p: &Point3| (p.x - 5000.0).abs() <= 4800.0 + 1e-6 && (p.y - 4000.0).abs() <= 3800.0 + 1e-6;
        let mut inward = 0;
        let mut outward_outer = 0;
        for f in ring.faces.iter().filter(|f| f.len() == 4 && f.iter().any(|p| (p.z - f[0].z).abs() > 1.0)) {
            let n = cross3(sub3(f[1], f[0]), sub3(f[3], f[0]));
            let to_centre = sub3(centre, f[0]);
            let facing = n.x * to_centre.x + n.y * to_centre.y;
            if f.iter().all(on_inner) {
                assert!(facing > 0.0, "hole wall faces away from the void: {f:?}");
                inward += 1;
            } else if facing < 0.0 {
                outward_outer += 1;
            }
        }
        assert_eq!(inward, 4);
        assert_eq!(outward_outer, 4);
    }

    #[test]
    fn three_rings_alternate_solid_void_solid() {
        let solids = synthesize(&concentric(3)).expect("synth").solids;
        assert_eq!(solids.iter().map(|s| s.entity_ids.clone()).collect::<Vec<_>>(), vec![vec![Id(1), Id(2)], vec![Id(3)]]);
        let solids = synthesize(&concentric(4)).expect("synth").solids;
        assert_eq!(solids.iter().map(|s| s.entity_ids.clone()).collect::<Vec<_>>(), vec![vec![Id(1), Id(2)], vec![Id(3), Id(4)]]);
    }

    #[test]
    fn loops_on_different_layers_never_subtract() {
        let p = plan_project(vec![
            ent(1, PLAN, "layer-0", closed_rect(10000.0, 8000.0, 5000.0, 4000.0)),
            ent(2, PLAN, "layer-wall-ext", closed_rect(9600.0, 7600.0, 5000.0, 4000.0)),
        ]);
        let solids = synthesize(&p).expect("synth").solids;
        assert_eq!(solids.len(), 2);
        assert!((top_cap_area(&solids[0]) - 10000.0 * 8000.0).abs() <= 1.0);
        assert!((top_cap_area(&solids[1]) - 9600.0 * 7600.0).abs() <= 1.0);
    }

    #[test]
    fn a_ring_emits_no_ribbons() {
        assert!(synthesize(&concentric(2)).expect("synth").surfaces.is_empty());
    }

    #[test]
    fn a_section_with_nested_loops_makes_one_holed_vertical_solid() {
        let mut p = plan_project(Vec::new());
        let cut_line = Polyline2 {
            start: Point2 { x: 0.0, y: 0.0 },
            segments: vec![PolySegment { to: Point2 { x: 20000.0, y: 0.0 }, bulge: 0.0, corner_at_to: None }],
            closed: false,
        };
        p.views.push(AnyView::Vertical(VerticalView {
            id: SECTION,
            name: "section".into(),
            intent: VerticalIntent::Section,
            entities: vec![
                ent(11, SECTION, "layer-0", closed_rect(4000.0, 3000.0, 5000.0, 1500.0)),
                ent(12, SECTION, "layer-0", closed_rect(3000.0, 2000.0, 5000.0, 1500.0)),
            ],
            placement: Some(Placement::Vertical { cut_line, host_view_id: PLAN, transform: IDENTITY_TRANSFORM, flipped: None }),
        }));
        let solids = synthesize(&p).expect("synth").solids;
        assert_eq!(solids.len(), 1);
        assert_eq!(solids[0].entity_ids, vec![Id(11), Id(12)]);
        // 4 outer walls + 4 hole walls + 2 caps.
        assert_eq!(solids[0].faces.len(), 10);
    }
}

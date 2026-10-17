//! Full 3D synthesis (TS `synthesis.ts`): surfaces from `cut` lines, paths from `seen` lines, and
//! capped solids from closed `cut` loops, in every placed view. Full recompute, no caching.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::markers::SOLID_CUT_PROPOSAL_PREFIX;
use super::projection::plan_local_to_global_xy;
use super::read::{EntityRef, ProjectRead, ViewKind, ViewRef};
use super::resolve::{ProvenanceKind, ResolvedSpan, VerticalExtent, resolve_cut_segment, resolve_seen_z};
use super::solids::{SolidForm, SynthesizedSolid, intersect_solids, most_doubtful, prism_from_loop, prism_half_spaces, vertical_prism};
use crate::geometry::polyline::{is_arc, point_at_param, resolved_segments, tangent_at_param};
use crate::geometry::tolerance::{TOLERANCE, deg_to_rad};
use crate::geometry::triangulate::is_convex_loop;
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
    let closed_entity_ids: HashSet<Id> = plan_solids.iter().filter(|r| surviving_ids.contains(&r.entity_id)).map(|r| r.entity_id).collect();

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
    entity_id: Id,
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

/// Closed `cut` loops in placed plan views, capped top and bottom.
fn plan_solid_records(doc: &impl ProjectRead) -> Result<(Vec<SolidRecord>, HashSet<Id>), String> {
    let mut out = Vec::new();
    let mut conflicted = HashSet::new();
    for view in doc.views() {
        let Some(contour_z) = plan_cut_elevation(&view) else { continue };

        for entity in doc.entities(view.id) {
            let Some(segs) = closed_cut_loop(doc, &entity) else { continue };

            let mut spans = Vec::new();
            for seg in &segs {
                spans.extend(resolve_cut_segment(doc, &entity, seg.index)?);
            }
            let result = loop_extent(doc, contour_z, &spans);
            if result.conflict {
                conflicted.insert(entity.id);
            }
            let Some(extent) = result.extent else { continue };

            let loop2 = sample_closed_loop(&segs);
            if loop2.len() < 3 {
                continue;
            }
            let base_loop = loop2
                .iter()
                .map(|&p| plan_local_to_global_xy(&view, p).map(|g| Point3 { x: g.x, y: g.y, z: contour_z }))
                .collect::<Result<Vec<_>, _>>()?;
            let axis = Point3 { x: 0.0, y: 0.0, z: 1.0 };
            let faces = prism_from_loop(&base_loop, axis, extent.bottom - contour_z, extent.top - contour_z);
            if faces.is_empty() {
                continue;
            }

            let kinds: Vec<ProvenanceKind> = spans.iter().map(|s| s.provenance).collect();
            out.push(SolidRecord {
                solid: SynthesizedSolid {
                    entity_ids: vec![entity.id],
                    provenance: most_doubtful(&kinds),
                    faces,
                    form: SolidForm::Shell,
                    layer_id: entity.layer_id.to_string(),
                },
                entity_id: entity.id,
                segment_count: segs.len(),
                loop2,
                base_loop,
                axis,
                layer_id: entity.layer_id.to_string(),
            });
        }
    }
    Ok((out, conflicted))
}

/// Closed `cut` loops in placed vertical views, extruded along the gaze direction. Always
/// `Defaulted`: nothing authored the depth.
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

        for entity in doc.entities(view.id) {
            let Some(segs) = closed_cut_loop(doc, &entity) else { continue };
            let loop2 = sample_closed_loop(&segs);
            if loop2.len() < 3 {
                continue;
            }

            let prism = vertical_prism(&view, &host_view, &loop2, doc.defaults().default_surface_depth_mm)?;
            if prism.faces.is_empty() {
                continue;
            }
            out.push(SolidRecord {
                solid: SynthesizedSolid {
                    entity_ids: vec![entity.id],
                    provenance: ProvenanceKind::Defaulted,
                    faces: prism.faces,
                    form: SolidForm::Shell,
                    layer_id: entity.layer_id.to_string(),
                },
                entity_id: entity.id,
                segment_count: segs.len(),
                loop2,
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
/// Unlinked shapes that merely overlap keep their own solids.
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
            // Convex only: a concave loop's half-space set is not the shape.
            if !is_convex_loop(&plan.loop2) || !is_convex_loop(&vert.loop2) {
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
        let mut entity_ids = vec![plan.entity_id];
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

//! Authority resolution for one segment (TS `resolve.ts`): a `cut` line's vertical extent and tilt
//! from linked vertical-view lines, and a `seen` line's Z. Source: LAMB's
//! `01-core-concept/authority-resolution-algorithm.md`.

use serde::{Deserialize, Serialize};

use super::projection::{vertical_basis, vertical_local_to_uz};
use super::read::{EntityRef, ProjectRead, ViewKind, ViewRef};
use super::tilt::signed_tilt_deg;
use crate::geometry::intersect::crossing_angle_deg;
use crate::geometry::polyline::{resolved_segments, tangent_at_param};
use crate::geometry::tolerance::TOLERANCE;
use crate::geometry::{Point2, Point3, ResolvedSegment};
use crate::model::{Id, Placement};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceKind {
    Authored,
    Proposed,
    Defaulted,
    Ambiguous,
    Conflicted,
    #[serde(rename = "not_3d")]
    NotThreeD,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VerticalExtent {
    pub bottom: f64,
    pub top: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtentCandidate {
    pub view_id: Id,
    pub extent: VerticalExtent,
}

/// One contiguous run of a segment's resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedSpan {
    pub from_t: f64,
    pub to_t: f64,
    pub provenance: ProvenanceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extent: Option<VerticalExtent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt_deg: Option<f64>,
    /// Present when `provenance` is `Conflicted`: the disagreeing candidates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_candidates: Option<Vec<ExtentCandidate>>,
}

impl ResolvedSpan {
    fn bare(from_t: f64, to_t: f64, provenance: ProvenanceKind) -> Self {
        ResolvedSpan { from_t, to_t, provenance, extent: None, tilt_deg: None, conflict_candidates: None }
    }

    fn with_extent(from_t: f64, to_t: f64, provenance: ProvenanceKind, extent: VerticalExtent, tilt_deg: f64) -> Self {
        ResolvedSpan { from_t, to_t, provenance, extent: Some(extent), tilt_deg: Some(tilt_deg), conflict_candidates: None }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZCandidate {
    pub view_id: Id,
    pub z: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeenZResult {
    pub z: f64,
    /// `Authored`, `Defaulted` or `Conflicted`.
    pub provenance: ProvenanceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_candidates: Option<Vec<ZCandidate>>,
}

struct CrossingContribution {
    /// Param along the plan segment, 0..1.
    t: f64,
    view_id: Id,
    extent: VerticalExtent,
    tilt_deg: f64,
    /// The contributing vertical line is an unaccepted AI line.
    proposed: bool,
}

/// A linked partner that is a polyline segment in a placed vertical view, and whether it is an
/// unaccepted AI line.
fn linked_vertical_segment<P: ProjectRead>(project: &P, other: Id, segment: usize) -> Option<(ViewRef<'_>, ResolvedSegment, bool)> {
    let view = project.view(project.entity_view_id(other)?)?;
    if view.kind != ViewKind::Vertical || view.placement.is_none() {
        return None;
    }
    let entity = project.entity(other)?;
    let seg = *resolved_segments(entity.polyline.as_deref()?).get(segment)?;
    let proposed = entity.ai_origin.is_some_and(|o| !o.accepted);
    Some((view, seg, proposed))
}

/// A `seen` line's Z: a linked vertical-view line gives an authored Z; with no link, the floor
/// level of the drawing view.
pub fn resolve_seen_z(project: &impl ProjectRead, entity: &EntityRef<'_>, segment_index: usize) -> Result<SeenZResult, String> {
    let mut candidates: Vec<ZCandidate> = Vec::new();
    for l in project.links_for_segment(entity.id, segment_index) {
        let Some((other_view, seg, _)) = linked_vertical_segment(project, l.other.entity_id, l.other.segment_index) else { continue };
        let t = l.other.segment_param;
        let pt = Point2 { x: seg.from.x + (seg.to.x - seg.from.x) * t, y: seg.from.y + (seg.to.y - seg.from.y) * t };
        candidates.push(ZCandidate { view_id: other_view.id, z: vertical_local_to_uz(&other_view, pt).z });
    }

    let Some(first) = candidates.first() else {
        let floor_z = match project.view(entity.view_id) {
            Some(ViewRef { kind: ViewKind::Plan, placement: Some(Placement::Plan { level_id, .. }), .. }) => {
                project.level(*level_id).map_or(0.0, |l| l.floor_elevation.value)
            }
            _ => 0.0,
        };
        return Ok(SeenZResult { z: floor_z, provenance: ProvenanceKind::Defaulted, conflict_candidates: None });
    };
    let z = first.z;
    if candidates.iter().all(|c| (c.z - z).abs() <= TOLERANCE.height_agreement_mm) {
        return Ok(SeenZResult { z, provenance: ProvenanceKind::Authored, conflict_candidates: None });
    }
    Ok(SeenZResult { z, provenance: ProvenanceKind::Conflicted, conflict_candidates: Some(candidates) })
}

fn extents_agree(a: &VerticalExtent, b: &VerticalExtent) -> bool {
    (a.top - b.top).abs() <= TOLERANCE.height_agreement_mm && (a.bottom - b.bottom).abs() <= TOLERANCE.height_agreement_mm
}

fn authored_or_proposed(proposed: bool) -> ProvenanceKind {
    if proposed { ProvenanceKind::Proposed } else { ProvenanceKind::Authored }
}

/// One segment of a `cut` line: XY path from the plan that drew it, contour elevation the plan's
/// cut elevation, vertical extent and tilt from linked vertical-view lines.
pub fn resolve_cut_segment(project: &impl ProjectRead, entity: &EntityRef<'_>, segment_index: usize) -> Result<Vec<ResolvedSpan>, String> {
    let Some(view) = project.view(entity.view_id).filter(|v| v.kind == ViewKind::Plan && v.placement.is_some()) else {
        return Ok(vec![ResolvedSpan::bare(0.0, 1.0, ProvenanceKind::Defaulted)]);
    };
    let Some(poly) = entity.polyline.as_deref() else { return Ok(vec![ResolvedSpan::bare(0.0, 1.0, ProvenanceKind::NotThreeD)]) };
    let Some(plan_seg) = resolved_segments(poly).get(segment_index).copied() else {
        return Ok(vec![ResolvedSpan::bare(0.0, 1.0, ProvenanceKind::NotThreeD)]);
    };

    let mut contributions: Vec<CrossingContribution> = Vec::new();
    for l in project.links_for_segment(entity.id, segment_index) {
        let Some((other_view, other_seg, proposed)) = linked_vertical_segment(project, l.other.entity_id, l.other.segment_index) else { continue };
        if let Some(mut c) = contribution_from_crossing(project, &view, &plan_seg, &other_view, &other_seg, l.mine.segment_param)? {
            c.proposed = proposed;
            contributions.push(c);
        }
    }

    if contributions.is_empty() {
        return Ok(vec![ResolvedSpan::bare(0.0, 1.0, ProvenanceKind::Defaulted)]);
    }

    // JS's stable sort by t.
    contributions.sort_by(|a, b| a.t.total_cmp(&b.t));

    // Group crossings that land at (nearly) the same param along the segment.
    let len = (plan_seg.to.x - plan_seg.from.x).hypot(plan_seg.to.y - plan_seg.from.y);
    let seg_length_mm = if len == 0.0 || len.is_nan() { 1.0 } else { len }; // JS `|| 1`
    let group_tolerance_t = TOLERANCE.grouping_mm / seg_length_mm;
    let mut groups: Vec<Vec<CrossingContribution>> = Vec::new();
    for c in contributions {
        match groups.last_mut() {
            Some(last) if last.first().is_some_and(|f| (c.t - f.t).abs() <= group_tolerance_t) => last.push(c),
            _ => groups.push(vec![c]),
        }
    }

    // Case 1: two+ links at the SAME point disagreeing -> conflicted.
    for group in &groups {
        let Some(first) = group.first() else { continue };
        if group.len() > 1 && !group.iter().all(|g| extents_agree(&g.extent, &first.extent)) {
            let mut span = ResolvedSpan::bare(0.0, 1.0, ProvenanceKind::Conflicted);
            span.conflict_candidates = Some(group.iter().map(|g| ExtentCandidate { view_id: g.view_id, extent: g.extent }).collect());
            return Ok(vec![span]);
        }
    }

    // Case 2: one crossing, or several that all agree -> authored (proposed if every contributing
    // line is an unaccepted AI line), whole segment.
    let representative: Vec<&CrossingContribution> = groups.iter().filter_map(|g| g.first()).collect();
    let (Some(rep), Some(last)) = (representative.first(), representative.last()) else {
        return Err("resolve_cut_segment: no crossing groups".into());
    };
    let all_agree = representative.iter().all(|g| extents_agree(&g.extent, &rep.extent));
    if groups.len() == 1 || all_agree {
        let provenance = authored_or_proposed(representative.iter().all(|g| g.proposed));
        return Ok(vec![ResolvedSpan::with_extent(0.0, 1.0, provenance, rep.extent, rep.tilt_deg)]);
    }

    // Case 3: crossings at different points that disagree -> outward-authored, ambiguous between.
    Ok(vec![
        ResolvedSpan::with_extent(0.0, rep.t, authored_or_proposed(rep.proposed), rep.extent, rep.tilt_deg),
        ResolvedSpan::bare(rep.t, last.t, ProvenanceKind::Ambiguous),
        ResolvedSpan::with_extent(last.t, 1.0, authored_or_proposed(last.proposed), last.extent, last.tilt_deg),
    ])
}

fn contribution_from_crossing<P: ProjectRead>(
    project: &P,
    plan_view: &ViewRef<'_>,
    plan_seg: &ResolvedSegment,
    vert_view: &ViewRef<'_>,
    vert_seg: &ResolvedSegment,
    plan_param: f64,
) -> Result<Option<CrossingContribution>, String> {
    let (Some(plan_t), Some(host_id)) = (plan_view.transform(), vert_view.host_view_id()) else { return Ok(None) };
    let Some(host_view) = project.view(host_id) else { return Ok(None) };
    let basis = vertical_basis(vert_view, &host_view)?;

    // p_hat is in the plan's local space; rotate it into global by the plan's rotation.
    let p_hat = tangent_at_param(plan_seg, plan_param);
    let (sin, cos) = plan_t.rotation.sin_cos();
    let p_hat_global = Point2 { x: p_hat.x * cos - p_hat.y * sin, y: p_hat.x * sin + p_hat.y * cos };

    let dx = vert_seg.to.x - vert_seg.from.x;
    let dy = vert_seg.to.y - vert_seg.from.y;
    let z1 = vertical_local_to_uz(vert_view, vert_seg.from).z;
    let z2 = vertical_local_to_uz(vert_view, vert_seg.to).z;

    Ok(Some(CrossingContribution {
        t: plan_param,
        view_id: vert_view.id,
        extent: VerticalExtent { top: z1.max(z2), bottom: z1.min(z2) },
        tilt_deg: signed_tilt_deg(p_hat_global, basis.c_hat, dx, dy),
        proposed: false,
    }))
}

/// Plan crossing angle between a plan segment's global direction and a vertical view's cut line;
/// 90 when the view is floating or its host is missing.
pub fn crossing_theta_deg(project: &impl ProjectRead, plan_seg_global_dir: Point2, vert_view: &ViewRef<'_>) -> Result<f64, String> {
    let Some(host_id) = vert_view.host_view_id() else { return Ok(90.0) };
    let Some(host_view) = project.view(host_id) else { return Ok(90.0) };
    Ok(crossing_angle_deg(plan_seg_global_dir, vertical_basis(vert_view, &host_view)?.c_hat))
}

pub fn point3(x: f64, y: f64, z: f64) -> Point3 {
    Point3 { x, y, z }
}

//! Authority resolution for one segment (TS `resolve.ts`): a `cut` line's vertical extent and tilt
//! from linked vertical-view lines, and a `seen` line's Z. Source: LAMB's
//! `01-core-concept/authority-resolution-algorithm.md`.

use serde::{Deserialize, Serialize};

use super::projection::{plan_local_to_global_xy, vertical_basis, vertical_local_to_uz};
use super::read::{EntityRef, ProjectRead, ViewKind, ViewRef};
use super::tilt::signed_tilt_deg;
use crate::geometry::intersect::crossing_angle_deg;
use crate::geometry::polyline::{point_at_param, resolved_segments, tangent_at_param};
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

/// [`resolve_cut_segment`], with one vertical reference setting the height of a whole polyline: a
/// segment with no crossing of its own takes the extent the polyline's other segments agree on.
/// Any disagreement among them, or a conflicted or ambiguous span, leaves the segment defaulted.
pub fn resolve_cut_segment_inherited(project: &impl ProjectRead, entity: &EntityRef<'_>, segment_index: usize) -> Result<Vec<ResolvedSpan>, String> {
    let own = resolve_cut_segment(project, entity, segment_index)?;
    let bare_default = matches!(own.as_slice(), [span] if span.provenance == ProvenanceKind::Defaulted && span.extent.is_none());
    if !bare_default {
        return Ok(own);
    }
    let Some(poly) = entity.polyline.as_deref() else { return Ok(own) };

    let mut sources: Vec<(VerticalExtent, ProvenanceKind)> = Vec::new();
    for seg in resolved_segments(poly) {
        if seg.index == segment_index {
            continue;
        }
        for span in resolve_cut_segment(project, entity, seg.index)? {
            match (span.provenance, span.extent) {
                (ProvenanceKind::Defaulted, _) => {}
                (ProvenanceKind::Authored | ProvenanceKind::Proposed, Some(extent)) => sources.push((extent, span.provenance)),
                _ => return Ok(own),
            }
        }
    }
    let Some(&(extent, _)) = sources.first() else { return Ok(own) };
    if !sources.iter().all(|(e, _)| extents_agree(e, &extent)) {
        return Ok(own);
    }
    let all_proposed = sources.iter().all(|(_, p)| *p == ProvenanceKind::Proposed);
    Ok(vec![ResolvedSpan::with_extent(0.0, 1.0, authored_or_proposed(all_proposed), extent, 0.0)])
}

/// Where a linked vertical-view line crosses a plan polyline.
struct PolylineCrossing {
    /// Plan-local.
    location: Point2,
    global_xy: Point2,
    c_hat: Point2,
    extent: VerticalExtent,
    /// `dz/du` along `c_hat`, when the vertical line slopes.
    slope: Option<f64>,
    /// A sloped line's z at the crossing; any other line's top.
    height: f64,
}

/// Every linked vertical line's crossing with this polyline, by segment, then along the segment.
fn polyline_crossings(project: &impl ProjectRead, entity: &EntityRef<'_>) -> Result<Vec<PolylineCrossing>, String> {
    let mut crossings = Vec::new();
    let Some(view) = project.view(entity.view_id).filter(|v| v.kind == ViewKind::Plan && v.placement.is_some()) else { return Ok(crossings) };
    let Some(poly) = entity.polyline.as_deref() else { return Ok(crossings) };
    for seg in resolved_segments(poly) {
        let mut links = project.links_for_segment(entity.id, seg.index);
        links.sort_by(|a, b| a.mine.segment_param.total_cmp(&b.mine.segment_param));
        for l in links {
            let Some((vert_view, vert_seg, _)) = linked_vertical_segment(project, l.other.entity_id, l.other.segment_index) else { continue };
            let Some(host_view) = vert_view.host_view_id().and_then(|h| project.view(h)) else { continue };
            let basis = vertical_basis(&vert_view, &host_view)?;
            let from = vertical_local_to_uz(&vert_view, vert_seg.from);
            let to = vertical_local_to_uz(&vert_view, vert_seg.to);
            let (du, dz) = (to.u - from.u, to.z - from.z);
            let sloped = du.abs() > TOLERANCE.coincidence_mm && dz.abs() > TOLERANCE.height_agreement_mm;
            let location = point_at_param(&seg, l.mine.segment_param);
            let global_xy = plan_local_to_global_xy(&view, location)?;
            // By the crossing's u, not the link's anchor param: only one end of a link has to lie on its marker.
            let u = (global_xy.x - basis.origin_xy.x) * basis.c_hat.x + (global_xy.y - basis.origin_xy.y) * basis.c_hat.y;
            let height = if sloped { from.z + dz / du * (u - from.u) } else { from.z.max(to.z) };
            crossings.push(PolylineCrossing {
                location,
                global_xy,
                c_hat: basis.c_hat,
                extent: VerticalExtent { top: from.z.max(to.z), bottom: from.z.min(to.z) },
                slope: sloped.then(|| dz / du),
                height,
            });
        }
    }
    Ok(crossings)
}

/// A plan polyline whose linked vertical lines disagree on its height: the plan-local location of
/// the first crossing that disagrees, and a note. Flat and vertical lines must give one extent. A
/// sloped line defines a plane rising along its view's cut line, and every other crossing's height
/// must lie on that plane at its own XY, so a horizontal line in a perpendicular section agrees
/// with a diagonal wherever it meets the diagonal's plane.
pub fn polyline_height_conflict(project: &impl ProjectRead, entity: &EntityRef<'_>) -> Result<Option<(Point2, String)>, String> {
    let crossings = polyline_crossings(project, entity)?;
    let plane = crossings.iter().find_map(|c| c.slope.map(|slope| (c, slope)));
    let failing = if let Some((origin, slope)) = plane {
        crossings.iter().find(|c| {
            let (dx, dy) = (c.global_xy.x - origin.global_xy.x, c.global_xy.y - origin.global_xy.y);
            let on_plane = origin.height + slope * (dx * origin.c_hat.x + dy * origin.c_hat.y);
            (c.height - on_plane).abs() > TOLERANCE.height_agreement_mm
        })
    } else {
        crossings.first().and_then(|first| crossings.iter().find(|c| !extents_agree(&c.extent, &first.extent)))
    };
    Ok(failing.map(|c| (c.location, "This polyline's linked lines disagree on its height.".to_owned())))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::transform2d::IDENTITY_TRANSFORM;
    use crate::geometry::{PolySegment, Polyline2};
    use crate::model::{
        Actor, AnyView, DrawingEntity, EntityGeometry, LineRole, LinkAnchor, LinkOrigin, LinkRecord, PlanView, Project, VerticalIntent, VerticalView,
        empty_project,
    };

    const PLAN: Id = Id(100);
    /// Cut line x = 3000, u = y + 1000.
    const SECTION_A: Id = Id(200);
    /// Cut line y = 2000, u = x + 1000.
    const SECTION_B: Id = Id(300);
    const CEILING: Id = Id(1);

    fn poly(points: &[(f64, f64)]) -> Polyline2 {
        let to = |&(x, y): &(f64, f64)| PolySegment { to: Point2 { x, y }, bulge: 0.0, corner_at_to: None };
        Polyline2 { start: Point2 { x: points[0].0, y: points[0].1 }, segments: points[1..].iter().map(to).collect(), closed: false }
    }

    fn ent(id: Id, view: Id, geometry: Polyline2) -> DrawingEntity {
        DrawingEntity {
            id,
            view_id: view,
            role: LineRole::Cut,
            layer_id: "layer-0".into(),
            geometry: EntityGeometry::Polyline { geometry },
            color_override: None,
            lineweight_override: None,
            ai_origin: None,
        }
    }

    fn section(id: Id, cut: &[(f64, f64)], entities: Vec<DrawingEntity>) -> AnyView {
        let placement = Placement::Vertical { cut_line: poly(cut), host_view_id: PLAN, transform: IDENTITY_TRANSFORM, flipped: None };
        AnyView::Vertical(VerticalView { id, name: format!("s{}", id.0), intent: VerticalIntent::Section, entities, placement: Some(placement) })
    }

    fn link(id: u64, seg: usize, param: f64, other: Id, other_param: f64) -> LinkRecord {
        LinkRecord {
            id: Id(id),
            a: LinkAnchor { entity_id: CEILING, segment_index: seg, segment_param: param },
            b: LinkAnchor { entity_id: other, segment_index: 0, segment_param: other_param },
            origin: LinkOrigin::Explicit,
            created_at: String::new(),
            created_by: Actor::User { user_id: "u".into() },
            stale: None,
        }
    }

    /// An 8000 x 5000 rectangle drawn open, with lines 11 and 12 in section A, 21 in section B.
    fn project(a: [(f64, f64); 2], a2: [(f64, f64); 2], b: [(f64, f64); 2], links: Vec<LinkRecord>) -> Project {
        let mut p = empty_project(None);
        let placement = Placement::Plan { level_id: Id(1), cut_elevation: 1000.0, transform: IDENTITY_TRANSFORM };
        let ceiling = ent(CEILING, PLAN, poly(&[(0.0, 0.0), (8000.0, 0.0), (8000.0, 5000.0), (0.0, 5000.0)]));
        p.views.push(AnyView::Plan(PlanView { id: PLAN, name: "plan".into(), entities: vec![ceiling], placement: Some(placement) }));
        let (e11, e12, e21) = (ent(Id(11), SECTION_A, poly(&a)), ent(Id(12), SECTION_A, poly(&a2)), ent(Id(21), SECTION_B, poly(&b)));
        p.views.push(section(SECTION_A, &[(3000.0, -1000.0), (3000.0, 6000.0)], vec![e11, e12]));
        p.views.push(section(SECTION_B, &[(-1000.0, 2000.0), (9000.0, 2000.0)], vec![e21]));
        p.links = links;
        p
    }

    const FLAT_2700: [(f64, f64); 2] = [(0.0, 2700.0), (7000.0, 2700.0)];
    /// z = 2400 at y = 0, z = 3400 at y = 5000: rises 0.2 per mm of y.
    const DIAGONAL: [(f64, f64); 2] = [(1000.0, 2400.0), (6000.0, 3400.0)];

    fn ceiling(p: &Project) -> EntityRef<'_> {
        p.entity(CEILING).expect("ceiling")
    }

    #[test]
    fn a_segment_with_no_crossing_inherits_the_polylines_one_height() {
        let p = project(FLAT_2700, FLAT_2700, FLAT_2700, vec![link(1, 0, 0.375, Id(11), 0.5)]);
        assert_eq!(resolve_cut_segment(&p, &ceiling(&p), 1).expect("own")[0].provenance, ProvenanceKind::Defaulted);
        let spans = resolve_cut_segment_inherited(&p, &ceiling(&p), 1).expect("inherited");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].provenance, ProvenanceKind::Authored);
        assert_eq!((spans[0].from_t, spans[0].to_t), (0.0, 1.0));
        assert_eq!(spans[0].extent, Some(VerticalExtent { bottom: 2700.0, top: 2700.0 }));
        assert_eq!(spans[0].tilt_deg, Some(0.0));
        assert_eq!(polyline_height_conflict(&p, &ceiling(&p)).expect("conflict"), None);
    }

    #[test]
    fn disagreeing_segments_or_no_reference_leave_the_default_alone() {
        let flat_3000 = [(0.0, 3000.0), (7000.0, 3000.0)];
        let p = project(FLAT_2700, flat_3000, FLAT_2700, vec![link(1, 0, 0.375, Id(11), 0.5), link(2, 2, 0.625, Id(12), 0.5)]);
        assert_eq!(resolve_cut_segment_inherited(&p, &ceiling(&p), 1).expect("inherited")[0].provenance, ProvenanceKind::Defaulted);
        let (at, note) = polyline_height_conflict(&p, &ceiling(&p)).expect("conflict").expect("reported");
        assert_eq!((at.x, at.y), (3000.0, 5000.0), "the second segment's crossing");
        assert_eq!(note, "This polyline's linked lines disagree on its height.");

        let p = project(FLAT_2700, flat_3000, FLAT_2700, vec![]);
        assert_eq!(resolve_cut_segment_inherited(&p, &ceiling(&p), 1).expect("inherited")[0].provenance, ProvenanceKind::Defaulted);
        assert_eq!(polyline_height_conflict(&p, &ceiling(&p)).expect("conflict"), None);
    }

    /// Section B's flat line at `z` crosses segment 1 at (8000, 2000); the diagonal's plane is 2800
    /// at y = 2000.
    fn ceiling_with_perpendicular_flat(z: f64) -> Project {
        let flat = [(500.0, z), (9500.0, z)];
        project(
            DIAGONAL,
            FLAT_2700,
            flat,
            vec![link(1, 0, 0.375, Id(11), 0.0), link(2, 2, 0.625, Id(11), 1.0), link(3, 1, 0.4, Id(21), 8500.0 / 9000.0)],
        )
    }

    #[test]
    fn a_flat_line_on_the_diagonals_plane_agrees_and_one_off_it_conflicts() {
        let p = ceiling_with_perpendicular_flat(2800.0);
        assert_eq!(polyline_height_conflict(&p, &ceiling(&p)).expect("conflict"), None);

        let p = ceiling_with_perpendicular_flat(3000.0);
        let (at, _) = polyline_height_conflict(&p, &ceiling(&p)).expect("conflict").expect("reported");
        assert_eq!((at.x, at.y), (8000.0, 2000.0));
    }
}

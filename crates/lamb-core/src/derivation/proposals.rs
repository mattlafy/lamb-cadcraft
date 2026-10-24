//! The AI-line reconcile sweep and its inverse (TS `proposals.ts`): plumb lines proposed in
//! placed sections where a cut-line pline crosses them, from the height real linked lines imply.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::linking::find_auto_links;
use super::projection::{Uz, plan_local_to_global_xy, project_to_u, vertical_basis, vertical_local_to_uz, vertical_uz_to_local};
use super::read::{EntityRef, ProjectRead, ViewKind, ViewRef, entity_ref};
use super::resolve::VerticalExtent;
use super::write::{ProjectWrite, RemovedEntity};
use crate::geometry::Point2;
use crate::geometry::intersect::intersect_segment_with_infinite_line;
use crate::geometry::polyline::{PolySegment, Polyline2, ResolvedSegment, resolved_segments};
use crate::geometry::tolerance::TOLERANCE;
use crate::model::{Actor, AiOrigin, AiProducer, DrawingEntity, EntityGeometry, Id, LineRole, LinkRecord};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedEntity {
    pub view_id: Id,
    pub entity: DrawingEntity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatedEntity {
    pub view_id: Id,
    pub entity_id: Id,
    /// Always the `Polyline` variant.
    pub previous_geometry: EntityGeometry,
}

/// What one sweep did, enough for [`invert_reconcile_ai_lines`] to undo it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileResult {
    pub created_entities: Vec<CreatedEntity>,
    pub removed_entities: Vec<RemovedEntity>,
    pub updated_entities: Vec<UpdatedEntity>,
    /// Links to drop on undo: every link the sweep added.
    pub created_links: Vec<LinkRecord>,
    /// Links to restore on undo: every link the sweep removed.
    pub removed_links: Vec<LinkRecord>,
    /// Unaccepted AI lines flipped to accepted because they were moved off their swept slot (T51).
    pub accepted_by_divergence: Vec<Id>,
    /// Suppressed-slot keys dropped because their source pline implies no height any more (T51).
    pub pruned_suppressions: Vec<String>,
}

/// The deterministic slot for one (source segment, section) crossing; also the created entity's
/// `aiOrigin.proposalId`.
pub fn ai_proposal_slot_key(source: Id, segment_index: usize, view: Id) -> String {
    format!("{source}:{segment_index}:{view}")
}

/// Rewrites the id parts of a slot key — an AI key `"{source}:{segment_index}:{view}"` or a solid
/// cut key `"solidcut:{id,id,…}:{view}"` — through `f`, for the migration points where ids change
/// (a `.lamb` load, an import into a drawing; T355). The segment index is never mapped; a solid
/// cut's list is re-sorted by the new strings, as [`super::solid_cut_lines::solid_cut_slot_key`]
/// sorts by display. A token `f` returns `None` for is kept; a key of neither shape is unchanged.
pub fn remap_slot_key(key: &str, f: impl Fn(&str) -> Option<String>) -> String {
    let map = |s: &str| f(s).unwrap_or_else(|| s.to_owned());
    if let Some(rest) = key.strip_prefix(super::markers::SOLID_CUT_PROPOSAL_PREFIX) {
        let Some((list, view)) = rest.rsplit_once(':') else { return key.to_owned() };
        let mut ids: Vec<String> = list.split(',').map(map).collect();
        ids.sort();
        return format!("{}{}:{}", super::markers::SOLID_CUT_PROPOSAL_PREFIX, ids.join(","), map(view));
    }
    let parts: Vec<&str> = key.split(':').collect();
    match parts.as_slice() {
        [source, index, view] if index.parse::<usize>().is_ok() => format!("{}:{index}:{}", map(source), map(view)),
        _ => key.to_owned(),
    }
}

fn extents_agree(a: &VerticalExtent, b: &VerticalExtent) -> bool {
    (a.top - b.top).abs() <= TOLERANCE.height_agreement_mm && (a.bottom - b.bottom).abs() <= TOLERANCE.height_agreement_mm
}

fn is_pending_ai(e: &EntityRef<'_>) -> bool {
    e.ai_origin.is_some_and(|o| !o.accepted)
}

/// The one extent every entry agrees on, if any.
fn agreed(extents: &[VerticalExtent]) -> Option<VerticalExtent> {
    let first = *extents.first()?;
    extents.iter().all(|e| extents_agree(e, &first)).then_some(first)
}

/// The extent a segment gets from REAL (not pending-AI) linked vertical lines, if they agree.
fn real_extent_for_segment(doc: &impl ProjectRead, entity: Id, segment_index: usize) -> Option<VerticalExtent> {
    let mut extents = Vec::new();
    for l in doc.links_for_segment(entity, segment_index) {
        let Some(other) = doc.entity(l.other.entity_id) else { continue };
        let Some(poly) = other.polyline.as_deref() else { continue };
        if is_pending_ai(&other) {
            continue;
        }
        let Some(other_view) = doc.entity_view_id(l.other.entity_id).and_then(|v| doc.view(v)) else { continue };
        if other_view.kind != ViewKind::Vertical || other_view.placement.is_none() {
            continue;
        }
        let Some(seg) = resolved_segments(poly).into_iter().nth(l.other.segment_index) else { continue };
        let z1 = vertical_local_to_uz(&other_view, seg.from).z;
        let z2 = vertical_local_to_uz(&other_view, seg.to).z;
        extents.push(VerticalExtent { top: z1.max(z2), bottom: z1.min(z2) });
    }
    agreed(&extents)
}

/// One implied extent per polyline entity: every segment's real extent, if any exist and agree.
pub fn implied_extent_for_entity(doc: &impl ProjectRead, entity: &EntityRef<'_>) -> Result<Option<VerticalExtent>, String> {
    let Some(poly) = entity.polyline.as_deref() else { return Ok(None) };
    let extents: Vec<VerticalExtent> = resolved_segments(poly).iter().filter_map(|seg| real_extent_for_segment(doc, entity.id, seg.index)).collect();
    Ok(agreed(&extents))
}

/// Does (entity, segment) already carry a real (not pending-AI) link into `view`?
fn has_real_link_in_view(doc: &impl ProjectRead, entity: Id, segment_index: usize, view: Id) -> bool {
    doc.links_for_segment(entity, segment_index).iter().any(|l| {
        if doc.entity_view_id(l.other.entity_id) != Some(view) {
            return false;
        }
        doc.entity(l.other.entity_id).is_some_and(|o| !is_pending_ai(&o))
    })
}

/// Where a plan segment crosses a vertical view's infinite cut line, in that view's u.
fn crossing_u(doc: &impl ProjectRead, plan_view: &ViewRef<'_>, seg: &ResolvedSegment, vert_view: &ViewRef<'_>) -> Result<Option<f64>, String> {
    let host_id = vert_view.host_view_id().ok_or_else(|| format!("Vertical view {} has no vertical placement", vert_view.id))?;
    let Some(host_view) = doc.view(host_id) else { return Ok(None) };
    let basis = vertical_basis(vert_view, &host_view)?;
    let cut_from = basis.origin_xy;
    let cut_to = Point2 { x: basis.origin_xy.x + basis.c_hat.x, y: basis.origin_xy.y + basis.c_hat.y };
    let global = ResolvedSegment {
        index: seg.index,
        from: plan_local_to_global_xy(plan_view, seg.from)?,
        to: plan_local_to_global_xy(plan_view, seg.to)?,
        bulge: seg.bulge,
    };
    let hits = intersect_segment_with_infinite_line(&global, cut_from, cut_to, 1e-6);
    Ok(hits.first().map(|h| project_to_u(&basis, h.point)))
}

/// A plumb 2-point line spanning `extent` at `u`, in `view`'s local space.
fn ai_line_geometry(view: &ViewRef<'_>, u: f64, extent: VerticalExtent) -> Polyline2 {
    let bottom = vertical_uz_to_local(view, Uz { u, z: extent.bottom });
    let top = vertical_uz_to_local(view, Uz { u, z: extent.top });
    Polyline2 { start: bottom, segments: vec![PolySegment { to: top, bulge: 0.0, corner_at_to: None }], closed: false }
}

fn geometry_equals(a: &Polyline2, b: &Polyline2) -> bool {
    let eps = TOLERANCE.coincidence_mm;
    let close = |p: Point2, q: Point2| (p.x - q.x).abs() <= eps && (p.y - q.y).abs() <= eps;
    close(a.start, b.start) && a.segments.len() == b.segments.len() && a.segments.iter().zip(&b.segments).all(|(s, t)| close(s.to, t.to))
}

struct DesiredProposal {
    source_entity_id: Id,
    source_layer_id: String,
    view_id: Id,
    extent: VerticalExtent,
    u: f64,
}

/// A JS `Map<String, V>`: first-insertion order, last value wins.
struct OrderedMap<V> {
    entries: Vec<(String, V)>,
    index: HashMap<String, usize>,
}

impl<V> OrderedMap<V> {
    fn new() -> Self {
        OrderedMap { entries: Vec::new(), index: HashMap::new() }
    }

    fn set(&mut self, key: String, value: V) {
        match self.index.get(&key) {
            Some(&i) => {
                if let Some(slot) = self.entries.get_mut(i) {
                    slot.1 = value;
                }
            }
            None => {
                self.index.insert(key.clone(), self.entries.len());
                self.entries.push((key, value));
            }
        }
    }

    fn contains(&self, key: &str) -> bool {
        self.index.contains_key(key)
    }
}

/// JS `key.slice(0, key.indexOf(":"))` (no colon drops the last character, as JS does).
fn source_id_prefix(key: &str) -> &str {
    match key.find(':') {
        Some(i) => &key[..i],
        None => key.char_indices().last().map_or("", |(i, _)| &key[..i]),
    }
}

/// The reconcile sweep: diff what is implied now against the unaccepted AI lines that exist —
/// create the missing, re-place the moved, delete the no-longer-implied. Never touches accepted AI
/// lines, assistant-produced ones, or user geometry.
pub fn reconcile_ai_lines(doc: &mut impl ProjectWrite, now: &str, mint: &mut impl FnMut() -> Id) -> Result<ReconcileResult, String> {
    let mut result = ReconcileResult::default();
    let view_ids: Vec<(Id, ViewKind, bool)> = doc.views().map(|v| (v.id, v.kind, v.placement.is_some())).collect();

    // Phase 0: an unaccepted AI line moved off the slot the sweep last put it in was edited
    // directly — take it as accepted before anything below reads its status.
    let mut diverged: Vec<(Id, AiOrigin)> = Vec::new();
    for &(view, ..) in &view_ids {
        for e in doc.entities(view) {
            let Some(origin) = e.ai_origin else { continue };
            if origin.accepted || origin.producer == Some(AiProducer::Assistant) {
                continue;
            }
            let (Some(poly), Some(swept)) = (e.polyline.as_deref(), origin.swept_geometry.as_ref()) else { continue };
            if !geometry_equals(poly, swept) {
                diverged.push((e.id, AiOrigin { accepted: true, ..origin.clone() }));
            }
        }
    }
    for (id, origin) in diverged {
        doc.set_ai_origin(id, Some(origin));
        result.accepted_by_divergence.push(id);
    }

    // proposalId -> (view, entity id) of every unaccepted rule-produced AI line.
    let mut existing: OrderedMap<(Id, Id)> = OrderedMap::new();
    for &(view, ..) in &view_ids {
        for e in doc.entities(view) {
            // Solid-cut lines belong to the sibling sweep (`reconcile_solid_cut_lines`); counting
            // them here removed every one each command and had it re-minted (T513).
            if let Some(o) = e.ai_origin.filter(|o| {
                !o.accepted && o.producer != Some(AiProducer::Assistant) && !o.proposal_id.starts_with(super::markers::SOLID_CUT_PROPOSAL_PREFIX)
            }) {
                existing.set(o.proposal_id.clone(), (view, e.id));
            }
        }
    }

    // Suppressed keys plus keys an accepted / assistant-produced line already holds (T513).
    let suppressed: HashSet<String> = super::solid_cut_lines::filled_slots(&*doc);
    let mut desired: OrderedMap<DesiredProposal> = OrderedMap::new();
    let placed_plans: Vec<Id> = view_ids.iter().filter(|v| v.1 == ViewKind::Plan && v.2).map(|v| v.0).collect();
    let placed_verticals: Vec<Id> = view_ids.iter().filter(|v| v.1 == ViewKind::Vertical && v.2).map(|v| v.0).collect();

    let mut no_longer_implied: HashSet<String> = HashSet::new();
    {
        let rd = &*doc;
        for &plan_id in &placed_plans {
            let Some(plan_view) = rd.view(plan_id) else { continue };
            for entity in rd.entities(plan_id) {
                let Some(poly) = entity.polyline.as_deref() else { continue };
                if is_pending_ai(&entity) || rd.role_of(&entity) != LineRole::Cut {
                    continue;
                }
                let Some(extent) = implied_extent_for_entity(rd, &entity)? else {
                    no_longer_implied.insert(entity.id.to_string());
                    continue;
                };
                for seg in resolved_segments(poly) {
                    if real_extent_for_segment(rd, entity.id, seg.index).is_some() {
                        continue;
                    }
                    for &vert_id in &placed_verticals {
                        let Some(vert_view) = rd.view(vert_id) else { continue };
                        let Some(u) = crossing_u(rd, &plan_view, &seg, &vert_view)? else { continue };
                        let key = ai_proposal_slot_key(entity.id, seg.index, vert_id);
                        if suppressed.contains(&key) || has_real_link_in_view(rd, entity.id, seg.index, vert_id) {
                            continue;
                        }
                        desired.set(
                            key,
                            DesiredProposal { source_entity_id: entity.id, source_layer_id: entity.layer_id.to_owned(), view_id: vert_id, extent, u },
                        );
                    }
                }
            }
        }
    }

    if !no_longer_implied.is_empty() {
        let mut keep = Vec::new();
        for key in doc.suppressed_proposals() {
            if no_longer_implied.contains(source_id_prefix(key)) {
                result.pruned_suppressions.push(key.clone());
            } else {
                keep.push(key.clone());
            }
        }
        if !result.pruned_suppressions.is_empty() {
            doc.set_suppressed_proposals(keep);
        }
    }

    // Unaccepted AI lines no longer implied (or now suppressed/superseded) get removed.
    let mut ids_to_remove: HashSet<Id> = HashSet::new();
    let mut active_existing: HashMap<String, (Id, Id)> = HashMap::new();
    for (proposal_id, val) in &existing.entries {
        if desired.contains(proposal_id) {
            active_existing.insert(proposal_id.clone(), *val);
        } else {
            ids_to_remove.insert(val.1);
        }
    }
    if !ids_to_remove.is_empty() {
        let removed_links = doc.take_links(&mut |l| ids_to_remove.contains(&l.a.entity_id) || ids_to_remove.contains(&l.b.entity_id));
        result.removed_links.extend(removed_links);
        let removed = doc.remove_entities(&ids_to_remove);
        result.removed_entities.extend(removed);
    }

    // Update existing ones whose geometry moved; create the rest.
    for (proposal_id, want) in &desired.entries {
        let want_geometry = {
            let view = doc.view(want.view_id).ok_or_else(|| format!("reconcile_ai_lines: no view {}", want.view_id))?;
            ai_line_geometry(&view, want.u, want.extent)
        };
        let created_by = Actor::Llm { proposal_id: proposal_id.clone() };

        if let Some(&(view_id, entity_id)) = active_existing.get(proposal_id) {
            let (previous, origin) = {
                let entity = doc.entity(entity_id).ok_or_else(|| format!("reconcile_ai_lines: AI line {entity_id} vanished"))?;
                let poly = entity.polyline.as_deref().ok_or_else(|| format!("reconcile_ai_lines: AI line {entity_id} is not a polyline"))?;
                if geometry_equals(poly, &want_geometry) {
                    continue;
                }
                (poly.clone(), entity.ai_origin.cloned())
            };
            let stale = doc.take_links(&mut |l| l.a.entity_id == entity_id || l.b.entity_id == entity_id);
            result.removed_links.extend(stale);

            doc.set_polyline(entity_id, want_geometry.clone());
            if let Some(origin) = origin {
                doc.set_ai_origin(entity_id, Some(AiOrigin { swept_geometry: Some(want_geometry.clone()), ..origin }));
            }
            result.updated_entities.push(UpdatedEntity { view_id, entity_id, previous_geometry: EntityGeometry::Polyline { geometry: previous } });

            let new_links = {
                let entity = doc.entity(entity_id).ok_or_else(|| format!("reconcile_ai_lines: AI line {entity_id} vanished"))?;
                find_auto_links(&*doc, &entity, &created_by, now, mint)?
            };
            for link in &new_links {
                doc.push_link(link.clone());
            }
            result.created_links.extend(new_links);
            continue;
        }

        let entity = DrawingEntity {
            id: mint(),
            view_id: want.view_id,
            role: LineRole::Cut,
            layer_id: want.source_layer_id.clone(),
            geometry: EntityGeometry::Polyline { geometry: want_geometry.clone() },
            color_override: None,
            lineweight_override: None,
            ai_origin: Some(AiOrigin {
                proposal_id: proposal_id.clone(),
                accepted: false,
                assumption_note: None,
                source_entity_id: Some(want.source_entity_id),
                swept_geometry: Some(want_geometry),
                producer: None,
            }),
        };
        doc.push_entity(entity.clone())?;
        let new_links = find_auto_links(&*doc, &entity_ref(&entity), &created_by, now, mint)?;
        result.created_entities.push(CreatedEntity { view_id: want.view_id, entity });
        for link in &new_links {
            doc.push_link(link.clone());
        }
        result.created_links.extend(new_links);
    }

    Ok(result)
}

/// Reverses everything [`reconcile_ai_lines`] did — the sweep half of an undo entry's inverse.
pub fn invert_reconcile_ai_lines(doc: &mut impl ProjectWrite, result: &ReconcileResult) {
    let created_link_ids: HashSet<Id> = result.created_links.iter().map(|l| l.id).collect();
    doc.take_links(&mut |l| created_link_ids.contains(&l.id));
    for link in &result.removed_links {
        doc.push_link(link.clone());
    }

    let created_entity_ids: HashSet<Id> = result.created_entities.iter().map(|c| c.entity.id).collect();
    doc.remove_entities(&created_entity_ids);

    // Per view in first-appearance order, ascending index (a stable sort, like JS's).
    let mut by_view: Vec<(Id, Vec<(usize, &DrawingEntity)>)> = Vec::new();
    for r in &result.removed_entities {
        match by_view.iter_mut().find(|(v, _)| *v == r.view_id) {
            Some((_, list)) => list.push((r.index, &r.entity)),
            None => by_view.push((r.view_id, vec![(r.index, &r.entity)])),
        }
    }
    for (view_id, mut list) in by_view {
        if doc.view(view_id).is_none() {
            continue;
        }
        list.sort_by_key(|(index, _)| *index);
        for (index, entity) in list {
            doc.insert_entity(view_id, index, entity.clone());
        }
    }

    for upd in &result.updated_entities {
        let EntityGeometry::Polyline { geometry } = &upd.previous_geometry else { continue };
        if doc.entities(upd.view_id).any(|e| e.id == upd.entity_id) {
            doc.set_polyline(upd.entity_id, geometry.clone());
        }
    }

    for &id in &result.accepted_by_divergence {
        let origin = doc.entity(id).and_then(|e| e.ai_origin.cloned());
        if let Some(origin) = origin {
            doc.set_ai_origin(id, Some(AiOrigin { accepted: false, ..origin }));
        }
    }

    if !result.pruned_suppressions.is_empty() {
        let mut keys = doc.suppressed_proposals().to_vec();
        keys.extend(result.pruned_suppressions.iter().cloned());
        doc.set_suppressed_proposals(keys);
    }
}

#[cfg(test)]
mod remap_slot_key_tests {
    use super::remap_slot_key;

    fn table(s: &str) -> Option<String> {
        match s {
            "wall-a" => Some("7".to_owned()),
            "sec-1" => Some("12".to_owned()),
            "b" => Some("100".to_owned()),
            "a" => Some("9".to_owned()),
            "3" => Some("999".to_owned()),
            _ => None,
        }
    }

    #[test]
    fn ai_key_maps_source_and_view_but_never_the_segment_index() {
        assert_eq!(remap_slot_key("wall-a:3:sec-1", table), "7:3:12");
    }

    #[test]
    fn solid_cut_list_is_mapped_and_resorted_by_the_new_strings() {
        // "a" < "b" before; "100" < "9" after, lexicographically, as display sorting is.
        assert_eq!(remap_slot_key("solidcut:a,b:sec-1", table), "solidcut:100,9:12");
    }

    #[test]
    fn unknown_tokens_and_foreign_shapes_are_kept() {
        assert_eq!(remap_slot_key("ghost:0:sec-1", table), "ghost:0:12");
        assert_eq!(remap_slot_key("not a key", table), "not a key");
        assert_eq!(remap_slot_key("a:b:c:d", table), "a:b:c:d");
    }
}

#[cfg(test)]
mod filled_slot_tests {
    //! T513: a slot filled once stays filled — a suppressed key, or one an accepted line holds,
    //! is never (re-)proposed by this sweep.
    use std::collections::BTreeMap;

    use super::*;
    use crate::derivation::write::ProjectWrite;
    use crate::model::Project;

    fn fixture() -> Project {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/lamb/u-wall-two-sections.lamb.json");
        let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json");
        crate::persistence::files_to_project(&files).expect("project")
    }

    fn sweep(p: &mut Project) -> ReconcileResult {
        let mut n = 1u64 << 40;
        let mut mint = || {
            n += 1;
            Id(n)
        };
        reconcile_ai_lines(p, "2026-01-01T00:00:00.000Z", &mut mint).expect("sweep")
    }

    /// The fixture swept to rest, and one pending line this sweep owns: (id, view, origin).
    fn settled() -> (Project, DrawingEntity) {
        let mut p = fixture();
        sweep(&mut p);
        let e = p
            .views
            .iter()
            .flat_map(|v| v.entities().iter())
            .find(|e| {
                e.ai_origin.as_ref().is_some_and(|o| !o.accepted && !o.proposal_id.starts_with(crate::derivation::markers::SOLID_CUT_PROPOSAL_PREFIX))
            })
            .cloned()
            .expect("a pending line of this sweep");
        (p, e)
    }

    fn holders(p: &Project, key: &str) -> Vec<(Id, bool)> {
        p.views
            .iter()
            .flat_map(|v| v.entities().iter())
            .filter_map(|e| e.ai_origin.as_ref().filter(|o| o.proposal_id == key).map(|o| (e.id, o.accepted)))
            .collect()
    }

    #[test]
    fn a_suppressed_key_is_removed_and_not_reproposed() {
        let (mut p, e) = settled();
        let key = e.ai_origin.as_ref().map(|o| o.proposal_id.clone()).unwrap_or_default();
        p.set_suppressed_proposals(vec![key.clone()]);
        let r = sweep(&mut p);
        assert!(r.created_entities.is_empty(), "{r:?}");
        assert!(holders(&p, &key).is_empty());
    }

    #[test]
    fn a_key_an_accepted_line_holds_is_not_reproposed() {
        let (mut p, e) = settled();
        let key = e.ai_origin.as_ref().map(|o| o.proposal_id.clone()).unwrap_or_default();
        let accepted = DrawingEntity { id: Id(1 << 50), ai_origin: e.ai_origin.clone().map(|o| AiOrigin { accepted: true, ..o }), ..e.clone() };
        p.push_entity(accepted).expect("push");
        let r = sweep(&mut p);
        assert!(r.created_entities.is_empty(), "{r:?}");
        assert_eq!(holders(&p, &key), vec![(Id(1 << 50), true)], "only the accepted holder is left on the slot");
        let r = sweep(&mut p);
        assert!(r.created_entities.is_empty() && r.removed_entities.is_empty(), "and it stays settled: {r:?}");
    }
}

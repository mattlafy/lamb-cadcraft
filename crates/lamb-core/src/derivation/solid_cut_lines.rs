//! The solid-cut reconcile sweep (TS `solidCutLines.ts`): materialises every placed view's
//! cross-section of every synthesized solid as an unaccepted AI polyline, keyed per (solid, view).
//! The sibling of [`super::proposals::reconcile_ai_lines`], returning the same [`ReconcileResult`]
//! so [`super::proposals::invert_reconcile_ai_lines`] undoes it unchanged.

use std::collections::{HashMap, HashSet};

use super::markers::{SOLID_CUT_PROPOSAL_PREFIX, compute_solid_cuts_for_view};
use super::proposals::{CreatedEntity, ReconcileResult, UpdatedEntity};
use super::read::ViewKind;
use super::resolve::ProvenanceKind;
use super::synthesis::synthesize;
use super::write::ProjectWrite;
use crate::geometry::Point2;
use crate::geometry::polyline::{PolySegment, Polyline2};
use crate::geometry::tolerance::TOLERANCE;
use crate::model::{AiOrigin, DrawingEntity, EntityGeometry, Id, LineRole};

/// `ids` sorted by their `Display` strings (TS `[...ids].sort()`), never by the `u64`.
fn sorted_by_display(ids: &[Id]) -> Vec<Id> {
    let mut keyed: Vec<(String, Id)> = ids.iter().map(|id| (id.to_string(), *id)).collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    keyed.into_iter().map(|(_, id)| id).collect()
}

/// The deterministic slot for one (solid, view) cross-section — the `aiOrigin.proposalId` a
/// materialised solid-cut entity carries. Keyed on the solid's sorted source ids, not a positional
/// index, so it survives other solids appearing or disappearing (T99).
pub fn solid_cut_slot_key(source_entity_ids: &[Id], view: Id) -> String {
    let joined = sorted_by_display(source_entity_ids).iter().map(Id::to_string).collect::<Vec<_>>().join(",");
    format!("{SOLID_CUT_PROPOSAL_PREFIX}{joined}:{view}")
}

/// A closed loop (first vertex not repeated) as the straight-segment `Polyline2` an entity stores.
fn loop_geometry(loop_: &[Point2]) -> Result<Polyline2, String> {
    let (&first, rest) = loop_.split_first().ok_or("reconcile_solid_cut_lines: empty solid-cut loop")?;
    let segments = rest.iter().chain(std::iter::once(&first)).map(|&to| PolySegment { to, bulge: 0.0, corner_at_to: None }).collect();
    Ok(Polyline2 { start: first, segments, closed: true })
}

/// Vertex-wise equality within `coincidence_mm`, `closed` included.
fn loop_geometry_equals(a: &Polyline2, b: &Polyline2) -> bool {
    let eps = TOLERANCE.coincidence_mm;
    let close = |p: Point2, q: Point2| (p.x - q.x).abs() <= eps && (p.y - q.y).abs() <= eps;
    a.closed == b.closed
        && close(a.start, b.start)
        && a.segments.len() == b.segments.len()
        && a.segments.iter().zip(&b.segments).all(|(s, t)| close(s.to, t.to))
}

/// A provenance's serialized name (`"not_3d"` etc.) — what `assumptionNote` stores.
fn provenance_name(kind: ProvenanceKind) -> Result<String, String> {
    match serde_json::to_value(kind) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        other => Err(format!("provenance did not serialize to a string: {other:?}")),
    }
}

fn is_pending_solid_cut(origin: &AiOrigin) -> bool {
    !origin.accepted && origin.proposal_id.starts_with(SOLID_CUT_PROPOSAL_PREFIX)
}

struct DesiredSolidCut {
    view_id: Id,
    layer_id: String,
    source_entity_ids: Vec<Id>,
    loop_: Vec<Point2>,
    provenance: ProvenanceKind,
}

/// Diff what `compute_solid_cuts_for_view` implies across every placed plan and vertical view
/// against the unaccepted solid-cut entities that exist: create the missing, update the moved,
/// delete the no-longer-implied. Never touches accepted ones or user geometry, and never auto-links
/// what it creates (a loop's edges coincide with its source solid's boundary, which would feed a
/// spurious link back into synthesis — T99).
pub fn reconcile_solid_cut_lines(doc: &mut impl ProjectWrite, mint: &mut impl FnMut() -> Id) -> Result<ReconcileResult, String> {
    let mut result = ReconcileResult::default();
    let views: Vec<(Id, ViewKind, bool)> = doc.views().map(|v| (v.id, v.kind, v.placement.is_some())).collect();

    // Phase 0: an unaccepted solid-cut entity moved off the loop the sweep last put it in was edited
    // directly — take it as accepted before anything below reads its status (T51).
    let mut diverged: Vec<(Id, AiOrigin)> = Vec::new();
    for &(view, ..) in &views {
        for e in doc.entities(view) {
            let Some(origin) = e.ai_origin.filter(|o| is_pending_solid_cut(o)) else { continue };
            let (Some(poly), Some(swept)) = (e.polyline.as_deref(), origin.swept_geometry.as_ref()) else { continue };
            if !loop_geometry_equals(poly, swept) {
                diverged.push((e.id, AiOrigin { accepted: true, ..origin.clone() }));
            }
        }
    }
    for (id, origin) in diverged {
        doc.set_ai_origin(id, Some(origin));
        result.accepted_by_divergence.push(id);
    }

    // proposalId -> (view, entity id); last one wins, as in a JS Map. Only ever looked up or
    // collected into a set, so its iteration order never reaches the output.
    let mut existing: HashMap<String, (Id, Id)> = HashMap::new();
    for &(view, ..) in &views {
        for e in doc.entities(view) {
            if let Some(o) = e.ai_origin.filter(|o| is_pending_solid_cut(o)) {
                existing.insert(o.proposal_id.clone(), (view, e.id));
            }
        }
    }

    // Desired, in first-insertion order with the last value winning (a JS Map).
    let mut desired: Vec<(String, DesiredSolidCut)> = Vec::new();
    let mut desired_index: HashMap<String, usize> = HashMap::new();
    {
        let rd = &*doc;
        let solids = synthesize(rd)?.solids;
        for &(view_id, kind, placed) in &views {
            if !matches!(kind, ViewKind::Plan | ViewKind::Vertical) || !placed {
                continue;
            }
            for cut in compute_solid_cuts_for_view(rd, view_id, Some(&solids))? {
                let key = solid_cut_slot_key(&cut.source_entity_ids, view_id);
                // The first (sorted) source entity's own layer, the one `sourceEntityId` points at.
                let layer_id = sorted_by_display(&cut.source_entity_ids)
                    .first()
                    .and_then(|&id| rd.entity(id))
                    .map_or_else(|| "layer-0".to_owned(), |e| e.layer_id.to_owned());
                let want =
                    DesiredSolidCut { view_id, layer_id, source_entity_ids: cut.source_entity_ids, loop_: cut.loop_, provenance: cut.provenance };
                match desired_index.get(&key) {
                    Some(&i) => {
                        if let Some(slot) = desired.get_mut(i) {
                            slot.1 = want;
                        }
                    }
                    None => {
                        desired_index.insert(key.clone(), desired.len());
                        desired.push((key, want));
                    }
                }
            }
        }
    }

    // Unaccepted solid-cut entities no longer implied get removed.
    let ids_to_remove: HashSet<Id> = existing.iter().filter(|(key, _)| !desired_index.contains_key(*key)).map(|(_, &(_, id))| id).collect();
    if !ids_to_remove.is_empty() {
        let removed_links = doc.take_links(&mut |l| ids_to_remove.contains(&l.a.entity_id) || ids_to_remove.contains(&l.b.entity_id));
        result.removed_links.extend(removed_links);
        let removed = doc.remove_entities(&ids_to_remove);
        result.removed_entities.extend(removed);
    }

    // Update existing ones whose loop moved; create the rest.
    for (key, want) in &desired {
        let want_geometry = loop_geometry(&want.loop_)?;
        let note = provenance_name(want.provenance)?;

        if let Some(&(view_id, entity_id)) = existing.get(key) {
            let (previous, origin) = {
                let entity = doc.entity(entity_id).ok_or_else(|| format!("reconcile_solid_cut_lines: solid-cut entity {entity_id} vanished"))?;
                // TS would record a non-polyline's geometry as `previousGeometry`; EntityRef can't
                // carry one, and the sweep only ever writes polylines.
                let poly =
                    entity.polyline.as_deref().ok_or_else(|| format!("reconcile_solid_cut_lines: solid-cut entity {entity_id} is not a polyline"))?;
                if loop_geometry_equals(poly, &want_geometry) {
                    continue;
                }
                (poly.clone(), entity.ai_origin.cloned())
            };
            let stale = doc.take_links(&mut |l| l.a.entity_id == entity_id || l.b.entity_id == entity_id);
            result.removed_links.extend(stale);

            doc.set_polyline(entity_id, want_geometry.clone());
            if let Some(origin) = origin {
                doc.set_ai_origin(entity_id, Some(AiOrigin { swept_geometry: Some(want_geometry), assumption_note: Some(note), ..origin }));
            }
            result.updated_entities.push(UpdatedEntity { view_id, entity_id, previous_geometry: EntityGeometry::Polyline { geometry: previous } });
            continue;
        }

        let entity = DrawingEntity {
            id: mint(),
            view_id: want.view_id,
            role: LineRole::Cut,
            layer_id: want.layer_id.clone(),
            geometry: EntityGeometry::Polyline { geometry: want_geometry.clone() },
            color_override: None,
            lineweight_override: None,
            ai_origin: Some(AiOrigin {
                proposal_id: key.clone(),
                accepted: false,
                assumption_note: Some(note),
                source_entity_id: sorted_by_display(&want.source_entity_ids).first().copied(),
                swept_geometry: Some(want_geometry),
                producer: None,
            }),
        };
        doc.push_entity(entity.clone())?;
        result.created_entities.push(CreatedEntity { view_id: want.view_id, entity });
    }

    Ok(result)
}

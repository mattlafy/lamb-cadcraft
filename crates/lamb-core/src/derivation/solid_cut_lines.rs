//! The solid-cut reconcile sweep (TS `solidCutLines.ts`): materialises every placed view's
//! cross-section of every synthesized solid as an unaccepted AI polyline, keyed per (solid, view).
//! The sibling of [`super::proposals::reconcile_ai_lines`], returning the same [`ReconcileResult`]
//! so [`super::proposals::invert_reconcile_ai_lines`] undoes it unchanged.

use std::collections::{HashMap, HashSet};

use super::markers::{SOLID_CUT_PROPOSAL_PREFIX, compute_solid_cuts_for_view};
use super::proposals::{CreatedEntity, ReconcileResult, UpdatedEntity};
use super::read::{ProjectRead, ViewKind};
use super::resolve::ProvenanceKind;
use super::synthesis::synthesize;
use super::write::ProjectWrite;
use crate::geometry::Point2;
use crate::geometry::polyline::{PolySegment, Polyline2};
use crate::geometry::tolerance::TOLERANCE;
use crate::model::{AiOrigin, AiProducer, DrawingEntity, EntityGeometry, Id, LineRole};

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

/// The slot for one piece of a (solid, view) cross-section (T542): [`solid_cut_slot_key`] for
/// piece 0, so a solid cut in one piece keeps its key, and `{key}#{piece}` for the rest.
pub fn solid_cut_piece_key(source_entity_ids: &[Id], view: Id, piece: usize) -> String {
    let key = solid_cut_slot_key(source_entity_ids, view);
    if piece == 0 { key } else { format!("{key}#{piece}") }
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

/// Every slot key that must not be (re-)proposed: the document's suppressed keys, plus every
/// `proposal_id` held by an accepted or assistant-produced entity in any view (T513). Shared by
/// both reconcile sweeps.
pub fn filled_slots(doc: &impl ProjectRead) -> HashSet<String> {
    let mut out: HashSet<String> = doc.suppressed_proposals().iter().cloned().collect();
    for v in doc.views() {
        for e in doc.entities(v.id) {
            if let Some(o) = e.ai_origin
                && (o.accepted || o.producer == Some(AiProducer::Assistant))
            {
                out.insert(o.proposal_id.clone());
            }
        }
    }
    out
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
///
/// TS-faithful (the golden cases replay it): a slot whose line was accepted or erased is
/// re-proposed. The fork's after-command sweep runs [`reconcile_solid_cut_lines_once`] instead.
pub fn reconcile_solid_cut_lines(doc: &mut impl ProjectWrite, mint: &mut impl FnMut() -> Id) -> Result<ReconcileResult, String> {
    reconcile(doc, mint, false)
}

/// [`reconcile_solid_cut_lines`] where a slot filled once stays filled (T513): a key in
/// [`filled_slots`] — suppressed by an erase, or held by an accepted / assistant-produced line —
/// is never (re-)proposed, and a pending line on it is removed. Diverges from TS on purpose (user
/// feedback 2026-10-24).
pub fn reconcile_solid_cut_lines_once(doc: &mut impl ProjectWrite, mint: &mut impl FnMut() -> Id) -> Result<ReconcileResult, String> {
    reconcile(doc, mint, true)
}

fn reconcile(doc: &mut impl ProjectWrite, mint: &mut impl FnMut() -> Id, keep_filled: bool) -> Result<ReconcileResult, String> {
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

    // T513 (`keep_filled` only): a key the user erased (suppressed) or that an accepted or
    // assistant-produced entity already holds is never re-proposed.
    let filled = if keep_filled { filled_slots(&*doc) } else { HashSet::new() };

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
                let key = solid_cut_piece_key(&cut.source_entity_ids, view_id, cut.piece);
                if filled.contains(&key) {
                    continue;
                }
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

#[cfg(test)]
mod filled_slot_tests {
    //! T513: a slot filled once stays filled — a suppressed key, or one an accepted line holds,
    //! is never (re-)proposed by this sweep.
    use super::*;
    use crate::derivation::write::ProjectWrite;
    use crate::model::Project;

    /// The golden "materialises a view's derived solid cut" project: one box solid, one placed
    /// section across it.
    fn fixture() -> Project {
        fn decode(v: &serde_json::Value) -> serde_json::Value {
            use serde_json::Value;
            match v {
                Value::Array(a) => Value::Array(a.iter().map(decode).collect()),
                Value::Object(o) => {
                    if o.get("$undefined") == Some(&Value::Bool(true)) {
                        return Value::Null;
                    }
                    if let Some(Value::Array(items)) = o.get("$map").or_else(|| o.get("$set")) {
                        return Value::Array(items.iter().map(decode).collect());
                    }
                    Value::Object(o.iter().map(|(k, v)| (k.clone(), decode(v))).collect())
                }
                _ => v.clone(),
            }
        }
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/derivation-engine/solidCutLines.json");
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json");
        let case = doc["cases"]
            .as_array()
            .and_then(|c| {
                c.iter().find(|c| c["export"] == "reconcileSolidCutLines" && c["test"].as_str().is_some_and(|t| t.contains("materialises")))
            })
            .expect("case")
            .clone();
        crate::model::with_string_ids(|| serde_json::from_value(decode(&case["args"][0])).expect("project"))
    }

    fn sweep(p: &mut Project) -> ReconcileResult {
        let mut n = 1u64 << 40;
        let mut mint = || {
            n += 1;
            Id(n)
        };
        reconcile_solid_cut_lines_once(p, &mut mint).expect("sweep")
    }

    /// The fixture swept to rest, and one pending line this sweep owns: (id, view, origin).
    fn settled() -> (Project, DrawingEntity) {
        let mut p = fixture();
        sweep(&mut p);
        let e = p
            .views
            .iter()
            .flat_map(|v| v.entities().iter())
            .find(|e| e.ai_origin.as_ref().is_some_and(|o| !o.accepted && o.proposal_id.starts_with(SOLID_CUT_PROPOSAL_PREFIX)))
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

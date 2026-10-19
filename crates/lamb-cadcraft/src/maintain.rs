//! The after-command observer that keeps `LambDocument.links` true (`cadcraft-integration.md` §7
//! "link formation runs in the after-command hook", §13 phase 3 "TRIM a linked line → links
//! maintained or stale").
//!
//! [`maintain_links`] compares the drawing before and after one command and, in order:
//! 0. travels: a linked segment the command rigidly translated carries its unchanged partner in the
//!    other view with it — only for a link in the selection's **travel set** (TS T109/T110: an idle
//!    crossing box seeds every link touching a swept entity, a window box seeds none, the set dies
//!    with the selection; [`travel_box_observer`] seeds it, engine patch 19 holds it). Any command
//!    whose rigid translation this step detects travels (MOVE, STRETCH, grips) — a superset of TS's
//!    MOVE, kept because §13 phase 3's gate is STRETCH. A touched link outside the set is left to
//!    step 2, which flags it stale rather than deleting it as TS does on separation (visible and
//!    reversible; RESOLVE lists it) (`selection-and-link-editing.md` §4b ray-sliding, lamb-core
//!    `travel_segment_move`), so the link stays live — §13 phase 3 "STRETCH a linked plan line → the
//!    section follows in one undo step"; an unaccepted AI line never travels — the after-command
//!    sweep ([`crate::sweeps`]) re-seats it on its slot instead;
//! 0b. re-anchors: a link end whose entity the command replaced (TRIM, BREAK erase the original
//!    handle and add a new one per piece) moves onto the new piece in the same view whose segment
//!    is a sub-span of the old one and still holds the anchor point — same link id, origin and
//!    creation stamp (§13 phase 3 "TRIM a linked line → links maintained"). No such piece: the
//!    anchor was cut away and step 1 drops the link (TS `break.ts` `remapAnchor` parity);
//! 0c. erases travelling partners: an entity the command erased outright (its view gained no
//!    entity, so not a TRIM/BREAK cut-away) takes the partner of every travel-set link on it with it
//!    — one hop, no cascade, unaccepted AI partners included (TS T109 `erase.ts` parity);
//! 0d. remaps vertex edits: a polyline that gained, lost or reversed vertices in place (grips' Add/
//!    Remove Vertex, `pedit` insert / straighten / reverse — same handle, so 0b never runs) keeps
//!    each anchor on its segment; one exactly at a removed vertex drops its link (TS `vertex.ts`
//!    parity, [`crate::revertex`]);
//! 1. drops every link with an anchor whose entity is no longer in any view block (ERASE);
//! 2. re-checks every link with an anchor on a touched entity (new, or a different polyline) or in
//!    a touched view (its document entry, or its host's, changed) with lamb-core's
//!    `link_still_coincides`: a broken crossing is flagged `stale: Some(true)`, a restored one is
//!    cleared back to `None` — a moved line is flagged, never dropped, because a flag is visible and
//!    reversible (`link-maintenance.md`: "visible flags beat silent resolution");
//!    Then, 2b, stamps each entity new to a view block whose stored role is `Bylayer` and whose layer's
//!    default is `Cut` with a concrete role: `Cut` in a plan, `Seen` in a vertical or detail view
//!    (TS T256 `createEntity.ts` `pushEntity`), so later layer-role edits leave it alone;
//! 3. auto-links each entity new to a view block through its view's hollow markers, skipping a
//!    pair of segments some link already joins (a re-anchored piece crosses the marker its own link
//!    fills) and any entity whose resolved role is `Reference` (TS T113);
//! 4. relayers: an entity whose layer the command changed carries its automatic-link component to
//!    the new layer, stopping at explicit links (TS SETLAYER, [`crate::relayer`]).
//!
//! [`install`] registers [`link_observer`] on a `Session`, so all of this lands in the command's own
//! undo step. Out of scope: travel or erase cascading past the first partner, Tier 2 declarations.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use cadcraft_doc::{Drawing, EntityKind, Handle};
use cadcraft_engine::Session;
use lamb_core::derivation::linking::{find_auto_links, link_still_coincides};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::derivation::read::ViewKind;
use lamb_core::derivation::travel::travel_segment_move;
use lamb_core::geometry::polyline::{ResolvedSegment, closest_point_on_segment, point_at_param, resolved_segments, virtual_corner_entries};
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::geometry::{Point2, Polyline2};
use lamb_core::model::{Actor, AnyView, Id, LineRole, LinkAnchor, LinkRecord, resolve_entity_role};

use crate::convert::from_polyline;

use crate::project::DrawingProject;
use crate::schema::{SchemaError, read_document, read_entity_data, set_entity_data, write_document};

/// What [`maintain_links`] changed, as link ids.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaintenanceReport {
    /// Links removed because an anchored entity was erased.
    pub dropped: Vec<Id>,
    /// Links newly flagged stale (the crossing no longer holds).
    pub staled: Vec<Id>,
    /// Stale links whose crossing holds again, flag cleared.
    pub cleared: Vec<Id>,
    /// Automatic links formed by newly drawn entities.
    pub created: Vec<Id>,
    /// Partner entities (not links) slid to follow a rigidly moved linked segment.
    pub travelled: Vec<Id>,
    /// Links moved onto a new piece of an entity the command replaced (TRIM, BREAK).
    pub reanchored: Vec<Id>,
    /// Partner entities (not links) erased with an erased entity across a travel-set link.
    pub erased: Vec<Id>,
    /// New entities (not links) given a concrete `Cut`/`Seen` role from a cut-default layer.
    pub stamped: Vec<Id>,
    /// Entities (not links) moved to the new layer of a relayered entity in their automatic-link component.
    pub relayered: Vec<Id>,
}

impl MaintenanceReport {
    pub fn is_empty(&self) -> bool {
        self.dropped.is_empty()
            && self.staled.is_empty()
            && self.cleared.is_empty()
            && self.created.is_empty()
            && self.travelled.is_empty()
            && self.reanchored.is_empty()
            && self.erased.is_empty()
            && self.stamped.is_empty()
            && self.relayered.is_empty()
    }
}

fn derivation_error(message: String) -> SchemaError {
    SchemaError::Malformed { what: "link maintenance".into(), message }
}

/// Entity id → (view id, polyline) over every view block.
pub(crate) type EntityMap = BTreeMap<Id, (Id, Option<Polyline2>)>;

pub(crate) fn entity_map(p: &DrawingProject<'_>) -> EntityMap {
    let mut out = BTreeMap::new();
    for v in p.views() {
        for e in p.entities(v.id) {
            out.insert(e.id, (v.id, e.polyline.map(std::borrow::Cow::into_owned)));
        }
    }
    out
}

fn host_of(v: &AnyView) -> Option<Id> {
    lamb_core::derivation::read::view_ref(v).host_view_id()
}

/// Views whose document entry differs between the two documents, plus views whose host does.
fn touched_views(before: &[AnyView], after: &[AnyView]) -> BTreeSet<Id> {
    let changed: BTreeSet<Id> = after.iter().filter(|v| !before.iter().any(|b| b.id() == v.id() && b == *v)).map(AnyView::id).collect();
    let mut out = changed.clone();
    out.extend(after.iter().filter(|v| host_of(v).is_some_and(|h| changed.contains(&h))).map(AnyView::id));
    out
}

/// The delta that rigidly translated segment `index` from `before` to `after`: same segment count,
/// both ends moved by the same delta, bulge unchanged, and the delta longer than coincidence.
fn rigid_delta(before: &Polyline2, after: &Polyline2, index: usize) -> Option<Point2> {
    let tol = TOLERANCE.coincidence_mm;
    if before.segments.len() != after.segments.len() {
        return None;
    }
    let b = resolved_segments(before).into_iter().nth(index)?;
    let a = resolved_segments(after).into_iter().nth(index)?;
    let d = Point2 { x: a.from.x - b.from.x, y: a.from.y - b.from.y };
    let d_end = Point2 { x: a.to.x - b.to.x, y: a.to.y - b.to.y };
    let rigid = (d.x - d_end.x).hypot(d.y - d_end.y) <= tol && (a.bulge - b.bulge).abs() <= 1e-9;
    (rigid && d.x.hypot(d.y) > tol).then_some(d)
}

/// Step 0, travel. For each link in `travel` with exactly one end's entity changed by the command (same view,
/// different polyline) and the other unchanged, when the anchored segment was rigidly translated,
/// slides the partner by `travel_segment_move` and writes its new geometry into `after` (keeping
/// the entity's handle, layer and `ext`). Each partner travels at most once, first link in
/// `doc.links` order; nothing cascades — the partner's other links are left to step 2's re-check.
/// A partial slide (`unreachable`) is still applied; step 2 decides stale.
///
/// Known limitation, TS parity: an oblique segment slid along its own direction is a translation
/// whose crossing does not move, yet travel shifts the partner by the delta's component along the
/// cut; step 2 then flags that link stale.
fn travel(before_entities: &EntityMap, after: &mut Drawing, travel_set: &[Id]) -> Result<Vec<Id>, SchemaError> {
    let writes: Vec<(Id, Polyline2)> = {
        let p = DrawingProject::new(after)?;
        let after_entities = entity_map(&p);
        let pair = |id: &Id| match (before_entities.get(id), after_entities.get(id)) {
            (Some((bv, Some(bp))), Some((av, Some(ap)))) if bv == av => Some((bp, ap)),
            _ => None,
        };
        let mut writes: Vec<(Id, Polyline2)> = Vec::new();
        for link in p.document().links.iter().filter(|l| travel_set.contains(&l.id)) {
            for (mine, other) in [(&link.a, &link.b), (&link.b, &link.a)] {
                let (Some((bp, ap)), Some((op_before, op_after))) = (pair(&mine.entity_id), pair(&other.entity_id)) else { continue };
                if bp == ap || op_before != op_after || writes.iter().any(|(id, _)| *id == other.entity_id) {
                    continue;
                }
                // An unaccepted AI line is the sweep's to move (`crate::sweeps`); travelling it
                // would read as a user edit and accept it by divergence.
                if p.entity(other.entity_id).and_then(|e| e.ai_origin).is_some_and(|o| !o.accepted) {
                    continue;
                }
                let Some(d) = rigid_delta(bp, ap, mine.segment_index) else { continue };
                let Some(t) = travel_segment_move(&p, link, mine, d).map_err(derivation_error)? else { continue };
                if t.entity_id == other.entity_id && &t.geometry != op_after {
                    writes.push((t.entity_id, t.geometry));
                }
            }
        }
        writes
    };
    for (id, geometry) in &writes {
        let (kind, _) = from_polyline(geometry);
        // The corner tags go with the geometry, so `ext` never disagrees with what was written.
        let corners = if matches!(kind, EntityKind::LwPolyline(_)) { virtual_corner_entries(geometry) } else { Vec::new() };
        let mut err = None;
        after
            .modify_entity(Handle(id.0), |e| {
                e.kind = kind;
                err = read_entity_data(&e.common)
                    .and_then(|mut data| {
                        data.corners = corners;
                        set_entity_data(&mut e.common, &data)
                    })
                    .err();
            })
            .map_err(|e| derivation_error(e.to_string()))?;
        if let Some(e) = err {
            return Err(e);
        }
    }
    Ok(writes.into_iter().map(|(id, _)| id).collect())
}

/// `piece` lies along `old`: both its ends within coincidence of `old`, and the same curvature sign.
fn is_sub_span(old: &ResolvedSegment, piece: &ResolvedSegment) -> bool {
    let tol = TOLERANCE.coincidence_mm;
    let sign = |b: f64| {
        if b.abs() <= 1e-12 {
            0
        } else if b > 0.0 {
            1
        } else {
            -1
        }
    };
    sign(old.bulge) == sign(piece.bulge)
        && closest_point_on_segment(old, piece.from).distance <= tol
        && closest_point_on_segment(old, piece.to).distance <= tol
}

/// Step 0b, re-anchor. For each link end whose entity is gone from `after`, finds the first entity
/// new in `after` (handle order) in the same view with a segment that is a sub-span of the old
/// anchored segment and holds the old anchor point, and moves the end there. Ends with no such
/// piece are left for step 1 to drop. Writes `after`'s document once, only when something moved.
fn reanchor(before_entities: &EntityMap, after: &mut Drawing) -> Result<Vec<Id>, SchemaError> {
    let tol = TOLERANCE.coincidence_mm;
    let (links, moved) = {
        let p = DrawingProject::new(after)?;
        let after_entities = entity_map(&p);
        let new: Vec<(Id, Id, Vec<ResolvedSegment>)> = after_entities
            .iter()
            .filter(|(id, _)| !before_entities.contains_key(id))
            .filter_map(|(id, (view, poly))| poly.as_ref().map(|poly| (*id, *view, resolved_segments(poly))))
            .collect();
        let remap = |end: &LinkAnchor| -> Option<LinkAnchor> {
            if after_entities.contains_key(&end.entity_id) {
                return None;
            }
            let (view, Some(old_poly)) = before_entities.get(&end.entity_id)? else { return None };
            let old = resolved_segments(old_poly).into_iter().nth(end.segment_index)?;
            let at = point_at_param(&old, end.segment_param);
            new.iter().filter(|(_, v, _)| v == view).find_map(|(id, _, segs)| {
                segs.iter().filter(|s| is_sub_span(&old, s)).find_map(|s| {
                    let c = closest_point_on_segment(s, at);
                    (c.distance <= tol).then_some(LinkAnchor { entity_id: *id, segment_index: s.index, segment_param: c.t })
                })
            })
        };
        let mut moved = Vec::new();
        let links: Vec<LinkRecord> = p
            .document()
            .links
            .iter()
            .map(|link| {
                let (a, b) = (remap(&link.a), remap(&link.b));
                if a.is_none() && b.is_none() {
                    return link.clone();
                }
                let mut l = link.clone();
                if let Some(a) = a {
                    l.a = a;
                }
                if let Some(b) = b {
                    l.b = b;
                }
                moved.push(l.id);
                l
            })
            .collect();
        (links, moved)
    };
    if !moved.is_empty() {
        let mut doc = read_document(after)?;
        doc.links = links;
        write_document(after, &doc)?;
    }
    Ok(moved)
}

/// Step 0c, erase cascade. For each link in `travel` with one end's entity gone from `after` (after
/// step 0b) and the other still there, removes that partner — unless the gone entity's view gained an
/// entity this command, which is TRIM/BREAK replacing it with pieces, not an erase. One hop: the
/// partners are chosen against `after` as the command left it, so a partner's own links never
/// cascade. An unaccepted AI partner goes too — the line it mirrors is gone.
fn erase_partners(before_entities: &EntityMap, after: &mut Drawing, travel_set: &[Id]) -> Result<Vec<Id>, SchemaError> {
    let erased = {
        let p = DrawingProject::new(after)?;
        let after_entities = entity_map(&p);
        let grown: BTreeSet<Id> = after_entities.iter().filter(|(id, _)| !before_entities.contains_key(id)).map(|(_, (v, _))| *v).collect();
        let mut out: Vec<Id> = Vec::new();
        for link in p.document().links.iter().filter(|l| travel_set.contains(&l.id)) {
            for (mine, other) in [(&link.a, &link.b), (&link.b, &link.a)] {
                let Some((view, _)) = before_entities.get(&mine.entity_id) else { continue };
                if after_entities.contains_key(&mine.entity_id)
                    || grown.contains(view)
                    || !after_entities.contains_key(&other.entity_id)
                    || out.contains(&other.entity_id)
                {
                    continue;
                }
                out.push(other.entity_id);
            }
        }
        out
    };
    for id in &erased {
        after.remove_entity(Handle(id.0));
    }
    Ok(erased)
}

/// Step 2b, creation-time role (TS T256 `pushEntity`): each of `new_entities` stored as `Bylayer`
/// on a layer whose default is `Cut` gets `Cut` in a plan view and `Seen` in a vertical or detail
/// view, written into its `EntityData`. Any other layer default, or a concrete stored role, is left.
fn stamp_roles(after: &mut Drawing, new_entities: &[Id]) -> Result<Vec<Id>, SchemaError> {
    let writes: Vec<(Id, LineRole)> = {
        let p = DrawingProject::new(after)?;
        new_entities
            .iter()
            .filter_map(|id| {
                let e = p.entity(*id)?;
                if e.role != LineRole::Bylayer {
                    return None;
                }
                let layer = p.layers().iter().find(|l| l.id == e.layer_id)?;
                if layer.default_role != LineRole::Cut {
                    return None;
                }
                let role = if p.view(e.view_id)?.kind == ViewKind::Plan { LineRole::Cut } else { LineRole::Seen };
                Some((*id, role))
            })
            .collect()
    };
    for (id, role) in &writes {
        let mut err = None;
        after
            .modify_entity(Handle(id.0), |e| {
                err = read_entity_data(&e.common)
                    .and_then(|mut data| {
                        data.role = *role;
                        set_entity_data(&mut e.common, &data)
                    })
                    .err();
            })
            .map_err(|e| derivation_error(e.to_string()))?;
        if let Some(e) = err {
            return Err(e);
        }
    }
    Ok(writes.into_iter().map(|(id, _)| id).collect())
}

/// Some link already joins segment `a` to segment `b`, either way round.
fn already_joined(links: &[LinkRecord], r: &LinkRecord) -> bool {
    let key = |x: &LinkAnchor| (x.entity_id, x.segment_index);
    links.iter().any(|l| {
        let (la, lb, ra, rb) = (key(&l.a), key(&l.b), key(&r.a), key(&r.b));
        (la == ra && lb == rb) || (la == rb && lb == ra)
    })
}

/// Keeps `after`'s links true to its geometry, given the drawing `before` the command ran and the
/// selection's travel set (`travel`: the only links step 0 may travel). Writes `after`'s LAMB document once per step that changed it; when nothing changes, `after` is
/// left untouched.
pub fn maintain_links(before: &Drawing, after: &mut Drawing, now: &str, travel_set: &[Id]) -> Result<MaintenanceReport, SchemaError> {
    let mut report = MaintenanceReport::default();
    let before_project = DrawingProject::new(before)?;
    let before_entities = entity_map(&before_project);
    let before_views = before_project.document().views.clone();
    drop(before_project);

    // 0: travel, before anything reads `after`.
    report.travelled = travel(&before_entities, after, travel_set)?;
    // 0b: re-anchor ends whose entity was replaced by pieces, before step 1 would drop them.
    report.reanchored = reanchor(&before_entities, after)?;
    // 0c: an erased entity takes its travel-set partners with it; step 1 drops their links.
    report.erased = erase_partners(&before_entities, after, travel_set)?;
    // 0d: vertex inserts, removals and reversals on the same handle keep anchors on their segments.
    let (remapped, dropped) = crate::revertex::remap_vertex_edits(&before_entities, after)?;
    report.reanchored.extend(remapped);
    report.dropped.extend(dropped);

    // 1 + 2: drop and re-check, against one read of `after`.
    let (new_entities, links) = {
        let p = DrawingProject::new(after)?;
        let after_entities = entity_map(&p);
        let touched_views = touched_views(&before_views, &p.document().views);
        let touched = |id: Id| match after_entities.get(&id) {
            None => false,
            Some((view, poly)) => touched_views.contains(view) || before_entities.get(&id).is_none_or(|(bv, bp)| bv != view || bp != poly),
        };
        let mut links = Vec::with_capacity(p.document().links.len());
        let mut changed = false;
        for link in &p.document().links {
            let (a, b) = (link.a.entity_id, link.b.entity_id);
            if !after_entities.contains_key(&a) || !after_entities.contains_key(&b) {
                report.dropped.push(link.id);
                changed = true;
                continue;
            }
            let mut link = link.clone();
            if touched(a) || touched(b) {
                let holds = link_still_coincides(&p, &link).map_err(derivation_error)?;
                let stale = link.stale == Some(true);
                if !holds && !stale {
                    link.stale = Some(true);
                    report.staled.push(link.id);
                    changed = true;
                } else if holds && stale {
                    link.stale = None;
                    report.cleared.push(link.id);
                    changed = true;
                }
            }
            links.push(link);
        }
        let new_entities: Vec<Id> = after_entities.keys().filter(|id| !before_entities.contains_key(id)).copied().collect();
        (new_entities, changed.then_some(links))
    };
    if let Some(links) = links {
        let mut doc = read_document(after)?;
        doc.links = links;
        write_document(after, &doc)?;
    }

    // 2b: stamp a concrete role on new entities of cut-default layers, before markers and links read it.
    report.stamped = stamp_roles(after, &new_entities)?;

    // 3: auto-link each new entity, in handle order, each seeing the links of the ones before it.
    let actor = Actor::User { user_id: "local".into() };
    for id in new_entities {
        let mut records: Vec<LinkRecord> = {
            let p = DrawingProject::new(after)?;
            let Some(entity) = p.entity(id) else { continue };
            // TS T113: a reference line never auto-links.
            if resolve_entity_role(p.layers(), entity.role, entity.layer_id) == LineRole::Reference {
                continue;
            }
            let mut placeholder = 0u64;
            let mut mint = || {
                placeholder += 1;
                Id(placeholder)
            };
            let existing = &p.document().links;
            let mut found = find_auto_links(&p, &entity, &actor, now, &mut mint).map_err(derivation_error)?;
            found.retain(|r| !already_joined(existing, r));
            found
        };
        if records.is_empty() {
            continue;
        }
        for r in &mut records {
            r.id = Id(after.new_handle().0);
            report.created.push(r.id);
        }
        let mut doc = read_document(after)?;
        doc.links.extend(records);
        write_document(after, &doc)?;
    }

    // 4: a layer change carries the entity's automatic-link component with it.
    report.relayered = crate::relayer::cascade_layers(before, after)?;
    Ok(report)
}

/// The after-command observer: runs [`maintain_links`] on a copy of the current drawing and
/// writes it back only when something changed. A failure is echoed and the drawing is left as the
/// command left it.
pub fn link_observer(s: &mut Session, before: &Arc<Drawing>) {
    let Ok(current) = s.doc() else { return };
    let mut after = current.clone();
    let travel_set: Vec<Id> = s.travel().into_iter().map(Id).collect();
    match maintain_links(before, &mut after, &now(), &travel_set) {
        Ok(report) if !report.is_empty() => {
            if let Ok(d) = s.doc_mut() {
                *d = after;
            }
        }
        Ok(_) => {}
        Err(e) => s.echo(format!("LAMB link maintenance: {e}")),
    }
}

/// The idle selection-box observer (engine patch 19): a crossing box makes the travel set every
/// link with an end on an entity it selected; a window box empties it. Not a LAMB drawing: nothing.
pub fn travel_box_observer(s: &mut Session, handles: &[Handle], crossing: bool) {
    if !crossing {
        s.set_travel(Vec::new());
        return;
    }
    let ids: Vec<u64> = {
        let Ok(d) = s.doc() else { return };
        let Ok(p) = DrawingProject::new(d) else { return };
        let on = |id: Id| handles.contains(&Handle(id.0));
        p.document().links.iter().filter(|l| on(l.a.entity_id) || on(l.b.entity_id)).map(|l| l.id.0).collect()
    };
    s.set_travel(ids);
}

/// Registers [`crate::corners::corner_observer`], [`link_observer`],
/// [`crate::sweeps::sweep_observer`], [`crate::assembly::assembly_observer`], [`crate::bubbles::bubble_observer`] then
/// [`crate::annotations::annotation_observer`] on `s` (each
/// once), so link travel reads corner tags already re-seated by the same command, the AI /
/// solid-cut sweeps see the links already maintained, and the assembly blocks regenerate from all
/// of it; and [`travel_box_observer`] once per process.
pub fn install(s: &mut Session) {
    let _ = cadcraft_engine::travel::register_box_observer(travel_box_observer);
    let fs: [cadcraft_engine::AfterCommand; 6] = [
        crate::corners::corner_observer,
        link_observer,
        crate::sweeps::sweep_observer,
        crate::assembly::assembly_observer,
        crate::bubbles::bubble_observer,
        crate::annotations::annotation_observer,
    ];
    for f in fs {
        if !s.observers.iter().any(|o| std::ptr::fn_addr_eq(*o, f)) {
            s.observers.push(f);
        }
    }
}

/// Now as ISO-8601 UTC with milliseconds (`2026-10-12T09:30:00.000Z`).
#[cfg(not(target_arch = "wasm32"))]
pub fn now() -> String {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    format_utc_ms(u64::try_from(ms).unwrap_or(u64::MAX))
}

/// `SystemTime::now` panics on wasm32-unknown-unknown; a fixed epoch stamp stands in.
#[cfg(target_arch = "wasm32")]
pub fn now() -> String {
    "1970-01-01T00:00:00.000Z".into()
}

/// Milliseconds since the Unix epoch as ISO-8601 UTC (Howard Hinnant's days-to-civil).
pub fn format_utc_ms(ms: u64) -> String {
    let secs = ms / 1000;
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX / 2);
    let sod = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z", sod / 3600, sod % 3600 / 60, sod % 60, ms % 1000)
}

#[cfg(test)]
mod tests {
    use super::format_utc_ms;

    #[test]
    fn utc_formatting() {
        assert_eq!(format_utc_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_utc_ms(951_782_400_123), "2000-02-29T00:00:00.123Z");
        assert_eq!(format_utc_ms(1_791_793_845_678), "2026-10-12T08:30:45.678Z");
    }
}

//! The after-command observer that keeps `LambDocument.links` true (`cadcraft-integration.md` §7
//! "link formation runs in the after-command hook", §13 phase 3 "TRIM a linked line → links
//! maintained or stale").
//!
//! [`maintain_links`] compares the drawing before and after one command and, in order:
//! 0. travels: a linked segment the command rigidly translated carries its unchanged partner in the
//!    other view with it (`selection-and-link-editing.md` §4b ray-sliding, lamb-core
//!    `travel_segment_move`), so the link stays live — §13 phase 3 "STRETCH a linked plan line → the
//!    section follows in one undo step";
//! 1. drops every link with an anchor whose entity is no longer in any view block (ERASE);
//! 2. re-checks every link with an anchor on a touched entity (new, or a different polyline) or in
//!    a touched view (its document entry, or its host's, changed) with lamb-core's
//!    `link_still_coincides`: a broken crossing is flagged `stale: Some(true)`, a restored one is
//!    cleared back to `None` — a moved line is flagged, never dropped, because a flag is visible and
//!    reversible (`link-maintenance.md`: "visible flags beat silent resolution");
//! 3. auto-links each entity new to a view block through its view's hollow markers.
//!
//! [`install`] registers [`link_observer`] on a `Session`, so all of this lands in the command's own
//! undo step. Out of scope: travel cascading past the first partner, Tier 2 declarations.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use cadcraft_doc::{Drawing, Handle};
use cadcraft_engine::Session;
use lamb_core::derivation::linking::{find_auto_links, link_still_coincides};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::derivation::travel::travel_segment_move;
use lamb_core::geometry::polyline::resolved_segments;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::geometry::{Point2, Polyline2};
use lamb_core::model::{Actor, AnyView, Id, LinkRecord};

use crate::convert::from_polyline;

use crate::project::DrawingProject;
use crate::schema::{SchemaError, read_document, write_document};

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
}

impl MaintenanceReport {
    pub fn is_empty(&self) -> bool {
        self.dropped.is_empty() && self.staled.is_empty() && self.cleared.is_empty() && self.created.is_empty() && self.travelled.is_empty()
    }
}

fn derivation_error(message: String) -> SchemaError {
    SchemaError::Malformed { what: "link maintenance".into(), message }
}

/// Entity id → (view id, polyline) over every view block.
type EntityMap = BTreeMap<Id, (Id, Option<Polyline2>)>;

fn entity_map(p: &DrawingProject<'_>) -> EntityMap {
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

/// Step 0, travel. For each link with exactly one end's entity changed by the command (same view,
/// different polyline) and the other unchanged, when the anchored segment was rigidly translated,
/// slides the partner by `travel_segment_move` and writes its new geometry into `after` (keeping
/// the entity's handle, layer and `ext`). Each partner travels at most once, first link in
/// `doc.links` order; nothing cascades — the partner's other links are left to step 2's re-check.
/// A partial slide (`unreachable`) is still applied; step 2 decides stale.
///
/// Known limitation, TS parity: an oblique segment slid along its own direction is a translation
/// whose crossing does not move, yet travel shifts the partner by the delta's component along the
/// cut; step 2 then flags that link stale.
fn travel(before_entities: &EntityMap, after: &mut Drawing) -> Result<Vec<Id>, SchemaError> {
    let writes: Vec<(Id, Polyline2)> = {
        let p = DrawingProject::new(after)?;
        let after_entities = entity_map(&p);
        let pair = |id: &Id| match (before_entities.get(id), after_entities.get(id)) {
            (Some((bv, Some(bp))), Some((av, Some(ap)))) if bv == av => Some((bp, ap)),
            _ => None,
        };
        let mut writes: Vec<(Id, Polyline2)> = Vec::new();
        for link in &p.document().links {
            for (mine, other) in [(&link.a, &link.b), (&link.b, &link.a)] {
                let (Some((bp, ap)), Some((op_before, op_after))) = (pair(&mine.entity_id), pair(&other.entity_id)) else { continue };
                if bp == ap || op_before != op_after || writes.iter().any(|(id, _)| *id == other.entity_id) {
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
        after.modify_entity(Handle(id.0), |e| e.kind = kind).map_err(|e| derivation_error(e.to_string()))?;
    }
    Ok(writes.into_iter().map(|(id, _)| id).collect())
}

/// Keeps `after`'s links true to its geometry, given the drawing `before` the command ran.
/// Writes `after`'s LAMB document once per step that changed it; when nothing changes, `after` is
/// left untouched.
pub fn maintain_links(before: &Drawing, after: &mut Drawing, now: &str) -> Result<MaintenanceReport, SchemaError> {
    let mut report = MaintenanceReport::default();
    let before_project = DrawingProject::new(before)?;
    let before_entities = entity_map(&before_project);
    let before_views = before_project.document().views.clone();
    drop(before_project);

    // 0: travel, before anything reads `after`.
    report.travelled = travel(&before_entities, after)?;

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

    // 3: auto-link each new entity, in handle order, each seeing the links of the ones before it.
    let actor = Actor::User { user_id: "local".into() };
    for id in new_entities {
        let mut records: Vec<LinkRecord> = {
            let p = DrawingProject::new(after)?;
            let Some(entity) = p.entity(id) else { continue };
            let mut placeholder = 0u64;
            let mut mint = || {
                placeholder += 1;
                Id(placeholder)
            };
            find_auto_links(&p, &entity, &actor, now, &mut mint).map_err(derivation_error)?
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
    Ok(report)
}

/// The after-command observer: runs [`maintain_links`] on a copy of the current drawing and
/// writes it back only when something changed. A failure is echoed and the drawing is left as the
/// command left it.
pub fn link_observer(s: &mut Session, before: &Arc<Drawing>) {
    let Ok(current) = s.doc() else { return };
    let mut after = current.clone();
    match maintain_links(before, &mut after, &now()) {
        Ok(report) if !report.is_empty() => {
            if let Ok(d) = s.doc_mut() {
                *d = after;
            }
        }
        Ok(_) => {}
        Err(e) => s.echo(format!("LAMB link maintenance: {e}")),
    }
}

/// Registers [`link_observer`] on `s` (once).
pub fn install(s: &mut Session) {
    let f: cadcraft_engine::AfterCommand = link_observer;
    if !s.observers.iter().any(|o| std::ptr::fn_addr_eq(*o, f)) {
        s.observers.push(f);
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

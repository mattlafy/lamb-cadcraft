//! The after-command AI-line and solid-cut sweeps — the fork's port of the TS engine's
//! `applyDocumentEffects` (`packages/command-engine/src/engine.ts`), run in the command's own undo
//! step:
//! 0. every view-block entity new after the command whose `proposal_id` an entity before it
//!    already held (a COPY) has its `ai_origin` cleared — the copy is plain user geometry;
//!    0a. every unaccepted AI line that is an end of an explicit link new after the command is
//!    accepted: linking a proposal is taking it, and an unaccepted solid cut would otherwise be
//!    re-synthesised around the new link and removed, link and all (LAMB T597);
//!    0b. every unaccepted AI polyline that an entity new after the command (not an AI line) is
//!    drawn over (a segment collinear with one of the AI line's, within the coincidence tolerance
//!    of it, overlapping it by more than the grouping tolerance) is erased whole, and its slot
//!    suppressed as step 1 does, so it is not re-proposed (T597);
//! 1. every AI line (accepted or not) in a view block of the drawing `before` the command that is
//!    gone after it (ERASE), its slot held by nothing left, has its `proposal_id` appended to
//!    `suppressed_proposals` (unless already there), so the sweep that follows doesn't re-propose
//!    it — a slot filled once stays filled (T513);
//! 2. lamb-core's `reconcile_ai_lines` proposes, moves and removes AI lines;
//! 3. lamb-core's `reconcile_solid_cut_lines_once` materialises derived solid cuts, never on a
//!    slot already filled (T513);
//! 4. ghost upkeep: every unaccepted AI line draws at [`AI_GHOST`] transparency, and an accepted
//!    one still at it goes back to `ByLayer` (T597; ACCEPT writes the origin directly).
//!
//! Both sweeps write through [`DrawingProjectMut`]; minted ids are handles taken from the
//! drawing's handle seed, which is bumped past the last one afterwards. The sweeps'
//! `ReconcileResult` is not kept: undo restores the drawing snapshot.

use std::cell::Cell;
use std::collections::HashSet;
use std::sync::Arc;

use cadcraft_doc::{Drawing, Handle, Transparency};
use cadcraft_engine::Session;
use lamb_core::derivation::proposals::{ReconcileResult, reconcile_ai_lines};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::derivation::solid_cut_lines::reconcile_solid_cut_lines_once;
use lamb_core::derivation::write::ProjectWrite;
use lamb_core::geometry::polyline::{ResolvedSegment, resolved_segments};
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AiOrigin, Id, LinkOrigin};

use crate::project::DrawingProject;
use crate::project_mut::{AI_GHOST, DrawingProjectMut};
use crate::schema::SchemaError;

/// (entity id, proposal id) of every AI line in a view block — every one when `accepted_too`,
/// else only the unaccepted (TS `listUnacceptedAiEntityIds`).
fn ai_lines(p: &DrawingProject<'_>, accepted_too: bool) -> Vec<(Id, String)> {
    let mut out = Vec::new();
    for v in p.views() {
        for e in p.entities(v.id) {
            if let Some(o) = e.ai_origin
                && (accepted_too || !o.accepted)
            {
                out.push((e.id, o.proposal_id.clone()));
            }
        }
    }
    out
}

/// Every view-block entity new in `a` whose `proposal_id` is also held by an entity that exists in
/// `b` — a COPY of an AI line, which carried its origin along (T513). The copy is the user's line.
fn copied_ai_lines(b: &DrawingProject<'_>, a: &DrawingProject<'_>) -> Vec<Id> {
    let mut held: HashSet<String> = HashSet::new();
    for v in b.views() {
        for e in b.entities(v.id) {
            if let Some(o) = e.ai_origin {
                held.insert(o.proposal_id.clone());
            }
        }
    }
    let mut out = Vec::new();
    for v in a.views() {
        for e in a.entities(v.id) {
            if let Some(o) = e.ai_origin
                && held.contains(&o.proposal_id)
                && b.entity(e.id).is_none()
            {
                out.push(e.id);
            }
        }
    }
    out
}

/// Every unaccepted AI line in `a` that is an end of an explicit link `b` did not have, with its
/// origin accepted.
fn newly_linked_proposals(b: &DrawingProject<'_>, a: &DrawingProject<'_>) -> Vec<(Id, AiOrigin)> {
    let old: HashSet<Id> = b.links().map(|l| l.id).collect();
    let mut out: Vec<(Id, AiOrigin)> = Vec::new();
    for l in a.links().filter(|l| l.origin == LinkOrigin::Explicit && !old.contains(&l.id)) {
        for end in [l.a.entity_id, l.b.entity_id] {
            if let Some(o) = a.entity(end).and_then(|e| e.ai_origin.filter(|o| !o.accepted).cloned())
                && !out.iter().any(|x| x.0 == end)
            {
                out.push((end, AiOrigin { accepted: true, ..o }));
            }
        }
    }
    out
}

/// Whether straight segments `n` (a new line's) and `ai` lie on one another: collinear within
/// `TOLERANCE.angle_deg`, both ends of `n` within `TOLERANCE.coincidence_mm` of `ai`'s line, and
/// their overlap along it longer than `TOLERANCE.grouping_mm`.
fn drawn_over(n: &ResolvedSegment, ai: &ResolvedSegment) -> bool {
    if n.bulge != 0.0 || ai.bulge != 0.0 {
        return false;
    }
    let (dx, dy) = (ai.to.x - ai.from.x, ai.to.y - ai.from.y);
    let len = dx.hypot(dy);
    let (ex, ey) = (n.to.x - n.from.x, n.to.y - n.from.y);
    let nlen = ex.hypot(ey);
    if len <= TOLERANCE.grouping_mm || nlen <= TOLERANCE.grouping_mm {
        return false;
    }
    if ((dx * ey - dy * ex) / (len * nlen)).abs() > TOLERANCE.angle_deg.to_radians().sin() {
        return false;
    }
    let (ux, uy) = (dx / len, dy / len);
    let off = |x: f64, y: f64| ((x - ai.from.x) * uy - (y - ai.from.y) * ux).abs();
    if off(n.from.x, n.from.y) > TOLERANCE.coincidence_mm || off(n.to.x, n.to.y) > TOLERANCE.coincidence_mm {
        return false;
    }
    let along = |x: f64, y: f64| (x - ai.from.x) * ux + (y - ai.from.y) * uy;
    let (t1, t2) = (along(n.from.x, n.from.y), along(n.to.x, n.to.y));
    t1.max(t2).min(len) - t1.min(t2).max(0.0) > TOLERANCE.grouping_mm
}

/// (entity, proposal id) of every unaccepted AI polyline in `a` that an entity new in `a` (not
/// itself an AI line) in the same view is drawn over.
fn drawn_over_proposals(b: &DrawingProject<'_>, a: &DrawingProject<'_>) -> Vec<(Id, String)> {
    let mut out = Vec::new();
    for v in a.views() {
        let new: Vec<ResolvedSegment> = a
            .entities(v.id)
            .filter(|e| e.ai_origin.is_none() && b.entity(e.id).is_none())
            .filter_map(|e| e.polyline.map(|p| resolved_segments(&p)))
            .flatten()
            .collect();
        if new.is_empty() {
            continue;
        }
        for e in a.entities(v.id) {
            let Some(o) = e.ai_origin.filter(|o| !o.accepted) else { continue };
            let Some(poly) = e.polyline else { continue };
            if resolved_segments(&poly).iter().any(|g| new.iter().any(|n| drawn_over(n, g))) {
                out.push((e.id, o.proposal_id.clone()));
            }
        }
    }
    out
}

/// Step 4: every unaccepted AI line at [`AI_GHOST`], every accepted one off it. Returns whether
/// anything was written.
fn ghost_upkeep(m: &mut DrawingProjectMut<'_>) -> bool {
    let want: Vec<(Handle, Transparency)> = {
        let views: Vec<Id> = m.views().map(|v| v.id).collect();
        let lines: Vec<(Handle, bool)> = views
            .into_iter()
            .flat_map(|v| m.entities(v).filter_map(|e| e.ai_origin.map(|o| (Handle(e.id.0), o.accepted))).collect::<Vec<_>>())
            .collect();
        lines
            .into_iter()
            .filter_map(|(h, accepted)| match (accepted, m.transparency(h)?) {
                (false, Transparency::ByLayer) => Some((h, AI_GHOST)),
                (true, t) if t == AI_GHOST => Some((h, Transparency::ByLayer)),
                _ => None,
            })
            .collect()
    };
    for &(h, t) in &want {
        m.set_transparency(h, t);
    }
    !want.is_empty()
}

fn sweep_error(message: String) -> SchemaError {
    SchemaError::Malformed { what: "AI / solid-cut sweep".into(), message }
}

/// Runs the four steps on `after`, given the drawing `before` the command ran. Returns whether
/// anything changed; on `Err`, `after` may be partly written and must be discarded.
pub fn run_sweeps(before: &Drawing, after: &mut Drawing, now: &str) -> Result<bool, SchemaError> {
    let (mut erased, copies, linked, over) = {
        let b = DrawingProject::new(before)?;
        let a = DrawingProject::new(after)?;
        let held_after = ai_lines(&a, true).into_iter().map(|(_, key)| key).collect::<HashSet<_>>();
        let erased: Vec<String> =
            ai_lines(&b, true).into_iter().filter(|(id, key)| a.entity(*id).is_none() && !held_after.contains(key)).map(|(_, key)| key).collect();
        (erased, copied_ai_lines(&b, &a), newly_linked_proposals(&b, &a), drawn_over_proposals(&b, &a))
    };
    let seed = after.handseed;
    let next = Cell::new(seed);
    let mut mint = || {
        let id = next.get();
        next.set(id.saturating_add(1));
        Id(id)
    };
    let changed = {
        let mut m = DrawingProjectMut::new(after)?;
        let mut suppressed = !copies.is_empty() || !linked.is_empty() || !over.is_empty();
        for id in copies {
            m.set_ai_origin(id, None);
        }
        for (id, origin) in linked {
            m.set_ai_origin(id, Some(origin));
        }
        if !over.is_empty() {
            let ids: HashSet<Id> = over.iter().map(|x| x.0).collect();
            m.take_links(&mut |l| ids.contains(&l.a.entity_id) || ids.contains(&l.b.entity_id));
            m.remove_entities(&ids);
            erased.extend(over.into_iter().map(|x| x.1));
        }
        let doc = m.document_mut();
        for key in erased {
            if !doc.suppressed_proposals.contains(&key) {
                doc.suppressed_proposals.push(key);
                suppressed = true;
            }
        }
        let ai = reconcile_ai_lines(&mut m, now, &mut mint).map_err(sweep_error)?;
        let cuts = reconcile_solid_cut_lines_once(&mut m, &mut mint).map_err(sweep_error)?;
        let ghosts = ghost_upkeep(&mut m);
        let changed = suppressed || ghosts || !is_empty(&ai) || !is_empty(&cuts);
        if changed {
            m.finish()?;
        }
        changed
    };
    if next.get() > seed {
        after.bump_handseed(Handle(next.get() - 1));
    }
    Ok(changed)
}

fn is_empty(r: &ReconcileResult) -> bool {
    r.created_entities.is_empty()
        && r.removed_entities.is_empty()
        && r.updated_entities.is_empty()
        && r.created_links.is_empty()
        && r.removed_links.is_empty()
        && r.accepted_by_divergence.is_empty()
        && r.pruned_suppressions.is_empty()
}

/// The after-command observer: runs [`run_sweeps`] on a copy of the current drawing and writes it
/// back only when something changed. A failure is echoed and the drawing is left as it was.
pub fn sweep_observer(s: &mut Session, before: &Arc<Drawing>) {
    let Ok(current) = s.doc() else { return };
    let mut after = current.clone();
    match run_sweeps(before, &mut after, &crate::maintain::now()) {
        Ok(true) => {
            if let Ok(d) = s.doc_mut() {
                *d = after;
            }
        }
        Ok(false) => {}
        Err(e) => s.echo(format!("LAMB sweeps: {e}")),
    }
}

//! The after-command AI-line and solid-cut sweeps — the fork's port of the TS engine's
//! `applyDocumentEffects` (`packages/command-engine/src/engine.ts`), run in the command's own undo
//! step:
//! 1. every unaccepted AI line in a view block of the drawing `before` the command that is gone
//!    after it (ERASE) has its `proposal_id` appended to `suppressed_proposals` (unless already
//!    there), so the sweep that follows doesn't re-propose it;
//! 2. lamb-core's `reconcile_ai_lines` proposes, moves and removes AI lines;
//! 3. lamb-core's `reconcile_solid_cut_lines` materialises derived solid cuts.
//!
//! Both sweeps write through [`DrawingProjectMut`]; minted ids are handles taken from the
//! drawing's handle seed, which is bumped past the last one afterwards. The sweeps'
//! `ReconcileResult` is not kept: undo restores the drawing snapshot.

use std::cell::Cell;
use std::sync::Arc;

use cadcraft_doc::{Drawing, Handle};
use cadcraft_engine::Session;
use lamb_core::derivation::proposals::{ReconcileResult, reconcile_ai_lines};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::derivation::solid_cut_lines::reconcile_solid_cut_lines;
use lamb_core::model::Id;

use crate::project::DrawingProject;
use crate::project_mut::DrawingProjectMut;
use crate::schema::SchemaError;

/// (entity id, proposal id) of every unaccepted AI line in a view block (TS
/// `listUnacceptedAiEntityIds`).
fn unaccepted_ai_lines(p: &DrawingProject<'_>) -> Vec<(Id, String)> {
    let mut out = Vec::new();
    for v in p.views() {
        for e in p.entities(v.id) {
            if let Some(o) = e.ai_origin
                && !o.accepted
            {
                out.push((e.id, o.proposal_id.clone()));
            }
        }
    }
    out
}

fn sweep_error(message: String) -> SchemaError {
    SchemaError::Malformed { what: "AI / solid-cut sweep".into(), message }
}

/// Runs the three steps on `after`, given the drawing `before` the command ran. Returns whether
/// anything changed; on `Err`, `after` may be partly written and must be discarded.
pub fn run_sweeps(before: &Drawing, after: &mut Drawing, now: &str) -> Result<bool, SchemaError> {
    let erased: Vec<String> = {
        let b = DrawingProject::new(before)?;
        let a = DrawingProject::new(after)?;
        unaccepted_ai_lines(&b).into_iter().filter(|(id, _)| a.entity(*id).is_none()).map(|(_, key)| key).collect()
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
        let doc = m.document_mut();
        let mut suppressed = false;
        for key in erased {
            if !doc.suppressed_proposals.contains(&key) {
                doc.suppressed_proposals.push(key);
                suppressed = true;
            }
        }
        let ai = reconcile_ai_lines(&mut m, now, &mut mint).map_err(sweep_error)?;
        let cuts = reconcile_solid_cut_lines(&mut m, &mut mint).map_err(sweep_error)?;
        let changed = suppressed || !is_empty(&ai) || !is_empty(&cuts);
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

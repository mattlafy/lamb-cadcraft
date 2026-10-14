//! WALLFIT: close a plan's width conflicts by moving each conflicted straight run's non-reference
//! face so the drawn wall is as thick as its detail. The detail's reference face always holds; the
//! face named by [`WidthConflict::toward`] moves by its `delta`, along the run's normal toward that
//! face. A LINE face moves both ends; an LWPOLYLINE face moves its segment's two vertices, so the
//! neighbouring segments stretch with it (a rectangle stays a rectangle, and two faces sharing a
//! corner vertex compose). Arc runs carry no conflict yet and are never fitted.

use cadcraft_doc::{Drawing, EntityKind, Handle};
use cadcraft_engine::{Result as EngineResult, Session};
use lamb_core::derivation::assembly::FaceRef;
use lamb_core::derivation::wall_spaces::SideKind;
use lamb_core::derivation::walls::RunCurve;
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::assembly::{assembly_lines, check_plan};
use crate::commands::{id_value, other};
use crate::detailtags::graph_and_sides;
use crate::project::DrawingProject;

/// One face segment to move: the entity, its segment index, and the displacement.
struct Move {
    handle: Handle,
    segment: usize,
    dx: f64,
    dy: f64,
}

/// The midpoint of segment `segment` of entity `h` (a LINE's only segment, or an LWPOLYLINE's).
fn segment_mid(d: &Drawing, h: Handle, segment: usize) -> Result<(f64, f64), String> {
    let e = d.entity(h).ok_or_else(|| format!("face entity {:X} not found", h.0))?;
    match &e.kind {
        EntityKind::Line(l) => Ok(((l.a.x + l.b.x) / 2.0, (l.a.y + l.b.y) / 2.0)),
        EntityKind::LwPolyline(p) => {
            let n = p.vertices.len();
            let next = if segment + 1 < n {
                segment + 1
            } else if p.closed {
                0
            } else {
                return Err(format!("face entity {:X} has no segment {segment}", h.0));
            };
            let (a, b) = (p.vertices.get(segment).ok_or_else(|| format!("face entity {:X} has no segment {segment}", h.0))?.p, p.vertices[next].p);
            Ok(((a.x + b.x) / 2.0, (a.y + b.y) / 2.0))
        }
        _ => Err(format!("face entity {:X} is neither a LINE nor an LWPOLYLINE", h.0)),
    }
}

fn apply(d: &mut Drawing, m: &Move) -> Result<(), String> {
    let (dx, dy, seg) = (m.dx, m.dy, m.segment);
    d.modify_entity(m.handle, |e| match &mut e.kind {
        EntityKind::Line(l) => {
            l.a.x += dx;
            l.a.y += dy;
            l.b.x += dx;
            l.b.y += dy;
        }
        EntityKind::LwPolyline(p) => {
            let n = p.vertices.len();
            let next = if seg + 1 < n { seg + 1 } else { 0 };
            for i in [seg, next] {
                p.vertices[i].p.x += dx;
                p.vertices[i].p.y += dy;
            }
        }
        _ => {}
    })
    .map_err(|e| e.to_string())
}

/// Moves the non-reference face of every conflicted straight run of `plan` (only those in `only`,
/// when given) by its conflict's delta. Returns the fitted run indices, ascending. Every face is
/// checked before anything moves; on error the drawing is untouched.
pub fn wall_fit(d: &mut Drawing, plan: Id, only: Option<&[usize]>) -> Result<Vec<usize>, String> {
    let moves = plan_moves(d, plan, only)?;
    for (_, m) in &moves {
        apply(d, m)?;
    }
    Ok(moves.into_iter().map(|(i, _)| i).collect())
}

/// The face moves [`wall_fit`] would make, with their run indices, computed without mutating.
fn plan_moves(d: &Drawing, plan: Id, only: Option<&[usize]>) -> Result<Vec<(usize, Move)>, String> {
    check_plan(d, plan)?;
    let runs = assembly_lines(d, plan)?;
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let (graph, sides) = graph_and_sides(&p, plan);
    let mut moves = Vec::new();
    for r in &runs {
        let Some(c) = r.conflict else { continue };
        if only.is_some_and(|o| !o.contains(&r.run)) {
            continue;
        }
        let (run, side) = (&graph.runs[r.run], &sides.runs[r.run]);
        let RunCurve::Line { from, to } = run.centreline else { continue };
        let outside_left = match (side.left, side.right) {
            (SideKind::Outside, SideKind::Space(_)) => true,
            (SideKind::Space(_), SideKind::Outside) => false,
            _ => continue,
        };
        let target_left = outside_left == (c.toward == FaceRef::Exterior);
        let (dx, dy) = (to.x - from.x, to.y - from.y);
        let len = dx.hypot(dy);
        let mut found = None;
        for f in &run.faces {
            let h = Handle(f.from.entity_id.0);
            let (mx, my) = segment_mid(d, h, f.from.segment_index)?;
            let left = dx * (my - from.y) - dy * (mx - from.x) > 0.0;
            if left == target_left {
                found = Some((h, f.from.segment_index));
            }
        }
        let Some((handle, segment)) = found else { return Err(format!("run {}: no face on the {} side", r.run, c.toward.name())) };
        let s = if target_left { 1.0 } else { -1.0 };
        let (nx, ny) = (-dy / len * s, dx / len * s);
        moves.push((r.run, Move { handle, segment, dx: nx * c.delta, dy: ny * c.delta }));
    }
    Ok(moves)
}

/// WALLFIT `{viewId, runs?: [index]}`: [`wall_fit`] in one undo step. Returns `{fitted: [index]}`;
/// nothing to fit records no undo step.
pub fn wallfit(s: &mut Session, p: &Value) -> EngineResult<Value> {
    let plan = p.get("viewId").and_then(id_value).ok_or_else(|| other("No such view"))?;
    let only: Option<Vec<usize>> = match p.get("runs") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => {
            Some(a.iter().map(|v| v.as_u64().map(|n| n as usize).ok_or_else(|| other("runs must be run indices"))).collect::<EngineResult<_>>()?)
        }
        Some(_) => return Err(other("runs must be run indices")),
    };
    // Validate before `doc_mut`: touching the drawing on a refusal would still record an undo step.
    let moves = plan_moves(s.doc()?, plan, only.as_deref()).map_err(other)?;
    if moves.is_empty() {
        s.echo("No width conflicts".to_string());
        return Ok(json!({ "fitted": [] }));
    }
    let fitted = wall_fit(s.doc_mut()?, plan, only.as_deref()).map_err(other)?;
    s.echo(format!("{} runs fitted", fitted.len()));
    Ok(json!({ "fitted": fitted }))
}

#[cfg(test)]
mod tests {
    use crate::assembly::tests::{room_with, undo_len};
    use cadcraft_doc::EntityKind;
    use cadcraft_engine::Session;
    use lamb_core::model::{AnyView, Id};
    use serde_json::json;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    /// Vertex lists of every LWPOLYLINE in the plan's view block, in block order.
    fn plines(s: &Session, plan: Id) -> T<Vec<Vec<(f64, f64)>>> {
        let d = s.doc()?;
        let b = d.block(&crate::schema::view_block_name(plan)).ok_or("no view block")?;
        Ok(b.entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::LwPolyline(p) => Some(p.vertices.iter().map(|v| ((v.p.x * 1e6).round() / 1e6, (v.p.y * 1e6).round() / 1e6)).collect()),
                _ => None,
            })
            .collect())
    }

    fn conflict_runs(s: &mut Session, plan: Id) -> T<Vec<u64>> {
        let r = s.execute("wallconflicts", &json!({ "viewId": plan.0 }))?;
        Ok(r["conflicts"].as_array().ok_or("no conflicts")?.iter().filter_map(|c| c["run"].as_u64()).collect())
    }

    #[test]
    fn wallfit_moves_every_inner_face_to_the_details_width_in_one_undo_step() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 210.0)?;
        let before = plines(&s, plan)?;
        assert_eq!(before.len(), 2);
        let n = undo_len(&s)?;
        let r = s.execute("wallfit", &json!({ "viewId": plan.0 }))?;
        assert_eq!(r, json!({ "fitted": [0, 1, 2, 3] }));
        assert_eq!(undo_len(&s)?, n + 1);
        let after = plines(&s, plan)?;
        assert_eq!(after[0], before[0], "outer face unchanged");
        assert_eq!(after[1], [(200.0, 200.0), (4800.0, 200.0), (4800.0, 3800.0), (200.0, 3800.0)]);
        assert!(conflict_runs(&mut s, plan)?.is_empty());

        s.undo()?;
        assert_eq!(plines(&s, plan)?, before);
        assert_eq!(before[1], [(210.0, 210.0), (4790.0, 210.0), (4790.0, 3790.0), (210.0, 3790.0)]);
        Ok(())
    }

    #[test]
    fn wallfit_with_runs_moves_only_those_faces() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 210.0)?;
        let d = s.doc()?;
        let p = crate::project::DrawingProject::new(d)?;
        let (graph, _) = crate::detailtags::graph_and_sides(&p, plan);
        let bottom = graph
            .runs
            .iter()
            .position(|r| matches!(r.centreline, lamb_core::derivation::walls::RunCurve::Line { from, to } if from.y < 1000.0 && to.y < 1000.0))
            .ok_or("no bottom run")?;
        let r = s.execute("wallfit", &json!({ "viewId": plan.0, "runs": [bottom] }))?;
        assert_eq!(r, json!({ "fitted": [bottom] }));
        assert_eq!(plines(&s, plan)?[1], [(210.0, 200.0), (4790.0, 200.0), (4790.0, 3790.0), (210.0, 3790.0)]);
        // The bottom run is now 200 mm and fits its detail. Detail resolution runs per (exterior,
        // width) class, so the three 210 mm runs no longer share the bottom run's tag: they are
        // untyped, not conflicted. Their faces did not move (the vertices above).
        assert!(conflict_runs(&mut s, plan)?.is_empty());
        let runs = crate::assembly::assembly_lines(s.doc()?, plan)?;
        assert!(runs.iter().all(|r| r.run == bottom || r.skipped.as_deref() == Some("untyped")), "{runs:?}");
        Ok(())
    }

    #[test]
    fn wallfit_with_nothing_to_fit_or_a_bad_view_records_nothing() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        let n = undo_len(&s)?;
        assert_eq!(s.execute("wallfit", &json!({ "viewId": plan.0 }))?, json!({ "fitted": [] }));
        assert_eq!(undo_len(&s)?, n, "no undo step");

        let (mut s, plan) = room_with("exterior_face", true, 210.0)?;
        let before = plines(&s, plan)?;
        let n = undo_len(&s)?;
        let detail = crate::schema::read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id();
        let e = s.execute("wallfit", &json!({ "viewId": detail.0 })).err().ok_or("detail accepted")?;
        assert!(e.to_string().contains("Not a plan view"), "{e}");
        let e = s.execute("wallfit", &json!({ "viewId": 999_999 })).err().ok_or("unknown accepted")?;
        assert!(e.to_string().contains("No such view"), "{e}");
        assert_eq!(undo_len(&s)?, n);
        assert_eq!(plines(&s, plan)?, before);
        Ok(())
    }
}

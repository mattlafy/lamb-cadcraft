//! Assembly lines for a plan view (LAMB's `docs/07-architecture/cadcraft-integration.md` §10
//! "Generation", steps 1, 2 and 4, straight runs only; LAMB T369): for each wall run whose edge
//! detail resolves ([`resolve_view_details`]) to a detail with a face reference
//! ([`detail_reference`]), that detail's continuous bands laid along the run on its matching face
//! (lamb-core's [`detail_bands`] and [`lay_bands`]). Computed on demand by [`assembly_lines`];
//! `assemblyplan {viewId}` ([`write_assembly_block`], LAMB T371) writes them as LINEs into the
//! block [`assembly_block_name`]`(plan)`, replacing its contents, in one undo step. Each laid run
//! also carries its width conflict (lamb-core's [`width_conflict`], LAMB T375): reported by
//! ASSEMBLYPLAN and by the read-only `wallconflicts {viewId}`. Not the AI-line sweep in
//! [`crate::sweeps`].

use std::sync::Arc;

use cadcraft_doc::{Block, Common, Drawing, EntityKind, Line, Space};
use cadcraft_engine::{Result as EngineResult, Session};
use cadcraft_geom::Vec2;
use lamb_core::derivation::assembly::{AssemblyLine, FaceRef, WidthConflict, detail_bands, lay_bands, width_conflict};
use lamb_core::derivation::detail_tags::RunDetail;
use lamb_core::geometry::Point2;
use lamb_core::model::AnyView;
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::commands::{id_value, other};
use crate::convert::to_polyline;
use crate::detailref::{DetailRefKind, detail_reference};
use crate::detailtags::{graph_and_sides, resolve_view_details};
use crate::project::DrawingProject;
use crate::repeats::repeat_members;
use crate::schema::{assembly_block_name, read_document, view_block_name};

/// One wall run's assembly: the detail it resolved to (if any), the laid lines, and why it was
/// skipped when `lines` is empty for a reason.
#[derive(Clone, Debug, PartialEq)]
pub struct RunAssembly {
    /// Index into the plan's wall-graph runs.
    pub run: usize,
    pub detail: Option<Id>,
    pub lines: Vec<AssemblyLine>,
    pub skipped: Option<String>,
    /// The detail is thicker or thinner than the drawn wall; `None` when they agree or the run was
    /// skipped.
    pub conflict: Option<WidthConflict>,
}

/// Every entity in `detail`'s block except `reference` and repeat members ([`crate::repeats`]) that
/// is one straight open segment, with its layer.
fn detail_lines(d: &Drawing, detail: Id, reference: cadcraft_doc::Handle) -> Result<Vec<(Point2, Point2, String)>, String> {
    let Some(block) = d.block(&view_block_name(detail)) else { return Ok(Vec::new()) };
    let repeated = repeat_members(d, detail)?;
    Ok(block
        .entities
        .iter()
        .filter(|e| e.handle != reference && !repeated.contains(&e.handle))
        .filter_map(|e| {
            let poly = to_polyline(e).ok()?.polyline;
            match poly.segments.as_slice() {
                [seg] if !poly.closed && seg.bulge == 0.0 => Some((poly.start, seg.to, e.common.layer.clone())),
                _ => None,
            }
        })
        .collect())
}

/// One run's lines from `detail` and its width conflict, or why not.
fn run_lines(
    d: &Drawing,
    detail: Id,
    run: &lamb_core::derivation::walls::WallRun,
    sides: &lamb_core::derivation::wall_spaces::RunSides,
) -> Result<(Vec<AssemblyLine>, Option<WidthConflict>), String> {
    let r = detail_reference(d, detail)?.ok_or("detail has no reference line")?;
    let face = match r.kind {
        DetailRefKind::ExteriorFace => FaceRef::Exterior,
        DetailRefKind::InteriorFace => FaceRef::Interior,
        DetailRefKind::StructuralCentre | DetailRefKind::Grid => return Err("reference kind not laid yet".into()),
    };
    let bands = detail_bands((r.from, r.to), &detail_lines(d, detail, r.handle)?)?;
    Ok((lay_bands(run, sides, face, &bands)?, width_conflict(run, face, &bands)))
}

/// Every run of `plan`'s wall graph, in graph order, with its detail's bands laid along it.
pub fn assembly_lines(d: &Drawing, plan: Id) -> Result<Vec<RunAssembly>, String> {
    let (details, _) = resolve_view_details(d, plan)?;
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let (graph, sides) = graph_and_sides(&p, plan);
    Ok(graph
        .runs
        .iter()
        .zip(&sides.runs)
        .zip(&details)
        .enumerate()
        .map(|(i, ((run, side), resolved))| {
            let skip = |why: &str, detail: Option<Id>| RunAssembly { run: i, detail, lines: Vec::new(), skipped: Some(why.into()), conflict: None };
            let id = match resolved {
                RunDetail::Tagged(id) | RunDetail::Spread(id) | RunDetail::Gap(id) => id,
                RunDetail::Conflict => return skip("conflict", None),
                RunDetail::Untyped => return skip("untyped", None),
            };
            let Ok(detail) = id.parse::<u64>().map(Id) else { return skip("detail id is not a number", None) };
            match run_lines(d, detail, run, side) {
                Ok((lines, conflict)) => RunAssembly { run: i, detail: Some(detail), lines, skipped: None, conflict },
                Err(e) => skip(&e, Some(detail)),
            }
        })
        .collect())
}

/// `Ok` when `plan` names a plan view.
pub(crate) fn check_plan(d: &Drawing, plan: Id) -> Result<(), String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    match doc.views.iter().find(|v| v.id() == plan) {
        None => Err("No such view".into()),
        Some(AnyView::Plan(_)) => Ok(()),
        Some(_) => Err("Not a plan view".into()),
    }
}

/// Replaces the block [`assembly_block_name`]`(plan)` with one LINE per laid assembly line (an
/// empty block when every run is skipped). `plan` must be a plan view; nothing is touched on error.
pub fn write_assembly_block(d: &mut Drawing, plan: Id) -> Result<Vec<RunAssembly>, String> {
    check_plan(d, plan)?;
    let runs = assembly_lines(d, plan)?;
    let name = assembly_block_name(plan);
    d.blocks.insert(name.clone(), Arc::new(Block::new(&name)));
    let space = Space::Block(name);
    for l in runs.iter().flat_map(|r| &r.lines) {
        let common = Common { layer: l.layer.clone(), ..Common::default() };
        let line = Line { a: Vec2::new(l.from.x, l.from.y).to3(0.0), b: Vec2::new(l.to.x, l.to.y).to3(0.0) };
        d.add(&space, common, EntityKind::Line(line)).map_err(|e| e.to_string())?;
    }
    Ok(runs)
}

/// The runs' width conflicts as `[{run, delta, toward, text}]`, in run order.
fn conflicts_json(runs: &[RunAssembly]) -> Vec<Value> {
    runs.iter().filter_map(|r| r.conflict.map(|c| json!({ "run": r.run, "delta": c.delta, "toward": c.toward.name(), "text": c.text() }))).collect()
}

/// ASSEMBLYPLAN `{viewId}`: [`write_assembly_block`] for that plan. Returns
/// `{block, lines, skipped: [{run, reason}], conflicts: [{run, delta, toward, text}]}`.
pub fn assemblyplan(s: &mut Session, p: &Value) -> EngineResult<Value> {
    let plan = p.get("viewId").and_then(id_value).ok_or_else(|| other("No such view"))?;
    // Validate before `doc_mut`: touching the drawing on a refusal would still record an undo step.
    check_plan(s.doc()?, plan).map_err(other)?;
    let runs = write_assembly_block(s.doc_mut()?, plan).map_err(other)?;
    let lines: usize = runs.iter().map(|r| r.lines.len()).sum();
    let skipped: Vec<Value> = runs.iter().filter_map(|r| r.skipped.as_ref().map(|why| json!({ "run": r.run, "reason": why }))).collect();
    s.echo(format!("{lines} assembly lines, {} runs skipped", skipped.len()));
    Ok(json!({ "block": assembly_block_name(plan), "lines": lines, "skipped": skipped, "conflicts": conflicts_json(&runs) }))
}

/// WALLCONFLICTS `{viewId}`: the plan's width conflicts, computed without writing anything.
/// Returns `{conflicts: [{run, delta, toward, text}]}`.
pub fn wallconflicts(s: &mut Session, p: &Value) -> EngineResult<Value> {
    let plan = p.get("viewId").and_then(id_value).ok_or_else(|| other("No such view"))?;
    let d = s.doc()?;
    check_plan(d, plan).map_err(other)?;
    let conflicts = conflicts_json(&assembly_lines(d, plan).map_err(other)?);
    s.echo(format!("{} width conflicts", conflicts.len()));
    Ok(json!({ "conflicts": conflicts }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use cadcraft_doc::Handle;
    use cadcraft_engine::Session;
    use serde_json::json;
    use std::collections::HashSet;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    fn all_handles(s: &Session) -> T<HashSet<Handle>> {
        let d = s.doc()?;
        Ok(d.model.iter().map(|e| e.handle).chain(d.blocks.values().flat_map(|b| b.entities.iter().map(|e| e.handle))).collect())
    }

    fn line(s: &mut Session, a: [f64; 2], b: [f64; 2]) -> T<Handle> {
        let before = all_handles(s)?;
        s.execute("line", &json!({ "points": [a, b] }))?;
        Ok(all_handles(s)?.into_iter().find(|h| !before.contains(h)).ok_or("line not drawn")?)
    }

    /// `detailtags.rs`'s room: a plan with a closed 5000 × 4000 room of 200 mm walls on `WALL-EXT`
    /// (outer face 0..5000 × 0..4000) and a detail `D1` holding a reference (0,0)–(1000,0) of
    /// `kind` and bands at y = −20 (`CLAD`), 50 and 200 (`GYP`). With `tagged`, one edge tag on the
    /// bottom wall. Returns (session, plan).
    fn room(kind: &str, tagged: bool) -> T<(Session, Id)> {
        room_with(kind, tagged, 200.0)
    }

    /// [`room`] with the inner face `inner` mm inside the outer one (the wall `inner` thick).
    pub(crate) fn room_with(kind: &str, tagged: bool, inner: f64) -> T<(Session, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let detail = Id(s.execute("newdetail", &json!({ "name": "D1" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        let r = line(&mut s, [0.0, 0.0], [1000.0, 0.0])?;
        s.execute("detailref", &json!({ "handle": format!("{:X}", r.0), "kind": kind }))?;
        s.execute("layer.new", &json!({ "name": "CLAD", "current": true }))?;
        line(&mut s, [0.0, -20.0], [1000.0, -20.0])?;
        s.execute("layer.new", &json!({ "name": "GYP", "current": true }))?;
        line(&mut s, [0.0, 50.0], [1000.0, 50.0])?;
        line(&mut s, [0.0, 200.0], [1000.0, 200.0])?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [5000, 0], [5000, 4000], [0, 4000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[inner, inner], [5000.0 - inner, inner], [5000.0 - inner, 4000.0 - inner], [inner, 4000.0 - inner]], "closed": true }))?;
        if tagged {
            s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
            s.execute("detailtag", &json!({ "detailViewId": detail.0, "tip": {"x": 2500.0, "y": 0.0}, "bubble": {"x": 1500.0, "y": -1000.0} }))?;
        }
        Ok((s, plan))
    }

    fn horizontal(a: &RunAssembly) -> Option<Vec<(f64, String)>> {
        let mut ys: Vec<(f64, String)> = Vec::new();
        for l in &a.lines {
            if (l.from.y - l.to.y).abs() > 1e-9 {
                return None;
            }
            ys.push(((l.from.y * 1e6).round() / 1e6, l.layer.clone()));
        }
        Some(ys)
    }

    #[test]
    fn a_face_detail_lays_its_bands_along_every_run_from_the_exterior_face() -> R {
        let (s, plan) = room("exterior_face", true)?;
        let d = s.doc()?;
        let runs = assembly_lines(d, plan)?;
        assert_eq!(runs.len(), 4);
        assert!(runs.iter().all(|r| r.skipped.is_none() && r.lines.len() == 3 && r.detail.is_some()), "{runs:?}");

        let p = DrawingProject::new(d)?;
        let (graph, _) = graph_and_sides(&p, plan);
        let layers = |ys: &[(f64, String)]| ys.iter().map(|(_, l)| l.clone()).collect::<Vec<_>>();
        let mut bottom = None;
        let mut top = None;
        for r in &runs {
            let Some(ys) = horizontal(r) else { continue };
            let lamb_core::derivation::walls::RunCurve::Line { from, to } = graph.runs[r.run].centreline else { continue };
            for l in &r.lines {
                assert_eq!((l.from.x, l.to.x), (from.x, to.x), "spans the centreline's x extent");
            }
            if from.y < 2000.0 { bottom = Some(ys) } else { top = Some(ys) }
        }
        let (bottom, top) = (bottom.ok_or("no bottom run")?, top.ok_or("no top run")?);
        assert_eq!(bottom.iter().map(|y| y.0).collect::<Vec<_>>(), [-20.0, 50.0, 200.0]);
        assert_eq!(layers(&bottom), ["CLAD", "GYP", "GYP"]);
        assert_eq!(top.iter().map(|y| y.0).collect::<Vec<_>>(), [4020.0, 3950.0, 3800.0]);
        assert_eq!(layers(&top), ["CLAD", "GYP", "GYP"]);
        Ok(())
    }

    #[test]
    fn a_grid_reference_or_no_tag_skips_every_run() -> R {
        let (s, plan) = room("grid", true)?;
        let runs = assembly_lines(s.doc()?, plan)?;
        assert_eq!(runs.len(), 4);
        assert!(runs.iter().all(|r| r.lines.is_empty() && r.skipped.as_deref() == Some("reference kind not laid yet")), "{runs:?}");

        let (s, plan) = room("exterior_face", false)?;
        let runs = assembly_lines(s.doc()?, plan)?;
        assert_eq!(runs.len(), 4);
        assert!(runs.iter().all(|r| r.lines.is_empty() && r.detail.is_none() && r.skipped.as_deref() == Some("untyped")), "{runs:?}");
        Ok(())
    }

    fn block_lines(s: &Session, name: &str) -> T<Option<Vec<(f64, f64, String)>>> {
        let d = s.doc()?;
        let Some(b) = d.block(name) else { return Ok(None) };
        let mut v = Vec::new();
        for e in b.entities.iter() {
            let EntityKind::Line(l) = &e.kind else { return Err(format!("not a LINE: {:?}", e.kind).into()) };
            v.push((l.a.y, l.b.y, e.common.layer.clone()));
        }
        Ok(Some(v))
    }

    #[test]
    fn assemblyplan_writes_the_laid_lines_into_the_plans_assembly_block_replacing_it() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        let name = assembly_block_name(plan);
        let r = s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert_eq!(r, json!({ "block": name, "lines": 12, "skipped": [], "conflicts": [] }));
        let lines = block_lines(&s, &name)?.ok_or("no block")?;
        assert_eq!(lines.len(), 12);
        assert!(lines.iter().all(|(_, _, l)| l == "CLAD" || l == "GYP"), "{lines:?}");
        let mut bottom: Vec<f64> = lines.iter().filter(|(a, b, _)| a == b && *a < 1000.0).map(|(a, _, _)| (a * 1e6).round() / 1e6).collect();
        bottom.sort_by(f64::total_cmp);
        assert_eq!(bottom, [-20.0, 50.0, 200.0]);
        let views_before = read_document(s.doc()?)?.views.len();

        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert_eq!(block_lines(&s, &name)?.ok_or("no block")?.len(), 12, "replaced, not appended");
        assert_eq!(read_document(s.doc()?)?.views.len(), views_before, "not a view");

        s.undo()?;
        assert_eq!(block_lines(&s, &name)?.map(|v| v.len()), Some(12), "undo the second run");
        s.undo()?;
        assert_eq!(block_lines(&s, &name)?, None, "undo the first run");
        Ok(())
    }

    #[test]
    fn assemblyplan_refuses_a_detail_view_and_an_unknown_id_without_touching_the_drawing() -> R {
        let (mut s, _plan) = room("exterior_face", true)?;
        let detail = read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id();
        let blocks = s.doc()?.blocks.len();
        let e = s.execute("assemblyplan", &json!({ "viewId": detail.0 })).err().ok_or("detail accepted")?;
        assert!(e.to_string().contains("Not a plan view"), "{e}");
        let e = s.execute("assemblyplan", &json!({ "viewId": 999_999 })).err().ok_or("unknown accepted")?;
        assert!(e.to_string().contains("No such view"), "{e}");
        assert_eq!(s.doc()?.blocks.len(), blocks);
        assert!(s.doc()?.blocks.keys().all(|k| !k.starts_with("LAMB_ASSEMBLY_")));
        Ok(())
    }

    pub(crate) fn undo_len(s: &Session) -> T<usize> {
        Ok(s.state()?.undo.len())
    }

    #[test]
    fn a_wall_thicker_than_its_detail_reports_a_conflict_on_every_run() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 210.0)?;
        let n = undo_len(&s)?;
        let r = s.execute("wallconflicts", &json!({ "viewId": plan.0 }))?;
        assert_eq!(undo_len(&s)?, n, "read-only: no undo step");
        let c = r["conflicts"].as_array().ok_or("no conflicts")?;
        assert_eq!(c.len(), 4, "{r}");
        for x in c {
            assert!((x["delta"].as_f64().ok_or("no delta")? + 10.0).abs() < 1e-9, "{x}");
            assert_eq!(x["toward"], "interior");
            assert_eq!(x["text"], "-10.0mm toward interior");
        }
        let a = s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert_eq!(a["conflicts"], r["conflicts"]);

        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        assert_eq!(s.execute("wallconflicts", &json!({ "viewId": plan.0 }))?, json!({ "conflicts": [] }));
        Ok(())
    }

    #[test]
    fn wallconflicts_refuses_a_detail_view_and_an_unknown_id() -> R {
        let (mut s, _plan) = room("exterior_face", true)?;
        let detail = read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id();
        let n = undo_len(&s)?;
        let e = s.execute("wallconflicts", &json!({ "viewId": detail.0 })).err().ok_or("detail accepted")?;
        assert!(e.to_string().contains("Not a plan view"), "{e}");
        let e = s.execute("wallconflicts", &json!({ "viewId": 999_999 })).err().ok_or("unknown accepted")?;
        assert!(e.to_string().contains("No such view"), "{e}");
        assert_eq!(undo_len(&s)?, n);
        Ok(())
    }
}

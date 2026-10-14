//! Assembly lines for a plan view (LAMB's `docs/07-architecture/cadcraft-integration.md` §10
//! "Generation", steps 1, 2 and 4, straight runs only; LAMB T369): for each wall run whose edge
//! detail resolves ([`resolve_view_details`]) to a detail with a face reference
//! ([`detail_reference`]), that detail's continuous bands laid along the run on its matching face
//! (lamb-core's [`detail_bands`] and [`lay_bands`]). Computed on demand and returned; nothing is
//! written yet. Not the AI-line sweep in [`crate::sweeps`].

use cadcraft_doc::Drawing;
use lamb_core::derivation::assembly::{AssemblyLine, FaceRef, detail_bands, lay_bands};
use lamb_core::derivation::detail_tags::RunDetail;
use lamb_core::geometry::Point2;
use lamb_core::model::Id;

use crate::convert::to_polyline;
use crate::detailref::{DetailRefKind, detail_reference};
use crate::detailtags::{graph_and_sides, resolve_view_details};
use crate::project::DrawingProject;
use crate::schema::view_block_name;

/// One wall run's assembly: the detail it resolved to (if any), the laid lines, and why it was
/// skipped when `lines` is empty for a reason.
#[derive(Clone, Debug, PartialEq)]
pub struct RunAssembly {
    /// Index into the plan's wall-graph runs.
    pub run: usize,
    pub detail: Option<Id>,
    pub lines: Vec<AssemblyLine>,
    pub skipped: Option<String>,
}

/// Every entity in `detail`'s block except `reference` that is one straight open segment, with its
/// layer.
fn detail_lines(d: &Drawing, detail: Id, reference: cadcraft_doc::Handle) -> Vec<(Point2, Point2, String)> {
    let Some(block) = d.block(&view_block_name(detail)) else { return Vec::new() };
    block
        .entities
        .iter()
        .filter(|e| e.handle != reference)
        .filter_map(|e| {
            let poly = to_polyline(e).ok()?.polyline;
            match poly.segments.as_slice() {
                [seg] if !poly.closed && seg.bulge == 0.0 => Some((poly.start, seg.to, e.common.layer.clone())),
                _ => None,
            }
        })
        .collect()
}

/// One run's lines from `detail`, or why not.
fn run_lines(
    d: &Drawing,
    detail: Id,
    run: &lamb_core::derivation::walls::WallRun,
    sides: &lamb_core::derivation::wall_spaces::RunSides,
) -> Result<Vec<AssemblyLine>, String> {
    let r = detail_reference(d, detail)?.ok_or("detail has no reference line")?;
    let face = match r.kind {
        DetailRefKind::ExteriorFace => FaceRef::Exterior,
        DetailRefKind::InteriorFace => FaceRef::Interior,
        DetailRefKind::StructuralCentre | DetailRefKind::Grid => return Err("reference kind not laid yet".into()),
    };
    let bands = detail_bands((r.from, r.to), &detail_lines(d, detail, r.handle))?;
    lay_bands(run, sides, face, &bands)
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
            let skip = |why: &str, detail: Option<Id>| RunAssembly { run: i, detail, lines: Vec::new(), skipped: Some(why.into()) };
            let id = match resolved {
                RunDetail::Tagged(id) | RunDetail::Spread(id) | RunDetail::Gap(id) => id,
                RunDetail::Conflict => return skip("conflict", None),
                RunDetail::Untyped => return skip("untyped", None),
            };
            let Ok(detail) = id.parse::<u64>().map(Id) else { return skip("detail id is not a number", None) };
            match run_lines(d, detail, run, side) {
                Ok(lines) => RunAssembly { run: i, detail: Some(detail), lines, skipped: None },
                Err(e) => skip(&e, Some(detail)),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
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
        s.execute("pline", &json!({ "vertices": [[200, 200], [4800, 200], [4800, 3800], [200, 3800]], "closed": true }))?;
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
}

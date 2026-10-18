//! WALLRUN (LAMB `docs/07-architecture/cadcraft-integration.md` §10, picking maps back to source):
//! a point on a wall in a plan or section view names that wall run — its resolved detail, its two faces, its
//! width and its width conflict — read-only, no undo step.
//!
//! - `wallrun {viewId, point:{x,y}}` (View ▸ LAMB ▸ Wall Run): returns `{run, width,
//!   faces:[{entityId, segmentIndex}]×2, detail:{state, detailViewId?, name?}, skipped?,
//!   conflict?}`. `state` is `tagged|spread|gap|conflict|untyped`. Refuses a view that is not a plan
//!   or section and a point on no run (`No wall run there`).
//! - Interactive, on the edited plan or section: `Select a wall:` repeats; each pick echoes
//!   `Run <i>: <state> <name>, <width>mm wide` and haloes the run's two face entities until the
//!   command ends.

use cadcraft_doc::Handle;
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use lamb_core::derivation::detail_tags::RunDetail;
use lamb_core::derivation::wall_junctions::WallGraph;
use lamb_core::derivation::walls::RunCurve;
use lamb_core::geometry::Point2;
use lamb_core::geometry::point::{dist2, dot2, scale2, sub2};
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::Id;
use serde_json::{Map, Value, json};
use std::f64::consts::TAU;

use crate::assembly::{assembly_lines, check_wall_view};
use crate::commands::{id_value, other, point_param};
use crate::detailtags::{edited_wall_view, graph_and_sides, resolve_view_details};
use crate::overlays::halo::set_attention;
use crate::project::DrawingProject;
use crate::schema::{read_document, view_block_name};

fn add(a: Point2, b: Point2) -> Point2 {
    Point2 { x: a.x + b.x, y: a.y + b.y }
}

/// Distance from `p` to the segment `a`–`b`.
fn segment_distance(p: Point2, a: Point2, b: Point2) -> f64 {
    let ab = sub2(b, a);
    let len2 = dot2(ab, ab);
    let t = if len2 > 0.0 { (dot2(sub2(p, a), ab) / len2).clamp(0.0, 1.0) } else { 0.0 };
    dist2(p, add(a, scale2(ab, t)))
}

/// Distance from `p` to a run's centreline.
fn curve_distance(c: &RunCurve, p: Point2) -> f64 {
    match *c {
        RunCurve::Line { from, to } => segment_distance(p, from, to),
        RunCurve::Arc { center, radius, start_angle, sweep } => {
            let angle = (p.y - center.y).atan2(p.x - center.x);
            let off = if sweep >= 0.0 { (angle - start_angle).rem_euclid(TAU) } else { (start_angle - angle).rem_euclid(TAU) };
            if off <= sweep.abs() {
                (dist2(p, center) - radius).abs()
            } else {
                let end = |a: f64| Point2 { x: center.x + radius * a.cos(), y: center.y + radius * a.sin() };
                dist2(p, end(start_angle)).min(dist2(p, end(start_angle + sweep)))
            }
        }
    }
}

/// The run whose centreline lies within half its width (plus coincidence tolerance) of `p`; the
/// nearest wins, ties to the lower index.
pub(crate) fn run_at(graph: &WallGraph, p: Point2) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, run) in graph.runs.iter().enumerate() {
        let d = curve_distance(&run.centreline, p);
        if d > run.width / 2.0 + TOLERANCE.coincidence_mm {
            continue;
        }
        if best.is_none_or(|(_, b)| d < b) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// WALLRUN's JSON form.
pub(crate) fn wallrun(s: &mut Session, p: &Value) -> Result<Value> {
    let plan = p.get("viewId").and_then(id_value).ok_or_else(|| other("No such view"))?;
    let at = point_param("wallrun", p, "point")?;
    let d = s.doc()?;
    check_wall_view(d, plan).map_err(other)?;
    let dp = DrawingProject::new(d).map_err(other)?;
    let (graph, _) = graph_and_sides(&dp, plan);
    let i = run_at(&graph, at).ok_or_else(|| other("No wall run there"))?;
    let run = &graph.runs[i];
    let (details, _) = resolve_view_details(d, plan).map_err(other)?;
    let (state, detail_id) = match details.get(i) {
        Some(RunDetail::Tagged(id)) => ("tagged", Some(id.clone())),
        Some(RunDetail::Spread(id)) => ("spread", Some(id.clone())),
        Some(RunDetail::Gap(id)) => ("gap", Some(id.clone())),
        Some(RunDetail::Conflict) => ("conflict", None),
        _ => ("untyped", None),
    };
    let mut detail = Map::new();
    detail.insert("state".into(), json!(state));
    if let Some(id) = detail_id {
        let doc = read_document(d).map_err(other)?;
        if let Some(v) = id.parse::<u64>().ok().and_then(|n| doc.views.iter().find(|v| v.id() == Id(n))) {
            detail.insert("name".into(), json!(v.name()));
        }
        detail.insert("detailViewId".into(), id.parse::<u64>().map_or(json!(id), |n| json!(n)));
    }
    let faces: Vec<Value> = run.faces.iter().map(|f| json!({ "entityId": f.from.entity_id.0, "segmentIndex": f.from.segment_index })).collect();
    let mut out = json!({ "run": i, "width": run.width, "faces": faces, "detail": Value::Object(detail) });
    if let Some(a) = assembly_lines(d, plan).map_err(other)?.into_iter().find(|a| a.run == i) {
        if let Some(why) = a.skipped {
            out["skipped"] = json!(why);
        }
        if let Some(c) = a.conflict {
            out["conflict"] = json!(c.text());
        }
    }
    Ok(out)
}

/// `Run <i>: <state> <name>, <width>mm wide[; <conflict>][; skipped: <why>]`.
fn line(r: &Value) -> String {
    let mut name = r["detail"]["state"].as_str().unwrap_or_default().to_owned();
    if let Some(n) = r["detail"]["name"].as_str() {
        name = format!("{name} {n}");
    }
    let mut out = format!("Run {}: {name}, {:.0}mm wide", r["run"], r["width"].as_f64().unwrap_or_default());
    if let Some(c) = r["conflict"].as_str() {
        out += &format!("; {c}");
    }
    if let Some(w) = r["skipped"].as_str() {
        out += &format!("; skipped: {w}");
    }
    out
}

/// WALLRUN's interactive form: repeated point picks on the edited plan or section.
struct WallRunM {
    plan: Option<Id>,
}

impl Drop for WallRunM {
    fn drop(&mut self) {
        set_attention(None);
    }
}

impl Interactive for WallRunM {
    fn name(&self) -> &'static str {
        "wallrun"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Select a wall", Accept::POINT)
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        match edited_wall_view(s) {
            Ok(plan) => {
                self.plan = Some(plan);
                Ok(Step::Continue)
            }
            Err(e) => {
                s.echo(e);
                Ok(Step::Done)
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let Some(plan) = self.plan else { return Ok(Step::Done) };
        match i {
            Input::Point(p) => {
                match wallrun(s, &json!({ "viewId": plan.0, "point": {"x": p.x, "y": p.y} })) {
                    Ok(r) => {
                        s.echo(line(&r));
                        let handles = r["faces"].as_array().into_iter().flatten().filter_map(|f| f["entityId"].as_u64()).map(Handle).collect();
                        set_attention(Some((view_block_name(plan), handles)));
                    }
                    Err(e) => {
                        set_attention(None);
                        s.echo(e.to_string());
                    }
                }
                Ok(Step::Continue)
            }
            Input::Enter => Ok(Step::Done),
            Input::Cancel => Ok(Step::Cancel),
            _ => Ok(Step::Continue),
        }
    }
}

pub(crate) fn wallrun_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(WallRunM { plan: None }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assembly::tests::{d_room, room_with};
    use crate::overlays::halo::current_attention;
    use cadcraft_geom::Vec2;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;

    fn pick(s: &mut Session, plan: Id, x: f64, y: f64) -> Result<Value> {
        s.execute("wallrun", &json!({ "viewId": plan.0, "point": {"x": x, "y": y} }))
    }

    fn detail_id(s: &Session) -> std::result::Result<Id, Box<dyn std::error::Error>> {
        let doc = read_document(s.doc()?)?;
        Ok(doc.views.iter().find(|v| v.name() == "D1").ok_or("no D1")?.id())
    }

    #[test]
    fn a_point_on_the_bottom_wall_names_its_tagged_detail_and_conflict() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 210.0)?;
        let undo = s.state()?.undo.len();
        let d1 = detail_id(&s)?;
        let r = pick(&mut s, plan, 2500.0, 105.0)?;
        assert_eq!(r["detail"]["state"], json!("tagged"), "{r}");
        assert_eq!(r["detail"]["name"], json!("D1"));
        assert_eq!(r["detail"]["detailViewId"], json!(d1.0));
        assert!((r["width"].as_f64().ok_or("no width")? - 210.0).abs() < 1e-9);
        assert!(r["conflict"].as_str().ok_or("no conflict")?.starts_with("-10.0mm toward "), "{r}");
        assert_eq!(r["faces"].as_array().ok_or("no faces")?.len(), 2);
        let bottom = r["run"].as_u64().ok_or("no run")?;
        for (x, y) in [(105.0, 2000.0), (4895.0, 2000.0), (2500.0, 3895.0)] {
            let o = pick(&mut s, plan, x, y)?;
            assert_ne!(o["run"].as_u64(), Some(bottom));
            assert_eq!(o["detail"]["state"], json!("spread"), "{o}");
        }
        assert!(pick(&mut s, plan, 2500.0, 2000.0).is_err_and(|e| e.to_string().contains("No wall run there")));
        assert!(pick(&mut s, d1, 2500.0, 105.0).is_err_and(|e| e.to_string().contains("Not a plan or section view")));
        assert_eq!(s.state()?.undo.len(), undo);
        Ok(())
    }

    #[test]
    fn an_arc_run_is_picked_on_its_centreline_radius() -> R {
        let (mut s, plan) = d_room()?;
        let r = pick(&mut s, plan, 4000.0, 1100.0)?;
        let p = DrawingProject::new(s.doc()?)?;
        let (graph, _) = graph_and_sides(&p, plan);
        let i = r["run"].as_u64().ok_or("no run")? as usize;
        assert!(matches!(graph.runs[i].centreline, RunCurve::Arc { .. }), "{r}");
        assert!(pick(&mut s, plan, 4200.0, 1100.0).is_err_and(|e| e.to_string().contains("No wall run there")));
        Ok(())
    }

    #[test]
    fn the_interactive_form_echoes_haloes_and_clears() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        set_attention(None);
        let (mut s, plan) = room_with("exterior_face", true, 210.0)?;
        s.start("wallrun")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Select a wall:"));
        s.input(Input::Point(Vec2::new(2500.0, 105.0)))?;
        assert!(s.running.is_some());
        let echoed = s.log.iter().find(|l| l.starts_with("Run ")).ok_or("no echo")?;
        assert!(echoed.contains(": tagged D1, 210mm wide; -10.0mm toward "), "{echoed}");
        let (block, handles) = current_attention().ok_or("no halo")?;
        assert_eq!(block, view_block_name(plan));
        assert_eq!(handles.len(), 2);

        s.input(Input::Point(Vec2::new(2500.0, 2000.0)))?;
        assert!(s.running.is_some(), "a miss re-prompts");
        assert!(s.log.iter().any(|l| l.contains("No wall run there")), "{:?}", s.log);
        s.input(Input::Enter)?;
        assert!(s.running.is_none());
        assert_eq!(current_attention(), None);

        s.execute("editview", &json!({}))?;
        let undo = s.state()?.undo.len();
        s.start("wallrun")?;
        assert!(s.running.is_none());
        assert!(s.log.iter().any(|l| l == "Edit a plan or section view first (EDITVIEW)"), "{:?}", s.log);
        assert_eq!(s.state()?.undo.len(), undo);
        Ok(())
    }
}

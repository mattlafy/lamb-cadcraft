//! The project's stud grid (LAMB T458, T463; the stud/joist grid request): one CADCraft `Point` in
//! a placed plan view's block carrying [`EntityData::stud_grid`] (the pitch, mm) is the origin, in
//! project-global XY, that every repeat in every placed plan and every placed section lines up on.
//! The project has at most one; [`project_grid`] reads it (views in document order, plans with a
//! placement only; a grid point in a floating plan is ignored). [`stud_grid`] maps it into a view's
//! local space — a plan through its placement, a section through its cut line (u of the origin's
//! projection, z 0) — and [`run_grid`] decides per run: every run in a plan, and in a section only a
//! straight run parallel to the cut line (floors and ceilings: joists), never a wall cut in
//! section. [`crate::assembly`] passes it to lamb-core's `lay_repeat`, which lays a straight run's
//! repeat whose pitch is within [`PITCH_TOLERANCE_MM`] of the grid's at exactly the grid pitch, each
//! copy centred on a grid line perpendicular to the run through `origin + k·pitch`. No grid point:
//! repeats lay as before. MOVE moves the grid and the assembly observer re-lays every view's block
//! in that step. DXF/DWG see a plain point.
//!
//! - `studgrid {point:{x,y}, pitch?}` (View ▸ LAMB ▸ Stud Grid): the project's grid origin, at a
//!   point of the edited plan, pitch default 406.4 mm; erases every grid point in every view and
//!   adds the new one, one undo step; refused in a plan with no placement. Returns
//!   `{"handle": <hex>, "pitch": P}`. Interactive: `Specify stud grid origin or [Pitch] <406.4>:`,
//!   `Pitch` asking `Specify stud grid pitch <406.4>:` and then the origin again.
//! - `studgrid {point, pitch?, wall: true, off?}` (the `Wall` option, T460): a wall-scoped grid point
//!   ([`EntityData::stud_grid_wall`]) inside exactly one run's band of the edited plan overrules the
//!   project grid for that run only — `Phase` lays its repeats from the point at its own pitch,
//!   `Off` as if there were no grid; it replaces any wall point already inside that run, and a
//!   project STUDGRID leaves wall points alone. Interactive: `Wall` →
//!   `Specify stud grid origin on the wall or [Pitch/Off] <406.4>:`, `Off` →
//!   `Specify a point on the wall:`.

use cadcraft_doc::{Drawing, EntityKind, Handle, Point};
use cadcraft_engine::units::parse_distance;
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::Vec2;
use lamb_core::derivation::assembly::RepeatGrid;
use lamb_core::derivation::projection::{Uz, plan_global_to_local_xy, plan_local_to_global_xy, project_to_u, vertical_basis, vertical_uz_to_local};
use lamb_core::derivation::read::{ViewKind, ViewRef, view_ref};
use lamb_core::derivation::walls::{RunCurve, WallRun};
use lamb_core::geometry::Point2;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::commands::{bad, other, point_param};
use crate::detailtags::{edited_plan, graph_and_sides};
use crate::project::DrawingProject;
use crate::schema::{EntityData, LambDocument, WallGrid, read_document, read_entity_data, set_entity_data, view_block_name};

/// 16 inches.
pub(crate) const DEFAULT_PITCH_MM: f64 = 406.4;
/// How far a repeat's pitch may be from the grid's and still take the grid ("+/- a few mm").
pub(crate) const PITCH_TOLERANCE_MM: f64 = 5.0;

/// Every stud-grid point in `view`'s block, as (handle, origin, pitch, wall scope); malformed data
/// skipped.
fn grid_points(d: &Drawing, view: Id) -> Vec<(Handle, Point2, f64, Option<WallGrid>)> {
    let Some(block) = d.block(&view_block_name(view)) else { return Vec::new() };
    block
        .entities
        .iter()
        .filter_map(|e| {
            let EntityKind::Point(pt) = &e.kind else { return None };
            let data = read_entity_data(&e.common).ok()?;
            let pitch = data.stud_grid?;
            (pitch.is_finite() && pitch > 0.0).then_some((e.handle, Point2 { x: pt.p.x, y: pt.p.y }, pitch, data.stud_grid_wall))
        })
        .collect()
}

/// Whether `q` lies inside straight `run`'s band: along its centreline within its length, and no
/// further from it than half its width. Arc runs hold no point.
fn in_band(run: &WallRun, q: Point2) -> bool {
    let RunCurve::Line { from, to } = run.centreline else { return false };
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let len = dx.hypot(dy);
    if len <= 0.0 {
        return false;
    }
    let (ux, uy, qx, qy) = (dx / len, dy / len, q.x - from.x, q.y - from.y);
    let t = qx * ux + qy * uy;
    (0.0..=len).contains(&t) && (ux * qy - uy * qx).abs() <= run.width / 2.0 + TOLERANCE.coincidence_mm
}

fn find_view(doc: &LambDocument, id: Id) -> Option<ViewRef<'_>> {
    doc.views.iter().map(view_ref).find(|v| v.id == id)
}

/// The project's stud grid as (global origin, pitch): the first grid point of the first placed
/// plan, in document order, that has one.
pub(crate) fn project_grid(d: &Drawing) -> Option<(Point2, f64)> {
    project_grid_of(d, &read_document(d).ok()?)
}

fn project_grid_of(d: &Drawing, doc: &LambDocument) -> Option<(Point2, f64)> {
    doc.views.iter().map(view_ref).filter(|v| v.kind == ViewKind::Plan && v.transform().is_some()).find_map(|v| {
        let &(_, local, pitch, _) = grid_points(d, v.id).iter().find(|g| g.3.is_none())?;
        Some((plan_local_to_global_xy(&v, local).ok()?, pitch))
    })
}

/// The project's stud grid in `view`'s local space: a placed plan, or a placed vertical view whose
/// host resolves; `None` for anything else, or with no grid.
pub(crate) fn stud_grid(d: &Drawing, view: Id) -> Option<RepeatGrid> {
    let doc = read_document(d).ok()?;
    grid_in(d, &doc, &find_view(&doc, view)?)
}

fn grid_in(d: &Drawing, doc: &LambDocument, v: &ViewRef<'_>) -> Option<RepeatGrid> {
    let (g, pitch) = project_grid_of(d, doc)?;
    let origin = match v.kind {
        ViewKind::Plan => plan_global_to_local_xy(v, g).ok()?,
        ViewKind::Vertical => {
            let host = find_view(doc, v.host_view_id()?)?;
            let basis = vertical_basis(v, &host).ok()?;
            vertical_uz_to_local(v, Uz { u: project_to_u(&basis, g), z: 0.0 })
        }
        ViewKind::Detail => return None,
    };
    Some(RepeatGrid { origin, pitch, pitch_tolerance_mm: PITCH_TOLERANCE_MM })
}

/// The grid `run`'s repeats in `view` lay on: every run of a plan; in a vertical view only a
/// straight run parallel to the cut line's u axis (joists), never a wall cut in section.
pub(crate) fn run_grid(d: &Drawing, view: Id, run: &WallRun) -> Option<RepeatGrid> {
    let doc = read_document(d).ok()?;
    let v = find_view(&doc, view)?;
    if v.kind == ViewKind::Plan
        && let Some(&(_, origin, pitch, wall)) = grid_points(d, view).iter().find(|g| g.3.is_some() && in_band(run, g.1))
    {
        return (wall == Some(WallGrid::Phase)).then_some(RepeatGrid { origin, pitch, pitch_tolerance_mm: PITCH_TOLERANCE_MM });
    }
    if v.kind == ViewKind::Vertical {
        let RunCurve::Line { from, to } = run.centreline else { return None };
        let (a, b) = (vertical_uz_to_local(&v, Uz { u: 0.0, z: 0.0 }), vertical_uz_to_local(&v, Uz { u: 1.0, z: 0.0 }));
        let (ux, uy, rx, ry) = (b.x - a.x, b.y - a.y, to.x - from.x, to.y - from.y);
        let angle = (ux * ry - uy * rx).abs().atan2((ux * rx + uy * ry).abs()).to_degrees();
        if angle > TOLERANCE.angle_deg {
            return None;
        }
    }
    stud_grid(d, view)
}

/// STUDGRID's JSON form: the project's one grid point at `point` in the edited plan, pitch `pitch`
/// (406.4).
pub(crate) fn studgrid(s: &mut Session, p: &Value) -> Result<Value> {
    let at = point_param("studgrid", p, "point")?;
    let pitch = match p.get("pitch") {
        None | Some(Value::Null) => DEFAULT_PITCH_MM,
        Some(v) => v.as_f64().filter(|v| v.is_finite()).ok_or_else(|| bad("studgrid", "`pitch` must be a number"))?,
    };
    if pitch <= 0.0 {
        return Err(bad("studgrid", "`pitch` must be positive"));
    }
    let flag = |k: &str| -> Result<bool> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(false),
            Some(v) => v.as_bool().ok_or_else(|| bad("studgrid", &format!("`{k}` must be a boolean"))),
        }
    };
    let (wall, off) = (flag("wall")?, flag("off")?);
    if off && !wall {
        return Err(bad("studgrid", "`off` needs `wall`"));
    }
    // Every refusal before `doc_mut`, so a refusal records nothing.
    let view = edited_plan(s).map_err(other)?;
    let doc = read_document(s.doc()?).map_err(other)?;
    let scope = if wall { Some(if off { WallGrid::Off } else { WallGrid::Phase }) } else { None };
    let old: Vec<Handle> = if wall {
        let d = s.doc()?;
        let (graph, _) = graph_and_sides(&DrawingProject::new(d).map_err(other)?, view);
        let mut holding = graph.runs.iter().filter(|r| in_band(r, at));
        let run = match (holding.next(), holding.next()) {
            (None, _) => return Err(other("No wall there")),
            (Some(_), Some(_)) => return Err(other("That point is inside more than one wall; pick away from the corner")),
            (Some(r), None) => r,
        };
        grid_points(d, view).into_iter().filter(|g| g.3.is_some() && in_band(run, g.1)).map(|g| g.0).collect()
    } else {
        if find_view(&doc, view).and_then(|v| v.transform()).is_none() {
            return Err(other("This plan has no placement"));
        }
        let d = s.doc()?;
        doc.views.iter().flat_map(|v| grid_points(d, v.id())).filter(|g| g.3.is_none()).map(|g| g.0).collect()
    };
    let mut common = s.current_common();
    set_entity_data(&mut common, &EntityData { stud_grid: Some(pitch), stud_grid_wall: scope, ..EntityData::default() }).map_err(other)?;
    let space = s.space();
    let d = s.doc_mut()?;
    for h in old {
        d.remove_entity(h);
    }
    let h = d.add(&space, common, EntityKind::Point(Point { p: Vec2::new(at.x, at.y).to3(0.0), angle: 0.0 }))?;
    let mut out = json!({ "handle": format!("{:X}", h.0), "pitch": pitch });
    if let Some(w) = scope {
        out["wall"] = json!(if w == WallGrid::Off { "off" } else { "phase" });
    }
    Ok(out)
}

/// STUDGRID: an origin prompt with `Pitch` and `Wall` options; `Wall` (and then `Off`) scope the
/// point to the wall it lands in.
struct StudGridM {
    pitch: f64,
    asking_pitch: bool,
    wall: Option<WallGrid>,
}

fn fmt_mm(v: f64) -> String {
    format!("{}", (v * 1e6).round() / 1e6)
}

impl Interactive for StudGridM {
    fn name(&self) -> &'static str {
        "studgrid"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.asking_pitch {
            Prompt::new("Specify stud grid pitch", Accept::NUMBER).default(fmt_mm(self.pitch))
        } else {
            match self.wall {
                None => Prompt::new("Specify stud grid origin", Accept::POINT).kw(&["Pitch", "Wall"]).default(fmt_mm(self.pitch)),
                Some(WallGrid::Phase) => {
                    Prompt::new("Specify stud grid origin on the wall", Accept::POINT).kw(&["Pitch", "Off"]).default(fmt_mm(self.pitch))
                }
                Some(WallGrid::Off) => Prompt::new("Specify a point on the wall", Accept::POINT),
            }
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        match edited_plan(s) {
            Ok(_) => {
                if let Some((_, pitch)) = project_grid(s.doc()?) {
                    self.pitch = pitch;
                }
                Ok(Step::Continue)
            }
            Err(e) => {
                s.echo(e);
                Ok(Step::Done)
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if self.asking_pitch {
            match i {
                Input::Enter => self.asking_pitch = false,
                Input::Text(t) | Input::Keyword(t) => match parse_distance(&t).filter(|v| v.is_finite() && *v > 0.0) {
                    Some(v) => {
                        self.pitch = v;
                        self.asking_pitch = false;
                    }
                    None => s.echo("Requires a positive distance."),
                },
                Input::Cancel => return Ok(Step::Cancel),
                _ => {}
            }
            return Ok(Step::Continue);
        }
        match i {
            Input::Point(p) => {
                let mut args = json!({ "point": {"x": p.x, "y": p.y}, "pitch": self.pitch });
                if let Some(w) = self.wall {
                    args["wall"] = json!(true);
                    args["off"] = json!(w == WallGrid::Off);
                }
                if let Err(e) = studgrid(s, &args) {
                    s.echo(e.to_string());
                }
                Ok(Step::Done)
            }
            Input::Text(t) | Input::Keyword(t) => {
                match (self.wall, t.trim().to_ascii_lowercase().as_str()) {
                    (Some(WallGrid::Off), _) => {}
                    (_, "p" | "pitch") => self.asking_pitch = true,
                    (None, "w" | "wall") => self.wall = Some(WallGrid::Phase),
                    (Some(WallGrid::Phase), "o" | "off") => self.wall = Some(WallGrid::Off),
                    _ => {}
                }
                Ok(Step::Continue)
            }
            Input::Cancel | Input::Enter => Ok(Step::Cancel),
            _ => Ok(Step::Continue),
        }
    }
}

pub(crate) fn studgrid_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(StudGridM { pitch: DEFAULT_PITCH_MM, asking_pitch: false, wall: None }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::assembly::assembly_lines;
    use crate::assembly::tests::{room_with, undo_len};
    use crate::detailtags::graph_and_sides;
    use crate::project::DrawingProject;
    use crate::schema::{assembly_block_name, write_document};
    use lamb_core::geometry::{IDENTITY_TRANSFORM, Transform2D};
    use lamb_core::model::{AnyView, Placement};
    use std::collections::HashSet;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    fn handles(s: &Session) -> T<HashSet<Handle>> {
        let d = s.doc()?;
        Ok(d.model.iter().map(|e| e.handle).chain(d.blocks.values().flat_map(|b| b.entities.iter().map(|e| e.handle))).collect())
    }

    fn new_handle(s: &mut Session, cmd: &str, p: Value) -> T<Handle> {
        let before = handles(s)?;
        s.execute(cmd, &p)?;
        Ok(handles(s)?.into_iter().find(|h| !before.contains(h)).ok_or("nothing drawn")?)
    }

    /// Gives `plan` a plan placement with `t` (rewriting the document, as `newviews.rs`'s tests do).
    pub(crate) fn place(s: &mut Session, plan: Id, t: Transform2D) -> R {
        let d = s.doc_mut()?;
        let mut doc = read_document(d)?;
        for v in &mut doc.views {
            if let AnyView::Plan(pv) = v
                && pv.id == plan
            {
                pv.placement = Some(Placement::Plan { level_id: Id(0), cut_elevation: 1200.0, transform: t });
            }
        }
        write_document(d, &doc)?;
        Ok(())
    }

    const MOVED: Transform2D = Transform2D { origin_x: 1000.0, origin_y: 500.0, rotation: 0.25 };

    /// The assembly room (5000 × 4000, 200 mm walls, one edge tag spread round it) with a 38-wide
    /// stud repeated at `pitch` in its detail, editing the plan. Returns (session, plan).
    fn stud_room(pitch: f64) -> T<(Session, Id)> {
        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        place(&mut s, plan, IDENTITY_TRANSFORM)?;
        let detail = read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id();
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        s.execute("layer.new", &json!({ "name": "STUD", "current": true }))?;
        let stud = new_handle(&mut s, "pline", json!({ "vertices": [[100, 50], [138, 50], [138, 200], [100, 200]], "closed": true }))?;
        let dim = new_handle(&mut s, "dimaligned", json!({ "p1": [100, 0], "p2": [100.0 + pitch, 0], "at": [300, -300] }))?;
        s.execute("repeat", &json!({ "dimension": dim.hex(), "handles": [stud.hex()] }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "MARKS", "current": true }))?;
        Ok((s, plan))
    }

    /// The stud copies' along-centres (x) on the horizontal run nearest `y`, sorted.
    fn stud_xs(d: &Drawing, plan: Id, y: f64) -> T<Vec<f64>> {
        let (graph, _) = graph_and_sides(&DrawingProject::new(d)?, plan);
        let runs = assembly_lines(d, plan)?;
        let r = runs
            .iter()
            .find(
                |r| matches!(graph.runs[r.run].centreline, RunCurve::Line { from, to } if (from.y - to.y).abs() < 1e-9 && (from.y - y).abs() < 500.0),
            )
            .ok_or("no such run")?;
        let mut xs: Vec<f64> = r.lines[r.bands..]
            .chunks(4)
            .map(|c| {
                let xs = c.iter().flat_map(|l| [l.from.x, l.to.x]);
                let (lo, hi) = (xs.clone().fold(f64::INFINITY, f64::min), xs.fold(f64::NEG_INFINITY, f64::max));
                ((lo + hi) / 2.0 * 1e6).round() / 1e6
            })
            .collect();
        xs.sort_by(f64::total_cmp);
        Ok(xs)
    }

    #[test]
    fn studs_on_the_north_and_south_walls_share_the_grid_s_x_values() -> R {
        let (mut s, plan) = stud_room(406.4)?;
        let (south, north) = (stud_xs(s.doc()?, plan, 100.0)?, stud_xs(s.doc()?, plan, 3900.0)?);
        assert_ne!(south, north, "without a grid each run phases from its own start");
        s.execute("studgrid", &json!({ "point": {"x": 13.0, "y": 7.0} }))?;
        let (south, north) = (stud_xs(s.doc()?, plan, 100.0)?, stud_xs(s.doc()?, plan, 3900.0)?);
        assert!(!south.is_empty());
        assert_eq!(south, north);
        for x in &south {
            let m = (x - 13.0).rem_euclid(406.4);
            assert!(m < 1e-6 || 406.4 - m < 1e-6, "{x} off the grid");
        }
        Ok(())
    }

    #[test]
    fn moving_the_grid_point_relays_the_assembly_block_in_that_undo_step() -> R {
        let (mut s, plan) = stud_room(406.4)?;
        crate::maintain::install(&mut s);
        let h = s.execute("studgrid", &json!({ "point": {"x": 13.0, "y": 7.0} }))?["handle"].as_str().ok_or("no handle")?.to_owned();
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let block = |s: &Session| -> T<Vec<String>> {
            let b = s.doc()?.block(&assembly_block_name(plan)).ok_or("no block")?.clone();
            Ok(b.entities.iter().map(|e| format!("{:?}", e.kind)).collect())
        };
        let before = block(&s)?;
        let n = undo_len(&s)?;
        s.execute("move", &json!({ "handles": [h], "delta": [100, 0] }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        let moved = block(&s)?;
        assert_ne!(moved, before, "the block re-laid");
        let south = stud_xs(s.doc()?, plan, 100.0)?;
        assert!(south.iter().all(|x| {
            let m = (x - 113.0).rem_euclid(406.4);
            m < 1e-6 || 406.4 - m < 1e-6
        }));
        s.undo()?;
        assert_eq!(block(&s)?, before, "one undo restores the grid and the block");
        Ok(())
    }

    #[test]
    fn a_far_pitch_or_no_grid_lays_as_before() -> R {
        let (mut s, plan) = stud_room(600.0)?;
        let before = assembly_lines(s.doc()?, plan)?;
        s.execute("studgrid", &json!({ "point": {"x": 13.0, "y": 7.0} }))?;
        assert_eq!(assembly_lines(s.doc()?, plan)?, before, "600 is not within 5 mm of 406.4");
        Ok(())
    }

    #[test]
    fn studgrid_refuses_outside_a_plan_and_a_non_positive_pitch_and_keeps_one_point() -> R {
        let (mut s, plan) = stud_room(406.4)?;
        let n = undo_len(&s)?;
        assert!(
            s.execute("studgrid", &json!({ "point": {"x": 0, "y": 0}, "pitch": 0 }))
                .is_err_and(|e| e.to_string().contains("`pitch` must be positive"))
        );
        assert_eq!(undo_len(&s)?, n, "a bad pitch records nothing");
        let detail = read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id();
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        let m = undo_len(&s)?;
        assert!(s.execute("studgrid", &json!({ "point": {"x": 0, "y": 0} })).is_err());
        s.execute("editview", &json!({}))?;
        assert!(s.execute("studgrid", &json!({ "point": {"x": 0, "y": 0} })).is_err_and(|e| e.to_string().contains("Edit a plan view first")));
        assert_eq!(undo_len(&s)?, m, "refusals record nothing");
        assert!(stud_grid(s.doc()?, plan).is_none());
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("studgrid", &json!({ "point": {"x": 0, "y": 0} }))?;
        s.execute("studgrid", &json!({ "point": {"x": 50, "y": 60}, "pitch": 600 }))?;
        assert_eq!(grid_points(s.doc()?, plan).len(), 1);
        let g = stud_grid(s.doc()?, plan).ok_or("no grid")?;
        assert_eq!((g.origin, g.pitch), (Point2 { x: 50.0, y: 60.0 }, 600.0));
        Ok(())
    }

    #[test]
    fn the_interactive_form_takes_a_pitch_then_the_origin() -> R {
        let (mut s, plan) = stud_room(406.4)?;
        s.start("studgrid")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Specify stud grid origin or [Pitch/Wall] <406.4>:"));
        s.input(Input::Text("P".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Specify stud grid pitch <406.4>:"));
        s.input(Input::Text("600".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Specify stud grid origin or [Pitch/Wall] <600>:"));
        s.input(Input::Point(Vec2::new(13.0, 7.0)))?;
        assert!(s.running.is_none());
        let g = stud_grid(s.doc()?, plan).ok_or("no grid")?;
        assert_eq!((g.origin, g.pitch), (Point2 { x: 13.0, y: 7.0 }, 600.0));

        s.execute("editview", &json!({}))?;
        s.start("studgrid")?;
        assert!(s.running.is_none(), "refused at once");
        Ok(())
    }

    /// [`stud_room`] plus a second plan holding the same tagged room, placed at [`MOVED`]. Returns
    /// (session, first plan, second plan), editing the first.
    fn two_plans() -> T<(Session, Id, Id)> {
        let (mut s, plan) = stud_room(406.4)?;
        let detail = read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id();
        let second = Id(s.execute("newplan", &json!({ "name": "Upper" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": second.0 }))?;
        s.execute("layer.current", &json!({ "name": "WALL-EXT" }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [5000, 0], [5000, 4000], [0, 4000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [4800, 200], [4800, 3800], [200, 3800]], "closed": true }))?;
        s.execute("layer.current", &json!({ "name": "TAGS" }))?;
        s.execute("detailtag", &json!({ "detailViewId": detail.0, "tip": {"x": 2500.0, "y": 0.0}, "bubble": {"x": 1500.0, "y": -1000.0} }))?;
        place(&mut s, second, MOVED)?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        Ok((s, plan, second))
    }

    /// `view`'s south-wall stud centres (local y 100) in global XY.
    fn global_studs(d: &Drawing, view: Id) -> T<Vec<Point2>> {
        let doc = read_document(d)?;
        let v = find_view(&doc, view).ok_or("no view")?;
        stud_xs(d, view, 100.0)?.into_iter().map(|x| Ok(plan_local_to_global_xy(&v, Point2 { x, y: 100.0 })?)).collect()
    }

    fn on_grid(p: Point2, g: Point2, dir: Point2, pitch: f64) -> bool {
        let m = ((p.x - g.x) * dir.x + (p.y - g.y) * dir.y).rem_euclid(pitch);
        m < 1e-6 || pitch - m < 1e-6
    }

    #[test]
    fn one_grid_lines_up_the_studs_of_every_placed_plan_in_global_xy() -> R {
        let (mut s, plan, second) = two_plans()?;
        s.execute("studgrid", &json!({ "point": {"x": 13.0, "y": 7.0} }))?;
        let (g, pitch) = project_grid(s.doc()?).ok_or("no project grid")?;
        assert_eq!((g, pitch), (Point2 { x: 13.0, y: 7.0 }, 406.4));
        let first = global_studs(s.doc()?, plan)?;
        assert!(!first.is_empty());
        assert!(first.iter().all(|&p| on_grid(p, g, Point2 { x: 1.0, y: 0.0 }, pitch)), "{first:?}");
        let rotated = global_studs(s.doc()?, second)?;
        assert!(!rotated.is_empty());
        let dir = Point2 { x: MOVED.rotation.cos(), y: MOVED.rotation.sin() };
        assert!(rotated.iter().all(|&p| on_grid(p, g, dir, pitch)), "{rotated:?}");
        Ok(())
    }

    #[test]
    fn a_grid_set_in_another_plan_replaces_the_project_s_one() -> R {
        let (mut s, plan, second) = two_plans()?;
        s.execute("studgrid", &json!({ "point": {"x": 13.0, "y": 7.0} }))?;
        s.execute("editview", &json!({ "viewId": second.0 }))?;
        let n = undo_len(&s)?;
        s.execute("studgrid", &json!({ "point": {"x": 100.0, "y": 0.0}, "pitch": 400 }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        assert!(grid_points(s.doc()?, plan).is_empty(), "the first plan's point is gone");
        assert_eq!(grid_points(s.doc()?, second).len(), 1);
        let (g, pitch) = project_grid(s.doc()?).ok_or("no project grid")?;
        let want = Point2 { x: 1000.0 + 100.0 * MOVED.rotation.cos(), y: 500.0 + 100.0 * MOVED.rotation.sin() };
        assert!((g.x - want.x).abs() < 1e-9 && (g.y - want.y).abs() < 1e-9 && pitch == 400.0, "{g:?}");
        s.undo()?;
        assert_eq!((grid_points(s.doc()?, plan).len(), grid_points(s.doc()?, second).len()), (1, 0), "one undo step");
        Ok(())
    }

    #[test]
    fn studgrid_refuses_a_plan_with_no_placement_and_ignores_a_floating_plan_s_point() -> R {
        let (mut s, plan) = stud_room(406.4)?;
        let floating = Id(s.execute("newplan", &json!({ "name": "Loose" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": floating.0 }))?;
        let n = undo_len(&s)?;
        assert!(s.execute("studgrid", &json!({ "point": {"x": 0, "y": 0} })).is_err_and(|e| e.to_string().contains("This plan has no placement")));
        assert_eq!(undo_len(&s)?, n, "the refusal records nothing");
        // A grid point left in a plan that later lost its placement is not the project's grid.
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("studgrid", &json!({ "point": {"x": 0, "y": 0} }))?;
        let d = s.doc_mut()?;
        let mut doc = read_document(d)?;
        for v in &mut doc.views {
            if let AnyView::Plan(pv) = v {
                pv.placement = None;
            }
        }
        write_document(d, &doc)?;
        assert!(project_grid(s.doc()?).is_none());
        assert!(stud_grid(s.doc()?, plan).is_none());
        Ok(())
    }

    #[test]
    fn the_grid_tag_survives_dxf() -> R {
        let (mut s, plan) = stud_room(406.4)?;
        s.execute("studgrid", &json!({ "point": {"x": 13.0, "y": 7.0}, "pitch": 400 }))?;
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert_eq!(stud_grid(&back, plan), stud_grid(s.doc()?, plan));
        Ok(())
    }

    fn on_pitch(xs: &[f64], origin: f64, pitch: f64) -> bool {
        xs.iter().all(|x| {
            let m = (x - origin).rem_euclid(pitch);
            m < 1e-6 || pitch - m < 1e-6
        })
    }

    #[test]
    fn a_wall_point_rephases_or_drops_the_grid_for_its_run_only() -> R {
        let (mut s, plan) = stud_room(406.4)?;
        let free_north = stud_xs(s.doc()?, plan, 3900.0)?;
        s.execute("studgrid", &json!({ "point": {"x": 13.0, "y": 7.0} }))?;
        let (south, north) = (stud_xs(s.doc()?, plan, 100.0)?, stud_xs(s.doc()?, plan, 3900.0)?);
        assert!(on_pitch(&north, 13.0, 406.4));
        let at = 13.0 + 100.0 + 406.4 * 5.0;
        let r = s.execute("studgrid", &json!({ "point": {"x": at, "y": 3850.0}, "wall": true }))?;
        assert_eq!(r["wall"], "phase");
        let shifted = stud_xs(s.doc()?, plan, 3900.0)?;
        assert!(!shifted.is_empty() && shifted != north);
        assert!(on_pitch(&shifted, 113.0, 406.4), "{shifted:?}");
        assert_eq!(stud_xs(s.doc()?, plan, 100.0)?, south, "the south wall keeps the project grid");
        let r = s.execute("studgrid", &json!({ "point": {"x": 2500.0, "y": 3900.0}, "wall": true, "off": true }))?;
        assert_eq!(r["wall"], "off");
        assert_eq!(grid_points(s.doc()?, plan).iter().filter(|g| g.3.is_some()).count(), 1, "the Off point replaced the Phase one");
        assert_eq!(stud_xs(s.doc()?, plan, 3900.0)?, free_north, "Off lays the run as with no grid");
        assert_eq!(stud_xs(s.doc()?, plan, 100.0)?, south);
        Ok(())
    }

    #[test]
    fn a_wall_point_s_own_pitch_takes_a_repeat_the_project_grid_does_not() -> R {
        let (mut s, plan) = stud_room(600.0)?;
        let before_south = stud_xs(s.doc()?, plan, 100.0)?;
        s.execute("studgrid", &json!({ "point": {"x": 13.0, "y": 7.0} }))?;
        let at = 2145.0;
        s.execute("studgrid", &json!({ "point": {"x": at, "y": 3900.0}, "pitch": 600, "wall": true }))?;
        let north = stud_xs(s.doc()?, plan, 3900.0)?;
        assert!(!north.is_empty() && on_pitch(&north, at, 600.0), "{north:?}");
        assert_eq!(stud_xs(s.doc()?, plan, 100.0)?, before_south, "600 is still not within 5 mm of 406.4 elsewhere");
        Ok(())
    }

    #[test]
    fn wall_point_refusals_record_nothing_and_a_project_studgrid_keeps_wall_points() -> R {
        let (mut s, plan) = stud_room(406.4)?;
        let n = undo_len(&s)?;
        let refuse = |s: &mut Session, p: Value, msg: &str| {
            assert!(s.execute("studgrid", &p).is_err_and(|e| e.to_string().contains(msg)), "{p} should say {msg}");
        };
        refuse(&mut s, json!({ "point": {"x": 2500, "y": 2000}, "wall": true }), "No wall there");
        // Runs stop where the corner square starts, so the square itself is in no run's band and
        // only its edges (here the inner corner) are in two.
        refuse(&mut s, json!({ "point": {"x": 100, "y": 100}, "wall": true }), "No wall there");
        refuse(&mut s, json!({ "point": {"x": 200, "y": 200}, "wall": true }), "inside more than one wall");
        refuse(&mut s, json!({ "point": {"x": 2500, "y": 3900}, "off": true }), "`off` needs `wall`");
        let r = s.execute(
            "newsectionline",
            &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 2000}, "cutTo": {"x": 5000, "y": 2000} }),
        )?;
        let sec = Id(r["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        let m = undo_len(&s)?;
        assert!(m >= n);
        assert!(s.execute("studgrid", &json!({ "point": {"x": 100, "y": 1000}, "wall": true })).is_err(), "a section is refused");
        assert_eq!(undo_len(&s)?, m, "refusals record nothing");
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let m = undo_len(&s)?;
        refuse(&mut s, json!({ "point": {"x": 2500, "y": 2000}, "wall": true }), "No wall there");
        assert_eq!(undo_len(&s)?, m);
        s.execute("studgrid", &json!({ "point": {"x": 1000, "y": 3900}, "wall": true }))?;
        s.execute("studgrid", &json!({ "point": {"x": 3000, "y": 3900}, "wall": true }))?;
        s.execute("studgrid", &json!({ "point": {"x": 3000, "y": 100}, "wall": true }))?;
        let walls = |s: &Session| -> T<usize> { Ok(grid_points(s.doc()?, plan).iter().filter(|g| g.3.is_some()).count()) };
        assert_eq!(walls(&s)?, 2, "one per wall");
        s.execute("studgrid", &json!({ "point": {"x": 13, "y": 7} }))?;
        s.execute("studgrid", &json!({ "point": {"x": 20, "y": 7} }))?;
        assert_eq!(walls(&s)?, 2, "a project STUDGRID keeps wall points");
        assert_eq!(grid_points(s.doc()?, plan).len(), 3);
        assert_eq!(project_grid(s.doc()?), Some((Point2 { x: 20.0, y: 7.0 }, 406.4)));
        Ok(())
    }

    #[test]
    fn the_interactive_wall_option_places_phase_or_off() -> R {
        let (mut s, plan) = stud_room(406.4)?;
        let prompt = |s: &Session| s.current_prompt().map(|p| p.display());
        s.start("studgrid")?;
        s.input(Input::Text("W".into()))?;
        assert_eq!(prompt(&s).as_deref(), Some("Specify stud grid origin on the wall or [Pitch/Off] <406.4>:"));
        s.input(Input::Point(Vec2::new(2500.0, 3900.0)))?;
        assert!(s.running.is_none());
        let wall = |s: &Session| -> T<Vec<Option<WallGrid>>> { Ok(grid_points(s.doc()?, plan).iter().map(|g| g.3).collect()) };
        assert_eq!(wall(&s)?, vec![Some(WallGrid::Phase)]);
        s.start("studgrid")?;
        s.input(Input::Text("W".into()))?;
        s.input(Input::Text("O".into()))?;
        assert_eq!(prompt(&s).as_deref(), Some("Specify a point on the wall:"));
        s.input(Input::Point(Vec2::new(2000.0, 3900.0)))?;
        assert!(s.running.is_none());
        assert_eq!(wall(&s)?, vec![Some(WallGrid::Off)], "it replaced the Phase point on the same wall");
        Ok(())
    }

    #[test]
    fn the_wall_tag_survives_dxf() -> R {
        let (mut s, plan) = stud_room(406.4)?;
        s.execute("studgrid", &json!({ "point": {"x": 13.0, "y": 7.0} }))?;
        s.execute("studgrid", &json!({ "point": {"x": 2145.0, "y": 3900.0}, "pitch": 400, "wall": true }))?;
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        let strip = |d: &Drawing| grid_points(d, plan).into_iter().map(|g| (g.1, g.2, g.3)).collect::<Vec<_>>();
        assert_eq!(strip(&back), strip(s.doc()?));
        assert_eq!(stud_xs(&back, plan, 3900.0)?, stud_xs(s.doc()?, plan, 3900.0)?);
        Ok(())
    }
}

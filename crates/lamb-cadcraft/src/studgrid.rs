//! A plan's stud grid (LAMB T458; the stud/joist grid request): one CADCraft `Point` in a plan
//! view's block carrying [`EntityData::stud_grid`] (the pitch, mm) is the origin every repeat in
//! that plan's assembly lines up on. [`stud_grid`] reads it; [`crate::assembly`] passes it to
//! lamb-core's `lay_repeat`, which lays a straight run's repeat whose pitch is within
//! [`PITCH_TOLERANCE_MM`] of the grid's at exactly the grid pitch, each copy centred on a grid line
//! perpendicular to the run through `origin + k·pitch` — so studs on every wall, at every angle,
//! line up from the one origin. No grid point: repeats lay as before. MOVE moves the grid and the
//! assembly observer re-lays the block in that step. DXF/DWG see a plain point.
//!
//! - `studgrid {point:{x,y}, pitch?}` (View ▸ LAMB ▸ Stud Grid): the edited plan's grid origin,
//!   pitch default 406.4 mm; replaces any existing grid point in that plan, one undo step. Returns
//!   `{"handle": <hex>, "pitch": P}`. Interactive: `Specify stud grid origin or [Pitch] <406.4>:`,
//!   `Pitch` asking `Specify stud grid pitch <406.4>:` and then the origin again.

use cadcraft_doc::{Drawing, EntityKind, Handle, Point};
use cadcraft_engine::units::parse_distance;
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::Vec2;
use lamb_core::derivation::assembly::RepeatGrid;
use lamb_core::geometry::Point2;
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::commands::{bad, other, point_param};
use crate::detailtags::edited_plan;
use crate::schema::{EntityData, read_entity_data, set_entity_data, view_block_name};

/// 16 inches.
pub(crate) const DEFAULT_PITCH_MM: f64 = 406.4;
/// How far a repeat's pitch may be from the grid's and still take the grid ("+/- a few mm").
pub(crate) const PITCH_TOLERANCE_MM: f64 = 5.0;

/// Every stud-grid point in `view`'s block, as (handle, origin, pitch); malformed data skipped.
fn grid_points(d: &Drawing, view: Id) -> Vec<(Handle, Point2, f64)> {
    let Some(block) = d.block(&view_block_name(view)) else { return Vec::new() };
    block
        .entities
        .iter()
        .filter_map(|e| {
            let EntityKind::Point(pt) = &e.kind else { return None };
            let pitch = read_entity_data(&e.common).ok()?.stud_grid?;
            (pitch.is_finite() && pitch > 0.0).then_some((e.handle, Point2 { x: pt.p.x, y: pt.p.y }, pitch))
        })
        .collect()
}

/// `view`'s stud grid: its first tagged point in block order, or `None`.
pub(crate) fn stud_grid(d: &Drawing, view: Id) -> Option<RepeatGrid> {
    grid_points(d, view).first().map(|&(_, origin, pitch)| RepeatGrid { origin, pitch, pitch_tolerance_mm: PITCH_TOLERANCE_MM })
}

/// STUDGRID's JSON form: the edited plan's one grid point at `point`, pitch `pitch` (406.4).
pub(crate) fn studgrid(s: &mut Session, p: &Value) -> Result<Value> {
    let at = point_param("studgrid", p, "point")?;
    let pitch = match p.get("pitch") {
        None | Some(Value::Null) => DEFAULT_PITCH_MM,
        Some(v) => v.as_f64().filter(|v| v.is_finite()).ok_or_else(|| bad("studgrid", "`pitch` must be a number"))?,
    };
    if pitch <= 0.0 {
        return Err(bad("studgrid", "`pitch` must be positive"));
    }
    // Every refusal before `doc_mut`, so a refusal records nothing.
    let view = edited_plan(s).map_err(other)?;
    let old: Vec<Handle> = grid_points(s.doc()?, view).into_iter().map(|(h, _, _)| h).collect();
    let mut common = s.current_common();
    set_entity_data(&mut common, &EntityData { stud_grid: Some(pitch), ..EntityData::default() }).map_err(other)?;
    let space = s.space();
    let d = s.doc_mut()?;
    for h in old {
        d.remove_entity(h);
    }
    let h = d.add(&space, common, EntityKind::Point(Point { p: Vec2::new(at.x, at.y).to3(0.0), angle: 0.0 }))?;
    Ok(json!({ "handle": format!("{:X}", h.0), "pitch": pitch }))
}

/// STUDGRID: an origin prompt with a `Pitch` option.
struct StudGridM {
    pitch: f64,
    asking_pitch: bool,
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
            Prompt::new("Specify stud grid origin", Accept::POINT).kw(&["Pitch"]).default(fmt_mm(self.pitch))
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        match edited_plan(s) {
            Ok(view) => {
                if let Some(g) = stud_grid(s.doc()?, view) {
                    self.pitch = g.pitch;
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
                if let Err(e) = studgrid(s, &json!({ "point": {"x": p.x, "y": p.y}, "pitch": self.pitch })) {
                    s.echo(e.to_string());
                }
                Ok(Step::Done)
            }
            Input::Text(t) | Input::Keyword(t) if matches!(t.trim().to_ascii_lowercase().as_str(), "p" | "pitch") => {
                self.asking_pitch = true;
                Ok(Step::Continue)
            }
            Input::Cancel | Input::Enter => Ok(Step::Cancel),
            _ => Ok(Step::Continue),
        }
    }
}

pub(crate) fn studgrid_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(StudGridM { pitch: DEFAULT_PITCH_MM, asking_pitch: false }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assembly::assembly_lines;
    use crate::assembly::tests::{room_with, undo_len};
    use crate::detailtags::graph_and_sides;
    use crate::project::DrawingProject;
    use crate::schema::{assembly_block_name, read_document};
    use lamb_core::derivation::walls::RunCurve;
    use lamb_core::model::AnyView;
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

    /// The assembly room (5000 × 4000, 200 mm walls, one edge tag spread round it) with a 38-wide
    /// stud repeated at `pitch` in its detail, editing the plan. Returns (session, plan).
    fn stud_room(pitch: f64) -> T<(Session, Id)> {
        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
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
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Specify stud grid origin or [Pitch] <406.4>:"));
        s.input(Input::Text("P".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Specify stud grid pitch <406.4>:"));
        s.input(Input::Text("600".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Specify stud grid origin or [Pitch] <600>:"));
        s.input(Input::Point(Vec2::new(13.0, 7.0)))?;
        assert!(s.running.is_none());
        let g = stud_grid(s.doc()?, plan).ok_or("no grid")?;
        assert_eq!((g.origin, g.pitch), (Point2 { x: 13.0, y: 7.0 }, 600.0));

        s.execute("editview", &json!({}))?;
        s.start("studgrid")?;
        assert!(s.running.is_none(), "refused at once");
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
}

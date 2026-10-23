//! LEVELLINE (LAMB T497): move a level by picking its level line in a placed section or elevation,
//! then placing it — `docs/02-data-model/views-and-levels.md` "Link elevation lines" (editing a
//! level's floor elevation means moving that line), as CUTLINE's click-click idiom.
//!
//! - `levelline {levelId, elevation}` (`lvl`, View ▸ LAMB ▸ Level Line): a thin wrapper over LEVEL
//!   `{mode:"elevation"}` — its refusals and its undo step are LEVEL's, so every placed plan on the
//!   level moves with it.
//! - Interactive, only while editing a placed vertical view: `Select level line:` (the level whose
//!   line's local Y is nearest the point's Y, within the pick aperture; X is ignored), then
//!   `Specify new floor elevation:` — a point (its Y through the view) or a typed absolute
//!   elevation in mm. A horizontal preview line follows the cursor at step 2.

use cadcraft_doc::{EntityKind, Line};
use cadcraft_engine::cmd::curves::pick_aperture;
use cadcraft_engine::units::parse_distance;
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::Vec2;
use lamb_core::derivation::projection::{Uz, vertical_local_to_uz, vertical_uz_to_local};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::geometry::Point2;
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::commands::{bad, edited_view, id_value, other};
use crate::newviews::{CUT_LINE_LENGTH_MM, is_placed_vertical};
use crate::project::DrawingProject;

const CMD: &str = "levelline";

/// LEVELLINE `{levelId, elevation}`: the level's floor elevation becomes `elevation` (absolute mm).
pub(crate) fn levelline(s: &mut Session, p: &Value) -> Result<Value> {
    let level = p.get("levelId").and_then(id_value).ok_or_else(|| bad(CMD, "`levelId` is required"))?;
    let z = p.get("elevation").and_then(Value::as_f64).filter(|v| v.is_finite()).ok_or_else(|| bad(CMD, "`elevation` must be a finite number"))?;
    crate::viewlife::level(s, &json!({ "mode": "elevation", "levelId": level.0, "floorElevationMm": z }))
}

/// The level whose line in vertical view `view` is nearest `p` by local Y, within `tolerance`;
/// ties go to the lower `order`, then the lower id. Returns (id, name).
fn hit_level_line(s: &Session, view: Id, p: Vec2, tolerance: f64) -> Option<(Id, String)> {
    let proj = DrawingProject::new(s.doc().ok()?).ok()?;
    let v = proj.view(view)?;
    proj.document()
        .levels
        .iter()
        .map(|l| ((vertical_uz_to_local(&v, Uz { u: 0.0, z: l.floor_elevation.value }).y - p.y).abs(), l))
        .filter(|(d, _)| *d <= tolerance)
        .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.order.total_cmp(&b.1.order)).then(a.1.id.0.cmp(&b.1.id.0)))
        .map(|(_, l)| (l.id, l.name.clone()))
}

/// The absolute elevation of local Y `y` in the vertical view.
fn elevation_at(s: &Session, view: Id, y: f64) -> Result<f64> {
    let proj = DrawingProject::new(s.doc()?).map_err(other)?;
    let v = proj.view(view).ok_or_else(|| other("No such view"))?;
    Ok(vertical_local_to_uz(&v, Point2 { x: 0.0, y }).z)
}

struct LevelLine {
    view: Option<Id>,
    picked: Option<(Id, String)>,
}

impl Interactive for LevelLine {
    fn name(&self) -> &'static str {
        CMD
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.picked {
            None => Prompt::new("Select level line", Accept::POINT),
            Some(_) => Prompt::new("Specify new floor elevation", Accept::POINT_OR_NUMBER),
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.view = edited_view(s).filter(|&id| is_placed_vertical(s, id));
        if self.view.is_none() {
            s.echo("Edit a placed section or elevation first (EDITVIEW).");
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel | Input::Enter) {
            return Ok(Step::Cancel);
        }
        let Some(view) = self.view else { return Ok(Step::Done) };
        let Some((level, name)) = self.picked.clone() else {
            let Input::Point(p) = i else { return Ok(Step::Continue) };
            match hit_level_line(s, view, p, pick_aperture(s)) {
                Some(hit) => self.picked = Some(hit),
                None => s.echo("No level line there."),
            }
            return Ok(Step::Continue);
        };
        let z = match i {
            Input::Point(p) => elevation_at(s, view, p.y)?,
            Input::Text(t) | Input::Keyword(t) => parse_distance(&t).filter(|v| v.is_finite()).ok_or_else(|| other("Requires a number."))?,
            _ => return Ok(Step::Continue),
        };
        levelline(s, &json!({ "levelId": level.0, "elevation": z }))?;
        s.echo(format!("{name} at {z} mm"));
        Ok(Step::Done)
    }
    fn preview(&self, _s: &Session, cursor: Vec2) -> Vec<EntityKind> {
        if self.picked.is_none() {
            return Vec::new();
        }
        let h = CUT_LINE_LENGTH_MM / 2.0;
        vec![EntityKind::Line(Line { a: Vec2::new(cursor.x - h, cursor.y).to3(0.0), b: Vec2::new(cursor.x + h, cursor.y).to3(0.0) })]
    }
}

pub(crate) fn levelline_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(LevelLine { view: None, picked: None }))
}

#[cfg(test)]
mod tests {
    //! Headless on LAMBPROJECT with section `A-A`, a second level at 3000 and a plan placed on it;
    //! the registry is process-global, so every test installs first.
    use super::*;
    use crate::schema::{annotation_block_name, read_document};
    use lamb_core::model::{AnyView, Placement, PlanView};

    type E<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    struct Fx {
        s: Session,
        plan: Id,
        upper: Id,
        sec: Id,
        l2: Id,
    }

    /// LAMBPROJECT's ground-floor plan (Level 1, floor 0), section `A-A` cut in it, Level 2 at
    /// 3000 with a duplicate plan placed on it (cut 4200), and the section being edited.
    fn fixture() -> E<Fx> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let r =
            s.execute("newsectionline", &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 0}, "cutTo": {"x": 5000, "y": 0} }))?;
        let sec = Id(r["viewId"].as_u64().ok_or("no id")?);
        let l2 =
            Id(s.execute("level", &json!({ "mode": "create", "name": "Level 2", "floorElevationMm": 3000 }))?["levelId"].as_u64().ok_or("no id")?);
        let upper = Id(s.execute("duplicateview", &json!({ "viewId": plan.0 }))?["viewId"].as_u64().ok_or("no id")?);
        let t = json!({ "originX": 0.0, "originY": 0.0, "rotation": 0.0 });
        s.execute("placeview", &json!({ "viewId": upper.0, "kind": "plan", "levelId": l2.0, "cutElevation": 4200, "transform": t }))?;
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [4000, 0], [4000, 3000], [0, 3000]], "closed": true }))?;
        Ok(Fx { s, plan, upper, sec, l2 })
    }

    fn floor_of(s: &Session, level: Id) -> E<f64> {
        Ok(read_document(s.doc()?)?.levels.iter().find(|l| l.id == level).ok_or("no level")?.floor_elevation.value)
    }

    fn cut_of(s: &Session, plan: Id) -> E<f64> {
        match read_document(s.doc()?)?.views.into_iter().find(|v| v.id() == plan) {
            Some(AnyView::Plan(PlanView { placement: Some(Placement::Plan { cut_elevation, .. }), .. })) => Ok(cut_elevation),
            _ => Err("not a placed plan".into()),
        }
    }

    fn local_y(s: &Session, view: Id, z: f64) -> E<f64> {
        let dp = DrawingProject::new(s.doc()?)?;
        let v = dp.view(view).ok_or("no view")?;
        Ok(vertical_uz_to_local(&v, Uz { u: 0.0, z }).y)
    }

    /// The Y of every LINE in the view's annotation block (its level lines).
    fn level_line_ys(s: &Session, view: Id) -> E<Vec<f64>> {
        let b = s.doc()?.blocks.get(&annotation_block_name(view)).cloned().ok_or("no annotation block")?;
        Ok(b.entities.iter().filter_map(|e| if let EntityKind::Line(l) = &e.kind { Some(l.a.y) } else { None }).collect())
    }

    fn prompt(s: &Session) -> Option<String> {
        s.current_prompt().map(|p| p.message)
    }

    #[test]
    fn pick_then_place_moves_the_level_its_plans_and_its_line_and_one_undo_restores_it() -> E<()> {
        let Fx { mut s, plan, upper, sec, l2 } = fixture()?;
        let (lower_cut, upper_cut) = (cut_of(&s, plan)?, cut_of(&s, upper)?);
        let y2 = local_y(&s, sec, 3000.0)?;
        assert!(level_line_ys(&s, sec)?.iter().any(|y| (y - y2).abs() < 1e-6), "Level 2's line drawn");
        let ap = pick_aperture(&s);

        s.start("lvl")?;
        assert_eq!(prompt(&s).as_deref(), Some("Select level line"));
        s.input(Input::Point(Vec2::new(123.0, y2 + 1000.0)))?;
        assert!(s.log.iter().any(|l| l == "No level line there."), "{:?}", s.log);
        assert_eq!(prompt(&s).as_deref(), Some("Select level line"), "still on step 1");
        assert_eq!(floor_of(&s, l2)?, 3000.0, "nothing changed");

        s.input(Input::Point(Vec2::new(-9999.0, y2 + 0.4 * ap)))?;
        assert_eq!(prompt(&s).as_deref(), Some("Specify new floor elevation"));
        let pv = s.running.as_ref().ok_or("not running")?.machine.preview(&s, Vec2::new(100.0, 777.0));
        let h = CUT_LINE_LENGTH_MM / 2.0;
        assert_eq!(pv, vec![EntityKind::Line(Line { a: Vec2::new(100.0 - h, 777.0).to3(0.0), b: Vec2::new(100.0 + h, 777.0).to3(0.0) })]);
        s.input(Input::Point(Vec2::new(500.0, local_y(&s, sec, 3300.0)?)))?;
        assert!(s.running.is_none(), "done");
        assert!((floor_of(&s, l2)? - 3300.0).abs() < 1e-9, "{}", floor_of(&s, l2)?);
        assert!((cut_of(&s, upper)? - (upper_cut + 300.0)).abs() < 1e-9, "the plan on Level 2 moved with it");
        assert_eq!(cut_of(&s, plan)?, lower_cut, "Level 1's plan stays");
        assert!(s.log.iter().any(|l| l.starts_with("Level 2 at ")), "{:?}", s.log);
        let y3 = local_y(&s, sec, 3300.0)?;
        let ys = level_line_ys(&s, sec)?;
        assert!(ys.iter().any(|y| (y - y3).abs() < 1e-6) && !ys.iter().any(|y| (y - y2).abs() < 1e-6), "line regenerated: {ys:?}");

        s.undo()?;
        assert_eq!(floor_of(&s, l2)?.to_bits(), 3000f64.to_bits(), "one undo restores it");
        assert_eq!(cut_of(&s, upper)?.to_bits(), upper_cut.to_bits());
        assert!(level_line_ys(&s, sec)?.iter().any(|y| (y - y2).abs() < 1e-6));
        Ok(())
    }

    #[test]
    fn a_typed_number_is_an_absolute_elevation_and_the_backend_matches() -> E<()> {
        let Fx { mut s, upper, sec, l2, .. } = fixture()?;
        s.start("levelline")?;
        s.input(Input::Point(Vec2::new(0.0, local_y(&s, sec, 3000.0)?)))?;
        s.input(Input::Text("3500".into()))?;
        let (typed_floor, typed_cut) = (floor_of(&s, l2)?, cut_of(&s, upper)?);
        assert_eq!(typed_floor, 3500.0);
        s.undo()?;
        s.execute("levelline", &json!({ "levelId": l2.0, "elevation": 3500 }))?;
        assert_eq!((floor_of(&s, l2)?, cut_of(&s, upper)?), (typed_floor, typed_cut));
        Ok(())
    }

    #[test]
    fn placing_back_on_its_own_line_is_bit_identical() -> E<()> {
        let Fx { mut s, sec, l2, .. } = fixture()?;
        let y2 = local_y(&s, sec, 3000.0)?;
        s.start("lvl")?;
        s.input(Input::Point(Vec2::new(0.0, y2)))?;
        s.input(Input::Point(Vec2::new(4000.0, y2)))?;
        assert_eq!(floor_of(&s, l2)?.to_bits(), 3000f64.to_bits());
        Ok(())
    }

    #[test]
    fn refused_while_editing_a_plan() -> E<()> {
        let Fx { mut s, plan, l2, .. } = fixture()?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let undo = s.state()?.undo.len();
        s.start("levelline")?;
        assert!(s.running.is_none());
        assert!(s.log.iter().any(|l| l == "Edit a placed section or elevation first (EDITVIEW)."), "{:?}", s.log);
        assert_eq!((floor_of(&s, l2)?, s.state()?.undo.len()), (3000.0, undo), "nothing changed");
        Ok(())
    }
}

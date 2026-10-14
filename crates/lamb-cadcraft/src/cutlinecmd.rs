//! CUTLINE (LAMB T358): move a placed plan's cut elevation by picking its cut line in a placed
//! section or elevation, then placing it — the TS app's drag of a plan cut line
//! (`apps/desktop/src/viewport2d/planCutLineDrag.ts`, T67) as CADCraft's click-click idiom.
//!
//! - `cutline {planViewId, elevation}` (`cl`, View ▸ LAMB ▸ Cut Line): a thin wrapper over
//!   VIEWPROPS `{mode:"cutElevation"}` — its refusals and its undo step are VIEWPROPS's.
//! - Interactive, only while editing a placed vertical view: `Select plan cut line:` (nearest
//!   overlay line by local Y within the pick aperture, TS `hitPlanCutLine`), then `Specify new cut
//!   height:` — a point (its Y through the view, TS `elevationForDrag`) or a typed absolute
//!   elevation in mm. A horizontal preview line follows the cursor at step 2.

use cadcraft_doc::{EntityKind, Line};
use cadcraft_engine::cmd::curves::pick_aperture;
use cadcraft_engine::units::parse_distance;
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::Vec2;
use lamb_core::derivation::markers::{PlanCutLineOverlay, compute_plan_cut_lines_for_view};
use lamb_core::derivation::projection::vertical_local_to_uz;
use lamb_core::derivation::read::ProjectRead;
use lamb_core::geometry::Point2;
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::commands::{bad, edited_view, id_value, other};
use crate::newviews::{CUT_LINE_LENGTH_MM, is_placed_vertical};
use crate::project::DrawingProject;

const CMD: &str = "cutline";

/// CUTLINE `{planViewId, elevation}`: the plan's cut elevation becomes `elevation` (absolute mm).
pub(crate) fn cutline(s: &mut Session, p: &Value) -> Result<Value> {
    let plan = p.get("planViewId").and_then(id_value).ok_or_else(|| bad(CMD, "`planViewId` is required"))?;
    let z = p.get("elevation").and_then(Value::as_f64).filter(|v| v.is_finite()).ok_or_else(|| bad(CMD, "`elevation` must be a finite number"))?;
    crate::viewprops::viewprops(s, &json!({ "mode": "cutElevation", "viewId": plan.0, "cutElevation": z }))
}

/// TS `hitPlanCutLine`: the nearest plan cut line to `p` by local Y, within `tolerance`.
fn hit_plan_cut_line(s: &Session, view: Id, p: Vec2, tolerance: f64) -> Option<PlanCutLineOverlay> {
    let proj = DrawingProject::new(s.doc().ok()?).ok()?;
    compute_plan_cut_lines_for_view(&proj, view)
        .into_iter()
        .map(|o| ((o.local_y - p.y).abs(), o))
        .filter(|(d, _)| *d <= tolerance)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, o)| o)
}

/// TS `elevationForDrag`: the absolute elevation of local Y `y` in the vertical view.
fn elevation_at(s: &Session, view: Id, y: f64) -> Result<f64> {
    let proj = DrawingProject::new(s.doc()?).map_err(other)?;
    let v = proj.view(view).ok_or_else(|| other("No such view"))?;
    Ok(vertical_local_to_uz(&v, Point2 { x: 0.0, y }).z)
}

struct CutLine {
    view: Option<Id>,
    picked: Option<PlanCutLineOverlay>,
}

impl Interactive for CutLine {
    fn name(&self) -> &'static str {
        CMD
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.picked {
            None => Prompt::new("Select plan cut line", Accept::POINT),
            Some(_) => Prompt::new("Specify new cut height", Accept::POINT_OR_NUMBER),
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
        let Some(line) = &self.picked else {
            let Input::Point(p) = i else { return Ok(Step::Continue) };
            match hit_plan_cut_line(s, view, p, pick_aperture(s)) {
                Some(o) => self.picked = Some(o),
                None => s.echo("No plan cut line there."),
            }
            return Ok(Step::Continue);
        };
        let z = match i {
            Input::Point(p) => elevation_at(s, view, p.y)?,
            Input::Text(t) | Input::Keyword(t) => parse_distance(&t).filter(|v| v.is_finite()).ok_or_else(|| other("Requires a number."))?,
            _ => return Ok(Step::Continue),
        };
        let name = line.name.clone();
        cutline(s, &json!({ "planViewId": line.source_view_id.0, "elevation": z }))?;
        s.echo(format!("{name} cut at {z} mm"));
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

pub(crate) fn cutline_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(CutLine { view: None, picked: None }))
}

#[cfg(test)]
mod tests {
    //! Headless on the imported u-wall-two-sections; the registry is process-global, so every test
    //! installs first.
    use super::*;
    use crate::commands::install_app;
    use crate::import::project_to_drawing;
    use crate::schema::read_document;
    use lamb_core::model::{AnyView, Placement, PlanView, Project};
    use lamb_core::persistence::files_to_project;
    use std::collections::BTreeMap;
    use std::path::Path;

    type E<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    fn load(name: &str) -> E<Project> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb").join(format!("{name}.lamb.json"));
        let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        Ok(files_to_project(&files).map_err(|e| format!("{e:?}"))?)
    }

    /// The imported u-wall-two-sections, editing its first placed vertical view; returns that
    /// view and the plan cut line drawn in it.
    fn editing_section() -> E<(Session, Id, PlanCutLineOverlay)> {
        install_app().map_err(|e| e.to_string())?;
        let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
        let mut s = Session::new();
        s.open_drawing(d, "u-wall", None);
        let doc = read_document(s.doc()?)?;
        let section = doc.views.iter().find(|v| matches!(v, AnyView::Vertical(v) if v.placement.is_some())).ok_or("no placed section")?.id();
        s.execute("editview", &json!({ "viewId": section.0 }))?;
        let proj = DrawingProject::new(s.doc()?)?;
        let line = compute_plan_cut_lines_for_view(&proj, section).into_iter().next().ok_or("no plan cut line")?;
        Ok((s, section, line))
    }

    fn cut_of(s: &Session, plan: Id) -> E<f64> {
        let proj = DrawingProject::new(s.doc()?)?;
        match proj.view(plan).ok_or("no plan")?.placement {
            Some(Placement::Plan { cut_elevation, .. }) => Ok(*cut_elevation),
            _ => Err("not a placed plan".into()),
        }
    }

    fn prompt(s: &Session) -> Option<String> {
        s.current_prompt().map(|p| p.message)
    }

    #[test]
    fn pick_then_place_raises_the_cut_and_one_undo_restores_it() -> E<()> {
        let (mut s, _, line) = editing_section()?;
        let plan = line.source_view_id;
        let before = cut_of(&s, plan)?;
        let ap = pick_aperture(&s);

        s.start("cutline")?;
        assert_eq!(prompt(&s).as_deref(), Some("Select plan cut line"));
        s.input(Input::Point(Vec2::new(123.0, line.local_y + 10.0 * ap)))?;
        assert!(s.log.iter().any(|l| l == "No plan cut line there."), "{:?}", s.log);
        assert_eq!(prompt(&s).as_deref(), Some("Select plan cut line"), "still on step 1");

        s.input(Input::Point(Vec2::new(500.0, line.local_y + 0.4 * ap)))?;
        assert_eq!(prompt(&s).as_deref(), Some("Specify new cut height"));
        let pv = s.running.as_ref().ok_or("not running")?.machine.preview(&s, Vec2::new(100.0, 777.0));
        let h = CUT_LINE_LENGTH_MM / 2.0;
        assert_eq!(pv, vec![EntityKind::Line(Line { a: Vec2::new(100.0 - h, 777.0).to3(0.0), b: Vec2::new(100.0 + h, 777.0).to3(0.0) })]);
        s.input(Input::Point(Vec2::new(500.0, line.local_y + 300.0)))?;
        assert!(s.running.is_none(), "done");
        assert!((cut_of(&s, plan)? - (before + 300.0)).abs() < 1e-9, "{} vs {}", cut_of(&s, plan)?, before + 300.0);
        assert!(s.log.iter().any(|l| l.starts_with(&format!("{} cut at ", line.name))), "{:?}", s.log);

        s.undo()?;
        assert_eq!(cut_of(&s, plan)?.to_bits(), before.to_bits(), "one undo restores it");
        Ok(())
    }

    #[test]
    fn placing_back_on_its_own_line_is_bit_identical() -> E<()> {
        let (mut s, _, line) = editing_section()?;
        let before = cut_of(&s, line.source_view_id)?;
        s.start("cl")?;
        s.input(Input::Point(Vec2::new(0.0, line.local_y)))?;
        s.input(Input::Point(Vec2::new(4000.0, line.local_y)))?;
        assert_eq!(cut_of(&s, line.source_view_id)?.to_bits(), before.to_bits());
        Ok(())
    }

    #[test]
    fn a_typed_number_is_an_absolute_elevation_and_the_backend_matches() -> E<()> {
        let (mut s, _, line) = editing_section()?;
        s.start("cutline")?;
        s.input(Input::Point(Vec2::new(0.0, line.local_y)))?;
        s.input(Input::Text("2400".into()))?;
        assert_eq!(cut_of(&s, line.source_view_id)?, 2400.0);
        s.execute("cutline", &json!({ "planViewId": line.source_view_id.0, "elevation": 1500 }))?;
        assert_eq!(cut_of(&s, line.source_view_id)?, 1500.0);
        Ok(())
    }

    #[test]
    fn refused_while_editing_a_plan() -> E<()> {
        let (mut s, _, line) = editing_section()?;
        let plan = line.source_view_id;
        let doc = read_document(s.doc()?)?;
        assert!(doc.views.iter().any(|v| v.id() == plan && matches!(v, AnyView::Plan(PlanView { placement: Some(_), .. }))));
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let before = cut_of(&s, plan)?;
        let undo = s.state()?.undo.len();
        s.start("cutline")?;
        assert!(s.running.is_none());
        assert!(s.log.iter().any(|l| l == "Edit a placed section or elevation first (EDITVIEW)."), "{:?}", s.log);
        assert_eq!((cut_of(&s, plan)?, s.state()?.undo.len()), (before, undo), "nothing changed");
        Ok(())
    }
}

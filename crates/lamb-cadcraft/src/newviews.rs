//! The view-creating commands that place a plan at a level, ported from the TS app
//! (`commandline/newPlanCommand.ts`, `commandline/oneClickViewCommands.ts`, backend
//! `command-engine/src/commands/views.ts`):
//!
//! - `newoffsetplan {mode:"offset"|"height", fromViewId, floorToFloorMm | floorElevationMm, name?,
//!   cutHeightMm?}` (`np`, View ▸ LAMB ▸ New Offset Plan): a new level and a placed plan on it, a
//!   floor-to-floor distance above the from-plan's level or at an absolute floor elevation, with
//!   the from-plan's transform; then edit the new plan.
//! - `newplanline {fromVerticalViewId, localPoint:{x,y}, name?, cutHeightMm?}` (`npln`, Plan Here):
//!   one point in a placed section or elevation gives a new level and plan at that point's
//!   absolute height, with the host plan's transform. The edit space stays where it is.
//! - NEWSECTIONLINE's one-click form in a placed section ([`perpendicular_cut`],
//!   [`vertical_preview`]): the branch itself lives in `commands.rs`'s machine.
//!
//! Both creating commands return `{viewId, levelId}`, name the level and the plan `Level <n+1>`
//! unless given a name, and are one undo step.

use std::sync::{Mutex, PoisonError};

use cadcraft_doc::{EntityKind, Line, Space};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step, units::parse_distance};
use cadcraft_geom::{Bounds2, Vec2};
use lamb_core::derivation::projection::{PerpendicularCutLine, perpendicular_cut_line_through, plan_placement_through};
use lamb_core::derivation::read::{ProjectRead, ViewKind};
use lamb_core::geometry::{Point2, Transform2D};
use lamb_core::model::{Actor, AnyView, Id, Level, Placement, PlanView, authored_field};
use serde_json::{Value, json};

use crate::commands::{bad, echo_created, edited_view, enter_space, id_value, other, point_param};
use crate::maintain::now;
use crate::project::DrawingProject;
use crate::schema::{read_document, view_block_name, write_document};
use crate::views::create_view;

/// How far a one-click preview line runs past the view's drawn extents (TS `PREVIEW_MARGIN_MM`).
const PREVIEW_MARGIN_MM: f64 = 2000.0;
/// Half the span of a preview line in an empty view (TS `EMPTY_VIEW_SPAN_MM / 2`).
const EMPTY_HALF_SPAN_MM: f64 = 5000.0;
/// Length of a one-click perpendicular section's cut line (TS `CUT_LINE_LENGTH_MM`).
pub(crate) const CUT_LINE_LENGTH_MM: f64 = 20000.0;

/// What a new level-and-plan gets; the name defaults to `Level <levels + 1>`.
struct PlanSpec {
    name: Option<String>,
    floor: f64,
    cut_height: Option<f64>,
    order: f64,
    transform: Transform2D,
}

/// TS `createLevelAndPlacedPlan`: the level and its placed plan, one undo step.
fn create_level_and_plan(s: &mut Session, spec: PlanSpec) -> Result<Value> {
    let doc = read_document(s.doc()?).map_err(other)?;
    let name = spec.name.unwrap_or_else(|| default_level_name(doc.levels.len()));
    let cut = spec.cut_height.unwrap_or(doc.defaults.default_cut_height_mm);
    let d = s.doc_mut()?;
    let level_id = Id(d.new_handle().0);
    let placement = Placement::Plan { level_id, cut_elevation: spec.floor + cut, transform: spec.transform };
    let plan = AnyView::Plan(PlanView { id: Id(0), name: name.clone(), entities: Vec::new(), placement: Some(placement) });
    let view_id = create_view(d, plan).map_err(other)?;
    let mut doc = read_document(d).map_err(other)?;
    let floor_elevation = authored_field(spec.floor, view_id, Actor::User { user_id: "local".into() }, &now());
    doc.levels.push(Level { id: level_id, name, order: spec.order, floor_elevation });
    write_document(d, &doc).map_err(other)?;
    Ok(json!({ "viewId": view_id.0, "levelId": level_id.0 }))
}

fn default_level_name(levels: usize) -> String {
    format!("Level {}", levels + 1)
}

fn opt_name(p: &Value) -> Option<String> {
    p.get("name").and_then(Value::as_str).map(str::to_owned)
}

fn opt_num(cmd: &str, p: &Value, key: &str) -> Result<Option<f64>> {
    match p.get(key).filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => v.as_f64().filter(|v| v.is_finite()).map(Some).ok_or_else(|| bad(cmd, &format!("`{key}` must be a finite number"))),
    }
}

fn num(cmd: &str, p: &Value, key: &str) -> Result<f64> {
    opt_num(cmd, p, key)?.ok_or_else(|| bad(cmd, &format!("`{key}` is required")))
}

/// NEWOFFSETPLAN (TS `applyNewOffsetPlan`), then edit the new plan.
pub(crate) fn newoffsetplan(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "newoffsetplan";
    let from = p.get("fromViewId").and_then(id_value).ok_or_else(|| bad(CMD, "`fromViewId` is required"))?;
    let (from_floor, order, transform) = {
        let doc = read_document(s.doc()?).map_err(other)?;
        let Some(AnyView::Plan(PlanView { placement: Some(Placement::Plan { level_id, transform, .. }), .. })) =
            doc.views.iter().find(|v| v.id() == from)
        else {
            return Err(other("fromViewId must be a placed plan"));
        };
        let level = doc.levels.iter().find(|l| l.id == *level_id);
        (level.map_or(0.0, |l| l.floor_elevation.value), level.map_or(0.0, |l| l.order) + 1.0, *transform)
    };
    let floor = match p.get("mode").and_then(Value::as_str) {
        Some("offset") => from_floor + num(CMD, p, "floorToFloorMm")?,
        Some("height") => num(CMD, p, "floorElevationMm")?,
        _ => return Err(bad(CMD, "`mode` must be \"offset\" or \"height\"")),
    };
    let cut_height = opt_num(CMD, p, "cutHeightMm")?;
    let r = create_level_and_plan(s, PlanSpec { name: opt_name(p), floor, cut_height, order, transform })?;
    if let Some(id) = r.get("viewId").and_then(Value::as_u64) {
        enter_space(s, Space::Block(view_block_name(Id(id))))?;
    }
    Ok(r)
}

/// NEWPLANLINE (TS `applyNewPlanLine`): the edit space is left alone.
pub(crate) fn newplanline(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "newplanline";
    let from = p.get("fromVerticalViewId").and_then(id_value).ok_or_else(|| bad(CMD, "`fromVerticalViewId` is required"))?;
    let local = point_param(CMD, p, "localPoint")?;
    let cut_height = opt_num(CMD, p, "cutHeightMm")?;
    let (through, order) = {
        let proj = DrawingProject::new(s.doc()?).map_err(other)?;
        let view = proj
            .view(from)
            .filter(|v| v.kind == ViewKind::Vertical && v.placement.is_some())
            .ok_or_else(|| other("fromVerticalViewId must be a placed vertical view"))?;
        let host = view.host_view_id().and_then(|h| proj.view(h)).ok_or_else(|| other(format!("Vertical view {} has no host view", from.0)))?;
        let through = plan_placement_through(&view, &host, local).map_err(other)?;
        let host_order = match host.placement {
            Some(Placement::Plan { level_id, .. }) => proj.level(*level_id).map_or(0.0, |l| l.order),
            _ => 0.0,
        };
        (through, host_order + 1.0)
    };
    create_level_and_plan(s, PlanSpec { name: opt_name(p), floor: through.floor_elevation_mm, cut_height, order, transform: through.transform })
}

// ---------------- shared one-click helpers ----------------

/// The view, when it is a placed plan.
fn is_placed_plan(s: &Session, id: Id) -> bool {
    read_document_of(s).is_some_and(|doc| doc.views.iter().any(|v| v.id() == id && matches!(v, AnyView::Plan(PlanView { placement: Some(_), .. }))))
}

/// The view, when it is a placed section or elevation.
pub(crate) fn is_placed_vertical(s: &Session, id: Id) -> bool {
    read_document_of(s).is_some_and(|doc| doc.views.iter().any(|v| v.id() == id && matches!(v, AnyView::Vertical(v) if v.placement.is_some())))
}

fn read_document_of(s: &Session) -> Option<crate::schema::LambDocument> {
    read_document(s.doc().ok()?).ok()
}

/// TS `perpendicularCutLineThrough` for a click at `p` in the placed vertical view `view`.
pub(crate) fn perpendicular_cut(s: &Session, view: Id, p: Vec2) -> Result<PerpendicularCutLine> {
    let proj = DrawingProject::new(s.doc()?).map_err(other)?;
    let v = proj.view(view).ok_or_else(|| other("No such view"))?;
    let host = v.host_view_id().and_then(|h| proj.view(h)).ok_or_else(|| other(format!("Section {} has no host plan", view.0)))?;
    perpendicular_cut_line_through(&v, &host, Point2 { x: p.x, y: p.y }, CUT_LINE_LENGTH_MM).map_err(other)
}

/// TS `previewSpan`: the view's drawn extents along one axis plus a margin, or a fixed span about
/// the cursor in an empty view.
fn preview_span(s: &Session, view: Id, axis_x: bool, at: f64) -> (f64, f64) {
    let b: Option<Bounds2> = s.doc().ok().map(|d| d.extents(&Space::Block(view_block_name(view)))).filter(|b| !b.is_empty());
    match b {
        None => (at - EMPTY_HALF_SPAN_MM, at + EMPTY_HALF_SPAN_MM),
        Some(b) if axis_x => (b.min.x - PREVIEW_MARGIN_MM, b.max.x + PREVIEW_MARGIN_MM),
        Some(b) => (b.min.y - PREVIEW_MARGIN_MM, b.max.y + PREVIEW_MARGIN_MM),
    }
}

fn line(a: Vec2, b: Vec2) -> Vec<EntityKind> {
    vec![EntityKind::Line(Line { a: a.to3(0.0), b: b.to3(0.0) })]
}

/// The one-click section's preview: a vertical line at the cursor's x over the view's y extents.
pub(crate) fn vertical_preview(s: &Session, view: Id, cursor: Vec2) -> Vec<EntityKind> {
    let (y0, y1) = preview_span(s, view, false, cursor.y);
    line(Vec2::new(cursor.x, y0), Vec2::new(cursor.x, y1))
}

/// NEWPLANLINE's preview: a horizontal line at the cursor's y over the view's x extents.
fn horizontal_preview(s: &Session, view: Id, cursor: Vec2) -> Vec<EntityKind> {
    let (x0, x1) = preview_span(s, view, true, cursor.x);
    line(Vec2::new(x0, cursor.y), Vec2::new(x1, cursor.y))
}

// ---------------- interactive machines ----------------

/// The last value typed at each NEWOFFSETPLAN number prompt, for this process (TS `getRemembered`).
static LAST_OFFSET: Mutex<f64> = Mutex::new(3000.0);
static LAST_HEIGHT: Mutex<f64> = Mutex::new(0.0);

fn remembered(m: &Mutex<f64>) -> f64 {
    *m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn remember(m: &Mutex<f64>, v: f64) {
    *m.lock().unwrap_or_else(PoisonError::into_inner) = v;
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Offset,
    Height,
}

/// NEWOFFSETPLAN: `New plan or [Offset/Height] <Offset>:`, then the distance or elevation.
struct NewOffsetPlan {
    from: Option<Id>,
    mode: Option<Mode>,
}

impl NewOffsetPlan {
    fn store(&self) -> &'static Mutex<f64> {
        if self.mode == Some(Mode::Height) { &LAST_HEIGHT } else { &LAST_OFFSET }
    }
}

impl Interactive for NewOffsetPlan {
    fn name(&self) -> &'static str {
        "newoffsetplan"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.mode {
            None => Prompt::new("New plan", Accept::TEXT).kw(&["Offset", "Height"]).default("Offset"),
            Some(Mode::Offset) => Prompt::new("Specify floor-to-floor distance", Accept::NUMBER).default(remembered(&LAST_OFFSET).to_string()),
            Some(Mode::Height) => Prompt::new("Specify absolute floor elevation", Accept::NUMBER).default(remembered(&LAST_HEIGHT).to_string()),
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.from = edited_view(s).filter(|&id| is_placed_plan(s, id));
        if self.from.is_none() {
            s.echo("Switch to a placed plan first");
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(mode) = self.mode else {
            let picked = match i {
                Input::Enter => Some("Offset".to_owned()),
                Input::Text(t) | Input::Keyword(t) => self.prompt(s).match_keyword(&t),
                _ => return Ok(Step::Continue),
            };
            self.mode = match picked.as_deref() {
                Some("Offset") => Some(Mode::Offset),
                Some("Height") => Some(Mode::Height),
                _ => return Err(other("Unrecognized mode (try O for offset or H for height)")),
            };
            return Ok(Step::Continue);
        };
        let value = match i {
            Input::Enter => remembered(self.store()),
            Input::Text(t) | Input::Keyword(t) => parse_distance(&t).filter(|v| v.is_finite()).ok_or_else(|| other("Requires a number."))?,
            _ => return Ok(Step::Continue),
        };
        let Some(from) = self.from else { return Ok(Step::Done) };
        remember(self.store(), value);
        let name = default_level_name(read_document(s.doc()?).map_err(other)?.levels.len());
        let params = match mode {
            Mode::Offset => json!({ "mode": "offset", "fromViewId": from.0, "floorToFloorMm": value, "name": name }),
            Mode::Height => json!({ "mode": "height", "fromViewId": from.0, "floorElevationMm": value, "name": name }),
        };
        match newoffsetplan(s, &params) {
            Ok(r) => {
                echo_created(s, "Plan", &name, &r);
            }
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

pub(crate) fn newoffsetplan_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(NewOffsetPlan { from: None, mode: None }))
}

/// NEWPLANLINE: one point in a placed section or elevation.
struct NewPlanLine {
    view: Option<Id>,
}

impl Interactive for NewPlanLine {
    fn name(&self) -> &'static str {
        "newplanline"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Click a point to place a plan at that elevation", Accept::POINT)
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.view = edited_view(s).filter(|&id| is_placed_vertical(s, id));
        if self.view.is_none() {
            s.echo("Switch to a placed section or elevation first");
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let p = match i {
            Input::Point(p) => p,
            Input::Cancel | Input::Enter => return Ok(Step::Cancel),
            _ => return Ok(Step::Continue),
        };
        let Some(view) = self.view else { return Ok(Step::Done) };
        let name = default_level_name(read_document(s.doc()?).map_err(other)?.levels.len());
        let params = json!({ "fromVerticalViewId": view.0, "localPoint": {"x": p.x, "y": p.y}, "name": name });
        match newplanline(s, &params) {
            Ok(r) => {
                echo_created(s, "Plan", &name, &r);
            }
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
    fn preview(&self, s: &Session, cursor: Vec2) -> Vec<EntityKind> {
        self.view.map_or_else(Vec::new, |v| horizontal_preview(s, v, cursor))
    }
}

pub(crate) fn newplanline_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(NewPlanLine { view: None }))
}

#[cfg(test)]
mod tests {
    //! Headless, as `commands.rs`'s own: the registry is process-global, so every test installs.
    use super::*;
    use crate::commands::install_app;
    use lamb_core::geometry::IDENTITY_TRANSFORM;
    use lamb_core::geometry::polyline::line2;
    use lamb_core::model::VerticalView;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type Res<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    const MOVED: Transform2D = Transform2D { origin_x: 1000.0, origin_y: 500.0, rotation: 0.25 };

    /// LAMBPROJECT (Level 1 at 0, `Ground Floor` placed and edited), with the plan moved to
    /// [`MOVED`] so a copied transform is not just the identity.
    fn project() -> Res<(Session, Id)> {
        install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let r = s.execute("lambproject", &json!({}))?;
        let plan = Id(r["viewId"].as_u64().ok_or("no id")?);
        let d = s.doc_mut()?;
        let mut doc = read_document(d)?;
        for v in &mut doc.views {
            if let AnyView::Plan(PlanView { placement: Some(Placement::Plan { transform, .. }), .. }) = v {
                *transform = MOVED;
            }
        }
        write_document(d, &doc)?;
        Ok((s, plan))
    }

    /// A section on the ground plan along local x from 0 to 10000, edited.
    fn with_section(s: &mut Session, plan: Id) -> Res<Id> {
        let r =
            s.execute("newsectionline", &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 0}, "cutTo": {"x": 10000, "y": 0} }))?;
        let id = Id(r["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": id.0 }))?;
        Ok(id)
    }

    fn level_of(s: &Session, view: Id) -> Res<(Level, Placement)> {
        let doc = read_document(s.doc()?)?;
        let Some(AnyView::Plan(PlanView { placement: Some(pl @ Placement::Plan { level_id, .. }), .. })) = doc.views.iter().find(|v| v.id() == view)
        else {
            return Err("not a placed plan".into());
        };
        let level = doc.levels.iter().find(|l| l.id == *level_id).ok_or("no level")?.clone();
        Ok((level, pl.clone()))
    }

    fn space(s: &Session) -> Res<Space> {
        Ok(s.state()?.space.clone())
    }

    fn check_offset_result(s: &Session, r: &Value) -> Res<Id> {
        let view = Id(r["viewId"].as_u64().ok_or("no viewId")?);
        let (level, placement) = level_of(s, view)?;
        let cut = read_document(s.doc()?)?.defaults.default_cut_height_mm;
        assert_eq!(r["levelId"], json!(level.id.0));
        assert_eq!((level.name.as_str(), level.order, level.floor_elevation.value), ("Level 2", 1.0, 3000.0));
        assert_eq!(placement, Placement::Plan { level_id: level.id, cut_elevation: 3000.0 + cut, transform: MOVED });
        assert_eq!(space(s)?, Space::Block(view_block_name(view)), "edits the new plan");
        Ok(view)
    }

    #[test]
    fn newoffsetplan_offset_makes_a_level_above_edits_it_and_undoes_both() -> R {
        let (mut s, plan) = project()?;
        let r = s.execute("newoffsetplan", &json!({ "mode": "offset", "fromViewId": plan.0, "floorToFloorMm": 3000 }))?;
        check_offset_result(&s, &r)?;
        s.undo()?;
        let doc = read_document(s.doc()?)?;
        assert_eq!((doc.views.len(), doc.levels.len()), (1, 1), "one undo removes the plan and its level");
        Ok(())
    }

    #[test]
    fn newoffsetplan_height_mode_and_refusals() -> R {
        let (mut s, plan) = project()?;
        let r = s.execute(
            "newoffsetplan",
            &json!({ "mode": "height", "fromViewId": plan.0, "floorElevationMm": -450, "name": "Basement", "cutHeightMm": 1000 }),
        )?;
        let (level, placement) = level_of(&s, Id(r["viewId"].as_u64().ok_or("no id")?))?;
        assert_eq!((level.name.as_str(), level.floor_elevation.value), ("Basement", -450.0));
        assert!(matches!(placement, Placement::Plan { cut_elevation, .. } if cut_elevation == 550.0));

        let floating = s.execute("newplan", &json!({ "name": "Loose" }))?["viewId"].as_u64().ok_or("no id")?;
        let n = read_document(s.doc()?)?.views.len();
        let e = s.execute("newoffsetplan", &json!({ "mode": "offset", "fromViewId": floating, "floorToFloorMm": 3000 }));
        assert!(e.is_err_and(|e| e.to_string().contains("fromViewId must be a placed plan")));
        assert_eq!(read_document(s.doc()?)?.views.len(), n, "a refusal changes nothing");
        Ok(())
    }

    #[test]
    fn newoffsetplan_interactive_matches_the_backend() -> R {
        let (mut s, plan) = project()?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.start("newoffsetplan")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("New plan or [Offset/Height] <Offset>:"));
        s.input(Input::Text("o".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Specify floor-to-floor distance"));
        s.input(Input::Text("3000".into()))?;
        assert!(s.running.is_none(), "done");
        let doc = read_document(s.doc()?)?;
        let view = doc.views.iter().find(|v| v.name() == "Level 2").ok_or("no plan")?.id();
        check_offset_result(&s, &json!({ "viewId": view.0, "levelId": level_of(&s, view)?.0.id.0 }))?;
        assert!(s.log.iter().any(|l| l == &format!("Plan \"Level 2\" created (view {}).", view.0)), "{:?}", s.log);

        // Height mode, Enter at the mode prompt is Offset, so pick H.
        s.start("np")?;
        s.input(Input::Keyword("Height".into()))?;
        s.input(Input::Text("6000".into()))?;
        let doc = read_document(s.doc()?)?;
        assert!(doc.levels.iter().any(|l| l.name == "Level 3" && l.floor_elevation.value == 6000.0 && l.order == 2.0));

        s.execute("editview", &json!({}))?;
        s.start("newoffsetplan")?;
        assert!(s.running.is_none(), "refused in Model");
        assert!(s.log.iter().any(|l| l == "Switch to a placed plan first"), "{:?}", s.log);
        Ok(())
    }

    #[test]
    fn newplanline_places_a_plan_at_the_clicked_height_and_stays() -> R {
        let (mut s, plan) = project()?;
        let section = with_section(&mut s, plan)?;
        let r = s.execute("newplanline", &json!({ "fromVerticalViewId": section.0, "localPoint": {"x": 4000, "y": 2700} }))?;
        let (level, placement) = level_of(&s, Id(r["viewId"].as_u64().ok_or("no id")?))?;
        let cut = read_document(s.doc()?)?.defaults.default_cut_height_mm;
        assert_eq!((level.name.as_str(), level.order, level.floor_elevation.value), ("Level 2", 1.0, 2700.0));
        assert_eq!(placement, Placement::Plan { level_id: level.id, cut_elevation: 2700.0 + cut, transform: MOVED });
        assert_eq!(space(&s)?, Space::Block(view_block_name(section)), "the edit space is unchanged");

        let e = s.execute("newplanline", &json!({ "fromVerticalViewId": plan.0, "localPoint": {"x": 0, "y": 0} }));
        assert!(e.is_err_and(|e| e.to_string().contains("fromVerticalViewId must be a placed vertical view")));
        Ok(())
    }

    #[test]
    fn newplanline_interactive_previews_and_matches_the_backend() -> R {
        let (mut s, plan) = project()?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.start("newplanline")?;
        assert!(s.running.is_none() && s.log.iter().any(|l| l == "Switch to a placed section or elevation first"), "{:?}", s.log);

        let section = with_section(&mut s, plan)?;
        s.start("newplanline")?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Click a point to place a plan at that elevation"));
        let pv = s.running.as_ref().ok_or("not running")?.machine.preview(&s, Vec2::new(10.0, 2700.0));
        assert_eq!(pv, line(Vec2::new(-4990.0, 2700.0), Vec2::new(5010.0, 2700.0)), "empty view: ±5000 about the cursor");
        s.execute("line", &json!({ "points": [[0, 0], [8000, 3000]] }))?;
        let pv = s.running.as_ref().ok_or("not running")?.machine.preview(&s, Vec2::new(10.0, 2700.0));
        assert_eq!(pv, line(Vec2::new(-2000.0, 2700.0), Vec2::new(10000.0, 2700.0)), "extents ± 2000");
        s.input(Input::Point(Vec2::new(4000.0, 2700.0)))?;
        assert!(s.running.is_none());
        let doc = read_document(s.doc()?)?;
        let view = doc.views.iter().find(|v| v.name() == "Level 2").ok_or("no plan")?.id();
        assert_eq!(level_of(&s, view)?.0.floor_elevation.value, 2700.0);
        assert_eq!(space(&s)?, Space::Block(view_block_name(section)));
        Ok(())
    }

    fn sections(s: &Session) -> Res<Vec<VerticalView>> {
        Ok(read_document(s.doc()?)?.views.into_iter().filter_map(|v| if let AnyView::Vertical(v) = v { Some(v) } else { None }).collect())
    }

    #[test]
    fn newsectionline_in_a_section_takes_one_click_for_a_perpendicular_cut() -> R {
        let (mut s, plan) = project()?;
        // Identity host, so the expected cut line reads straight off the click.
        let d = s.doc_mut()?;
        let mut doc = read_document(d)?;
        for v in &mut doc.views {
            if let AnyView::Plan(PlanView { placement: Some(Placement::Plan { transform, .. }), .. }) = v {
                *transform = IDENTITY_TRANSFORM;
            }
        }
        write_document(d, &doc)?;
        let first = with_section(&mut s, plan)?;
        s.start("newsectionline")?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Click a point for a section perpendicular to this one"));
        let pv = s.running.as_ref().ok_or("not running")?.machine.preview(&s, Vec2::new(4000.0, 100.0));
        assert_eq!(pv, line(Vec2::new(4000.0, -4900.0), Vec2::new(4000.0, 5100.0)));
        s.input(Input::Point(Vec2::new(4000.0, 1500.0)))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter section name <Section 2>:"));
        s.input(Input::Text("B-B".into()))?;
        let b = sections(&s)?.into_iter().find(|v| v.name == "B-B").ok_or("no section")?;
        let Some(Placement::Vertical { host_view_id, cut_line, .. }) = &b.placement else { return Err("not placed".into()) };
        assert_eq!(*host_view_id, plan, "on the host plan, not the section");
        assert_eq!(cut_line, &line2(Point2 { x: 4000.0, y: -10000.0 }, Point2 { x: 4000.0, y: 10000.0 }));
        assert_eq!(space(&s)?, Space::Block(view_block_name(first)));

        // The backend form gives the same line.
        let cut = perpendicular_cut(&s, first, Vec2::new(4000.0, 1500.0))?;
        assert_eq!((cut.host_view_id, cut.cut_from, cut.cut_to), (plan, Point2 { x: 4000.0, y: -10000.0 }, Point2 { x: 4000.0, y: 10000.0 }));
        Ok(())
    }
}

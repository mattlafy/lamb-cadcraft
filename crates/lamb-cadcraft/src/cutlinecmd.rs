//! CUTLINE (LAMB T358, T499): move a cut line from the view it is drawn in.
//!
//! - In a placed section or elevation (T358): move a placed plan's cut elevation by picking its cut
//!   line, then placing it — the TS app's drag of a plan cut line
//!   (`apps/desktop/src/viewport2d/planCutLineDrag.ts`, T67) as CADCraft's click-click idiom.
//!   `Select plan cut line:` (nearest overlay line by local Y within the pick aperture, TS
//!   `hitPlanCutLine`), then `Specify new cut height:` — a point (its Y through the view, TS
//!   `elevationForDrag`) or a typed absolute elevation in mm. A horizontal preview line follows the
//!   cursor at step 2.
//! - In a placed plan (T499, `link-maintenance.md` "Move a vertical view's cut line"): `Select
//!   section or elevation cut line:` (nearest cut-line overlay within the aperture by
//!   point-to-segment distance; one hosted in another plan is refused there), then `Specify new
//!   position:` — a point (its signed perpendicular distance from the picked line, positive = left
//!   of the cut's own direction) or a typed signed offset in mm. The cut line moves parallel to
//!   itself; the view's linework stays put, and `maintain.rs` step 2 flags any link whose crossing
//!   broke (and clears it when restored). The preview is the picked line translated through the
//!   cursor.
//! - `cutline {planViewId, elevation}` / `cutline {viewId, offsetMm}` (`cl`, View ▸ LAMB ▸ Cut
//!   Line): thin wrappers over VIEWPROPS `cutElevation` / `cutOffset` — their refusals and their
//!   undo step are VIEWPROPS's.

use cadcraft_doc::{EntityKind, Line};
use cadcraft_engine::cmd::curves::pick_aperture;
use cadcraft_engine::units::parse_distance;
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::Vec2;
use lamb_core::derivation::markers::{CutLineOverlay, PlanCutLineOverlay, compute_cut_lines_for_view, compute_plan_cut_lines_for_view};
use lamb_core::derivation::projection::vertical_local_to_uz;
use lamb_core::derivation::read::ProjectRead;
use lamb_core::geometry::Point2;
use lamb_core::model::{AnyView, Id};
use serde_json::{Value, json};

use crate::commands::{bad, edited_view, id_value, other};
use crate::newviews::{CUT_LINE_LENGTH_MM, is_placed_vertical};
use crate::offsetviews::signed_mm;
use crate::project::DrawingProject;
use crate::schema::read_document;
use crate::viewpick::dist_to_segment;

const CMD: &str = "cutline";

/// CUTLINE `{planViewId, elevation}`: the plan's cut elevation becomes `elevation` (absolute mm).
/// CUTLINE `{viewId, offsetMm}`: the vertical view's cut line moves `offsetMm` to its own left.
pub(crate) fn cutline(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(view) = p.get("viewId") {
        let offset = p.get("offsetMm").cloned().unwrap_or(Value::Null);
        return crate::viewprops::viewprops(s, &json!({ "mode": "cutOffset", "viewId": view, "offsetMm": offset }));
    }
    let plan = p.get("planViewId").and_then(id_value).ok_or_else(|| bad(CMD, "`planViewId` (or `viewId`) is required"))?;
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

/// The section/elevation cut line drawn in plan `view` nearest `p`, within `tolerance`.
fn hit_cut_line(s: &Session, view: Id, p: Vec2, tolerance: f64) -> Option<CutLineOverlay> {
    let proj = DrawingProject::new(s.doc().ok()?).ok()?;
    compute_cut_lines_for_view(&proj, view)
        .ok()?
        .into_iter()
        .map(|o| (dist_to_segment(p, o.from, o.to), o))
        .filter(|(d, _)| *d <= tolerance)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, o)| o)
}

/// The name of the plan hosting `o`'s cut line, when that is not `plan`.
fn hosted_elsewhere(s: &Session, plan: Id, o: &CutLineOverlay) -> Option<String> {
    let proj = DrawingProject::new(s.doc().ok()?).ok()?;
    let host = proj.view(o.source_view_id)?.host_view_id()?;
    if host == plan {
        return None;
    }
    Some(proj.view(host).map_or_else(|| host.to_string(), |h| h.name.to_owned()))
}

/// The unit CCW perpendicular of the overlay's from→to direction (`offsetviews` convention).
fn left_normal(o: &CutLineOverlay) -> Option<(f64, f64)> {
    let (dx, dy) = (o.to.x - o.from.x, o.to.y - o.from.y);
    let len = dx.hypot(dy);
    (len > 0.0).then(|| (-dy / len, dx / len))
}

/// Signed distance of `p` from the overlay's line along [`left_normal`].
fn signed_offset(o: &CutLineOverlay, p: Vec2) -> f64 {
    left_normal(o).map_or(0.0, |(nx, ny)| (p.x - o.from.x) * nx + (p.y - o.from.y) * ny)
}

fn is_placed_plan(s: &Session, id: Id) -> bool {
    s.doc()
        .ok()
        .and_then(|d| read_document(d).ok())
        .is_some_and(|doc| doc.views.iter().any(|v| v.id() == id && matches!(v, AnyView::Plan(p) if p.placement.is_some())))
}

/// TS `elevationForDrag`: the absolute elevation of local Y `y` in the vertical view.
fn elevation_at(s: &Session, view: Id, y: f64) -> Result<f64> {
    let proj = DrawingProject::new(s.doc()?).map_err(other)?;
    let v = proj.view(view).ok_or_else(|| other("No such view"))?;
    Ok(vertical_local_to_uz(&v, Point2 { x: 0.0, y }).z)
}

struct CutLine {
    view: Option<Id>,
    /// The edited view is a placed plan (T499), not a placed vertical view (T358).
    in_plan: bool,
    picked: Option<PlanCutLineOverlay>,
    picked_cut: Option<CutLineOverlay>,
}

impl CutLine {
    /// The placed-plan direction: pick a section/elevation cut line, then its new position.
    fn input_plan(&mut self, s: &mut Session, plan: Id, i: Input) -> Result<Step> {
        let Some(o) = &self.picked_cut else {
            let Input::Point(p) = i else { return Ok(Step::Continue) };
            match hit_cut_line(s, plan, p, pick_aperture(s)) {
                Some(o) => match hosted_elsewhere(s, plan, &o) {
                    Some(host) => s.echo(format!("That cut line is drawn in {host}; edit it there.")),
                    None => self.picked_cut = Some(o),
                },
                None => s.echo("No section or elevation cut line there."),
            }
            return Ok(Step::Continue);
        };
        let offset = match i {
            Input::Point(p) => signed_offset(o, p),
            Input::Text(t) | Input::Keyword(t) => parse_distance(&t).filter(|v| v.is_finite()).ok_or_else(|| other("Requires a number."))?,
            _ => return Ok(Step::Continue),
        };
        if offset == 0.0 {
            s.echo("No change.");
            return Ok(Step::Done);
        }
        let (name, id) = (o.name.clone(), o.source_view_id);
        cutline(s, &json!({ "viewId": id.0, "offsetMm": offset }))?;
        s.echo(format!("{name} moved {}", signed_mm(offset)));
        Ok(Step::Done)
    }
}

impl Interactive for CutLine {
    fn name(&self) -> &'static str {
        CMD
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.in_plan {
            return match self.picked_cut {
                None => Prompt::new("Select section or elevation cut line", Accept::POINT),
                Some(_) => Prompt::new("Specify new position", Accept::POINT_OR_NUMBER),
            };
        }
        match self.picked {
            None => Prompt::new("Select plan cut line", Accept::POINT),
            Some(_) => Prompt::new("Specify new cut height", Accept::POINT_OR_NUMBER),
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let edited = edited_view(s);
        self.view = edited.filter(|&id| is_placed_vertical(s, id));
        if self.view.is_none() {
            self.view = edited.filter(|&id| is_placed_plan(s, id));
            self.in_plan = self.view.is_some();
        }
        if self.view.is_none() {
            s.echo("Edit a placed plan, section or elevation first (EDITVIEW).");
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel | Input::Enter) {
            return Ok(Step::Cancel);
        }
        let Some(view) = self.view else { return Ok(Step::Done) };
        if self.in_plan {
            return self.input_plan(s, view, i);
        }
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
        if let Some(o) = &self.picked_cut {
            let Some((nx, ny)) = left_normal(o) else { return Vec::new() };
            let d = signed_offset(o, cursor);
            let (ox, oy) = (nx * d, ny * d);
            return vec![EntityKind::Line(Line {
                a: Vec2::new(o.from.x + ox, o.from.y + oy).to3(0.0),
                b: Vec2::new(o.to.x + ox, o.to.y + oy).to3(0.0),
            })];
        }
        if self.picked.is_none() {
            return Vec::new();
        }
        let h = CUT_LINE_LENGTH_MM / 2.0;
        vec![EntityKind::Line(Line { a: Vec2::new(cursor.x - h, cursor.y).to3(0.0), b: Vec2::new(cursor.x + h, cursor.y).to3(0.0) })]
    }
}

pub(crate) fn cutline_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(CutLine { view: None, in_plan: false, picked: None, picked_cut: None }))
}

#[cfg(test)]
mod tests {
    //! Headless on the imported u-wall-two-sections; the registry is process-global, so every test
    //! installs first.
    use super::*;
    use crate::commands::install_app;
    use crate::import::project_to_drawing;
    use lamb_core::derivation::markers::compute_markers_for_view;
    use lamb_core::model::{LinkRecord, Placement, PlanView, Project};
    use lamb_core::persistence::files_to_project;
    use std::collections::BTreeMap;
    use std::path::Path;

    type E<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    fn load(name: &str) -> E<Project> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb").join(format!("{name}.lamb.json"));
        let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        Ok(files_to_project(&files).map_err(|e| format!("{e:?}"))?)
    }

    fn opened() -> E<Session> {
        install_app().map_err(|e| e.to_string())?;
        let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
        let mut s = Session::new();
        s.open_drawing(d, "u-wall", None);
        crate::maintain::install(&mut s);
        Ok(s)
    }

    /// The imported u-wall-two-sections, editing its first placed vertical view; returns that
    /// view and the plan cut line drawn in it.
    fn editing_section() -> E<(Session, Id, PlanCutLineOverlay)> {
        let mut s = opened()?;
        let doc = read_document(s.doc()?)?;
        let section = doc.views.iter().find(|v| matches!(v, AnyView::Vertical(v) if v.placement.is_some())).ok_or("no placed section")?.id();
        s.execute("editview", &json!({ "viewId": section.0 }))?;
        let proj = DrawingProject::new(s.doc()?)?;
        let line = compute_plan_cut_lines_for_view(&proj, section).into_iter().next().ok_or("no plan cut line")?;
        Ok((s, section, line))
    }

    fn view_named(s: &Session, name: &str) -> E<Id> {
        Ok(read_document(s.doc()?)?.views.iter().find(|v| v.name() == name).ok_or("no such view")?.id())
    }

    /// Editing the plan `P`; returns it and the `Crossbar` section (cut x = 5000, y -1000 → 3000).
    fn editing_plan() -> E<(Session, Id, Id)> {
        let mut s = opened()?;
        let plan = view_named(&s, "P")?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let crossbar = view_named(&s, "Crossbar")?;
        Ok((s, plan, crossbar))
    }

    fn cut_of(s: &Session, plan: Id) -> E<f64> {
        let proj = DrawingProject::new(s.doc()?)?;
        match proj.view(plan).ok_or("no plan")?.placement {
            Some(Placement::Plan { cut_elevation, .. }) => Ok(*cut_elevation),
            _ => Err("not a placed plan".into()),
        }
    }

    /// The vertical view's cut-line start, in its host's local frame.
    fn cut_start(s: &Session, id: Id) -> E<Point2> {
        match read_document(s.doc()?)?.views.into_iter().find(|v| v.id() == id) {
            Some(AnyView::Vertical(v)) => match v.placement {
                Some(Placement::Vertical { cut_line, .. }) => Ok(cut_line.start),
                _ => Err("not placed".into()),
            },
            _ => Err("not vertical".into()),
        }
    }

    /// Every link with an end on an entity of `view`.
    fn links_of(s: &Session, view: Id) -> E<Vec<LinkRecord>> {
        let proj = DrawingProject::new(s.doc()?)?;
        let in_view = |id: Id| proj.entity_view_id(id) == Some(view);
        Ok(proj.links().filter(|l| in_view(l.a.entity_id) || in_view(l.b.entity_id)).cloned().collect())
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
    fn refused_outside_a_placed_view() -> E<()> {
        let (mut s, _, line) = editing_section()?;
        let plan = line.source_view_id;
        let doc = read_document(s.doc()?)?;
        assert!(doc.views.iter().any(|v| v.id() == plan && matches!(v, AnyView::Plan(PlanView { placement: Some(_), .. }))));
        s.execute("editview", &json!({}))?;
        let before = cut_of(&s, plan)?;
        let undo = s.state()?.undo.len();
        s.start("cutline")?;
        assert!(s.running.is_none());
        assert!(s.log.iter().any(|l| l == "Edit a placed plan, section or elevation first (EDITVIEW)."), "{:?}", s.log);
        assert_eq!((cut_of(&s, plan)?, s.state()?.undo.len()), (before, undo), "nothing changed");
        Ok(())
    }

    /// T499: a parallel move that still crosses the wall keeps the link and moves the marker's
    /// plan crossing; one past the wall's end drops it; undo restores it.
    #[test]
    fn moving_a_cut_line_keeps_crossing_links_and_drops_broken_ones() -> E<()> {
        let (mut s, plan, crossbar) = editing_plan()?;
        let legs = view_named(&s, "Legs")?;
        assert_eq!(cut_start(&s, crossbar)?, Point2 { x: 5000.0, y: -1000.0 });
        assert_eq!(links_of(&s, crossbar)?.len(), 1);
        let undo = s.state()?.undo.len();

        // The cut runs +Y, so +1000 is to its left: x 5000 → 4000. Still crosses wall-u's base.
        s.execute("cutline", &json!({ "viewId": crossbar.0, "offsetMm": 1000 }))?;
        assert_eq!(s.state()?.undo.len(), undo + 1, "one undo step");
        assert_eq!(cut_start(&s, crossbar)?, Point2 { x: 4000.0, y: -1000.0 });
        assert!(links_of(&s, crossbar)?.iter().chain(links_of(&s, legs)?.iter()).all(|l| l.stale.is_none()), "no link stale");
        let proj = DrawingProject::new(s.doc()?)?;
        let overlay = compute_cut_lines_for_view(&proj, plan)?.into_iter().find(|o| o.source_view_id == crossbar).ok_or("no overlay")?;
        assert_eq!((overlay.from, overlay.to), (Point2 { x: 4000.0, y: -1000.0 }, Point2 { x: 4000.0, y: 3000.0 }), "the cut symbol moved");
        let markers = compute_markers_for_view(&proj, crossbar)?;
        let m = markers.iter().find(|m| m.source_segment_index == 1).ok_or("no marker on the wall's base")?;
        assert!((m.position.x - 1000.0).abs() < 1e-9, "the crossing is still 1000 along the cut: {:?}", m.position);
        assert!((m.source_segment_param - 0.4).abs() < 1e-9, "at the new crossing x = 4000: {}", m.source_segment_param);
        drop(proj);
        s.undo()?;
        assert_eq!(cut_start(&s, crossbar)?, Point2 { x: 5000.0, y: -1000.0 }, "undo restores it");

        // -6000 → x 11000, past the wall's end at 10000: the link is dropped (a link exists only
        // where its lines cross, LAMB T514), in one undo step; undo brings it back.
        let undo = s.state()?.undo.len();
        let before = links_of(&s, crossbar)?;
        s.execute("cutline", &json!({ "viewId": crossbar.0, "offsetMm": -6000 }))?;
        assert_eq!(s.state()?.undo.len(), undo + 1);
        assert!(links_of(&s, crossbar)?.is_empty(), "dropped");
        assert!(links_of(&s, legs)?.iter().all(|l| l.stale.is_none()), "other views' links untouched");
        s.undo()?;
        assert_eq!(links_of(&s, crossbar)?, before, "undo restores the link");
        Ok(())
    }

    #[test]
    fn interactive_in_the_plan_picks_a_cut_line_and_moves_it() -> E<()> {
        let (mut s, _plan, crossbar) = editing_plan()?;
        let ap = pick_aperture(&s);
        s.start("cutline")?;
        assert_eq!(prompt(&s).as_deref(), Some("Select section or elevation cut line"));
        s.input(Input::Point(Vec2::new(20000.0, 20000.0)))?;
        assert!(s.log.iter().any(|l| l == "No section or elevation cut line there."), "{:?}", s.log);
        assert_eq!(prompt(&s).as_deref(), Some("Select section or elevation cut line"), "still on step 1");

        s.input(Input::Point(Vec2::new(5000.0 + 0.4 * ap, -500.0)))?;
        assert_eq!(prompt(&s).as_deref(), Some("Specify new position"));
        let pv = s.running.as_ref().ok_or("not running")?.machine.preview(&s, Vec2::new(4200.0, 100.0));
        assert_eq!(pv, vec![EntityKind::Line(Line { a: Vec2::new(4200.0, -1000.0).to3(0.0), b: Vec2::new(4200.0, 3000.0).to3(0.0) })]);
        s.input(Input::Text("750".into()))?;
        assert!(s.running.is_none(), "done");
        assert_eq!(cut_start(&s, crossbar)?, Point2 { x: 4250.0, y: -1000.0 }, "+750 to the cut's left");
        assert!(s.log.iter().any(|l| l == "Crossbar moved +750"), "{:?}", s.log);

        // A clicked point: the offset is its perpendicular distance from the line.
        s.start("cl")?;
        s.input(Input::Point(Vec2::new(4250.0, -500.0)))?;
        s.input(Input::Point(Vec2::new(3000.0, 77.0)))?;
        assert_eq!(cut_start(&s, crossbar)?, Point2 { x: 3000.0, y: -1000.0 });
        assert!(s.log.iter().any(|l| l == "Crossbar moved +1250"), "{:?}", s.log);

        // A click on the line itself is no change and no undo step.
        let undo = s.state()?.undo.len();
        s.start("cl")?;
        s.input(Input::Point(Vec2::new(3000.0, 0.0)))?;
        s.input(Input::Point(Vec2::new(3000.0, 2000.0)))?;
        assert!(s.log.iter().any(|l| l == "No change."), "{:?}", s.log);
        assert_eq!(s.state()?.undo.len(), undo);
        Ok(())
    }
}

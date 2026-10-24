//! LINK and UNLINK at the command line (TS `linkCommand.ts` `LinkCommand`, `pickCommands.ts`
//! `UnlinkCommand`): in the edited view, click a hollow marker, then the line through it; or click a
//! filled marker to unlink it.
//!
//! Both are repeatable until Enter/Esc, and each completed link or unlink is its own undo step: the
//! machines run under the non-undoable `link` / `unlink` specs and apply through `link.apply` /
//! `unlink.apply` with [`Session::execute`], which records one step per call (an undoable
//! interactive spec would fold the whole run into one step when it ends).
//!
//! A link only exists where two lines cross, which is where a marker is drawn, so LINK never leaves
//! the edited view: the marker names the far end, the clicked line the near one. Crossings stacked
//! at one marker position are a drawing error and are refused, never resolved by choosing one.

use cadcraft_engine::cmd::curves::{pick_aperture, pick_at};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::Vec2;
use lamb_core::derivation::linking::find_link_for_marker;
use lamb_core::derivation::markers::{Marker, compute_markers_for_view, is_near_marker};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::geometry::Point2;
use lamb_core::geometry::polyline::{closest_point_on_segment, resolved_segments};
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::commands::{edited_view, other};
use crate::project::DrawingProject;

/// One end of a link as picked: the entity, its nearest segment and the parameter along it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Picked {
    pub entity: Id,
    pub segment_index: usize,
    pub param: f64,
}

impl Picked {
    fn json(self) -> Value {
        json!({ "entityId": self.entity.0, "segmentIndex": self.segment_index, "segmentParam": self.param })
    }
}

/// The line under `p` in the current space and the nearest point on it. Block coordinates are
/// view-local (a view block is inserted at the origin, scale 1), so `p` needs no transform.
pub(crate) fn pick_line(s: &Session, p: Vec2) -> Option<Picked> {
    let h = pick_at(s, p)?;
    let proj = DrawingProject::new(s.doc().ok()?).ok()?;
    let e = proj.entity(Id(h.0))?;
    let poly = e.polyline.as_deref()?;
    let q = Point2 { x: p.x, y: p.y };
    resolved_segments(poly)
        .iter()
        .map(|seg| (seg.index, closest_point_on_segment(seg, q)))
        .min_by(|a, b| a.1.distance.total_cmp(&b.1.distance))
        .map(|(segment_index, c)| Picked { entity: e.id, segment_index, param: c.t })
}

const NO_LINE: &str = "No line found near click";
const NO_MARKER: &str = "No marker near click";
const STACKED: &str = "Several crossings meet here — fix the drawing first";
const FILLED: &str = "That marker is already linked";
const MISSES: &str = "That line does not pass through the marker";

/// The one marker of `view` under `p`, or the refusal to echo.
fn marker_under(s: &Session, view: Id, p: Vec2) -> Result<std::result::Result<Marker, &'static str>> {
    let dp = DrawingProject::new(s.doc()?).map_err(other)?;
    let markers = compute_markers_for_view(&dp, view).map_err(other)?;
    let q = Point2 { x: p.x, y: p.y };
    let near: Vec<&Marker> = markers.iter().filter(|m| is_near_marker(m, q, pick_aperture(s))).collect();
    let Some(first) = near.first() else { return Ok(Err(NO_MARKER)) };
    if near.iter().any(|m| m.source_entity_id != first.source_entity_id) {
        return Ok(Err(STACKED));
    }
    if first.filled {
        return Ok(Err(FILLED));
    }
    Ok(Ok((*first).clone()))
}

/// The line under `p`, anchored at its point nearest `m` — or the refusal to echo when there is no
/// line or it misses the marker by more than the auto-link slack.
fn line_through(s: &Session, p: Vec2, m: &Marker) -> std::result::Result<Picked, &'static str> {
    let h = pick_line(s, p).ok_or(NO_LINE)?;
    let proj = DrawingProject::new(s.doc().map_err(|_| NO_LINE)?).map_err(|_| NO_LINE)?;
    let poly = proj.entity(h.entity).and_then(|e| e.polyline.map(std::borrow::Cow::into_owned)).ok_or(NO_LINE)?;
    let (segment_index, c) = resolved_segments(&poly)
        .iter()
        .map(|seg| (seg.index, closest_point_on_segment(seg, m.position)))
        .min_by(|a, b| a.1.distance.total_cmp(&b.1.distance))
        .ok_or(NO_LINE)?;
    if c.distance > TOLERANCE.coincidence_mm + 2.0 {
        return Err(MISSES);
    }
    Ok(Picked { entity: h.entity, segment_index, param: c.t })
}

/// LINK: a hollow marker, then the line through it, in the edited view; repeatable.
#[derive(Default)]
struct Link {
    marker: Option<Marker>,
}

impl Interactive for Link {
    fn name(&self) -> &'static str {
        "link"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        let msg = if self.marker.is_some() { "Select the line through it" } else { "Select a marker to link (Esc/Enter to end)" };
        Prompt::new(msg, Accept::POINT)
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if edited_view(s).is_none() {
            s.echo("Edit a view first");
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let p = match i {
            Input::Enter | Input::Cancel => return Ok(Step::Done),
            Input::Point(p) => p,
            _ => return Ok(Step::Continue),
        };
        let Some(view) = edited_view(s) else {
            s.echo("Edit a view first");
            return Ok(Step::Done);
        };
        let Some(m) = self.marker.take() else {
            match marker_under(s, view, p)? {
                Ok(m) => self.marker = Some(m),
                Err(msg) => s.echo(msg),
            }
            return Ok(Step::Continue);
        };
        // Repeatable: back to the first prompt whatever the outcome.
        match line_through(s, p, &m) {
            Err(msg) => s.echo(msg),
            Ok(near) => {
                let far = Picked { entity: m.source_entity_id, segment_index: m.source_segment_index, param: m.source_segment_param };
                match s.execute("link.apply", &json!({ "a": near.json(), "b": far.json() })) {
                    Ok(_) => s.echo("Linked"),
                    Err(e) => s.echo(format!("Error: {e}")),
                }
            }
        }
        Ok(Step::Continue)
    }
}

pub(crate) fn link_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(Link::default()))
}

/// UNLINK: a filled marker in the edited view; repeatable.
struct Unlink;

impl Interactive for Unlink {
    fn name(&self) -> &'static str {
        "unlink"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Select a filled marker to unlink (Esc/Enter to end)", Accept::POINT)
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if edited_view(s).is_none() {
            s.echo("Edit a view first");
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let p = match i {
            Input::Enter | Input::Cancel => return Ok(Step::Done),
            Input::Point(p) => p,
            _ => return Ok(Step::Continue),
        };
        let Some(view) = edited_view(s) else {
            s.echo("Edit a view first");
            return Ok(Step::Done);
        };
        let link_id = {
            let dp = DrawingProject::new(s.doc()?).map_err(other)?;
            let markers = compute_markers_for_view(&dp, view).map_err(other)?;
            let q = Point2 { x: p.x, y: p.y };
            match markers.iter().find(|m| is_near_marker(m, q, pick_aperture(s))) {
                None => Err("No marker near click"),
                Some(m) if !m.filled => Err("That marker has no link yet"),
                Some(m) => find_link_for_marker(&dp, m, view).map(|l| l.id).ok_or("Could not find the underlying link"),
            }
        };
        match link_id {
            Err(msg) => s.echo(msg),
            Ok(id) => match s.execute("unlink.apply", &json!({ "linkId": id.0 })) {
                Ok(_) => s.echo("Unlinked"),
                Err(e) => s.echo(format!("Error: {e}")),
            },
        }
        Ok(Step::Continue)
    }
}

pub(crate) fn unlink_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(Unlink))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{enter_space, install_app};
    use crate::schema::{read_document, view_block_name};
    use cadcraft_doc::Space;
    use lamb_core::model::{LinkOrigin, LinkRecord};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type E<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    struct Fixture {
        s: Session,
        plan: Id,
        section: Id,
        /// Plan line (0,0)–(10000,0), crossing the cut line x = 3000 at 30% of its length.
        plan_line: Id,
        /// Plan line (0,2000)–(10000,2000): crosses the cut line too, never linked.
        other_line: Id,
        /// Section line (5000,-5000)–(5000,5000): through the plan line's marker in the section.
        section_line: Id,
    }

    fn new_entity(s: &mut Session, view: Id, points: Value) -> E<Id> {
        let before: Vec<_> = s.doc()?.block(&view_block_name(view)).map(|b| b.entities.iter().map(|e| e.handle).collect()).unwrap_or_default();
        s.execute("line", &json!({ "points": points }))?;
        let b = s.doc()?.block(&view_block_name(view)).ok_or("no block")?;
        let h = b.entities.iter().map(|e| e.handle).find(|h| !before.contains(h)).ok_or("line not drawn")?;
        Ok(Id(h.0))
    }

    /// A placed plan with two lines and a placed section cutting both at x = 3000, with one line.
    fn fixture() -> E<Fixture> {
        install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no plan")?);
        let plan_line = new_entity(&mut s, plan, json!([[0, 0], [10000, 0]]))?;
        let other_line = new_entity(&mut s, plan, json!([[0, 2000], [10000, 2000]]))?;
        let r = s.execute(
            "newsectionline",
            &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 3000, "y": -5000}, "cutTo": {"x": 3000, "y": 5000} }),
        )?;
        let section = Id(r["viewId"].as_u64().ok_or("no section")?);
        enter_space(&mut s, Space::Block(view_block_name(section)))?;
        // Vertical through u = 5000 (plan (3000, 0)), so it crosses the plan cut where the plan
        // line crosses the section's cut line (a link needs its lines to cross, LAMB T514).
        let section_line = new_entity(&mut s, section, json!([[5000, -5000], [5000, 5000]]))?;
        enter_space(&mut s, Space::Block(view_block_name(plan)))?;
        Ok(Fixture { s, plan, section, plan_line, other_line, section_line })
    }

    fn links(s: &Session) -> E<Vec<LinkRecord>> {
        Ok(read_document(s.doc()?)?.links)
    }

    fn prompt(s: &Session) -> Option<String> {
        s.current_prompt().map(|p| p.message)
    }

    /// Where `source` crosses `view`'s cut, in `view`'s local space.
    fn marker_at(s: &Session, view: Id, source: Id) -> E<Vec2> {
        let markers = compute_markers_for_view(&DrawingProject::new(s.doc()?)?, view)?;
        let m = markers.iter().find(|m| m.source_entity_id == source).ok_or("no marker")?;
        Ok(Vec2::new(m.position.x, m.position.y))
    }

    const FIRST: &str = "Select a marker to link (Esc/Enter to end)";
    const SECOND: &str = "Select the line through it";

    fn filled(s: &Session, view: Id, source: Id) -> E<bool> {
        let markers = compute_markers_for_view(&DrawingProject::new(s.doc()?)?, view)?;
        Ok(markers.iter().find(|m| m.source_entity_id == source).ok_or("no marker")?.filled)
    }

    #[test]
    fn link_clicks_a_marker_then_the_line_through_it_in_one_view_and_repeats() -> R {
        let mut f = fixture()?;
        let s = &mut f.s;
        let base = links(s)?.len();
        enter_space(s, Space::Block(view_block_name(f.section)))?;
        let crossing = marker_at(s, f.section, f.plan_line)?;
        assert!(!filled(s, f.section, f.plan_line)? && !filled(s, f.plan, f.section_line)?, "hollow before");
        s.start("link")?;
        assert_eq!(prompt(s).as_deref(), Some(FIRST));
        s.input(Input::Point(crossing))?;
        assert_eq!(prompt(s).as_deref(), Some(SECOND));
        // The line, clicked away from the crossing: anchored at the marker, not the click.
        s.input(Input::Point(Vec2::new(5000.0, 4000.0)))?;
        assert!(s.log.iter().any(|l| l == "Linked"), "{:?}", s.log);
        let now = links(s)?;
        assert_eq!(now.len(), base + 1);
        let l = now.last().ok_or("no link")?;
        assert_eq!(l.origin, LinkOrigin::Explicit);
        assert_eq!((l.a.entity_id, l.a.segment_index), (f.section_line, 0));
        let want = (crossing.y + 5000.0) / 10000.0;
        assert!((l.a.segment_param - want).abs() < 1e-9, "{} vs {want}", l.a.segment_param);
        assert_eq!((l.b.entity_id, l.b.segment_index), (f.plan_line, 0));
        assert!((l.b.segment_param - 0.3).abs() < 1e-9, "{}", l.b.segment_param);
        assert!(filled(s, f.section, f.plan_line)? && filled(s, f.plan, f.section_line)?, "filled in both views");
        assert_eq!(prompt(s).as_deref(), Some(FIRST), "repeatable");
        assert!(s.running.is_some());
        s.input(Input::Enter)?;
        assert!(s.running.is_none(), "Enter ends it");
        s.undo()?;
        assert_eq!(links(s)?.len(), base, "one undo step");
        Ok(())
    }

    #[test]
    fn link_refusals_echo_and_keep_prompting() -> R {
        let mut f = fixture()?;
        let s = &mut f.s;
        // A third plan line through (3000, 0): its crossing stacks on the plan line's in the section.
        let stacked = new_entity(s, f.plan, json!([[0, -3000], [6000, 3000]]))?;
        enter_space(s, Space::Block(view_block_name(f.section)))?;
        let base = links(s)?.len();
        let (crossing, other) = (marker_at(s, f.section, f.plan_line)?, marker_at(s, f.section, f.other_line)?);
        assert!((marker_at(s, f.section, stacked)? - crossing).len() < 1e-6, "stacked");
        s.start("lk")?;
        let n = s.log.len();
        s.input(Input::Point(Vec2::new(-50000.0, -50000.0)))?;
        assert_eq!(prompt(s).as_deref(), Some(FIRST), "empty spot keeps the first prompt");
        s.input(Input::Point(crossing))?;
        assert_eq!(prompt(s).as_deref(), Some(FIRST), "stacked crossings refused");
        // The other line's marker is hollow, but the section line misses it.
        s.input(Input::Point(other))?;
        assert_eq!(prompt(s).as_deref(), Some(SECOND));
        s.input(Input::Point(Vec2::new(5000.0, 4000.0)))?;
        assert_eq!(prompt(s).as_deref(), Some(FIRST));
        // Second prompt, nothing there.
        s.input(Input::Point(other))?;
        s.input(Input::Point(Vec2::new(-50000.0, 50000.0)))?;
        assert_eq!(&s.log[n..], [NO_MARKER, STACKED, MISSES, NO_LINE].map(str::to_owned));
        assert_eq!(links(s)?.len(), base);
        s.input(Input::Enter)?;

        // Filled: erase the stacked line, link the crossing, then click it again.
        enter_space(s, Space::Block(view_block_name(f.plan)))?;
        s.execute("erase", &json!({ "handles": [cadcraft_doc::Handle(stacked.0).hex()] }))?;
        enter_space(s, Space::Block(view_block_name(f.section)))?;
        s.start("link")?;
        s.input(Input::Point(crossing))?;
        s.input(Input::Point(Vec2::new(5000.0, 4000.0)))?;
        assert_eq!(links(s)?.len(), base + 1);
        let n = s.log.len();
        s.input(Input::Point(crossing))?;
        assert_eq!(&s.log[n..], [FILLED.to_owned()]);
        assert_eq!(links(s)?.len(), base + 1);
        Ok(())
    }

    #[test]
    fn link_outside_a_view_refuses() -> R {
        let mut f = fixture()?;
        let s = &mut f.s;
        enter_space(s, Space::Model)?;
        s.start("link")?;
        assert!(s.running.is_none());
        assert!(s.log.iter().any(|l| l == "Edit a view first"), "{:?}", s.log);
        Ok(())
    }

    #[test]
    fn unlink_clicks_a_filled_marker_and_undo_restores_it() -> R {
        let mut f = fixture()?;
        let s = &mut f.s;
        let crossing = marker_at(s, f.section, f.plan_line)?;
        enter_space(s, Space::Block(view_block_name(f.section)))?;
        s.start("link")?;
        s.input(Input::Point(crossing))?;
        s.input(Input::Point(Vec2::new(5000.0, 4000.0)))?;
        s.input(Input::Enter)?;
        let linked = links(s)?;
        let link = linked.last().ok_or("no link")?.clone();

        let markers = compute_markers_for_view(&DrawingProject::new(s.doc()?)?, f.section)?;
        let at = |id: Id| markers.iter().find(|m| m.source_entity_id == id).map(|m| Vec2::new(m.position.x, m.position.y)).ok_or("no marker");
        let (filled, empty) = (at(f.plan_line)?, at(f.other_line)?);

        s.start("ulk")?;
        assert_eq!(prompt(s).as_deref(), Some("Select a filled marker to unlink (Esc/Enter to end)"));
        let n = s.log.len();
        s.input(Input::Point(Vec2::new(-90000.0, -90000.0)))?;
        s.input(Input::Point(empty))?;
        assert_eq!(&s.log[n..], ["No marker near click".to_owned(), "That marker has no link yet".to_owned()]);
        s.input(Input::Point(filled))?;
        assert_eq!(s.log.last().map(String::as_str), Some("Unlinked"));
        assert!(!links(s)?.iter().any(|l| l.id == link.id), "the link is gone");
        assert!(s.running.is_some(), "repeatable");
        s.input(Input::Enter)?;
        s.undo()?;
        assert_eq!(links(s)?, linked, "undo restores the link");
        Ok(())
    }

    #[test]
    fn unlink_outside_a_view_refuses() -> R {
        let mut f = fixture()?;
        let s = &mut f.s;
        enter_space(s, Space::Model)?;
        s.start("unlink")?;
        assert!(s.running.is_none());
        assert!(s.log.iter().any(|l| l == "Edit a view first"), "{:?}", s.log);
        Ok(())
    }
}

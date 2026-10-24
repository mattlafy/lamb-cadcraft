//! LINK and UNLINK at the command line (TS `linkCommand.ts` `LinkCommand`, `pickCommands.ts`
//! `UnlinkCommand`): pick a line, then its match in another view; or click a filled marker.
//!
//! Both are repeatable until Enter/Esc, and each completed link or unlink is its own undo step: the
//! machines run under the non-undoable `link` / `unlink` specs and apply through `link.apply` /
//! `unlink.apply` with [`Session::execute`], which records one step per call (an undoable
//! interactive spec would fold the whole run into one step when it ends).
//!
//! The user reaches the other view by clicking the other tile mid-command; LINK never binds itself
//! to one view (TS T154's `atDefaultState` opt-out).

use cadcraft_engine::cmd::curves::{pick_aperture, pick_at};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::Vec2;
use lamb_core::derivation::linking::find_link_for_marker;
use lamb_core::derivation::markers::{compute_markers_for_view, is_near_marker};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::geometry::Point2;
use lamb_core::geometry::polyline::{closest_point_on_segment, resolved_segments};
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

/// LINK: a line, then the matching line in another view; repeatable.
#[derive(Default)]
struct Link {
    a: Option<Picked>,
}

impl Interactive for Link {
    fn name(&self) -> &'static str {
        "link"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        let msg =
            if self.a.is_some() { "Pick the matching line in another view (Esc/Enter to end)" } else { "Select a line to link (Esc/Enter to end)" };
        Prompt::new(msg, Accept::POINT)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let p = match i {
            Input::Enter | Input::Cancel => return Ok(Step::Done),
            Input::Point(p) => p,
            _ => return Ok(Step::Continue),
        };
        let Some(picked) = pick_line(s, p) else {
            s.echo(NO_LINE);
            return Ok(Step::Continue);
        };
        let Some(a) = self.a.take() else {
            self.a = Some(picked);
            return Ok(Step::Continue);
        };
        // Repeatable: back to the first prompt whatever the outcome.
        match s.execute("link.apply", &json!({ "a": a.json(), "b": picked.json() })) {
            Ok(_) => s.echo("Linked"),
            Err(e) => s.echo(format!("Error: {e}")),
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

    const FIRST: &str = "Select a line to link (Esc/Enter to end)";
    const SECOND: &str = "Pick the matching line in another view (Esc/Enter to end)";

    #[test]
    fn link_picks_a_line_then_its_match_in_another_view_and_repeats() -> R {
        let mut f = fixture()?;
        let s = &mut f.s;
        let base = links(s)?.len();
        s.start("link")?;
        assert_eq!(prompt(s).as_deref(), Some(FIRST));
        s.input(Input::Point(Vec2::new(3000.0, 0.0)))?;
        assert_eq!(prompt(s).as_deref(), Some(SECOND));
        // Mid-command, the user clicks the section's tile, then the crossing.
        let crossing = marker_at(s, f.section, f.plan_line)?;
        enter_space(s, Space::Block(view_block_name(f.section)))?;
        s.input(Input::Point(crossing))?;
        assert!(s.log.iter().any(|l| l == "Linked"), "{:?}", s.log);
        let now = links(s)?;
        assert_eq!(now.len(), base + 1);
        let l = now.last().ok_or("no link")?;
        assert_eq!(l.origin, LinkOrigin::Explicit);
        assert_eq!((l.a.entity_id, l.a.segment_index), (f.plan_line, 0));
        assert!((l.a.segment_param - 0.3).abs() < 1e-9, "{}", l.a.segment_param);
        assert_eq!((l.b.entity_id, l.b.segment_index), (f.section_line, 0));
        let want = (crossing.y + 5000.0) / 10000.0;
        assert!((l.b.segment_param - want).abs() < 1e-9, "{} vs {want}", l.b.segment_param);
        assert_eq!(prompt(s).as_deref(), Some(FIRST), "repeatable");
        assert!(s.running.is_some());
        // A second pair in the same run, back from the section to a plan line it does not cross:
        // refused (LAMB T514), and the run goes on.
        s.input(Input::Point(Vec2::new(5000.0, 4000.0)))?;
        enter_space(s, Space::Block(view_block_name(f.plan)))?;
        s.input(Input::Point(Vec2::new(8000.0, 2000.0)))?;
        assert!(s.log.iter().any(|l| l.contains("Those lines do not cross at a marker")), "{:?}", s.log);
        assert_eq!(links(s)?.len(), base + 1);
        assert_eq!(prompt(s).as_deref(), Some(FIRST), "still repeatable");
        s.input(Input::Enter)?;
        assert!(s.running.is_none(), "Enter ends it");
        s.undo()?;
        assert_eq!(links(s)?.len(), base, "one undo step for the link, none for the refusal");
        Ok(())
    }

    #[test]
    fn link_misses_and_same_view_pairs_echo_and_keep_prompting() -> R {
        let mut f = fixture()?;
        let s = &mut f.s;
        let base = links(s)?.len();
        s.start("lk")?;
        let n = s.log.len();
        s.input(Input::Point(Vec2::new(-50000.0, -50000.0)))?;
        assert_eq!(&s.log[n..], [NO_LINE.to_owned()]);
        assert_eq!(prompt(s).as_deref(), Some(FIRST), "same prompt");
        s.input(Input::Point(Vec2::new(3000.0, 0.0)))?;
        s.input(Input::Point(Vec2::new(6000.0, 2000.0)))?;
        assert!(s.log[n..].iter().any(|l| l.contains("Link ends must be in different views")), "{:?}", &s.log[n..]);
        assert_eq!(links(s)?.len(), base);
        assert_eq!(prompt(s).as_deref(), Some(FIRST));
        Ok(())
    }

    #[test]
    fn unlink_clicks_a_filled_marker_and_undo_restores_it() -> R {
        let mut f = fixture()?;
        let s = &mut f.s;
        let crossing = marker_at(s, f.section, f.plan_line)?;
        s.start("link")?;
        s.input(Input::Point(Vec2::new(3000.0, 0.0)))?;
        enter_space(s, Space::Block(view_block_name(f.section)))?;
        s.input(Input::Point(crossing))?;
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

//! RESOLVE — LAMB ▸ Walls & Framing ▸ Review Issues — (LAMB T351, TS `resolveCommand.ts`, T158): step through
//! conflicting data in the drawing, then the gaps in its drawing set. Reported: lamb-core's
//! [`find_issues`] conflicts and stale links, each plan polyline whose linked lines disagree on its
//! height ([`polyline_height_conflict`], which takes a sloped line as a plane), each marker with more
//! than one same-layer line through it ([`crowded_marker_issues`], LAMB T650), then the assembly
//! plans' own gaps ([`wall_issues`], LAMB T393: width conflicts and junctions with no corner
//! detail, then runs with no detail (LAMB T471), for plans that have an assembly block). Not
//! reported: a segment with no height of its own, whether defaulted or between two crossings: one
//! vertical reference sets a whole polyline's height, and none is a working default rather than a
//! conflict. Each step edits the issue's view, centres the camera on its location (block coordinates
//! are the view's local coordinates, so no transform), names it on the command line and haloes its
//! entities ([`crate::overlays::halo`]). The list is snapshotted when the command starts and never
//! re-derived mid-command; `Next`/`Previous` (or a typed `>`/`<`) step, clamped at both ends;
//! Enter or Esc ends it. The halo goes when the machine is dropped — however the command ends.
//!
//! The JSON form `resolve {}` returns `{issues:[{id, kind, viewId, location:{x,y}, entityIds,
//! note}]}` and changes nothing.

use cadcraft_doc::{Handle, Space};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step, View};
use cadcraft_geom::Vec2;
use lamb_core::derivation::issues::{Issue, IssueKind, find_issues};
use lamb_core::derivation::read::{ProjectRead, ViewKind};
use lamb_core::derivation::resolve::polyline_height_conflict;
use lamb_core::model::LineRole;
use serde_json::{Value, json};

use crate::assembly::wall_issues;
use crate::commands::{enter_space, other};
use crate::linkcmds::crowded_marker_issues;
use crate::overlays::halo::set_attention;
use crate::project::DrawingProject;
use crate::schema::view_block_name;

fn issues(s: &Session) -> Result<Vec<Issue>> {
    let d = s.doc()?;
    let dp = DrawingProject::new(d).map_err(other)?;
    let mut list = find_issues(&dp).map_err(other)?;
    list.retain(|i| !matches!(i.kind, IssueKind::DefaultedExtent | IssueKind::AmbiguousExtent));
    let conflicts = polyline_conflicts(&dp, &list)?;
    list.extend(conflicts);
    list.extend(crowded_marker_issues(d).map_err(other)?);
    list.sort_by_key(|i| i.priority);
    list.extend(wall_issues(d).map_err(other)?);
    Ok(list)
}

/// One issue per plan polyline whose linked lines disagree on its height, leaving out a polyline
/// `listed` already holds a same-point conflict for.
fn polyline_conflicts(dp: &DrawingProject<'_>, listed: &[Issue]) -> Result<Vec<Issue>> {
    let mut found = Vec::new();
    for view in dp.views().filter(|v| v.kind == ViewKind::Plan && v.placement.is_some()) {
        for entity in dp.entities(view.id).filter(|e| dp.role_of(e) == LineRole::Cut) {
            let same_point = format!("conflicted_extent:{}:", entity.id);
            if listed.iter().any(|i| i.id.starts_with(&same_point)) {
                continue;
            }
            let Some((location, note)) = polyline_height_conflict(dp, &entity).map_err(other)? else { continue };
            let kind = IssueKind::ConflictedExtent;
            found.push(Issue {
                id: format!("conflicted_extent:polyline:{}", entity.id),
                kind,
                view_id: view.id,
                location,
                entity_ids: vec![entity.id],
                priority: kind.priority(),
                note,
            });
        }
    }
    Ok(found)
}

/// RESOLVE `{}`: the project's conflicts and stale links by priority, then [`wall_issues`].
pub(crate) fn resolve(s: &mut Session, _p: &Value) -> Result<Value> {
    let list: Vec<Value> = issues(s)?
        .iter()
        .map(|i| {
            json!({
                "id": i.id,
                "kind": i.kind,
                "viewId": i.view_id.0,
                "location": { "x": i.location.x, "y": i.location.y },
                "entityIds": i.entity_ids.iter().map(|e| e.0).collect::<Vec<_>>(),
                "note": i.note,
            })
        })
        .collect();
    Ok(json!({ "issues": list }))
}

/// RESOLVE: `<i>/<n> — <note> [Next/Previous]:`.
struct Resolve {
    issues: Vec<Issue>,
    at: usize,
}

impl Resolve {
    fn line(&self) -> String {
        let note = self.issues.get(self.at).map_or("", |i| i.note.as_str());
        format!("{}/{} — {note}", self.at + 1, self.issues.len())
    }

    /// Show issue [`Self::at`]: its view, centred on it, haloed, named.
    fn show(&self, s: &mut Session) -> Result<()> {
        let Some(issue) = self.issues.get(self.at) else { return Ok(()) };
        let block = view_block_name(issue.view_id);
        enter_space(s, Space::Block(block.clone()))?;
        let st = s.state_mut()?;
        let v = st.view();
        st.set_view(View { center: Vec2::new(issue.location.x, issue.location.y), height: v.height, twist: v.twist });
        set_attention(Some((block, issue.entity_ids.iter().map(|id| Handle(id.0)).collect())));
        s.echo(self.line());
        Ok(())
    }

    fn step(&mut self, s: &mut Session, forward: bool) -> Result<Step> {
        let to = if forward { (self.at + 1).min(self.issues.len().saturating_sub(1)) } else { self.at.saturating_sub(1) };
        if to != self.at {
            self.at = to;
            self.show(s)?;
        }
        Ok(Step::Continue)
    }
}

impl Drop for Resolve {
    fn drop(&mut self) {
        set_attention(None);
    }
}

impl Interactive for Resolve {
    fn name(&self) -> &'static str {
        "resolve"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new(self.line(), Accept::TEXT).kw(&["Next", "Previous"])
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        s.echo("Review Issues: steps through conflicts in the drawing; Next/Previous to move, Enter to finish.");
        if self.issues.is_empty() {
            s.echo("No issues found");
            return Ok(Step::Done);
        }
        self.show(s)?;
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let text = match i {
            Input::Enter => return Ok(Step::Done),
            Input::Cancel => return Ok(Step::Cancel),
            Input::Text(t) | Input::Keyword(t) => t.trim().to_ascii_lowercase(),
            _ => return Ok(Step::Continue),
        };
        match text.as_str() {
            "" => Ok(Step::Done),
            ">" | "n" | "next" => self.step(s, true),
            "<" | "p" | "previous" => self.step(s, false),
            _ => Err(other("Enter Next, Previous, > or <.")),
        }
    }
}

pub(crate) fn resolve_machine(s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(Resolve { issues: issues(s)?, at: 0 }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlays::halo::{ATTENTION_COLOR, current_attention};
    use crate::overlays::lamb_overlay;
    use crate::schema::{read_document, write_document};
    use cadcraft_render::Options;
    use lamb_core::model::{Actor, Id, LinkAnchor, LinkOrigin, LinkRecord};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type E = Box<dyn std::error::Error>;

    /// The entity `cmd` adds to `view`.
    fn draw(s: &mut Session, view: Id, cmd: &str, args: Value) -> std::result::Result<Id, E> {
        let block = view_block_name(view);
        s.execute("editview", &json!({ "viewId": view.0 }))?;
        let before: Vec<Handle> = s.doc()?.block(&block).map(|b| b.entities.iter().map(|e| e.handle).collect()).unwrap_or_default();
        s.execute(cmd, &args)?;
        let b = s.doc()?.block(&block).ok_or("no block")?;
        let h = b.entities.iter().map(|e| e.handle).find(|h| !before.contains(h)).ok_or("line not drawn")?;
        Ok(Id(h.0))
    }

    fn new_line(s: &mut Session, view: Id, points: Value) -> std::result::Result<Id, E> {
        draw(s, view, "line", json!({ "points": points }))
    }

    fn new_pline(s: &mut Session, view: Id, vertices: Value, closed: bool) -> std::result::Result<Id, E> {
        draw(s, view, "pline", json!({ "vertices": vertices, "closed": closed }))
    }

    /// A section of `plan` cut along `from`–`to`.
    fn new_section(s: &mut Session, plan: Id, name: &str, from: [f64; 2], to: [f64; 2]) -> std::result::Result<Id, E> {
        let r = s.execute(
            "newsectionline",
            &json!({ "name": name, "hostViewId": plan.0, "cutFrom": {"x": from[0], "y": from[1]}, "cutTo": {"x": to[0], "y": to[1]} }),
        )?;
        Ok(Id(r["viewId"].as_u64().ok_or("no section")?))
    }

    /// A link written straight to the document: the LINK command only takes lines through a marker
    /// at the plan's cut elevation, and these heights are the point of the tests.
    fn link(s: &mut Session, a: (Id, usize, f64), b: (Id, f64)) -> std::result::Result<(), E> {
        let d = s.doc_mut()?;
        let mut doc = read_document(d)?;
        let id = Id(d.new_handle().0);
        doc.links.push(LinkRecord {
            id,
            a: LinkAnchor { entity_id: a.0, segment_index: a.1, segment_param: a.2 },
            b: LinkAnchor { entity_id: b.0, segment_index: 0, segment_param: b.1 },
            origin: LinkOrigin::Explicit,
            created_at: String::new(),
            created_by: Actor::User { user_id: "local".into() },
            stale: None,
        });
        write_document(d, &doc)?;
        Ok(())
    }

    fn new_project() -> std::result::Result<(Session, Id), E> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no plan")?);
        Ok((s, plan))
    }

    /// A vertical section line through the crossing at u = 5000, from z = 0 to `top`.
    fn wall_line(s: &mut Session, section: Id, top: f64) -> std::result::Result<Id, E> {
        new_line(s, section, json!([[5000, 0], [5000, top]]))
    }

    /// A plan line along y, crossed at x = 3000 and x = 7000 by section lines 2700 and 3000 tall.
    fn conflicted_line(s: &mut Session, plan: Id, y: f64) -> std::result::Result<Id, E> {
        let line = new_line(s, plan, json!([[0, y], [10000, y]]))?;
        for (name, x, top) in [("a", 3000.0, 2700.0), ("b", 7000.0, 3000.0)] {
            let section = new_section(s, plan, &format!("{name}{}", plan.0), [x, y - 5000.0], [x, y + 5000.0])?;
            let upright = wall_line(s, section, top)?;
            link(s, (line, 0, x / 10000.0), (upright, 0.5))?;
        }
        Ok(line)
    }

    /// Ground Floor and an offset plan, each with one polyline whose linked lines disagree.
    fn two_plans() -> std::result::Result<(Session, Id, Id), E> {
        let (mut s, a) = new_project()?;
        let r = s.execute("newoffsetplan", &json!({ "mode": "offset", "fromViewId": a.0, "floorToFloorMm": 3000 }))?;
        let b = Id(r["viewId"].as_u64().ok_or("no plan B")?);
        conflicted_line(&mut s, b, 0.0)?;
        conflicted_line(&mut s, a, -3000.0)?;
        Ok((s, a, b))
    }

    fn space(s: &Session) -> std::result::Result<Space, E> {
        Ok(s.state()?.space.clone())
    }

    fn at(s: &Session, issue: &Issue) -> std::result::Result<bool, E> {
        let c = s.state()?.view().center;
        Ok(space(s)? == Space::Block(view_block_name(issue.view_id)) && c.dist(Vec2::new(issue.location.x, issue.location.y)) < 1e-6)
    }

    fn attention_prims(s: &Session, issue: &Issue) -> std::result::Result<usize, E> {
        let list = lamb_overlay(s.doc()?, &Space::Block(view_block_name(issue.view_id)), &Options::default());
        Ok(list.prims.iter().filter(|p| p.color == ATTENTION_COLOR && issue.entity_ids.iter().any(|e| Handle(e.0) == p.handle)).count())
    }

    #[test]
    fn the_json_form_lists_each_polylines_conflict() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        let (mut s, a, b) = two_plans()?;
        let found = issues(&s)?;
        assert_eq!(found.len(), 2, "{found:?}");
        let views: Vec<Id> = found.iter().map(|i| i.view_id).collect();
        assert!(views.contains(&a) && views.contains(&b), "{views:?}");
        let r = s.execute("resolve", &json!({}))?;
        let got = r["issues"].as_array().ok_or("no issues")?;
        assert_eq!(got.len(), 2);
        for (g, i) in got.iter().zip(&found) {
            assert_eq!(g["id"], json!(i.id));
            assert_eq!(g["kind"], json!("conflicted_extent"));
            assert_eq!(g["viewId"], json!(i.view_id.0));
            assert_eq!(g["location"], json!({ "x": i.location.x, "y": i.location.y }));
            assert_eq!(g["entityIds"], json!(i.entity_ids.iter().map(|e| e.0).collect::<Vec<_>>()));
            assert_eq!(g["note"], json!(i.note));
        }
        Ok(())
    }

    #[test]
    fn stepping_edits_centres_haloes_and_clamps() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        set_attention(None);
        let (mut s, _a, _b) = two_plans()?;
        let found = issues(&s)?;
        assert_eq!(found.len(), 2);
        s.start("resolve")?;
        assert!(s.running.is_some());
        assert!(at(&s, &found[0])?, "issue 1's view, centred");
        assert_eq!(s.current_prompt().map(|p| p.message.clone()), Some(format!("1/2 — {}", found[0].note)));
        assert_eq!(attention_prims(&s, &found[0])?, found[0].entity_ids.len());

        s.input(Input::Keyword("Previous".into()))?;
        assert!(at(&s, &found[0])?, "< at the start changes nothing");
        s.input(Input::Keyword("Next".into()))?;
        assert!(at(&s, &found[1])?);
        assert_eq!(s.current_prompt().map(|p| p.message.clone()), Some(format!("2/2 — {}", found[1].note)));
        assert!(s.log.iter().any(|l| *l == format!("2/2 — {}", found[1].note)), "named on the command line");
        assert_eq!(attention_prims(&s, &found[1])?, found[1].entity_ids.len());
        assert_eq!(attention_prims(&s, &found[0])?, 0, "only the current issue");
        s.input(Input::Text(">".into()))?;
        assert!(at(&s, &found[1])?, "> at the end changes nothing");
        s.input(Input::Text("<".into()))?;
        assert!(at(&s, &found[0])?);

        s.input(Input::Cancel)?;
        assert!(s.running.is_none());
        assert_eq!(current_attention(), None, "Esc clears the halo");
        assert_eq!(attention_prims(&s, &found[0])?, 0);

        s.start("resolve")?;
        assert!(current_attention().is_some());
        s.start("underlay")?;
        assert_eq!(current_attention(), None, "another command clears it");
        s.input(Input::Cancel)?;
        s.start("resolve")?;
        s.input(Input::Enter)?;
        assert!(s.running.is_none(), "Enter ends it");
        assert_eq!(current_attention(), None);
        Ok(())
    }

    #[test]
    fn resolve_steps_onto_an_undetailed_run_with_its_faces_haloed() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        set_attention(None);
        let (mut s, plan) = crate::assembly::tests::room_with("exterior_face", false, 200.0)?;
        let core = find_issues(&DrawingProject::new(s.doc()?)?)?.len();
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let found = issues(&s)?;
        assert_eq!(found.len(), core + 4);
        let first = &found[core];
        assert_eq!(first.id, format!("undetailed_run:{plan}:0"));
        s.start("resolve")?;
        for _ in 0..core {
            s.input(Input::Keyword("Next".into()))?;
        }
        assert!(at(&s, first)?, "the plan's view, centred on the run midpoint");
        let (block, handles) = current_attention().ok_or("no halo")?;
        assert_eq!(block, view_block_name(plan));
        assert_eq!(handles.len(), 2);
        assert_eq!(handles, first.entity_ids.iter().map(|e| Handle(e.0)).collect::<Vec<_>>());
        s.input(Input::Cancel)?;
        Ok(())
    }

    #[test]
    fn wall_issues_follow_find_issues_and_step_to_the_run() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        set_attention(None);
        let (mut s, plan) = crate::assembly::tests::room_with("exterior_face", true, 210.0)?;
        let core = find_issues(&DrawingProject::new(s.doc()?)?)?.len();
        assert_eq!(issues(&s)?.len(), core, "no assembly block yet");
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let r = s.execute("resolve", &json!({}))?;
        let got = r["issues"].as_array().ok_or("no issues")?;
        assert_eq!(got.len(), core + 8, "{r}");
        let kinds: Vec<&str> = got[core..].iter().filter_map(|i| i["kind"].as_str()).collect();
        assert_eq!(kinds, [["width_conflict"; 4], ["undetailed_corner"; 4]].concat());
        assert_eq!(got[core]["id"], json!(format!("width_conflict:{}:0", plan)));

        let found = issues(&s)?;
        let first = &found[core];
        s.start("resolve")?;
        for _ in 0..core {
            s.input(Input::Keyword("Next".into()))?;
        }
        assert!(at(&s, first)?, "the plan's view, centred on the run midpoint");
        assert_eq!(first.entity_ids.len(), 2);
        let (block, handles) = current_attention().ok_or("no halo")?;
        assert_eq!(block, view_block_name(plan));
        assert_eq!(handles, first.entity_ids.iter().map(|e| Handle(e.0)).collect::<Vec<_>>());
        s.input(Input::Cancel)?;
        Ok(())
    }

    /// A three-segment plan polyline; section A crosses its first segment at (3000, 0), u = 5000.
    fn three_segments(top: Option<f64>) -> std::result::Result<(Session, Id, Id), E> {
        let (mut s, plan) = new_project()?;
        let line = new_pline(&mut s, plan, json!([[0, 0], [6000, 0], [6000, 4000], [9000, 4000]]), false)?;
        if let Some(top) = top {
            let section = new_section(&mut s, plan, "A-A", [3000.0, -5000.0], [3000.0, 5000.0])?;
            let upright = wall_line(&mut s, section, top)?;
            link(&mut s, (line, 0, 0.5), (upright, 0.5))?;
        }
        Ok((s, plan, line))
    }

    #[test]
    fn one_vertical_reference_sets_the_whole_polyline_in_the_review_and_in_3d() -> R {
        let (mut s, _plan, line) = three_segments(Some(2700.0))?;
        let r = s.execute("resolve", &json!({}))?;
        assert_eq!(r["issues"], json!([]));

        let surfaces = lamb_core::derivation::synthesis::synthesize(&DrawingProject::new(s.doc()?)?)?.surfaces;
        for segment in 0..3 {
            let surface = surfaces.iter().find(|f| f.entity_id == line && f.segment_index == segment).ok_or("no surface")?;
            assert_eq!(surface.provenance, lamb_core::derivation::resolve::ProvenanceKind::Authored, "segment {segment}");
            let (low, high) = surface.triangles.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), p| (l.min(p.z), h.max(p.z)));
            assert_eq!((low, high), (0.0, 2700.0), "segment {segment}");
        }
        Ok(())
    }

    #[test]
    fn a_polyline_with_no_section_reports_nothing() -> R {
        let (mut s, _plan, _line) = three_segments(None)?;
        assert_eq!(s.execute("resolve", &json!({}))?["issues"], json!([]));
        Ok(())
    }

    #[test]
    fn two_segments_at_different_heights_report_once_at_the_second() -> R {
        let (mut s, plan, line) = three_segments(Some(2700.0))?;
        let section = new_section(&mut s, plan, "B-B", [7500.0, -1000.0], [7500.0, 9000.0])?;
        let upright = wall_line(&mut s, section, 3000.0)?;
        link(&mut s, (line, 2, 0.5), (upright, 0.5))?;
        let r = s.execute("resolve", &json!({}))?;
        let got = r["issues"].as_array().ok_or("no issues")?;
        assert_eq!(got.len(), 1, "{r}");
        assert_eq!(got[0]["id"], json!(format!("conflicted_extent:polyline:{line}")));
        assert_eq!(got[0]["kind"], json!("conflicted_extent"));
        assert_eq!(got[0]["viewId"], json!(plan.0));
        assert_eq!(got[0]["location"], json!({ "x": 7500.0, "y": 4000.0 }));
        assert_eq!(got[0]["entityIds"], json!([line.0]));
        assert_eq!(got[0]["note"], json!("This polyline's linked lines disagree on its height."));
        Ok(())
    }

    /// An 8000 x 5000 ceiling: a diagonal in section A rising 0.2 per mm of y (z = 2800 at y =
    /// 2000), a horizontal at `z` in perpendicular section B.
    fn sloped_ceiling(z: f64) -> std::result::Result<Session, E> {
        let (mut s, plan) = new_project()?;
        let ceiling = new_pline(&mut s, plan, json!([[0, 0], [8000, 0], [8000, 5000], [0, 5000]]), true)?;
        let a = new_section(&mut s, plan, "A-A", [3000.0, -1000.0], [3000.0, 6000.0])?;
        let diagonal = new_line(&mut s, a, json!([[1000, 2400], [6000, 3400]]))?;
        link(&mut s, (ceiling, 0, 0.375), (diagonal, 0.0))?;
        link(&mut s, (ceiling, 2, 0.625), (diagonal, 1.0))?;
        let b = new_section(&mut s, plan, "B-B", [-1000.0, 2000.0], [9000.0, 2000.0])?;
        let flat = new_line(&mut s, b, json!([[500, z], [9500, z]]))?;
        link(&mut s, (ceiling, 1, 0.4), (flat, 8500.0 / 9000.0))?;
        Ok(s)
    }

    #[test]
    fn a_horizontal_cut_on_the_diagonals_plane_is_fully_specified() -> R {
        let mut s = sloped_ceiling(2800.0)?;
        let r = s.execute("resolve", &json!({}))?;
        assert_eq!(r["issues"], json!([]));
        Ok(())
    }

    #[test]
    fn a_horizontal_cut_off_the_diagonals_plane_conflicts() -> R {
        let mut s = sloped_ceiling(3000.0)?;
        let r = s.execute("resolve", &json!({}))?;
        let got = r["issues"].as_array().ok_or("no issues")?;
        assert_eq!(got.len(), 1, "{r}");
        assert_eq!(got[0]["kind"], json!("conflicted_extent"));
        assert_eq!(got[0]["location"], json!({ "x": 8000.0, "y": 2000.0 }));
        Ok(())
    }

    #[test]
    fn no_issues_says_so_and_ends() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        s.execute("lambproject", &json!({}))?;
        s.start("res")?;
        assert!(s.running.is_none());
        assert!(s.log.iter().any(|l| l == "No issues found"), "{:?}", s.log);
        Ok(())
    }
}

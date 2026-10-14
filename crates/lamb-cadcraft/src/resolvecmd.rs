//! RESOLVE (LAMB T351, TS `resolveCommand.ts`, T158): step through lamb-core's [`find_issues`]
//! list. Each step edits the issue's view, centres the camera on its location (block coordinates
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
use lamb_core::derivation::issues::{Issue, find_issues};
use serde_json::{Value, json};

use crate::commands::{enter_space, other};
use crate::overlays::halo::set_attention;
use crate::project::DrawingProject;
use crate::schema::view_block_name;

fn issues(s: &Session) -> Result<Vec<Issue>> {
    let dp = DrawingProject::new(s.doc()?).map_err(other)?;
    find_issues(&dp).map_err(other)
}

/// RESOLVE `{}`: the project's issues in `find_issues` order.
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
    use cadcraft_render::Options;
    use lamb_core::model::Id;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type E = Box<dyn std::error::Error>;

    /// Ground Floor and an offset plan, both placed, with one uncrossed cut line each: one
    /// defaulted-extent issue per plan.
    fn two_plans() -> std::result::Result<(Session, Id, Id), E> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let a = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no plan")?);
        let r = s.execute("newoffsetplan", &json!({ "mode": "offset", "fromViewId": a.0, "floorToFloorMm": 3000 }))?;
        let b = Id(r["viewId"].as_u64().ok_or("no plan B")?);
        s.execute("editview", &json!({ "viewId": b.0 }))?;
        s.execute("line", &json!({ "points": [[0, 0], [1000, 0]] }))?;
        s.execute("editview", &json!({ "viewId": a.0 }))?;
        s.execute("line", &json!({ "points": [[-3000, -3000], [-2000, -3000]] }))?;
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
    fn the_json_form_lists_issues_in_find_issues_order() -> R {
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
            assert_eq!(g["kind"], json!("defaulted_extent"));
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

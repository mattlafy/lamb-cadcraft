//! HIDEVIEW / UNHIDEVIEW (LAMB T607): a view hidden has the effect of deleting it, reversibly.
//!
//! - `hideview {viewId?}` (the edited view when absent) moves the view out of
//!   [`LambDocument`]`.views` into `hidden_views`, with every link that has an end on one of its
//!   block's entities, and turns off its layer `LAMB_VIEW_<id>` (and `LAMB_ASSEMBLY_<id>` when it
//!   exists), remembering each layer's prior `on`. Every reader of the document then treats the view
//!   as gone: its cut lines, cut symbols, Views palette row and schedules disappear. Its block,
//!   layer, generated blocks and the document's per-view lists stay, so unhiding gets them back.
//!   Hiding the edited view returns to Model space. Returns `{droppedLinks}`.
//! - `unhideview {viewId}` puts the view back at the end of `views`, restores each layer's `on` and
//!   re-adds each set-aside link whose two ends are both in a visible view's block (the rest are
//!   dropped), and un-suppresses the proposals of its AI lines (the sweep after HIDEVIEW read them
//!   as erased). Returns `{restoredLinks, droppedLinks}`.
//!
//! Refusals are decided on a read, before `doc_mut`, so they record no undo step: `No such view`; a
//! plan hosting a visible section or elevation's cut line; `View is not hidden`. Each command is
//! one undo step. Interactively HIDEVIEW acts on the edited view; UNHIDEVIEW asks `Enter hidden
//! view name or [?]:`, `?` listing the hidden views numbered.

use std::collections::HashSet;

use cadcraft_doc::{Drawing, Space};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use lamb_core::model::{AnyView, Id, Placement};
use serde_json::{Value, json};

use crate::commands::{bad, edited_view, enter_space, id_value, kind_word, other};
use crate::schema::{HiddenView, LambDocument, assembly_block_name, read_document, read_entity_data, view_block_name, write_document};

fn doc_of(s: &Session) -> Result<LambDocument> {
    read_document(s.doc()?).map_err(other)
}

fn block_ids(d: &Drawing, id: Id) -> Vec<Id> {
    d.block(&view_block_name(id)).map(|b| b.entities.iter().map(|e| Id(e.handle.0)).collect()).unwrap_or_default()
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// HIDEVIEW.
pub(crate) fn hideview(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "hideview";
    let id = match p.get("viewId").filter(|v| !v.is_null()) {
        Some(v) => id_value(v).ok_or_else(|| bad(CMD, "`viewId` must be a view id"))?,
        None => edited_view(s).ok_or_else(|| bad(CMD, "`viewId` is required (or edit a view first)"))?,
    };
    let mut doc = doc_of(s)?;
    let at = doc.views.iter().position(|v| v.id() == id).ok_or_else(|| other("No such view"))?;
    let hosted: Vec<&str> = doc
        .views
        .iter()
        .filter(|v| matches!(v.placement(), Some(Placement::Vertical { host_view_id, .. }) if *host_view_id == id))
        .map(AnyView::name)
        .collect();
    if !hosted.is_empty() {
        return Err(other(format!("{} hosts the cut lines of {}; hide those first", doc.views[at].name(), hosted.join(", "))));
    }
    let entities: HashSet<Id> = block_ids(s.doc()?, id).into_iter().collect();
    let (links, kept): (Vec<_>, Vec<_>) = doc.links.drain(..).partition(|l| entities.contains(&l.a.entity_id) || entities.contains(&l.b.entity_id));
    doc.links = kept;
    let dropped = links.len();
    let view = doc.views.remove(at);
    let d = s.doc_mut()?;
    let mut layers_on = Vec::new();
    for name in [view_block_name(id), assembly_block_name(id)] {
        if let Some(l) = d.layer_mut(&name) {
            layers_on.push((name, l.on));
            l.on = false;
        }
    }
    doc.hidden_views.push(HiddenView { view, links, layers_on });
    write_document(d, &doc).map_err(other)?;
    if edited_view_block(s) == Some(view_block_name(id)) {
        enter_space(s, Space::Model)?;
    }
    Ok(json!({ "droppedLinks": dropped }))
}

/// The block the session is editing, LAMB view or not.
fn edited_view_block(s: &Session) -> Option<String> {
    match &s.state().ok()?.space {
        Space::Block(name) => Some(name.clone()),
        _ => None,
    }
}

/// UNHIDEVIEW.
pub(crate) fn unhideview(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "unhideview";
    let id = p.get("viewId").and_then(id_value).ok_or_else(|| bad(CMD, "`viewId` is required"))?;
    let mut doc = doc_of(s)?;
    let at = doc.hidden_views.iter().position(|h| h.view.id() == id).ok_or_else(|| other("View is not hidden"))?;
    let hidden = doc.hidden_views.remove(at);
    doc.views.push(hidden.view);
    let d = s.doc()?;
    let visible: HashSet<Id> = doc.views.iter().flat_map(|v| block_ids(d, v.id())).collect();
    let (back, gone): (Vec<_>, Vec<_>) = hidden.links.into_iter().partition(|l| visible.contains(&l.a.entity_id) && visible.contains(&l.b.entity_id));
    let restored = back.len();
    doc.links.extend(back);
    // Hiding took the view's AI lines out of every view block, which the after-command sweep reads
    // as erasing them and suppresses their slots; the lines are back, so their slots are filled.
    let proposals: HashSet<String> = d
        .block(&view_block_name(id))
        .into_iter()
        .flat_map(|b| b.entities.iter())
        .filter_map(|e| read_entity_data(&e.common).ok()?.ai_origin.map(|o| o.proposal_id))
        .collect();
    doc.suppressed_proposals.retain(|k| !proposals.contains(k));
    let d = s.doc_mut()?;
    for (name, on) in &hidden.layers_on {
        if let Some(l) = d.layer_mut(name) {
            l.on = *on;
        }
    }
    write_document(d, &doc).map_err(other)?;
    Ok(json!({ "restoredLinks": restored, "droppedLinks": gone.len() }))
}

// ---------------- interactive machines ----------------

/// HIDEVIEW on the edited view, no prompt.
struct HideView;

impl Interactive for HideView {
    fn name(&self) -> &'static str {
        "hideview"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Hide view", Accept::TEXT)
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let Some(id) = edited_view(s) else {
            s.echo("Edit a view first (EDITVIEW).");
            return Ok(Step::Done);
        };
        let name = doc_of(s)?.views.iter().find(|v| v.id() == id).map(|v| v.name().to_owned()).unwrap_or_default();
        match hideview(s, &json!({ "viewId": id.0 })) {
            Ok(r) => {
                let n = r["droppedLinks"].as_u64().unwrap_or(0) as usize;
                s.echo(format!("{name} hidden; {n} link{} set aside.", plural(n)));
            }
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
    fn input(&mut self, _s: &mut Session, _i: Input) -> Result<Step> {
        Ok(Step::Done)
    }
}

pub(crate) fn hideview_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(HideView))
}

/// UNHIDEVIEW: `Enter hidden view name or [?]:`.
struct UnhideView;

impl Interactive for UnhideView {
    fn name(&self) -> &'static str {
        "unhideview"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter hidden view name or [?]", Accept::TEXT)
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if doc_of(s)?.hidden_views.is_empty() {
            s.echo("No hidden views.");
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let text = match i {
            Input::Cancel => return Ok(Step::Cancel),
            Input::Text(t) | Input::Keyword(t) => t.trim().to_owned(),
            _ => return Ok(Step::Continue),
        };
        let doc = doc_of(s)?;
        if text == "?" {
            let rows: Vec<String> = doc.hidden_views.iter().map(|h| format!("{}  {}", kind_word(&h.view), h.view.name())).collect();
            crate::choices::echo_numbered(s, &rows);
            return Ok(Step::Continue);
        }
        let names: Vec<&str> = doc.hidden_views.iter().map(|h| h.view.name()).collect();
        let Some(h) = crate::choices::pick_numbered(&names, &text).map(|n| &doc.hidden_views[n]) else {
            s.echo("No such hidden view");
            return Ok(Step::Continue);
        };
        let name = h.view.name().to_owned();
        match unhideview(s, &json!({ "viewId": h.view.id().0 })) {
            Ok(r) => {
                let n = r["restoredLinks"].as_u64().unwrap_or(0) as usize;
                s.echo(format!("{name} restored; {n} link{} restored.", plural(n)));
            }
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

pub(crate) fn unhideview_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(UnhideView))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::install_app;
    use crate::import::project_to_drawing;
    use crate::schema::annotation_block_name;
    use crate::views::browser_rows;
    use lamb_core::model::Project;
    use lamb_core::persistence::files_to_project;
    use std::collections::BTreeMap;
    use std::path::Path;

    type E<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    /// The imported u-wall-two-sections: plan `P` hosting sections `Legs` (linked) and `Crossbar`.
    fn u_wall() -> E<Session> {
        install_app().map_err(|e| e.to_string())?;
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb/u-wall-two-sections.lamb.json");
        let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        let project: Project = files_to_project(&files).map_err(|e| format!("{e:?}"))?;
        let (d, _, _) = project_to_drawing(&project)?;
        let mut s = Session::new();
        s.open_drawing(d, "u-wall", None);
        crate::maintain::install(&mut s);
        // Settle every observer once, so later comparisons see only the command under test.
        s.execute("layer.new", &json!({ "name": "SETTLE" }))?;
        Ok(s)
    }

    fn id_of(s: &Session, name: &str) -> E<Id> {
        Ok(read_document(s.doc()?)?.views.iter().find(|v| v.name() == name).ok_or(format!("no view {name}"))?.id())
    }

    fn anno_len(s: &Session, plan: Id) -> E<usize> {
        Ok(s.doc()?.block(&annotation_block_name(plan)).map_or(0, |b| b.entities.len()))
    }

    fn layer_on(s: &Session, id: Id) -> E<bool> {
        Ok(s.doc()?.layer(&view_block_name(id)).ok_or("no layer")?.on)
    }

    fn has_row(s: &Session, name: &str) -> E<bool> {
        Ok(browser_rows(s.doc()?, &Space::Model).iter().any(|r| r.label == name))
    }

    fn undo_len(s: &Session) -> E<usize> {
        Ok(s.state()?.undo.len())
    }

    #[test]
    fn hiding_a_section_removes_it_everywhere_and_unhiding_brings_it_back() -> E<()> {
        let mut s = u_wall()?;
        let plan = id_of(&s, "P")?;
        let legs = id_of(&s, "Legs")?;
        let entities = block_ids(s.doc()?, legs);
        let links = read_document(s.doc()?)?.links;
        let touching = links.iter().filter(|l| entities.contains(&l.a.entity_id) || entities.contains(&l.b.entity_id)).count();
        assert!(touching > 0, "fixture: Legs is linked");
        let symbols = anno_len(&s, plan)?;
        assert!(symbols > 0, "fixture: P draws cut symbols");
        assert!(layer_on(&s, legs)?, "layer on");
        assert!(has_row(&s, "Legs")?, "{:?}", browser_rows(s.doc()?, &Space::Model).iter().map(|r| r.label.clone()).collect::<Vec<_>>());
        let shown = s.doc()?.clone();

        let r = s.execute("hideview", &json!({ "viewId": legs.0 }))?;
        assert_eq!(r["droppedLinks"], json!(touching));
        let doc = read_document(s.doc()?)?;
        assert!(!doc.views.iter().any(|v| v.id() == legs), "out of views");
        assert_eq!(doc.hidden_views.len(), 1);
        assert!(doc.links.iter().all(|l| !entities.contains(&l.a.entity_id) && !entities.contains(&l.b.entity_id)));
        assert!(anno_len(&s, plan)? < symbols, "its cut symbol is gone");
        assert!(!layer_on(&s, legs)?, "layer off");
        assert!(!has_row(&s, "Legs")?, "no Views palette row");
        assert_eq!(block_ids(s.doc()?, legs), entities, "linework kept");
        let hidden = s.doc()?.clone();

        let r = s.execute("unhideview", &json!({ "viewId": legs.0 }))?;
        assert_eq!((r["restoredLinks"].clone(), r["droppedLinks"].clone()), (json!(touching), json!(0)));
        let doc = read_document(s.doc()?)?;
        assert!(doc.views.iter().any(|v| v.id() == legs) && doc.hidden_views.is_empty());
        let mut back = doc.links.clone();
        let mut before = links.clone();
        back.sort_by_key(|l| l.id.0);
        before.sort_by_key(|l| l.id.0);
        assert_eq!(back, before, "links restored");
        assert_eq!(block_ids(s.doc()?, legs), entities, "AI lines kept");
        assert_eq!(read_document(s.doc()?)?.suppressed_proposals, read_document(&shown)?.suppressed_proposals);
        assert_eq!(anno_len(&s, plan)?, symbols, "cut symbol back");
        assert!(layer_on(&s, legs)? && has_row(&s, "Legs")?);

        let e = s.execute("unhideview", &json!({ "viewId": legs.0 })).err().ok_or("accepted")?;
        assert!(e.to_string().contains("View is not hidden"), "{e}");

        s.undo()?;
        assert_eq!(*s.doc()?, hidden, "undo of UNHIDEVIEW");
        s.undo()?;
        assert_eq!(*s.doc()?, shown, "undo of HIDEVIEW");
        Ok(())
    }

    #[test]
    fn hiding_a_host_plan_refuses_with_no_undo_step() -> E<()> {
        let mut s = u_wall()?;
        let plan = id_of(&s, "P")?;
        let n = undo_len(&s)?;
        let e = s.execute("hideview", &json!({ "viewId": plan.0 })).err().ok_or("accepted")?;
        assert!(e.to_string().contains("P hosts the cut lines of Crossbar, Legs; hide those first"), "{e}");
        let e = s.execute("hideview", &json!({ "viewId": 999_999 })).err().ok_or("accepted")?;
        assert!(e.to_string().contains("No such view"), "{e}");
        assert_eq!(undo_len(&s)?, n);
        Ok(())
    }

    #[test]
    fn a_hidden_dimensioned_tagged_plan_leaves_every_observer_settled() -> E<()> {
        let mut s = u_wall()?;
        let plan = id_of(&s, "P")?;
        s.execute("autodim", &json!({ "view": plan.0 }))?;
        s.execute("walltags", &json!({ "viewId": plan.0 }))?;
        for name in ["Legs", "Crossbar", "P"] {
            let id = id_of(&s, name)?;
            s.execute("hideview", &json!({ "viewId": id.0 }))?;
        }
        let doc = read_document(s.doc()?)?;
        assert!(doc.views.is_empty() && doc.auto_dim_plans.contains(&plan) && doc.wall_tag_plans.contains(&plan));
        let settled = s.doc()?.clone();
        s.execute("layer.new", &json!({ "name": "NOOP" }))?;
        s.doc_mut()?.layers.retain(|l| l.name != "NOOP");
        assert_eq!(*s.doc()?, settled, "no observer changes anything");
        s.execute("unhideview", &json!({ "viewId": plan.0 }))?;
        assert!(read_document(s.doc()?)?.views.iter().any(|v| v.id() == plan));
        Ok(())
    }

    #[test]
    fn typed_hideview_leaves_the_view_and_unhideview_lists_and_picks() -> E<()> {
        let mut s = u_wall()?;
        let legs = id_of(&s, "Legs")?;
        let crossbar = id_of(&s, "Crossbar")?;
        s.execute("editview", &json!({ "viewId": crossbar.0 }))?;
        s.start("hideview")?;
        assert!(s.running.is_none());
        assert_eq!(s.state()?.space, Space::Model, "back in Model space");
        assert!(s.log.iter().any(|l| l == "Crossbar hidden; 1 link set aside."), "{:?}", s.log);
        s.execute("editview", &json!({ "viewId": legs.0 }))?;
        s.start("hideview")?;
        assert_eq!(s.state()?.space, Space::Model);

        s.start("unhideview")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter hidden view name or [?]:"));
        s.input(Input::Text("?".into()))?;
        assert!(s.log.iter().any(|l| l == "1  Section  Crossbar"), "{:?}", s.log);
        assert!(s.log.iter().any(|l| l == "2  Section  Legs"), "{:?}", s.log);
        s.input(Input::Text("nope".into()))?;
        assert!(s.log.iter().any(|l| l == "No such hidden view"));
        assert!(s.running.is_some(), "re-prompts");
        s.input(Input::Text("1".into()))?;
        assert!(s.running.is_none());
        let doc = read_document(s.doc()?)?;
        assert!(doc.views.iter().any(|v| v.id() == crossbar));
        assert_eq!(doc.hidden_views.iter().map(|h| h.view.id()).collect::<Vec<_>>(), vec![legs]);
        Ok(())
    }
}

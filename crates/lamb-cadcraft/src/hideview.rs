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
//! one undo step. Interactively HIDEVIEW acts on the edited view; UNHIDEVIEW asks `Select a hidden
//! view or enter its name [?]:`, `?` listing the hidden views numbered.
//!
//! While UNHIDEVIEW runs in a placed plan `P` (LAMB T719) it ghosts the hidden views
//! ([`ghosts`]): each hidden section's or elevation's cut line, and every entity of each hidden
//! plan on `P`'s level, in `P`'s local coordinates, drawn ACI 8 on layer `0` with the view's name
//! as a TEXT 12 px high. A click within 8 px of a ghost's linework restores the nearest one. The
//! Views palette lists the hidden views in a `Hidden` group while the command runs ([`unhiding`]).

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

use cadcraft_doc::color::Color;
use cadcraft_doc::{Common, Drawing, Entity, EntityKind, HAlign, Handle, Line, Prim, Space, Text, VAlign};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::{Mat3, Vec2};
use lamb_core::geometry::transform2d::{Transform2D, local_to_world2, world_to_local2};
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

/// Whether UNHIDEVIEW is the running command (the Views palette's `Hidden` group).
static UNHIDING: AtomicBool = AtomicBool::new(false);

/// Whether UNHIDEVIEW is running: the Views palette then lists the hidden views.
pub(crate) fn unhiding() -> bool {
    UNHIDING.load(Ordering::Relaxed)
}

fn set_unhiding(on: bool) {
    UNHIDING.store(on, Ordering::Relaxed);
}

/// One hidden view drawn in the edited plan: its linework and its name's position, in the plan's
/// local coordinates.
pub(crate) struct Ghost {
    pub id: Id,
    pub name: String,
    pub lines: Vec<EntityKind>,
    pub label_at: Vec2,
}

/// A plan transform's local -> global matrix (the inverse of `makeview.rs`'s `global_to_local`).
fn local_to_global(t: &Transform2D) -> Mat3 {
    Mat3::translate(Vec2::new(t.origin_x, t.origin_y)).then_before(Mat3::rotate(t.rotation))
}

fn global_to_local(t: &Transform2D) -> Mat3 {
    Mat3::rotate(-t.rotation).then_before(Mat3::translate(Vec2::new(-t.origin_x, -t.origin_y)))
}

fn plan_transform(v: &AnyView) -> Option<(Id, Transform2D)> {
    match v.placement() {
        Some(Placement::Plan { level_id, transform, .. }) => Some((*level_id, *transform)),
        _ => None,
    }
}

/// The hidden views ghosted while the edited space is a placed plan `P`: each hidden vertical view's
/// cut line (host-local -> global -> `P` local) and each hidden plan on `P`'s level (its block's
/// entities, hidden-local -> global -> `P` local). Hidden details, unplaced views and any other edit
/// space ghost nothing.
pub(crate) fn ghosts(s: &Session) -> Vec<Ghost> {
    let (Some(edited), Ok(doc), Ok(d)) = (edited_view(s), doc_of(s), s.doc()) else { return Vec::new() };
    let Some((level, pt)) = doc.views.iter().find(|v| v.id() == edited).and_then(plan_transform) else { return Vec::new() };
    let mut out = Vec::new();
    for h in &doc.hidden_views {
        let v = &h.view;
        let (lines, label_at) = match v.placement() {
            Some(Placement::Vertical { cut_line, host_view_id, .. }) => {
                let Some((_, ht)) = doc.views.iter().find(|x| x.id() == *host_view_id).and_then(plan_transform) else { continue };
                let pts: Vec<Vec2> = std::iter::once(cut_line.start)
                    .chain(cut_line.segments.iter().map(|g| g.to))
                    .map(|p| world_to_local2(&pt, local_to_world2(&ht, p)))
                    .map(|p| Vec2::new(p.x, p.y))
                    .collect();
                let lines: Vec<EntityKind> = pts.windows(2).map(|w| EntityKind::Line(Line { a: w[0].to3(0.0), b: w[1].to3(0.0) })).collect();
                (lines, pts[0])
            }
            Some(Placement::Plan { level_id, transform: ht, .. }) if *level_id == level => {
                let m = global_to_local(&pt).then_before(local_to_global(ht));
                let lines: Vec<EntityKind> = d
                    .block(&view_block_name(v.id()))
                    .into_iter()
                    .flat_map(|b| b.entities.iter())
                    .map(|e| {
                        let mut k = e.kind.clone();
                        k.transform(&m);
                        k
                    })
                    .collect();
                let grips: Vec<Vec2> = lines.iter().flat_map(EntityKind::grips).collect();
                let Some(x) = grips.iter().map(|p| p.x).reduce(f64::min) else { continue };
                let y = grips.iter().map(|p| p.y).fold(f64::MIN, f64::max);
                (lines, Vec2::new(x, y))
            }
            _ => continue,
        };
        if !lines.is_empty() {
            out.push(Ghost { id: v.id(), name: v.name().to_owned(), lines, label_at });
        }
    }
    out
}

/// The distance from `p` to `k`'s linework (segments and circles; other primitives are ignored).
fn distance(k: &EntityKind, p: Vec2) -> f64 {
    k.prims()
        .iter()
        .filter_map(|prim| match prim {
            Prim::Seg(g) => Some(g.closest(p).dist(p)),
            Prim::Circle(c) => Some(c.closest(p).dist(p)),
            _ => None,
        })
        .fold(f64::INFINITY, f64::min)
}

/// The ghost whose linework is nearest `p` within `aperture`.
fn ghost_at(ghosts: &[Ghost], p: Vec2, aperture: f64) -> Option<&Ghost> {
    ghosts
        .iter()
        .map(|g| (g, g.lines.iter().map(|k| distance(k, p)).fold(f64::INFINITY, f64::min)))
        .filter(|(_, d)| *d <= aperture)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(g, _)| g)
}

/// UNHIDEVIEW: `Select a hidden view or enter its name [?]:`.
struct UnhideView;

impl UnhideView {
    fn restore(s: &mut Session, id: Id, name: &str) -> Result<Step> {
        set_unhiding(false);
        match unhideview(s, &json!({ "viewId": id.0 })) {
            Ok(r) => {
                let n = r["restoredLinks"].as_u64().unwrap_or(0) as usize;
                s.echo(format!("{name} restored; {n} link{} restored.", plural(n)));
            }
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

impl Interactive for UnhideView {
    fn name(&self) -> &'static str {
        "unhideview"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Select a hidden view or enter its name [?]", Accept { point: true, text: true, enter: true, ..Accept::default() })
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if doc_of(s)?.hidden_views.is_empty() {
            s.echo("No hidden views.");
            return Ok(Step::Done);
        }
        set_unhiding(true);
        Ok(Step::Continue)
    }
    fn cancelled(&mut self, _s: &mut Session) {
        set_unhiding(false);
    }
    fn preview_objects(&self, s: &Session, _cursor: Vec2) -> Vec<Entity> {
        let common = Common { layer: "0".into(), color: Color::Index(8), ..Common::default() };
        let height = 12.0 * s.pixel_size();
        let mut out = Vec::new();
        for g in ghosts(s) {
            let label = Text {
                insert: g.label_at.to3(0.0),
                align_pt: None,
                height,
                value: g.name.clone(),
                rotation: 0.0,
                width_factor: 1.0,
                oblique: 0.0,
                style: "Standard".into(),
                halign: HAlign::Left,
                valign: VAlign::Baseline,
            };
            let kinds = g.lines.into_iter().chain([EntityKind::Text(label)]);
            out.extend(kinds.map(|kind| Entity { handle: Handle(0), common: common.clone(), kind }));
        }
        out
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let text = match i {
            Input::Cancel => {
                set_unhiding(false);
                return Ok(Step::Cancel);
            }
            Input::Point(p) => {
                let all = ghosts(s);
                let Some(g) = ghost_at(&all, p, 8.0 * s.pixel_size()) else {
                    s.echo("No hidden view there");
                    return Ok(Step::Continue);
                };
                let (id, name) = (g.id, g.name.clone());
                return Self::restore(s, id, &name);
            }
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
        let (id, name) = (h.view.id(), h.view.name().to_owned());
        Self::restore(s, id, &name)
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
        let _g = crate::overlays::underlay::tests::lock();
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
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Select a hidden view or enter its name [?]:"));
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

    /// LAMB T719: plan `Ground Floor` placed rotated and translated, hosting section `A-A`; plan
    /// `Same` on its level and `Other` on another, each holding one line; all three hidden, the
    /// ground plan edited. Returns (session, ground, A-A, Same).
    fn ghost_project() -> E<(Session, Id, Id, Id)> {
        install_app().map_err(|e| e.to_string())?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let ground = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no id")?);
        let level = read_document(s.doc()?)?.levels[0].id;
        let place = |s: &mut Session, v: Id, level: Id, x: f64, y: f64, r: f64| {
            let t = json!({ "originX": x, "originY": y, "rotation": r });
            s.execute("placeview", &json!({ "viewId": v.0, "kind": "plan", "levelId": level.0, "cutElevation": 1200, "transform": t }))
        };
        s.execute("unplaceview", &json!({ "viewId": ground.0 }))?;
        place(&mut s, ground, level, 1000.0, 2000.0, std::f64::consts::FRAC_PI_2)?;
        let r = s.execute(
            "newsectionline",
            &json!({ "name": "A-A", "hostViewId": ground.0, "cutFrom": {"x": 0, "y": 2000}, "cutTo": {"x": 5000, "y": 2000} }),
        )?;
        let section = Id(r["viewId"].as_u64().ok_or("no section")?);
        let same = Id(s.execute("newplan", &json!({ "name": "Same" }))?["viewId"].as_u64().ok_or("no id")?);
        place(&mut s, same, level, 500.0, -300.0, 0.3)?;
        let l2 = Id(s.execute("level", &json!({ "mode": "create", "name": "L2", "floorElevationMm": 3000 }))?["levelId"].as_u64().ok_or("no id")?);
        let other = Id(s.execute("newplan", &json!({ "name": "Other" }))?["viewId"].as_u64().ok_or("no id")?);
        place(&mut s, other, l2, 0.0, 0.0, 0.0)?;
        for v in [same, other] {
            s.execute("editview", &json!({ "viewId": v.0 }))?;
            crate::assembly::tests::line(&mut s, [0.0, 0.0], [1000.0, 0.0])?;
        }
        for v in [section, same, other] {
            s.execute("hideview", &json!({ "viewId": v.0 }))?;
        }
        s.execute("editview", &json!({ "viewId": ground.0 }))?;
        Ok((s, ground, section, same))
    }

    fn near(a: Vec2, b: Vec2) -> bool {
        a.dist(b) < 1e-6
    }

    fn ghost_lines(s: &Session) -> Vec<(Vec2, Vec2)> {
        s.preview(Vec2::ZERO)
            .into_iter()
            .filter_map(|e| match e.kind {
                EntityKind::Line(l) => Some((l.a.xy(), l.b.xy())),
                _ => None,
            })
            .collect()
    }

    fn texts(s: &Session) -> Vec<String> {
        s.preview(Vec2::ZERO)
            .into_iter()
            .filter_map(|e| match e.kind {
                EntityKind::Text(t) => Some(t.value),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn unhideview_ghosts_hidden_views_in_the_edited_plan() -> E<()> {
        let _g = crate::overlays::underlay::tests::lock();
        let (mut s, ground, section, same) = ghost_project()?;
        let doc = read_document(s.doc()?)?;
        let pt = doc.views.iter().find(|v| v.id() == ground).and_then(plan_transform).ok_or("ground unplaced")?.1;
        let placement = doc.hidden_views.iter().find(|h| h.view.id() == section).and_then(|h| h.view.placement().cloned());
        let Some(Placement::Vertical { cut_line, .. }) = placement else { return Err("no cut line".into()) };
        let st = doc.hidden_views.iter().find(|h| h.view.id() == same).and_then(|h| plan_transform(&h.view)).ok_or("Same unplaced")?.1;
        // The authority: lamb-core's transforms, point by point.
        let to_p = |from: &Transform2D, x: f64, y: f64| {
            let q = world_to_local2(&pt, local_to_world2(from, lamb_core::geometry::point::Point2 { x, y }));
            Vec2::new(q.x, q.y)
        };
        let end = cut_line.segments.first().ok_or("empty cut line")?.to;
        let cut = (to_p(&pt, cut_line.start.x, cut_line.start.y), to_p(&pt, end.x, end.y));
        let same_line = (to_p(&st, 0.0, 0.0), to_p(&st, 1000.0, 0.0));
        assert!(!near(same_line.0, Vec2::ZERO) && !near(same_line.1, Vec2::new(1000.0, 0.0)), "the transforms do not cancel");

        s.start("unhideview")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Select a hidden view or enter its name [?]:"));
        let ghosts = s.preview(Vec2::ZERO);
        assert!(ghosts.iter().all(|e| e.common.color == Color::Index(8) && e.common.layer == "0"), "ACI 8 on layer 0");
        let lines = ghost_lines(&s);
        assert_eq!(lines.len(), 2, "the cut line and Same's line; Other is on another level: {lines:?}");
        assert!(lines.iter().any(|l| near(l.0, cut.0) && near(l.1, cut.1)), "{lines:?} vs {cut:?}");
        assert!(lines.iter().any(|l| near(l.0, same_line.0) && near(l.1, same_line.1)), "{lines:?} vs {same_line:?}");
        let mut names = texts(&s);
        names.sort();
        assert_eq!(names, ["A-A", "Same"]);
        let label = ghosts
            .iter()
            .find_map(|e| match &e.kind {
                EntityKind::Text(t) if t.value == "A-A" => Some(t.clone()),
                _ => None,
            })
            .ok_or("no A-A label")?;
        assert!(near(label.insert.xy(), cut.0) && (label.height - 12.0 * s.pixel_size()).abs() < 1e-9, "{label:?}");

        // Model space ghosts nothing; the prompt still works.
        s.cancel();
        s.execute("editview", &json!({}))?;
        s.start("unhideview")?;
        assert!(s.running.is_some() && s.preview(Vec2::ZERO).is_empty(), "no ghosts in Model space");
        s.cancel();
        assert!(!unhiding());
        Ok(())
    }

    #[test]
    fn a_click_on_a_ghost_restores_it_and_a_miss_says_so() -> E<()> {
        let _g = crate::overlays::underlay::tests::lock();
        let (mut s, _ground, section, _same) = ghost_project()?;
        s.start("unhideview")?;
        let g = ghosts(&s).into_iter().find(|g| g.id == section).ok_or("no section ghost")?;
        let EntityKind::Line(l) = g.lines.first().ok_or("no line")? else { return Err("not a line".into()) };
        let (a, b) = (l.a.xy(), l.b.xy());
        let hidden = |s: &Session| -> E<bool> { Ok(read_document(s.doc()?)?.hidden_views.iter().any(|h| h.view.id() == section)) };
        let px = s.pixel_size();
        let mid = a.mid(b);
        let off = (b - a).normalized().perp();
        s.input(Input::Point(mid + off * (40.0 * px)))?;
        assert!(s.log.iter().any(|l| l == "No hidden view there"), "{:?}", s.log);
        assert!(s.running.is_some() && hidden(&s)?, "still hidden, re-prompts");
        s.input(Input::Point(mid + off * (5.0 * px)))?;
        assert!(s.running.is_none());
        assert!(!hidden(&s)? && read_document(s.doc()?)?.views.iter().any(|v| v.id() == section), "restored");
        assert!(!unhiding(), "the flag is cleared");
        s.undo()?;
        assert!(hidden(&s)?, "one undo step");
        Ok(())
    }
}

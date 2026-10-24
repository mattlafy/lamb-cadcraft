//! MAKEVIEW (`cadcraft-integration.md` §4, fork-only — the TS app has none): loose model-space
//! entities — a DXF's line work, or anything drawn before a LAMB view existed — move into a LAMB
//! plan view.
//!
//! - `makeview {handles: [hex], viewId?, name?}` (`mkv`, LAMB ▸ Project ▸ Make View) → `{viewId,
//!   moved}`. Every handle must be a model-space entity that is not a LAMB-generated insert (a view's
//!   canonical insert, a tag bubble); anything else refuses the whole call: `Only loose model-space
//!   entities can make a view`. `viewId` absent → a new floating plan named `name` or `View <n>`
//!   (first free n); present → that plan (placed or floating); a vertical or detail target refuses.
//! - A placed plan's block is a model-space overlay through its placement, so each entity is mapped
//!   global → plan-local (lamb-core's `world_to_local2`); a floating plan takes it unchanged. Each
//!   entity keeps its own `Common` (layer, colour, linetype); every kind moves, text and hatch
//!   included (hatch associativity to a moved boundary is not kept). One undo step; the
//!   after-command link maintenance then stamps roles and auto-links through hollow markers, as
//!   for any entity new to a view block.
//! - Interactive (Model space only): select objects, then `Enter target view name or [New] <New>:`
//!   (a plan's name, or New → `Enter view name <View n>:`). Stays in Model space afterwards.
//!
//! Every refusal comes before `doc_mut`, so a refusal records nothing.

use cadcraft_doc::{EntityKind, Handle, Space};
use cadcraft_engine::cmd::machines::{SelOutcome, SelectPhase};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::{Mat3, Vec2};
use lamb_core::model::{AnyView, Id, Placement, PlanView};
use serde_json::{Value, json};

use crate::commands::{bad, create, id_value, name_input, other};
use crate::schema::{read_document, view_block_name};

const CMD: &str = "makeview";
const LOOSE_ONLY: &str = "Only loose model-space entities can make a view";

/// `View <n>` for the first n no view is named.
fn free_name(s: &Session) -> String {
    let names: Vec<String> =
        s.doc().ok().and_then(|d| read_document(d).ok()).map(|d| d.views.iter().map(|v| v.name().to_owned()).collect()).unwrap_or_default();
    (1..).map(|n| format!("View {n}")).find(|n| !names.contains(n)).unwrap_or_else(|| "View".to_owned())
}

/// Plan-local from project-global: inverse translation, then inverse rotation (`world_to_local2`).
fn global_to_local(t: &lamb_core::geometry::transform2d::Transform2D) -> Mat3 {
    Mat3::rotate(-t.rotation).then_before(Mat3::translate(Vec2::new(-t.origin_x, -t.origin_y)))
}

fn handle_list(p: &Value) -> Result<Vec<Handle>> {
    let Some(Value::Array(a)) = p.get("handles") else { return Err(bad(CMD, "`handles` must name at least one entity")) };
    let hs = a
        .iter()
        .map(|v| v.as_str().and_then(Handle::parse_hex).or_else(|| v.as_u64().map(Handle)))
        .map(|h| h.ok_or_else(|| bad(CMD, "`handles` must be hex handle strings or numbers")))
        .collect::<Result<Vec<_>>>()?;
    if hs.is_empty() {
        return Err(bad(CMD, "`handles` must name at least one entity"));
    }
    Ok(hs)
}

/// MAKEVIEW's JSON form.
pub(crate) fn makeview(s: &mut Session, p: &Value) -> Result<Value> {
    let mut hs = handle_list(p)?;
    hs.dedup();
    let d = s.doc()?;
    for &h in &hs {
        let loose = d.space_of(h) == Some(Space::Model)
            && d.entity(h).is_some_and(|e| !matches!(&e.kind, EntityKind::Insert(i) if i.block.starts_with("LAMB_")));
        if !loose {
            return Err(other(LOOSE_ONLY));
        }
    }
    let doc = read_document(d).map_err(other)?;
    let target = match p.get("viewId").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => {
            let id = id_value(v).ok_or_else(|| other("No such view"))?;
            match doc.views.iter().find(|v| v.id() == id) {
                None => return Err(other("No such view")),
                Some(AnyView::Plan(plan)) => Some((id, plan.name.clone(), plan.placement.clone())),
                Some(_) => return Err(other("MAKEVIEW targets a plan view")),
            }
        }
    };
    let (id, name, m) = match target {
        Some((id, name, placement)) => {
            let m = match placement {
                Some(Placement::Plan { transform, .. }) => Some(global_to_local(&transform)),
                _ => None,
            };
            (id, name, m.filter(|m| *m != Mat3::IDENTITY))
        }
        None => {
            let name = p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).map_or_else(|| free_name(s), str::to_owned);
            let r = create(s, AnyView::Plan(PlanView { id: Id(0), name: name.clone(), entities: Vec::new(), placement: None }))?;
            (r.get("viewId").and_then(Value::as_u64).map(Id).ok_or_else(|| other("no view id"))?, name, None)
        }
    };
    let space = Space::Block(view_block_name(id));
    let d = s.doc_mut()?;
    for &h in &hs {
        let Some(e) = d.remove_entity(h) else { continue };
        let mut kind = e.kind.clone();
        if let Some(m) = &m {
            kind.transform(m);
        }
        d.add(&space, e.common.clone(), kind)?;
    }
    let moved = hs.len();
    s.set_selection(Vec::new());
    s.echo(format!("Moved {moved} entities into \"{name}\""));
    Ok(json!({ "viewId": id.0, "moved": moved }))
}

/// MAKEVIEW interactively: select, then the target view (or a new one and its name).
struct MakeViewM {
    sel: SelectPhase,
    picked: Option<Vec<Handle>>,
    naming: bool,
}

impl MakeViewM {
    fn run(&self, s: &mut Session, p: Value) {
        let hs: Vec<String> = self.picked.iter().flatten().map(|h| h.hex()).collect();
        let mut p = p;
        p["handles"] = json!(hs);
        if let Err(e) = makeview(s, &p) {
            s.echo(e.to_string());
        }
    }
}

impl Interactive for MakeViewM {
    fn name(&self) -> &'static str {
        "MAKEVIEW"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if s.state()?.space != Space::Model {
            s.echo("MAKEVIEW works on model-space entities; leave the view first (EDITVIEW with no view).");
            return Ok(Step::Done);
        }
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.picked = Some(self.sel.picked.clone());
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match (&self.picked, self.naming) {
            (None, _) => self.sel.prompt(),
            (Some(_), false) => Prompt::new("Enter target view name", Accept::TEXT).kw(&["New"]).default("New".to_owned()),
            (Some(_), true) => Prompt::new("Enter view name", Accept::TEXT).default(free_name(s)),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        if self.picked.is_none() {
            return Ok(match self.sel.feed(s, &i)? {
                SelOutcome::More => Step::Continue,
                SelOutcome::Empty => Step::Done,
                SelOutcome::Done(hs) => {
                    self.picked = Some(hs);
                    Step::Continue
                }
            });
        }
        if self.naming {
            let Some(name) = name_input(i, &free_name(s)) else { return Ok(Step::Continue) };
            self.run(s, json!({ "name": name }));
            return Ok(Step::Done);
        }
        let text = match i {
            Input::Enter => String::new(),
            Input::Text(t) | Input::Keyword(t) => t.trim().to_owned(),
            _ => return Ok(Step::Continue),
        };
        if text.is_empty() || text.eq_ignore_ascii_case("new") || text.eq_ignore_ascii_case("n") {
            self.naming = true;
            return Ok(Step::Continue);
        }
        let doc = read_document(s.doc()?).map_err(other)?;
        let plan = |exact: bool| {
            doc.views.iter().find(|v| matches!(v, AnyView::Plan(_)) && if exact { v.name() == text } else { v.name().eq_ignore_ascii_case(&text) })
        };
        match plan(true).or_else(|| plan(false)) {
            Some(v) => {
                self.run(s, json!({ "viewId": v.id().0 }));
                Ok(Step::Done)
            }
            None => {
                s.echo(format!("No plan view named \"{text}\""));
                Ok(Step::Continue)
            }
        }
    }
}

pub(crate) fn makeview_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(MakeViewM { sel: SelectPhase::default(), picked: None, naming: false }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::enter_space;
    use crate::project::DrawingProject;
    use lamb_core::derivation::markers::compute_markers_for_view;
    use lamb_core::derivation::read::ProjectRead;
    use lamb_core::geometry::Point2;
    use lamb_core::geometry::transform2d::local_to_world2;
    use std::collections::BTreeMap;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    fn session() -> T<Session> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        Ok(s)
    }

    fn line(s: &mut Session, a: [f64; 2], b: [f64; 2]) -> T<Handle> {
        s.execute("line", &json!({ "points": [a, b] }))?;
        Ok(s.doc()?.model.iter().last().ok_or("no line")?.handle)
    }

    fn lines_of(s: &Session, id: Id) -> T<Vec<(Vec2, Vec2)>> {
        let block = s.doc()?.block(&view_block_name(id)).ok_or("no block")?;
        Ok(block
            .entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Line(l) => Some((Vec2::new(l.a.x, l.a.y), Vec2::new(l.b.x, l.b.y))),
                _ => None,
            })
            .collect())
    }

    fn loose_lines(s: &Session) -> T<usize> {
        Ok(s.doc()?.model.iter().filter(|e| matches!(e.kind, EntityKind::Line(_))).count())
    }

    fn near(a: Vec2, x: f64, y: f64) -> bool {
        (a.x - x).abs() < 1e-6 && (a.y - y).abs() < 1e-6
    }

    #[test]
    fn three_model_lines_make_a_floating_plan_in_one_undo_step() -> R {
        let mut s = session()?;
        s.execute("layer.new", &json!({ "name": "WALLS", "current": true }))?;
        let hs = [line(&mut s, [0.0, 0.0], [1000.0, 0.0])?, line(&mut s, [0.0, 0.0], [0.0, 3000.0])?, line(&mut s, [5.0, 7.0], [11.0, 13.0])?];
        let undo = s.state()?.undo.len();
        let r = s.execute(CMD, &json!({ "handles": hs.iter().map(|h| h.hex()).collect::<Vec<_>>() }))?;
        assert_eq!(r["moved"], 3);
        let id = Id(r["viewId"].as_u64().ok_or("no id")?);
        let doc = read_document(s.doc()?)?;
        let Some(AnyView::Plan(plan)) = doc.views.iter().find(|v| v.id() == id) else { return Err("no plan".into()) };
        assert_eq!(plan.name, "View 1");
        assert!(plan.placement.is_none());
        let ls = lines_of(&s, id)?;
        assert_eq!(ls.len(), 3);
        assert!(ls.iter().any(|(a, b)| near(*a, 5.0, 7.0) && near(*b, 11.0, 13.0)), "{ls:?}");
        let block = s.doc()?.block(&view_block_name(id)).ok_or("no block")?;
        assert!(block.entities.iter().all(|e| e.common.layer == "WALLS"), "own layer kept");
        assert_eq!(loose_lines(&s)?, 0, "model space keeps none");
        assert_eq!(s.state()?.space, Space::Model);
        assert_eq!(s.state()?.undo.len(), undo + 1);
        s.undo()?;
        assert_eq!(loose_lines(&s)?, 3);
        assert!(read_document(s.doc()?)?.views.is_empty());
        Ok(())
    }

    #[test]
    fn into_a_placed_plan_maps_global_to_plan_local() -> R {
        let mut s = session()?;
        let plan = Id(s.execute("newplan", &json!({ "name": "P" }))?["viewId"].as_u64().ok_or("no id")?);
        let level = s.execute("level", &json!({ "mode": "create", "name": "L1", "floorElevationMm": 0 }))?["levelId"].as_u64().ok_or("no id")?;
        let t = json!({ "originX": 1000.0, "originY": 2000.0, "rotation": std::f64::consts::FRAC_PI_2 });
        s.execute("placeview", &json!({ "viewId": plan.0, "kind": "plan", "levelId": level, "cutElevation": 1200, "transform": t }))?;
        enter_space(&mut s, Space::Model)?;
        let h = line(&mut s, [1000.0, 2000.0], [1000.0, 3000.0])?;
        let r = s.execute(CMD, &json!({ "handles": [h.hex()], "viewId": plan.0 }))?;
        assert_eq!(r, json!({ "viewId": plan.0, "moved": 1 }));
        let ls = lines_of(&s, plan)?;
        assert_eq!(ls.len(), 1);
        // The authority: lamb-core's plan_global_to_local_xy for the same placement.
        let p = DrawingProject::new(s.doc()?)?;
        let view = p.view(plan).ok_or("no view")?;
        let b = lamb_core::derivation::projection::plan_global_to_local_xy(&view, Point2 { x: 1000.0, y: 3000.0 })?;
        assert!(near(ls[0].0, 0.0, 0.0) && near(ls[0].1, 1000.0, 0.0), "{ls:?}");
        assert!(near(ls[0].1, b.x, b.y));
        Ok(())
    }

    #[test]
    fn a_view_line_a_vertical_target_and_an_empty_list_refuse_and_record_nothing() -> R {
        let mut s = session()?;
        let plan = Id(s.execute("newplan", &json!({ "name": "P" }))?["viewId"].as_u64().ok_or("no id")?);
        enter_space(&mut s, Space::Block(view_block_name(plan)))?;
        let in_view = line(&mut s, [0.0, 0.0], [10.0, 0.0])?;
        let in_view = s.doc()?.block(&view_block_name(plan)).and_then(|b| b.entities.iter().last().map(|e| e.handle)).unwrap_or(in_view);
        let sec = s.execute(
            "newsectionline",
            &json!({ "name": "S", "hostViewId": plan.0, "cutFrom": { "x": 0, "y": -100 }, "cutTo": { "x": 0, "y": 100 } }),
        )?;
        let sec = sec["viewId"].as_u64().ok_or("no section")?;
        enter_space(&mut s, Space::Model)?;
        let loose = line(&mut s, [0.0, 0.0], [10.0, 0.0])?;
        let canonical = Handle(plan.0);
        let undo = s.state()?.undo.len();
        for (p, msg) in [
            (json!({ "handles": [in_view.hex()] }), LOOSE_ONLY),
            (json!({ "handles": [canonical.hex()] }), LOOSE_ONLY),
            (json!({ "handles": [loose.hex()], "viewId": sec }), "MAKEVIEW targets a plan view"),
            (json!({ "handles": [] }), "must name at least one entity"),
        ] {
            let e = s.execute(CMD, &p).err().ok_or("no refusal")?;
            assert!(e.to_string().contains(msg), "{e}");
        }
        assert_eq!(s.state()?.undo.len(), undo);
        assert_eq!(read_document(s.doc()?)?.views.len(), 2);
        assert!(s.doc()?.model.contains(loose));
        Ok(())
    }

    #[test]
    fn a_moved_line_through_a_hollow_marker_of_a_placed_plan_auto_links() -> R {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb/u-wall-two-sections.lamb.json");
        let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        let project = lamb_core::persistence::files_to_project(&files).map_err(|e| format!("{e:?}"))?;
        crate::commands::install_app()?;
        let (d, _, _) = crate::import::project_to_drawing(&project)?;
        let mut s = Session::new();
        s.open_drawing(d, "u-wall", None);
        crate::maintain::install(&mut s);
        // Erase the plan end of a link: its partner's marker in the plan goes hollow.
        let (plan, plan_end) = {
            let doc = read_document(s.doc()?)?;
            let p = DrawingProject::new(s.doc()?)?;
            let is_plan =
                |e: Id| p.entity_view_id(e).and_then(|v| doc.views.iter().find(|x| x.id() == v)).is_some_and(|v| matches!(v, AnyView::Plan(_)));
            let mut found = None;
            for l in &doc.links {
                for e in [l.a.entity_id, l.b.entity_id] {
                    if found.is_none() && is_plan(e) {
                        found = Some((p.entity_view_id(e).ok_or("no view")?, e));
                    }
                }
            }
            found.ok_or("no plan-side link end")?
        };
        let layer = DrawingProject::new(s.doc()?)?.entity(plan_end).ok_or("no entity")?.layer_id.to_owned();
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("erase", &json!({ "handles": [Handle(plan_end.0).hex()] }))?;
        let (marker, transform) = {
            let p = DrawingProject::new(s.doc()?)?;
            let m = compute_markers_for_view(&p, plan)?.into_iter().find(|m| !m.filled).ok_or("no hollow marker")?;
            let doc = read_document(s.doc()?)?;
            let Some(AnyView::Plan(PlanView { placement: Some(Placement::Plan { transform, .. }), .. })) = doc.views.iter().find(|v| v.id() == plan)
            else {
                return Err("plan not placed".into());
            };
            (m.position, *transform)
        };
        // Draw through the marker in Model space, in global coordinates, on the erased line's layer.
        s.execute("editview", &json!({}))?;
        s.execute("layer.current", &json!({ "name": layer }))?;
        let a = local_to_world2(&transform, Point2 { x: marker.x - 500.0, y: marker.y - 500.0 });
        let b = local_to_world2(&transform, Point2 { x: marker.x + 500.0, y: marker.y + 500.0 });
        line(&mut s, [a.x, a.y], [b.x, b.y])?;
        let h = s.doc()?.model.iter().last().ok_or("no line")?.handle;
        let before = read_document(s.doc()?)?.links.len();
        s.execute(CMD, &json!({ "handles": [h.hex()], "viewId": plan.0 }))?;
        let moved = s.doc()?.block(&view_block_name(plan)).and_then(|b| b.entities.iter().last().map(|e| Id(e.handle.0))).ok_or("nothing moved")?;
        let links = read_document(s.doc()?)?.links;
        assert_eq!(links.len(), before + 1, "one automatic link");
        assert!(links.iter().any(|l| l.a.entity_id == moved || l.b.entity_id == moved));
        Ok(())
    }

    #[test]
    fn interactive_select_new_and_a_name_makes_the_view_and_stays_in_model() -> R {
        let mut s = session()?;
        let h = line(&mut s, [0.0, 0.0], [10.0, 0.0])?;
        s.start("mkv")?;
        s.input(Input::Pick(vec![h]))?;
        s.input(Input::Enter)?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter target view name or [New] <New>:"));
        s.input(Input::Enter)?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter view name <View 1>:"));
        s.input(Input::Text("Site".into()))?;
        assert!(s.current_prompt().is_none());
        let doc = read_document(s.doc()?)?;
        let v = doc.views.iter().find(|v| v.name() == "Site").ok_or("no view named Site")?;
        assert_eq!(lines_of(&s, v.id())?.len(), 1);
        assert_eq!(s.state()?.space, Space::Model);
        assert!(s.log.iter().any(|l| l == "Moved 1 entities into \"Site\""), "{:?}", s.log);
        Ok(())
    }

    #[test]
    fn interactive_names_an_existing_plan_and_refuses_inside_a_view() -> R {
        let mut s = session()?;
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        enter_space(&mut s, Space::Block(view_block_name(plan)))?;
        s.start(CMD)?;
        assert!(s.current_prompt().is_none());
        assert!(s.log.iter().any(|l| l.starts_with("MAKEVIEW works on model-space entities")), "{:?}", s.log);
        enter_space(&mut s, Space::Model)?;
        let h = line(&mut s, [0.0, 0.0], [10.0, 0.0])?;
        s.start(CMD)?;
        s.input(Input::Pick(vec![h]))?;
        s.input(Input::Enter)?;
        s.input(Input::Text("nope".into()))?;
        assert!(s.current_prompt().is_some(), "an unknown name re-prompts");
        s.input(Input::Text("ground".into()))?;
        assert!(s.current_prompt().is_none());
        assert_eq!(lines_of(&s, plan)?.len(), 1);
        Ok(())
    }

    #[test]
    fn free_name_skips_taken_names() -> R {
        let mut s = session()?;
        s.execute("newplan", &json!({ "name": "View 1" }))?;
        assert_eq!(free_name(&s), "View 2");
        Ok(())
    }
}

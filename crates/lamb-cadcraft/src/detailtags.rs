//! Detail tags (LAMB's `docs/07-architecture/cadcraft-integration.md` §9 "Detail tags are the
//! assignment", phase 8, LAMB T349): a tag is an ordinary CADCraft `Leader` in a plan or section
//! view's block (LAMB T456: §8 "Not only plans" — plan details tag plan views, section details
//! vertical views, whose runs come from cut faces on `WALL*`, `FLOOR*`, `ROOF*` and `SLAB*`),
//! its tip `vertices[0]`, carrying [`EntityData::detail_tag`] — the detail view it names and whether
//! it tags a junction (corner) or a run (edge). DXF/DWG see a plain leader.
//!
//! - `detailtag {detailViewId, corner?, tip:{x,y}, bubble:{x,y}}` (`dtag`): one tag, on the current
//!   layer, in the edited plan or section view; one undo step; the detail's cut direction must match. Returns `{"handle": <hex>}`. Interactive:
//!   `Enter detail name or [Corner/?]:` → `Specify tag point:` → `Specify bubble point:`.
//! - `detailtags {viewId?}`: the resolution report for a plan or section view (default: the edited one) —
//!   `<12 runs: 3 tagged, 7 spread, 2 untyped; 4 junctions: 4 spread>`, echoed and returned.
//!
//! Reading tags back ([`view_detail_tags`]): an edge tag's tip attaches to the run whose centreline
//! is nearest, if within `width / 2 + TOLERANCE.grouping_mm`; a corner tag's tip to the nearest
//! junction within `max arm width / 2 + TOLERANCE.grouping_mm`. Anything else — a tip in open
//! space, a detail view that no longer exists or is drawn in the other direction — is `unattached`. [`resolve_view_details`] then runs
//! lamb-core's greedy resolution (edges and corners). Computed on demand, never stored.

use cadcraft_doc::{Drawing, EntityKind, Handle, Leader, Line};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::Vec2;
use lamb_core::derivation::detail_tags::{CornerTag, EdgeTag, RunDetail, resolve_corner_details, resolve_edge_details};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::derivation::wall_junctions::{WallGraph, wall_graph};
use lamb_core::derivation::wall_spaces::{SideParams, WallSides, wall_sides};
use lamb_core::derivation::walls::{RunCurve, WallRunParams};
use lamb_core::geometry::Point2;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AnyView, CutDirection, Id};
use serde_json::{Value, json};

use crate::commands::{bad, edited_view, id_value, other, point_param};
use crate::project::DrawingProject;
use crate::schema::{EntityData, read_document, read_entity_data, set_entity_data, view_block_name};

pub use crate::schema::DetailTag;

/// One plan view's tags, attached: edge tags to runs, corner tags to junctions of the view's
/// [`WallGraph`]; `detail` is the detail view id in decimal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewTags {
    pub edges: Vec<EdgeTag>,
    pub corners: Vec<CornerTag>,
    /// Tags whose tip attaches to nothing, or whose detail view no longer exists.
    pub unattached: Vec<Handle>,
}

fn starts_with_ci(name: &str, prefix: &str) -> bool {
    name.len() >= prefix.len() && name.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn dist(a: Point2, b: Point2) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

/// Distance from `p` to a run's centreline.
fn curve_distance(c: &RunCurve, p: Point2) -> f64 {
    match *c {
        RunCurve::Line { from, to } => {
            let (dx, dy) = (to.x - from.x, to.y - from.y);
            let len2 = dx * dx + dy * dy;
            let t = if len2 > 0.0 { (((p.x - from.x) * dx + (p.y - from.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            dist(p, Point2 { x: from.x + t * dx, y: from.y + t * dy })
        }
        RunCurve::Arc { center, radius, start_angle, sweep } => {
            let a = (p.y - center.y).atan2(p.x - center.x);
            let tau = std::f64::consts::TAU;
            let inside = if sweep >= 0.0 { (a - start_angle).rem_euclid(tau) <= sweep } else { (start_angle - a).rem_euclid(tau) <= -sweep };
            if inside {
                return (dist(p, center) - radius).abs();
            }
            let at = |ang: f64| Point2 { x: center.x + radius * ang.cos(), y: center.y + radius * ang.sin() };
            dist(p, at(start_angle)).min(dist(p, at(start_angle + sweep)))
        }
    }
}

/// The run an edge tag at `tip` attaches to.
fn attach_run(graph: &WallGraph, tip: Point2) -> Option<usize> {
    let (i, d) = graph.runs.iter().enumerate().map(|(i, r)| (i, curve_distance(&r.centreline, tip))).min_by(|a, b| a.1.total_cmp(&b.1))?;
    (d <= graph.runs[i].width / 2.0 + TOLERANCE.grouping_mm).then_some(i)
}

/// The junction a corner tag at `tip` attaches to: the nearest one whose reach covers it.
fn attach_junction(graph: &WallGraph, tip: Point2) -> Option<usize> {
    graph
        .junctions
        .iter()
        .enumerate()
        .filter_map(|(i, j)| {
            let w = j.arms.iter().filter_map(|a| graph.runs.get(a.run)).map(|r| r.width).fold(0.0, f64::max);
            let d = dist(j.point, tip);
            (d <= w / 2.0 + TOLERANCE.grouping_mm).then_some((i, d))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// The view's wall graph and sides, with `WALL-EXT*` layers exterior and the view's outside pick
/// points ([`crate::outside::outside_points`]) marking their regions outside.
pub(crate) fn graph_and_sides(p: &DrawingProject<'_>, view: Id) -> (WallGraph, WallSides) {
    let graph = wall_graph(p, view, &wall_params(p, view));
    let exterior_layer_ids = p.layers().iter().filter(|l| starts_with_ci(&l.name, "WALL-EXT")).map(|l| l.id.clone()).collect();
    let sides = wall_sides(p, &graph, &SideParams { outside_points: crate::outside::outside_points(p.drawing(), view), exterior_layer_ids });
    (graph, sides)
}

/// What counts as a wall face in `view`: [`WallRunParams::for_vertical`] in a vertical view, else
/// [`WallRunParams::defaults`].
fn wall_params(p: &DrawingProject<'_>, view: Id) -> WallRunParams {
    let vertical = read_document(p.drawing()).is_ok_and(|doc| doc.views.iter().any(|v| matches!(v, AnyView::Vertical(_)) && v.id() == view));
    if vertical { WallRunParams::for_vertical(p.layers()) } else { WallRunParams::defaults(p.layers()) }
}

/// The cut direction details tagging `view` must have: `Vertical` in a vertical view, else `Plan`.
fn view_direction(d: &Drawing, view: Id) -> CutDirection {
    let vertical = read_document(d).is_ok_and(|doc| doc.views.iter().any(|v| matches!(v, AnyView::Vertical(_)) && v.id() == view));
    if vertical { CutDirection::Vertical } else { CutDirection::Plan }
}

/// Every tagged leader in `view`'s block: (handle, tip, tag).
fn tags_in(d: &Drawing, view: Id) -> std::result::Result<Vec<(Handle, Point2, DetailTag)>, String> {
    let Some(block) = d.block(&view_block_name(view)) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for e in block.entities.iter() {
        let EntityKind::Leader(l) = &e.kind else { continue };
        let Some(tag) = read_entity_data(&e.common).map_err(|e| e.to_string())?.detail_tag else { continue };
        let Some(tip) = l.vertices.first() else { continue };
        out.push((e.handle, Point2 { x: tip.x, y: tip.y }, tag));
    }
    Ok(out)
}

fn graph_tags(d: &Drawing, view: Id, graph: &WallGraph) -> std::result::Result<ViewTags, String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    let want = view_direction(d, view);
    let is_detail = |id: Id| doc.views.iter().any(|v| matches!(v, AnyView::Detail(x) if x.id == id && x.cut_direction == want));
    let mut out = ViewTags::default();
    for (h, tip, tag) in tags_in(d, view)? {
        let detail = tag.detail_view_id.0.to_string();
        let attached = if !is_detail(tag.detail_view_id) {
            false
        } else if tag.corner {
            attach_junction(graph, tip).map(|junction| out.corners.push(CornerTag { detail, junction })).is_some()
        } else {
            attach_run(graph, tip).map(|run| out.edges.push(EdgeTag { detail, run })).is_some()
        };
        if !attached {
            out.unattached.push(h);
        }
    }
    Ok(out)
}

/// `Ok` for a plan view only — ASSEMBLYPLAN, WALLCONFLICTS, WALLRUN and WALLFIT ([`edited_plan`]).
fn plan_view(d: &Drawing, view: Id) -> std::result::Result<(), String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    match doc.views.iter().find(|v| v.id() == view) {
        Some(AnyView::Plan(_)) => Ok(()),
        Some(_) => Err("Detail tags live in plan views".into()),
        None => Err("No such view".into()),
    }
}

/// `Ok` for a plan or vertical view — the views with a wall graph.
fn wall_view(d: &Drawing, view: Id) -> std::result::Result<(), String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    match doc.views.iter().find(|v| v.id() == view) {
        Some(AnyView::Plan(_) | AnyView::Vertical(_)) => Ok(()),
        Some(_) => Err("Detail tags live in plan and section views".into()),
        None => Err("No such view".into()),
    }
}

/// `view`'s detail tags, attached to its wall graph's runs and junctions.
pub fn view_detail_tags(d: &Drawing, view: Id) -> std::result::Result<ViewTags, String> {
    wall_view(d, view)?;
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let graph = wall_graph(&p, view, &wall_params(&p, view));
    graph_tags(d, view, &graph)
}

/// Every run's edge detail and every junction's corner detail in `view` (parallel to its wall
/// graph's `runs` and `junctions`).
pub fn resolve_view_details(d: &Drawing, view: Id) -> std::result::Result<(Vec<RunDetail>, Vec<RunDetail>), String> {
    resolve_with_tags(d, view).map(|(runs, junctions, _)| (runs, junctions))
}

fn resolve_with_tags(d: &Drawing, view: Id) -> std::result::Result<(Vec<RunDetail>, Vec<RunDetail>, ViewTags), String> {
    wall_view(d, view)?;
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let (graph, sides) = graph_and_sides(&p, view);
    let tags = graph_tags(d, view, &graph)?;
    let runs = resolve_edge_details(&graph, &sides, &tags.edges);
    let junctions = resolve_corner_details(&graph, &sides, &tags.corners);
    Ok((runs, junctions, tags))
}

/// `3 tagged, 7 spread, 2 untyped` — the non-zero kinds, in a fixed order.
fn counts(xs: &[RunDetail]) -> String {
    let mut n = [0usize; 5];
    for x in xs {
        n[match x {
            RunDetail::Tagged(_) => 0,
            RunDetail::Spread(_) => 1,
            RunDetail::Gap(_) => 2,
            RunDetail::Conflict => 3,
            RunDetail::Untyped => 4,
        }] += 1;
    }
    let words = ["tagged", "spread", "gap", "conflict", "untyped"];
    n.iter().zip(words).filter(|(c, _)| **c > 0).map(|(c, w)| format!("{c} {w}")).collect::<Vec<_>>().join(", ")
}

fn kind_json(x: &RunDetail) -> Value {
    match x {
        RunDetail::Tagged(d) => json!({ "kind": "tagged", "detail": d }),
        RunDetail::Spread(d) => json!({ "kind": "spread", "detail": d }),
        RunDetail::Gap(d) => json!({ "kind": "gap", "detail": d }),
        RunDetail::Conflict => json!({ "kind": "conflict" }),
        RunDetail::Untyped => json!({ "kind": "untyped" }),
    }
}

/// The report line: `<12 runs: 3 tagged, …; 4 junctions: 4 spread; 1 unattached>`.
pub fn report(runs: &[RunDetail], junctions: &[RunDetail], unattached: usize) -> String {
    let part = |n: usize, word: &str, xs: &[RunDetail]| {
        let c = counts(xs);
        if c.is_empty() { format!("{n} {word}") } else { format!("{n} {word}: {c}") }
    };
    let mut s = format!("<{}; {}", part(runs.len(), "runs", runs), part(junctions.len(), "junctions", junctions));
    if unattached > 0 {
        s.push_str(&format!("; {unattached} unattached"));
    }
    s.push('>');
    s
}

/// DETAILTAGS: the resolution report for `viewId` (default: the edited view). Not an undo step.
pub(crate) fn detailtags(s: &mut Session, p: &Value) -> Result<Value> {
    let view = match p.get("viewId").filter(|v| !v.is_null()) {
        Some(v) => id_value(v).ok_or_else(|| other("No such view"))?,
        None => edited_view(s).ok_or_else(|| other("Edit a plan or section view first (EDITVIEW)"))?,
    };
    let (runs, junctions, tags) = resolve_with_tags(s.doc()?, view).map_err(other)?;
    let message = report(&runs, &junctions, tags.unattached.len());
    s.echo(message.clone());
    Ok(json!({
        "message": message,
        "runs": runs.iter().map(kind_json).collect::<Vec<_>>(),
        "junctions": junctions.iter().map(kind_json).collect::<Vec<_>>(),
        "unattached": tags.unattached.iter().map(|h| format!("{:X}", h.0)).collect::<Vec<_>>(),
    }))
}

/// The edited view, if it is a plan view; else the refusal.
pub(crate) fn edited_plan(s: &Session) -> std::result::Result<Id, String> {
    let view = edited_view(s).ok_or("Edit a plan view first (EDITVIEW)")?;
    plan_view(s.doc().map_err(|e| e.to_string())?, view)?;
    Ok(view)
}

/// The edited view, if it is a plan or vertical view; else the refusal.
pub(crate) fn edited_wall_view(s: &Session) -> std::result::Result<Id, String> {
    let view = edited_view(s).ok_or("Edit a plan or section view first (EDITVIEW)")?;
    wall_view(s.doc().map_err(|e| e.to_string())?, view)?;
    Ok(view)
}

/// `id`'s cut direction, if it is a detail view.
fn detail_direction(s: &Session, id: Id) -> Option<CutDirection> {
    let doc = read_document(s.doc().ok()?).ok()?;
    doc.views.iter().find_map(|v| match v {
        AnyView::Detail(x) if x.id == id => Some(x.cut_direction),
        _ => None,
    })
}

/// DETAILTAG's JSON form: a two-vertex leader from `tip` to `bubble` in the edited plan or section view.
pub(crate) fn detailtag(s: &mut Session, p: &Value) -> Result<Value> {
    let detail = p.get("detailViewId").and_then(id_value).ok_or_else(|| bad("detailtag", "`detailViewId` is required"))?;
    let corner = p.get("corner").and_then(Value::as_bool).unwrap_or(false);
    let tip = point_param("detailtag", p, "tip")?;
    let bubble = point_param("detailtag", p, "bubble")?;
    // Every refusal before `doc_mut`, so a refusal records nothing.
    let view = edited_wall_view(s).map_err(other)?;
    let Some(dir) = detail_direction(s, detail) else { return Err(other("No such detail view")) };
    match (dir, view_direction(s.doc()?, view)) {
        (CutDirection::Vertical, CutDirection::Plan) => return Err(other("That detail is drawn in section; tag it in a section view")),
        (CutDirection::Plan, CutDirection::Vertical) => return Err(other("That detail is drawn in plan; tag it in a plan view")),
        _ => {}
    }
    let mut common = s.current_common();
    set_entity_data(&mut common, &EntityData { detail_tag: Some(DetailTag { detail_view_id: detail, corner }), ..EntityData::default() })
        .map_err(other)?;
    let style = s.doc()?.header.str("DIMSTYLE", "Standard");
    let space = s.space();
    let leader =
        Leader { vertices: vec![Vec2::new(tip.x, tip.y).to3(0.0), Vec2::new(bubble.x, bubble.y).to3(0.0)], arrow: true, spline: false, style };
    let h = s.doc_mut()?.add(&space, common, EntityKind::Leader(leader))?;
    Ok(json!({ "handle": format!("{:X}", h.0) }))
}

/// The detail the prompt offers by default: the most recently placed tag's detail (highest handle,
/// any view) if that view still exists, else the drawing's only detail view.
fn default_detail(s: &Session) -> Option<Id> {
    let d = s.doc().ok()?;
    let doc = read_document(d).ok()?;
    let mut last: Option<(Handle, Id)> = None;
    for v in &doc.views {
        for (h, _, tag) in tags_in(d, v.id()).ok()? {
            if last.is_none_or(|(lh, _)| h.0 > lh.0) {
                last = Some((h, tag.detail_view_id));
            }
        }
    }
    let details: Vec<&AnyView> = doc.views.iter().filter(|v| matches!(v, AnyView::Detail(_))).collect();
    last.map(|(_, id)| id).filter(|id| details.iter().any(|v| v.id() == *id)).or_else(|| (details.len() == 1).then(|| details[0].id()))
}

/// DETAILTAG: detail (name or id, `Corner` toggles corner mode, `?` lists), tag point, bubble point.
struct DetailTagM {
    default: Option<Id>,
    detail: Option<Id>,
    corner: bool,
    tip: Option<Vec2>,
}

impl DetailTagM {
    fn default_name(&self, s: &Session) -> Option<String> {
        let id = self.default?;
        let doc = read_document(s.doc().ok()?).ok()?;
        doc.views.iter().find(|v| v.id() == id).map(|v| v.name().to_owned())
    }
}

impl Interactive for DetailTagM {
    fn name(&self) -> &'static str {
        "detailtag"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match (self.detail, self.tip) {
            (None, _) => {
                let msg = if self.corner { "Enter corner detail name" } else { "Enter detail name" };
                let p = Prompt::new(msg, Accept::TEXT).kw(&["Corner", "?"]);
                match self.default_name(s) {
                    Some(n) => p.default(n),
                    None => p,
                }
            }
            (Some(_), None) => Prompt::new("Specify tag point", Accept::POINT),
            (Some(_), Some(tip)) => Prompt::new("Specify bubble point", Accept::POINT).base(tip),
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if let Err(e) = edited_wall_view(s) {
            s.echo(e);
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(detail) = self.detail else {
            let text = match i {
                Input::Enter => match self.default {
                    Some(id) => {
                        self.detail = Some(id);
                        return Ok(Step::Continue);
                    }
                    None => return Ok(Step::Cancel),
                },
                Input::Text(t) | Input::Keyword(t) => t.trim().to_owned(),
                _ => return Ok(Step::Continue),
            };
            if text.eq_ignore_ascii_case("corner") || text.eq_ignore_ascii_case("c") {
                self.corner = !self.corner;
                return Ok(Step::Continue);
            }
            let doc = read_document(s.doc()?).map_err(other)?;
            let details = || doc.views.iter().filter(|v| matches!(v, AnyView::Detail(_)));
            if text == "?" {
                let lines: Vec<String> = details().map(|v| format!("{}  {}", v.id().0, v.name())).collect();
                if lines.is_empty() {
                    s.echo("No detail views (NEWDETAIL).");
                }
                for l in lines {
                    s.echo(l);
                }
                return Ok(Step::Continue);
            }
            let by_name = details().find(|v| v.name().eq_ignore_ascii_case(&text));
            let by_id = || text.parse::<u64>().ok().and_then(|n| details().find(|v| v.id() == Id(n)));
            match by_name.or_else(by_id).map(AnyView::id) {
                Some(id) => self.detail = Some(id),
                None => return Err(other("No such detail view")),
            }
            return Ok(Step::Continue);
        };
        let Input::Point(p) = i else {
            return Ok(if i == Input::Enter { Step::Cancel } else { Step::Continue });
        };
        let Some(tip) = self.tip else {
            self.tip = Some(p);
            return Ok(Step::Continue);
        };
        let params = json!({
            "detailViewId": detail.0,
            "corner": self.corner,
            "tip": {"x": tip.x, "y": tip.y},
            "bubble": {"x": p.x, "y": p.y},
        });
        if let Err(e) = detailtag(s, &params) {
            s.echo(e.to_string());
        }
        Ok(Step::Done)
    }
    fn preview(&self, _s: &Session, cursor: Vec2) -> Vec<EntityKind> {
        match (self.detail, self.tip) {
            (Some(_), Some(a)) => vec![EntityKind::Line(Line { a: a.to3(0.0), b: cursor.to3(0.0) })],
            _ => Vec::new(),
        }
    }
}

pub(crate) fn detailtag_machine(s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(DetailTagM { default: default_detail(s), detail: None, corner: false, tip: None }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_doc::Space;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    fn session() -> T<Session> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        Ok(s)
    }

    /// A plan with a closed 5000 × 4000 room of 200 mm walls on `WALL-EXT` (outer face 0..5000 ×
    /// 0..4000), a detail view `D1`, editing the plan on layer `TAGS`. Returns (session, plan, D1).
    fn room() -> T<(Session, Id, Id)> {
        let mut s = session()?;
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let detail = Id(s.execute("newdetail", &json!({ "name": "D1" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [5000, 0], [5000, 4000], [0, 4000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [4800, 200], [4800, 3800], [200, 3800]], "closed": true }))?;
        s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
        Ok((s, plan, detail))
    }

    fn tag(s: &mut Session, detail: Id, corner: bool, tip: (f64, f64)) -> Result<Value> {
        s.execute(
            "detailtag",
            &json!({ "detailViewId": detail.0, "corner": corner, "tip": {"x": tip.0, "y": tip.1}, "bubble": {"x": tip.0 - 1000.0, "y": tip.1 - 1000.0} }),
        )
    }

    fn kinds(xs: &[RunDetail]) -> (usize, usize, usize) {
        let n = |f: fn(&RunDetail) -> bool| xs.iter().filter(|x| f(x)).count();
        (n(|x| matches!(x, RunDetail::Tagged(_))), n(|x| matches!(x, RunDetail::Spread(_))), n(|x| matches!(x, RunDetail::Untyped)))
    }

    fn leaders(s: &Session, plan: Id) -> T<Vec<cadcraft_doc::Entity>> {
        let b = s.doc()?.block(&view_block_name(plan)).ok_or("no block")?.clone();
        Ok(b.entities.iter().filter(|e| matches!(e.kind, EntityKind::Leader(_))).map(|e| (**e).clone()).collect())
    }

    #[test]
    fn newdetail_makes_a_floating_detail_view_enters_it_and_lists_it() -> R {
        let mut s = session()?;
        s.start("newdetail")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter detail name or [Plan/Section] <Detail 1>:"));
        s.input(Input::Text("Eaves".into()))?;
        let doc = read_document(s.doc()?)?;
        let AnyView::Detail(d) = &doc.views[0] else { return Err("not a detail".into()) };
        assert_eq!((d.name.as_str(), d.placement.is_none()), ("Eaves", true));
        assert_eq!(s.state()?.space, Space::Block(view_block_name(d.id)));
        let rows = crate::views::browser_rows(s.doc()?, &s.state()?.space);
        assert!(rows.iter().any(|r| r.label == "Eaves — Detail (floating)" && r.active), "{rows:?}");
        Ok(())
    }

    #[test]
    fn an_edge_tag_spreads_to_the_other_three_runs_and_a_corner_tag_to_three_junctions() -> R {
        let (mut s, plan, detail) = room()?;
        let (runs, junctions) = resolve_view_details(s.doc()?, plan)?;
        assert_eq!((runs.len(), junctions.len()), (4, 4));
        assert_eq!(kinds(&runs), (0, 0, 4));

        // On the outer face of the bottom wall.
        tag(&mut s, detail, false, (2500.0, 0.0))?;
        let (runs, junctions) = resolve_view_details(s.doc()?, plan)?;
        assert_eq!(kinds(&runs), (1, 3, 0));
        assert!(runs.iter().all(|r| matches!(r, RunDetail::Tagged(d) | RunDetail::Spread(d) if *d == detail.0.to_string())));
        assert_eq!(kinds(&junctions), (0, 0, 4));

        // At the outer corner: 141 mm from the junction, inside its 150 mm reach.
        tag(&mut s, detail, true, (0.0, 0.0))?;
        let (_, junctions) = resolve_view_details(s.doc()?, plan)?;
        assert_eq!(kinds(&junctions), (1, 3, 0));

        let r = s.execute("detailtags", &json!({}))?;
        assert_eq!(r["message"], "<4 runs: 1 tagged, 3 spread; 4 junctions: 1 tagged, 3 spread>");
        assert!(s.log.iter().any(|l| l == "<4 runs: 1 tagged, 3 spread; 4 junctions: 1 tagged, 3 spread>"), "{:?}", s.log);
        Ok(())
    }

    #[test]
    fn a_tag_in_open_space_and_one_naming_a_deleted_detail_are_unattached() -> R {
        let (mut s, plan, detail) = room()?;
        let open = tag(&mut s, detail, false, (2500.0, 2000.0))?["handle"].as_str().ok_or("no handle")?.to_owned();
        let far_corner = tag(&mut s, detail, true, (2500.0, -500.0))?["handle"].as_str().ok_or("no handle")?.to_owned();
        let t = view_detail_tags(s.doc()?, plan)?;
        assert!(t.edges.is_empty() && t.corners.is_empty());
        assert_eq!(t.unattached.iter().map(|h| format!("{:X}", h.0)).collect::<Vec<_>>(), [open, far_corner]);

        tag(&mut s, detail, false, (2500.0, 100.0))?;
        assert_eq!(view_detail_tags(s.doc()?, plan)?.edges.len(), 1);
        let d = s.doc_mut()?;
        let mut doc = read_document(d)?;
        doc.views.retain(|v| v.id() != detail);
        crate::schema::write_document(d, &doc)?;
        let t = view_detail_tags(s.doc()?, plan)?;
        assert!(t.edges.is_empty());
        assert_eq!(t.unattached.len(), 3);
        let r = s.execute("detailtags", &json!({ "viewId": plan.0 }))?;
        assert_eq!(r["message"], "<4 runs: 4 untyped; 4 junctions: 4 untyped; 3 unattached>");
        Ok(())
    }

    #[test]
    fn tags_survive_undo_redo_copy_and_a_dxf_round_trip() -> R {
        let (mut s, plan, detail) = room()?;
        tag(&mut s, detail, false, (2500.0, 0.0))?;
        assert_eq!(view_detail_tags(s.doc()?, plan)?.edges.len(), 1);
        s.undo()?;
        assert!(leaders(&s, plan)?.is_empty(), "one undo step removes the tag");
        s.redo()?;
        assert_eq!(view_detail_tags(s.doc()?, plan)?.edges.len(), 1);

        let h = leaders(&s, plan)?[0].handle;
        s.execute("copy", &json!({ "handles": [format!("{:X}", h.0)], "delta": [0, 4000] }))?;
        let t = view_detail_tags(s.doc()?, plan)?;
        assert_eq!(t.edges.len(), 2, "the copy is a second tag");
        assert_ne!(t.edges[0].run, t.edges[1].run);

        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert_eq!(view_detail_tags(&back, plan)?, view_detail_tags(s.doc()?, plan)?);
        assert_eq!(resolve_view_details(&back, plan)?, resolve_view_details(s.doc()?, plan)?);
        Ok(())
    }

    #[test]
    fn detailtag_outside_a_plan_view_refuses_and_adds_nothing() -> R {
        let (mut s, _plan, detail) = room()?;
        s.execute("editview", &json!({}))?;
        let n = s.doc()?.model.len();
        assert!(tag(&mut s, detail, false, (2500.0, 0.0)).is_err_and(|e| e.to_string().contains("Edit a plan or section view first")));
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        assert!(tag(&mut s, detail, false, (2500.0, 0.0)).is_err_and(|e| e.to_string().contains("Detail tags live in plan and section views")));
        assert_eq!(s.doc()?.model.len(), n);
        assert!(s.doc()?.block(&view_block_name(detail)).ok_or("no block")?.entities.is_empty());

        s.execute("editview", &json!({}))?;
        s.start("dtag")?;
        assert!(s.running.is_none(), "refused at once");
        assert!(s.log.iter().any(|l| l == "Edit a plan or section view first (EDITVIEW)"), "{:?}", s.log);
        Ok(())
    }

    #[test]
    fn the_interactive_form_places_the_same_tag_as_the_parameter_form() -> R {
        let (mut s, plan, detail) = room()?;
        s.start("dtag")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter detail name or [Corner/?] <D1>:"));
        s.input(Input::Keyword("?".into()))?;
        assert!(s.log.iter().any(|l| l == &format!("{}  D1", detail.0)), "{:?}", s.log);
        s.input(Input::Keyword("Corner".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Enter corner detail name"));
        s.input(Input::Text("d1".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Specify tag point"));
        s.input(Input::Point(Vec2::new(0.0, 0.0)))?;
        assert_eq!(s.current_prompt().and_then(|p| p.base), Some(Vec2::new(0.0, 0.0)));
        s.input(Input::Point(Vec2::new(-1000.0, -1000.0)))?;
        assert!(s.running.is_none());
        tag(&mut s, detail, true, (0.0, 0.0))?;
        let l = leaders(&s, plan)?;
        assert_eq!(l.len(), 2);
        assert_eq!((&l[0].kind, &l[0].common.layer, &l[0].common.ext), (&l[1].kind, &l[1].common.layer, &l[1].common.ext));
        assert_eq!(view_detail_tags(s.doc()?, plan)?.corners.len(), 2);

        // The default is now the last detail used; one undo removes the interactive tag alone.
        s.start("detailtag")?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("D1"));
        s.input(Input::Cancel)?;
        s.undo()?;
        s.undo()?;
        assert!(leaders(&s, plan)?.is_empty());
        Ok(())
    }

    /// A plan with section `A-A` across it, edited, holding two vertical `WALL-EXT` faces 300 apart
    /// (u 0 and 300, z 0..3000) and two horizontal `FLOOR` faces 250 apart (z 1000 and 1250, u
    /// 300..4000) meeting the wall, all SETROLE cut; layer `TAGS` current. Returns (session, plan,
    /// section, plan detail, section detail).
    fn section_room() -> T<(Session, Id, Id, Id, Id)> {
        let mut s = session()?;
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let pd = Id(s.execute("newdetail", &json!({ "name": "PD" }))?["viewId"].as_u64().ok_or("no id")?);
        let sd = Id(s.execute("newdetail", &json!({ "name": "SD", "cutDirection": "vertical" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let r =
            s.execute("newsectionline", &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 0}, "cutTo": {"x": 5000, "y": 0} }))?;
        let sec = Id(r["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [0, 3000]] }))?;
        s.execute("pline", &json!({ "vertices": [[300, 0], [300, 3000]] }))?;
        s.execute("layer.new", &json!({ "name": "FLOOR", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[300, 1000], [4000, 1000]] }))?;
        s.execute("pline", &json!({ "vertices": [[300, 1250], [4000, 1250]] }))?;
        let block = s.doc()?.block(&view_block_name(sec)).ok_or("no block")?;
        let hs: Vec<String> = block.entities.iter().map(|e| format!("{:X}", e.handle.0)).collect();
        assert_eq!(hs.len(), 4);
        s.execute("setrole", &json!({ "handles": hs, "role": "cut" }))?;
        s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
        Ok((s, plan, sec, pd, sd))
    }

    #[test]
    fn a_section_s_cut_walls_and_floors_form_runs_and_a_section_detail_tags_them() -> R {
        let (mut s, _plan, sec, pd, sd) = section_room()?;
        let r = s.execute("detailtags", &json!({ "viewId": sec.0 }))?;
        assert_eq!(r["runs"].as_array().map(Vec::len), Some(2), "{r}");
        assert_eq!(kinds(&resolve_view_details(s.doc()?, sec)?.0), (0, 0, 2));

        // A plan detail is refused in the section and records nothing.
        let n = s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.len();
        assert!(tag(&mut s, pd, false, (150.0, 2000.0)).is_err_and(|e| e.to_string() == "That detail is drawn in plan; tag it in a plan view"));
        assert_eq!(s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.len(), n);

        // The section detail's edge tag on the wall run: that run is tagged.
        tag(&mut s, sd, false, (150.0, 2000.0))?;
        let t = view_detail_tags(s.doc()?, sec)?;
        assert_eq!(t.edges.len(), 1);
        let (runs, _) = resolve_view_details(s.doc()?, sec)?;
        assert_eq!(runs[t.edges[0].run], RunDetail::Tagged(sd.0.to_string()));
        let r = s.execute("detailtags", &json!({}))?;
        assert_eq!(r["message"], FLOOR_REPORT, "{r}");

        // OUTSIDEPOINT is accepted in the edited section.
        s.execute("outsidepoint", &json!({ "point": {"x": 2000, "y": 2500} }))?;
        assert_eq!(crate::outside::outside_points(s.doc()?, sec).len(), 1);
        Ok(())
    }

    /// lamb-core's greedy rule, unchanged: the floor abutting the wall's inner face forms no junction
    /// with it, so the wall tag does not spread to it.
    const FLOOR_REPORT: &str = "<2 runs: 1 tagged, 1 untyped; 0 junctions>";

    #[test]
    fn a_section_detail_is_refused_in_a_plan() -> R {
        let (mut s, plan, _sec, _pd, sd) = section_room()?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        assert!(tag(&mut s, sd, false, (0.0, 0.0)).is_err_and(|e| e.to_string() == "That detail is drawn in section; tag it in a section view"));
        Ok(())
    }

    #[test]
    fn newdetail_section_keyword_makes_a_vertical_detail() -> R {
        let mut s = session()?;
        s.start("newdetail")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter detail name or [Plan/Section] <Detail 1>:"));
        s.input(Input::Keyword("S".into()))?;
        assert!(s.log.iter().any(|l| l == "<Section detail>"), "{:?}", s.log);
        assert!(s.running.is_some(), "asks the name again");
        s.input(Input::Text("Sill".into()))?;
        let doc = read_document(s.doc()?)?;
        let AnyView::Detail(d) = &doc.views[0] else { return Err("not a detail".into()) };
        assert_eq!((d.name.as_str(), d.cut_direction), ("Sill", CutDirection::Vertical));
        assert_eq!(s.state()?.space, Space::Block(view_block_name(d.id)));
        assert!(
            s.execute("newdetail", &json!({ "name": "X", "cutDirection": "up" }))
                .is_err_and(|e| e.to_string().contains("`cutDirection` must be plan or vertical"))
        );
        s.start("newplan")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter plan name <Plan 1>:"));
        Ok(())
    }
}

//! SHOWLINKS and QUERY (TS `commands/showlinks.ts`, `commands/query.ts`): read-only lookups. Both
//! are `.noundo()` and only ever read through [`DrawingProject`]; neither calls `doc_mut`.
//!
//! - `showlinks {handle?}` (`sl`, LAMB ▸ Links ▸ Show Links): every entity an edit to `handle`
//!   would propagate to (lamb-core `linked_entity_component`: automatic links only), the links
//!   among them, and the boundary links (one end inside) that were not followed. No `handle` → the
//!   one selected entity (`handles`, as `SelectRun` passes it, is accepted too). Echoes
//!   `<N entities in M views; K links; B boundary links>`. Interactively (LAMB T543) it haloes
//!   the component in every view block and steps view to view on Next/Previous, like RESOLVE.
//! - `query {viewId?, layerName?, role?, kind?, closed?, near?: {entityIds?, layerName?, minMm?,
//!   maxMm?}, include?: "geometry", limit?}`: parameter-only, as in TS (it is the assistant's
//!   lookup). No filter → the index of views, layers and per-(view, layer) counts; otherwise
//!   `{result: "filter", rows, total}`. `layerId` is an alias of `layerName` (a layer's id is its
//!   name here); names match case-insensitively; an unknown view or layer is an error.
//!
//! Ids: entity ids out are hex handle strings, in as hex strings or numbers; view and link ids are
//! the numbers every other fork command takes (`viewId`, `unlink {linkId}`); layer ids are names.

use std::collections::{BTreeSet, HashMap};

use cadcraft_doc::{Drawing, EntityKind, Handle, Space, entity_bounds};
use cadcraft_engine::cmd::machines::{SelOutcome, SelectPhase};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step, View};
use cadcraft_geom::Bounds2;
use lamb_core::derivation::linking::linked_entity_component;
use lamb_core::derivation::read::{EntityRef, ProjectRead, ViewKind};
use lamb_core::geometry::Polyline2;
use lamb_core::geometry::distance::polyline_distance;
use lamb_core::geometry::polyline::{is_arc, polyline_bbox, polyline_length};
use lamb_core::model::{DrawingLayer, Id, LineRole, LinkAnchor, LinkOrigin, LinkRecord, resolve_entity_role};
use serde_json::{Map, Value, json};

use crate::commands::{bad, enter_space, id_value, other};
use crate::overlays::halo::{set_attention_blocks, set_hover_blocks, set_hovered};
use crate::project::DrawingProject;
use crate::schema::view_block_name;

const DEFAULT_LIMIT: usize = 200;

fn hex(id: Id) -> String {
    format!("{:X}", id.0)
}

/// An entity id parameter: a hex handle string or a number.
fn entity_id(v: &Value) -> Option<Id> {
    v.as_str().and_then(Handle::parse_hex).map(|h| Id(h.0)).or_else(|| v.as_u64().map(Id))
}

fn view_name<'a>(dp: &'a DrawingProject<'_>, entity: Id) -> &'a str {
    dp.entity_view_id(entity).and_then(|v| dp.view(v)).map_or("(unknown view)", |v| v.name)
}

// ------------------------------------------------------------------------------- SHOWLINKS --

fn anchor_json(dp: &DrawingProject<'_>, a: &LinkAnchor) -> Value {
    json!({ "entityId": hex(a.entity_id), "segmentIndex": a.segment_index, "viewName": view_name(dp, a.entity_id) })
}

fn link_json(dp: &DrawingProject<'_>, l: &LinkRecord) -> Value {
    json!({
        "linkId": l.id.0,
        "origin": if l.origin == LinkOrigin::Automatic { "automatic" } else { "explicit" },
        "stale": l.stale == Some(true),
        "a": anchor_json(dp, &l.a),
        "b": anchor_json(dp, &l.b),
    })
}

/// SHOWLINKS: TS `applyShowLinks`.
pub(crate) fn showlinks(s: &mut Session, p: &Value) -> Result<Value> {
    let id = match (p.get("handle"), p.get("handles")) {
        (Some(v), _) if !v.is_null() => entity_id(v).ok_or_else(|| bad("showlinks", "`handle` must be a hex handle string or a number"))?,
        (_, Some(Value::Array(a))) => match a.as_slice() {
            [v] => entity_id(v).ok_or_else(|| bad("showlinks", "`handles` must be hex handle strings or numbers"))?,
            _ => return Err(other("Select one entity")),
        },
        _ => match s.selection().as_slice() {
            [h] => Id(h.0),
            _ => return Err(other("Select one entity")),
        },
    };
    let dp = DrawingProject::new(s.doc()?).map_err(other)?;
    if dp.entity(id).is_none() {
        return Err(other(format!("No such entity: {}", hex(id))));
    }
    let component = linked_entity_component(&dp, id);
    let (mut links, mut boundary): (Vec<&LinkRecord>, Vec<&LinkRecord>) = (Vec::new(), Vec::new());
    for l in dp.links() {
        match (component.contains(&l.a.entity_id), component.contains(&l.b.entity_id)) {
            (true, true) => links.push(l),
            (true, false) | (false, true) => boundary.push(l),
            _ => {}
        }
    }
    links.sort_by_key(|l| l.id);
    boundary.sort_by_key(|l| l.id);
    let mut members: Vec<(Option<Id>, Id)> = component.iter().map(|e| (dp.entity_view_id(*e), *e)).collect();
    members.sort();
    let views: BTreeSet<Option<Id>> = members.iter().map(|m| m.0).collect();
    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    let message = format!(
        "<{} in {}; {}; {}>",
        plural(members.len(), "entity").replace("entitys", "entities"),
        plural(views.len(), "view"),
        plural(links.len(), "link"),
        plural(boundary.len(), "boundary link"),
    );
    Ok(json!({
        "entityId": hex(id),
        "component": members.iter().map(|(v, e)| json!({
            "entityId": hex(*e),
            "viewId": v.map(|v| v.0),
            "viewName": view_name(&dp, *e),
        })).collect::<Vec<_>>(),
        "links": links.iter().map(|l| link_json(&dp, l)).collect::<Vec<_>>(),
        "boundary": boundary.iter().map(|l| link_json(&dp, l)).collect::<Vec<_>>(),
        "message": message,
    }))
}

/// A linked component's members grouped by view, in the drawing's view order; members in no
/// view are left out. SHOWLINKS' halo and the hover halo both group this way.
fn component_blocks(dp: &DrawingProject<'_>, comp: &BTreeSet<Id>) -> Vec<(Id, Vec<Handle>)> {
    let mut out: Vec<(Id, Vec<Handle>)> = Vec::new();
    for e in comp {
        let Some(v) = dp.entity_view_id(*e) else { continue };
        match out.iter_mut().find(|(id, _)| *id == v) {
            Some((_, hs)) => hs.push(Handle(e.0)),
            None => out.push((v, vec![Handle(e.0)])),
        }
    }
    let order: Vec<Id> = dp.views().map(|v| v.id).collect();
    out.sort_by_key(|(id, _)| order.iter().position(|o| o == id).unwrap_or(usize::MAX));
    out
}

/// The canvas hover observer (LAMB T550, registered by the app with
/// `canvas::hoverext::register_hover_observer`): halo every other member of `hovered`'s linked
/// component in its own view block, paler than SHOWLINKS. Nothing hovered, an entity in no LAMB
/// view, or one with no links → no hover halo. The hovered entity itself, when in a LAMB view,
/// is kept for the 3D pane's highlight (LAMB T552).
pub fn hover_links(d: &Drawing, hovered: Option<Handle>) {
    let (own, blocks) = hovered.map_or((None, Vec::new()), |h| hover_blocks(d, h));
    set_hovered(own);
    set_hover_blocks(blocks);
}

/// The hovered entity when it is in a LAMB view, and the rest of its component by block.
fn hover_blocks(d: &Drawing, h: Handle) -> (Option<Handle>, Vec<(String, Vec<Handle>)>) {
    let Ok(dp) = DrawingProject::new(d) else { return (None, Vec::new()) };
    let id = Id(h.0);
    if dp.entity_view_id(id).is_none() {
        return (None, Vec::new());
    }
    let mut comp = linked_entity_component(&dp, id);
    comp.remove(&id);
    (Some(h), component_blocks(&dp, &comp).into_iter().map(|(v, hs)| (view_block_name(v), hs)).collect())
}

/// One view of a SHOWLINKS component: its id, name and the members drawn in it.
struct LinkedView {
    id: Id,
    name: String,
    members: Vec<Handle>,
}

/// Interactive SHOWLINKS: pick an entity, halo its linked component in every view block, then
/// `<i>/<M> <view> [Next/Previous]:` steps view to view (clamped, as RESOLVE), centring the camera
/// on each view's members. Starts at the picked entity's own view without moving the camera.
/// Enter or Esc ends it; the halo goes when the machine is dropped.
struct ShowLinks {
    sel: SelectPhase,
    views: Vec<LinkedView>,
    at: usize,
}

impl ShowLinks {
    fn line(&self) -> String {
        let name = self.views.get(self.at).map_or("", |v| v.name.as_str());
        format!("{}/{} {name}", self.at + 1, self.views.len())
    }

    /// Run SHOWLINKS on `hs`, halo the component and stay up to step it; `Done` with no links.
    fn picked(&mut self, s: &mut Session, hs: &[Handle]) -> Result<Step> {
        let r = showlinks(s, &json!({ "handles": hs.iter().map(|h| h.hex()).collect::<Vec<_>>() }))?;
        if let Some(m) = r["message"].as_str() {
            s.echo(m.to_string());
        }
        let comp = r["component"].as_array().cloned().unwrap_or_default();
        if comp.len() <= 1 {
            s.echo("No links");
            return Ok(Step::Done);
        }
        let views: Vec<LinkedView> = {
            let dp = DrawingProject::new(s.doc()?).map_err(other)?;
            let members: BTreeSet<Id> = comp.iter().filter_map(|c| entity_id(&c["entityId"])).collect();
            component_blocks(&dp, &members)
                .into_iter()
                .map(|(id, members)| LinkedView { id, name: dp.view(id).map_or(String::new(), |v| v.name.to_owned()), members })
                .collect()
        };
        let own = r["entityId"].as_str().and_then(Handle::parse_hex);
        self.at = views.iter().position(|lv| own.is_some_and(|h| lv.members.contains(&h))).unwrap_or(0);
        set_attention_blocks(views.iter().map(|lv| (view_block_name(lv.id), lv.members.clone())).collect());
        self.views = views;
        // Objects picked at the prompt don't stay selected afterwards.
        s.set_selection(Vec::new());
        Ok(Step::Continue)
    }

    /// Edit view [`Self::at`]'s block, centred on its members' bounds.
    fn show(&self, s: &mut Session) -> Result<()> {
        let Some(lv) = self.views.get(self.at) else { return Ok(()) };
        enter_space(s, Space::Block(view_block_name(lv.id)))?;
        let d = s.doc()?;
        let mut b = Bounds2::EMPTY;
        for h in &lv.members {
            if let Some(e) = d.entity(*h) {
                b = b.union(&entity_bounds(d, e, 0));
            }
        }
        let st = s.state_mut()?;
        let v = st.view();
        if !b.is_empty() {
            st.set_view(View { center: b.center(), height: v.height, twist: v.twist });
        }
        Ok(())
    }

    fn step(&mut self, s: &mut Session, forward: bool) -> Result<Step> {
        let to = if forward { (self.at + 1).min(self.views.len().saturating_sub(1)) } else { self.at.saturating_sub(1) };
        if to != self.at {
            self.at = to;
            self.show(s)?;
        }
        Ok(Step::Continue)
    }
}

impl Drop for ShowLinks {
    fn drop(&mut self) {
        set_attention_blocks(Vec::new());
    }
}

impl Interactive for ShowLinks {
    fn name(&self) -> &'static str {
        "SHOWLINKS"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if !self.views.is_empty() {
            Prompt::new(self.line(), Accept::TEXT).kw(&["Next", "Previous"])
        } else if self.sel.removing {
            self.sel.prompt()
        } else {
            Prompt::new("Select an entity to show its links", Accept::SELECT)
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            let hs = self.sel.picked.clone();
            return self.picked(s, &hs);
        }
        self.sel.single = true;
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if self.views.is_empty() {
            return match self.sel.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                SelOutcome::Empty => Ok(Step::Done),
                SelOutcome::Done(hs) => self.picked(s, &hs),
            };
        }
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

pub(crate) fn showlinks_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(ShowLinks { sel: SelectPhase::default(), views: Vec::new(), at: 0 }))
}

// ----------------------------------------------------------------------------------- QUERY --

fn view_kind(k: ViewKind) -> &'static str {
    match k {
        ViewKind::Plan => "plan",
        ViewKind::Vertical => "vertical",
        ViewKind::Detail => "detail",
    }
}

/// QUERY's `kind`: `polyline` for anything with a `Polyline2` form, else from the block entity.
fn entity_kind(dp: &DrawingProject<'_>, e: &EntityRef<'_>) -> &'static str {
    if e.polyline.is_some() {
        return "polyline";
    }
    let Some(ent) = dp.drawing().entity(Handle(e.id.0)) else { return "unknown" };
    match &ent.kind {
        EntityKind::Text(_) | EntityKind::MText(_) => "text",
        EntityKind::Hatch(_) => "hatch",
        EntityKind::Dimension(_) => "dimension",
        EntityKind::Line(_) => "line",
        EntityKind::Point(_) => "point",
        EntityKind::Circle(_) => "circle",
        EntityKind::Arc(_) => "arc",
        EntityKind::Ellipse(_) => "ellipse",
        EntityKind::LwPolyline(_) => "lwpolyline",
        EntityKind::Polyline3d(_) => "polyline3d",
        EntityKind::Spline(_) => "spline",
        EntityKind::Helix(_) => "helix",
        EntityKind::Ray(_) => "ray",
        EntityKind::XLine(_) => "xline",
        EntityKind::AttDef(_) => "attdef",
        EntityKind::Insert(_) => "insert",
        EntityKind::MLine(_) => "mline",
        EntityKind::Leader(_) => "leader",
        EntityKind::MLeader(_) => "mleader",
        EntityKind::Solid(_) => "solid",
        EntityKind::Trace(_) => "trace",
        EntityKind::Face3d(_) => "face3d",
        EntityKind::Region(_) => "region",
        EntityKind::Viewport(_) => "viewport",
        EntityKind::Image(_) => "image",
        EntityKind::Wipeout(_) => "wipeout",
        EntityKind::Table(_) => "table",
        EntityKind::Tolerance(_) => "tolerance",
        EntityKind::Unknown(_) => "unknown",
    }
}

fn find_layer<'a>(layers: &'a [DrawingLayer], name: &str) -> Option<&'a DrawingLayer> {
    layers.iter().find(|l| l.name.eq_ignore_ascii_case(name))
}

/// `layerName`, or its alias `layerId`, resolved to a layer id; an unknown name is an error.
fn layer_param(cmd: &str, layers: &[DrawingLayer], p: &Value) -> Result<Option<String>> {
    let Some(v) = p.get("layerName").filter(|v| !v.is_null()).or_else(|| p.get("layerId").filter(|v| !v.is_null())) else { return Ok(None) };
    let name = v.as_str().ok_or_else(|| bad(cmd, "`layerName` must be a string"))?;
    Ok(Some(find_layer(layers, name).ok_or_else(|| other(format!("No such layer: {name}")))?.id.clone()))
}

pub(crate) fn index(dp: &DrawingProject<'_>) -> Value {
    let views: Vec<Value> = dp.views().map(|v| json!({ "id": v.id.0, "name": v.name, "kind": view_kind(v.kind) })).collect();
    let layers: Vec<Value> =
        dp.layers().iter().map(|l| json!({ "id": l.id, "name": l.name, "defaultRole": l.default_role, "locked": l.locked })).collect();
    let mut order: Vec<(Id, String)> = Vec::new();
    let mut tally: HashMap<(Id, String), usize> = HashMap::new();
    for v in dp.views() {
        for e in dp.entities(v.id) {
            let key = (v.id, e.layer_id.to_owned());
            let n = tally.entry(key.clone()).or_insert(0);
            if *n == 0 {
                order.push(key);
            }
            *n += 1;
        }
    }
    let counts: Vec<Value> = order.iter().map(|k| json!({ "viewId": k.0.0, "layerId": k.1, "n": tally.get(k).copied().unwrap_or(0) })).collect();
    json!({ "result": "index", "views": views, "layers": layers, "counts": counts })
}

fn row(dp: &DrawingProject<'_>, e: &EntityRef<'_>, geometry: bool) -> Map<String, Value> {
    let role = resolve_entity_role(dp.layers(), e.role, e.layer_id);
    let mut r = Map::new();
    r.insert("id".into(), json!(hex(e.id)));
    r.insert("viewId".into(), json!(e.view_id.0));
    r.insert("layerId".into(), json!(e.layer_id));
    r.insert("role".into(), json!(role));
    r.insert("kind".into(), json!(entity_kind(dp, e)));
    if let Some(poly) = e.polyline.as_deref() {
        r.insert("closed".into(), json!(poly.closed));
        r.insert("vertexCount".into(), json!(poly.segments.len() + usize::from(!poly.closed)));
        r.insert("lengthMm".into(), json!(polyline_length(poly)));
        r.insert("bbox".into(), json!(polyline_bbox(poly)));
        if let [seg] = poly.segments.as_slice()
            && !is_arc(seg.bulge)
        {
            r.insert("from".into(), json!(poly.start));
            r.insert("to".into(), json!(seg.to));
        }
        if geometry {
            r.insert("geometry".into(), json!(poly));
        }
    }
    r
}

/// QUERY: TS `applyQuery`.
pub(crate) fn query(s: &mut Session, p: &Value) -> Result<Value> {
    let dp = DrawingProject::new(s.doc()?).map_err(other)?;
    let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
    if !["viewId", "layerId", "layerName", "role", "kind", "closed", "near"].iter().any(|k| given(k)) {
        return Ok(index(&dp));
    }
    let view = match p.get("viewId").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => {
            let id = id_value(v).ok_or_else(|| bad("query", "`viewId` must be a view id"))?;
            if dp.view(id).is_none() {
                return Err(other(format!("No such view: {v}")));
            }
            Some(id)
        }
    };
    let layer = layer_param("query", dp.layers(), p)?;
    let role: Option<LineRole> = match p.get("role").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(serde_json::from_value(v.clone()).map_err(|_| bad("query", "`role` must be a line role"))?),
    };
    let kind = match p.get("kind").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(v.as_str().ok_or_else(|| bad("query", "`kind` must be a string"))?),
    };
    let closed = match p.get("closed").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(v.as_bool().ok_or_else(|| bad("query", "`closed` must be a boolean"))?),
    };
    let geometry = p.get("include").and_then(Value::as_str) == Some("geometry");
    let limit = p.get("limit").and_then(Value::as_u64).map_or(DEFAULT_LIMIT, |n| n as usize);

    // `near`: its own selector resolved to the line set every candidate is measured against.
    struct Near {
        set: Vec<(Id, Polyline2)>,
        min: f64,
        max: f64,
    }
    let near = match p.get("near").filter(|v| !v.is_null()) {
        None => None,
        Some(n) => {
            let ids: BTreeSet<Id> = match n.get("entityIds") {
                Some(Value::Array(a)) => {
                    a.iter().map(|v| entity_id(v).ok_or_else(|| bad("query", "`near.entityIds` must be entity ids"))).collect::<Result<_>>()?
                }
                _ => BTreeSet::new(),
            };
            let layer = layer_param("query", dp.layers(), n)?;
            let mut set = Vec::new();
            for v in dp.views() {
                for e in dp.entities(v.id) {
                    let Some(poly) = e.polyline.as_deref() else { continue };
                    if ids.contains(&e.id) || layer.as_deref() == Some(e.layer_id) {
                        set.push((e.id, poly.clone()));
                    }
                }
            }
            let num = |k: &str| n.get(k).and_then(Value::as_f64);
            Some(Near { set, min: num("minMm").unwrap_or(f64::NEG_INFINITY), max: num("maxMm").unwrap_or(f64::INFINITY) })
        }
    };

    let mut rows = Vec::new();
    for v in dp.views() {
        if view.is_some_and(|id| id != v.id) {
            continue;
        }
        for e in dp.entities(v.id) {
            if layer.as_deref().is_some_and(|l| l != e.layer_id) {
                continue;
            }
            if role.is_some_and(|r| resolve_entity_role(dp.layers(), e.role, e.layer_id) != r) {
                continue;
            }
            if kind.is_some_and(|k| k != entity_kind(&dp, &e)) {
                continue;
            }
            if closed.is_some_and(|c| e.polyline.as_deref().is_none_or(|p| p.closed != c)) {
                continue;
            }
            let mut r = row(&dp, &e, geometry);
            if let Some(n) = &near {
                let Some(poly) = e.polyline.as_deref() else { continue };
                let nearest = n
                    .set
                    .iter()
                    .filter(|(id, _)| *id != e.id)
                    .map(|(id, q)| (polyline_distance(poly, q), *id))
                    .fold(None::<(f64, Id)>, |best, x| if best.is_none_or(|b| x.0 < b.0) { Some(x) } else { best });
                let Some((d, id)) = nearest else { continue };
                if d < n.min || d > n.max {
                    continue;
                }
                r.insert("nearestMm".into(), json!(d));
                r.insert("nearestId".into(), json!(hex(id)));
            }
            rows.push(Value::Object(r));
        }
    }
    let total = rows.len();
    rows.truncate(limit);
    Ok(json!({ "result": "filter", "rows": rows, "total": total }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::install_app;
    use crate::import::project_to_drawing;
    use crate::overlays::halo::{current_attention_blocks, current_hover_blocks};
    use crate::schema::{read_document, write_document};
    use lamb_core::model::Project;
    use lamb_core::persistence::files_to_project;
    use std::collections::BTreeMap;
    use std::path::Path;

    type E<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    fn load(name: &str) -> E<Project> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb").join(format!("{name}.lamb.json"));
        let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        Ok(files_to_project(&files).map_err(|e| format!("{e:?}"))?)
    }

    /// The imported u-wall-two-sections in a session; the handle of `wall-u` (the plan line) and
    /// of `face-1` (its crossbar section line). `explicit` turns the wall-u ↔ face-1 link explicit.
    fn session(explicit: bool) -> E<(Session, Id, Id)> {
        install_app().map_err(|e| e.to_string())?;
        let p = with_ids(load("u-wall-two-sections")?)?;
        let (mut d, map, _) = project_to_drawing(&p.0)?;
        let (wall, face) = (map.get(p.1).ok_or("wall")?, map.get(p.2).ok_or("face")?);
        if explicit {
            let mut doc = read_document(&d)?;
            let l = doc.links.iter_mut().find(|l| [l.a.entity_id, l.b.entity_id] == [face, wall]).ok_or("no face link")?;
            l.origin = LinkOrigin::Explicit;
            write_document(&mut d, &doc)?;
        }
        let mut s = Session::new();
        s.open_drawing(d, "u-wall", None);
        Ok((s, wall, face))
    }

    /// The project with the ids of `wall-u` (plan `P`'s only line) and `face-1` (`Crossbar`'s).
    fn with_ids(p: Project) -> E<(Project, Id, Id)> {
        let first = |name: &str| -> E<Id> {
            Ok(p.views.iter().find(|v| v.name() == name).and_then(|v| v.entities().first()).ok_or(format!("no line in {name}"))?.id)
        };
        let (w, f) = (first("P")?, first("Crossbar")?);
        Ok((p, w, f))
    }

    fn ids(v: &Value) -> Vec<String> {
        v.as_array().into_iter().flatten().filter_map(|x| x["entityId"].as_str().map(str::to_owned)).collect()
    }

    #[test]
    fn showlinks_spans_plan_and_both_sections_and_stops_at_an_explicit_link() -> E<()> {
        let (mut s, wall, face) = session(false)?;
        let undo = s.state()?.undo.len();
        let r = s.execute("showlinks", &json!({ "handle": hex(wall) }))?;
        let comp = ids(&r["component"]);
        assert_eq!(comp.len(), 4, "{r}");
        assert!(comp.contains(&hex(face)));
        let views: BTreeSet<&str> = r["component"].as_array().ok_or("component")?.iter().filter_map(|c| c["viewName"].as_str()).collect();
        assert_eq!(views.len(), 3, "plan and both sections: {views:?}");
        assert_eq!(r["links"].as_array().map(Vec::len), Some(3));
        assert!(r["links"].as_array().ok_or("links")?.iter().all(|l| l["origin"] == "automatic"));
        assert_eq!(r["boundary"], json!([]));
        assert_eq!(r["message"], "<4 entities in 3 views; 3 links; 0 boundary links>");
        assert_eq!(s.state()?.undo.len(), undo, "read-only");

        let (mut s, wall, face) = session(true)?;
        let r = s.execute("showlinks", &json!({ "handle": hex(wall) }))?;
        assert!(!ids(&r["component"]).contains(&hex(face)), "the far end of an explicit link leaves the component");
        assert_eq!(r["links"].as_array().map(Vec::len), Some(2));
        let b = r["boundary"].as_array().ok_or("boundary")?;
        assert_eq!(b.len(), 1);
        assert_eq!(b[0]["origin"], "explicit");
        Ok(())
    }

    #[test]
    fn showlinks_unknown_handle_errors_and_interactive_picks_one() -> E<()> {
        let (mut s, wall, _) = session(false)?;
        let undo = s.state()?.undo.len();
        let e = s.execute("showlinks", &json!({ "handle": "FFFFFF" })).err().ok_or("expected an error")?;
        assert!(e.to_string().contains("No such entity: FFFFFF"), "{e}");
        assert_eq!(s.state()?.undo.len(), undo);

        let _g = crate::overlays::underlay::tests::lock();
        s.start("sl")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Select an entity to show its links:"));
        s.input(Input::Pick(vec![Handle(wall.0)]))?;
        assert!(s.running.is_some(), "stays up to step the views");
        assert!(s.log.iter().any(|l| l == "<4 entities in 3 views; 3 links; 0 boundary links>"), "{:?}", s.log);
        s.input(Input::Cancel)?;
        assert!(s.running.is_none());
        assert_eq!(s.state()?.undo.len(), undo);
        Ok(())
    }

    /// The component's members grouped by view block, as the halo holds them.
    fn halo_union(blocks: &[(String, Vec<Handle>)]) -> BTreeSet<String> {
        blocks.iter().flat_map(|(_, hs)| hs.iter().map(|h| h.hex())).collect()
    }

    fn members_centre(s: &Session, block: &str) -> E<cadcraft_geom::Vec2> {
        let blocks = current_attention_blocks();
        let (_, hs) = blocks.iter().find(|(b, _)| b == block).ok_or("block not haloed")?;
        let d = s.doc()?;
        let mut b = Bounds2::EMPTY;
        for h in hs {
            b = b.union(&entity_bounds(d, d.entity(*h).ok_or("no entity")?, 0));
        }
        Ok(b.center())
    }

    #[test]
    fn interactive_showlinks_haloes_every_view_and_steps_through_them() -> E<()> {
        let _g = crate::overlays::underlay::tests::lock();
        set_attention_blocks(Vec::new());
        let (mut s, wall, _) = session(false)?;
        let undo = s.state()?.undo.len();
        let (space0, view0) = (s.state()?.space.clone(), s.state()?.view());
        s.start("sl")?;
        s.input(Input::Pick(vec![Handle(wall.0)]))?;
        let blocks = current_attention_blocks();
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        let r = showlinks(&mut s, &json!({ "handle": hex(wall) }))?;
        assert_eq!(halo_union(&blocks), ids(&r["component"]).into_iter().collect::<BTreeSet<_>>());
        assert_eq!(s.state()?.space, space0, "the edited space is unchanged");
        assert_eq!(s.state()?.view().center, view0.center, "the camera did not move");
        let own = blocks.iter().position(|(_, hs)| hs.contains(&Handle(wall.0))).ok_or("wall not haloed")?;
        let prompt = s.current_prompt().map(|p| p.display()).unwrap_or_default();
        assert!(prompt.starts_with(&format!("{}/3 ", own + 1)) && prompt.contains("[Next/Previous]"), "{prompt}");

        // Step to the last view, past the end, and back.
        let order: Vec<String> = blocks.iter().map(|(b, _)| b.clone()).collect();
        for b in order.iter().skip(own + 1) {
            s.input(Input::Keyword("Next".into()))?;
            assert_eq!(s.state()?.space, Space::Block(b.clone()));
            assert!(s.state()?.view().center.dist(members_centre(&s, b)?) < 1e-6, "centred on {b}'s members");
        }
        s.input(Input::Text(">".into()))?;
        assert_eq!(s.state()?.space, Space::Block(order.last().ok_or("no views")?.clone()), "clamped at the end");
        s.input(Input::Keyword("Previous".into()))?;
        let back = order.len().saturating_sub(2);
        assert_eq!(s.state()?.space, Space::Block(order[back].clone()));
        assert!(s.state()?.view().center.dist(members_centre(&s, &order[back])?) < 1e-6);
        assert_eq!(current_attention_blocks().len(), 3, "the halo stays while stepping");

        s.input(Input::Cancel)?;
        assert!(current_attention_blocks().is_empty(), "Esc clears every halo");
        assert!(s.running.is_none());
        assert_eq!(s.state()?.undo.len(), undo, "not an undo step");
        Ok(())
    }

    #[test]
    fn interactive_showlinks_on_an_unlinked_entity_says_no_links_and_ends() -> E<()> {
        let _g = crate::overlays::underlay::tests::lock();
        set_attention_blocks(Vec::new());
        let (mut s, wall, _) = session(false)?;
        let r = showlinks(&mut s, &json!({ "handle": hex(wall) }))?;
        let view = r["component"].as_array().and_then(|c| c.iter().find(|c| c["entityId"] == json!(hex(wall)))).ok_or("wall")?["viewId"].clone();
        s.execute("editview", &json!({ "viewId": view }))?;
        let block = view_block_name(Id(view.as_u64().ok_or("view id")?));
        let before: Vec<Handle> = s.doc()?.block(&block).ok_or("no block")?.entities.iter().map(|e| e.handle).collect();
        s.execute("line", &json!({ "points": [[90_000, 90_000], [91_000, 90_000]] }))?;
        let lone = s.doc()?.block(&block).ok_or("no block")?.entities.iter().map(|e| e.handle).find(|h| !before.contains(h)).ok_or("no line")?;
        s.start("sl")?;
        s.input(Input::Pick(vec![lone]))?;
        assert!(s.running.is_none(), "ended");
        assert_eq!(s.log.last().map(String::as_str), Some("No links"));
        assert!(current_attention_blocks().is_empty(), "no halo");
        Ok(())
    }

    #[test]
    fn hover_links_haloes_the_rest_of_the_component_by_block_and_clears() -> E<()> {
        let _g = crate::overlays::underlay::tests::lock();
        let (mut s, wall, _) = session(false)?;
        let r = showlinks(&mut s, &json!({ "handle": hex(wall) }))?;
        let mut want: BTreeSet<String> = ids(&r["component"]).into_iter().collect();
        want.remove(&hex(wall));
        hover_links(s.doc()?, Some(Handle(wall.0)));
        let blocks = current_hover_blocks();
        assert_eq!(halo_union(&blocks), want, "every member but the wall");
        let views: BTreeSet<String> = r["component"]
            .as_array()
            .ok_or("component")?
            .iter()
            .filter(|c| c["entityId"] != json!(hex(wall)))
            .filter_map(|c| c["viewId"].as_u64().map(|v| view_block_name(Id(v))))
            .collect();
        assert_eq!(blocks.iter().map(|(b, _)| b.clone()).collect::<BTreeSet<_>>(), views, "{blocks:?}");
        assert!(views.len() >= 2, "both sections: {views:?}");
        let dp = DrawingProject::new(s.doc()?)?;
        for (b, hs) in &blocks {
            assert!(hs.iter().all(|h| dp.entity_view_id(Id(h.0)).is_some_and(|v| view_block_name(v) == *b)), "{b}: {hs:?}");
        }
        drop(dp);

        hover_links(s.doc()?, None);
        assert!(current_hover_blocks().is_empty(), "nothing hovered");
        hover_links(s.doc()?, Some(Handle(wall.0)));
        hover_links(s.doc()?, Some(Handle(0xFF_FFFF)));
        assert!(current_hover_blocks().is_empty(), "an entity in no view");

        let view = r["component"].as_array().and_then(|c| c.iter().find(|c| c["entityId"] == json!(hex(wall)))).ok_or("wall")?["viewId"].clone();
        s.execute("editview", &json!({ "viewId": view }))?;
        let block = view_block_name(Id(view.as_u64().ok_or("view id")?));
        let before: Vec<Handle> = s.doc()?.block(&block).ok_or("no block")?.entities.iter().map(|e| e.handle).collect();
        s.execute("line", &json!({ "points": [[90_000, 90_000], [91_000, 90_000]] }))?;
        let lone = s.doc()?.block(&block).ok_or("no block")?.entities.iter().map(|e| e.handle).find(|h| !before.contains(h)).ok_or("no line")?;
        hover_links(s.doc()?, Some(Handle(wall.0)));
        hover_links(s.doc()?, Some(lone));
        assert!(current_hover_blocks().is_empty(), "an unlinked entity");
        Ok(())
    }

    #[test]
    fn highlight_3d_holds_the_hovered_entity_its_component_and_the_attention_set() -> E<()> {
        let _g = crate::overlays::underlay::tests::lock();
        set_attention_blocks(Vec::new());
        let (mut s, wall, _) = session(false)?;
        let r = showlinks(&mut s, &json!({ "handle": hex(wall) }))?;
        let comp: BTreeSet<String> = ids(&r["component"]).into_iter().collect();
        let lit = || crate::overlays::halo::highlight_3d().iter().map(|h| h.hex()).collect::<Vec<_>>();
        hover_links(s.doc()?, Some(Handle(wall.0)));
        let got = lit();
        assert_eq!(got.iter().cloned().collect::<BTreeSet<_>>(), comp, "the wall and every member");
        assert_eq!(got.len(), comp.len(), "deduplicated");
        hover_links(s.doc()?, None);
        assert!(lit().is_empty(), "nothing hovered");

        let view = r["component"].as_array().and_then(|c| c.iter().find(|c| c["entityId"] == json!(hex(wall)))).ok_or("wall")?["viewId"].clone();
        s.execute("editview", &json!({ "viewId": view }))?;
        let block = view_block_name(Id(view.as_u64().ok_or("view id")?));
        let before: Vec<Handle> = s.doc()?.block(&block).ok_or("no block")?.entities.iter().map(|e| e.handle).collect();
        s.execute("line", &json!({ "points": [[90_000, 90_000], [91_000, 90_000]] }))?;
        let lone = s.doc()?.block(&block).ok_or("no block")?.entities.iter().map(|e| e.handle).find(|h| !before.contains(h)).ok_or("no line")?;
        hover_links(s.doc()?, Some(lone));
        assert_eq!(lit(), vec![lone.hex()], "an unlinked LAMB-view entity: just itself");
        hover_links(s.doc()?, Some(Handle(0xFF_FFFF)));
        assert!(lit().is_empty(), "an entity in no view");

        set_attention_blocks(vec![(block.clone(), vec![lone])]);
        assert_eq!(lit(), vec![lone.hex()], "the attention set alone");
        set_attention_blocks(Vec::new());
        assert!(lit().is_empty());
        Ok(())
    }

    #[test]
    fn query_index_filters_near_and_unknowns() -> E<()> {
        let (mut s, _, _) = session(false)?;
        let idx = s.execute("query", &json!({}))?;
        assert_eq!(idx["result"], "index");
        let dp = DrawingProject::new(s.doc()?)?;
        let total: usize = dp.views().map(|v| dp.entities(v.id).count()).sum();
        let nviews = dp.views().count();
        let nlayers = dp.layers().len();
        drop(dp);
        assert_eq!(idx["views"].as_array().map(Vec::len), Some(nviews));
        assert_eq!(idx["layers"].as_array().map(Vec::len), Some(nlayers));
        let counted: u64 = idx["counts"].as_array().ok_or("counts")?.iter().filter_map(|c| c["n"].as_u64()).sum();
        assert_eq!(counted as usize, total);
        assert!(total >= 4);

        let layer = idx["counts"][0]["layerId"].as_str().ok_or("layer")?.to_owned();
        let r = s.execute("query", &json!({ "layerName": layer.to_lowercase(), "closed": false }))?;
        assert_eq!(r["result"], "filter");
        let rows = r["rows"].as_array().ok_or("rows")?;
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|x| x["layerId"] == json!(layer) && x["closed"] == json!(false) && x["kind"] == "polyline"));
        assert_eq!(r["total"].as_u64(), Some(rows.len() as u64));

        let r = s.execute("query", &json!({ "near": { "layerName": layer, "maxMm": 1.0e9 } }))?;
        let rows = r["rows"].as_array().ok_or("rows")?;
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|x| x["nearestMm"].as_f64().is_some_and(|d| d <= 1.0e9) && x["nearestId"].is_string()));
        let r = s.execute("query", &json!({ "near": { "layerName": layer, "maxMm": -1.0 } }))?;
        assert_eq!(r["total"], 0);

        let on_layer: u64 =
            idx["counts"].as_array().ok_or("counts")?.iter().filter(|c| c["layerId"] == json!(layer)).filter_map(|c| c["n"].as_u64()).sum();
        let r = s.execute("query", &json!({ "layerName": layer, "limit": 1 }))?;
        assert_eq!((r["rows"].as_array().map(Vec::len), r["total"].as_u64()), (Some(1), Some(on_layer)));

        assert!(s.execute("query", &json!({ "viewId": 987_654_321u64 })).is_err_and(|e| e.to_string().contains("No such view")));
        assert!(s.execute("query", &json!({ "layerName": "NOPE" })).is_err_and(|e| e.to_string().contains("No such layer: NOPE")));
        Ok(())
    }
}

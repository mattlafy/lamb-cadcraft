//! LAMB's view commands as engine `CommandSpec`s, registered once per process with
//! `cadcraft_engine::register_command_provider(lamb_cadcraft::commands)` (FORK.md patch 4).
//!
//! - `newplan {name}` (`npl`): a floating plan view.
//! - `newsectionline` / `newelevationline {name, hostViewId, cutFrom:{x,y}, cutTo:{x,y}}` (`nsl`,
//!   `nel`): a vertical view placed on a straight cut line in the host view.
//! - `editview {viewId?}`: edit inside the view's block; no `viewId` returns to Model.
//! - `newsheet {name, viewIds?, assemblyIds?}` (`nsh`): a paper layout with one viewport per view
//!   (default every view, when neither list is given), then one per plan in `assemblyIds` showing
//!   its assembly plan; each freezes every other view's layer and every other assembly layer;
//!   returns `{layout, viewports: [hex handles]}`.
//!
//! - `link {a:{entityId, segmentIndex, segmentParam}, b:{…}}` (`lk`): an explicit link between two
//!   segments in different views (the escape hatch from the same-layer auto-link rule); `entityId`
//!   is a hex handle string or a JSON number. Returns `{"linkId": <id>}`.
//! - `unlink {linkId}` (`ulk`): removes that link.
//!   Both are interactive too (View ▸ LAMB ▸ Link / Unlink): pick a line then its match in another
//!   pane, or click a filled marker — see [`crate::linkcmds`]. Both run through `link.apply` /
//!   `unlink.apply`, which hold the undo step.
//! - `panelock {on?}`: camera lock for VPORTS panes (absent `on` toggles; off by default) — panning
//!   or zooming the current tile re-centres every related tile at the same scale. Returns
//!   `{on, message}` (`<Pane lock on>` / `<Pane lock off>`); not an undo step.
//! - `ucsalign {on?}`: ORTHO and direct distance entry follow the current pane's rotation (on, the
//!   default) or keep to world X/Y (off); absent `on` toggles. Transparent, session-only, not an
//!   undo step; returns `{on, message}` (`<UCS align on>` / `<UCS align off>`).
//!
//! - `newoffsetplan` (`np`) and `newplanline` (`npln`): placed plans at a new level — see
//!   [`crate::newviews`]. NEWSECTIONLINE in a placed section takes one click for a perpendicular cut.
//!
//! - `refpane {on?, viewId?}` (View ▸ LAMB ▸ Reference Pane): the reference pane — two tiles, the
//!   edited view's reference in the other one, chosen for you and overridable; see [`crate::refpane`].
//! - `underlay {viewId?}` (View ▸ LAMB ▸ Underlay): another placed view of the edited view's kind
//!   drawn pale and snappable under it; the current one again, or none, turns it off — see
//!   [`crate::overlays::underlay`].
//! - `resolve {}` (`res`, View ▸ LAMB ▸ Resolve): the project's issues; interactively, step through
//!   them, each one's view edited, centred and haloed — see [`crate::resolvecmd`].
//!
//! - `lambproject` (View ▸ LAMB ▸ Start Project): `Level 1` and a placed `Ground Floor` plan on
//!   it, then edit the plan; refused ("Already a LAMB project") once the drawing has views.
//!
//! - `newdetail {name, cutDirection?}` (View ▸ LAMB ▸ New Detail): a floating detail view drawn in
//!   `plan` (default) or `vertical` (section), entered; interactively `Enter detail name or
//!   [Plan/Section] <Detail N>:`. `detailtag` (`dtag`) / `detailtags`: detail tags in a plan or
//!   section view and their resolution report — see
//!   [`crate::detailtags`]. `dxfimport {path | data, fileName?, name?, unitScale?}` (View ▸ LAMB ▸
//!   Import DXF as View): a DXF/DWG file's line work as a new unplaced plan view — see
//!   [`crate::dxfimport`]. `lambopen {path}` (View ▸ LAMB ▸ Open LAMB Project): a TS-app `.lamb`
//!   project (directory or its JSON file form) opened as a new document tab — see
//!   [`crate::lambopen`]. `lambsave {path}` (View ▸ LAMB ▸ Save LAMB Project): the open drawing
//!   written as a `.lamb` project directory — see [`crate::lambopen`]. `outsidepoint {point:{x,y}}` (View ▸ LAMB ▸ Outside Point): a tagged
//!   POINT in the edited plan view marking its region outside — see [`crate::outside`]. `studgrid {point:{x,y}, pitch?}` (View ▸ LAMB ▸ Stud Grid): the edited
//!   the project's stud grid origin (pitch 406.4), at a point of the edited plan, that every view's repeats line up on; with `wall: true` (and `off?`) a point inside one plan wall's band overrules it for that run only; `studmove {point:{x,y}}` (View ▸ LAMB ▸ Move Stud) a point in one plan or section run's band that its nearest stud or joist is laid centred on — see [`crate::studgrid`]. `assemblyplan {viewId, mitre?}` (View ▸ LAMB ▸ Assembly Plan): the plan's or section's laid
//!   detail bands, then each two-arm corner's placed corner detail, written into its block
//!   `LAMB_ASSEMBLY_<id>` (`corners` counts the placed ones), `mitre` turning the plan's mitring
//!   of undetailed corners on or off — see [`crate::assembly`]; interactive on the edited plan or section:
//!   `Mitre missing corner details [Yes/No]`, Enter keeping the current setting.
//!   `wallconflicts {viewId}` (View ▸ LAMB ▸ Wall Conflicts): each run whose detail is thicker or
//!   thinner than the drawn wall, read-only, in a plan or section; interactive, the edited view's conflicts one per line.
//!   `wallrun {viewId, point:{x,y}}` (View ▸ LAMB ▸ Wall Run): the wall run under a point — its
//!   resolved detail, faces, width and conflict, read-only — see [`crate::wallrun`]; interactive on
//!   the edited plan or section, each pick echoed and its faces haloed.
//!   `wallfit {viewId, runs?, hold?}` (View ▸ LAMB ▸ Wall Fit): moves each conflicted run's faces to
//!   its detail's width, holding the reference face, the exterior, the interior or the centre — see
//!   [`crate::wallfit`]; interactive on the edited plan: `Hold [Reference/Exterior/Interior/Centre]`,
//!   then every conflicted run is fitted.
//!   `detailref {handle?, kind?}` (View ▸ LAMB ▸ Detail Reference): the
//!   edited detail view's reference line and its kind — see [`crate::detailref`].
//!   `repeat {dimension?, handles?}` (View ▸ LAMB ▸ Repeat): in the edited detail view, a linear or
//!   aligned dimension tagged as the pitch of the geometry it repeats — see [`crate::repeats`].
//!
//! - `viewprops {mode, viewId, …}` (View ▸ LAMB ▸ View Properties): rename a view, a placed plan's
//!   cut elevation, a vertical view's intent or flip — see [`crate::viewprops`].
//!
//! - `accept {handles?}` (`acc`), `setrole {handles?, role}`, `layerrole {layer, role}` (View ▸
//!   LAMB): take ownership of AI lines, an entity's role, a layer's default role — see
//!   [`crate::entitycmds`].
//! - `showlinks {handle?}` (`sl`, View ▸ LAMB ▸ Show Links) and `query {…}` (parameter-only):
//!   read-only — an edit's link blast radius, and the filter lookup; see [`crate::querycmds`].
//! - `duplicateview {viewId?, name?}`, `placeview {viewId, kind, …}` (parameter-only),
//!   `unplaceview {viewId?}` and `level {mode, …}` (View ▸ LAMB): a view's lifecycle and the level
//!   cascade — see [`crate::viewlife`].
//! - `cutline {planViewId, elevation}` (`cl`, View ▸ LAMB ▸ Cut Line): move a placed plan's cut
//!   elevation by picking its cut line in a placed section, then placing it — see
//!   [`crate::cutlinecmd`].
//!
//! Creating commands return `{"viewId": <id>}`; each is one undo step (block, layer, insert and
//! document entry together). Refusals use the TS command engine's wording.
//!
//! Each command is also interactive (typed at the command line or picked from View > LAMB): the
//! machines below prompt for what the JSON form takes and call the same `run` functions.
//! [`install_app`] registers all of it for an app process.

use std::sync::OnceLock;

use cadcraft_doc::{EntityKind, Handle, Line, Space};
use cadcraft_engine::{Accept, CommandSpec, EngineError, Input, Interactive, Prompt, Result, Session, Step, register_command_provider};
use cadcraft_geom::Vec2;
use lamb_core::derivation::read::ProjectRead;
use lamb_core::geometry::polyline::line2;
use lamb_core::geometry::polyline::resolved_segments;
use lamb_core::geometry::{IDENTITY_TRANSFORM, Point2};
use lamb_core::model::{
    Actor, AnyView, CutDirection, DetailView, Id, Level, LinkAnchor, LinkOrigin, LinkRecord, Placement, PlanView, VerticalIntent, VerticalView,
    authored_field, is_plan_view,
};
use serde_json::{Value, json};

use crate::maintain::now;
use crate::newviews;
use crate::project::DrawingProject;
use crate::schema::{read_document, view_block_name, write_document};
use crate::sheets::create_sheet;
use crate::views::create_view;

pub(crate) fn other(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

pub(crate) fn bad(cmd: &str, msg: &str) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn str_param<'a>(cmd: &str, p: &'a Value, key: &str) -> Result<&'a str> {
    p.get(key).and_then(Value::as_str).ok_or_else(|| bad(cmd, &format!("`{key}` must be a string")))
}

/// A view id: a JSON number, or a decimal string.
pub(crate) fn id_value(v: &Value) -> Option<Id> {
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).map(Id)
}

pub(crate) fn point_param(cmd: &str, p: &Value, key: &str) -> Result<Point2> {
    let q = p.get(key).ok_or_else(|| bad(cmd, &format!("`{key}` is required")))?;
    let c = |k: &str| q.get(k).and_then(Value::as_f64).filter(|v| v.is_finite());
    match (c("x"), c("y")) {
        (Some(x), Some(y)) => Ok(Point2 { x, y }),
        _ => Err(bad(cmd, &format!("`{key}` must be {{x, y}} with finite numbers"))),
    }
}

pub(crate) fn create(s: &mut Session, view: AnyView) -> Result<Value> {
    let id = create_view(s.doc_mut()?, view).map_err(other)?;
    Ok(json!({ "viewId": id.0 }))
}

fn newplan(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_param("newplan", p, "name")?.to_owned();
    create(s, AnyView::Plan(PlanView { id: Id(0), name, entities: Vec::new(), placement: None }))
}

/// NEWDETAIL: a floating detail view (`cutDirection` plan — the default — or vertical; no
/// condition or hosts yet).
fn newdetail(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_param("newdetail", p, "name")?.to_owned();
    let cut_direction = match p.get("cutDirection").filter(|v| !v.is_null()).map(Value::as_str) {
        None | Some(Some("plan")) => CutDirection::Plan,
        Some(Some("vertical")) => CutDirection::Vertical,
        Some(_) => return Err(bad("newdetail", "`cutDirection` must be plan or vertical")),
    };
    create(
        s,
        AnyView::Detail(DetailView {
            id: Id(0),
            name,
            entities: Vec::new(),
            placement: None,
            condition_type: String::new(),
            host_element_ids: Vec::new(),
            cut_direction,
        }),
    )
}

fn new_vertical(s: &mut Session, p: &Value, cmd: &str, intent: VerticalIntent) -> Result<Value> {
    let name = str_param(cmd, p, "name")?.to_owned();
    let host = p.get("hostViewId").and_then(id_value).ok_or_else(|| bad(cmd, "`hostViewId` is required"))?;
    let from = point_param(cmd, p, "cutFrom")?;
    let to = point_param(cmd, p, "cutTo")?;
    let doc = read_document(s.doc()?).map_err(other)?;
    let host_view = doc.views.iter().find(|v| v.id() == host).ok_or_else(|| other("No such host view"))?;
    if let AnyView::Vertical(v) = host_view
        && v.placement.is_none()
    {
        return Err(other("Host vertical view must be placed"));
    }
    let placement = Placement::Vertical { cut_line: line2(from, to), host_view_id: host, transform: IDENTITY_TRANSFORM, flipped: None };
    create(s, AnyView::Vertical(VerticalView { id: Id(0), name, intent, entities: Vec::new(), placement: Some(placement) }))
}

fn newsectionline(s: &mut Session, p: &Value) -> Result<Value> {
    new_vertical(s, p, "newsectionline", VerticalIntent::Section)
}

fn newelevationline(s: &mut Session, p: &Value) -> Result<Value> {
    new_vertical(s, p, "newelevationline", VerticalIntent::Elevation)
}

fn editview(s: &mut Session, p: &Value) -> Result<Value> {
    let space = match p.get("viewId").filter(|v| !v.is_null()) {
        None => Space::Model,
        Some(v) => {
            let id = id_value(v).ok_or_else(|| other("No such view"))?;
            let doc = read_document(s.doc()?).map_err(other)?;
            if !doc.views.iter().any(|v| v.id() == id) {
                return Err(other("No such view"));
            }
            Space::Block(view_block_name(id))
        }
    };
    enter_space(s, space)?;
    Ok(Value::Null)
}

/// LAMBPROJECT: the TS app's starting state (`bootstrap.ts`) — `Level 1` at floor 0 and a placed
/// plan `Ground Floor` on it at the default cut height, then edit inside the plan. Refused when the
/// drawing already has views.
fn lambproject(s: &mut Session, _p: &Value) -> Result<Value> {
    // Read before `doc_mut`: touching the drawing on a refusal would still record an undo step.
    let doc = read_document(s.doc()?).map_err(other)?;
    if !doc.views.is_empty() {
        return Err(other("Already a LAMB project"));
    }
    let d = s.doc_mut()?;
    let level_id = Id(d.new_handle().0);
    let placement = Placement::Plan { level_id, cut_elevation: doc.defaults.default_cut_height_mm, transform: IDENTITY_TRANSFORM };
    let plan = AnyView::Plan(PlanView { id: Id(0), name: "Ground Floor".into(), entities: Vec::new(), placement: Some(placement) });
    let plan_id = create_view(d, plan).map_err(other)?;
    let mut doc = read_document(d).map_err(other)?;
    let floor_elevation = authored_field(0.0, plan_id, Actor::User { user_id: "local".into() }, &now());
    doc.levels.push(Level { id: level_id, name: "Level 1".into(), order: 0.0, floor_elevation });
    write_document(d, &doc).map_err(other)?;
    crate::import::apply_annotation_scale(d, &doc.defaults);
    enter_space(s, Space::Block(view_block_name(plan_id)))?;
    Ok(json!({ "viewId": plan_id.0, "levelId": level_id.0 }))
}

fn newsheet(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_param("newsheet", p, "name")?.to_owned();
    let ids = |key: &str| match p.get(key).filter(|v| !v.is_null()) {
        None => Ok(Vec::new()),
        Some(v) => {
            let arr = v.as_array().ok_or_else(|| bad("newsheet", &format!("`{key}` must be an array")))?;
            arr.iter().map(|x| id_value(x).ok_or_else(|| other("No such view"))).collect::<Result<Vec<Id>>>()
        }
    };
    let (views, assemblies) = (ids("viewIds")?, ids("assemblyIds")?);
    // Build on a copy: touching the drawing on a refusal would still record an undo step.
    let mut d = s.doc()?.clone();
    let handles = create_sheet(&mut d, &name, &views, &assemblies).map_err(other)?;
    *s.doc_mut()? = d;
    let hex: Vec<String> = handles.iter().map(|h| format!("{:X}", h.0)).collect();
    Ok(json!({ "layout": name, "viewports": hex }))
}

/// One end of a `link` call, as given: the entity id (hex handle string or JSON number), the
/// segment index and param. Shapes are `BadParams`; ranges are checked in [`link`].
struct RawAnchor {
    entity: Id,
    index: Value,
    param: f64,
}

fn raw_anchor(p: &Value, end: &str) -> Result<RawAnchor> {
    let q = p.get(end).filter(|q| q.is_object()).ok_or_else(|| bad("link", &format!("`{end}` must be {{entityId, segmentIndex, segmentParam}}")))?;
    let entity = match q.get("entityId") {
        Some(Value::String(h)) => Handle::parse_hex(h).map(|h| Id(h.0)),
        Some(v) => v.as_u64().map(Id),
        None => None,
    }
    .ok_or_else(|| bad("link", &format!("`{end}.entityId` must be a hex handle or a number")))?;
    let index =
        q.get("segmentIndex").filter(|v| v.is_number()).cloned().ok_or_else(|| bad("link", &format!("`{end}.segmentIndex` must be a number")))?;
    let param = q.get("segmentParam").and_then(Value::as_f64).ok_or_else(|| bad("link", &format!("`{end}.segmentParam` must be a number")))?;
    Ok(RawAnchor { entity, index, param })
}

/// TS T282's range check for one end: the param in 0..1, the index a whole number naming a real
/// segment of the entity (left unchecked when the entity is missing — that refusal comes next).
fn anchor_range(p: &DrawingProject<'_>, end: &str, a: &RawAnchor) -> Result<LinkAnchor> {
    if !(0.0..=1.0).contains(&a.param) {
        return Err(other(format!("Link end {end}: segmentParam {} is outside 0..1", a.param)));
    }
    let Some(index) = a.index.as_u64().and_then(|i| usize::try_from(i).ok()) else {
        return Err(other(format!("Link end {end}: segmentIndex {} is not a whole number >= 0", a.index)));
    };
    if let Some(e) = p.entity(a.entity) {
        let count = e.polyline.as_deref().map_or(1, |poly| resolved_segments(poly).len());
        if index >= count {
            let last = count.checked_sub(1).map_or_else(|| "-1".to_owned(), |n| n.to_string());
            return Err(other(format!("Link end {end}: segmentIndex {index} is past the last segment ({last})")));
        }
    }
    Ok(LinkAnchor { entity_id: a.entity, segment_index: index, segment_param: a.param })
}

fn link(s: &mut Session, p: &Value) -> Result<Value> {
    let (ra, rb) = (raw_anchor(p, "a")?, raw_anchor(p, "b")?);
    let (a, b) = {
        let proj = DrawingProject::new(s.doc()?).map_err(other)?;
        let (a, b) = (anchor_range(&proj, "a", &ra)?, anchor_range(&proj, "b", &rb)?);
        let (va, vb) = (proj.entity_view_id(a.entity_id), proj.entity_view_id(b.entity_id));
        let (Some(va), Some(vb)) = (va, vb) else { return Err(other("No such entity")) };
        if va == vb {
            return Err(other("Link ends must be in different views"));
        }
        (a, b)
    };
    let d = s.doc_mut()?;
    let mut doc = read_document(d).map_err(other)?;
    let id = Id(d.new_handle().0);
    doc.links.push(LinkRecord {
        id,
        a,
        b,
        origin: LinkOrigin::Explicit,
        created_at: now(),
        created_by: Actor::User { user_id: "local".into() },
        stale: None,
    });
    write_document(d, &doc).map_err(other)?;
    Ok(json!({ "linkId": id.0 }))
}

/// LINK's JSON form: `link.apply`, one undo step.
fn link_json(s: &mut Session, p: &Value) -> Result<Value> {
    s.execute("link.apply", p)
}

/// UNLINK's JSON form: `unlink.apply`, one undo step.
fn unlink_json(s: &mut Session, p: &Value) -> Result<Value> {
    s.execute("unlink.apply", p)
}

fn unlink(s: &mut Session, p: &Value) -> Result<Value> {
    let id = p.get("linkId").and_then(id_value).ok_or_else(|| bad("unlink", "`linkId` is required"))?;
    let mut doc = read_document(s.doc()?).map_err(other)?;
    let n = doc.links.len();
    doc.links.retain(|l| l.id != id);
    if doc.links.len() == n {
        return Err(other("No such link"));
    }
    write_document(s.doc_mut()?, &doc).map_err(other)?;
    Ok(Value::Null)
}

// ---------------- interactive machines ----------------

/// The `Plan N` default: one more than the views already of that kind.
fn next_name(s: &Session, prefix: &str, kind: impl Fn(&AnyView) -> bool) -> String {
    let n = s.doc().ok().and_then(|d| read_document(d).ok()).map_or(0, |doc| doc.views.iter().filter(|v| kind(v)).count());
    format!("{prefix} {}", n + 1)
}

/// The LAMB view the session is editing (`Space::Block(view_block_name(id))`), if any.
pub(crate) fn edited_view(s: &Session) -> Option<Id> {
    let Space::Block(name) = &s.state().ok()?.space else { return None };
    let id = Id(name.strip_prefix("LAMB_VIEW_")?.parse().ok()?);
    let doc = read_document(s.doc().ok()?).ok()?;
    (view_block_name(id) == *name && doc.views.iter().any(|v| v.id() == id)).then_some(id)
}

/// The first layout tab's text while editing a LAMB view (LAMB T466): the edited view's name; None
/// in model space, a layout, or a non-LAMB block.
pub fn view_tab_label(s: &Session) -> Option<String> {
    let id = edited_view(s)?;
    let doc = read_document(s.doc().ok()?).ok()?;
    doc.views.iter().find(|v| v.id() == id).map(|v| v.name().to_owned())
}

/// The answer to a name prompt: typed text, or the default on Enter.
pub(crate) fn name_input(i: Input, default: &str) -> Option<String> {
    match i {
        Input::Enter => Some(default.to_owned()),
        Input::Text(t) | Input::Keyword(t) => {
            let t = t.trim();
            Some(if t.is_empty() { default.to_owned() } else { t.to_owned() })
        }
        _ => None,
    }
}

pub(crate) fn kind_word(v: &AnyView) -> &'static str {
    match v {
        AnyView::Plan(_) => "Plan",
        AnyView::Vertical(v) if v.intent == VerticalIntent::Elevation => "Elevation",
        AnyView::Vertical(_) => "Section",
        _ => "Detail",
    }
}

/// Echo `Plan "Ground" created (view 12).` for a creating command's result.
pub(crate) fn echo_created(s: &mut Session, word: &str, name: &str, r: &Value) -> Option<Id> {
    let id = r.get("viewId").and_then(Value::as_u64).map(Id)?;
    s.echo(format!("{word} \"{name}\" created (view {}).", id.0));
    Some(id)
}

/// Make `space` the edit space (EDITVIEW, NEWPLAN, LAMBPROJECT, a Views palette click). A space
/// the session has never shown (no stored view for it) is fitted with zoom extents, so it doesn't
/// open on whatever window the last space had. A shown reference pane then re-picks the other
/// tile's view ([`crate::refpane::refresh`]).
pub(crate) fn enter_space(s: &mut Session, space: Space) -> Result<()> {
    let st = s.state_mut()?;
    let first_visit = !st.views.iter().any(|(sp, _)| *sp == space);
    st.space = space;
    st.mspace = None;
    if first_visit {
        // A failed fit (no viewport yet) still enters the space.
        let _ = s.zoom_extents();
    }
    s.touch();
    // The reference pane follows the edited view.
    crate::refpane::refresh(s)
}

/// NEWPLAN: `Enter plan name <Plan N>:`, then edit inside the new plan. NEWDETAIL the same with
/// `Enter detail name or [Plan/Section] <Detail N>:`, where `P` / `S` set the detail's cut direction
/// and ask the name again.
struct NewPlan {
    default: String,
    detail: bool,
    /// NEWDETAIL's cut direction.
    direction: CutDirection,
}

impl Interactive for NewPlan {
    fn name(&self) -> &'static str {
        if self.detail { "newdetail" } else { "newplan" }
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if !self.detail {
            return Prompt::new("Enter plan name", Accept::TEXT).default(self.default.clone());
        }
        Prompt::new("Enter detail name", Accept::TEXT).kw(&["Plan", "Section"]).default(self.default.clone())
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(name) = name_input(i, &self.default) else { return Ok(Step::Continue) };
        if self.detail {
            let dir = match name.to_ascii_lowercase().as_str() {
                "p" | "plan" => Some((CutDirection::Plan, "<Plan detail>")),
                "s" | "section" => Some((CutDirection::Vertical, "<Section detail>")),
                _ => None,
            };
            if let Some((d, echo)) = dir {
                self.direction = d;
                s.echo(echo);
                return Ok(Step::Continue);
            }
        }
        let dir = if self.direction == CutDirection::Vertical { "vertical" } else { "plan" };
        let (run, word): (fn(&mut Session, &Value) -> Result<Value>, &str) = if self.detail { (newdetail, "Detail") } else { (newplan, "Plan") };
        match run(s, &json!({ "name": name, "cutDirection": dir })) {
            Ok(r) => {
                if let Some(id) = echo_created(s, word, &name, &r) {
                    enter_space(s, Space::Block(view_block_name(id)))?;
                }
            }
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

fn newplan_machine(s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(NewPlan { default: next_name(s, "Plan", is_plan_view), detail: false, direction: CutDirection::Plan }))
}

fn newdetail_machine(s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(NewPlan { default: next_name(s, "Detail", |v| matches!(v, AnyView::Detail(_))), detail: true, direction: CutDirection::Plan }))
}

/// NEWSECTIONLINE / NEWELEVATIONLINE: two points of the cut line in the edited view, then a name.
struct NewVertical {
    intent: VerticalIntent,
    host: Option<Id>,
    first: Option<Vec2>,
    second: Option<Vec2>,
    /// NEWSECTIONLINE begun in a placed section: one click gives a cut perpendicular to it, on its
    /// host plan ([`newviews::perpendicular_cut`]); `host` is the section until then.
    one_click: bool,
}

impl NewVertical {
    fn new(intent: VerticalIntent) -> Self {
        NewVertical { intent, host: None, first: None, second: None, one_click: false }
    }
    fn word(&self) -> &'static str {
        if self.intent == VerticalIntent::Elevation { "Elevation" } else { "Section" }
    }
    fn default_name(&self, s: &Session) -> String {
        let intent = self.intent;
        next_name(s, self.word(), move |v| matches!(v, AnyView::Vertical(v) if v.intent == intent))
    }
}

impl Interactive for NewVertical {
    fn name(&self) -> &'static str {
        if self.intent == VerticalIntent::Elevation { "newelevationline" } else { "newsectionline" }
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match (self.first, self.second) {
            (None, _) if self.one_click => Prompt::new("Click a point for a section perpendicular to this one", Accept::POINT),
            (None, _) => Prompt::new("Specify first point of cut line", Accept::POINT),
            (Some(a), None) => Prompt::new("Specify second point of cut line", Accept::POINT).base(a),
            (Some(_), Some(_)) => Prompt::new(format!("Enter {} name", self.word().to_ascii_lowercase()), Accept::TEXT).default(self.default_name(s)),
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.host = edited_view(s);
        if self.host.is_none() {
            s.echo("Edit a plan or section first (EDITVIEW).");
            return Ok(Step::Done);
        }
        self.one_click = self.intent == VerticalIntent::Section && self.host.is_some_and(|h| newviews::is_placed_vertical(s, h));
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        if let (true, None, Some(view)) = (self.one_click, self.first, self.host) {
            match i {
                Input::Point(p) => match newviews::perpendicular_cut(s, view, p) {
                    Ok(cut) => {
                        self.host = Some(cut.host_view_id);
                        self.first = Some(Vec2::new(cut.cut_from.x, cut.cut_from.y));
                        self.second = Some(Vec2::new(cut.cut_to.x, cut.cut_to.y));
                    }
                    Err(e) => {
                        s.echo(e.to_string());
                        return Ok(Step::Done);
                    }
                },
                Input::Enter => return Ok(Step::Cancel),
                _ => {}
            }
            return Ok(Step::Continue);
        }
        let (Some(first), Some(second), Some(host)) = (self.first, self.second, self.host) else {
            match i {
                Input::Point(p) if self.first.is_none() => self.first = Some(p),
                Input::Point(p) if self.first.is_some_and(|a| a.dist(p) > 1e-9) => self.second = Some(p),
                Input::Point(_) => return Err(other("The cut line needs two different points")),
                Input::Enter => return Ok(Step::Cancel),
                _ => {}
            }
            return Ok(Step::Continue);
        };
        let default = self.default_name(s);
        let Some(name) = name_input(i, &default) else { return Ok(Step::Continue) };
        let params = json!({
            "name": name,
            "hostViewId": host.0,
            "cutFrom": {"x": first.x, "y": first.y},
            "cutTo": {"x": second.x, "y": second.y},
        });
        match new_vertical(s, &params, self.name(), self.intent) {
            Ok(r) => {
                echo_created(s, self.word(), &name, &r);
            }
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
    fn preview(&self, s: &Session, cursor: Vec2) -> Vec<EntityKind> {
        match (self.first, self.second, self.host) {
            (None, _, Some(view)) if self.one_click => newviews::vertical_preview(s, view, cursor),
            (Some(a), None, _) => vec![EntityKind::Line(Line { a: a.to3(0.0), b: cursor.to3(0.0) })],
            _ => Vec::new(),
        }
    }
}

fn newsectionline_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(NewVertical::new(VerticalIntent::Section)))
}

fn newelevationline_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(NewVertical::new(VerticalIntent::Elevation)))
}

/// EDITVIEW: `Enter view name or [Model/?] <Model>:`.
struct EditView;

impl Interactive for EditView {
    fn name(&self) -> &'static str {
        "editview"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter view name", Accept::TEXT).kw(&["Model", "?"]).default("Model")
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let text = match i {
            Input::Cancel => return Ok(Step::Cancel),
            Input::Enter => "Model".to_owned(),
            Input::Text(t) | Input::Keyword(t) => t.trim().to_owned(),
            _ => return Ok(Step::Continue),
        };
        if text.is_empty() || text.eq_ignore_ascii_case("model") {
            enter_space(s, Space::Model)?;
            return Ok(Step::Done);
        }
        let doc = read_document(s.doc()?).map_err(other)?;
        if text == "?" {
            for v in &doc.views {
                s.echo(format!("{}  {}  {}", v.id().0, kind_word(v), v.name()));
            }
            return Ok(Step::Continue);
        }
        let by_name = doc.views.iter().find(|v| v.name().eq_ignore_ascii_case(&text));
        let by_id = || text.parse::<u64>().ok().and_then(|n| doc.views.iter().find(|v| v.id() == Id(n)));
        match by_name.or_else(by_id).map(AnyView::id) {
            Some(id) => {
                enter_space(s, Space::Block(view_block_name(id)))?;
                Ok(Step::Done)
            }
            None => Err(other("No such view")),
        }
    }
}

fn editview_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(EditView))
}

/// NEWSHEET: `Enter sheet name <Sheet N>:`, one viewport per view.
struct NewSheet {
    default: String,
}

impl Interactive for NewSheet {
    fn name(&self) -> &'static str {
        "newsheet"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter sheet name", Accept::TEXT).default(self.default.clone())
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(name) = name_input(i, &self.default) else { return Ok(Step::Continue) };
        match newsheet(s, &json!({ "name": name })) {
            Ok(r) => {
                let n = r.get("viewports").and_then(Value::as_array).map_or(0, Vec::len);
                s.echo(format!("Sheet \"{name}\" created ({n} viewports)."));
            }
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

fn newsheet_machine(s: &Session) -> Result<Box<dyn Interactive>> {
    let taken = |n: &str| s.doc().is_ok_and(|d| d.layout(n).is_some());
    let default = (1..).map(|n| format!("Sheet {n}")).find(|n| !taken(n)).unwrap_or_default();
    Ok(Box::new(NewSheet { default }))
}

/// Register LAMB with an app process: its commands (a command provider) and its overlay and snap
/// providers. Call before anything builds the command registry. Only the first call registers;
/// later calls return its result.
pub fn install_app() -> std::result::Result<(), &'static str> {
    static DONE: OnceLock<std::result::Result<(), &'static str>> = OnceLock::new();
    *DONE.get_or_init(|| {
        register_command_provider(commands)?;
        crate::overlays::register_providers()
    })
}

/// PANELOCK: toggle (or set, with `on`) the panes' camera lock.
fn panelock(s: &mut Session, p: &Value) -> Result<Value> {
    let on = p.get("on").and_then(Value::as_bool).unwrap_or(!s.panes_locked());
    s.set_panes_locked(on);
    Ok(json!({ "on": on, "message": format!("<Pane lock {}>", if on { "on" } else { "off" }) }))
}

/// UCSALIGN: toggle (or set, with `on`) whether ORTHO follows the current pane's rotation.
fn ucsalign(s: &mut Session, p: &Value) -> Result<Value> {
    let on = p.get("on").and_then(Value::as_bool).unwrap_or(s.world_ortho());
    s.set_world_ortho(!on);
    Ok(json!({ "on": on, "message": format!("<UCS align {}>", if on { "on" } else { "off" }) }))
}

/// One LAMB status-bar toggle; the app maps it onto ui-egui's `statusext::StatusToggle`.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusToggle {
    pub label: String,
    pub on: bool,
    pub tip: String,
    pub command: (String, Value),
}

/// The status-bar toggles (LAMB T360): pane LOCK (PANELOCK) and UCS (UCSALIGN), only while the
/// drawing has LAMB views.
pub fn status_toggles(s: &Session) -> Vec<StatusToggle> {
    let has_views = s.doc().ok().and_then(|d| DrawingProject::new(d).ok()).is_some_and(|p| p.views().next().is_some());
    if !has_views {
        return Vec::new();
    }
    vec![
        StatusToggle {
            label: "LOCK".into(),
            on: s.panes_locked(),
            tip: "Pane camera lock (PANELOCK)".into(),
            command: ("panelock".into(), json!({})),
        },
        StatusToggle {
            label: "UCS".into(),
            on: !s.world_ortho(),
            tip: "ORTHO follows the pane's rotation (UCSALIGN, F6)".into(),
            command: ("ucsalign".into(), json!({})),
        },
    ]
}

/// Whether the canvas shows its scale bar HUD (LAMB T363): only in a drawing with LAMB views.
pub fn scale_bar_wanted(d: &cadcraft_doc::Drawing) -> bool {
    DrawingProject::new(d).ok().is_some_and(|p| p.views().next().is_some())
}

/// View ▸ LAMB ▸ <the command's label>.
const MENU: &[&str] = &["View", "LAMB", ""];

/// The LAMB commands, for `register_command_provider`.
pub fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("newplan", "New Plan", newplan).alias(&["npl"]).params("{name}").menu(MENU).interactive(newplan_machine),
        CommandSpec::new("newsectionline", "New Section Line", newsectionline)
            .alias(&["nsl"])
            .params("{name, hostViewId, cutFrom:{x,y}, cutTo:{x,y}}")
            .menu(MENU)
            .interactive(newsectionline_machine),
        CommandSpec::new("newelevationline", "New Elevation Line", newelevationline)
            .alias(&["nel"])
            .params("{name, hostViewId, cutFrom:{x,y}, cutTo:{x,y}}")
            .menu(MENU)
            .interactive(newelevationline_machine),
        CommandSpec::new("newsheet", "New Sheet", newsheet).alias(&["nsh"]).params("{name, viewIds?, assemblyIds?}").menu(MENU).interactive(newsheet_machine),
        CommandSpec::new("editview", "Edit View", editview).params("{viewId?}").noundo().menu(MENU).interactive(editview_machine),
        // The typed/menu forms are outside undo; each link or unlink they make is its own step,
        // recorded by the `.apply` spec they run through (see `crate::linkcmds`).
        CommandSpec::new("link", "Link", link_json)
            .alias(&["lk"])
            .params("{a:{entityId, segmentIndex, segmentParam}, b:{…}}")
            .noundo()
            .menu(MENU)
            .interactive(crate::linkcmds::link_machine),
        CommandSpec::new("unlink", "Unlink", unlink_json)
            .alias(&["ulk"])
            .params("{linkId}")
            .noundo()
            .menu(MENU)
            .interactive(crate::linkcmds::unlink_machine),
        CommandSpec::new("link.apply", "Link", link).params("{a:{entityId, segmentIndex, segmentParam}, b:{…}}"),
        CommandSpec::new("unlink.apply", "Unlink", unlink).params("{linkId}"),
        CommandSpec::new("panelock", "Pane Lock", panelock).params("{on?: bool}").noundo().menu(MENU),
        CommandSpec::new("ucsalign", "UCS Align", ucsalign).params("{on?: bool}").key("F6").noundo().transparent().menu(MENU),
        CommandSpec::new("lambproject", "Start Project", lambproject).menu(MENU),
        CommandSpec::new("refpane", "Reference Pane", crate::refpane::refpane)
            .params("{on?: bool, viewId?}")
            .noundo()
            .menu(MENU)
            .interactive(crate::refpane::refpane_machine),
        CommandSpec::new("underlay", "Underlay", crate::overlays::underlay::underlay)
            .params("{viewId?}")
            .noundo()
            .menu(MENU)
            .interactive(crate::overlays::underlay::underlay_machine),
        CommandSpec::new("resolve", "Resolve", crate::resolvecmd::resolve)
            .alias(&["res"])
            .params("{}")
            .noundo()
            .menu(MENU)
            .interactive(crate::resolvecmd::resolve_machine),
        CommandSpec::new("newoffsetplan", "New Offset Plan", newviews::newoffsetplan)
            .alias(&["np"])
            .params("{mode: \"offset\"|\"height\", fromViewId, floorToFloorMm | floorElevationMm, name?, cutHeightMm?}")
            .menu(MENU)
            .interactive(newviews::newoffsetplan_machine),
        CommandSpec::new("newplanline", "Plan Here", newviews::newplanline)
            .alias(&["npln"])
            .params("{fromVerticalViewId, localPoint:{x,y}, name?, cutHeightMm?}")
            .menu(MENU)
            .interactive(newviews::newplanline_machine),
        CommandSpec::new("viewprops", "View Properties", crate::viewprops::viewprops)
            .params("{mode: rename|cutElevation|intent|flip, viewId, name | cutElevation | intent: section|elevation | flipped}")
            .menu(MENU)
            .interactive(crate::viewprops::viewprops_machine),
        CommandSpec::new("newdetail", "New Detail", newdetail).params("{name, cutDirection?}").menu(MENU).interactive(newdetail_machine),
        CommandSpec::new("detailtag", "Detail Tag", crate::detailtags::detailtag)
            .alias(&["dtag"])
            .params("{detailViewId, corner?: bool, tip:{x,y}, bubble:{x,y}}")
            .menu(MENU)
            .interactive(crate::detailtags::detailtag_machine),
        CommandSpec::new("dxfimport", "Import DXF as View", crate::dxfimport::dxfimport)
            .params("{path | data: base64, fileName?, name?, unitScale?} → {viewId, imported, skipped: {TYPE: n}}")
            .menu(MENU)
            .interactive(crate::dxfimport::dxfimport_machine),
        CommandSpec::new("lambopen", "Open LAMB Project", crate::lambopen::lambopen)
            .params("{path} → {views, entities, skipped: {kind: n}, droppedLinks, unknownLayers, document}")
            .noundo()
            .menu(MENU)
            .interactive(crate::lambopen::lambopen_machine),
        CommandSpec::new("lambsave", "Save LAMB Project", crate::lambopen::lambsave)
            .params("{path} → {views, entities, skipped: {kind: n}, droppedTags, unknownPatterns, path}")
            .noundo()
            .menu(MENU)
            .interactive(crate::lambopen::lambsave_machine),
        CommandSpec::new("outsidepoint", "Outside Point", crate::outside::outsidepoint)
            .params("{point:{x,y}}")
            .menu(MENU)
            .interactive(crate::outside::outsidepoint_machine),
        CommandSpec::new("studgrid", "Stud Grid", crate::studgrid::studgrid)
            .params("{point:{x,y}, pitch?: mm} → {handle, pitch}")
            .menu(MENU)
            .interactive(crate::studgrid::studgrid_machine),
        CommandSpec::new("studmove", "Move Stud", crate::studgrid::studmove)
            .alias(&["smv"])
            .params("{point:{x,y}} → {handle}")
            .menu(MENU)
            .interactive(crate::studgrid::studmove_machine),
        CommandSpec::new("detailtags", "Detail Tags Report", crate::detailtags::detailtags).params("{viewId?}").noundo().menu(MENU),
        CommandSpec::new("assemblyplan", "Assembly Plan", crate::assembly::assemblyplan)
            .params("{viewId, mitre?: bool}")
            .menu(MENU)
            .interactive(crate::assembly::assemblyplan_machine),
        CommandSpec::new("wallconflicts", "Wall Conflicts", crate::assembly::wallconflicts)
            .params("{viewId}")
            .noundo()
            .menu(MENU)
            .interactive(crate::assembly::wallconflicts_machine),
        CommandSpec::new("wallrun", "Wall Run", crate::wallrun::wallrun)
            .params("{viewId, point}")
            .noundo()
            .menu(MENU)
            .interactive(crate::wallrun::wallrun_machine),
        CommandSpec::new("wallfit", "Wall Fit", crate::wallfit::wallfit)
            .params("{viewId, runs?: [index], hold?: reference|exterior|interior|centre}")
            .menu(MENU)
            .interactive(crate::wallfit::wallfit_machine),
        CommandSpec::new("detailref", "Detail Reference", crate::detailref::detailref)
            .params("{handle?, kind?: exterior_face|interior_face|structural_centre|grid}")
            .menu(MENU)
            .interactive(crate::detailref::detailref_machine),
        CommandSpec::new("repeat", "Repeat", crate::repeats::repeat).params("{dimension?, handles?: [handle], copies?: [handle]}").menu(MENU),
        CommandSpec::new(crate::assistant::ID, "Apply Assistant Script", crate::assistant::apply).params("{steps:[{command, params}], proposalId?}"),
        CommandSpec::new(crate::assistant::PROMPT_ID, "Assistant Prompt", crate::assistant::prompt_cmd).params("{message}").noundo(),
        CommandSpec::new(crate::assistant::START_ID, "Assistant Start", crate::assistant::start_cmd).params("{message, dryRun?}").noundo(),
        CommandSpec::new(crate::assistant::RESUME_ID, "Assistant Resume", crate::assistant::resume_cmd).params("{turn, reply}").noundo(),
        CommandSpec::new(crate::assistant::GHOST_ID, "Assistant Ghost", crate::assistant::ghost_cmd).params("{steps?}").noundo(),
        CommandSpec::new("accept", "Accept AI Line", crate::entitycmds::accept)
            .alias(&["acc"])
            .params("{handles?}")
            .menu(MENU)
            .interactive(crate::entitycmds::accept_machine),
        CommandSpec::new("setrole", "Set Role", crate::entitycmds::setrole)
            .params("{handles?, role: cut|seen|datum|view_clip|annotation|reference|bylayer}")
            .menu(MENU)
            .interactive(crate::entitycmds::setrole_machine),
        CommandSpec::new("layerrole", "Layer Default Role", crate::entitycmds::layerrole)
            .params("{layer, role: cut|seen|datum|view_clip|annotation|reference}")
            .menu(MENU)
            .interactive(crate::entitycmds::layerrole_machine),
        CommandSpec::new("showlinks", "Show Links", crate::querycmds::showlinks)
            .alias(&["sl"])
            .params("{handle?}")
            .noundo()
            .menu(MENU)
            .interactive(crate::querycmds::showlinks_machine),
        CommandSpec::new("query", "Query", crate::querycmds::query)
            .params("{viewId?, layerName?, role?, kind?, closed?, near?: {entityIds?, layerName?, minMm?, maxMm?}, include?: \"geometry\", limit?}")
            .noundo(),
        CommandSpec::new("duplicateview", "Duplicate View", crate::viewlife::duplicateview)
            .params("{viewId?, name?}")
            .menu(MENU)
            .interactive(crate::viewlife::duplicateview_machine),
        CommandSpec::new("placeview", "Place View", crate::viewlife::placeview).params(
            "{viewId, kind: \"plan\", levelId, cutElevation, transform} | {viewId, kind: \"vertical\", cutLine, hostViewId, transform, intent?}",
        ),
        CommandSpec::new("unplaceview", "Unplace View", crate::viewlife::unplaceview)
            .params("{viewId?}")
            .menu(MENU)
            .interactive(crate::viewlife::unplaceview_machine),
        CommandSpec::new("level", "Level", crate::viewlife::level)
            .params("{mode: \"create\", name, floorElevationMm, order?} | {mode: \"rename\", levelId, name} | {mode: \"elevation\", levelId, floorElevationMm}")
            .menu(MENU)
            .interactive(crate::viewlife::level_machine),
        CommandSpec::new("cutline", "Cut Line", crate::cutlinecmd::cutline)
            .alias(&["cl"])
            .params("{planViewId, elevation}")
            .menu(MENU)
            .interactive(crate::cutlinecmd::cutline_machine),
    ]
}

#[cfg(test)]
mod tests {
    //! The interactive forms, driven like the command line. The command registry is process-global:
    //! every test calls [`install_app`] first, which registers once.
    use super::*;
    use cadcraft_engine::command_specs;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;

    fn session() -> std::result::Result<Session, Box<dyn std::error::Error>> {
        install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        Ok(s)
    }

    fn views(s: &Session) -> std::result::Result<Vec<AnyView>, Box<dyn std::error::Error>> {
        Ok(read_document(s.doc()?)?.views)
    }

    fn space(s: &Session) -> std::result::Result<Space, Box<dyn std::error::Error>> {
        Ok(s.state()?.space.clone())
    }

    fn text(s: &mut Session, t: &str) -> Result<()> {
        s.input(Input::Text(t.into()))
    }

    #[test]
    fn every_spec_has_a_menu_path_and_a_factory() -> R {
        install_app()?;
        let ids = [
            "newplan",
            "newsectionline",
            "newelevationline",
            "newsheet",
            "editview",
            "newoffsetplan",
            "newplanline",
            "link",
            "unlink",
            "viewprops",
            "newdetail",
            "detailtag",
            "outsidepoint",
            "studgrid",
            "studmove",
            "wallrun",
            "dxfimport",
            "lambopen",
            "lambsave",
            "detailref",
            "accept",
            "setrole",
            "layerrole",
            "cutline",
        ];
        for id in ids {
            let c = command_specs().iter().find(|c| c.id == id).ok_or(id)?;
            assert_eq!(c.menu, MENU, "{id}");
            assert!(c.interactive.is_some(), "{id}");
        }
        Ok(())
    }

    #[test]
    fn newplan_enter_takes_the_default_enters_it_and_undoes() -> R {
        let mut s = session()?;
        s.start("newplan")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter plan name <Plan 1>:"));
        s.input(Input::Enter)?;
        let v = views(&s)?;
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name(), "Plan 1");
        assert_eq!(space(&s)?, Space::Block(view_block_name(v[0].id())));
        assert!(s.log.iter().any(|l| l == &format!("Plan \"Plan 1\" created (view {}).", v[0].id().0)), "{:?}", s.log);

        s.undo()?;
        assert!(views(&s)?.is_empty(), "undo removes the plan");
        assert_eq!(space(&s)?, Space::Model, "undo leaves the removed view's block");
        Ok(())
    }

    #[test]
    fn newsectionline_refuses_in_model_and_cuts_the_edited_plan() -> R {
        let mut s = session()?;
        s.start("newsectionline")?;
        assert!(s.running.is_none(), "refused at once");
        assert!(s.log.iter().any(|l| l == "Edit a plan or section first (EDITVIEW)."), "{:?}", s.log);
        assert!(views(&s)?.is_empty());

        s.start("newplan")?;
        text(&mut s, "Ground")?;
        let plan = views(&s)?[0].id();
        s.start("nsl")?;
        assert_eq!(s.current_prompt().map(|p| p.message), Some("Specify first point of cut line".into()));
        s.input(Input::Point(Vec2::new(0.0, 0.0)))?;
        let p = s.current_prompt().ok_or("no prompt")?;
        assert_eq!(p.base, Some(Vec2::new(0.0, 0.0)));
        s.input(Input::Point(Vec2::new(5000.0, 1000.0)))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter section name <Section 1>:"));
        text(&mut s, "A-A")?;
        let v = views(&s)?;
        let AnyView::Vertical(sec) = v.iter().find(|v| v.name() == "A-A").ok_or("no section")? else { return Err("not vertical".into()) };
        assert_eq!(sec.intent, VerticalIntent::Section);
        let Some(Placement::Vertical { host_view_id, cut_line, .. }) = &sec.placement else { return Err("not placed".into()) };
        assert_eq!(*host_view_id, plan);
        assert_eq!(cut_line, &line2(Point2 { x: 0.0, y: 0.0 }, Point2 { x: 5000.0, y: 1000.0 }));
        assert_eq!(space(&s)?, Space::Block(view_block_name(plan)), "stays in the host");
        Ok(())
    }

    #[test]
    fn newelevationline_preview_is_the_rubber_band() -> R {
        let mut s = session()?;
        s.start("newplan")?;
        s.input(Input::Enter)?;
        s.start("newelevationline")?;
        s.input(Input::Point(Vec2::new(1.0, 2.0)))?;
        let pv = s.running.as_ref().ok_or("not running")?.machine.preview(&s, Vec2::new(3.0, -4.0));
        assert_eq!(pv, vec![EntityKind::Line(Line { a: Vec2::new(1.0, 2.0).to3(0.0), b: Vec2::new(3.0, -4.0).to3(0.0) })]);
        s.input(Input::Point(Vec2::new(3.0, -4.0)))?;
        s.input(Input::Enter)?;
        assert!(views(&s)?.iter().any(|v| v.name() == "Elevation 1"));
        Ok(())
    }

    #[test]
    fn editview_by_name_id_model_and_unknown() -> R {
        let mut s = session()?;
        s.start("newplan")?;
        text(&mut s, "Ground")?;
        s.start("newplan")?;
        text(&mut s, "First")?;
        let v = views(&s)?;
        let (ground, first) = (v[0].id(), v[1].id());

        s.start("editview")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter view name or [Model/?] <Model>:"));
        s.input(Input::Enter)?;
        assert_eq!(space(&s)?, Space::Model);

        s.start("editview")?;
        text(&mut s, "ground")?;
        assert_eq!(space(&s)?, Space::Block(view_block_name(ground)), "by name, any case");

        s.start("editview")?;
        text(&mut s, &first.0.to_string())?;
        assert_eq!(space(&s)?, Space::Block(view_block_name(first)), "by id");

        s.start("editview")?;
        let n = s.log.len();
        text(&mut s, "Nowhere")?;
        assert!(s.log[n..].iter().any(|l| l.contains("No such view")), "{:?}", &s.log[n..]);
        assert!(s.running.is_some(), "re-prompts");
        s.input(Input::Keyword("?".into()))?;
        assert!(s.log.iter().any(|l| l == &format!("{}  Plan  Ground", ground.0)), "{:?}", s.log);
        s.input(Input::Keyword("Model".into()))?;
        assert_eq!(space(&s)?, Space::Model);
        assert!(s.running.is_none());
        Ok(())
    }

    #[test]
    fn newsheet_lays_out_every_view() -> R {
        let mut s = session()?;
        s.start("newplan")?;
        s.input(Input::Enter)?;
        s.start("newsheet")?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("Sheet 1"));
        s.input(Input::Enter)?;
        assert!(s.doc()?.layout("Sheet 1").is_some());
        assert!(s.log.iter().any(|l| l == "Sheet \"Sheet 1\" created (1 viewports)."), "{:?}", s.log);
        Ok(())
    }

    #[test]
    fn lambproject_starts_one_level_and_a_placed_plan_undoes_and_refuses_twice() -> R {
        let mut s = session()?;
        let fresh = |s: &Session, v: &str| s.doc().map(|d| d.header.f64(v, -1.0));
        let (dimscale0, textsize0) = (fresh(&s, "DIMSCALE")?, fresh(&s, "TEXTSIZE")?);
        let r = s.execute("lambproject", &json!({}))?;
        assert_eq!((fresh(&s, "DIMSCALE")?, fresh(&s, "TEXTSIZE")?), (50.0, 150.0), "sized at 1:50 x 3 mm");
        assert!(s.doc()?.dim_overrides().is_empty());
        let doc = read_document(s.doc()?)?;
        assert_eq!(doc.levels.len(), 1);
        let level = &doc.levels[0];
        assert_eq!((level.name.as_str(), level.order, level.floor_elevation.value), ("Level 1", 0.0, 0.0));
        assert_eq!(doc.views.len(), 1);
        let AnyView::Plan(plan) = &doc.views[0] else { return Err("not a plan".into()) };
        assert_eq!(plan.name, "Ground Floor");
        assert_eq!(
            plan.placement,
            Some(Placement::Plan { level_id: level.id, cut_elevation: doc.defaults.default_cut_height_mm, transform: IDENTITY_TRANSFORM })
        );
        assert_eq!(r["viewId"], json!(plan.id.0));
        assert_eq!(space(&s)?, Space::Block(view_block_name(plan.id)), "edits the new plan");

        assert!(s.execute("lambproject", &json!({})).is_err_and(|e| e.to_string().contains("Already a LAMB project")));
        assert_eq!(read_document(s.doc()?)?.views.len(), 1, "a refusal changes nothing");

        s.execute("text", &json!({ "at": [0, 0], "text": "A" }))?;
        let last = s.doc()?.block(&view_block_name(plan.id)).and_then(|b| b.entities.iter().last().cloned()).ok_or("no text")?;
        let cadcraft_doc::EntityKind::Text(t) = &last.kind else { return Err("not a text".into()) };
        assert_eq!(t.height, 150.0, "TEXT takes the LAMB default height");
        s.undo()?;

        s.undo()?;
        let doc = read_document(s.doc()?)?;
        assert!(doc.levels.is_empty() && doc.views.is_empty(), "one undo step removes both");
        assert_eq!((fresh(&s, "DIMSCALE")?, fresh(&s, "TEXTSIZE")?), (dimscale0, textsize0), "and the sizing");
        assert_eq!(space(&s)?, Space::Model);
        Ok(())
    }

    #[test]
    fn entering_a_never_visited_view_fits_it_and_a_revisit_keeps_its_window() -> R {
        let mut s = session()?;
        let id = s.execute("newplan", &json!({ "name": "Far" }))?["viewId"].as_u64().ok_or("no id")?;
        s.execute("editview", &json!({ "viewId": id }))?;
        s.execute("line", &json!({ "points": [[100000, 50000], [101000, 50000]] }))?;
        s.execute("editview", &json!({}))?;
        // As in a freshly opened drawing: no space has a stored view yet.
        s.state_mut()?.views.clear();
        s.execute("view.set", &json!({ "center": [0, 0], "height": 100 }))?;
        s.execute("editview", &json!({ "viewId": id }))?;
        let v = s.state()?.view();
        assert!((v.center.x - 100500.0).abs() < 1.0 && (v.center.y - 50000.0).abs() < 1.0, "fitted to the line: {v:?}");
        s.execute("view.set", &json!({ "center": [5, 5], "height": 7 }))?;
        s.execute("editview", &json!({}))?;
        assert_eq!(s.state()?.view().height, 100.0, "Model keeps its window");
        s.execute("editview", &json!({ "viewId": id }))?;
        assert_eq!(s.state()?.view().height, 7.0, "a revisit keeps the view it had");
        Ok(())
    }

    #[test]
    fn the_view_tab_label_is_the_edited_views_name() -> R {
        let mut s = session()?;
        assert_eq!(view_tab_label(&s), None, "model space");
        let id = s.execute("newplan", &json!({ "name": "Ground floor" }))?["viewId"].as_u64().ok_or("no id")?;
        s.execute("editview", &json!({ "viewId": id }))?;
        assert_eq!(view_tab_label(&s).as_deref(), Some("Ground floor"));
        s.execute("editview", &json!({}))?;
        assert_eq!(view_tab_label(&s), None, "back in model space");
        Ok(())
    }

    #[test]
    fn scale_bar_is_wanted_on_a_lamb_drawing_only() -> R {
        use std::collections::BTreeMap;
        use std::path::Path;
        let plain = session()?;
        assert!(!scale_bar_wanted(plain.doc()?), "a plain drawing has no scale bar");
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb/u-wall-two-sections.lamb.json");
        let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        let project = lamb_core::persistence::files_to_project(&files).map_err(|e| format!("{e:?}"))?;
        let (d, _, _) = crate::import::project_to_drawing(&project)?;
        assert!(scale_bar_wanted(&d), "a drawing with LAMB views shows the scale bar");
        Ok(())
    }

    #[test]
    fn status_toggles_follow_panelock_and_ucsalign_on_a_lamb_drawing_only() -> R {
        use std::collections::BTreeMap;
        use std::path::Path;
        install_app()?;
        let plain = session()?;
        assert!(status_toggles(&plain).is_empty(), "a plain drawing has no LAMB toggles");
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb/u-wall-two-sections.lamb.json");
        let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        let project = lamb_core::persistence::files_to_project(&files).map_err(|e| format!("{e:?}"))?;
        let (d, _, _) = crate::import::project_to_drawing(&project)?;
        let mut s = Session::new();
        s.open_drawing(d, "u-wall", None);
        let state = |s: &Session| status_toggles(s).into_iter().map(|t| (t.label, t.on)).collect::<Vec<_>>();
        assert_eq!(state(&s), vec![("LOCK".to_string(), false), ("UCS".to_string(), true)]);
        for t in status_toggles(&s) {
            s.execute(&t.command.0, &t.command.1)?;
        }
        assert_eq!(state(&s), vec![("LOCK".to_string(), true), ("UCS".to_string(), false)]);
        Ok(())
    }
}

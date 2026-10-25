//! LAMB's view commands as engine `CommandSpec`s, registered once per process with
//! `cadcraft_engine::register_command_provider(lamb_cadcraft::commands)` (FORK.md patch 4).
//!
//! - `newplan {name}` (`npl`): a floating plan view. Interactively NEWPLAN is the TS app's New
//!   Plan and places by default (`New plan [Offset/Height/Floating]`) — see [`crate::newviews`].
//! - `newsectionline` / `newelevationline {name, hostViewId, cutFrom:{x,y}, cutTo:{x,y}}` (`nsl`,
//!   `nel`): a vertical view placed on a straight cut line in the host view; a floating host plan
//!   is placed first, in the same undo step ([`crate::viewlife::place_floating_plan`]).
//! - `editview {viewId?}`: edit inside the view's block; no `viewId` returns to Model.
//! - `newsheet {name, viewIds?, assemblyIds?}` (`nsh`): a paper layout with one viewport per view
//!   (default every view, when neither list is given), then one per plan in `assemblyIds` showing
//!   its assembly plan; each freezes every other view's layer and every other assembly layer;
//!   returns `{layout, viewports: [hex handles]}`. Typed only: a sheet is an ordinary layout (`+`,
//!   MVIEW) whose viewports each pick their view with VPVIEW.
//! - `vpview {handle, viewId | null}`: binds a layout viewport to a view — its `view_id`, every other
//!   view's layer frozen in it, zoomed to the view; `null` unbinds it. One undo step; the
//!   Properties palette's View row runs it ([`crate::props`]).
//!
//! - `link {a:{entityId, segmentIndex, segmentParam}, b:{…}}` (`lk`): an explicit link between two
//!   segments in different views (the escape hatch from the same-layer auto-link rule); `entityId`
//!   is a hex handle string or a JSON number. Returns `{"linkId": <id>}`.
//! - `unlink {linkId}` (`ulk`): removes that link.
//!   Both are interactive too (LAMB ▸ Links ▸ Link / Unlink): pick a line then its match in another
//!   pane, or click a filled marker — see [`crate::linkcmds`]. Both run through `link.apply` /
//!   `unlink.apply`, which hold the undo step.
//! - `panelock {on?}`: camera lock for VPORTS panes (absent `on` toggles; off by default) — panning
//!   or zooming the current tile re-centres every related tile at the same scale. Returns
//!   `{on, message}` (`<Pane lock on>` / `<Pane lock off>`); not an undo step.
//! - `ghosthide {}` / `ghostshow {}`: the 3D pane leaves ghosted geometry (surfaces, massing,
//!   defaulted paths) undrawn / draws it again — see [`crate::scene::without_ghosts`]. Display only:
//!   no element's state changes, RESOLVE still lists them; session-only, not an undo step. Returns
//!   `{hidden, message}` (`<Ghosted geometry hidden: N of M meshes>` / `<Ghosted geometry shown>`).
//! - `ucsalign {on?}`: ORTHO and direct distance entry follow the current pane's rotation (on, the
//!   default) or keep to world X/Y (off); absent `on` toggles. Transparent, session-only, not an
//!   undo step; returns `{on, message}` (`<UCS align on>` / `<UCS align off>`).
//!
//! - `newoffsetplan` (`np`) and `newplanline` (`npln`): placed plans at a new level — see
//!   [`crate::newviews`]. NEWSECTIONLINE in a placed section takes one click for a perpendicular cut.
//! - `newoffsetsection` (`nos`) and `newoffsetelevation` (`noe`): a parallel section/elevation a
//!   typed distance from the edited one — see [`crate::offsetviews`].
//!
//! - `refpane {on?, viewId?}` (LAMB ▸ Display ▸ Reference Pane): the reference pane — two tiles, the
//!   edited view's reference in the other one, chosen for you and overridable; see [`crate::refpane`].
//! - `underlay {viewId?}` (LAMB ▸ Display ▸ Underlay): another placed view of the edited view's kind
//!   drawn pale and snappable under it; the current one again, or none, turns it off — see
//!   [`crate::overlays::underlay`].
//! - `viewhole {boundary?, source?, remove?}` (`vhole`, LAMB ▸ Levels & Cuts ▸ View Hole): a closed polyline
//!   in a placed section or elevation showing a parallel placed view's line work through it — see
//!   [`crate::holes`].
//! - `resolve {}` (`res`, LAMB ▸ Walls & Framing ▸ Review Issues): the project's issues (heights that
//!   disagree, undetailed walls, corners and openings); interactively, step through
//!   them, each one's view edited, centred and haloed — see [`crate::resolvecmd`].
//!
//! - `lambproject` (LAMB ▸ Project ▸ Start Project): `Level 1` and a placed `Ground Floor` plan on
//!   it, then edit the plan; refused ("Already a LAMB project") once the drawing has views.
//!
//! - `newdetail {name, cutDirection?}` (LAMB ▸ New View ▸ New Detail): a floating detail view drawn in
//!   `plan` (default) or `vertical` (section), entered; interactively `Enter detail name or
//!   [Plan/Section] <Detail N>:`. `detailtag` (`dtag`) / `detailtags`: detail tags in a plan or
//!   section view and their resolution report — see
//!   [`crate::detailtags`]. `dxfimport {path | data, fileName?, name?, unitScale?}` (LAMB ▸ Project ▸
//!   Import DXF as View): a DXF/DWG file's line work as a new unplaced plan view — see
//!   [`crate::dxfimport`]. `lambopen {path}` (typed only, Import TS-app Project): TS-app interchange, not the save format
//!   (LAMB's data lives in the DWG/DXF and ordinary Save/Open carry it) — a TS-app `.lamb`
//!   project (directory or its JSON file form) opened as a new document tab — see
//!   [`crate::lambopen`]. `lambsave {path}` (typed only, Export TS-app Project): the open drawing
//!   written as a `.lamb` project directory — see [`crate::lambopen`]. `outsidepoint {point:{x,y}}` (LAMB ▸ Details ▸ Outside Point): a tagged
//!   POINT in the edited plan view marking its region outside — see [`crate::outside`]. `studgrid {point:{x,y}, pitch?}` (LAMB ▸ Walls & Framing ▸ Stud Grid): the edited
//!   the project's stud grid origin (pitch 406.4), at a point of the edited plan, that every view's repeats line up on; with `wall: true` (and `off?`) a point inside one plan wall's band overrules it for that run only; `studmove {point:{x,y}}` (LAMB ▸ Walls & Framing ▸ Move Stud) a point in one plan or section run's band that its nearest stud or joist is laid centred on — see [`crate::studgrid`]. `assemblyplan {viewId, mitre?}` (LAMB ▸ Walls & Framing ▸ Assembly Plan): the plan's or section's laid
//!   detail bands, then each two-arm corner's placed corner detail, written into its block
//!   `LAMB_ASSEMBLY_<id>` (`corners` counts the placed ones), `mitre` turning the plan's mitring
//!   of undetailed corners on or off — see [`crate::assembly`]; interactive on the edited plan or section:
//!   `Mitre missing corner details [Yes/No]`, Enter keeping the current setting.
//!   `assemblyview {viewId?}` (`asv`, LAMB ▸ Walls & Framing ▸ Assembly View) edits inside that block,
//!   read-only (edits regenerate away), and `assemblysource {point:{x,y}, viewId?, aperture?}` names
//!   the run or junction, detail and faces an assembly line came from — see [`crate::asmview`].
//!   `wallconflicts {viewId}` (LAMB ▸ Walls & Framing ▸ Wall Conflicts): each run whose detail is thicker or
//!   thinner than the drawn wall, read-only, in a plan or section; interactive, the edited view's conflicts one per line.
//!   `wallrun {viewId, point:{x,y}}` (LAMB ▸ Walls & Framing ▸ Wall Run): the wall run under a point — its
//!   resolved detail, faces, width and conflict, read-only — see [`crate::wallrun`]; interactive on
//!   the edited plan or section, each pick echoed and its faces haloed.
//!   `wallfit {viewId, runs?, hold?}` (LAMB ▸ Walls & Framing ▸ Wall Fit): moves each conflicted run's faces to
//!   its detail's width, holding the reference face, the exterior, the interior or the centre — see
//!   [`crate::wallfit`]; interactive on the edited plan: `Hold [Reference/Exterior/Interior/Centre]`,
//!   then every conflicted run is fitted.
//!   `detailref {handle?, kind?}` (LAMB ▸ Details ▸ Detail Reference): the
//!   edited detail view's reference line and its kind — see [`crate::detailref`].
//!   `repeat {dimension?, handles?}` (LAMB ▸ Details ▸ Repeat): in the edited detail view, a linear or
//!   aligned dimension tagged as the pitch of the geometry it repeats — see [`crate::repeats`].
//!
//! - `viewprops {mode, viewId, …}` (LAMB ▸ View Setup ▸ View Properties): rename a view, a placed plan's
//!   cut elevation, a vertical view's intent or flip — see [`crate::viewprops`].
//!
//! - `accept {handles?}` (`acc`), `setrole {handles?, role}`, `layerrole {layer, role}` (View ▸
//!   LAMB): take ownership of AI lines, an entity's role, a layer's default role — see
//!   [`crate::entitycmds`].
//! - `showlinks {handle?}` (`sl`, typed only, off the menu) and `query {…}` (parameter-only):
//!   read-only — an edit's link blast radius, and the filter lookup; see [`crate::querycmds`].
//! - `duplicateview {viewId?, name?}`, `placeview {viewId, kind, …}` (interactive: PLACEVIEW / `pv`),
//!   `unplaceview {viewId?}` and `level {mode, …}` (LAMB ▸ View Setup, LAMB ▸ Levels & Cuts): a view's lifecycle and the level
//!   cascade — see [`crate::viewlife`].
//! - `cutline {planViewId, elevation}` (`cl`, LAMB ▸ Levels & Cuts ▸ Cut Line): move a placed plan's cut
//!   elevation by picking its cut line in a placed section, then placing it — see
//!   [`crate::cutlinecmd`].
//! - `levelline {levelId, elevation}` (`lvl`, LAMB ▸ Levels & Cuts ▸ Level Line): move a level (and every
//!   plan on it) by picking its level line in a placed section, then placing it — see
//!   [`crate::levellinecmd`].
//! - `makeview {handles, viewId?, name?}` (`mkv`, LAMB ▸ Project ▸ Make View): loose model-space
//!   entities move into a new floating plan or an existing plan view — see [`crate::makeview`].
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
use lamb_core::derivation::linking::link_still_coincides;
use lamb_core::derivation::read::ProjectRead;
use lamb_core::geometry::polyline::line2;
use lamb_core::geometry::polyline::resolved_segments;
use lamb_core::geometry::{IDENTITY_TRANSFORM, Point2};
use lamb_core::model::{
    Actor, AnyView, CutDirection, DetailView, Id, Level, LinkAnchor, LinkOrigin, LinkRecord, Placement, PlanView, VerticalIntent, VerticalView,
    authored_field,
};
use serde_json::{Value, json};

use crate::maintain::now;
use crate::newviews;
use crate::project::DrawingProject;
use crate::schema::{read_document, view_block_name, write_document};
use crate::sheets::{create_sheet, set_viewport_view};
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

pub(crate) fn newplan(s: &mut Session, p: &Value) -> Result<Value> {
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
    match host_view {
        AnyView::Vertical(v) if v.placement.is_none() => return Err(other("Host vertical view must be placed")),
        // A floating plan host is placed first, in this command's undo step (LAMB T507).
        AnyView::Plan(PlanView { placement: None, .. }) => {
            crate::viewlife::place_floating_plan(s, host)?;
        }
        _ => {}
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

fn vpview(s: &mut Session, p: &Value) -> Result<Value> {
    let handle =
        p.get("handle").and_then(Value::as_str).and_then(Handle::parse_hex).ok_or_else(|| bad("vpview", "`handle` must be a hex viewport handle"))?;
    let view = match p.get("viewId") {
        None | Some(Value::Null) => None,
        Some(v) => Some(id_value(v).ok_or_else(|| other("No such view"))?),
    };
    // Build on a copy: touching the drawing on a refusal would still record an undo step.
    let mut d = s.doc()?.clone();
    set_viewport_view(&mut d, handle, view).map_err(other)?;
    *s.doc_mut()? = d;
    Ok(json!({ "handle": handle.hex(), "viewId": view.map(|v| v.0) }))
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
    let record = {
        let proj = DrawingProject::new(s.doc()?).map_err(other)?;
        let (a, b) = (anchor_range(&proj, "a", &ra)?, anchor_range(&proj, "b", &rb)?);
        let (va, vb) = (proj.entity_view_id(a.entity_id), proj.entity_view_id(b.entity_id));
        let (Some(va), Some(vb)) = (va, vb) else { return Err(other("No such entity")) };
        if va == vb {
            return Err(other("Link ends must be in different views"));
        }
        // LAMB T514: a link exists only where its two lines cross.
        let probe = LinkRecord {
            id: Id(0),
            a,
            b,
            origin: LinkOrigin::Explicit,
            created_at: String::new(),
            created_by: Actor::User { user_id: "local".into() },
            stale: None,
        };
        if !link_still_coincides(&proj, &probe).map_err(other)? {
            return Err(other("Those lines do not cross at a marker"));
        }
        probe
    };
    let d = s.doc_mut()?;
    let mut doc = read_document(d).map_err(other)?;
    let id = Id(d.new_handle().0);
    doc.links.push(LinkRecord { id, created_at: now(), ..record });
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
pub(crate) fn next_name(s: &Session, prefix: &str, kind: impl Fn(&AnyView) -> bool) -> String {
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

/// NEWDETAIL: `Enter detail name or [Plan/Section] <Detail N>:`, where `P` / `S` set the detail's
/// cut direction and ask the name again; then edit inside the new detail.
struct NewDetail {
    default: String,
    direction: CutDirection,
}

impl Interactive for NewDetail {
    fn name(&self) -> &'static str {
        "newdetail"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter detail name", Accept::TEXT).kw(&["Plan", "Section"]).default(self.default.clone())
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(name) = name_input(i, &self.default) else { return Ok(Step::Continue) };
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
        let dir = if self.direction == CutDirection::Vertical { "vertical" } else { "plan" };
        match newdetail(s, &json!({ "name": name, "cutDirection": dir })) {
            Ok(r) => {
                if let Some(id) = echo_created(s, "Detail", &name, &r) {
                    enter_space(s, Space::Block(view_block_name(id)))?;
                }
            }
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

fn newdetail_machine(s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(NewDetail { default: next_name(s, "Detail", |v| matches!(v, AnyView::Detail(_))), direction: CutDirection::Plan }))
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

/// GHOSTHIDE: the 3D pane stops drawing ghosted geometry; the message says how much it hides.
fn ghosthide(s: &mut Session, _: &Value) -> Result<Value> {
    crate::scene::set_ghosts_hidden(true);
    let (n, m) = s
        .doc()
        .ok()
        .and_then(|d| crate::scene::scene(d).ok())
        .map_or((0, 0), |sc| (sc.meshes.len() - crate::scene::without_ghosts(&sc).meshes.len(), sc.meshes.len()));
    Ok(json!({ "hidden": true, "message": format!("<Ghosted geometry hidden: {n} of {m} meshes>") }))
}

/// GHOSTSHOW: the 3D pane draws ghosted geometry again.
fn ghostshow(_: &mut Session, _: &Value) -> Result<Value> {
    crate::scene::set_ghosts_hidden(false);
    Ok(json!({ "hidden": false, "message": "<Ghosted geometry shown>" }))
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

/// LAMB ▸ <submenu> ▸ <the command's label>: the top-level LAMB menu, grouped by task. Item order
/// inside a submenu is registration order in [`commands`].
const MENU_PROJECT: &[&str] = &["LAMB", "Project", ""];
const MENU_NEW_VIEW: &[&str] = &["LAMB", "New View", ""];
const MENU_VIEW_SETUP: &[&str] = &["LAMB", "View Setup", ""];
const MENU_LEVELS: &[&str] = &["LAMB", "Levels & Cuts", ""];
const MENU_DISPLAY: &[&str] = &["LAMB", "Display", ""];
const MENU_LINKS: &[&str] = &["LAMB", "Links", ""];
const MENU_DETAILS: &[&str] = &["LAMB", "Details", ""];
const MENU_WALLS: &[&str] = &["LAMB", "Walls & Framing", ""];
const MENU_ROLES: &[&str] = &["LAMB", "Line Roles", ""];

/// The LAMB commands, for `register_command_provider`.
pub fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("lambproject", "Start Project", lambproject).menu(MENU_PROJECT),
        CommandSpec::new("dxfimport", "Import DXF as View", crate::dxfimport::dxfimport)
            .params("{path | data: base64, fileName?, name?, unitScale?} → {viewId, imported, skipped: {TYPE: n}}")
            .menu(MENU_PROJECT)
            .interactive(crate::dxfimport::dxfimport_machine),
        CommandSpec::new("makeview", "Make View", crate::makeview::makeview)
            .alias(&["mkv"])
            .params("{handles: [hex], viewId?, name?} → {viewId, moved}")
            .menu(MENU_PROJECT)
            .interactive(crate::makeview::makeview_machine),
        CommandSpec::new("newplan", "New Plan", newplan).alias(&["npl"]).params("{name}").menu(MENU_NEW_VIEW).interactive(newviews::newplan_machine),
        CommandSpec::new("newoffsetplan", "New Offset Plan", newviews::newoffsetplan)
            .alias(&["np"])
            .params("{mode: \"offset\"|\"height\", fromViewId, floorToFloorMm | floorElevationMm, name?, cutHeightMm?}")
            .menu(MENU_NEW_VIEW)
            .interactive(newviews::newoffsetplan_machine),
        CommandSpec::new("newplanline", "Plan Here", newviews::newplanline)
            .alias(&["npln"])
            .params("{fromVerticalViewId, localPoint:{x,y}, name?, cutHeightMm?}")
            .menu(MENU_NEW_VIEW)
            .interactive(newviews::newplanline_machine),
        CommandSpec::new("newsectionline", "New Section Line", newsectionline)
            .alias(&["nsl"])
            .params("{name, hostViewId, cutFrom:{x,y}, cutTo:{x,y}}")
            .menu(MENU_NEW_VIEW)
            .interactive(newsectionline_machine),
        CommandSpec::new("newelevationline", "New Elevation Line", newelevationline)
            .alias(&["nel"])
            .params("{name, hostViewId, cutFrom:{x,y}, cutTo:{x,y}}")
            .menu(MENU_NEW_VIEW)
            .interactive(newelevationline_machine),
        CommandSpec::new("newoffsetsection", "New Offset Section", crate::offsetviews::newoffsetsection)
            .alias(&["nos"])
            .params("{fromViewId, offsetMm, name?}")
            .menu(MENU_NEW_VIEW)
            .interactive(crate::offsetviews::newoffsetsection_machine),
        CommandSpec::new("newoffsetelevation", "New Offset Elevation", crate::offsetviews::newoffsetelevation)
            .alias(&["noe"])
            .params("{fromViewId, offsetMm, name?}")
            .menu(MENU_NEW_VIEW)
            .interactive(crate::offsetviews::newoffsetelevation_machine),
        CommandSpec::new("newdetail", "New Detail", newdetail).params("{name, cutDirection?}").menu(MENU_NEW_VIEW).interactive(newdetail_machine),
        CommandSpec::new("duplicateview", "Duplicate View", crate::viewlife::duplicateview)
            .params("{viewId?, name?}")
            .menu(MENU_NEW_VIEW)
            .interactive(crate::viewlife::duplicateview_machine),
        CommandSpec::new("newsheet", "New Sheet", newsheet).alias(&["nsh"]).params("{name, viewIds?, assemblyIds?}").interactive(newsheet_machine),
        CommandSpec::new("vpview", "Viewport View", vpview).params("{handle, viewId | null} → {handle, viewId}"),
        CommandSpec::new("editview", "Edit View", editview).params("{viewId?}").noundo().menu(MENU_VIEW_SETUP).interactive(editview_machine),
        CommandSpec::new("viewprops", "View Properties", crate::viewprops::viewprops)
            .params("{mode: rename|cutElevation|intent|flip|cutOffset, viewId, name | cutElevation | intent: section|elevation | flipped | offsetMm}")
            .menu(MENU_VIEW_SETUP)
            .interactive(crate::viewprops::viewprops_machine),
        CommandSpec::new("unplaceview", "Unplace View", crate::viewlife::unplaceview)
            .params("{viewId?}")
            .menu(MENU_VIEW_SETUP)
            .interactive(crate::viewlife::unplaceview_machine),
        CommandSpec::new("level", "Level", crate::viewlife::level)
            .params("{mode: \"create\", name, floorElevationMm, order?} | {mode: \"rename\", levelId, name} | {mode: \"elevation\", levelId, floorElevationMm}")
            .menu(MENU_LEVELS)
            .interactive(crate::viewlife::level_machine),
        CommandSpec::new("levelline", "Level Line", crate::levellinecmd::levelline)
            .alias(&["lvl"])
            .params("{levelId, elevation}")
            .menu(MENU_LEVELS)
            .interactive(crate::levellinecmd::levelline_machine),
        CommandSpec::new("cutline", "Cut Line", crate::cutlinecmd::cutline)
            .alias(&["cl"])
            .params("{planViewId, elevation} | {viewId, offsetMm}")
            .menu(MENU_LEVELS)
            .interactive(crate::cutlinecmd::cutline_machine),
        CommandSpec::new("viewhole", "View Hole", crate::holes::viewhole)
            .alias(&["vhole"])
            .params("{boundary?, source?, remove?: bool} → {holes} | {boundary, source, sourceName}")
            .menu(MENU_LEVELS)
            .interactive(crate::holes::viewhole_machine),
        CommandSpec::new("refpane", "Reference Pane", crate::refpane::refpane)
            .params("{on?: bool, viewId?}")
            .noundo()
            .menu(MENU_DISPLAY)
            .interactive(crate::refpane::refpane_machine),
        CommandSpec::new("underlay", "Underlay", crate::overlays::underlay::underlay)
            .params("{viewId?}")
            .noundo()
            .menu(MENU_DISPLAY)
            .interactive(crate::overlays::underlay::underlay_machine),
        CommandSpec::new("panelock", "Pane Lock", panelock).params("{on?: bool}").noundo().menu(MENU_DISPLAY),
        CommandSpec::new("ucsalign", "UCS Align", ucsalign).params("{on?: bool}").key("F6").noundo().transparent().menu(MENU_DISPLAY),
        CommandSpec::new("ghosthide", "Ghost Hide", ghosthide).params("{}").noundo().menu(MENU_DISPLAY),
        CommandSpec::new("ghostshow", "Ghost Show", ghostshow).params("{}").noundo().menu(MENU_DISPLAY),
        // The typed/menu forms are outside undo; each link or unlink they make is its own step,
        // recorded by the `.apply` spec they run through (see `crate::linkcmds`).
        CommandSpec::new("link", "Link", link_json)
            .alias(&["lk"])
            .params("{a:{entityId, segmentIndex, segmentParam}, b:{…}}")
            .noundo()
            .menu(MENU_LINKS)
            .interactive(crate::linkcmds::link_machine),
        CommandSpec::new("unlink", "Unlink", unlink_json)
            .alias(&["ulk"])
            .params("{linkId}")
            .noundo()
            .menu(MENU_LINKS)
            .interactive(crate::linkcmds::unlink_machine),
        CommandSpec::new("detailtag", "Detail Tag", crate::detailtags::detailtag)
            .alias(&["dtag"])
            .params("{detailViewId, corner?: bool, tip:{x,y}, bubble:{x,y}}")
            .menu(MENU_DETAILS)
            .interactive(crate::detailtags::detailtag_machine),
        CommandSpec::new("detailtags", "Detail Tags Report", crate::detailtags::detailtags).params("{viewId?}").noundo().menu(MENU_DETAILS),
        CommandSpec::new("detailref", "Detail Reference", crate::detailref::detailref)
            .params("{handle?, kind?: exterior_face|interior_face|structural_centre|grid}")
            .menu(MENU_DETAILS)
            .interactive(crate::detailref::detailref_machine),
        CommandSpec::new("repeat", "Repeat", crate::repeats::repeat).params("{dimension?, handles?: [handle], copies?: [handle]}").menu(MENU_DETAILS),
        CommandSpec::new("outsidepoint", "Outside Point", crate::outside::outsidepoint)
            .params("{point:{x,y}}")
            .menu(MENU_DETAILS)
            .interactive(crate::outside::outsidepoint_machine),
        CommandSpec::new("assemblyplan", "Assembly Plan", crate::assembly::assemblyplan)
            .params("{viewId, mitre?: bool}")
            .menu(MENU_WALLS)
            .interactive(crate::assembly::assemblyplan_machine),
        CommandSpec::new("assemblyview", "Assembly View", crate::asmview::assemblyview)
            .alias(&["asv"])
            .params("{viewId?} → {block}")
            .noundo()
            .menu(MENU_WALLS)
            .interactive(crate::asmview::assemblyview_machine),
        CommandSpec::new("wallrun", "Wall Run", crate::wallrun::wallrun)
            .params("{viewId, point}")
            .noundo()
            .menu(MENU_WALLS)
            .interactive(crate::wallrun::wallrun_machine),
        CommandSpec::new("wallfit", "Wall Fit", crate::wallfit::wallfit)
            .params("{viewId, runs?: [index], hold?: reference|exterior|interior|centre}")
            .menu(MENU_WALLS)
            .interactive(crate::wallfit::wallfit_machine),
        CommandSpec::new("wallconflicts", "Wall Conflicts", crate::assembly::wallconflicts)
            .params("{viewId}")
            .noundo()
            .menu(MENU_WALLS)
            .interactive(crate::assembly::wallconflicts_machine),
        CommandSpec::new("studgrid", "Stud Grid", crate::studgrid::studgrid)
            .params("{point:{x,y}, pitch?: mm} → {handle, pitch}")
            .menu(MENU_WALLS)
            .interactive(crate::studgrid::studgrid_machine),
        CommandSpec::new("studmove", "Move Stud", crate::studgrid::studmove)
            .alias(&["smv"])
            .params("{point:{x,y}} → {handle}")
            .menu(MENU_WALLS)
            .interactive(crate::studgrid::studmove_machine),
        CommandSpec::new("resolve", "Review Issues", crate::resolvecmd::resolve)
            .alias(&["res"])
            .params("{}")
            .noundo()
            .menu(MENU_WALLS)
            .interactive(crate::resolvecmd::resolve_machine),
        CommandSpec::new("setrole", "Set Role", crate::entitycmds::setrole)
            .params("{handles?, role: cut|seen|datum|view_clip|annotation|reference|bylayer}")
            .menu(MENU_ROLES)
            .interactive(crate::entitycmds::setrole_machine),
        CommandSpec::new("layerrole", "Layer Default Role", crate::entitycmds::layerrole)
            .params("{layer, role: cut|seen|datum|view_clip|annotation|reference}")
            .menu(MENU_ROLES)
            .interactive(crate::entitycmds::layerrole_machine),
        CommandSpec::new("accept", "Accept AI Line", crate::entitycmds::accept)
            .alias(&["acc"])
            .params("{handles?}")
            .menu(MENU_ROLES)
            .interactive(crate::entitycmds::accept_machine),
        CommandSpec::new("link.apply", "Link", link).params("{a:{entityId, segmentIndex, segmentParam}, b:{…}}"),
        CommandSpec::new("unlink.apply", "Unlink", unlink).params("{linkId}"),
        CommandSpec::new("lambopen", "Import TS-app Project", crate::lambopen::lambopen)
            .params("{path} → {views, entities, skipped: {kind: n}, droppedLinks, unknownLayers, document}")
            .noundo()
            .interactive(crate::lambopen::lambopen_machine),
        CommandSpec::new("lambsave", "Export TS-app Project", crate::lambopen::lambsave)
            .params("{path} → {views, entities, skipped: {kind: n}, droppedTags, unknownPatterns, path}")
            .noundo()
            .interactive(crate::lambopen::lambsave_machine),
        CommandSpec::new("assemblysource", "Assembly Source", crate::asmview::assemblysource)
            .params("{point:{x,y}, viewId?, aperture?: mm} → {view, run, junction, detail, kind, layer, sources} | null")
            .noundo(),
        CommandSpec::new(crate::assistant::ID, "Apply Assistant Script", crate::assistant::apply).params("{steps:[{command, params}], proposalId?}"),
        CommandSpec::new(crate::assistant::PROMPT_ID, "Assistant Prompt", crate::assistant::prompt_cmd).params("{message}").noundo(),
        CommandSpec::new(crate::assistant::START_ID, "Assistant Start", crate::assistant::start_cmd).params("{message, dryRun?}").noundo(),
        CommandSpec::new(crate::assistant::RESUME_ID, "Assistant Resume", crate::assistant::resume_cmd).params("{turn, reply}").noundo(),
        CommandSpec::new(crate::assistant::GHOST_ID, "Assistant Ghost", crate::assistant::ghost_cmd).params("{steps?}").noundo(),
        CommandSpec::new("showlinks", "Show Links", crate::querycmds::showlinks)
            .alias(&["sl"])
            .params("{handle?}")
            .noundo()
            .interactive(crate::querycmds::showlinks_machine),
        CommandSpec::new("query", "Query", crate::querycmds::query)
            .params("{viewId?, layerName?, role?, kind?, closed?, near?: {entityIds?, layerName?, minMm?, maxMm?}, include?: \"geometry\", limit?}")
            .noundo(),
        CommandSpec::new("placeview", "Place View", crate::viewlife::placeview)
            .alias(&["pv"])
            .params("{viewId, kind: \"plan\", levelId, cutElevation, transform} | {viewId, kind: \"vertical\", cutLine, hostViewId, transform, intent?}")
            .menu(MENU_VIEW_SETUP)
            .interactive(crate::viewlife::placeview_machine),
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

    /// NEWPLAN's `Floating` option: a floating plan named `name`, edited.
    fn floating_plan(s: &mut Session, name: &str) -> Result<()> {
        s.start("newplan")?;
        s.input(Input::Keyword("Floating".into()))?;
        text(s, name)
    }

    #[test]
    fn the_lamb_menu_groups_every_command_by_task() -> R {
        install_app()?;
        let groups: [(&str, &[&str]); 9] = [
            ("Project", &["lambproject", "dxfimport", "makeview"]),
            (
                "New View",
                &[
                    "newplan",
                    "newoffsetplan",
                    "newplanline",
                    "newsectionline",
                    "newelevationline",
                    "newoffsetsection",
                    "newoffsetelevation",
                    "newdetail",
                    "duplicateview",
                ],
            ),
            ("View Setup", &["editview", "viewprops", "unplaceview", "placeview"]),
            ("Levels & Cuts", &["level", "levelline", "cutline", "viewhole"]),
            ("Display", &["refpane", "underlay", "panelock", "ucsalign", "ghosthide", "ghostshow"]),
            ("Links", &["link", "unlink"]),
            ("Details", &["detailtag", "detailtags", "detailref", "repeat", "outsidepoint"]),
            ("Walls & Framing", &["assemblyplan", "assemblyview", "wallrun", "wallfit", "wallconflicts", "studgrid", "studmove", "resolve"]),
            ("Line Roles", &["setrole", "layerrole", "accept"]),
        ];
        let ours: Vec<&str> = commands().iter().map(|c| c.id).collect();
        let specs: Vec<_> = command_specs().iter().filter(|c| ours.contains(&c.id)).collect();
        for (sub, ids) in groups {
            let got: Vec<&str> = specs.iter().filter(|c| c.menu.first() == Some(&"LAMB") && c.menu.get(1) == Some(&sub)).map(|c| c.id).collect();
            assert_eq!(got, ids, "{sub}");
            for c in specs.iter().filter(|c| ids.contains(&c.id)) {
                assert_eq!(c.menu, ["LAMB", sub, ""], "{}", c.id);
            }
        }
        let menued = specs.iter().filter(|c| !c.menu.is_empty()).count();
        assert_eq!(menued, groups.iter().map(|(_, ids)| ids.len()).sum::<usize>(), "every menu item is in a listed submenu");
        assert!(specs.iter().all(|c| c.menu.first() != Some(&"View")), "View ▸ LAMB is gone");
        for id in ["showlinks", "lambopen", "lambsave", "newsheet"] {
            let c = command_specs().iter().find(|c| c.id == id).ok_or(id)?;
            assert!(c.menu.is_empty(), "{id} is typed only");
            assert!(c.interactive.is_some(), "{id}");
        }
        // One-shot commands with no prompts; everything else on the menu prompts.
        let one_shot = ["lambproject", "panelock", "ucsalign", "ghosthide", "ghostshow", "detailtags", "repeat"];
        for (sub, ids) in groups {
            for id in ids.iter().filter(|id| !one_shot.contains(id)) {
                let c = command_specs().iter().find(|c| c.id == *id).ok_or(*id)?;
                assert!(c.interactive.is_some(), "{sub} ▸ {id} has an interactive form");
            }
        }
        assert_eq!(command_specs().iter().find(|c| c.id == "resolve").map(|c| c.label), Some("Review Issues"));
        Ok(())
    }

    #[test]
    fn newplan_floating_takes_the_default_name_enters_it_and_undoes() -> R {
        let mut s = session()?;
        s.start("newplan")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("New plan or [Height/Floating] <Height>:"));
        s.input(Input::Text("f".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter plan name <Plan 1>:"));
        s.input(Input::Enter)?;
        let v = views(&s)?;
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name(), "Plan 1");
        assert!(v[0].placement().is_none(), "floating on request");
        assert_eq!(space(&s)?, Space::Block(view_block_name(v[0].id())));
        assert!(s.log.iter().any(|l| l == &format!("Plan \"Plan 1\" created (view {}).", v[0].id().0)), "{:?}", s.log);

        s.undo()?;
        assert!(views(&s)?.is_empty(), "undo removes the plan");
        assert_eq!(space(&s)?, Space::Model, "undo leaves the removed view's block");
        Ok(())
    }

    /// LAMB T507: NEWPLAN in an empty drawing places the plan, so a section drawn in it derives.
    #[test]
    fn newplan_places_by_default_and_a_section_in_it_derives() -> R {
        use lamb_core::derivation::markers::{compute_cut_lines_for_view, compute_markers_for_view};
        use lamb_core::derivation::reference::find_eligible_reference_views;
        let mut s = session()?;
        s.start("newplan")?;
        s.input(Input::Enter)?; // Height
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Specify absolute floor elevation"));
        s.input(Input::Enter)?; // 0
        assert!(s.running.is_none());
        let doc = read_document(s.doc()?)?;
        let level = doc.levels.first().ok_or("no level")?;
        assert_eq!((doc.levels.len(), level.name.as_str(), level.floor_elevation.value, level.order), (1, "Level 1", 0.0, 0.0));
        let plan = doc.views.first().ok_or("no plan")?.id();
        assert_eq!(
            doc.views[0].placement(),
            Some(&Placement::Plan { level_id: level.id, cut_elevation: doc.defaults.default_cut_height_mm, transform: IDENTITY_TRANSFORM })
        );
        assert_eq!(space(&s)?, Space::Block(view_block_name(plan)), "edits the new plan");

        s.execute("line", &json!({ "points": [[0, -1000], [0, 1000]] }))?;
        let r = s.execute(
            "newsectionline",
            &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": -2000, "y": 0}, "cutTo": {"x": 2000, "y": 0} }),
        )?;
        let section = Id(r["viewId"].as_u64().ok_or("no id")?);
        let proj = DrawingProject::new(s.doc()?)?;
        assert!(!compute_cut_lines_for_view(&proj, plan)?.is_empty(), "the cut line shows in the plan");
        assert_eq!(compute_markers_for_view(&proj, section)?.len(), 1, "the crossed line is a marker in the section");
        assert!(find_eligible_reference_views(&proj, plan).contains(&section), "the plan can reference the section");
        Ok(())
    }

    /// LAMB T507: a section drawn in a floating plan places the plan in the same undo step.
    #[test]
    fn a_section_in_a_floating_plan_places_it_in_one_undo_step() -> R {
        let mut s = session()?;
        let plan = Id(s.execute("newplan", &json!({ "name": "Loose" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("newsectionline", &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 0}, "cutTo": {"x": 5000, "y": 0} }))?;
        let doc = read_document(s.doc()?)?;
        let level = doc.levels.first().ok_or("no level created")?;
        assert_eq!(level.name, "Level 1");
        assert!(matches!(doc.views[0].placement(), Some(Placement::Plan { level_id, .. }) if *level_id == level.id));
        assert!(s.log.iter().any(|l| l == "Loose placed on Level 1"), "{:?}", s.log);
        s.undo()?;
        let doc = read_document(s.doc()?)?;
        assert_eq!(doc.views.len(), 1, "the section is gone");
        assert!(doc.views[0].placement().is_none(), "the plan floats again");
        assert!(doc.levels.is_empty(), "and its level is gone");
        Ok(())
    }

    #[test]
    fn newsectionline_refuses_in_model_and_cuts_the_edited_plan() -> R {
        let mut s = session()?;
        s.start("newsectionline")?;
        assert!(s.running.is_none(), "refused at once");
        assert!(s.log.iter().any(|l| l == "Edit a plan or section first (EDITVIEW)."), "{:?}", s.log);
        assert!(views(&s)?.is_empty());

        floating_plan(&mut s, "Ground")?;
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
        floating_plan(&mut s, "Plan 1")?;
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
        floating_plan(&mut s, "Ground")?;
        floating_plan(&mut s, "First")?;
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
        floating_plan(&mut s, "Plan 1")?;
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

    /// GHOSTHIDE / GHOSTSHOW on T493's room whose top wall is untyped (three runs laid, the rest
    /// massing). One test touches the process-wide flag, and leaves it off.
    #[test]
    fn ghosthide_and_ghostshow_only_toggle_the_display_flag() -> R {
        install_app()?;
        let (mut s, plan) = crate::assembly::tests::room("exterior_face", true)?;
        let old: Vec<String> = crate::layered::tests::plines(&s, plan)?.iter().map(|h| h.hex()).collect();
        s.execute("erase", &json!({ "handles": old }))?;
        s.execute("layer.current", &json!({ "name": "WALL-EXT" }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [5000, 0], [5000, 4000], [0, 4000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [4800, 200], [4800, 3700], [200, 3700]], "closed": true }))?;
        let level = s.execute("level", &json!({ "mode": "create", "name": "L1", "floorElevationMm": 0 }))?["levelId"].as_u64().ok_or("no id")?;
        let t = json!({ "originX": 0.0, "originY": 0.0, "rotation": 0.0 });
        s.execute("placeview", &json!({ "viewId": plan.0, "kind": "plan", "levelId": level, "cutElevation": 1200, "transform": t }))?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;

        let before = crate::scene::scene(s.doc()?)?;
        let massing = before
            .meshes
            .iter()
            .filter(|m| m.kind == crate::scene::SceneMeshKind::Surface || m.construction == Some(crate::scene::ConstructionState::Massing))
            .count();
        assert!(massing > 0 && massing < before.meshes.len(), "some massing, some laid layers");
        let issues = s.execute("resolve", &json!({}))?;
        let undo = s.state()?.undo.len();

        let r = s.execute("ghosthide", &json!({}));
        let hidden = crate::scene::ghosts_hidden();
        let after = crate::scene::scene(s.doc()?);
        let issues_after = s.execute("resolve", &json!({}));
        let undo_after = s.state()?.undo.len();
        let shown = s.execute("ghostshow", &json!({}));
        let hidden_after_show = crate::scene::ghosts_hidden();
        crate::scene::set_ghosts_hidden(false);

        let r = r?;
        assert_eq!(r["hidden"], json!(true));
        assert_eq!(r["message"], json!(format!("<Ghosted geometry hidden: {massing} of {} meshes>", before.meshes.len())));
        assert!(hidden);
        assert_eq!(after?, before, "scene() never filters");
        assert_eq!(issues_after?, issues, "RESOLVE still lists everything");
        assert_eq!(undo_after, undo);
        let shown = shown?;
        assert_eq!(shown["hidden"], json!(false));
        assert_eq!(shown["message"], json!("<Ghosted geometry shown>"));
        assert!(!hidden_after_show);
        assert_eq!(s.state()?.undo.len(), undo, "neither is an undo step");

        // A drawing with no LAMB model hides nothing.
        let mut plain = session()?;
        let r = plain.execute("ghosthide", &json!({}));
        crate::scene::set_ghosts_hidden(false);
        assert_eq!(r?["message"], json!("<Ghosted geometry hidden: 0 of 0 meshes>"));
        Ok(())
    }
}

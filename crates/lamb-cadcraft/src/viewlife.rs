//! A view's lifecycle and the level cascade (LAMB T357), ported from the TS command engine's
//! `applyDuplicateView` / `applyPlaceView` / `applyUnplaceView`
//! (`packages/command-engine/src/commands/viewEdits.ts`) and `applyLevel` (`commands/levels.ts`),
//! refusals word for word:
//!
//! - `duplicateview {viewId?, name?}` (no alias: `dv` is DVIEW's): every entity of the view's block copied verbatim
//!   (`Common` and `EntityKind` cloned, so `ext["LAMB"]` — role, AI origin, corner tags — rides
//!   along) into a new **unplaced** view of the same kind; no links copied. Returns
//!   `{viewId, entities}`.
//! - `placeview {viewId, kind:"plan", levelId, cutElevation, transform}` |
//!   `{viewId, kind:"vertical", cutLine, hostViewId, transform, intent?}`: placing a floating view
//!   as the other kind converts it. Interactively (LAMB T506, alias `pv`): name, `Place as
//!   [Plan/Section/Elevation]`, then a level, cut height and base/target point pairs (translation
//!   and rotation, no scale), or a cut line clicked in a placed plan plus a base point's elevation.
//! - `unplaceview {viewId?}`: back to floating, dropping every link with an end on one of the
//!   view's entities. Returns `{droppedLinks}`.
//! - `level {mode:"create", name, floorElevationMm, order?}` | `{mode:"rename", levelId, name}` |
//!   `{mode:"elevation", levelId, floorElevationMm}`: moving a level moves every placed plan on it,
//!   each keeping its own cut height above the floor. LEVEL begun in a floating plan places it
//!   first ([`place_floating_plan`]).
//! - [`place_floating_plan`] (LAMB T507): a floating plan placed on the first level by `order`
//!   (`Level 1` at 0 created when there is none) at that floor plus the default cut height, identity
//!   transform, inside the caller's undo step — NEWSECTIONLINE, NEWPLANLINE and LEVEL repair a
//!   floating host with it instead of refusing.
//!
//! Each is one undo step (the drawing snapshot is the inverse); every refusal is decided on a read
//! before `doc_mut`, so it records no step.

use cadcraft_doc::{Common, EntityKind, Space};
use cadcraft_engine::geom::Vec2;
use cadcraft_engine::prompt::parse_point;
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step, units::parse_distance};
use lamb_core::geometry::polyline::Polyline2;
use lamb_core::geometry::transform2d::local_to_world2;
use lamb_core::geometry::{Point2, Transform2D};
use lamb_core::model::{Actor, AnyView, Id, Level, Placement, PlanView, VerticalIntent, VerticalView, authored_field};
use serde_json::{Value, json};

use crate::commands::{bad, echo_created, edited_view, id_value, kind_word, name_input, other};
use crate::maintain::now;
use crate::schema::{LambDocument, read_document, view_block_name, write_document};
use crate::views::create_view;

fn user() -> Actor {
    Actor::User { user_id: "local".into() }
}

/// `p[key]` as a view or level id, else the edited view (when `fallback`).
fn view_param(s: &Session, cmd: &str, p: &Value, fallback: bool) -> Result<Id> {
    match p.get("viewId").filter(|v| !v.is_null()) {
        Some(v) => id_value(v).ok_or_else(|| bad(cmd, "`viewId` must be a view id")),
        None if fallback => edited_view(s).ok_or_else(|| bad(cmd, "`viewId` is required (or edit a view first)")),
        None => Err(bad(cmd, "`viewId` is required")),
    }
}

fn finite(cmd: &str, p: &Value, key: &str) -> Result<f64> {
    p.get(key).and_then(Value::as_f64).filter(|v| v.is_finite()).ok_or_else(|| bad(cmd, &format!("`{key}` must be a finite number")))
}

fn str_of<'a>(cmd: &str, p: &'a Value, key: &str) -> Result<&'a str> {
    p.get(key).and_then(Value::as_str).ok_or_else(|| bad(cmd, &format!("`{key}` must be a string")))
}

fn doc_of(s: &Session) -> Result<LambDocument> {
    read_document(s.doc()?).map_err(other)
}

// ---------------- DUPLICATEVIEW ----------------

/// DUPLICATEVIEW (TS `applyDuplicateView`).
pub(crate) fn duplicateview(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "duplicateview";
    let id = view_param(s, CMD, p, true)?;
    let doc = doc_of(s)?;
    let source = doc.views.iter().find(|v| v.id() == id).ok_or_else(|| other("No such view"))?;
    let name = match p.get("name").filter(|v| !v.is_null()) {
        Some(v) => v.as_str().ok_or_else(|| bad(CMD, "`name` must be a string"))?.to_owned(),
        None => format!("{} copy", source.name()),
    };
    let view = match source {
        AnyView::Plan(_) => AnyView::Plan(PlanView { id: Id(0), name, entities: Vec::new(), placement: None }),
        AnyView::Vertical(v) => AnyView::Vertical(VerticalView { id: Id(0), name, intent: v.intent, entities: Vec::new(), placement: None }),
        AnyView::Detail(_) => return Err(other("DUPLICATEVIEW does not support detail views")),
    };
    let copies: Vec<(Common, EntityKind)> =
        s.doc()?.block(&view_block_name(id)).map(|b| b.entities.iter().map(|e| (e.common.clone(), e.kind.clone())).collect()).unwrap_or_default();
    let d = s.doc_mut()?;
    let new_id = create_view(d, view).map_err(other)?;
    let space = Space::Block(view_block_name(new_id));
    for (common, kind) in copies.iter().cloned() {
        d.add(&space, common, kind).map_err(other)?;
    }
    Ok(json!({ "viewId": new_id.0, "entities": copies.len() }))
}

// ---------------- PLACEVIEW ----------------

/// A TS `Polyline2` (`bulge` and `closed` optional) as lamb-core's.
fn cut_line(cmd: &str, p: &Value) -> Result<Polyline2> {
    let mut v = p.get("cutLine").cloned().ok_or_else(|| bad(cmd, "`cutLine` is required"))?;
    if let Some(o) = v.as_object_mut() {
        o.entry("closed").or_insert(json!(false));
        if let Some(segs) = o.get_mut("segments").and_then(Value::as_array_mut) {
            for seg in segs.iter_mut().filter_map(Value::as_object_mut) {
                seg.entry("bulge").or_insert(json!(0));
            }
        }
    }
    serde_json::from_value(v).map_err(|e| bad(cmd, &format!("`cutLine` must be {{start, segments:[{{to, bulge?}}], closed?}}: {e}")))
}

fn transform(cmd: &str, p: &Value) -> Result<Transform2D> {
    let v = p.get("transform").cloned().ok_or_else(|| bad(cmd, "`transform` is required"))?;
    serde_json::from_value(v).map_err(|_| bad(cmd, "`transform` must be {originX, originY, rotation}"))
}

fn intent_param(cmd: &str, p: &Value) -> Result<Option<VerticalIntent>> {
    match p.get("intent").filter(|v| !v.is_null()).map(Value::as_str) {
        None => Ok(None),
        Some(Some("section")) => Ok(Some(VerticalIntent::Section)),
        Some(Some("elevation")) => Ok(Some(VerticalIntent::Elevation)),
        Some(_) => Err(bad(cmd, "`intent` must be section|elevation")),
    }
}

/// PLACEVIEW (TS `applyPlaceView`, T156's conversion included).
pub(crate) fn placeview(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "placeview";
    let id = view_param(s, CMD, p, false)?;
    let mut doc = doc_of(s)?;
    let idx = doc.views.iter().position(|v| v.id() == id).ok_or_else(|| other("No such view"))?;
    let (name, previous_intent) = match &doc.views[idx] {
        AnyView::Detail(_) => return Err(other("PLACEVIEW does not support detail views")),
        v if v.placement().is_some() => return Err(other("View is already placed; use UNPLACEVIEW first")),
        AnyView::Plan(v) => (v.name.clone(), None),
        AnyView::Vertical(v) => (v.name.clone(), Some(v.intent)),
    };
    let new_view = match p.get("kind").and_then(Value::as_str) {
        Some("plan") => {
            let level_id = p.get("levelId").and_then(id_value).ok_or_else(|| bad(CMD, "`levelId` is required"))?;
            if !doc.levels.iter().any(|l| l.id == level_id) {
                return Err(other(format!("No such level: {}", level_id.0)));
            }
            let placement = Placement::Plan { level_id, cut_elevation: finite(CMD, p, "cutElevation")?, transform: transform(CMD, p)? };
            AnyView::Plan(PlanView { id, name, entities: Vec::new(), placement: Some(placement) })
        }
        Some("vertical") => {
            let host = p.get("hostViewId").and_then(id_value).ok_or_else(|| bad(CMD, "`hostViewId` is required"))?;
            if !doc.views.iter().any(|v| v.id() == host) {
                return Err(other(format!("No such host view: {}", host.0)));
            }
            let intent = intent_param(CMD, p)?.or(previous_intent).unwrap_or(VerticalIntent::Section);
            let placement = Placement::Vertical { cut_line: cut_line(CMD, p)?, host_view_id: host, transform: transform(CMD, p)?, flipped: None };
            AnyView::Vertical(VerticalView { id, name, intent, entities: Vec::new(), placement: Some(placement) })
        }
        _ => return Err(bad(CMD, "`kind` must be \"plan\" or \"vertical\"")),
    };
    doc.views[idx] = new_view;
    write_document(s.doc_mut()?, &doc).map_err(other)?;
    Ok(Value::Null)
}

/// Place the floating plan `plan` on the first level by `order` (creating `Level 1` at floor 0 when
/// there is none) at that floor plus the default cut height, identity transform. No step of its
/// own: it lands in the caller's. Echoes `<plan> placed on <level>`; returns the level id.
pub(crate) fn place_floating_plan(s: &mut Session, plan: Id) -> Result<Id> {
    let doc = doc_of(s)?;
    let name = doc.views.iter().find(|v| v.id() == plan).map(|v| v.name().to_owned()).ok_or_else(|| other("No such view"))?;
    let first = doc.levels.iter().min_by(|a, b| a.order.total_cmp(&b.order)).map(|l| (l.id, l.name.clone(), l.floor_elevation.value));
    let (level_id, level_name, floor) = match first {
        Some(l) => l,
        None => {
            let r = level(s, &json!({ "mode": "create", "name": "Level 1", "floorElevationMm": 0.0 }))?;
            (r.get("levelId").and_then(id_value).ok_or_else(|| other("level create returned no id"))?, "Level 1".to_owned(), 0.0)
        }
    };
    let cut = floor + doc.defaults.default_cut_height_mm;
    let t = json!({ "originX": 0.0, "originY": 0.0, "rotation": 0.0 });
    placeview(s, &json!({ "viewId": plan.0, "kind": "plan", "levelId": level_id.0, "cutElevation": cut, "transform": t }))?;
    s.echo(format!("{name} placed on {level_name}"));
    Ok(level_id)
}

// ---------------- UNPLACEVIEW ----------------

/// UNPLACEVIEW (TS `applyUnplaceView`).
pub(crate) fn unplaceview(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "unplaceview";
    let id = view_param(s, CMD, p, true)?;
    let mut doc = doc_of(s)?;
    let view = doc.views.iter_mut().find(|v| v.id() == id).filter(|v| v.placement().is_some()).ok_or_else(|| other("View is not placed"))?;
    match view {
        AnyView::Plan(v) => v.placement = None,
        AnyView::Vertical(v) => v.placement = None,
        AnyView::Detail(v) => v.placement = None,
    }
    let entities: Vec<Id> = s.doc()?.block(&view_block_name(id)).map(|b| b.entities.iter().map(|e| Id(e.handle.0)).collect()).unwrap_or_default();
    let before = doc.links.len();
    doc.links.retain(|l| !entities.contains(&l.a.entity_id) && !entities.contains(&l.b.entity_id));
    let dropped = before - doc.links.len();
    write_document(s.doc_mut()?, &doc).map_err(other)?;
    Ok(json!({ "droppedLinks": dropped }))
}

// ---------------- LEVEL ----------------

fn level_id_param(cmd: &str, p: &Value) -> Result<Id> {
    p.get("levelId").and_then(id_value).ok_or_else(|| bad(cmd, "`levelId` is required"))
}

fn no_level(p: &Value) -> cadcraft_engine::EngineError {
    let shown = p.get("levelId").map_or_else(String::new, |v| v.as_str().map_or_else(|| v.to_string(), str::to_owned));
    other(format!("No such level: {shown}"))
}

/// LEVEL (TS `applyLevel`), the elevation cascade included.
pub(crate) fn level(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "level";
    let mut doc = doc_of(s)?;
    match p.get("mode").and_then(Value::as_str) {
        Some("create") => {
            let name = str_of(CMD, p, "name")?.to_owned();
            let floor = finite(CMD, p, "floorElevationMm")?;
            let order = match p.get("order").filter(|v| !v.is_null()) {
                Some(_) => finite(CMD, p, "order")?,
                None => doc.levels.iter().map(|l| l.order).fold(None, |m: Option<f64>, o| Some(m.map_or(o, |m| m.max(o)))).map_or(0.0, |m| m + 1.0),
            };
            let d = s.doc_mut()?;
            let id = Id(d.new_handle().0);
            doc.levels.push(Level { id, name, order, floor_elevation: authored_field(floor, Id(0), user(), &now()) });
            write_document(d, &doc).map_err(other)?;
            Ok(json!({ "levelId": id.0 }))
        }
        Some("rename") => {
            let id = level_id_param(CMD, p)?;
            let name = str_of(CMD, p, "name")?.to_owned();
            let level = doc.levels.iter_mut().find(|l| l.id == id).ok_or_else(|| no_level(p))?;
            level.name = name;
            write_document(s.doc_mut()?, &doc).map_err(other)?;
            Ok(Value::Null)
        }
        Some("elevation") => {
            let id = level_id_param(CMD, p)?;
            let new_floor = finite(CMD, p, "floorElevationMm")?;
            let old_floor = doc.levels.iter().find(|l| l.id == id).ok_or_else(|| no_level(p))?.floor_elevation.value;
            let mut first_plan = None;
            let mut moved = 0usize;
            for v in &mut doc.views {
                if let AnyView::Plan(PlanView { id: view_id, placement: Some(Placement::Plan { level_id, cut_elevation, .. }), .. }) = v
                    && *level_id == id
                {
                    // Each plan keeps its own cut height above the floor.
                    *cut_elevation = new_floor + (*cut_elevation - old_floor);
                    first_plan.get_or_insert(*view_id);
                    moved += 1;
                }
            }
            if let Some(level) = doc.levels.iter_mut().find(|l| l.id == id) {
                level.floor_elevation = authored_field(new_floor, first_plan.unwrap_or(Id(0)), user(), &now());
            }
            write_document(s.doc_mut()?, &doc).map_err(other)?;
            Ok(json!({ "plans": moved }))
        }
        _ => Err(bad(CMD, "`mode` must be create|rename|elevation")),
    }
}

// ---------------- interactive machines ----------------

/// DUPLICATEVIEW on the edited view: `Enter name for the copy <… copy>:`.
struct DuplicateView {
    view: Option<(Id, String, String)>,
}

impl Interactive for DuplicateView {
    fn name(&self) -> &'static str {
        "duplicateview"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        let p = Prompt::new("Enter name for the copy", Accept::TEXT);
        match &self.view {
            Some((_, default, _)) => p.default(default.clone()),
            None => p,
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let view = edited_view(s).and_then(|id| doc_of(s).ok()?.views.into_iter().find(|v| v.id() == id));
        match view {
            None => {
                s.echo("Edit a view first (EDITVIEW).");
                Ok(Step::Done)
            }
            Some(AnyView::Detail(_)) => {
                s.echo("DUPLICATEVIEW does not support detail views");
                Ok(Step::Done)
            }
            Some(v) => {
                self.view = Some((v.id(), format!("{} copy", v.name()), kind_word(&v).to_owned()));
                Ok(Step::Continue)
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some((id, default, word)) = self.view.clone() else { return Ok(Step::Done) };
        let Some(name) = name_input(i, &default) else { return Ok(Step::Continue) };
        match duplicateview(s, &json!({ "viewId": id.0, "name": name })) {
            Ok(r) => {
                echo_created(s, &word, &name, &r);
            }
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

pub(crate) fn duplicateview_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(DuplicateView { view: None }))
}

/// UNPLACEVIEW on the edited view, no prompt.
struct UnplaceView;

impl Interactive for UnplaceView {
    fn name(&self) -> &'static str {
        "unplaceview"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Unplace view", Accept::TEXT)
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let Some(id) = edited_view(s) else {
            s.echo("Edit a view first (EDITVIEW).");
            return Ok(Step::Done);
        };
        let name = doc_of(s)?.views.iter().find(|v| v.id() == id).map(|v| v.name().to_owned()).unwrap_or_default();
        match unplaceview(s, &json!({ "viewId": id.0 })) {
            Ok(r) => {
                let n = r["droppedLinks"].as_u64().unwrap_or(0);
                s.echo(format!("{name} is now floating; {n} link{} dropped.", if n == 1 { "" } else { "s" }));
            }
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
    fn input(&mut self, _s: &mut Session, _i: Input) -> Result<Step> {
        Ok(Step::Done)
    }
}

pub(crate) fn unplaceview_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(UnplaceView))
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum LevelStep {
    Option,
    Name,
    Elevation,
    NewName,
    NewElevation,
}

/// LEVEL on the edited plan's level (a floating plan placed first): `Level option
/// [Name/Elevation/New] <Elevation>:`.
struct LevelMachine {
    level: Option<Level>,
    step: LevelStep,
    new_name: String,
}

impl LevelMachine {
    fn run(s: &mut Session, p: &Value) -> Result<Option<Value>> {
        match level(s, p) {
            Ok(r) => Ok(Some(r)),
            Err(e) => {
                s.echo(e.to_string());
                Ok(None)
            }
        }
    }
}

impl Interactive for LevelMachine {
    fn name(&self) -> &'static str {
        "level"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        let (name, floor) = self.level.as_ref().map_or((String::new(), 0.0), |l| (l.name.clone(), l.floor_elevation.value));
        match self.step {
            LevelStep::Option => Prompt::new("Level option", Accept::TEXT).kw(&["Name", "Elevation", "New"]).default("Elevation"),
            LevelStep::Name => Prompt::new("Enter level name", Accept::TEXT).default(name),
            LevelStep::Elevation | LevelStep::NewElevation => Prompt::new("Floor elevation", Accept::NUMBER).default(floor.to_string()),
            LevelStep::NewName => {
                let n = doc_of(s).map_or(0, |d| d.levels.len());
                Prompt::new("Enter level name", Accept::TEXT).default(format!("Level {}", n + 1))
            }
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        // A floating plan is placed first, in this command's undo step (LAMB T507).
        if let Some(id) = edited_view(s)
            && doc_of(s)?.views.iter().any(|v| v.id() == id && matches!(v, AnyView::Plan(PlanView { placement: None, .. })))
        {
            place_floating_plan(s, id)?;
        }
        let doc = doc_of(s)?;
        let level_id = edited_view(s).and_then(|id| match doc.views.iter().find(|v| v.id() == id) {
            Some(AnyView::Plan(PlanView { placement: Some(Placement::Plan { level_id, .. }), .. })) => Some(*level_id),
            _ => None,
        });
        self.level = level_id.and_then(|l| doc.levels.into_iter().find(|x| x.id == l));
        if self.level.is_none() {
            s.echo("Switch to a placed plan first");
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(level) = self.level.clone() else { return Ok(Step::Done) };
        let number = |i: Input| -> Result<Option<f64>> {
            match i {
                Input::Enter => Ok(Some(level.floor_elevation.value)),
                Input::Text(t) | Input::Keyword(t) => {
                    parse_distance(&t).filter(|v| v.is_finite()).map(Some).ok_or_else(|| other("Requires a number."))
                }
                _ => Ok(None),
            }
        };
        match self.step {
            LevelStep::Option => {
                let picked = match i {
                    Input::Enter => Some("Elevation".to_owned()),
                    Input::Text(t) | Input::Keyword(t) => self.prompt(s).match_keyword(&t),
                    _ => return Ok(Step::Continue),
                };
                self.step = match picked.as_deref() {
                    Some("Name") => LevelStep::Name,
                    Some("Elevation") => LevelStep::Elevation,
                    Some("New") => LevelStep::NewName,
                    _ => return Err(other("Invalid option keyword.")),
                };
                Ok(Step::Continue)
            }
            LevelStep::Name => {
                let Some(name) = name_input(i, &level.name) else { return Ok(Step::Continue) };
                if name != level.name && Self::run(s, &json!({ "mode": "rename", "levelId": level.id.0, "name": name }))?.is_some() {
                    s.echo(format!("Level renamed to \"{name}\"."));
                }
                Ok(Step::Done)
            }
            LevelStep::Elevation => {
                let Some(v) = number(i)? else { return Ok(Step::Continue) };
                if v != level.floor_elevation.value
                    && let Some(r) = Self::run(s, &json!({ "mode": "elevation", "levelId": level.id.0, "floorElevationMm": v }))?
                {
                    s.echo(format!("{} floor at {v}; {} plan(s) moved.", level.name, r["plans"]));
                }
                Ok(Step::Done)
            }
            LevelStep::NewName => {
                let default = self.prompt(s).default.unwrap_or_default();
                let Some(name) = name_input(i, &default) else { return Ok(Step::Continue) };
                self.new_name = name;
                self.step = LevelStep::NewElevation;
                Ok(Step::Continue)
            }
            LevelStep::NewElevation => {
                let Some(v) = number(i)? else { return Ok(Step::Continue) };
                let name = self.new_name.clone();
                if Self::run(s, &json!({ "mode": "create", "name": name, "floorElevationMm": v }))?.is_some() {
                    s.echo(format!("Level \"{name}\" created at {v}."));
                }
                Ok(Step::Done)
            }
        }
    }
}

pub(crate) fn level_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(LevelMachine { level: None, step: LevelStep::Option, new_name: String::new() }))
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum PvStep {
    Name,
    Kind,
    Level,
    Height,
    Base1,
    Base2,
    Target1,
    Target2,
    Cut1,
    Cut2,
    VBase,
    VElev,
}

/// PLACEVIEW interactively (LAMB T506, `view-creation-and-placement.md` §4): a floating view's
/// name, `Place as [Plan/Section/Elevation]`, then for a plan a level, a cut height and one or two
/// base/target point pairs (targets clicked in a placed plan's tile, or typed global XY after
/// `Global`), for a section/elevation a cut line clicked in a placed plan plus a base point and its
/// elevation. The machine computes only [`placeview`]'s parameters.
struct PlaceViewMachine {
    step: PvStep,
    default_name: Option<String>,
    view: Option<(Id, String)>,
    kind: &'static str,
    level: Option<(Id, f64)>,
    cut: f64,
    base: Vec<Point2>,
    target: Vec<Point2>,
    global: bool,
    host: Option<Id>,
    cut_pts: Vec<Point2>,
    vbase: Point2,
}

fn p2(v: Vec2) -> Point2 {
    Point2 { x: v.x, y: v.y }
}

/// A point input: a click, or typed `x,y`.
fn point_of(i: &Input) -> Option<Point2> {
    match i {
        Input::Point(p) => Some(p2(*p)),
        Input::Text(t) => parse_point(t, Vec2::new(0.0, 0.0)).map(p2),
        _ => None,
    }
}

/// The edited view when it is a placed plan, with its transform.
fn edited_placed_plan(s: &Session) -> Option<(Id, Transform2D)> {
    let id = edited_view(s)?;
    match doc_of(s).ok()?.views.into_iter().find(|v| v.id() == id)? {
        AnyView::Plan(PlanView { placement: Some(Placement::Plan { transform, .. }), .. }) => Some((id, transform)),
        _ => None,
    }
}

impl PlaceViewMachine {
    fn first_level(s: &Session) -> Option<Level> {
        doc_of(s).ok()?.levels.into_iter().min_by(|a, b| a.order.total_cmp(&b.order))
    }

    fn apply(&self, s: &mut Session, p: Value) -> Result<Step> {
        let name = self.view.as_ref().map(|v| v.1.clone()).unwrap_or_default();
        match placeview(s, &p) {
            Ok(_) => s.echo(format!("{name} placed")),
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }

    fn place_plan(&self, s: &mut Session) -> Result<Step> {
        let (Some((view, _)), Some((level, _)), Some(&b1), Some(&t1)) = (&self.view, self.level, self.base.first(), self.target.first()) else {
            return Ok(Step::Done);
        };
        let rotation = match (self.base.get(1), self.target.get(1)) {
            (Some(b2), Some(t2)) => (t2.y - t1.y).atan2(t2.x - t1.x) - (b2.y - b1.y).atan2(b2.x - b1.x),
            _ => 0.0,
        };
        let r = local_to_world2(&Transform2D { origin_x: 0.0, origin_y: 0.0, rotation }, b1);
        let t = json!({ "originX": t1.x - r.x, "originY": t1.y - r.y, "rotation": rotation });
        self.apply(s, json!({ "viewId": view.0, "kind": "plan", "levelId": level.0, "cutElevation": self.cut, "transform": t }))
    }

    /// A target point: a click in a placed plan's tile mapped to global XY, or (after `Global`)
    /// the point as typed. None re-prompts.
    fn target_of(&mut self, s: &mut Session, i: &Input) -> Option<Point2> {
        let p = point_of(i)?;
        if self.global {
            self.global = false;
            return Some(p);
        }
        match edited_placed_plan(s) {
            Some((_, t)) => Some(local_to_world2(&t, p)),
            None => {
                s.echo("Pick the target in a placed plan");
                None
            }
        }
    }
}

impl Interactive for PlaceViewMachine {
    fn name(&self) -> &'static str {
        "placeview"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        let with = |p: Prompt, d: Option<String>| match d {
            Some(d) => p.default(d),
            None => p,
        };
        match self.step {
            PvStep::Name => with(Prompt::new("Enter floating view name", Accept::TEXT), self.default_name.clone()),
            PvStep::Kind => Prompt::new("Place as", Accept::TEXT).kw(&["Plan", "Section", "Elevation"]).options_only().default(self.kind),
            PvStep::Level => with(Prompt::new("Enter level name", Accept::TEXT).kw(&["?"]), Self::first_level(s).map(|l| l.name)),
            PvStep::Height => {
                let h = doc_of(s).map_or(0.0, |d| d.defaults.default_cut_height_mm);
                Prompt::new("Specify cut height above the level", Accept::NUMBER).default(h.to_string())
            }
            PvStep::Base1 => Prompt::new("Specify base point", Accept::POINT),
            PvStep::Base2 => Prompt::new("Specify second base point", Accept::POINT).default("none"),
            PvStep::Target1 | PvStep::Target2 => {
                let second = if self.step == PvStep::Target2 { "second " } else { "" };
                if self.global {
                    Prompt::new(format!("Specify {second}global target point"), Accept::POINT)
                } else {
                    Prompt::new(format!("Specify {second}target point"), Accept::POINT).kw(&["Global"])
                }
            }
            PvStep::Cut1 => Prompt::new("Specify first point of cut line", Accept::POINT),
            PvStep::Cut2 => Prompt::new("Specify second point of cut line", Accept::POINT),
            PvStep::VBase => Prompt::new("Specify base point in the view", Accept::POINT).default("0,0"),
            PvStep::VElev => Prompt::new("Specify its elevation", Accept::NUMBER).default("0"),
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let id = edited_view(s);
        self.default_name = id.and_then(|id| {
            doc_of(s)
                .ok()?
                .views
                .into_iter()
                .find(|v| v.id() == id && v.placement().is_none() && !matches!(v, AnyView::Detail(_)))
                .map(|v| v.name().to_owned())
        });
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        match self.step {
            PvStep::Name => {
                let name = match (&i, &self.default_name) {
                    (Input::Enter, None) => None,
                    (Input::Text(t), None) if t.trim().is_empty() => None,
                    _ => name_input(i, self.default_name.as_deref().unwrap_or_default()),
                };
                let Some(name) = name else {
                    s.echo("Name a floating view");
                    return Ok(Step::Continue);
                };
                let doc = doc_of(s)?;
                let refusal = match doc.views.iter().find(|v| v.name() == name) {
                    None => Some(format!("No such view: {name}")),
                    Some(AnyView::Detail(_)) => Some("PLACEVIEW does not support detail views".to_owned()),
                    Some(v) if v.placement().is_some() => Some("View is already placed; use UNPLACEVIEW first".to_owned()),
                    Some(v) => {
                        self.kind = kind_word(v);
                        self.view = Some((v.id(), name.clone()));
                        None
                    }
                };
                match refusal {
                    Some(r) => s.echo(r),
                    None => self.step = PvStep::Kind,
                }
                Ok(Step::Continue)
            }
            PvStep::Kind => {
                let picked = match i {
                    Input::Enter => Some(self.kind.to_owned()),
                    Input::Text(t) | Input::Keyword(t) => self.prompt(s).match_keyword(&t),
                    _ => return Ok(Step::Continue),
                };
                match picked.as_deref() {
                    Some("Plan") => {
                        self.kind = "Plan";
                        if Self::first_level(s).is_none() {
                            s.echo("Create a level first (LEVEL)");
                            return Ok(Step::Done);
                        }
                        self.step = PvStep::Level;
                    }
                    Some("Section") => {
                        self.kind = "Section";
                        self.step = PvStep::Cut1;
                    }
                    Some("Elevation") => {
                        self.kind = "Elevation";
                        self.step = PvStep::Cut1;
                    }
                    _ => return Err(other("Invalid option keyword.")),
                }
                Ok(Step::Continue)
            }
            PvStep::Level => {
                let default = Self::first_level(s).map(|l| l.name).unwrap_or_default();
                let Some(name) = name_input(i, &default) else { return Ok(Step::Continue) };
                let levels = doc_of(s)?.levels;
                if name == "?" {
                    s.echo(levels.iter().map(|l| l.name.as_str()).collect::<Vec<_>>().join(", "));
                    return Ok(Step::Continue);
                }
                match levels.iter().find(|l| l.name == name) {
                    Some(l) => {
                        self.level = Some((l.id, l.floor_elevation.value));
                        self.step = PvStep::Height;
                    }
                    None => s.echo(format!("No such level: {name}")),
                }
                Ok(Step::Continue)
            }
            PvStep::Height => {
                let h = match i {
                    Input::Enter => doc_of(s)?.defaults.default_cut_height_mm,
                    Input::Text(t) | Input::Keyword(t) => parse_distance(&t).filter(|v| v.is_finite()).ok_or_else(|| other("Requires a number."))?,
                    _ => return Ok(Step::Continue),
                };
                self.cut = self.level.map_or(0.0, |l| l.1) + h;
                self.step = PvStep::Base1;
                Ok(Step::Continue)
            }
            PvStep::Base1 | PvStep::Base2 => {
                if self.step == PvStep::Base2 && i == Input::Enter {
                    self.step = PvStep::Target1;
                    return Ok(Step::Continue);
                }
                let Some(p) = point_of(&i) else { return Ok(Step::Continue) };
                let floating = self.view.as_ref().map(|v| v.0);
                if matches!(i, Input::Point(_)) && edited_view(s) != floating {
                    s.echo(format!("Pick the base point in {}", self.view.as_ref().map_or("", |v| v.1.as_str())));
                    return Ok(Step::Continue);
                }
                self.base.push(p);
                self.step = if self.step == PvStep::Base1 { PvStep::Base2 } else { PvStep::Target1 };
                Ok(Step::Continue)
            }
            PvStep::Target1 | PvStep::Target2 => {
                if let Input::Text(t) | Input::Keyword(t) = &i
                    && !self.global
                    && self.prompt(s).match_keyword(t).as_deref() == Some("Global")
                {
                    self.global = true;
                    return Ok(Step::Continue);
                }
                let Some(p) = self.target_of(s, &i) else { return Ok(Step::Continue) };
                self.target.push(p);
                if self.step == PvStep::Target1 && self.base.len() == 2 {
                    self.step = PvStep::Target2;
                    return Ok(Step::Continue);
                }
                self.place_plan(s)
            }
            PvStep::Cut1 | PvStep::Cut2 => {
                let Some(p) = point_of(&i) else { return Ok(Step::Continue) };
                let host = edited_placed_plan(s).map(|(id, _)| id);
                if host.is_none() || (self.step == PvStep::Cut2 && host != self.host) {
                    s.echo("Pick the cut line in a placed plan");
                    return Ok(Step::Continue);
                }
                if self.step == PvStep::Cut2 && self.cut_pts.first().is_some_and(|a| (a.x - p.x).hypot(a.y - p.y) <= 1e-9) {
                    s.echo("The cut line needs two different points");
                    return Ok(Step::Continue);
                }
                self.host = host;
                self.cut_pts.push(p);
                self.step = if self.step == PvStep::Cut1 { PvStep::Cut2 } else { PvStep::VBase };
                Ok(Step::Continue)
            }
            PvStep::VBase => {
                self.vbase = match i {
                    Input::Enter => Point2 { x: 0.0, y: 0.0 },
                    ref i => match point_of(i) {
                        Some(p) => p,
                        None => return Ok(Step::Continue),
                    },
                };
                self.step = PvStep::VElev;
                Ok(Step::Continue)
            }
            PvStep::VElev => {
                let elev = match i {
                    Input::Enter => 0.0,
                    Input::Text(t) | Input::Keyword(t) => parse_distance(&t).filter(|v| v.is_finite()).ok_or_else(|| other("Requires a number."))?,
                    _ => return Ok(Step::Continue),
                };
                let (Some((view, _)), Some(host), [a, b]) = (&self.view, self.host, self.cut_pts.as_slice()) else { return Ok(Step::Done) };
                let cut = json!({ "start": { "x": a.x, "y": a.y }, "segments": [{ "to": { "x": b.x, "y": b.y } }] });
                let t = json!({ "originX": -self.vbase.x, "originY": elev - self.vbase.y, "rotation": 0.0 });
                let intent = if self.kind == "Elevation" { "elevation" } else { "section" };
                self.apply(s, json!({ "viewId": view.0, "kind": "vertical", "cutLine": cut, "hostViewId": host.0, "transform": t, "intent": intent }))
            }
        }
    }
}

pub(crate) fn placeview_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(PlaceViewMachine {
        step: PvStep::Name,
        default_name: None,
        view: None,
        kind: "Plan",
        level: None,
        cut: 0.0,
        base: Vec::new(),
        target: Vec::new(),
        global: false,
        host: None,
        cut_pts: Vec::new(),
        vbase: Point2 { x: 0.0, y: 0.0 },
    }))
}

#[cfg(test)]
mod tests {
    //! Headless, as the other command modules': the registry is process-global, so every test
    //! installs first.
    use super::*;
    use crate::commands::{enter_space, install_app};
    use crate::import::project_to_drawing;
    use crate::project::DrawingProject;
    use crate::schema::{EntityData, read_entity_data, set_entity_data};
    use cadcraft_doc::LwPolyline;
    use cadcraft_geom::{PolyVertex, Vec2};
    use lamb_core::derivation::read::{ProjectRead, ViewKind};
    use lamb_core::geometry::polyline::{CornerEntry, VirtualCornerEntry};
    use lamb_core::model::{LineRole, Project, Provenance};
    use lamb_core::persistence::files_to_project;
    use std::collections::BTreeMap;
    use std::path::Path;

    type E<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    fn load(name: &str) -> E<Project> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb").join(format!("{name}.lamb.json"));
        let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        Ok(files_to_project(&files).map_err(|e| format!("{e:?}"))?)
    }

    fn view_named(s: &Session, name: &str) -> E<AnyView> {
        Ok(read_document(s.doc()?)?.views.into_iter().find(|v| v.name() == name).ok_or(format!("no view {name}"))?)
    }

    fn view_by_id(s: &Session, id: Id) -> E<AnyView> {
        Ok(read_document(s.doc()?)?.views.into_iter().find(|v| v.id() == id).ok_or("no view")?)
    }

    fn block_entities(s: &Session, id: Id) -> E<Vec<(Id, Common, EntityKind)>> {
        let d = s.doc()?;
        let b = d.block(&view_block_name(id)).ok_or("no block")?;
        Ok(b.entities.iter().map(|e| (Id(e.handle.0), e.common.clone(), e.kind.clone())).collect())
    }

    fn undo_len(s: &Session) -> E<usize> {
        Ok(s.state()?.undo.len())
    }

    /// The imported u-wall-two-sections (plan `P`, sections `Legs` — an AI line, linked — and
    /// `Crossbar`), with a filleted closed polyline added to `Legs`.
    fn u_wall() -> E<Session> {
        install_app().map_err(|e| e.to_string())?;
        let (mut d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
        let legs = read_document(&d)?.views.iter().find(|v| v.name() == "Legs").ok_or("no Legs")?.id();
        let pts = [(0.0, 0.0), (2000.0, 0.0), (2000.0, 1000.0), (0.0, 1000.0)];
        let vertices = pts.iter().map(|&(x, y)| PolyVertex { p: Vec2::new(x, y), bulge: 0.0, start_width: 0.0, end_width: 0.0 }).collect();
        let kind = EntityKind::LwPolyline(LwPolyline { vertices, closed: true, const_width: 0.0, elevation: 0.0, plinegen: false });
        let mut common = Common { layer: "0".into(), ..Common::default() };
        let data = EntityData {
            role: LineRole::Cut,
            corners: vec![CornerEntry { corner_index: 2, entry: VirtualCornerEntry::Fillet { radius: 300.0 } }],
            ..serde_json::from_value(json!({}))?
        };
        set_entity_data(&mut common, &data)?;
        d.add(&Space::Block(view_block_name(legs)), common, kind)?;
        let mut s = Session::new();
        s.open_drawing(d, "u-wall", None);
        Ok(s)
    }

    #[test]
    fn duplicateview_copies_entities_verbatim_into_an_unplaced_view_and_undoes() -> E<()> {
        let mut s = u_wall()?;
        let legs = view_named(&s, "Legs")?;
        let src = block_entities(&s, legs.id())?;
        assert!(src.iter().any(|(_, c, _)| read_entity_data(c).is_ok_and(|d| d.ai_origin.is_some())), "an AI line");
        assert!(src.iter().any(|(_, c, _)| read_entity_data(c).is_ok_and(|d| !d.corners.is_empty())), "a filleted polyline");
        let links = read_document(s.doc()?)?.links;
        let views = read_document(s.doc()?)?.views.len();

        let r = s.execute("duplicateview", &json!({ "viewId": legs.id().0 }))?;
        let copy = Id(r["viewId"].as_u64().ok_or("no viewId")?);
        assert_eq!(r["entities"], json!(src.len()));
        let AnyView::Vertical(v) = view_by_id(&s, copy)? else { return Err("not vertical".into()) };
        let AnyView::Vertical(lv) = &legs else { return Err("Legs not vertical".into()) };
        assert_eq!((v.name.as_str(), v.intent, v.placement.is_none()), ("Legs copy", lv.intent, true));
        let dup = block_entities(&s, copy)?;
        assert_eq!(dup.len(), src.len());
        for ((sid, sc, sk), (did, dc, dk)) in src.iter().zip(&dup) {
            assert_ne!(sid, did, "fresh handles");
            assert_eq!((sc, sk), (dc, dk), "Common and geometry cloned");
            assert_eq!(read_entity_data(sc)?, read_entity_data(dc)?, "EntityData byte-equal");
        }
        assert_eq!(read_document(s.doc()?)?.links, links, "no links copied");

        s.undo()?;
        assert_eq!(read_document(s.doc()?)?.views.len(), views);
        assert!(s.doc()?.block(&view_block_name(copy)).is_none(), "block gone");
        assert!(!s.doc()?.model.iter().any(|e| e.handle.0 == copy.0), "insert gone");
        Ok(())
    }

    #[test]
    fn duplicateview_refuses_a_detail_and_unknown_view_with_no_undo_step() -> E<()> {
        let mut s = u_wall()?;
        let detail = Id(s.execute("newdetail", &json!({ "name": "D1" }))?["viewId"].as_u64().ok_or("no id")?);
        let n = undo_len(&s)?;
        let e = s.execute("duplicateview", &json!({ "viewId": detail.0 })).err().ok_or("accepted")?;
        assert!(e.to_string().contains("DUPLICATEVIEW does not support detail views"), "{e}");
        let e = s.execute("duplicateview", &json!({ "viewId": 999_999 })).err().ok_or("accepted")?;
        assert!(e.to_string().contains("No such view"), "{e}");
        assert_eq!(undo_len(&s)?, n);
        Ok(())
    }

    #[test]
    fn placeview_converts_a_floating_view_and_refuses_a_placed_one() -> E<()> {
        let mut s = u_wall()?;
        let legs = view_named(&s, "Legs")?.id();
        let plan = view_named(&s, "P")?.id();
        let copy = Id(s.execute("duplicateview", &json!({ "viewId": legs.0 }))?["viewId"].as_u64().ok_or("no id")?);
        let level = Id(s.execute("level", &json!({ "mode": "create", "name": "L1", "floorElevationMm": 0 }))?["levelId"].as_u64().ok_or("no id")?);
        let t = json!({ "originX": 100.0, "originY": 200.0, "rotation": 0.5 });
        s.execute("placeview", &json!({ "viewId": copy.0, "kind": "plan", "levelId": level.0, "cutElevation": 1200, "transform": t }))?;
        {
            let proj = DrawingProject::new(s.doc()?)?;
            let v = proj.view(copy).ok_or("no view")?;
            assert_eq!(v.kind, ViewKind::Plan);
            assert_eq!(
                v.placement,
                Some(&Placement::Plan {
                    level_id: level,
                    cut_elevation: 1200.0,
                    transform: Transform2D { origin_x: 100.0, origin_y: 200.0, rotation: 0.5 }
                })
            );
        }
        assert_eq!(block_entities(&s, copy)?.len(), block_entities(&s, legs)?.len(), "no entity touched");

        let loose = Id(s.execute("newplan", &json!({ "name": "Loose" }))?["viewId"].as_u64().ok_or("no id")?);
        let cut = json!({ "start": {"x": 0, "y": 0}, "segments": [{ "to": {"x": 5000, "y": 0} }] });
        s.execute(
            "placeview",
            &json!({ "viewId": loose.0, "kind": "vertical", "cutLine": cut, "hostViewId": plan.0, "transform": t, "intent": "elevation" }),
        )?;
        let AnyView::Vertical(v) = view_by_id(&s, loose)? else { return Err("not vertical".into()) };
        assert_eq!((v.id, v.name.as_str(), v.intent), (loose, "Loose", VerticalIntent::Elevation));
        let Some(Placement::Vertical { host_view_id, cut_line, flipped, .. }) = &v.placement else { return Err("not placed".into()) };
        assert_eq!((*host_view_id, cut_line.closed, cut_line.segments[0].bulge, *flipped), (plan, false, 0.0, None));
        assert_eq!(DrawingProject::new(s.doc()?)?.view(loose).ok_or("no view")?.kind, ViewKind::Vertical);

        let n = undo_len(&s)?;
        let e = s
            .execute("placeview", &json!({ "viewId": loose.0, "kind": "plan", "levelId": level.0, "cutElevation": 0, "transform": t }))
            .err()
            .ok_or("accepted")?;
        assert!(e.to_string().contains("View is already placed; use UNPLACEVIEW first"), "{e}");
        assert_eq!(undo_len(&s)?, n, "a refusal records no undo step");

        s.undo()?;
        assert!(matches!(view_by_id(&s, loose)?, AnyView::Plan(PlanView { placement: None, .. })), "undo converts back");
        Ok(())
    }

    #[test]
    fn unplaceview_drops_exactly_its_links_and_undo_restores_them() -> E<()> {
        let mut s = u_wall()?;
        let legs = view_named(&s, "Legs")?.id();
        let ids: Vec<Id> = block_entities(&s, legs)?.into_iter().map(|(id, ..)| id).collect();
        let before = read_document(s.doc()?)?.links;
        let touching = before.iter().filter(|l| ids.contains(&l.a.entity_id) || ids.contains(&l.b.entity_id)).count();
        assert!(touching > 0 && touching < before.len(), "fixture: some links on Legs, some not");
        let r = s.execute("unplaceview", &json!({ "viewId": legs.0 }))?;
        assert_eq!(r["droppedLinks"], json!(touching));
        let after = read_document(s.doc()?)?.links;
        assert_eq!(after.len(), before.len() - touching);
        assert!(after.iter().all(|l| !ids.contains(&l.a.entity_id) && !ids.contains(&l.b.entity_id)));
        assert!(view_by_id(&s, legs)?.placement().is_none());
        assert_eq!(block_entities(&s, legs)?.len(), ids.len(), "linework kept");

        let n = undo_len(&s)?;
        let e = s.execute("unplaceview", &json!({ "viewId": legs.0 })).err().ok_or("accepted")?;
        assert!(e.to_string().contains("View is not placed"), "{e}");
        assert_eq!(undo_len(&s)?, n);

        s.undo()?;
        assert_eq!(read_document(s.doc()?)?.links, before);
        assert!(view_by_id(&s, legs)?.placement().is_some());
        Ok(())
    }

    /// LAMBPROJECT (`Level 1` at 0, `Ground Floor` at the default cut, edited) plus `RCP`, a
    /// second plan on Level 1 at 2400.
    fn two_plans() -> E<(Session, Id, Id, Id)> {
        install_app().map_err(|e| e.to_string())?;
        let mut s = Session::new();
        s.new_drawing(true);
        let gf = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no id")?);
        let level = read_document(s.doc()?)?.levels.first().ok_or("no level")?.id;
        let rcp = Id(s.execute("newplan", &json!({ "name": "RCP" }))?["viewId"].as_u64().ok_or("no id")?);
        let t = json!({ "originX": 0, "originY": 0, "rotation": 0 });
        s.execute("placeview", &json!({ "viewId": rcp.0, "kind": "plan", "levelId": level.0, "cutElevation": 2400, "transform": t }))?;
        s.execute("editview", &json!({ "viewId": gf.0 }))?;
        Ok((s, level, gf, rcp))
    }

    fn cut_of(s: &Session, id: Id) -> E<f64> {
        match view_by_id(s, id)? {
            AnyView::Plan(PlanView { placement: Some(Placement::Plan { cut_elevation, .. }), .. }) => Ok(cut_elevation),
            _ => Err("not a placed plan".into()),
        }
    }

    fn level_of(s: &Session, id: Id) -> E<Level> {
        Ok(read_document(s.doc()?)?.levels.into_iter().find(|l| l.id == id).ok_or("no level")?)
    }

    #[test]
    fn level_elevation_moves_every_plan_on_it_keeping_each_height() -> E<()> {
        let (mut s, level, gf, rcp) = two_plans()?;
        let (gf_cut, rcp_cut) = (cut_of(&s, gf)?, cut_of(&s, rcp)?);
        assert_ne!(gf_cut, rcp_cut);
        let r = s.execute("level", &json!({ "mode": "elevation", "levelId": level.0, "floorElevationMm": 500 }))?;
        assert_eq!(r["plans"], json!(2));
        assert_eq!((cut_of(&s, gf)?, cut_of(&s, rcp)?), (gf_cut + 500.0, rcp_cut + 500.0));
        let l = level_of(&s, level)?;
        assert_eq!(l.floor_elevation.value, 500.0);
        assert!(matches!(&l.floor_elevation.provenance, Provenance::Authored { view_id, .. } if *view_id == gf), "authored against the first plan");
        s.undo()?;
        assert_eq!((cut_of(&s, gf)?, cut_of(&s, rcp)?, level_of(&s, level)?.floor_elevation.value), (gf_cut, rcp_cut, 0.0));
        Ok(())
    }

    #[test]
    fn level_rename_create_and_unknown_level() -> E<()> {
        let (mut s, level, ..) = two_plans()?;
        s.execute("level", &json!({ "mode": "rename", "levelId": level.0, "name": "Ground" }))?;
        assert_eq!(level_of(&s, level)?.name, "Ground");
        let r = s.execute("level", &json!({ "mode": "create", "name": "Roof", "floorElevationMm": 6000 }))?;
        let roof = level_of(&s, Id(r["levelId"].as_u64().ok_or("no id")?))?;
        assert_eq!((roof.name.as_str(), roof.order, roof.floor_elevation.value), ("Roof", 1.0, 6000.0));
        assert!(matches!(&roof.floor_elevation.provenance, Provenance::Authored { view_id, .. } if *view_id == Id(0)));
        let n = undo_len(&s)?;
        let e = s.execute("level", &json!({ "mode": "elevation", "levelId": 999_999, "floorElevationMm": 1 })).err().ok_or("accepted")?;
        assert!(e.to_string().contains("No such level: 999999"), "{e}");
        let e = s.execute("level", &json!({ "mode": "rename", "levelId": "424242", "name": "x" })).err().ok_or("accepted")?;
        assert!(e.to_string().contains("No such level: 424242"), "{e}");
        assert_eq!(undo_len(&s)?, n);
        Ok(())
    }

    #[test]
    fn machines_duplicate_unplace_and_level_on_the_edited_view() -> E<()> {
        let (mut s, level, gf, rcp) = two_plans()?;
        s.start("level")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Level option or [Name/Elevation/New] <Elevation>:"));
        s.input(Input::Enter)?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Floor elevation <0>:"));
        s.input(Input::Text("300".into()))?;
        assert!(s.running.is_none());
        assert_eq!(level_of(&s, level)?.floor_elevation.value, 300.0);
        assert_eq!(cut_of(&s, rcp)?, 2700.0);

        s.start("level")?;
        s.input(Input::Text("n".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter level name <Level 1>:"));
        s.input(Input::Text("Ground".into()))?;
        assert_eq!(level_of(&s, level)?.name, "Ground");

        s.start("level")?;
        s.input(Input::Keyword("New".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter level name <Level 2>:"));
        s.input(Input::Enter)?;
        s.input(Input::Text("3000".into()))?;
        assert!(read_document(s.doc()?)?.levels.iter().any(|l| l.name == "Level 2" && l.floor_elevation.value == 3000.0));

        s.start("duplicateview")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter name for the copy <Ground Floor copy>:"));
        s.input(Input::Enter)?;
        let copy = view_named(&s, "Ground Floor copy")?;
        assert!(matches!(copy, AnyView::Plan(PlanView { placement: None, .. })));
        assert!(s.log.iter().any(|l| l == &format!("Plan \"Ground Floor copy\" created (view {}).", copy.id().0)), "{:?}", s.log);

        s.start("unplaceview")?;
        assert!(s.running.is_none());
        assert!(view_by_id(&s, gf)?.placement().is_none());
        assert!(s.log.iter().any(|l| l == "Ground Floor is now floating; 0 links dropped."), "{:?}", s.log);
        // LEVEL in a floating plan places it on the first level by order, then prompts (T507).
        let steps = undo_len(&s)?;
        s.start("level")?;
        let cut = read_document(s.doc()?)?.defaults.default_cut_height_mm;
        assert_eq!(
            view_by_id(&s, gf)?.placement(),
            Some(&Placement::Plan { level_id: level, cut_elevation: 300.0 + cut, transform: lamb_core::geometry::IDENTITY_TRANSFORM })
        );
        assert!(s.log.iter().any(|l| l == "Ground Floor placed on Ground"), "{:?}", s.log);
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Level option"));
        s.input(Input::Text("n".into()))?;
        s.input(Input::Text("Ground".into()))?;
        assert_eq!(undo_len(&s)?, steps + 1, "placing and the command are one step");
        s.undo()?;
        assert!(view_by_id(&s, gf)?.placement().is_none(), "one undo floats it again");

        s.execute("editview", &json!({}))?;
        s.start("duplicateview")?;
        assert!(s.running.is_none() && s.log.iter().any(|l| l == "Edit a view first (EDITVIEW)."));
        Ok(())
    }

    /// LAMBPROJECT, a placed plan `P` at transform (5000, 2000, 0) on Level 1, and a floating plan
    /// `F` holding a polyline (0,0)→(1000,300), edited.
    fn floating_and_placed() -> E<(Session, Id, Id)> {
        install_app().map_err(|e| e.to_string())?;
        let mut s = Session::new();
        s.new_drawing(true);
        s.execute("lambproject", &json!({}))?;
        let level = read_document(s.doc()?)?.levels.first().ok_or("no level")?.id;
        let p = Id(s.execute("newplan", &json!({ "name": "P" }))?["viewId"].as_u64().ok_or("no id")?);
        let t = json!({ "originX": 5000, "originY": 2000, "rotation": 0 });
        s.execute("placeview", &json!({ "viewId": p.0, "kind": "plan", "levelId": level.0, "cutElevation": 1200, "transform": t }))?;
        let f = Id(s.execute("newplan", &json!({ "name": "F" }))?["viewId"].as_u64().ok_or("no id")?);
        let vertices = [(0.0, 0.0), (1000.0, 300.0)]
            .iter()
            .map(|&(x, y)| PolyVertex { p: Vec2::new(x, y), bulge: 0.0, start_width: 0.0, end_width: 0.0 })
            .collect();
        let kind = EntityKind::LwPolyline(LwPolyline { vertices, closed: false, const_width: 0.0, elevation: 0.0, plinegen: false });
        s.doc_mut()?.add(&Space::Block(view_block_name(f)), Common { layer: "0".into(), ..Common::default() }, kind)?;
        s.execute("editview", &json!({ "viewId": f.0 }))?;
        Ok((s, p, f))
    }

    fn shown(s: &Session) -> Option<String> {
        s.current_prompt().map(|p| p.display())
    }

    fn pt(x: f64, y: f64) -> Input {
        Input::Point(Vec2::new(x, y))
    }

    fn plan_transform(s: &Session, id: Id) -> E<Transform2D> {
        match view_by_id(s, id)? {
            AnyView::Plan(PlanView { placement: Some(Placement::Plan { transform, .. }), .. }) => Ok(transform),
            _ => Err("not a placed plan".into()),
        }
    }

    #[test]
    fn placeview_machine_aligns_two_point_pairs_with_the_target_clicked_in_a_placed_plan() -> E<()> {
        let (mut s, p, f) = floating_and_placed()?;
        let steps = undo_len(&s)?;
        s.start("pv")?;
        assert_eq!(shown(&s).as_deref(), Some("Enter floating view name <F>:"));
        s.input(Input::Enter)?;
        assert_eq!(shown(&s).as_deref(), Some("Place as [Plan/Section/Elevation] <Plan>:"));
        s.input(Input::Enter)?;
        assert_eq!(shown(&s).as_deref(), Some("Enter level name or [?] <Level 1>:"));
        s.input(Input::Text("?".into()))?;
        assert!(s.log.iter().any(|l| l == "Level 1"), "{:?}", s.log);
        s.input(Input::Enter)?;
        let height = read_document(s.doc()?)?.defaults.default_cut_height_mm;
        assert_eq!(shown(&s), Some(format!("Specify cut height above the level <{height}>:")));
        s.input(Input::Enter)?;
        s.input(pt(0.0, 0.0))?;
        assert_eq!(shown(&s).as_deref(), Some("Specify second base point <none>:"));
        s.input(pt(1000.0, 0.0))?;
        assert_eq!(shown(&s).as_deref(), Some("Specify target point or [Global]:"));
        enter_space(&mut s, Space::Block(view_block_name(p)))?;
        s.input(pt(100.0, 200.0))?;
        assert_eq!(shown(&s).as_deref(), Some("Specify second target point or [Global]:"));
        s.input(pt(100.0, 1200.0))?;
        assert!(s.running.is_none());
        assert!(s.log.iter().any(|l| l == "F placed"), "{:?}", s.log);

        let t = plan_transform(&s, f)?;
        assert!((t.rotation - std::f64::consts::FRAC_PI_2).abs() < 1e-9, "{t:?}");
        let (_, _, kind) = block_entities(&s, f)?.into_iter().next().ok_or("no entity")?;
        let EntityKind::LwPolyline(pl) = kind else { return Err("not a polyline".into()) };
        let (a, b) = (pl.vertices[0].p, pl.vertices[1].p);
        let ga = local_to_world2(&t, Point2 { x: a.x, y: a.y });
        let gb = local_to_world2(&t, Point2 { x: b.x, y: b.y });
        assert!((ga.x - 5100.0).abs() < 1e-6 && (ga.y - 2200.0).abs() < 1e-6, "{ga:?}");
        assert!((gb.x - 4800.0).abs() < 1e-6 && (gb.y - 3200.0).abs() < 1e-6, "{gb:?}");
        let level = read_document(s.doc()?)?.levels[0].clone();
        assert!(matches!(view_by_id(&s, f)?, AnyView::Plan(PlanView { placement: Some(Placement::Plan { level_id, cut_elevation, .. }), .. })
            if level_id == level.id && cut_elevation == level.floor_elevation.value + height));
        assert_eq!(undo_len(&s)?, steps + 1, "one undo step");
        s.undo()?;
        assert!(view_by_id(&s, f)?.placement().is_none(), "undo floats it again");
        Ok(())
    }

    #[test]
    fn placeview_machine_global_keyword_translates_only() -> E<()> {
        let (mut s, _, f) = floating_and_placed()?;
        s.start("placeview")?;
        for i in [Input::Enter, Input::Enter, Input::Enter, Input::Text("1000".into())] {
            s.input(i)?;
        }
        s.input(pt(50.0, -20.0))?;
        s.input(Input::Enter)?;
        s.input(Input::Text("g".into()))?;
        assert_eq!(shown(&s).as_deref(), Some("Specify global target point:"));
        s.input(pt(7000.0, 3000.0))?;
        assert!(s.running.is_none());
        assert_eq!(plan_transform(&s, f)?, Transform2D { origin_x: 6950.0, origin_y: 3020.0, rotation: 0.0 });
        assert!(
            matches!(view_by_id(&s, f)?, AnyView::Plan(PlanView { placement: Some(Placement::Plan { cut_elevation, .. }), .. }) if cut_elevation == 1000.0)
        );
        Ok(())
    }

    #[test]
    fn placeview_machine_places_a_floating_plan_as_an_elevation_on_a_cut_line() -> E<()> {
        let (mut s, p, f) = floating_and_placed()?;
        s.start("placeview")?;
        s.input(Input::Enter)?;
        s.input(Input::Text("e".into()))?;
        assert_eq!(shown(&s).as_deref(), Some("Specify first point of cut line:"));
        s.input(pt(0.0, 0.0))?;
        assert!(s.log.iter().any(|l| l == "Pick the cut line in a placed plan"), "a click in F re-prompts");
        enter_space(&mut s, Space::Block(view_block_name(p)))?;
        s.input(pt(0.0, 0.0))?;
        s.input(pt(4000.0, 0.0))?;
        assert_eq!(shown(&s).as_deref(), Some("Specify base point in the view <0,0>:"));
        s.input(Input::Text("0,0".into()))?;
        assert_eq!(shown(&s).as_deref(), Some("Specify its elevation <0>:"));
        s.input(Input::Text("3000".into()))?;
        assert!(s.running.is_none() && s.log.iter().any(|l| l == "F placed"), "{:?}", s.log);
        let AnyView::Vertical(v) = view_by_id(&s, f)? else { return Err("not vertical".into()) };
        assert_eq!(v.intent, VerticalIntent::Elevation);
        let Some(Placement::Vertical { host_view_id, cut_line, .. }) = &v.placement else { return Err("not placed".into()) };
        assert_eq!((*host_view_id, cut_line.start.x, cut_line.segments[0].to.x), (p, 0.0, 4000.0));
        let proj = DrawingProject::new(s.doc()?)?;
        let uz = lamb_core::derivation::projection::vertical_local_to_uz(&proj.view(f).ok_or("no view")?, Point2 { x: 0.0, y: 0.0 });
        assert_eq!((uz.u, uz.z), (0.0, 3000.0));
        Ok(())
    }

    #[test]
    fn placeview_machine_reprompts_a_target_outside_a_placed_plan_and_esc_records_nothing() -> E<()> {
        let (mut s, _, f) = floating_and_placed()?;
        let steps = undo_len(&s)?;
        s.execute("editview", &json!({}))?;
        s.start("placeview")?;
        assert_eq!(shown(&s).as_deref(), Some("Enter floating view name:"));
        s.input(Input::Enter)?;
        assert!(s.log.iter().any(|l| l == "Name a floating view"));
        s.input(Input::Text("P".into()))?;
        assert!(s.log.iter().any(|l| l == "View is already placed; use UNPLACEVIEW first"), "{:?}", s.log);
        s.input(Input::Text("F".into()))?;
        s.execute("editview", &json!({ "viewId": f.0 }))?;
        for i in [Input::Enter, Input::Enter, Input::Enter] {
            s.input(i)?;
        }
        s.input(pt(0.0, 0.0))?;
        s.input(Input::Enter)?;
        s.input(pt(10.0, 10.0))?;
        assert!(s.log.iter().any(|l| l == "Pick the target in a placed plan"), "{:?}", s.log);
        assert_eq!(shown(&s).as_deref(), Some("Specify target point or [Global]:"));
        s.input(Input::Cancel)?;
        assert!(s.running.is_none());
        assert!(view_by_id(&s, f)?.placement().is_none());
        assert_eq!(undo_len(&s)?, steps, "Esc records nothing");
        Ok(())
    }
}

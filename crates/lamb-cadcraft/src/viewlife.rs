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
//!   `{viewId, kind:"vertical", cutLine, hostViewId, transform, intent?}` (parameter-only, as the
//!   TS app has no button for it): placing a floating view as the other kind converts it.
//! - `unplaceview {viewId?}`: back to floating, dropping every link with an end on one of the
//!   view's entities. Returns `{droppedLinks}`.
//! - `level {mode:"create", name, floorElevationMm, order?}` | `{mode:"rename", levelId, name}` |
//!   `{mode:"elevation", levelId, floorElevationMm}`: moving a level moves every placed plan on it,
//!   each keeping its own cut height above the floor.
//!
//! Each is one undo step (the drawing snapshot is the inverse); every refusal is decided on a read
//! before `doc_mut`, so it records no step.

use cadcraft_doc::{Common, EntityKind, Space};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step, units::parse_distance};
use lamb_core::geometry::Transform2D;
use lamb_core::geometry::polyline::Polyline2;
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

/// LEVEL on the edited placed plan's level: `Level option [Name/Elevation/New] <Elevation>:`.
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

#[cfg(test)]
mod tests {
    //! Headless, as the other command modules': the registry is process-global, so every test
    //! installs first.
    use super::*;
    use crate::commands::install_app;
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
        s.start("level")?;
        assert!(s.log.iter().any(|l| l == "Switch to a placed plan first"), "a floating plan has no level");

        s.execute("editview", &json!({}))?;
        s.start("duplicateview")?;
        assert!(s.running.is_none() && s.log.iter().any(|l| l == "Edit a view first (EDITVIEW)."));
        Ok(())
    }
}

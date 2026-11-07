//! ROOM (LAMB T656): names a plan's space, and the tag each named room shows on its plan.
//!
//! A room is stored, not drawn: a [`RoomRecord`] on the document — its plan, a plan-local point,
//! name and number. Its space is the smallest space of the plan's wall graph
//! ([`lamb_core::derivation::wall_spaces`]) containing the point, recomputed whenever asked, so the
//! area follows the walls. The area is measured **inside the walls' inner faces**
//! ([`inner_area_mm2`]), not along the centrelines.
//!
//! `room {viewId?, at, name, number?}` (`rm`): `viewId` defaults to the edited plan, `at` is
//! plan-local. A placed plan only, and `at` must be inside a closed space. A point in a space that
//! already has a room edits that room (its `id` and `at` stay); an empty `name` removes it. A new
//! room's `number` defaults to the plan's level ordinal (1-based, levels by floor elevation) and the
//! lowest unused two digits on that level: `101`, `102`, …, `201` on the second level. One undo
//! step. Returns `{id, name, number, area_m2, message}` (`id`, `area_m2` null once removed).
//!
//! [`crate::annotations`] writes each [`room_tags`] entry into its plan's annotation block: name,
//! number and area (`18.2 m²`), centred on `at`; a room whose point is no longer in a space shows
//! `—` for its area and stays.
//!
//! `roomschedule {layout?, at?}` (`rsch`, LAMB T657): the `Room Schedule` table (`Number | Name |
//! Level | Area (m²)`) on layout `layout` (default `A603`, its sheet in the set ([`crate::sheetset`]),
//! created without viewports when absent) at paper point `at`, placed and replaced as
//! [`crate::schedule`]'s opening schedule is. Rows by level (floor elevation, then order; an unplaced
//! plan's rooms last, level `—`), then number (numerically where both parse, else as text), then a
//! `Total` row summing the known areas. One undo step; no rooms is refused, nothing changed. Returns
//! `{layout, handle, rows: [{number, name, level, area_m2}], message}`.

use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use lamb_core::derivation::wall_spaces::{WallSides, inner_area_mm2, space_contains};
use lamb_core::geometry::Point2;
use lamb_core::model::{AnyView, Id, Placement};
use serde_json::{Value, json};

use cadcraft_doc::Table;

use crate::bom::{self, Refreshed};
use crate::commands::{bad, edited_view, id_value, name_input, other, point_param};
use crate::detailtags::graph_and_sides;
use crate::project::DrawingProject;
use crate::schedule::write_schedule;
use crate::schema::{LambDocument, RoomRecord, read_document, write_document};
use cadcraft_doc::Drawing;

/// A room's tag on its plan, in plan-local coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct RoomTag {
    pub plan: Id,
    pub at: Point2,
    pub name: String,
    pub number: String,
    /// Inside the inner faces, m²; `None` when `at` is in no space.
    pub area_m2: Option<f64>,
}

/// The tag's area line: `18.2 m²`, or `—`.
pub fn area_text(area_m2: Option<f64>) -> String {
    area_m2.map_or_else(|| "—".to_owned(), |a| format!("{a:.1} m²"))
}

/// The index of the smallest space containing `at`.
fn space_at(sides: &WallSides, at: Point2) -> Option<usize> {
    (0..sides.spaces.len())
        .filter(|&i| space_contains(&sides.spaces[i], at))
        .min_by(|&a, &b| sides.spaces[a].area_mm2.total_cmp(&sides.spaces[b].area_mm2))
}

/// Every room's tag, in document order. An unreadable drawing has none.
pub fn room_tags(d: &Drawing) -> Vec<RoomTag> {
    let Ok(doc) = read_document(d) else { return Vec::new() };
    if doc.rooms.is_empty() {
        return Vec::new();
    }
    let Ok(p) = DrawingProject::new(d) else { return Vec::new() };
    let mut sides_of: Vec<(Id, WallSides)> = Vec::new();
    doc.rooms
        .iter()
        .map(|r| {
            if !sides_of.iter().any(|(v, _)| *v == r.plan) {
                sides_of.push((r.plan, graph_and_sides(&p, r.plan).1));
            }
            let sides = sides_of.iter().find(|(v, _)| *v == r.plan).map(|(_, s)| s);
            let area_m2 = sides.and_then(|s| space_at(s, r.at).map(|i| inner_area_mm2(&s.spaces[i]) / 1e6));
            RoomTag { plan: r.plan, at: r.at, name: r.name.clone(), number: r.number.clone(), area_m2 }
        })
        .collect()
}

/// The 1-based ordinal of `plan`'s level, levels by floor elevation (then order); or why not.
fn level_ordinal(doc: &LambDocument, plan: Id) -> std::result::Result<(usize, Id), String> {
    match doc.views.iter().find(|v| v.id() == plan) {
        None => Err("No such view".into()),
        Some(AnyView::Plan(pv)) => {
            let Some(Placement::Plan { level_id, .. }) = &pv.placement else {
                return Err(format!("{} is floating: place it on a level before naming its rooms", pv.name));
            };
            let mut levels: Vec<_> = doc.levels.iter().collect();
            levels.sort_by(|a, b| a.floor_elevation.value.total_cmp(&b.floor_elevation.value).then(a.order.total_cmp(&b.order)));
            let i = levels.iter().position(|l| l.id == *level_id).ok_or("The plan's level is gone")?;
            Ok((i + 1, *level_id))
        }
        Some(_) => Err("Rooms are named in plan views: open a plan with EDITVIEW".into()),
    }
}

/// The plan's level id, when it is a placed plan.
fn level_of(doc: &LambDocument, plan: Id) -> Option<Id> {
    doc.views.iter().find_map(|v| match v {
        AnyView::Plan(pv) if pv.id == plan => match &pv.placement {
            Some(Placement::Plan { level_id, .. }) => Some(*level_id),
            _ => None,
        },
        _ => None,
    })
}

/// The lowest unused `<ordinal>01..99` among the rooms on `level`.
fn next_number(doc: &LambDocument, ordinal: usize, level: Id) -> String {
    let used: Vec<&str> = doc.rooms.iter().filter(|r| level_of(doc, r.plan) == Some(level)).map(|r| r.number.as_str()).collect();
    (1..=99).map(|n| format!("{ordinal}{n:02}")).find(|n| !used.contains(&n.as_str())).unwrap_or_else(|| format!("{ordinal}{}", used.len() + 1))
}

/// What a pick at `at` in `plan` would do: the room already there (its index), and the default
/// number for a new one. Refused for a non-plan, an unplaced plan or a point in no space.
fn resolve(d: &Drawing, plan: Id, at: Point2) -> std::result::Result<(Option<usize>, String, f64), String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    let (ordinal, level) = level_ordinal(&doc, plan)?;
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let sides = graph_and_sides(&p, plan).1;
    let space = space_at(&sides, at).ok_or("Not inside a closed room: walls must enclose it")?;
    let existing = doc.rooms.iter().position(|r| r.plan == plan && space_at(&sides, r.at) == Some(space));
    Ok((existing, next_number(&doc, ordinal, level), inner_area_mm2(&sides.spaces[space]) / 1e6))
}

fn text_param(p: &Value, key: &str) -> Result<Option<String>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(t)) => Ok(Some(t.trim().to_owned())),
        Some(_) => Err(bad("room", &format!("`{key}` must be a string"))),
    }
}

/// ROOM `{viewId?, at, name, number?}`. See the module doc.
pub(crate) fn room(s: &mut Session, p: &Value) -> Result<Value> {
    let plan = match p.get("viewId") {
        None | Some(Value::Null) => edited_view(s).ok_or_else(|| other("Edit a plan first (EDITVIEW), or give `viewId`"))?,
        Some(v) => id_value(v).ok_or_else(|| bad("room", "`viewId` must be a view id"))?,
    };
    let at = point_param("room", p, "at")?;
    let name = text_param(p, "name")?.ok_or_else(|| bad("room", "`name` is required"))?;
    let number = text_param(p, "number")?.filter(|n| !n.is_empty());
    let (existing, default_number, area) = resolve(s.doc()?, plan, at).map_err(other)?;
    let mut d = s.doc()?.clone();
    let mut doc = read_document(&d).map_err(|e| other(e.to_string()))?;
    let out = match existing {
        Some(i) if name.is_empty() => {
            let gone = doc.rooms.remove(i);
            let message = format!("Room {} {} removed", gone.number, gone.name);
            json!({ "id": null, "name": gone.name, "number": gone.number, "area_m2": null, "message": message })
        }
        None if name.is_empty() => return Err(other("Give the room a name")),
        Some(i) => {
            let r = &mut doc.rooms[i];
            r.name = name;
            if let Some(n) = number {
                r.number = n;
            }
            let message = format!("Room {} {}: {}", r.number, r.name, area_text(Some(area)));
            json!({ "id": r.id.0, "name": r.name, "number": r.number, "area_m2": area, "message": message })
        }
        None => {
            let id = Id(d.new_handle().0);
            let r = RoomRecord { id, plan, at, name, number: number.unwrap_or(default_number) };
            let message = format!("Room {} {}: {}", r.number, r.name, area_text(Some(area)));
            let v = json!({ "id": id.0, "name": r.name, "number": r.number, "area_m2": area, "message": message });
            doc.rooms.push(r);
            v
        }
    };
    write_document(&mut d, &doc).map_err(|e| other(e.to_string()))?;
    *s.doc_mut()? = d;
    s.echo(out["message"].as_str().unwrap_or_default().to_owned());
    Ok(out)
}

/// ROOM's interactive form, in the edited plan: `Pick a point inside the room`, `Room name`, then
/// `Room number <101>`.
#[derive(Default)]
struct RoomM {
    plan: Option<Id>,
    at: Option<Point2>,
    name: Option<String>,
    default_name: String,
    default_number: String,
}

impl Interactive for RoomM {
    fn name(&self) -> &'static str {
        "room"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match (&self.at, &self.name) {
            (None, _) => Prompt::new("Pick a point inside the room", Accept::POINT),
            (Some(_), None) => {
                let p = Prompt::new("Room name", Accept::TEXT);
                if self.default_name.is_empty() { p } else { p.default(self.default_name.clone()) }
            }
            (Some(_), Some(_)) => Prompt::new("Room number", Accept::TEXT).default(self.default_number.clone()),
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        match edited_view(s) {
            Some(v) => {
                self.plan = Some(v);
                Ok(Step::Continue)
            }
            None => {
                s.echo("ROOM needs a plan: open one from the Views palette or with EDITVIEW.");
                Ok(Step::Done)
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(plan) = self.plan else { return Ok(Step::Done) };
        match (self.at, self.name.is_some()) {
            (None, _) => {
                let Input::Point(q) = i else { return Ok(Step::Continue) };
                let at = Point2 { x: q.x, y: q.y };
                match resolve(s.doc()?, plan, at) {
                    Ok((existing, number, _)) => {
                        let doc = read_document(s.doc()?).map_err(|e| other(e.to_string()))?;
                        let r = existing.and_then(|i| doc.rooms.get(i));
                        self.default_name = r.map(|r| r.name.clone()).unwrap_or_default();
                        self.default_number = r.map_or(number, |r| r.number.clone());
                        self.at = Some(at);
                        Ok(Step::Continue)
                    }
                    Err(e) => {
                        s.echo(e);
                        Ok(Step::Continue)
                    }
                }
            }
            (Some(_), false) => {
                // An empty answer with no default is an empty name: it removes the room there.
                let name = match i {
                    Input::Enter => Some(self.default_name.clone()),
                    Input::Text(t) | Input::Keyword(t) => Some(t.trim().to_owned()),
                    _ => None,
                };
                let Some(name) = name else { return Ok(Step::Continue) };
                if name.is_empty() {
                    return self.finish(s, plan, String::new(), None);
                }
                self.name = Some(name);
                Ok(Step::Continue)
            }
            (Some(_), true) => {
                let Some(number) = name_input(i, &self.default_number) else { return Ok(Step::Continue) };
                let name = self.name.clone().unwrap_or_default();
                self.finish(s, plan, name, Some(number))
            }
        }
    }
}

impl RoomM {
    fn finish(&self, s: &mut Session, plan: Id, name: String, number: Option<String>) -> Result<Step> {
        let Some(at) = self.at else { return Ok(Step::Done) };
        if let Err(e) = room(s, &json!({ "viewId": plan.0, "at": {"x": at.x, "y": at.y}, "name": name, "number": number })) {
            s.echo(e.to_string());
        }
        Ok(Step::Done)
    }
}

pub(crate) fn room_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::<RoomM>::default())
}

pub const ROOM_TITLE: &str = "Room Schedule";
pub(crate) const DEFAULT_LAYOUT: &str = "A603";
const HEADER: [&str; 4] = ["Number", "Name", "Level", "Area (m²)"];
const COL_WIDTHS: [f64; 4] = [20.0, 50.0, 30.0, 25.0];

/// One room of the schedule; `area_m2` as [`RoomTag`]'s.
#[derive(Clone, Debug, PartialEq)]
pub struct RoomRow {
    pub number: String,
    pub name: String,
    pub level: String,
    pub area_m2: Option<f64>,
}

/// Room numbers in schedule order: numerically when both parse, else as text.
fn by_number(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(x), Ok(y)) => x.total_cmp(&y).then_with(|| a.cmp(b)),
        _ => a.cmp(b),
    }
}

/// Every room, by level then number. An unreadable drawing has none.
pub fn room_rows(d: &Drawing) -> Vec<RoomRow> {
    let tags = room_tags(d);
    if tags.is_empty() {
        return Vec::new();
    }
    let Ok(doc) = read_document(d) else { return Vec::new() };
    let mut levels: Vec<_> = doc.levels.iter().collect();
    levels.sort_by(|a, b| a.floor_elevation.value.total_cmp(&b.floor_elevation.value).then(a.order.total_cmp(&b.order)));
    let mut rows: Vec<(usize, RoomRow)> = tags
        .into_iter()
        .map(|t| {
            let at = level_of(&doc, t.plan).and_then(|id| levels.iter().position(|l| l.id == id));
            let level = at.and_then(|i| levels.get(i)).map_or_else(|| "—".to_owned(), |l| l.name.clone());
            (at.unwrap_or(usize::MAX), RoomRow { number: t.number, name: t.name, level, area_m2: t.area_m2 })
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| by_number(&a.1.number, &b.1.number)));
    rows.into_iter().map(|(_, r)| r).collect()
}

/// The room schedule with its top-left corner at `at` (paper mm): a row per room, then the total.
pub fn room_table(rows: &[RoomRow], at: Point2) -> Table {
    let area = |a: Option<f64>| a.map_or_else(|| "—".to_owned(), |a| format!("{a:.1}"));
    let mut cells: Vec<Vec<String>> = rows.iter().map(|r| vec![r.number.clone(), r.name.clone(), r.level.clone(), area(r.area_m2)]).collect();
    let known: Vec<f64> = rows.iter().filter_map(|r| r.area_m2).collect();
    let total = (!known.is_empty()).then(|| known.iter().sum::<f64>());
    cells.push(vec![String::new(), "Total".to_owned(), String::new(), area(total)]);
    bom::schedule_table(ROOM_TITLE, &HEADER, &COL_WIDTHS, &cells, at)
}

/// Refreshes the Room Schedule on its set sheet from [`room_rows`]; no rooms touches nothing.
pub(crate) fn refresh_room_schedule(d: &mut Drawing) -> Result<Option<Refreshed>> {
    let rows = room_rows(d);
    if rows.is_empty() {
        return Ok(None);
    }
    bom::refresh_table(d, DEFAULT_LAYOUT, ROOM_TITLE, &|at| room_table(&rows, at))
}

/// ROOMSCHEDULE `{layout?, at?}`. See the module doc.
pub(crate) fn roomschedule(s: &mut Session, p: &Value) -> Result<Value> {
    let rows = room_rows(s.doc()?);
    if rows.is_empty() {
        return Err(other("No rooms: name a plan's rooms with ROOM first"));
    }
    let (layout, handle) = write_schedule(s, "roomschedule", p, DEFAULT_LAYOUT, ROOM_TITLE, &|at| room_table(&rows, at))?;
    let message = format!("{ROOM_TITLE} on \"{layout}\": {} room{}.", rows.len(), if rows.len() == 1 { "" } else { "s" });
    let rows: Vec<Value> = rows.iter().map(|r| json!({ "number": r.number, "name": r.name, "level": r.level, "area_m2": r.area_m2 })).collect();
    Ok(json!({ "layout": layout, "handle": format!("{:X}", handle.0), "rows": rows, "message": message }))
}

/// ROOMSCHEDULE: `Enter layout for the room schedule <A603>:`.
struct RoomSchedule;

impl Interactive for RoomSchedule {
    fn name(&self) -> &'static str {
        "roomschedule"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter layout for the room schedule", Accept::TEXT).default(DEFAULT_LAYOUT.to_owned())
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(layout) = name_input(i, DEFAULT_LAYOUT) else { return Ok(Step::Continue) };
        match roomschedule(s, &json!({ "layout": layout })) {
            Ok(r) => s.echo(r["message"].as_str().unwrap_or_default().to_owned()),
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

pub(crate) fn roomschedule_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(RoomSchedule))
}

#[cfg(test)]
mod tests {
    use super::*;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    fn id(v: &Value) -> T<Id> {
        Ok(Id(v["viewId"].as_u64().ok_or("no id")?))
    }

    /// A LAMB project with a 5000 × 4000 centreline room of 200 walls on its edited plan (faces
    /// -100..5100 and 100..4900 by -100..4100 and 100..3900), offset by `dx`.
    fn project() -> T<(Session, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = id(&s.execute("lambproject", &json!({}))?)?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        walls(&mut s, 0.0)?;
        Ok((s, plan))
    }

    fn walls(s: &mut Session, dx: f64) -> R {
        if !s.doc()?.layers.iter().any(|l| l.name == "WALL-EXT") {
            s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        }
        for (a, b) in [(-100.0, 5100.0), (100.0, 4900.0)] {
            let (x0, x1, y0, y1) = (a + dx, b + dx, a, b - 1000.0);
            s.execute("pline", &json!({ "vertices": [[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]] }))?;
        }
        Ok(())
    }

    /// The room tag texts in the plan's annotation block: (text, x, y).
    fn tag_texts(s: &Session, plan: Id) -> T<Vec<(String, f64, f64)>> {
        let d = s.doc()?;
        let b = d.block(&crate::schema::annotation_block_name(plan)).ok_or("no annotation block")?;
        Ok(b.entities
            .iter()
            .filter_map(|e| match &e.kind {
                cadcraft_doc::EntityKind::Text(t) => {
                    let at = t.align_pt.unwrap_or(t.insert);
                    Some((t.value.clone(), at.x, at.y))
                }
                _ => None,
            })
            .collect())
    }

    fn undo_len(s: &Session) -> T<usize> {
        Ok(s.state()?.undo.len())
    }

    #[test]
    fn a_room_is_numbered_by_level_and_tagged_with_its_inner_area() -> R {
        let (mut s, plan) = project()?;
        let n = undo_len(&s)?;
        let r = s.execute("room", &json!({ "at": {"x": 2500, "y": 2000}, "name": "Living" }))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!((r["number"].as_str(), r["name"].as_str()), (Some("101"), Some("Living")));
        assert!((r["area_m2"].as_f64().ok_or("no area")? - 18.24).abs() < 1e-6, "{r}");
        let texts = tag_texts(&s, plan)?;
        let h = crate::annotations::CUT_TEXT_MM * 1.35;
        for (t, y) in [("Living", 2000.0 + h), ("101", 2000.0), ("18.2 m²", 2000.0 - h)] {
            assert!(texts.iter().any(|(v, x, yy)| v == t && (*x - 2500.0).abs() < 1e-6 && (*yy - y).abs() < 1e-6), "{t} in {texts:?}");
        }
        Ok(())
    }

    #[test]
    fn a_second_room_on_the_level_takes_the_next_number_and_a_second_level_starts_at_201() -> R {
        let (mut s, plan) = project()?;
        s.execute("room", &json!({ "at": {"x": 2500, "y": 2000}, "name": "Living" }))?;
        walls(&mut s, 6000.0)?;
        let r = s.execute("room", &json!({ "at": {"x": 8500, "y": 2000}, "name": "Bed" }))?;
        assert_eq!(r["number"], "102");
        let first = id(&s.execute("newoffsetplan", &json!({ "mode": "offset", "fromViewId": plan.0, "floorToFloorMm": 3000, "name": "First" }))?)?;
        s.execute("editview", &json!({ "viewId": first.0 }))?;
        walls(&mut s, 0.0)?;
        let r = s.execute("room", &json!({ "at": {"x": 2500, "y": 2000}, "name": "Study" }))?;
        assert_eq!(r["number"], "201");
        Ok(())
    }

    #[test]
    fn stretching_a_wall_out_updates_the_tag_in_that_step() -> R {
        let (mut s, plan) = project()?;
        s.execute("room", &json!({ "at": {"x": 2500, "y": 2000}, "name": "Living" }))?;
        let n = undo_len(&s)?;
        s.execute("stretch", &json!({ "window": [[4800, -500], [5500, 4500]], "delta": [1000, 0] }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        assert!(tag_texts(&s, plan)?.iter().any(|(t, ..)| t == "22.0 m²"), "{:?}", tag_texts(&s, plan)?);
        s.undo()?;
        assert!(tag_texts(&s, plan)?.iter().any(|(t, ..)| t == "18.2 m²"), "{:?}", tag_texts(&s, plan)?);
        Ok(())
    }

    #[test]
    fn re_picking_renames_an_empty_name_removes_and_each_undoes_in_one_step() -> R {
        let (mut s, plan) = project()?;
        let first = s.execute("room", &json!({ "at": {"x": 2500, "y": 2000}, "name": "Living" }))?;
        let r = s.execute("room", &json!({ "at": {"x": 1000, "y": 1000}, "name": "Lounge" }))?;
        assert_eq!((r["id"].clone(), r["number"].as_str()), (first["id"].clone(), Some("101")));
        let doc = read_document(s.doc()?)?;
        assert_eq!((doc.rooms.len(), doc.rooms[0].at), (1, Point2 { x: 2500.0, y: 2000.0 }), "keeps its point");
        assert!(tag_texts(&s, plan)?.iter().any(|(t, ..)| t == "Lounge"));
        s.execute("room", &json!({ "at": {"x": 1000, "y": 1000}, "name": "" }))?;
        assert!(read_document(s.doc()?)?.rooms.is_empty());
        assert!(tag_texts(&s, plan).map_or(true, |t| t.is_empty()));
        s.undo()?;
        assert_eq!(read_document(s.doc()?)?.rooms[0].name, "Lounge");
        s.undo()?;
        assert_eq!(read_document(s.doc()?)?.rooms[0].name, "Living");
        s.undo()?;
        assert!(read_document(s.doc()?)?.rooms.is_empty());
        Ok(())
    }

    #[test]
    fn a_point_outside_every_space_is_refused_with_the_reason() -> R {
        let (mut s, _plan) = project()?;
        let n = undo_len(&s)?;
        let e = s.execute("room", &json!({ "at": {"x": 9000, "y": 2000}, "name": "Yard" })).err().ok_or("refused")?;
        assert!(e.to_string().contains("Not inside a closed room"), "{e}");
        assert_eq!(undo_len(&s)?, n);
        Ok(())
    }

    #[test]
    fn the_interactive_form_prompts_point_name_then_number() -> R {
        let (mut s, plan) = project()?;
        s.start("rm")?;
        s.input(Input::Point(cadcraft_geom::Vec2::new(2500.0, 2000.0)))?;
        s.input(Input::Text("Kitchen".into()))?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("101"));
        s.input(Input::Enter)?;
        assert!(tag_texts(&s, plan)?.iter().any(|(t, ..)| t == "Kitchen"));
        Ok(())
    }

    #[test]
    fn rooms_survive_a_dxf_round_trip() -> R {
        let (mut s, _plan) = project()?;
        s.execute("room", &json!({ "at": {"x": 2500, "y": 2000}, "name": "Living" }))?;
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert!(crate::annotations::settled(&back)?, "the observer would change nothing after a round trip");
        assert_eq!(read_document(&back)?.rooms, read_document(s.doc()?)?.rooms);
        assert_eq!(room_tags(&back).first().map(|t| area_text(t.area_m2)).as_deref(), Some("18.2 m²"));
        Ok(())
    }

    /// Every cell of the A603 table below its header row.
    fn schedule_body(s: &Session) -> T<Vec<Vec<String>>> {
        let (_, t) = crate::schedule::titled_on(s.doc()?, DEFAULT_LAYOUT, ROOM_TITLE).ok_or("no schedule")?;
        Ok(t.cells[2..].iter().map(|r| r.iter().map(|c| c.text.clone()).collect()).collect())
    }

    /// Ground: 102 Bed (x 6000) named before 101 Living; `First` at 3000: 201 Study. Ground edited.
    fn two_levels() -> T<(Session, Id)> {
        let (mut s, plan) = project()?;
        walls(&mut s, 6000.0)?;
        s.execute("room", &json!({ "at": {"x": 8500, "y": 2000}, "name": "Bed", "number": "102" }))?;
        s.execute("room", &json!({ "at": {"x": 2500, "y": 2000}, "name": "Living", "number": "101" }))?;
        let first = id(&s.execute("newoffsetplan", &json!({ "mode": "offset", "fromViewId": plan.0, "floorToFloorMm": 3000, "name": "First" }))?)?;
        s.execute("editview", &json!({ "viewId": first.0 }))?;
        walls(&mut s, 0.0)?;
        s.execute("room", &json!({ "at": {"x": 2500, "y": 2000}, "name": "Study" }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        Ok((s, plan))
    }

    fn tables_on(s: &Session, layout: &str) -> T<usize> {
        let l = s.doc()?.layout(layout).ok_or("no layout")?;
        Ok(l.entities.iter().filter(|e| matches!(e.kind, cadcraft_doc::EntityKind::Table(_))).count())
    }

    #[test]
    fn the_schedule_runs_by_level_then_number_with_a_total_and_reruns_in_place() -> R {
        let (mut s, _plan) = two_levels()?;
        let n = undo_len(&s)?;
        let r = s.execute("roomschedule", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!((r["layout"].as_str(), r["rows"][0]["number"].as_str()), (Some("A603"), Some("101")));
        assert_eq!(r["message"], "Room Schedule on \"A603\": 3 rooms.");
        assert_eq!(
            schedule_body(&s)?,
            [["101", "Living", "Level 1", "18.2"], ["102", "Bed", "Level 1", "18.2"], ["201", "Study", "First", "18.2"], ["", "Total", "", "54.7"],]
        );
        let (_, t) = crate::schedule::titled_on(s.doc()?, DEFAULT_LAYOUT, ROOM_TITLE).ok_or("no schedule")?;
        assert_eq!(t.cells[1].iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), HEADER);

        let n = undo_len(&s)?;
        s.execute("roomschedule", &json!({}))?;
        assert_eq!(undo_len(&s)?, n, "an unchanged rerun changes nothing");
        s.execute("room", &json!({ "at": {"x": 2500, "y": 2000}, "name": "Lounge" }))?;
        s.execute("roomschedule", &json!({ "at": {"x": 50, "y": 100} }))?;
        assert_eq!(schedule_body(&s)?[0][1], "Lounge");
        assert_eq!(tables_on(&s, DEFAULT_LAYOUT)?, 1, "replaced, not added");
        let (_, t) = crate::schedule::titled_on(s.doc()?, DEFAULT_LAYOUT, ROOM_TITLE).ok_or("no schedule")?;
        assert_eq!((t.insert.x, t.insert.y), (50.0, 100.0));
        s.undo()?;
        assert_eq!(schedule_body(&s)?[0][1], "Living", "one undo takes the rerun back");
        Ok(())
    }

    #[test]
    fn no_rooms_is_refused_and_one_undo_removes_a_new_schedule() -> R {
        let (mut s, _plan) = project()?;
        let n = undo_len(&s)?;
        assert!(s.execute("roomschedule", &json!({})).is_err(), "nothing to schedule");
        assert_eq!(undo_len(&s)?, n);
        s.execute("sheetset", &json!({}))?;
        assert!(s.doc()?.layout(DEFAULT_LAYOUT).is_none(), "SHEETSET makes no A603");
        s.execute("room", &json!({ "at": {"x": 2500, "y": 2000}, "name": "Living" }))?;
        s.execute("roomschedule", &json!({}))?;
        assert!(s.doc()?.layout(DEFAULT_LAYOUT).is_some());
        s.undo()?;
        assert!(s.doc()?.layout(DEFAULT_LAYOUT).is_none(), "one undo removes it");
        Ok(())
    }

    #[test]
    fn sheetset_writes_a603_after_a602_lists_it_and_follows_a_rename() -> R {
        let (mut s, _plan) = two_levels()?;
        // A third ground room, unnamed, with a window in a gap of its south wall: the set's A602.
        for (a, b) in [(-100.0, 5100.0), (100.0, 4900.0)] {
            let (x0, x1, y0, y1) = (a + 12000.0, b + 12000.0, a, b - 1000.0);
            s.execute("pline", &json!({ "vertices": [[13000.0, y0], [x0, y0], [x0, y1], [x1, y1], [x1, y0], [14000.0, y0]] }))?;
        }
        s.execute("layer.new", &json!({ "name": "WINDOW", "current": true }))?;
        s.execute("line", &json!({ "points": [[13000, 0], [14000, 0]] }))?;
        assert_eq!(crate::schedule::opening_rows(s.doc()?).len(), 1);
        let r = s.execute("sheetset", &json!({}))?;
        let laid: Vec<(&str, &str)> = r["sheets"]
            .as_array()
            .ok_or("no sheets")?
            .iter()
            .map(|x| (x["layout"].as_str().unwrap_or(""), x["group"].as_str().unwrap_or("")))
            .collect();
        assert!(laid.contains(&("A603", "schedule")), "{r}");
        assert!(r["message"].as_str().is_some_and(|m| m.contains("A603 Room Schedule")), "{r}");
        assert_eq!(schedule_body(&s)?.len(), 4);
        let mut tabs: Vec<_> = s.doc()?.layouts.iter().map(|l| (l.tab_order, l.name.clone())).collect();
        tabs.sort();
        let names: Vec<&str> = tabs.iter().map(|(_, n)| n.as_str()).collect();
        let (a602, a603) = (names.iter().position(|n| *n == "A602"), names.iter().position(|n| *n == "A603"));
        assert!(a602.is_some() && a603 == a602.map(|i| i + 1), "{names:?}");
        let l = s.doc()?.layout("A001").ok_or("no list")?;
        let list = l
            .entities
            .iter()
            .find_map(|e| if let cadcraft_doc::EntityKind::Table(t) = &e.kind { Some(t.clone()) } else { None })
            .ok_or("no table")?;
        assert!(list.cells.iter().any(|r| r[0].text == "A603" && r[1].text == ROOM_TITLE), "{list:?}");

        s.execute("room", &json!({ "at": {"x": 8500, "y": 2000}, "name": "Bedroom" }))?;
        let r = s.execute("sheetset", &json!({}))?;
        assert!(r["message"].as_str().is_some_and(|m| m.contains("A603 Room Schedule updated")), "{r}");
        assert_eq!(schedule_body(&s)?[1], ["102", "Bedroom", "Level 1", "18.2"]);
        Ok(())
    }

    #[test]
    fn the_interactive_form_prompts_for_its_sheet() -> R {
        let (mut s, _plan) = project()?;
        s.execute("room", &json!({ "at": {"x": 2500, "y": 2000}, "name": "Living" }))?;
        s.start("rsch")?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("A603"));
        s.input(Input::Text("Rooms".into()))?;
        assert!(s.doc()?.layout("Rooms").is_some());
        Ok(())
    }
}

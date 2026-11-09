//! WALLTYPES (LAMB T688): the drawing set's wall-type legend — what each wall is built of, read from
//! the edge details assigned to the walls.
//!
//! A wall type is an edge detail (a detail whose reference is one face line, not a corner) that
//! resolves onto at least one wall run of a plan or section view ([`resolve_view_details`]); corner,
//! opening and end details are not wall types. Marks are `W1`, `W2`, … in the order the types are
//! first met: views in document order, runs in graph order. A detail whose build-up cannot be read
//! ([`detail_layers`]) is skipped.
//!
//! `walltypes {layout?, at?}` (`wtype`): the `Wall Types` table (`Mark | Detail | Build-up |
//! Thickness | Repeats | Used in`) on layout `layout` (default `A604`, its sheet in the set
//! ([`crate::sheetset`]), created without viewports when absent) at paper point `at`, placed and
//! replaced as [`crate::schedule`]'s opening schedule is. Build-up is each layer's whole-mm thickness
//! then its name, `·`-separated from the reference face (`13 GYP · 140 STUD`; a zero-thickness layer
//! shows its name alone); Thickness the largest band offset; Repeats `STUD @ 406` per repeat (its
//! first member's layer and pitch length); Used in the views' names in document order. One undo
//! step; no wall types is refused, nothing changed. Returns `{layout, handle, rows: [{mark, detail,
//! buildUp, thicknessMm, repeats, usedIn}], message}`.
//!
//! `walltags {viewId?, off?}` (`wtag`, LAMB T690) adds the placed plan to, or removes it from,
//! `wall_tag_plans` (refused for anything but a placed plan); [`crate::annotations`] writes each
//! listed plan's [`wall_tags`] into its annotation block: per run whose resolved detail is a wall
//! type, its mark in a hexagon on the run's inside face at its midpoint, none on a run shorter than
//! [`MIN_TAGGED_RUN_MM`] at 1:50. A plan already in that state records no undo step. Returns
//! `{viewId, tags, message}`.

use cadcraft_doc::{Drawing, Table};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use lamb_core::geometry::Point2;
use lamb_core::model::{AnyView, Id};
use serde_json::{Value, json};

use lamb_core::derivation::wall_spaces::outside_on_left;
use lamb_core::derivation::walls::{RunCurve, WallRun};
use lamb_core::geometry::point::{normalize2, sub2};

use crate::annotations::k;
use crate::assembly::{detail_layers, run_midpoint};
use crate::bom::{self, Refreshed};
use crate::commands::{bad, edited_view, id_value, name_input, other};
use crate::detailref::detail_reference;
use crate::detailsheets::named;
use crate::detailtags::{graph_and_sides, resolve_view_details};
use crate::project::DrawingProject;
use crate::repeats::detail_repeats;
use crate::schedule::write_schedule;
use crate::schema::{read_document, view_block_name, write_document};
use crate::sheets::view_scale;

pub const WALL_TYPES_TITLE: &str = "Wall Types";
pub(crate) const DEFAULT_LAYOUT: &str = "A604";
const HEADER: [&str; 6] = ["Mark", "Detail", "Build-up", "Thickness", "Repeats", "Used in"];
const COL_WIDTHS: [f64; 6] = [12.0, 40.0, 90.0, 20.0, 35.0, 50.0];

/// One wall type of the legend.
#[derive(Clone, Debug, PartialEq)]
pub struct WallTypeRow {
    pub mark: String,
    pub detail: Id,
    /// The detail view's name.
    pub name: String,
    pub build_up: String,
    /// The largest band offset, whole mm.
    pub thickness_mm: f64,
    pub repeats: String,
    /// Names of the plan and section views using it, document order.
    pub used_in: Vec<String>,
}

/// `STUD @ 406, …` for `detail`'s repeats; none (or unreadable) is blank.
fn repeats_text(d: &Drawing, detail: Id) -> String {
    let Ok(repeats) = detail_repeats(d, detail) else { return String::new() };
    let block = d.block(&view_block_name(detail));
    repeats
        .iter()
        .map(|r| {
            let layer = r
                .members
                .first()
                .and_then(|h| block.and_then(|b| b.entities.iter().find(|e| e.handle == *h)))
                .map_or_else(String::new, |e| e.common.layer.clone());
            format!("{layer} @ {:.0}", r.pitch.x.hypot(r.pitch.y))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every wall type in first-met order with the names of the views using it, a type whose build-up
/// cannot be read left out: the one numbering the legend and the plan tags both read (index `i` is
/// [`mark`]`(i)`). An unreadable drawing has none.
fn wall_types(d: &Drawing) -> Vec<(Id, Vec<String>)> {
    let Ok(doc) = read_document(d) else { return Vec::new() };
    let mut found: Vec<(Id, Vec<String>)> = Vec::new();
    for v in doc.views.iter().filter(|v| matches!(v, AnyView::Plan(_) | AnyView::Vertical(_))) {
        let Ok((runs, _)) = resolve_view_details(d, v.id()) else { continue };
        for detail in runs.iter().filter_map(named) {
            let edge = matches!(detail_reference(d, detail), Ok(Some(r)) if r.corner.is_none());
            if !edge {
                continue;
            }
            let name = v.name().to_owned();
            match found.iter_mut().find(|(id, _)| *id == detail) {
                Some((_, views)) if !views.contains(&name) => views.push(name),
                Some(_) => {}
                None => found.push((detail, vec![name])),
            }
        }
    }
    found.retain(|(detail, _)| detail_layers(d, *detail).is_ok());
    found
}

/// The mark of the `i`th wall type: `W1`, `W2`, …
fn mark(i: usize) -> String {
    format!("W{}", i + 1)
}

/// Every wall type, marked in first-met order. An unreadable drawing has none.
pub fn wall_type_rows(d: &Drawing) -> Vec<WallTypeRow> {
    let Ok(doc) = read_document(d) else { return Vec::new() };
    let mut rows = Vec::new();
    for (i, (detail, used_in)) in wall_types(d).into_iter().enumerate() {
        let Ok(layers) = detail_layers(d, detail) else { continue };
        let Some(view) = doc.views.iter().find(|v| v.id() == detail) else { continue };
        let build_up = layers
            .iter()
            .map(|l| if l.thickness.round() == 0.0 { l.layer.clone() } else { format!("{:.0} {}", l.thickness, l.layer) })
            .collect::<Vec<_>>()
            .join(" · ");
        let thickness_mm = layers.iter().map(|l| l.offset + l.thickness).fold(0.0, f64::max).round();
        rows.push(WallTypeRow {
            mark: mark(i),
            detail,
            name: view.name().to_owned(),
            build_up,
            thickness_mm,
            repeats: repeats_text(d, detail),
            used_in,
        });
    }
    rows
}

/// One wall type tag on a plan: its type's mark at a run's midpoint, `inward` the unit normal
/// toward the run's inside, `half_width` half the run's width.
#[derive(Clone, Debug, PartialEq)]
pub struct WallTag {
    pub plan: Id,
    pub mark: String,
    pub at: Point2,
    pub inward: Point2,
    pub half_width: f64,
}

/// A run shorter than this (× [`k`] of its plan's scale) gets no tag.
pub const MIN_TAGGED_RUN_MM: f64 = 1500.0;

fn run_length(run: &WallRun) -> f64 {
    match run.centreline {
        RunCurve::Line { from, to } => (to.x - from.x).hypot(to.y - from.y),
        RunCurve::Arc { radius, sweep, .. } => (radius * sweep).abs(),
    }
}

/// The left of `run`'s direction of travel at its midpoint: for an arc, radial at mid-sweep.
fn left_at_mid(run: &WallRun) -> Point2 {
    let dir = match run.centreline {
        RunCurve::Line { from, to } => normalize2(sub2(to, from)),
        RunCurve::Arc { start_angle, sweep, .. } => {
            let a = start_angle + sweep / 2.0;
            let turn = if sweep < 0.0 { -1.0 } else { 1.0 };
            Point2 { x: -a.sin() * turn, y: a.cos() * turn }
        }
    };
    Point2 { x: -dir.y, y: dir.x }
}

/// Every wall type tag of every placed plan in `wall_tag_plans`: one per run whose resolved edge
/// detail is a wall type and whose length is at least [`MIN_TAGGED_RUN_MM`] × k(scale), on its
/// inside (an undecided run's left).
pub fn wall_tags(d: &Drawing) -> Vec<WallTag> {
    let Ok(doc) = read_document(d) else { return Vec::new() };
    if doc.wall_tag_plans.is_empty() {
        return Vec::new();
    }
    let Ok(p) = DrawingProject::new(d) else { return Vec::new() };
    let types = wall_types(d);
    let mut out = Vec::new();
    for &plan in &doc.wall_tag_plans {
        if !doc.views.iter().any(|v| matches!(v, AnyView::Plan(pv) if pv.id == plan && pv.placement.is_some())) {
            continue;
        }
        let Ok((details, _)) = resolve_view_details(d, plan) else { continue };
        let min = MIN_TAGGED_RUN_MM * k(view_scale(d, &doc, plan));
        let (graph, sides) = graph_and_sides(&p, plan);
        for (i, rd) in details.iter().enumerate() {
            let Some(t) = named(rd).and_then(|id| types.iter().position(|(t, _)| *t == id)) else { continue };
            let Some(run) = graph.runs.get(i).filter(|r| run_length(r) >= min) else { continue };
            let left = left_at_mid(run);
            let inward = match sides.runs.get(i).and_then(outside_on_left) {
                Some(true) => Point2 { x: -left.x, y: -left.y },
                _ => left,
            };
            out.push(WallTag { plan, mark: mark(t), at: run_midpoint(run), inward, half_width: run.width / 2.0 });
        }
    }
    out
}

/// The Wall Types table with its top-left corner at `at` (paper mm): a row per type.
pub fn wall_type_table(rows: &[WallTypeRow], at: Point2) -> Table {
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|r| vec![r.mark.clone(), r.name.clone(), r.build_up.clone(), format!("{:.0}", r.thickness_mm), r.repeats.clone(), r.used_in.join(", ")])
        .collect();
    bom::schedule_table(WALL_TYPES_TITLE, &HEADER, &COL_WIDTHS, &cells, at)
}

/// Refreshes the Wall Types table on its set sheet from [`wall_type_rows`]; none touches nothing.
pub(crate) fn refresh_wall_types(d: &mut Drawing) -> Result<Option<Refreshed>> {
    let rows = wall_type_rows(d);
    if rows.is_empty() {
        return Ok(None);
    }
    bom::refresh_table(d, DEFAULT_LAYOUT, WALL_TYPES_TITLE, None, &|at| wall_type_table(&rows, at))
}

/// WALLTYPES `{layout?, at?}`. See the module doc.
pub(crate) fn walltypes(s: &mut Session, p: &Value) -> Result<Value> {
    let rows = wall_type_rows(s.doc()?);
    if rows.is_empty() {
        return Err(other("No wall types: tag a wall with an edge detail first (DETAILTAG)"));
    }
    let (layout, handle) = write_schedule(s, "walltypes", p, DEFAULT_LAYOUT, WALL_TYPES_TITLE, &|at| wall_type_table(&rows, at))?;
    let message = format!("{WALL_TYPES_TITLE} on \"{layout}\": {} type{}.", rows.len(), if rows.len() == 1 { "" } else { "s" });
    let rows: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({ "mark": r.mark, "detail": r.name, "buildUp": r.build_up, "thicknessMm": r.thickness_mm, "repeats": r.repeats, "usedIn": r.used_in })
        })
        .collect();
    Ok(json!({ "layout": layout, "handle": format!("{:X}", handle.0), "rows": rows, "message": message }))
}

/// WALLTYPES: `Enter layout for the wall types <A604>:`.
struct WallTypes;

impl Interactive for WallTypes {
    fn name(&self) -> &'static str {
        "walltypes"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter layout for the wall types", Accept::TEXT).default(DEFAULT_LAYOUT.to_owned())
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(layout) = name_input(i, DEFAULT_LAYOUT) else { return Ok(Step::Continue) };
        match walltypes(s, &json!({ "layout": layout })) {
            Ok(r) => s.echo(r["message"].as_str().unwrap_or_default().to_owned()),
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

pub(crate) fn walltypes_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(WallTypes))
}

/// A placed plan `view`'s name, or why it is not one.
fn check_plan(d: &Drawing, view: Id) -> std::result::Result<String, String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    match doc.views.iter().find(|v| v.id() == view) {
        None => Err("No such view".into()),
        Some(AnyView::Plan(p)) if p.placement.is_some() => Ok(p.name.clone()),
        Some(AnyView::Plan(p)) => Err(format!("{} is floating: place it on a level before tagging its walls", p.name)),
        Some(_) => Err("WALLTAGS tags plan views only".into()),
    }
}

/// WALLTAGS `{viewId?, off?}`. See the module doc.
pub(crate) fn walltags(s: &mut Session, p: &Value) -> Result<Value> {
    let view = match p.get("viewId") {
        None | Some(Value::Null) => edited_view(s).ok_or_else(|| other("Edit a plan first (EDITVIEW), or give `viewId`"))?,
        Some(v) => id_value(v).ok_or_else(|| bad("walltags", "`viewId` must be a view id"))?,
    };
    let off = match p.get("off") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(bad("walltags", "`off` must be true or false")),
    };
    let name = check_plan(s.doc()?, view).map_err(other)?;
    let mut doc = read_document(s.doc()?).map_err(|e| other(e.to_string()))?;
    let list = &mut doc.wall_tag_plans;
    if list.contains(&view) == off {
        list.retain(|v| *v != view);
        if !off {
            list.push(view);
        }
        write_document(s.doc_mut()?, &doc).map_err(|e| other(e.to_string()))?;
    }
    let (n, message) = if off {
        (0, format!("Wall tags off for {name}."))
    } else {
        let n = wall_tags(s.doc()?).iter().filter(|t| t.plan == view).count();
        (n, format!("{n} wall tag{} on {name}.", if n == 1 { "" } else { "s" }))
    };
    s.echo(message.clone());
    Ok(json!({ "viewId": view.0, "tags": n, "message": message }))
}

/// WALLTAGS' interactive form, on the edited plan: `Wall tags [On/Off] <On>:`.
struct WallTagsM {
    view: Id,
}

impl Interactive for WallTagsM {
    fn name(&self) -> &'static str {
        "walltags"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Wall tags", Accept::TEXT).kw(&["On", "Off"]).options_only().default("On".to_owned())
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        s.echo("WALLTAGS: tags each wall of this plan with its wall type mark from the Wall Types legend, on screen and on its sheet.");
        let edited = edited_view(s).ok_or_else(|| "WALLTAGS needs a plan: open one from the Views palette or with EDITVIEW.".to_owned());
        match edited.and_then(|v| check_plan(s.doc().map_err(|e| e.to_string())?, v).map(|_| v)) {
            Ok(v) => {
                self.view = v;
                Ok(Step::Continue)
            }
            Err(e) => {
                s.echo(e);
                Ok(Step::Done)
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let off = match i {
            Input::Cancel => return Ok(Step::Cancel),
            Input::Enter => false,
            Input::Text(t) | Input::Keyword(t) => match t.trim().to_ascii_lowercase().as_str() {
                "on" => false,
                "off" => true,
                _ => {
                    s.echo("On or Off");
                    return Ok(Step::Continue);
                }
            },
            _ => return Ok(Step::Continue),
        };
        if let Err(e) = walltags(s, &json!({ "viewId": self.view.0, "off": off })) {
            s.echo(e.to_string());
        }
        Ok(Step::Done)
    }
}

pub(crate) fn walltags_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(WallTagsM { view: Id(0) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assembly::tests::{corner_detail, line, room_with};
    use cadcraft_doc::{EntityKind, Handle};
    use std::collections::HashSet;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    fn undo_len(s: &Session) -> T<usize> {
        Ok(s.state()?.undo.len())
    }

    fn all_handles(s: &Session) -> T<HashSet<Handle>> {
        let d = s.doc()?;
        Ok(d.model.iter().map(|e| e.handle).chain(d.blocks.values().flat_map(|b| b.entities.iter().map(|e| e.handle))).collect())
    }

    fn id(r: &Value) -> T<Id> {
        Ok(Id(r["viewId"].as_u64().ok_or("no id")?))
    }

    /// `assembly.rs`'s tagged room (plan `Ground`, detail `D1`: CLAD at −20, GYP at 50 and 200) plus
    /// a corner detail `CD` at its (0,0) corner, a vertical detail `SD` (GYP at 50 and 300, a STUD
    /// repeat at 406), section `A-A` along y = 0 whose left cut wall takes `SD`, and a second plan
    /// `Upper` with the same room taking `D1`. Returns (session, [D1, SD]).
    fn tagged() -> T<(Session, [Id; 2])> {
        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        corner_detail(&mut s, plan, "exterior_face", false, (0.0, 0.0))?;
        let d1 = read_document(s.doc()?)?.views.iter().find(|v| v.name() == "D1").ok_or("no D1")?.id();
        let sd = id(&s.execute("newdetail", &json!({ "name": "SD", "cutDirection": "vertical" }))?)?;
        s.execute("editview", &json!({ "viewId": sd.0 }))?;
        s.execute("layer.current", &json!({ "name": "0" }))?;
        let r = line(&mut s, [0.0, 0.0], [1000.0, 0.0])?;
        s.execute("detailref", &json!({ "handle": format!("{:X}", r.0), "kind": "exterior_face" }))?;
        s.execute("layer.current", &json!({ "name": "GYP" }))?;
        line(&mut s, [0.0, 50.0], [1000.0, 50.0])?;
        line(&mut s, [0.0, 300.0], [1000.0, 300.0])?;
        s.execute("layer.new", &json!({ "name": "STUD", "current": true }))?;
        let before = all_handles(&s)?;
        s.execute("pline", &json!({ "vertices": [[100, 50], [138, 50], [138, 200], [100, 200]], "closed": true }))?;
        let stud = all_handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("no stud")?;
        let before = all_handles(&s)?;
        s.execute("dimaligned", &json!({ "p1": [100, 0], "p2": [506, 0], "at": [300, -300] }))?;
        let pitch = all_handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("no dimension")?;
        s.execute("repeat", &json!({ "dimension": format!("{:X}", pitch.0), "handles": [format!("{:X}", stud.0)] }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let r =
            s.execute("newsectionline", &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 0}, "cutTo": {"x": 5000, "y": 0} }))?;
        let sec = id(&r)?;
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.execute("layer.current", &json!({ "name": "WALL-EXT" }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [4000, 0], [4000, 3000], [0, 3000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[300, 250], [3700, 250], [3700, 2750], [300, 2750]], "closed": true }))?;
        let hs: Vec<String> = s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.iter().map(|e| format!("{:X}", e.handle.0)).collect();
        s.execute("setrole", &json!({ "handles": hs, "role": "cut" }))?;
        s.execute("layer.current", &json!({ "name": "TAGS" }))?;
        s.execute("detailtag", &json!({ "detailViewId": sd.0, "tip": {"x": 150.0, "y": 2000.0}, "bubble": {"x": -1000.0, "y": 2500.0} }))?;
        let upper = id(&s.execute("newplan", &json!({ "name": "Upper" }))?)?;
        s.execute("editview", &json!({ "viewId": upper.0 }))?;
        s.execute("layer.current", &json!({ "name": "WALL-EXT" }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [5000, 0], [5000, 4000], [0, 4000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [4800, 200], [4800, 3800], [200, 3800]], "closed": true }))?;
        s.execute("layer.current", &json!({ "name": "TAGS" }))?;
        s.execute("detailtag", &json!({ "detailViewId": d1.0, "tip": {"x": 2500.0, "y": 0.0}, "bubble": {"x": 1500.0, "y": -1000.0} }))?;
        Ok((s, [d1, sd]))
    }

    /// Every cell of the A604 table below its header row.
    fn body(s: &Session) -> T<Vec<Vec<String>>> {
        let (_, t) = crate::schedule::titled_on(s.doc()?, DEFAULT_LAYOUT, WALL_TYPES_TITLE).ok_or("no table")?;
        assert_eq!(t.cells[1].iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), HEADER);
        Ok(t.cells[2..].iter().map(|r| r.iter().map(|c| c.text.clone()).collect()).collect())
    }

    /// An edge detail `name`: reference (0,0)–(1000,0), GYP bands at `bands`.
    fn edge_detail(s: &mut Session, name: &str, bands: [f64; 2]) -> T<Id> {
        let d = id(&s.execute("newdetail", &json!({ "name": name }))?)?;
        s.execute("editview", &json!({ "viewId": d.0 }))?;
        s.execute("layer.current", &json!({ "name": "0" }))?;
        let r = line(s, [0.0, 0.0], [1000.0, 0.0])?;
        s.execute("detailref", &json!({ "handle": format!("{:X}", r.0), "kind": "exterior_face" }))?;
        if s.execute("layer.new", &json!({ "name": "GYP", "current": true })).is_err() {
            s.execute("layer.current", &json!({ "name": "GYP" }))?;
        }
        for y in bands {
            line(s, [0.0, y], [1000.0, y])?;
        }
        Ok(d)
    }

    /// A placed plan with the observers installed, a closed room of 200 mm WALL-EXT walls, outer face
    /// 0..`w` × 0..4000, edge detail `D1` tagged on its bottom and left walls and `D2` on its top and
    /// right. Returns (session, plan, [D1, D2], the top tag's entities).
    fn two_types(w: f64) -> T<(Session, Id, [Id; 2], Vec<Handle>)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = id(&s.execute("lambproject", &json!({}))?)?;
        let d1 = edge_detail(&mut s, "D1", [50.0, 200.0])?;
        let d2 = edge_detail(&mut s, "D2", [13.0, 140.0])?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [w, 0], [w, 4000], [0, 4000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [w - 200.0, 200], [w - 200.0, 3800], [200, 3800]], "closed": true }))?;
        s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
        s.execute("detailtag", &json!({ "detailViewId": d1.0, "tip": {"x": w / 2.0, "y": 0.0}, "bubble": {"x": w / 2.0, "y": -1000.0} }))?;
        s.execute("detailtag", &json!({ "detailViewId": d1.0, "tip": {"x": 0.0, "y": 2000.0}, "bubble": {"x": -1000.0, "y": 2000.0} }))?;
        s.execute("detailtag", &json!({ "detailViewId": d2.0, "tip": {"x": w, "y": 2000.0}, "bubble": {"x": w + 1000.0, "y": 2000.0} }))?;
        let before = all_handles(&s)?;
        s.execute("detailtag", &json!({ "detailViewId": d2.0, "tip": {"x": w / 2.0, "y": 4000.0}, "bubble": {"x": w / 2.0, "y": 5000.0} }))?;
        let top = all_handles(&s)?.into_iter().filter(|h| !before.contains(h)).collect();
        Ok((s, plan, [d1, d2], top))
    }

    /// Plan `plan`'s annotation block: each hexagon's centre (the centroid of its six LINEs) with
    /// the text inside it, in block order.
    fn hexagons(s: &Session, plan: Id) -> T<Vec<(f64, f64, String)>> {
        let Some(b) = s.doc()?.block(&crate::schema::annotation_block_name(plan)) else { return Ok(Vec::new()) };
        let es: Vec<&EntityKind> = b.entities.iter().map(|e| &e.kind).collect();
        let mut out = Vec::new();
        for w in es.windows(7) {
            let lines: Vec<_> = w[..6].iter().filter_map(|k| if let EntityKind::Line(l) = k { Some(l) } else { None }).collect();
            let (EntityKind::Text(t), 6) = (w[6], lines.len()) else { continue };
            let closed = (0..6).all(|j| (lines[j].b.x - lines[(j + 1) % 6].a.x).abs() < 1e-6 && (lines[j].b.y - lines[(j + 1) % 6].a.y).abs() < 1e-6);
            if !closed {
                continue;
            }
            let (x, y) = (lines.iter().map(|l| l.a.x).sum::<f64>() / 6.0, lines.iter().map(|l| l.a.y).sum::<f64>() / 6.0);
            out.push((x.round(), y.round(), t.value.clone()));
        }
        Ok(out)
    }

    fn sorted(mut h: Vec<(f64, f64, String)>) -> Vec<(f64, f64, String)> {
        h.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        h
    }

    fn tag(x: f64, y: f64, m: &str) -> (f64, f64, String) {
        (x, y, m.to_owned())
    }

    #[test]
    fn a_tagged_plan_marks_each_run_inside_with_its_a604_row() -> R {
        let (mut s, plan, [d1, d2], _) = two_types(5000.0)?;
        assert!(hexagons(&s, plan)?.is_empty(), "off by default");
        let n = undo_len(&s)?;
        let r = s.execute("walltags", &json!({ "viewId": plan.0 }))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!(r["tags"], 4, "{r}");
        s.execute("walltypes", &json!({}))?;
        let rows = body(&s)?;
        let mark = |d: &str| rows.iter().find(|r| r[1] == d).map(|r| r[0].clone()).ok_or("no row");
        let (m1, m2) = (mark("D1")?, mark("D2")?);
        assert_eq!(wall_type_rows(s.doc()?).iter().map(|r| r.detail).collect::<Vec<_>>(), [d1, d2]);
        // Each centre sits half the wall plus 400 inside the run's midpoint: inside the room.
        assert_eq!(
            sorted(hexagons(&s, plan)?),
            sorted(vec![tag(2500.0, 600.0, &m1), tag(600.0, 2000.0, &m1), tag(2500.0, 3400.0, &m2), tag(4400.0, 2000.0, &m2)]),
        );
        let n = undo_len(&s)?;
        s.execute("walltags", &json!({ "viewId": plan.0 }))?;
        assert_eq!(undo_len(&s)?, n, "already on records no undo step");
        Ok(())
    }

    #[test]
    fn a_short_run_gets_no_tag() -> R {
        let (mut s, plan, _, _) = two_types(1000.0)?;
        let r = s.execute("walltags", &json!({ "viewId": plan.0 }))?;
        assert_eq!(r["message"], "2 wall tags on Ground Floor.", "{r}");
        assert_eq!(hexagons(&s, plan)?.len(), 2);
        assert!(hexagons(&s, plan)?.iter().all(|h| (h.1 - 2000.0).abs() < 1.0), "only the 4000 walls");
        Ok(())
    }

    #[test]
    fn tags_follow_their_walls_and_details_and_turn_off() -> R {
        let (mut s, plan, [d1, _], top) = two_types(5000.0)?;
        s.execute("walltags", &json!({ "viewId": plan.0 }))?;
        let n = undo_len(&s)?;
        s.execute("stretch", &json!({ "window": [[4700, -100], [5100, 4100]], "delta": [500, 0] }))?;
        assert_eq!(undo_len(&s)?, n + 1, "the move and its tag are one step");
        assert!(hexagons(&s, plan)?.contains(&tag(4900.0, 2000.0, "W2")), "{:?}", hexagons(&s, plan)?);
        s.undo()?;
        assert!(hexagons(&s, plan)?.contains(&tag(4400.0, 2000.0, "W2")));
        s.execute("erase", &json!({ "handles": top.iter().map(|h| format!("{:X}", h.0)).collect::<Vec<_>>() }))?;
        s.execute("detailtag", &json!({ "detailViewId": d1.0, "tip": {"x": 2500.0, "y": 4000.0}, "bubble": {"x": 2500.0, "y": 5000.0} }))?;
        assert!(hexagons(&s, plan)?.contains(&tag(2500.0, 3400.0, "W1")), "retagged: {:?}", hexagons(&s, plan)?);
        let r = s.execute("walltags", &json!({ "viewId": plan.0, "off": true }))?;
        assert_eq!(r["message"], "Wall tags off for Ground Floor.");
        assert!(hexagons(&s, plan)?.is_empty());
        Ok(())
    }

    #[test]
    fn a_plan_never_turned_on_is_unchanged_and_the_list_survives_dxf() -> R {
        let (mut s, plan, _, _) = two_types(5000.0)?;
        let block = |s: &Session| -> T<Option<String>> {
            Ok(s.doc()?.block(&crate::schema::annotation_block_name(plan)).map(|b| format!("{:?}", b.entities)))
        };
        let before = block(&s)?;
        s.execute("walltypes", &json!({}))?;
        assert_eq!(block(&s)?, before, "a legend alone tags nothing");
        assert!(hexagons(&s, plan)?.is_empty());
        s.execute("walltags", &json!({ "viewId": plan.0 }))?;
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert_eq!(read_document(&back)?.wall_tag_plans, [plan]);
        assert!(crate::annotations::settled(&back)?, "the observer would change nothing after a round trip");
        Ok(())
    }

    #[test]
    fn walltags_outside_a_placed_plan_is_refused_with_no_undo_step() -> R {
        let (mut s, _, [d1, _], _) = two_types(5000.0)?;
        let floating = id(&s.execute("newplan", &json!({ "name": "Loose" }))?)?;
        let n = undo_len(&s)?;
        assert!(s.execute("walltags", &json!({ "viewId": d1.0 })).is_err(), "a detail");
        assert!(s.execute("walltags", &json!({ "viewId": floating.0 })).is_err(), "a floating plan");
        assert_eq!(undo_len(&s)?, n);
        assert!(read_document(s.doc()?)?.wall_tag_plans.is_empty());
        Ok(())
    }

    #[test]
    fn the_interactive_form_asks_on_or_off() -> R {
        let (mut s, plan, _, _) = two_types(5000.0)?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.start("wtag")?;
        let p = s.current_prompt().ok_or("no prompt")?;
        assert_eq!((p.message.as_str(), p.default.as_deref()), ("Wall tags", Some("On")));
        s.input(Input::Enter)?;
        assert_eq!(hexagons(&s, plan)?.len(), 4);
        Ok(())
    }

    #[test]
    fn edge_details_on_walls_become_marked_types_and_the_corner_detail_does_not() -> R {
        let (mut s, [d1, sd]) = tagged()?;
        let rows = wall_type_rows(s.doc()?);
        assert_eq!(rows.iter().map(|r| r.detail).collect::<Vec<_>>(), [d1, sd], "{rows:?}");
        let n = undo_len(&s)?;
        let r = s.execute("walltypes", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!(r["message"], "Wall Types on \"A604\": 2 types.");
        assert_eq!(r["rows"][1]["repeats"], "STUD @ 406", "{r}");
        assert_eq!(
            body(&s)?,
            [["W1", "D1", "CLAD · 150 GYP", "200", "", "Ground, Upper"], ["W2", "SD", "250 GYP", "300", "STUD @ 406", "A-A"],],
            "{r}"
        );
        let n = undo_len(&s)?;
        s.execute("walltypes", &json!({}))?;
        assert_eq!(undo_len(&s)?, n, "an unchanged rerun changes nothing");
        Ok(())
    }

    #[test]
    fn sheetset_writes_a604_lists_it_on_a001_and_a_rerun_changes_nothing() -> R {
        let (mut s, _) = tagged()?;
        let r = s.execute("sheetset", &json!({}))?;
        assert!(r["message"].as_str().is_some_and(|m| m.contains("A604 Wall Types")), "{r}");
        assert_eq!(body(&s)?.len(), 2);
        let l = s.doc()?.layout("A001").ok_or("no list")?;
        let list = l.entities.iter().find_map(|e| if let EntityKind::Table(t) = &e.kind { Some(t.clone()) } else { None }).ok_or("no table")?;
        assert!(list.cells.iter().any(|r| r[0].text == "A604" && r[1].text == WALL_TYPES_TITLE), "{list:?}");
        let n = undo_len(&s)?;
        s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n, "a second SHEETSET changes nothing");
        Ok(())
    }

    #[test]
    fn no_edge_detail_is_refused_and_sheetset_makes_no_a604() -> R {
        let (mut s, _plan) = room_with("exterior_face", false, 200.0)?;
        let n = undo_len(&s)?;
        assert!(s.execute("walltypes", &json!({})).is_err(), "nothing to list");
        assert_eq!(undo_len(&s)?, n, "no undo step");
        s.execute("sheetset", &json!({}))?;
        assert!(s.doc()?.layout(DEFAULT_LAYOUT).is_none(), "SHEETSET makes no A604");
        Ok(())
    }

    #[test]
    fn the_interactive_form_prompts_for_its_sheet() -> R {
        let (mut s, _) = tagged()?;
        s.start("wtype")?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("A604"));
        s.input(Input::Text("Walls".into()))?;
        assert!(s.doc()?.layout("Walls").is_some());
        Ok(())
    }
}

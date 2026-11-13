//! OPENINGSCHEDULE (LAMB T639): the door and window schedule of a drawing set, written as a `Table`
//! on a sheet layout, and the mark each opening shows on its plan.
//!
//! Computed on demand, never stored: one row per opening bridge of the wall graph of every placed
//! plan, by level order, then document order, then x and y of the bridge's midpoint. Marks are
//! `D01`, `D02`, … and `W01`, `W02`, … over the whole project in row order, so **they renumber when
//! an opening is added or moves past another**. A type is the layer of the first entity spanning
//! the bridge: a name starting `DOOR` (ASCII case-insensitive) is a door, otherwise a window, the
//! only other layer the wall graph takes an opening from.
//!
//! `Width` is the bridge's length. `Jamb detail` is the name of the detail
//! [`crate::assembly::opening_assembly`] resolves for the bridge. `Height` and `Sill` come from the
//! first placed section, in document order, whose cut line crosses the bridge: of that section's
//! bridges with a head and a sill, the nearest to the crossing's `u`, no further from it than the
//! plan's wall is wide, whose sill stands on the row's level (the level's floor up to the next
//! floor, so a window above another in one cut is read at its own level). Height is head z minus
//! sill z, Sill is sill z minus the floor. No such section, or no such bridge in it, shows `—` in
//! both; nothing is guessed from defaults. Elevations are read for marks only, and blocks used as
//! openings are not read.
//!
//! [`crate::annotations`] writes each [`plan_marks`] entry into the plan's annotation block, so the
//! plan and the schedule agree, and each [`elevation_marks`] entry into its elevation's (LAMB T710).
//! An elevation marks an opening whose plan `out` faces the viewer (within 45° of the reverse gaze),
//! whose midpoint projects onto the cut line (`0 ≤ u ≤ length`), and which is not hidden: another
//! such opening on the same level within [`HIDE_U_MM`] in `u` and nearer the viewer hides it. The
//! mark sits at the opening's centre, `floor + sill + height / 2`, when its row has both, else at
//! `floor +` [`UNREAD_MARK_Z_MM`] — a first try, so a wing hiding a wall further back can still
//! leave a wrong mark. An opening on no level is not marked.
//!
//! `openingschedule {layout?, at?}` (`osch`): the table (`Mark | Type | Level | Width | Height |
//! Sill | Jamb detail`, titled `Opening Schedule`) on layout `layout` (default `A602`, its sheet in
//! the set ([`crate::sheetset`]), created without viewports when absent) at paper point `at` (default
//! where a table of that title already stands, else the top-left corner of the sheet's usable area).
//! A table on that layout with that title is replaced; one undo step. No openings is refused,
//! nothing changed. Returns `{layout, handle, rows: [{mark, type, level, width, height, sill,
//! jambDetail}], message}`.

use std::collections::HashMap;

use cadcraft_doc::{Drawing, EntityKind, Handle, Table};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::Vec3;
use lamb_core::derivation::markers::compute_cut_lines_for_view;
use lamb_core::derivation::projection::{Uz, plan_local_to_global_xy, vertical_basis, vertical_gaze_dir, vertical_local_to_uz, vertical_uz_to_local};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::derivation::wall_junctions::{RunEnd, wall_graph};
use lamb_core::derivation::wall_spaces::outside_on_left;
use lamb_core::derivation::walls::{RunCurve, WallRun};
use lamb_core::geometry::Point2;
use lamb_core::geometry::point::{add2, cross2, dist2, dot2, lerp2, normalize2, sub2};
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AnyView, Id, Level, Placement, VerticalIntent};
use serde_json::{Value, json};

use crate::assembly::opening_assembly;
use crate::bom::{self, Refreshed};
use crate::commands::{bad, name_input, other, point_param};
use crate::detailtags::{graph_and_sides, wall_params};
use crate::project::DrawingProject;
use crate::schema::read_document;

pub const OPENING_TITLE: &str = "Opening Schedule";
pub(crate) const DEFAULT_LAYOUT: &str = "A602";
const NONE: &str = "—";
/// An elevation's mark height above the floor for an opening no section reads (LAMB T710).
pub const UNREAD_MARK_Z_MM: f64 = 1200.0;
/// How near in `u` a nearer facing opening hides one behind it on an elevation.
pub const HIDE_U_MM: f64 = 300.0;
const HEADER: [&str; 7] = ["Mark", "Type", "Level", "Width", "Height", "Sill", "Jamb detail"];
const COL_WIDTHS: [f64; 7] = [15.0, 20.0, 30.0, 20.0, 20.0, 20.0, 45.0];

/// One opening of a placed plan. Lengths are millimetres, as computed.
#[derive(Clone, Debug, PartialEq)]
pub struct OpeningRow {
    pub mark: String,
    /// `Door` or `Window`.
    pub kind: String,
    pub level: String,
    pub width: f64,
    pub height: Option<f64>,
    pub sill: Option<f64>,
    pub jamb_detail: Option<String>,
}

/// Where a plan shows an opening's mark, in the plan's local coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanMark {
    pub plan: Id,
    pub mark: String,
    /// The bridge's midpoint.
    pub at: Point2,
    /// Unit vector square to the bridge, to the outside, or to the left of the wall run's direction
    /// when the outside is undecided.
    pub out: Point2,
}

/// Where an elevation shows an opening's mark, in the elevation's local coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct ElevationMark {
    pub view: Id,
    pub mark: String,
    pub at: Point2,
    /// The opening's midpoint along the elevation's cut line.
    pub u: f64,
    /// The opening's level's floor elevation.
    pub floor: f64,
    /// The opening's index into [`opening_rows`].
    pub row: usize,
}

/// One opening before its section and detail are read.
struct Opening {
    bridge: usize,
    level: Option<usize>,
    from: Point2,
    to: Point2,
    wall_width: f64,
    mark: PlanMark,
    door: bool,
}

/// The left of `run`'s direction of travel at `end`.
fn run_left(run: &WallRun, end: RunEnd) -> Point2 {
    let dir = match run.centreline {
        RunCurve::Line { from, to } => normalize2(sub2(to, from)),
        RunCurve::Arc { start_angle, sweep, .. } => {
            let a = if end == RunEnd::Start { start_angle } else { start_angle + sweep };
            let turn = if sweep < 0.0 { -1.0 } else { 1.0 };
            Point2 { x: -a.sin() * turn, y: a.cos() * turn }
        }
    };
    Point2 { x: -dir.y, y: dir.x }
}

/// Every opening of every placed plan in row order, marked, and the levels their `level` indexes.
/// An unreadable drawing has none.
fn openings(d: &Drawing) -> (Vec<Opening>, Vec<Level>) {
    let Ok(doc) = read_document(d) else { return (Vec::new(), Vec::new()) };
    let Ok(p) = DrawingProject::new(d) else { return (Vec::new(), doc.levels) };
    let mut found: Vec<(f64, usize, Opening)> = Vec::new();
    for (vi, v) in doc.views.iter().enumerate() {
        let AnyView::Plan(pv) = v else { continue };
        let Some(Placement::Plan { level_id, .. }) = &pv.placement else { continue };
        let level = doc.levels.iter().position(|l| l.id == *level_id);
        let order = level.and_then(|i| doc.levels.get(i)).map_or(f64::INFINITY, |l| l.order);
        let (graph, sides) = graph_and_sides(&p, pv.id);
        for (bridge, b) in graph.bridges.iter().enumerate() {
            let Some(run) = graph.runs.get(b.a.0) else { continue };
            let outside_left = sides.runs.get(b.a.0).and_then(outside_on_left).unwrap_or(true);
            let along = normalize2(sub2(b.to, b.from));
            let square = Point2 { x: -along.y, y: along.x };
            // `square` is on the run's left or its right; the mark takes the outside of the two.
            let square_is_left = dot2(square, run_left(run, b.a.1)) >= 0.0;
            let out = if square_is_left == outside_left { square } else { Point2 { x: along.y, y: -along.x } };
            let layer = b.opening_entity_ids.first().and_then(|id| p.entity(*id)).map(|e| e.layer_id.to_owned());
            let name = layer.and_then(|id| p.layers().iter().find(|l| l.id == id).map(|l| l.name.clone())).unwrap_or_default();
            let door = name.get(..4).is_some_and(|s| s.eq_ignore_ascii_case("DOOR"));
            let mark = PlanMark { plan: pv.id, mark: String::new(), at: lerp2(b.from, b.to, 0.5), out };
            found.push((order, vi, Opening { bridge, level, from: b.from, to: b.to, wall_width: run.width, mark, door }));
        }
    }
    found.sort_by(|a, b| {
        a.0.total_cmp(&b.0)
            .then(a.1.cmp(&b.1))
            .then_with(|| a.2.mark.at.x.total_cmp(&b.2.mark.at.x))
            .then_with(|| a.2.mark.at.y.total_cmp(&b.2.mark.at.y))
    });
    let (mut doors, mut windows) = (0, 0);
    let mut out: Vec<Opening> = found.into_iter().map(|(_, _, o)| o).collect();
    for o in &mut out {
        let (letter, n) = if o.door { ('D', &mut doors) } else { ('W', &mut windows) };
        *n += 1;
        o.mark.mark = format!("{letter}{n:02}");
    }
    (out, doc.levels)
}

/// Each opening's mark, for the plans' annotation blocks.
pub fn plan_marks(d: &Drawing) -> Vec<PlanMark> {
    openings(d).0.into_iter().map(|o| o.mark).collect()
}

/// Where segment `a`–`b` crosses the cut `from`–`to`, as the distance along the cut.
fn crossing_u(from: Point2, to: Point2, a: Point2, b: Point2) -> Option<f64> {
    let (cut, span) = (sub2(to, from), sub2(b, a));
    let (cut_len, span_len) = (dist2(from, to), dist2(a, b));
    let denom = cross2(cut, span);
    if denom.abs() <= TOLERANCE.angle_deg.to_radians().sin() * cut_len * span_len {
        return None;
    }
    let along_cut = cross2(sub2(a, from), span) / denom * cut_len;
    let along_span = cross2(sub2(a, from), cut) / denom * span_len;
    let within = |t: f64, len: f64| t >= -TOLERANCE.coincidence_mm && t <= len + TOLERANCE.coincidence_mm;
    (within(along_cut, cut_len) && within(along_span, span_len)).then_some(along_cut)
}

/// A section's openings that have a head and a sill: (u, sill z, head z).
fn section_openings(p: &DrawingProject<'_>, section: Id) -> Vec<(f64, f64, f64)> {
    let Some(view) = p.view(section) else { return Vec::new() };
    let graph = wall_graph(p, section, &wall_params(p, section));
    graph
        .bridges
        .iter()
        .filter_map(|b| {
            let (a, c) = (vertical_local_to_uz(&view, b.from), vertical_local_to_uz(&view, b.to));
            ((a.z - c.z).abs() > TOLERANCE.coincidence_mm).then(|| ((a.u + c.u) / 2.0, a.z.min(c.z), a.z.max(c.z)))
        })
        .collect()
}

/// Every opening of every placed plan, in row order.
pub fn opening_rows(d: &Drawing) -> Vec<OpeningRow> {
    let (found, levels) = openings(d);
    if found.is_empty() {
        return Vec::new();
    }
    let Ok(doc) = read_document(d) else { return Vec::new() };
    let Ok(p) = DrawingProject::new(d) else { return Vec::new() };
    let is_section = |id: Id| doc.views.iter().any(|v| matches!(v, AnyView::Vertical(s) if s.id == id && s.intent == VerticalIntent::Section));
    let name_of = |id: Id| doc.views.iter().find(|v| v.id() == id).map(|v| v.name().to_owned());
    let mut sections: HashMap<Id, Vec<(f64, f64, f64)>> = HashMap::new();
    let mut rows = Vec::with_capacity(found.len());
    for o in found {
        let level = o.level.and_then(|i| levels.get(i));
        let cuts = compute_cut_lines_for_view(&p, o.mark.plan).unwrap_or_default();
        let crossing =
            cuts.iter().filter(|c| is_section(c.source_view_id)).find_map(|c| crossing_u(c.from, c.to, o.from, o.to).map(|u| (c.source_view_id, u)));
        let cut = crossing.zip(level).and_then(|((section, u), level)| {
            let floor = level.floor_elevation.value;
            let next = levels.iter().map(|l| l.floor_elevation.value).filter(|z| *z > floor + TOLERANCE.coincidence_mm).fold(f64::INFINITY, f64::min);
            let on_level = |sill: f64| sill >= floor - TOLERANCE.coincidence_mm && sill < next - TOLERANCE.coincidence_mm;
            sections
                .entry(section)
                .or_insert_with(|| section_openings(&p, section))
                .iter()
                .filter(|(at, sill, _)| (at - u).abs() <= o.wall_width && on_level(*sill))
                .min_by(|a, b| (a.0 - u).abs().total_cmp(&(b.0 - u).abs()))
                .map(|(_, sill, head)| (head - sill, sill - floor))
        });
        let assembly = opening_assembly(d, o.mark.plan).unwrap_or_default();
        let jamb_detail = assembly.iter().find(|a| a.bridge == o.bridge && a.end.is_none()).and_then(|a| a.detail).and_then(name_of);
        rows.push(OpeningRow {
            mark: o.mark.mark,
            kind: if o.door { "Door" } else { "Window" }.to_owned(),
            level: level.map_or_else(|| NONE.to_owned(), |l| l.name.clone()),
            width: dist2(o.from, o.to),
            height: cut.map(|c| c.0),
            sill: cut.map(|c| c.1),
            jamb_detail,
        });
    }
    rows
}

/// Every placed elevation's marks of the openings facing it, in the elevations' document order,
/// then row order. An unreadable drawing has none.
pub fn elevation_marks(d: &Drawing) -> Vec<ElevationMark> {
    let (found, levels) = openings(d);
    if found.is_empty() {
        return Vec::new();
    }
    let rows = opening_rows(d);
    let (Ok(doc), Ok(p)) = (read_document(d), DrawingProject::new(d)) else { return Vec::new() };
    // Each opening in global XY: (row index, level, global midpoint, global out, z, floor).
    let placed: Vec<(usize, usize, Point2, Point2, f64, f64)> = found
        .iter()
        .enumerate()
        .filter_map(|(i, o)| {
            let level = o.level?;
            let floor = levels.get(level)?.floor_elevation.value;
            let plan = p.view(o.mark.plan)?;
            let at = plan_local_to_global_xy(&plan, o.mark.at).ok()?;
            let out = sub2(plan_local_to_global_xy(&plan, add2(o.mark.at, o.mark.out)).ok()?, at);
            let row = rows.get(i)?;
            let z = match (row.sill, row.height) {
                (Some(sill), Some(height)) => floor + sill + height / 2.0,
                _ => floor + UNREAD_MARK_Z_MM,
            };
            Some((i, level, at, out, z, floor))
        })
        .collect();
    let mut marks = Vec::new();
    for v in &doc.views {
        let AnyView::Vertical(e) = v else { continue };
        if e.intent != VerticalIntent::Elevation {
            continue;
        }
        let Some(view) = p.view(e.id) else { continue };
        let Some(host) = view.host_view_id().and_then(|h| p.view(h)) else { continue };
        let Ok(b) = vertical_basis(&view, &host) else { continue };
        let gaze = vertical_gaze_dir(b.c_hat, b.flipped);
        let tol = TOLERANCE.coincidence_mm;
        // (row index, level, u, depth, z, floor) of each facing opening inside the cut line.
        let facing: Vec<(usize, usize, f64, f64, f64, f64)> = placed
            .iter()
            .filter(|(.., out, _, _)| dot2(*out, gaze) <= -std::f64::consts::FRAC_1_SQRT_2)
            .map(|&(i, level, at, _, z, floor)| (i, level, dot2(sub2(at, b.origin_xy), b.c_hat), dot2(sub2(at, b.origin_xy), gaze), z, floor))
            .filter(|&(_, _, u, ..)| u >= -tol && u <= b.length_mm + tol)
            .collect();
        for &(i, level, u, depth, z, floor) in &facing {
            let hidden = facing.iter().any(|&(j, l, u2, d2, ..)| j != i && l == level && (u2 - u).abs() <= HIDE_U_MM && d2 < depth - tol);
            if hidden {
                continue;
            }
            let Some(o) = found.get(i) else { continue };
            marks.push(ElevationMark { view: e.id, mark: o.mark.mark.clone(), at: vertical_uz_to_local(&view, Uz { u, z }), u, floor, row: i });
        }
    }
    marks
}

/// Whole millimetres, never `-0`.
fn mm(v: f64) -> String {
    format!("{:.0}", v.round() + 0.0)
}

/// A row's table cells, in [`HEADER`] order.
fn cells(r: &OpeningRow) -> Vec<String> {
    let opt = |v: Option<f64>| v.map_or_else(|| NONE.to_owned(), mm);
    vec![
        r.mark.clone(),
        r.kind.clone(),
        r.level.clone(),
        mm(r.width),
        opt(r.height),
        opt(r.sill),
        r.jamb_detail.clone().unwrap_or_else(|| NONE.to_owned()),
    ]
}

/// The opening schedule with its top-left corner at `at` (paper mm).
pub fn opening_table(rows: &[OpeningRow], at: Point2) -> Table {
    let rows: Vec<Vec<String>> = rows.iter().map(cells).collect();
    bom::schedule_table(OPENING_TITLE, &HEADER, &COL_WIDTHS, &rows, at)
}

/// Refreshes the Opening Schedule on its set sheet from [`opening_rows`]; no rows touches nothing.
pub(crate) fn refresh_opening_schedule(d: &mut Drawing) -> Result<Option<Refreshed>> {
    let rows = opening_rows(d);
    if rows.is_empty() {
        return Ok(None);
    }
    bom::refresh_table(d, DEFAULT_LAYOUT, OPENING_TITLE, None, &|at| opening_table(&rows, at))
}

/// The handle of the opening schedule on `layout`, and its table.
#[cfg(test)]
fn schedule_on(d: &Drawing, layout: &str) -> Option<(Handle, Table)> {
    titled_on(d, layout, OPENING_TITLE)
}

/// The handle of the table titled `title` on `layout`, and the table.
pub(crate) fn titled_on(d: &Drawing, layout: &str, title: &str) -> Option<(Handle, Table)> {
    d.layout(layout)?.entities.iter().find_map(|e| match &e.kind {
        EntityKind::Table(t) if t.cells.first().and_then(|r| r.first()).is_some_and(|c| c.text == title) => Some((e.handle, t.clone())),
        _ => None,
    })
}

/// A schedule command's `{layout?, at?}`: writes the table `make` builds, titled `title`, on
/// `layout` (default `default_layout`) at `at` (default where it stands, else the sheet's usable
/// top-left), replacing one of that title. One undo step, none when nothing changes. Returns the
/// layout and the table's handle.
pub(crate) fn write_schedule(
    s: &mut Session,
    cmd: &str,
    p: &Value,
    default_layout: &str,
    title: &str,
    make: &dyn Fn(Point2) -> Table,
) -> Result<(String, Handle)> {
    let layout = match p.get("layout").filter(|v| !v.is_null()) {
        None => default_layout.to_owned(),
        Some(v) => v.as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(cmd, "`layout` must be a layout name"))?.to_owned(),
    };
    let at = match p.get("at").filter(|v| !v.is_null()) {
        None => None,
        Some(_) => Some(point_param(cmd, p, "at")?),
    };
    // Build on a copy and assign it back once: one undo step, and none when nothing changes.
    let mut d = s.doc()?.clone();
    let mut changed = bom::refresh_table(&mut d, &layout, title, None, make)?.is_some();
    let (handle, table) = titled_on(&d, &layout, title).ok_or_else(|| other("The schedule was not written"))?;
    if let Some(at) = at.filter(|at| (table.insert.x, table.insert.y) != (at.x, at.y)) {
        let l = d.layouts.iter_mut().find(|l| l.name == layout).ok_or_else(|| other("The schedule's layout is gone"))?;
        l.entities.modify(handle, |e| {
            if let EntityKind::Table(t) = &mut e.kind {
                t.insert = Vec3::new(at.x, at.y, 0.0);
            }
        });
        changed = true;
    }
    if changed {
        *s.doc_mut()? = d;
    }
    Ok((layout, handle))
}

pub(crate) fn openingschedule(s: &mut Session, p: &Value) -> Result<Value> {
    let rows = opening_rows(s.doc()?);
    if rows.is_empty() {
        return Err(other("No openings: no placed plan has a door or window spanning a wall gap"));
    }
    let (layout, handle) = write_schedule(s, "openingschedule", p, DEFAULT_LAYOUT, OPENING_TITLE, &|at| opening_table(&rows, at))?;
    let message = format!("{OPENING_TITLE} on \"{layout}\": {} openings.", rows.len());
    let rows: Vec<Value> = rows
        .iter()
        .map(|r| json!({ "mark": r.mark, "type": r.kind, "level": r.level, "width": r.width, "height": r.height, "sill": r.sill, "jambDetail": r.jamb_detail }))
        .collect();
    Ok(json!({ "layout": layout, "handle": format!("{:X}", handle.0), "rows": rows, "message": message }))
}

/// OPENINGSCHEDULE: `Enter layout for the opening schedule <A602>:`.
struct OpeningSchedule;

impl Interactive for OpeningSchedule {
    fn name(&self) -> &'static str {
        "openingschedule"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter layout for the opening schedule", Accept::TEXT).default(DEFAULT_LAYOUT.to_owned())
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(layout) = name_input(i, DEFAULT_LAYOUT) else { return Ok(Step::Continue) };
        match openingschedule(s, &json!({ "layout": layout })) {
            Ok(r) => s.echo(r["message"].as_str().unwrap_or_default().to_owned()),
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

pub(crate) fn openingschedule_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(OpeningSchedule))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::annotations::tests::mark_texts;
    use crate::schema::{annotation_block_name, view_block_name};
    use cadcraft_engine::cmd::file::{IoHooks, set_io};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    fn id(v: &Value) -> T<Id> {
        Ok(Id(v["viewId"].as_u64().ok_or("no id")?))
    }

    /// A fresh LAMB project, its observers installed and its plan edited.
    pub(crate) fn project() -> T<(Session, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = id(&s.execute("lambproject", &json!({}))?)?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        Ok((s, plan))
    }

    /// Makes layer `name` current, adding it when absent.
    pub(crate) fn layer(s: &mut Session, name: &str) -> R {
        if s.doc()?.layers.iter().any(|l| l.name == name) {
            s.execute("layer.current", &json!({ "name": name }))?;
        } else {
            s.execute("layer.new", &json!({ "name": name, "current": true }))?;
        }
        Ok(())
    }

    /// Draws a 5000 × 4000, 200-thick room on the edited plan: a `(lo, hi)` gap in its south wall
    /// (y 0..200), and one in its north wall (y 3800..4000) when given.
    pub(crate) fn walls(s: &mut Session, south: (f64, f64), north: Option<(f64, f64)>) -> R {
        walls_at(s, 0.0, south, north)
    }

    /// [`walls`] moved `dy` north.
    fn walls_at(s: &mut Session, dy: f64, south: (f64, f64), north: Option<(f64, f64)>) -> R {
        layer(s, "WALL-EXT")?;
        for (y0, x0, x1, y1) in [(dy, 0.0, 5000.0, 4000.0 + dy), (200.0 + dy, 200.0, 4800.0, 3800.0 + dy)] {
            match north {
                Some(n) => {
                    s.execute("pline", &json!({ "vertices": [[south.0, y0], [x0, y0], [x0, y1], [n.0, y1]] }))?;
                    s.execute("pline", &json!({ "vertices": [[n.1, y1], [x1, y1], [x1, y0], [south.1, y0]] }))?;
                }
                None => {
                    s.execute("pline", &json!({ "vertices": [[south.0, y0], [x0, y0], [x0, y1], [x1, y1], [x1, y0], [south.1, y0]] }))?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn opening(s: &mut Session, layer: &str, a: (f64, f64), b: (f64, f64)) -> R {
        self::layer(s, layer)?;
        s.execute("line", &json!({ "points": [[a.0, a.1], [b.0, b.1]] }))?;
        Ok(())
    }

    /// Level 1: a window 2000..3000 in the south wall, a door 1000..1900 in the north. `First` at
    /// 3000: a door 500..1400 in the south wall. Returns (session, ground, first), ground edited.
    fn two_levels() -> T<(Session, Id, Id)> {
        let (mut s, ground) = project()?;
        walls(&mut s, (2000.0, 3000.0), Some((1000.0, 1900.0)))?;
        opening(&mut s, "WINDOW", (2000.0, 100.0), (3000.0, 100.0))?;
        opening(&mut s, "door-1", (1000.0, 3900.0), (1900.0, 3900.0))?;
        let first = id(&s.execute("newoffsetplan", &json!({ "mode": "offset", "fromViewId": ground.0, "floorToFloorMm": 3000, "name": "First" }))?)?;
        s.execute("editview", &json!({ "viewId": first.0 }))?;
        walls(&mut s, (500.0, 1400.0), None)?;
        opening(&mut s, "door-1", (500.0, 100.0), (1400.0, 100.0))?;
        s.execute("editview", &json!({ "viewId": ground.0 }))?;
        Ok((s, ground, first))
    }

    /// A section cut across the south wall at x = 2500 (the wall's centreline at u 1100), with each
    /// `(sill, head)` opening drawn in the wall from z 0 up to `top`.
    pub(crate) fn section_through(s: &mut Session, plan: Id, windows: &[(f64, f64)], top: f64) -> T<Id> {
        let r = s.execute(
            "newsectionline",
            &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 2500, "y": -1000}, "cutTo": {"x": 2500, "y": 1000} }),
        )?;
        let sec = id(&r)?;
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        layer(s, "WALL-EXT")?;
        let mut z = 0.0;
        for &(sill, head) in windows.iter().chain([&(top, top)]) {
            for u in [1000.0, 1200.0] {
                s.execute("pline", &json!({ "vertices": [[u, z], [u, sill]] }))?;
            }
            z = head;
        }
        layer(s, "WINDOW")?;
        for &(sill, head) in windows {
            s.execute("pline", &json!({ "vertices": [[1100.0, sill], [1100.0, head]] }))?;
        }
        let hs: Vec<String> = s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.iter().map(|e| e.handle.hex()).collect();
        s.execute("setrole", &json!({ "handles": hs, "role": "cut" }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        Ok(sec)
    }

    fn rows(s: &Session) -> T<Vec<Vec<String>>> {
        Ok(opening_rows(s.doc()?).iter().map(cells).collect())
    }

    /// Every cell of the A602 table below its header row.
    fn body(s: &Session) -> T<Vec<Vec<String>>> {
        let (_, t) = schedule_on(s.doc()?, DEFAULT_LAYOUT).ok_or("no schedule")?;
        Ok(t.cells[2..].iter().map(|r| r.iter().map(|c| c.text.clone()).collect()).collect())
    }

    fn undo_len(s: &Session) -> T<usize> {
        Ok(s.state()?.undo.len())
    }

    fn laid(r: &Value) -> Vec<(String, String)> {
        let sheets = r["sheets"].as_array().map(Vec::as_slice).unwrap_or_default();
        sheets.iter().map(|x| (x["layout"].as_str().unwrap_or_default().to_owned(), x["group"].as_str().unwrap_or_default().to_owned())).collect()
    }

    fn erase_view(s: &mut Session, view: Id, keep_walls: bool) -> R {
        let block = s.doc()?.block(&view_block_name(view)).ok_or("no block")?;
        let gone: Vec<String> = block.entities.iter().filter(|e| !keep_walls || e.common.layer != "WALL-EXT").map(|e| e.handle.hex()).collect();
        s.execute("erase", &json!({ "handles": gone }))?;
        Ok(())
    }

    #[test]
    fn rows_marks_and_widths_run_by_level_and_a_section_fills_only_the_window_it_cuts() -> R {
        let (mut s, ground, _first) = two_levels()?;
        section_through(&mut s, ground, &[(900.0, 2100.0)], 3000.0)?;
        assert_eq!(
            rows(&s)?,
            [
                ["D01", "Door", "Level 1", "900", "—", "—", "—"],
                ["W01", "Window", "Level 1", "1000", "1200", "900", "—"],
                ["D02", "Door", "First", "900", "—", "—", "—"],
            ]
        );
        Ok(())
    }

    #[test]
    fn a_window_above_another_in_one_cut_is_read_at_its_own_level() -> R {
        let (mut s, ground, first) = two_levels()?;
        s.execute("editview", &json!({ "viewId": first.0 }))?;
        erase_view(&mut s, first, false)?;
        walls(&mut s, (2000.0, 3000.0), None)?;
        opening(&mut s, "WINDOW", (2000.0, 100.0), (3000.0, 100.0))?;
        s.execute("editview", &json!({ "viewId": ground.0 }))?;
        section_through(&mut s, ground, &[(900.0, 2100.0), (3600.0, 5000.0)], 6000.0)?;
        let got = rows(&s)?;
        assert_eq!(got[1], ["W01", "Window", "Level 1", "1000", "1200", "900", "—"]);
        assert_eq!(got[2], ["W02", "Window", "First", "1000", "1400", "600", "—"]);
        Ok(())
    }

    #[test]
    fn a_tagged_opening_names_its_jamb_detail() -> R {
        let (mut s, plan) = crate::assembly::tests::window_room(false, true)?;
        let level =
            s.execute("level", &json!({ "mode": "create", "name": "Level 1", "floorElevationMm": 0 }))?["levelId"].as_u64().ok_or("no level")?;
        let t = lamb_core::geometry::IDENTITY_TRANSFORM;
        s.execute("placeview", &json!({ "viewId": plan.0, "kind": "plan", "levelId": level, "cutElevation": 1200, "transform": t }))?;
        assert_eq!(rows(&s)?, [["W01", "Window", "Level 1", "1000", "—", "—", "JD"]]);
        Ok(())
    }

    #[test]
    fn sheetset_makes_a602_lists_it_and_a_rerun_is_a_no_op() -> R {
        let (mut s, _ground, _first) = two_levels()?;
        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert!(laid(&r).contains(&("A602".to_owned(), "schedule".to_owned())), "{r}");
        assert_eq!(body(&s)?.len(), 3);
        let (_, t) = schedule_on(s.doc()?, DEFAULT_LAYOUT).ok_or("no schedule")?;
        assert_eq!(t.cells[0][0].text, OPENING_TITLE);
        assert_eq!(t.cells[1].iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), HEADER);
        let l = s.doc()?.layout("A001").ok_or("no list")?;
        let list = l.entities.iter().find_map(|e| if let EntityKind::Table(t) = &e.kind { Some(t.clone()) } else { None }).ok_or("no table")?;
        assert!(list.cells.iter().any(|r| r[0].text == "A602" && r[1].text == OPENING_TITLE), "{list:?}");

        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!((undo_len(&s)?, laid(&r)), (n, vec![]), "{r}");
        Ok(())
    }

    #[test]
    fn a_moved_window_updates_only_a602_and_the_plans_mark() -> R {
        let (mut s, ground, _first) = two_levels()?;
        s.execute("sheetset", &json!({}))?;
        assert!(mark_texts(&s, ground)?.iter().any(|(m, x, y)| m == "W01" && *x == 2500.0 && *y == -300.0), "{:?}", mark_texts(&s, ground)?);

        erase_view(&mut s, ground, false)?;
        walls(&mut s, (2200.0, 3400.0), Some((1000.0, 1900.0)))?;
        opening(&mut s, "WINDOW", (2200.0, 100.0), (3400.0, 100.0))?;
        opening(&mut s, "door-1", (1000.0, 3900.0), (1900.0, 3900.0))?;
        assert!(mark_texts(&s, ground)?.iter().any(|(m, x, y)| m == "W01" && *x == 2800.0 && *y == -300.0), "{:?}", mark_texts(&s, ground)?);

        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(laid(&r), [("A602".to_owned(), "schedule".to_owned())], "{r}");
        assert_eq!(body(&s)?[1][3], "1200");
        Ok(())
    }

    #[test]
    fn no_openings_makes_no_a602_and_one_undo_takes_a_new_one_away() -> R {
        let (mut s, _plan) = project()?;
        walls(&mut s, (2000.0, 3000.0), None)?;
        let n = undo_len(&s)?;
        assert!(s.execute("openingschedule", &json!({})).is_err(), "nothing to schedule");
        assert_eq!(undo_len(&s)?, n);
        s.execute("sheetset", &json!({}))?;
        assert!(s.doc()?.layout(DEFAULT_LAYOUT).is_none());

        opening(&mut s, "WINDOW", (2000.0, 100.0), (3000.0, 100.0))?;
        s.execute("sheetset", &json!({}))?;
        assert_eq!(body(&s)?.len(), 1);
        s.undo()?;
        assert!(s.doc()?.layout(DEFAULT_LAYOUT).is_none(), "one undo takes the sheet away");
        Ok(())
    }

    #[test]
    fn an_existing_schedule_stays_when_the_last_opening_goes() -> R {
        let (mut s, ground, first) = two_levels()?;
        s.execute("sheetset", &json!({}))?;
        let before = body(&s)?;
        for view in [ground, first] {
            s.execute("editview", &json!({ "viewId": view.0 }))?;
            erase_view(&mut s, view, true)?;
        }
        assert!(rows(&s)?.is_empty());
        s.execute("sheetset", &json!({}))?;
        assert_eq!(body(&s)?, before);
        Ok(())
    }

    #[test]
    fn the_pdf_grows_by_the_schedule_sheet() -> R {
        set_io(IoHooks {
            read: |b, n| cadcraft_io::read(b, n).map_err(|e| e.to_string()),
            write: |d, n| cadcraft_io::write(d, n).map_err(|e| e.to_string()),
            plot: Some(|d, space, opts| cadcraft_io::plotter::plot(d, space, opts).map_err(|e| e.to_string())),
        });
        let (mut s, _plan) = project()?;
        walls(&mut s, (2000.0, 3000.0), None)?;
        let dir = std::env::temp_dir().join(format!("lamb-openings-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("set.pdf").to_string_lossy().into_owned();
        let before = s.execute("sheetset", &json!({ "pdf": path }))?["pdf"]["pages"].as_u64().ok_or("no pages")?;
        opening(&mut s, "WINDOW", (2000.0, 100.0), (3000.0, 100.0))?;
        let after = s.execute("sheetset", &json!({ "pdf": path }))?["pdf"]["pages"].as_u64().ok_or("no pages")?;
        assert_eq!(after, before + 1);
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn the_plan_shows_each_mark_outside_in_its_annotation_block_not_its_view_block() -> R {
        let (s, ground, first) = two_levels()?;
        assert_eq!(mark_texts(&s, ground)?, [("D01".to_owned(), 1450.0, 4300.0), ("W01".to_owned(), 2500.0, -300.0)]);
        assert_eq!(mark_texts(&s, first)?, [("D02".to_owned(), 950.0, -300.0)]);
        let d = s.doc()?;
        let view = d.block(&view_block_name(ground)).ok_or("no block")?;
        assert!(!view.entities.iter().any(|e| matches!(&e.kind, EntityKind::Text(t) if t.value == "D01")), "the view block has no mark");
        let ann = d.block(&annotation_block_name(ground)).ok_or("no annotation")?;
        let h = ann.entities.iter().find_map(|e| if let EntityKind::Text(t) = &e.kind { Some(t.height) } else { None }).ok_or("no text")?;
        assert_eq!(h, crate::annotations::CUT_TEXT_MM);
        Ok(())
    }

    #[test]
    fn an_undecided_outside_takes_the_left_of_the_wall_run() -> R {
        let (mut s, plan) = project()?;
        s.execute("layer.new", &json!({ "name": "WALL-INT", "current": true }))?;
        for y in [0, 100] {
            s.execute("line", &json!({ "points": [[0, y], [1000, y]] }))?;
            s.execute("line", &json!({ "points": [[2000, y], [3000, y]] }))?;
        }
        opening(&mut s, "DOOR", (1000.0, 50.0), (2000.0, 50.0))?;
        let d = s.doc()?;
        let p = DrawingProject::new(d)?;
        let (graph, sides) = graph_and_sides(&p, plan);
        let b = graph.bridges.first().ok_or("no bridge")?;
        assert_eq!(sides.runs.get(b.a.0).and_then(outside_on_left), None);
        let left = run_left(&graph.runs[b.a.0], b.a.1);
        let marks = plan_marks(d);
        assert_eq!(marks.len(), 1);
        assert!(dot2(marks[0].out, left) > 0.999, "{marks:?} {left:?}");
        assert_eq!(marks[0].at, Point2 { x: 1500.0, y: 50.0 });
        Ok(())
    }

    #[test]
    fn the_command_writes_where_asked_in_one_undo_step_and_prompts_for_its_sheet() -> R {
        let (mut s, _ground, _first) = two_levels()?;
        let n = undo_len(&s)?;
        let r = s.execute("openingschedule", &json!({ "at": {"x": 50, "y": 100} }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        assert_eq!(r["message"], "Opening Schedule on \"A602\": 3 openings.");
        assert_eq!((r["rows"][1]["mark"].as_str(), r["rows"][1]["height"].is_null()), (Some("W01"), true));
        let (_, t) = schedule_on(s.doc()?, DEFAULT_LAYOUT).ok_or("no schedule")?;
        assert_eq!((t.insert.x, t.insert.y), (50.0, 100.0));
        let n = undo_len(&s)?;
        s.execute("openingschedule", &json!({}))?;
        assert_eq!(undo_len(&s)?, n, "nothing changed");
        s.start("osch")?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("A602"));
        s.input(Input::Text("Sched".into()))?;
        assert!(s.doc()?.layout("Sched").is_some());
        Ok(())
    }

    /// Elevation `name` on `plan`, cut from `from` to `to` (it looks to the left of that direction).
    pub(crate) fn elevation(s: &mut Session, plan: Id, name: &str, from: (f64, f64), to: (f64, f64)) -> T<Id> {
        let r = s.execute(
            "newelevationline",
            &json!({ "name": name, "hostViewId": plan.0, "cutFrom": {"x": from.0, "y": from.1}, "cutTo": {"x": to.0, "y": to.1} }),
        )?;
        id(&r)
    }

    /// A room with window W01 at x 2000..3000 in its south wall, the plan edited.
    pub(crate) fn window_south() -> T<(Session, Id)> {
        let (mut s, plan) = project()?;
        walls(&mut s, (2000.0, 3000.0), None)?;
        opening(&mut s, "WINDOW", (2000.0, 100.0), (3000.0, 100.0))?;
        Ok((s, plan))
    }

    #[test]
    fn an_elevation_marks_the_window_facing_it_at_1200_or_at_its_section_read_centre() -> R {
        let (mut s, plan) = window_south()?;
        let south = elevation(&mut s, plan, "South", (-500.0, -1000.0), (5500.0, -1000.0))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        assert_eq!(elevation_marks(s.doc()?).iter().map(|m| (m.view, m.mark.as_str())).collect::<Vec<_>>(), [(south, "W01")]);
        assert_eq!(mark_texts(&s, south)?, [("W01".to_owned(), 3000.0, UNREAD_MARK_Z_MM)], "in the elevation's annotation block");
        assert!(crate::annotations::settled(s.doc()?)?, "a second pass changes nothing");
        let sec = section_through(&mut s, plan, &[(900.0, 2100.0)], 3000.0)?;
        assert_eq!(mark_texts(&s, south)?, [("W01".to_owned(), 3000.0, 1500.0)]);
        assert!(mark_texts(&s, sec)?.is_empty(), "a section gets no marks");
        assert!(crate::annotations::settled(s.doc()?)?);
        Ok(())
    }

    #[test]
    fn an_elevation_from_inside_one_off_the_window_and_one_square_to_its_wall_mark_nothing() -> R {
        let (mut s, plan) = window_south()?;
        elevation(&mut s, plan, "Inside", (5000.0, 2000.0), (0.0, 2000.0))?;
        elevation(&mut s, plan, "Short", (3500.0, -1000.0), (5500.0, -1000.0))?;
        elevation(&mut s, plan, "West", (-1000.0, 5000.0), (-1000.0, -1000.0))?;
        assert_eq!(elevation_marks(s.doc()?), []);
        Ok(())
    }

    #[test]
    fn a_window_behind_a_nearer_one_within_300_in_u_is_not_marked() -> R {
        let (mut s, plan) = window_south()?;
        walls_at(&mut s, 6000.0, (2100.0, 3100.0), None)?;
        opening(&mut s, "WINDOW", (2100.0, 6100.0), (3100.0, 6100.0))?;
        let south = elevation(&mut s, plan, "South", (-500.0, -1000.0), (5500.0, -1000.0))?;
        let marks = elevation_marks(s.doc()?);
        assert_eq!(opening_rows(s.doc()?).len(), 2);
        assert_eq!(
            marks,
            [ElevationMark { view: south, mark: "W01".to_owned(), at: Point2 { x: 3000.0, y: UNREAD_MARK_Z_MM }, u: 3000.0, floor: 0.0, row: 0 }]
        );
        Ok(())
    }
}

//! BOMTABLE (LAMB T601): a framing bill of materials counted from what the assembly plans lay,
//! per floor level, written as a `Table` on a sheet layout.
//!
//! Computed on demand, never stored: for every placed plan and every section or elevation view,
//! [`assembly_lines`], [`corner_assembly`], [`opening_assembly`] and [`end_assembly`] (a call that errs adds nothing
//! for that view). Each repeat copy of a run (`lines[bands..]` chunked by `copies`) is one of the
//! item named by its first line's layer (`STUD`, `JOIST`); a placed corner detail is one
//! `"<detail> (corner)"`; a placed opening detail is `"<detail> (jamb)"`, two when it is laid at
//! both jambs (a plan), one for a single end (a section's head or sill); a placed termination at a
//! wall's free end is one `"<detail> (end)"` (LAMB T617).
//!
//! Members drawn inside a placed corner, opening or end count too (LAMB T638). A drawing's member
//! layers are those naming a repeat copy in any view, or a repeat's first member in any detail, so
//! a project whose only studs are in corners still counts. In a placement's `lines`, those on a
//! member layer are grouped per layer into connected components, ends joining within
//! `TOLERANCE.coincidence_mm` (a drawn rectangle is one, a lone line is one), and each component
//! is one of the item named by its layer, added to the same row as the run copies. A plan's
//! opening `lines` hold both jambs, so its components need no doubling.
//!
//! Bands have a running length (LAMB T649), plans only: in a section a band's length is its height
//! or its run across the cut. For each laid run of a placed plan, each layer among its bands
//! (`lines[..bands]`) adds the length of its longest line in that run, once: a band is drawn as
//! face lines, so summing would double it, and two separate bands on one layer in one run therefore
//! count once. An arc line's length is its radius times its sweep. Corners, jambs and ends add no
//! length: their pieces are short and counted as items. A layer that has both members and a band
//! gets its count and its length on one row.
//!
//! A plan's level is its placement's level; a floating plan is skipped. In a vertical view a run
//! belongs to the level with the greatest floor elevation at or below its centreline's mid-point
//! height (view-local y is absolute z), a corner or opening to the one below the mid-point of its
//! placed lines; none → level `—`. Rows are keyed (level, item, view), so two sections through
//! one floor show as two rows, each naming its view — nothing is silently counted twice. Sorted
//! by level order, then item, then view document order.
//!
//! `bomtable {layout?, at?}` (`bom`): the table (`Level | Item | Count | Length | View`, titled
//! `Framing BOM`, a zero count or length left blank, a length written `12.4 m`) on layout `layout` (default `A601`, the BOM's sheet in the set
//! ([`crate::sheetset`]), created without viewports when absent) at
//! paper point `at` (default the top-left corner of the sheet's
//! [`crate::sheets::usable_area`]). A table on that
//! layout titled `Framing BOM` is replaced; one undo step. Nothing to count is refused, nothing
//! changed. Returns `{layout, handle, rows: [{level, item, count, lengthMm, view}], message}`.

use std::collections::{HashMap, HashSet};

use cadcraft_doc::{Common, Drawing, EntityKind, Layout, Space, Table, TableCell};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::Vec3;
use cadcraft_render::Sheet;
use lamb_core::derivation::assembly::AssemblyLine;
use lamb_core::derivation::projection::vertical_local_to_uz;
use lamb_core::derivation::read::view_ref;
use lamb_core::derivation::walls::RunCurve;
use lamb_core::geometry::Point2;
use lamb_core::geometry::polyline::sweep_angle;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AnyView, Id, Level, Placement};
use serde_json::{Value, json};

use crate::assembly::{RunAssembly, assembly_lines, corner_assembly, end_assembly, opening_assembly};
use crate::commands::{bad, name_input, other, point_param};
use crate::detailtags::graph_and_sides;
use crate::project::DrawingProject;
use crate::repeats::detail_repeats;
use crate::schema::{read_document, view_block_name};
use crate::sheets::{sheet_page, usable_area};

pub const BOM_TITLE: &str = "Framing BOM";
pub(crate) const DEFAULT_LAYOUT: &str = "A601";
const NO_LEVEL: &str = "—";
const TEXT_HEIGHT: f64 = 2.5;
const ROW_HEIGHT: f64 = 7.0;
const COL_WIDTHS: [f64; 5] = [30.0, 45.0, 15.0, 20.0, 45.0];

/// One BOM row: `count` of `item` and `length_mm` of it as a continuous band (0 when there is
/// none) on `level`, as laid in view `view`.
#[derive(Clone, Debug, PartialEq)]
pub struct BomRow {
    pub level: String,
    pub item: String,
    pub count: usize,
    pub length_mm: f64,
    pub view: String,
}

/// The level whose floor is the highest at or below `z`.
fn level_at(levels: &[Level], z: f64) -> Option<&Level> {
    levels
        .iter()
        .filter(|l| l.floor_elevation.value <= z + TOLERANCE.coincidence_mm)
        .max_by(|a, b| a.floor_elevation.value.total_cmp(&b.floor_elevation.value))
}

fn curve_mid(c: &RunCurve) -> Point2 {
    match *c {
        RunCurve::Line { from, to } => Point2 { x: (from.x + to.x) / 2.0, y: (from.y + to.y) / 2.0 },
        RunCurve::Arc { center, radius, start_angle, sweep } => {
            let a = start_angle + sweep / 2.0;
            Point2 { x: center.x + radius * a.cos(), y: center.y + radius * a.sin() }
        }
    }
}

/// The centre of `lines`' bounding box (view-local).
fn lines_mid(lines: &[AssemblyLine]) -> Point2 {
    let ys = lines.iter().flat_map(|l| [l.from.y, l.to.y]);
    let (lo, hi) = (ys.clone().fold(f64::INFINITY, f64::min), ys.fold(f64::NEG_INFINITY, f64::max));
    let xs = lines.iter().flat_map(|l| [l.from.x, l.to.x]);
    let (xl, xh) = (xs.clone().fold(f64::INFINITY, f64::min), xs.fold(f64::NEG_INFINITY, f64::max));
    Point2 { x: (xl + xh) / 2.0, y: (lo + hi) / 2.0 }
}

/// The layer each repeat copy of `r` is counted under: its first line's.
fn copy_layers(r: &RunAssembly) -> impl Iterator<Item = &str> {
    let mut at = r.bands;
    r.copies.iter().filter_map(move |&n| {
        let first = r.lines.get(at..at + n).and_then(|c| c.first());
        at += n;
        first.map(|l| l.layer.as_str())
    })
}

/// The length of a laid line: a straight one's distance, an arc's radius times its sweep.
fn line_length(l: &AssemblyLine) -> f64 {
    let Some(arc) = l.arc else { return (l.to.x - l.from.x).hypot(l.to.y - l.from.y) };
    let angle = |p: Point2| (p.y - arc.center.y).atan2(p.x - arc.center.x);
    arc.radius * sweep_angle(angle(l.from), angle(l.to), arc.ccw).abs()
}

/// Each band layer of `r` with the length of its longest line in the run.
fn band_lengths(r: &RunAssembly) -> HashMap<&str, f64> {
    let mut lengths: HashMap<&str, f64> = HashMap::new();
    for l in r.lines.get(..r.bands).unwrap_or_default() {
        let len = lengths.entry(l.layer.as_str()).or_insert(0.0);
        *len = len.max(line_length(l));
    }
    lengths
}

/// The layers that hold framing members: those naming a repeat copy in `laid`, or a repeat's first
/// member in a detail of `views`.
fn member_layers(d: &Drawing, views: &[AnyView], laid: &[Option<Vec<RunAssembly>>]) -> HashSet<String> {
    let mut layers: HashSet<String> = laid.iter().flatten().flatten().flat_map(copy_layers).map(str::to_owned).collect();
    for v in views.iter().filter(|v| matches!(v, AnyView::Detail(_))) {
        let Some(block) = d.block(&view_block_name(v.id())) else { continue };
        for r in detail_repeats(d, v.id()).unwrap_or_default() {
            let first = r.members.first().and_then(|h| block.entities.iter().find(|e| e.handle == *h));
            layers.extend(first.map(|e| e.common.layer.clone()));
        }
    }
    layers
}

/// How many connected components `lines` hold on each of the `members` layers.
fn member_counts<'a>(lines: &'a [AssemblyLine], members: &HashSet<String>) -> HashMap<&'a str, usize> {
    let near = |a: Point2, b: Point2| (a.x - b.x).hypot(a.y - b.y) <= TOLERANCE.coincidence_mm;
    let touches = |a: &AssemblyLine, b: &AssemblyLine| a.layer == b.layer && [a.from, a.to].into_iter().any(|p| near(p, b.from) || near(p, b.to));
    let mut counts = HashMap::new();
    let mut rest: Vec<&AssemblyLine> = lines.iter().filter(|l| members.contains(&l.layer)).collect();
    while let Some(seed) = rest.pop() {
        *counts.entry(seed.layer.as_str()).or_insert(0) += 1;
        let mut open = vec![seed];
        while let Some(l) = open.pop() {
            let (touching, others): (Vec<_>, Vec<_>) = std::mem::take(&mut rest).into_iter().partition(|o| touches(l, o));
            open.extend(touching);
            rest = others;
        }
    }
    counts
}

/// Every framing count and band length the assembly plans lay, per (level, item, view), sorted.
pub fn bom_rows(d: &Drawing) -> Vec<BomRow> {
    let Ok(doc) = read_document(d) else { return Vec::new() };
    let Ok(p) = DrawingProject::new(d) else { return Vec::new() };
    let detail_name = |id: Option<Id>| {
        let id = id.unwrap_or(Id(0));
        doc.views.iter().find(|v| v.id() == id).map_or_else(|| format!("detail {}", id.0), |v| v.name().to_owned())
    };
    let laid: Vec<Option<Vec<RunAssembly>>> = doc.views.iter().map(|v| assembly_lines(d, v.id()).ok()).collect();
    let members = member_layers(d, &doc.views, &laid);
    // (level name, item, view index) → (level order, count, length).
    let mut counts: HashMap<(String, String, usize), (f64, usize, f64)> = HashMap::new();
    let mut bump = |level: Option<&Level>, item: String, view: usize, n: usize, length: f64| {
        let (order, name) = level.map_or((f64::INFINITY, NO_LEVEL.to_owned()), |l| (l.order, l.name.clone()));
        let entry = counts.entry((name, item, view)).or_insert((order, 0, 0.0));
        entry.1 += n;
        entry.2 += length;
    };
    for (vi, v) in doc.views.iter().enumerate() {
        let id = v.id();
        // The level of a point in this view's local space: a plan's fixed one, or by height.
        let (fixed, vertical) = match v {
            AnyView::Plan(pv) => match &pv.placement {
                Some(Placement::Plan { level_id, .. }) => (doc.levels.iter().find(|l| l.id == *level_id), None),
                _ => continue,
            },
            AnyView::Vertical(_) => (None, Some(view_ref(v))),
            AnyView::Detail(_) => continue,
        };
        let levels = &doc.levels;
        let level_of = |pt: Point2| match &vertical {
            Some(vr) => level_at(levels, vertical_local_to_uz(vr, pt).z),
            None => fixed,
        };
        if let Some(runs) = laid.get(vi).and_then(Option::as_ref) {
            let (graph, _) = graph_and_sides(&p, id);
            for r in runs {
                let level = graph.runs.get(r.run).and_then(|run| level_of(curve_mid(&run.centreline)));
                for layer in copy_layers(r) {
                    bump(level, layer.to_owned(), vi, 1, 0.0);
                }
                if vertical.is_none() {
                    for (layer, length) in band_lengths(r) {
                        bump(level, layer.to_owned(), vi, 0, length);
                    }
                }
            }
        }
        // A placed detail: its own row, plus the members drawn inside it, at one level.
        let mut place = |item: String, n: usize, lines: &[AssemblyLine]| {
            let level = level_of(lines_mid(lines));
            bump(level, item, vi, n, 0.0);
            for (layer, c) in member_counts(lines, &members) {
                bump(level, layer.to_owned(), vi, c, 0.0);
            }
        };
        if let Ok(corners) = corner_assembly(d, id) {
            for c in corners.iter().filter(|c| c.detail.is_some() && c.skipped.is_none() && !c.lines.is_empty()) {
                place(format!("{} (corner)", detail_name(c.detail)), 1, &c.lines);
            }
        }
        if let Ok(openings) = opening_assembly(d, id) {
            for o in openings.iter().filter(|o| o.detail.is_some() && o.skipped.is_none() && !o.lines.is_empty()) {
                let n = if o.end.is_none() { 2 } else { 1 };
                place(format!("{} (jamb)", detail_name(o.detail)), n, &o.lines);
            }
        }
        if let Ok(ends) = end_assembly(d, id) {
            for e in ends.iter().filter(|e| e.detail.is_some() && e.skipped.is_none() && !e.lines.is_empty()) {
                place(format!("{} (end)", detail_name(e.detail)), 1, &e.lines);
            }
        }
    }
    let mut rows: Vec<((f64, String, String, usize), (usize, f64))> = counts
        .into_iter()
        .filter(|(_, (_, n, length))| *n > 0 || *length > 0.0)
        .map(|((level, item, view), (order, n, length))| ((order, level, item, view), (n, length)))
        .collect();
    rows.sort_by(|a, b| a.0.0.total_cmp(&b.0.0).then_with(|| a.0.1.cmp(&b.0.1)).then_with(|| a.0.2.cmp(&b.0.2)).then_with(|| a.0.3.cmp(&b.0.3)));
    rows.into_iter()
        .map(|((_, level, item, view), (count, length_mm))| BomRow {
            level,
            item,
            count,
            length_mm,
            view: doc.views.get(view).map(|v| v.name().to_owned()).unwrap_or_default(),
        })
        .collect()
}

/// A schedule table with its top-left corner at `at` (paper mm): a merged title row over a header
/// row over `rows`.
pub(crate) fn schedule_table(title: &str, header: &[&str], col_widths: &[f64], rows: &[Vec<String>], at: Point2) -> Table {
    let cell = |s: &str| TableCell { text: s.to_owned(), ..TableCell::default() };
    let mut title_row = vec![TableCell::default(); col_widths.len()];
    if let Some(c) = title_row.first_mut() {
        *c = TableCell { text: title.to_owned(), merged: Some((1, col_widths.len() as u32)), ..TableCell::default() };
    }
    let mut cells = vec![title_row, header.iter().map(|s| cell(s)).collect()];
    cells.extend(rows.iter().map(|r| r.iter().map(|s| cell(s)).collect()));
    Table {
        insert: Vec3::new(at.x, at.y, 0.0),
        col_widths: col_widths.to_vec(),
        row_heights: vec![ROW_HEIGHT; cells.len()],
        cells,
        text_height: TEXT_HEIGHT,
        title: true,
        header: true,
        ..Table::default()
    }
}

/// The BOM table with its top-left corner at `at` (paper mm).
pub fn bom_table(rows: &[BomRow], at: Point2) -> Table {
    let count = |n: usize| if n > 0 { n.to_string() } else { String::new() };
    let length = |mm: f64| if mm > 0.0 { format!("{:.1} m", mm / 1000.0) } else { String::new() };
    let rows: Vec<Vec<String>> =
        rows.iter().map(|r| vec![r.level.clone(), r.item.clone(), count(r.count), length(r.length_mm), r.view.clone()]).collect();
    schedule_table(BOM_TITLE, &["Level", "Item", "Count", "Length", "View"], &COL_WIDTHS, &rows, at)
}

/// Whether `e` is a table titled `title`.
fn is_titled(e: &cadcraft_doc::Entity, title: &str) -> bool {
    matches!(&e.kind, EntityKind::Table(t) if t.cells.first().and_then(|r| r.first()).is_some_and(|c| c.text == title))
}

/// The table titled `title` on `layout`.
fn titled_table<'a>(d: &'a Drawing, layout: &str, title: &str) -> Option<&'a Table> {
    d.layout(layout)?.entities.iter().filter(|e| is_titled(e, title)).find_map(|e| match &e.kind {
        EntityKind::Table(t) => Some(t),
        _ => None,
    })
}

/// Adds `layout`, without viewports and on the project's sheet paper, unless it exists.
pub(crate) fn ensure_sheet_layout(d: &mut Drawing, layout: &str) -> Result<()> {
    if d.layout(layout).is_none() {
        let page = sheet_page(&read_document(d).map_err(other)?);
        let tab_order = d.layouts.iter().map(|l| l.tab_order).max().map_or(0, |t| t + 1);
        d.layouts.push(Layout { page, ..Layout::new(layout, tab_order) });
    }
    Ok(())
}

/// `layout`'s sheet, by its own page or else the project's sheet paper.
fn layout_sheet(d: &Drawing, layout: &str) -> Result<Sheet> {
    match cadcraft_render::sheet(d, layout) {
        Some(sheet) => Ok(sheet),
        None => Ok(Sheet::from_page(&sheet_page(&read_document(d).map_err(other)?), 1.0)),
    }
}

/// Writes the table `make` builds to `layout`, created if absent, in place of any table titled
/// `title` there: at `at`, or else the top-left corner of the sheet's usable area.
fn write_table(d: &mut Drawing, layout: &str, title: &str, at: Option<Point2>, make: &dyn Fn(Point2) -> Table) -> Result<cadcraft_doc::Handle> {
    ensure_sheet_layout(d, layout)?;
    let old: Vec<cadcraft_doc::Handle> =
        d.layout(layout).map(|l| l.entities.iter().filter(|e| is_titled(e, title)).map(|e| e.handle).collect()).unwrap_or_default();
    for h in old {
        d.remove_entity(h);
    }
    let at = match at {
        Some(at) => at,
        None => {
            let area = usable_area(&layout_sheet(d, layout)?);
            Point2 { x: area.min.x, y: area.max.y }
        }
    };
    d.add(&Space::Paper(layout.to_owned()), Common::default(), EntityKind::Table(make(at))).map_err(other)
}

/// What [`refresh_table`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refreshed {
    Made,
    Updated,
}

/// Brings the table titled `title` on `layout` up to what `make` builds, where it stands: a table
/// with the same cell text is left alone (`None`), a different one replaced at its own corner, a
/// missing one written at the sheet's usable top-left.
pub(crate) fn refresh_table(d: &mut Drawing, layout: &str, title: &str, make: &dyn Fn(Point2) -> Table) -> Result<Option<Refreshed>> {
    let texts = |t: &Table| -> Vec<Vec<String>> { t.cells.iter().map(|r| r.iter().map(|c| c.text.clone()).collect()).collect() };
    let at = match titled_table(d, layout, title) {
        None => None,
        Some(t) => {
            let at = Point2 { x: t.insert.x, y: t.insert.y };
            if texts(t) == texts(&make(at)) {
                return Ok(None);
            }
            Some(at)
        }
    };
    write_table(d, layout, title, at, make)?;
    Ok(Some(if at.is_some() { Refreshed::Updated } else { Refreshed::Made }))
}

/// Refreshes the Framing BOM on its set sheet from [`bom_rows`]; no rows touches nothing.
pub(crate) fn refresh_bom(d: &mut Drawing) -> Result<Option<Refreshed>> {
    let rows = bom_rows(d);
    if rows.is_empty() {
        return Ok(None);
    }
    refresh_table(d, DEFAULT_LAYOUT, BOM_TITLE, &|at| bom_table(&rows, at))
}

/// Whether the table titled `title` on `layout` reaches below the sheet's usable area.
pub(crate) fn table_runs_off(d: &Drawing, layout: &str, title: &str) -> Result<bool> {
    let Some(t) = titled_table(d, layout, title) else { return Ok(false) };
    let area = usable_area(&layout_sheet(d, layout)?);
    Ok(t.insert.y - t.row_heights.iter().sum::<f64>() < area.min.y - TOLERANCE.coincidence_mm)
}

pub(crate) fn bomtable(s: &mut Session, p: &Value) -> Result<Value> {
    let layout = match p.get("layout").filter(|v| !v.is_null()) {
        None => DEFAULT_LAYOUT.to_owned(),
        Some(v) => v.as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad("bomtable", "`layout` must be a layout name"))?.to_owned(),
    };
    let at = match p.get("at").filter(|v| !v.is_null()) {
        None => None,
        Some(_) => Some(point_param("bomtable", p, "at")?),
    };
    let d0 = s.doc()?;
    let rows = bom_rows(d0);
    if rows.is_empty() {
        return Err(other("Nothing to count: no assembly plan lays repeats or placed details"));
    }
    // Build on a copy and assign it back once: one undo step.
    let mut d = d0.clone();
    let handle = write_table(&mut d, &layout, BOM_TITLE, at, &|at| bom_table(&rows, at))?;
    *s.doc_mut()? = d;
    let items: usize = rows.iter().map(|r| r.count).sum();
    let message = format!("{BOM_TITLE} on \"{layout}\": {} rows, {items} items.", rows.len());
    Ok(json!({
        "layout": layout,
        "handle": format!("{:X}", handle.0),
        "rows": rows.iter().map(|r| json!({ "level": r.level, "item": r.item, "count": r.count, "lengthMm": r.length_mm, "view": r.view })).collect::<Vec<_>>(),
        "message": message,
    }))
}

/// BOMTABLE: `Enter layout for the BOM <A601>:`.
struct BomTable;

impl Interactive for BomTable {
    fn name(&self) -> &'static str {
        "bomtable"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter layout for the BOM", Accept::TEXT).default(DEFAULT_LAYOUT.to_owned())
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(layout) = name_input(i, DEFAULT_LAYOUT) else { return Ok(Step::Continue) };
        match bomtable(s, &json!({ "layout": layout })) {
            Ok(r) => s.echo(r["message"].as_str().unwrap_or_default().to_owned()),
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

pub(crate) fn bomtable_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(BomTable))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::assembly::tests::{corner_detail, d_room, gap_room, line, room_with, undo_len, window_room, window_section, without_assembly};
    use crate::schema::{read_document, write_document};
    use lamb_core::geometry::IDENTITY_TRANSFORM;
    use lamb_core::model::{Actor, authored_field};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    /// Adds level `name` at `floor` (order = floor) and returns its id.
    fn add_level(s: &mut Session, name: &str, floor: f64) -> T<Id> {
        let d = s.doc_mut()?;
        let id = Id(d.new_handle().0);
        let mut doc = read_document(d)?;
        let floor_elevation = authored_field(floor, Id(0), Actor::User { user_id: "local".into() }, "2026-11-02T00:00:00Z");
        doc.levels.push(Level { id, name: name.into(), order: floor, floor_elevation });
        write_document(d, &doc)?;
        Ok(id)
    }

    /// Places `plan` on `level` with the identity transform.
    fn place(s: &mut Session, plan: Id, level: Id) -> R {
        let d = s.doc_mut()?;
        let mut doc = read_document(d)?;
        for v in &mut doc.views {
            if let AnyView::Plan(pv) = v
                && pv.id == plan
            {
                pv.placement = Some(Placement::Plan { level_id: level, cut_elevation: 1200.0, transform: IDENTITY_TRANSFORM });
            }
        }
        write_document(d, &doc)?;
        Ok(())
    }

    /// The assembly room with a 38-wide `STUD` repeated at 400 in its detail, placed on `Level 1`.
    pub(crate) fn stud_room() -> T<(Session, Id)> {
        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        let l1 = add_level(&mut s, "Level 1", 0.0)?;
        place(&mut s, plan, l1)?;
        let detail = read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id();
        repeat_on(&mut s, detail, "STUD")?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        Ok((s, plan))
    }

    /// Adds to `detail` a 38-wide member on the new layer `layer`, repeated at 400.
    fn repeat_on(s: &mut Session, detail: Id, layer: &str) -> R {
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        s.execute("layer.new", &json!({ "name": layer, "current": true }))?;
        let stud = new_handle(s, "pline", json!({ "vertices": [[100, 50], [138, 50], [138, 200], [100, 200]], "closed": true }))?;
        let dim = new_handle(s, "dimaligned", json!({ "p1": [100, 0], "p2": [500, 0], "at": [300, -300] }))?;
        s.execute("repeat", &json!({ "dimension": dim, "handles": [stud] }))?;
        Ok(())
    }

    /// Draws a rectangle of four lines on `layer` in `detail`, then returns to editing `back`.
    fn draw_rect(s: &mut Session, detail: Id, layer: &str, (x0, y0): (f64, f64), (x1, y1): (f64, f64), back: Id) -> R {
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        s.execute("layer.current", &json!({ "name": layer }))?;
        line(s, [x0, y0], [x1, y0])?;
        line(s, [x1, y0], [x1, y1])?;
        line(s, [x1, y1], [x0, y1])?;
        line(s, [x0, y1], [x0, y0])?;
        s.execute("editview", &json!({ "viewId": back.0 }))?;
        Ok(())
    }

    fn length_of(rows: &[BomRow], item: &str) -> f64 {
        rows.iter().filter(|r| r.item == item).map(|r| r.length_mm).sum()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    /// A 4000 x 3000 room, 200 thick, on `Level 1`, whose detail has a `SHEATHING` band drawn as two
    /// face lines and `STUD` repeats.
    fn sheathed_room() -> T<(Session, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let detail = Id(s.execute("newdetail", &json!({ "name": "D1" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        let r = line(&mut s, [0.0, 0.0], [1000.0, 0.0])?;
        s.execute("detailref", &json!({ "handle": format!("{:X}", r.0), "kind": "exterior_face" }))?;
        s.execute("layer.new", &json!({ "name": "SHEATHING", "current": true }))?;
        line(&mut s, [0.0, 20.0], [1000.0, 20.0])?;
        line(&mut s, [0.0, 32.0], [1000.0, 32.0])?;
        s.execute("layer.new", &json!({ "name": "GYP", "current": true }))?;
        line(&mut s, [0.0, 200.0], [1000.0, 200.0])?;
        repeat_on(&mut s, detail, "STUD")?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [4000, 0], [4000, 3000], [0, 3000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [3800, 200], [3800, 2800], [200, 2800]], "closed": true }))?;
        s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
        s.execute("detailtag", &json!({ "detailViewId": detail.0, "tip": {"x": 2000.0, "y": 0.0}, "bubble": {"x": 1500.0, "y": -1000.0} }))?;
        without_assembly(&mut s)?;
        let l1 = add_level(&mut s, "Level 1", 0.0)?;
        place(&mut s, plan, l1)?;
        Ok((s, plan))
    }

    #[test]
    fn a_band_of_two_face_lines_runs_once_per_run_and_studs_still_count() -> R {
        let (s, plan) = sheathed_room()?;
        let d = s.doc()?;
        let runs = assembly_lines(d, plan)?;
        assert_eq!(runs.len(), 4);
        // Each run lays the band as two face lines, between the inner corners.
        let mut laid = 0.0;
        for r in &runs {
            let faces: Vec<f64> = r.lines[..r.bands].iter().filter(|l| l.layer == "SHEATHING").map(line_length).collect();
            assert_eq!(faces.len(), 2, "{r:?}");
            assert!(close(faces[0], faces[1]), "{faces:?}");
            laid += faces[0];
        }
        assert!(close(laid, 2.0 * 3600.0 + 2.0 * 2600.0), "{laid}");
        let rows = bom_rows(d);
        assert!(
            rows.contains(&BomRow { level: "Level 1".into(), item: "SHEATHING".into(), count: 0, length_mm: laid, view: "Ground".into() }),
            "{rows:?}"
        );
        assert!(
            rows.contains(&BomRow { level: "Level 1".into(), item: "STUD".into(), count: copies(d, plan)?, length_mm: 0.0, view: "Ground".into() }),
            "{rows:?}"
        );
        let cells = bom_table(&rows, Point2 { x: 0.0, y: 0.0 }).cells;
        let texts: Vec<Vec<&str>> = cells.iter().map(|r| r.iter().map(|c| c.text.as_str()).collect()).collect();
        assert!(texts.contains(&vec!["Level 1", "SHEATHING", "", "12.4 m", "Ground"]), "{texts:?}");
        Ok(())
    }

    #[test]
    fn a_layer_s_longest_line_in_a_run_counts_once_and_repeat_lines_add_none() {
        let at =
            |x1: f64, layer: &str| AssemblyLine { from: Point2 { x: 0.0, y: 0.0 }, to: Point2 { x: x1, y: 0.0 }, layer: layer.to_owned(), arc: None };
        let r = RunAssembly {
            run: 0,
            detail: None,
            // Two separate bands on A, and a repeat copy on A past `bands`.
            lines: vec![at(100.0, "A"), at(300.0, "A"), at(200.0, "B"), at(900.0, "A")],
            bands: 3,
            copies: vec![1],
            skipped: None,
            conflict: None,
            face: None,
            hatches: Vec::new(),
        };
        assert_eq!(band_lengths(&r), HashMap::from([("A", 300.0), ("B", 200.0)]));
    }

    #[test]
    fn an_arc_line_s_length_is_its_radius_times_its_sweep_either_way_round() {
        let center = Point2 { x: 10.0, y: -5.0 };
        let arc = |ccw: bool| AssemblyLine {
            from: Point2 { x: 1010.0, y: -5.0 },
            to: Point2 { x: 10.0, y: 995.0 },
            layer: "A".to_owned(),
            arc: Some(lamb_core::derivation::assembly::ArcSpan { center, radius: 1000.0, ccw }),
        };
        assert!(close(line_length(&arc(true)), 500.0 * std::f64::consts::PI));
        assert!(close(line_length(&arc(false)), 1500.0 * std::f64::consts::PI));
    }

    #[test]
    fn an_arc_run_s_band_reads_its_arc_length() -> R {
        let (mut s, plan) = d_room()?;
        let l1 = add_level(&mut s, "Level 1", 0.0)?;
        place(&mut s, plan, l1)?;
        let rows = bom_rows(s.doc()?);
        // Three straight runs between the inner corners, and the half circle at each layer's outer radius.
        let straight = 2800.0 + 2800.0 + 1800.0;
        let half = std::f64::consts::PI;
        assert!(close(length_of(&rows, "CLAD"), straight + half * 1120.0), "{rows:?}");
        assert!(close(length_of(&rows, "GYP"), straight + half * 1050.0), "{rows:?}");
        Ok(())
    }

    #[test]
    fn a_section_adds_no_length_and_keeps_its_counted_rows() -> R {
        let (mut s, sec, _) = window_section(true)?;
        add_level(&mut s, "Level 1", 0.0)?;
        add_level(&mut s, "Level 2", 2000.0)?;
        let d = s.doc()?;
        let gyp = assembly_lines(d, sec)?.iter().flat_map(|r| r.lines[..r.bands].to_vec()).filter(|l| l.layer == "GYP").count();
        assert!(gyp > 0, "the section lays GYP bands");
        let rows = bom_rows(d);
        assert!(!rows.iter().any(|r| r.item == "GYP"), "{rows:?}");
        assert!(count_of(&rows, "STUD") > 0, "{rows:?}");
        assert!(rows.iter().all(|r| r.length_mm == 0.0), "{rows:?}");
        Ok(())
    }

    #[test]
    fn a_member_layer_with_a_band_has_its_count_and_length_on_one_row() -> R {
        let (mut s, plan) = stud_room()?;
        let detail = view_named(&s, "D1")?;
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        s.execute("layer.current", &json!({ "name": "STUD" }))?;
        line(&mut s, [0.0, 30.0], [1000.0, 30.0])?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let d = s.doc()?;
        let rows = bom_rows(d);
        let stud: Vec<&BomRow> = rows.iter().filter(|r| r.item == "STUD").collect();
        assert_eq!(stud.len(), 1, "{rows:?}");
        assert_eq!(stud[0].count, copies(d, plan)?);
        assert!(close(stud[0].length_mm, 16400.0), "{rows:?}");
        Ok(())
    }

    #[test]
    fn sheetset_brings_an_old_four_column_bom_up_to_the_length_column_once() -> R {
        let (mut s, _) = sheathed_room()?;
        s.execute("sheetset", &json!({}))?;
        let old = |at: Point2| {
            let row = vec!["Level 1".to_owned(), "STUD".to_owned(), "3".to_owned(), "Ground".to_owned()];
            schedule_table(BOM_TITLE, &["Level", "Item", "Count", "View"], &[30.0, 45.0, 15.0, 45.0], &[row], at)
        };
        write_table(s.doc_mut()?, DEFAULT_LAYOUT, BOM_TITLE, Some(Point2 { x: 50.0, y: 100.0 }), &old)?;
        let n = undo_len(&s)?;
        s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        let t = boms(&s)?;
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].cells[1].iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["Level", "Item", "Count", "Length", "View"]);
        assert_eq!((t[0].insert.x, t[0].insert.y, t[0].col_widths.clone()), (50.0, 100.0, COL_WIDTHS.to_vec()), "it stays where it stood");
        s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "a rerun adds no undo step");
        Ok(())
    }

    #[test]
    fn bomtable_returns_each_row_s_length() -> R {
        let (mut s, _) = sheathed_room()?;
        let r = s.execute("bomtable", &json!({}))?;
        let rows = r["rows"].as_array().ok_or("no rows")?;
        let of = |item: &str| rows.iter().find(|r| r["item"] == item).map(|r| (r["count"].as_u64(), r["lengthMm"].as_f64()));
        assert_eq!(of("SHEATHING"), Some((Some(0), Some(12400.0))), "{r}");
        assert_eq!(of("STUD").and_then(|c| c.1), Some(0.0), "{r}");
        Ok(())
    }

    fn count_of(rows: &[BomRow], item: &str) -> usize {
        rows.iter().filter(|r| r.item == item).map(|r| r.count).sum()
    }

    fn view_named(s: &Session, name: &str) -> T<Id> {
        Ok(read_document(s.doc()?)?.views.iter().find(|v| v.name() == name).ok_or("no such view")?.id())
    }

    /// Runs `cmd` and returns the hex handle of the one entity it added to the edited block.
    pub(crate) fn new_handle(s: &mut Session, cmd: &str, p: Value) -> T<String> {
        let all = |s: &Session| -> T<Vec<cadcraft_doc::Handle>> {
            let d = s.doc()?;
            Ok(d.blocks.values().flat_map(|b| b.entities.iter().map(|e| e.handle)).collect())
        };
        let before = all(s)?;
        s.execute(cmd, &p)?;
        let h = all(s)?.into_iter().find(|h| !before.contains(h)).ok_or("nothing added")?;
        Ok(format!("{:X}", h.0))
    }

    fn copies(d: &Drawing, plan: Id) -> T<usize> {
        Ok(assembly_lines(d, plan)?.iter().map(|r| r.copies.len()).sum())
    }

    #[test]
    fn a_stud_repeat_counts_one_stud_per_copy_on_the_plan_s_level() -> R {
        let (s, plan) = stud_room()?;
        let d = s.doc()?;
        let n = copies(d, plan)?;
        assert!(n > 10, "studs laid round the room: {n}");
        let rows = bom_rows(d);
        let studs: Vec<&BomRow> = rows.iter().filter(|r| r.item == "STUD").collect();
        assert_eq!(studs, [&BomRow { level: "Level 1".into(), item: "STUD".into(), count: n, length_mm: 0.0, view: "Ground".into() }]);
        Ok(())
    }

    #[test]
    fn a_placed_termination_is_one_end_row_per_free_end() -> R {
        let (mut s, plan, jd) = gap_room()?;
        let l1 = add_level(&mut s, "Level 1", 0.0)?;
        place(&mut s, plan, l1)?;
        s.execute("detailtag", &json!({ "detailViewId": jd.0, "corner": true, "tip": {"x": 2000, "y": 100}, "bubble": {"x": 1500, "y": -400} }))?;
        let rows = bom_rows(s.doc()?);
        assert!(
            rows.contains(&BomRow { level: "Level 1".into(), item: "JD (end)".into(), count: 2, length_mm: 0.0, view: "Ground".into() }),
            "{rows:?}"
        );
        assert!(!rows.iter().any(|r| r.item.ends_with("(jamb)")), "{rows:?}");
        Ok(())
    }

    #[test]
    fn jamb_and_corner_details_add_their_own_rows() -> R {
        let (mut s, plan) = window_room(false, true)?;
        let l1 = add_level(&mut s, "Level 1", 0.0)?;
        place(&mut s, plan, l1)?;
        let bridges = opening_assembly(s.doc()?, plan)?.iter().filter(|o| !o.lines.is_empty()).count();
        assert_eq!(bridges, 1);
        let rows = bom_rows(s.doc()?);
        assert!(
            rows.contains(&BomRow { level: "Level 1".into(), item: "JD (jamb)".into(), count: 2, length_mm: 0.0, view: "Ground".into() }),
            "{rows:?}"
        );
        // One corner tag spreads to all four corners of the room.
        corner_detail(&mut s, plan, "exterior_face", false, (0.0, 4000.0))?;
        let rows = bom_rows(s.doc()?);
        assert!(
            rows.contains(&BomRow { level: "Level 1".into(), item: "CD (corner)".into(), count: 4, length_mm: 0.0, view: "Ground".into() }),
            "{rows:?}"
        );
        Ok(())
    }

    #[test]
    fn two_placed_plans_count_under_their_levels_and_a_floating_one_adds_none() -> R {
        let (mut s, plan) = stud_room()?;
        let l2 = add_level(&mut s, "Level 2", 3000.0)?;
        let r = s.execute("duplicateview", &json!({ "viewId": plan.0, "name": "Upper" }))?;
        let upper = Id(r["viewId"].as_u64().ok_or("no id")?);
        place(&mut s, upper, l2)?;
        let r = s.execute("duplicateview", &json!({ "viewId": plan.0, "name": "Loose" }))?;
        let loose = Id(r["viewId"].as_u64().ok_or("no id")?);
        assert!(copies(s.doc()?, loose)? > 0, "the floating copy lays studs too");
        {
            let d = s.doc_mut()?;
            let mut doc = read_document(d)?;
            for v in &mut doc.views {
                if let AnyView::Plan(pv) = v
                    && pv.id == loose
                {
                    pv.placement = None;
                }
            }
            write_document(d, &doc)?;
        }
        let n = copies(s.doc()?, plan)?;
        let rows = bom_rows(s.doc()?);
        let got: Vec<(&str, &str, usize, &str)> =
            rows.iter().filter(|r| r.item == "STUD").map(|r| (r.level.as_str(), r.item.as_str(), r.count, r.view.as_str())).collect();
        assert_eq!(got, [("Level 1", "STUD", n, "Ground"), ("Level 2", "STUD", copies(s.doc()?, upper)?, "Upper")]);
        let clad: Vec<(&str, f64, &str)> =
            rows.iter().filter(|r| r.item == "CLAD").map(|r| (r.level.as_str(), r.length_mm, r.view.as_str())).collect();
        assert_eq!(clad, [("Level 1", 16400.0, "Ground"), ("Level 2", 16400.0, "Upper")], "each level its own row");
        Ok(())
    }

    #[test]
    fn studs_drawn_in_a_corner_detail_count_with_the_run_copies() -> R {
        let (mut s, plan) = stud_room()?;
        // One corner tag spreads to all four corners of the room.
        let cd = corner_detail(&mut s, plan, "exterior_face", false, (0.0, 0.0))?;
        draw_rect(&mut s, cd, "STUD", (-100.0, 50.0), (-62.0, 140.0), plan)?;
        draw_rect(&mut s, cd, "STUD", (-50.0, 50.0), (-12.0, 140.0), plan)?;
        let d = s.doc()?;
        assert!(corner_assembly(d, plan)?.iter().all(|c| c.skipped.is_none()), "{:?}", corner_assembly(d, plan)?);
        let n = copies(d, plan)?;
        let rows = bom_rows(d);
        assert!(
            rows.contains(&BomRow { level: "Level 1".into(), item: "STUD".into(), count: n + 2 * 4, length_mm: 0.0, view: "Ground".into() }),
            "{n}: {rows:?}"
        );
        assert_eq!(count_of(&rows, "CD (corner)"), 4, "{rows:?}");
        Ok(())
    }

    #[test]
    fn a_jamb_stud_counts_at_both_jambs_and_sheathing_lines_are_not_members() -> R {
        let (mut s, plan) = window_room(false, true)?;
        let l1 = add_level(&mut s, "Level 1", 0.0)?;
        place(&mut s, plan, l1)?;
        let (d1, jd) = (view_named(&s, "D1")?, view_named(&s, "JD")?);
        repeat_on(&mut s, d1, "STUD")?;
        draw_rect(&mut s, jd, "STUD", (-48.0, 50.0), (-10.0, 150.0), plan)?;
        corner_detail(&mut s, plan, "exterior_face", false, (0.0, 4000.0))?;
        let d = s.doc()?;
        let n = copies(d, plan)?;
        let rows = bom_rows(d);
        assert_eq!(count_of(&rows, "STUD"), n + 2, "{n}: {rows:?}");
        assert_eq!(count_of(&rows, "JD (jamb)"), 2, "{rows:?}");
        assert_eq!(count_of(&rows, "CD (corner)"), 4, "{rows:?}");
        for layer in ["CRN-A", "CRN-B", "JAMB", "GYP", "CLAD"] {
            assert_eq!(count_of(&rows, layer), 0, "{layer}: {rows:?}");
        }
        Ok(())
    }

    #[test]
    fn a_section_s_head_detail_counts_its_joist_at_the_level_under_its_mid_point() -> R {
        let (mut s, sec, _) = window_section(true)?;
        add_level(&mut s, "Level 1", 0.0)?;
        add_level(&mut s, "Level 2", 2000.0)?;
        // JOIST is a member layer only through this detail's repeat: no view lays a copy of it.
        let r = s.execute("newdetail", &json!({ "name": "R" }))?;
        let repeated = Id(r["viewId"].as_u64().ok_or("no id")?);
        repeat_on(&mut s, repeated, "JOIST")?;
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        let head = view_named(&s, "H")?;
        let block = s.doc()?.block(&view_block_name(head)).ok_or("no H block")?;
        let handle = block.entities.iter().find(|e| e.common.layer == "HEAD").ok_or("no HEAD line")?.handle;
        s.doc_mut()?.modify_entity(handle, |e| e.common.layer = "JOIST".to_owned())?;
        let rows = bom_rows(s.doc()?);
        let got: Vec<(&str, usize)> = rows.iter().filter(|r| r.item == "JOIST").map(|r| (r.level.as_str(), r.count)).collect();
        assert_eq!(got, [("Level 2", 1)], "{rows:?}");
        assert_eq!(count_of(&rows, "H (jamb)"), 1, "{rows:?}");
        assert_eq!(count_of(&rows, "SILL"), 0, "{rows:?}");
        Ok(())
    }

    #[test]
    fn members_group_into_connected_components_per_layer() {
        let at = |x0: f64, y0: f64, x1: f64, y1: f64, layer: &str| AssemblyLine {
            from: Point2 { x: x0, y: y0 },
            to: Point2 { x: x1, y: y1 },
            layer: layer.to_owned(),
            arc: None,
        };
        let members: HashSet<String> = ["A".to_owned(), "B".to_owned()].into();
        let lines = [
            // A chain joined out of order, its ends either way round.
            at(0.0, 0.0, 10.0, 0.0, "A"),
            at(20.0, 0.0, 30.0, 0.0, "A"),
            at(20.0, 0.0, 10.0, 0.0, "A"),
            // A lone line.
            at(0.0, 50.0, 10.0, 50.0, "A"),
            // Touches A's chain, but on another layer.
            at(0.0, 0.0, 0.0, 9.0, "B"),
            // Not a member layer.
            at(0.0, 0.0, 0.0, -9.0, "C"),
        ];
        let got = member_counts(&lines, &members);
        assert_eq!(got, HashMap::from([("A", 2), ("B", 1)]));
    }

    fn boms(s: &Session) -> T<Vec<Table>> {
        let l = s.doc()?.layout(DEFAULT_LAYOUT).ok_or("no BOM layout")?;
        Ok(l.entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Table(t) if is_titled(e, BOM_TITLE) => Some(t.clone()),
                _ => None,
            })
            .collect())
    }

    #[test]
    fn bomtable_writes_one_table_in_one_undo_step_and_a_rerun_replaces_it() -> R {
        let (mut s, plan) = stud_room()?;
        let n = undo_len(&s)?;
        let r = s.execute("bomtable", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        let studs = copies(s.doc()?, plan)?;
        assert_eq!(r["message"], format!("Framing BOM on \"A601\": 3 rows, {studs} items."));
        let t = boms(&s)?;
        assert_eq!(t.len(), 1);
        let texts: Vec<Vec<&str>> = t[0].cells.iter().map(|r| r.iter().map(|c| c.text.as_str()).collect()).collect();
        assert_eq!(texts[1], ["Level", "Item", "Count", "Length", "View"]);
        assert_eq!(texts[2], ["Level 1", "CLAD", "", "16.4 m", "Ground"]);
        assert_eq!(texts[4], ["Level 1", "STUD", studs.to_string().as_str(), "", "Ground"]);
        assert_eq!((t[0].title, t[0].header, t[0].text_height, t[0].col_widths.clone()), (true, true, 2.5, COL_WIDTHS.to_vec()));
        assert_eq!(t[0].row_heights, [7.0; 5]);
        let first = t[0].clone();

        // A second run at another point replaces it.
        s.execute("bomtable", &json!({ "at": {"x": 50, "y": 100} }))?;
        let t = boms(&s)?;
        assert_eq!(t.len(), 1, "still one table");
        assert_eq!((t[0].insert.x, t[0].insert.y), (50.0, 100.0));
        s.undo()?;
        assert_eq!(boms(&s)?, [first], "undo restores the first");
        s.undo()?;
        assert!(s.doc()?.layout(DEFAULT_LAYOUT).is_none(), "and the layout it created");
        Ok(())
    }

    #[test]
    fn nothing_to_count_is_refused_and_interactive_defaults_to_the_bom_sheet() -> R {
        let (mut s, _plan) = room_with("exterior_face", true, 200.0)?;
        let n = undo_len(&s)?;
        assert!(s.execute("bomtable", &json!({})).is_err(), "a floating plan counts nothing");
        assert_eq!(undo_len(&s)?, n);
        assert!(s.doc()?.layout(DEFAULT_LAYOUT).is_none());

        let (mut s, _plan) = stud_room()?;
        s.start("bomtable")?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("A601"));
        s.input(Input::Text("Sched".into()))?;
        assert!(s.doc()?.layout("Sched").is_some());
        assert!(s.log.iter().any(|l| l.starts_with("Framing BOM on \"Sched\": 3 rows")), "{:?}", s.log);
        Ok(())
    }
}

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
//! A plan's level is its placement's level; a floating plan is skipped. In a vertical view a run
//! belongs to the level with the greatest floor elevation at or below its centreline's mid-point
//! height (view-local y is absolute z), a corner or opening to the one below the mid-point of its
//! placed lines; none → level `—`. Rows are keyed (level, item, view), so two sections through
//! one floor show as two rows, each naming its view — nothing is silently counted twice. Sorted
//! by level order, then item, then view document order.
//!
//! `bomtable {layout?, at?}` (`bom`): the table (`Level | Item | Count | View`, titled
//! `Framing BOM`) on layout `layout` (default `BOM`, created without viewports when absent) at
//! paper point `at` (default the top-left corner of the sheet's
//! [`crate::sheets::usable_area`]). A table on that
//! layout titled `Framing BOM` is replaced; one undo step. Nothing to count is refused, nothing
//! changed. Returns `{layout, handle, rows: [{level, item, count, view}], message}`.

use std::collections::HashMap;

use cadcraft_doc::{Common, Drawing, EntityKind, Layout, Space, Table, TableCell};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::Vec3;
use cadcraft_render::Sheet;
use lamb_core::derivation::assembly::AssemblyLine;
use lamb_core::derivation::projection::vertical_local_to_uz;
use lamb_core::derivation::read::view_ref;
use lamb_core::derivation::walls::RunCurve;
use lamb_core::geometry::Point2;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AnyView, Id, Level, Placement};
use serde_json::{Value, json};

use crate::assembly::{assembly_lines, corner_assembly, end_assembly, opening_assembly};
use crate::commands::{bad, name_input, other, point_param};
use crate::detailtags::graph_and_sides;
use crate::project::DrawingProject;
use crate::schema::read_document;
use crate::sheets::{sheet_page, usable_area};

pub const BOM_TITLE: &str = "Framing BOM";
const DEFAULT_LAYOUT: &str = "BOM";
const NO_LEVEL: &str = "—";
const TEXT_HEIGHT: f64 = 2.5;
const ROW_HEIGHT: f64 = 7.0;
const COL_WIDTHS: [f64; 4] = [30.0, 45.0, 15.0, 45.0];

/// One BOM row: `count` of `item` on `level`, as laid in view `view`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BomRow {
    pub level: String,
    pub item: String,
    pub count: usize,
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

/// Every framing count the assembly plans lay, per (level, item, view), sorted.
pub fn bom_rows(d: &Drawing) -> Vec<BomRow> {
    let Ok(doc) = read_document(d) else { return Vec::new() };
    let Ok(p) = DrawingProject::new(d) else { return Vec::new() };
    let detail_name = |id: Option<Id>| {
        let id = id.unwrap_or(Id(0));
        doc.views.iter().find(|v| v.id() == id).map_or_else(|| format!("detail {}", id.0), |v| v.name().to_owned())
    };
    // (level order, level name, item, view index) → count.
    let mut counts: HashMap<(String, String, usize), (f64, usize)> = HashMap::new();
    let mut bump = |level: Option<&Level>, item: String, view: usize, n: usize| {
        let (order, name) = level.map_or((f64::INFINITY, NO_LEVEL.to_owned()), |l| (l.order, l.name.clone()));
        counts.entry((name, item, view)).or_insert((order, 0)).1 += n;
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
        if let Ok(runs) = assembly_lines(d, id) {
            let (graph, _) = graph_and_sides(&p, id);
            for r in &runs {
                let level = graph.runs.get(r.run).and_then(|run| level_of(curve_mid(&run.centreline)));
                let mut at = r.bands;
                for &n in &r.copies {
                    if let Some(first) = r.lines.get(at..at + n).and_then(|c| c.first()) {
                        bump(level, first.layer.clone(), vi, 1);
                    }
                    at += n;
                }
            }
        }
        if let Ok(corners) = corner_assembly(d, id) {
            for c in corners.iter().filter(|c| c.detail.is_some() && c.skipped.is_none() && !c.lines.is_empty()) {
                bump(level_of(lines_mid(&c.lines)), format!("{} (corner)", detail_name(c.detail)), vi, 1);
            }
        }
        if let Ok(openings) = opening_assembly(d, id) {
            for o in openings.iter().filter(|o| o.detail.is_some() && o.skipped.is_none() && !o.lines.is_empty()) {
                let n = if o.end.is_none() { 2 } else { 1 };
                bump(level_of(lines_mid(&o.lines)), format!("{} (jamb)", detail_name(o.detail)), vi, n);
            }
        }
        if let Ok(ends) = end_assembly(d, id) {
            for e in ends.iter().filter(|e| e.detail.is_some() && e.skipped.is_none() && !e.lines.is_empty()) {
                bump(level_of(lines_mid(&e.lines)), format!("{} (end)", detail_name(e.detail)), vi, 1);
            }
        }
    }
    let mut rows: Vec<((f64, String, String, usize), usize)> =
        counts.into_iter().filter(|(_, (_, n))| *n > 0).map(|((level, item, view), (order, n))| ((order, level, item, view), n)).collect();
    rows.sort_by(|a, b| a.0.0.total_cmp(&b.0.0).then_with(|| a.0.1.cmp(&b.0.1)).then_with(|| a.0.2.cmp(&b.0.2)).then_with(|| a.0.3.cmp(&b.0.3)));
    rows.into_iter()
        .map(|((_, level, item, view), count)| BomRow {
            level,
            item,
            count,
            view: doc.views.get(view).map(|v| v.name().to_owned()).unwrap_or_default(),
        })
        .collect()
}

/// The BOM table with its top-left corner at `at` (paper mm).
pub fn bom_table(rows: &[BomRow], at: Point2) -> Table {
    let cell = |s: &str| TableCell { text: s.to_owned(), ..TableCell::default() };
    let mut title = vec![TableCell::default(); COL_WIDTHS.len()];
    title[0] = TableCell { text: BOM_TITLE.to_owned(), merged: Some((1, COL_WIDTHS.len() as u32)), ..TableCell::default() };
    let mut cells = vec![title, ["Level", "Item", "Count", "View"].iter().map(|s| cell(s)).collect()];
    cells.extend(rows.iter().map(|r| vec![cell(&r.level), cell(&r.item), cell(&r.count.to_string()), cell(&r.view)]));
    Table {
        insert: Vec3::new(at.x, at.y, 0.0),
        col_widths: COL_WIDTHS.to_vec(),
        row_heights: vec![ROW_HEIGHT; cells.len()],
        cells,
        text_height: TEXT_HEIGHT,
        title: true,
        header: true,
        ..Table::default()
    }
}

fn is_bom(e: &cadcraft_doc::Entity) -> bool {
    matches!(&e.kind, EntityKind::Table(t) if t.cells.first().and_then(|r| r.first()).is_some_and(|c| c.text == BOM_TITLE))
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
    let page = sheet_page(&read_document(&d).map_err(other)?);
    let fallback = Sheet::from_page(&page, 1.0);
    if d.layout(&layout).is_none() {
        let tab_order = d.layouts.iter().map(|l| l.tab_order).max().map_or(0, |t| t + 1);
        d.layouts.push(Layout { page, ..Layout::new(&layout, tab_order) });
    }
    let old: Vec<cadcraft_doc::Handle> =
        d.layout(&layout).map(|l| l.entities.iter().filter(|e| is_bom(e)).map(|e| e.handle).collect()).unwrap_or_default();
    for h in old {
        d.remove_entity(h);
    }
    let at = at.unwrap_or_else(|| {
        let area = usable_area(&cadcraft_render::sheet(&d, &layout).unwrap_or(fallback));
        Point2 { x: area.min.x, y: area.max.y }
    });
    let handle = d.add(&Space::Paper(layout.clone()), Common::default(), EntityKind::Table(bom_table(&rows, at))).map_err(other)?;
    *s.doc_mut()? = d;
    let items: usize = rows.iter().map(|r| r.count).sum();
    let message = format!("{BOM_TITLE} on \"{layout}\": {} rows, {items} items.", rows.len());
    Ok(json!({
        "layout": layout,
        "handle": format!("{:X}", handle.0),
        "rows": rows.iter().map(|r| json!({ "level": r.level, "item": r.item, "count": r.count, "view": r.view })).collect::<Vec<_>>(),
        "message": message,
    }))
}

/// BOMTABLE: `Enter layout for the BOM <BOM>:`.
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
mod tests {
    use super::*;
    use crate::assembly::tests::{corner_detail, gap_room, room_with, undo_len, window_room};
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
    fn stud_room() -> T<(Session, Id)> {
        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        let l1 = add_level(&mut s, "Level 1", 0.0)?;
        place(&mut s, plan, l1)?;
        let detail = read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id();
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        s.execute("layer.new", &json!({ "name": "STUD", "current": true }))?;
        let stud = new_handle(&mut s, "pline", json!({ "vertices": [[100, 50], [138, 50], [138, 200], [100, 200]], "closed": true }))?;
        let dim = new_handle(&mut s, "dimaligned", json!({ "p1": [100, 0], "p2": [500, 0], "at": [300, -300] }))?;
        s.execute("repeat", &json!({ "dimension": dim, "handles": [stud] }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        Ok((s, plan))
    }

    /// Runs `cmd` and returns the hex handle of the one entity it added to the edited block.
    fn new_handle(s: &mut Session, cmd: &str, p: Value) -> T<String> {
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
        assert_eq!(rows, [BomRow { level: "Level 1".into(), item: "STUD".into(), count: n, view: "Ground".into() }]);
        Ok(())
    }

    #[test]
    fn a_placed_termination_is_one_end_row_per_free_end() -> R {
        let (mut s, plan, jd) = gap_room()?;
        let l1 = add_level(&mut s, "Level 1", 0.0)?;
        place(&mut s, plan, l1)?;
        s.execute("detailtag", &json!({ "detailViewId": jd.0, "corner": true, "tip": {"x": 2000, "y": 100}, "bubble": {"x": 1500, "y": -400} }))?;
        let rows = bom_rows(s.doc()?);
        assert!(rows.contains(&BomRow { level: "Level 1".into(), item: "JD (end)".into(), count: 2, view: "Ground".into() }), "{rows:?}");
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
        assert!(rows.contains(&BomRow { level: "Level 1".into(), item: "JD (jamb)".into(), count: 2, view: "Ground".into() }), "{rows:?}");
        // One corner tag spreads to all four corners of the room.
        corner_detail(&mut s, plan, "exterior_face", false, (0.0, 4000.0))?;
        let rows = bom_rows(s.doc()?);
        assert!(rows.contains(&BomRow { level: "Level 1".into(), item: "CD (corner)".into(), count: 4, view: "Ground".into() }), "{rows:?}");
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
        let got: Vec<(&str, &str, usize, &str)> = rows.iter().map(|r| (r.level.as_str(), r.item.as_str(), r.count, r.view.as_str())).collect();
        assert_eq!(got, [("Level 1", "STUD", n, "Ground"), ("Level 2", "STUD", copies(s.doc()?, upper)?, "Upper")]);
        Ok(())
    }

    fn boms(s: &Session) -> T<Vec<Table>> {
        let l = s.doc()?.layout("BOM").ok_or("no BOM layout")?;
        Ok(l.entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Table(t) if is_bom(e) => Some(t.clone()),
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
        assert_eq!(r["message"], format!("Framing BOM on \"BOM\": 1 rows, {studs} items."));
        let t = boms(&s)?;
        assert_eq!(t.len(), 1);
        let texts: Vec<Vec<&str>> = t[0].cells.iter().map(|r| r.iter().map(|c| c.text.as_str()).collect()).collect();
        assert_eq!(texts[1], ["Level", "Item", "Count", "View"]);
        assert_eq!(texts[2], ["Level 1", "STUD", studs.to_string().as_str(), "Ground"]);
        assert_eq!((t[0].title, t[0].header, t[0].text_height, t[0].col_widths.clone()), (true, true, 2.5, COL_WIDTHS.to_vec()));
        assert_eq!(t[0].row_heights, [7.0; 3]);
        let first = t[0].clone();

        // A second run at another point replaces it.
        s.execute("bomtable", &json!({ "at": {"x": 50, "y": 100} }))?;
        let t = boms(&s)?;
        assert_eq!(t.len(), 1, "still one table");
        assert_eq!((t[0].insert.x, t[0].insert.y), (50.0, 100.0));
        s.undo()?;
        assert_eq!(boms(&s)?, [first], "undo restores the first");
        s.undo()?;
        assert!(s.doc()?.layout("BOM").is_none(), "and the layout it created");
        Ok(())
    }

    #[test]
    fn nothing_to_count_is_refused_and_interactive_defaults_to_bom() -> R {
        let (mut s, _plan) = room_with("exterior_face", true, 200.0)?;
        let n = undo_len(&s)?;
        assert!(s.execute("bomtable", &json!({})).is_err(), "a floating plan counts nothing");
        assert_eq!(undo_len(&s)?, n);
        assert!(s.doc()?.layout("BOM").is_none());

        let (mut s, _plan) = stud_room()?;
        s.start("bomtable")?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("BOM"));
        s.input(Input::Text("Sched".into()))?;
        assert!(s.doc()?.layout("Sched").is_some());
        assert!(s.log.iter().any(|l| l.starts_with("Framing BOM on \"Sched\": 1 rows")), "{:?}", s.log);
        Ok(())
    }
}

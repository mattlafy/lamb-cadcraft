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

use cadcraft_doc::{Drawing, Table};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use lamb_core::geometry::Point2;
use lamb_core::model::{AnyView, Id};
use serde_json::{Value, json};

use crate::assembly::detail_layers;
use crate::bom::{self, Refreshed};
use crate::commands::{name_input, other};
use crate::detailref::detail_reference;
use crate::detailsheets::named;
use crate::detailtags::resolve_view_details;
use crate::repeats::detail_repeats;
use crate::schedule::write_schedule;
use crate::schema::{read_document, view_block_name};

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

/// Every wall type, marked in first-met order. An unreadable drawing has none.
pub fn wall_type_rows(d: &Drawing) -> Vec<WallTypeRow> {
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
    let mut rows = Vec::new();
    for (detail, used_in) in found {
        let Ok(layers) = detail_layers(d, detail) else { continue };
        let Some(view) = doc.views.iter().find(|v| v.id() == detail) else { continue };
        let build_up = layers
            .iter()
            .map(|l| if l.thickness.round() == 0.0 { l.layer.clone() } else { format!("{:.0} {}", l.thickness, l.layer) })
            .collect::<Vec<_>>()
            .join(" · ");
        let thickness_mm = layers.iter().map(|l| l.offset + l.thickness).fold(0.0, f64::max).round();
        rows.push(WallTypeRow {
            mark: format!("W{}", rows.len() + 1),
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

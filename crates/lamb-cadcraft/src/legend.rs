//! The drawing set's symbol legend (LAMB T696): SHEETSET puts one on the cover sheet `A001`, below
//! the Drawing List, showing each kind of generated symbol the set's sheets actually carry, in this
//! order — section cut symbol, elevation marker, level line, grid line, detail bubble, enlarged-area box, wall type
//! tag, opening mark, room tag, revision delta — each beside a one-line description naming the
//! schedule sheet where one applies. A kind nothing in the drawing uses gets no row.
//!
//! Each sample is drawn by the code that draws the symbol on the sheets
//! ([`crate::annotations::cut_symbol`], [`crate::annotations::elevation_marker`], [`crate::annotations::grid_bubble`], [`crate::annotations::wall_tag_symbol`],
//! [`crate::annotations::room_tag_items`], [`crate::bubbles::bubble_insert`],
//! [`crate::titleblock::delta_insert`]) at its size on paper, with fixed generic sample text.
//!
//! The legend is one block [`LEGEND_BLOCK`] in paper millimetres, origin at its top-left, and one
//! INSERT of it in `A001`'s paper space on layer `0`. A new legend goes at the Drawing List's
//! bottom-left, 10 mm below it (or the usable area's top-left with no list); an existing INSERT
//! keeps its point, since the user may have moved it. A legend with the same content is left alone;
//! with no symbol used the legend is removed. Without `A001` nothing is drawn.

use std::sync::Arc;

use cadcraft_doc::{Block, Common, Drawing, EntityKind, HAlign, Insert, Space, Text, VAlign};
use cadcraft_engine::Result;
use cadcraft_geom::Vec3;
use lamb_core::derivation::datums::named_grids;
use lamb_core::derivation::markers::compute_cut_lines_for_view;
use lamb_core::derivation::read::{ProjectRead, ViewKind};

use crate::annotations::{
    CUT_TEXT_MM, Item, LEVEL_TEXT_MM, cut_symbol, elevation_marker, entity_of, grid_bubble, is_elevation, k, room_tag_items, wall_tag_symbol,
};
use crate::bom::{self, Refreshed};
use crate::bubbles::{BUBBLE_BLOCK, BUBBLE_RADIUS_MM, bubble_insert};
use crate::commands::other;
use crate::project::DrawingProject;
use crate::rooms::{area_text, room_tags};
use crate::schedule::plan_marks;
use crate::schema::EXT_KEY;
use crate::sheets::{enlarged_windows, usable_area};
use crate::sheetset::{LIST_SHEET, LIST_TITLE};
use crate::titleblock::{DELTA_BLOCK, delta_insert};
use crate::walltypes::wall_tags;

/// The legend block's name; one per drawing.
pub const LEGEND_BLOCK: &str = "LAMB_LEGEND";
/// The legend's title.
pub const LEGEND_TITLE: &str = "Legend";
const TITLE_MM: f64 = 3.5;
const TEXT_MM: f64 = 2.5;
/// The width of the symbol column; a sample is centred in it.
const SYMBOL_COL_MM: f64 = 40.0;
/// Where each description starts.
const TEXT_X_MM: f64 = 45.0;
/// A row's least sample height, and the space added to every row.
const MIN_ROW_MM: f64 = 8.0;
const ROW_GAP_MM: f64 = 4.0;
/// A new legend sits this far below the Drawing List.
const BELOW_LIST_MM: f64 = 10.0;
/// The revision delta's side (paper mm, [`crate::titleblock`]'s triangle).
const DELTA_SIDE_MM: f64 = 8.0;
/// Every sample length a 1:50 annotation length is multiplied by to give paper mm.
fn f() -> f64 {
    k(1.0)
}

/// One symbol kind's sample, centred on the origin: generated items, or a block INSERT and its
/// half height.
enum Sample {
    Items(Vec<Item>),
    Insert(Insert, f64),
}

/// The rows the drawing uses, in legend order: each sample and its description.
fn rows(d: &Drawing) -> Vec<(Sample, &'static str)> {
    let f = f();
    let h = CUT_TEXT_MM * f;
    let mut out = Vec::new();
    let Ok(dp) = DrawingProject::new(d) else { return out };
    let doc = dp.document();
    let placed = |kind: ViewKind| dp.views().filter(move |v| v.kind == kind && v.placement.is_some()).map(|v| v.id).collect::<Vec<_>>();
    let shown: Vec<bool> = placed(ViewKind::Plan)
        .into_iter()
        .flat_map(|p| compute_cut_lines_for_view(&dp, p).unwrap_or_default())
        .map(|c| is_elevation(&dp, c.source_view_id))
        .collect();
    if shown.contains(&false) {
        let s = cut_symbol(v(-10.0, 0.0), v(10.0, 0.0), (0.0, 1.0), "A", Some("1/A301"), f);
        out.push((Sample::Items(s), "Section — name, and the sheet that shows it"));
    }
    if shown.contains(&true) {
        let s = elevation_marker(Vec3::ZERO, (0.0, 1.0), "North", Some("1/A201"), f);
        out.push((Sample::Items(s), "Elevation — name, and the sheet that shows it"));
    }
    if !doc.levels.is_empty() && !placed(ViewKind::Vertical).is_empty() {
        let s = vec![Item::Line(v(-20.0, 0.0), v(0.0, 0.0)), Item::Label(v(-20.0, LEVEL_TEXT_MM * f), "Level 1 +0".into(), LEVEL_TEXT_MM * f)];
        out.push((Sample::Items(s), "Level — name and floor elevation"));
    }
    if named_grids(&dp, &crate::gridname::overrides(doc)).is_ok_and(|g| !g.is_empty()) {
        let mut s = vec![Item::Line(v(-20.0, 0.0), v(0.0, 0.0))];
        s.extend(grid_bubble(v(0.0, 0.0), (1.0, 0.0), "1", f));
        out.push((Sample::Items(s), "Grid line — the same name on every plan, section and elevation"));
    }
    let is_bubble = |e: &cadcraft_doc::Entity| matches!(&e.kind, EntityKind::Insert(i) if i.block.eq_ignore_ascii_case(BUBBLE_BLOCK));
    let bubble_used = d.model.iter().any(|e| is_bubble(e))
        || d.blocks.iter().filter(|(n, _)| !n.eq_ignore_ascii_case(LEGEND_BLOCK)).any(|(_, b)| b.entities.iter().any(|e| is_bubble(e)))
        || d.layouts.iter().any(|l| l.entities.iter().any(|e| is_bubble(e)));
    if bubble_used {
        let s = bubble_insert(Vec3::ZERO, "D1", "1/A501", f);
        out.push((Sample::Insert(s, BUBBLE_RADIUS_MM * f), "Detail — detail name, and the sheet that shows it"));
    }
    if doc.views.iter().any(|w| !enlarged_windows(d, w.id()).is_empty()) {
        let (a, b, c, e) = (v(-10.0, -5.0), v(10.0, -5.0), v(10.0, 5.0), v(-10.0, 5.0));
        let s =
            vec![Item::Line(a, b), Item::Line(b, c), Item::Line(c, e), Item::Line(e, a), Item::Label(v(-10.0, 5.0 + 0.5 * h), "1/A102".into(), h)];
        out.push((Sample::Items(s), "Enlarged area — the sheet that shows it at a larger scale"));
    }
    if !wall_tags(d).is_empty() {
        out.push((Sample::Items(wall_tag_symbol(Vec3::ZERO, "W1", h)), "Wall type — see A604"));
    }
    if !plan_marks(d).is_empty() {
        out.push((Sample::Items(vec![Item::Text(Vec3::ZERO, "D01".into(), h)]), "Door or window mark — see A602"));
    }
    if !room_tags(d).is_empty() {
        let s = room_tag_items(Vec3::ZERO, "LIVING", "101", &area_text(Some(18.2)), h);
        out.push((Sample::Items(s), "Room — name, number and area — see A603"));
    }
    let is_delta = |e: &cadcraft_doc::Entity| matches!(&e.kind, EntityKind::Insert(i) if i.block.eq_ignore_ascii_case(DELTA_BLOCK));
    if d.layouts.iter().any(|l| l.entities.iter().any(|e| is_delta(e))) {
        // The triangle's centroid is its insert point: its extents run −h/3 … 2h/3.
        let th = DELTA_SIDE_MM * 3f64.sqrt() / 2.0;
        out.push((Sample::Insert(delta_insert(v(0.0, -th / 6.0), 1), th / 2.0), "Revision — the issue that changed it"));
    }
    out
}

fn v(x: f64, y: f64) -> Vec3 {
    Vec3::new(x, y, 0.0)
}

/// The vertical extent (low, high) of `items`, a text taken as its height centred on its point.
fn y_range(items: &[Item]) -> (f64, f64) {
    let mut r = (f64::INFINITY, f64::NEG_INFINITY);
    let mut add = |lo: f64, hi: f64| r = (r.0.min(lo), r.1.max(hi));
    for i in items {
        match i {
            Item::Line(a, b) => add(a.y.min(b.y), a.y.max(b.y)),
            Item::Text(at, _, h) | Item::Label(at, _, h) => add(at.y - h / 2.0, at.y + h / 2.0),
            Item::Dim(a, b, ..) => add(a.y.min(b.y), a.y.max(b.y)),
            Item::Circle(c, r) => add(c.y - r, c.y + r),
        }
    }
    if r.0 > r.1 { (0.0, 0.0) } else { r }
}

fn shifted(i: &Item, dx: f64, dy: f64) -> Item {
    let m = |p: &Vec3| v(p.x + dx, p.y + dy);
    match i {
        Item::Line(a, b) => Item::Line(m(a), m(b)),
        Item::Text(at, s, h) => Item::Text(m(at), s.clone(), *h),
        Item::Label(at, s, h) => Item::Label(m(at), s.clone(), *h),
        Item::Dim(a, b, r, l, sc) => Item::Dim(m(a), m(b), *r, m(l), *sc),
        Item::Circle(c, r) => Item::Circle(m(c), *r),
    }
}

fn shifted_insert(mut i: Insert, dx: f64, dy: f64) -> Insert {
    i.insert = v(i.insert.x + dx, i.insert.y + dy);
    for a in &mut i.attribs {
        a.text.insert = v(a.text.insert.x + dx, a.text.insert.y + dy);
        a.text.align_pt = a.text.align_pt.map(|p| v(p.x + dx, p.y + dy));
    }
    i
}

fn label(at: Vec3, value: &str, height: f64) -> EntityKind {
    EntityKind::Text(Text {
        insert: at,
        align_pt: Some(at),
        height,
        value: value.into(),
        rotation: 0.0,
        width_factor: 1.0,
        oblique: 0.0,
        style: "Standard".into(),
        halign: HAlign::Left,
        valign: VAlign::Middle,
    })
}

/// The legend block's entities for `rows`, in block order, and its total height.
fn contents(rows: Vec<(Sample, &'static str)>) -> (Vec<EntityKind>, f64) {
    let mut out = vec![label(v(0.0, -TITLE_MM / 2.0), LEGEND_TITLE, TITLE_MM)];
    let mut top = -(TITLE_MM + ROW_GAP_MM);
    let cx = SYMBOL_COL_MM / 2.0;
    for (sample, text) in rows {
        let (lo, hi) = match &sample {
            Sample::Items(items) => y_range(items),
            Sample::Insert(i, half) => (i.insert.y - half, i.insert.y + half),
        };
        let height = (hi - lo).max(MIN_ROW_MM) + ROW_GAP_MM;
        let yc = top - height / 2.0;
        let dy = yc - (lo + hi) / 2.0;
        match sample {
            Sample::Items(items) => out.extend(items.iter().map(|i| entity_of(&shifted(i, cx, dy)))),
            Sample::Insert(i, _) => out.push(EntityKind::Insert(shifted_insert(i, cx, dy))),
        }
        out.push(label(v(TEXT_X_MM, yc), text, TEXT_MM));
        top -= height;
    }
    (out, -top)
}

/// The INSERT of [`LEGEND_BLOCK`] on `A001`.
fn legend_insert(d: &Drawing) -> Option<(cadcraft_doc::Handle, Vec3)> {
    d.layout(LIST_SHEET)?.entities.iter().find_map(|e| match &e.kind {
        EntityKind::Insert(i) if i.block.eq_ignore_ascii_case(LEGEND_BLOCK) => Some((e.handle, i.insert)),
        _ => None,
    })
}

/// Brings `A001`'s legend up to the symbols the drawing uses (see the module doc).
pub(crate) fn refresh_legend(d: &mut Drawing) -> Result<Option<Refreshed>> {
    if d.layout(LIST_SHEET).is_none() || !d.ext.contains_key(EXT_KEY) {
        return Ok(None);
    }
    let rows = rows(d);
    let existing = legend_insert(d);
    if rows.is_empty() {
        let had_block = d.blocks.remove(LEGEND_BLOCK).is_some();
        if let Some((h, _)) = existing {
            d.remove_entity(h);
        }
        return Ok((had_block || existing.is_some()).then_some(Refreshed::Updated));
    }
    let (want, _) = contents(rows);
    let same = d
        .block(LEGEND_BLOCK)
        .is_some_and(|b| b.entities.len() == want.len() && b.entities.iter().zip(&want).all(|(e, w)| e.common.layer == "0" && e.kind == *w));
    if same && existing.is_some() {
        return Ok(None);
    }
    d.blocks.insert(LEGEND_BLOCK.into(), Arc::new(Block::new(LEGEND_BLOCK)));
    let space = Space::Block(LEGEND_BLOCK.into());
    for kind in want {
        d.add(&space, Common { layer: "0".into(), ..Common::default() }, kind).map_err(other)?;
    }
    if existing.is_some() {
        return Ok(Some(Refreshed::Updated));
    }
    let at = match bom::titled_table(d, LIST_SHEET, LIST_TITLE) {
        Some(t) => v(t.insert.x, t.insert.y - t.row_heights.iter().sum::<f64>() - BELOW_LIST_MM),
        None => {
            let sheet = cadcraft_render::sheet(d, LIST_SHEET).ok_or_else(|| other("A001 has no page"))?;
            let area = usable_area(&sheet);
            v(area.min.x, area.max.y)
        }
    };
    let insert = Insert {
        clip: None,
        block: LEGEND_BLOCK.into(),
        insert: at,
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: Vec::new(),
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    };
    d.add(&Space::Paper(LIST_SHEET.into()), Common { layer: "0".into(), ..Common::default() }, EntityKind::Insert(insert)).map_err(other)?;
    Ok(Some(Refreshed::Made))
}

/// `true` when `A001`'s legend reaches below the sheet's usable area.
pub(crate) fn legend_runs_off(d: &Drawing) -> bool {
    let (Some((_, at)), Some(sheet)) = (legend_insert(d), cadcraft_render::sheet(d, LIST_SHEET)) else { return false };
    let ext = d.extents(&Space::Block(LEGEND_BLOCK.into()));
    !ext.is_empty() && at.y + ext.min.y < usable_area(&sheet).min.y - lamb_core::geometry::tolerance::TOLERANCE.coincidence_mm
}

/// Every description in `A001`'s legend, in row order (none without one).
#[cfg(test)]
pub(crate) fn legend_rows(d: &Drawing) -> Vec<String> {
    let Some(b) = d.block(LEGEND_BLOCK) else { return Vec::new() };
    b.entities
        .iter()
        .filter_map(|e| match &e.kind {
            EntityKind::Text(t) if t.halign == HAlign::Left && (t.insert.x - TEXT_X_MM).abs() < 1e-9 => Some(t.value.clone()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assembly::tests::room_with;
    use cadcraft_engine::Session;
    use lamb_core::model::Id;
    use serde_json::json;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    const CUT: &str = "Section — name, and the sheet that shows it";
    const ELEV: &str = "Elevation — name, and the sheet that shows it";
    const LEVEL: &str = "Level — name and floor elevation";
    const BUBBLE: &str = "Detail — detail name, and the sheet that shows it";
    const WALL: &str = "Wall type — see A604";
    const ROOM: &str = "Room — name, number and area — see A603";
    const GRID: &str = "Grid line — the same name on every plan, section and elevation";

    /// `assembly.rs`'s tagged room (a placed plan, edge detail `D1` tagged on its bottom wall) plus
    /// the observers, section `A-A` drawn along y = 2000 and room `Living`. Returns (session, plan).
    fn set() -> T<(Session, Id)> {
        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        crate::maintain::install(&mut s);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let r = s.execute(
            "newsectionline",
            &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 2000}, "cutTo": {"x": 5000, "y": 2000} }),
        )?;
        let sec = Id(r["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.execute("line", &json!({ "points": [[0, 0], [5000, 0]] }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("room", &json!({ "at": {"x": 2500, "y": 1000}, "name": "Living" }))?;
        s.execute("editview", &json!({}))?;
        Ok((s, plan))
    }

    fn undo_len(s: &Session) -> T<usize> {
        Ok(s.state()?.undo.len())
    }

    fn insert_at(s: &Session) -> T<Vec3> {
        Ok(legend_insert(s.doc()?).ok_or("no legend insert")?.1)
    }

    #[test]
    fn sheetset_puts_the_used_symbols_below_the_drawing_list_and_a_rerun_changes_nothing() -> R {
        let (mut s, _plan) = set()?;
        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert!(r["message"].as_str().is_some_and(|m| m.contains("A001 Legend")), "{r}");
        assert_eq!(legend_rows(s.doc()?), [CUT, LEVEL, BUBBLE, ROOM]);
        let t = bom::titled_table(s.doc()?, LIST_SHEET, LIST_TITLE).ok_or("no list")?;
        let want = (t.insert.x, t.insert.y - t.row_heights.iter().sum::<f64>() - BELOW_LIST_MM);
        let at = insert_at(&s)?;
        assert!((at.x - want.0).abs() < 1e-9 && (at.y - want.1).abs() < 1e-9, "{at:?} {want:?}");
        // The samples are the sheets' own symbols at paper size: the cut name is 5 mm.
        let b = s.doc()?.block(LEGEND_BLOCK).ok_or("no block")?;
        assert!(b.entities.iter().any(|e| matches!(&e.kind, EntityKind::Text(t) if t.value == "A" && (t.height - 5.0).abs() < 1e-9)));
        assert!(b.entities.iter().any(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == BUBBLE_BLOCK)));

        let n = undo_len(&s)?;
        let b0 = s.doc()?.blocks.get(LEGEND_BLOCK).cloned().ok_or("no block")?;
        s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n, "a rerun records no undo step");
        assert!(Arc::ptr_eq(&b0, s.doc()?.blocks.get(LEGEND_BLOCK).ok_or("no block")?));

        s.undo()?;
        assert!(s.doc()?.block(LEGEND_BLOCK).is_none() && legend_insert(s.doc()?).is_none(), "undone with the set");
        Ok(())
    }

    #[test]
    fn a_wall_tag_plan_adds_its_row_in_place_and_a_set_without_symbols_gets_none() -> R {
        let (mut s, plan) = set()?;
        s.execute("sheetset", &json!({}))?;
        // Move the legend; the update keeps where it went.
        let d = s.doc_mut()?;
        let (h, at) = legend_insert(d).ok_or("no insert")?;
        let moved = v(at.x + 100.0, at.y);
        let mut e = (**d.entity(h).ok_or("gone")?).clone();
        if let EntityKind::Insert(i) = &mut e.kind {
            i.insert = moved;
        }
        d.remove_entity(h);
        d.add(&Space::Paper(LIST_SHEET.into()), e.common.clone(), e.kind.clone()).map_err(|e| e.to_string())?;
        s.execute("walltags", &json!({ "viewId": plan.0 }))?;
        let r = s.execute("sheetset", &json!({}))?;
        assert!(r["message"].as_str().is_some_and(|m| m.contains("A001 Legend updated")), "{r}");
        assert_eq!(legend_rows(s.doc()?), [CUT, LEVEL, BUBBLE, WALL, ROOM]);
        assert_eq!(insert_at(&s)?, moved);

        // A plain set: one placed plan with a line and a second sheet, so A001 exists.
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("line", &json!({ "points": [[0, 0], [5000, 0]] }))?;
        s.execute("editview", &json!({}))?;
        s.execute("sheetset", &json!({}))?;
        assert!(s.doc()?.layout(LIST_SHEET).is_some(), "A001 made");
        assert!(s.doc()?.block(LEGEND_BLOCK).is_none() && legend_insert(s.doc()?).is_none());
        Ok(())
    }

    #[test]
    fn an_elevation_gets_its_own_row_and_a_plan_with_only_elevations_no_section_row() -> R {
        let (mut s, plan) = set()?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let r = s.execute(
            "newelevationline",
            &json!({ "name": "North", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 6000}, "cutTo": {"x": 5000, "y": 6000} }),
        )?;
        let el = Id(r["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": el.0 }))?;
        s.execute("line", &json!({ "points": [[0, 0], [5000, 0]] }))?;
        s.execute("editview", &json!({}))?;
        s.execute("sheetset", &json!({}))?;
        assert_eq!(legend_rows(s.doc()?), [CUT, ELEV, LEVEL, BUBBLE, ROOM]);
        let b = s.doc()?.block(LEGEND_BLOCK).ok_or("no block")?;
        assert!(b.entities.iter().any(|e| matches!(&e.kind, EntityKind::Circle(c) if (c.radius - 6.0).abs() < 1e-9)), "the marker at paper size");

        let sec = read_section(&s)?;
        s.execute("unplaceview", &json!({ "viewId": sec.0 }))?;
        s.execute("sheetset", &json!({}))?;
        assert_eq!(legend_rows(s.doc()?), [ELEV, LEVEL, BUBBLE, ROOM]);
        Ok(())
    }

    /// The id of section `A-A`.
    fn read_section(s: &Session) -> T<Id> {
        let doc = crate::schema::read_document(s.doc()?)?;
        Ok(doc.views.iter().find(|w| w.name() == "A-A").ok_or("no section")?.id())
    }

    #[test]
    fn a_set_with_a_grid_line_gets_its_row_after_the_levels() -> R {
        let (mut s, plan) = set()?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        crate::annotations::tests::datum_line(&mut s, plan, [1000.0, -1000.0], [1000.0, 4000.0])?;
        s.execute("editview", &json!({}))?;
        s.execute("sheetset", &json!({}))?;
        assert_eq!(legend_rows(s.doc()?), [CUT, LEVEL, GRID, BUBBLE, ROOM]);
        let b = s.doc()?.block(LEGEND_BLOCK).ok_or("no block")?;
        assert!(b.entities.iter().any(|e| matches!(&e.kind, EntityKind::Circle(c) if (c.radius - 5.0).abs() < 1e-9)), "the bubble at paper size");
        Ok(())
    }
}

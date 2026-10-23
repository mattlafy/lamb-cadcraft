//! Generated annotation (LAMB's `docs/07-architecture/cadcraft-integration.md` §5, LAMB T474):
//! every placed plan gets a plotted section cut symbol for each placed vertical view whose cut line
//! projects into it — the cut LINE, a gaze arrow at each end, and the section's name at each end
//! (plus `<n>/<layout>` when a sheet shows the section, [`crate::sheets::sheet_refs`]) — and every
//! placed section or elevation gets a plotted level line for each LEVEL (LAMB T473): a LINE at the
//! level's projected floor elevation spanning the view block's extents ±[`LEVEL_OVERRUN_MM`], and
//! `"<name> <±elev>"` left-aligned past its right end, plus a vertical floor-to-floor dimension per
//! pair of consecutive levels by elevation (LAMB T500). Plan cut elevations stay overlay-only.
//!
//! The annotation lives in its own block per view, [`annotation_block_name`]`(view)`, never in the
//! view block, so derivation, the wall graph, links and LAMBSAVE never see them. The block's one
//! model-space INSERT sits at the origin on the view's own view layer, so a sheet viewport showing
//! the view shows its annotation and every other viewport freezes it. [`annotation_observer`]
//! regenerates the blocks after every command, writing the drawing back only when one changed, so
//! the change lands in the command's own undo step.

use std::sync::Arc;

use cadcraft_doc::{Block, Common, DimKind, Dimension, Drawing, Entity, EntityKind, HAlign, Insert, Line, Space, Text, VAlign};
use cadcraft_engine::Session;
use cadcraft_geom::Vec3;
use lamb_core::derivation::markers::compute_cut_lines_for_view;
use lamb_core::derivation::projection::{Uz, vertical_uz_to_local};
use lamb_core::derivation::read::{ProjectRead, ViewKind};
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AnyView, Id};

use crate::project::DrawingProject;
use crate::schema::{annotation_block_name, read_document, view_block_name};
use crate::sheets::sheet_refs;

/// Length of a gaze arrow's shaft, from the cut line's end.
pub const CUT_ARROW_MM: f64 = 600.0;
/// Length of each of an arrow head's two strokes, ±30° back from the tip.
pub const CUT_HEAD_MM: f64 = 200.0;
/// The section name's text height.
pub const CUT_TEXT_MM: f64 = 250.0;
/// The name sits this far beyond the arrow's tip.
const CUT_TEXT_GAP_MM: f64 = 300.0;
/// A level line runs this far past the view block's extents at each end.
pub const LEVEL_OVERRUN_MM: f64 = 1000.0;
/// A level label's text height.
pub const LEVEL_TEXT_MM: f64 = 250.0;
/// The level label starts this far past the line's right end.
const LEVEL_TEXT_GAP_MM: f64 = 200.0;
/// A floor-to-floor dimension's line sits this far right of the level lines' right end, clear of
/// the labels.
pub const LEVEL_DIM_OFFSET_MM: f64 = 1200.0;
/// Every generated entity's layer inside the block.
const LAYER: &str = "0";

/// One generated entity, in view-local coordinates.
#[derive(Clone, Debug, PartialEq)]
enum Item {
    Line(Vec3, Vec3),
    /// A cut symbol's name: centred, [`CUT_TEXT_MM`] high.
    Text(Vec3, String),
    /// A level label: left-aligned, [`LEVEL_TEXT_MM`] high.
    Label(Vec3, String),
    /// A floor-to-floor dimension: vertical rotated-linear, measuring from the lower to the upper
    /// level line's right end, its dimension line [`LEVEL_DIM_OFFSET_MM`] further right.
    Dim(Vec3, Vec3),
}

fn v(x: f64, y: f64) -> Vec3 {
    Vec3::new(x, y, 0.0)
}

fn near(a: Vec3, b: Vec3) -> bool {
    (a.x - b.x).hypot(a.y - b.y) <= TOLERANCE.coincidence_mm
}

/// What view `id` wants, in write order: a plan its cut symbols, a vertical view its level lines;
/// nothing for an unplaced view.
fn wanted(d: &Drawing, id: Id, sheet_of: &dyn Fn(Id) -> Option<String>) -> Result<Vec<Item>, String> {
    let dp = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let Some(view) = dp.view(id).filter(|v| v.placement.is_some()) else { return Ok(Vec::new()) };
    if view.kind == ViewKind::Vertical {
        return Ok(level_lines(d, &dp, id));
    }
    let plan = id;
    let mut out = Vec::new();
    for c in compute_cut_lines_for_view(&dp, plan)? {
        let (g, n) = ((c.gaze_dir.x, c.gaze_dir.y), c.name);
        out.push(Item::Line(v(c.from.x, c.from.y), v(c.to.x, c.to.y)));
        for end in [c.from, c.to] {
            let tip = v(end.x + g.0 * CUT_ARROW_MM, end.y + g.1 * CUT_ARROW_MM);
            out.push(Item::Line(v(end.x, end.y), tip));
            for a in [30f64, -30f64] {
                let (s, co) = a.to_radians().sin_cos();
                // Back from the tip: -gaze rotated by ±30°.
                let (bx, by) = (-(g.0 * co - g.1 * s), -(g.0 * s + g.1 * co));
                out.push(Item::Line(tip, v(tip.x + bx * CUT_HEAD_MM, tip.y + by * CUT_HEAD_MM)));
            }
        }
        for end in [c.from, c.to] {
            let k = CUT_ARROW_MM + CUT_TEXT_GAP_MM;
            let at = v(end.x + g.0 * k, end.y + g.1 * k);
            out.push(Item::Text(at, n.clone()));
            if let Some(sheet) = sheet_of(c.source_view_id) {
                out.push(Item::Text(v(at.x, at.y - 1.5 * CUT_TEXT_MM), sheet));
            }
        }
    }
    Ok(out)
}

/// `"+0"`, `"+3000"`, `"-2700"`: signed whole millimetres, never `-0`.
fn signed_mm(z: f64) -> String {
    let r = z.round();
    format!("{:+.0}", if r == 0.0 { 0.0 } else { r })
}

/// Placed vertical view `id`'s level lines: per LEVEL (by `order`, then id) a LINE at its projected
/// floor elevation across the view block's x-extents ±[`LEVEL_OVERRUN_MM`], then its label. An
/// empty view block wants none.
fn level_lines(d: &Drawing, dp: &DrawingProject, id: Id) -> Vec<Item> {
    let Some(view) = dp.view(id) else { return Vec::new() };
    let ext = d.extents(&Space::Block(view_block_name(id)));
    if ext.is_empty() {
        return Vec::new();
    }
    let (x0, x1) = (ext.min.x - LEVEL_OVERRUN_MM, ext.max.x + LEVEL_OVERRUN_MM);
    let mut levels: Vec<_> = dp.document().levels.iter().collect();
    levels.sort_by(|a, b| a.order.total_cmp(&b.order).then(a.id.0.cmp(&b.id.0)));
    let y_of = |z: f64| vertical_uz_to_local(&view, Uz { u: 0.0, z }).y;
    let mut out = Vec::new();
    for l in &levels {
        let z = l.floor_elevation.value;
        let y = y_of(z);
        out.push(Item::Line(v(x0, y), v(x1, y)));
        out.push(Item::Label(v(x1 + LEVEL_TEXT_GAP_MM, y), format!("{} {}", l.name, signed_mm(z))));
    }
    let mut zs: Vec<f64> = levels.iter().map(|l| l.floor_elevation.value).collect();
    zs.sort_by(f64::total_cmp);
    for w in zs.windows(2) {
        if w[1] - w[0] > TOLERANCE.coincidence_mm {
            out.push(Item::Dim(v(x1, y_of(w[0])), v(x1, y_of(w[1]))));
        }
    }
    out
}

fn text(at: Vec3, value: &str, height: f64, halign: HAlign) -> Text {
    Text {
        insert: at,
        align_pt: Some(at),
        height,
        value: value.into(),
        rotation: 0.0,
        width_factor: 1.0,
        oblique: 0.0,
        style: "Standard".into(),
        halign,
        valign: VAlign::Middle,
    }
}

fn entity_of(i: &Item) -> EntityKind {
    match i {
        Item::Line(a, b) => EntityKind::Line(Line { a: *a, b: *b }),
        Item::Text(at, s) => EntityKind::Text(text(*at, s, CUT_TEXT_MM, HAlign::Center)),
        Item::Label(at, s) => EntityKind::Text(text(*at, s, LEVEL_TEXT_MM, HAlign::Left)),
        Item::Dim(a, b) => EntityKind::Dimension(level_dim(*a, *b)),
    }
}

/// Where a floor-to-floor dimension's line passes: level with `b`, [`LEVEL_DIM_OFFSET_MM`] right.
fn dim_line_pt(b: Vec3) -> Vec3 {
    v(b.x + LEVEL_DIM_OFFSET_MM, b.y)
}

/// A vertical (rotation π/2) linear dimension from `a` to `b`, as DIMLINEAR draws one: `defpt` on
/// the dimension line, measured text, style `Standard`, no overrides, never associative.
fn level_dim(a: Vec3, b: Vec3) -> Dimension {
    Dimension {
        kind: DimKind::Linear { rotation: std::f64::consts::FRAC_PI_2 },
        defpt: dim_line_pt(b),
        text_mid: Vec3::ZERO,
        p13: a,
        p14: b,
        p15: Vec3::ZERO,
        p16: Vec3::ZERO,
        text: String::new(),
        style: "Standard".into(),
        measurement: 0.0,
        text_rotation: 0.0,
        user_text_pos: false,
        block: None,
        overrides: Default::default(),
        assoc: Vec::new(),
        marks: Default::default(),
    }
}

fn is_vertical(k: &DimKind) -> bool {
    matches!(k, DimKind::Linear { rotation } if (rotation - std::f64::consts::FRAC_PI_2).abs() < 1e-9)
}

/// `true` when `block` holds exactly `want`, in order: each on layer `0`, LINE ends, TEXT
/// alignment points and DIMENSION points within [`TOLERANCE`]`.coincidence_mm`, the same text
/// strings, and every dimension vertical with measured text.
fn block_matches(block: &Block, want: &[Item]) -> bool {
    block.entities.len() == want.len()
        && block.entities.iter().zip(want).all(|(e, w)| {
            e.common.layer == LAYER
                && match (&e.kind, w) {
                    (EntityKind::Line(l), Item::Line(a, b)) => near(l.a, *a) && near(l.b, *b),
                    (EntityKind::Text(t), Item::Text(at, s)) => {
                        t.halign == HAlign::Center && near(t.align_pt.unwrap_or(t.insert), *at) && t.value == *s
                    }
                    (EntityKind::Text(t), Item::Label(at, s)) => {
                        t.halign == HAlign::Left && near(t.align_pt.unwrap_or(t.insert), *at) && t.value == *s
                    }
                    (EntityKind::Dimension(dm), Item::Dim(a, b)) => {
                        is_vertical(&dm.kind)
                            && near(dm.p13, *a)
                            && near(dm.p14, *b)
                            && (dm.defpt.x - dim_line_pt(*b).x).abs() <= TOLERANCE.coincidence_mm
                            && dm.text.is_empty()
                    }
                    _ => false,
                }
        })
}

/// The block's canonical model-space INSERT: at the origin, scale 1, on the view's own layer.
/// Added only when model space has no INSERT of the block.
fn ensure_insert(d: &mut Drawing, plan: Id) {
    let name = annotation_block_name(plan);
    if d.model.iter().any(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == name)) {
        return;
    }
    let layer = view_block_name(plan);
    d.ensure_layer(&layer);
    let insert = Insert {
        block: name,
        insert: Vec3::ZERO,
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: Vec::new(),
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    };
    let handle = d.new_handle();
    d.model.push(Entity { handle, common: Common { layer, ..Common::default() }, kind: EntityKind::Insert(insert) });
}

fn replace_block(d: &mut Drawing, plan: Id, items: &[Item]) -> Result<(), String> {
    let name = annotation_block_name(plan);
    d.blocks.insert(name.clone(), Arc::new(Block::new(&name)));
    let space = Space::Block(name);
    for i in items {
        d.add(&space, Common { layer: LAYER.into(), ..Common::default() }, entity_of(i)).map_err(|e| e.to_string())?;
    }
    ensure_insert(d, plan);
    Ok(())
}

/// Per view whose block must change: the view and its wanted items.
fn plan_changes(d: &Drawing) -> Result<Vec<(Id, Vec<Item>)>, String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    let blocks: Vec<(Id, String)> = doc.views.iter().map(|v| (v.id(), view_block_name(v.id()))).collect();
    let refs = sheet_refs(d, &blocks);
    let sheet_of = |id: Id| refs.iter().find(|(v, _, _)| *v == id).map(|(_, n, l)| format!("{n}/{l}"));
    let mut out = Vec::new();
    for p in doc.views.iter().map(AnyView::id) {
        let want = wanted(d, p, &sheet_of)?;
        let changed = match d.block(&annotation_block_name(p)) {
            Some(b) => !block_matches(b, &want),
            None => !want.is_empty(),
        };
        if changed {
            out.push((p, want));
        }
    }
    Ok(out)
}

/// The after-command observer: every view's [`annotation_block_name`] block made to equal its
/// wanted annotation — untouched when it already does, rewritten (and given its INSERT) when not,
/// emptied but kept when the view now wants nothing. The drawing is written back only when some
/// block changed; an error is echoed and changes nothing.
pub fn annotation_observer(s: &mut Session, _before: &Arc<Drawing>) {
    let todo = match s.doc().map_err(|e| e.to_string()).and_then(plan_changes) {
        Ok(t) if t.is_empty() => return,
        Ok(t) => t,
        Err(e) => {
            s.echo(format!("LAMB annotations: {e}"));
            return;
        }
    };
    let Ok(current) = s.doc() else { return };
    let mut after = current.clone();
    for (p, items) in &todo {
        if let Err(e) = replace_block(&mut after, *p, items) {
            s.echo(format!("LAMB annotations: {e}"));
            return;
        }
    }
    if let Ok(d) = s.doc_mut() {
        *d = after;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    /// LAMBPROJECT's placed ground-floor plan, edited, with section `A-A` cut from (0,0) to
    /// (5000,0); observers installed. Returns (session, plan, section).
    fn fixture() -> T<(Session, Id, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let r =
            s.execute("newsectionline", &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 0}, "cutTo": {"x": 5000, "y": 0} }))?;
        let sec = Id(r["viewId"].as_u64().ok_or("no id")?);
        Ok((s, plan, sec))
    }

    fn block_of(s: &Session, plan: Id) -> Option<Arc<Block>> {
        s.doc().ok()?.blocks.get(&annotation_block_name(plan)).cloned()
    }

    /// The block's content as items (layer checked to be `0`).
    fn items(s: &Session, plan: Id) -> T<Vec<Item>> {
        let b = block_of(s, plan).ok_or("no annotation block")?;
        b.entities
            .iter()
            .map(|e| {
                if e.common.layer != LAYER {
                    return Err("not on layer 0".into());
                }
                match &e.kind {
                    EntityKind::Line(l) => Ok(Item::Line(l.a, l.b)),
                    EntityKind::Text(t) if t.halign == HAlign::Left => Ok(Item::Label(t.align_pt.unwrap_or(t.insert), t.value.clone())),
                    EntityKind::Text(t) => Ok(Item::Text(t.align_pt.unwrap_or(t.insert), t.value.clone())),
                    EntityKind::Dimension(dm) if is_vertical(&dm.kind) => Ok(Item::Dim(dm.p13, dm.p14)),
                    _ => Err("unexpected kind".into()),
                }
            })
            .collect()
    }

    fn texts(s: &Session, plan: Id) -> T<Vec<String>> {
        Ok(items(s, plan)?.into_iter().filter_map(|i| if let Item::Text(_, t) | Item::Label(_, t) = i { Some(t) } else { None }).collect())
    }

    fn undo_len(s: &Session) -> T<usize> {
        Ok(s.state()?.undo.len())
    }

    fn inserts(s: &Session, plan: Id) -> T<Vec<String>> {
        let name = annotation_block_name(plan);
        Ok(s.doc()?.model.iter().filter(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == name)).map(|e| e.common.layer.clone()).collect())
    }

    fn close(a: Vec3, x: f64, y: f64) -> bool {
        (a.x - x).abs() < 1e-6 && (a.y - y).abs() < 1e-6
    }

    #[test]
    fn a_section_line_gets_its_cut_symbol_in_the_same_undo_step() -> R {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let n = undo_len(&s)?;
        let r =
            s.execute("newsectionline", &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 0}, "cutTo": {"x": 5000, "y": 0} }))?;
        let _sec = Id(r["viewId"].as_u64().ok_or("no id")?);
        assert_eq!(undo_len(&s)?, n + 1);
        let it = items(&s, plan)?;
        assert_eq!(it.len(), 1 + 2 * 3 + 2, "{it:?}");
        let Item::Line(a, b) = &it[0] else { return Err("first is not the cut line".into()) };
        assert!(close(*a, 0.0, 0.0) && close(*b, 5000.0, 0.0), "{a:?} {b:?}");
        // The arrows point along the gaze, perpendicular to the cut.
        let Item::Line(e, tip) = &it[1] else { return Err("no arrow".into()) };
        assert!(close(*e, 0.0, 0.0));
        assert!((tip.x).abs() < 1e-6 && (tip.y.abs() - CUT_ARROW_MM).abs() < 1e-6, "{tip:?}");
        let gy = tip.y.signum();
        let Item::Line(t2, h) = &it[2] else { return Err("no head".into()) };
        assert!(near(*t2, *tip));
        assert!(((h.x - tip.x).hypot(h.y - tip.y) - CUT_HEAD_MM).abs() < 1e-6 && (h.y - tip.y) * gy < 0.0, "head points back: {h:?}");
        let Item::Line(e2, _) = &it[4] else { return Err("no second arrow".into()) };
        assert!(close(*e2, 5000.0, 0.0));
        let k = gy * (CUT_ARROW_MM + CUT_TEXT_GAP_MM);
        assert_eq!(it[7..].to_vec(), vec![Item::Text(v(0.0, k), "A-A".into()), Item::Text(v(5000.0, k), "A-A".into())]);
        assert_eq!(inserts(&s, plan)?, [view_block_name(plan)]);
        let ins = s.doc()?.model.iter().find(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == annotation_block_name(plan))).cloned();
        let Some(EntityKind::Insert(i)) = ins.as_deref().map(|e| &e.kind) else { return Err("no insert".into()) };
        assert_eq!((i.insert, i.scale, i.rotation), (Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0), 0.0));

        s.undo()?;
        assert!(block_of(&s, plan).is_none() && inserts(&s, plan)?.is_empty());
        s.redo()?;
        assert_eq!(items(&s, plan)?, it);
        Ok(())
    }

    #[test]
    fn rename_and_flip_regenerate_unrelated_commands_rewrite_nothing_unplace_empties() -> R {
        let (mut s, plan, sec) = fixture()?;
        let first = items(&s, plan)?;
        let b0 = block_of(&s, plan).ok_or("no block")?;
        s.execute("line", &json!({ "points": [[9000, 9000], [9500, 9000]] }))?;
        assert!(Arc::ptr_eq(&b0, &block_of(&s, plan).ok_or("no block")?), "an unrelated command leaves the block alone");

        s.execute("viewprops", &json!({ "mode": "rename", "viewId": sec.0, "name": "B-B" }))?;
        assert_eq!(texts(&s, plan)?, ["B-B", "B-B"]);

        s.execute("viewprops", &json!({ "mode": "flip", "viewId": sec.0, "flipped": true }))?;
        let flipped = items(&s, plan)?;
        let (Item::Line(_, t0), Item::Line(_, t1)) = (&first[1], &flipped[1]) else { return Err("no arrows".into()) };
        assert!(near(*t1, v(t0.x, -t0.y)), "the arrow turns round: {t0:?} -> {t1:?}");

        s.execute("unplaceview", &json!({ "viewId": sec.0 }))?;
        assert!(items(&s, plan)?.is_empty(), "emptied");
        assert_eq!(inserts(&s, plan)?.len(), 1, "the insert stays");
        Ok(())
    }

    #[test]
    fn a_sheet_showing_the_section_adds_its_reference() -> R {
        let (mut s, plan, sec) = fixture()?;
        let n = undo_len(&s)?;
        s.execute("newsheet", &json!({ "name": "A301", "viewIds": [plan.0, sec.0] }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        assert_eq!(texts(&s, plan)?, ["A-A", "2/A301", "A-A", "2/A301"]);
        let it = items(&s, plan)?;
        let (Item::Text(a, _), Item::Text(b, _)) = (&it[7], &it[8]) else { return Err("no texts".into()) };
        assert!(close(*b, a.x, a.y - 1.5 * CUT_TEXT_MM));
        s.undo()?;
        assert_eq!(texts(&s, plan)?, ["A-A", "A-A"]);
        Ok(())
    }

    #[test]
    fn derivation_ignores_the_block_and_a_dxf_round_trip_keeps_it() -> R {
        let (s, plan, _sec) = fixture()?;
        let d = s.doc()?;
        let mut bare = d.clone();
        bare.blocks.remove(&annotation_block_name(plan));
        let h =
            bare.model.iter().find(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == annotation_block_name(plan))).ok_or("no insert")?.handle;
        bare.model.remove(h);
        let (with, without) = (DrawingProject::new(d)?, DrawingProject::new(&bare)?);
        let ents = |p: &DrawingProject| -> String {
            use lamb_core::derivation::read::ProjectRead;
            let ids: Vec<Id> = p.views().map(|v| v.id).collect();
            ids.iter().map(|id| format!("{:?}", p.entities(*id).collect::<Vec<_>>())).collect()
        };
        assert!(!ents(&with).is_empty());
        assert_eq!(ents(&with), ents(&without));
        let markers = |p: &DrawingProject| format!("{:?}", lamb_core::derivation::markers::compute_markers_for_view(p, plan));
        assert_eq!(markers(&with), markers(&without));

        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(d).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert!(back.model.iter().any(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == annotation_block_name(plan))));
        assert!(plan_changes(&back)?.is_empty(), "the observer would change nothing after a round trip");
        Ok(())
    }
    /// [`fixture`] plus `Level 2` at 3000 and a 4000-wide rectangle drawn in the section from
    /// (0,0); returns (session, plan, section, level 1, level 2).
    fn levelled() -> T<(Session, Id, Id, Id, Id)> {
        let (mut s, plan, sec) = fixture()?;
        let l1 = read_document(s.doc()?)?.levels.first().ok_or("no level")?.id;
        let l2 =
            Id(s.execute("level", &json!({ "mode": "create", "name": "Level 2", "floorElevationMm": 3000 }))?["levelId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [4000, 0], [4000, 3000], [0, 3000]], "closed": true }))?;
        Ok((s, plan, sec, l1, l2))
    }

    /// The level lines' (y, x0, x1, label) per level, in block order.
    fn levels_of(s: &Session, view: Id) -> T<Vec<(f64, f64, f64, String)>> {
        let it: Vec<Item> = items(s, view)?.into_iter().filter(|i| !matches!(i, Item::Dim(..))).collect();
        it.chunks(2)
            .map(|c| match c {
                [Item::Line(a, b), Item::Label(at, t)] => {
                    assert!((a.y - b.y).abs() < 1e-9 && (at.y - a.y).abs() < 1e-9 && (at.x - b.x - LEVEL_TEXT_GAP_MM).abs() < 1e-9);
                    Ok((a.y, a.x, b.x, t.clone()))
                }
                _ => Err(format!("not a level line: {c:?}").into()),
            })
            .collect()
    }

    fn local_y(s: &Session, view: Id, z: f64) -> T<f64> {
        let d = s.doc()?;
        let dp = DrawingProject::new(d)?;
        let vr = dp.view(view).ok_or("no view")?;
        Ok(vertical_uz_to_local(&vr, Uz { u: 0.0, z }).y)
    }

    #[test]
    fn a_placed_section_gets_a_level_line_per_level_and_its_block_is_unchanged() -> R {
        let (s, plan, sec, _l1, _l2) = levelled()?;
        let (y0, y1) = (local_y(&s, sec, 0.0)?, local_y(&s, sec, 3000.0)?);
        assert!((y1 - y0 - 3000.0).abs() < 1e-6, "{y0} {y1}");
        let ext = s.doc()?.extents(&Space::Block(view_block_name(sec)));
        let (x0, x1) = (ext.min.x - LEVEL_OVERRUN_MM, ext.max.x + LEVEL_OVERRUN_MM);
        let got = levels_of(&s, sec)?;
        assert_eq!(got.len(), 2, "{got:?}");
        for ((y, a, b, t), (wy, wt)) in got.iter().zip([(y0, "Level 1 +0"), (y1, "Level 2 +3000")]) {
            assert!((y - wy).abs() < 1e-6 && (a - x0).abs() < 1e-6 && (b - x1).abs() < 1e-6, "{y} {a} {b}");
            assert_eq!(t, wt);
        }
        // The section's own block holds only what was drawn; the plan keeps its cut symbols only.
        assert_eq!(s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.len(), 1);
        assert_eq!(texts(&s, plan)?, ["A-A", "A-A"]);
        assert_eq!(inserts(&s, sec)?, [view_block_name(sec)]);
        let t = s.doc()?.block(&annotation_block_name(sec)).ok_or("no block")?;
        let Some(EntityKind::Text(tx)) = t.entities.iter().nth(1).map(|e| &e.kind) else { return Err("no label".into()) };
        assert_eq!((tx.height, tx.halign), (LEVEL_TEXT_MM, HAlign::Left));
        Ok(())
    }

    #[test]
    fn level_edits_move_and_relabel_the_line_in_one_undo_step_and_longer_geometry_widens_it() -> R {
        let (mut s, _plan, sec, l1, l2) = levelled()?;
        let before = levels_of(&s, sec)?;
        let n = undo_len(&s)?;
        s.execute("level", &json!({ "mode": "elevation", "levelId": l2.0, "floorElevationMm": 3300 }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        let moved = levels_of(&s, sec)?;
        assert!((moved[1].0 - local_y(&s, sec, 3300.0)?).abs() < 1e-6 && moved[1].3 == "Level 2 +3300", "{moved:?}");
        s.undo()?;
        assert_eq!(levels_of(&s, sec)?, before);

        s.execute("level", &json!({ "mode": "rename", "levelId": l1.0, "name": "Basement" }))?;
        s.execute("level", &json!({ "mode": "elevation", "levelId": l1.0, "floorElevationMm": -2700 }))?;
        assert_eq!(levels_of(&s, sec)?[0].3, "Basement -2700");

        s.execute("line", &json!({ "points": [[-2000, 500], [9000, 500]] }))?;
        let wide = levels_of(&s, sec)?;
        assert!(
            wide.iter().all(|l| (l.1 - (-2000.0 - LEVEL_OVERRUN_MM)).abs() < 1e-6 && (l.2 - (9000.0 + LEVEL_OVERRUN_MM)).abs() < 1e-6),
            "{wide:?}"
        );
        Ok(())
    }

    #[test]
    fn elevations_get_them_unplaced_verticals_and_plans_do_not_unrelated_commands_rewrite_nothing() -> R {
        let (mut s, plan, sec, _l1, _l2) = levelled()?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let r = s.execute(
            "newelevationline",
            &json!({ "name": "North", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 8000}, "cutTo": {"x": 5000, "y": 8000} }),
        )?;
        let el = Id(r["viewId"].as_u64().ok_or("no id")?);
        assert!(block_of(&s, el).is_none_or(|b| b.entities.is_empty()), "an empty elevation wants nothing");
        s.execute("editview", &json!({ "viewId": el.0 }))?;
        s.execute("line", &json!({ "points": [[0, 0], [5000, 0]] }))?;
        assert_eq!(levels_of(&s, el)?.len(), 2);
        assert!(items(&s, plan)?.iter().all(|i| !matches!(i, Item::Label(..))), "a plan gets no level lines");

        let b0 = block_of(&s, sec).ok_or("no block")?;
        s.execute("line", &json!({ "points": [[100, 100], [200, 100]] }))?;
        assert!(Arc::ptr_eq(&b0, &block_of(&s, sec).ok_or("no block")?), "an unrelated command leaves the block alone");

        s.execute("unplaceview", &json!({ "viewId": sec.0 }))?;
        assert!(items(&s, sec)?.is_empty(), "an unplaced vertical wants nothing");
        Ok(())
    }

    #[test]
    fn level_lines_change_no_marker_link_or_wall_graph_run() -> R {
        let (s, _plan, sec, _l1, _l2) = levelled()?;
        let d = s.doc()?;
        assert!(!items(&s, sec)?.is_empty());
        let mut bare = d.clone();
        bare.blocks.remove(&annotation_block_name(sec));
        let h =
            bare.model.iter().find(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == annotation_block_name(sec))).ok_or("no insert")?.handle;
        bare.model.remove(h);
        let (with, without) = (DrawingProject::new(d)?, DrawingProject::new(&bare)?);
        let all = |p: &DrawingProject| -> String {
            let ids: Vec<Id> = p.views().map(|v| v.id).collect();
            let ents: String = ids.iter().map(|id| format!("{:?}", p.entities(*id).collect::<Vec<_>>())).collect();
            let marks: String = ids.iter().map(|id| format!("{:?}", lamb_core::derivation::markers::compute_markers_for_view(p, *id))).collect();
            let params = lamb_core::derivation::walls::WallRunParams::for_vertical(p.layers());
            let walls = format!("{:?}", lamb_core::derivation::wall_junctions::wall_graph(p, sec, &params));
            format!("{ents}|{marks}|{walls}|{:?}", p.document().links)
        };
        assert_eq!(all(&with), all(&without));
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(d).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert!(plan_changes(&back)?.is_empty(), "level labels survive a DXF round trip unchanged");
        Ok(())
    }

    /// The view's floor-to-floor dimensions, in block order.
    fn dims_of(s: &Session, view: Id) -> T<Vec<Dimension>> {
        let b = block_of(s, view).ok_or("no annotation block")?;
        Ok(b.entities.iter().filter_map(|e| if let EntityKind::Dimension(d) = &e.kind { Some(d.clone()) } else { None }).collect())
    }

    /// The value CADCraft displays for `dm`, under the drawing's own `Standard` style.
    fn measured(s: &Session, dm: &Dimension) -> T<f64> {
        let st = s.doc()?.dim_style(&dm.style).cloned().unwrap_or_default();
        Ok(cadcraft_render::dimension_geometry(dm, &st, 1.0).measurement)
    }

    /// The right end of the section's level lines.
    fn x1_of(s: &Session, view: Id) -> T<f64> {
        Ok(s.doc()?.extents(&Space::Block(view_block_name(view))).max.x + LEVEL_OVERRUN_MM)
    }

    #[test]
    fn consecutive_levels_get_a_vertical_floor_to_floor_dimension_measuring_their_height() -> R {
        let (s, _plan, sec, _l1, _l2) = levelled()?;
        let dims = dims_of(&s, sec)?;
        assert_eq!(dims.len(), 1, "{dims:?}");
        let (dm, x1) = (&dims[0], x1_of(&s, sec)?);
        assert!(close(dm.p13, x1, local_y(&s, sec, 0.0)?) && close(dm.p14, x1, local_y(&s, sec, 3000.0)?), "{dm:?}");
        assert!((dm.defpt.x - (x1 + LEVEL_DIM_OFFSET_MM)).abs() < 1e-6);
        assert!(is_vertical(&dm.kind) && dm.text.is_empty() && dm.style == "Standard" && dm.assoc.is_empty() && dm.block.is_none());
        assert!((measured(&s, dm)? - 3000.0).abs() < 1e-6, "{}", measured(&s, dm)?);
        // The section's own block still holds only what was drawn.
        assert_eq!(s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.len(), 1);
        Ok(())
    }

    #[test]
    fn levels_out_of_order_are_dimensioned_by_elevation() -> R {
        let (mut s, _plan, sec) = fixture()?;
        // Level 1 (order 0) at 0; then order 1 at 6000 and order 2 at 3000.
        s.execute("level", &json!({ "mode": "create", "name": "Roof", "floorElevationMm": 6000, "order": 1 }))?;
        s.execute("level", &json!({ "mode": "create", "name": "Level 2", "floorElevationMm": 3000, "order": 2 }))?;
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.execute("line", &json!({ "points": [[0, 0], [4000, 0]] }))?;
        let dims = dims_of(&s, sec)?;
        let spans: Vec<(f64, f64, f64)> = dims.iter().map(|d| (d.p13.y, d.p14.y, measured(&s, d).unwrap_or(f64::NAN))).collect();
        let want = [(0.0, 3000.0), (3000.0, 6000.0)].map(|(a, b)| (local_y(&s, sec, a), local_y(&s, sec, b)));
        assert_eq!(spans.len(), 2, "{spans:?}");
        for ((a, b, m), (wa, wb)) in spans.iter().zip(want) {
            assert!((a - wa?).abs() < 1e-6 && (b - wb?).abs() < 1e-6 && (m - 3000.0).abs() < 1e-6, "{spans:?}");
        }
        Ok(())
    }

    #[test]
    fn a_level_edit_moves_the_dimension_in_one_undo_step_and_unrelated_commands_leave_it() -> R {
        let (mut s, _plan, sec, _l1, l2) = levelled()?;
        let before = dims_of(&s, sec)?;
        let b0 = block_of(&s, sec).ok_or("no block")?;
        s.execute("line", &json!({ "points": [[100, 100], [200, 100]] }))?;
        assert!(Arc::ptr_eq(&b0, &block_of(&s, sec).ok_or("no block")?), "an unrelated command leaves the block alone");

        let n = undo_len(&s)?;
        s.execute("level", &json!({ "mode": "elevation", "levelId": l2.0, "floorElevationMm": 3200 }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        let moved = dims_of(&s, sec)?;
        assert_eq!(moved.len(), 1);
        assert!((moved[0].p14.y - local_y(&s, sec, 3200.0)?).abs() < 1e-6 && (measured(&s, &moved[0])? - 3200.0).abs() < 1e-6);
        s.undo()?;
        assert_eq!(dims_of(&s, sec)?, before);

        s.execute("level", &json!({ "mode": "elevation", "levelId": l2.0, "floorElevationMm": 0 }))?;
        assert!(dims_of(&s, sec)?.is_empty(), "two levels at one elevation get no dimension");
        assert_eq!(levels_of(&s, sec)?.len(), 2, "but keep both level lines");
        Ok(())
    }

    #[test]
    fn one_level_gets_no_dimension() -> R {
        let (mut s, _plan, sec) = fixture()?;
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.execute("line", &json!({ "points": [[0, 0], [4000, 0]] }))?;
        assert_eq!(levels_of(&s, sec)?.len(), 1);
        assert!(dims_of(&s, sec)?.is_empty());
        Ok(())
    }

    #[test]
    fn a_dimensioned_section_survives_a_dxf_round_trip_and_derivation_ignores_it() -> R {
        let (s, _plan, sec, _l1, _l2) = levelled()?;
        let d = s.doc()?;
        assert_eq!(dims_of(&s, sec)?.len(), 1);
        let mut bare = d.clone();
        bare.blocks.remove(&annotation_block_name(sec));
        let (with, without) = (DrawingProject::new(d)?, DrawingProject::new(&bare)?);
        let all = |p: &DrawingProject| -> String {
            let ids: Vec<Id> = p.views().map(|v| v.id).collect();
            let ents: String = ids.iter().map(|id| format!("{:?}", p.entities(*id).collect::<Vec<_>>())).collect();
            let marks: String = ids.iter().map(|id| format!("{:?}", lamb_core::derivation::markers::compute_markers_for_view(p, *id))).collect();
            format!("{ents}|{marks}|{:?}", p.document().links)
        };
        assert_eq!(all(&with), all(&without));
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(d).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert!(plan_changes(&back)?.is_empty(), "the observer would change nothing after a round trip");
        Ok(())
    }
}

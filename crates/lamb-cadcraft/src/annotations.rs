//! Generated annotation (LAMB's `docs/07-architecture/cadcraft-integration.md` §5, LAMB T474):
//! every placed plan gets a plotted section cut symbol for each placed vertical view whose cut line
//! projects into it — the cut LINE, a gaze arrow at each end, and the section's name at each end
//! (plus `<n>/<layout>` when a sheet shows the section, [`crate::sheets::sheet_refs`]).
//!
//! The symbols live in their own block per plan, [`annotation_block_name`]`(plan)`, never in the
//! view block, so derivation, the wall graph, links and LAMBSAVE never see them. The block's one
//! model-space INSERT sits at the origin on the plan's own view layer, so a sheet viewport showing
//! the plan shows its annotation and every other viewport freezes it. [`annotation_observer`]
//! regenerates the blocks after every command, writing the drawing back only when one changed, so
//! the change lands in the command's own undo step.

use std::sync::Arc;

use cadcraft_doc::{Block, Common, Drawing, Entity, EntityKind, HAlign, Insert, Line, Space, Text, VAlign};
use cadcraft_engine::Session;
use cadcraft_geom::Vec3;
use lamb_core::derivation::markers::compute_cut_lines_for_view;
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
/// Every generated entity's layer inside the block.
const LAYER: &str = "0";

/// One generated entity, in plan-local coordinates.
#[derive(Clone, Debug, PartialEq)]
enum Item {
    Line(Vec3, Vec3),
    Text(Vec3, String),
}

fn v(x: f64, y: f64) -> Vec3 {
    Vec3::new(x, y, 0.0)
}

fn near(a: Vec3, b: Vec3) -> bool {
    (a.x - b.x).hypot(a.y - b.y) <= TOLERANCE.coincidence_mm
}

/// The cut symbols plan `plan` wants, in write order; nothing for an unplaced plan.
fn wanted(d: &Drawing, plan: Id, sheet_of: &dyn Fn(Id) -> Option<String>) -> Result<Vec<Item>, String> {
    let dp = DrawingProject::new(d).map_err(|e| e.to_string())?;
    if dp.document().views.iter().find(|v| v.id() == plan).is_none_or(|v| v.placement().is_none()) {
        return Ok(Vec::new());
    }
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

fn text(at: Vec3, value: &str) -> Text {
    Text {
        insert: at,
        align_pt: Some(at),
        height: CUT_TEXT_MM,
        value: value.into(),
        rotation: 0.0,
        width_factor: 1.0,
        oblique: 0.0,
        style: "Standard".into(),
        halign: HAlign::Center,
        valign: VAlign::Middle,
    }
}

fn entity_of(i: &Item) -> EntityKind {
    match i {
        Item::Line(a, b) => EntityKind::Line(Line { a: *a, b: *b }),
        Item::Text(at, s) => EntityKind::Text(text(*at, s)),
    }
}

/// `true` when `block` holds exactly `want`, in order: each on layer `0`, LINE ends and TEXT
/// alignment points within [`TOLERANCE`]`.coincidence_mm`, the same text strings.
fn block_matches(block: &Block, want: &[Item]) -> bool {
    block.entities.len() == want.len()
        && block.entities.iter().zip(want).all(|(e, w)| {
            e.common.layer == LAYER
                && match (&e.kind, w) {
                    (EntityKind::Line(l), Item::Line(a, b)) => near(l.a, *a) && near(l.b, *b),
                    (EntityKind::Text(t), Item::Text(at, s)) => near(t.align_pt.unwrap_or(t.insert), *at) && t.value == *s,
                    _ => false,
                }
        })
}

/// The block's canonical model-space INSERT: at the origin, scale 1, on the plan's view layer.
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

/// Per plan whose block must change: the plan and its wanted items.
fn plan_changes(d: &Drawing) -> Result<Vec<(Id, Vec<Item>)>, String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    let blocks: Vec<(Id, String)> = doc.views.iter().map(|v| (v.id(), view_block_name(v.id()))).collect();
    let refs = sheet_refs(d, &blocks);
    let sheet_of = |id: Id| refs.iter().find(|(v, _, _)| *v == id).map(|(_, n, l)| format!("{n}/{l}"));
    let mut out = Vec::new();
    for p in doc.views.iter().filter(|v| matches!(v, AnyView::Plan(_))).map(AnyView::id) {
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

/// The after-command observer: every plan's [`annotation_block_name`] block made to equal its
/// wanted cut symbols — untouched when it already does, rewritten (and given its INSERT) when not,
/// emptied but kept when the plan now wants nothing. The drawing is written back only when some
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
                    EntityKind::Text(t) => Ok(Item::Text(t.align_pt.unwrap_or(t.insert), t.value.clone())),
                    _ => Err("unexpected kind".into()),
                }
            })
            .collect()
    }

    fn texts(s: &Session, plan: Id) -> T<Vec<String>> {
        Ok(items(s, plan)?.into_iter().filter_map(|i| if let Item::Text(_, t) = i { Some(t) } else { None }).collect())
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
}

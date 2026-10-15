//! Detail-tag bubbles (LAMB's `docs/07-architecture/cadcraft-integration.md` §9, LAMB T395): a
//! detail tag is a leader ([`crate::detailtags`]) plus a bubble — an `Insert` of the shared block
//! [`BUBBLE_BLOCK`] at the leader's last vertex, on the leader's layer, with a `DETAIL` attribute
//! holding the detail view's name (`?` when that view is gone) and a `SHEET` attribute naming the
//! sheet that shows the detail (`<n>/<layout>`, [`crate::sheets::sheet_refs`]; empty when none).
//! DXF/DWG see an ordinary callout.
//!
//! Nothing links a bubble to its leader: a bubble belongs to the tag leader whose last vertex
//! coincides with its insert point in the same view block. [`bubble_observer`] reconciles every
//! plan view's bubbles with its tag leaders after every command, writing the drawing back only when
//! something changed, so the bubble change lands in the command's own undo step.

use std::sync::Arc;

use cadcraft_doc::{Attrib, Block, Circle, Common, Drawing, Entity, EntityKind, HAlign, Handle, Insert, Line, Space, Text, VAlign};
use cadcraft_engine::Session;
use cadcraft_geom::Vec3;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AnyView, Id};

use crate::schema::{read_document, read_entity_data, view_block_name};
use crate::sheets::sheet_refs;

/// The bubble block's name; one definition per drawing.
pub const BUBBLE_BLOCK: &str = "LAMB_DTAG";
/// The bubble circle's radius.
pub const BUBBLE_RADIUS_MM: f64 = 250.0;
/// The bubble attributes' text height.
pub const BUBBLE_TEXT_MM: f64 = 120.0;
/// `DETAIL` sits centred just above the bubble's line, `SHEET` centred below it.
const DETAIL_OFFSET_MM: f64 = 60.0;
const SHEET_OFFSET_MM: f64 = -180.0;

/// One bubble a tag leader wants: where, on which layer, and its two attribute values.
#[derive(Clone, Debug, PartialEq)]
struct Wanted {
    at: Vec3,
    layer: String,
    detail: String,
    sheet: String,
}

fn near(a: Vec3, b: Vec3) -> bool {
    (a.x - b.x).hypot(a.y - b.y) <= TOLERANCE.coincidence_mm
}

fn attrib(tag: &str, at: Vec3, dy: f64, value: &str) -> Attrib {
    let p = Vec3::new(at.x, at.y + dy, at.z);
    Attrib {
        tag: tag.into(),
        text: Text {
            insert: p,
            align_pt: Some(p),
            height: BUBBLE_TEXT_MM,
            value: value.into(),
            rotation: 0.0,
            width_factor: 1.0,
            oblique: 0.0,
            style: "Standard".into(),
            halign: HAlign::Center,
            valign: VAlign::Bottom,
        },
        invisible: false,
        constant: false,
        prompt: String::new(),
        props: Default::default(),
        multiline: None,
        modes: Default::default(),
    }
}

fn insert_of(w: &Wanted) -> Insert {
    Insert {
        block: BUBBLE_BLOCK.into(),
        insert: w.at,
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: vec![attrib("DETAIL", w.at, DETAIL_OFFSET_MM, &w.detail), attrib("SHEET", w.at, SHEET_OFFSET_MM, &w.sheet)],
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    }
}

fn attrib_value<'a>(i: &'a Insert, tag: &str) -> Option<&'a str> {
    i.attribs.iter().find(|a| a.tag.eq_ignore_ascii_case(tag)).map(|a| a.text.value.as_str())
}

/// Whether `e` is the bubble `w` wants, exactly.
fn matches(e: &Entity, i: &Insert, w: &Wanted) -> bool {
    e.common.layer == w.layer
        && near(i.insert, w.at)
        && attrib_value(i, "DETAIL") == Some(w.detail.as_str())
        && attrib_value(i, "SHEET") == Some(w.sheet.as_str())
}

/// The block definition: a circle and a horizontal line across it, on layer `0`.
fn ensure_block(d: &mut Drawing) -> Result<(), String> {
    if d.block(BUBBLE_BLOCK).is_some() {
        return Ok(());
    }
    d.blocks.insert(BUBBLE_BLOCK.into(), Arc::new(Block::new(BUBBLE_BLOCK)));
    let space = Space::Block(BUBBLE_BLOCK.into());
    let common = || Common { layer: "0".into(), ..Common::default() };
    let r = BUBBLE_RADIUS_MM;
    d.add(&space, common(), EntityKind::Circle(Circle { center: Vec3::ZERO, radius: r })).map_err(|e| e.to_string())?;
    d.add(&space, common(), EntityKind::Line(Line { a: Vec3::new(-r, 0.0, 0.0), b: Vec3::new(r, 0.0, 0.0) })).map_err(|e| e.to_string())?;
    Ok(())
}

/// Per plan view block: the bubbles to remove and the ones to add.
type Plan = Vec<(String, Vec<Handle>, Vec<Wanted>)>;

fn plan(d: &Drawing) -> Result<Plan, String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    let name_of = |id| doc.views.iter().find(|v| matches!(v, AnyView::Detail(_)) && v.id() == id).map(|v| v.name().to_owned());
    let blocks: Vec<(Id, String)> = doc.views.iter().map(|v| (v.id(), view_block_name(v.id()))).collect();
    let refs = sheet_refs(d, &blocks);
    let sheet_of = |id: Id| refs.iter().find(|(v, _, _)| *v == id).map_or_else(String::new, |(_, n, l)| format!("{n}/{l}"));
    let mut out = Vec::new();
    for v in doc.views.iter().filter(|v| matches!(v, AnyView::Plan(_))) {
        let block_name = view_block_name(v.id());
        let Some(block) = d.block(&block_name) else { continue };
        let mut wanted = Vec::new();
        for e in block.entities.iter() {
            let EntityKind::Leader(l) = &e.kind else { continue };
            let Some(tag) = read_entity_data(&e.common).map_err(|e| e.to_string())?.detail_tag else { continue };
            let Some(&at) = l.vertices.last() else { continue };
            let detail = name_of(tag.detail_view_id).unwrap_or_else(|| "?".into());
            let sheet = sheet_of(tag.detail_view_id);
            wanted.push(Wanted { at, layer: e.common.layer.clone(), detail, sheet });
        }
        let mut remove = Vec::new();
        for e in block.entities.iter() {
            let EntityKind::Insert(i) = &e.kind else { continue };
            if !i.block.eq_ignore_ascii_case(BUBBLE_BLOCK) {
                continue;
            }
            match wanted.iter().position(|w| matches(e, i, w)) {
                Some(k) => {
                    wanted.remove(k);
                }
                None => remove.push(e.handle),
            }
        }
        if !remove.is_empty() || !wanted.is_empty() {
            out.push((block_name, remove, wanted));
        }
    }
    Ok(out)
}

fn apply(d: &mut Drawing, plan: &Plan) -> Result<(), String> {
    if plan.iter().any(|(_, _, add)| !add.is_empty()) {
        ensure_block(d)?;
    }
    for (block, remove, add) in plan {
        let space = Space::Block(block.clone());
        let store = d.space_mut(&space).ok_or("view block gone")?;
        for h in remove {
            store.remove(*h);
        }
        for w in add {
            let common = Common { layer: w.layer.clone(), ..Common::default() };
            d.add(&space, common, EntityKind::Insert(insert_of(w))).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// The after-command observer: every plan view's `LAMB_DTAG` inserts made to equal its tag
/// leaders' wanted bubbles — matching ones kept, the rest erased, missing ones added. The drawing
/// is written back only when something changed.
pub fn bubble_observer(s: &mut Session, _before: &Arc<Drawing>) {
    let todo = match s.doc().map_err(|e| e.to_string()).and_then(plan) {
        Ok(p) if p.is_empty() => return,
        Ok(p) => p,
        Err(e) => {
            s.echo(format!("LAMB bubbles: {e}"));
            return;
        }
    };
    let Ok(current) = s.doc() else { return };
    let mut after = current.clone();
    if let Err(e) = apply(&mut after, &todo) {
        s.echo(format!("LAMB bubbles: {e}"));
        return;
    }
    if let Ok(d) = s.doc_mut() {
        *d = after;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_engine::Session;
    use lamb_core::model::Id;
    use serde_json::{Value, json};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    /// detailtags.rs's room: a 5000 × 4000 room of 200 mm walls, detail `D1`, editing the plan on
    /// layer `TAGS`, observers installed. Returns (session, plan, D1).
    fn room() -> T<(Session, Id, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let detail = Id(s.execute("newdetail", &json!({ "name": "D1" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [5000, 0], [5000, 4000], [0, 4000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [4800, 200], [4800, 3800], [200, 3800]], "closed": true }))?;
        s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
        Ok((s, plan, detail))
    }

    /// A tag at `tip` with its bubble 1000 mm down-left; returns the leader's handle (hex).
    fn tag(s: &mut Session, detail: Id, tip: (f64, f64)) -> T<String> {
        let r = s.execute(
            "detailtag",
            &json!({ "detailViewId": detail.0, "tip": {"x": tip.0, "y": tip.1}, "bubble": {"x": tip.0 - 1000.0, "y": tip.1 - 1000.0} }),
        )?;
        Ok(r["handle"].as_str().ok_or("no handle")?.to_owned())
    }

    /// The plan block's bubbles: (handle, point, layer, DETAIL, SHEET).
    fn bubbles(s: &Session, plan: Id) -> T<Vec<(Handle, (f64, f64), String, String, String)>> {
        let b = s.doc()?.block(&view_block_name(plan)).ok_or("no block")?;
        Ok(b.entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Insert(i) if i.block == BUBBLE_BLOCK => Some((
                    e.handle,
                    (i.insert.x, i.insert.y),
                    e.common.layer.clone(),
                    attrib_value(i, "DETAIL").unwrap_or("-").to_owned(),
                    attrib_value(i, "SHEET").unwrap_or("-").to_owned(),
                )),
                _ => None,
            })
            .collect())
    }

    fn undo_len(s: &Session) -> T<usize> {
        Ok(s.state()?.undo.len())
    }

    #[test]
    fn a_tag_gets_one_bubble_that_undoes_and_redoes_with_it() -> R {
        let (mut s, plan, detail) = room()?;
        let n = undo_len(&s)?;
        tag(&mut s, detail, (2500.0, 0.0))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        let b = bubbles(&s, plan)?;
        assert_eq!(b.len(), 1, "{b:?}");
        assert_eq!((b[0].1, b[0].2.as_str(), b[0].3.as_str(), b[0].4.as_str()), ((1500.0, -1000.0), "TAGS", "D1", ""));
        let EntityKind::Insert(i) = &s.doc()?.entity(b[0].0).ok_or("gone")?.kind else { return Err("not an insert".into()) };
        let d = &i.attribs[0].text;
        assert_eq!(
            (d.align_pt, d.height, d.halign, d.valign),
            (Some(Vec3::new(1500.0, -940.0, 0.0)), BUBBLE_TEXT_MM, HAlign::Center, VAlign::Bottom)
        );
        assert_eq!(i.attribs[1].text.align_pt, Some(Vec3::new(1500.0, -1180.0, 0.0)));
        let def = s.doc()?.block(BUBBLE_BLOCK).ok_or("no block definition")?;
        assert_eq!(def.entities.len(), 2);
        assert!(def.entities.iter().all(|e| e.common.layer == "0"));

        s.undo()?;
        assert!(bubbles(&s, plan)?.is_empty());
        assert!(s.doc()?.block(&view_block_name(plan)).ok_or("no block")?.entities.iter().all(|e| !matches!(e.kind, EntityKind::Leader(_))));
        s.redo()?;
        assert_eq!(bubbles(&s, plan)?.len(), 1);
        Ok(())
    }

    #[test]
    fn the_bubble_follows_move_erase_rename_and_comes_back_when_erased_alone() -> R {
        let (mut s, plan, detail) = room()?;
        let leader = tag(&mut s, detail, (2500.0, 0.0))?;
        s.execute("move", &json!({ "handles": [leader], "delta": [500, 0] }))?;
        let b = bubbles(&s, plan)?;
        assert_eq!(b.iter().map(|b| b.1).collect::<Vec<_>>(), [(2000.0, -1000.0)]);

        s.execute("viewprops", &json!({ "mode": "rename", "viewId": detail.0, "name": "Eaves" }))?;
        assert_eq!(bubbles(&s, plan)?.iter().map(|b| b.3.clone()).collect::<Vec<_>>(), ["Eaves"]);

        let only = bubbles(&s, plan)?[0].0.hex();
        s.execute("erase", &json!({ "handles": [only] }))?;
        assert_eq!(bubbles(&s, plan)?.len(), 1, "the observer restores it in the same command");
        s.execute("line", &json!({ "points": [[9000, 9000], [9500, 9000]] }))?;
        assert_eq!(bubbles(&s, plan)?.len(), 1);

        s.execute("erase", &json!({ "handles": [leader] }))?;
        assert!(bubbles(&s, plan)?.is_empty());
        Ok(())
    }

    #[test]
    fn an_unrelated_command_leaves_bubbles_alone_and_two_tags_share_one_block() -> R {
        let (mut s, plan, detail) = room()?;
        tag(&mut s, detail, (2500.0, 0.0))?;
        tag(&mut s, detail, (2500.0, 4000.0))?;
        let before = bubbles(&s, plan)?;
        assert_eq!(before.len(), 2);
        let block_before = s.doc()?.block(BUBBLE_BLOCK).ok_or("no def")?.clone();
        s.execute("line", &json!({ "points": [[9000, 9000], [9500, 9000]] }))?;
        assert_eq!(bubbles(&s, plan)?, before, "same handles, same values");
        assert_eq!(s.doc()?.blocks.keys().filter(|k| k.eq_ignore_ascii_case(BUBBLE_BLOCK)).count(), 1);
        assert_eq!(s.doc()?.block(BUBBLE_BLOCK).ok_or("no def")?.entities.len(), block_before.entities.len());

        // A tag naming a deleted detail reads `?`.
        let d = s.doc_mut()?;
        let mut doc = read_document(d)?;
        doc.views.retain(|v| v.id() != detail);
        crate::schema::write_document(d, &doc)?;
        s.execute("line", &json!({ "points": [[9000, 9100], [9500, 9100]] }))?;
        assert!(bubbles(&s, plan)?.iter().all(|b| b.3 == "?"));
        Ok(())
    }

    #[test]
    fn a_dxf_round_trip_keeps_the_bubble_and_its_attributes() -> R {
        let (mut s, plan, detail) = room()?;
        tag(&mut s, detail, (2500.0, 0.0))?;
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert!(back.block(BUBBLE_BLOCK).is_some_and(|b| b.entities.len() == 2));
        let wanted = plan_for(&back)?;
        assert!(wanted.is_empty(), "the observer would change nothing after a round trip: {wanted:?}");
        let ins: Vec<(String, String)> = back
            .block(&view_block_name(plan))
            .ok_or("no block")?
            .entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Insert(i) if i.block == BUBBLE_BLOCK => {
                    Some((attrib_value(i, "DETAIL")?.to_owned(), attrib_value(i, "SHEET")?.to_owned()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(ins, [("D1".to_owned(), String::new())]);
        Ok(())
    }

    #[test]
    fn the_sheet_attribute_names_the_sheet_showing_the_detail() -> R {
        let (mut s, plan, detail) = room()?;
        tag(&mut s, detail, (2500.0, 0.0))?;
        let sheet = |s: &Session| -> T<Vec<String>> { Ok(bubbles(s, plan)?.into_iter().map(|b| b.4).collect()) };

        let n = undo_len(&s)?;
        s.execute("newsheet", &json!({ "name": "A501", "viewIds": [plan.0, detail.0] }))?;
        assert_eq!((sheet(&s)?, undo_len(&s)?), (vec!["2/A501".to_owned()], n + 1), "in NEWSHEET's own undo step");
        s.undo()?;
        assert_eq!(sheet(&s)?, [""]);
        s.redo()?;
        assert_eq!(sheet(&s)?, ["2/A501"]);

        let out = s.execute("newsheet", &json!({ "name": "A100", "viewIds": [detail.0] }))?;
        assert_eq!(sheet(&s)?, ["1/A100"], "name order wins");
        let vp = out["viewports"][0].as_str().ok_or("no viewport")?.to_owned();
        s.execute("erase", &json!({ "handles": [vp] }))?;
        assert_eq!(sheet(&s)?, ["2/A501"], "falls back when A100's viewport goes");

        let before = bubbles(&s, plan)?;
        let n = undo_len(&s)?;
        s.execute("line", &json!({ "points": [[9000, 9000], [9500, 9000]] }))?;
        assert_eq!((bubbles(&s, plan)?, undo_len(&s)?), (before, n + 1), "an unrelated command changes no bubble");
        Ok(())
    }

    fn plan_for(d: &Drawing) -> T<Vec<Value>> {
        Ok(plan(d)?.into_iter().map(|(b, r, a)| json!([b, r.len(), a.len()])).collect())
    }
}

//! Detail-tag bubbles (LAMB's `docs/07-architecture/cadcraft-integration.md` §9, LAMB T395): a
//! detail tag is a leader ([`crate::detailtags`]) plus a bubble — an `Insert` of the shared block
//! [`BUBBLE_BLOCK`] at the leader's last vertex, on the leader's layer, with a `DETAIL` attribute
//! holding the detail view's name (`?` when that view is gone) and a `SHEET` attribute naming the
//! sheet that shows the detail (`<n>/<layout>`, [`crate::sheets::sheet_refs`]; empty when none).
//! DXF/DWG see an ordinary callout.
//!
//! A tag leader and its bubble are the two members of one unnamed, selectable group
//! ([`GROUP_DESCRIPTION`], LAMB T509), so picking either selects both and MOVE/COPY/ERASE act on
//! the pair. The group is the bubble's identity: [`bubble_observer`] reconciles every plan view's
//! bubbles with its tag leaders after every command, updating a leader's bubble in place (same
//! handle), and writes the drawing back only when something changed, so the bubble change lands in
//! the command's own undo step. A bubble moved on its own (its grip) drags the leader's last vertex
//! with it; otherwise the bubble goes to the leader's last vertex. The bubble's attributes have
//! Lock position set, so they have no grips: the text can't be dragged off the bubble.

use std::sync::Arc;

use cadcraft_doc::{
    Attrib, AttribModes, Block, Circle, Common, Drawing, Entity, EntityKind, Group, HAlign, Handle, Insert, Line, Space, Text, VAlign,
};
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
/// The description of the unnamed group holding a tag leader and its bubble.
pub const GROUP_DESCRIPTION: &str = "LAMB detail tag";
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
        modes: AttribModes { lock_position: true, ..AttribModes::default() },
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
        && i.attribs.len() == 2
        && i.attribs.iter().all(|a| a.modes.lock_position)
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

/// Whether `g` is a tag leader's group (made by this module).
fn is_tag_group(g: &Group) -> bool {
    g.name.starts_with('*') && g.description == GROUP_DESCRIPTION
}

/// The first free unnamed group name, `*A1`, `*A2`… (the engine's scheme).
fn next_unnamed(groups: &[Group]) -> String {
    let used: std::collections::HashSet<String> = groups.iter().map(|g| g.name.to_ascii_uppercase()).collect();
    (1..=groups.len() + 1).map(|n| format!("*A{n}")).find(|n| !used.contains(n)).unwrap_or_else(|| format!("*A{}", groups.len() + 1))
}

/// A tag leader's bubble: one already in the view block, or the `k`-th one [`apply`] adds there.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Bubble {
    Kept(Handle),
    Added(usize),
}

/// One view block's changes.
#[derive(Debug, Default)]
struct BlockPlan {
    block: String,
    remove: Vec<Handle>,
    /// Bubbles rewritten in place.
    update: Vec<(Handle, Wanted)>,
    add: Vec<Wanted>,
    /// Leaders whose last vertex moves to their dragged bubble.
    leader_ends: Vec<(Handle, Vec3)>,
}

impl BlockPlan {
    fn is_empty(&self) -> bool {
        self.remove.is_empty() && self.update.is_empty() && self.add.is_empty() && self.leader_ends.is_empty()
    }
}

/// Every plan view block's changes, plus each tag leader's (block index, leader, bubble) — the
/// pairs the tag groups must hold — and whether the groups need rewriting.
#[derive(Debug, Default)]
struct Plan {
    blocks: Vec<BlockPlan>,
    pairs: Vec<(usize, Handle, Bubble)>,
    groups_dirty: bool,
}

impl Plan {
    fn is_empty(&self) -> bool {
        !self.groups_dirty && self.blocks.iter().all(BlockPlan::is_empty)
    }
}

fn insert_at(d: &Drawing, h: Handle) -> Option<Vec3> {
    match &d.entity(h)?.kind {
        EntityKind::Insert(i) => Some(i.insert),
        _ => None,
    }
}

fn leader_end(d: &Drawing, h: Handle) -> Option<Vec3> {
    match &d.entity(h)?.kind {
        EntityKind::Leader(l) => l.vertices.last().copied(),
        _ => None,
    }
}

fn is_bubble(e: &Entity) -> bool {
    matches!(&e.kind, EntityKind::Insert(i) if i.block.eq_ignore_ascii_case(BUBBLE_BLOCK))
}

/// The changes that make every plan view's bubbles and tag groups equal what its tag leaders want.
/// `before` (the drawing before the command) decides who drives: a bubble that moved while its
/// leader's last vertex did not drags that vertex; otherwise the bubble follows the leader.
fn plan(d: &Drawing, before: Option<&Drawing>) -> Result<Plan, String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    let name_of = |id| doc.views.iter().find(|v| matches!(v, AnyView::Detail(_)) && v.id() == id).map(|v| v.name().to_owned());
    let blocks: Vec<(Id, String)> = doc.views.iter().map(|v| (v.id(), view_block_name(v.id()))).collect();
    let refs = sheet_refs(d, &blocks);
    let sheet_of = |id: Id| refs.iter().find(|(v, _, _)| *v == id).map_or_else(String::new, |(_, n, l)| format!("{n}/{l}"));
    let grouped = |h: Handle| d.groups.iter().any(|g| is_tag_group(g) && g.members.contains(&h));
    let mut out = Plan::default();
    // Each view block is reconciled on its own; section views extend the filter.
    for v in doc.views.iter().filter(|v| matches!(v, AnyView::Plan(_))) {
        let block_name = view_block_name(v.id());
        let Some(block) = d.block(&block_name) else { continue };
        let bi = out.blocks.len();
        let mut bp = BlockPlan { block: block_name, ..BlockPlan::default() };
        let mut claimed: Vec<Handle> = Vec::new();
        let mut orphans = Vec::new();
        for e in block.entities.iter() {
            let EntityKind::Leader(l) = &e.kind else { continue };
            let Some(tag) = read_entity_data(&e.common).map_err(|e| e.to_string())?.detail_tag else { continue };
            let Some(&end) = l.vertices.last() else { continue };
            let wanted_at = |at: Vec3| Wanted {
                at,
                layer: e.common.layer.clone(),
                detail: name_of(tag.detail_view_id).unwrap_or_else(|| "?".into()),
                sheet: sheet_of(tag.detail_view_id),
            };
            let mine = d
                .groups
                .iter()
                .filter(|g| is_tag_group(g) && g.members.contains(&e.handle))
                .flat_map(|g| g.members.iter().copied())
                .find(|h| *h != e.handle && !claimed.contains(h) && block.entities.get(*h).is_some_and(|e| is_bubble(e)));
            let Some(hb) = mine else {
                orphans.push((e.handle, wanted_at(end)));
                continue;
            };
            claimed.push(hb);
            let at = insert_at(d, hb).unwrap_or(end);
            let dragged = before
                .is_some_and(|b| insert_at(b, hb).is_some_and(|was| !near(was, at)) && leader_end(b, e.handle).is_some_and(|was| near(was, end)));
            let w = if dragged && !near(at, end) {
                bp.leader_ends.push((e.handle, at));
                wanted_at(at)
            } else {
                wanted_at(end)
            };
            if let Some(be) = block.entities.get(hb)
                && let EntityKind::Insert(i) = &be.kind
                && !matches(be, i, &w)
            {
                bp.update.push((hb, w));
            }
            out.pairs.push((bi, e.handle, Bubble::Kept(hb)));
        }
        // A leader without a grouped bubble adopts an ungrouped one at its last vertex, or gets one.
        for (hl, w) in orphans {
            let adopt = block.entities.iter().find(|be| {
                is_bubble(be) && !claimed.contains(&be.handle) && !grouped(be.handle) && insert_at(d, be.handle).is_some_and(|p| near(p, w.at))
            });
            match adopt {
                Some(be) => {
                    claimed.push(be.handle);
                    if let EntityKind::Insert(i) = &be.kind
                        && !matches(be, i, &w)
                    {
                        bp.update.push((be.handle, w));
                    }
                    out.pairs.push((bi, hl, Bubble::Kept(be.handle)));
                }
                None => {
                    out.pairs.push((bi, hl, Bubble::Added(bp.add.len())));
                    bp.add.push(w);
                }
            }
        }
        bp.remove = block.entities.iter().filter(|e| is_bubble(e) && !claimed.contains(&e.handle)).map(|e| e.handle).collect();
        out.blocks.push(bp);
    }
    // The tag groups must be exactly one selectable {leader, bubble} group per pair.
    let tag_groups: Vec<&Group> = d.groups.iter().filter(|g| is_tag_group(g)).collect();
    out.groups_dirty = tag_groups.len() != out.pairs.len()
        || out.pairs.iter().any(|(_, hl, b)| match b {
            Bubble::Added(_) => true,
            Bubble::Kept(hb) => !tag_groups.iter().any(|g| g.selectable && g.members == [*hl, *hb]),
        });
    Ok(out)
}

fn apply(d: &mut Drawing, plan: &Plan) -> Result<(), String> {
    if plan.blocks.iter().any(|b| !b.add.is_empty()) {
        ensure_block(d)?;
    }
    let mut added: Vec<Vec<Handle>> = Vec::new();
    for bp in &plan.blocks {
        let space = Space::Block(bp.block.clone());
        let store = d.space_mut(&space).ok_or("view block gone")?;
        for h in &bp.remove {
            store.remove(*h);
        }
        for (h, w) in &bp.update {
            store.modify(*h, |e| {
                e.common.layer = w.layer.clone();
                e.kind = EntityKind::Insert(insert_of(w));
            });
        }
        for (h, to) in &bp.leader_ends {
            store.modify(*h, |e| {
                if let EntityKind::Leader(l) = &mut e.kind
                    && let Some(v) = l.vertices.last_mut()
                {
                    *v = *to;
                }
            });
        }
        let mut hs = Vec::new();
        for w in &bp.add {
            let common = Common { layer: w.layer.clone(), ..Common::default() };
            hs.push(d.add(&space, common, EntityKind::Insert(insert_of(w))).map_err(|e| e.to_string())?);
        }
        added.push(hs);
    }
    if plan.groups_dirty {
        // Each leader keeps its group's name; the groups of leaders that are gone go too.
        let old: Vec<Group> = d.groups.iter().filter(|g| is_tag_group(g)).cloned().collect();
        d.groups.retain(|g| !is_tag_group(g));
        for (bi, hl, b) in &plan.pairs {
            let hb = match b {
                Bubble::Kept(h) => *h,
                Bubble::Added(k) => *added.get(*bi).and_then(|hs| hs.get(*k)).ok_or("added bubble missing")?,
            };
            let name = old
                .iter()
                .find(|g| g.members.contains(hl))
                .map(|g| g.name.clone())
                .filter(|n| !d.groups.iter().any(|g| g.name.eq_ignore_ascii_case(n)))
                .unwrap_or_else(|| next_unnamed(&d.groups));
            d.groups.push(Group { name, description: GROUP_DESCRIPTION.into(), selectable: true, members: vec![*hl, hb] });
        }
    }
    Ok(())
}

/// The after-command observer: every plan view's `LAMB_DTAG` inserts and tag groups made to equal
/// its tag leaders' wanted bubbles — grouped ones updated in place, the rest erased, missing ones
/// added. The drawing is written back only when something changed.
pub fn bubble_observer(s: &mut Session, before: &Arc<Drawing>) {
    let todo = match s.doc().map_err(|e| e.to_string()).and_then(|d| plan(d, Some(before))) {
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
        // The drawing's own save version keeps attribute Lock position (R2000 DXF has no group for it).
        let bytes = cadcraft_io::write(s.doc()?, "t.dxf").map_err(|e| format!("{e:?}"))?;
        let back = cadcraft_io::read(&bytes, "t.dxf").map_err(|e| format!("{e:?}"))?;
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

    fn hx(h: &str) -> T<Handle> {
        Ok(Handle::parse_hex(h).ok_or("bad handle")?)
    }

    fn tag_groups(s: &Session) -> T<Vec<Group>> {
        Ok(s.doc()?.groups.iter().filter(|g| is_tag_group(g)).cloned().collect())
    }

    fn end_of(s: &Session, leader: Handle) -> T<(f64, f64)> {
        let v = leader_end(s.doc()?, leader).ok_or("no leader")?;
        Ok((v.x, v.y))
    }

    #[test]
    fn a_tag_and_its_bubble_are_one_selectable_group() -> R {
        let (mut s, plan, detail) = room()?;
        let leader = hx(&tag(&mut s, detail, (2500.0, 0.0))?)?;
        let b = bubbles(&s, plan)?;
        let g = tag_groups(&s)?;
        assert_eq!(g.len(), 1, "{g:?}");
        assert!(g[0].selectable && g[0].name.starts_with("*A") && g[0].description == GROUP_DESCRIPTION);
        assert_eq!(g[0].members, [leader, b[0].0]);
        // Picking the leader (halfway along it, clear of the walls) selects both.
        s.idle_click(cadcraft_geom::Vec2::new(2000.0, -500.0), false)?;
        assert_eq!(s.selection(), vec![leader, b[0].0]);
        // An unrelated command keeps the group as it is.
        s.execute("line", &json!({ "points": [[9000, 9000], [9500, 9000]] }))?;
        assert_eq!(tag_groups(&s)?, g);
        Ok(())
    }

    #[test]
    fn the_bubble_grip_drags_the_leader_end_and_the_leader_grip_drags_the_bubble() -> R {
        let (mut s, plan, detail) = room()?;
        let leader = hx(&tag(&mut s, detail, (2500.0, 0.0))?)?;
        let hb = bubbles(&s, plan)?[0].0;
        s.set_selection(Vec::new());
        let n = undo_len(&s)?;
        s.grip_edit(hb, 0, cadcraft_geom::Vec2::new(1900.0, -1250.0), cadcraft_engine::grips::GripMode::Stretch)?;
        assert_eq!(undo_len(&s)?, n + 1);
        assert_eq!(end_of(&s, leader)?, (1900.0, -1250.0), "the leader's end follows the bubble");
        let b = bubbles(&s, plan)?;
        assert_eq!((b.len(), b[0].0, b[0].1), (1, hb, (1900.0, -1250.0)), "same bubble, where it was dragged");
        let EntityKind::Insert(i) = &s.doc()?.entity(hb).ok_or("gone")?.kind else { return Err("not an insert".into()) };
        assert_eq!(i.attribs[0].text.align_pt, Some(Vec3::new(1900.0, -1190.0, 0.0)), "the attributes went with it");
        s.undo()?;
        assert_eq!((end_of(&s, leader)?, bubbles(&s, plan)?[0].1), ((1500.0, -1000.0), (1500.0, -1000.0)));

        // The leader's last-vertex grip carries the bubble.
        let g = s.grips_of(leader)?;
        let k = g.iter().position(|p| p.near(cadcraft_geom::Vec2::new(1500.0, -1000.0), 1e-6)).ok_or("no end grip")?;
        s.grip_edit(leader, k, cadcraft_geom::Vec2::new(1000.0, -800.0), cadcraft_engine::grips::GripMode::Stretch)?;
        let b = bubbles(&s, plan)?;
        assert_eq!((b.len(), b[0].0, b[0].1), (1, hb, (1000.0, -800.0)));
        Ok(())
    }

    #[test]
    fn move_carries_the_pair_and_erasing_the_leader_takes_bubble_and_group() -> R {
        let (mut s, plan, detail) = room()?;
        let leader = hx(&tag(&mut s, detail, (2500.0, 0.0))?)?;
        let hb = bubbles(&s, plan)?[0].0;
        s.execute("move", &json!({ "handles": [leader.hex(), hb.hex()], "delta": [300, 200] }))?;
        let b = bubbles(&s, plan)?;
        assert_eq!((b.len(), b[0].0, b[0].1), (1, hb, (1800.0, -800.0)), "moved, not re-created");
        assert_eq!(end_of(&s, leader)?, (1800.0, -800.0));
        assert_eq!(tag_groups(&s)?.len(), 1);
        s.execute("erase", &json!({ "handles": [leader.hex()] }))?;
        assert!(bubbles(&s, plan)?.is_empty());
        assert!(tag_groups(&s)?.is_empty());
        Ok(())
    }

    #[test]
    fn bubble_text_has_no_grips_and_old_ungrouped_bubbles_are_adopted() -> R {
        let (mut s, plan, detail) = room()?;
        let leader = hx(&tag(&mut s, detail, (2500.0, 0.0))?)?;
        let hb = bubbles(&s, plan)?[0].0;
        assert_eq!(s.grips_of(hb)?.len(), 1, "locked attributes have no grips");
        // A drawing from before the groups: the bubble at the leader's end is adopted, unlocked
        // attributes are locked, and the handle stays.
        let d = s.doc_mut()?;
        d.groups.clear();
        d.space_mut(&Space::Block(view_block_name(plan))).ok_or("no block")?.modify(hb, |e| {
            if let EntityKind::Insert(i) = &mut e.kind {
                i.attribs.iter_mut().for_each(|a| a.modes.lock_position = false);
            }
        });
        s.execute("line", &json!({ "points": [[9000, 9000], [9500, 9000]] }))?;
        assert_eq!(tag_groups(&s)?.iter().map(|g| g.members.clone()).collect::<Vec<_>>(), [vec![leader, hb]]);
        assert_eq!(s.grips_of(hb)?.len(), 1);
        Ok(())
    }

    fn plan_for(d: &Drawing) -> T<Vec<Value>> {
        let p = plan(d, None)?;
        Ok(if p.is_empty() { Vec::new() } else { vec![json!(format!("{p:?}"))] })
    }
}

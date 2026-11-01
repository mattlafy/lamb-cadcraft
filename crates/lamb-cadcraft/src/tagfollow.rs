//! Detail tags follow their walls (LAMB T495, `docs/07-architecture/cadcraft-integration.md` §9
//! "anchored like an associative dimension … so tags follow walls through edits, including
//! WALLFIT").
//!
//! [`tag_follow_observer`] runs after every command. For each plan and vertical view, each tagged
//! leader the command left **identical** (a tag the command moved itself is already where the user
//! put it) whose tip attached in the drawing **before** the command is anchored there:
//!
//! - an edge tag to the nearest face segment of its run — `(entity, segment, t)` plus the tip's
//!   offset along and across that segment at `t` (across = the left normal; on a bulged segment,
//!   radial);
//! - a corner tag to its junction — the junction point and the set of `(face entity, segment)`
//!   pairs of its arms' runs;
//! - an opening tag to its bridge — the bridge midpoint and its `opening_entity_ids`.
//!
//! After the command the anchor is found again: the same face entity and segment index; the
//! junction whose arms' face set overlaps the anchor's most (a tie or no overlap leaves the tag);
//! the bridge with the same opening entities. The tip (`vertices[0]`, nothing else) moves to the
//! re-found anchor when that is more than `TOLERANCE.coincidence_mm` away, in the command's own
//! undo step. An error is echoed, never raised.
//!
//! A callout-box tag (LAMB T594) is followed the same way through its `anchor`: the whole box and
//! its anchor shift by (new tip − old tip). A box the command itself moved or copied whole (its
//! polyline shifted, its tag otherwise the same) has its anchor shifted with it.
//!
//! Out of scope: TRIM/BREAK replacing a face entity with new ones — the tip stays, and a tag that
//! then falls off shows as unattached in RESOLVE / DETAILTAGS.

use std::collections::BTreeSet;
use std::sync::Arc;

use cadcraft_doc::{Drawing, Entity, EntityKind, Handle, Space};
use cadcraft_engine::Session;
use cadcraft_geom::{PolyVertex, Vec2};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::derivation::wall_junctions::{WallGraph, wall_graph};
use lamb_core::geometry::Point2;
use lamb_core::geometry::polyline::{ResolvedSegment, closest_point_on_segment, point_at_param, resolved_segments, tangent_at_param};
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AnyView, Id};

use crate::detailtags::{attach_bridge, attach_junction, attach_run, tags_in, wall_params};
use crate::project::DrawingProject;
use crate::schema::{DetailTag, read_document, read_entity_data, set_entity_data, view_block_name};

/// Where a tag's tip was, relative to what it tagged.
enum Anchor {
    /// `along` / `across`: the tip's offset from the point at `t`, along the segment's tangent and
    /// its left normal there.
    Edge {
        entity: Id,
        segment: usize,
        t: f64,
        along: f64,
        across: f64,
    },
    Corner {
        point: Point2,
        faces: BTreeSet<(Id, usize)>,
    },
    Opening {
        mid: Point2,
        ids: Vec<Id>,
    },
}

fn segment_of(p: &DrawingProject<'_>, entity: Id, segment: usize) -> Option<ResolvedSegment> {
    let e = p.entity(entity)?;
    let poly = e.polyline.as_ref()?;
    resolved_segments(poly).get(segment).copied()
}

fn mid(a: Point2, b: Point2) -> Point2 {
    Point2 { x: (a.x + b.x) / 2.0, y: (a.y + b.y) / 2.0 }
}

/// The `(face entity, segment)` pairs of every run meeting at junction `j`.
fn junction_faces(graph: &WallGraph, j: usize) -> BTreeSet<(Id, usize)> {
    graph.junctions[j]
        .arms
        .iter()
        .filter_map(|a| graph.runs.get(a.run))
        .flat_map(|r| r.faces.iter().map(|f| (f.from.entity_id, f.from.segment_index)))
        .collect()
}

/// An edge tag's anchor: the nearest of its run's two face segments.
fn edge_anchor(p: &DrawingProject<'_>, graph: &WallGraph, run: usize, tip: Point2) -> Option<Anchor> {
    let mut best: Option<(f64, Anchor)> = None;
    for face in &graph.runs[run].faces {
        let (entity, segment) = (face.from.entity_id, face.from.segment_index);
        let Some(seg) = segment_of(p, entity, segment) else { continue };
        let c = closest_point_on_segment(&seg, tip);
        if best.as_ref().is_some_and(|(d, _)| *d <= c.distance) {
            continue;
        }
        let tan = tangent_at_param(&seg, c.t);
        let (dx, dy) = (tip.x - c.point.x, tip.y - c.point.y);
        let along = dx * tan.x + dy * tan.y;
        let across = -dx * tan.y + dy * tan.x;
        best = Some((c.distance, Anchor::Edge { entity, segment, t: c.t, along, across }));
    }
    best.map(|(_, a)| a)
}

/// The tag's anchor in the `before` graph, read the way DETAILTAGS attaches it; `None` when the
/// tip attached to nothing.
fn anchor(p: &DrawingProject<'_>, graph: &WallGraph, tip: Point2, corner: bool) -> Option<Anchor> {
    if !corner {
        return edge_anchor(p, graph, attach_run(graph, tip)?, tip);
    }
    if let Some(j) = attach_junction(graph, tip) {
        return Some(Anchor::Corner { point: graph.junctions[j].point, faces: junction_faces(graph, j) });
    }
    let b = &graph.bridges[attach_bridge(graph, tip)?];
    Some(Anchor::Opening { mid: mid(b.from, b.to), ids: b.opening_entity_ids.clone() })
}

/// Where the tip goes after the command, or `None` to leave it.
fn followed(p: &DrawingProject<'_>, graph: &WallGraph, tip: Point2, a: &Anchor) -> Option<Point2> {
    let shift = |from: Point2, to: Point2| Point2 { x: tip.x + to.x - from.x, y: tip.y + to.y - from.y };
    match a {
        Anchor::Edge { entity, segment, t, along, across } => {
            let seg = segment_of(p, *entity, *segment)?;
            let at = point_at_param(&seg, *t);
            let tan = tangent_at_param(&seg, *t);
            Some(Point2 { x: at.x + along * tan.x - across * tan.y, y: at.y + along * tan.y + across * tan.x })
        }
        Anchor::Corner { point, faces } => {
            let mut scored: Vec<(usize, usize)> =
                (0..graph.junctions.len()).map(|j| (junction_faces(graph, j).intersection(faces).count(), j)).filter(|(n, _)| *n > 0).collect();
            scored.sort_by_key(|a| std::cmp::Reverse(a.0));
            match scored.as_slice() {
                [] => None,
                [(n, _), (m, _), ..] if n == m => None,
                [(_, j), ..] => Some(shift(*point, graph.junctions[*j].point)),
            }
        }
        Anchor::Opening { mid: m, ids } => {
            let b = graph.bridges.iter().find(|b| b.opening_entity_ids == *ids)?;
            Some(shift(*m, mid(b.from, b.to)))
        }
    }
}

/// Every tip to move: (view, leader, new tip).
fn follow(before: &Drawing, after: &Drawing) -> Result<Vec<(Id, Handle, Point2)>, String> {
    let Ok(doc) = read_document(before) else { return Ok(Vec::new()) };
    let views: Vec<Id> = doc.views.iter().filter(|v| matches!(v, AnyView::Plan(_) | AnyView::Vertical(_))).map(AnyView::id).collect();
    let mut out = Vec::new();
    for view in views {
        let name = view_block_name(view);
        let (Some(bb), Some(ab)) = (before.block(&name), after.block(&name)) else { continue };
        if Arc::ptr_eq(bb, ab) || **bb == **ab {
            continue;
        }
        let unchanged = |h: Handle| matches!((bb.entities.get(h), ab.entities.get(h)), (Some(x), Some(y)) if Arc::ptr_eq(x, y) || **x == **y);
        let tags: Vec<_> = tags_in(before, view)?.into_iter().filter(|(h, _, _)| unchanged(*h)).collect();
        if tags.is_empty() {
            continue;
        }
        let bp = DrawingProject::new(before).map_err(|e| e.to_string())?;
        let bgraph = wall_graph(&bp, view, &wall_params(&bp, view));
        let anchors: Vec<(Handle, Point2, Anchor)> =
            tags.into_iter().filter_map(|(h, tip, tag)| anchor(&bp, &bgraph, tip, tag.corner).map(|a| (h, tip, a))).collect();
        if anchors.is_empty() {
            continue;
        }
        let ap = DrawingProject::new(after).map_err(|e| e.to_string())?;
        let agraph = wall_graph(&ap, view, &wall_params(&ap, view));
        for (h, tip, a) in anchors {
            let Some(to) = followed(&ap, &agraph, tip, &a) else { continue };
            if (to.x - tip.x).hypot(to.y - tip.y) > TOLERANCE.coincidence_mm {
                out.push((view, h, to));
            }
        }
    }
    Ok(out)
}

/// A callout-box tag's polyline vertices and anchor, if `e` is one.
fn box_of(e: &Entity) -> Option<(&[PolyVertex], DetailTag, Point2)> {
    let EntityKind::LwPolyline(pl) = &e.kind else { return None };
    let tag = read_entity_data(&e.common).ok()?.detail_tag?;
    let [x, y] = tag.anchor?;
    Some((&pl.vertices, tag, Point2 { x, y }))
}

/// `after` as `before` moved by one shift, with its bulges unchanged: that shift.
fn translation(before: &[PolyVertex], after: &[PolyVertex]) -> Option<Vec2> {
    let d = after.first()?.p - before.first()?.p;
    let same =
        before.len() == after.len() && before.iter().zip(after).all(|(b, a)| (a.p - b.p - d).len() <= TOLERANCE.coincidence_mm && a.bulge == b.bulge);
    same.then_some(d)
}

/// Every callout box the command moved or copied whole, whose anchor it left behind (LAMB T594):
/// (view, box, new anchor). A copy is a new box with the same tag as one before, a shifted copy
/// of its polyline.
fn box_follow(before: &Drawing, after: &Drawing) -> Vec<(Id, Handle, Point2)> {
    let Ok(doc) = read_document(after) else { return Vec::new() };
    let mut out = Vec::new();
    for view in doc.views.iter().filter(|v| matches!(v, AnyView::Plan(_) | AnyView::Vertical(_))).map(AnyView::id) {
        let name = view_block_name(view);
        let (Some(bb), Some(ab)) = (before.block(&name), after.block(&name)) else { continue };
        if Arc::ptr_eq(bb, ab) || **bb == **ab {
            continue;
        }
        for e in ab.entities.iter() {
            let Some((av, atag, anchor)) = box_of(e) else { continue };
            let shift = match bb.entities.get(e.handle) {
                Some(was) if Arc::ptr_eq(was, e) || **was == **e => continue,
                Some(was) => box_of(was).filter(|(_, t, _)| *t == atag).and_then(|(bv, _, _)| translation(bv, av)),
                None => bb.entities.iter().filter_map(|b| box_of(b)).filter(|(_, t, _)| *t == atag).find_map(|(bv, _, _)| translation(bv, av)),
            };
            if let Some(d) = shift.filter(|d| d.len() > TOLERANCE.coincidence_mm) {
                out.push((view, e.handle, Point2 { x: anchor.x + d.x, y: anchor.y + d.y }));
            }
        }
    }
    out
}

/// Moves each tag's tip: a leader's `vertices[0]`, a callout box's whole polyline with its anchor.
fn apply(d: &mut Drawing, moves: &[(Id, Handle, Point2)]) -> Result<(), String> {
    for (view, h, to) in moves {
        let store = d.space_mut(&Space::Block(view_block_name(*view))).ok_or("view block gone")?;
        let Some(e) = store.get(*h) else { continue };
        let mut e = (**e).clone();
        match &mut e.kind {
            EntityKind::Leader(l) => {
                let Some(v) = l.vertices.first_mut() else { continue };
                (v.x, v.y) = (to.x, to.y);
            }
            EntityKind::LwPolyline(_) => {
                let Some((_, _, from)) = box_of(&e) else { continue };
                let d = Vec2::new(to.x - from.x, to.y - from.y);
                if let EntityKind::LwPolyline(pl) = &mut e.kind {
                    for v in &mut pl.vertices {
                        v.p += d;
                    }
                }
                set_anchor(&mut e, *to)?;
            }
            _ => continue,
        }
        store.replace(e);
    }
    Ok(())
}

/// Sets a callout box's anchor alone (after [`box_follow`]).
fn apply_anchors(d: &mut Drawing, moves: &[(Id, Handle, Point2)]) -> Result<(), String> {
    for (view, h, to) in moves {
        let store = d.space_mut(&Space::Block(view_block_name(*view))).ok_or("view block gone")?;
        let Some(e) = store.get(*h) else { continue };
        let mut e = (**e).clone();
        set_anchor(&mut e, *to)?;
        store.replace(e);
    }
    Ok(())
}

fn set_anchor(e: &mut Entity, to: Point2) -> Result<(), String> {
    let mut data = read_entity_data(&e.common).map_err(|e| e.to_string())?;
    let Some(tag) = data.detail_tag.as_mut() else { return Ok(()) };
    tag.anchor = Some([to.x, to.y]);
    set_entity_data(&mut e.common, &data).map_err(|e| e.to_string())
}

/// The after-command observer: moves the tip of every tag whose wall, junction or opening the
/// command moved (module doc). Writes back only when a tip moved.
pub fn tag_follow_observer(s: &mut Session, before: &Arc<Drawing>) {
    let Ok(current) = s.doc() else { return };
    let boxes = box_follow(before, current);
    let moves = match follow(before, current) {
        Ok(m) if m.is_empty() && boxes.is_empty() => return,
        Ok(m) => m,
        Err(e) => {
            s.echo(format!("LAMB tag follow: {e}"));
            return;
        }
    };
    let mut after = current.clone();
    if let Err(e) = apply_anchors(&mut after, &boxes).and_then(|()| apply(&mut after, &moves)) {
        s.echo(format!("LAMB tag follow: {e}"));
        return;
    }
    if let Ok(d) = s.doc_mut() {
        *d = after;
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_engine::Session;
    use lamb_core::model::Id;
    use serde_json::{Value, json};

    use crate::detailtags::view_detail_tags;
    use crate::schema::view_block_name;
    use cadcraft_doc::EntityKind;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    /// detailtags.rs's room (5000 × 4000, 200 mm walls on `WALL-EXT`, detail `D1`), observers
    /// installed, layer `TAGS` current. Returns (session, plan, D1).
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

    fn tag(s: &mut Session, detail: Id, corner: bool, tip: (f64, f64)) -> T<String> {
        let r = s.execute(
            "detailtag",
            &json!({ "detailViewId": detail.0, "corner": corner, "tip": {"x": tip.0, "y": tip.1}, "bubble": {"x": tip.0 - 1000.0, "y": tip.1 - 1000.0} }),
        )?;
        Ok(r["handle"].as_str().ok_or("no handle")?.to_owned())
    }

    /// A leader's tip, rounded to 1e-6.
    fn tip(s: &Session, view: Id, h: &str) -> T<(f64, f64)> {
        let b = s.doc()?.block(&view_block_name(view)).ok_or("no block")?;
        let e = b.entities.iter().find(|e| format!("{:X}", e.handle.0) == h).ok_or("no leader")?;
        let EntityKind::Leader(l) = &e.kind else { return Err("not a leader".into()) };
        let v = l.vertices.first().ok_or("no vertices")?;
        Ok(((v.x * 1e6).round() / 1e6, (v.y * 1e6).round() / 1e6))
    }

    /// Handles of the view block's wall and opening geometry (polylines and lines).
    fn geometry(s: &Session, view: Id) -> T<Vec<Value>> {
        let b = s.doc()?.block(&view_block_name(view)).ok_or("no block")?;
        Ok(b.entities
            .iter()
            .filter(|e| matches!(e.kind, EntityKind::LwPolyline(_) | EntityKind::Line(_)))
            .map(|e| json!(format!("{:X}", e.handle.0)))
            .collect())
    }

    #[test]
    fn edge_and_corner_tags_follow_a_move_of_their_walls_in_one_undo_step() -> R {
        let (mut s, plan, detail) = room()?;
        let edge = tag(&mut s, detail, false, (2500.0, 100.0))?;
        let corner = tag(&mut s, detail, true, (100.0, 100.0))?;
        let walls = geometry(&s, plan)?;
        assert_eq!(walls.len(), 2);
        s.execute("move", &json!({ "handles": walls, "delta": [1000, 500] }))?;
        assert_eq!(tip(&s, plan, &edge)?, (3500.0, 600.0));
        assert_eq!(tip(&s, plan, &corner)?, (1100.0, 600.0));
        let t = view_detail_tags(s.doc()?, plan)?;
        assert_eq!((t.edges.len(), t.corners.len(), t.unattached.len()), (1, 1, 0), "{t:?}");

        s.undo()?;
        assert_eq!(tip(&s, plan, &edge)?, (2500.0, 100.0));
        assert_eq!(tip(&s, plan, &corner)?, (100.0, 100.0));
        Ok(())
    }

    #[test]
    fn moving_the_tag_itself_is_not_undone_by_the_follow() -> R {
        let (mut s, plan, detail) = room()?;
        let edge = tag(&mut s, detail, false, (2500.0, 100.0))?;
        s.execute("move", &json!({ "handles": [edge.clone()], "delta": [300, 2000] }))?;
        assert_eq!(tip(&s, plan, &edge)?, (2800.0, 2100.0));
        Ok(())
    }

    #[test]
    fn an_edge_tag_follows_a_stretch_of_its_wall() -> R {
        let (mut s, plan, detail) = room()?;
        let edge = tag(&mut s, detail, false, (2500.0, 100.0))?;
        // Both faces of the bottom wall, with a notch keeping the tag's tip out of the polygon.
        let polygon = json!([[-100, -100], [5100, -100], [5100, 300], [2600, 300], [2600, 50], [2400, 50], [2400, 300], [-100, 300]]);
        s.execute("stretch", &json!({ "polygon": polygon, "delta": [0, -300] }))?;
        assert_eq!(tip(&s, plan, &edge)?, (2500.0, -200.0));
        assert_eq!(view_detail_tags(s.doc()?, plan)?.edges.len(), 1);
        Ok(())
    }

    #[test]
    fn wallfit_keeps_an_inner_face_tag_on_its_run_and_its_assembly_laid() -> R {
        let (mut s, plan) = crate::assembly::tests::room_with("exterior_face", false, 210.0)?;
        crate::maintain::install(&mut s);
        let doc = crate::schema::read_document(s.doc()?)?;
        let detail = doc.views.iter().find(|v| v.name() == "D1").map(lamb_core::model::AnyView::id).ok_or("no D1")?;
        s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
        let edge = tag(&mut s, detail, false, (2500.0, 250.0))?;
        assert!(!s.execute("wallconflicts", &json!({ "viewId": plan.0 }))?["conflicts"].as_array().ok_or("no list")?.is_empty());
        s.execute("wallfit", &json!({ "viewId": plan.0 }))?;
        assert_eq!(tip(&s, plan, &edge)?, (2500.0, 240.0), "the inner face moved 10 toward the outer");
        let t = view_detail_tags(s.doc()?, plan)?;
        assert_eq!((t.edges.len(), t.unattached.len()), (1, 0));
        let runs = crate::assembly::assembly_lines(s.doc()?, plan)?;
        assert!(runs.iter().all(|r| r.skipped.is_none() && !r.lines.is_empty()), "{runs:?}");
        Ok(())
    }

    #[test]
    fn an_opening_tag_follows_its_wall_pieces_and_opening() -> R {
        let (mut s, plan) = crate::assembly::tests::window_room(false, true)?;
        crate::maintain::install(&mut s);
        let b = s.doc()?.block(&view_block_name(plan)).ok_or("no block")?;
        let opening = b
            .entities
            .iter()
            .filter(|e| matches!(&e.kind, EntityKind::Leader(l) if l.vertices.first().is_some_and(|v| v.x == 2500.0 && v.y == 100.0)))
            .map(|e| format!("{:X}", e.handle.0))
            .next()
            .ok_or("no opening tag")?;
        let geo = geometry(&s, plan)?;
        s.execute("move", &json!({ "handles": geo, "delta": [700, -400] }))?;
        assert_eq!(tip(&s, plan, &opening)?, (3200.0, -300.0));
        let t = view_detail_tags(s.doc()?, plan)?;
        assert_eq!((t.openings.len(), t.unattached.len()), (1, 0), "{t:?}");
        Ok(())
    }

    /// A callout-box edge tag at `tip`, 500 × 400 around it. Returns its handle.
    fn box_tag(s: &mut Session, detail: Id, tip: (f64, f64)) -> T<String> {
        let r = s.execute(
            "detailtag",
            &json!({ "detailViewId": detail.0, "tip": {"x": tip.0, "y": tip.1},
                     "box": {"a": {"x": tip.0 - 250.0, "y": tip.1 - 200.0}, "b": {"x": tip.0 + 250.0, "y": tip.1 + 200.0}} }),
        )?;
        Ok(r["handle"].as_str().ok_or("no handle")?.to_owned())
    }

    /// A box tag's anchor and first vertex, rounded to 1e-6.
    fn box_at(s: &Session, view: Id, h: &str) -> T<((f64, f64), (f64, f64))> {
        let b = s.doc()?.block(&view_block_name(view)).ok_or("no block")?;
        let e = b.entities.iter().find(|e| format!("{:X}", e.handle.0) == h).ok_or("no box")?;
        let EntityKind::LwPolyline(pl) = &e.kind else { return Err("not a polyline".into()) };
        let [x, y] = crate::schema::read_entity_data(&e.common)?.detail_tag.and_then(|t| t.anchor).ok_or("no anchor")?;
        let v = pl.vertices.first().ok_or("no vertices")?.p;
        let r = |a: f64| (a * 1e6).round() / 1e6;
        Ok(((r(x), r(y)), (r(v.x), r(v.y))))
    }

    #[test]
    fn a_box_tag_follows_a_move_of_its_wall_box_and_anchor_together() -> R {
        let (mut s, plan, detail) = room()?;
        let walls = geometry(&s, plan)?;
        let b = box_tag(&mut s, detail, (2500.0, 100.0))?;
        s.execute("move", &json!({ "handles": walls, "delta": [1000, 500] }))?;
        assert_eq!(box_at(&s, plan, &b)?, ((3500.0, 600.0), (3250.0, 400.0)));
        let t = view_detail_tags(s.doc()?, plan)?;
        assert_eq!((t.edges.len(), t.unattached.len()), (1, 0), "{t:?}");
        s.undo()?;
        assert_eq!(box_at(&s, plan, &b)?, ((2500.0, 100.0), (2250.0, -100.0)));
        Ok(())
    }

    #[test]
    fn moving_or_copying_a_box_tag_takes_its_anchor_along() -> R {
        let (mut s, plan, detail) = room()?;
        let b = box_tag(&mut s, detail, (2500.0, 100.0))?;
        s.execute("move", &json!({ "handles": [b.clone()], "delta": [300, 0] }))?;
        assert_eq!(box_at(&s, plan, &b)?, ((2800.0, 100.0), (2550.0, -100.0)));
        // Onto the top wall's centreline.
        s.execute("copy", &json!({ "handles": [b.clone()], "delta": [0, 3800] }))?;
        let t = view_detail_tags(s.doc()?, plan)?;
        assert_eq!((t.edges.len(), t.unattached.len()), (2, 0), "{t:?}");
        assert_ne!(t.edges[0].run, t.edges[1].run, "the copy tags the top wall");
        assert_eq!(box_at(&s, plan, &b)?, ((2800.0, 100.0), (2550.0, -100.0)), "the original stays");
        Ok(())
    }
}

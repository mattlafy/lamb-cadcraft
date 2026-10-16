//! LAMB's FILLET and CHAMFER on two adjacent segments of one polyline: the vertex stays where it
//! is and a virtual corner tag goes into the entity's `Common.ext["LAMB"].corners` (T9's virtual
//! corners), registered as CADCraft's virtual corner hook (`cadcraft_engine::vcorner`, fork patch
//! 10). A polyline with width keeps CADCraft's baked arc. Radius 0 (or both chamfer distances 0)
//! removes a tag at that vertex.
//!
//! Also registered: the two-entity FILLET pick check (same patch). A line/arc pair with at least one
//! arc whose two clicks are nearer different apparent intersections is refused, as TS `corner.ts`
//! refuses it; every other pair is left to CADCraft.

use cadcraft_doc::Handle;
use cadcraft_doc::{Entity, EntityKind};
use cadcraft_engine::vcorner::{CornerOp, register_corner_pick_check, register_virtual_corner_hook};
use cadcraft_engine::{EngineError, Result, Session};
use cadcraft_geom::Vec2;
use lamb_core::geometry::VirtualCornerEntry;
use lamb_core::geometry::fillet::{apparent_curve_intersections, bulge_for_arc, choose_shared_corner, curve_from_edge, resolve_virtual_corner};
use lamb_core::geometry::point::Point2;
use lamb_core::geometry::polyline::{CornerEntry, virtual_corner_entries};

use crate::convert::{apply_corners, to_polyline};
use crate::schema::{read_entity_data, set_entity_data};

fn other(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

/// The hook: tag (or untag) `vertex` of polyline `h`. `Ok(false)` hands the corner back to
/// CADCraft (a polyline with width, or an entity that isn't a polyline).
fn virtual_corner(s: &mut Session, h: Handle, vertex: usize, op: CornerOp) -> Result<bool> {
    let e = s.doc()?.entity(h).cloned().ok_or_else(|| other("No such entity."))?;
    let cadcraft_doc::EntityKind::LwPolyline(pl) = &e.kind else { return Ok(false) };
    if pl.const_width != 0.0 || pl.vertices.iter().any(|v| v.start_width != 0.0 || v.end_width != 0.0) {
        return Ok(false);
    }
    let Ok(conv) = to_polyline(&e) else { return Ok(false) };
    let mut poly = conv.polyline;
    let mut data = read_entity_data(&e.common).map_err(other)?;
    apply_corners(&mut poly, &data.corners);
    let entry = match op {
        CornerOp::Fillet { radius } if radius <= 0.0 => None,
        CornerOp::Fillet { radius } => Some(VirtualCornerEntry::Fillet { radius }),
        CornerOp::Chamfer { distance1, distance2 } if distance1 <= 0.0 && distance2 <= 0.0 => None,
        CornerOp::Chamfer { distance1, distance2 } if distance1 <= 0.0 || distance2 <= 0.0 => {
            return Err(other("Chamfer distances must both be zero or both be positive."));
        }
        CornerOp::Chamfer { distance1, distance2 } => Some(VirtualCornerEntry::Chamfer { distance1, distance2 }),
    };
    let Some(seg) = lamb_core::geometry::polyline::virtual_corner_segment_index(&poly, vertex) else {
        return Err(other("Corner geometry does not admit the requested operation"));
    };
    if let Some(entry) = &entry
        && resolve_virtual_corner(&poly, vertex, entry).is_none()
    {
        return Err(other("Corner geometry does not admit the requested operation"));
    }
    if let Some(sg) = poly.segments.get_mut(seg) {
        sg.corner_at_to = entry;
    }
    let corners: Vec<CornerEntry> = virtual_corner_entries(&poly);
    data.corners = corners;
    let mut common = e.common.clone();
    set_entity_data(&mut common, &data).map_err(other)?;
    s.doc_mut()?.modify_entity(h, |e| e.common.ext = common.ext)?;
    Ok(true)
}

/// A line or arc as lamb-core's `(p0, p1, bulge)`; `None` for anything else.
fn edge(e: &Entity) -> Option<(Point2, Point2, f64)> {
    match &e.kind {
        EntityKind::Line(l) => Some((Point2 { x: l.a.x, y: l.a.y }, Point2 { x: l.b.x, y: l.b.y }, 0.0)),
        EntityKind::Arc(a) => {
            let c = Point2 { x: a.center.x, y: a.center.y };
            let at = |t: f64| Point2 { x: c.x + a.radius * t.cos(), y: c.y + a.radius * t.sin() };
            let (p0, p1) = (at(a.start), at(a.end));
            Some((p0, p1, bulge_for_arc(c, true, p0, p1)))
        }
        _ => None,
    }
}

/// The pick check: refuse a line/arc pair (at least one arc) whose clicks name different corners.
fn corner_pick(_s: &Session, e1: &Entity, p1: Vec2, e2: &Entity, p2: Vec2) -> Result<()> {
    if !matches!(e1.kind, EntityKind::Arc(_)) && !matches!(e2.kind, EntityKind::Arc(_)) {
        return Ok(());
    }
    let (Some((a0, a1, ab)), Some((b0, b1, bb))) = (edge(e1), edge(e2)) else { return Ok(()) };
    let pts = apparent_curve_intersections(&curve_from_edge(a0, a1, ab), &curve_from_edge(b0, b1, bb));
    let (c1, c2) = (Point2 { x: p1.x, y: p1.y }, Point2 { x: p2.x, y: p2.y });
    if !pts.is_empty() && choose_shared_corner(&pts, a0, a1, ab, c1, b0, b1, bb, c2).is_none() {
        return Err(other("Those two clicks don't agree on the same corner."));
    }
    Ok(())
}

/// Register both hooks with CADCraft. Once per process; a second call is an error.
pub fn register() -> std::result::Result<(), &'static str> {
    register_virtual_corner_hook(virtual_corner)?;
    register_corner_pick_check(corner_pick)
}

#[cfg(test)]
mod tests {
    //! Driven through the typed commands. The hook is process-global: every test calls
    //! [`crate::commands::install_app`] first, which registers once.
    use cadcraft_doc::EntityKind;
    use serde_json::{Value, json};

    use super::*;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type Res<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    fn session() -> Res<Session> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        Ok(s)
    }

    fn handle(r: &Value) -> Res<String> {
        Ok(r["handle"].as_str().or_else(|| r["handles"][0].as_str()).ok_or("no handle")?.to_owned())
    }

    fn hnd(h: &str) -> Res<Handle> {
        Ok(Handle::parse_hex(h).ok_or("bad handle")?)
    }

    fn corners(s: &Session, h: &str) -> Res<Vec<CornerEntry>> {
        let e = s.doc()?.entity(hnd(h)?).cloned().ok_or("no entity")?;
        Ok(read_entity_data(&e.common)?.corners)
    }

    fn vertex_count(s: &Session, h: &str) -> Res<usize> {
        let e = s.doc()?.entity(hnd(h)?).cloned().ok_or("no entity")?;
        match &e.kind {
            EntityKind::LwPolyline(p) => Ok(p.vertices.len()),
            _ => Err("not a polyline".into()),
        }
    }

    fn l_shape(s: &mut Session) -> Res<String> {
        handle(&s.execute("pline", &json!({"vertices": [[0, 0], [100, 0], [100, 80]]}))?)
    }

    #[test]
    fn fillet_tags_the_vertex_and_undo_removes_it() -> R {
        let mut s = session()?;
        let p = l_shape(&mut s)?;
        s.execute("fillet", &json!({"h1": p, "p1": [50, 0], "h2": p, "p2": [100, 40], "radius": 10}))?;
        assert_eq!(vertex_count(&s, &p)?, 3);
        assert_eq!(corners(&s, &p)?, vec![CornerEntry { corner_index: 1, entry: VirtualCornerEntry::Fillet { radius: 10.0 } }]);
        s.undo()?;
        assert!(corners(&s, &p)?.is_empty());
        Ok(())
    }

    #[test]
    fn closed_wrap_corner_is_vertex_zero() -> R {
        let mut s = session()?;
        let p = handle(&s.execute("pline", &json!({"vertices": [[0, 0], [100, 0], [100, 80], [0, 80]], "closed": true}))?)?;
        s.execute("fillet", &json!({"h1": p, "p1": [0, 40], "h2": p, "p2": [50, 0], "radius": 5}))?;
        assert_eq!(vertex_count(&s, &p)?, 4);
        assert_eq!(corners(&s, &p)?, vec![CornerEntry { corner_index: 0, entry: VirtualCornerEntry::Fillet { radius: 5.0 } }]);
        Ok(())
    }

    #[test]
    fn chamfer_distance1_is_on_the_segment_ending_at_the_vertex() -> R {
        let mut s = session()?;
        let p = l_shape(&mut s)?;
        // Second leg picked first: d1 = 10 goes on the second leg, so the tag's distance1 (on the
        // first segment) is 20.
        s.execute("chamfer", &json!({"h1": p, "p1": [100, 40], "h2": p, "p2": [50, 0], "d1": 10, "d2": 20}))?;
        assert_eq!(vertex_count(&s, &p)?, 3);
        assert_eq!(corners(&s, &p)?, vec![CornerEntry { corner_index: 1, entry: VirtualCornerEntry::Chamfer { distance1: 20.0, distance2: 10.0 } }]);
        Ok(())
    }

    #[test]
    fn radius_zero_removes_and_too_large_errors() -> R {
        let mut s = session()?;
        let p = l_shape(&mut s)?;
        s.execute("fillet", &json!({"h1": p, "p1": [50, 0], "h2": p, "p2": [100, 40], "radius": 10}))?;
        assert!(s.execute("fillet", &json!({"h1": p, "p1": [50, 0], "h2": p, "p2": [100, 40], "radius": 200})).is_err());
        assert_eq!(corners(&s, &p)?.len(), 1);
        s.execute("fillet", &json!({"h1": p, "p1": [50, 0], "h2": p, "p2": [100, 40], "radius": 0}))?;
        assert!(corners(&s, &p)?.is_empty());
        assert_eq!(vertex_count(&s, &p)?, 3);
        // Radius 0 with no tag there is still a success.
        s.execute("fillet", &json!({"h1": p, "p1": [50, 0], "h2": p, "p2": [100, 40], "radius": 0}))?;
        Ok(())
    }

    /// The matrix's arc x arc pair (crossings at (3, -4) and (3, 4)): `(arc a, arc b)` handles.
    fn two_arcs(s: &mut Session) -> Res<(String, String)> {
        let a = handle(&s.execute("arc", &json!({"center": [0, 0], "radius": 5, "start": -110, "end": 190}))?)?;
        let b = handle(&s.execute("arc", &json!({"center": [6, 0], "radius": 5, "start": 60, "end": 300}))?)?;
        Ok((a, b))
    }

    fn on(c: (f64, f64), r: f64, deg: f64) -> [f64; 2] {
        let t = deg.to_radians();
        [c.0 + r * t.cos(), c.1 + r * t.sin()]
    }

    fn entity_count(s: &Session) -> Res<usize> {
        Ok(s.doc()?.entity_count())
    }

    #[test]
    fn curved_picks_on_different_corners_are_refused() -> R {
        let mut s = session()?;
        let (a, b) = two_arcs(&mut s)?;
        let kinds = |s: &Session| -> Res<(usize, Vec<EntityKind>)> {
            let d = s.doc()?;
            let k = [&a, &b].iter().map(|h| Ok(d.entity(hnd(h)?).ok_or("no entity")?.kind.clone())).collect::<Res<Vec<_>>>()?;
            Ok((d.entity_count(), k))
        };
        let before = kinds(&s)?;
        // Arc a clicked near (3, -4) (at -60 deg), arc b clicked near (3, 4) (at 120 deg).
        let err = s
            .execute("fillet", &json!({"h1": a, "p1": on((0.0, 0.0), 5.0, -60.0), "h2": b, "p2": on((6.0, 0.0), 5.0, 120.0), "radius": 2}))
            .err()
            .ok_or("fillet succeeded")?;
        assert!(err.to_string().contains("Those two clicks don't agree on the same corner."), "{err}");
        assert_eq!(before, kinds(&s)?);
        // Radius 0 is checked too.
        assert!(
            s.execute("fillet", &json!({"h1": a, "p1": on((0.0, 0.0), 5.0, -60.0), "h2": b, "p2": on((6.0, 0.0), 5.0, 120.0), "radius": 0})).is_err()
        );
        Ok(())
    }

    #[test]
    fn curved_picks_on_one_corner_fillet_there() -> R {
        let mut s = session()?;
        let (a, b) = two_arcs(&mut s)?;
        // Both clicks near (3, 4): arc a at 60 deg, arc b at 120 deg.
        let r = s.execute("fillet", &json!({"h1": a, "p1": on((0.0, 0.0), 5.0, 60.0), "h2": b, "p2": on((6.0, 0.0), 5.0, 120.0), "radius": 2}))?;
        let arc = hnd(r["arc"].as_str().ok_or("no arc")?)?;
        let e = s.doc()?.entity(arc).cloned().ok_or("no fillet arc")?;
        let EntityKind::Arc(f) = &e.kind else { return Err("fillet is not an arc".into()) };
        assert!(f.center.y > 0.0, "fillet at the upper corner: {f:?}");
        Ok(())
    }

    #[test]
    fn line_line_still_fillets_and_line_circle_is_not_checked() -> R {
        let mut s = session()?;
        let l1 = handle(&s.execute("line", &json!({"points": [[0, 0], [100, 0]]}))?)?;
        let l2 = handle(&s.execute("line", &json!({"points": [[50, -50], [50, 50]]}))?)?;
        let r = s.execute("fillet", &json!({"h1": l1, "p1": [20, 0], "h2": l2, "p2": [50, 20], "radius": 8}))?;
        assert!(r["arc"].as_str().is_some(), "{r}");
        let n = entity_count(&s)?;
        // A line through a circle, clicked near different crossings: the check leaves it to CADCraft.
        let l = handle(&s.execute("line", &json!({"points": [[200, 3], [220, 3]]}))?)?;
        let c = handle(&s.execute("circle", &json!({"center": [210, 0], "radius": 5}))?)?;
        assert_eq!(entity_count(&s)?, n + 2);
        let ccw: Entity = (*s.doc()?.entity(hnd(&c)?).cloned().ok_or("no circle")?).clone();
        let line: Entity = (*s.doc()?.entity(hnd(&l)?).cloned().ok_or("no line")?).clone();
        assert!(corner_pick(&s, &line, Vec2::new(201.0, 3.0), &ccw, Vec2::new(214.0, 3.0)).is_ok());
        assert!(corner_pick(&s, &ccw, Vec2::new(214.0, 3.0), &line, Vec2::new(201.0, 3.0)).is_ok());
        Ok(())
    }

    #[test]
    fn a_polyline_with_width_keeps_the_baked_arc() -> R {
        let mut s = session()?;
        let p = handle(&s.execute("pline", &json!({"vertices": [[0, 0], [100, 0], [100, 80]], "width": 5}))?)?;
        s.execute("fillet", &json!({"h1": p, "p1": [50, 0], "h2": p, "p2": [100, 40], "radius": 10}))?;
        assert_eq!(vertex_count(&s, &p)?, 4);
        assert!(corners(&s, &p)?.is_empty());
        Ok(())
    }
}

//! LAMB's FILLET and CHAMFER on two adjacent segments of one polyline: the vertex stays where it
//! is and a virtual corner tag goes into the entity's `Common.ext["LAMB"].corners` (T9's virtual
//! corners), registered as CADCraft's virtual corner hook (`cadcraft_engine::vcorner`, fork patch
//! 10). A polyline with width keeps CADCraft's baked arc. Radius 0 (or both chamfer distances 0)
//! removes a tag at that vertex.

use cadcraft_doc::Handle;
use cadcraft_engine::vcorner::{CornerOp, register_virtual_corner_hook};
use cadcraft_engine::{EngineError, Result, Session};
use lamb_core::geometry::VirtualCornerEntry;
use lamb_core::geometry::fillet::resolve_virtual_corner;
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

/// Register the hook with CADCraft. Once per process; a second call is an error.
pub fn register() -> std::result::Result<(), &'static str> {
    register_virtual_corner_hook(virtual_corner)
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
        Ok(r["handle"].as_str().ok_or("no handle")?.to_owned())
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

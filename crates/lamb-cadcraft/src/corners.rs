//! The after-command observer that keeps virtual corner tags (`Common.ext["LAMB"].corners`, T329–
//! T331) on the vertex they describe when a CADCraft command rewrites a polyline's vertices.
//!
//! [`maintain_corners`] walks every tagged `LwPolyline` whose kind the command changed (or that is
//! new) and re-seats its tags by the TS identity rules (`vertex.ts`, `polyline.ts`
//! `reversePolyline`):
//! 1. same handle, same vertex count: a pure reversal maps vertex k to n-1-k; anything else (move,
//!    rotate, stretch, decurve) keeps every index;
//! 2. different vertex count (insert, straighten, grip add/remove, fit): a tag moves to the after
//!    vertex at its before vertex's position (coincidence tolerance, nearest index to the old one
//!    if several), else it is dropped;
//! 3. a new handle (COPY, MIRROR, ARRAY, OFFSET, BREAK/TRIM pieces carry `ext`): the source is the
//!    first `before` entity (handle order) with an equal `corners` list; same count keeps indices,
//!    otherwise rule 2 against the source's vertices; no source keeps them;
//! 4. then an index that is no corner of the new shape (an open polyline's ends) is dropped. A tag
//!    the geometry no longer admits is kept — it draws sharp (`rounding`) and comes back if the
//!    geometry does.
//!
//! [`corner_observer`] runs it in the command's own undo step; `maintain::install` registers it
//! before the link observer, so link travel reads re-seated tags.

use std::sync::Arc;

use cadcraft_doc::{Drawing, Entity, EntityKind, Handle};
use cadcraft_engine::Session;
use lamb_core::geometry::polyline::{CornerEntry, virtual_corner_segment_index};
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::geometry::{Point2, Polyline2};

use crate::convert::to_polyline;
use crate::schema::{SchemaError, read_entity_data, set_entity_data};

fn near(a: Point2, b: Point2) -> bool {
    (a.x - b.x).hypot(a.y - b.y) <= TOLERANCE.coincidence_mm
}

/// The polyline's vertices in `corner_index` order (a closed one's closing segment adds none).
fn vertices(p: &Polyline2) -> Vec<Point2> {
    let n = if p.closed { p.segments.len() } else { p.segments.len() + 1 };
    std::iter::once(p.start).chain(p.segments.iter().map(|s| s.to)).take(n).collect()
}

fn has_width(e: &Entity) -> bool {
    matches!(&e.kind, EntityKind::LwPolyline(pl) if pl.const_width != 0.0 || pl.vertices.iter().any(|v| v.start_width != 0.0 || v.end_width != 0.0))
}

/// A tagged, width-free `LwPolyline` as (its polyline, its corners); anything else `None`.
fn tagged(e: &Entity) -> Result<Option<(Polyline2, Vec<CornerEntry>)>, SchemaError> {
    if !matches!(e.kind, EntityKind::LwPolyline(_)) || has_width(e) {
        return Ok(None);
    }
    let corners = read_entity_data(&e.common)?.corners;
    if corners.is_empty() {
        return Ok(None);
    }
    Ok(to_polyline(e).ok().map(|c| (c.polyline, corners)))
}

/// Rule 2: the after vertex at `old`'s position, nearest index to `index` if several.
fn by_position(after: &[Point2], old: Point2, index: usize) -> Option<usize> {
    after.iter().enumerate().filter(|(_, v)| near(**v, old)).map(|(i, _)| i).min_by_key(|i| i.abs_diff(index))
}

/// Re-seats `corners` from `before`'s vertices onto `after`'s (rules 1, 2), then rule 4.
fn reseat(before: &[Point2], after_poly: &Polyline2, corners: &[CornerEntry], same_handle: bool) -> Vec<CornerEntry> {
    let after = vertices(after_poly);
    let n = after.len();
    let reseated: Vec<CornerEntry> = if before.len() == n {
        let identical = before.iter().zip(&after).all(|(b, a)| near(*b, *a));
        let reversed = same_handle && !identical && n > 0 && (0..n).all(|k| near(after[k], before[n - 1 - k]));
        corners.iter().map(|c| if reversed { CornerEntry { corner_index: n - 1 - c.corner_index, ..*c } } else { *c }).collect()
    } else {
        corners
            .iter()
            .filter_map(|c| {
                let old = before.get(c.corner_index)?;
                by_position(&after, *old, c.corner_index).map(|i| CornerEntry { corner_index: i, ..*c })
            })
            .collect()
    };
    reseated.into_iter().filter(|c| virtual_corner_segment_index(after_poly, c.corner_index).is_some()).collect()
}

/// Every entity of the drawing: model space, then every layout, then every block definition.
fn all_entities(d: &Drawing) -> impl Iterator<Item = &Arc<Entity>> + '_ {
    d.model.iter().chain(d.layouts.iter().flat_map(|l| l.entities.iter())).chain(d.blocks.values().flat_map(|b| b.entities.iter()))
}

/// Keeps `after`'s virtual corner tags on their vertices, given the drawing `before` the command
/// ran. Returns how many entities were rewritten; `after` is untouched when that is 0.
pub fn maintain_corners(before: &Drawing, after: &mut Drawing) -> Result<usize, SchemaError> {
    let mut writes: Vec<(Handle, Vec<CornerEntry>)> = Vec::new();
    for e in all_entities(after) {
        let prior = before.entity(e.handle);
        if prior.is_some_and(|b| Arc::ptr_eq(b, e) || b.kind == e.kind) {
            continue;
        }
        let Some((poly, corners)) = tagged(e)? else { continue };
        let next = match prior {
            Some(b) => match to_polyline(b) {
                Ok(c) => reseat(&vertices(&c.polyline), &poly, &corners, true),
                Err(_) => continue,
            },
            None => {
                let mut source = None;
                for b in all_entities(before) {
                    if let Some((bp, bc)) = tagged(b)?
                        && bc == corners
                        && source.as_ref().is_none_or(|(h, _): &(Handle, Polyline2)| b.handle < *h)
                    {
                        source = Some((b.handle, bp));
                    }
                }
                match source {
                    Some((_, bp)) => reseat(&vertices(&bp), &poly, &corners, false),
                    None => reseat(&vertices(&poly), &poly, &corners, false),
                }
            }
        };
        if next != corners {
            writes.push((e.handle, next));
        }
    }
    for (h, corners) in &writes {
        let mut err = None;
        after
            .modify_entity(*h, |e| {
                err = read_entity_data(&e.common)
                    .and_then(|mut data| {
                        data.corners.clone_from(corners);
                        set_entity_data(&mut e.common, &data)
                    })
                    .err();
            })
            .map_err(|e| SchemaError::Malformed { what: "corner maintenance".into(), message: e.to_string() })?;
        if let Some(e) = err {
            return Err(e);
        }
    }
    Ok(writes.len())
}

/// The after-command observer: runs [`maintain_corners`] on a copy of the current drawing and
/// writes it back only when a tag moved. A failure is echoed and the drawing is left as the command
/// left it.
pub fn corner_observer(s: &mut Session, before: &Arc<Drawing>) {
    let Ok(current) = s.doc() else { return };
    let mut after = current.clone();
    match maintain_corners(before, &mut after) {
        Ok(n) if n > 0 => {
            if let Ok(d) = s.doc_mut() {
                *d = after;
            }
        }
        Ok(_) => {}
        Err(e) => s.echo(format!("LAMB corner maintenance: {e}")),
    }
}

#[cfg(test)]
mod tests {
    //! Driven through the typed commands with the observer installed. The hooks are process-global:
    //! every test calls [`crate::commands::install_app`] first, which registers once.
    use lamb_core::geometry::VirtualCornerEntry;
    use serde_json::{Value, json};

    use super::*;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type Res<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    const FILLET: VirtualCornerEntry = VirtualCornerEntry::Fillet { radius: 10.0 };

    fn session() -> Res<Session> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        Ok(s)
    }

    fn handle(r: &Value) -> Res<String> {
        Ok(r["handle"].as_str().ok_or("no handle")?.to_owned())
    }

    fn entity(s: &Session, h: &str) -> Res<Entity> {
        let h = Handle::parse_hex(h).ok_or("bad handle")?;
        Ok((**s.doc()?.entity(h).ok_or("no entity")?).clone())
    }

    fn corners(s: &Session, h: &str) -> Res<Vec<CornerEntry>> {
        Ok(read_entity_data(&entity(s, h)?.common)?.corners)
    }

    fn at(s: &Session, h: &str, i: usize) -> Res<Point2> {
        let p = to_polyline(&entity(s, h)?).map_err(|e| format!("{e:?}"))?.polyline;
        Ok(*vertices(&p).get(i).ok_or("no vertex")?)
    }

    fn tag(i: usize) -> Vec<CornerEntry> {
        vec![CornerEntry { corner_index: i, entry: FILLET }]
    }

    /// An open U with the fillet on vertex 1 at (100, 0).
    fn filleted_u(s: &mut Session) -> Res<String> {
        let p = handle(&s.execute("pline", &json!({"vertices": [[0, 0], [100, 0], [100, 80], [0, 80]]}))?)?;
        s.execute("fillet", &json!({"h1": p, "p1": [50, 0], "h2": p, "p2": [100, 40], "radius": 10}))?;
        assert_eq!(corners(s, &p)?, tag(1));
        Ok(p)
    }

    #[test]
    fn insert_before_the_tag_shifts_it_and_undo_restores() -> R {
        let mut s = session()?;
        let p = filleted_u(&mut s)?;
        s.execute("pedit", &json!({"handle": p, "option": "vertex", "action": "insert", "index": 0, "to": [50, -10]}))?;
        assert_eq!(corners(&s, &p)?, tag(2));
        assert!(near(at(&s, &p, 2)?, Point2 { x: 100.0, y: 0.0 }));
        s.undo()?;
        assert_eq!(corners(&s, &p)?, tag(1));
        Ok(())
    }

    #[test]
    fn straightening_away_the_tagged_vertex_drops_it() -> R {
        let mut s = session()?;
        let p = filleted_u(&mut s)?;
        s.execute("pedit", &json!({"handle": p, "option": "vertex", "action": "straighten", "index": 0, "index2": 2}))?;
        assert!(corners(&s, &p)?.is_empty());
        Ok(())
    }

    #[test]
    fn reverse_maps_k_to_n_minus_1_minus_k() -> R {
        let mut s = session()?;
        let p = filleted_u(&mut s)?;
        s.execute("pedit", &json!({"handle": p, "option": "reverse"}))?;
        assert_eq!(corners(&s, &p)?, tag(2));
        assert!(near(at(&s, &p, 2)?, Point2 { x: 100.0, y: 0.0 }));
        Ok(())
    }

    #[test]
    fn move_keeps_and_an_infeasible_vertex_move_keeps() -> R {
        let mut s = session()?;
        let p = filleted_u(&mut s)?;
        s.execute("move", &json!({"handles": [p], "delta": [5, 7]}))?;
        assert_eq!(corners(&s, &p)?, tag(1));
        // Vertex 0 next to the corner: a 10 mm fillet no longer fits on a 2 mm leg.
        s.execute("pedit", &json!({"handle": p, "option": "vertex", "action": "move", "index": 0, "to": [103, 7]}))?;
        assert_eq!(corners(&s, &p)?, tag(1));
        Ok(())
    }

    #[test]
    fn a_copy_keeps_its_tag() -> R {
        let mut s = session()?;
        let p = filleted_u(&mut s)?;
        let before: Vec<Handle> = s.doc()?.model.handles();
        s.execute("copy", &json!({"handles": [p], "delta": [0, 200]}))?;
        let new: Vec<Handle> = s.doc()?.model.handles().into_iter().filter(|h| !before.contains(h)).collect();
        let [c] = new.as_slice() else { return Err("expected one copy".into()) };
        assert_eq!(corners(&s, &c.hex())?, tag(1));
        Ok(())
    }

    #[test]
    fn rule_4_drops_an_open_end() {
        let pt = |x: f64, y: f64| Point2 { x, y };
        let seg = |to| lamb_core::geometry::PolySegment { to, bulge: 0.0, corner_at_to: None };
        let l = Polyline2 { start: pt(0.0, 0.0), segments: vec![seg(pt(10.0, 0.0)), seg(pt(10.0, 10.0))], closed: false };
        let v = vertices(&l);
        assert!(reseat(&v, &l, &tag(0), true).is_empty());
        assert!(reseat(&v, &l, &tag(2), true).is_empty());
        assert_eq!(reseat(&v, &l, &tag(1), true), tag(1));
    }
}

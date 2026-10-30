//! Link maintenance step 0e: a link end whose polyline was reshaped — joined into another polyline
//! (FILLET/CHAMFER of two polylines, LAMB T562), trimmed, extended, re-ordered or rotated (FILLET
//! removing segments and closing an open polyline through its gap, LAMB T559) — moves onto the
//! segment that now holds its anchor point.
//!
//! [`reanchor_on_carriers`] looks at every link end that steps 0b and 0d left as it was before the
//! command, whose entity is erased or whose polyline changed in the same view, and whose current
//! anchor no longer holds its before point P. It searches the end's own entity (when it still
//! exists), then every other entity changed or new in the same view, in handle order, for a segment
//! on the old segment's **carrier** — a straight segment with both ends on the old one's infinite
//! line, an arc on the old one's circle — that holds P, and re-anchors the end there (same link id,
//! origin and stamps). No such segment: the end is left for step 1 (erased entity) or step 2 (no
//! longer crossing) to drop.

use std::collections::BTreeMap;

use cadcraft_doc::Drawing;
use lamb_core::geometry::Point2;
use lamb_core::geometry::polyline::{ResolvedSegment, arc_from_bulge, closest_point_on_segment, is_arc, point_at_param, resolved_segments};
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{Id, LinkAnchor, LinkRecord};

use crate::maintain::{EntityMap, entity_map};
use crate::project::DrawingProject;
use crate::schema::{SchemaError, read_document, write_document};

fn dist(a: Point2, b: Point2) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

/// Distance from `p` to the infinite line through straight `seg`; infinite for a degenerate one.
fn line_distance(seg: &ResolvedSegment, p: Point2) -> f64 {
    let (dx, dy) = (seg.to.x - seg.from.x, seg.to.y - seg.from.y);
    let len = dx.hypot(dy);
    if len <= TOLERANCE.coincidence_mm {
        return f64::INFINITY;
    }
    ((p.x - seg.from.x) * dy - (p.y - seg.from.y) * dx).abs() / len
}

/// `s` lies on `old`'s carrier: both straight with `s`'s ends on `old`'s infinite line, or both
/// arcs of one circle (either direction — a joined polyline may run reversed).
fn on_carrier(old: &ResolvedSegment, s: &ResolvedSegment) -> bool {
    let tol = TOLERANCE.coincidence_mm;
    match (is_arc(old.bulge), is_arc(s.bulge)) {
        (false, false) => line_distance(old, s.from) <= tol && line_distance(old, s.to) <= tol,
        (true, true) => match (arc_from_bulge(old.from, old.to, old.bulge), arc_from_bulge(s.from, s.to, s.bulge)) {
            (Some(a), Some(b)) => dist(a.center, b.center) <= tol && (a.radius - b.radius).abs() <= tol,
            _ => false,
        },
        _ => false,
    }
}

/// Step 0e. `before_links` are the links as the command found them; an end in `after` that differs
/// from its before value was already moved by step 0b or 0d and is left alone. Writes `after`'s
/// document once, only when something moved, and returns the moved links' ids.
pub(crate) fn reanchor_on_carriers(before_entities: &EntityMap, before_links: &[LinkRecord], after: &mut Drawing) -> Result<Vec<Id>, SchemaError> {
    let tol = TOLERANCE.coincidence_mm;
    let (links, moved) = {
        let p = DrawingProject::new(after)?;
        let after_entities = entity_map(&p);
        let segs: BTreeMap<Id, (Id, Vec<ResolvedSegment>)> =
            after_entities.iter().filter_map(|(id, (v, poly))| poly.as_ref().map(|poly| (*id, (*v, resolved_segments(poly))))).collect();
        let changed = |id: &Id| match (before_entities.get(id), after_entities.get(id)) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some((bv, bp)), Some((av, ap))) => bv != av || bp != ap,
        };
        let before_by_id: BTreeMap<Id, &LinkRecord> = before_links.iter().map(|l| (l.id, l)).collect();
        let remap = |end: &LinkAnchor, was: Option<&LinkAnchor>| -> Option<LinkAnchor> {
            if was != Some(end) {
                return None;
            }
            let (view, Some(old_poly)) = before_entities.get(&end.entity_id)? else { return None };
            match after_entities.get(&end.entity_id) {
                None => {}
                Some((av, ap)) if av == view && ap.as_ref() != Some(old_poly) => {}
                Some(_) => return None,
            }
            let old = resolved_segments(old_poly).into_iter().nth(end.segment_index)?;
            let at = point_at_param(&old, end.segment_param);
            let own = segs.get(&end.entity_id);
            if own.and_then(|(_, s)| s.get(end.segment_index)).is_some_and(|s| dist(point_at_param(s, end.segment_param), at) <= tol) {
                return None;
            }
            let others = segs.iter().filter(|(id, (v, _))| **id != end.entity_id && v == view && changed(id));
            own.map(|x| (&end.entity_id, x)).into_iter().chain(others).find_map(|(id, (_, ss))| {
                ss.iter().filter(|s| on_carrier(&old, s)).find_map(|s| {
                    let c = closest_point_on_segment(s, at);
                    (c.distance <= tol).then_some(LinkAnchor { entity_id: *id, segment_index: s.index, segment_param: c.t })
                })
            })
        };
        let mut moved = Vec::new();
        let links: Vec<LinkRecord> = p
            .document()
            .links
            .iter()
            .map(|link| {
                let was = before_by_id.get(&link.id);
                let (a, b) = (remap(&link.a, was.map(|w| &w.a)), remap(&link.b, was.map(|w| &w.b)));
                if a.is_none() && b.is_none() {
                    return link.clone();
                }
                let mut l = link.clone();
                if let Some(a) = a {
                    l.a = a;
                }
                if let Some(b) = b {
                    l.b = b;
                }
                moved.push(l.id);
                l
            })
            .collect();
        (links, moved)
    };
    if !moved.is_empty() {
        let mut doc = read_document(after)?;
        doc.links = links;
        write_document(after, &doc)?;
    }
    Ok(moved)
}

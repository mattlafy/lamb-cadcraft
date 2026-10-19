//! Link maintenance step 0d: a vertex added to, removed from or reversed in a linked polyline keeps
//! its links on their segments (TS `vertex.ts` `applyInsertVertex` / `applyDeleteVertex` remap).
//!
//! CADCraft's Add/Remove Vertex grips and `pedit` (vertex insert, straighten, reverse) edit a
//! polyline in place, on the same handle, so step 0b never sees them. [`remap_vertex_edits`]
//! compares each entity's vertex list (`start` + each `to`) before and after the command and, when
//! it changed by exactly one of these, moves every anchor on that entity:
//! - **insertion** (before's vertices an ordered subsequence of after's): an anchor on a split
//!   segment moves to the piece nearest its old point; any other shifts by the vertices inserted
//!   before it;
//! - **removal** (after's vertices an ordered subsequence of before's): an anchor on a segment
//!   folded into a merged one maps by arc length; one exactly at a removed vertex, or on a segment
//!   removed from an open end, drops its link (TS parity, ruling T469.1);
//! - **reversal**: index `n−1−i`, param `1−t`.
//!
//! A closed polyline may also have its start rotated. Any other change is left to step 2's stale
//! check.

use lamb_core::geometry::Point2;
use lamb_core::geometry::polyline::{ResolvedSegment, closest_point_on_segment, resolved_segments, segment_length};
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{Id, LinkAnchor, LinkRecord};

use cadcraft_doc::Drawing;

use crate::maintain::{EntityMap, entity_map};
use crate::project::DrawingProject;
use crate::schema::{SchemaError, read_document, write_document};

/// How one anchor fares under an edit.
#[derive(Debug, Clone, PartialEq)]
enum Fate {
    Keep,
    Move(usize, f64),
    Drop,
}

/// The vertex-list change between two polylines' segments, before's segments rotated by `rot`
/// (closed only; before segment `j` is rotated segment `(j + n − rot) % n`).
#[derive(Debug)]
enum Edit {
    /// `pos[j]`: after's vertex index of rotated before vertex `j`.
    Insert {
        rot: usize,
        pos: Vec<usize>,
    },
    /// `pos[k]`: rotated before's vertex index of after vertex `k`.
    Remove {
        rot: usize,
        pos: Vec<usize>,
    },
    Reverse,
}

fn near(a: Point2, b: Point2) -> bool {
    (a.x - b.x).hypot(a.y - b.y) <= TOLERANCE.coincidence_mm
}

fn vertices(segs: &[ResolvedSegment]) -> Vec<Point2> {
    segs.first().map(|s| s.from).into_iter().chain(segs.iter().map(|s| s.to)).collect()
}

/// `small`'s vertices as an ordered subsequence of `big`'s (greedy, earliest match), or `None`.
fn subsequence(small: &[Point2], big: &[Point2]) -> Option<Vec<usize>> {
    let mut pos = Vec::with_capacity(small.len());
    let mut from = 0;
    for v in small {
        let k = (from..big.len()).find(|&k| near(big[k], *v))?;
        pos.push(k);
        from = k + 1;
    }
    Some(pos)
}

/// `segs` rotated to start at segment `rot`, re-indexed from 0.
fn rotate(segs: &[ResolvedSegment], rot: usize) -> Vec<ResolvedSegment> {
    segs.iter().cycle().skip(rot).take(segs.len()).enumerate().map(|(index, s)| ResolvedSegment { index, ..*s }).collect()
}

fn detect(before: &[ResolvedSegment], after: &[ResolvedSegment], closed: bool) -> Option<Edit> {
    let (vb, va) = (vertices(before), vertices(after));
    if vb.len() < 2 || va.len() < 2 {
        return None;
    }
    if vb.len() == va.len() {
        let same = vb.iter().zip(&va).all(|(a, b)| near(*a, *b));
        let reversed = vb.iter().rev().zip(&va).all(|(a, b)| near(*a, *b));
        return (!same && reversed).then_some(Edit::Reverse);
    }
    // A closed polyline's explicit closing segment returns to its start; its start may rotate.
    let closed = closed && near(vb[0], vb[vb.len() - 1]) && near(va[0], va[va.len() - 1]);
    let rots: Vec<usize> = if closed { (0..before.len()).collect() } else { vec![0] };
    for rot in rots {
        let vr = vertices(&rotate(before, rot));
        if va.len() > vr.len() {
            if let Some(pos) = subsequence(&vr, &va) {
                // Closed: the same first vertex (after rotation); open: an end may be prepended.
                if !closed || pos[0] == 0 {
                    return Some(Edit::Insert { rot, pos });
                }
            }
        } else if let Some(pos) = subsequence(&va, &vr)
            && (!closed || pos[0] == 0)
        {
            return Some(Edit::Remove { rot, pos });
        }
    }
    None
}

/// The fate of an anchor on segment `index` at `t` of `before`, under `edit`.
fn fate(edit: &Edit, before: &[ResolvedSegment], after: &[ResolvedSegment], index: usize, t: f64) -> Fate {
    let n = before.len();
    if index >= n {
        return Fate::Keep;
    }
    let moved = |i: usize, p: f64| if i == index && (p - t).abs() <= 1e-12 { Fate::Keep } else { Fate::Move(i, p) };
    match edit {
        Edit::Reverse => moved(n - 1 - index, 1.0 - t),
        Edit::Insert { rot, pos } => {
            let j = (index + n - rot) % n;
            let (lo, hi) = (pos[j], pos[j + 1]);
            if hi - lo == 1 {
                return moved(lo, t);
            }
            let old = &rotate(before, *rot)[j];
            let at = lamb_core::geometry::polyline::point_at_param(old, t);
            after[lo..hi]
                .iter()
                .map(|s| (s.index, closest_point_on_segment(s, at)))
                .min_by(|a, b| a.1.distance.total_cmp(&b.1.distance))
                .map_or(Fate::Drop, |(i, c)| moved(i, c.t))
        }
        Edit::Remove { rot, pos } => {
            let j = (index + n - rot) % n;
            let Some(k) = (0..pos.len() - 1).find(|&k| pos[k] <= j && j < pos[k + 1]) else { return Fate::Drop };
            let (lo, hi) = (pos[k], pos[k + 1]);
            if hi - lo == 1 {
                return moved(k, t);
            }
            let rotated = rotate(before, *rot);
            if (t >= 1.0 - 1e-9 && j + 1 < hi) || (t <= 1e-9 && j > lo) {
                return Fate::Drop;
            }
            let lens: Vec<f64> = rotated[lo..hi].iter().map(segment_length).collect();
            let total: f64 = lens.iter().sum();
            if total <= 0.0 {
                return moved(k, 0.0);
            }
            let prior: f64 = lens[..j - lo].iter().sum();
            moved(k, (prior + t * lens[j - lo]) / total)
        }
    }
}

/// Step 0d. Remaps every link anchor on an entity whose polyline gained, lost or reversed vertices
/// in place (same id, same view). Returns (remapped link ids, dropped link ids). Writes `after`'s
/// document once, only if something changed.
pub(crate) fn remap_vertex_edits(before_entities: &EntityMap, after: &mut Drawing) -> Result<(Vec<Id>, Vec<Id>), SchemaError> {
    let (links, moved, dropped) = {
        let p = DrawingProject::new(after)?;
        let after_entities = entity_map(&p);
        let mut edits: Vec<(Id, Edit, Vec<ResolvedSegment>, Vec<ResolvedSegment>)> = Vec::new();
        for (id, (view, poly)) in &after_entities {
            let (Some(ap), Some((bv, Some(bp)))) = (poly, before_entities.get(id)) else { continue };
            if bv != view || ap == bp {
                continue;
            }
            let (bs, as_) = (resolved_segments(bp), resolved_segments(ap));
            if let Some(e) = detect(&bs, &as_, bp.closed && ap.closed) {
                edits.push((*id, e, bs, as_));
            }
        }
        let remap = |end: &LinkAnchor| -> Fate {
            edits
                .iter()
                .find(|(id, ..)| *id == end.entity_id)
                .map_or(Fate::Keep, |(_, e, bs, as_)| fate(e, bs, as_, end.segment_index, end.segment_param))
        };
        let (mut moved, mut dropped) = (Vec::new(), Vec::new());
        let mut links: Vec<LinkRecord> = Vec::with_capacity(p.document().links.len());
        if !edits.is_empty() {
            for link in &p.document().links {
                let (fa, fb) = (remap(&link.a), remap(&link.b));
                if fa == Fate::Drop || fb == Fate::Drop {
                    dropped.push(link.id);
                    continue;
                }
                let mut l = link.clone();
                for (end, f) in [(&mut l.a, fa), (&mut l.b, fb)] {
                    if let Fate::Move(i, t) = f {
                        end.segment_index = i;
                        end.segment_param = t;
                    }
                }
                if l != *link {
                    moved.push(l.id);
                }
                links.push(l);
            }
        }
        (links, moved, dropped)
    };
    if !moved.is_empty() || !dropped.is_empty() {
        let mut doc = read_document(after)?;
        doc.links = links;
        write_document(after, &doc)?;
    }
    Ok((moved, dropped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lamb_core::geometry::Polyline2;
    use lamb_core::geometry::polyline::PolySegment;

    fn pl(pts: &[(f64, f64)], closed: bool) -> Vec<ResolvedSegment> {
        let p = |(x, y): (f64, f64)| Point2 { x, y };
        let poly = Polyline2 {
            start: p(pts[0]),
            segments: pts[1..].iter().map(|&q| PolySegment { to: p(q), bulge: 0.0, corner_at_to: None }).collect(),
            closed,
        };
        resolved_segments(&poly)
    }

    fn run(b: &[(f64, f64)], a: &[(f64, f64)], closed: bool, i: usize, t: f64) -> Fate {
        let (bs, as_) = (pl(b, closed), pl(a, closed));
        let e = detect(&bs, &as_, closed).expect("an edit");
        fate(&e, &bs, &as_, i, t)
    }

    #[test]
    fn insertion_shifts_and_splits() {
        let b = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        let a = [(0.0, 0.0), (4.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        assert_eq!(run(&b, &a, false, 1, 0.5), Fate::Move(2, 0.5));
        assert_eq!(run(&b, &a, false, 0, 0.2), Fate::Move(0, 0.5));
        assert_eq!(run(&b, &a, false, 0, 0.7), Fate::Move(1, 0.5));
        // A prepended start (TS `segmentIndex === -1`): every index +1.
        let a = [(-5.0, 0.0), (0.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        assert_eq!(run(&b, &a, false, 0, 0.3), Fate::Move(1, 0.3));
    }

    #[test]
    fn removal_maps_by_length_and_drops_at_the_vertex() {
        let b = [(0.0, 0.0), (4.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        let a = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        assert_eq!(run(&b, &a, false, 0, 0.5), Fate::Move(0, 0.2));
        assert_eq!(run(&b, &a, false, 1, 0.5), Fate::Move(0, 0.7));
        assert_eq!(run(&b, &a, false, 2, 0.5), Fate::Move(1, 0.5));
        assert_eq!(run(&b, &a, false, 0, 1.0), Fate::Drop);
        assert_eq!(run(&b, &a, false, 1, 0.0), Fate::Drop);
        // An open end removed: its segment is gone.
        let a = [(4.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        assert_eq!(run(&b, &a, false, 0, 0.5), Fate::Drop);
        assert_eq!(run(&b, &a, false, 2, 0.5), Fate::Move(1, 0.5));
    }

    #[test]
    fn reversal_flips() {
        let b = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        let a = [(10.0, 10.0), (10.0, 0.0), (0.0, 0.0)];
        assert_eq!(run(&b, &a, false, 0, 0.25), Fate::Move(1, 0.75));
    }

    #[test]
    fn closed_insertion_with_a_rotated_start() {
        let b = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0), (0.0, 0.0)];
        // Starts at (10,0) now, with a vertex inserted in the closing segment.
        let a = [(10.0, 0.0), (10.0, 10.0), (0.0, 10.0), (0.0, 5.0), (0.0, 0.0), (10.0, 0.0)];
        assert_eq!(run(&b, &a, true, 1, 0.5), Fate::Move(0, 0.5));
        assert_eq!(run(&b, &a, true, 3, 0.75), Fate::Move(3, 0.5));
        assert_eq!(run(&b, &a, true, 0, 0.5), Fate::Move(4, 0.5));
    }

    #[test]
    fn other_changes_are_left_alone() {
        let b = pl(&[(0.0, 0.0), (10.0, 0.0)], false);
        assert!(detect(&b, &pl(&[(0.0, 0.0), (10.0, 1.0)], false), false).is_none());
        assert!(detect(&b, &pl(&[(0.0, 0.0), (5.0, 3.0), (10.0, 1.0)], false), false).is_none());
    }
}

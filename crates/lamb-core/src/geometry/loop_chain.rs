//! Chaining picked entities into closed boundary rings by endpoint matching. Port of
//! `geometry-primitives/src/loopChain.ts`.

use serde::{Deserialize, Serialize};

use super::point::{Point2, dist2};
use super::polyline::{Polyline2, reverse_polyline};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopChainEntity {
    pub entity_id: String,
    pub polyline: Polyline2,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainedLoop {
    /// Always `closed: true`.
    pub polyline: Polyline2,
    /// Every entity stitched into this ring, in traversal order.
    pub entity_ids: Vec<String>,
}

/// One end of an entity that never found a match.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopChainDanglingEnd {
    pub entity_id: String,
    pub point: Point2,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopChainResult {
    pub loops: Vec<ChainedLoop>,
    pub dangling: Vec<LoopChainDanglingEnd>,
}

fn poly_end(poly: &Polyline2) -> Point2 {
    poly.segments.last().map_or(poly.start, |s| s.to)
}

/// Chains entities into closed rings by matching endpoints within `tolerance`. An already-closed
/// polyline is a ring by itself; open ones are stitched onto the growing chain's tail in pick order,
/// reversed first when their END is what matches. Segment structure (bulge included) is preserved.
/// A chain that never closes is reported as two dangling ends.
pub fn chain_boundary_loops(entities: &[LoopChainEntity], tolerance: f64) -> LoopChainResult {
    let mut loops = Vec::new();
    let mut dangling = Vec::new();

    let mut remaining: Vec<&LoopChainEntity> = Vec::new();
    for e in entities {
        if e.polyline.closed {
            loops.push(ChainedLoop { polyline: e.polyline.clone(), entity_ids: vec![e.entity_id.clone()] });
        } else {
            remaining.push(e);
        }
    }
    while !remaining.is_empty() {
        let first = remaining.remove(0);
        let mut chain = Polyline2 { start: first.polyline.start, segments: first.polyline.segments.clone(), closed: false };
        let mut entity_ids = vec![first.entity_id.clone()];
        let mut closed = false;

        loop {
            let tail = poly_end(&chain);
            if !chain.segments.is_empty() && dist2(tail, chain.start) <= tolerance {
                closed = true;
                break;
            }
            let Some(idx) =
                remaining.iter().position(|e| dist2(e.polyline.start, tail) <= tolerance || dist2(poly_end(&e.polyline), tail) <= tolerance)
            else {
                break;
            };
            let m = remaining.remove(idx);
            let piece = if dist2(m.polyline.start, tail) <= tolerance { m.polyline.clone() } else { reverse_polyline(&m.polyline) };
            chain.segments.extend(piece.segments);
            entity_ids.push(m.entity_id.clone());
        }

        if closed {
            chain.closed = true;
            loops.push(ChainedLoop { polyline: chain, entity_ids });
        } else {
            let last_id = entity_ids.last().cloned().unwrap_or_default();
            dangling.push(LoopChainDanglingEnd { entity_id: first.entity_id.clone(), point: chain.start });
            dangling.push(LoopChainDanglingEnd { entity_id: last_id, point: poly_end(&chain) });
        }
    }

    LoopChainResult { loops, dangling }
}

//! Automatic linking through hollow markers, and finding the link behind a marker (TS
//! `linking.ts`).

use std::collections::{BTreeSet, HashSet, VecDeque};

use super::markers::{Marker, compute_markers_for_view};
use super::read::{EntityRef, ProjectRead};
use crate::geometry::polyline::{closest_point_on_segment, point_at_param, resolved_segments};
use crate::geometry::tolerance::TOLERANCE;
use crate::model::{Actor, Id, LinkAnchor, LinkOrigin, LinkRecord};

/// The links that drawing `entity` through its view's hollow markers should create (same layer
/// only). Pure: the caller appends them to the project's links.
pub fn find_auto_links(
    doc: &impl ProjectRead,
    entity: &EntityRef<'_>,
    created_by: &Actor,
    now: &str,
    mint: &mut impl FnMut() -> Id,
) -> Result<Vec<LinkRecord>, String> {
    let Some(view) = doc.view(entity.view_id) else { return Ok(Vec::new()) };
    let Some(poly) = entity.polyline.as_deref() else { return Ok(Vec::new()) };

    let markers = compute_markers_for_view(doc, view.id)?;
    let tolerance_mm = TOLERANCE.coincidence_mm + 2.0; // small drafting slack beyond pure coincidence
    let mut created = Vec::new();
    let mut already_linked: HashSet<(Id, usize)> = HashSet::new();

    for seg in resolved_segments(poly) {
        for marker in &markers {
            let key = (marker.source_entity_id, marker.source_segment_index);
            if already_linked.contains(&key) {
                continue;
            }
            let same_layer = doc.entity(marker.source_entity_id).is_some_and(|src| src.layer_id == entity.layer_id);
            if !same_layer {
                continue;
            }
            let closest = closest_point_on_segment(&seg, marker.position);
            if closest.distance > tolerance_mm {
                continue;
            }
            already_linked.insert(key);
            created.push(LinkRecord {
                id: mint(),
                a: LinkAnchor { entity_id: entity.id, segment_index: seg.index, segment_param: closest.t },
                b: LinkAnchor {
                    entity_id: marker.source_entity_id,
                    segment_index: marker.source_segment_index,
                    segment_param: marker.source_segment_param,
                },
                origin: LinkOrigin::Automatic,
                created_at: now.to_owned(),
                created_by: created_by.clone(),
                stale: None,
            });
        }
    }
    Ok(created)
}

/// The link backing a (filled) marker as seen from `view_id`: anchored to the marker's source
/// segment, with its other end in `view_id`.
pub fn find_link_for_marker<'a>(doc: &'a impl ProjectRead, marker: &Marker, view_id: Id) -> Option<&'a LinkRecord> {
    doc.links().find(|link| {
        let matches = |a: &LinkAnchor| a.entity_id == marker.source_entity_id && a.segment_index == marker.source_segment_index;
        if matches(&link.a) {
            doc.entity_view_id(link.b.entity_id) == Some(view_id)
        } else if matches(&link.b) {
            doc.entity_view_id(link.a.entity_id) == Some(view_id)
        } else {
            false
        }
    })
}

/// Whether `link` still holds: either end's current point lies within the auto-link tolerance of
/// a marker sourced from the other end's segment (TS `move.ts` `linkStillCoincides`). The same
/// test [`find_auto_links`] uses to create a link, run against the live geometry.
pub fn link_still_coincides(doc: &impl ProjectRead, link: &LinkRecord) -> Result<bool, String> {
    Ok(coincides_one_way(doc, &link.a, &link.b)? || coincides_one_way(doc, &link.b, &link.a)?)
}

fn coincides_one_way(doc: &impl ProjectRead, drawn: &LinkAnchor, source: &LinkAnchor) -> Result<bool, String> {
    let Some(entity) = doc.entity(drawn.entity_id) else { return Ok(false) };
    let Some(poly) = entity.polyline.as_deref() else { return Ok(false) };
    let Some(seg) = resolved_segments(poly).into_iter().find(|s| s.index == drawn.segment_index) else { return Ok(false) };
    let point = point_at_param(&seg, drawn.segment_param);
    let markers = compute_markers_for_view(doc, entity.view_id)?;
    let Some(marker) = markers.iter().find(|m| m.source_entity_id == source.entity_id && m.source_segment_index == source.segment_index) else {
        return Ok(false);
    };
    let tolerance_mm = TOLERANCE.coincidence_mm + 2.0;
    Ok((marker.position.x - point.x).hypot(marker.position.y - point.y) <= tolerance_mm)
}

/// Every entity an edit to `entity` propagates to (TS `lookup.ts` `linkedEntityComponent`): a
/// breadth-first walk over `Automatic` links only, entity-level, `entity` itself included. An
/// `Explicit` link is where propagation stops.
pub fn linked_entity_component(doc: &impl ProjectRead, entity: Id) -> BTreeSet<Id> {
    let mut visited = BTreeSet::from([entity]);
    let mut queue = VecDeque::from([entity]);
    while let Some(current) = queue.pop_front() {
        for link in doc.links() {
            if link.origin != LinkOrigin::Automatic {
                continue;
            }
            let other = if link.a.entity_id == current {
                Some(link.b.entity_id)
            } else if link.b.entity_id == current {
                Some(link.a.entity_id)
            } else {
                None
            };
            if let Some(o) = other.filter(|o| visited.insert(*o)) {
                queue.push_back(o);
            }
        }
    }
    visited
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::empty_project;

    fn link(id: u64, a: u64, b: u64, origin: LinkOrigin) -> LinkRecord {
        LinkRecord {
            id: Id(id),
            a: LinkAnchor { entity_id: Id(a), segment_index: 0, segment_param: 0.5 },
            b: LinkAnchor { entity_id: Id(b), segment_index: 0, segment_param: 0.5 },
            origin,
            created_at: String::new(),
            created_by: Actor::User { user_id: "u".into() },
            stale: None,
        }
    }

    #[test]
    fn component_follows_automatic_links_and_stops_at_explicit() {
        let mut p = empty_project(None);
        p.links = vec![
            link(1, 10, 11, LinkOrigin::Automatic),
            link(2, 12, 11, LinkOrigin::Automatic),
            link(3, 12, 13, LinkOrigin::Explicit),
            link(4, 14, 15, LinkOrigin::Automatic),
        ];
        let ids = |v: &[u64]| v.iter().map(|x| Id(*x)).collect::<BTreeSet<_>>();
        assert_eq!(linked_entity_component(&p, Id(10)), ids(&[10, 11, 12]));
        assert_eq!(linked_entity_component(&p, Id(13)), ids(&[13]));
        assert_eq!(linked_entity_component(&p, Id(99)), ids(&[99]), "an unlinked id is its own component");
    }
}

//! A project's unresolved gaps as data (TS `issues.ts`, T69): every conflicted / ambiguous /
//! defaulted span `resolve_cut_segment` reports, every conflicted `resolve_seen_z`, and every stale
//! link, as one fully ordered priority list. Derived fresh each call; nothing persists.

use serde::{Deserialize, Serialize};

use super::js_to_fixed;
use super::read::{ProjectRead, ViewKind};
use super::resolve::{ProvenanceKind, resolve_cut_segment, resolve_seen_z};
use crate::geometry::Point2;
use crate::geometry::polyline::{point_at_param, resolved_segments};
use crate::model::{Id, LineRole};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    ConflictedExtent,
    ConflictedSeenZ,
    AmbiguousExtent,
    StaleLink,
    DefaultedExtent,
    /// A laid wall run whose detail is wider or narrower than the drawn wall. Reported by the
    /// CADCraft adapter's assembly plan, never by [`find_issues`].
    WidthConflict,
    /// A wall junction with no corner detail (mitred or ghosted). Adapter-only, like
    /// [`IssueKind::WidthConflict`].
    UndetailedCorner,
    /// A wall opening bridge with no opening detail, in a view that tags some opening. Adapter-only,
    /// like [`IssueKind::WidthConflict`].
    UndetailedOpening,
    /// A wall run of an assembly plan that got no detail. Adapter-only, like
    /// [`IssueKind::WidthConflict`].
    UndetailedRun,
}

impl IssueKind {
    /// A conflict means the model is provably wrong; a default is only a working guess. The order
    /// is fixed here, never computed from the data.
    pub fn priority(self) -> u32 {
        match self {
            IssueKind::ConflictedExtent => 0,
            IssueKind::ConflictedSeenZ => 1,
            IssueKind::AmbiguousExtent => 2,
            IssueKind::StaleLink => 3,
            IssueKind::DefaultedExtent => 4,
            IssueKind::WidthConflict => 5,
            IssueKind::UndetailedCorner | IssueKind::UndetailedOpening => 6,
            IssueKind::UndetailedRun => 7,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub id: String,
    pub kind: IssueKind,
    pub view_id: Id,
    /// In `view_id`'s own local 2D space.
    pub location: Point2,
    pub entity_ids: Vec<Id>,
    pub priority: u32,
    pub note: String,
}

/// The kind's serialized name, as it appears in issue ids.
fn kind_name(kind: IssueKind) -> Result<String, String> {
    match serde_json::to_value(kind) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        other => Err(format!("issue kind did not serialize to a string: {other:?}")),
    }
}

fn sort_by_display(ids: &mut [Id]) {
    ids.sort_by_cached_key(Id::to_string);
}

fn join_ids(ids: &[Id]) -> String {
    ids.iter().map(Id::to_string).collect::<Vec<_>>().join(",")
}

/// The placed-vertical-view polyline entities linked to (entity, segment) — the same filters the
/// resolvers apply before trusting a link — deduped, sorted by display string. Names WHICH
/// entities a conflict is between; doesn't re-derive the conflict.
fn linked_vertical_entity_ids(doc: &impl ProjectRead, entity: Id, segment_index: usize) -> Vec<Id> {
    let mut ids: Vec<Id> = Vec::new();
    for l in doc.links_for_segment(entity, segment_index) {
        let other_id = l.other.entity_id;
        let Some(other_view) = doc.entity_view_id(other_id).and_then(|v| doc.view(v)) else { continue };
        if other_view.kind != ViewKind::Vertical || other_view.placement.is_none() {
            continue;
        }
        if !doc.entity(other_id).is_some_and(|e| e.polyline.is_some()) {
            continue;
        }
        if !ids.contains(&other_id) {
            ids.push(other_id);
        }
    }
    sort_by_display(&mut ids);
    ids
}

/// Every unresolved gap in the project, in a stable, fully ordered priority list.
pub fn find_issues(doc: &impl ProjectRead) -> Result<Vec<Issue>, String> {
    let mut issues: Vec<Issue> = Vec::new();

    for view in doc.views() {
        if view.kind != ViewKind::Plan || view.placement.is_none() {
            continue;
        }
        for entity in doc.entities(view.id) {
            let Some(poly) = entity.polyline.as_deref() else { continue };
            let role = doc.role_of(&entity);
            let segs = resolved_segments(poly);

            if role == LineRole::Cut {
                for seg in &segs {
                    for span in resolve_cut_segment(doc, &entity, seg.index)? {
                        let kind = match span.provenance {
                            ProvenanceKind::Conflicted => IssueKind::ConflictedExtent,
                            ProvenanceKind::Ambiguous => IssueKind::AmbiguousExtent,
                            ProvenanceKind::Defaulted => IssueKind::DefaultedExtent,
                            _ => continue,
                        };
                        let location = point_at_param(seg, (span.from_t + span.to_t) / 2.0);
                        let entity_ids =
                            if kind == IssueKind::ConflictedExtent { linked_vertical_entity_ids(doc, entity.id, seg.index) } else { vec![entity.id] };
                        let note = match kind {
                            IssueKind::ConflictedExtent => "Linked sections disagree on this segment's vertical extent.",
                            IssueKind::AmbiguousExtent => "No section crosses this stretch — its vertical extent can't be resolved from either side.",
                            _ => "No section crosses this segment at all — its vertical extent is a project default.",
                        };
                        issues.push(Issue {
                            id: format!("{}:{}:{}:{}:{}", kind_name(kind)?, entity.id, seg.index, js_to_fixed(span.from_t, 6), join_ids(&entity_ids)),
                            kind,
                            view_id: view.id,
                            location,
                            entity_ids,
                            priority: kind.priority(),
                            note: note.to_owned(),
                        });
                    }
                }
            }

            if role == LineRole::Seen {
                for seg in &segs {
                    if resolve_seen_z(doc, &entity, seg.index)?.provenance != ProvenanceKind::Conflicted {
                        continue;
                    }
                    let entity_ids = linked_vertical_entity_ids(doc, entity.id, seg.index);
                    let kind = IssueKind::ConflictedSeenZ;
                    issues.push(Issue {
                        id: format!("{}:{}:{}:{}", kind_name(kind)?, entity.id, seg.index, join_ids(&entity_ids)),
                        kind,
                        view_id: view.id,
                        location: point_at_param(seg, 0.5),
                        entity_ids,
                        priority: kind.priority(),
                        note: "Linked sections disagree on this seen line's Z.".to_owned(),
                    });
                }
            }
        }
    }

    for link in doc.links() {
        if link.stale != Some(true) {
            continue;
        }
        let Some(anchor_view) = doc.entity_view_id(link.a.entity_id).and_then(|v| doc.view(v)) else { continue };
        let Some(anchor_entity) = doc.entity(link.a.entity_id) else { continue };
        let Some(poly) = anchor_entity.polyline.as_deref() else { continue };
        let Some(seg) = resolved_segments(poly).into_iter().nth(link.a.segment_index) else { continue };

        let mut entity_ids = vec![link.a.entity_id, link.b.entity_id];
        sort_by_display(&mut entity_ids);
        let kind = IssueKind::StaleLink;
        issues.push(Issue {
            id: format!("{}:{}", kind_name(kind)?, link.id),
            kind,
            view_id: anchor_view.id,
            location: point_at_param(&seg, link.a.segment_param),
            entity_ids,
            priority: kind.priority(),
            note: "This link no longer crosses the cut it was made against.".to_owned(),
        });
    }

    // TS's comparator; `localeCompare` on these lowercase-ASCII-and-hyphen ids agrees with `str::cmp`.
    let first = |i: &Issue| i.entity_ids.first().map(Id::to_string).unwrap_or_default();
    issues.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.view_id.to_string().cmp(&b.view_id.to_string()))
            .then_with(|| first(a).cmp(&first(b)))
            .then_with(|| a.location.x.total_cmp(&b.location.x))
            .then_with(|| a.location.y.total_cmp(&b.location.y))
    });

    Ok(issues)
}

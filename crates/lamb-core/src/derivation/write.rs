//! The write side of the drawing, for the mutating sweeps (`reconcileAiLines`,
//! `invertReconcileAiLines`, `reconcileSolidCutLines`). They read and write interleaved — a new AI
//! line is pushed, then `findAutoLinks` reads the project with it in — so they take
//! `&mut impl ProjectWrite` rather than returning an edit list. `model::Project` implements it
//! here; CADCraft's adapter implements it over its own store.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::read::ProjectRead;
use crate::geometry::Polyline2;
use crate::model::{AiOrigin, AnyView, DrawingEntity, EntityGeometry, Id, LinkRecord, Project};

/// One entity taken out of a view, with where it was (lookup.ts `spliceOutEntities`' element).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovedEntity {
    pub view_id: Id,
    pub index: usize,
    pub entity: DrawingEntity,
}

pub trait ProjectWrite: ProjectRead {
    fn push_link(&mut self, link: LinkRecord);
    /// Removes every link `pred` accepts and returns them in document order.
    fn take_links(&mut self, pred: &mut dyn FnMut(&LinkRecord) -> bool) -> Vec<LinkRecord>;
    /// Appends to the end of `entity.view_id`'s entity list; Err if that view doesn't exist.
    fn push_entity(&mut self, entity: DrawingEntity) -> Result<(), String>;
    /// `view.entities.splice(min(index, len), 0, entity)`; no-op for an unknown view.
    fn insert_entity(&mut self, view: Id, index: usize, entity: DrawingEntity);
    /// lookup.ts `spliceOutEntities`: views in order, each view's entities scanned from the back.
    fn remove_entities(&mut self, ids: &HashSet<Id>) -> Vec<RemovedEntity>;
    /// entity.geometry = { kind: "polyline", geometry }; no-op if the entity is absent.
    fn set_polyline(&mut self, id: Id, geometry: Polyline2);
    /// Replaces the entity's whole aiOrigin (covers `accepted`, `sweptGeometry`, `assumptionNote`).
    fn set_ai_origin(&mut self, id: Id, origin: Option<AiOrigin>);
    /// project.suppressedProposals = keys (always present afterwards, even when empty).
    fn set_suppressed_proposals(&mut self, keys: Vec<String>);
}

fn entities_mut(view: &mut AnyView) -> &mut Vec<DrawingEntity> {
    match view {
        AnyView::Plan(v) => &mut v.entities,
        AnyView::Vertical(v) => &mut v.entities,
        AnyView::Detail(v) => &mut v.entities,
    }
}

impl Project {
    /// `findEntity`, mutably: first match, views in order.
    fn entity_mut(&mut self, id: Id) -> Option<&mut DrawingEntity> {
        self.views.iter_mut().find_map(|v| entities_mut(v).iter_mut().find(|e| e.id == id))
    }

    fn view_entities_mut(&mut self, view: Id) -> Option<&mut Vec<DrawingEntity>> {
        self.views.iter_mut().find(|v| v.id() == view).map(entities_mut)
    }
}

impl ProjectWrite for Project {
    fn push_link(&mut self, link: LinkRecord) {
        self.links.push(link);
    }

    fn take_links(&mut self, pred: &mut dyn FnMut(&LinkRecord) -> bool) -> Vec<LinkRecord> {
        let (taken, kept) = std::mem::take(&mut self.links).into_iter().partition(|l| pred(l));
        self.links = kept;
        taken
    }

    fn push_entity(&mut self, entity: DrawingEntity) -> Result<(), String> {
        let view = entity.view_id;
        let list = self.view_entities_mut(view).ok_or_else(|| format!("push_entity: no view {view}"))?;
        list.push(entity);
        Ok(())
    }

    fn insert_entity(&mut self, view: Id, index: usize, entity: DrawingEntity) {
        if let Some(list) = self.view_entities_mut(view) {
            let at = index.min(list.len());
            list.insert(at, entity);
        }
    }

    fn remove_entities(&mut self, ids: &HashSet<Id>) -> Vec<RemovedEntity> {
        let mut removed = Vec::new();
        for view in &mut self.views {
            let view_id = view.id();
            let list = entities_mut(view);
            for i in (0..list.len()).rev() {
                if ids.contains(&list[i].id) {
                    removed.push(RemovedEntity { view_id, index: i, entity: list.remove(i) });
                }
            }
        }
        removed
    }

    fn set_polyline(&mut self, id: Id, geometry: Polyline2) {
        if let Some(e) = self.entity_mut(id) {
            e.geometry = EntityGeometry::Polyline { geometry };
        }
    }

    fn set_ai_origin(&mut self, id: Id, origin: Option<AiOrigin>) {
        if let Some(e) = self.entity_mut(id) {
            e.ai_origin = origin;
        }
    }

    fn set_suppressed_proposals(&mut self, keys: Vec<String>) {
        self.suppressed_proposals = Some(keys);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Point2, polyline::line2};
    use crate::model::{LineRole, PlanView, empty_project};

    fn entity(id: u64, view: u64) -> DrawingEntity {
        DrawingEntity {
            id: Id(id),
            view_id: Id(view),
            role: LineRole::Bylayer,
            layer_id: "0".into(),
            geometry: EntityGeometry::Polyline { geometry: line2(Point2 { x: 0.0, y: 0.0 }, Point2 { x: 1.0, y: 0.0 }) },
            color_override: None,
            lineweight_override: None,
            ai_origin: None,
        }
    }

    fn plan(id: u64, entities: Vec<DrawingEntity>) -> AnyView {
        AnyView::Plan(PlanView { id: Id(id), name: format!("v{id}"), entities, placement: None })
    }

    fn ids(p: &Project, view: u64) -> Vec<u64> {
        p.entities(Id(view)).map(|e| e.id.0).collect()
    }

    #[test]
    fn remove_entities_views_in_order_each_scanned_from_the_back() {
        let mut p = empty_project(None);
        p.views = vec![plan(1, vec![entity(10, 1), entity(11, 1), entity(12, 1)]), plan(2, vec![entity(20, 2), entity(21, 2)])];
        let removed = p.remove_entities(&[Id(10), Id(12), Id(21), Id(20)].into_iter().collect());
        let got: Vec<(u64, usize, u64)> = removed.iter().map(|r| (r.view_id.0, r.index, r.entity.id.0)).collect();
        assert_eq!(got, vec![(1, 2, 12), (1, 0, 10), (2, 1, 21), (2, 0, 20)]);
        assert_eq!(ids(&p, 1), vec![11]);
        assert_eq!(ids(&p, 2), Vec::<u64>::new());
    }

    #[test]
    fn insert_entity_clamps_index_and_ignores_unknown_view() {
        let mut p = empty_project(None);
        p.views = vec![plan(1, vec![entity(10, 1), entity(11, 1)])];
        p.insert_entity(Id(1), 99, entity(12, 1));
        p.insert_entity(Id(1), 0, entity(13, 1));
        p.insert_entity(Id(1), 2, entity(14, 1));
        p.insert_entity(Id(9), 0, entity(15, 9));
        assert_eq!(ids(&p, 1), vec![13, 10, 14, 11, 12]);
        assert!(p.push_entity(entity(16, 9)).is_err());
    }
}

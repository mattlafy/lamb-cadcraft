//! The read side of the drawing, as derivation sees it (TS `lookup.ts`'s read functions plus the
//! `Project` fields derivation touches). Everything in `derivation` reads through [`ProjectRead`];
//! `model::Project` implements it here, CADCraft's adapter implements it over its own store.

use std::borrow::Cow;

use crate::geometry::{Polyline2, Transform2D};
use crate::model::{
    AiOrigin, AnyView, DrawingEntity, DrawingLayer, EntityGeometry, Id, Level, LineRole, LinkAnchor, LinkRecord, Placement, Project, ProjectDefaults,
    resolve_entity_role,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewKind {
    Plan,
    Vertical,
    Detail,
}

/// A view's own data without its entities: what `AnyView` minus `entities` is in TS.
#[derive(Clone, Copy, Debug)]
pub struct ViewRef<'a> {
    pub id: Id,
    pub name: &'a str,
    pub kind: ViewKind,
    pub placement: Option<&'a Placement>,
}

impl<'a> ViewRef<'a> {
    /// The placement's transform, either variant (TS `view.placement?.transform`).
    pub fn transform(&self) -> Option<&'a Transform2D> {
        match self.placement? {
            Placement::Plan { transform, .. } | Placement::Vertical { transform, .. } => Some(transform),
        }
    }

    /// `Some` only for a vertical placement.
    pub fn host_view_id(&self) -> Option<Id> {
        match self.placement? {
            Placement::Vertical { host_view_id, .. } => Some(*host_view_id),
            Placement::Plan { .. } => None,
        }
    }

    /// `Some` only for a vertical placement; in the host view's local coordinates.
    pub fn cut_line(&self) -> Option<&'a Polyline2> {
        match self.placement? {
            Placement::Vertical { cut_line, .. } => Some(cut_line),
            Placement::Plan { .. } => None,
        }
    }

    /// A vertical placement's stored flip; `false` when absent or not vertical.
    pub fn flipped(&self) -> bool {
        matches!(self.placement, Some(Placement::Vertical { flipped: Some(true), .. }))
    }
}

/// One entity as derivation sees it.
#[derive(Clone, Debug)]
pub struct EntityRef<'a> {
    pub id: Id,
    pub view_id: Id,
    /// As stored: may be `Bylayer`; resolve with [`ProjectRead::role_of`].
    pub role: LineRole,
    pub layer_id: &'a str,
    /// `Some` iff TS `geometry.kind === "polyline"`; view-local coords.
    pub polyline: Option<Cow<'a, Polyline2>>,
    pub ai_origin: Option<&'a AiOrigin>,
}

pub struct SegmentLink<'a> {
    pub link: &'a LinkRecord,
    pub mine: &'a LinkAnchor,
    pub other: &'a LinkAnchor,
}

pub trait ProjectRead {
    /// Document order.
    fn views(&self) -> impl Iterator<Item = ViewRef<'_>>;
    /// View order; empty if the view is unknown.
    fn entities(&self, view: Id) -> impl Iterator<Item = EntityRef<'_>>;
    /// Document order.
    fn links(&self) -> impl Iterator<Item = &LinkRecord>;
    fn layers(&self) -> &[DrawingLayer];
    fn level(&self, id: Id) -> Option<&Level>;
    fn defaults(&self) -> &ProjectDefaults;
    /// TS `project.suppressedProposals ?? []`.
    fn suppressed_proposals(&self) -> &[String];

    // Provided; these are lookup.ts. Don't override them.

    /// `findView`.
    fn view(&self, id: Id) -> Option<ViewRef<'_>> {
        self.views().find(|v| v.id == id)
    }

    /// `findEntity`: first match, views in order.
    fn entity(&self, id: Id) -> Option<EntityRef<'_>> {
        self.views().find_map(|v| self.entities(v.id).find(|e| e.id == id))
    }

    /// `entityViewId`: the view whose entity list holds it.
    fn entity_view_id(&self, id: Id) -> Option<Id> {
        self.views().find(|v| self.entities(v.id).any(|e| e.id == id)).map(|v| v.id)
    }

    fn role_of(&self, e: &EntityRef<'_>) -> LineRole {
        resolve_entity_role(self.layers(), e.role, e.layer_id)
    }

    /// `linksForSegment`: every link anchored to (entity, segment), checking the a side first.
    fn links_for_segment(&self, entity: Id, segment: usize) -> Vec<SegmentLink<'_>> {
        self.links()
            .filter_map(|link| {
                if link.a.entity_id == entity && link.a.segment_index == segment {
                    Some(SegmentLink { link, mine: &link.a, other: &link.b })
                } else if link.b.entity_id == entity && link.b.segment_index == segment {
                    Some(SegmentLink { link, mine: &link.b, other: &link.a })
                } else {
                    None
                }
            })
            .collect()
    }

    /// `hasLinkToView`.
    fn has_link_to_view(&self, entity: Id, segment: usize, view: Id) -> bool {
        self.links_for_segment(entity, segment).iter().any(|l| self.entity_view_id(l.other.entity_id) == Some(view))
    }

    /// `planeElevation`: the level's floor (0 when unknown) plus the cut height.
    fn plane_elevation(&self, level: Id, cut_height_mm: f64) -> f64 {
        self.level(level).map_or(0.0, |l| l.floor_elevation.value) + cut_height_mm
    }
}

impl ProjectRead for Project {
    fn views(&self) -> impl Iterator<Item = ViewRef<'_>> {
        self.views.iter().map(view_ref)
    }

    fn entities(&self, view: Id) -> impl Iterator<Item = EntityRef<'_>> {
        self.views.iter().filter(move |v| v.id() == view).take(1).flat_map(|v| v.entities().iter().map(entity_ref))
    }

    fn links(&self) -> impl Iterator<Item = &LinkRecord> {
        self.links.iter()
    }

    fn layers(&self) -> &[DrawingLayer] {
        &self.layers
    }

    fn level(&self, id: Id) -> Option<&Level> {
        self.levels.iter().find(|l| l.id == id)
    }

    fn defaults(&self) -> &ProjectDefaults {
        &self.defaults
    }

    fn suppressed_proposals(&self) -> &[String] {
        self.suppressed_proposals.as_deref().unwrap_or(&[])
    }
}

/// For exports that take a view argument.
pub fn view_ref(v: &AnyView) -> ViewRef<'_> {
    let kind = match v {
        AnyView::Plan(_) => ViewKind::Plan,
        AnyView::Vertical(_) => ViewKind::Vertical,
        AnyView::Detail(_) => ViewKind::Detail,
    };
    ViewRef { id: v.id(), name: v.name(), kind, placement: v.placement() }
}

/// For exports that take an entity argument.
pub fn entity_ref(e: &DrawingEntity) -> EntityRef<'_> {
    let polyline = match &e.geometry {
        EntityGeometry::Polyline { geometry } => Some(Cow::Borrowed(geometry)),
        _ => None,
    };
    EntityRef { id: e.id, view_id: e.view_id, role: e.role, layer_id: &e.layer_id, polyline, ai_origin: e.ai_origin.as_ref() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Point2, PolySegment};
    use crate::model::{Actor, LinkOrigin, PlanView, empty_project};

    fn line() -> Polyline2 {
        Polyline2 {
            start: Point2 { x: 0.0, y: 0.0 },
            segments: vec![PolySegment { to: Point2 { x: 1.0, y: 0.0 }, bulge: 0.0, corner_at_to: None }],
            closed: false,
        }
    }

    fn entity(id: u64, view: u64, geometry: EntityGeometry) -> DrawingEntity {
        DrawingEntity {
            id: Id(id),
            view_id: Id(view),
            role: LineRole::Bylayer,
            layer_id: "0".into(),
            geometry,
            color_override: None,
            lineweight_override: None,
            ai_origin: None,
        }
    }

    fn plan(id: u64, entities: Vec<DrawingEntity>) -> AnyView {
        AnyView::Plan(PlanView { id: Id(id), name: format!("v{id}"), entities, placement: None })
    }

    fn anchor(entity: u64, segment: usize) -> LinkAnchor {
        LinkAnchor { entity_id: Id(entity), segment_index: segment, segment_param: 0.5 }
    }

    fn link(id: u64, a: LinkAnchor, b: LinkAnchor) -> LinkRecord {
        LinkRecord {
            id: Id(id),
            a,
            b,
            origin: LinkOrigin::Automatic,
            created_at: String::new(),
            created_by: Actor::User { user_id: "u".into() },
            stale: None,
        }
    }

    fn project(views: Vec<AnyView>, links: Vec<LinkRecord>) -> Project {
        let mut p = empty_project(None);
        p.views = views;
        p.links = links;
        p
    }

    #[test]
    fn entity_is_first_match_in_view_order() {
        let p = project(
            vec![
                plan(1, vec![entity(10, 1, EntityGeometry::Polyline { geometry: line() })]),
                plan(2, vec![entity(10, 2, EntityGeometry::Polyline { geometry: line() })]),
            ],
            vec![],
        );
        let e = p.entity(Id(10));
        assert_eq!(e.map(|e| e.view_id), Some(Id(1)));
        assert_eq!(p.entity_view_id(Id(10)), Some(Id(1)));
        assert!(p.entity(Id(99)).is_none());
        assert_eq!(p.entities(Id(42)).count(), 0);
    }

    #[test]
    fn links_for_segment_a_side_before_b_side_with_mine_other_swapped() {
        let p = project(
            vec![],
            vec![
                link(100, anchor(7, 0), anchor(8, 0)),
                link(101, anchor(9, 1), anchor(7, 0)),
                link(102, anchor(7, 1), anchor(9, 0)),
                link(103, anchor(7, 0), anchor(9, 2)),
            ],
        );
        let got: Vec<(Id, Id, Id)> = p.links_for_segment(Id(7), 0).iter().map(|l| (l.link.id, l.mine.entity_id, l.other.entity_id)).collect();
        assert_eq!(got, vec![(Id(100), Id(7), Id(8)), (Id(101), Id(7), Id(9)), (Id(103), Id(7), Id(9))]);
    }

    #[test]
    fn circle_geometry_gives_no_polyline() {
        let p = project(vec![plan(1, vec![entity(10, 1, EntityGeometry::Circle { center: Point2 { x: 0.0, y: 0.0 }, radius: 5.0 })])], vec![]);
        let e = p.entity(Id(10));
        assert!(e.is_some_and(|e| e.polyline.is_none()));
    }
}

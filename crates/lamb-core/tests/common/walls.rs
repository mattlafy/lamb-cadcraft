//! Wall-drawing builders shared by the wall-graph tests (`walls.rs`, `wall_junctions.rs`).
#![allow(dead_code)]

use lamb_core::geometry::{Point2, PolySegment, Polyline2};
use lamb_core::model::{AnyView, DrawingEntity, EntityGeometry, Id, LineRole, PlanView, Project, empty_project};

pub const VIEW: Id = Id(100);

pub fn pt(x: f64, y: f64) -> Point2 {
    Point2 { x, y }
}

/// A polyline through `pts`; `bulges[i]` is segment i's bulge (missing = straight).
pub fn poly(pts: &[(f64, f64)], bulges: &[f64]) -> Polyline2 {
    Polyline2 {
        start: pt(pts[0].0, pts[0].1),
        segments: pts[1..]
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| PolySegment { to: pt(x, y), bulge: bulges.get(i).copied().unwrap_or(0.0), corner_at_to: None })
            .collect(),
        closed: false,
    }
}

pub fn ent(id: u64, layer: &str, role: LineRole, geometry: Polyline2) -> DrawingEntity {
    DrawingEntity {
        id: Id(id),
        view_id: VIEW,
        role,
        layer_id: layer.into(),
        geometry: EntityGeometry::Polyline { geometry },
        color_override: None,
        lineweight_override: None,
        ai_origin: None,
    }
}

/// A straight-segment face polyline on WALL-EXT.
pub fn wall(id: u64, pts: &[(f64, f64)]) -> DrawingEntity {
    ent(id, "layer-wall-ext", LineRole::Bylayer, poly(pts, &[]))
}

/// A plan view `VIEW` holding `entities`.
pub fn project(entities: Vec<DrawingEntity>) -> Project {
    let mut p = empty_project(None);
    p.views.push(AnyView::Plan(PlanView { id: VIEW, name: "plan".into(), entities, placement: None }));
    p
}

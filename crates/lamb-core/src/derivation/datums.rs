//! Datum planes across views (LAMB T578). A straight segment of a `datum`-role line is a plane, set
//! by the view it is drawn in, and every other placed view oriented across that plane shows it as a
//! grid line. Derived per view on every call, never written into a view's entities.
//!
//! Sources: a placed plan's segment is a vertical plane through its global XY line (finite, the
//! segment's extent); a placed vertical view's segment that is horizontal in (u, z) is a horizontal
//! plane at its z, and one that is vertical in (u, z) is a vertical plane through
//! `origin_xy + u·c_hat` along the view's gaze (infinite). Sloped and arc segments give nothing.
//!
//! Targets: a placed plan shows every vertical plane; a placed vertical view shows a vertical plane
//! where its XY line crosses the cut line (taken infinite; a plan-sourced plane only where its
//! finite segment reaches it) and every horizontal plane. Never in its own source view; detail and
//! unplaced views show nothing.

use serde::Serialize;

use super::projection::{
    Uz, VerticalBasis, plan_global_to_local_xy, plan_local_to_global_xy, project_to_u, vertical_basis, vertical_gaze_dir, vertical_local_to_uz,
    vertical_uz_to_local,
};
use super::read::{ProjectRead, ViewKind, ViewRef};
use crate::geometry::intersect::{infinite_line_intersect, intersect_segment_with_infinite_line};
use crate::geometry::point::{add2, dist2, normalize2, sub2};
use crate::geometry::polyline::resolved_segments;
use crate::geometry::tolerance::TOLERANCE;
use crate::geometry::transform2d::world_to_local_dir2;
use crate::geometry::{Point2, ResolvedSegment};
use crate::model::{Id, LineRole};

/// TS `intersectSegmentWithInfiniteLine` default, as `markers.rs` uses it.
const INTERSECT_EPS: f64 = 1e-6;

/// One datum plane drawn in `view_id`'s local coordinates. When `infinite`, `to − from` is a unit
/// direction and the line runs both ways without end; otherwise `from`–`to` is its finite extent.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatumLineOverlay {
    pub view_id: Id,
    pub source_view_id: Id,
    pub source_entity_id: Id,
    pub source_segment_index: usize,
    pub from: Point2,
    pub to: Point2,
    pub infinite: bool,
}

#[derive(Debug, Clone, Copy)]
enum Plane {
    /// A plan segment's vertical plane: global XY `a`–`b`.
    VerticalFinite {
        a: Point2,
        b: Point2,
    },
    /// A section's vertical plane: through global XY `p` along unit `dir`.
    VerticalInfinite {
        p: Point2,
        dir: Point2,
    },
    Horizontal {
        z: f64,
    },
}

struct Datum {
    source_view_id: Id,
    source_entity_id: Id,
    source_segment_index: usize,
    plane: Plane,
}

/// A placed vertical view's basis, when its host is a placed plan.
fn basis_of(doc: &impl ProjectRead, view: &ViewRef<'_>) -> Result<Option<VerticalBasis>, String> {
    if view.kind != ViewKind::Vertical || view.placement.is_none() {
        return Ok(None);
    }
    let Some(host) = view.host_view_id().and_then(|h| doc.view(h)) else { return Ok(None) };
    if host.kind != ViewKind::Plan || host.transform().is_none() {
        return Ok(None);
    }
    vertical_basis(view, &host).map(Some)
}

/// Every datum plane in the project, by source view.
fn datum_planes(doc: &impl ProjectRead) -> Result<Vec<Datum>, String> {
    let coinc = TOLERANCE.coincidence_mm;
    let mut out = Vec::new();
    for view in doc.views() {
        let basis = match view.kind {
            ViewKind::Plan if view.transform().is_some() => None,
            ViewKind::Vertical => match basis_of(doc, &view)? {
                Some(b) => Some(b),
                None => continue,
            },
            _ => continue,
        };
        for entity in doc.entities(view.id) {
            if doc.role_of(&entity) != LineRole::Datum {
                continue;
            }
            let Some(poly) = entity.polyline.as_deref() else { continue };
            for seg in resolved_segments(poly) {
                if seg.bulge != 0.0 || dist2(seg.from, seg.to) <= coinc {
                    continue;
                }
                let plane = match &basis {
                    None => Plane::VerticalFinite { a: plan_local_to_global_xy(&view, seg.from)?, b: plan_local_to_global_xy(&view, seg.to)? },
                    Some(basis) => {
                        let (s, e) = (vertical_local_to_uz(&view, seg.from), vertical_local_to_uz(&view, seg.to));
                        if (s.z - e.z).abs() <= coinc {
                            Plane::Horizontal { z: (s.z + e.z) / 2.0 }
                        } else if (s.u - e.u).abs() <= coinc {
                            let u = (s.u + e.u) / 2.0;
                            let p = add2(basis.origin_xy, Point2 { x: u * basis.c_hat.x, y: u * basis.c_hat.y });
                            Plane::VerticalInfinite { p, dir: vertical_gaze_dir(basis.c_hat, view.flipped()) }
                        } else {
                            continue;
                        }
                    }
                };
                out.push(Datum { source_view_id: view.id, source_entity_id: entity.id, source_segment_index: seg.index, plane });
            }
        }
    }
    Ok(out)
}

/// Every datum plane from another view that `view_id` is oriented across, as a line in its local
/// coordinates. Nothing for a detail, unplaced or missing view.
pub fn compute_datum_lines_for_view(doc: &impl ProjectRead, view_id: Id) -> Result<Vec<DatumLineOverlay>, String> {
    let Some(view) = doc.view(view_id) else { return Ok(Vec::new()) };
    let target_basis = match view.kind {
        ViewKind::Plan if view.transform().is_some() => None,
        ViewKind::Vertical => match basis_of(doc, &view)? {
            Some(b) => Some(b),
            None => return Ok(Vec::new()),
        },
        _ => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for d in datum_planes(doc)? {
        if d.source_view_id == view_id {
            continue;
        }
        let line = |from: Point2, to: Point2, infinite: bool| DatumLineOverlay {
            view_id,
            source_view_id: d.source_view_id,
            source_entity_id: d.source_entity_id,
            source_segment_index: d.source_segment_index,
            from,
            to,
            infinite,
        };
        match (&target_basis, d.plane) {
            (None, Plane::VerticalFinite { a, b }) => out.push(line(plan_global_to_local_xy(&view, a)?, plan_global_to_local_xy(&view, b)?, false)),
            (None, Plane::VerticalInfinite { p, dir }) => {
                let Some(t) = view.transform() else { continue };
                let base = plan_global_to_local_xy(&view, p)?;
                out.push(line(base, add2(base, normalize2(world_to_local_dir2(t, dir))), true));
            }
            (None, Plane::Horizontal { .. }) => {}
            (Some(basis), plane) => {
                let cut_to = add2(basis.origin_xy, basis.c_hat);
                let u = match plane {
                    Plane::VerticalFinite { a, b } => {
                        let seg = ResolvedSegment { index: d.source_segment_index, from: a, to: b, bulge: 0.0 };
                        let Some(hit) = intersect_segment_with_infinite_line(&seg, basis.origin_xy, cut_to, INTERSECT_EPS).into_iter().next() else {
                            continue;
                        };
                        project_to_u(basis, hit.point)
                    }
                    Plane::VerticalInfinite { p, dir } => {
                        let Some(hit) = infinite_line_intersect(p, add2(p, dir), basis.origin_xy, cut_to) else { continue };
                        project_to_u(basis, hit.point)
                    }
                    Plane::Horizontal { z } => {
                        let base = vertical_uz_to_local(&view, Uz { u: 0.0, z });
                        let along = normalize2(sub2(vertical_uz_to_local(&view, Uz { u: 1.0, z }), base));
                        out.push(line(base, add2(base, along), true));
                        continue;
                    }
                };
                let base = vertical_uz_to_local(&view, Uz { u, z: 0.0 });
                let up = normalize2(sub2(vertical_uz_to_local(&view, Uz { u, z: 1.0 }), base));
                out.push(line(base, add2(base, up), true));
            }
        }
    }
    Ok(out)
}

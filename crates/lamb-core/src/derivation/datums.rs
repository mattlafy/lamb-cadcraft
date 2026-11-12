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
//!
//! Grids (LAMB T703): every vertical plane is a building grid line. Coincident planes, whichever
//! views drew them, are one grid with one name ([`named_grids`]): a grid within 45° of global Y is
//! numbered 1, 2, 3… west to east, any other lettered A, B, C… south to north (no I or O, then AA,
//! AB…), unless one of its members carries an explicit name. Horizontal planes are never named.

use serde::Serialize;

use super::projection::{
    Uz, VerticalBasis, plan_global_to_local_xy, plan_local_to_global_xy, project_to_u, vertical_basis, vertical_gaze_dir, vertical_local_to_uz,
    vertical_uz_to_local,
};
use super::read::{ProjectRead, ViewKind, ViewRef};
use crate::geometry::intersect::{infinite_line_intersect, intersect_segment_with_infinite_line};
use crate::geometry::point::{add2, cross2, dist2, dot2, normalize2, sub2};
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

/// One datum segment that belongs to a grid: where it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GridMember {
    pub view_id: Id,
    pub entity_id: Id,
    pub segment_index: usize,
}

/// A building grid: its name, a point on it and its unit direction in global XY (`dir.y > 0`, or
/// `dir.x > 0` when `dir.y == 0`), and every datum segment that draws it, in project order.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedGrid {
    pub name: String,
    pub p: Point2,
    pub dir: Point2,
    pub members: Vec<GridMember>,
}

/// Numbered grids run within 45° of global Y; lettered ones are the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Numbered,
    Lettered,
}

/// The `i`-th (from 0) automatic name in `family`: `1`, `2`, … or `A` … `Z` without `I` and `O`,
/// then `AA`, `AB`, ….
fn grid_label(family: Family, i: usize) -> String {
    match family {
        Family::Numbered => (i + 1).to_string(),
        Family::Lettered => {
            const LETTERS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
            let mut n = i + 1;
            let mut out = Vec::new();
            while n > 0 {
                n -= 1;
                out.push(LETTERS.get(n % LETTERS.len()).copied().unwrap_or(b'A'));
                n /= LETTERS.len();
            }
            out.reverse();
            String::from_utf8(out).unwrap_or_default()
        }
    }
}

/// `dir` turned so that `dir.y > 0`, or `dir.x > 0` when `dir.y == 0`.
fn canonical_dir(dir: Point2) -> Point2 {
    if dir.y < 0.0 || (dir.y == 0.0 && dir.x < 0.0) { Point2 { x: -dir.x, y: -dir.y } } else { dir }
}

type Group = (Point2, Point2, Vec<GridMember>);

/// Every building grid in the project: coincident vertical datum planes grouped (parallel within
/// `TOLERANCE.angle_deg`, no more than `coincidence_mm` apart), numbered grids west to east and then
/// lettered ones south to north. A grid with a member in `names` takes that name (the first by
/// member order); the automatic names skip every explicit one.
pub fn named_grids(doc: &impl ProjectRead, names: &[(GridMember, String)]) -> Result<Vec<NamedGrid>, String> {
    let sin_tol = TOLERANCE.angle_deg.to_radians().sin();
    let mut groups: Vec<Group> = Vec::new();
    for d in datum_planes(doc)? {
        let (p, dir) = match d.plane {
            Plane::VerticalFinite { a, b } => (a, normalize2(sub2(b, a))),
            Plane::VerticalInfinite { p, dir } => (p, normalize2(dir)),
            Plane::Horizontal { .. } => continue,
        };
        let dir = canonical_dir(dir);
        let member = GridMember { view_id: d.source_view_id, entity_id: d.source_entity_id, segment_index: d.source_segment_index };
        let same =
            groups.iter_mut().find(|(gp, gd, _)| cross2(*gd, dir).abs() <= sin_tol && cross2(*gd, sub2(p, *gp)).abs() <= TOLERANCE.coincidence_mm);
        match same {
            Some((_, _, members)) => members.push(member),
            None => groups.push((p, dir, vec![member])),
        }
    }
    let explicit = |members: &[GridMember]| members.iter().find_map(|m| names.iter().find(|(n, _)| n == m).map(|(_, s)| s.clone()));
    let mut out = Vec::new();
    for family in [Family::Numbered, Family::Lettered] {
        let mut fam: Vec<(f64, &Group)> = groups
            .iter()
            .filter(|(_, dir, _)| (dir.y.abs() >= dir.x.abs()) == (family == Family::Numbered))
            .map(|g| {
                let n = Point2 { x: g.1.y, y: -g.1.x };
                let flip = match family {
                    Family::Numbered => n.x < 0.0,
                    Family::Lettered => n.y < 0.0,
                };
                let n = if flip { Point2 { x: -n.x, y: -n.y } } else { n };
                (dot2(g.0, n), g)
            })
            .collect();
        fam.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut next = 0;
        for (_, (p, dir, members)) in fam {
            let name = match explicit(members) {
                Some(n) => n,
                None => loop {
                    let n = grid_label(family, next);
                    next += 1;
                    if !names.iter().any(|(_, s)| *s == n) {
                        break n;
                    }
                },
            };
            out.push(NamedGrid { name, p: *p, dir: *dir, members: members.clone() });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod grid_tests {
    use super::*;
    use crate::geometry::transform2d::IDENTITY_TRANSFORM;
    use crate::geometry::{PolySegment, Polyline2};
    use crate::model::{AnyView, DrawingEntity, EntityGeometry, Field, Level, Placement, PlanView, Project, Provenance, empty_project};

    const LEVEL: Id = Id(5);

    fn datum(id: u64, view: Id, a: (f64, f64), b: (f64, f64)) -> DrawingEntity {
        DrawingEntity {
            id: Id(id),
            view_id: view,
            role: LineRole::Datum,
            layer_id: "0".into(),
            geometry: EntityGeometry::Polyline {
                geometry: Polyline2 {
                    start: Point2 { x: a.0, y: a.1 },
                    segments: vec![PolySegment { to: Point2 { x: b.0, y: b.1 }, bulge: 0.0, corner_at_to: None }],
                    closed: false,
                },
            },
            color_override: None,
            lineweight_override: None,
            ai_origin: None,
        }
    }

    /// Placed plans (identity transforms) with the given entities.
    fn project(plans: Vec<(Id, Vec<DrawingEntity>)>) -> Project {
        let mut p = empty_project(None);
        p.levels.push(Level {
            id: LEVEL,
            name: "L1".into(),
            order: 1.0,
            floor_elevation: Field { value: 0.0, provenance: Provenance::Defaulted { source_default_id: "test".into() } },
        });
        for (id, entities) in plans {
            p.views.push(AnyView::Plan(PlanView {
                id,
                name: format!("p{}", id.0),
                entities,
                placement: Some(Placement::Plan { level_id: LEVEL, cut_elevation: 0.0, transform: IDENTITY_TRANSFORM }),
            }));
        }
        p
    }

    fn names(g: &[NamedGrid]) -> Vec<&str> {
        g.iter().map(|g| g.name.as_str()).collect()
    }

    #[test]
    fn labels_skip_i_and_o_and_run_on_to_two_letters() {
        let l = |i| grid_label(Family::Lettered, i);
        assert_eq!([l(0), l(7), l(8), l(13), l(23), l(24), l(25)], ["A", "H", "J", "P", "Z", "AA", "AB"]);
        assert_eq!(grid_label(Family::Numbered, 0), "1");
    }

    #[test]
    fn the_same_line_in_two_plans_is_one_grid() -> Result<(), String> {
        let p =
            project(vec![(Id(1), vec![datum(10, Id(1), (0.0, 5000.0), (0.0, 0.0))]), (Id(2), vec![datum(20, Id(2), (0.0, -1000.0), (0.0, 3000.0))])]);
        let g = named_grids(&p, &[])?;
        assert_eq!(names(&g), ["1"]);
        assert_eq!(g[0].members.len(), 2);
        assert_eq!(g[0].members[1], GridMember { view_id: Id(2), entity_id: Id(20), segment_index: 0 });
        assert!(g[0].dir.y > 0.0);
        Ok(())
    }

    #[test]
    fn numbered_west_to_east_lettered_south_to_north_and_explicit_names_kept() -> Result<(), String> {
        let ents = vec![
            datum(1, Id(1), (6000.0, 0.0), (6000.0, 9000.0)),
            datum(2, Id(1), (0.0, 0.0), (0.0, 9000.0)),
            datum(3, Id(1), (3000.0, 9000.0), (3000.0, 0.0)),
            datum(4, Id(1), (9000.0, 4000.0), (0.0, 4000.0)),
            datum(5, Id(1), (0.0, 0.0), (9000.0, 0.0)),
        ];
        let p = project(vec![(Id(1), ents)]);
        let g = named_grids(&p, &[])?;
        assert_eq!(names(&g), ["1", "2", "3", "A", "B"]);
        assert_eq!(g.iter().map(|g| g.members[0].entity_id.0).collect::<Vec<_>>(), [2, 3, 1, 5, 4]);

        let x = GridMember { view_id: Id(1), entity_id: Id(3), segment_index: 0 };
        let g = named_grids(&p, &[(x, "2".into())])?;
        assert_eq!(names(&g), ["1", "2", "3", "A", "B"], "the explicit one where it sits");
        let g = named_grids(&p, &[(GridMember { entity_id: Id(2), ..x }, "2".into())])?;
        assert_eq!(names(&g), ["2", "1", "3", "A", "B"], "the auto names skip it");
        Ok(())
    }
}

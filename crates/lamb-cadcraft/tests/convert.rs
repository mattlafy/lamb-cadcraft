//! The entity <-> `Polyline2` adapter: every supported kind converts per segment at the right WCS
//! points, flattening warns without refusing, and `to -> from -> to` reproduces the polyline.

use std::f64::consts::{FRAC_PI_2, PI};

use cadcraft_doc::{Arc, Circle, Ellipse, Entity, EntityKind, Handle, Line, LwPolyline};
use cadcraft_geom::{PolyVertex, Vec2, Vec3};
use lamb_cadcraft::{Flattened, UnsupportedReason, from_polyline, to_polyline};
use lamb_core::geometry::circle_fit::{CircleFit, circle_as_polyline};
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::geometry::{Point2, PolySegment, Polyline2, VirtualCornerEntry};

type R = Result<(), String>;

const EPS: f64 = TOLERANCE.coincidence_mm / 1000.0;

fn ent(kind: EntityKind) -> Entity {
    Entity::new(Handle(1), kind)
}

fn pt(x: f64, y: f64) -> Point2 {
    Point2 { x, y }
}

fn close(a: Point2, b: Point2) -> bool {
    (a.x - b.x).abs() < EPS && (a.y - b.y).abs() < EPS
}

fn same(a: &Polyline2, b: &Polyline2) -> R {
    let ok = a.closed == b.closed
        && close(a.start, b.start)
        && a.segments.len() == b.segments.len()
        && a.segments.iter().zip(&b.segments).all(|(s, t)| close(s.to, t.to) && (s.bulge - t.bulge).abs() < 1e-9);
    if ok { Ok(()) } else { Err(format!("polylines differ:\n  {a:?}\n  {b:?}")) }
}

fn to(e: &Entity) -> Result<Polyline2, String> {
    let c = to_polyline(e).map_err(|u| format!("{u:?}"))?;
    Ok(c.polyline)
}

/// `to -> from -> to` reproduces the first polyline.
fn round_trip(e: &Entity) -> Result<Polyline2, String> {
    let first = to(e)?;
    let (kind, warnings) = from_polyline(&first);
    if !warnings.is_empty() {
        return Err(format!("unexpected warnings {warnings:?}"));
    }
    let back = to(&ent(kind))?;
    same(&first, &back)?;
    Ok(first)
}

fn arc(cx: f64, cy: f64, r: f64, start: f64, end: f64) -> Entity {
    ent(EntityKind::Arc(Arc { center: Vec3::new(cx, cy, 0.0), radius: r, start, end }))
}

fn lw(pts: &[(f64, f64, f64)], closed: bool) -> Entity {
    let vertices = pts.iter().map(|&(x, y, b)| PolyVertex::with_bulge(Vec2::new(x, y), b)).collect();
    ent(EntityKind::LwPolyline(LwPolyline { vertices, closed, const_width: 0.0, elevation: 0.0, plinegen: false }))
}

#[test]
fn line_is_one_straight_segment() -> R {
    let e = ent(EntityKind::Line(Line { a: Vec3::new(1.0, 2.0, 0.0), b: Vec3::new(4.0, -6.0, 0.0) }));
    let p = round_trip(&e)?;
    same(&p, &Polyline2 { start: pt(1.0, 2.0), segments: vec![PolySegment { to: pt(4.0, -6.0), bulge: 0.0, corner_at_to: None }], closed: false })?;
    if !matches!(from_polyline(&p).0, EntityKind::Line(_)) {
        return Err("expected a Line back".into());
    }
    Ok(())
}

#[test]
fn ccw_arcs_small_large_and_across_zero() -> R {
    // 90 degrees: (10,0) -> (0,10), bulge tan(22.5deg).
    let p = round_trip(&arc(0.0, 0.0, 10.0, 0.0, FRAC_PI_2))?;
    same(
        &p,
        &Polyline2 {
            start: pt(10.0, 0.0),
            segments: vec![PolySegment { to: pt(0.0, 10.0), bulge: (PI / 8.0).tan(), corner_at_to: None }],
            closed: false,
        },
    )?;
    // 270 degrees: bulge > 1.
    let p = round_trip(&arc(5.0, 5.0, 2.0, 0.0, 1.5 * PI))?;
    if !(p.segments[0].bulge > 1.0 && close(p.segments[0].to, pt(5.0, 3.0))) {
        return Err(format!("270deg arc wrong: {p:?}"));
    }
    // Across angle 0: 330deg -> 30deg is a 60deg CCW sweep.
    let p = round_trip(&arc(0.0, 0.0, 10.0, 11.0 * PI / 6.0, PI / 6.0))?;
    let b = (PI / 12.0).tan();
    if !((p.segments[0].bulge - b).abs() < 1e-9 && p.start.y < 0.0 && p.segments[0].to.y > 0.0) {
        return Err(format!("arc across 0 wrong: {p:?}"));
    }
    if !matches!(from_polyline(&p).0, EntityKind::Arc(_)) {
        return Err("expected an Arc back".into());
    }
    Ok(())
}

#[test]
fn circle_is_circle_as_polyline() -> R {
    let e = ent(EntityKind::Circle(Circle { center: Vec3::new(3.0, -4.0, 0.0), radius: 7.0 }));
    let p = round_trip(&e)?;
    same(&p, &circle_as_polyline(&CircleFit { center: pt(3.0, -4.0), radius: 7.0 }))?;
    if !matches!(from_polyline(&p).0, EntityKind::Circle(_)) {
        return Err("expected a Circle back".into());
    }
    Ok(())
}

#[test]
fn open_and_closed_lwpolylines() -> R {
    let open = round_trip(&lw(&[(0.0, 0.0, 0.0), (10.0, 0.0, 0.5), (10.0, 10.0, 0.0), (0.0, 12.0, 0.0)], false))?;
    if !(open.segments.len() == 3 && open.segments[1].bulge == 0.5 && close(open.segments[1].to, pt(10.0, 10.0))) {
        return Err(format!("open lwpolyline wrong: {open:?}"));
    }
    let closed = round_trip(&lw(&[(0.0, 0.0, 0.0), (10.0, 0.0, 0.0), (10.0, 10.0, 0.3)], true))?;
    if !(closed.closed && closed.segments.len() == 3 && close(closed.segments[2].to, pt(0.0, 0.0)) && closed.segments[2].bulge == 0.3) {
        return Err(format!("closed lwpolyline wrong: {closed:?}"));
    }
    // A last vertex repeating the first is dropped, not turned into a zero-length segment.
    let dup = round_trip(&lw(&[(0.0, 0.0, 0.0), (10.0, 0.0, 0.0), (10.0, 10.0, 0.3), (0.0, 0.0, 0.0)], true))?;
    same(&dup, &closed)?;
    if !matches!(from_polyline(&closed).0, EntityKind::LwPolyline(ref l) if l.vertices.len() == 3 && l.closed && l.vertices[2].bulge == 0.3) {
        return Err("closed polyline should come back as a 3-vertex closed LwPolyline".into());
    }
    let degenerate = to_polyline(&lw(&[(1.0, 1.0, 0.0), (1.0, 1.0, 0.0)], false));
    if !matches!(degenerate, Err(ref u) if u.reason == UnsupportedReason::Degenerate) {
        return Err(format!("expected Degenerate, got {degenerate:?}"));
    }
    Ok(())
}

#[test]
fn mirrored_extrusion_lands_at_wcs_points() -> R {
    let down = Vec3::new(0.0, 0.0, -1.0);
    // OCS arc centre (10,0), 0 -> 90deg: OCS points (20,0) and (10,10) are WCS (-20,0), (-10,10).
    let mut a = arc(10.0, 0.0, 10.0, 0.0, FRAC_PI_2);
    a.common.extrusion = down;
    let p = round_trip(&a)?;
    // Still CCW in WCS: from the image of the OCS end to the image of the OCS start.
    same(
        &p,
        &Polyline2 {
            start: pt(-10.0, 10.0),
            segments: vec![PolySegment { to: pt(-20.0, 0.0), bulge: (PI / 8.0).tan(), corner_at_to: None }],
            closed: false,
        },
    )?;
    let mut c = ent(EntityKind::Circle(Circle { center: Vec3::new(5.0, 2.0, 0.0), radius: 1.0 }));
    c.common.extrusion = down;
    same(&round_trip(&c)?, &circle_as_polyline(&CircleFit { center: pt(-5.0, 2.0), radius: 1.0 }))?;
    let mut l = lw(&[(1.0, 0.0, 0.5), (3.0, 0.0, 0.0)], false);
    l.common.extrusion = down;
    let p = round_trip(&l)?;
    if !(close(p.start, pt(-1.0, 0.0)) && close(p.segments[0].to, pt(-3.0, 0.0)) && p.segments[0].bulge == -0.5) {
        return Err(format!("mirrored lwpolyline wrong: {p:?}"));
    }
    Ok(())
}

#[test]
fn flattening_warns_and_still_converts() -> R {
    let z = ent(EntityKind::Line(Line { a: Vec3::new(0.0, 0.0, 5.0), b: Vec3::new(1.0, 0.0, 5.0) }));
    let mut thick = arc(0.0, 0.0, 1.0, 0.0, 1.0);
    thick.common.thickness = 200.0;
    let mut wide = lw(&[(0.0, 0.0, 0.0), (5.0, 0.0, 0.0)], false);
    if let EntityKind::LwPolyline(ref mut l) = wide.kind {
        l.const_width = 2.0;
    }
    let mut elev = lw(&[(0.0, 0.0, 0.0), (5.0, 0.0, 0.0)], false);
    if let EntityKind::LwPolyline(ref mut l) = elev.kind {
        l.elevation = 3000.0;
        l.vertices[0].start_width = 1.0;
    }
    for (e, want) in [
        (&z, vec![Flattened::Elevation]),
        (&thick, vec![Flattened::Thickness]),
        (&wide, vec![Flattened::Width]),
        (&elev, vec![Flattened::Elevation, Flattened::Width]),
    ] {
        let c = to_polyline(e).map_err(|u| format!("{u:?}"))?;
        if c.warnings != want {
            return Err(format!("{:?}: warnings {:?}, want {want:?}", e.kind.type_name(), c.warnings));
        }
    }
    Ok(())
}

#[test]
fn non_participating_and_tilted_are_unsupported() -> R {
    let ell = ent(EntityKind::Ellipse(Ellipse { center: Vec3::ZERO, major: Vec3::new(2.0, 0.0, 0.0), ratio: 0.5, start: 0.0, end: 2.0 * PI }));
    match to_polyline(&ell) {
        Err(u) if u.reason == UnsupportedReason::NonParticipating && u.type_name == "Ellipse" => {}
        other => return Err(format!("ellipse: {other:?}")),
    }
    let mut tilted = arc(0.0, 0.0, 1.0, 0.0, 1.0);
    tilted.common.extrusion = Vec3::new(1.0, 0.0, 0.0);
    match to_polyline(&tilted) {
        Err(u) if u.reason == UnsupportedReason::Extrusion => {}
        other => return Err(format!("tilted arc: {other:?}")),
    }
    match to_polyline(&arc(0.0, 0.0, 1.0, 1.0, 1.0)) {
        Err(u) if u.reason == UnsupportedReason::Degenerate => Ok(()),
        other => Err(format!("zero-sweep arc: {other:?}")),
    }
}

#[test]
fn virtual_corner_warns_on_the_way_back() -> R {
    let mut p = to(&lw(&[(0.0, 0.0, 0.0), (10.0, 0.0, 0.0), (10.0, 10.0, 0.0)], false))?;
    p.segments[0].corner_at_to = Some(VirtualCornerEntry::Fillet { radius: 2.0 });
    let (kind, warnings) = from_polyline(&p);
    if warnings != vec![Flattened::VirtualCorner] || !matches!(kind, EntityKind::LwPolyline(_)) {
        return Err(format!("got {kind:?} {warnings:?}"));
    }
    Ok(())
}

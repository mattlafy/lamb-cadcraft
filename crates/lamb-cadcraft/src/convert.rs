//! The entity <-> `Polyline2` adapter (`docs/07-architecture/cadcraft-integration.md` §3, §6 in the
//! LAMB repo): lamb-core sees only polylines with bulge arcs.
//!
//! [`to_polyline`] turns a CADCraft Line, Arc, Circle or LwPolyline into a WCS `Polyline2`, one
//! segment per source segment and deterministically, so a link anchor
//! `{entityId, segmentIndex, segmentParam}` means the same thing on both sides. [`from_polyline`]
//! turns a `Polyline2` back into the simplest entity kind. What a `Polyline2` cannot carry (Z,
//! thickness, widths) is dropped with a [`Flattened`] warning rather than refused; an extrusion
//! other than `(0,0,±1)` or a kind that does not participate is [`Unsupported`].

use std::f64::consts::{PI, TAU};

use cadcraft_doc::{Arc, Circle, Entity, EntityKind, Line, LwPolyline};
use cadcraft_geom::{PolyVertex, Vec2, Vec3, arc_to_bulge};
use lamb_core::geometry::circle_fit::{CircleFit, circle_as_polyline, polyline_as_circle};
use lamb_core::geometry::polyline::{CornerEntry, arc_from_bulge, virtual_corner_segment_index};
use lamb_core::geometry::tolerance::{TOLERANCE, deg_to_rad};
use lamb_core::geometry::{Point2, PolySegment, Polyline2};

/// Something the conversion dropped because the other side has no place for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flattened {
    /// A non-zero Z (point Z, or an LwPolyline's `elevation`) was projected onto Z = 0.
    Elevation,
    /// A non-zero `thickness` was ignored.
    Thickness,
    /// A non-zero `const_width` or vertex width was ignored.
    Width,
    /// `from_polyline` dropped one or more `corner_at_to` virtual-corner tags.
    VirtualCorner,
}

/// Why an entity has no `Polyline2` form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unsupported {
    /// The entity's `EntityKind::type_name()`.
    pub type_name: &'static str,
    pub reason: UnsupportedReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnsupportedReason {
    /// The kind does not participate in LAMB geometry (§6: Ellipse, Spline, Text, Insert, ...).
    NonParticipating,
    /// The extrusion is neither `(0,0,1)` nor `(0,0,-1)`.
    Extrusion,
    /// Zero-sweep arc, or a polyline with fewer than two distinct vertices.
    Degenerate,
}

/// A converted entity: the WCS polyline plus whatever was flattened on the way.
#[derive(Clone, Debug, PartialEq)]
pub struct Converted {
    pub polyline: Polyline2,
    pub warnings: Vec<Flattened>,
}

fn p2(v: Vec2) -> Point2 {
    Point2 { x: v.x, y: v.y }
}

fn near(a: Point2, b: Point2) -> bool {
    (a.x - b.x).hypot(a.y - b.y) <= TOLERANCE.coincidence_mm
}

fn push_once(w: &mut Vec<Flattened>, f: Flattened) {
    if !w.contains(&f) {
        w.push(f);
    }
}

/// `Some(false)` for `(0,0,1)`, `Some(true)` for `(0,0,-1)` (mirrored OCS), `None` otherwise.
fn mirrored(extrusion: Vec3) -> Option<bool> {
    let len = extrusion.len();
    if len <= 0.0 || !len.is_finite() {
        return None;
    }
    let cos = extrusion.z / len;
    let max = deg_to_rad(TOLERANCE.angle_deg).cos();
    if cos >= max {
        Some(false)
    } else if cos <= -max {
        Some(true)
    } else {
        None
    }
}

/// OCS -> WCS for a `(0,0,-1)` extrusion: X mirrors, bulges negate.
fn mirror(p: &Polyline2) -> Polyline2 {
    let m = |q: Point2| Point2 { x: -q.x, y: q.y };
    Polyline2 {
        start: m(p.start),
        segments: p.segments.iter().map(|s| PolySegment { to: m(s.to), bulge: -s.bulge, corner_at_to: s.corner_at_to }).collect(),
        closed: p.closed,
    }
}

fn seg(to: Point2, bulge: f64) -> PolySegment {
    PolySegment { to, bulge, corner_at_to: None }
}

/// An entity as a WCS `Polyline2`.
///
/// - Line: open, one straight segment `a -> b` (Line ignores extrusion).
/// - Arc: open, one CCW segment from the start point to the end point. Under a `(0,0,-1)`
///   extrusion the WCS arc runs from the image of the OCS end to the image of the OCS start, so
///   it is still CCW (angles map `θ -> π - θ`).
/// - Circle: exactly `circle_as_polyline` of the WCS circle.
/// - LwPolyline: segment `i` runs to vertex `i+1` with vertex `i`'s bulge; a closed one gets an
///   explicit closing segment (lamb-core has no implicit one), after dropping a last vertex that
///   repeats the first.
pub fn to_polyline(e: &Entity) -> Result<Converted, Unsupported> {
    let type_name = e.kind.type_name();
    let fail = |reason| Unsupported { type_name, reason };
    let mut warnings = Vec::new();
    if e.common.thickness != 0.0 {
        warnings.push(Flattened::Thickness);
    }
    let polyline = match &e.kind {
        EntityKind::Line(Line { a, b }) => {
            if a.z != 0.0 || b.z != 0.0 {
                push_once(&mut warnings, Flattened::Elevation);
            }
            Polyline2 { start: Point2 { x: a.x, y: a.y }, segments: vec![seg(Point2 { x: b.x, y: b.y }, 0.0)], closed: false }
        }
        EntityKind::Circle(Circle { center, radius }) => {
            let flip = mirrored(e.common.extrusion).ok_or_else(|| fail(UnsupportedReason::Extrusion))?;
            if center.z != 0.0 {
                push_once(&mut warnings, Flattened::Elevation);
            }
            if *radius < TOLERANCE.coincidence_mm {
                return Err(fail(UnsupportedReason::Degenerate));
            }
            let cx = if flip { -center.x } else { center.x };
            circle_as_polyline(&CircleFit { center: Point2 { x: cx, y: center.y }, radius: *radius })
        }
        EntityKind::Arc(Arc { center, radius, start, end }) => {
            let flip = mirrored(e.common.extrusion).ok_or_else(|| fail(UnsupportedReason::Extrusion))?;
            if center.z != 0.0 {
                push_once(&mut warnings, Flattened::Elevation);
            }
            let sweep = (end - start).rem_euclid(TAU);
            if sweep < deg_to_rad(TOLERANCE.angle_deg) || *radius < TOLERANCE.coincidence_mm {
                return Err(fail(UnsupportedReason::Degenerate));
            }
            let (cx, a0, a1) = if flip { (-center.x, PI - end, PI - start) } else { (center.x, *start, *end) };
            let at = |a: f64| Point2 { x: cx + radius * a.cos(), y: center.y + radius * a.sin() };
            Polyline2 { start: at(a0), segments: vec![seg(at(a1), arc_to_bulge(sweep))], closed: false }
        }
        EntityKind::LwPolyline(lw) => {
            let flip = mirrored(e.common.extrusion).ok_or_else(|| fail(UnsupportedReason::Extrusion))?;
            lw_to_polyline(lw, flip, &mut warnings).ok_or_else(|| fail(UnsupportedReason::Degenerate))?
        }
        _ => return Err(fail(UnsupportedReason::NonParticipating)),
    };
    Ok(Converted { polyline, warnings })
}

pub(crate) fn lw_to_polyline(lw: &LwPolyline, flip: bool, warnings: &mut Vec<Flattened>) -> Option<Polyline2> {
    if lw.elevation != 0.0 {
        push_once(warnings, Flattened::Elevation);
    }
    if lw.const_width != 0.0 || lw.vertices.iter().any(|v| v.start_width != 0.0 || v.end_width != 0.0) {
        push_once(warnings, Flattened::Width);
    }
    let mut verts: &[PolyVertex] = &lw.vertices;
    if lw.closed
        && let [first, .., last] = verts
        && near(p2(first.p), p2(last.p))
    {
        verts = &verts[..verts.len() - 1];
    }
    let (first, rest) = verts.split_first()?;
    if rest.iter().all(|v| near(p2(v.p), p2(first.p))) {
        return None;
    }
    let mut segments: Vec<PolySegment> = verts.windows(2).map(|w| seg(p2(w[1].p), w[0].bulge)).collect();
    if lw.closed {
        let last_bulge = verts.last().map_or(0.0, |v| v.bulge);
        segments.push(seg(p2(first.p), last_bulge));
    }
    let p = Polyline2 { start: p2(first.p), segments, closed: lw.closed };
    Some(if flip { mirror(&p) } else { p })
}

/// The simplest entity kind for a WCS `Polyline2`: a `Circle` when `polyline_as_circle` says so,
/// a `Line` for one open straight segment, an `Arc` for one open CCW arc segment, else an
/// `LwPolyline` (a closed one drops the explicit closing segment the entity's implicit one
/// replaces). Entity `Common` (layer, colour, `ext`) is the caller's.
///
/// A single CW arc segment stays a one-segment `LwPolyline` rather than becoming an `Arc`: an
/// `Arc` is always CCW, so its start/end would be the polyline's end/start and `to_polyline` of it
/// would traverse the other way, reversing every `segmentParam` anchored on it.
pub fn from_polyline(p: &Polyline2) -> (EntityKind, Vec<Flattened>) {
    let mut warnings = Vec::new();
    if p.segments.iter().any(|s| s.corner_at_to.is_some()) {
        warnings.push(Flattened::VirtualCorner);
    }
    if let Some(fit) = polyline_as_circle(p) {
        return (EntityKind::Circle(Circle { center: Vec3::new(fit.center.x, fit.center.y, 0.0), radius: fit.radius }), warnings);
    }
    if !p.closed
        && let [only] = p.segments.as_slice()
    {
        let v3 = |q: Point2| Vec3::new(q.x, q.y, 0.0);
        match arc_from_bulge(p.start, only.to, only.bulge) {
            None if only.bulge == 0.0 => return (EntityKind::Line(Line { a: v3(p.start), b: v3(only.to) }), warnings),
            None => {}
            Some(arc) if arc.ccw => {
                return (EntityKind::Arc(Arc { center: v3(arc.center), radius: arc.radius, start: arc.start_angle, end: arc.end_angle }), warnings);
            }
            Some(_) => {}
        }
    }
    let mut vertices = vec![PolyVertex::new(Vec2::new(p.start.x, p.start.y))];
    for s in &p.segments {
        if let Some(prev) = vertices.last_mut() {
            prev.bulge = s.bulge;
        }
        vertices.push(PolyVertex::new(Vec2::new(s.to.x, s.to.y)));
    }
    // The closing segment's bulge now sits on the vertex before the duplicate of the start; the
    // duplicate itself goes, since the entity's closing segment is implicit.
    if p.closed && vertices.len() > 2 && p.segments.last().is_some_and(|s| near(s.to, p.start)) {
        vertices.pop();
    }
    (EntityKind::LwPolyline(LwPolyline { vertices, closed: p.closed, const_width: 0.0, elevation: 0.0, plinegen: false }), warnings)
}

/// Puts stored virtual-corner tags back on `p`: each entry's tag goes on the segment that owns
/// vertex `corner_index` (`virtual_corner_segment_index`). An index that is not a corner of `p` —
/// stale once a CADCraft command has edited the vertices — is skipped; returns how many were set.
pub fn apply_corners(p: &mut Polyline2, corners: &[CornerEntry]) -> usize {
    let mut set = 0;
    for c in corners {
        if let Some(i) = virtual_corner_segment_index(p, c.corner_index)
            && let Some(s) = p.segments.get_mut(i)
        {
            s.corner_at_to = Some(c.entry);
            set += 1;
        }
    }
    set
}

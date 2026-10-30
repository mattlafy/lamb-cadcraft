//! FILLET and CHAMFER with polyline segments (#466): two straight segments of one polyline
//! (with any segments between them on the picked side removed, closing an open polyline when
//! the corner lies in its gap), or a straight segment of an open polyline with a line or with a
//! segment of another open polyline, which join into one polyline through the corner.

use cadcraft_doc::{Entity, EntityKind, Handle, LwPolyline};
use cadcraft_geom::{EPS, Line, PolyVertex, Polyline, Segment, Vec2, Vec3, arc_to_bulge, bulge_to_arc, line_line_infinite};

use super::{curves, other, push_entity, set_kind};
use crate::cmd::helpers::{arc, line};
use crate::{Result, Session};

/// CHAMFER's distances back from a corner along the first and second picked objects, given the
/// unit directions from the corner towards their kept sides.
pub(crate) type ChamferDistances<'a> = &'a dyn Fn(Vec2, Vec2) -> Result<(f64, f64)>;

/// A cut corner: the new end on each side and the bulge from the first to the second (0 for a
/// chamfer or a sharp corner).
struct Corner {
    t1: Vec2,
    t2: Vec2,
    bulge: f64,
}

/// The corner at `x` between runs leaving it along `u1` (first pick) and `u2` (second pick),
/// whose kept ends are `room1` and `room2` away.
fn corner(x: Vec2, (u1, room1): (Vec2, f64), (u2, room2): (Vec2, f64), r: f64, chamfer: Option<ChamferDistances>) -> Result<Corner> {
    if u1 == Vec2::ZERO || u2 == Vec2::ZERO {
        return Err(other("The segments meet at a kept end; nothing to cut."));
    }
    let (d1, d2, bulge) = if let Some(f) = chamfer {
        let (d1, d2) = f(u1, u2)?;
        (d1, d2, 0.0)
    } else if r <= 1e-12 {
        (0.0, 0.0, 0.0)
    } else {
        let half = u1.dot(u2).clamp(-1.0, 1.0).acos() / 2.0;
        if half < 1e-9 || (std::f64::consts::FRAC_PI_2 - half).abs() < 1e-9 {
            return Err(other("The segments are parallel or in line."));
        }
        let d = r / half.tan();
        // Travelling in along -u1 and out along u2.
        let turn = (-u1).cross(u2).atan2((-u1).dot(u2));
        (d, d, arc_to_bulge(turn))
    };
    if !(d1.is_finite() && d2.is_finite()) || d1 > room1 + 1e-9 || d2 > room2 + 1e-9 {
        return Err(other(if chamfer.is_some() { "The chamfer distances are too large." } else { "Radius is too large." }));
    }
    Ok(Corner { t1: x + u1 * d1, t2: x + u2 * d2, bulge })
}

fn push_point(vs: &mut Vec<PolyVertex>, v: PolyVertex) {
    match vs.last_mut() {
        Some(l) if l.p.near(v.p, 1e-12) => l.bulge = v.bulge,
        _ => vs.push(v),
    }
}

/// The index of the polyline segment nearest `p`.
pub(crate) fn picked_segment(segs: &[Segment], p: Vec2) -> Option<usize> {
    (0..segs.len()).min_by(|a, b| {
        let d = |i: &usize| segs.get(*i).map_or(f64::INFINITY, |s| s.closest(p).dist(p));
        d(a).total_cmp(&d(b))
    })
}

/// The straight segment nearest `pick`.
fn picked_line(segs: &[Segment], pick: Vec2) -> Result<(usize, Line)> {
    let k = picked_segment(segs, pick).ok_or_else(|| other("Select a polyline segment."))?;
    match segs.get(k) {
        Some(Segment::Line(l)) => Ok((k, *l)),
        _ => Err(other("Fillet and chamfer work on straight polyline segments.")),
    }
}

/// One pick against the corner `x`: the picked segment, and whether its trimmed end is the
/// segment's end vertex (the corner lies ahead of the pick).
fn pick_side(segs: &[Segment], pick: Vec2, x: Vec2) -> Result<(usize, bool)> {
    let (k, l) = picked_line(segs, pick)?;
    Ok((k, l.param_of(x) > l.param_of(pick)))
}

/// With TRIMMODE 0: the arc (or chamfer line) on its own, both objects left as they are.
fn add_loose(s: &mut Session, near: Handle, c: &Corner) -> Result<Option<Handle>> {
    let e = curves::entity(s, near)?;
    let space = s.doc()?.space_of(near).unwrap_or_else(|| s.space());
    let k = match bulge_to_arc(c.t1, c.t2, c.bulge) {
        Some((a, _)) => arc(&a),
        None if !c.t1.near(c.t2, 1e-12) => line(c.t1, c.t2),
        None => return Ok(None),
    };
    Ok(Some(push_entity(s, &space, e.common.clone(), k)?))
}

/// Two segments of one polyline. LAMB fork patch 31: any number of segments may lie between them.
/// The polyline is a cycle of `n` segments (segment `k` from vertex `k` to `k + 1`), an open one
/// closed by a virtual gap segment `n - 1`. Each pick keeps the part of its segment containing it
/// and trims or extends the other end to the corner; the run beyond those trimmed ends is removed.
/// When that run holds an open polyline's gap, the polyline's tails go with it and it closes.
fn same_polyline(
    s: &mut Session,
    h: Handle,
    pl: &LwPolyline,
    (p1, p2): (Vec2, Vec2),
    r: f64,
    chamfer: Option<ChamferDistances>,
    trim: bool,
) -> Result<Option<Handle>> {
    let segs = Polyline { vertices: pl.vertices.clone(), closed: pl.closed }.segments();
    let n = pl.vertices.len();
    let ns = segs.len();
    let (Some(a), Some(b)) = (picked_segment(&segs, p1), picked_segment(&segs, p2)) else { return Err(other("Select two segments.")) };
    if a == b || n < 2 {
        return Err(other("Select two different segments of the polyline."));
    }
    let next = |k: usize| if pl.closed { Some((k + 1) % ns) } else { (k + 1 < ns).then_some(k + 1) };
    // Adjacent through a real vertex: (arriving segment, leaving segment).
    let adjacent = if next(a) == Some(b) {
        Some((a, b))
    } else if next(b) == Some(a) {
        Some((b, a))
    } else {
        None
    };
    // LAMB fork patch 10: an adapter may keep the shared vertex and tag it instead (crate::vcorner).
    let hook = crate::vcorner::virtual_corner_hook().filter(|_| adjacent.is_some() && trim);
    let vertex = adjacent.map_or(0, |(i, _)| (i + 1) % n);
    if let Some(f) = hook
        && chamfer.is_none()
        && f(s, h, vertex, crate::vcorner::CornerOp::Fillet { radius: r })?
    {
        return Ok(Some(h));
    }
    let (Some(Segment::Line(sa)), Some(Segment::Line(sb))) = (segs.get(a), segs.get(b)) else {
        return Err(other("Fillet and chamfer work on straight polyline segments."));
    };
    let (sa, sb) = (*sa, *sb);
    if let Some(f) = hook
        && let Some(ch) = chamfer
        && let Some((i, _)) = adjacent
    {
        let (si, sj) = if i == a { (sa, sb) } else { (sb, sa) };
        let (ui, uj) = ((si.a - si.b).normalized(), (sj.b - sj.a).normalized());
        let in_order = i == a;
        let (df, ds) = if in_order { ch(ui, uj)? } else { ch(uj, ui)? };
        let (distance1, distance2) = if in_order { (df, ds) } else { (ds, df) };
        if f(s, h, vertex, crate::vcorner::CornerOp::Chamfer { distance1, distance2 })? {
            return Ok(Some(h));
        }
    }
    // The corner, and which picked segment arrives at it along its direction (A) and which
    // leaves it (B).
    let (x, arrive) = if let Some((i, _)) = adjacent {
        (if i == a { sa.b } else { sb.b }, i)
    } else {
        let x = line_line_infinite(sa.a, sa.b, sb.a, sb.b).map(|r| r.0).ok_or_else(|| other("The segments are parallel."))?;
        match (pick_side(&segs, p1, x)?.1, pick_side(&segs, p2, x)?.1) {
            (true, false) => (x, a),
            (false, true) => (x, b),
            _ => return Err(other("The picks do not meet on one side of the polyline.")),
        }
    };
    let leave = if arrive == a { b } else { a };
    let (s_arr, s_lv) = if arrive == a { (sa, sb) } else { (sb, sa) };
    let side = |keep: Vec2| ((keep - x).normalized(), keep.dist(x));
    // CHAMFER's first distance goes on the first picked segment.
    let c = if arrive == a {
        corner(x, side(s_arr.a), side(s_lv.b), r, chamfer)?
    } else {
        let c = corner(x, side(s_lv.b), side(s_arr.a), r, chamfer)?;
        Corner { t1: c.t2, t2: c.t1, bulge: -c.bulge }
    };
    if !trim {
        return add_loose(s, h, &c);
    }
    // The kept cycle: t2, B's kept end … A's kept end, t1 (the corner's bulge back to t2). An
    // open polyline's gap is segment n - 1; it is kept unless it lies in the removed run.
    let offset = |k: usize| (k + n - arrive) % n;
    let removed = |k: usize| offset(k) > 0 && offset(k) < offset(leave);
    let closed = pl.closed || removed(n - 1);
    let tmpl = |k: usize| pl.vertices.get(k).copied().unwrap_or_default();
    let mut cyc: Vec<(PolyVertex, Option<usize>)> = vec![(PolyVertex { p: c.t2, bulge: 0.0, ..tmpl(leave) }, None)];
    let mut k = (leave + 1) % n;
    loop {
        cyc.push((tmpl(k), Some(k)));
        if k == arrive {
            break;
        }
        k = (k + 1) % n;
    }
    cyc.push((PolyVertex { p: c.t1, bulge: c.bulge, ..tmpl(arrive) }, None));
    // Start at the lowest surviving original vertex: an open result then runs from vertex 0 to
    // vertex n - 1 as before.
    let start = cyc.iter().enumerate().filter_map(|(at, (_, k))| k.map(|k| (k, at))).min().map_or(0, |(_, at)| at);
    cyc.rotate_left(start);
    let mut out: Vec<PolyVertex> = Vec::with_capacity(cyc.len());
    for (v, _) in cyc {
        push_point(&mut out, v);
    }
    if closed && out.len() > 1 && out.first().zip(out.last()).is_some_and(|(f, l)| f.p.near(l.p, 1e-12)) {
        out.pop();
    }
    set_kind(s, h, EntityKind::LwPolyline(LwPolyline { vertices: out, closed, ..pl.clone() }))?;
    Ok(Some(h))
}

/// A polyline's vertices and whether it is closed; a line is an open chain of one segment.
fn chain(kind: &EntityKind) -> Option<(Vec<PolyVertex>, bool)> {
    match kind {
        EntityKind::LwPolyline(p) => Some((p.vertices.clone(), p.closed)),
        EntityKind::Line(l) => Some((vec![PolyVertex::new(l.a.xy()), PolyVertex::new(l.b.xy())], false)),
        _ => None,
    }
}

/// The plane a chain lies in: its elevation and normal. `None` for a line that is not level.
fn plane(e: &Entity) -> Option<(f64, Vec3)> {
    match &e.kind {
        EntityKind::LwPolyline(p) => Some((p.elevation, e.common.extrusion)),
        EntityKind::Line(l) => ((l.a.z - l.b.z).abs() <= EPS).then_some((l.a.z, Vec3::Z)),
        _ => None,
    }
}

/// Two different objects, at least one a polyline, each a line or an open polyline (LAMB fork
/// patch 31). Each pick keeps its chain from the free end through the pick to the kept end of the
/// picked segment; everything past the trimmed end goes. The result is one open polyline from the
/// first pick's free end through the corner, held by the first picked polyline; the other object
/// is removed.
fn two_chains(
    s: &mut Session,
    (h1, e1, p1): (Handle, &Entity, Vec2),
    (h2, e2, p2): (Handle, &Entity, Vec2),
    r: f64,
    chamfer: Option<ChamferDistances>,
    trim: bool,
) -> Result<Option<Handle>> {
    let (Some((v1, c1)), Some((v2, c2))) = (chain(&e1.kind), chain(&e2.kind)) else {
        return Err(other("Select a line or a polyline."));
    };
    let (hp, pl) = match (&e1.kind, &e2.kind) {
        (EntityKind::LwPolyline(p), _) => (h1, p.clone()),
        (_, EntityKind::LwPolyline(p)) => (h2, p.clone()),
        _ => return Err(other("Select a polyline.")),
    };
    if c1 || c2 {
        return Err(other("A closed polyline fillets or chamfers only with another segment of itself."));
    }
    match (plane(e1), plane(e2)) {
        (Some((z1, n1)), Some((z2, n2))) if (z1 - z2).abs() <= EPS && (n1 - n2).len() <= EPS => {}
        _ => return Err(other("The polylines are not in the same plane.")),
    }
    let segs = |v: &[PolyVertex]| Polyline { vertices: v.to_vec(), closed: false }.segments();
    let (s1, s2) = (segs(&v1), segs(&v2));
    let ((_, l1), (_, l2)) = (picked_line(&s1, p1)?, picked_line(&s2, p2)?);
    let x = line_line_infinite(l1.a, l1.b, l2.a, l2.b).map(|r| r.0).ok_or_else(|| other("Lines are parallel."))?;
    let ((k1, f1), (k2, f2)) = (pick_side(&s1, p1, x)?, pick_side(&s2, p2, x)?);
    // The first chain arrives at the corner (its trimmed end is vertex ka + 1); the second leaves
    // it (its trimmed end is vertex kb).
    let (a, ka) = if f1 { (v1, k1) } else { (curves::reverse_vertices(&v1, false), s1.len() - 1 - k1) };
    let (b, kb) = if f2 { (curves::reverse_vertices(&v2, false), s2.len() - 1 - k2) } else { (v2, k2) };
    let (Some(keep1), Some(cut2), Some(keep2)) = (a.get(ka).copied(), b.get(kb).copied(), b.get(kb + 1).copied()) else {
        return Err(other("Select a polyline segment."));
    };
    let side = |keep: Vec2| ((keep - x).normalized(), keep.dist(x));
    let c = corner(x, side(keep1.p), side(keep2.p), r, chamfer)?;
    if !trim {
        return add_loose(s, hp, &c);
    }
    let mut out: Vec<PolyVertex> = Vec::with_capacity(ka + b.len() + 2);
    for v in a.iter().take(ka + 1) {
        push_point(&mut out, *v);
    }
    push_point(&mut out, PolyVertex { p: c.t1, bulge: c.bulge, ..keep1 });
    push_point(&mut out, PolyVertex { p: c.t2, bulge: 0.0, ..cut2 });
    for v in b.iter().skip(kb + 1) {
        push_point(&mut out, *v);
    }
    set_kind(s, hp, EntityKind::LwPolyline(LwPolyline { vertices: out, closed: false, ..pl }))?;
    s.doc_mut()?.remove_entity(if hp == h1 { h2 } else { h1 });
    Ok(Some(hp))
}

/// FILLET (`chamfer` None, radius `r`) or CHAMFER where a polyline is picked: `None` when
/// neither object is a polyline. Returns the polyline that now holds the corner (or, with
/// `trim` false, the separate arc or line).
#[allow(clippy::too_many_arguments)]
pub(crate) fn fillet_pline(
    s: &mut Session,
    h1: Handle,
    p1: Vec2,
    h2: Handle,
    p2: Vec2,
    r: f64,
    chamfer: Option<ChamferDistances>,
    trim: bool,
) -> Result<Option<Option<Handle>>> {
    let (e1, e2) = (curves::entity(s, h1)?, curves::entity(s, h2)?);
    if !matches!(e1.kind, EntityKind::LwPolyline(_)) && !matches!(e2.kind, EntityKind::LwPolyline(_)) {
        return Ok(None);
    }
    if curves::is_locked(s, h1) || curves::is_locked(s, h2) {
        return Err(other("The object is on a locked layer."));
    }
    let made = match &e1.kind {
        EntityKind::LwPolyline(pl) if h1 == h2 => same_polyline(s, h1, pl, (p1, p2), r, chamfer, trim)?,
        _ if h1 != h2 && chain(&e1.kind).is_some() && chain(&e2.kind).is_some() => two_chains(s, (h1, &e1, p1), (h2, &e2, p2), r, chamfer, trim)?,
        _ => return Err(other("A polyline fillets or chamfers with a line, another polyline, or another segment of itself.")),
    };
    Ok(Some(made))
}

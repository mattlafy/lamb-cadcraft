//! Hit testing and window/crossing selection.

use std::borrow::Cow;
use std::collections::HashMap;

use cadcraft_doc::{
    AnnoDraw, Block, Common, Drawing, Entity, EntityKind, HAlign, Handle, Insert, LwPolyline, MAX_BLOCK_DEPTH, MText, Prim, Space, Text, anno_scaled,
    entity_bounds,
};
use cadcraft_geom::{Bounds2, Line, Mat3, Polyline, Segment, Vec2, point_in_polygon};

use crate::spatial::{self, SpatialIndex};
use crate::{Accept, Prompt};

/// Polylines approximating an entity for hit testing, tessellated at `tol`.
pub fn hit_polylines(d: &Drawing, e: &Entity, tol: f64) -> Vec<Vec<Vec2>> {
    match &e.kind {
        // A block reference, dimension or multileader is hit on what it draws and text on its
        // letters, not on the empty rest of their bounding rectangle or of the text's column.
        EntityKind::Insert(_) | EntityKind::Dimension(_) | EntityKind::MLeader(_) => Drawn::of(d, e, tol, None).polylines(),
        EntityKind::Text(_) | EntityKind::AttDef(_) | EntityKind::MText(_) => letters_of(d, e).polylines(),
        EntityKind::Table(_) | EntityKind::Tolerance(_) | EntityKind::Image(_) => {
            // Tables, tolerances and images are hit inside their box.
            let b = entity_bounds(d, e, 0);
            if b.is_empty() {
                return Vec::new();
            }
            let c = b.corners();
            vec![vec![c[0], c[1], c[2], c[3], c[0]]]
        }
        kind => {
            let mut out: Vec<Vec<Vec2>> = kind.prims().into_iter().map(|p| prim_points(p, tol)).collect();
            // A pattern hatch is also picked on its drawn pattern strokes, not only its boundary.
            if let EntityKind::Hatch(h) = kind
                && !h.solid
                && h.gradient.is_none()
                && !h.pattern.eq_ignore_ascii_case("SOLID")
            {
                let list = cadcraft_render::build_entities(
                    d,
                    std::iter::once(e),
                    &cadcraft_render::Options { tolerance: tol, text: false, fill: false, ..Default::default() },
                );
                out.extend(list.prims.iter().map(|p| list.points(p).to_vec()));
            }
            out
        }
    }
}

/// Primitive `p` as a polyline tessellated at `tol` (a fill closed back to its start).
fn prim_points(p: Prim, tol: f64) -> Vec<Vec2> {
    let mut pts = Vec::new();
    match p {
        Prim::Seg(s) => s.tessellate(tol, &mut pts),
        Prim::Circle(c) => c.tessellate(tol, &mut pts),
        Prim::Ellipse(el) => el.tessellate(tol, &mut pts),
        Prim::Spline(s) => pts = s.tessellate(tol),
        Prim::Point(p) => pts = vec![p, p],
        Prim::Infinite { base, dir, ray } => {
            let far = 1e9;
            pts = vec![if ray { base } else { base - dir * far }, base + dir * far];
        }
        Prim::Fill(f) => pts = ring(&f),
    }
    pts
}

fn dist_to_polyline(pts: &[Vec2], p: Vec2) -> f64 {
    if pts.len() == 1 {
        return pts.first().map(|q| q.dist(p)).unwrap_or(f64::INFINITY);
    }
    pts.windows(2).filter_map(|w| Some(Line::new(*w.first()?, *w.get(1)?).dist(p))).fold(f64::INFINITY, f64::min)
}

/// Distance from `p` to the entity (0 inside the letters of an outline font, table and tolerance
/// boxes and solid fills).
pub fn entity_distance(d: &Drawing, e: &Entity, p: Vec2, tol: f64) -> f64 {
    match &e.kind {
        EntityKind::Insert(_) | EntityKind::Dimension(_) | EntityKind::MLeader(_) => return Drawn::of(d, e, tol, None).distance(p),
        EntityKind::Text(_) | EntityKind::AttDef(_) | EntityKind::MText(_) => return letters_of(d, e).distance(p),
        _ => {}
    }
    let polys = hit_polylines(d, e, tol);
    let mut best = polys.iter().map(|pl| dist_to_polyline(pl, p)).fold(f64::INFINITY, f64::min);
    let filled = matches!(e.kind, EntityKind::Table(_) | EntityKind::Tolerance(_));
    if filled && entity_bounds(d, e, 0).contains(p) {
        best = 0.0;
    }
    // A solid hatch fills its loops with odd parity under its island style, as it is drawn, not
    // its bounding rectangle.
    if let EntityKind::Hatch(h) = &e.kind
        && h.solid
        && cadcraft_render::hatch_fill_loops(h, tol).iter().filter(|l| point_in_polygon(l, p)).count() % 2 == 1
    {
        best = 0.0;
    }
    // A SOLID fills its polygon, not its bounding rectangle (half of which is empty for a triangle).
    if matches!(e.kind, EntityKind::Solid(_)) && e.kind.prims().iter().any(|prim| matches!(prim, Prim::Fill(poly) if point_in_polygon(poly, p))) {
        best = 0.0;
    }
    best
}

fn selectable(d: &Drawing, e: &Entity) -> bool {
    d.is_visible(e)
}

/// Entities to test, in the order to test them, each with its bounds when already known.
pub(crate) type Cands<'a> = Box<dyn Iterator<Item = (&'a Entity, Option<Bounds2>)> + 'a>;

pub(crate) fn bounds_of(d: &Drawing, e: &Entity, known: Option<Bounds2>) -> Bounds2 {
    known.unwrap_or_else(|| entity_bounds(d, e, 0))
}

pub(crate) fn is_infinite(e: &Entity) -> bool {
    matches!(e.kind, EntityKind::XLine(_) | EntityKind::Ray(_))
}

/// Candidates from the spatial index touching `query` (draw order, reversed when `rev`), or every
/// entity of the space when it is too small to index (or the query box isn't finite).
pub(crate) fn candidates<'a>(
    d: &'a Drawing,
    space: &Space,
    ix: &'a Option<std::sync::Arc<SpatialIndex>>,
    query: &Bounds2,
    infinite: bool,
    rev: bool,
) -> Option<Cands<'a>> {
    if let Some(ix) = ix
        && let Some(c) = ix.query(query, infinite)
    {
        let it = move |i: u32| ix.entity(i).map(|e| (e.as_ref(), Some(ix.bounds(i))));
        return Some(if rev { Box::new(c.into_iter().rev().filter_map(it)) } else { Box::new(c.into_iter().filter_map(it)) });
    }
    let store = d.space(space)?;
    if rev {
        let ents: Vec<&Entity> = store.iter().map(|e| e.as_ref()).collect();
        Some(Box::new(ents.into_iter().rev().map(|e| (e, None))))
    } else {
        Some(Box::new(store.iter().map(|e| (e.as_ref(), None))))
    }
}

/// The topmost entity within `aperture` of `p`.
pub fn pick(d: &Drawing, space: &Space, p: Vec2, aperture: f64) -> Option<Handle> {
    let ix = spatial::index(d, space);
    pick_with(d, space, &ix, p, aperture)
}

pub(crate) fn pick_with(d: &Drawing, space: &Space, ix: &Option<std::sync::Arc<SpatialIndex>>, p: Vec2, aperture: f64) -> Option<Handle> {
    let probe = Bounds2::new(p, p).expand(aperture);
    let mut best: Option<(f64, Handle)> = None;
    for (e, known) in candidates(d, space, ix, &probe.expand(aperture), true, true)? {
        if !selectable(d, e) {
            continue;
        }
        let infinite = is_infinite(e);
        if !infinite && !bounds_of(d, e, known).expand(aperture).intersects(&probe) {
            continue;
        }
        let dist = match &e.kind {
            // Only the text near the point is laid out.
            EntityKind::Insert(_) | EntityKind::Dimension(_) | EntityKind::MLeader(_) => Drawn::of(d, e, aperture / 4.0, Some(probe)).distance(p),
            _ => entity_distance(d, e, p, aperture / 4.0),
        };
        if dist <= aperture && best.is_none_or(|(bd, _)| dist < bd - 1e-12) {
            best = Some((dist, e.handle));
            if dist == 0.0 {
                break;
            }
        }
    }
    best.map(|(_, h)| h)
}

/// Whether PREVIEWFILTER keeps `e` from being highlighted under the cursor: bits 1 objects on
/// locked layers, 2 xrefs, 4 tables, 8 multiline text, 16 hatches, 32 objects in groups.
pub fn preview_filtered(d: &Drawing, e: &Entity, filter: u8) -> bool {
    let on = |bit: u8| filter & bit != 0;
    (on(1) && d.layer(&e.common.layer).is_some_and(|l| l.locked))
        || (on(2) && matches!(&e.kind, EntityKind::Insert(i) if d.block(&i.block).is_some_and(|b| b.xref_path.is_some())))
        || (on(4) && matches!(e.kind, EntityKind::Table(_)))
        || (on(8) && matches!(e.kind, EntityKind::MText(_)))
        || (on(16) && matches!(e.kind, EntityKind::Hatch(_)))
        || (on(32) && d.groups.iter().any(|g| g.members.contains(&e.handle)))
}

fn seg_hits_box(a: Vec2, b: Vec2, bx: &Bounds2) -> bool {
    if bx.contains(a) || bx.contains(b) {
        return true;
    }
    let c = bx.corners();
    (0..4).any(|i| cadcraft_geom::line_line(&Line::new(a, b), &Line::new(c[i], c[(i + 1) % 4])).is_some())
}

/// Window selection (entirely inside) or crossing selection (inside or touching).
pub fn select_window(d: &Drawing, space: &Space, bx: Bounds2, crossing: bool) -> Vec<Handle> {
    let ix = spatial::index(d, space);
    select_window_with(d, space, &ix, bx, crossing)
}

pub(crate) fn select_window_with(d: &Drawing, space: &Space, ix: &Option<std::sync::Arc<SpatialIndex>>, bx: Bounds2, crossing: bool) -> Vec<Handle> {
    let Some(cands) = candidates(d, space, ix, &bx, crossing, false) else { return Vec::new() };
    let tol = window_tol(&bx);
    cands.filter(|(e, known)| selectable(d, e) && in_window(d, e, *known, &bx, crossing, tol)).map(|(e, _)| e.handle).collect()
}

/// The tessellation tolerance window and crossing tests use for a box.
pub(crate) fn window_tol(bx: &Bounds2) -> f64 {
    (bx.width() + bx.height()).max(1e-9) / 2000.0
}

/// Whether a window box (`crossing` false: entirely inside) or a crossing box (inside or
/// touching its geometry, not just its bounding box) selects `e`.
pub(crate) fn in_window(d: &Drawing, e: &Entity, known: Option<Bounds2>, bx: &Bounds2, crossing: bool, tol: f64) -> bool {
    let infinite = is_infinite(e);
    let eb = bounds_of(d, e, known);
    if !infinite && bx.contains_box(&eb) {
        return true;
    }
    // Text is selected on its letters: by a window around them all (not necessarily around the
    // whole column of an MTEXT), and by a crossing box that touches them, not one between them.
    if matches!(e.kind, EntityKind::Text(_) | EntityKind::AttDef(_) | EntityKind::MText(_)) {
        if !eb.intersects(bx) {
            return false;
        }
        let l = letters_of(d, e);
        return l.within(bx) || (crossing && l.crossed_by(bx));
    }
    if !crossing || (!infinite && !eb.intersects(bx)) {
        return false;
    }
    // A crossing box selects a block reference, dimension or multileader across what it draws,
    // not inside the empty rest of its bounding rectangle.
    if matches!(e.kind, EntityKind::Insert(_) | EntityKind::Dimension(_) | EntityKind::MLeader(_)) {
        return Drawn::of(d, e, tol, Some(*bx)).crossed_by(bx);
    }
    let polys = hit_polylines(d, e, tol);
    let hit = polys.iter().any(|pl| {
        if pl.len() == 1 {
            pl.first().is_some_and(|p| bx.contains(*p))
        } else {
            pl.windows(2).any(|w| w.first().zip(w.get(1)).is_some_and(|(a, b)| seg_hits_box(*a, *b, bx)))
        }
    });
    if hit {
        return true;
    }
    // SOLID/TRACE are filled polygons. A crossing window wholly within the
    // fill must select them even when it does not touch their outline. Check
    // the polygon, not just its bounding rectangle (which can cover empty
    // space around triangular solids).
    if matches!(e.kind, EntityKind::Solid(_) | EntityKind::Trace(_)) && eb.contains_box(bx) {
        return e.kind.prims().iter().any(|prim| match prim {
            Prim::Fill(poly) => bx.corners().iter().all(|p| cadcraft_geom::point_in_polygon(poly, *p)),
            _ => false,
        });
    }
    false
}

/// `e` as it is drawn: an annotative object at the current annotation scale; `None` when it is
/// hidden at that scale.
fn as_drawn<'a>(d: &Drawing, e: &'a Entity) -> Option<Cow<'a, Entity>> {
    match d.anno_draw(e) {
        AnnoDraw::Plain => Some(Cow::Borrowed(e)),
        AnnoDraw::Scaled(f) => Some(Cow::Owned(anno_scaled(e, f))),
        AnnoDraw::Hidden => None,
    }
}

/// `c` as a closed polyline.
fn closed(c: &[Vec2; 4]) -> Vec<Vec2> {
    vec![c[0], c[1], c[2], c[3], c[0]]
}

/// `c` as a polyline that ends where it starts.
fn ring(c: &[Vec2]) -> Vec<Vec2> {
    let mut v = c.to_vec();
    if let (Some(first), Some(last)) = (c.first(), c.last())
        && first != last
    {
        v.push(*first);
    }
    v
}

/// Whether polyline `pl` (a single point: a dot) touches box `bx`.
fn polyline_meets_box(pl: &[Vec2], bx: &Bounds2) -> bool {
    match pl {
        [q] => bx.contains(*q),
        pl => pl.windows(2).any(|w| w.first().zip(w.get(1)).is_some_and(|(a, b)| seg_hits_box(*a, *b, bx))),
    }
}

/// Whether box `bx` touches the area inside the closed polygon `poly`: crosses its edges, holds a
/// corner of it, or lies inside it.
fn area_meets_box(poly: &[Vec2], bx: &Bounds2) -> bool {
    let n = poly.len();
    (0..n).any(|i| poly.get(i).zip(poly.get((i + 1) % n)).is_some_and(|(a, b)| seg_hits_box(*a, *b, bx))) || point_in_polygon(poly, bx.center())
}

/// Where the area that triangles `tris` (three points each) fill meets the outside, within `near`
/// (everywhere without it): the pieces of their edges with no fill on one side, holes included.
/// An edge two triangles share, or a part of one that another triangle covers, is inside the area
/// however the triangles are cut — render's strips meet at T-junctions, its band segments overlap
/// where they join — and a gap narrower than a hair between them isn't an edge.
fn fill_outline(tris: &[Vec2], near: Option<Bounds2>) -> Vec<Vec<Vec2>> {
    let mut ts: Vec<[Vec2; 3]> = tris
        .as_chunks::<3>()
        .0
        .iter()
        .copied()
        .filter(|[a, b, c]| [a, b, c].iter().all(|q| q.is_finite()) && (*b - *a).cross(*c - *a) != 0.0)
        .collect();
    let all = Bounds2::from_points(ts.iter().flatten().copied());
    if all.is_empty() {
        return Vec::new();
    }
    // A hair: far below any drawn detail, far above rounding.
    let size = all.width().max(all.height());
    let far = all.min.x.abs().max(all.max.x.abs()).max(all.min.y.abs()).max(all.max.y.abs());
    let hair = (size * 1e-7).max(far * 1e-12).max(f64::MIN_POSITIVE);
    // Spread so far that squared lengths overflow (only a hostile file): each triangle's edges,
    // as they are.
    if !(size < 1e150 && far < 1e150) {
        return ts.iter().map(|t| vec![t[0], t[1], t[2], t[0]]).collect();
    }
    let look = near.map(|b| b.expand(hair * 4.0));
    if let Some(l) = look {
        ts.retain(|t| Bounds2::from_points(t.iter().copied()).intersects(&l));
    }
    // An edge two triangles share exactly, one on each side, is inside: most of them, found at once.
    let key = |a: Vec2, b: Vec2| {
        let (a, b) = ((a.x.to_bits(), a.y.to_bits()), (b.x.to_bits(), b.y.to_bits()));
        if a <= b { (a, b) } else { (b, a) }
    };
    let mut sides: HashMap<_, (f64, f64)> = HashMap::new();
    for [a, b, c] in &ts {
        for (p, q, r) in [(*a, *b, *c), (*b, *c, *a), (*c, *a, *b)] {
            // The side of the third corner, measured from the edge's lower end.
            let (lo, hi) = if key(p, q).0 == (p.x.to_bits(), p.y.to_bits()) { (p, q) } else { (q, p) };
            let side = (hi - lo).cross(r - lo).signum();
            let e = sides.entry(key(p, q)).or_insert((0.0, 0.0));
            if side > 0.0 { e.0 += 1.0 } else { e.1 += 1.0 }
        }
    }
    let inside = |p: Vec2, q: Vec2| sides.get(&key(p, q)).is_some_and(|(l, r)| *l >= 1.0 && *r >= 1.0);
    let grid = TriGrid::new(&ts);
    let mut seen = vec![u64::MAX; ts.len()];
    let mut query = 0u64;
    // The work allowed (triangles looked at, and tried against each piece of an edge): some four
    // times what the busiest real fills take (a slab with thousands of round columns, about 620 a
    // triangle). Past it, as when triangles pile up by the thousand (a wide polyline crossing
    // itself over and over), the edges left are kept whole without looking around them.
    let mut budget = ts.len().saturating_mul(2048).max(20_000_000);
    let mut out: Vec<Vec<Vec2>> = Vec::new();
    for t in &ts {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            if inside(a, b) {
                continue;
            }
            let d = b - a;
            let Some((t0, t1)) = clip_to(a, b, look) else { continue };
            if budget == 0 {
                out.push(vec![a + d * t0, a + d * t1]);
                continue;
            }
            let n = d.perp() * (hair / d.len());
            query += 1;
            let (found, scanned) = grid.near(&Bounds2::new(a + d * t0, a + d * t1).expand(hair * 2.0), &mut seen, query);
            budget = budget.saturating_sub(scanned.saturating_mul(10));
            let around: Vec<&[Vec2; 3]> = found.into_iter().filter_map(|i| ts.get(i)).collect();
            // Where the edges of the triangles around it cross or touch it, its side may change.
            let mut cuts = vec![t0, t1];
            for o in &around {
                for (p, q) in [(o[0], o[1]), (o[1], o[2]), (o[2], o[0])] {
                    if let Some((_, s, u)) = cadcraft_geom::line_line_infinite(a, b, p, q)
                        && (-1e-9..=1.0 + 1e-9).contains(&u)
                    {
                        cuts.push(s);
                    }
                    for v in [p, q] {
                        if Line::new(a, b).dist(v) <= hair {
                            cuts.push((v - a).dot(d) / d.len2());
                        }
                    }
                }
            }
            cuts.retain(|s| s.is_finite() && *s >= t0 && *s <= t1);
            cuts.sort_by(f64::total_cmp);
            cuts.dedup();
            let work = around.len().saturating_mul(cuts.len());
            if work > budget {
                budget = 0;
                out.push(vec![a + d * t0, a + d * t1]);
                continue;
            }
            budget -= work;
            let filled = |q: Vec2| around.iter().any(|o| in_triangle(o, q));
            for w in cuts.windows(2) {
                let (Some(&s0), Some(&s1)) = (w.first(), w.get(1)) else { continue };
                let m = a + d * ((s0 + s1) / 2.0);
                if filled(m + n) && filled(m - n) {
                    continue;
                }
                let (p, q) = (a + d * s0, a + d * s1);
                // One piece on from the last one, the same way, extends it.
                match out.last_mut() {
                    Some(last)
                        if last.len() == 2
                            && last.get(1) == Some(&p)
                            && (q - p).dot(p - last[0]) > 0.0
                            && (q - p).cross(p - last[0]).abs() <= hair * (q - p).len() =>
                    {
                        if let Some(end) = last.get_mut(1) {
                            *end = q;
                        }
                    }
                    _ => out.push(vec![p, q]),
                }
            }
        }
    }
    out
}

/// Whether `q` is in triangle `t` or on its edges.
fn in_triangle(t: &[Vec2; 3], q: Vec2) -> bool {
    let [a, b, c] = *t;
    let (u, v, w) = ((b - a).cross(q - a), (c - b).cross(q - b), (a - c).cross(q - c));
    (u >= 0.0 && v >= 0.0 && w >= 0.0) || (u <= 0.0 && v <= 0.0 && w <= 0.0)
}

/// The part of segment `a`–`b` inside box `bx` (all of it without one), as parameters along it.
fn clip_to(a: Vec2, b: Vec2, bx: Option<Bounds2>) -> Option<(f64, f64)> {
    let Some(bx) = bx else { return Some((0.0, 1.0)) };
    let d = b - a;
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-d.x, a.x - bx.min.x), (d.x, bx.max.x - a.x), (-d.y, a.y - bx.min.y), (d.y, bx.max.y - a.y)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
        }
    }
    (t0 < t1).then_some((t0, t1))
}

/// Triangles filed by the cells of a grid over them, to find those near a box. The cells take the
/// average shape of the triangles (render fills areas with long flat strips), and a triangle over
/// a great many cells is kept apart and always looked at.
struct TriGrid {
    min: Vec2,
    cell: Vec2,
    cols: i64,
    rows: i64,
    cells: HashMap<(i64, i64), Vec<usize>>,
    big: Vec<usize>,
}

impl TriGrid {
    /// Most cells a triangle is filed under.
    const MAX_CELLS: i64 = 4096;

    fn new(ts: &[[Vec2; 3]]) -> TriGrid {
        let all = Bounds2::from_points(ts.iter().flatten().copied());
        let n = ts.len().max(1) as f64;
        let mean =
            ts.iter().map(|t| Bounds2::from_points(t.iter().copied())).fold(Vec2::ZERO, |m, b| m + Vec2::new(b.width(), b.height())) * (1.0 / n);
        // At least the average triangle, and no more cells than four per triangle.
        let mut cell = Vec2::new(mean.x.max(all.width() / 4096.0), mean.y.max(all.height() / 4096.0));
        let count = (all.width() / cell.x).max(1.0) * (all.height() / cell.y).max(1.0);
        if count.is_nan() || count > 4.0 * n {
            cell = cell * (count / (4.0 * n)).sqrt();
        }
        let cell = Vec2::new(cell.x.max(f64::MIN_POSITIVE), cell.y.max(f64::MIN_POSITIVE));
        let span = |len: f64, c: f64| {
            let n = (len / c).ceil();
            if n.is_finite() { n.clamp(1.0, 1e6) as i64 } else { 1 }
        };
        let mut g =
            TriGrid { min: all.min, cell, cols: span(all.width(), cell.x), rows: span(all.height(), cell.y), cells: HashMap::new(), big: Vec::new() };
        for (i, t) in ts.iter().enumerate() {
            let ((c0, r0), (c1, r1)) = g.span(&Bounds2::from_points(t.iter().copied()));
            if (c1 - c0 + 1) * (r1 - r0 + 1) > Self::MAX_CELLS {
                g.big.push(i);
                continue;
            }
            for c in c0..=c1 {
                for r in r0..=r1 {
                    g.cells.entry((c, r)).or_default().push(i);
                }
            }
        }
        g
    }

    /// The first and last cells box `b` lies over, kept to the grid.
    fn span(&self, b: &Bounds2) -> ((i64, i64), (i64, i64)) {
        let at = |v: f64, lo: f64, c: f64, n: i64| {
            let k = ((v - lo) / c).floor();
            if k.is_nan() { 0 } else { k.clamp(0.0, (n - 1) as f64) as i64 }
        };
        let (x, y) = (self.cell.x, self.cell.y);
        (
            (at(b.min.x, self.min.x, x, self.cols), at(b.min.y, self.min.y, y, self.rows)),
            (at(b.max.x, self.min.x, x, self.cols), at(b.max.y, self.min.y, y, self.rows)),
        )
    }

    /// The triangles that may meet box `b`, each once (`seen` holds, for each, the last `stamp`
    /// it was found under), and how many entries were looked at to find them.
    fn near(&self, b: &Bounds2, seen: &mut [u64], stamp: u64) -> (Vec<usize>, usize) {
        let ((c0, r0), (c1, r1)) = self.span(b);
        let mut v = Vec::new();
        // A box over more cells than there are filed: every triangle.
        let all: Vec<usize>;
        let found: Box<dyn Iterator<Item = &usize>> =
            if (c1 - c0 + 1).saturating_mul(r1 - r0 + 1) > (seen.len() as i64).saturating_mul(4).saturating_add(64) {
                all = (0..seen.len()).collect();
                Box::new(all.iter())
            } else {
                let cells = (c0..=c1).flat_map(move |c| (r0..=r1).map(move |r| (c, r)));
                Box::new(self.big.iter().chain(cells.flat_map(|k| self.cells.get(&k).into_iter().flatten())))
            };
        let mut scanned = 0usize;
        for i in found {
            scanned += 1;
            if let Some(s) = seen.get_mut(*i)
                && *s != stamp
            {
                *s = stamp;
                v.push(*i);
            }
        }
        (v, scanned)
    }
}

/// The letters a text draws, where it is picked and selected: on the strokes of a font drawn
/// with lines and inside each glyph of a filled outline font (its holes, as in an "O", left out by
/// odd parity). Not between the letters, nor across the rest of an MTEXT's column.
#[derive(Default)]
struct Letters {
    strokes: Vec<Vec<Vec2>>,
    glyphs: Vec<Vec<Vec<Vec2>>>,
}

impl Letters {
    /// Add shaped text placed by `mm`. With `TEXTFILL` 0 outline glyphs are drawn, and picked, on
    /// their outline only.
    fn push(&mut self, d: &Drawing, strokes: Vec<Vec<Vec2>>, glyphs: Vec<Vec<Vec<Vec2>>>, mm: &Mat3) {
        let place = |pl: Vec<Vec2>| pl.into_iter().map(|q| mm.apply(q)).collect::<Vec<_>>();
        self.strokes.extend(strokes.into_iter().map(place));
        let glyphs = glyphs.into_iter().map(|g| g.into_iter().map(place).collect::<Vec<_>>());
        if d.header.i64("TEXTFILL", 1) == 0 {
            self.strokes.extend(glyphs.flatten().map(|c| ring(&c)));
        } else {
            self.glyphs.extend(glyphs);
        }
    }

    /// Add a TEXT, attribute or attribute definition showing `value`, placed by `mm`.
    fn push_text(&mut self, d: &Drawing, t: &Text, value: &str, mm: &Mat3) {
        let (sh, _) = cadcraft_render::place_text_entity(d, t, value);
        self.push(d, sh.strokes, sh.glyphs, mm);
    }

    /// Add an MTEXT, placed by `mm`.
    fn push_mtext(&mut self, d: &Drawing, t: &MText, mm: &Mat3) {
        for p in cadcraft_render::layout_mtext_entity(d, t).pieces {
            self.push(d, p.shaped.strokes, p.shaped.glyphs, mm);
        }
    }

    fn is_empty(&self) -> bool {
        self.strokes.is_empty() && self.glyphs.is_empty()
    }

    /// Whether `p` is inside a filled glyph.
    fn inside(&self, p: Vec2) -> bool {
        self.glyphs.iter().any(|g| g.iter().filter(|c| point_in_polygon(c, p)).count() % 2 == 1)
    }

    /// Distance from `p` to the letters: 0 inside a filled glyph.
    fn distance(&self, p: Vec2) -> f64 {
        if self.inside(p) {
            return 0.0;
        }
        let strokes = self.strokes.iter().map(|s| dist_to_polyline(s, p));
        let outlines = self.glyphs.iter().flatten().map(|c| dist_to_polyline(&ring(c), p));
        strokes.chain(outlines).fold(f64::INFINITY, f64::min)
    }

    /// Whether a crossing box touches the letters.
    fn crossed_by(&self, bx: &Bounds2) -> bool {
        self.strokes.iter().any(|s| polyline_meets_box(s, bx))
            || self.glyphs.iter().flatten().any(|c| polyline_meets_box(&ring(c), bx))
            || self.inside(bx.center())
    }

    /// Whether every letter lies inside box `bx` (never, when there are none).
    fn within(&self, bx: &Bounds2) -> bool {
        !self.is_empty() && self.strokes.iter().chain(self.glyphs.iter().flatten()).flatten().all(|q| bx.contains(*q))
    }

    /// The strokes and the closed glyph outlines.
    fn polylines(self) -> Vec<Vec<Vec2>> {
        let mut v = self.strokes;
        v.extend(self.glyphs.iter().flatten().map(|c| ring(c)));
        v
    }
}

/// The letters of a top-level TEXT, attribute definition or MTEXT, as it is drawn (none for other
/// objects, or when it is hidden at the annotation scale).
fn letters_of(d: &Drawing, e: &Entity) -> Letters {
    let mut l = Letters::default();
    match as_drawn(d, e).as_deref().map(|e| &e.kind) {
        Some(EntityKind::Text(t)) => l.push_text(d, t, &t.value, &Mat3::IDENTITY),
        Some(EntityKind::AttDef(a)) => l.push_text(d, &a.text, &a.tag, &Mat3::IDENTITY),
        Some(EntityKind::MText(t)) => l.push_mtext(d, t, &Mat3::IDENTITY),
        _ => {}
    }
    l
}

/// Whether `c` is a plain character, which stays within four heights: not a control character
/// (a tab among them) nor one past Latin Extended-B.
fn plain_char(c: char) -> bool {
    !c.is_control() && c <= '\u{24F}'
}

/// Whether MTEXT `contents` is plain: plain characters (see [`plain_char`]), and only the
/// formatting codes that neither move nor enlarge the words — paragraph and column breaks, hard
/// spaces, escaped characters, underline, overline, strike-through, colour, alignment and font.
/// Height, width, slant, tracking, stacking and paragraph (indent, tab) codes are not.
fn plain_mtext(contents: &str) -> bool {
    let mut chars = contents.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if !matches!(chars.next(), Some('P' | 'N' | '~' | '\\' | '{' | '}' | 'L' | 'l' | 'O' | 'o' | 'K' | 'k' | 'C' | 'c' | 'A' | 'f' | 'F')) {
                return false;
            }
        } else if !plain_char(c) {
            return false;
        }
    }
    true
}

/// What a block reference, dimension or multileader draws, in world coordinates, as it is picked
/// and selected: its lines and curves, the outline of its fills (not their inside: a fill in a
/// block is picked on its edges, a SOLID and each straight segment of a wide polyline also on the
/// diagonal they are filled across) and the letters of the text it shows.
#[derive(Default)]
struct Drawn {
    lines: Vec<Vec<Vec2>>,
    letters: Letters,
    /// The bounding rectangle of one that isn't drawn (see [`Drawn::of`]).
    boxes: Vec<[Vec2; 4]>,
}

impl Drawn {
    /// What `e` draws; with `near`, only the text that may come within that box is laid out.
    fn of(d: &Drawing, e: &Entity, tol: f64, near: Option<Bounds2>) -> Drawn {
        let mut out = Drawn::default();
        let Some(drawn) = as_drawn(d, e) else { return out };
        // One that isn't drawn (hidden, or on a layer that is off or frozen) is never picked, but
        // queries that look past visibility (the entities inquiry) still find it by its bounding
        // rectangle, as before.
        if !d.is_visible(e) {
            let b = entity_bounds(d, e, 0);
            if !b.is_empty() {
                out.boxes.push(b.corners());
            }
            return out;
        }
        // Lines whole, across the gaps of their linetype, as they are picked on their own.
        let opts = cadcraft_render::Options { tolerance: tol, text: false, linetypes: false, ..Default::default() };
        let list = cadcraft_render::build_entities(d, std::iter::once(e), &opts);
        for p in &list.prims {
            let pts = list.points(p);
            match p.kind {
                cadcraft_render::Kind::Tris => out.lines.extend(fill_outline(pts, near)),
                // A wipeout only hides what is under it; its frame, when shown, is a polyline.
                cadcraft_render::Kind::Mask => {}
                cadcraft_render::Kind::Infinite { ray } => {
                    if let [base, dir] = pts {
                        let far = 1e9;
                        out.lines.push(vec![if ray { *base } else { *base - *dir * far }, *base + *dir * far]);
                    }
                }
                cadcraft_render::Kind::Polyline | cadcraft_render::Kind::Point => out.lines.push(pts.to_vec()),
            }
        }
        // As many objects as drawing it expands.
        let mut w = Walk { budget: cadcraft_render::MAX_BLOCK_EXPANSION, near, letters: Letters::default(), lines: Vec::new() };
        parts_of(d, &drawn, &Mat3::IDENTITY, 0, &mut w);
        out.letters = w.letters;
        out.lines.extend(w.lines);
        out
    }

    /// Its lines, the outlines of its letters and its bounding rectangle when it isn't drawn.
    fn polylines(self) -> Vec<Vec<Vec2>> {
        let mut v = self.lines;
        v.extend(self.letters.polylines());
        v.extend(self.boxes.iter().map(closed));
        v
    }

    /// Distance from `p` to what it draws: 0 inside a filled glyph.
    fn distance(self, p: Vec2) -> f64 {
        if self.boxes.iter().any(|b| point_in_polygon(b, p)) {
            return 0.0;
        }
        let letters = self.letters.distance(p);
        self.lines.iter().map(|pl| dist_to_polyline(pl, p)).fold(letters, f64::min)
    }

    /// Whether a crossing box touches what it draws.
    fn crossed_by(self, bx: &Bounds2) -> bool {
        self.lines.iter().any(|pl| polyline_meets_box(pl, bx)) || self.letters.crossed_by(bx) || self.boxes.iter().any(|b| area_meets_box(b, bx))
    }
}

/// What a block reference at nesting `depth` draws besides its rendered lines, placed by `m` into
/// world coordinates: the letters of its attributes as `ATTMODE` shows them, of its TEXT and
/// MTEXT, of its dimensions and multileaders and of those of the references nested in it, and the
/// diagonal of its filled SOLIDs and of the straight segments of its wide polylines, within
/// [`MAX_BLOCK_DEPTH`] levels and the walk's budget. Hidden
/// objects, objects on a layer that is off and attribute definitions are left out, as they are
/// when drawn.
fn block_parts(d: &Drawing, ins: &Insert, m: &Mat3, depth: usize, w: &mut Walk) {
    if depth >= MAX_BLOCK_DEPTH {
        return;
    }
    let blk = d.block(&ins.block);
    // ATTMODE: 0 shows no attributes, 1 the visible ones, 2 all of them.
    let attmode = d.header.i64("ATTMODE", 1);
    if attmode != 0 && !ins.attribs.is_empty() {
        // Each tag's definition (the first one, as when drawn), looked up once per reference.
        let mut defs: HashMap<String, &Common> = HashMap::new();
        for be in blk.iter().flat_map(|b| b.entities.iter()) {
            if w.budget == 0 {
                return;
            }
            w.budget -= 1;
            if let EntityKind::AttDef(a) = &be.kind {
                defs.entry(a.tag.to_ascii_lowercase()).or_insert(&be.common);
            }
        }
        for a in &ins.attribs {
            if w.budget == 0 {
                return;
            }
            w.budget -= 1;
            if a.invisible && attmode != 2 {
                continue;
            }
            // Its own properties over its definition's; layer 0 takes the reference's, which shows.
            let c = match (defs.get(&a.tag.to_ascii_lowercase()), a.props.is_empty()) {
                (None, true) => Common::default(),
                (def, _) => a.props.over(&def.map(|c| (*c).clone()).unwrap_or_default()),
            };
            if !c.visible || (c.layer != "0" && d.layer(&c.layer).is_some_and(|l| !l.visible())) {
                continue;
            }
            if w.may_reach_text(&a.text, &a.text.value, m) {
                w.letters.push_text(d, &a.text, &a.text.value, m);
            }
        }
    }
    let Some(blk) = blk else { return };
    let (cols, rows) = (ins.cols.clamp(1, 10_000), ins.rows.clamp(1, 10_000));
    for r in 0..rows {
        for c in 0..cols {
            if w.budget == 0 {
                return;
            }
            w.budget -= 1;
            let off = Vec2::new(ins.col_spacing * f64::from(c), ins.row_spacing * f64::from(r)).rotate(ins.rotation);
            let mm = m.then_before(Mat3::translate(off).then_before(ins.transform(blk.base.xy())));
            parts_in(d, blk, &mm, depth, w);
        }
    }
}

/// [`block_parts`] for the objects of `blk`, placed by `mm`, inside an object at `depth` (so they
/// are at `depth + 1`).
fn parts_in(d: &Drawing, blk: &Block, mm: &Mat3, depth: usize, w: &mut Walk) {
    for be in blk.entities.iter() {
        if w.budget == 0 {
            return;
        }
        w.budget -= 1;
        // Objects on layer 0 take the reference's layer, which is visible.
        if !be.common.visible || (be.common.layer != "0" && !d.is_visible(be)) {
            continue;
        }
        parts_of(d, be, mm, depth + 1, w);
    }
}

/// The parts of `e` (see [`block_parts`]), itself at nesting `depth`, placed by `mm`.
fn parts_of(d: &Drawing, e: &Entity, mm: &Mat3, depth: usize, w: &mut Walk) {
    match &e.kind {
        EntityKind::Text(t) if w.may_reach_text(t, &t.value, mm) => w.letters.push_text(d, t, &t.value, mm),
        EntityKind::MText(t) if w.may_reach(d, t, mm) => w.letters.push_mtext(d, t, mm),
        // A filled SOLID is filled across its second and third corners, and picked there too.
        EntityKind::Solid(s) if d.header.i64("FILLMODE", 1) != 0 => {
            let [_, b, c, _] = s.corners;
            w.lines.push(vec![mm.apply(b.xy()), mm.apply(c.xy())]);
        }
        // A wide polyline is filled a quad per segment, and each straight one is picked on the
        // sides of its quad and across its diagonal too.
        EntityKind::LwPolyline(p)
            if d.header.i64("FILLMODE", 1) != 0 && (p.const_width > 0.0 || p.vertices.iter().any(|v| v.start_width > 0.0 || v.end_width > 0.0)) =>
        {
            wide_quads(p, mm, w)
        }
        EntityKind::Insert(ins) => block_parts(d, ins, mm, depth, w),
        // A dimension with a block (as AutoCAD saves them) is drawn from it, where its text is;
        // one without (as CADCraft makes them), or too deep to expand, from its own geometry.
        EntityKind::Dimension(dm) => match dm.block.as_ref().and_then(|n| d.block(n)).filter(|_| depth < MAX_BLOCK_DEPTH) {
            Some(b) => parts_in(d, b, mm, depth, w),
            // Always laid out: its rough bounds don't hold its text (a long text of its own, a text
            // style of fixed height, alternate units, tolerances).
            None => {
                let g = cadcraft_render::dimension_in(d, dm);
                w.letters.push(d, g.text, g.text_glyphs, mm);
            }
        },
        // A multileader's block content is drawn at the multileader's own depth.
        EntityKind::MLeader(ml) => {
            if let Some(content) = &ml.block {
                block_parts(d, content, mm, depth, w);
            }
            if let Some(t) = &ml.text {
                w.letters.push_mtext(d, t, mm);
            }
        }
        _ => {}
    }
}

/// What a wide polyline `p` is picked on besides the outline of its band, placed by `mm`, within
/// the walk's box: for each straight segment, the four sides of the quad it is filled as and the
/// diagonal from the right corner of its start to the left corner of its end. Where two straight
/// segments meet (at the closing vertex too) the quad's corners are the mitre of their edges, so
/// the side there is the mitre line; at a free end, next to a curved segment, or when the mitre
/// would run away (turning back on itself) they are square to the segment. A curved segment,
/// filled with a fan of quads, is picked on its edges only.
fn wide_quads(p: &LwPolyline, mm: &Mat3, w: &mut Walk) {
    let pl = Polyline { vertices: p.vertices.clone(), closed: p.closed };
    let segs = pl.segments();
    let n = p.vertices.len();
    // Each straight segment: its line, unit direction and the half widths at its ends, as
    // `render::fill::wide_polyline` takes them.
    let straight: Vec<Option<(Line, Vec2, f64, f64)>> = segs
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let Segment::Line(l) = s else { return None };
            let v = p.vertices.get(i % n.max(1)).copied().unwrap_or_default();
            let (w0, w1) = if p.const_width > 0.0 { (p.const_width, p.const_width) } else { (v.start_width, v.end_width) };
            let d = (l.b - l.a).normalized();
            (d != Vec2::ZERO && d.is_finite() && w0.is_finite() && w1.is_finite()).then_some((*l, d, w0 / 2.0, w1 / 2.0))
        })
        .collect();
    let m = straight.len();
    // The neighbour of segment `i` past its end (or before its start), when straight.
    let next = |i: usize, at_end: bool| -> Option<(Line, Vec2, f64, f64)> {
        let j = if at_end {
            (i + 1 < m).then_some(i + 1).or_else(|| (p.closed && m > 1).then_some(0))
        } else {
            i.checked_sub(1).or_else(|| (p.closed && m > 1).then_some(m - 1))
        };
        j.and_then(|j| straight.get(j).copied().flatten())
    };
    // The corner of segment `i` at its end or start, on its left (`side` 1) or right (-1).
    let corner = |i: usize, at_end: bool, side: f64| -> Vec2 {
        let Some((l, d, h0, h1)) = straight.get(i).copied().flatten() else { return Vec2::new(f64::NAN, f64::NAN) };
        let (v, h) = if at_end { (l.b, h1) } else { (l.a, h0) };
        let square = v + d.perp() * (side * h);
        let Some((_, dj, hj0, hj1)) = next(i, at_end) else { return square };
        let hj = if at_end { hj0 } else { hj1 };
        let edge_j = v + dj.perp() * (side * hj);
        // The mitre: where this segment's edge meets the neighbour's; square when they are
        // parallel or the mitre runs past ten half widths (as the renderer's pen limits a mitre).
        match cadcraft_geom::line_line_infinite(square, square + d, edge_j, edge_j + dj) {
            Some((q, _, _)) if q.is_finite() && (q - v).len() <= 10.0 * h.abs().max(hj.abs()) => q,
            _ => square,
        }
    };
    for i in 0..m {
        if straight.get(i).is_none_or(|s| s.is_none()) {
            continue;
        }
        let (sl, sr) = (corner(i, false, 1.0), corner(i, false, -1.0));
        let (el, er) = (corner(i, true, 1.0), corner(i, true, -1.0));
        let quad = [sl, sr, er, el];
        if quad.iter().all(|q| q.is_finite()) && w.meets(&Bounds2::from_points(quad), mm) {
            w.lines.push(quad.iter().chain(std::iter::once(&sl)).map(|q| mm.apply(*q)).collect());
            w.lines.push(vec![mm.apply(sr), mm.apply(el)]);
        }
    }
}

/// A walk through what a block reference draws: the objects it may still look at, the box looked
/// at (text away from it isn't laid out), and the letters and lines found.
struct Walk {
    budget: usize,
    near: Option<Bounds2>,
    letters: Letters,
    lines: Vec<Vec<Vec2>>,
}

impl Walk {
    /// Whether box `b`, placed by `mm`, meets the box looked at (always, when there is none).
    fn meets(&self, b: &Bounds2, mm: &Mat3) -> bool {
        let Some(near) = self.near else { return true };
        !b.is_empty() && Bounds2::from_points(b.corners().map(|q| mm.apply(q))).intersects(&near)
    }

    /// Whether the letters of TEXT `t` showing `value`, placed by `mm`, may come within the box
    /// looked at, judged without shaping it: plain characters (see [`plain_char`]) of a positive
    /// height, neither aligned nor fit between two points and slanted 45° at most, stay within the
    /// reach of all its characters from the points it is placed by. Any other text is shaped (one
    /// of no positive height is drawn one unit high).
    fn may_reach_text(&self, t: &Text, value: &str, mm: &Mat3) -> bool {
        if self.near.is_none()
            || t.height.is_nan()
            || t.height <= 0.0
            || matches!(t.halign, HAlign::Aligned | HAlign::Fit)
            || !value.chars().all(plain_char)
            || t.oblique.is_nan()
            || t.oblique.abs() > std::f64::consts::FRAC_PI_4
        {
            return true;
        }
        let n = value.chars().count() as f64 + 1.0;
        let reach = n * t.height * 4.0 * t.width_factor.abs().max(1.0);
        if !(reach.is_finite() && reach > 0.0) {
            return true;
        }
        self.meets(&Bounds2::new(t.insert.xy(), t.justify_point()).expand(reach), mm)
    }

    /// Whether the words of MTEXT `t`, placed by `mm`, may come within the box looked at, judged
    /// without laying it out: plain text (see [`plain_mtext`]) in a style slanted 45° at most
    /// stays within the reach of all its characters, whether set on one line or wrapped one per
    /// line, from its insertion point. Any other MTEXT is laid out.
    fn may_reach(&self, d: &Drawing, t: &MText, mm: &Mat3) -> bool {
        let style = d.text_style(&t.style);
        if self.near.is_none()
            || t.columns.is_some()
            || !plain_mtext(&t.contents)
            || style.is_some_and(|s| s.oblique.is_nan() || s.oblique.abs() > std::f64::consts::FRAC_PI_4)
        {
            return true;
        }
        let wf = style.map_or(1.0, |s| s.width_factor.abs()).max(1.0);
        let ls = if t.line_spacing.is_finite() { t.line_spacing.max(1.0) } else { 1.0 };
        // Across: its column, or a word running past it, each character at most four times the
        // height wide. Down: a line per character at most, each 5/3 of the height apart.
        let n = t.contents.chars().count() as f64 + 1.0;
        let reach = n * t.height * 4.0 * wf * ls + t.width.abs();
        if !(reach.is_finite() && t.height > 0.0) {
            return true;
        }
        let o = t.insert.xy();
        self.meets(&Bounds2::new(o, o).expand(reach), mm)
    }
}

/// Fence selection: entities crossed by the fence polyline.
pub fn select_fence(d: &Drawing, space: &Space, fence: &[Vec2]) -> Vec<Handle> {
    let ix = spatial::index(d, space);
    select_fence_with(d, space, &ix, fence)
}

pub(crate) fn select_fence_with(d: &Drawing, space: &Space, ix: &Option<std::sync::Arc<SpatialIndex>>, fence: &[Vec2]) -> Vec<Handle> {
    let fb = Bounds2::from_points(fence.iter().copied());
    let tol = (fb.width() + fb.height()).max(1e-9) / 2000.0;
    let probe = fb.expand(tol);
    let Some(cands) = candidates(d, space, ix, &probe, true, false) else { return Vec::new() };
    let mut out = Vec::new();
    for (e, known) in cands {
        // Infinite objects have no useful bounds; like window selection, test their geometry directly.
        if !selectable(d, e) || (!is_infinite(e) && !bounds_of(d, e, known).intersects(&probe)) {
            continue;
        }
        let polys = hit_polylines(d, e, tol);
        let hit = fence.windows(2).any(|f| {
            let (Some(a), Some(b)) = (f.first(), f.get(1)) else { return false };
            let fl = Line::new(*a, *b);
            polys.iter().any(|pl| {
                pl.windows(2).any(|w| w.first().zip(w.get(1)).is_some_and(|(p, q)| cadcraft_geom::line_line(&fl, &Line::new(*p, *q)).is_some()))
            })
        });
        if hit {
            out.push(e.handle);
        }
    }
    out
}

/// Window polygon selection (entities entirely inside `poly`) or crossing polygon selection
/// (inside or crossing its edges). The polygon closes back to its first point.
pub fn select_polygon(d: &Drawing, space: &Space, poly: &[Vec2], crossing: bool) -> Vec<Handle> {
    let (Some(first), true) = (poly.first(), poly.len() >= 3) else { return Vec::new() };
    let ix = spatial::index(d, space);
    let pb = Bounds2::from_points(poly.iter().copied());
    let tol = (pb.width() + pb.height()).max(1e-9) / 2000.0;
    let Some(cands) = candidates(d, space, &ix, &pb, crossing, false) else { return Vec::new() };
    let edges: Vec<Line> =
        poly.windows(2).filter_map(|w| Some(Line::new(*w.first()?, *w.get(1)?))).chain(poly.last().map(|l| Line::new(*l, *first))).collect();
    let mut out = Vec::new();
    for (e, known) in cands {
        let infinite = is_infinite(e);
        if !selectable(d, e) || (infinite && !crossing) {
            continue;
        }
        let eb = bounds_of(d, e, known);
        if !infinite && if crossing { !eb.intersects(&pb) } else { !pb.contains_box(&eb) } {
            continue;
        }
        let polys = hit_polylines(d, e, tol);
        let crosses = polys.iter().any(|pl| {
            pl.windows(2).any(|w| {
                w.first().zip(w.get(1)).is_some_and(|(p, q)| edges.iter().any(|f| cadcraft_geom::line_line(f, &Line::new(*p, *q)).is_some()))
            })
        });
        let mut pts = polys.iter().flatten();
        let hit = if crossing {
            crosses || pts.any(|p| point_in_polygon(poly, *p))
        } else {
            !crosses && !polys.is_empty() && pts.all(|p| point_in_polygon(poly, *p))
        };
        if hit {
            out.push(e.handle);
        }
    }
    out
}

/// The visible objects in `space` of the group called `name` (any case), or `None` when the
/// drawing has no such group.
pub fn select_group(d: &Drawing, space: &Space, name: &str) -> Option<Vec<Handle>> {
    let g = d.groups.iter().find(|g| g.name.eq_ignore_ascii_case(name.trim()))?;
    let store = d.space(space)?;
    Some(g.members.iter().copied().filter(|h| store.get(*h).is_some_and(|e| d.is_visible(e))).collect())
}

/// A selection mode typed at "Select objects", collecting its points before it selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectKind {
    /// `W`: objects entirely inside two corners, whichever order they come in.
    Window,
    /// `C`: objects inside or crossing two corners, whichever order they come in.
    Crossing,
    /// `F`: objects crossed by a series of points; Enter ends it.
    Fence,
    /// `WP`: objects entirely inside a polygon; Enter closes it.
    WPolygon,
    /// `CP`: objects inside or crossing a polygon; Enter closes it.
    CPolygon,
    /// `BOX`: two corners; left to right is a window, right to left a crossing.
    Box,
    /// `G`: the objects of a group, by name.
    Group,
}

/// Most points a fence or polygon collects (typed input is hostile).
pub(crate) const MAX_MODE_POINTS: usize = 10_000;

/// A typed selection mode in progress and the points given so far.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectMode {
    pub kind: SelectKind,
    pub pts: Vec<Vec2>,
}

impl SelectMode {
    /// The mode named by text typed at "Select objects" (`W`, `Window`, `C`, `F`, `WP`, `CP`…).
    pub fn from_keyword(text: &str) -> Option<Self> {
        let kind = match text.trim().to_ascii_lowercase().as_str() {
            "w" | "window" => SelectKind::Window,
            "c" | "crossing" => SelectKind::Crossing,
            "f" | "fence" => SelectKind::Fence,
            "wp" | "wpolygon" => SelectKind::WPolygon,
            "cp" | "cpolygon" => SelectKind::CPolygon,
            "box" => SelectKind::Box,
            "g" | "group" => SelectKind::Group,
            _ => return None,
        };
        Some(SelectMode { kind, pts: Vec::new() })
    }
    fn is_box(&self) -> bool {
        matches!(self.kind, SelectKind::Window | SelectKind::Crossing | SelectKind::Box)
    }
    /// The prompt shown while the mode collects points.
    pub fn prompt(&self) -> Prompt {
        let last = self.pts.last().copied();
        let (first, next) = match self.kind {
            SelectKind::Group => return Prompt::new("Enter group name", Accept::TEXT),
            SelectKind::Window | SelectKind::Crossing | SelectKind::Box => ("Specify first corner", "Specify opposite corner"),
            SelectKind::Fence => ("Specify first fence point", "Specify next fence point"),
            SelectKind::WPolygon | SelectKind::CPolygon => ("Specify first polygon point", "Specify endpoint of line"),
        };
        match last {
            None => Prompt::new(first, Accept::POINT),
            Some(p) if self.is_box() => Prompt::new(next, Accept::POINT).base(p),
            Some(p) => Prompt::new(next, Accept::POINT).kw(&["Undo"]).base(p),
        }
    }
    /// Add a point. Returns whether the mode is complete (the second corner of W/C).
    pub fn push(&mut self, p: Vec2) -> bool {
        if p.is_finite() && self.pts.len() < MAX_MODE_POINTS {
            self.pts.push(p);
        }
        self.is_box() && self.pts.len() >= 2
    }
    /// The objects the mode selects, or `None` while it has too few points.
    pub fn select(&self, d: &Drawing, space: &Space) -> Option<Vec<Handle>> {
        match (self.kind, self.pts.as_slice()) {
            (SelectKind::Window | SelectKind::Crossing, [a, b, ..]) => {
                Some(select_window(d, space, Bounds2::new(*a, *b), self.kind == SelectKind::Crossing))
            }
            (SelectKind::Box, [a, b, ..]) => Some(select_window(d, space, Bounds2::new(*a, *b), b.x < a.x)),
            (SelectKind::Fence, pts) if pts.len() >= 2 => Some(select_fence(d, space, pts)),
            (SelectKind::WPolygon | SelectKind::CPolygon, pts) if pts.len() >= 3 => {
                Some(select_polygon(d, space, pts, self.kind == SelectKind::CPolygon))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_doc::{Common, RayLine};
    use cadcraft_geom::Vec3;

    fn fence_hits(kind: EntityKind, fence: [(f64, f64); 2]) -> usize {
        let mut d = Drawing::new_metric();
        d.add(&Space::Model, Common::default(), kind).unwrap();
        let f = [Vec2::new(fence[0].0, fence[0].1), Vec2::new(fence[1].0, fence[1].1)];
        select_fence(&d, &Space::Model, &f).len()
    }

    #[test]
    fn fence_selects_crossed_xline_and_ray() {
        let line = RayLine { base: Vec3::new(0.0, 0.0, 0.0), dir: Vec3::new(1.0, 0.0, 0.0) };
        for x in [-5.0, 0.0, 5.0] {
            assert_eq!(fence_hits(EntityKind::XLine(line.clone()), [(x, -5.0), (x, 5.0)]), 1, "xline at x={x}");
        }
        assert_eq!(fence_hits(EntityKind::Ray(line.clone()), [(5.0, -5.0), (5.0, 5.0)]), 1);
        assert_eq!(fence_hits(EntityKind::Ray(line.clone()), [(0.0, -5.0), (0.0, 5.0)]), 1);
        assert_eq!(fence_hits(EntityKind::Ray(line.clone()), [(-5.0, -5.0), (-5.0, 5.0)]), 0);
        assert_eq!(fence_hits(EntityKind::XLine(line.clone()), [(0.0, 1.0), (0.0, 5.0)]), 0);
        assert_eq!(fence_hits(EntityKind::Ray(line), [(5.0, 1.0), (5.0, 5.0)]), 0);
    }
}

#[cfg(test)]
mod solid_interior_tests {
    use super::*;

    fn face(kind: bool, triangular: bool) -> Entity {
        let a = Vec2::new(0.0, 0.0).to3(0.0);
        let b = Vec2::new(10.0, 0.0).to3(0.0);
        let c = Vec2::new(0.0, 10.0).to3(0.0);
        let d = if triangular { c } else { Vec2::new(10.0, 10.0).to3(0.0) };
        let solid = cadcraft_doc::Solid { corners: [a, b, c, d] };
        Entity::new(Handle(256), if kind { EntityKind::Solid(solid) } else { EntityKind::Trace(solid) })
    }

    #[test]
    fn crossing_window_inside_solid_and_trace_selects_fill() {
        let inside = Bounds2::new(Vec2::new(4.0, 4.0), Vec2::new(6.0, 6.0));
        for solid in [true, false] {
            let mut d = Drawing::default();
            let e = face(solid, false);
            assert!(in_window(&d, &e, None, &inside, true, 0.01));
            assert!(!in_window(&d, &e, None, &inside, false, 0.01));
            d.model.push(e);
            assert_eq!(select_window(&d, &Space::Model, inside, true), vec![Handle(256)]);
            assert!(select_window(&d, &Space::Model, inside, false).is_empty());
        }
    }

    #[test]
    fn crossing_window_in_triangular_bbox_but_outside_fill_does_not_select() {
        let d = Drawing::default();
        let e = face(true, true);
        let empty = Bounds2::new(Vec2::new(7.0, 7.0), Vec2::new(8.0, 8.0));
        assert!(!in_window(&d, &e, None, &empty, true, 0.01));
        let inside = Bounds2::new(Vec2::new(3.0, 3.0), Vec2::new(4.0, 4.0));
        assert!(in_window(&d, &e, None, &inside, true, 0.01));
    }

    #[test]
    fn pick_in_triangular_bbox_but_outside_fill_does_not_select() {
        let mut d = Drawing::default();
        d.model.push(face(true, true));
        let at = |x: f64, y: f64| pick(&d, &Space::Model, Vec2::new(x, y), 0.25);
        // The empty corner of the bounding rectangle, 5.66 from the hypotenuse.
        assert_eq!(at(9.0, 9.0), None);
        // Inside the fill, farther than the aperture from every edge.
        assert_eq!(at(2.0, 2.0), Some(Handle(256)));
        // Outside the fill but within the aperture of the hypotenuse.
        assert_eq!(at(5.1, 5.1), Some(Handle(256)));
    }
}

#[cfg(test)]
mod hatch_pattern_pick_tests {
    use super::*;
    use cadcraft_doc::{Common, Hatch, HatchLoop};
    use cadcraft_geom::PolyVertex;

    fn square(lo: f64, hi: f64, outer: bool) -> HatchLoop {
        let vertices = [(lo, lo), (hi, lo), (hi, hi), (lo, hi)].iter().map(|&(x, y)| PolyVertex::new(Vec2::new(x, y))).collect();
        HatchLoop { vertices, outer, sources: Vec::new() }
    }

    fn ansi31_hatch(loops: Vec<HatchLoop>) -> Drawing {
        let mut d = Drawing::new_metric();
        let hatch = Hatch {
            pattern: "ANSI31".into(),
            solid: false,
            loops,
            scale: 1.0,
            angle: 0.0,
            associative: false,
            style: 0,
            elevation: 0.0,
            gradient: None,
            origin: Vec2::ZERO,
            background: None,
            pattern_lines: Vec::new(),
        };
        d.add(&Space::Model, Common::default(), EntityKind::Hatch(hatch)).unwrap();
        d
    }

    #[test]
    fn pattern_hatch_is_picked_on_a_visible_stroke() {
        let d = ansi31_hatch(vec![square(0.0, 10.0, true)]);
        // The ANSI31 diagonal passes through the middle of the square.
        assert!(pick(&d, &Space::Model, Vec2::new(5.0, 5.0), 0.05).is_some());
        // The boundary is still pickable and a point outside is not.
        assert!(pick(&d, &Space::Model, Vec2::new(5.0, 0.0), 0.05).is_some());
        assert!(pick(&d, &Space::Model, Vec2::new(15.0, 5.0), 0.05).is_none());
    }

    #[test]
    fn pattern_hatch_island_stays_empty() {
        // The diagonal is clipped to [0,0]-[4,4] and [6,6]-[10,10] around the island.
        let d = ansi31_hatch(vec![square(0.0, 10.0, true), square(4.0, 6.0, false)]);
        assert!(pick(&d, &Space::Model, Vec2::new(1.0, 1.0), 0.05).is_some());
        assert!(pick(&d, &Space::Model, Vec2::new(5.0, 5.0), 0.05).is_none());
    }

    fn solid_hatch(loops: Vec<HatchLoop>, style: u8) -> Drawing {
        let mut d = Drawing::new_metric();
        let hatch = Hatch {
            pattern: "SOLID".into(),
            solid: true,
            loops,
            scale: 1.0,
            angle: 0.0,
            associative: false,
            style,
            elevation: 0.0,
            gradient: None,
            origin: Vec2::ZERO,
            background: None,
            pattern_lines: Vec::new(),
        };
        d.add(&Space::Model, Common::default(), EntityKind::Hatch(hatch)).unwrap();
        d
    }

    #[test]
    fn solid_hatch_is_picked_inside_its_fill_not_its_bounding_rectangle() {
        let tri = HatchLoop {
            vertices: [(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)].iter().map(|&(x, y)| PolyVertex::new(Vec2::new(x, y))).collect(),
            outer: true,
            sources: Vec::new(),
        };
        let d = solid_hatch(vec![tri], 0);
        // The empty corner of the bounding rectangle, 5.66 from the hypotenuse.
        assert!(pick(&d, &Space::Model, Vec2::new(9.0, 9.0), 0.25).is_none());
        assert!(pick(&d, &Space::Model, Vec2::new(2.0, 2.0), 0.25).is_some());
        // Outside the fill but within the aperture of the hypotenuse.
        assert!(pick(&d, &Space::Model, Vec2::new(5.1, 5.1), 0.25).is_some());
    }

    #[test]
    fn solid_hatch_island_follows_the_island_style() {
        let loops = || vec![square(0.0, 10.0, true), square(4.0, 6.0, false)];
        // Normal style leaves the island empty, as it is drawn.
        let d = solid_hatch(loops(), 0);
        assert!(pick(&d, &Space::Model, Vec2::new(5.0, 5.0), 0.05).is_none());
        assert!(pick(&d, &Space::Model, Vec2::new(2.0, 2.0), 0.05).is_some());
        // Ignore style fills over the island.
        let d = solid_hatch(loops(), 2);
        assert!(pick(&d, &Space::Model, Vec2::new(5.0, 5.0), 0.05).is_some());
    }
}

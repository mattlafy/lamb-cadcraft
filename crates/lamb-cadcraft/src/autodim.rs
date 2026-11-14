//! AUTODIM: a plan's generated exterior dimension strings (LAMB T641), an automatic first try at
//! dimensioning it. `autodim {view?, off?}` (`adim`) adds the placed plan to, or removes it from, the
//! document's `auto_dim_plans`; [`crate::annotations`] writes each listed plan's [`strings`] into its
//! annotation block, so they regenerate with the walls after every command, plot on the view's
//! sheets, and are never in the view block, derivation or LAMBSAVE. SHEETSET does not turn them on: a
//! plan with hand dimensions must not gain a second set unasked.
//!
//! Exterior straight runs only: a run whose outside is undecided, an arc run and an interior run get
//! none. Exterior faces that are collinear with the same outside and contiguous through a junction
//! or an opening bridge form one wall line. Its stations are its runs' exterior face ends; a run's
//! end at a corner (a junction with no collinear partner) is the face's own drawn end, because a
//! run stops where its two faces stop overlapping, short of the outside corner.
//!
//! Interior clear dimensions (LAMB T686) are a separate opt-in, `autodim {interior: true}`, kept in
//! `auto_dim_interior_plans`: [`interior`] gives each enclosed space its clear width and depth, inner
//! face to inner face. A space's inner faces are its straight boundary edges (every point on the
//! chord, junction stubs included) offset into it by half their wall's width; of the facing
//! antiparallel pairs that overlap, the longest-overlapping pair per direction (ties: the narrower)
//! is kept for the two directions with the longest overlap. Each dimension starts a third of the way
//! along the overlap and runs straight across the room, its line through the room. Arc edges never
//! pair; a space with no pair gets none.
//!
//! Locating dimensions (LAMB T694) are a third opt-in, `autodim {locate: true}`, kept in
//! `auto_dim_locate_plans`: [`locate`] sets out every straight interior wall on its centreline.
//! Interior straight runs are grouped by direction; per group, a chain is placed where the most
//! still-uncovered runs cross (the candidates are those runs' extent midpoints), and takes the
//! centreline of every run of the group crossing it. Each end continues to the inner face of the
//! nearest parallel exterior wall crossing the chain beyond it, or stops at the outermost centreline
//! when there is none; chains repeat until every run is covered, and a chain of one station gives no
//! dimension. Arc runs never take part.
//!
//! Grid dimensions (LAMB T705) are a fourth opt-in, `autodim {grid: true}`, kept in
//! `auto_dim_grid_plans`: [`grid`] gives every family of parallel grid lines the plan shows (its own
//! datum segments and those other views draw, [`crate::annotations::grid_lines`]) with two or more
//! members a chain, grid to grid, and with three or more an overall dimension too. Both sit at the
//! bubble end: a numbered family's (within 45° of local Y) top, any other's left. The chain is half
//! [`GRID_GAP_MM`] past the furthest line end, inside the bubble's stub; the overall one is
//! [`GRID_OVERALL_GAP_MM`] closer to the building. Each measured point is a grid line's intersection
//! with its dimension line.
//!
//! Opening dimensions (LAMB T713) are the elevation form, `autodim {view: <elevation>}` with no kind
//! flag, kept in `auto_dim_elevations`: [`elevation`] gives every opening the elevation marks
//! ([`crate::schedule::elevation_marks`]) whose schedule row has both a sill and a height one
//! vertical chain from its floor to its sill to its head (floor to head when the sill is zero, as
//! for a door), its dimension line [`ELEV_DIM_OFFSET_MM`] right of the opening's right jamb. An
//! opening no section reads gets none.
//!
//! The section form (LAMB T714), `autodim {view: <section>}` with no kind flag, kept in
//! `auto_dim_sections`: [`section`] gives every opening the section's own wall graph reads as a
//! bridge (cut wall above and below, [`crate::schedule::section_openings`]) one vertical chain from
//! its floor (the highest level at or below its sill) to its sill to its head. Its dimension line
//! sits [`ELEV_DIM_OFFSET_MM`] beyond the cut wall's face, on the side away from the middle of the
//! view block (the outside of an exterior wall). An opening below every level gets none.

use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use lamb_core::derivation::projection::{Uz, vertical_uz_to_local};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::derivation::wall_junctions::{RunEnd, WallGraph};
use lamb_core::derivation::wall_spaces::{WallSides, outside_on_left};
use lamb_core::derivation::walls::{RunCurve, RunFace};
use lamb_core::geometry::Point2;
use lamb_core::geometry::point::{add2, cross2, dist2, dot2, lerp2, normalize2, scale2, sub2};
use lamb_core::geometry::polyline::resolved_segments;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AnyView, Id, VerticalIntent};
use serde_json::{Value, json};
use std::f64::consts::FRAC_PI_2;

use crate::annotations::GRID_GAP_MM;
use crate::commands::{bad, edited_view, id_value, other};
use crate::detailtags::graph_and_sides;
use crate::project::DrawingProject;
use crate::schema::{read_document, view_block_name, write_document};
use cadcraft_doc::{Drawing, Space};

/// The inner string's dimension line sits this far off the face, at 1:50.
pub const INNER_STRING_MM: f64 = 800.0;
/// The outer string's dimension line sits this far off the face, at 1:50.
pub const OUTER_STRING_MM: f64 = 1400.0;

/// The overall grid dimension's line sits this far closer to the building than the chain's, at 1:50.
pub const GRID_OVERALL_GAP_MM: f64 = 600.0;

/// An elevation's opening chain sits this far right of the opening's right jamb, at 1:50.
pub const ELEV_DIM_OFFSET_MM: f64 = 300.0;

/// One generated dimension, in view-local coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DimSpan {
    pub a: Point2,
    pub b: Point2,
    /// The wall line's heading in `(-90°, 90°]`, radians, so the text reads upright.
    pub rotation: f64,
    /// A point on the dimension line.
    pub line_at: Point2,
}

/// A run's exterior face: the centreline's ends offset to the outside, and how far the drawn face
/// reaches, measured along `dir` from `start`.
struct ExteriorFace {
    start: Point2,
    end: Point2,
    dir: Point2,
    out: Point2,
    drawn_lo: f64,
    drawn_hi: f64,
}

/// The end points of `face`'s drawn segment; `None` for a face no longer resolvable.
fn drawn_segment(p: &DrawingProject<'_>, face: &RunFace) -> Option<(Point2, Point2)> {
    let e = p.entity(face.from.entity_id)?;
    let seg = resolved_segments(e.polyline.as_ref()?).get(face.from.segment_index).copied()?;
    Some((seg.from, seg.to))
}

/// Run `i`'s exterior face, when it is an exterior straight run with a decided outside.
fn exterior_face(p: &DrawingProject<'_>, graph: &WallGraph, sides: &WallSides, i: usize) -> Option<ExteriorFace> {
    let (run, side) = (graph.runs.get(i)?, sides.runs.get(i)?);
    let RunCurve::Line { from, to } = run.centreline else { return None };
    if !side.exterior || dist2(from, to) <= TOLERANCE.coincidence_mm {
        return None;
    }
    let left = outside_on_left(side)?;
    let dir = normalize2(sub2(to, from));
    let out = if left { Point2 { x: -dir.y, y: dir.x } } else { Point2 { x: dir.y, y: -dir.x } };
    let start = add2(from, scale2(out, run.width / 2.0));
    let end = add2(to, scale2(out, run.width / 2.0));
    let off = |s: &(Point2, Point2)| dot2(sub2(lerp2(s.0, s.1, 0.5), from), out);
    // The face on the outside of the centreline.
    let (a, b) = run.faces.iter().filter_map(|f| drawn_segment(p, f)).max_by(|x, y| off(x).total_cmp(&off(y)))?;
    let (s0, s1) = (dot2(sub2(a, start), dir), dot2(sub2(b, start), dir));
    Some(ExteriorFace { start, end, dir, out, drawn_lo: s0.min(s1), drawn_hi: s0.max(s1) })
}

/// Whether two exterior faces lie on one line with the same outside.
fn collinear(a: &ExteriorFace, b: &ExteriorFace) -> bool {
    cross2(a.dir, b.dir).abs() <= TOLERANCE.angle_deg.to_radians().sin()
        && dot2(a.out, b.out) > 0.0
        && dot2(a.out, sub2(b.start, a.start)).abs() <= TOLERANCE.coincidence_mm
}

/// The wall line `i` belongs to: the end of its parent chain.
fn root(parent: &[usize], i: usize) -> usize {
    let mut r = i;
    while let Some(&next) = parent.get(r).filter(|n| **n != r) {
        r = next;
    }
    r
}

/// `view`'s generated dimensions with their offsets multiplied by `factor`: per wall line, in the
/// order of its first run, the inner string (between consecutive stations) and, when the line has
/// more than two stations, the outer string (first to last).
pub(crate) fn strings(p: &DrawingProject<'_>, view: Id, factor: f64) -> Vec<DimSpan> {
    let (graph, sides) = graph_and_sides(p, view);
    let faces: Vec<Option<ExteriorFace>> = (0..graph.runs.len()).map(|i| exterior_face(p, &graph, &sides, i)).collect();
    let face = |i: usize| faces.get(i).and_then(Option::as_ref);
    let pair = |a: usize, b: usize| a != b && face(a).zip(face(b)).is_some_and(|(x, y)| collinear(x, y));

    // Union the runs along each line; a bridge's two ends are its runs' ends, so it adds no station.
    let mut joins: Vec<((usize, RunEnd), (usize, RunEnd))> = Vec::new();
    for j in &graph.junctions {
        for (n, x) in j.arms.iter().enumerate() {
            joins.extend(j.arms.iter().skip(n + 1).filter(|y| pair(x.run, y.run)).map(|y| ((x.run, x.end), (y.run, y.end))));
        }
    }
    joins.extend(graph.bridges.iter().filter(|b| pair(b.a.0, b.b.0)).map(|b| (b.a, b.b)));
    let mut parent: Vec<usize> = (0..faces.len()).collect();
    for (a, b) in &joins {
        let (ra, rb) = (root(&parent, a.0), root(&parent, b.0));
        if let Some(slot) = parent.get_mut(ra) {
            *slot = rb;
        }
    }
    let cornered = |i: usize, end: RunEnd| {
        !joins.iter().any(|(a, b)| *a == (i, end) || *b == (i, end))
            && graph.junctions.iter().any(|j| j.arms.iter().any(|a| a.run == i && a.end == end))
    };

    let mut out = Vec::new();
    let mut lines_done: Vec<usize> = Vec::new();
    for (first, f0) in faces.iter().enumerate() {
        let Some(f0) = f0 else { continue };
        let line = root(&parent, first);
        if lines_done.contains(&line) {
            continue;
        }
        lines_done.push(line);
        // The line's direction, turned so the text reads upright.
        let turn = f0.dir.y < 0.0 || (f0.dir.y == 0.0 && f0.dir.x < 0.0);
        let dir = if turn { scale2(f0.dir, -1.0) } else { f0.dir };
        let heading = dir.y.atan2(dir.x);
        let heading = if heading > FRAC_PI_2 + 1e-12 { heading - std::f64::consts::PI } else { heading };
        let dir = Point2 { x: heading.cos(), y: heading.sin() };
        let origin = f0.start;
        let mut stations = Vec::new();
        for (i, f) in faces.iter().enumerate().filter(|(i, _)| root(&parent, *i) == line) {
            let Some(f) = f else { continue };
            let length = dist2(f.start, f.end);
            let s0 = if cornered(i, RunEnd::Start) { f.drawn_lo.min(0.0) } else { 0.0 };
            let s1 = if cornered(i, RunEnd::End) { f.drawn_hi.max(length) } else { length };
            stations.extend([s0, s1].map(|s| dot2(sub2(add2(f.start, scale2(f.dir, s)), origin), dir)));
        }
        stations.sort_by(f64::total_cmp);
        stations.dedup_by(|b, a| *b - *a <= TOLERANCE.coincidence_mm);
        let place = |s: f64| add2(origin, scale2(dir, s));
        let span =
            |a: f64, b: f64, off: f64| DimSpan { a: place(a), b: place(b), rotation: heading, line_at: add2(place(b), scale2(f0.out, off * factor)) };
        out.extend(stations.windows(2).filter_map(|w| Some(span(*w.first()?, *w.get(1)?, INNER_STRING_MM))));
        if let (true, Some(a), Some(b)) = (stations.len() > 2, stations.first(), stations.last()) {
            out.push(span(*a, *b, OUTER_STRING_MM));
        }
    }
    out
}

/// A straight inner face of a space: its start, unit direction and length; the space is on its left.
struct InnerFace {
    start: Point2,
    dir: Point2,
    len: f64,
}

/// One direction's best facing pair so far: a representative direction, the overlap, the gap and
/// the dimension's two points.
struct Pairing {
    dir: Point2,
    overlap: f64,
    gap: f64,
    a: Point2,
    b: Point2,
}

/// `view`'s generated interior clear dimensions: per enclosed space, in order, its clear width and
/// depth between facing inner faces (see the module doc).
pub(crate) fn interior(p: &DrawingProject<'_>, view: Id) -> Vec<DimSpan> {
    let (_, sides) = graph_and_sides(p, view);
    let sin_tol = TOLERANCE.angle_deg.to_radians().sin();
    let tol = TOLERANCE.coincidence_mm;
    let left_of = |d: Point2| Point2 { x: -d.y, y: d.x };
    let mut out = Vec::new();
    for space in &sides.spaces {
        let faces: Vec<InnerFace> = space
            .boundary
            .iter()
            .filter_map(|e| {
                // Straight: every point (junction stubs included) on the chord; an arc's samples are not.
                let (a, b) = (*e.points.first()?, *e.points.last()?);
                let len = dist2(a, b);
                if len <= tol {
                    return None;
                }
                let dir = normalize2(sub2(b, a));
                if e.points.iter().any(|q| cross2(dir, sub2(*q, a)).abs() > tol) {
                    return None;
                }
                Some(InnerFace { start: add2(a, scale2(left_of(dir), e.half_width)), dir, len })
            })
            .collect();
        let mut groups: Vec<Pairing> = Vec::new();
        for fi in &faces {
            let left = left_of(fi.dir);
            for fj in &faces {
                if dot2(fi.dir, fj.dir) >= 0.0 || cross2(fi.dir, fj.dir).abs() > sin_tol {
                    continue;
                }
                // fj runs back along fi: its start is the far end of its projection.
                let s_hi = dot2(sub2(fj.start, fi.start), fi.dir);
                let (lo, hi) = ((s_hi - fj.len).max(0.0), s_hi.min(fi.len));
                let overlap = hi - lo;
                if overlap <= tol {
                    continue;
                }
                let a = add2(fi.start, scale2(fi.dir, lo + overlap / 3.0));
                let gap = dot2(sub2(fj.start, a), left);
                if gap <= tol {
                    continue;
                }
                let b = add2(a, scale2(left, gap));
                match groups.iter_mut().find(|g| cross2(g.dir, fi.dir).abs() <= sin_tol) {
                    Some(g) => {
                        if overlap > g.overlap + tol || ((overlap - g.overlap).abs() <= tol && gap < g.gap - tol) {
                            *g = Pairing { dir: g.dir, overlap, gap, a, b };
                        }
                    }
                    None => groups.push(Pairing { dir: fi.dir, overlap, gap, a, b }),
                }
            }
        }
        groups.sort_by(|x, y| y.overlap.total_cmp(&x.overlap));
        for g in groups.iter().take(2) {
            let d = sub2(g.b, g.a);
            let mut heading = d.y.atan2(d.x);
            if heading > FRAC_PI_2 + 1e-12 {
                heading -= std::f64::consts::PI;
            } else if heading <= -FRAC_PI_2 + 1e-12 {
                heading += std::f64::consts::PI;
            }
            out.push(DimSpan { a: g.a, b: g.b, rotation: heading, line_at: lerp2(g.a, g.b, 0.5) });
        }
    }
    out
}

/// A straight run's centreline, for [`locate`].
struct Centreline {
    from: Point2,
    to: Point2,
    dir: Point2,
    width: f64,
    exterior: bool,
}

/// `view`'s generated locating dimensions: per direction group of straight interior runs, chains
/// through their centrelines out to the bounding exterior walls' inner faces (see the module doc).
pub(crate) fn locate(p: &DrawingProject<'_>, view: Id) -> Vec<DimSpan> {
    let (graph, sides) = graph_and_sides(p, view);
    let tol = TOLERANCE.coincidence_mm;
    let sin_tol = TOLERANCE.angle_deg.to_radians().sin();
    let lines: Vec<Centreline> = graph
        .runs
        .iter()
        .zip(&sides.runs)
        .filter_map(|(r, side)| {
            let RunCurve::Line { from, to } = r.centreline else { return None };
            if dist2(from, to) <= tol {
                return None;
            }
            Some(Centreline { from, to, dir: normalize2(sub2(to, from)), width: r.width, exterior: side.exterior })
        })
        .collect();
    let upright = |mut heading: f64| {
        if heading > FRAC_PI_2 + 1e-12 {
            heading -= std::f64::consts::PI;
        } else if heading <= -FRAC_PI_2 + 1e-12 {
            heading += std::f64::consts::PI;
        }
        heading
    };
    // Interior runs by direction, in the order of their first run.
    let mut groups: Vec<(Point2, Vec<&Centreline>)> = Vec::new();
    for l in lines.iter().filter(|l| !l.exterior) {
        match groups.iter_mut().find(|g| cross2(g.0, l.dir).abs() <= sin_tol) {
            Some(g) => g.1.push(l),
            None => {
                let h = upright(l.dir.y.atan2(l.dir.x));
                groups.push((Point2 { x: h.cos(), y: h.sin() }, vec![l]));
            }
        }
    }
    let mut out = Vec::new();
    for (d, members) in &groups {
        let d = *d;
        let n = Point2 { x: -d.y, y: d.x };
        let rotation = upright(n.y.atan2(n.x));
        let extent = |l: &Centreline| {
            let (a, b) = (dot2(l.from, d), dot2(l.to, d));
            (a.min(b), a.max(b))
        };
        let crosses = |l: &Centreline, t: f64| {
            let (lo, hi) = extent(l);
            lo - tol <= t && t <= hi + tol
        };
        let mut covered = vec![false; members.len()];
        loop {
            // The uncovered midpoint the most uncovered runs cross; ties: the smallest.
            let mut best: Option<(usize, f64)> = None;
            for (l, _) in members.iter().zip(&covered).filter(|(_, c)| !**c) {
                let (lo, hi) = extent(l);
                let t = (lo + hi) / 2.0;
                let count = members.iter().zip(&covered).filter(|(m, c)| !**c && crosses(m, t)).count();
                if best.is_none_or(|(bc, bt)| count > bc || (count == bc && t < bt)) {
                    best = Some((count, t));
                }
            }
            let Some((_, t)) = best else { break };
            let mut stations: Vec<f64> = Vec::new();
            for (l, c) in members.iter().zip(covered.iter_mut()) {
                if crosses(l, t) {
                    *c = true;
                    stations.push(dot2(l.from, n));
                }
            }
            stations.sort_by(f64::total_cmp);
            stations.dedup_by(|b, a| *b - *a <= tol);
            let (Some(&first), Some(&last)) = (stations.first(), stations.last()) else { continue };
            // The nearest parallel exterior run crossing the chain on each side: its inner face.
            let walls: Vec<(f64, f64)> = lines
                .iter()
                .filter(|l| l.exterior && cross2(d, l.dir).abs() <= sin_tol && crosses(l, t))
                .map(|l| (dot2(l.from, n), l.width / 2.0))
                .collect();
            let below = walls.iter().filter(|(s, _)| *s < first - tol).max_by(|a, b| a.0.total_cmp(&b.0));
            let above = walls.iter().filter(|(s, _)| *s > last + tol).min_by(|a, b| a.0.total_cmp(&b.0));
            if let Some(face) = below.map(|(s, w)| s + w).filter(|f| *f < first - tol) {
                stations.insert(0, face);
            }
            if let Some(face) = above.map(|(s, w)| s - w).filter(|f| *f > last + tol) {
                stations.push(face);
            }
            let place = |s: f64| add2(scale2(d, t), scale2(n, s));
            out.extend(stations.windows(2).filter_map(|w| {
                let a = place(*w.first()?);
                Some(DimSpan { a, b: place(*w.get(1)?), rotation, line_at: a })
            }));
        }
    }
    out
}

/// A grid line in a family: a point on it, its unit direction, and its two ends.
type GridMemberLine = (Point2, Point2, [Point2; 2]);

/// `view`'s generated grid dimensions with their offsets multiplied by `factor`: per family of
/// parallel grid lines, in the order of its first line, the chain and then (three or more grids)
/// the overall dimension (see the module doc).
pub(crate) fn grid(p: &DrawingProject<'_>, view: Id, factor: f64) -> Vec<DimSpan> {
    let lines = crate::annotations::grid_lines(p.drawing(), p, view, factor).unwrap_or_default();
    let tol = TOLERANCE.coincidence_mm;
    let sin_tol = TOLERANCE.angle_deg.to_radians().sin();
    // Families: (direction, numbered, members).
    let mut families: Vec<(Point2, bool, Vec<GridMemberLine>)> = Vec::new();
    for g in &lines {
        let (a, b) = (Point2 { x: g.a.x, y: g.a.y }, Point2 { x: g.b.x, y: g.b.y });
        if dist2(a, b) <= tol {
            continue;
        }
        let u = normalize2(sub2(b, a));
        match families.iter_mut().find(|f| cross2(f.0, u).abs() <= sin_tol) {
            Some(f) => f.2.push((a, u, [a, b])),
            None => {
                let numbered = u.y.abs() >= u.x.abs();
                let flip = if numbered { u.y < 0.0 } else { u.x < 0.0 };
                families.push((if flip { scale2(u, -1.0) } else { u }, numbered, vec![(a, u, [a, b])]));
            }
        }
    }
    let upright = |mut heading: f64| {
        if heading > FRAC_PI_2 + 1e-12 {
            heading -= std::f64::consts::PI;
        } else if heading <= -FRAC_PI_2 + 1e-12 {
            heading += std::f64::consts::PI;
        }
        heading
    };
    let mut out = Vec::new();
    for (d, numbered, members) in &families {
        let (d, numbered) = (*d, *numbered);
        let n = if numbered { Point2 { x: d.y, y: -d.x } } else { Point2 { x: -d.y, y: d.x } };
        let rotation = upright(n.y.atan2(n.x));
        // Each grid's intersection with the dimension line `dot(q, d) = t`, by offset along `n`;
        // coincident grids count once.
        let stations = |t: f64| {
            let mut q: Vec<(f64, Point2)> = members
                .iter()
                .filter_map(|(a, u, _)| {
                    let along = dot2(*u, d);
                    (along.abs() > 0.5).then(|| add2(*a, scale2(*u, (t - dot2(*a, d)) / along)))
                })
                .map(|q| (dot2(q, n), q))
                .collect();
            q.sort_by(|x, y| x.0.total_cmp(&y.0));
            q.dedup_by(|b, a| b.0 - a.0 <= tol);
            q
        };
        let ends = members.iter().flat_map(|m| m.2).map(|e| dot2(e, d));
        let gap = GRID_GAP_MM * factor / 2.0;
        let t = if numbered { ends.fold(f64::NEG_INFINITY, f64::max) + gap } else { ends.fold(f64::INFINITY, f64::min) - gap };
        let chain = stations(t);
        if chain.len() < 2 {
            continue;
        }
        out.extend(chain.windows(2).filter_map(|w| {
            let (a, b) = (w.first()?.1, w.get(1)?.1);
            Some(DimSpan { a, b, rotation, line_at: a })
        }));
        if chain.len() > 2 {
            let toward = GRID_OVERALL_GAP_MM * factor;
            let overall = stations(if numbered { t - toward } else { t + toward });
            if let (Some(a), Some(b)) = (overall.first(), overall.last()) {
                out.push(DimSpan { a: a.1, b: b.1, rotation, line_at: a.1 });
            }
        }
    }
    out
}

/// Elevation `view`'s opening dimensions with their offset multiplied by `factor` (see the module
/// doc), per marked opening in [`crate::schedule::elevation_marks`] order, bottom up.
pub(crate) fn elevation(d: &Drawing, dp: &DrawingProject<'_>, view: Id, factor: f64) -> Vec<DimSpan> {
    let Some(v) = dp.view(view) else { return Vec::new() };
    let rows = crate::schedule::opening_rows(d);
    let tol = TOLERANCE.coincidence_mm;
    let mut out = Vec::new();
    for m in crate::schedule::elevation_marks(d).into_iter().filter(|m| m.view == view) {
        let Some(row) = rows.get(m.row) else { continue };
        let (Some(sill), Some(height)) = (row.sill, row.height) else { continue };
        let jamb = |u: f64| vertical_uz_to_local(&v, Uz { u, z: m.floor }).x;
        let x = jamb(m.u - row.width / 2.0).max(jamb(m.u + row.width / 2.0)) + ELEV_DIM_OFFSET_MM * factor;
        let mut zs = vec![m.floor];
        if sill > tol {
            zs.push(m.floor + sill);
        }
        zs.push(m.floor + sill + height);
        let at = |z: f64| Point2 { x, y: vertical_uz_to_local(&v, Uz { u: m.u, z }).y };
        out.extend(zs.windows(2).filter_map(|w| {
            let (a, b) = (at(*w.first()?), at(*w.get(1)?));
            Some(DimSpan { a, b, rotation: FRAC_PI_2, line_at: a })
        }));
    }
    out
}

/// Section `view`'s opening dimensions with their offset multiplied by `factor` (see the module
/// doc), per cut opening in wall-graph bridge order, bottom up.
pub(crate) fn section(d: &Drawing, dp: &DrawingProject<'_>, view: Id, factor: f64) -> Vec<DimSpan> {
    let Some(v) = dp.view(view) else { return Vec::new() };
    let tol = TOLERANCE.coincidence_mm;
    let ext = d.extents(&Space::Block(view_block_name(view)));
    let middle = if ext.is_empty() { None } else { Some((ext.min.x + ext.max.x) / 2.0) };
    let floors: Vec<f64> = dp.document().levels.iter().map(|l| l.floor_elevation.value).collect();
    let mut out = Vec::new();
    for (u, sill, head, width) in crate::schedule::section_openings(dp, view) {
        let Some(floor) = floors.iter().copied().filter(|z| *z <= sill + tol).reduce(f64::max) else { continue };
        let jamb = vertical_uz_to_local(&v, Uz { u, z: sill }).x;
        let side = match middle {
            Some(m) if jamb < m => -1.0,
            _ => 1.0,
        };
        let x = jamb + side * (width / 2.0 + ELEV_DIM_OFFSET_MM * factor);
        let mut zs = vec![floor];
        if sill - floor > tol {
            zs.push(sill);
        }
        zs.push(head);
        let at = |z: f64| Point2 { x, y: vertical_uz_to_local(&v, Uz { u, z }).y };
        out.extend(zs.windows(2).filter_map(|w| {
            let (a, b) = (at(*w.first()?), at(*w.get(1)?));
            Some(DimSpan { a, b, rotation: FRAC_PI_2, line_at: a })
        }));
    }
    out
}

/// What a non-plan view AUTODIM accepts is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Plan,
    Elevation,
    Section,
}

/// Which of AUTODIM's four independent kinds a call turns on or off.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Exterior,
    Interior,
    Locate,
    Grid,
}

/// Which kind of placed view `view` is, an elevation or section only for [`Kind::Exterior`], else
/// why not.
fn check_plan(s: &Session, view: Id, kind: Kind) -> std::result::Result<Target, String> {
    let doc = read_document(s.doc().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    match doc.views.iter().find(|v| v.id() == view) {
        None => Err("No such view".into()),
        Some(AnyView::Plan(p)) if p.placement.is_some() => Ok(Target::Plan),
        Some(AnyView::Plan(p)) => Err(format!("{} is floating: place it on a level before dimensioning it", p.name)),
        Some(AnyView::Vertical(e)) if e.placement.is_some() && matches!(e.intent, VerticalIntent::Elevation | VerticalIntent::Section) => {
            if kind != Kind::Exterior {
                Err("Interior, locate and grid dimensions are for plans".into())
            } else if e.intent == VerticalIntent::Section {
                Ok(Target::Section)
            } else {
                Ok(Target::Elevation)
            }
        }
        Some(_) => Err("AUTODIM dimensions plans, sections and elevations only".into()),
    }
}

/// AUTODIM `{view?, off?, interior?, locate?, grid?}`: turns the plan's generated dimension strings
/// on, or off with `off: true`; with `interior: true` the same for its interior clear dimensions
/// instead, with `locate: true` for its locating dimensions, with `grid: true` for its grid
/// dimensions, the four independent (any two of `interior`, `locate` and `grid` together refuse).
/// `view` defaults to the edited view. Returns `{view, dimensions, interior, locate, grid, message}`, `dimensions` counting what the plan now has of that kind. A plan already in
/// that state records no undo step.
pub fn autodim(s: &mut Session, p: &Value) -> Result<Value> {
    let view = match p.get("view") {
        None | Some(Value::Null) => edited_view(s).ok_or_else(|| other("Edit a plan first (EDITVIEW), or give `view`"))?,
        Some(v) => id_value(v).ok_or_else(|| bad("autodim", "`view` must be a view id"))?,
    };
    let off = match p.get("off") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(bad("autodim", "`off` must be true or false")),
    };
    let inside = match p.get("interior") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(bad("autodim", "`interior` must be true or false")),
    };
    let locating = match p.get("locate") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(bad("autodim", "`locate` must be true or false")),
    };
    let gridded = match p.get("grid") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(bad("autodim", "`grid` must be true or false")),
    };
    let kind = match (inside, locating, gridded) {
        (true, false, false) => Kind::Interior,
        (false, true, false) => Kind::Locate,
        (false, false, true) => Kind::Grid,
        (false, false, false) => Kind::Exterior,
        _ => return Err(bad("autodim", "give one of interior, locate or grid")),
    };
    let target = check_plan(s, view, kind).map_err(other)?;
    let vertical = target != Target::Plan;
    let mut doc = read_document(s.doc()?).map_err(|e| other(e.to_string()))?;
    let list = match kind {
        Kind::Exterior if target == Target::Elevation => &mut doc.auto_dim_elevations,
        Kind::Exterior if target == Target::Section => &mut doc.auto_dim_sections,
        Kind::Exterior => &mut doc.auto_dim_plans,
        Kind::Interior => &mut doc.auto_dim_interior_plans,
        Kind::Locate => &mut doc.auto_dim_locate_plans,
        Kind::Grid => &mut doc.auto_dim_grid_plans,
    };
    if list.contains(&view) == off {
        list.retain(|v| *v != view);
        if !off {
            list.push(view);
        }
        write_document(s.doc_mut()?, &doc).map_err(|e| other(e.to_string()))?;
    }
    let dimensions = if off {
        0
    } else {
        let dp = DrawingProject::new(s.doc()?).map_err(|e| other(e.to_string()))?;
        match kind {
            Kind::Exterior if target == Target::Elevation => elevation(s.doc()?, &dp, view, 1.0).len(),
            Kind::Exterior if target == Target::Section => section(s.doc()?, &dp, view, 1.0).len(),
            Kind::Exterior => strings(&dp, view, 1.0).len(),
            Kind::Interior => interior(&dp, view).len(),
            Kind::Locate => locate(&dp, view).len(),
            Kind::Grid => grid(&dp, view, 1.0).len(),
        }
    };
    let message = match (kind, off) {
        (Kind::Exterior, true) if vertical => "Opening dimensions off".to_owned(),
        (Kind::Exterior, false) if vertical => format!("{dimensions} opening dimensions"),
        (Kind::Exterior, true) => "Generated dimensions off".to_owned(),
        (Kind::Exterior, false) => format!("{dimensions} generated dimensions"),
        (Kind::Interior, true) => "Interior dimensions off".to_owned(),
        (Kind::Interior, false) => format!("{dimensions} interior dimensions"),
        (Kind::Locate, true) => "Locating dimensions off".to_owned(),
        (Kind::Locate, false) => format!("{dimensions} locating dimensions"),
        (Kind::Grid, true) => "Grid dimensions off".to_owned(),
        (Kind::Grid, false) => format!("{dimensions} grid dimensions"),
    };
    s.echo(message.clone());
    Ok(json!({ "view": view.0, "dimensions": dimensions, "interior": inside, "locate": locating, "grid": gridded, "message": message }))
}

/// AUTODIM's interactive form, on the edited plan.
struct AutoDimM {
    view: Id,
    /// `Exterior` at the first prompt; otherwise the kind whose `On or Off` sub-prompt it is at.
    kind: Kind,
}

impl Interactive for AutoDimM {
    fn name(&self) -> &'static str {
        "autodim"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        let on_off = |label: &str| Prompt::new(label, Accept::TEXT).kw(&["On", "Off"]).options_only().default("On".to_owned());
        match self.kind {
            Kind::Interior => on_off("Interior dimensions"),
            Kind::Locate => on_off("Locating dimensions"),
            Kind::Grid => on_off("Grid dimensions"),
            Kind::Exterior => {
                Prompt::new("Dimension plan", Accept::TEXT).kw(&["On", "Off", "Interior", "Locate", "Grid"]).options_only().default("On".to_owned())
            }
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        s.echo("AUTODIM: gives this plan generated dimension strings outside its exterior walls, which follow the walls and plot on its sheets.");
        let edited = edited_view(s).ok_or_else(|| "AUTODIM needs a plan: open one from the Views palette or with EDITVIEW.".to_owned());
        match edited.and_then(|v| check_plan(s, v, Kind::Exterior).map(|_| v)) {
            Ok(v) => {
                self.view = v;
                Ok(Step::Continue)
            }
            Err(e) => {
                s.echo(e);
                Ok(Step::Done)
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let off = match i {
            Input::Cancel => return Ok(Step::Cancel),
            Input::Enter => false,
            Input::Text(t) | Input::Keyword(t) => match t.trim().to_ascii_lowercase().as_str() {
                "on" => false,
                "off" => true,
                "interior" | "i" if self.kind == Kind::Exterior => {
                    self.kind = Kind::Interior;
                    return Ok(Step::Continue);
                }
                "locate" | "l" if self.kind == Kind::Exterior => {
                    self.kind = Kind::Locate;
                    return Ok(Step::Continue);
                }
                "grid" | "g" if self.kind == Kind::Exterior => {
                    self.kind = Kind::Grid;
                    return Ok(Step::Continue);
                }
                _ => {
                    s.echo(if self.kind == Kind::Exterior { "On, Off, Interior, Locate or Grid" } else { "On or Off" });
                    return Ok(Step::Continue);
                }
            },
            _ => return Ok(Step::Continue),
        };
        let p = json!({ "view": self.view.0, "off": off, "interior": self.kind == Kind::Interior, "locate": self.kind == Kind::Locate, "grid": self.kind == Kind::Grid });
        if let Err(e) = autodim(s, &p) {
            s.echo(e.to_string());
        }
        Ok(Step::Done)
    }
}

pub(crate) fn autodim_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(AutoDimM { view: Id(0), kind: Kind::Exterior }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{annotation_block_name, view_block_name};
    use cadcraft_doc::{DimKind, Dimension, EntityKind};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    /// A placed plan, edited, on `WALL-EXT`, observers installed.
    fn placed() -> T<(Session, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        Ok((s, plan))
    }

    /// [`placed`] with a closed 4000 × 3000 room of 200 mm walls, outer face 0..4000 × 0..3000.
    fn room() -> T<(Session, Id)> {
        let (mut s, plan) = placed()?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [4000, 0], [4000, 3000], [0, 3000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [3800, 200], [3800, 2800], [200, 2800]], "closed": true }))?;
        Ok((s, plan))
    }

    /// [`room`]'s walls with a 1000 window in the south wall, x 1500..2500.
    fn window_room() -> T<(Session, Id)> {
        let (mut s, plan) = placed()?;
        s.execute("pline", &json!({ "vertices": [[1500, 0], [0, 0], [0, 3000], [4000, 3000], [4000, 0], [2500, 0]] }))?;
        s.execute("pline", &json!({ "vertices": [[1500, 200], [200, 200], [200, 2800], [3800, 2800], [3800, 200], [2500, 200]] }))?;
        s.execute("layer.new", &json!({ "name": "WINDOW", "current": true }))?;
        s.execute("line", &json!({ "points": [[1500, 100], [2500, 100]] }))?;
        Ok((s, plan))
    }

    fn dims(s: &Session, view: Id) -> T<Vec<Dimension>> {
        let d = s.doc()?;
        let Some(b) = d.block(&annotation_block_name(view)) else { return Ok(Vec::new()) };
        Ok(b.entities.iter().filter_map(|e| if let EntityKind::Dimension(x) = &e.kind { Some(x.clone()) } else { None }).collect())
    }

    fn measured(s: &Session, dm: &Dimension) -> T<f64> {
        let st = s.doc()?.dim_style(&dm.style).cloned().unwrap_or_default();
        Ok(cadcraft_render::dimension_geometry(dm, &st, 1.0).measurement)
    }

    fn undo_len(s: &Session) -> T<usize> {
        Ok(s.state()?.undo.len())
    }

    type Summary = (i64, [i64; 2], [i64; 2], [i64; 2]);

    /// `(measurement, p13, p14, defpt)` in whole mm, sorted, for comparing with a literal.
    fn summary(s: &Session, view: Id) -> T<Vec<Summary>> {
        let xy = |v: cadcraft_geom::Vec3| [v.x.round() as i64, v.y.round() as i64];
        let mut got = Vec::new();
        for d in dims(s, view)? {
            got.push((measured(s, &d)?.round() as i64, xy(d.p13), xy(d.p14), xy(d.defpt)));
        }
        got.sort();
        Ok(got)
    }

    #[test]
    fn a_room_gets_four_single_strings_each_outside_its_wall_measuring_its_exterior_face() -> R {
        let (mut s, plan) = room()?;
        assert!(dims(&s, plan)?.is_empty(), "nothing changes without AUTODIM");
        let n = undo_len(&s)?;
        let r = s.execute("autodim", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!((r["view"].as_u64(), r["dimensions"].as_u64()), (Some(plan.0), Some(4)));
        assert_eq!(r["message"], "4 generated dimensions");
        assert_eq!(
            summary(&s, plan)?,
            [
                (3000, [0, 0], [0, 3000], [-800, 3000]),
                (3000, [4000, 0], [4000, 3000], [4800, 3000]),
                (4000, [0, 0], [4000, 0], [4000, -800]),
                (4000, [0, 3000], [4000, 3000], [4000, 3800]),
            ]
        );
        s.undo()?;
        assert!(dims(&s, plan)?.is_empty() && read_document(s.doc()?)?.auto_dim_plans.is_empty(), "undo removes both");
        Ok(())
    }

    #[test]
    fn dimensions_read_upright_with_the_standard_style_and_measured_text() -> R {
        let (mut s, plan) = room()?;
        s.execute("autodim", &json!({ "view": plan.0 }))?;
        for d in dims(&s, plan)? {
            let DimKind::Linear { rotation } = d.kind else { return Err("not a linear dimension".into()) };
            assert!(rotation.abs() < 1e-9 || (rotation - FRAC_PI_2).abs() < 1e-9, "{rotation}");
            assert!(d.text.is_empty() && d.style == "Standard" && d.assoc.is_empty() && d.block.is_none());
        }
        Ok(())
    }

    #[test]
    fn a_window_gap_gets_three_dimensions_summing_to_the_outer_string_on_the_outside() -> R {
        let (mut s, plan) = window_room()?;
        s.execute("autodim", &json!({ "view": plan.0 }))?;
        let got = summary(&s, plan)?;
        let south: Vec<_> = got.iter().filter(|d| d.1[1] == 0 && d.2[1] == 0).cloned().collect();
        assert_eq!(
            south,
            [
                (1000, [1500, 0], [2500, 0], [2500, -800]),
                (1500, [0, 0], [1500, 0], [1500, -800]),
                (1500, [2500, 0], [4000, 0], [4000, -800]),
                (4000, [0, 0], [4000, 0], [4000, -1400]),
            ]
        );
        assert_eq!(got.len(), 4 + 3, "the other three walls are single strings: {got:?}");
        Ok(())
    }

    #[test]
    fn stretching_a_wall_updates_the_measurement_in_that_commands_undo_step() -> R {
        let (mut s, plan) = room()?;
        s.execute("autodim", &json!({ "view": plan.0 }))?;
        let before = summary(&s, plan)?;
        let n = undo_len(&s)?;
        s.execute("stretch", &json!({ "window": [[3500, -100], [4100, 3100]], "delta": [500, 0] }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        let got = summary(&s, plan)?;
        assert_eq!(got.iter().filter(|d| d.0 == 4500).count(), 2, "{got:?}");
        assert!(got.iter().any(|d| d.1 == [4500, 0] && d.3 == [5300, 3000]), "the east string moved: {got:?}");
        s.undo()?;
        assert_eq!(summary(&s, plan)?, before);
        Ok(())
    }

    #[test]
    fn off_removes_them_and_an_interior_wall_and_an_arc_wall_get_none() -> R {
        let (mut s, plan) = room()?;
        s.execute("layer.new", &json!({ "name": "WALL-INT", "current": true }))?;
        s.execute("line", &json!({ "points": [[2000, 200], [2000, 2800]] }))?;
        s.execute("line", &json!({ "points": [[2100, 200], [2100, 2800]] }))?;
        s.execute("autodim", &json!({ "view": plan.0 }))?;
        let got = summary(&s, plan)?;
        assert!(!got.is_empty());
        assert!(got.iter().all(|d| d.3[0] < 0 || d.3[0] > 4000 || d.3[1] < 0 || d.3[1] > 3000), "all outside the room: {got:?}");
        assert!(got.iter().all(|d| !(d.1[0] == d.2[0] && (d.1[0] == 2000 || d.1[0] == 2100))), "no string along the interior wall: {got:?}");
        let n = undo_len(&s)?;
        let r = s.execute("autodim", &json!({ "view": plan.0, "off": true }))?;
        assert_eq!((undo_len(&s)?, r["dimensions"].as_u64()), (n + 1, Some(0)));
        assert!(dims(&s, plan)?.is_empty());
        assert!(read_document(s.doc()?)?.auto_dim_plans.is_empty());
        s.execute("autodim", &json!({ "view": plan.0, "off": true }))?;
        assert_eq!(undo_len(&s)?, n + 1, "already off: no undo step");

        // A D-shaped room: the arc wall gets no string, the straight walls keep theirs.
        let (mut s, plan) = placed()?;
        let v = |x: f64, y: f64, bulge: f64| json!({ "p": [x, y], "bulge": bulge });
        s.execute(
            "pline",
            &json!({ "vertices": [v(0.0, 0.0, 0.0), v(3000.0, 0.0, 1.0), v(3000.0, 2200.0, 0.0), v(0.0, 2200.0, 0.0)], "closed": true }),
        )?;
        s.execute(
            "pline",
            &json!({ "vertices": [v(200.0, 200.0, 0.0), v(3000.0, 200.0, 1.0), v(3000.0, 2000.0, 0.0), v(200.0, 2000.0, 0.0)], "closed": true }),
        )?;
        s.execute("autodim", &json!({ "view": plan.0 }))?;
        let got = summary(&s, plan)?;
        assert!(got.iter().any(|d| d.1 == [0, 0] && d.2 == [0, 2200]), "the west wall: {got:?}");
        assert!(got.iter().all(|d| d.1[0] != d.2[0] || d.1[0] == 0), "nothing on the arc's side: {got:?}");
        Ok(())
    }

    #[test]
    fn they_live_in_the_annotation_block_and_survive_a_dxf_round_trip() -> R {
        let (mut s, plan) = window_room()?;
        let in_view = |s: &Session| -> T<usize> { Ok(s.doc()?.block(&view_block_name(plan)).ok_or("no block")?.entities.len()) };
        let n = in_view(&s)?;
        let links = |s: &Session| -> T<String> { Ok(format!("{:?}", DrawingProject::new(s.doc()?)?.document().links)) };
        let before = links(&s)?;
        s.execute("autodim", &json!({ "view": plan.0 }))?;
        assert_eq!(in_view(&s)?, n, "not one in the view block");
        assert_eq!(dims(&s, plan)?.len(), 7);
        assert_eq!(links(&s)?, before);
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert!(crate::annotations::settled(&back)?, "the observer would change nothing after a round trip");
        assert_eq!(read_document(&back)?.auto_dim_plans, [plan]);

        // An angled wall's rotation survives too.
        let (mut s, plan) = placed()?;
        let tan = 15f64.to_radians().tan();
        let k = 200.0 / 15f64.to_radians().cos();
        s.execute("pline", &json!({ "vertices": [[0.0, 0.0], [4000.0, 4000.0 * tan], [4000.0, 3000.0], [0.0, 3000.0]], "closed": true }))?;
        s.execute(
            "pline",
            &json!({ "vertices": [[200.0, 200.0 * tan + k], [3800.0, 3800.0 * tan + k], [3800.0, 2800.0], [200.0, 2800.0]], "closed": true }),
        )?;
        s.execute("autodim", &json!({ "view": plan.0 }))?;
        let angled = dims(&s, plan)?;
        assert_eq!(angled.len(), 4, "{angled:?}");
        assert!(angled.iter().any(|d| matches!(d.kind, DimKind::Linear { rotation } if (rotation - 15f64.to_radians()).abs() < 1e-6)), "{angled:?}");
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert!(crate::annotations::settled(&back)?, "nor for an angled wall");
        Ok(())
    }

    #[test]
    fn it_follows_the_viewport_scale() -> R {
        let (mut s, plan) = room()?;
        s.execute("autodim", &json!({ "view": plan.0 }))?;
        let out = s.execute("newsheet", &json!({ "name": "P1", "viewIds": [plan.0] }))?;
        let hex = out["viewports"][0].as_str().ok_or("no viewport")?.to_owned();
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("line", &json!({ "points": [[0, 500], [40000, 500]] }))?;
        s.execute("vpview", &json!({ "handle": hex, "viewId": plan.0 }))?;
        let got = summary(&s, plan)?;
        assert!(got.iter().any(|d| d.1 == [0, 0] && d.2 == [4000, 0] && d.3 == [4000, -1600]), "800 mm at 1:50 is 1600 at 1:100: {got:?}");
        Ok(())
    }

    #[test]
    fn it_refuses_grid_on_a_section_and_a_floating_plan_and_sheetset_does_not_turn_it_on() -> R {
        let (mut s, plan) = room()?;
        let sec = s.execute(
            "newsectionline",
            &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 1000}, "cutTo": {"x": 4000, "y": 1000} }),
        )?;
        let sec = sec["viewId"].as_u64().ok_or("no id")?;
        let floating = s.execute("newplan", &json!({ "name": "Loose" }))?["viewId"].as_u64().ok_or("no id")?;
        let n = undo_len(&s)?;
        for v in [floating, 99_999] {
            assert!(s.execute("autodim", &json!({ "view": v })).is_err(), "{v}");
        }
        assert!(s.execute("autodim", &json!({ "view": sec, "grid": true })).is_err(), "grid on a section");
        assert_eq!(undo_len(&s)?, n, "a refusal records nothing");
        s.execute("sheetset", &json!({}))?;
        let doc = read_document(s.doc()?)?;
        assert!(dims(&s, plan)?.is_empty() && doc.auto_dim_plans.is_empty() && doc.auto_dim_sections.is_empty());
        Ok(())
    }

    /// The interior dimensions' measurements, sorted, in whole mm.
    fn interior_measured(s: &Session, view: Id) -> T<Vec<i64>> {
        let mut got = Vec::new();
        for d in dims(s, view)? {
            got.push(measured(s, &d)?.round() as i64);
        }
        got.sort();
        Ok(got)
    }

    #[test]
    fn a_room_gets_its_clear_width_and_depth_independently_of_the_exterior_strings() -> R {
        let (mut s, plan) = room()?;
        let n = undo_len(&s)?;
        let r = s.execute("autodim", &json!({ "view": plan.0, "interior": true }))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!((r["dimensions"].as_u64(), r["interior"].as_bool()), (Some(2), Some(true)));
        assert_eq!(r["message"], "2 interior dimensions");
        assert_eq!(interior_measured(&s, plan)?, [2600, 3600], "inner faces 200..3800 × 200..2800");
        assert!(read_document(s.doc()?)?.auto_dim_plans.is_empty(), "exterior strings untouched");
        for d in dims(&s, plan)? {
            let DimKind::Linear { rotation } = d.kind else { return Err("not a linear dimension".into()) };
            assert!(rotation.abs() < 1e-9 || (rotation - FRAC_PI_2).abs() < 1e-9, "upright: {rotation}");
            for q in [d.p13, d.p14, d.defpt] {
                assert!((200.0 - 1e-6..=3800.0 + 1e-6).contains(&q.x) && (200.0 - 1e-6..=2800.0 + 1e-6).contains(&q.y), "inside: {q:?}");
            }
        }
        s.execute("autodim", &json!({ "view": plan.0, "interior": true }))?;
        assert_eq!(undo_len(&s)?, n + 1, "a repeat is no undo step");
        s.execute("autodim", &json!({ "view": plan.0 }))?;
        assert_eq!(dims(&s, plan)?.len(), 2 + 4, "both kinds together");
        s.execute("autodim", &json!({ "view": plan.0, "off": true }))?;
        assert_eq!(interior_measured(&s, plan)?, [2600, 3600], "exterior off leaves interior");
        let n = undo_len(&s)?;
        let r = s.execute("autodim", &json!({ "view": plan.0, "interior": true, "off": true }))?;
        assert_eq!((undo_len(&s)?, r["message"].as_str()), (n + 1, Some("Interior dimensions off")));
        assert!(dims(&s, plan)?.is_empty() && read_document(s.doc()?)?.auto_dim_interior_plans.is_empty());
        s.execute("autodim", &json!({ "view": plan.0, "interior": true, "off": true }))?;
        assert_eq!(undo_len(&s)?, n + 1, "already off: no undo step");
        s.undo()?;
        assert_eq!(interior_measured(&s, plan)?, [2600, 3600], "undo brings them back");
        Ok(())
    }

    #[test]
    fn an_l_room_two_rooms_and_an_arc_room_pair_their_straight_walls() -> R {
        // L: outer face (0,0) (6000,0) (6000,3000) (3000,3000) (3000,6000) (0,6000), 200 walls.
        let (mut s, plan) = placed()?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [6000, 0], [6000, 3000], [3000, 3000], [3000, 6000], [0, 6000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [5800, 200], [5800, 2800], [2800, 2800], [2800, 5800], [200, 5800]], "closed": true }))?;
        let r = s.execute("autodim", &json!({ "view": plan.0, "interior": true }))?;
        assert_eq!(r["dimensions"].as_u64(), Some(2), "{r}");
        let got = interior_measured(&s, plan)?;
        assert!(got.iter().all(|m| [2600, 5600].contains(m)), "clear widths of the L: {got:?}");

        // Two rooms sharing a 100 wall at x 2000..2100, standing on the south and north walls (two T's).
        let (mut s, plan) = placed()?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [4000, 0], [4000, 3000], [0, 3000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [2000, 200], [2000, 2800], [200, 2800]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[2100, 200], [3800, 200], [3800, 2800], [2100, 2800]], "closed": true }))?;
        let r = s.execute("autodim", &json!({ "view": plan.0, "interior": true }))?;
        assert_eq!(r["dimensions"].as_u64(), Some(4), "{r}");
        assert_eq!(interior_measured(&s, plan)?, [1700, 1800, 2600, 2600]);

        // A D-shaped room: the arc never pairs, the north and south walls still do.
        let (mut s, plan) = placed()?;
        let v = |x: f64, y: f64, bulge: f64| json!({ "p": [x, y], "bulge": bulge });
        s.execute(
            "pline",
            &json!({ "vertices": [v(0.0, 0.0, 0.0), v(3000.0, 0.0, 1.0), v(3000.0, 2200.0, 0.0), v(0.0, 2200.0, 0.0)], "closed": true }),
        )?;
        s.execute(
            "pline",
            &json!({ "vertices": [v(200.0, 200.0, 0.0), v(3000.0, 200.0, 1.0), v(3000.0, 2000.0, 0.0), v(200.0, 2000.0, 0.0)], "closed": true }),
        )?;
        s.execute("autodim", &json!({ "view": plan.0, "interior": true }))?;
        assert_eq!(interior_measured(&s, plan)?, [1800]);
        Ok(())
    }

    #[test]
    fn interior_dimensions_follow_a_moved_wall_and_survive_a_dxf_round_trip() -> R {
        let (mut s, plan) = room()?;
        s.execute("autodim", &json!({ "view": plan.0, "interior": true }))?;
        let n = undo_len(&s)?;
        s.execute("stretch", &json!({ "window": [[3500, -100], [4100, 3100]], "delta": [500, 0] }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        assert_eq!(interior_measured(&s, plan)?, [2600, 4100]);
        s.undo()?;
        assert_eq!(interior_measured(&s, plan)?, [2600, 3600]);
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert!(crate::annotations::settled(&back)?, "the observer would change nothing after a round trip");
        assert_eq!(read_document(&back)?.auto_dim_interior_plans, [plan]);
        Ok(())
    }

    #[test]
    fn the_interactive_interior_option_asks_on_or_off() -> R {
        let (mut s, plan) = room()?;
        s.start("autodim")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Dimension plan [On/Off/Interior/Locate/Grid] <On>:"));
        s.input(Input::Keyword("Interior".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Interior dimensions [On/Off] <On>:"));
        s.input(Input::Enter)?;
        assert!(s.running.is_none());
        assert_eq!(interior_measured(&s, plan)?, [2600, 3600]);
        assert!(read_document(s.doc()?)?.auto_dim_plans.is_empty());
        s.start("autodim")?;
        s.input(Input::Keyword("Interior".into()))?;
        s.input(Input::Keyword("Off".into()))?;
        assert!(dims(&s, plan)?.is_empty());
        Ok(())
    }

    #[test]
    fn the_interactive_form_defaults_to_on_and_off_turns_it_off() -> R {
        let (mut s, plan) = room()?;
        s.start("autodim")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Dimension plan [On/Off/Interior/Locate/Grid] <On>:"));
        s.input(Input::Text("x".into()))?;
        assert!(s.running.is_some(), "re-asks");
        s.input(Input::Enter)?;
        assert!(s.running.is_none());
        assert_eq!(dims(&s, plan)?.len(), 4);
        s.start("adim")?;
        s.input(Input::Keyword("Off".into()))?;
        assert!(dims(&s, plan)?.is_empty());

        s.execute("editview", &json!({}))?;
        s.start("autodim")?;
        assert!(s.running.is_none(), "ends outside a plan");
        assert!(s.log.iter().any(|l| l == "AUTODIM needs a plan: open one from the Views palette or with EDITVIEW."), "{:?}", s.log);
        Ok(())
    }

    /// A 6000 × 4000 room of 200 walls (outer face on `WALL-EXT`) split into three by two 100
    /// partitions centred on x 2000 and x 4000; the room faces are on `WALL-INT`, so the partitions
    /// are interior.
    fn partitioned() -> T<(Session, Id)> {
        let (mut s, plan) = placed()?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [6000, 0], [6000, 4000], [0, 4000]], "closed": true }))?;
        s.execute("layer.new", &json!({ "name": "WALL-INT", "current": true }))?;
        for (x0, x1) in [(200, 1950), (2050, 3950), (4050, 5800)] {
            s.execute("pline", &json!({ "vertices": [[x0, 200], [x1, 200], [x1, 3800], [x0, 3800]], "closed": true }))?;
        }
        Ok((s, plan))
    }

    /// The locating dimensions as `(measurement, p13.y)` in whole mm, sorted by `p13.x`.
    fn chain(s: &Session, view: Id) -> T<Vec<(i64, i64, i64)>> {
        let mut got = Vec::new();
        for d in dims(s, view)? {
            got.push((d.p13.x.min(d.p14.x).round() as i64, measured(s, &d)?.round() as i64, d.p13.y.round() as i64));
        }
        got.sort();
        Ok(got)
    }

    #[test]
    fn two_partitions_get_one_chain_from_inner_face_through_their_centrelines() -> R {
        let (mut s, plan) = partitioned()?;
        let n = undo_len(&s)?;
        let r = s.execute("autodim", &json!({ "view": plan.0, "locate": true }))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!((r["dimensions"].as_u64(), r["locate"].as_bool()), (Some(3), Some(true)), "{r}");
        assert_eq!(r["message"], "3 locating dimensions");
        let got = chain(&s, plan)?;
        assert_eq!(got.iter().map(|g| (g.0, g.1)).collect::<Vec<_>>(), [(200, 1800), (2000, 2000), (4000, 1800)], "{got:?}");
        assert!(got.iter().all(|g| g.2 == got[0].2) && (200..=3800).contains(&got[0].2), "one string line through the room: {got:?}");
        for d in dims(&s, plan)? {
            let DimKind::Linear { rotation } = d.kind else { return Err("not a linear dimension".into()) };
            assert!(rotation.abs() < 1e-9, "reads along x: {rotation}");
        }
        assert!(read_document(s.doc()?)?.auto_dim_plans.is_empty() && read_document(s.doc()?)?.auto_dim_interior_plans.is_empty());

        // Moving a partition updates them in that command's undo step.
        let n = undo_len(&s)?;
        s.execute("stretch", &json!({ "window": [[1900, 100], [2100, 3900]], "delta": [300, 0] }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        assert_eq!(chain(&s, plan)?.iter().map(|g| (g.0, g.1)).collect::<Vec<_>>(), [(200, 2100), (2300, 1700), (4000, 1800)]);
        s.undo()?;
        assert_eq!(chain(&s, plan)?, got);

        // interior + locate refuses; off removes them; a repeat records nothing.
        let n = undo_len(&s)?;
        assert!(s.execute("autodim", &json!({ "view": plan.0, "locate": true, "interior": true })).is_err());
        assert_eq!(undo_len(&s)?, n);
        let r = s.execute("autodim", &json!({ "view": plan.0, "locate": true, "off": true }))?;
        assert_eq!((undo_len(&s)?, r["message"].as_str()), (n + 1, Some("Locating dimensions off")));
        assert!(dims(&s, plan)?.is_empty() && read_document(s.doc()?)?.auto_dim_locate_plans.is_empty());
        s.execute("autodim", &json!({ "view": plan.0, "locate": true, "off": true }))?;
        assert_eq!(undo_len(&s)?, n + 1, "already off: no undo step");
        Ok(())
    }

    #[test]
    fn locating_dimensions_live_in_the_annotation_block_and_survive_a_dxf_round_trip() -> R {
        let (mut s, plan) = partitioned()?;
        let in_view = |s: &Session| -> T<usize> { Ok(s.doc()?.block(&view_block_name(plan)).ok_or("no block")?.entities.len()) };
        let n = in_view(&s)?;
        s.execute("autodim", &json!({ "view": plan.0, "locate": true }))?;
        assert_eq!(in_view(&s)?, n, "not one in the view block");
        assert_eq!(dims(&s, plan)?.len(), 3);
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert!(crate::annotations::settled(&back)?, "the observer would change nothing after a round trip");
        assert_eq!(read_document(&back)?.auto_dim_locate_plans, [plan]);
        Ok(())
    }

    #[test]
    fn a_partition_off_the_first_line_gets_its_own_chain_and_a_lone_one_or_an_arc_none() -> R {
        // Two partitions standing on the south and north walls, x 2000 (y 200..1500) and x 4000
        // (y 2500..3800): no one line crosses both.
        let (mut s, plan) = placed()?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [6000, 0], [6000, 4000], [0, 4000]], "closed": true }))?;
        s.execute("layer.new", &json!({ "name": "WALL-INT", "current": true }))?;
        s.execute(
            "pline",
            &json!({ "vertices": [[200, 200], [1950, 200], [1950, 1500], [2050, 1500], [2050, 200], [5800, 200], [5800, 3800],
                [4050, 3800], [4050, 2500], [3950, 2500], [3950, 3800], [200, 3800]], "closed": true }),
        )?;
        s.execute("autodim", &json!({ "view": plan.0, "locate": true }))?;
        let mut got = chain(&s, plan)?;
        got.sort_by_key(|g| (g.2, g.0));
        let ys: Vec<i64> = got.iter().map(|g| g.2).collect();
        assert_eq!(got.iter().map(|g| (g.0, g.1)).collect::<Vec<_>>(), [(200, 1800), (2000, 3800), (200, 3800), (4000, 1800)], "{got:?}");
        assert!(ys[0] == ys[1] && ys[2] == ys[3] && ys[0] < 1500 && ys[2] > 2500, "two string lines: {got:?}");

        // A stadium room, arcs east and west: nothing parallel bounds a vertical stub's chain, so
        // the lone stub gets no dimension.
        let (mut s, plan) = placed()?;
        let v = |x: f64, y: f64, bulge: f64| json!({ "p": [x, y], "bulge": bulge });
        s.execute(
            "pline",
            &json!({ "vertices": [v(1000.0, 0.0, 0.0), v(3000.0, 0.0, 1.0), v(3000.0, 2200.0, 0.0), v(1000.0, 2200.0, 1.0)], "closed": true }),
        )?;
        s.execute("layer.new", &json!({ "name": "WALL-INT", "current": true }))?;
        s.execute(
            "pline",
            &json!({ "vertices": [v(1000.0, 200.0, 0.0), v(1950.0, 200.0, 0.0), v(1950.0, 1000.0, 0.0), v(2050.0, 1000.0, 0.0),
                v(2050.0, 200.0, 0.0), v(3000.0, 200.0, 1.0), v(3000.0, 2000.0, 0.0), v(1000.0, 2000.0, 1.0)], "closed": true }),
        )?;
        let dp = DrawingProject::new(s.doc()?)?;
        let (graph, sides) = graph_and_sides(&dp, plan);
        assert!(
            graph.runs.iter().zip(&sides.runs).any(|(r, x)| !x.exterior && matches!(r.centreline, RunCurve::Line { .. })),
            "the stub is an interior straight run"
        );
        let r = s.execute("autodim", &json!({ "view": plan.0, "locate": true }))?;
        assert_eq!(r["dimensions"].as_u64(), Some(0), "{r}");
        assert!(dims(&s, plan)?.is_empty());

        // A D-shaped room: the stub is located off the west wall; the arc bounds nothing.
        let (mut s, plan) = placed()?;
        let v = |x: f64, y: f64, bulge: f64| json!({ "p": [x, y], "bulge": bulge });
        s.execute(
            "pline",
            &json!({ "vertices": [v(0.0, 0.0, 0.0), v(3000.0, 0.0, 1.0), v(3000.0, 2200.0, 0.0), v(0.0, 2200.0, 0.0)], "closed": true }),
        )?;
        s.execute("layer.new", &json!({ "name": "WALL-INT", "current": true }))?;
        s.execute(
            "pline",
            &json!({ "vertices": [v(200.0, 200.0, 0.0), v(1450.0, 200.0, 0.0), v(1450.0, 1000.0, 0.0), v(1550.0, 1000.0, 0.0),
                v(1550.0, 200.0, 0.0), v(3000.0, 200.0, 1.0), v(3000.0, 2000.0, 0.0), v(200.0, 2000.0, 0.0)], "closed": true }),
        )?;
        s.execute("autodim", &json!({ "view": plan.0, "locate": true }))?;
        assert_eq!(chain(&s, plan)?.iter().map(|g| (g.0, g.1)).collect::<Vec<_>>(), [(200, 1300)]);
        Ok(())
    }

    #[test]
    fn the_interactive_locate_option_asks_on_or_off() -> R {
        let (mut s, plan) = partitioned()?;
        s.start("autodim")?;
        s.input(Input::Text("L".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Locating dimensions [On/Off] <On>:"));
        s.input(Input::Enter)?;
        assert!(s.running.is_none());
        assert_eq!(dims(&s, plan)?.len(), 3);
        assert_eq!(read_document(s.doc()?)?.auto_dim_locate_plans, [plan]);
        Ok(())
    }

    /// A placed plan with N–S grids at x 0, 6000 and 13500 (y −1000..9000) and E–W grids at y 0
    /// and 8000 (x −1000..14500), and a second placed plan on Level 2 that shows them all derived.
    /// Returns (session, plan, the other plan, the middle N–S grid's entity id).
    fn grids() -> T<(Session, Id, Id, Id)> {
        let (mut s, plan) = placed()?;
        let l2 =
            Id(s.execute("level", &json!({ "mode": "create", "name": "Level 2", "floorElevationMm": 3000 }))?["levelId"].as_u64().ok_or("no id")?);
        let upper = Id(s.execute("newplan", &json!({ "name": "Upper" }))?["viewId"].as_u64().ok_or("no id")?);
        let t = json!({ "originX": 0.0, "originY": 0.0, "rotation": 0.0 });
        s.execute("placeview", &json!({ "viewId": upper.0, "kind": "plan", "levelId": l2.0, "cutElevation": 4200, "transform": t }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "GRID", "current": true }))?;
        let datum = crate::annotations::tests::datum_line;
        datum(&mut s, plan, [0.0, -1000.0], [0.0, 9000.0])?;
        let middle = datum(&mut s, plan, [6000.0, -1000.0], [6000.0, 9000.0])?;
        datum(&mut s, plan, [13500.0, -1000.0], [13500.0, 9000.0])?;
        datum(&mut s, plan, [-1000.0, 0.0], [14500.0, 0.0])?;
        datum(&mut s, plan, [-1000.0, 8000.0], [14500.0, 8000.0])?;
        Ok((s, plan, upper, middle))
    }

    /// The grid dimensions as (measurement, rotation in whole degrees, p13, p14, defpt), sorted.
    fn grid_dims(s: &Session, view: Id) -> T<Vec<(i64, i64, [i64; 2], [i64; 2], [i64; 2])>> {
        let xy = |v: cadcraft_geom::Vec3| [v.x.round() as i64, v.y.round() as i64];
        let mut got = Vec::new();
        for d in dims(s, view)? {
            let DimKind::Linear { rotation } = d.kind else { return Err("not a linear dimension".into()) };
            got.push((measured(s, &d)?.round() as i64, rotation.to_degrees().round() as i64, xy(d.p13), xy(d.p14), xy(d.defpt)));
        }
        got.sort();
        Ok(got)
    }

    /// Checks [`grids`]' string with the middle N–S grid at `mid`: 1 at the top across x, 1 at the
    /// left across y, each point on its grid line, the chain between the line ends and the bubbles,
    /// the overall one closer to the building.
    fn check_grid_string(got: &[(i64, i64, [i64; 2], [i64; 2], [i64; 2])], mid: i64) -> R {
        let across: Vec<_> = got.iter().filter(|g| g.1 == 0).collect();
        let down: Vec<_> = got.iter().filter(|g| g.1 == 90).collect();
        assert_eq!(across.iter().map(|g| g.0).collect::<Vec<_>>(), {
            let mut m = vec![mid, 13500 - mid, 13500];
            m.sort();
            m
        });
        for g in &across {
            assert!([g.2[0], g.3[0]].iter().all(|x| [0, mid, 13500].contains(x)), "on a N–S grid: {g:?}");
            assert!(g.2[1] == g.4[1] && g.3[1] == g.4[1], "measured on its dimension line: {g:?}");
        }
        let chain_y = across.iter().find(|g| g.0 != 13500).ok_or("no chain")?.4[1];
        let overall_y = across.iter().find(|g| g.0 == 13500).ok_or("no overall")?.4[1];
        assert!(9000 < chain_y && chain_y < 9500 && overall_y < chain_y, "{across:?}");
        assert_eq!(down.len(), 1, "two E–W grids: one dimension, no overall: {down:?}");
        let g = down[0];
        assert!(g.0 == 8000 && [g.2[1], g.3[1]].iter().all(|y| [0, 8000].contains(y)), "{g:?}");
        assert!(g.2[0] == g.4[0] && -1500 < g.4[0] && g.4[0] < -1000, "at the left, before the bubbles: {g:?}");
        Ok(())
    }

    #[test]
    fn a_gridded_plan_gets_its_bay_and_overall_grid_strings_and_a_derived_plan_the_same() -> R {
        let (mut s, plan, upper, _) = grids()?;
        assert!(dims(&s, plan)?.is_empty(), "nothing changes without AUTODIM");
        let n = undo_len(&s)?;
        let r = s.execute("autodim", &json!({ "view": plan.0, "grid": true }))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!((r["dimensions"].as_u64(), r["grid"].as_bool(), r["message"].as_str()), (Some(4), Some(true), Some("4 grid dimensions")));
        let got = grid_dims(&s, plan)?;
        check_grid_string(&got, 6000)?;
        assert!(read_document(s.doc()?)?.auto_dim_plans.is_empty(), "exterior strings untouched");

        // The other plan's grids are all derived from this one: the same string.
        s.execute("autodim", &json!({ "view": upper.0, "grid": true }))?;
        assert_eq!(grid_dims(&s, upper)?, got);
        assert_eq!(read_document(s.doc()?)?.auto_dim_grid_plans, [plan, upper]);

        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert!(crate::annotations::settled(&back)?, "the observer would change nothing after a round trip");
        assert_eq!(read_document(&back)?.auto_dim_grid_plans, [plan, upper]);
        Ok(())
    }

    #[test]
    fn moving_a_grid_regenerates_the_string_in_its_undo_step_and_off_removes_it() -> R {
        let (mut s, plan, upper, middle) = grids()?;
        s.execute("autodim", &json!({ "view": plan.0, "grid": true }))?;
        s.execute("autodim", &json!({ "view": upper.0, "grid": true }))?;
        let before = grid_dims(&s, plan)?;
        let n = undo_len(&s)?;
        s.execute("move", &json!({ "handles": [format!("{:X}", middle.0)], "delta": [500, 0] }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        check_grid_string(&grid_dims(&s, plan)?, 6500)?;
        check_grid_string(&grid_dims(&s, upper)?, 6500)?;
        s.undo()?;
        assert_eq!(grid_dims(&s, plan)?, before);

        let n = undo_len(&s)?;
        for p in [json!({ "view": plan.0, "grid": true, "interior": true }), json!({ "view": plan.0, "grid": true, "locate": true })] {
            assert!(s.execute("autodim", &p).is_err(), "{p}");
        }
        assert_eq!(undo_len(&s)?, n, "a refusal records nothing");
        let r = s.execute("autodim", &json!({ "view": plan.0, "grid": true, "off": true }))?;
        assert_eq!((undo_len(&s)?, r["message"].as_str()), (n + 1, Some("Grid dimensions off")));
        assert!(dims(&s, plan)?.is_empty() && read_document(s.doc()?)?.auto_dim_grid_plans == [upper]);
        s.execute("autodim", &json!({ "view": plan.0, "grid": true, "off": true }))?;
        assert_eq!(undo_len(&s)?, n + 1, "already off: no undo step");

        // The interactive form's Grid option.
        s.start("autodim")?;
        s.input(Input::Keyword("Grid".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Grid dimensions [On/Off] <On>:"));
        s.input(Input::Enter)?;
        assert!(s.running.is_none());
        assert_eq!(grid_dims(&s, plan)?, before);
        Ok(())
    }

    /// The elevation's opening dimensions as `(line x, z from, z to)`, whole mm, bottom up: the
    /// vertical ones right of the window, not the floor-to-floor level dimension.
    fn opening_dims(s: &Session, view: Id) -> T<Vec<(i64, i64, i64)>> {
        let mut got: Vec<(i64, i64, i64)> = dims(s, view)?
            .iter()
            .map(|d| (d.defpt.x.round() as i64, d.p13.y.min(d.p14.y).round() as i64, d.p13.y.max(d.p14.y).round() as i64))
            .collect();
        got.sort_by_key(|g| (g.1, g.2));
        Ok(got)
    }

    /// The south room with `layer`'s opening x 2000..3000, a South elevation facing it, and, when
    /// given, a section reading its `(sill, head)`. Returns (session, plan, elevation).
    fn elevated(layer: &str, read: Option<(f64, f64)>) -> T<(Session, Id, Id)> {
        use crate::schedule::tests as st;
        let (mut s, plan) = st::project()?;
        st::walls(&mut s, (2000.0, 3000.0), None)?;
        st::opening(&mut s, layer, (2000.0, 100.0), (3000.0, 100.0))?;
        let south = st::elevation(&mut s, plan, "South", (-500.0, -1000.0), (5500.0, -1000.0))?;
        if let Some(w) = read {
            st::section_through(&mut s, plan, &[w], 3000.0)?;
        }
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        Ok((s, plan, south))
    }

    #[test]
    fn an_opted_in_elevation_chains_a_window_floor_sill_head_right_of_its_jamb() -> R {
        let (mut s, _plan, south) = elevated("WINDOW", Some((900.0, 2100.0)))?;
        assert!(opening_dims(&s, south)?.is_empty(), "nothing before AUTODIM");
        let n = undo_len(&s)?;
        let r = s.execute("autodim", &json!({ "view": south.0 }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        assert_eq!(r["message"], "2 opening dimensions");
        assert_eq!(read_document(s.doc()?)?.auto_dim_elevations, [south]);
        // The window is u 2500..3500 on the elevation; its chain sits 300 right of 3500.
        assert_eq!(opening_dims(&s, south)?, [(3800, 0, 900), (3800, 900, 2100)]);
        assert!(crate::annotations::settled(s.doc()?)?, "a second pass changes nothing");
        let r = s.execute("autodim", &json!({ "view": south.0, "off": true }))?;
        assert_eq!(r["message"], "Opening dimensions off");
        assert!(opening_dims(&s, south)?.is_empty());
        Ok(())
    }

    #[test]
    fn a_door_gets_one_floor_to_head_dimension_and_an_unread_opening_none() -> R {
        // A section reads an opening only between wall above and below, so the door's cut wall
        // starts on a 300 footing below the floor.
        let (mut s, plan, south) = elevated("door-1", None)?;
        let sec = Id(s.execute(
            "newsectionline",
            &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 2500, "y": -1000}, "cutTo": {"x": 2500, "y": 1000} }),
        )?["viewId"]
            .as_u64()
            .ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        crate::schedule::tests::layer(&mut s, "WALL-EXT")?;
        for (z0, z1) in [(-300.0, 0.0), (2100.0, 3000.0)] {
            for u in [1000.0, 1200.0] {
                s.execute("pline", &json!({ "vertices": [[u, z0], [u, z1]] }))?;
            }
        }
        crate::schedule::tests::layer(&mut s, "WINDOW")?;
        s.execute("pline", &json!({ "vertices": [[1100.0, 0.0], [1100.0, 2100.0]] }))?;
        let hs: Vec<String> = s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.iter().map(|e| e.handle.hex()).collect();
        s.execute("setrole", &json!({ "handles": hs, "role": "cut" }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let rows = crate::schedule::opening_rows(s.doc()?);
        assert_eq!(rows.iter().map(|r| (r.kind.as_str(), r.sill, r.height)).collect::<Vec<_>>(), [("Door", Some(0.0), Some(2100.0))]);
        s.execute("autodim", &json!({ "view": south.0 }))?;
        assert_eq!(opening_dims(&s, south)?, [(3800, 0, 2100)]);
        let (mut s, _plan, south) = elevated("WINDOW", None)?;
        let r = s.execute("autodim", &json!({ "view": south.0 }))?;
        assert_eq!(r["message"], "0 opening dimensions");
        assert!(opening_dims(&s, south)?.is_empty());
        Ok(())
    }

    #[test]
    fn an_elevation_and_a_section_refuse_the_plan_kinds_and_a_floating_vertical_view_is_refused() -> R {
        let (mut s, plan, south) = elevated("WINDOW", Some((900.0, 2100.0)))?;
        let sec = s.execute(
            "newsectionline",
            &json!({ "name": "B-B", "hostViewId": plan.0, "cutFrom": {"x": 4000, "y": -1000}, "cutTo": {"x": 4000, "y": 1000} }),
        )?["viewId"]
            .as_u64()
            .ok_or("no id")?;
        let floating = Id(s.execute("newdetail", &json!({ "name": "Loose", "cutDirection": "vertical" }))?["viewId"].as_u64().ok_or("no id")?);
        let n = undo_len(&s)?;
        let e = s.execute("autodim", &json!({ "view": floating.0 })).err().ok_or("a floating vertical view is refused")?;
        assert!(e.to_string().contains("plans, sections and elevations only"), "{e}");
        for view in [south.0, sec] {
            for flag in ["grid", "interior", "locate"] {
                let e = s.execute("autodim", &json!({ "view": view, flag: true })).err().ok_or("refused")?;
                assert!(e.to_string().contains("are for plans"), "{flag}: {e}");
            }
        }
        assert_eq!(undo_len(&s)?, n, "a refusal records nothing");
        Ok(())
    }

    /// `section_through` the south wall, plus the north wall's cut (u 4800..5000) so the view block
    /// has a middle, inside the room.
    fn sectioned(s: &mut Session, plan: Id, windows: &[(f64, f64)], top: f64) -> T<Id> {
        let sec = crate::schedule::tests::section_through(s, plan, windows, top)?;
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        crate::schedule::tests::layer(s, "WALL-EXT")?;
        let before = s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.len();
        for u in [4800.0, 5000.0] {
            s.execute("pline", &json!({ "vertices": [[u, 0.0], [u, top]] }))?;
        }
        let hs: Vec<String> = s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.iter().skip(before).map(|e| e.handle.hex()).collect();
        s.execute("setrole", &json!({ "handles": hs, "role": "cut" }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        Ok(sec)
    }

    #[test]
    fn an_opted_in_section_chains_each_cut_window_floor_sill_head_outside_its_wall() -> R {
        let (mut s, plan) = crate::schedule::tests::window_south()?;
        let sec = sectioned(&mut s, plan, &[(900.0, 2100.0)], 3000.0)?;
        assert!(opening_dims(&s, sec)?.is_empty(), "nothing before AUTODIM");
        let n = undo_len(&s)?;
        let r = s.execute("autodim", &json!({ "view": sec.0 }))?;
        assert_eq!((undo_len(&s)?, r["message"].as_str()), (n + 1, Some("2 opening dimensions")));
        assert_eq!(read_document(s.doc()?)?.auto_dim_sections, [sec]);
        // The wall is u 1000..1200, outside towards u 0; the chain sits 300 beyond its face.
        assert_eq!(opening_dims(&s, sec)?, [(700, 0, 900), (700, 900, 2100)]);
        assert!(crate::annotations::settled(s.doc()?)?, "a second pass changes nothing");
        let r = s.execute("autodim", &json!({ "view": sec.0, "off": true }))?;
        assert_eq!(r["message"], "Opening dimensions off");
        assert!(opening_dims(&s, sec)?.is_empty() && read_document(s.doc()?)?.auto_dim_sections.is_empty());
        Ok(())
    }

    #[test]
    fn a_section_window_above_a_second_level_measures_from_that_level() -> R {
        let (mut s, plan) = crate::schedule::tests::window_south()?;
        s.execute("newoffsetplan", &json!({ "mode": "offset", "fromViewId": plan.0, "floorToFloorMm": 3000, "name": "First" }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let sec = sectioned(&mut s, plan, &[(900.0, 2100.0), (3900.0, 5100.0)], 6000.0)?;
        let r = s.execute("autodim", &json!({ "view": sec.0 }))?;
        assert_eq!(r["message"], "4 opening dimensions");
        // The floor-to-floor level dimension (0 to 3000) sits clear, further out.
        let chains: Vec<_> = opening_dims(&s, sec)?.into_iter().filter(|d| d.0 == 700).collect();
        assert_eq!(chains, [(700, 0, 900), (700, 900, 2100), (700, 3000, 3900), (700, 3900, 5100)]);
        Ok(())
    }
}

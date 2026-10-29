//! Convex-polyhedron section and clipping by a half-space. Port of
//! `geometry-primitives/src/halfspace.ts`.
//!
//! `section_convex_polyhedron` and `clip_convex_polyhedron` build the cap by sorting cut points
//! around their centroid, which is valid only for a convex polyhedron. [`section_polyhedron`]
//! (LAMB T542, fork-only) clips face by face and chains the chords, so it follows a concave or
//! holed solid's real cross-section.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use super::point::{Point3, cross3, dot3, normalize3, sub3};
use super::tolerance::TOLERANCE;

/// `n . p <= d`, with `n` a unit normal pointing OUT of the kept side.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HalfSpace {
    pub n: Point3,
    pub d: f64,
}

fn signed_distance(plane: HalfSpace, p: Point3) -> f64 {
    dot3(plane.n, p) - plane.d
}

fn lerp_at(cur: Point3, next: Point3, t: f64) -> Point3 {
    Point3 { x: cur.x + (next.x - cur.x) * t, y: cur.y + (next.y - cur.y) * t, z: cur.z + (next.z - cur.z) * t }
}

/// Sutherland–Hodgman one face against one plane, reporting the edge it cut.
fn clip_face(face: &[Point3], plane: HalfSpace, cut_points: &mut Vec<Point3>) -> Vec<Point3> {
    let eps = TOLERANCE.coincidence_mm;
    let mut out = Vec::new();
    let n = face.len();
    for i in 0..n {
        let cur = face[i];
        let next = face[(i + 1) % n];
        let d_cur = signed_distance(plane, cur);
        let d_next = signed_distance(plane, next);
        let cur_in = d_cur <= eps;
        let next_in = d_next <= eps;

        if cur_in {
            out.push(cur);
        }
        if cur_in != next_in {
            let crossing = lerp_at(cur, next, d_cur / (d_cur - d_next));
            out.push(crossing);
            cut_points.push(crossing);
        } else if cur_in && d_cur.abs() <= eps && d_next.abs() <= eps {
            // A whole edge lying in the plane is itself part of the opening.
            cut_points.push(cur);
            cut_points.push(next);
        }
    }
    if out.len() >= 3 { out } else { Vec::new() }
}

fn same_point(a: Point3, b: Point3) -> bool {
    let (dx, dy, dz) = (b.x - a.x, b.y - a.y, b.z - a.z);
    (dx * dx + dy * dy + dz * dz).sqrt() <= TOLERANCE.coincidence_mm
}

/// Order coplanar points into a convex loop winding CCW about `+normal`.
fn loop_from_cut_points(points: &[Point3], normal: Point3) -> Vec<Point3> {
    let mut unique: Vec<Point3> = Vec::new();
    for &p in points {
        if !unique.iter().any(|&q| same_point(p, q)) {
            unique.push(p);
        }
    }
    if unique.len() < 3 {
        return Vec::new();
    }

    let len = unique.len() as f64;
    let centroid =
        unique.iter().fold(Point3 { x: 0.0, y: 0.0, z: 0.0 }, |acc, p| Point3 { x: acc.x + p.x / len, y: acc.y + p.y / len, z: acc.z + p.z / len });

    // Any two axes spanning the plane; pick a seed not parallel to n.
    let seed = if normal.x.abs() < 0.9 { Point3 { x: 1.0, y: 0.0, z: 0.0 } } else { Point3 { x: 0.0, y: 1.0, z: 0.0 } };
    let u = normalize3(cross3(normal, seed));
    let v = cross3(normal, u);

    let angle = |p: Point3| {
        let r = sub3(p, centroid);
        dot3(r, v).atan2(dot3(r, u))
    };
    let mut keyed: Vec<(f64, Point3)> = unique.into_iter().map(|p| (angle(p), p)).collect();
    keyed.sort_by(|a, b| (a.0 - b.0).partial_cmp(&0.0).unwrap_or(Ordering::Equal));
    keyed.into_iter().map(|(_, p)| p).collect()
}

/// The cross-section of a convex polyhedron by the plane `n . p == d`, wound CCW about `+n`.
/// Empty unless the solid has geometry STRICTLY on both sides.
pub fn section_convex_polyhedron(faces: &[Vec<Point3>], plane: HalfSpace) -> Vec<Point3> {
    let eps = TOLERANCE.coincidence_mm;
    let mut points = Vec::new();
    let mut any_above = false;
    let mut any_below = false;

    for face in faces {
        let n = face.len();
        for i in 0..n {
            let cur = face[i];
            let next = face[(i + 1) % n];
            let d_cur = signed_distance(plane, cur);
            let d_next = signed_distance(plane, next);

            if d_cur > eps {
                any_above = true;
            } else if d_cur < -eps {
                any_below = true;
            }

            if d_cur.abs() <= eps {
                points.push(cur);
            } else if (d_cur > eps && d_next < -eps) || (d_cur < -eps && d_next > eps) {
                points.push(lerp_at(cur, next, d_cur / (d_cur - d_next)));
            }
        }
    }

    if !any_above || !any_below {
        return Vec::new();
    }
    loop_from_cut_points(&points, plane.n)
}

/// Clip a closed set of face loops by one half-space. Empty when the solid lies entirely outside.
pub fn clip_convex_polyhedron(faces: &[Vec<Point3>], plane: HalfSpace) -> Vec<Vec<Point3>> {
    let mut cut_points = Vec::new();
    let mut kept = Vec::new();
    let mut anything_removed = false;

    for face in faces {
        if face.iter().any(|&p| signed_distance(plane, p) > TOLERANCE.coincidence_mm) {
            anything_removed = true;
        }
        let clipped = clip_face(face, plane, &mut cut_points);
        if clipped.len() >= 3 {
            kept.push(clipped);
        }
    }

    if kept.is_empty() {
        return Vec::new();
    }
    // A plane merely touching or containing a face leaves the solid alone.
    if !anything_removed {
        return kept;
    }

    let cap = loop_from_cut_points(&cut_points, plane.n);
    if cap.len() >= 3 {
        kept.push(cap);
    }
    kept
}

/// Newell's-method normal of a closed loop (not normalised; its length is twice the loop's area).
fn newell(points: &[Point3]) -> Point3 {
    let mut n = Point3 { x: 0.0, y: 0.0, z: 0.0 };
    for (i, a) in points.iter().enumerate() {
        let b = points[(i + 1) % points.len()];
        n.x += (a.y - b.y) * (a.z + b.z);
        n.y += (a.z - b.z) * (a.x + b.x);
        n.z += (a.x - b.x) * (a.y + b.y);
    }
    n
}

/// One face's chords on the plane: its edge crossings sorted along the plane–face line, paired.
/// A vertex is `above` when `signed_distance >= -eps`, so an on-plane vertex is counted once.
fn face_chords(face: &[Point3], plane: HalfSpace, eps: f64, chords: &mut Vec<(Point3, Point3)>) {
    let dists: Vec<f64> = face.iter().map(|&p| signed_distance(plane, p)).collect();
    if dists.iter().all(|d| d.abs() <= eps) {
        return;
    }
    let n = face.len();
    let mut crossings: Vec<Point3> = Vec::new();
    for (i, (&a, &da)) in face.iter().zip(&dists).enumerate() {
        let j = (i + 1) % n;
        let (Some(&b), Some(&db)) = (face.get(j), dists.get(j)) else { continue };
        if (da >= -eps) != (db >= -eps) {
            // Lerp from the lexicographically lower endpoint, so the edge's twin in the
            // neighbouring face lands on the bit-identical point.
            let (a, b, da, db) = if (a.x, a.y, a.z) <= (b.x, b.y, b.z) { (a, b, da, db) } else { (b, a, db, da) };
            crossings.push(lerp_at(a, b, da / (da - db)));
        }
    }
    if crossings.len() < 2 {
        return;
    }
    let dir = cross3(plane.n, newell(face));
    crossings.sort_by(|a, b| dot3(*a, dir).total_cmp(&dot3(*b, dir)));
    for &[a, b] in crossings.as_chunks::<2>().0 {
        if !same_point(a, b) {
            chords.push((a, b));
        }
    }
}

/// Join chords end to end into closed loops; anything that fails to close is dropped.
fn chain_chords(mut chords: Vec<(Point3, Point3)>) -> Vec<Vec<Point3>> {
    let mut loops = Vec::new();
    while let Some((start, mut cur)) = chords.pop() {
        let mut pts = vec![start];
        let closed = loop {
            if same_point(cur, start) {
                break true;
            }
            pts.push(cur);
            let Some(i) = chords.iter().position(|&(a, b)| same_point(a, cur) || same_point(b, cur)) else { break false };
            let (a, b) = chords.swap_remove(i);
            cur = if same_point(a, cur) { b } else { a };
        };
        if closed {
            loops.push(pts);
        }
    }
    loops
}

/// Drop coincident and collinear vertices of a closed loop.
fn simplify_loop(mut pts: Vec<Point3>) -> Vec<Point3> {
    let eps = TOLERANCE.coincidence_mm;
    loop {
        let n = pts.len();
        if n < 3 {
            return pts;
        }
        let drop = (0..n).find(|&i| {
            let (prev, cur, next) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
            let span = sub3(next, prev);
            let len = dot3(span, span).sqrt();
            if len <= eps {
                return same_point(cur, prev);
            }
            let c = cross3(span, sub3(cur, prev));
            dot3(c, c).sqrt() / len <= eps
        });
        match drop {
            Some(i) => {
                pts.remove(i);
            }
            None => return pts,
        }
    }
}

/// The cross-section of any closed polyhedron (convex, concave or holed) by the plane
/// `n . p == d`: one closed loop per boundary of the cut, each wound CCW about `+n` and started
/// where [`section_convex_polyhedron`] would start a convex one, ordered by descending area (ties:
/// lowest first vertex). A hole is its own loop. Empty unless the solid has geometry STRICTLY on
/// both sides; a face lying in the plane contributes nothing.
pub fn section_polyhedron(faces: &[Vec<Point3>], plane: HalfSpace) -> Vec<Vec<Point3>> {
    let eps = TOLERANCE.coincidence_mm;
    let (mut above, mut below) = (false, false);
    for d in faces.iter().flatten().map(|&p| signed_distance(plane, p)) {
        above |= d > eps;
        below |= d < -eps;
    }
    if !above || !below {
        return Vec::new();
    }

    let mut chords = Vec::new();
    for face in faces.iter().filter(|f| f.len() >= 3) {
        face_chords(face, plane, eps, &mut chords);
    }

    let seed = if plane.n.x.abs() < 0.9 { Point3 { x: 1.0, y: 0.0, z: 0.0 } } else { Point3 { x: 0.0, y: 1.0, z: 0.0 } };
    let u = normalize3(cross3(plane.n, seed));
    let v = cross3(plane.n, u);

    let mut loops: Vec<(f64, Vec<Point3>)> = chain_chords(chords)
        .into_iter()
        .map(simplify_loop)
        .filter(|l| l.len() >= 3)
        .map(|mut l| {
            let normal = newell(&l);
            if dot3(normal, plane.n) < 0.0 {
                l.reverse();
            }
            let len = l.len() as f64;
            let c = l.iter().fold(Point3 { x: 0.0, y: 0.0, z: 0.0 }, |acc, p| Point3 {
                x: acc.x + p.x / len,
                y: acc.y + p.y / len,
                z: acc.z + p.z / len,
            });
            let angle = |p: Point3| {
                let r = sub3(p, c);
                dot3(r, v).atan2(dot3(r, u))
            };
            let first = (0..l.len()).min_by(|&a, &b| angle(l[a]).total_cmp(&angle(l[b]))).unwrap_or(0);
            l.rotate_left(first);
            (dot3(normal, normal).sqrt() / 2.0, l)
        })
        .collect();
    loops.sort_by(|a, b| {
        b.0.total_cmp(&a.0).then_with(|| match (a.1.first(), b.1.first()) {
            (Some(p), Some(q)) => (p.x, p.y, p.z).partial_cmp(&(q.x, q.y, q.z)).unwrap_or(Ordering::Equal),
            _ => Ordering::Equal,
        })
    });
    loops.into_iter().map(|(_, l)| l).collect()
}

#[cfg(test)]
mod section_polyhedron_tests {
    use super::*;

    fn p(x: f64, y: f64, z: f64) -> Point3 {
        Point3 { x, y, z }
    }

    /// The 1000 mm cube's six faces.
    fn cube() -> Vec<Vec<Point3>> {
        let s = 1000.0;
        vec![
            vec![p(0.0, 0.0, 0.0), p(0.0, s, 0.0), p(s, s, 0.0), p(s, 0.0, 0.0)],
            vec![p(0.0, 0.0, s), p(s, 0.0, s), p(s, s, s), p(0.0, s, s)],
            vec![p(0.0, 0.0, 0.0), p(s, 0.0, 0.0), p(s, 0.0, s), p(0.0, 0.0, s)],
            vec![p(0.0, s, 0.0), p(0.0, s, s), p(s, s, s), p(s, s, 0.0)],
            vec![p(0.0, 0.0, 0.0), p(0.0, 0.0, s), p(0.0, s, s), p(0.0, s, 0.0)],
            vec![p(s, 0.0, 0.0), p(s, s, 0.0), p(s, s, s), p(s, 0.0, s)],
        ]
    }

    fn close(a: &[Point3], b: &[Point3]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(p, q)| same_point(*p, *q))
    }

    #[test]
    fn a_plane_through_two_edges_gives_the_convex_sections_rectangle() {
        // x + y == 1000 contains the edges x=0,y=1000 and x=1000,y=0: every corner on a vertex.
        let r = std::f64::consts::FRAC_1_SQRT_2;
        let plane = HalfSpace { n: p(r, r, 0.0), d: 1000.0 * r };
        let loops = section_polyhedron(&cube(), plane);
        assert_eq!(loops.len(), 1, "{loops:?}");
        assert!(close(&loops[0], &section_convex_polyhedron(&cube(), plane)), "{loops:?}");
        assert_eq!(loops[0].len(), 4);
    }

    #[test]
    fn a_plane_through_three_vertices_gives_a_triangle() {
        let r = 1.0 / 3f64.sqrt();
        let plane = HalfSpace { n: p(r, r, r), d: 1000.0 * r };
        let loops = section_polyhedron(&cube(), plane);
        assert_eq!(loops.len(), 1, "{loops:?}");
        assert_eq!(loops[0].len(), 3, "{loops:?}");
        assert!(close(&loops[0], &section_convex_polyhedron(&cube(), plane)), "{loops:?}");
    }

    #[test]
    fn a_face_plane_is_not_a_section() {
        assert!(section_polyhedron(&cube(), HalfSpace { n: p(0.0, 0.0, 1.0), d: 0.0 }).is_empty());
        assert!(section_polyhedron(&cube(), HalfSpace { n: p(0.0, 0.0, 1.0), d: 1000.0 }).is_empty());
    }
}

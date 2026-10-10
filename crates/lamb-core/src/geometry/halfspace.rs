//! Convex-polyhedron section and clipping by a half-space. Port of
//! `geometry-primitives/src/halfspace.ts`.
//!
//! The cap face is built by sorting cut points around their centroid, which is valid only because a
//! convex polyhedron's cross-section is convex. Non-convex input is rejected upstream.

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

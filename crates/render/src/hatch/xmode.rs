//! X hatch patterns (`X`, `X_ALIGNED`): two lines across each region of a hatch, trimmed to it.
//!
//! The fill loops are split into simple loops (a self-crossing hourglass is two triangles) and
//! nested by containment. Every loop at an even depth is a region: its two diagonals (its
//! bounding box's, or a quadrilateral's own corners in aligned mode) are cut where they cross it
//! or its islands, and a piece is kept when its middle lies inside the region, outside its
//! islands and inside the hatch by odd parity over every loop — so overlapping separate loops
//! leave their overlap bare and an island inside an island gets its own X.

use cadcraft_doc::library::XMode;
use cadcraft_geom::region::simple_loops;
use cadcraft_geom::{Bounds2, Vec2, point_in_polygon};

/// Upper bound on loop count × vertex count for nesting the simple loops (every loop is tested
/// against every other); past it every loop is treated as a region of its own.
const MAX_NEST_WORK: usize = 20_000_000;

/// The X runs (two-point runs, world coordinates) for the fill loops `loops`, at most `max_runs`.
pub(super) fn x_lines(loops: &[Vec<Vec2>], mode: XMode, max_runs: usize) -> Vec<Vec<Vec2>> {
    let bb = Bounds2::from_points(loops.iter().flatten().copied().filter(|p| p.is_finite()));
    if bb.is_empty() {
        return Vec::new();
    }
    let size = bb.width().max(bb.height());
    let tol = (size * 1e-9).max(1e-9);
    let simple: Vec<Vec<Vec2>> =
        loops.iter().flat_map(|l| simple_loops(&open(l, tol), tol)).map(|l| open(&l, tol)).filter(|l| l.len() >= 3).collect();
    let verts: usize = simple.iter().map(Vec::len).sum();
    let nest = simple.len().saturating_mul(verts) <= MAX_NEST_WORK;
    // Nesting is tested with a point on each loop (an edge's middle, as `fill_loops` does): a
    // point inside a loop could lie inside its own island.
    let probes: Vec<Option<Vec2>> = simple.iter().map(|l| super::probe(l)).collect();
    let inside = |j: usize, i: usize| -> bool {
        match (simple.get(j), probes.get(i).copied().flatten()) {
            (Some(o), Some(p)) => point_in_polygon(o, p),
            _ => false,
        }
    };
    let depth: Vec<usize> = (0..simple.len()).map(|i| if nest { (0..simple.len()).filter(|&j| j != i && inside(j, i)).count() } else { 0 }).collect();
    let boxes: Vec<Bounds2> = simple.iter().map(|l| Bounds2::from_points(l.iter().copied())).collect();
    let parity = |p: Vec2| simple.iter().filter(|l| point_in_polygon(l, p)).count() % 2 == 1;
    let mut out = Vec::new();
    for (i, outer) in simple.iter().enumerate() {
        let d = depth.get(i).copied().unwrap_or(0);
        if d % 2 == 1 {
            continue;
        }
        let children: Vec<&Vec<Vec2>> =
            (0..simple.len()).filter(|&j| j != i && depth.get(j) == Some(&(d + 1)) && inside(i, j)).filter_map(|j| simple.get(j)).collect();
        for (p, q) in diagonals(outer, mode, tol) {
            let mut ts = vec![0.0, 1.0];
            // Every loop can cut the diagonal: the region's own, its islands', and an overlapping
            // loop's, where the parity changes.
            let db = Bounds2::from_points([p, q]);
            for (l, lb) in simple.iter().zip(&boxes) {
                if lb.intersects(&db) {
                    crossings(l, p, q, &mut ts);
                }
            }
            ts.sort_by(f64::total_cmp);
            ts.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
            // Consecutive kept pieces are joined (a diagonal through a boundary vertex it only touches).
            let mut run: Option<(f64, f64)> = None;
            for w in ts.windows(2) {
                let [t0, t1] = w else { continue };
                let m = p + (q - p) * ((t0 + t1) / 2.0);
                let keep = point_in_polygon(outer, m) && !children.iter().any(|c| point_in_polygon(c, m)) && parity(m);
                match (keep, run) {
                    (true, Some((a, _))) => run = Some((a, *t1)),
                    (true, None) => run = Some((*t0, *t1)),
                    (false, Some((a, b))) => {
                        out.push(vec![p + (q - p) * a, p + (q - p) * b]);
                        run = None;
                    }
                    (false, None) => {}
                }
            }
            if let Some((a, b)) = run {
                out.push(vec![p + (q - p) * a, p + (q - p) * b]);
            }
            if out.len() >= max_runs {
                out.truncate(max_runs);
                return out;
            }
        }
    }
    out
}

/// `l` without a closing duplicate of its first vertex.
fn open(l: &[Vec2], tol: f64) -> Vec<Vec2> {
    let mut v = l.to_vec();
    while v.len() > 1 && v.first().zip(v.last()).is_some_and(|(a, b)| a.near(*b, tol)) {
        v.pop();
    }
    v
}

/// The two diagonals of a region.
fn diagonals(outer: &[Vec2], mode: XMode, tol: f64) -> [(Vec2, Vec2); 2] {
    if mode == XMode::Aligned
        && let [a, b, c, d] = corners(outer, tol).as_slice()
    {
        return [(*a, *c), (*b, *d)];
    }
    let bb = Bounds2::from_points(outer.iter().copied());
    [(bb.min, bb.max), (Vec2::new(bb.min.x, bb.max.y), Vec2::new(bb.max.x, bb.min.y))]
}

/// The vertices of `l` where it turns (repeated and collinear vertices dropped).
fn corners(l: &[Vec2], tol: f64) -> Vec<Vec2> {
    let mut v: Vec<Vec2> = Vec::with_capacity(l.len());
    for p in l {
        if v.last().is_none_or(|q| !q.near(*p, tol)) {
            v.push(*p);
        }
    }
    let v = open(&v, tol);
    let n = v.len();
    (0..n)
        .filter_map(|i| {
            let (a, b, c) = (*v.get((i + n - 1) % n)?, *v.get(i)?, *v.get((i + 1) % n)?);
            let (u, w) = (b - a, c - b);
            let scale = u.len() * w.len();
            (u.cross(w).abs() > 1e-9 * scale).then_some(b)
        })
        .collect()
}

/// Push the parameters along `p`→`q` (strictly between its ends) where it crosses an edge of the
/// closed loop `l`.
fn crossings(l: &[Vec2], p: Vec2, q: Vec2, ts: &mut Vec<f64>) {
    let d = q - p;
    let n = l.len();
    for i in 0..n {
        let (Some(&a), Some(&b)) = (l.get(i), l.get((i + 1) % n)) else { continue };
        let e = b - a;
        let den = d.cross(e);
        if den.abs() <= 1e-12 * d.len() * e.len() {
            continue;
        }
        let t = (a - p).cross(e) / den;
        let u = (a - p).cross(d) / den;
        if (-1e-12..=1.0 + 1e-12).contains(&u) && t > 0.0 && t < 1.0 && t.is_finite() {
            ts.push(t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::pattern_lines;
    use cadcraft_doc::{Drawing, Hatch};
    use cadcraft_geom::Vec2;

    fn v(x: f64, y: f64) -> Vec2 {
        Vec2::new(x, y)
    }

    fn hatch(pattern: &str) -> Hatch {
        Hatch {
            pattern: pattern.into(),
            solid: false,
            loops: Vec::new(),
            scale: 1.0,
            angle: 0.0,
            associative: false,
            style: 0,
            elevation: 0.0,
            gradient: None,
            origin: Vec2::ZERO,
            background: None,
            pattern_lines: Vec::new(),
        }
    }

    fn runs(pattern: &str, loops: &[Vec<Vec2>]) -> Vec<Vec<Vec2>> {
        let r = pattern_lines(&Drawing::default(), &hatch(pattern), loops, 0.0, None);
        assert!(r.iter().all(|r| r.len() == 2), "{r:?}");
        r
    }

    /// True when `r` runs from `a` to `b` (either way) within 1e-6.
    fn is_run(r: &[Vec2], a: Vec2, b: Vec2) -> bool {
        match r {
            [p, q] => (p.near(a, 1e-6) && q.near(b, 1e-6)) || (p.near(b, 1e-6) && q.near(a, 1e-6)),
            _ => false,
        }
    }

    fn has(rs: &[Vec<Vec2>], a: Vec2, b: Vec2) -> bool {
        rs.iter().any(|r| is_run(r, a, b))
    }

    fn rect(x0: f64, y0: f64, w: f64, h: f64) -> Vec<Vec2> {
        vec![v(x0, y0), v(x0 + w, y0), v(x0 + w, y0 + h), v(x0, y0 + h)]
    }

    #[test]
    fn a_rectangle_gets_its_two_diagonals() {
        let rs = runs("X", &[rect(0.0, 0.0, 38.0, 89.0)]);
        assert_eq!(rs.len(), 2, "{rs:?}");
        assert!(has(&rs, v(0.0, 0.0), v(38.0, 89.0)));
        assert!(has(&rs, v(0.0, 89.0), v(38.0, 0.0)));
    }

    #[test]
    fn a_rotated_rectangle_trims_bbox_diagonals_or_uses_its_own_corners() {
        let (s, c) = 20f64.to_radians().sin_cos();
        let rot = |p: Vec2| v(p.x * c - p.y * s, p.x * s + p.y * c);
        let r: Vec<Vec2> = rect(0.0, 0.0, 38.0, 89.0).into_iter().map(rot).collect();
        let aligned = runs("x_aligned", std::slice::from_ref(&r));
        assert_eq!(aligned.len(), 2, "{aligned:?}");
        assert!(has(&aligned, r[0], r[2]) && has(&aligned, r[1], r[3]));
        let bounds = runs("X", std::slice::from_ref(&r));
        assert_eq!(bounds.len(), 2, "{bounds:?}");
        let bb = cadcraft_geom::Bounds2::from_points(r.iter().copied());
        for run in &bounds {
            // On a bbox diagonal, both ends on the rectangle's boundary.
            let (p, q) = (run[0], run[1]);
            let on_diag =
                |d0: Vec2, d1: Vec2| (p - d0).cross(d1 - d0).abs() < 1e-6 * (d1 - d0).len() && (q - d0).cross(d1 - d0).abs() < 1e-6 * (d1 - d0).len();
            assert!(on_diag(bb.min, bb.max) || on_diag(v(bb.min.x, bb.max.y), v(bb.max.x, bb.min.y)), "{run:?}");
            for e in [p, q] {
                let on_edge = (0..4).any(|i| {
                    let (a, b) = (r[i], r[(i + 1) % 4]);
                    let t = (e - a).dot(b - a) / (b - a).len2();
                    (-1e-9..=1.0 + 1e-9).contains(&t) && (a + (b - a) * t).near(e, 1e-6)
                });
                assert!(on_edge, "{e:?} not on the rectangle");
            }
        }
    }

    #[test]
    fn an_l_shape_splits_a_diagonal_in_two() {
        // L: 10×10 with the top-right 6×6 cut away; the anti-diagonal (0,10)→(10,0) leaves and re-enters.
        let l = vec![v(0.0, 0.0), v(10.0, 0.0), v(10.0, 4.0), v(4.0, 4.0), v(4.0, 10.0), v(0.0, 10.0)];
        let rs = runs("X", &[l]);
        assert!(has(&rs, v(0.0, 0.0), v(4.0, 4.0)), "{rs:?}");
        assert!(has(&rs, v(0.0, 10.0), v(4.0, 6.0)), "{rs:?}");
        assert!(has(&rs, v(6.0, 4.0), v(10.0, 0.0)), "{rs:?}");
        assert_eq!(rs.len(), 3, "{rs:?}");
    }

    #[test]
    fn an_hourglass_loop_is_two_triangles_each_with_its_own_x() {
        let hg = vec![v(0.0, 0.0), v(10.0, 10.0), v(10.0, 0.0), v(0.0, 10.0)];
        let rs = runs("X", &[hg]);
        assert_eq!(rs.len(), 4, "{rs:?}");
        // Left triangle (0,0) (5,5) (0,10), bbox x 0..5: diagonals trimmed to it.
        assert!(has(&rs, v(0.0, 0.0), v(10.0 / 3.0, 20.0 / 3.0)), "{rs:?}");
        assert!(has(&rs, v(0.0, 10.0), v(10.0 / 3.0, 10.0 / 3.0)), "{rs:?}");
        // Right triangle (10,10) (10,0) (5,5), bbox x 5..10.
        assert!(has(&rs, v(20.0 / 3.0, 10.0 / 3.0), v(10.0, 10.0)), "{rs:?}");
        assert!(has(&rs, v(20.0 / 3.0, 20.0 / 3.0), v(10.0, 0.0)), "{rs:?}");
    }

    #[test]
    fn an_island_leaves_a_gap_and_an_island_in_it_gets_its_own_x() {
        let outer = rect(0.0, 0.0, 10.0, 10.0);
        let island = rect(3.0, 3.0, 4.0, 4.0);
        let rs = runs("X", &[outer.clone(), island.clone()]);
        assert_eq!(rs.len(), 4, "{rs:?}");
        assert!(has(&rs, v(0.0, 0.0), v(3.0, 3.0)) && has(&rs, v(7.0, 7.0), v(10.0, 10.0)), "{rs:?}");
        assert!(has(&rs, v(0.0, 10.0), v(3.0, 7.0)) && has(&rs, v(7.0, 3.0), v(10.0, 0.0)), "{rs:?}");
        let inner = rect(4.0, 4.0, 2.0, 2.0);
        let rs = runs("X", &[outer, island, inner]);
        assert_eq!(rs.len(), 6, "{rs:?}");
        assert!(has(&rs, v(4.0, 4.0), v(6.0, 6.0)) && has(&rs, v(4.0, 6.0), v(6.0, 4.0)), "{rs:?}");
        assert!(has(&rs, v(0.0, 0.0), v(3.0, 3.0)), "{rs:?}");
    }

    #[test]
    fn overlapping_loops_leave_their_overlap_bare() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(6.0, 3.0, 10.0, 10.0);
        let rs = runs("X", &[a, b]);
        assert!(!rs.is_empty());
        for r in &rs {
            let m = (r[0] + r[1]) * 0.5;
            let in_overlap = m.x > 6.0 + 1e-9 && m.x < 10.0 - 1e-9 && m.y > 3.0 + 1e-9 && m.y < 10.0 - 1e-9;
            assert!(!in_overlap, "{r:?} runs through the overlap");
        }
        // The first square's main diagonal stops where it enters the second.
        assert!(has(&rs, v(0.0, 0.0), v(6.0, 6.0)), "{rs:?}");
    }

    #[test]
    fn a_plotted_x_hatch_carries_its_diagonals() {
        use cadcraft_doc::{Common, EntityKind, HatchLoop, Space};
        let mut d = Drawing::new_metric();
        let vertices = rect(0.0, 0.0, 38.0, 89.0).into_iter().map(cadcraft_geom::PolyVertex::new).collect();
        let h = Hatch { loops: vec![HatchLoop { vertices, outer: true, sources: Vec::new() }], ..hatch("X") };
        d.add(&Space::Model, Common::default(), EntityKind::Hatch(h)).expect("add");
        let l = crate::build_plot(&d, &Space::Model, &crate::Options::default());
        let lines: Vec<Vec<Vec2>> = l.prims.iter().filter(|p| p.kind == crate::Kind::Polyline).map(|p| l.points(p).to_vec()).collect();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(has(&lines, v(0.0, 0.0), v(38.0, 89.0)) && has(&lines, v(0.0, 89.0), v(38.0, 0.0)), "{lines:?}");
    }

    #[test]
    fn an_x_hatch_never_reads_solid() {
        let h = hatch("X");
        assert!(!super::super::reads_solid(&Drawing::default(), &h, 100.0));
    }
}

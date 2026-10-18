//! The wall graph, first slice: **runs** (LAMB's `docs/07-architecture/cadcraft-integration.md` §8,
//! phase 7). In one view, two parallel cut-role face lines on wall layers a plausible wall width
//! apart, with overlapping extents, form a derived run: both face anchors, the width and the
//! centreline. Computed on demand, never stored, never displayed. Junctions
//! ([`super::wall_junctions`]) and opening bridging ([`super::wall_openings`]) build on it; the
//! exterior side is a later slice.
//!
//! Pairing is nearest-partner only: a pair is dropped when a third face of the same kind lies
//! strictly between the two and overlaps the pair's interval. So a cavity wall drawn as three
//! parallel lines gives two thin runs (outer leaf, inner leaf), never one run spanning both — an
//! accepted limitation until a later slice groups leaves into one wall.

use std::f64::consts::PI;

use super::read::ProjectRead;
use crate::geometry::Point2;
use crate::geometry::point::{cross2, dist2, dot2, lerp2, normalize2, sub2};
use crate::geometry::polyline::{arc_from_bulge, is_arc, resolved_segments, sweep_angle};
use crate::geometry::tolerance::{TOLERANCE, deg_to_rad};
use crate::model::{DrawingLayer, Id, LineRole, LinkAnchor};

/// What counts as a wall face and a plausible wall.
#[derive(Debug, Clone, PartialEq)]
pub struct WallRunParams {
    pub wall_layer_ids: Vec<String>,
    pub min_width_mm: f64,
    pub max_width_mm: f64,
    /// Minimum shared length (along the first face; on the centreline radius for arcs).
    pub min_overlap_mm: f64,
    /// Layers whose geometry is a drawn window or door, for opening bridging
    /// ([`super::wall_openings`]). Role is ignored.
    pub opening_layer_ids: Vec<String>,
}

impl WallRunParams {
    /// Wall layers are those whose name starts with `WALL` (ASCII case-insensitive): `WALL-EXT`
    /// and `WALL-INT` in `default_layers`. Width 50–800 mm, overlap ≥ `TOLERANCE.grouping_mm`.
    /// Opening layers are those whose name starts with `WINDOW` or `DOOR` (none in `default_layers`).
    pub fn defaults(layers: &[DrawingLayer]) -> Self {
        let starts = |name: &str, prefix: &[u8]| name.len() >= prefix.len() && name.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix);
        let ids = |pred: &dyn Fn(&str) -> bool| layers.iter().filter(|l| pred(&l.name)).map(|l| l.id.clone()).collect::<Vec<_>>();
        WallRunParams {
            wall_layer_ids: ids(&|n| starts(n, b"WALL")),
            min_width_mm: 50.0,
            max_width_mm: 800.0,
            min_overlap_mm: TOLERANCE.grouping_mm,
            opening_layer_ids: ids(&|n| starts(n, b"WINDOW") || starts(n, b"DOOR")),
        }
    }

    /// [`Self::defaults`] for a vertical (section/elevation) view: wall layers are those whose name
    /// starts with `WALL`, `FLOOR`, `ROOF` or `SLAB` (ASCII case-insensitive) — walls, floors and
    /// roofs the cut passes through. Opening layers as in `defaults`.
    pub fn for_vertical(layers: &[DrawingLayer]) -> Self {
        let starts = |name: &str, prefix: &[u8]| name.len() >= prefix.len() && name.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix);
        let wall_layer_ids = layers
            .iter()
            .filter(|l| [b"WALL".as_slice(), b"FLOOR", b"ROOF", b"SLAB"].iter().any(|p| starts(&l.name, p)))
            .map(|l| l.id.clone())
            .collect();
        WallRunParams { wall_layer_ids, ..Self::defaults(layers) }
    }
}

/// A run's centreline, in the view's local coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RunCurve {
    Line {
        from: Point2,
        to: Point2,
    },
    /// Angles in radians; `sweep` > 0 is counterclockwise.
    Arc {
        center: Point2,
        radius: f64,
        start_angle: f64,
        sweep: f64,
    },
}

/// The run's extent on one face: the overlap interval's two ends on that face's own segment. On a
/// face antiparallel to `faces[0]`, `from.segment_param > to.segment_param` — intended.
#[derive(Debug, Clone, PartialEq)]
pub struct RunFace {
    pub from: LinkAnchor,
    pub to: LinkAnchor,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WallRun {
    pub view_id: Id,
    /// `faces[0]` is the face whose `(entity_id, segment_index)` sorts first; the centreline runs
    /// in its direction.
    pub faces: [RunFace; 2],
    pub width: f64,
    pub centreline: RunCurve,
}

#[derive(Clone, Copy)]
enum FaceShape {
    /// Unit direction and length.
    Line { dir: Point2, len: f64 },
    /// `lo` is the counterclockwise-first angle; the arc covers `[lo, lo + span]`.
    Arc { center: Point2, radius: f64, lo: f64, span: f64, ccw: bool },
}

#[derive(Clone, Copy)]
struct Face {
    entity_id: Id,
    index: usize,
    from: Point2,
    to: Point2,
    shape: FaceShape,
}

impl Face {
    fn key(&self) -> (Id, usize) {
        (self.entity_id, self.index)
    }
    fn anchor(&self, segment_param: f64) -> LinkAnchor {
        LinkAnchor { entity_id: self.entity_id, segment_index: self.index, segment_param }
    }
}

fn candidate_faces(p: &impl ProjectRead, view: Id, params: &WallRunParams) -> Vec<Face> {
    let mut faces = Vec::new();
    for e in p.entities(view) {
        let Some(poly) = e.polyline.as_ref() else { continue };
        if p.role_of(&e) != LineRole::Cut || !params.wall_layer_ids.iter().any(|id| id == e.layer_id) {
            continue;
        }
        for seg in resolved_segments(poly) {
            if dist2(seg.from, seg.to) < TOLERANCE.coincidence_mm {
                continue;
            }
            let arc = if is_arc(seg.bulge) { arc_from_bulge(seg.from, seg.to, seg.bulge) } else { None };
            let shape = match arc {
                Some(a) => {
                    let sweep = sweep_angle(a.start_angle, a.end_angle, a.ccw);
                    let lo = if a.ccw { a.start_angle } else { a.end_angle };
                    FaceShape::Arc { center: a.center, radius: a.radius, lo, span: sweep.abs(), ccw: a.ccw }
                }
                None => FaceShape::Line { dir: normalize2(sub2(seg.to, seg.from)), len: dist2(seg.from, seg.to) },
            };
            faces.push(Face { entity_id: e.id, index: seg.index, from: seg.from, to: seg.to, shape });
        }
    }
    faces.sort_by_key(Face::key);
    faces
}

/// Length of `[a0, a1] ∩ [b0, b1]` (negative when disjoint), and its ends.
fn overlap(a0: f64, a1: f64, b0: f64, b1: f64) -> (f64, f64, f64) {
    let lo = a0.max(b0);
    let hi = a1.min(b1);
    (hi - lo, lo, hi)
}

/// Face `f` in line face `a`'s frame: its extent projected onto `a`'s axis (`t` from `a.from`),
/// low then high, and its midpoint's signed offset from `a`'s line.
fn line_frame(a: &Face, a_dir: Point2, f: &Face) -> (f64, f64, f64) {
    let s0 = dot2(sub2(f.from, a.from), a_dir);
    let s1 = dot2(sub2(f.to, a.from), a_dir);
    let mid = lerp2(f.from, f.to, 0.5);
    (s0.min(s1), s0.max(s1), cross2(a_dir, sub2(mid, a.from)))
}

fn pair_lines(view: Id, a: &Face, b: &Face, faces: &[Face], params: &WallRunParams) -> Option<WallRun> {
    let (FaceShape::Line { dir: da, len: la }, FaceShape::Line { dir: db, .. }) = (a.shape, b.shape) else { return None };
    let sin_tol = deg_to_rad(TOLERANCE.angle_deg).sin();
    if cross2(da, db).abs() > sin_tol {
        return None;
    }
    let (b_lo, b_hi, offset) = line_frame(a, da, b);
    let width = offset.abs();
    if width < params.min_width_mm || width > params.max_width_mm {
        return None;
    }
    let (len, lo, hi) = overlap(0.0, la, b_lo, b_hi);
    if len < params.min_overlap_mm {
        return None;
    }
    let side = offset.signum();
    let between = faces.iter().any(|k| {
        if k.key() == a.key() || k.key() == b.key() {
            return false;
        }
        let FaceShape::Line { dir: dk, .. } = k.shape else { return false };
        if cross2(da, dk).abs() > sin_tol {
            return false;
        }
        let (k_lo, k_hi, k_off) = line_frame(a, da, k);
        let d = side * k_off;
        d > TOLERANCE.coincidence_mm && d < width - TOLERANCE.coincidence_mm && overlap(lo, hi, k_lo, k_hi).0 > TOLERANCE.coincidence_mm
    });
    if between {
        return None;
    }
    // b's own param at a's axis position t.
    let b_s0 = dot2(sub2(b.from, a.from), da);
    let b_s1 = dot2(sub2(b.to, a.from), da);
    let b_param = |t: f64| (t - b_s0) / (b_s1 - b_s0);
    let a_from = lerp2(a.from, a.to, lo / la);
    let a_to = lerp2(a.from, a.to, hi / la);
    let b_from = lerp2(b.from, b.to, b_param(lo));
    let b_to = lerp2(b.from, b.to, b_param(hi));
    Some(WallRun {
        view_id: view,
        faces: [RunFace { from: a.anchor(lo / la), to: a.anchor(hi / la) }, RunFace { from: b.anchor(b_param(lo)), to: b.anchor(b_param(hi)) }],
        width,
        centreline: RunCurve::Line { from: lerp2(a_from, b_from, 0.5), to: lerp2(a_to, b_to, 0.5) },
    })
}

/// `x` reduced into `[0, 2π)`.
fn wrap(x: f64) -> f64 {
    x.rem_euclid(2.0 * PI)
}

/// Angular overlap of arc `f`'s span with `[0, span_a]`, in `a`'s frame (angles relative to
/// `a`'s `lo`): `(length, lo, hi, f's own lo in that frame)`. The better of the two wraps wins.
fn arc_frame(a_lo: f64, span_a: f64, f_lo: f64, f_span: f64) -> (f64, f64, f64, f64) {
    let rel = wrap(f_lo - a_lo);
    let first = overlap(0.0, span_a, rel, rel + f_span);
    let second = overlap(0.0, span_a, rel - 2.0 * PI, rel - 2.0 * PI + f_span);
    if second.0 > first.0 { (second.0, second.1, second.2, rel - 2.0 * PI) } else { (first.0, first.1, first.2, rel) }
}

fn arc_param(span: f64, ccw: bool, u: f64) -> f64 {
    if ccw { u / span } else { (span - u) / span }
}

fn pair_arcs(view: Id, a: &Face, b: &Face, faces: &[Face], params: &WallRunParams) -> Option<WallRun> {
    let (
        FaceShape::Arc { center: ca, radius: ra, lo: a_lo, span: a_span, ccw: a_ccw },
        FaceShape::Arc { center: cb, radius: rb, lo: b_lo, span: b_span, ccw: b_ccw },
    ) = (a.shape, b.shape)
    else {
        return None;
    };
    if dist2(ca, cb) > TOLERANCE.coincidence_mm {
        return None;
    }
    let width = (ra - rb).abs();
    if width < params.min_width_mm || width > params.max_width_mm {
        return None;
    }
    let rc = (ra + rb) / 2.0;
    let (len, lo, hi, b_rel) = arc_frame(a_lo, a_span, b_lo, b_span);
    if len * rc < params.min_overlap_mm {
        return None;
    }
    let (r_min, r_max) = (ra.min(rb), ra.max(rb));
    let between = faces.iter().any(|k| {
        if k.key() == a.key() || k.key() == b.key() {
            return false;
        }
        let FaceShape::Arc { center: ck, radius: rk, lo: k_lo, span: k_span, .. } = k.shape else { return false };
        if dist2(ca, ck) > TOLERANCE.coincidence_mm || rk <= r_min + TOLERANCE.coincidence_mm || rk >= r_max - TOLERANCE.coincidence_mm {
            return false;
        }
        let (_, _, _, k_rel) = arc_frame(a_lo, a_span, k_lo, k_span);
        let shared = overlap(lo, hi, k_rel, k_rel + k_span).0.max(overlap(lo, hi, k_rel + 2.0 * PI, k_rel + 2.0 * PI + k_span).0);
        shared * rc > TOLERANCE.coincidence_mm
    });
    if between {
        return None;
    }
    // In a's direction: from `lo` to `hi` when a is ccw, else `hi` to `lo`.
    let (t_from, t_to) = if a_ccw { (lo, hi) } else { (hi, lo) };
    let center = lerp2(ca, cb, 0.5);
    let start_angle = a_lo + t_from;
    let start_angle = if start_angle > PI { start_angle - 2.0 * PI } else { start_angle };
    Some(WallRun {
        view_id: view,
        faces: [
            RunFace { from: a.anchor(arc_param(a_span, a_ccw, t_from)), to: a.anchor(arc_param(a_span, a_ccw, t_to)) },
            RunFace { from: b.anchor(arc_param(b_span, b_ccw, t_from - b_rel)), to: b.anchor(arc_param(b_span, b_ccw, t_to - b_rel)) },
        ],
        width,
        centreline: RunCurve::Arc { center, radius: rc, start_angle, sweep: t_to - t_from },
    })
}

/// Every wall run in `view`, sorted by `(faces[0], faces[1])` keys. Candidates are the view's
/// polyline entities with a resolved `Cut` role on a layer in `params.wall_layer_ids`; each
/// segment (true corners — virtual corner tags are ignored, as snap ignores them) is a face. Lines
/// pair with lines, concentric arcs with arcs; a straight face never pairs with an arc.
pub fn wall_runs(p: &impl ProjectRead, view: Id, params: &WallRunParams) -> Vec<WallRun> {
    let faces = candidate_faces(p, view, params);
    let mut runs = Vec::new();
    for (i, a) in faces.iter().enumerate() {
        for b in &faces[i + 1..] {
            let run = match (a.shape, b.shape) {
                (FaceShape::Line { .. }, FaceShape::Line { .. }) => pair_lines(view, a, b, &faces, params),
                (FaceShape::Arc { .. }, FaceShape::Arc { .. }) => pair_arcs(view, a, b, &faces, params),
                _ => None,
            };
            runs.extend(run);
        }
    }
    let key = |r: &WallRun| {
        let [f0, f1] = &r.faces;
        (f0.from.entity_id, f0.from.segment_index, f1.from.entity_id, f1.from.segment_index)
    };
    runs.sort_by_key(key);
    runs
}

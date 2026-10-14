//! Assembly lines (LAMB's `docs/07-architecture/cadcraft-integration.md` §10 "Generation", steps 1,
//! 2 and 4, straight runs only; LAMB T369): a detail view's **continuous bands** — its straight
//! lines parallel to the reference line — laid along a wall run, aligned on the run's matching
//! face — and each **repeat** (LAMB T380) arrayed at its pitch along the run ([`lay_repeat`]).
//! Junctions are not trimmed by default; for a plan that opts into mitring (§10 step 3, LAMB T385)
//! the adapter moves band ends at undetailed two-arm junctions onto [`junction_bisector`] with
//! [`mitre_end`]. Computed on demand.
//!
//! Not the AI-line sweep (`crate::sweeps` in the adapter); everything here is "assembly".

use super::wall_junctions::{Junction, RunEnd};
use super::wall_spaces::{RunSides, SideKind};
use super::walls::{RunCurve, WallRun};
use crate::geometry::Point2;
use crate::geometry::tolerance::TOLERANCE;

/// One continuous band of a detail: a line parallel to the reference, `offset` mm from it,
/// positive into the wall.
#[derive(Debug, Clone, PartialEq)]
pub struct Band {
    pub offset: f64,
    pub layer: String,
}

/// Which face of a run the detail's reference line sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceRef {
    Exterior,
    Interior,
}

/// One laid band: a straight line in the plan view's local space.
#[derive(Debug, Clone, PartialEq)]
pub struct AssemblyLine {
    pub from: Point2,
    pub to: Point2,
    pub layer: String,
}

/// A detail's own frame: `origin` is the reference line's `from`, `along` its unit direction and
/// `into` the unit normal pointing to the wall side, as [`detail_frame`] decides it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetailFrame {
    pub origin: Point2,
    pub along: Point2,
    pub into: Point2,
}

impl DetailFrame {
    /// `p` as `(t along, o into)`.
    fn local(&self, p: Point2) -> (f64, f64) {
        let (dx, dy) = (p.x - self.origin.x, p.y - self.origin.y);
        (dx * self.along.x + dy * self.along.y, dx * self.into.x + dy * self.into.y)
    }
}

/// Every line of `lines` parallel to the unit direction `u` (either direction, within
/// `TOLERANCE.angle_deg`), its offset the signed distance of its midpoint from `a` along `n`.
fn parallel_offsets(a: Point2, u: Point2, n: Point2, lines: &[(Point2, Point2, String)]) -> Vec<Band> {
    let max_sin = TOLERANCE.angle_deg.to_radians().sin();
    let mut bands = Vec::new();
    for (p, q, layer) in lines {
        let (lx, ly) = (q.x - p.x, q.y - p.y);
        let l = lx.hypot(ly);
        if l <= TOLERANCE.coincidence_mm || (u.x * ly - u.y * lx).abs() / l > max_sin {
            continue;
        }
        let (mx, my) = ((p.x + q.x) / 2.0, (p.y + q.y) / 2.0);
        bands.push(Band { offset: (mx - a.x) * n.x + (my - a.y) * n.y, layer: layer.clone() });
    }
    bands
}

/// The frame of a detail whose reference line runs `reference.0 → reference.1`. The wall side is
/// the side of the band line ([`detail_bands`]) farthest from the reference; a tie between the two
/// sides, or no bands at all, is an error rather than a guess.
pub fn detail_frame(reference: (Point2, Point2), lines: &[(Point2, Point2, String)]) -> Result<DetailFrame, String> {
    let (a, b) = reference;
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = dx.hypot(dy);
    if len <= TOLERANCE.coincidence_mm {
        return Err("the reference line has no length".into());
    }
    let along = Point2 { x: dx / len, y: dy / len };
    let left_normal = Point2 { x: -along.y, y: along.x };
    let bands = parallel_offsets(a, along, left_normal, lines);
    let far = |sign: f64| bands.iter().map(|b| b.offset * sign).filter(|o| *o > 0.0).fold(0.0, f64::max);
    let (left, right) = (far(1.0), far(-1.0));
    if bands.is_empty() || (left - right).abs() <= TOLERANCE.coincidence_mm {
        return Err("the detail's wall side is ambiguous: its farthest bands on each side of the reference tie".into());
    }
    let into = if right > left { Point2 { x: along.y, y: -along.x } } else { left_normal };
    Ok(DetailFrame { origin: a, along, into })
}

/// The bands of a detail whose reference line runs `reference.0 → reference.1`: every line parallel
/// to it (either direction, within `TOLERANCE.angle_deg`), its offset the signed distance of its
/// midpoint toward the wall side ([`detail_frame`], whose errors it shares). Sorted by offset,
/// duplicates (same offset within `TOLERANCE.coincidence_mm`, same layer) collapsed.
pub fn detail_bands(reference: (Point2, Point2), lines: &[(Point2, Point2, String)]) -> Result<Vec<Band>, String> {
    let f = detail_frame(reference, lines)?;
    let mut bands = parallel_offsets(f.origin, f.along, f.into, lines);
    bands.sort_by(|a, b| a.offset.total_cmp(&b.offset).then_with(|| a.layer.cmp(&b.layer)));
    let mut out: Vec<Band> = Vec::new();
    for b in bands {
        if !out.iter().any(|o| o.layer == b.layer && (o.offset - b.offset).abs() <= TOLERANCE.coincidence_mm) {
            out.push(b);
        }
    }
    Ok(out)
}

/// A straight run's face frame: `from`, `to`, unit direction and length of the centreline, and the
/// unit normal toward the `face` side (the `Outside` side for `Exterior`, the other for
/// `Interior`). A detail point `(t along, o into)` lands at `from + dir·t + normal·(width/2 − o)`.
struct FaceFrame {
    from: Point2,
    to: Point2,
    dir: Point2,
    len: f64,
    normal: Point2,
}

impl FaceFrame {
    fn place(&self, width: f64, t: f64, o: f64) -> Point2 {
        let d = width / 2.0 - o;
        Point2 { x: self.from.x + self.dir.x * t + self.normal.x * d, y: self.from.y + self.dir.y * t + self.normal.y * d }
    }
}

fn face_frame(run: &WallRun, sides: &RunSides, face: FaceRef) -> Result<FaceFrame, String> {
    let RunCurve::Line { from, to } = run.centreline else { return Err("arc runs are not laid yet".into()) };
    let outside_left = match (sides.left, sides.right) {
        (SideKind::Outside, SideKind::Space(_)) => true,
        (SideKind::Space(_), SideKind::Outside) => false,
        _ => return Err("no single exterior side".into()),
    };
    let toward_left = outside_left == (face == FaceRef::Exterior);
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let len = dx.hypot(dy);
    if len <= TOLERANCE.coincidence_mm {
        return Err("the run has no length".into());
    }
    let s = if toward_left { 1.0 } else { -1.0 };
    Ok(FaceFrame { from, to, dir: Point2 { x: dx / len, y: dy / len }, len, normal: Point2 { x: -dy / len * s, y: dx / len * s } })
}

/// `bands` laid along a straight `run`: the face line is the centreline offset `width / 2` toward
/// the `face` side, each band that face line shifted `offset` back toward the centreline, over the
/// centreline's extent.
pub fn lay_bands(run: &WallRun, sides: &RunSides, face: FaceRef, bands: &[Band]) -> Result<Vec<AssemblyLine>, String> {
    let f = face_frame(run, sides, face)?;
    Ok(bands
        .iter()
        .map(|b| {
            let d = run.width / 2.0 - b.offset;
            let shift = |p: Point2| Point2 { x: p.x + f.normal.x * d, y: p.y + f.normal.y * d };
            AssemblyLine { from: shift(f.from), to: shift(f.to), layer: b.layer.clone() }
        })
        .collect())
}

/// A repeat's `members` (straight segments in the detail's space, with their layers) arrayed along
/// a straight `run`: copies shifted `k·|p|` along, `p` being `pitch`'s component along the
/// reference, for every integer `k` whose copy's along-extent lies within `[0, run length]`
/// (± `TOLERANCE.coincidence_mm`). Points map through the same face frame as [`lay_bands`], the
/// reference's `from` at the centreline's `from`. No along component, no members, or no copy that
/// fits gives no lines.
pub fn lay_repeat(
    run: &WallRun,
    sides: &RunSides,
    face: FaceRef,
    frame: &DetailFrame,
    members: &[(Point2, Point2, String)],
    pitch: Point2,
) -> Result<Vec<AssemblyLine>, String> {
    let f = face_frame(run, sides, face)?;
    let p = (pitch.x * frame.along.x + pitch.y * frame.along.y).abs();
    if p <= TOLERANCE.coincidence_mm || members.is_empty() {
        return Ok(Vec::new());
    }
    let local: Vec<_> = members.iter().map(|(a, b, layer)| (frame.local(*a), frame.local(*b), layer)).collect();
    let ts = || local.iter().flat_map(|(a, b, _)| [a.0, b.0]);
    let (tmin, tmax) = (ts().fold(f64::INFINITY, f64::min), ts().fold(f64::NEG_INFINITY, f64::max));
    let tol = TOLERANCE.coincidence_mm;
    let (kmin, kmax) = (((-tol - tmin) / p).ceil() as i64, ((f.len + tol - tmax) / p).floor() as i64);
    let mut out = Vec::new();
    for k in kmin..=kmax {
        let s = k as f64 * p;
        for ((ta, oa), (tb, ob), layer) in &local {
            out.push(AssemblyLine { from: f.place(run.width, ta + s, *oa), to: f.place(run.width, tb + s, *ob), layer: (*layer).clone() });
        }
    }
    Ok(out)
}

/// A run whose detail is thicker or thinner than the drawn wall: the detail's width minus the
/// run's, and the face that would move to close it (the non-reference face). Positive `delta`
/// moves that face further toward its own side — the wall grows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WidthConflict {
    pub delta: f64,
    pub toward: FaceRef,
}

impl WidthConflict {
    /// `+10.2mm toward interior`.
    pub fn text(&self) -> String {
        format!("{:+.1}mm toward {}", self.delta, self.toward.name())
    }
}

impl FaceRef {
    /// `exterior` / `interior`.
    pub fn name(self) -> &'static str {
        match self {
            FaceRef::Exterior => "exterior",
            FaceRef::Interior => "interior",
        }
    }
}

/// The width conflict between `run` and a detail referenced on `face` with `bands`, or `None` when
/// they agree within `TOLERANCE.coincidence_mm`. The detail's width is its farthest band into the
/// wall; bands outside the reference don't count. Arc runs are allowed — width is a run property.
pub fn width_conflict(run: &WallRun, face: FaceRef, bands: &[Band]) -> Option<WidthConflict> {
    let width = bands.iter().map(|b| b.offset).fold(0.0, f64::max);
    let delta = width - run.width;
    if delta.abs() <= TOLERANCE.coincidence_mm {
        return None;
    }
    let toward = match face {
        FaceRef::Exterior => FaceRef::Interior,
        FaceRef::Interior => FaceRef::Exterior,
    };
    Some(WidthConflict { delta, toward })
}

/// The mitre direction at a two-arm junction: the sum of the arms' unit directions, normalised; for
/// opposed arms (a sum shorter than 1e-9), the left normal of arm 0. `None` unless exactly two arms.
pub fn junction_bisector(j: &Junction) -> Option<Point2> {
    let [a, b] = j.arms.as_slice() else { return None };
    let unit = |deg: f64| {
        let r = deg.to_radians();
        Point2 { x: r.cos(), y: r.sin() }
    };
    let (u, v) = (unit(a.direction_deg), unit(b.direction_deg));
    let (sx, sy) = (u.x + v.x, u.y + v.y);
    let len = sx.hypot(sy);
    if len < 1e-9 {
        return Some(Point2 { x: -u.y, y: u.x });
    }
    Some(Point2 { x: sx / len, y: sy / len })
}

/// Moves `line`'s `end` (`Start` → `from`, `End` → `to`) to where the line's infinite extension
/// crosses the line through `point` along `bisector`; a line parallel to `bisector` is unchanged.
pub fn mitre_end(line: &mut AssemblyLine, end: RunEnd, point: Point2, bisector: Point2) {
    let (dx, dy) = (line.to.x - line.from.x, line.to.y - line.from.y);
    let denom = dx * bisector.y - dy * bisector.x;
    let len = dx.hypot(dy) * bisector.x.hypot(bisector.y);
    if len <= 0.0 || denom.abs() <= 1e-12 * len {
        return;
    }
    let t = ((point.x - line.from.x) * bisector.y - (point.y - line.from.y) * bisector.x) / denom;
    let hit = Point2 { x: line.from.x + dx * t, y: line.from.y + dy * t };
    match end {
        RunEnd::Start => line.from = hit,
        RunEnd::End => line.to = hit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derivation::walls::RunFace;
    use crate::model::{Id, LinkAnchor};

    fn p(x: f64, y: f64) -> Point2 {
        Point2 { x, y }
    }

    fn l(a: (f64, f64), b: (f64, f64), layer: &str) -> (Point2, Point2, String) {
        (p(a.0, a.1), p(b.0, b.1), layer.into())
    }

    fn offsets(bands: &[Band]) -> Vec<(f64, &str)> {
        bands.iter().map(|b| ((b.offset * 1e6).round() / 1e6, b.layer.as_str())).collect()
    }

    #[test]
    fn bands_take_the_wall_side_from_the_farthest_line_and_ignore_non_parallel_ones() {
        let r = (p(0.0, 0.0), p(1000.0, 0.0));
        let lines = [
            l((0.0, -20.0), (1000.0, -20.0), "CLAD"),
            l((1000.0, 50.0), (0.0, 50.0), "GYP"),
            l((0.0, 200.0), (500.0, 200.0), "GYP"),
            l((0.0, 200.0), (500.0, 200.2), "GYP"), // duplicate within coincidence (0.02°)
            l((0.0, 0.0), (0.0, 300.0), "STUD"),    // perpendicular
            l((0.0, 0.0), (1000.0, 100.0), "X"),    // skew
        ];
        let b = detail_bands(r, &lines).unwrap();
        assert_eq!(offsets(&b), [(-20.0, "CLAD"), (50.0, "GYP"), (200.0, "GYP")]);

        // The wall on the reference's right: offsets flip so they stay positive into the wall.
        let mirrored: Vec<_> = lines.iter().map(|(a, b, n)| (p(a.x, -a.y), p(b.x, -b.y), n.clone())).collect();
        assert_eq!(offsets(&detail_bands(r, &mirrored).unwrap()), [(-20.0, "CLAD"), (50.0, "GYP"), (200.0, "GYP")]);
    }

    #[test]
    fn a_tie_or_no_bands_is_an_error() {
        let r = (p(0.0, 0.0), p(1000.0, 0.0));
        assert!(detail_bands(r, &[l((0.0, -100.0), (1000.0, -100.0), "A"), l((0.0, 100.2), (1000.0, 100.2), "A")]).is_err());
        assert!(detail_bands(r, &[l((0.0, 0.0), (0.0, 100.0), "A")]).is_err());
        assert!(detail_bands(r, &[]).is_err());
    }

    fn run(from: Point2, to: Point2, width: f64) -> WallRun {
        let a = LinkAnchor { entity_id: Id(1), segment_index: 0, segment_param: 0.0 };
        WallRun {
            view_id: Id(1),
            faces: [RunFace { from: a.clone(), to: a.clone() }, RunFace { from: a.clone(), to: a }],
            width,
            centreline: RunCurve::Line { from, to },
        }
    }

    fn ys(lines: &[AssemblyLine]) -> Vec<(f64, f64, f64, f64)> {
        lines.iter().map(|l| (l.from.x, l.from.y, l.to.x, l.to.y)).collect()
    }

    #[test]
    fn bands_lay_from_the_exterior_face_on_either_side() {
        let bands = [Band { offset: -20.0, layer: "CLAD".into() }, Band { offset: 200.0, layer: "GYP".into() }];
        let r = run(p(0.0, 100.0), p(5000.0, 100.0), 200.0);
        // Travelling +x, right is -y: outside below, face at y = 0.
        let right_out = RunSides { left: SideKind::Space(0), right: SideKind::Outside, exterior: true };
        let out = lay_bands(&r, &right_out, FaceRef::Exterior, &bands).unwrap();
        assert_eq!(ys(&out), [(0.0, -20.0, 5000.0, -20.0), (0.0, 200.0, 5000.0, 200.0)]);
        assert_eq!(out[0].layer, "CLAD");
        // Outside on the left (above): face at y = 200.
        let left_out = RunSides { left: SideKind::Outside, right: SideKind::Space(0), exterior: true };
        assert_eq!(ys(&lay_bands(&r, &left_out, FaceRef::Exterior, &bands).unwrap()), [(0.0, 220.0, 5000.0, 220.0), (0.0, 0.0, 5000.0, 0.0)]);
        // Interior face of the first case is the top face, y = 200.
        assert_eq!(ys(&lay_bands(&r, &right_out, FaceRef::Interior, &bands).unwrap()), [(0.0, 220.0, 5000.0, 220.0), (0.0, 0.0, 5000.0, 0.0)]);
    }

    #[test]
    fn arc_runs_and_ambiguous_sides_are_errors() {
        let bands = [Band { offset: 0.0, layer: "A".into() }];
        let mut r = run(p(0.0, 0.0), p(1000.0, 0.0), 200.0);
        let both = RunSides { left: SideKind::Outside, right: SideKind::Outside, exterior: true };
        assert_eq!(lay_bands(&r, &both, FaceRef::Exterior, &bands).unwrap_err(), "no single exterior side");
        let neither = RunSides { left: SideKind::Space(0), right: SideKind::Space(1), exterior: false };
        assert_eq!(lay_bands(&r, &neither, FaceRef::Interior, &bands).unwrap_err(), "no single exterior side");
        r.centreline = RunCurve::Arc { center: p(0.0, 0.0), radius: 1000.0, start_angle: 0.0, sweep: 1.0 };
        let one = RunSides { left: SideKind::Outside, right: SideKind::Space(0), exterior: true };
        assert_eq!(lay_bands(&r, &one, FaceRef::Exterior, &bands).unwrap_err(), "arc runs are not laid yet");
    }

    fn rounded(lines: &[AssemblyLine]) -> Vec<(f64, f64, f64, f64)> {
        let r = |v: f64| (v * 1e6).round() / 1e6;
        lines.iter().map(|l| (r(l.from.x), r(l.from.y), r(l.to.x), r(l.to.y))).collect()
    }

    /// A 38-wide stud at t in [100, 138], o in [50, 200], as four segments, in a detail whose
    /// reference is (0,0)-(1000,0) with its wall above (+y).
    fn stud() -> (DetailFrame, Vec<(Point2, Point2, String)>) {
        let frame = detail_frame((p(0.0, 0.0), p(1000.0, 0.0)), &[l((0.0, 200.0), (1000.0, 200.0), "GYP")]).unwrap();
        let members = vec![
            l((100.0, 50.0), (138.0, 50.0), "STUD"),
            l((138.0, 50.0), (138.0, 200.0), "STUD"),
            l((138.0, 200.0), (100.0, 200.0), "STUD"),
            l((100.0, 200.0), (100.0, 50.0), "STUD"),
        ];
        (frame, members)
    }

    #[test]
    fn the_detail_frame_points_to_the_wall_side_detail_bands_picks() {
        let r = (p(0.0, 0.0), p(1000.0, 0.0));
        let up = detail_frame(r, &[l((0.0, -20.0), (1000.0, -20.0), "A"), l((0.0, 200.0), (1000.0, 200.0), "A")]).unwrap();
        assert_eq!((up.origin, up.along, up.into), (p(0.0, 0.0), p(1.0, 0.0), p(0.0, 1.0)));
        let down = detail_frame(r, &[l((0.0, 20.0), (1000.0, 20.0), "A"), l((0.0, -200.0), (1000.0, -200.0), "A")]).unwrap();
        assert_eq!(down.into, p(0.0, -1.0));
        assert!(detail_frame(r, &[]).is_err());
    }

    #[test]
    fn a_repeat_arrays_along_the_run_on_either_exterior_side() {
        let (frame, members) = stud();
        let r = run(p(0.0, 100.0), p(2000.0, 100.0), 200.0);
        // Outside below (right of +x): face at y = 0, into the wall is +y.
        let right_out = RunSides { left: SideKind::Space(0), right: SideKind::Outside, exterior: true };
        let out = lay_repeat(&r, &right_out, FaceRef::Exterior, &frame, &members, p(400.0, 0.0)).unwrap();
        assert_eq!(out.len(), 20, "k = 0..4, four segments each");
        assert!(out.iter().all(|l| l.layer == "STUD"));
        let got = rounded(&out);
        for k in 0..5 {
            let x = 100.0 + 400.0 * k as f64;
            let want = [(x, 50.0, x + 38.0, 50.0), (x + 38.0, 50.0, x + 38.0, 200.0), (x + 38.0, 200.0, x, 200.0), (x, 200.0, x, 50.0)];
            assert_eq!(got[4 * k..4 * k + 4], want, "copy {k}");
        }
        // Outside above: face at y = 200, into the wall is -y.
        let left_out = RunSides { left: SideKind::Outside, right: SideKind::Space(0), exterior: true };
        let got = rounded(&lay_repeat(&r, &left_out, FaceRef::Exterior, &frame, &members, p(400.0, 0.0)).unwrap());
        assert_eq!(got.len(), 20);
        assert_eq!(got[0], (100.0, 150.0, 138.0, 150.0));
        assert_eq!(got[18], (1738.0, 0.0, 1700.0, 0.0));
        // A reversed pitch arrays the same copies.
        assert_eq!(rounded(&lay_repeat(&r, &left_out, FaceRef::Exterior, &frame, &members, p(-400.0, 0.0)).unwrap()), got);
    }

    #[test]
    fn a_repeat_uses_only_the_along_component_of_its_pitch() {
        let (frame, members) = stud();
        let r = run(p(0.0, 100.0), p(2000.0, 100.0), 200.0);
        let sides = RunSides { left: SideKind::Space(0), right: SideKind::Outside, exterior: true };
        assert!(lay_repeat(&r, &sides, FaceRef::Exterior, &frame, &members, p(0.0, 400.0)).unwrap().is_empty());
        let skew = lay_repeat(&r, &sides, FaceRef::Exterior, &frame, &members, p(400.0, 30.0)).unwrap();
        let straight = lay_repeat(&r, &sides, FaceRef::Exterior, &frame, &members, p(400.0, 0.0)).unwrap();
        assert_eq!(rounded(&skew), rounded(&straight));
        // A run shorter than the member: no copy fits.
        let short = run(p(0.0, 100.0), p(120.0, 100.0), 200.0);
        assert!(lay_repeat(&short, &sides, FaceRef::Exterior, &frame, &members, p(400.0, 0.0)).unwrap().is_empty());
    }

    #[test]
    fn a_repeat_on_an_arc_run_is_an_error() {
        let (frame, members) = stud();
        let mut r = run(p(0.0, 0.0), p(1000.0, 0.0), 200.0);
        r.centreline = RunCurve::Arc { center: p(0.0, 0.0), radius: 1000.0, start_angle: 0.0, sweep: 1.0 };
        let one = RunSides { left: SideKind::Outside, right: SideKind::Space(0), exterior: true };
        assert_eq!(lay_repeat(&r, &one, FaceRef::Exterior, &frame, &members, p(400.0, 0.0)).unwrap_err(), "arc runs are not laid yet");
    }

    #[test]
    fn a_detail_wider_or_narrower_than_the_run_is_a_conflict_on_the_other_face() {
        let bands =
            [Band { offset: -20.0, layer: "CLAD".into() }, Band { offset: 50.0, layer: "GYP".into() }, Band { offset: 200.0, layer: "GYP".into() }];
        let mut r = run(p(0.0, 0.0), p(1000.0, 0.0), 200.0);
        assert_eq!(width_conflict(&r, FaceRef::Exterior, &bands), None);
        r.width = 189.8;
        let c = width_conflict(&r, FaceRef::Exterior, &bands).unwrap();
        assert!((c.delta - 10.2).abs() < 1e-9, "{c:?}");
        assert_eq!(c.toward, FaceRef::Interior);
        assert_eq!(c.text(), "+10.2mm toward interior");
        let c = width_conflict(&r, FaceRef::Interior, &bands).unwrap();
        assert_eq!(c.toward, FaceRef::Exterior);
        assert_eq!(c.text(), "+10.2mm toward exterior");
        r.width = 210.0;
        let c = width_conflict(&r, FaceRef::Exterior, &bands).unwrap();
        assert!((c.delta + 10.0).abs() < 1e-9, "{c:?}");
        assert_eq!(c.text(), "-10.0mm toward interior");
        // Arc runs are judged by width alone.
        r.width = 200.0;
        r.centreline = RunCurve::Arc { center: p(0.0, 0.0), radius: 1000.0, start_angle: 0.0, sweep: 1.0 };
        assert_eq!(width_conflict(&r, FaceRef::Exterior, &bands), None);
    }

    fn junction(dirs: &[f64]) -> Junction {
        use crate::derivation::wall_junctions::JunctionArm;
        let arms = dirs.iter().enumerate().map(|(i, d)| JunctionArm { run: i, end: RunEnd::Start, direction_deg: *d }).collect();
        Junction { point: p(100.0, 100.0), arms, gaps_deg: Vec::new() }
    }

    #[test]
    fn a_two_arm_junction_bisects_its_arms_and_others_have_no_bisector() {
        let b = junction_bisector(&junction(&[0.0, 90.0])).unwrap();
        let h = 0.5f64.sqrt();
        assert!((b.x - h).abs() < 1e-12 && (b.y - h).abs() < 1e-12, "{b:?}");
        let b = junction_bisector(&junction(&[0.0, 180.0])).unwrap();
        assert!(b.x.abs() < 1e-12 && (b.y - 1.0).abs() < 1e-12, "perpendicular: {b:?}");
        assert_eq!(junction_bisector(&junction(&[0.0, 90.0, 180.0])), None);
        assert_eq!(junction_bisector(&junction(&[0.0])), None);
    }

    #[test]
    fn mitre_end_moves_one_end_onto_the_bisector_line() {
        let h = 0.5f64.sqrt();
        let mut l = AssemblyLine { from: p(100.0, -20.0), to: p(4900.0, -20.0), layer: "CLAD".into() };
        mitre_end(&mut l, RunEnd::Start, p(100.0, 100.0), p(h, h));
        assert!((l.from.x + 20.0).abs() < 1e-9 && (l.from.y + 20.0).abs() < 1e-9, "{l:?}");
        assert_eq!(l.to, p(4900.0, -20.0));
        mitre_end(&mut l, RunEnd::End, p(4900.0, 100.0), p(-h, h));
        assert!((l.to.x - 5020.0).abs() < 1e-9, "{l:?}");
        // Parallel to the bisector: unchanged.
        let mut d = AssemblyLine { from: p(0.0, 0.0), to: p(10.0, 10.0), layer: "A".into() };
        let before = d.clone();
        mitre_end(&mut d, RunEnd::Start, p(100.0, 0.0), p(h, h));
        assert_eq!(d, before);
    }
}

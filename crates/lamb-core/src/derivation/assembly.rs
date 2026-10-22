//! Assembly lines (LAMB's `docs/07-architecture/cadcraft-integration.md` §10 "Generation", steps 1,
//! 2 and 4; LAMB T369): a detail view's **continuous bands** — its straight lines parallel to the
//! reference line — laid along a wall run, aligned on the run's matching face (concentric arcs on
//! an arc run, LAMB T389) — and each **repeat** (LAMB T380) arrayed at its pitch along a straight
//! run ([`lay_repeat`]; arc runs are an error). A plan's stud grid ([`RepeatGrid`], LAMB T458)
//! snaps a straight run's repeat whose pitch is near the grid's onto the grid's lines.
//! Junctions are not trimmed by default; for a plan that opts into mitring (§10 step 3, LAMB T385)
//! the adapter moves band ends at undetailed two-arm junctions onto [`junction_bisector`] with
//! [`mitre_end`], and a T's branch band ends onto the through run's face with [`tee_butt`] (LAMB
//! T472). Computed on demand.
//!
//! Not the AI-line sweep (`crate::sweeps` in the adapter); everything here is "assembly".

use super::wall_junctions::{Junction, RunEnd};
use super::wall_openings::OpeningBridge;
use super::wall_spaces::{RunSides, outside_on_left};
use super::walls::{RunCurve, WallRun};
use crate::geometry::Point2;
use crate::geometry::intersect::infinite_line_intersect;
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

/// One laid band: a straight line in the plan view's local space, or — with `arc` — an arc from
/// `from` to `to` about `arc.center`, travelling counter-clockwise when `arc.ccw`.
#[derive(Debug, Clone, PartialEq)]
pub struct AssemblyLine {
    pub from: Point2,
    pub to: Point2,
    pub layer: String,
    pub arc: Option<ArcSpan>,
}

/// The circle an arc [`AssemblyLine`] lies on, and its travel direction from `from` to `to`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArcSpan {
    pub center: Point2,
    pub radius: f64,
    pub ccw: bool,
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

/// Whether the `face` side of a run is the left of its centreline's travel: the `Outside` side for
/// `Exterior`, the other for `Interior`. Errors unless [`outside_on_left`] decides it.
fn face_on_left(sides: &RunSides, face: FaceRef) -> Result<bool, String> {
    let outside_left = outside_on_left(sides).ok_or("no single exterior side")?;
    Ok(outside_left == (face == FaceRef::Exterior))
}

fn face_frame(run: &WallRun, sides: &RunSides, face: FaceRef) -> Result<FaceFrame, String> {
    let RunCurve::Line { from, to } = run.centreline else { return Err("arc runs are not laid yet".into()) };
    let toward_left = face_on_left(sides, face)?;
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let len = dx.hypot(dy);
    if len <= TOLERANCE.coincidence_mm {
        return Err("the run has no length".into());
    }
    let s = if toward_left { 1.0 } else { -1.0 };
    Ok(FaceFrame { from, to, dir: Point2 { x: dx / len, y: dy / len }, len, normal: Point2 { x: -dy / len * s, y: dx / len * s } })
}

/// `bands` laid along a `run`: the face line is the centreline offset `width / 2` toward the `face`
/// side, each band that face line shifted `offset` back toward the centreline, over the
/// centreline's extent. On an arc run each band is a concentric arc over the same angles; the left
/// of its travel is toward the centre when `sweep > 0`. A band radius not above
/// `TOLERANCE.coincidence_mm` is an error.
pub fn lay_bands(run: &WallRun, sides: &RunSides, face: FaceRef, bands: &[Band]) -> Result<Vec<AssemblyLine>, String> {
    if let RunCurve::Arc { center, radius, start_angle, sweep } = run.centreline {
        let toward_left = face_on_left(sides, face)?;
        // +1 when the face side is away from the centre.
        let outward = if toward_left == (sweep > 0.0) { -1.0 } else { 1.0 };
        let at = |r: f64, a: f64| Point2 { x: center.x + r * a.cos(), y: center.y + r * a.sin() };
        return bands
            .iter()
            .map(|b| {
                let r = radius + outward * (run.width / 2.0 - b.offset);
                if r <= TOLERANCE.coincidence_mm {
                    return Err("band radius is not positive".to_string());
                }
                Ok(AssemblyLine {
                    from: at(r, start_angle),
                    to: at(r, start_angle + sweep),
                    layer: b.layer.clone(),
                    arc: Some(ArcSpan { center, radius: r, ccw: sweep > 0.0 }),
                })
            })
            .collect();
    }
    let f = face_frame(run, sides, face)?;
    Ok(bands
        .iter()
        .map(|b| {
            let d = run.width / 2.0 - b.offset;
            let shift = |p: Point2| Point2 { x: p.x + f.normal.x * d, y: p.y + f.normal.y * d };
            AssemblyLine { from: shift(f.from), to: shift(f.to), layer: b.layer.clone(), arc: None }
        })
        .collect())
}

/// A plan's stud grid: grid lines perpendicular to each straight run through `origin + k·pitch`
/// along it. A repeat whose along-pitch is within `pitch_tolerance_mm` of `pitch` is laid at exactly
/// `pitch`, each copy's along-centre on a grid line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RepeatGrid {
    pub origin: Point2,
    pub pitch: f64,
    pub pitch_tolerance_mm: f64,
}

/// A repeat's `members` (straight segments in the detail's space, with their layers) arrayed along
/// a `run`: copies shifted `k·|p|` along, `p` being `pitch`'s component along the reference, for
/// every integer `k` whose copy's along-extent lies within `[0, run length]`
/// (± `TOLERANCE.coincidence_mm`). Points map through the same face frame as [`lay_bands`], the
/// reference's `from` at the centreline's `from`. On an arc run the run length is the arc length
/// of the reference face (radius `rf`); a detail point `(t, o)` maps to radius
/// `radius + outward·(width/2 − o)` at angle `start_angle + sign(sweep)·t / rf`, and each member
/// segment is written as a straight chord between its mapped ends; a mapped radius not above
/// `TOLERANCE.coincidence_mm` is an error. No along component, no members, or no copy that fits
/// gives no lines. With a `grid` and a straight run whose `|p|` is within the grid's tolerance of
/// its pitch, copies are `δ + k·pitch` instead, `δ` putting the members' along-centre on a grid
/// line (independent of the run's travel direction); arc runs ignore the grid.
pub fn lay_repeat(
    run: &WallRun,
    sides: &RunSides,
    face: FaceRef,
    frame: &DetailFrame,
    members: &[(Point2, Point2, String)],
    pitch: Point2,
    grid: Option<&RepeatGrid>,
) -> Result<Vec<AssemblyLine>, String> {
    // The run's along-coordinate of its `from` measured from the grid origin, straight runs only.
    let mut grid_a0: Option<f64> = None;
    // The run length along the reference face, and a detail point `(t, o)` placed on the run.
    let (len, place): (f64, Box<dyn Fn(f64, f64) -> Result<Point2, String>>) = match run.centreline {
        RunCurve::Arc { center, radius, start_angle, sweep } => {
            let toward_left = face_on_left(sides, face)?;
            // +1 when the face side is away from the centre, as in `lay_bands`.
            let outward = if toward_left == (sweep > 0.0) { -1.0 } else { 1.0 };
            let width = run.width;
            let rf = radius + outward * width / 2.0;
            if rf <= TOLERANCE.coincidence_mm {
                return Err("repeat radius is not positive".into());
            }
            let place = move |t: f64, o: f64| {
                let r = radius + outward * (width / 2.0 - o);
                if r <= TOLERANCE.coincidence_mm {
                    return Err("repeat radius is not positive".to_string());
                }
                let a = start_angle + sweep.signum() * t / rf;
                Ok(Point2 { x: center.x + r * a.cos(), y: center.y + r * a.sin() })
            };
            (rf * sweep.abs(), Box::new(place))
        }
        RunCurve::Line { .. } => {
            let f = face_frame(run, sides, face)?;
            if let Some(g) = grid {
                grid_a0 = Some((f.from.x - g.origin.x) * f.dir.x + (f.from.y - g.origin.y) * f.dir.y);
            }
            let width = run.width;
            (f.len, Box::new(move |t: f64, o: f64| Ok(f.place(width, t, o))))
        }
    };
    let p = (pitch.x * frame.along.x + pitch.y * frame.along.y).abs();
    if p <= TOLERANCE.coincidence_mm || members.is_empty() {
        return Ok(Vec::new());
    }
    let local: Vec<_> = members.iter().map(|(a, b, layer)| (frame.local(*a), frame.local(*b), layer)).collect();
    let ts = || local.iter().flat_map(|(a, b, _)| [a.0, b.0]);
    let (tmin, tmax) = (ts().fold(f64::INFINITY, f64::min), ts().fold(f64::NEG_INFINITY, f64::max));
    let tol = TOLERANCE.coincidence_mm;
    let (p, shift) = match (grid, grid_a0) {
        (Some(g), Some(a0)) if g.pitch > 0.0 && (p - g.pitch).abs() <= g.pitch_tolerance_mm => {
            (g.pitch, (-(a0 + (tmin + tmax) / 2.0)).rem_euclid(g.pitch))
        }
        _ => (p, 0.0),
    };
    let (kmin, kmax) = (((-tol - tmin - shift) / p).ceil() as i64, ((len + tol - tmax - shift) / p).floor() as i64);
    let mut out = Vec::new();
    for k in kmin..=kmax {
        let s = shift + k as f64 * p;
        for ((ta, oa), (tb, ob), layer) in &local {
            out.push(AssemblyLine { from: place(ta + s, *oa)?, to: place(tb + s, *ob)?, layer: (*layer).clone(), arc: None });
        }
    }
    Ok(out)
}

/// A corner detail placed at an L or T junction (§10 step 3, LAMB T461, T481). The detail's
/// reference line sits on `face` (exterior or interior) with its `to` end on a defined corner point;
/// the placement is a rigid rotation, never a mirror. `sides` is indexed like `runs`. Every
/// straight segment of `lines` is mapped; layers are kept.
///
/// - **Two arms**: the `to` end lands where the arms' `face` lines cross and the reference runs
///   along the arm whose handedness — travel toward the corner against the wall side — matches the
///   detail's, the wall side away from that face.
/// - **Three arms (T)**: exactly one gap within `TOLERANCE.angle_deg` of 180° and straight through
///   arms of equal width ([`tee_butt`]). The through run's face opposite the branch carries the
///   reference — it must be the `face` side (per the first through arm whose `sides` decide it),
///   else an error — its `to` end at the foot of the
///   branch's centreline on that face, the wall side toward the branch, the travel the along-face
///   direction matching the detail's handedness.
///
/// Any other junction is an error.
///
/// `insets` (indexed like `j.arms`; a missing entry is 0) move each arm's `face` line that far into
/// the wall before anything else — an L's corner is the crossing of the moved lines, a T's corner
/// point moves by its through arm's inset — and `outward` maps the detail's `into` away from the
/// wall instead of into it (the handedness tests use that image). Structural-centre and grid
/// references place through these (§9, LAMB T490); `&[]` and `false` are a face reference.
#[allow(clippy::too_many_arguments)]
pub fn place_corner(
    j: &Junction,
    runs: &[WallRun],
    sides: &[RunSides],
    face: FaceRef,
    reference: (Point2, Point2),
    lines: &[(Point2, Point2, String)],
    insets: &[f64],
    outward: bool,
) -> Result<Vec<AssemblyLine>, String> {
    let frame = detail_frame(reference, lines)?;
    let cross = |u: Point2, v: Point2| u.x * v.y - u.y * v.x;
    let want = cross(frame.along, frame.into).signum();
    let inset = |k: usize| insets.get(k).copied().unwrap_or(0.0);
    let sgn = if outward { -1.0 } else { 1.0 };
    let image = |ni: Point2| Point2 { x: ni.x * sgn, y: ni.y * sgn };
    // The corner point `c`, the reference's travel `t` and the unit normal `ni` into the wall.
    let (c, t, ni) = match j.arms.as_slice() {
        [a0, a1] => {
            // Per arm: two points of its face line, the travel toward the junction and the normal
            // into the wall.
            let mut arms = Vec::new();
            for (k, arm) in [a0, a1].into_iter().enumerate() {
                let run = runs.get(arm.run).ok_or("junction arm names no run")?;
                let side = sides.get(arm.run).ok_or("junction arm has no sides")?;
                let f = face_frame(run, side, face)?;
                let h = run.width / 2.0 - inset(k);
                let a = Point2 { x: f.from.x + f.normal.x * h, y: f.from.y + f.normal.y * h };
                let b = Point2 { x: f.to.x + f.normal.x * h, y: f.to.y + f.normal.y * h };
                let s = match arm.end {
                    RunEnd::Start => -1.0,
                    RunEnd::End => 1.0,
                };
                let t = Point2 { x: f.dir.x * s, y: f.dir.y * s };
                let ni = Point2 { x: -f.normal.x, y: -f.normal.y };
                arms.push((a, b, t, ni));
            }
            let c = infinite_line_intersect(arms[0].0, arms[0].1, arms[1].0, arms[1].1).ok_or("the corner's arms are parallel")?.point;
            let fits: Vec<_> = arms.iter().filter(|(_, _, t, ni)| cross(*t, image(*ni)).signum() == want).collect();
            let &[&(_, _, t, ni)] = fits.as_slice() else { return Err("the corner's arms do not fit the detail".into()) };
            (c, t, ni)
        }
        [_, _, _] => {
            let one_straight = j.gaps_deg.len() == 3 && j.gaps_deg.iter().filter(|g| (*g - 180.0).abs() <= TOLERANCE.angle_deg).count() == 1;
            if !one_straight {
                return Err("only L and T corners are placed yet".into());
            }
            let (branch, toward, _) = tee_butt(j, runs).ok_or("the T's through arms are not straight runs of equal width")?;
            // The first through arm whose sides decide the face (one may be shared by two spaces).
            let frame_of = |k: usize| -> Result<(&WallRun, FaceFrame, f64), String> {
                let a = (branch + k) % 3;
                let i = j.arms[a].run;
                let run = runs.get(i).ok_or("junction arm names no run")?;
                Ok((run, face_frame(run, sides.get(i).ok_or("junction arm has no sides")?, face)?, inset(a)))
            };
            let (run, f, ins) = frame_of(1).or_else(|e| frame_of(2).map_err(|_| e))?;
            // `toward` is the through run's face on the branch side.
            if (toward.x - j.point.x) * f.normal.x + (toward.y - j.point.y) * f.normal.y > 0.0 {
                return Err("the detail's reference face is on the branch side".into());
            }
            let h = run.width / 2.0 - ins;
            let c = Point2 { x: j.point.x + f.normal.x * h, y: j.point.y + f.normal.y * h };
            let ni = Point2 { x: -f.normal.x, y: -f.normal.y };
            let t = if cross(f.dir, image(ni)).signum() == want { f.dir } else { Point2 { x: -f.dir.x, y: -f.dir.y } };
            (c, t, ni)
        }
        _ => return Err("only L and T corners are placed yet".into()),
    };
    let ni = image(ni);
    let r = reference.1;
    let map = |p: Point2| {
        let (dx, dy) = (p.x - r.x, p.y - r.y);
        let (u, v) = (dx * frame.along.x + dy * frame.along.y, dx * frame.into.x + dy * frame.into.y);
        Point2 { x: c.x + t.x * u + ni.x * v, y: c.y + t.y * u + ni.y * v }
    };
    Ok(lines.iter().map(|(p, q, layer)| AssemblyLine { from: map(*p), to: map(*q), layer: layer.clone(), arc: None }).collect())
}

/// A jamb of an opening bridge (§9 item 3, LAMB T485): the run `end`'s `face` line end `c` — its
/// centreline end offset `width / 2` toward the face — and the unit travel `t` toward the opening
/// (along the centreline at `End`, against it at `Start`). Errors as `face_frame`: an arc run, an
/// undecided face side.
pub fn jamb_point(run: &WallRun, sides: &RunSides, face: FaceRef, end: RunEnd) -> Result<(Point2, Point2), String> {
    let f = face_frame(run, sides, face)?;
    let h = run.width / 2.0;
    let (e, s) = match end {
        RunEnd::Start => (f.from, -1.0),
        RunEnd::End => (f.to, 1.0),
    };
    Ok((Point2 { x: e.x + f.normal.x * h, y: e.y + f.normal.y * h }, Point2 { x: f.dir.x * s, y: f.dir.y * s }))
}

/// An opening detail placed at both jambs of `bridge` (§9 item 3, LAMB T485), `[a's, b's]`. The
/// detail is drawn for one jamb: its reference line on `face`, its `to` end at the jamb. At each
/// jamb ([`jamb_point`]) a detail point `p` maps, as in [`place_corner`], from `(u, v)` — `p −
/// reference.1` in the detail's `(along, into)` — to `c + t·u + ni·v`, `ni` pointing into the wall.
/// Each jamb uses its own travel, so the detail lands rigidly at the jamb whose handedness matches
/// the detail's and **as its mirror image at the other** — intended: a jamb is drawn once, "typ.".
/// `sides` is indexed like `runs`. Errors: the detail's frame ([`detail_frame`]), a missing run, an
/// arc run, an undecided face side.
///
/// `insets` (`[a's, b's]`; a missing entry is 0) move each jamb's point that far into the wall
/// along `ni`, and `outward` maps the detail's `into` to `−ni` — as in [`place_corner`] (LAMB T490).
#[allow(clippy::too_many_arguments)]
pub fn place_opening(
    bridge: &OpeningBridge,
    runs: &[WallRun],
    sides: &[RunSides],
    face: FaceRef,
    reference: (Point2, Point2),
    lines: &[(Point2, Point2, String)],
    insets: &[f64],
    outward: bool,
) -> Result<[Vec<AssemblyLine>; 2], String> {
    let frame = detail_frame(reference, lines)?;
    let r = reference.1;
    let sgn = if outward { -1.0 } else { 1.0 };
    let jamb = |(i, end): (usize, RunEnd), inset: f64| -> Result<Vec<AssemblyLine>, String> {
        let run = runs.get(i).ok_or("the opening bridge names no run")?;
        let side = sides.get(i).ok_or("the opening bridge's run has no sides")?;
        let f = face_frame(run, side, face)?;
        let (c, t) = jamb_point(run, side, face, end)?;
        let c = Point2 { x: c.x - f.normal.x * inset, y: c.y - f.normal.y * inset };
        let ni = Point2 { x: -f.normal.x * sgn, y: -f.normal.y * sgn };
        let map = |p: Point2| {
            let (dx, dy) = (p.x - r.x, p.y - r.y);
            let (u, v) = (dx * frame.along.x + dy * frame.along.y, dx * frame.into.x + dy * frame.into.y);
            Point2 { x: c.x + t.x * u + ni.x * v, y: c.y + t.y * u + ni.y * v }
        };
        Ok(lines.iter().map(|(p, q, layer)| AssemblyLine { from: map(*p), to: map(*q), layer: layer.clone(), arc: None }).collect())
    };
    let inset = |k: usize| insets.get(k).copied().unwrap_or(0.0);
    Ok([jamb(bridge.a, inset(0))?, jamb(bridge.b, inset(1))?])
}

/// How far a placed jamb detail reaches back into its run from the jamb point `c`, `t` being the
/// travel toward the opening ([`jamb_point`]): the farthest distance along `−t` of any line end,
/// floored at 0. The run's bands are trimmed to it.
pub fn jamb_reach(c: Point2, t: Point2, lines: &[AssemblyLine]) -> f64 {
    lines.iter().flat_map(|l| [l.from, l.to]).map(|p| -((p.x - c.x) * t.x + (p.y - c.y) * t.y)).fold(0.0, f64::max)
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
/// crosses the line through `point` along `bisector`; a line parallel to `bisector`, or an arc, is
/// unchanged.
pub fn mitre_end(line: &mut AssemblyLine, end: RunEnd, point: Point2, bisector: Point2) {
    if line.arc.is_some() {
        return;
    }
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

/// The unit direction from `j.point` into arm `i`'s straight run (toward its far end), or `None` for
/// an arc run, a degenerate run or a missing arm.
pub fn arm_direction(j: &Junction, runs: &[WallRun], i: usize) -> Option<Point2> {
    let arm = j.arms.get(i)?;
    let RunCurve::Line { from, to } = runs.get(arm.run)?.centreline else { return None };
    let (dx, dy) = match arm.end {
        RunEnd::Start => (to.x - from.x, to.y - from.y),
        RunEnd::End => (from.x - to.x, from.y - to.y),
    };
    let len = dx.hypot(dy);
    (len > 0.0).then(|| Point2 { x: dx / len, y: dy / len })
}

/// The butt line for an undetailed T junction (§10 step 3, LAMB T472): `None` unless `j` has three
/// arms and exactly one gap within `TOLERANCE.angle_deg` of 180°, whose two through arms
/// (`arms[i]`, `arms[(i + 1) % 3]`) are straight and of equal `width` within
/// `TOLERANCE.coincidence_mm`. Returns `(branch arm index, face point, face dir)`: the through run's
/// drawn face on the branch's side — `j.point + n · width / 2`, along the first through arm `t`.
pub fn tee_butt(j: &Junction, runs: &[WallRun]) -> Option<(usize, Point2, Point2)> {
    if j.arms.len() != 3 || j.gaps_deg.len() != 3 {
        return None;
    }
    let mut straight = (0..3).filter(|&i| (j.gaps_deg[i] - 180.0).abs() <= TOLERANCE.angle_deg);
    let i = straight.next()?;
    if straight.next().is_some() {
        return None;
    }
    let (a, b, branch) = (i, (i + 1) % 3, (i + 2) % 3);
    let t = arm_direction(j, runs, a)?;
    arm_direction(j, runs, b)?;
    let (wa, wb) = (runs.get(j.arms[a].run)?.width, runs.get(j.arms[b].run)?.width);
    if (wa - wb).abs() > TOLERANCE.coincidence_mm {
        return None;
    }
    let r = j.arms[branch].direction_deg.to_radians();
    let left = Point2 { x: -t.y, y: t.x };
    let s = if left.x * r.cos() + left.y * r.sin() >= 0.0 { 1.0 } else { -1.0 };
    let half = wa / 2.0 * s;
    Some((branch, Point2 { x: j.point.x + left.x * half, y: j.point.y + left.y * half }, t))
}

/// How far a placed corner detail (§10 step 3, LAMB T462) reaches into each arm of `j`, in arm
/// order: the farthest along-arm distance, from `j.point` along [`arm_direction`], of any end of
/// the `corner` lines, floored at 0. An arc arm's reach is 0.
pub fn corner_reach(j: &Junction, runs: &[WallRun], corner: &[AssemblyLine]) -> Vec<f64> {
    (0..j.arms.len())
        .map(|i| {
            let Some(u) = arm_direction(j, runs, i) else { return 0.0 };
            corner.iter().flat_map(|l| [l.from, l.to]).map(|p| (p.x - j.point.x) * u.x + (p.y - j.point.y) * u.y).fold(0.0, f64::max)
        })
        .collect()
}

/// Clips `line` to the part at least `reach` along `u` from `node` (`a(x) = (x − node)·u`): both
/// ends below `reach − TOLERANCE.coincidence_mm` → `false` (drop it); one end below → that end moves
/// along the line to `a = reach`; otherwise unchanged. An arc line is kept unchanged.
pub fn trim_to_reach(line: &mut AssemblyLine, node: Point2, u: Point2, reach: f64) -> bool {
    if line.arc.is_some() {
        return true;
    }
    let a = |p: Point2| (p.x - node.x) * u.x + (p.y - node.y) * u.y;
    let (af, at) = (a(line.from), a(line.to));
    let limit = reach - TOLERANCE.coincidence_mm;
    match (af < limit, at < limit) {
        (true, true) => false,
        (false, false) => true,
        (from_below, _) => {
            let t = (reach - af) / (at - af);
            let hit = Point2 { x: line.from.x + (line.to.x - line.from.x) * t, y: line.from.y + (line.to.y - line.from.y) * t };
            if from_below {
                line.from = hit;
            } else {
                line.to = hit;
            }
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derivation::wall_spaces::SideKind;
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
        let right_out = RunSides { left: SideKind::Space(0), right: SideKind::Outside, exterior: true, open_outside_left: None };
        let out = lay_bands(&r, &right_out, FaceRef::Exterior, &bands).unwrap();
        assert_eq!(ys(&out), [(0.0, -20.0, 5000.0, -20.0), (0.0, 200.0, 5000.0, 200.0)]);
        assert_eq!(out[0].layer, "CLAD");
        // Outside on the left (above): face at y = 200.
        let left_out = RunSides { left: SideKind::Outside, right: SideKind::Space(0), exterior: true, open_outside_left: None };
        assert_eq!(ys(&lay_bands(&r, &left_out, FaceRef::Exterior, &bands).unwrap()), [(0.0, 220.0, 5000.0, 220.0), (0.0, 0.0, 5000.0, 0.0)]);
        // Interior face of the first case is the top face, y = 200.
        assert_eq!(ys(&lay_bands(&r, &right_out, FaceRef::Interior, &bands).unwrap()), [(0.0, 220.0, 5000.0, 220.0), (0.0, 0.0, 5000.0, 0.0)]);
    }

    #[test]
    fn ambiguous_sides_are_errors() {
        let bands = [Band { offset: 0.0, layer: "A".into() }];
        let r = run(p(0.0, 0.0), p(1000.0, 0.0), 200.0);
        let both = RunSides { left: SideKind::Outside, right: SideKind::Outside, exterior: true, open_outside_left: None };
        assert_eq!(lay_bands(&r, &both, FaceRef::Exterior, &bands).unwrap_err(), "no single exterior side");
        let neither = RunSides { left: SideKind::Space(0), right: SideKind::Space(1), exterior: false, open_outside_left: None };
        assert_eq!(lay_bands(&r, &neither, FaceRef::Interior, &bands).unwrap_err(), "no single exterior side");
    }

    fn arc_run(radius: f64, sweep: f64) -> WallRun {
        let mut r = run(p(0.0, 0.0), p(1.0, 0.0), 200.0);
        r.centreline = RunCurve::Arc { center: p(0.0, 0.0), radius, start_angle: 0.0, sweep };
        r
    }

    fn arc_bands() -> [Band; 3] {
        [Band { offset: -20.0, layer: "CLAD".into() }, Band { offset: 50.0, layer: "GYP".into() }, Band { offset: 200.0, layer: "GYP".into() }]
    }

    fn radii(lines: &[AssemblyLine]) -> Vec<f64> {
        lines.iter().map(|l| (l.arc.unwrap().radius * 1e6).round() / 1e6).collect()
    }

    #[test]
    fn bands_lay_as_concentric_arcs_along_an_arc_run() {
        use std::f64::consts::FRAC_PI_2;
        let bands = arc_bands();
        let outside_right = RunSides { left: SideKind::Space(0), right: SideKind::Outside, exterior: true, open_outside_left: None };
        let outside_left = RunSides { left: SideKind::Outside, right: SideKind::Space(0), exterior: true, open_outside_left: None };
        // Exterior outside the curve, travelling CCW (left is toward the centre).
        let ccw = lay_bands(&arc_run(1000.0, FRAC_PI_2), &outside_right, FaceRef::Exterior, &bands).unwrap();
        assert_eq!(radii(&ccw), [1120.0, 1050.0, 900.0]);
        assert!(ccw.iter().all(|l| l.arc.unwrap().ccw && l.arc.unwrap().center == p(0.0, 0.0)));
        assert_eq!(rounded(&ccw[..1]), [(1120.0, 0.0, 0.0, 1120.0)]);
        assert_eq!(ccw[0].layer, "CLAD");
        // Exterior outside the curve, travelling CW (left is away from the centre).
        let cw = lay_bands(&arc_run(1000.0, -FRAC_PI_2), &outside_left, FaceRef::Exterior, &bands).unwrap();
        assert_eq!(radii(&cw), [1120.0, 1050.0, 900.0]);
        assert!(cw.iter().all(|l| !l.arc.unwrap().ccw));
        assert_eq!(rounded(&cw[..1]), [(1120.0, 0.0, 0.0, -1120.0)]);
        // Exterior inside the curve.
        let inside = lay_bands(&arc_run(1000.0, FRAC_PI_2), &outside_left, FaceRef::Exterior, &bands).unwrap();
        assert_eq!(radii(&inside), [880.0, 950.0, 1100.0]);
        // Interior face of the first case is the inner face.
        assert_eq!(radii(&lay_bands(&arc_run(1000.0, FRAC_PI_2), &outside_right, FaceRef::Interior, &bands).unwrap()), [880.0, 950.0, 1100.0]);
    }

    #[test]
    fn a_band_past_the_centre_of_an_arc_run_is_an_error() {
        let sides = RunSides { left: SideKind::Space(0), right: SideKind::Outside, exterior: true, open_outside_left: None };
        let deep = [Band { offset: 1000.0, layer: "A".into() }];
        assert_eq!(lay_bands(&arc_run(300.0, 1.0), &sides, FaceRef::Exterior, &deep).unwrap_err(), "band radius is not positive");
    }

    #[test]
    fn mitre_end_leaves_an_arc_unchanged() {
        let sides = RunSides { left: SideKind::Space(0), right: SideKind::Outside, exterior: true, open_outside_left: None };
        let mut a = lay_bands(&arc_run(1000.0, 1.0), &sides, FaceRef::Exterior, &arc_bands()).unwrap().remove(0);
        let before = a.clone();
        mitre_end(&mut a, RunEnd::Start, p(0.0, 0.0), p(0.0, 1.0));
        assert_eq!(a, before);
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
        let right_out = RunSides { left: SideKind::Space(0), right: SideKind::Outside, exterior: true, open_outside_left: None };
        let out = lay_repeat(&r, &right_out, FaceRef::Exterior, &frame, &members, p(400.0, 0.0), None).unwrap();
        assert_eq!(out.len(), 20, "k = 0..4, four segments each");
        assert!(out.iter().all(|l| l.layer == "STUD"));
        let got = rounded(&out);
        for k in 0..5 {
            let x = 100.0 + 400.0 * k as f64;
            let want = [(x, 50.0, x + 38.0, 50.0), (x + 38.0, 50.0, x + 38.0, 200.0), (x + 38.0, 200.0, x, 200.0), (x, 200.0, x, 50.0)];
            assert_eq!(got[4 * k..4 * k + 4], want, "copy {k}");
        }
        // Outside above: face at y = 200, into the wall is -y.
        let left_out = RunSides { left: SideKind::Outside, right: SideKind::Space(0), exterior: true, open_outside_left: None };
        let got = rounded(&lay_repeat(&r, &left_out, FaceRef::Exterior, &frame, &members, p(400.0, 0.0), None).unwrap());
        assert_eq!(got.len(), 20);
        assert_eq!(got[0], (100.0, 150.0, 138.0, 150.0));
        assert_eq!(got[18], (1738.0, 0.0, 1700.0, 0.0));
        // A reversed pitch arrays the same copies.
        assert_eq!(rounded(&lay_repeat(&r, &left_out, FaceRef::Exterior, &frame, &members, p(-400.0, 0.0), None).unwrap()), got);
    }

    #[test]
    fn a_repeat_uses_only_the_along_component_of_its_pitch() {
        let (frame, members) = stud();
        let r = run(p(0.0, 100.0), p(2000.0, 100.0), 200.0);
        let sides = RunSides { left: SideKind::Space(0), right: SideKind::Outside, exterior: true, open_outside_left: None };
        assert!(lay_repeat(&r, &sides, FaceRef::Exterior, &frame, &members, p(0.0, 400.0), None).unwrap().is_empty());
        let skew = lay_repeat(&r, &sides, FaceRef::Exterior, &frame, &members, p(400.0, 30.0), None).unwrap();
        let straight = lay_repeat(&r, &sides, FaceRef::Exterior, &frame, &members, p(400.0, 0.0), None).unwrap();
        assert_eq!(rounded(&skew), rounded(&straight));
        // A run shorter than the member: no copy fits.
        let short = run(p(0.0, 100.0), p(120.0, 100.0), 200.0);
        assert!(lay_repeat(&short, &sides, FaceRef::Exterior, &frame, &members, p(400.0, 0.0), None).unwrap().is_empty());
    }

    /// The along (x) centre of each four-segment stud copy.
    fn copy_centres(lines: &[AssemblyLine]) -> Vec<f64> {
        lines
            .chunks(4)
            .map(|c| {
                let xs = c.iter().flat_map(|l| [l.from.x, l.to.x]);
                let (lo, hi) = (xs.clone().fold(f64::INFINITY, f64::min), xs.fold(f64::NEG_INFINITY, f64::max));
                (lo + hi) / 2.0
            })
            .collect()
    }

    #[test]
    fn a_repeat_near_the_grid_pitch_lays_on_the_stud_grid_from_its_origin() {
        let (frame, members) = stud();
        let grid = RepeatGrid { origin: p(13.0, 7.0), pitch: 406.4, pitch_tolerance_mm: 5.0 };
        let r = run(p(0.0, 100.0), p(5000.0, 100.0), 200.0);
        let right_out = RunSides { left: SideKind::Space(0), right: SideKind::Outside, exterior: true, open_outside_left: None };
        let out = lay_repeat(&r, &right_out, FaceRef::Exterior, &frame, &members, p(406.0, 0.0), Some(&grid)).unwrap();
        let centres = copy_centres(&out);
        assert_eq!(centres.len(), 12);
        for (i, c) in centres.iter().enumerate() {
            let m = (c - 13.0).rem_euclid(406.4);
            assert!(m < 1e-6 || 406.4 - m < 1e-6, "copy {i} at {c} is off the grid");
            if i > 0 {
                assert!((c - centres[i - 1] - 406.4).abs() < 1e-6, "spacing at copy {i}");
            }
            assert!(*c - 19.0 >= -1e-9 && *c + 19.0 <= 5000.0 + 1e-9, "copy {i} fits the run");
        }
        // The same wall drawn the other way: outside is now on its left; the centres are the same.
        let rev = run(p(5000.0, 100.0), p(0.0, 100.0), 200.0);
        let left_out = RunSides { left: SideKind::Outside, right: SideKind::Space(0), exterior: true, open_outside_left: None };
        let mut back = copy_centres(&lay_repeat(&rev, &left_out, FaceRef::Exterior, &frame, &members, p(406.0, 0.0), Some(&grid)).unwrap());
        back.sort_by(f64::total_cmp);
        assert_eq!(back.len(), centres.len());
        assert!(back.iter().zip(&centres).all(|(a, b)| (a - b).abs() < 1e-6), "{back:?} vs {centres:?}");
        // A pitch outside the tolerance ignores the grid.
        let far = lay_repeat(&r, &right_out, FaceRef::Exterior, &frame, &members, p(600.0, 0.0), Some(&grid)).unwrap();
        assert_eq!(far, lay_repeat(&r, &right_out, FaceRef::Exterior, &frame, &members, p(600.0, 0.0), None).unwrap());
        // An arc run ignores the grid.
        let arc = lay_repeat(&arc_run(1000.0, 1.0), &right_out, FaceRef::Exterior, &frame, &members, p(406.0, 0.0), Some(&grid)).unwrap();
        assert_eq!(arc, lay_repeat(&arc_run(1000.0, 1.0), &right_out, FaceRef::Exterior, &frame, &members, p(406.0, 0.0), None).unwrap());
    }

    #[test]
    fn a_repeat_arrays_along_an_arc_run_as_chords_at_the_face_arc_length() {
        use std::f64::consts::FRAC_PI_2;
        let frame = detail_frame((p(0.0, 0.0), p(1000.0, 0.0)), &[l((0.0, 200.0), (1000.0, 200.0), "GYP")]).unwrap();
        let members = vec![l((100.0, 50.0), (100.0, 200.0), "STUD")];
        let outside_right = RunSides { left: SideKind::Space(0), right: SideKind::Outside, exterior: true, open_outside_left: None };
        let outside_left = RunSides { left: SideKind::Outside, right: SideKind::Space(0), exterior: true, open_outside_left: None };
        let check = |sweep: f64, sides: &RunSides, rf: f64, radii: (f64, f64)| {
            let out = lay_repeat(&arc_run(1000.0, sweep), sides, FaceRef::Exterior, &frame, &members, p(400.0, 0.0), None).unwrap();
            let n = ((rf * FRAC_PI_2 - 100.0) / 400.0).floor() as usize + 1;
            assert_eq!(out.len(), n, "sweep {sweep}, rf {rf}");
            for (k, line) in out.iter().enumerate() {
                assert!(line.arc.is_none() && line.layer == "STUD");
                let a = sweep.signum() * (100.0 + 400.0 * k as f64) / rf;
                for (pt, r) in [(line.from, radii.0), (line.to, radii.1)] {
                    assert!((pt.x - r * a.cos()).abs() < 1e-9 && (pt.y - r * a.sin()).abs() < 1e-9, "copy {k}: {pt:?} vs r {r} a {a}");
                }
            }
        };
        // Exterior outside the curve, both travel directions.
        check(FRAC_PI_2, &outside_right, 1100.0, (1050.0, 900.0));
        check(-FRAC_PI_2, &outside_left, 1100.0, (1050.0, 900.0));
        // Exterior inside the curve.
        check(FRAC_PI_2, &outside_left, 900.0, (950.0, 1100.0));
        check(-FRAC_PI_2, &outside_right, 900.0, (950.0, 1100.0));
        // A member reaching past the centre is an error.
        let deep = vec![l((100.0, 50.0), (100.0, 2000.0), "STUD")];
        assert_eq!(
            lay_repeat(&arc_run(1000.0, 1.0), &outside_right, FaceRef::Exterior, &frame, &deep, p(400.0, 0.0), None).unwrap_err(),
            "repeat radius is not positive"
        );
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
        let mut l = AssemblyLine { from: p(100.0, -20.0), to: p(4900.0, -20.0), layer: "CLAD".into(), arc: None };
        mitre_end(&mut l, RunEnd::Start, p(100.0, 100.0), p(h, h));
        assert!((l.from.x + 20.0).abs() < 1e-9 && (l.from.y + 20.0).abs() < 1e-9, "{l:?}");
        assert_eq!(l.to, p(4900.0, -20.0));
        mitre_end(&mut l, RunEnd::End, p(4900.0, 100.0), p(-h, h));
        assert!((l.to.x - 5020.0).abs() < 1e-9, "{l:?}");
        // Parallel to the bisector: unchanged.
        let mut d = AssemblyLine { from: p(0.0, 0.0), to: p(10.0, 10.0), layer: "A".into(), arc: None };
        let before = d.clone();
        mitre_end(&mut d, RunEnd::Start, p(100.0, 0.0), p(h, h));
        assert_eq!(d, before);
    }

    /// An L of two 200 mm runs with its outer corner at (0,0): `a` along +x (outside below), `b`
    /// along +y (outside left), both starting at the junction (100,100) — or ending there, reversed.
    fn l_corner(reversed: bool) -> (Junction, Vec<WallRun>, Vec<RunSides>) {
        use crate::derivation::wall_junctions::JunctionArm;
        let (o, ax, by) = (p(100.0, 100.0), p(5000.0, 100.0), p(100.0, 5000.0));
        let right = RunSides { left: SideKind::Space(0), right: SideKind::Outside, exterior: true, open_outside_left: None };
        let left = RunSides { left: SideKind::Outside, right: SideKind::Space(0), exterior: true, open_outside_left: None };
        let (runs, sides, end) = if reversed {
            (vec![run(ax, o, 200.0), run(by, o, 200.0)], vec![left.clone(), right.clone()], RunEnd::End)
        } else {
            (vec![run(o, ax, 200.0), run(o, by, 200.0)], vec![right, left], RunEnd::Start)
        };
        let arms = vec![JunctionArm { run: 0, end, direction_deg: 0.0 }, JunctionArm { run: 1, end, direction_deg: 90.0 }];
        (Junction { point: o, arms, gaps_deg: Vec::new() }, runs, sides)
    }

    /// A T of 200 mm runs at (100,100): the through run along x as two runs (west, east, both
    /// starting at the node) and the branch along +y; arms in signature order, gaps [90, 90, 180].
    fn t_junction() -> (Junction, Vec<WallRun>) {
        use crate::derivation::wall_junctions::JunctionArm;
        let o = p(100.0, 100.0);
        let runs = vec![run(o, p(5000.0, 100.0), 200.0), run(o, p(100.0, 5000.0), 200.0), run(o, p(-5000.0, 100.0), 200.0)];
        let arms = vec![
            JunctionArm { run: 0, end: RunEnd::Start, direction_deg: 0.0 },
            JunctionArm { run: 1, end: RunEnd::Start, direction_deg: 90.0 },
            JunctionArm { run: 2, end: RunEnd::Start, direction_deg: 180.0 },
        ];
        (Junction { point: o, arms, gaps_deg: vec![90.0, 90.0, 180.0] }, runs)
    }

    #[test]
    fn tee_butt_finds_the_branch_and_the_through_run_s_face_on_its_side() {
        let (j, runs) = t_junction();
        let (b, point, dir) = tee_butt(&j, &runs).unwrap();
        assert_eq!(b, 1);
        assert!((point.x - 100.0).abs() < 1e-9 && (point.y - 200.0).abs() < 1e-9, "{point:?}");
        assert!((dir.x.abs() - 1.0).abs() < 1e-12 && dir.y.abs() < 1e-12, "{dir:?}");
        // Branch along -y: arms east, west, south (gaps 180, 90, 90) — the face below.
        use crate::derivation::wall_junctions::JunctionArm;
        let mut r2 = runs.clone();
        r2[1] = run(p(100.0, 100.0), p(100.0, -5000.0), 200.0);
        let arms = vec![
            JunctionArm { run: 0, end: RunEnd::Start, direction_deg: 0.0 },
            JunctionArm { run: 2, end: RunEnd::Start, direction_deg: 180.0 },
            JunctionArm { run: 1, end: RunEnd::Start, direction_deg: 270.0 },
        ];
        let j2 = Junction { point: j.point, arms, gaps_deg: vec![180.0, 90.0, 90.0] };
        let (b2, pt2, _) = tee_butt(&j2, &r2).unwrap();
        assert_eq!(b2, 2);
        assert!((pt2.x - 100.0).abs() < 1e-9 && pt2.y.abs() < 1e-9, "{pt2:?}");
        // Unequal through widths.
        let mut wide = runs.clone();
        wide[2].width = 300.0;
        assert_eq!(tee_butt(&j, &wide), None);
        // A Y, an X and an L.
        assert_eq!(tee_butt(&Junction { gaps_deg: vec![120.0, 120.0, 120.0], ..j.clone() }, &runs), None);
        let mut x = j.clone();
        x.arms.push(JunctionArm { run: 1, end: RunEnd::Start, direction_deg: 270.0 });
        x.gaps_deg = vec![90.0, 90.0, 90.0, 90.0];
        assert_eq!(tee_butt(&x, &runs), None);
        let (l, lruns, _) = l_corner(false);
        assert_eq!(tee_butt(&Junction { gaps_deg: vec![90.0, 270.0], ..l }, &lruns), None);
    }

    #[test]
    fn a_corner_detail_lands_on_the_outer_corner_along_the_matching_arm() {
        let reference = (p(-1000.0, 0.0), p(0.0, 0.0));
        let lines = [l((-1000.0, 200.0), (0.0, 200.0), "GYP"), l((0.0, 0.0), (0.0, 1000.0), "CLAD")];
        let want = [(200.0, 1000.0, 200.0, 0.0), (0.0, 0.0, 1000.0, 0.0)];
        for reversed in [false, true] {
            let (j, runs, sides) = l_corner(reversed);
            let out = place_corner(&j, &runs, &sides, FaceRef::Exterior, reference, &lines, &[], false).unwrap();
            assert_eq!(rounded(&out), want, "reversed {reversed}");
            assert_eq!(out[1].layer, "CLAD");
            assert!(out.iter().all(|l| l.arc.is_none()));
        }
        // The mirrored detail (wall side -y) runs along the other arm.
        let mirrored = [l((-1000.0, -200.0), (0.0, -200.0), "GYP"), l((0.0, 0.0), (0.0, -1000.0), "CLAD")];
        let (j, runs, sides) = l_corner(false);
        assert_eq!(
            rounded(&place_corner(&j, &runs, &sides, FaceRef::Exterior, reference, &mirrored, &[], false).unwrap()),
            [(1000.0, 200.0, 0.0, 200.0), (0.0, 0.0, 0.0, 1000.0)]
        );
    }

    #[test]
    fn a_corner_detail_refuses_a_non_t_three_arm_junction_and_parallel_arms() {
        use crate::derivation::wall_junctions::JunctionArm;
        let reference = (p(-1000.0, 0.0), p(0.0, 0.0));
        let lines = [l((-1000.0, 200.0), (0.0, 200.0), "GYP")];
        let (mut j, mut runs, mut sides) = l_corner(false);
        j.arms.push(JunctionArm { run: 0, end: RunEnd::End, direction_deg: 180.0 });
        assert_eq!(
            place_corner(&j, &runs, &sides, FaceRef::Exterior, reference, &lines, &[], false).unwrap_err(),
            "only L and T corners are placed yet"
        );
        j.arms.pop();
        runs[1] = run(p(100.0, 100.0), p(-5000.0, 100.0), 200.0);
        sides[1] = RunSides { left: SideKind::Outside, right: SideKind::Space(0), exterior: true, open_outside_left: None };
        assert_eq!(place_corner(&j, &runs, &sides, FaceRef::Exterior, reference, &lines, &[], false).unwrap_err(), "the corner's arms are parallel");
    }

    /// The corner detail of the adapter's tests: reference (−1000,0)–(0,0), `CRN-A` (0,0)–(0,1000),
    /// `CRN-B` band (−1000,200)–(0,200), wall side +y.
    fn crn() -> ((Point2, Point2), Vec<(Point2, Point2, String)>) {
        ((p(-1000.0, 0.0), p(0.0, 0.0)), vec![l((0.0, 0.0), (0.0, 1000.0), "CRN-A"), l((-1000.0, 200.0), (0.0, 200.0), "CRN-B")])
    }

    #[test]
    fn a_t_corner_detail_lands_on_the_far_face_at_the_foot_of_the_branch() {
        let (j, runs) = t_junction();
        // Outside below: the far face y = 0 is exterior; the branch is a partition.
        let right = RunSides { left: SideKind::Space(0), right: SideKind::Outside, exterior: true, open_outside_left: None };
        let left = RunSides { left: SideKind::Outside, right: SideKind::Space(0), exterior: true, open_outside_left: None };
        let both = RunSides { left: SideKind::Space(0), right: SideKind::Space(1), exterior: false, open_outside_left: None };
        let sides = vec![right, both, left];
        let (reference, lines) = crn();
        let out = place_corner(&j, &runs, &sides, FaceRef::Exterior, reference, &lines, &[], false).unwrap();
        assert_eq!(rounded(&out), [(100.0, 0.0, 100.0, 1000.0), (-900.0, 200.0, 100.0, 200.0)]);
        assert_eq!((out[0].layer.as_str(), out[1].layer.as_str()), ("CRN-A", "CRN-B"));
        // The interior face is the branch side.
        assert_eq!(
            place_corner(&j, &runs, &sides, FaceRef::Interior, reference, &lines, &[], false).unwrap_err(),
            "the detail's reference face is on the branch side"
        );
        // The mirrored detail (wall side −y) runs the other way along the same face.
        let mirrored: Vec<_> = lines.iter().map(|(a, b, n)| (p(a.x, -a.y), p(b.x, -b.y), n.clone())).collect();
        assert_eq!(
            rounded(&place_corner(&j, &runs, &sides, FaceRef::Exterior, reference, &mirrored, &[], false).unwrap()),
            [(100.0, 0.0, 100.0, 1000.0), (1100.0, 200.0, 100.0, 200.0)]
        );
        // A Y junction, and unequal through widths.
        let y = Junction { gaps_deg: vec![120.0, 120.0, 120.0], ..j.clone() };
        assert_eq!(
            place_corner(&y, &runs, &sides, FaceRef::Exterior, reference, &lines, &[], false).unwrap_err(),
            "only L and T corners are placed yet"
        );
        let mut wide = runs.clone();
        wide[2].width = 300.0;
        assert!(place_corner(&j, &wide, &sides, FaceRef::Exterior, reference, &lines, &[], false).is_err());
    }

    #[test]
    fn corner_insets_move_the_corner_and_outward_mirrors_across_the_face() {
        let reference = (p(-1000.0, 0.0), p(0.0, 0.0));
        let lines = [l((-1000.0, 200.0), (0.0, 200.0), "GYP"), l((0.0, 0.0), (0.0, 1000.0), "CLAD")];
        for reversed in [false, true] {
            let (j, runs, sides) = l_corner(reversed);
            let out = place_corner(&j, &runs, &sides, FaceRef::Exterior, reference, &lines, &[200.0, 200.0], false).unwrap();
            assert_eq!(rounded(&out), [(400.0, 1200.0, 400.0, 200.0), (200.0, 200.0, 1200.0, 200.0)], "reversed {reversed}");
            // Outward: the detail's wall side points out of the wall, so the other arm carries it.
            let out = place_corner(&j, &runs, &sides, FaceRef::Exterior, reference, &lines, &[100.0, 100.0], true).unwrap();
            assert_eq!(rounded(&out), [(1100.0, -100.0, 100.0, -100.0), (100.0, 100.0, 100.0, -900.0)], "reversed {reversed}");
        }
    }

    #[test]
    fn an_interior_face_corner_detail_lands_on_the_inner_corner() {
        let reference = (p(-1000.0, 0.0), p(0.0, 0.0));
        let lines = [l((-1000.0, 200.0), (0.0, 200.0), "GYP")];
        for reversed in [false, true] {
            let (j, runs, sides) = l_corner(reversed);
            let out = place_corner(
                &j,
                &runs,
                &sides,
                FaceRef::Interior,
                (reference.0, reference.1),
                &[l((-1000.0, 0.0), (0.0, 0.0), "REF"), lines[0].clone()],
                &[],
                false,
            )
            .unwrap();
            assert_eq!(rounded(&out[..1]), [(1200.0, 200.0, 200.0, 200.0)], "reversed {reversed}");
            assert_eq!(rounded(&out[1..]), [(1200.0, 0.0, 200.0, 0.0)], "reversed {reversed}");
        }
    }

    #[test]
    fn corner_reach_is_the_corner_geometry_s_extent_along_each_arm() {
        let lines = [l((-1000.0, 200.0), (0.0, 200.0), "GYP"), l((0.0, 0.0), (0.0, 1000.0), "CLAD")];
        for reversed in [false, true] {
            let (j, runs, sides) = l_corner(reversed);
            let placed = place_corner(&j, &runs, &sides, FaceRef::Exterior, (p(-1000.0, 0.0), p(0.0, 0.0)), &lines, &[], false).unwrap();
            assert_eq!(corner_reach(&j, &runs, &placed), vec![900.0, 900.0], "reversed {reversed}");
            assert_eq!(arm_direction(&j, &runs, 1), Some(p(0.0, 1.0)));
        }
        let (j, runs, _) = l_corner(false);
        let short = [AssemblyLine { from: p(0.0, 0.0), to: p(300.0, 50.0), layer: "A".into(), arc: None }];
        assert_eq!(corner_reach(&j, &runs, &short), vec![200.0, 0.0]);
        let mut arc = runs.clone();
        arc[0].centreline = RunCurve::Arc { center: p(0.0, 0.0), radius: 1000.0, start_angle: 0.0, sweep: 1.0 };
        assert_eq!(corner_reach(&j, &arc, &short)[0], 0.0);
    }

    #[test]
    fn trim_to_reach_clips_a_crossing_line_drops_one_inside_and_keeps_one_outside() {
        let (node, u) = (p(100.0, 100.0), p(1.0, 0.0));
        let line = |a: f64, b: f64| AssemblyLine { from: p(a, 0.0), to: p(b, 0.0), layer: "A".into(), arc: None };
        let mut crossing = line(0.0, 5000.0);
        assert!(trim_to_reach(&mut crossing, node, u, 900.0));
        assert_eq!(crossing, line(1000.0, 5000.0));
        let mut reversed = line(5000.0, 0.0);
        assert!(trim_to_reach(&mut reversed, node, u, 900.0));
        assert_eq!(reversed, line(5000.0, 1000.0));
        assert!(!trim_to_reach(&mut line(0.0, 900.0), node, u, 900.0));
        let mut outside = line(1000.0, 2000.0);
        assert!(trim_to_reach(&mut outside, node, u, 900.0));
        assert_eq!(outside, line(1000.0, 2000.0));
    }

    /// A 200-wide horizontal wall 0..1000 and 2000..3000 on centreline y = 100, exterior below (face
    /// y = 0), bridged 1000 → 2000; `reversed` draws both runs right to left.
    fn bridged(reversed: bool) -> (OpeningBridge, Vec<WallRun>, Vec<RunSides>) {
        let below_right = RunSides { left: SideKind::Space(0), right: SideKind::Outside, exterior: true, open_outside_left: None };
        let below_left = RunSides { left: SideKind::Outside, right: SideKind::Space(0), exterior: true, open_outside_left: None };
        let (runs, sides, a, b) = if reversed {
            (
                vec![run(p(1000.0, 100.0), p(0.0, 100.0), 200.0), run(p(3000.0, 100.0), p(2000.0, 100.0), 200.0)],
                vec![below_left.clone(), below_left],
                (0, RunEnd::Start),
                (1, RunEnd::End),
            )
        } else {
            (
                vec![run(p(0.0, 100.0), p(1000.0, 100.0), 200.0), run(p(2000.0, 100.0), p(3000.0, 100.0), 200.0)],
                vec![below_right.clone(), below_right],
                (0, RunEnd::End),
                (1, RunEnd::Start),
            )
        };
        (OpeningBridge { a, b, from: p(1000.0, 100.0), to: p(2000.0, 100.0), opening_entity_ids: vec![Id(9)] }, runs, sides)
    }

    /// A jamb detail: reference (0,0) → (100,0) on the exterior face, the jamb line at its `to` end
    /// and a band 200 into the wall reaching 100 back from the jamb.
    fn jamb_detail() -> ((Point2, Point2), Vec<(Point2, Point2, String)>) {
        ((p(0.0, 0.0), p(100.0, 0.0)), vec![l((100.0, 0.0), (100.0, 200.0), "JAMB"), l((0.0, 200.0), (100.0, 200.0), "GYP")])
    }

    #[test]
    fn an_opening_detail_lands_at_both_jambs_mirrored_at_the_second() {
        let (reference, lines) = jamb_detail();
        for reversed in [false, true] {
            let (bridge, runs, sides) = bridged(reversed);
            let [a, b] = place_opening(&bridge, &runs, &sides, FaceRef::Exterior, reference, &lines, &[], false).unwrap();
            assert_eq!(rounded(&a), [(1000.0, 0.0, 1000.0, 200.0), (900.0, 200.0, 1000.0, 200.0)], "reversed {reversed}");
            assert_eq!(rounded(&b), [(2000.0, 0.0, 2000.0, 200.0), (2100.0, 200.0, 2000.0, 200.0)], "reversed {reversed}");
            assert_eq!((a[0].layer.as_str(), a[1].layer.as_str()), ("JAMB", "GYP"));
            assert!(a.iter().chain(&b).all(|l| l.arc.is_none()));
            // Each jamb's reach back into its run is the band's 100.
            for (lines, (i, end)) in [(&a, bridge.a), (&b, bridge.b)] {
                let (c, t) = jamb_point(&runs[i], &sides[i], FaceRef::Exterior, end).unwrap();
                assert!((jamb_reach(c, t, lines) - 100.0).abs() < 1e-9, "reversed {reversed}, run {i}");
            }
        }
        // Nothing behind the jamb: reach 0.
        let ahead = [AssemblyLine { from: p(1000.0, 0.0), to: p(1200.0, 0.0), layer: "A".into(), arc: None }];
        assert_eq!(jamb_reach(p(1000.0, 0.0), p(1.0, 0.0), &ahead), 0.0);
    }

    #[test]
    fn a_jamb_inset_moves_both_jambs_into_the_wall() {
        let (reference, lines) = jamb_detail();
        let (bridge, runs, sides) = bridged(false);
        let [a, b] = place_opening(&bridge, &runs, &sides, FaceRef::Exterior, reference, &lines, &[100.0, 100.0], false).unwrap();
        assert_eq!(rounded(&a), [(1000.0, 100.0, 1000.0, 300.0), (900.0, 300.0, 1000.0, 300.0)]);
        assert_eq!(rounded(&b), [(2000.0, 100.0, 2000.0, 300.0), (2100.0, 300.0, 2000.0, 300.0)]);
        // Outward: the bands turn toward the face from the moved reference.
        let [a, _] = place_opening(&bridge, &runs, &sides, FaceRef::Exterior, reference, &lines, &[200.0, 200.0], true).unwrap();
        assert_eq!(rounded(&a), [(1000.0, 200.0, 1000.0, 0.0), (900.0, 0.0, 1000.0, 0.0)]);
    }

    #[test]
    fn an_opening_detail_on_the_interior_face_lands_on_the_top_face() {
        let (reference, lines) = jamb_detail();
        let (bridge, runs, sides) = bridged(false);
        let [a, b] = place_opening(&bridge, &runs, &sides, FaceRef::Interior, reference, &lines, &[], false).unwrap();
        assert_eq!(rounded(&a), [(1000.0, 200.0, 1000.0, 0.0), (900.0, 0.0, 1000.0, 0.0)]);
        assert_eq!(rounded(&b), [(2000.0, 200.0, 2000.0, 0.0), (2100.0, 0.0, 2000.0, 0.0)]);
    }

    #[test]
    fn an_opening_detail_refuses_an_arc_run_and_an_undecided_side() {
        let (reference, lines) = jamb_detail();
        let (bridge, mut runs, sides) = bridged(false);
        let neither = RunSides { left: SideKind::Space(0), right: SideKind::Space(1), exterior: false, open_outside_left: None };
        let undecided = vec![sides[0].clone(), neither];
        assert_eq!(
            place_opening(&bridge, &runs, &undecided, FaceRef::Exterior, reference, &lines, &[], false).unwrap_err(),
            "no single exterior side"
        );
        runs[0].centreline = RunCurve::Arc { center: p(0.0, 0.0), radius: 1000.0, start_angle: 0.0, sweep: 1.0 };
        assert_eq!(place_opening(&bridge, &runs, &sides, FaceRef::Exterior, reference, &lines, &[], false).unwrap_err(), "arc runs are not laid yet");
    }
}

//! Assembly lines (LAMB's `docs/07-architecture/cadcraft-integration.md` §10 "Generation", steps 1,
//! 2 and 4, straight runs only; LAMB T369): a detail view's **continuous bands** — its straight
//! lines parallel to the reference line — laid along a wall run, aligned on the run's matching
//! face. No junction trimming, no repeats; computed on demand.
//!
//! Not the AI-line sweep (`crate::sweeps` in the adapter); everything here is "assembly".

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

/// The bands of a detail whose reference line runs `reference.0 → reference.1`: every line parallel
/// to it (either direction, within `TOLERANCE.angle_deg`), its offset the signed distance of its
/// midpoint. The wall side is the side of the band farthest from the reference; a tie between the
/// two sides, or no bands at all, is an error rather than a guess. Sorted by offset, duplicates
/// (same offset within `TOLERANCE.coincidence_mm`, same layer) collapsed.
pub fn detail_bands(reference: (Point2, Point2), lines: &[(Point2, Point2, String)]) -> Result<Vec<Band>, String> {
    let (a, b) = reference;
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = dx.hypot(dy);
    if len <= TOLERANCE.coincidence_mm {
        return Err("the reference line has no length".into());
    }
    let (ux, uy) = (dx / len, dy / len);
    let max_sin = TOLERANCE.angle_deg.to_radians().sin();
    let mut bands: Vec<Band> = Vec::new();
    for (p, q, layer) in lines {
        let (lx, ly) = (q.x - p.x, q.y - p.y);
        let l = lx.hypot(ly);
        if l <= TOLERANCE.coincidence_mm || (ux * ly - uy * lx).abs() / l > max_sin {
            continue;
        }
        let (mx, my) = ((p.x + q.x) / 2.0, (p.y + q.y) / 2.0);
        // Left normal of the reference is (-uy, ux).
        let offset = (mx - a.x) * -uy + (my - a.y) * ux;
        bands.push(Band { offset, layer: layer.clone() });
    }
    let far = |sign: f64| bands.iter().map(|b| b.offset * sign).filter(|o| *o > 0.0).fold(0.0, f64::max);
    let (left, right) = (far(1.0), far(-1.0));
    if bands.is_empty() || (left - right).abs() <= TOLERANCE.coincidence_mm {
        return Err("the detail's wall side is ambiguous: its farthest bands on each side of the reference tie".into());
    }
    if right > left {
        for b in &mut bands {
            b.offset = -b.offset;
        }
    }
    bands.sort_by(|a, b| a.offset.total_cmp(&b.offset).then_with(|| a.layer.cmp(&b.layer)));
    let mut out: Vec<Band> = Vec::new();
    for b in bands {
        if !out.iter().any(|o| o.layer == b.layer && (o.offset - b.offset).abs() <= TOLERANCE.coincidence_mm) {
            out.push(b);
        }
    }
    Ok(out)
}

/// `bands` laid along a straight `run`: the face line is the centreline offset `width / 2` toward
/// the `face` side (the `Outside` side for `Exterior`, the other for `Interior`), each band that
/// face line shifted `offset` back toward the centreline, over the centreline's extent.
pub fn lay_bands(run: &WallRun, sides: &RunSides, face: FaceRef, bands: &[Band]) -> Result<Vec<AssemblyLine>, String> {
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
    let (nx, ny) = (-dy / len * s, dx / len * s);
    Ok(bands
        .iter()
        .map(|b| {
            let d = run.width / 2.0 - b.offset;
            let shift = |p: Point2| Point2 { x: p.x + nx * d, y: p.y + ny * d };
            AssemblyLine { from: shift(from), to: shift(to), layer: b.layer.clone() }
        })
        .collect())
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
}

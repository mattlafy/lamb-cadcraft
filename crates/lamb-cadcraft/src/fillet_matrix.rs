//! The 9-click-region FILLET matrix (TS `tools/fillet-matrix.ts`): for curve pairs whose full
//! curves cross, runs CADCraft's own two-entity `fillet {h1, p1, h2, p2, radius}` for every
//! combination of click region and prints beside each run what LAMB's corner rule
//! (`choose_shared_corner` / `resolve_ends_keeps`) says should happen. A report only — it changes
//! no fillet behaviour.
//!
//! ```text
//! cargo run -p lamb-cadcraft --example lamb_fillet_matrix -- [--json]
//! ```
//!
//! - Each edge gets three click regions relative to its own landmarks (the params of the two
//!   curves' apparent intersections): `before` both, `between` (35% from the nearer to the
//!   farther), `after` both. 3 x 3 = 9 runs per scenario, five scenarios, 45 runs.
//! - CADCraft's corner is the apparent intersection nearest the returned fillet arc's midpoint.
//! - `!!` marks a run where LAMB names a corner and CADCraft's differs (or CADCraft failed), or
//!   LAMB names none and CADCraft filleted anyway.
//! - `--json` prints the runs as one JSON array instead.
//!
//! Numbers are rounded to 3 dp. Exit 0 always (an inventory, not a gate); 1 on an unknown argument.

use std::io::Write;

use cadcraft_doc::{EntityKind, Handle};
use cadcraft_engine::Session;
use cadcraft_geom::Arc;
use lamb_core::geometry::fillet::{
    EndsPick, Keep, apparent_curve_intersections, bulge_for_arc, choose_shared_corner, curve_from_edge, param_along_edge, resolve_ends_keeps,
};
use lamb_core::geometry::point::Point2;
use serde::Serialize;
use serde_json::{Value, json};

const USAGE: &str = "usage: lamb_fillet_matrix [--json]";

/// One picked entity, as CADCraft creates it. Arcs are CCW, angles in degrees.
#[derive(Debug, Clone, Copy)]
pub enum EdgeDef {
    Line { p0: Point2, p1: Point2 },
    Arc { center: Point2, radius: f64, start: f64, end: f64 },
}

fn pt(x: f64, y: f64) -> Point2 {
    Point2 { x, y }
}

fn on_circle(c: Point2, r: f64, deg: f64) -> Point2 {
    let a = deg.to_radians();
    pt(c.x + r * a.cos(), c.y + r * a.sin())
}

impl EdgeDef {
    /// lamb-core's `(p0, p1, bulge)` form.
    pub fn lamb(&self) -> (Point2, Point2, f64) {
        match *self {
            EdgeDef::Line { p0, p1 } => (p0, p1, 0.0),
            EdgeDef::Arc { center, radius, start, end } => {
                let p0 = on_circle(center, radius, start);
                let p1 = on_circle(center, radius, end);
                (p0, p1, bulge_for_arc(center, true, p0, p1))
            }
        }
    }

    /// The point at edge param `t` (unclamped): the lerp on a line, `start + sweep·t` on an arc.
    pub fn point_at(&self, t: f64) -> Point2 {
        match *self {
            EdgeDef::Line { p0, p1 } => pt(p0.x + (p1.x - p0.x) * t, p0.y + (p1.y - p0.y) * t),
            EdgeDef::Arc { center, radius, start, end } => {
                let sweep = (end - start).rem_euclid(360.0);
                on_circle(center, radius, start + sweep * t)
            }
        }
    }

    fn create(&self, s: &mut Session) -> Result<String, String> {
        let (cmd, params) = match *self {
            EdgeDef::Line { p0, p1 } => ("line", json!({"points": [[p0.x, p0.y], [p1.x, p1.y]]})),
            EdgeDef::Arc { center, radius, start, end } => {
                ("arc", json!({"center": [center.x, center.y], "radius": radius, "start": start, "end": end}))
            }
        };
        let r = s.execute(cmd, &params).map_err(|e| e.to_string())?;
        r["handle"].as_str().or_else(|| r["handles"][0].as_str()).map(str::to_owned).ok_or_else(|| format!("{cmd} returned no handle"))
    }
}

/// One scenario: two edges and a fillet radius.
#[derive(Debug, Clone)]
pub struct Pair {
    pub label: &'static str,
    pub a: EdgeDef,
    pub b: EdgeDef,
}

/// TS's five scenarios, same numbers: `(pair, radius)`.
pub fn scenarios() -> Vec<(Pair, f64)> {
    let arc_arc = Pair {
        label: "arc x arc",
        a: EdgeDef::Arc { center: pt(0.0, 0.0), radius: 5.0, start: -110.0, end: 190.0 },
        b: EdgeDef::Arc { center: pt(6.0, 0.0), radius: 5.0, start: 60.0, end: 300.0 },
    };
    let line_arc = Pair {
        label: "line x arc",
        a: EdgeDef::Line { p0: pt(-10.0, 3.0), p1: pt(10.0, 3.0) },
        b: EdgeDef::Arc { center: pt(0.0, 0.0), radius: 5.0, start: -20.0, end: 200.0 },
    };
    let line_line = Pair {
        label: "line x line (control)",
        a: EdgeDef::Line { p0: pt(0.0, 0.0), p1: pt(100.0, 0.0) },
        b: EdgeDef::Line { p0: pt(50.0, -50.0), p1: pt(50.0, 50.0) },
    };
    vec![(arc_arc.clone(), 2.0), (arc_arc, 4.0), (line_arc.clone(), 2.0), (line_arc, 3.5), (line_line, 8.0)]
}

/// One of the 9 runs.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub scenario: String,
    pub radius: f64,
    pub region_a: &'static str,
    pub region_b: &'static str,
    pub click_a: Point2,
    pub click_b: Point2,
    /// The apparent intersections of the two full curves.
    pub intersections: Vec<Point2>,
    /// Each intersection's param along edge A / edge B.
    pub landmarks_a: Vec<f64>,
    pub landmarks_b: Vec<f64>,
    pub cadcraft_corner: Option<Point2>,
    pub fillet_center: Option<Point2>,
    pub fillet_radius: Option<f64>,
    /// `Err` text, or "no arc" for `arc: null`.
    pub cadcraft_error: Option<String>,
    /// Both entities' endpoints after the fillet (start, end).
    pub ends_a: Option<[Point2; 2]>,
    pub ends_b: Option<[Point2; 2]>,
    pub lamb_corner: Option<Point2>,
    pub keep_a: Keep,
    pub keep_b: Keep,
    pub disagree: bool,
}

const REGIONS: [&str; 3] = ["before", "between", "after"];

/// TS `clickRegions`: the params of `before` / `between` / `after` from an edge's landmarks.
fn region_params(landmarks: &[f64]) -> [f64; 3] {
    let lo = landmarks.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = landmarks.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let (lo, hi) = if lo.is_finite() { (lo, hi) } else { (0.5, 0.5) };
    let margin = 0.05_f64.max(0.2_f64.min(lo - 0.02).min(0.98 - hi));
    [lo - margin, lo + 0.35 * (hi - lo), hi + margin]
}

fn dist(a: Point2, b: Point2) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

fn ends(s: &Session, h: &str) -> Option<[Point2; 2]> {
    let d = s.doc().ok()?;
    let e = d.entity(Handle::parse_hex(h)?)?;
    match &e.kind {
        EntityKind::Line(l) => Some([pt(l.a.x, l.a.y), pt(l.b.x, l.b.y)]),
        EntityKind::Arc(a) => {
            let g = Arc::new(a.center.xy(), a.radius, a.start, a.end);
            let (s0, e0) = (g.start_point(), g.end_point());
            Some([pt(s0.x, s0.y), pt(e0.x, e0.y)])
        }
        _ => None,
    }
}

struct Cad {
    corner: Option<Point2>,
    center: Option<Point2>,
    radius: Option<f64>,
    error: Option<String>,
    ends_a: Option<[Point2; 2]>,
    ends_b: Option<[Point2; 2]>,
}

fn run_cadcraft(pair: &Pair, click_a: Point2, click_b: Point2, radius: f64, pts: &[Point2]) -> Cad {
    let mut cad = Cad { corner: None, center: None, radius: None, error: None, ends_a: None, ends_b: None };
    let mut s = Session::new();
    let handles = pair.a.create(&mut s).and_then(|ha| Ok((ha, pair.b.create(&mut s)?)));
    let (ha, hb) = match handles {
        Ok(h) => h,
        Err(e) => {
            cad.error = Some(e);
            return cad;
        }
    };
    let params = json!({"h1": ha, "p1": [click_a.x, click_a.y], "h2": hb, "p2": [click_b.x, click_b.y], "radius": radius});
    match s.execute("fillet", &params) {
        Err(e) => cad.error = Some(e.to_string()),
        Ok(r) => match r.get("arc").and_then(Value::as_str) {
            None => cad.error = Some("no arc".to_owned()),
            Some(h) => {
                let arc = s.doc().ok().and_then(|d| Handle::parse_hex(h).and_then(|h| d.entity(h))).and_then(|e| match &e.kind {
                    EntityKind::Arc(a) => Some(Arc::new(a.center.xy(), a.radius, a.start, a.end)),
                    _ => None,
                });
                match arc {
                    None => cad.error = Some(format!("arc {h} not found")),
                    Some(g) => {
                        let m = g.mid_point();
                        let mid = pt(m.x, m.y);
                        cad.center = Some(pt(g.center.x, g.center.y));
                        cad.radius = Some(g.radius);
                        cad.corner = pts.iter().copied().min_by(|p, q| dist(*p, mid).total_cmp(&dist(*q, mid)));
                    }
                }
            }
        },
    }
    cad.ends_a = ends(&s, &ha);
    cad.ends_b = ends(&s, &hb);
    cad
}

/// The 9 runs of one scenario.
pub fn matrix(pair: &Pair, radius: f64) -> Vec<Run> {
    let (a0, a1, ab) = pair.a.lamb();
    let (b0, b1, bb) = pair.b.lamb();
    let pts = apparent_curve_intersections(&curve_from_edge(a0, a1, ab), &curve_from_edge(b0, b1, bb));
    let landmarks_a: Vec<f64> = pts.iter().map(|&p| param_along_edge(a0, a1, ab, p)).collect();
    let landmarks_b: Vec<f64> = pts.iter().map(|&p| param_along_edge(b0, b1, bb, p)).collect();
    let (ta, tb) = (region_params(&landmarks_a), region_params(&landmarks_b));
    let mut runs = Vec::with_capacity(9);
    for (ia, &region_a) in REGIONS.iter().enumerate() {
        for (ib, &region_b) in REGIONS.iter().enumerate() {
            let click_a = pair.a.point_at(ta[ia]);
            let click_b = pair.b.point_at(tb[ib]);
            let lamb_corner = choose_shared_corner(&pts, a0, a1, ab, click_a, b0, b1, bb, click_b);
            let keeps =
                resolve_ends_keeps(&EndsPick { p0: a0, p1: a1, bulge: ab, click: click_a }, &EndsPick { p0: b0, p1: b1, bulge: bb, click: click_b });
            let cad = run_cadcraft(pair, click_a, click_b, radius, &pts);
            let disagree = match (lamb_corner, cad.corner) {
                (Some(l), Some(c)) => dist(l, c) > 1e-6,
                (Some(_), None) => true,
                (None, Some(_)) => true,
                (None, None) => false,
            };
            runs.push(Run {
                scenario: pair.label.to_owned(),
                radius,
                region_a,
                region_b,
                click_a,
                click_b,
                intersections: pts.clone(),
                landmarks_a: landmarks_a.clone(),
                landmarks_b: landmarks_b.clone(),
                cadcraft_corner: cad.corner,
                fillet_center: cad.center,
                fillet_radius: cad.radius,
                cadcraft_error: cad.error,
                ends_a: cad.ends_a,
                ends_b: cad.ends_b,
                lamb_corner,
                keep_a: keeps.keep_a,
                keep_b: keeps.keep_b,
                disagree,
            });
        }
    }
    runs
}

fn r(n: f64) -> String {
    let v = (n * 1000.0).round() / 1000.0;
    format!("{}", if v == 0.0 { 0.0 } else { v })
}

fn p(q: Point2) -> String {
    format!("({},{})", r(q.x), r(q.y))
}

fn opt_p(q: Option<Point2>) -> String {
    q.map_or_else(|| "none".to_owned(), p)
}

fn keep(k: Keep) -> &'static str {
    match k {
        Keep::Before => "before",
        Keep::After => "after",
    }
}

fn line(run: &Run) -> String {
    let cad = match (&run.cadcraft_error, run.cadcraft_corner) {
        (Some(e), _) => format!("cadcraft ERROR {e}"),
        (None, c) => {
            format!("cadcraft corner {} arc c{} r{}", opt_p(c), opt_p(run.fillet_center), run.fillet_radius.map_or_else(|| "?".to_owned(), r))
        }
    };
    format!(
        "{}A {:<7} {} / B {:<7} {} | {} | lamb corner {} keep {}/{}",
        if run.disagree { "!! " } else { "   " },
        run.region_a,
        p(run.click_a),
        run.region_b,
        p(run.click_b),
        cad,
        opt_p(run.lamb_corner),
        keep(run.keep_a),
        keep(run.keep_b)
    )
}

/// Runs the command line `args` (without the program name); returns the exit code.
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let mut as_json = false;
    for a in args {
        match a.as_str() {
            "--json" => as_json = true,
            other => {
                let _ = writeln!(err, "unknown argument {other}\n{USAGE}");
                return 1;
            }
        }
    }
    let mut all = Vec::new();
    let mut text = String::new();
    for (pair, radius) in scenarios() {
        let runs = matrix(&pair, radius);
        text.push_str(&format!("=== {} (radius={}) ===\n", pair.label, r(radius)));
        for run in &runs {
            text.push_str(&line(run));
            text.push('\n');
        }
        all.extend(runs);
    }
    let flagged = all.iter().filter(|r| r.disagree).count();
    let res = if as_json {
        match serde_json::to_string_pretty(&all) {
            Ok(s) => writeln!(out, "{s}"),
            Err(e) => writeln!(err, "{e}"),
        }
    } else {
        writeln!(out, "{text}{flagged} of {} runs disagree", all.len())
    };
    if let Err(e) = res {
        let _ = writeln!(err, "{e}");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: Point2, x: f64, y: f64) -> bool {
        dist(a, pt(x, y)) < 1e-6
    }

    #[test]
    fn line_line_control_corner_is_the_crossing() {
        let (pair, radius) = scenarios().into_iter().find(|(p, _)| p.label.starts_with("line x line")).unwrap();
        let runs = matrix(&pair, radius);
        assert_eq!(runs.len(), 9);
        for run in &runs {
            assert_eq!(run.intersections.len(), 1);
            assert!(near(run.intersections[0], 50.0, 0.0));
            assert!(run.lamb_corner.is_some_and(|c| near(c, 50.0, 0.0)), "{run:?}");
            if let Some(c) = run.cadcraft_corner {
                assert!(near(c, 50.0, 0.0), "{run:?}");
            } else {
                assert!(run.region_a == "between" || run.region_b == "between", "{run:?}");
            }
        }
    }

    #[test]
    fn arc_arc_has_two_landmarks_and_an_outcome_per_run() {
        let (pair, radius) = scenarios().into_iter().find(|(p, r)| p.label == "arc x arc" && *r == 2.0).unwrap();
        let runs = matrix(&pair, radius);
        assert_eq!(runs.len(), 9);
        for run in &runs {
            assert_eq!(run.landmarks_a.len(), 2);
            assert_eq!(run.landmarks_b.len(), 2);
            let mut pts = run.intersections.clone();
            pts.sort_by(|p, q| p.y.total_cmp(&q.y));
            assert!(near(pts[0], 3.0, -4.0) && near(pts[1], 3.0, 4.0), "{pts:?}");
            assert!(run.cadcraft_corner.is_some() || run.cadcraft_error.is_some(), "{run:?}");
        }
    }

    #[test]
    fn json_is_45_runs_and_bad_flag_fails() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run(&["--json".to_owned()], &mut out, &mut err), 0);
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v.as_array().map(Vec::len), Some(45));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run(&["--bogus".to_owned()], &mut out, &mut err), 1);
    }
}

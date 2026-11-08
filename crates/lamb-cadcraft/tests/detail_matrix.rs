//! LAMB T675: one detail tag on a closed wall made by OFFSET details every run of it, once, on the
//! side its reference says. The matrix: an outer 5000 × 4000 rectangle on `WALL-EXT`, its other
//! face made by the `offset` command inward or outward at 100, 200 and 400 mm; a detail `D1` with
//! an `exterior_face`, `interior_face` or `structural_centre` reference and bands on both sides of
//! it; one edge tag on each side of the outer polyline in turn, bubble outside or inside.
//!
//! Each case passes when every run resolves to the detail, each run lays its three bands exactly
//! once at the expected distances from the outer face (so an exterior-face detail's outermost band
//! sits outside the outer rectangle on all four sides), each laid line keeps the layer of the detail
//! line it came from, no two lines of the assembly block overlap, and RESOLVE lists nothing for the
//! plan beyond undetailed corners (corner details are out of scope here).
//!
//! The command registry is process-global, so registration goes through `install_app`.

use std::collections::HashSet;

use cadcraft_doc::{EntityKind, Handle};
use cadcraft_engine::Session;
use lamb_cadcraft::assembly::assembly_lines;
use lamb_cadcraft::detailtags::resolve_view_details;
use lamb_cadcraft::schema::assembly_block_name;
use lamb_core::derivation::detail_tags::RunDetail;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::Id;
use serde_json::json;

type T<X> = Result<X, Box<dyn std::error::Error>>;

const W: f64 = 5000.0;
const H: f64 = 4000.0;

fn all_handles(s: &Session) -> T<HashSet<Handle>> {
    let d = s.doc()?;
    Ok(d.model.iter().map(|e| e.handle).chain(d.blocks.values().flat_map(|b| b.entities.iter().map(|e| e.handle))).collect())
}

fn new_handle(s: &mut Session, cmd: &str, p: serde_json::Value) -> T<Handle> {
    let before = all_handles(s)?;
    s.execute(cmd, &p)?;
    Ok(all_handles(s)?.into_iter().find(|h| !before.contains(h)).ok_or("nothing drawn")?)
}

fn view_id(v: serde_json::Value) -> T<Id> {
    Ok(Id(v["viewId"].as_u64().ok_or("no id")?))
}

#[derive(Clone, Copy, Debug)]
struct Case {
    inward: bool,
    t: f64,
    kind: &'static str,
    /// 0 bottom, 1 right, 2 top, 3 left.
    side: usize,
    bubble_out: bool,
}

/// The detail's band offsets from its reference (positive into the wall), layers `BA`, `BB`, `BC`
/// in that order; and each band's expected distance into the wall from the outer face.
fn bands(kind: &str, t: f64) -> ([f64; 3], [f64; 3]) {
    match kind {
        "exterior_face" => ([-20.0, t / 2.0, t], [-20.0, t / 2.0, t]),
        "interior_face" => ([-20.0, t / 2.0, t], [t + 20.0, t / 2.0, 0.0]),
        _ => ([-t / 4.0, t / 4.0, 3.0 * t / 4.0], [t, t / 2.0, 0.0]),
    }
}

const LAYERS: [&str; 3] = ["BA", "BB", "BC"];

/// Builds the case's drawing, tags it and writes its assembly block. Returns (session, plan,
/// outer rectangle as (x0, y0, x1, y1)).
fn build(c: Case) -> T<(Session, Id, [f64; 4])> {
    lamb_cadcraft::commands::install_app()?;
    let mut s = Session::new();
    s.new_drawing(true);
    let plan = view_id(s.execute("newplan", &json!({ "name": "Ground" }))?)?;
    let detail = view_id(s.execute("newdetail", &json!({ "name": "D1" }))?)?;
    s.execute("editview", &json!({ "viewId": detail.0 }))?;
    let r = new_handle(&mut s, "line", json!({ "points": [[0.0, 0.0], [1000.0, 0.0]] }))?;
    s.execute("detailref", &json!({ "handle": format!("{:X}", r.0), "kind": c.kind }))?;
    for (off, layer) in bands(c.kind, c.t).0.into_iter().zip(LAYERS) {
        s.execute("layer.new", &json!({ "name": layer, "current": true }))?;
        s.execute("line", &json!({ "points": [[0.0, off], [1000.0, off]] }))?;
    }
    s.execute("editview", &json!({ "viewId": plan.0 }))?;
    s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
    let ring = [[0.0, 0.0], [W, 0.0], [W, H], [0.0, H]];
    let pl = new_handle(&mut s, "pline", json!({ "vertices": ring, "closed": true }))?;
    let side = if c.inward { [W / 2.0, H / 2.0] } else { [-1000.0, -1000.0] };
    s.execute("offset", &json!({ "handle": pl.hex(), "distance": c.t, "side": { "x": side[0], "y": side[1] } }))?;
    let outer = if c.inward { [0.0, 0.0, W, H] } else { [-c.t, -c.t, W + c.t, H + c.t] };
    let [x0, y0, x1, y1] = outer;
    let (mx, my) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let b = if c.bubble_out { -1000.0 } else { 1000.0 };
    let (tip, bubble) = match c.side {
        0 => ([mx, y0], [mx, y0 + b]),
        1 => ([x1, my], [x1 - b, my]),
        2 => ([mx, y1], [mx, y1 - b]),
        _ => ([x0, my], [x0 + b, my]),
    };
    s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
    s.execute("detailtag", &json!({ "detailViewId": detail.0, "tip": {"x": tip[0], "y": tip[1]}, "bubble": {"x": bubble[0], "y": bubble[1]} }))?;
    s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
    Ok((s, plan, outer))
}

/// Which side of the outer rectangle an axis-aligned line runs along, and its distance into the
/// wall from that side's outer face; `None` for a slanted line.
fn side_of(from: [f64; 2], to: [f64; 2], o: [f64; 4]) -> Option<(usize, f64)> {
    let tol = TOLERANCE.coincidence_mm;
    let [x0, y0, x1, y1] = o;
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    if (from[1] - to[1]).abs() <= tol {
        let y = from[1];
        return Some(if y < cy { (0, y - y0) } else { (2, y1 - y) });
    }
    if (from[0] - to[0]).abs() <= tol {
        let x = from[0];
        return Some(if x > cx { (1, x1 - x) } else { (3, x - x0) });
    }
    None
}

/// Every failure of case `c`, as one-line reasons.
fn check(c: Case) -> T<Vec<String>> {
    let (mut s, plan, outer) = build(c)?;
    let issues = s.execute("resolve", &json!({}))?["issues"].as_array().cloned().unwrap_or_default();
    let d = s.doc()?;
    let tol = TOLERANCE.coincidence_mm;
    let mut bad = Vec::new();

    let (runs_detail, _) = resolve_view_details(d, plan)?;
    if runs_detail.len() != 4 || runs_detail.iter().any(|r| matches!(r, RunDetail::Untyped | RunDetail::Conflict)) {
        bad.push(format!("resolved {runs_detail:?}"));
    }

    let runs = assembly_lines(d, plan)?;
    if runs.len() != 4 {
        bad.push(format!("{} runs", runs.len()));
    }
    let expected = bands(c.kind, c.t).1;
    let mut per_side: [Vec<(f64, String)>; 4] = Default::default();
    for r in &runs {
        if r.detail.is_none() || r.skipped.is_some() || r.bands != 3 {
            bad.push(format!("run {}: detail {:?} skipped {:?} bands {}", r.run, r.detail, r.skipped, r.bands));
        }
        if let Some(w) = r.conflict {
            bad.push(format!("run {}: width conflict {}", r.run, w.text()));
        }
        for l in &r.lines[..r.bands.min(r.lines.len())] {
            match side_of([l.from.x, l.from.y], [l.to.x, l.to.y], outer) {
                Some((side, dist)) => per_side[side].push((dist, l.layer.clone())),
                None => bad.push(format!("run {}: slanted band {l:?}", r.run)),
            }
        }
    }
    for (side, got) in per_side.iter_mut().enumerate() {
        got.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut want: Vec<(f64, String)> = expected.iter().copied().zip(LAYERS.iter().map(|l| l.to_string())).collect();
        want.sort_by(|a, b| a.0.total_cmp(&b.0));
        let same = got.len() == want.len() && got.iter().zip(&want).all(|(g, w)| (g.0 - w.0).abs() <= tol && g.1 == w.1);
        if !same {
            let show = |v: &[(f64, String)]| v.iter().map(|(o, l)| format!("{o:.0}{l}")).collect::<Vec<_>>().join(",");
            bad.push(format!("side {side}: bands [{}] want [{}]", show(got), show(&want)));
        }
    }

    // No two lines of the whole assembly block overlap along their length.
    let block = d.block(&assembly_block_name(plan)).ok_or("no assembly block")?;
    let segs: Vec<([f64; 2], [f64; 2], String)> = block
        .entities
        .iter()
        .filter_map(|e| match &e.kind {
            EntityKind::Line(l) => Some(([l.a.x, l.a.y], [l.b.x, l.b.y], e.common.layer.clone())),
            _ => None,
        })
        .collect();
    let mut overlaps = 0;
    for (i, a) in segs.iter().enumerate() {
        for b in &segs[i + 1..] {
            if overlap(a.0, a.1, b.0, b.1, tol) {
                overlaps += 1;
            }
        }
    }
    if overlaps > 0 {
        bad.push(format!("{overlaps} overlapping block line pairs"));
    }
    let layers: HashSet<&str> = segs.iter().map(|s| s.2.as_str()).collect();
    if !layers.iter().all(|l| LAYERS.contains(l)) {
        bad.push(format!("block layers {layers:?}"));
    }

    let mine: Vec<String> = issues
        .into_iter()
        .filter(|i| i["viewId"].as_u64() == Some(plan.0) && i["kind"] != "undetailed_corner")
        .map(|i| i["note"].as_str().unwrap_or("?").to_owned())
        .collect();
    if !mine.is_empty() {
        bad.push(format!("issues {mine:?}"));
    }
    Ok(bad)
}

/// `true` when segments `a0–a1` and `b0–b1` are collinear and share more than `tol` of length.
fn overlap(a0: [f64; 2], a1: [f64; 2], b0: [f64; 2], b1: [f64; 2], tol: f64) -> bool {
    let (dx, dy) = (a1[0] - a0[0], a1[1] - a0[1]);
    let len = dx.hypot(dy);
    if len <= tol {
        return false;
    }
    let (ux, uy) = (dx / len, dy / len);
    let off = |p: [f64; 2]| ((p[0] - a0[0]) * uy - (p[1] - a0[1]) * ux).abs();
    if off(b0) > tol || off(b1) > tol {
        return false;
    }
    let along = |p: [f64; 2]| (p[0] - a0[0]) * ux + (p[1] - a0[1]) * uy;
    let (s, e) = {
        let (p, q) = (along(b0), along(b1));
        (p.min(q), p.max(q))
    };
    e.min(len) - s.max(0.0) > tol
}

#[test]
fn one_tag_on_an_offset_closed_wall_details_every_run_once() -> T<()> {
    let mut failures = Vec::new();
    let mut n = 0;
    for inward in [true, false] {
        for t in [100.0, 200.0, 400.0] {
            for kind in ["exterior_face", "interior_face", "structural_centre"] {
                for side in 0..4 {
                    for bubble_out in [true, false] {
                        let c = Case { inward, t, kind, side, bubble_out };
                        n += 1;
                        match check(c) {
                            Ok(bad) if bad.is_empty() => {}
                            Ok(bad) => failures.push(format!("{c:?} | {}", bad.join(" | "))),
                            Err(e) => failures.push(format!("{c:?} | error: {e}")),
                        }
                    }
                }
            }
        }
    }
    assert_eq!(n, 144);
    assert!(failures.is_empty(), "{} of {n} cases fail:\n{}", failures.len(), failures.join("\n"));
    Ok(())
}

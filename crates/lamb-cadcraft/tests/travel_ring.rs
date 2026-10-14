//! The travel ring and the marker-click travel toggle (TS T110 `drawTravelRing`,
//! `toggleTravelLink`), on the imported `u-wall-two-sections`. One test in its own binary: the
//! travel mirror (`viewpick::travelling`) is process-global.

use std::collections::BTreeMap;
use std::path::Path;

use cadcraft_doc::Handle;
use cadcraft_engine::Session;
use cadcraft_engine::render::{Kind, Options, overlays_generation};
use cadcraft_geom::Vec2;
use lamb_cadcraft::{DrawingProject, commands, install, lamb_overlay, project_to_drawing};
use lamb_core::derivation::linking::find_link_for_marker;
use lamb_core::derivation::markers::{Marker, compute_markers_for_view};
use lamb_core::derivation::read::{ProjectRead, ViewKind};
use lamb_core::geometry::polyline::resolved_segments;
use lamb_core::model::{Id, Project};
use lamb_core::persistence::files_to_project;
use serde_json::json;

type R = Result<(), Box<dyn std::error::Error>>;
type E = Box<dyn std::error::Error>;

fn load(name: &str) -> Result<Project, E> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb").join(format!("{name}.lamb.json"));
    let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(files_to_project(&files).map_err(|e| format!("{e:?}"))?)
}

/// The session editing the placed plan (a tall viewport, so the pick aperture is well inside a
/// marker), and the plan's id.
fn editing_plan() -> Result<(Session, Id), E> {
    commands::install_app().map_err(|e| e.to_string())?;
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let mut s = Session::new();
    s.open_drawing(d, "u-wall", None);
    install(&mut s);
    s.viewport_px = (3000.0, 2000.0);
    let plan = {
        let dp = DrawingProject::new(s.doc()?)?;
        dp.views().into_iter().find(|v| v.kind == ViewKind::Plan && v.placement.is_some()).ok_or("no placed plan")?.id
    };
    s.execute("editview", &json!({"viewId": plan.0}))?;
    Ok((s, plan))
}

/// Each marker of the plan with its link id (if any).
fn markers(s: &Session, plan: Id) -> Result<Vec<(Marker, Option<Id>)>, E> {
    let dp = DrawingProject::new(s.doc()?)?;
    let ms = compute_markers_for_view(&dp, plan)?;
    Ok(ms.into_iter().map(|m| (m.clone(), find_link_for_marker(&dp, &m, plan).map(|l| l.id))).collect())
}

/// The max radii of the closed polylines drawn around marker `m` (its outline, then any ring).
fn outlines(s: &Session, m: &Marker) -> Result<Vec<f64>, E> {
    let list = lamb_overlay(s.doc()?, &s.space(), &Options::default());
    let c = Vec2::new(m.position.x, m.position.y);
    let mut out = Vec::new();
    for p in &list.prims {
        if p.handle != Handle(m.source_entity_id.0) || !matches!(p.kind, Kind::Polyline) || p.len < 4 {
            continue;
        }
        let pts = &list.verts[p.start as usize..(p.start + p.len) as usize];
        if pts.first() != pts.last() {
            continue;
        }
        let n = (pts.len() - 1) as f64;
        let centroid = pts[..pts.len() - 1].iter().fold(Vec2::new(0.0, 0.0), |a, q| Vec2::new(a.x + q.x / n, a.y + q.y / n));
        if centroid.dist(c) < 1e-6 {
            out.push(pts.iter().map(|q| q.dist(c)).fold(0.0, f64::max));
        }
    }
    Ok(out)
}

/// Rings drawn per marker id.
fn rings(s: &Session, plan: Id) -> Result<BTreeMap<String, usize>, E> {
    let mut out = BTreeMap::new();
    for (m, _) in markers(s, plan)? {
        let o = outlines(s, &m)?;
        assert!(!o.is_empty(), "marker {} has its outline", m.id);
        if o.len() == 2 {
            assert!(o[1] > o[0], "the ring sits outside the marker: {o:?}");
        }
        out.insert(m.id.clone(), o.len() - 1);
    }
    Ok(out)
}

/// The rings expected for a travel set: one on each marker whose link travels.
fn expected(s: &Session, plan: Id) -> Result<BTreeMap<String, usize>, E> {
    let travel = s.travel();
    Ok(markers(s, plan)?.into_iter().map(|(m, l)| (m.id, usize::from(l.is_some_and(|l| travel.contains(&l.0))))).collect())
}

/// Distance from `p` to the nearest segment (chord) of the selected entities' polylines.
fn to_selection(s: &Session, p: Vec2) -> Result<f64, E> {
    let dp = DrawingProject::new(s.doc()?)?;
    let mut best = f64::INFINITY;
    for h in s.selection() {
        let Some(poly) = dp.entity(Id(h.0)).and_then(|e| e.polyline.map(std::borrow::Cow::into_owned)) else { continue };
        for g in resolved_segments(&poly) {
            let (dx, dy) = (g.to.x - g.from.x, g.to.y - g.from.y);
            let len2 = (dx * dx + dy * dy).max(1e-12);
            let t = (((p.x - g.from.x) * dx + (p.y - g.from.y) * dy) / len2).clamp(0.0, 1.0);
            best = best.min(p.dist(Vec2::new(g.from.x + t * dx, g.from.y + t * dy)));
        }
    }
    Ok(best)
}

/// A point inside marker `m`, off the selected wall it sits on: 0.8 of the marker radius from its
/// centre, in whichever of eight directions is farthest from the selection.
fn inside_off_wall(s: &Session, m: &Marker) -> Result<Vec2, E> {
    let r = s.state()?.view().height / 150.0;
    let mut best = (f64::NEG_INFINITY, Vec2::new(m.position.x, m.position.y));
    for i in 0..8 {
        let t = std::f64::consts::TAU * f64::from(i) / 8.0;
        let p = Vec2::new(m.position.x + 0.8 * r * t.cos(), m.position.y + 0.8 * r * t.sin());
        let d = to_selection(s, p)?;
        if d > best.0 {
            best = (d, p);
        }
    }
    Ok(best.1)
}

#[test]
fn a_crossing_box_rings_its_links_and_a_marker_click_toggles_one() -> R {
    let (mut s, plan) = editing_plan()?;
    assert!(rings(&s, plan)?.values().all(|n| *n == 0), "no rings before a gather");

    // Crossing box over the plan wall: the wall's links travel and their markers ring.
    let g = overlays_generation();
    assert!(s.begin_window(Vec2::new(5100.0, 100.0)));
    s.idle_click(Vec2::new(4900.0, -100.0), false)?;
    assert!(overlays_generation() > g, "the travel set redraws the overlays");
    let sel = s.selection();
    let seeded = s.travel();
    assert!(!sel.is_empty() && !seeded.is_empty(), "crossing box seeded the set");
    assert_eq!(rings(&s, plan)?, expected(&s, plan)?);
    assert!(rings(&s, plan)?.values().any(|n| *n == 1));

    // Click inside a ringed filled marker, off its wall: its link stays behind, selection kept.
    let (m, link) =
        markers(&s, plan)?.into_iter().find(|(m, l)| m.filled && l.is_some_and(|l| seeded.contains(&l.0))).ok_or("no ringed filled marker")?;
    let link = link.ok_or("no link")?;
    let at = inside_off_wall(&s, &m)?;
    s.idle_click(at, false)?;
    assert_eq!(s.selection(), sel, "selection kept");
    assert!(s.pending_window.is_none(), "no window opened");
    assert!(!s.travel().contains(&link.0), "the link left the set");
    assert_eq!(s.travel().len(), seeded.len() - 1, "the others kept");
    let now = rings(&s, plan)?;
    assert_eq!(now.get(&m.id), Some(&0), "its ring is gone");
    assert_eq!(now, expected(&s, plan)?);

    // Again: back in.
    s.idle_click(at, false)?;
    assert!(s.travel().contains(&link.0));
    assert_eq!(rings(&s, plan)?.get(&m.id), Some(&1));
    assert_eq!(s.selection(), sel);

    // Esc: no rings.
    s.cancel();
    assert!(s.travel().is_empty());
    assert!(rings(&s, plan)?.values().all(|n| *n == 0), "Esc clears every ring");

    // Nothing selected: a marker click toggles nothing.
    s.idle_click(at, false)?;
    assert!(s.travel().is_empty() && s.selection().is_empty(), "no toggle without a selection");
    Ok(())
}

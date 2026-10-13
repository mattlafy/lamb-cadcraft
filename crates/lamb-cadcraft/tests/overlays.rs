//! LAMB's overlay and snap providers (T305): an edited view's markers and cut lines as a display
//! list, markers as snap hits. The provider registries are process-global, so this is its own test
//! binary with a single test fn.

use std::collections::BTreeMap;
use std::path::Path;

use cadcraft_doc::{Drawing, Handle, Space};
use cadcraft_engine::render::{DisplayList, Kind, Options, overlays};
use cadcraft_engine::snap::{mode, osnap};
use cadcraft_geom::Vec2;
use lamb_cadcraft::{DrawingProject, lamb_overlay, project_to_drawing, register_providers, view_block_name};
use lamb_core::derivation::markers::{Marker, MarkerShape, compute_cut_lines_for_view, compute_markers_for_view, compute_plan_cut_lines_for_view};
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AnyView, Id, Project};
use lamb_core::persistence::files_to_project;

type R = Result<(), Box<dyn std::error::Error>>;

fn load(name: &str) -> Result<Project, Box<dyn std::error::Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb").join(format!("{name}.lamb.json"));
    let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(files_to_project(&files).map_err(|e| format!("{e:?}"))?)
}

fn drawing(name: &str) -> Result<Drawing, Box<dyn std::error::Error>> {
    Ok(project_to_drawing(&load(name)?)?.0)
}

fn block(id: Id) -> Space {
    Space::Block(view_block_name(id))
}

/// The marker outlines of a list: (handle, distinct vertices) per closed polyline.
fn outlines(l: &DisplayList) -> Vec<(Handle, Vec<Vec2>)> {
    l.prims
        .iter()
        .filter(|p| p.kind == Kind::Polyline && p.len > 2)
        .map(|p| {
            let mut v = l.verts[p.start as usize..(p.start + p.len) as usize].to_vec();
            if v.len() > 1 && v.first() == v.last() {
                v.pop();
            }
            (p.handle, v)
        })
        .collect()
}

fn mean(v: &[Vec2]) -> Vec2 {
    let n = v.len() as f64;
    Vec2::new(v.iter().map(|p| p.x).sum::<f64>() / n, v.iter().map(|p| p.y).sum::<f64>() / n)
}

fn extent(v: &[Vec2]) -> (f64, f64) {
    let (mut x0, mut x1, mut y0, mut y1) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
    for p in v {
        (x0, x1, y0, y1) = (x0.min(p.x), x1.max(p.x), y0.min(p.y), y1.max(p.y));
    }
    (x1 - x0, y1 - y0)
}

/// Every marker of the view has one outline centred on it; `Tris` exactly for the filled ones.
fn check_markers(name: &str, l: &DisplayList, markers: &[Marker]) {
    let rings = outlines(l);
    for m in markers {
        let at = Vec2::new(m.position.x, m.position.y);
        let n = rings.iter().filter(|(h, v)| *h == Handle(m.source_entity_id.0) && mean(v).dist(at) <= TOLERANCE.coincidence_mm).count();
        assert!(n >= 1, "{name}: no outline centred on marker {}", m.id);
    }
    let tris = l.prims.iter().filter(|p| p.kind == Kind::Tris).count();
    assert_eq!(tris, markers.iter().filter(|m| m.filled).count(), "{name}: one fan per filled marker");
    for p in &l.prims {
        assert!(!(p.kind == Kind::Tris && p.len % 3 != 0), "{name}: whole triangles");
    }
}

#[test]
fn markers_and_cut_lines_overlay_and_snap() -> R {
    // u-wall-two-sections: plan with two section cut lines, each section with the plan's cut line.
    let d = drawing("u-wall-two-sections")?;
    let dp = DrawingProject::new(&d)?;
    let opts = Options::default();
    let (mut plans, mut verticals) = (0, 0);
    for v in &dp.document().views {
        let id = v.id();
        let l = lamb_overlay(&d, &block(id), &opts);
        let markers = compute_markers_for_view(&dp, id)?;
        check_markers("u-wall", &l, &markers);
        let lines = l.prims.iter().filter(|p| p.kind == Kind::Polyline && p.len == 2).count();
        let infinite = l.prims.iter().filter(|p| p.kind == Kind::Infinite { ray: false }).count();
        match v {
            AnyView::Plan(_) => {
                plans += 1;
                let cuts = compute_cut_lines_for_view(&dp, id)?;
                assert_eq!(lines, cuts.len(), "plan: one polyline per section cut line");
                assert!(!cuts.is_empty(), "plan has section cut lines");
                assert_eq!(infinite, 0);
            }
            AnyView::Vertical(_) => {
                verticals += 1;
                let cuts = compute_plan_cut_lines_for_view(&dp, id);
                assert_eq!(infinite, cuts.len(), "section: one infinite line per plan cut");
                assert!(!cuts.is_empty(), "section shows the plan's cut line");
                for c in &cuts {
                    assert!(l.verts.iter().any(|p| (p.y - c.local_y).abs() <= TOLERANCE.coincidence_mm && p.x == 0.0));
                }
            }
            AnyView::Detail(_) => {}
        }
    }
    assert!(plans >= 1 && verticals >= 2, "fixture shape: {plans} plans, {verticals} verticals");
    assert!(lamb_overlay(&d, &Space::Model, &opts).prims.is_empty(), "model space: empty");
    assert!(lamb_overlay(&d, &Space::Block("LAMB_VIEW_999999".into()), &opts).prims.is_empty(), "unknown view: empty");

    // Marker size follows the visible height.
    let plan = dp.document().views.iter().find(|v| matches!(v, AnyView::Plan(_))).map(AnyView::id).ok_or("no plan")?;
    let pm = compute_markers_for_view(&dp, plan)?;
    if let Some(m) = pm.iter().find(|m| m.shape == MarkerShape::Circle) {
        let big = lamb_overlay(&d, &block(plan), &Options { view_height: 15_000.0, ..Options::default() });
        let ring =
            outlines(&big).into_iter().find(|(h, v)| *h == Handle(m.source_entity_id.0) && mean(v).dist(Vec2::new(m.position.x, m.position.y)) < 1.0);
        let (w, _) = extent(&ring.ok_or("ring")?.1);
        assert!((w - 200.0).abs() < 1.0, "radius view_height/150 = 100: width {w}");
    }

    // Ellipse markers: X extent / Y extent = 2 or 5.
    let mut ellipses = 0;
    for name in ["t76-rotated-section", "u-wall-two-sections", "dimensioned-u-wall", "filleted-corners"] {
        let d = drawing(name)?;
        let dp = DrawingProject::new(&d)?;
        for v in &dp.document().views {
            let markers = compute_markers_for_view(&dp, v.id())?;
            let l = lamb_overlay(&d, &block(v.id()), &opts);
            check_markers(name, &l, &markers);
            let rings = outlines(&l);
            for m in markers.iter().filter(|m| m.shape != MarkerShape::Circle) {
                let want = if m.shape == MarkerShape::Ellipse2 { 2.0 } else { 5.0 };
                let at = Vec2::new(m.position.x, m.position.y);
                let (_, ring) = rings
                    .iter()
                    .find(|(h, r)| *h == Handle(m.source_entity_id.0) && mean(r).dist(at) <= TOLERANCE.coincidence_mm)
                    .ok_or("ellipse ring")?;
                let (w, h) = extent(ring);
                assert!(((w / h) / want - 1.0).abs() < 0.01, "{name}: {:?} extent ratio {}", m.shape, w / h);
                ellipses += 1;
            }
        }
    }
    assert!(ellipses > 0, "no fixture has an ellipse marker");

    // Registered providers: render::overlays and osnap see them; registering twice is a no-op.
    register_providers()?;
    register_providers()?;
    let pl = overlays(&d, &block(plan), &opts);
    assert_eq!(pl.len(), 1, "one LAMB overlay list");
    assert_eq!(pl[0].prims, lamb_overlay(&d, &block(plan), &opts).prims);
    assert!(overlays(&d, &Space::Model, &opts).is_empty());

    let m = pm.first().ok_or("plan has no markers")?;
    let cursor = Vec2::new(m.position.x + 1.0, m.position.y);
    let hit = osnap(&d, &block(plan), cursor, 50.0, mode::CEN, None, false).ok_or("no snap")?;
    assert!(hit.point.dist(Vec2::new(m.position.x, m.position.y)) <= TOLERANCE.coincidence_mm, "snap at the marker");
    assert_eq!((hit.mode, hit.name), (mode::PROVIDER, "LAMB marker"));
    assert!(osnap(&d, &Space::Model, cursor, 50.0, mode::CEN, None, false).is_none(), "model space: no LAMB snap");
    Ok(())
}

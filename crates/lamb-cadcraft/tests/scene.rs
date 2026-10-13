//! The derived 3D scene as renderer-agnostic meshes that pick back to source lines
//! (`cadcraft-integration.md` §13 phase 5, data half; LAMB T333).

use std::collections::BTreeMap;
use std::path::Path;

use cadcraft_doc::{Drawing, Handle};
use lamb_cadcraft::{DrawingProject, Scene, SceneMeshKind, pick, project_to_drawing, scene};
use lamb_core::derivation::solids::solid_triangles;
use lamb_core::derivation::synthesis::synthesize;
use lamb_core::geometry::Point3;
use lamb_core::model::Project;
use lamb_core::persistence::files_to_project;

type R = Result<(), Box<dyn std::error::Error>>;

const FIXTURES: [&str; 2] = ["u-wall-two-sections", "t76-rotated-section"];

fn load(name: &str) -> Result<Project, Box<dyn std::error::Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb").join(format!("{name}.lamb.json"));
    let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(files_to_project(&files).map_err(|e| format!("{e:?}"))?)
}

fn drawing(name: &str) -> Result<Drawing, Box<dyn std::error::Error>> {
    Ok(project_to_drawing(&load(name)?)?.0)
}

fn global(s: &Scene, p: [f32; 3]) -> [f64; 3] {
    [s.origin[0] + f64::from(p[0]), s.origin[1] + f64::from(p[1]), s.origin[2] + f64::from(p[2])]
}

fn near(a: [f64; 3], b: &Point3) -> bool {
    (a[0] - b.x).abs() <= 0.5 && (a[1] - b.y).abs() <= 0.5 && (a[2] - b.z).abs() <= 0.5
}

fn area2(t: &[Point3]) -> f64 {
    let (e1, e2) = ([t[1].x - t[0].x, t[1].y - t[0].y, t[1].z - t[0].z], [t[2].x - t[0].x, t[2].y - t[0].y, t[2].z - t[0].z]);
    let c = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
    (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt()
}

#[test]
fn meshes_match_synthesis_and_sources_resolve() -> R {
    for name in FIXTURES {
        let d = drawing(name)?;
        let s = scene(&d)?;
        let synth = synthesize(&DrawingProject::new(&d)?)?;
        let sources: Vec<Vec<Point3>> = synth.surfaces.iter().map(|x| x.triangles.clone()).chain(synth.solids.iter().map(solid_triangles)).collect();
        assert_eq!(s.meshes.len(), sources.len(), "{name}: one mesh per surface and solid");
        assert_eq!(s.paths.len(), synth.paths.len(), "{name}: one path per synthesized path");
        assert!(!s.meshes.is_empty(), "{name}: fixture derives something");

        let mut raw = 0;
        let mut kept = 0;
        for (mi, (mesh, src)) in s.meshes.iter().zip(&sources).enumerate() {
            assert_eq!(mesh.positions.len(), mesh.normals.len());
            assert_eq!(mesh.indices, (0..u32::try_from(mesh.positions.len())?).collect::<Vec<_>>());
            for h in &mesh.sources {
                assert!(d.entity(*h).is_some(), "{name}: mesh {mi} source {h:?} resolves");
            }
            // Walk source triangles in order; each either matches the next emitted one or had no area.
            let mut out = mesh.positions.as_chunks::<3>().0.iter();
            let mut next = out.next();
            for tri in src.as_chunks::<3>().0 {
                raw += 1;
                match next {
                    Some(t) if (0..3).all(|k| near(global(&s, t[k]), &tri[k])) => {
                        kept += 1;
                        next = out.next();
                    }
                    _ => assert!(area2(tri) < 1e-3, "{name}: mesh {mi} dropped a triangle with area"),
                }
            }
            assert!(next.is_none(), "{name}: mesh {mi} emitted a triangle with no source");
        }
        let emitted: usize = s.meshes.iter().map(|m| m.positions.len() / 3).sum();
        assert_eq!(emitted, kept);
        assert!(kept <= raw);
        for (p, sp) in s.paths.iter().zip(&synth.paths) {
            assert!(d.entity(p.source).is_some());
            assert_eq!(p.points.len(), sp.points.len());
            assert!(p.points.iter().zip(&sp.points).all(|(a, b)| near(global(&s, *a), b)));
        }
    }
    Ok(())
}

/// The fixtures' walls are open polylines, so they derive vertical ribbons only — edge-on to a
/// vertical ray. A ray across the wall at mid-height is how a 3D camera meets them.
#[test]
fn ray_across_a_ribbon_picks_back_to_its_plan_line() -> R {
    for name in FIXTURES {
        let d = drawing(name)?;
        let s = scene(&d)?;
        for (mi, mesh) in s.meshes.iter().enumerate() {
            let (Some(n), Some(tri)) = (mesh.normals.first(), mesh.positions.as_chunks::<3>().0.first()) else { continue };
            let g: Vec<[f64; 3]> = tri.iter().map(|p| global(&s, *p)).collect();
            let c = [0, 1, 2].map(|k| (g[0][k] + g[1][k] + g[2][k]) / 3.0);
            let dir = [-f64::from(n[0]), -f64::from(n[1]), -f64::from(n[2])];
            let start = [c[0] - 50_000.0 * dir[0], c[1] - 50_000.0 * dir[1], c[2] - 50_000.0 * dir[2]];
            let hit = pick(&s, start, dir).ok_or(format!("{name}: ray at mesh {mi} missed"))?;
            assert!(hit.distance <= 50_000.0 + 0.5, "{name}: mesh {mi} hit is not that mesh or one in front of it");
            if hit.mesh == mi {
                assert!((0..3).all(|k| (hit.point[k] - c[k]).abs() < 0.5));
            }
            let picked = &s.meshes[hit.mesh];
            assert_eq!(hit.sources, picked.sources);
            assert_eq!(hit.segment_index, picked.segment_index);
            assert!(d.entity(hit.sources[0]).is_some());
        }
        // Beside everything, and pointing away: nothing.
        let max_x = s.meshes.iter().flat_map(|m| m.positions.iter()).map(|p| global(&s, *p)[0]).fold(f64::MIN, f64::max);
        assert!(pick(&s, [max_x + 10_000.0, 0.0, 100_000.0], [0.0, 0.0, -1.0]).is_none());
        assert!(pick(&s, [max_x + 10_000.0, 2_500.0, 1_000.0], [1.0, 0.0, 0.0]).is_none());
    }
    Ok(())
}

/// A closed room added to the u-wall plan closes into a capped prism; a ray straight down onto its
/// roof names the room's plan line.
#[test]
fn downward_ray_onto_a_solid_picks_its_plan_line() -> R {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb/u-wall-two-sections.lamb.json");
    let mut files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let view = files.get_mut("views/p--plan-1.json").ok_or("no plan view file")?;
    let mut v: serde_json::Value = serde_json::from_str(view)?;
    let pt = |x: i32, y: i32| serde_json::json!({ "bulge": 0, "to": { "x": x, "y": y } });
    let room = serde_json::json!({
        "geometry": { "kind": "polyline", "geometry": { "closed": true, "start": { "x": 20000, "y": 0 },
            "segments": [pt(24000, 0), pt(24000, 3000), pt(20000, 3000), pt(20000, 0)] } },
        "id": "room", "layerId": "layer-wall-int", "role": "cut", "viewId": "plan-1"
    });
    v.get_mut("entities").and_then(|e| e.as_array_mut()).ok_or("no entities")?.push(room);
    *view = serde_json::to_string(&v)?;
    let project = files_to_project(&files).map_err(|e| format!("{e:?}"))?;
    let (d, map, _) = project_to_drawing(&project)?;
    let s = scene(&d)?;

    let solid = s.meshes.iter().position(|m| m.kind == SceneMeshKind::Solid).ok_or("the room derived no solid")?;
    let mesh = &s.meshes[solid];
    assert_eq!(mesh.segment_index, None);
    // The room's own plan line, not the U wall the ribbons come from.
    assert_eq!(mesh.sources.len(), 1);
    assert!(s.meshes.iter().filter(|m| m.kind == SceneMeshKind::Surface).all(|m| m.sources != mesh.sources));
    assert_eq!(map.0.values().filter(|h| Handle(h.0) == mesh.sources[0]).count(), 1, "source is an imported entity");
    assert!(mesh.normals.iter().any(|n| n[2] > 0.9), "the prism has a roof");

    let hit = pick(&s, [22_000.0, 1_500.0, 100_000.0], [0.0, 0.0, -1.0]).ok_or("ray onto the roof missed")?;
    assert_eq!(hit.mesh, solid);
    assert_eq!(hit.sources, mesh.sources);
    assert!(d.entity(hit.sources[0]).is_some());
    let roof = mesh.positions.iter().map(|p| global(&s, *p)[2]).fold(f64::MIN, f64::max);
    assert!((hit.point[2] - roof).abs() < 0.5 && (hit.distance - (100_000.0 - roof)).abs() < 0.5);
    // Beside the room (inside the U's open side, below nothing): nothing.
    assert!(pick(&s, [30_000.0, 1_500.0, 100_000.0], [0.0, 0.0, -1.0]).is_none());
    Ok(())
}

#[test]
fn empty_drawing_gives_empty_scene() -> R {
    let s = scene(&Drawing::new_metric())?;
    assert_eq!(s.origin, [0.0; 3]);
    assert!(s.meshes.is_empty() && s.paths.is_empty());
    assert!(pick(&s, [0.0, 0.0, 100.0], [0.0, 0.0, -1.0]).is_none());
    Ok(())
}

//! LAMB (fork, T341): the 3D pane's scene source. `lamb_cadcraft::scene` derives LAMB's 3D model
//! from the drawing; this converts it into the pane's [`Scene3`], coloured as the TS app colours it
//! (`Viewport3D.tsx` `materialForForm` and `PROVENANCE_MATERIAL`): a solid is opaque in its layer's
//! colour, a surface is ghosted grey, a conflicted mesh gets red edges.

use cadcraft_engine::doc::color::{Color, Rgb};
use cadcraft_engine::doc::{Drawing, Handle};
use cadcraft_ui_egui::scene3d::{Mesh3, Path3, Scene3, SceneSource, register_scene_source};
use lamb_cadcraft::{Scene, SceneMeshKind};
use lamb_core::derivation::resolve::ProvenanceKind;

/// A solid whose layer is missing (TS `materialForForm`'s default fill).
const SOLID_FALLBACK: u32 = 0xd8d8d8;
/// A surface: ghosted grey.
const SURFACE: (u32, f32) = (0x888888, 0.28);
/// A conflicted mesh's edges.
const CONFLICT_EDGES: u32 = 0xe2543a;

/// Register LAMB's derived model as the 3D pane's scene.
pub fn register() -> Result<(), &'static str> {
    register_scene_source(SceneSource { build, pick })
}

fn build(d: &Drawing) -> Option<Scene3> {
    let s = lamb_cadcraft::scene(d).ok()?;
    // The pane treats `None` as "nothing to draw": a drawing with no LAMB model derives nothing.
    if s.meshes.is_empty() && s.paths.is_empty() {
        return None;
    }
    Some(to_scene3(&s, d))
}

fn pick(d: &Drawing, o: [f64; 3], dir: [f64; 3]) -> Vec<Handle> {
    lamb_cadcraft::scene(d).ok().and_then(|s| lamb_cadcraft::pick(&s, o, dir)).map(|p| p.sources).unwrap_or_default()
}

fn rgba(rgb: u32, alpha: f32) -> [u8; 4] {
    let Rgb(r, g, b) = Rgb::from_u32(rgb);
    [r, g, b, (alpha.clamp(0.0, 1.0) * 255.0).round() as u8]
}

/// A layer's colour as `cadcraft_render` resolves a ByLayer entity on it.
fn layer_rgb(d: &Drawing, layer: &str) -> Option<u32> {
    d.layer(layer).map(|l| Color::ByLayer.resolve(l.color, Color::Index(7)).to_u32())
}

/// The TS `PROVENANCE_MATERIAL` table: colour and opacity per provenance.
fn provenance_material(p: ProvenanceKind) -> (u32, f32) {
    match p {
        ProvenanceKind::Authored => (0xd8d8d8, 1.0),
        ProvenanceKind::Proposed => (0xd8d8d8, 0.55),
        ProvenanceKind::Defaulted => (0x888888, 0.28),
        ProvenanceKind::Ambiguous => (0xb8860b, 0.4),
        ProvenanceKind::Conflicted => (0xe2543a, 0.65),
        ProvenanceKind::NotThreeD => (0x888888, 0.1),
    }
}

/// `lamb_cadcraft::scene`'s output as the pane's scene, with the TS app's materials.
pub fn to_scene3(s: &Scene, d: &Drawing) -> Scene3 {
    let meshes = s
        .meshes
        .iter()
        .map(|m| {
            let rgba = match m.kind {
                SceneMeshKind::Solid | SceneMeshKind::Layer => rgba(layer_rgb(d, &m.layer).unwrap_or(SOLID_FALLBACK), 1.0),
                SceneMeshKind::Surface => rgba(SURFACE.0, SURFACE.1),
            };
            let edges_rgba = (m.provenance == ProvenanceKind::Conflicted).then(|| rgba_opaque(CONFLICT_EDGES));
            Mesh3 { positions: m.positions.clone(), indices: m.indices.clone(), rgba, edges_rgba, sources: m.sources.clone() }
        })
        .collect();
    let paths = s
        .paths
        .iter()
        .map(|p| {
            let (c, a) = provenance_material(p.provenance);
            Path3 { points: p.points.clone(), rgba: rgba(c, a.max(0.6)) }
        })
        .collect();
    Scene3 { origin: s.origin, meshes, paths }
}

fn rgba_opaque(rgb: u32) -> [u8; 4] {
    rgba(rgb, 1.0)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use cadcraft_engine::doc::Drawing;
    use cadcraft_engine::doc::color::Color;
    use lamb_cadcraft::{SceneMeshKind, project_to_drawing, scene};
    use lamb_core::persistence::files_to_project;

    use super::{build, pick, to_scene3};

    type R = Result<(), Box<dyn std::error::Error>>;

    fn files() -> Result<BTreeMap<String, String>, Box<dyn std::error::Error>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/lamb-core/tests/lamb/u-wall-two-sections.lamb.json");
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }

    fn drawing(files: &BTreeMap<String, String>) -> Result<Drawing, Box<dyn std::error::Error>> {
        Ok(project_to_drawing(&files_to_project(files).map_err(|e| format!("{e:?}"))?)?.0)
    }

    /// The u-wall plan plus a closed room, which derives a capped prism (as
    /// `crates/lamb-cadcraft/tests/scene.rs` builds it).
    fn with_room() -> Result<Drawing, Box<dyn std::error::Error>> {
        let mut files = files()?;
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
        drawing(&files)
    }

    #[test]
    fn surfaces_ghost_grey_and_solids_take_their_layer_colour() -> R {
        let mut d = with_room()?;
        let s = scene(&d)?;
        let solid = s.meshes.iter().position(|m| m.kind == SceneMeshKind::Solid).ok_or("no solid")?;
        let layer = s.meshes[solid].layer.clone();
        // A distinctive true colour, so the assertion cannot pass on the fallback.
        d.layer_mut(&layer).ok_or("solid's layer is missing")?.color = Color::True(cadcraft_engine::doc::color::Rgb(10, 200, 30));

        let s3 = build(&d).ok_or("a LAMB drawing built no scene")?;
        assert_eq!(s3.origin, s.origin);
        assert_eq!(s3.meshes.len(), s.meshes.len());
        assert_eq!(s3.paths.len(), s.paths.len());
        let mut surfaces = 0;
        for (m, m3) in s.meshes.iter().zip(&s3.meshes) {
            assert_eq!(m3.sources, m.sources);
            assert_eq!(m3.positions, m.positions);
            assert_eq!(m3.indices, m.indices);
            if m.kind == SceneMeshKind::Surface {
                surfaces += 1;
                assert_eq!(m3.rgba, [0x88, 0x88, 0x88, 71]);
            }
        }
        assert!(surfaces > 0, "the U wall derives ribbons");
        assert_eq!(s3.meshes[solid].rgba, [10, 200, 30, 255]);
        assert_eq!(to_scene3(&s, &d), s3);
        Ok(())
    }

    #[test]
    fn a_ray_along_a_ribbon_normal_picks_its_source() -> R {
        let d = drawing(&files()?)?;
        let s = scene(&d)?;
        let mut direct = 0;
        for (mi, m) in s.meshes.iter().enumerate().filter(|(_, m)| m.kind == SceneMeshKind::Surface) {
            let n = m.normals[0];
            let tri = &m.positions[..3];
            let c = [0, 1, 2].map(|k| s.origin[k] + tri.iter().map(|p| f64::from(p[k])).sum::<f64>() / 3.0);
            let dir = [-f64::from(n[0]), -f64::from(n[1]), -f64::from(n[2])];
            let start = [c[0] - 50_000.0 * dir[0], c[1] - 50_000.0 * dir[1], c[2] - 50_000.0 * dir[2]];
            let hit = pick(&d, start, dir);
            let expect = lamb_cadcraft::pick(&s, start, dir).ok_or("ray missed")?;
            assert_eq!(hit, expect.sources);
            assert!(!hit.is_empty() && d.entity(hit[0]).is_some());
            if expect.mesh == mi {
                assert_eq!(hit, m.sources);
                direct += 1;
            }
        }
        assert!(direct > 0, "no ribbon was hit along its own normal");
        Ok(())
    }

    #[test]
    fn a_plain_drawing_builds_nothing() {
        let d = Drawing::new_metric();
        assert!(build(&d).is_none());
        assert!(pick(&d, [0.0, 0.0, 100.0], [0.0, 0.0, -1.0]).is_empty());
    }
}

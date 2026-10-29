//! LAMB (fork, T341): the 3D pane's scene source. `lamb_cadcraft::scene` derives LAMB's 3D model
//! from the drawing; this converts it into the pane's [`Scene3`], coloured as the TS app colours it
//! (`Viewport3D.tsx` `materialForForm` and `PROVENANCE_MATERIAL`): a solid is opaque in its layer's
//! colour, a surface is ghosted grey, a conflicted mesh gets red edges. With an assembly plan
//! (LAMB T493, `ghosting-and-visualization.md` "Construction states"), massing still standing in
//! for an undetailed wall is ghosted in its own colour and a detail-conflicted mesh gets red edges.
//! While GHOSTHIDE is in force (LAMB T498) the pane draws and picks `without_ghosts`.

use cadcraft_engine::doc::color::{Color, Rgb};
use cadcraft_engine::doc::{Drawing, Handle};
use cadcraft_ui_egui::scene3d::{Mesh3, Path3, Scene3, SceneSource, register_scene_source};
use lamb_cadcraft::{ConstructionState, Scene, SceneMeshKind};
use lamb_core::derivation::resolve::ProvenanceKind;

/// A solid whose layer is missing (TS `materialForForm`'s default fill).
const SOLID_FALLBACK: u32 = 0xd8d8d8;
/// A surface: ghosted grey.
const SURFACE: (u32, f32) = (0x888888, 0.28);
/// Undetailed massing's alpha (the `Defaulted` ghost).
const MASSING_ALPHA: f32 = 0.28;
/// A conflicted mesh's edges.
const CONFLICT_EDGES: u32 = 0xe2543a;

/// Register LAMB's derived model as the 3D pane's scene.
pub fn register() -> Result<(), &'static str> {
    register_scene_source(SceneSource { build, pick, stamp, highlight: lamb_cadcraft::highlight_3d, hover: lamb_cadcraft::hover_3d })
}

/// GHOSTHIDE / GHOSTSHOW change what `build` returns without editing the drawing.
fn stamp() -> u64 {
    u64::from(lamb_cadcraft::ghosts_hidden())
}

/// `s`, without its ghosts when `hidden`.
fn shown(s: Scene, hidden: bool) -> Scene {
    if hidden { lamb_cadcraft::without_ghosts(&s) } else { s }
}

fn build(d: &Drawing) -> Option<Scene3> {
    let s = shown(lamb_cadcraft::scene(d).ok()?, lamb_cadcraft::ghosts_hidden());
    built(&s, d)
}

/// The pane treats `None` as "nothing to draw": a drawing with no LAMB model derives nothing,
/// and neither does one whose every mesh is a hidden ghost.
fn built(s: &Scene, d: &Drawing) -> Option<Scene3> {
    if s.meshes.is_empty() && s.paths.is_empty() {
        return None;
    }
    Some(to_scene3(s, d))
}

/// A hidden ghost cannot be picked through.
fn pick(d: &Drawing, o: [f64; 3], dir: [f64; 3]) -> Vec<Handle> {
    let Ok(s) = lamb_cadcraft::scene(d) else { return Vec::new() };
    lamb_cadcraft::pick(&shown(s, lamb_cadcraft::ghosts_hidden()), o, dir).map(|p| p.sources).unwrap_or_default()
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
            let (rgb, alpha) = match m.kind {
                SceneMeshKind::Solid | SceneMeshKind::Layer => (layer_rgb(d, &m.layer).unwrap_or(SOLID_FALLBACK), 1.0),
                SceneMeshKind::Surface => SURFACE,
            };
            let alpha = if m.construction == Some(ConstructionState::Massing) { MASSING_ALPHA } else { alpha };
            let rgba = rgba(rgb, alpha);
            let conflicted = m.provenance == ProvenanceKind::Conflicted || m.construction == Some(ConstructionState::DetailConflicted);
            let edges_rgba = conflicted.then(|| rgba_opaque(CONFLICT_EDGES));
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
    use lamb_cadcraft::{ConstructionState, SceneMeshKind, project_to_drawing, scene};
    use lamb_core::persistence::files_to_project;

    use super::{MASSING_ALPHA, build, built, pick, rgba, shown, to_scene3};

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

    /// `with_room`'s scene converted with every mesh in `state`, beside the stateless conversion,
    /// its solid's layer a distinctive true colour.
    fn with_state(state: Option<ConstructionState>) -> Result<Vec<(SceneMeshKind, [u8; 4], Option<[u8; 4]>, [u8; 4])>, Box<dyn std::error::Error>> {
        let mut d = with_room()?;
        let mut s = scene(&d)?;
        assert!(s.meshes.iter().all(|m| m.construction.is_none()), "no assembly block, no state");
        let solid = s.meshes.iter().find(|m| m.kind == SceneMeshKind::Solid).ok_or("no solid")?.layer.clone();
        d.layer_mut(&solid).ok_or("solid's layer is missing")?.color = Color::True(cadcraft_engine::doc::color::Rgb(10, 200, 30));
        let plain = to_scene3(&s, &d);
        for m in &mut s.meshes {
            m.construction = state;
        }
        let stated = to_scene3(&s, &d);
        Ok(s.meshes.iter().zip(stated.meshes.iter().zip(&plain.meshes)).map(|(m, (a, b))| (m.kind, a.rgba, a.edges_rgba, b.rgba)).collect())
    }

    #[test]
    fn massing_keeps_its_colour_at_the_ghost_alpha() -> R {
        let got = with_state(Some(ConstructionState::Massing))?;
        assert!(got.iter().any(|g| g.0 == SceneMeshKind::Solid));
        for (_, rgba, edges, plain) in got {
            assert_eq!(rgba, [plain[0], plain[1], plain[2], 71]);
            assert_eq!(edges, None);
        }
        Ok(())
    }

    #[test]
    fn detail_conflicted_keeps_its_colour_and_gets_conflict_edges() -> R {
        for (_, rgba, edges, plain) in with_state(Some(ConstructionState::DetailConflicted))? {
            assert_eq!(rgba, plain);
            assert_eq!(edges, Some([0xe2, 0x54, 0x3a, 255]));
        }
        Ok(())
    }

    #[test]
    fn resolved_and_stateless_meshes_draw_as_before() -> R {
        for state in [Some(ConstructionState::Resolved), None] {
            let got = with_state(state)?;
            assert!(got.iter().any(|g| g.0 == SceneMeshKind::Solid && g.1 == [10, 200, 30, 255]));
            for (_, rgba, edges, plain) in got {
                assert_eq!(rgba, plain);
                assert_eq!(edges, None);
            }
        }
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

    /// The room's scene with every mesh massing (as T493 states an undetailed room): shown, the
    /// solid draws at the ghost alpha and a ray down through the room picks it; hidden, nothing is
    /// left to draw or pick. The flag is passed to `shown` directly, so this test never races the
    /// others through the process-wide static, which stays off: `build`/`pick` draw everything.
    #[test]
    fn hidden_ghosts_neither_draw_nor_pick() -> R {
        let d = with_room()?;
        let s = scene(&d)?;
        let mut massing = s.clone();
        massing.meshes.iter_mut().for_each(|m| m.construction = Some(ConstructionState::Massing));
        let solid = massing.meshes.iter().find(|m| m.kind == SceneMeshKind::Solid).ok_or("no solid")?.sources.clone();
        let ghost_alpha = rgba(0, MASSING_ALPHA)[3];
        // A ray straight down through the room's middle (20000..24000 × 0..3000).
        let (o, dir) = ([22_000.0, 1_500.0, 100_000.0], [0.0, 0.0, -1.0]);

        let on = shown(massing.clone(), false);
        assert!(built(&on, &d).ok_or("nothing built")?.meshes.iter().any(|m| m.rgba[3] == ghost_alpha));
        assert_eq!(lamb_cadcraft::pick(&on, o, dir).map(|p| p.sources), Some(solid));

        let off = shown(massing, true);
        assert!(built(&off, &d).is_none_or(|s3| s3.meshes.iter().all(|m| m.rgba[3] != ghost_alpha)));
        assert!(lamb_cadcraft::pick(&off, o, dir).is_none(), "a hidden ghost cannot be picked through");

        assert_eq!(build(&d), built(&s, &d), "the flag is off by default");
        assert_eq!(pick(&d, o, dir), lamb_cadcraft::pick(&s, o, dir).map(|p| p.sources).unwrap_or_default());
        Ok(())
    }
}

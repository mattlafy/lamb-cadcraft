//! The derived 3D scene as renderer-agnostic data (`cadcraft-integration.md` §3.1, §13 phase 5):
//! `synthesize` over a [`Drawing`] converted into flat-shaded `f32` meshes and polylines, each
//! carrying the CADCraft [`Handle`]s of the lines it came from, its provenance and its layer, plus
//! a ray [`pick`] that names those lines. No pixels: the 3D pane that draws this is a GUI concern.
//!
//! Positions are `f32` offsets from [`Scene::origin`] (the scene's minimum corner), because
//! project-global millimetres can exceed what `f32` holds to half a millimetre. Meshes are not
//! welded: every triangle owns its three corners, `indices` is `0..n`, and each corner carries the
//! triangle's face normal — the shape a renderer wants for flat shading.
//!
//! In a placed plan with an assembly block, each laid straight run is layered (LAMB T487,
//! [`crate::layered`]): one [`SceneMeshKind::Layer`] prism per band gap replaces the massing
//! surfaces of the run's face segments, and a solid whose every plan segment is replaced is dropped.
//! In a placed section with an assembly block, each laid floor or roof run is layered the same way
//! (LAMB T494), extruded along the section's gaze, replacing its entity's section solid.
//!
//! Every mesh of such a view carries its [`ConstructionState`] (LAMB T493,
//! `ghosting-and-visualization.md` "Construction states"); a mesh of any other view carries none.

use std::fmt;

use cadcraft_doc::{Drawing, Handle};
use lamb_core::derivation::resolve::ProvenanceKind;
use lamb_core::derivation::solids::solid_triangles;
use lamb_core::derivation::synthesis::synthesize;
use lamb_core::geometry::Point3;
use lamb_core::model::Id;

use crate::project::DrawingProject;
use crate::schema::SchemaError;

/// What a mesh was synthesized as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneMeshKind {
    /// A ribbon surface swept from one segment of one entity.
    Surface,
    /// A closed solid from a plan entity (and its linked vertical partners).
    Solid,
    /// One layer of a detail-resolved wall run: a thin prism between two band boundaries.
    Layer,
}

/// How far a wall's construction has got, for a mesh of a view with an assembly block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConstructionState {
    /// Laid as layers.
    Resolved,
    /// Massing still standing in for a run no detail was laid along, or a partly layered solid.
    Massing,
    /// Contradicted: a run skipped for conflicting details, or laid with a width conflict.
    DetailConflicted,
}

/// One flat-shaded triangle mesh.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneMesh {
    pub kind: SceneMeshKind,
    /// The source lines: a surface's one entity; a solid's entities in order, plan entity first; a
    /// layer's run's two face entities.
    pub sources: Vec<Handle>,
    /// The segment a surface was swept from; `None` for a solid or a layer.
    pub segment_index: Option<usize>,
    pub provenance: ProvenanceKind,
    pub layer: String,
    /// `None` outside a view with an assembly block.
    pub construction: Option<ConstructionState>,
    /// Offsets from [`Scene::origin`], three per triangle, unshared.
    pub positions: Vec<[f32; 3]>,
    /// The triangle's unit face normal, repeated on each of its corners.
    pub normals: Vec<[f32; 3]>,
    /// `0..positions.len()`.
    pub indices: Vec<u32>,
}

/// A derived 3D polyline (not pickable).
#[derive(Debug, Clone, PartialEq)]
pub struct ScenePath {
    pub source: Handle,
    pub segment_index: usize,
    pub provenance: ProvenanceKind,
    /// Offsets from [`Scene::origin`].
    pub points: Vec<[f32; 3]>,
}

/// Everything a 3D pane draws for one drawing.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scene {
    /// Project-global mm that every position is offset from; `[0.0; 3]` when empty.
    pub origin: [f64; 3],
    pub meshes: Vec<SceneMesh>,
    pub paths: Vec<ScenePath>,
}

/// The nearest mesh a ray hits.
#[derive(Debug, Clone, PartialEq)]
pub struct ScenePick {
    /// Index into [`Scene::meshes`].
    pub mesh: usize,
    pub sources: Vec<Handle>,
    pub segment_index: Option<usize>,
    /// Ray parameter of the hit, in units of `dir`'s length (mm when `dir` is a unit vector).
    pub distance: f64,
    /// The hit point, project-global mm.
    pub point: [f64; 3],
}

/// Why a scene could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneError {
    /// The drawing's LAMB document could not be read.
    Schema(SchemaError),
    /// lamb-core's synthesis failed.
    Synthesis(String),
}

impl fmt::Display for SceneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SceneError::Schema(e) => e.fmt(f),
            SceneError::Synthesis(e) => write!(f, "3D synthesis failed: {e}"),
        }
    }
}

impl std::error::Error for SceneError {}

impl From<SchemaError> for SceneError {
    fn from(e: SchemaError) -> Self {
        SceneError::Schema(e)
    }
}

fn handle(id: Id) -> Handle {
    Handle(id.0)
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn len(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn arr(p: &Point3) -> [f64; 3] {
    [p.x, p.y, p.z]
}

fn offset(p: [f64; 3], origin: [f64; 3]) -> [f32; 3] {
    // Narrowing is the point: offsets from the scene's own corner fit f32 to well under 0.5 mm.
    #[allow(clippy::cast_possible_truncation)]
    let v = [(p[0] - origin[0]) as f32, (p[1] - origin[1]) as f32, (p[2] - origin[2]) as f32];
    v
}

/// The unit normal of a triangle, or `None` when it has no area.
fn face_normal(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> Option<[f64; 3]> {
    let (e1, e2) = (sub(b, a), sub(c, a));
    let n = cross(e1, e2);
    let l = len(n);
    let scale = dot(e1, e1).max(dot(e2, e2));
    if !l.is_finite() || l <= 1e-12 * scale || l == 0.0 {
        return None;
    }
    Some([n[0] / l, n[1] / l, n[2] / l])
}

struct Pending {
    kind: SceneMeshKind,
    sources: Vec<Handle>,
    segment_index: Option<usize>,
    provenance: ProvenanceKind,
    layer: String,
    construction: Option<ConstructionState>,
    triangles: Vec<Point3>,
}

/// A surviving solid's state over its plan entity `e`'s segments that have one: any conflicted →
/// conflicted; else any massing, or some segment layered (partly layered) → massing; else none.
fn solid_state(layers: &crate::layered::Layered, e: Id) -> Option<ConstructionState> {
    let worst = layers.states.range((e, 0)..=(e, usize::MAX)).map(|(_, &st)| st).max()?;
    if worst > ConstructionState::Resolved {
        return Some(worst);
    }
    layers.replaced.range((e, 0)..=(e, usize::MAX)).next().map(|_| ConstructionState::Massing)
}

/// The derived 3D scene of `d`.
pub fn scene(d: &Drawing) -> Result<Scene, SceneError> {
    let project = DrawingProject::new(d)?;
    let result = synthesize(&project).map_err(SceneError::Synthesis)?;
    let layers = crate::layered::layered(&project, &result);

    let mut pending: Vec<Pending> = Vec::with_capacity(result.surfaces.len() + result.solids.len() + layers.strips.len());
    for s in result.surfaces.iter().filter(|s| !layers.replaced.contains(&(s.entity_id, s.segment_index))) {
        pending.push(Pending {
            kind: SceneMeshKind::Surface,
            sources: vec![handle(s.entity_id)],
            segment_index: Some(s.segment_index),
            provenance: s.provenance,
            layer: s.layer_id.clone(),
            // A surviving surface of a laid run was never layered, so it is still massing.
            construction: layers.states.get(&(s.entity_id, s.segment_index)).map(|&st| st.max(ConstructionState::Massing)),
            triangles: s.triangles.clone(),
        });
    }
    for (_, s) in result.solids.iter().enumerate().filter(|(i, _)| !layers.dropped_solids.contains(i)) {
        let construction = s.entity_ids.first().and_then(|&e| solid_state(&layers, e));
        pending.push(Pending {
            kind: SceneMeshKind::Solid,
            sources: s.entity_ids.iter().copied().map(handle).collect(),
            segment_index: None,
            provenance: s.provenance,
            layer: s.layer_id.clone(),
            construction,
            triangles: solid_triangles(s),
        });
    }
    for l in layers.strips {
        pending.push(Pending {
            kind: SceneMeshKind::Layer,
            sources: l.sources.iter().copied().map(handle).collect(),
            segment_index: None,
            provenance: l.provenance,
            layer: l.layer,
            construction: Some(l.state),
            triangles: l.triangles,
        });
    }

    let mut min = [f64::INFINITY; 3];
    let all_points = pending.iter().flat_map(|p| p.triangles.iter()).chain(result.paths.iter().flat_map(|p| p.points.iter()));
    for p in all_points {
        let a = arr(p);
        for k in 0..3 {
            if a[k].is_finite() {
                min[k] = min[k].min(a[k]);
            }
        }
    }
    let origin = if min.iter().all(|v| v.is_finite()) { min } else { [0.0; 3] };

    let mut meshes = Vec::with_capacity(pending.len());
    for p in pending {
        let mut positions = Vec::with_capacity(p.triangles.len());
        let mut normals = Vec::with_capacity(p.triangles.len());
        for [ta, tb, tc] in p.triangles.as_chunks::<3>().0 {
            let [a, b, c] = [arr(ta), arr(tb), arr(tc)];
            let Some(n) = face_normal(a, b, c) else { continue };
            #[allow(clippy::cast_possible_truncation)]
            let nf = [n[0] as f32, n[1] as f32, n[2] as f32];
            for v in [a, b, c] {
                positions.push(offset(v, origin));
                normals.push(nf);
            }
        }
        let indices = (0..positions.len()).filter_map(|i| u32::try_from(i).ok()).collect();
        meshes.push(SceneMesh {
            kind: p.kind,
            sources: p.sources,
            segment_index: p.segment_index,
            provenance: p.provenance,
            layer: p.layer,
            construction: p.construction,
            positions,
            normals,
            indices,
        });
    }

    let paths = result
        .paths
        .into_iter()
        .map(|p| ScenePath {
            source: handle(p.entity_id),
            segment_index: p.segment_index,
            provenance: p.provenance,
            points: p.points.iter().map(|q| offset(arr(q), origin)).collect(),
        })
        .collect();

    Ok(Scene { origin, meshes, paths })
}

/// Möller–Trumbore, double-sided: the ray parameter `t > 0` of the hit, if any.
fn ray_triangle(o: [f64; 3], dir: [f64; 3], a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> Option<f64> {
    let (e1, e2) = (sub(b, a), sub(c, a));
    let p = cross(dir, e2);
    let det = dot(e1, p);
    let scale = len(dir) * len(e1) * len(e2);
    if !det.is_finite() || det.abs() <= 1e-12 * scale {
        return None;
    }
    let inv = 1.0 / det;
    let s = sub(o, a);
    let u = dot(s, p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(s, e1);
    let v = dot(dir, q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = dot(e2, q) * inv;
    (t > 0.0 && t.is_finite()).then_some(t)
}

/// The nearest mesh the ray `origin + t * dir` (t > 0, project-global mm) hits; ties go to the
/// lower mesh index. Paths are not pickable.
pub fn pick(scene: &Scene, origin: [f64; 3], dir: [f64; 3]) -> Option<ScenePick> {
    let o = scene.origin;
    let global = |p: &[f32; 3]| [o[0] + f64::from(p[0]), o[1] + f64::from(p[1]), o[2] + f64::from(p[2])];
    let mut best: Option<(usize, f64)> = None;
    for (mi, mesh) in scene.meshes.iter().enumerate() {
        for &[ia, ib, ic] in mesh.indices.as_chunks::<3>().0 {
            let corner = |i: u32| mesh.positions.get(usize::try_from(i).ok()?).map(global);
            let (Some(a), Some(b), Some(c)) = (corner(ia), corner(ib), corner(ic)) else { continue };
            if let Some(t) = ray_triangle(origin, dir, a, b, c)
                && best.is_none_or(|(_, bt)| t < bt)
            {
                best = Some((mi, t));
            }
        }
    }
    let (mi, t) = best?;
    let mesh = scene.meshes.get(mi)?;
    Some(ScenePick {
        mesh: mi,
        sources: mesh.sources.clone(),
        segment_index: mesh.segment_index,
        distance: t,
        point: [origin[0] + t * dir[0], origin[1] + t * dir[1], origin[2] + t * dir[2]],
    })
}

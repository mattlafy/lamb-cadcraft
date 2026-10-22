//! Layered wall solids for the 3D scene (LAMB's `synthesis-rules.md` "Tier 2: detail-resolved
//! elements" and `ghosting-and-visualization.md` "partial"; LAMB T487). In a placed plan that has
//! an assembly block (the user ran ASSEMBLYPLAN), every straight run whose detail laid continuous
//! bands ([`assembly_lines`]) becomes one thin prism per gap between consecutive boundaries — the
//! run's reference face line and each laid band line — in the layer of the gap's boundary farther
//! from the reference face, extruded over the z range the run's massing already has (the
//! synthesized surfaces of its two face segments, else the solid of either face's entity). Those
//! face segments' surfaces are then replaced, and a solid is dropped once every segment of its plan
//! entity is. Arc runs, skipped runs and repeat copies keep their massing; a run with no massing to
//! take a z range from emits nothing. Unreadable layered data yields no layers, never an error.

use std::collections::BTreeSet;

use cadcraft_doc::Drawing;
use lamb_core::derivation::assembly::{AssemblyLine, Band, lay_bands};
use lamb_core::derivation::projection::plan_local_to_global_xy;
use lamb_core::derivation::read::{ProjectRead, ViewKind};
use lamb_core::derivation::resolve::ProvenanceKind;
use lamb_core::derivation::solids::{SolidForm, SynthesizedSolid, most_doubtful, prism_from_loop, solid_triangles};
use lamb_core::derivation::synthesis::SynthesisResult;
use lamb_core::derivation::walls::RunCurve;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::geometry::{Point2, Point3};
use lamb_core::model::Id;

use crate::assembly::assembly_lines;
use crate::detailtags::graph_and_sides;
use crate::project::DrawingProject;
use crate::schema::assembly_block_name;

/// One layer prism of one run.
pub(crate) struct LayerStrip {
    /// The run's two face entities, `faces[0]`'s first.
    pub sources: [Id; 2],
    pub provenance: ProvenanceKind,
    pub layer: String,
    /// Triangle list, project-global mm.
    pub triangles: Vec<Point3>,
}

/// What [`layered`] adds to and removes from a synthesis result.
#[derive(Default)]
pub(crate) struct Layered {
    pub strips: Vec<LayerStrip>,
    /// `(entity, segment)` of every face segment whose surfaces the strips replace.
    pub replaced: BTreeSet<(Id, usize)>,
    /// Indices into `SynthesisResult::solids` the strips replace.
    pub dropped_solids: BTreeSet<usize>,
}

fn sub(a: Point2, b: Point2) -> Point2 {
    Point2 { x: a.x - b.x, y: a.y - b.y }
}

fn dot(a: Point2, b: Point2) -> f64 {
    a.x * b.x + a.y * b.y
}

/// The layered replacement of `result`'s massing for every placed plan of `p` with an assembly block.
pub(crate) fn layered(p: &DrawingProject<'_>, result: &SynthesisResult) -> Layered {
    let d = p.drawing();
    let mut out = Layered::default();
    for view in p.views().filter(|v| v.kind == ViewKind::Plan && v.transform().is_some()) {
        if d.block(&assembly_block_name(view.id)).is_none() {
            continue;
        }
        plan_strips(d, p, &view, result, &mut out);
    }
    if out.replaced.is_empty() {
        return out;
    }
    for (i, solid) in result.solids.iter().enumerate() {
        let Some(&e) = solid.entity_ids.first() else { continue };
        let Some(n) = p.entity(e).and_then(|x| x.polyline.as_ref().map(|poly| poly.segments.len())) else { continue };
        if n > 0 && (0..n).all(|k| out.replaced.contains(&(e, k))) {
            out.dropped_solids.insert(i);
        }
    }
    out
}

/// A boundary of a run's layers: its signed offset from the reference face line (positive toward
/// the centreline) and its band line, `None` for the reference itself.
struct Boundary<'a> {
    offset: f64,
    line: Option<&'a AssemblyLine>,
}

fn plan_strips(d: &Drawing, p: &DrawingProject<'_>, view: &lamb_core::derivation::read::ViewRef<'_>, result: &SynthesisResult, out: &mut Layered) {
    let Ok(runs) = assembly_lines(d, view.id) else { return };
    let (graph, sides) = graph_and_sides(p, view.id);
    let tol = TOLERANCE.coincidence_mm;
    for ra in runs.iter().filter(|r| r.skipped.is_none() && r.bands > 0) {
        let Some(face) = ra.face else { continue };
        let (Some(run), Some(side)) = (graph.runs.get(ra.run), sides.runs.get(ra.run)) else { continue };
        let RunCurve::Line { from: c0, to: c1 } = run.centreline else { continue };
        let Ok(reference) = lay_bands(run, side, face, &[Band { offset: 0.0, layer: String::new() }]) else { continue };
        let Some(reference) = reference.first() else { continue };
        let (a, b) = (reference.from, reference.to);
        let len = sub(b, a).x.hypot(sub(b, a).y);
        if len <= tol {
            continue;
        }
        let u = Point2 { x: (b.x - a.x) / len, y: (b.y - a.y) / len };
        let mid_c = Point2 { x: (c0.x + c1.x) / 2.0, y: (c0.y + c1.y) / 2.0 };
        let mut n = Point2 { x: -u.y, y: u.x };
        if dot(sub(mid_c, a), n) < 0.0 {
            n = Point2 { x: u.y, y: -u.x };
        }
        let off = |q: Point2| dot(sub(q, a), n);
        let Some(bands) = ra.lines.get(..ra.bands) else { continue };
        if bands.iter().any(|l| l.arc.is_some()) {
            continue;
        }
        let mut all: Vec<Boundary<'_>> = vec![Boundary { offset: 0.0, line: None }];
        all.extend(
            bands.iter().map(|l| Boundary { offset: off(Point2 { x: (l.from.x + l.to.x) / 2.0, y: (l.from.y + l.to.y) / 2.0 }), line: Some(l) }),
        );
        all.sort_by(|x, y| x.offset.total_cmp(&y.offset));
        let mut merged: Vec<Boundary<'_>> = Vec::with_capacity(all.len());
        for bd in all {
            match merged.last_mut() {
                Some(last) if (bd.offset - last.offset).abs() <= tol => {
                    if last.line.is_none() {
                        *last = bd;
                    }
                }
                _ => merged.push(bd),
            }
        }

        let ends = [&run.faces[0].from, &run.faces[1].from];
        let keys = ends.map(|f| (f.entity_id, f.segment_index));
        let mut zs: Vec<f64> = Vec::new();
        let mut provs: Vec<ProvenanceKind> = Vec::new();
        for s in result.surfaces.iter().filter(|s| keys.contains(&(s.entity_id, s.segment_index))) {
            zs.extend(s.triangles.iter().map(|q| q.z));
            provs.push(s.provenance);
        }
        if zs.is_empty() {
            for s in result.solids.iter().filter(|s| s.entity_ids.first().is_some_and(|e| keys.iter().any(|k| k.0 == *e))) {
                zs.extend(s.faces.iter().flatten().map(|q| q.z));
                provs.push(s.provenance);
            }
        }
        let zs: Vec<f64> = zs.into_iter().filter(|z| z.is_finite()).collect();
        let (bottom, top) = (zs.iter().copied().fold(f64::INFINITY, f64::min), zs.iter().copied().fold(f64::NEG_INFINITY, f64::max));
        if top - bottom <= tol || !(top - bottom).is_finite() {
            continue;
        }
        let provenance = most_doubtful(&provs);
        let foot = |q: Point2| {
            let h = off(q);
            Point2 { x: q.x - n.x * h, y: q.y - n.y * h }
        };
        let mut emitted = false;
        for pair in merged.windows(2) {
            let [lo, hi] = pair else { continue };
            if hi.offset - lo.offset <= tol {
                continue;
            }
            let far = if hi.offset.abs() >= lo.offset.abs() { hi } else { lo };
            let Some(layer) = far.line.map(|l| l.layer.clone()) else { continue };
            let quad = match (lo.line, hi.line) {
                (Some(l), Some(h)) => [l.from, l.to, h.to, h.from],
                (Some(l), None) | (None, Some(l)) => [l.from, l.to, foot(l.to), foot(l.from)],
                (None, None) => continue,
            };
            let Ok(global) = quad.iter().map(|&q| plan_local_to_global_xy(view, q)).collect::<Result<Vec<_>, _>>() else { continue };
            let base: Vec<Point3> = global.iter().map(|g| Point3 { x: g.x, y: g.y, z: bottom }).collect();
            let faces = prism_from_loop(&base, Point3 { x: 0.0, y: 0.0, z: 1.0 }, 0.0, top - bottom);
            let solid =
                SynthesizedSolid { entity_ids: keys.map(|k| k.0).to_vec(), provenance, faces, form: SolidForm::Shell, layer_id: layer.clone() };
            let triangles = solid_triangles(&solid);
            if triangles.is_empty() {
                continue;
            }
            out.strips.push(LayerStrip { sources: keys.map(|k| k.0), provenance, layer, triangles });
            emitted = true;
        }
        if emitted {
            out.replaced.extend(keys);
        }
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::Handle;
    use cadcraft_engine::Session;
    use serde_json::json;

    use crate::assembly::tests::room;
    use crate::scene::{SceneMeshKind, pick, scene};
    use lamb_core::model::Id;

    type R = Result<(), Box<dyn std::error::Error>>;
    type T<X> = Result<X, Box<dyn std::error::Error>>;

    fn placed(tagged: bool) -> T<(Session, Id)> {
        let (mut s, plan) = room("exterior_face", tagged)?;
        let level = Id(s.execute("level", &json!({ "mode": "create", "name": "L1", "floorElevationMm": 0 }))?["levelId"].as_u64().ok_or("no id")?);
        let t = json!({ "originX": 0.0, "originY": 0.0, "rotation": 0.0 });
        s.execute("placeview", &json!({ "viewId": plan.0, "kind": "plan", "levelId": level.0, "cutElevation": 1200, "transform": t }))?;
        Ok((s, plan))
    }

    fn plines(s: &Session, plan: Id) -> T<Vec<Handle>> {
        let d = s.doc()?;
        let b = d.block(&crate::schema::view_block_name(plan)).ok_or("no view block")?;
        Ok(b.entities.iter().filter(|e| matches!(e.kind, cadcraft_doc::EntityKind::LwPolyline(_))).map(|e| e.handle).collect())
    }

    #[test]
    fn a_laid_room_shows_as_three_layer_prisms_per_run_replacing_its_solids() -> R {
        let (mut s, plan) = placed(true)?;
        let before = scene(s.doc()?)?;
        let solids: Vec<_> = before.meshes.iter().filter(|m| m.kind == SceneMeshKind::Solid).collect();
        assert_eq!(solids.len(), 2, "the two room plines derive solids");
        let zr = |ms: &[&crate::scene::SceneMesh], sc: &crate::scene::Scene| {
            let z = ms.iter().flat_map(|m| m.positions.iter()).map(|p| sc.origin[2] + f64::from(p[2]));
            (z.clone().fold(f64::INFINITY, f64::min), z.fold(f64::NEG_INFINITY, f64::max))
        };
        let (zlo, zhi) = zr(&solids, &before);

        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let sc = scene(s.doc()?)?;
        let layers: Vec<_> = sc.meshes.iter().filter(|m| m.kind == SceneMeshKind::Layer).collect();
        assert_eq!(layers.len(), 12, "3 per run");
        for run in layers.chunks(3) {
            assert_eq!(run.iter().map(|m| m.layer.as_str()).collect::<Vec<_>>(), ["CLAD", "GYP", "GYP"]);
        }
        assert!(sc.meshes.iter().all(|m| m.kind != SceneMeshKind::Solid), "the plines' solids are replaced");
        let faces = plines(&s, plan)?;
        assert!(layers.iter().all(|m| m.sources.len() == 2 && m.sources.iter().all(|h| faces.contains(h)) && m.segment_index.is_none()));

        // The bottom run's CLAD: plan y −20..0, over the solids' z range.
        let y = |m: &crate::scene::SceneMesh| {
            let ys = m.positions.iter().map(|p| sc.origin[1] + f64::from(p[1]));
            (ys.clone().fold(f64::INFINITY, f64::min), ys.fold(f64::NEG_INFINITY, f64::max))
        };
        let clad = layers.iter().find(|m| m.layer == "CLAD" && y(m).1 < 1.0).ok_or("no bottom CLAD")?;
        let (ylo, yhi) = y(clad);
        assert!((ylo + 20.0).abs() < 0.01 && yhi.abs() < 0.01, "{ylo}..{yhi}");
        let (lo, hi) = zr(&[*clad], &sc);
        assert!((lo - zlo).abs() < 0.01 && (hi - zhi).abs() < 0.01, "{lo}..{hi} vs {zlo}..{zhi}");
        let gyp: Vec<_> = layers.iter().filter(|m| m.layer == "GYP" && y(m).0 > -1.0 && y(m).1 < 250.0).map(|m| y(m)).collect();
        assert_eq!(gyp.len(), 2);
        assert!(gyp.iter().any(|g| g.0.abs() < 0.01 && (g.1 - 50.0).abs() < 0.01), "{gyp:?}");
        assert!(gyp.iter().any(|g| (g.0 - 50.0).abs() < 0.01 && (g.1 - 200.0).abs() < 0.01), "{gyp:?}");

        // A ray straight down onto the bottom CLAD names the run's face handles.
        let hit = pick(&sc, [2500.0, -10.0, 100_000.0], [0.0, 0.0, -1.0]).ok_or("missed")?;
        let mesh = &sc.meshes[hit.mesh];
        assert_eq!(mesh.kind, SceneMeshKind::Layer);
        assert_eq!(mesh.layer, "CLAD");
        let mut got = hit.sources.clone();
        got.sort_by_key(|h| h.0);
        let mut want = faces.clone();
        want.sort_by_key(|h| h.0);
        assert_eq!(got, want);
        Ok(())
    }

    #[test]
    fn no_assembly_block_or_no_tag_leaves_the_scene_unchanged() -> R {
        let (s, _) = placed(true)?;
        let sc = scene(s.doc()?)?;
        assert!(sc.meshes.iter().all(|m| m.kind != SceneMeshKind::Layer));
        let synth = lamb_core::derivation::synthesis::synthesize(&crate::project::DrawingProject::new(s.doc()?)?)?;
        assert_eq!(sc.meshes.len(), synth.surfaces.len() + synth.solids.len());

        let (mut s, plan) = placed(false)?;
        let before = scene(s.doc()?)?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let after = scene(s.doc()?)?;
        assert!(after.meshes.iter().all(|m| m.kind != SceneMeshKind::Layer));
        assert_eq!(before, after);
        Ok(())
    }
}

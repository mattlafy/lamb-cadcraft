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
//! Each repeat copy of such a run (studs, LAMB T489) whose lines chain end to start into one closed
//! loop becomes a prism of that loop over the same z range, in its first line's layer, after the
//! run's strips; a copy that does not close stays 2D.
//! Each placed corner and jamb detail (LAMB T491) then adds, per adjoining straight run with a z
//! range, strips from its lines parallel to that run (same boundary rule, each strip over the
//! overlap of its two boundaries' extents) and a prism per closed chain of its lines (whose lines
//! bound no strip), replacing nothing.

use std::collections::BTreeSet;

use cadcraft_doc::Drawing;
use lamb_core::derivation::assembly::{AssemblyLine, Band, FaceRef, lay_bands};
use lamb_core::derivation::projection::plan_local_to_global_xy;
use lamb_core::derivation::read::{ProjectRead, ViewKind, ViewRef};
use lamb_core::derivation::resolve::ProvenanceKind;
use lamb_core::derivation::solids::{SolidForm, SynthesizedSolid, most_doubtful, prism_from_loop, solid_triangles};
use lamb_core::derivation::synthesis::SynthesisResult;
use lamb_core::derivation::wall_spaces::RunSides;
use lamb_core::derivation::walls::{RunCurve, WallRun};
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::geometry::{Point2, Point3};
use lamb_core::model::Id;

use crate::assembly::{assembly_lines, corner_assembly, opening_assembly};
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

/// A straight run's reference face line as a frame: origin `a` at its start, unit `u` along it and
/// unit `n` normal to it toward the centreline.
struct Frame {
    a: Point2,
    u: Point2,
    n: Point2,
}

impl Frame {
    /// Signed offset of `q` from the reference line, positive toward the centreline.
    fn off(&self, q: Point2) -> f64 {
        dot(sub(q, self.a), self.n)
    }

    /// Distance of `q`'s foot along the reference line from `a`.
    fn along(&self, q: Point2) -> f64 {
        dot(sub(q, self.a), self.u)
    }

    fn at(&self, t: f64, h: f64) -> Point2 {
        Point2 { x: self.a.x + self.u.x * t + self.n.x * h, y: self.a.y + self.u.y * t + self.n.y * h }
    }

    fn foot(&self, q: Point2) -> Point2 {
        let h = self.off(q);
        Point2 { x: q.x - self.n.x * h, y: q.y - self.n.y * h }
    }
}

/// The frame of straight `run`'s `face` line ([`lay_bands`] at offset 0), or `None` for an arc run
/// or a degenerate face.
fn run_frame(run: &WallRun, side: &RunSides, face: FaceRef) -> Option<Frame> {
    let RunCurve::Line { from: c0, to: c1 } = run.centreline else { return None };
    let reference = lay_bands(run, side, face, &[Band { offset: 0.0, layer: String::new() }]).ok()?;
    let reference = reference.first()?;
    let (a, b) = (reference.from, reference.to);
    let len = sub(b, a).x.hypot(sub(b, a).y);
    if len <= TOLERANCE.coincidence_mm {
        return None;
    }
    let u = Point2 { x: (b.x - a.x) / len, y: (b.y - a.y) / len };
    let mid_c = Point2 { x: (c0.x + c1.x) / 2.0, y: (c0.y + c1.y) / 2.0 };
    let mut n = Point2 { x: -u.y, y: u.x };
    if dot(sub(mid_c, a), n) < 0.0 {
        n = Point2 { x: u.y, y: -u.x };
    }
    Some(Frame { a, u, n })
}

/// The z range a run's massing has, with the run's face keys and the most doubtful provenance.
struct ZRange {
    bottom: f64,
    top: f64,
    provenance: ProvenanceKind,
    keys: [(Id, usize); 2],
}

/// `run`'s z range: the synthesized surfaces of its two face segments, else the solid of either
/// face's entity; `None` when that has no height.
fn run_z(run: &WallRun, result: &SynthesisResult) -> Option<ZRange> {
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
    if top - bottom <= TOLERANCE.coincidence_mm || !(top - bottom).is_finite() {
        return None;
    }
    Some(ZRange { bottom, top, provenance: most_doubtful(&provs), keys })
}

/// Pushes the prism of plan-local loop `local` over `z` in `layer`; false when nothing was emitted.
fn push_prism(view: &ViewRef<'_>, local: &[Point2], layer: String, z: &ZRange, out: &mut Layered) -> bool {
    let Ok(global) = local.iter().map(|&q| plan_local_to_global_xy(view, q)).collect::<Result<Vec<_>, _>>() else { return false };
    let base: Vec<Point3> = global.iter().map(|g| Point3 { x: g.x, y: g.y, z: z.bottom }).collect();
    let faces = prism_from_loop(&base, Point3 { x: 0.0, y: 0.0, z: 1.0 }, 0.0, z.top - z.bottom);
    let sources = z.keys.map(|k| k.0);
    let solid = SynthesizedSolid { entity_ids: sources.to_vec(), provenance: z.provenance, faces, form: SolidForm::Shell, layer_id: layer.clone() };
    let triangles = solid_triangles(&solid);
    if triangles.is_empty() {
        return false;
    }
    out.strips.push(LayerStrip { sources, provenance: z.provenance, layer, triangles });
    true
}

/// `all` sorted by offset and merged within tolerance, a band line replacing the reference it
/// coincides with.
fn merge(mut all: Vec<Boundary<'_>>) -> Vec<Boundary<'_>> {
    let tol = TOLERANCE.coincidence_mm;
    all.sort_by(|x, y| x.offset.total_cmp(&y.offset));
    let mut out: Vec<Boundary<'_>> = Vec::with_capacity(all.len());
    for bd in all {
        match out.last_mut() {
            Some(last) if (bd.offset - last.offset).abs() <= tol => {
                if last.line.is_none() {
                    *last = bd;
                }
            }
            _ => out.push(bd),
        }
    }
    out
}

fn midpoint(l: &AssemblyLine) -> Point2 {
    Point2 { x: (l.from.x + l.to.x) / 2.0, y: (l.from.y + l.to.y) / 2.0 }
}

/// The layer of a pair of consecutive boundaries — the boundary farther from the face — or `None`
/// for a pair too thin or with no band line on that side.
fn pair_layer<'a>(lo: &Boundary<'a>, hi: &Boundary<'a>) -> Option<String> {
    if hi.offset - lo.offset <= TOLERANCE.coincidence_mm {
        return None;
    }
    let far = if hi.offset.abs() >= lo.offset.abs() { hi } else { lo };
    far.line.map(|l| l.layer.clone())
}

fn plan_strips(d: &Drawing, p: &DrawingProject<'_>, view: &ViewRef<'_>, result: &SynthesisResult, out: &mut Layered) {
    let Ok(runs) = assembly_lines(d, view.id) else { return };
    let (graph, sides) = graph_and_sides(p, view.id);
    let tol = TOLERANCE.coincidence_mm;
    for ra in runs.iter().filter(|r| r.skipped.is_none() && r.bands > 0) {
        let Some(face) = ra.face else { continue };
        let (Some(run), Some(side)) = (graph.runs.get(ra.run), sides.runs.get(ra.run)) else { continue };
        let Some(fr) = run_frame(run, side, face) else { continue };
        let Some(bands) = ra.lines.get(..ra.bands) else { continue };
        if bands.iter().any(|l| l.arc.is_some()) {
            continue;
        }
        let mut all: Vec<Boundary<'_>> = vec![Boundary { offset: 0.0, line: None }];
        all.extend(bands.iter().map(|l| Boundary { offset: fr.off(midpoint(l)), line: Some(l) }));
        let merged = merge(all);
        let Some(z) = run_z(run, result) else { continue };
        let mut emitted = false;
        for pair in merged.windows(2) {
            let [lo, hi] = pair else { continue };
            let Some(layer) = pair_layer(lo, hi) else { continue };
            let quad = match (lo.line, hi.line) {
                (Some(l), Some(h)) => [l.from, l.to, h.to, h.from],
                (Some(l), None) | (None, Some(l)) => [l.from, l.to, fr.foot(l.to), fr.foot(l.from)],
                (None, None) => continue,
            };
            emitted |= push_prism(view, &quad, layer, &z, out);
        }
        let mut start = ra.bands;
        for &count in &ra.copies {
            let Some(copy) = ra.lines.get(start..start + count) else { break };
            start += count;
            if let (Some(ring), Some(head)) = (closed_loop(copy, tol), copy.first()) {
                push_prism(view, &ring, head.layer.clone(), &z, out);
            }
        }
        if emitted {
            out.replaced.extend(z.keys);
        }
    }

    // Placed corner and jamb details (LAMB T491): after the runs' strips, replacing nothing.
    let faces: Vec<(usize, FaceRef)> = runs.iter().filter_map(|r| r.face.map(|f| (r.run, f))).collect();
    let corners = corner_assembly(d, view.id).unwrap_or_default();
    let openings = opening_assembly(d, view.id).unwrap_or_default();
    let mut groups: Vec<(&[AssemblyLine], Vec<usize>)> = Vec::new();
    for c in corners.iter().filter(|c| c.skipped.is_none()) {
        let Some(j) = graph.junctions.get(c.junction) else { continue };
        groups.push((&c.lines, j.arms.iter().map(|a| a.run).collect()));
    }
    for o in openings.iter().filter(|o| o.skipped.is_none() && o.lines.len() % 2 == 0) {
        let Some(b) = graph.bridges.get(o.bridge) else { continue };
        let (first, second) = o.lines.split_at(o.lines.len() / 2);
        groups.push((first, vec![b.a.0]));
        groups.push((second, vec![b.b.0]));
    }
    let ctx = DetailCtx { view, runs: &graph.runs, sides: &sides.runs, faces: &faces, result };
    for (lines, adjoining) in groups {
        ctx.meshes(lines, &adjoining, out);
    }
}

/// What placing one corner or jamb detail's meshes reads.
struct DetailCtx<'a> {
    view: &'a ViewRef<'a>,
    runs: &'a [WallRun],
    sides: &'a [RunSides],
    /// Each laid run's reference face, by run index.
    faces: &'a [(usize, FaceRef)],
    result: &'a SynthesisResult,
}

impl DetailCtx<'_> {
    /// One placed detail's meshes: per `adjoining` straight run with a z range, `lines` parallel to
    /// it as strips over the overlap of consecutive boundaries; then each closed chain of `lines` as
    /// a prism over the first such run's z range. A closed chain's lines are not strip boundaries.
    fn meshes(&self, lines: &[AssemblyLine], adjoining: &[usize], out: &mut Layered) {
        let tol = TOLERANCE.coincidence_mm;
        let sin_tol = TOLERANCE.angle_deg.to_radians().sin();
        let near = |a: Point2, b: Point2| (a.x - b.x).hypot(a.y - b.y) <= tol;
        let mut rings: Vec<(Vec<Point2>, String)> = Vec::new();
        let mut in_ring = vec![false; lines.len()];
        let mut start = 0;
        for k in 1..=lines.len() {
            let (Some(prev), Some(head)) = (lines.get(k - 1), lines.get(start)) else { break };
            if !near(prev.to, head.from) && lines.get(k).is_some_and(|l| near(l.from, prev.to)) {
                continue;
            }
            if let Some(ring) = lines.get(start..k).and_then(|chain| closed_loop(chain, tol)) {
                rings.push((ring, head.layer.clone()));
                in_ring.iter_mut().take(k).skip(start).for_each(|f| *f = true);
            }
            start = k;
        }
        let mut first_z: Option<ZRange> = None;
        for &i in adjoining {
            let (Some(run), Some(side)) = (self.runs.get(i), self.sides.get(i)) else { continue };
            let Some(&(_, face)) = self.faces.iter().find(|f| f.0 == i) else { continue };
            let (Some(fr), Some(z)) = (run_frame(run, side, face), run_z(run, self.result)) else { continue };
            let parallel = |l: &AssemblyLine| {
                let v = sub(l.to, l.from);
                let len = v.x.hypot(v.y);
                l.arc.is_none() && len > tol && (dot(v, fr.n) / len).abs() <= sin_tol
            };
            let mut all: Vec<Boundary<'_>> = vec![Boundary { offset: 0.0, line: None }];
            all.extend(
                lines.iter().zip(&in_ring).filter(|(l, r)| !**r && parallel(l)).map(|(l, _)| Boundary { offset: fr.off(midpoint(l)), line: Some(l) }),
            );
            let extent = |l: &AssemblyLine| {
                let (s, e) = (fr.along(l.from), fr.along(l.to));
                (s.min(e), s.max(e))
            };
            for pair in merge(all).windows(2) {
                let [lo, hi] = pair else { continue };
                let Some(layer) = pair_layer(lo, hi) else { continue };
                let (t0, t1) = match (lo.line, hi.line) {
                    (Some(l), Some(h)) => {
                        let (x, y) = (extent(l), extent(h));
                        (x.0.max(y.0), x.1.min(y.1))
                    }
                    (Some(l), None) | (None, Some(l)) => extent(l),
                    (None, None) => continue,
                };
                if t1 - t0 <= tol {
                    continue;
                }
                let quad = [fr.at(t0, lo.offset), fr.at(t1, lo.offset), fr.at(t1, hi.offset), fr.at(t0, hi.offset)];
                push_prism(self.view, &quad, layer, &z, out);
            }
            if first_z.is_none() {
                first_z = Some(z);
            }
        }
        let Some(z) = first_z else { return };
        for (ring, layer) in rings {
            push_prism(self.view, &ring, layer, &z, out);
        }
    }
}

/// `lines` chained end to start into one closed loop of at least three distinct points (the closing
/// point not repeated), or `None` when they do not close or include an arc.
fn closed_loop(lines: &[AssemblyLine], tol: f64) -> Option<Vec<Point2>> {
    let near = |a: Point2, b: Point2| (a.x - b.x).hypot(a.y - b.y) <= tol;
    let first = lines.first()?;
    let mut ring = vec![first.from];
    let mut end = first.from;
    for l in lines {
        if l.arc.is_some() || !near(l.from, end) {
            return None;
        }
        if !near(l.to, l.from) {
            ring.push(l.to);
        }
        end = l.to;
    }
    if !near(end, first.from) {
        return None;
    }
    ring.pop();
    let mut distinct: Vec<Point2> = Vec::new();
    for q in &ring {
        if !distinct.iter().any(|d| near(*d, *q)) {
            distinct.push(*q);
        }
    }
    (distinct.len() >= 3).then_some(ring)
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::Handle;
    use cadcraft_engine::Session;
    use serde_json::json;

    use crate::assembly::tests::room;
    use crate::scene::{SceneMeshKind, pick, scene};
    use lamb_core::derivation::read::ProjectRead;
    use lamb_core::model::Id;

    type R = Result<(), Box<dyn std::error::Error>>;
    type T<X> = Result<X, Box<dyn std::error::Error>>;

    fn placed(tagged: bool) -> T<(Session, Id)> {
        let (mut s, plan) = room("exterior_face", tagged)?;
        place(&mut s, plan)?;
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

    fn handles(s: &Session) -> T<std::collections::HashSet<Handle>> {
        let d = s.doc()?;
        Ok(d.model.iter().map(|e| e.handle).chain(d.blocks.values().flat_map(|b| b.entities.iter().map(|e| e.handle))).collect())
    }

    fn drawn(s: &mut Session, cmd: &str, p: serde_json::Value) -> T<Handle> {
        let before = handles(s)?;
        s.execute(cmd, &p)?;
        Ok(handles(s)?.into_iter().find(|h| !before.contains(h)).ok_or("nothing drawn")?)
    }

    /// [`placed`] (tagged) with a STUD repeat in detail D1 at a 400 mm pitch — the closed stud
    /// outline, or with `open` a single stud line — then ASSEMBLYPLAN.
    fn studded(open: bool) -> T<(Session, Id)> {
        let (mut s, plan) = placed(true)?;
        let detail = {
            let p = crate::project::DrawingProject::new(s.doc()?)?;
            p.views().find(|v| v.kind == lamb_core::derivation::read::ViewKind::Detail).ok_or("no detail")?.id
        };
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        s.execute("layer.new", &json!({ "name": "STUD", "current": true }))?;
        let stud = if open {
            drawn(&mut s, "line", json!({ "points": [[100, 50], [100, 200]] }))?
        } else {
            drawn(&mut s, "pline", json!({ "vertices": [[100, 50], [138, 50], [138, 200], [100, 200]], "closed": true }))?
        };
        let pitch = drawn(&mut s, "dimaligned", json!({ "p1": [100, 0], "p2": [500, 0], "at": [300, -300] }))?;
        s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [stud.hex()] }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        Ok((s, plan))
    }

    fn range(sc: &crate::scene::Scene, m: &crate::scene::SceneMesh, axis: usize) -> (f64, f64) {
        let v = m.positions.iter().map(|p| sc.origin[axis] + f64::from(p[axis]));
        (v.clone().fold(f64::INFINITY, f64::min), v.fold(f64::NEG_INFINITY, f64::max))
    }

    #[test]
    fn each_closed_stud_copy_becomes_a_prism_over_the_strips_z_range() -> R {
        let (s, plan) = studded(false)?;
        let d = s.doc()?;
        let copies: usize = crate::assembly::assembly_lines(d, plan)?.iter().map(|r| r.copies.len()).sum();
        assert!(copies >= 4, "{copies} copies");
        let sc = scene(d)?;
        let layers: Vec<_> = sc.meshes.iter().filter(|m| m.kind == SceneMeshKind::Layer).collect();
        let studs: Vec<_> = layers.iter().filter(|m| m.layer == "STUD").collect();
        assert_eq!(studs.len(), copies, "one mesh per copy");
        let faces = plines(&s, plan)?;
        assert!(studs.iter().all(|m| m.sources.len() == 2 && m.sources.iter().all(|h| faces.contains(h))));

        // The strips are T487's, unchanged.
        let (mut plain, plain_plan) = placed(true)?;
        plain.execute("assemblyplan", &json!({ "viewId": plain_plan.0 }))?;
        let base = scene(plain.doc()?)?;
        let strips: Vec<_> = layers.iter().filter(|m| m.layer != "STUD").collect();
        let want: Vec<_> = base.meshes.iter().filter(|m| m.kind == SceneMeshKind::Layer).collect();
        assert_eq!(strips.len(), want.len());
        let clad = strips.iter().find(|m| m.layer == "CLAD").ok_or("no CLAD")?;
        let (zlo, zhi) = range(&sc, clad, 2);
        for (a, b) in strips.iter().zip(&want) {
            assert_eq!(a.layer, b.layer);
            for k in 0..3 {
                let (x, y) = (range(&sc, a, k), range(&base, b, k));
                assert!((x.0 - y.0).abs() < 0.01 && (x.1 - y.1).abs() < 0.01, "{x:?} vs {y:?}");
            }
        }

        // The bottom run's studs: 38 mm along x, plan y 50..200, over the strips' z range.
        let bottom: Vec<_> = studs.iter().filter(|m| range(&sc, m, 1).1 < 250.0).collect();
        assert!(!bottom.is_empty());
        let first = bottom.iter().min_by(|a, b| range(&sc, a, 0).0.total_cmp(&range(&sc, b, 0).0)).ok_or("no stud")?;
        let (xlo, xhi) = range(&sc, first, 0);
        let (ylo, yhi) = range(&sc, first, 1);
        let (lo, hi) = range(&sc, first, 2);
        assert!((xhi - xlo - 38.0).abs() < 0.01, "{xlo}..{xhi}");
        assert!((ylo - 50.0).abs() < 0.01 && (yhi - 200.0).abs() < 0.01, "{ylo}..{yhi}");
        assert!((lo - zlo).abs() < 0.01 && (hi - zhi).abs() < 0.01, "{lo}..{hi} vs {zlo}..{zhi}");
        Ok(())
    }

    #[test]
    fn a_repeat_of_one_open_line_adds_no_mesh() -> R {
        let (s, plan) = studded(true)?;
        let d = s.doc()?;
        assert!(crate::assembly::assembly_lines(d, plan)?.iter().any(|r| !r.copies.is_empty()), "copies laid");
        let sc = scene(d)?;
        assert!(sc.meshes.iter().all(|m| m.layer != "STUD"));
        assert_eq!(sc.meshes.iter().filter(|m| m.kind == SceneMeshKind::Layer).count(), 12);
        Ok(())
    }

    /// Places `plan` on a new level L1 at the origin.
    fn place(s: &mut Session, plan: Id) -> R {
        let level = Id(s.execute("level", &json!({ "mode": "create", "name": "L1", "floorElevationMm": 0 }))?["levelId"].as_u64().ok_or("no id")?);
        let t = json!({ "originX": 0.0, "originY": 0.0, "rotation": 0.0 });
        s.execute("placeview", &json!({ "viewId": plan.0, "kind": "plan", "levelId": level.0, "cutElevation": 1200, "transform": t }))?;
        Ok(())
    }

    /// [`placed`] (tagged) with [`corner_detail`](crate::assembly::tests::corner_detail) at every
    /// corner — with `post`, plus a closed `POST` pline (−300,50)…(−200,150) in it — then ASSEMBLYPLAN.
    fn cornered(post: bool) -> T<(Session, Id)> {
        let (mut s, plan) = placed(true)?;
        let cd = crate::assembly::tests::corner_detail(&mut s, plan, "exterior_face", false, (0.0, 0.0))?;
        if post {
            s.execute("editview", &json!({ "viewId": cd.0 }))?;
            s.execute("layer.new", &json!({ "name": "POST", "current": true }))?;
            s.execute("pline", &json!({ "vertices": [[-300, 50], [-200, 50], [-200, 150], [-300, 150]], "closed": true }))?;
            s.execute("editview", &json!({ "viewId": plan.0 }))?;
        }
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        Ok((s, plan))
    }

    #[test]
    fn each_placed_corner_fills_its_band_over_the_arm_z_range() -> R {
        let (s, _) = cornered(false)?;
        let sc = scene(s.doc()?)?;
        let layers: Vec<_> = sc.meshes.iter().filter(|m| m.kind == SceneMeshKind::Layer).collect();
        let crn: Vec<_> = layers.iter().filter(|m| m.layer.starts_with("CRN")).collect();
        assert_eq!(crn.len(), 4, "{:?}", crn.iter().map(|m| &m.layer).collect::<Vec<_>>());
        assert!(crn.iter().all(|m| m.layer == "CRN-B"));
        let clad = layers.iter().find(|m| m.layer == "CLAD").ok_or("no CLAD")?;
        let (zlo, zhi) = range(&sc, clad, 2);
        for m in &crn {
            let (x, y, z) = (range(&sc, m, 0), range(&sc, m, 1), range(&sc, m, 2));
            let (w, h) = (x.1 - x.0, y.1 - y.0);
            assert!((w - 1000.0).abs() < 0.01 && (h - 200.0).abs() < 0.01 || (w - 200.0).abs() < 0.01 && (h - 1000.0).abs() < 0.01, "{x:?} {y:?}");
            // Spans face to offset 200: one side on the outer face of the box.
            let on_face = |r: (f64, f64), hi: f64| r.0.abs() < 0.01 || (r.1 - hi).abs() < 0.01;
            if h < w {
                assert!(on_face(y, 4000.0), "{y:?}")
            } else {
                assert!(on_face(x, 5000.0), "{x:?}")
            }
            assert!((z.0 - zlo).abs() < 0.01 && (z.1 - zhi).abs() < 0.01, "{z:?} vs {zlo}..{zhi}");
            assert_eq!(m.sources.len(), 2);
        }

        // The run strips are the uncornered room's, trimmed: same count and layers.
        let (mut plain, plain_plan) = placed(true)?;
        plain.execute("assemblyplan", &json!({ "viewId": plain_plan.0 }))?;
        let base = scene(plain.doc()?)?;
        let want: Vec<_> = base.meshes.iter().filter(|m| m.kind == SceneMeshKind::Layer).map(|m| m.layer.clone()).collect();
        let got: Vec<_> = layers.iter().filter(|m| !m.layer.starts_with("CRN")).map(|m| m.layer.clone()).collect();
        assert_eq!(got, want);
        let total = |sc: &crate::scene::Scene| -> f64 {
            sc.meshes
                .iter()
                .filter(|m| m.kind == SceneMeshKind::Layer && !m.layer.starts_with("CRN"))
                .map(|m| range(sc, m, 0).1 - range(sc, m, 0).0 + range(sc, m, 1).1 - range(sc, m, 1).0)
                .sum()
        };
        assert!(total(&sc) < total(&base) - 1.0, "the corners trim the run strips");
        Ok(())
    }

    #[test]
    fn a_closed_chain_in_a_corner_detail_is_one_prism_per_corner() -> R {
        let (s, _) = cornered(true)?;
        let sc = scene(s.doc()?)?;
        let layers: Vec<_> = sc.meshes.iter().filter(|m| m.kind == SceneMeshKind::Layer).collect();
        let posts: Vec<_> = layers.iter().filter(|m| m.layer == "POST").collect();
        assert_eq!(posts.len(), 4);
        for m in &posts {
            let (x, y) = (range(&sc, m, 0), range(&sc, m, 1));
            assert!((x.1 - x.0 - 100.0).abs() < 0.01 && (y.1 - y.0 - 100.0).abs() < 0.01, "{x:?} {y:?}");
        }
        assert_eq!(layers.iter().filter(|m| m.layer == "CRN-B").count(), 4, "the post bounds no strip");
        Ok(())
    }

    #[test]
    fn a_jamb_detail_fills_both_jambs() -> R {
        let layered_of = |tag: bool| -> T<crate::scene::Scene> {
            let (mut s, plan) = crate::assembly::tests::window_room(false, tag)?;
            place(&mut s, plan)?;
            s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
            Ok(scene(s.doc()?)?)
        };
        let (base, sc) = (layered_of(false)?, layered_of(true)?);
        let count = |sc: &crate::scene::Scene| sc.meshes.iter().filter(|m| m.kind == SceneMeshKind::Layer).count();
        assert_eq!(count(&sc), count(&base) + 2, "one GYP strip per jamb");
        // Each jamb's GYP band: 50 along the bottom wall beside the gap, face (y 0) to 200.
        let jambs: Vec<_> = sc
            .meshes
            .iter()
            .filter(|m| m.kind == SceneMeshKind::Layer && m.layer == "GYP")
            .map(|m| (range(&sc, m, 0), range(&sc, m, 1)))
            .filter(|(x, y)| (x.1 - x.0 - 50.0).abs() < 0.01 && y.0.abs() < 0.01 && (y.1 - 200.0).abs() < 0.01)
            .collect();
        assert_eq!(jambs.len(), 2, "{jambs:?}");
        assert!(jambs.iter().any(|(x, _)| (x.1 - 2000.0).abs() < 0.01), "{jambs:?}");
        assert!(jambs.iter().any(|(x, _)| (x.0 - 3000.0).abs() < 0.01), "{jambs:?}");
        Ok(())
    }
}

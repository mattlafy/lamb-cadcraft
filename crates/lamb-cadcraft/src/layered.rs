//! Layered wall solids for the 3D scene (LAMB's `synthesis-rules.md` "Tier 2: detail-resolved
//! elements" and `ghosting-and-visualization.md` "partial"; LAMB T487). In a placed plan that has
//! an assembly block (the user ran ASSEMBLYPLAN), every straight run whose detail laid continuous
//! bands ([`assembly_lines`]) becomes one thin prism per gap between consecutive boundaries — the
//! run's reference face line and each laid band line — in the layer of the gap's boundary farther
//! from the reference face, extruded over the z range the run's massing already has (the
//! synthesized surfaces of its two face segments, else the solid of either face's entity). Those
//! face segments' surfaces are then replaced, and a solid is dropped once every segment of its plan
//! entity is. An arc run whose bands are all arcs (LAMB T492) does the same with concentric
//! boundaries: each strip is the ring between two consecutive arcs over the overlap of their angular
//! spans, sampled as `asmview`'s ghost face lines are. Skipped runs and runs mixing arc and straight
//! bands keep their massing; a run with no massing to take a z range from emits nothing.
//! Unreadable layered data yields no layers, never an error.
//! Each repeat copy of such a run (studs, LAMB T489) whose lines chain end to start into one closed
//! loop becomes a prism of that loop over the same z range, in its first line's layer, after the
//! run's strips; a copy that does not close stays 2D.
//! Each placed corner and jamb detail (LAMB T491) then adds, per adjoining straight run with a z
//! range, strips from its lines parallel to that run (same boundary rule, each strip over the
//! overlap of its two boundaries' extents) and a prism per closed chain of its lines (whose lines
//! bound no strip), replacing nothing.
//! A placed vertical view (section) with an assembly block (LAMB T494) does the same for its laid
//! straight runs that are not within 1° of vertical — the floors and roofs it cuts; walls cut in
//! section are left to the plan — each strip and closed repeat copy (joists) extruded along the gaze
//! over the gaze range of the solid of either face's entity, which is then dropped, else over
//! `default_surface_depth_mm` centred on the cut plane, `Defaulted`. Corner and jamb details placed
//! in a section add nothing.
//! Each run those views lay or skip (a section's off-vertical ones only) also records a construction
//! state on its two face segments (LAMB T493): laid → resolved, or detail-conflicted with a width
//! conflict; skipped for conflicting details → detail-conflicted; skipped otherwise → massing. A
//! run's strips take its state; corner and jamb strips are resolved.

use std::collections::{BTreeMap, BTreeSet};

use cadcraft_doc::Drawing;
use lamb_core::derivation::assembly::{AssemblyLine, Band, FaceRef, lay_bands};
use lamb_core::derivation::projection::{
    plan_local_to_global_xy, vertical_basis, vertical_gaze_dir, vertical_local_to_global3, vertical_local_to_uz,
};
use lamb_core::derivation::read::{ProjectRead, ViewKind, ViewRef};
use lamb_core::derivation::resolve::ProvenanceKind;
use lamb_core::derivation::solids::{SolidForm, SynthesizedSolid, most_doubtful, prism_from_loop, solid_triangles};
use lamb_core::derivation::synthesis::SynthesisResult;
use lamb_core::derivation::wall_spaces::RunSides;
use lamb_core::derivation::walls::{RunCurve, WallRun};
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::geometry::{Point2, Point3};
use lamb_core::model::Id;

use crate::assembly::{RunAssembly, assembly_lines, corner_assembly, opening_assembly};
use crate::detailtags::graph_and_sides;
use crate::project::DrawingProject;
use crate::scene::ConstructionState;
use crate::schema::assembly_block_name;

/// One layer prism of one run.
pub(crate) struct LayerStrip {
    /// The run's two face entities, `faces[0]`'s first.
    pub sources: [Id; 2],
    pub provenance: ProvenanceKind,
    pub layer: String,
    /// Its run's state; a corner or jamb strip is resolved.
    pub state: ConstructionState,
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
    /// Each face segment `(entity, segment)` of every run laid or skipped, with its run's state (the
    /// worst, when two runs share a segment).
    pub states: BTreeMap<(Id, usize), ConstructionState>,
}

impl Layered {
    /// Records `ra`'s state on `run`'s face segments and stamps it on the strips from `first` on.
    fn record(&mut self, ra: &RunAssembly, run: &WallRun, first: usize) {
        let state = match (&ra.skipped, ra.conflict.is_some()) {
            (None, false) => ConstructionState::Resolved,
            (None, true) => ConstructionState::DetailConflicted,
            (Some(why), _) if why == "conflict" => ConstructionState::DetailConflicted,
            (Some(_), _) => ConstructionState::Massing,
        };
        for f in &run.faces {
            let slot = self.states.entry((f.from.entity_id, f.from.segment_index)).or_insert(state);
            *slot = (*slot).max(state);
        }
        for strip in self.strips.iter_mut().skip(first) {
            strip.state = state;
        }
    }
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
    for view in p.views().filter(|v| v.kind == ViewKind::Vertical) {
        if d.block(&assembly_block_name(view.id)).is_none() {
            continue;
        }
        let Some(host) = view.host_view_id().and_then(|h| p.view(h)) else { continue };
        if host.kind == ViewKind::Plan && host.placement.is_none() {
            continue;
        }
        section_strips(d, p, &view, &host, result, &mut out);
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
    let up = Point3 { x: 0.0, y: 0.0, z: 1.0 };
    push_extrusion(&base, up, (0.0, z.top - z.bottom), z.keys.map(|k| k.0), z.provenance, layer, out)
}

/// Pushes the prism of global loop `base` along `axis` over `range` in `layer`; false when nothing
/// was emitted.
fn push_extrusion(
    base: &[Point3],
    axis: Point3,
    range: (f64, f64),
    sources: [Id; 2],
    provenance: ProvenanceKind,
    layer: String,
    out: &mut Layered,
) -> bool {
    let faces = prism_from_loop(base, axis, range.0, range.1);
    let solid = SynthesizedSolid { entity_ids: sources.to_vec(), provenance, faces, form: SolidForm::Shell, layer_id: layer.clone() };
    let triangles = solid_triangles(&solid);
    if triangles.is_empty() {
        return false;
    }
    out.strips.push(LayerStrip { sources, provenance, layer, state: ConstructionState::Resolved, triangles });
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
    for ra in &runs {
        let Some(run) = graph.runs.get(ra.run) else { continue };
        let first = out.strips.len();
        'lay: {
            let (None, Some(face), Some(side), true) = (&ra.skipped, ra.face, sides.runs.get(ra.run), ra.bands > 0) else { break 'lay };
            let Some(bands) = ra.lines.get(..ra.bands) else { break 'lay };
            let loops = match run.centreline {
                RunCurve::Line { .. } => straight_loops(run, side, face, bands),
                RunCurve::Arc { radius, .. } => arc_loops(run, side, face, bands, radius),
            };
            let Some(loops) = loops else { break 'lay };
            let Some(z) = run_z(run, result) else { break 'lay };
            let mut emitted = false;
            for (ring, layer) in loops {
                emitted |= push_prism(view, &ring, layer, &z, out);
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
        out.record(ra, run, first);
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

/// A section's laid straight runs off vertical (floors and roofs), extruded along the gaze.
fn section_strips(d: &Drawing, p: &DrawingProject<'_>, view: &ViewRef<'_>, host: &ViewRef<'_>, result: &SynthesisResult, out: &mut Layered) {
    let Ok(runs) = assembly_lines(d, view.id) else { return };
    let Ok(basis) = vertical_basis(view, host) else { return };
    let (graph, sides) = graph_and_sides(p, view.id);
    let g = vertical_gaze_dir(basis.c_hat, false);
    let axis = Point3 { x: g.x, y: g.y, z: 0.0 };
    let tol = TOLERANCE.coincidence_mm;
    let sin_vertical = 1f64.to_radians().sin();
    let depth = p.defaults().default_surface_depth_mm;
    let lift = |ring: &[Point2]| ring.iter().map(|&q| vertical_local_to_global3(view, host, q)).collect::<Result<Vec<_>, _>>().ok();
    for ra in &runs {
        let Some(run) = graph.runs.get(ra.run) else { continue };
        let RunCurve::Line { from: c0, to: c1 } = run.centreline else { continue };
        let (a, b) = (vertical_local_to_uz(view, c0), vertical_local_to_uz(view, c1));
        let len = (b.u - a.u).hypot(b.z - a.z);
        if len <= tol || (b.u - a.u).abs() / len <= sin_vertical {
            continue;
        }
        let first = out.strips.len();
        'lay: {
            let (None, Some(face), Some(side), true) = (&ra.skipped, ra.face, sides.runs.get(ra.run), ra.bands > 0) else { break 'lay };
            let Some(bands) = ra.lines.get(..ra.bands) else { break 'lay };
            let Some(loops) = straight_loops(run, side, face, bands) else { break 'lay };
            let Some(origin) = lift(&[c0]).and_then(|o| o.first().copied()) else { break 'lay };
            let sources = [run.faces[0].from.entity_id, run.faces[1].from.entity_id];
            let solid = result.solids.iter().position(|s| s.entity_ids.iter().any(|e| sources.contains(e)));
            let along = |q: &Point3| (q.x - origin.x) * axis.x + (q.y - origin.y) * axis.y;
            let span = solid.and_then(|i| result.solids.get(i)).and_then(|s| {
                let v: Vec<f64> = s.faces.iter().flatten().map(along).filter(|t| t.is_finite()).collect();
                let (lo, hi) = (v.iter().copied().fold(f64::INFINITY, f64::min), v.iter().copied().fold(f64::NEG_INFINITY, f64::max));
                (hi - lo > tol).then_some(((lo, hi), s.provenance))
            });
            let (range, provenance) = span.unwrap_or(((-depth / 2.0, depth / 2.0), ProvenanceKind::Defaulted));
            let mut emitted = false;
            for (ring, layer) in loops {
                if let Some(base) = lift(&ring) {
                    emitted |= push_extrusion(&base, axis, range, sources, provenance, layer, out);
                }
            }
            let mut start = ra.bands;
            for &count in &ra.copies {
                let Some(copy) = ra.lines.get(start..start + count) else { break };
                start += count;
                if let (Some(base), Some(head)) = (closed_loop(copy, tol).and_then(|r| lift(&r)), copy.first()) {
                    push_extrusion(&base, axis, range, sources, provenance, head.layer.clone(), out);
                }
            }
            if let (true, Some(i), Some(_)) = (emitted, solid, span) {
                out.dropped_solids.insert(i);
            }
        }
        out.record(ra, run, first);
    }
}

/// A straight run's layer loops (plan-local) with their layers, or `None` when its frame is
/// degenerate or a band is an arc.
fn straight_loops(run: &WallRun, side: &RunSides, face: FaceRef, bands: &[AssemblyLine]) -> Option<Vec<(Vec<Point2>, String)>> {
    let fr = run_frame(run, side, face)?;
    if bands.iter().any(|l| l.arc.is_some()) {
        return None;
    }
    let mut all: Vec<Boundary<'_>> = vec![Boundary { offset: 0.0, line: None }];
    all.extend(bands.iter().map(|l| Boundary { offset: fr.off(midpoint(l)), line: Some(l) }));
    let mut out = Vec::new();
    for pair in merge(all).windows(2) {
        let [lo, hi] = pair else { continue };
        let Some(layer) = pair_layer(lo, hi) else { continue };
        let quad = match (lo.line, hi.line) {
            (Some(l), Some(h)) => vec![l.from, l.to, h.to, h.from],
            (Some(l), None) | (None, Some(l)) => vec![l.from, l.to, fr.foot(l.to), fr.foot(l.from)],
            (None, None) => continue,
        };
        out.push((quad, layer));
    }
    Some(out)
}

/// An arc line as `(center, radius, start angle, ccw sweep ≥ 0)`, direction-free.
fn arc_of(l: &AssemblyLine) -> Option<(Point2, f64, f64, f64)> {
    let a = l.arc.as_ref()?;
    let (s, e) = if a.ccw { (l.from, l.to) } else { (l.to, l.from) };
    let ang = |q: Point2| (q.y - a.center.y).atan2(q.x - a.center.x);
    let (a0, a1) = (ang(s), ang(e));
    Some((a.center, a.radius, a0, (a1 - a0).rem_euclid(std::f64::consts::TAU)))
}

/// An arc run's layer loops (plan-local) with their layers: each pair of consecutive concentric
/// boundaries gives the ring between them over the overlap of their angular spans. `None` when a
/// band is straight or the reference face is not an arc.
fn arc_loops(run: &WallRun, side: &RunSides, face: FaceRef, bands: &[AssemblyLine], centre_r: f64) -> Option<Vec<(Vec<Point2>, String)>> {
    use crate::asmview::{GHOST_MIN_SEGMENTS, GHOST_SEGMENTS_PER_TURN};
    use std::f64::consts::{PI, TAU};
    let reference = lay_bands(run, side, face, &[Band { offset: 0.0, layer: String::new() }]).ok()?;
    let (rc, rr, ra0, rsw) = arc_of(reference.first()?)?;
    if bands.iter().any(|l| l.arc.is_none()) {
        return None;
    }
    let toward = if centre_r >= rr { 1.0 } else { -1.0 };
    let mut all: Vec<Boundary<'_>> = vec![Boundary { offset: 0.0, line: None }];
    all.extend(bands.iter().filter_map(|l| arc_of(l).map(|(_, r, ..)| Boundary { offset: (r - rr) * toward, line: Some(l) })));
    // A boundary's (center, radius, start relative to the reference start, sweep).
    let geom = |b: &Boundary<'_>| -> Option<(Point2, f64, f64, f64)> {
        match b.line {
            None => Some((rc, rr, 0.0, rsw)),
            Some(l) => {
                let (c, r, a0, sw) = arc_of(l)?;
                Some((c, r, (a0 - ra0 + PI).rem_euclid(TAU) - PI, sw))
            }
        }
    };
    let tol = TOLERANCE.coincidence_mm;
    let mut out = Vec::new();
    for pair in merge(all).windows(2) {
        let [lo, hi] = pair else { continue };
        let Some(layer) = pair_layer(lo, hi) else { continue };
        let (Some(g0), Some(g1)) = (geom(lo), geom(hi)) else { continue };
        let (t0, t1) = (g0.2.max(g1.2), (g0.2 + g0.3).min(g1.2 + g1.3));
        if (t1 - t0) * g0.1.max(g1.1) <= tol {
            continue;
        }
        let n = (((t1 - t0) / TAU * GHOST_SEGMENTS_PER_TURN).ceil() as usize).max(GHOST_MIN_SEGMENTS);
        let pts = |(c, r, ..): (Point2, f64, f64, f64)| -> Vec<Point2> {
            (0..=n)
                .map(|i| {
                    let t = ra0 + t0 + (t1 - t0) * i as f64 / n as f64;
                    Point2 { x: c.x + r * t.cos(), y: c.y + r * t.sin() }
                })
                .collect()
        };
        let (outer, inner) = if g0.1 >= g1.1 { (g0, g1) } else { (g1, g0) };
        let mut ring = pts(outer);
        ring.extend(pts(inner).into_iter().rev());
        out.push((ring, layer));
    }
    Some(out)
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
pub(crate) mod tests {
    use cadcraft_doc::Handle;
    use cadcraft_engine::Session;
    use serde_json::json;

    use crate::assembly::tests::room;
    use crate::scene::{ConstructionState, SceneMeshKind, pick, scene};
    use lamb_core::derivation::read::ProjectRead;
    use lamb_core::model::Id;

    type R = Result<(), Box<dyn std::error::Error>>;
    type T<X> = Result<X, Box<dyn std::error::Error>>;

    fn placed(tagged: bool) -> T<(Session, Id)> {
        let (mut s, plan) = room("exterior_face", tagged)?;
        place(&mut s, plan)?;
        Ok((s, plan))
    }

    pub(crate) fn plines(s: &Session, plan: Id) -> T<Vec<Handle>> {
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

        assert!(sc.meshes.iter().all(|m| m.construction.is_none()), "no assembly block, no construction state");

        let (mut s, plan) = placed(false)?;
        let before = scene(s.doc()?)?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let mut after = scene(s.doc()?)?;
        assert!(after.meshes.iter().all(|m| m.kind != SceneMeshKind::Layer));
        // Every run untyped: all of it is massing still to detail.
        assert!(after.meshes.iter().all(|m| m.construction == Some(ConstructionState::Massing)));
        after.meshes.iter_mut().for_each(|m| m.construction = None);
        assert_eq!(before, after);
        Ok(())
    }

    fn states(sc: &crate::scene::Scene, kind: SceneMeshKind) -> Vec<Option<ConstructionState>> {
        sc.meshes.iter().filter(|m| m.kind == kind).map(|m| m.construction).collect()
    }

    #[test]
    fn a_fully_laid_room_is_resolved_with_no_massing_left() -> R {
        let (mut s, plan) = placed(true)?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let sc = scene(s.doc()?)?;
        let layers = states(&sc, SceneMeshKind::Layer);
        assert_eq!(layers.len(), 12);
        assert!(layers.iter().all(|st| *st == Some(ConstructionState::Resolved)), "{layers:?}");
        assert!(sc.meshes.iter().all(|m| m.kind == SceneMeshKind::Layer), "no massing left");
        Ok(())
    }

    #[test]
    fn an_untyped_run_leaves_its_room_massing_partly_layered() -> R {
        // The top wall 300 thick, a class of its own with no tag: untyped.
        let (mut s, plan) = crate::assembly::tests::room("exterior_face", true)?;
        let old: Vec<String> = plines(&s, plan)?.iter().map(|h| h.hex()).collect();
        s.execute("erase", &json!({ "handles": old }))?;
        s.execute("layer.current", &json!({ "name": "WALL-EXT" }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [5000, 0], [5000, 4000], [0, 4000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [4800, 200], [4800, 3700], [200, 3700]], "closed": true }))?;
        place(&mut s, plan)?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let skipped: Vec<_> = crate::assembly::assembly_lines(s.doc()?, plan)?.into_iter().filter_map(|r| r.skipped).collect();
        assert_eq!(skipped, ["untyped"]);
        let sc = scene(s.doc()?)?;
        let layers = states(&sc, SceneMeshKind::Layer);
        assert_eq!(layers.len(), 9, "three laid runs");
        assert!(layers.iter().all(|st| *st == Some(ConstructionState::Resolved)), "{layers:?}");
        let solids = states(&sc, SceneMeshKind::Solid);
        assert_eq!(solids, [Some(ConstructionState::Massing); 2], "both plines survive partly layered");
        assert!(states(&sc, SceneMeshKind::Surface).iter().all(|st| *st == Some(ConstructionState::Massing)));
        Ok(())
    }

    #[test]
    fn conflicting_details_and_width_conflicts_are_detail_conflicted() -> R {
        // A second detail D2 tagged on the bottom run beside D1: that run is skipped as "conflict",
        // and with two details in the class the rest are untyped.
        let (mut s, plan) = crate::assembly::tests::room("exterior_face", true)?;
        let d2 = Id(s.execute("newdetail", &json!({ "name": "D2" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": d2.0 }))?;
        let r = drawn(&mut s, "line", json!({ "points": [[0, 0], [1000, 0]] }))?;
        s.execute("detailref", &json!({ "handle": r.hex(), "kind": "exterior_face" }))?;
        s.execute("layer.current", &json!({ "name": "GYP" }))?;
        drawn(&mut s, "line", json!({ "points": [[0, 200], [1000, 200]] }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.current", &json!({ "name": "TAGS" }))?;
        s.execute("detailtag", &json!({ "detailViewId": d2.0, "tip": {"x": 1000.0, "y": 0.0}, "bubble": {"x": 500.0, "y": -1000.0} }))?;
        place(&mut s, plan)?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let mut skipped: Vec<_> = crate::assembly::assembly_lines(s.doc()?, plan)?.into_iter().filter_map(|r| r.skipped).collect();
        skipped.sort();
        assert_eq!(skipped, ["conflict", "untyped", "untyped", "untyped"]);
        let sc = scene(s.doc()?)?;
        assert!(states(&sc, SceneMeshKind::Layer).is_empty());
        assert_eq!(states(&sc, SceneMeshKind::Solid), [Some(ConstructionState::DetailConflicted); 2]);

        // A 210 wall under a 200 detail: every run laid with a width conflict.
        let (mut s, plan) = crate::assembly::tests::room_with("exterior_face", true, 210.0)?;
        place(&mut s, plan)?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert!(crate::assembly::assembly_lines(s.doc()?, plan)?.iter().all(|r| r.skipped.is_none() && r.conflict.is_some()));
        let sc = scene(s.doc()?)?;
        let layers = states(&sc, SceneMeshKind::Layer);
        assert_eq!(layers.len(), 12);
        assert!(layers.iter().all(|st| *st == Some(ConstructionState::DetailConflicted)), "{layers:?}");
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
        add_studs(&mut s, plan, open)?;
        Ok((s, plan))
    }

    /// Adds [`studded`]'s STUD repeat to `plan`'s detail D1, then runs ASSEMBLYPLAN.
    fn add_studs(s: &mut Session, plan: Id, open: bool) -> R {
        let detail = {
            let p = crate::project::DrawingProject::new(s.doc()?)?;
            p.views().find(|v| v.kind == lamb_core::derivation::read::ViewKind::Detail).ok_or("no detail")?.id
        };
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        s.execute("layer.new", &json!({ "name": "STUD", "current": true }))?;
        let stud = if open {
            drawn(s, "line", json!({ "points": [[100, 50], [100, 200]] }))?
        } else {
            drawn(s, "pline", json!({ "vertices": [[100, 50], [138, 50], [138, 200], [100, 200]], "closed": true }))?
        };
        let pitch = drawn(s, "dimaligned", json!({ "p1": [100, 0], "p2": [500, 0], "at": [300, -300] }))?;
        s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [stud.hex()] }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        Ok(())
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

    /// Places `plan` on a new level L1 at the origin, unless a section drawn in it already placed it
    /// (LAMB T507).
    fn place(s: &mut Session, plan: Id) -> R {
        if crate::schema::read_document(s.doc()?)?.views.iter().any(|v| v.id() == plan && v.placement().is_some()) {
            return Ok(());
        }
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

    /// [`d_room`](crate::assembly::tests::d_room) placed, its two plines redrawn clockwise when
    /// `cw`, with [`add_studs`]'s closed STUD repeat when `studs`, then ASSEMBLYPLAN.
    fn arc_room(cw: bool, studs: bool) -> T<(Session, Id)> {
        let (mut s, plan) = crate::assembly::tests::d_room()?;
        if cw {
            let old: Vec<String> = plines(&s, plan)?.iter().map(|h| h.hex()).collect();
            s.execute("erase", &json!({ "handles": old }))?;
            s.execute("layer.current", &json!({ "name": "WALL-EXT" }))?;
            let v = |x: f64, y: f64, bulge: f64| json!({ "p": [x, y], "bulge": bulge });
            for (lo, hi, x0) in [(0.0, 2200.0, 0.0), (200.0, 2000.0, 200.0)] {
                let vs = [v(x0, hi, 0.0), v(3000.0, hi, -1.0), v(3000.0, lo, 0.0), v(x0, lo, 0.0)];
                s.execute("pline", &json!({ "vertices": vs, "closed": true }))?;
            }
        }
        place(&mut s, plan)?;
        if studs {
            add_studs(&mut s, plan, false)?;
        } else {
            s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        }
        Ok((s, plan))
    }

    /// The scene's Layer meshes on the arc run (every vertex at x ≥ 3000, some past 3100).
    fn arc_meshes(sc: &crate::scene::Scene) -> Vec<&crate::scene::SceneMesh> {
        sc.meshes.iter().filter(|m| m.kind == SceneMeshKind::Layer && range(sc, m, 0).0 > 2999.99 && range(sc, m, 0).1 > 3100.0).collect()
    }

    /// `m`'s vertex radii about the arc's centre (3000, 1100): (min, max).
    fn radii(sc: &crate::scene::Scene, m: &crate::scene::SceneMesh) -> (f64, f64) {
        let r = m.positions.iter().map(|p| (sc.origin[0] + f64::from(p[0]) - 3000.0).hypot(sc.origin[1] + f64::from(p[1]) - 1100.0));
        (r.clone().fold(f64::INFINITY, f64::min), r.fold(f64::NEG_INFINITY, f64::max))
    }

    #[test]
    fn a_laid_arc_run_shows_as_curved_layer_slabs_at_the_band_radii() -> R {
        let (s, _) = arc_room(false, false)?;
        let sc = scene(s.doc()?)?;
        let arcs = arc_meshes(&sc);
        let mut got: Vec<(String, f64, f64)> = arcs.iter().map(|m| (m.layer.clone(), radii(&sc, m).0, radii(&sc, m).1)).collect();
        got.sort_by(|a, b| a.1.total_cmp(&b.1));
        assert_eq!(got.len(), 3, "{got:?}");
        // Reference (exterior) face r 1100; CLAD −20 outward, GYP 50 and 200 toward the centre.
        let want = [("GYP", 900.0, 1050.0), ("GYP", 1050.0, 1100.0), ("CLAD", 1100.0, 1120.0)];
        for ((l, lo, hi), (wl, wlo, whi)) in got.iter().zip(want) {
            assert_eq!(l, wl);
            assert!((lo - wlo).abs() < 0.05 && (hi - whi).abs() < 0.05, "{got:?}");
        }
        // Smooth: a semicircle sampled at 32 segments per arc, so 33 points per side.
        let clad = arcs.iter().find(|m| m.layer == "CLAD").ok_or("no CLAD")?;
        let distinct: std::collections::BTreeSet<_> =
            clad.positions.iter().map(|p| ((p[0] * 10.0).round() as i64, (p[1] * 10.0).round() as i64)).collect();
        assert_eq!(distinct.len(), 66, "two arcs of 33 points");
        // Every run is layered, so both plines' solids — arc segment included — are replaced.
        assert!(sc.meshes.iter().all(|m| m.kind != SceneMeshKind::Solid));
        assert_eq!(sc.meshes.iter().filter(|m| m.kind == SceneMeshKind::Layer).count(), 12);
        Ok(())
    }

    #[test]
    fn a_clockwise_arc_gives_the_same_slabs() -> R {
        let key = |cw: bool| -> T<Vec<(String, std::collections::BTreeSet<(i64, i64, i64)>)>> {
            let (s, _) = arc_room(cw, false)?;
            let sc = scene(s.doc()?)?;
            let mut v: Vec<_> = arc_meshes(&sc)
                .iter()
                .map(|m| {
                    let pts =
                        m.positions.iter().map(|p| (0..3).map(|k| ((sc.origin[k] + f64::from(p[k])) * 10.0).round() as i64).collect::<Vec<_>>());
                    (m.layer.clone(), pts.map(|q| (q[0], q[1], q[2])).collect())
                })
                .collect();
            v.sort();
            Ok(v)
        };
        let (ccw, cw) = (key(false)?, key(true)?);
        assert_eq!(ccw.len(), 3);
        assert_eq!(ccw, cw);
        Ok(())
    }

    #[test]
    fn closed_stud_copies_on_an_arc_run_become_prisms() -> R {
        let (s, plan) = arc_room(false, true)?;
        let d = s.doc()?;
        let p = crate::project::DrawingProject::new(d)?;
        let (graph, _) = crate::detailtags::graph_and_sides(&p, plan);
        let runs = crate::assembly::assembly_lines(d, plan)?;
        let arc = runs
            .iter()
            .find(|r| graph.runs.get(r.run).is_some_and(|w| matches!(w.centreline, lamb_core::derivation::walls::RunCurve::Arc { .. })))
            .ok_or("no arc run")?;
        assert!(!arc.copies.is_empty(), "copies laid along the arc");
        let sc = scene(d)?;
        let studs: Vec<_> = arc_meshes(&sc).into_iter().filter(|m| m.layer == "STUD").collect();
        assert_eq!(studs.len(), arc.copies.len());
        for m in &studs {
            let (lo, hi) = radii(&sc, m);
            assert!(lo > 900.0 - 2.0 && hi < 1050.0 + 2.0, "{lo}..{hi}");
        }
        Ok(())
    }

    /// The section's gaze direction in global XY.
    fn gaze(s: &Session, sec: Id) -> T<[f64; 2]> {
        let p = crate::project::DrawingProject::new(s.doc()?)?;
        let v = p.view(sec).ok_or("no section")?;
        let host = p.view(v.host_view_id().ok_or("no host")?).ok_or("no host view")?;
        let b = lamb_core::derivation::projection::vertical_basis(&v, &host)?;
        let g = lamb_core::derivation::projection::vertical_gaze_dir(b.c_hat, false);
        Ok([g.x, g.y])
    }

    /// `m`'s vertices projected on `g`, relative to `at` (global XY): (min, max).
    fn along(sc: &crate::scene::Scene, m: &crate::scene::SceneMesh, g: [f64; 2], at: [f64; 2]) -> (f64, f64) {
        let t = m.positions.iter().map(|p| (sc.origin[0] + f64::from(p[0]) - at[0]) * g[0] + (sc.origin[1] + f64::from(p[1]) - at[1]) * g[1]);
        (t.clone().fold(f64::INFINITY, f64::min), t.fold(f64::NEG_INFINITY, f64::max))
    }

    /// The global XY of section-local point `q`.
    fn global_of(s: &Session, sec: Id, q: [f64; 2]) -> T<[f64; 2]> {
        let p = crate::project::DrawingProject::new(s.doc()?)?;
        let v = p.view(sec).ok_or("no section")?;
        let host = p.view(v.host_view_id().ok_or("no host")?).ok_or("no host view")?;
        let g = lamb_core::derivation::projection::vertical_local_to_global3(&v, &host, lamb_core::geometry::Point2 { x: q[0], y: q[1] })?;
        Ok([g.x, g.y])
    }

    fn floor_handles(s: &Session, sec: Id) -> T<Vec<Handle>> {
        let d = s.doc()?;
        let b = d.block(&crate::schema::view_block_name(sec)).ok_or("no view block")?;
        Ok(b.entities.iter().filter(|e| e.common.layer == "FLOOR").map(|e| e.handle).collect())
    }

    #[test]
    fn a_section_s_laid_floor_shows_as_slabs_and_joists_over_the_default_depth() -> R {
        let (mut s, sec, _) = crate::assembly::tests::joist_section()?;
        let floor = crate::assembly::tests::floor_run(s.doc()?, sec)?;
        let copies = floor.copies.len();
        assert!(copies > 0);
        s.execute("assemblyplan", &json!({ "viewId": sec.0 }))?;
        let sc = scene(s.doc()?)?;
        let layers: Vec<_> = sc.meshes.iter().filter(|m| m.kind == SceneMeshKind::Layer).collect();
        let fl = floor_handles(&s, sec)?;
        assert_eq!(fl.len(), 2);
        assert!(!layers.is_empty());
        // Every Layer mesh is the floor's: the cut wall run (z 0..3000) gets none.
        assert!(layers.iter().all(|m| m.sources.iter().all(|h| fl.contains(h))), "only floor meshes");
        // The 300 detail in a 250 floor: laid with a width conflict, so every strip is flagged.
        assert!(floor.conflict.is_some());
        assert!(layers.iter().all(|m| m.construction == Some(ConstructionState::DetailConflicted)));
        let (g, at) = (gaze(&s, sec)?, global_of(&s, sec, [300.0, 1000.0])?);
        let depth = crate::project::DrawingProject::new(s.doc()?)?.defaults().default_surface_depth_mm;
        for m in &layers {
            let (lo, hi) = along(&sc, m, g, at);
            assert!((lo + depth / 2.0).abs() < 0.01 && (hi - depth / 2.0).abs() < 0.01, "{lo}..{hi}");
            assert_eq!(m.provenance, lamb_core::derivation::resolve::ProvenanceKind::Defaulted);
        }
        let mut bands: Vec<(String, f64, f64)> = layers
            .iter()
            .filter(|m| m.layer != "STUD")
            .map(|m| {
                let z = range(&sc, m, 2);
                (m.layer.clone(), (z.0 * 100.0).round() / 100.0, (z.1 * 100.0).round() / 100.0)
            })
            .collect();
        bands.sort_by(|a, b| a.1.total_cmp(&b.1));
        // Exterior face below (z 1000); GYP bands at offsets 50 and 300 toward the floor's top.
        assert_eq!(bands, [("GYP".to_string(), 1000.0, 1050.0), ("GYP".to_string(), 1050.0, 1300.0)]);
        // One prism per closed joist copy: 38 along u, z 1050..1200.
        let joists: Vec<_> = layers.iter().filter(|m| m.layer == "STUD").collect();
        assert_eq!(joists.len(), copies);
        for m in &joists {
            let z = range(&sc, m, 2);
            assert!((z.0 - 1050.0).abs() < 0.01 && (z.1 - 1200.0).abs() < 0.01, "{z:?}");
        }
        Ok(())
    }

    #[test]
    fn a_closed_section_floor_s_slabs_take_its_solid_s_gaze_range_and_replace_it() -> R {
        let (mut s, sec) = crate::assembly::tests::section_fixture(150.0, Some((-1000.0, 1500.0)), |s| {
            s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
            s.execute("pline", &json!({ "vertices": [[0, 0], [0, 3000]] }))?;
            s.execute("pline", &json!({ "vertices": [[300, 0], [300, 3000]] }))?;
            s.execute("layer.new", &json!({ "name": "FLOOR", "current": true }))?;
            s.execute("pline", &json!({ "vertices": [[300, 1000], [4000, 1000], [4000, 1250], [300, 1250]], "closed": true }))?;
            Ok(())
        })?;
        s.execute("outsidepoint", &json!({ "point": {"x": 2000.0, "y": 500.0} }))?;
        let sd = {
            let p = crate::project::DrawingProject::new(s.doc()?)?;
            p.views().find(|v| v.kind == lamb_core::derivation::read::ViewKind::Detail).ok_or("no detail")?.id
        };
        s.execute("detailtag", &json!({ "detailViewId": sd.0, "tip": {"x": 2000.0, "y": 1000.0}, "bubble": {"x": 2500.0, "y": 500.0} }))?;
        let plan = {
            let p = crate::project::DrawingProject::new(s.doc()?)?;
            p.views().find(|v| v.kind == lamb_core::derivation::read::ViewKind::Plan).ok_or("no plan")?.id
        };
        place(&mut s, plan)?;
        let before = scene(s.doc()?)?;
        let fl = floor_handles(&s, sec)?;
        let solid =
            before.meshes.iter().find(|m| m.kind == SceneMeshKind::Solid && m.sources.iter().any(|h| fl.contains(h))).ok_or("no floor solid")?;
        let (g, at) = (gaze(&s, sec)?, global_of(&s, sec, [300.0, 1000.0])?);
        let want = along(&before, solid, g, at);
        assert!(want.1 - want.0 > 1.0);

        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.execute("assemblyplan", &json!({ "viewId": sec.0 }))?;
        let sc = scene(s.doc()?)?;
        let layers: Vec<_> = sc.meshes.iter().filter(|m| m.kind == SceneMeshKind::Layer && m.sources.iter().any(|h| fl.contains(h))).collect();
        assert!(!layers.is_empty(), "the closed floor is laid");
        for m in &layers {
            let (lo, hi) = along(&sc, m, g, at);
            assert!((lo - want.0).abs() < 0.01 && (hi - want.1).abs() < 0.01, "{lo}..{hi} vs {want:?}");
        }
        assert!(!sc.meshes.iter().any(|m| m.kind == SceneMeshKind::Solid && m.sources.iter().any(|h| fl.contains(h))), "the floor solid is gone");
        Ok(())
    }
}

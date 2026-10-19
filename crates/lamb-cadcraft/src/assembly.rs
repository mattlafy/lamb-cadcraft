//! Assembly lines for a plan or section view (LAMB's `docs/07-architecture/cadcraft-integration.md` §10
//! "Generation", steps 1, 2 and 4; LAMB T369): for each wall run whose edge
//! detail resolves ([`resolve_view_details`]) to a detail with a face reference
//! ([`detail_reference`]), that detail's continuous bands laid along the run on its matching face
//! (lamb-core's [`detail_bands`] and [`lay_bands`]). Computed on demand by [`assembly_lines`];
//! `assemblyplan {viewId}` ([`write_assembly_block`], LAMB T371) writes them as LINEs into the
//! block [`assembly_block_name`]`(plan)`, replacing its contents, in one undo step — an arc run's
//! bands (LAMB T389) as ARCs, written counter-clockwise (a clockwise band with its ends swapped).
//! Each laid run
//! also carries its width conflict (lamb-core's [`width_conflict`], LAMB T375): reported by
//! ASSEMBLYPLAN and by the read-only `wallconflicts {viewId}`. Each of the detail's repeats
//! ([`detail_repeats`], LAMB T380) is arrayed at its pitch along the run too (lamb-core's
//! [`lay_repeat`]), every straight segment of its members kept on the member's layer; on an arc
//! run (LAMB T392) the copies are spaced by arc length along the reference face and each segment
//! is written as a straight chord. Once a plan
//! has its block, [`assembly_observer`] (LAMB T381) keeps it equal to [`assembly_lines`] after every
//! command, inside that command's undo step; a plan without the block gets none. A plan listed in
//! the document's `mitre_plans` (`assemblyplan {viewId, mitre}`, LAMB T385; §10 step 3) has every
//! band end at a two-arm junction with no corner detail (`Untyped` or `Conflict`) moved onto that
//! junction's bisector (lamb-core's [`junction_bisector`], [`mitre_end`]), and at such a T junction
//! the branch run's band ends butted onto the through run's face on its side (lamb-core's
//! [`tee_butt`], LAMB T472); repeats are not mitred.
//! The project's stud grid ([`crate::studgrid`], LAMB T458, T463) lays each straight run's repeat whose
//! pitch is near the grid's on that grid's lines — every plan run, and a section's runs along its cut line.
//! At each two-arm junction a corner tag resolves to (§10 step 3, LAMB T461), [`corner_assembly`]
//! places the corner detail once (lamb-core's [`place_corner`]); the block holds the run lines,
//! then the corner lines in junction order. A placed corner owns the region around its junction
//! (LAMB T462): on each straight arm's run, band lines are clipped to start at the corner's reach
//! (lamb-core's [`corner_reach`], [`trim_to_reach`]; dropped when wholly inside) and every repeat
//! copy with a line end inside the reach is dropped whole. Before that trim, each stud-move point
//! ([`crate::studgrid`], LAMB T464) shifts its run's nearest repeat copy along the run onto itself.
//! Not the AI-line sweep in [`crate::sweeps`].

use std::sync::Arc;

use cadcraft_doc::{Arc as ArcEntity, Block, Common, Drawing, Entity, EntityKind, Insert, Line, Space};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result as EngineResult, Session, Step};
use cadcraft_geom::{Vec2, Vec3};
use lamb_core::derivation::assembly::{
    AssemblyLine, FaceRef, WidthConflict, arm_direction, corner_reach, detail_bands, detail_frame, junction_bisector, lay_bands, lay_repeat,
    mitre_end, place_corner, tee_butt, trim_to_reach, width_conflict,
};
use lamb_core::derivation::detail_tags::RunDetail;
use lamb_core::derivation::issues::{Issue, IssueKind};
use lamb_core::derivation::walls::{RunCurve, WallRun};
use lamb_core::geometry::Point2;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::AnyView;
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::commands::{id_value, other};
use crate::convert::to_polyline;
use crate::detailref::{DetailRefKind, detail_reference};
use crate::detailtags::{edited_wall_view, graph_and_sides, resolve_view_details};
use crate::project::DrawingProject;
use crate::repeats::{detail_repeats, repeat_members};
use crate::schema::{assembly_block_name, read_document, view_block_name, write_document};

/// One wall run's assembly: the detail it resolved to (if any), the laid lines, and why it was
/// skipped when `lines` is empty for a reason.
#[derive(Clone, Debug, PartialEq)]
pub struct RunAssembly {
    /// Index into the plan's wall-graph runs.
    pub run: usize,
    pub detail: Option<Id>,
    pub lines: Vec<AssemblyLine>,
    /// `lines[..bands]` are the continuous bands; the rest are repeat copies.
    pub bands: usize,
    /// The line count of each repeat copy, in order, after the bands: `bands + copies.iter().sum()
    /// == lines.len()`.
    pub copies: Vec<usize>,
    pub skipped: Option<String>,
    /// The detail is thicker or thinner than the drawn wall; `None` when they agree or the run was
    /// skipped.
    pub conflict: Option<WidthConflict>,
}

/// Every entity in `detail`'s block except `reference` and repeat members ([`crate::repeats`]) that
/// is one straight open segment, with its layer.
fn detail_lines(d: &Drawing, detail: Id, reference: cadcraft_doc::Handle) -> Result<Vec<(Point2, Point2, String)>, String> {
    let Some(block) = d.block(&view_block_name(detail)) else { return Ok(Vec::new()) };
    let repeated = repeat_members(d, detail)?;
    Ok(block
        .entities
        .iter()
        .filter(|e| e.handle != reference && !repeated.contains(&e.handle))
        .filter_map(|e| {
            let poly = to_polyline(e).ok()?.polyline;
            match poly.segments.as_slice() {
                [seg] if !poly.closed && seg.bulge == 0.0 => Some((poly.start, seg.to, e.common.layer.clone())),
                _ => None,
            }
        })
        .collect())
}

/// Every straight segment of the `members` of `detail`'s block (each through [`to_polyline`], open
/// or closed; bulged segments and entities it refuses are skipped), with the member's layer.
fn member_segments(d: &Drawing, detail: Id, members: &[cadcraft_doc::Handle]) -> Vec<(Point2, Point2, String)> {
    let Some(block) = d.block(&view_block_name(detail)) else { return Vec::new() };
    let mut out = Vec::new();
    for e in block.entities.iter().filter(|e| members.contains(&e.handle)) {
        let Ok(c) = to_polyline(e) else { continue };
        let poly = c.polyline;
        let mut at = poly.start;
        for seg in &poly.segments {
            if seg.bulge == 0.0 {
                out.push((at, seg.to, e.common.layer.clone()));
            }
            at = seg.to;
        }
        if poly.closed && at != poly.start {
            out.push((at, poly.start, e.common.layer.clone()));
        }
    }
    out
}

/// One run's lines from `detail` (bands, then each repeat's copies), how many of them are bands, the
/// line count of each copy, and its width conflict, or why not.
fn run_lines(
    d: &Drawing,
    view: Id,
    detail: Id,
    run: &lamb_core::derivation::walls::WallRun,
    sides: &lamb_core::derivation::wall_spaces::RunSides,
) -> Result<(Vec<AssemblyLine>, usize, Vec<usize>, Option<WidthConflict>), String> {
    let r = detail_reference(d, detail)?.ok_or("detail has no reference line")?;
    let face = match r.kind {
        DetailRefKind::ExteriorFace => FaceRef::Exterior,
        DetailRefKind::InteriorFace => FaceRef::Interior,
        DetailRefKind::StructuralCentre | DetailRefKind::Grid => return Err("reference kind not laid yet".into()),
    };
    let lines = detail_lines(d, detail, r.handle)?;
    let bands = detail_bands((r.from, r.to), &lines)?;
    let frame = detail_frame((r.from, r.to), &lines)?;
    let mut laid = lay_bands(run, sides, face, &bands)?;
    let n = laid.len();
    let grid = crate::studgrid::run_grid(d, view, run);
    let mut copies = Vec::new();
    for rep in detail_repeats(d, detail)? {
        let members = member_segments(d, detail, &rep.members);
        let copy = lay_repeat(run, sides, face, &frame, &members, rep.pitch, grid.as_ref())?;
        if !members.is_empty() {
            copies.extend(std::iter::repeat_n(members.len(), copy.len() / members.len()));
        }
        laid.extend(copy);
    }
    move_studs(&mut laid[n..], &copies, run, &crate::studgrid::stud_moves(d, view, run));
    Ok((laid, n, copies, width_conflict(run, face, &bands)))
}

/// LAMB T464: each stud-move point, in order, shifts the not-yet-moved repeat copy whose centre
/// along straight `run` is nearest it (ties → the lower index) so that its centre sits at the point.
/// `lines` are the copies' lines, `copies` their line counts. Arc runs or no copies: inert.
fn move_studs(lines: &mut [AssemblyLine], copies: &[usize], run: &WallRun, moves: &[Point2]) {
    let RunCurve::Line { from, to } = run.centreline else { return };
    let len = (to.x - from.x).hypot(to.y - from.y);
    if len <= 0.0 || moves.is_empty() {
        return;
    }
    let u = Point2 { x: (to.x - from.x) / len, y: (to.y - from.y) / len };
    let along = |p: Point2| (p.x - from.x) * u.x + (p.y - from.y) * u.y;
    let mut ranges = Vec::with_capacity(copies.len());
    let mut at = 0;
    for &c in copies {
        ranges.push((at.min(lines.len()), (at + c).min(lines.len())));
        at += c;
    }
    let centre = |ls: &[AssemblyLine]| {
        let ts = ls.iter().flat_map(|l| [along(l.from), along(l.to)]);
        (ts.clone().fold(f64::INFINITY, f64::min) + ts.fold(f64::NEG_INFINITY, f64::max)) / 2.0
    };
    let mut moved = vec![false; ranges.len()];
    for &q in moves {
        let t_p = along(q);
        let best = ranges
            .iter()
            .enumerate()
            .filter(|&(i, &(a, b))| !moved[i] && b > a)
            .map(|(i, &(a, b))| (i, centre(&lines[a..b])))
            .min_by(|x, y| (x.1 - t_p).abs().total_cmp(&(y.1 - t_p).abs()).then(x.0.cmp(&y.0)));
        let Some((i, t_c)) = best else { continue };
        moved[i] = true;
        let (dx, dy) = ((t_p - t_c) * u.x, (t_p - t_c) * u.y);
        let (a, b) = ranges[i];
        for l in &mut lines[a..b] {
            l.from = Point2 { x: l.from.x + dx, y: l.from.y + dy };
            l.to = Point2 { x: l.to.x + dx, y: l.to.y + dy };
        }
    }
}

/// Every run of `plan`'s wall graph, in graph order, with its detail's bands laid along it — mitred
/// at undetailed two-arm junctions, and butted at undetailed T junctions, when `plan` is in the document's `mitre_plans`.
pub fn assembly_lines(d: &Drawing, plan: Id) -> Result<Vec<RunAssembly>, String> {
    let (details, corners) = resolve_view_details(d, plan)?;
    let mitre = read_document(d).map_err(|e| e.to_string())?.mitre_plans.contains(&plan);
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let (graph, sides) = graph_and_sides(&p, plan);
    let mut runs: Vec<RunAssembly> = graph
        .runs
        .iter()
        .zip(&sides.runs)
        .zip(&details)
        .enumerate()
        .map(|(i, ((run, side), resolved))| {
            let skip = |why: &str, detail: Option<Id>| RunAssembly {
                run: i,
                detail,
                lines: Vec::new(),
                bands: 0,
                copies: Vec::new(),
                skipped: Some(why.into()),
                conflict: None,
            };
            let id = match resolved {
                RunDetail::Tagged(id) | RunDetail::Spread(id) | RunDetail::Gap(id) => id,
                RunDetail::Conflict => return skip("conflict", None),
                RunDetail::Untyped => return skip("untyped", None),
            };
            let Ok(detail) = id.parse::<u64>().map(Id) else { return skip("detail id is not a number", None) };
            match run_lines(d, plan, detail, run, side) {
                Ok((lines, bands, copies, conflict)) => RunAssembly { run: i, detail: Some(detail), lines, bands, copies, skipped: None, conflict },
                Err(e) => skip(&e, Some(detail)),
            }
        })
        .collect();
    if mitre {
        for (j, corner) in graph.junctions.iter().zip(&corners) {
            if !matches!(corner, RunDetail::Untyped | RunDetail::Conflict) {
                continue;
            }
            if let Some(bisector) = junction_bisector(j) {
                for arm in &j.arms {
                    let Some(r) = runs.get_mut(arm.run).filter(|r| r.skipped.is_none()) else { continue };
                    let n = r.bands;
                    for l in &mut r.lines[..n] {
                        mitre_end(l, arm.end, j.point, bisector);
                    }
                }
            } else if let Some((b, point, dir)) = tee_butt(j, &graph.runs) {
                let arm = &j.arms[b];
                let Some(r) = runs.get_mut(arm.run).filter(|r| r.skipped.is_none()) else { continue };
                let n = r.bands;
                for l in &mut r.lines[..n] {
                    mitre_end(l, arm.end, point, dir);
                }
            }
        }
    }
    for c in corner_assembly(d, plan)?.iter().filter(|c| c.skipped.is_none()) {
        let Some(j) = graph.junctions.get(c.junction) else { continue };
        for (i, reach) in corner_reach(j, &graph.runs, &c.lines).into_iter().enumerate() {
            let Some(u) = arm_direction(j, &graph.runs, i).filter(|_| reach > 0.0) else { continue };
            let Some(r) = runs.get_mut(j.arms[i].run).filter(|r| r.skipped.is_none()) else { continue };
            trim_run(r, j.point, u, reach);
        }
    }
    Ok(runs)
}

/// Clips `r`'s band lines to `reach` along `u` from `node` ([`trim_to_reach`], dropping those wholly
/// inside) and drops every repeat copy with a line end inside it.
fn trim_run(r: &mut RunAssembly, node: Point2, u: Point2, reach: f64) {
    let limit = reach - TOLERANCE.coincidence_mm;
    let inside = |p: Point2| (p.x - node.x) * u.x + (p.y - node.y) * u.y < limit;
    let mut rest = r.lines.split_off(r.bands);
    r.lines.retain_mut(|l| trim_to_reach(l, node, u, reach));
    r.bands = r.lines.len();
    let mut copies = Vec::new();
    for n in std::mem::take(&mut r.copies) {
        let copy: Vec<AssemblyLine> = rest.drain(..n.min(rest.len())).collect();
        if !copy.iter().any(|l| inside(l.from) || inside(l.to)) {
            r.lines.extend(copy);
            copies.push(n);
        }
    }
    r.lines.extend(rest);
    r.copies = copies;
}

/// One junction's placed corner detail, or why it was not placed.
#[derive(Clone, Debug, PartialEq)]
pub struct CornerAssembly {
    /// Index into the view's wall-graph junctions.
    pub junction: usize,
    pub detail: Option<Id>,
    pub lines: Vec<AssemblyLine>,
    pub skipped: Option<String>,
}

/// One corner detail's lines placed at junction `j`, or why not.
fn corner_lines(
    d: &Drawing,
    detail: Id,
    j: &lamb_core::derivation::wall_junctions::Junction,
    runs: &[WallRun],
    sides: &[lamb_core::derivation::wall_spaces::RunSides],
) -> Result<Vec<AssemblyLine>, String> {
    let r = detail_reference(d, detail)?.ok_or("detail has no reference line")?;
    if !matches!(r.kind, DetailRefKind::ExteriorFace) {
        return Err("corner reference kind not placed yet".into());
    }
    let Some(block) = d.block(&view_block_name(detail)) else { return Err("detail has no block".into()) };
    let others: Vec<cadcraft_doc::Handle> = block.entities.iter().map(|e| e.handle).filter(|h| *h != r.handle).collect();
    place_corner(j, runs, sides, (r.from, r.to), &member_segments(d, detail, &others))
}

/// Every junction of `view`'s wall graph whose corner detail resolves (`Tagged`, `Spread` or
/// `Gap`), in junction order, with that detail placed there ([`place_corner`]); a failure is the
/// entry's `skipped` reason, never an error of the view.
pub fn corner_assembly(d: &Drawing, view: Id) -> Result<Vec<CornerAssembly>, String> {
    let (_, corners) = resolve_view_details(d, view)?;
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let (graph, sides) = graph_and_sides(&p, view);
    let mut out = Vec::new();
    for (i, (j, corner)) in graph.junctions.iter().zip(&corners).enumerate() {
        let id = match corner {
            RunDetail::Tagged(id) | RunDetail::Spread(id) | RunDetail::Gap(id) => id,
            RunDetail::Conflict | RunDetail::Untyped => continue,
        };
        let skip = |why: String, detail: Option<Id>| CornerAssembly { junction: i, detail, lines: Vec::new(), skipped: Some(why) };
        let Ok(detail) = id.parse::<u64>().map(Id) else {
            out.push(skip("detail id is not a number".into(), None));
            continue;
        };
        out.push(match corner_lines(d, detail, j, &graph.runs, &sides.runs) {
            Ok(lines) => CornerAssembly { junction: i, detail: Some(detail), lines, skipped: None },
            Err(e) => skip(e, Some(detail)),
        });
    }
    Ok(out)
}

/// Every line written to a view's assembly block, in write order: the runs' lines, then the
/// corners'.
fn block_lines_of<'a>(runs: &'a [RunAssembly], corners: &'a [CornerAssembly]) -> impl Iterator<Item = &'a AssemblyLine> {
    runs.iter().flat_map(|r| &r.lines).chain(corners.iter().flat_map(|c| &c.lines))
}

/// The midpoint of a run's centreline: a line's mean of ends, an arc's point at half its sweep.
fn run_midpoint(run: &WallRun) -> Point2 {
    match run.centreline {
        RunCurve::Line { from, to } => Point2 { x: (from.x + to.x) / 2.0, y: (from.y + to.y) / 2.0 },
        RunCurve::Arc { center, radius, start_angle, sweep } => {
            let a = start_angle + sweep / 2.0;
            Point2 { x: center.x + radius * a.cos(), y: center.y + radius * a.sin() }
        }
    }
}

/// The face entity ids of `runs`, deduped, sorted by display string.
fn face_ids<'a>(runs: impl IntoIterator<Item = &'a WallRun>) -> Vec<Id> {
    let mut ids: Vec<Id> = runs.into_iter().flat_map(|r| r.faces.iter().map(|f| f.from.entity_id)).collect();
    ids.sort_by_cached_key(Id::to_string);
    ids.dedup();
    ids
}

/// The assembly plans' own gaps as RESOLVE issues (§10): for every plan or section view that has an assembly
/// block, in document view order, each laid run's width conflict (by run), then each junction with
/// no corner detail that has a laid arm (by junction), then each run that got no detail — any skip
/// reason, LAMB T471 (by run). Locations are view-local.
pub(crate) fn wall_issues(d: &Drawing) -> Result<Vec<Issue>, String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for v in doc.views.iter().filter(|v| matches!(v, AnyView::Plan(_) | AnyView::Vertical(_))) {
        let plan = v.id();
        if d.block(&assembly_block_name(plan)).is_none() {
            continue;
        }
        let runs = assembly_lines(d, plan)?;
        let (graph, _) = graph_and_sides(&p, plan);
        let (_, corners) = resolve_view_details(d, plan)?;
        let mitred = doc.mitre_plans.contains(&plan);
        let issue = |kind: IssueKind, id: String, location: Point2, entity_ids: Vec<Id>, note: String| Issue {
            id,
            kind,
            view_id: plan,
            location,
            entity_ids,
            priority: kind.priority(),
            note,
        };
        for r in runs.iter().filter(|r| r.skipped.is_none()) {
            let (Some(c), Some(run)) = (r.conflict, graph.runs.get(r.run)) else { continue };
            out.push(issue(
                IssueKind::WidthConflict,
                format!("width_conflict:{plan}:{}", r.run),
                run_midpoint(run),
                face_ids([run]),
                format!("Wall run {}: {}", r.run, c.text()),
            ));
        }
        for (j, (junction, corner)) in graph.junctions.iter().zip(&corners).enumerate() {
            if !matches!(corner, RunDetail::Untyped | RunDetail::Conflict) {
                continue;
            }
            if !junction.arms.iter().any(|a| runs.get(a.run).is_some_and(|r| r.skipped.is_none())) {
                continue;
            }
            let cut = junction_bisector(junction).is_some() || tee_butt(junction, &graph.runs).is_some();
            let suffix = if mitred && cut { " (mitred)" } else { "" };
            out.push(issue(
                IssueKind::UndetailedCorner,
                format!("undetailed_corner:{plan}:{j}"),
                junction.point,
                face_ids(junction.arms.iter().filter_map(|a| graph.runs.get(a.run))),
                format!("Corner at ({:.0}, {:.0}) has no corner detail{suffix}", junction.point.x, junction.point.y),
            ));
        }
        for r in &runs {
            let (Some(why), Some(run)) = (&r.skipped, graph.runs.get(r.run)) else { continue };
            out.push(issue(
                IssueKind::UndetailedRun,
                format!("undetailed_run:{plan}:{}", r.run),
                run_midpoint(run),
                face_ids([run]),
                format!("Wall run {} has no detail ({why})", r.run),
            ));
        }
    }
    Ok(out)
}

/// `Ok` when `plan` names a plan view.
pub(crate) fn check_plan(d: &Drawing, plan: Id) -> Result<(), String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    match doc.views.iter().find(|v| v.id() == plan) {
        None => Err("No such view".into()),
        Some(AnyView::Plan(_)) => Ok(()),
        Some(_) => Err("Not a plan view".into()),
    }
}

/// `Ok` when `view` names a plan or section view — the views ASSEMBLYPLAN, WALLCONFLICTS and
/// WALLRUN accept.
pub(crate) fn check_wall_view(d: &Drawing, view: Id) -> Result<(), String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    match doc.views.iter().find(|v| v.id() == view) {
        None => Err("No such view".into()),
        Some(AnyView::Plan(_) | AnyView::Vertical(_)) => Ok(()),
        Some(_) => Err("Not a plan or section view".into()),
    }
}

/// The entity an assembly line is written as: a LINE, or for an arc line an ARC, counter-clockwise
/// from `start` to `end` (a clockwise span's ends swapped).
fn line_entity(l: &AssemblyLine) -> EntityKind {
    let Some(arc) = l.arc else {
        return EntityKind::Line(Line { a: Vec2::new(l.from.x, l.from.y).to3(0.0), b: Vec2::new(l.to.x, l.to.y).to3(0.0) });
    };
    let angle = |p: Point2| (p.y - arc.center.y).atan2(p.x - arc.center.x);
    let (a, b) = if arc.ccw { (l.from, l.to) } else { (l.to, l.from) };
    EntityKind::Arc(ArcEntity { center: Vec2::new(arc.center.x, arc.center.y).to3(0.0), radius: arc.radius, start: angle(a), end: angle(b) })
}

/// Replaces the block [`assembly_block_name`]`(plan)` with one LINE or ARC ([`line_entity`]) per
/// line of `runs`, then of `corners`.
fn replace_block(d: &mut Drawing, plan: Id, runs: &[RunAssembly], corners: &[CornerAssembly]) -> Result<(), String> {
    let name = assembly_block_name(plan);
    d.blocks.insert(name.clone(), Arc::new(Block::new(&name)));
    let space = Space::Block(name);
    for l in block_lines_of(runs, corners) {
        let common = Common { layer: l.layer.clone(), ..Common::default() };
        d.add(&space, common, line_entity(l)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The block's canonical model-space INSERT (like a view's, `views::create_view`): on its own
/// layer named after the block, at the origin, scale 1. Added only when model space has no INSERT
/// of the block; identity is the block name, no handle is stored.
fn ensure_assembly_insert(d: &mut Drawing, plan: Id) {
    let name = assembly_block_name(plan);
    if d.model.iter().any(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == name)) {
        return;
    }
    d.ensure_layer(&name);
    let insert = Insert {
        block: name.clone(),
        insert: Vec3::ZERO,
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: Vec::new(),
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    };
    let handle = d.new_handle();
    d.model.push(Entity { handle, common: Common { layer: name, ..Common::default() }, kind: EntityKind::Insert(insert) });
}

/// Replaces the block [`assembly_block_name`]`(plan)` with one LINE per laid assembly line and
/// placed corner line (an empty block when nothing is laid), and gives it its model-space INSERT on
/// its own layer when it has none. `plan` must be a plan or section view; nothing is touched on
/// error.
pub fn write_assembly_block(d: &mut Drawing, plan: Id) -> Result<(Vec<RunAssembly>, Vec<CornerAssembly>), String> {
    check_wall_view(d, plan)?;
    let runs = assembly_lines(d, plan)?;
    let corners = corner_assembly(d, plan)?;
    replace_block(d, plan, &runs, &corners)?;
    ensure_assembly_insert(d, plan);
    Ok((runs, corners))
}

/// `true` when `block` holds exactly the lines of `runs` then `corners`, in write order: same count, each pair on
/// the same layer and of the same kind ([`line_entity`]) — a LINE's ends within
/// [`TOLERANCE`]`.coincidence_mm`; an ARC's centre and radius within that, its start and end angles
/// within `TOLERANCE.angle_deg`.
fn block_matches(block: &Block, runs: &[RunAssembly], corners: &[CornerAssembly]) -> bool {
    let tol = TOLERANCE.coincidence_mm;
    let near = |p: cadcraft_geom::Vec3, q: cadcraft_geom::Vec3| (p.x - q.x).hypot(p.y - q.y) <= tol;
    let same_angle = |a: f64, b: f64| {
        let d = (a - b).rem_euclid(std::f64::consts::TAU);
        d.min(std::f64::consts::TAU - d) <= TOLERANCE.angle_deg.to_radians()
    };
    let mut want = block_lines_of(runs, corners);
    for e in block.entities.iter() {
        let Some(l) = want.next() else { return false };
        if e.common.layer != l.layer {
            return false;
        }
        let ok = match (&e.kind, line_entity(l)) {
            (EntityKind::Line(got), EntityKind::Line(w)) => near(got.a, w.a) && near(got.b, w.b),
            (EntityKind::Arc(got), EntityKind::Arc(w)) => {
                near(got.center, w.center) && (got.radius - w.radius).abs() <= tol && same_angle(got.start, w.start) && same_angle(got.end, w.end)
            }
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    want.next().is_none()
}

/// The after-command observer: every plan or section view whose assembly block exists gets it rewritten when
/// it no longer equals [`assembly_lines`]; the drawing is written back only when some block
/// changed. A plan whose derivation fails is echoed and its block left as it was.
pub fn assembly_observer(s: &mut Session, _before: &Arc<Drawing>) {
    let mut rewrite = Vec::new();
    let mut errors = Vec::new();
    {
        let Ok(d) = s.doc() else { return };
        let Ok(doc) = read_document(d) else { return };
        for v in doc.views.iter().filter(|v| matches!(v, AnyView::Plan(_) | AnyView::Vertical(_))) {
            let plan = v.id();
            let Some(block) = d.block(&assembly_block_name(plan)) else { continue };
            match assembly_lines(d, plan).and_then(|runs| Ok((runs, corner_assembly(d, plan)?))) {
                Ok((runs, corners)) if !block_matches(block, &runs, &corners) => rewrite.push((plan, runs, corners)),
                Ok(_) => {}
                Err(e) => errors.push(e),
            }
        }
    }
    for e in errors {
        s.echo(format!("LAMB assembly: {e}"));
    }
    if rewrite.is_empty() {
        return;
    }
    let Ok(current) = s.doc() else { return };
    let mut after = current.clone();
    for (plan, runs, corners) in &rewrite {
        if let Err(e) = replace_block(&mut after, *plan, runs, corners) {
            s.echo(format!("LAMB assembly: {e}"));
            return;
        }
    }
    if let Ok(d) = s.doc_mut() {
        *d = after;
    }
}

/// The runs' width conflicts as `[{run, delta, toward, text}]`, in run order.
fn conflicts_json(runs: &[RunAssembly]) -> Vec<Value> {
    runs.iter().filter_map(|r| r.conflict.map(|c| json!({ "run": r.run, "delta": c.delta, "toward": c.toward.name(), "text": c.text() }))).collect()
}

/// ASSEMBLYPLAN `{viewId, mitre?}`: [`write_assembly_block`] for that plan; a given `mitre` first
/// adds the plan to (or removes it from) the document's `mitre_plans`, in the same undo step. Returns
/// `{block, lines, corners, skipped: [{run, reason} | {junction, reason}], conflicts: [{run, delta,
/// toward, text}]}` — `lines` counting run and corner lines, `corners` the placed corner details.
pub fn assemblyplan(s: &mut Session, p: &Value) -> EngineResult<Value> {
    let plan = p.get("viewId").and_then(id_value).ok_or_else(|| other("No such view"))?;
    // Validate before `doc_mut`: touching the drawing on a refusal would still record an undo step.
    let mitre = match p.get("mitre") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return Err(other("mitre must be true or false")),
    };
    check_wall_view(s.doc()?, plan).map_err(other)?;
    let mut doc = read_document(s.doc()?).map_err(|e| other(e.to_string()))?;
    let flag_changes = mitre.is_some_and(|m| m != doc.mitre_plans.contains(&plan));
    if let (true, Some(m)) = (flag_changes, mitre) {
        doc.mitre_plans.retain(|v| *v != plan);
        if m {
            doc.mitre_plans.push(plan);
        }
    }
    let d = s.doc_mut()?;
    if flag_changes {
        write_document(d, &doc).map_err(|e| other(e.to_string()))?;
    }
    let (runs, corners) = write_assembly_block(d, plan).map_err(other)?;
    let lines = block_lines_of(&runs, &corners).count();
    let mut skipped: Vec<Value> = runs.iter().filter_map(|r| r.skipped.as_ref().map(|why| json!({ "run": r.run, "reason": why }))).collect();
    let run_skips = skipped.len();
    skipped.extend(corners.iter().filter_map(|c| c.skipped.as_ref().map(|why| json!({ "junction": c.junction, "reason": why }))));
    let placed = corners.iter().filter(|c| c.skipped.is_none()).count();
    s.echo(format!("{lines} assembly lines, {run_skips} runs skipped, {placed} corners placed"));
    Ok(json!({ "block": assembly_block_name(plan), "lines": lines, "corners": placed, "skipped": skipped, "conflicts": conflicts_json(&runs) }))
}

/// WALLCONFLICTS `{viewId}`: the plan's width conflicts, computed without writing anything.
/// Returns `{conflicts: [{run, delta, toward, text}]}`.
pub fn wallconflicts(s: &mut Session, p: &Value) -> EngineResult<Value> {
    let plan = p.get("viewId").and_then(id_value).ok_or_else(|| other("No such view"))?;
    let d = s.doc()?;
    check_wall_view(d, plan).map_err(other)?;
    let conflicts = conflicts_json(&assembly_lines(d, plan).map_err(other)?);
    s.echo(format!("{} width conflicts", conflicts.len()));
    Ok(json!({ "conflicts": conflicts }))
}

/// ASSEMBLYPLAN's interactive form, on the edited plan or section view: one Yes/No prompt for mitring, Enter
/// keeping the plan's current setting.
struct AssemblyPlanM;

fn mitred(s: &Session, plan: Id) -> bool {
    s.doc().ok().and_then(|d| read_document(d).ok()).is_some_and(|doc| doc.mitre_plans.contains(&plan))
}

impl Interactive for AssemblyPlanM {
    fn name(&self) -> &'static str {
        "assemblyplan"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        let current = edited_wall_view(s).is_ok_and(|plan| mitred(s, plan));
        Prompt::new("Mitre missing corner details", Accept::TEXT).kw(&["Yes", "No"]).default(if current { "Yes" } else { "No" }.to_owned())
    }
    fn begin(&mut self, s: &mut Session) -> EngineResult<Step> {
        if let Err(e) = edited_wall_view(s) {
            s.echo(e);
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> EngineResult<Step> {
        let mitre = match i {
            Input::Cancel => return Ok(Step::Cancel),
            Input::Enter => None,
            Input::Text(t) | Input::Keyword(t) => match t.trim().to_ascii_lowercase().as_str() {
                "y" | "yes" => Some(true),
                "n" | "no" => Some(false),
                _ => {
                    s.echo("Yes or No");
                    return Ok(Step::Continue);
                }
            },
            _ => return Ok(Step::Continue),
        };
        let plan = edited_wall_view(s).map_err(other)?;
        let mut p = json!({ "viewId": plan.0 });
        if let Some(m) = mitre {
            p["mitre"] = json!(m);
        }
        match assemblyplan(s, &p) {
            Ok(r) => {
                let skipped = r["skipped"].as_array().map_or(0, |v| v.iter().filter(|x| x.get("run").is_some()).count());
                let conflicts = r["conflicts"].as_array().map_or(0, Vec::len);
                s.echo(format!("Assembly plan: {} lines, {skipped} runs skipped, {conflicts} width conflicts", r["lines"]));
            }
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

pub(crate) fn assemblyplan_machine(_s: &Session) -> EngineResult<Box<dyn Interactive>> {
    Ok(Box::new(AssemblyPlanM))
}

/// WALLCONFLICTS' interactive form: no prompt; the edited view's conflicts, one line each.
struct WallConflictsM;

impl Interactive for WallConflictsM {
    fn name(&self) -> &'static str {
        "wallconflicts"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Wall conflicts", Accept::TEXT)
    }
    fn begin(&mut self, s: &mut Session) -> EngineResult<Step> {
        let plan = match edited_wall_view(s) {
            Ok(plan) => plan,
            Err(e) => {
                s.echo(e);
                return Ok(Step::Done);
            }
        };
        match wallconflicts(s, &json!({ "viewId": plan.0 })) {
            Ok(r) => {
                let list = r["conflicts"].as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    s.echo("No width conflicts");
                }
                for c in list {
                    s.echo(format!("Run {}: {}", c["run"], c["text"].as_str().unwrap_or_default()));
                }
            }
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
    fn input(&mut self, _s: &mut Session, _i: Input) -> EngineResult<Step> {
        Ok(Step::Done)
    }
}

pub(crate) fn wallconflicts_machine(_s: &Session) -> EngineResult<Box<dyn Interactive>> {
    Ok(Box::new(WallConflictsM))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use cadcraft_doc::Handle;
    use cadcraft_engine::Session;
    use serde_json::json;
    use std::collections::HashSet;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    fn all_handles(s: &Session) -> T<HashSet<Handle>> {
        let d = s.doc()?;
        Ok(d.model.iter().map(|e| e.handle).chain(d.blocks.values().flat_map(|b| b.entities.iter().map(|e| e.handle))).collect())
    }

    fn line(s: &mut Session, a: [f64; 2], b: [f64; 2]) -> T<Handle> {
        let before = all_handles(s)?;
        s.execute("line", &json!({ "points": [a, b] }))?;
        Ok(all_handles(s)?.into_iter().find(|h| !before.contains(h)).ok_or("line not drawn")?)
    }

    /// `detailtags.rs`'s room: a plan with a closed 5000 × 4000 room of 200 mm walls on `WALL-EXT`
    /// (outer face 0..5000 × 0..4000) and a detail `D1` holding a reference (0,0)–(1000,0) of
    /// `kind` and bands at y = −20 (`CLAD`), 50 and 200 (`GYP`). With `tagged`, one edge tag on the
    /// bottom wall. Returns (session, plan).
    fn room(kind: &str, tagged: bool) -> T<(Session, Id)> {
        room_with(kind, tagged, 200.0)
    }

    /// [`room`] with the inner face `inner` mm inside the outer one (the wall `inner` thick).
    pub(crate) fn room_with(kind: &str, tagged: bool, inner: f64) -> T<(Session, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let detail = Id(s.execute("newdetail", &json!({ "name": "D1" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        let r = line(&mut s, [0.0, 0.0], [1000.0, 0.0])?;
        s.execute("detailref", &json!({ "handle": format!("{:X}", r.0), "kind": kind }))?;
        s.execute("layer.new", &json!({ "name": "CLAD", "current": true }))?;
        line(&mut s, [0.0, -20.0], [1000.0, -20.0])?;
        s.execute("layer.new", &json!({ "name": "GYP", "current": true }))?;
        line(&mut s, [0.0, 50.0], [1000.0, 50.0])?;
        line(&mut s, [0.0, 200.0], [1000.0, 200.0])?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [5000, 0], [5000, 4000], [0, 4000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[inner, inner], [5000.0 - inner, inner], [5000.0 - inner, 4000.0 - inner], [inner, 4000.0 - inner]], "closed": true }))?;
        if tagged {
            s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
            s.execute("detailtag", &json!({ "detailViewId": detail.0, "tip": {"x": 2500.0, "y": 0.0}, "bubble": {"x": 1500.0, "y": -1000.0} }))?;
        }
        Ok((s, plan))
    }

    fn horizontal(a: &RunAssembly) -> Option<Vec<(f64, String)>> {
        let mut ys: Vec<(f64, String)> = Vec::new();
        for l in &a.lines {
            if (l.from.y - l.to.y).abs() > 1e-9 {
                return None;
            }
            ys.push(((l.from.y * 1e6).round() / 1e6, l.layer.clone()));
        }
        Some(ys)
    }

    #[test]
    fn a_face_detail_lays_its_bands_along_every_run_from_the_exterior_face() -> R {
        let (s, plan) = room("exterior_face", true)?;
        let d = s.doc()?;
        let runs = assembly_lines(d, plan)?;
        assert_eq!(runs.len(), 4);
        assert!(runs.iter().all(|r| r.skipped.is_none() && r.lines.len() == 3 && r.detail.is_some()), "{runs:?}");

        let p = DrawingProject::new(d)?;
        let (graph, _) = graph_and_sides(&p, plan);
        let layers = |ys: &[(f64, String)]| ys.iter().map(|(_, l)| l.clone()).collect::<Vec<_>>();
        let mut bottom = None;
        let mut top = None;
        for r in &runs {
            let Some(ys) = horizontal(r) else { continue };
            let lamb_core::derivation::walls::RunCurve::Line { from, to } = graph.runs[r.run].centreline else { continue };
            for l in &r.lines {
                assert_eq!((l.from.x, l.to.x), (from.x, to.x), "spans the centreline's x extent");
            }
            if from.y < 2000.0 { bottom = Some(ys) } else { top = Some(ys) }
        }
        let (bottom, top) = (bottom.ok_or("no bottom run")?, top.ok_or("no top run")?);
        assert_eq!(bottom.iter().map(|y| y.0).collect::<Vec<_>>(), [-20.0, 50.0, 200.0]);
        assert_eq!(layers(&bottom), ["CLAD", "GYP", "GYP"]);
        assert_eq!(top.iter().map(|y| y.0).collect::<Vec<_>>(), [4020.0, 3950.0, 3800.0]);
        assert_eq!(layers(&top), ["CLAD", "GYP", "GYP"]);
        Ok(())
    }

    #[test]
    fn a_grid_reference_or_no_tag_skips_every_run() -> R {
        let (s, plan) = room("grid", true)?;
        let runs = assembly_lines(s.doc()?, plan)?;
        assert_eq!(runs.len(), 4);
        assert!(runs.iter().all(|r| r.lines.is_empty() && r.skipped.as_deref() == Some("reference kind not laid yet")), "{runs:?}");

        let (s, plan) = room("exterior_face", false)?;
        let runs = assembly_lines(s.doc()?, plan)?;
        assert_eq!(runs.len(), 4);
        assert!(runs.iter().all(|r| r.lines.is_empty() && r.detail.is_none() && r.skipped.as_deref() == Some("untyped")), "{runs:?}");
        Ok(())
    }

    fn block_lines(s: &Session, name: &str) -> T<Option<Vec<(f64, f64, String)>>> {
        let d = s.doc()?;
        let Some(b) = d.block(name) else { return Ok(None) };
        let mut v = Vec::new();
        for e in b.entities.iter() {
            let EntityKind::Line(l) = &e.kind else { return Err(format!("not a LINE: {:?}", e.kind).into()) };
            v.push((l.a.y, l.b.y, e.common.layer.clone()));
        }
        Ok(Some(v))
    }

    #[test]
    fn assemblyplan_writes_the_laid_lines_into_the_plans_assembly_block_replacing_it() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        let name = assembly_block_name(plan);
        let r = s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert_eq!(r, json!({ "block": name, "lines": 12, "corners": 0, "skipped": [], "conflicts": [] }));
        let lines = block_lines(&s, &name)?.ok_or("no block")?;
        assert_eq!(lines.len(), 12);
        assert!(lines.iter().all(|(_, _, l)| l == "CLAD" || l == "GYP"), "{lines:?}");
        let mut bottom: Vec<f64> = lines.iter().filter(|(a, b, _)| a == b && *a < 1000.0).map(|(a, _, _)| (a * 1e6).round() / 1e6).collect();
        bottom.sort_by(f64::total_cmp);
        assert_eq!(bottom, [-20.0, 50.0, 200.0]);
        let views_before = read_document(s.doc()?)?.views.len();

        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert_eq!(block_lines(&s, &name)?.ok_or("no block")?.len(), 12, "replaced, not appended");
        assert_eq!(read_document(s.doc()?)?.views.len(), views_before, "not a view");

        s.undo()?;
        assert_eq!(block_lines(&s, &name)?.map(|v| v.len()), Some(12), "undo the second run");
        s.undo()?;
        assert_eq!(block_lines(&s, &name)?, None, "undo the first run");
        Ok(())
    }

    #[test]
    fn assemblyplan_refuses_a_detail_view_and_an_unknown_id_without_touching_the_drawing() -> R {
        let (mut s, _plan) = room("exterior_face", true)?;
        let detail = read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id();
        let blocks = s.doc()?.blocks.len();
        let e = s.execute("assemblyplan", &json!({ "viewId": detail.0 })).err().ok_or("detail accepted")?;
        assert!(e.to_string().contains("Not a plan or section view"), "{e}");
        let e = s.execute("assemblyplan", &json!({ "viewId": 999_999 })).err().ok_or("unknown accepted")?;
        assert!(e.to_string().contains("No such view"), "{e}");
        assert_eq!(s.doc()?.blocks.len(), blocks);
        assert!(s.doc()?.blocks.keys().all(|k| !k.starts_with("LAMB_ASSEMBLY_")));
        Ok(())
    }

    #[test]
    fn assemblyplan_arrays_a_repeat_at_its_pitch_along_each_run() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        let detail = read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id();
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        s.execute("layer.new", &json!({ "name": "STUD", "current": true }))?;
        let before = all_handles(&s)?;
        s.execute("pline", &json!({ "vertices": [[100, 50], [138, 50], [138, 200], [100, 200]], "closed": true }))?;
        let stud = all_handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("no stud")?;
        let before = all_handles(&s)?;
        s.execute("dimaligned", &json!({ "p1": [100, 0], "p2": [500, 0], "at": [300, -300] }))?;
        let pitch = all_handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("no dimension")?;
        s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [stud.hex()] }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;

        let d = s.doc()?;
        let p = DrawingProject::new(d)?;
        let (graph, _) = graph_and_sides(&p, plan);
        let runs = assembly_lines(d, plan)?;
        assert_eq!(runs.len(), 4);
        let r6 = |v: f64| (v * 1e6).round() / 1e6;
        let mut total = 0;
        let mut saw_bottom = false;
        for r in &runs {
            let lamb_core::derivation::walls::RunCurve::Line { from, to } = graph.runs[r.run].centreline else { return Err("arc run".into()) };
            let len = (to.x - from.x).hypot(to.y - from.y);
            let copies = ((len - 138.0) / 400.0).floor() as usize + 1;
            assert_eq!(r.lines.len(), 3 + 4 * copies, "run {}: {r:?}", r.run);
            assert_eq!(r.lines[..3].iter().map(|l| l.layer.as_str()).collect::<Vec<_>>(), ["CLAD", "GYP", "GYP"]);
            assert!(r.lines[3..].iter().all(|l| l.layer == "STUD"));
            total += r.lines.len();
            if from.y < 2000.0 && (from.y - to.y).abs() < 1e-9 {
                saw_bottom = true;
                // Exterior face at y = 0: the stud's first edge sits 50 into the wall, 100 along.
                let first = &r.lines[3];
                let dir = (to.x - from.x) / len;
                assert_eq!((r6(first.from.x), r6(first.from.y), r6(first.to.y)), (r6(from.x + dir * 100.0), 50.0, 50.0));
            }
        }
        assert!(saw_bottom, "no bottom run");

        let n = undo_len(&s)?;
        let out = s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert_eq!(out["lines"].as_u64(), Some(total as u64), "{out}");
        let name = assembly_block_name(plan);
        let lines = block_lines(&s, &name)?.ok_or("no block")?;
        assert_eq!(lines.len(), total);
        assert_eq!(lines.iter().filter(|(_, _, l)| l == "STUD").count(), total - 12);
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        s.undo()?;
        assert_eq!(block_lines(&s, &name)?, None, "one undo removes it all");
        Ok(())
    }

    fn detail_id(s: &Session) -> T<Id> {
        Ok(read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id())
    }

    #[test]
    fn the_assembly_block_regenerates_in_the_same_undo_step_as_a_detail_edit() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        crate::maintain::install(&mut s);
        let name = assembly_block_name(plan);
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert_eq!(block_lines(&s, &name)?.map(|v| v.len()), Some(12));

        let detail = detail_id(&s)?;
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        s.execute("layer.current", &json!({ "name": "GYP" }))?;
        let n = undo_len(&s)?;
        line(&mut s, [0.0, 100.0], [1000.0, 100.0])?;
        assert_eq!(undo_len(&s)?, n + 1);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let lines = block_lines(&s, &name)?.ok_or("no block")?;
        assert_eq!(lines.len(), 16, "no further ASSEMBLYPLAN");
        assert_eq!(lines.iter().filter(|(a, b, l)| l == "GYP" && a == b && (*a - 100.0).abs() < 1e-6).count(), 1, "{lines:?}");

        while undo_len(&s)? > n {
            s.undo()?;
        }
        assert_eq!(block_lines(&s, &name)?.map(|v| v.len()), Some(12), "regeneration rode the line's step");
        Ok(())
    }

    #[test]
    fn a_plan_without_an_assembly_block_gets_none_and_unrelated_edits_leave_the_block_alone() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        crate::maintain::install(&mut s);
        let detail = detail_id(&s)?;
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        s.execute("layer.current", &json!({ "name": "GYP" }))?;
        line(&mut s, [0.0, 100.0], [1000.0, 100.0])?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        line(&mut s, [0.0, -3000.0], [1000.0, -3000.0])?;
        assert!(s.doc()?.blocks.keys().all(|k| !k.starts_with("LAMB_ASSEMBLY_")));

        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let name = assembly_block_name(plan);
        let block = s.doc()?.block(&name).ok_or("no block")?.clone();
        s.execute("editview", &json!({ "viewId": null }))?;
        s.execute("layer.new", &json!({ "name": "NOTES", "current": true }))?;
        line(&mut s, [90_000.0, 90_000.0], [91_000.0, 90_000.0])?;
        assert!(Arc::ptr_eq(&block, s.doc()?.block(&name).ok_or("no block")?), "unrelated command left the block untouched");
        Ok(())
    }

    #[test]
    fn erasing_the_detail_tag_empties_the_block_but_keeps_it() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        crate::maintain::install(&mut s);
        let name = assembly_block_name(plan);
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let d = s.doc()?;
        let tags: Vec<String> = d
            .model
            .iter()
            .chain(d.blocks.values().flat_map(|b| b.entities.iter()))
            .filter(|e| e.common.layer == "TAGS")
            .map(|e| e.handle.hex())
            .collect();
        assert!(!tags.is_empty(), "no tag entities");
        s.execute("erase", &json!({ "handles": tags }))?;
        assert!(assembly_lines(s.doc()?, plan)?.iter().all(|r| r.skipped.as_deref() == Some("untyped")));
        assert_eq!(block_lines(&s, &name)?, Some(Vec::new()));
        Ok(())
    }

    /// Model-space INSERTs of `block`: (handle, layer).
    pub(crate) fn model_inserts(d: &Drawing, block: &str) -> Vec<(Handle, String)> {
        d.model.iter().filter(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == block)).map(|e| (e.handle, e.common.layer.clone())).collect()
    }

    #[test]
    fn assemblyplan_gives_the_block_one_model_space_insert_on_its_own_layer() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        crate::maintain::install(&mut s);
        let name = assembly_block_name(plan);
        let n = undo_len(&s)?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let ins = model_inserts(s.doc()?, &name);
        assert_eq!(ins.len(), 1, "{ins:?}");
        assert_eq!(ins[0].1, name);
        assert!(s.doc()?.layers.iter().any(|l| l.name == name));
        let EntityKind::Insert(i) = &s.doc()?.model.iter().find(|e| e.handle == ins[0].0).ok_or("gone")?.kind else {
            return Err("not an insert".into());
        };
        assert_eq!((i.insert, i.scale, i.rotation, i.cols, i.rows), (Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0), 0.0, 1, 1));

        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert_eq!(model_inserts(s.doc()?, &name).len(), 1, "a second ASSEMBLYPLAN keeps one");
        let detail = detail_id(&s)?;
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        s.execute("layer.current", &json!({ "name": "GYP" }))?;
        line(&mut s, [0.0, 100.0], [1000.0, 100.0])?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        assert_eq!(block_lines(&s, &name)?.map(|v| v.len()), Some(16), "observer regenerated");
        assert_eq!(model_inserts(s.doc()?, &name), ins, "the observer keeps the same insert");

        // DXF keeps the insert and its layer.
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes())?;
        let kept = model_inserts(&back, &name);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].1, name);
        assert!(back.block(&name).is_some());

        s.execute("erase", &json!({ "handles": [ins[0].0.hex()] }))?;
        assert!(model_inserts(s.doc()?, &name).is_empty());
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert_eq!(model_inserts(s.doc()?, &name).len(), 1, "erased insert restored");

        while undo_len(&s)? > n + 1 {
            s.undo()?;
        }
        assert_eq!(model_inserts(s.doc()?, &name).len(), 1);
        s.undo()?;
        let d = s.doc()?;
        assert!(d.block(&name).is_none() && model_inserts(d, &name).is_empty(), "one undo removes block and insert");
        assert!(!d.layers.iter().any(|l| l.name == name), "and the layer");
        Ok(())
    }

    pub(crate) fn undo_len(s: &Session) -> T<usize> {
        Ok(s.state()?.undo.len())
    }

    #[test]
    fn a_wall_thicker_than_its_detail_reports_a_conflict_on_every_run() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 210.0)?;
        let n = undo_len(&s)?;
        let r = s.execute("wallconflicts", &json!({ "viewId": plan.0 }))?;
        assert_eq!(undo_len(&s)?, n, "read-only: no undo step");
        let c = r["conflicts"].as_array().ok_or("no conflicts")?;
        assert_eq!(c.len(), 4, "{r}");
        for x in c {
            assert!((x["delta"].as_f64().ok_or("no delta")? + 10.0).abs() < 1e-9, "{x}");
            assert_eq!(x["toward"], "interior");
            assert_eq!(x["text"], "-10.0mm toward interior");
        }
        let a = s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert_eq!(a["conflicts"], r["conflicts"]);

        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        assert_eq!(s.execute("wallconflicts", &json!({ "viewId": plan.0 }))?, json!({ "conflicts": [] }));
        Ok(())
    }

    #[test]
    fn wall_issues_list_each_undetailed_run_after_the_corners() -> R {
        let (mut s, plan) = room("exterior_face", false)?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let list = wall_issues(s.doc()?)?;
        assert_eq!(list.len(), 4, "{list:?}");
        let p = DrawingProject::new(s.doc()?)?;
        let (graph, _) = graph_and_sides(&p, plan);
        for (i, u) in list.iter().enumerate() {
            assert_eq!(u.kind, IssueKind::UndetailedRun);
            assert_eq!(u.id, format!("undetailed_run:{plan}:{i}"));
            assert_eq!(u.priority, 7);
            assert_eq!(u.note, format!("Wall run {i} has no detail (untyped)"));
            assert_eq!(u.entity_ids.len(), 2, "{u:?}");
            assert_eq!(u.location, run_midpoint(&graph.runs[i]));
        }
        assert_eq!(serde_json::to_value(&list[0])?["kind"], "undetailed_run");

        let (mut s, plan) = room("exterior_face", true)?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert!(wall_issues(s.doc()?)?.iter().all(|i| i.kind != IssueKind::UndetailedRun), "fully tagged room");
        Ok(())
    }

    #[test]
    fn two_different_edge_tags_list_the_runs_between_them() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        let d2 = Id(s.execute("newdetail", &json!({ "name": "D2" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": d2.0 }))?;
        let r = line(&mut s, [0.0, 0.0], [1000.0, 0.0])?;
        s.execute("detailref", &json!({ "handle": format!("{:X}", r.0), "kind": "exterior_face" }))?;
        line(&mut s, [0.0, 30.0], [1000.0, 30.0])?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("detailtag", &json!({ "detailViewId": d2.0, "tip": {"x": 2500.0, "y": 4000.0}, "bubble": {"x": 2500.0, "y": 5000.0} }))?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let runs = assembly_lines(s.doc()?, plan)?;
        let skipped: Vec<usize> = runs.iter().filter(|r| r.skipped.is_some()).map(|r| r.run).collect();
        assert_eq!(skipped.len(), 2, "the two side walls: {runs:?}");
        let all = wall_issues(s.doc()?)?;
        assert!(all.iter().any(|i| i.kind == IssueKind::UndetailedCorner), "{all:?}");
        let first_run = all.iter().position(|i| i.kind == IssueKind::UndetailedRun).ok_or("no run issue")?;
        assert!(all[first_run..].iter().all(|i| i.kind == IssueKind::UndetailedRun), "runs come after the corners: {all:?}");
        let listed: Vec<String> = all.into_iter().filter(|i| i.kind == IssueKind::UndetailedRun).map(|i| i.id).collect();
        assert_eq!(listed, skipped.iter().map(|i| format!("undetailed_run:{plan}:{i}")).collect::<Vec<_>>());
        Ok(())
    }

    #[test]
    fn wall_issues_list_each_plans_width_conflicts_then_undetailed_corners() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 210.0)?;
        assert!(wall_issues(s.doc()?)?.is_empty(), "no assembly block, no issues");
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let list = wall_issues(s.doc()?)?;
        assert_eq!(list.len(), 8, "{list:?}");
        let p = DrawingProject::new(s.doc()?)?;
        let (graph, _) = graph_and_sides(&p, plan);
        for (i, w) in list[..4].iter().enumerate() {
            assert_eq!(w.kind, IssueKind::WidthConflict);
            assert_eq!(w.id, format!("width_conflict:{plan}:{i}"));
            assert_eq!(w.view_id, plan);
            assert_eq!(w.priority, 5);
            assert!(w.note.starts_with(&format!("Wall run {i}: -10.0mm toward ")), "{}", w.note);
            assert_eq!(w.entity_ids.len(), 2, "{w:?}");
            assert_eq!(w.location, run_midpoint(&graph.runs[i]));
        }
        for (j, c) in list[4..].iter().enumerate() {
            assert_eq!(c.kind, IssueKind::UndetailedCorner);
            assert_eq!(c.id, format!("undetailed_corner:{plan}:{j}"));
            assert_eq!(c.priority, 6);
            assert_eq!(c.location, graph.junctions[j].point);
            assert!(c.note.starts_with("Corner at (") && c.note.ends_with(") has no corner detail"), "{}", c.note);
            assert!(!c.entity_ids.is_empty());
        }
        let v = serde_json::to_value(&list[0])?;
        assert_eq!(v["kind"], "width_conflict");
        assert_eq!(serde_json::to_value(&list[4])?["kind"], "undetailed_corner");

        s.execute("assemblyplan", &json!({ "viewId": plan.0, "mitre": true }))?;
        let list = wall_issues(s.doc()?)?;
        assert_eq!(list.len(), 8);
        assert!(list[4..].iter().all(|c| c.note.ends_with(") has no corner detail (mitred)")), "{list:?}");
        assert!(list[..4].iter().all(|w| !w.note.ends_with("(mitred)")));
        Ok(())
    }

    #[test]
    fn wallconflicts_refuses_a_detail_view_and_an_unknown_id() -> R {
        let (mut s, _plan) = room("exterior_face", true)?;
        let detail = read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id();
        let n = undo_len(&s)?;
        let e = s.execute("wallconflicts", &json!({ "viewId": detail.0 })).err().ok_or("detail accepted")?;
        assert!(e.to_string().contains("Not a plan or section view"), "{e}");
        let e = s.execute("wallconflicts", &json!({ "viewId": 999_999 })).err().ok_or("unknown accepted")?;
        assert!(e.to_string().contains("No such view"), "{e}");
        assert_eq!(undo_len(&s)?, n);
        Ok(())
    }

    fn r6(v: f64) -> f64 {
        (v * 1e6).round() / 1e6
    }

    /// Each band of the run whose centreline is horizontal at `y` (within 1 mm): (band y, min x,
    /// max x), in lay order.
    fn horizontal_spans(runs: &[RunAssembly], graph: &lamb_core::derivation::wall_junctions::WallGraph, y: f64) -> T<Vec<(f64, f64, f64)>> {
        for r in runs {
            let lamb_core::derivation::walls::RunCurve::Line { from, to } = graph.runs[r.run].centreline else { continue };
            if (from.y - y).abs() < 1.0 && (to.y - y).abs() < 1.0 {
                return Ok(r.lines[..r.bands].iter().map(|l| (r6(l.from.y), r6(l.from.x.min(l.to.x)), r6(l.from.x.max(l.to.x)))).collect());
            }
        }
        Err(format!("no run at y = {y}").into())
    }

    fn block_xy(s: &Session, name: &str) -> T<Vec<(f64, f64, f64, f64, String)>> {
        let b = s.doc()?.block(name).ok_or("no block")?;
        let mut v = Vec::new();
        for e in b.entities.iter() {
            let EntityKind::Line(l) = &e.kind else { return Err("not a LINE".into()) };
            v.push((r6(l.a.x), r6(l.a.y), r6(l.b.x), r6(l.b.y), e.common.layer.clone()));
        }
        Ok(v)
    }

    /// A plan with a D-shaped room of 200 mm walls: straight bottom, left and top faces and a
    /// semicircular right end about (3000, 1100) — outer face radius 1100, inner 900, so the arc
    /// run's centreline has radius 1000 — and [`room`]'s exterior-face detail tagged on the bottom.
    pub(crate) fn d_room() -> T<(Session, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let detail = Id(s.execute("newdetail", &json!({ "name": "D1" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        let r = line(&mut s, [0.0, 0.0], [1000.0, 0.0])?;
        s.execute("detailref", &json!({ "handle": format!("{:X}", r.0), "kind": "exterior_face" }))?;
        s.execute("layer.new", &json!({ "name": "CLAD", "current": true }))?;
        line(&mut s, [0.0, -20.0], [1000.0, -20.0])?;
        s.execute("layer.new", &json!({ "name": "GYP", "current": true }))?;
        line(&mut s, [0.0, 50.0], [1000.0, 50.0])?;
        line(&mut s, [0.0, 200.0], [1000.0, 200.0])?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        let v = |x: f64, y: f64, bulge: f64| json!({ "p": [x, y], "bulge": bulge });
        s.execute(
            "pline",
            &json!({ "vertices": [v(0.0, 0.0, 0.0), v(3000.0, 0.0, 1.0), v(3000.0, 2200.0, 0.0), v(0.0, 2200.0, 0.0)], "closed": true }),
        )?;
        s.execute(
            "pline",
            &json!({ "vertices": [v(200.0, 200.0, 0.0), v(3000.0, 200.0, 1.0), v(3000.0, 2000.0, 0.0), v(200.0, 2000.0, 0.0)], "closed": true }),
        )?;
        s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
        s.execute("detailtag", &json!({ "detailViewId": detail.0, "tip": {"x": 1500.0, "y": 0.0}, "bubble": {"x": 1500.0, "y": -1000.0} }))?;
        Ok((s, plan))
    }

    #[test]
    fn an_arc_run_gets_its_bands_as_concentric_arcs_in_the_block() -> R {
        let (mut s, plan) = d_room()?;
        crate::maintain::install(&mut s);
        let d = s.doc()?;
        let p = DrawingProject::new(d)?;
        let (graph, _) = graph_and_sides(&p, plan);
        let arcs = graph.runs.iter().filter(|r| matches!(r.centreline, lamb_core::derivation::walls::RunCurve::Arc { .. })).count();
        assert_eq!(arcs, 1, "{:?}", graph.runs.iter().map(|r| &r.centreline).collect::<Vec<_>>());
        let runs = assembly_lines(d, plan)?;
        assert!(runs.iter().all(|r| r.skipped.is_none() && r.lines.len() == 3), "{runs:?}");

        let name = assembly_block_name(plan);
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let b = s.doc()?.block(&name).ok_or("no block")?.clone();
        let mut got_arcs: Vec<(f64, String, f64, f64)> = Vec::new();
        let mut lines = 0;
        for e in b.entities.iter() {
            match &e.kind {
                EntityKind::Arc(a) => {
                    assert_eq!((r6(a.center.x), r6(a.center.y)), (3000.0, 1100.0), "{a:?}");
                    got_arcs.push((r6(a.radius), e.common.layer.clone(), r6(a.start.to_degrees()), r6(a.end.to_degrees())));
                }
                EntityKind::Line(_) => lines += 1,
                k => return Err(format!("unexpected {k:?}").into()),
            }
        }
        assert_eq!(lines, runs.len() * 3 - 3, "straight runs keep LINEs");
        got_arcs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let half = |r: f64, l: &str| (r, l.to_string(), -90.0, 90.0);
        assert_eq!(got_arcs, [half(900.0, "GYP"), half(1050.0, "GYP"), half(1120.0, "CLAD")]);

        // A second ASSEMBLYPLAN and an unrelated edit leave the block as it is.
        assert!(block_matches(&b, &assembly_lines(s.doc()?, plan)?, &[]));
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let b = s.doc()?.block(&name).ok_or("no block")?.clone();
        assert!(block_matches(&b, &assembly_lines(s.doc()?, plan)?, &[]));
        s.execute("editview", &json!({ "viewId": null }))?;
        s.execute("layer.new", &json!({ "name": "NOTES", "current": true }))?;
        line(&mut s, [90_000.0, 90_000.0], [91_000.0, 90_000.0])?;
        assert!(Arc::ptr_eq(&b, s.doc()?.block(&name).ok_or("no block")?), "observer saw no difference");
        Ok(())
    }

    #[test]
    fn a_repeat_arrays_along_an_arc_run_as_chords() -> R {
        let (mut s, plan) = d_room()?;
        let detail = detail_id(&s)?;
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        s.execute("layer.new", &json!({ "name": "STUD", "current": true }))?;
        let before = all_handles(&s)?;
        s.execute("pline", &json!({ "vertices": [[100, 50], [138, 50], [138, 200], [100, 200]], "closed": true }))?;
        let stud = all_handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("no stud")?;
        let before = all_handles(&s)?;
        s.execute("dimaligned", &json!({ "p1": [100, 0], "p2": [500, 0], "at": [300, -300] }))?;
        let pitch = all_handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("no dimension")?;
        s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [stud.hex()] }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        crate::maintain::install(&mut s);

        let d = s.doc()?;
        let p = DrawingProject::new(d)?;
        let (graph, _) = graph_and_sides(&p, plan);
        let runs = assembly_lines(d, plan)?;
        let arc =
            runs.iter().find(|r| matches!(graph.runs[r.run].centreline, lamb_core::derivation::walls::RunCurve::Arc { .. })).ok_or("no arc run")?;
        assert_eq!(arc.bands, 3);
        assert!(arc.lines[..3].iter().all(|l| l.arc.is_some()), "bands stay arcs");
        assert_eq!(arc.lines.len(), 3 + 9 * 4, "{arc:?}");
        for l in &arc.lines[3..] {
            assert!(l.arc.is_none() && l.layer == "STUD", "{l:?}");
            for pt in [l.from, l.to] {
                let r = r6((pt.x - 3000.0).hypot(pt.y - 1100.0));
                assert!(r == 1050.0 || r == 900.0, "{pt:?} at {r}");
            }
        }

        let name = assembly_block_name(plan);
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let b = s.doc()?.block(&name).ok_or("no block")?.clone();
        let total: usize = runs.iter().map(|r| r.lines.len()).sum();
        assert_eq!(b.entities.len(), total);
        let arcs = b.entities.iter().filter(|e| matches!(e.kind, EntityKind::Arc(_))).count();
        assert_eq!(arcs, 3, "only the bands are ARCs; the chords are LINEs");
        assert!(block_matches(&b, &assembly_lines(s.doc()?, plan)?, &[]));
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let b2 = s.doc()?.block(&name).ok_or("no block")?.clone();
        assert!(block_matches(&b2, &assembly_lines(s.doc()?, plan)?, &[]));
        assert_eq!(b2.entities.len(), total);
        Ok(())
    }

    #[test]
    fn block_matches_tells_an_arc_from_a_line_and_a_clockwise_arc_is_written_swapped() {
        let cw = AssemblyLine {
            from: Point2 { x: 0.0, y: 1000.0 },
            to: Point2 { x: 1000.0, y: 0.0 },
            layer: "A".into(),
            arc: Some(lamb_core::derivation::assembly::ArcSpan { center: Point2 { x: 0.0, y: 0.0 }, radius: 1000.0, ccw: false }),
        };
        let EntityKind::Arc(a) = line_entity(&cw) else { panic!("not an arc") };
        assert_eq!((r6(a.start.to_degrees()), r6(a.end.to_degrees())), (0.0, 90.0));
        let run = |l: AssemblyLine| RunAssembly { run: 0, detail: None, lines: vec![l], bands: 1, copies: vec![], skipped: None, conflict: None };
        let mut block = Block::new("X");
        block.entities.push(Entity {
            handle: cadcraft_doc::Handle(1),
            common: Common { layer: "A".into(), ..Common::default() },
            kind: line_entity(&cw),
        });
        assert!(block_matches(&block, &[run(cw.clone())], &[]));
        let straight = AssemblyLine { arc: None, ..cw.clone() };
        assert!(!block_matches(&block, &[run(straight)], &[]), "a LINE where an ARC is");
        let bigger = AssemblyLine { arc: Some(lamb_core::derivation::assembly::ArcSpan { radius: 1001.0, ..cw.arc.unwrap() }), ..cw };
        assert!(!block_matches(&block, &[run(bigger)], &[]));
    }

    #[test]
    fn mitre_moves_band_ends_at_undetailed_l_corners_onto_the_bisector() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        let r = s.execute("assemblyplan", &json!({ "viewId": plan.0, "mitre": true }))?;
        assert_eq!(r["lines"], 12, "{r}");
        assert_eq!(read_document(s.doc()?)?.mitre_plans, [plan]);
        let d = s.doc()?;
        let p = DrawingProject::new(d)?;
        let (graph, _) = graph_and_sides(&p, plan);
        let runs = assembly_lines(d, plan)?;
        assert_eq!(horizontal_spans(&runs, &graph, 100.0)?, [(-20.0, -20.0, 5020.0), (50.0, 50.0, 4950.0), (200.0, 200.0, 4800.0)]);
        assert_eq!(horizontal_spans(&runs, &graph, 3900.0)?, [(4020.0, -20.0, 5020.0), (3950.0, 50.0, 4950.0), (3800.0, 200.0, 4800.0)]);
        // The left run's bands meet the bottom ones on y = x.
        let mut met = 0;
        for r in &runs {
            let lamb_core::derivation::walls::RunCurve::Line { from, to } = graph.runs[r.run].centreline else { continue };
            if (from.x - 100.0).abs() < 1.0 && (to.x - 100.0).abs() < 1.0 {
                for l in &r.lines[..r.bands] {
                    let low = if l.from.y < l.to.y { l.from } else { l.to };
                    assert!((low.x - low.y).abs() < 1e-6, "{l:?}");
                    met += 1;
                }
            }
        }
        assert_eq!(met, 3);
        // The block holds the mitred lines.
        let block = block_xy(&s, &assembly_block_name(plan))?;
        assert!(block.iter().any(|l| l.1 == -20.0 && l.0.min(l.2) == -20.0 && l.0.max(l.2) == 5020.0), "{block:?}");

        // Without the option, a fresh fixture spans the centreline's x extent, as before.
        let (s, plan) = room("exterior_face", true)?;
        let d = s.doc()?;
        let p = DrawingProject::new(d)?;
        let (graph, _) = graph_and_sides(&p, plan);
        assert_eq!(
            horizontal_spans(&assembly_lines(d, plan)?, &graph, 100.0)?,
            [(-20.0, 200.0, 4800.0), (50.0, 200.0, 4800.0), (200.0, 200.0, 4800.0)]
        );
        Ok(())
    }

    /// [`room`]'s detail and tag, but with an annex of 200 mm walls built onto the main room's east
    /// wall (outer face x = 5000): its bottom wall rises at 15° from (5000, 1000) to x = 6500, its
    /// top wall is horizontal at y = 2800..3000. The east wall's run is split into exterior, shared
    /// and exterior parts, and both annex walls are T branches with a single exterior side — the
    /// bottom one skewed, so its unmitred bands end on a cut square to the branch, not on the face.
    /// (A partition inside the room is no fixture for this: a run with a space on both sides lays no
    /// face detail, and a square branch's run already ends on the through run's face.)
    fn t_plan() -> T<(Session, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let detail = Id(s.execute("newdetail", &json!({ "name": "D1" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        let r = line(&mut s, [0.0, 0.0], [1000.0, 0.0])?;
        s.execute("detailref", &json!({ "handle": format!("{:X}", r.0), "kind": "exterior_face" }))?;
        s.execute("layer.new", &json!({ "name": "CLAD", "current": true }))?;
        line(&mut s, [0.0, -20.0], [1000.0, -20.0])?;
        s.execute("layer.new", &json!({ "name": "GYP", "current": true }))?;
        line(&mut s, [0.0, 50.0], [1000.0, 50.0])?;
        line(&mut s, [0.0, 200.0], [1000.0, 200.0])?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        let tan = 15f64.to_radians().tan();
        let outer = [
            [0.0, 0.0],
            [5000.0, 0.0],
            [5000.0, 1000.0],
            [6500.0, 1000.0 + 1500.0 * tan],
            [6500.0, 3000.0],
            [5000.0, 3000.0],
            [5000.0, 4000.0],
            [0.0, 4000.0],
        ];
        s.execute("pline", &json!({ "vertices": outer, "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [4800, 200], [4800, 3800], [200, 3800]], "closed": true }))?;
        let k = 1000.0 + 200.0 / 15f64.to_radians().cos();
        let annex = [[5000.0, k], [6300.0, k + 1300.0 * tan], [6300.0, 2800.0], [5000.0, 2800.0]];
        s.execute("pline", &json!({ "vertices": annex, "closed": true }))?;
        s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
        s.execute("detailtag", &json!({ "detailViewId": detail.0, "tip": {"x": 2500.0, "y": 0.0}, "bubble": {"x": 1500.0, "y": -1000.0} }))?;
        Ok((s, plan))
    }

    /// The T junctions of `graph` with their branch run and branch end.
    fn tees(graph: &lamb_core::derivation::wall_junctions::WallGraph) -> Vec<(Point2, usize, lamb_core::derivation::wall_junctions::RunEnd)> {
        graph.junctions.iter().filter_map(|j| tee_butt(j, &graph.runs).map(|(b, _, _)| (j.point, j.arms[b].run, j.arms[b].end))).collect()
    }

    /// The x of each band line's end at `end`.
    fn end_xs(a: &RunAssembly, end: lamb_core::derivation::wall_junctions::RunEnd) -> Vec<f64> {
        use lamb_core::derivation::wall_junctions::RunEnd;
        a.lines[..a.bands].iter().map(|l| r6(if end == RunEnd::Start { l.from.x } else { l.to.x })).collect()
    }

    #[test]
    fn mitre_butts_a_t_branch_s_bands_into_the_through_run_s_face() -> R {
        let (s, plan) = t_plan()?;
        let d = s.doc()?;
        let p = DrawingProject::new(d)?;
        let (graph, _) = graph_and_sides(&p, plan);
        let off = assembly_lines(d, plan)?;
        let tees = tees(&graph);
        assert_eq!(tees.len(), 2, "{:?}", graph.junctions);
        for &(_, run, _) in &tees {
            assert!(off[run].skipped.is_none() && off[run].bands == 3, "{:?}", off[run]);
        }
        // Unmitred, the skewed branch's bands end on a cut square to it, not on the face.
        let skewed = tees.iter().find(|t| end_xs(&off[t.1], t.2).iter().any(|x| (x - 5000.0).abs() > 1.0)).ok_or("no skewed branch")?;

        let (mut s, plan) = t_plan()?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0, "mitre": true }))?;
        let d = s.doc()?;
        let on = assembly_lines(d, plan)?;
        for &(_, run, end) in &tees {
            assert_eq!(end_xs(&on[run], end), [5000.0; 3], "butted on the face x = 5000: {:?}", on[run]);
        }
        assert_ne!(on[skewed.1].lines, off[skewed.1].lines);
        // The through runs' band ends at the T nodes are as with mitre off.
        let branch_runs: Vec<usize> = tees.iter().map(|t| t.1).collect();
        for i in (0..graph.runs.len()).filter(|i| !branch_runs.contains(i)) {
            let near = |a: &RunAssembly| -> Vec<(f64, f64)> {
                a.lines[..a.bands]
                    .iter()
                    .flat_map(|l| [l.from, l.to])
                    .filter(|q| tees.iter().any(|t| (q.x - t.0.x).abs() < 300.0 && (q.y - t.0.y).abs() < 300.0))
                    .map(|q| (r6(q.x), r6(q.y)))
                    .collect()
            };
            assert_eq!(near(&on[i]), near(&off[i]), "run {i}");
        }
        // RESOLVE's notes: the T corners and the L corners read "(mitred)".
        let corners: Vec<Issue> = wall_issues(d)?.into_iter().filter(|c| c.kind == IssueKind::UndetailedCorner).collect();
        for t in &tees {
            let c = corners.iter().find(|c| c.location == t.0).ok_or("no T issue")?;
            assert!(c.note.ends_with("(mitred)"), "{}", c.note);
        }
        assert!(corners.iter().all(|c| c.note.ends_with("(mitred)")), "{corners:?}");
        // Mitre off again leaves the branches as they were.
        s.execute("assemblyplan", &json!({ "viewId": plan.0, "mitre": false }))?;
        let back = assembly_lines(s.doc()?, plan)?;
        for &(_, run, _) in &tees {
            assert_eq!(back[run].lines, off[run].lines);
        }
        Ok(())
    }

    #[test]
    fn corner_tagged_junctions_stay_unmitred_and_mitre_false_or_undo_restores() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        // Two different corner details on the bottom corners: those two junctions are tagged; the
        // top two see both details across their gap and resolve to a conflict, still undetailed.
        let d1 = detail_id(&s)?;
        let d2 = Id(s.execute("newdetail", &json!({ "name": "D2" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        for (detail, tip) in [(d1, (0.0, 0.0)), (d2, (5000.0, 0.0))] {
            s.execute("detailtag", &json!({ "detailViewId": detail.0, "corner": true, "tip": {"x": tip.0, "y": tip.1}, "bubble": {"x": tip.0 - 1000.0, "y": tip.1 - 1000.0} }))?;
        }
        let (_, corners) = resolve_view_details(s.doc()?, plan)?;
        assert_eq!(corners.iter().filter(|c| matches!(c, RunDetail::Tagged(_))).count(), 2, "{corners:?}");
        assert_eq!(corners.iter().filter(|c| matches!(c, RunDetail::Conflict | RunDetail::Untyped)).count(), 2, "{corners:?}");
        let unmitred = {
            s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
            let b = block_xy(&s, &assembly_block_name(plan))?;
            s.undo()?;
            b
        };

        let name = assembly_block_name(plan);
        let n = undo_len(&s)?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0, "mitre": true }))?;
        assert_eq!(undo_len(&s)?, n + 1, "flag and block in one undo step");
        let d = s.doc()?;
        let p = DrawingProject::new(d)?;
        let (graph, _) = graph_and_sides(&p, plan);
        let runs = assembly_lines(d, plan)?;
        assert_eq!(
            horizontal_spans(&runs, &graph, 100.0)?,
            [(-20.0, 200.0, 4800.0), (50.0, 200.0, 4800.0), (200.0, 200.0, 4800.0)],
            "tagged corners"
        );
        assert_eq!(
            horizontal_spans(&runs, &graph, 3900.0)?,
            [(4020.0, -20.0, 5020.0), (3950.0, 50.0, 4950.0), (3800.0, 200.0, 4800.0)],
            "untagged corners"
        );
        let mitred = block_xy(&s, &name)?;
        assert_ne!(mitred, unmitred);

        s.execute("assemblyplan", &json!({ "viewId": plan.0, "mitre": false }))?;
        assert_eq!(block_xy(&s, &name)?, unmitred);
        assert!(read_document(s.doc()?)?.mitre_plans.is_empty());
        s.undo()?;
        assert_eq!(block_xy(&s, &name)?, mitred, "undo restores the mitred block");
        assert_eq!(read_document(s.doc()?)?.mitre_plans, [plan]);
        s.undo()?;
        assert!(s.doc()?.block(&name).is_none());
        assert!(read_document(s.doc()?)?.mitre_plans.is_empty(), "undo removes the flag too");

        let e = s.execute("assemblyplan", &json!({ "viewId": plan.0, "mitre": "yes" })).err().ok_or("accepted a string")?;
        assert!(e.to_string().contains("mitre must be"), "{e}");
        Ok(())
    }

    #[test]
    fn an_unrelated_edit_keeps_the_mitred_block() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        crate::maintain::install(&mut s);
        s.execute("assemblyplan", &json!({ "viewId": plan.0, "mitre": true }))?;
        let name = assembly_block_name(plan);
        let block = s.doc()?.block(&name).ok_or("no block")?.clone();
        s.execute("editview", &json!({ "viewId": null }))?;
        s.execute("layer.new", &json!({ "name": "NOTES", "current": true }))?;
        line(&mut s, [90_000.0, 90_000.0], [91_000.0, 90_000.0])?;
        assert!(Arc::ptr_eq(&block, s.doc()?.block(&name).ok_or("no block")?), "observer saw no difference");
        // Omitting `mitre` keeps the option.
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert_eq!(read_document(s.doc()?)?.mitre_plans, [plan]);
        Ok(())
    }
    fn prompt_default(s: &Session) -> Option<String> {
        s.current_prompt().and_then(|p| p.default)
    }

    #[test]
    fn the_interactive_assemblyplan_matches_the_parameter_form_and_keeps_or_sets_mitre() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        let name = assembly_block_name(plan);
        let n = undo_len(&s)?;
        s.start("assemblyplan")?;
        let p = s.current_prompt().ok_or("no prompt")?;
        assert_eq!(p.display(), "Mitre missing corner details or [Yes/No] <No>:");
        s.input(Input::Enter)?;
        assert!(s.running.is_none());
        assert_eq!(undo_len(&s)?, n + 1);
        assert!(s.log.iter().any(|l| l == "Assembly plan: 12 lines, 0 runs skipped, 0 width conflicts"), "{:?}", s.log);
        let interactive = block_xy(&s, &name)?;
        assert!(read_document(s.doc()?)?.mitre_plans.is_empty(), "Enter keeps the setting");
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert_eq!(block_xy(&s, &name)?, interactive);

        s.start("assemblyplan")?;
        s.input(Input::Text("x".into()))?;
        assert!(s.running.is_some(), "re-asks");
        assert!(s.log.iter().any(|l| l == "Yes or No"), "{:?}", s.log);
        s.input(Input::Text("Y".into()))?;
        assert!(s.running.is_none());
        assert_eq!(read_document(s.doc()?)?.mitre_plans, [plan]);
        let mitred = block_xy(&s, &name)?;
        assert_ne!(mitred, interactive);
        s.execute("assemblyplan", &json!({ "viewId": plan.0, "mitre": true }))?;
        assert_eq!(block_xy(&s, &name)?, mitred);

        s.start("assemblyplan")?;
        assert_eq!(prompt_default(&s).as_deref(), Some("Yes"));
        s.input(Input::Enter)?;
        assert_eq!(read_document(s.doc()?)?.mitre_plans, [plan], "Enter keeps Yes");
        assert_eq!(block_xy(&s, &name)?, mitred);
        Ok(())
    }

    #[test]
    fn the_interactive_wallconflicts_lists_each_conflict_without_an_undo_step() -> R {
        let (mut s, _plan) = room_with("exterior_face", true, 210.0)?;
        let n = undo_len(&s)?;
        s.log.clear();
        s.start("wallconflicts")?;
        assert!(s.running.is_none());
        assert_eq!(undo_len(&s)?, n);
        let lines: Vec<&String> = s.log.iter().filter(|l| l.starts_with("Run ")).collect();
        assert_eq!(lines.len(), 4, "{:?}", s.log);
        assert!(lines.iter().all(|l| l.ends_with(": -10.0mm toward interior")), "{lines:?}");

        let (mut s, _plan) = room_with("exterior_face", true, 200.0)?;
        s.start("wallconflicts")?;
        assert!(s.log.iter().any(|l| l == "No width conflicts"), "{:?}", s.log);
        Ok(())
    }

    #[test]
    fn the_interactive_forms_refuse_without_an_edited_plan_and_change_nothing() -> R {
        let (mut s, _plan) = room_with("exterior_face", true, 210.0)?;
        let detail = detail_id(&s)?;
        for edit in [json!({ "viewId": detail.0 }), json!({})] {
            s.execute("editview", &edit)?;
            if edit == json!({}) {
                assert_eq!(crate::detailtags::edited_plan(&s), Err("Edit a plan view first (EDITVIEW)".to_owned()));
                assert_eq!(edited_wall_view(&s), Err("Edit a plan or section view first (EDITVIEW)".to_owned()));
            }
            for cmd in ["assemblyplan", "wallconflicts", "wallfit"] {
                // WALLFIT is plan-only; the other two take a section too.
                let why = if cmd == "wallfit" { crate::detailtags::edited_plan(&s) } else { edited_wall_view(&s) };
                let why = why.err().ok_or("a plan is edited")?;
                let before = s.doc()?.clone();
                let n = undo_len(&s)?;
                s.log.clear();
                s.start(cmd)?;
                assert!(s.running.is_none(), "{cmd}");
                assert!(s.log.contains(&why), "{cmd}: {:?}", s.log);
                assert_eq!(undo_len(&s)?, n, "{cmd}");
                assert_eq!(*s.doc()?, before, "{cmd}");
            }
        }
        Ok(())
    }
    /// `detailtags.rs`'s section fixture: a plan with section `A-A`, edited, holding two vertical
    /// `WALL-EXT` faces (u 0 and `inner`, z 0..3000) and two horizontal `FLOOR` faces (z 1000 and
    /// 1250, u `inner`..4000), all SETROLE cut; and a vertical detail `SD` with an exterior-face
    /// reference (0,0)–(1000,0), a `GYP` band at y = 50 and a `STUD` repeat (38 × 150 at x 100, pitch
    /// 400), edge-tagged on the wall. Returns (session, section).
    fn section_wall(inner: f64) -> T<(Session, Id)> {
        section_fixture(inner / 2.0, Some((-2000.0, 2000.0)), |s| {
            s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
            s.execute("pline", &json!({ "vertices": [[0, 0], [4000, 0], [4000, 3000], [0, 3000]], "closed": true }))?;
            s.execute(
                "pline",
                &json!({ "vertices": [[inner, 250.0], [4000.0 - inner, 250.0], [4000.0 - inner, 2750.0], [inner, 2750.0]], "closed": true }),
            )?;
            Ok(())
        })
    }

    /// `section_wall`'s plan, section and `SD` detail with the section's cut faces drawn by `faces`,
    /// an `outsidepoint` at `outside` if given, and `SD` edge-tagged at (`tip_u`, 2000).
    fn section_fixture(tip_u: f64, outside: Option<(f64, f64)>, faces: impl FnOnce(&mut Session) -> R) -> T<(Session, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let sd = Id(s.execute("newdetail", &json!({ "name": "SD", "cutDirection": "vertical" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": sd.0 }))?;
        let r = line(&mut s, [0.0, 0.0], [1000.0, 0.0])?;
        s.execute("detailref", &json!({ "handle": r.hex(), "kind": "exterior_face" }))?;
        s.execute("layer.new", &json!({ "name": "GYP", "current": true }))?;
        line(&mut s, [0.0, 50.0], [1000.0, 50.0])?;
        line(&mut s, [0.0, 300.0], [1000.0, 300.0])?;
        s.execute("layer.new", &json!({ "name": "STUD", "current": true }))?;
        let before = all_handles(&s)?;
        s.execute("pline", &json!({ "vertices": [[100, 50], [138, 50], [138, 200], [100, 200]], "closed": true }))?;
        let stud = all_handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("no stud")?;
        let before = all_handles(&s)?;
        s.execute("dimaligned", &json!({ "p1": [100, 0], "p2": [500, 0], "at": [300, -300] }))?;
        let pitch = all_handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("no dimension")?;
        s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [stud.hex()] }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let r =
            s.execute("newsectionline", &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 0}, "cutTo": {"x": 5000, "y": 0} }))?;
        let sec = Id(r["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        faces(&mut s)?;
        let hs: Vec<String> = s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.iter().map(|e| e.handle.hex()).collect();
        s.execute("setrole", &json!({ "handles": hs, "role": "cut" }))?;
        s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
        if let Some((x, y)) = outside {
            s.execute("outsidepoint", &json!({ "point": {"x": x, "y": y} }))?;
        }
        s.execute("detailtag", &json!({ "detailViewId": sd.0, "tip": {"x": tip_u, "y": 2000.0}, "bubble": {"x": -1000.0, "y": 2500.0} }))?;
        Ok((s, sec))
    }

    /// T456's open section: a `WALL-EXT` pair at u 0/300 (z 0..3000) and a `FLOOR` pair at z
    /// 1000/1250 from the wall's inner face to u 4000 — a structure that doesn't close.
    fn open_section(outside: Option<(f64, f64)>) -> T<(Session, Id)> {
        section_fixture(150.0, outside, |s| {
            s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
            s.execute("pline", &json!({ "vertices": [[0, 0], [0, 3000]] }))?;
            s.execute("pline", &json!({ "vertices": [[300, 0], [300, 3000]] }))?;
            s.execute("layer.new", &json!({ "name": "FLOOR", "current": true }))?;
            s.execute("pline", &json!({ "vertices": [[300, 1000], [4000, 1000]] }))?;
            s.execute("pline", &json!({ "vertices": [[300, 1250], [4000, 1250]] }))?;
            Ok(())
        })
    }

    #[test]
    fn an_open_section_takes_its_exterior_side_from_the_outside_point() -> R {
        // Without a point the wall run has no exterior side and is skipped, as before.
        let (s, sec) = open_section(None)?;
        let tagged = section_left_run(&s, sec)?;
        let runs = assembly_lines(s.doc()?, sec)?;
        let r = runs.iter().find(|r| r.run == tagged).ok_or("no tagged run")?;
        assert_eq!(r.skipped.as_deref(), Some("no single exterior side"), "{r:?}");

        // With the point at u -1000: the bands are measured from the wall's u 0 face.
        let (mut s, sec) = open_section(Some((-1000.0, 1500.0)))?;
        let d = s.doc()?;
        let p = DrawingProject::new(d)?;
        let (graph, _) = graph_and_sides(&p, sec);
        let tagged = section_left_run(&s, sec)?;
        let runs = assembly_lines(d, sec)?;
        let r = runs.iter().find(|r| r.run == tagged).ok_or("no tagged run")?;
        assert_eq!((r.skipped.as_deref(), r.conflict), (None, None), "{r:?}");
        let RunCurve::Line { from, to } = graph.runs[tagged].centreline else { return Err("arc run".into()) };
        let len = (to.x - from.x).hypot(to.y - from.y);
        let copies = ((len - 138.0) / 400.0).floor() as usize + 1;
        assert_eq!((r.bands, r.lines.len()), (2, 2 + 4 * copies), "{r:?}");
        let mut us: Vec<f64> = r.lines[..2].iter().map(|l| r6(l.from.x)).collect();
        us.sort_by(f64::total_cmp);
        assert_eq!(us, [50.0, 300.0]);
        assert!(r.lines[2..].iter().all(|l| l.layer == "STUD"));
        let total: usize = runs.iter().map(|r| r.lines.len()).sum();
        let out = s.execute("assemblyplan", &json!({ "viewId": sec.0 }))?;
        assert_eq!(out["lines"].as_u64(), Some(total as u64), "{out}");
        assert_eq!(s.doc()?.block(&assembly_block_name(sec)).ok_or("no block")?.entities.len(), total);
        Ok(())
    }

    #[test]
    fn a_section_detail_lays_its_bands_and_repeats_along_the_section_s_cut_wall() -> R {
        let (mut s, sec) = section_wall(300.0)?;
        let d = s.doc()?;
        let p = DrawingProject::new(d)?;
        let (graph, _) = graph_and_sides(&p, sec);
        let runs = assembly_lines(d, sec)?;
        let tagged = section_left_run(&s, sec)?;
        let r = runs.iter().find(|r| r.run == tagged).ok_or("no tagged run")?;
        assert_eq!((r.skipped.as_deref(), r.conflict), (None, None), "{r:?}");
        let RunCurve::Line { from, to } = graph.runs[tagged].centreline else { return Err("arc run".into()) };
        let len = (to.x - from.x).hypot(to.y - from.y);
        let copies = ((len - 138.0) / 400.0).floor() as usize + 1;
        assert_eq!((r.bands, r.lines.len()), (2, 2 + 4 * copies), "{r:?}");
        // Exterior face at u = 0: the bands at u 50 and 300, along the run's z extent.
        let mut us: Vec<f64> = r.lines[..2].iter().map(|l| r6(l.from.x)).collect();
        us.sort_by(f64::total_cmp);
        assert_eq!(us, [50.0, 300.0]);
        for l in &r.lines[..2] {
            assert_eq!((r6(l.from.x), l.layer.as_str()), (r6(l.to.x), "GYP"));
            let (lo, hi) = (l.from.y.min(l.to.y), l.from.y.max(l.to.y));
            assert_eq!((r6(lo), r6(hi)), (r6(from.y.min(to.y)), r6(from.y.max(to.y))));
        }
        assert!(r.lines[2..].iter().all(|l| l.layer == "STUD"));
        let total: usize = runs.iter().map(|r| r.lines.len()).sum();

        let n = undo_len(&s)?;
        let out = s.execute("assemblyplan", &json!({ "viewId": sec.0 }))?;
        assert_eq!(out["lines"].as_u64(), Some(total as u64), "{out}");
        assert!(total > 0);
        let name = assembly_block_name(sec);
        assert_eq!(s.doc()?.block(&name).ok_or("no block")?.entities.len(), total);
        assert_eq!(model_inserts(s.doc()?, &name).len(), 1);
        assert_eq!(undo_len(&s)?, n + 1);
        Ok(())
    }

    /// T463's joists: [`open_section`] with its host plan placed at a rotated, moved transform, an
    /// outside point below the floor and `SD` also tagged on the floor, so the floor run lays the
    /// stud repeat (pitch 400) as joists. Returns (session, section, plan), editing the section.
    fn joist_section() -> T<(Session, Id, Id)> {
        let (mut s, sec) = open_section(Some((-1000.0, 1500.0)))?;
        s.execute("outsidepoint", &json!({ "point": {"x": 2000.0, "y": 500.0} }))?;
        let doc = read_document(s.doc()?)?;
        let sd = doc.views.iter().find(|v| v.name() == "SD").ok_or("no SD")?.id();
        let plan = doc.views.iter().find(|v| matches!(v, AnyView::Plan(_))).ok_or("no plan")?.id();
        s.execute("detailtag", &json!({ "detailViewId": sd.0, "tip": {"x": 2000.0, "y": 1000.0}, "bubble": {"x": 2500.0, "y": 500.0} }))?;
        let moved = lamb_core::geometry::Transform2D { origin_x: 1000.0, origin_y: 500.0, rotation: 0.25 };
        crate::studgrid::tests::place(&mut s, plan, moved)?;
        Ok((s, sec, plan))
    }

    /// The section's floor run (horizontal centreline) and its laid assembly.
    fn floor_run(d: &Drawing, sec: Id) -> T<RunAssembly> {
        let p = DrawingProject::new(d)?;
        let (graph, _) = graph_and_sides(&p, sec);
        let i = graph
            .runs
            .iter()
            .position(|r| matches!(r.centreline, RunCurve::Line { from, to } if (from.y - to.y).abs() < 1e-9))
            .ok_or("no floor run")?;
        Ok(assembly_lines(d, sec)?.into_iter().find(|r| r.run == i).ok_or("floor run not laid")?)
    }

    #[test]
    fn a_section_s_joists_take_the_project_grid_and_its_cut_walls_do_not() -> R {
        let (mut s, sec, plan) = joist_section()?;
        let wall = section_left_run(&s, sec)?;
        let wall_lines =
            |d: &Drawing| -> T<Vec<AssemblyLine>> { Ok(assembly_lines(d, sec)?.into_iter().find(|r| r.run == wall).ok_or("no wall run")?.lines) };
        let (wall_before, floor_before) = (wall_lines(s.doc()?)?, floor_run(s.doc()?, sec)?);
        assert_eq!(floor_before.skipped, None, "{floor_before:?}");
        assert!(floor_before.lines.len() > floor_before.bands, "the floor lays joists");

        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("studgrid", &json!({ "point": {"x": 13.0, "y": 7.0}, "pitch": 400 }))?;
        let d = s.doc()?;
        assert_eq!(wall_lines(d)?, wall_before, "a wall cut in section never takes the grid");
        let floor = floor_run(d, sec)?;
        assert_ne!(floor.lines, floor_before.lines, "the joists moved onto the grid");
        let (g, pitch) = crate::studgrid::project_grid(d).ok_or("no grid")?;
        let p = DrawingProject::new(d)?;
        use lamb_core::derivation::read::ProjectRead;
        let (v, host) = (p.view(sec).ok_or("no section")?, p.view(plan).ok_or("no plan")?);
        let basis = lamb_core::derivation::projection::vertical_basis(&v, &host)?;
        let u0 = lamb_core::derivation::projection::project_to_u(&basis, g);
        let copies: Vec<&[AssemblyLine]> = floor.lines[floor.bands..].chunks(4).collect();
        assert!(!copies.is_empty());
        for c in copies {
            let xs = c.iter().flat_map(|l| [l.from.x, l.to.x]);
            let (lo, hi) = (xs.clone().fold(f64::INFINITY, f64::min), xs.fold(f64::NEG_INFINITY, f64::max));
            let u = lamb_core::derivation::projection::vertical_local_to_uz(&v, Point2 { x: (lo + hi) / 2.0, y: 1125.0 }).u;
            let m = (u - u0).rem_euclid(pitch);
            assert!(m < 1e-6 || pitch - m < 1e-6, "joist at u {u} off the grid through u {u0}");
        }
        Ok(())
    }

    #[test]
    fn a_stud_move_point_on_a_section_s_floor_run_moves_one_joist() -> R {
        let (mut s, sec, plan) = joist_section()?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("studgrid", &json!({ "point": {"x": 13.0, "y": 7.0}, "pitch": 400 }))?;
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        let centres = |d: &Drawing| -> T<Vec<f64>> {
            let f = floor_run(d, sec)?;
            let mut xs: Vec<f64> = f.lines[f.bands..]
                .chunks(4)
                .map(|c| {
                    let xs = c.iter().flat_map(|l| [l.from.x, l.to.x]);
                    let (lo, hi) = (xs.clone().fold(f64::INFINITY, f64::min), xs.fold(f64::NEG_INFINITY, f64::max));
                    ((lo + hi) / 2.0 * 1e6).round() / 1e6
                })
                .collect();
            xs.sort_by(f64::total_cmp);
            Ok(xs)
        };
        let before = centres(s.doc()?)?;
        assert!(before.len() > 2);
        let y = {
            let p = DrawingProject::new(s.doc()?)?;
            let (graph, _) = graph_and_sides(&p, sec);
            graph
                .runs
                .iter()
                .find_map(|r| match r.centreline {
                    RunCurve::Line { from, to } if (from.y - to.y).abs() < 1e-9 => Some(from.y),
                    _ => None,
                })
                .ok_or("no floor run")?
        };
        let x0 = before[before.len() / 2];
        s.execute("studmove", &json!({ "point": {"x": x0 + 60.0, "y": y} }))?;
        let after = centres(s.doc()?)?;
        let mut want: Vec<f64> = before.iter().copied().filter(|&x| x != x0).chain([((x0 + 60.0) * 1e6).round() / 1e6]).collect();
        want.sort_by(f64::total_cmp);
        assert_eq!(after, want);
        Ok(())
    }

    #[test]
    fn moving_the_grid_in_a_plan_relays_a_section_s_block_in_that_undo_step() -> R {
        let (mut s, sec, plan) = joist_section()?;
        crate::maintain::install(&mut s);
        s.execute("assemblyplan", &json!({ "viewId": sec.0 }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let h = s.execute("studgrid", &json!({ "point": {"x": 13.0, "y": 7.0}, "pitch": 400 }))?["handle"].as_str().ok_or("no handle")?.to_owned();
        let name = assembly_block_name(sec);
        let before = s.doc()?.block(&name).ok_or("no block")?.clone();
        let n = undo_len(&s)?;
        s.execute("move", &json!({ "handles": [h], "delta": [100, 0] }))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        let d = s.doc()?;
        let block = d.block(&name).ok_or("no block")?;
        assert!(!Arc::ptr_eq(&before, block), "rewritten");
        assert!(block_matches(block, &assembly_lines(d, sec)?, &corner_assembly(d, sec)?));
        s.undo()?;
        assert_eq!(**s.doc()?.block(&name).ok_or("no block")?, *before, "undo restores the section's block");
        Ok(())
    }

    /// The section's run whose centreline is vertical near u = 0 (the tagged wall).
    fn section_left_run(s: &Session, sec: Id) -> T<usize> {
        let p = DrawingProject::new(s.doc()?)?;
        let (graph, _) = graph_and_sides(&p, sec);
        graph
            .runs
            .iter()
            .position(|r| matches!(r.centreline, RunCurve::Line { from, to } if from.x < 1000.0 && (from.x - to.x).abs() < 1e-9))
            .ok_or_else(|| "no left wall run".into())
    }

    #[test]
    fn moving_a_section_face_rewrites_the_section_s_assembly_block_in_the_same_undo_step() -> R {
        let (mut s, sec) = section_wall(300.0)?;
        crate::maintain::install(&mut s);
        s.execute("assemblyplan", &json!({ "viewId": sec.0 }))?;
        let name = assembly_block_name(sec);
        let before = s.doc()?.block(&name).ok_or("no block")?.clone();
        let outer = s
            .doc()?
            .block(&view_block_name(sec))
            .ok_or("no section block")?
            .entities
            .iter()
            .find(|e| e.common.layer == "WALL-EXT" && to_polyline(e).is_ok_and(|c| c.polyline.start.x == 0.0 && c.polyline.start.y == 0.0))
            .ok_or("no outer face")?
            .handle;
        let n = undo_len(&s)?;
        s.execute("move", &json!({ "handles": [outer.hex()], "delta": [-20, 0] }))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        let d = s.doc()?;
        let block = d.block(&name).ok_or("no block")?;
        assert!(!Arc::ptr_eq(&before, block), "rewritten");
        assert!(block_matches(block, &assembly_lines(d, sec)?, &corner_assembly(d, sec)?));
        // The exterior face moved to u = −20: the GYP bands sit at u 30 and 280.
        let mut us: Vec<f64> = block
            .entities
            .iter()
            .filter(|e| e.common.layer == "GYP")
            .filter_map(|e| if let EntityKind::Line(l) = &e.kind { Some(r6(l.a.x)) } else { None })
            .collect();
        us.sort_by(f64::total_cmp);
        us.dedup();
        assert!(us.contains(&30.0) && us.contains(&280.0), "{us:?}");

        s.undo()?;
        assert_eq!(undo_len(&s)?, n);
        assert!(Arc::ptr_eq(&before, s.doc()?.block(&name).ok_or("no block")?), "undo restores the block");
        Ok(())
    }

    #[test]
    fn wallconflicts_and_resolve_report_a_section_s_width_mismatch() -> R {
        let (mut s, sec) = section_wall(310.0)?;
        let left = section_left_run(&s, sec)?;
        let n = undo_len(&s)?;
        let r = s.execute("wallconflicts", &json!({ "viewId": sec.0 }))?;
        assert_eq!(undo_len(&s)?, n, "read-only");
        let c = r["conflicts"].as_array().ok_or("no conflicts")?;
        let mine = c.iter().find(|x| x["run"].as_u64() == Some(left as u64)).ok_or("no conflict on the tagged wall")?;
        assert_eq!(mine["text"], "-10.0mm toward interior", "{r}");

        s.execute("assemblyplan", &json!({ "viewId": sec.0 }))?;
        let r = s.execute("resolve", &json!({}))?;
        let id = format!("width_conflict:{sec}:{left}");
        let issue = r["issues"].as_array().ok_or("no issues")?.iter().find(|i| i["id"] == json!(id)).ok_or("not in RESOLVE")?;
        assert_eq!(issue["viewId"], json!(sec.0));
        assert!(issue["note"].as_str().is_some_and(|n| n.ends_with("-10.0mm toward interior")), "{issue}");
        Ok(())
    }

    #[test]
    fn wallrun_names_a_section_wall_and_wallfit_stays_plan_only() -> R {
        let (mut s, sec) = section_wall(310.0)?;
        let left = section_left_run(&s, sec)?;
        let r = s.execute("wallrun", &json!({ "viewId": sec.0, "point": {"x": 155, "y": 1500} }))?;
        assert_eq!(r["run"].as_u64(), Some(left as u64), "{r}");
        assert_eq!((r["detail"]["state"].as_str(), r["detail"]["name"].as_str()), (Some("tagged"), Some("SD")), "{r}");
        assert_eq!(r["conflict"], "-10.0mm toward interior");

        let n = undo_len(&s)?;
        let e = s.execute("wallfit", &json!({ "viewId": sec.0 })).err().ok_or("wallfit accepted a section")?;
        assert!(e.to_string().contains("Not a plan view"), "{e}");
        s.log.clear();
        s.start("wallfit")?;
        assert!(s.running.is_none());
        assert_eq!(crate::detailtags::edited_plan(&s).err().as_ref().map(|w| s.log.contains(w)), Some(true), "{:?}", s.log);
        assert_eq!(undo_len(&s)?, n);
        Ok(())
    }

    /// A corner detail `CD` (vertical when `vertical`): a reference (−1000,0)–(0,0) of `kind`, a
    /// `CRN-A` line (0,0)–(0,1000) and a `CRN-B` band (−1000,200)–(0,200) putting the wall side at
    /// +y; corner-tagged at `tip` in `view`, which is edited afterwards.
    fn corner_detail(s: &mut Session, view: Id, kind: &str, vertical: bool, tip: (f64, f64)) -> T<Id> {
        let mut p = json!({ "name": "CD" });
        if vertical {
            p["cutDirection"] = json!("vertical");
        }
        let cd = Id(s.execute("newdetail", &p)?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": cd.0 }))?;
        let r = line(s, [-1000.0, 0.0], [0.0, 0.0])?;
        s.execute("detailref", &json!({ "handle": r.hex(), "kind": kind }))?;
        s.execute("layer.new", &json!({ "name": "CRN-A", "current": true }))?;
        line(s, [0.0, 0.0], [0.0, 1000.0])?;
        s.execute("layer.new", &json!({ "name": "CRN-B", "current": true }))?;
        line(s, [-1000.0, 200.0], [0.0, 200.0])?;
        s.execute("editview", &json!({ "viewId": view.0 }))?;
        s.execute("layer.current", &json!({ "name": "TAGS" }))?;
        s.execute(
            "detailtag",
            &json!({ "detailViewId": cd.0, "corner": true, "tip": {"x": tip.0, "y": tip.1}, "bubble": {"x": tip.0 - 1000.0, "y": tip.1 - 1000.0} }),
        )?;
        Ok(cd)
    }

    /// The corner lines of `corners` as rounded tuples with layers, in write order.
    fn corner_xy(corners: &[CornerAssembly]) -> Vec<(f64, f64, f64, f64, String)> {
        corners.iter().flat_map(|c| &c.lines).map(|l| (r6(l.from.x), r6(l.from.y), r6(l.to.x), r6(l.to.y), l.layer.clone())).collect()
    }

    #[test]
    fn a_corner_detail_is_placed_at_each_corner_it_resolves_to_after_the_run_lines() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        crate::maintain::install(&mut s);
        let cd = corner_detail(&mut s, plan, "exterior_face", false, (0.0, 0.0))?;
        let n = undo_len(&s)?;
        let r = s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        assert_eq!((r["corners"].as_u64(), r["lines"].as_u64(), r["skipped"].clone()), (Some(4), Some(20), json!([])), "{r}");
        let corners = corner_assembly(s.doc()?, plan)?;
        assert_eq!(corners.len(), 4);
        assert!(corners.iter().all(|c| c.detail == Some(cd) && c.skipped.is_none() && c.lines.len() == 2), "{corners:?}");
        // Each corner's CRN-A line starts at its outer corner and runs 1000 along a box face.
        let outer = [(0.0, 0.0), (5000.0, 0.0), (5000.0, 4000.0), (0.0, 4000.0)];
        let mut seen = Vec::new();
        for l in corner_xy(&corners).iter().filter(|l| l.4 == "CRN-A") {
            assert!(outer.contains(&(l.0, l.1)), "{l:?}");
            assert_eq!((l.2 - l.0).hypot(l.3 - l.1), 1000.0, "{l:?}");
            assert!((l.0 == l.2 && (l.0 == 0.0 || l.0 == 5000.0)) || (l.1 == l.3 && (l.1 == 0.0 || l.1 == 4000.0)), "on a face: {l:?}");
            seen.push((l.0, l.1));
        }
        seen.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        assert_eq!(seen, [(0.0, 0.0), (0.0, 4000.0), (5000.0, 0.0), (5000.0, 4000.0)]);
        // The bottom-left corner exactly: the reference runs down the left face to (0,0).
        let all = corner_xy(&corners);
        assert!(all.contains(&(0.0, 0.0, 1000.0, 0.0, "CRN-A".into())), "{all:?}");
        assert!(all.contains(&(200.0, 1000.0, 200.0, 0.0, "CRN-B".into())), "{all:?}");
        // Every corner reaches 900 from its node along each arm: the bands start there (T462).
        assert_trimmed(s.doc()?, plan)?;
        let runs = assembly_lines(s.doc()?, plan)?;
        let bottom: Vec<_> = runs.iter().flat_map(|r| &r.lines[..r.bands]).filter(|l| l.from.y == l.to.y && l.from.y < 300.0).collect();
        assert_eq!(bottom.len(), 3, "{bottom:?}");
        assert!(bottom.iter().all(|l| r6(l.from.x.min(l.to.x)) == 1000.0 && r6(l.from.x.max(l.to.x)) == 4000.0), "{bottom:?}");
        // The block: the 12 run lines, then the corner lines in junction order.
        let block = block_xy(&s, &assembly_block_name(plan))?;
        assert_eq!(block.len(), 20);
        assert!(block[..12].iter().all(|l| l.4 == "CLAD" || l.4 == "GYP"), "{block:?}");
        assert_eq!(block[12..], all[..]);

        // MOVE the wall's faces 20 left: the corners re-place in that undo step.
        let faces: Vec<String> = s
            .doc()?
            .block(&view_block_name(plan))
            .ok_or("no plan block")?
            .entities
            .iter()
            .filter(|e| e.common.layer == "WALL-EXT")
            .map(|e| e.handle.hex())
            .collect();
        assert_eq!(faces.len(), 2);
        let before = s.doc()?.block(&assembly_block_name(plan)).ok_or("no block")?.clone();
        let n = undo_len(&s)?;
        s.execute("move", &json!({ "handles": faces, "delta": [-20, 0] }))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        let block = block_xy(&s, &assembly_block_name(plan))?;
        assert!(block.contains(&(-20.0, 0.0, 980.0, 0.0, "CRN-A".into())), "{block:?}");
        assert!(block.iter().any(|l| l.4 == "CRN-A" && l.0 == 4980.0 && l.1 == 0.0), "{block:?}");
        assert_eq!(block.len(), 20);
        assert_trimmed(s.doc()?, plan)?;
        assert!(block_matches(
            s.doc()?.block(&assembly_block_name(plan)).ok_or("no block")?,
            &assembly_lines(s.doc()?, plan)?,
            &corner_assembly(s.doc()?, plan)?
        ));
        s.undo()?;
        assert!(Arc::ptr_eq(&before, s.doc()?.block(&assembly_block_name(plan)).ok_or("no block")?), "undo restores the block");
        Ok(())
    }

    #[test]
    fn an_interior_face_corner_reference_is_skipped_with_its_reason() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        corner_detail(&mut s, plan, "interior_face", false, (0.0, 0.0))?;
        let r = s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert_eq!((r["corners"].as_u64(), r["lines"].as_u64()), (Some(0), Some(12)), "{r}");
        let skipped = r["skipped"].as_array().ok_or("no skipped")?;
        assert_eq!(skipped.len(), 4, "{r}");
        assert!(skipped.iter().all(|x| x["junction"].is_u64() && x["reason"] == "corner reference kind not placed yet"), "{r}");
        Ok(())
    }

    #[test]
    fn a_section_places_a_corner_detail_at_its_tagged_junction() -> R {
        let (mut s, sec) = section_wall(300.0)?;
        corner_detail(&mut s, sec, "exterior_face", true, (0.0, 0.0))?;
        let corners = corner_assembly(s.doc()?, sec)?;
        assert!(!corners.is_empty(), "no corner resolved");
        let placed: Vec<_> = corners.iter().filter(|c| c.skipped.is_none()).collect();
        assert!(!placed.is_empty(), "{corners:?}");
        let all = corner_xy(&corners);
        assert!(all.iter().any(|l| l.4 == "CRN-A" && (l.0, l.1) == (0.0, 0.0)), "{all:?}");
        let runs: usize = assembly_lines(s.doc()?, sec)?.iter().map(|r| r.lines.len()).sum();
        let r = s.execute("assemblyplan", &json!({ "viewId": sec.0 }))?;
        assert_eq!(r["corners"].as_u64(), Some(placed.len() as u64), "{r}");
        assert_eq!(r["lines"].as_u64(), Some((runs + all.len()) as u64), "{r}");
        assert_trimmed(s.doc()?, sec)?;
        assert!(trims_something(s.doc()?, sec)?, "no section band was clipped at a corner");

        // MOVE the outer face: the trimmed block re-lays in that one undo step.
        crate::maintain::install(&mut s);
        let outer = s
            .doc()?
            .block(&view_block_name(sec))
            .ok_or("no section block")?
            .entities
            .iter()
            .find(|e| e.common.layer == "WALL-EXT" && to_polyline(e).is_ok_and(|c| c.polyline.start.x == 0.0 && c.polyline.start.y == 0.0))
            .ok_or("no outer face")?
            .handle;
        let n = undo_len(&s)?;
        s.execute("move", &json!({ "handles": [outer.hex()], "delta": [-20, 0] }))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        let d = s.doc()?;
        assert!(block_matches(d.block(&assembly_block_name(sec)).ok_or("no block")?, &assembly_lines(d, sec)?, &corner_assembly(d, sec)?));
        assert_trimmed(d, sec)?;
        Ok(())
    }

    /// The along-arm distances of `l`'s ends from junction `j`'s node into arm `i`, or `None` for an
    /// arc arm.
    fn along(j: &lamb_core::derivation::wall_junctions::Junction, runs: &[WallRun], i: usize, l: &AssemblyLine) -> Option<[f64; 2]> {
        let u = arm_direction(j, runs, i)?;
        let a = |p: Point2| (p.x - j.point.x) * u.x + (p.y - j.point.y) * u.y;
        Some([a(l.from), a(l.to)])
    }

    /// Every placed corner of `view` with each arm's reach: no line end (band or repeat copy) of that
    /// arm's run lies inside the reach, and each run's `copies` sum to its repeat lines.
    fn assert_trimmed(d: &Drawing, view: Id) -> R {
        let (graph, _) = graph_and_sides(&DrawingProject::new(d)?, view);
        let runs = assembly_lines(d, view)?;
        for r in &runs {
            assert_eq!(r.bands + r.copies.iter().sum::<usize>(), r.lines.len(), "run {}", r.run);
        }
        for c in corner_assembly(d, view)?.iter().filter(|c| c.skipped.is_none()) {
            let j = &graph.junctions[c.junction];
            for (i, reach) in corner_reach(j, &graph.runs, &c.lines).into_iter().enumerate() {
                for l in &runs[j.arms[i].run].lines {
                    let Some(a) = along(j, &graph.runs, i, l) else { continue };
                    assert!(a.iter().all(|a| *a >= reach - TOLERANCE.coincidence_mm), "junction {} arm {i} reach {reach}: {l:?}", c.junction);
                }
            }
        }
        Ok(())
    }

    /// Whether some band end of `view` sits exactly at a placed corner's reach — the trim clipped it.
    fn trims_something(d: &Drawing, view: Id) -> T<bool> {
        let (graph, _) = graph_and_sides(&DrawingProject::new(d)?, view);
        let runs = assembly_lines(d, view)?;
        for c in corner_assembly(d, view)?.iter().filter(|c| c.skipped.is_none()) {
            let j = &graph.junctions[c.junction];
            for (i, reach) in corner_reach(j, &graph.runs, &c.lines).into_iter().enumerate() {
                let r = &runs[j.arms[i].run];
                let at_reach = |l: &AssemblyLine| along(j, &graph.runs, i, l).is_some_and(|a| a.iter().any(|a| (a - reach).abs() < 1e-6));
                if reach > 0.0 && r.lines[..r.bands].iter().any(at_reach) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// `room` with a 38-wide closed stud repeated at `pitch` in its detail (T458's stud room).
    fn stud_room(pitch: f64) -> T<(Session, Id)> {
        let (mut s, plan) = room("exterior_face", true)?;
        let detail = read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id();
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        s.execute("layer.new", &json!({ "name": "STUD", "current": true }))?;
        let before = all_handles(&s)?;
        s.execute("pline", &json!({ "vertices": [[100, 50], [138, 50], [138, 200], [100, 200]], "closed": true }))?;
        let stud = all_handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("no stud")?;
        let before = all_handles(&s)?;
        s.execute("dimaligned", &json!({ "p1": [100, 0], "p2": [100.0 + pitch, 0], "at": [300, -300] }))?;
        let dim = all_handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("no dimension")?;
        s.execute("repeat", &json!({ "dimension": dim.hex(), "handles": [stud.hex()] }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        Ok((s, plan))
    }

    /// Each run's repeat copies as rounded line tuples, per copy.
    fn copies_of(runs: &[RunAssembly]) -> Vec<Vec<Vec<(f64, f64, f64, f64)>>> {
        runs.iter()
            .map(|r| {
                let mut at = r.bands;
                r.copies
                    .iter()
                    .map(|n| {
                        let c = r.lines[at..at + n].iter().map(|l| (r6(l.from.x), r6(l.from.y), r6(l.to.x), r6(l.to.y))).collect();
                        at += n;
                        c
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_placed_corner_drops_the_stud_copies_inside_its_reach_and_keeps_the_rest() -> R {
        let (mut s, plan) = stud_room(406.4)?;
        let plain = assembly_lines(s.doc()?, plan)?;
        assert!(plain.iter().all(|r| r.skipped.is_none() && !r.copies.is_empty() && r.copies.iter().all(|n| *n == 4)), "{plain:?}");
        corner_detail(&mut s, plan, "exterior_face", false, (0.0, 0.0))?;
        let d = s.doc()?;
        assert_trimmed(d, plan)?;
        // Every corner reaches 900 from its node (100 in from each outer face) along both arms, so a
        // copy is kept exactly when each of its points has x in 1000..4000 (bottom and top runs) or
        // y in 1000..3000 (side runs); kept copies are unchanged.
        let ok = |x: f64, y: f64| (1000.0 - 1e-6..=4000.0 + 1e-6).contains(&x) || (1000.0 - 1e-6..=3000.0 + 1e-6).contains(&y);
        let want: Vec<Vec<_>> =
            copies_of(&plain).into_iter().map(|run| run.into_iter().filter(|c| c.iter().all(|v| ok(v.0, v.1) && ok(v.2, v.3))).collect()).collect();
        let got = copies_of(&assembly_lines(d, plan)?);
        assert_eq!(got, want);
        assert!(got.iter().zip(copies_of(&plain)).all(|(g, p)| !g.is_empty() && g.len() < p.len()), "every run lost copies at its corners");
        Ok(())
    }
}

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
//! junction's bisector (lamb-core's [`junction_bisector`], [`mitre_end`]); repeats are not mitred.
//! A view with a stud grid ([`crate::studgrid`], LAMB T458) lays each straight run's repeat whose
//! pitch is near the grid's on that grid's lines.
//! Not the AI-line sweep in [`crate::sweeps`].

use std::sync::Arc;

use cadcraft_doc::{Arc as ArcEntity, Block, Common, Drawing, Entity, EntityKind, Insert, Line, Space};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result as EngineResult, Session, Step};
use cadcraft_geom::{Vec2, Vec3};
use lamb_core::derivation::assembly::{
    AssemblyLine, FaceRef, WidthConflict, detail_bands, detail_frame, junction_bisector, lay_bands, lay_repeat, mitre_end, width_conflict,
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

/// One run's lines from `detail` (bands, then each repeat's copies), how many of them are bands, and
/// its width conflict, or why not.
fn run_lines(
    d: &Drawing,
    view: Id,
    detail: Id,
    run: &lamb_core::derivation::walls::WallRun,
    sides: &lamb_core::derivation::wall_spaces::RunSides,
) -> Result<(Vec<AssemblyLine>, usize, Option<WidthConflict>), String> {
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
    let grid = crate::studgrid::stud_grid(d, view);
    for rep in detail_repeats(d, detail)? {
        laid.extend(lay_repeat(run, sides, face, &frame, &member_segments(d, detail, &rep.members), rep.pitch, grid.as_ref())?);
    }
    Ok((laid, n, width_conflict(run, face, &bands)))
}

/// Every run of `plan`'s wall graph, in graph order, with its detail's bands laid along it — mitred
/// at undetailed two-arm junctions when `plan` is in the document's `mitre_plans`.
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
                Ok((lines, bands, conflict)) => RunAssembly { run: i, detail: Some(detail), lines, bands, skipped: None, conflict },
                Err(e) => skip(&e, Some(detail)),
            }
        })
        .collect();
    if mitre {
        for (j, corner) in graph.junctions.iter().zip(&corners) {
            if !matches!(corner, RunDetail::Untyped | RunDetail::Conflict) {
                continue;
            }
            let Some(bisector) = junction_bisector(j) else { continue };
            for arm in &j.arms {
                let Some(r) = runs.get_mut(arm.run).filter(|r| r.skipped.is_none()) else { continue };
                let n = r.bands;
                for l in &mut r.lines[..n] {
                    mitre_end(l, arm.end, j.point, bisector);
                }
            }
        }
    }
    Ok(runs)
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
/// no corner detail that has a laid arm (by junction). Locations are view-local.
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
            let suffix = if mitred { " (mitred)" } else { "" };
            out.push(issue(
                IssueKind::UndetailedCorner,
                format!("undetailed_corner:{plan}:{j}"),
                junction.point,
                face_ids(junction.arms.iter().filter_map(|a| graph.runs.get(a.run))),
                format!("Corner at ({:.0}, {:.0}) has no corner detail{suffix}", junction.point.x, junction.point.y),
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
/// line of `runs`.
fn replace_block(d: &mut Drawing, plan: Id, runs: &[RunAssembly]) -> Result<(), String> {
    let name = assembly_block_name(plan);
    d.blocks.insert(name.clone(), Arc::new(Block::new(&name)));
    let space = Space::Block(name);
    for l in runs.iter().flat_map(|r| &r.lines) {
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

/// Replaces the block [`assembly_block_name`]`(plan)` with one LINE per laid assembly line (an
/// empty block when every run is skipped), and gives it its model-space INSERT on its own layer
/// when it has none. `plan` must be a plan or section view; nothing is touched on error.
pub fn write_assembly_block(d: &mut Drawing, plan: Id) -> Result<Vec<RunAssembly>, String> {
    check_wall_view(d, plan)?;
    let runs = assembly_lines(d, plan)?;
    replace_block(d, plan, &runs)?;
    ensure_assembly_insert(d, plan);
    Ok(runs)
}

/// `true` when `block` holds exactly the lines of `runs`, in write order: same count, each pair on
/// the same layer and of the same kind ([`line_entity`]) — a LINE's ends within
/// [`TOLERANCE`]`.coincidence_mm`; an ARC's centre and radius within that, its start and end angles
/// within `TOLERANCE.angle_deg`.
fn block_matches(block: &Block, runs: &[RunAssembly]) -> bool {
    let tol = TOLERANCE.coincidence_mm;
    let near = |p: cadcraft_geom::Vec3, q: cadcraft_geom::Vec3| (p.x - q.x).hypot(p.y - q.y) <= tol;
    let same_angle = |a: f64, b: f64| {
        let d = (a - b).rem_euclid(std::f64::consts::TAU);
        d.min(std::f64::consts::TAU - d) <= TOLERANCE.angle_deg.to_radians()
    };
    let mut want = runs.iter().flat_map(|r| &r.lines);
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
            match assembly_lines(d, plan) {
                Ok(runs) if !block_matches(block, &runs) => rewrite.push((plan, runs)),
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
    for (plan, runs) in &rewrite {
        if let Err(e) = replace_block(&mut after, *plan, runs) {
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
/// `{block, lines, skipped: [{run, reason}], conflicts: [{run, delta, toward, text}]}`.
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
    let runs = write_assembly_block(d, plan).map_err(other)?;
    let lines: usize = runs.iter().map(|r| r.lines.len()).sum();
    let skipped: Vec<Value> = runs.iter().filter_map(|r| r.skipped.as_ref().map(|why| json!({ "run": r.run, "reason": why }))).collect();
    s.echo(format!("{lines} assembly lines, {} runs skipped", skipped.len()));
    Ok(json!({ "block": assembly_block_name(plan), "lines": lines, "skipped": skipped, "conflicts": conflicts_json(&runs) }))
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
                let skipped = r["skipped"].as_array().map_or(0, Vec::len);
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
        assert_eq!(r, json!({ "block": name, "lines": 12, "skipped": [], "conflicts": [] }));
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
        assert!(block_matches(&b, &assembly_lines(s.doc()?, plan)?));
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let b = s.doc()?.block(&name).ok_or("no block")?.clone();
        assert!(block_matches(&b, &assembly_lines(s.doc()?, plan)?));
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
        assert!(block_matches(&b, &assembly_lines(s.doc()?, plan)?));
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let b2 = s.doc()?.block(&name).ok_or("no block")?.clone();
        assert!(block_matches(&b2, &assembly_lines(s.doc()?, plan)?));
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
        let run = |l: AssemblyLine| RunAssembly { run: 0, detail: None, lines: vec![l], bands: 1, skipped: None, conflict: None };
        let mut block = Block::new("X");
        block.entities.push(Entity {
            handle: cadcraft_doc::Handle(1),
            common: Common { layer: "A".into(), ..Common::default() },
            kind: line_entity(&cw),
        });
        assert!(block_matches(&block, &[run(cw.clone())]));
        let straight = AssemblyLine { arc: None, ..cw.clone() };
        assert!(!block_matches(&block, &[run(straight)]), "a LINE where an ARC is");
        let bigger = AssemblyLine { arc: Some(lamb_core::derivation::assembly::ArcSpan { radius: 1001.0, ..cw.arc.unwrap() }), ..cw };
        assert!(!block_matches(&block, &[run(bigger)]));
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

    #[test]
    fn corner_tagged_junctions_stay_unmitred_and_mitre_false_or_undo_restores() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        let unmitred = {
            s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
            let b = block_xy(&s, &assembly_block_name(plan))?;
            s.undo()?;
            b
        };
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
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [4000, 0], [4000, 3000], [0, 3000]], "closed": true }))?;
        s.execute(
            "pline",
            &json!({ "vertices": [[inner, 250.0], [4000.0 - inner, 250.0], [4000.0 - inner, 2750.0], [inner, 2750.0]], "closed": true }),
        )?;
        let hs: Vec<String> = s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.iter().map(|e| e.handle.hex()).collect();
        s.execute("setrole", &json!({ "handles": hs, "role": "cut" }))?;
        s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
        s.execute("outsidepoint", &json!({ "point": {"x": -2000, "y": 2000} }))?;
        s.execute("detailtag", &json!({ "detailViewId": sd.0, "tip": {"x": inner / 2.0, "y": 2000.0}, "bubble": {"x": -1000.0, "y": 2500.0} }))?;
        Ok((s, sec))
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
        assert!(block_matches(block, &assembly_lines(d, sec)?));
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
}

//! Opening a plan's or section's assembly plan and asking where its lines came from
//! (`docs/07-architecture/cadcraft-integration.md` §10 "Generation", LAMB T482).
//!
//! - `assemblyview {viewId?}` (`asv`, LAMB ▸ Walls & Framing ▸ Assembly View): edit inside the view's
//!   assembly block `LAMB_ASSEMBLY_<id>` (written by ASSEMBLYPLAN, [`crate::assembly`]); the view
//!   defaults to the edited plan or section. Not an undo step; EDITVIEW leaves it.
//! - The block stays derived: [`crate::assembly::assembly_observer`] rewrites it after any command
//!   that changed it, in that command's undo step, so an edit there nets to nothing;
//!   [`assembly_edit_observer`] says so, pointing back at the view.
//! - `assemblysource {point:{x,y}, viewId?, aperture?}`: the source of the assembly line nearest a
//!   point ([`assembly_source`]) — its run or junction, detail, band/repeat/corner, layer and the
//!   wall faces it was laid from. An idle click in the assembly space echoes the same
//!   ([`pick_assembly`], from [`crate::viewpick::idle_pick`]).
//! - While the space is an assembly block, [`draw`] (an overlay, via
//!   [`crate::overlays::lamb_overlay`]) shows in ghost grey what the assembly plan does not lay
//!   (§9 "Greedy resolution": anything untyped "renders ghosted"): the two face lines of every run
//!   skipped for any reason (untyped, conflict, unplaceable detail), and a cross at every junction
//!   whose resolved corner detail was skipped. The block and plots are unchanged.

use std::sync::Arc;

use cadcraft_color::Rgb;
use cadcraft_doc::{Drawing, Handle, Space};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result as EngineResult, Session, Step};
use cadcraft_geom::{Bounds2, Vec2};
use cadcraft_render::{DisplayList, Kind, Options};
use lamb_core::derivation::assembly::AssemblyLine;
use lamb_core::derivation::walls::{RunCurve, WallRun};
use lamb_core::geometry::Point2;
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::assembly::{assembly_lines, block_matches_view, check_wall_view, corner_assembly, face_ids};
use crate::commands::{bad, edited_view, enter_space, id_value, other, point_param};
use crate::detailtags::graph_and_sides;
use crate::project::DrawingProject;
use crate::schema::{assembly_block_name, read_document};

/// Where one assembly line came from.
#[derive(Clone, Debug, PartialEq)]
pub struct AssemblySource {
    pub view: Id,
    /// The wall-graph run, for a band or repeat line.
    pub run: Option<usize>,
    /// The wall-graph junction, for a corner line.
    pub junction: Option<usize>,
    pub detail: Option<Id>,
    /// `"band"`, `"repeat"` or `"corner"`.
    pub kind: &'static str,
    pub layer: String,
    /// The face entity ids the line was laid from: the run's, or every arm run's for a corner.
    pub sources: Vec<Id>,
}

impl AssemblySource {
    pub fn to_json(&self) -> Value {
        json!({
            "view": self.view.0,
            "run": self.run,
            "junction": self.junction,
            "detail": self.detail.map(|d| d.0),
            "kind": self.kind,
            "layer": self.layer,
            "sources": self.sources.iter().map(|i| i.0).collect::<Vec<_>>(),
        })
    }
}

fn dist(p: Point2, q: Point2) -> f64 {
    (p.x - q.x).hypot(p.y - q.y)
}

/// Distance from `p` to an assembly line: its segment, or for an arc line the arc between its ends.
fn dist_to_line(p: Point2, l: &AssemblyLine) -> f64 {
    let Some(arc) = l.arc else {
        let (dx, dy) = (l.to.x - l.from.x, l.to.y - l.from.y);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 { (((p.x - l.from.x) * dx + (p.y - l.from.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
        return dist(p, Point2 { x: l.from.x + t * dx, y: l.from.y + t * dy });
    };
    let tau = std::f64::consts::TAU;
    let angle = |q: Point2| (q.y - arc.center.y).atan2(q.x - arc.center.x);
    // Counter-clockwise sweep from `a` to `b`.
    let (a, b) = if arc.ccw { (angle(l.from), angle(l.to)) } else { (angle(l.to), angle(l.from)) };
    let sweep = (b - a).rem_euclid(tau);
    let at = (angle(p) - a).rem_euclid(tau);
    if at <= sweep { (dist(p, arc.center) - arc.radius).abs() } else { dist(p, l.from).min(dist(p, l.to)) }
}

/// The source of the assembly line of `view` nearest `p` within `aperture` (ties: the first in
/// block write order — each run's bands then its repeat copies, then the corners), recomputed from
/// [`assembly_lines`] and [`corner_assembly`]; `None` when no line is that near or the view does
/// not derive.
pub fn assembly_source(d: &Drawing, view: Id, p: Point2, aperture: f64) -> Option<AssemblySource> {
    let runs = assembly_lines(d, view).ok()?;
    let corners = corner_assembly(d, view).ok()?;
    let mut best: Option<(f64, AssemblySource)> = None;
    let mut consider = |l: &AssemblyLine, make: &dyn Fn() -> AssemblySource| {
        let dd = dist_to_line(p, l);
        if dd <= aperture && best.as_ref().is_none_or(|(b, _)| dd < *b) {
            best = Some((dd, make()));
        }
    };
    let dp = DrawingProject::new(d).ok()?;
    let (graph, _) = graph_and_sides(&dp, view);
    for r in &runs {
        let sources = graph.runs.get(r.run).map(|run| face_ids([run])).unwrap_or_default();
        for (i, l) in r.lines.iter().enumerate() {
            let kind = if i < r.bands { "band" } else { "repeat" };
            consider(l, &|| AssemblySource {
                view,
                run: Some(r.run),
                junction: None,
                detail: r.detail,
                kind,
                layer: l.layer.clone(),
                sources: sources.clone(),
            });
        }
    }
    for c in &corners {
        let arms =
            graph.junctions.get(c.junction).map(|j| j.arms.iter().filter_map(|a| graph.runs.get(a.run)).collect::<Vec<_>>()).unwrap_or_default();
        let sources = face_ids(arms);
        for l in &c.lines {
            consider(l, &|| AssemblySource {
                view,
                run: None,
                junction: Some(c.junction),
                detail: c.detail,
                kind: "corner",
                layer: l.layer.clone(),
                sources: sources.clone(),
            });
        }
    }
    best.map(|(_, s)| s)
}

/// The view whose assembly block is the session's space, while that view exists.
pub fn assembly_space_view(s: &Session) -> Option<Id> {
    let Space::Block(name) = s.space() else { return None };
    let id = Id(name.strip_prefix("LAMB_ASSEMBLY_")?.parse().ok()?);
    let doc = read_document(s.doc().ok()?).ok()?;
    (assembly_block_name(id) == name && doc.views.iter().any(|v| v.id() == id)).then_some(id)
}

fn view_name(d: &Drawing, id: Id) -> String {
    read_document(d).ok().and_then(|doc| doc.views.iter().find(|v| v.id() == id).map(|v| v.name().to_owned())).unwrap_or_else(|| id.0.to_string())
}

/// The command-line text for a source: `Run 2 of "Ground": detail "D1", band on GYP`, or
/// `Corner at junction 1 of "Ground": detail "CD"`; `no detail` when it has none.
pub fn describe(d: &Drawing, src: &AssemblySource) -> String {
    let view = view_name(d, src.view);
    let detail = src.detail.map_or_else(|| "no detail".to_owned(), |id| format!("detail \"{}\"", view_name(d, id)));
    match (src.run, src.junction) {
        (_, Some(j)) => format!("Corner at junction {} of \"{view}\": {detail}", j + 1),
        (run, None) => format!("Run {} of \"{view}\": {detail}, {} on {}", run.unwrap_or(0) + 1, src.kind, src.layer),
    }
}

/// The idle-click step: in an assembly space, the line under `p` within `aperture` echoes its
/// source ([`describe`]) and consumes the click; `false` (nothing echoed) otherwise.
pub fn pick_assembly(s: &mut Session, p: Vec2, aperture: f64) -> bool {
    let Some(view) = assembly_space_view(s) else { return false };
    let msg = {
        let Ok(d) = s.doc() else { return false };
        let Some(src) = assembly_source(d, view, Point2 { x: p.x, y: p.y }, aperture) else { return false };
        describe(d, &src)
    };
    s.echo(msg);
    true
}

/// The after-command observer, run just before [`crate::assembly::assembly_observer`]: when the
/// space is an assembly block, the command changed that block and the block no longer equals its
/// derivation (so it is about to be regenerated), echo
/// `The assembly plan is derived: edit "<view>" or its details.`
pub fn assembly_edit_observer(s: &mut Session, before: &Arc<Drawing>) {
    let Some(view) = assembly_space_view(s) else { return };
    let msg = {
        let Ok(d) = s.doc() else { return };
        let name = assembly_block_name(view);
        if before.block(&name) == d.block(&name) || block_matches_view(d, view) {
            return;
        }
        format!("The assembly plan is derived: edit \"{}\" or its details.", view_name(d, view))
    };
    s.echo(msg);
}

/// ASSEMBLYVIEW `{viewId?}`: edit inside the view's assembly block (default: the edited plan or
/// section). Refused, changing nothing, for a detail view, an unknown id, or a view without one.
/// Returns `{block}`.
pub fn assemblyview(s: &mut Session, p: &Value) -> EngineResult<Value> {
    let view = match p.get("viewId").filter(|v| !v.is_null()) {
        Some(v) => id_value(v).ok_or_else(|| other("No such view"))?,
        None => edited_view(s).ok_or_else(|| other("Not editing a plan or section view"))?,
    };
    let d = s.doc()?;
    check_wall_view(d, view).map_err(other)?;
    let name = assembly_block_name(view);
    let label = view_name(d, view);
    if d.block(&name).is_none() {
        return Err(other(format!("No assembly plan for \"{label}\" — run ASSEMBLYPLAN")));
    }
    enter_space(s, Space::Block(name.clone()))?;
    s.echo(format!("Assembly plan of \"{label}\" (read-only). EDITVIEW to return."));
    Ok(json!({ "block": name }))
}

/// ASSEMBLYSOURCE `{point:{x,y}, viewId?, aperture?}`: [`assembly_source`] as JSON, or `null`.
/// The view defaults to the assembly space's, else the edited view; `aperture` to 50 mm.
pub fn assemblysource(s: &mut Session, p: &Value) -> EngineResult<Value> {
    const CMD: &str = "assemblysource";
    let point = point_param(CMD, p, "point")?;
    let view = match p.get("viewId").filter(|v| !v.is_null()) {
        Some(v) => id_value(v).ok_or_else(|| other("No such view"))?,
        None => assembly_space_view(s).or_else(|| edited_view(s)).ok_or_else(|| other("Not editing a plan or section view"))?,
    };
    let aperture = match p.get("aperture") {
        None | Some(Value::Null) => 50.0,
        Some(v) => v.as_f64().filter(|a| a.is_finite() && *a >= 0.0).ok_or_else(|| bad(CMD, "`aperture` must be a non-negative number"))?,
    };
    let d = s.doc()?;
    check_wall_view(d, view).map_err(other)?;
    Ok(assembly_source(d, view, point, aperture).map_or(Value::Null, |src| src.to_json()))
}

/// The assembly-space ghost (TS ghost grey).
pub const GHOST_COLOR: Rgb = Rgb(150, 150, 150);
/// Arc face lines are sampled at this many segments per full turn (at least [`GHOST_MIN_SEGMENTS`]).
pub(crate) const GHOST_SEGMENTS_PER_TURN: f64 = 64.0;
pub(crate) const GHOST_MIN_SEGMENTS: usize = 8;

/// The view whose assembly block `space` is, when that view is a plan or section holding one.
fn assembly_view_of(d: &Drawing, space: &Space) -> Option<Id> {
    let Space::Block(name) = space else { return None };
    let id = Id(name.strip_prefix("LAMB_ASSEMBLY_")?.parse().ok()?);
    (assembly_block_name(id) == *name && d.block(name).is_some() && check_wall_view(d, id).is_ok()).then_some(id)
}

/// A run's two face lines: its centreline offset by ±`width / 2` (an arc run: concentric arcs).
fn face_lines(run: &WallRun) -> [Vec<Vec2>; 2] {
    let h = run.width / 2.0;
    match run.centreline {
        RunCurve::Line { from, to } => {
            let (dx, dy) = (to.x - from.x, to.y - from.y);
            let len = dx.hypot(dy);
            let (nx, ny) = if len > 0.0 { (-dy / len * h, dx / len * h) } else { (0.0, 0.0) };
            let side = |k: f64| vec![Vec2::new(from.x + k * nx, from.y + k * ny), Vec2::new(to.x + k * nx, to.y + k * ny)];
            [side(1.0), side(-1.0)]
        }
        RunCurve::Arc { center, radius, start_angle, sweep } => {
            let n = ((sweep.abs() / std::f64::consts::TAU * GHOST_SEGMENTS_PER_TURN).ceil() as usize).max(GHOST_MIN_SEGMENTS);
            let arc = |r: f64| {
                (0..=n)
                    .map(|i| {
                        let t = start_angle + sweep * i as f64 / n as f64;
                        Vec2::new(center.x + r * t.cos(), center.y + r * t.sin())
                    })
                    .collect()
            };
            [arc(radius + h), arc(radius - h)]
        }
    }
}

/// The assembly-space ghost overlay: in `Space::Block(assembly_block_name(id))` of a plan or
/// section with that block, every skipped run's two face lines and a cross at every skipped
/// corner's junction (half-size: its widest arm's width / 2), as polylines in [`GHOST_COLOR`].
/// Empty in any other space or when the view does not derive.
pub fn draw(d: &Drawing, space: &Space, _opts: &Options) -> DisplayList {
    let mut list = DisplayList { bounds: Bounds2::EMPTY, ..DisplayList::default() };
    let Some(view) = assembly_view_of(d, space) else { return list };
    let (Ok(runs), Ok(corners)) = (assembly_lines(d, view), corner_assembly(d, view)) else { return list };
    let Ok(dp) = DrawingProject::new(d) else { return list };
    let (graph, _) = graph_and_sides(&dp, view);
    let ghost = |list: &mut DisplayList, handle: Handle, pts: Vec<Vec2>| {
        for p in &pts {
            list.bounds.add(*p);
        }
        crate::overlays::push(list, handle, GHOST_COLOR, Kind::Polyline, &pts);
    };
    for r in runs.iter().filter(|r| r.skipped.is_some()) {
        let Some(run) = graph.runs.get(r.run) else { continue };
        let handle = Handle(run.faces[0].from.entity_id.0);
        for pts in face_lines(run) {
            ghost(&mut list, handle, pts);
        }
    }
    for c in corners.iter().filter(|c| c.skipped.is_some()) {
        let Some(j) = graph.junctions.get(c.junction) else { continue };
        let arms: Vec<&WallRun> = j.arms.iter().filter_map(|a| graph.runs.get(a.run)).collect();
        let h = arms.iter().map(|r| r.width).fold(0.0, f64::max) / 2.0;
        let handle = arms.first().map_or(Handle(0), |r| Handle(r.faces[0].from.entity_id.0));
        let (x, y) = (j.point.x, j.point.y);
        ghost(&mut list, handle, vec![Vec2::new(x - h, y - h), Vec2::new(x + h, y + h)]);
        ghost(&mut list, handle, vec![Vec2::new(x - h, y + h), Vec2::new(x + h, y - h)]);
    }
    list
}

/// ASSEMBLYVIEW's interactive form: no prompt; the edited plan or section's assembly plan.
struct AssemblyViewM;

impl Interactive for AssemblyViewM {
    fn name(&self) -> &'static str {
        "assemblyview"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Assembly view", Accept::TEXT)
    }
    fn begin(&mut self, s: &mut Session) -> EngineResult<Step> {
        if let Err(e) = assemblyview(s, &json!({})) {
            s.echo(e.to_string());
        }
        Ok(Step::Done)
    }
    fn input(&mut self, _s: &mut Session, _i: Input) -> EngineResult<Step> {
        Ok(Step::Done)
    }
}

pub(crate) fn assemblyview_machine(_s: &Session) -> EngineResult<Box<dyn Interactive>> {
    Ok(Box::new(AssemblyViewM))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assembly::tests::{corner_detail, room, undo_len};
    use crate::schema::view_block_name;
    use cadcraft_doc::EntityKind;
    use cadcraft_engine::idle_pick::IdlePicked;
    use lamb_core::model::AnyView;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;

    fn detail_id(s: &Session) -> std::result::Result<Id, Box<dyn std::error::Error>> {
        Ok(read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id())
    }

    #[test]
    fn assemblyview_enters_the_block_and_refuses_a_detail_or_a_plan_without_one() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        let plan_space = s.space();
        let e = s.execute("assemblyview", &json!({})).err().ok_or("no block accepted")?;
        assert!(e.to_string().contains("No assembly plan for \"Ground\" — run ASSEMBLYPLAN"), "{e}");
        assert_eq!(s.space(), plan_space);
        let detail = detail_id(&s)?;
        let e = s.execute("assemblyview", &json!({ "viewId": detail.0 })).err().ok_or("detail accepted")?;
        assert!(e.to_string().contains("Not a plan or section view"), "{e}");
        assert!(s.execute("assemblyview", &json!({ "viewId": 999_999 })).is_err());
        assert_eq!(s.space(), plan_space);

        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let n = undo_len(&s)?;
        let r = s.execute("assemblyview", &json!({}))?;
        assert_eq!(r, json!({ "block": assembly_block_name(plan) }));
        assert_eq!(s.space(), Space::Block(assembly_block_name(plan)));
        assert_eq!(assembly_space_view(&s), Some(plan));
        assert_eq!(undo_len(&s)?, n, "not an undo step");
        assert!(s.log.iter().any(|l| l == "Assembly plan of \"Ground\" (read-only). EDITVIEW to return."), "{:?}", s.log);
        Ok(())
    }

    #[test]
    fn an_erase_inside_the_assembly_plan_is_regenerated_away_with_a_pointer_back() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        crate::maintain::install(&mut s);
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        s.execute("assemblyview", &json!({ "viewId": plan.0 }))?;
        let name = assembly_block_name(plan);
        let first = s.doc()?.block(&name).ok_or("no block")?.entities.iter().next().ok_or("empty block")?.handle;
        s.execute("erase", &json!({ "handles": [first.hex()] }))?;
        let d = s.doc()?;
        assert!(block_matches_view(d, plan), "regenerated to the derivation");
        assert_eq!(d.block(&name).ok_or("no block")?.entities.len(), 12);
        assert!(s.log.iter().any(|l| l == "The assembly plan is derived: edit \"Ground\" or its details."), "{:?}", s.log);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        assert_eq!(edited_view(&s), Some(plan));
        Ok(())
    }

    #[test]
    fn assemblysource_names_a_band_a_repeat_a_corner_and_nothing_in_open_space() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        let detail = detail_id(&s)?;
        // A repeated stud in the detail (as assembly.rs's repeat test).
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        s.execute("layer.new", &json!({ "name": "STUD", "current": true }))?;
        let handles = |s: &Session| -> std::result::Result<Vec<cadcraft_doc::Handle>, Box<dyn std::error::Error>> {
            let d = s.doc()?;
            Ok(d.blocks.values().flat_map(|b| b.entities.iter().map(|e| e.handle)).collect())
        };
        let before = handles(&s)?;
        s.execute("pline", &json!({ "vertices": [[100, 50], [138, 50], [138, 200], [100, 200]], "closed": true }))?;
        let stud = handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("no stud")?;
        let before = handles(&s)?;
        s.execute("dimaligned", &json!({ "p1": [100, 0], "p2": [500, 0], "at": [300, -300] }))?;
        let pitch = handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("no dimension")?;
        s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [stud.hex()] }))?;
        let cd = corner_detail(&mut s, plan, "exterior_face", false, (0.0, 0.0))?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;

        let d = s.doc()?;
        let runs = assembly_lines(d, plan)?;
        let p = DrawingProject::new(d)?;
        let (graph, _) = graph_and_sides(&p, plan);
        let bottom = runs
            .iter()
            .find(|r| matches!(graph.runs[r.run].centreline, lamb_core::derivation::walls::RunCurve::Line { from, to } if from.y < 2000.0 && (from.y - to.y).abs() < 1e-9))
            .ok_or("no bottom run")?;
        let faces: Vec<u64> = face_ids([&graph.runs[bottom.run]]).iter().map(|i| i.0).collect();
        assert_eq!(faces.len(), 2);

        // The CLAD band at y = −20, mid-run.
        let r = s.execute("assemblysource", &json!({ "viewId": plan.0, "point": {"x": 2500.0, "y": -21.0} }))?;
        assert_eq!(r["kind"], "band", "{r}");
        assert_eq!(r["run"], json!(bottom.run));
        assert_eq!(r["detail"], json!(detail.0));
        assert_eq!(r["layer"], "CLAD");
        assert_eq!(r["sources"], json!(faces));
        assert_eq!(r["junction"], Value::Null);

        // A stud copy's edge across the wall (its long edges lie on the GYP bands, which win ties).
        let stud_line = bottom.lines[bottom.bands..].iter().find(|l| (l.from.x - l.to.x).abs() < 1e-9).ok_or("no stud edge")?;
        let mid = Point2 { x: (stud_line.from.x + stud_line.to.x) / 2.0, y: (stud_line.from.y + stud_line.to.y) / 2.0 };
        let r = s.execute("assemblysource", &json!({ "viewId": plan.0, "point": {"x": mid.x, "y": mid.y}, "aperture": 1.0 }))?;
        assert_eq!((r["kind"].as_str(), r["layer"].as_str(), r["run"].as_u64()), (Some("repeat"), Some("STUD"), Some(bottom.run as u64)), "{r}");

        // The bottom-left corner's CRN-A line (0,0)–(1000,0) — clear of the band ends at 1000.
        let corners = corner_assembly(s.doc()?, plan)?;
        let c = corners.iter().find(|c| c.lines.iter().any(|l| l.layer == "CRN-A" && l.from.x == 0.0 && l.from.y == 0.0)).ok_or("no corner")?;
        let r = s.execute("assemblysource", &json!({ "viewId": plan.0, "point": {"x": 500.0, "y": 1.0}, "aperture": 5.0 }))?;
        assert_eq!(r["kind"], "corner", "{r}");
        assert_eq!(r["junction"], json!(c.junction));
        assert_eq!(r["detail"], json!(cd.0));
        assert_eq!(r["run"], Value::Null);
        // Both arm runs' faces, deduped: the room's two closed polylines carry every run.
        assert_eq!(r["sources"], json!(faces), "{r}");

        let r = s.execute("assemblysource", &json!({ "viewId": plan.0, "point": {"x": 2500.0, "y": 2000.0} }))?;
        assert_eq!(r, Value::Null);
        Ok(())
    }

    #[test]
    fn an_idle_click_in_the_assembly_space_echoes_its_run_and_falls_through_elsewhere() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        // Outside an assembly space: no assembly step.
        let n = s.log.len();
        assert!(!pick_assembly(&mut s, Vec2::new(2500.0, -20.0), 10.0));
        assert_eq!(s.log.len(), n);

        s.execute("assemblyview", &json!({}))?;
        let picked = crate::viewpick::idle_pick(&mut s, Vec2::new(2500.0, -19.0), 10.0);
        assert!(matches!(picked, Some(IdlePicked::Consumed)), "{picked:?}");
        let last = s.log.last().ok_or("no echo")?;
        assert!(last.starts_with("Run ") && last.ends_with(" of \"Ground\": detail \"D1\", band on CLAD"), "{last}");
        // A miss falls through to the cut-line pick (none here).
        assert!(crate::viewpick::idle_pick(&mut s, Vec2::new(2500.0, 2000.0), 10.0).is_none());
        // Every block entity is a LINE.
        assert!(s.doc()?.block(&assembly_block_name(plan)).ok_or("no block")?.entities.iter().all(|e| matches!(e.kind, EntityKind::Line(_))));
        Ok(())
    }

    #[test]
    fn arc_lines_measure_to_the_arc_between_their_ends() {
        use lamb_core::derivation::assembly::ArcSpan;
        let c = Point2 { x: 0.0, y: 0.0 };
        let l = AssemblyLine {
            from: Point2 { x: 10.0, y: 0.0 },
            to: Point2 { x: 0.0, y: 10.0 },
            layer: "A".into(),
            arc: Some(ArcSpan { center: c, radius: 10.0, ccw: true }),
        };
        let r = std::f64::consts::FRAC_1_SQRT_2 * 12.0;
        assert!((dist_to_line(Point2 { x: r, y: r }, &l) - 2.0).abs() < 1e-9);
        assert!((dist_to_line(Point2 { x: 0.0, y: -10.0 }, &l) - 200f64.sqrt()).abs() < 1e-9, "outside the sweep: nearest end");
        let cw = AssemblyLine { from: l.to, to: l.from, arc: Some(ArcSpan { center: c, radius: 10.0, ccw: false }), ..l.clone() };
        assert!((dist_to_line(Point2 { x: r, y: r }, &cw) - 2.0).abs() < 1e-9);
    }

    /// The ghost-grey prims of `lamb_overlay` in `space`, as point lists.
    fn ghosts(s: &Session, space: &Space) -> std::result::Result<Vec<Vec<Vec2>>, Box<dyn std::error::Error>> {
        let list = crate::overlays::lamb_overlay(s.doc()?, space, &Options::default());
        Ok(list.prims.iter().filter(|p| p.color == GHOST_COLOR).map(|p| list.points(p).to_vec()).collect())
    }

    #[test]
    fn an_untagged_wall_draws_its_face_lines_ghosted_in_the_assembly_space_only() -> R {
        let (mut s, plan) = room("exterior_face", false)?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let asm = Space::Block(assembly_block_name(plan));
        let g = ghosts(&s, &asm)?;
        let runs = assembly_lines(s.doc()?, plan)?;
        assert_eq!(runs.len(), 4);
        assert!(runs.iter().all(|r| r.skipped.as_deref() == Some("untyped")));
        assert_eq!(g.len(), 8, "two face lines per run: {g:?}");
        // Every ghost is an axis-aligned segment on the outer (0/5000, 0/4000) or inner (200/4800,
        // 200/3800) face.
        for pts in &g {
            let [a, b] = pts[..] else { return Err(format!("not a segment: {pts:?}").into()) };
            let on = if (a.y - b.y).abs() < 1e-9 {
                [0.0, 200.0, 3800.0, 4000.0].iter().any(|y| (a.y - y).abs() < 1e-9)
            } else {
                (a.x - b.x).abs() < 1e-9 && [0.0, 200.0, 4800.0, 5000.0].iter().any(|x| (a.x - x).abs() < 1e-9)
            };
            assert!(on, "{pts:?}");
        }
        // Bounds cover the ghost.
        let list = crate::overlays::lamb_overlay(s.doc()?, &asm, &Options::default());
        assert!(list.bounds.min.x <= 0.0 + 1e-9 && list.bounds.max.y >= 4000.0 - 1e-9, "{:?}", list.bounds);

        // Not in the plan's own block, nor in model space.
        assert!(ghosts(&s, &Space::Block(view_block_name(plan)))?.is_empty());
        assert!(ghosts(&s, &Space::Model)?.is_empty());

        // Tagged: nothing ghosted.
        let (mut s, plan) = room("exterior_face", true)?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert!(ghosts(&s, &Space::Block(assembly_block_name(plan)))?.is_empty());
        Ok(())
    }

    #[test]
    fn a_corner_that_cannot_be_placed_draws_a_cross_at_each_junction_it_resolves_to() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        corner_detail(&mut s, plan, "grid", false, (0.0, 0.0))?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let d = s.doc()?;
        let corners = corner_assembly(d, plan)?;
        let skipped: Vec<_> = corners.iter().filter(|c| c.skipped.is_some()).collect();
        assert_eq!(skipped.len(), 4, "{corners:?}");
        let p = DrawingProject::new(d)?;
        let (graph, _) = graph_and_sides(&p, plan);
        let g = ghosts(&s, &Space::Block(assembly_block_name(plan)))?;
        assert_eq!(g.len(), 8, "a two-segment cross per junction: {g:?}");
        for (pair, c) in g.chunks(2).zip(&skipped) {
            let j = graph.junctions[c.junction].point;
            for seg in pair {
                let mid = (seg[0] + seg[1]) * 0.5;
                assert!((mid.x - j.x).abs() < 1e-9 && (mid.y - j.y).abs() < 1e-9, "{seg:?} at {j:?}");
                // Half-size 100 (a 200 wide wall): each arm of the cross spans 200 in x and y.
                assert!(((seg[0].x - seg[1].x).abs() - 200.0).abs() < 1e-9 && ((seg[0].y - seg[1].y).abs() - 200.0).abs() < 1e-9, "{seg:?}");
            }
        }

        // A placed corner draws none.
        let (mut s, plan) = room("exterior_face", true)?;
        corner_detail(&mut s, plan, "exterior_face", false, (0.0, 0.0))?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        assert!(corner_assembly(s.doc()?, plan)?.iter().all(|c| c.skipped.is_none()));
        assert!(ghosts(&s, &Space::Block(assembly_block_name(plan)))?.is_empty());
        Ok(())
    }

    #[test]
    fn an_arc_run_ghosts_as_two_concentric_arcs() {
        use lamb_core::model::LinkAnchor;
        let a = LinkAnchor { entity_id: Id(7), segment_index: 0, segment_param: 0.0 };
        let face = lamb_core::derivation::walls::RunFace { from: a.clone(), to: a };
        let run = WallRun {
            view_id: Id(1),
            faces: [face.clone(), face],
            width: 20.0,
            centreline: RunCurve::Arc { center: Point2 { x: 0.0, y: 0.0 }, radius: 100.0, start_angle: 0.0, sweep: std::f64::consts::FRAC_PI_2 },
        };
        let [outer, inner] = face_lines(&run);
        assert_eq!(outer.len(), GHOST_SEGMENTS_PER_TURN as usize / 4 + 1);
        assert!(outer.iter().all(|p| (p.len() - 110.0).abs() < 1e-9) && inner.iter().all(|p| (p.len() - 90.0).abs() < 1e-9));
        let last = outer[outer.len() - 1];
        assert!(last.x.abs() < 1e-9 && (last.y - 110.0).abs() < 1e-9, "{last:?}");
    }
}

//! The selected detail tag's highlight (LAMB T616): with exactly one detail tag selected, its view
//! draws what the automation tied to it, in [`ATTENTION_COLOR`] — both face lines of every wall run
//! laying its detail (so a concentric "other side" it grabbed shows), the outside face stronger;
//! and for a corner, opening or wall-end tag its arms' faces and the detail's lines placed at its
//! target, which shows which way the detail is oriented.
//!
//! The selection arrives through the canvas selection observer ([`selection_changed`], fork patch
//! 30); only the tag's handle is stored (session UI state, never saved), and the geometry is
//! computed at draw time ([`highlight`]), so an edit while selected redraws right. Nothing in plots.

use std::sync::Mutex;

use cadcraft_doc::{Drawing, Handle, Space};
use cadcraft_geom::{Bounds2, Vec2};
use cadcraft_render::{DisplayList, Kind, Options};
use lamb_core::derivation::assembly::AssemblyLine;
use lamb_core::derivation::detail_tags::RunDetail;
use lamb_core::derivation::wall_spaces::{RunSides, outside_on_left};
use lamb_core::derivation::walls::{RunCurve, WallRun};
use lamb_core::geometry::Point2;
use lamb_core::model::{CutDirection, Id};

use crate::assembly::{corner_assembly, end_assembly, opening_assembly};
use crate::detailtags::{attach_bridge, attach_free_end, attach_junction, graph_and_sides, resolve_view_details, tags_in, view_direction};
use crate::overlays::halo::ATTENTION_COLOR;
use crate::overlays::{push, view_id};
use crate::project::DrawingProject;
use crate::schema::read_document;

const OUTSIDE_TRANSPARENCY: u8 = 30;
const INSIDE_TRANSPARENCY: u8 = 65;
/// Lineweight floor (mm), the halo's.
const MIN_LW: f32 = 0.7;
/// Arc sampling step (degrees).
const STEP_DEG: f64 = 1.0;

/// The selected tag: its view and handle.
static SELECTED: Mutex<Option<(Id, Handle)>> = Mutex::new(None);

fn selected() -> std::sync::MutexGuard<'static, Option<(Id, Handle)>> {
    SELECTED.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The view whose block holds detail tag `h`, if `h` is one.
fn tag_view(d: &Drawing, h: Handle) -> Option<Id> {
    let doc = read_document(d).ok()?;
    doc.views.iter().map(|v| v.id()).find(|&v| tags_in(d, v).is_ok_and(|ts| ts.iter().any(|(x, _, _)| *x == h)))
}

/// The selection observer: store the tag when exactly one selected entity is a detail tag, else
/// clear; bumps the overlays on any change.
pub fn selection_changed(d: &Drawing, sel: &[Handle]) {
    let now = match sel {
        [h] => tag_view(d, *h).map(|v| (v, *h)),
        _ => None,
    };
    let mut s = selected();
    if *s != now {
        *s = now;
        drop(s);
        cadcraft_render::bump_overlays();
    }
}

/// What one tag governs, view-local: face polylines (`true`: the run's outside face) and the
/// detail's placed lines.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Highlight {
    pub faces: Vec<(Vec<Point2>, bool)>,
    pub placed: Vec<AssemblyLine>,
}

/// Points along `c` offset `off` to the left of its travel (an arc sampled every [`STEP_DEG`]).
fn offset_curve(c: &RunCurve, off: f64) -> Vec<Point2> {
    match *c {
        RunCurve::Line { from, to } => {
            let (dx, dy) = (to.x - from.x, to.y - from.y);
            let len = dx.hypot(dy);
            if len <= 0.0 {
                return Vec::new();
            }
            let (nx, ny) = (-dy / len * off, dx / len * off);
            vec![Point2 { x: from.x + nx, y: from.y + ny }, Point2 { x: to.x + nx, y: to.y + ny }]
        }
        RunCurve::Arc { center, radius, start_angle, sweep } => {
            // Left of a counter-clockwise travel is toward the centre.
            let r = radius - off * sweep.signum();
            arc_points(center, r, start_angle, sweep)
        }
    }
}

fn arc_points(center: Point2, r: f64, start: f64, sweep: f64) -> Vec<Point2> {
    let n = (sweep.abs().to_degrees() / STEP_DEG).ceil().max(1.0) as usize;
    (0..=n)
        .map(|i| {
            let a = start + sweep * i as f64 / n as f64;
            Point2 { x: center.x + r * a.cos(), y: center.y + r * a.sin() }
        })
        .collect()
}

/// Run `i`'s two faces, the outside one `true` (both `false` when undecided).
fn run_faces(out: &mut Highlight, runs: &[WallRun], sides: &[RunSides], i: usize) {
    let Some(run) = runs.get(i) else { return };
    let left = sides.get(i).and_then(outside_on_left);
    let h = run.width / 2.0;
    out.faces.push((offset_curve(&run.centreline, h), left == Some(true)));
    out.faces.push((offset_curve(&run.centreline, -h), left == Some(false)));
}

fn resolves_to(x: &RunDetail, key: &str) -> bool {
    matches!(x, RunDetail::Tagged(id) | RunDetail::Spread(id) | RunDetail::Gap(id) if id == key)
}

/// What tag `tag` in `view` governs: an edge tag's every run laying its detail; a corner tag's
/// junction arms and placed corner; an opening tag's two jamb runs and placed jambs (in a section,
/// the end nearer its tip); a wall-end tag's run and placed termination. Empty when `tag` is no
/// tag of `view` or attaches to nothing.
pub(crate) fn highlight(d: &Drawing, view: Id, tag: Handle) -> Highlight {
    let mut out = Highlight::default();
    let Ok(tags) = tags_in(d, view) else { return out };
    let Some((_, tip, t)) = tags.into_iter().find(|(h, _, _)| *h == tag) else { return out };
    let Ok(p) = DrawingProject::new(d) else { return out };
    let (graph, sides) = graph_and_sides(&p, view);
    let key = t.detail_view_id.0.to_string();
    if !t.corner {
        let Ok((runs, _)) = resolve_view_details(d, view) else { return out };
        for (i, r) in runs.iter().enumerate() {
            if resolves_to(r, &key) {
                run_faces(&mut out, &graph.runs, &sides.runs, i);
            }
        }
    } else if let Some(j) = attach_junction(&graph, tip) {
        let mut arms: Vec<usize> = graph.junctions[j].arms.iter().map(|a| a.run).collect();
        arms.sort_unstable();
        arms.dedup();
        for i in arms {
            run_faces(&mut out, &graph.runs, &sides.runs, i);
        }
        if let Some(c) = corner_assembly(d, view).ok().and_then(|cs| cs.into_iter().find(|c| c.junction == j)) {
            out.placed = c.lines;
        }
    } else if let Some(b) = attach_bridge(&graph, tip) {
        let bridge = &graph.bridges[b];
        run_faces(&mut out, &graph.runs, &sides.runs, bridge.a.0);
        if bridge.b.0 != bridge.a.0 {
            run_faces(&mut out, &graph.runs, &sides.runs, bridge.b.0);
        }
        let dist = |q: Point2| (q.x - tip.x).hypot(q.y - tip.y);
        let end = (view_direction(d, view) == CutDirection::Vertical).then(|| dist(bridge.to) < dist(bridge.from));
        if let Some(o) = opening_assembly(d, view).ok().and_then(|os| os.into_iter().find(|o| o.bridge == b && o.end == end)) {
            out.placed = o.lines;
        }
    } else if view_direction(d, view) == CutDirection::Plan
        && let Some(e) = attach_free_end(&graph, tip)
    {
        run_faces(&mut out, &graph.runs, &sides.runs, graph.free_ends[e].0);
        if let Some(x) = end_assembly(d, view).ok().and_then(|es| es.into_iter().find(|x| x.end == e)) {
            out.placed = x.lines;
        }
    }
    out
}

/// A placed line's points: its two ends, or an arc sampled every [`STEP_DEG`].
fn line_points(l: &AssemblyLine) -> Vec<Point2> {
    let Some(arc) = l.arc else { return vec![l.from, l.to] };
    let a0 = (l.from.y - arc.center.y).atan2(l.from.x - arc.center.x);
    let a1 = (l.to.y - arc.center.y).atan2(l.to.x - arc.center.x);
    let tau = std::f64::consts::TAU;
    let sweep = if arc.ccw { (a1 - a0).rem_euclid(tau) } else { -(a0 - a1).rem_euclid(tau) };
    arc_points(arc.center, arc.radius, a0, sweep)
}

fn push_line(out: &mut DisplayList, h: Handle, pts: &[Point2], transparency: u8) {
    if pts.len() < 2 {
        return;
    }
    let vs: Vec<Vec2> = pts.iter().map(|p| Vec2::new(p.x, p.y)).collect();
    for v in &vs {
        out.bounds.add(*v);
    }
    push(out, h, ATTENTION_COLOR, Kind::Polyline, &vs);
    if let Some(p) = out.prims.last_mut() {
        p.transparency = transparency;
        p.lw = MIN_LW;
    }
}

/// The overlay: the selected tag's [`highlight`] in its own view's block — outside faces and
/// placed lines at [`OUTSIDE_TRANSPARENCY`], inside faces at [`INSIDE_TRANSPARENCY`]. Empty in any
/// other space, with no tag selected, and in plots (`opts.min_dash == 0`).
pub(crate) fn draw(d: &Drawing, space: &Space, opts: &Options) -> DisplayList {
    let mut out = DisplayList { bounds: Bounds2::EMPTY, ..DisplayList::default() };
    if opts.min_dash <= 0.0 {
        return out;
    }
    let Some(view) = view_id(space) else { return out };
    let Some((v, h)) = *selected() else { return out };
    if v != view {
        return out;
    }
    let hl = highlight(d, view, h);
    for (pts, outside) in &hl.faces {
        push_line(&mut out, h, pts, if *outside { OUTSIDE_TRANSPARENCY } else { INSIDE_TRANSPARENCY });
    }
    for l in &hl.placed {
        push_line(&mut out, h, &line_points(l), OUTSIDE_TRANSPARENCY);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assembly::tests::{corner_detail, room, window_room};
    use cadcraft_engine::Session;
    use serde_json::json;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;

    /// The handle of `view`'s tag of `detail` (a corner tag when `corner`).
    fn tag_of(s: &Session, view: Id, detail: Option<Id>, corner: bool) -> std::result::Result<Handle, Box<dyn std::error::Error>> {
        Ok(tags_in(s.doc()?, view)?
            .into_iter()
            .find(|(_, _, t)| t.corner == corner && detail.is_none_or(|d| t.detail_view_id == d))
            .map(|(h, _, _)| h)
            .ok_or("no such tag")?)
    }

    fn pixels() -> Options {
        Options { min_dash: 2.0, ..Options::default() }
    }

    #[test]
    fn an_edge_tag_highlights_both_faces_of_every_run_laying_its_detail() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        let d1 = tags_in(s.doc()?, plan)?.first().map(|(_, _, t)| t.detail_view_id).ok_or("no tag")?;
        // A second detail edge-tagged on the right wall (x 5000).
        let cd = corner_detail(&mut s, plan, "exterior_face", false, (0.0, 0.0))?;
        s.execute("detailtag", &json!({ "detailViewId": cd.0, "tip": {"x": 5000.0, "y": 2000.0}, "bubble": {"x": 6000.0, "y": 2000.0} }))?;
        let d = s.doc()?;
        let (runs, _) = resolve_view_details(d, plan)?;
        let key = d1.0.to_string();
        let want = runs.iter().filter(|r| resolves_to(r, &key)).count();
        assert!(want >= 1 && runs.iter().any(|r| resolves_to(r, &cd.0.to_string())), "{runs:?}");
        let hl = highlight(d, plan, tag_of(&s, plan, Some(d1), false)?);
        assert_eq!(hl.faces.len(), 2 * want, "{hl:?}");
        assert!(hl.faces.chunks(2).all(|f| f.iter().filter(|(_, o)| *o).count() == 1), "one outside face per run: {hl:?}");
        assert!(hl.placed.is_empty());
        // The right wall (centreline x 5000 − w/2) is absent.
        assert!(!hl.faces.iter().any(|(pts, _)| pts.iter().all(|p| p.x > 4000.0)), "{hl:?}");
        Ok(())
    }

    #[test]
    fn a_corner_tag_highlights_its_placed_corner_and_an_opening_tag_its_jambs() -> R {
        let (mut s, plan) = room("exterior_face", true)?;
        let cd = corner_detail(&mut s, plan, "exterior_face", false, (0.0, 0.0))?;
        let d = s.doc()?;
        let p = DrawingProject::new(d)?;
        let (graph, _) = graph_and_sides(&p, plan);
        let tag = tag_of(&s, plan, Some(cd), true)?;
        let tip = tags_in(d, plan)?.into_iter().find(|(h, _, _)| *h == tag).map(|(_, t, _)| t).ok_or("tip")?;
        let j = attach_junction(&graph, tip).ok_or("no junction")?;
        let want = corner_assembly(d, plan)?.into_iter().find(|c| c.junction == j).ok_or("no corner")?.lines;
        let hl = highlight(d, plan, tag);
        assert!(!want.is_empty());
        assert_eq!(hl.placed, want);
        assert_eq!(hl.faces.len(), 4, "two arms' faces: {hl:?}");
        drop(p);

        let (s, plan) = window_room(false, true)?;
        let d = s.doc()?;
        let tag = tag_of(&s, plan, None, true)?;
        let want = opening_assembly(d, plan)?.into_iter().next().ok_or("no opening")?.lines;
        assert!(!want.is_empty());
        assert_eq!(highlight(d, plan, tag).placed, want);
        Ok(())
    }

    #[test]
    fn selecting_a_tag_stores_it_and_a_plain_line_clears_it() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        let (mut s, plan) = room("exterior_face", true)?;
        let space = Space::Block(crate::schema::view_block_name(plan));
        *selected() = None;
        assert!(draw(s.doc()?, &space, &pixels()).prims.is_empty(), "nothing stored");
        let tag = tag_of(&s, plan, None, false)?;
        selection_changed(s.doc()?, &[tag]);
        assert_eq!(*selected(), Some((plan, tag)));
        let list = draw(s.doc()?, &space, &pixels());
        assert!(!list.prims.is_empty());
        assert!(list.prims.iter().all(|p| p.color == ATTENTION_COLOR && p.lw >= MIN_LW));
        assert!(draw(s.doc()?, &space, &Options::default()).prims.is_empty(), "nothing in plots");
        assert!(draw(s.doc()?, &Space::Model, &pixels()).prims.is_empty(), "other spaces");
        let before: Vec<Handle> = s.doc()?.blocks.values().flat_map(|b| b.entities.iter().map(|e| e.handle)).collect();
        s.execute("line", &json!({ "points": [[100, 100], [900, 100]] }))?;
        let line = s.doc()?.blocks.values().flat_map(|b| b.entities.iter().map(|e| e.handle)).find(|h| !before.contains(h)).ok_or("no line")?;
        selection_changed(s.doc()?, &[line]);
        assert_eq!(*selected(), None);
        selection_changed(s.doc()?, &[tag, line]);
        assert_eq!(*selected(), None, "two selected");
        Ok(())
    }
}

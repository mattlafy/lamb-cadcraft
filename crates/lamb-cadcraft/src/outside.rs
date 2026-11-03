//! Outside pick points (LAMB's `docs/07-architecture/cadcraft-integration.md` §8 "Inside and
//! outside", override 1): a CADCraft `Point` in a plan or section view's block carrying
//! [`EntityData::outside`] marks the region around it outside the building. Every such point feeds
//! [`SideParams::outside_points`](lamb_core::derivation::wall_spaces::SideParams) through
//! [`crate::detailtags::graph_and_sides`], so detail-tag resolution, ASSEMBLYPLAN, WALLCONFLICTS and
//! WALLFIT all see it. DXF/DWG see a plain point.
//!
//! - `outsidepoint {point:{x,y}}` (LAMB ▸ Details ▸ Outside Point): one tagged point in the edited
//!   plan or section view, on layer [`OUTSIDE_LAYER`] (created non-plotting, colour 8); one undo
//!   step. A closed building's outside is found automatically, so a point is accepted only when it
//!   changes some run's sides — a courtyard, or the side of an open wall chain — and is refused
//!   otherwise with the reason, recording nothing. Returns `{"handle": <hex>, "message": …}` and
//!   echoes the message. Interactive: `Specify a point outside the building (a courtyard, or beside
//!   an open wall):`.
//! - [`draw`]: each outside point of the edited view as a faint circle with a centre dot (overlay
//!   only; the point itself never plots).
//!
//! Detail-tag sides (LAMB T615) override outside points: [`apply_tag_sides`], called from
//! [`crate::detailtags::graph_and_sides`] after `wall_sides`, gives every edge tag's run the tag's
//! side — the run's automatic side where its structure closes, else the side its bubble sits on (a leader's last
//! vertex, a callout box's centre), inverted by [`DetailTag::flip`](crate::schema::DetailTag) — and
//! carries it run to run through every two-arm junction and opening bridge (stopping at T/X
//! junctions and free ends), each run taking the nearest tag by hops (ties: the earlier tag). The
//! result is written into `RunSides::open_outside_left`, which `outside_on_left` reads first.
//! OUTSIDEPOINT stays typed-only for courtyards and saved projects; it is off the Details menu.

use cadcraft_color::{Color, Rgb};
use cadcraft_doc::{Drawing, EntityKind, Handle, Point, Space};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::{Bounds2, Vec2};
use cadcraft_render::{DisplayList, Kind, Options};
use lamb_core::derivation::wall_junctions::{RunEnd, WallGraph};
use lamb_core::derivation::wall_spaces::{RunSides, SideKind, SideParams, WallSides, outside_on_left, wall_sides};
use lamb_core::derivation::walls::RunCurve;
use lamb_core::geometry::Point2;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::commands::{other, point_param};
use crate::detailtags::{attach_run, edited_wall_view, exterior_layer_ids, graph_and_sides, tags_in};
use crate::overlays::{push, view_id};
use crate::project::DrawingProject;
use crate::schema::{EntityData, read_entity_data, set_entity_data, view_block_name};

/// The layer every outside point goes on: non-plotting, colour 8.
pub(crate) const OUTSIDE_LAYER: &str = "LAMB-OUTSIDE";
/// The marker's grey.
const MARKER_COLOR: Rgb = Rgb(128, 128, 128);
/// The marker's transparency, percent: faint.
const MARKER_TRANSPARENCY: u8 = 65;
/// Marker circle radius on screen, px.
const MARKER_RADIUS_PX: f64 = 8.0;
/// Marker centre dot radius on screen, px.
const DOT_RADIUS_PX: f64 = 1.5;
/// Marker circle radius when there are no pixels (`Options::min_dash == 0`), mm.
const MARKER_FALLBACK_RADIUS_MM: f64 = 150.0;
/// Segments of the marker's circle and dot.
const SEGMENTS: usize = 32;

/// Every outside pick point in `view`'s block with its handle; malformed LAMB data is skipped.
fn tagged_points(d: &Drawing, view: Id) -> Vec<(Handle, Point2)> {
    let Some(block) = d.block(&view_block_name(view)) else { return Vec::new() };
    block
        .entities
        .iter()
        .filter_map(|e| {
            let EntityKind::Point(pt) = &e.kind else { return None };
            read_entity_data(&e.common).ok().filter(|data| data.outside).map(|_| (e.handle, Point2 { x: pt.p.x, y: pt.p.y }))
        })
        .collect()
}

/// Every outside pick point in `view`'s block; a point with malformed LAMB data is skipped.
pub(crate) fn outside_points(d: &Drawing, view: Id) -> Vec<Point2> {
    tagged_points(d, view).into_iter().map(|(_, p)| p).collect()
}

/// What adding an outside point at `at` to `view` would do: `Ok(message)` when it changes some
/// run's sides, `Err(reason)` when it changes nothing.
fn placement_effect(d: &Drawing, view: Id, at: Point2) -> std::result::Result<String, String> {
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    // Plain `wall_sides` both ways: tag sides would mask what the point itself changes.
    let graph = graph_and_sides(&p, view).0;
    let ext = exterior_layer_ids(&p);
    let sides = |pts: Vec<Point2>| wall_sides(&p, &graph, &SideParams { outside_points: pts, exterior_layer_ids: ext.clone() });
    let mut pts = outside_points(d, view);
    let without = sides(pts.clone());
    pts.push(at);
    let with = sides(pts);
    if with.runs != without.runs {
        if with.spaces.len() < without.spaces.len() {
            // The space that became outside: the first of `without`'s that `with` lacks.
            let n = without.spaces.iter().position(|sp| !with.spaces.contains(sp)).unwrap_or(0);
            return Ok(format!("Outside: space {} is now a courtyard", n + 1));
        }
        let k = with.runs.iter().zip(&without.runs).filter(|(a, b)| a.open_outside_left != b.open_outside_left).count();
        return Ok(format!("Outside: {k} open wall(s) now face this side"));
    }
    // Refused: say why, from what the point would do on its own.
    let bare = sides(Vec::new());
    let alone = sides(vec![at]);
    if alone.spaces.len() < bare.spaces.len() {
        Err("That courtyard is already marked outside".into())
    } else if alone.runs == bare.runs && !has_open_runs(&bare) {
        Err("Already outside: a closed building's outside is found automatically. Use an outside point for a courtyard or beside an open wall".into())
    } else {
        Err("Changes nothing: those walls already face this way".into())
    }
}

/// Whether any run has `Outside` on both sides (an open wall chain).
fn has_open_runs(w: &WallSides) -> bool {
    w.runs.iter().any(|r| r.left == SideKind::Outside && r.right == SideKind::Outside)
}

/// OUTSIDEPOINT's JSON form: a tagged point at `point` in the edited plan or section view, on
/// [`OUTSIDE_LAYER`], when it changes a side.
pub(crate) fn outsidepoint(s: &mut Session, p: &Value) -> Result<Value> {
    let at = point_param("outsidepoint", p, "point")?;
    // Every refusal before `doc_mut`, so a refusal records nothing.
    let view = edited_wall_view(s).map_err(other)?;
    let message = placement_effect(s.doc()?, view, at).map_err(other)?;
    let mut common = s.current_common();
    common.layer = OUTSIDE_LAYER.to_owned();
    common.color = Color::ByLayer;
    set_entity_data(&mut common, &EntityData { outside: true, ..EntityData::default() }).map_err(other)?;
    let space = s.space();
    let d = s.doc_mut()?;
    if d.ensure_layer(OUTSIDE_LAYER)
        && let Some(l) = d.layer_mut(OUTSIDE_LAYER)
    {
        l.plot = false;
        l.color = Color::Index(8);
    }
    let h = d.add(&space, common, EntityKind::Point(Point { p: Vec2::new(at.x, at.y).to3(0.0), angle: 0.0 }))?;
    s.echo(message.clone());
    Ok(json!({ "handle": format!("{:X}", h.0), "message": message }))
}

/// The side of run curve `c` (left of its travel = `true`) holding `p`; `None` within the wall band
/// (`width / 2` of the centreline).
fn point_side(c: &RunCurve, width: f64, p: Point2) -> Option<bool> {
    match *c {
        RunCurve::Line { from, to } => {
            let len = (to.x - from.x).hypot(to.y - from.y);
            if len <= TOLERANCE.coincidence_mm {
                return None;
            }
            let off = ((to.x - from.x) * (p.y - from.y) - (to.y - from.y) * (p.x - from.x)) / len;
            (off.abs() > width / 2.0).then_some(off > 0.0)
        }
        RunCurve::Arc { center, radius, sweep, .. } => {
            let r = (p.x - center.x).hypot(p.y - center.y);
            // Travelling counter-clockwise, the centre is on the left.
            ((r - radius).abs() > width / 2.0).then_some((r < radius) == (sweep > 0.0))
        }
    }
}

/// Where a tag's bubble sits: a leader's last vertex, a callout box's centre.
fn bubble_of(kind: &EntityKind) -> Option<Point2> {
    match kind {
        EntityKind::Leader(l) => l.vertices.last().map(|v| Point2 { x: v.x, y: v.y }),
        EntityKind::LwPolyline(pl) if !pl.vertices.is_empty() => {
            let xs = pl.vertices.iter().map(|v| v.p.x);
            let ys = pl.vertices.iter().map(|v| v.p.y);
            let (x0, x1) = (xs.clone().fold(f64::INFINITY, f64::min), xs.fold(f64::NEG_INFINITY, f64::max));
            let (y0, y1) = (ys.clone().fold(f64::INFINITY, f64::min), ys.fold(f64::NEG_INFINITY, f64::max));
            Some(Point2 { x: (x0 + x1) / 2.0, y: (y0 + y1) / 2.0 })
        }
        _ => None,
    }
}

/// Each edge tag's (run, outside-on-left) in `view`, in block order: the run's automatic side in
/// `sides` when its structure closes there (one side `Outside`), else its bubble's side, inverted by `flip`. A tag whose side is undecided
/// (bubble inside the wall band) or that attaches to no run is left out.
fn tag_sources(d: &Drawing, view: Id, graph: &WallGraph, sides: &WallSides) -> Vec<(usize, bool)> {
    let Ok(tags) = tags_in(d, view) else { return Vec::new() };
    let Some(block) = d.block(&view_block_name(view)) else { return Vec::new() };
    tags.into_iter()
        .filter(|(_, _, t)| !t.corner)
        .filter_map(|(h, tip, t)| {
            let run = attach_run(graph, tip)?;
            // The closed-structure side only: an outside point never overrules a tag.
            let auto = sides.runs.get(run).and_then(|r| outside_on_left(&RunSides { open_outside_left: None, ..r.clone() }));
            let side = auto.or_else(|| {
                let e = block.entities.iter().find(|e| e.handle == h)?;
                let r = &graph.runs[run];
                point_side(&r.centreline, r.width, bubble_of(&e.kind)?)
            })?;
            Some((run, side != t.flip))
        })
        .collect()
}

/// The runs each run's side carries to: `(other run, swap)` through every two-arm junction and every
/// opening bridge; `swap` when the two ends meeting are alike (Start–Start, End–End).
fn chain_links(graph: &WallGraph) -> Vec<Vec<(usize, bool)>> {
    let mut out = vec![Vec::new(); graph.runs.len()];
    let mut link = |(a, ea): (usize, RunEnd), (b, eb): (usize, RunEnd)| {
        if a < out.len() && b < out.len() {
            out[a].push((b, ea == eb));
            out[b].push((a, ea == eb));
        }
    };
    for j in graph.junctions.iter().filter(|j| j.arms.len() == 2) {
        link((j.arms[0].run, j.arms[0].end), (j.arms[1].run, j.arms[1].end));
    }
    for b in &graph.bridges {
        link(b.a, b.b);
    }
    out
}

/// Each run's tag-governed outside-on-left (parallel to `graph.runs`), `None` where no tag reaches:
/// a breadth-first walk from every tagged run at once along [`chain_links`], each run taking the
/// first tag to reach it — ties to the earlier tag, a tagged run always its own (first) tag's.
pub(crate) fn tag_sides(d: &Drawing, view: Id, graph: &WallGraph, sides: &WallSides) -> Vec<Option<bool>> {
    let mut out = vec![None; graph.runs.len()];
    let mut queue = std::collections::VecDeque::new();
    for (run, left) in tag_sources(d, view, graph, sides) {
        if out[run].is_none() {
            out[run] = Some(left);
            queue.push_back(run);
        }
    }
    let links = chain_links(graph);
    while let Some(r) = queue.pop_front() {
        let Some(left) = out[r] else { continue };
        for &(n, swap) in &links[r] {
            if out[n].is_none() {
                out[n] = Some(left != swap);
                queue.push_back(n);
            }
        }
    }
    out
}

/// Writes [`tag_sides`] into `sides`: each governed run's `open_outside_left`.
pub(crate) fn apply_tag_sides(d: &Drawing, view: Id, graph: &WallGraph, sides: &mut WallSides) {
    for (r, side) in tag_sides(d, view, graph, sides).into_iter().enumerate() {
        if let (Some(left), Some(rs)) = (side, sides.runs.get_mut(r)) {
            rs.open_outside_left = Some(left);
        }
    }
}

/// A circle of radius `r` about `c`, closed (its first point repeated).
fn circle(c: Vec2, r: f64) -> Vec<Vec2> {
    (0..=SEGMENTS)
        .map(|i| {
            let a = std::f64::consts::TAU * (i % SEGMENTS) as f64 / SEGMENTS as f64;
            c + Vec2::new(r * a.cos(), r * a.sin())
        })
        .collect()
}

/// The overlay: for each outside point of the edited view, a faint grey circle of
/// [`MARKER_RADIUS_PX`] ([`MARKER_FALLBACK_RADIUS_MM`] without pixels) and a filled centre dot of
/// [`DOT_RADIUS_PX`]. Nothing in any other space.
pub(crate) fn draw(d: &Drawing, space: &Space, opts: &Options) -> DisplayList {
    let mut out = DisplayList { bounds: Bounds2::EMPTY, ..DisplayList::default() };
    let Some(view) = view_id(space) else { return out };
    // One pixel in world units (`min_dash` is two).
    let px = if opts.min_dash > 0.0 { opts.min_dash / 2.0 } else { MARKER_FALLBACK_RADIUS_MM / MARKER_RADIUS_PX };
    for (h, p) in tagged_points(d, view) {
        let c = Vec2::new(p.x, p.y);
        let ring = circle(c, MARKER_RADIUS_PX * px);
        for q in &ring {
            out.bounds.add(*q);
        }
        push(&mut out, h, MARKER_COLOR, Kind::Polyline, &ring);
        let dot = circle(c, DOT_RADIUS_PX * px);
        let fan: Vec<Vec2> = dot.windows(2).flat_map(|w| [c, w[0], w[1]]).collect();
        push(&mut out, h, MARKER_COLOR, Kind::Tris, &fan);
    }
    for prim in &mut out.prims {
        prim.transparency = MARKER_TRANSPARENCY;
    }
    out
}

/// OUTSIDEPOINT: one point prompt.
struct OutsidePointM;

impl Interactive for OutsidePointM {
    fn name(&self) -> &'static str {
        "outsidepoint"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Specify a point outside the building (a courtyard, or beside an open wall)", Accept::POINT)
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if let Err(e) = edited_wall_view(s) {
            s.echo(e);
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Point(p) => {
                if let Err(e) = outsidepoint(s, &json!({ "point": {"x": p.x, "y": p.y} })) {
                    s.echo(e.to_string());
                }
                Ok(Step::Done)
            }
            Input::Cancel | Input::Enter => Ok(Step::Cancel),
            _ => Ok(Step::Continue),
        }
    }
}

pub(crate) fn outsidepoint_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(OutsidePointM))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detailtags::graph_and_sides;
    use crate::project::DrawingProject;
    use lamb_core::derivation::wall_spaces::{SideKind, WallSides};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    /// A plan with a closed 5000 × 4000 room of 200 mm walls on `WALL-EXT` (outer face 0..5000 ×
    /// 0..4000) and a detail view `D1`, editing the plan on layer `MARKS`. Returns (session, plan, D1).
    fn room() -> T<(Session, Id, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let detail = Id(s.execute("newdetail", &json!({ "name": "D1" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [5000, 0], [5000, 4000], [0, 4000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [4800, 200], [4800, 3800], [200, 3800]], "closed": true }))?;
        s.execute("layer.new", &json!({ "name": "MARKS", "current": true }))?;
        Ok((s, plan, detail))
    }

    /// A plan with an open L of 200 mm walls on `WALL` (outer faces along y = 0 and x = 5000),
    /// editing the plan. Returns (session, plan).
    fn l_chain() -> T<(Session, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [5000, 0], [5000, 4000]] }))?;
        s.execute("pline", &json!({ "vertices": [[0, 200], [4800, 200], [4800, 4000]] }))?;
        Ok((s, plan))
    }

    fn sides_of(d: &Drawing, plan: Id) -> T<WallSides> {
        Ok(graph_and_sides(&DrawingProject::new(d)?, plan).1)
    }

    fn any_space(w: &WallSides) -> bool {
        w.runs.iter().any(|r| matches!(r.left, SideKind::Space(_)) || matches!(r.right, SideKind::Space(_)))
    }

    fn mark(s: &mut Session, x: f64, y: f64) -> Result<Value> {
        s.execute("outsidepoint", &json!({ "point": {"x": x, "y": y} }))
    }

    fn points(s: &Session, plan: Id) -> T<Vec<cadcraft_doc::Entity>> {
        let b = s.doc()?.block(&view_block_name(plan)).ok_or("no block")?.clone();
        Ok(b.entities.iter().filter(|e| matches!(e.kind, EntityKind::Point(_))).map(|e| (**e).clone()).collect())
    }

    /// `mark` fails with a message containing `want`, adding nothing and no undo step.
    fn refused(s: &mut Session, plan: Id, x: f64, y: f64, want: &str) -> R {
        let (n, undo) = (points(s, plan)?.len(), s.state()?.undo.len());
        let e = mark(s, x, y).err().ok_or("accepted")?;
        assert!(e.to_string().contains(want), "{e}");
        assert_eq!((points(s, plan)?.len(), s.state()?.undo.len()), (n, undo));
        Ok(())
    }

    #[test]
    fn a_point_inside_the_room_makes_the_courtyard_outside_in_one_undo_step() -> R {
        let (mut s, plan, _) = room()?;
        let before = sides_of(s.doc()?, plan)?;
        assert!(any_space(&before));
        let undo = s.state()?.undo.len();
        let out = mark(&mut s, 2500.0, 2000.0)?;
        let h = out["handle"].as_str().ok_or("no handle")?.to_owned();
        assert_eq!(out["message"], "Outside: space 1 is now a courtyard");
        assert!(s.log.iter().any(|l| l == "Outside: space 1 is now a courtyard"), "{:?}", s.log);
        let after = sides_of(s.doc()?, plan)?;
        assert!(!any_space(&after), "{after:?}");
        assert_eq!(s.state()?.undo.len(), undo + 1);
        let p = points(&s, plan)?;
        assert_eq!(p.len(), 1);
        assert_eq!(format!("{:X}", p[0].handle.0), h);
        assert!(read_entity_data(&p[0].common)?.outside);
        assert_eq!(outside_points(s.doc()?, plan), [Point2 { x: 2500.0, y: 2000.0 }]);
        Ok(())
    }

    #[test]
    fn accepted_points_go_on_a_non_plotting_layer_whatever_the_current_layer() -> R {
        let (mut s, plan, _) = room()?;
        mark(&mut s, 2500.0, 2000.0)?;
        let p = points(&s, plan)?;
        assert_eq!(p[0].common.layer, OUTSIDE_LAYER);
        assert_eq!(p[0].common.color, Color::ByLayer);
        let l = s.doc()?.layer(OUTSIDE_LAYER).ok_or("no layer")?;
        assert!(!l.plot);
        assert_eq!(l.color, Color::Index(8));
        assert_eq!(s.doc()?.header.str("CLAYER", "0"), "MARKS");
        Ok(())
    }

    #[test]
    fn a_point_outside_a_closed_building_is_refused() -> R {
        let (mut s, plan, _) = room()?;
        refused(&mut s, plan, -1000.0, -1000.0, "Already outside: a closed building's outside is found automatically")?;
        assert!(s.doc()?.layer(OUTSIDE_LAYER).is_none());
        Ok(())
    }

    #[test]
    fn a_second_point_in_the_same_courtyard_is_refused() -> R {
        let (mut s, plan, _) = room()?;
        mark(&mut s, 2500.0, 2000.0)?;
        refused(&mut s, plan, 1000.0, 1000.0, "That courtyard is already marked outside")
    }

    #[test]
    fn beside_an_open_chain_one_point_decides_the_side_and_a_second_changes_nothing() -> R {
        let (mut s, plan) = l_chain()?;
        let before = sides_of(s.doc()?, plan)?;
        assert!(before.runs.iter().all(|r| r.open_outside_left.is_none()), "{before:?}");
        let out = mark(&mut s, 2500.0, -1000.0)?;
        assert!(out["message"].as_str().is_some_and(|m| m.starts_with("Outside: ") && m.ends_with("open wall(s) now face this side")), "{out}");
        assert!(sides_of(s.doc()?, plan)?.runs.iter().any(|r| r.open_outside_left.is_some()));
        refused(&mut s, plan, 3000.0, -2000.0, "Changes nothing: those walls already face this way")
    }

    #[test]
    fn erase_and_undo_restore_the_sides_and_the_tag_survives_dxf() -> R {
        let (mut s, plan, _) = room()?;
        let before = sides_of(s.doc()?, plan)?;
        let h = mark(&mut s, 2500.0, 2000.0)?["handle"].as_str().ok_or("no handle")?.to_owned();
        let marked = sides_of(s.doc()?, plan)?;

        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert_eq!(outside_points(&back, plan), outside_points(s.doc()?, plan));
        assert_eq!(sides_of(&back, plan)?, marked);
        assert!(!back.layer(OUTSIDE_LAYER).ok_or("no layer")?.plot);

        s.undo()?;
        assert_eq!(sides_of(s.doc()?, plan)?, before);
        s.redo()?;
        assert_eq!(sides_of(s.doc()?, plan)?, marked);
        s.execute("erase", &json!({ "handles": [h] }))?;
        assert!(points(&s, plan)?.is_empty());
        assert_eq!(sides_of(s.doc()?, plan)?, before);
        Ok(())
    }

    #[test]
    fn outside_a_plan_or_section_view_it_refuses_and_adds_nothing() -> R {
        let (mut s, _plan, detail) = room()?;
        s.execute("editview", &json!({}))?;
        let n = s.doc()?.model.len();
        assert!(mark(&mut s, 2500.0, 2000.0).is_err_and(|e| e.to_string().contains("Edit a plan or section view first")));
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        assert!(mark(&mut s, 2500.0, 2000.0).is_err_and(|e| e.to_string().contains("Detail tags live in plan and section views")));
        assert_eq!(s.doc()?.model.len(), n);
        assert!(s.doc()?.block(&view_block_name(detail)).ok_or("no block")?.entities.is_empty());

        s.execute("editview", &json!({}))?;
        s.start("outsidepoint")?;
        assert!(s.running.is_none(), "refused at once");
        assert!(s.log.iter().any(|l| l == "Edit a plan or section view first (EDITVIEW)"), "{:?}", s.log);
        Ok(())
    }

    #[test]
    fn the_interactive_form_places_the_same_point_as_the_parameter_form_and_echoes_refusals() -> R {
        let (mut s, plan, _) = room()?;
        s.start("outsidepoint")?;
        assert_eq!(
            s.current_prompt().map(|p| p.display()).as_deref(),
            Some("Specify a point outside the building (a courtyard, or beside an open wall):")
        );
        s.input(Input::Point(Vec2::new(2500.0, 2000.0)))?;
        assert!(s.running.is_none());
        let (mut t, plan_t, _) = room()?;
        mark(&mut t, 2500.0, 2000.0)?;
        let (p, q) = (points(&s, plan)?, points(&t, plan_t)?);
        assert_eq!((p.len(), q.len()), (1, 1));
        assert_eq!((&p[0].kind, &p[0].common.layer, &p[0].common.ext), (&q[0].kind, &q[0].common.layer, &q[0].common.ext));

        s.start("outsidepoint")?;
        s.input(Input::Point(Vec2::new(1000.0, 1000.0)))?;
        assert!(s.running.is_none());
        assert!(s.log.iter().any(|l| l.contains("That courtyard is already marked outside")), "{:?}", s.log);
        assert_eq!(points(&s, plan)?.len(), 1);
        Ok(())
    }

    #[test]
    fn draw_gives_a_circle_and_a_dot_per_point_in_the_edited_view_only() -> R {
        let (mut s, plan, detail) = room()?;
        mark(&mut s, 2500.0, 2000.0)?;
        let d = s.doc()?;
        let opts = Options::default();
        let list = draw(d, &Space::Block(view_block_name(plan)), &opts);
        let kinds: Vec<Kind> = list.prims.iter().map(|p| p.kind).collect();
        assert_eq!(kinds, [Kind::Polyline, Kind::Tris]);
        assert!(list.prims.iter().all(|p| p.transparency == MARKER_TRANSPARENCY && p.color == MARKER_COLOR));
        let ring = list.points(&list.prims[0]);
        assert!(ring.iter().all(|q| ((q.x - 2500.0).hypot(q.y - 2000.0) - MARKER_FALLBACK_RADIUS_MM).abs() < 1e-6));
        // On screen: 8 px at min_dash = 2 px = 20 mm → 80 mm.
        let screen = draw(d, &Space::Block(view_block_name(plan)), &Options { min_dash: 20.0, ..Options::default() });
        assert!(screen.points(&screen.prims[0]).iter().all(|q| ((q.x - 2500.0).hypot(q.y - 2000.0) - 80.0).abs() < 1e-6));
        assert!(draw(d, &Space::Block(view_block_name(detail)), &opts).prims.is_empty());
        assert!(draw(d, &Space::Model, &opts).prims.is_empty());
        Ok(())
    }

    // ---- Detail-tag sides (LAMB T615) ----

    /// A detail view `D1` added to `s`, then back to editing `plan`.
    fn add_detail(s: &mut Session, plan: Id) -> T<Id> {
        let d = Id(s.execute("newdetail", &json!({ "name": "D1" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        Ok(d)
    }

    /// An edge tag of `detail` at `tip` with its bubble at `bubble`; returns its handle.
    fn edge_tag(s: &mut Session, detail: Id, tip: (f64, f64), bubble: (f64, f64), flip: bool) -> T<Handle> {
        let r = s.execute(
            "detailtag",
            &json!({ "detailViewId": detail.0, "flip": flip, "tip": {"x": tip.0, "y": tip.1}, "bubble": {"x": bubble.0, "y": bubble.1} }),
        )?;
        Ok(Handle::parse_hex(r["handle"].as_str().ok_or("no handle")?).ok_or("bad handle")?)
    }

    /// The run whose band holds `p`, and whether its outside is on `far`'s side: `Some(true)` when
    /// it is, `Some(false)` when it is the other side, `None` when undecided.
    fn faces(d: &Drawing, plan: Id, on: (f64, f64), far: (f64, f64)) -> T<Option<bool>> {
        let p = DrawingProject::new(d)?;
        let (graph, sides) = graph_and_sides(&p, plan);
        let r = attach_run(&graph, Point2 { x: on.0, y: on.1 }).ok_or("no run there")?;
        let run = &graph.runs[r];
        let want = point_side(&run.centreline, run.width, Point2 { x: far.0, y: far.1 }).ok_or("far point in the band")?;
        Ok(outside_on_left(&sides.runs[r]).map(|left| left == want))
    }

    /// [`l_chain`]'s two runs: (bottom, right) — a point in each band and one well outside the L.
    const L_RUNS: [((f64, f64), (f64, f64)); 2] = [((2500.0, 100.0), (2500.0, -1000.0)), ((4900.0, 2000.0), (6000.0, 2000.0))];

    fn l_faces(d: &Drawing, plan: Id) -> T<Vec<Option<bool>>> {
        L_RUNS.iter().map(|&(on, far)| faces(d, plan, on, far)).collect()
    }

    #[test]
    fn one_edge_tag_gives_an_open_l_chain_its_bubble_s_side_and_detailflip_inverts_it_in_one_undo_step() -> R {
        let (mut s, plan) = l_chain()?;
        let detail = add_detail(&mut s, plan)?;
        assert_eq!(l_faces(s.doc()?, plan)?, [None, None]);
        let h = edge_tag(&mut s, detail, (2500.0, 0.0), (2500.0, -1000.0), false)?;
        assert_eq!(l_faces(s.doc()?, plan)?, [Some(true), Some(true)]);

        let undo = s.state()?.undo.len();
        let out = s.execute("detailflip", &json!({ "handle": h.hex() }))?;
        assert_eq!((out["handle"].as_str(), out["flip"].as_bool()), (Some(h.hex().as_str()), Some(true)));
        assert_eq!(s.state()?.undo.len(), undo + 1);
        assert_eq!(l_faces(s.doc()?, plan)?, [Some(false), Some(false)]);
        s.undo()?;
        assert_eq!(l_faces(s.doc()?, plan)?, [Some(true), Some(true)]);
        s.redo()?;
        assert_eq!(l_faces(s.doc()?, plan)?, [Some(false), Some(false)]);
        Ok(())
    }

    #[test]
    fn a_tag_whose_bubble_is_inside_the_wall_band_decides_nothing() -> R {
        let (mut s, plan) = l_chain()?;
        let detail = add_detail(&mut s, plan)?;
        edge_tag(&mut s, detail, (2500.0, 0.0), (2600.0, 150.0), false)?;
        assert_eq!(l_faces(s.doc()?, plan)?, [None, None]);
        Ok(())
    }

    #[test]
    fn detailflip_refuses_what_is_not_a_tag_of_the_edited_view_and_records_nothing() -> R {
        let (mut s, plan) = l_chain()?;
        let detail = add_detail(&mut s, plan)?;
        edge_tag(&mut s, detail, (2500.0, 0.0), (2500.0, -1000.0), false)?;
        let wall = s.doc()?.block(&view_block_name(plan)).ok_or("no block")?.entities.iter().next().ok_or("empty")?.handle;
        let undo = s.state()?.undo.len();
        let e = s.execute("detailflip", &json!({ "handle": wall.hex() })).err().ok_or("accepted")?;
        assert!(e.to_string().contains("That is not a detail tag in this view"), "{e}");
        assert!(s.execute("detailflip", &json!({})).is_err());
        assert_eq!(s.state()?.undo.len(), undo);
        Ok(())
    }

    #[test]
    fn the_interactive_forms_flip_a_new_tag_and_an_existing_one() -> R {
        let (mut s, plan) = l_chain()?;
        add_detail(&mut s, plan)?;
        s.start("detailtag")?;
        s.input(Input::Enter)?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Specify tag point on a wall face or [Flip]:"));
        s.input(Input::Keyword("Flip".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Specify tag point on a wall face (flipped)"));
        s.input(Input::Point(Vec2::new(2500.0, 0.0)))?;
        s.input(Input::Point(Vec2::new(2500.0, -1000.0)))?;
        assert!(s.running.is_none());
        let tags = tags_in(s.doc()?, plan)?;
        assert_eq!(tags.len(), 1);
        assert!(tags[0].2.flip);
        assert_eq!(l_faces(s.doc()?, plan)?, [Some(false), Some(false)]);

        s.start("detailflip")?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Select detail tag to flip"));
        s.input(Input::Pick(vec![tags[0].0]))?;
        assert!(s.running.is_none());
        assert!(!tags_in(s.doc()?, plan)?[0].2.flip);
        assert_eq!(l_faces(s.doc()?, plan)?, [Some(true), Some(true)]);
        Ok(())
    }

    /// [`room`]'s four runs: a point in each band and one outside the room beside it.
    const ROOM_RUNS: [((f64, f64), (f64, f64)); 4] = [
        ((2500.0, 100.0), (2500.0, -1000.0)),
        ((4900.0, 2000.0), (6000.0, 2000.0)),
        ((2500.0, 3900.0), (2500.0, 5000.0)),
        ((100.0, 2000.0), (-1000.0, 2000.0)),
    ];

    fn room_faces(d: &Drawing, plan: Id) -> T<Vec<Option<bool>>> {
        ROOM_RUNS.iter().map(|&(on, far)| faces(d, plan, on, far)).collect()
    }

    #[test]
    fn in_a_closed_room_a_flipped_tag_inverts_the_loop_and_a_second_tag_splits_it_by_hops() -> R {
        let (mut s, plan, detail) = room()?;
        let auto = [Some(true); 4];
        assert_eq!(room_faces(s.doc()?, plan)?, auto);
        let h = edge_tag(&mut s, detail, (2500.0, 0.0), (2500.0, -1000.0), false)?;
        assert_eq!(room_faces(s.doc()?, plan)?, auto, "an unflipped tag changes no side");
        s.execute("detailflip", &json!({ "handle": h.hex() }))?;
        assert_eq!(room_faces(s.doc()?, plan)?, [Some(false); 4]);
        // Unflipped on the top wall: the top keeps the automatic side; the side walls are one hop
        // from both tags, so the earlier (flipped) tag takes them.
        edge_tag(&mut s, detail, (2500.0, 4000.0), (2500.0, 5000.0), false)?;
        assert_eq!(room_faces(s.doc()?, plan)?, [Some(false), Some(false), Some(true), Some(false)]);
        Ok(())
    }

    #[test]
    fn a_tag_side_stops_at_a_t_junction_and_crosses_an_opening_bridge() -> R {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let detail = add_detail(&mut s, plan)?;
        s.execute("layer.new", &json!({ "name": "WALL", "current": true }))?;
        // An open straight wall y 0..200 with a window gap at x 2000..3000, and a stub wall
        // x 3900..4100 standing on it (a T).
        s.execute("pline", &json!({ "vertices": [[0, 0], [2000, 0]] }))?;
        s.execute("pline", &json!({ "vertices": [[0, 200], [2000, 200]] }))?;
        s.execute("pline", &json!({ "vertices": [[3000, 0], [6000, 0]] }))?;
        s.execute("pline", &json!({ "vertices": [[3000, 200], [3900, 200], [3900, 3000]] }))?;
        s.execute("pline", &json!({ "vertices": [[4100, 3000], [4100, 200], [6000, 200]] }))?;
        s.execute("layer.new", &json!({ "name": "WINDOW", "current": true }))?;
        s.execute("line", &json!({ "points": [[2000, 100], [3000, 100]] }))?;
        s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
        {
            let p = DrawingProject::new(s.doc()?)?;
            let (graph, _) = graph_and_sides(&p, plan);
            assert_eq!(graph.bridges.len(), 1, "{graph:?}");
            assert!(graph.junctions.iter().any(|j| j.arms.len() == 3), "{graph:?}");
        }
        edge_tag(&mut s, detail, (1000.0, 0.0), (1000.0, -1000.0), false)?;
        let d = s.doc()?;
        assert_eq!(faces(d, plan, (1000.0, 100.0), (1000.0, -1000.0))?, Some(true));
        assert_eq!(faces(d, plan, (3500.0, 100.0), (3500.0, -1000.0))?, Some(true), "across the window");
        assert_eq!(faces(d, plan, (5000.0, 100.0), (5000.0, -1000.0))?, None, "past the T");
        assert_eq!(faces(d, plan, (4000.0, 2000.0), (5000.0, 2000.0))?, None, "up the stub");
        Ok(())
    }

    #[test]
    fn an_outside_point_on_a_tag_governed_run_changes_nothing_there() -> R {
        let (mut s, plan) = l_chain()?;
        let detail = add_detail(&mut s, plan)?;
        edge_tag(&mut s, detail, (2500.0, 0.0), (2500.0, -1000.0), false)?;
        // Inside the L: plain sides would face it; the tag's side wins.
        mark(&mut s, 2500.0, 1500.0)?;
        assert_eq!(l_faces(s.doc()?, plan)?, [Some(true), Some(true)]);
        Ok(())
    }

    #[test]
    fn a_tag_s_flip_survives_dxf() -> R {
        let (mut s, plan) = l_chain()?;
        let detail = add_detail(&mut s, plan)?;
        edge_tag(&mut s, detail, (2500.0, 0.0), (2500.0, -1000.0), true)?;
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert!(tags_in(&back, plan)?[0].2.flip);
        assert_eq!(l_faces(&back, plan)?, [Some(false), Some(false)]);
        assert_eq!(sides_of(&back, plan)?, sides_of(s.doc()?, plan)?);
        Ok(())
    }
}

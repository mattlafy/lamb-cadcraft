//! Detail tags (LAMB's `docs/07-architecture/cadcraft-integration.md` §9 "Detail tags are the
//! assignment", phase 8, LAMB T349): a tag is an ordinary CADCraft `Leader` in a plan or section
//! view's block (LAMB T456: §8 "Not only plans" — plan details tag plan views, section details
//! vertical views, whose runs come from cut faces on `WALL*`, `FLOOR*`, `ROOF*` and `SLAB*` — and,
//! LAMB T681, on the layer of any line a tag in that view sits on ([`wall_params_with`])),
//! its tip `vertices[0]`, carrying [`EntityData::detail_tag`] — the detail view it names and whether
//! it tags a junction (corner) or a run (edge). DXF/DWG see a plain leader.
//!
//! - `detailtag {detailViewId, corner?, tip:{x,y}, bubble:{x,y}}` (`dtag`): one tag, on the current
//!   layer, in the edited plan or section view; one undo step; the detail's cut direction must match.
//!   A tip that would attach to nothing is refused (LAMB T590), as is an edge tag on a face-corner
//!   vertex; a placed tag echoes `Tagged run 2 of "Ground" with "D1"` and the view's report line,
//!   and a view's first tag writes its assembly block ([`crate::assembly::write_assembly_block`])
//!   in the same undo step. Returns `{"handle": <hex>, "attached": {"run"|"junction"|"opening"|"end": n}}`.
//!   Interactive (LAMB T591): `Select detail [D1/Corner_Detail/Corner]:` — one keyword per detail
//!   drawn in the edited view's direction (name with `/[]` and spaces as `_`; a typed name or id
//!   also selects; none ends the command) → `Specify tag point on a wall face:` (`at a wall
//!   corner` in Corner mode; asked again after a refused point) → `Specify bubble point or [Box]:`.
//! - A callout-box tag (LAMB T594): `detailtag {…, tip, box: {a:{x,y}, b:{x,y}}}` in place of
//!   `bubble` (both or neither is a bad-params error) makes one closed 4-vertex `LwPolyline`,
//!   linetype `DASHED` (loaded when missing), encircling the area, with no leader — its bubble sits
//!   on the box's upper-right corner ([`crate::bubbles`], LAMB T684); its tag point is
//!   [`DetailTag::anchor`]. The box must enclose the tag point and be wider and taller than
//!   `TOLERANCE.coincidence_mm`. Interactive: `Box` → `Specify first corner of the callout box:` →
//!   `Specify opposite corner:` (a refused box asks for the first corner again).
//! - A tag's flip (LAMB T615): `detailtag {…, flip?: bool}` sets [`DetailTag::flip`]; interactively
//!   `Flip` at the edge tag point prompt (`Specify tag point on a wall face or [Flip]:`) toggles it
//!   for the tag being placed. `detailflip {handle}` toggles an existing tag's flip in the edited
//!   view, one undo step, returning `{"handle", "flip"}`; interactive `Select detail tag to flip:`.
//!   What the flip does to a wall chain's outside: [`crate::outside`].
//! - `detailtags {viewId?}`: the resolution report for a plan or section view (default: the edited one) —
//!   `<12 runs: 3 tagged, 7 spread, 2 untyped; 4 junctions: 4 spread>`, echoed and returned.
//!
//! Reading tags back ([`view_detail_tags`]): an edge tag's tip attaches to the run whose centreline
//! is nearest, if within `width / 2 + TOLERANCE.grouping_mm`; a corner tag's tip to the nearest
//! junction within its corner reach (`max over gaps of (max arm width / 2) / sin(gap / 2)`, so
//! every face-corner vertex) `+ TOLERANCE.grouping_mm`, else an opening bridge, else — in a plan
//! only (LAMB T617) — a wall's free end within `width / 2 + TOLERANCE.grouping_mm` of its
//! centreline end ([`attach_free_end`]; `{"end": n}`), a termination resolved by
//! [`resolve_view_ends`]. Anything else — a tip left in open space
//! by a later edit, a detail view that no longer exists or is drawn in the other direction — is `unattached`. [`resolve_view_details`] then runs
//! lamb-core's greedy resolution (edges and corners). Computed on demand, never stored.

use cadcraft_doc::{Drawing, EntityKind, Handle, Leader, Line, LwPolyline};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::{PolyVertex, Vec2};
use lamb_core::derivation::detail_tags::{
    CornerTag, EdgeTag, EndTag, OpeningEndTag, OpeningTag, RunDetail, resolve_corner_details, resolve_edge_details, resolve_end_details,
    resolve_opening_details, resolve_opening_end_details,
};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::derivation::wall_junctions::{Junction, RunEnd, WallGraph, wall_graph};
use lamb_core::derivation::wall_spaces::{SideParams, WallSides, wall_sides};
use lamb_core::derivation::walls::{RunCurve, WallRunParams};
use lamb_core::geometry::Point2;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AnyView, CutDirection, Id};
use serde_json::{Value, json};

use crate::commands::{bad, edited_view, id_value, other, point_param};
use crate::project::DrawingProject;
use crate::schema::{EntityData, assembly_block_name, read_document, read_entity_data, set_entity_data, view_block_name};

pub use crate::schema::DetailTag;

/// One plan view's tags, attached: edge tags to runs, corner tags to junctions of the view's
/// [`WallGraph`]; `detail` is the detail view id in decimal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewTags {
    pub edges: Vec<EdgeTag>,
    pub corners: Vec<CornerTag>,
    /// Corner-shaped tags whose tip attaches to no junction but to an opening bridge (LAMB T486).
    pub openings: Vec<OpeningTag>,
    /// In a vertical view, opening tags instead attach to the bridge end their tip is nearer — a
    /// head or a sill (LAMB T505); `openings` stays empty there.
    pub opening_ends: Vec<OpeningEndTag>,
    /// In a plan view, corner-shaped tags that attach to no junction or bridge but to a wall's free
    /// end — a termination (LAMB T617).
    pub ends: Vec<EndTag>,
    /// Tags whose tip attaches to nothing, or whose detail view no longer exists.
    pub unattached: Vec<Handle>,
}

fn starts_with_ci(name: &str, prefix: &str) -> bool {
    name.len() >= prefix.len() && name.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn dist(a: Point2, b: Point2) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

/// Distance from `p` to a run's centreline.
fn curve_distance(c: &RunCurve, p: Point2) -> f64 {
    match *c {
        RunCurve::Line { from, to } => {
            let (dx, dy) = (to.x - from.x, to.y - from.y);
            let len2 = dx * dx + dy * dy;
            let t = if len2 > 0.0 { (((p.x - from.x) * dx + (p.y - from.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            dist(p, Point2 { x: from.x + t * dx, y: from.y + t * dy })
        }
        RunCurve::Arc { center, radius, start_angle, sweep } => {
            let a = (p.y - center.y).atan2(p.x - center.x);
            let tau = std::f64::consts::TAU;
            let inside = if sweep >= 0.0 { (a - start_angle).rem_euclid(tau) <= sweep } else { (start_angle - a).rem_euclid(tau) <= -sweep };
            if inside {
                return (dist(p, center) - radius).abs();
            }
            let at = |ang: f64| Point2 { x: center.x + radius * ang.cos(), y: center.y + radius * ang.sin() };
            dist(p, at(start_angle)).min(dist(p, at(start_angle + sweep)))
        }
    }
}

/// The run an edge tag at `tip` attaches to.
pub(crate) fn attach_run(graph: &WallGraph, tip: Point2) -> Option<usize> {
    let (i, d) = graph.runs.iter().enumerate().map(|(i, r)| (i, curve_distance(&r.centreline, tip))).min_by(|a, b| a.1.total_cmp(&b.1))?;
    (d <= graph.runs[i].width / 2.0 + TOLERANCE.grouping_mm).then_some(i)
}

/// How far junction `j`'s face-corner vertices sit from its point (LAMB T590): the largest
/// `(max arm width / 2) / sin(gap / 2)` over its gaps — 1.41 × w/2 at an L. `sin` is floored at
/// 0.2 so a sliver gap stays bounded, and a dead end's 360° gap counts as w/2.
fn corner_reach(graph: &WallGraph, j: &Junction) -> f64 {
    let half = j.arms.iter().filter_map(|a| graph.runs.get(a.run)).map(|r| r.width).fold(0.0, f64::max) / 2.0;
    j.gaps_deg
        .iter()
        .map(|g| {
            let s = (g / 2.0).to_radians().sin().abs();
            if s < 1e-9 { half } else { half / s.max(0.2) }
        })
        .fold(half, f64::max)
}

/// The junction a corner tag at `tip` attaches to: the nearest one within
/// [`corner_reach`]` + TOLERANCE.grouping_mm` of it.
pub(crate) fn attach_junction(graph: &WallGraph, tip: Point2) -> Option<usize> {
    graph
        .junctions
        .iter()
        .enumerate()
        .filter_map(|(i, j)| {
            let d = dist(j.point, tip);
            (d <= corner_reach(graph, j) + TOLERANCE.grouping_mm).then_some((i, d))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Distance from `p` to the nearer of a run's two face lines — its centreline offset by
/// `width / 2` either way, extended past the run's ends (a face circle for an arc run).
fn face_line_distance(c: &RunCurve, width: f64, p: Point2) -> f64 {
    let off = match *c {
        RunCurve::Line { from, to } => {
            let len = dist(from, to);
            if len <= 0.0 {
                return f64::INFINITY;
            }
            ((p.x - from.x) * (to.y - from.y) - (p.y - from.y) * (to.x - from.x)).abs() / len
        }
        RunCurve::Arc { center, radius, .. } => (dist(p, center) - radius).abs(),
    };
    (off - width / 2.0).abs()
}

/// `true` when `tip` is a face-corner vertex: within a junction's corner reach and within
/// `TOLERANCE.grouping_mm` of the face lines of two of its arms' runs.
fn on_face_corner(graph: &WallGraph, tip: Point2) -> bool {
    graph.junctions.iter().any(|j| {
        if dist(j.point, tip) > corner_reach(graph, j) + TOLERANCE.grouping_mm {
            return false;
        }
        let mut runs: Vec<usize> = j.arms.iter().map(|a| a.run).collect();
        runs.sort_unstable();
        runs.dedup();
        runs.iter().filter_map(|&r| graph.runs.get(r)).filter(|r| face_line_distance(&r.centreline, r.width, tip) <= TOLERANCE.grouping_mm).count()
            >= 2
    })
}

/// What a new tag at its tip attaches to (LAMB T590).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Target {
    Run(usize),
    Junction(usize),
    Opening(usize),
    /// A wall's free end ([`WallGraph::free_ends`]), in a plan view (LAMB T617).
    End(usize),
}

impl Target {
    fn json(self) -> Value {
        match self {
            Target::Run(i) => json!({ "run": i }),
            Target::Junction(i) => json!({ "junction": i }),
            Target::Opening(i) => json!({ "opening": i }),
            Target::End(i) => json!({ "end": i }),
        }
    }
    fn words(self) -> String {
        match self {
            Target::Run(i) => format!("run {i}"),
            Target::Junction(i) => format!("junction {i}"),
            Target::Opening(i) => format!("opening {i}"),
            Target::End(i) => format!("end {i}"),
        }
    }
}

/// Where a new edge (or `corner`) tag at `tip` attaches in `graph`, or the refusal. A corner tag
/// that misses every junction and bridge attaches to a free end ([`attach_free_end`]) only when
/// `ends` — in a plan view; a vertical view's free ends take no tag.
pub(crate) fn place_target(graph: &WallGraph, corner: bool, ends: bool, tip: Point2) -> std::result::Result<Target, String> {
    if graph.runs.is_empty() {
        return Err("No walls here: wall faces are pairs of parallel lines on a WALL* layer (50–800 mm apart)".into());
    }
    if corner {
        if let Some(j) = attach_junction(graph, tip) {
            return Ok(Target::Junction(j));
        }
        if let Some(b) = attach_bridge(graph, tip) {
            return Ok(Target::Opening(b));
        }
        if !ends {
            return Err("Not at a wall corner: put the tag point on the corner of two walls".into());
        }
        return attach_free_end(graph, tip)
            .map(Target::End)
            .ok_or_else(|| "Not at a wall corner or wall end: put the tag point on the corner of two walls, or on a wall's end".into());
    }
    if on_face_corner(graph, tip) {
        return Err("That point is the corner of two walls: pick a point along one face, or Corner for a corner detail".into());
    }
    attach_run(graph, tip).map(Target::Run).ok_or_else(|| "Not on a wall face: put the tag point on a wall line".into())
}

/// [`place_target`] in `view`.
fn view_target(s: &Session, view: Id, corner: bool, tip: Point2) -> std::result::Result<Target, String> {
    let p = DrawingProject::new(s.doc().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let graph = wall_graph(&p, view, &wall_params_with(&p, view, &[tip]));
    place_target(&graph, corner, view_direction(p.drawing(), view) == CutDirection::Plan, tip).map_err(|e| {
        // A tip on a line that makes no wall: say which layer and why (LAMB T681).
        let lone = graph.runs.is_empty() || (!corner && !on_face_corner(&graph, tip) && attach_run(&graph, tip).is_none());
        match lines_at(p.drawing(), view, tip).into_iter().next() {
            Some((layer, _)) if lone => format!("No wall here: {layer} has no parallel face 50–800 mm from this line"),
            _ => e,
        }
    })
}

/// A run's centreline end point at `end`.
pub(crate) fn run_end_point(c: &RunCurve, end: RunEnd) -> Point2 {
    match (*c, end) {
        (RunCurve::Line { from, .. }, RunEnd::Start) => from,
        (RunCurve::Line { to, .. }, RunEnd::End) => to,
        (RunCurve::Arc { center, radius, start_angle, sweep }, e) => {
            let a = if e == RunEnd::Start { start_angle } else { start_angle + sweep };
            Point2 { x: center.x + radius * a.cos(), y: center.y + radius * a.sin() }
        }
    }
}

/// The free end a corner-shaped tag at `tip` attaches to when no junction or bridge does (LAMB
/// T617): the nearest [`WallGraph::free_ends`] entry whose centreline end point is within
/// `run.width / 2 + TOLERANCE.grouping_mm` of the tip.
pub(crate) fn attach_free_end(graph: &WallGraph, tip: Point2) -> Option<usize> {
    graph
        .free_ends
        .iter()
        .enumerate()
        .filter_map(|(i, &(r, end))| {
            let run = graph.runs.get(r)?;
            let d = dist(run_end_point(&run.centreline, end), tip);
            (d <= run.width / 2.0 + TOLERANCE.grouping_mm).then_some((i, d))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// The opening bridge a corner-shaped tag at `tip` attaches to when no junction does (LAMB T486):
/// the nearest bridge whose segment `from → to` is within `max(its two runs' widths) / 2 +
/// TOLERANCE.grouping_mm` of the tip.
pub(crate) fn attach_bridge(graph: &WallGraph, tip: Point2) -> Option<usize> {
    graph
        .bridges
        .iter()
        .enumerate()
        .filter_map(|(i, b)| {
            let w = [b.a.0, b.b.0].iter().filter_map(|&r| graph.runs.get(r)).map(|r| r.width).fold(0.0, f64::max);
            let d = curve_distance(&RunCurve::Line { from: b.from, to: b.to }, tip);
            (d <= w / 2.0 + TOLERANCE.grouping_mm).then_some((i, d))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// The view's wall graph and sides, with `WALL-EXT*` layers exterior, the view's outside pick
/// points ([`crate::outside::outside_points`]) marking their regions outside, and then each edge
/// tag's side carried along its wall chain ([`crate::outside::apply_tag_sides`], LAMB T615).
pub(crate) fn graph_and_sides(p: &DrawingProject<'_>, view: Id) -> (WallGraph, WallSides) {
    let graph = wall_graph(p, view, &wall_params(p, view));
    let mut sides = wall_sides(
        p,
        &graph,
        &SideParams { outside_points: crate::outside::outside_points(p.drawing(), view), exterior_layer_ids: exterior_layer_ids(p) },
    );
    crate::outside::apply_tag_sides(p.drawing(), view, &graph, &mut sides);
    (graph, sides)
}

/// The ids of the layers whose faces make a run exterior: names starting `WALL-EXT`.
pub(crate) fn exterior_layer_ids(p: &DrawingProject<'_>) -> Vec<String> {
    p.layers().iter().filter(|l| starts_with_ci(&l.name, "WALL-EXT")).map(|l| l.id.clone()).collect()
}

/// What counts as a wall face in `view`: [`WallRunParams::for_vertical`] in a vertical view, else
/// [`WallRunParams::defaults`] — plus the layer of every line a detail tag in `view` sits on
/// ([`wall_params_with`], LAMB T681).
pub(crate) fn wall_params(p: &DrawingProject<'_>, view: Id) -> WallRunParams {
    wall_params_with(p, view, &[])
}

/// [`wall_params`] with `extra_tips` counted as tag tips too — the tag DETAILTAG is placing. Each
/// tip (a callout box's anchor) adds the layer of every Line / LwPolyline / Arc in `view`'s block
/// within `TOLERANCE.grouping_mm` of it, skipping detail tags, outside points and LAMB-internal
/// layers, so a wall drawn on `A-WALL` or `0` is a wall in the view where it is tagged.
pub(crate) fn wall_params_with(p: &DrawingProject<'_>, view: Id, extra_tips: &[Point2]) -> WallRunParams {
    let vertical = read_document(p.drawing()).is_ok_and(|doc| doc.views.iter().any(|v| matches!(v, AnyView::Vertical(_)) && v.id() == view));
    let mut params = if vertical { WallRunParams::for_vertical(p.layers()) } else { WallRunParams::defaults(p.layers()) };
    let mut tips: Vec<Point2> = tags_in(p.drawing(), view).unwrap_or_default().into_iter().map(|(_, tip, _)| tip).collect();
    tips.extend_from_slice(extra_tips);
    for tip in tips {
        for (layer, _) in lines_at(p.drawing(), view, tip) {
            if !params.wall_layer_ids.contains(&layer) {
                params.wall_layer_ids.push(layer);
            }
        }
    }
    params
}

/// `(layer, distance)` of every Line / LwPolyline / Arc in `view`'s block within
/// `TOLERANCE.grouping_mm` of `tip`, nearest first — not detail tags, outside points or entities on
/// LAMB-internal layers.
fn lines_at(d: &Drawing, view: Id, tip: Point2) -> Vec<(String, f64)> {
    use lamb_core::geometry::polyline::{closest_point_on_segment, resolved_segments};
    let Some(block) = d.block(&view_block_name(view)) else { return Vec::new() };
    let mut out: Vec<(String, f64)> = Vec::new();
    for e in block.entities.iter() {
        if !matches!(e.kind, EntityKind::Line(_) | EntityKind::LwPolyline(_) | EntityKind::Arc(_)) {
            continue;
        }
        let layer = e.common.layer.as_str();
        if layer.eq_ignore_ascii_case(crate::outside::OUTSIDE_LAYER) || crate::views::internal_layer(d, layer) {
            continue;
        }
        if read_entity_data(&e.common).ok().and_then(|x| x.detail_tag).is_some() {
            continue;
        }
        let Ok(c) = crate::convert::to_polyline(e) else { continue };
        let Some(d) = resolved_segments(&c.polyline).iter().map(|s| closest_point_on_segment(s, tip).distance).min_by(f64::total_cmp) else {
            continue;
        };
        if d <= TOLERANCE.grouping_mm {
            out.push((layer.to_string(), d));
        }
    }
    out.sort_by(|a, b| a.1.total_cmp(&b.1));
    out
}

/// The cut direction details tagging `view` must have: `Vertical` in a vertical view, else `Plan`.
pub(crate) fn view_direction(d: &Drawing, view: Id) -> CutDirection {
    let vertical = read_document(d).is_ok_and(|doc| doc.views.iter().any(|v| matches!(v, AnyView::Vertical(_)) && v.id() == view));
    if vertical { CutDirection::Vertical } else { CutDirection::Plan }
}

/// Every detail tag in `view`'s block: (handle, tip, tag). A leader's tip is `vertices[0]`; a
/// callout box's (a tagged `LwPolyline`) its `anchor` — one without an anchor is skipped.
pub(crate) fn tags_in(d: &Drawing, view: Id) -> std::result::Result<Vec<(Handle, Point2, DetailTag)>, String> {
    let Some(block) = d.block(&view_block_name(view)) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for e in block.entities.iter() {
        if !matches!(e.kind, EntityKind::Leader(_) | EntityKind::LwPolyline(_)) {
            continue;
        }
        let Some(tag) = read_entity_data(&e.common).map_err(|e| e.to_string())?.detail_tag else { continue };
        let tip = match (&e.kind, tag.anchor) {
            (EntityKind::Leader(l), _) => l.vertices.first().map(|v| Point2 { x: v.x, y: v.y }),
            (EntityKind::LwPolyline(_), Some([x, y])) => Some(Point2 { x, y }),
            _ => None,
        };
        let Some(tip) = tip else { continue };
        out.push((e.handle, tip, tag));
    }
    Ok(out)
}

/// A callout box's four corners from two opposite ones, counter-clockwise from the lower left.
pub(crate) fn box_vertices(a: Point2, b: Point2) -> Vec<PolyVertex> {
    let (x0, x1, y0, y1) = (a.x.min(b.x), a.x.max(b.x), a.y.min(b.y), a.y.max(b.y));
    [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
        .iter()
        .map(|&(x, y)| PolyVertex { p: Vec2::new(x, y), bulge: 0.0, start_width: 0.0, end_width: 0.0 })
        .collect()
}

/// The refusal for a callout box from `a` to `b` around `tip`, if any.
fn box_refusal(tip: Point2, a: Point2, b: Point2) -> Option<&'static str> {
    let t = TOLERANCE.coincidence_mm;
    let (x0, x1, y0, y1) = (a.x.min(b.x), a.x.max(b.x), a.y.min(b.y), a.y.max(b.y));
    if x1 - x0 <= t || y1 - y0 <= t {
        return Some("The box needs two opposite corners apart in both directions");
    }
    (tip.x < x0 - t || tip.x > x1 + t || tip.y < y0 - t || tip.y > y1 + t).then_some("The box must enclose the tag point")
}

/// What a new tag draws besides its tip: a leader to a bubble point, or a callout box.
#[derive(Clone, Copy)]
enum Shape {
    Bubble(Point2),
    Box(Point2, Point2),
}

fn graph_tags(d: &Drawing, view: Id, graph: &WallGraph) -> std::result::Result<ViewTags, String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    let want = view_direction(d, view);
    let is_detail = |id: Id| doc.views.iter().any(|v| matches!(v, AnyView::Detail(x) if x.id == id && x.cut_direction == want));
    let mut out = ViewTags::default();
    for (h, tip, tag) in tags_in(d, view)? {
        let detail = tag.detail_view_id.0.to_string();
        let attached = if !is_detail(tag.detail_view_id) {
            false
        } else if tag.corner {
            if let Some(junction) = attach_junction(graph, tip) {
                out.corners.push(CornerTag { detail, junction });
                true
            } else {
                match attach_bridge(graph, tip) {
                    Some(bridge) if want == CutDirection::Vertical => {
                        let b = &graph.bridges[bridge];
                        let at_b = dist(tip, b.to) < dist(tip, b.from);
                        out.opening_ends.push(OpeningEndTag { detail, bridge, at_b });
                        true
                    }
                    Some(bridge) => {
                        out.openings.push(OpeningTag { detail, bridge });
                        true
                    }
                    None if want == CutDirection::Plan => attach_free_end(graph, tip).map(|end| out.ends.push(EndTag { detail, end })).is_some(),
                    None => false,
                }
            }
        } else {
            attach_run(graph, tip).map(|run| out.edges.push(EdgeTag { detail, run })).is_some()
        };
        if !attached {
            out.unattached.push(h);
        }
    }
    Ok(out)
}

/// `Ok` for a plan view only — ASSEMBLYPLAN, WALLCONFLICTS, WALLRUN and WALLFIT ([`edited_plan`]).
fn plan_view(d: &Drawing, view: Id) -> std::result::Result<(), String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    match doc.views.iter().find(|v| v.id() == view) {
        Some(AnyView::Plan(_)) => Ok(()),
        Some(_) => Err("Detail tags live in plan views".into()),
        None => Err("No such view".into()),
    }
}

/// `Ok` for a plan or vertical view — the views with a wall graph.
fn wall_view(d: &Drawing, view: Id) -> std::result::Result<(), String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    match doc.views.iter().find(|v| v.id() == view) {
        Some(AnyView::Plan(_) | AnyView::Vertical(_)) => Ok(()),
        Some(_) => Err("Detail tags live in plan and section views".into()),
        None => Err("No such view".into()),
    }
}

/// `view`'s detail tags, attached to its wall graph's runs and junctions.
pub fn view_detail_tags(d: &Drawing, view: Id) -> std::result::Result<ViewTags, String> {
    wall_view(d, view)?;
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let graph = wall_graph(&p, view, &wall_params(&p, view));
    graph_tags(d, view, &graph)
}

/// Every run's edge detail and every junction's corner detail in `view` (parallel to its wall
/// graph's `runs` and `junctions`).
pub fn resolve_view_details(d: &Drawing, view: Id) -> std::result::Result<(Vec<RunDetail>, Vec<RunDetail>), String> {
    resolve_with_tags(d, view).map(|(runs, junctions, _)| (runs, junctions))
}

/// Every opening bridge's opening detail in `view` (parallel to its wall graph's `bridges`, LAMB
/// T486). In a vertical view every bridge is `Untyped` — its ends resolve separately
/// ([`resolve_view_opening_ends`]).
pub fn resolve_view_openings(d: &Drawing, view: Id) -> std::result::Result<Vec<RunDetail>, String> {
    wall_view(d, view)?;
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let (graph, sides) = graph_and_sides(&p, view);
    let tags = graph_tags(d, view, &graph)?;
    Ok(resolve_opening_details(&graph, &sides, &tags.openings))
}

/// Every free end's termination detail in `view` (parallel to its wall graph's `free_ends`, LAMB
/// T617). In a vertical view no end carries a tag, so every end is `Untyped`.
pub fn resolve_view_ends(d: &Drawing, view: Id) -> std::result::Result<Vec<RunDetail>, String> {
    wall_view(d, view)?;
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let (graph, sides) = graph_and_sides(&p, view);
    let tags = graph_tags(d, view, &graph)?;
    Ok(resolve_end_details(&graph, &sides, &tags.ends))
}

/// Which end of each bridge is higher: `Some(true)` when `from` (the a end) is, `Some(false)` when
/// `to` is, `None` within `TOLERANCE.coincidence_mm` — a vertical view's local y is z.
pub fn bridge_upper(graph: &WallGraph) -> Vec<Option<bool>> {
    graph
        .bridges
        .iter()
        .map(|b| {
            let dz = b.from.y - b.to.y;
            (dz.abs() > TOLERANCE.coincidence_mm).then_some(dz > 0.0)
        })
        .collect()
}

/// `[a-end, b-end]` opening details per bridge of a vertical `view` (LAMB T505): head and sill
/// resolve separately. In a plan view, every end is `Untyped`.
pub fn resolve_view_opening_ends(d: &Drawing, view: Id) -> std::result::Result<Vec<[RunDetail; 2]>, String> {
    wall_view(d, view)?;
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let (graph, sides) = graph_and_sides(&p, view);
    let tags = graph_tags(d, view, &graph)?;
    Ok(resolve_opening_end_details(&graph, &sides, &tags.opening_ends, &bridge_upper(&graph)))
}

fn resolve_with_tags(d: &Drawing, view: Id) -> std::result::Result<(Vec<RunDetail>, Vec<RunDetail>, ViewTags), String> {
    resolve_all(d, view).map(|(runs, junctions, _, tags)| (runs, junctions, tags))
}

/// Runs, junctions, and — in a vertical view only, flattened a-then-b per bridge — opening ends.
#[allow(clippy::type_complexity)]
fn resolve_all(d: &Drawing, view: Id) -> std::result::Result<(Vec<RunDetail>, Vec<RunDetail>, Vec<RunDetail>, ViewTags), String> {
    wall_view(d, view)?;
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let (graph, sides) = graph_and_sides(&p, view);
    let tags = graph_tags(d, view, &graph)?;
    let runs = resolve_edge_details(&graph, &sides, &tags.edges);
    let junctions = resolve_corner_details(&graph, &sides, &tags.corners);
    let ends = if view_direction(d, view) == CutDirection::Vertical {
        resolve_opening_end_details(&graph, &sides, &tags.opening_ends, &bridge_upper(&graph)).into_iter().flatten().collect()
    } else {
        Vec::new()
    };
    Ok((runs, junctions, ends, tags))
}

/// `3 tagged, 7 spread, 2 untyped` — the non-zero kinds, in a fixed order.
fn counts(xs: &[RunDetail]) -> String {
    let mut n = [0usize; 5];
    for x in xs {
        n[match x {
            RunDetail::Tagged(_) => 0,
            RunDetail::Spread(_) => 1,
            RunDetail::Gap(_) => 2,
            RunDetail::Conflict => 3,
            RunDetail::Untyped => 4,
        }] += 1;
    }
    let words = ["tagged", "spread", "gap", "conflict", "untyped"];
    n.iter().zip(words).filter(|(c, _)| **c > 0).map(|(c, w)| format!("{c} {w}")).collect::<Vec<_>>().join(", ")
}

fn kind_json(x: &RunDetail) -> Value {
    match x {
        RunDetail::Tagged(d) => json!({ "kind": "tagged", "detail": d }),
        RunDetail::Spread(d) => json!({ "kind": "spread", "detail": d }),
        RunDetail::Gap(d) => json!({ "kind": "gap", "detail": d }),
        RunDetail::Conflict => json!({ "kind": "conflict" }),
        RunDetail::Untyped => json!({ "kind": "untyped" }),
    }
}

/// The report line: `<12 runs: 3 tagged, …; 4 junctions: 4 spread; 1 unattached>`.
pub fn report(runs: &[RunDetail], junctions: &[RunDetail], unattached: usize) -> String {
    let part = |n: usize, word: &str, xs: &[RunDetail]| {
        let c = counts(xs);
        if c.is_empty() { format!("{n} {word}") } else { format!("{n} {word}: {c}") }
    };
    let mut s = format!("<{}; {}", part(runs.len(), "runs", runs), part(junctions.len(), "junctions", junctions));
    if unattached > 0 {
        s.push_str(&format!("; {unattached} unattached"));
    }
    s.push('>');
    s
}

/// DETAILTAGS: the resolution report for `viewId` (default: the edited view). Not an undo step.
pub(crate) fn detailtags(s: &mut Session, p: &Value) -> Result<Value> {
    let view = match p.get("viewId").filter(|v| !v.is_null()) {
        Some(v) => id_value(v).ok_or_else(|| other("No such view"))?,
        None => edited_view(s).ok_or_else(|| other("Edit a plan or section view first (EDITVIEW)"))?,
    };
    let (runs, junctions, ends, tags) = resolve_all(s.doc()?, view).map_err(other)?;
    let mut message = report(&runs, &junctions, tags.unattached.len());
    if !ends.is_empty() {
        // A section's opening ends (LAMB T505), before the unattached count.
        let c = counts(&ends);
        let part = format!("; {} opening ends: {c}", ends.len());
        let at = message.find("; ").and_then(|i| message[i + 2..].find("; ").map(|j| i + 2 + j)).unwrap_or(message.len() - 1);
        message.insert_str(at, &part);
    }
    s.echo(message.clone());
    Ok(json!({
        "message": message,
        "runs": runs.iter().map(kind_json).collect::<Vec<_>>(),
        "junctions": junctions.iter().map(kind_json).collect::<Vec<_>>(),
        "openingEnds": ends.iter().map(kind_json).collect::<Vec<_>>(),
        "unattached": tags.unattached.iter().map(|h| format!("{:X}", h.0)).collect::<Vec<_>>(),
    }))
}

/// The edited view, if it is a plan view; else the refusal.
pub(crate) fn edited_plan(s: &Session) -> std::result::Result<Id, String> {
    let view = edited_view(s).ok_or("Edit a plan view first (EDITVIEW)")?;
    plan_view(s.doc().map_err(|e| e.to_string())?, view)?;
    Ok(view)
}

/// The edited view, if it is a plan or vertical view; else the refusal.
pub(crate) fn edited_wall_view(s: &Session) -> std::result::Result<Id, String> {
    let view = edited_view(s).ok_or("Edit a plan or section view first (EDITVIEW)")?;
    wall_view(s.doc().map_err(|e| e.to_string())?, view)?;
    Ok(view)
}

/// `id`'s cut direction, if it is a detail view.
fn detail_direction(s: &Session, id: Id) -> Option<CutDirection> {
    let doc = read_document(s.doc().ok()?).ok()?;
    doc.views.iter().find_map(|v| match v {
        AnyView::Detail(x) if x.id == id => Some(x.cut_direction),
        _ => None,
    })
}

/// DETAILTAG's JSON form: a two-vertex leader from `tip` to `bubble`, or a dashed callout `box`
/// around `tip`, in the edited plan or section view.
pub(crate) fn detailtag(s: &mut Session, p: &Value) -> Result<Value> {
    let detail = p.get("detailViewId").and_then(id_value).ok_or_else(|| bad("detailtag", "`detailViewId` is required"))?;
    let corner_given = p.get("corner").and_then(Value::as_bool);
    let flip = p.get("flip").and_then(Value::as_bool).unwrap_or(false);
    let tip = point_param("detailtag", p, "tip")?;
    let given = |k: &str| p.get(k).filter(|v| !v.is_null());
    let shape = match (given("bubble"), given("box")) {
        (Some(_), None) => Shape::Bubble(point_param("detailtag", p, "bubble")?),
        (None, Some(b)) => Shape::Box(point_param("detailtag", b, "a")?, point_param("detailtag", b, "b")?),
        (Some(_), Some(_)) => return Err(bad("detailtag", "give `bubble` or `box`, not both")),
        (None, None) => return Err(bad("detailtag", "`bubble` or `box` is required")),
    };
    // Every refusal before `doc_mut`, so a refusal records nothing.
    let view = edited_wall_view(s).map_err(other)?;
    let Some(dir) = detail_direction(s, detail) else { return Err(other("No such detail view")) };
    match (dir, view_direction(s.doc()?, view)) {
        (CutDirection::Vertical, CutDirection::Plan) => return Err(other("That detail is drawn in section; tag it in a section view")),
        (CutDirection::Plan, CutDirection::Vertical) => return Err(other("That detail is drawn in plan; tag it in a plan view")),
        _ => {}
    }
    // The detail's kind (from its reference, LAMB T674/T683): an anchored detail (corner
    // reference) tags a wall corner, opening or free end, never a face; a linear one tags faces
    // only. No reference yet accepts either. Tags already placed are not re-checked.
    let kind = crate::detailref::detail_is_anchored(s.doc()?, detail);
    let corner = corner_given.or(kind).unwrap_or(false);
    let detail_name =
        || read_document(s.doc()?).map_err(other).map(|d| d.views.iter().find(|v| v.id() == detail).map(|v| v.name().to_owned()).unwrap_or_default());
    if kind == Some(true) && !corner {
        return Err(other(format!("{} is an anchored detail: tag a wall corner, opening or wall end with it", detail_name()?)));
    }
    let target = view_target(s, view, corner, Point2 { x: tip.x, y: tip.y }).map_err(other)?;
    if kind == Some(false) && !matches!(target, Target::Run(_)) {
        return Err(other(format!(
            "{} is a linear detail: tag a wall face with it (a corner, jamb or end detail needs a corner reference — DETAILREF)",
            detail_name()?
        )));
    }
    if let Shape::Box(a, b) = shape
        && let Some(e) = box_refusal(tip, a, b)
    {
        return Err(other(e));
    }
    let mut common = s.current_common();
    let anchor = matches!(shape, Shape::Box(..)).then_some([tip.x, tip.y]);
    set_entity_data(
        &mut common,
        &EntityData { detail_tag: Some(DetailTag { detail_view_id: detail, corner, anchor, flip }), ..EntityData::default() },
    )
    .map_err(other)?;
    let space = s.space();
    // The tag and, for a view's first, its assembly plan land together: one undo step.
    let mut after = s.doc()?.clone();
    let kind = match shape {
        Shape::Bubble(bubble) => {
            let style = after.header.str("DIMSTYLE", "Standard");
            EntityKind::Leader(Leader {
                vertices: vec![Vec2::new(tip.x, tip.y).to3(0.0), Vec2::new(bubble.x, bubble.y).to3(0.0)],
                arrow: true,
                spline: false,
                style,
            })
        }
        Shape::Box(a, b) => {
            if after.linetype("DASHED").is_none()
                && let Some(lt) = cadcraft_doc::library::standard_linetypes_for(&after).into_iter().find(|l| l.name == "DASHED")
            {
                after.linetypes.push(lt);
            }
            common.linetype = "DASHED".into();
            EntityKind::LwPolyline(LwPolyline { vertices: box_vertices(a, b), closed: true, const_width: 0.0, elevation: 0.0, plinegen: false })
        }
    };
    let h = after.add(&space, common, kind)?;
    let mut notes = Vec::new();
    if after.block(&assembly_block_name(view)).is_none()
        && let Err(e) = crate::assembly::write_assembly_block(&mut after, view)
    {
        notes.push(format!("LAMB assembly: {e}"));
    }
    *s.doc_mut()? = after;
    let doc = read_document(s.doc()?).map_err(other)?;
    let name = |id: Id| doc.views.iter().find(|v| v.id() == id).map(|v| v.name().to_owned()).unwrap_or_else(|| id.0.to_string());
    let tagged = format!("Tagged {} of \"{}\" with \"{}\"", target.words(), name(view), name(detail));
    s.echo(tagged);
    if let Ok((runs, junctions, tags)) = resolve_with_tags(s.doc()?, view) {
        s.echo(report(&runs, &junctions, tags.unattached.len()));
    }
    for n in notes {
        s.echo(n);
    }
    Ok(json!({ "handle": format!("{:X}", h.0), "attached": target.json() }))
}

fn parse_handle(v: &Value) -> Option<Handle> {
    v.as_str().and_then(Handle::parse_hex).or_else(|| v.as_u64().map(Handle))
}

/// DETAILFLIP's JSON form: toggles the flip of detail tag `handle` in the edited plan or section
/// view, one undo step. Returns `{"handle", "flip"}`.
pub(crate) fn detailflip(s: &mut Session, p: &Value) -> Result<Value> {
    let h = p.get("handle").and_then(parse_handle).ok_or_else(|| bad("detailflip", "`handle` is required"))?;
    // Every refusal before `doc_mut`, so a refusal records nothing.
    let view = edited_wall_view(s).map_err(other)?;
    let Some((_, _, tag)) = tags_in(s.doc()?, view).map_err(other)?.into_iter().find(|(th, _, _)| *th == h) else {
        return Err(other("That is not a detail tag in this view"));
    };
    let flip = !tag.flip;
    let mut res = Ok(());
    s.doc_mut()?
        .modify_entity(h, |e| {
            res = read_entity_data(&e.common).and_then(|mut data| {
                if let Some(t) = data.detail_tag.as_mut() {
                    t.flip = flip;
                }
                set_entity_data(&mut e.common, &data)
            });
        })
        .map_err(other)?;
    res.map_err(other)?;
    s.echo(if flip { "Detail tag flipped: its wall chain's outside is the other side" } else { "Detail tag unflipped" }.to_owned());
    Ok(json!({ "handle": format!("{:X}", h.0), "flip": flip }))
}

/// DETAILFLIP: one pick.
struct DetailFlipM;

impl Interactive for DetailFlipM {
    fn name(&self) -> &'static str {
        "detailflip"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Select detail tag to flip", Accept::SELECT)
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
            Input::Pick(hs) => {
                let Some(h) = hs.first().copied() else { return Ok(Step::Continue) };
                if let Err(e) = detailflip(s, &json!({ "handle": format!("{:X}", h.0) })) {
                    s.echo(e.to_string());
                    return Ok(Step::Continue);
                }
                Ok(Step::Done)
            }
            Input::Cancel | Input::Enter => Ok(Step::Cancel),
            _ => Ok(Step::Continue),
        }
    }
}

pub(crate) fn detailflip_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(DetailFlipM))
}

/// The detail the prompt offers by default: the most recently placed tag's detail (highest handle,
/// any view) if that view still exists, else the drawing's only detail view.
fn default_detail(s: &Session) -> Option<Id> {
    let d = s.doc().ok()?;
    let doc = read_document(d).ok()?;
    let mut last: Option<(Handle, Id)> = None;
    for v in &doc.views {
        for (h, _, tag) in tags_in(d, v.id()).ok()? {
            if last.is_none_or(|(lh, _)| h.0 > lh.0) {
                last = Some((h, tag.detail_view_id));
            }
        }
    }
    let details: Vec<&AnyView> = doc.views.iter().filter(|v| matches!(v, AnyView::Detail(_))).collect();
    last.map(|(_, id)| id).filter(|id| details.iter().any(|v| v.id() == *id)).or_else(|| (details.len() == 1).then(|| details[0].id()))
}

/// A detail's name as one command-line keyword: `/`, `[`, `]` and spaces become `_`.
fn detail_keyword(name: &str) -> String {
    name.chars().map(|c| if matches!(c, '/' | '[' | ']') || c.is_whitespace() { '_' } else { c }).collect()
}

/// A detail DETAILTAG offers: its id, keyword and name.
struct Offered {
    id: Id,
    keyword: String,
    name: String,
    /// Its kind from its reference: `Some(true)` anchored, `Some(false)` linear, `None` no reference yet.
    anchored: Option<bool>,
}

/// DETAILTAG: detail (one keyword per detail drawn in the edited view's direction, `Corner`
/// toggles corner mode, which lists only anchored and reference-less details; a typed name or id
/// also selects), tag point, bubble point. The picked detail's kind sets the mode (LAMB T683):
/// anchored → corner, linear → face.
struct DetailTagM {
    default: Option<Id>,
    detail: Option<Id>,
    corner: bool,
    /// The edited view is a plan view (its free ends take tags).
    plan: bool,
    /// `Flip` was toggled at the tag point prompt (this tag only).
    flip: bool,
    tip: Option<Vec2>,
    offered: Vec<Offered>,
    /// `Box` was chosen at the bubble prompt (this tag only), and the box's first corner.
    boxing: bool,
    box_a: Option<Vec2>,
}

impl DetailTagM {
    fn default_name(&self) -> Option<String> {
        let id = self.default?;
        self.listed().find(|o| o.id == id).map(|o| o.name.clone())
    }
    /// The details the keyword list holds: all, or in corner mode the anchored and reference-less.
    fn listed(&self) -> impl Iterator<Item = &Offered> {
        self.offered.iter().filter(|o| !self.corner || o.anchored != Some(false))
    }
    /// Detail `id` is picked: its kind sets the mode.
    fn pick(&mut self, id: Id) {
        self.detail = Some(id);
        if let Some(a) = self.offered.iter().find(|o| o.id == id).and_then(|o| o.anchored) {
            self.corner = a;
        }
    }
}

impl Interactive for DetailTagM {
    fn name(&self) -> &'static str {
        "detailtag"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match (self.detail, self.tip) {
            (None, _) => {
                let msg = if self.corner { "Select anchored detail" } else { "Select detail" };
                let kws: Vec<&str> = self.listed().map(|o| o.keyword.as_str()).chain(["Corner"]).collect();
                let p = Prompt::new(msg, Accept::TEXT).kw(&kws).options_only();
                match self.default_name() {
                    Some(n) => p.default(n),
                    None => p,
                }
            }
            (Some(_), None) => {
                if self.corner {
                    let msg = if self.plan {
                        "Specify tag point at a wall corner, opening or wall end"
                    } else {
                        "Specify tag point at a wall corner or opening"
                    };
                    Prompt::new(msg, Accept::POINT)
                } else {
                    let msg = if self.flip { "Specify tag point on a wall face (flipped)" } else { "Specify tag point on a wall face" };
                    Prompt::new(msg, Accept::POINT).kw(&["Flip"])
                }
            }
            (Some(_), Some(tip)) => match (self.boxing, self.box_a) {
                (false, _) => Prompt::new("Specify bubble point", Accept::POINT).base(tip).kw(&["Box"]),
                (true, None) => Prompt::new("Specify first corner of the callout box", Accept::POINT),
                (true, Some(a)) => Prompt::new("Specify opposite corner", Accept::POINT).corner(a),
            },
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let view = match edited_wall_view(s) {
            Ok(v) => v,
            Err(e) => {
                s.echo(e);
                return Ok(Step::Done);
            }
        };
        let dir = view_direction(s.doc()?, view);
        self.plan = dir == CutDirection::Plan;
        let doc = read_document(s.doc()?).map_err(other)?;
        self.offered = doc
            .views
            .iter()
            .filter_map(|v| match v {
                AnyView::Detail(d) if d.cut_direction == dir => Some(Offered {
                    id: d.id,
                    keyword: detail_keyword(&d.name),
                    name: d.name.clone(),
                    anchored: crate::detailref::detail_is_anchored(s.doc().ok()?, d.id),
                }),
                _ => None,
            })
            .collect();
        if self.offered.is_empty() {
            let kind = if dir == CutDirection::Vertical { "section" } else { "plan" };
            s.echo(format!("No {kind} details in this drawing — make one with NEWDETAIL, draw it, then tag a wall with it"));
            return Ok(Step::Done);
        }
        // The last detail used, if drawn this way; else the only one offered.
        self.default =
            default_detail(s).filter(|id| self.offered.iter().any(|o| o.id == *id)).or_else(|| (self.offered.len() == 1).then(|| self.offered[0].id));
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(detail) = self.detail else {
            let text = match i {
                Input::Enter => match self.default {
                    Some(id) if self.listed().any(|o| o.id == id) => {
                        self.pick(id);
                        return Ok(Step::Continue);
                    }
                    _ => return Ok(Step::Cancel),
                },
                Input::Text(t) | Input::Keyword(t) => t.trim().to_owned(),
                _ => return Ok(Step::Continue),
            };
            if text.eq_ignore_ascii_case("corner") || text.eq_ignore_ascii_case("c") {
                self.corner = !self.corner;
                return Ok(Step::Continue);
            }
            let by_name = self.offered.iter().find(|o| o.keyword.eq_ignore_ascii_case(&text) || o.name.eq_ignore_ascii_case(&text));
            let by_id = || text.parse::<u64>().ok().and_then(|n| self.offered.iter().find(|o| o.id == Id(n)));
            if let Some(id) = by_name.or_else(by_id).map(|o| o.id) {
                self.pick(id);
                return Ok(Step::Continue);
            }
            // A detail drawn the other way gets the reason it is not offered.
            let doc = read_document(s.doc()?).map_err(other)?;
            let other_way = doc.views.iter().find_map(|v| match v {
                AnyView::Detail(d)
                    if detail_keyword(&d.name).eq_ignore_ascii_case(&text)
                        || d.name.eq_ignore_ascii_case(&text)
                        || text.parse::<u64>().is_ok_and(|n| d.id == Id(n)) =>
                {
                    Some(d.cut_direction)
                }
                _ => None,
            });
            return Err(other(match other_way {
                Some(CutDirection::Vertical) => "That detail is drawn in section; tag it in a section view",
                Some(_) => "That detail is drawn in plan; tag it in a plan view",
                None => "No such detail view",
            }));
        };
        if self.tip.is_none()
            && !self.corner
            && let Input::Text(t) | Input::Keyword(t) = &i
            && (t.trim().eq_ignore_ascii_case("flip") || t.trim().eq_ignore_ascii_case("f"))
        {
            self.flip = !self.flip;
            return Ok(Step::Continue);
        }
        if self.tip.is_some()
            && !self.boxing
            && let Input::Text(t) | Input::Keyword(t) = &i
            && (t.trim().eq_ignore_ascii_case("box") || t.trim().eq_ignore_ascii_case("b"))
        {
            self.boxing = true;
            return Ok(Step::Continue);
        }
        let Input::Point(p) = i else {
            return Ok(if i == Input::Enter { Step::Cancel } else { Step::Continue });
        };
        let Some(tip) = self.tip else {
            // A point that attaches to nothing is refused here and the tag point asked again.
            let view = edited_wall_view(s).map_err(other)?;
            match view_target(s, view, self.corner, Point2 { x: p.x, y: p.y }) {
                Ok(_) => self.tip = Some(p),
                Err(e) => s.echo(e),
            }
            return Ok(Step::Continue);
        };
        let mut params = json!({
            "detailViewId": detail.0,
            "corner": self.corner,
            "flip": self.flip,
            "tip": {"x": tip.x, "y": tip.y},
        });
        if self.boxing {
            let Some(a) = self.box_a else {
                self.box_a = Some(p);
                return Ok(Step::Continue);
            };
            params["box"] = json!({ "a": {"x": a.x, "y": a.y}, "b": {"x": p.x, "y": p.y} });
        } else {
            params["bubble"] = json!({"x": p.x, "y": p.y});
        }
        if let Err(e) = detailtag(s, &params) {
            s.echo(e.to_string());
            if self.boxing {
                // A refused box asks for its first corner again.
                self.box_a = None;
                return Ok(Step::Continue);
            }
        }
        Ok(Step::Done)
    }
    fn preview(&self, _s: &Session, cursor: Vec2) -> Vec<EntityKind> {
        match (self.detail, self.tip, self.boxing, self.box_a) {
            (Some(_), Some(_), true, Some(a)) => vec![EntityKind::LwPolyline(LwPolyline {
                vertices: box_vertices(Point2 { x: a.x, y: a.y }, Point2 { x: cursor.x, y: cursor.y }),
                closed: true,
                const_width: 0.0,
                elevation: 0.0,
                plinegen: false,
            })],
            (Some(_), Some(a), false, _) => vec![EntityKind::Line(Line { a: a.to3(0.0), b: cursor.to3(0.0) })],
            _ => Vec::new(),
        }
    }
}

pub(crate) fn detailtag_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(DetailTagM {
        default: None,
        detail: None,
        corner: false,
        plan: true,
        flip: false,
        tip: None,
        offered: Vec::new(),
        boxing: false,
        box_a: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_doc::Space;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    fn session() -> T<Session> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        Ok(s)
    }

    /// A plan with a closed 5000 × 4000 room of 200 mm walls on `WALL-EXT` (outer face 0..5000 ×
    /// 0..4000), a detail view `D1`, editing the plan on layer `TAGS`. Returns (session, plan, D1).
    fn room() -> T<(Session, Id, Id)> {
        let mut s = session()?;
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let detail = Id(s.execute("newdetail", &json!({ "name": "D1" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [5000, 0], [5000, 4000], [0, 4000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [4800, 200], [4800, 3800], [200, 3800]], "closed": true }))?;
        s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
        Ok((s, plan, detail))
    }

    fn tag(s: &mut Session, detail: Id, corner: bool, tip: (f64, f64)) -> Result<Value> {
        s.execute(
            "detailtag",
            &json!({ "detailViewId": detail.0, "corner": corner, "tip": {"x": tip.0, "y": tip.1}, "bubble": {"x": tip.0 - 1000.0, "y": tip.1 - 1000.0} }),
        )
    }

    fn kinds(xs: &[RunDetail]) -> (usize, usize, usize) {
        let n = |f: fn(&RunDetail) -> bool| xs.iter().filter(|x| f(x)).count();
        (n(|x| matches!(x, RunDetail::Tagged(_))), n(|x| matches!(x, RunDetail::Spread(_))), n(|x| matches!(x, RunDetail::Untyped)))
    }

    fn leaders(s: &Session, plan: Id) -> T<Vec<cadcraft_doc::Entity>> {
        let b = s.doc()?.block(&view_block_name(plan)).ok_or("no block")?.clone();
        Ok(b.entities.iter().filter(|e| matches!(e.kind, EntityKind::Leader(_))).map(|e| (**e).clone()).collect())
    }

    #[test]
    fn newdetail_makes_a_floating_detail_view_enters_it_and_lists_it() -> R {
        let mut s = session()?;
        s.start("newdetail")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter detail name or [Plan/Section] <Detail 1>:"));
        s.input(Input::Text("Eaves".into()))?;
        let doc = read_document(s.doc()?)?;
        let AnyView::Detail(d) = &doc.views[0] else { return Err("not a detail".into()) };
        assert_eq!((d.name.as_str(), d.placement.is_none()), ("Eaves", true));
        assert_eq!(s.state()?.space, Space::Block(view_block_name(d.id)));
        let rows = crate::views::browser_rows(s.doc()?, &s.state()?.space);
        assert!(rows.iter().any(|r| r.label == "Eaves" && r.active), "{rows:?}");
        Ok(())
    }

    #[test]
    fn an_edge_tag_spreads_to_the_other_three_runs_and_a_corner_tag_to_three_junctions() -> R {
        let (mut s, plan, detail) = room()?;
        let (runs, junctions) = resolve_view_details(s.doc()?, plan)?;
        assert_eq!((runs.len(), junctions.len()), (4, 4));
        assert_eq!(kinds(&runs), (0, 0, 4));

        // On the outer face of the bottom wall.
        tag(&mut s, detail, false, (2500.0, 0.0))?;
        let (runs, junctions) = resolve_view_details(s.doc()?, plan)?;
        assert_eq!(kinds(&runs), (1, 3, 0));
        assert!(runs.iter().all(|r| matches!(r, RunDetail::Tagged(d) | RunDetail::Spread(d) if *d == detail.0.to_string())));
        assert_eq!(kinds(&junctions), (0, 0, 4));

        // At the outer corner: 141 mm from the junction, inside its 150 mm reach.
        tag(&mut s, detail, true, (0.0, 0.0))?;
        let (_, junctions) = resolve_view_details(s.doc()?, plan)?;
        assert_eq!(kinds(&junctions), (1, 3, 0));

        let r = s.execute("detailtags", &json!({}))?;
        assert_eq!(r["message"], "<4 runs: 1 tagged, 3 spread; 4 junctions: 1 tagged, 3 spread>");
        assert!(s.log.iter().any(|l| l == "<4 runs: 1 tagged, 3 spread; 4 junctions: 1 tagged, 3 spread>"), "{:?}", s.log);
        Ok(())
    }

    #[test]
    fn a_tag_in_open_space_is_refused_and_one_naming_a_deleted_detail_is_unattached() -> R {
        let (mut s, plan, detail) = room()?;
        let n = leaders(&s, plan)?.len();
        let e = tag(&mut s, detail, false, (2500.0, 2000.0)).err().ok_or("open space accepted")?;
        assert_eq!(e.to_string(), "Not on a wall face: put the tag point on a wall line");
        let e = tag(&mut s, detail, true, (2500.0, -500.0)).err().ok_or("far corner accepted")?;
        assert_eq!(e.to_string(), "Not at a wall corner or wall end: put the tag point on the corner of two walls, or on a wall's end");
        assert_eq!(leaders(&s, plan)?.len(), n, "a refusal records nothing");

        tag(&mut s, detail, false, (2500.0, 100.0))?;
        assert_eq!(view_detail_tags(s.doc()?, plan)?.edges.len(), 1);
        let d = s.doc_mut()?;
        let mut doc = read_document(d)?;
        doc.views.retain(|v| v.id() != detail);
        crate::schema::write_document(d, &doc)?;
        let t = view_detail_tags(s.doc()?, plan)?;
        assert!(t.edges.is_empty());
        assert_eq!(t.unattached.len(), 1);
        let r = s.execute("detailtags", &json!({ "viewId": plan.0 }))?;
        assert_eq!(r["message"], "<4 runs: 4 untyped; 4 junctions: 4 untyped; 1 unattached>");
        Ok(())
    }

    /// [`room`]'s 5000 × 4000 outline with `width` mm walls on `layer`: outer face corner (0, 0),
    /// inner face corner (width, width), bottom outer face midpoint (2500, 0).
    fn walled(width: f64, layer: &str) -> T<(Session, Id, Id)> {
        let mut s = session()?;
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let detail = Id(s.execute("newdetail", &json!({ "name": "D1" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        if layer != "0" {
            s.execute("layer.new", &json!({ "name": layer, "current": true }))?;
        }
        let (a, b, c) = (width, 5000.0 - width, 4000.0 - width);
        s.execute("pline", &json!({ "vertices": [[0, 0], [5000, 0], [5000, 4000], [0, 4000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[a, a], [b, a], [b, c], [a, c]], "closed": true }))?;
        Ok((s, plan, detail))
    }

    /// LAMB T590's reproduction: a face midpoint takes an edge tag; a face-corner vertex, outer or
    /// inner, takes a corner tag and refuses an edge tag — for 150 and 300 mm walls alike.
    #[test]
    fn face_midpoints_and_face_corner_vertices_attach_or_refuse_as_their_kind_fits() -> R {
        for w in [150.0, 300.0] {
            let (mut s, plan, detail) = walled(w, "WALL-EXT")?;
            tag(&mut s, detail, false, (2500.0, 0.0))?;
            let e = tag(&mut s, detail, true, (2500.0, 0.0)).err().ok_or("corner tag at a face midpoint accepted")?;
            assert!(e.to_string().starts_with("Not at a wall corner"), "{e}");
            for v in [(0.0, 0.0), (w, w)] {
                let e = tag(&mut s, detail, false, v).err().ok_or(format!("edge tag at the corner vertex {v:?} accepted ({w} mm)"))?;
                assert_eq!(e.to_string(), "That point is the corner of two walls: pick a point along one face, or Corner for a corner detail");
                tag(&mut s, detail, true, v).map_err(|e| format!("corner tag at {v:?} ({w} mm): {e}"))?;
            }
            let t = view_detail_tags(s.doc()?, plan)?;
            assert_eq!((t.edges.len(), t.corners.len(), t.unattached.len()), (1, 2, 0), "{w} mm");
            assert_eq!(t.corners[0].junction, t.corners[1].junction, "both vertices are the same corner");
        }
        Ok(())
    }

    /// LAMB T681: walls on `A-WALL` or `0` are no walls until a tag sits on them, then they are.
    #[test]
    fn walls_on_any_layer_become_walls_where_a_tag_sits_on_them() -> R {
        for layer in ["A-WALL", "0"] {
            let (mut s, plan, detail) = walled(200.0, layer)?;
            let r = s.execute("detailtags", &json!({ "viewId": plan.0 }))?;
            assert!(r["message"].as_str().is_some_and(|m| m.starts_with("<0 runs")), "{layer}: {r}");
            tag(&mut s, detail, true, (0.0, 0.0)).map_err(|e| format!("corner tag on {layer}: {e}"))?;
            tag(&mut s, detail, false, (2500.0, 0.0)).map_err(|e| format!("edge tag on {layer}: {e}"))?;
            let r = s.execute("detailtags", &json!({ "viewId": plan.0 }))?;
            assert_eq!(r["message"], "<4 runs: 1 tagged, 3 spread; 4 junctions: 1 tagged, 3 spread>", "{layer}");
        }
        Ok(())
    }

    /// LAMB T681: a tip on a lone line says which layer has no partner face, and records nothing.
    #[test]
    fn a_tag_on_a_lone_line_is_refused_naming_its_layer() -> R {
        let mut s = session()?;
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let detail = Id(s.execute("newdetail", &json!({ "name": "D1" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "A-WALL", "current": true }))?;
        s.execute("line", &json!({ "points": [[0, 0], [5000, 0]] }))?;
        let n = s.doc()?.block(&view_block_name(plan)).ok_or("no block")?.entities.len();
        let e = tag(&mut s, detail, false, (2500.0, 0.0)).err().ok_or("lone line accepted")?;
        assert_eq!(e.to_string(), "No wall here: A-WALL has no parallel face 50–800 mm from this line");
        assert_eq!(s.doc()?.block(&view_block_name(plan)).ok_or("no block")?.entities.len(), n, "a refusal records nothing");
        Ok(())
    }

    #[test]
    fn a_placed_tag_names_its_target_and_the_first_one_makes_the_assembly_plan_in_its_own_undo_step() -> R {
        let (mut s, plan, detail) = room()?;
        let name = crate::schema::assembly_block_name(plan);
        assert!(s.doc()?.block(&name).is_none());
        let n = s.state()?.undo.len();
        let r = tag(&mut s, detail, false, (2500.0, 0.0))?;
        assert_eq!(s.state()?.undo.len(), n + 1, "one undo step");
        assert!(s.doc()?.block(&name).is_some(), "the first tag makes the assembly block");
        let run = r["attached"]["run"].as_u64().ok_or("no run")?;
        assert!(s.log.iter().any(|l| l == &format!("Tagged run {run} of \"Ground\" with \"D1\"")), "{:?}", s.log);
        assert!(s.log.iter().any(|l| l == "<4 runs: 1 tagged, 3 spread; 4 junctions: 4 untyped>"), "{:?}", s.log);

        let r = tag(&mut s, detail, true, (0.0, 0.0))?;
        let j = r["attached"]["junction"].as_u64().ok_or("no junction")?;
        assert!(s.log.iter().any(|l| l == &format!("Tagged junction {j} of \"Ground\" with \"D1\"")), "{:?}", s.log);
        s.undo()?;
        assert!(s.doc()?.block(&name).is_some(), "a later tag leaves the block alone");
        s.undo()?;
        assert!(leaders(&s, plan)?.is_empty() && s.doc()?.block(&name).is_none(), "one undo removes the first tag and the block together");
        Ok(())
    }

    #[test]
    fn the_interactive_form_refuses_a_bad_tag_point_and_asks_again() -> R {
        let (mut s, plan, _detail) = room()?;
        s.start("dtag")?;
        s.input(Input::Enter)?;
        s.input(Input::Point(Vec2::new(2500.0, 2000.0)))?;
        assert!(s.log.iter().any(|l| l == "Not on a wall face: put the tag point on a wall line"), "{:?}", s.log);
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Specify tag point on a wall face"));
        s.input(Input::Point(Vec2::new(2500.0, 0.0)))?;
        s.input(Input::Point(Vec2::new(1500.0, -1000.0)))?;
        assert!(s.running.is_none());
        assert_eq!(view_detail_tags(s.doc()?, plan)?.edges.len(), 1);
        Ok(())
    }

    #[test]
    fn tags_survive_undo_redo_copy_and_a_dxf_round_trip() -> R {
        let (mut s, plan, detail) = room()?;
        tag(&mut s, detail, false, (2500.0, 0.0))?;
        assert_eq!(view_detail_tags(s.doc()?, plan)?.edges.len(), 1);
        s.undo()?;
        assert!(leaders(&s, plan)?.is_empty(), "one undo step removes the tag");
        s.redo()?;
        assert_eq!(view_detail_tags(s.doc()?, plan)?.edges.len(), 1);

        let h = leaders(&s, plan)?[0].handle;
        s.execute("copy", &json!({ "handles": [format!("{:X}", h.0)], "delta": [0, 4000] }))?;
        let t = view_detail_tags(s.doc()?, plan)?;
        assert_eq!(t.edges.len(), 2, "the copy is a second tag");
        assert_ne!(t.edges[0].run, t.edges[1].run);

        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert_eq!(view_detail_tags(&back, plan)?, view_detail_tags(s.doc()?, plan)?);
        assert_eq!(resolve_view_details(&back, plan)?, resolve_view_details(s.doc()?, plan)?);
        Ok(())
    }

    #[test]
    fn detailtag_outside_a_plan_view_refuses_and_adds_nothing() -> R {
        let (mut s, _plan, detail) = room()?;
        s.execute("editview", &json!({}))?;
        let n = s.doc()?.model.len();
        assert!(tag(&mut s, detail, false, (2500.0, 0.0)).is_err_and(|e| e.to_string().contains("Edit a plan or section view first")));
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        assert!(tag(&mut s, detail, false, (2500.0, 0.0)).is_err_and(|e| e.to_string().contains("Detail tags live in plan and section views")));
        assert_eq!(s.doc()?.model.len(), n);
        assert!(s.doc()?.block(&view_block_name(detail)).ok_or("no block")?.entities.is_empty());

        s.execute("editview", &json!({}))?;
        s.start("dtag")?;
        assert!(s.running.is_none(), "refused at once");
        assert!(s.log.iter().any(|l| l == "Edit a plan or section view first (EDITVIEW)"), "{:?}", s.log);
        Ok(())
    }

    #[test]
    fn the_interactive_form_places_the_same_tag_as_the_parameter_form() -> R {
        let (mut s, plan, detail) = room()?;
        s.start("dtag")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Select detail [D1/Corner] <D1>:"));
        s.input(Input::Keyword("Corner".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Select anchored detail"));
        s.input(Input::Text("d1".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Specify tag point at a wall corner, opening or wall end"));
        s.input(Input::Point(Vec2::new(0.0, 0.0)))?;
        assert_eq!(s.current_prompt().and_then(|p| p.base), Some(Vec2::new(0.0, 0.0)));
        s.input(Input::Point(Vec2::new(-1000.0, -1000.0)))?;
        assert!(s.running.is_none());
        tag(&mut s, detail, true, (0.0, 0.0))?;
        let l = leaders(&s, plan)?;
        assert_eq!(l.len(), 2);
        assert_eq!((&l[0].kind, &l[0].common.layer, &l[0].common.ext), (&l[1].kind, &l[1].common.layer, &l[1].common.ext));
        assert_eq!(view_detail_tags(s.doc()?, plan)?.corners.len(), 2);

        // The default is now the last detail used; one undo removes the interactive tag alone.
        s.start("detailtag")?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("D1"));
        s.input(Input::Cancel)?;
        s.undo()?;
        s.undo()?;
        assert!(leaders(&s, plan)?.is_empty());
        Ok(())
    }

    /// A plan with section `A-A` across it, edited, holding two vertical `WALL-EXT` faces 300 apart
    /// (u 0 and 300, z 0..3000) and two horizontal `FLOOR` faces 250 apart (z 1000 and 1250, u
    /// 300..4000) meeting the wall, all SETROLE cut; layer `TAGS` current. Returns (session, plan,
    /// section, plan detail, section detail).
    fn section_room() -> T<(Session, Id, Id, Id, Id)> {
        let mut s = session()?;
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let pd = Id(s.execute("newdetail", &json!({ "name": "PD" }))?["viewId"].as_u64().ok_or("no id")?);
        let sd = Id(s.execute("newdetail", &json!({ "name": "SD", "cutDirection": "vertical" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let r =
            s.execute("newsectionline", &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 0}, "cutTo": {"x": 5000, "y": 0} }))?;
        let sec = Id(r["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [0, 3000]] }))?;
        s.execute("pline", &json!({ "vertices": [[300, 0], [300, 3000]] }))?;
        s.execute("layer.new", &json!({ "name": "FLOOR", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[300, 1000], [4000, 1000]] }))?;
        s.execute("pline", &json!({ "vertices": [[300, 1250], [4000, 1250]] }))?;
        let block = s.doc()?.block(&view_block_name(sec)).ok_or("no block")?;
        let hs: Vec<String> = block.entities.iter().map(|e| format!("{:X}", e.handle.0)).collect();
        assert_eq!(hs.len(), 4);
        s.execute("setrole", &json!({ "handles": hs, "role": "cut" }))?;
        s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
        Ok((s, plan, sec, pd, sd))
    }

    #[test]
    fn a_section_s_cut_walls_and_floors_form_runs_and_a_section_detail_tags_them() -> R {
        let (mut s, _plan, sec, pd, sd) = section_room()?;
        let r = s.execute("detailtags", &json!({ "viewId": sec.0 }))?;
        assert_eq!(r["runs"].as_array().map(Vec::len), Some(2), "{r}");
        assert_eq!(kinds(&resolve_view_details(s.doc()?, sec)?.0), (0, 0, 2));

        // A plan detail is refused in the section and records nothing.
        let n = s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.len();
        assert!(tag(&mut s, pd, false, (150.0, 2000.0)).is_err_and(|e| e.to_string() == "That detail is drawn in plan; tag it in a plan view"));
        assert_eq!(s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.len(), n);

        // The section detail's edge tag on the wall run: that run is tagged.
        tag(&mut s, sd, false, (150.0, 2000.0))?;
        let t = view_detail_tags(s.doc()?, sec)?;
        assert_eq!(t.edges.len(), 1);
        let (runs, _) = resolve_view_details(s.doc()?, sec)?;
        assert_eq!(runs[t.edges[0].run], RunDetail::Tagged(sd.0.to_string()));
        let r = s.execute("detailtags", &json!({}))?;
        assert_eq!(r["message"], FLOOR_REPORT, "{r}");

        // A corner tag at the wall's free top end still refuses in a section (no termination there).
        let n = s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.len();
        let e = tag(&mut s, sd, true, (150.0, 3000.0)).err().ok_or("section free end accepted")?;
        assert_eq!(e.to_string(), "Not at a wall corner: put the tag point on the corner of two walls");
        assert_eq!(s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.len(), n);

        // OUTSIDEPOINT is accepted in the edited section.
        s.execute("outsidepoint", &json!({ "point": {"x": 2000, "y": 2500} }))?;
        assert_eq!(crate::outside::outside_points(s.doc()?, sec).len(), 1);
        Ok(())
    }

    /// lamb-core's greedy rule, unchanged: the floor abutting the wall's inner face forms no junction
    /// with it, so the wall tag does not spread to it.
    const FLOOR_REPORT: &str = "<2 runs: 1 tagged, 1 untyped; 0 junctions>";

    #[test]
    fn a_section_detail_is_refused_in_a_plan() -> R {
        let (mut s, plan, _sec, _pd, sd) = section_room()?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        assert!(tag(&mut s, sd, false, (0.0, 0.0)).is_err_and(|e| e.to_string() == "That detail is drawn in section; tag it in a section view"));
        Ok(())
    }

    #[test]
    fn newdetail_section_keyword_makes_a_vertical_detail() -> R {
        let mut s = session()?;
        s.start("newdetail")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter detail name or [Plan/Section] <Detail 1>:"));
        s.input(Input::Keyword("S".into()))?;
        assert!(s.log.iter().any(|l| l == "<Section detail>"), "{:?}", s.log);
        assert!(s.running.is_some(), "asks the name again");
        s.input(Input::Text("Sill".into()))?;
        let doc = read_document(s.doc()?)?;
        let AnyView::Detail(d) = &doc.views[0] else { return Err("not a detail".into()) };
        assert_eq!((d.name.as_str(), d.cut_direction), ("Sill", CutDirection::Vertical));
        assert_eq!(s.state()?.space, Space::Block(view_block_name(d.id)));
        assert!(
            s.execute("newdetail", &json!({ "name": "X", "cutDirection": "up" }))
                .is_err_and(|e| e.to_string().contains("`cutDirection` must be plan or vertical"))
        );
        s.start("newplan")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("New plan or [Height/Floating] <Height>:"));
        Ok(())
    }

    #[test]
    fn dtag_offers_this_drawing_s_details_drawn_the_edited_view_s_way_as_keywords() -> R {
        let (mut s, plan, _d1) = room()?;
        let cd = Id(s.execute("newdetail", &json!({ "name": "Corner Detail" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("newdetail", &json!({ "name": "S1", "cutDirection": "vertical" }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.start("dtag")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Select detail [D1/Corner_Detail/Corner]:"));
        s.input(Input::Keyword("Corner_Detail".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Specify tag point on a wall face"));
        s.input(Input::Point(Vec2::new(2500.0, 0.0)))?;
        s.input(Input::Point(Vec2::new(1500.0, -1000.0)))?;
        assert!(s.running.is_none());
        let t = view_detail_tags(s.doc()?, plan)?;
        assert_eq!(t.edges.len(), 1);
        assert_eq!(t.edges[0].detail, cd.0.to_string());
        assert!(s.log.iter().any(|l| l.starts_with("Tagged run ") && l.ends_with("with \"Corner Detail\"")), "{:?}", s.log);

        // The section detail is not offered in a plan, and typing it says why.
        s.start("dtag")?;
        s.input(Input::Text("s1".into()))?;
        assert_eq!(s.log.last().map(String::as_str), Some("That detail is drawn in section; tag it in a section view"));
        s.input(Input::Cancel)?;

        // In a section only S1 is offered, and as the only one it is the default.
        let r = s.execute(
            "newsectionline",
            &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": -500}, "cutTo": {"x": 5000, "y": -500} }),
        )?;
        s.execute("editview", &json!({ "viewId": r["viewId"].as_u64().ok_or("no id")? }))?;
        s.start("dtag")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Select detail [S1/Corner] <S1>:"));
        Ok(())
    }

    fn box_tag(s: &mut Session, detail: Id, corner: bool, tip: (f64, f64), a: (f64, f64), b: (f64, f64)) -> Result<Value> {
        s.execute(
            "detailtag",
            &json!({ "detailViewId": detail.0, "corner": corner, "tip": {"x": tip.0, "y": tip.1},
                     "box": {"a": {"x": a.0, "y": a.1}, "b": {"x": b.0, "y": b.1}} }),
        )
    }

    /// The view block's callout-box tags.
    fn boxes(s: &Session, plan: Id) -> T<Vec<cadcraft_doc::Entity>> {
        let b = s.doc()?.block(&view_block_name(plan)).ok_or("no block")?.clone();
        Ok(b.entities
            .iter()
            .filter(|e| matches!(e.kind, EntityKind::LwPolyline(_)) && read_entity_data(&e.common).is_ok_and(|d| d.detail_tag.is_some()))
            .map(|e| (**e).clone())
            .collect())
    }

    #[test]
    fn a_box_tag_resolves_as_a_leader_tag_at_the_same_anchor_and_is_a_closed_dashed_rectangle() -> R {
        let (mut s, plan, detail) = room()?;
        let r = box_tag(&mut s, detail, false, (2500.0, 0.0), (2000.0, -300.0), (3000.0, 300.0))?;
        box_tag(&mut s, detail, true, (0.0, 0.0), (300.0, 300.0), (-300.0, -300.0))?;
        let (mut l, lplan, ldetail) = room()?;
        tag(&mut l, ldetail, false, (2500.0, 0.0))?;
        tag(&mut l, ldetail, true, (0.0, 0.0))?;
        assert_eq!((plan, detail), (lplan, ldetail));
        let (rb, rl) = (s.execute("detailtags", &json!({}))?, l.execute("detailtags", &json!({}))?);
        assert_eq!(rb, rl);
        assert_eq!(rb["message"], "<4 runs: 1 tagged, 3 spread; 4 junctions: 1 tagged, 3 spread>");

        assert!(leaders(&s, plan)?.is_empty(), "a box tag makes no leader");
        let b = boxes(&s, plan)?;
        assert_eq!(b.len(), 2);
        assert_eq!(r["handle"], format!("{:X}", b[0].handle.0));
        let EntityKind::LwPolyline(pl) = &b[0].kind else { return Err("not a polyline".into()) };
        let corners: Vec<(f64, f64)> = pl.vertices.iter().map(|v| (v.p.x, v.p.y)).collect();
        assert_eq!((pl.closed, corners), (true, vec![(2000.0, -300.0), (3000.0, -300.0), (3000.0, 300.0), (2000.0, 300.0)]));
        assert_eq!((b[0].common.linetype.as_str(), b[0].common.layer.as_str()), ("DASHED", "TAGS"));
        assert!(s.doc()?.linetype("DASHED").is_some(), "DASHED is loaded");
        let tag = read_entity_data(&b[0].common)?.detail_tag.ok_or("no tag")?;
        assert_eq!(tag, DetailTag { detail_view_id: detail, corner: false, anchor: Some([2500.0, 0.0]), flip: false });
        Ok(())
    }

    #[test]
    fn a_box_that_misses_the_tag_point_or_has_no_area_is_refused_and_records_nothing() -> R {
        let (mut s, plan, detail) = room()?;
        let n = (s.doc()?.block(&view_block_name(plan)).ok_or("no block")?.entities.len(), s.state()?.undo.len());
        let e = box_tag(&mut s, detail, false, (2500.0, 0.0), (3000.0, -300.0), (4000.0, 300.0)).err().ok_or("box missing the point accepted")?;
        assert_eq!(e.to_string(), "The box must enclose the tag point");
        let e = box_tag(&mut s, detail, false, (2500.0, 0.0), (2000.0, 0.0), (3000.0, 0.0)).err().ok_or("flat box accepted")?;
        assert_eq!(e.to_string(), "The box needs two opposite corners apart in both directions");
        let e = box_tag(&mut s, detail, false, (2500.0, 2000.0), (2000.0, 1500.0), (3000.0, 2500.0)).err().ok_or("open space accepted")?;
        assert_eq!(e.to_string(), "Not on a wall face: put the tag point on a wall line");
        let both = json!({ "detailViewId": detail.0, "tip": {"x": 2500, "y": 0}, "bubble": {"x": 0, "y": -1000},
                           "box": {"a": {"x": 2000, "y": -300}, "b": {"x": 3000, "y": 300}} });
        assert!(s.execute("detailtag", &both).is_err_and(|e| e.to_string().contains("give `bubble` or `box`, not both")));
        let neither = json!({ "detailViewId": detail.0, "tip": {"x": 2500, "y": 0} });
        assert!(s.execute("detailtag", &neither).is_err_and(|e| e.to_string().contains("`bubble` or `box` is required")));
        assert_eq!((s.doc()?.block(&view_block_name(plan)).ok_or("no block")?.entities.len(), s.state()?.undo.len()), n);
        assert!(s.doc()?.linetype("DASHED").is_none(), "a refusal loads nothing");
        Ok(())
    }

    #[test]
    fn the_interactive_box_makes_the_same_entity_as_the_parameter_form() -> R {
        let (mut s, plan, detail) = room()?;
        s.start("dtag")?;
        s.input(Input::Enter)?;
        s.input(Input::Point(Vec2::new(2500.0, 0.0)))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Specify bubble point or [Box]:"));
        s.input(Input::Keyword("Box".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Specify first corner of the callout box"));
        // A box that misses the tag point is refused and its first corner asked again.
        s.input(Input::Point(Vec2::new(3000.0, 300.0)))?;
        assert_eq!(s.current_prompt().map(|p| (p.message, p.base)), Some(("Specify opposite corner".to_owned(), Some(Vec2::new(3000.0, 300.0)))));
        s.input(Input::Point(Vec2::new(2800.0, -300.0)))?;
        assert_eq!(s.log.last().map(String::as_str), Some("The box must enclose the tag point"));
        assert!(boxes(&s, plan)?.is_empty());
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Specify first corner of the callout box"));
        s.input(Input::Point(Vec2::new(3000.0, 300.0)))?;
        s.input(Input::Point(Vec2::new(2000.0, -300.0)))?;
        assert!(s.running.is_none());

        box_tag(&mut s, detail, false, (2500.0, 0.0), (3000.0, 300.0), (2000.0, -300.0))?;
        let b = boxes(&s, plan)?;
        assert_eq!(b.len(), 2);
        assert_eq!(
            (&b[0].kind, &b[0].common.layer, &b[0].common.linetype, &b[0].common.ext),
            (&b[1].kind, &b[1].common.layer, &b[1].common.linetype, &b[1].common.ext)
        );

        // `Box` was for that tag only: the next one asks for a bubble point again.
        s.start("dtag")?;
        s.input(Input::Enter)?;
        s.input(Input::Point(Vec2::new(2500.0, 0.0)))?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Specify bubble point"));
        Ok(())
    }

    #[test]
    fn box_tags_survive_undo_redo_and_a_dxf_round_trip() -> R {
        let (mut s, plan, detail) = room()?;
        box_tag(&mut s, detail, false, (2500.0, 0.0), (2000.0, -300.0), (3000.0, 300.0))?;
        s.undo()?;
        assert!(boxes(&s, plan)?.is_empty(), "one undo step removes the box");
        s.redo()?;
        assert_eq!(view_detail_tags(s.doc()?, plan)?.edges.len(), 1);
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert_eq!(view_detail_tags(&back, plan)?, view_detail_tags(s.doc()?, plan)?);
        let block = back.block(&view_block_name(plan)).ok_or("no block")?;
        let e = block.entities.iter().find(|e| read_entity_data(&e.common).is_ok_and(|d| d.detail_tag.is_some())).ok_or("box lost")?;
        assert_eq!(e.common.linetype.to_ascii_uppercase(), "DASHED");
        assert!(matches!(&e.kind, EntityKind::LwPolyline(pl) if pl.closed && pl.vertices.len() == 4));
        Ok(())
    }

    #[test]
    fn dtag_with_no_details_drawn_the_edited_view_s_way_ends_and_records_nothing() -> R {
        let mut s = session()?;
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("newdetail", &json!({ "name": "S1", "cutDirection": "vertical" }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let before = cadcraft_io::write_dxf(s.doc()?);
        s.start("dtag")?;
        assert!(s.running.is_none(), "ends at once");
        assert!(
            s.log.iter().any(|l| l == "No plan details in this drawing — make one with NEWDETAIL, draw it, then tag a wall with it"),
            "{:?}",
            s.log
        );
        assert_eq!(cadcraft_io::write_dxf(s.doc()?), before);
        Ok(())
    }

    /// LAMB T674/T683: the detail's reference decides its kind — a linear detail refuses a wall
    /// corner, an anchored (corner reference) detail a wall face — and picking an anchored detail
    /// asks for a wall corner, opening or end by itself.
    #[test]
    fn a_detail_s_reference_decides_corner_or_face() -> R {
        let (mut s, plan) = crate::assembly::tests::room("exterior_face", true)?;
        let d1 = read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id();
        assert_eq!(crate::detailref::detail_is_anchored(s.doc()?, d1), Some(false));
        let undo = s.state()?.undo.len();
        let e = tag(&mut s, d1, true, (0.0, 0.0)).err().ok_or("linear detail tagged a corner")?;
        assert_eq!(e.to_string(), LINEAR_D1);
        assert_eq!(s.state()?.undo.len(), undo, "nothing recorded");

        let cd = crate::assembly::tests::corner_detail(&mut s, plan, "exterior_face", false, (5000.0, 0.0))?;
        assert_eq!(crate::detailref::detail_is_anchored(s.doc()?, cd), Some(true));
        for (p, want) in [
            (json!({}), "Not at a wall corner"),
            (json!({ "corner": false }), "CD is an anchored detail: tag a wall corner, opening or wall end with it"),
        ] {
            let mut p = p;
            p["detailViewId"] = json!(cd.0);
            p["tip"] = json!({"x": 2500.0, "y": 0.0});
            p["bubble"] = json!({"x": 2500.0, "y": -1000.0});
            let e = s.execute("detailtag", &p).err().ok_or("corner detail tagged a face")?;
            assert!(e.to_string().starts_with(want), "{e}");
        }
        // Omitting `corner` takes the detail's kind.
        s.execute("detailtag", &json!({ "detailViewId": cd.0, "tip": {"x": 0.0, "y": 4000.0}, "bubble": {"x": -1000.0, "y": 5000.0} }))?;

        s.start("detailtag")?;
        s.input(Input::Keyword("CD".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Specify tag point at a wall corner, opening or wall end"));
        Ok(())
    }

    const LINEAR_D1: &str = "D1 is a linear detail: tag a wall face with it (a corner, jamb or end detail needs a corner reference — DETAILREF)";

    /// LAMB T683: a linear (single-line) detail is refused at an opening jamb, at a wall's free end
    /// and at a section's head, recording nothing; it still tags a face. (The corner-reference `JD`,
    /// `H` and `S` lay at jambs, ends, heads and sills in `assembly.rs`'s tests.)
    #[test]
    fn a_linear_detail_refuses_a_jamb_a_free_end_and_a_head() -> R {
        let (mut s, _plan) = crate::assembly::tests::window_room(false, false)?;
        let d1 = d1_of(&s)?;
        let undo = s.state()?.undo.len();
        let e = tag(&mut s, d1, true, (2500.0, 100.0)).err().ok_or("linear detail tagged a jamb")?;
        assert_eq!(e.to_string(), LINEAR_D1);
        assert_eq!(s.state()?.undo.len(), undo, "nothing recorded");
        s.execute("detailtag", &json!({ "detailViewId": d1.0, "tip": {"x": 4000.0, "y": 4000.0}, "bubble": {"x": 4000.0, "y": 5000.0} }))?;

        let (mut s, _plan, _jd) = crate::assembly::tests::gap_room()?;
        let undo = s.state()?.undo.len();
        let d1 = d1_of(&s)?;
        let e = tag(&mut s, d1, true, (2000.0, 100.0)).err().ok_or("linear detail tagged a free end")?;
        assert_eq!(e.to_string(), LINEAR_D1);
        assert_eq!(s.state()?.undo.len(), undo, "nothing recorded");

        let (mut s, _sec, _) = crate::assembly::tests::window_section(false)?;
        let sd = read_document(s.doc()?)?.views.iter().find(|v| v.name() == "SD").ok_or("no SD")?.id();
        let undo = s.state()?.undo.len();
        let e = s
            .execute("detailtag", &json!({ "detailViewId": sd.0, "corner": true, "tip": {"x": 150, "y": 2050}, "bubble": {"x": 1500, "y": 2050} }))
            .err()
            .ok_or("linear detail tagged a head")?;
        assert!(e.to_string().starts_with("SD is a linear detail: tag a wall face with it"), "{e}");
        assert_eq!(s.state()?.undo.len(), undo, "nothing recorded");
        Ok(())
    }

    fn d1_of(s: &Session) -> T<Id> {
        Ok(read_document(s.doc()?)?.views.iter().find(|v| v.name() == "D1").ok_or("no D1")?.id())
    }

    /// LAMB T683: `Corner` lists only anchored and reference-less details, and picking a linear
    /// detail (typed by name) after `Corner` asks the face prompt.
    #[test]
    fn corner_mode_lists_anchored_details_and_a_linear_pick_asks_for_a_face() -> R {
        let (mut s, _plan) = crate::assembly::tests::window_room(false, false)?;
        s.start("dtag")?;
        assert_eq!(s.current_prompt().map(|p| p.keywords.clone()), Some(vec!["D1".to_owned(), "JD".to_owned(), "Corner".to_owned()]));
        s.input(Input::Keyword("Corner".into()))?;
        let p = s.current_prompt().ok_or("no prompt")?;
        assert_eq!((p.message.as_str(), p.keywords.clone()), ("Select anchored detail", vec!["JD".to_owned(), "Corner".to_owned()]));
        s.input(Input::Text("D1".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Specify tag point on a wall face"));
        s.input(Input::Cancel)?;

        s.start("dtag")?;
        s.input(Input::Text("JD".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Specify tag point at a wall corner, opening or wall end"));
        Ok(())
    }
}

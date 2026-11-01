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

use cadcraft_color::{Color, Rgb};
use cadcraft_doc::{Drawing, EntityKind, Handle, Point, Space};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::{Bounds2, Vec2};
use cadcraft_render::{DisplayList, Kind, Options};
use lamb_core::derivation::wall_spaces::{SideKind, SideParams, WallSides, wall_sides};
use lamb_core::geometry::Point2;
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::commands::{other, point_param};
use crate::detailtags::{edited_wall_view, exterior_layer_ids, graph_and_sides};
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
    let (graph, without) = graph_and_sides(&p, view);
    let ext = exterior_layer_ids(&p);
    let sides = |pts: Vec<Point2>| wall_sides(&p, &graph, &SideParams { outside_points: pts, exterior_layer_ids: ext.clone() });
    let mut pts = outside_points(d, view);
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
}

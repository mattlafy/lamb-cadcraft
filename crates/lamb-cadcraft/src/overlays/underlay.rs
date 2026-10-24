//! The underlay (LAMB T350, TS `ViewsPanel.tsx` checkbox / `underlayByView`): another placed view
//! of the same kind as the edited view, drawn pale under it in the edited view's local
//! coordinates, with its endpoints, midpoints, centres, intersections and nodes snappable there.
//! Which views qualify, and the point map between the two local spaces, are lamb-core's
//! [`find_underlay_candidates`] and [`map_view_local_point`] — an affine map for both supported
//! pairings (plan↔plan, parallel vertical↔vertical), so mapping display-list vertices is exact.
//!
//! Session UI state, never saved (ruling T350.1): each edited view's underlay ([`CHOSEN`], block
//! name → block name). A choice that is no longer a candidate reads as off. UNDERLAY changes no
//! drawing revision, so it bumps [`cadcraft_render::bump_overlays`] for the canvas's overlay cache.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Mutex;

use cadcraft_color::Rgb;
use cadcraft_doc::{Drawing, Space};
use cadcraft_engine::snap::{SnapHit, mode, osnap};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::{Bounds2, Vec2};
use cadcraft_render::{DPrim, DisplayList, Kind, Options};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::derivation::underlay::{UnderlayCandidate, find_underlay_candidates, map_view_local_point};
use lamb_core::geometry::Point2;
use lamb_core::model::Id;
use serde_json::{Value, json};

use super::{view_id, view_of};
use crate::commands::{edited_view, id_value, other};
use crate::project::DrawingProject;
use crate::schema::view_block_name;

/// TS `rgba(210,210,210,0.35)`.
const UNDERLAY_COLOR: Rgb = Rgb(210, 210, 210);
const UNDERLAY_TRANSPARENCY: u8 = 65;
/// What TS `collectSnapPoints` offers from the underlay.
const UNDERLAY_SNAPS: u32 = mode::END | mode::MID | mode::CEN | mode::INT | mode::NOD;

/// Edited view's block name -> its underlay's block name.
static CHOSEN: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

thread_local! {
    /// Set while [`snaps`] runs CADCraft's `osnap` in the underlay's space: that call asks the snap
    /// providers again, and LAMB's answers nothing then, so A↔B underlays never recurse.
    static NESTED: Cell<bool> = const { Cell::new(false) };
}

fn chosen() -> std::sync::MutexGuard<'static, BTreeMap<String, String>> {
    CHOSEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Inside [`snaps`]' own `osnap` call: the LAMB snap provider offers nothing.
pub(crate) fn nested() -> bool {
    NESTED.with(Cell::get)
}

/// `edit`'s underlay candidates (empty when it has none or the slots can't be read).
pub(crate) fn candidates(d: &Drawing, edit: Id) -> Vec<UnderlayCandidate> {
    DrawingProject::new(d).ok().and_then(|dp| find_underlay_candidates(&dp, edit).ok()).unwrap_or_default()
}

/// `edit`'s underlay: the remembered choice while it is still a candidate.
pub(crate) fn current(d: &Drawing, edit: Id) -> Option<Id> {
    let u = chosen().get(&view_block_name(edit)).and_then(|n| view_id(&Space::Block(n.clone())))?;
    candidates(d, edit).iter().any(|c| c.view_id == u).then_some(u)
}

fn view_name(d: &Drawing, id: Id) -> String {
    DrawingProject::new(d).ok().and_then(|dp| dp.view(id).map(|v| v.name.to_owned())).unwrap_or_else(|| id.0.to_string())
}

/// Map `p` from view `from`'s local space into `to`'s.
fn map(dp: &DrawingProject<'_>, from: Id, to: Id, p: Vec2) -> Option<Vec2> {
    let (f, t) = (dp.view(from)?, dp.view(to)?);
    let q = map_view_local_point(dp, &f, &t, Point2 { x: p.x, y: p.y }).ok()??;
    let q = Vec2::new(q.x, q.y);
    q.is_finite().then_some(q)
}

fn set(d: &Drawing, edit: Id, to: Option<Id>) -> Value {
    match to {
        Some(u) => {
            chosen().insert(view_block_name(edit), view_block_name(u));
        }
        None => {
            chosen().remove(&view_block_name(edit));
        }
    }
    cadcraft_render::bump_overlays();
    match to {
        Some(u) => json!({ "viewId": u.0, "message": format!("<Underlay: {}>", view_name(d, u)) }),
        None => json!({ "viewId": null, "message": "<Underlay off>" }),
    }
}

/// UNDERLAY `{viewId?}`: an underlay candidate of the edited view becomes its underlay; the current
/// one again, or no `viewId`, turns it off. Returns `{viewId, message}`; not an undo step.
pub(crate) fn underlay(s: &mut Session, p: &Value) -> Result<Value> {
    let edit = edited_view(s).ok_or_else(|| other("Edit a plan or section first (EDITVIEW)."))?;
    let d = s.doc()?;
    let Some(v) = p.get("viewId").filter(|v| !v.is_null()) else { return Ok(set(d, edit, None)) };
    let id = id_value(v).ok_or_else(|| other("No such view"))?;
    if !candidates(d, edit).iter().any(|c| c.view_id == id) {
        return Err(other(format!("Not an underlay view for {}", view_name(d, edit))));
    }
    let to = (current(d, edit) != Some(id)).then_some(id);
    Ok(set(d, edit, to))
}

/// UNDERLAY: `Enter underlay view name or [Off/?]:` — a candidate's name or id sets it, `Off`
/// clears it, `?` lists the candidates, Enter leaves it as it is.
struct UnderlayMachine;

impl Interactive for UnderlayMachine {
    fn name(&self) -> &'static str {
        "underlay"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter underlay view name", Accept::TEXT).kw(&["Off", "?"])
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let text = match i {
            Input::Cancel | Input::Enter => return Ok(Step::Done),
            Input::Text(t) | Input::Keyword(t) => t.trim().to_owned(),
            _ => return Ok(Step::Continue),
        };
        if text.is_empty() {
            return Ok(Step::Done);
        }
        let edit = edited_view(s).ok_or_else(|| other("Edit a plan or section first (EDITVIEW)."))?;
        let d = s.doc()?;
        if text.eq_ignore_ascii_case("off") {
            let r = set(d, edit, None);
            s.echo(r["message"].as_str().unwrap_or_default().to_owned());
            return Ok(Step::Done);
        }
        let dp = DrawingProject::new(d).map_err(other)?;
        let cands: Vec<(Id, String, bool)> =
            candidates(d, edit).iter().filter_map(|c| dp.view(c.view_id).map(|v| (c.view_id, v.name.to_owned(), c.mirrored))).collect();
        if text == "?" {
            let lines: Vec<String> = cands.iter().map(|(id, name, m)| format!("{}  {name}{}", id.0, if *m { " (mirrored)" } else { "" })).collect();
            for l in lines {
                s.echo(l);
            }
            return Ok(Step::Continue);
        }
        let hit = cands.iter().find(|(_, n, _)| n.eq_ignore_ascii_case(&text)).or_else(|| {
            let n = text.parse::<u64>().ok()?;
            cands.iter().find(|(id, _, _)| id.0 == n)
        });
        match hit {
            Some((id, _, _)) => {
                let r = set(s.doc()?, edit, Some(*id));
                s.echo(r["message"].as_str().unwrap_or_default().to_owned());
                Ok(Step::Done)
            }
            None => Err(other(format!("Not an underlay view for {}", view_name(s.doc()?, edit)))),
        }
    }
}

pub(crate) fn underlay_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(UnderlayMachine))
}

/// The edited view and its underlay, for `space`.
fn pair<'a>(d: &'a Drawing, space: &Space) -> Option<(DrawingProject<'a>, Id, Id)> {
    let (dp, edit) = view_of(d, space)?;
    let u = current(d, edit)?;
    Some((dp, edit, u))
}

/// The underlay's linework for `space`, mapped into the edited view's local coordinates, pale.
/// Infinite lines and wipeout masks are left out. Empty without an underlay.
pub(super) fn draw(d: &Drawing, space: &Space, opts: &Options) -> DisplayList {
    let mut out = DisplayList { bounds: Bounds2::EMPTY, ..DisplayList::default() };
    let Some((dp, edit, u)) = pair(d, space) else { return out };
    let Some(block) = d.block(&view_block_name(u)) else { return out };
    let opts = Options { view: None, ..opts.clone() };
    let src = cadcraft_render::build_entities(d, block.entities.iter().map(|e| &**e), &opts);
    for prim in &src.prims {
        if matches!(prim.kind, Kind::Infinite { .. } | Kind::Mask) {
            continue;
        }
        let Some(pts) = src.points(prim).iter().map(|p| map(&dp, u, edit, *p)).collect::<Option<Vec<Vec2>>>() else { continue };
        let buf = if prim.kind == Kind::Tris { &mut out.tris } else { &mut out.verts };
        let start = buf.len() as u32;
        buf.extend_from_slice(&pts);
        for p in &pts {
            out.bounds.add(*p);
        }
        out.prims.push(DPrim { color: UNDERLAY_COLOR, aci7: false, transparency: UNDERLAY_TRANSPARENCY, start, len: pts.len() as u32, ..*prim });
    }
    out
}

/// The bounds of the underlay's mapped linework in `space` (EMPTY without one): ZOOM Extents
/// fits it with the edited view's own entities.
pub(super) fn extents(d: &Drawing, space: &Space) -> Bounds2 {
    draw(d, space, &Options::default()).bounds
}

/// Resets [`NESTED`] when dropped.
struct NestedGuard;

impl Drop for NestedGuard {
    fn drop(&mut self) {
        NESTED.with(|n| n.set(false));
    }
}

/// CADCraft's own snap in the underlay's block at the cursor (mapped edited → underlay, the
/// aperture scaled by the map), mapped back. Empty without an underlay, or inside such a call.
pub(super) fn snaps(d: &Drawing, space: &Space, cursor: Vec2, aperture: f64) -> Vec<SnapHit> {
    if nested() {
        return Vec::new();
    }
    let Some((dp, edit, u)) = pair(d, space) else { return Vec::new() };
    let Some(c) = map(&dp, edit, u, cursor) else { return Vec::new() };
    let Some(edge) = map(&dp, edit, u, cursor + Vec2::new(aperture, 0.0)) else { return Vec::new() };
    let ap = c.dist(edge);
    NESTED.with(|n| n.set(true));
    let hit = {
        let _g = NestedGuard;
        osnap(d, &Space::Block(view_block_name(u)), c, ap, UNDERLAY_SNAPS, None, false)
    };
    let Some(hit) = hit else { return Vec::new() };
    let Some(point) = map(&dp, u, edit, hit.point) else { return Vec::new() };
    vec![SnapHit { point, mode: hit.mode, name: hit.name, viewport: None, deferred: None }]
}

#[cfg(test)]
pub(crate) fn reset() {
    chosen().clear();
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::overlays::{lamb_overlay, lamb_snaps};
    use crate::schema::{read_document, write_document};
    use lamb_core::geometry::transform2d::Transform2D;
    use lamb_core::model::{AnyView, Placement};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type E = Box<dyn std::error::Error>;

    /// Holds the reference pane's test lock too (both are process-wide palette state).
    pub(crate) fn lock() -> std::sync::MutexGuard<'static, ()> {
        let g = crate::refpane::tests::lock();
        reset();
        g
    }

    fn block(id: Id) -> Space {
        Space::Block(view_block_name(id))
    }

    fn near(a: Vec2, b: Vec2) -> bool {
        a.dist(b) < 1e-6
    }

    /// Ground Floor (A, identity) and an offset plan B whose placement is rotated 90° and moved
    /// to (10000, 5000), with a line (0,0)-(1000,0) drawn in B and a line in A; editing A.
    fn plans() -> std::result::Result<(Session, Id, Id), E> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let a = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no plan")?);
        let r = s.execute("newoffsetplan", &json!({ "mode": "offset", "fromViewId": a.0, "floorToFloorMm": 3000 }))?;
        let b = Id(r["viewId"].as_u64().ok_or("no plan B")?);
        let mut doc = read_document(s.doc()?)?;
        for v in &mut doc.views {
            if let AnyView::Plan(p) = v
                && p.id == b
                && let Some(Placement::Plan { transform, .. }) = &mut p.placement
            {
                *transform = Transform2D { origin_x: 10000.0, origin_y: 5000.0, rotation: std::f64::consts::FRAC_PI_2 };
            }
        }
        write_document(s.doc_mut()?, &doc)?;
        s.execute("editview", &json!({ "viewId": b.0 }))?;
        s.execute("line", &json!({ "points": [[0, 0], [1000, 0]] }))?;
        s.execute("editview", &json!({ "viewId": a.0 }))?;
        s.execute("line", &json!({ "points": [[-3000, -3000], [-2000, -3000]] }))?;
        Ok((s, a, b))
    }

    fn polylines(l: &DisplayList) -> Vec<(DPrim, Vec<Vec2>)> {
        l.prims.iter().filter(|p| p.kind == Kind::Polyline).map(|p| (*p, l.points(p).to_vec())).collect()
    }

    #[test]
    fn a_plan_underlay_draws_mapped_and_pale_before_the_markers() -> R {
        let _g = lock();
        let (mut s, a, b) = plans()?;
        let opts = Options { view_height: 10000.0, ..Options::default() };
        let before = lamb_overlay(s.doc()?, &block(a), &opts);
        let gen0 = cadcraft_render::overlays_generation();
        let r = s.execute("underlay", &json!({ "viewId": b.0 }))?;
        assert_eq!(r["message"], json!(format!("<Underlay: {}>", view_name(s.doc()?, b))));
        assert_eq!(r["viewId"], json!(b.0));
        assert!(cadcraft_render::overlays_generation() > gen0, "UNDERLAY bumps the overlay generation");
        let list = lamb_overlay(s.doc()?, &block(a), &opts);
        assert_eq!(list.prims.len(), before.prims.len() + 1, "one line added");
        let (prim, pts) = polylines(&list).into_iter().next().ok_or("no polyline")?;
        assert_eq!((prim.color, prim.transparency, prim.aci7), (UNDERLAY_COLOR, UNDERLAY_TRANSPARENCY, false), "pale");
        assert_eq!(list.prims[0], prim, "drawn first, under the markers");
        assert_eq!(pts.len(), 2);
        assert!(near(pts[0], Vec2::new(10000.0, 5000.0)) && near(pts[1], Vec2::new(10000.0, 6000.0)), "{pts:?}");

        let r = s.execute("underlay", &json!({ "viewId": b.0 }))?;
        assert_eq!(r["message"], json!("<Underlay off>"), "the current one again turns it off");
        assert_eq!(lamb_overlay(s.doc()?, &block(a), &opts).prims.len(), before.prims.len());
        Ok(())
    }

    #[test]
    fn underlay_endpoints_snap_and_nothing_when_off() -> R {
        let _g = lock();
        let (mut s, a, b) = plans()?;
        let cursor = Vec2::new(10003.0, 6002.0);
        assert!(lamb_snaps(s.doc()?, &block(a), cursor, 10.0).is_empty(), "off: nothing");
        s.execute("underlay", &json!({ "viewId": b.0 }))?;
        let hits = lamb_snaps(s.doc()?, &block(a), cursor, 10.0);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(near(hits[0].point, Vec2::new(10000.0, 6000.0)), "{:?}", hits[0].point);
        let mid = lamb_snaps(s.doc()?, &block(a), Vec2::new(9998.0, 5501.0), 10.0);
        assert!(mid.iter().any(|h| near(h.point, Vec2::new(10000.0, 5500.0))), "{mid:?}");
        s.execute("underlay", &json!({}))?;
        assert!(lamb_snaps(s.doc()?, &block(a), cursor, 10.0).is_empty(), "no viewId turns it off");
        Ok(())
    }

    /// The zoomed view's visible box (untwisted), as (min, max).
    fn visible(s: &Session) -> std::result::Result<(Vec2, Vec2), E> {
        let v = s.state()?.view();
        let (w, h) = s.viewport_px;
        let half = Vec2::new(v.height * w / h / 2.0, v.height / 2.0);
        Ok((v.center - half, v.center + half))
    }

    fn inside(b: (Vec2, Vec2), p: Vec2) -> bool {
        p.x >= b.0.x && p.x <= b.1.x && p.y >= b.0.y && p.y <= b.1.y
    }

    #[test]
    fn zoom_extents_fits_the_visible_underlay_and_ignores_it_when_off() -> R {
        let _g = lock();
        let (mut s, _a, b) = plans()?;
        s.execute("underlay", &json!({ "viewId": b.0 }))?;
        s.execute("zoom.extents", &json!({}))?;
        let box_on = visible(&s)?;
        for p in [Vec2::new(-3000.0, -3000.0), Vec2::new(-2000.0, -3000.0), Vec2::new(10000.0, 5000.0), Vec2::new(10000.0, 6000.0)] {
            assert!(inside(box_on, p), "{p:?} outside {box_on:?}");
        }
        s.execute("underlay", &json!({}))?;
        s.execute("zoom.extents", &json!({}))?;
        let box_off = visible(&s)?;
        assert!(inside(box_off, Vec2::new(-3000.0, -3000.0)) && inside(box_off, Vec2::new(-2000.0, -3000.0)));
        assert!(!inside(box_off, Vec2::new(10000.0, 5000.0)), "underlay off: only A's own line ({box_off:?})");
        Ok(())
    }

    #[test]
    fn mutual_underlays_terminate_with_each_ones_own_hits() -> R {
        let _g = lock();
        let (mut s, a, b) = plans()?;
        s.execute("underlay", &json!({ "viewId": b.0 }))?;
        s.execute("editview", &json!({ "viewId": b.0 }))?;
        s.execute("underlay", &json!({ "viewId": a.0 }))?;
        // In A: B's line end. In B: A's line end (-3000,-3000) in B's local = R(-90°)(p - origin).
        let in_a = lamb_snaps(s.doc()?, &block(a), Vec2::new(10001.0, 6001.0), 10.0);
        assert_eq!(in_a.len(), 1);
        assert!(near(in_a[0].point, Vec2::new(10000.0, 6000.0)));
        let a_end_in_b = Vec2::new(-8000.0, 13000.0);
        let in_b = lamb_snaps(s.doc()?, &block(b), a_end_in_b + Vec2::new(2.0, 1.0), 10.0);
        assert_eq!(in_b.len(), 1, "{in_b:?}");
        assert!(near(in_b[0].point, a_end_in_b), "{:?}", in_b[0].point);
        // The whole osnap, providers included, terminates too.
        let hit = osnap(s.doc()?, &block(b), a_end_in_b + Vec2::new(2.0, 1.0), 10.0, mode::END, None, false).ok_or("no snap")?;
        assert!(near(hit.point, a_end_in_b));
        Ok(())
    }

    /// Ground Floor with sections A-A (0,1000)->(5000,1000) and B-B the other way at y = 2000.
    fn sections() -> std::result::Result<(Session, Id, Id, Id), E> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no plan")?);
        let mut sec = |name: &str, from: [f64; 2], to: [f64; 2]| -> std::result::Result<Id, E> {
            let r = s.execute(
                "newsectionline",
                &json!({ "name": name, "hostViewId": plan.0, "cutFrom": {"x": from[0], "y": from[1]}, "cutTo": {"x": to[0], "y": to[1]} }),
            )?;
            Ok(Id(r["viewId"].as_u64().ok_or("no section")?))
        };
        let a = sec("A-A", [0.0, 1000.0], [5000.0, 1000.0])?;
        let b = sec("B-B", [5000.0, 2000.0], [0.0, 2000.0])?;
        Ok((s, plan, a, b))
    }

    #[test]
    fn opposite_parallel_sections_are_mirrored() -> R {
        let _g = lock();
        let (mut s, _plan, a, b) = sections()?;
        let cands = candidates(s.doc()?, a);
        assert_eq!(cands, [UnderlayCandidate { view_id: b, mirrored: true }]);
        s.execute("editview", &json!({ "viewId": b.0 }))?;
        s.execute("line", &json!({ "points": [[100, 500], [600, 500]] }))?;
        s.execute("editview", &json!({ "viewId": a.0 }))?;
        s.execute("underlay", &json!({ "viewId": b.0 }))?;
        let list = lamb_overlay(s.doc()?, &block(a), &Options::default());
        let (_, pts) = polylines(&list).into_iter().next().ok_or("no polyline")?;
        assert!((pts[0].y - pts[1].y).abs() < 1e-6, "z unchanged: {pts:?}");
        assert!(((pts[0].x - pts[1].x) - 500.0).abs() < 1e-6, "mirrored in u: {pts:?}");
        Ok(())
    }

    #[test]
    fn a_plan_and_a_section_are_not_underlays_of_each_other() -> R {
        let _g = lock();
        let (mut s, plan, a, _b) = sections()?;
        assert!(candidates(s.doc()?, plan).is_empty(), "no other plan");
        let e = s.execute("underlay", &json!({ "viewId": a.0 })).err().ok_or("accepted a section under a plan")?;
        assert!(e.to_string().contains("Not an underlay view for Ground Floor"), "{e}");
        s.execute("editview", &json!({}))?;
        let e = s.execute("underlay", &json!({})).err().ok_or("accepted outside a view")?;
        assert!(e.to_string().contains("EDITVIEW"), "{e}");
        Ok(())
    }

    #[test]
    fn the_prompt_lists_picks_and_turns_off() -> R {
        let _g = lock();
        let (mut s, _plan, a, b) = sections()?;
        s.execute("editview", &json!({ "viewId": a.0 }))?;
        s.start("underlay")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter underlay view name or [Off/?]:"));
        s.input(Input::Keyword("?".into()))?;
        assert!(s.log.iter().any(|l| l == &format!("{}  B-B (mirrored)", b.0)), "{:?}", s.log);
        s.input(Input::Text("b-b".into()))?;
        assert!(s.running.is_none());
        assert_eq!(current(s.doc()?, a), Some(b));
        s.start("underlay")?;
        s.input(Input::Keyword("Off".into()))?;
        assert_eq!(current(s.doc()?, a), None);
        Ok(())
    }
}

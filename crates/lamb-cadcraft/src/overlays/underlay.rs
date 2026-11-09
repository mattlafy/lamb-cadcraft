//! The underlay (LAMB T350, TS `ViewsPanel.tsx` checkbox / `underlayByView`): another placed view
//! of the same kind as the edited view, drawn pale under it in the edited view's local
//! coordinates, with its endpoints, midpoints, centres, intersections and nodes snappable there.
//! Which views qualify, and the point map between the two local spaces, are lamb-core's
//! [`find_underlay_candidates`] and [`map_view_local_point`] — an affine map for both supported
//! pairings (plan↔plan, parallel vertical↔vertical), so mapping display-list vertices is exact.
//!
//! Session UI state, never saved (ruling T350.1): each edited view's underlays ([`CHOSEN`], block
//! name → set of block names; LAMB T522: any number at once). A choice that is no longer a
//! candidate reads as off. LAMB T590: an edited plan or section with an assembly block
//! ([`assembly_block_name`]) also draws that block's lines under it, on by default (LAMB T673:
//! in their layers' colours, as on sheets — only other views' underlays are pale),
//! `underlay {assembly: false}` turning it off ([`ASSEMBLY_OFF`], session state too). Display list
//! only: plots are unchanged. UNDERLAY changes no
//! drawing revision, so it bumps [`cadcraft_render::bump_overlays`] for the canvas's overlay cache.
//! LAMB T685: the assembly block's lines snap with the running object snaps (the `osmode` the
//! snap call was given) while DETAILSNAP is on ([`DETAIL_SNAPS_OFF`], session state, on by
//! default); other views' underlays keep their fixed [`UNDERLAY_SNAPS`].

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

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
use crate::schema::{assembly_block_name, view_block_name};

/// TS `rgba(210,210,210,0.35)`.
const UNDERLAY_COLOR: Rgb = Rgb(210, 210, 210);
const UNDERLAY_TRANSPARENCY: u8 = 65;
/// What TS `collectSnapPoints` offers from the underlay.
const UNDERLAY_SNAPS: u32 = mode::END | mode::MID | mode::CEN | mode::INT | mode::NOD;

/// Edited view's block name -> its underlays' block names.
static CHOSEN: Mutex<BTreeMap<String, BTreeSet<String>>> = Mutex::new(BTreeMap::new());

/// Block names of the views whose assembly underlay is turned off (LAMB T590; on by default).
static ASSEMBLY_OFF: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Set while DETAILSNAP is off: the assembly block's lines offer no snaps (LAMB T685).
static DETAIL_SNAPS_OFF: AtomicBool = AtomicBool::new(false);

/// Whether the assembly block's lines snap (DETAILSNAP; on by default).
pub(crate) fn detail_snaps_on() -> bool {
    !DETAIL_SNAPS_OFF.load(Ordering::Relaxed)
}

/// Turn detail snaps on or off; returns its message. Nothing draws differently, so no overlay bump.
fn set_detail_snaps(on: bool) -> String {
    DETAIL_SNAPS_OFF.store(!on, Ordering::Relaxed);
    if on { "Detail snaps on".to_owned() } else { "Detail snaps off".to_owned() }
}

/// DETAILSNAP `{on?: bool}`: the assembled detail lines snap with the running object snaps, or
/// not; `on` omitted toggles. Returns `{on, message}`; not an undo step.
pub(crate) fn detailsnap(_s: &mut Session, p: &Value) -> Result<Value> {
    let on = match p.get("on").filter(|v| !v.is_null()) {
        Some(v) => v.as_bool().ok_or_else(|| other("on must be true or false"))?,
        None => !detail_snaps_on(),
    };
    Ok(json!({ "on": on, "message": set_detail_snaps(on) }))
}

/// DETAILSNAP typed or from the menu: no prompt, it toggles and echoes, like a status-bar switch.
struct DetailSnapMachine;

impl Interactive for DetailSnapMachine {
    fn name(&self) -> &'static str {
        "detailsnap"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Detail snaps", Accept::TEXT)
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let m = set_detail_snaps(!detail_snaps_on());
        s.echo(m);
        Ok(Step::Done)
    }
    fn input(&mut self, _s: &mut Session, _i: Input) -> Result<Step> {
        Ok(Step::Done)
    }
}

pub(crate) fn detailsnap_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(DetailSnapMachine))
}

fn assembly_off() -> std::sync::MutexGuard<'static, BTreeSet<String>> {
    ASSEMBLY_OFF.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether `edit`'s assembly block is drawn pale under it.
pub(crate) fn assembly_shown(edit: Id) -> bool {
    !assembly_off().contains(&view_block_name(edit))
}

/// Turn `edit`'s assembly underlay on or off; returns its message.
fn set_assembly(edit: Id, on: bool) -> String {
    let key = view_block_name(edit);
    if on {
        assembly_off().remove(&key);
    } else {
        assembly_off().insert(key);
    }
    cadcraft_render::bump_overlays();
    if on { "<Assembly underlay on>".to_owned() } else { "<Assembly underlay off>".to_owned() }
}

thread_local! {
    /// Set while [`snaps`] runs CADCraft's `osnap` in the underlay's space: that call asks the snap
    /// providers again, and LAMB's answers nothing then, so A↔B underlays never recurse.
    static NESTED: Cell<bool> = const { Cell::new(false) };
}

fn chosen() -> std::sync::MutexGuard<'static, BTreeMap<String, BTreeSet<String>>> {
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

/// `edit`'s underlays: the remembered choices still among its candidates, in candidate order.
pub(crate) fn current(d: &Drawing, edit: Id) -> Vec<Id> {
    let Some(set) = chosen().get(&view_block_name(edit)).cloned() else { return Vec::new() };
    let on: BTreeSet<Id> = set.iter().filter_map(|n| view_id(&Space::Block(n.clone()))).collect();
    candidates(d, edit).into_iter().map(|c| c.view_id).filter(|id| on.contains(id)).collect()
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

/// Toggle `toggle` in or out of `edit`'s underlays, or clear them all (`None`). Choices no longer
/// among the candidates are forgotten. Returns `{viewIds, message}` for the resulting set.
fn set(d: &Drawing, edit: Id, toggle: Option<Id>) -> Value {
    let mut next: BTreeSet<Id> = current(d, edit).into_iter().collect();
    match toggle {
        Some(u) => {
            if !next.remove(&u) {
                next.insert(u);
            }
        }
        None => next.clear(),
    }
    {
        let mut map = chosen();
        let key = view_block_name(edit);
        if next.is_empty() {
            map.remove(&key);
        } else {
            map.insert(key, next.iter().map(|u| view_block_name(*u)).collect());
        }
    }
    cadcraft_render::bump_overlays();
    let on = current(d, edit);
    let message = if on.is_empty() {
        "<Underlay off>".to_owned()
    } else {
        format!("<Underlay: {}>", on.iter().map(|u| view_name(d, *u)).collect::<Vec<_>>().join(", "))
    };
    json!({ "viewIds": on.iter().map(|u| u.0).collect::<Vec<_>>(), "message": message })
}

/// UNDERLAY `{viewId?, assembly?}`: toggles an underlay candidate of the edited view in or out of its
/// underlays; no `viewId` (or `null`) clears them. Returns `{viewIds, message}`; not an undo step.
/// `assembly: false|true` (LAMB T590) instead turns the edited view's assembly underlay off or on,
/// leaving its view underlays as they are, and returns `{assembly, message}`.
pub(crate) fn underlay(s: &mut Session, p: &Value) -> Result<Value> {
    let edit = edited_view(s).ok_or_else(|| other("Edit a plan or section first (EDITVIEW)."))?;
    if let Some(a) = p.get("assembly").filter(|v| !v.is_null()) {
        let on = a.as_bool().ok_or_else(|| other("assembly must be true or false"))?;
        return Ok(json!({ "assembly": on, "message": set_assembly(edit, on) }));
    }
    let d = s.doc()?;
    let Some(v) = p.get("viewId").filter(|v| !v.is_null()) else { return Ok(set(d, edit, None)) };
    let id = id_value(v).ok_or_else(|| other("No such view"))?;
    if !candidates(d, edit).iter().any(|c| c.view_id == id) {
        return Err(other(format!("Not an underlay view for {}", view_name(d, edit))));
    }
    Ok(set(d, edit, Some(id)))
}

/// UNDERLAY: `Enter underlay view name or [Assembly/Off/?]:` — a candidate's name or id toggles it,
/// `Assembly` toggles the assembly underlay, `Off` clears the view underlays, `?` lists the
/// candidates, Enter leaves it as it is.
struct UnderlayMachine;

impl Interactive for UnderlayMachine {
    fn name(&self) -> &'static str {
        "underlay"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter underlay view name", Accept::TEXT).kw(&["Assembly", "Off", "?"])
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
        if text.eq_ignore_ascii_case("assembly") || text.eq_ignore_ascii_case("a") {
            let m = set_assembly(edit, !assembly_shown(edit));
            s.echo(m);
            return Ok(Step::Done);
        }
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

/// The edited view's assembly block (LAMB T590, unless turned off) and then every underlay's
/// linework for `space`, mapped into the edited view's local coordinates, pale, one pass per
/// underlay in candidate order; the assembly lines keep their own colours (LAMB T673). Infinite lines and wipeout masks are left out. Empty without either.
pub(super) fn draw(d: &Drawing, space: &Space, opts: &Options) -> DisplayList {
    let mut out = DisplayList { bounds: Bounds2::EMPTY, ..DisplayList::default() };
    let Some((dp, edit)) = view_of(d, space) else { return out };
    let opts = Options { view: None, ..opts.clone() };
    if assembly_shown(edit)
        && let Some(block) = d.block(&assembly_block_name(edit))
    {
        // Assembly lines are in the view's own local coordinates: no map.
        push_mapped(&cadcraft_render::build_entities(d, block.entities.iter().map(|e| &**e), &opts), Some, false, &mut out);
    }
    for u in current(d, edit) {
        draw_one(d, &dp, edit, u, &opts, &mut out);
    }
    out
}

/// Append `src`'s primitives to `out`, each point through `map`, pale when `pale` (else in their
/// own colour and transparency); infinite lines and masks are left out, and so is any primitive
/// with a point that does not map.
fn push_mapped(src: &DisplayList, map: impl Fn(Vec2) -> Option<Vec2>, pale: bool, out: &mut DisplayList) {
    for prim in &src.prims {
        if matches!(prim.kind, Kind::Infinite { .. } | Kind::Mask) {
            continue;
        }
        let Some(pts) = src.points(prim).iter().map(|p| map(*p)).collect::<Option<Vec<Vec2>>>() else { continue };
        let buf = if prim.kind == Kind::Tris { &mut out.tris } else { &mut out.verts };
        let start = buf.len() as u32;
        buf.extend_from_slice(&pts);
        for p in &pts {
            out.bounds.add(*p);
        }
        let prim = DPrim { start, len: pts.len() as u32, ..*prim };
        out.prims.push(if pale { DPrim { color: UNDERLAY_COLOR, aci7: false, transparency: UNDERLAY_TRANSPARENCY, ..prim } } else { prim });
    }
}

/// Append underlay `u`'s mapped, pale linework to `out`.
fn draw_one(d: &Drawing, dp: &DrawingProject<'_>, edit: Id, u: Id, opts: &Options, out: &mut DisplayList) {
    let Some(block) = d.block(&view_block_name(u)) else { return };
    let src = cadcraft_render::build_entities(d, block.entities.iter().map(|e| &**e), opts);
    push_mapped(&src, |p| map(dp, u, edit, p), true, out);
}

/// The bounds of every underlay's mapped linework in `space` (EMPTY without one): ZOOM Extents
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

/// CADCraft's own snap in the edited view's assembly block with the running `osmode` (LAMB T685;
/// shown, DETAILSNAP on, no map: its lines are in the view's own coordinates), then in each
/// underlay's block at the cursor (mapped edited → underlay, the aperture scaled by the map),
/// mapped back: at most one hit per block. Empty without either, or inside such a call.
pub(super) fn snaps(d: &Drawing, space: &Space, cursor: Vec2, aperture: f64, osmode: u32) -> Vec<SnapHit> {
    if nested() {
        return Vec::new();
    }
    let Some((dp, edit)) = view_of(d, space) else { return Vec::new() };
    let mut hits: Vec<SnapHit> = snap_assembly(d, edit, cursor, aperture, osmode).into_iter().collect();
    hits.extend(current(d, edit).into_iter().filter_map(|u| snap_one(d, &dp, edit, u, cursor, aperture)));
    hits
}

/// [`snaps`] for `edit`'s assembly block.
fn snap_assembly(d: &Drawing, edit: Id, cursor: Vec2, aperture: f64, osmode: u32) -> Option<SnapHit> {
    if !detail_snaps_on() || !assembly_shown(edit) {
        return None;
    }
    let name = assembly_block_name(edit);
    d.block(&name)?;
    NESTED.with(|n| n.set(true));
    let hit = {
        let _g = NestedGuard;
        osnap(d, &Space::Block(name), cursor, aperture, osmode & !mode::PROVIDER, None, false)
    }?;
    Some(SnapHit { viewport: None, deferred: None, ..hit })
}

/// [`snaps`] for one underlay `u`.
fn snap_one(d: &Drawing, dp: &DrawingProject<'_>, edit: Id, u: Id, cursor: Vec2, aperture: f64) -> Option<SnapHit> {
    let c = map(dp, edit, u, cursor)?;
    let edge = map(dp, edit, u, cursor + Vec2::new(aperture, 0.0))?;
    let ap = c.dist(edge);
    NESTED.with(|n| n.set(true));
    let hit = {
        let _g = NestedGuard;
        osnap(d, &Space::Block(view_block_name(u)), c, ap, UNDERLAY_SNAPS, None, false)
    }?;
    let point = map(dp, u, edit, hit.point)?;
    Some(SnapHit { point, mode: hit.mode, name: hit.name, viewport: None, deferred: None })
}

#[cfg(test)]
pub(crate) fn reset() {
    chosen().clear();
    assembly_off().clear();
    DETAIL_SNAPS_OFF.store(false, Ordering::Relaxed);
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
        assert_eq!(r["viewIds"], json!([b.0]));
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
        assert!(lamb_snaps(s.doc()?, &block(a), cursor, 10.0, mode::END).is_empty(), "off: nothing");
        s.execute("underlay", &json!({ "viewId": b.0 }))?;
        let hits = lamb_snaps(s.doc()?, &block(a), cursor, 10.0, mode::END);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(near(hits[0].point, Vec2::new(10000.0, 6000.0)), "{:?}", hits[0].point);
        let mid = lamb_snaps(s.doc()?, &block(a), Vec2::new(9998.0, 5501.0), 10.0, mode::END);
        assert!(mid.iter().any(|h| near(h.point, Vec2::new(10000.0, 5500.0))), "{mid:?}");
        s.execute("underlay", &json!({}))?;
        assert!(lamb_snaps(s.doc()?, &block(a), cursor, 10.0, mode::END).is_empty(), "no viewId turns it off");
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
        let in_a = lamb_snaps(s.doc()?, &block(a), Vec2::new(10001.0, 6001.0), 10.0, mode::END);
        assert_eq!(in_a.len(), 1);
        assert!(near(in_a[0].point, Vec2::new(10000.0, 6000.0)));
        let a_end_in_b = Vec2::new(-8000.0, 13000.0);
        let in_b = lamb_snaps(s.doc()?, &block(b), a_end_in_b + Vec2::new(2.0, 1.0), 10.0, mode::END);
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
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter underlay view name or [Assembly/Off/?]:"));
        s.input(Input::Keyword("?".into()))?;
        assert!(s.log.iter().any(|l| l == &format!("{}  B-B (mirrored)", b.0)), "{:?}", s.log);
        s.input(Input::Text("b-b".into()))?;
        assert!(s.running.is_none());
        assert_eq!(current(s.doc()?, a), [b]);
        s.start("underlay")?;
        s.input(Input::Keyword("Off".into()))?;
        assert!(current(s.doc()?, a).is_empty());
        Ok(())
    }

    /// LAMB T590: the tagged room's assembly lines draw under the edited plan, in its own
    /// coordinates, until `underlay {assembly: false}`; LAMB T673: in their layers' colours.
    #[test]
    fn the_edited_plans_assembly_block_draws_in_its_layer_colours_until_turned_off() -> R {
        let _g = lock();
        let (mut s, plan) = crate::assembly::tests::room("exterior_face", true)?;
        assert!(draw(s.doc()?, &block(plan), &Options::default()).prims.is_empty(), "no assembly block, nothing drawn");
        s.doc_mut()?.layer_mut("CLAD").ok_or("no layer")?.color = cadcraft_color::Color::Index(1);
        s.doc_mut()?.layer_mut("GYP").ok_or("no layer")?.color = cadcraft_color::Color::Index(5);
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let asm = s.doc()?.block(&assembly_block_name(plan)).ok_or("no assembly block")?.clone();
        assert_eq!(asm.entities.len(), 12);
        let list = draw(s.doc()?, &block(plan), &Options::default());
        let lines = polylines(&list);
        assert_eq!(lines.len(), 12, "one line per assembly line");
        let (red, blue) = (Rgb(255, 0, 0), Rgb(0, 0, 255));
        assert!(lines.iter().all(|(p, _)| (p.color == red || p.color == blue) && p.transparency == 0), "{lines:?}");
        // The bottom run's CLAD band at y = -20, unmapped, in CLAD's red.
        assert!(lines.iter().any(|(p, pts)| p.color == red && pts.iter().all(|p| (p.y + 20.0).abs() < 1e-6)), "{lines:?}");
        assert!(lines.iter().any(|(p, _)| p.color == blue), "GYP's bands in blue");
        let full = lamb_overlay(s.doc()?, &block(plan), &Options::default());
        assert_eq!(full.prims[..12], list.prims[..], "drawn first, under the markers");

        let r = s.execute("underlay", &json!({ "assembly": false }))?;
        assert_eq!(r, json!({ "assembly": false, "message": "<Assembly underlay off>" }));
        assert!(draw(s.doc()?, &block(plan), &Options::default()).prims.is_empty());
        s.start("underlay")?;
        s.input(Input::Keyword("Assembly".into()))?;
        assert!(s.log.iter().any(|l| l == "<Assembly underlay on>"), "{:?}", s.log);
        assert_eq!(polylines(&draw(s.doc()?, &block(plan), &Options::default())).len(), 12);
        Ok(())
    }

    /// LAMB T685: the edited plan's assembled detail lines snap with the running OSMODE while
    /// DETAILSNAP is on and the assembly underlay shows; nowhere else.
    #[test]
    fn assembled_detail_lines_snap_with_the_running_osnaps_until_detailsnap_is_off() -> R {
        let _g = lock();
        let (mut s, plan) = crate::assembly::tests::room("exterior_face", true)?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        // The bottom run's CLAD band at y = -20: its first end and its midpoint.
        let lines = polylines(&draw(s.doc()?, &block(plan), &Options::default()));
        let band = lines
            .iter()
            .map(|(_, pts)| pts.clone())
            .find(|pts| pts.len() == 2 && pts.iter().all(|p| (p.y + 20.0).abs() < 1e-6))
            .ok_or("no bottom CLAD band")?;
        let (end, mid) = (band[0], (band[0] + band[1]) * 0.5);
        let off = Vec2::new(1.0, -2.0);
        let snap = |s: &Session, at: Vec2, osmode: u32| -> std::result::Result<Option<SnapHit>, E> {
            Ok(osnap(s.doc()?, &block(plan), at + off, 10.0, osmode, None, false))
        };
        let hits = |s: &Session, at: Vec2| -> std::result::Result<bool, E> { Ok(snap(s, at, mode::END)?.is_some_and(|h| near(h.point, at))) };

        let hit = snap(&s, end, mode::END)?.ok_or("no END hit")?;
        assert!(near(hit.point, end), "{hit:?} vs {end:?}");
        assert_eq!(hit.mode, mode::PROVIDER, "a provider hit");
        assert!(snap(&s, end, mode::CEN)?.is_none(), "CEN only: nothing");
        let hit = snap(&s, mid, mode::MID)?.ok_or("no MID hit")?;
        assert!(near(hit.point, mid), "{hit:?} vs {mid:?}");

        let r = s.execute("detailsnap", &json!({ "on": false }))?;
        assert_eq!(r, json!({ "on": false, "message": "Detail snaps off" }));
        assert!(!detail_snaps_on());
        assert!(!hits(&s, end)?, "detail snaps off: no hit");
        let r = s.execute("detailsnap", &json!({}))?;
        assert_eq!(r, json!({ "on": true, "message": "Detail snaps on" }), "{{}} toggles back");
        assert!(hits(&s, end)?);

        s.start("detailsnap")?;
        assert!(s.running.is_none(), "no prompt");
        assert!(s.log.iter().any(|l| l == "Detail snaps off"), "{:?}", s.log);
        assert!(!hits(&s, end)?);
        s.execute("detailsnap", &json!({ "on": true }))?;

        s.execute("underlay", &json!({ "assembly": false }))?;
        assert!(!hits(&s, end)?, "assembly underlay off: no hit");
        s.execute("underlay", &json!({ "assembly": true }))?;
        assert!(hits(&s, end)?);

        let d = s.doc()?;
        assert!(snaps(d, &Space::Model, end + off, 10.0, mode::END).is_empty(), "the Model tab: no assembly hits");
        assert!(snaps(d, &Space::Paper("Layout1".into()), end + off, 10.0, mode::END).is_empty(), "a sheet: no assembly hits");
        Ok(())
    }

    /// LAMB T522: plan A with two other placed plans B (rotated, offset) and C (identity, 6 m up),
    /// each with its own line; any number of them are underlays at once.
    #[test]
    fn several_underlays_draw_and_snap_together_and_toggle_one_at_a_time() -> R {
        let _g = lock();
        let (mut s, a, b) = plans()?;
        let r = s.execute("newoffsetplan", &json!({ "mode": "offset", "fromViewId": a.0, "floorToFloorMm": 6000 }))?;
        let c = Id(r["viewId"].as_u64().ok_or("no plan C")?);
        s.execute("editview", &json!({ "viewId": c.0 }))?;
        s.execute("line", &json!({ "points": [[0, 2000], [0, 3000]] }))?;
        s.execute("editview", &json!({ "viewId": a.0 }))?;
        let cands: Vec<Id> = candidates(s.doc()?, a).iter().map(|c| c.view_id).collect();
        assert!(cands.contains(&b) && cands.contains(&c), "{cands:?}");
        let opts = Options { view_height: 10000.0, ..Options::default() };
        let base = lamb_overlay(s.doc()?, &block(a), &opts).prims.len();
        let b_end = Vec2::new(10000.0, 6000.0);
        let c_end = Vec2::new(0.0, 3000.0);
        let snaps_at = |s: &Session, p: Vec2| -> std::result::Result<bool, E> {
            Ok(lamb_snaps(s.doc()?, &block(a), p + Vec2::new(2.0, 1.0), 10.0, mode::END).iter().any(|h| near(h.point, p)))
        };

        s.execute("underlay", &json!({ "viewId": b.0 }))?;
        let r = s.execute("underlay", &json!({ "viewId": c.0 }))?;
        let both: Vec<u64> = cands.iter().filter(|id| **id == b || **id == c).map(|id| id.0).collect();
        assert_eq!(r["viewIds"], json!(both), "both on, in candidate order");
        assert!(r["message"].as_str().is_some_and(|m| m.starts_with("<Underlay: ") && m.contains(", ")), "{}", r["message"]);
        let list = lamb_overlay(s.doc()?, &block(a), &opts);
        assert_eq!(list.prims.len(), base + 2, "both underlays' lines drawn");
        let lines: Vec<Vec<Vec2>> = polylines(&list).into_iter().filter(|(p, _)| p.color == UNDERLAY_COLOR).map(|(_, pts)| pts).collect();
        assert!(lines.iter().any(|l| l.iter().any(|p| near(*p, b_end))), "B's line: {lines:?}");
        assert!(lines.iter().any(|l| l.iter().any(|p| near(*p, c_end))), "C's line: {lines:?}");
        assert!(snaps_at(&s, b_end)? && snaps_at(&s, c_end)?, "both snappable");
        assert_eq!(current(s.doc()?, a).len(), 2);

        let r = s.execute("underlay", &json!({ "viewId": b.0 }))?;
        assert_eq!(r["viewIds"], json!([c.0]), "B toggled off, C stays");
        assert_eq!(lamb_overlay(s.doc()?, &block(a), &opts).prims.len(), base + 1);
        assert!(!snaps_at(&s, b_end)? && snaps_at(&s, c_end)?);

        let r = s.execute("underlay", &json!({}))?;
        assert_eq!((r["viewIds"].clone(), r["message"].clone()), (json!([]), json!("<Underlay off>")));
        assert_eq!(lamb_overlay(s.doc()?, &block(a), &opts).prims.len(), base);
        assert!(!snaps_at(&s, c_end)?);
        Ok(())
    }
}

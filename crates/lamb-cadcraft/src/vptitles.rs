//! Drawing titles under sheet viewports (LAMB T626): every single-view viewport on any layout —
//! the ones [`crate::sheets::sheet_refs`] numbers, so a detail tag's `<n>/<layout>` names the
//! title `n` on that layout — carries an `Insert` of a title block in its layout's paper space, on
//! layer [`TITLE_LAYER`], with its bottom-left corner's insert point [`TITLE_DROP_MM`] below the
//! viewport's bottom-left corner. The block is a Ø 10 mm circle, an underline and a graphic scale
//! bar; its attributes are `NUM` (the viewport's `n`, centred in the circle), `NAME` (the
//! view's name), `SCALE` (`1:<n>`, [`crate::sheets::viewport_scale`]) and the invisible `VP` (the
//! viewport's `id`, the title's identity). All have Lock position set.
//!
//! The block is named for what it draws and made on demand: `LAMB_VTITLE_<n>` for the viewport's
//! 1:`n` (whole, or one decimal with `P` for the point: `33P3`), with `_N<deg>` added on a plan
//! (whole degrees 0..359, clockwise of up on the sheet). The bar is the largest length from
//! {1, 2, 5} × 10^k mm that is at most [`BAR_MAX_MM`] on paper, with five ticks and its two ends
//! labelled. A plan's arrow shows project north (`SheetInfo.north_deg`, degrees clockwise from the
//! project's global +Y) in the plan's own frame, which is the frame its viewport shows: viewports
//! do not twist on LAMB sheets. Sections, elevations and details have no arrow. A plain
//! `LAMB_VTITLE` insert from an older drawing is a title too and is updated in place.
//!
//! An assembly viewport (`EntityData.assembly_of`, LAMB T635) gets a title too: `NAME` is
//! `<view name> framing`, and `NUM` continues after its layout's single-view viewports, by viewport
//! `id`. It is not in [`crate::sheets::sheet_refs`]' numbering, so no bubble's `SHEET` names it. It
//! counts as its plan for the arrow.
//!
//! [`vptitle_observer`] reconciles every layout's titles after every command: a title whose `VP`
//! matches is updated in place (same handle) when its place or text is off — so a title moved by
//! hand snaps back under its viewport — missing ones are added and the rest erased, and the drawing
//! is written back only when something changed, so the change lands in the command's own undo
//! step. Non-LAMB viewports get no title.

use std::sync::Arc;

use cadcraft_doc::{
    Attrib, AttribModes, Block, Circle, Common, Drawing, Entity, EntityKind, HAlign, Handle, Insert, Line, Space, Text, VAlign, Viewport,
};
use cadcraft_engine::Session;
use cadcraft_geom::Vec3;
use lamb_core::geometry::Point2;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::geometry::transform2d::{IDENTITY_TRANSFORM, world_to_local_dir2};
use lamb_core::model::{AnyView, Id, Placement};

use crate::schema::{read_document, read_entity_data, view_block_name};
use crate::sheets::{numbered_viewports, viewport_scale};
use cadcraft_doc::color::Color;

/// Every title block's name starts with this; a block by exactly this name is an older drawing's.
pub const TITLE_BLOCK: &str = "LAMB_VTITLE";
/// The layer titles are placed on.
pub const TITLE_LAYER: &str = "LAMB-TITLES";
/// How far the title's insert point sits below its viewport's bottom edge (3 mm clear of the
/// circle's top).
pub const TITLE_DROP_MM: f64 = 8.0;
const CIRCLE_R_MM: f64 = 5.0;
const LINE_FROM_MM: f64 = 12.0;
const LINE_TO_MM: f64 = 90.0;
/// The scale bar's greatest length on paper, mm.
const BAR_MAX_MM: f64 = 45.0;
const BAR_FROM_MM: f64 = 30.0;
const BAR_Y_MM: f64 = -3.0;
const BAR_TICK_TOP_MM: f64 = -1.5;
const BAR_TEXT_Y_MM: f64 = -5.5;
const BAR_TEXT_MM: f64 = 1.8;
const ARROW_R_MM: f64 = 4.0;
/// The arrow circle's centre, past the underline's end.
const ARROW_X_MM: f64 = LINE_TO_MM + 7.0;
const ARROW_HEAD_MM: f64 = 1.8;
/// The head's half angle, degrees.
const ARROW_HEAD_DEG: f64 = 25.0;
/// How far the `N`'s centre sits from the arrow's centre: just past the tip whichever way it points,
/// and inside the viewport above's clearance and [`crate::sheets`]' title room below.
const ARROW_N_AT_MM: f64 = 5.6;
const ARROW_N_MM: f64 = 2.5;

/// One title a viewport wants.
#[derive(Clone, Debug, PartialEq)]
struct Wanted {
    at: Vec3,
    vp: String,
    num: String,
    name: String,
    scale: String,
    /// The title block's name, from `n` and `north`.
    block: String,
    /// The denominator the bar is drawn for, to one decimal.
    n: f64,
    /// The arrow's whole degrees clockwise of up, on a plan.
    north: Option<i32>,
}

/// One layout's changes.
#[derive(Debug, Default)]
struct LayoutPlan {
    layout: String,
    remove: Vec<Handle>,
    update: Vec<(Handle, Wanted)>,
    add: Vec<Wanted>,
}

impl LayoutPlan {
    fn is_empty(&self) -> bool {
        self.remove.is_empty() && self.update.is_empty() && self.add.is_empty()
    }
}

fn near(a: Vec3, b: Vec3) -> bool {
    (a.x - b.x).hypot(a.y - b.y) <= TOLERANCE.coincidence_mm
}

/// `1:<n>`: an integer when within 1e-6 of one, else one decimal.
pub fn scale_label(n: f64) -> String {
    if (n - n.round()).abs() <= 1e-6 { format!("1:{}", n.round()) } else { format!("1:{n:.1}") }
}

/// A viewport's denominator to the one decimal block names carry, in tenths.
fn scale_tenths(n: f64) -> u64 {
    if n.is_finite() && n > 0.0 { ((n * 10.0).round() as u64).max(1) } else { 10 }
}

/// `LAMB_VTITLE_<n>`, `LAMB_VTITLE_<n>_N<deg>` on a plan.
fn block_name(tenths: u64, north: Option<i32>) -> String {
    let n = if tenths.is_multiple_of(10) { (tenths / 10).to_string() } else { format!("{}P{}", tenths / 10, tenths % 10) };
    north.map_or_else(|| format!("{TITLE_BLOCK}_{n}"), |deg| format!("{TITLE_BLOCK}_{n}_N{deg}"))
}

/// The largest model length from {1, 2, 5} × 10^k mm that is at most [`BAR_MAX_MM`] on paper at 1:`n`.
fn bar_length(n: f64) -> f64 {
    let max = BAR_MAX_MM * n;
    let k = max.log10().floor() as i32;
    [5.0, 2.0, 1.0].iter().map(|m| m * 10f64.powi(k)).find(|l| *l <= max).unwrap_or(0.5 * 10f64.powi(k))
}

/// A bar's far-end label: `200 mm` below a metre, else `2 m`.
fn bar_label(length: f64) -> String {
    if length < 1000.0 { format!("{length} mm") } else { format!("{} m", length / 1000.0) }
}

/// Project north (`north_deg`, clockwise from the global +Y) in a plan's frame, whose `rotation` is
/// counter-clockwise from local to global: whole degrees clockwise of up, 0..359.
fn plan_north(north_deg: f64, view: &AnyView) -> Option<i32> {
    let AnyView::Plan(plan) = view else { return None };
    let t = match &plan.placement {
        Some(Placement::Plan { transform, .. }) => transform,
        _ => &IDENTITY_TRANSFORM,
    };
    let (sin, cos) = north_deg.to_radians().sin_cos();
    let d = world_to_local_dir2(t, Point2 { x: sin, y: cos });
    Some((d.x.atan2(d.y).to_degrees().round() as i32).rem_euclid(360))
}

fn attrib(tag: &str, p: Vec3, height: f64, halign: HAlign, valign: VAlign, value: &str, invisible: bool) -> Attrib {
    Attrib {
        tag: tag.into(),
        text: Text {
            insert: p,
            align_pt: Some(p),
            height,
            value: value.into(),
            rotation: 0.0,
            width_factor: 1.0,
            oblique: 0.0,
            style: "Standard".into(),
            halign,
            valign,
        },
        invisible,
        constant: false,
        prompt: String::new(),
        props: Default::default(),
        multiline: None,
        modes: AttribModes { lock_position: true, ..AttribModes::default() },
    }
}

fn insert_of(w: &Wanted) -> Insert {
    let at = |dx: f64, dy: f64| Vec3::new(w.at.x + dx, w.at.y + dy, w.at.z);
    Insert {
        clip: None,
        block: w.block.clone(),
        insert: w.at,
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: vec![
            attrib("NUM", at(CIRCLE_R_MM, 0.0), 4.0, HAlign::Center, VAlign::Middle, &w.num, false),
            attrib("NAME", at(LINE_FROM_MM, 1.5), 3.5, HAlign::Left, VAlign::Baseline, &w.name, false),
            attrib("SCALE", at(LINE_FROM_MM, -4.5), 2.5, HAlign::Left, VAlign::Baseline, &w.scale, false),
            attrib("VP", w.at, 2.5, HAlign::Left, VAlign::Baseline, &w.vp, true),
        ],
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    }
}

fn attrib_value<'a>(i: &'a Insert, tag: &str) -> Option<&'a str> {
    i.attribs.iter().find(|a| a.tag.eq_ignore_ascii_case(tag)).map(|a| a.text.value.as_str())
}

/// An attribute's anchor: its alignment point when set, else its insert point.
fn pos(a: &Attrib) -> Vec3 {
    a.text.align_pt.unwrap_or(a.text.insert)
}

/// Whether a block's name makes its inserts drawing titles.
pub(crate) fn is_title_block(name: &str) -> bool {
    name.get(..TITLE_BLOCK.len()).is_some_and(|p| p.eq_ignore_ascii_case(TITLE_BLOCK))
}

fn title_of(e: &Entity) -> Option<&Insert> {
    match &e.kind {
        EntityKind::Insert(i) if is_title_block(&i.block) => Some(i),
        _ => None,
    }
}

/// Whether `e` is the title `w` wants, exactly.
fn matches(e: &Entity, i: &Insert, w: &Wanted) -> bool {
    e.common.layer == TITLE_LAYER
        && i.block.eq_ignore_ascii_case(&w.block)
        && near(i.insert, w.at)
        && attrib_value(i, "NUM") == Some(w.num.as_str())
        && attrib_value(i, "NAME") == Some(w.name.as_str())
        && attrib_value(i, "SCALE") == Some(w.scale.as_str())
        && i.attribs.len() == 4
        && i.attribs.iter().all(|a| a.modes.lock_position)
        && i.attribs.iter().zip(insert_of(w).attribs.iter()).all(|(a, b)| a.tag == b.tag && near(pos(a), pos(b)))
}

/// A centred text on layer `0` of a title block.
fn block_text(p: Vec3, height: f64, valign: VAlign, value: &str) -> EntityKind {
    EntityKind::Text(Text {
        insert: p,
        align_pt: Some(p),
        height,
        value: value.into(),
        rotation: 0.0,
        width_factor: 1.0,
        oblique: 0.0,
        style: "Standard".into(),
        halign: HAlign::Center,
        valign,
    })
}

/// A block's definition, on layer `0`: the number circle, the underline, the bar for 1:`n` and, with
/// `north`, the arrow.
fn ensure_block(d: &mut Drawing, name: &str, n: f64, north: Option<i32>) -> Result<(), String> {
    if d.block(name).is_some() {
        return Ok(());
    }
    d.blocks.insert(name.into(), Arc::new(Block::new(name)));
    let p = |x: f64, y: f64| Vec3::new(x, y, 0.0);
    let line = |a: Vec3, b: Vec3| EntityKind::Line(Line { a, b });
    let mut kinds =
        vec![EntityKind::Circle(Circle { center: p(CIRCLE_R_MM, 0.0), radius: CIRCLE_R_MM }), line(p(LINE_FROM_MM, 0.0), p(LINE_TO_MM, 0.0))];
    let length = bar_length(n);
    let on_paper = length / n;
    kinds.push(line(p(BAR_FROM_MM, BAR_Y_MM), p(BAR_FROM_MM + on_paper, BAR_Y_MM)));
    for i in 0..5 {
        let x = BAR_FROM_MM + on_paper * f64::from(i) / 4.0;
        kinds.push(line(p(x, BAR_Y_MM), p(x, BAR_TICK_TOP_MM)));
    }
    kinds.push(block_text(p(BAR_FROM_MM, BAR_TEXT_Y_MM), BAR_TEXT_MM, VAlign::Baseline, "0"));
    kinds.push(block_text(p(BAR_FROM_MM + on_paper, BAR_TEXT_Y_MM), BAR_TEXT_MM, VAlign::Baseline, &bar_label(length)));
    if let Some(deg) = north {
        // `along(deg, r)`: `r` from the arrow's centre, `deg` clockwise of up.
        let along = |deg: f64, r: f64| {
            let (sin, cos) = deg.to_radians().sin_cos();
            (r * sin, r * cos)
        };
        let deg = f64::from(deg);
        let (tx, ty) = along(deg, ARROW_R_MM);
        let tip = p(ARROW_X_MM + tx, ty);
        kinds.push(EntityKind::Circle(Circle { center: p(ARROW_X_MM, 0.0), radius: ARROW_R_MM }));
        kinds.push(line(p(ARROW_X_MM - tx, -ty), tip));
        for side in [-ARROW_HEAD_DEG, ARROW_HEAD_DEG] {
            let (bx, by) = along(deg + side, ARROW_HEAD_MM);
            kinds.push(line(tip, p(tip.x - bx, tip.y - by)));
        }
        let (nx, ny) = along(deg, ARROW_N_AT_MM);
        kinds.push(block_text(p(ARROW_X_MM + nx, ny), ARROW_N_MM, VAlign::Middle, "N"));
    }
    let space = Space::Block(name.into());
    for kind in kinds {
        d.add(&space, Common { layer: "0".into(), ..Common::default() }, kind).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Every layout's changes that make its titles equal what its single-view viewports want.
fn plan(d: &Drawing) -> Result<Vec<LayoutPlan>, String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    let blocks: Vec<(Id, String)> = doc.views.iter().map(|v| (v.id(), view_block_name(v.id()))).collect();
    let numbered = numbered_viewports(d, &blocks);
    let north_deg = doc.sheet_info.as_ref().map_or(0.0, |i| i.north_deg);
    let view_of = |view: Id| doc.views.iter().find(|v| v.id() == view);
    let mut out = Vec::new();
    for l in &d.layouts {
        let mut wanted: Vec<Wanted> = Vec::new();
        let view_name = |view: Id| view_of(view).map(|v| v.name().to_owned());
        let title = |vp: &Viewport, num: usize, name: String, view: Id| {
            let tenths = scale_tenths(viewport_scale(vp));
            let north = view_of(view).and_then(|v| plan_north(north_deg, v));
            Wanted {
                at: Vec3::new(vp.center.x - vp.width / 2.0, vp.center.y - vp.height / 2.0 - TITLE_DROP_MM, 0.0),
                vp: vp.id.to_string(),
                num: num.to_string(),
                name,
                scale: scale_label(viewport_scale(vp)),
                block: block_name(tenths, north),
                n: tenths as f64 / 10.0,
                north,
            }
        };
        for (view, n, _, h) in numbered.iter().filter(|(_, _, ln, _)| *ln == l.name) {
            let Some(EntityKind::Viewport(vp)) = l.entities.get(*h).map(|e| &e.kind) else { continue };
            wanted.push(title(vp, *n, view_name(*view).unwrap_or_default(), *view));
        }
        // Assembly viewports number on from the single-view ones, by viewport id.
        let mut assemblies: Vec<(&Viewport, String, Id)> = l
            .entities
            .iter()
            .filter_map(|e| {
                let EntityKind::Viewport(vp) = &e.kind else { return None };
                let data = read_entity_data(&e.common).ok()?;
                if data.view_id.is_some() {
                    return None;
                }
                let plan = data.assembly_of?;
                Some((vp, format!("{} framing", view_name(plan)?), plan))
            })
            .collect();
        assemblies.sort_by_key(|(vp, ..)| vp.id);
        let singles = wanted.len();
        for (i, (vp, name, plan)) in assemblies.into_iter().enumerate() {
            wanted.push(title(vp, singles + i + 1, name, plan));
        }
        let mut lp = LayoutPlan { layout: l.name.clone(), ..LayoutPlan::default() };
        let mut claimed: Vec<Handle> = Vec::new();
        for w in wanted {
            let mine =
                l.entities.iter().find(|e| !claimed.contains(&e.handle) && title_of(e).is_some_and(|i| attrib_value(i, "VP") == Some(w.vp.as_str())));
            match mine {
                Some(e) => {
                    claimed.push(e.handle);
                    if let Some(i) = title_of(e)
                        && !matches(e, i, &w)
                    {
                        lp.update.push((e.handle, w));
                    }
                }
                None => lp.add.push(w),
            }
        }
        lp.remove = l.entities.iter().filter(|e| title_of(e).is_some() && !claimed.contains(&e.handle)).map(|e| e.handle).collect();
        if !lp.is_empty() {
            out.push(lp);
        }
    }
    Ok(out)
}

fn apply(d: &mut Drawing, plan: &[LayoutPlan]) -> Result<(), String> {
    if plan.iter().any(|p| !p.add.is_empty() || !p.update.is_empty()) {
        for w in plan.iter().flat_map(|p| p.add.iter().chain(p.update.iter().map(|u| &u.1))) {
            ensure_block(d, &w.block, w.n, w.north)?;
        }
        if d.ensure_layer(TITLE_LAYER)
            && let Some(l) = d.layer_mut(TITLE_LAYER)
        {
            l.plot = true;
            l.color = Color::Index(7);
        }
    }
    for lp in plan {
        let space = Space::Paper(lp.layout.clone());
        let store = d.space_mut(&space).ok_or("layout gone")?;
        for h in &lp.remove {
            store.remove(*h);
        }
        for (h, w) in &lp.update {
            store.modify(*h, |e| {
                e.common.layer = TITLE_LAYER.into();
                e.kind = EntityKind::Insert(insert_of(w));
            });
        }
        for w in &lp.add {
            let common = Common { layer: TITLE_LAYER.into(), ..Common::default() };
            d.add(&space, common, EntityKind::Insert(insert_of(w))).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// The after-command observer: every layout's title inserts made to equal its single-view
/// viewports' wanted titles. The drawing is written back only when something changed.
pub fn vptitle_observer(s: &mut Session, _before: &Arc<Drawing>) {
    let todo = match s.doc().map_err(|e| e.to_string()).and_then(plan) {
        Ok(p) if p.is_empty() => return,
        Ok(p) => p,
        Err(e) => {
            s.echo(format!("LAMB titles: {e}"));
            return;
        }
    };
    let Ok(current) = s.doc() else { return };
    let mut after = current.clone();
    if let Err(e) = apply(&mut after, &todo) {
        s.echo(format!("LAMB titles: {e}"));
        return;
    }
    if let Ok(d) = s.doc_mut() {
        *d = after;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assembly::tests::{room_with, undo_len};
    use cadcraft_doc::{Layout, PageSetup};
    use serde_json::json;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    /// assembly.rs's room (plan `Ground`, detail `D1`) with the observers installed.
    fn room() -> T<(Session, Id, Id)> {
        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        crate::maintain::install(&mut s);
        let doc = read_document(s.doc()?)?;
        let detail = doc.views.iter().find(|v| v.id() != plan).ok_or("no detail")?.id();
        Ok((s, plan, detail))
    }

    /// Layout `name`'s titles: (handle, insert point, layer, NUM, NAME, SCALE, VP).
    #[allow(clippy::type_complexity)]
    fn titles(s: &Session, name: &str) -> T<Vec<(Handle, (f64, f64), String, String, String, String, String)>> {
        let l = s.doc()?.layout(name).ok_or("no layout")?;
        Ok(l.entities
            .iter()
            .filter_map(|e| {
                let i = title_of(e)?;
                let v = |t| attrib_value(i, t).unwrap_or("-").to_owned();
                Some((e.handle, (i.insert.x, i.insert.y), e.common.layer.clone(), v("NUM"), v("NAME"), v("SCALE"), v("VP")))
            })
            .collect())
    }

    fn viewport(s: &Session, h: Handle) -> T<Viewport> {
        match &s.doc()?.entity(h).ok_or("no viewport")?.kind {
            EntityKind::Viewport(vp) => Ok(vp.clone()),
            _ => Err("not a viewport".into()),
        }
    }

    fn corner(vp: &Viewport) -> (f64, f64) {
        (vp.center.x - vp.width / 2.0, vp.center.y - vp.height / 2.0 - TITLE_DROP_MM)
    }

    fn hx(v: &serde_json::Value) -> T<Handle> {
        Ok(Handle::parse_hex(v.as_str().ok_or("no handle")?).ok_or("bad handle")?)
    }

    #[test]
    fn newsheet_titles_each_view_in_its_own_undo_step() -> R {
        let (mut s, plan, detail) = room()?;
        let n = undo_len(&s)?;
        let out = s.execute("newsheet", &json!({ "name": "A501", "viewIds": [plan.0, detail.0] }))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        let (vp1, vp2) = (viewport(&s, hx(&out["viewports"][0])?)?, viewport(&s, hx(&out["viewports"][1])?)?);
        let t = titles(&s, "A501")?;
        assert_eq!(t.len(), 2, "{t:?}");
        // The 1 m detail plots at 1:5 in a half-width cell (T625's rule); the title reads the viewport.
        let rows: Vec<_> = t.iter().map(|t| (t.1, t.2.as_str(), t.3.as_str(), t.4.as_str(), t.5.as_str(), t.6.clone())).collect();
        assert_eq!(
            rows,
            [
                (corner(&vp1), TITLE_LAYER, "1", "Ground", "1:50", vp1.id.to_string()),
                (corner(&vp2), TITLE_LAYER, "2", "D1", "1:5", vp2.id.to_string()),
            ]
        );
        let def = s.doc()?.block("LAMB_VTITLE_50_N0").ok_or("no block definition")?;
        assert_eq!(def.entities.len(), 15, "circle, underline, bar, five ticks, two texts, and the arrow's five");
        let layer = s.doc()?.layer(TITLE_LAYER).ok_or("no layer")?;
        assert!(layer.plot && layer.color == Color::Index(7));
        let EntityKind::Insert(i) = &s.doc()?.entity(t[0].0).ok_or("gone")?.kind else { return Err("not an insert".into()) };
        assert!(i.attribs.iter().all(|a| a.modes.lock_position));
        assert_eq!(i.attribs.iter().map(|a| a.invisible).collect::<Vec<_>>(), [false, false, false, true]);

        // An unrelated command changes nothing.
        let before = titles(&s, "A501")?;
        s.execute("line", &json!({ "points": [[9000, 9000], [9500, 9000]] }))?;
        assert_eq!(titles(&s, "A501")?, before);

        s.undo()?;
        s.undo()?;
        assert!(s.doc()?.layout("A501").is_none());
        s.redo()?;
        assert_eq!(titles(&s, "A501")?.len(), 2);
        Ok(())
    }

    #[test]
    fn titles_follow_move_erase_and_rename() -> R {
        let (mut s, plan, detail) = room()?;
        let out = s.execute("newsheet", &json!({ "name": "A501", "viewIds": [plan.0, detail.0] }))?;
        let h1 = hx(&out["viewports"][0])?;
        let t0 = titles(&s, "A501")?;
        s.execute("move", &json!({ "handles": [h1.hex()], "delta": [10, -5] }))?;
        let vp1 = viewport(&s, h1)?;
        let t = titles(&s, "A501")?;
        let first = t.iter().find(|t| t.6 == vp1.id.to_string()).ok_or("no title")?;
        assert_eq!((first.0, first.1), (t0[0].0, corner(&vp1)), "same handle, under the moved viewport");

        // A title moved by hand snaps back.
        s.execute("move", &json!({ "handles": [first.0.hex()], "delta": [40, 40] }))?;
        assert_eq!(titles(&s, "A501")?.iter().find(|t| t.0 == first.0).ok_or("gone")?.1, corner(&vp1));

        s.execute("viewprops", &json!({ "mode": "rename", "viewId": detail.0, "name": "Eaves" }))?;
        assert!(titles(&s, "A501")?.iter().any(|t| t.3 == "2" && t.4 == "Eaves"));

        s.execute("erase", &json!({ "handles": [h1.hex()] }))?;
        let t = titles(&s, "A501")?;
        assert_eq!(t.iter().map(|t| (t.3.as_str(), t.4.as_str())).collect::<Vec<_>>(), [("1", "Eaves")], "renumbered, plan title gone");
        Ok(())
    }

    #[test]
    fn assembly_viewports_number_after_single_view_ones_and_leave_sheet_refs_alone() -> R {
        let (mut s, plan, detail) = room()?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let out = s.execute("newsheet", &json!({ "name": "A1", "assemblyIds": [plan.0] }))?;
        let vp = viewport(&s, hx(&out["viewports"][0])?)?;
        let t = titles(&s, "A1")?;
        let rows: Vec<_> = t.iter().map(|t| (t.1, t.3.as_str(), t.4.as_str(), t.5.clone(), t.6.clone())).collect();
        assert_eq!(rows, [(corner(&vp), "1", "Ground framing", scale_label(viewport_scale(&vp)), vp.id.to_string())]);
        assert_eq!(crate::sheets::view_sheet_ref(s.doc()?, plan), None, "no number a bubble could name");

        // On a sheet with views the assembly comes last.
        s.execute("newsheet", &json!({ "name": "A2", "viewIds": [plan.0, detail.0], "assemblyIds": [plan.0] }))?;
        let t = titles(&s, "A2")?;
        assert_eq!(t.iter().map(|t| (t.3.as_str(), t.4.as_str())).collect::<Vec<_>>(), [("1", "Ground"), ("2", "D1"), ("3", "Ground framing")]);
        assert_eq!(crate::sheets::view_sheet_ref(s.doc()?, detail), Some((2, "A2".to_owned())));

        // The title follows a rename, with the same handle.
        let h = t[2].0;
        s.execute("viewprops", &json!({ "mode": "rename", "viewId": plan.0, "name": "Lower" }))?;
        let t = titles(&s, "A2")?;
        assert!(t.iter().any(|t| t.0 == h && t.4 == "Lower framing"), "{t:?}");
        Ok(())
    }

    #[test]
    fn non_lamb_viewports_get_no_title() -> R {
        let (mut s, ..) = room()?;

        // A plain layout whose viewport freezes nothing shows every view: no title.
        let d = s.doc_mut()?;
        d.layouts.push(Layout { page: PageSetup::default(), ..Layout::new("Plain", 9) });
        let vp = Viewport { center: Vec3::new(100.0, 100.0, 0.0), width: 100.0, height: 80.0, view_height: 4000.0, id: 50, ..Viewport::default() };
        d.add(&Space::Paper("Plain".into()), Common::default(), EntityKind::Viewport(vp))?;
        s.execute("line", &json!({ "points": [[9000, 9000], [9500, 9000]] }))?;
        assert!(titles(&s, "Plain")?.is_empty());
        Ok(())
    }

    #[test]
    fn scale_labels() {
        assert_eq!(
            (scale_label(50.0), scale_label(5.0), scale_label(12.34), scale_label(20.0000001)),
            ("1:50".into(), "1:5".into(), "1:12.3".into(), "1:20".into())
        );
    }

    #[test]
    fn a_dxf_round_trip_keeps_the_titles() -> R {
        let (mut s, plan, detail) = room()?;
        s.execute("newsheet", &json!({ "name": "A501", "viewIds": [plan.0, detail.0] }))?;
        let bytes = cadcraft_io::write(s.doc()?, "t.dxf").map_err(|e| format!("{e:?}"))?;
        let back = cadcraft_io::read(&bytes, "t.dxf").map_err(|e| format!("{e:?}"))?;
        let p = super::plan(&back)?;
        assert!(p.is_empty(), "the observer would change nothing after a round trip: {p:?}");
        Ok(())
    }

    /// A block's lines and texts, each coordinate to 0.001 mm.
    #[allow(clippy::type_complexity)]
    fn drawn(s: &Session, block: &str) -> T<(Vec<((f64, f64), (f64, f64))>, Vec<(String, (f64, f64), f64)>, Vec<(f64, f64, f64)>)> {
        let r = |v: f64| (v * 1000.0).round() / 1000.0;
        let (mut lines, mut texts, mut circles) = (Vec::new(), Vec::new(), Vec::new());
        for e in s.doc()?.block(block).ok_or_else(|| format!("no block {block}"))?.entities.iter() {
            assert_eq!(e.common.layer, "0");
            match &e.kind {
                EntityKind::Line(l) => lines.push(((r(l.a.x), r(l.a.y)), (r(l.b.x), r(l.b.y)))),
                EntityKind::Text(t) => {
                    let p = t.align_pt.unwrap_or(t.insert);
                    texts.push((t.value.clone(), (r(p.x), r(p.y)), t.height));
                }
                EntityKind::Circle(c) => circles.push((r(c.center.x), r(c.center.y), c.radius)),
                other => return Err(format!("unexpected {other:?}").into()),
            }
        }
        Ok((lines, texts, circles))
    }

    /// Layout `name`'s title blocks, by NUM order.
    fn blocks_of(s: &Session, name: &str) -> T<Vec<String>> {
        let l = s.doc()?.layout(name).ok_or("no layout")?;
        Ok(l.entities.iter().filter_map(|e| Some(title_of(e)?.block.clone())).collect())
    }

    /// The arrow's direction from a block's shaft, (x, y) of unit length.
    fn arrow(s: &Session, block: &str) -> T<(f64, f64)> {
        let (lines, ..) = drawn(s, block)?;
        // The underline, bar and ticks all come first; the shaft is the first line past them.
        let (a, b) = lines.get(7).ok_or("no shaft")?;
        assert!(((a.0 + b.0) / 2.0 - ARROW_X_MM).abs() < 1e-3 && ((a.1 + b.1) / 2.0).abs() < 1e-3, "through the centre: {a:?} {b:?}");
        assert!((((b.0 - a.0).hypot(b.1 - a.1)) - 8.0).abs() < 1e-2, "from -4 to +4");
        Ok(((b.0 - a.0) / 8.0, (b.1 - a.1) / 8.0))
    }

    #[test]
    fn bar_lengths_and_block_names() {
        let rows = [
            (50.0, 2000.0, "2 m"),
            (100.0, 2000.0, "2 m"),
            (5.0, 200.0, "200 mm"),
            (20.0, 500.0, "500 mm"),
            (22.0, 500.0, "500 mm"),
            (25.0, 1000.0, "1 m"),
        ];
        for (n, l, label) in rows {
            assert_eq!((bar_length(n), bar_label(bar_length(n)).as_str()), (l, label), "1:{n}");
            assert!(l / n <= BAR_MAX_MM);
        }
        assert_eq!(bar_length(1000.0), 20000.0);
        assert_eq!(bar_label(20000.0), "20 m");
        assert_eq!(block_name(scale_tenths(50.0), None), "LAMB_VTITLE_50");
        assert_eq!(block_name(scale_tenths(33.34), Some(359)), "LAMB_VTITLE_33P3_N359");
        assert_eq!(block_name(scale_tenths(f64::NAN), None), "LAMB_VTITLE_1");
    }

    #[test]
    fn a_title_block_draws_its_scales_bar_and_a_plans_arrow() -> R {
        let (mut s, plan, detail) = room()?;
        s.execute("newsheet", &json!({ "name": "A501", "viewIds": [plan.0, detail.0] }))?;
        assert_eq!(blocks_of(&s, "A501")?, ["LAMB_VTITLE_50_N0", "LAMB_VTITLE_5"]);

        // 1:50: 2 m over 40 mm from x 30, five ticks at 10 mm.
        let (lines, texts, circles) = drawn(&s, "LAMB_VTITLE_50_N0")?;
        assert_eq!(lines[0], ((12.0, 0.0), (90.0, 0.0)), "the underline");
        assert_eq!(lines[1], ((30.0, -3.0), (70.0, -3.0)), "the bar");
        assert_eq!(lines[2..7].iter().map(|l| l.0.0).collect::<Vec<_>>(), [30.0, 40.0, 50.0, 60.0, 70.0]);
        assert!(lines[2..7].iter().all(|l| l.0.0 == l.1.0 && l.0.1 == -3.0 && l.1.1 == -1.5));
        assert_eq!(texts[0], ("0".to_owned(), (30.0, -5.5), 1.8));
        assert_eq!(texts[1], ("2 m".to_owned(), (70.0, -5.5), 1.8));
        assert_eq!(circles, [(5.0, 0.0, 5.0), (97.0, 0.0, 4.0)]);
        assert_eq!(texts[2], ("N".to_owned(), (97.0, 5.6), 2.5), "just past the tip");
        let (x, y) = arrow(&s, "LAMB_VTITLE_50_N0")?;
        assert!(x.abs() < 1e-3 && (y - 1.0).abs() < 1e-3, "up: {x} {y}");

        // 1:5: 200 mm over 40 mm, and a detail has no arrow.
        let (lines, texts, circles) = drawn(&s, "LAMB_VTITLE_5")?;
        assert_eq!(lines[1], ((30.0, -3.0), (70.0, -3.0)));
        assert_eq!(texts.iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), ["0", "200 mm"]);
        assert_eq!((lines.len(), circles.len()), (7, 1), "no arrow");

        // The plan's assembly viewport counts as the plan.
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        s.execute("newsheet", &json!({ "name": "A1", "assemblyIds": [plan.0] }))?;
        assert!(blocks_of(&s, "A1")?.iter().all(|b| b.contains("_N0")), "{:?}", blocks_of(&s, "A1")?);

        // A section has the bar and no arrow.
        let sec = s.execute(
            "newsectionline",
            &json!({ "name": "S", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 2000}, "cutTo": {"x": 4000, "y": 2000} }),
        )?;
        let sec = Id(sec["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.execute("line", &json!({ "points": [[0, 0], [4000, 0]] }))?;
        s.execute("newsheet", &json!({ "name": "A301", "viewIds": [sec.0] }))?;
        let b = blocks_of(&s, "A301")?;
        assert_eq!(b.len(), 1, "{b:?}");
        assert!(!b[0].contains("_N"), "{b:?}");
        Ok(())
    }

    #[test]
    fn a_scale_change_swaps_the_block_in_its_undo_step() -> R {
        let (mut s, plan, _) = room()?;
        let out = s.execute("newsheet", &json!({ "name": "A501", "viewIds": [plan.0] }))?;
        let h = hx(&out["viewports"][0])?;
        let handle = titles(&s, "A501")?[0].0;
        assert_eq!(blocks_of(&s, "A501")?, ["LAMB_VTITLE_50_N0"]);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("line", &json!({ "points": [[0, 500], [40000, 500]] }))?;
        let n = undo_len(&s)?;
        s.execute("vpview", &json!({ "handle": h.hex(), "viewId": plan.0 }))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!(viewport_scale(&viewport(&s, h)?), 100.0);
        assert_eq!(blocks_of(&s, "A501")?, ["LAMB_VTITLE_100_N0"]);
        let t = titles(&s, "A501")?;
        assert_eq!((t[0].0, t[0].5.as_str()), (handle, "1:100"), "same handle");
        let (lines, texts, _) = drawn(&s, "LAMB_VTITLE_100_N0")?;
        assert_eq!(lines[1], ((30.0, -3.0), (50.0, -3.0)), "2 m over 20 mm");
        assert_eq!(texts[1].0, "2 m");
        s.undo()?;
        assert_eq!(blocks_of(&s, "A501")?, ["LAMB_VTITLE_50_N0"]);
        assert_eq!(titles(&s, "A501")?[0].5, "1:50");
        Ok(())
    }

    #[test]
    fn the_arrow_follows_project_north_and_the_plans_rotation() -> R {
        let (mut s, plan, _) = room()?;
        s.execute("newsheet", &json!({ "name": "A501", "viewIds": [plan.0] }))?;
        let n = undo_len(&s)?;
        let r = s.execute("titleblock", &json!({ "north": 30 }))?;
        assert_eq!(r["north"], json!(30.0));
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!(blocks_of(&s, "A501")?, ["LAMB_VTITLE_50_N30"]);
        let (x, y) = arrow(&s, "LAMB_VTITLE_50_N30")?;
        assert!((x - 0.5).abs() < 1e-3 && (y - 0.866).abs() < 1e-3, "30 degrees clockwise of up: {x} {y}");
        s.undo()?;
        assert_eq!(blocks_of(&s, "A501")?, ["LAMB_VTITLE_50_N0"]);

        // A plan placed turned 90 degrees counter-clockwise: the global +Y is the plan's local +X,
        // so north points right on the sheet, not left.
        let turned = Id(s.execute("newplan", &json!({ "name": "Turned" }))?["viewId"].as_u64().ok_or("no id")?);
        let level = s.execute("level", &json!({ "mode": "create", "name": "L2", "floorElevationMm": 3000 }))?["levelId"].as_u64().ok_or("no id")?;
        let rotation = std::f64::consts::FRAC_PI_2;
        let t = json!({ "originX": 1000.0, "originY": 2000.0, "rotation": rotation });
        s.execute("placeview", &json!({ "viewId": turned.0, "kind": "plan", "levelId": level, "cutElevation": 1200, "transform": t }))?;
        s.execute("editview", &json!({ "viewId": turned.0 }))?;
        s.execute("line", &json!({ "points": [[0, 0], [4000, 1000]] }))?;
        s.execute("newsheet", &json!({ "name": "A102", "viewIds": [turned.0] }))?;
        assert_eq!(blocks_of(&s, "A102")?, ["LAMB_VTITLE_50_N90"]);
        let (x, y) = arrow(&s, "LAMB_VTITLE_50_N90")?;
        assert!((x - 1.0).abs() < 1e-3 && y.abs() < 1e-3, "{x} {y}");
        let (lines, ..) = drawn(&s, "LAMB_VTITLE_50_N90")?;
        assert_eq!(lines[7], ((93.0, 0.0), (101.0, 0.0)), "tail left, tip right");

        // North 30 on it: 120 degrees clockwise of up, down and to the right.
        s.execute("titleblock", &json!({ "north": 30 }))?;
        assert_eq!(blocks_of(&s, "A102")?, ["LAMB_VTITLE_50_N120"]);
        assert_eq!(blocks_of(&s, "A501")?, ["LAMB_VTITLE_50_N30"]);
        let (x, y) = arrow(&s, "LAMB_VTITLE_50_N120")?;
        assert!((x - 0.866).abs() < 1e-3 && (y + 0.5).abs() < 1e-3, "{x} {y}");
        Ok(())
    }

    #[test]
    fn an_old_drawings_titles_upgrade_with_the_same_handles() -> R {
        let (mut s, plan, detail) = room()?;
        s.execute("newsheet", &json!({ "name": "A501", "viewIds": [plan.0, detail.0] }))?;
        let before: Vec<Handle> = titles(&s, "A501")?.iter().map(|t| t.0).collect();
        let d = s.doc_mut()?;
        d.blocks.insert(TITLE_BLOCK.into(), Arc::new(Block::new(TITLE_BLOCK)));
        let store = d.space_mut(&Space::Paper("A501".into())).ok_or("no layout")?;
        for h in &before {
            store.modify(*h, |e| {
                if let EntityKind::Insert(i) = &mut e.kind {
                    i.block = TITLE_BLOCK.into();
                }
            });
        }
        assert_eq!(titles(&s, "A501")?.iter().map(|t| t.0).collect::<Vec<_>>(), before, "still titles");
        s.execute("line", &json!({ "points": [[9000, 9000], [9500, 9000]] }))?;
        assert_eq!(titles(&s, "A501")?.iter().map(|t| t.0).collect::<Vec<_>>(), before, "same handles");
        assert_eq!(blocks_of(&s, "A501")?, ["LAMB_VTITLE_50_N0", "LAMB_VTITLE_5"]);
        assert!(s.doc()?.block(TITLE_BLOCK).is_some(), "the old definition stays");
        Ok(())
    }

    #[test]
    fn the_plot_of_a_sheet_includes_the_bar() -> R {
        use cadcraft_render::{Options, build_plot};
        let (mut s, plan, _) = room()?;
        s.execute("newsheet", &json!({ "name": "A501", "viewIds": [plan.0] }))?;
        let t = titles(&s, "A501")?;
        let (x, y) = t[0].1;
        let list = build_plot(s.doc()?, &Space::Paper("A501".into()), &Options::default());
        let has = |px: f64, py: f64| list.verts.iter().any(|v| (v.x - px).hypot(v.y - py) < 1e-3);
        assert!(has(x + 30.0, y - 3.0) && has(x + 70.0, y - 3.0), "the bar's ends");
        assert!(has(x + 50.0, y - 1.5), "the middle tick's top");
        Ok(())
    }
}

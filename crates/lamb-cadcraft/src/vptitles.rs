//! Drawing titles under sheet viewports (LAMB T626): every single-view viewport on any layout —
//! the ones [`crate::sheets::sheet_refs`] numbers, so a detail tag's `<n>/<layout>` names the
//! title `n` on that layout — carries an `Insert` of the shared block [`TITLE_BLOCK`] in its
//! layout's paper space, on layer [`TITLE_LAYER`], with its bottom-left corner's insert point
//! [`TITLE_DROP_MM`] below the viewport's bottom-left corner. The block is a Ø 10 mm circle and an
//! underline; its attributes are `NUM` (the viewport's `n`, centred in the circle), `NAME` (the
//! view's name), `SCALE` (`1:<n>`, [`crate::sheets::viewport_scale`]) and the invisible `VP` (the
//! viewport's `id`, the title's identity). All have Lock position set.
//!
//! [`vptitle_observer`] reconciles every layout's titles after every command: a title whose `VP`
//! matches is updated in place (same handle) when its place or text is off — so a title moved by
//! hand snaps back under its viewport — missing ones are added and the rest erased, and the drawing
//! is written back only when something changed, so the change lands in the command's own undo
//! step. Assembly viewports (every view frozen) and non-LAMB viewports get no title.

use std::sync::Arc;

use cadcraft_doc::{Attrib, AttribModes, Block, Circle, Common, Drawing, Entity, EntityKind, HAlign, Handle, Insert, Line, Space, Text, VAlign};
use cadcraft_engine::Session;
use cadcraft_geom::Vec3;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::Id;

use crate::schema::{read_document, view_block_name};
use crate::sheets::{numbered_viewports, viewport_scale};
use cadcraft_doc::color::Color;

/// The title block's name; one definition per drawing.
pub const TITLE_BLOCK: &str = "LAMB_VTITLE";
/// The layer titles are placed on.
pub const TITLE_LAYER: &str = "LAMB-TITLES";
/// How far the title's insert point sits below its viewport's bottom edge (3 mm clear of the
/// circle's top).
pub const TITLE_DROP_MM: f64 = 8.0;
const CIRCLE_R_MM: f64 = 5.0;
const LINE_FROM_MM: f64 = 12.0;
const LINE_TO_MM: f64 = 90.0;

/// One title a viewport wants.
#[derive(Clone, Debug, PartialEq)]
struct Wanted {
    at: Vec3,
    vp: String,
    num: String,
    name: String,
    scale: String,
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
        block: TITLE_BLOCK.into(),
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

fn title_of(e: &Entity) -> Option<&Insert> {
    match &e.kind {
        EntityKind::Insert(i) if i.block.eq_ignore_ascii_case(TITLE_BLOCK) => Some(i),
        _ => None,
    }
}

/// Whether `e` is the title `w` wants, exactly.
fn matches(e: &Entity, i: &Insert, w: &Wanted) -> bool {
    e.common.layer == TITLE_LAYER
        && near(i.insert, w.at)
        && attrib_value(i, "NUM") == Some(w.num.as_str())
        && attrib_value(i, "NAME") == Some(w.name.as_str())
        && attrib_value(i, "SCALE") == Some(w.scale.as_str())
        && i.attribs.len() == 4
        && i.attribs.iter().all(|a| a.modes.lock_position)
        && i.attribs.iter().zip(insert_of(w).attribs.iter()).all(|(a, b)| a.tag == b.tag && near(pos(a), pos(b)))
}

/// The block definition: the number circle and the underline, on layer `0`.
fn ensure_block(d: &mut Drawing) -> Result<(), String> {
    if d.block(TITLE_BLOCK).is_some() {
        return Ok(());
    }
    d.blocks.insert(TITLE_BLOCK.into(), Arc::new(Block::new(TITLE_BLOCK)));
    let space = Space::Block(TITLE_BLOCK.into());
    let common = || Common { layer: "0".into(), ..Common::default() };
    d.add(&space, common(), EntityKind::Circle(Circle { center: Vec3::new(CIRCLE_R_MM, 0.0, 0.0), radius: CIRCLE_R_MM }))
        .map_err(|e| e.to_string())?;
    d.add(&space, common(), EntityKind::Line(Line { a: Vec3::new(LINE_FROM_MM, 0.0, 0.0), b: Vec3::new(LINE_TO_MM, 0.0, 0.0) }))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Every layout's changes that make its titles equal what its single-view viewports want.
fn plan(d: &Drawing) -> Result<Vec<LayoutPlan>, String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    let blocks: Vec<(Id, String)> = doc.views.iter().map(|v| (v.id(), view_block_name(v.id()))).collect();
    let numbered = numbered_viewports(d, &blocks);
    let mut out = Vec::new();
    for l in &d.layouts {
        let mut wanted: Vec<Wanted> = Vec::new();
        for (view, n, _, h) in numbered.iter().filter(|(_, _, ln, _)| *ln == l.name) {
            let Some(EntityKind::Viewport(vp)) = l.entities.get(*h).map(|e| &e.kind) else { continue };
            let name = doc.views.iter().find(|v| v.id() == *view).map_or_else(String::new, |v| v.name().to_owned());
            wanted.push(Wanted {
                at: Vec3::new(vp.center.x - vp.width / 2.0, vp.center.y - vp.height / 2.0 - TITLE_DROP_MM, 0.0),
                vp: vp.id.to_string(),
                num: n.to_string(),
                name,
                scale: scale_label(viewport_scale(vp)),
            });
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
        ensure_block(d)?;
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

/// The after-command observer: every layout's `LAMB_VTITLE` inserts made to equal its single-view
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
    use cadcraft_doc::{Layout, PageSetup, Viewport};
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
        // The 1 m detail in a half-width cell fits 1:10, not 1:5 (T625's rule); the title reads the viewport.
        let rows: Vec<_> = t.iter().map(|t| (t.1, t.2.as_str(), t.3.as_str(), t.4.as_str(), t.5.as_str(), t.6.clone())).collect();
        assert_eq!(
            rows,
            [
                (corner(&vp1), TITLE_LAYER, "1", "Ground", "1:50", vp1.id.to_string()),
                (corner(&vp2), TITLE_LAYER, "2", "D1", "1:10", vp2.id.to_string()),
            ]
        );
        let def = s.doc()?.block(TITLE_BLOCK).ok_or("no block definition")?;
        assert_eq!(def.entities.len(), 2);
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
    fn assembly_and_non_lamb_viewports_get_no_title() -> R {
        let (mut s, plan, _) = room()?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        s.execute("newsheet", &json!({ "name": "A1", "assemblyIds": [plan.0] }))?;
        assert!(titles(&s, "A1")?.is_empty());

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
}

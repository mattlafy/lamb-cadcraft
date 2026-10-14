//! Detail reference lines (LAMB's `docs/07-architecture/cadcraft-integration.md` §9 "Detail views"
//! item 2, phase 8 "Details: reference kinds", LAMB T354): a detail view declares one straight line
//! in its block as its reference, and the reference's kind — exterior face, interior face,
//! structural centre or grid — which the later sweep aligns to a run. The tag is
//! [`EntityData::detail_ref`] on the line itself (ruling T354.1), so it travels with the line
//! through CADCraft edits and DXF; DXF/DWG see a plain line.
//!
//! - `detailref {handle, kind?}` (View ▸ LAMB ▸ Detail Reference): only while editing a detail view;
//!   the handle must be a straight line in its block. Clears every other line's tag in the block,
//!   then tags this one; one undo step. `kind` defaults to `exterior_face`. Returns
//!   `{handle: <hex>, kind}`. `detailref {}` reads the current one back (`{handle, kind}` or
//!   `null`) and changes nothing.
//! - Interactive: `Select reference line:` (Enter echoes the current one) → `Reference kind
//!   or [Exterior/Interior/Centre/Grid] <Exterior>:`.
//! - [`detail_reference`] reads it back with its endpoints, refusing two tagged lines (COPY clones
//!   the tag) or a tagged line that is no longer straight — never picking one silently.
//! - [`draw`]: the overlay redraws the tagged line magenta, at least 0.5 mm, in a detail's block.

use cadcraft_color::Rgb;
use cadcraft_doc::{Drawing, Entity, Handle, Space};
use cadcraft_engine::cmd::curves::pick_at;
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_render::{DisplayList, Options};
use lamb_core::geometry::Point2;
use lamb_core::model::{AnyView, Id};
use serde_json::{Value, json};

use crate::commands::{bad, edited_view, other};
use crate::convert::to_polyline;
use crate::overlays::view_id;
use crate::schema::{read_document, read_entity_data, set_entity_data, view_block_name};

pub use crate::schema::DetailRefKind;

/// The reference line's colour in the overlay.
pub const REFERENCE_COLOR: Rgb = Rgb(230, 80, 200);
/// The reference line's minimum lineweight (mm) in the overlay.
const REFERENCE_MIN_LW: f32 = 0.5;

/// A detail view's reference line: the tagged entity, its kind, and its endpoints in the detail's
/// local coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetailRef {
    pub handle: Handle,
    pub kind: DetailRefKind,
    pub from: Point2,
    pub to: Point2,
}

/// `(keyword, kind, JSON name)`, in prompt order.
const KINDS: [(&str, DetailRefKind, &str); 4] = [
    ("Exterior", DetailRefKind::ExteriorFace, "exterior_face"),
    ("Interior", DetailRefKind::InteriorFace, "interior_face"),
    ("Centre", DetailRefKind::StructuralCentre, "structural_centre"),
    ("Grid", DetailRefKind::Grid, "grid"),
];

fn kind_name(k: DetailRefKind) -> &'static str {
    KINDS.iter().find(|(_, x, _)| *x == k).map_or("exterior_face", |(_, _, n)| n)
}

/// A kind typed or passed: its JSON name, its keyword, or the keyword's first letter (any case).
fn parse_kind(t: &str) -> Option<DetailRefKind> {
    let t = t.trim();
    KINDS
        .iter()
        .find(|(w, _, n)| t.eq_ignore_ascii_case(n) || t.eq_ignore_ascii_case(w) || (t.len() == 1 && w[..1].eq_ignore_ascii_case(t)))
        .map(|(_, k, _)| *k)
}

/// The entity's one straight segment, or `None` (an arc, a bent polyline, text, ...).
fn straight(e: &Entity) -> Option<(Point2, Point2)> {
    let poly = to_polyline(e).ok()?.polyline;
    match poly.segments.as_slice() {
        [seg] if !poly.closed && seg.bulge == 0.0 => Some((poly.start, seg.to)),
        _ => None,
    }
}

fn is_detail(d: &Drawing, id: Id) -> std::result::Result<Option<String>, String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    Ok(doc.views.iter().find(|v| v.id() == id && matches!(v, AnyView::Detail(_))).map(|v| v.name().to_owned()))
}

/// Every entity in `detail`'s block tagged as a reference: (entity, kind).
fn tagged(d: &Drawing, detail: Id) -> std::result::Result<Vec<(&Entity, DetailRefKind)>, String> {
    let Some(block) = d.block(&view_block_name(detail)) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for e in block.entities.iter() {
        if let Some(k) = read_entity_data(&e.common).map_err(|e| e.to_string())?.detail_ref {
            out.push((&**e, k));
        }
    }
    Ok(out)
}

/// `detail`'s reference line, or `None` when none is tagged. Two or more tagged lines, or a tagged
/// entity that is no longer one straight segment, is an error naming it.
pub fn detail_reference(d: &Drawing, detail: Id) -> std::result::Result<Option<DetailRef>, String> {
    let name = is_detail(d, detail)?.ok_or("No such detail view")?;
    let refs = tagged(d, detail)?;
    match refs.as_slice() {
        [] => Ok(None),
        [(e, kind)] => {
            let (from, to) = straight(e).ok_or_else(|| format!("{name}'s reference line {:X} is no longer one straight segment", e.handle.0))?;
            Ok(Some(DetailRef { handle: e.handle, kind: *kind, from, to }))
        }
        many => Err(format!("{name} has {} reference lines", many.len())),
    }
}

/// The edited view, if it is a detail view; else the refusal.
pub(crate) fn edited_detail(s: &Session) -> std::result::Result<Id, String> {
    let refuse = || "Edit a detail view first (EDITVIEW).".to_owned();
    let view = edited_view(s).ok_or_else(refuse)?;
    is_detail(s.doc().map_err(|e| e.to_string())?, view)?.ok_or_else(refuse)?;
    Ok(view)
}

fn ref_json(r: Option<DetailRef>) -> Value {
    r.map_or(Value::Null, |r| json!({ "handle": format!("{:X}", r.handle.0), "kind": kind_name(r.kind) }))
}

/// DETAILREF: tag `handle` as the edited detail's reference line; with no `handle`, read it back.
pub(crate) fn detailref(s: &mut Session, p: &Value) -> Result<Value> {
    let detail = edited_detail(s).map_err(other)?;
    let Some(hv) = p.get("handle").filter(|v| !v.is_null()) else {
        return Ok(ref_json(detail_reference(s.doc()?, detail).map_err(other)?));
    };
    let h = hv
        .as_str()
        .and_then(Handle::parse_hex)
        .or_else(|| hv.as_u64().map(Handle))
        .ok_or_else(|| bad("detailref", "`handle` must be a hex handle string or a number"))?;
    let kind = match p.get("kind").filter(|v| !v.is_null()) {
        None => DetailRefKind::ExteriorFace,
        Some(v) => v
            .as_str()
            .and_then(parse_kind)
            .ok_or_else(|| bad("detailref", "`kind` must be exterior_face, interior_face, structural_centre or grid"))?,
    };
    // Every refusal before `doc_mut`, so a refusal records nothing.
    let others: Vec<Handle> = {
        let d = s.doc()?;
        let block = d.block(&view_block_name(detail)).ok_or_else(|| other("Not in the edited detail view"))?;
        let e = block.entities.iter().find(|e| e.handle == h).ok_or_else(|| other("Not in the edited detail view"))?;
        if straight(e).is_none() {
            return Err(other("Reference line must be one straight segment"));
        }
        tagged(d, detail).map_err(other)?.iter().map(|(e, _)| e.handle).filter(|x| *x != h).collect()
    };
    let d = s.doc_mut()?;
    for (x, k) in others.iter().map(|x| (*x, None)).chain([(h, Some(kind))]) {
        let mut res = Ok(());
        d.modify_entity(x, |e| {
            res = read_entity_data(&e.common).and_then(|mut data| {
                data.detail_ref = k;
                set_entity_data(&mut e.common, &data)
            });
        })
        .map_err(other)?;
        res.map_err(other)?;
    }
    Ok(json!({ "handle": format!("{:X}", h.0), "kind": kind_name(kind) }))
}

/// The echo for the current reference: `<Reference: exterior_face, line 2A>` or `<No reference line>`.
fn current_message(s: &Session, detail: Id) -> String {
    match s.doc().map_err(|e| e.to_string()).and_then(|d| detail_reference(d, detail)) {
        Ok(Some(r)) => format!("<Reference: {}, line {:X}>", kind_name(r.kind), r.handle.0),
        Ok(None) => "<No reference line>".to_owned(),
        Err(e) => e,
    }
}

/// DETAILREF: pick the line, then its kind.
struct DetailRefM {
    line: Option<Handle>,
}

impl Interactive for DetailRefM {
    fn name(&self) -> &'static str {
        "detailref"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.line {
            None => Prompt::new("Select reference line", Accept::POINT),
            Some(_) => Prompt::new("Reference kind", Accept::TEXT).kw(&["Exterior", "Interior", "Centre", "Grid"]).default("Exterior".to_owned()),
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if let Err(e) = edited_detail(s) {
            s.echo(e);
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(h) = self.line else {
            match i {
                Input::Enter => {
                    let detail = edited_detail(s).map_err(other)?;
                    let m = current_message(s, detail);
                    s.echo(m);
                    return Ok(Step::Done);
                }
                Input::Point(p) => match pick_at(s, p) {
                    Some(h) => self.line = Some(h),
                    None => s.echo("No line found near click"),
                },
                _ => {}
            }
            return Ok(Step::Continue);
        };
        let kind = match i {
            Input::Enter => DetailRefKind::ExteriorFace,
            Input::Text(t) | Input::Keyword(t) => match parse_kind(&t) {
                Some(k) => k,
                None => {
                    s.echo("Exterior, Interior, Centre or Grid");
                    return Ok(Step::Continue);
                }
            },
            _ => return Ok(Step::Continue),
        };
        match detailref(s, &json!({ "handle": format!("{:X}", h.0), "kind": kind_name(kind) })) {
            Ok(_) => {
                let m = format!("<Reference: {}, line {:X}>", kind_name(kind), h.0);
                s.echo(m);
            }
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

pub(crate) fn detailref_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(DetailRefM { line: None }))
}

/// The overlay: in a detail view's block, every tagged reference line redrawn in
/// [`REFERENCE_COLOR`], at least [`REFERENCE_MIN_LW`] wide. Empty anywhere else.
pub(crate) fn draw(d: &Drawing, space: &Space, opts: &Options) -> DisplayList {
    let empty = DisplayList { bounds: cadcraft_geom::Bounds2::EMPTY, ..DisplayList::default() };
    let Some(id) = view_id(space) else { return empty };
    if !matches!(is_detail(d, id), Ok(Some(_))) {
        return empty;
    }
    let Ok(refs) = tagged(d, id) else { return empty };
    if refs.is_empty() {
        return empty;
    }
    let opts = Options { view: None, ..opts.clone() };
    let mut list = cadcraft_render::build_entities(d, refs.iter().map(|(e, _)| *e), &opts);
    for p in &mut list.prims {
        p.color = REFERENCE_COLOR;
        p.aci7 = false;
        p.lw = p.lw.max(REFERENCE_MIN_LW);
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_doc::{Arc, EntityKind};
    use cadcraft_geom::{Vec2, Vec3};
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

    fn hex(h: Handle) -> String {
        format!("{:X}", h.0)
    }

    /// A plan `Ground` and a detail `D1` with lines A (0,0)-(1000,0) and B (0,500)-(1000,500),
    /// editing D1. Returns (session, plan, detail, A, B).
    fn detail() -> T<(Session, Id, Id, Handle, Handle)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let detail = Id(s.execute("newdetail", &json!({ "name": "D1" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        let a = line(&mut s, [0.0, 0.0], [1000.0, 0.0])?;
        let b = line(&mut s, [0.0, 500.0], [1000.0, 500.0])?;
        Ok((s, plan, detail, a, b))
    }

    fn tag_of(s: &Session, h: Handle) -> T<Option<DetailRefKind>> {
        Ok(read_entity_data(&s.doc()?.entity(h).ok_or("no entity")?.common)?.detail_ref)
    }

    #[test]
    fn retagging_moves_the_reference_and_undo_restores_it() -> R {
        let (mut s, _plan, detail, a, b) = detail()?;
        assert_eq!(detail_reference(s.doc()?, detail)?, None);
        assert_eq!(s.execute("detailref", &json!({}))?, Value::Null);

        let r = s.execute("detailref", &json!({ "handle": hex(a), "kind": "interior_face" }))?;
        assert_eq!(r, json!({ "handle": hex(a), "kind": "interior_face" }));
        let got = detail_reference(s.doc()?, detail)?.ok_or("no reference")?;
        assert_eq!((got.handle, got.kind), (a, DetailRefKind::InteriorFace));
        assert_eq!((got.from, got.to), (Point2 { x: 0.0, y: 0.0 }, Point2 { x: 1000.0, y: 0.0 }));

        s.execute("detailref", &json!({ "handle": hex(b) }))?;
        let got = detail_reference(s.doc()?, detail)?.ok_or("no reference")?;
        assert_eq!((got.handle, got.kind), (b, DetailRefKind::ExteriorFace));
        assert_eq!(tag_of(&s, a)?, None, "A's tag is cleared");
        assert_eq!(s.execute("detailref", &json!({}))?, json!({ "handle": hex(b), "kind": "exterior_face" }));

        s.undo()?;
        let got = detail_reference(s.doc()?, detail)?.ok_or("no reference")?;
        assert_eq!((got.handle, got.kind), (a, DetailRefKind::InteriorFace), "one undo step");
        assert_eq!(tag_of(&s, b)?, None);
        Ok(())
    }

    #[test]
    fn refuses_an_arc_another_views_line_and_a_plan() -> R {
        let (mut s, plan, detail, a, _b) = detail()?;
        let space = s.space();
        let common = s.current_common();
        let arc = s.doc_mut()?.add(&space, common, EntityKind::Arc(Arc { center: Vec3::new(0.0, 0.0, 0.0), radius: 300.0, start: 0.0, end: 1.0 }))?;
        let undo = s.state()?.undo.len();
        let e = s.execute("detailref", &json!({ "handle": hex(arc) })).err().ok_or("accepted an arc")?;
        assert!(e.to_string().contains("Reference line must be one straight segment"), "{e}");

        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let p = line(&mut s, [0.0, 0.0], [10.0, 0.0])?;
        let e = s.execute("detailref", &json!({ "handle": hex(a) })).err().ok_or("accepted in a plan")?;
        assert!(e.to_string().contains("Edit a detail view first (EDITVIEW)."), "{e}");
        s.start("detailref")?;
        assert!(s.running.is_none(), "refused at once");
        assert!(s.log.iter().any(|l| l == "Edit a detail view first (EDITVIEW)."), "{:?}", s.log);

        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        let undo2 = s.state()?.undo.len();
        let e = s.execute("detailref", &json!({ "handle": hex(p) })).err().ok_or("accepted another view's line")?;
        assert!(e.to_string().contains("Not in the edited detail view"), "{e}");
        assert_eq!(s.state()?.undo.len(), undo2, "a refusal records no step");
        assert!(undo2 > undo);
        assert_eq!(detail_reference(s.doc()?, detail)?, None);
        Ok(())
    }

    #[test]
    fn a_copied_reference_line_is_flagged_and_a_bent_one_too() -> R {
        let (mut s, _plan, detail, a, _b) = detail()?;
        s.execute("detailref", &json!({ "handle": hex(a) }))?;
        s.execute("copy", &json!({ "handles": [hex(a)], "delta": [0, 2000] }))?;
        let e = detail_reference(s.doc()?, detail).err().ok_or("picked one of two")?;
        assert_eq!(e, "D1 has 2 reference lines");

        s.undo()?;
        s.doc_mut()?.modify_entity(a, |e| {
            e.kind = EntityKind::Arc(Arc { center: Vec3::new(500.0, 0.0, 0.0), radius: 500.0, start: 0.0, end: std::f64::consts::PI });
        })?;
        let e = detail_reference(s.doc()?, detail).err().ok_or("accepted an arc")?;
        assert!(e.contains("no longer one straight segment") && e.contains(&hex(a)), "{e}");
        Ok(())
    }

    #[test]
    fn the_overlay_draws_the_reference_magenta_in_the_detail_only() -> R {
        let (mut s, plan, detail, a, _b) = detail()?;
        let block = |id: Id| Space::Block(view_block_name(id));
        let opts = Options::default();
        let none = crate::overlays::lamb_overlay(s.doc()?, &block(detail), &opts);
        assert!(!none.prims.iter().any(|p| p.color == REFERENCE_COLOR));
        s.execute("detailref", &json!({ "handle": hex(a) }))?;
        let list = crate::overlays::lamb_overlay(s.doc()?, &block(detail), &opts);
        let prims: Vec<_> = list.prims.iter().filter(|p| p.color == REFERENCE_COLOR).collect();
        assert!(!prims.is_empty() && prims.iter().all(|p| p.handle == a && p.lw >= REFERENCE_MIN_LW), "{prims:?}");

        // A tag on a plan's line (written by hand: DETAILREF refuses there) draws nothing.
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let p = line(&mut s, [0.0, 0.0], [10.0, 0.0])?;
        let mut res = Ok(());
        s.doc_mut()?.modify_entity(p, |e| {
            res = read_entity_data(&e.common).and_then(|mut d| {
                d.detail_ref = Some(DetailRefKind::Grid);
                set_entity_data(&mut e.common, &d)
            });
        })?;
        res?;
        let list = crate::overlays::lamb_overlay(s.doc()?, &block(plan), &opts);
        assert!(!list.prims.iter().any(|p| p.color == REFERENCE_COLOR));
        Ok(())
    }

    #[test]
    fn a_dxf_round_trip_keeps_the_reference() -> R {
        let (mut s, _plan, detail, a, _b) = detail()?;
        s.execute("detailref", &json!({ "handle": hex(a), "kind": "structural_centre" }))?;
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert_eq!(detail_reference(&back, detail)?, detail_reference(s.doc()?, detail)?);
        assert_eq!(detail_reference(&back, detail)?.map(|r| r.kind), Some(DetailRefKind::StructuralCentre));
        Ok(())
    }

    #[test]
    fn the_interactive_form_picks_then_asks_the_kind() -> R {
        let (mut s, _plan, detail, a, _b) = detail()?;
        s.start("detailref")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Select reference line:"));
        s.input(Input::Point(Vec2::new(500.0, -5000.0)))?;
        assert!(s.log.iter().any(|l| l == "No line found near click"), "{:?}", s.log);
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Select reference line"));
        s.input(Input::Point(Vec2::new(500.0, 0.0)))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Reference kind or [Exterior/Interior/Centre/Grid] <Exterior>:"));
        s.input(Input::Keyword("Grid".into()))?;
        assert!(s.running.is_none());
        let got = detail_reference(s.doc()?, detail)?.ok_or("no reference")?;
        assert_eq!((got.handle, got.kind), (a, DetailRefKind::Grid));

        s.start("detailref")?;
        s.input(Input::Enter)?;
        assert!(s.log.iter().any(|l| *l == format!("<Reference: grid, line {}>", hex(a))), "{:?}", s.log);
        Ok(())
    }
}

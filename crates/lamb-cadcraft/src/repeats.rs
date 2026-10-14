//! Repeats (LAMB's `docs/07-architecture/cadcraft-integration.md` §9 "Repeats", phase 8, LAMB
//! T377): in a detail view, a linear or aligned dimension tagged as the pitch of the geometry it
//! repeats — a stud at 406.4 mm centres. The tag is [`EntityData::repeat`] on the dimension
//! (`RepeatTag { members }`, member ids being handles), so it travels with the dimension through
//! CADCraft edits and DXF; DXF/DWG see a plain dimension.
//!
//! - `repeat {}` (View ▸ LAMB ▸ Repeat): only while editing a detail view; returns
//!   `{"repeats": [...]}`. `repeat {dimension}` returns that one or `null`.
//!   `repeat {dimension, handles: [...]}` tags (replacing that dimension's members);
//!   `handles: []` untags. One undo step; every refusal changes nothing.
//! - `repeat {dimension, handles, copies: [...]}` converts two drawn copies (LAMB T386): the copies
//!   must be the members' line work shifted by the pitch (either direction, one direction for the
//!   whole set, within `TOLERANCE.coincidence_mm`); they are erased and the dimension tagged, in
//!   one undo step. The reply adds `"erased": [...]`.
//! - Each repeat reads back as `{dimension, members, pitch: [x, y], pitchLength, parallel}`;
//!   `parallel` is `null` when the detail has no reference line. A pitch not parallel to the
//!   reference is tagged anyway and flagged ("Repeat is not parallel to the reference line").
//! - Repeat members are not continuous: [`crate::assembly`] lays no band from them; it arrays them
//!   at the pitch along each run instead (LAMB T380).
//! - [`draw`], through LAMB's overlay provider, draws one ghosted copy of each repeat's members at
//!   the pitch while the detail is edited (LAMB T383). Overlay only: nothing is added to the
//!   drawing.

use std::collections::HashSet;

use cadcraft_color::Rgb;
use cadcraft_doc::{DimKind, Drawing, Entity, EntityKind, Handle, Space};
use cadcraft_engine::{Result, Session};
use cadcraft_geom::{Bounds2, Vec2};
use cadcraft_render::{DPrim, DisplayList, Kind, Options};
use lamb_core::geometry::Point2;
use lamb_core::geometry::tolerance::{TOLERANCE, deg_to_rad};
use lamb_core::model::{AnyView, Id};
use serde_json::{Value, json};

use crate::commands::{bad, other};
use crate::convert::to_polyline;
use crate::detailref::{detail_reference, edited_detail};
use crate::overlays::view_id;
use crate::schema::{RepeatTag, read_document, read_entity_data, set_entity_data, view_block_name};

/// The ghosted copy's colour.
pub(crate) const GHOST_COLOR: Rgb = Rgb(150, 150, 150);
/// The ghosted copy's transparency, percent.
const GHOST_TRANSPARENCY: u8 = 60;

/// A detail's repeat: the pitch dimension, the members still in the block, the pitch vector in
/// the detail's local coordinates, and whether it runs parallel to the reference line (`None`
/// when the detail has none).
#[derive(Clone, Debug, PartialEq)]
pub struct Repeat {
    pub dimension: Handle,
    pub members: Vec<Handle>,
    pub pitch: Point2,
    pub parallel: Option<bool>,
}

/// The pitch vector of a linear or aligned dimension, or `None` for any other entity.
fn pitch_of(e: &Entity) -> Option<Point2> {
    let EntityKind::Dimension(dim) = &e.kind else { return None };
    let (dx, dy) = (dim.p14.x - dim.p13.x, dim.p14.y - dim.p13.y);
    match dim.kind {
        DimKind::Aligned => Some(Point2 { x: dx, y: dy }),
        DimKind::Linear { rotation } => {
            let (ux, uy) = (rotation.cos(), rotation.sin());
            let along = dx * ux + dy * uy;
            Some(Point2 { x: ux * along, y: uy * along })
        }
        _ => None,
    }
}

fn length(p: Point2) -> f64 {
    p.x.hypot(p.y)
}

/// Whether `v` runs along `from → to`, either direction, within `TOLERANCE.angle_deg`.
fn is_parallel(v: Point2, from: Point2, to: Point2) -> bool {
    let w = Point2 { x: to.x - from.x, y: to.y - from.y };
    let (lv, lw) = (length(v), length(w));
    if lv <= TOLERANCE.coincidence_mm || lw <= TOLERANCE.coincidence_mm {
        return false;
    }
    (v.x * w.y - v.y * w.x).abs() / (lv * lw) <= deg_to_rad(TOLERANCE.angle_deg).sin()
}

/// Every tagged entity in `detail`'s block: (entity, tag), members unfiltered.
fn tagged(d: &Drawing, detail: Id) -> std::result::Result<Vec<(&Entity, RepeatTag)>, String> {
    let Some(block) = d.block(&view_block_name(detail)) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for e in block.entities.iter() {
        if let Some(t) = read_entity_data(&e.common).map_err(|e| e.to_string())?.repeat {
            out.push((&**e, t));
        }
    }
    Ok(out)
}

/// Every repeat in `detail`'s block, members filtered to those still in the block; a repeat with
/// no member left, or whose dimension is no longer linear or aligned, is omitted.
pub fn detail_repeats(d: &Drawing, detail: Id) -> std::result::Result<Vec<Repeat>, String> {
    let Some(block) = d.block(&view_block_name(detail)) else { return Ok(Vec::new()) };
    let present: HashSet<Handle> = block.entities.iter().map(|e| e.handle).collect();
    let reference = detail_reference(d, detail)?;
    let mut out = Vec::new();
    for (e, tag) in tagged(d, detail)? {
        let Some(pitch) = pitch_of(e) else { continue };
        let members: Vec<Handle> = tag.members.iter().map(|id| Handle(id.0)).filter(|h| present.contains(h) && *h != e.handle).collect();
        if members.is_empty() {
            continue;
        }
        let parallel = reference.map(|r| is_parallel(pitch, r.from, r.to));
        out.push(Repeat { dimension: e.handle, members, pitch, parallel });
    }
    Ok(out)
}

/// Every handle that is a member of one of `detail`'s repeats.
pub fn repeat_members(d: &Drawing, detail: Id) -> std::result::Result<HashSet<Handle>, String> {
    Ok(detail_repeats(d, detail)?.into_iter().flat_map(|r| r.members).collect())
}

fn repeat_json(r: &Repeat) -> Value {
    json!({
        "dimension": r.dimension.hex(),
        "members": r.members.iter().map(|h| h.hex()).collect::<Vec<_>>(),
        "pitch": [r.pitch.x, r.pitch.y],
        "pitchLength": length(r.pitch),
        "parallel": r.parallel,
    })
}

fn parse_handle(v: &Value) -> Option<Handle> {
    v.as_str().and_then(Handle::parse_hex).or_else(|| v.as_u64().map(Handle))
}

fn parse_handles(v: &Value, name: &str) -> Result<Vec<Handle>> {
    v.as_array()
        .ok_or_else(|| bad("repeat", &format!("`{name}` must be an array of handles")))?
        .iter()
        .map(|v| parse_handle(v).ok_or_else(|| bad("repeat", "each handle must be a hex handle string or a number")))
        .collect()
}

/// Whether each member pairs one-to-one with a distinct copy whose polyline is the member's shifted
/// by `σ·pitch`, for one `σ ∈ {+1, −1}`.
fn pair_copies(d: &Drawing, members: &[Handle], copies: &[Handle], pitch: Point2) -> std::result::Result<(), String> {
    const LINE_WORK: &str = "Only line work can be converted";
    const NO_MATCH: &str = "The copies do not match the selection";
    let poly = |h: &Handle| -> std::result::Result<lamb_core::geometry::Polyline2, String> {
        let e = d.entity(*h).ok_or(LINE_WORK)?;
        to_polyline(e).map(|c| c.polyline).map_err(|_| LINE_WORK.to_string())
    };
    let ms: Vec<_> = members.iter().map(poly).collect::<std::result::Result<_, _>>()?;
    let cs: Vec<_> = copies.iter().map(poly).collect::<std::result::Result<_, _>>()?;
    if ms.len() != cs.len() {
        return Err(NO_MATCH.into());
    }
    let tol = TOLERANCE.coincidence_mm;
    let near = |a: Point2, b: Point2, dx: f64, dy: f64| (a.x + dx - b.x).abs() <= tol && (a.y + dy - b.y).abs() <= tol;
    let shifted = |m: &lamb_core::geometry::Polyline2, c: &lamb_core::geometry::Polyline2, dx: f64, dy: f64| {
        m.closed == c.closed
            && m.segments.len() == c.segments.len()
            && near(m.start, c.start, dx, dy)
            && m.segments.iter().zip(&c.segments).all(|(a, b)| near(a.to, b.to, dx, dy) && (a.bulge - b.bulge).abs() <= 1e-9)
    };
    for sigma in [1.0, -1.0] {
        let (dx, dy) = (sigma * pitch.x, sigma * pitch.y);
        let mut used = vec![false; cs.len()];
        let all = ms.iter().all(|m| match (0..cs.len()).find(|&i| !used[i] && shifted(m, &cs[i], dx, dy)) {
            Some(i) => {
                used[i] = true;
                true
            }
            None => false,
        });
        if all {
            return Ok(());
        }
    }
    Err(NO_MATCH.into())
}

/// REPEAT: read the edited detail's repeats, or tag / untag one dimension's members.
pub(crate) fn repeat(s: &mut Session, p: &Value) -> Result<Value> {
    let detail = edited_detail(s).map_err(other)?;
    let Some(dv) = p.get("dimension").filter(|v| !v.is_null()) else {
        let all = detail_repeats(s.doc()?, detail).map_err(other)?;
        return Ok(json!({ "repeats": all.iter().map(repeat_json).collect::<Vec<_>>() }));
    };
    let dim = parse_handle(dv).ok_or_else(|| bad("repeat", "`dimension` must be a hex handle string or a number"))?;
    let find = |s: &Session| -> Result<Value> {
        let all = detail_repeats(s.doc()?, detail).map_err(other)?;
        Ok(all.iter().find(|r| r.dimension == dim).map_or(Value::Null, repeat_json))
    };
    let Some(hv) = p.get("handles").filter(|v| !v.is_null()) else { return find(s) };
    let members = parse_handles(hv, "handles")?;
    let copies = match p.get("copies").filter(|v| !v.is_null()) {
        Some(cv) => Some(parse_handles(cv, "copies")?),
        None => None,
    };
    if copies.is_some() && members.is_empty() {
        return Err(other("Nothing to repeat"));
    }

    // Every refusal before `doc_mut`, so a refusal records nothing.
    let parallel = {
        let d = s.doc()?;
        let not_here = || other("Not in the edited detail view");
        let block = d.block(&view_block_name(detail)).ok_or_else(not_here)?;
        let e = block.entities.iter().find(|e| e.handle == dim).ok_or_else(not_here)?;
        if !members.is_empty() {
            let pitch = pitch_of(e).ok_or_else(|| other("Pitch must be a linear or aligned dimension"))?;
            if length(pitch) <= TOLERANCE.coincidence_mm {
                return Err(other("The pitch has no length"));
            }
            let reference = detail_reference(d, detail).map_err(other)?;
            let taken: HashSet<Handle> = tagged(d, detail)
                .map_err(other)?
                .iter()
                .filter(|(e, _)| e.handle != dim)
                .flat_map(|(_, t)| t.members.iter().map(|id| Handle(id.0)))
                .collect();
            for m in &members {
                if !block.entities.iter().any(|e| e.handle == *m) {
                    return Err(not_here());
                }
                if *m == dim {
                    return Err(other("A repeat cannot contain its own pitch dimension"));
                }
                if reference.is_some_and(|r| r.handle == *m) {
                    return Err(other("The reference line cannot be repeated"));
                }
                if taken.contains(m) {
                    return Err(other("Already in a repeat"));
                }
            }
            if let Some(copies) = &copies {
                for c in copies {
                    if !block.entities.iter().any(|e| e.handle == *c) {
                        return Err(not_here());
                    }
                    if *c == dim {
                        return Err(other("A repeat cannot contain its own pitch dimension"));
                    }
                    if reference.is_some_and(|r| r.handle == *c) {
                        return Err(other("The reference line cannot be repeated"));
                    }
                    if members.contains(c) {
                        return Err(other("A copy cannot also be repeated"));
                    }
                    if taken.contains(c) {
                        return Err(other("Already in a repeat"));
                    }
                }
                pair_copies(d, &members, copies, pitch).map_err(other)?;
            }
            reference.map(|r| is_parallel(pitch, r.from, r.to))
        } else {
            None
        }
    };

    let tag = (!members.is_empty()).then(|| RepeatTag { members: members.iter().map(|h| Id(h.0)).collect() });
    let d = s.doc_mut()?;
    for c in copies.iter().flatten() {
        d.remove_entity(*c);
    }
    let mut res = Ok(());
    d.modify_entity(dim, |e| {
        res = read_entity_data(&e.common).and_then(|mut data| {
            data.repeat = tag;
            set_entity_data(&mut e.common, &data)
        });
    })
    .map_err(other)?;
    res.map_err(other)?;
    if parallel == Some(false) {
        s.echo("Repeat is not parallel to the reference line");
    }
    let Some(copies) = copies else { return find(s) };
    s.echo(format!("Converted {} copies into a repeat", copies.len()));
    let mut out = find(s)?;
    if let Value::Object(m) = &mut out {
        m.insert("erased".into(), json!(copies.iter().map(|h| h.hex()).collect::<Vec<_>>()));
    }
    Ok(out)
}

/// The overlay: in a detail view's block, each repeat's members drawn once more, shifted by the
/// pitch, in [`GHOST_COLOR`] and translucent. Wipeouts are skipped; an infinite line's base moves,
/// its direction does not. Empty anywhere else, and on any error.
pub(crate) fn draw(d: &Drawing, space: &Space, opts: &Options) -> DisplayList {
    let mut out = DisplayList { bounds: Bounds2::EMPTY, ..DisplayList::default() };
    let Some(id) = view_id(space) else { return out };
    let Ok(doc) = read_document(d) else { return out };
    if !doc.views.iter().any(|v| v.id() == id && matches!(v, AnyView::Detail(_))) {
        return out;
    }
    let Ok(repeats) = detail_repeats(d, id) else { return out };
    let opts = Options { view: None, ..opts.clone() };
    for r in &repeats {
        let src = cadcraft_render::build_entities(d, r.members.iter().filter_map(|h| d.entity(*h).map(|e| &**e)), &opts);
        let shift = Vec2::new(r.pitch.x, r.pitch.y);
        for prim in &src.prims {
            if prim.kind == Kind::Mask {
                continue;
            }
            let infinite = matches!(prim.kind, Kind::Infinite { .. });
            let pts: Vec<Vec2> = src.points(prim).iter().enumerate().map(|(i, p)| if infinite && i > 0 { *p } else { *p + shift }).collect();
            let buf = if prim.kind == Kind::Tris { &mut out.tris } else { &mut out.verts };
            let start = buf.len() as u32;
            buf.extend_from_slice(&pts);
            if !infinite {
                for p in &pts {
                    out.bounds.add(*p);
                }
            }
            out.prims.push(DPrim { color: GHOST_COLOR, aci7: false, transparency: GHOST_TRANSPARENCY, start, len: pts.len() as u32, ..*prim });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    fn all_handles(s: &Session) -> T<HashSet<Handle>> {
        let d = s.doc()?;
        Ok(d.model.iter().map(|e| e.handle).chain(d.blocks.values().flat_map(|b| b.entities.iter().map(|e| e.handle))).collect())
    }

    /// Runs `cmd` and returns the one entity it added.
    fn draw(s: &mut Session, cmd: &str, p: Value) -> T<Handle> {
        let before = all_handles(s)?;
        s.execute(cmd, &p)?;
        Ok(all_handles(s)?.into_iter().find(|h| !before.contains(h)).ok_or("nothing drawn")?)
    }

    fn line(s: &mut Session, a: [f64; 2], b: [f64; 2]) -> T<Handle> {
        draw(s, "line", json!({ "points": [a, b] }))
    }

    struct Fixture {
        s: Session,
        plan: Id,
        detail: Id,
        reference: Handle,
        stud: Handle,
        edge: Handle,
        pitch: Handle,
    }

    /// A plan and a detail `D1` (edited) with a reference (0,0)–(1000,0), a 38 × 90 stud rectangle,
    /// a line at x = 0 (0..90) and a `dimaligned` (0,0)→(406.4,0).
    fn fixture() -> T<Fixture> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let detail = Id(s.execute("newdetail", &json!({ "name": "D1" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        let reference = line(&mut s, [0.0, 0.0], [1000.0, 0.0])?;
        s.execute("detailref", &json!({ "handle": reference.hex() }))?;
        let stud = draw(&mut s, "pline", json!({ "vertices": [[0, 0], [38, 0], [38, 90], [0, 90]], "closed": true }))?;
        let edge = line(&mut s, [0.0, 0.0], [0.0, 90.0])?;
        let pitch = draw(&mut s, "dimaligned", json!({ "p1": [0, 0], "p2": [406.4, 0], "at": [200, -300] }))?;
        Ok(Fixture { s, plan, detail, reference, stud, edge, pitch })
    }

    fn undo_len(s: &Session) -> T<usize> {
        Ok(s.state()?.undo.len())
    }

    #[test]
    fn tagging_an_aligned_pitch_reads_back_and_survives_a_dxf_round_trip() -> R {
        let Fixture { mut s, detail, stud, edge, pitch, .. } = fixture()?;
        assert_eq!(s.execute("repeat", &json!({}))?, json!({ "repeats": [] }));
        let r = s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [stud.hex(), edge.hex()] }))?;
        assert_eq!(r["pitch"], json!([406.4, 0.0]));
        assert_eq!(r["parallel"], json!(true));
        assert_eq!(r["members"], json!([stud.hex(), edge.hex()]));
        assert!((r["pitchLength"].as_f64().ok_or("no length")? - 406.4).abs() < 1e-9);
        assert_eq!(s.execute("repeat", &json!({}))?, json!({ "repeats": [r.clone()] }));
        assert_eq!(s.execute("repeat", &json!({ "dimension": pitch.hex() }))?, r);
        assert_eq!(s.execute("repeat", &json!({ "dimension": stud.hex() }))?, Value::Null);

        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert_eq!(detail_repeats(&back, detail)?, detail_repeats(s.doc()?, detail)?);
        assert_eq!(detail_repeats(&back, detail)?.len(), 1);
        Ok(())
    }

    #[test]
    fn a_vertical_linear_pitch_is_tagged_but_flagged_not_parallel() -> R {
        let Fixture { mut s, stud, .. } = fixture()?;
        let v = draw(&mut s, "dimlinear", json!({ "p1": [0, 0], "p2": [0, 600], "at": [-300, 300] }))?;
        let r = s.execute("repeat", &json!({ "dimension": v.hex(), "handles": [stud.hex()] }))?;
        assert_eq!(r["parallel"], json!(false));
        let pitch = r["pitch"].as_array().ok_or("no pitch")?;
        assert!(pitch[0].as_f64().ok_or("x")?.abs() < 1e-9 && (pitch[1].as_f64().ok_or("y")? - 600.0).abs() < 1e-9, "{pitch:?}");
        assert!(s.log.iter().any(|l| l == "Repeat is not parallel to the reference line"), "{:?}", s.log);
        Ok(())
    }

    #[test]
    fn refusals_leave_the_drawing_unchanged() -> R {
        let Fixture { mut s, plan, detail, reference, stud, edge, pitch } = fixture()?;
        s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [stud.hex()] }))?;
        let ang = draw(&mut s, "dimangular", json!({ "lines": [reference.hex(), edge.hex()], "at": [30, 30] }))?;
        let second = draw(&mut s, "dimaligned", json!({ "p1": [0, 0], "p2": [600, 0], "at": [300, -600] }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let outside = line(&mut s, [0.0, 0.0], [10.0, 0.0])?;
        let e = s.execute("repeat", &json!({})).err().ok_or("accepted in a plan")?;
        assert!(e.to_string().contains("Edit a detail view first"), "{e}");
        s.execute("editview", &json!({ "viewId": detail.0 }))?;

        let before = detail_repeats(s.doc()?, detail)?;
        let undo = undo_len(&s)?;
        let cases = [
            (ang, edge, "Pitch must be a linear or aligned dimension"),
            (second, outside, "Not in the edited detail view"),
            (second, reference, "The reference line cannot be repeated"),
            (second, stud, "Already in a repeat"),
            (second, second, "A repeat cannot contain its own pitch dimension"),
            (outside, edge, "Not in the edited detail view"),
        ];
        for (dim, member, want) in cases {
            let e = s.execute("repeat", &json!({ "dimension": dim.hex(), "handles": [member.hex()] })).err().ok_or(want)?;
            assert!(e.to_string().contains(want), "{e} / {want}");
            assert_eq!(undo_len(&s)?, undo, "{want}: no undo step");
            assert_eq!(detail_repeats(s.doc()?, detail)?, before, "{want}: unchanged");
        }
        Ok(())
    }

    #[test]
    fn empty_handles_untag_and_undo_restores() -> R {
        let Fixture { mut s, detail, stud, pitch, .. } = fixture()?;
        s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [stud.hex()] }))?;
        assert_eq!(s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [] }))?, Value::Null);
        assert!(detail_repeats(s.doc()?, detail)?.is_empty());
        let data = read_entity_data(&s.doc()?.entity(pitch).ok_or("no dim")?.common)?;
        assert_eq!(data.repeat, None);
        s.undo()?;
        assert_eq!(detail_repeats(s.doc()?, detail)?.iter().map(|r| r.members.clone()).collect::<Vec<_>>(), [vec![stud]]);
        Ok(())
    }

    #[test]
    fn erasing_every_member_omits_the_repeat() -> R {
        let Fixture { mut s, detail, stud, edge, pitch, .. } = fixture()?;
        s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [stud.hex(), edge.hex()] }))?;
        s.execute("erase", &json!({ "handles": [stud.hex()] }))?;
        assert_eq!(detail_repeats(s.doc()?, detail)?.iter().map(|r| r.members.clone()).collect::<Vec<_>>(), [vec![edge]]);
        s.execute("erase", &json!({ "handles": [edge.hex()] }))?;
        assert!(detail_repeats(s.doc()?, detail)?.is_empty());
        assert_eq!(s.execute("repeat", &json!({}))?, json!({ "repeats": [] }));
        Ok(())
    }

    #[test]
    fn a_repeated_line_parallel_to_the_reference_lays_no_band() -> R {
        let (mut s, plan) = crate::assembly::tests::room_with("exterior_face", true, 200.0)?;
        let detail = read_document(s.doc()?)?.views.iter().find(|v| v.name() == "D1").ok_or("no D1")?.id();
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        let stud = line(&mut s, [0.0, 100.0], [1000.0, 100.0])?;
        let pitch = draw(&mut s, "dimaligned", json!({ "p1": [0, 0], "p2": [406.4, 0], "at": [200, -300] }))?;
        // Bands span the whole run; the member's own copies (laid by the repeat) are 1000 long.
        let bands = |s: &Session| -> T<Vec<usize>> {
            let runs = crate::assembly::assembly_lines(s.doc()?, plan)?;
            Ok(runs.iter().map(|r| r.lines.iter().filter(|l| (l.to.x - l.from.x).hypot(l.to.y - l.from.y) > 1001.0).count()).collect())
        };
        assert_eq!(bands(&s)?, [4, 4, 4, 4], "untagged, the line is a band");
        s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [stud.hex()] }))?;
        assert_eq!(bands(&s)?, [3, 3, 3, 3], "tagged, it is not");
        Ok(())
    }

    fn ghost_points(s: &Session, space: &Space) -> T<(Vec<Vec2>, Bounds2)> {
        let list = crate::overlays::lamb_overlay(s.doc()?, space, &Options::default());
        let mut pts = Vec::new();
        let mut bounds = Bounds2::EMPTY;
        for p in list.prims.iter().filter(|p| p.color == GHOST_COLOR && p.transparency == GHOST_TRANSPARENCY) {
            for q in list.points(p) {
                pts.push(*q);
                bounds.add(*q);
            }
        }
        Ok((pts, bounds))
    }

    fn has(pts: &[Vec2], x: f64, y: f64) -> bool {
        pts.iter().any(|p| (p.x - x).abs() < 1e-9 && (p.y - y).abs() < 1e-9)
    }

    #[test]
    fn a_repeat_ghosts_its_members_one_pitch_along_without_changing_the_drawing() -> R {
        let Fixture { mut s, plan, detail, stud, pitch, .. } = fixture()?;
        let space = Space::Block(view_block_name(detail));
        assert!(ghost_points(&s, &space)?.0.is_empty(), "untagged: no ghost");
        s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [stud.hex()] }))?;

        let (undo, count) = (undo_len(&s)?, s.doc()?.blocks.values().map(|b| b.entities.len()).sum::<usize>() + s.doc()?.model.len());
        let (pts, bounds) = ghost_points(&s, &space)?;
        let built = cadcraft_render::build_entities(s.doc()?, s.doc()?.entity(stud).map(|e| &**e), &Options::default());
        let want: Vec<Vec2> = built.prims.iter().flat_map(|p| built.points(p).iter().map(|q| *q + Vec2::new(406.4, 0.0))).collect();
        assert_eq!(pts, want);
        assert!(has(&pts, 406.4, 0.0) && has(&pts, 444.4, 90.0), "{pts:?}");
        assert!((bounds.max.x - 444.4).abs() < 1e-9, "{bounds:?}");
        assert_eq!(undo_len(&s)?, undo, "drawing the overlay records nothing");
        assert_eq!(s.doc()?.blocks.values().map(|b| b.entities.len()).sum::<usize>() + s.doc()?.model.len(), count);

        assert!(ghost_points(&s, &Space::Block(view_block_name(plan)))?.0.is_empty(), "plan: no ghost");
        assert!(ghost_points(&s, &Space::Model)?.0.is_empty(), "model: no ghost");

        s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [] }))?;
        assert!(ghost_points(&s, &space)?.0.is_empty(), "untagged: no ghost");
        s.undo()?;
        assert!(!ghost_points(&s, &space)?.0.is_empty(), "retagged by undo");
        s.execute("erase", &json!({ "handles": [stud.hex()] }))?;
        assert!(ghost_points(&s, &space)?.0.is_empty(), "erased member: no ghost");
        Ok(())
    }

    fn stud_at(s: &mut Session, x: f64, y: f64) -> T<Handle> {
        draw(s, "pline", json!({ "vertices": [[x, y], [x + 38.0, y], [x + 38.0, y + 90.0], [x, y + 90.0]], "closed": true }))
    }

    #[test]
    fn two_drawn_copies_convert_into_one_repeat_in_one_undo_step() -> R {
        let Fixture { mut s, detail, stud, pitch, .. } = fixture()?;
        let copy = stud_at(&mut s, 406.4, 0.0)?;
        let undo = undo_len(&s)?;
        let r = s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [stud.hex()], "copies": [copy.hex()] }))?;
        assert_eq!(r["members"], json!([stud.hex()]));
        assert_eq!(r["pitch"], json!([406.4, 0.0]));
        assert_eq!(r["erased"], json!([copy.hex()]));
        assert!(s.doc()?.entity(copy).is_none(), "copy erased");
        assert_eq!(undo_len(&s)?, undo + 1);
        assert!(s.log.iter().any(|l| l == "Converted 1 copies into a repeat"), "{:?}", s.log);
        assert_eq!(detail_repeats(s.doc()?, detail)?.iter().map(|r| r.members.clone()).collect::<Vec<_>>(), [vec![stud]]);
        s.undo()?;
        assert!(s.doc()?.entity(copy).is_some(), "undo restores the copy");
        assert!(detail_repeats(s.doc()?, detail)?.is_empty(), "undo untags");
        Ok(())
    }

    #[test]
    fn a_copy_on_the_negative_side_converts_too() -> R {
        let Fixture { mut s, detail, stud, pitch, .. } = fixture()?;
        let copy = stud_at(&mut s, -406.4, 0.0)?;
        s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [stud.hex()], "copies": [copy.hex()] }))?;
        assert!(s.doc()?.entity(copy).is_none());
        assert_eq!(detail_repeats(s.doc()?, detail)?.len(), 1);
        Ok(())
    }

    #[test]
    fn copies_that_do_not_match_refuse_with_the_drawing_unchanged() -> R {
        let Fixture { mut s, detail, reference, stud, edge, pitch, .. } = fixture()?;
        let off = stud_at(&mut s, 407.4, 0.0)?;
        let other_shape = draw(&mut s, "pline", json!({ "vertices": [[406.4, 0], [446.4, 0], [446.4, 90], [406.4, 90]], "closed": true }))?;
        let good = stud_at(&mut s, 406.4, 0.0)?;
        let text = draw(&mut s, "text", json!({ "at": [500, 500], "text": "abc" }))?;
        let undo = undo_len(&s)?;
        let cases: [(Vec<Handle>, Vec<Handle>, &str); 6] = [
            (vec![stud], vec![off], "The copies do not match the selection"),
            (vec![stud], vec![other_shape], "The copies do not match the selection"),
            (vec![stud], vec![good, edge], "The copies do not match the selection"),
            (vec![stud], vec![reference], "The reference line cannot be repeated"),
            (vec![stud], vec![text], "Only line work can be converted"),
            (vec![], vec![good], "Nothing to repeat"),
        ];
        for (members, copies, want) in cases {
            let hex = |v: &[Handle]| v.iter().map(|h| h.hex()).collect::<Vec<_>>();
            let e = s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": hex(&members), "copies": hex(&copies) })).err().ok_or(want)?;
            assert!(e.to_string().contains(want), "{e} / {want}");
            assert_eq!(undo_len(&s)?, undo, "{want}: no undo step");
            assert!(detail_repeats(s.doc()?, detail)?.is_empty(), "{want}: untagged");
            for h in copies {
                assert!(s.doc()?.entity(h).is_some(), "{want}: copy kept");
            }
        }
        Ok(())
    }

    #[test]
    fn a_converted_repeat_is_laid_exactly_as_one_tagged_by_hand() -> R {
        let laid = |convert: bool| -> T<Vec<crate::assembly::RunAssembly>> {
            let (mut s, plan) = crate::assembly::tests::room_with("exterior_face", true, 200.0)?;
            let detail = read_document(s.doc()?)?.views.iter().find(|v| v.name() == "D1").ok_or("no D1")?.id();
            s.execute("editview", &json!({ "viewId": detail.0 }))?;
            let stud = stud_at(&mut s, 0.0, 50.0)?;
            let pitch = draw(&mut s, "dimaligned", json!({ "p1": [0, 0], "p2": [406.4, 0], "at": [200, -300] }))?;
            let mut p = json!({ "dimension": pitch.hex(), "handles": [stud.hex()] });
            if convert {
                let copy = stud_at(&mut s, 406.4, 50.0)?;
                p["copies"] = json!([copy.hex()]);
            }
            s.execute("repeat", &p)?;
            Ok(crate::assembly::assembly_lines(s.doc()?, plan)?)
        };
        let (by_hand, converted) = (laid(false)?, laid(true)?);
        assert!(by_hand.iter().any(|r| r.lines.len() > r.bands), "the repeat lays copies");
        assert_eq!(converted, by_hand);
        Ok(())
    }
}

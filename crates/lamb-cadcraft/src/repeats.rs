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
//! - Each repeat reads back as `{dimension, members, pitch: [x, y], pitchLength, parallel}`;
//!   `parallel` is `null` when the detail has no reference line. A pitch not parallel to the
//!   reference is tagged anyway and flagged ("Repeat is not parallel to the reference line").
//! - Repeat members are not continuous: [`crate::assembly`] lays no band from them.

use std::collections::HashSet;

use cadcraft_doc::{DimKind, Drawing, Entity, EntityKind, Handle};
use cadcraft_engine::{Result, Session};
use lamb_core::geometry::Point2;
use lamb_core::geometry::tolerance::{TOLERANCE, deg_to_rad};
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::commands::{bad, other};
use crate::detailref::{detail_reference, edited_detail};
use crate::schema::{RepeatTag, read_entity_data, set_entity_data, view_block_name};

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
    let members: Vec<Handle> = hv
        .as_array()
        .ok_or_else(|| bad("repeat", "`handles` must be an array of handles"))?
        .iter()
        .map(|v| parse_handle(v).ok_or_else(|| bad("repeat", "each handle must be a hex handle string or a number")))
        .collect::<Result<_>>()?;

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
            reference.map(|r| is_parallel(pitch, r.from, r.to))
        } else {
            None
        }
    };

    let tag = (!members.is_empty()).then(|| RepeatTag { members: members.iter().map(|h| Id(h.0)).collect() });
    let d = s.doc_mut()?;
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
    find(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::read_document;
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
        let bands = |s: &Session| -> T<Vec<usize>> { Ok(crate::assembly::assembly_lines(s.doc()?, plan)?.iter().map(|r| r.lines.len()).collect()) };
        assert_eq!(bands(&s)?, [4, 4, 4, 4], "untagged, the line is a band");
        s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [stud.hex()] }))?;
        assert_eq!(bands(&s)?, [3, 3, 3, 3], "tagged, it is not");
        Ok(())
    }
}

//! DETAILNOTES (LAMB T689): an edge detail's generated layer notes, one per layer of its build-up
//! ([`crate::assembly::detail_layers`]) and one per repeat ([`crate::repeats::detail_repeats`]),
//! each with a two-segment leader from the layer to a column of notes past one end of the
//! reference line. `detailnotes {viewId?, off?}` (`dnote`) adds the detail to, or removes it from,
//! the document's `detail_note_views`; [`crate::annotations`] writes each listed detail's [`notes`]
//! into its annotation block, so they follow the bands after every command and plot wherever the
//! detail does. Off by default: a drafter who writes their own notes is not given a second set.
//!
//! A layer's note reads `<thickness> <material>` (`13 Gypsum board`), the material being the CAD
//! layer's description, or its name when the description is empty; a zero-thickness (one-band)
//! layer's note is the material alone. A repeat reads `<material> @ <pitch>`, the material its
//! first member's layer. Corner details get none.
//!
//! Layout, in the detail's frame ([`detail_frame`]): the label end `E` is whichever reference end
//! has the larger x (ties: larger y), `M` the reference's midpoint. Notes are sorted by offset `m`
//! (a layer's mid-offset, a repeat's first member's centroid on `into`) and stacked at rows
//! `y₀ = m₀`, `yᵢ = max(mᵢ, yᵢ₋₁ + 1.5h)`. A leader runs `M + into·m` → `E + s·along·gap/2 +
//! into·y` → `E + s·along·gap + into·y`, `s` pointing away from `M`; its note is left-aligned at
//! the leader's end + `(0.3h, −0.5h)`. `h` is half the cut-symbol text height and `gap` the
//! cut-arrow length, both sized for the paper like every generated annotation
//! ([`crate::annotations::k`]).

use cadcraft_doc::Drawing;
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use lamb_core::derivation::assembly::detail_frame;
use lamb_core::geometry::Point2;
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::annotations::{CUT_ARROW_MM, CUT_TEXT_MM, k};
use crate::assembly::{detail_layers, detail_lines};
use crate::commands::{bad, edited_view, id_value, other};
use crate::convert::to_polyline;
use crate::detailref::detail_reference;
use crate::repeats::detail_repeats;
use crate::schema::{read_document, write_document};

/// One generated note in the detail's local coordinates: its leader polyline (from the layer to
/// the note), the note's left-aligned insertion point, its text and height.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub leader: Vec<Point2>,
    pub at: Point2,
    pub text: String,
    pub height: f64,
}

/// The CAD layer's description, or its name when that is empty.
fn material(d: &Drawing, layer: &str) -> String {
    d.layer(layer).map(|l| l.description.trim().to_owned()).filter(|t| !t.is_empty()).unwrap_or_else(|| layer.to_owned())
}

fn add(a: Point2, b: Point2, f: f64) -> Point2 {
    Point2 { x: a.x + b.x * f, y: a.y + b.y * f }
}

/// The centroid of `e`'s polyline vertices, if it converts.
fn centroid(e: &cadcraft_doc::Entity) -> Option<Point2> {
    let poly = to_polyline(e).ok()?.polyline;
    let mut pts: Vec<Point2> = std::iter::once(poly.start).chain(poly.segments.iter().map(|s| s.to)).collect();
    if pts.len() > 1 && pts.last() == Some(&poly.start) {
        pts.pop();
    }
    let n = pts.len() as f64;
    Some(Point2 { x: pts.iter().map(|p| p.x).sum::<f64>() / n, y: pts.iter().map(|p| p.y).sum::<f64>() / n })
}

/// The notes of edge detail `detail` at 1:`scale`, in offset order; none for a corner detail, a
/// detail without a reference or bands, or one whose wall side is ambiguous.
pub fn notes(d: &Drawing, detail: Id, scale: f64) -> Vec<Note> {
    let Ok(Some(r)) = detail_reference(d, detail) else { return Vec::new() };
    if r.corner.is_some() {
        return Vec::new();
    }
    let Ok(lines) = detail_lines(d, detail, &r.handles()) else { return Vec::new() };
    let Ok(f) = detail_frame((r.from, r.to), &lines) else { return Vec::new() };
    let Ok(layers) = detail_layers(d, detail) else { return Vec::new() };
    let mut rows: Vec<(f64, String)> = layers
        .iter()
        .map(|l| {
            let m = material(d, &l.layer);
            let text = if l.thickness.round() == 0.0 { m } else { format!("{:.0} {m}", l.thickness) };
            (l.offset + l.thickness / 2.0, text)
        })
        .collect();
    for rep in detail_repeats(d, detail).unwrap_or_default() {
        let Some(e) = rep.members.first().and_then(|h| d.entity(*h)) else { continue };
        let Some(c) = centroid(e) else { continue };
        let off = (c.x - f.origin.x) * f.into.x + (c.y - f.origin.y) * f.into.y;
        let pitch = rep.pitch.x.hypot(rep.pitch.y);
        rows.push((off, format!("{} @ {pitch:.0}", material(d, &e.common.layer))));
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));

    let (h, gap) = (0.5 * CUT_TEXT_MM * k(scale), CUT_ARROW_MM * k(scale));
    let e_end = if r.to.x > r.from.x || (r.to.x == r.from.x && r.to.y > r.from.y) { r.to } else { r.from };
    let mid = Point2 { x: (r.from.x + r.to.x) / 2.0, y: (r.from.y + r.to.y) / 2.0 };
    let s = if (e_end.x - mid.x) * f.along.x + (e_end.y - mid.y) * f.along.y >= 0.0 { 1.0 } else { -1.0 };
    let mut out = Vec::new();
    let mut prev: Option<f64> = None;
    for (m, text) in rows {
        let y = prev.map_or(m, |p| m.max(p + 1.5 * h));
        prev = Some(y);
        let end = add(add(e_end, f.along, s * gap), f.into, y);
        let leader = vec![add(mid, f.into, m), add(add(e_end, f.along, s * gap / 2.0), f.into, y), end];
        out.push(Note { leader, at: Point2 { x: end.x + 0.3 * h, y: end.y - 0.5 * h }, text, height: h });
    }
    out
}

/// Whether `view` is a detail with a linear (face) reference; else the refusal.
fn check_detail(d: &Drawing, view: Id) -> std::result::Result<(), String> {
    const REFUSE: &str = "Not a detail with a face reference";
    match detail_reference(d, view) {
        Ok(Some(r)) if r.corner.is_none() => Ok(()),
        _ => Err(REFUSE.to_owned()),
    }
}

/// DETAILNOTES `{viewId?, off?}`: turns the detail's generated layer notes on, or off with
/// `off: true`. `viewId` defaults to the edited view. Returns `{viewId, notes, message}`. A detail
/// already in that state records no undo step.
pub fn detailnotes(s: &mut Session, p: &Value) -> Result<Value> {
    let view = match p.get("viewId") {
        None | Some(Value::Null) => edited_view(s).ok_or_else(|| other("Edit a detail first (EDITVIEW), or give `viewId`"))?,
        Some(v) => id_value(v).ok_or_else(|| bad("detailnotes", "`viewId` must be a view id"))?,
    };
    let off = match p.get("off") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(bad("detailnotes", "`off` must be true or false")),
    };
    check_detail(s.doc()?, view).map_err(other)?;
    let mut doc = read_document(s.doc()?).map_err(|e| other(e.to_string()))?;
    let name = doc.views.iter().find(|v| v.id() == view).map(|v| v.name().to_owned()).unwrap_or_default();
    let list = &mut doc.detail_note_views;
    if list.contains(&view) == off {
        list.retain(|v| *v != view);
        if !off {
            list.push(view);
        }
        write_document(s.doc_mut()?, &doc).map_err(|e| other(e.to_string()))?;
    }
    let (n, message) = if off {
        (0, format!("Detail notes off for {name}."))
    } else {
        let n = notes(s.doc()?, view, 50.0).len();
        (n, format!("{n} notes on {name}."))
    };
    s.echo(message.clone());
    Ok(json!({ "viewId": view.0, "notes": n, "message": message }))
}

/// DETAILNOTES' interactive form, on the edited detail.
struct DetailNotesM {
    view: Id,
}

impl Interactive for DetailNotesM {
    fn name(&self) -> &'static str {
        "detailnotes"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Detail notes", Accept::TEXT).kw(&["On", "Off"]).options_only().default("On".to_owned())
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        s.echo("DETAILNOTES: labels each layer of this detail with its thickness and material, with leaders, on screen and on its sheet.");
        let edited = edited_view(s).ok_or_else(|| "DETAILNOTES needs a detail: open one from the Views palette or with EDITVIEW.".to_owned());
        match edited.and_then(|v| check_detail(s.doc().map_err(|e| e.to_string())?, v).map(|()| v)) {
            Ok(v) => {
                self.view = v;
                Ok(Step::Continue)
            }
            Err(e) => {
                s.echo(e);
                Ok(Step::Done)
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let off = match i {
            Input::Cancel => return Ok(Step::Cancel),
            Input::Enter => false,
            Input::Text(t) | Input::Keyword(t) => match t.trim().to_ascii_lowercase().as_str() {
                "on" => false,
                "off" => true,
                _ => {
                    s.echo("On or Off");
                    return Ok(Step::Continue);
                }
            },
            _ => return Ok(Step::Continue),
        };
        if let Err(e) = detailnotes(s, &json!({ "viewId": self.view.0, "off": off })) {
            s.echo(e.to_string());
        }
        Ok(Step::Done)
    }
}

pub(crate) fn detailnotes_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(DetailNotesM { view: Id(0) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{annotation_block_name, view_block_name};
    use cadcraft_doc::{EntityKind, HAlign, Handle};
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

    fn undo_len(s: &Session) -> T<usize> {
        Ok(s.state()?.undo.len())
    }

    /// A horizontal edge detail `2x6 wall`, edited: reference (0,0)→(1000,0), bands GYP 0/13 (layer
    /// described "Gypsum board"), STUD 13/153, OSB 153/164, and a STUD stud 100..138 × 13..153
    /// repeated at 406.
    fn detail() -> T<(Session, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let d = Id(s.execute("newdetail", &json!({ "name": "2x6 wall" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": d.0 }))?;
        let r = line(&mut s, [0.0, 0.0], [1000.0, 0.0])?;
        s.execute("detailref", &json!({ "handle": format!("{:X}", r.0), "kind": "interior_face" }))?;
        for (layer, a, b) in [("GYP", 0.0, 13.0), ("STUD", 13.0, 153.0), ("OSB", 153.0, 164.0)] {
            s.execute("layer.new", &json!({ "name": layer, "current": true }))?;
            line(&mut s, [0.0, a], [1000.0, a])?;
            line(&mut s, [0.0, b], [1000.0, b])?;
        }
        s.doc_mut()?.layers.iter_mut().find(|l| l.name == "GYP").ok_or("no GYP")?.description = "Gypsum board".into();
        s.execute("layer.current", &json!({ "name": "STUD" }))?;
        let before = all_handles(&s)?;
        s.execute("pline", &json!({ "vertices": [[100, 13], [138, 13], [138, 153], [100, 153]], "closed": true }))?;
        let stud = all_handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("no stud")?;
        let before = all_handles(&s)?;
        s.execute("dimaligned", &json!({ "p1": [100, -50], "p2": [506, -50], "at": [300, -300] }))?;
        let pitch = all_handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("no dimension")?;
        s.execute("repeat", &json!({ "dimension": format!("{:X}", pitch.0), "handles": [format!("{:X}", stud.0)] }))?;
        Ok((s, d))
    }

    /// The annotation block's labels (text, insert, height) and lines.
    fn block(s: &Session, view: Id) -> T<(Vec<(String, Point2, f64)>, Vec<(Point2, Point2)>)> {
        let Some(b) = s.doc()?.block(&annotation_block_name(view)) else { return Ok((Vec::new(), Vec::new())) };
        let (mut labels, mut lines) = (Vec::new(), Vec::new());
        for e in b.entities.iter() {
            match &e.kind {
                EntityKind::Text(t) => {
                    assert_eq!(t.halign, HAlign::Left);
                    let p = t.align_pt.unwrap_or(t.insert);
                    labels.push((t.value.clone(), Point2 { x: p.x, y: p.y }, t.height));
                }
                EntityKind::Line(l) => lines.push((Point2 { x: l.a.x, y: l.a.y }, Point2 { x: l.b.x, y: l.b.y })),
                _ => return Err("unexpected entity".into()),
            }
        }
        Ok((labels, lines))
    }

    #[test]
    fn notes_label_each_layer_and_the_repeat_in_offset_order() -> R {
        let (mut s, d) = detail()?;
        assert!(block(&s, d)?.0.is_empty(), "off by default");
        let r = s.execute("detailnotes", &json!({}))?;
        assert_eq!(r["notes"], 4);
        assert_eq!(r["message"], "4 notes on 2x6 wall.");
        let (labels, lines) = block(&s, d)?;
        let texts: Vec<&str> = labels.iter().map(|l| l.0.as_str()).collect();
        assert_eq!(texts, ["13 Gypsum board", "140 STUD", "STUD @ 406", "11 OSB"]);
        let h = labels[0].2;
        assert!(labels.windows(2).all(|w| w[1].1.y - w[0].1.y >= 1.5 * h - 1e-9), "{labels:?}");
        // Two leader segments per note, the first starting on the layer's mid-offset at the
        // reference's midpoint, the note past the right end.
        assert_eq!(lines.len(), 8);
        let starts: Vec<f64> = lines.iter().step_by(2).map(|(a, _)| a.y).collect();
        assert_eq!(starts, [6.5, 83.0, 83.0, 158.5]);
        assert!(lines.iter().step_by(2).all(|(a, _)| a.x == 500.0));
        assert!(labels.iter().all(|l| l.1.x > 1000.0));
        // A rerun changes nothing and records no undo step.
        let n = undo_len(&s)?;
        s.execute("detailnotes", &json!({}))?;
        assert_eq!(undo_len(&s)?, n);
        Ok(())
    }

    #[test]
    fn on_a_1_5_sheet_the_notes_are_2_5_mm_on_paper_and_follow_the_viewport_scale() -> R {
        let (mut s, d) = detail()?;
        s.execute("detailnotes", &json!({}))?;
        let out = s.execute("newsheet", &json!({ "name": "A501", "viewIds": [d.0] }))?;
        let hex = out["viewports"][0].as_str().ok_or("no viewport")?.to_owned();
        s.execute("viewport.set", &json!({ "handle": hex, "scale": "1:5" }))?;
        let (labels, _) = block(&s, d)?;
        assert!(!labels.is_empty() && labels.iter().all(|l| (l.2 / 5.0 - 2.5).abs() < 1e-9), "{labels:?}");
        s.execute("viewport.set", &json!({ "handle": hex, "scale": "1:10" }))?;
        let (labels, _) = block(&s, d)?;
        assert!(labels.iter().all(|l| (l.2 / 10.0 - 2.5).abs() < 1e-9), "{labels:?}");
        Ok(())
    }

    #[test]
    fn off_empties_refusals_record_nothing_undo_restores_and_dxf_keeps_the_list() -> R {
        let (mut s, d) = detail()?;
        s.execute("detailnotes", &json!({}))?;
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert_eq!(read_document(&back)?.detail_note_views, [d]);

        let n = undo_len(&s)?;
        let r = s.execute("detailnotes", &json!({ "off": true }))?;
        assert_eq!(r["message"], "Detail notes off for 2x6 wall.");
        assert!(block(&s, d)?.0.is_empty() && block(&s, d)?.1.is_empty());
        assert_eq!(undo_len(&s)?, n + 1);
        s.undo()?;
        assert_eq!(block(&s, d)?.0.len(), 4);

        // A plan and a corner detail refuse, with no undo step.
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let cd = Id(s.execute("newdetail", &json!({ "name": "CD" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": cd.0 }))?;
        s.execute("layer.current", &json!({ "name": "0" }))?;
        let before = all_handles(&s)?;
        s.execute("pline", &json!({ "vertices": [[-1000, 0], [0, 0], [0, 200]] }))?;
        let c = all_handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("no corner")?;
        s.execute("detailref", &json!({ "handle": format!("{:X}", c.0), "kind": "exterior_face" }))?;
        let n = undo_len(&s)?;
        for v in [plan, cd] {
            let e = s.execute("detailnotes", &json!({ "viewId": v.0 })).err().ok_or("accepted")?;
            assert!(e.to_string().contains("Not a detail with a face reference"), "{e}");
        }
        assert_eq!(undo_len(&s)?, n);
        assert!(s.doc()?.block(&view_block_name(cd)).is_some());
        Ok(())
    }

    #[test]
    fn the_interactive_form_prompts_on_or_off() -> R {
        let (mut s, d) = detail()?;
        s.start("detailnotes")?;
        s.input(Input::Enter)?;
        assert_eq!(read_document(s.doc()?)?.detail_note_views, [d]);
        s.start("detailnotes")?;
        s.input(Input::Keyword("Off".into()))?;
        assert!(read_document(s.doc()?)?.detail_note_views.is_empty());
        Ok(())
    }
}

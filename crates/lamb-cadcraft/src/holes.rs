//! View holes (LAMB's `docs/06-ui-ux/selection-and-link-editing.md` §4c, P2-9; LAMB T502): a
//! region of a placed section or elevation that shows a parallel placed vertical view's line work
//! through it — the equivalent of a clipped xref, needing no setup because placed views already
//! align. Purely aesthetic: it affects what is drawn and plotted, never what is authoritative.
//!
//! A hole is a closed straight-edged `LwPolyline` in the view's block carrying
//! [`EntityData::hole`] (its source view). Tagging also stores its role as `Reference`, so it never
//! auto-links and never makes markers. The source's line work — each entity of its block through
//! [`to_polyline`], arcs chorded at ≤ [`CHORD_DEG`]°, every vertex through lamb-core's
//! [`map_view_local_point`] — is clipped to each boundary (even-odd) and written as LINEs on the
//! source entity's layer into one block per view, [`hole_block_name`]`(view)`, never the view block,
//! so links, derivation, the wall graph and LAMBSAVE never see it. Its one model-space INSERT sits at
//! the origin on the view's own layer, as [`crate::annotations`] does, so sheet viewports showing the
//! view show and plot it. [`hole_observer`] regenerates the blocks after every command and writes
//! only when one changed, so the change lands in the command's own undo step. A tag whose source no
//! longer qualifies (moved, unplaced, deleted) reveals nothing; the tag stays. The view's own lines
//! inside the hole are not hidden.
//!
//! - `viewhole {}` → `{"holes": [{boundary, source, sourceName}]}` for the edited view.
//! - `viewhole {boundary, source?}` tags; `source` absent → the nearest parallel placed vertical
//!   view (smallest perpendicular distance between the cut lines in global XY, ties → first).
//! - `viewhole {boundary, remove: true}` untags. One undo step each; refusals change nothing.
//!
//! Interactive: `Select hole boundary or [Remove/List]:`, then `Enter source view name or
//! [Nearest] <Nearest>:`.

use std::sync::Arc;

use cadcraft_doc::{Block, Common, Drawing, Entity, EntityKind, Handle, Insert, Line, Space};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::{Vec2, Vec3};
use lamb_core::derivation::projection::vertical_basis;
use lamb_core::derivation::read::{ProjectRead, ViewKind};
use lamb_core::derivation::underlay::{find_underlay_candidates, map_view_local_point};
use lamb_core::geometry::Point2;
use lamb_core::geometry::polyline::arc_from_bulge;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{Id, LineRole};
use serde_json::{Value, json};

use crate::commands::{bad, edited_view, id_value, other};
use crate::convert::to_polyline;
use crate::project::DrawingProject;
use crate::schema::{EntityData, HoleTag, hole_block_name, read_document, read_entity_data, set_entity_data, view_block_name};

/// An arc is chorded at most this many degrees per chord.
pub const CHORD_DEG: f64 = 5.0;

/// One revealed line, in the edited view's local coordinates, on the source entity's layer.
#[derive(Clone, Debug, PartialEq)]
struct Item {
    layer: String,
    a: Vec2,
    b: Vec2,
}

fn near(a: Vec2, b: Vec2) -> bool {
    (a.x - b.x).hypot(a.y - b.y) <= TOLERANCE.coincidence_mm
}

/// Even-odd point-in-polygon.
fn inside(poly: &[Vec2], p: Vec2) -> bool {
    let mut c = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) * (b.x - a.x) / (b.y - a.y) {
            c = !c;
        }
    }
    c
}

/// Segment `a`→`b` clipped to the closed polygon `poly` (even-odd): the parameter intervals between
/// its crossings with every edge whose midpoint is inside, adjacent kept intervals merged.
fn clip_segment(a: Vec2, b: Vec2, poly: &[Vec2]) -> Vec<(Vec2, Vec2)> {
    let d = b - a;
    let mut ts = vec![0.0, 1.0];
    let n = poly.len();
    for i in 0..n {
        let (p, q) = (poly[i], poly[(i + 1) % n]);
        let e = q - p;
        let den = d.x * e.y - d.y * e.x;
        if den.abs() < 1e-12 {
            continue; // parallel: an overlap's ends are found by the neighbouring edges
        }
        let w = p - a;
        let t = (w.x * e.y - w.y * e.x) / den;
        let u = (w.x * d.y - w.y * d.x) / den;
        if (0.0..=1.0).contains(&t) && (-1e-12..=1.0 + 1e-12).contains(&u) {
            ts.push(t);
        }
    }
    ts.sort_by(f64::total_cmp);
    let at = |t: f64| a + d * t;
    let mut out: Vec<(f64, f64)> = Vec::new();
    for w in ts.windows(2) {
        let (t0, t1) = (w[0], w[1]);
        if (t1 - t0) * d.len() <= TOLERANCE.coincidence_mm || !inside(poly, at((t0 + t1) / 2.0)) {
            continue;
        }
        match out.last_mut() {
            Some(last) if (last.1 - t0).abs() * d.len() <= TOLERANCE.coincidence_mm => last.1 = t1,
            _ => out.push((t0, t1)),
        }
    }
    out.into_iter().map(|(t0, t1)| (at(t0), at(t1))).collect()
}

/// A polyline's vertices with each bulge segment chorded at ≤ [`CHORD_DEG`]°.
fn chorded(p: &lamb_core::geometry::Polyline2) -> Vec<Point2> {
    let mut out = vec![p.start];
    let mut from = p.start;
    for s in &p.segments {
        if let Some(arc) = arc_from_bulge(from, s.to, s.bulge) {
            let sweep = 4.0 * s.bulge.atan();
            let n = (sweep.abs() / CHORD_DEG.to_radians()).ceil().max(1.0) as usize;
            for i in 1..n {
                let a = arc.start_angle + sweep * i as f64 / n as f64;
                out.push(Point2 { x: arc.center.x + arc.radius * a.cos(), y: arc.center.y + arc.radius * a.sin() });
            }
        }
        out.push(s.to);
        from = s.to;
    }
    out
}

/// A closed `LwPolyline` with no bulges: its vertices (a last vertex repeating the first dropped).
fn boundary_of(e: &Entity) -> Option<Vec<Vec2>> {
    let EntityKind::LwPolyline(pl) = &e.kind else { return None };
    if !pl.closed || pl.vertices.iter().any(|v| v.bulge.abs() > 1e-12) {
        return None;
    }
    let mut v: Vec<Vec2> = pl.vertices.iter().map(|v| v.p).collect();
    if v.len() > 1 && v.first().zip(v.last()).is_some_and(|(a, b)| near(*a, *b)) {
        v.pop();
    }
    (v.len() >= 3).then_some(v)
}

fn hole_tag(e: &Entity) -> Option<HoleTag> {
    read_entity_data(&e.common).ok().and_then(|d| d.hole)
}

/// Every hole of `view`: (boundary handle, its polygon, its tag).
fn holes_of(d: &Drawing, view: Id) -> Vec<(Handle, Vec<Vec2>, HoleTag)> {
    let Some(block) = d.block(&view_block_name(view)) else { return Vec::new() };
    block.entities.iter().filter_map(|e| Some((e.handle, boundary_of(e)?, hole_tag(e)?))).collect()
}

/// `edit`'s parallel placed vertical views, nearest first (perpendicular distance between the cut
/// lines in global XY; ties keep lamb-core's order).
fn sources_by_distance(dp: &DrawingProject<'_>, edit: Id) -> Vec<Id> {
    let Ok(cands) = find_underlay_candidates(dp, edit) else { return Vec::new() };
    let basis = |id: Id| {
        let v = dp.view(id)?;
        let host = dp.view(v.host_view_id()?)?;
        vertical_basis(&v, &host).ok()
    };
    let Some(b0) = basis(edit) else { return Vec::new() };
    let n = Point2 { x: -b0.c_hat.y, y: b0.c_hat.x };
    let mut out: Vec<(f64, Id)> = cands
        .iter()
        .filter_map(|c| {
            let b = basis(c.view_id)?;
            Some((((b.origin_xy.x - b0.origin_xy.x) * n.x + (b.origin_xy.y - b0.origin_xy.y) * n.y).abs(), c.view_id))
        })
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.into_iter().map(|(_, id)| id).collect()
}

/// What view `id` wants in its hole block, in write order.
fn wanted(d: &Drawing, dp: &DrawingProject<'_>, id: Id) -> Vec<Item> {
    let holes = holes_of(d, id);
    if holes.is_empty() {
        return Vec::new();
    }
    let Some(to) = dp.view(id).filter(|v| v.kind == ViewKind::Vertical && v.placement.is_some()) else { return Vec::new() };
    let sources = sources_by_distance(dp, id);
    let mut out = Vec::new();
    for (_, poly, tag) in holes {
        if !sources.contains(&tag.source) {
            continue;
        }
        let Some(from) = dp.view(tag.source) else { continue };
        let Some(block) = d.block(&view_block_name(tag.source)) else { continue };
        for e in block.entities.iter() {
            if hole_tag(e).is_some() {
                continue;
            }
            let Ok(c) = to_polyline(e) else { continue };
            let mut pts = chorded(&c.polyline);
            if c.polyline.closed {
                pts.push(c.polyline.start);
            }
            let mapped: Option<Vec<Vec2>> =
                pts.iter().map(|p| map_view_local_point(dp, &from, &to, *p).ok().flatten().map(|q| Vec2::new(q.x, q.y))).collect();
            let Some(mapped) = mapped else { continue };
            for w in mapped.windows(2) {
                for (a, b) in clip_segment(w[0], w[1], &poly) {
                    out.push(Item { layer: e.common.layer.clone(), a, b });
                }
            }
        }
    }
    out
}

fn block_matches(block: &Block, want: &[Item]) -> bool {
    block.entities.len() == want.len()
        && block
            .entities
            .iter()
            .zip(want)
            .all(|(e, w)| e.common.layer == w.layer && matches!(&e.kind, EntityKind::Line(l) if near(l.a.xy(), w.a) && near(l.b.xy(), w.b)))
}

fn ensure_insert(d: &mut Drawing, view: Id) {
    let name = hole_block_name(view);
    if d.model.iter().any(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == name)) {
        return;
    }
    let layer = view_block_name(view);
    d.ensure_layer(&layer);
    let insert = Insert {
        block: name,
        insert: Vec3::ZERO,
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: Vec::new(),
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    };
    let handle = d.new_handle();
    d.model.push(Entity { handle, common: Common { layer, ..Common::default() }, kind: EntityKind::Insert(insert) });
}

fn replace_block(d: &mut Drawing, view: Id, items: &[Item]) -> std::result::Result<(), String> {
    let name = hole_block_name(view);
    d.blocks.insert(name.clone(), Arc::new(Block::new(&name)));
    let space = Space::Block(name);
    for i in items {
        d.ensure_layer(&i.layer);
        let kind = EntityKind::Line(Line { a: i.a.to3(0.0), b: i.b.to3(0.0) });
        d.add(&space, Common { layer: i.layer.clone(), ..Common::default() }, kind).map_err(|e| e.to_string())?;
    }
    ensure_insert(d, view);
    Ok(())
}

/// Per view whose hole block must change: the view and its wanted items.
fn changes(d: &Drawing) -> std::result::Result<Vec<(Id, Vec<Item>)>, String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    let mut ids: Vec<Id> = doc.views.iter().map(lamb_core::model::AnyView::id).collect();
    ids.retain(|id| d.block(&hole_block_name(*id)).is_some() || !holes_of(d, *id).is_empty());
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let dp = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for id in ids {
        let want = wanted(d, &dp, id);
        let changed = match d.block(&hole_block_name(id)) {
            Some(b) => !block_matches(b, &want),
            None => !want.is_empty(),
        };
        if changed {
            out.push((id, want));
        }
    }
    Ok(out)
}

/// The after-command observer: every view's [`hole_block_name`] block made to equal what its holes
/// reveal — untouched when it already does, rewritten (and given its INSERT) when not, emptied but
/// kept when nothing shows. Writes the drawing only when some block changed.
pub fn hole_observer(s: &mut Session, _before: &Arc<Drawing>) {
    let todo = match s.doc().map_err(|e| e.to_string()).and_then(changes) {
        Ok(t) if t.is_empty() => return,
        Ok(t) => t,
        Err(e) => {
            s.echo(format!("LAMB holes: {e}"));
            return;
        }
    };
    let Ok(current) = s.doc() else { return };
    let mut after = current.clone();
    for (v, items) in &todo {
        if let Err(e) = replace_block(&mut after, *v, items) {
            s.echo(format!("LAMB holes: {e}"));
            return;
        }
    }
    if let Ok(d) = s.doc_mut() {
        *d = after;
    }
}

// ------------------------------------------------------------------------------ the command --

const NOT_VERTICAL: &str = "VIEWHOLE works in a placed section or elevation";

/// The edited view, when it is a placed vertical view.
fn edited_vertical(s: &Session) -> Result<Id> {
    let id = edited_view(s).ok_or_else(|| other(NOT_VERTICAL))?;
    let dp = DrawingProject::new(s.doc()?).map_err(other)?;
    dp.view(id).filter(|v| v.kind == ViewKind::Vertical && v.placement.is_some()).ok_or_else(|| other(NOT_VERTICAL))?;
    Ok(id)
}

fn view_name(dp: &DrawingProject<'_>, id: Id) -> String {
    dp.view(id).map_or_else(|| id.0.to_string(), |v| v.name.to_owned())
}

fn parse_handle(v: &Value) -> Option<Handle> {
    v.as_str().and_then(Handle::parse_hex).or_else(|| v.as_u64().map(Handle))
}

/// VIEWHOLE's JSON form (see the module docs).
pub(crate) fn viewhole(s: &mut Session, p: &Value) -> Result<Value> {
    let edit = edited_vertical(s)?;
    let Some(bv) = p.get("boundary") else {
        let d = s.doc()?;
        let dp = DrawingProject::new(d).map_err(other)?;
        let holes: Vec<Value> = holes_of(d, edit)
            .into_iter()
            .map(|(h, _, t)| json!({ "boundary": h.hex(), "source": t.source.0, "sourceName": view_name(&dp, t.source) }))
            .collect();
        return Ok(json!({ "holes": holes }));
    };
    let h = parse_handle(bv).ok_or_else(|| bad("viewhole", "`boundary` must be a hex handle string or a number"))?;
    let remove = p.get("remove").and_then(Value::as_bool).unwrap_or(false);
    let (data, source, source_name) = {
        let d = s.doc()?;
        let block = d.block(&view_block_name(edit)).ok_or_else(|| other(NOT_VERTICAL))?;
        let e = block.entities.iter().find(|e| e.handle == h).ok_or_else(|| other(format!("No such entity in this view: {}", h.hex())))?;
        let data = read_entity_data(&e.common).map_err(other)?;
        if remove {
            if data.hole.is_none() {
                return Err(other("That polyline is not a hole boundary"));
            }
            (data, None, String::new())
        } else {
            boundary_of(e).ok_or_else(|| other("A hole boundary must be a closed straight-edged polyline"))?;
            let dp = DrawingProject::new(d).map_err(other)?;
            let sources = sources_by_distance(&dp, edit);
            let src = match p.get("source") {
                None | Some(Value::Null) => *sources.first().ok_or_else(|| other("No parallel placed section or elevation to show"))?,
                Some(v) => {
                    let id = id_value(v).ok_or_else(|| bad("viewhole", "`source` must be a view id"))?;
                    if !sources.contains(&id) {
                        return Err(other(format!("{} is not a parallel placed section or elevation", view_name(&dp, id))));
                    }
                    id
                }
            };
            (data, Some(src), view_name(&dp, src))
        }
    };
    let new = match source {
        Some(src) => EntityData { role: LineRole::Reference, hole: Some(HoleTag { source: src }), ..data.clone() },
        None => EntityData { hole: None, ..data.clone() },
    };
    if new != data {
        let mut res = Ok(());
        s.doc_mut()?
            .modify_entity(h, |e| {
                res = set_entity_data(&mut e.common, &new);
            })
            .map_err(other)?;
        res.map_err(other)?;
    }
    Ok(match source {
        Some(src) => json!({ "boundary": h.hex(), "source": src.0, "sourceName": source_name, "message": format!("Hole shows \"{source_name}\"") }),
        None => json!({ "boundary": h.hex(), "removed": true, "message": "Hole removed" }),
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Boundary,
    Remove,
    Source(Handle),
}

/// VIEWHOLE: pick a boundary (or Remove / List), then the source view by name or Nearest.
struct ViewHoleM {
    phase: Phase,
}

impl ViewHoleM {
    fn run(s: &mut Session, p: &Value) {
        match viewhole(s, p) {
            Ok(r) => {
                if let Some(m) = r.get("message").and_then(Value::as_str) {
                    s.echo(m.to_owned());
                }
            }
            Err(e) => s.echo(e.to_string()),
        }
    }

    fn list(s: &mut Session) {
        match viewhole(s, &json!({})) {
            Ok(r) => {
                let holes = r["holes"].as_array().cloned().unwrap_or_default();
                if holes.is_empty() {
                    s.echo("No holes in this view");
                }
                for h in holes {
                    s.echo(format!("{}  shows \"{}\"", h["boundary"].as_str().unwrap_or(""), h["sourceName"].as_str().unwrap_or("")));
                }
            }
            Err(e) => s.echo(e.to_string()),
        }
    }
}

impl Interactive for ViewHoleM {
    fn name(&self) -> &'static str {
        "VIEWHOLE"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if let Err(e) = edited_vertical(s) {
            s.echo(e.to_string());
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.phase {
            Phase::Boundary => Prompt::new("Select hole boundary", Accept::SELECT).kw(&["Remove", "List"]),
            Phase::Remove => Prompt::new("Select hole boundary to remove", Accept::SELECT),
            Phase::Source(_) => Prompt::new("Enter source view name", Accept::TEXT).kw(&["Nearest"]).default("Nearest".to_owned()),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        match self.phase {
            Phase::Boundary | Phase::Remove => match i {
                Input::Keyword(k) if self.phase == Phase::Boundary && k.eq_ignore_ascii_case("list") => {
                    Self::list(s);
                    Ok(Step::Done)
                }
                Input::Keyword(k) if self.phase == Phase::Boundary && k.eq_ignore_ascii_case("remove") => {
                    self.phase = Phase::Remove;
                    Ok(Step::Continue)
                }
                Input::Pick(hs) => {
                    let Some(h) = hs.first().copied() else { return Ok(Step::Continue) };
                    if self.phase == Phase::Remove {
                        Self::run(s, &json!({ "boundary": h.hex(), "remove": true }));
                        return Ok(Step::Done);
                    }
                    self.phase = Phase::Source(h);
                    Ok(Step::Continue)
                }
                Input::Enter => Ok(Step::Cancel),
                _ => Ok(Step::Continue),
            },
            Phase::Source(h) => {
                let text = match i {
                    Input::Enter => String::new(),
                    Input::Text(t) | Input::Keyword(t) => t.trim().to_owned(),
                    _ => return Ok(Step::Continue),
                };
                if text.is_empty() || text.eq_ignore_ascii_case("nearest") || text.eq_ignore_ascii_case("n") {
                    Self::run(s, &json!({ "boundary": h.hex() }));
                    return Ok(Step::Done);
                }
                let found = s
                    .doc()
                    .ok()
                    .and_then(|d| read_document(d).ok())
                    .and_then(|doc| doc.views.iter().find(|v| v.name().eq_ignore_ascii_case(&text)).map(lamb_core::model::AnyView::id));
                match found {
                    Some(id) => {
                        Self::run(s, &json!({ "boundary": h.hex(), "source": id.0 }));
                        Ok(Step::Done)
                    }
                    None => {
                        s.echo(format!("No view named \"{text}\""));
                        Ok(Step::Continue)
                    }
                }
            }
        }
    }
}

pub(crate) fn viewhole_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(ViewHoleM { phase: Phase::Boundary }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_render::{Options, build_plot};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    /// LAMBPROJECT's plan with sections A-A (0,0)->(5000,0), B-B (0,1000)->(5000,1000), C-C the
    /// other way at y = 3000 (mirrored to A-A) and D-D across them, (100,-500)->(100,4000). B-B
    /// and C-C each hold a line on `SRC` from (1000,500) to (4000,2000); A-A, edited, holds a closed
    /// rectangle (2000,0)..(3000,3000). Observers installed.
    struct Fx {
        s: Session,
        plan: Id,
        a: Id,
        b: Id,
        c: Id,
        d: Id,
        rect: Handle,
        src_b: Handle,
    }

    fn last_handle(s: &Session, view: Id) -> T<Handle> {
        Ok(s.doc()?.block(&view_block_name(view)).ok_or("no block")?.entities.iter().last().ok_or("empty")?.handle)
    }

    fn fixture() -> T<Fx> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no plan")?);
        let sec = |s: &mut Session, name: &str, from: [f64; 2], to: [f64; 2]| -> T<Id> {
            let r = s.execute(
                "newsectionline",
                &json!({ "name": name, "hostViewId": plan.0, "cutFrom": {"x": from[0], "y": from[1]}, "cutTo": {"x": to[0], "y": to[1]} }),
            )?;
            Ok(Id(r["viewId"].as_u64().ok_or("no section")?))
        };
        let a = sec(&mut s, "A-A", [0.0, 0.0], [5000.0, 0.0])?;
        let b = sec(&mut s, "B-B", [0.0, 1000.0], [5000.0, 1000.0])?;
        let c = sec(&mut s, "C-C", [5000.0, 3000.0], [0.0, 3000.0])?;
        let d = sec(&mut s, "D-D", [100.0, -500.0], [100.0, 4000.0])?;
        s.execute("layer.new", &json!({ "name": "SRC", "current": true }))?;
        for v in [c, b] {
            s.execute("editview", &json!({ "viewId": v.0 }))?;
            s.execute("line", &json!({ "points": [[1000, 500], [4000, 2000]] }))?;
        }
        let src_b = last_handle(&s, b)?;
        s.execute("layer.new", &json!({ "name": "HOLES", "current": true }))?;
        s.execute("editview", &json!({ "viewId": a.0 }))?;
        s.execute("pline", &json!({ "vertices": [[2000, 0], [3000, 0], [3000, 3000], [2000, 3000]], "closed": true }))?;
        let rect = last_handle(&s, a)?;
        Ok(Fx { s, plan, a, b, c, d, rect, src_b })
    }

    fn lines(s: &Session, view: Id) -> T<Vec<(String, Vec2, Vec2)>> {
        let Some(b) = s.doc()?.block(&hole_block_name(view)).cloned() else { return Ok(Vec::new()) };
        b.entities
            .iter()
            .map(|e| match &e.kind {
                EntityKind::Line(l) => Ok((e.common.layer.clone(), l.a.xy(), l.b.xy())),
                _ => Err("not a line".into()),
            })
            .collect()
    }

    fn close(p: Vec2, x: f64, y: f64) -> bool {
        (p.x - x).abs() < 1e-6 && (p.y - y).abs() < 1e-6
    }

    fn undo_len(s: &Session) -> T<usize> {
        Ok(s.state()?.undo.len())
    }

    fn hole_of(s: &Session, h: Handle) -> T<EntityData> {
        Ok(read_entity_data(&s.doc()?.entity(h).ok_or("no boundary")?.common)?)
    }

    #[test]
    fn clip_keeps_the_inside_part_of_a_segment() {
        let sq = [Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0), Vec2::new(0.0, 10.0)];
        let r = clip_segment(Vec2::new(-5.0, 2.0), Vec2::new(15.0, 7.0), &sq);
        assert_eq!(r.len(), 1);
        assert!(close(r[0].0, 0.0, 3.25) && close(r[0].1, 10.0, 5.75), "{r:?}");
        assert!(clip_segment(Vec2::new(-5.0, -2.0), Vec2::new(15.0, -1.0), &sq).is_empty());
        let r = clip_segment(Vec2::new(2.0, 2.0), Vec2::new(3.0, 4.0), &sq);
        assert!(r.len() == 1 && close(r[0].0, 2.0, 2.0) && close(r[0].1, 3.0, 4.0));
        // A U shape: the segment crosses the notch and comes out as two pieces.
        let u = [
            Vec2::new(0.0, 0.0),
            Vec2::new(9.0, 0.0),
            Vec2::new(9.0, 9.0),
            Vec2::new(6.0, 9.0),
            Vec2::new(6.0, 3.0),
            Vec2::new(3.0, 3.0),
            Vec2::new(3.0, 9.0),
            Vec2::new(0.0, 9.0),
        ];
        let r = clip_segment(Vec2::new(-1.0, 6.0), Vec2::new(10.0, 6.0), &u);
        assert_eq!(r.len(), 2, "{r:?}");
        assert!(close(r[0].0, 0.0, 6.0) && close(r[0].1, 3.0, 6.0) && close(r[1].0, 6.0, 6.0) && close(r[1].1, 9.0, 6.0), "{r:?}");
    }

    #[test]
    fn a_hole_shows_the_nearest_parallel_section_clipped_in_one_undo_step() -> R {
        let mut f = fixture()?;
        let n = undo_len(&f.s)?;
        let r = f.s.execute("viewhole", &json!({ "boundary": f.rect.hex() }))?;
        assert_eq!(r["source"], f.b.0, "{r}");
        assert_eq!(r["message"], "Hole shows \"B-B\"");
        assert_eq!(undo_len(&f.s)?, n + 1);
        let l = lines(&f.s, f.a)?;
        assert_eq!(l.len(), 1, "{l:?}");
        assert_eq!(l[0].0, "SRC");
        assert!(close(l[0].1, 2000.0, 1000.0) && close(l[0].2, 3000.0, 1500.0), "{l:?}");
        let ins: Vec<String> =
            f.s.doc()?
                .model
                .iter()
                .filter(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == hole_block_name(f.a)))
                .map(|e| e.common.layer.clone())
                .collect();
        assert_eq!(ins, [view_block_name(f.a)]);
        let data = hole_of(&f.s, f.rect)?;
        assert_eq!((data.role, data.hole), (LineRole::Reference, Some(HoleTag { source: f.b })));
        let listed = f.s.execute("viewhole", &json!({}))?;
        assert_eq!(listed["holes"], json!([{ "boundary": f.rect.hex(), "source": f.b.0, "sourceName": "B-B" }]));

        f.s.undo()?;
        assert!(lines(&f.s, f.a)?.is_empty());
        assert!(hole_of(&f.s, f.rect)?.hole.is_none());
        Ok(())
    }

    #[test]
    fn a_mirrored_source_maps_with_u_flipped() -> R {
        let mut f = fixture()?;
        f.s.execute("viewhole", &json!({ "boundary": f.rect.hex(), "source": f.c.0 }))?;
        let l = lines(&f.s, f.a)?;
        assert_eq!(l.len(), 1, "{l:?}");
        // C-C's (1000,500)->(4000,2000) is A-A's (4000,500)->(1000,2000), clipped to x 2000..3000.
        assert!(close(l[0].1, 3000.0, 1000.0) && close(l[0].2, 2000.0, 1500.0), "{l:?}");
        Ok(())
    }

    #[test]
    fn a_source_edit_regenerates_in_its_undo_step_and_remove_empties() -> R {
        let mut f = fixture()?;
        f.s.execute("viewhole", &json!({ "boundary": f.rect.hex() }))?;
        let first = lines(&f.s, f.a)?;
        f.s.execute("editview", &json!({ "viewId": f.b.0 }))?;
        let n = undo_len(&f.s)?;
        f.s.execute("move", &json!({ "handles": [f.src_b.hex()], "delta": [0, 300] }))?;
        assert_eq!(undo_len(&f.s)?, n + 1);
        let moved = lines(&f.s, f.a)?;
        assert!(moved.len() == 1 && close(moved[0].1, 2000.0, 1300.0) && close(moved[0].2, 3000.0, 1800.0), "{moved:?}");
        f.s.undo()?;
        assert_eq!(lines(&f.s, f.a)?, first);

        f.s.execute("editview", &json!({ "viewId": f.a.0 }))?;
        let r = f.s.execute("viewhole", &json!({ "boundary": f.rect.hex(), "remove": true }))?;
        assert_eq!(r["removed"], true);
        assert!(lines(&f.s, f.a)?.is_empty());
        assert_eq!(f.s.execute("viewhole", &json!({}))?["holes"], json!([]));
        Ok(())
    }

    #[test]
    fn bad_boundaries_plans_and_non_parallel_sources_refuse_without_an_undo_step() -> R {
        let mut f = fixture()?;
        f.s.execute("pline", &json!({ "vertices": [[0, 0], [500, 0], [500, 500]] }))?;
        let open = last_handle(&f.s, f.a)?;
        f.s.execute("pline", &json!({ "vertices": [[0, 0], [500, 0], [500, 500]], "closed": true }))?;
        let bulged = last_handle(&f.s, f.a)?;
        f.s.doc_mut()?.modify_entity(bulged, |e| {
            if let EntityKind::LwPolyline(p) = &mut e.kind {
                p.vertices[0].bulge = 0.5;
            }
        })?;
        let n = undo_len(&f.s)?;
        for h in [open, bulged] {
            let e = f.s.execute("viewhole", &json!({ "boundary": h.hex() })).err().ok_or("accepted")?;
            assert!(e.to_string().contains("A hole boundary must be a closed straight-edged polyline"), "{e}");
        }
        let e = f.s.execute("viewhole", &json!({ "boundary": f.rect.hex(), "source": f.d.0 })).err().ok_or("accepted D-D")?;
        assert!(e.to_string().contains("D-D is not a parallel placed section or elevation"), "{e}");
        assert_eq!(undo_len(&f.s)?, n);
        f.s.execute("editview", &json!({ "viewId": f.plan.0 }))?;
        let n = undo_len(&f.s)?;
        let e = f.s.execute("viewhole", &json!({})).err().ok_or("accepted a plan")?;
        assert!(e.to_string().contains("VIEWHOLE works in a placed section or elevation"), "{e}");
        assert_eq!(undo_len(&f.s)?, n);
        Ok(())
    }

    #[test]
    fn the_boundary_makes_no_link_or_marker_and_a_sheet_plots_the_revealed_lines() -> R {
        let mut f = fixture()?;
        f.s.execute("viewhole", &json!({ "boundary": f.rect.hex() }))?;
        let d = f.s.doc()?.clone();
        let mut bare = d.clone();
        let name = view_block_name(f.a);
        let mut blk = (**bare.block(&name).ok_or("no block")?).clone();
        blk.entities.remove(f.rect);
        bare.blocks.insert(name, Arc::new(blk));
        let all = |p: &DrawingProject| -> String {
            let ids: Vec<Id> = p.views().map(|v| v.id).collect();
            ids.iter().map(|id| format!("{:?}", lamb_core::derivation::markers::compute_markers_for_view(p, *id))).collect()
        };
        assert_eq!(all(&DrawingProject::new(&d)?), all(&DrawingProject::new(&bare)?));
        let dp = DrawingProject::new(&d)?;
        assert!(!dp.document().links.iter().any(|l| l.a.entity_id.0 == f.rect.0 || l.b.entity_id.0 == f.rect.0));
        drop(dp);

        f.s.execute("newsheet", &json!({ "name": "A301", "viewIds": [f.a.0] }))?;
        let count = |s: &Session| -> T<usize> { Ok(build_plot(s.doc()?, &Space::Paper("A301".into()), &Options::default()).prims.len()) };
        let with = count(&f.s)?;
        f.s.execute("viewhole", &json!({ "boundary": f.rect.hex(), "remove": true }))?;
        assert_eq!(count(&f.s)?, with - 1, "the one revealed line plots");
        Ok(())
    }

    #[test]
    fn interactive_pick_then_enter_tags_the_nearest_view() -> R {
        let mut f = fixture()?;
        f.s.start("vhole")?;
        assert_eq!(f.s.current_prompt().map(|p| p.display()).as_deref(), Some("Select hole boundary or [Remove/List]:"));
        f.s.input(Input::Pick(vec![f.rect]))?;
        assert_eq!(f.s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter source view name or [Nearest] <Nearest>:"));
        f.s.input(Input::Enter)?;
        assert!(f.s.running.is_none());
        assert!(f.s.log.iter().any(|l| l == "Hole shows \"B-B\""), "{:?}", f.s.log);
        assert_eq!(hole_of(&f.s, f.rect)?.hole, Some(HoleTag { source: f.b }));
        f.s.start("viewhole")?;
        f.s.input(Input::Keyword("List".into()))?;
        assert!(f.s.log.iter().any(|l| l == &format!("{}  shows \"B-B\"", f.rect.hex())), "{:?}", f.s.log);
        Ok(())
    }
}

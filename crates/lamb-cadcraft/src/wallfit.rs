//! WALLFIT: close a plan's or section's width conflicts by moving a conflicted straight run's faces so the drawn
//! wall is as thick as its detail. A [`Hold`] chooses which face stays put: the detail's reference
//! face (the default), the exterior face, the interior face, or the centre (both faces move by half
//! the delta). A moving face moves along the run's normal toward its own side by
//! [`WidthConflict::delta`]. A LINE face moves both ends; an LWPOLYLINE face moves its segment's two vertices, so the
//! neighbouring segments stretch with it (a rectangle stays a rectangle, and two faces sharing a
//! corner vertex compose). Arc runs carry no conflict yet and are never fitted. Two runs sharing
//! one face segment (either side of a T) move it once. Every other wall-run face line whose free
//! end (a LINE end, or an open LWPOLYLINE's first or last vertex) lay on a moved face segment has
//! that end extended or trimmed along its own line to meet the moved face (T478). Every line in a
//! vertical view linked to a moved face segment travels with it in the same undo step (T484); in a
//! section, every linked plan line travels the same way. In a section the exterior side comes from
//! its wall sides (`outsidepoint`) exactly as for its assembly. While the Hold prompt is up, the
//! interactive form previews the reference hold's moved face segments as LINEs (T488).

use cadcraft_doc::{Drawing, EntityKind, Handle, Line};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result as EngineResult, Session, Step};
use cadcraft_geom::Vec2;
use lamb_core::derivation::assembly::{FaceRef, WidthConflict};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::derivation::travel::travel_segment_move;
use lamb_core::derivation::wall_spaces::outside_on_left;
use lamb_core::derivation::walls::RunCurve;
use lamb_core::geometry::intersect::infinite_line_intersect;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::geometry::{Point2, Polyline2};
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::assembly::{assembly_lines, check_wall_view};
use crate::commands::{id_value, other};
use crate::detailtags::{edited_wall_view, graph_and_sides};
use crate::maintain::write_travelled;
use crate::project::DrawingProject;

/// Which face of a fitted run stays put.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// The detail's reference face holds; the other face moves.
    Reference,
    /// The exterior face holds.
    Exterior,
    /// The interior face holds.
    Interior,
    /// Both faces move by half the delta.
    Centre,
}

const HOLD_VALUES: &str = "hold must be reference, exterior, interior or centre";

impl Hold {
    /// `reference` / `exterior` / `interior` / `centre`.
    pub fn name(self) -> &'static str {
        match self {
            Hold::Reference => "reference",
            Hold::Exterior => "exterior",
            Hold::Interior => "interior",
            Hold::Centre => "centre",
        }
    }

    fn parse(v: &str) -> Result<Hold, String> {
        match v {
            "reference" => Ok(Hold::Reference),
            "exterior" => Ok(Hold::Exterior),
            "interior" => Ok(Hold::Interior),
            "centre" => Ok(Hold::Centre),
            "grid" => Err("Grid hold is not available yet".to_string()),
            _ => Err(HOLD_VALUES.to_string()),
        }
    }

    /// The faces that move for conflict `c`, each with the amount it moves toward its own side.
    fn moves(self, c: &WidthConflict) -> Vec<(FaceRef, f64)> {
        let held = match self {
            Hold::Reference => opposite(c.toward),
            Hold::Exterior => FaceRef::Exterior,
            Hold::Interior => FaceRef::Interior,
            Hold::Centre => return vec![(FaceRef::Exterior, c.delta / 2.0), (FaceRef::Interior, c.delta / 2.0)],
        };
        vec![(opposite(held), c.delta)]
    }
}

fn opposite(f: FaceRef) -> FaceRef {
    match f {
        FaceRef::Exterior => FaceRef::Interior,
        FaceRef::Interior => FaceRef::Exterior,
    }
}

/// One face segment to move: the entity, its segment index, and the displacement.
struct Move {
    handle: Handle,
    segment: usize,
    dx: f64,
    dy: f64,
}

/// The midpoint of segment `segment` of entity `h` (a LINE's only segment, or an LWPOLYLINE's).
fn segment_mid(d: &Drawing, h: Handle, segment: usize) -> Result<(f64, f64), String> {
    let e = d.entity(h).ok_or_else(|| format!("face entity {:X} not found", h.0))?;
    match &e.kind {
        EntityKind::Line(l) => Ok(((l.a.x + l.b.x) / 2.0, (l.a.y + l.b.y) / 2.0)),
        EntityKind::LwPolyline(p) => {
            let n = p.vertices.len();
            let next = if segment + 1 < n {
                segment + 1
            } else if p.closed {
                0
            } else {
                return Err(format!("face entity {:X} has no segment {segment}", h.0));
            };
            let (a, b) = (p.vertices.get(segment).ok_or_else(|| format!("face entity {:X} has no segment {segment}", h.0))?.p, p.vertices[next].p);
            Ok(((a.x + b.x) / 2.0, (a.y + b.y) / 2.0))
        }
        _ => Err(format!("face entity {:X} is neither a LINE nor an LWPOLYLINE", h.0)),
    }
}

/// A follower's free end moved to meet a moved face: LINE vertex 0 = `a`, 1 = `b`; LWPOLYLINE the
/// vertex index.
struct EndMove {
    handle: Handle,
    vertex: usize,
    to: Point2,
}

/// Everything one WALLFIT does: the fitted run indices, the face moves (one per face segment) and
/// the end follow-ups.
struct Plan {
    fitted: Vec<usize>,
    moves: Vec<Move>,
    ends: Vec<EndMove>,
}

/// The straight segment `segment` of entity `h` as `(start, end)`, with each end's vertex index and
/// whether it is free (a LINE end, or the first/last vertex of an open LWPOLYLINE).
fn segment_ends(d: &Drawing, h: Handle, segment: usize) -> Option<[(Point2, usize, bool); 2]> {
    let e = d.entity(h)?;
    match &e.kind {
        EntityKind::Line(l) => Some([(Point2 { x: l.a.x, y: l.a.y }, 0, true), (Point2 { x: l.b.x, y: l.b.y }, 1, true)]),
        EntityKind::LwPolyline(p) => {
            let n = p.vertices.len();
            let next = if segment + 1 < n {
                segment + 1
            } else if p.closed {
                0
            } else {
                return None;
            };
            let (a, b) = (p.vertices.get(segment)?.p, p.vertices.get(next)?.p);
            let free = |i: usize| !p.closed && (i == 0 || i + 1 == n);
            Some([(Point2 { x: a.x, y: a.y }, segment, free(segment)), (Point2 { x: b.x, y: b.y }, next, free(next))])
        }
        _ => None,
    }
}

/// Whether `q` lies on segment `a→b` (inside its extent) within `TOLERANCE.coincidence_mm`.
fn on_segment(q: Point2, a: Point2, b: Point2) -> bool {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    if len2 <= 0.0 {
        return false;
    }
    let t = ((q.x - a.x) * dx + (q.y - a.y) * dy) / len2;
    let (px, py) = (a.x + t * dx, a.y + t * dy);
    let tol = TOLERANCE.coincidence_mm;
    let ext = tol / len2.sqrt();
    (-ext..=1.0 + ext).contains(&t) && (q.x - px).hypot(q.y - py) <= tol
}

fn apply_end(d: &mut Drawing, m: &EndMove) -> Result<(), String> {
    let (to, v) = (m.to, m.vertex);
    d.modify_entity(m.handle, |e| match &mut e.kind {
        EntityKind::Line(l) => {
            let p = if v == 0 { &mut l.a } else { &mut l.b };
            p.x = to.x;
            p.y = to.y;
        }
        EntityKind::LwPolyline(p) => {
            if let Some(vx) = p.vertices.get_mut(v) {
                vx.p.x = to.x;
                vx.p.y = to.y;
            }
        }
        _ => {}
    })
    .map_err(|e| e.to_string())
}

fn apply(d: &mut Drawing, m: &Move) -> Result<(), String> {
    let (dx, dy, seg) = (m.dx, m.dy, m.segment);
    d.modify_entity(m.handle, |e| match &mut e.kind {
        EntityKind::Line(l) => {
            l.a.x += dx;
            l.a.y += dy;
            l.b.x += dx;
            l.b.y += dy;
        }
        EntityKind::LwPolyline(p) => {
            let n = p.vertices.len();
            let next = if seg + 1 < n { seg + 1 } else { 0 };
            for i in [seg, next] {
                p.vertices[i].p.x += dx;
                p.vertices[i].p.y += dy;
            }
        }
        _ => {}
    })
    .map_err(|e| e.to_string())
}

/// Moves the non-reference face of every conflicted straight run of `plan` (only those in `only`,
/// when given) by its conflict's delta. Returns the fitted run indices, ascending. Every face is
/// checked before anything moves; on error the drawing is untouched.
pub fn wall_fit(d: &mut Drawing, plan: Id, only: Option<&[usize]>) -> Result<Vec<usize>, String> {
    wall_fit_holding(d, plan, only, Hold::Reference)
}

/// [`wall_fit`] with `hold` choosing which face of every fitted run stays put.
pub fn wall_fit_holding(d: &mut Drawing, plan: Id, only: Option<&[usize]>, hold: Hold) -> Result<Vec<usize>, String> {
    wall_fit_travelling(d, plan, only, hold).map(|(fitted, _)| fitted)
}

/// [`wall_fit_holding`], returning also the entity ids of the linked partners that travelled with
/// their moved faces.
fn wall_fit_travelling(d: &mut Drawing, plan: Id, only: Option<&[usize]>, hold: Hold) -> Result<(Vec<usize>, Vec<Id>), String> {
    let plan = plan_moves(d, plan, only, hold)?;
    for m in &plan.moves {
        apply(d, m)?;
    }
    for e in &plan.ends {
        apply_end(d, e)?;
    }
    let writes = linked_travel(d, &plan.moves)?;
    write_travelled(d, &writes).map_err(|e| e.to_string())?;
    Ok((plan.fitted, writes.into_iter().map(|(id, _)| id).collect()))
}

/// The new geometry of every partner linked to a moved face segment, slid by that segment's move
/// (`travel_segment_move`) on the already-moved drawing `d`. Each partner travels at most once
/// (first move, then `doc.links` order); a partner that is itself a moved face, or an unaccepted AI
/// line, never travels. A partial slide (`unreachable`) is still returned; the link re-check
/// decides stale.
fn linked_travel(d: &Drawing, moves: &[Move]) -> Result<Vec<(Id, Polyline2)>, String> {
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let mut writes: Vec<(Id, Polyline2)> = Vec::new();
    for m in moves {
        let id = Id(m.handle.0);
        for link in &p.document().links {
            for (mine, other) in [(&link.a, &link.b), (&link.b, &link.a)] {
                if mine.entity_id != id || mine.segment_index != m.segment {
                    continue;
                }
                if moves.iter().any(|o| Id(o.handle.0) == other.entity_id) || writes.iter().any(|(w, _)| *w == other.entity_id) {
                    continue;
                }
                if p.entity(other.entity_id).and_then(|e| e.ai_origin).is_some_and(|o| !o.accepted) {
                    continue;
                }
                let Some(t) = travel_segment_move(&p, link, mine, Point2 { x: m.dx, y: m.dy })? else { continue };
                if t.entity_id == other.entity_id {
                    writes.push((t.entity_id, t.geometry));
                }
            }
        }
    }
    Ok(writes)
}

/// The face moves and end follow-ups [`wall_fit_holding`] would make, with the fitted run indices,
/// computed without mutating.
fn plan_moves(d: &Drawing, view: Id, only: Option<&[usize]>, hold: Hold) -> Result<Plan, String> {
    check_wall_view(d, view)?;
    let runs = assembly_lines(d, view)?;
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let (graph, sides) = graph_and_sides(&p, view);
    let mut moves = Vec::new();
    for r in &runs {
        let Some(c) = r.conflict else { continue };
        if only.is_some_and(|o| !o.contains(&r.run)) {
            continue;
        }
        let (run, side) = (&graph.runs[r.run], &sides.runs[r.run]);
        let RunCurve::Line { from, to } = run.centreline else { continue };
        let Some(outside_left) = outside_on_left(side) else { continue };
        let (dx, dy) = (to.x - from.x, to.y - from.y);
        let len = dx.hypot(dy);
        for (face, amount) in hold.moves(&c) {
            let target_left = outside_left == (face == FaceRef::Exterior);
            let mut found = None;
            for f in &run.faces {
                let h = Handle(f.from.entity_id.0);
                let (mx, my) = segment_mid(d, h, f.from.segment_index)?;
                let left = dx * (my - from.y) - dy * (mx - from.x) > 0.0;
                if left == target_left {
                    found = Some((h, f.from.segment_index));
                }
            }
            let Some((handle, segment)) = found else { return Err(format!("run {}: no face on the {} side", r.run, face.name())) };
            let s = if target_left { 1.0 } else { -1.0 };
            let (nx, ny) = (-dy / len * s, dx / len * s);
            moves.push((r.run, Move { handle, segment, dx: nx * amount, dy: ny * amount }));
        }
    }
    // One move per face segment: runs either side of a T share the through wall's segment.
    let tol = TOLERANCE.coincidence_mm;
    let mut fitted = Vec::new();
    let mut unique: Vec<(usize, Move)> = Vec::new();
    for (run, m) in moves {
        fitted.push(run);
        if let Some((other_run, u)) = unique.iter().find(|(_, u)| u.handle == m.handle && u.segment == m.segment) {
            if (u.dx - m.dx).hypot(u.dy - m.dy) > tol {
                return Err(format!("runs {other_run} and {run} disagree on face {:X}", m.handle.0));
            }
            continue;
        }
        unique.push((run, m));
    }
    fitted.dedup();
    let moves: Vec<Move> = unique.into_iter().map(|(_, m)| m).collect();

    // End follow-ups: a free end of any other run face lying on a moved segment meets the moved line.
    let shift = |h: Handle, seg: usize| moves.iter().find(|m| m.handle == h && m.segment == seg).map_or((0.0, 0.0), |m| (m.dx, m.dy));
    let mut ends: Vec<EndMove> = Vec::new();
    for m in &moves {
        let Some([(a, ..), (b, ..)]) = segment_ends(d, m.handle, m.segment) else { continue };
        let (na, nb) = (Point2 { x: a.x + m.dx, y: a.y + m.dy }, Point2 { x: b.x + m.dx, y: b.y + m.dy });
        for run in &graph.runs {
            for f in &run.faces {
                let (h, seg) = (Handle(f.from.entity_id.0), f.from.segment_index);
                if h == m.handle && seg == m.segment {
                    continue;
                }
                let Some(fe) = segment_ends(d, h, seg) else { continue };
                let (sx, sy) = shift(h, seg);
                let (p0, p1) = (Point2 { x: fe[0].0.x + sx, y: fe[0].0.y + sy }, Point2 { x: fe[1].0.x + sx, y: fe[1].0.y + sy });
                for &(q, vertex, free) in &fe {
                    if !free || ends.iter().any(|e| e.handle == h && e.vertex == vertex) || !on_segment(q, a, b) {
                        continue;
                    }
                    if let Some(hit) = infinite_line_intersect(p0, p1, na, nb) {
                        ends.push(EndMove { handle: h, vertex, to: hit.point });
                    }
                }
            }
        }
    }
    Ok(Plan { fitted, moves, ends })
}

/// WALLFIT `{viewId, runs?: [index], hold?}`: [`wall_fit_holding`] in one undo step, `hold` one of
/// `reference` (the default), `exterior`, `interior` or `centre`. Returns `{fitted: [index], hold}`;
/// nothing to fit records no undo step.
pub fn wallfit(s: &mut Session, p: &Value) -> EngineResult<Value> {
    let plan = p.get("viewId").and_then(id_value).ok_or_else(|| other("No such view"))?;
    let only: Option<Vec<usize>> = match p.get("runs") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => {
            Some(a.iter().map(|v| v.as_u64().map(|n| n as usize).ok_or_else(|| other("runs must be run indices"))).collect::<EngineResult<_>>()?)
        }
        Some(_) => return Err(other("runs must be run indices")),
    };
    let hold = match p.get("hold") {
        None | Some(Value::Null) => Hold::Reference,
        Some(Value::String(h)) => Hold::parse(h).map_err(other)?,
        Some(_) => return Err(other(HOLD_VALUES)),
    };
    // Validate before `doc_mut`: touching the drawing on a refusal would still record an undo step.
    let planned = plan_moves(s.doc()?, plan, only.as_deref(), hold).map_err(other)?;
    if planned.moves.is_empty() {
        s.echo("No width conflicts".to_string());
        return Ok(json!({ "fitted": [], "hold": hold.name(), "travelled": [] }));
    }
    let (fitted, travelled) = wall_fit_travelling(s.doc_mut()?, plan, only.as_deref(), hold).map_err(other)?;
    let moved = if travelled.is_empty() { String::new() } else { format!(", {} linked lines moved", travelled.len()) };
    s.echo(format!("{} runs fitted, {} holds{moved}", fitted.len(), hold.name()));
    let travelled: Vec<Value> = travelled.iter().map(|id| json!(format!("{:X}", id.0))).collect();
    Ok(json!({ "fitted": fitted, "hold": hold.name(), "travelled": travelled }))
}

/// WALLFIT's interactive form, on the edited plan or section view: which face holds, then every
/// conflicted run is fitted. The Hold prompt previews the reference hold's moved face segments.
struct WallFitM;

const HOLD_WORDS: [&str; 4] = ["Reference", "Exterior", "Interior", "Centre"];

fn parse_hold_word(t: &str) -> Option<&'static str> {
    let t = t.trim();
    HOLD_WORDS.iter().find(|w| t.eq_ignore_ascii_case(w) || (t.len() == 1 && w[..1].eq_ignore_ascii_case(t))).map(|w| match *w {
        "Reference" => "reference",
        "Exterior" => "exterior",
        "Interior" => "interior",
        _ => "centre",
    })
}

impl Interactive for WallFitM {
    fn name(&self) -> &'static str {
        "wallfit"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Hold", Accept::TEXT).kw(&HOLD_WORDS).default("Reference".to_owned())
    }
    fn begin(&mut self, s: &mut Session) -> EngineResult<Step> {
        if let Err(e) = edited_wall_view(s) {
            s.echo(e);
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> EngineResult<Step> {
        let hold = match i {
            Input::Cancel => return Ok(Step::Cancel),
            Input::Enter => "reference",
            Input::Text(t) | Input::Keyword(t) => match parse_hold_word(&t) {
                Some(h) => h,
                None => {
                    s.echo("Reference, Exterior, Interior or Centre");
                    return Ok(Step::Continue);
                }
            },
            _ => return Ok(Step::Continue),
        };
        let view = edited_wall_view(s).map_err(other)?;
        if let Err(e) = wallfit(s, &json!({ "viewId": view.0, "hold": hold })) {
            s.echo(e.to_string());
        }
        Ok(Step::Done)
    }
    fn preview(&self, s: &Session, _cursor: Vec2) -> Vec<EntityKind> {
        let Ok(view) = edited_wall_view(s) else { return Vec::new() };
        let Ok(d) = s.doc() else { return Vec::new() };
        let Ok(plan) = plan_moves(d, view, None, Hold::Reference) else { return Vec::new() };
        plan.moves
            .iter()
            .filter_map(|m| {
                let [(a, ..), (b, ..)] = segment_ends(d, m.handle, m.segment)?;
                let shifted = |p: Point2| Vec2::new(p.x + m.dx, p.y + m.dy).to3(0.0);
                Some(EntityKind::Line(Line { a: shifted(a), b: shifted(b) }))
            })
            .collect()
    }
}

pub(crate) fn wallfit_machine(_s: &Session) -> EngineResult<Box<dyn Interactive>> {
    Ok(Box::new(WallFitM))
}

#[cfg(test)]
mod tests {
    use crate::assembly::tests::{room_with, undo_len};
    use cadcraft_doc::EntityKind;
    use cadcraft_engine::Session;
    use lamb_core::model::{AnyView, Id};
    use serde_json::json;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    /// Vertex lists of every LWPOLYLINE in the plan's view block, in block order.
    fn plines(s: &Session, plan: Id) -> T<Vec<Vec<(f64, f64)>>> {
        let d = s.doc()?;
        let b = d.block(&crate::schema::view_block_name(plan)).ok_or("no view block")?;
        Ok(b.entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::LwPolyline(p) => Some(p.vertices.iter().map(|v| ((v.p.x * 1e6).round() / 1e6, (v.p.y * 1e6).round() / 1e6)).collect()),
                _ => None,
            })
            .collect())
    }

    fn conflict_runs(s: &mut Session, plan: Id) -> T<Vec<u64>> {
        let r = s.execute("wallconflicts", &json!({ "viewId": plan.0 }))?;
        Ok(r["conflicts"].as_array().ok_or("no conflicts")?.iter().filter_map(|c| c["run"].as_u64()).collect())
    }

    #[test]
    fn wallfit_moves_every_inner_face_to_the_details_width_in_one_undo_step() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 210.0)?;
        let before = plines(&s, plan)?;
        assert_eq!(before.len(), 2);
        let n = undo_len(&s)?;
        let r = s.execute("wallfit", &json!({ "viewId": plan.0 }))?;
        assert_eq!(r, json!({ "fitted": [0, 1, 2, 3], "hold": "reference", "travelled": [] }));
        assert_eq!(undo_len(&s)?, n + 1);
        let after = plines(&s, plan)?;
        assert_eq!(after[0], before[0], "outer face unchanged");
        assert_eq!(after[1], [(200.0, 200.0), (4800.0, 200.0), (4800.0, 3800.0), (200.0, 3800.0)]);
        assert!(conflict_runs(&mut s, plan)?.is_empty());

        s.undo()?;
        assert_eq!(plines(&s, plan)?, before);
        assert_eq!(before[1], [(210.0, 210.0), (4790.0, 210.0), (4790.0, 3790.0), (210.0, 3790.0)]);
        Ok(())
    }

    #[test]
    fn wallfit_with_runs_moves_only_those_faces() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 210.0)?;
        let d = s.doc()?;
        let p = crate::project::DrawingProject::new(d)?;
        let (graph, _) = crate::detailtags::graph_and_sides(&p, plan);
        let bottom = graph
            .runs
            .iter()
            .position(|r| matches!(r.centreline, lamb_core::derivation::walls::RunCurve::Line { from, to } if from.y < 1000.0 && to.y < 1000.0))
            .ok_or("no bottom run")?;
        let r = s.execute("wallfit", &json!({ "viewId": plan.0, "runs": [bottom] }))?;
        assert_eq!(r, json!({ "fitted": [bottom], "hold": "reference", "travelled": [] }));
        assert_eq!(plines(&s, plan)?[1], [(210.0, 200.0), (4790.0, 200.0), (4790.0, 3790.0), (210.0, 3790.0)]);
        // The bottom run is now 200 mm and fits its detail. Detail resolution runs per (exterior,
        // width) class, so the three 210 mm runs no longer share the bottom run's tag: they are
        // untyped, not conflicted. Their faces did not move (the vertices above).
        assert!(conflict_runs(&mut s, plan)?.is_empty());
        let runs = crate::assembly::assembly_lines(s.doc()?, plan)?;
        assert!(runs.iter().all(|r| r.run == bottom || r.skipped.as_deref() == Some("untyped")), "{runs:?}");
        Ok(())
    }

    #[test]
    fn wallfit_with_nothing_to_fit_or_a_bad_view_records_nothing() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        let n = undo_len(&s)?;
        assert_eq!(s.execute("wallfit", &json!({ "viewId": plan.0 }))?, json!({ "fitted": [], "hold": "reference", "travelled": [] }));
        assert_eq!(undo_len(&s)?, n, "no undo step");

        let (mut s, plan) = room_with("exterior_face", true, 210.0)?;
        let before = plines(&s, plan)?;
        let n = undo_len(&s)?;
        let detail = crate::schema::read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id();
        let e = s.execute("wallfit", &json!({ "viewId": detail.0 })).err().ok_or("detail accepted")?;
        assert!(e.to_string().contains("Not a plan or section view"), "{e}");
        let e = s.execute("wallfit", &json!({ "viewId": 999_999 })).err().ok_or("unknown accepted")?;
        assert!(e.to_string().contains("No such view"), "{e}");
        assert_eq!(undo_len(&s)?, n);
        assert_eq!(plines(&s, plan)?, before);
        Ok(())
    }

    /// Runs WALLFIT on a fresh 210 mm room with `hold` and returns the outer and inner plines,
    /// checking one undo step and no conflicts left.
    fn fit_holding(hold: Option<&str>) -> T<(Vec<(f64, f64)>, Vec<(f64, f64)>)> {
        let (mut s, plan) = room_with("exterior_face", true, 210.0)?;
        let n = undo_len(&s)?;
        let mut p = json!({ "viewId": plan.0 });
        if let Some(h) = hold {
            p["hold"] = json!(h);
        }
        let r = s.execute("wallfit", &p)?;
        assert_eq!(r["hold"], json!(hold.unwrap_or("reference")));
        assert_eq!(r["fitted"], json!([0, 1, 2, 3]));
        assert_eq!(undo_len(&s)?, n + 1);
        assert!(conflict_runs(&mut s, plan)?.is_empty());
        let mut pl = plines(&s, plan)?;
        let inner = pl.pop().ok_or("no inner")?;
        let outer = pl.pop().ok_or("no outer")?;
        Ok((outer, inner))
    }

    #[test]
    fn wallfit_hold_chooses_which_face_stays_put() -> R {
        let outer0 = vec![(0.0, 0.0), (5000.0, 0.0), (5000.0, 4000.0), (0.0, 4000.0)];
        let inner0 = vec![(210.0, 210.0), (4790.0, 210.0), (4790.0, 3790.0), (210.0, 3790.0)];
        let inner_fit = vec![(200.0, 200.0), (4800.0, 200.0), (4800.0, 3800.0), (200.0, 3800.0)];
        for h in [None, Some("reference"), Some("exterior")] {
            assert_eq!(fit_holding(h)?, (outer0.clone(), inner_fit.clone()), "{h:?}");
        }
        assert_eq!(fit_holding(Some("interior"))?, (vec![(10.0, 10.0), (4990.0, 10.0), (4990.0, 3990.0), (10.0, 3990.0)], inner0));
        assert_eq!(
            fit_holding(Some("centre"))?,
            (
                vec![(5.0, 5.0), (4995.0, 5.0), (4995.0, 3995.0), (5.0, 3995.0)],
                vec![(205.0, 205.0), (4795.0, 205.0), (4795.0, 3795.0), (205.0, 3795.0)]
            )
        );
        Ok(())
    }

    #[test]
    fn wallfit_refuses_grid_and_unknown_holds_without_touching_the_drawing() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 210.0)?;
        let before = plines(&s, plan)?;
        let n = undo_len(&s)?;
        let e = s.execute("wallfit", &json!({ "viewId": plan.0, "hold": "grid" })).err().ok_or("grid accepted")?;
        assert!(e.to_string().contains("Grid hold is not available yet"), "{e}");
        let e = s.execute("wallfit", &json!({ "viewId": plan.0, "hold": "sideways" })).err().ok_or("sideways accepted")?;
        assert!(e.to_string().contains("hold must be reference, exterior, interior or centre"), "{e}");
        assert_eq!(undo_len(&s)?, n);
        assert_eq!(plines(&s, plan)?, before);
        Ok(())
    }
    #[test]
    fn the_interactive_wallfit_matches_the_parameter_form() -> R {
        use cadcraft_engine::Input;
        let (mut s, plan) = room_with("exterior_face", true, 210.0)?;
        let n = undo_len(&s)?;
        s.start("wallfit")?;
        assert_eq!(s.current_prompt().ok_or("no prompt")?.display(), "Hold or [Reference/Exterior/Interior/Centre] <Reference>:");
        s.input(Input::Text("bogus".into()))?;
        assert!(s.running.is_some(), "re-asks");
        assert!(s.log.iter().any(|l| l == "Reference, Exterior, Interior or Centre"), "{:?}", s.log);
        s.input(Input::Keyword("Exterior".into()))?;
        assert!(s.running.is_none());
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        let interactive = plines(&s, plan)?;

        let (mut p, plan2) = room_with("exterior_face", true, 210.0)?;
        p.execute("wallfit", &json!({ "viewId": plan2.0, "hold": "exterior" }))?;
        assert_eq!(plines(&p, plan2)?, interactive);

        let (mut s, plan) = room_with("exterior_face", true, 210.0)?;
        s.start("wallfit")?;
        s.input(Input::Enter)?;
        let (mut p, plan2) = room_with("exterior_face", true, 210.0)?;
        p.execute("wallfit", &json!({ "viewId": plan2.0 }))?;
        assert_eq!(plines(&s, plan)?, plines(&p, plan2)?, "Enter holds the reference");
        Ok(())
    }

    /// Endpoints of every LINE in the plan's view block, in block order, rounded to 1e-6.
    fn lines(s: &Session, plan: Id) -> T<Vec<((f64, f64), (f64, f64))>> {
        let d = s.doc()?;
        let b = d.block(&crate::schema::view_block_name(plan)).ok_or("no view block")?;
        let r = |v: f64| (v * 1e6).round() / 1e6;
        Ok(b.entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Line(l) => Some(((r(l.a.x), r(l.a.y)), (r(l.b.x), r(l.b.y)))),
                _ => None,
            })
            .collect())
    }

    /// `room_with(.., inner)` plus a partition of two WALL-EXT LINEs x = 2400 and x = 2600 from
    /// y = `y0` to y = `y1`, and any `extra` WALL-EXT LINEs.
    fn room_with_partition(inner: f64, y0: f64, y1: f64, extra: &[[[f64; 2]; 2]]) -> T<(Session, Id)> {
        let (mut s, plan) = room_with("exterior_face", true, inner)?;
        s.execute("layer.current", &json!({ "name": "WALL-EXT" }))?;
        for x in [2400.0, 2600.0] {
            s.execute("line", &json!({ "points": [[x, y0], [x, y1]] }))?;
        }
        for l in extra {
            s.execute("line", &json!({ "points": l }))?;
        }
        Ok((s, plan))
    }

    #[test]
    fn wallfit_extends_partition_face_lines_to_the_moved_face_and_moves_a_shared_segment_once() -> R {
        let (mut s, plan) = room_with_partition(210.0, 210.0, 3790.0, &[])?;
        let (pl0, ln0) = (plines(&s, plan)?, lines(&s, plan)?);
        assert_eq!(ln0, [((2400.0, 210.0), (2400.0, 3790.0)), ((2600.0, 210.0), (2600.0, 3790.0))]);
        let n = undo_len(&s)?;
        s.execute("wallfit", &json!({ "viewId": plan.0 }))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!(plines(&s, plan)?[1], [(200.0, 200.0), (4800.0, 200.0), (4800.0, 3800.0), (200.0, 3800.0)], "not shifted twice");
        assert_eq!(lines(&s, plan)?, [((2400.0, 200.0), (2400.0, 3800.0)), ((2600.0, 200.0), (2600.0, 3800.0))]);
        s.undo()?;
        assert_eq!(plines(&s, plan)?, pl0);
        assert_eq!(lines(&s, plan)?, ln0);
        Ok(())
    }

    #[test]
    fn wallfit_trims_partition_face_lines_when_the_face_moves_toward_them() -> R {
        let (mut s, plan) = room_with_partition(190.0, 190.0, 3810.0, &[])?;
        s.execute("wallfit", &json!({ "viewId": plan.0 }))?;
        assert_eq!(plines(&s, plan)?[1], [(200.0, 200.0), (4800.0, 200.0), (4800.0, 3800.0), (200.0, 3800.0)]);
        assert_eq!(lines(&s, plan)?, [((2400.0, 200.0), (2400.0, 3800.0)), ((2600.0, 200.0), (2600.0, 3800.0))]);
        Ok(())
    }

    #[test]
    fn wallfit_leaves_lines_off_the_face_and_moves_a_collinear_line_with_its_wall() -> R {
        // A second partition 1 mm short of the face at its bottom end, and a line lying along the
        // bottom inner face's line, touching it at its corner. A line parallel to a face can only
        // touch it collinearly, which makes it a face of the same wall: it moves with that wall's
        // face, and its corner end meets the moved side face — never a sideways jump.
        let extra = [[[3400.0, 211.0], [3400.0, 3790.0]], [[3600.0, 211.0], [3600.0, 3790.0]], [[4790.0, 210.0], [4900.0, 210.0]]];
        let (mut s, plan) = room_with_partition(210.0, 210.0, 3790.0, &extra)?;
        s.execute("wallfit", &json!({ "viewId": plan.0 }))?;
        let after = lines(&s, plan)?;
        assert_eq!(after[2].0, (3400.0, 211.0), "1 mm off: bottom end untouched");
        assert_eq!(after[3].0, (3600.0, 211.0), "1 mm off: bottom end untouched");
        assert_eq!(after[4], ((4800.0, 200.0), (4900.0, 200.0)), "collinear: moves with its wall");
        Ok(())
    }

    /// `room_with(.., 190)` (a 10 mm conflict) plus a section across the bottom wall at x = 2500
    /// (u = y + 1000) holding two vertical lines, at the outer face's u and the inner face's u, each
    /// linked to its face pline's bottom segment. Returns the session, plan and the two
    /// section lines' hex handles (outer, inner).
    fn room_with_linked_section() -> T<(Session, Id, String, String)> {
        let (mut s, plan) = room_with("exterior_face", true, 190.0)?;
        crate::maintain::install(&mut s);
        let level = Id(s.execute("level", &json!({ "mode": "create", "name": "L1", "floorElevationMm": 0 }))?["levelId"].as_u64().ok_or("no id")?);
        let t = json!({ "originX": 0.0, "originY": 0.0, "rotation": 0.0 });
        s.execute("placeview", &json!({ "viewId": plan.0, "kind": "plan", "levelId": level.0, "cutElevation": 1200, "transform": t }))?;
        let faces: Vec<String> = {
            let d = s.doc()?;
            let b = d.block(&crate::schema::view_block_name(plan)).ok_or("no view block")?;
            b.entities.iter().filter(|e| matches!(e.kind, EntityKind::LwPolyline(_))).map(|e| e.handle.hex()).collect()
        };
        let r = s.execute(
            "newsectionline",
            &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 2500, "y": -1000}, "cutTo": {"x": 2500, "y": 5000} }),
        )?;
        let sec = Id(r["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        let mut drawn = Vec::new();
        for (face, u, param) in [(&faces[0], 1000.0, 0.5), (&faces[1], 1190.0, (2500.0 - 190.0) / 4620.0)] {
            let handles = |s: &Session| -> T<Vec<String>> {
                Ok(s.doc()?.block(&crate::schema::view_block_name(sec)).ok_or("no section block")?.entities.iter().map(|e| e.handle.hex()).collect())
            };
            let before = handles(&s)?;
            s.execute("line", &json!({ "points": [[u, 0.0], [u, 3000.0]] }))?;
            // The sweep may add or remove AI lines in the same step: the new LINE at `u` is ours.
            let h = handles(&s)?.into_iter().find(|h| !before.contains(h) && us(&s, &[h]).is_ok_and(|x| x == [(u, u)])).ok_or("line not drawn")?;
            let a = json!({ "entityId": face, "segmentIndex": 0, "segmentParam": param });
            s.execute("link.apply", &json!({ "a": a, "b": { "entityId": h, "segmentIndex": 0, "segmentParam": 0.5 } }))?;
            drawn.push(h);
        }
        let inner = drawn.pop().ok_or("no inner")?;
        let outer = drawn.pop().ok_or("no outer")?;
        Ok((s, plan, outer, inner))
    }

    /// The u (local x) of each named section LINE's two ends, rounded to 1e-6. The section also
    /// holds the sweep's unaccepted AI lines, which these tests don't name.
    fn us(s: &Session, lines: &[&str]) -> T<Vec<(f64, f64)>> {
        let d = s.doc()?;
        let r = |v: f64| (v * 1e6).round() / 1e6;
        lines
            .iter()
            .map(|h| match d.entity(cadcraft_doc::Handle::parse_hex(h).ok_or("bad handle")?).map(|e| &e.kind) {
                Some(EntityKind::Line(l)) => Ok((r(l.a.x), r(l.b.x))),
                _ => Err(format!("{h} is not a LINE").into()),
            })
            .collect()
    }

    fn stale_links(s: &Session) -> T<usize> {
        Ok(crate::schema::read_document(s.doc()?)?.links.iter().filter(|l| l.stale == Some(true)).count())
    }

    #[test]
    fn wallfit_carries_a_moved_faces_linked_section_line_in_the_same_undo_step() -> R {
        let (mut s, plan, outer, inner) = room_with_linked_section()?;
        let (pl0, sl0) = (plines(&s, plan)?, us(&s, &[&outer, &inner])?);
        assert_eq!(sl0, [(1000.0, 1000.0), (1190.0, 1190.0)]);
        let links = crate::schema::read_document(s.doc()?)?.links.len();
        assert_eq!(stale_links(&s)?, 0);
        let n = undo_len(&s)?;
        let r = s.execute("wallfit", &json!({ "viewId": plan.0 }))?;
        assert_eq!(r["travelled"], json!([inner]), "{r}");
        assert!(s.log.iter().any(|l| l.ends_with(", 1 linked lines moved")), "{:?}", s.log);
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!(plines(&s, plan)?[1][0], (200.0, 200.0));
        assert_eq!(us(&s, &[&outer, &inner])?, [(1000.0, 1000.0), (1200.0, 1200.0)], "inner partner at the moved face's u; outer untouched");
        assert_eq!(crate::schema::read_document(s.doc()?)?.links.len(), links, "links survive");
        assert_eq!(stale_links(&s)?, 0, "no stale link");
        s.undo()?;
        assert_eq!(plines(&s, plan)?, pl0);
        assert_eq!(us(&s, &[&outer, &inner])?, sl0);
        Ok(())
    }

    #[test]
    fn wallfit_centre_hold_travels_both_faces_partners_by_half_the_delta() -> R {
        let (mut s, plan, outer, inner) = room_with_linked_section()?;
        let r = s.execute("wallfit", &json!({ "viewId": plan.0, "hold": "centre" }))?;
        assert_eq!(r["travelled"], json!([outer, inner]), "{r}");
        assert_eq!(us(&s, &[&outer, &inner])?, [(995.0, 995.0), (1195.0, 1195.0)]);
        assert_eq!(stale_links(&s)?, 0, "no stale link");

        let (mut s, plan, outer, inner) = room_with_linked_section()?;
        let r = s.execute("wallfit", &json!({ "viewId": plan.0, "hold": "interior" }))?;
        assert_eq!(r["travelled"], json!([outer]), "{r}");
        assert_eq!(us(&s, &[&outer, &inner])?, [(990.0, 990.0), (1190.0, 1190.0)], "the held interior face's partner is untouched");
        Ok(())
    }

    #[test]
    fn wallfit_fits_a_sections_conflicted_run_under_each_hold() -> R {
        let (mut s, sec) = crate::assembly::tests::section_wall(290.0)?;
        let before = plines(&s, sec)?;
        assert_eq!(before[1], [(290.0, 250.0), (3710.0, 250.0), (3710.0, 2750.0), (290.0, 2750.0)]);
        let n = undo_len(&s)?;
        let r = s.execute("wallfit", &json!({ "viewId": sec.0 }))?;
        assert_eq!(r["fitted"], json!([1, 3]), "both vertical runs carry SD's 10 mm conflict: {r}");
        assert_eq!(undo_len(&s)?, n + 1);
        let after = plines(&s, sec)?;
        assert_eq!(after[1], [(300.0, 250.0), (3700.0, 250.0), (3700.0, 2750.0), (300.0, 2750.0)], "inner faces move 10 mm inward, z unchanged");
        assert_eq!(after[0], before[0], "outer face holds at u 0");
        s.undo()?;
        assert_eq!(plines(&s, sec)?, before);

        let (mut s, sec) = crate::assembly::tests::section_wall(290.0)?;
        s.execute("wallfit", &json!({ "viewId": sec.0, "hold": "exterior" }))?;
        assert_eq!(plines(&s, sec)?[1][0], (300.0, 250.0), "exterior holds: the inner face moves");

        let (mut s, sec) = crate::assembly::tests::section_wall(290.0)?;
        s.execute("wallfit", &json!({ "viewId": sec.0, "hold": "interior" }))?;
        let pl = plines(&s, sec)?;
        assert_eq!(pl[0], [(-10.0, 0.0), (4010.0, 0.0), (4010.0, 3000.0), (-10.0, 3000.0)], "interior holds: the outer faces move out");
        assert_eq!(pl[1], before[1]);
        Ok(())
    }

    #[test]
    fn wallfit_in_a_section_travels_a_linked_plan_line() -> R {
        let (mut s, sec) = crate::assembly::tests::section_wall(290.0)?;
        crate::maintain::install(&mut s);
        let doc = crate::schema::read_document(s.doc()?)?;
        let plan = doc.views.iter().find_map(|v| match v {
            AnyView::Plan(p) => Some(p.id),
            _ => None,
        });
        let plan = plan.ok_or("no plan")?;
        let level = Id(s.execute("level", &json!({ "mode": "create", "name": "L1", "floorElevationMm": 0 }))?["levelId"].as_u64().ok_or("no id")?);
        let t = json!({ "originX": 0.0, "originY": 0.0, "rotation": 0.0 });
        s.execute("placeview", &json!({ "viewId": plan.0, "kind": "plan", "levelId": level.0, "cutElevation": 1200, "transform": t }))?;
        let inner = {
            let d = s.doc()?;
            let b = d.block(&crate::schema::view_block_name(sec)).ok_or("no section block")?;
            let pl: Vec<String> = b.entities.iter().filter(|e| matches!(e.kind, EntityKind::LwPolyline(_))).map(|e| e.handle.hex()).collect();
            pl.get(1).cloned().ok_or("no inner face")?
        };
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let handles = |s: &Session| -> T<Vec<String>> {
            Ok(s.doc()?.block(&crate::schema::view_block_name(plan)).ok_or("no plan block")?.entities.iter().map(|e| e.handle.hex()).collect())
        };
        let before = handles(&s)?;
        s.execute("line", &json!({ "points": [[290.0, 500.0], [290.0, 1500.0]] }))?;
        let h =
            handles(&s)?.into_iter().find(|h| !before.contains(h) && us(&s, &[h]).is_ok_and(|x| x == [(290.0, 290.0)])).ok_or("line not drawn")?;
        let a = json!({ "entityId": inner, "segmentIndex": 3, "segmentParam": 0.5 });
        s.execute("link.apply", &json!({ "a": a, "b": { "entityId": h, "segmentIndex": 0, "segmentParam": 0.5 } }))?;
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        let r = s.execute("wallfit", &json!({ "viewId": sec.0 }))?;
        assert_eq!(r["travelled"], json!([h]), "{r}");
        assert_eq!(us(&s, &[&h])?, [(300.0, 300.0)], "the plan line follows the moved face");
        Ok(())
    }

    #[test]
    fn wallfit_machine_previews_the_reference_holds_moved_face_and_refuses_a_detail() -> R {
        let (mut s, _sec) = crate::assembly::tests::section_wall(290.0)?;
        s.start("wallfit")?;
        let pv = s.running.as_ref().ok_or("not running")?.machine.preview(&s, cadcraft_geom::Vec2::new(0.0, 0.0));
        let mut spans: Vec<(f64, f64, f64, f64)> = pv
            .iter()
            .map(|k| match k {
                EntityKind::Line(l) => Ok((l.a.x, l.b.x, l.a.y.min(l.b.y), l.a.y.max(l.b.y))),
                k => Err(format!("not a LINE: {k:?}")),
            })
            .collect::<Result<_, _>>()?;
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(spans, [(300.0, 300.0, 250.0, 2750.0), (3700.0, 3700.0, 250.0, 2750.0)], "one LINE per moved inner face segment");

        let doc = crate::schema::read_document(s.doc()?)?;
        let sd = doc.views.iter().find_map(|v| match v {
            AnyView::Detail(d) => Some(d.id),
            _ => None,
        });
        let sd = sd.ok_or("no detail")?;
        s.running = None;
        s.execute("editview", &json!({ "viewId": sd.0 }))?;
        let n = undo_len(&s)?;
        s.log.clear();
        s.start("wallfit")?;
        assert!(s.running.is_none());
        assert!(s.log.iter().any(|l| l.contains("plan and section views")), "{:?}", s.log);
        assert_eq!(undo_len(&s)?, n);
        Ok(())
    }
}

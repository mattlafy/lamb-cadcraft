//! The AI-line / solid-cut sweeps over a drawing (`project_mut`, `sweeps`):
//! - parity: every `reconcileAiLines` / `reconcileSolidCutLines` golden case, imported with
//!   `project_to_drawing`, reconciles through `DrawingProjectMut` to the same counts and the same
//!   per-view polylines as lamb-core on the `Project`;
//! - session: MOVE of an AI line's source moves the AI line in the same command, one UNDO restores
//!   both; ERASE of an unaccepted AI line suppresses its slot, so the next command doesn't
//!   re-propose it.
//!
//! The command registry is process-global, so registration goes through `install_app`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use cadcraft_doc::{Drawing, Handle};
use cadcraft_engine::Session;
use lamb_cadcraft::{DrawingProject, DrawingProjectMut, IdMap, commands, install, project_to_drawing, read_document, run_sweeps, write_document};
use lamb_core::derivation::proposals::{ReconcileResult, reconcile_ai_lines};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::derivation::solid_cut_lines::reconcile_solid_cut_lines;
use lamb_core::geometry::Polyline2;
use lamb_core::model::{AiOrigin, EntityGeometry, Id, Project, with_string_ids};
use lamb_core::persistence::files_to_project;
use serde_json::{Value, json};

type R = Result<(), Box<dyn std::error::Error>>;

const NOW: &str = "2026-01-01T00:00:00.000Z";

// ---------------------------------------------------------------------------------------------
// Parity with lamb-core on the golden cases
// ---------------------------------------------------------------------------------------------

/// The golden codec's argument decoding, as much of it as a `Project` needs.
fn decode(v: &Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.iter().map(decode).collect()),
        Value::Object(o) => {
            if o.get("$undefined") == Some(&Value::Bool(true)) {
                return Value::Null;
            }
            if let Some(Value::Array(items)) = o.get("$map").or_else(|| o.get("$set")) {
                return Value::Array(items.iter().map(decode).collect());
            }
            Value::Object(o.iter().map(|(k, v)| (k.clone(), decode(v))).collect())
        }
        _ => v.clone(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sweep {
    AiLines,
    SolidCuts,
}

fn run(sweep: Sweep, doc: &mut impl lamb_core::derivation::write::ProjectWrite, mint: &mut impl FnMut() -> Id) -> Result<ReconcileResult, String> {
    match sweep {
        Sweep::AiLines => reconcile_ai_lines(doc, NOW, mint),
        Sweep::SolidCuts => reconcile_solid_cut_lines(doc, mint),
    }
}

/// Counts that must agree: created, removed, updated, links created / removed, accepted, pruned.
fn counts(r: &ReconcileResult) -> [usize; 7] {
    [
        r.created_entities.len(),
        r.removed_entities.len(),
        r.updated_entities.len(),
        r.created_links.len(),
        r.removed_links.len(),
        r.accepted_by_divergence.len(),
        r.pruned_suppressions.len(),
    ]
}

/// A polyline as comparable text (coordinates rounded to 1e-6 mm).
fn key(p: &Polyline2) -> String {
    let r = |x: f64| format!("{:.6}", x + 0.0);
    let mut s = format!("{}({},{})", if p.closed { "C" } else { "O" }, r(p.start.x), r(p.start.y));
    for seg in &p.segments {
        s += &format!("->({},{} b{})", r(seg.to.x), r(seg.to.y), r(seg.bulge));
    }
    s
}

/// `p` as the drawing stores it (through an entity and back), so a polyline the import normalises
/// (a closed one whose last segment doesn't return to its start) compares equal.
fn round_trip(p: &Polyline2) -> Polyline2 {
    let (kind, _) = lamb_cadcraft::from_polyline(p);
    lamb_cadcraft::to_polyline(&cadcraft_doc::Entity::new(Handle(1), kind)).map_or_else(|_| p.clone(), |c| c.polyline)
}

/// Per view (old id), the sorted polyline keys of every polyline entity.
fn project_polylines(p: &Project) -> BTreeMap<Id, Vec<String>> {
    p.views
        .iter()
        .map(|v| {
            let mut keys: Vec<String> = v
                .entities()
                .iter()
                .filter_map(|e| match &e.geometry {
                    EntityGeometry::Polyline { geometry } => Some(key(&round_trip(geometry))),
                    _ => None,
                })
                .collect();
            keys.sort();
            (v.id(), keys)
        })
        .collect()
}

/// The same, read from the drawing and keyed by the old view id.
fn drawing_polylines(d: &Drawing, map: &IdMap, old_views: &[Id]) -> Result<BTreeMap<Id, Vec<String>>, Box<dyn std::error::Error>> {
    let p = DrawingProject::new(d)?;
    let mut out = BTreeMap::new();
    for old in old_views {
        let new = map.get(*old).ok_or("view not imported")?;
        let mut keys: Vec<String> = p.entities(new).filter_map(|e| e.polyline.map(|q| key(&q))).collect();
        keys.sort();
        out.insert(*old, keys);
    }
    Ok(out)
}

/// A slot key written with the fixture's string ids, rewritten with the imported handles: each
/// `:`-part, and each `,`-item of a solid-cut key's id list, that names an id is mapped through the
/// import (`strings`: id display string inside the fixture's interner → old id).
fn remap_key(k: &str, strings: &HashMap<String, Id>, map: &IdMap) -> String {
    let one = |s: &str| strings.get(s).and_then(|old| map.get(*old)).map_or_else(|| s.to_owned(), |new| new.0.to_string());
    k.split(':')
        .map(|part| {
            if part.contains(',') || strings.contains_key(part) {
                let mut items: Vec<String> = part.split(',').map(one).collect();
                items.sort();
                items.join(",")
            } else {
                part.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(":")
}

/// What the fixture's project looked like, decoded inside its own string-id interner, and what
/// lamb-core's sweep did to it there.
struct Expected {
    project: Project,
    after: Project,
    result: ReconcileResult,
    strings: HashMap<String, Id>,
}

fn expected(sweep: Sweep, arg: &Value) -> Result<Expected, String> {
    with_string_ids(|| {
        let project: Project = serde_json::from_value(decode(arg)).map_err(|e| e.to_string())?;
        let mut strings = HashMap::new();
        for v in &project.views {
            strings.insert(v.id().to_string(), v.id());
            for e in v.entities() {
                strings.insert(e.id.to_string(), e.id);
            }
        }
        let mut after = project.clone();
        let mut n = 1u64 << 40;
        let mut mint = || {
            n += 1;
            Id(n)
        };
        let result = run(sweep, &mut after, &mut mint)?;
        Ok(Expected { project, after, result, strings })
    })
}

/// Imports the case's project, rewrites its slot keys onto the new handles, and reconciles it
/// through `DrawingProjectMut`.
fn actual(sweep: Sweep, e: &Expected) -> Result<(Drawing, IdMap, ReconcileResult), Box<dyn std::error::Error>> {
    let (mut d, map, _) = project_to_drawing(&e.project)?;
    let mut doc = read_document(&d)?;
    doc.suppressed_proposals = doc.suppressed_proposals.iter().map(|k| remap_key(k, &e.strings, &map)).collect();
    write_document(&mut d, &doc)?;
    let ai: Vec<(Id, AiOrigin)> = {
        let p = DrawingProject::new(&d)?;
        p.views().flat_map(|v| p.entities(v.id).filter_map(|x| x.ai_origin.map(|o| (x.id, o.clone()))).collect::<Vec<_>>()).collect()
    };
    {
        use lamb_core::derivation::write::ProjectWrite;
        let mut m = DrawingProjectMut::new(&mut d)?;
        for (id, mut o) in ai {
            o.proposal_id = remap_key(&o.proposal_id, &e.strings, &map);
            m.set_ai_origin(id, Some(o));
        }
        m.finish()?;
    }
    let mut next = d.handseed;
    let mut mint = || {
        next += 1;
        Id(next)
    };
    let mut m = DrawingProjectMut::new(&mut d)?;
    let result = run(sweep, &mut m, &mut mint)?;
    m.finish()?;
    d.bump_handseed(Handle(next));
    Ok((d, map, result))
}

fn parity(file: &str, export: &str, sweep: Sweep) -> Result<usize, Box<dyn std::error::Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/golden/derivation-engine").join(file);
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let cases = fixture.get("cases").and_then(Value::as_array).ok_or("no cases")?;
    let mut checked = 0;
    for case in cases.iter().filter(|c| c.get("export").and_then(Value::as_str) == Some(export)) {
        let name = case.get("test").and_then(Value::as_str).unwrap_or("?");
        let arg = case.get("args").and_then(|a| a.get(0)).ok_or("no args")?;
        let e = expected(sweep, arg)?;
        let (d, map, got) = actual(sweep, &e)?;
        assert_eq!(counts(&got), counts(&e.result), "{name}: created/removed/updated/links/accepted/pruned counts");
        let views: Vec<Id> = e.project.views.iter().map(|v| v.id()).collect();
        assert_eq!(drawing_polylines(&d, &map, &views)?, project_polylines(&e.after), "{name}: per-view polylines after the sweep");
        checked += 1;
    }
    Ok(checked)
}

#[test]
fn reconcile_ai_lines_parity() -> R {
    let n = parity("proposals.json", "reconcileAiLines", Sweep::AiLines)?;
    assert!(n >= 8, "every reconcileAiLines case replayed ({n})");
    Ok(())
}

#[test]
fn reconcile_solid_cut_lines_parity() -> R {
    let n = parity("solidCutLines.json", "reconcileSolidCutLines", Sweep::SolidCuts)?;
    assert!(n >= 6, "every reconcileSolidCutLines case replayed ({n})");
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// In a session
// ---------------------------------------------------------------------------------------------

fn load(name: &str) -> Result<Project, Box<dyn std::error::Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb").join(format!("{name}.lamb.json"));
    let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(files_to_project(&files).map_err(|e| format!("{e:?}"))?)
}

/// The imported `u-wall-two-sections` after one sweep (its slot keys name the TS string ids, so
/// the first sweep re-proposes its AI lines under the imported handles), open in a session with
/// every LAMB observer.
fn session() -> Result<Session, Box<dyn std::error::Error>> {
    commands::install_app().map_err(|e| e.to_string())?;
    let (mut d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let before = d.clone();
    run_sweeps(&before, &mut d, NOW)?;
    let mut s = Session::new();
    s.open_drawing(d, "u-wall", None);
    install(&mut s);
    Ok(s)
}

/// (id, view, origin, polyline) of every AI line.
fn ai_lines(d: &Drawing) -> Result<Vec<(Id, Id, AiOrigin, Polyline2)>, Box<dyn std::error::Error>> {
    let p = DrawingProject::new(d)?;
    let mut out = Vec::new();
    for v in p.views() {
        for e in p.entities(v.id) {
            if let (Some(o), Some(poly)) = (e.ai_origin, e.polyline) {
                out.push((e.id, v.id, o.clone(), poly.into_owned()));
            }
        }
    }
    Ok(out)
}

fn polyline_of(d: &Drawing, id: Id) -> Result<Polyline2, Box<dyn std::error::Error>> {
    let p = DrawingProject::new(d)?;
    Ok(p.entity(id).and_then(|e| e.polyline.map(std::borrow::Cow::into_owned)).ok_or("no polyline")?)
}

#[test]
fn move_source_moves_ai_line_and_one_undo_restores_both() -> R {
    let mut s = session()?;
    let lines = ai_lines(s.doc()?)?;
    let (ai, _, origin, ai_before) =
        lines.iter().find(|l| !l.2.accepted && l.2.source_entity_id.is_some()).cloned().ok_or("no unaccepted AI line")?;
    let source = origin.source_entity_id.ok_or("no source")?;
    let source_view = DrawingProject::new(s.doc()?)?.entity_view_id(source).ok_or("source in no view")?;
    let source_before = polyline_of(s.doc()?, source)?;
    let doc_before = s.doc()?.clone();

    s.execute("editview", &json!({"viewId": source_view.0}))?;
    s.execute("move", &json!({"handles": [Handle(source.0).hex()], "delta": [300, 200]}))?;
    assert_ne!(polyline_of(s.doc()?, source)?, source_before, "the source moved");
    let after = ai_lines(s.doc()?)?;
    let moved = after.iter().find(|l| l.2.proposal_id == origin.proposal_id).ok_or("the AI line's slot is still filled")?;
    assert_ne!(moved.3, ai_before, "the AI line moved with its source in the same command");
    assert!(!moved.2.accepted, "and is still an unaccepted proposal (the sweep moved it; link travel leaves AI lines alone)");
    let links = read_document(s.doc()?)?.links;
    assert!(links.iter().all(|l| l.stale.is_none()), "no link left stale: {links:?}");

    s.undo()?;
    assert_eq!(polyline_of(s.doc()?, source)?, source_before, "one undo restores the source");
    assert_eq!(polyline_of(s.doc()?, ai)?, ai_before, "and the AI line");
    assert_eq!(read_document(s.doc()?)?, read_document(&doc_before)?, "and the LAMB document");
    Ok(())
}

#[test]
fn erase_suppresses_the_slot() -> R {
    let mut s = session()?;
    let lines = ai_lines(s.doc()?)?;
    let (ai, view, origin, _) = lines.iter().find(|l| !l.2.accepted).cloned().ok_or("no unaccepted AI line")?;
    assert!(!read_document(s.doc()?)?.suppressed_proposals.contains(&origin.proposal_id));

    s.execute("editview", &json!({"viewId": view.0}))?;
    s.execute("erase", &json!({"handles": [Handle(ai.0).hex()]}))?;
    assert!(s.doc()?.entity(Handle(ai.0)).is_none(), "erased");
    assert!(read_document(s.doc()?)?.suppressed_proposals.contains(&origin.proposal_id), "its slot is suppressed in the same command");
    let refilled =
        |s: &Session| -> Result<bool, Box<dyn std::error::Error>> { Ok(ai_lines(s.doc()?)?.iter().any(|l| l.2.proposal_id == origin.proposal_id)) };
    assert!(!refilled(&s)?, "not re-proposed by the erase's own sweep");

    // The next document-changing command: a line drawn in the same view, far from everything.
    s.execute("line", &json!({"points": [[-90000, -90000], [-85000, -90000]]}))?;
    assert!(!refilled(&s)?, "not re-proposed by the next command");

    s.undo()?;
    s.undo()?;
    assert!(s.doc()?.entity(Handle(ai.0)).is_some(), "two undos bring the AI line back");
    assert!(!read_document(s.doc()?)?.suppressed_proposals.contains(&origin.proposal_id), "and lift the suppression");
    Ok(())
}

/// The real face line the fixture's AI lines are implied from: a non-AI line in a vertical view
/// other than `ai_view`, and that view.
fn face_line(d: &Drawing, ai_view: Id) -> Result<(Id, Id), Box<dyn std::error::Error>> {
    let p = DrawingProject::new(d)?;
    let doc = read_document(d)?;
    let vertical = |id: Id| doc.views.iter().any(|v| v.id() == id && matches!(v, lamb_core::model::AnyView::Vertical(_)));
    for v in p.views().filter(|v| v.id != ai_view && vertical(v.id)) {
        if let Some(e) = p.entities(v.id).find(|e| e.ai_origin.is_none() && e.polyline.is_some()) {
            return Ok((e.id, v.id));
        }
    }
    Err("no face line".into())
}

/// Raises the real face line 500 mm (a z move in its section: nothing travels to the plan), so
/// the height every AI line is implied from changes. Returns whether `ai` moved.
fn raise_face_line(s: &mut Session, ai: Id, ai_view: Id) -> Result<bool, Box<dyn std::error::Error>> {
    let before = polyline_of(s.doc()?, ai)?;
    let (face, view) = face_line(s.doc()?, ai_view)?;
    s.execute("editview", &json!({"viewId": view.0}))?;
    s.execute("move", &json!({"handles": [Handle(face.0).hex()], "delta": [0, 500]}))?;
    Ok(polyline_of(s.doc()?, ai)? != before)
}

#[test]
fn accepted_ai_line_no_longer_follows_the_sweep() -> R {
    // Control: unaccepted, the sweep re-seats the AI line on the new height.
    let mut s = session()?;
    let lines = ai_lines(s.doc()?)?;
    let (ai, ai_view, origin, _) = lines.iter().find(|l| !l.2.accepted && l.2.source_entity_id.is_some()).cloned().ok_or("no unaccepted AI line")?;
    assert!(raise_face_line(&mut s, ai, ai_view)?, "control: an unaccepted AI line follows the height it is implied from");

    // Accepted: the same edit leaves it where it was.
    let mut s = session()?;
    let r = s.execute("accept", &json!({"handles": [Handle(ai.0).hex()]}))?;
    assert!(r["accepted"].as_u64().is_some_and(|n| n >= 1), "{r}");
    let now = ai_lines(s.doc()?)?;
    assert!(now.iter().find(|l| l.0 == ai).ok_or("the accepted line survived the accept's own sweep")?.2.accepted);
    let source = origin.source_entity_id.ok_or("no source")?;
    assert!(now.iter().filter(|l| l.2.source_entity_id == Some(source)).all(|l| l.2.accepted), "every AI line of that source is accepted");
    assert!(!raise_face_line(&mut s, ai, ai_view)?, "an accepted AI line is the user's: the sweep leaves it alone");
    Ok(())
}

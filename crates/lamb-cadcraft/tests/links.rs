//! The explicit link commands `link` / `unlink` on the imported `u-wall-two-sections` fixture: a
//! link between two crossing lines is recorded `Explicit`, two lines that do not cross are refused, each command is
//! one undo step restoring `ext["LAMB"]` byte-identical, and every refusal leaves the drawing as it was.
//!
//! The command registry is process-global, so registration goes through one `Once`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Once;

use cadcraft_doc::{Drawing, Handle};
use cadcraft_engine::cmd::resolve_alias;
use cadcraft_engine::{Session, register_command_provider};
use lamb_cadcraft::{DrawingProject, EXT_KEY, commands, install, project_to_drawing, read_document, view_block_name};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::model::{Actor, AnyView, Id, LinkOrigin, LinkRecord, Project};
use lamb_core::persistence::files_to_project;
use serde_json::{Value, json};

type R = Result<(), Box<dyn std::error::Error>>;

fn load(name: &str) -> Result<Project, Box<dyn std::error::Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb").join(format!("{name}.lamb.json"));
    let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(files_to_project(&files).map_err(|e| format!("{e:?}"))?)
}

static REGISTER: Once = Once::new();

fn register() {
    REGISTER.call_once(|| register_command_provider(commands).expect("register LAMB commands"));
}

/// The imported drawing after one AI / solid-cut sweep. A `.lamb` file's slot keys name the TS
/// string ids the import migrates to handles, so the first sweep re-proposes its AI lines once;
/// these tests start from the converged drawing that leaves.
fn settled(mut d: Drawing) -> Result<Drawing, Box<dyn std::error::Error>> {
    let before = d.clone();
    lamb_cadcraft::run_sweeps(&before, &mut d, "2026-01-01T00:00:00.000Z")?;
    Ok(d)
}

fn links(s: &Session) -> Result<Vec<LinkRecord>, Box<dyn std::error::Error>> {
    Ok(read_document(s.doc()?)?.links)
}

/// `ext["LAMB"]` as its debug text, for byte-for-byte comparison.
fn ext(s: &Session) -> Result<String, Box<dyn std::error::Error>> {
    Ok(format!("{:?}", s.doc()?.ext.get(EXT_KEY)))
}

fn view_of(d: &Drawing, entity: Id) -> Result<Id, String> {
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    p.entity_view_id(entity).ok_or_else(|| format!("{entity} in no view"))
}

fn is_plan(d: &Drawing, view: Id) -> Result<bool, String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    Ok(doc.views.iter().any(|v| v.id() == view && matches!(v, AnyView::Plan(_))))
}

fn block_handles(d: &Drawing, view: Id) -> Vec<Handle> {
    d.block(&view_block_name(view)).map(|b| b.entities.iter().map(|e| e.handle).collect()).unwrap_or_default()
}

fn anchor(entity: Value, index: Value, param: f64) -> Value {
    json!({"entityId": entity, "segmentIndex": index, "segmentParam": param})
}

/// Runs `cmd` expecting a refusal whose message contains `msg`; the drawing must not change.
fn refuses(s: &mut Session, cmd: &str, args: &Value, msg: &str) -> R {
    let before = s.doc()?.clone();
    let err = s.execute(cmd, args).err().ok_or_else(|| format!("{cmd} {args} was not refused"))?;
    assert!(err.to_string().contains(msg), "{cmd} {args}: expected {msg:?}, got {err}");
    assert!(*s.doc()? == before, "{cmd} {args}: a refusal leaves the drawing unchanged");
    Ok(())
}

#[test]
fn link_and_unlink() -> R {
    register();
    let d = settled(project_to_drawing(&load("u-wall-two-sections")?)?.0)?;
    let original = read_document(&d)?.links;
    assert_eq!(original.len(), 3, "fixture has 3 links");
    // A section line and its view, from an imported link's non-plan end; the plan view from the other.
    let (sec_e, _sec_view, plan_view) = {
        let l = &original[0];
        let (va, vb) = (view_of(&d, l.a.entity_id)?, view_of(&d, l.b.entity_id)?);
        if is_plan(&d, va)? { (l.b.entity_id, vb, va) } else { (l.a.entity_id, va, vb) }
    };

    let mut s = Session::new();
    s.open_drawing(d, "u-wall", None);
    install(&mut s);

    // A plan line on layer 0, far from every marker: it auto-links nothing.
    let before = block_handles(s.doc()?, plan_view);
    s.execute("editview", &json!({"viewId": plan_view.0}))?;
    s.execute("layer.current", &json!({"name": "0"}))?;
    s.execute("line", &json!({"points": [[-90000, -90000], [-85000, -90000]]}))?;
    let line = block_handles(s.doc()?, plan_view).into_iter().find(|h| !before.contains(h)).ok_or("line not drawn")?;
    assert_eq!(links(&s)?, original, "the layer-0 line links nothing");

    // --- LINK refuses two lines that do not cross (LAMB T514), with no undo step of its own.
    let target = original[1].clone();
    s.execute("unlink", &json!({"linkId": target.id.0}))?;
    let unlinked = links(&s)?;
    let far = json!({"a": anchor(json!(line.hex()), json!(0), 0.25), "b": anchor(json!(sec_e.0), json!(0), 0.5)});
    refuses(&mut s, "link", &far, "Those lines do not cross at a marker")?;
    s.undo()?;
    assert_eq!(links(&s)?, original, "the refusal left no undo step: one undo reverts the UNLINK");
    s.redo()?;
    assert_eq!(links(&s)?, unlinked);

    // --- LINK the unlinked crossing again: hex handle on one end, raw number on the other.
    let ext_before = ext(&s)?;
    let (ta, tb) = (&target.a, &target.b);
    let args = json!({
        "a": anchor(json!(Handle(ta.entity_id.0).hex()), json!(ta.segment_index), ta.segment_param),
        "b": anchor(json!(tb.entity_id.0), json!(tb.segment_index), tb.segment_param),
    });
    let r = s.execute(&resolve_alias("lk"), &args)?;
    let link_id = Id(r.get("linkId").and_then(Value::as_u64).ok_or("no linkId")?);
    let now = links(&s)?;
    assert_eq!(now.len(), unlinked.len() + 1, "link count +1");
    assert_eq!(&now[..unlinked.len()], &unlinked[..], "the other links are untouched");
    let l = now.last().ok_or("no link")?;
    assert_eq!(l.id, link_id, "the returned linkId is the record's");
    assert_eq!((&l.a, &l.b), (ta, tb));
    assert_eq!(l.origin, LinkOrigin::Explicit);
    assert_eq!(l.created_by, Actor::User { user_id: "local".into() });
    assert_eq!(l.stale, None);
    let linked = s.doc()?.clone();
    s.undo()?;
    assert_eq!(ext(&s)?, ext_before, "undo restores ext[LAMB] byte-identical");
    s.redo()?;
    assert_eq!(links(&s)?, now, "redo brings it back");
    assert!(*s.doc()? == linked);

    // --- UNLINK the new link (decimal string id), undo, and an unknown id.
    let gone = link_id;
    let ext_linked = ext(&s)?;
    assert_eq!(s.execute(&resolve_alias("ulk"), &json!({"linkId": gone.0.to_string()}))?, Value::Null);
    let after = links(&s)?;
    assert_eq!(after.len(), now.len() - 1);
    assert!(!after.iter().any(|l| l.id == gone), "the link is gone");
    s.undo()?;
    assert_eq!(ext(&s)?, ext_linked, "undo: back byte-identical");
    refuses(&mut s, "unlink", &json!({"linkId": 987_654_321u64}), "No such link")?;

    // --- Refusals, in order, each leaving the drawing unchanged.
    let good_a = anchor(json!(line.hex()), json!(0), 0.5);
    let good_b = anchor(json!(sec_e.0), json!(0), 0.5);
    refuses(&mut s, "link", &json!({"a": anchor(json!(line.hex()), json!(0), 1.5), "b": good_b}), "Link end a: segmentParam 1.5 is outside 0..1")?;
    refuses(&mut s, "link", &json!({"a": good_a, "b": anchor(json!(sec_e.0), json!(0), -0.5)}), "Link end b: segmentParam -0.5 is outside 0..1")?;
    refuses(
        &mut s,
        "link",
        &json!({"a": anchor(json!(line.hex()), json!(3), 0.5), "b": good_b}),
        "Link end a: segmentIndex 3 is past the last segment (0)",
    )?;
    refuses(
        &mut s,
        "link",
        &json!({"a": anchor(json!(line.hex()), json!(1.5), 0.5), "b": good_b}),
        "Link end a: segmentIndex 1.5 is not a whole number >= 0",
    )?;
    refuses(&mut s, "link", &json!({"a": anchor(json!("FFFFFF"), json!(0), 0.5), "b": good_b}), "No such entity")?;
    refuses(&mut s, "link", &json!({"a": good_a, "b": anchor(json!(line.hex()), json!(0), 0.1)}), "Link ends must be in different views")?;
    // A multi-segment entity reports its own last index: the plan end of an imported link.
    let plan_e = if view_of(s.doc()?, original[0].a.entity_id)? == plan_view { original[0].a.entity_id } else { original[0].b.entity_id };
    let segs = {
        let p = DrawingProject::new(s.doc()?)?;
        let poly = p.entity(plan_e).and_then(|e| e.polyline.map(std::borrow::Cow::into_owned)).ok_or("no polyline")?;
        lamb_core::geometry::polyline::resolved_segments(&poly).len()
    };
    refuses(
        &mut s,
        "link",
        &json!({"a": good_b, "b": anchor(json!(plan_e.0), json!(segs), 0.5)}),
        &format!("Link end b: segmentIndex {segs} is past the last segment ({})", segs - 1),
    )?;
    Ok(())
}

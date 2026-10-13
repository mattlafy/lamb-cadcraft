//! The link-maintenance observer (`maintain`): on the imported `u-wall-two-sections` fixture, ERASE
//! drops exactly the erased entity's links, MOVE off a crossing flags stale and back clears, a line
//! drawn through a hollow marker auto-links, an unlinked MOVE leaves links alone — each in the
//! command's own undo step.
//!
//! The command registry is process-global, so this binary has one test fn that registers.

use std::collections::BTreeMap;
use std::path::Path;

use cadcraft_doc::{Drawing, Handle};
use cadcraft_engine::{Session, register_command_provider};
use lamb_cadcraft::{DrawingProject, commands, install, maintain_links, project_to_drawing, read_document, view_block_name};
use lamb_core::derivation::issues::{IssueKind, find_issues};
use lamb_core::derivation::linking::link_still_coincides;
use lamb_core::derivation::markers::compute_markers_for_view;
use lamb_core::derivation::read::ProjectRead;
use lamb_core::model::{AnyView, Id, LinkRecord, Project};
use lamb_core::persistence::files_to_project;
use serde_json::json;

type R = Result<(), Box<dyn std::error::Error>>;

fn load(name: &str) -> Result<Project, Box<dyn std::error::Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb").join(format!("{name}.lamb.json"));
    let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(files_to_project(&files).map_err(|e| format!("{e:?}"))?)
}

fn links(s: &Session) -> Result<Vec<LinkRecord>, Box<dyn std::error::Error>> {
    Ok(read_document(s.doc()?)?.links)
}

fn hex(id: Id) -> String {
    Handle(id.0).hex()
}

fn view_of(d: &Drawing, entity: Id) -> Result<Id, String> {
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    p.entity_view_id(entity).ok_or_else(|| format!("{entity} in no view"))
}

fn is_plan(d: &Drawing, view: Id) -> Result<bool, String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    Ok(doc.views.iter().any(|v| v.id() == view && matches!(v, AnyView::Plan(_))))
}

fn on(link: &LinkRecord, e: Id) -> bool {
    link.a.entity_id == e || link.b.entity_id == e
}

fn stale_issue_count(s: &Session) -> Result<usize, Box<dyn std::error::Error>> {
    let p = DrawingProject::new(s.doc()?)?;
    Ok(find_issues(&p)?.iter().filter(|i| i.kind == IssueKind::StaleLink).count())
}

#[test]
fn link_observer_maintains_links() -> R {
    register_command_provider(commands)?;
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let original = read_document(&d)?.links;
    assert_eq!(original.len(), 3, "fixture has 3 links");
    {
        let p = DrawingProject::new(&d)?;
        for l in &original {
            assert!(link_still_coincides(&p, l)?, "untouched link {} holds", l.id);
        }
    }
    // An identical before/after changes nothing.
    let mut same = d.clone();
    assert!(maintain_links(&d, &mut same, "t")?.is_empty());

    let mut s = Session::new();
    s.open_drawing(d, "u-wall", None);
    install(&mut s);
    install(&mut s);
    assert_eq!(s.observers.len(), 1, "install is idempotent");

    // --- ERASE a linked plan entity: exactly its links go; one undo restores both.
    let plan_end = {
        let d = s.doc()?;
        let mut found = None;
        for l in &original {
            for e in [l.a.entity_id, l.b.entity_id] {
                if found.is_none() && is_plan(d, view_of(d, e)?)? {
                    found = Some(e);
                }
            }
        }
        found.ok_or("no plan-side link end")?
    };
    let plan_view = view_of(s.doc()?, plan_end)?;
    s.execute("editview", &json!({"viewId": plan_view.0}))?;
    s.execute("erase", &json!({"handles": [hex(plan_end)]}))?;
    let kept: Vec<LinkRecord> = original.iter().filter(|l| !on(l, plan_end)).cloned().collect();
    assert!(kept.len() < original.len());
    assert_eq!(links(&s)?, kept, "erase drops exactly the erased entity's links");
    s.undo()?;
    assert!(s.doc()?.entity(Handle(plan_end.0)).is_some(), "undo restores the entity");
    assert_eq!(links(&s)?, original, "and its links");

    // --- MOVE a linked section line along the cut: stale; back: cleared. One undo step each.
    let (sec_end, sec_view) = {
        let d = s.doc()?;
        let mut found = None;
        for l in &original {
            for e in [l.a.entity_id, l.b.entity_id] {
                let v = view_of(d, e)?;
                if found.is_none() && !is_plan(d, v)? {
                    found = Some((e, v));
                }
            }
        }
        found.ok_or("no section-side link end")?
    };
    let sec_links: Vec<Id> = original.iter().filter(|l| on(l, sec_end)).map(|l| l.id).collect();
    s.execute("editview", &json!({"viewId": sec_view.0}))?;
    assert_eq!(stale_issue_count(&s)?, 0);
    s.execute("move", &json!({"handles": [hex(sec_end)], "delta": [3000, 0]}))?;
    let after_move = links(&s)?;
    assert_eq!(after_move.len(), original.len(), "a broken crossing is flagged, not dropped");
    for l in &after_move {
        let expect = if sec_links.contains(&l.id) { Some(true) } else { None };
        assert_eq!(l.stale, expect, "link {} stale flag after the move", l.id);
    }
    assert_eq!(stale_issue_count(&s)?, sec_links.len(), "find_issues reports StaleLink");
    s.execute("move", &json!({"handles": [hex(sec_end)], "delta": [-3000, 0]}))?;
    assert_eq!(links(&s)?, original, "moving back clears the flags");
    s.undo()?;
    assert_eq!(links(&s)?, after_move, "one undo: stale again");
    s.undo()?;
    assert_eq!(links(&s)?, original, "second undo: as imported");

    // --- A line drawn through a hollow marker auto-links; undo removes line and link together.
    // The fixture's markers are all filled, so erase the section end first to hollow its markers.
    s.execute("erase", &json!({"handles": [hex(sec_end)]}))?;
    let without_sec: Vec<LinkRecord> = original.iter().filter(|l| !on(l, sec_end)).cloned().collect();
    assert_eq!(links(&s)?, without_sec);
    let view = sec_view;
    let marker = {
        let p = DrawingProject::new(s.doc()?)?;
        compute_markers_for_view(&p, view)?.into_iter().find(|m| !m.filled).ok_or("no hollow marker after the erase")?
    };
    let layer = {
        let p = DrawingProject::new(s.doc()?)?;
        p.entity(marker.source_entity_id).ok_or("marker source missing")?.layer_id.to_owned()
    };
    s.execute("editview", &json!({"viewId": view.0}))?;
    s.execute("layer.current", &json!({"name": layer}))?;
    let before_line: Vec<Handle> = s.doc()?.block(&view_block_name(view)).map(|b| b.entities.iter().map(|e| e.handle).collect()).unwrap_or_default();
    let (x, y) = (marker.position.x, marker.position.y);
    s.execute("line", &json!({"points": [[x - 500.0, y - 500.0], [x + 500.0, y + 500.0]]}))?;
    let new_line = s
        .doc()?
        .block(&view_block_name(view))
        .and_then(|b| b.entities.iter().map(|e| e.handle).find(|h| !before_line.contains(h)))
        .ok_or("line not drawn in the view block")?;
    let now = links(&s)?;
    let created: Vec<&LinkRecord> = now.iter().filter(|l| !original.iter().any(|o| o.id == l.id)).collect();
    assert_eq!(created.len(), 1, "one automatic link: {created:?}");
    {
        let p = DrawingProject::new(s.doc()?)?;
        let found = p.links_for_segment(Id(new_line.0), 0);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].other.entity_id, marker.source_entity_id);
    }
    s.undo()?;
    assert!(s.doc()?.entity(new_line).is_none(), "undo removes the line");
    assert_eq!(links(&s)?, without_sec, "and the link");
    s.undo()?; // layer.current
    s.undo()?; // erase
    assert!(s.doc()?.entity(Handle(sec_end.0)).is_some(), "the erased section line is back");
    assert_eq!(links(&s)?, original);

    // --- Moving an unlinked line leaves links identical (the LAMB slot is not rewritten).
    // Every fixture line is linked, so draw one well clear of the plan's markers first.
    s.execute("editview", &json!({"viewId": plan_view.0}))?;
    let before_draw = links(&s)?;
    s.execute("line", &json!({"points": [[-90000, -90000], [-85000, -90000]]}))?;
    assert_eq!(links(&s)?, before_draw, "a line through no marker forms no link");
    let unlinked = s.doc()?.block(&view_block_name(plan_view)).and_then(|b| b.entities.iter().map(|e| e.handle).max()).ok_or("no plan entity")?;
    let ext_before = s.doc()?.ext.get(lamb_cadcraft::EXT_KEY).cloned();
    s.execute("move", &json!({"handles": [unlinked.hex()], "delta": [123, 45]}))?;
    assert_eq!(links(&s)?, original);
    assert_eq!(s.doc()?.ext.get(lamb_cadcraft::EXT_KEY).cloned(), ext_before, "LAMB slot untouched");
    Ok(())
}

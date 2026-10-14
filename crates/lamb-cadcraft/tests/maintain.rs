//! The link-maintenance observer (`maintain`): on the imported `u-wall-two-sections` fixture, ERASE
//! drops exactly the erased entity's links, a non-rigid STRETCH off a crossing flags stale and back
//! clears, a line drawn through a hollow marker auto-links, an unlinked MOVE leaves links alone, and
//! a rigidly moved linked segment carries its partner across views (travel) only when the link is
//! in the selection's travel set — seeded by an idle crossing box, emptied by a window box or Esc —
//! and is flagged stale otherwise, erasing a travel-set line erases its partner, each in the command's own undo step — and a FILLET's virtual corner tag follows a PEDIT vertex insert (`corners`).
//!
//! The command registry is process-global, so registration goes through one `Once`.

use std::collections::BTreeMap;
use std::path::Path;

use cadcraft_doc::{Drawing, Handle};
use cadcraft_engine::Session;
use lamb_cadcraft::{DrawingProject, commands, install, maintain_links, project_to_drawing, read_document, view_block_name};
use lamb_core::derivation::issues::{IssueKind, find_issues};
use lamb_core::derivation::linking::link_still_coincides;
use lamb_core::derivation::markers::compute_markers_for_view;
use lamb_core::derivation::read::ProjectRead;
use lamb_core::geometry::polyline::resolved_segments;
use lamb_core::geometry::{Point2, Polyline2};
use lamb_core::model::{AnyView, Id, LinkRecord, Project};
use lamb_core::persistence::files_to_project;
use serde_json::json;

type R = Result<(), Box<dyn std::error::Error>>;

fn load(name: &str) -> Result<Project, Box<dyn std::error::Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb").join(format!("{name}.lamb.json"));
    let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(files_to_project(&files).map_err(|e| format!("{e:?}"))?)
}

/// [`install`], minus the AI / solid-cut sweep observer: these tests gate link maintenance alone
/// (the sweeps' own session tests are in `tests/sweeps.rs`).
fn install_link_maintenance(s: &mut Session) {
    install(s);
    s.observers.retain(|o| !std::ptr::fn_addr_eq(*o, lamb_cadcraft::sweep_observer as cadcraft_engine::AfterCommand));
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

/// The LAMB commands plus the process-global hooks (FILLET's virtual corners, rounded drawing).
fn register() {
    commands::install_app().expect("register LAMB");
}

fn poly(d: &Drawing, e: Id) -> Result<Polyline2, Box<dyn std::error::Error>> {
    let p = DrawingProject::new(d)?;
    Ok(p.entity(e).and_then(|x| x.polyline.map(std::borrow::Cow::into_owned)).ok_or("no polyline")?)
}

#[test]
fn link_observer_maintains_links() -> R {
    register();
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
    assert!(maintain_links(&d, &mut same, "t", &[])?.is_empty());

    let mut s = Session::new();
    s.open_drawing(d, "u-wall", None);
    install(&mut s);
    install(&mut s);
    assert_eq!(s.observers.len(), 4, "install is idempotent: corner, link, sweep, then assembly observer");
    install_link_maintenance(&mut s);

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

    // --- STRETCH one end of a linked section line (non-rigid, so no travel): stale; back: cleared.
    // One undo step each. A rigid MOVE along the cut travels instead (`rigid_move_travels_partner`).
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
    let plan_poly = poly(s.doc()?, plan_end)?;
    let top = {
        let segs = resolved_segments(&poly(s.doc()?, sec_end)?);
        let (a, b) = (segs[0].from, segs[segs.len() - 1].to);
        if a.y > b.y { a } else { b }
    };
    let window = |x: f64, y: f64| json!([[x - 50.0, y - 50.0], [x + 50.0, y + 50.0]]);
    s.execute("stretch", &json!({"window": window(top.x, top.y), "delta": [3000, 0]}))?;
    assert_eq!(poly(s.doc()?, plan_end)?, plan_poly, "a non-rigid edit travels nothing");
    let after_move = links(&s)?;
    assert_eq!(after_move.len(), original.len(), "a broken crossing is flagged, not dropped");
    for l in &after_move {
        let expect = if sec_links.contains(&l.id) { Some(true) } else { None };
        assert_eq!(l.stale, expect, "link {} stale flag after the move", l.id);
    }
    assert_eq!(stale_issue_count(&s)?, sec_links.len(), "find_issues reports StaleLink");
    s.execute("stretch", &json!({"window": window(top.x + 3000.0, top.y), "delta": [-3000, 0]}))?;
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

/// Runs one command on a copy of `d` in a session WITHOUT the observer, then `maintain_links` on it
/// with travel set `travel`: the report the observer would have acted on.
fn raw_report(
    d: &Drawing,
    view: Id,
    cmd: &str,
    args: &serde_json::Value,
    travel: &[Id],
) -> Result<(Drawing, lamb_cadcraft::MaintenanceReport), Box<dyn std::error::Error>> {
    let mut s = Session::new();
    s.open_drawing(d.clone(), "raw", None);
    s.execute("editview", &json!({"viewId": view.0}))?;
    s.execute(cmd, args)?;
    let mut after = s.doc()?.clone();
    let report = maintain_links(d, &mut after, "t", travel)?;
    Ok((after, report))
}

fn anchor_point(d: &Drawing, link: &LinkRecord, e: Id) -> Result<Point2, Box<dyn std::error::Error>> {
    let anchor = if link.a.entity_id == e { &link.a } else { &link.b };
    let seg = resolved_segments(&poly(d, e)?).into_iter().nth(anchor.segment_index).ok_or("no segment")?;
    let t = anchor.segment_param;
    Ok(Point2 { x: seg.from.x + (seg.to.x - seg.from.x) * t, y: seg.from.y + (seg.to.y - seg.from.y) * t })
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

/// §13 phase 3 gate: STRETCH a linked plan line, its link in the travel set → the section follows in
/// one undo step; the same from the section side. Moving both ends, or a non-rigid edit, travels
/// nothing.
#[test]
fn rigid_move_travels_partner() -> R {
    register();
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let original = read_document(&d)?.links;
    // The crossbar link: plan wall segment 1 (0,0)-(10000,0) ↔ a section line at u = 1000. The
    // crossbar's cut line runs plan +Y, so a plan delta (0, k) is a section delta (k, 0).
    let link = original.iter().find(|l| l.a.segment_index == 1 || l.b.segment_index == 1).ok_or("no seg-1 link")?.clone();
    let (plan_e, sec_e) =
        if is_plan(&d, view_of(&d, link.a.entity_id)?)? { (link.a.entity_id, link.b.entity_id) } else { (link.b.entity_id, link.a.entity_id) };
    let i = if link.a.entity_id == plan_e { link.a.segment_index } else { link.b.segment_index };
    let (plan_view, sec_view) = (view_of(&d, plan_e)?, view_of(&d, sec_e)?);
    let seg = resolved_segments(&poly(&d, plan_e)?).into_iter().nth(i).ok_or("no plan segment")?;
    let (lo, hi) = (seg.from.x.min(seg.to.x), seg.from.x.max(seg.to.x));
    let stretch = json!({"window": [[lo - 100.0, seg.from.y - 100.0], [hi + 100.0, seg.from.y + 100.0]], "delta": [0, 1000]});

    // The gate, plan side. The report names the section partner.
    let (_, report) = raw_report(&d, plan_view, "stretch", &stretch, &[link.id])?;
    assert_eq!(report.travelled, vec![sec_e], "{report:?}");
    let mut s = Session::new();
    s.open_drawing(d.clone(), "u-wall", None);
    install_link_maintenance(&mut s);
    let sec_before = anchor_point(s.doc()?, &link, sec_e)?;
    let ext_before = s.doc()?.ext.get(lamb_cadcraft::EXT_KEY).cloned();
    let (plan_ent, sec_ent) = (s.doc()?.entity(Handle(plan_e.0)).cloned(), s.doc()?.entity(Handle(sec_e.0)).cloned());
    s.execute("editview", &json!({"viewId": plan_view.0}))?;
    s.set_travel(vec![link.id.0]);
    s.execute("stretch", &stretch)?;
    let sec_after = anchor_point(s.doc()?, &link, sec_e)?;
    assert!(close(sec_after.x - sec_before.x, 1000.0) && close(sec_after.y, sec_before.y), "section anchor {sec_before:?} -> {sec_after:?}");
    {
        let p = DrawingProject::new(s.doc()?)?;
        assert!(link_still_coincides(&p, &link)?, "the travelled link holds");
    }
    assert_eq!(links(&s)?, original, "nothing stale, nothing dropped");
    s.undo()?;
    assert_eq!(s.doc()?.entity(Handle(plan_e.0)).cloned(), plan_ent, "one undo restores the plan entity");
    assert_eq!(s.doc()?.entity(Handle(sec_e.0)).cloned(), sec_ent, "and the section entity");
    assert_eq!(s.doc()?.ext.get(lamb_cadcraft::EXT_KEY).cloned(), ext_before, "and the LAMB slot, byte-identical");

    // Section side: MOVE the section line along u → the plan segment slides along the cut, its
    // neighbours ray-slide, vertex count unchanged.
    let mv = json!({"handles": [hex(sec_e)], "delta": [1000, 0]});
    let (_, report) = raw_report(&d, sec_view, "move", &mv, &[link.id])?;
    assert_eq!(report.travelled, vec![plan_e], "{report:?}");
    let before = poly(s.doc()?, plan_e)?;
    s.execute("editview", &json!({"viewId": sec_view.0}))?;
    s.set_travel(vec![link.id.0]);
    s.execute("move", &mv)?;
    let after = poly(s.doc()?, plan_e)?;
    assert_eq!(after.segments.len(), before.segments.len(), "vertex count unchanged");
    let (b, a) = (resolved_segments(&before), resolved_segments(&after));
    for (pb, pa) in [(b[i].from, a[i].from), (b[i].to, a[i].to)] {
        assert!(close(pa.x, pb.x) && close(pa.y - pb.y, 1000.0), "segment {i} slid along the cut: {pb:?} -> {pa:?}");
    }
    assert_eq!(a[i - 1].from, b[i - 1].from, "the neighbour's far end stays put");
    assert!(close(a[i - 1].to.x, b[i - 1].to.x), "the neighbour ray-slides along itself");
    assert_eq!(links(&s)?, original, "link not stale");
    s.undo()?;
    assert_eq!(s.doc()?.entity(Handle(plan_e.0)).cloned(), plan_ent);
    assert_eq!(s.doc()?.entity(Handle(sec_e.0)).cloned(), sec_ent);
    assert_eq!(s.doc()?.ext.get(lamb_cadcraft::EXT_KEY).cloned(), ext_before);

    // Both ends moved in one command: the user moved both, nothing travels.
    let (after, report) = raw_report(&d, plan_view, "move", &json!({"handles": [hex(plan_e), hex(sec_e)], "delta": [0, 500]}), &[link.id])?;
    assert!(report.travelled.is_empty(), "{report:?}");
    assert!(poly(&after, plan_e)? != poly(&d, plan_e)? && poly(&after, sec_e)? != poly(&d, sec_e)?, "both ends really moved");

    // A non-rigid edit (one endpoint of the plan segment) travels nothing.
    let one_end = json!({"window": [[seg.to.x - 100.0, seg.to.y - 100.0], [seg.to.x + 100.0, seg.to.y + 100.0]], "delta": [0, 1000]});
    let (after, report) = raw_report(&d, plan_view, "stretch", &one_end, &[link.id])?;
    assert!(report.travelled.is_empty(), "{report:?}");
    assert_eq!(poly(&after, sec_e)?, poly(&d, sec_e)?);
    Ok(())
}

/// The crossbar link (plan wall segment 1 (0,0)-(10000,0) crossed at (5000,0) ↔ the section line
/// at u = 1000), its plan and section entities, and their views.
fn crossbar(d: &Drawing) -> Result<(LinkRecord, Id, Id, Id), Box<dyn std::error::Error>> {
    let link = read_document(d)?.links.into_iter().find(|l| l.a.segment_index == 1 || l.b.segment_index == 1).ok_or("no seg-1 link")?;
    let (plan_e, sec_e) =
        if is_plan(d, view_of(d, link.a.entity_id)?)? { (link.a.entity_id, link.b.entity_id) } else { (link.b.entity_id, link.a.entity_id) };
    let plan_view = view_of(d, plan_e)?;
    Ok((link, plan_e, sec_e, plan_view))
}

/// T367: the same rigid STRETCH with the link outside the travel set leaves the section partner
/// where it is and flags the link stale; one undo restores both.
#[test]
fn outside_the_travel_set_a_rigid_move_flags_stale() -> R {
    register();
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let original = read_document(&d)?.links;
    let (link, plan_e, sec_e, plan_view) = crossbar(&d)?;
    let stretch = json!({"window": [[-100, -100], [10100, 100]], "delta": [0, 1000]});

    let (_, report) = raw_report(&d, plan_view, "stretch", &stretch, &[])?;
    assert!(report.travelled.is_empty(), "{report:?}");
    assert!(report.staled.contains(&link.id), "{report:?}");

    let mut s = Session::new();
    s.open_drawing(d.clone(), "u-wall", None);
    install_link_maintenance(&mut s);
    let (plan_ent, sec_poly) = (s.doc()?.entity(Handle(plan_e.0)).cloned(), poly(s.doc()?, sec_e)?);
    s.execute("editview", &json!({"viewId": plan_view.0}))?;
    assert!(s.travel().is_empty());
    s.execute("stretch", &stretch)?;
    assert!(s.doc()?.entity(Handle(plan_e.0)).cloned() != plan_ent, "the plan wall moved");
    assert_eq!(poly(s.doc()?, sec_e)?, sec_poly, "the section partner did not travel");
    assert_eq!(find_link(&links(&s)?, link.id)?.stale, Some(true), "the link is flagged, not dropped");
    s.undo()?;
    assert_eq!(s.doc()?.entity(Handle(plan_e.0)).cloned(), plan_ent);
    assert_eq!(links(&s)?, original, "undo clears the flag");
    Ok(())
}

/// T367, through real idle input in the plan: a right-to-left (crossing) box over the wall seeds
/// the travel set with the wall's links, a left-to-right (window) box empties it, Esc clears it,
/// and an interactive MOVE of a crossing-boxed wall carries the section partner — the set dies
/// with the selection afterwards.
#[test]
fn a_crossing_box_seeds_the_travel_set() -> R {
    use cadcraft_engine::Input;
    use cadcraft_geom::Vec2;

    register();
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let original = read_document(&d)?.links;
    let (link, plan_e, sec_e, plan_view) = crossbar(&d)?;
    let mut s = Session::new();
    s.open_drawing(d.clone(), "u-wall", None);
    install_link_maintenance(&mut s);
    s.execute("editview", &json!({"viewId": plan_view.0}))?;
    let boxed = |s: &mut Session, a: (f64, f64), b: (f64, f64)| -> R {
        assert!(s.begin_window(Vec2::new(a.0, a.1)), "box opened");
        s.idle_click(Vec2::new(b.0, b.1), false)?;
        Ok(())
    };
    let crossing = |s: &mut Session| boxed(s, (5100.0, 100.0), (4900.0, -100.0));

    crossing(&mut s)?;
    let sel = s.selection();
    assert!(sel.contains(&Handle(plan_e.0)), "the crossing box caught the wall: {sel:?}");
    let expect: Vec<u64> = original.iter().filter(|l| sel.iter().any(|h| on(l, Id(h.0)))).map(|l| l.id.0).collect();
    assert!(expect.contains(&link.id.0));
    assert_eq!(s.travel(), expect, "every link on a swept entity");

    boxed(&mut s, (4900.0, -100.0), (5100.0, 100.0))?;
    assert!(s.travel().is_empty(), "a window box seeds none");
    crossing(&mut s)?;
    assert!(!s.travel().is_empty());
    s.cancel();
    assert!(s.selection().is_empty() && s.travel().is_empty(), "Esc clears");

    crossing(&mut s)?;
    let sec_before = anchor_point(s.doc()?, &link, sec_e)?;
    s.start("move")?;
    s.input(Input::Point(Vec2::new(0.0, 0.0)))?;
    s.input(Input::Point(Vec2::new(0.0, 1000.0)))?;
    assert!(s.running.is_none(), "MOVE finished");
    let sec_after = anchor_point(s.doc()?, &link, sec_e)?;
    assert!(close(sec_after.x - sec_before.x, 1000.0) && close(sec_after.y, sec_before.y), "section anchor {sec_before:?} -> {sec_after:?}");
    {
        let p = DrawingProject::new(s.doc()?)?;
        assert!(link_still_coincides(&p, &find_link(&links(&s)?, link.id)?)?, "the travelled link holds");
    }
    assert!(s.selection().is_empty() && s.travel().is_empty(), "the set died with the selection");
    s.undo()?;
    assert_eq!(anchor_point(s.doc()?, &link, sec_e)?, sec_before, "one undo puts the partner back");
    Ok(())
}

fn block_handles(d: &Drawing, view: Id) -> Vec<Handle> {
    d.block(&view_block_name(view)).map(|b| b.entities.iter().map(|e| e.handle).collect()).unwrap_or_default()
}

/// Draws a cutting line on layer `0` in `view` (so it never auto-links) and returns its handle.
fn draw_edge(s: &mut Session, view: Id, from: [f64; 2], to: [f64; 2]) -> Result<Handle, Box<dyn std::error::Error>> {
    let before = block_handles(s.doc()?, view);
    s.execute("editview", &json!({"viewId": view.0}))?;
    s.execute("layer.current", &json!({"name": "0"}))?;
    s.execute("line", &json!({"points": [from, to]}))?;
    Ok(block_handles(s.doc()?, view).into_iter().find(|h| !before.contains(h)).ok_or("edge not drawn")?)
}

fn find_link(links: &[LinkRecord], id: Id) -> Result<LinkRecord, String> {
    links.iter().find(|l| l.id == id).cloned().ok_or_else(|| format!("link {id} gone"))
}

fn joins(l: &LinkRecord, x: Id, y: Id) -> bool {
    (l.a.entity_id == x && l.b.entity_id == y) || (l.a.entity_id == y && l.b.entity_id == x)
}

/// §13 phase 3 gate: TRIM a linked line → the link is maintained on the piece that still holds its
/// anchor (same id), dropped when the anchor was cut away; BREAK the same; from either view.
#[test]
fn trim_reanchors_links() -> R {
    register();
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let original = read_document(&d)?.links;
    // The crossbar link: plan wall segment 1 (0,0)-(10000,0) crossed at (5000,0) ↔ the section line
    // (1000,0)-(1000,2700) anchored at (1000,1000).
    let link = original.iter().find(|l| l.a.segment_index == 1 || l.b.segment_index == 1).ok_or("no seg-1 link")?.clone();
    let (plan_e, sec_e) =
        if is_plan(&d, view_of(&d, link.a.entity_id)?)? { (link.a.entity_id, link.b.entity_id) } else { (link.b.entity_id, link.a.entity_id) };
    let (plan_view, sec_view) = (view_of(&d, plan_e)?, view_of(&d, sec_e)?);

    let mut s = Session::new();
    s.open_drawing(d.clone(), "u-wall", None);
    install_link_maintenance(&mut s);
    // Two cutting edges across segment 1, away from the crossing.
    let e1 = draw_edge(&mut s, plan_view, [7000.0, -500.0], [7000.0, 500.0])?;
    let e2 = draw_edge(&mut s, plan_view, [8000.0, -500.0], [8000.0, 500.0])?;
    assert_eq!(links(&s)?, original, "edges on layer 0 link nothing");
    let base = s.doc()?.clone();
    let edges = json!([e1.hex(), e2.hex()]);

    // --- The gate: trim the far span.
    let far = json!({"handle": hex(plan_e), "pick": [7500, 0], "edges": edges});
    let (_, report) = raw_report(&base, plan_view, "trim", &far, &[])?;
    assert!(report.reanchored.contains(&link.id), "{report:?}");
    assert!(report.dropped.is_empty() && report.created.is_empty(), "{report:?}");
    let ext_before = s.doc()?.ext.get(lamb_cadcraft::EXT_KEY).cloned();
    let plan_ent = s.doc()?.entity(Handle(plan_e.0)).cloned();
    let handles_before = block_handles(s.doc()?, plan_view);
    s.execute("editview", &json!({"viewId": plan_view.0}))?;
    s.execute("trim", &far)?;
    let now = links(&s)?;
    assert_eq!(now.len(), original.len(), "link count unchanged: {now:?}");
    let l = find_link(&now, link.id)?;
    assert_eq!((l.origin, &l.created_at, &l.created_by, l.stale), (link.origin, &link.created_at, &link.created_by, None));
    let new_plan = if l.a.entity_id == sec_e { l.b.entity_id } else { l.a.entity_id };
    assert!(!handles_before.contains(&Handle(new_plan.0)) && s.doc()?.entity(Handle(new_plan.0)).is_some(), "plan end on a new piece");
    assert_eq!(now.iter().filter(|x| joins(x, new_plan, sec_e)).count(), 1, "exactly one link joins the pair");
    {
        let p = DrawingProject::new(s.doc()?)?;
        for x in &now {
            assert!(link_still_coincides(&p, x)?, "link {} holds after the trim", x.id);
        }
    }
    s.undo()?;
    assert_eq!(s.doc()?.entity(Handle(plan_e.0)).cloned(), plan_ent, "one undo restores the plan entity");
    assert_eq!(s.doc()?.ext.get(lamb_cadcraft::EXT_KEY).cloned(), ext_before, "and the LAMB slot, byte-identical");

    // --- Trimming the span holding the crossing drops that link; it is not re-formed.
    let e3 = draw_edge(&mut s, plan_view, [4000.0, -500.0], [4000.0, 500.0])?;
    let e4 = draw_edge(&mut s, plan_view, [6000.0, -500.0], [6000.0, 500.0])?;
    let base2 = s.doc()?.clone();
    let near = json!({"handle": hex(plan_e), "pick": [5000, 0], "edges": [e3.hex(), e4.hex()]});
    let (after, report) = raw_report(&base2, plan_view, "trim", &near, &[])?;
    assert_eq!(report.dropped, vec![link.id], "{report:?}");
    assert!(report.created.is_empty(), "{report:?}");
    let after_links = read_document(&after)?.links;
    assert_eq!(after_links.len(), original.len() - 1);
    assert!(!after_links.iter().any(|x| on(x, sec_e)), "nothing re-formed on the section line");

    // --- BREAK across the line on one side of the crossing: the link lands on the piece holding it.
    let brk = json!({"handle": hex(plan_e), "p1": [7000, 0], "p2": [8000, 0]});
    let (after, report) = raw_report(&base, plan_view, "break", &brk, &[])?;
    assert!(report.reanchored.contains(&link.id) && report.dropped.is_empty(), "{report:?}");
    let l = find_link(&read_document(&after)?.links, link.id)?;
    let piece = if l.a.entity_id == sec_e { l.b.entity_id } else { l.a.entity_id };
    assert!(after.entity(Handle(plan_e.0)).is_none() && after.entity(Handle(piece.0)).is_some());
    let at = anchor_point(&after, &l, piece)?;
    assert!(close(at.x, 5000.0) && close(at.y, 0.0), "anchor still at the crossing: {at:?}");
    assert!(link_still_coincides(&DrawingProject::new(&after)?, &l)?);

    // --- Section side: cut the section line above its anchor at z = 1000, trim the top away.
    let e5 = draw_edge(&mut s, sec_view, [500.0, 2000.0], [1500.0, 2000.0])?;
    let base3 = s.doc()?.clone();
    let top = json!({"handle": hex(sec_e), "pick": [1000, 2500], "edges": [e5.hex()]});
    let (after, report) = raw_report(&base3, sec_view, "trim", &top, &[])?;
    assert_eq!(report.reanchored, vec![link.id], "{report:?}");
    assert!(report.dropped.is_empty() && report.created.is_empty(), "{report:?}");
    let l = find_link(&read_document(&after)?.links, link.id)?;
    assert!(!on(&l, sec_e) && on(&l, plan_e) && l.stale.is_none());
    assert!(link_still_coincides(&DrawingProject::new(&after)?, &l)?);
    // Trimming the bottom (holding the anchor) away drops it instead.
    let bottom = json!({"handle": hex(sec_e), "pick": [1000, 500], "edges": [e5.hex()]});
    let (_, report) = raw_report(&base3, sec_view, "trim", &bottom, &[])?;
    assert_eq!(report.dropped, vec![link.id], "{report:?}");
    Ok(())
}

/// T370 (TS T109 ERASE): erasing a line whose link is in the travel set erases the partner in the
/// other view too, in one undo step; with an empty travel set the partner stays; a TRIM or BREAK
/// that cuts the anchor away is not an erase and leaves the partner.
#[test]
fn erase_takes_travelling_partners() -> R {
    register();
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let original = read_document(&d)?.links;
    let (link, plan_e, sec_e, plan_view) = crossbar(&d)?;
    let erase = json!({"handles": [hex(plan_e)]});

    let (after, report) = raw_report(&d, plan_view, "erase", &erase, &[link.id])?;
    assert_eq!(report.erased, vec![sec_e], "{report:?}");
    assert!(after.entity(Handle(sec_e.0)).is_none(), "the section partner is gone");
    let gone: Vec<Id> = original.iter().filter(|l| on(l, plan_e) || on(l, sec_e)).map(|l| l.id).collect();
    assert_eq!(report.dropped, gone, "every link touching either is dropped");
    assert_eq!(read_document(&after)?.links, original.iter().filter(|l| !gone.contains(&l.id)).cloned().collect::<Vec<_>>());

    let mut s = Session::new();
    s.open_drawing(d.clone(), "u-wall", None);
    install_link_maintenance(&mut s);
    let ext_before = s.doc()?.ext.get(lamb_cadcraft::EXT_KEY).cloned();
    let (plan_ent, sec_ent) = (s.doc()?.entity(Handle(plan_e.0)).cloned(), s.doc()?.entity(Handle(sec_e.0)).cloned());
    s.execute("editview", &json!({"viewId": plan_view.0}))?;
    s.set_travel(vec![link.id.0]);
    s.execute("erase", &erase)?;
    assert!(s.doc()?.entity(Handle(plan_e.0)).is_none() && s.doc()?.entity(Handle(sec_e.0)).is_none(), "both erased");
    assert!(!links(&s)?.iter().any(|l| on(l, plan_e) || on(l, sec_e)));
    s.undo()?;
    assert_eq!(s.doc()?.entity(Handle(plan_e.0)).cloned(), plan_ent, "one undo restores the plan entity");
    assert_eq!(s.doc()?.entity(Handle(sec_e.0)).cloned(), sec_ent, "and the section partner");
    assert_eq!(s.doc()?.ext.get(lamb_cadcraft::EXT_KEY).cloned(), ext_before, "and the LAMB slot, byte-identical");

    // Empty travel set: the link is dropped, the partner stays.
    let (after, report) = raw_report(&d, plan_view, "erase", &erase, &[])?;
    assert!(report.erased.is_empty(), "{report:?}");
    assert!(after.entity(Handle(sec_e.0)).is_some(), "the partner stays");
    assert!(report.dropped.contains(&link.id), "{report:?}");

    // TRIM / BREAK cutting the anchor away, link in the travel set: not an erase.
    let mut s = Session::new();
    s.open_drawing(d.clone(), "u-wall", None);
    install_link_maintenance(&mut s);
    let e1 = draw_edge(&mut s, plan_view, [4000.0, -500.0], [4000.0, 500.0])?;
    let e2 = draw_edge(&mut s, plan_view, [6000.0, -500.0], [6000.0, 500.0])?;
    let base = s.doc()?.clone();
    let near = json!({"handle": hex(plan_e), "pick": [5000, 0], "edges": [e1.hex(), e2.hex()]});
    let (after, report) = raw_report(&base, plan_view, "trim", &near, &[link.id])?;
    assert_eq!(report.dropped, vec![link.id], "{report:?}");
    assert!(report.erased.is_empty() && after.entity(Handle(sec_e.0)).is_some(), "trim erases no partner: {report:?}");
    let brk = json!({"handle": hex(plan_e), "p1": [4000, 0], "p2": [6000, 0]});
    let (after, report) = raw_report(&base, plan_view, "break", &brk, &[link.id])?;
    assert_eq!(report.dropped, vec![link.id], "{report:?}");
    assert!(report.erased.is_empty() && after.entity(Handle(sec_e.0)).is_some(), "break erases no partner: {report:?}");
    Ok(())
}

/// T332: through `install`, a PEDIT vertex insert before a filleted vertex moves its tag with it,
/// the drawn arc stays at the same corner, and undo puts the old index back.
#[test]
fn corner_tag_follows_a_vertex_insert() -> R {
    use cadcraft_doc::{EntityKind, Space};
    use cadcraft_render::{Options, build};
    use lamb_cadcraft::read_entity_data;

    register();
    let mut s = Session::new();
    s.new_drawing(true);
    install_link_maintenance(&mut s);
    let r = s.execute("pline", &json!({"vertices": [[0, 0], [100, 0], [100, 80]]}))?;
    let hx = r["handle"].as_str().ok_or("no handle")?.to_owned();
    let h = Handle::parse_hex(&hx).ok_or("bad handle")?;
    s.execute("fillet", &json!({"h1": hx, "p1": [50, 0], "h2": hx, "p2": [100, 40], "radius": 10}))?;
    let index = |s: &Session| -> Result<Vec<usize>, Box<dyn std::error::Error>> {
        let e = s.doc()?.entity(h).ok_or("no entity")?;
        Ok(read_entity_data(&e.common)?.corners.iter().map(|c| c.corner_index).collect())
    };
    assert_eq!(index(&s)?, vec![1]);

    s.execute("pedit", &json!({"handle": hx, "option": "vertex", "action": "insert", "index": 0, "to": [50, 0]}))?;
    assert_eq!(index(&s)?, vec![2]);
    let d = s.doc()?;
    let EntityKind::LwPolyline(pl) = &d.entity(h).ok_or("no entity")?.kind else { return Err("not a polyline".into()) };
    let v = pl.vertices.get(2).ok_or("no vertex 2")?.p;
    assert!((v.x - 100.0).abs() < 1e-9 && v.y.abs() < 1e-9, "vertex 2 is the tagged corner: {v:?}");
    let list = build(d, &Space::Model, &Options::default());
    let pts: Vec<_> = list.prims.iter().filter(|p| p.handle == h).flat_map(|p| list.points(p).to_vec()).collect();
    let near = |x: f64, y: f64| pts.iter().any(|p| (p.x - x).abs() < 1e-6 && (p.y - y).abs() < 1e-6);
    assert!(!near(100.0, 0.0) && near(90.0, 0.0) && near(100.0, 10.0), "the arc is drawn at (100, 0): {pts:?}");

    s.undo()?;
    assert_eq!(index(&s)?, vec![1]);
    Ok(())
}

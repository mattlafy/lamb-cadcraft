//! The link-maintenance observer (`maintain`): on the imported `u-wall-two-sections` fixture, ERASE
//! drops exactly the erased entity's links, a non-rigid STRETCH off a crossing drops the link (a
//! link exists only where its lines cross, LAMB T514), a line drawn through a hollow marker auto-links, an unlinked MOVE leaves links alone, and
//! a rigidly moved linked segment carries its partner across views (travel) only when the link is
//! in the selection's travel set — seeded by an idle crossing box, emptied by a window box or Esc —
//! and is dropped otherwise, erasing a line never erases its partner, a marker fills in both views
//! whichever segment of its source the link names, each in the command's own undo step — and a FILLET's virtual corner tag follows a PEDIT vertex insert (`corners`).
//!
//! The command registry is process-global, so registration goes through one `Once`.

use std::collections::BTreeMap;
use std::path::Path;

use cadcraft_doc::{Drawing, Handle};
use cadcraft_engine::Session;
use lamb_cadcraft::{DrawingProject, commands, install, maintain_links, project_to_drawing, read_document, view_block_name};
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

/// The LAMB commands plus the process-global hooks (FILLET's virtual corners, rounded drawing).
fn register() {
    commands::install_app().expect("register LAMB");
}

fn poly(d: &Drawing, e: Id) -> Result<Polyline2, Box<dyn std::error::Error>> {
    let p = DrawingProject::new(d)?;
    Ok(p.entity(e).and_then(|x| x.polyline.map(std::borrow::Cow::into_owned)).ok_or("no polyline")?)
}

#[test]
#[allow(clippy::cognitive_complexity)]
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
    assert_eq!(
        s.observers.len(),
        10,
        "install is idempotent: corner, link, tag follow, sweep, assembly edit, repeat parameter, assembly, bubble, annotation, then hole observer"
    );
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

    // --- STRETCH one end of a linked section line (non-rigid, so no travel): its links are
    // dropped; one undo brings them back.
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
    let plan_poly = poly(s.doc()?, plan_end)?;
    let top = {
        let segs = resolved_segments(&poly(s.doc()?, sec_end)?);
        let (a, b) = (segs[0].from, segs[segs.len() - 1].to);
        if a.y > b.y { a } else { b }
    };
    let window = |x: f64, y: f64| json!([[x - 50.0, y - 50.0], [x + 50.0, y + 50.0]]);
    s.execute("stretch", &json!({"window": window(top.x, top.y), "delta": [3000, 0]}))?;
    assert_eq!(poly(s.doc()?, plan_end)?, plan_poly, "a non-rigid edit travels nothing");
    let kept: Vec<LinkRecord> = original.iter().filter(|l| !sec_links.contains(&l.id)).cloned().collect();
    assert_eq!(links(&s)?, kept, "a broken crossing drops the link");
    s.undo()?;
    assert_eq!(links(&s)?, original, "one undo: as imported");

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

/// Every marker `source` makes in `view` — hollow or filled.
fn markers_from(d: &Drawing, view: Id, source: Id) -> Result<Vec<lamb_core::derivation::markers::Marker>, Box<dyn std::error::Error>> {
    let p = DrawingProject::new(d)?;
    Ok(compute_markers_for_view(&p, view)?.into_iter().filter(|m| m.source_entity_id == source).collect())
}

/// T367 + LAMB T514: the same rigid STRETCH with the link outside the travel set leaves the section
/// partner where it is and drops the link — a link exists only where its lines cross; one undo
/// restores both. A section line MOVEd off its marker the same way drops its link and leaves both
/// markers hollow.
#[test]
fn outside_the_travel_set_a_rigid_move_drops_the_link() -> R {
    register();
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let original = read_document(&d)?.links;
    let (link, plan_e, sec_e, plan_view) = crossbar(&d)?;
    let sec_view = view_of(&d, sec_e)?;
    let stretch = json!({"window": [[-100, -100], [10100, 100]], "delta": [0, 1000]});

    let (_, report) = raw_report(&d, plan_view, "stretch", &stretch, &[])?;
    assert!(report.travelled.is_empty(), "{report:?}");
    assert!(report.separated.contains(&link.id), "{report:?}");

    let mut s = Session::new();
    s.open_drawing(d.clone(), "u-wall", None);
    install_link_maintenance(&mut s);
    let (plan_ent, sec_poly) = (s.doc()?.entity(Handle(plan_e.0)).cloned(), poly(s.doc()?, sec_e)?);
    s.execute("editview", &json!({"viewId": plan_view.0}))?;
    assert!(s.travel().is_empty());
    s.execute("stretch", &stretch)?;
    assert!(s.doc()?.entity(Handle(plan_e.0)).cloned() != plan_ent, "the plan wall moved");
    assert_eq!(poly(s.doc()?, sec_e)?, sec_poly, "the section partner did not travel");
    assert!(find_link(&links(&s)?, link.id).is_err(), "the link is dropped");
    s.undo()?;
    assert_eq!(s.doc()?.entity(Handle(plan_e.0)).cloned(), plan_ent);
    assert_eq!(links(&s)?, original, "undo brings the link back");

    // The section line moved along u, off its marker: link gone, both markers hollow; undo: back.
    let filled = |d: &Drawing| -> Result<(bool, bool), Box<dyn std::error::Error>> {
        let plan_side = markers_from(d, plan_view, sec_e)?;
        let sec_side = markers_from(d, sec_view, plan_e)?;
        assert!(!plan_side.is_empty() && !sec_side.is_empty(), "a marker on each side");
        Ok((plan_side.iter().any(|m| m.filled), sec_side.iter().any(|m| m.filled)))
    };
    assert_eq!(filled(s.doc()?)?, (true, true), "linked: filled on both sides");
    s.execute("editview", &json!({"viewId": sec_view.0}))?;
    s.execute("move", &json!({"handles": [hex(sec_e)], "delta": [500, 0]}))?;
    let now = links(&s)?;
    assert!(!now.iter().any(|l| on(l, sec_e)), "every link on the moved line is dropped: {now:?}");
    assert_eq!(filled(s.doc()?)?, (false, false), "both markers hollow");
    s.undo()?;
    assert_eq!(links(&s)?, original, "undo brings the link back");
    assert_eq!(filled(s.doc()?)?, (true, true));
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

/// LAMB T514: erasing a line never erases its partner, travel set or not — the link is dropped and
/// the partner's marker turns hollow; one undo restores line and link.
#[test]
fn erase_never_takes_partners() -> R {
    register();
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let original = read_document(&d)?.links;
    let (link, plan_e, sec_e, _) = crossbar(&d)?;
    let sec_view = view_of(&d, sec_e)?;
    let erase = json!({"handles": [hex(sec_e)]});
    let plan_anchor = if link.a.entity_id == plan_e { link.a.clone() } else { link.b.clone() };
    // The marker the plan wall makes in the section, at the link's plan anchor segment.
    let plan_marker_filled = |d: &Drawing| -> Result<bool, Box<dyn std::error::Error>> {
        let ms = markers_from(d, sec_view, plan_e)?;
        let m = ms.iter().find(|m| m.source_segment_index == plan_anchor.segment_index).ok_or("no plan-wall marker in the section")?;
        Ok(m.filled)
    };

    let mut s = Session::new();
    s.open_drawing(d.clone(), "u-wall", None);
    install_link_maintenance(&mut s);
    assert!(plan_marker_filled(s.doc()?)?);
    let ext_before = s.doc()?.ext.get(lamb_cadcraft::EXT_KEY).cloned();
    let plan_ent = s.doc()?.entity(Handle(plan_e.0)).cloned();
    s.execute("editview", &json!({"viewId": sec_view.0}))?;
    s.set_travel(vec![link.id.0]);
    s.execute("erase", &erase)?;
    assert!(s.doc()?.entity(Handle(sec_e.0)).is_none(), "the section line is erased");
    assert_eq!(s.doc()?.entity(Handle(plan_e.0)).cloned(), plan_ent, "its plan partner stays, unchanged");
    assert!(find_link(&links(&s)?, link.id).is_err(), "the link is gone");
    assert!(!plan_marker_filled(s.doc()?)?, "the plan wall's marker is hollow");
    s.undo()?;
    assert!(s.doc()?.entity(Handle(sec_e.0)).is_some(), "one undo restores the line");
    assert_eq!(links(&s)?, original, "and its link");
    assert_eq!(s.doc()?.ext.get(lamb_cadcraft::EXT_KEY).cloned(), ext_before, "and the LAMB slot, byte-identical");
    Ok(())
}

/// LAMB T514: a section polyline whose vertex sits on the plan cut makes two plan markers at one
/// point (segment 0 ends there, segment 1 starts there). Linked on segment 0 only, both are filled,
/// and so is the section-side marker: a link fills its marker in both views.
#[test]
fn a_link_fills_its_marker_in_both_views_whatever_the_segment() -> R {
    register();
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let (link, plan_e, sec_e, plan_view) = crossbar(&d)?;
    let sec_view = view_of(&d, sec_e)?;
    let plan_anchor = if link.a.entity_id == plan_e { link.a.clone() } else { link.b.clone() };
    let mut s = Session::new();
    s.open_drawing(d.clone(), "u-wall", None);
    install_link_maintenance(&mut s);
    // Where the plan wall crosses the section, in the section's local space: (u, plan cut z).
    let sec_marker = |d: &Drawing| -> Result<lamb_core::derivation::markers::Marker, Box<dyn std::error::Error>> {
        Ok(markers_from(d, sec_view, plan_e)?.into_iter().find(|m| m.source_segment_index == plan_anchor.segment_index).ok_or("no marker")?)
    };
    let at = sec_marker(s.doc()?)?.position;
    let layer = {
        let p = DrawingProject::new(s.doc()?)?;
        p.entity(sec_e).ok_or("no section line")?.layer_id.to_owned()
    };
    // Replace the section line by a polyline through the same point, bent there.
    s.execute("editview", &json!({"viewId": sec_view.0}))?;
    s.execute("erase", &json!({"handles": [hex(sec_e)]}))?;
    s.execute("layer.current", &json!({"name": layer}))?;
    let before = block_handles(s.doc()?, sec_view);
    s.execute("pline", &json!({"vertices": [[at.x, at.y - 1000.0], [at.x, at.y], [at.x + 600.0, at.y + 1000.0]]}))?;
    let pl = Id(block_handles(s.doc()?, sec_view).into_iter().find(|h| !before.contains(h)).ok_or("pline not drawn")?.0);
    // Clear whatever auto-linked, then link segment 0 alone.
    for l in links(&s)?.into_iter().filter(|l| on(l, pl)) {
        s.execute("unlink", &json!({"linkId": l.id.0}))?;
    }
    let plan_markers = markers_from(s.doc()?, plan_view, pl)?;
    assert_eq!(plan_markers.len(), 2, "segments 0 and 1 each make a plan marker: {plan_markers:?}");
    assert!(plan_markers.iter().all(|m| !m.filled), "unlinked: hollow");
    assert!(!sec_marker(s.doc()?)?.filled, "unlinked: the section-side marker hollow too");
    s.execute(
        "link",
        &json!({
            "a": {"entityId": pl.0, "segmentIndex": 0, "segmentParam": 1.0},
            "b": {"entityId": plan_e.0, "segmentIndex": plan_anchor.segment_index, "segmentParam": plan_anchor.segment_param},
        }),
    )?;
    let plan_markers = markers_from(s.doc()?, plan_view, pl)?;
    assert!(plan_markers.iter().all(|m| m.filled), "both plan markers filled: {plan_markers:?}");
    assert!(sec_marker(s.doc()?)?.filled, "and the section-side marker");
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

/// The stored `EntityData.role` of `h` (the slot as written, not resolved).
fn stored_role(s: &Session, h: Handle) -> Result<lamb_core::model::LineRole, Box<dyn std::error::Error>> {
    let e = s.doc()?.entity(h).ok_or("entity missing")?;
    Ok(lamb_cadcraft::read_entity_data(&e.common)?.role)
}

/// Draws a LINE on `layer` in `view` and returns its handle.
fn draw_on(s: &mut Session, view: Id, layer: &str, from: [f64; 2], to: [f64; 2]) -> Result<Handle, Box<dyn std::error::Error>> {
    let before = block_handles(s.doc()?, view);
    s.execute("editview", &json!({"viewId": view.0}))?;
    s.execute("layer.current", &json!({"name": layer}))?;
    s.execute("line", &json!({"points": [from, to]}))?;
    Ok(block_handles(s.doc()?, view).into_iter().find(|h| !before.contains(h)).ok_or("line not drawn")?)
}

/// The fixture's first plan view and first non-plan (section) view.
fn plan_and_section(d: &Drawing) -> Result<(Id, Id), Box<dyn std::error::Error>> {
    let doc = read_document(d)?;
    let plan = doc.views.iter().find(|v| matches!(v, AnyView::Plan(_))).ok_or("no plan")?.id();
    let sec = doc.views.iter().find(|v| matches!(v, AnyView::Vertical(_))).ok_or("no section")?.id();
    Ok((plan, sec))
}

/// TS T256: a line on a cut-default layer is stored `cut` in a plan and `seen` in a section; any
/// other layer default stores nothing; one undo takes the line and its stamp together.
#[test]
fn new_lines_on_cut_layers_get_a_concrete_role() -> R {
    use lamb_core::model::LineRole;
    register();
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let (plan, sec) = plan_and_section(&d)?;
    let mut s = Session::new();
    s.open_drawing(d, "u-wall", None);
    install_link_maintenance(&mut s);

    let far = |k: f64| -> ([f64; 2], [f64; 2]) { ([-90000.0, -90000.0 + k], [-85000.0, -90000.0 + k]) };
    let (a, b) = far(0.0);
    let in_plan = draw_on(&mut s, plan, "WALL-EXT", a, b)?;
    assert_eq!(stored_role(&s, in_plan)?, LineRole::Cut, "cut layer in a plan → cut");
    let in_sec = draw_on(&mut s, sec, "WALL-EXT", a, b)?;
    assert_eq!(stored_role(&s, in_sec)?, LineRole::Seen, "cut layer in a section → seen");
    // The stamp rides in the LINE's own undo step: one undo takes both, redo restores both.
    s.undo()?;
    assert!(s.doc()?.entity(in_sec).is_none(), "one undo removes the line");
    s.redo()?;
    assert_eq!(stored_role(&s, in_sec)?, LineRole::Seen, "redo brings line and stamp back together");
    let (a, b) = far(1000.0);
    let grid = draw_on(&mut s, sec, "GRID", a, b)?;
    assert_eq!(stored_role(&s, grid)?, LineRole::Bylayer, "datum layer → no slot written");
    let note = draw_on(&mut s, plan, "ANNOTATION", a, b)?;
    assert_eq!(stored_role(&s, note)?, LineRole::Bylayer, "annotation layer → no slot written");

    Ok(())
}

/// TS T113: a line whose resolved role is reference never auto-links; the same line on a cut
/// layer does.
#[test]
fn reference_lines_never_auto_link() -> R {
    use lamb_core::model::LineRole;
    register();
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let original = read_document(&d)?.links;
    let mut s = Session::new();
    s.open_drawing(d, "u-wall", None);
    install_link_maintenance(&mut s);

    // Hollow a section marker by erasing a linked section line.
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
    s.execute("editview", &json!({"viewId": sec_view.0}))?;
    s.execute("erase", &json!({"handles": [hex(sec_end)]}))?;
    let marker = {
        let p = DrawingProject::new(s.doc()?)?;
        compute_markers_for_view(&p, sec_view)?.into_iter().find(|m| !m.filled).ok_or("no hollow marker")?
    };
    let layer = {
        let p = DrawingProject::new(s.doc()?)?;
        p.entity(marker.source_entity_id).ok_or("marker source missing")?.layer_id.to_owned()
    };
    // The source keeps a concrete role, so its marker survives the layer becoming reference.
    s.execute("setrole", &json!({"handles": [hex(marker.source_entity_id)], "role": "cut"}))?;
    s.execute("layerrole", &json!({"layer": layer, "role": "reference"}))?;
    {
        let p = DrawingProject::new(s.doc()?)?;
        let still = compute_markers_for_view(&p, sec_view)?;
        assert!(still.iter().any(|m| !m.filled && m.source_entity_id == marker.source_entity_id), "the hollow marker survives");
    }
    let (x, y) = (marker.position.x, marker.position.y);
    let through = ([x - 500.0, y - 500.0], [x + 500.0, y + 500.0]);
    let before = links(&s)?;
    let refline = draw_on(&mut s, sec_view, &layer, through.0, through.1)?;
    assert_eq!(stored_role(&s, refline)?, LineRole::Bylayer, "reference layer: nothing stamped");
    assert_eq!(links(&s)?, before, "a reference line forms no link");

    s.execute("erase", &json!({"handles": [refline.hex()]}))?;
    s.execute("layerrole", &json!({"layer": layer, "role": "cut"}))?;
    let cutline = draw_on(&mut s, sec_view, &layer, through.0, through.1)?;
    let created: Vec<LinkRecord> = links(&s)?.into_iter().filter(|l| !before.iter().any(|b| b.id == l.id)).collect();
    assert_eq!(created.len(), 1, "the same line on a cut layer links: {created:?}");
    assert!(on(&created[0], Id(cutline.0)));
    Ok(())
}

/// T469: PEDIT vertex insert, straighten and reverse on the linked plan wall (0,5000)-(0,0)-(10000,0)-
/// (10000,5000), each segment linked at its midpoint, keep every link on the segment that holds its
/// point; straightening away the vertex a link sits on drops it, and undo restores it.
#[test]
fn vertex_edits_keep_links_on_their_segments() -> R {
    register();
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let (_, plan_e, _, plan_view) = crossbar(&d)?;
    let original = read_document(&d)?.links;
    let mut s = Session::new();
    s.open_drawing(d, "u-wall", None);
    install_link_maintenance(&mut s);
    s.execute("editview", &json!({"viewId": plan_view.0}))?;
    let points =
        |s: &Session| -> Result<Vec<Point2>, Box<dyn std::error::Error>> { links(s)?.iter().map(|l| anchor_point(s.doc()?, l, plan_e)).collect() };
    let holds = |s: &Session| -> Result<(), Box<dyn std::error::Error>> {
        let p = DrawingProject::new(s.doc()?)?;
        for l in links(s)? {
            assert!(l.stale.is_none(), "link {} not stale", l.id);
            assert!(link_still_coincides(&p, &l)?, "link {} holds", l.id);
        }
        Ok(())
    };
    let same = |a: &[Point2], b: &[Point2]| a.len() == b.len() && a.iter().zip(b).all(|(p, q)| close(p.x, q.x) && close(p.y, q.y));
    let seg_of = |s: &Session, i: usize| -> Result<usize, Box<dyn std::error::Error>> {
        let l = links(s)?.into_iter().nth(i).ok_or("no link")?;
        Ok(if l.a.entity_id == plan_e { l.a.segment_index } else { l.b.segment_index })
    };
    let start = points(&s)?;
    let pedit = |s: &mut Session, args: serde_json::Value| -> R {
        let mut a = json!({"handle": hex(plan_e), "option": "vertex"});
        a.as_object_mut().ok_or("obj")?.extend(args.as_object().ok_or("obj")?.clone());
        s.execute("pedit", &a)?;
        Ok(())
    };

    // Insert (0,1000) inside segment 0: its anchor (0,2500) lands on piece 0; segments 1, 2 shift.
    pedit(&mut s, json!({"action": "insert", "index": 0, "to": [0, 1000]}))?;
    assert_eq!(poly(s.doc()?, plan_e)?.segments.len(), 4, "the vertex went in");
    holds(&s)?;
    assert!(same(&points(&s)?, &start), "anchor points unchanged: {:?} vs {start:?}", points(&s)?);
    assert_eq!((seg_of(&s, 0)?, seg_of(&s, 1)?, seg_of(&s, 2)?), (2, 0, 3));
    s.undo()?;
    assert_eq!(links(&s)?, original, "undo restores the anchors");

    // Insert a collinear (3000,0) before the crossbar's anchor, then straighten it away again.
    pedit(&mut s, json!({"action": "insert", "index": 1, "to": [3000, 0]}))?;
    holds(&s)?;
    assert!(same(&points(&s)?, &start));
    assert_eq!((seg_of(&s, 0)?, seg_of(&s, 1)?, seg_of(&s, 2)?), (2, 0, 3));
    pedit(&mut s, json!({"action": "straighten", "index": 1, "index2": 3}))?;
    assert_eq!(poly(s.doc()?, plan_e)?.segments.len(), 3, "the vertex came out");
    holds(&s)?;
    assert!(same(&points(&s)?, &start), "anchor points unchanged after the removal");
    assert_eq!(links(&s)?.len(), 3);
    s.undo()?;
    s.undo()?;
    assert_eq!(links(&s)?, original);

    // A vertex at the crossbar's own anchor (5000,0), then straightened away: that link drops,
    // the others stay; undo restores it.
    pedit(&mut s, json!({"action": "insert", "index": 1, "to": [5000, 0]}))?;
    holds(&s)?;
    assert_eq!(links(&s)?.len(), 3);
    let before_drop = links(&s)?;
    pedit(&mut s, json!({"action": "straighten", "index": 1, "index2": 3}))?;
    let after = links(&s)?;
    assert_eq!(after.len(), 2, "the link at the removed vertex dropped: {after:?}");
    assert!(after.iter().all(|l| l.id != original[0].id), "it is the crossbar's");
    holds(&s)?;
    s.undo()?;
    assert_eq!(links(&s)?, before_drop, "undo restores the dropped link");
    s.undo()?;
    assert_eq!(links(&s)?, original);

    // Reverse: every link kept, anchor points unchanged.
    s.execute("pedit", &json!({"handle": hex(plan_e), "option": "reverse"}))?;
    holds(&s)?;
    assert!(same(&points(&s)?, &start), "reverse keeps anchor points");
    assert_eq!((seg_of(&s, 0)?, seg_of(&s, 1)?, seg_of(&s, 2)?), (1, 2, 0));
    Ok(())
}

/// Every view-block entity's layer, by id.
fn layers_by_entity(d: &Drawing) -> Result<BTreeMap<Id, String>, Box<dyn std::error::Error>> {
    let p = DrawingProject::new(d)?;
    let mut out = BTreeMap::new();
    for v in p.views() {
        for e in p.entities(v.id) {
            out.insert(e.id, e.layer_id.to_owned());
        }
    }
    Ok(out)
}

/// T468: `properties.set` of the plan wall to a new layer moves its whole automatic-link component
/// (the linked lines of both sections) with it, in one undo step; an explicit link stops the walk.
#[test]
fn a_layer_change_carries_the_automatic_link_component() -> R {
    register();
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let (link, plan_e, sec_e, plan_view) = crossbar(&d)?;
    let plan_anchor = if link.a.entity_id == plan_e { link.a.clone() } else { link.b.clone() };
    let component = lamb_core::derivation::linking::linked_entity_component(&DrawingProject::new(&d)?, plan_e);
    let views: std::collections::BTreeSet<Id> = component.iter().map(|m| view_of(&d, *m)).collect::<Result<_, _>>()?;
    assert!(views.len() >= 3, "the component spans the plan and both sections: {views:?}");
    let mut s = Session::new();
    s.open_drawing(d, "u-wall", None);
    install_link_maintenance(&mut s);
    s.execute("layer.new", &json!({"name": "WALLS"}))?;

    // --- An explicit link from the plan wall to an otherwise unlinked section line through the
    // wall's marker (a link needs its lines to cross, LAMB T514).
    let sec_view = view_of(s.doc()?, sec_e)?;
    let at = markers_from(s.doc()?, sec_view, plan_e)?
        .into_iter()
        .find(|m| m.source_segment_index == plan_anchor.segment_index)
        .ok_or("no plan-wall marker in the section")?
        .position;
    let edge = Id(draw_edge(&mut s, sec_view, [at.x, at.y - 500.0], [at.x, at.y + 500.0])?.0);
    let anchor = |e: Id, i: usize, t: f64| json!({"entityId": hex(e), "segmentIndex": i, "segmentParam": t});
    s.execute("link", &json!({"a": anchor(plan_e, plan_anchor.segment_index, plan_anchor.segment_param), "b": anchor(edge, 0, 0.5)}))?;
    let before = layers_by_entity(s.doc()?)?;
    assert!(component.iter().all(|m| before[m] != "WALLS"));

    s.execute("editview", &json!({"viewId": plan_view.0}))?;
    s.execute("properties.set", &json!({"handles": [hex(plan_e)], "layer": "WALLS"}))?;
    let after = layers_by_entity(s.doc()?)?;
    for (id, layer) in &after {
        let expect = if component.contains(id) { "WALLS" } else { before[id].as_str() };
        assert_eq!(layer, expect, "entity {id}");
    }
    assert_eq!(after[&edge], before[&edge], "the explicitly linked line keeps its layer");

    s.undo()?;
    assert_eq!(layers_by_entity(s.doc()?)?, before, "one undo puts every one back");
    Ok(())
}

/// T468: two members of one component set to different layers relayer nothing else; an unlinked
/// view entity or a model-space entity relayers nothing.
#[test]
fn a_disagreeing_or_unlinked_layer_change_relayers_nothing() -> R {
    register();
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let (_, plan_e, sec_e, plan_view) = crossbar(&d)?;

    // Both changes land in one drawing (no observer), then one maintenance pass.
    let mut raw = Session::new();
    raw.open_drawing(d.clone(), "raw", None);
    raw.execute("layer.new", &json!({"name": "ONE"}))?;
    raw.execute("layer.new", &json!({"name": "TWO"}))?;
    raw.execute("editview", &json!({"viewId": plan_view.0}))?;
    raw.execute("properties.set", &json!({"handles": [hex(plan_e)], "layer": "ONE"}))?;
    raw.execute("properties.set", &json!({"handles": [hex(sec_e)], "layer": "TWO"}))?;
    let mut after = raw.doc()?.clone();
    let report = maintain_links(&d, &mut after, "t", &[])?;
    assert!(report.relayered.is_empty(), "{report:?}");
    let (old, new) = (layers_by_entity(&d)?, layers_by_entity(&after)?);
    for (id, layer) in &new {
        if *id != plan_e && *id != sec_e {
            assert_eq!(layer, &old[id], "entity {id} untouched");
        }
    }

    let mut s = Session::new();
    s.open_drawing(d, "u-wall", None);
    install_link_maintenance(&mut s);
    s.execute("layer.new", &json!({"name": "X"}))?;
    let model = {
        let model_handles = |s: &Session| -> Result<Vec<Handle>, Box<dyn std::error::Error>> {
            Ok(s.doc()?.space(&cadcraft_doc::Space::Model).map(|m| m.handles()).unwrap_or_default())
        };
        let before = model_handles(&s)?;
        s.execute("line", &json!({"points": [[0, 0], [100, 0]]}))?;
        model_handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("model line not drawn")?
    };
    let edge = draw_edge(&mut s, plan_view, [-90000.0, -90000.0], [-85000.0, -90000.0])?;
    assert!(!layers_by_entity(s.doc()?)?.contains_key(&Id(model.0)), "the model-space line is in no view block");
    for h in [edge, model] {
        let before = layers_by_entity(s.doc()?)?;
        let ext = read_document(s.doc()?)?;
        s.execute("properties.set", &json!({"handles": [h.hex()], "layer": "X"}))?;
        let now = layers_by_entity(s.doc()?)?;
        for (id, layer) in &now {
            if *id != Id(h.0) {
                assert_eq!(layer, &before[id], "entity {id} untouched by {}", h.hex());
            }
        }
        assert_eq!(read_document(s.doc()?)?.links, ext.links);
    }
    Ok(())
}

/// LAMB T541's fixture: a one-segment plan wall `wall` linked to segment 1 of the section face
/// `face`, the section's cut line along global +X at y = 0 with an identity transform (local x = u).
fn wall_and_face(wall: [[f64; 2]; 2], face: &[[f64; 2]]) -> Result<Drawing, Box<dyn std::error::Error>> {
    let poly = |pts: &[[f64; 2]]| {
        json!({"kind": "polyline", "geometry": {"closed": false, "start": {"x": pts[0][0], "y": pts[0][1]},
            "segments": pts[1..].iter().map(|p| json!({"bulge": 0, "to": {"x": p[0], "y": p[1]}})).collect::<Vec<_>>()}})
    };
    let ident = json!({"originX": 0, "originY": 0, "rotation": 0});
    let project: Project = serde_json::from_value(json!({
        "commandLog": [], "name": "T541", "patterns": {}, "schemaVersion": 3,
        "levels": [],
        "defaults": {"defaultCutHeightMm": 1000, "defaultFloorToFloorMm": 3000, "defaultSurfaceDepthMm": 2700,
            "defaultSurfaceHeightMm": 2700, "dimTextHeightPaperMm": 3, "plotScale": 50, "units": "mm"},
        "layers": [{"color": "#d0d0d0", "defaultRole": "cut", "id": "layer-0", "lineweight": 0.25, "locked": false, "name": "0", "visible": true}],
        "links": [{"a": {"entityId": 11, "segmentIndex": 0, "segmentParam": 0.5}, "b": {"entityId": 21, "segmentIndex": 1, "segmentParam": 0.5},
            "createdAt": "2026-01-01T00:00:00.000Z", "createdBy": {"kind": "user", "userId": "t"}, "id": 31, "origin": "automatic"}],
        "views": [
            {"id": 1, "kind": "plan", "name": "plan", "entities": [{"id": 11, "layerId": "layer-0", "role": "cut", "viewId": 1, "geometry": poly(&wall)}],
             "placement": {"kind": "plan", "cutElevation": 1200, "levelId": 41, "transform": ident}},
            {"id": 2, "kind": "vertical", "intent": "section", "name": "sec", "entities": [{"id": 21, "layerId": "layer-0", "role": "cut", "viewId": 2, "geometry": poly(face)}],
             "placement": {"kind": "vertical", "hostViewId": 1, "transform": ident,
                "cutLine": {"closed": false, "start": {"x": 0, "y": 0}, "segments": [{"bulge": 0, "to": {"x": 10000, "y": 0}}]}}}
        ]
    }))?;
    Ok(project_to_drawing(&project)?.0)
}

/// The fixture's one link, its plan entity and plan view.
fn wall_link(d: &Drawing) -> Result<(LinkRecord, Id, Id), Box<dyn std::error::Error>> {
    let link = read_document(d)?.links.into_iter().next().ok_or("no link")?;
    let plan_e = if is_plan(d, view_of(d, link.a.entity_id)?)? { link.a.entity_id } else { link.b.entity_id };
    Ok((link, plan_e, view_of(d, plan_e)?))
}

/// LAMB T541: a 45° plan wall moved along its own direction keeps its section crossing, so its
/// travelled link survives (the crossing shift is (d·n)/(c·n), not d·c).
#[test]
fn an_oblique_wall_slid_along_itself_keeps_its_link() -> R {
    register();
    // The face's crossing segment holds the wall's section marker (u = 5000, z = the 1200 cut) at
    // its link parameter, so the link holds from the section side whatever the wall's own anchor does.
    let d = wall_and_face([[0.0, -5000.0], [10000.0, 5000.0]], &[[4000.0, 0.0], [5000.0, 0.0], [5000.0, 2400.0], [6000.0, 2400.0]])?;
    let (link, plan_e, plan_view) = wall_link(&d)?;
    {
        let p = DrawingProject::new(&d)?;
        assert!(link_still_coincides(&p, &link)?, "the fixture's link holds");
    }
    let k = 100.0 / 2f64.sqrt();
    let (after, report) = raw_report(&d, plan_view, "move", &json!({"handles": [hex(plan_e)], "delta": [k, k]}), &[link.id])?;
    assert!(report.separated.is_empty() && report.unreachable.is_empty(), "{report:?}");
    assert!(read_document(&after)?.links.iter().any(|l| l.id == link.id), "the link survives");
    // Along its normal: the section line follows 100·√2 and the link still holds.
    let (after, report) = raw_report(&d, plan_view, "move", &json!({"handles": [hex(plan_e)], "delta": [-k, k]}), &[link.id])?;
    assert!(report.separated.is_empty() && report.travelled.len() == 1, "{report:?}");
    let sec_e = report.travelled[0];
    let x = resolved_segments(&poly(&after, sec_e)?)[1].from.x;
    assert!((x - (5000.0 - 100.0 * 2f64.sqrt())).abs() < 0.1, "{x}");
    Ok(())
}

/// LAMB T541: a section partner whose crossing segment continues straight into its neighbour
/// cannot absorb a move across them; the partial slide is applied and the unreachable length is
/// reported and echoed.
#[test]
fn an_unreachable_travel_is_reported_and_echoed() -> R {
    register();
    let d = wall_and_face([[5000.0, -5000.0], [5000.0, 5000.0]], &[[5000.0, 0.0], [5000.0, 1000.0], [5000.0, 3000.0], [6000.0, 3000.0]])?;
    let (link, plan_e, plan_view) = wall_link(&d)?;
    let mv = json!({"handles": [hex(plan_e)], "delta": [100, 0]});
    let (_, report) = raw_report(&d, plan_view, "move", &mv, &[link.id])?;
    assert_eq!(report.unreachable.len(), 1, "{report:?}");
    let sec_e = if link.a.entity_id == plan_e { link.b.entity_id } else { link.a.entity_id };
    assert_eq!(report.unreachable[0].0, sec_e);
    assert!(close(report.unreachable[0].1, 100.0), "{report:?}");

    let mut s = Session::new();
    s.open_drawing(d, "t541", None);
    install_link_maintenance(&mut s);
    s.execute("editview", &json!({"viewId": plan_view.0}))?;
    s.set_travel(vec![link.id.0]);
    s.execute("move", &mv)?;
    let line = format!("LAMB: 100 mm of the move could not travel to entity {} (its neighbours cannot absorb it)", hex(sec_e));
    assert!(s.log.contains(&line), "{:?}", s.log);
    Ok(())
}

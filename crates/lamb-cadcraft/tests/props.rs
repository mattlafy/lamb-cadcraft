//! The LAMB group of the Properties palette (`props::property_group`) on the imported
//! `u-wall-two-sections`: the Role row and its SETROLE choices, the cut wall's per-segment height
//! source matching `resolve_cut_segment`, `*VARIES*`, and `None` for a non-LAMB entity.

use std::collections::BTreeMap;
use std::path::Path;

use cadcraft_doc::{Drawing, Handle};
use cadcraft_engine::Session;
use lamb_cadcraft::props::{picked_view_group, property_group};
use lamb_cadcraft::{DrawingProject, commands, install, project_to_drawing, read_document, view_block_name};
use lamb_core::derivation::read::{ProjectRead, ViewKind};
use lamb_core::derivation::resolve::resolve_cut_segment;
use lamb_core::model::{AnyView, Id, LineRole, Placement, Project};
use lamb_core::persistence::files_to_project;
use serde_json::Value;

type R = Result<(), Box<dyn std::error::Error>>;

fn load(name: &str) -> Result<Project, Box<dyn std::error::Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb").join(format!("{name}.lamb.json"));
    let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(files_to_project(&files).map_err(|e| format!("{e:?}"))?)
}

fn session() -> Result<Session, Box<dyn std::error::Error>> {
    commands::install_app().map_err(|e| e.to_string())?;
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let mut s = Session::new();
    s.open_drawing(d, "u-wall", None);
    install(&mut s);
    Ok(s)
}

/// A multi-segment polyline in a placed plan whose resolved role is cut, and another LAMB entity.
fn cut_wall_and_other(d: &Drawing) -> Result<(Handle, usize, Handle), Box<dyn std::error::Error>> {
    let dp = DrawingProject::new(d)?;
    let mut wall = None;
    let mut other = None;
    for v in dp.views() {
        for e in dp.entities(v.id) {
            let n = e.polyline.as_ref().map_or(0, |p| p.segments.len());
            if wall.is_none() && v.kind == ViewKind::Plan && v.placement.is_some() && n > 1 && dp.role_of(&e) == LineRole::Cut {
                wall = Some((Handle(e.id.0), n));
            } else if other.is_none() {
                other = Some(Handle(e.id.0));
            }
        }
    }
    let (w, n) = wall.ok_or("no plan cut wall")?;
    Ok((w, n, other.ok_or("no second LAMB entity")?))
}

fn role_of(d: &Drawing, h: Handle) -> Result<LineRole, Box<dyn std::error::Error>> {
    let dp = DrawingProject::new(d)?;
    Ok(dp.entity(Id(h.0)).ok_or("not a LAMB entity")?.role)
}

#[test]
fn a_plan_cut_wall_shows_its_role_and_each_segments_height_source() -> R {
    let s = session()?;
    let d = s.doc()?;
    let (wall, n, _) = cut_wall_and_other(d)?;
    let dp = DrawingProject::new(d)?;
    let e = dp.entity(Id(wall.0)).ok_or("wall")?;
    for seg in [0, 1] {
        let g = property_group(d, &[wall], seg).ok_or("no group")?;
        assert_eq!((g.title.as_str(), g.segments), ("LAMB", n));
        let role = &g.rows[0];
        assert!(role.label == "Role" && (role.value == "Cut" || role.value == "Bylayer → Cut"), "{role:?}");
        assert_eq!(role.choices.len(), 7);
        let spans = resolve_cut_segment(&dp, &e, seg)?;
        let heights: Vec<_> = g.rows[1..].iter().collect();
        assert_eq!(heights.len(), spans.len(), "one Height source row per span");
        for (row, sp) in heights.iter().zip(&spans) {
            let word = serde_json::to_value(sp.provenance)?;
            assert_eq!(row.label, "Height source");
            assert!(row.value.starts_with(word.as_str().ok_or("word")?), "{row:?} vs {sp:?}");
            if let Some(x) = sp.extent {
                assert!(row.value.contains(&format!("{:.0}…{:.0} mm", x.bottom, x.top)), "{row:?} vs {sp:?}");
            }
        }
    }
    let past = property_group(d, &[wall], 99).ok_or("no group")?;
    assert_eq!(past, property_group(d, &[wall], n - 1).ok_or("no group")?, "a segment past the end clamps to the last");
    Ok(())
}

#[test]
fn varies_none_for_non_lamb_and_a_role_choice_sets_the_role_with_one_undo() -> R {
    let mut s = session()?;
    let (wall, _, other) = cut_wall_and_other(s.doc()?)?;
    // A model-space entity (a view's canonical insert) is not a LAMB entity.
    let foreign = s.doc()?.model.iter().map(|e| e.handle).find(|h| *h != wall).ok_or("no model entity")?;
    assert!(property_group(s.doc()?, &[wall, foreign], 0).is_none());
    s.execute("setrole", &serde_json::json!({ "handles": [other.hex()], "role": "datum" }))?;
    let g = property_group(s.doc()?, &[wall, other], 0).ok_or("no group")?;
    assert_eq!((g.rows[0].value.as_str(), g.segments, g.rows.len()), ("*VARIES*", 0, 1), "two lines: Role only");
    // The Seen choice is the command the palette runs.
    let g = property_group(s.doc()?, &[wall], 0).ok_or("no group")?;
    let (_, id, params): &(String, String, Value) = g.rows[0].choices.iter().find(|c| c.0 == "Seen").ok_or("no Seen")?;
    let before = role_of(s.doc()?, wall)?;
    s.execute(id, params)?;
    assert_eq!(role_of(s.doc()?, wall)?, LineRole::Seen);
    let g = property_group(s.doc()?, &[wall], 0).ok_or("no group")?;
    assert_eq!(g.rows[0].value, "Seen");
    assert_eq!(g.rows[1].label, "Z source", "a seen line shows its z source");
    s.undo()?;
    assert_eq!(role_of(s.doc()?, wall)?, before, "one UNDO restores the role");
    Ok(())
}

/// The placed plan and a placed vertical view.
fn plan_and_section(d: &Drawing) -> Result<(Id, Id), Box<dyn std::error::Error>> {
    let doc = read_document(d)?;
    let plan = doc.views.iter().find(|v| matches!(v, AnyView::Plan(_)) && v.placement().is_some()).ok_or("no placed plan")?.id();
    let sec = doc.views.iter().find(|v| matches!(v, AnyView::Vertical(_)) && v.placement().is_some()).ok_or("no placed section")?.id();
    Ok((plan, sec))
}

fn view_name(d: &Drawing, id: Id) -> Result<String, Box<dyn std::error::Error>> {
    Ok(read_document(d)?.views.iter().find(|v| v.id() == id).ok_or("view gone")?.name().to_owned())
}

fn flipped(d: &Drawing, id: Id) -> Result<bool, Box<dyn std::error::Error>> {
    let doc = read_document(d)?;
    let v = doc.views.iter().find(|v| v.id() == id).ok_or("view gone")?;
    let Some(Placement::Vertical { flipped, .. }) = v.placement() else { return Err("not placed vertical".into()) };
    Ok(flipped.unwrap_or(false))
}

#[test]
fn the_picked_views_form_rows_run_viewprops_with_one_undo_each() -> R {
    let mut s = session()?;
    let (plan, sec) = plan_and_section(s.doc()?)?;
    assert!(picked_view_group(s.doc()?, "LAMB_VIEW_999999").is_none(), "unknown view");
    assert!(picked_view_group(s.doc()?, "SOMEBLOCK").is_none(), "not a view key");

    let g = picked_view_group(s.doc()?, &view_block_name(plan)).ok_or("no plan form")?;
    assert_eq!(g.title, "Plan");
    let labels: Vec<_> = g.rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, ["Name", "Cut elevation"]);
    let name = g.rows[0].edit.as_ref().ok_or("Name not editable")?;
    assert_eq!((name.command.as_str(), name.key.as_str(), name.number), ("viewprops", "name", false));
    assert_eq!(name.params, serde_json::json!({ "mode": "rename", "viewId": plan.0 }));
    let cut = g.rows[1].edit.as_ref().ok_or("Cut elevation not editable")?;
    assert_eq!((cut.key.as_str(), cut.number), ("cutElevation", true));
    assert_eq!(cut.params, serde_json::json!({ "mode": "cutElevation", "viewId": plan.0 }));

    // The Name edit, as the palette commits it.
    let before = view_name(s.doc()?, plan)?;
    let mut p = name.params.clone();
    p["name"] = "Ground".into();
    s.execute(&name.command, &p)?;
    assert_eq!(view_name(s.doc()?, plan)?, "Ground");
    assert_eq!(picked_view_group(s.doc()?, &view_block_name(plan)).ok_or("form")?.rows[0].value, "Ground");
    s.undo()?;
    assert_eq!(view_name(s.doc()?, plan)?, before, "one UNDO restores the name");

    let g = picked_view_group(s.doc()?, &view_block_name(sec)).ok_or("no section form")?;
    let labels: Vec<_> = g.rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, ["Name", "Intent", "Flip"]);
    assert!(g.rows[0].edit.is_some());
    let intent: Vec<_> = g.rows[1].choices.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(intent, ["Section", "Elevation"]);
    assert_eq!(g.title, g.rows[1].value, "the title is the kind word, which for a vertical is its intent");
    let flip = &g.rows[2];
    assert_eq!(flip.value, "Not flipped");
    let (label, id, params) = flip.choices.first().ok_or("no Flip choice")?;
    assert_eq!(label, "Flip");
    assert_eq!(params, &serde_json::json!({ "mode": "flip", "viewId": sec.0, "flipped": true }));
    s.execute(id, params)?;
    assert!(flipped(s.doc()?, sec)?);
    assert_eq!(picked_view_group(s.doc()?, &view_block_name(sec)).ok_or("form")?.rows[2].value, "Flipped");
    s.undo()?;
    assert!(!flipped(s.doc()?, sec)?, "one UNDO restores the flip");
    Ok(())
}

#[test]
fn a_selected_layout_viewport_shows_a_view_row_running_vpview() -> R {
    let mut s = session()?;
    let views: Vec<(Id, String)> = read_document(s.doc()?)?.views.iter().map(|v| (v.id(), v.name().to_owned())).collect();
    let (plan, _) = plan_and_section(s.doc()?)?;
    s.execute("layout.new", &serde_json::json!({ "name": "L1", "viewport": false }))?;
    let out = s.execute("mview", &serde_json::json!({ "layout": "L1" }))?;
    let h = Handle::parse_hex(out["viewports"][0].as_str().ok_or("no viewport")?).ok_or("bad handle")?;

    let g = property_group(s.doc()?, &[h], 0).ok_or("no group for a viewport")?;
    assert_eq!((g.title.as_str(), g.rows.len()), ("LAMB", 1));
    let row = &g.rows[0];
    assert_eq!((row.label.as_str(), row.value.as_str()), ("View", "(none)"));
    let labels: Vec<&str> = row.choices.iter().map(|c| c.0.as_str()).collect();
    let want: Vec<&str> = std::iter::once("(none)").chain(views.iter().map(|v| v.1.as_str())).collect();
    assert_eq!(labels, want);
    assert!(row.choices.iter().all(|c| c.1 == "vpview"));
    assert_eq!(row.choices[0].2, serde_json::json!({ "handle": h.hex(), "viewId": null }));

    let (_, cmd, params) = row.choices.iter().find(|c| c.2["viewId"] == serde_json::json!(plan.0)).ok_or("no plan choice")?;
    s.execute(cmd, params)?;
    let g = property_group(s.doc()?, &[h], 0).ok_or("no group")?;
    assert_eq!(g.rows[0].value, view_name(s.doc()?, plan)?);
    s.undo()?;
    assert_eq!(property_group(s.doc()?, &[h], 0).ok_or("no group")?.rows[0].value, "(none)");
    Ok(())
}

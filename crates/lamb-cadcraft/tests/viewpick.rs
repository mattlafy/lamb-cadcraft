//! Clicking a cut line in a placed plan picks that section's view (`viewpick`), on the imported
//! `u-wall-two-sections`: entities win, a click away from every cut line opens a window as
//! before, and a section has no cut lines to pick.

use std::collections::BTreeMap;
use std::path::Path;

use cadcraft_engine::Session;
use cadcraft_geom::Vec2;
use lamb_cadcraft::viewpick::picked_view;
use lamb_cadcraft::{DrawingProject, commands, install, project_to_drawing};
use lamb_core::derivation::markers::{CutLineOverlay, compute_cut_lines_for_view};
use lamb_core::derivation::read::{ProjectRead, ViewKind};
use lamb_core::model::{Id, Project};
use lamb_core::persistence::files_to_project;
use serde_json::json;

type R = Result<(), Box<dyn std::error::Error>>;

fn load(name: &str) -> Result<Project, Box<dyn std::error::Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb").join(format!("{name}.lamb.json"));
    let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(files_to_project(&files).map_err(|e| format!("{e:?}"))?)
}

/// The session editing the placed plan, the plan's id and its cut lines.
fn editing_plan() -> Result<(Session, Id, Vec<CutLineOverlay>), Box<dyn std::error::Error>> {
    commands::install_app().map_err(|e| e.to_string())?;
    let (d, _, _) = project_to_drawing(&load("u-wall-two-sections")?)?;
    let mut s = Session::new();
    s.open_drawing(d, "u-wall", None);
    install(&mut s);
    let (plan, cuts) = {
        let dp = DrawingProject::new(s.doc()?)?;
        let plan = dp.views().into_iter().find(|v| v.kind == ViewKind::Plan && v.placement.is_some()).ok_or("no placed plan")?.id;
        (plan, compute_cut_lines_for_view(&dp, plan)?)
    };
    assert!(!cuts.is_empty(), "the plan shows its sections' cut lines");
    s.execute("editview", &json!({"viewId": plan.0}))?;
    Ok((s, plan, cuts))
}

/// A point a fifth of the way along a cut line (the two sections' cut lines cross at their
/// midpoints in this fixture).
fn on(c: &CutLineOverlay) -> Vec2 {
    Vec2::new(c.from.x + (c.to.x - c.from.x) * 0.2, c.from.y + (c.to.y - c.from.y) * 0.2)
}

#[test]
fn clicking_a_cut_line_picks_its_section() -> R {
    let (mut s, _, cuts) = editing_plan()?;
    for c in &cuts {
        s.idle_click(on(c), false)?;
        assert_eq!(picked_view(&s), Some(c.source_view_id), "{}", c.name);
        assert_eq!(s.log.last().map(String::as_str), Some(format!("Selected {}", c.name).as_str()));
        assert!(s.selection().is_empty());
        assert!(s.pending_window.is_none(), "no selection window opened");
    }
    Ok(())
}

#[test]
fn a_click_far_from_every_cut_line_opens_a_window() -> R {
    let (mut s, _, cuts) = editing_plan()?;
    s.idle_click(on(&cuts[0]), false)?;
    assert!(picked_view(&s).is_some());
    s.idle_click(Vec2::new(1.0e7, 1.0e7), false)?;
    assert_eq!(picked_view(&s), None);
    assert!(s.pending_window.is_some());
    Ok(())
}

#[test]
fn a_click_on_a_wall_selects_the_wall_not_a_view() -> R {
    let (mut s, plan, cuts) = editing_plan()?;
    s.idle_click(on(&cuts[0]), false)?;
    let start = {
        let dp = DrawingProject::new(s.doc()?)?;
        let e = dp.entities(plan).into_iter().find(|e| e.polyline.is_some()).ok_or("no plan line")?;
        e.polyline.ok_or("no polyline")?.start
    };
    let logged = s.log.len();
    s.idle_click(Vec2::new(start.x, start.y), false)?;
    assert_eq!(s.selection().len(), 1, "the wall is selected");
    assert_eq!(picked_view(&s), None);
    assert!(!s.log[logged..].iter().any(|m| m.starts_with("Selected ")), "hook not consulted");
    Ok(())
}

#[test]
fn editing_a_section_picks_nothing() -> R {
    let (mut s, _, cuts) = editing_plan()?;
    s.execute("editview", &json!({"viewId": cuts[0].source_view_id.0}))?;
    s.idle_click(on(&cuts[0]), false)?;
    assert_eq!(picked_view(&s), None);
    assert!(s.pending_window.is_some() || !s.selection().is_empty());
    Ok(())
}

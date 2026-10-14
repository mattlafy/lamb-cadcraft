//! A picked view's cut line draws in the selected colour (TS T91 `SELECTED_OVERLAY_COLOR`), on the
//! imported `u-wall-two-sections`. One test in its own binary: the highlight mirror
//! (`viewpick::highlighted`) is process-global.

use std::collections::BTreeMap;
use std::path::Path;

use cadcraft_color::Rgb;
use cadcraft_doc::Handle;
use cadcraft_engine::Session;
use cadcraft_engine::render::{Options, overlays_generation};
use cadcraft_geom::Vec2;
use lamb_cadcraft::{DrawingProject, commands, install, lamb_overlay, project_to_drawing};
use lamb_core::derivation::markers::{CutLineOverlay, compute_cut_lines_for_view};
use lamb_core::derivation::read::{ProjectRead, ViewKind};
use lamb_core::model::{Id, Project};
use lamb_core::persistence::files_to_project;
use serde_json::json;

type R = Result<(), Box<dyn std::error::Error>>;

const CUT_LINE_COLOR: Rgb = Rgb(255, 170, 0);
const SELECTED_OVERLAY_COLOR: Rgb = Rgb(0x4d, 0xa3, 0xff);

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
    s.execute("editview", &json!({"viewId": plan.0}))?;
    Ok((s, plan, cuts))
}

/// A point a fifth of the way along a cut line (the two cut lines cross at their midpoints).
fn on(c: &CutLineOverlay) -> Vec2 {
    Vec2::new(c.from.x + (c.to.x - c.from.x) * 0.2, c.from.y + (c.to.y - c.from.y) * 0.2)
}

/// The colour of the cut-line prims of view `v` in the edited space's overlay (all must agree).
fn colour(s: &Session, v: Id) -> Result<Rgb, Box<dyn std::error::Error>> {
    let list = lamb_overlay(s.doc()?, &s.space(), &Options::default());
    let cs: Vec<Rgb> = list.prims.iter().filter(|p| p.handle == Handle(v.0)).map(|p| p.color).collect();
    let first = *cs.first().ok_or("no cut-line prim for the view")?;
    assert!(cs.iter().all(|c| *c == first), "one colour per view");
    Ok(first)
}

#[test]
fn the_picked_views_cut_line_draws_highlighted() -> R {
    let (mut s, _, cuts) = editing_plan()?;
    assert!(cuts.len() >= 2, "two sections");
    let (a, b) = (cuts[0].source_view_id, cuts[1].source_view_id);
    assert_eq!(colour(&s, a)?, CUT_LINE_COLOR);
    assert_eq!(colour(&s, b)?, CUT_LINE_COLOR);

    let g = overlays_generation();
    s.idle_click(on(&cuts[0]), false)?;
    assert!(overlays_generation() > g, "the pick redraws the overlays");
    assert_eq!(colour(&s, a)?, SELECTED_OVERLAY_COLOR);
    assert_eq!(colour(&s, b)?, CUT_LINE_COLOR);

    s.set_selection(vec![]);
    assert_eq!(colour(&s, a)?, CUT_LINE_COLOR);
    assert_eq!(colour(&s, b)?, CUT_LINE_COLOR);

    s.idle_click(on(&cuts[1]), false)?;
    assert_eq!(colour(&s, a)?, CUT_LINE_COLOR);
    assert_eq!(colour(&s, b)?, SELECTED_OVERLAY_COLOR);
    Ok(())
}

//! Pane relations (LAMB T323) on imported fixtures: the rotation a plan tile takes for a section in
//! another tile, where the active tile's point lands in the others (the crosshair), and the
//! camera-lock centre, and PANELOCK driving it. Tiles come from `-vports`, each showing a view through EDITVIEW.
//!
//! The command and relation registries are process-global, so registration goes through one `Once`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Once;

use cadcraft_doc::Space;
use cadcraft_engine::panes::PaneMap;
use cadcraft_engine::{Session, View, register_command_provider};
use cadcraft_geom::Vec2;
use lamb_cadcraft::{DrawingProject, commands, panes, project_to_drawing, read_document, register_providers, view_block_name};
use lamb_core::derivation::projection::{Uz, vertical_local_to_uz, vertical_uz_to_local};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::geometry::Point2;
use lamb_core::model::{AnyView, Id, Project};
use lamb_core::persistence::files_to_project;
use serde_json::json;

type R = Result<(), Box<dyn std::error::Error>>;

const EPS: f64 = 1e-9;

fn load(name: &str) -> Result<Project, Box<dyn std::error::Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb").join(format!("{name}.lamb.json"));
    let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(files_to_project(&files).map_err(|e| format!("{e:?}"))?)
}

static REGISTER: Once = Once::new();

fn register() {
    REGISTER.call_once(|| {
        register_command_provider(commands).expect("register LAMB commands");
        register_providers().expect("register LAMB providers");
    });
}

/// The fixture opened in a session with `tiles` model tiles; the plan view and the vertical views
/// (with their cut lines' plan-local start and end), in document order.
struct Fixture {
    s: Session,
    plan: Id,
    secs: Vec<(Id, Point2, Point2)>,
}

fn open(name: &str, tiles: u32) -> Result<Fixture, Box<dyn std::error::Error>> {
    register();
    let (d, _, _) = project_to_drawing(&load(name)?)?;
    let doc = read_document(&d)?;
    let mut plan = None;
    let mut secs = Vec::new();
    for v in &doc.views {
        match v {
            AnyView::Plan(_) => plan = Some(v.id()),
            AnyView::Vertical(_) => {
                let dp = DrawingProject::new(&d)?;
                let cut = dp.view(v.id()).and_then(|r| r.cut_line()).ok_or("no cut line")?;
                secs.push((v.id(), cut.start, cut.segments[0].to));
            }
            AnyView::Detail(_) => {}
        }
    }
    let mut s = Session::new();
    s.open_drawing(d, name, None);
    s.execute("-vports", &json!({"option": tiles.to_string()}))?;
    assert_eq!(s.model_tiles().len(), tiles as usize);
    Ok(Fixture { s, plan: plan.ok_or("no plan")?, secs })
}

fn show(s: &mut Session, tile: usize, view: Id) -> R {
    s.activate_tile(tile)?;
    s.execute("editview", &json!({"viewId": view.0}))?;
    assert_eq!(s.space(), Space::Block(view_block_name(view)));
    Ok(())
}

fn close(a: Vec2, b: Vec2) -> bool {
    (a.x - b.x).abs() < EPS && (a.y - b.y).abs() < EPS
}

/// A vertical view's local point at (u, z).
fn uz_local(s: &Session, view: Id, u: f64, z: f64) -> Result<Vec2, Box<dyn std::error::Error>> {
    let dp = DrawingProject::new(s.doc()?)?;
    let p = vertical_uz_to_local(&dp.view(view).ok_or("no view")?, Uz { u, z });
    Ok(Vec2::new(p.x, p.y))
}

fn uz_of(s: &Session, view: Id, p: Vec2) -> Result<Uz, Box<dyn std::error::Error>> {
    let dp = DrawingProject::new(s.doc()?)?;
    Ok(vertical_local_to_uz(&dp.view(view).ok_or("no view")?, Point2 { x: p.x, y: p.y }))
}

#[test]
fn rotated_section_turns_the_plan_tile() -> R {
    let mut f = open("t76-rotated-section", 2)?;
    let (sec, a, b) = f.secs[0];
    show(&mut f.s, 0, f.plan)?;
    show(&mut f.s, 1, sec)?;
    let want = (b.y - a.y).atan2(b.x - a.x);
    assert!(want.abs() > 0.1, "the fixture's cut line is not axis-aligned");
    let r = f.s.tile_rotations();
    assert_eq!(r.len(), 2);
    assert!((r[0] - want).abs() < EPS, "plan tile rotation {} vs cut direction {want}", r[0]);
    assert_eq!(r[1], 0.0, "the section tile does not rotate");
    // Same answer with the plan tile current: it falls back to the first related tile.
    f.s.activate_tile(0)?;
    let r = f.s.tile_rotations();
    assert!((r[0] - want).abs() < EPS && r[1] == 0.0, "{r:?}");
    Ok(())
}

#[test]
fn crosshairs_and_centre_on_u_wall() -> R {
    let mut f = open("u-wall-two-sections", 3)?;
    let ((sa, a0, a1), (sb, _, _)) = (f.secs[0], f.secs[1]);
    let plan = f.plan;
    // Tile 0 = section A, tile 1 = plan, tile 2 stays Model, then section A current.
    show(&mut f.s, 1, plan)?;
    show(&mut f.s, 0, sa)?;
    assert_eq!(f.s.tile_spaces()[2], Space::Model);

    // Section A point -> a point on the plan's cut line, u along it; the Model tile has none.
    let (u, z) = (1234.5, 678.0);
    let p = uz_local(&f.s, sa, u, z)?;
    let c = f.s.tile_crosshairs(p);
    assert_eq!(c.len(), 3);
    assert_eq!(c[0], None, "no crosshair in the current tile");
    assert_eq!(c[2], None, "a Model tile relates to nothing");
    let Some(PaneMap::Point(q)) = c[1] else { return Err(format!("plan crosshair {:?}", c[1]).into()) };
    let len = (a1.x - a0.x).hypot(a1.y - a0.y);
    let along = Vec2::new(a0.x + (a1.x - a0.x) * u / len, a0.y + (a1.y - a0.y) * u / len);
    assert!(close(q, along), "{q:?} vs {along:?} on the cut line");

    // Section B in tile 2: section A -> B lands where the cut lines cross, at the same z.
    show(&mut f.s, 2, sb)?;
    f.s.activate_tile(0)?;
    assert_eq!(f.s.space(), Space::Block(view_block_name(sa)));
    let c = f.s.tile_crosshairs(p);
    let Some(PaneMap::Point(qb)) = c[2] else { return Err(format!("section B crosshair {:?}", c[2]).into()) };
    let uzb = uz_of(&f.s, sb, qb)?;
    assert!((uzb.z - z).abs() < EPS, "z carried: {} vs {z}", uzb.z);
    // u in B = where A's cut line crosses B's: both lines here are axis-aligned through (5000, 2500).
    let cross = Vec2::new(5000.0, 2500.0);
    let (_, b0, b1) = f.secs[1];
    let blen = (b1.x - b0.x).hypot(b1.y - b0.y);
    let ub = ((cross.x - b0.x) * (b1.x - b0.x) + (cross.y - b0.y) * (b1.y - b0.y)) / blen;
    assert!((uzb.u - ub).abs() < EPS, "u at the crossing: {} vs {ub}", uzb.u);

    // Plan current: a plan point -> a vertical line in each section, at the point's u.
    f.s.activate_tile(1)?;
    let pp = Vec2::new(5000.0, 1000.0);
    let c = f.s.tile_crosshairs(pp);
    assert_eq!(c[1], None);
    for (tile, (sec, s0, s1)) in [(0, f.secs[0]), (2, f.secs[1])] {
        let Some(PaneMap::Line { through, dir }) = c[tile] else { return Err(format!("tile {tile}: {:?}", c[tile]).into()) };
        assert!(close(dir, Vec2::new(0.0, 1.0)), "vertical line in tile {tile}: {dir:?}");
        let slen = (s1.x - s0.x).hypot(s1.y - s0.y);
        let u = ((pp.x - s0.x) * (s1.x - s0.x) + (pp.y - s0.y) * (s1.y - s0.y)) / slen;
        assert!((uz_of(&f.s, sec, through)?.u - u).abs() < EPS, "tile {tile} line at u {u}");
    }

    // Camera lock, plan -> section A: u from the plan point, z kept from the previous centre.
    let d = f.s.doc()?;
    let prev = uz_local(&f.s, sa, 50.0, 2222.0)?;
    let (pspace, aspace) = (Space::Block(view_block_name(plan)), Space::Block(view_block_name(sa)));
    let got = panes::centre(d, &pspace, pp, &aspace, prev).ok_or("no centre")?;
    let uz = uz_of(&f.s, sa, got)?;
    assert!((uz.z - 2222.0).abs() < EPS, "z kept: {}", uz.z);
    let u = ((pp.x - a0.x) * (a1.x - a0.x) + (pp.y - a0.y) * (a1.y - a0.y)) / len;
    assert!((uz.u - u).abs() < EPS, "u from the plan point: {} vs {u}", uz.u);
    assert_eq!(panes::centre(d, &Space::Model, pp, &aspace, prev), None, "Model relates to nothing");
    // Plan <-> section are unrelated for rotation the other way round, and Model never rotates.
    assert_eq!(panes::rotation(d, &aspace, &pspace), None);
    assert_eq!(panes::rotation(d, &Space::Model, &aspace), None);
    Ok(())
}

#[test]
fn panelock_recentres_the_section_tile() -> R {
    let mut f = open("u-wall-two-sections", 2)?;
    let (sa, a0, a1) = f.secs[0];
    let plan = f.plan;
    show(&mut f.s, 0, plan)?;
    show(&mut f.s, 1, sa)?;
    f.s.activate_tile(0)?;
    assert!(!f.s.panes_locked(), "off by default");
    let r = f.s.execute("panelock", &json!({}))?;
    assert_eq!(r["on"], json!(true));
    assert_eq!(r["message"], json!("<Pane lock on>"));

    let prev = f.s.model_tiles()[1].view;
    let z_prev = uz_of(&f.s, sa, prev.center)?.z;
    let pc = Vec2::new(4321.0, 1500.0);
    assert!(f.s.state_mut()?.set_view(View { center: pc, height: 3000.0 }));
    let tiles = f.s.model_tiles();
    let got = tiles[1].view;
    let uz = uz_of(&f.s, sa, got.center)?;
    let len = (a1.x - a0.x).hypot(a1.y - a0.y);
    let u = ((pc.x - a0.x) * (a1.x - a0.x) + (pc.y - a0.y) * (a1.y - a0.y)) / len;
    assert!((uz.u - u).abs() < EPS, "u from the plan centre: {} vs {u}", uz.u);
    assert!((uz.z - z_prev).abs() < EPS, "z kept: {} vs {z_prev}", uz.z);
    let fh = |i: usize| tiles[i].max.y - tiles[i].min.y;
    assert!((got.height - 3000.0 * fh(1) / fh(0)).abs() < EPS, "scaled height {}", got.height);

    // Off: tile 1 no longer follows.
    let r = f.s.execute("panelock", &json!({}))?;
    assert_eq!(r["on"], json!(false));
    assert_eq!(r["message"], json!("<Pane lock off>"));
    f.s.state_mut()?.set_view(View { center: Vec2::new(100.0, 200.0), height: 500.0 });
    assert_eq!(f.s.model_tiles()[1].view, got);

    // Re-tiling keeps the lock.
    f.s.execute("panelock", &json!({"on": true}))?;
    f.s.execute("-vports", &json!({"option": "3", "applyTo": "display"}))?;
    assert_eq!(f.s.model_tiles().len(), 3);
    assert!(f.s.panes_locked(), "re-tiling keeps the lock");
    f.s.activate_tile(1)?;
    assert!(f.s.panes_locked());
    Ok(())
}

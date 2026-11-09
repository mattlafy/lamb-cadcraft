//! The drafting toggles on the status bar are transparent, as F7-F12 are: clicking Grid or Snap
//! while drawing a line changes the setting and leaves LINE running (#1108).

use cadcraft_engine::doc::{Drawing, EntityKind, Space};
use cadcraft_engine::geom::Vec2;
use cadcraft_engine::{Input, Session};
use cadcraft_ui_egui::{CadApp, Services};
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

fn harness() -> Harness<'static, CadApp> {
    let mut session = Session::empty();
    session.open_drawing(Drawing::new_imperial(), "Status bar toggles", None);
    let app = CadApp::new(session, Services::default());
    let mut h = Harness::builder().with_size(vec2(1600.0, 1000.0)).build_ui_state(
        |ui, app: &mut CadApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(3);
    h
}

fn running(h: &Harness<'_, CadApp>) -> Option<String> {
    h.state().session.running.as_ref().map(|r| r.id.clone())
}

#[test]
fn clicking_grid_and_snap_on_the_status_bar_keeps_line_running() {
    let mut h = harness();
    let (grid, snap) = (h.state().session.settings.gridmode, h.state().session.settings.snapmode);
    {
        let s = &mut h.state_mut().session;
        s.start("line").unwrap();
        s.input(Input::Point(Vec2::new(0.0, 0.0))).unwrap();
    }
    h.run_steps(2);
    h.get_by_label("Grid (F7)").click();
    h.run_steps(2);
    h.get_by_label("Snap Mode (F9)").click();
    h.run_steps(2);
    assert_eq!(h.state().session.settings.gridmode, !grid);
    assert_eq!(h.state().session.settings.snapmode, !snap);
    assert_eq!(running(&h).as_deref(), Some("line"));
    h.state_mut().session.input(Input::Point(Vec2::new(10.0, 0.0))).unwrap();
    let lines: Vec<_> = h
        .state()
        .session
        .state()
        .unwrap()
        .doc
        .space(&Space::Model)
        .unwrap()
        .iter()
        .filter_map(|e| match &e.kind {
            EntityKind::Line(l) => Some((l.a.xy(), l.b.xy())),
            _ => None,
        })
        .collect();
    assert_eq!(lines, vec![(Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0))]);
}

fn osmode(h: &Harness<'_, CadApp>) -> u32 {
    h.state().session.settings.osmode
}

fn open_osnap_menu(h: &mut Harness<'_, CadApp>) {
    h.get_by_label("Object Snap menu").click();
    h.run_steps(2);
}

/// The chevron beside OSNAP opens a menu of the running snaps (LAMB T679): ticking one sets its
/// bit and turns OSNAP on, unticking clears only its bit, several ticks fit in one opening, and
/// LINE keeps running throughout.
#[test]
fn the_object_snap_menu_toggles_single_snaps_and_keeps_line_running() {
    use cadcraft_engine::snap::mode;
    let mut h = harness();
    {
        let s = &mut h.state_mut().session;
        s.settings.osmode = mode::OFF;
        s.start("line").unwrap();
        s.input(Input::Point(Vec2::new(0.0, 0.0))).unwrap();
    }
    h.run_steps(2);
    open_osnap_menu(&mut h);
    h.get_by_label("Midpoint").click();
    h.run_steps(2);
    assert_eq!(osmode(&h), mode::MID, "MID set, OSNAP turned on");
    h.state_mut().session.settings.osmode |= mode::OFF;
    h.run_steps(2);
    h.get_by_label("Endpoint").click();
    h.run_steps(2);
    assert_eq!(osmode(&h), mode::MID | mode::END, "the menu stayed open; END set and OFF cleared");
    h.get_by_label("Midpoint").click();
    h.run_steps(2);
    assert_eq!(osmode(&h), mode::END, "unticking clears only MID");
    assert_eq!(running(&h).as_deref(), Some("line"));
}

/// Hatch snaps flips OSNAPHATCH; Osnap Settings... opens Drafting Settings. Nothing registers
/// menu extras in this binary, so the menu has none.
#[test]
fn the_object_snap_menu_flips_hatch_snaps_and_opens_drafting_settings() {
    let mut h = harness();
    let hatch = h.state().session.settings.osnaphatch;
    open_osnap_menu(&mut h);
    assert!(cadcraft_ui_egui::chrome::statusext::osnap_extras(h.state()).is_empty(), "unregistered: no extras");
    h.get_by_label("Hatch snaps").click();
    h.run_steps(2);
    assert_eq!(h.state().session.settings.osnaphatch, !hatch);
    h.get_by_label("Osnap Settings...").click();
    h.run_steps(2);
    assert_eq!(h.state().ui.dialog.as_deref(), Some("dsettings"));
}

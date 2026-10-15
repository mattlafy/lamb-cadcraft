//! Fast drafting: every click on the canvas places a point, however fast it comes (#815).

use cadcraft_engine::Session;
use cadcraft_engine::doc::{Drawing, EntityKind, Space};
use cadcraft_engine::geom::Vec2;
use cadcraft_ui_egui::{CadApp, Services};
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use serde_json::json;

fn harness() -> Harness<'static, CadApp> {
    let mut session = Session::empty();
    session.open_drawing(Drawing::new_imperial(), "Rapid clicks", None);
    session.execute("view.set", &json!({"center":[50,50],"height":100})).unwrap();
    session.settings.osmode = 0;
    session.settings.snapmode = false;
    session.settings.orthomode = false;
    let mut app = CadApp::new(session, Services::default());
    app.canvas.zoom_pending = false;
    let mut h = Harness::builder().with_size(vec2(1600.0, 1000.0)).build_ui_state(
        |ui, app: &mut CadApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(5);
    h
}

fn screen(h: &Harness<'_, CadApp>, p: Vec2) -> Pos2 {
    h.state().canvas.xf.unwrap().to_screen(p)
}

fn button(h: &mut Harness<'_, CadApp>, pos: Pos2, pressed: bool) {
    h.event(Event::PointerMoved(pos));
    h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
}

fn start_line_at(h: &mut Harness<'_, CadApp>, first: Vec2) {
    h.state_mut().start("line");
    h.run_steps(2);
    assert_eq!(h.state().session.running.as_ref().unwrap().id, "line");
    let pos = screen(h, first);
    button(h, pos, true);
    h.run_steps(1);
    button(h, pos, false);
    h.run_steps(2);
}

fn lines(h: &Harness<'_, CadApp>) -> Vec<(Vec2, Vec2)> {
    let st = h.state().session.state().unwrap();
    st.doc
        .space(&Space::Model)
        .unwrap()
        .iter()
        .filter_map(|e| match &e.kind {
            EntityKind::Line(l) => Some((l.a.xy(), l.b.xy())),
            _ => None,
        })
        .collect()
}

#[test]
fn two_clicks_within_one_frame_place_two_points() {
    let mut h = harness();
    start_line_at(&mut h, Vec2::new(10.0, 10.0));
    let (a, b) = (screen(&h, Vec2::new(40.0, 10.0)), screen(&h, Vec2::new(40.0, 40.0)));
    // A slow frame: both clicks arrive before the next one is drawn, and eframe hands them over
    // together. (`Harness::event` would run a frame per event, so they go into the input directly.)
    for pos in [a, b] {
        for pressed in [true, false] {
            h.input_mut().events.push(Event::PointerMoved(pos));
            h.input_mut().events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        }
    }
    h.run_steps(2);
    let got = lines(&h);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got[0].1.near(Vec2::new(40.0, 10.0), 1e-4), "{got:?}");
    assert!(got[1].1.near(Vec2::new(40.0, 40.0), 1e-4), "{got:?}");
}

#[test]
fn a_click_made_while_the_mouse_moves_still_places_the_point() {
    let mut h = harness();
    start_line_at(&mut h, Vec2::new(10.0, 10.0));
    let down = screen(&h, Vec2::new(40.0, 10.0));
    button(&mut h, down, true);
    h.run_steps(1);
    // The hand is already on its way to the next point when the button comes up.
    let up = down + vec2(12.0, 0.0);
    button(&mut h, up, false);
    h.run_steps(2);
    let got = lines(&h);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].1.near(h.state().canvas.xf.unwrap().to_world(up), 1e-4), "{got:?}");
}

/// A model-space line from (20,20) to (60,20) to click on.
fn with_line(h: &mut Harness<'_, CadApp>) {
    h.state_mut().session.execute("line", &json!({"points": [[20, 20], [60, 20]]})).unwrap();
    h.run_steps(2);
}

/// Press on `at`, let the hand travel `dx` screen pixels, release there (#1001).
fn press_move_release(h: &mut Harness<'_, CadApp>, at: Pos2, dx: f32) {
    button(h, at, true);
    h.run_steps(1);
    button(h, at + vec2(dx, 0.0), false);
    h.run_steps(2);
}

#[test]
fn a_pick_made_while_the_mouse_moves_selects_the_object() {
    let mut h = harness();
    with_line(&mut h);
    let on_line = screen(&h, Vec2::new(40.0, 20.0));
    press_move_release(&mut h, on_line, 9.0);
    assert_eq!(h.state().session.selection().len(), 1, "no command running");
    assert!(h.state().session.pending_window.is_none());
}

#[test]
fn an_object_pick_made_while_the_mouse_moves_counts_in_a_selection_prompt() {
    let mut h = harness();
    with_line(&mut h);
    h.state_mut().start("erase");
    h.run_steps(2);
    let on_line = screen(&h, Vec2::new(40.0, 20.0));
    press_move_release(&mut h, on_line, 9.0);
    h.state_mut().session.input(cadcraft_engine::prompt::Input::Enter).unwrap();
    h.run_steps(2);
    assert!(lines(&h).is_empty(), "the line was picked and erased");
}

#[test]
fn a_real_drag_still_selects_with_a_window() {
    let mut h = harness();
    with_line(&mut h);
    // A window from lower left to upper right, 400 px wide, takes in the whole line.
    let from = screen(&h, Vec2::new(10.0, 10.0));
    let to = screen(&h, Vec2::new(70.0, 30.0));
    button(&mut h, from, true);
    h.run_steps(1);
    h.event(Event::PointerMoved(from + (to - from) * 0.5));
    h.run_steps(1);
    button(&mut h, to, false);
    h.run_steps(2);
    assert_eq!(h.state().session.selection().len(), 1);
}

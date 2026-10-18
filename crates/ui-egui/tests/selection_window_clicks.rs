//! Click-click selection windows tolerate a shaky hand: a press that wanders a few pixels is still
//! the first click (at the point where the button went down), and the second click closes the
//! window where the button comes up, however far the pointer moved while it was down.

use cadcraft_engine::Session;
use cadcraft_engine::doc::{Common, Drawing, EntityKind, Line, Space};
use cadcraft_engine::geom::Vec2;
use cadcraft_ui_egui::{CadApp, Services};
use egui::{Event, Modifiers, PointerButton, Pos2, Vec2 as V, vec2};
use egui_kittest::Harness;
use serde_json::json;

fn harness() -> Harness<'static, CadApp> {
    let mut d = Drawing::new_imperial();
    d.add(&Space::Model, Common::default(), EntityKind::Line(Line { a: Vec2::new(60.0, 40.0).to3(0.0), b: Vec2::new(62.0, 42.0).to3(0.0) })).unwrap();
    let mut session = Session::empty();
    session.open_drawing(d, "Selection window clicks", None);
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

fn world(h: &Harness<'_, CadApp>, p: Pos2) -> Vec2 {
    h.state().canvas.xf.unwrap().to_world(p)
}

fn button(h: &mut Harness<'_, CadApp>, pos: Pos2, pressed: bool) {
    h.event(Event::PointerMoved(pos));
    h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
    h.run_steps(1);
}

/// Press at `at`, wander through `path` (offsets from `at`), release at the last offset.
fn click(h: &mut Harness<'_, CadApp>, at: Pos2, path: &[V]) {
    button(h, at, true);
    for d in path {
        h.event(Event::PointerMoved(at + *d));
        h.run_steps(1);
    }
    button(h, at + path.last().copied().unwrap_or(V::ZERO), false);
    h.run_steps(1);
}

fn corner(h: &Harness<'_, CadApp>) -> Option<Vec2> {
    h.state().session.pending_window.as_ref().map(|w| w.corner)
}

#[test]
fn a_press_that_wanders_a_few_pixels_is_the_first_click_where_it_went_down() {
    let mut h = harness();
    let p = screen(&h, Vec2::new(55.0, 35.0));
    click(&mut h, p, &[vec2(1.0, 0.0), vec2(2.0, 1.0), vec2(3.0, 2.0)]);
    let c = corner(&h).expect("the first click opens a window");
    assert!(c.near(world(&h, p), 1e-6), "{c:?}");
    assert!(h.state().session.selection().is_empty());
}

#[test]
fn a_press_held_still_for_a_while_is_still_a_click() {
    let mut h = harness();
    let p = screen(&h, Vec2::new(55.0, 35.0));
    button(&mut h, p, true);
    h.run_steps(90);
    button(&mut h, p, false);
    h.run_steps(1);
    let c = corner(&h).expect("the first click opens a window");
    assert!(c.near(world(&h, p), 1e-6), "{c:?}");
}

#[test]
fn the_second_click_closes_the_window_where_the_button_comes_up_however_it_moved() {
    let mut h = harness();
    let p = screen(&h, Vec2::new(55.0, 35.0));
    click(&mut h, p, &[]);
    assert!(corner(&h).is_some());
    let q = screen(&h, Vec2::new(64.0, 44.0));
    click(&mut h, q, &[vec2(5.0, -3.0), vec2(20.0, -10.0)]);
    assert!(corner(&h).is_none(), "the second click closes the window");
    assert_eq!(h.state().session.selection().len(), 1);
}

#[test]
fn a_real_drag_still_selects_with_a_window_from_press_to_release() {
    let mut h = harness();
    let p = screen(&h, Vec2::new(55.0, 35.0));
    let q = screen(&h, Vec2::new(66.0, 46.0));
    click(&mut h, p, &[(q - p) * 0.25, (q - p) * 0.75, q - p]);
    assert!(corner(&h).is_none(), "a drag opens and closes its window");
    assert_eq!(h.state().session.selection().len(), 1);
}

fn line_ends(h: &Harness<'_, CadApp>) -> Vec<(Vec2, Vec2)> {
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
fn a_point_click_that_wanders_a_few_pixels_places_the_point_where_it_went_down() {
    let mut h = harness();
    h.state_mut().start("line");
    h.run_steps(2);
    let a = screen(&h, Vec2::new(10.0, 10.0));
    click(&mut h, a, &[vec2(2.0, 1.0), vec2(4.0, -2.0)]);
    let b = screen(&h, Vec2::new(30.0, 20.0));
    // Held still long enough that egui would call it a drag.
    button(&mut h, b, true);
    h.run_steps(90);
    button(&mut h, b, false);
    h.run_steps(1);
    let got = line_ends(&h);
    assert_eq!(got.len(), 2, "{got:?}");
    let new = got[1];
    assert!(new.0.near(world(&h, a), 1e-6), "{new:?}");
    assert!(new.1.near(world(&h, b), 1e-6), "{new:?}");
}

#[test]
fn a_select_objects_prompt_uses_the_same_window_clicks() {
    let mut h = harness();
    h.state_mut().start("erase");
    h.run_steps(2);
    assert!(h.state().session.prompting());
    let p = screen(&h, Vec2::new(55.0, 35.0));
    click(&mut h, p, &[vec2(2.0, 2.0)]);
    let c = corner(&h).expect("the first click opens a window");
    assert!(c.near(world(&h, p), 1e-6), "{c:?}");
    let q = screen(&h, Vec2::new(64.0, 44.0));
    click(&mut h, q, &[vec2(15.0, -15.0)]);
    assert!(corner(&h).is_none(), "the second click closes the window");
}

#[test]
fn a_grip_dragged_is_picked_up_where_it_went_down_and_put_down_where_it_comes_up() {
    let mut h = harness();
    let mid = screen(&h, Vec2::new(61.0, 41.0));
    click(&mut h, mid, &[]);
    assert_eq!(h.state().session.selection().len(), 1);
    let end = screen(&h, Vec2::new(62.0, 42.0));
    let to = screen(&h, Vec2::new(70.0, 30.0));
    click(&mut h, end, &[(to - end) * 0.5, to - end]);
    let got = line_ends(&h);
    assert!(got[0].1.near(world(&h, to), 1e-6), "{got:?}");
}

//! The drawn cursor tells what a click does (#1241): with no command running the crosshair carries
//! the pick box, a prompt asking for objects shows the pick box alone, a prompt asking for a point
//! the crosshair alone.

use cadcraft_engine::Session;
use cadcraft_ui_egui::theme::Tokens;
use cadcraft_ui_egui::{CadApp, Services};
use egui::epaint::RectShape;
use egui::{Pos2, Shape, pos2};

const AT: Pos2 = pos2(640.0, 400.0);

/// What the cursor at `AT` is drawn with: (crosshair arms, pick box).
fn cursor(app: &mut CadApp) -> (bool, bool) {
    let ctx = egui::Context::default();
    let mut shapes = Vec::new();
    // A few frames: layout settles and the pointer hover is known from the second frame on.
    for _ in 0..3 {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            events: vec![egui::Event::PointerMoved(AT)],
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            app.logic(ui.ctx());
            app.ui(ui);
        });
        shapes = out.shapes.into_iter().map(|c| c.shape).collect();
        out.textures_delta.clear(); // no renderer here
    }
    let color = Tokens::get().crosshair;
    let (mut arms, mut boxed) = (false, false);
    let mut stack = shapes;
    while let Some(s) = stack.pop() {
        match s {
            Shape::Vec(v) => stack.extend(v),
            // An arm runs along the pointer's row or column and ends at it or at the box's edge.
            Shape::LineSegment { points: [a, b], stroke } if stroke.color == color => {
                let horizontal = a.y == AT.y && b.y == AT.y && a.x.min(b.x) - 1.0 < AT.x + 20.0 && a.x.max(b.x) + 1.0 > AT.x - 20.0;
                let vertical = a.x == AT.x && b.x == AT.x && a.y.min(b.y) - 1.0 < AT.y + 20.0 && a.y.max(b.y) + 1.0 > AT.y - 20.0;
                arms |= horizontal || vertical;
            }
            Shape::Rect(RectShape { rect, stroke, .. }) if stroke.color == color && rect.center() == AT => boxed = true,
            _ => {}
        }
    }
    (arms, boxed)
}

fn app() -> CadApp {
    let mut app = CadApp::new(Session::new(), Services::default());
    app.ui.system_cursor = false;
    app
}

#[test]
fn no_command_shows_the_crosshair_with_the_pick_box() {
    let mut app = app();
    assert!(app.session.settings.pickfirst);
    assert_eq!(cursor(&mut app), (true, true));
}

#[test]
fn a_prompt_for_objects_shows_only_the_pick_box() {
    let mut app = app();
    app.start("erase");
    assert!(app.session.current_prompt().is_some_and(|p| p.accept.select), "ERASE asks for objects");
    assert_eq!(cursor(&mut app), (false, true), "Select objects: the pick box alone, no crosshair");

    // The pick box is the cursor there, so it stays when the rollover highlight is off.
    app.session.settings.selectionpreview = 0;
    assert_eq!(cursor(&mut app), (false, true), "SELECTIONPREVIEW 0 keeps the pick box");
}

#[test]
fn a_prompt_for_one_object_shows_only_the_pick_box() {
    let mut app = app();
    app.start("fillet");
    assert!(app.session.current_prompt().is_some_and(|p| p.picks_objects()), "FILLET asks for an object");
    assert_eq!(cursor(&mut app), (false, true));
}

#[test]
fn a_prompt_for_a_point_shows_only_the_crosshair() {
    let mut app = app();
    app.start("line");
    assert!(app.session.current_prompt().is_some_and(|p| p.accept.point && !p.picks_objects()), "LINE asks for a point");
    assert_eq!(cursor(&mut app), (true, false));
}

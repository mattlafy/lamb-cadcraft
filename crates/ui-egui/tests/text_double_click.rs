//! Double-clicking text with no command running edits it, as in AutoCAD (#842).

use cadcraft_engine::doc::EntityKind;
use cadcraft_engine::geom::Vec2;
use cadcraft_engine::{Input, Session};
use cadcraft_ui_egui::{CadApp, Services};
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use serde_json::json;

fn harness() -> Harness<'static, CadApp> {
    let mut app = CadApp::new(Session::new(), Services::default());
    app.run("text", json!({ "at": [0, 0], "text": "HELLO", "height": 2.5 })).unwrap();
    app.run("mtext", json!({ "at": [0, 20], "text": "first\\Psecond", "height": 2.5, "width": 20 })).unwrap();
    app.run("line", json!({ "points": [[30, 0], [30, 20]] })).unwrap();
    app.session.execute("view.set", &json!({ "center": [15, 10], "height": 40 })).unwrap();
    app.canvas.zoom_pending = false;
    // 60 frames a second: the default step (0.25 s) spreads two clicks past the double-click delay.
    let mut h = Harness::builder().with_size(vec2(1600.0, 1000.0)).with_step_dt(1.0 / 60.0).build_ui_state(
        |ui, app: &mut CadApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(5);
    h
}

fn double_click(h: &mut Harness<'_, CadApp>, at: Vec2) {
    let pos: Pos2 = h.state().canvas.xf.unwrap().to_screen(at);
    h.event(Event::PointerMoved(pos));
    h.run_steps(2);
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
            h.run_steps(1);
        }
    }
    h.run_steps(2);
}

fn text_at(h: &Harness<'_, CadApp>, i: usize) -> String {
    let d = h.state().session.doc().unwrap();
    let e = d.model.iter().nth(i).unwrap();
    match &e.kind {
        EntityKind::Text(t) => t.value.clone(),
        EntityKind::MText(t) => t.contents.clone(),
        k => panic!("not text: {}", k.type_name()),
    }
}

/// A point on a letter of text `i` (text is picked on its letters, not between them): the middle
/// of the first line of its first stroke.
fn on_letter(h: &Harness<'_, CadApp>, i: usize) -> Vec2 {
    let d = h.state().session.doc().unwrap();
    let strokes = match &d.model.iter().nth(i).unwrap().kind {
        EntityKind::Text(t) => cadcraft_engine::render::place_text_entity(d, t, &t.value).0.strokes,
        EntityKind::MText(t) => cadcraft_engine::render::layout_mtext_entity(d, t).strokes,
        k => panic!("not text: {}", k.type_name()),
    };
    let l = strokes.iter().find(|l| l.len() >= 2).unwrap();
    (l[0] + l[1]) * 0.5
}

#[test]
fn double_click_on_text_starts_textedit_for_that_object() {
    for (i, old, new) in [(0, "HELLO", "HELLO WORLD"), (1, "first\nsecond", "edited")] {
        let mut h = harness();
        let at = on_letter(&h, i);
        double_click(&mut h, at);
        let s = &h.state().session;
        assert_eq!(s.running.as_ref().map(|r| r.id.as_str()), Some("textedit"), "object {i}");
        let p = s.current_prompt().unwrap();
        assert_eq!(p.message, "Enter new text", "object {i}: the clicked text is already picked");
        assert_eq!(p.default.as_deref(), Some(old), "object {i}");
        h.state_mut().session.input(Input::Text(new.into())).unwrap();
        h.run_steps(2);
        assert_eq!(text_at(&h, i), new);
        // TEXTEDIT goes on asking for objects (its Multiple mode), as AutoCAD's does.
        assert_eq!(h.state().session.current_prompt().unwrap().message, "Select an annotation object");
    }
}

#[test]
fn double_click_elsewhere_starts_no_command() {
    for at in [Vec2::new(30.0, 5.0), Vec2::new(15.0, 30.0)] {
        let mut h = harness();
        double_click(&mut h, at);
        assert_eq!(h.state().session.running.as_ref().map(|r| r.id.as_str()), None, "{at:?}");
    }
}

//! Selection windows ignore ortho: STRETCH's own crossing window and the typed `W`/`C`/`BOX`
//! modes every command shares take their opposite corner where it is clicked.

use cadcraft_engine::snap::tracking::CursorQuery;
use cadcraft_engine::{Input, Session};
use cadcraft_geom::Vec2;
use serde_json::json;

/// Click at `raw` at the current prompt, snapped as the canvas snaps it.
fn click(s: &mut Session, raw: Vec2) {
    let p = s.current_prompt().unwrap();
    let q = CursorQuery { base: p.base, deferred: p.deferred, corner: p.corner, aperture: 1e-6, tol: 1e-6, now: 0.0, shift: false };
    let c = s.snap_cursor(raw, &q).point;
    s.cursor = c;
    s.input(Input::Point(c)).unwrap();
}

/// Ortho on, a diagonal line 0,0–10,10 and a far line; `cmd` is started and handed the typed
/// inputs `pre`, then the window -1,-1 to 11,11 is clicked, and the selection is moved by 5,0.
/// Returns the drawing's extents.
fn window_then_move(cmd: &str, pre: &[&str]) -> (Vec2, Vec2) {
    let mut s = Session::new();
    s.execute("setvar", &json!({ "name": "OSMODE", "value": 0 })).unwrap();
    s.execute("setvar", &json!({ "name": "ORTHOMODE", "value": 1 })).unwrap();
    s.execute("line", &json!({ "points": [[0, 0], [10, 10]] })).unwrap();
    s.execute("line", &json!({ "points": [[100, 100], [110, 100]] })).unwrap();
    s.cmdline(cmd).unwrap();
    for t in pre {
        s.cmdline(t).unwrap();
    }
    click(&mut s, Vec2::new(-1.0, -1.0));
    assert_eq!(s.current_prompt().unwrap().message, "Specify opposite corner");
    click(&mut s, Vec2::new(11.0, 11.0));
    s.cmdline("").unwrap();
    assert_eq!(s.current_prompt().unwrap().message.split(" or").next(), Some("Specify base point"), "{cmd}: something was selected");
    s.cmdline("0,0").unwrap();
    s.cmdline("5,0").unwrap();
    assert!(s.current_prompt().is_none(), "{cmd} is done, still at {:?}", s.current_prompt().map(|p| p.message));
    let b = s.doc().unwrap().extents(&s.space());
    (b.min, b.max)
}

#[test]
fn stretch_window_ignores_ortho() {
    assert_eq!(window_then_move("stretch", &[]), (Vec2::new(5.0, 0.0), Vec2::new(110.0, 100.0)), "the whole diagonal line moved");
}

#[test]
fn typed_window_ignores_ortho() {
    assert_eq!(window_then_move("move", &["w"]), (Vec2::new(5.0, 0.0), Vec2::new(110.0, 100.0)), "the diagonal line moved");
}

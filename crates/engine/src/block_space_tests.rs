//! `Space::Block`: commands edit inside a block definition (LAMB fork patch 1).

use std::sync::Arc;

use cadcraft_doc::{Drawing, Entity, EntityKind, Handle, Space};
use serde_json::json;

use crate::*;

fn block_handles(d: &Drawing, name: &str) -> Vec<Handle> {
    d.block(name).map(|b| b.entities.iter().map(|e| e.handle).collect()).unwrap_or_default()
}

fn model_kinds(s: &Session) -> Vec<&'static str> {
    s.doc().unwrap().model.iter().map(|e| e.kind.type_name()).collect()
}

/// A session with block `V` holding one line, an INSERT of `V` in model space, and the original
/// block member's handle.
fn session_with_block() -> (Session, Handle) {
    let mut s = Session::new();
    s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    let l = s.doc().unwrap().model.iter().next().unwrap().handle;
    s.execute("block", &json!({ "name": "V", "base": [0, 0], "handles": [l.hex()], "keep": "convert" })).unwrap();
    assert_eq!(model_kinds(&s), vec!["Block Reference"]);
    let member = block_handles(s.doc().unwrap(), "V");
    assert_eq!(member.len(), 1);
    (s, member[0])
}

fn enter(s: &mut Session, name: &str) {
    let st = s.state_mut().unwrap();
    st.space = Space::Block(name.into());
    st.mspace = None;
}

fn line_end(e: &Entity) -> [f64; 4] {
    match &e.kind {
        EntityKind::Line(l) => [l.a.x, l.a.y, l.b.x, l.b.y],
        k => panic!("not a line: {}", k.type_name()),
    }
}

#[test]
fn line_in_block_space_goes_into_the_block() {
    let (mut s, member) = session_with_block();
    let before = s.doc().unwrap().clone();
    enter(&mut s, "V");
    s.execute("line", &json!({"points": [[0, 5], [10, 5]]})).unwrap();
    let d = s.doc().unwrap();
    let hs = block_handles(d, "V");
    assert_eq!(hs.len(), 2);
    assert_eq!(hs[0], member);
    assert_eq!(model_kinds(&s), vec!["Block Reference"], "nothing added to model space");
    assert_eq!(d.space_of(hs[1]), Some(Space::Block("V".into())));
    assert_eq!(d.space(&Space::Block("V".into())).map(|st| st.len()), Some(2));
    s.undo().unwrap();
    assert_eq!(**s.doc().unwrap().block("V").unwrap(), **before.block("V").unwrap(), "undo restores V exactly");
    assert_eq!(s.space(), Space::Block("V".into()));
}

#[test]
fn erase_and_move_the_original_block_member() {
    let (mut s, member) = session_with_block();
    let before = s.doc().unwrap().clone();
    enter(&mut s, "V");
    s.execute("move", &json!({ "handles": [member.hex()], "delta": [3, 4] })).unwrap();
    assert_eq!(line_end(s.doc().unwrap().entity(member).unwrap()), [3.0, 4.0, 13.0, 4.0]);
    s.execute("erase", &json!({ "handles": [member.hex()] })).unwrap();
    assert!(block_handles(s.doc().unwrap(), "V").is_empty());
    assert!(s.doc().unwrap().entity(member).is_none());
    s.undo().unwrap();
    s.undo().unwrap();
    assert_eq!(**s.doc().unwrap().block("V").unwrap(), **before.block("V").unwrap(), "undo restores V exactly");
    assert_eq!(line_end(s.doc().unwrap().entity(member).unwrap()), [0.0, 0.0, 10.0, 0.0]);
}

#[test]
fn model_and_paper_lookups_are_unchanged() {
    let (s, member) = session_with_block();
    let d = s.doc().unwrap();
    let ins = d.model.iter().next().unwrap().handle;
    assert_eq!(d.space_of(ins), Some(Space::Model));
    assert!(Arc::ptr_eq(d.entity(ins).unwrap(), d.model.get(ins).unwrap()));
    assert_eq!(d.space(&Space::Model).map(|st| st.len()), Some(1));
    let layout = d.layouts.first().map(|l| l.name.clone()).unwrap();
    assert_eq!(d.space(&Space::Paper(layout.clone())).map(|st| st.len()), d.layout(&layout).map(|l| l.entities.len()));
    // Block members are found only after model and layouts, and are not model or paper entities.
    assert_eq!(d.space_of(member), Some(Space::Block("V".into())));
    assert!(d.model.get(member).is_none());
    assert!(d.space(&Space::Block("nope".into())).is_none());
    assert_eq!(d.space(&Space::Block("v".into())).map(|st| st.len()), Some(1), "block lookup is case-insensitive like Drawing::block");
}

#[test]
fn undoing_the_block_while_inside_it_returns_to_model() {
    let (mut s, _) = session_with_block();
    enter(&mut s, "V");
    s.undo().unwrap();
    assert!(s.doc().unwrap().block("V").is_none());
    assert_eq!(s.space(), Space::Model);
    assert_eq!(s.state().unwrap().mspace, None);
}

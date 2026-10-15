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

fn hatch_in(s: &Session, h: Handle) -> cadcraft_doc::Hatch {
    match &s.doc().unwrap().entity(h).unwrap().kind {
        EntityKind::Hatch(x) => x.clone(),
        k => panic!("not a hatch: {}", k.type_name()),
    }
}

fn hatch_area(h: &cadcraft_doc::Hatch) -> f64 {
    h.loops
        .iter()
        .map(|l| {
            let pts = cadcraft_geom::Polyline { vertices: l.vertices.clone(), closed: true }.tessellate(1e-4);
            cadcraft_geom::shoelace(&pts).abs() * if l.outer { 1.0 } else { -1.0 }
        })
        .sum()
}

#[test]
fn associative_hatch_in_block_space_follows_a_stretch() {
    let (mut s, _) = session_with_block();
    enter(&mut s, "V");
    s.execute("rectang", &json!({ "p1": [0, 0], "p2": [10, 10] })).unwrap();
    let r = s.execute("hatch", &json!({ "points": [[5, 5]] })).unwrap();
    let h = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
    assert_eq!(s.doc().unwrap().space_of(h), Some(Space::Block("V".into())));
    s.script("STRETCH\nC\n12,-1\n8,11\n\n0,0\n5,0\n").unwrap();
    assert!(s.running.is_none(), "{}", s.prompt_text());
    assert_eq!(s.doc().unwrap().space_of(h), Some(Space::Block("V".into())), "the hatch stays in V");
    let hh = hatch_in(&s, h);
    assert!(hh.associative);
    assert!((hatch_area(&hh) - 150.0).abs() < 1e-6, "area {}", hatch_area(&hh));
}

#[test]
fn associative_dimension_in_block_space_follows_a_move() {
    let (mut s, member) = session_with_block();
    enter(&mut s, "V");
    let r = s.execute("dimlinear", &json!({ "p1": [0, 0], "p2": [10, 0], "at": [0, 5] })).unwrap();
    let dh = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
    let dim = |s: &Session| match &s.doc().unwrap().entity(dh).unwrap().kind {
        EntityKind::Dimension(d) => d.clone(),
        k => panic!("not a dimension: {}", k.type_name()),
    };
    assert!(!dim(&s).assoc.is_empty(), "the dimension is associated with the block member");
    s.execute("move", &json!({ "handles": [member.hex()], "delta": [3, 4] })).unwrap();
    let d = dim(&s);
    assert!((d.p13.x - 3.0).abs() < 1e-9 && (d.p13.y - 4.0).abs() < 1e-9, "{:?}", d.p13);
    assert!((d.p14.x - 13.0).abs() < 1e-9 && (d.p14.y - 4.0).abs() < 1e-9, "{:?}", d.p14);
}

#[test]
fn associativity_skips_xref_and_anonymous_blocks() {
    let (mut s, _) = session_with_block();
    s.execute("line", &json!({"points": [[0, 20], [10, 20]]})).unwrap();
    let st = s.state_mut().unwrap();
    let d = Arc::make_mut(&mut st.doc);
    for (name, xref) in [("X", true), ("*U1", false)] {
        let mut b = cadcraft_doc::Block::new(name);
        if xref {
            b.xref_path = Some("other.dwg".into());
        }
        d.blocks.insert(name.into(), Arc::new(b));
    }
    let d: &Drawing = s.doc().unwrap();
    let n = crate::assoc::assoc_stores(d).count();
    // Model, the layouts, then block V; X (an xref) and *U1 (anonymous) are skipped.
    assert_eq!(n, 2 + d.layouts.len());
}

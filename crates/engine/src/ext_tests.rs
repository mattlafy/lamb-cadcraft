//! Extension slots (`Common::ext`, `Layer::ext`, `Drawing::ext`, LAMB fork patch 2) travel through
//! every entity copy and through undo/redo.

use std::collections::BTreeSet;

use cadcraft_doc::{Extension, Handle};
use serde_json::{Value, json};

use crate::*;

fn lamb_ext() -> Extension {
    let mut x = Extension::new();
    x.insert("LAMB".into(), json!({"role": "cut"}));
    x
}

fn handles(s: &Session) -> BTreeSet<Handle> {
    s.doc().unwrap().model.iter().map(|e| e.handle).collect()
}

fn hex(hs: &[Handle]) -> Value {
    json!(hs.iter().map(|h| h.hex()).collect::<Vec<_>>())
}

/// A session with one line and one circle, both carrying `ext["LAMB"]`.
fn setup() -> (Session, Vec<Handle>) {
    let mut s = Session::new();
    s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    s.execute("circle", &json!({"center": [20, 5], "radius": 2})).unwrap();
    let hs: Vec<Handle> = handles(&s).into_iter().collect();
    assert_eq!(hs.len(), 2);
    for &h in &hs {
        s.doc_mut().unwrap().modify_entity(h, |e| e.common.ext = lamb_ext()).unwrap();
    }
    (s, hs)
}

/// Run `id` with `params`; every entity it created carries the original `ext`.
fn created_carry_ext(s: &mut Session, id: &str, params: Value) -> Vec<Handle> {
    let before = handles(s);
    s.execute(id, &params).unwrap_or_else(|e| panic!("{id}: {e}"));
    let new: Vec<Handle> = handles(s).difference(&before).copied().collect();
    assert!(!new.is_empty(), "{id} created nothing");
    let d = s.doc().unwrap();
    for h in &new {
        assert_eq!(d.entity(*h).unwrap().common.ext, lamb_ext(), "{id} dropped ext on {h:?}");
    }
    new
}

#[test]
fn copy_keeps_ext() {
    let (mut s, hs) = setup();
    let n = created_carry_ext(&mut s, "copy", json!({"handles": hex(&hs), "delta": [0, 50]}));
    assert_eq!(n.len(), 2);
}

#[test]
fn mirror_keeps_ext() {
    let (mut s, hs) = setup();
    let n = created_carry_ext(&mut s, "mirror", json!({"handles": hex(&hs), "p1": [0, -10], "p2": [30, -10], "erase": false}));
    assert_eq!(n.len(), 2);
}

#[test]
fn arrayrect_keeps_ext() {
    let (mut s, hs) = setup();
    created_carry_ext(&mut s, "arrayrect", json!({"handles": hex(&hs), "rows": 2, "cols": 3, "rowSpacing": 20, "colSpacing": 40}));
}

#[test]
fn offset_keeps_ext() {
    let (mut s, hs) = setup();
    for h in hs {
        let n = created_carry_ext(&mut s, "offset", json!({"handle": h.hex(), "distance": 1, "side": [5, 50]}));
        assert_eq!(n.len(), 1);
    }
}

#[test]
fn rotate_copy_keeps_ext() {
    let (mut s, hs) = setup();
    let n = created_carry_ext(&mut s, "rotate", json!({"handles": hex(&hs), "base": [0, 0], "angle": 90, "copy": true}));
    assert_eq!(n.len(), 2);
}

#[test]
fn copyclip_pasteclip_keeps_ext() {
    let (mut s, hs) = setup();
    s.execute("copyclip", &json!({"handles": hex(&hs)})).unwrap();
    let n = created_carry_ext(&mut s, "pasteclip", json!({"at": [100, 100]}));
    assert_eq!(n.len(), 2);
}

#[test]
fn undo_redo_of_copy_keeps_ext() {
    let (mut s, hs) = setup();
    let copies = created_carry_ext(&mut s, "copy", json!({"handles": hex(&hs), "delta": [0, 50]}));
    s.undo().unwrap();
    let d = s.doc().unwrap();
    for h in &copies {
        assert!(d.entity(*h).is_none(), "undo left copy {h:?}");
    }
    for h in &hs {
        assert_eq!(d.entity(*h).unwrap().common.ext, lamb_ext());
    }
    s.redo().unwrap();
    let d = s.doc().unwrap();
    for h in copies.iter().chain(&hs) {
        assert_eq!(d.entity(*h).unwrap().common.ext, lamb_ext(), "redo lost ext on {h:?}");
    }
}

#[test]
fn drawing_and_layer_ext_survive_undo() {
    let mut s = Session::new();
    {
        let d = s.doc_mut().unwrap();
        d.ext.insert("LAMB".into(), json!({"views": []}));
        d.layers.iter_mut().find(|l| l.name == "0").unwrap().ext.insert("LAMB".into(), json!({"role": "cut"}));
    }
    s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    s.undo().unwrap();
    let d = s.doc().unwrap();
    assert!(d.model.iter().next().is_none());
    assert_eq!(d.ext.get("LAMB"), Some(&json!({"views": []})));
    assert_eq!(d.layer("0").unwrap().ext.get("LAMB"), Some(&json!({"role": "cut"})));
}

#[test]
fn empty_ext_is_not_serialized() {
    let c = cadcraft_doc::Common::default();
    assert!(!serde_json::to_string(&c).unwrap().contains("\"ext\""));
    let l = cadcraft_doc::Layer::default();
    assert!(!serde_json::to_string(&l).unwrap().contains("\"ext\""));
}

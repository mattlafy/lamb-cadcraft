//! Extension slots (`Common::ext`, `Layer::ext`, `Drawing::ext`) through DXF: entity and layer
//! slots as xdata under one registered application per key, drawing slots as
//! `CADCRAFT_EXT_<key>` XRECORDs in the named object dictionary.

use cadcraft_doc::*;
use cadcraft_geom::Vec3;
use serde_json::{Value, json};

use crate::*;

fn roundtrip(d: &Drawing) -> Drawing {
    read_dxf(write_dxf(d).as_bytes()).unwrap()
}

fn ext(pairs: &[(&str, Value)]) -> Extension {
    pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
}

fn with_ext(e: &Extension) -> Common {
    Common { ext: e.clone(), ..Common::default() }
}

fn line() -> EntityKind {
    EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(10.0, 5.0, 0.0) })
}

fn text(value: &str) -> Text {
    Text {
        insert: Vec3::ZERO,
        align_pt: None,
        height: 2.5,
        value: value.into(),
        rotation: 0.0,
        width_factor: 1.0,
        oblique: 0.0,
        style: "Standard".into(),
        halign: HAlign::Left,
        valign: VAlign::Baseline,
    }
}

fn lamb(role: &str) -> Extension {
    ext(&[("LAMB", json!({"role": role, "provenance": {"view": "plan-1", "seq": [1, 2, 3]}}))])
}

#[test]
fn line_ext_roundtrips() {
    let mut d = Drawing::new_metric();
    let e = lamb("wall-face");
    d.add(&Space::Model, with_ext(&e), line()).unwrap();
    let text = write_dxf(&d);
    assert!(text.contains("AcDbRegAppTableRecord\r\n  2\r\nLAMB\r\n"), "LAMB is a registered application");
    let back = read_dxf(text.as_bytes()).unwrap();
    assert_eq!(back.model.iter().next().unwrap().common.ext, e);
}

#[test]
fn two_keys_on_one_entity() {
    let mut d = Drawing::new_metric();
    let e = ext(&[("LAMB", json!({"role": "cut"})), ("OTHER_TOOL", json!([1, "two", null, 3.5]))]);
    d.add(&Space::Model, with_ext(&e), line()).unwrap();
    assert_eq!(roundtrip(&d).model.iter().next().unwrap().common.ext, e);
}

#[test]
fn entities_with_child_records_keep_ext_and_children() {
    let mut d = Drawing::new_metric();
    let pts = vec![Vec3::ZERO, Vec3::new(10.0, 0.0, 1.0), Vec3::new(10.0, 10.0, 2.0)];
    let pe = lamb("section-edge");
    d.add(&Space::Model, with_ext(&pe), EntityKind::Polyline3d(Polyline3d { points: pts.clone(), closed: false })).unwrap();
    let mut b = Block::new("Tag");
    b.entities.push(Entity::new(
        Handle(0x700),
        EntityKind::AttDef(Attrib {
            tag: "NUM".into(),
            text: text("0"),
            invisible: false,
            constant: false,
            prompt: String::new(),
            props: Default::default(),
            multiline: None,
            modes: Default::default(),
        }),
    ));
    d.blocks.insert("Tag".into(), std::sync::Arc::new(b));
    d.bump_handseed(Handle(0x710));
    let ie = lamb("door-tag");
    let attrib = Attrib {
        tag: "NUM".into(),
        text: text("D1"),
        invisible: false,
        constant: false,
        prompt: String::new(),
        props: Default::default(),
        multiline: None,
        modes: Default::default(),
    };
    d.add(
        &Space::Model,
        with_ext(&ie),
        EntityKind::Insert(Insert {
            block: "Tag".into(),
            insert: Vec3::new(2.0, 3.0, 0.0),
            scale: Vec3::new(1.0, 1.0, 1.0),
            rotation: 0.0,
            attribs: vec![attrib],
            cols: 1,
            rows: 1,
            col_spacing: 0.0,
            row_spacing: 0.0,
        }),
    )
    .unwrap();
    let text = write_dxf(&d);
    let back = read_dxf(text.as_bytes()).unwrap();
    let mut seen = 0;
    for e in back.model.iter() {
        match &e.kind {
            EntityKind::Polyline3d(p) => {
                assert_eq!(p.points, pts, "vertices still read back");
                assert_eq!(e.common.ext, pe);
                seen += 1;
            }
            EntityKind::Insert(i) => {
                assert_eq!(i.attribs.len(), 1, "the ATTRIB still reads back");
                assert_eq!(i.attribs.first().unwrap().text.value, "D1");
                assert_eq!(e.common.ext, ie);
                seen += 1;
            }
            _ => {}
        }
    }
    assert_eq!(seen, 2);
    // Each record carries its slot once, written before its first child record.
    assert_eq!(text.matches("1001\r\nLAMB\r\n").count(), 2);
}

#[test]
fn block_member_ext_roundtrips() {
    let mut d = Drawing::new_metric();
    let e = lamb("block-member");
    let mut b = Block::new("Door");
    let mut m = Entity::new(Handle(0x600), line());
    m.common.ext = e.clone();
    b.entities.push(m);
    d.blocks.insert("Door".into(), std::sync::Arc::new(b));
    d.bump_handseed(Handle(0x610));
    let back = roundtrip(&d);
    assert_eq!(back.block("Door").unwrap().entities.iter().next().unwrap().common.ext, e);
}

#[test]
fn layer_ext_roundtrips() {
    let mut d = Drawing::new_metric();
    let e = lamb("layer-role");
    d.layers.push(Layer { name: "A-WALL".into(), transparency: 30, description: "Walls".into(), ext: e.clone(), ..Layer::default() });
    let back = roundtrip(&d);
    let l = back.layer("A-WALL").unwrap();
    assert_eq!(l.ext, e);
    assert_eq!((l.transparency, l.description.as_str()), (30, "Walls"), "the layer's other xdata still reads");
}

#[test]
fn drawing_ext_roundtrips_long_backslash_and_non_ascii() {
    let mut d = Drawing::new_metric();
    let long = format!("C:\\path\\to\\{}Ångström – ünïcode \\U+0041 end", "x".repeat(300));
    let e = ext(&[("LAMB", json!({"views": [{"name": "Plan ①", "note": long}]})), ("Z", json!(42))]);
    d.ext = e.clone();
    let text = write_dxf(&d);
    assert!(text.contains("CADCRAFT_EXT_LAMB"));
    assert_eq!(roundtrip(&d).ext, e);
}

#[test]
fn empty_ext_writes_nothing_extra() {
    let d = Drawing::new_metric();
    let text = write_dxf(&d);
    assert!(!text.contains("CADCRAFT_EXT_"));
    assert_eq!(text.matches("AcDbRegAppTableRecord").count(), 5, "ACAD, CADCRAFT, AcadAnnotative and the two layer apps");
    let mut with_line = Drawing::new_metric();
    with_line.add(&Space::Model, Common::default(), line()).unwrap();
    assert!(!write_dxf(&with_line).contains("1000\r\nEXT\r\n"));
}

#[test]
fn foreign_xdata_without_the_marker_is_not_read() {
    let mut d = Drawing::new_metric();
    d.add(&Space::Model, Common::default(), line()).unwrap();
    let text = write_dxf(&d);
    // Add another application's xdata (a JSON-looking string, no EXT marker) to the LINE.
    let at = text.find("AcDbLine").unwrap();
    let end = at + text[at..].find("\r\n  0\r\n").unwrap();
    let mut doc = text.clone();
    doc.insert_str(end, "\r\n1001\r\nOTHERAPP\r\n1000\r\n{\"a\":1}");
    let back = read_dxf(doc.as_bytes()).unwrap();
    assert!(back.model.iter().next().unwrap().common.ext.is_empty());
    // It is still kept and written back as other applications' data.
    assert!(write_dxf(&back).contains("OTHERAPP"));
}

#[test]
fn reserved_and_invalid_keys_are_skipped() {
    let mut d = Drawing::new_metric();
    let mut e = lamb("kept");
    for bad in ["", "CADCRAFT", "acad", "AcCmTransparency", "a/b", "semi;colon", "back`tick", " padded", &"k".repeat(256)] {
        e.insert(bad.to_string(), json!(1));
    }
    d.add(&Space::Model, with_ext(&e), line()).unwrap();
    d.layers.push(Layer { name: "L".into(), ext: e.clone(), ..Layer::default() });
    d.ext = e.clone();
    let back = roundtrip(&d);
    let want = lamb("kept");
    assert_eq!(back.model.iter().next().unwrap().common.ext, want);
    assert_eq!(back.layer("L").unwrap().ext, want);
    assert_eq!(back.ext, want);
    assert_eq!(back.model.len(), 1);
}

#[test]
fn a_slot_removed_after_reading_stays_removed() {
    let mut d = Drawing::new_metric();
    d.add(&Space::Model, with_ext(&lamb("gone")), line()).unwrap();
    let mut back = roundtrip(&d);
    let h = back.model.iter().next().unwrap().handle;
    back.modify_entity(h, |e| e.common.ext.clear()).unwrap();
    let again = roundtrip(&back);
    assert!(again.model.iter().next().unwrap().common.ext.is_empty());
    // And one that stays is written once, not again from the kept copy.
    let twice = write_dxf(&roundtrip(&d));
    assert_eq!(twice.matches("1001\r\nLAMB\r\n").count(), 1);
}

/// A layout's first viewport writes the implicit id-1 paper view ahead of itself; its ext must land
/// on its own VIEWPORT record, not on that paper view (which the reader drops).
#[test]
fn first_viewport_of_a_layout_keeps_its_ext() {
    let mut d = Drawing::new_metric();
    let paper = Space::Paper("Layout1".into());
    let vp = |id: u32| {
        EntityKind::Viewport(Viewport {
            center: Vec3::new(5.0, 4.0, 0.0),
            width: 8.0,
            height: 6.0,
            view_center: cadcraft_geom::Vec2::new(5.0, 2.5),
            view_height: 12.0,
            id,
            locked: false,
            frozen_layers: Vec::new(),
            layer_colors: Vec::new(),
            clip: None,
            on: true,
            twist: 0.0,
        })
    };
    for id in [2, 3] {
        d.add(&paper, with_ext(&ext(&[("LAMB", json!({"viewId": id}))])), vp(id)).unwrap();
    }
    let back = roundtrip(&d);
    let got: Vec<(u32, Option<Value>)> = back
        .layouts
        .iter()
        .flat_map(|l| l.entities.iter())
        .filter_map(|e| match &e.kind {
            EntityKind::Viewport(v) => Some((v.id, e.common.ext.get("LAMB").cloned())),
            _ => None,
        })
        .collect();
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got.contains(&(2, Some(json!({"viewId": 2})))), "{got:?}");
    assert!(got.contains(&(3, Some(json!({"viewId": 3})))), "{got:?}");
}

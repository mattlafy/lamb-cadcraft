//! Picking (and the hover highlight, which picks the same way) finds dimensions and multileaders
//! on the letters of their text, as plain text is found on its letters (not between them: as
//! AutoCAD does, observed with the same synthetic drawings).

use cadcraft_engine::Session;
use cadcraft_engine::doc::{EntityKind, Handle, Space};
use cadcraft_engine::geom::{Bounds2, Line, Vec2};
use cadcraft_engine::render;
use cadcraft_engine::select::pick;
use serde_json::json;

/// The last entity of `kind`, the strokes of its text (the built-in font draws with lines) and the
/// box (centre, half size along the text, angle) of its words.
fn text_of(s: &Session, kind: &str) -> (Handle, Vec<Vec<Vec2>>, Vec2, Vec2, f64) {
    let d = s.doc().unwrap();
    let e = d.model.iter().filter(|e| format!("{:?}", e.kind).starts_with(kind)).last().unwrap();
    let (strokes, angle): (Vec<Vec<Vec2>>, f64) = match &e.kind {
        EntityKind::Dimension(dm) => {
            let g = render::dimension_in(d, dm);
            assert!(g.text_glyphs.is_empty(), "the built-in font draws with lines");
            (g.text, g.text_angle)
        }
        EntityKind::MLeader(m) => {
            let t = m.text.as_ref().unwrap();
            (render::layout_mtext_entity(d, t).strokes, t.rotation)
        }
        _ => unreachable!(),
    };
    let b = Bounds2::from_points(strokes.iter().flatten().map(|q| q.rotate(-angle)));
    (e.handle, strokes, b.center().rotate(angle), Vec2::new(b.width() / 2.0, b.height() / 2.0), angle)
}

/// Points on the letters: the middle of a line of the first stroke and of the middle one (the end
/// of the words can pass the rough bounds that pick candidates are found by).
fn on_letters(strokes: &[Vec<Vec2>]) -> Vec<Vec2> {
    let lines: Vec<&Vec<Vec2>> = strokes.iter().filter(|l| l.len() >= 2).collect();
    [0, lines.len() / 2].iter().map(|&i| (lines[i][0] + lines[i][1]) * 0.5).collect()
}

/// A point inside the box of the words at least `gap` from every letter.
fn between_letters(strokes: &[Vec<Vec2>], c: Vec2, half: Vec2, angle: f64, gap: f64) -> Vec2 {
    let far = |p: Vec2| strokes.iter().flat_map(|l| l.windows(2)).all(|w| Line::new(w[0], w[1]).dist(p) > gap);
    (1..40)
        .flat_map(|i| (1..8).map(move |j| (i, j)))
        .map(|(i, j)| at(c, half, angle, f64::from(i) / 20.0 - 1.0, f64::from(j) / 4.0 - 1.0))
        .find(|p| far(*p))
        .unwrap()
}

/// A point `(u, v)` half-sizes from the text box's centre, along and across the text.
fn at(c: Vec2, half: Vec2, angle: f64, u: f64, v: f64) -> Vec2 {
    c + Vec2::new(half.x * u, half.y * v).rotate(angle)
}

#[test]
fn dimension_and_multileader_text_is_picked_on_its_letters() {
    let mut s = Session::new();
    s.execute("dimlinear", &json!({"p1": [0, 0], "p2": [100, 0], "at": [50, 30], "text": "TWO WORDS HERE"})).unwrap();
    let (h, strokes, c, half, angle) = text_of(&s, "Dimension");
    let tiny = half.y * 0.01;
    let d = s.doc().unwrap();
    // On the letters, at the start and middle of the words: the dimension.
    for p in on_letters(&strokes) {
        assert_eq!(pick(d, &Space::Model, p, tiny), Some(h), "{p:?}");
    }
    // Between the letters, and well above the words: nothing.
    let gap = between_letters(&strokes, c, half, angle, half.y * 0.2);
    assert_eq!(pick(d, &Space::Model, gap, tiny), None, "between the letters at {gap:?}");
    assert_eq!(pick(d, &Space::Model, at(c, half, angle, 0.0, 6.0), tiny), None, "well above the words: nothing");

    s.execute("mleader", &json!({"points": [[200, 0], [220, 20]], "text": "LEADER NOTE"})).unwrap();
    let (h, strokes, c, half, angle) = text_of(&s, "MLeader");
    let d = s.doc().unwrap();
    for p in on_letters(&strokes) {
        assert_eq!(pick(d, &Space::Model, p, half.y * 0.01), Some(h), "leader text {p:?}");
    }
    let gap = between_letters(&strokes, c, half, angle, half.y * 0.2);
    assert_eq!(pick(d, &Space::Model, gap, half.y * 0.01), None, "between the leader's letters at {gap:?}");
}

#[test]
fn slanted_dimension_text_is_picked_on_its_turned_letters_only() {
    let mut s = Session::new();
    // Text along the dimension line (not kept horizontal), as in most surveying styles.
    for name in ["DIMTIH", "DIMTOH"] {
        s.execute("setvar", &json!({"name": name, "value": 0})).unwrap();
    }
    s.execute("dimaligned", &json!({"p1": [0, 0], "p2": [100, 100], "at": [40, 60], "text": "SLANTED LABEL"})).unwrap();
    let (h, strokes, c, half, angle) = text_of(&s, "Dimension");
    assert!((angle - std::f64::consts::FRAC_PI_4).abs() < 1e-6, "aligned with the 45° line: {angle}");
    let d = s.doc().unwrap();
    let tiny = half.y * 0.01;
    for p in on_letters(&strokes) {
        assert_eq!(pick(d, &Space::Model, p, tiny), Some(h), "{p:?}");
    }
    // Inside the axis-aligned box around the turned words, but off them (and off the lines).
    let off = at(c, half, angle, 0.3, 4.0);
    let aabb = Bounds2::from_points([
        at(c, half, angle, -1.0, -1.0),
        at(c, half, angle, 1.0, -1.0),
        at(c, half, angle, 1.0, 1.0),
        at(c, half, angle, -1.0, 1.0),
    ]);
    assert!(aabb.contains(off), "the probe sits in the loose box");
    assert_ne!(pick(d, &Space::Model, off, tiny), Some(h), "but not on the words");
}

#[test]
fn a_dimension_drawn_from_its_block_is_picked_on_the_block_text() {
    // As AutoCAD saves dimensions: the geometry in an anonymous block, here with the text moved
    // well away from where the dimension's own geometry would set it.
    let mut s = Session::new();
    s.execute("dimlinear", &json!({"p1": [0, 0], "p2": [100, 0], "at": [50, 30]})).unwrap();
    s.execute("mtext", &json!({"at": [400, 400], "text": "MOVED TEXT", "height": 5})).unwrap();
    let d = s.doc_mut().unwrap();
    let text = d.model.iter().find(|e| matches!(e.kind, EntityKind::MText(_))).unwrap().as_ref().clone();
    let dim = d.model.iter().find(|e| matches!(e.kind, EntityKind::Dimension(_))).unwrap().handle;
    d.model.remove(text.handle);
    let mut entities = cadcraft_engine::doc::EntityStore::new();
    entities.push(text);
    let block = cadcraft_engine::doc::Block {
        xref_overlay: false,
        xref_status: Default::default(),
        name: "*D1".into(),
        annotative: false,
        base: Default::default(),
        entities,
        description: String::new(),
        anonymous: true,
        xref_path: None,
        explodable: true,
        units: 0,
        array: None,
    };
    d.blocks.insert("*D1".into(), std::sync::Arc::new(block));
    d.model.modify(dim, |e| {
        if let EntityKind::Dimension(dm) = &mut e.kind {
            dm.block = Some("*D1".into());
        }
    });
    let d = s.doc().unwrap();
    let m = render::layout_mtext_entity(
        d,
        match &d.block("*D1").unwrap().entities.iter().next().unwrap().kind {
            EntityKind::MText(t) => t,
            _ => unreachable!(),
        },
    );
    let line = m.strokes.iter().find(|l| l.len() >= 2).unwrap();
    let on = (line[0] + line[1]) * 0.5;
    assert_eq!(pick(d, &Space::Model, on, 0.01), Some(dim), "on the block's text");
    assert_eq!(pick(d, &Space::Model, Vec2::new(50.0, 33.0), 0.01), None, "not where the dimension would have set it");
}

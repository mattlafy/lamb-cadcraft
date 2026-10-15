//! Picking (and the hover highlight, which picks the same way), crossing and fence selection find
//! a block reference only on what it draws: its lines and curves, the edges of its fills and the
//! letters of its text. Text is found on its letters: not between them, nor across the whole width
//! of an MTEXT's column. The empty rest of a block's bounding rectangle picks nothing.
//!
//! As AutoCAD does, observed with the same synthetic drawings: a click or a crossing window takes
//! TEXT and MTEXT (on their own, in a block, turned, the text of a dimension) only on the strokes
//! of their letters; a window around the letters of an MTEXT takes it though it doesn't hold its
//! whole column; a SOLID or solid hatch inside a block is taken on its edges (a SOLID also on the
//! diagonal it is filled across), a solid hatch on its own anywhere inside it.

use std::sync::Arc;

use cadcraft_engine::Session;
use cadcraft_engine::doc::geom::Vec3;
use cadcraft_engine::doc::{Block, Circle, Common, Drawing, Entity, EntityKind, Handle, Insert, Line, Solid, Space};
use cadcraft_engine::geom::{Bounds2, PolyVertex, Vec2};
use cadcraft_engine::render;
use cadcraft_engine::select::{pick, select_fence, select_window};
use serde_json::{Value, json};

const AP: f64 = 0.25;

fn v3(x: f64, y: f64) -> Value {
    json!({ "x": x, "y": y, "z": 0.0 })
}

fn text(x: f64, y: f64, h: f64, value: &str) -> EntityKind {
    EntityKind::Text(serde_json::from_value(json!({ "insert": v3(x, y), "height": h, "value": value })).unwrap())
}

/// An MTEXT `width` wide, hanging from its top-left corner at `x`,`y`.
fn mtext(x: f64, y: f64, h: f64, width: f64, contents: &str) -> EntityKind {
    EntityKind::MText(serde_json::from_value(json!({ "insert": v3(x, y), "height": h, "width": width, "contents": contents })).unwrap())
}

fn line(ax: f64, ay: f64, bx: f64, by: f64) -> EntityKind {
    EntityKind::Line(Line { a: Vec3::new(ax, ay, 0.0), b: Vec3::new(bx, by, 0.0) })
}

fn circle(x: f64, y: f64, r: f64) -> EntityKind {
    EntityKind::Circle(Circle { center: Vec3::new(x, y, 0.0), radius: r })
}

/// A SOLID through `c` in DXF order (1, 2, 4, 3 when drawn; a repeated last corner = a triangle).
fn solid(c: [(f64, f64); 4]) -> EntityKind {
    EntityKind::Solid(Solid { corners: c.map(|(x, y)| Vec3::new(x, y, 0.0)) })
}

/// A solid hatch filling the polygon `pts`.
fn solid_hatch(pts: &[(f64, f64)]) -> EntityKind {
    let vertices = pts.iter().map(|&(x, y)| PolyVertex::new(Vec2::new(x, y))).collect();
    EntityKind::Hatch(cadcraft_engine::doc::Hatch {
        pattern: "SOLID".into(),
        solid: true,
        loops: vec![cadcraft_engine::doc::HatchLoop { vertices, outer: true, sources: Vec::new() }],
        scale: 1.0,
        angle: 0.0,
        associative: false,
        style: 0,
        elevation: 0.0,
        gradient: None,
        origin: Vec2::ZERO,
        background: None,
        pattern_lines: Vec::new(),
    })
}

/// The strokes TEXT or MTEXT `k` draws where it is (the built-in font draws with lines).
fn strokes(d: &Drawing, k: &EntityKind) -> Vec<Vec<Vec2>> {
    let s: Vec<Vec<Vec2>> = match k {
        EntityKind::Text(t) => render::place_text_entity(d, t, &t.value).0.strokes,
        EntityKind::MText(t) => render::layout_mtext_entity(d, t).pieces.into_iter().flat_map(|p| p.shaped.strokes).collect(),
        _ => panic!("not text"),
    };
    assert!(s.iter().any(|l| l.len() >= 2), "it draws with lines");
    s
}

/// A point on a letter: the middle of the first line of the first stroke.
fn on_letter(strokes: &[Vec<Vec2>]) -> Vec2 {
    let l = strokes.iter().find(|l| l.len() >= 2).unwrap();
    (l[0] + l[1]) * 0.5
}

/// A point on a letter halfway along the text: the middle of the first line of the middle stroke.
fn mid_letter(strokes: &[Vec<Vec2>]) -> Vec2 {
    let lines: Vec<&Vec<Vec2>> = strokes.iter().filter(|l| l.len() >= 2).collect();
    let l = lines[lines.len() / 2];
    (l[0] + l[1]) * 0.5
}

/// The box around the letters.
fn letters_box(strokes: &[Vec<Vec2>]) -> Bounds2 {
    Bounds2::from_points(strokes.iter().flatten().copied())
}

fn insert(block: &str, x: f64, y: f64) -> Insert {
    Insert {
        block: block.into(),
        insert: Vec3::new(x, y, 0.0),
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: Vec::new(),
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    }
}

/// Define block `name` holding `kinds`, with its base point at the origin.
fn define(d: &mut Drawing, name: &str, kinds: Vec<EntityKind>) {
    let mut b = Block::new(name);
    for k in kinds {
        let h = d.new_handle();
        b.entities.push(Entity::new(h, k));
    }
    d.blocks.insert(name.into(), Arc::new(b));
}

fn add(d: &mut Drawing, k: EntityKind) -> Handle {
    d.add(&Space::Model, Common::default(), k).unwrap()
}

fn at(d: &Drawing, x: f64, y: f64) -> Option<Handle> {
    pick(d, &Space::Model, Vec2::new(x, y), AP)
}

fn crossing(d: &Drawing, x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Handle> {
    select_window(d, &Space::Model, Bounds2::new(Vec2::new(x0, y0), Vec2::new(x1, y1)), true)
}

#[test]
fn a_block_is_picked_on_its_drawing_not_on_its_bounding_rectangle() {
    let mut d = Drawing::new_metric();
    define(&mut d, "RING", vec![circle(0.0, 0.0, 10.0)]);
    let b = add(&mut d, EntityKind::Insert(insert("RING", 100.0, 0.0)));
    // On the circle.
    assert_eq!(at(&d, 110.0, 0.0), Some(b));
    assert_eq!(at(&d, 100.0, 10.1), Some(b));
    // On the edge of the bounding rectangle, away from the circle, and at its corner.
    assert_eq!(at(&d, 110.0, 8.0), None);
    assert_eq!(at(&d, 110.0, 10.0), None);
    assert_eq!(at(&d, 92.0, -10.0), None);
    // Inside the circle, where nothing is drawn.
    assert_eq!(at(&d, 100.0, 0.0), None);
}

#[test]
fn a_crossing_window_inside_a_blocks_bounding_rectangle_selects_it_only_across_its_drawing() {
    let mut d = Drawing::new_metric();
    define(&mut d, "RING", vec![circle(0.0, 0.0, 10.0)]);
    let b = add(&mut d, EntityKind::Insert(insert("RING", 100.0, 0.0)));
    // In the empty corner of the bounding rectangle, outside the circle.
    assert!(crossing(&d, 108.5, 8.5, 109.5, 9.5).is_empty());
    // Inside the circle, touching nothing.
    assert!(crossing(&d, 95.0, -5.0, 105.0, 5.0).is_empty());
    // Across the circle.
    assert_eq!(crossing(&d, 108.0, -1.0, 112.0, 1.0), vec![b]);
    // A window around the whole block still selects it.
    assert_eq!(select_window(&d, &Space::Model, Bounds2::new(Vec2::new(85.0, -15.0), Vec2::new(115.0, 15.0)), false), vec![b]);
}

#[test]
fn a_fence_across_a_blocks_empty_bounding_rectangle_misses_it() {
    let mut d = Drawing::new_metric();
    define(&mut d, "RING", vec![circle(0.0, 0.0, 10.0)]);
    let b = add(&mut d, EntityKind::Insert(insert("RING", 100.0, 0.0)));
    // Through the corner of the rectangle only.
    let corner = [Vec2::new(107.0, 12.0), Vec2::new(112.0, 7.0)];
    assert!(select_fence(&d, &Space::Model, &corner).is_empty());
    let across = [Vec2::new(95.0, 0.0), Vec2::new(115.0, 0.0)];
    assert_eq!(select_fence(&d, &Space::Model, &across), vec![b]);
}

#[test]
fn a_block_is_picked_on_the_letters_of_its_text_and_the_edges_of_its_fills() {
    let mut d = Drawing::new_metric();
    // A label over a line, and a solid triangle beside them.
    let label = text(0.0, 0.0, 2.5, "A");
    let on = on_letter(&strokes(&d, &label)) + Vec2::new(200.0, 0.0);
    define(&mut d, "TAG", vec![label, line(0.0, -5.0, 30.0, -5.0), solid([(40.0, -5.0), (50.0, -5.0), (40.0, 5.0), (40.0, 5.0)])]);
    let b = add(&mut d, EntityKind::Insert(insert("TAG", 200.0, 0.0)));
    // On the letter: the text draws there.
    assert_eq!(at(&d, on.x, on.y), Some(b));
    // Beside the letter, over nothing.
    assert_eq!(at(&d, 220.0, 1.25), None);
    // On the edges of the solid triangle; not inside it, away from its edges, nor beside it.
    assert_eq!(at(&d, 245.0, -5.0), Some(b));
    assert_eq!(at(&d, 240.0, 0.0), Some(b));
    assert_eq!(at(&d, 242.0, -3.0), None);
    assert_eq!(at(&d, 248.0, 3.0), None);
    // Crossing windows on the letter and across an edge; not inside the fill.
    assert_eq!(crossing(&d, on.x - 0.1, on.y - 0.1, on.x + 0.1, on.y + 0.1), vec![b]);
    assert_eq!(crossing(&d, 244.0, -6.0, 246.0, -4.0), vec![b]);
    assert!(crossing(&d, 241.0, -4.0, 243.0, -2.0).is_empty());
    assert!(crossing(&d, 218.0, 0.0, 222.0, 2.0).is_empty());
}

#[test]
fn a_block_is_picked_on_its_attributes_and_nested_blocks() {
    let mut d = Drawing::new_metric();
    define(&mut d, "RING", vec![circle(0.0, 0.0, 10.0)]);
    define(&mut d, "OUTER", vec![EntityKind::Insert(insert("RING", 20.0, 0.0)), line(0.0, -10.0, 0.0, 10.0)]);
    let mut ins = insert("OUTER", 300.0, 0.0);
    let attrib = json!({ "tag": "N", "text": { "insert": v3(300.0, 15.0), "height": 2.5, "value": "D1" } });
    ins.attribs.push(serde_json::from_value(attrib).unwrap());
    let on = on_letter(&strokes(&d, &EntityKind::Text(ins.attribs[0].text.clone())));
    let b = add(&mut d, EntityKind::Insert(ins));
    // The nested circle and the attribute's letters.
    assert_eq!(at(&d, 330.0, 0.0), Some(b));
    assert_eq!(at(&d, on.x, on.y), Some(b));
    // The empty corner of the bounding rectangle (circle 310..330, line at 300, attribute at 15).
    assert_eq!(at(&d, 329.0, -10.0), None);
}

#[test]
fn an_mtext_is_picked_on_its_words_not_across_its_column() {
    let mut d = Drawing::new_metric();
    // A door leaf drawn across the wide column of the door's label (the label drawn on top).
    let leaf = add(&mut d, line(50.0, -10.0, 50.0, 10.0));
    let label = add(&mut d, mtext(0.0, 0.0, 2.5, 100.0, "P02"));
    // On the words.
    assert_eq!(at(&d, 1.0, -1.25), Some(label));
    // In the column, far right of the words: nothing, or the leaf drawn there.
    assert_eq!(at(&d, 80.0, -1.25), None);
    assert_eq!(at(&d, 50.0, -1.25), Some(leaf));
    // A crossing window in the empty column misses the label.
    assert!(crossing(&d, 70.0, -2.0, 75.0, -1.0).is_empty());
    assert_eq!(crossing(&d, 0.5, -2.0, 1.5, -1.0), vec![label]);
}

#[test]
fn an_mtext_in_a_block_is_picked_on_its_words_only() {
    let mut d = Drawing::new_metric();
    define(&mut d, "LABEL", vec![mtext(0.0, 0.0, 2.5, 100.0, "P02")]);
    let b = add(&mut d, EntityKind::Insert(insert("LABEL", 0.0, 50.0)));
    assert_eq!(at(&d, 1.0, 48.75), Some(b));
    assert_eq!(at(&d, 80.0, 48.75), None);
}

#[test]
fn a_rotated_mtext_is_picked_on_its_turned_letters() {
    let mut d = Drawing::new_metric();
    let mut k = mtext(0.0, 0.0, 2.5, 100.0, "P02 P02 P02");
    if let EntityKind::MText(t) = &mut k {
        t.rotation = std::f64::consts::FRAC_PI_4;
    }
    let on = on_letter(&strokes(&d, &k));
    let label = add(&mut d, k);
    assert_eq!(at(&d, on.x, on.y), Some(label));
    // Inside the axis-aligned box around the words but outside the turned one.
    assert_eq!(at(&d, 15.0, 2.0), None);
}

#[test]
fn text_in_nested_turned_scaled_and_arrayed_blocks_is_picked_where_it_is_drawn() {
    let mut d = Drawing::new_metric();
    define(&mut d, "INNER", vec![text(0.0, 0.0, 2.5, "A")]);
    // INNER doubled and turned 90° inside OUTER: its letter covers x 5..10, y 0..4.5 of OUTER.
    let mut inner = insert("INNER", 10.0, 0.0);
    inner.scale = Vec3::new(2.0, 2.0, 1.0);
    inner.rotation = std::f64::consts::FRAC_PI_2;
    define(&mut d, "OUTER", vec![EntityKind::Insert(inner)]);
    // Two copies of OUTER, 100 apart.
    let mut ins = insert("OUTER", 500.0, 0.0);
    ins.cols = 2;
    ins.col_spacing = 100.0;
    let b = add(&mut d, EntityKind::Insert(ins));
    assert_eq!(at(&d, 507.5, 2.25), Some(b));
    assert_eq!(at(&d, 607.5, 2.25), Some(b));
    assert_eq!(at(&d, 590.0, 2.25), None);
    assert_eq!(crossing(&d, 606.0, 1.0, 607.0, 2.0), vec![b]);
    assert!(crossing(&d, 580.0, 1.0, 585.0, 2.0).is_empty());
}

#[test]
fn attributes_are_picked_as_they_are_drawn() {
    let attrib = |tag: &str, value: &str, x: f64, rot: f64, invisible: bool| {
        let a = json!({ "tag": tag, "invisible": invisible, "text": { "insert": v3(x, 0.0), "height": 2.5, "value": value, "rotation": rot } });
        serde_json::from_value(a).unwrap()
    };
    let mut d = Drawing::new_metric();
    define(&mut d, "DOT", vec![circle(0.0, 0.0, 1.0)]);
    // A reference turned 45° with its attribute turned with it.
    let q = std::f64::consts::FRAC_PI_4;
    let mut ins = insert("DOT", 0.0, 0.0);
    ins.rotation = q;
    ins.attribs.push(attrib("N", "CONFERENCE ROOM", 0.0, q, false));
    // An attribute is placed where it is drawn: halfway along its words, turned 45°.
    let on = mid_letter(&strokes(&d, &EntityKind::Text(ins.attribs[0].text.clone())));
    assert!(on.x > 4.0 && on.y > 4.0, "away from the dot: {on:?}");
    let b = add(&mut d, EntityKind::Insert(ins));
    assert_eq!(at(&d, on.x, on.y), Some(b));
    // Inside the axis-aligned box around the turned words, far from them.
    assert_eq!(at(&d, 20.0, 2.0), None);
    // ATTMODE 0 draws no attribute, so none is picked.
    d.header.set_i64("ATTMODE", 0);
    assert_eq!(at(&d, on.x, on.y), None);
    d.header.set_i64("ATTMODE", 1);

    // An attribute whose definition is on a layer that is off isn't drawn.
    let mut d = Drawing::new_metric();
    d.ensure_layer("DATA");
    let mut def = Entity::new(
        d.new_handle(),
        EntityKind::AttDef(serde_json::from_value(json!({ "tag": "N", "text": { "insert": v3(0.0, 0.0), "height": 2.5, "value": "" } })).unwrap()),
    );
    def.common.layer = "DATA".into();
    let mut blk = Block::new("TAGGED");
    blk.entities.push(Entity::new(d.new_handle(), circle(0.0, 0.0, 1.0)));
    blk.entities.push(def);
    d.blocks.insert("TAGGED".into(), Arc::new(blk));
    let mut ins = insert("TAGGED", 0.0, 0.0);
    ins.attribs.push(attrib("N", "D1", 10.0, 0.0, false));
    // No definition: it follows the reference.
    ins.attribs.push(attrib("S", "SECRET", 4.0, 0.0, true));
    let (d1, secret) = (on_letter(&strokes(&d, &EntityKind::Text(ins.attribs[0].text.clone()))), ins.attribs[1].text.clone());
    // On a letter of SECRET away from D1 (which starts at 10).
    let secret = strokes(&d, &EntityKind::Text(secret)).into_iter().flatten().find(|q| q.x < 9.0).unwrap();
    let b = add(&mut d, EntityKind::Insert(ins));
    assert_eq!(at(&d, d1.x, d1.y), Some(b));
    d.layers.iter_mut().filter(|l| l.name == "DATA").for_each(|l| l.on = false);
    assert_eq!(at(&d, d1.x, d1.y), None);
    // An invisible attribute is drawn, and picked, only with ATTMODE 2 (inside the reference's
    // bounds, which leave invisible attributes out).
    assert_eq!(at(&d, secret.x, secret.y), None);
    d.header.set_i64("ATTMODE", 2);
    assert_eq!(at(&d, secret.x, secret.y), Some(b));
}

/// The kind of the object at handle `h` in model space.
fn kind_of(s: &Session, h: Handle) -> EntityKind {
    s.doc().unwrap().entity(h).unwrap().kind.clone()
}

fn handle(r: &Value) -> Handle {
    let hex = r["handle"].as_str().or_else(|| r["handles"][0].as_str()).unwrap();
    Handle(u64::from_str_radix(hex, 16).unwrap())
}

#[test]
fn dimension_and_multileader_text_in_a_block_is_picked() {
    let mut s = Session::new();
    // A dimension drawn from its block (as AutoCAD saves them), whose text sits at 45..52, 20..22.5.
    let dim = handle(&s.execute("dimlinear", &json!({ "p1": [0, 0], "p2": [100, 0], "at": [50, 20] })).unwrap());
    let note = handle(&s.execute("mleader", &json!({ "points": [[0, -40], [5, -35]], "text": "NOTE" })).unwrap());
    let (mut dk, nk) = (kind_of(&s, dim), kind_of(&s, note));
    let d = s.doc_mut().unwrap();
    define(d, "*D9", vec![text(45.0, 20.0, 2.5, "100"), line(0.0, 18.0, 100.0, 18.0)]);
    if let EntityKind::Dimension(dm) = &mut dk {
        dm.block = Some("*D9".into());
    }
    define(d, "DETAIL", vec![dk, nk.clone()]);
    let b = add(d, EntityKind::Insert(insert("DETAIL", 1000.0, 0.0)));
    let d = s.doc().unwrap();
    // On the dimension's number, above its line; and on the multileader's words.
    assert_eq!(at(d, 1046.0, 21.25), Some(b));
    let EntityKind::MLeader(m) = &nk else { panic!("not a multileader") };
    let words = cadcraft_engine::render::layout_mtext_entity(d, m.text.as_ref().unwrap()).strokes;
    let c = Bounds2::from_points(words.into_iter().flatten()).center();
    assert_eq!(at(d, c.x + 1000.0, c.y), Some(b));
    // Beside the number, over nothing.
    assert_eq!(at(d, 1080.0, 21.25), None);
}

#[test]
fn the_entities_inquiry_still_finds_a_block_on_a_layer_that_is_off() {
    let mut s = Session::new();
    s.execute("layer.new", &json!({ "name": "FURN" })).unwrap();
    let d = s.doc_mut().unwrap();
    define(d, "DESK", vec![line(0.0, 0.0, 10.0, 0.0), line(10.0, 0.0, 10.0, 5.0), line(10.0, 5.0, 0.0, 5.0), line(0.0, 5.0, 0.0, 0.0)]);
    let common = Common { layer: "FURN".into(), ..Common::default() };
    d.add(&Space::Model, common, EntityKind::Insert(insert("DESK", 0.0, 0.0))).unwrap();
    s.execute("layer.set", &json!({ "name": "FURN", "on": false })).unwrap();
    for w in [json!([[-1, 2], [1, 3]]), json!([[2, 2], [3, 3]])] {
        let r = s.execute("entities", &json!({ "layer": "FURN", "window": w, "crossing": true })).unwrap();
        assert_eq!(r["count"], 1, "{w}: {r}");
    }
}

#[test]
fn a_wrapped_mtext_in_a_block_is_picked_on_its_last_line() {
    let mut d = Drawing::new_metric();
    // A frame 12 × 40 around a note 10 wide that wraps a word per line, far down the frame.
    let note = mtext(1.0, -1.0, 2.5, 10.0, "ONE TWO THREE FOUR FIVE SIX SEVEN EIGHT");
    let s = strokes(&d, &note);
    let words = letters_box(&s);
    assert!(words.min.y < -25.0, "it wraps: {words:?}");
    // The lowest point of the last line, 2 or more from the frame.
    let low =
        s.iter().flatten().copied().filter(|q| q.x > 2.0 && q.x < 10.0).fold(Vec2::new(0.0, f64::INFINITY), |a, q| if q.y < a.y { q } else { a });
    let frame = vec![line(0.0, 0.0, 12.0, 0.0), line(12.0, 0.0, 12.0, -40.0), line(12.0, -40.0, 0.0, -40.0), line(0.0, -40.0, 0.0, 0.0)];
    define(&mut d, "NOTES", frame.into_iter().chain([note]).collect());
    let b = add(&mut d, EntityKind::Insert(insert("NOTES", 3000.0, 0.0)));
    // On the last line, away from the frame.
    assert!(low.x > 2.0 && low.x < 10.0 && low.y < -25.0, "{low:?}");
    let p = low + Vec2::new(3000.0, 0.0);
    assert_eq!(at(&d, p.x, p.y), Some(b));
    assert_eq!(crossing(&d, p.x - 0.1, p.y - 0.1, p.x + 0.1, p.y + 0.1), vec![b]);
}

#[test]
fn a_dimension_made_in_cadcraft_in_a_block_is_picked_on_its_text() {
    let mut s = Session::new();
    // CADCraft's own dimensions have no block: they are drawn from their definition.
    let dim = handle(&s.execute("dimlinear", &json!({ "p1": [0, 0], "p2": [100, 0], "at": [50, 20] })).unwrap());
    let dk = kind_of(&s, dim);
    let EntityKind::Dimension(dm) = &dk else { panic!("not a dimension") };
    let g = cadcraft_engine::render::dimension_in(s.doc().unwrap(), dm);
    let words = Bounds2::from_points(g.text.iter().flatten().chain(g.text_glyphs.iter().flatten().flatten()).copied());
    assert!(!words.is_empty(), "the dimension shows its value");
    let d = s.doc_mut().unwrap();
    define(d, "DETAIL", vec![dk.clone()]);
    let b = add(d, EntityKind::Insert(insert("DETAIL", 1000.0, 0.0)));
    let d = s.doc().unwrap();
    let c = words.center();
    assert_eq!(at(d, c.x + 1000.0, c.y), Some(b));
}

/// An MTEXT `contents` in a block whose only other drawing is a far frame, picked at the right end
/// of its words (and at the top of its words, for a slanted style).
fn picked_at_its_far_end(d: &mut Drawing, contents: &str, style: &str) {
    let mut k = mtext(0.0, 0.0, 2.5, 0.0, contents);
    if let EntityKind::MText(t) = &mut k {
        t.style = style.into();
    }
    let s = strokes(d, &k);
    let words = letters_box(&s);
    // The point of the letters farthest right and up.
    let far = s.iter().flatten().copied().fold(Vec2::new(f64::NEG_INFINITY, 0.0), |a, q| if q.x + q.y > a.x + a.y { q } else { a });
    let name = format!("FAR{}", d.blocks.len());
    define(d, &name, vec![k, line(0.0, -300.0, 300.0, -300.0)]);
    let b = add(d, EntityKind::Insert(insert(&name, 5000.0, 0.0)));
    let p = far + Vec2::new(5000.0, 0.0);
    assert_eq!(at(d, p.x, p.y), Some(b), "{contents:?} in style {style}: words {words:?}");
}

#[test]
fn mtext_set_far_by_tabs_is_picked_in_a_block() {
    picked_at_its_far_end(&mut Drawing::new_metric(), "D1\t\t\t\t\t\t\t\tX", "Standard");
}

#[test]
fn mtext_set_far_by_paragraph_tab_stops_is_picked_in_a_block() {
    picked_at_its_far_end(&mut Drawing::new_metric(), "{\\pxt40;1\tPILASTRO}", "Standard");
}

#[test]
fn mtext_in_a_steeply_slanted_style_is_picked_in_a_block() {
    let mut d = Drawing::new_metric();
    let mut steep = d.text_style("Standard").unwrap().clone();
    steep.name = "STEEP".into();
    steep.oblique = 85f64.to_radians();
    d.text_styles.push(steep);
    picked_at_its_far_end(&mut d, "A", "STEEP");
}

#[test]
fn dimensions_and_multileaders_are_picked_on_what_they_draw_not_their_bounding_rectangle() {
    let mut s = Session::new();
    let dim = handle(&s.execute("dimlinear", &json!({ "p1": [0, 0], "p2": [100, 0], "at": [50, 20] })).unwrap());
    let note = handle(&s.execute("mleader", &json!({ "points": [[300, 0], [310, 10]], "text": "NOTE" })).unwrap());
    let d = s.doc().unwrap();
    for h in [dim, note] {
        let e = d.entity(h).unwrap();
        let b = cadcraft_engine::doc::entity_bounds(d, e, 0);
        // The corner of the bounding rectangle below its right end: nothing drawn there.
        let corner = Vec2::new(b.max.x, b.min.y);
        assert_eq!(at(d, corner.x, corner.y), None, "{h:?} at {corner:?} in {b:?}");
    }
    // A crossing window on the dimension's words only, and on the multileader's.
    let EntityKind::Dimension(dm) = &d.entity(dim).unwrap().kind else { panic!("not a dimension") };
    let g = cadcraft_engine::render::dimension_in(d, dm);
    let c = Bounds2::from_points(g.text.iter().flatten().chain(g.text_glyphs.iter().flatten().flatten()).copied()).center();
    assert_eq!(crossing(d, c.x - 0.1, c.y - 0.1, c.x + 0.1, c.y + 0.1), vec![dim]);
    let EntityKind::MLeader(m) = &d.entity(note).unwrap().kind else { panic!("not a multileader") };
    let words = cadcraft_engine::render::layout_mtext_entity(d, m.text.as_ref().unwrap()).strokes;
    let c = Bounds2::from_points(words.into_iter().flatten()).center();
    assert_eq!(crossing(d, c.x - 0.1, c.y - 0.1, c.x + 0.1, c.y + 0.1), vec![note]);
}

#[test]
fn text_is_picked_down_to_the_depth_it_is_drawn_at() {
    let mut s = Session::new();
    // A multileader whose content is a block holding the letter, at the deepest level drawn: the
    // content is drawn at the multileader's own depth (15), its letter one below.
    let note = handle(&s.execute("mleader", &json!({ "points": [[100, 100], [110, 110]], "text": "N" })).unwrap());
    let mut k = kind_of(&s, note);
    if let EntityKind::MLeader(m) = &mut k {
        m.text = None;
        m.block = Some(insert("TXT", 0.0, 0.0));
    }
    let d = s.doc_mut().unwrap();
    define(d, "TXT", vec![text(0.0, 0.0, 2.5, "A")]);
    define(d, "L14", vec![k]);
    for i in (0..14).rev() {
        define(d, &format!("L{i}"), vec![EntityKind::Insert(insert(&format!("L{}", i + 1), 0.0, 0.0))]);
    }
    let b = add(d, EntityKind::Insert(insert("L0", 0.0, 0.0)));
    let d = s.doc().unwrap();
    assert_eq!(at(d, 1.0, 1.25), Some(b));
}

/// `"I     I"`: two letters with a wide gap, and the middle of the gap.
fn gap_of(d: &Drawing, k: &EntityKind) -> (Vec<Vec<Vec2>>, Vec2) {
    let s = strokes(d, k);
    (s.clone(), letters_box(&s).center())
}

#[test]
fn text_is_picked_on_its_letters_not_between_them() {
    let mut d = Drawing::new_metric();
    let k = text(1000.0, 0.0, 10.0, "I     I");
    let (s, gap) = gap_of(&d, &k);
    let t = add(&mut d, k);
    assert_eq!(at(&d, on_letter(&s).x, on_letter(&s).y), Some(t));
    // Between the letters, inside the box around them.
    assert_eq!(at(&d, gap.x, gap.y), None);
    assert!(crossing(&d, gap.x - 1.0, gap.y - 1.0, gap.x + 1.0, gap.y + 1.0).is_empty());
    let on = on_letter(&s);
    assert_eq!(crossing(&d, on.x - 0.1, on.y - 0.1, on.x + 0.1, on.y + 0.1), vec![t]);

    // Turned 30°, the same.
    let mut k = text(2000.0, 0.0, 10.0, "I     I");
    if let EntityKind::Text(t) = &mut k {
        t.rotation = 30f64.to_radians();
    }
    let (s, gap) = gap_of(&d, &k);
    let t = add(&mut d, k);
    assert_eq!(at(&d, on_letter(&s).x, on_letter(&s).y), Some(t));
    assert_eq!(at(&d, gap.x, gap.y), None);

    // Not inside the hole of an O.
    let k = text(3000.0, 0.0, 10.0, "O");
    let (s, hole) = gap_of(&d, &k);
    let o = add(&mut d, k);
    assert_eq!(at(&d, on_letter(&s).x, on_letter(&s).y), Some(o));
    assert_eq!(at(&d, hole.x, hole.y), None);
}

#[test]
fn text_in_a_block_is_picked_on_its_letters_not_between_them() {
    let mut d = Drawing::new_metric();
    let k = text(10.0, 0.0, 10.0, "I     I");
    let (s, gap) = gap_of(&d, &k);
    define(&mut d, "GAP", vec![k, line(0.0, -25.0, 170.0, -25.0)]);
    let b = add(&mut d, EntityKind::Insert(insert("GAP", 4000.0, 0.0)));
    let (on, gap) = (on_letter(&s) + Vec2::new(4000.0, 0.0), gap + Vec2::new(4000.0, 0.0));
    assert_eq!(at(&d, on.x, on.y), Some(b));
    assert_eq!(at(&d, gap.x, gap.y), None);
    assert!(crossing(&d, gap.x - 1.0, gap.y - 1.0, gap.x + 1.0, gap.y + 1.0).is_empty());
}

#[test]
fn an_mtext_is_selected_by_a_window_around_its_letters_and_crossed_only_on_them() {
    let mut d = Drawing::new_metric();
    // A column 150 wide holding "I     I" and, a paragraph below, "I".
    let k = mtext(0.0, 0.0, 10.0, 150.0, "I     I\\PI");
    let s = strokes(&d, &k);
    let letters = letters_box(&s);
    assert!(letters.max.x < 75.0, "the letters fill half the column: {letters:?}");
    let first_line: Vec<Vec2> = s.iter().flatten().copied().filter(|q| q.y > letters.center().y).collect();
    let gap = Bounds2::from_points(first_line).center();
    let t = add(&mut d, k);
    // Between the two letters of the first line, and in the empty column right of them.
    assert!(crossing(&d, gap.x - 1.0, gap.y - 1.0, gap.x + 1.0, gap.y + 1.0).is_empty());
    assert!(crossing(&d, 100.0, -6.0, 110.0, -4.0).is_empty());
    // A window around the letters selects it, though not around the whole column; one that cuts
    // through the letters doesn't.
    let window = |b: Bounds2| select_window(&d, &Space::Model, b, false);
    assert_eq!(window(letters.expand(0.5)), vec![t]);
    assert_eq!(window(Bounds2::new(Vec2::new(-5.0, -40.0), Vec2::new(160.0, 5.0))), vec![t]);
    assert!(window(Bounds2::new(letters.min - Vec2::new(0.5, 0.5), Vec2::new(gap.x, letters.max.y + 0.5))).is_empty());
}

#[test]
fn fills_in_a_block_are_picked_on_their_edges_and_a_solid_also_on_its_diagonal() {
    let mut d = Drawing::new_metric();
    // A square SOLID 0..40 (corners 1, 2, 3, 4 = bottom left, bottom right, top left, top right)
    // and a triangle of solid hatch 50,0 90,0 50,40.
    let square = solid([(0.0, 0.0), (40.0, 0.0), (0.0, 40.0), (40.0, 40.0)]);
    let tri = [(50.0, 0.0), (90.0, 0.0), (50.0, 40.0)];
    define(&mut d, "FILLS", vec![square, solid_hatch(&tri)]);
    let b = add(&mut d, EntityKind::Insert(insert("FILLS", 5000.0, 0.0)));
    // The square: on its edges and on the diagonal from its second to its third corner (x + y =
    // 40); not inside, away from them, nor on the other diagonal.
    for (x, y) in [(5020.0, 0.0), (5000.0, 20.0), (5040.0, 30.0), (5020.0, 40.0), (5020.0, 20.0), (5010.0, 30.0)] {
        assert_eq!(at(&d, x, y), Some(b), "on the square at {x},{y}");
    }
    for (x, y) in [(5010.0, 10.0), (5030.0, 30.0), (5005.0, 5.0)] {
        assert_eq!(at(&d, x, y), None, "inside the square at {x},{y}");
    }
    // The triangle: on its edges, not inside.
    for (x, y) in [(5070.0, 0.0), (5050.0, 20.0), (5070.0, 20.0)] {
        assert_eq!(at(&d, x, y), Some(b), "on the triangle at {x},{y}");
    }
    assert_eq!(at(&d, 5057.0, 7.0), None);
    // Crossing windows: across the diagonal of the square, inside the triangle, in the empty
    // corner between them.
    assert_eq!(crossing(&d, 5015.0, 15.0, 5025.0, 25.0), vec![b]);
    assert!(crossing(&d, 5055.0, 5.0, 5060.0, 10.0).is_empty());
    assert!(crossing(&d, 5043.0, 30.0, 5047.0, 38.0).is_empty());

    // The same triangle on its own is picked anywhere inside it.
    let h = add(&mut d, solid_hatch(&[(7000.0, 0.0), (7040.0, 0.0), (7000.0, 40.0)]));
    assert_eq!(at(&d, 7007.0, 7.0), Some(h));
}

/// Define block `name` holding `kinds`, each with its own properties.
fn define_with(d: &mut Drawing, name: &str, kinds: Vec<(Common, EntityKind)>) {
    let mut b = Block::new(name);
    for (c, k) in kinds {
        let mut e = Entity::new(d.new_handle(), k);
        e.common = c;
        b.entities.push(e);
    }
    d.blocks.insert(name.into(), Arc::new(b));
}

#[test]
fn a_dashed_line_in_a_block_is_picked_across_its_gaps_as_on_its_own() {
    let mut d = Drawing::new_metric();
    d.linetypes = cadcraft_engine::doc::library::standard_linetypes_for(&d);
    // Dots 6.35 apart, the line at y 0 in a block and the same at y 50 on its own; x 50 is in a gap.
    let dotted = || Common { linetype: "DOT".into(), ..Common::default() };
    define_with(&mut d, "DOTS", vec![(dotted(), line(0.0, 0.0, 100.0, 0.0))]);
    let b = add(&mut d, EntityKind::Insert(insert("DOTS", 0.0, 0.0)));
    let alone = d.add(&Space::Model, dotted(), line(0.0, 50.0, 100.0, 50.0)).unwrap();
    assert_eq!(at(&d, 50.0, 50.0), Some(alone));
    assert_eq!(at(&d, 50.0, 0.0), Some(b));
    assert_eq!(crossing(&d, 49.0, -0.1, 51.0, 0.1), vec![b]);
}

#[test]
fn a_wide_polyline_in_a_block_is_picked_on_the_mitred_sides_and_the_diagonal_of_each_straight_segment() {
    let mut d = Drawing::new_metric();
    // An L 10 wide: 0,0 50,0 50,50.
    let v = [(0.0, 0.0), (50.0, 0.0), (50.0, 50.0)].map(|(x, y)| PolyVertex::new(Vec2::new(x, y)));
    let pl = cadcraft_engine::doc::LwPolyline { vertices: v.to_vec(), closed: false, const_width: 10.0, elevation: 0.0, plinegen: false };
    define(&mut d, "WIDE", vec![EntityKind::LwPolyline(pl)]);
    let b = add(&mut d, EntityKind::Insert(insert("WIDE", 0.0, 0.0)));
    // Where the segments meet their quads are mitred: the first is 0,5 0,-5 55,-5 45,5, the
    // second 45,5 55,-5 55,50 45,50. On the edges of the band and its ends, on the mitre line
    // between the segments, and on the diagonal of each from the right corner of its start to
    // the left corner of its end (0,-5 to 45,5; 55,-5 to 45,50).
    for (x, y) in [
        (25.0, 5.0),
        (25.0, -5.0),
        (53.0, -5.0),
        (55.0, 25.0),
        (45.0, 25.0),
        (0.0, 2.0),
        (48.0, 50.0),
        (50.0, 0.0),
        (48.0, 2.0),
        (22.5, 0.0),
        (9.0, -3.0),
        (36.0, 3.0),
        (50.0, 22.5),
        (53.0, 6.0),
        (47.0, 39.0),
    ] {
        assert_eq!(at(&d, x, y), Some(b), "on the band at {x},{y}");
    }
    // Not inside it off those lines: nor on the other diagonal (0,5 to 50,-5), where the two
    // triangles render fills the segment with meet.
    for (x, y) in [(25.0, 2.5), (52.0, 30.0), (10.0, 3.0), (40.0, -3.0), (47.0, 10.0)] {
        assert_eq!(at(&d, x, y), None, "inside the band at {x},{y}");
    }
    // A crossing window across the diagonal selects it; one inside the band off it (across the
    // other diagonal) doesn't.
    assert_eq!(crossing(&d, 24.0, -1.0, 26.0, 1.0), vec![b]);
    assert!(crossing(&d, 10.0, 2.0, 15.0, 4.0).is_empty());

    // Closed, a square 10 wide: every corner is mitred, the closing one too, so the closing
    // segment (0,50 to 0,0, going down) has its diagonal from -5,55 to 5,5 and the first one
    // from -5,-5 to 45,5.
    let sq = [(0.0, 0.0), (50.0, 0.0), (50.0, 50.0), (0.0, 50.0)].map(|(x, y)| PolyVertex::new(Vec2::new(x, y)));
    let pl = cadcraft_engine::doc::LwPolyline { vertices: sq.to_vec(), closed: true, const_width: 10.0, elevation: 0.0, plinegen: false };
    define(&mut d, "SQUARE", vec![EntityKind::LwPolyline(pl)]);
    let q = add(&mut d, EntityKind::Insert(insert("SQUARE", 1000.0, 0.0)));
    for (x, y) in [(1000.0, 30.0), (1002.0, 20.0), (1020.0, 0.0), (995.0, 25.0), (1005.0, 25.0), (1000.0, 0.0)] {
        assert_eq!(at(&d, x, y), Some(q), "on the closed square at {x},{y}");
    }
    assert_eq!(at(&d, 1003.0, 40.0), None);

    // A segment of no length left by a bulge that isn't a number (the reader never makes one)
    // adds nothing: the segments on either side end square there, and are picked on that end.
    let mut dg = [(0.0, 0.0), (20.0, 0.0), (20.0, 0.0), (50.0, 0.0)].map(|(x, y)| PolyVertex::new(Vec2::new(x, y)));
    dg[1].bulge = f64::NAN;
    let pl = cadcraft_engine::doc::LwPolyline { vertices: dg.to_vec(), closed: false, const_width: 10.0, elevation: 0.0, plinegen: false };
    define(&mut d, "DEGEN", vec![EntityKind::LwPolyline(pl)]);
    let g = add(&mut d, EntityKind::Insert(insert("DEGEN", 4000.0, 0.0)));
    assert_eq!(at(&d, 4020.0, 3.0), Some(g));
    assert_eq!(at(&d, 4010.0, 0.0), Some(g));
    assert_eq!(at(&d, 4010.0, 2.5), None);

    // Tapered, 10 wide at its start and 20 at its end: the diagonal runs from 0,-5 to 50,10.
    let mut t = [(0.0, 0.0), (50.0, 0.0)].map(|(x, y)| PolyVertex::new(Vec2::new(x, y)));
    (t[0].start_width, t[0].end_width) = (10.0, 20.0);
    let pl = cadcraft_engine::doc::LwPolyline { vertices: t.to_vec(), closed: false, const_width: 0.0, elevation: 0.0, plinegen: false };
    define(&mut d, "TAPER", vec![EntityKind::LwPolyline(pl)]);
    let c = add(&mut d, EntityKind::Insert(insert("TAPER", 2000.0, 0.0)));
    for (x, y) in [(2025.0, 2.5), (2040.0, 7.0), (2025.0, 7.5), (2025.0, -7.5), (2050.0, 5.0), (2000.0, 3.0)] {
        assert_eq!(at(&d, x, y), Some(c), "on the tapered band at {x},{y}");
    }
    for (x, y) in [(2025.0, -2.5), (2040.0, -4.0), (2010.0, 3.0)] {
        assert_eq!(at(&d, x, y), None, "inside the tapered band at {x},{y}");
    }

    // Curved: a half turn 10 wide from 50,0 to 50,50 round 50,25 is picked on its two edges (20
    // and 30 from the centre), not inside.
    let mut a = [(0.0, 0.0), (50.0, 0.0), (50.0, 50.0)].map(|(x, y)| PolyVertex::new(Vec2::new(x, y)));
    a[1].bulge = 1.0;
    let pl = cadcraft_engine::doc::LwPolyline { vertices: a.to_vec(), closed: false, const_width: 10.0, elevation: 0.0, plinegen: false };
    define(&mut d, "ARC", vec![EntityKind::LwPolyline(pl)]);
    let r = add(&mut d, EntityKind::Insert(insert("ARC", 3000.0, 0.0)));
    assert_eq!(at(&d, 3070.0, 25.0), Some(r));
    assert_eq!(at(&d, 3080.0, 25.0), Some(r));
    let hits: Vec<i32> = (-85..=85)
        .step_by(5)
        .filter(|deg| {
            let (s, c) = f64::from(*deg).to_radians().sin_cos();
            at(&d, 3050.0 + 25.0 * c, 25.0 + 25.0 * s).is_some()
        })
        .collect();
    assert!(hits.is_empty(), "picked inside the curved band at {hits:?} degrees");
}

#[test]
fn text_of_no_positive_height_in_a_block_is_picked_on_its_far_letters() {
    let mut d = Drawing::new_metric();
    // Drawn one unit high.
    let k = text(0.0, 0.0, -0.1, "ABCDEFGHIJKLMNOPQRST");
    let far = strokes(&d, &k).iter().rfind(|l| l.len() >= 2).map(|l| (l[0] + l[1]) * 0.5).unwrap();
    assert!(far.x > 10.0, "{far:?}");
    define(&mut d, "NEG", vec![k, line(0.0, -50.0, 0.0, 50.0), line(0.0, -50.0, 50.0, -50.0), line(0.0, 50.0, 50.0, 50.0)]);
    let b = add(&mut d, EntityKind::Insert(insert("NEG", 0.0, 0.0)));
    assert_eq!(at(&d, far.x, far.y), Some(b));
}

#[test]
fn a_dimension_in_a_text_style_of_fixed_height_in_a_block_is_picked_on_its_text() {
    let mut s = Session::new();
    let dim = handle(&s.execute("dimlinear", &json!({ "p1": [0, 0], "p2": [10, 0], "at": [5, 2] })).unwrap());
    let dk = kind_of(&s, dim);
    let d = s.doc_mut().unwrap();
    let EntityKind::Dimension(dm) = &dk else { panic!("not a dimension") };
    // Its text style 30 high: the text doesn't fit between the extension lines and goes far out.
    let ts = d.dim_style(&dm.style).map(|st| st.text_style.clone()).unwrap();
    d.text_styles.iter_mut().filter(|st| st.name.eq_ignore_ascii_case(&ts)).for_each(|st| st.height = 30.0);
    let g = render::dimension_in(d, dm);
    let o = Vec2::new(5.0, 2.0);
    let far = g.text.iter().filter(|l| l.len() >= 2).map(|l| (l[0] + l[1]) * 0.5).fold(o, |a, q| if q.dist(o) > a.dist(o) { q } else { a });
    assert!(far.dist(o) > 100.0, "{far:?}");
    define(d, "DETAIL", vec![dk.clone(), line(-200.0, -200.0, 200.0, -200.0), line(-200.0, 200.0, 200.0, 200.0)]);
    let b = add(d, EntityKind::Insert(insert("DETAIL", 1000.0, 0.0)));
    let d = s.doc().unwrap();
    let p = far + Vec2::new(1000.0, 0.0);
    assert_eq!(at(d, p.x, p.y), Some(b));
}

fn wide_pl(pts: &[(f64, f64)], w: f64) -> EntityKind {
    let v = pts.iter().map(|&(x, y)| PolyVertex::new(Vec2::new(x, y))).collect();
    EntityKind::LwPolyline(cadcraft_engine::doc::LwPolyline { vertices: v, closed: false, const_width: w, elevation: 0.0, plinegen: false })
}

#[test]
fn a_dotted_wide_polyline_in_a_block_is_picked_on_its_band_across_the_gaps() {
    let mut d = Drawing::new_metric();
    d.linetypes = cadcraft_engine::doc::library::standard_linetypes_for(&d);
    let dotted = Common { linetype: "DOT".into(), ..Common::default() };
    define_with(&mut d, "WDOT", vec![(dotted, wide_pl(&[(0.0, 0.0), (100.0, 0.0)], 10.0))]);
    let b = add(&mut d, EntityKind::Insert(insert("WDOT", 0.0, 0.0)));
    // The top edge of the band, where a gap between dots is.
    assert_eq!(at(&d, 48.0, 5.0), Some(b));
}

#[test]
fn fills_in_a_block_are_not_picked_where_their_triangles_meet_inside_them() {
    let mut d = Drawing::new_metric();
    // An L of wide polyline, an L of solid hatch and a disc of solid hatch.
    define(&mut d, "WIDE", vec![wide_pl(&[(0.0, 0.0), (50.0, 0.0), (50.0, 50.0)], 10.0)]);
    add(&mut d, EntityKind::Insert(insert("WIDE", 0.0, 0.0)));
    let l = solid_hatch(&[(0.0, 0.0), (40.0, 0.0), (40.0, 10.0), (10.0, 10.0), (10.0, 40.0), (0.0, 40.0)]);
    define(&mut d, "LFILL", vec![l]);
    let lb = add(&mut d, EntityKind::Insert(insert("LFILL", 1000.0, 0.0)));
    let mut disc = solid_hatch(&[(10.0, 0.0), (-10.0, 0.0)]);
    if let EntityKind::Hatch(h) = &mut disc {
        for v in &mut h.loops[0].vertices {
            v.bulge = 1.0;
        }
    }
    define(&mut d, "DISC", vec![disc]);
    let db = add(&mut d, EntityKind::Insert(insert("DISC", 2000.0, 0.0)));
    // Inside the band at its corner.
    for (x, y) in [(47.0, 0.0), (50.0, 2.0), (52.5, 2.5)] {
        assert_eq!(at(&d, x, y), None, "inside the band at {x},{y}");
    }
    // The L: on its edges, not inside at the height of its inner corner.
    assert_eq!(at(&d, 1020.0, 0.0), Some(lb));
    assert_eq!(at(&d, 1025.0, 10.0), Some(lb));
    assert_eq!(at(&d, 1005.0, 10.0), None);
    assert_eq!(at(&d, 1003.0, 5.0), None);
    // The disc: on its edge, nowhere inside along a line across it.
    assert_eq!(at(&d, 2010.0, 0.0), Some(db));
    let hits: Vec<f64> = (-180..=180).map(|i| f64::from(i) * 0.05).filter(|&y| at(&d, 2000.3, y).is_some()).collect();
    assert!(hits.is_empty(), "picked inside the disc at x 0.3, y {hits:?}");
    assert!(crossing(&d, 1002.0, 9.0, 1008.0, 11.0).is_empty());
}

#[test]
fn a_dimension_with_long_text_of_its_own_in_a_block_is_picked_on_its_far_letters() {
    let mut s = Session::new();
    let dim = handle(&s.execute("dimlinear", &json!({ "p1": [0, 0], "p2": [10, 0], "at": [5, 2] })).unwrap());
    let mut dk = kind_of(&s, dim);
    if let EntityKind::Dimension(dm) = &mut dk {
        dm.text = "A VERY LONG NOTE WRITTEN BY HAND OVER THE MEASURED VALUE, CHECK ON SITE BEFORE CUTTING".into();
    }
    let d = s.doc_mut().unwrap();
    let EntityKind::Dimension(dm) = &dk else { panic!("not a dimension") };
    let g = render::dimension_in(d, dm);
    let o = Vec2::new(5.0, 2.0);
    let far = g.text.iter().filter(|l| l.len() >= 2).map(|l| (l[0] + l[1]) * 0.5).fold(o, |a, q| if q.dist(o) > a.dist(o) { q } else { a });
    // Past its rough bounds grown by their own size.
    let eb = cadcraft_engine::doc::entity_bounds(d, &Entity::new(Handle(0), dk.clone()), 0);
    assert!(!eb.expand(eb.width().max(eb.height())).contains(far), "{far:?} {eb:?}");
    define(d, "DETAIL", vec![dk.clone(), line(-400.0, -400.0, 400.0, -400.0), line(-400.0, 400.0, 400.0, 400.0)]);
    let b = add(d, EntityKind::Insert(insert("DETAIL", 1000.0, 0.0)));
    let d = s.doc().unwrap();
    let p = far + Vec2::new(1000.0, 0.0);
    assert_eq!(at(d, p.x, p.y), Some(b));
}

#[test]
fn a_fill_in_a_block_spread_past_what_a_float_measures_does_not_crash_a_pick() {
    let mut d = Drawing::new_metric();
    let huge = solid([(-1e308, 0.0), (1e308, 0.0), (0.0, 1e308), (0.0, 1e308)]);
    define(&mut d, "HUGE", vec![huge]);
    add(&mut d, EntityKind::Insert(insert("HUGE", 0.0, 0.0)));
    let _ = at(&d, 0.0, 0.0);
    let _ = crossing(&d, -1.0, -1.0, 1.0, 1.0);
}

#[test]
fn inside_the_band_of_a_wide_polyline_in_a_block_by_its_corner_picks_nothing() {
    let mut d = Drawing::new_metric();
    define(&mut d, "WIDE", vec![wide_pl(&[(0.0, 0.0), (50.0, 0.0), (50.0, 50.0)], 10.0)]);
    add(&mut d, EntityKind::Insert(insert("WIDE", 0.0, 0.0)));
    // 47,-2 is 2 or more from every edge of the band, from where its segments meet and from the
    // diagonals of its segments (not from the other diagonal of the first, 0,5 to 50,-5, where
    // the two triangles render fills it with meet: that one isn't picked).
    assert_eq!(pick(&d, &Space::Model, Vec2::new(47.0, -2.0), 1.5), None);
    assert!(crossing(&d, 46.0, 0.5, 47.5, 1.5).is_empty());
    assert!(select_fence(&d, &Space::Model, &[Vec2::new(46.0, 0.5), Vec2::new(48.0, 1.5)]).is_empty());
}

#[test]
fn a_fill_in_a_block_with_a_thin_spike_keeps_its_whole_edge() {
    let mut d = Drawing::new_metric();
    // The bottom edge runs to 100,0 and back to a spike 1e-6 high at 50.
    let spike = solid_hatch(&[(0.0, 0.0), (100.0, 0.0), (50.0, 1e-6), (50.0, 10.0), (0.0, 10.0)]);
    define(&mut d, "SPIKE", vec![spike]);
    let b = add(&mut d, EntityKind::Insert(insert("SPIKE", 0.0, 0.0)));
    assert_eq!(select_fence(&d, &Space::Model, &[Vec2::new(75.0, -1.0), Vec2::new(75.0, 1.0)]), vec![b]);
    assert_eq!(crossing(&d, 95.0, -1.0, 105.0, 1.0), vec![b]);
}

#[test]
fn a_wide_polyline_crossing_itself_over_and_over_in_a_block_is_selected_in_bounded_time() {
    // 1000 strips 0.5 wide across 1000 others: a million crossings of their bands.
    let n = 1000;
    let len = 2.0 * n as f64;
    let mut pts = Vec::new();
    for i in 0..n {
        let y = 2.0 * i as f64;
        pts.extend(if i % 2 == 0 { [(0.0, y), (len, y)] } else { [(len, y), (0.0, y)] });
    }
    for j in 0..n {
        let x = 2.0 * j as f64 + 1.0;
        pts.extend(if j % 2 == 0 { [(x, len), (x, 0.0)] } else { [(x, 0.0), (x, len)] });
    }
    let mut d = Drawing::new_metric();
    define(&mut d, "GRID", vec![wide_pl(&pts, 0.5)]);
    let b = add(&mut d, EntityKind::Insert(insert("GRID", 0.0, 0.0)));
    let t = std::time::Instant::now();
    assert_eq!(crossing(&d, -10.0, 0.6, len * 0.9, len * 0.9), vec![b]);
    // Seconds at most even in a debug build (it took minutes when every pile-up was tried in full).
    assert!(t.elapsed() < std::time::Duration::from_secs(60), "{:?}", t.elapsed());
}

/// A ring of `n` points of radius `r` about `cx`,`cy`.
fn round_loop(cx: f64, cy: f64, r: f64, n: usize) -> Vec<PolyVertex> {
    (0..n)
        .map(|i| {
            let a = i as f64 / n as f64 * std::f64::consts::TAU;
            PolyVertex::new(Vec2::new(cx + r * a.cos(), cy + r * a.sin()))
        })
        .collect()
}

#[test]
fn a_slab_with_thousands_of_round_columns_in_a_block_shows_no_line_through_its_fill() {
    // A slab 100 × 120 with 60 × 30 round columns 0.3 across, as one solid hatch: about 88,000
    // triangles, among the busiest fills a real drawing holds.
    let mut loops = vec![[(0.0, 0.0), (100.0, 0.0), (100.0, 120.0), (0.0, 120.0)].map(|(x, y)| PolyVertex::new(Vec2::new(x, y))).to_vec()];
    let mut centres = Vec::new();
    for i in 0..60 {
        for j in 0..30 {
            let c = Vec2::new(1.25 + f64::from(i) * (97.5 / 60.0), 1.5 + f64::from(j) * (117.0 / 30.0));
            centres.push(c);
            loops.push(round_loop(c.x, c.y, 0.3, 48));
        }
    }
    let mut slab = solid_hatch(&[]);
    if let EntityKind::Hatch(h) = &mut slab {
        h.loops = loops
            .into_iter()
            .enumerate()
            .map(|(i, vertices)| cadcraft_engine::doc::HatchLoop { vertices, outer: i == 0, sources: Vec::new() })
            .collect();
    }
    let mut d = Drawing::new_metric();
    define(&mut d, "SLAB", vec![slab]);
    let b = add(&mut d, EntityKind::Insert(insert("SLAB", 0.0, 0.0)));
    let e = d.entity(b).unwrap().clone();
    let through = cadcraft_engine::select::hit_polylines(&d, &e, 1e-3)
        .iter()
        .flat_map(|pl| pl.windows(2).map(|w| (w[0] + w[1]) * 0.5).collect::<Vec<_>>())
        .filter(|m| m.x > 0.05 && m.x < 99.95 && m.y > 0.05 && m.y < 119.95 && centres.iter().all(|c| (*m - *c).len() > 0.35))
        .count();
    assert_eq!(through, 0, "{through} pieces of outline run through the fill");
}

#[test]
fn the_outline_of_a_wide_polyline_crossing_itself_by_the_thousand_takes_bounded_time() {
    // 3000 strips 0.5 wide across 3000 others: nine million crossings of their bands.
    let n = 3000;
    let len = 2.0 * n as f64;
    let mut pts = Vec::new();
    for i in 0..n {
        let y = 2.0 * i as f64;
        pts.extend(if i % 2 == 0 { [(0.0, y), (len, y)] } else { [(len, y), (0.0, y)] });
    }
    for j in 0..n {
        let x = 2.0 * j as f64 + 1.0;
        pts.extend(if j % 2 == 0 { [(x, len), (x, 0.0)] } else { [(x, 0.0), (x, len)] });
    }
    let mut d = Drawing::new_metric();
    define(&mut d, "GRID", vec![wide_pl(&pts, 0.5)]);
    let b = add(&mut d, EntityKind::Insert(insert("GRID", 0.0, 0.0)));
    let e = d.entity(b).unwrap().clone();
    let t = std::time::Instant::now();
    let _ = cadcraft_engine::select::hit_polylines(&d, &e, 1e-3);
    // Seconds at most, in a debug build too (it grew with the square of the triangles).
    assert!(t.elapsed() < std::time::Duration::from_secs(10), "{:?}", t.elapsed());
}

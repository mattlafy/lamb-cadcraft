//! A multi-page plot has a flat outline, one bookmark per page: the page's label and any title
//! for it, going to the page's top-left corner.

use cadcraft_doc::{Common, Drawing, EntityKind, Layout, Line, Space};
use cadcraft_geom::Vec3;
use cadcraft_io::plot;
use craft_pdf::syntax::object::{Array, Dict, Name, String as PdfString};
use serde_json::{Value, json};

#[path = "support/pdf_read.rs"]
mod pdf_read;

/// Each bookmark's text and the destination's page index and x, y.
fn bookmarks(bytes: &[u8]) -> Vec<(String, usize, f64, f64)> {
    let pdf = craft_pdf::open_slice(bytes, None).expect("the PDF opens");
    let pages: Vec<_> = pdf.pages().iter().map(|p| p.raw().obj_id()).collect();
    let catalog = pdf.xref().get::<Dict<'_>>(pdf.xref().root_id()).expect("a catalog");
    let mut at = catalog.get::<Dict<'_>>(b"Outlines").and_then(|o| o.get::<Dict<'_>>(b"First"));
    let mut out = Vec::new();
    while let Some(item) = at {
        let title = item.get::<PdfString<'_>>(b"Title").map(|t| pdf_read::text_string(t.as_bytes())).expect("a title");
        let dest = item.get::<Array<'_>>(b"Dest").expect("a destination");
        let mut it = dest.flex_iter();
        let page = it.next::<Dict<'_>>().and_then(|p| p.obj_id()).expect("a page");
        assert_eq!(it.next::<Name<'_>>().map(|n| n.as_str().to_string()).as_deref(), Some("XYZ"));
        let (x, y) = (it.next::<f64>().expect("x"), it.next::<f64>().expect("y"));
        out.push((title, pages.iter().position(|p| *p == Some(page)).expect("a page of the file"), x, y));
        at = item.get::<Dict<'_>>(b"Next");
    }
    out
}

fn line() -> EntityKind {
    EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(10.0, 10.0, 0.0) })
}

fn drawing() -> Drawing {
    let mut d = Drawing::new_metric();
    for (i, name) in ["A101", "A102", "A103"].into_iter().enumerate() {
        d.layouts.push(Layout::new(name, i as u32 + 10));
        d.add(&Space::Paper(name.into()), Common::default(), line()).unwrap();
    }
    d
}

fn texts(marks: &[(String, usize, f64, f64)]) -> Vec<&str> {
    marks.iter().map(|m| m.0.as_str()).collect()
}

/// The bookmarks of the three sheets plotted out of tab order.
fn sheets(d: &Drawing, extra: Value) -> Vec<(String, usize, f64, f64)> {
    let mut o = json!({"layouts": ["A103", "A101", "A102"], "compress": false});
    if let (Some(m), Some(e)) = (o.as_object_mut(), extra.as_object()) {
        m.extend(e.clone());
    }
    bookmarks(&plot(d, &Space::Model, &o).unwrap())
}

#[test]
fn every_page_has_a_bookmark_in_page_order() {
    let d = drawing();
    let marks = sheets(&d, json!({}));
    assert_eq!(texts(&marks), ["A103", "A101", "A102"]);
    let bytes = plot(&d, &Space::Model, &json!({"layouts": ["A103", "A101", "A102"]})).unwrap();
    let doc = pdf_read::read(&bytes);
    for (i, (_, page, x, y)) in marks.iter().enumerate() {
        assert_eq!(*page, i, "each bookmark goes to its own page");
        let m = doc.pages[i].media;
        assert!(x.abs() < 1e-3 && (y - m[3]).abs() < 1e-3, "the page's top-left corner, not ({x}, {y}) of {m:?}");
    }
}

#[test]
fn a_title_follows_the_label() {
    let d = drawing();
    let marks = sheets(&d, json!({"pageTitles": {"A101": "Level 1", "a102": "Level 2", "A103": "", "A999": "Nowhere"}}));
    assert_eq!(texts(&marks), ["A103", "A101 Level 1", "A102 Level 2"]);
    let marks = sheets(&d, json!({"pageTitles": {"A101": "Étage 1"}}));
    assert_eq!(marks[1].0, "A101 Étage 1");
}

#[test]
fn the_option_turns_them_off_or_on() {
    let d = drawing();
    assert!(sheets(&d, json!({"bookmarks": false})).is_empty());
    let one = plot(&d, &Space::Paper("A101".into()), &json!({"bookmarks": true})).unwrap();
    assert_eq!(texts(&bookmarks(&one)), ["A101"]);
}

#[test]
fn a_one_page_plot_has_none() {
    let d = drawing();
    assert!(bookmarks(&plot(&d, &Space::Paper("A101".into()), &json!({})).unwrap()).is_empty());
    assert!(bookmarks(&plot(&d, &Space::Model, &json!({"layouts": ["A102"]})).unwrap()).is_empty());
}

#[test]
fn a_book_of_model_windows_reads_model() {
    let mut d = drawing();
    d.add(&Space::Model, Common::default(), line()).unwrap();
    let w = json!({"window": [[0, 0], [5, 5]]});
    let bytes = plot(&d, &Space::Model, &json!({"pages": [w, w]})).unwrap();
    assert_eq!(texts(&bookmarks(&bytes)), ["Model", "Model"]);
}

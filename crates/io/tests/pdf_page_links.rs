//! A page link is a link annotation on one page that goes to another page of the same file:
//! its `/Dest` names that page.

use cadcraft_doc::{Common, Drawing, EntityKind, Layout, Line, Space};
use cadcraft_geom::Vec3;
use cadcraft_io::plot;
use craft_pdf::syntax::object::{Array, Dict, Name};
use serde_json::{Value, json};

/// Each page's link annotations: rectangle and the index of the page its destination names
/// (`None` for a link without a page destination).
fn page_links(bytes: &[u8]) -> Vec<Vec<([f64; 4], Option<usize>)>> {
    let pdf = craft_pdf::open_slice(bytes, None).expect("the PDF opens");
    let ids: Vec<_> = pdf.pages().iter().map(|p| p.raw().obj_id()).collect();
    let mut out = Vec::new();
    for page in pdf.pages().iter() {
        let annots = page.raw().get::<Array<'_>>(b"Annots").map(|a| a.iter::<Dict<'_>>().collect::<Vec<_>>()).unwrap_or_default();
        let mut links = Vec::new();
        for a in annots.iter().filter(|a| a.get::<Name<'_>>(b"Subtype").is_some_and(|n| n.as_str() == "Link")) {
            let r: Vec<f64> = a.get::<Array<'_>>(b"Rect").expect("a rectangle").iter::<f64>().collect();
            let dest = a
                .get::<Array<'_>>(b"Dest")
                .or_else(|| a.get::<Dict<'_>>(b"A").and_then(|act| act.get::<Array<'_>>(b"D")))
                .and_then(|d| d.flex_iter().next::<Dict<'_>>())
                .and_then(|p| p.obj_id())
                .and_then(|id| ids.iter().position(|p| *p == Some(id)));
            links.push(([r[0], r[1], r[2], r[3]], dest));
        }
        out.push(links);
    }
    out
}

fn drawing() -> Drawing {
    let mut d = Drawing::new_metric();
    for (i, name) in ["A101", "A301"].into_iter().enumerate() {
        d.layouts.push(Layout::new(name, i as u32 + 10));
        let line = EntityKind::Line(Line { a: Vec3::ZERO, b: Vec3::new(100.0, 100.0, 0.0) });
        d.add(&Space::Paper(name.into()), Common::default(), line).unwrap();
    }
    d
}

fn sheets(links: Value) -> Vec<Vec<([f64; 4], Option<usize>)>> {
    let o = json!({"layouts": ["A101", "A301"], "compress": false, "pageLinks": links});
    page_links(&plot(&drawing(), &Space::Model, &o).unwrap())
}

#[test]
fn a_page_link_goes_to_its_page() {
    let got = sheets(json!([{"page": "A101", "rect": [10, 20, 30, 25], "to": "a301"}]));
    assert_eq!(got[0].len(), 1, "one link on the first page");
    assert_eq!(got[0][0].1, Some(1), "it goes to the second page");
    assert!(got[1].is_empty(), "none on the second page");
    // Paper mm on a 1:1 layout, with 1 pt of slack.
    let k = 72.0 / 25.4;
    let r = got[0][0].0;
    assert!((r[0] - (10.0 * k - 1.0)).abs() < 0.5 && (r[2] - (30.0 * k + 1.0)).abs() < 0.5, "{r:?}");
}

#[test]
fn an_unknown_page_makes_no_link() {
    let got = sheets(json!([
        {"page": "A101", "rect": [10, 20, 30, 25], "to": "A999"},
        {"page": "A999", "rect": [10, 20, 30, 25], "to": "A301"},
        {"page": "A101", "rect": [10, 20, 10, 25], "to": "A301"},
    ]));
    assert!(got.iter().all(Vec::is_empty), "{got:?}");
}

//! The sheet references of a set PDF as page links (LAMB T702): clicking a reference on one sheet
//! goes to the sheet it names. The references are every row of `A001`'s Drawing List, and on each
//! shown view's viewport every `<n>/<layout>` text of the view's annotation block (section cut
//! symbols, elevation markers, enlarged-area boxes) and every detail bubble ([`BUBBLE_BLOCK`]) whose `SHEET` reads
//! `<n>/<layout>`, in the view's own block or its annotation block. A link's area is the
//! reference's extent box, carried through its block's model-space INSERT and the viewport onto
//! the sheet, and clipped to the viewport. The printed page is unchanged.

use cadcraft_doc::{Drawing, Entity, EntityKind, Insert, Viewport, extent_bounds};
use cadcraft_geom::{Bounds2, Mat3, Vec2};
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::bom;
use crate::bubbles::BUBBLE_BLOCK;
use crate::schema::{annotation_block_name, read_document, view_block_name};
use crate::sheets::numbered_viewports;
use crate::sheetset::{LIST_SHEET, LIST_TITLE};

/// The `pageLinks` of a plot of `pages` (layout names): `[{page, rect: [x0, y0, x1, y1], to}]`,
/// rectangles in paper units.
pub(crate) fn page_links(d: &Drawing, pages: &[String]) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let mut push = |page: &str, r: Bounds2, to: &str| {
        if !page.eq_ignore_ascii_case(to) && r.width() > 0.0 && r.height() > 0.0 {
            out.push(json!({ "page": page, "rect": [r.min.x, r.min.y, r.max.x, r.max.y], "to": to }));
        }
    };
    let known = |to: &str| pages.iter().find(|p| p.eq_ignore_ascii_case(to)).cloned();
    if pages.iter().any(|p| p == LIST_SHEET) {
        for (r, to) in list_rows(d) {
            if let Some(to) = known(&to) {
                push(LIST_SHEET, r, &to);
            }
        }
    }
    let doc = read_document(d).map_err(|e| e.to_string())?;
    let blocks: Vec<(Id, String)> = doc.views.iter().map(|v| (v.id(), view_block_name(v.id()))).collect();
    for (view, _, layout, h) in numbered_viewports(d, &blocks) {
        if !pages.contains(&layout) {
            continue;
        }
        let Some(EntityKind::Viewport(vp)) = d.layout(&layout).and_then(|l| l.entities.get(h)).map(|e| &e.kind) else { continue };
        for (r, to) in view_refs(d, view, vp) {
            if let Some(to) = known(&to) {
                push(&layout, r, &to);
            }
        }
    }
    Ok(out)
}

/// The sheet a reference `<n>/<layout>` names.
fn sheet_of(s: &str) -> Option<&str> {
    let (n, layout) = s.trim().split_once('/')?;
    (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && !layout.is_empty()).then_some(layout)
}

/// Each data row of `A001`'s Drawing List: its full-width band (paper units) and the sheet its
/// first cell names. The table hangs down from its insertion point.
fn list_rows(d: &Drawing) -> Vec<(Bounds2, String)> {
    let Some(t) = bom::titled_table(d, LIST_SHEET, LIST_TITLE) else { return Vec::new() };
    let width: f64 = t.col_widths.iter().sum();
    let skip = usize::from(t.title) + usize::from(t.header);
    let mut top = t.insert.y;
    let mut out = Vec::new();
    for (i, h) in t.row_heights.iter().enumerate() {
        let bottom = top - h;
        if i >= skip
            && let Some(sheet) = t.cells.get(i).and_then(|r| r.first()).map(|c| c.text.trim()).filter(|s| !s.is_empty())
        {
            out.push((Bounds2::new(Vec2::new(t.insert.x, bottom), Vec2::new(t.insert.x + width, top)), sheet.to_owned()));
        }
        top = bottom;
    }
    out
}

/// The references `view` shows in viewport `vp`: each one's area on the sheet, clipped to the
/// viewport, and the sheet it names.
fn view_refs(d: &Drawing, view: Id, vp: &Viewport) -> Vec<(Bounds2, String)> {
    let half = Vec2::new(vp.width, vp.height) / 2.0;
    let window = Bounds2::new(vp.center.xy() - half, vp.center.xy() + half);
    let mut out = Vec::new();
    for (name, texts) in [(view_block_name(view), false), (annotation_block_name(view), true)] {
        let (Some(block), Some(ins)) = (d.block(&name), model_insert(d, &name)) else { continue };
        let m = ins.transform(block.base.xy());
        for e in block.entities.iter() {
            let Some(to) = reference(e, texts) else { continue };
            let b = extent_bounds(d, e);
            if b.is_empty() {
                continue;
            }
            let r = on_sheet(&b, &m, vp);
            let r = Bounds2 { min: r.min.max(window.min), max: r.max.min(window.max) };
            if r.min.x < r.max.x && r.min.y < r.max.y {
                out.push((r, to.to_owned()));
            }
        }
    }
    out
}

/// The sheet `e` refers to: a bubble's `SHEET`, or (with `texts`) a TEXT's own value.
fn reference(e: &Entity, texts: bool) -> Option<&str> {
    match &e.kind {
        EntityKind::Insert(i) if i.block == BUBBLE_BLOCK => sheet_of(&i.attribs.iter().find(|a| a.tag.eq_ignore_ascii_case("SHEET"))?.text.value),
        EntityKind::Text(t) if texts => sheet_of(&t.value),
        _ => None,
    }
}

/// The box on the sheet of block-coordinate box `b`, through block transform `m` and viewport `vp`.
fn on_sheet(b: &Bounds2, m: &Mat3, vp: &Viewport) -> Bounds2 {
    let mut r = Bounds2::EMPTY;
    for c in [b.min, Vec2::new(b.max.x, b.min.y), b.max, Vec2::new(b.min.x, b.max.y)] {
        r.add(vp.model_to_paper(m.apply(c)));
    }
    r
}

/// The model-space INSERT of block `name`.
fn model_insert<'a>(d: &'a Drawing, name: &str) -> Option<&'a Insert> {
    d.model.iter().find_map(|e| match &e.kind {
        EntityKind::Insert(i) if i.block == name => Some(i),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheetset::tests::set;
    use cadcraft_doc::Text;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;

    fn links(d: &Drawing, pages: &[String]) -> Vec<(String, Bounds2, String)> {
        page_links(d, pages)
            .unwrap()
            .iter()
            .map(|l| {
                let r: Vec<f64> = l["rect"].as_array().unwrap().iter().filter_map(Value::as_f64).collect();
                let b = Bounds2::new(Vec2::new(r[0], r[1]), Vec2::new(r[2], r[3]));
                (l["page"].as_str().unwrap().to_owned(), b, l["to"].as_str().unwrap().to_owned())
            })
            .collect()
    }

    #[test]
    fn a_reference_is_read_only_as_number_slash_sheet() {
        assert_eq!(sheet_of("3/A501"), Some("A501"));
        assert_eq!(sheet_of(" 12/A301 "), Some("A301"));
        for s in ["A501", "/A501", "3/", "x3/A501", "Section A", ""] {
            assert_eq!(sheet_of(s), None, "{s}");
        }
    }

    #[test]
    fn a_set_links_its_list_rows_cut_symbols_and_bubbles_to_their_sheets() -> R {
        let (mut s, [ground, _, elev, sec]) = set()?;
        s.execute("sheetset", &json!({}))?;
        let d = s.doc()?;
        let pages: Vec<String> = {
            let mut ls: Vec<_> = d.layouts.iter().filter(|l| l.name.len() == 4 && l.name.starts_with('A')).collect();
            ls.sort_by_key(|l| l.tab_order);
            ls.into_iter().map(|l| l.name.clone()).collect()
        };
        let got = links(d, &pages);

        // The Drawing List: one link per data row but its own, each to the sheet it names.
        let list = bom::titled_table(d, LIST_SHEET, LIST_TITLE).ok_or("no list")?;
        let named: Vec<String> = list.cells.iter().skip(2).filter_map(|r| r.first()).map(|c| c.text.clone()).collect();
        let from_list: Vec<String> = got.iter().filter(|(p, ..)| p == LIST_SHEET).map(|(_, _, to)| to.clone()).collect();
        let expect: Vec<String> = named.iter().filter(|n| *n != LIST_SHEET).cloned().collect();
        assert_eq!(from_list, expect);
        assert!(from_list.len() + 1 >= pages.len(), "{from_list:?} of {pages:?}");

        // The plan's sheet: a link to the section's sheet and one to the details' sheet.
        let (_, _, plan_sheet, h) = numbered_viewports(d, &[(ground, view_block_name(ground))]).into_iter().next().ok_or("plan not shown")?;
        let Some(EntityKind::Viewport(vp)) = d.layout(&plan_sheet).and_then(|l| l.entities.get(h)).map(|e| &e.kind) else {
            return Err("no viewport".into());
        };
        let half = Vec2::new(vp.width, vp.height) / 2.0;
        let window = Bounds2::new(vp.center.xy() - half, vp.center.xy() + half);
        let on_plan: Vec<_> = got.iter().filter(|(p, ..)| *p == plan_sheet).collect();
        let sec_sheet = crate::sheets::view_sheet_ref(d, sec).ok_or("section not on a sheet")?.1;
        assert!(on_plan.iter().any(|(.., to)| *to == sec_sheet), "{on_plan:?}");
        assert!(on_plan.iter().any(|(.., to)| to.starts_with("A5")), "{on_plan:?}");
        for (_, r, _) in &on_plan {
            assert!(r.min.x >= window.min.x - 1e-9 && r.max.y <= window.max.y + 1e-9, "{r:?} in {window:?}");
        }
        // The cut symbol's link is around the text's place on the sheet.
        let anno = d.block(&annotation_block_name(ground)).ok_or("no annotation block")?;
        let texts: Vec<&Text> = anno
            .entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Text(t) if sheet_of(&t.value) == Some(sec_sheet.as_str()) => Some(t),
                _ => None,
            })
            .collect();
        assert!(!texts.is_empty());
        for t in texts {
            let at = vp.model_to_paper(t.insert.xy());
            assert!(on_plan.iter().any(|(_, r, to)| *to == sec_sheet && r.expand(1.0).contains(at)), "{at:?} in {on_plan:?}");
        }
        // The elevation marker's sheet text links to the elevation's sheet.
        let elev_sheet = crate::sheets::view_sheet_ref(d, elev).ok_or("elevation not on a sheet")?.1;
        let marker: Vec<&Text> = anno
            .entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Text(t) if sheet_of(&t.value) == Some(elev_sheet.as_str()) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(marker.len(), 1, "one marker text");
        let at = vp.model_to_paper(marker[0].insert.xy());
        assert!(on_plan.iter().any(|(_, r, to)| *to == elev_sheet && r.expand(1.0).contains(at)), "{at:?} in {on_plan:?}");
        // No sheet links to itself, and an unlisted page gets no links.
        assert!(got.iter().all(|(p, _, to)| p != to));
        assert!(links(d, &[LIST_SHEET.to_owned()]).is_empty());
        Ok(())
    }
}

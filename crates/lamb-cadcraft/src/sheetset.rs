//! SHEETSET (LAMB T627): the automatic first try at the whole drawing set. Every placed plan,
//! elevation and section that no sheet shows yet gets a sheet of its own — plans `A101`, `A102`, …
//! by level order (then document order), elevations `A201`, …, sections `A301`, … in document
//! order — and the unplaced assigned details are gathered onto detail sheets `A501`, … six to a
//! sheet, exactly as DETAILSHEETS does ([`crate::detailsheets::lay_detail_sheets`]). Floating
//! plans, unplaced vertical views and untagged details are left alone; a view already on a sheet
//! is skipped and counted. The set's layouts (prefix + two digits) then lead the layout tabs in
//! name order, every other layout after them in its previous order, so PLOT `layouts: "all"` and
//! PUBLISH run in set order. All of it is one undo step; nothing new to place changes nothing.
//!
//! `sheetset {pdf?}` (`sset`) returns `{sheets: [{layout, group, viewIds}], alreadyOnSheets,
//! pdf?, message}`. With `pdf`, every set sheet in tab order is plotted to that one PDF, a page
//! per sheet, whether or not this run placed anything.

use cadcraft_doc::Drawing;
use cadcraft_engine::{Result, Session};
use lamb_core::model::{AnyView, Id, VerticalIntent};
use serde_json::{Value, json};

use crate::commands::{bad, other};
use crate::detailsheets::{lay_detail_sheets, next_sheet_name};
use crate::schema::read_document;
use crate::sheets::{create_sheet, view_sheet_ref};

const PLAN_PREFIX: &str = "A1";
const ELEVATION_PREFIX: &str = "A2";
const SECTION_PREFIX: &str = "A3";
const DETAIL_PREFIX: &str = "A5";
const DETAILS_PER_SHEET: usize = 6;
const SET_PREFIXES: [&str; 4] = [PLAN_PREFIX, ELEVATION_PREFIX, SECTION_PREFIX, DETAIL_PREFIX];

/// Whether `name` is a set sheet name: a set prefix and two digits.
fn is_set_name(name: &str) -> bool {
    SET_PREFIXES.iter().any(|p| name.strip_prefix(p).is_some_and(|rest| rest.len() == 2 && rest.bytes().all(|b| b.is_ascii_digit())))
}

/// The placed plans, elevations and sections, as (group, prefix, [(view, name)]) in set order.
fn set_views(d: &Drawing) -> Result<Vec<(&'static str, &'static str, Vec<(Id, String)>)>> {
    let doc = read_document(d).map_err(other)?;
    let level_order = |level: Id| doc.levels.iter().find(|l| l.id == level).map_or(f64::INFINITY, |l| l.order);
    let mut plans: Vec<(f64, Id, String)> = Vec::new();
    let (mut elevations, mut sections) = (Vec::new(), Vec::new());
    for v in &doc.views {
        match v {
            AnyView::Plan(p) => {
                if let Some(lamb_core::model::Placement::Plan { level_id, .. }) = &p.placement {
                    plans.push((level_order(*level_id), p.id, p.name.clone()));
                }
            }
            AnyView::Vertical(x) if x.placement.is_some() => match x.intent {
                VerticalIntent::Elevation => elevations.push((x.id, x.name.clone())),
                VerticalIntent::Section => sections.push((x.id, x.name.clone())),
            },
            _ => {}
        }
    }
    plans.sort_by(|a, b| a.0.total_cmp(&b.0)); // stable: document order within a level
    Ok(vec![
        ("plan", PLAN_PREFIX, plans.into_iter().map(|(_, id, n)| (id, n)).collect()),
        ("elevation", ELEVATION_PREFIX, elevations),
        ("section", SECTION_PREFIX, sections),
    ])
}

/// Renumbers the layout tabs: set sheets first by name, every other layout after them in its
/// previous relative order.
fn order_tabs(d: &mut Drawing) {
    let base = d.layouts.iter().map(|l| l.tab_order).min().unwrap_or(0);
    let mut order: Vec<(bool, String, u32, usize)> =
        d.layouts.iter().enumerate().map(|(i, l)| (!is_set_name(&l.name), l.name.clone(), l.tab_order, i)).collect();
    order.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| if a.0 { a.2.cmp(&b.2) } else { a.1.cmp(&b.1) }));
    for (k, (_, _, _, i)) in order.into_iter().enumerate() {
        if let Some(l) = d.layouts.get_mut(i) {
            l.tab_order = base + k as u32;
        }
    }
}

/// The set sheets in tab order.
fn set_sheets(d: &Drawing) -> Vec<String> {
    let mut ls: Vec<_> = d.layouts.iter().filter(|l| is_set_name(&l.name)).collect();
    ls.sort_by_key(|l| l.tab_order);
    ls.into_iter().map(|l| l.name.clone()).collect()
}

pub(crate) fn sheetset(s: &mut Session, p: &Value) -> Result<Value> {
    let pdf = match p.get("pdf").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(v.as_str().filter(|x| !x.trim().is_empty()).ok_or_else(|| bad("sheetset", "`pdf` must be a file path"))?.to_owned()),
    };
    // Build every sheet on a copy and assign it back once: one undo step.
    let mut d = s.doc()?.clone();
    let mut sheets: Vec<Value> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    let mut already = 0usize;
    for (group, prefix, views) in set_views(&d)? {
        for (view, name) in views {
            if view_sheet_ref(&d, view).is_some() {
                already += 1;
                continue;
            }
            let layout = next_sheet_name(&d, prefix);
            create_sheet(&mut d, &layout, &[view], &[]).map_err(other)?;
            parts.push(format!("{layout} {name}"));
            sheets.push(json!({ "layout": layout, "group": group, "viewIds": [view.0] }));
        }
    }
    let (laid, placed_details) = lay_detail_sheets(&mut d, DETAIL_PREFIX, DETAILS_PER_SHEET).map_err(other)?;
    already += placed_details;
    for (layout, _, ids) in &laid {
        let n = ids.len();
        parts.push(format!("{layout} ({n} detail{})", if n == 1 { "" } else { "s" }));
        sheets.push(json!({ "layout": layout, "group": "details", "viewIds": ids.iter().map(|id| id.0).collect::<Vec<_>>() }));
    }
    if !sheets.is_empty() {
        order_tabs(&mut d);
        *s.doc_mut()? = d;
    }

    let mut message = if parts.is_empty() { "Sheet set: nothing new to lay out".to_owned() } else { format!("Sheet set: {}", parts.join(", ")) };
    if already > 0 {
        message.push_str(&format!("; {already} already on sheets"));
    }
    message.push('.');
    let mut out = json!({ "sheets": sheets, "alreadyOnSheets": already, "message": message });
    if let Some(path) = pdf {
        let names = set_sheets(s.doc()?);
        if names.is_empty() {
            return Err(bad("sheetset", "There are no set sheets to plot"));
        }
        let r = s.execute("plot", &json!({ "layouts": names, "path": path }))?;
        let pages = r["pages"].as_u64().unwrap_or_default();
        out["message"] = json!(format!("{message} Plotted {pages} sheet{} to {path}.", if pages == 1 { "" } else { "s" }));
        out["pdf"] = r;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{view_block_name, write_document};
    use cadcraft_doc::EntityKind;
    use cadcraft_engine::cmd::file::{IoHooks, set_io};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    fn id(r: &Value) -> T<Id> {
        Ok(Id(r["viewId"].as_u64().ok_or("no id")?))
    }

    /// A project: Ground (lambproject) and First (offset) plans, but First first in document
    /// order; an elevation and a section on Ground; a 5000 × 4000 room on Ground with D1 and D2
    /// tagged and D3 untagged; a floating plan. Returns (session, [ground, first, elev, sec]).
    fn set() -> T<(Session, [Id; 4])> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let ground = id(&s.execute("lambproject", &json!({}))?)?;
        let first = id(&s.execute("newoffsetplan", &json!({ "mode": "offset", "fromViewId": ground.0, "floorToFloorMm": 3000, "name": "First" }))?)?;
        let elev = id(&s.execute(
            "newelevationline",
            &json!({ "name": "North", "hostViewId": ground.0, "cutFrom": {"x": 0, "y": 6000}, "cutTo": {"x": 5000, "y": 6000} }),
        )?)?;
        let sec = id(&s.execute(
            "newsectionline",
            &json!({ "name": "Section A", "hostViewId": ground.0, "cutFrom": {"x": 0, "y": 2000}, "cutTo": {"x": 5000, "y": 2000} }),
        )?)?;
        s.execute("newplan", &json!({ "name": "Loose" }))?;
        let mut ds = Vec::new();
        for k in 1..=3 {
            ds.push(id(&s.execute("newdetail", &json!({ "name": format!("D{k}") }))?)?);
        }
        s.execute("editview", &json!({ "viewId": ground.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [5000, 0], [5000, 4000], [0, 4000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [4800, 200], [4800, 3800], [200, 3800]], "closed": true }))?;
        s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
        for (d, tip) in [(ds[0], (2500.0, 0.0)), (ds[1], (5000.0, 2000.0))] {
            s.execute(
                "detailtag",
                &json!({ "detailViewId": d.0, "tip": {"x": tip.0, "y": tip.1}, "bubble": {"x": tip.0 - 1000.0, "y": tip.1 - 1000.0} }),
            )?;
        }
        s.execute("editview", &json!({}))?;
        // First ahead of Ground in document order: level order decides.
        let d = s.doc_mut()?;
        let mut doc = read_document(d)?;
        let i = doc.views.iter().position(|v| v.id() == first).ok_or("no first")?;
        let v = doc.views.remove(i);
        doc.views.insert(0, v);
        write_document(d, &doc)?;
        Ok((s, [ground, first, elev, sec]))
    }

    fn laid(r: &Value) -> Vec<(String, String, Vec<u64>)> {
        r["sheets"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| {
                        let ids = x["viewIds"].as_array().map(|v| v.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
                        (x["layout"].as_str().unwrap_or_default().to_owned(), x["group"].as_str().unwrap_or_default().to_owned(), ids)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn tabs(s: &Session) -> T<Vec<String>> {
        let mut ls: Vec<_> = s.doc()?.layouts.iter().collect();
        ls.sort_by_key(|l| l.tab_order);
        Ok(ls.into_iter().map(|l| l.name.clone()).collect())
    }

    fn undo_len(s: &Session) -> T<usize> {
        Ok(s.state()?.undo.len())
    }

    #[test]
    fn lays_out_the_set_in_one_undo_step_and_reruns_add_only_new_views() -> R {
        let (mut s, [ground, first, elev, sec]) = set()?;
        let details = crate::detailsheets::assigned_details(s.doc()?);
        assert_eq!(details.len(), 2);
        let before = tabs(&s)?;
        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        let want = [
            ("A101".to_owned(), "plan".to_owned(), vec![ground.0]),
            ("A102".to_owned(), "plan".to_owned(), vec![first.0]),
            ("A201".to_owned(), "elevation".to_owned(), vec![elev.0]),
            ("A301".to_owned(), "section".to_owned(), vec![sec.0]),
            ("A501".to_owned(), "details".to_owned(), details.iter().map(|d| d.0).collect()),
        ];
        assert_eq!(laid(&r), want, "{r}");
        assert_eq!(r["message"], "Sheet set: A101 Ground Floor, A102 First, A201 North, A301 Section A, A501 (2 details).");
        assert_eq!(view_sheet_ref(s.doc()?, ground), Some((1, "A101".to_owned())));
        let mut want_tabs: Vec<String> = ["A101", "A102", "A201", "A301", "A501"].map(String::from).to_vec();
        want_tabs.extend(before);
        assert_eq!(tabs(&s)?, want_tabs, "set sheets lead, other layouts follow in order");

        // A re-run changes nothing.
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "no undo step");
        assert_eq!(r["message"], "Sheet set: nothing new to lay out; 6 already on sheets.");

        // A third plan, placed later (the floating one): only A103, and the tabs stay in set order.
        s.execute("newsheet", &json!({ "name": "Notes", "viewIds": [ground.0] }))?;
        let third = read_document(s.doc()?)?.views.iter().find(|v| v.name() == "Loose").map(|v| v.id()).ok_or("no Loose")?;
        let level = s.execute("level", &json!({ "mode": "create", "name": "Roof", "floorElevationMm": 6000, "order": 2 }))?["levelId"]
            .as_u64()
            .ok_or("no id")?;
        let t = json!({ "originX": 0.0, "originY": 0.0, "rotation": 0.0 });
        s.execute("placeview", &json!({ "viewId": third.0, "kind": "plan", "levelId": level, "cutElevation": 7200, "transform": t }))?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(laid(&r), [("A103".to_owned(), "plan".to_owned(), vec![third.0])], "{r}");
        let t = tabs(&s)?;
        assert_eq!(t[..6], ["A101", "A102", "A103", "A201", "A301", "A501"], "{t:?}");
        assert_eq!(t.last().map(String::as_str), Some("Notes"), "a non-set layout follows");

        // Undo takes the whole run back.
        s.undo()?;
        assert!(s.doc()?.layout("A103").is_none());
        assert!(s.doc()?.layout("A102").is_some());
        Ok(())
    }

    /// The `SHEET` attribute of every bubble in `view`'s block.
    fn bubble_sheets(s: &Session, view: Id) -> T<Vec<String>> {
        let b = s.doc()?.block(&view_block_name(view)).ok_or("no block")?;
        Ok(b.entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Insert(i) => i.attribs.iter().find(|a| a.tag == "SHEET").map(|a| a.text.value.clone()),
                _ => None,
            })
            .collect())
    }

    /// The frozen layers of layout `name`'s viewports.
    fn frozen(s: &Session, name: &str) -> T<Vec<String>> {
        let l = s.doc()?.layout(name).ok_or("no layout")?;
        Ok(l.entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Viewport(vp) if vp.id > 1 => Some(vp.frozen_layers.clone()),
                _ => None,
            })
            .flatten()
            .collect())
    }

    #[test]
    fn a_plan_made_after_the_set_gets_its_own_sheet_and_stays_out_of_the_others() -> R {
        let (mut s, [ground, ..]) = set()?;
        s.execute("sheetset", &json!({}))?;
        let bubbles = bubble_sheets(&s, ground)?;
        assert_eq!(bubbles.len(), 2);
        assert!(bubbles.iter().all(|b| b.ends_with("/A501")), "{bubbles:?}");
        let held = frozen(&s, "A101")?;

        let n = undo_len(&s)?;
        let later = id(&s.execute("newoffsetplan", &json!({ "mode": "offset", "fromViewId": ground.0, "floorToFloorMm": 6000, "name": "Later" }))?)?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        let layer = view_block_name(later);
        for sheet in ["A101", "A102", "A201", "A301", "A501"] {
            assert!(frozen(&s, sheet)?.contains(&layer), "{sheet}");
        }
        assert_eq!(view_sheet_ref(s.doc()?, ground), Some((1, "A101".to_owned())));
        assert_eq!(bubble_sheets(&s, ground)?, bubbles);

        // The undo takes the frozen layer with the view.
        s.undo()?;
        assert_eq!(frozen(&s, "A101")?, held);
        s.redo()?;
        assert!(frozen(&s, "A101")?.contains(&layer));

        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(laid(&r), [("A103".to_owned(), "plan".to_owned(), vec![later.0])], "{r}");
        assert_eq!(view_sheet_ref(s.doc()?, ground), Some((1, "A101".to_owned())));
        assert_eq!(view_sheet_ref(s.doc()?, later), Some((1, "A103".to_owned())));
        assert_eq!(bubble_sheets(&s, ground)?, bubbles);
        assert!(frozen(&s, "A101")?.contains(&layer));
        Ok(())
    }

    #[test]
    fn a_view_already_on_a_sheet_is_skipped_and_counted() -> R {
        let (mut s, [_, _, elev, _]) = set()?;
        s.execute("newsheet", &json!({ "name": "Mine", "viewIds": [elev.0] }))?;
        let r = s.execute("sheetset", &json!({}))?;
        let names: Vec<String> = laid(&r).into_iter().map(|(l, _, _)| l).collect();
        assert_eq!(names, ["A101", "A102", "A301", "A501"], "{r}");
        assert_eq!(r["alreadyOnSheets"], 1);
        assert!(r["message"].as_str().unwrap_or_default().ends_with("; 1 already on sheets."), "{r}");
        assert_eq!(view_sheet_ref(s.doc()?, elev), Some((1, "Mine".to_owned())));
        Ok(())
    }

    #[test]
    fn the_pdf_has_a_page_per_set_sheet() -> R {
        set_io(IoHooks {
            read: |b, n| cadcraft_io::read(b, n).map_err(|e| e.to_string()),
            write: |d, n| cadcraft_io::write(d, n).map_err(|e| e.to_string()),
            plot: Some(|d, space, opts| cadcraft_io::plotter::plot(d, space, opts).map_err(|e| e.to_string())),
        });
        let (mut s, _) = set()?;
        let dir = std::env::temp_dir().join(format!("lamb-sheetset-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("set.pdf");
        let path_s = path.to_string_lossy().into_owned();
        let r = s.execute("sheetset", &json!({ "pdf": path_s }))?;
        assert_eq!(r["pdf"]["pages"], 5, "{r}");
        let bytes = std::fs::read(&path)?;
        let pdf = craft_pdf::open_slice(&bytes, None).map_err(|e| format!("the PDF opens: {e:?}"))?;
        assert_eq!(pdf.pages().len(), 5);
        // A re-run with nothing new still plots the set.
        let r = s.execute("sheetset", &json!({ "pdf": path_s }))?;
        assert_eq!(r["pdf"]["pages"], 5, "{r}");
        let _ = std::fs::remove_dir_all(&dir);

        // A drawing with no set sheets has nothing to plot.
        let mut empty = Session::new();
        empty.new_drawing(true);
        let e = empty.execute("sheetset", &json!({ "pdf": path_s })).err().ok_or("plotted")?;
        assert!(e.to_string().contains("There are no set sheets to plot"), "{e}");
        Ok(())
    }

    #[test]
    fn set_names_are_a_prefix_and_two_digits() {
        assert!(is_set_name("A101") && is_set_name("A599") && is_set_name("A301"));
        assert!(!is_set_name("A401") && !is_set_name("A1") && !is_set_name("A1001") && !is_set_name("Layout1") && !is_set_name("A1x1"));
    }
}

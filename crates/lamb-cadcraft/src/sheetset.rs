//! SHEETSET (LAMB T627): the automatic first try at the whole drawing set. Every placed plan,
//! elevation and section that no sheet shows yet gets a sheet of its own — plans `A101`, `A102`, …
//! by level order (then document order), elevations `A201`, …, sections `A301`, … in document
//! order — and the unplaced assigned details are gathered onto detail sheets `A501`, … six to a
//! sheet, exactly as DETAILSHEETS does ([`crate::detailsheets::lay_detail_sheets`]). Plan sheets
//! show the outline only: the walls as their details build them go on framing sheets `A401`, …
//! (LAMB T635), one per placed view with an assembly block, plans in level order and then
//! verticals in document order, each showing only that block. SHEETSET does not generate
//! assemblies, so no block means no framing sheet. Floating plans, unplaced vertical views and
//! untagged details are left alone; a view already on a sheet, or an assembly already in a
//! viewport, is skipped and counted. The set's layouts (prefix + two digits) then lead the layout tabs in
//! name order, every other layout after them in its previous order, so PLOT `layouts: "all"` and
//! PUBLISH run in set order. All of it is one undo step; nothing new to place changes nothing.
//!
//! `sheetset {pdf?}` (`sset`) returns `{sheets: [{layout, group, viewIds}], alreadyOnSheets,
//! pdf?, message}`. With `pdf`, every set sheet in tab order is plotted to that one PDF, a page
//! per sheet, whether or not this run placed anything.
//!
//! SHEETSIZE (LAMB T633): `sheetsize {paper}` (`ssize`) sets the one project setting new sheets
//! take their paper from ([`crate::sheets::sheet_page`]), in one undo step, and returns
//! `{paper, widthMm, heightMm, message}` with the landscape sheet size. Existing sheets are never
//! resized or re-flowed.

use cadcraft_doc::{Drawing, EntityKind};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use lamb_core::model::{AnyView, Id, VerticalIntent};
use serde_json::{Value, json};

use crate::commands::{bad, name_input, other};
use crate::detailsheets::{lay_detail_sheets, next_sheet_name};
use crate::schema::{assembly_block_name, read_document, read_entity_data, write_document};
use crate::sheets::{DEFAULT_SHEET_PAPER, create_sheet, sheet_paper_size, view_sheet_ref};

const PLAN_PREFIX: &str = "A1";
const ELEVATION_PREFIX: &str = "A2";
const SECTION_PREFIX: &str = "A3";
const FRAMING_PREFIX: &str = "A4";
const DETAIL_PREFIX: &str = "A5";
const FRAMING_GROUP: &str = "framing";
const DETAILS_PER_SHEET: usize = 6;
const SET_PREFIXES: [&str; 5] = [PLAN_PREFIX, ELEVATION_PREFIX, SECTION_PREFIX, FRAMING_PREFIX, DETAIL_PREFIX];

/// Whether `name` is a set sheet name: a set prefix and two digits.
fn is_set_name(name: &str) -> bool {
    SET_PREFIXES.iter().any(|p| name.strip_prefix(p).is_some_and(|rest| rest.len() == 2 && rest.bytes().all(|b| b.is_ascii_digit())))
}

/// The placed plans, elevations and sections, then the placed views with an assembly block (plans
/// in level order, then verticals in document order), as (group, prefix, [(view, name)]) in set
/// order.
fn set_views(d: &Drawing) -> Result<Vec<(&'static str, &'static str, Vec<(Id, String)>)>> {
    let doc = read_document(d).map_err(other)?;
    let level_order = |level: Id| doc.levels.iter().find(|l| l.id == level).map_or(f64::INFINITY, |l| l.order);
    let mut plans: Vec<(f64, Id, String)> = Vec::new();
    let (mut elevations, mut sections, mut verticals) = (Vec::new(), Vec::new(), Vec::new());
    for v in &doc.views {
        match v {
            AnyView::Plan(p) => {
                if let Some(lamb_core::model::Placement::Plan { level_id, .. }) = &p.placement {
                    plans.push((level_order(*level_id), p.id, p.name.clone()));
                }
            }
            AnyView::Vertical(x) if x.placement.is_some() => {
                verticals.push((x.id, x.name.clone()));
                match x.intent {
                    VerticalIntent::Elevation => elevations.push((x.id, x.name.clone())),
                    VerticalIntent::Section => sections.push((x.id, x.name.clone())),
                }
            }
            _ => {}
        }
    }
    plans.sort_by(|a, b| a.0.total_cmp(&b.0)); // stable: document order within a level
    let plans: Vec<(Id, String)> = plans.into_iter().map(|(_, id, n)| (id, n)).collect();
    let framing = plans.iter().chain(&verticals).filter(|(id, _)| d.block(&assembly_block_name(*id)).is_some()).cloned().collect();
    Ok(vec![
        ("plan", PLAN_PREFIX, plans),
        ("elevation", ELEVATION_PREFIX, elevations),
        ("section", SECTION_PREFIX, sections),
        (FRAMING_GROUP, FRAMING_PREFIX, framing),
    ])
}

/// Whether some viewport on some layout shows `view`'s assembly block.
fn assembly_on_sheet(d: &Drawing, view: Id) -> bool {
    d.layouts
        .iter()
        .flat_map(|l| l.entities.iter())
        .any(|e| matches!(e.kind, EntityKind::Viewport(_)) && read_entity_data(&e.common).is_ok_and(|data| data.assembly_of == Some(view)))
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
        let framing = group == FRAMING_GROUP;
        for (view, name) in views {
            let on_sheet = if framing { assembly_on_sheet(&d, view) } else { view_sheet_ref(&d, view).is_some() };
            if on_sheet {
                already += 1;
                continue;
            }
            let layout = next_sheet_name(&d, prefix);
            if framing {
                create_sheet(&mut d, &layout, &[], &[view]).map_err(other)?;
                parts.push(format!("{layout} {name} framing"));
            } else {
                create_sheet(&mut d, &layout, &[view], &[]).map_err(other)?;
                parts.push(format!("{layout} {name}"));
            }
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

pub(crate) fn sheetsize(s: &mut Session, p: &Value) -> Result<Value> {
    let name = p.get("paper").and_then(Value::as_str).ok_or_else(|| bad("sheetsize", "`paper` must be a paper size name"))?;
    let paper = cadcraft_render::paper_size(name)
        .ok_or_else(|| bad("sheetsize", &format!("Unknown sheet size \"{name}\"; try ARCH D, ARCH C, ANSI D, ANSI B or A1")))?;
    let mut doc = read_document(s.doc()?).map_err(other)?;
    if sheet_paper_size(&doc).map(|c| c.name) != Some(paper.name) {
        doc.sheet_paper = Some(paper.name.to_owned());
        write_document(s.doc_mut()?, &doc).map_err(other)?;
    }
    let (w, h) = (paper.width_mm.max(paper.height_mm), paper.width_mm.min(paper.height_mm));
    let message = format!("New sheets are {}; existing sheets keep their size.", short_name(paper));
    Ok(json!({ "paper": paper.name, "widthMm": w, "heightMm": h, "message": message }))
}

/// `paper`'s short name (`ARCH D`), as typed at the prompt.
fn short_name(paper: &cadcraft_render::PaperSize) -> &'static str {
    paper.aliases.first().copied().unwrap_or(paper.name)
}

/// SHEETSIZE: `Enter sheet size <ARCH D>:`, the current setting as default.
struct SheetSize {
    current: String,
}

impl Interactive for SheetSize {
    fn name(&self) -> &'static str {
        "sheetsize"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter sheet size", Accept::TEXT).default(self.current.clone())
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(paper) = name_input(i, &self.current) else { return Ok(Step::Continue) };
        match sheetsize(s, &json!({ "paper": paper })) {
            Ok(r) => s.echo(r["message"].as_str().unwrap_or_default().to_owned()),
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

pub(crate) fn sheetsize_machine(s: &Session) -> Result<Box<dyn Interactive>> {
    let doc = read_document(s.doc()?).map_err(other)?;
    let current = sheet_paper_size(&doc).map_or(DEFAULT_SHEET_PAPER, short_name).to_owned();
    Ok(Box::new(SheetSize { current }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::view_block_name;
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
            ("A401".to_owned(), "framing".to_owned(), vec![ground.0]),
            ("A501".to_owned(), "details".to_owned(), details.iter().map(|d| d.0).collect()),
        ];
        assert_eq!(laid(&r), want, "{r}");
        assert_eq!(
            r["message"],
            "Sheet set: A101 Ground Floor, A102 First, A201 North, A301 Section A, A401 Ground Floor framing, A501 (2 details)."
        );
        assert_eq!(view_sheet_ref(s.doc()?, ground), Some((1, "A101".to_owned())));
        let mut want_tabs: Vec<String> = ["A101", "A102", "A201", "A301", "A401", "A501"].map(String::from).to_vec();
        want_tabs.extend(before);
        assert_eq!(tabs(&s)?, want_tabs, "set sheets lead, other layouts follow in order");

        // A re-run changes nothing.
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "no undo step");
        assert_eq!(r["message"], "Sheet set: nothing new to lay out; 7 already on sheets.");

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
        assert_eq!(t[..7], ["A101", "A102", "A103", "A201", "A301", "A401", "A501"], "{t:?}");
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
        for sheet in ["A101", "A102", "A201", "A301", "A401", "A501"] {
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

    /// The one viewport on layout `name`, as (viewport, its assembly_of).
    fn only_viewport(s: &Session, name: &str) -> T<(cadcraft_doc::Viewport, Option<Id>)> {
        let l = s.doc()?.layout(name).ok_or("no layout")?;
        let mut vps = l.entities.iter().filter_map(|e| match &e.kind {
            EntityKind::Viewport(vp) if vp.id > 1 => Some((vp.clone(), read_entity_data(&e.common).ok()?.assembly_of)),
            _ => None,
        });
        let first = vps.next().ok_or("no viewport")?;
        assert!(vps.next().is_none(), "one viewport on {name}");
        Ok(first)
    }

    /// Layout `name`'s drawing titles, as (NUM, NAME, SCALE).
    fn sheet_titles(s: &Session, name: &str) -> T<Vec<(String, String, String)>> {
        let l = s.doc()?.layout(name).ok_or("no layout")?;
        Ok(l.entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Insert(i) if i.block == crate::vptitles::TITLE_BLOCK => {
                    let v = |t: &str| i.attribs.iter().find(|a| a.tag == t).map_or_else(String::new, |a| a.text.value.clone());
                    Some((v("NUM"), v("NAME"), v("SCALE")))
                }
                _ => None,
            })
            .collect())
    }

    #[test]
    fn a_framing_sheet_shows_the_views_assembly_at_its_scale_with_a_title() -> R {
        let (mut s, [ground, first, ..]) = set()?;
        assert!(s.doc()?.block(&assembly_block_name(first)).is_none(), "First's details are untagged");
        let r = s.execute("sheetset", &json!({}))?;
        let framing: Vec<_> = laid(&r).into_iter().filter(|(_, g, _)| g == "framing").collect();
        assert_eq!(framing, [("A401".to_owned(), "framing".to_owned(), vec![ground.0])], "First has no assembly, so no sheet: {r}");

        let (vp, of) = only_viewport(&s, "A401")?;
        assert_eq!(of, Some(ground));
        let shown = assembly_block_name(ground);
        assert!(!vp.frozen_layers.contains(&shown), "its own block shows");
        assert!(vp.frozen_layers.contains(&view_block_name(ground)), "the outline does not");
        let (plan_vp, _) = only_viewport(&s, "A101")?;
        assert!(plan_vp.frozen_layers.contains(&shown), "the plan sheet stays outline-only");
        let scale = crate::sheets::viewport_scale(&vp);
        assert_eq!(scale, crate::sheets::viewport_scale(&plan_vp), "the plan's scale");
        assert_eq!(sheet_titles(&s, "A401")?, [("1".to_owned(), "Ground Floor framing".to_owned(), crate::vptitles::scale_label(scale))]);
        Ok(())
    }

    #[test]
    fn framing_sheets_run_plans_by_level_then_verticals_by_document_order() -> R {
        let (mut s, [ground, first, elev, sec]) = set()?;
        for v in [sec, elev, first] {
            s.execute("assemblyplan", &json!({ "viewId": v.0 }))?;
        }
        let r = s.execute("sheetset", &json!({}))?;
        let framing: Vec<_> = laid(&r).into_iter().filter(|(_, g, _)| g == "framing").map(|(l, _, ids)| (l, ids)).collect();
        let want = [("A401", ground), ("A402", first), ("A403", elev), ("A404", sec)].map(|(l, v)| (l.to_owned(), vec![v.0]));
        assert_eq!(framing, want, "{r}");
        assert_eq!(only_viewport(&s, "A404")?.1, Some(sec));
        assert_eq!(sheet_titles(&s, "A404")?.first().map(|t| t.1.as_str()), Some("Section A framing"));
        Ok(())
    }

    #[test]
    fn an_assembly_made_later_adds_one_framing_sheet_and_moves_no_reference() -> R {
        let (mut s, [ground, first, elev, sec]) = set()?;
        s.execute("sheetset", &json!({}))?;
        let refs = |s: &Session| -> T<Vec<_>> {
            Ok([ground, first, elev, sec].iter().map(|v| s.doc().map(|d| view_sheet_ref(d, *v))).collect::<std::result::Result<Vec<_>, _>>()?)
        };
        let (before_refs, before_bubbles) = (refs(&s)?, bubble_sheets(&s, ground)?);
        assert_eq!(before_refs[0], Some((1, "A101".to_owned())), "the framing sheet takes no reference");
        assert!(before_bubbles.len() == 2 && before_bubbles.iter().all(|b| b.ends_with("/A501")), "{before_bubbles:?}");

        // A section's assembly gets the next number.
        s.execute("assemblyplan", &json!({ "viewId": sec.0 }))?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(laid(&r), [("A402".to_owned(), "framing".to_owned(), vec![sec.0])], "{r}");
        assert_eq!(r["message"], "Sheet set: A402 Section A framing; 7 already on sheets.");

        // A re-run adds nothing and no undo step.
        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n, "no undo step");
        assert_eq!(r["message"], "Sheet set: nothing new to lay out; 8 already on sheets.");

        // A second plan's assembly adds exactly one, and the tabs stay in set order.
        s.execute("assemblyplan", &json!({ "viewId": first.0 }))?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(laid(&r), [("A403".to_owned(), "framing".to_owned(), vec![first.0])], "{r}");
        let t = tabs(&s)?;
        assert_eq!(t[..8], ["A101", "A102", "A201", "A301", "A401", "A402", "A403", "A501"], "{t:?}");

        assert_eq!(refs(&s)?, before_refs);
        assert_eq!(bubble_sheets(&s, ground)?, before_bubbles);
        Ok(())
    }

    #[test]
    fn an_assembly_already_in_a_viewport_gets_no_framing_sheet() -> R {
        let (mut s, [ground, ..]) = set()?;
        s.execute("newsheet", &json!({ "name": "Mine", "assemblyIds": [ground.0] }))?;
        let r = s.execute("sheetset", &json!({}))?;
        assert!(laid(&r).iter().all(|(_, g, _)| g != "framing"), "{r}");
        assert_eq!(r["alreadyOnSheets"], 1, "{r}");
        assert!(s.doc()?.layout("A401").is_none());
        Ok(())
    }

    #[test]
    fn a_view_already_on_a_sheet_is_skipped_and_counted() -> R {
        let (mut s, [_, _, elev, _]) = set()?;
        s.execute("newsheet", &json!({ "name": "Mine", "viewIds": [elev.0] }))?;
        let r = s.execute("sheetset", &json!({}))?;
        let names: Vec<String> = laid(&r).into_iter().map(|(l, _, _)| l).collect();
        assert_eq!(names, ["A101", "A102", "A301", "A401", "A501"], "{r}");
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
        assert_eq!(r["pdf"]["pages"], 6, "{r}");
        let bytes = std::fs::read(&path)?;
        let pdf = craft_pdf::open_slice(&bytes, None).map_err(|e| format!("the PDF opens: {e:?}"))?;
        assert_eq!(pdf.pages().len(), 6, "the framing sheet is a page");
        for page in pdf.pages().iter() {
            let m = page.media_box();
            let size = ((m.x1 - m.x0).abs() / 72.0 * 25.4, (m.y1 - m.y0).abs() / 72.0 * 25.4);
            assert!((size.0 - ARCH_D.0).abs() < 0.1 && (size.1 - ARCH_D.1).abs() < 0.1, "{size:?}");
        }
        // A re-run with nothing new still plots the set.
        let r = s.execute("sheetset", &json!({ "pdf": path_s }))?;
        assert_eq!(r["pdf"]["pages"], 6, "{r}");
        let _ = std::fs::remove_dir_all(&dir);

        // A drawing with no set sheets has nothing to plot.
        let mut empty = Session::new();
        empty.new_drawing(true);
        let e = empty.execute("sheetset", &json!({ "pdf": path_s })).err().ok_or("plotted")?;
        assert!(e.to_string().contains("There are no set sheets to plot"), "{e}");
        Ok(())
    }

    fn sheet_size(s: &Session, layout: &str) -> T<(f64, f64)> {
        let sheet = cadcraft_render::sheet(s.doc()?, layout).ok_or("no sheet")?;
        Ok((sheet.size.x, sheet.size.y))
    }

    fn near(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6
    }

    const ARCH_D: (f64, f64) = (914.4, 609.6);
    const ANSI_B: (f64, f64) = (431.8, 279.4);

    #[test]
    fn a_fresh_projects_set_is_arch_d_landscape_with_the_plan_at_1_50() -> R {
        let (mut s, _) = set()?;
        s.execute("sheetset", &json!({}))?;
        for sheet in ["A101", "A102", "A201", "A301", "A501"] {
            assert!(near(sheet_size(&s, sheet)?, ARCH_D), "{sheet}: {:?}", sheet_size(&s, sheet)?);
        }
        let layout = s.doc()?.layout("A101").ok_or("no A101")?;
        assert_eq!(layout.page.paper, "ARCH D (24.00 x 36.00 Inches)");
        assert!(layout.page.landscape && layout.page.paper_mm);
        assert_eq!(layout.page.margins_mm, [10.0; 4]);
        let scale = layout.entities.iter().find_map(|e| match &e.kind {
            EntityKind::Viewport(vp) if vp.id > 1 => Some(crate::sheets::viewport_scale(vp)),
            _ => None,
        });
        assert_eq!(scale, Some(50.0));
        Ok(())
    }

    #[test]
    fn sheetsize_sets_new_sheets_only_in_one_undo_step_and_refuses_an_unknown_name() -> R {
        let (mut s, [ground, ..]) = set()?;
        s.execute("newsheet", &json!({ "name": "A101", "viewIds": [ground.0] }))?;
        let n = undo_len(&s)?;
        let r = s.execute("sheetsize", &json!({ "paper": "ansi b" }))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!(r["paper"], "ANSI B (11.00 x 17.00 Inches)");
        assert!(near((r["widthMm"].as_f64().ok_or("no width")?, r["heightMm"].as_f64().ok_or("no height")?), ANSI_B), "{r}");
        assert_eq!(r["message"], "New sheets are ANSI B; existing sheets keep their size.");
        assert_eq!(read_document(s.doc()?)?.sheet_paper.as_deref(), Some("ANSI B (11.00 x 17.00 Inches)"));

        s.execute("newsheet", &json!({ "name": "B1", "viewIds": [ground.0] }))?;
        assert!(near(sheet_size(&s, "B1")?, ANSI_B), "{:?}", sheet_size(&s, "B1")?);
        assert!(near(sheet_size(&s, "A101")?, ARCH_D), "an existing sheet keeps its size");
        s.undo()?;

        // Setting the same paper again changes nothing.
        s.execute("sheetsize", &json!({ "paper": "ANSI B" }))?;
        assert_eq!(undo_len(&s)?, n + 1, "no undo step");

        let e = s.execute("sheetsize", &json!({ "paper": "nope" })).err().ok_or("accepted")?;
        assert!(e.to_string().contains("nope") && e.to_string().contains("ARCH D"), "{e}");
        assert_eq!(read_document(s.doc()?)?.sheet_paper.as_deref(), Some("ANSI B (11.00 x 17.00 Inches)"));
        assert_eq!(undo_len(&s)?, n + 1, "a refusal is no undo step");

        s.undo()?;
        assert_eq!(read_document(s.doc()?)?.sheet_paper, None);
        assert!(near(sheet_size(&s, "A101")?, ARCH_D));
        Ok(())
    }

    #[test]
    fn the_setting_survives_a_dxf_round_trip() -> R {
        let (mut s, _) = set()?;
        s.execute("sheetsize", &json!({ "paper": "ANSI B" }))?;
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert_eq!(read_document(&back)?.sheet_paper.as_deref(), Some("ANSI B (11.00 x 17.00 Inches)"));
        Ok(())
    }

    #[test]
    fn sheetsize_prompts_with_the_current_setting() -> R {
        let (mut s, _) = set()?;
        s.start("ssize")?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("ARCH D"));
        s.input(Input::Text("ANSI B".into()))?;
        assert!(s.log.iter().any(|l| l == "New sheets are ANSI B; existing sheets keep their size."), "{:?}", s.log);
        s.start("sheetsize")?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("ANSI B"));
        let n = undo_len(&s)?;
        s.input(Input::Enter)?;
        assert_eq!(undo_len(&s)?, n, "Enter keeps the setting");
        s.start("sheetsize")?;
        s.input(Input::Text("nope".into()))?;
        assert!(s.log.iter().any(|l| l.contains("Unknown sheet size")), "{:?}", s.log);
        assert_eq!(read_document(s.doc()?)?.sheet_paper.as_deref(), Some("ANSI B (11.00 x 17.00 Inches)"));
        Ok(())
    }

    #[test]
    fn set_names_are_a_prefix_and_two_digits() {
        assert!(is_set_name("A101") && is_set_name("A599") && is_set_name("A301") && is_set_name("A401"));
        assert!(!is_set_name("A601") && !is_set_name("A1") && !is_set_name("A1001") && !is_set_name("Layout1") && !is_set_name("A1x1"));
    }
}

//! DETAILSHEETS (LAMB T600): every detail view actually assigned to a wall — named by a resolved
//! `Tagged`, `Spread` or `Gap` edge, corner, opening or opening-end detail in any plan or section
//! view — that no sheet shows yet, laid out on new numbered detail sheets (`A501`, `A502`, … for
//! prefix `A5`), `perSheet` per sheet (default 6, the 3 × 2 grid of [`create_sheet`]) in document
//! view order. Each sheet's viewport order numbers its details 1..n, so every tag bubble's `SHEET`
//! attribute fills in (`3/A501`) in the same undo step. Details already on a sheet keep their
//! number; existing sheets are never re-flowed. Each detail plots at the smallest standard scale
//! that is no larger than 1:5 and fits its viewport ([`crate::sheets::STANDARD_SCALES`]).
//!
//! `detailsheets {prefix?, perSheet?}` (`dsh`) returns
//! `{sheets: [{layout, viewports: [hex], detailIds: [n]}], alreadyOnSheets, message}`; nothing to
//! place is `Ok` with no change and no undo step. Interactively `Enter detail sheet prefix <A5>:`.

use cadcraft_doc::{Drawing, Handle};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use lamb_core::derivation::detail_tags::RunDetail;
use lamb_core::model::{AnyView, Id};
use serde_json::{Value, json};

use crate::commands::{bad, name_input, other};
use crate::detailtags::{resolve_view_details, resolve_view_opening_ends, resolve_view_openings};
use crate::schema::read_document;
use crate::sheets::{SheetError, create_sheet, view_sheet_ref};

const DEFAULT_PREFIX: &str = "A5";
const DEFAULT_PER_SHEET: usize = 6;

/// The detail view a resolved detail names, if it is an assignment.
fn named(r: &RunDetail) -> Option<Id> {
    match r {
        RunDetail::Tagged(s) | RunDetail::Spread(s) | RunDetail::Gap(s) => s.parse().ok().map(Id),
        RunDetail::Conflict | RunDetail::Untyped => None,
    }
}

/// Every detail view assigned to a wall in some plan or section view, in document view order. A
/// view whose resolution fails is skipped.
pub fn assigned_details(d: &Drawing) -> Vec<Id> {
    let Ok(doc) = read_document(d) else { return Vec::new() };
    let mut used: Vec<Id> = Vec::new();
    for v in doc.views.iter().filter(|v| matches!(v, AnyView::Plan(_) | AnyView::Vertical(_))) {
        let view = v.id();
        let (Ok((runs, junctions)), Ok(openings), Ok(ends)) =
            (resolve_view_details(d, view), resolve_view_openings(d, view), resolve_view_opening_ends(d, view))
        else {
            continue;
        };
        let all = runs.iter().chain(&junctions).chain(&openings).chain(ends.iter().flatten());
        used.extend(all.filter_map(named));
    }
    doc.views.iter().filter(|v| matches!(v, AnyView::Detail(_))).map(|v| v.id()).filter(|id| used.contains(id)).collect()
}

/// `{prefix}{k:02}` for the first k ≥ 1 with no layout of that name.
pub(crate) fn next_sheet_name(d: &Drawing, prefix: &str) -> String {
    (1..).map(|k| format!("{prefix}{k:02}")).find(|n| d.layout(n).is_none()).unwrap_or_default()
}

/// One detail sheet laid out: (layout name, viewport handles, detail view ids).
pub(crate) type DetailSheet = (String, Vec<Handle>, Vec<Id>);

/// Lays every assigned detail that no sheet shows yet onto new sheets `{prefix}nn`, `per` to a
/// sheet, in document view order. Returns the sheets laid out and how many assigned details were
/// already on sheets.
pub(crate) fn lay_detail_sheets(d: &mut Drawing, prefix: &str, per: usize) -> std::result::Result<(Vec<DetailSheet>, usize), SheetError> {
    let assigned = assigned_details(d);
    let (placed, todo): (Vec<Id>, Vec<Id>) = assigned.iter().partition(|id| view_sheet_ref(d, **id).is_some());
    let mut sheets = Vec::new();
    for chunk in todo.chunks(per.max(1)) {
        let name = next_sheet_name(d, prefix);
        let handles = create_sheet(d, &name, chunk, &[])?;
        sheets.push((name, handles, chunk.to_vec()));
    }
    Ok((sheets, placed.len()))
}

pub(crate) fn detailsheets(s: &mut Session, p: &Value) -> Result<Value> {
    let prefix = match p.get("prefix").filter(|v| !v.is_null()) {
        None => DEFAULT_PREFIX.to_owned(),
        Some(v) => v.as_str().ok_or_else(|| bad("detailsheets", "`prefix` must be a string"))?.trim().to_owned(),
    };
    let per = match p.get("perSheet").filter(|v| !v.is_null()) {
        None => DEFAULT_PER_SHEET,
        Some(v) => match v.as_u64() {
            Some(n) if n >= 1 => n as usize,
            _ => return Err(bad("detailsheets", "`perSheet` must be a whole number of at least 1")),
        },
    };
    // Build every sheet on a copy and assign it back once: one undo step.
    let mut d = s.doc()?.clone();
    let (laid, placed) = lay_detail_sheets(&mut d, &prefix, per).map_err(other)?;
    if laid.is_empty() {
        let message = if placed == 0 {
            "No details are assigned to walls.".to_owned()
        } else {
            format!("Every assigned detail is already on a sheet ({placed}).")
        };
        return Ok(json!({ "sheets": [], "alreadyOnSheets": placed, "message": message }));
    }
    *s.doc_mut()? = d;
    let parts: Vec<String> = laid.iter().map(|(name, _, ids)| format!("{name} ({})", ids.len())).collect();
    let sheets: Vec<Value> = laid
        .iter()
        .map(|(name, handles, ids)| {
            json!({
                "layout": name,
                "viewports": handles.iter().map(|h| format!("{:X}", h.0)).collect::<Vec<_>>(),
                "detailIds": ids.iter().map(|id| id.0).collect::<Vec<_>>(),
            })
        })
        .collect();
    let mut message = format!("Detail sheets: {}", parts.join(", "));
    if placed > 0 {
        message.push_str(&format!("; {placed} already on sheets"));
    }
    message.push('.');
    Ok(json!({ "sheets": sheets, "alreadyOnSheets": placed, "message": message }))
}

/// DETAILSHEETS: `Enter detail sheet prefix <A5>:`.
struct DetailSheets;

impl Interactive for DetailSheets {
    fn name(&self) -> &'static str {
        "detailsheets"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter detail sheet prefix", Accept::TEXT).default(DEFAULT_PREFIX.to_owned())
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(prefix) = name_input(i, DEFAULT_PREFIX) else { return Ok(Step::Continue) };
        match detailsheets(s, &json!({ "prefix": prefix })) {
            Ok(r) => s.echo(r["message"].as_str().unwrap_or_default().to_owned()),
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

pub(crate) fn detailsheets_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(DetailSheets))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bubbles::BUBBLE_BLOCK;
    use crate::schema::view_block_name;
    use cadcraft_doc::{EntityKind, Insert};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    /// bubbles.rs's room — a 5000 × 4000 room of 200 mm walls, editing the plan on layer `TAGS` —
    /// with details D1, D2, D3. Returns (session, plan, [D1, D2, D3]).
    fn room() -> T<(Session, Id, [Id; 3])> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let mut ds = [Id(0); 3];
        for (k, d) in ds.iter_mut().enumerate() {
            *d = Id(s.execute("newdetail", &json!({ "name": format!("D{}", k + 1) }))?["viewId"].as_u64().ok_or("no id")?);
        }
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [5000, 0], [5000, 4000], [0, 4000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [4800, 200], [4800, 3800], [200, 3800]], "closed": true }))?;
        s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
        Ok((s, plan, ds))
    }

    fn tag(s: &mut Session, detail: Id, tip: (f64, f64)) -> T<()> {
        s.execute(
            "detailtag",
            &json!({ "detailViewId": detail.0, "tip": {"x": tip.0, "y": tip.1}, "bubble": {"x": tip.0 - 1000.0, "y": tip.1 - 1000.0} }),
        )?;
        Ok(())
    }

    fn undo_len(s: &Session) -> T<usize> {
        Ok(s.state()?.undo.len())
    }

    /// Every bubble's `SHEET` attribute in `view`'s block.
    fn sheet_attrs(s: &Session, view: Id) -> T<Vec<String>> {
        let b = s.doc()?.block(&view_block_name(view)).ok_or("no block")?;
        let attr = |i: &Insert| i.attribs.iter().find(|a| a.tag == "SHEET").map(|a| a.text.value.clone()).unwrap_or_default();
        Ok(b.entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Insert(i) if i.block == BUBBLE_BLOCK => Some(attr(i)),
                _ => None,
            })
            .collect())
    }

    fn details_on(r: &Value) -> Vec<(String, Vec<u64>)> {
        r["sheets"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| {
                        let ids = x["detailIds"].as_array().map(|ids| ids.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
                        (x["layout"].as_str().unwrap_or_default().to_owned(), ids)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn assigned_details_get_one_sheet_each_in_one_undo_step_and_reruns_add_only_new_ones() -> R {
        let (mut s, plan, [d1, d2, d3]) = room()?;
        tag(&mut s, d1, (2500.0, 0.0))?;
        tag(&mut s, d2, (5000.0, 2000.0))?;
        assert_eq!(assigned_details(s.doc()?), [d1, d2]);

        let n = undo_len(&s)?;
        let r = s.execute("detailsheets", &json!({ "perSheet": 1 }))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!(details_on(&r), [("A501".to_owned(), vec![d1.0]), ("A502".to_owned(), vec![d2.0])], "{r}");
        assert_eq!(r["message"], "Detail sheets: A501 (1), A502 (1).");
        assert_eq!(view_sheet_ref(s.doc()?, d3), None, "D3 is on no sheet");
        let mut attrs = sheet_attrs(&s, plan)?;
        attrs.sort();
        assert_eq!(attrs, ["1/A501", "1/A502"]);

        // A second run changes nothing and records no undo step.
        let r = s.execute("detailsheets", &json!({ "perSheet": 1 }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        assert_eq!(r["message"], "Every assigned detail is already on a sheet (2).");

        // Tag D3: only it goes on the next sheet.
        tag(&mut s, d3, (2500.0, 4000.0))?;
        let r = s.execute("detailsheets", &json!({ "perSheet": 1 }))?;
        assert_eq!(details_on(&r), [("A503".to_owned(), vec![d3.0])], "{r}");
        assert_eq!(r["message"], "Detail sheets: A503 (1); 2 already on sheets.");

        // Undo removes the whole step.
        s.undo()?;
        assert!(s.doc()?.layout("A503").is_none());
        assert!(s.doc()?.layout("A502").is_some());
        Ok(())
    }

    #[test]
    fn the_default_packs_details_on_one_sheet_numbered_in_order() -> R {
        let (mut s, plan, [d1, d2, _]) = room()?;
        tag(&mut s, d2, (5000.0, 2000.0))?;
        tag(&mut s, d1, (2500.0, 0.0))?;
        let r = s.execute("detailsheets", &json!({}))?;
        assert_eq!(details_on(&r), [("A501".to_owned(), vec![d1.0, d2.0])], "document order, not tag order");
        assert_eq!(view_sheet_ref(s.doc()?, d1), Some((1, "A501".to_owned())));
        assert_eq!(view_sheet_ref(s.doc()?, d2), Some((2, "A501".to_owned())));
        let mut attrs = sheet_attrs(&s, plan)?;
        attrs.sort();
        assert_eq!(attrs, ["1/A501", "2/A501"]);
        Ok(())
    }

    #[test]
    fn nothing_assigned_changes_nothing_and_bad_per_sheet_is_refused() -> R {
        let (mut s, _plan, _) = room()?;
        let n = undo_len(&s)?;
        let r = s.execute("detailsheets", &json!({}))?;
        assert_eq!(r["message"], "No details are assigned to walls.");
        assert_eq!(undo_len(&s)?, n);
        assert!(s.execute("detailsheets", &json!({ "perSheet": 0 })).is_err());
        Ok(())
    }

    #[test]
    fn a_detail_tagged_only_in_a_section_is_gathered() -> R {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let sd = Id(s.execute("newdetail", &json!({ "name": "SD", "cutDirection": "vertical" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let r =
            s.execute("newsectionline", &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 0}, "cutTo": {"x": 5000, "y": 0} }))?;
        let sec = Id(r["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [0, 3000]] }))?;
        s.execute("pline", &json!({ "vertices": [[300, 0], [300, 3000]] }))?;
        let block = s.doc()?.block(&view_block_name(sec)).ok_or("no block")?;
        let hs: Vec<String> = block.entities.iter().map(|e| format!("{:X}", e.handle.0)).collect();
        s.execute("setrole", &json!({ "handles": hs, "role": "cut" }))?;
        s.execute("layer.new", &json!({ "name": "TAGS", "current": true }))?;
        tag(&mut s, sd, (150.0, 2000.0))?;
        assert_eq!(assigned_details(s.doc()?), [sd]);
        let r = s.execute("detailsheets", &json!({}))?;
        assert_eq!(details_on(&r), [("A501".to_owned(), vec![sd.0])], "{r}");
        Ok(())
    }

    #[test]
    fn a_small_detail_plots_at_one_to_five_never_larger() -> R {
        let (mut s, plan, [d1, ..]) = room()?;
        s.execute("editview", &json!({ "viewId": d1.0 }))?;
        s.execute("line", &json!({ "points": [[0, 0], [100, 50]] }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        tag(&mut s, d1, (2500.0, 0.0))?;
        s.execute("detailsheets", &json!({}))?;
        let l = s.doc()?.layout("A501").ok_or("no sheet")?;
        let scales: Vec<f64> = l
            .entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Viewport(vp) if vp.id > 1 => Some(crate::sheets::viewport_scale(vp)),
                _ => None,
            })
            .collect();
        assert_eq!(scales, [5.0], "a 100 mm detail would fit at 1:1");
        Ok(())
    }

    #[test]
    fn interactive_prefix_defaults_to_a5() -> R {
        let (mut s, _plan, [d1, ..]) = room()?;
        tag(&mut s, d1, (2500.0, 0.0))?;
        s.start("detailsheets")?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("A5"));
        s.input(Input::Text("S".into()))?;
        assert!(s.doc()?.layout("S01").is_some());
        assert!(s.log.iter().any(|l| l == "Detail sheets: S01 (1)."), "{:?}", s.log);
        Ok(())
    }
}

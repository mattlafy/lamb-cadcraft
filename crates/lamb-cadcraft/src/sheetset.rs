//! SHEETSET (LAMB T627): the automatic first try at the whole drawing set. Every placed plan that
//! no sheet shows yet gets a sheet of its own — `A101`, `A102`, … by level order, then document
//! order. The drawn elevations and sections no sheet shows yet share new sheets, elevations `A201`,
//! … and sections `A301`, …, packed in rows in document order ([`crate::sheets::pack_verticals`],
//! LAMB T647), so each is numbered on its sheet in that order; an undrawn one has nothing to size a
//! viewport from and gets a sheet of its own after them. A rerun's new views go on a new sheet and
//! existing sheets are left alone. The unplaced assigned details are packed onto detail sheets
//! `A501`, …, exactly as DETAILSHEETS does ([`crate::detailsheets::lay_detail_sheets`]). Plan sheets
//! show the outline only: the walls as their details build them go on framing sheets `A401`, …
//! (LAMB T635), one per placed view with an assembly block, plans in level order and then
//! verticals in document order, each showing only that block. SHEETSET does not generate
//! assemblies, so no block means no framing sheet. Floating plans, unplaced vertical views and
//! untagged details are left alone; a view already on a sheet, or an assembly already in a
//! viewport, is skipped and counted. The set's layouts (prefix + two digits) then lead the layout tabs in
//! name order, every other layout after them in its previous order, so PLOT `layouts: "all"` and
//! PUBLISH run in set order.
//!
//! The set opens on a drawing list and ends on the BOM (LAMB T636), both rewritten by this command
//! alone, so a sheet renamed by hand shows at the next run. `A001` holds a `Drawing List` table
//! (`Sheet | Title`) of every set sheet in tab order, itself included, titled by
//! [`crate::titleblock::sheet_title`]; it is made once another set sheet exists. `A601` holds the
//! [`crate::bom`] table when there is framing to count; with none an existing one is left alone.
//! `A602` holds the [`crate::schedule`] opening schedule (LAMB T639) likewise, when a placed plan has
//! openings.
//! Each table is compared with what it would become and replaced, at its own corner, only when its
//! text differs. A table taller than the sheet is written whole and the message says it runs off.
//! All of it is one undo step; nothing to change changes nothing.
//!
//! A viewport on a set sheet whose content has outgrown it is refit (LAMB T643,
//! [`crate::sheets::refit_viewport`]): re-centred, or taken to a larger scale when that is not
//! enough, in the same undo step and before the tables and the PDF. Its size and place on the sheet
//! stay, so a detail on a packed sheet shrinks rather than re-packing the sheet. A viewport naming
//! a view no longer placed or no longer in the project stays as it is and the message names it.
//!
//! `sheetset {pdf?}` (`sset`) returns `{sheets: [{layout, group, viewIds}], alreadyOnSheets,
//! refit: [{layout, view, scale}], weighted, pdf?, message}`, `weighted` counting the lines this run
//! stamped with a lineweight and `group` being `list`, `bom` or `schedule` for a table made
//! or updated, with no `viewIds`.
//! With `pdf`, every set sheet in tab order is plotted to that one PDF, a page and a bookmark
//! per sheet, whether or not this run placed anything, and `proposedLines` counts the unaccepted
//! AI lines in the views those sheets show: a plot ignores their ghost transparency, so the
//! message says they plot as drawn. Nothing is hidden. The session's observers run on a changed
//! drawing before it plots, so the PDF has the titles and annotation for the scales it shows.
//!
//! Older sheets come along for plotting (LAMB T645), in the same undo step: a viewport naming a
//! view or an assembly that is still on layer `0` moves to [`VPORT_LAYER`], and a set sheet with
//! no plot style table takes monochrome. A table a user chose stays, a foreign viewport stays.
//!
//! The sheets plot in black, so weight is what separates a cut wall from what lies beyond (LAMB
//! T646). In the same undo step, each entity in the view block of a placed plan or vertical takes a
//! lineweight from its resolved role: `Cut` 0.50 mm, `Annotation` 0.18 mm, `Datum` and `Reference`
//! 0.13 mm, and `Seen` and anything else none, which plots at the default 0.25 mm. The weight is
//! stamped on the entity, since one layer holds different roles in different views, and stays
//! visible in DXF, DWG and Properties. Only an entity that is ByLayer on a layer whose lineweight is
//! `Default` takes one (layer `0` plots through the view's own layer, so that layer decides): a
//! weight chosen on the entity or the layer stands. Details, assembly blocks, generated annotation
//! and paper space are not stamped. A stamp is not revisited: an entity whose role changes later
//! keeps its weight until it is set back to ByLayer.
//!
//! SHEETSIZE (LAMB T633): `sheetsize {paper}` (`ssize`) sets the one project setting new sheets
//! take their paper from ([`crate::sheets::sheet_page`]), in one undo step, and returns
//! `{paper, widthMm, heightMm, message}` with the landscape sheet size. Existing sheets are never
//! resized or re-flowed.

use cadcraft_doc::{Drawing, EntityKind, Handle, Lineweight, MONOCHROME_CTB, Space};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::model::entities::LineRole;
use lamb_core::model::{AnyView, Id, VerticalIntent};
use serde_json::{Value, json};

use crate::bom::{self, BOM_TITLE, Refreshed};
use crate::commands::{bad, name_input, other};
use crate::detailsheets::{lay_detail_sheets, next_sheet_name};
use crate::project::DrawingProject;
use crate::schedule::{self, OPENING_TITLE, refresh_opening_schedule};
use crate::schema::{assembly_block_name, read_document, read_entity_data, view_block_name, write_document};
use crate::sheets::{
    DEFAULT_SHEET_PAPER, VPORT_LAYER, create_packed_sheet, create_sheet, ensure_vport_layer, pack_verticals, refit_viewport, sheet_paper_size,
    view_drawn, view_placed, view_sheet_ref, viewport_scale,
};
use crate::titleblock::sheet_title;
use crate::vptitles::scale_label;

const PLAN_PREFIX: &str = "A1";
const ELEVATION_PREFIX: &str = "A2";
const SECTION_PREFIX: &str = "A3";
const FRAMING_PREFIX: &str = "A4";
const DETAIL_PREFIX: &str = "A5";
const ELEVATION_GROUP: &str = "elevation";
const SECTION_GROUP: &str = "section";
const FRAMING_GROUP: &str = "framing";
const LIST_PREFIX: &str = "A0";
const BOM_PREFIX: &str = "A6";
const SET_PREFIXES: [&str; 7] = [LIST_PREFIX, PLAN_PREFIX, ELEVATION_PREFIX, SECTION_PREFIX, FRAMING_PREFIX, DETAIL_PREFIX, BOM_PREFIX];
const LIST_SHEET: &str = "A001";
const LIST_TITLE: &str = "Drawing List";
const LIST_COL_WIDTHS: [f64; 2] = [30.0, 150.0];

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
        (ELEVATION_GROUP, ELEVATION_PREFIX, elevations),
        (SECTION_GROUP, SECTION_PREFIX, sections),
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

/// Brings `A001`'s list up to the set sheets, once there is another to list. Sorts the tabs first,
/// since the list follows them.
fn refresh_list(d: &mut Drawing) -> Result<Option<Refreshed>> {
    if set_sheets(d).iter().all(|n| n == LIST_SHEET) {
        return Ok(None);
    }
    bom::ensure_sheet_layout(d, LIST_SHEET)?;
    order_tabs(d);
    let rows: Vec<Vec<String>> = set_titles(d)?.into_iter().map(|(n, t)| vec![n, t]).collect();
    bom::refresh_table(d, LIST_SHEET, LIST_TITLE, &|at| bom::schedule_table(LIST_TITLE, &["Sheet", "Title"], &LIST_COL_WIDTHS, &rows, at))
}

/// Every set sheet in tab order, with its title.
fn set_titles(d: &Drawing) -> Result<Vec<(String, String)>> {
    let doc = read_document(d).map_err(other)?;
    // The list's own row names the table being written, which a new sheet does not hold yet.
    let title = |n: &str| if n == LIST_SHEET { LIST_TITLE.to_owned() } else { sheet_title(d, &doc, n) };
    Ok(set_sheets(d)
        .into_iter()
        .map(|n| {
            let t = title(&n);
            (n, t)
        })
        .collect())
}

/// The plot's `pageTitles`: each set sheet with a title of its own (a sheet with nothing to name
/// it is titled with its own name, which its bookmark already reads).
fn page_titles(d: &Drawing) -> Result<Value> {
    let titled = set_titles(d)?.into_iter().filter(|(n, t)| n != t).map(|(n, t)| (n, Value::String(t)));
    Ok(Value::Object(titled.collect()))
}

/// A table's part of the message and its `sheets` entry.
fn table_report(layout: &str, group: &str, title: &str, change: Refreshed) -> (String, Value) {
    let part = match change {
        Refreshed::Made => format!("{layout} {title}"),
        Refreshed::Updated => format!("{layout} {title} updated"),
    };
    (part, json!({ "layout": layout, "group": group, "viewIds": [] }))
}

/// Brings sheets made before they plotted as drawings along: LAMB viewports on layer `0` move to
/// [`VPORT_LAYER`] and set sheets without a plot style table take monochrome. Returns how many
/// sheets changed.
fn update_for_plotting(d: &mut Drawing) -> Result<usize> {
    let mut moved = Vec::new();
    let mut updated: Vec<String> = Vec::new();
    for layout in &d.layouts {
        let on_zero = d.space(&Space::Paper(layout.name.clone())).into_iter().flat_map(|store| store.iter()).filter(|e| {
            matches!(e.kind, EntityKind::Viewport(_))
                && e.common.layer == "0"
                && read_entity_data(&e.common).is_ok_and(|x| x.view_id.is_some() || x.assembly_of.is_some())
        });
        let before = moved.len();
        moved.extend(on_zero.map(|e| e.handle));
        if moved.len() > before {
            updated.push(layout.name.clone());
        }
    }
    if !moved.is_empty() {
        ensure_vport_layer(d);
    }
    for h in moved {
        d.modify_entity(h, |e| e.common.layer = VPORT_LAYER.to_owned()).map_err(other)?;
    }
    for layout in d.layouts.iter_mut().filter(|l| is_set_name(&l.name) && l.page.plot_style_table.is_empty()) {
        layout.page.plot_style_table = MONOCHROME_CTB.to_owned();
        if !updated.contains(&layout.name) {
            updated.push(layout.name.clone());
        }
    }
    Ok(updated.len())
}

/// The lineweight a view line plots at for its role; `None` keeps the default.
fn role_lineweight(role: LineRole) -> Option<Lineweight> {
    match role {
        LineRole::Cut => Some(Lineweight::Mm100(50)),
        LineRole::Annotation => Some(Lineweight::Mm100(18)),
        LineRole::Datum | LineRole::Reference => Some(Lineweight::Mm100(13)),
        _ => None,
    }
}

/// Stamps each entity in the view block of a placed plan or vertical with its role's lineweight,
/// unless the entity or its layer has a weight chosen. Returns how many were stamped.
fn stamp_lineweights(d: &mut Drawing) -> Result<usize> {
    let mut stamps: Vec<(Handle, Lineweight)> = Vec::new();
    {
        let project = DrawingProject::new(d).map_err(other)?;
        for (group, _, views) in set_views(d)? {
            if group == FRAMING_GROUP {
                continue; // The same views again, for their assembly blocks.
            }
            for (view, _) in views {
                let block = view_block_name(view);
                for e in project.entities(view) {
                    let Some(weight) = role_lineweight(project.role_of(&e)) else { continue };
                    let handle = Handle(e.id.0);
                    // Layer 0 inside a block plots through the layer of the block's reference.
                    let layer = if e.layer_id == "0" { block.as_str() } else { e.layer_id };
                    if d.entity(handle).is_some_and(|x| x.common.lineweight == Lineweight::ByLayer)
                        && d.layer(layer).is_none_or(|l| l.lineweight == Lineweight::Default)
                    {
                        stamps.push((handle, weight));
                    }
                }
            }
        }
    }
    for (handle, weight) in &stamps {
        d.modify_entity(*handle, |e| e.common.lineweight = *weight).map_err(other)?;
    }
    Ok(stamps.len())
}

/// The unaccepted AI lines in the views the set sheets show.
fn proposed_lines(d: &Drawing) -> Result<usize> {
    let mut views: Vec<Id> = Vec::new();
    for name in set_sheets(d) {
        for e in d.space(&Space::Paper(name)).into_iter().flat_map(|store| store.iter()) {
            if matches!(e.kind, EntityKind::Viewport(_))
                && let Some(v) = read_entity_data(&e.common).ok().and_then(|x| x.view_id)
                && !views.contains(&v)
            {
                views.push(v);
            }
        }
    }
    let project = DrawingProject::new(d).map_err(other)?;
    Ok(views.into_iter().map(|v| project.entities(v).filter(|e| e.ai_origin.is_some_and(|o| !o.accepted)).count()).sum())
}

/// Refits every viewport on a set sheet that has outgrown its window, in sheet name order. Returns
/// the `refit` report, each refit as `A101 now 1:100`, and each viewport left alone because its view
/// is not placed as `A103 shows <name>, which is not placed`.
fn refit_viewports(d: &mut Drawing) -> Result<(Vec<Value>, Vec<String>, Vec<String>)> {
    let doc = read_document(d).map_err(other)?;
    let mut layouts: Vec<_> = d.layouts.iter().filter(|l| is_set_name(&l.name)).collect();
    layouts.sort_by(|a, b| a.name.cmp(&b.name));
    let mut changes = Vec::new();
    let mut unplaced = Vec::new();
    for l in layouts {
        for e in l.entities.iter() {
            if !matches!(e.kind, EntityKind::Viewport(_)) {
                continue;
            }
            let data = read_entity_data(&e.common).map_err(other)?;
            let Some(view) = data.assembly_of.or(data.view_id) else { continue };
            if !view_placed(&doc, view) {
                let name = doc.views.iter().find(|v| v.id() == view).map_or_else(|| format!("view {}", view.0), |v| v.name().to_owned());
                unplaced.push(format!("{} shows {name}, which is not placed", l.name));
            } else if let Some((center, height)) = refit_viewport(d, &doc, e.handle).map_err(other)? {
                changes.push((l.name.clone(), view, e.handle, center, height));
            }
        }
    }
    let (mut refit, mut labels) = (Vec::new(), Vec::new());
    for (layout, view, handle, center, height) in changes {
        let mut scale = 0.0;
        d.modify_entity(handle, |e| {
            if let EntityKind::Viewport(vp) = &mut e.kind {
                vp.view_center = center;
                vp.view_height = height;
                scale = viewport_scale(vp);
            }
        })
        .map_err(other)?;
        labels.push(format!("{layout} now {}", scale_label(scale)));
        refit.push(json!({ "layout": layout, "view": view.0, "scale": scale }));
    }
    Ok((refit, labels, unplaced))
}

/// Lays every set view no sheet shows yet: a sheet each, but for the drawn elevations and sections,
/// which share packed sheets. Adds each new sheet to `sheets` and `parts` and returns how many views
/// were already on sheets.
fn lay_views(d: &mut Drawing, sheets: &mut Vec<Value>, parts: &mut Vec<String>) -> Result<usize> {
    let mut already = 0usize;
    for (group, prefix, mut views) in set_views(d)? {
        let framing = group == FRAMING_GROUP;
        if group == ELEVATION_GROUP || group == SECTION_GROUP {
            let (placed, todo): (Vec<_>, Vec<_>) = views.into_iter().partition(|(v, _)| view_sheet_ref(d, *v).is_some());
            already += placed.len();
            let (drawn, undrawn): (Vec<_>, Vec<_>) = todo.into_iter().partition(|(v, _)| view_drawn(d, *v));
            let ids: Vec<Id> = drawn.iter().map(|(v, _)| *v).collect();
            for packed in pack_verticals(d, &ids).map_err(other)? {
                let layout = next_sheet_name(d, prefix);
                create_packed_sheet(d, &layout, &packed).map_err(other)?;
                let shown: Vec<Id> = packed.iter().map(|(v, _)| *v).collect();
                parts.push(match shown.as_slice() {
                    [one] => format!("{layout} {}", drawn.iter().find(|(v, _)| v == one).map_or("", |(_, name)| name.as_str())),
                    many => format!("{layout} ({} views)", many.len()),
                });
                sheets.push(json!({ "layout": layout, "group": group, "viewIds": shown.iter().map(|v| v.0).collect::<Vec<_>>() }));
            }
            views = undrawn;
        }
        for (view, name) in views {
            let on_sheet = if framing { assembly_on_sheet(d, view) } else { view_sheet_ref(d, view).is_some() };
            if on_sheet {
                already += 1;
                continue;
            }
            let layout = next_sheet_name(d, prefix);
            if framing {
                create_sheet(d, &layout, &[], &[view]).map_err(other)?;
                parts.push(format!("{layout} {name} framing"));
            } else {
                create_sheet(d, &layout, &[view], &[]).map_err(other)?;
                parts.push(format!("{layout} {name}"));
            }
            sheets.push(json!({ "layout": layout, "group": group, "viewIds": [view.0] }));
        }
    }
    Ok(already)
}

pub(crate) fn sheetset(s: &mut Session, p: &Value) -> Result<Value> {
    let pdf = match p.get("pdf").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(v.as_str().filter(|x| !x.trim().is_empty()).ok_or_else(|| bad("sheetset", "`pdf` must be a file path"))?.to_owned()),
    };
    // Build every sheet on a copy and assign it back once: one undo step.
    let mut d = s.doc()?.clone();
    let updated = update_for_plotting(&mut d)?;
    let weighted = stamp_lineweights(&mut d)?;
    let mut sheets: Vec<Value> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    let mut already = lay_views(&mut d, &mut sheets, &mut parts)?;
    let (laid, placed_details) = lay_detail_sheets(&mut d, DETAIL_PREFIX, None).map_err(other)?;
    already += placed_details;
    for (layout, _, ids) in &laid {
        let n = ids.len();
        parts.push(format!("{layout} ({n} detail{})", if n == 1 { "" } else { "s" }));
        sheets.push(json!({ "layout": layout, "group": "details", "viewIds": ids.iter().map(|id| id.0).collect::<Vec<_>>() }));
    }
    let (refit, refit_labels, unplaced) = refit_viewports(&mut d)?;
    // The BOM's row in the list needs the BOM, so the BOM goes first; set order in what is reported.
    if let Some(change) = bom::refresh_bom(&mut d)? {
        let (part, sheet) = table_report(bom::DEFAULT_LAYOUT, "bom", BOM_TITLE, change);
        parts.push(part);
        sheets.push(sheet);
    }
    if let Some(change) = refresh_opening_schedule(&mut d)? {
        let (part, sheet) = table_report(schedule::DEFAULT_LAYOUT, "schedule", OPENING_TITLE, change);
        parts.push(part);
        sheets.push(sheet);
    }
    if let Some(change) = refresh_list(&mut d)? {
        let (part, sheet) = table_report(LIST_SHEET, "list", LIST_TITLE, change);
        parts.insert(0, part);
        sheets.insert(0, sheet);
    }
    let mut off_sheet: Vec<String> = Vec::new();
    for (layout, title) in [(LIST_SHEET, LIST_TITLE), (bom::DEFAULT_LAYOUT, BOM_TITLE), (schedule::DEFAULT_LAYOUT, OPENING_TITLE)] {
        if bom::table_runs_off(&d, layout, title)? {
            off_sheet.push(format!("{layout} {title}"));
        }
    }
    let before = s.state()?.doc.clone();
    let changed = updated > 0 || weighted > 0 || !sheets.is_empty() || !refit.is_empty();
    if changed {
        order_tabs(&mut d);
        *s.doc_mut()? = d;
    }

    let mut message = if parts.is_empty() { "Sheet set: nothing new to lay out".to_owned() } else { format!("Sheet set: {}", parts.join(", ")) };
    if already > 0 {
        message.push_str(&format!("; {already} already on sheets"));
    }
    if updated > 0 {
        message.push_str(&format!("; {updated} sheet{} updated for plotting", if updated == 1 { "" } else { "s" }));
    }
    if weighted > 0 {
        message.push_str(&format!("; {weighted} line{} weighted by role", if weighted == 1 { "" } else { "s" }));
    }
    if !refit.is_empty() {
        let n = refit.len();
        message.push_str(&format!("; {n} viewport{} refit ({})", if n == 1 { "" } else { "s" }, refit_labels.join(", ")));
    }
    for note in &unplaced {
        message.push_str(&format!("; {note}"));
    }
    if !off_sheet.is_empty() {
        message.push_str(&format!("; {} runs off the sheet", off_sheet.join(" and ")));
    }
    message.push('.');
    let mut out = json!({ "sheets": sheets, "alreadyOnSheets": already, "refit": refit, "weighted": weighted, "message": message });
    if let Some(path) = pdf {
        let names = set_sheets(s.doc()?);
        if names.is_empty() {
            return Err(bad("sheetset", "There are no set sheets to plot"));
        }
        // This command's own observers run after it returns, which is too late for the PDF.
        if changed {
            for f in s.observers.clone() {
                f(s, &before);
            }
        }
        let titles = page_titles(s.doc()?)?;
        let r = s.execute("plot", &json!({ "layouts": names, "pageTitles": titles, "path": path }))?;
        let pages = r["pages"].as_u64().unwrap_or_default();
        let proposed = proposed_lines(s.doc()?)?;
        let warning = match proposed {
            0 => String::new(),
            1 => "; 1 proposed AI line plots as drawn: accept or erase it".to_owned(),
            n => format!("; {n} proposed AI lines plot as drawn: accept or erase them"),
        };
        out["message"] = json!(format!("{message} Plotted {pages} sheet{} to {path}{warning}.", if pages == 1 { "" } else { "s" }));
        out["proposedLines"] = json!(proposed);
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
    use crate::bom::tests::{new_handle, stud_room};
    use crate::schema::annotation_block_name;
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
            ("A001".to_owned(), "list".to_owned(), vec![]),
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
            "Sheet set: A001 Drawing List, A101 Ground Floor, A102 First, A201 North, A301 Section A, A401 Ground Floor framing, A501 (2 details); 8 lines weighted by role."
        );
        assert_eq!(view_sheet_ref(s.doc()?, ground), Some((1, "A101".to_owned())));
        let mut want_tabs: Vec<String> = ["A001", "A101", "A102", "A201", "A301", "A401", "A501"].map(String::from).to_vec();
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
        assert_eq!(laid(&r), [("A001".to_owned(), "list".to_owned(), vec![]), ("A103".to_owned(), "plan".to_owned(), vec![third.0])], "{r}");
        let t = tabs(&s)?;
        assert_eq!(t[..8], ["A001", "A101", "A102", "A103", "A201", "A301", "A401", "A501"], "{t:?}");
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
        assert_eq!(laid(&r), [("A001".to_owned(), "list".to_owned(), vec![]), ("A103".to_owned(), "plan".to_owned(), vec![later.0])], "{r}");
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
                EntityKind::Insert(i) if crate::vptitles::is_title_block(&i.block) => {
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
        assert_eq!(laid(&r), [("A001".to_owned(), "list".to_owned(), vec![]), ("A402".to_owned(), "framing".to_owned(), vec![sec.0])], "{r}");
        assert_eq!(r["message"], "Sheet set: A001 Drawing List updated, A402 Section A framing; 7 already on sheets.");

        // A re-run adds nothing and no undo step.
        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n, "no undo step");
        assert_eq!(r["message"], "Sheet set: nothing new to lay out; 8 already on sheets.");

        // A second plan's assembly adds exactly one, and the tabs stay in set order.
        s.execute("assemblyplan", &json!({ "viewId": first.0 }))?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(laid(&r), [("A001".to_owned(), "list".to_owned(), vec![]), ("A403".to_owned(), "framing".to_owned(), vec![first.0])], "{r}");
        let t = tabs(&s)?;
        assert_eq!(t[..9], ["A001", "A101", "A102", "A201", "A301", "A401", "A402", "A403", "A501"], "{t:?}");

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
        assert_eq!(names, ["A001", "A101", "A102", "A301", "A401", "A501"], "{r}");
        assert_eq!(r["alreadyOnSheets"], 1);
        assert!(r["message"].as_str().unwrap_or_default().contains("; 1 already on sheets;"), "{r}");
        assert_eq!(view_sheet_ref(s.doc()?, elev), Some((1, "Mine".to_owned())));
        Ok(())
    }

    /// The titles of a PDF's top-level bookmarks, in order.
    fn bookmarks(bytes: &[u8]) -> T<Vec<String>> {
        use craft_pdf::syntax::object::{Dict, String as PdfString};
        let pdf = craft_pdf::open_slice(bytes, None).map_err(|e| format!("the PDF opens: {e:?}"))?;
        let catalog = pdf.xref().get::<Dict<'_>>(pdf.xref().root_id()).ok_or("a catalog")?;
        let mut at = catalog.get::<Dict<'_>>(b"Outlines").and_then(|o| o.get::<Dict<'_>>(b"First"));
        let mut out = Vec::new();
        while let Some(item) = at {
            let title = item.get::<PdfString<'_>>(b"Title").ok_or("a bookmark's title")?;
            out.push(String::from_utf8_lossy(title.as_bytes()).into_owned());
            at = item.get::<Dict<'_>>(b"Next");
        }
        Ok(out)
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
        assert_eq!(r["pdf"]["pages"], 7, "{r}");
        let bytes = std::fs::read(&path)?;
        let pdf = craft_pdf::open_slice(&bytes, None).map_err(|e| format!("the PDF opens: {e:?}"))?;
        assert_eq!(pdf.pages().len(), 7, "the framing and list sheets are pages");
        let marks = bookmarks(&bytes)?;
        assert_eq!(marks.len(), 7, "{marks:?}");
        assert_eq!(marks.first().map(String::as_str), Some("A001 Drawing List"), "{marks:?}");
        let d = s.doc()?;
        let doc = read_document(d)?;
        let plan = set_sheets(d).into_iter().find(|n| n.starts_with("A1")).ok_or("a plan's sheet")?;
        let title = sheet_title(d, &doc, &plan);
        assert_ne!(title, plan, "the plan's sheet has a title");
        assert!(marks.contains(&format!("{plan} {title}")), "{plan} {title} in {marks:?}");
        for page in pdf.pages().iter() {
            let m = page.media_box();
            let size = ((m.x1 - m.x0).abs() / 72.0 * 25.4, (m.y1 - m.y0).abs() / 72.0 * 25.4);
            assert!((size.0 - ARCH_D.0).abs() < 0.1 && (size.1 - ARCH_D.1).abs() < 0.1, "{size:?}");
        }
        // A re-run with nothing new still plots the set.
        let r = s.execute("sheetset", &json!({ "pdf": path_s }))?;
        assert_eq!(r["pdf"]["pages"], 7, "{r}");

        // Counted framing: the BOM sheet is a page too.
        let (mut s, _) = stud_room()?;
        let r = s.execute("sheetset", &json!({ "pdf": path_s }))?;
        let names = set_sheets(s.doc()?);
        assert!(names.first().is_some_and(|n| n == "A001") && names.last().is_some_and(|n| n == "A601"), "{names:?}");
        assert_eq!(r["pdf"]["pages"], names.len(), "{r}");
        assert_eq!(craft_pdf::open_slice(&std::fs::read(&path)?, None).map_err(|e| format!("{e:?}"))?.pages().len(), names.len());
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

    /// Layout `layout`'s table titled `title`.
    fn table(s: &Session, layout: &str, title: &str) -> T<cadcraft_doc::Table> {
        let l = s.doc()?.layout(layout).ok_or("no layout")?;
        l.entities
            .iter()
            .find_map(|e| match &e.kind {
                EntityKind::Table(t) if t.cells[0][0].text == title => Some(t.clone()),
                _ => None,
            })
            .ok_or_else(|| format!("no {title} on {layout}").into())
    }

    /// A table's data rows as text.
    fn body(t: &cadcraft_doc::Table) -> Vec<Vec<String>> {
        t.cells[2..].iter().map(|r| r.iter().map(|c| c.text.clone()).collect()).collect()
    }

    fn row(sheet: &str, title: &str) -> Vec<String> {
        vec![sheet.to_owned(), title.to_owned()]
    }

    /// Moves the table on `layout` by hand.
    fn move_table(s: &mut Session, layout: &str, x: f64, y: f64) -> R {
        let d = s.doc_mut()?;
        let l = d.layouts.iter_mut().find(|l| l.name == layout).ok_or("no layout")?;
        let h = l.entities.iter().find(|e| matches!(e.kind, EntityKind::Table(_))).map(|e| e.handle).ok_or("no table")?;
        l.entities.modify(h, |e| {
            if let EntityKind::Table(t) = &mut e.kind {
                t.insert = cadcraft_geom::Vec3::new(x, y, 0.0);
            }
        });
        Ok(())
    }

    /// A second stud repeat in the only detail of a `stud_room`, so the count changes.
    fn add_stud(s: &mut Session, plan: Id) -> R {
        let detail = read_document(s.doc()?)?.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id();
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        let stud = new_handle(s, "pline", json!({ "vertices": [[300, 50], [338, 50], [338, 200], [300, 200]], "closed": true }))?;
        let dim = new_handle(s, "dimaligned", json!({ "p1": [300, 0], "p2": [700, 0], "at": [500, -300] }))?;
        s.execute("repeat", &json!({ "dimension": dim, "handles": [stud] }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        Ok(())
    }

    #[test]
    fn counted_framing_opens_on_the_list_and_ends_on_the_bom_in_one_undo_step() -> R {
        let (mut s, plan) = stud_room()?;
        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        let got = laid(&r);
        assert_eq!(got.first(), Some(&("A001".to_owned(), "list".to_owned(), vec![])), "{r}");
        assert_eq!(got.last(), Some(&("A601".to_owned(), "bom".to_owned(), vec![])), "{r}");

        let names: Vec<String> = tabs(&s)?.into_iter().filter(|n| is_set_name(n)).collect();
        assert_eq!(names.first().map(String::as_str), Some("A001"));
        assert_eq!(names.last().map(String::as_str), Some("A601"));
        assert_eq!(tabs(&s)?[..names.len()], names[..], "the set leads the tabs");
        let list = table(&s, "A001", "Drawing List")?;
        assert_eq!(list.cells[1].iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["Sheet", "Title"]);
        assert_eq!(list.col_widths, [30.0, 150.0]);
        assert_eq!((list.text_height, list.row_heights[0]), (2.5, 7.0));
        let rows = body(&list);
        assert_eq!(rows.iter().map(|r| r[0].clone()).collect::<Vec<_>>(), names, "every set sheet in tab order");
        assert_eq!(rows[0], row("A001", "Drawing List"));
        assert_eq!(rows.last(), Some(&row("A601", "Framing BOM")));
        assert!(rows.contains(&row("A101", "Ground")), "{rows:?}");

        let area = crate::sheets::usable_area(&cadcraft_render::sheet(s.doc()?, "A001").ok_or("no sheet")?);
        assert_eq!((list.insert.x, list.insert.y), (area.min.x, area.max.y), "the usable area's top-left");
        let bom = table(&s, "A601", "Framing BOM")?;
        assert_eq!((bom.insert.x, bom.insert.y), (area.min.x, area.max.y));
        assert_eq!(body(&bom)[0][..2], ["Level 1".to_owned(), "STUD".to_owned()]);

        // An immediate re-run changes nothing and adds no undo step.
        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n, "no undo step");
        assert_eq!(laid(&r), [], "{r}");
        assert_eq!(r["message"].as_str().map(|m| m.starts_with("Sheet set: nothing new to lay out")), Some(true), "{r}");

        // More framing changes only the BOM.
        add_stud(&mut s, plan)?;
        let list_before = table(&s, "A001", "Drawing List")?;
        let bom_before = table(&s, "A601", "Framing BOM")?;
        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!(laid(&r), [("A601".to_owned(), "bom".to_owned(), vec![])], "{r}");
        assert!(r["message"].as_str().is_some_and(|m| m.contains("A601 Framing BOM updated")), "{r}");
        assert_eq!(table(&s, "A001", "Drawing List")?, list_before, "the list holds");
        let bom = table(&s, "A601", "Framing BOM")?;
        assert_ne!(body(&bom), body(&bom_before), "the count moved");
        s.undo()?;
        assert_eq!(table(&s, "A601", "Framing BOM")?, bom_before, "undo restores the old count");
        Ok(())
    }

    #[test]
    fn a_new_plan_joins_the_list_in_its_sheets_undo_step() -> R {
        let (mut s, [ground, ..]) = set()?;
        s.execute("sheetset", &json!({}))?;
        let before = body(&table(&s, "A001", "Drawing List")?);
        assert_eq!(
            before,
            [
                row("A001", "Drawing List"),
                row("A101", "Ground Floor"),
                row("A102", "First"),
                row("A201", "North"),
                row("A301", "Section A"),
                row("A401", "Ground Floor framing"),
                row("A501", "Details"),
            ]
        );
        s.execute("newoffsetplan", &json!({ "mode": "offset", "fromViewId": ground.0, "floorToFloorMm": 6000, "name": "Later" }))?;
        let n = undo_len(&s)?;
        s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!(body(&table(&s, "A001", "Drawing List")?)[3], row("A103", "Later"));
        s.undo()?;
        assert_eq!(body(&table(&s, "A001", "Drawing List")?), before);
        assert!(s.doc()?.layout("A103").is_none());
        Ok(())
    }

    #[test]
    fn no_framing_means_no_bom_sheet_and_an_empty_project_no_list() -> R {
        let (mut s, _) = set()?;
        let r = s.execute("sheetset", &json!({}))?;
        assert!(s.doc()?.layout("A601").is_none(), "{r}");

        crate::commands::install_app()?;
        let mut empty = Session::new();
        empty.new_drawing(true);
        crate::maintain::install(&mut empty);
        empty.execute("lambproject", &json!({}))?;
        // With its one view gone the project has nothing to put on a sheet.
        let d = empty.doc_mut()?;
        let mut doc = read_document(d)?;
        doc.views.clear();
        write_document(d, &doc)?;
        let n = undo_len(&empty)?;
        let r = empty.execute("sheetset", &json!({}))?;
        assert!(empty.doc()?.layout("A001").is_none(), "{r}");
        assert_eq!(undo_len(&empty)?, n, "no undo step");
        Ok(())
    }

    #[test]
    fn a_bom_with_no_rows_left_stays_as_it_was() -> R {
        let (mut s, _) = stud_room()?;
        s.execute("sheetset", &json!({}))?;
        let before = table(&s, "A601", "Framing BOM")?;
        // Floating, the plan counts nothing.
        let d = s.doc_mut()?;
        let mut doc = read_document(d)?;
        for v in &mut doc.views {
            if let AnyView::Plan(p) = v {
                p.placement = None;
            }
        }
        write_document(d, &doc)?;
        s.execute("sheetset", &json!({}))?;
        assert_eq!(table(&s, "A601", "Framing BOM")?, before);
        Ok(())
    }

    #[test]
    fn a_moved_table_stays_where_it_was_put_and_a_renamed_view_shows_next_run() -> R {
        let (mut s, [ground, ..]) = set()?;
        s.execute("sheetset", &json!({}))?;
        move_table(&mut s, "A001", 40.0, 400.0)?;
        let d = s.doc_mut()?;
        let mut doc = read_document(d)?;
        for v in &mut doc.views {
            if let AnyView::Plan(p) = v
                && p.id == ground
            {
                p.name = "Renamed".into();
            }
        }
        write_document(d, &doc)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert!(r["message"].as_str().is_some_and(|m| m.contains("A001 Drawing List updated")), "{r}");
        let list = table(&s, "A001", "Drawing List")?;
        assert_eq!((list.insert.x, list.insert.y), (40.0, 400.0));
        assert!(body(&list).contains(&row("A101", "Renamed")), "{:?}", body(&list));
        Ok(())
    }

    #[test]
    fn a_table_past_the_sheets_edge_is_kept_whole_and_reported() -> R {
        let (mut s, _) = stud_room()?;
        s.execute("sheetset", &json!({}))?;
        move_table(&mut s, "A601", 40.0, 10.0)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert!(r["message"].as_str().is_some_and(|m| m.ends_with("; A601 Framing BOM runs off the sheet.")), "{r}");
        assert_eq!(body(&table(&s, "A601", "Framing BOM")?).len(), 1, "written whole");
        Ok(())
    }

    /// The plain lines of a display list, as segments.
    fn segments(list: &cadcraft_render::DisplayList) -> Vec<(cadcraft_geom::Vec2, cadcraft_geom::Vec2)> {
        list.prims
            .iter()
            .filter(|p| matches!(p.kind, cadcraft_render::Kind::Polyline))
            .flat_map(|p| {
                let v = &list.verts[p.start as usize..(p.start + p.len) as usize];
                v.windows(2).map(|w| (w[0], w[1])).collect::<Vec<_>>()
            })
            .collect()
    }

    #[test]
    fn a_set_sheet_plots_the_view_without_the_viewports_rectangle() -> R {
        let (mut s, _) = set()?;
        s.execute("sheetset", &json!({}))?;
        let (vp, _) = only_viewport(&s, "A101")?;
        let d = s.doc()?;
        let (hw, hh) = (vp.width / 2.0, vp.height / 2.0);
        let (c, near) = (vp.center, |a: f64, b: f64| (a - b).abs() < 1e-6);
        let on_rect = |a: cadcraft_geom::Vec2, b: cadcraft_geom::Vec2| {
            (near(a.x, b.x) && (near(a.x, c.x - hw) || near(a.x, c.x + hw))) || (near(a.y, b.y) && (near(a.y, c.y - hh) || near(a.y, c.y + hh)))
        };
        let space = Space::Paper("A101".into());
        let opts = cadcraft_render::Options::default();
        let shown = segments(&cadcraft_render::build(d, &space, &opts));
        assert!(shown.iter().filter(|(a, b)| on_rect(*a, *b)).count() >= 4, "the border stays on screen: {shown:?}");
        let plotted = segments(&cadcraft_render::build_plot(d, &space, &opts));
        assert!(!plotted.iter().any(|(a, b)| on_rect(*a, *b)), "no rectangle on paper: {plotted:?}");
        // The 5000 mm wall at the viewport's scale.
        let wall = 5000.0 / crate::sheets::viewport_scale(&vp);
        assert!(plotted.iter().any(|(a, b)| near((b.x - a.x).abs(), wall) && near(a.y, b.y)), "the view's lines plot: {plotted:?}");
        Ok(())
    }

    /// The RGB colours a PDF's content stream sets, in 0..=1.
    fn pdf_colours(d: &Drawing, sheet: &str) -> T<Vec<[f64; 3]>> {
        let bytes = cadcraft_io::plot(d, &Space::Paper(sheet.into()), &json!({ "compress": false }))?;
        let pdf = craft_pdf::open_slice(&bytes, None).map_err(|e| format!("the PDF opens: {e:?}"))?;
        let pages = pdf.pages();
        let page = pages.iter().next().ok_or("no page")?;
        let content = String::from_utf8_lossy(page.page_stream().ok_or("no content")?).into_owned();
        let tokens: Vec<&str> = content.split_whitespace().collect();
        let mut out: Vec<[f64; 3]> = Vec::new();
        for w in tokens.windows(4) {
            if matches!(w[3], "RG" | "rg" | "SC" | "sc" | "SCN" | "scn")
                && let (Ok(r), Ok(g), Ok(b)) = (w[0].parse(), w[1].parse(), w[2].parse())
                && !out.contains(&[r, g, b])
            {
                out.push([r, g, b]);
            }
        }
        Ok(out)
    }

    #[test]
    fn a_red_layer_plots_black_and_a_chosen_table_survives() -> R {
        let (mut s, _) = set()?;
        s.execute("sheetset", &json!({}))?;
        s.doc_mut()?.layer_mut("WALL-EXT").ok_or("no layer")?.color = cadcraft_color::Color::Index(1);
        let page = |d: &Drawing, name: &str| d.layout(name).map(|l| l.page.plot_style_table.clone());
        assert_eq!(page(s.doc()?, "A101").as_deref(), Some(MONOCHROME_CTB));
        let red = [1.0, 0.0, 0.0];
        let black = pdf_colours(s.doc()?, "A101")?;
        assert!(black.contains(&[0.0, 0.0, 0.0]) && !black.contains(&red), "{black:?}");
        // The same sheet in the object's colours prints red: the test can see it.
        let mut d = s.doc()?.clone();
        d.layouts.iter_mut().find(|l| l.name == "A101").ok_or("no layout")?.page.plot_style_table.clear();
        let coloured = pdf_colours(&d, "A101")?;
        assert!(coloured.contains(&red), "{coloured:?}");

        // A user's choice stands, and the screen keeps its colours.
        s.doc_mut()?.layouts.iter_mut().find(|l| l.name == "A101").ok_or("no layout")?.page.plot_style_table = "grayscale.ctb".to_owned();
        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(page(s.doc()?, "A101").as_deref(), Some("grayscale.ctb"), "{r}");
        assert_eq!(undo_len(&s)?, n, "nothing to change: {r}");
        assert!(!s.doc()?.layout("A101").ok_or("no layout")?.page.show_plot_styles);
        Ok(())
    }

    #[test]
    fn older_sheets_move_in_one_undo_step_and_a_second_run_is_a_no_op() -> R {
        let (mut s, _) = set()?;
        s.execute("sheetset", &json!({}))?;
        // As older sheets were built, with a foreign viewport beside them.
        let handles: Vec<cadcraft_doc::Handle> = {
            let d = s.doc()?;
            let mut out = Vec::new();
            for l in &d.layouts {
                for e in d.space(&Space::Paper(l.name.clone())).into_iter().flat_map(|st| st.iter()) {
                    if matches!(e.kind, EntityKind::Viewport(_))
                        && read_entity_data(&e.common).is_ok_and(|x| x.view_id.is_some() || x.assembly_of.is_some())
                    {
                        out.push(e.handle);
                    }
                }
            }
            out
        };
        assert!(!handles.is_empty());
        let (vp, _) = only_viewport(&s, "A101")?;
        let d = s.doc_mut()?;
        for h in &handles {
            d.modify_entity(*h, |e| e.common.layer = "0".to_owned())?;
        }
        for l in &mut d.layouts {
            l.page.plot_style_table.clear();
        }
        d.layers.retain(|l| l.name != VPORT_LAYER);
        let foreign = d.add(&Space::Paper("A201".into()), cadcraft_doc::Common::default(), EntityKind::Viewport(vp))?;
        let layer_of = |d: &Drawing, h| d.entity(h).map(|e| e.common.layer.clone());

        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step: {r}");
        let d = s.doc()?;
        for h in &handles {
            assert_eq!(layer_of(d, *h).as_deref(), Some(VPORT_LAYER), "{r}");
        }
        assert_eq!(layer_of(d, foreign).as_deref(), Some("0"), "a foreign viewport stays");
        assert!(d.layer(VPORT_LAYER).is_some_and(|l| !l.plot));
        for name in set_sheets(d) {
            assert_eq!(d.layout(&name).ok_or("no layout")?.page.plot_style_table, MONOCHROME_CTB, "{name}");
        }
        assert!(r["message"].as_str().ok_or("no message")?.contains(&format!("; {} sheets updated for plotting", set_sheets(d).len())), "{r}");

        let again = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "no change, no undo step: {again}");
        assert!(!again["message"].as_str().ok_or("no message")?.contains("updated for plotting"), "{again}");

        s.execute("undo", &json!({}))?;
        let d = s.doc()?;
        for h in &handles {
            assert_eq!(layer_of(d, *h).as_deref(), Some("0"), "undone");
        }
        assert!(d.layout("A101").ok_or("no layout")?.page.plot_style_table.is_empty(), "undone");
        Ok(())
    }

    #[test]
    fn proposed_lines_are_counted_and_drop_to_zero_after_accept() -> R {
        set_io(IoHooks {
            read: |b, n| cadcraft_io::read(b, n).map_err(|e| e.to_string()),
            write: |d, n| cadcraft_io::write(d, n).map_err(|e| e.to_string()),
            plot: Some(|d, space, opts| cadcraft_io::plotter::plot(d, space, opts).map_err(|e| e.to_string())),
        });
        // The room crossing the section line gives the section its proposed lines.
        let (mut s, [_, _, _, sec]) = set()?;
        let proposals = |s: &Session| -> T<Vec<cadcraft_doc::Handle>> {
            let d = s.doc()?;
            let block = d.block(&view_block_name(sec)).ok_or("no block")?;
            let unaccepted =
                |e: &&std::sync::Arc<cadcraft_doc::Entity>| read_entity_data(&e.common).is_ok_and(|x| x.ai_origin.is_some_and(|o| !o.accepted));
            Ok(block.entities.iter().filter(unaccepted).map(|e| e.handle).collect())
        };
        let lines = proposals(&s)?;
        assert!(!lines.is_empty(), "the fixture proposes lines");
        let dir = std::env::temp_dir().join(format!("lamb-sheetset-ai-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("set.pdf").to_string_lossy().into_owned();

        // Only with a plot: a sheet with no PDF says nothing about it.
        let r = s.execute("sheetset", &json!({}))?;
        assert!(r.get("proposedLines").is_none(), "{r}");
        let r = s.execute("sheetset", &json!({ "pdf": path }))?;
        assert_eq!(r["proposedLines"], lines.len(), "{r}");
        let n = lines.len();
        let plural = if n == 1 {
            "; 1 proposed AI line plots as drawn: accept or erase it.".to_owned()
        } else {
            format!("; {n} proposed AI lines plot as drawn: accept or erase them.")
        };
        assert!(r["message"].as_str().ok_or("no message")?.ends_with(&plural), "{r}");

        let hexes: Vec<String> = lines.iter().map(|h| h.hex()).collect();
        s.execute("accept", &json!({ "handles": hexes }))?;
        assert!(proposals(&s)?.is_empty());
        let r = s.execute("sheetset", &json!({ "pdf": path }))?;
        assert_eq!(r["proposedLines"], 0, "{r}");
        assert!(!r["message"].as_str().ok_or("no message")?.contains("proposed"), "{r}");
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    /// Adds a line to `view`'s block from the origin to `(x, 0)`.
    fn far_line(s: &mut Session, view: Id, x: f64) -> R {
        s.execute("editview", &json!({ "viewId": view.0 }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [x, 0]], "closed": false }))?;
        s.execute("editview", &json!({}))?;
        Ok(())
    }

    fn refit_layouts(r: &Value) -> Vec<(String, f64)> {
        r["refit"].as_array().map_or_else(Vec::new, |a| {
            a.iter().map(|x| (x["layout"].as_str().unwrap_or_default().to_owned(), x["scale"].as_f64().unwrap_or_default())).collect()
        })
    }

    fn inside_window(s: &Session, view: Id, vp: &cadcraft_doc::Viewport) -> T<bool> {
        let ext = s.doc()?.extents(&Space::Block(view_block_name(view)));
        let (hw, hh) = (vp.width * viewport_scale(vp) / 2.0, vp.view_height / 2.0);
        let c = vp.view_center;
        Ok(ext.min.x >= c.x - hw && ext.max.x <= c.x + hw && ext.min.y >= c.y - hh && ext.max.y <= c.y + hh)
    }

    #[test]
    fn a_plan_that_outgrew_its_viewport_recentres_at_its_scale_then_goes_larger() -> R {
        let (mut s, [ground, ..]) = set()?;
        s.execute("sheetset", &json!({}))?;
        let (old, _) = only_viewport(&s, "A101")?;
        assert_eq!(viewport_scale(&old), 50.0);

        // Too far for the window, but the viewport holds it at 1:50.
        far_line(&mut s, ground, old.width * 50.0 * 0.7)?;
        assert!(!inside_window(&s, ground, &old)?, "the line is cut off");
        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!(refit_layouts(&r), [("A101".to_owned(), 50.0)], "{r}");
        assert_eq!(r["refit"][0]["view"], ground.0);
        assert!(r["message"].as_str().is_some_and(|m| m.contains("; 1 viewport refit (A101 now 1:50)")), "{r}");
        let (vp, _) = only_viewport(&s, "A101")?;
        assert!(inside_window(&s, ground, &vp)?, "centred on the extents");
        assert_eq!((vp.width, vp.height, vp.center, vp.view_height), (old.width, old.height, old.center, old.view_height), "only the centre moved");
        assert_eq!(sheet_titles(&s, "A101")?[0].2, "1:50");

        // One undo restores the old window.
        s.undo()?;
        let (back, _) = only_viewport(&s, "A101")?;
        assert_eq!((back.view_center, back.view_height), (old.view_center, old.view_height));
        s.redo()?;

        // Past what 1:50 holds: the next larger denominator, and the title follows.
        far_line(&mut s, ground, old.width * 50.0 * 1.5)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(refit_layouts(&r), [("A101".to_owned(), 100.0)], "{r}");
        assert!(r["message"].as_str().is_some_and(|m| m.contains("; 1 viewport refit (A101 now 1:100)")), "{r}");
        let (vp, _) = only_viewport(&s, "A101")?;
        assert!(inside_window(&s, ground, &vp)?);
        assert_eq!(vp.center, old.center, "same place on the sheet");
        assert_eq!(sheet_titles(&s, "A101")?[0].2, "1:100");

        // Done: a second run changes nothing.
        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n, "no undo step");
        assert_eq!(r["refit"], json!([]), "{r}");
        Ok(())
    }

    #[test]
    fn a_viewport_panned_by_hand_that_still_shows_everything_is_untouched() -> R {
        let (mut s, _) = set()?;
        s.execute("sheetset", &json!({}))?;
        let d = s.doc_mut()?;
        let l = d.layout("A101").ok_or("no layout")?;
        let h = l.entities.iter().find(|e| matches!(&e.kind, EntityKind::Viewport(vp) if vp.id > 1)).map(|e| e.handle).ok_or("no viewport")?;
        d.modify_entity(h, |e| {
            if let EntityKind::Viewport(vp) = &mut e.kind {
                vp.view_center += cadcraft_geom::Vec2::new(500.0, -300.0);
            }
        })?;
        let (panned, _) = only_viewport(&s, "A101")?;
        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(r["refit"], json!([]), "{r}");
        assert_eq!(undo_len(&s)?, n, "no undo step");
        let (vp, _) = only_viewport(&s, "A101")?;
        assert_eq!((vp.view_center, vp.view_height), (panned.view_center, panned.view_height));
        Ok(())
    }

    #[test]
    fn the_pdf_shows_the_refit_viewport_and_its_title() -> R {
        set_io(IoHooks {
            read: |b, n| cadcraft_io::read(b, n).map_err(|e| e.to_string()),
            write: |d, n| cadcraft_io::write(d, n).map_err(|e| e.to_string()),
            plot: Some(|d, space, opts| cadcraft_io::plotter::plot(d, space, opts).map_err(|e| e.to_string())),
        });
        let (mut s, [ground, ..]) = set()?;
        s.execute("sheetset", &json!({}))?;
        let (old, _) = only_viewport(&s, "A101")?;
        far_line(&mut s, ground, old.width * 50.0 * 1.5)?;
        let dir = std::env::temp_dir().join(format!("lamb-sheetset-refit-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("set.pdf");
        let path_s = path.to_string_lossy().into_owned();
        let a101_page = |p: &std::path::Path| -> T<Vec<u8>> {
            let pdf = craft_pdf::open_slice(&std::fs::read(p)?, None).map_err(|e| format!("{e:?}"))?;
            let pages = pdf.pages();
            let page = pages.get(1).ok_or("no page")?;
            Ok(page.page_stream().ok_or("no content")?.to_vec())
        };
        let r = s.execute("sheetset", &json!({ "pdf": path_s }))?;
        assert_eq!(refit_layouts(&r), [("A101".to_owned(), 100.0)], "{r}");
        let first = a101_page(&path)?;
        // A run with nothing left to change plots the drawing as it now stands: the same page.
        let r = s.execute("sheetset", &json!({ "pdf": path_s }))?;
        assert_eq!(r["refit"], json!([]), "{r}");
        assert_eq!(first, a101_page(&path)?, "the first PDF already showed the refit, its title and annotation");
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn a_sheet_whose_view_is_not_placed_is_reported_and_unchanged() -> R {
        let (mut s, [ground, first, _, sec]) = set()?;
        s.execute("sheetset", &json!({}))?;
        let (old, _) = only_viewport(&s, "A102")?;
        let (old_sec, _) = only_viewport(&s, "A301")?;
        far_line(&mut s, first, old.width * 50.0 * 1.5)?;
        far_line(&mut s, sec, old_sec.width * 50.0 * 1.5)?;
        let d = s.doc_mut()?;
        let mut doc = read_document(d)?;
        for v in &mut doc.views {
            if let AnyView::Plan(p) = v
                && p.id == first
            {
                p.placement = None;
            }
        }
        doc.views.retain(|v| v.id() != sec);
        write_document(d, &doc)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(r["refit"], json!([]), "{r}");
        assert!(laid(&r).iter().all(|(_, group, _)| group == "list"), "only the list's rows change: {r}");
        let m = r["message"].as_str().ok_or("no message")?;
        assert!(m.contains("; A102 shows First, which is not placed"), "{m}");
        assert!(m.contains(&format!("; A301 shows view {}, which is not placed", sec.0)), "{m}");
        let (vp, _) = only_viewport(&s, "A102")?;
        assert_eq!((vp.view_center, vp.view_height), (old.view_center, old.view_height));
        let (vp, _) = only_viewport(&s, "A301")?;
        assert_eq!((vp.view_center, vp.view_height), (old_sec.view_center, old_sec.view_height));
        assert_eq!(view_sheet_ref(s.doc()?, ground), Some((1, "A101".to_owned())));
        Ok(())
    }

    /// A line of `length` drawn along y = `y` in the view being edited, on the current layer.
    fn line_of(s: &mut Session, y: f64, length: f64) -> T<String> {
        new_handle(s, "line", json!({ "points": [[0, y], [length, y]] }))
    }

    /// [`line_of`] in `view`, whose block alone is searched: drawing in a vertical view can add
    /// generated entities to other blocks.
    fn line_in(s: &mut Session, view: Id, y: f64, length: f64) -> T<String> {
        let all = |s: &Session| -> T<Vec<Handle>> {
            Ok(s.doc()?.block(&view_block_name(view)).ok_or("no block")?.entities.iter().map(|e| e.handle).collect())
        };
        let before = all(s)?;
        s.execute("line", &json!({ "points": [[0, y], [length, y]] }))?;
        let h = all(s)?.into_iter().find(|h| !before.contains(h)).ok_or("nothing added")?;
        Ok(format!("{:X}", h.0))
    }

    /// The width (mm) `layout` plots the line of `length` (model mm) with, in monochrome.
    fn plotted_width(d: &Drawing, layout: &str, length: f64) -> T<f32> {
        let table = cadcraft_doc::plot_style_table(d, MONOCHROME_CTB).ok_or("no table")?;
        let opts = cadcraft_render::Options { lineweights: true, plot_style_table: Some(std::sync::Arc::new(table)), ..Default::default() };
        let space = Space::Paper(layout.into());
        let list = cadcraft_render::build_plot(d, &space, &opts);
        let port = d.space(&space).ok_or("no sheet")?.iter().find_map(|e| match &e.kind {
            EntityKind::Viewport(v) if read_entity_data(&e.common).is_ok_and(|x| x.view_id.is_some()) => Some(v.clone()),
            _ => None,
        });
        let want = length / crate::sheets::viewport_scale(&port.ok_or("no viewport")?);
        let widths: Vec<f32> = list
            .prims
            .iter()
            .filter(|p| matches!(p.kind, cadcraft_render::Kind::Polyline) && p.len == 2)
            .filter(|p| {
                let v = &list.verts[p.start as usize..(p.start + p.len) as usize];
                ((v[1].x - v[0].x).hypot(v[1].y - v[0].y) - want).abs() < 1e-6
            })
            .map(|p| p.lw)
            .collect();
        match widths.as_slice() {
            [w] => Ok(*w),
            found => Err(format!("{layout}: a line {length} long plots as {found:?}").into()),
        }
    }

    /// The widths (hundredths of a mm) a sheet's PDF strokes with.
    fn pdf_widths(d: &Drawing, sheet: &str) -> T<Vec<i64>> {
        let bytes = cadcraft_io::plot(d, &Space::Paper(sheet.into()), &json!({ "compress": false }))?;
        let pdf = craft_pdf::open_slice(&bytes, None).map_err(|e| format!("the PDF opens: {e:?}"))?;
        let pages = pdf.pages();
        let page = pages.iter().next().ok_or("no page")?;
        let content = String::from_utf8_lossy(page.page_stream().ok_or("no content")?).into_owned();
        let tokens: Vec<&str> = content.split_whitespace().collect();
        let mut out: Vec<i64> = Vec::new();
        for w in tokens.windows(2).filter(|w| w[1] == "w") {
            let hundredths = (w[0].parse::<f64>()? * 25.4 / 72.0 * 100.0).round() as i64;
            if !out.contains(&hundredths) {
                out.push(hundredths);
            }
        }
        Ok(out)
    }

    fn weight_of(s: &Session, handle: &str) -> T<Lineweight> {
        let h = Handle::parse_hex(handle).ok_or("not a handle")?;
        Ok(s.doc()?.entity(h).ok_or("no entity")?.common.lineweight)
    }

    #[test]
    fn a_plans_cut_lines_plot_heavier_than_its_seen_lines_in_one_undo_step() -> R {
        let (mut s, [ground, ..]) = set()?;
        s.execute("editview", &json!({ "viewId": ground.0 }))?;
        s.execute("layer.new", &json!({ "name": "MIXED", "current": true }))?;
        let cut = line_of(&mut s, 1000.0, 3100.0)?;
        let seen = line_of(&mut s, 1500.0, 2100.0)?;
        let grid = line_of(&mut s, 2000.0, 2300.0)?;
        let note = line_of(&mut s, 2500.0, 2700.0)?;
        for (h, role) in [(&seen, "seen"), (&grid, "datum"), (&note, "annotation")] {
            s.execute("setrole", &json!({ "handles": [h], "role": role }))?;
        }
        s.execute("editview", &json!({}))?;

        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step: {r}");
        let d = s.doc()?;
        let w = |len| plotted_width(d, "A101", len);
        assert_eq!((w(3100.0)?, w(2100.0)?, w(2300.0)?, w(2700.0)?), (0.5, 0.25, 0.13, 0.18), "{r}");
        assert_eq!(weight_of(&s, &seen)?, Lineweight::ByLayer, "seen stays at the default");
        // On paper, in the monochrome PDF.
        let widths = pdf_widths(d, "A101")?;
        assert!(widths.contains(&50) && widths.contains(&25), "{widths:?}");
        let weighted = r["weighted"].as_u64().ok_or("no count")?;
        assert!(weighted >= 3, "{r}");
        assert!(r["message"].as_str().ok_or("no message")?.contains(&format!("; {weighted} lines weighted by role")), "{r}");

        // A rerun stamps nothing and adds no undo step.
        let again = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "no undo step: {again}");
        assert_eq!(again["weighted"], 0, "{again}");
        assert!(!again["message"].as_str().ok_or("no message")?.contains("weighted"), "{again}");

        // One new line is one line, in the singular.
        s.execute("editview", &json!({ "viewId": ground.0 }))?;
        let later = line_of(&mut s, 3000.0, 3300.0)?;
        s.execute("editview", &json!({}))?;
        let one = s.execute("sheetset", &json!({}))?;
        assert_eq!(one["weighted"], 1, "{one}");
        assert!(one["message"].as_str().ok_or("no message")?.contains("; 1 line weighted by role"), "{one}");
        s.execute("undo", &json!({}))?;
        assert_eq!(weight_of(&s, &later)?, Lineweight::ByLayer, "undone");
        assert_eq!(weight_of(&s, &cut)?, Lineweight::Mm100(50), "the earlier stamp stays");

        // Undoing back past the first run restores every stamp along with the sheets.
        while undo_len(&s)? > n {
            s.execute("undo", &json!({}))?;
        }
        assert_eq!(weight_of(&s, &cut)?, Lineweight::ByLayer, "undone");
        assert!(s.doc()?.layout("A101").is_none(), "the sheets go with it");
        Ok(())
    }

    #[test]
    fn a_rerun_writes_the_same_pdf() -> R {
        set_io(IoHooks {
            read: |b, n| cadcraft_io::read(b, n).map_err(|e| e.to_string()),
            write: |d, n| cadcraft_io::write(d, n).map_err(|e| e.to_string()),
            plot: Some(|d, space, opts| cadcraft_io::plotter::plot(d, space, opts).map_err(|e| e.to_string())),
        });
        let (mut s, _) = set()?;
        let dir = std::env::temp_dir().join(format!("lamb-sheetset-weights-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("set.pdf");
        let path_s = path.to_string_lossy().into_owned();
        s.execute("sheetset", &json!({ "pdf": path_s }))?;
        let first = std::fs::read(&path)?;
        let n = undo_len(&s)?;
        let again = s.execute("sheetset", &json!({ "pdf": path_s }))?;
        let second = std::fs::read(&path)?;
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(again["weighted"], 0, "{again}");
        assert_eq!(undo_len(&s)?, n, "{again}");
        assert!(first == second, "the PDF bytes are unchanged: {} and {} long", first.len(), second.len());
        Ok(())
    }

    #[test]
    fn a_vertical_views_lines_are_weighted_by_their_own_role() -> R {
        let (mut s, [_, _, elev, _]) = set()?;
        s.execute("editview", &json!({ "viewId": elev.0 }))?;
        s.execute("layer.new", &json!({ "name": "BEYOND", "current": true }))?;
        let seen = line_in(&mut s, elev, 500.0, 1000.0)?; // A cut-default layer stamps seen in a vertical.
        let cut = line_in(&mut s, elev, 900.0, 1200.0)?;
        s.execute("setrole", &json!({ "handles": [&cut], "role": "cut" }))?;
        s.execute("editview", &json!({}))?;
        s.execute("sheetset", &json!({}))?;
        assert_eq!(weight_of(&s, &seen)?, Lineweight::ByLayer);
        assert_eq!(weight_of(&s, &cut)?, Lineweight::Mm100(50));
        Ok(())
    }

    #[test]
    fn a_chosen_weight_stands_and_layer_0_goes_by_the_views_layer() -> R {
        let (mut s, [ground, first, ..]) = set()?;
        s.execute("editview", &json!({ "viewId": ground.0 }))?;
        s.execute("layer.new", &json!({ "name": "CHOSEN", "current": true }))?;
        let on_chosen = line_of(&mut s, 1000.0, 3100.0)?;
        s.execute("layer.new", &json!({ "name": "PLAIN", "current": true }))?;
        let own = line_of(&mut s, 1500.0, 2100.0)?;
        let by_block = line_of(&mut s, 1700.0, 2200.0)?;
        let on_zero = line_of(&mut s, 2000.0, 2300.0)?;
        s.execute("editview", &json!({ "viewId": first.0 }))?;
        let zero_chosen = line_of(&mut s, 500.0, 2400.0)?;
        s.execute("editview", &json!({}))?;
        let at = |h: &str| Handle::parse_hex(h).ok_or("not a handle");
        let d = s.doc_mut()?;
        d.layer_mut("CHOSEN").ok_or("no layer")?.lineweight = Lineweight::Mm100(70);
        d.layer_mut(&view_block_name(first)).ok_or("no view layer")?.lineweight = Lineweight::Mm100(35);
        d.modify_entity(at(&own)?, |e| e.common.lineweight = Lineweight::Mm100(35))?;
        d.modify_entity(at(&by_block)?, |e| e.common.lineweight = Lineweight::ByBlock)?;
        d.modify_entity(at(&on_zero)?, |e| e.common.layer = "0".to_owned())?;
        d.modify_entity(at(&zero_chosen)?, |e| e.common.layer = "0".to_owned())?;

        s.execute("sheetset", &json!({}))?;
        assert_eq!(weight_of(&s, &on_chosen)?, Lineweight::ByLayer, "the layer's weight stands");
        assert_eq!(weight_of(&s, &own)?, Lineweight::Mm100(35), "the entity's weight stands");
        assert_eq!(weight_of(&s, &by_block)?, Lineweight::ByBlock, "so does ByBlock");
        assert_eq!(weight_of(&s, &on_zero)?, Lineweight::Mm100(50), "layer 0 goes by the view's plain layer");
        assert_eq!(weight_of(&s, &zero_chosen)?, Lineweight::ByLayer, "this view's layer has a weight chosen");
        let d = s.doc()?;
        let sheet = view_sheet_ref(d, ground).ok_or("no sheet")?.1;
        assert_eq!(plotted_width(d, &sheet, 2300.0)?, 0.5, "the renderer agrees about layer 0");
        assert_eq!(plotted_width(d, &sheet, 3100.0)?, 0.7, "the layer's weight plots");
        let sheet = view_sheet_ref(d, first).ok_or("no sheet")?.1;
        assert_eq!(plotted_width(d, &sheet, 2400.0)?, 0.35, "layer 0 plots at the view layer's weight");
        Ok(())
    }

    #[test]
    fn details_and_assembly_blocks_are_not_stamped() -> R {
        let (mut s, plan) = stud_room()?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        s.execute("editview", &json!({}))?;
        let r = s.execute("sheetset", &json!({}))?;
        assert!(r["weighted"].as_u64().ok_or("no count")? > 0, "{r}");
        let d = s.doc()?;
        let doc = read_document(d)?;
        let detail = doc.views.iter().find(|v| matches!(v, AnyView::Detail(_))).ok_or("no detail")?.id();
        for name in [view_block_name(detail), assembly_block_name(plan)] {
            let b = d.block(&name).ok_or_else(|| format!("no block {name}"))?;
            assert!(b.entities.iter().next().is_some(), "{name} has lines");
            assert!(b.entities.iter().all(|e| e.common.lineweight == Lineweight::ByLayer), "{name}");
        }
        Ok(())
    }

    /// A project of `lambproject`'s plan alone, plus a vertical view per `(command, name, width,
    /// height)`, each drawn as a rectangle of that size (`width` 0: undrawn). Returns (session, plan,
    /// the views).
    fn drawn_verticals(views: &[(&str, &str, f64, f64)]) -> T<(Session, Id, Vec<Id>)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = id(&s.execute("lambproject", &json!({}))?)?;
        let mut ids = Vec::new();
        for (k, (command, name, w, h)) in views.iter().enumerate() {
            ids.push(add_vertical(&mut s, plan, command, name, 6000.0 + 500.0 * k as f64, *w, *h)?);
        }
        Ok((s, plan, ids))
    }

    /// One vertical view on `plan` with its cut at `y`, drawn as a `w` × `h` rectangle unless `w` is 0.
    fn add_vertical(s: &mut Session, plan: Id, command: &str, name: &str, y: f64, w: f64, h: f64) -> T<Id> {
        let v = id(&s.execute(command, &json!({ "name": name, "hostViewId": plan.0, "cutFrom": {"x": 0, "y": y}, "cutTo": {"x": 5000, "y": y} }))?)?;
        if w > 0.0 {
            s.execute("editview", &json!({ "viewId": v.0 }))?;
            s.execute("pline", &json!({ "vertices": [[0, 0], [w, 0], [w, h], [0, h]], "closed": true }))?;
            s.execute("editview", &json!({}))?;
        }
        Ok(v)
    }

    /// Layout `name`'s sheet viewports with the view each names, in numbering order.
    fn sheet_viewports(s: &Session, name: &str) -> T<Vec<(Id, cadcraft_doc::Viewport)>> {
        let l = s.doc()?.layout(name).ok_or("no layout")?;
        let mut out: Vec<(Id, cadcraft_doc::Viewport)> = l
            .entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Viewport(vp) if vp.id > 1 => Some((read_entity_data(&e.common).ok()?.view_id?, vp.clone())),
                _ => None,
            })
            .collect();
        out.sort_by_key(|(_, vp)| vp.id);
        Ok(out)
    }

    /// Whether `vp`'s window holds `view`'s extents and its annotation's.
    fn shows_all_of(s: &Session, view: Id, vp: &cadcraft_doc::Viewport) -> T<bool> {
        let d = s.doc()?;
        let (hw, hh) = (vp.width * viewport_scale(vp) / 2.0, vp.view_height / 2.0);
        let c = vp.view_center;
        let blocks = [view_block_name(view), annotation_block_name(view)];
        Ok(blocks.into_iter().map(|b| d.extents(&Space::Block(b))).all(|b| {
            b.is_empty() || (b.min.x >= c.x - hw - 1e-6 && b.max.x <= c.x + hw + 1e-6 && b.min.y >= c.y - hh - 1e-6 && b.max.y <= c.y + hh + 1e-6)
        }))
    }

    /// Each viewport of `layout` and its title clear of the others and inside the usable area.
    fn assert_packed(s: &Session, layout: &str) -> R {
        let d = s.doc()?;
        let area = crate::sheets::usable_area(&cadcraft_render::sheet(d, layout).ok_or("no sheet")?);
        const TITLE: f64 = 15.0;
        let claims: Vec<(f64, f64, f64, f64)> = sheet_viewports(s, layout)?
            .iter()
            .map(|(_, vp)| {
                (vp.center.x - vp.width / 2.0, vp.center.y - vp.height / 2.0 - TITLE, vp.center.x + vp.width / 2.0, vp.center.y + vp.height / 2.0)
            })
            .collect();
        for (i, a) in claims.iter().enumerate() {
            assert!(a.0 >= area.min.x - 1e-6 && a.1 >= area.min.y - 1e-6 && a.2 <= area.max.x + 1e-6 && a.3 <= area.max.y + 1e-6, "{layout}: {a:?}");
            for b in &claims[i + 1..] {
                assert!(a.2 <= b.0 || b.2 <= a.0 || a.3 <= b.1 || b.3 <= a.1, "{layout}: {a:?} and {b:?}");
            }
        }
        Ok(())
    }

    /// The text values in `view`'s annotation block.
    fn annotation_texts(s: &Session, view: Id) -> T<Vec<String>> {
        let b = s.doc()?.block(&annotation_block_name(view)).ok_or("no annotation block")?;
        Ok(b.entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Text(t) => Some(t.value.clone()),
                _ => None,
            })
            .collect())
    }

    const ELEV: &str = "newelevationline";
    const SECT: &str = "newsectionline";
    const FOUR: [(&str, &str, f64, f64); 4] =
        [(ELEV, "North", 5000.0, 3000.0), (ELEV, "East", 5000.0, 3000.0), (ELEV, "South", 5000.0, 3000.0), (ELEV, "West", 5000.0, 3000.0)];

    #[test]
    fn four_elevations_share_a_sheet_numbered_in_document_order() -> R {
        let (mut s, plan, ids) = drawn_verticals(&FOUR)?;
        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        let elevations: Vec<_> = laid(&r).into_iter().filter(|(_, g, _)| g == "elevation").collect();
        assert_eq!(elevations, [("A201".to_owned(), "elevation".to_owned(), ids.iter().map(|v| v.0).collect())], "{r}");
        assert!(r["message"].as_str().unwrap_or_default().contains("A201 (4 views)"), "{r}");
        assert!(s.doc()?.layout("A202").is_none());

        let vps = sheet_viewports(&s, "A201")?;
        assert_eq!(vps.iter().map(|(v, _)| *v).collect::<Vec<_>>(), ids);
        assert_packed(&s, "A201")?;
        let mut titles = sheet_titles(&s, "A201")?;
        titles.sort();
        let cut = annotation_texts(&s, plan)?;
        for (k, ((view, vp), name)) in vps.iter().zip(["North", "East", "South", "West"]).enumerate() {
            let num = k + 1;
            assert_eq!(viewport_scale(vp), 50.0, "the plot scale on ARCH D");
            assert!(shows_all_of(&s, *view, vp)?, "{view:?} in {vp:?}");
            assert_eq!(view_sheet_ref(s.doc()?, *view), Some((num, "A201".to_owned())));
            assert_eq!(titles[k], (num.to_string(), name.to_owned(), "1:50".to_owned()), "{titles:?}");
            let reference = format!("{num}/A201");
            assert!(cut.windows(2).any(|w| w[0] == name && w[1] == reference), "{name} {reference} in {cut:?}");
        }

        // A rerun adds nothing, refits nothing and takes no undo step.
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "no undo step");
        assert_eq!(r["alreadyOnSheets"], 5, "the plan and the four: {r}");
        assert_eq!(r["sheets"], json!([]), "{r}");
        assert_eq!(r["refit"], json!([]), "{r}");

        s.undo()?;
        assert!(s.doc()?.layout("A201").is_none() && s.doc()?.layout("A101").is_none(), "one undo removes the run");
        Ok(())
    }

    #[test]
    fn sections_overflow_in_order_and_a_long_one_takes_its_own_scale() -> R {
        // 250 × 200 on paper before the annotation's room: more than one sheet holds.
        let many: Vec<String> = (1..=7).map(|k| format!("S{k}")).collect();
        let spec: Vec<(&str, &str, f64, f64)> = many.iter().map(|n| (SECT, n.as_str(), 10_000.0, 8000.0)).collect();
        let (mut s, _, ids) = drawn_verticals(&spec)?;
        let r = s.execute("sheetset", &json!({}))?;
        let sections: Vec<_> = laid(&r).into_iter().filter(|(_, g, _)| g == "section").collect();
        assert_eq!(sections.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["A301", "A302"], "{r}");
        let order: Vec<u64> = sections.iter().flat_map(|x| x.2.iter().copied()).collect();
        assert_eq!(order, ids.iter().map(|v| v.0).collect::<Vec<_>>(), "in document order: {r}");
        let first = sections[0].2.len();
        assert!(first > 1 && first < ids.len(), "{r}");
        let message = r["message"].as_str().unwrap_or_default();
        assert!(message.contains(&format!("A301 ({first} views), A302 ")), "{message}");
        for layout in ["A301", "A302"] {
            assert_packed(&s, layout)?;
        }
        assert_eq!(view_sheet_ref(s.doc()?, ids[first]), Some((1, "A302".to_owned())));

        let (mut s, _, ids) =
            drawn_verticals(&[(SECT, "Short", 5000.0, 3000.0), (SECT, "Long", 100_000.0, 3000.0), (SECT, "Also short", 5000.0, 3000.0)])?;
        let r = s.execute("sheetset", &json!({}))?;
        let vps = sheet_viewports(&s, "A301")?;
        assert_eq!(vps.iter().map(|(v, vp)| (*v, viewport_scale(vp))).collect::<Vec<_>>(), [(ids[0], 50.0), (ids[1], 200.0), (ids[2], 50.0)], "{r}");
        assert_packed(&s, "A301")?;
        for (view, vp) in &vps {
            assert!(shows_all_of(&s, *view, vp)?, "{view:?} in {vp:?}");
        }
        let long = sheet_titles(&s, "A301")?.into_iter().find(|t| t.1 == "Long").ok_or("no title")?;
        assert_eq!(long.2, "1:200", "the title has its own scale from the first run");
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(r["refit"], json!([]), "{r}");
        Ok(())
    }

    #[test]
    fn a_later_elevation_gets_a_new_sheet_and_an_empty_section_its_own() -> R {
        let (mut s, plan, ids) = drawn_verticals(&[
            (ELEV, "North", 5000.0, 3000.0),
            (ELEV, "East", 5000.0, 3000.0),
            (SECT, "Drawn", 5000.0, 3000.0),
            (SECT, "Empty", 0.0, 0.0),
        ])?;
        let r = s.execute("sheetset", &json!({}))?;
        let sections: Vec<_> = laid(&r).into_iter().filter(|(_, g, _)| g == "section").map(|(l, _, v)| (l, v)).collect();
        assert_eq!(sections, [("A301".to_owned(), vec![ids[2].0]), ("A302".to_owned(), vec![ids[3].0])], "{r}");
        assert!(r["message"].as_str().unwrap_or_default().contains("A301 Drawn, A302 Empty"), "{r}");
        // An undrawn view keeps create_sheet's viewport, with room for what is drawn later.
        let area = crate::sheets::usable_area(&cadcraft_render::sheet(s.doc()?, "A302").ok_or("no sheet")?);
        let (vp, _) = only_viewport(&s, "A302")?;
        assert!((vp.width - area.width() * 0.9).abs() < 1e-6 && (vp.height - area.height() * 0.9).abs() < 1e-6, "{vp:?}");

        let held = sheet_viewports(&s, "A201")?;
        let later = add_vertical(&mut s, plan, ELEV, "South", 9000.0, 5000.0, 3000.0)?;
        let n = undo_len(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        let new: Vec<_> = laid(&r).into_iter().filter(|(_, g, _)| g == "elevation").map(|(l, _, v)| (l, v)).collect();
        assert_eq!(new, [("A202".to_owned(), vec![later.0])], "{r}");
        assert_eq!(r["alreadyOnSheets"], 5, "the plan, two elevations, two sections: {r}");
        assert!(r["message"].as_str().unwrap_or_default().contains("A202 South"), "{r}");
        let now = sheet_viewports(&s, "A201")?;
        assert_eq!(now.len(), held.len());
        for ((_, a), (_, b)) in held.iter().zip(&now) {
            assert_eq!((a.center, a.width, a.height, a.view_center, a.view_height), (b.center, b.width, b.height, b.view_center, b.view_height));
        }
        Ok(())
    }

    #[test]
    fn a_packed_sets_pdf_has_a_page_per_sheet() -> R {
        set_io(IoHooks {
            read: |b, n| cadcraft_io::read(b, n).map_err(|e| e.to_string()),
            write: |d, n| cadcraft_io::write(d, n).map_err(|e| e.to_string()),
            plot: Some(|d, space, opts| cadcraft_io::plotter::plot(d, space, opts).map_err(|e| e.to_string())),
        });
        let (mut s, _, _) = drawn_verticals(&FOUR)?;
        let dir = std::env::temp_dir().join(format!("lamb-sheetset-packed-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("set.pdf");
        let r = s.execute("sheetset", &json!({ "pdf": path.to_string_lossy() }))?;
        assert_eq!(set_sheets(s.doc()?), ["A001", "A101", "A201"], "{r}");
        assert_eq!(r["pdf"]["pages"], 3, "{r}");
        assert_eq!(craft_pdf::open_slice(&std::fs::read(&path)?, None).map_err(|e| format!("{e:?}"))?.pages().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn set_names_are_a_prefix_and_two_digits() {
        assert!(
            is_set_name("A001") && is_set_name("A101") && is_set_name("A599") && is_set_name("A301") && is_set_name("A401") && is_set_name("A601")
        );
        assert!(!is_set_name("A701") && !is_set_name("A1") && !is_set_name("A1001") && !is_set_name("Layout1") && !is_set_name("A1x1"));
    }
}

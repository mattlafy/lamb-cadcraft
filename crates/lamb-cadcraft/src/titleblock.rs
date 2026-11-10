//! Sheet frames (LAMB T634): every LAMB sheet carries a border and a title block, generated and kept
//! current. A LAMB sheet is a layout with a viewport whose `view_id` or `assembly_of` is set
//! ([`crate::sheets`]), or one named a capital letter and three digits (`A101`, `A001`). Drawings
//! with no LAMB document get no frame.
//!
//! The frame is one `Insert` at the paper origin, on layer [`FRAME_LAYER`], of a block
//! `LAMB_SHEETFRAME_<W>X<H>` (whole millimetres; one definition per page size, made on demand; a
//! page whose margins are not [`crate::sheets::SHEET_MARGIN_MM`] or whose paper units are not
//! millimetres says so in a suffix). The block holds the printable-area rectangle, a strip down
//! its right edge ([`crate::sheets::frame_strip_width`]) and the strip's horizontal dividers. The
//! Insert's attributes, bottom up and with Lock position set: `SHEET` (the layout name, largest
//! text), `TITLE` ([`sheet_title`]), `SCALE`, `DATE`, `DRAWN`, `ADDRESS`, `CLIENT`, `PROJECT`,
//! then `ISSUE1`…`ISSUE6`. A long value is narrowed to fit its cell. Each of the eight cells carries
//! its name (`Project`, `Sheet`…) as small text in its top-left corner, part of the block.
//!
//! Above `PROJECT` is the ISSUES area: a heading row (`No.`, `Date`, `Description`, in the block) over
//! [`ISSUE_ROWS`] rows, the attributes `ISSUE1`…, which hold the latest issues, `ISSUE1` (the bottom
//! row) the oldest shown, each `<n>  <date>  <description>`; unused rows are empty. A sheet lists the
//! set-wide issues and the revisions that touch it: those whose `sheets` name it, and those whose
//! number is the `REV` of a [`DELTA_BLOCK`] Insert in its paper space. The whole strip shrinks by one
//! factor to fit a narrow strip or a short page.
//!
//! `PROJECT` is `LambDocument.sheet_info.project`, or else the document's name; `CLIENT`,
//! `ADDRESS`, `DRAWN` and `DATE` are stored text, empty until set. `SCALE` is the common label of
//! the layout's numbered viewports, `As noted` when they differ, empty when there are none.
//!
//! [`titleblock_observer`] reconciles every layout after every command: a frame that is off is
//! updated in place (same handle), so one moved by hand snaps back; a missing one is added, an extra
//! one erased; and the drawing is written back only when something changed, so the change lands in
//! the command's own undo step. A frame block whose contents are not what its page wants (one made
//! before the labels and the ISSUES area) is rebuilt in place under the same name.
//!
//! `titleblock {project?, client?, address?, drawnBy?, date?, north?}` (`tblock`) sets the given ones
//! in one undo step and returns all six. `north` is project north, degrees clockwise from the
//! project's global +Y, and turns the arrows of plan drawing titles ([`crate::vptitles`]). Typed,
//! `Enter an option [Project/Client/Address/Drawnby/Date/North]:` then the value, looping until Enter.
//!
//! `issue {description, date?}` (`iss`) adds an issue numbered one past the highest so far, dated
//! today (UTC, `YYYY-MM-DD`) unless `date` is given; `issue {remove: n}` removes issue `n`, and the
//! others keep their numbers, and its revision deltas are erased with it. An empty description is
//! refused. `revision: true` makes it a revision, listed only on the sheets it touches; a non-empty
//! `sheets` (LAMB sheet layout names, else refused) implies it. One undo step each; returns
//! `{issues: [{number, date, description, revision, sheets}], message}`. Typed, `Issue description:`
//! then `Date <today>:`.
//!
//! `revdelta {issue, at: [x, y], layout?}` (`rdelta`) marks a change on a sheet: one [`DELTA_BLOCK`]
//! Insert (an 8 mm triangle, apex up, centred on `at`, its `REV` attribute the revision's number) in
//! the paper space of `layout` (default the current layout), on layer [`DELTA_LAYER`], for the
//! drafter to set beside a REVCLOUD. Refused in model space, on a layout that is not a LAMB sheet,
//! for an unknown issue or a set-wide one. One undo step, the frame's new issue row in it; returns
//! `{issue, layout, handle, message}`. Typed, `Revision number <latest>:` then `Specify delta
//! location:`. A delta whose `REV` names no issue is left alone.
//!
//! `generalnote {text}` (`gnote`) appends one of the set's general notes, `{remove: n}` removes note
//! `n` (the rest renumber: a note's number is its position) and `{text, at: n}` replaces it; empty text
//! or a number out of range is refused. One undo step each, returning `{notes, message}`; on a set with
//! a sheet `A001` the `General Notes` table there ([`refresh_notes`], right of the Drawing List) changes
//! in the same step. Typed, `General note:`; an empty answer cancels.

use std::sync::Arc;

use cadcraft_doc::color::Color;
use cadcraft_doc::{Attrib, AttribModes, Block, Common, Drawing, Entity, EntityKind, HAlign, Handle, Insert, Layout, Line, Space, Text, VAlign};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::Vec3;
use cadcraft_render::Sheet;
use lamb_core::geometry::Point2;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AnyView, Id};
use serde_json::{Value, json};

use crate::bom::{self, Refreshed};
use crate::commands::{bad, name_input, other};
use crate::schema::{EXT_KEY, Issue, LambDocument, SheetInfo, read_document, read_entity_data, view_block_name, write_document};
use crate::sheets::{SHEET_MARGIN_MM, frame_strip_width, numbered_viewports, usable_area, viewport_scale};
use crate::sheetset::{LIST_SHEET, LIST_TITLE};
use crate::vptitles::scale_label;

const CMD: &str = "titleblock";
const ISSUE_CMD: &str = "issue";
const NOTE_CMD: &str = "generalnote";
/// The general notes table's title, header and column widths (mm).
pub(crate) const NOTES_TITLE: &str = "General Notes";
const NOTES_HEADER: [&str; 2] = ["No.", "Note"];
const NOTES_COL_WIDTHS: [f64; 2] = [12.0, 160.0];
/// The gap between the Drawing List and the notes table, mm.
const NOTES_GAP_MM: f64 = 10.0;
/// Every frame block's name starts with this.
const FRAME_PREFIX: &str = "LAMB_SHEETFRAME_";
/// The layer frames are placed on.
pub const FRAME_LAYER: &str = "LAMB-SHEETFRAME";
const DELTA_CMD: &str = "revdelta";
/// The revision delta's block, and the layer its Inserts go on.
pub const DELTA_BLOCK: &str = "LAMB_REVDELTA";
pub const DELTA_LAYER: &str = "LAMB-REVISION";
/// The delta triangle's side and its number's text height, paper mm.
const DELTA_SIDE_MM: f64 = 8.0;
const DELTA_TEXT_MM: f64 = 3.5;

/// The strip's cells, bottom up: (tag, cell height mm, text height mm).
const CELLS: [(&str, f64, f64); 8] = [
    ("SHEET", 30.0, 12.0),
    ("TITLE", 20.0, 5.0),
    ("SCALE", 14.0, 3.5),
    ("DATE", 14.0, 3.5),
    ("DRAWN", 14.0, 3.5),
    ("ADDRESS", 14.0, 3.5),
    ("CLIENT", 14.0, 3.5),
    ("PROJECT", 20.0, 5.0),
];
/// How many issues the strip lists: the latest ones.
pub const ISSUE_ROWS: usize = 6;
const ISSUE_TAGS: [&str; ISSUE_ROWS] = ["ISSUE1", "ISSUE2", "ISSUE3", "ISSUE4", "ISSUE5", "ISSUE6"];
/// An issue row's height and text height, mm; the heading row is the same.
const ISSUE_ROW_MM: f64 = 7.0;
const ISSUE_TEXT_MM: f64 = 2.5;
/// The heading's words, each at an offset in characters of issue text, matching `<n>  <date>  `.
const ISSUE_HEADING: [(&str, f64); 3] = [("No.", 0.0), ("Date", 3.0), ("Description", 15.0)];
/// A cell label's text height, and its inset from the cell's top-left corner, mm.
const LABEL_MM: f64 = 1.8;
const LABEL_INSET_MM: f64 = 1.0;
/// The strip width the cells and text heights are sized for, mm; a narrower strip shrinks them.
const FULL_STRIP_MM: f64 = 70.0;
const CELL_PAD_MM: f64 = 3.0;
/// Estimated advance of a character, as a fraction of its height.
const CHAR_ADVANCE: f64 = 0.6;

/// Where a page's frame lies, in paper units.
struct Geometry {
    min: (f64, f64),
    max: (f64, f64),
    strip_x: f64,
    /// Per [`CELLS`] entry: (bottom y, cell height, text height).
    cells: [(f64, f64, f64); CELLS.len()],
    /// Per issue row, bottom up, then the heading row: (bottom y, row height, text height).
    issues: [(f64, f64, f64); ISSUE_ROWS + 1],
    pad: f64,
    /// A cell label's text height and inset.
    label: f64,
    inset: f64,
}

fn printable(sheet: &Sheet) -> cadcraft_geom::Bounds2 {
    if sheet.printable.is_empty() { sheet.bounds() } else { sheet.printable }
}

fn geometry(sheet: &Sheet) -> Geometry {
    let p = printable(sheet);
    let strip = frame_strip_width(sheet);
    let mm = 1.0 / sheet.unit_mm;
    let full_height: f64 = CELLS.iter().map(|c| c.1).sum::<f64>() + (ISSUE_ROWS + 1) as f64 * ISSUE_ROW_MM;
    let k = (strip / (FULL_STRIP_MM * mm)).min(p.height() / (full_height * mm)).min(1.0);
    let mut y = p.min.y;
    let mut row = |cell: f64, text: f64| {
        let c = (y, cell * k * mm, text * k * mm);
        y += c.1;
        c
    };
    let cells = CELLS.map(|(_, cell, text)| row(cell, text));
    let issues = [(); ISSUE_ROWS + 1].map(|()| row(ISSUE_ROW_MM, ISSUE_TEXT_MM));
    Geometry {
        min: (p.min.x, p.min.y),
        max: (p.max.x, p.max.y),
        strip_x: p.max.x - strip,
        cells,
        issues,
        pad: CELL_PAD_MM * k * mm,
        label: LABEL_MM * k * mm,
        inset: LABEL_INSET_MM * k * mm,
    }
}

/// `PROJECT` → `Project`.
fn title_case(tag: &str) -> String {
    let mut c = tag.chars();
    c.next().map_or_else(String::new, |f| f.to_string() + &c.as_str().to_ascii_lowercase())
}

/// The block a page's frame uses; only pages drawn alike share one.
fn block_name(sheet: &Sheet) -> String {
    let u = sheet.unit_mm;
    let (w, h) = (sheet.size.x * u, sheet.size.y * u);
    let p = printable(sheet);
    let margins = [p.min.x * u, p.min.y * u, (sheet.size.x - p.max.x) * u, (sheet.size.y - p.max.y) * u];
    let mut name = format!("{FRAME_PREFIX}{w:.0}X{h:.0}");
    if margins.iter().any(|m| (m - SHEET_MARGIN_MM).abs() > TOLERANCE.coincidence_mm) {
        name.push_str(&format!("_M{:.0}_{:.0}_{:.0}_{:.0}", margins[0], margins[1], margins[2], margins[3]));
    }
    if (u - 1.0).abs() > 1e-9 {
        name.push_str("_IN");
    }
    name
}

/// One attribute a frame wants: (tag, place, text height, width factor, value).
type WantedAttrib = (&'static str, Vec3, f64, f64, String);

/// Every attribute's tag, in order: the cells, then the issue rows.
fn tags() -> impl Iterator<Item = &'static str> {
    CELLS.iter().map(|c| c.0).chain(ISSUE_TAGS)
}

/// One sheet's wanted frame.
#[derive(Clone, Debug, PartialEq)]
struct Wanted {
    block: String,
    attribs: Vec<WantedAttrib>,
}

fn text(p: Vec3, height: f64, width_factor: f64, value: &str) -> Text {
    Text {
        insert: p,
        align_pt: Some(p),
        height,
        value: value.into(),
        rotation: 0.0,
        width_factor,
        oblique: 0.0,
        style: "Standard".into(),
        halign: HAlign::Left,
        valign: VAlign::Baseline,
    }
}

fn attrib(tag: &str, p: Vec3, height: f64, width_factor: f64, value: &str) -> Attrib {
    Attrib {
        tag: tag.into(),
        text: text(p, height, width_factor, value),
        invisible: false,
        constant: false,
        prompt: String::new(),
        props: Default::default(),
        multiline: None,
        modes: AttribModes { lock_position: true, ..AttribModes::default() },
    }
}

/// The frame a sheet wants, its values in [`tags`] order (missing ones empty).
fn wanted(sheet: &Sheet, values: Vec<String>) -> Wanted {
    let g = geometry(sheet);
    let room = g.max.0 - g.strip_x - 2.0 * g.pad;
    // A cell's value is centred in the room below its label; an issue row's in the row.
    let label_zone = g.label + 2.0 * g.inset;
    let baselines = g.cells.iter().map(|(bottom, cell, text)| (*bottom + (cell - label_zone - text) / 2.0, *text));
    let baselines = baselines.chain(g.issues.iter().take(ISSUE_ROWS).map(|(bottom, row, text)| (*bottom + (row - text) / 2.0, *text)));
    let mut values = values.into_iter();
    let attribs = tags()
        .zip(baselines)
        .map(|(tag, (y, text))| {
            let value = values.next().unwrap_or_default();
            let advance = value.chars().count() as f64 * text * CHAR_ADVANCE;
            let width_factor = if advance > room { room / advance } else { 1.0 };
            (tag, Vec3::new(g.strip_x + g.pad, y, 0.0), text, width_factor, value)
        })
        .collect();
    Wanted { block: block_name(sheet), attribs }
}

/// What an issue row says: `<n>  <date>  <description>`.
fn issue_line(i: &Issue) -> String {
    format!("{}  {}  {}", i.number, i.date, i.description)
}

/// The issue rows' values, bottom up: the latest [`ISSUE_ROWS`] issues, oldest first, then empties.
fn issue_values(issues: &[Issue]) -> Vec<String> {
    let mut sorted: Vec<&Issue> = issues.iter().collect();
    sorted.sort_by_key(|i| i.number);
    let shown = sorted.iter().skip(sorted.len().saturating_sub(ISSUE_ROWS));
    let mut out: Vec<String> = shown.map(|i| issue_line(i)).collect();
    out.resize(ISSUE_ROWS, String::new());
    out
}

/// The revision number a delta Insert carries, when `e` is one.
fn delta_rev(e: &Entity) -> Option<u32> {
    match &e.kind {
        EntityKind::Insert(i) if i.block.eq_ignore_ascii_case(DELTA_BLOCK) => {
            i.attribs.iter().find(|a| a.tag.eq_ignore_ascii_case("REV")).and_then(|a| a.text.value.trim().parse().ok())
        }
        _ => None,
    }
}

/// The issues layout `l` lists: the set-wide ones, and the revisions that name it or whose delta it
/// carries.
pub(crate) fn sheet_issues(issues: &[Issue], l: &Layout) -> Vec<Issue> {
    let deltas: Vec<u32> = l.entities.iter().filter_map(|e| delta_rev(e)).collect();
    issues.iter().filter(|i| !i.revision || i.sheets.contains(&l.name) || deltas.contains(&i.number)).cloned().collect()
}

fn insert_of(w: &Wanted) -> Insert {
    Insert {
        clip: None,
        block: w.block.clone(),
        insert: Vec3::new(0.0, 0.0, 0.0),
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: w.attribs.iter().map(|(tag, p, h, wf, v)| attrib(tag, *p, *h, *wf, v)).collect(),
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    }
}

fn frame_of(e: &Entity) -> Option<&Insert> {
    match &e.kind {
        EntityKind::Insert(i) if i.block.to_ascii_uppercase().starts_with(FRAME_PREFIX) => Some(i),
        _ => None,
    }
}

fn near(a: Vec3, b: Vec3) -> bool {
    (a.x - b.x).hypot(a.y - b.y) <= TOLERANCE.coincidence_mm
}

/// Whether `e` is the frame `w` wants, exactly.
fn matches(e: &Entity, i: &Insert, w: &Wanted) -> bool {
    e.common.layer == FRAME_LAYER
        && i.block.eq_ignore_ascii_case(&w.block)
        && near(i.insert, Vec3::new(0.0, 0.0, 0.0))
        && i.scale == Vec3::new(1.0, 1.0, 1.0)
        && i.rotation == 0.0
        && i.attribs.len() == w.attribs.len()
        && i.attribs.iter().zip(&w.attribs).all(|(a, (tag, p, _, _, v))| {
            a.tag == *tag && a.text.value == *v && a.modes.lock_position && near(a.text.align_pt.unwrap_or(a.text.insert), *p)
        })
}

/// A capital letter and three digits.
fn is_sheet_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 4 && b.first().is_some_and(u8::is_ascii_uppercase) && b.iter().skip(1).all(u8::is_ascii_digit)
}

/// What viewport entity `e` names: (view_id, assembly_of), when it names either.
fn named(e: &Entity) -> Option<(Option<Id>, Option<Id>)> {
    let EntityKind::Viewport(_) = &e.kind else { return None };
    let data = read_entity_data(&e.common).ok()?;
    (data.view_id.is_some() || data.assembly_of.is_some()).then_some((data.view_id, data.assembly_of))
}

/// Whether `l` is a LAMB sheet: a viewport names a view or an assembly, or it is named a capital
/// letter and three digits.
pub(crate) fn is_lamb_sheet(l: &Layout) -> bool {
    is_sheet_name(&l.name) || l.entities.iter().any(|e| named(e).is_some())
}

/// What a sheet is called in its title block: one view viewport gives the view's name; one assembly
/// viewport `<view name> framing`; one enlarged viewport `<view name>, enlarged`; several viewports all showing details `Details`, otherwise
/// `Views`; no viewports the title cell of the layout's first table (never the General Notes), else
/// the layout's name.
pub(crate) fn sheet_title(d: &Drawing, doc: &LambDocument, layout: &str) -> String {
    let Some(l) = d.layout(layout) else { return layout.to_owned() };
    let view = |id: Id| doc.views.iter().find(|v| v.id() == id);
    // (the view shown or framed, whether it is an assembly viewport)
    let shown: Vec<(&AnyView, bool)> = l
        .entities
        .iter()
        .filter_map(|e| named(e))
        .filter_map(|(v, a)| match (v, a) {
            (Some(v), _) => view(v).map(|x| (x, false)),
            (None, Some(p)) => view(p).map(|x| (x, true)),
            (None, None) => None,
        })
        .collect();
    let enlarged = l.entities.iter().any(|e| named(e).is_some() && read_entity_data(&e.common).is_ok_and(|x| x.enlarged));
    match shown.as_slice() {
        [] => l
            .entities
            .iter()
            .find_map(|e| match &e.kind {
                // The general notes sit beside the sheet's own table and never name the sheet.
                EntityKind::Table(t) => Some(t.cells.first().and_then(|r| r.first()).map_or("", |c| c.text.as_str())).filter(|t| *t != NOTES_TITLE),
                _ => None,
            })
            .filter(|t| !t.is_empty())
            .unwrap_or(&l.name)
            .to_owned(),
        [(v, _)] if enlarged => format!("{}, enlarged", v.name()),
        [(v, false)] => v.name().to_owned(),
        [(v, true)] => format!("{} framing", v.name()),
        many if many.iter().all(|(v, assembly)| !assembly && matches!(v, AnyView::Detail(_))) => "Details".to_owned(),
        _ => "Views".to_owned(),
    }
}

/// One layout's changes.
#[derive(Debug, Default)]
struct LayoutPlan {
    layout: String,
    remove: Vec<Handle>,
    update: Option<(Handle, Wanted)>,
    add: Option<Wanted>,
}

fn plan(d: &Drawing) -> std::result::Result<Vec<LayoutPlan>, String> {
    if !d.ext.contains_key(EXT_KEY) {
        return Ok(Vec::new());
    }
    let doc = read_document(d).map_err(|e| e.to_string())?;
    let blocks: Vec<(Id, String)> = doc.views.iter().map(|v| (v.id(), view_block_name(v.id()))).collect();
    let numbered = numbered_viewports(d, &blocks);
    let info = doc.sheet_info.clone().unwrap_or_default();
    let project = if info.project.is_empty() { doc.name.clone() } else { info.project.clone() };
    let mut out = Vec::new();
    for l in &d.layouts {
        let mut frames = l.entities.iter().filter(|e| frame_of(e).is_some());
        let is_sheet = is_lamb_sheet(l);
        let Some(sheet) = cadcraft_render::sheet(d, &l.name).filter(|_| is_sheet) else {
            let remove: Vec<Handle> = frames.map(|e| e.handle).collect();
            if !remove.is_empty() {
                out.push(LayoutPlan { layout: l.name.clone(), remove, ..LayoutPlan::default() });
            }
            continue;
        };
        let mut labels: Vec<String> = numbered
            .iter()
            .filter(|(_, _, ln, _)| *ln == l.name)
            .filter_map(|(_, _, _, h)| match l.entities.get(*h).map(|e| &e.kind) {
                Some(EntityKind::Viewport(vp)) => Some(scale_label(viewport_scale(vp))),
                _ => None,
            })
            .collect();
        labels.sort();
        labels.dedup();
        let scale = match labels.as_slice() {
            [] => String::new(),
            [one] => one.clone(),
            _ => "As noted".to_owned(),
        };
        let mut values = vec![
            l.name.clone(),
            sheet_title(d, &doc, &l.name),
            scale,
            info.date.clone(),
            info.drawn_by.clone(),
            info.address.clone(),
            info.client.clone(),
            project.clone(),
        ];
        values.extend(issue_values(&sheet_issues(&info.issues, l)));
        let w = wanted(&sheet, values);
        let stale = d.block(&w.block).is_some_and(|b| !block_is(b, &frame_contents(&sheet)));
        let mut lp = LayoutPlan { layout: l.name.clone(), ..LayoutPlan::default() };
        match frames.next() {
            Some(e) => {
                if let Some(i) = frame_of(e)
                    && (stale || !matches(e, i, &w))
                {
                    lp.update = Some((e.handle, w));
                }
            }
            None => lp.add = Some(w),
        }
        lp.remove = frames.map(|e| e.handle).collect();
        if lp.update.is_some() || lp.add.is_some() || !lp.remove.is_empty() {
            out.push(lp);
        }
    }
    Ok(out)
}

/// What a page's frame block holds: the printable rectangle, the strip's edge, a divider above every
/// cell and issue row, each cell's label in its top-left corner and the ISSUES heading.
fn frame_contents(sheet: &Sheet) -> Vec<EntityKind> {
    let g = geometry(sheet);
    let (x0, y0, x1, y1) = (g.min.0, g.min.1, g.max.0, g.max.1);
    let mut segments =
        vec![((x0, y0), (x1, y0)), ((x1, y0), (x1, y1)), ((x1, y1), (x0, y1)), ((x0, y1), (x0, y0)), ((g.strip_x, y0), (g.strip_x, y1))];
    segments.extend(g.cells.iter().chain(&g.issues).map(|(bottom, cell, _)| ((g.strip_x, bottom + cell), (x1, bottom + cell))));
    let mut out: Vec<EntityKind> =
        segments.into_iter().map(|(a, b)| EntityKind::Line(Line { a: Vec3::new(a.0, a.1, 0.0), b: Vec3::new(b.0, b.1, 0.0) })).collect();
    for ((tag, ..), (bottom, cell, _)) in CELLS.iter().zip(g.cells) {
        let at = Vec3::new(g.strip_x + g.inset, bottom + cell - g.inset - g.label, 0.0);
        out.push(EntityKind::Text(text(at, g.label, 1.0, &title_case(tag))));
    }
    if let Some((bottom, row, h)) = g.issues.last().copied() {
        for (word, chars) in ISSUE_HEADING {
            let at = Vec3::new(g.strip_x + g.pad + chars * h * CHAR_ADVANCE, bottom + (row - h) / 2.0, 0.0);
            out.push(EntityKind::Text(text(at, h, 1.0, word)));
        }
    }
    out
}

/// Whether block `b` holds `want`, within drawing tolerance (so a DXF round trip still matches).
fn block_is(b: &Block, want: &[EntityKind]) -> bool {
    let have: Vec<&EntityKind> = b.entities.iter().map(|e| &e.kind).collect();
    have.len() == want.len()
        && have.iter().zip(want).all(|(h, w)| match (h, w) {
            (EntityKind::Line(h), EntityKind::Line(w)) => near(h.a, w.a) && near(h.b, w.b),
            (EntityKind::Text(h), EntityKind::Text(w)) => {
                h.value == w.value && near(h.insert, w.insert) && (h.height - w.height).abs() <= TOLERANCE.coincidence_mm
            }
            _ => false,
        })
}

/// A page's block definition, on layer `0`: made when missing, rebuilt when it holds anything else.
fn ensure_block(d: &mut Drawing, name: &str, sheet: &Sheet) -> std::result::Result<(), String> {
    let want = frame_contents(sheet);
    if d.block(name).is_some_and(|b| block_is(b, &want)) {
        return Ok(());
    }
    d.blocks.insert(name.into(), Arc::new(Block::new(name)));
    let space = Space::Block(name.into());
    for kind in want {
        d.add(&space, Common { layer: "0".into(), ..Common::default() }, kind).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn apply(d: &mut Drawing, plan: &[LayoutPlan]) -> std::result::Result<(), String> {
    if plan.iter().any(|p| p.add.is_some() || p.update.is_some())
        && d.ensure_layer(FRAME_LAYER)
        && let Some(l) = d.layer_mut(FRAME_LAYER)
    {
        l.plot = true;
        l.color = Color::Index(7);
    }
    for lp in plan {
        if let Some(w) = lp.add.as_ref().or(lp.update.as_ref().map(|u| &u.1)) {
            let sheet = cadcraft_render::sheet(d, &lp.layout).ok_or("layout gone")?;
            ensure_block(d, &w.block, &sheet)?;
        }
        let space = Space::Paper(lp.layout.clone());
        let store = d.space_mut(&space).ok_or("layout gone")?;
        for h in &lp.remove {
            store.remove(*h);
        }
        if let Some((h, w)) = &lp.update {
            store.modify(*h, |e| {
                e.common.layer = FRAME_LAYER.into();
                e.kind = EntityKind::Insert(insert_of(w));
            });
        }
        if let Some(w) = &lp.add {
            let common = Common { layer: FRAME_LAYER.into(), ..Common::default() };
            d.add(&space, common, EntityKind::Insert(insert_of(w))).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// The after-command observer: every sheet's frame made to equal what the project and the sheet
/// say. The drawing is written back only when something changed.
pub fn titleblock_observer(s: &mut Session, _before: &Arc<Drawing>) {
    let todo = match s.doc().map_err(|e| e.to_string()).and_then(plan) {
        Ok(p) if p.is_empty() => return,
        Ok(p) => p,
        Err(e) => {
            s.echo(format!("LAMB sheet frames: {e}"));
            return;
        }
    };
    let Ok(current) = s.doc() else { return };
    let mut after = current.clone();
    if let Err(e) = apply(&mut after, &todo) {
        s.echo(format!("LAMB sheet frames: {e}"));
        return;
    }
    if let Ok(d) = s.doc_mut() {
        *d = after;
    }
}

fn info_json(info: &SheetInfo) -> Value {
    json!({ "project": info.project, "client": info.client, "address": info.address, "drawnBy": info.drawn_by, "date": info.date, "north": info.north_deg })
}

pub(crate) fn titleblock(s: &mut Session, p: &Value) -> Result<Value> {
    let mut doc = read_document(s.doc()?).map_err(other)?;
    let old = doc.sheet_info.clone().unwrap_or_default();
    let mut info = old.clone();
    let fields: [(&str, &mut String); 5] = [
        ("project", &mut info.project),
        ("client", &mut info.client),
        ("address", &mut info.address),
        ("drawnBy", &mut info.drawn_by),
        ("date", &mut info.date),
    ];
    for (key, slot) in fields {
        if let Some(v) = p.get(key).filter(|v| !v.is_null()) {
            let text = v.as_str().ok_or_else(|| bad(CMD, &format!("`{key}` must be a string")))?;
            text.trim().clone_into(slot);
        }
    }
    if let Some(v) = p.get("north").filter(|v| !v.is_null()) {
        let deg = v.as_f64().ok_or_else(|| bad(CMD, "`north` must be a number"))?;
        info.north_deg = deg.rem_euclid(360.0);
    }
    if info != old {
        doc.sheet_info = Some(info.clone());
        write_document(s.doc_mut()?, &doc).map_err(other)?;
    }
    Ok(info_json(&info))
}

/// Today, UTC, `YYYY-MM-DD`.
fn today() -> String {
    let now = crate::maintain::now();
    now.get(..10).unwrap_or(&now).to_owned()
}

fn issues_json(issues: &[Issue]) -> Value {
    Value::Array(
        issues
            .iter()
            .map(|i| json!({ "number": i.number, "date": i.date, "description": i.description, "revision": i.revision, "sheets": i.sheets }))
            .collect(),
    )
}

/// An optional string parameter, trimmed; absent or null is empty.
fn text_param(p: &Value, key: &str) -> Result<String> {
    match p.get(key).filter(|v| !v.is_null()) {
        Some(v) => Ok(v.as_str().ok_or_else(|| bad(ISSUE_CMD, &format!("`{key}` must be a string")))?.trim().to_owned()),
        None => Ok(String::new()),
    }
}

/// The `sheets` parameter: LAMB sheet layout names, each one checked.
fn sheets_param(d: &Drawing, p: &Value) -> Result<Vec<String>> {
    let Some(v) = p.get("sheets").filter(|v| !v.is_null()) else { return Ok(Vec::new()) };
    let list = v.as_array().ok_or_else(|| bad(ISSUE_CMD, "`sheets` must be a list of sheet names"))?;
    let mut out: Vec<String> = Vec::new();
    for n in list {
        let name = n.as_str().map(str::trim).ok_or_else(|| bad(ISSUE_CMD, "`sheets` must be a list of sheet names"))?;
        if !d.layout(name).is_some_and(is_lamb_sheet) {
            return Err(bad(ISSUE_CMD, &format!("{name} is not a LAMB sheet")));
        }
        if !out.iter().any(|o| o == name) {
            out.push(name.to_owned());
        }
    }
    Ok(out)
}

/// Erases every delta Insert numbered `n`, on every layout.
fn erase_deltas(d: &mut Drawing, n: u32) {
    let found: Vec<(String, Handle)> =
        d.layouts.iter().flat_map(|l| l.entities.iter().filter(|e| delta_rev(e) == Some(n)).map(|e| (l.name.clone(), e.handle))).collect();
    for (layout, h) in found {
        if let Some(store) = d.space_mut(&Space::Paper(layout)) {
            store.remove(h);
        }
    }
}

pub(crate) fn issue(s: &mut Session, p: &Value) -> Result<Value> {
    let mut doc = read_document(s.doc()?).map_err(other)?;
    let mut info = doc.sheet_info.clone().unwrap_or_default();
    // Build on a copy and assign it back once: one undo step, erased deltas in it.
    let mut d = s.doc()?.clone();
    let message = if let Some(v) = p.get("remove").filter(|v| !v.is_null()) {
        let n = v.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(|| bad(ISSUE_CMD, "`remove` must be an issue number"))?;
        let before = info.issues.len();
        info.issues.retain(|i| i.number != n);
        if info.issues.len() == before {
            return Err(bad(ISSUE_CMD, &format!("there is no issue {n}")));
        }
        erase_deltas(&mut d, n);
        format!("Issue {n} removed.")
    } else {
        let description = text_param(p, "description")?;
        if description.is_empty() {
            return Err(bad(ISSUE_CMD, "an issue needs a description"));
        }
        let date = text_param(p, "date")?;
        let date = if date.is_empty() { today() } else { date };
        let sheets = sheets_param(&d, p)?;
        let revision = match p.get("revision").filter(|v| !v.is_null()) {
            Some(v) => v.as_bool().ok_or_else(|| bad(ISSUE_CMD, "`revision` must be true or false"))?,
            None => false,
        } || !sheets.is_empty();
        let number = info.issues.iter().map(|i| i.number).max().unwrap_or(0) + 1;
        let word = if revision { "Revision" } else { "Issue" };
        let m = format!("{word} {number} added: {date} {description}.");
        info.issues.push(Issue { number, date, description, revision, sheets });
        m
    };
    doc.sheet_info = Some(info.clone());
    write_document(&mut d, &doc).map_err(other)?;
    *s.doc_mut()? = d;
    Ok(json!({ "issues": issues_json(&info.issues), "message": message }))
}

/// The delta block: an equilateral triangle, apex up, centred (its centroid) on the origin, on layer `0`.
fn ensure_delta_block(d: &mut Drawing) -> std::result::Result<(), String> {
    if d.block(DELTA_BLOCK).is_some() {
        return Ok(());
    }
    d.blocks.insert(DELTA_BLOCK.into(), Arc::new(Block::new(DELTA_BLOCK)));
    let space = Space::Block(DELTA_BLOCK.into());
    let (half, h) = (DELTA_SIDE_MM / 2.0, DELTA_SIDE_MM * 3f64.sqrt() / 2.0);
    let corners = [Vec3::new(-half, -h / 3.0, 0.0), Vec3::new(half, -h / 3.0, 0.0), Vec3::new(0.0, 2.0 * h / 3.0, 0.0)];
    for k in 0..3 {
        let (a, b) = (corners[k], corners[(k + 1) % 3]);
        d.add(&space, Common { layer: "0".into(), ..Common::default() }, EntityKind::Line(Line { a, b })).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn delta_insert(at: Vec3, n: u32) -> Insert {
    let mut a = attrib("REV", at, DELTA_TEXT_MM, 1.0, &n.to_string());
    a.text.halign = HAlign::Center;
    a.text.valign = VAlign::Middle;
    Insert {
        clip: None,
        block: DELTA_BLOCK.into(),
        insert: at,
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: vec![a],
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    }
}

pub(crate) fn revdelta(s: &mut Session, p: &Value) -> Result<Value> {
    let n = p
        .get("issue")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| bad(DELTA_CMD, "`issue` must be an issue number"))?;
    let at = p.get("at").and_then(Value::as_array).filter(|a| a.len() == 2).and_then(|a| Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?)));
    let (x, y) = at.ok_or_else(|| bad(DELTA_CMD, "`at` must be [x, y]"))?;
    let layout = match p.get("layout").filter(|v| !v.is_null()) {
        Some(v) => v.as_str().ok_or_else(|| bad(DELTA_CMD, "`layout` must be a layout name"))?.trim().to_owned(),
        None => match s.layout_space() {
            Space::Paper(name) => name,
            _ => return Err(bad(DELTA_CMD, "a revision delta goes on a sheet: switch to its layout or give `layout`")),
        },
    };
    if !s.doc()?.layout(&layout).is_some_and(is_lamb_sheet) {
        return Err(bad(DELTA_CMD, &format!("{layout} is not a LAMB sheet")));
    }
    let info = read_document(s.doc()?).map_err(other)?.sheet_info.unwrap_or_default();
    let issue = info.issues.iter().find(|i| i.number == n).ok_or_else(|| bad(DELTA_CMD, &format!("there is no issue {n}")))?;
    if !issue.revision {
        return Err(bad(DELTA_CMD, &format!("Issue {n} is set-wide; give it sheets or revision: true")));
    }
    let mut d = s.doc()?.clone();
    ensure_delta_block(&mut d).map_err(other)?;
    if d.ensure_layer(DELTA_LAYER)
        && let Some(l) = d.layer_mut(DELTA_LAYER)
    {
        l.plot = true;
    }
    let common = Common { layer: DELTA_LAYER.into(), ..Common::default() };
    let handle = d.add(&Space::Paper(layout.clone()), common, EntityKind::Insert(delta_insert(Vec3::new(x, y, 0.0), n))).map_err(other)?;
    *s.doc_mut()? = d;
    Ok(json!({ "issue": n, "layout": layout, "handle": handle.hex(), "message": format!("Revision {n} delta on {layout}.") }))
}

/// The latest revision's number, if any.
fn latest_revision(s: &Session) -> Option<u32> {
    let doc = read_document(s.doc().ok()?).ok()?;
    doc.sheet_info?.issues.iter().filter(|i| i.revision).map(|i| i.number).max()
}

/// REVDELTA at the command line: the revision (the latest by default), then the delta's location.
struct RevDelta {
    latest: u32,
    issue: Option<u32>,
}

impl Interactive for RevDelta {
    fn name(&self) -> &'static str {
        DELTA_CMD
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.issue {
            None => Prompt::new("Revision number", Accept::TEXT).default(self.latest.to_string()),
            Some(_) => Prompt::new("Specify delta location", Accept::POINT),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(n) = self.issue else {
            let Some(text) = name_input(i, &self.latest.to_string()) else { return Ok(Step::Continue) };
            match text.parse::<u32>() {
                Ok(n) => self.issue = Some(n),
                Err(_) => s.echo("Requires a revision number.".to_owned()),
            }
            return Ok(Step::Continue);
        };
        let Input::Point(p) = i else { return Ok(Step::Continue) };
        let r = revdelta(s, &json!({ "issue": n, "at": [p.x, p.y] }))?;
        if let Some(m) = r["message"].as_str() {
            s.echo(m.to_owned());
        }
        Ok(Step::Done)
    }
}

pub(crate) fn revdelta_machine(s: &Session) -> Result<Box<dyn Interactive>> {
    let latest = latest_revision(s).ok_or_else(|| other("There is no revision to mark: add one with ISSUE (revision or sheets)."))?;
    Ok(Box::new(RevDelta { latest, issue: None }))
}

/// ISSUE at the command line: the description, then the date (today by default).
struct IssueMachine {
    description: Option<String>,
}

impl Interactive for IssueMachine {
    fn name(&self) -> &'static str {
        ISSUE_CMD
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.description {
            None => Prompt::new("Issue description", Accept::TEXT),
            Some(_) => Prompt::new("Date", Accept::TEXT).default(today()),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(description) = self.description.clone() else {
            let Some(text) = name_input(i, "") else { return Ok(Step::Continue) };
            if text.is_empty() {
                return Ok(Step::Cancel);
            }
            self.description = Some(text);
            return Ok(Step::Continue);
        };
        let Some(date) = name_input(i, &today()) else { return Ok(Step::Continue) };
        let r = issue(s, &json!({ "description": description, "date": date }))?;
        if let Some(m) = r["message"].as_str() {
            s.echo(m.to_owned());
        }
        Ok(Step::Done)
    }
}

pub(crate) fn issue_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(IssueMachine { description: None }))
}

fn notes_json(notes: &[String]) -> Value {
    Value::Array(notes.iter().map(|n| Value::String(n.clone())).collect())
}

/// Note number `key` as a position in `notes`: 1-based, in range.
fn note_index(p: &Value, key: &str, notes: &[String]) -> Result<Option<usize>> {
    let Some(v) = p.get(key).filter(|v| !v.is_null()) else { return Ok(None) };
    let n = v.as_u64().ok_or_else(|| bad(NOTE_CMD, &format!("`{key}` must be a note number")))?;
    match usize::try_from(n).ok().filter(|n| (1..=notes.len()).contains(n)) {
        Some(n) => Ok(Some(n - 1)),
        None => Err(bad(NOTE_CMD, &format!("there is no note {n}"))),
    }
}

pub(crate) fn generalnote(s: &mut Session, p: &Value) -> Result<Value> {
    let mut doc = read_document(s.doc()?).map_err(other)?;
    let mut info = doc.sheet_info.clone().unwrap_or_default();
    let message = if let Some(k) = note_index(p, "remove", &info.notes)? {
        info.notes.remove(k);
        format!("Note {} removed.", k + 1)
    } else {
        let text = p
            .get("text")
            .filter(|v| !v.is_null())
            .map_or(Ok(String::new()), |v| v.as_str().map(|t| t.trim().to_owned()).ok_or_else(|| bad(NOTE_CMD, "`text` must be a string")))?;
        if text.is_empty() {
            return Err(bad(NOTE_CMD, "a note needs text"));
        }
        match note_index(p, "at", &info.notes)? {
            Some(k) => {
                if let Some(slot) = info.notes.get_mut(k) {
                    *slot = text;
                }
                format!("Note {} replaced.", k + 1)
            }
            None => {
                info.notes.push(text);
                format!("Note {} added.", info.notes.len())
            }
        }
    };
    doc.sheet_info = Some(info.clone());
    // Build on a copy and assign it back once: one undo step, the table in it.
    let mut d = s.doc()?.clone();
    write_document(&mut d, &doc).map_err(other)?;
    refresh_notes(&mut d)?;
    *s.doc_mut()? = d;
    Ok(json!({ "notes": notes_json(&info.notes), "message": message }))
}

/// Brings `A001`'s General Notes table up to the set's notes: right of the Drawing List when new, at
/// its own corner otherwise; with no notes an existing table is removed. Without `A001` nothing is
/// drawn.
pub(crate) fn refresh_notes(d: &mut Drawing) -> Result<Option<Refreshed>> {
    if d.layout(LIST_SHEET).is_none() || !d.ext.contains_key(EXT_KEY) {
        return Ok(None);
    }
    let notes = read_document(d).map_err(other)?.sheet_info.unwrap_or_default().notes;
    if notes.is_empty() {
        return Ok(bom::remove_table(d, LIST_SHEET, NOTES_TITLE).then_some(Refreshed::Updated));
    }
    let beside = bom::titled_table(d, LIST_SHEET, LIST_TITLE)
        .map(|t| Point2 { x: t.insert.x + t.col_widths.iter().sum::<f64>() + NOTES_GAP_MM, y: t.insert.y });
    let default_at = match beside {
        Some(at) => at,
        None => {
            let sheet = cadcraft_render::sheet(d, LIST_SHEET).ok_or_else(|| other("A001 has no page"))?;
            let area = usable_area(&sheet);
            Point2 { x: area.min.x, y: area.max.y }
        }
    };
    let rows: Vec<Vec<String>> = notes.iter().enumerate().map(|(k, n)| vec![(k + 1).to_string(), n.clone()]).collect();
    bom::refresh_table(d, LIST_SHEET, NOTES_TITLE, Some(default_at), &|at| {
        bom::schedule_table(NOTES_TITLE, &NOTES_HEADER, &NOTES_COL_WIDTHS, &rows, at)
    })
}

/// GENERALNOTE at the command line: the note's text; an empty answer cancels.
struct GeneralNote;

impl Interactive for GeneralNote {
    fn name(&self) -> &'static str {
        NOTE_CMD
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("General note", Accept::TEXT)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(text) = name_input(i, "") else { return Ok(Step::Continue) };
        if text.is_empty() {
            return Ok(Step::Cancel);
        }
        let r = generalnote(s, &json!({ "text": text }))?;
        if let Some(m) = r["message"].as_str() {
            s.echo(m.to_owned());
        }
        Ok(Step::Done)
    }
}

pub(crate) fn generalnote_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(GeneralNote))
}

/// (keyword, parameter, prompt).
const FIELDS: [(&str, &str, &str); 6] = [
    ("Project", "project", "Enter project"),
    ("Client", "client", "Enter client"),
    ("Address", "address", "Enter address"),
    ("Drawnby", "drawnBy", "Enter drawn by"),
    ("Date", "date", "Enter date"),
    ("North", "north", "Enter project north, degrees clockwise from +Y"),
];

/// TITLEBLOCK at the command line: the option, then its text, looping until Enter.
struct TitleBlock {
    /// The field being typed: (parameter, prompt).
    field: Option<(&'static str, &'static str)>,
}

fn stored(s: &Session, key: &str) -> String {
    s.doc()
        .ok()
        .and_then(|d| read_document(d).ok())
        .and_then(|doc| {
            let info = info_json(&doc.sheet_info.unwrap_or_default());
            let v = info.get(key)?;
            v.as_str().map(str::to_owned).or_else(|| v.as_f64().map(|n| n.to_string()))
        })
        .unwrap_or_default()
}

impl Interactive for TitleBlock {
    fn name(&self) -> &'static str {
        CMD
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match self.field {
            None => Prompt::new("Enter an option", Accept::TEXT).kw(&FIELDS.map(|f| f.0)),
            Some((key, text)) => {
                let now = stored(s, key);
                let p = Prompt::new(text, Accept::TEXT);
                if now.is_empty() { p } else { p.default(now) }
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some((key, _)) = self.field else {
            let picked = match i {
                Input::Enter => return Ok(Step::Done),
                Input::Text(t) | Input::Keyword(t) => self.prompt(s).match_keyword(&t),
                _ => return Ok(Step::Continue),
            };
            let field = picked.and_then(|k| FIELDS.iter().find(|f| f.0 == k)).ok_or_else(|| other("Invalid option keyword."))?;
            self.field = Some((field.1, field.2));
            return Ok(Step::Continue);
        };
        let now = stored(s, key);
        let Some(text) = name_input(i, &now) else { return Ok(Step::Continue) };
        self.field = None;
        if text != now {
            let done = if key == "north" {
                text.parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite())
                    .ok_or_else(|| other("Requires a number."))
                    .and_then(|n| titleblock(s, &json!({ key: n })))
            } else {
                titleblock(s, &json!({ key: text }))
            };
            if let Err(e) = done {
                s.echo(e.to_string());
            }
        }
        Ok(Step::Continue)
    }
}

pub(crate) fn titleblock_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(TitleBlock { field: None }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assembly::tests::{room_with, undo_len};
    use cadcraft_doc::{Layout, PageSetup, Table, Viewport};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    fn id(r: &Value) -> T<Id> {
        Ok(Id(r["viewId"].as_u64().ok_or("no id")?))
    }

    /// `lambproject` (`Ground Floor`) and an offset plan `First`, observers installed.
    fn two_plans() -> T<Session> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let ground = id(&s.execute("lambproject", &json!({}))?)?;
        s.execute("newoffsetplan", &json!({ "mode": "offset", "fromViewId": ground.0, "floorToFloorMm": 3000, "name": "First" }))?;
        Ok(s)
    }

    /// assembly.rs's room (plan `Ground`, detail `D1`) and an empty detail `D2`, observers installed.
    fn room() -> T<(Session, Id, Id, Id)> {
        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        crate::maintain::install(&mut s);
        let d1 = read_document(s.doc()?)?.views.iter().find(|v| v.id() != plan).ok_or("no detail")?.id();
        let d2 = id(&s.execute("newdetail", &json!({ "name": "D2" }))?)?;
        Ok((s, plan, d1, d2))
    }

    /// Layout `name`'s frames.
    fn frames(s: &Session, name: &str) -> T<Vec<(Handle, Insert, String)>> {
        let l = s.doc()?.layout(name).ok_or("no layout")?;
        Ok(l.entities.iter().filter_map(|e| Some((e.handle, frame_of(e)?.clone(), e.common.layer.clone()))).collect())
    }

    fn val(i: &Insert, tag: &str) -> String {
        i.attribs.iter().find(|a| a.tag == tag).map_or_else(|| "-".to_owned(), |a| a.text.value.clone())
    }

    /// The one frame's (SHEET, TITLE, SCALE, PROJECT).
    fn four(s: &Session, name: &str) -> T<(String, String, String, String)> {
        let f = frames(s, name)?;
        assert_eq!(f.len(), 1, "{name}: {f:?}");
        let i = &f[0].1;
        Ok((val(i, "SHEET"), val(i, "TITLE"), val(i, "SCALE"), val(i, "PROJECT")))
    }

    fn strs(a: &str, b: &str, c: &str, d: &str) -> (String, String, String, String) {
        (a.into(), b.into(), c.into(), d.into())
    }

    #[test]
    fn sheetset_frames_each_sheet_in_the_commands_undo_step() -> R {
        let mut s = two_plans()?;
        let name = read_document(s.doc()?)?.name;
        let n = undo_len(&s)?;
        s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!(four(&s, "A101")?, strs("A101", "Ground Floor", "1:50", &name));
        assert_eq!(four(&s, "A102")?, strs("A102", "First", "1:50", &name));

        let f = frames(&s, "A101")?;
        let i = &f[0].1;
        assert_eq!(f[0].2, FRAME_LAYER);
        assert_eq!((i.insert.x, i.insert.y), (0.0, 0.0));
        assert_eq!(i.attribs.iter().map(|a| a.tag.as_str()).collect::<Vec<_>>(), tags().collect::<Vec<_>>());
        assert!(i.attribs.iter().all(|a| a.modes.lock_position && !a.invisible));
        assert!(i.attribs.windows(2).all(|p| p[0].text.insert.y < p[1].text.insert.y), "bottom up");
        assert!(i.attribs[0].text.height > i.attribs[1].text.height, "SHEET is largest");
        assert_eq!(i.block, "LAMB_SHEETFRAME_914X610");
        // The printable rectangle, the strip's edge, eight cell and seven issue dividers, eight
        // labels and three heading words; one definition.
        let def = s.doc()?.block(&i.block).ok_or("no definition")?;
        assert_eq!(def.entities.len(), 5 + 8 + 7 + 8 + 3);
        assert_eq!(frames(&s, "A102")?[0].1.block, i.block);
        let layer = s.doc()?.layer(FRAME_LAYER).ok_or("no layer")?;
        assert!(layer.plot && layer.color == Color::Index(7));
        s.undo()?;
        assert!(s.doc()?.layout("A101").is_none());
        Ok(())
    }

    #[test]
    fn titles_and_scales_follow_what_the_sheet_shows() -> R {
        let (mut s, plan, d1, d2) = room()?;
        s.execute("newsheet", &json!({ "name": "A501", "viewIds": [d1.0, d2.0] }))?;
        let f = four(&s, "A501")?;
        assert_eq!((f.1.as_str(), f.2.as_str()), ("Details", "As noted"));
        s.execute("newsheet", &json!({ "name": "S1", "viewIds": [plan.0, d2.0] }))?;
        let f = four(&s, "S1")?;
        assert_eq!((f.0.as_str(), f.1.as_str(), f.2.as_str()), ("S1", "Views", "1:50"), "a named viewport frames any layout; one scale");
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        s.execute("newsheet", &json!({ "name": "S2", "assemblyIds": [plan.0] }))?;
        let f = four(&s, "S2")?;
        assert_eq!((f.1.as_str(), f.2.as_str()), ("Ground framing", ""));
        s.execute("newsheet", &json!({ "name": "S3", "viewIds": [d1.0], "assemblyIds": [plan.0] }))?;
        assert_eq!(four(&s, "S3")?.1, "Views");
        Ok(())
    }

    #[test]
    fn a_fresh_sheets_viewports_clear_the_strip() -> R {
        for paper in ["ARCH D", "ANSI B", "A4"] {
            let (mut s, plan, d1, d2) = room()?;
            s.execute("sheetsize", &json!({ "paper": paper }))?;
            s.execute("newsheet", &json!({ "name": "A501", "viewIds": [plan.0, d1.0, d2.0] }))?;
            let sheet = cadcraft_render::sheet(s.doc()?, "A501").ok_or("no sheet")?;
            let (g, area) = (geometry(&sheet), usable_area(&sheet));
            assert!((g.max.0 - g.strip_x - 70.0_f64.min(0.25 * sheet.printable.width())).abs() < 1e-9, "{paper}");
            assert!((area.max.x + 5.0 - g.strip_x).abs() < 1e-9 && (area.min.x - 5.0 - sheet.printable.min.x).abs() < 1e-9, "{paper}");
            let mut n = 0;
            for e in s.doc()?.layout("A501").ok_or("no layout")?.entities.iter() {
                if let EntityKind::Viewport(vp) = &e.kind {
                    assert!(vp.center.x + vp.width / 2.0 <= area.max.x, "{paper}: {vp:?}");
                    assert!(vp.center.x - vp.width / 2.0 >= area.min.x, "{paper}: {vp:?}");
                    n += 1;
                }
            }
            assert_eq!(n, 3, "{paper}");
            // The strip's cells fit the page, and a long value is narrowed to fit its cell.
            assert!(g.issues[ISSUE_ROWS].0 + g.issues[ISSUE_ROWS].1 <= g.max.1 + 1e-9, "{paper}");
            s.execute("titleblock", &json!({ "address": "x".repeat(200) }))?;
            let i = frames(&s, "A501")?.remove(0).1;
            let a = i.attribs.iter().find(|a| a.tag == "ADDRESS").ok_or("no ADDRESS")?;
            let width = 200.0 * a.text.height * CHAR_ADVANCE * a.text.width_factor;
            assert!(a.text.insert.x + width <= g.max.0, "{paper}");
        }
        Ok(())
    }

    /// The frame block's texts: (value, insert).
    fn block_texts(s: &Session, block: &str) -> T<Vec<(String, Vec3, f64)>> {
        let def = s.doc()?.block(block).ok_or("no definition")?;
        Ok(def
            .entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Text(t) => Some((t.value.clone(), t.insert, t.height)),
                _ => None,
            })
            .collect())
    }

    #[test]
    fn issues_number_up_and_every_sheet_lists_the_latest_six() -> R {
        let mut s = two_plans()?;
        s.execute("sheetset", &json!({}))?;
        let n = undo_len(&s)?;
        for (k, d) in ["Issued for review", "Issued for permit", "Issued for tender"].iter().enumerate() {
            let r = s.execute("issue", &json!({ "description": d, "date": format!("2026-11-0{}", k + 1) }))?;
            assert_eq!(r["issues"].as_array().map(Vec::len), Some(k + 1));
            assert_eq!(r["issues"][k]["number"], json!(k + 1));
        }
        assert_eq!(undo_len(&s)?, n + 3, "one undo step each, the frames in it");
        for name in ["A101", "A102"] {
            let i = frames(&s, name)?.remove(0).1;
            assert_eq!(
                ["ISSUE1", "ISSUE2", "ISSUE3", "ISSUE4"].map(|t| val(&i, t)),
                ["1  2026-11-01  Issued for review", "2  2026-11-02  Issued for permit", "3  2026-11-03  Issued for tender", ""].map(str::to_owned),
                "{name}"
            );
        }
        let handle = frames(&s, "A101")?[0].0;
        s.undo()?;
        assert_eq!(val(&frames(&s, "A101")?[0].1, "ISSUE3"), "", "undo takes the row with it");
        s.redo()?;

        // Removing keeps the others' numbers; the next one is one past the highest.
        let r = s.execute("issue", &json!({ "remove": 2 }))?;
        assert_eq!(r["issues"].as_array().map(|a| a.iter().map(|i| i["number"].clone()).collect::<Vec<_>>()), Some(vec![json!(1), json!(3)]));
        let i = frames(&s, "A101")?.remove(0).1;
        assert_eq!(val(&i, "ISSUE2"), "3  2026-11-03  Issued for tender");
        assert_eq!(val(&i, "ISSUE3"), "");
        assert!(s.execute("issue", &json!({ "remove": 2 })).is_err(), "no issue 2 now");
        let r = s.execute("issue", &json!({ "description": "Rev" }))?;
        assert_eq!(r["issues"][2]["number"], json!(4));
        let today = r["issues"][2]["date"].as_str().unwrap_or_default().to_owned();
        assert!(today.len() == 10 && today.as_bytes()[4] == b'-' && today.as_bytes()[7] == b'-', "{today}");

        // Removing the latest frees its number.
        s.execute("issue", &json!({ "remove": 4 }))?;
        let r = s.execute("issue", &json!({ "description": "Again", "date": "d" }))?;
        assert_eq!(r["issues"][2]["number"], json!(4));

        // Eight issues: the strip shows 3…8, oldest at the bottom.
        let mut e = two_plans()?;
        e.execute("sheetset", &json!({}))?;
        for k in 1..=8 {
            e.execute("issue", &json!({ "description": format!("I{k}"), "date": "d" }))?;
        }
        let i = frames(&e, "A102")?.remove(0).1;
        assert_eq!(ISSUE_TAGS.map(|t| val(&i, t)), ["3  d  I3", "4  d  I4", "5  d  I5", "6  d  I6", "7  d  I7", "8  d  I8"].map(str::to_owned));
        assert_eq!(frames(&s, "A101")?[0].0, handle, "updated in place");

        // An empty description is refused and changes nothing.
        let n = undo_len(&s)?;
        assert!(s.execute("issue", &json!({ "description": "  " })).is_err());
        assert!(s.execute("issue", &json!({})).is_err());
        assert_eq!(undo_len(&s)?, n);
        Ok(())
    }

    #[test]
    fn cells_carry_labels_above_their_values() -> R {
        let mut s = two_plans()?;
        s.execute("sheetset", &json!({}))?;
        let i = frames(&s, "A101")?.remove(0).1;
        let texts = block_texts(&s, &i.block)?;
        for (tag, ..) in CELLS {
            let label = title_case(tag);
            let (_, at, h) = texts.iter().find(|t| t.0 == label).ok_or(format!("no label {label}"))?;
            let a = i.attribs.iter().find(|a| a.tag == tag).ok_or("no attrib")?;
            assert!(a.text.insert.y + a.text.height < at.y, "{tag}: the value sits below its label");
            assert!((h - LABEL_MM).abs() < 1e-9, "{tag}");
        }
        for word in ["No.", "Date", "Description"] {
            let heading = |t: &&(String, Vec3, f64)| t.0 == word && (t.2 - ISSUE_TEXT_MM).abs() < 1e-9;
            let (_, at, _) = texts.iter().find(heading).ok_or(format!("no heading {word}"))?;
            let top = i.attribs.iter().map(|a| a.text.insert.y).fold(f64::MIN, f64::max);
            assert!(at.y > top, "{word}: the heading is above every row");
        }
        assert_eq!(title_case("ADDRESS"), "Address");
        Ok(())
    }

    #[test]
    fn an_old_frame_block_is_rebuilt_on_the_next_command() -> R {
        let mut s = two_plans()?;
        s.execute("sheetset", &json!({}))?;
        let (handle, i, _) = frames(&s, "A101")?.remove(0);
        // As T634 left it: lines only, and no issue attributes.
        let d = s.doc_mut()?;
        let name = i.block.clone();
        let lines: Vec<EntityKind> =
            d.block(&name).ok_or("no def")?.entities.iter().map(|e| e.kind.clone()).filter(|k| matches!(k, EntityKind::Line(_))).take(13).collect();
        d.blocks.insert(name.clone(), Arc::new(Block::new(&name)));
        for k in lines {
            d.add(&Space::Block(name.clone()), Common::default(), k)?;
        }
        for l in ["A101", "A102"] {
            let h = d.layout(l).ok_or("no layout")?.entities.iter().find(|e| frame_of(e).is_some()).ok_or("no frame")?.handle;
            d.space_mut(&Space::Paper(l.into())).ok_or("no space")?.modify(h, |e| {
                if let EntityKind::Insert(i) = &mut e.kind {
                    i.attribs.truncate(CELLS.len());
                }
            });
        }
        assert_eq!(s.doc()?.block(&name).map(|b| b.entities.len()), Some(13));
        s.execute("line", &json!({ "points": [[9000, 9000], [9500, 9000]] }))?;
        assert_eq!(s.doc()?.block(&name).map(|b| b.entities.len()), Some(31), "rebuilt");
        let (h, i, _) = frames(&s, "A101")?.remove(0);
        assert_eq!(h, handle, "same Insert");
        assert_eq!(i.attribs.len(), CELLS.len() + ISSUE_ROWS);
        assert!(super::plan(s.doc()?)?.is_empty(), "settled");
        Ok(())
    }

    #[test]
    fn a_small_page_shrinks_the_whole_strip_to_fit() -> R {
        // ARCH A portrait is narrow (the strip shrinks with it); A5 landscape is short.
        for (paper, landscape) in [("ARCH A", false), ("A5", true)] {
            let (mut s, _, d1, _) = room()?;
            s.execute("newsheet", &json!({ "name": "A501", "viewIds": [d1.0] }))?;
            s.execute("pagesetup", &json!({ "layout": "A501", "paper": paper, "landscape": landscape }))?;
            let sheet = cadcraft_render::sheet(s.doc()?, "A501").ok_or("no sheet")?;
            let g = geometry(&sheet);
            let top = g.issues[ISSUE_ROWS];
            assert!(top.0 + top.1 <= g.max.1 + 1e-9, "{paper}: fits the printable height");
            assert!(top.1 < ISSUE_ROW_MM / sheet.unit_mm, "{paper}: shrunk");
            assert!((g.cells[0].1 / top.1 - CELLS[0].1 / ISSUE_ROW_MM).abs() < 1e-9, "{paper}: one factor for all");
            let i = frames(&s, "A501")?.remove(0).1;
            assert_eq!(i.attribs.len(), CELLS.len() + ISSUE_ROWS);
            assert!(i.attribs.iter().all(|a| a.text.insert.y < g.max.1), "{paper}");
        }
        Ok(())
    }

    #[test]
    fn typed_issue_takes_a_description_then_a_date() -> R {
        let mut s = two_plans()?;
        s.execute("sheetset", &json!({}))?;
        s.start("iss")?;
        assert_eq!(s.current_prompt().map(|p| p.message.clone()).as_deref(), Some("Issue description"));
        s.input(Input::Text("Issued for permit".into()))?;
        let today = s.current_prompt().and_then(|p| p.default).ok_or("no default date")?;
        assert_eq!(today.len(), 10);
        s.input(Input::Enter)?;
        assert!(s.current_prompt().is_none());
        assert_eq!(val(&frames(&s, "A101")?[0].1, "ISSUE1"), format!("1  {today}  Issued for permit"));
        s.start("issue")?;
        s.input(Input::Text("Rev".into()))?;
        s.input(Input::Text("2027-01-02".into()))?;
        assert_eq!(val(&frames(&s, "A102")?[0].1, "ISSUE2"), "2  2027-01-02  Rev");
        Ok(())
    }

    #[test]
    fn which_layouts_get_a_frame() -> R {
        let (mut s, ..) = room()?;
        let d = s.doc_mut()?;
        for (name, tab) in [("Plain", 9), ("A001", 10), ("a101", 11), ("A1011", 12), ("A601", 13)] {
            d.layouts.push(Layout { page: PageSetup::default(), ..Layout::new(name, tab) });
        }
        let vp = Viewport { center: Vec3::new(100.0, 100.0, 0.0), width: 100.0, height: 80.0, view_height: 4000.0, id: 50, ..Viewport::default() };
        d.add(&Space::Paper("Plain".into()), Common::default(), EntityKind::Viewport(vp))?;
        let table = crate::bom::bom_table(&[], Point2 { x: 50.0, y: 50.0 });
        d.add(&Space::Paper("A601".into()), Common::default(), EntityKind::Table(table))?;
        s.execute("line", &json!({ "points": [[9000, 9000], [9500, 9000]] }))?;
        for name in ["Plain", "a101", "A1011"] {
            assert!(frames(&s, name)?.is_empty(), "{name}");
        }
        assert_eq!(four(&s, "A001")?.1, "A001", "a viewport-less sheet takes its own name");
        assert_eq!(four(&s, "A601")?.1, "Framing BOM", "its first table's title");
        assert_eq!(four(&s, "A601")?.2, "");
        Ok(())
    }

    #[test]
    fn a_drawing_with_no_lamb_document_gets_no_frame() -> R {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        s.doc_mut()?.layouts.push(Layout { page: PageSetup::default(), ..Layout::new("A101", 9) });
        s.execute("line", &json!({ "points": [[0, 0], [500, 0]] }))?;
        assert!(frames(&s, "A101")?.is_empty());
        Ok(())
    }

    #[test]
    fn titleblock_updates_every_frame_in_one_undo_step() -> R {
        let mut s = two_plans()?;
        s.execute("sheetset", &json!({}))?;
        let n = undo_len(&s)?;
        let r = s.execute("titleblock", &json!({ "client": " X " }))?;
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        assert_eq!(r, json!({ "project": "", "client": "X", "address": "", "drawnBy": "", "date": "", "north": 0.0 }));
        let handle = frames(&s, "A101")?[0].0;
        for name in ["A101", "A102"] {
            assert_eq!(val(&frames(&s, name)?[0].1, "CLIENT"), "X", "{name}");
        }

        // The same value changes nothing; a bad one is refused.
        s.execute("titleblock", &json!({ "client": "X" }))?;
        assert_eq!(undo_len(&s)?, n + 1, "no undo step");
        assert!(s.execute("titleblock", &json!({ "client": 3 })).is_err());
        assert!(s.execute("titleblock", &json!({ "north": "up" })).is_err());

        s.execute("titleblock", &json!({ "project": "P", "address": "A", "drawnBy": "D", "date": "2026-11-05" }))?;
        let i = frames(&s, "A102")?.remove(0).1;
        assert_eq!(["PROJECT", "ADDRESS", "DRAWN", "DATE", "CLIENT"].map(|t| val(&i, t)), ["P", "A", "D", "2026-11-05", "X"].map(str::to_owned));
        assert_eq!(frames(&s, "A101")?[0].0, handle, "updated in place");
        s.undo()?;
        s.undo()?;
        assert_eq!(val(&frames(&s, "A101")?[0].1, "CLIENT"), "");
        Ok(())
    }

    #[test]
    fn frames_follow_the_project_and_snap_back() -> R {
        let (mut s, _, d1, d2) = room()?;
        s.execute("newsheet", &json!({ "name": "A501", "viewIds": [d1.0] }))?;
        let (handle, ..) = frames(&s, "A501")?.remove(0);
        s.execute("viewprops", &json!({ "mode": "rename", "viewId": d1.0, "name": "Eaves" }))?;
        assert_eq!(four(&s, "A501")?.1, "Eaves");

        // An unrelated command changes nothing and adds only its own undo step.
        let before = frames(&s, "A501")?;
        let n = undo_len(&s)?;
        s.execute("line", &json!({ "points": [[9000, 9000], [9500, 9000]] }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        assert_eq!(frames(&s, "A501")?, before);

        // Moved by hand it snaps back; erased it returns.
        s.execute("move", &json!({ "handles": [handle.hex()], "delta": [40, 40] }))?;
        assert_eq!(frames(&s, "A501")?, before);
        s.execute("erase", &json!({ "handles": [handle.hex()] }))?;
        assert_eq!(frames(&s, "A501")?.len(), 1);

        // A new page takes the frame with it.
        s.execute("pagesetup", &json!({ "layout": "A501", "paper": "ANSI B", "landscape": true }))?;
        assert_eq!(frames(&s, "A501")?[0].1.block, "LAMB_SHEETFRAME_432X279");

        // Other margins are another definition.
        s.execute("newsheet", &json!({ "name": "A502", "viewIds": [d2.0] }))?;
        s.execute("pagesetup", &json!({ "layout": "A502", "margins": [5, 5, 5, 5] }))?;
        assert_eq!(frames(&s, "A502")?[0].1.block, "LAMB_SHEETFRAME_914X610_M5_5_5_5");
        Ok(())
    }

    #[test]
    fn a_dxf_round_trip_keeps_the_frame() -> R {
        let (mut s, plan, d1, _) = room()?;
        s.execute("newsheet", &json!({ "name": "A501", "viewIds": [plan.0, d1.0] }))?;
        s.execute("titleblock", &json!({ "client": "Client & Co", "date": "x".repeat(200) }))?;
        s.execute("issue", &json!({ "description": "Issued for permit", "date": "2026-11-08" }))?;
        let bytes = cadcraft_io::write(s.doc()?, "t.dxf").map_err(|e| format!("{e:?}"))?;
        let back = cadcraft_io::read(&bytes, "t.dxf").map_err(|e| format!("{e:?}"))?;
        let p = super::plan(&back)?;
        assert!(p.is_empty(), "the observer would change nothing after a round trip: {p:?}");
        let l = back.layout("A501").ok_or("no layout")?;
        assert_eq!(l.entities.iter().filter(|e| frame_of(e).is_some()).count(), 1);
        let issues = read_document(&back)?.sheet_info.ok_or("no info")?.issues;
        assert_eq!(issues, vec![Issue { number: 1, date: "2026-11-08".into(), description: "Issued for permit".into(), ..Issue::default() }]);
        Ok(())
    }

    #[test]
    fn typed_titleblock_loops_until_enter() -> R {
        let mut s = two_plans()?;
        s.execute("sheetset", &json!({}))?;
        s.start("tblock")?;
        assert_eq!(s.current_prompt().map(|p| p.keywords.len()), Some(6));
        s.input(Input::Text("cl".into()))?;
        s.input(Input::Text("Acme".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.keywords.len()), Some(6), "back at the options");
        s.input(Input::Text("Da".into()))?;
        s.input(Input::Enter)?;
        s.input(Input::Enter)?;
        assert!(s.current_prompt().is_none(), "Enter at the option prompt ends it");
        assert_eq!(val(&frames(&s, "A101")?[0].1, "CLIENT"), "Acme");
        assert_eq!(val(&frames(&s, "A101")?[0].1, "DATE"), "", "Enter keeps the text");
        s.start("titleblock")?;
        s.input(Input::Text("Client".into()))?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("Acme"));
        s.input(Input::Enter)?;
        s.input(Input::Text("North".into()))?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("0"));
        s.input(Input::Text("-30".into()))?;
        s.input(Input::Text("North".into()))?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("330"), "stored 0..360");
        s.input(Input::Text("up".into()))?;
        s.input(Input::Text("North".into()))?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("330"), "not a number changes nothing");
        Ok(())
    }

    /// A001's General Notes rows, `(number, text)`, and the table's corner; `None` without one.
    fn notes_table(s: &Session) -> T<Option<(Vec<(String, String)>, Vec3)>> {
        Ok(bom::titled_table(s.doc()?, LIST_SHEET, NOTES_TITLE).map(|t: &Table| {
            let rows = t.cells.iter().skip(2).map(|r| (r[0].text.clone(), r[1].text.clone())).collect();
            (rows, t.insert)
        }))
    }

    fn rows(texts: &[&str]) -> Vec<(String, String)> {
        texts.iter().enumerate().map(|(k, t)| ((k + 1).to_string(), (*t).to_owned())).collect()
    }

    #[test]
    fn sheetset_writes_the_notes_beside_the_drawing_list() -> R {
        let mut s = two_plans()?;
        for t in ["Verify all dimensions on site.", "Do not scale.", "Framing to NZS 3604."] {
            s.execute("generalnote", &json!({ "text": t }))?;
        }
        assert!(s.doc()?.layout(LIST_SHEET).is_none(), "no A001 yet: stored only");
        let r = s.execute("sheetset", &json!({}))?;
        assert!(r["message"].as_str().is_some_and(|m| m.contains("A001 General Notes")), "{r}");
        let (got, at) = notes_table(&s)?.ok_or("no notes table")?;
        assert_eq!(got, rows(&["Verify all dimensions on site.", "Do not scale.", "Framing to NZS 3604."]));
        let list = bom::titled_table(s.doc()?, LIST_SHEET, LIST_TITLE).ok_or("no list")?;
        let list_right = list.insert.x + list.col_widths.iter().sum::<f64>();
        assert!((at.x - list_right - NOTES_GAP_MM).abs() < 1e-9 && (at.y - list.insert.y).abs() < 1e-9, "right of the list, tops aligned");
        let t = bom::titled_table(s.doc()?, LIST_SHEET, NOTES_TITLE).ok_or("no notes")?;
        assert_eq!(t.cells[1].iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), NOTES_HEADER);
        assert_eq!(t.col_widths, NOTES_COL_WIDTHS);
        let area = usable_area(&cadcraft_render::sheet(s.doc()?, LIST_SHEET).ok_or("no sheet")?);
        assert!(at.x + NOTES_COL_WIDTHS.iter().sum::<f64>() <= area.max.x, "inside the usable area");
        assert_eq!(four(&s, LIST_SHEET)?.1, LIST_TITLE, "the frame still names the list");

        let n = undo_len(&s)?;
        let before = notes_table(&s)?;
        let r = s.execute("sheetset", &json!({}))?;
        assert_eq!(undo_len(&s)?, n, "a second run changes nothing: {r}");
        assert_eq!(notes_table(&s)?, before);
        Ok(())
    }

    #[test]
    fn generalnote_edits_the_table_in_its_own_undo_step() -> R {
        let mut s = two_plans()?;
        s.execute("sheetset", &json!({}))?;
        assert!(notes_table(&s)?.is_none(), "no notes, no table");
        let n = undo_len(&s)?;
        for (k, t) in ["A", "B", "C"].iter().enumerate() {
            let r = s.execute("generalnote", &json!({ "text": t }))?;
            assert_eq!(r["message"], json!(format!("Note {} added.", k + 1)));
        }
        assert_eq!(undo_len(&s)?, n + 3);
        assert_eq!(notes_table(&s)?.ok_or("no table")?.0, rows(&["A", "B", "C"]));
        let corner = notes_table(&s)?.ok_or("no table")?.1;

        let r = s.execute("generalnote", &json!({ "remove": 2 }))?;
        assert_eq!(r, json!({ "notes": ["A", "C"], "message": "Note 2 removed." }));
        assert_eq!(notes_table(&s)?.ok_or("no table")?, (rows(&["A", "C"]), corner), "renumbered, in place");
        let r = s.execute("generalnote", &json!({ "text": " Z ", "at": 1 }))?;
        assert_eq!(r["message"], json!("Note 1 replaced."));
        assert_eq!(notes_table(&s)?.ok_or("no table")?.0, rows(&["Z", "C"]));
        s.undo()?;
        assert_eq!(notes_table(&s)?.ok_or("no table")?.0, rows(&["A", "C"]), "undo takes the table with it");

        // Refusals record nothing.
        let n = undo_len(&s)?;
        for p in [
            json!({ "text": "  " }),
            json!({}),
            json!({ "remove": 3 }),
            json!({ "remove": 0 }),
            json!({ "text": "x", "at": 9 }),
            json!({ "text": 4 }),
        ] {
            assert!(s.execute("generalnote", &p).is_err(), "{p}");
        }
        assert_eq!(undo_len(&s)?, n);

        s.execute("generalnote", &json!({ "remove": 1 }))?;
        s.execute("generalnote", &json!({ "remove": 1 }))?;
        assert!(notes_table(&s)?.is_none(), "the last note takes the table");
        assert!(bom::titled_table(s.doc()?, LIST_SHEET, LIST_TITLE).is_some());
        Ok(())
    }

    #[test]
    fn notes_without_a001_are_kept_and_survive_dxf() -> R {
        let (mut s, ..) = room()?;
        s.execute("generalnote", &json!({ "text": "Client & Co approval required." }))?;
        s.execute("generalnote", &json!({ "text": "Second" }))?;
        assert!(s.doc()?.layout(LIST_SHEET).is_none());
        let bytes = cadcraft_io::write(s.doc()?, "t.dxf").map_err(|e| format!("{e:?}"))?;
        let back = cadcraft_io::read(&bytes, "t.dxf").map_err(|e| format!("{e:?}"))?;
        let notes = read_document(&back)?.sheet_info.ok_or("no info")?.notes;
        assert_eq!(notes, vec!["Client & Co approval required.".to_owned(), "Second".to_owned()]);
        Ok(())
    }

    #[test]
    fn typed_generalnote_adds_a_note_and_empty_cancels() -> R {
        let mut s = two_plans()?;
        s.execute("sheetset", &json!({}))?;
        s.start("gnote")?;
        assert_eq!(s.current_prompt().map(|p| p.message.clone()).as_deref(), Some("General note"));
        s.input(Input::Text("Do not scale.".into()))?;
        assert!(s.current_prompt().is_none());
        assert_eq!(notes_table(&s)?.ok_or("no table")?.0, rows(&["Do not scale."]));
        let n = undo_len(&s)?;
        s.start("generalnote")?;
        s.input(Input::Enter)?;
        assert!(s.current_prompt().is_none(), "empty cancels");
        assert_eq!(undo_len(&s)?, n);
        Ok(())
    }

    /// Layout `name`'s revision deltas: (handle, Insert, layer).
    fn deltas(s: &Session, name: &str) -> T<Vec<(Handle, Insert, String)>> {
        let l = s.doc()?.layout(name).ok_or("no layout")?;
        Ok(l.entities
            .iter()
            .filter(|e| delta_rev(e).is_some())
            .filter_map(|e| match &e.kind {
                EntityKind::Insert(i) => Some((e.handle, i.clone(), e.common.layer.clone())),
                _ => None,
            })
            .collect())
    }

    fn row(s: &Session, name: &str, tag: &str) -> T<String> {
        Ok(val(&frames(s, name)?.remove(0).1, tag))
    }

    /// Two sheets A101 and A102, issue 1 set-wide, revision 2 on A102, revision 3 on no sheet.
    fn revised() -> T<Session> {
        let mut s = two_plans()?;
        s.execute("sheetset", &json!({}))?;
        s.execute("issue", &json!({ "description": "Issued for permit", "date": "d1" }))?;
        let r = s.execute("issue", &json!({ "description": "Rev A", "date": "d2", "sheets": ["A102"] }))?;
        assert_eq!(r["message"], json!("Revision 2 added: d2 Rev A."));
        assert_eq!((r["issues"][1]["revision"].clone(), r["issues"][1]["sheets"].clone()), (json!(true), json!(["A102"])));
        s.execute("issue", &json!({ "description": "Rev B", "date": "d3", "revision": true }))?;
        Ok(s)
    }

    #[test]
    fn a_revision_lists_only_on_the_sheets_it_touches() -> R {
        let mut s = revised()?;
        assert_eq!((row(&s, "A101", "ISSUE1")?, row(&s, "A101", "ISSUE2")?), ("1  d1  Issued for permit".to_owned(), String::new()));
        assert_eq!(row(&s, "A102", "ISSUE2")?, "2  d2  Rev A");
        assert_eq!(row(&s, "A102", "ISSUE3")?, "", "revision 3 names no sheet and has no delta");

        // A delta lists revision 3 on A101 alone, in revdelta's own undo step.
        let n = undo_len(&s)?;
        let r = s.execute("revdelta", &json!({ "issue": 3, "at": [100, 120], "layout": "A101" }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        assert_eq!(r["message"], json!("Revision 3 delta on A101."));
        assert_eq!(row(&s, "A101", "ISSUE2")?, "3  d3  Rev B");
        assert_eq!(row(&s, "A102", "ISSUE3")?, "");
        let d = deltas(&s, "A101")?;
        assert_eq!(d.len(), 1);
        let (h, i, layer) = &d[0];
        assert_eq!(r["handle"], json!(h.hex()));
        assert_eq!((i.block.as_str(), layer.as_str(), (i.insert.x, i.insert.y)), (DELTA_BLOCK, DELTA_LAYER, (100.0, 120.0)));
        assert_eq!(i.attribs.iter().map(|a| (a.tag.as_str(), a.text.value.as_str())).collect::<Vec<_>>(), vec![("REV", "3")]);
        assert_eq!(i.scale, Vec3::new(1.0, 1.0, 1.0));
        let def = s.doc()?.block(DELTA_BLOCK).ok_or("no delta block")?;
        let lines: Vec<&Line> = def.entities.iter().filter_map(|e| if let EntityKind::Line(l) = &e.kind { Some(l) } else { None }).collect();
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| ((l.a.x - l.b.x).hypot(l.a.y - l.b.y) - DELTA_SIDE_MM).abs() < 1e-9), "equilateral, 8 mm");
        let (cx, cy) = lines.iter().fold((0.0, 0.0), |(x, y), l| (x + l.a.x / 3.0, y + l.a.y / 3.0));
        assert!(cx.abs() < 1e-9 && cy.abs() < 1e-9, "centred on the insert point");
        assert!(lines.iter().any(|l| l.a.x.abs() < 1e-9 && l.a.y > 0.0), "apex up");
        assert!(s.doc()?.layer(DELTA_LAYER).is_some_and(|l| l.plot));

        // Undo takes the row with it; erasing the delta drops it again.
        s.undo()?;
        assert_eq!(row(&s, "A101", "ISSUE2")?, "");
        s.redo()?;
        assert_eq!(row(&s, "A101", "ISSUE2")?, "3  d3  Rev B");

        // A DXF round trip keeps the issues, the delta and the listing.
        let bytes = cadcraft_io::write(s.doc()?, "t.dxf").map_err(|e| format!("{e:?}"))?;
        let back = cadcraft_io::read(&bytes, "t.dxf").map_err(|e| format!("{e:?}"))?;
        assert!(super::plan(&back)?.is_empty(), "settled after a round trip");
        let issues = read_document(&back)?.sheet_info.ok_or("no info")?.issues;
        assert_eq!(issues, read_document(s.doc()?)?.sheet_info.ok_or("no info")?.issues);
        assert!(issues[1].revision && issues[1].sheets == ["A102"] && issues[2].revision && !issues[0].revision);
        let l = back.layout("A101").ok_or("no layout")?;
        assert_eq!(l.entities.iter().filter_map(|e| delta_rev(e)).collect::<Vec<_>>(), vec![3]);

        let handle = h.hex();
        s.execute("erase", &json!({ "handles": [handle] }))?;
        assert_eq!(row(&s, "A101", "ISSUE2")?, "", "erased, unlisted");
        s.undo()?;
        assert_eq!(deltas(&s, "A101")?.len(), 1);

        // Removing the revision erases its delta in the same undo step.
        let n = undo_len(&s)?;
        s.execute("issue", &json!({ "remove": 3 }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        assert!(deltas(&s, "A101")?.is_empty());
        s.undo()?;
        assert_eq!(deltas(&s, "A101")?.len(), 1, "undo restores it");
        Ok(())
    }

    #[test]
    fn revision_refusals_record_nothing() -> R {
        let mut s = revised()?;
        let n = undo_len(&s)?;
        for p in [
            json!({ "description": "x", "sheets": ["Nope"] }),
            json!({ "description": "x", "sheets": "A101" }),
            json!({ "description": "x", "revision": 1 }),
        ] {
            assert!(s.execute("issue", &p).is_err(), "{p}");
        }
        let set_wide = s.execute("revdelta", &json!({ "issue": 1, "at": [0, 0], "layout": "A101" }));
        assert!(set_wide.is_err_and(|e| e.to_string().contains("Issue 1 is set-wide")));
        for p in [
            json!({ "issue": 9, "at": [0, 0], "layout": "A101" }),
            json!({ "issue": 3, "at": [0, 0], "layout": "Nope" }),
            json!({ "issue": 3, "at": [0, 0] }),
            json!({ "issue": 3, "layout": "A101" }),
        ] {
            assert!(s.execute("revdelta", &p).is_err(), "{p}");
        }
        assert_eq!(undo_len(&s)?, n);
        assert!(s.doc()?.block(DELTA_BLOCK).is_none());
        Ok(())
    }

    #[test]
    fn typed_revdelta_places_the_latest_revision_at_a_click() -> R {
        let mut s = two_plans()?;
        s.execute("sheetset", &json!({}))?;
        s.execute("layout", &json!({ "option": "set", "name": "A101" }))?;
        s.execute("issue", &json!({ "description": "Issued for permit", "date": "d1" }))?;
        assert!(s.start("rdelta").is_err(), "no revision to mark");
        assert!(s.current_prompt().is_none());
        s.execute("issue", &json!({ "description": "Rev A", "date": "d2", "revision": true }))?;
        s.start("revdelta")?;
        let p = s.current_prompt().ok_or("no prompt")?;
        assert_eq!((p.message.as_str(), p.default.as_deref()), ("Revision number", Some("2")));
        s.input(Input::Enter)?;
        assert_eq!(s.current_prompt().map(|p| p.message.clone()).as_deref(), Some("Specify delta location"));
        s.input(Input::Point(cadcraft_geom::Vec2::new(50.0, 60.0)))?;
        assert!(s.current_prompt().is_none());
        let d = deltas(&s, "A101")?;
        assert_eq!((d.len(), d[0].1.insert.x, d[0].1.insert.y), (1, 50.0, 60.0));
        assert_eq!(row(&s, "A101", "ISSUE2")?, "2  d2  Rev A");
        assert_eq!(row(&s, "A102", "ISSUE2")?, "");
        Ok(())
    }
}

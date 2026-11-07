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
//! text), `TITLE` ([`sheet_title`]), `SCALE`, `DATE`, `DRAWN`, `ADDRESS`, `CLIENT`, `PROJECT`. A
//! long value is narrowed to fit its cell.
//!
//! `PROJECT` is `LambDocument.sheet_info.project`, or else the document's name; `CLIENT`,
//! `ADDRESS`, `DRAWN` and `DATE` are stored text, empty until set. `SCALE` is the common label of
//! the layout's numbered viewports, `As noted` when they differ, empty when there are none.
//!
//! [`titleblock_observer`] reconciles every layout after every command: a frame that is off is
//! updated in place (same handle), so one moved by hand snaps back; a missing one is added, an extra
//! one erased; and the drawing is written back only when something changed, so the change lands in
//! the command's own undo step.
//!
//! `titleblock {project?, client?, address?, drawnBy?, date?, north?}` (`tblock`) sets the given ones
//! in one undo step and returns all six. `north` is project north, degrees clockwise from the
//! project's global +Y, and turns the arrows of plan drawing titles ([`crate::vptitles`]). Typed,
//! `Enter an option [Project/Client/Address/Drawnby/Date/North]:` then the value, looping until Enter.

use std::sync::Arc;

use cadcraft_doc::color::Color;
use cadcraft_doc::{Attrib, AttribModes, Block, Common, Drawing, Entity, EntityKind, HAlign, Handle, Insert, Layout, Line, Space, Text, VAlign};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::Vec3;
use cadcraft_render::Sheet;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AnyView, Id};
use serde_json::{Value, json};

use crate::commands::{bad, name_input, other};
use crate::schema::{EXT_KEY, LambDocument, SheetInfo, read_document, read_entity_data, view_block_name, write_document};
use crate::sheets::{SHEET_MARGIN_MM, frame_strip_width, numbered_viewports, viewport_scale};
use crate::vptitles::scale_label;

const CMD: &str = "titleblock";
/// Every frame block's name starts with this.
const FRAME_PREFIX: &str = "LAMB_SHEETFRAME_";
/// The layer frames are placed on.
pub const FRAME_LAYER: &str = "LAMB-SHEETFRAME";

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
    pad: f64,
}

fn printable(sheet: &Sheet) -> cadcraft_geom::Bounds2 {
    if sheet.printable.is_empty() { sheet.bounds() } else { sheet.printable }
}

fn geometry(sheet: &Sheet) -> Geometry {
    let p = printable(sheet);
    let strip = frame_strip_width(sheet);
    let mm = 1.0 / sheet.unit_mm;
    let full_height: f64 = CELLS.iter().map(|c| c.1).sum();
    let k = (strip / (FULL_STRIP_MM * mm)).min(p.height() / (full_height * mm)).min(1.0);
    let mut y = p.min.y;
    let cells = CELLS.map(|(_, cell, text)| {
        let c = (y, cell * k * mm, text * k * mm);
        y += c.1;
        c
    });
    Geometry { min: (p.min.x, p.min.y), max: (p.max.x, p.max.y), strip_x: p.max.x - strip, cells, pad: CELL_PAD_MM * k * mm }
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

/// One sheet's wanted frame.
#[derive(Clone, Debug, PartialEq)]
struct Wanted {
    block: String,
    attribs: Vec<WantedAttrib>,
}

fn attrib(tag: &str, p: Vec3, height: f64, width_factor: f64, value: &str) -> Attrib {
    Attrib {
        tag: tag.into(),
        text: Text {
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
        },
        invisible: false,
        constant: false,
        prompt: String::new(),
        props: Default::default(),
        multiline: None,
        modes: AttribModes { lock_position: true, ..AttribModes::default() },
    }
}

/// The frame a sheet wants, its values in [`CELLS`] order.
fn wanted(sheet: &Sheet, values: [String; CELLS.len()]) -> Wanted {
    let g = geometry(sheet);
    let room = g.max.0 - g.strip_x - 2.0 * g.pad;
    let attribs = CELLS
        .iter()
        .zip(g.cells)
        .zip(values)
        .map(|((c, (bottom, cell, text)), value)| {
            let advance = value.chars().count() as f64 * text * CHAR_ADVANCE;
            let width_factor = if advance > room { room / advance } else { 1.0 };
            (c.0, Vec3::new(g.strip_x + g.pad, bottom + (cell - text) / 2.0, 0.0), text, width_factor, value)
        })
        .collect();
    Wanted { block: block_name(sheet), attribs }
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
/// `Views`; no viewports the title cell of the layout's first table, else the layout's name.
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
                EntityKind::Table(t) => Some(t.cells.first().and_then(|r| r.first()).map_or("", |c| c.text.as_str())),
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
        let w = wanted(
            &sheet,
            [
                l.name.clone(),
                sheet_title(d, &doc, &l.name),
                scale,
                info.date.clone(),
                info.drawn_by.clone(),
                info.address.clone(),
                info.client.clone(),
                project.clone(),
            ],
        );
        let mut lp = LayoutPlan { layout: l.name.clone(), ..LayoutPlan::default() };
        match frames.next() {
            Some(e) => {
                if let Some(i) = frame_of(e)
                    && !matches(e, i, &w)
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

/// A page's block definition: the printable rectangle, the strip's edge and its dividers, on layer
/// `0`.
fn ensure_block(d: &mut Drawing, name: &str, sheet: &Sheet) -> std::result::Result<(), String> {
    if d.block(name).is_some() {
        return Ok(());
    }
    d.blocks.insert(name.into(), Arc::new(Block::new(name)));
    let space = Space::Block(name.into());
    let g = geometry(sheet);
    let (x0, y0, x1, y1) = (g.min.0, g.min.1, g.max.0, g.max.1);
    let mut segments =
        vec![((x0, y0), (x1, y0)), ((x1, y0), (x1, y1)), ((x1, y1), (x0, y1)), ((x0, y1), (x0, y0)), ((g.strip_x, y0), (g.strip_x, y1))];
    segments.extend(g.cells.iter().map(|(bottom, cell, _)| ((g.strip_x, bottom + cell), (x1, bottom + cell))));
    for (a, b) in segments {
        let line = Line { a: Vec3::new(a.0, a.1, 0.0), b: Vec3::new(b.0, b.1, 0.0) };
        d.add(&space, Common { layer: "0".into(), ..Common::default() }, EntityKind::Line(line)).map_err(|e| e.to_string())?;
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
    use crate::sheets::usable_area;
    use cadcraft_doc::{Layout, PageSetup, Viewport};
    use lamb_core::geometry::Point2;

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
        assert_eq!(i.attribs.iter().map(|a| a.tag.as_str()).collect::<Vec<_>>(), CELLS.map(|c| c.0));
        assert!(i.attribs.iter().all(|a| a.modes.lock_position && !a.invisible));
        assert!(i.attribs.windows(2).all(|p| p[0].text.insert.y < p[1].text.insert.y), "bottom up");
        assert!(i.attribs[0].text.height > i.attribs[1].text.height, "SHEET is largest");
        assert_eq!(i.block, "LAMB_SHEETFRAME_914X610");
        // The printable rectangle, the strip's edge and the eight dividers, one definition.
        let def = s.doc()?.block(&i.block).ok_or("no definition")?;
        assert_eq!(def.entities.len(), 13);
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
            assert!(g.cells[7].0 + g.cells[7].1 <= g.max.1 + 1e-9, "{paper}");
            s.execute("titleblock", &json!({ "address": "x".repeat(200) }))?;
            let i = frames(&s, "A501")?.remove(0).1;
            let a = i.attribs.iter().find(|a| a.tag == "ADDRESS").ok_or("no ADDRESS")?;
            let width = 200.0 * a.text.height * CHAR_ADVANCE * a.text.width_factor;
            assert!(a.text.insert.x + width <= g.max.0, "{paper}");
        }
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
        let bytes = cadcraft_io::write(s.doc()?, "t.dxf").map_err(|e| format!("{e:?}"))?;
        let back = cadcraft_io::read(&bytes, "t.dxf").map_err(|e| format!("{e:?}"))?;
        let p = super::plan(&back)?;
        assert!(p.is_empty(), "the observer would change nothing after a round trip: {p:?}");
        let l = back.layout("A501").ok_or("no layout")?;
        assert_eq!(l.entities.iter().filter(|e| frame_of(e).is_some()).count(), 1);
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
}

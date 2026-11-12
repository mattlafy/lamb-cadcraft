//! Views as blocks (`cadcraft-integration.md` §4, insertion regime): a LAMB view is a block named
//! [`view_block_name`]`(id)` holding its entities, a layer of the same name, and one canonical
//! INSERT of that block in model space. The canonical insert is the one whose handle equals the
//! view id — identity marks it, no extra field.

use std::fmt;
use std::sync::Arc;

use cadcraft_doc::{Block, Common, Drawing, Entity, EntityKind, Handle, Insert, Space};
use cadcraft_geom::Vec3;
use lamb_core::model::{AnyView, Id, Placement};

use crate::LambDocument;
use crate::schema::{SchemaError, read_document, view_block_name, write_document};

/// Why a view could not be created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewError {
    /// The drawing's `ext["LAMB"]` document could not be read or written.
    Schema(SchemaError),
    /// A block of the view's name already exists (a handle reused across drawings).
    BlockExists(String),
}

impl fmt::Display for ViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ViewError::Schema(e) => e.fmt(f),
            ViewError::BlockExists(n) => write!(f, "block {n} already exists"),
        }
    }
}

impl std::error::Error for ViewError {}

impl From<SchemaError> for ViewError {
    fn from(e: SchemaError) -> Self {
        ViewError::Schema(e)
    }
}

fn set_id(view: &mut AnyView, id: Id) {
    match view {
        AnyView::Plan(v) => v.id = id,
        AnyView::Vertical(v) => v.id = id,
        AnyView::Detail(v) => v.id = id,
    }
}

/// Creates `view` in `d`: allocates a handle `h`, gives the view id `Id(h.0)` (its own id and
/// entities are ignored), adds the layer and empty block `LAMB_VIEW_<id>`, a model-space INSERT of
/// that block at the origin (scale 1, rotation 0) on that layer **with handle `h`**, and appends
/// the view to the LAMB document. Returns the id. Nothing is changed on error.
pub fn create_view(d: &mut Drawing, mut view: AnyView) -> Result<Id, ViewError> {
    let mut doc = read_document(d)?;
    let h: Handle = d.new_handle();
    let id = Id(h.0);
    let name = view_block_name(id);
    if d.block(&name).is_some() {
        return Err(ViewError::BlockExists(name));
    }
    set_id(&mut view, id);
    doc.views.push(view);
    write_document(d, &doc)?;
    d.ensure_layer(&name);
    d.blocks.insert(name.clone(), Arc::new(Block::new(&name)));
    let insert = Insert {
        clip: None,
        block: name.clone(),
        insert: Vec3::ZERO,
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: Vec::new(),
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    };
    d.model.push(Entity { handle: h, common: Common { layer: name, ..Common::default() }, kind: EntityKind::Insert(insert) });
    Ok(id)
}

/// The Views palette's tabs (LAMB T659), as `(name, text columns, icon columns)`: [`VIEWS_TAB`]
/// lists Model and the views, [`SHEETS_TAB`] the sheets in plot order.
pub const BROWSER_TABS: [(&str, &[&str], &[&str]); 2] =
    [("Views", &["Level", "Sheet"], &["Ref", "Underlay"]), ("Sheets", &["Title"], &["Up", "Down"])];
/// The index of the Views tab in [`BROWSER_TABS`].
pub const VIEWS_TAB: usize = 0;
/// The index of the Sheets tab in [`BROWSER_TABS`].
pub const SHEETS_TAB: usize = 1;

/// One icon cell of a [`BrowserRow`] (`cadcraft_ui_egui::viewlist::Cell` without the icon, which
/// the column picks): `on` draws it full-strength; a click runs `command`.
#[derive(Clone, Debug, PartialEq)]
pub struct BrowserCell {
    pub on: bool,
    pub tip: String,
    pub command: (String, serde_json::Value),
}

/// One row of the desktop app's Views palette (`cadcraft_ui_egui::viewlist::Row`, same fields):
/// the TS app's Views panel (`ViewsPanel.tsx`) as data. `tab` indexes [`BROWSER_TABS`]; `fields`
/// and `cells` hold one entry per text and icon column of that tab, a cell `None` where that mode
/// is not valid for the row.
#[derive(Clone, Debug, PartialEq)]
pub struct BrowserRow {
    pub label: String,
    pub active: bool,
    pub heading: bool,
    pub tab: usize,
    pub fields: Vec<String>,
    pub command: Option<(String, serde_json::Value)>,
    pub cells: Vec<Option<BrowserCell>>,
}

fn row(label: String, active: bool, command: &str, params: serde_json::Value) -> BrowserRow {
    BrowserRow { label, active, heading: false, tab: VIEWS_TAB, fields: Vec::new(), command: Some((command.to_owned(), params)), cells: Vec::new() }
}

fn heading(label: &str) -> BrowserRow {
    BrowserRow { label: label.to_owned(), active: false, heading: true, tab: VIEWS_TAB, fields: Vec::new(), command: None, cells: Vec::new() }
}

fn cell(on: bool, tip: String, command: &str, params: serde_json::Value) -> Option<BrowserCell> {
    Some(BrowserCell { on, tip, command: (command.to_owned(), params) })
}

/// The Views tab's groups, in order: heading and the views' kind word ([`crate::commands::kind_word`]).
const GROUPS: [(&str, &str); 4] = [("Plans", "Plan"), ("Sections", "Section"), ("Elevations", "Elevation"), ("Details", "Detail")];

/// The Views palette for `d` while editing `edit`. On [`VIEWS_TAB`]: `Model`, then the views
/// grouped under `Plans`, `Sections`, `Elevations` and `Details` headings (an empty group has
/// none), in document order inside a group; each row is labelled with the view's name, the edited
/// one active, each running `editview`; a drawing with no views offers `Start LAMB project`
/// (LAMBPROJECT). A view row's fields are **Level** ([`level_field`]) and **Sheet** (the layout the
/// view is numbered on, else empty), and its two cells, `None` unless the mode is valid for it while
/// editing a view: **Ref** for the edited view's eligible reference views — on for the one the shown
/// pane displays (at most one), clicking an off one runs `refpane {viewId}` (which also turns the
/// pane on), the on one `refpane {on: false}`; **Underlay** for its underlay candidates — on while
/// it is one of the underlays (any number), a click runs `underlay {viewId}` (toggle), the tip
/// noting `(mirrored)`. Cell tips name the view as `"<name> — <kind>"`. The edited view's own row
/// has neither. On [`SHEETS_TAB`]: one row per LAMB sheet in tab order, which is plot order,
/// labelled with the layout name, field **Title** (`titleblock::sheet_title`), the current one
/// active, each running `layout set`, with two cells that run `sheetorder` by a step: move up,
/// `None` on the first, and move down, `None` on the last. An unreadable LAMB slot gives just the
/// Model row.
pub fn browser_rows(d: &Drawing, edit: &Space) -> Vec<BrowserRow> {
    let mut rows = vec![row("Model".into(), *edit == Space::Model, "editview", serde_json::json!({}))];
    let Ok(doc) = read_document(d) else { return rows };
    let edited = crate::overlays::view_id(edit);
    // The reference pane (LAMB T345): (eligible views, the one shown) when the pane is on.
    let reference = edited.and_then(|e| crate::refpane::pick(d, e));
    let shown = crate::refpane::shown_in(d);
    // The underlays (LAMB T350, T522).
    let candidates = edited.map(|e| crate::overlays::underlay::candidates(d, e)).unwrap_or_default();
    let under = edited.map(|e| crate::overlays::underlay::current(d, e)).unwrap_or_default();
    let blocks: Vec<(Id, String)> = doc.views.iter().map(|v| (v.id(), view_block_name(v.id()))).collect();
    let numbered = crate::sheets::numbered_viewports(d, &blocks);
    let view_row = |v: &AnyView| {
        let active = matches!(edit, Space::Block(name) if *name == view_block_name(v.id()));
        let tip_name = format!("{} — {}", v.name(), crate::commands::kind_word(v));
        let mut r = row(v.name().to_owned(), active, "editview", serde_json::json!({ "viewId": v.id().0 }));
        let ref_cell = reference.as_ref().filter(|(eligible, _)| eligible.contains(&v.id())).and_then(|(_, pick)| {
            if shown && *pick == v.id() {
                cell(true, format!("Reference pane off ({tip_name})"), "refpane", serde_json::json!({ "on": false }))
            } else {
                cell(false, format!("Show {tip_name} in the reference pane"), "refpane", serde_json::json!({ "viewId": v.id().0 }))
            }
        });
        let under_cell = candidates.iter().find(|c| c.view_id == v.id()).and_then(|c| {
            let mirrored = if c.mirrored { " (mirrored)" } else { "" };
            cell(under.contains(&c.view_id), format!("Underlay {tip_name}{mirrored}"), "underlay", serde_json::json!({ "viewId": c.view_id.0 }))
        });
        r.cells = vec![ref_cell, under_cell];
        let sheet = numbered.iter().find(|(view, ..)| *view == v.id()).map(|(_, _, layout, _)| layout.to_string()).unwrap_or_default();
        r.fields = vec![level_field(&doc, v), sheet];
        r
    };
    for (title, kind) in GROUPS {
        let group: Vec<&AnyView> = doc.views.iter().filter(|v| crate::commands::kind_word(v) == kind).collect();
        if !group.is_empty() {
            rows.push(heading(title));
            rows.extend(group.into_iter().map(view_row));
        }
    }
    if doc.views.is_empty() {
        rows.push(row("Start LAMB project".into(), false, "lambproject", serde_json::json!({})));
    }
    let mut sheets: Vec<_> = d.layouts.iter().filter(|l| crate::titleblock::is_lamb_sheet(l)).collect();
    sheets.sort_by_key(|l| l.tab_order);
    let last = sheets.len().saturating_sub(1);
    for (k, l) in sheets.into_iter().enumerate() {
        let active = matches!(edit, Space::Paper(name) if *name == l.name);
        let mut r = row(l.name.clone(), active, "layout", serde_json::json!({ "option": "set", "name": l.name }));
        let step = |tip: &str, by: i32| cell(true, tip.to_owned(), "sheetorder", serde_json::json!({ "layout": l.name, "step": by }));
        r.tab = SHEETS_TAB;
        r.fields = vec![crate::titleblock::sheet_title(d, &doc, &l.name)];
        r.cells =
            vec![if k == 0 { None } else { step("Move up in plot order", -1) }, if k == last { None } else { step("Move down in plot order", 1) }];
        rows.push(r);
    }
    rows
}

/// A view row's Level field: a placed plan's `"<level> <floor>"` (floor in signed mm, `+0`,
/// `+3000`, `-450`), `unplaced` for an unplaced plan, section or elevation, else empty (a placed
/// section or elevation, a detail, or a plan whose level cannot be found).
fn level_field(doc: &LambDocument, v: &AnyView) -> String {
    match v.placement() {
        Some(Placement::Plan { level_id, .. }) => doc
            .levels
            .iter()
            .find(|l| l.id == *level_id)
            .map(|l| format!("{} {:+}", l.name, l.floor_elevation.value.round() as i64))
            .unwrap_or_default(),
        None if matches!(v, AnyView::Plan(_) | AnyView::Vertical(_)) => "unplaced".into(),
        Some(Placement::Vertical { .. }) | None => String::new(),
    }
}

/// Whether `name` is a layer LAMB keeps for itself: `LAMB_VIEW_<id>` or `LAMB_ASSEMBLY_<id>`
/// (any case) with a block of that name in the drawing. Such layers exist for outside software to
/// display sheets; layer lists leave them out (registered as the fork's hidden-layer predicate).
pub fn internal_layer(d: &Drawing, name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let id = upper.strip_prefix("LAMB_VIEW_").or_else(|| upper.strip_prefix("LAMB_ASSEMBLY_"));
    id.is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())) && d.block(name).is_some()
}

/// Whether `e` is LAMB's own model-space reference to a view, assembly, annotation, hole or layer-hatch block
/// (any case): the Model tab leaves it out, while sheets and DWG/DXF keep it (registered as the
/// fork's model-hidden hook). A loose reference to any other block stays.
pub fn model_hidden(d: &Drawing, e: &Entity) -> bool {
    const PREFIXES: [&str; 5] = ["LAMB_VIEW_", "LAMB_ASSEMBLY_", "LAMB_ANNO_", "LAMB_HOLE_", "LAMB_LHATCH_"];
    let EntityKind::Insert(insert) = &e.kind else {
        return false;
    };
    let name = insert.block.as_str();
    PREFIXES.iter().any(|p| name.get(..p.len()).is_some_and(|head| head.eq_ignore_ascii_case(p)))
        && d.block(name).is_some()
        && d.model.get(e.handle).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_engine::Session;
    use cadcraft_geom::Vec2;
    use serde_json::json;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;

    fn session() -> std::result::Result<Session, Box<dyn std::error::Error>> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        Ok(s)
    }

    fn full(s: &Session) -> std::result::Result<Vec<BrowserRow>, Box<dyn std::error::Error>> {
        Ok(browser_rows(s.doc()?, &s.state()?.space))
    }

    /// The Views tab's clickable rows: Model, then the views (no headings, no sheets).
    fn rows(s: &Session) -> std::result::Result<Vec<BrowserRow>, Box<dyn std::error::Error>> {
        Ok(full(s)?.into_iter().filter(|r| r.tab == VIEWS_TAB && !r.heading).collect())
    }

    #[test]
    fn an_empty_drawing_offers_model_and_start_project() -> R {
        let s = session()?;
        let r = full(&s)?;
        assert_eq!(r.len(), 2, "no headings without views");
        assert_eq!((r[0].label.as_str(), r[0].active), ("Model", true));
        assert_eq!(r[0].command, Some(("editview".into(), json!({}))));
        assert_eq!(r[1].label, "Start LAMB project");
        assert_eq!(r[1].command, Some(("lambproject".into(), json!({}))));
        assert!(!r[1].active && !r[1].heading);
        Ok(())
    }

    #[test]
    fn view_layers_are_internal_only_while_their_block_exists() -> R {
        let mut s = session()?;
        s.execute("lambproject", &json!({}))?;
        let plan = read_document(s.doc()?)?.views[0].id();
        let d = s.doc()?;
        assert!(internal_layer(d, &view_block_name(plan)));
        assert!(internal_layer(d, &view_block_name(plan).to_lowercase()));
        assert!(!internal_layer(d, "0"));
        assert!(!internal_layer(d, "LAMB_VIEW_999999"), "no such block");
        assert!(!internal_layer(d, "LAMB_VIEW_"));
        Ok(())
    }

    #[test]
    fn views_list_in_order_with_the_edited_one_active() -> R {
        let mut s = session()?;
        s.execute("lambproject", &json!({}))?;
        let r = rows(&s)?;
        assert_eq!(r.iter().map(|r| (r.label.as_str(), r.active)).collect::<Vec<_>>(), [("Model", false), ("Ground Floor", true)]);
        assert_eq!(r[1].fields, ["Level 1 +0", ""]);
        let ground = read_document(s.doc()?)?.views[0].id();
        assert_eq!(r[1].command, Some(("editview".into(), json!({ "viewId": ground.0 }))));

        let id = s.execute("newplan", &json!({ "name": "Sketch" }))?["viewId"].as_u64().ok_or("no id")?;
        let r = rows(&s)?;
        assert_eq!(r.len(), 3, "no Start row once views exist");
        assert_eq!((r[2].label.as_str(), r[2].fields.clone()), ("Sketch", vec!["unplaced".to_owned(), String::new()]));
        assert_eq!(r[2].command, Some(("editview".into(), json!({ "viewId": id }))));

        // A row's command is what a click runs: it makes that view the edit space.
        let (cmd, params) = r[2].command.clone().ok_or("no command")?;
        s.execute(&cmd, &params)?;
        let r = rows(&s)?;
        assert!(r[2].active && !r[1].active && !r[0].active);
        let (cmd, params) = r[0].command.clone().ok_or("no command")?;
        s.execute(&cmd, &params)?;
        assert!(rows(&s)?[0].active);
        Ok(())
    }

    /// (label, Ref cell on?, Underlay cell on?) per view row; `None` where there is no cell.
    fn cells(r: &[BrowserRow]) -> Vec<(&str, Option<bool>, Option<bool>)> {
        r[1..]
            .iter()
            .map(|r| {
                let on = |i: usize| r.cells.get(i).and_then(Option::as_ref).map(|c| c.on);
                (r.label.as_str(), on(0), on(1))
            })
            .collect()
    }

    fn click(s: &mut Session, r: &BrowserRow, col: usize) -> R {
        let c = r.cells.get(col).and_then(Option::as_ref).ok_or("no cell")?;
        s.execute(&c.command.0, &c.command.1)?;
        Ok(())
    }

    #[test]
    fn ref_cells_pick_the_reference_view_like_a_radio_button() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        let (mut s, plan, a, b) = crate::refpane::tests::project()?;
        let r = rows(&s)?;
        let (gf, aa, bb) = ("Ground Floor", "A-A", "B-B");
        assert_eq!(cells(&r), [(gf, None, None), (aa, Some(false), None), (bb, Some(false), None)], "the edited plan has no cells");
        let c = r[3].cells[0].as_ref().ok_or("no B-B Ref cell")?;
        assert_eq!(c.command, ("refpane".into(), json!({ "viewId": b.0 })));
        assert!(rows(&s)?[0].cells.is_empty(), "Model has no cells");

        click(&mut s, &r[3], 0)?;
        let r = rows(&s)?;
        assert_eq!(cells(&r), [(gf, None, None), (aa, Some(false), None), (bb, Some(true), None)], "shown, showing B-B");
        assert_eq!(r[3].cells[0].as_ref().map(|c| c.command.clone()), Some(("refpane".into(), json!({ "on": false }))));

        click(&mut s, &r[2], 0)?;
        let r = rows(&s)?;
        assert_eq!(cells(&r), [(gf, None, None), (aa, Some(true), None), (bb, Some(false), None)], "one on at most");
        click(&mut s, &r[2], 0)?;
        assert_eq!(cells(&rows(&s)?)[1], (aa, Some(false), None), "clicking the on one turns the pane off");
        assert!(!crate::refpane::shown(&s));

        s.execute("editview", &json!({ "viewId": a.0 }))?;
        let r = rows(&s)?;
        assert_eq!(
            cells(&r),
            [(gf, Some(false), None), (aa, None, None), (bb, Some(false), Some(false))],
            "a section's parallel sibling is also an underlay candidate"
        );
        assert_eq!(r[1].cells[0].as_ref().map(|c| c.command.clone()), Some(("refpane".into(), json!({ "viewId": plan.0 }))));

        s.execute("editview", &json!({}))?;
        assert!(rows(&s)?.iter().all(|r| r.cells.iter().all(Option::is_none)), "Model edits no view: no cells");
        Ok(())
    }

    /// LAMB T604: a section or an elevation is named for what it is, never for a plan.
    #[test]
    fn vertical_rows_name_no_plan() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        let (mut s, _plan, a, b) = crate::refpane::tests::project()?;
        s.execute("viewprops", &json!({ "mode": "rename", "viewId": b.0, "name": "North" }))?;
        s.execute("viewprops", &json!({ "mode": "intent", "viewId": b.0, "intent": "elevation" }))?;
        s.execute("editview", &json!({ "viewId": a.0 }))?;
        let r = rows(&s)?;
        assert_eq!(r[2].label, "A-A");
        assert_eq!(r[3].label, "North");
        assert_eq!(r[3].fields, ["", ""], "a placed elevation on no sheet has empty fields");
        assert_eq!(r[3].cells[0].as_ref().map(|c| c.tip.as_str()), Some("Show North — Elevation in the reference pane"));
        assert_eq!(r[3].cells[1].as_ref().map(|c| c.tip.as_str()), Some("Underlay North — Elevation"));
        Ok(())
    }

    #[test]
    fn underlay_cells_mark_mirroring_and_toggle_several() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        let (mut s, plan, a, _b) = crate::refpane::tests::project()?;
        assert!(rows(&s)?.iter().all(|r| r.cells.get(1).is_none_or(Option::is_none)), "a lone plan has no underlay cells");
        let c = s.execute(
            "newsectionline",
            &json!({ "name": "C-C", "hostViewId": plan.0, "cutFrom": {"x": 5000, "y": 3000}, "cutTo": {"x": 0, "y": 3000} }),
        )?;
        let c = c["viewId"].as_u64().ok_or("no C-C")?;
        s.execute("editview", &json!({ "viewId": a.0 }))?;
        let r = rows(&s)?;
        let u = |r: &BrowserRow| r.cells[1].clone();
        assert_eq!(u(&r[3]).map(|c| c.tip), Some("Underlay B-B — Section".into()));
        let cc = u(&r[4]).ok_or("no C-C underlay cell")?;
        assert_eq!((cc.tip.as_str(), cc.command), ("Underlay C-C — Section (mirrored)", ("underlay".into(), json!({ "viewId": c }))));

        click(&mut s, &r[3], 1)?;
        let r = rows(&s)?;
        assert_eq!((u(&r[3]).map(|c| c.on), u(&r[4]).map(|c| c.on)), (Some(true), Some(false)), "B-B is the underlay");
        click(&mut s, &r[4], 1)?;
        let r = rows(&s)?;
        assert_eq!((u(&r[3]).map(|c| c.on), u(&r[4]).map(|c| c.on)), (Some(true), Some(true)), "B-B and C-C both (LAMB T522)");
        click(&mut s, &r[3], 1)?;
        let r = rows(&s)?;
        assert_eq!((u(&r[3]).map(|c| c.on), u(&r[4]).map(|c| c.on)), (Some(false), Some(true)), "a click turns only that one off");
        Ok(())
    }

    /// (label, heading, fields) per Views-tab row.
    fn views_tab(r: &[BrowserRow]) -> Vec<(&str, bool, Vec<&str>)> {
        r.iter().filter(|r| r.tab == VIEWS_TAB).map(|r| (r.label.as_str(), r.heading, r.fields.iter().map(String::as_str).collect())).collect()
    }

    #[test]
    fn views_group_by_kind_and_sheets_list_on_their_own_tab_in_plot_order() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        let mut s = session()?;
        crate::maintain::install(&mut s);
        let ground = s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no id")?;
        // A section created between the two plans stays out of the Plans group (LAMB T670).
        s.execute("newsectionline", &json!({ "name": "S", "hostViewId": ground, "cutFrom": {"x": 0, "y": 2000}, "cutTo": {"x": 5000, "y": 2000} }))?;
        s.execute("newoffsetplan", &json!({ "mode": "offset", "fromViewId": ground, "floorToFloorMm": 3000, "name": "First" }))?;
        s.start("newdetail")?;
        s.input(cadcraft_engine::Input::Text("Eaves".into()))?;
        s.execute("editview", &json!({}))?;
        assert!(!full(&s)?.iter().any(|r| r.tab == SHEETS_TAB), "no sheets before any sheet exists");
        assert_eq!(
            views_tab(&full(&s)?),
            [
                ("Model", false, vec![]),
                ("Plans", true, vec![]),
                ("Ground Floor", false, vec!["Level 1 +0", ""]),
                ("First", false, vec!["First +3000", ""]),
                ("Sections", true, vec![]),
                ("S", false, vec!["", ""]),
                ("Details", true, vec![]),
                ("Eaves", false, vec!["", ""]),
            ]
        );
        s.execute("layout", &json!({ "option": "new", "name": "Notes" }))?;
        assert!(!full(&s)?.iter().any(|r| r.tab == SHEETS_TAB), "a plain layout is not a sheet");

        s.execute("sheetset", &json!({}))?;
        s.execute("editview", &json!({}))?;
        let r = full(&s)?;
        let sheet_of = |name: &str| r.iter().find(|r| r.label == name).and_then(|r| r.fields.get(1)).cloned().unwrap_or_default();
        assert_eq!((sheet_of("Ground Floor").as_str(), sheet_of("First").as_str()), ("A101", "A102"));
        assert!(r.iter().filter(|r| r.heading).all(|r| r.tab == VIEWS_TAB && r.command.is_none() && r.cells.is_empty()), "no Sheets heading");
        let sheets: Vec<&BrowserRow> = r.iter().filter(|r| r.tab == SHEETS_TAB).collect();
        let names: Vec<&str> = sheets.iter().map(|r| r.label.as_str()).collect();
        let mut tabs: Vec<_> =
            s.doc()?.layouts.iter().filter(|l| crate::titleblock::is_lamb_sheet(l)).map(|l| (l.tab_order, l.name.clone())).collect();
        tabs.sort();
        assert_eq!(names, tabs.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>(), "plot order; Notes is off the list");
        assert!(names.len() >= 3 && names[..3] == ["A001", "A101", "A102"], "{names:?}");
        assert_eq!(sheets[0].fields, ["Drawing List"]);
        assert_eq!(sheets[1].fields, ["Ground Floor"]);
        let has = |r: &BrowserRow, col: usize| r.cells[col].is_some();
        let n = sheets.len();
        assert!(!has(sheets[0], 0) && has(sheets[0], 1), "the first has no Up");
        assert!(has(sheets[n - 1], 0) && !has(sheets[n - 1], 1), "the last has no Down");
        assert!(sheets[1..n - 1].iter().all(|r| has(r, 0) && has(r, 1)));
        let up = sheets[1].cells[0].as_ref().ok_or("no up cell")?;
        assert!(up.on && up.tip == "Move up in plot order");
        assert_eq!(up.command, ("sheetorder".into(), json!({ "layout": "A101", "step": -1 })));
        let down = sheets[1].cells[1].as_ref().ok_or("no down cell")?;
        assert_eq!(down.command, ("sheetorder".into(), json!({ "layout": "A101", "step": 1 })));

        let (cmd, params) = sheets[2].command.clone().ok_or("no command")?;
        assert_eq!((cmd.as_str(), &params), ("layout", &json!({ "option": "set", "name": "A102" })));
        s.execute(&cmd, &params)?;
        assert_eq!(s.state()?.space, Space::Paper("A102".into()));
        let r = full(&s)?;
        assert_eq!(r.iter().filter(|r| r.active).map(|r| r.label.as_str()).collect::<Vec<_>>(), ["A102"], "the sheet alone");

        // A cell moves the sheet and leaves the row's own command alone: A102 stays the current sheet.
        let a102 = r.iter().find(|r| r.label == "A102").ok_or("no A102")?.clone();
        click(&mut s, &a102, 0)?;
        assert_eq!(s.state()?.space, Space::Paper("A102".into()));
        let after: Vec<String> = full(&s)?.into_iter().filter(|r| r.tab == SHEETS_TAB).map(|r| r.label).collect();
        assert_eq!(after[..3], ["A001", "A102", "A101"]);
        Ok(())
    }

    #[test]
    fn a_sheet_field_leaves_the_cell_tips_alone() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        let (mut s, _plan, a, b) = crate::refpane::tests::project()?;
        s.execute("editview", &json!({ "viewId": a.0 }))?;
        s.execute("sheetset", &json!({}))?;
        s.execute("editview", &json!({ "viewId": a.0 }))?;
        let r = rows(&s)?;
        let bb = r.iter().find(|r| r.command == Some(("editview".into(), json!({ "viewId": b.0 })))).ok_or("no B-B")?;
        assert_eq!(bb.label, "B-B");
        assert!(!bb.fields[1].is_empty(), "{bb:?}");
        assert_eq!(bb.cells[0].as_ref().map(|c| c.tip.as_str()), Some("Show B-B — Section in the reference pane"));
        assert_eq!(bb.cells[1].as_ref().map(|c| c.tip.as_str()), Some("Underlay B-B — Section"));
        Ok(())
    }

    #[test]
    fn an_unreadable_slot_gives_just_model() -> R {
        let mut s = session()?;
        s.doc_mut()?.ext.insert("LAMB".into(), json!({ "schemaVersion": "nope" }));
        let r = rows(&s)?;
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].label, "Model");
        Ok(())
    }

    /// A plan with its assembly block and detail, a second plan and a section, each holding a line;
    /// then back on the Model tab with a loose line far from them all.
    fn all_views() -> std::result::Result<(Session, Vec<Id>, Handle), Box<dyn std::error::Error>> {
        let (mut s, plan) = crate::assembly::tests::room_with("exterior_face", true, 200.0)?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let second = Id(s.execute("newplan", &json!({ "name": "First" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": second.0 }))?;
        crate::assembly::tests::line(&mut s, [100.0, 100.0], [900.0, 100.0])?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let r = s.execute(
            "newsectionline",
            &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 2000}, "cutTo": {"x": 5000, "y": 2000} }),
        )?;
        let sec = Id(r["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        crate::assembly::tests::line(&mut s, [100.0, 100.0], [900.0, 100.0])?;
        s.execute("editview", &json!({}))?;
        let loose = crate::assembly::tests::line(&mut s, [-90000.0, -90000.0], [-89000.0, -90000.0])?;
        Ok((s, vec![plan, second, sec], loose))
    }

    #[test]
    fn the_model_tab_draws_picks_frames_and_selects_only_loose_geometry() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        let (mut s, views, loose) = all_views()?;
        let d = s.doc()?;
        let hidden = d.model.iter().filter(|e| d.is_model_hidden(e)).count();
        assert!(hidden >= 5, "{hidden}");
        assert_eq!(d.model.iter().filter(|e| !d.is_model_hidden(e)).count(), 1);
        let list = cadcraft_render::build(d, &Space::Model, &cadcraft_render::Options::default());
        assert_eq!(list.prims.len(), 1);
        assert!(list.verts.iter().all(|p| p.y == -90000.0), "{:?}", list.verts);
        let ext = d.extents(&Space::Model);
        assert_eq!((ext.min.x, ext.min.y, ext.max.x, ext.max.y), (-90000.0, -90000.0, -89000.0, -90000.0));
        s.zoom_extents()?;
        let view = s.state()?.view();
        assert!((view.center.x + 89500.0).abs() < 1.0 && (view.center.y + 90000.0).abs() < 1.0 && view.height < 2000.0, "{view:?}");
        assert!(cadcraft_engine::cmd::curves::pick_at(&s, Vec2::new(-89500.0, -90000.0)) == Some(loose));
        assert!(cadcraft_engine::cmd::curves::pick_at(&s, Vec2::new(2500.0, 0.0)).is_none());
        s.execute("selectall", &json!({}))?;
        assert_eq!(s.state()?.selection, [loose]);
        // The Model tab's choice stops there: editing a view draws and picks its block as before.
        for v in &views {
            let list = cadcraft_render::build(s.doc()?, &Space::Block(view_block_name(*v)), &cadcraft_render::Options::default());
            assert!(!list.prims.is_empty(), "{v:?}");
        }
        s.execute("editview", &json!({ "viewId": views[0].0 }))?;
        assert!(cadcraft_engine::cmd::curves::pick_at(&s, Vec2::new(2500.0, 0.0)).is_some());
        Ok(())
    }

    #[test]
    fn a_large_model_tab_built_on_all_cores_leaves_the_views_out_too() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        let (mut s, _, _) = all_views()?;
        for i in 0..600 {
            let y = -90000.0 - f64::from(i);
            crate::assembly::tests::line(&mut s, [-90000.0, y], [-89000.0, y])?;
        }
        let list = cadcraft_render::build(s.doc()?, &Space::Model, &cadcraft_render::Options::default());
        assert_eq!(list.prims.len(), 601);
        assert!(list.verts.iter().all(|p| p.y <= -90000.0));
        Ok(())
    }

    /// The prims a sheet's plot loses when the drawing's references to `block` go.
    fn plot_of_block(d: &Drawing, sheet: &Space, block: &str) -> usize {
        let opts = cadcraft_render::Options::default();
        let mut bare = d.clone();
        let refs: Vec<Handle> =
            bare.model.iter().filter(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == block)).map(|e| e.handle).collect();
        for h in refs {
            bare.remove_entity(h);
        }
        cadcraft_render::build_plot(d, sheet, &opts).prims.len().saturating_sub(cadcraft_render::build_plot(&bare, sheet, &opts).prims.len())
    }

    #[test]
    fn sheets_still_show_each_view_also_after_a_dxf_round_trip() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        let (mut s, views, _) = all_views()?;
        let mut sheets = Vec::new();
        for (i, v) in views.iter().enumerate() {
            let name = format!("A30{i}");
            s.execute("newsheet", &json!({ "name": name, "viewIds": [v.0] }))?;
            sheets.push((Space::Paper(name), *v));
        }
        s.execute("newsheet", &json!({ "name": "A310", "assemblyIds": [views[0].0] }))?;
        let d = s.doc()?;
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(d).as_bytes()).map_err(|e| format!("{e:?}"))?;
        for drawing in [d, &back] {
            assert!(drawing.model.iter().filter(|e| drawing.is_model_hidden(e)).count() >= 5);
            for (sheet, v) in &sheets {
                assert!(plot_of_block(drawing, sheet, &view_block_name(*v)) > 0, "{v:?} on {sheet:?}");
            }
            let assembly = crate::schema::assembly_block_name(views[0]);
            assert!(plot_of_block(drawing, &Space::Paper("A310".into()), &assembly) > 0, "the assembly viewport");
        }
        Ok(())
    }
}

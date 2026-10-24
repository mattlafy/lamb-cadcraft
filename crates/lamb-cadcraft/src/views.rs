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

/// The Views palette's icon columns, left to right: index 0 is the reference pane, 1 the underlay.
pub const BROWSER_COLUMNS: [&str; 2] = ["Ref", "Underlay"];

/// One icon cell of a [`BrowserRow`] (`cadcraft_ui_egui::viewlist::Cell` without the icon, which
/// the column picks): `on` draws it full-strength; a click runs `command`.
#[derive(Clone, Debug, PartialEq)]
pub struct BrowserCell {
    pub on: bool,
    pub tip: String,
    pub command: (String, serde_json::Value),
}

/// One row of the desktop app's Views palette (`cadcraft_ui_egui::viewlist::Row`, same fields):
/// the TS app's Views panel (`ViewsPanel.tsx`) as data. `cells` holds one entry per
/// [`BROWSER_COLUMNS`] column, `None` where that mode is not valid for the row.
#[derive(Clone, Debug, PartialEq)]
pub struct BrowserRow {
    pub label: String,
    pub active: bool,
    pub heading: bool,
    pub command: Option<(String, serde_json::Value)>,
    pub cells: Vec<Option<BrowserCell>>,
}

fn row(label: String, active: bool, command: &str, params: serde_json::Value) -> BrowserRow {
    BrowserRow { label, active, heading: false, command: Some((command.to_owned(), params)), cells: Vec::new() }
}

fn cell(on: bool, tip: String, command: &str, params: serde_json::Value) -> Option<BrowserCell> {
    Some(BrowserCell { on, tip, command: (command.to_owned(), params) })
}

/// The Views palette for `d` while editing `edit`: `Model`, then each view in document order as
/// [`view_label`] (its level and floor, its host plan, or `(unplaced)`), the edited one active, each running
/// `editview`; a drawing with no views offers `Start LAMB project` (LAMBPROJECT). Each view row has
/// two cells ([`BROWSER_COLUMNS`]), `None` unless the mode is valid for it while editing a view:
/// **Ref** for the edited view's eligible reference views — on for the one the shown pane displays
/// (at most one), clicking an off one runs `refpane {viewId}` (which also turns the pane on), the on
/// one `refpane {on: false}`; **Underlay** for its underlay candidates — on while it is one of the
/// underlays (any number), a click runs `underlay {viewId}` (toggle), the tip noting `(mirrored)`.
/// The edited view's own row has neither. An unreadable LAMB slot gives just the Model row.
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
    for v in &doc.views {
        let active = matches!(edit, Space::Block(name) if *name == view_block_name(v.id()));
        let label = view_label(&doc, v);
        let mut r = row(label.clone(), active, "editview", serde_json::json!({ "viewId": v.id().0 }));
        let ref_cell = reference.as_ref().filter(|(eligible, _)| eligible.contains(&v.id())).and_then(|(_, pick)| {
            if shown && *pick == v.id() {
                cell(true, format!("Reference pane off ({label})"), "refpane", serde_json::json!({ "on": false }))
            } else {
                cell(false, format!("Show {label} in the reference pane"), "refpane", serde_json::json!({ "viewId": v.id().0 }))
            }
        });
        let under_cell = candidates.iter().find(|c| c.view_id == v.id()).and_then(|c| {
            let mirrored = if c.mirrored { " (mirrored)" } else { "" };
            cell(under.contains(&c.view_id), format!("Underlay {label}{mirrored}"), "underlay", serde_json::json!({ "viewId": c.view_id.0 }))
        });
        r.cells = vec![ref_cell, under_cell];
        rows.push(r);
    }
    if doc.views.is_empty() {
        rows.push(row("Start LAMB project".into(), false, "lambproject", serde_json::json!({})));
    }
    rows
}

/// A placed plan `"<name> — Plan · <level> <floor>"` (floor in signed mm, `+0`, `+3000`, `-450`);
/// a placed section or elevation `"<name> — Section · on <host plan>"`; an unplaced plan, section or
/// elevation `"<name> — <kind> (unplaced)"`; a detail `"<name> — Detail"`. A level or host that
/// cannot be found leaves just `"<name> — <kind>"`.
fn view_label(doc: &LambDocument, v: &AnyView) -> String {
    let kind = crate::commands::kind_word(v);
    let base = format!("{} — {kind}", v.name());
    match v.placement() {
        Some(Placement::Plan { level_id, .. }) => match doc.levels.iter().find(|l| l.id == *level_id) {
            Some(l) => format!("{base} · {} {:+}", l.name, l.floor_elevation.value.round() as i64),
            None => base,
        },
        Some(Placement::Vertical { host_view_id, .. }) => match doc.views.iter().find(|h| h.id() == *host_view_id) {
            Some(h) => format!("{base} · on {}", h.name()),
            None => base,
        },
        None if matches!(v, AnyView::Plan(_) | AnyView::Vertical(_)) => format!("{base} (unplaced)"),
        None => base,
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

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_engine::Session;
    use serde_json::json;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;

    fn session() -> std::result::Result<Session, Box<dyn std::error::Error>> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        Ok(s)
    }

    fn rows(s: &Session) -> std::result::Result<Vec<BrowserRow>, Box<dyn std::error::Error>> {
        Ok(browser_rows(s.doc()?, &s.state()?.space))
    }

    #[test]
    fn an_empty_drawing_offers_model_and_start_project() -> R {
        let s = session()?;
        let r = rows(&s)?;
        assert_eq!(r.len(), 2);
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
        assert_eq!(
            r.iter().map(|r| (r.label.as_str(), r.active)).collect::<Vec<_>>(),
            [("Model", false), ("Ground Floor — Plan · Level 1 +0", true)]
        );
        let ground = read_document(s.doc()?)?.views[0].id();
        assert_eq!(r[1].command, Some(("editview".into(), json!({ "viewId": ground.0 }))));

        let id = s.execute("newplan", &json!({ "name": "Sketch" }))?["viewId"].as_u64().ok_or("no id")?;
        let r = rows(&s)?;
        assert_eq!(r.len(), 3, "no Start row once views exist");
        assert_eq!(r[2].label, "Sketch — Plan (unplaced)");
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
        assert!(!r.iter().any(|r| r.heading), "no heading rows: {r:?}");
        let (gf, aa, bb) = ("Ground Floor — Plan · Level 1 +0", "A-A — Section · on Ground Floor", "B-B — Section · on Ground Floor");
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
        assert_eq!(u(&r[3]).map(|c| c.tip), Some("Underlay B-B — Section · on Ground Floor".into()));
        let cc = u(&r[4]).ok_or("no C-C underlay cell")?;
        assert_eq!(
            (cc.tip.as_str(), cc.command),
            ("Underlay C-C — Section · on Ground Floor (mirrored)", ("underlay".into(), json!({ "viewId": c })))
        );

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

    #[test]
    fn an_unreadable_slot_gives_just_model() -> R {
        let mut s = session()?;
        s.doc_mut()?.ext.insert("LAMB".into(), json!({ "schemaVersion": "nope" }));
        let r = rows(&s)?;
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].label, "Model");
        Ok(())
    }
}

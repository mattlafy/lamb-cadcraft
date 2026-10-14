//! Views as blocks (`cadcraft-integration.md` §4, insertion regime): a LAMB view is a block named
//! [`view_block_name`]`(id)` holding its entities, a layer of the same name, and one canonical
//! INSERT of that block in model space. The canonical insert is the one whose handle equals the
//! view id — identity marks it, no extra field.

use std::fmt;
use std::sync::Arc;

use cadcraft_doc::{Block, Common, Drawing, Entity, EntityKind, Handle, Insert, Space};
use cadcraft_geom::Vec3;
use lamb_core::model::{AnyView, Id};

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

/// One row of the desktop app's Views palette (`cadcraft_ui_egui::viewlist::Row`, same fields):
/// the TS app's Views panel (`ViewsPanel.tsx`) as data.
#[derive(Clone, Debug, PartialEq)]
pub struct BrowserRow {
    pub label: String,
    pub active: bool,
    pub heading: bool,
    pub command: Option<(String, serde_json::Value)>,
}

fn row(label: String, active: bool, command: &str, params: serde_json::Value) -> BrowserRow {
    BrowserRow { label, active, heading: false, command: Some((command.to_owned(), params)) }
}

/// The Views palette for `d` while editing `edit`: `Model`, then each view in document order as
/// `"<name> — <kind>"` (`" (floating)"` without a placement), the edited one active, each running
/// `editview`; a drawing with no views offers `Start LAMB project` (LAMBPROJECT). An unreadable
/// LAMB slot gives just the Model row.
pub fn browser_rows(d: &Drawing, edit: &Space) -> Vec<BrowserRow> {
    let mut rows = vec![row("Model".into(), *edit == Space::Model, "editview", serde_json::json!({}))];
    let Ok(doc) = read_document(d) else { return rows };
    for v in &doc.views {
        let floating = if v.placement().is_none() { " (floating)" } else { "" };
        let label = format!("{} — {}{floating}", v.name(), crate::commands::kind_word(v));
        let active = matches!(edit, Space::Block(name) if *name == view_block_name(v.id()));
        rows.push(row(label, active, "editview", serde_json::json!({ "viewId": v.id().0 })));
    }
    if doc.views.is_empty() {
        rows.push(row("Start LAMB project".into(), false, "lambproject", serde_json::json!({})));
    }
    rows
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
    fn views_list_in_order_with_the_edited_one_active() -> R {
        let mut s = session()?;
        s.execute("lambproject", &json!({}))?;
        let r = rows(&s)?;
        assert_eq!(r.iter().map(|r| (r.label.as_str(), r.active)).collect::<Vec<_>>(), [("Model", false), ("Ground Floor — Plan", true)]);
        let ground = read_document(s.doc()?)?.views[0].id();
        assert_eq!(r[1].command, Some(("editview".into(), json!({ "viewId": ground.0 }))));

        let id = s.execute("newplan", &json!({ "name": "Sketch" }))?["viewId"].as_u64().ok_or("no id")?;
        let r = rows(&s)?;
        assert_eq!(r.len(), 3, "no Start row once views exist");
        assert_eq!(r[2].label, "Sketch — Plan (floating)");
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

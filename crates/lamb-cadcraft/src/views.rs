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
/// [`view_label`] (its level and floor, its host plan, or `(unplaced)`), the edited one active, each running
/// `editview`; a drawing with no views offers `Start LAMB project` (LAMBPROJECT). When the edited
/// view has eligible reference views: a `Reference pane` heading, `Show reference pane` (active
/// while shown, toggling REFPANE) and one row per eligible view, active when the pane shows it,
/// running `refpane {viewId}`. When it has underlay candidates: an `Underlay` heading and one row
/// per candidate (`" (mirrored)"` when it renders mirrored), active when it is the underlay,
/// running `underlay {viewId}` (clicking the active one turns it off). An unreadable LAMB slot
/// gives just the Model row.
pub fn browser_rows(d: &Drawing, edit: &Space) -> Vec<BrowserRow> {
    let mut rows = vec![row("Model".into(), *edit == Space::Model, "editview", serde_json::json!({}))];
    let Ok(doc) = read_document(d) else { return rows };
    for v in &doc.views {
        let active = matches!(edit, Space::Block(name) if *name == view_block_name(v.id()));
        rows.push(row(view_label(&doc, v), active, "editview", serde_json::json!({ "viewId": v.id().0 })));
    }
    if doc.views.is_empty() {
        rows.push(row("Start LAMB project".into(), false, "lambproject", serde_json::json!({})));
    }
    // The reference pane (LAMB T345), when the edited view has a reference to show.
    if let Some((eligible, pick)) = crate::overlays::view_id(edit).and_then(|e| crate::refpane::pick(d, e)) {
        let shown = crate::refpane::shown_in(d);
        rows.push(BrowserRow { label: "Reference pane".into(), active: false, heading: true, command: None });
        rows.push(row("Show reference pane".into(), shown, "refpane", serde_json::json!({ "on": !shown })));
        for v in eligible.iter().filter_map(|id| doc.views.iter().find(|v| v.id() == *id)) {
            rows.push(row(view_label(&doc, v), shown && v.id() == pick, "refpane", serde_json::json!({ "viewId": v.id().0 })));
        }
    }
    // The underlay (LAMB T350).
    if let Some(e) = crate::overlays::view_id(edit) {
        let candidates = crate::overlays::underlay::candidates(d, e);
        if !candidates.is_empty() {
            let current = crate::overlays::underlay::current(d, e);
            rows.push(BrowserRow { label: "Underlay".into(), active: false, heading: true, command: None });
            for c in &candidates {
                let Some(v) = doc.views.iter().find(|v| v.id() == c.view_id) else { continue };
                let label = if c.mirrored { format!("{} (mirrored)", view_label(&doc, v)) } else { view_label(&doc, v) };
                rows.push(row(label, current == Some(c.view_id), "underlay", serde_json::json!({ "viewId": c.view_id.0 })));
            }
        }
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

    #[test]
    fn reference_rows_toggle_the_pane_and_pick_its_view() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        let (mut s, plan, a, b) = crate::refpane::tests::project()?;
        let r = rows(&s)?;
        let tail: Vec<_> = r[4..].iter().map(|r| (r.label.as_str(), r.active, r.heading)).collect();
        assert_eq!(
            tail,
            [
                ("Reference pane", false, true),
                ("Show reference pane", false, false),
                ("A-A — Section · on Ground Floor", false, false),
                ("B-B — Section · on Ground Floor", false, false)
            ]
        );
        assert_eq!(r[4].command, None);
        assert_eq!(r[5].command, Some(("refpane".into(), json!({ "on": true }))));
        assert_eq!(r[7].command, Some(("refpane".into(), json!({ "viewId": b.0 }))));

        let (cmd, params) = r[7].command.clone().ok_or("no command")?;
        s.execute(&cmd, &params)?;
        let r = rows(&s)?;
        assert_eq!(r[4..].iter().map(|r| r.active).collect::<Vec<_>>(), [false, true, false, true], "shown, showing B-B");
        assert_eq!(r[5].command, Some(("refpane".into(), json!({ "on": false }))));

        s.execute("editview", &json!({ "viewId": a.0 }))?;
        let r = rows(&s)?;
        let tail: Vec<_> = r[4..].iter().map(|r| (r.label.as_str(), r.active)).collect();
        assert_eq!(
            tail,
            [
                ("Reference pane", false),
                ("Show reference pane", true),
                ("Ground Floor — Plan · Level 1 +0", true),
                ("B-B — Section · on Ground Floor", false),
                ("Underlay", false),
                ("B-B — Section · on Ground Floor", false)
            ],
            "a section's parallel sibling is also an underlay candidate"
        );
        assert_eq!(r[6].command, Some(("refpane".into(), json!({ "viewId": plan.0 }))));

        s.execute("editview", &json!({}))?;
        assert_eq!(rows(&s)?.len(), 4, "Model has no reference rows");
        Ok(())
    }

    #[test]
    fn underlay_rows_mark_mirroring_and_toggle_it() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        let (mut s, plan, a, _b) = crate::refpane::tests::project()?;
        assert!(!rows(&s)?.iter().any(|r| r.label == "Underlay"), "a lone plan has no underlay rows");
        let c = s.execute(
            "newsectionline",
            &json!({ "name": "C-C", "hostViewId": plan.0, "cutFrom": {"x": 5000, "y": 3000}, "cutTo": {"x": 0, "y": 3000} }),
        )?;
        let c = c["viewId"].as_u64().ok_or("no C-C")?;
        s.execute("editview", &json!({ "viewId": a.0 }))?;
        let r = rows(&s)?;
        let at = r.iter().position(|r| r.label == "Underlay" && r.heading).ok_or("no Underlay heading")?;
        let under: Vec<_> = r[at + 1..].iter().map(|r| (r.label.as_str(), r.active)).collect();
        assert_eq!(under, [("B-B — Section · on Ground Floor", false), ("C-C — Section · on Ground Floor (mirrored)", false)]);
        assert_eq!(r[at + 2].command, Some(("underlay".into(), json!({ "viewId": c }))));

        let (cmd, params) = r[at + 1].command.clone().ok_or("no command")?;
        s.execute(&cmd, &params)?;
        let r = rows(&s)?;
        assert!(r[at + 1].active && !r[at + 2].active, "B-B is the underlay");
        s.execute(&cmd, &params)?;
        assert!(!rows(&s)?[at + 1].active, "clicking the active row turns it off");
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

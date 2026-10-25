//! A generic list palette (fork-only, LAMB T343): rows come from one registered [`RowSource`],
//! a row can be a dim caption (`heading`), highlighted (`active`), and run a command when clicked.
//! Like the Layers palette's state columns, a row can carry icon [`Cell`]s in fixed-width columns
//! named by [`register_columns`]; a cell click runs the cell's command, not the row's.
//! It knows nothing of what the rows are; the LAMB desktop app registers its views into it.

use std::sync::OnceLock;

use egui::{Sense, vec2};
use serde_json::Value;

use cadcraft_doc::{Drawing, Space};

use crate::CadApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// The width of one icon column.
const COL_W: f32 = 56.0;

/// One icon in a row's column: drawn full-strength when `on`, dim when off; a click runs `command`.
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub icon: Icon,
    pub on: bool,
    /// Hover text, and the cell's accessible label.
    pub tip: String,
    pub command: (String, Value),
}

/// One palette row.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub label: String,
    /// Drawn with the selected-row fill.
    pub active: bool,
    /// A dim, unclickable caption.
    pub heading: bool,
    /// The command (id, params) a click runs.
    pub command: Option<(String, Value)>,
    /// One entry per registered column; `None` (or a missing entry) draws nothing there.
    pub cells: Vec<Option<Cell>>,
}

/// The rows for a drawing and its edit space.
pub type RowSource = fn(&Drawing, &Space) -> Vec<Row>;

static SOURCE: OnceLock<RowSource> = OnceLock::new();

/// Register the row source. Once per process; a second call is an error.
pub fn register_row_source(f: RowSource) -> Result<(), &'static str> {
    SOURCE.set(f).map_err(|_| "view list row source already registered")
}

static COLUMNS: OnceLock<&'static [&'static str]> = OnceLock::new();

/// Register the icon columns' header labels, left to right. Once per process; a second call is an error.
pub fn register_columns(names: &'static [&'static str]) -> Result<(), &'static str> {
    COLUMNS.set(names).map_err(|_| "view list columns already registered")
}

/// The registered column labels; empty when none are.
pub fn columns() -> &'static [&'static str] {
    COLUMNS.get().copied().unwrap_or(&[])
}

/// The rows for the app's current drawing; empty when nothing is registered or no drawing is open.
pub fn rows(app: &CadApp) -> Vec<Row> {
    match (SOURCE.get(), app.session.doc(), app.session.state()) {
        (Some(f), Ok(d), Ok(st)) => f(d, &st.space),
        _ => Vec::new(),
    }
}

/// Run a row's command, as a click does. `None` for a heading or a row without a command.
pub fn activate(app: &mut CadApp, row: &Row) -> Option<Result<Value, String>> {
    if row.heading {
        return None;
    }
    let (id, params) = row.command.as_ref()?;
    Some(app.run(id, params.clone()))
}

/// Run the command of a row's cell in column `col`, as a click on it does. `None` when there is no cell.
pub fn activate_cell(app: &mut CadApp, row: &Row, col: usize) -> Option<Result<Value, String>> {
    let (id, params) = &row.cells.get(col)?.as_ref()?.command;
    Some(app.run(id, params.clone()))
}

pub fn show(app: &mut CadApp, ui: &mut egui::Ui) {
    let t = Tokens::get();
    let rows = rows(app);
    ui.add_space(4.0);
    if rows.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.label(egui::RichText::new(crate::i18n::t("No views")).color(t.text_dim));
        });
        return;
    }
    let cols = columns().len().max(rows.iter().map(|r| r.cells.len()).max().unwrap_or(0));
    let cells_w = cols as f32 * COL_W;
    if !columns().is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let (r, _) = ui.allocate_exact_size(vec2((ui.available_width() - 10.0).max(cells_w), 18.0), Sense::hover());
            for (c, name) in columns().iter().enumerate() {
                let x = r.right() - cells_w + (c as f32 + 0.5) * COL_W;
                ui.painter().text(egui::pos2(x, r.center().y), egui::Align2::CENTER_CENTER, crate::i18n::t(name), crate::theme::body(), t.text_dim);
            }
        });
    }
    let mut clicked = None;
    let mut cell_clicked = None;
    for (i, row) in rows.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(8.0);
            let label_w = (ui.available_width() - 10.0 - cells_w).max(40.0);
            if row.heading {
                let (r, _) = ui.allocate_exact_size(vec2(label_w, 20.0), Sense::hover());
                ui.painter().text(egui::pos2(r.left() + 2.0, r.center().y), egui::Align2::LEFT_CENTER, &row.label, crate::theme::body(), t.text_dim);
                return;
            }
            let r = ui.add_sized(vec2(label_w, 20.0), egui::Button::selectable(row.active, &row.label));
            if r.clicked() && row.command.is_some() {
                clicked = Some(i);
            }
            for c in 0..cols {
                let Some(cell) = row.cells.get(c).and_then(Option::as_ref) else {
                    ui.allocate_exact_size(vec2(COL_W, 20.0), Sense::hover());
                    continue;
                };
                let (r, resp) = ui.allocate_exact_size(vec2(COL_W, 20.0), Sense::click());
                resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, cell.on, &cell.tip));
                if resp.hovered() {
                    ui.painter().rect_filled(egui::Rect::from_center_size(r.center(), vec2(20.0, 20.0)), 3.0, t.hover);
                }
                icons::paint(ui.painter(), egui::Rect::from_center_size(r.center(), vec2(16.0, 16.0)), cell.icon, !cell.on);
                if resp.on_hover_text(&cell.tip).clicked() {
                    cell_clicked = Some((i, c));
                }
            }
        });
    }
    if let Some((i, c)) = cell_clicked {
        let _ = activate_cell(app, &rows[i], c);
    } else if let Some(i) = clicked {
        let _ = activate(app, &rows[i]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_engine::Session;
    use serde_json::json;

    /// Model, then one row per layout; a click switches to it. Active = the edit space.
    fn source(d: &Drawing, space: &Space) -> Vec<Row> {
        let mut v = vec![Row { label: "Spaces".into(), active: false, heading: true, command: None, cells: vec![] }];
        v.push(Row {
            label: "Model".into(),
            active: *space == Space::Model,
            heading: false,
            command: Some(("layout.set".into(), json!({"name": "Model"}))),
            cells: vec![None],
        });
        for l in &d.layouts {
            // A cell that switches to model space, so a click on it is told apart from the row's own.
            let cell =
                Cell { icon: Icon::Layers, on: false, tip: format!("Cell {}", l.name), command: ("layout.set".into(), json!({"name": "Model"})) };
            v.push(Row {
                label: l.name.clone(),
                active: false,
                heading: false,
                command: Some(("layout.set".into(), json!({"name": l.name}))),
                cells: vec![Some(cell)],
            });
        }
        v
    }

    #[test]
    fn a_row_click_runs_its_command_and_a_heading_does_nothing() {
        let _ = register_row_source(source);
        assert!(register_row_source(source).is_err(), "one source per process");
        let mut app = CadApp::new(Session::default(), crate::Services::default());
        let rows = rows(&app);
        assert!(rows[1].active, "{rows:?}");
        assert_eq!(activate(&mut app, &rows[0]), None);
        let layout = rows.iter().find(|r| r.label != "Model" && !r.heading).cloned().ok_or("no layout row").unwrap();
        assert!(activate(&mut app, &layout).is_some_and(|r| r.is_ok()));
        assert_ne!(app.session.state().unwrap().space, Space::Model, "the edit space changed");
        assert!(!self::rows(&app)[1].active);
    }

    #[test]
    fn clicking_a_rendered_row_switches_the_space() {
        let _ = register_row_source(source);
        let app = CadApp::new(Session::default(), crate::Services::default());
        let name = app.session.doc().unwrap().layouts.first().map(|l| l.name.clone()).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(240.0, 300.0)).build_ui_state(|ui, app: &mut CadApp| show(app, ui), app);
        h.run();
        use egui_kittest::kittest::Queryable;
        h.get_by_label(&name).click();
        h.run();
        assert_ne!(h.state().session.state().unwrap().space, Space::Model);
    }

    #[test]
    fn a_cell_click_runs_the_cells_command_not_the_rows() {
        let _ = register_row_source(source);
        let _ = register_columns(&["Col"]);
        assert!(register_columns(&["Col"]).is_err(), "one column set per process");
        let mut app = CadApp::new(Session::default(), crate::Services::default());
        let rows = rows(&app);
        assert_eq!(activate_cell(&mut app, &rows[1], 0), None, "Model has no cell");
        let layout = rows.iter().find(|r| r.label != "Model" && !r.heading).cloned().unwrap();
        assert!(activate_cell(&mut app, &layout, 0).is_some_and(|r| r.is_ok()));
        assert_eq!(app.session.state().unwrap().space, Space::Model, "the cell ran, not the row");

        // Rendered: clicking the cell keeps model space; clicking the label leaves it.
        let mut h = egui_kittest::Harness::builder().with_size(vec2(300.0, 300.0)).build_ui_state(|ui, app: &mut CadApp| show(app, ui), app);
        h.run();
        use egui_kittest::kittest::Queryable;
        h.get_by_label(&format!("Cell {}", layout.label)).click();
        h.run();
        assert_eq!(h.state().session.state().unwrap().space, Space::Model);
        h.get_by_label(&layout.label).click();
        h.run();
        assert_ne!(h.state().session.state().unwrap().space, Space::Model);
    }
}

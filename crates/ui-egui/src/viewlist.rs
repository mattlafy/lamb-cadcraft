//! A generic list palette (fork-only, LAMB T343): rows come from one registered [`RowSource`],
//! a row can be a dim caption (`heading`), highlighted (`active`), and run a command when clicked.
//! Rows sit on one of the [`Tab`]s named by [`register_tabs`]; two or more draw a tab strip, and
//! only the selected tab's rows are drawn (LAMB T659). A row shows its label, then its `fields` in
//! the tab's fixed-width text columns, then icon [`Cell`]s in the tab's icon columns (like the
//! Layers palette's state columns); a cell click runs the cell's command, not the row's.
//! It knows nothing of what the rows are; the LAMB desktop app registers its views into it.

use std::sync::{Arc, OnceLock};

use egui::{Sense, vec2};
use serde_json::Value;

use cadcraft_doc::{Drawing, Space};

use crate::CadApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// The width of one icon column.
const COL_W: f32 = 56.0;
/// The width of one text column.
const TEXT_COL_W: f32 = 72.0;
/// The height of one row.
const ROW_H: f32 = 20.0;

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
    /// The index of the registered [`Tab`] the row is drawn on.
    pub tab: usize,
    /// One entry per text column of its tab; a missing entry draws nothing there.
    pub fields: Vec<String>,
    /// The command (id, params) a click runs.
    pub command: Option<(String, Value)>,
    /// One entry per icon column of its tab; `None` (or a missing entry) draws nothing there.
    pub cells: Vec<Option<Cell>>,
}

/// One palette tab: its label and its text and icon column header labels, left to right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tab {
    pub name: &'static str,
    pub text_columns: &'static [&'static str],
    pub icon_columns: &'static [&'static str],
}

/// The rows for a drawing and its edit space.
pub type RowSource = fn(&Drawing, &Space) -> Vec<Row>;

static SOURCE: OnceLock<RowSource> = OnceLock::new();

/// Register the row source. Once per process; a second call is an error.
pub fn register_row_source(f: RowSource) -> Result<(), &'static str> {
    SOURCE.set(f).map_err(|_| "view list row source already registered")
}

static TABS: OnceLock<&'static [Tab]> = OnceLock::new();

/// Register the palette's tabs, left to right. Once per process; a second call is an error.
pub fn register_tabs(tabs: &'static [Tab]) -> Result<(), &'static str> {
    TABS.set(tabs).map_err(|_| "view list tabs already registered")
}

/// The registered tabs; empty when none are.
pub fn tabs() -> &'static [Tab] {
    TABS.get().copied().unwrap_or(&[])
}

/// The rows for the app's current drawing; empty when nothing is registered or no drawing is open.
pub fn rows(app: &CadApp) -> Vec<Row> {
    match (SOURCE.get(), app.session.doc(), app.session.state()) {
        (Some(f), Ok(d), Ok(st)) => f(d, &st.space),
        _ => Vec::new(),
    }
}

/// The rows the palette draws: all but Model, which the status bar's views popup offers (LAMB T606).
pub fn palette_rows(rows: Vec<Row>) -> Vec<Row> {
    rows.into_iter().filter(|r| r.label != "Model").collect()
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
    let rows = palette_rows(rows(app));
    show_tabs(app, ui, tabs(), rows);
}

/// The selected tab's index, kept in egui temp memory for the session (never saved); 0 by default.
fn selected_tab(ui: &egui::Ui, id: egui::Id, count: usize) -> usize {
    let i = ui.ctx().data(|d| d.get_temp::<usize>(id)).unwrap_or(0);
    if i < count { i } else { 0 }
}

/// `text` in the body font, elided with `…` to `width`; `true` when it was cut.
fn fit(ui: &egui::Ui, text: &str, width: f32, color: egui::Color32) -> (Arc<egui::Galley>, bool) {
    let g = egui::WidgetText::from(egui::RichText::new(text).color(color)).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        width.max(1.0),
        crate::theme::body(),
    );
    let elided = g.elided;
    (g, elided)
}

/// The tab strip, when there are two or more tabs.
fn tab_strip(ui: &mut egui::Ui, tabs: &[Tab], id: egui::Id, current: usize) {
    if tabs.len() < 2 {
        return;
    }
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        for (i, tab) in tabs.iter().enumerate() {
            if ui.selectable_label(i == current, crate::i18n::t(tab.name)).clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(id, i));
            }
        }
    });
    ui.add_space(2.0);
}

/// The header row: `Name`, the tab's text-column names, then its icon-column names.
fn header(ui: &mut egui::Ui, tab: &Tab, fields_w: f32, cells_w: f32) {
    if tab.text_columns.is_empty() && tab.icon_columns.is_empty() {
        return;
    }
    let t = Tokens::get();
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let (r, _) = ui.allocate_exact_size(vec2((ui.available_width() - 10.0).max(fields_w + cells_w), 18.0), Sense::hover());
        let dim = |x: f32, align, name: &str| {
            ui.painter().text(egui::pos2(x, r.center().y), align, crate::i18n::t(name), crate::theme::body(), t.text_dim);
        };
        let fields_left = r.right() - cells_w - fields_w;
        dim(r.left() + 4.0, egui::Align2::LEFT_CENTER, "Name");
        for (c, name) in tab.text_columns.iter().enumerate() {
            dim(fields_left + c as f32 * TEXT_COL_W + 4.0, egui::Align2::LEFT_CENTER, name);
        }
        for (c, name) in tab.icon_columns.iter().enumerate() {
            dim(r.right() - cells_w + (c as f32 + 0.5) * COL_W, egui::Align2::CENTER_CENTER, name);
        }
    });
}

/// The palette body for `tabs` and `rows` (each row on its own `tab`); see the module docs.
fn show_tabs(app: &mut CadApp, ui: &mut egui::Ui, tabs: &[Tab], rows: Vec<Row>) {
    let t = Tokens::get();
    ui.add_space(4.0);
    let tab_id = ui.scope_id().with("viewlist-tab");
    let current = selected_tab(ui, tab_id, tabs.len());
    tab_strip(ui, tabs, tab_id, current);
    let rows: Vec<Row> = rows.into_iter().filter(|r| r.tab == current).collect();
    if rows.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.label(egui::RichText::new(crate::i18n::t("No views")).color(t.text_dim));
        });
        return;
    }
    let tab = tabs.get(current);
    let text_cols = tab.map_or(0, |t| t.text_columns.len()).max(rows.iter().map(|r| r.fields.len()).max().unwrap_or(0));
    let cols = tab.map_or(0, |t| t.icon_columns.len()).max(rows.iter().map(|r| r.cells.len()).max().unwrap_or(0));
    let fields_w = text_cols as f32 * TEXT_COL_W;
    let cells_w = cols as f32 * COL_W;
    if let Some(tab) = tab {
        header(ui, tab, fields_w, cells_w);
    }
    let mut clicked = None;
    let mut cell_clicked = None;
    for (i, row) in rows.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(8.0);
            let label_w = (ui.available_width() - 10.0 - cells_w - fields_w).max(40.0);
            if row.heading {
                let (r, _) = ui.allocate_exact_size(vec2(label_w + fields_w, ROW_H), Sense::hover());
                ui.painter().text(egui::pos2(r.left() + 2.0, r.center().y), egui::Align2::LEFT_CENTER, &row.label, crate::theme::body(), t.text_dim);
                return;
            }
            if text_row(ui, row, label_w, text_cols) && row.command.is_some() {
                clicked = Some(i);
            }
            if let Some(c) = cell_row(ui, row, cols) {
                cell_clicked = Some((i, c));
            }
        });
    }
    if let Some((i, c)) = cell_clicked {
        let _ = activate_cell(app, &rows[i], c);
    } else if let Some(i) = clicked {
        let _ = activate(app, &rows[i]);
    }
}

/// A row's label and fields as one selectable rect, each left-aligned and elided (the full text on
/// hover); its accessible label is the row's label. `true` on a click.
fn text_row(ui: &mut egui::Ui, row: &Row, label_w: f32, text_cols: usize) -> bool {
    let t = Tokens::get();
    let w = label_w + text_cols as f32 * TEXT_COL_W;
    let (r, resp) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, row.active, &row.label));
    if row.active {
        ui.painter().rect_filled(r, 2.0, ui.visuals().selection.bg_fill);
    } else if resp.hovered() {
        ui.painter().rect_filled(r, 2.0, t.hover);
    }
    let color = if row.active { ui.visuals().selection.stroke.color } else { t.text };
    let mut cut = Vec::new();
    let texts = std::iter::once((row.label.as_str(), r.left(), label_w))
        .chain(row.fields.iter().take(text_cols).enumerate().map(|(c, f)| (f.as_str(), r.left() + label_w + c as f32 * TEXT_COL_W, TEXT_COL_W)));
    for (text, left, width) in texts {
        let (g, elided) = fit(ui, text, width - 8.0, color);
        ui.painter().galley(egui::pos2(left + 4.0, r.center().y - g.size().y / 2.0), g, color);
        if elided {
            cut.push(text);
        }
    }
    let resp = if cut.is_empty() { resp } else { resp.on_hover_text(cut.join(" · ")) };
    resp.clicked()
}

/// A row's icon cells in `cols` fixed-width columns; the clicked column, if any.
fn cell_row(ui: &mut egui::Ui, row: &Row, cols: usize) -> Option<usize> {
    let t = Tokens::get();
    let mut clicked = None;
    for c in 0..cols {
        let Some(cell) = row.cells.get(c).and_then(Option::as_ref) else {
            ui.allocate_exact_size(vec2(COL_W, ROW_H), Sense::hover());
            continue;
        };
        let (r, resp) = ui.allocate_exact_size(vec2(COL_W, ROW_H), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, cell.on, &cell.tip));
        if resp.hovered() {
            ui.painter().rect_filled(egui::Rect::from_center_size(r.center(), vec2(20.0, 20.0)), 3.0, t.hover);
        }
        icons::paint(ui.painter(), egui::Rect::from_center_size(r.center(), vec2(16.0, 16.0)), cell.icon, !cell.on);
        if resp.on_hover_text(&cell.tip).clicked() {
            clicked = Some(c);
        }
    }
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_engine::Session;
    use serde_json::json;

    const TWO: [Tab; 2] = [
        Tab { name: "Views", text_columns: &["Level"], icon_columns: &["Col"] },
        Tab { name: "Sheets", text_columns: &["Title"], icon_columns: &["Col"] },
    ];
    const ONE: [Tab; 1] = [Tab { name: "Views", text_columns: &["Level"], icon_columns: &["Col"] }];

    fn row(label: &str, tab: usize, heading: bool, command: Option<(String, Value)>, cells: Vec<Option<Cell>>) -> Row {
        Row { label: label.into(), active: false, heading, tab, fields: vec!["F".into()], command, cells }
    }

    /// Model, then one row per layout; a click switches to it. Active = the edit space.
    fn source(d: &Drawing, space: &Space) -> Vec<Row> {
        let mut v = vec![row("Spaces", 0, true, None, vec![])];
        let mut model = row("Model", 0, false, Some(("layout.set".into(), json!({"name": "Model"}))), vec![None]);
        model.active = *space == Space::Model;
        v.push(model);
        for l in &d.layouts {
            // A cell that switches to model space, so a click on it is told apart from the row's own.
            let cell =
                Cell { icon: Icon::Layers, on: false, tip: format!("Cell {}", l.name), command: ("layout.set".into(), json!({"name": "Model"})) };
            v.push(row(&l.name, 0, false, Some(("layout.set".into(), json!({"name": l.name}))), vec![Some(cell)]));
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
    fn the_palette_draws_every_row_but_model() {
        let kept =
            palette_rows(vec![row("Model", 0, false, None, vec![]), row("Level 1", 0, false, None, vec![]), row("Sheets", 1, true, None, vec![])]);
        assert_eq!(kept.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(), ["Level 1", "Sheets"]);

        let _ = register_row_source(source);
        let app = CadApp::new(Session::default(), crate::Services::default());
        let mut h = egui_kittest::Harness::builder().with_size(vec2(240.0, 300.0)).build_ui_state(|ui, app: &mut CadApp| show(app, ui), app);
        h.run();
        use egui_kittest::kittest::Queryable;
        assert!(h.query_by_label("Model").is_none());
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

    /// The test source with every layout row moved to tab 1.
    fn on_two_tabs(app: &CadApp) -> Vec<Row> {
        let mut rows = palette_rows(rows(app));
        for r in rows.iter_mut().filter(|r| !r.heading) {
            r.tab = 1;
        }
        rows
    }

    #[test]
    fn two_tabs_draw_a_strip_and_each_shows_only_its_rows() {
        let _ = register_row_source(source);
        let _ = register_tabs(&TWO);
        assert!(register_tabs(&TWO).is_err(), "one tab set per process");
        let app = CadApp::new(Session::default(), crate::Services::default());
        let name = app.session.doc().unwrap().layouts.first().map(|l| l.name.clone()).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(360.0, 300.0)).build_ui_state(
            |ui, app: &mut CadApp| {
                let rows = on_two_tabs(app);
                show_tabs(app, ui, &TWO, rows);
            },
            app,
        );
        h.run();
        use egui_kittest::kittest::Queryable;
        assert!(h.query_by_label("Views").is_some() && h.query_by_label("Sheets").is_some(), "a tab strip");
        assert!(h.query_by_label(&name).is_none(), "tab 0 is selected: no tab-1 rows");
        h.get_by_label("Sheets").click();
        h.run();
        assert!(h.query_by_label(&name).is_some(), "Sheets shows the tab-1 rows");

        // A cell click runs the cell's command; a row click the row's.
        h.get_by_label(&format!("Cell {name}")).click();
        h.run();
        assert_eq!(h.state().session.state().unwrap().space, Space::Model);
        h.get_by_label(&name).click();
        h.run();
        assert_eq!(h.state().session.state().unwrap().space, Space::Paper(name.clone()));
        h.get_by_label("Views").click();
        h.run();
        assert!(h.query_by_label(&name).is_none(), "back on Views");
    }

    #[test]
    fn one_tab_draws_no_strip() {
        let _ = register_row_source(source);
        let app = CadApp::new(Session::default(), crate::Services::default());
        let name = app.session.doc().unwrap().layouts.first().map(|l| l.name.clone()).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(360.0, 300.0)).build_ui_state(
            |ui, app: &mut CadApp| {
                let rows = palette_rows(rows(app));
                show_tabs(app, ui, &ONE, rows);
            },
            app,
        );
        h.run();
        use egui_kittest::kittest::Queryable;
        assert!(h.query_by_label("Views").is_none(), "no strip");
        assert!(h.query_by_label(&name).is_some());
    }
}

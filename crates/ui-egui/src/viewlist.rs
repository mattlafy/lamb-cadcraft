//! A generic list palette (fork-only, LAMB T343): rows come from one registered [`RowSource`],
//! a row can be a dim caption (`heading`), highlighted (`active`), and run a command when clicked.
//! It knows nothing of what the rows are; the LAMB desktop app registers its views into it.

use std::sync::OnceLock;

use egui::{Sense, vec2};
use serde_json::Value;

use cadcraft_doc::{Drawing, Space};

use crate::CadApp;
use crate::theme::Tokens;

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
}

/// The rows for a drawing and its edit space.
pub type RowSource = fn(&Drawing, &Space) -> Vec<Row>;

static SOURCE: OnceLock<RowSource> = OnceLock::new();

/// Register the row source. Once per process; a second call is an error.
pub fn register_row_source(f: RowSource) -> Result<(), &'static str> {
    SOURCE.set(f).map_err(|_| "view list row source already registered")
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
    let mut clicked = None;
    for (i, row) in rows.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if row.heading {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width() - 10.0, 20.0), Sense::hover());
                ui.painter().text(egui::pos2(r.left() + 2.0, r.center().y), egui::Align2::LEFT_CENTER, &row.label, crate::theme::body(), t.text_dim);
                return;
            }
            let r = ui.add_sized(vec2(ui.available_width() - 10.0, 20.0), egui::Button::selectable(row.active, &row.label));
            if r.clicked() && row.command.is_some() {
                clicked = Some(i);
            }
        });
    }
    if let Some(i) = clicked {
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
        let mut v = vec![Row { label: "Spaces".into(), active: false, heading: true, command: None }];
        v.push(Row {
            label: "Model".into(),
            active: *space == Space::Model,
            heading: false,
            command: Some(("layout.set".into(), json!({"name": "Model"}))),
        });
        for l in &d.layouts {
            v.push(Row { label: l.name.clone(), active: false, heading: false, command: Some(("layout.set".into(), json!({"name": l.name}))) });
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
}

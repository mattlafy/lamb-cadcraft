//! A property-group slot in the Properties palette (fork-only, LAMB T361): one registered
//! [`PropertySource`] turns the selection into an extra group of rows drawn under Geometry. A row is
//! read-only text or a drop-down of choices, each running a command; a group with more than one
//! segment gets a "Segment ‹ i / n ›" stepper, and the stepped index goes back to the source. It
//! knows nothing of what the rows mean; the LAMB desktop app registers its Role and height rows.
//!
//! A second source (LAMB T365), [`register_picked_source`], supplies a group for the object the
//! engine's idle-pick hook picked (`Session::picked`) while nothing is selected; [`show_picked`]
//! draws it in place of the current-properties rows. A row may also be an editable field
//! ([`PropEdit`]) whose committed text runs a command.

use std::sync::OnceLock;

use cadcraft_doc::{Drawing, Handle};
use serde_json::Value;

use super::{edit_field, group, number, pick_menu, prop_row, read_only, report_bad, value_box};
use crate::CadApp;

/// One row of the group.
#[derive(Clone, Debug, PartialEq)]
pub struct PropRow {
    pub label: String,
    pub value: String,
    /// Hover text on the value.
    pub tip: Option<String>,
    /// (label, command id, params); empty → a read-only value (unless `edit`).
    pub choices: Vec<(String, String, Value)>,
    /// An editable field committing into a command; a row has this or `choices` or neither.
    pub edit: Option<PropEdit>,
}

/// An editable row: on commit the typed text goes into `params[key]` — a string, or when `number`
/// a number read as the command line reads distances (unreadable → reported) — and `command` runs.
#[derive(Clone, Debug, PartialEq)]
pub struct PropEdit {
    pub command: String,
    pub params: Value,
    pub key: String,
    pub number: bool,
}

/// The extra group: a title, how many segments the stepper walks (0 or 1 → no stepper), its rows.
#[derive(Clone, Debug, PartialEq)]
pub struct PropGroup {
    pub title: String,
    pub segments: usize,
    pub rows: Vec<PropRow>,
}

/// The group for a drawing, its selected handles and the stepped segment; `None` draws nothing.
pub type PropertySource = fn(&Drawing, &[Handle], usize) -> Option<PropGroup>;

static SOURCE: OnceLock<PropertySource> = OnceLock::new();

/// The group for a drawing and the picked object's key; `None` draws nothing.
pub type PickedSource = fn(&Drawing, &str) -> Option<PropGroup>;

static PICKED: OnceLock<PickedSource> = OnceLock::new();

/// Register the picked-object source. Once per process; a second call is an error.
pub fn register_picked_source(f: PickedSource) -> Result<(), &'static str> {
    PICKED.set(f).map_err(|_| "picked source already registered")
}

/// The group for the picked object: `None` unless a source is registered, nothing is selected and
/// the session has a picked object the source knows.
pub fn picked_rows(app: &CadApp) -> Option<PropGroup> {
    let f = PICKED.get()?;
    if !app.session.selection().is_empty() {
        return None;
    }
    let key = app.session.picked()?;
    f(app.session.doc().ok()?, &key)
}

/// Draws the picked object's group; whether anything was drawn.
pub fn show_picked(app: &mut CadApp, ui: &mut egui::Ui) -> bool {
    let Some(g) = picked_rows(app) else { return false };
    let key = app.session.picked().unwrap_or_default();
    group(ui, &g.title);
    let run = rows(ui, &g.rows, egui::Id::unique(("lamb-props-picked", key)));
    finish(app, run);
    true
}

/// What a drawn row asked for: a command to run, or typed text that wasn't a number.
type Run = (Option<(String, Value)>, Option<String>);

/// Draws `rows` (shared by both groups); `id` keys the editable fields.
fn rows(ui: &mut egui::Ui, rows: &[PropRow], id: egui::Id) -> Run {
    let mut run: Option<(String, Value)> = None;
    let mut bad: Option<String> = None;
    for r in rows {
        prop_row(ui, &r.label, |ui| {
            if let Some(e) = &r.edit {
                let typed = edit_field(ui, id.with(&r.label), r.value.clone());
                let v = if e.number { number(typed, &mut bad).map(Value::from) } else { typed.map(Value::from) };
                if let Some(v) = v {
                    let mut params = e.params.clone();
                    if let Some(o) = params.as_object_mut() {
                        o.insert(e.key.clone(), v);
                    }
                    run = Some((e.command.clone(), params));
                }
            } else if r.choices.is_empty() {
                match &r.tip {
                    Some(tip) => {
                        value_box(ui, &r.value, false, false).on_hover_text(tip);
                    }
                    None => read_only(ui, &r.value),
                }
            } else if let Some(picked) = pick_menu(ui, &r.value, r.choices.iter().map(|c| c.0.clone()))
                && let Some((_, id, params)) = r.choices.iter().find(|c| c.0 == picked)
            {
                run = Some((id.clone(), params.clone()));
            }
        });
    }
    (run, bad)
}

fn finish(app: &mut CadApp, (run, bad): Run) {
    report_bad(app, bad);
    if let Some((id, params)) = run {
        let _ = app.run(&id, params);
    }
}

/// Register the property source. Once per process; a second call is an error.
pub fn register_property_source(f: PropertySource) -> Result<(), &'static str> {
    SOURCE.set(f).map_err(|_| "property source already registered")
}

/// The group for the app's current selection at `segment`, clamped to the group's segment count
/// (the returned group is for the clamped segment). `None` when nothing is registered, nothing is
/// selected, no drawing is open, or the source says so.
pub fn rows_for(app: &CadApp, segment: usize) -> Option<(PropGroup, usize)> {
    let f = SOURCE.get()?;
    let sel = app.session.selection();
    if sel.is_empty() {
        return None;
    }
    let d = app.session.doc().ok()?;
    let g = f(d, &sel, segment)?;
    if g.segments > 0 && segment >= g.segments {
        let last = g.segments - 1;
        return Some((f(d, &sel, last)?, last));
    }
    Some((g, segment))
}

/// The egui key holding the stepped segment: per (first selected handle, drawing).
fn segment_key(app: &CadApp) -> Option<egui::Id> {
    let st = app.session.state().ok()?;
    let first = *st.selection.first()?;
    Some(egui::Id::unique(("lamb-props-segment", first, st.uid)))
}

pub fn show(app: &mut CadApp, ui: &mut egui::Ui) {
    let Some(key) = segment_key(app) else { return };
    let stored = ui.data(|d| d.get_temp::<usize>(key)).unwrap_or(0);
    let Some((g, seg)) = rows_for(app, stored) else { return };
    if seg != stored {
        ui.data_mut(|d| d.insert_temp(key, seg));
    }
    group(ui, &g.title);
    let mut step: Option<usize> = None;
    if g.segments > 1 {
        prop_row(ui, "Segment", |ui| {
            if ui.add_enabled(seg > 0, egui::Button::new("‹")).on_hover_text(crate::tl!("Previous segment")).clicked() {
                step = Some(seg - 1);
            }
            ui.label(format!("{} / {}", seg + 1, g.segments));
            if ui.add_enabled(seg + 1 < g.segments, egui::Button::new("›")).on_hover_text(crate::tl!("Next segment")).clicked() {
                step = Some(seg + 1);
            }
        });
    }
    let run = rows(ui, &g.rows, key);
    if let Some(i) = step {
        ui.data_mut(|d| d.insert_temp(key, i));
    }
    finish(app, run);
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_engine::Session;
    use serde_json::json;

    /// One row naming the selection size and the segment; three segments; a choice that sets PICKADD.
    fn source(_: &Drawing, sel: &[Handle], segment: usize) -> Option<PropGroup> {
        Some(PropGroup {
            title: "Test".into(),
            segments: 3,
            rows: vec![
                PropRow { label: "Count".into(), value: format!("{} @ {segment}", sel.len()), tip: None, choices: vec![], edit: None },
                PropRow {
                    label: "Pickadd".into(),
                    value: "On".into(),
                    tip: None,
                    choices: vec![("Off".into(), "setvar".into(), json!({"name": "PICKADD", "value": 0}))],
                    edit: None,
                },
            ],
        })
    }

    #[test]
    fn the_registered_source_supplies_the_group_for_the_selection() {
        let _ = register_property_source(source);
        assert!(register_property_source(source).is_err(), "one source per process");
        let mut app = CadApp::new(Session::default(), crate::Services::default());
        assert_eq!(rows_for(&app, 0), None, "nothing selected");
        app.session.set_selection(vec![Handle(0x99), Handle(0x9a)]);
        let (g, seg) = rows_for(&app, 1).unwrap();
        assert_eq!((g.title.as_str(), g.segments, seg), ("Test", 3, 1));
        assert_eq!(g.rows[0].value, "2 @ 1");
        let (g, seg) = rows_for(&app, 7).unwrap();
        assert_eq!((seg, g.rows[0].value.as_str()), (2, "2 @ 2"), "clamped to the last segment");
        assert!(segment_key(&app).is_some());
    }

    /// A group naming the picked key, with an editable row.
    fn picked_source(_: &Drawing, key: &str) -> Option<PropGroup> {
        (key != "unknown").then(|| PropGroup {
            title: "Picked".into(),
            segments: 0,
            rows: vec![PropRow {
                label: "Name".into(),
                value: key.into(),
                tip: None,
                choices: vec![],
                edit: Some(PropEdit { command: "viewprops".into(), params: json!({"mode": "rename"}), key: "name".into(), number: false }),
            }],
        })
    }

    #[test]
    fn the_picked_source_supplies_the_group_while_nothing_is_selected() {
        let _ = register_picked_source(picked_source);
        assert!(register_picked_source(picked_source).is_err(), "one picked source per process");
        let mut app = CadApp::new(Session::default(), crate::Services::default());
        assert_eq!(picked_rows(&app), None, "nothing picked");
        let space = app.session.space();
        app.session.state_mut().unwrap().picked = Some((space.clone(), "LAMB_VIEW_7".into()));
        let g = picked_rows(&app).unwrap();
        assert_eq!((g.title.as_str(), g.rows[0].value.as_str()), ("Picked", "LAMB_VIEW_7"));
        assert!(g.rows[0].edit.is_some());
        app.session.state_mut().unwrap().picked = Some((space.clone(), "unknown".into()));
        assert_eq!(picked_rows(&app), None, "the source doesn't know it");
        app.session.set_selection(vec![Handle(0x99)]);
        app.session.state_mut().unwrap().picked = Some((space, "LAMB_VIEW_7".into()));
        assert_eq!(picked_rows(&app), None, "a non-empty selection shows no picked form");
    }
}

//! A property-group slot in the Properties palette (fork-only, LAMB T361): one registered
//! [`PropertySource`] turns the selection into an extra group of rows drawn under Geometry. A row is
//! read-only text or a drop-down of choices, each running a command; a group with more than one
//! segment gets a "Segment ‹ i / n ›" stepper, and the stepped index goes back to the source. It
//! knows nothing of what the rows mean; the LAMB desktop app registers its Role and height rows.

use std::sync::OnceLock;

use cadcraft_doc::{Drawing, Handle};
use serde_json::Value;

use super::{group, pick_menu, prop_row, read_only, value_box};
use crate::CadApp;

/// One row of the group.
#[derive(Clone, Debug, PartialEq)]
pub struct PropRow {
    pub label: String,
    pub value: String,
    /// Hover text on the value.
    pub tip: Option<String>,
    /// (label, command id, params); empty → a read-only value.
    pub choices: Vec<(String, String, Value)>,
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
    Some(egui::Id::new(("lamb-props-segment", first, st.uid)))
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
    let mut run: Option<(String, Value)> = None;
    for r in &g.rows {
        prop_row(ui, &r.label, |ui| {
            if r.choices.is_empty() {
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
    if let Some(i) = step {
        ui.data_mut(|d| d.insert_temp(key, i));
    }
    if let Some((id, params)) = run {
        let _ = app.run(&id, params);
    }
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
                PropRow { label: "Count".into(), value: format!("{} @ {segment}", sel.len()), tip: None, choices: vec![] },
                PropRow {
                    label: "Pickadd".into(),
                    value: "On".into(),
                    tip: None,
                    choices: vec![("Off".into(), "setvar".into(), json!({"name": "PICKADD", "value": 0}))],
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
}

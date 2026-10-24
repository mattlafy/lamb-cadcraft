//! A layer-cell slot in the Layers palette's layer list (fork-only, LAMB T362): one registered
//! [`LayerCellSource`] gives each layer an extra cell drawn at the end of its row, read-only text
//! or a drop-down of choices each running a command. It knows nothing of what the cell means; the
//! LAMB desktop app registers its default-role column. The LAYER manager dialog is untouched.
//!
//! A second slot, [`LayerHiddenSource`], names the layers the app keeps for itself: they stay in
//! the drawing and its output but are left out of the Layers palette, the LAYER manager and the
//! Properties layer drop-downs ([`visible_layers`]).

use std::sync::OnceLock;

use cadcraft_doc::{Drawing, Layer};
use serde_json::Value;

use super::{pick_menu, value_box};

/// One layer's cell.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerCell {
    pub value: String,
    /// Hover text on the cell.
    pub tip: Option<String>,
    /// (label, command id, params); empty → read-only text.
    pub choices: Vec<(String, String, Value)>,
}

/// The cell for a drawing's layer by name; `None` draws nothing.
pub type LayerCellSource = fn(&Drawing, &str) -> Option<LayerCell>;

static SOURCE: OnceLock<LayerCellSource> = OnceLock::new();

/// Register the layer-cell source. Once per process; a second call is an error.
pub fn register_layer_cell(f: LayerCellSource) -> Result<(), &'static str> {
    SOURCE.set(f).map_err(|_| "layer cell source already registered")
}

/// `layer`'s cell, or `None` when nothing is registered or the source says so.
pub fn cell_for(d: &Drawing, layer: &str) -> Option<LayerCell> {
    SOURCE.get()?(d, layer)
}

/// Whether a drawing's layer, by name, is one the app keeps for itself.
pub type LayerHiddenSource = fn(&Drawing, &str) -> bool;

static HIDDEN: OnceLock<LayerHiddenSource> = OnceLock::new();

/// Register the hidden-layer predicate. Once per process; a second call is an error.
pub fn register_layer_hidden(f: LayerHiddenSource) -> Result<(), &'static str> {
    HIDDEN.set(f).map_err(|_| "layer hidden source already registered")
}

/// Whether `layer` is a layer the app keeps for itself; false when nothing is registered.
pub fn hidden(d: &Drawing, layer: &str) -> bool {
    HIDDEN.get().is_some_and(|f| f(d, layer))
}

/// The drawing's layers that layer lists offer, in drawing order.
pub fn visible_layers(d: &Drawing) -> impl Iterator<Item = &Layer> {
    visible_layers_by(d, hidden)
}

fn visible_layers_by(d: &Drawing, hide: LayerHiddenSource) -> impl Iterator<Item = &Layer> {
    d.layers.iter().filter(move |l| !hide(d, &l.name))
}

/// Draw `cell` (≈90 px) and return the picked choice's (command id, params).
pub fn show(ui: &mut egui::Ui, cell: &LayerCell) -> Option<(String, Value)> {
    let r = ui.scope(|ui| {
        ui.set_max_width(90.0);
        if cell.choices.is_empty() {
            value_box(ui, &cell.value, false, false);
            return None;
        }
        let picked = pick_menu(ui, &cell.value, cell.choices.iter().map(|c| c.0.clone()))?;
        cell.choices.iter().find(|c| c.0 == picked).map(|(_, id, params)| (id.clone(), params.clone()))
    });
    if let Some(tip) = &cell.tip {
        r.response.on_hover_text(tip);
    }
    r.inner
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source(_: &Drawing, layer: &str) -> Option<LayerCell> {
        (layer != "skip").then(|| LayerCell {
            value: format!("role of {layer}"),
            tip: None,
            choices: vec![("Seen".into(), "layerrole".into(), json!({ "layer": layer, "role": "seen" }))],
        })
    }

    #[test]
    fn the_registered_source_supplies_each_layers_cell() {
        let d = Drawing::default();
        let _ = register_layer_cell(source);
        assert!(register_layer_cell(source).is_err(), "one source per process");
        let c = cell_for(&d, "A-WALL").unwrap();
        assert_eq!(c.value, "role of A-WALL");
        assert_eq!(c.choices[0].2, json!({ "layer": "A-WALL", "role": "seen" }));
        assert_eq!(cell_for(&d, "skip"), None);
    }

    fn internal(_: &Drawing, layer: &str) -> bool {
        layer.starts_with("APP_")
    }

    #[test]
    fn visible_layers_drop_exactly_the_hidden_ones() {
        let mut d = Drawing::default();
        for n in ["A-WALL", "APP_1", "B-DOOR", "APP_2"] {
            d.layers.push(Layer { name: n.into(), ..Layer::default() });
        }
        // Nothing in this crate registers a predicate, so every layer is offered.
        assert!(!hidden(&d, "APP_1"));
        assert_eq!(visible_layers(&d).count(), d.layers.len());
        let names: Vec<&str> = visible_layers_by(&d, internal).map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["0", "Defpoints", "A-WALL", "B-DOOR"]);
    }
}

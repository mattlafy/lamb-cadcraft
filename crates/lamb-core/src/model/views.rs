//! Placement, views, levels, project defaults and the project (TS `views.ts`, `project.ts`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::entities::{DrawingEntity, DrawingLayer, default_layers};
use super::{Field, Id, LinkRecord};
use crate::geometry::hatch_pattern::PatternDefinition;
use crate::geometry::{Polyline2, Transform2D};

pub const DEFAULT_CUT_HEIGHT_MM: f64 = 1000.0;
pub const DEFAULT_FLOOR_TO_FLOOR_MM: f64 = 3000.0;
/// 2 (T58): virtual corners on `PolySegment.cornerAtTo`. 3 (T94): circles became two-arc polylines.
pub const CURRENT_SCHEMA_VERSION: u32 = 3;

/// Where a view sits in the model. `None` on a view means floating.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum Placement {
    Plan {
        level_id: Id,
        cut_elevation: f64,
        transform: Transform2D,
    },
    Vertical {
        cut_line: Polyline2,
        host_view_id: Id,
        transform: Transform2D,
        /// Reverses which way the view looks, nothing else. Absent means not flipped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        flipped: Option<bool>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanView {
    pub id: Id,
    pub name: String,
    pub entities: Vec<DrawingEntity>,
    /// Always a `Placement::Plan` when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<Placement>,
}

/// Label only: never affects geometry or authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerticalIntent {
    Section,
    Elevation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerticalView {
    pub id: Id,
    pub name: String,
    pub intent: VerticalIntent,
    pub entities: Vec<DrawingEntity>,
    /// Always a `Placement::Vertical` when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<Placement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CutDirection {
    Vertical,
    Plan,
}

/// Tier 2 / R3+.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailView {
    pub id: Id,
    pub name: String,
    pub entities: Vec<DrawingEntity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<Placement>,
    pub condition_type: String,
    pub host_element_ids: Vec<Id>,
    pub cut_direction: CutDirection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AnyView {
    Plan(PlanView),
    Vertical(VerticalView),
    Detail(DetailView),
}

impl AnyView {
    pub fn id(&self) -> Id {
        match self {
            AnyView::Plan(v) => v.id,
            AnyView::Vertical(v) => v.id,
            AnyView::Detail(v) => v.id,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            AnyView::Plan(v) => &v.name,
            AnyView::Vertical(v) => &v.name,
            AnyView::Detail(v) => &v.name,
        }
    }

    pub fn entities(&self) -> &[DrawingEntity] {
        match self {
            AnyView::Plan(v) => &v.entities,
            AnyView::Vertical(v) => &v.entities,
            AnyView::Detail(v) => &v.entities,
        }
    }

    pub fn placement(&self) -> Option<&Placement> {
        match self {
            AnyView::Plan(v) => v.placement.as_ref(),
            AnyView::Vertical(v) => v.placement.as_ref(),
            AnyView::Detail(v) => v.placement.as_ref(),
        }
    }
}

pub fn is_plan_view(v: &AnyView) -> bool {
    matches!(v, AnyView::Plan(_))
}

pub fn is_vertical_view(v: &AnyView) -> bool {
    matches!(v, AnyView::Vertical(_))
}

pub fn is_placed(v: &AnyView) -> bool {
    v.placement().is_some()
}

/// A named floor elevation. Not an authority: Z ultimately comes from lines and links.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Level {
    pub id: Id,
    pub name: String,
    pub order: f64,
    pub floor_elevation: Field<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Units {
    #[serde(rename = "mm")]
    Mm,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDefaults {
    pub default_cut_height_mm: f64,
    pub default_floor_to_floor_mm: f64,
    /// Fallback vertical extent for a line that resolves with no links.
    pub default_surface_height_mm: f64,
    /// Placeholder extrusion depth for a closed shape drawn in a vertical view (display only).
    pub default_surface_depth_mm: f64,
    /// Plot scale denominator: 50 means 1:50.
    pub plot_scale: f64,
    /// Dimension text height on paper; model height = this * `plot_scale`.
    pub dim_text_height_paper_mm: f64,
    pub units: Units,
}

pub fn default_project_defaults() -> ProjectDefaults {
    ProjectDefaults {
        default_cut_height_mm: DEFAULT_CUT_HEIGHT_MM,
        default_floor_to_floor_mm: DEFAULT_FLOOR_TO_FLOOR_MM,
        default_surface_height_mm: 2700.0,
        default_surface_depth_mm: 2700.0,
        plot_scale: 50.0,
        dim_text_height_paper_mm: 3.0,
        units: Units::Mm,
    }
}

/// The whole in-memory document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub schema_version: u32,
    pub name: String,
    pub defaults: ProjectDefaults,
    pub levels: Vec<Level>,
    pub layers: Vec<DrawingLayer>,
    pub views: Vec<AnyView>,
    pub links: Vec<LinkRecord>,
    /// Opaque: CADCraft owns undo.
    pub command_log: Vec<Value>,
    /// Every hatch pattern this project can create a new hatch with, by name, fully parsed.
    pub patterns: BTreeMap<String, PatternDefinition>,
    /// `aiOrigin.proposalId` slots an unaccepted AI line was erased from; the sweep skips them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppressed_proposals: Option<Vec<String>>,
}

/// `name` defaults to `"Untitled Project"`.
pub fn empty_project(name: Option<&str>) -> Project {
    Project {
        schema_version: CURRENT_SCHEMA_VERSION,
        name: name.unwrap_or("Untitled Project").to_owned(),
        defaults: default_project_defaults(),
        levels: Vec::new(),
        layers: default_layers(),
        views: Vec::new(),
        links: Vec::new(),
        command_log: Vec::new(),
        patterns: BTreeMap::new(),
        suppressed_proposals: None,
    }
}

pub fn find_view(project: &Project, view_id: Id) -> Option<&AnyView> {
    project.views.iter().find(|v| v.id() == view_id)
}

pub fn find_layer<'a>(project: &'a Project, layer_id: &str) -> Option<&'a DrawingLayer> {
    project.layers.iter().find(|l| l.id == layer_id)
}

pub fn find_level(project: &Project, level_id: Id) -> Option<&Level> {
    project.levels.iter().find(|l| l.id == level_id)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::model::with_string_ids;
    use serde_json::json;

    /// A string-id project document round-trips byte-for-byte (as JSON values) through the types.
    #[test]
    fn string_id_project_round_trips() {
        let poly = json!({"closed": false, "segments": [{"bulge": 0, "to": {"x": 1, "y": 0}}], "start": {"x": 0, "y": 0}});
        let t = json!({"originX": 0, "originY": 0, "rotation": 0});
        let doc = json!({
            "schemaVersion": 3, "name": "P", "defaults": serde_json::to_value(default_project_defaults()).unwrap(),
            "levels": [{"id": "lvl-1", "name": "L1", "order": 0, "floorElevation": {"value": 0, "provenance": {"kind": "defaulted", "sourceDefaultId": "d"}}}],
            "layers": serde_json::to_value(default_layers()).unwrap(),
            "views": [
                {"kind": "plan", "id": "v1", "name": "Plan", "placement": {"kind": "plan", "levelId": "lvl-1", "cutElevation": 1000, "transform": t},
                 "entities": [
                    {"id": "e1", "viewId": "v1", "role": "bylayer", "layerId": "layer-0", "geometry": {"kind": "polyline", "geometry": poly},
                     "aiOrigin": {"proposalId": "p", "accepted": false, "sourceEntityId": "e2", "producer": "rule"}},
                    {"id": "e2", "viewId": "v1", "role": "view_clip", "layerId": "layer-0", "geometry": {"kind": "text", "position": {"x": 0, "y": 0}, "content": "x", "height": 1, "rotation": 0}, "lineweightOverride": 0.5},
                    {"id": "e3", "viewId": "v1", "role": "annotation", "layerId": "layer-0", "geometry": {"kind": "hatch", "pattern": "ANSI31", "scale": 1, "angle": 0, "loops": [{"polyline": poly, "entityIds": ["e1"]}]}},
                    {"id": "e4", "viewId": "v1", "role": "cut", "layerId": "layer-0", "geometry": {"kind": "circle", "center": {"x": 0, "y": 0}, "radius": 2}}
                 ]},
                {"kind": "vertical", "id": "v2", "name": "S", "intent": "section", "entities": [],
                 "placement": {"kind": "vertical", "cutLine": poly, "hostViewId": "v1", "transform": t, "flipped": true}},
                {"kind": "detail", "id": "v3", "name": "D", "entities": [], "conditionType": "eave", "hostElementIds": ["e1"], "cutDirection": "plan"}
            ],
            "links": [{"id": "k1", "a": {"entityId": "e1", "segmentIndex": 0, "segmentParam": 0.5}, "b": {"entityId": "e2", "segmentIndex": 0, "segmentParam": 0},
                       "origin": "explicit", "createdAt": "now", "createdBy": {"kind": "user", "userId": "u"}}],
            "commandLog": [{"anything": true}], "patterns": {}, "suppressedProposals": ["p"]
        });
        with_string_ids(|| {
            let p: Project = serde_json::from_value(doc.clone()).unwrap();
            assert!(is_placed(&p.views[0]) && is_vertical_view(&p.views[1]) && !is_placed(&p.views[2]));
            let v1 = p.views[0].id();
            assert_eq!(find_view(&p, v1).map(AnyView::name), Some("Plan"));
            assert_eq!(find_level(&p, p.levels[0].id).map(|l| l.name.as_str()), Some("L1"));
            let back = serde_json::to_value(&p).unwrap();
            let num = |v: &Value| serde_json::to_string(v).unwrap().replace(".0", "");
            assert_eq!(num(&back), num(&doc));
        });
    }
}

//! Line roles, entity geometry, drawing entities and drawing layers (TS `entities.ts`, `layers.ts`).

use serde::{Deserialize, Serialize};

use super::Id;
use crate::geometry::dimension::Dimension2;
use crate::geometry::{Point2, Polyline2};

/// Every drawn line carries a role. `Bylayer` defers to the entity's layer's `default_role`; read
/// an entity's actual role through [`resolve_entity_role`], never `entity.role` directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineRole {
    /// The view's cut plane passes through this material.
    Cut,
    /// Visible but not cut; Z comes from linked vertical-view lines.
    Seen,
    /// Grid line, level line: a spatial reference plane, not material.
    Datum,
    /// Break line: limits what the view shows; no 3D presence.
    ViewClip,
    /// Text, dimensions, hatch.
    Annotation,
    /// Imported underlay.
    Reference,
    /// Inherit the layer's default role.
    Bylayer,
}

/// One closed boundary ring a hatch fills. `polyline` is a snapshot; `entity_ids` are every entity
/// stitched together to form it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HatchLoop {
    pub polyline: Polyline2,
    pub entity_ids: Vec<Id>,
}

/// TS `HatchEntityGeometry` minus its `kind` tag. `boundary` / `boundary_entity_id` are the pre-T31
/// single-loop fields, read only as a fallback; use [`hatch_loops`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HatchGeometry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary: Option<Polyline2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary_entity_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loops: Option<Vec<HatchLoop>>,
    pub pattern: String,
    pub scale: f64,
    pub angle: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum EntityGeometry {
    Polyline {
        geometry: Polyline2,
    },
    /// Legacy load only: a circle is a closed two-arc polyline since schema 3.
    Circle {
        center: Point2,
        radius: f64,
    },
    Text {
        position: Point2,
        content: String,
        height: f64,
        rotation: f64,
    },
    Dimension {
        dimension: Dimension2,
    },
    Hatch(HatchGeometry),
}

/// A hatch's boundary loops, whichever shape it was saved in (`loops`, or the legacy singular
/// `boundary` / `boundary_entity_id`).
pub fn hatch_loops(geometry: &HatchGeometry) -> Vec<HatchLoop> {
    if let Some(loops) = &geometry.loops {
        return loops.clone();
    }
    match &geometry.boundary {
        Some(b) => vec![HatchLoop { polyline: b.clone(), entity_ids: geometry.boundary_entity_id.into_iter().collect() }],
        None => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AiProducer {
    Rule,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiOrigin {
    pub proposal_id: String,
    /// `false` renders with a glow.
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assumption_note: Option<String>,
    /// The plan/cut-line entity this line's height was derived from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_entity_id: Option<Id>,
    /// Geometry as the reconcile sweep itself last set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swept_geometry: Option<Polyline2>,
    /// Absent means `Rule`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<AiProducer>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrawingEntity {
    pub id: Id,
    pub view_id: Id,
    pub role: LineRole,
    pub layer_id: String,
    pub geometry: EntityGeometry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineweight_override: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_origin: Option<AiOrigin>,
}

pub fn is_line_like(entity: &DrawingEntity) -> bool {
    matches!(entity.geometry, EntityGeometry::Polyline { .. })
}

// ---------------------------------------------------------------------------------------------
// layers
// ---------------------------------------------------------------------------------------------

/// Project-wide drawing layer, keyed by its string `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrawingLayer {
    pub id: String,
    pub name: String,
    pub color: String,
    pub lineweight: f64,
    pub visible: bool,
    pub locked: bool,
    /// The concrete role a `Bylayer` entity on this layer resolves to (never `Bylayer` itself).
    pub default_role: LineRole,
}

pub fn default_layers() -> Vec<DrawingLayer> {
    let layer = |id: &str, name: &str, color: &str, lineweight: f64, locked: bool, default_role: LineRole| DrawingLayer {
        id: id.to_owned(),
        name: name.to_owned(),
        color: color.to_owned(),
        lineweight,
        visible: true,
        locked,
        default_role,
    };
    vec![
        layer("layer-0", "0", "#d0d0d0", 0.25, false, LineRole::Cut),
        layer("layer-wall-ext", "WALL-EXT", "#e0a030", 0.5, false, LineRole::Cut),
        layer("layer-wall-int", "WALL-INT", "#c8c840", 0.35, false, LineRole::Cut),
        layer("layer-grid", "GRID", "#5090e0", 0.18, false, LineRole::Datum),
        layer("layer-stair", "STAIR", "#b070d0", 0.25, false, LineRole::Cut),
        layer("layer-annotation", "ANNOTATION", "#a0a0a0", 0.18, false, LineRole::Annotation),
        layer("layer-reference", "REFERENCE", "#606060", 0.13, true, LineRole::Reference),
    ]
}

// The four rules that read `visible` / `locked` live here and only here. An invisible layer
// behaves as locked; a locked-but-visible layer still renders and snaps.

pub fn is_layer_rendered(layer: &DrawingLayer) -> bool {
    layer.visible
}

pub fn is_layer_snappable(layer: &DrawingLayer) -> bool {
    layer.visible
}

pub fn is_layer_selectable(layer: &DrawingLayer) -> bool {
    layer.visible && !layer.locked
}

pub fn is_layer_editable(layer: &DrawingLayer) -> bool {
    layer.visible && !layer.locked
}

/// An entity's actual role: `role` unless it is `Bylayer`, then the layer's `default_role`
/// (`Cut` for a dangling `layer_id`, or a layer whose default is itself `Bylayer`). Never `Bylayer`.
pub fn resolve_entity_role(layers: &[DrawingLayer], role: LineRole, layer_id: &str) -> LineRole {
    if role != LineRole::Bylayer {
        return role;
    }
    layers.iter().find(|l| l.id == layer_id).map(|l| l.default_role).filter(|r| *r != LineRole::Bylayer).unwrap_or(LineRole::Cut)
}

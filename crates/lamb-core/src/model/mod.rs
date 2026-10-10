//! LAMB's document model, ported from `packages/document-model/src/` in LAMB: plain owned
//! structs whose serde shape is the TS JSON shape (camelCase, `kind`-tagged unions), with document
//! ids as an opaque [`Id`]. The small `links` and `provenance` types live here; entities and layers
//! in [`entities`], views, levels and the project in [`views`].
//!
//! TS `T | null | undefined` collapses to `Option<T>` (absent when `None`). `generateId` is not
//! ported: id allocation belongs to the caller (CADCraft hands out handles).

pub mod entities;
pub mod id;
pub mod views;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use entities::{
    AiOrigin, AiProducer, DrawingEntity, DrawingLayer, EntityGeometry, HatchGeometry, HatchLoop, LineRole, default_layers, hatch_loops,
    is_layer_editable, is_layer_rendered, is_layer_selectable, is_layer_snappable, is_line_like, resolve_entity_role,
};
pub use id::{INTERNED_BASE, Id, with_string_ids};
pub use views::{
    AnyView, CURRENT_SCHEMA_VERSION, CutDirection, DEFAULT_CUT_HEIGHT_MM, DEFAULT_FLOOR_TO_FLOOR_MM, DetailView, Level, Placement, PlanView, Project,
    ProjectDefaults, Units, VerticalIntent, VerticalView, default_project_defaults, empty_project, find_layer, find_level, find_view, is_placed,
    is_plan_view, is_vertical_view,
};

// ---------------------------------------------------------------------------------------------
// links
// ---------------------------------------------------------------------------------------------

/// A link end: a parameter along ONE segment of an entity, so edits to other segments don't move it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkAnchor {
    pub entity_id: Id,
    pub segment_index: usize,
    /// 0..1 along that segment only.
    pub segment_param: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkOrigin {
    Automatic,
    Explicit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkRecord {
    pub id: Id,
    pub a: LinkAnchor,
    /// In a different view from `a`.
    pub b: LinkAnchor,
    pub origin: LinkOrigin,
    pub created_at: String,
    pub created_by: Actor,
    /// Set when a cut line moved and no longer crosses this link's geometry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
}

// ---------------------------------------------------------------------------------------------
// provenance
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum Actor {
    User { user_id: String },
    Llm { proposal_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownAt {
    pub param: f64,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub view_id: Id,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum Provenance {
    Authored { view_id: Id, actor: Actor, timestamp: String },
    Defaulted { source_default_id: String },
    Ambiguous { known_at: Vec<KnownAt> },
    Conflicted { candidates: Vec<Candidate> },
}

/// A value that can come from more than one source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field<T> {
    pub value: T,
    pub provenance: Provenance,
}

pub fn authored_field<T>(value: T, view_id: Id, actor: Actor, timestamp: &str) -> Field<T> {
    Field { value, provenance: Provenance::Authored { view_id, actor, timestamp: timestamp.to_owned() } }
}

pub fn defaulted_field<T>(value: T, source_default_id: &str) -> Field<T> {
    Field { value, provenance: Provenance::Defaulted { source_default_id: source_default_id.to_owned() } }
}

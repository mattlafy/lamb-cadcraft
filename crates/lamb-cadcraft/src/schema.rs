//! Where LAMB's own data lives inside a CADCraft `Drawing` (`docs/07-architecture/
//! cadcraft-integration.md` §3.1, §4 in the LAMB repo): three extension slots, all keyed
//! [`EXT_KEY`], never a side table.
//!
//! - `Drawing.ext["LAMB"]` = [`LambDocument`]: name, defaults, levels, views (metadata only),
//!   links, suppressed proposals.
//! - `Common.ext["LAMB"]` = [`EntityData`]: an entity's line role and AI provenance.
//! - `Layer.ext["LAMB"]` = [`LayerData`]: a layer's default role.
//!
//! A view's entities live in the block [`view_block_name`]`(id)`; an entity's id is its handle.
//! Writers remove a slot whose value is all-default, so a plain CADCraft drawing's DXF output is
//! unchanged.

use std::fmt;

use cadcraft_doc::{Common, Drawing, Extension, Layer};
use lamb_core::model::{AiOrigin, AnyView, Id, Level, LineRole, LinkRecord, ProjectDefaults, empty_project};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The application name every LAMB extension slot is stored under.
pub const EXT_KEY: &str = "LAMB";

/// This schema's own version counter (not lamb-core's `CURRENT_SCHEMA_VERSION`).
pub const SCHEMA_VERSION: u32 = 1;

/// The block holding view `id`'s entities: `LAMB_VIEW_<id>`, id in decimal.
pub fn view_block_name(id: Id) -> String {
    format!("LAMB_VIEW_{}", id.0)
}

/// Why a LAMB slot could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaError {
    /// The slot's JSON does not match the schema; `what` names the slot.
    Malformed { what: String, message: String },
    /// The document was written by a newer schema than this code reads.
    FutureVersion { found: u64, supported: u32 },
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SchemaError::Malformed { what, message } => write!(f, "malformed LAMB data in {what}: {message}"),
            SchemaError::FutureVersion { found, supported } => {
                write!(f, "LAMB document schemaVersion {found} is newer than this build reads ({supported})")
            }
        }
    }
}

impl std::error::Error for SchemaError {}

/// `Drawing.ext["LAMB"]`. Views carry no entities in storage: the writer clears them and the
/// reader drops any it finds; entities live in each view's block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LambDocument {
    pub schema_version: u32,
    pub name: String,
    pub defaults: ProjectDefaults,
    pub levels: Vec<Level>,
    pub views: Vec<AnyView>,
    pub links: Vec<LinkRecord>,
    #[serde(default)]
    pub suppressed_proposals: Vec<String>,
}

impl LambDocument {
    /// What a drawing with no slot reads as: lamb-core's empty project's name and defaults.
    pub fn empty() -> Self {
        let p = empty_project(None);
        LambDocument {
            schema_version: SCHEMA_VERSION,
            name: p.name,
            defaults: p.defaults,
            levels: Vec::new(),
            views: Vec::new(),
            links: Vec::new(),
            suppressed_proposals: Vec::new(),
        }
    }
}

impl Default for LambDocument {
    fn default() -> Self {
        Self::empty()
    }
}

fn clear_entities(views: &mut [AnyView]) {
    for v in views {
        match v {
            AnyView::Plan(p) => p.entities.clear(),
            AnyView::Vertical(p) => p.entities.clear(),
            AnyView::Detail(p) => p.entities.clear(),
        }
    }
}

fn bylayer() -> LineRole {
    LineRole::Bylayer
}

fn cut() -> LineRole {
    LineRole::Cut
}

/// `Common.ext["LAMB"]`. Absent slot or field: `role` is `Bylayer`, no AI origin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityData {
    #[serde(default = "bylayer")]
    pub role: LineRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_origin: Option<AiOrigin>,
}

impl Default for EntityData {
    fn default() -> Self {
        EntityData { role: LineRole::Bylayer, ai_origin: None }
    }
}

/// `Layer.ext["LAMB"]`. Absent: `Cut`, `resolve_entity_role`'s own fallback.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerData {
    #[serde(default = "cut")]
    pub default_role: LineRole,
}

impl Default for LayerData {
    fn default() -> Self {
        LayerData { default_role: LineRole::Cut }
    }
}

fn malformed(what: impl Into<String>, e: impl fmt::Display) -> SchemaError {
    SchemaError::Malformed { what: what.into(), message: e.to_string() }
}

fn read_slot<T: DeserializeOwned + Default>(ext: &Extension, what: impl FnOnce() -> String) -> Result<T, SchemaError> {
    match ext.get(EXT_KEY) {
        None => Ok(T::default()),
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| malformed(what(), e)),
    }
}

fn write_slot<T: Serialize + Default + PartialEq>(ext: &mut Extension, data: &T, what: &str) -> Result<(), SchemaError> {
    if *data == T::default() {
        ext.remove(EXT_KEY);
        return Ok(());
    }
    let v = serde_json::to_value(data).map_err(|e| malformed(what, e))?;
    ext.insert(EXT_KEY.to_owned(), v);
    Ok(())
}

/// The drawing's LAMB document; an absent slot is [`LambDocument::empty`].
pub fn read_document(d: &Drawing) -> Result<LambDocument, SchemaError> {
    let Some(v) = d.ext.get(EXT_KEY) else { return Ok(LambDocument::empty()) };
    let what = "Drawing.ext[\"LAMB\"]";
    let found = v.get("schemaVersion").and_then(Value::as_u64).ok_or_else(|| malformed(what, "missing or non-integer schemaVersion"))?;
    if found > u64::from(SCHEMA_VERSION) {
        return Err(SchemaError::FutureVersion { found, supported: SCHEMA_VERSION });
    }
    let mut doc: LambDocument = serde_json::from_value(v.clone()).map_err(|e| malformed(what, e))?;
    clear_entities(&mut doc.views);
    Ok(doc)
}

/// Stores `doc` (views' entities cleared, `schemaVersion` set to [`SCHEMA_VERSION`]).
pub fn write_document(d: &mut Drawing, doc: &LambDocument) -> Result<(), SchemaError> {
    let mut doc = doc.clone();
    doc.schema_version = SCHEMA_VERSION;
    clear_entities(&mut doc.views);
    let v = serde_json::to_value(&doc).map_err(|e| malformed("Drawing.ext[\"LAMB\"]", e))?;
    d.ext.insert(EXT_KEY.to_owned(), v);
    Ok(())
}

/// The entity's LAMB data; an absent slot is the default.
pub fn read_entity_data(c: &Common) -> Result<EntityData, SchemaError> {
    read_slot(&c.ext, || "an entity's Common.ext[\"LAMB\"]".to_owned())
}

/// Stores `data`, or removes the slot when it is all-default.
pub fn set_entity_data(c: &mut Common, data: &EntityData) -> Result<(), SchemaError> {
    write_slot(&mut c.ext, data, "Common.ext[\"LAMB\"]")
}

/// The layer's LAMB data; an absent slot is the default.
pub fn read_layer_data(l: &Layer) -> Result<LayerData, SchemaError> {
    read_slot(&l.ext, || format!("layer {:?}'s ext[\"LAMB\"]", l.name))
}

/// Stores `data`, or removes the slot when it is all-default.
pub fn set_layer_data(l: &mut Layer, data: &LayerData) -> Result<(), SchemaError> {
    write_slot(&mut l.ext, data, "Layer.ext[\"LAMB\"]")
}

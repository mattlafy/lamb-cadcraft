//! Where LAMB's own data lives inside a CADCraft `Drawing` (`docs/07-architecture/
//! cadcraft-integration.md` §3.1, §4 in the LAMB repo): three extension slots, all keyed
//! [`EXT_KEY`], never a side table.
//!
//! - `Drawing.ext["LAMB"]` = [`LambDocument`]: name, defaults, levels, views (metadata only),
//!   links, suppressed proposals.
//! - `Common.ext["LAMB"]` = [`EntityData`]: an entity's line role, AI provenance and virtual
//!   (fillet/chamfer) corner tags, on a sheet's viewport the view it shows, and on a plan's POINT
//!   whether it is an outside pick point or the plan's stud grid origin.
//! - `Layer.ext["LAMB"]` = [`LayerData`]: a layer's default role.
//!
//! A view's entities live in the block [`view_block_name`]`(id)`; an entity's id is its handle.
//! Writers remove a slot whose value is all-default, so a plain CADCraft drawing's DXF output is
//! unchanged.

use std::fmt;

use cadcraft_doc::{Common, Drawing, Extension, Layer};
use lamb_core::geometry::polyline::CornerEntry;
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

/// The block holding plan `id`'s generated assembly lines: `LAMB_ASSEMBLY_<id>`, id in decimal.
/// Not a view: it has no entry in [`LambDocument`]`.views`.
pub fn assembly_block_name(id: Id) -> String {
    format!("LAMB_ASSEMBLY_{}", id.0)
}

/// The block holding plan `id`'s generated annotation (section cut symbols, [`crate::annotations`]):
/// `LAMB_ANNO_<id>`, id in decimal. Not a view: it has no entry in [`LambDocument`]`.views`.
pub fn annotation_block_name(id: Id) -> String {
    format!("LAMB_ANNO_{}", id.0)
}

/// The block holding vertical view `id`'s hole content (the line work its holes reveal,
/// [`crate::holes`]): `LAMB_HOLE_<id>`, id in decimal. Not a view.
pub fn hole_block_name(id: Id) -> String {
    format!("LAMB_HOLE_{}", id.0)
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
    /// Plan views whose assembly mitres band ends at undetailed two-arm junctions
    /// ([`crate::assembly`], `assemblyplan {mitre}`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mitre_plans: Vec<Id>,
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
            mitre_plans: Vec::new(),
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

/// `Common.ext["LAMB"]`. Absent slot or field: `role` is `Bylayer`, no AI origin, no corners.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityData {
    #[serde(default = "bylayer")]
    pub role: LineRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_origin: Option<AiOrigin>,
    /// On a sheet's viewport: the LAMB view it shows ([`crate::sheets`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_id: Option<Id>,
    /// LAMB virtual corners (`PolySegment.corner_at_to`) as (vertex index, tag) pairs; the vertex
    /// indices are the entity's `LwPolyline` vertex indices (closed: the wrap corner is vertex 0).
    /// Re-applied by [`crate::convert::apply_corners`]; an index that is no longer a corner is
    /// skipped there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corners: Vec<CornerEntry>,
    /// On a `Leader` in a plan view's block: a detail tag (`cadcraft-integration.md` §9), its tip
    /// `vertices[0]` ([`crate::detailtags`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_tag: Option<DetailTag>,
    /// On a straight line in a detail view's block: that detail's reference line and its kind
    /// (`cadcraft-integration.md` §9, [`crate::detailref`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_ref: Option<DetailRefKind>,
    /// On a linear or aligned `Dimension` in a detail view's block: the geometry it repeats at its
    /// pitch (`cadcraft-integration.md` §9 "Repeats", [`crate::repeats`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<RepeatTag>,
    /// On a `Point` in a plan view's block: an outside pick point — the region around it is
    /// outside the building (`cadcraft-integration.md` §8 "Inside and outside", [`crate::outside`]).
    #[serde(default, skip_serializing_if = "is_false")]
    pub outside: bool,
    /// On a `Point` in a plan view's block: that plan's stud grid origin, holding the grid pitch
    /// in mm ([`crate::studgrid`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stud_grid: Option<f64>,
    /// With `stud_grid`: the point is wall-scoped — it overrules the project grid for the one plan
    /// run whose band holds it ([`crate::studgrid`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stud_grid_wall: Option<WallGrid>,
    /// On a `Point` in a plan or vertical view's block: a stud-move point — the nearest repeat copy
    /// of the run whose band holds it is laid centred on it ([`crate::studgrid`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stud_move: bool,
    /// On a closed straight-edged `LwPolyline` in a placed section or elevation's block: a hole
    /// showing the source view's line work through it ([`crate::holes`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hole: Option<HoleTag>,
}

/// A view hole's source: the parallel placed vertical view whose line work it reveals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoleTag {
    pub source: Id,
}

/// How a wall-scoped stud-grid point overrules the project grid for its run: `Phase` lays the run's
/// repeats from the point at its pitch, `Off` as if there were no grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WallGrid {
    Phase,
    Off,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A repeat's members: the entity ids (handles) the tagged dimension repeats.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepeatTag {
    pub members: Vec<Id>,
    /// The drawing's user parameter (Parameters Manager) that drives the pitch length (LAMB T503).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter: Option<String>,
}

/// What a detail view's reference line stands for, which the sweep aligns to a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetailRefKind {
    ExteriorFace,
    InteriorFace,
    StructuralCentre,
    Grid,
}

/// A detail tag's assignment: the detail view it names, and whether it tags a junction (a corner
/// detail) rather than a run (an edge detail).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailTag {
    pub detail_view_id: Id,
    #[serde(default)]
    pub corner: bool,
}

impl Default for EntityData {
    fn default() -> Self {
        EntityData {
            role: LineRole::Bylayer,
            ai_origin: None,
            view_id: None,
            corners: Vec::new(),
            detail_tag: None,
            detail_ref: None,
            repeat: None,
            outside: false,
            stud_grid: None,
            stud_grid_wall: None,
            stud_move: false,
            hole: None,
        }
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

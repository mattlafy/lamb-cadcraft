//! LAMB on CADCraft: the adapter layer between `lamb-core` and the CADCraft engine.
//!
//! Holds the entity <-> `Polyline2` adapter ([`convert`]: Line, Arc, Circle and LwPolyline
//! converted per segment), where LAMB's own data lives in a `Drawing` ([`schema`]: the three
//! `ext["LAMB"]` slots, one block per view) and lamb-core's `ProjectRead` over a `Drawing`
//! ([`project::DrawingProject`]), views created as block + layer + canonical insert ([`views`]) and
//! the view commands `newplan` / `newsectionline` / `newelevationline` / `editview` ([`commands`]),
//! a lamb-core `Project` imported as a `Drawing` ([`import`]), the after-command observer that
//! keeps links true ([`maintain`]: erased ends drop, broken crossings go stale, new lines auto-link),
//! the overlay and snap providers drawing an edited view's markers and cut lines ([`overlays`]), and
//! sheets: a paper layout with one viewport per view, every other view's layer frozen there
//! ([`sheets`], command `newsheet`), and the explicit link commands `link` / `unlink`
//! ([`commands`]), and the relations between two panes' views — rotation, crosshair mapping,
//! camera-lock centre ([`panes`]), and FILLET/CHAMFER on two adjacent polyline segments writing a
//! virtual corner tag instead of baking an arc ([`fillet`]).
//! Will hold the remaining LAMB commands and `Interactive` prompts, the after-command observers that maintain cut lines, derived views and tag anchors, and
//! the remaining overlay display lists (datums, repeat ghosts). Sits above
//! `cadcraft-engine` and below the UI shells and MCP.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

pub mod commands;
pub mod convert;
pub mod fillet;
pub mod import;
pub mod maintain;
pub mod overlays;
pub mod panes;
pub mod project;
pub mod schema;
pub mod sheets;
pub mod views;

pub use commands::commands;
pub use convert::{Converted, Flattened, Unsupported, UnsupportedReason, from_polyline, to_polyline};
pub use import::{IdMap, ImportError, ImportReport, project_to_drawing};
pub use maintain::{MaintenanceReport, install, link_observer, maintain_links};
pub use overlays::{lamb_overlay, lamb_snaps, register_providers};
pub use project::DrawingProject;
pub use schema::{
    EXT_KEY, EntityData, LambDocument, LayerData, SchemaError, read_document, read_entity_data, set_entity_data, view_block_name, write_document,
};
pub use sheets::{SheetError, create_sheet};
pub use views::{ViewError, create_view};

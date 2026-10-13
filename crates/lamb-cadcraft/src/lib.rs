//! LAMB on CADCraft: the adapter layer between `lamb-core` and the CADCraft engine.
//!
//! Holds the entity <-> `Polyline2` adapter ([`convert`]: Line, Arc, Circle and LwPolyline
//! converted per segment), where LAMB's own data lives in a `Drawing` ([`schema`]: the three
//! `ext["LAMB"]` slots, one block per view) and lamb-core's `ProjectRead` over a `Drawing`
//! ([`project::DrawingProject`]), views created as block + layer + canonical insert ([`views`]) and
//! the view commands `newplan` / `newsectionline` / `newelevationline` / `editview` ([`commands`]),
//! a lamb-core `Project` imported as a `Drawing` ([`import`]), and the after-command observer that
//! keeps links true ([`maintain`]: erased ends drop, broken crossings go stale, new lines auto-link).
//! Will hold the remaining LAMB commands and `Interactive` prompts, the after-command observers that maintain cut lines, derived views and tag anchors, and
//! the overlay display lists (markers, cut lines, datums, repeat ghosts). Sits above
//! `cadcraft-engine` and below the UI shells and MCP.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

pub mod commands;
pub mod convert;
pub mod import;
pub mod maintain;
pub mod project;
pub mod schema;
pub mod views;

pub use commands::commands;
pub use convert::{Converted, Flattened, Unsupported, UnsupportedReason, from_polyline, to_polyline};
pub use import::{IdMap, ImportError, ImportReport, project_to_drawing};
pub use maintain::{MaintenanceReport, install, link_observer, maintain_links};
pub use project::DrawingProject;
pub use schema::{EXT_KEY, EntityData, LambDocument, LayerData, SchemaError, read_document, view_block_name, write_document};
pub use views::{ViewError, create_view};

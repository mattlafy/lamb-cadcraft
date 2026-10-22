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
//! virtual corner tag instead of baking an arc ([`fillet`]), drawn rounded on screen and in plots
//! while the stored vertices stay sharp ([`rounding`]), and kept on their vertex when a CADCraft
//! command rewrites the vertices ([`corners`]), and the derived 3D scene as renderer-agnostic meshes
//! that pick back to their source lines ([`scene`]), lamb-core's `ProjectWrite` over a `Drawing`
//! ([`project_mut`]) and the after-command AI-line / solid-cut sweeps written through it ([`sweeps`]), and
//! each plan's assembly lines, written by ASSEMBLYPLAN into a block that an after-command observer
//! keeps regenerated ([`assembly`]), each detail tag's bubble — a `LAMB_DTAG` insert naming its detail — kept by an after-command observer ([`bubbles`]), each placed plan's plotted section cut symbols and each placed vertical view's level lines in a `LAMB_ANNO` block kept by an after-command observer ([`annotations`]), and ACCEPT /
//! SETROLE / LAYERROLE ([`entitycmds`]), the read-only SHOWLINKS / QUERY ([`querycmds`]), and
//! DUPLICATEVIEW / PLACEVIEW / UNPLACEVIEW / LEVEL ([`viewlife`]), and CUTLINE ([`cutlinecmd`]), and the Properties palette's LAMB group — Role, cut/seen height source per segment ([`props`]), and clicking a cut line in a plan picks that view ([`viewpick`]).
//! Will hold the remaining LAMB commands and `Interactive` prompts, the after-command observers that maintain cut lines, derived views and tag anchors, and
//! the remaining overlay display lists (datums, repeat ghosts). Sits above
//! `cadcraft-engine` and below the UI shells and MCP.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

pub mod annotations;
pub mod asmview;
pub mod assembly;
pub mod assistant;
pub mod assistant_cli;
pub mod assistant_lint;
pub mod bubbles;
pub mod cli;
pub mod commands;
pub mod convert;
pub mod corners;
mod cutlinecmd;
pub mod detailref;
pub mod detailtags;
mod dxfimport;
mod entitycmds;
pub mod export;
pub mod fillet;
pub mod fillet_matrix;
pub mod import;
mod lambopen;
mod layered;
mod linkcmds;
pub mod maintain;
mod newviews;
pub mod oneclick_cli;
mod outside;
pub mod overlays;
pub mod panes;
pub mod project;
pub mod project_mut;
pub mod props;
pub mod provider;
mod querycmds;
mod refpane;
mod relayer;
pub mod repeats;
mod resolvecmd;
mod revertex;
pub mod rounding;
pub mod scene;
pub mod schema;
pub mod script_cli;
pub mod session_cli;
pub mod sheets;
pub mod solids_cli;
mod studgrid;
pub mod sweeps;
mod viewlife;
pub mod viewpick;
mod viewprops;
pub mod views;
pub mod wallfit;
mod wallrun;

pub use annotations::annotation_observer;
pub use assembly::assembly_observer;
pub use bubbles::bubble_observer;
pub use commands::commands;
pub use convert::{Converted, Flattened, Unsupported, UnsupportedReason, from_polyline, to_polyline};
pub use corners::{corner_observer, maintain_corners};
pub use entitycmds::{LayerRoleCell, layer_role_cell};
pub use export::{ExportReport, drawing_to_project};
pub use import::{IdMap, ImportError, ImportReport, project_to_drawing};
pub use maintain::{MaintenanceReport, install, link_observer, maintain_links, travel_box_observer};
pub use overlays::{lamb_overlay, lamb_snaps, register_providers};
pub use project::DrawingProject;
pub use project_mut::DrawingProjectMut;
pub use scene::{Scene, SceneError, SceneMesh, SceneMeshKind, ScenePath, ScenePick, pick, scene};
pub use schema::{
    EXT_KEY, EntityData, LambDocument, LayerData, SchemaError, read_document, read_entity_data, set_entity_data, view_block_name, write_document,
};
pub use sheets::{SheetError, create_sheet};
pub use sweeps::{run_sweeps, sweep_observer};
pub use views::{ViewError, create_view};

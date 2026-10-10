//! LAMB on CADCraft: the adapter layer between `lamb-core` and the CADCraft engine.
//!
//! Will hold the entity <-> `Polyline2` adapter (Line, Arc, Circle and LwPolyline converted
//! per segment), the LAMB commands registered as engine `CommandSpec`s / `Interactive` prompts,
//! the after-command observers that maintain links, cut lines, derived views and tag anchors, and
//! the overlay display lists (markers, cut lines, datums, repeat ghosts). Sits above
//! `cadcraft-engine` and below the UI shells and MCP.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

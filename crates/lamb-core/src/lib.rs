//! LAMB core: the CAD-independent half of LAMB.
//!
//! Will hold LAMB's own geometry (`Polyline2` with bulge arcs, virtual corners, segment anchors),
//! the document model (views, levels, links, roles, provenance), the derivation engine, the wall
//! graph, details and tags, and sweeps / wall fitting. It sees only polylines with bulge arcs and
//! reads the drawing through traits, so it has no CADCraft dependency at all; the adapter lives in
//! `lamb-cadcraft`.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

pub mod geometry;

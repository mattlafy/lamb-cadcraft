//! LAMB's derivation engine, ported from `packages/derivation-engine/src/` in LAMB. It reads the
//! drawing only through [`read::ProjectRead`], so CADCraft's adapter can implement the same trait
//! over its own store. Design: LAMB's `docs/07-architecture/lamb-core-derivation-port.md`.

pub mod alignment;
pub mod projection;
pub mod read;

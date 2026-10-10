//! LAMB's geometry primitives, ported from `packages/geometry-primitives/src/` in LAMB.
//! Module layout mirrors the TypeScript; function names are the snake_case of the TS exports.

pub mod point;
pub mod tolerance;
pub mod transform2d;

pub use point::{Point2, Point3};
pub use transform2d::{IDENTITY_TRANSFORM, Transform2D};

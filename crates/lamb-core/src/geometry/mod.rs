//! LAMB's geometry primitives, ported from `packages/geometry-primitives/src/` in LAMB.
//! Module layout mirrors the TypeScript; function names are the snake_case of the TS exports.

pub mod circle_fit;
pub mod cursor_snaps;
pub mod dimension;
pub mod distance;
pub mod fillet;
pub mod halfspace;
pub mod hatch_pattern;
pub mod intersect;
pub mod loop_chain;
pub mod offset;
pub mod point;
pub mod polyline;
pub mod tangent_circle;
pub mod tolerance;
pub mod transform2d;
pub mod transform_ops;
pub mod triangulate;

pub use point::{Point2, Point3};
pub use polyline::{PolySegment, Polyline2, ResolvedSegment, VirtualCornerEntry};
pub use transform2d::{IDENTITY_TRANSFORM, Transform2D};

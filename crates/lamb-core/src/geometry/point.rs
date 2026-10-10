//! 2D and 3D points and vector arithmetic. Port of `geometry-primitives/src/point.ts`.

use serde::{Deserialize, Serialize};

/// mm, in the owning view's local 2D space.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Point2 {
    pub x: f64,
    pub y: f64,
}

/// mm, project-global.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Point3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

pub fn add2(a: Point2, b: Point2) -> Point2 {
    Point2 { x: a.x + b.x, y: a.y + b.y }
}

pub fn sub2(a: Point2, b: Point2) -> Point2 {
    Point2 { x: a.x - b.x, y: a.y - b.y }
}

pub fn scale2(a: Point2, s: f64) -> Point2 {
    Point2 { x: a.x * s, y: a.y * s }
}

pub fn lerp2(a: Point2, b: Point2, t: f64) -> Point2 {
    Point2 { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t }
}

pub fn dist2(a: Point2, b: Point2) -> f64 {
    (b.x - a.x).hypot(b.y - a.y)
}

pub fn dot2(a: Point2, b: Point2) -> f64 {
    a.x * b.x + a.y * b.y
}

pub fn cross2(a: Point2, b: Point2) -> f64 {
    a.x * b.y - a.y * b.x
}

pub fn normalize2(a: Point2) -> Point2 {
    let len = a.x.hypot(a.y);
    if len == 0.0 {
        return Point2 { x: 0.0, y: 0.0 };
    }
    Point2 { x: a.x / len, y: a.y / len }
}

pub fn angle_of2(a: Point2) -> f64 {
    a.y.atan2(a.x)
}

pub fn point3(x: f64, y: f64, z: f64) -> Point3 {
    Point3 { x, y, z }
}

pub fn add3(a: Point3, b: Point3) -> Point3 {
    Point3 { x: a.x + b.x, y: a.y + b.y, z: a.z + b.z }
}

pub fn sub3(a: Point3, b: Point3) -> Point3 {
    Point3 { x: a.x - b.x, y: a.y - b.y, z: a.z - b.z }
}

pub fn scale3(a: Point3, s: f64) -> Point3 {
    Point3 { x: a.x * s, y: a.y * s, z: a.z * s }
}

pub fn dot3(a: Point3, b: Point3) -> f64 {
    a.x * b.x + a.y * b.y + a.z * b.z
}

pub fn cross3(a: Point3, b: Point3) -> Point3 {
    Point3 { x: a.y * b.z - a.z * b.y, y: a.z * b.x - a.x * b.z, z: a.x * b.y - a.y * b.x }
}

/// `Math.hypot(x, y, z)`.
fn hypot3(x: f64, y: f64, z: f64) -> f64 {
    (x * x + y * y + z * z).sqrt()
}

pub fn normalize3(a: Point3) -> Point3 {
    let len = hypot3(a.x, a.y, a.z);
    if len == 0.0 {
        return Point3 { x: 0.0, y: 0.0, z: 0.0 };
    }
    Point3 { x: a.x / len, y: a.y / len, z: a.z / len }
}

pub fn dist3(a: Point3, b: Point3) -> f64 {
    hypot3(b.x - a.x, b.y - a.y, b.z - a.z)
}

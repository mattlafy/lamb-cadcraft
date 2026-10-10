//! Relates a view's local 2D drawing space to project-global space.
//! Port of `geometry-primitives/src/transform2d.ts`.

use serde::{Deserialize, Serialize};

use super::point::Point2;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transform2D {
    pub origin_x: f64,
    pub origin_y: f64,
    /// Radians, about the origin.
    pub rotation: f64,
}

pub const IDENTITY_TRANSFORM: Transform2D = Transform2D { origin_x: 0.0, origin_y: 0.0, rotation: 0.0 };

pub fn local_to_world2(t: &Transform2D, p: Point2) -> Point2 {
    let (sin, cos) = t.rotation.sin_cos();
    Point2 { x: t.origin_x + p.x * cos - p.y * sin, y: t.origin_y + p.x * sin + p.y * cos }
}

pub fn world_to_local2(t: &Transform2D, p: Point2) -> Point2 {
    let (sin, cos) = t.rotation.sin_cos();
    let dx = p.x - t.origin_x;
    let dy = p.y - t.origin_y;
    Point2 { x: dx * cos + dy * sin, y: -dx * sin + dy * cos }
}

/// Rotate a direction vector (no translation) from local to world space.
pub fn local_to_world_dir2(t: &Transform2D, d: Point2) -> Point2 {
    let (sin, cos) = t.rotation.sin_cos();
    Point2 { x: d.x * cos - d.y * sin, y: d.x * sin + d.y * cos }
}

/// Rotate a direction vector (no translation) from world to local space — the inverse of
/// [`local_to_world_dir2`].
pub fn world_to_local_dir2(t: &Transform2D, d: Point2) -> Point2 {
    let (sin, cos) = t.rotation.sin_cos();
    Point2 { x: d.x * cos + d.y * sin, y: -d.x * sin + d.y * cos }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The golden fixtures have no direct transform2d cases yet, so pin the round trip here.
    #[test]
    fn local_world_round_trip() {
        let t = Transform2D { origin_x: 100.0, origin_y: -50.0, rotation: 0.7 };
        let p = Point2 { x: 12.5, y: -3.0 };
        let back = world_to_local2(&t, local_to_world2(&t, p));
        assert!((back.x - p.x).abs() < 1e-9 && (back.y - p.y).abs() < 1e-9);
        let d = world_to_local_dir2(&t, local_to_world_dir2(&t, p));
        assert!((d.x - p.x).abs() < 1e-9 && (d.y - p.y).abs() < 1e-9);
        let q = local_to_world2(&t, Point2 { x: 1.0, y: 0.0 });
        assert!((q.x - (100.0 + 0.7f64.cos())).abs() < 1e-9 && (q.y - (-50.0 + 0.7f64.sin())).abs() < 1e-9);
        assert_eq!(local_to_world2(&IDENTITY_TRANSFORM, p), p);
    }
}

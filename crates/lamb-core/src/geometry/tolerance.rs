//! Project-wide tolerances. One epsilon per comparison kind, shared by drafting (osnaps) and
//! derivation (coincidence tests) so what snaps together is what the engine treats as coincident.
//! Port of `geometry-primitives/src/tolerance.ts`.

/// The shape of [`TOLERANCE`] (TypeScript's `TOLERANCE` object literal).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tolerances {
    /// Point/line coincidence (intersection detection), in mm.
    pub coincidence_mm: f64,
    /// Grouping links as "the same crossing point," in mm along the line's path.
    pub grouping_mm: f64,
    /// Height "agreement" for conflict vs. ambiguity detection, in mm.
    pub height_agreement_mm: f64,
    /// Angle comparison (tilt, alignment), in degrees.
    pub angle_deg: f64,
    /// Marker shape thresholds.
    pub marker_circle_min_deg: f64,
    pub marker_near_parallel_max_deg: f64,
}

pub const TOLERANCE: Tolerances = Tolerances {
    coincidence_mm: 0.5,
    grouping_mm: 50.0,
    height_agreement_mm: 2.0,
    angle_deg: 0.1,
    marker_circle_min_deg: 87.0,
    marker_near_parallel_max_deg: 10.0,
};

pub fn deg_to_rad(deg: f64) -> f64 {
    (deg * std::f64::consts::PI) / 180.0
}

pub fn rad_to_deg(rad: f64) -> f64 {
    (rad * 180.0) / std::f64::consts::PI
}

pub fn nearly_equal(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() <= eps
}

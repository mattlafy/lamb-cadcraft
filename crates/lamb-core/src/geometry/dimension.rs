//! Dimensions as document geometry, plus the resolver that turns one into draw-ready line work and
//! a text string. Port of `geometry-primitives/src/dimension.ts`.
//!
//! `resolve_dimension` emits sampled polylines only: an angular dimension's arc arrives already
//! sampled and arrowheads arrive as closed triangles. Everything is model space, millimetres;
//! `text_height_mm` is the MODEL-space text height.

use std::f64::consts::PI;

use serde::{Deserialize, Serialize};

use super::point::{Point2, add2, angle_of2, dist2, dot2, normalize2, scale2, sub2};

/// A linear dimension's measurement axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    X,
    Y,
}

/// TS `Dimension2`. `override_text` is the TS field `override`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "dim", rename_all = "lowercase")]
pub enum Dimension2 {
    Linear {
        from: Point2,
        to: Point2,
        axis: Axis,
        offset: f64,
        #[serde(rename = "override", default, skip_serializing_if = "Option::is_none")]
        override_text: Option<String>,
    },
    Aligned {
        from: Point2,
        to: Point2,
        offset: f64,
        #[serde(rename = "override", default, skip_serializing_if = "Option::is_none")]
        override_text: Option<String>,
    },
    /// `from` = centre, `to` = a point on the curve.
    Radius {
        from: Point2,
        to: Point2,
        #[serde(rename = "override", default, skip_serializing_if = "Option::is_none")]
        override_text: Option<String>,
    },
    /// `from`/`to` are points on the two rays; the angle is swept CCW from the `from` ray to the
    /// `to` ray.
    Angular {
        from: Point2,
        to: Point2,
        vertex: Point2,
        radius: f64,
        #[serde(rename = "override", default, skip_serializing_if = "Option::is_none")]
        override_text: Option<String>,
    },
}

/// One piece of resolved line work. `filled` = an arrowhead triangle (closed, solid).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DimLine {
    pub points: Vec<Point2>,
    pub filled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedDimension {
    pub lines: Vec<DimLine>,
    pub text: String,
    /// Baseline start, model space.
    pub text_anchor: Point2,
    /// Radians, never outside (-PI/2, PI/2].
    pub text_rotation: f64,
    /// mm, or degrees for angular: the raw measurement.
    pub value: f64,
}

/// Advance width of one character as a fraction of the text height.
const TEXT_WIDTH_FACTOR: f64 = 0.6;

/// Angular dimensions sample their arc at no coarser than this.
const ARC_SAMPLE_RAD: f64 = (5.0 * PI) / 180.0;

const TWO_PI: f64 = PI * 2.0;

/// Rotate +90 degrees: the "left" side of a direction.
fn left_normal(dir: Point2) -> Point2 {
    Point2 { x: -dir.y, y: dir.x }
}

fn unit_at(angle: f64) -> Point2 {
    Point2 { x: angle.cos(), y: angle.sin() }
}

/// JS `Math.sign` (0 for ±0, NaN for NaN).
fn js_sign(v: f64) -> f64 {
    if v > 0.0 {
        1.0
    } else if v < 0.0 {
        -1.0
    } else {
        v
    }
}

/// Tip at `tip`, base centred one text height back along `back`.
fn arrowhead(tip: Point2, back: Point2, text_height_mm: f64) -> DimLine {
    let dir = normalize2(back);
    let base = add2(tip, scale2(dir, text_height_mm));
    let half = scale2(left_normal(dir), text_height_mm / 3.0);
    DimLine { points: vec![tip, add2(base, half), sub2(base, half), tip], filled: true }
}

/// Text centred on `at`, reading along `along`, flipped so it is never upside-down.
fn place_text(text: &str, at: Point2, along: Point2, text_height_mm: f64) -> (Point2, f64) {
    let mut rot = angle_of2(along);
    rot = (((rot + PI) % TWO_PI) + TWO_PI) % TWO_PI - PI;
    if rot > PI / 2.0 || rot <= -PI / 2.0 {
        rot += PI;
    }
    if rot > PI {
        rot -= TWO_PI;
    }
    let dir = unit_at(rot);
    let up = left_normal(dir);
    // JS `string.length` counts UTF-16 code units.
    let width = text.encode_utf16().count() as f64 * text_height_mm * TEXT_WIDTH_FACTOR;
    let anchor = add2(add2(at, scale2(up, text_height_mm / 2.0)), scale2(dir, -width / 2.0));
    (anchor, rot)
}

/// JS `` `${Math.round(v)}` ``: round half up, `-0` prints `0`.
fn js_round_string(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let r = (v + 0.5).floor();
    if r.abs() < 9.0e15 { format!("{}", r as i64) } else { format!("{r}") }
}

/// JS `v.toFixed(1)`: exact ties round away from zero (Rust `{:.1}` rounds them to even).
fn to_fixed1(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v == 0.0 {
        return "0.0".into();
    }
    let a = v.abs();
    // An exact tie at one decimal is an odd multiple of 0.05 that a double can hold: x.25 / x.75.
    let tie = a < 1.0e15 && (a * 4.0).fract() == 0.0 && (a * 2.0).fract() != 0.0;
    let s = if tie { format!("{:.1}", ((a * 10.0) + 0.5).floor() / 10.0) } else { format!("{a:.1}") };
    if v < 0.0 { format!("-{s}") } else { s }
}

fn override_of(dim: &Dimension2) -> Option<&String> {
    match dim {
        Dimension2::Linear { override_text, .. }
        | Dimension2::Aligned { override_text, .. }
        | Dimension2::Radius { override_text, .. }
        | Dimension2::Angular { override_text, .. } => override_text.as_ref(),
    }
}

fn format_value(dim: &Dimension2, value: f64) -> String {
    if let Some(o) = override_of(dim) {
        return o.clone();
    }
    match dim {
        Dimension2::Radius { .. } => format!("R{}", js_round_string(value)),
        Dimension2::Angular { .. } => format!("{}°", to_fixed1(value)),
        _ => js_round_string(value),
    }
}

fn finish(dim: &Dimension2, lines: Vec<DimLine>, value: f64, at: Point2, along: Point2, text_height_mm: f64) -> ResolvedDimension {
    let text = format_value(dim, value);
    let (text_anchor, text_rotation) = place_text(&text, at, along, text_height_mm);
    ResolvedDimension { lines, text, text_anchor, text_rotation, value }
}

/// Linear and aligned: only the measurement direction differs.
fn resolve_offset_dimension(dim: &Dimension2, from: Point2, to: Point2, dir: Point2, offset: f64, text_height_mm: f64) -> ResolvedDimension {
    let span = sub2(to, from);
    let n = left_normal(dir);
    let value = dot2(span, dir).abs();

    let foot = |p: Point2| add2(add2(from, scale2(dir, dot2(sub2(p, from), dir))), scale2(n, offset));
    let foot_from = foot(from);
    let foot_to = foot(to);

    let mut lines = Vec::new();
    let overshoot = text_height_mm / 2.0;
    for p in [from, to] {
        let reach = offset - dot2(sub2(p, from), n);
        if reach.abs() < 1e-9 {
            continue;
        }
        let away = scale2(n, js_sign(reach));
        lines.push(DimLine { points: vec![p, add2(foot(p), scale2(away, overshoot))], filled: false });
    }

    lines.push(DimLine { points: vec![foot_from, foot_to], filled: false });

    let along = sub2(foot_to, foot_from);
    let has_length = dist2(foot_from, foot_to) > 1e-9;
    if has_length {
        lines.push(arrowhead(foot_from, along, text_height_mm));
        lines.push(arrowhead(foot_to, scale2(along, -1.0), text_height_mm));
    }

    let mid = scale2(add2(foot_from, foot_to), 0.5);
    finish(dim, lines, value, mid, if has_length { along } else { dir }, text_height_mm)
}

fn resolve_radius(dim: &Dimension2, from: Point2, to: Point2, text_height_mm: f64) -> ResolvedDimension {
    let value = dist2(from, to);
    let mut lines = vec![DimLine { points: vec![from, to], filled: false }];
    let along = sub2(to, from);
    if value > 1e-9 {
        lines.push(arrowhead(to, scale2(along, -1.0), text_height_mm));
    }
    let mid = scale2(add2(from, to), 0.5);
    finish(dim, lines, value, mid, if value > 1e-9 { along } else { Point2 { x: 1.0, y: 0.0 } }, text_height_mm)
}

fn resolve_angular(dim: &Dimension2, from: Point2, to: Point2, vertex: Point2, radius: f64, text_height_mm: f64) -> ResolvedDimension {
    let a1 = angle_of2(sub2(from, vertex));
    let a2 = angle_of2(sub2(to, vertex));
    let sweep = ((a2 - a1) % TWO_PI + TWO_PI) % TWO_PI;
    let value = (sweep * 180.0) / PI;

    let mut lines = Vec::new();
    let overshoot = text_height_mm / 2.0;
    for p in [from, to] {
        let here = dist2(vertex, p);
        if here < 1e-9 || here >= radius {
            continue;
        }
        lines.push(DimLine { points: vec![p, add2(vertex, scale2(normalize2(sub2(p, vertex)), radius + overshoot))], filled: false });
    }

    let steps = (sweep / ARC_SAMPLE_RAD).ceil().max(1.0) as usize;
    let arc: Vec<Point2> = (0..=steps).map(|i| add2(vertex, scale2(unit_at(a1 + (sweep * i as f64) / steps as f64), radius))).collect();
    lines.push(DimLine { points: arc, filled: false });

    let start = add2(vertex, scale2(unit_at(a1), radius));
    let end = add2(vertex, scale2(unit_at(a2), radius));
    if sweep > 1e-9 && radius > 1e-9 {
        lines.push(arrowhead(start, left_normal(unit_at(a1)), text_height_mm));
        lines.push(arrowhead(end, scale2(left_normal(unit_at(a2)), -1.0), text_height_mm));
    }

    let mid_angle = a1 + sweep / 2.0;
    let at = add2(vertex, scale2(unit_at(mid_angle), radius));
    finish(dim, lines, value, at, left_normal(unit_at(mid_angle)), text_height_mm)
}

pub fn resolve_dimension(dim: &Dimension2, text_height_mm: f64) -> ResolvedDimension {
    match *dim {
        Dimension2::Linear { from, to, axis, offset, .. } => {
            let dir = match axis {
                Axis::X => Point2 { x: 1.0, y: 0.0 },
                Axis::Y => Point2 { x: 0.0, y: 1.0 },
            };
            resolve_offset_dimension(dim, from, to, dir, offset, text_height_mm)
        }
        Dimension2::Aligned { from, to, offset, .. } => resolve_offset_dimension(dim, from, to, normalize2(sub2(to, from)), offset, text_height_mm),
        Dimension2::Radius { from, to, .. } => resolve_radius(dim, from, to, text_height_mm),
        Dimension2::Angular { from, to, vertex, radius, .. } => resolve_angular(dim, from, to, vertex, radius, text_height_mm),
    }
}

/// Maps every point a dimension owns. `offset` and `radius` are carried through unchanged:
/// correct for translate, rotate and mirror, a known limitation under SCALE.
pub fn transform_dimension(dim: &Dimension2, t: impl Fn(Point2) -> Point2) -> Dimension2 {
    let mut out = dim.clone();
    match &mut out {
        Dimension2::Linear { from, to, .. } | Dimension2::Aligned { from, to, .. } | Dimension2::Radius { from, to, .. } => {
            *from = t(*from);
            *to = t(*to);
        }
        Dimension2::Angular { from, to, vertex, .. } => {
            *from = t(*from);
            *to = t(*to);
            *vertex = t(*vertex);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_number_formatting() {
        assert_eq!(to_fixed1(0.25), "0.3");
        assert_eq!(to_fixed1(0.75), "0.8");
        assert_eq!(to_fixed1(90.0), "90.0");
        assert_eq!(to_fixed1(0.05), "0.1"); // 0.05 is just above the tie as a double
        assert_eq!(js_round_string(-0.4), "0");
        assert_eq!(js_round_string(2.5), "3");
        assert_eq!(js_round_string(-2.5), "-2");
    }
}

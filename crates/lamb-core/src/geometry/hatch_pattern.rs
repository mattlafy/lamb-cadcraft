//! Hatch pattern definitions in the `.pat` text format, the built-in set, and dash-cycle interval
//! clipping. Port of `geometry-primitives/src/hatchPattern.ts`.
//!
//! A `.pat` file is one or more `*NAME, description` headers, each followed by line-family rows
//! `angle, x-origin, y-origin, delta-along, delta-across [, dash...]`; `;` starts a comment.

use serde::{Deserialize, Serialize};

/// One family of parallel lines. `dash` empty = a solid continuous line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternFamily {
    pub angle_deg: f64,
    pub origin_x: f64,
    pub origin_y: f64,
    pub delta_along: f64,
    pub delta_across: f64,
    pub dash: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternDefinition {
    pub name: String,
    pub description: String,
    pub families: Vec<PatternFamily>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternParseError {
    /// 1-based line number.
    pub line: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternParseResult {
    pub patterns: Vec<PatternDefinition>,
    pub errors: Vec<PatternParseError>,
}

/// JS `Number(p)` for an already-trimmed, non-empty string; `None` where JS gives NaN.
/// Rust's parser also accepts `inf`/`infinity`/`nan` in any case, which JS does not.
fn js_number(p: &str) -> Option<f64> {
    let unsigned = p.strip_prefix(['+', '-']).unwrap_or(p);
    if unsigned.eq_ignore_ascii_case("inf") || unsigned.eq_ignore_ascii_case("infinity") {
        return (unsigned == "Infinity").then(|| if p.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY });
    }
    p.parse::<f64>().ok().filter(|v| !v.is_nan())
}

/// Parses `.pat` text. Malformed family lines are skipped and reported, never thrown.
pub fn parse_pat_file(text: &str) -> PatternParseResult {
    let mut patterns: Vec<PatternDefinition> = Vec::new();
    let mut errors = Vec::new();
    // Index into `patterns` of the definition family lines currently attach to.
    let mut current: Option<usize> = None;

    for (i, raw) in text.split('\n').enumerate() {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }

        if let Some(rest) = line.strip_prefix('*') {
            let (name, description) = match rest.find(',') {
                Some(c) => (rest[..c].trim(), rest[c + 1..].trim()),
                None => (rest.trim(), ""),
            };
            if name.is_empty() {
                errors.push(PatternParseError { line: i + 1, message: "Pattern name is empty".into() });
                current = None;
                continue;
            }
            patterns.push(PatternDefinition { name: name.into(), description: description.into(), families: Vec::new() });
            current = Some(patterns.len() - 1);
            continue;
        }

        let Some(cur) = current.and_then(|c| patterns.get_mut(c)) else {
            errors.push(PatternParseError { line: i + 1, message: format!("Family line before any \"*NAME\" header: \"{line}\"") });
            continue;
        };
        let parts: Vec<&str> = line.split(',').map(str::trim).collect();
        let nums: Option<Vec<f64>> = parts.iter().map(|p| if p.is_empty() { None } else { js_number(p) }).collect();
        let nums = match nums {
            Some(n) if n.len() >= 5 => n,
            _ => {
                errors.push(PatternParseError { line: i + 1, message: format!("Malformed family line: \"{line}\"") });
                continue;
            }
        };
        cur.families.push(PatternFamily {
            angle_deg: nums[0],
            origin_x: nums[1],
            origin_y: nums[2],
            delta_along: nums[3],
            delta_across: nums[4],
            dash: nums[5..].to_vec(),
        });
    }

    PatternParseResult { patterns, errors }
}

/// JS `String(n)` for a number.
fn js_number_string(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if v == 0.0 {
        return "0".into();
    }
    let a = v.abs();
    if (1e-6..1e21).contains(&a) {
        return format!("{v}");
    }
    // Exponent form: JS writes `1e+21` / `1.5e-7`.
    let s = format!("{v:e}");
    match s.split_once('e') {
        Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
        _ => s,
    }
}

/// Serializes pattern definitions back to `.pat` text: the inverse of [`parse_pat_file`].
pub fn stringify_pat_file(patterns: &[PatternDefinition]) -> String {
    let mut out: Vec<String> = Vec::new();
    for p in patterns {
        out.push(if p.description.is_empty() { format!("*{}", p.name) } else { format!("*{}, {}", p.name, p.description) });
        for f in &p.families {
            let nums: Vec<String> =
                [f.angle_deg, f.origin_x, f.origin_y, f.delta_along, f.delta_across].iter().chain(&f.dash).map(|&n| js_number_string(n)).collect();
            out.push(nums.join(", "));
        }
    }
    out.join("\n") + "\n"
}

fn solid(angle_deg: f64, delta_across: f64) -> PatternFamily {
    PatternFamily { angle_deg, origin_x: 0.0, origin_y: 0.0, delta_along: 0.0, delta_across, dash: Vec::new() }
}

/// TS `BUILTIN_PATTERNS`: always available regardless of any loaded `.pat` library.
pub fn builtin_patterns() -> Vec<PatternDefinition> {
    vec![
        PatternDefinition { name: "ANSI31".into(), description: "ANSI Iron, Brick, Stone masonry".into(), families: vec![solid(45.0, 3.175)] },
        PatternDefinition { name: "ANSI37".into(), description: "ANSI Cross-hatch".into(), families: vec![solid(45.0, 3.175), solid(135.0, 3.175)] },
        PatternDefinition {
            name: "BRICK".into(),
            description: "Staggered brick coursing".into(),
            families: vec![PatternFamily {
                angle_deg: 0.0,
                origin_x: 0.0,
                origin_y: 0.0,
                delta_along: 12.0,
                delta_across: 6.35,
                dash: vec![24.0, -6.0],
            }],
        },
    ]
}

pub fn find_builtin_pattern(name: &str) -> Option<PatternDefinition> {
    builtin_patterns().into_iter().find(|p| p.name == name)
}

/// The sub-intervals of `[range_start, range_end]` a repeating dash cycle draws, the cycle starting
/// at `phase` and extending both ways. Positive dash = draw, negative = skip; a ~0 cycle draws
/// nothing.
pub fn dash_intervals_in_range(dash: &[f64], phase: f64, range_start: f64, range_end: f64) -> Vec<(f64, f64)> {
    if range_end <= range_start || dash.is_empty() {
        return Vec::new();
    }
    let cycle_length: f64 = dash.iter().fold(0.0, |sum, d| sum + d.abs());
    if cycle_length < 1e-9 {
        return Vec::new();
    }

    let mut out = Vec::new();
    let cycles_back = ((phase - range_start) / cycle_length).ceil();
    let mut cursor = phase - cycles_back * cycle_length;
    let mut i = 0usize;
    while cursor < range_end {
        let seg = dash[i % dash.len()];
        let seg_len = seg.abs();
        if seg_len > 1e-9 {
            let seg_start = cursor;
            let seg_end = cursor + seg_len;
            if seg > 0.0 {
                let clipped_start = seg_start.max(range_start);
                let clipped_end = seg_end.min(range_end);
                if clipped_end > clipped_start {
                    out.push((clipped_start, clipped_end));
                }
            }
            cursor = seg_end;
        }
        i += 1;
        // Safety valve: never iterate more than a bounded number of dash entries for one range.
        if i > 100_000 {
            break;
        }
    }
    out
}

//! Replays LAMB's golden fixtures (`tests/golden/`, copied from LAMB's `tools/fixtures/golden/`)
//! against the Rust port. The format is specified in `tests/golden/README.md`; the reference
//! implementation is LAMB's `tools/golden/codec.ts`.
//!
//! Every case whose `"<module>#<export>"` is in [`dispatch`] must pass. Cases of exports not in the
//! table are counted, not failed, and a per-package progress line is printed.
//!
//! To port a function: implement it in `lamb_core`, add one line to [`dispatch`], run
//! `cargo test -p lamb-core --test golden`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write as _;
use std::path::Path;

use lamb_core::geometry::{Point2, Point3, Transform2D, point, tolerance, transform2d};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Calls one port with a case's decoded positional arguments and returns its encoded result.
/// `Err(message)` means the call threw.
type Handler = fn(&[Value]) -> Result<Value, String>;

/// The frozen clock every fixture was captured under. A port that stamps "now" must use this when
/// replaying (nothing ported reads the clock yet).
#[allow(dead_code)]
const GOLDEN_NOW: &str = "2026-01-01T00:00:00.000Z";

/// |a − b| ≤ TOLERANCE · max(1, |a|, |b|).
const TOLERANCE: f64 = 1e-9;

// ---------------------------------------------------------------------------------------------
// Dispatch table
// ---------------------------------------------------------------------------------------------

fn dispatch() -> HashMap<&'static str, Handler> {
    let table: &[(&str, Handler)] = &[
        // geometry-primitives/tolerance
        ("geometry-primitives/tolerance#degToRad", |a| Ok(num_out(tolerance::deg_to_rad(num(a, 0)?)))),
        ("geometry-primitives/tolerance#radToDeg", |a| Ok(num_out(tolerance::rad_to_deg(num(a, 0)?)))),
        ("geometry-primitives/tolerance#nearlyEqual", |a| Ok(Value::Bool(tolerance::nearly_equal(num(a, 0)?, num(a, 1)?, num(a, 2)?)))),
        // geometry-primitives/point
        ("geometry-primitives/point#add2", |a| out(point::add2(arg::<Point2>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/point#sub2", |a| out(point::sub2(arg::<Point2>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/point#scale2", |a| out(point::scale2(arg::<Point2>(a, 0)?, num(a, 1)?))),
        ("geometry-primitives/point#lerp2", |a| out(point::lerp2(arg::<Point2>(a, 0)?, arg(a, 1)?, num(a, 2)?))),
        ("geometry-primitives/point#dist2", |a| Ok(num_out(point::dist2(arg::<Point2>(a, 0)?, arg(a, 1)?)))),
        ("geometry-primitives/point#dot2", |a| Ok(num_out(point::dot2(arg::<Point2>(a, 0)?, arg(a, 1)?)))),
        ("geometry-primitives/point#cross2", |a| Ok(num_out(point::cross2(arg::<Point2>(a, 0)?, arg(a, 1)?)))),
        ("geometry-primitives/point#normalize2", |a| out(point::normalize2(arg::<Point2>(a, 0)?))),
        ("geometry-primitives/point#angleOf2", |a| Ok(num_out(point::angle_of2(arg::<Point2>(a, 0)?)))),
        ("geometry-primitives/point#point3", |a| out(point::point3(num(a, 0)?, num(a, 1)?, num(a, 2)?))),
        ("geometry-primitives/point#add3", |a| out(point::add3(arg::<Point3>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/point#sub3", |a| out(point::sub3(arg::<Point3>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/point#scale3", |a| out(point::scale3(arg::<Point3>(a, 0)?, num(a, 1)?))),
        ("geometry-primitives/point#dot3", |a| Ok(num_out(point::dot3(arg::<Point3>(a, 0)?, arg(a, 1)?)))),
        ("geometry-primitives/point#cross3", |a| out(point::cross3(arg::<Point3>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/point#normalize3", |a| out(point::normalize3(arg::<Point3>(a, 0)?))),
        ("geometry-primitives/point#dist3", |a| Ok(num_out(point::dist3(arg::<Point3>(a, 0)?, arg(a, 1)?)))),
        // geometry-primitives/transform2d
        ("geometry-primitives/transform2d#localToWorld2", |a| out(transform2d::local_to_world2(&arg::<Transform2D>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/transform2d#worldToLocal2", |a| out(transform2d::world_to_local2(&arg::<Transform2D>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/transform2d#localToWorldDir2", |a| out(transform2d::local_to_world_dir2(&arg::<Transform2D>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/transform2d#worldToLocalDir2", |a| out(transform2d::world_to_local_dir2(&arg::<Transform2D>(a, 0)?, arg(a, 1)?))),
    ];
    table.iter().copied().collect()
}

// ---------------------------------------------------------------------------------------------
// Argument / result helpers for handlers
// ---------------------------------------------------------------------------------------------

/// Positional argument `i`, deserialized. A missing trailing argument is `null` (JS `undefined`),
/// so `Option<T>` parameters work.
fn arg<T: DeserializeOwned>(args: &[Value], i: usize) -> Result<T, String> {
    let v = args.get(i).cloned().unwrap_or(Value::Null);
    serde_json::from_value(v).map_err(|e| format!("arg {i}: {e}"))
}

/// A numeric argument, accepting the `{"$number": "NaN" | "Infinity" | "-Infinity"}` tag.
fn num(args: &[Value], i: usize) -> Result<f64, String> {
    match args.get(i) {
        Some(Value::Number(n)) => n.as_f64().ok_or_else(|| format!("arg {i}: bad number")),
        Some(Value::Object(o)) => match o.get("$number").and_then(Value::as_str) {
            Some("NaN") => Ok(f64::NAN),
            Some("Infinity") => Ok(f64::INFINITY),
            Some("-Infinity") => Ok(f64::NEG_INFINITY),
            _ => Err(format!("arg {i}: not a number: {}", Value::Object(o.clone()))),
        },
        other => Err(format!("arg {i}: not a number: {other:?}")),
    }
}

/// Encode a number result, tagging non-finite values as the format requires.
fn num_out(x: f64) -> Value {
    if x.is_nan() {
        tagged_number("NaN")
    } else if x == f64::INFINITY {
        tagged_number("Infinity")
    } else if x == f64::NEG_INFINITY {
        tagged_number("-Infinity")
    } else {
        serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number)
    }
}

fn tagged_number(s: &str) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("$number".into(), Value::String(s.into()));
    Value::Object(m)
}

/// Encode a result through serde. NOTE: serde_json writes a non-finite f64 inside a struct as
/// `null`, so a struct result holding NaN/Infinity will show as a mismatch (never a false pass).
fn out<T: Serialize>(v: T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| format!("encode result: {e}"))
}

// ---------------------------------------------------------------------------------------------
// Codec: decode, minted-id canonicalisation, comparison (mirrors tools/golden/codec.ts)
// ---------------------------------------------------------------------------------------------

/// Fixture JSON → the plain JSON handed to handlers: `$undefined` → `null`, `$map` → array of
/// `[key, value]` pairs, `$set` → array. `$number` stays tagged (serde_json can't hold NaN);
/// [`num`] reads it.
fn decode(v: &Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.iter().map(decode).collect()),
        Value::Object(o) => {
            if o.get("$undefined") == Some(&Value::Bool(true)) {
                return Value::Null;
            }
            if o.get("$number").is_some_and(Value::is_string) {
                return v.clone();
            }
            if let Some(Value::Array(pairs)) = o.get("$map") {
                return Value::Array(pairs.iter().map(decode).collect());
            }
            if let Some(Value::Array(items)) = o.get("$set") {
                return Value::Array(items.iter().map(decode).collect());
            }
            Value::Object(o.iter().map(|(k, v)| (k.clone(), decode(v))).collect())
        }
        _ => v.clone(),
    }
}

fn collect_strings<'a>(v: &'a Value, into: &mut HashSet<&'a str>) {
    match v {
        Value::String(s) => {
            into.insert(s);
        }
        Value::Array(a) => a.iter().for_each(|x| collect_strings(x, into)),
        Value::Object(o) => o.values().for_each(|x| collect_strings(x, into)),
        _ => {}
    }
}

/// `generateId`'s shape: `^[a-z][a-z0-9-]*-[0-9a-z]{8,}-[0-9a-z]+$`.
fn is_minted_id(s: &str) -> bool {
    let alnum = |p: &str| p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    let mut parts = s.rsplitn(3, '-');
    let (Some(last), Some(mid), Some(prefix)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !last.is_empty()
        && alnum(last)
        && mid.len() >= 8
        && alnum(mid)
        && prefix.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && prefix.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Replace minted ids (id-shaped strings absent from the args) by `$id:N` in order of first
/// appearance, walking `values` in order with object keys sorted.
fn canonicalize(values: &[Value], given: &HashSet<&str>) -> Vec<Value> {
    fn walk(v: &Value, given: &HashSet<&str>, ids: &mut HashMap<String, String>) -> Value {
        match v {
            Value::String(s) => {
                if given.contains(s.as_str()) || !is_minted_id(s) {
                    return v.clone();
                }
                let n = ids.len();
                Value::String(ids.entry(s.clone()).or_insert_with(|| format!("$id:{n}")).clone())
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, given, ids)).collect()),
            Value::Object(o) => {
                let sorted: BTreeMap<&String, &Value> = o.iter().collect();
                Value::Object(sorted.into_iter().map(|(k, x)| (k.clone(), walk(x, given, ids))).collect())
            }
            _ => v.clone(),
        }
    }
    let mut ids = HashMap::new();
    values.iter().map(|v| walk(v, given, &mut ids)).collect()
}

/// Differences between expected and actual, as `path: expected vs actual` lines.
fn compare(expected: &Value, actual: &Value, path: &str, out: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Number(e), Value::Number(a)) => {
            let (e, a) = (e.as_f64().unwrap_or(f64::NAN), a.as_f64().unwrap_or(f64::NAN));
            // Written so NaN on either side is a difference.
            let within = (e - a).abs() <= TOLERANCE * 1f64.max(e.abs()).max(a.abs());
            if !within {
                out.push(format!("{path}: {e} vs {a}"));
            }
        }
        (Value::Array(e), Value::Array(a)) => {
            if e.len() != a.len() {
                out.push(format!("{path}: length {} vs {}", e.len(), a.len()));
            } else {
                for (i, (x, y)) in e.iter().zip(a).enumerate() {
                    compare(x, y, &format!("{path}[{i}]"), out);
                }
            }
        }
        (Value::Object(e), Value::Object(a)) => {
            let keys: BTreeSet<&String> = e.keys().chain(a.keys()).collect();
            for k in keys {
                compare(e.get(k).unwrap_or(&Value::Null), a.get(k).unwrap_or(&Value::Null), &format!("{path}.{k}"), out);
            }
        }
        _ => {
            if expected != actual {
                out.push(format!("{path}: {} vs {}", short(expected), short(actual)));
            }
        }
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 80 { format!("{}...", s.chars().take(77).collect::<String>()) } else { s }
}

// ---------------------------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------------------------

/// Replays one case through its handler. `Ok(())` = pass, `Err(lines)` = what differed.
fn replay(case: &Value, handler: Handler) -> Result<(), Vec<String>> {
    let raw_args = case.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
    let args: Vec<Value> = raw_args.iter().map(decode).collect();
    if case.get("argsAfter").is_some() {
        // Handlers take `&[Value]`, so a ported export that mutates its arguments can't be checked
        // yet. Extend `Handler` to return the arguments afterwards when the first such port lands.
        return Err(vec!["case records argsAfter (mutating call): runner has no argsAfter support yet".into()]);
    }
    let outcome = handler(&args);
    match (case.get("throws"), outcome) {
        (Some(_), Err(_)) => Ok(()),
        (Some(msg), Ok(v)) => Err(vec![format!("expected throw {msg}, got {}", short(&v))]),
        (None, Err(e)) => Err(vec![format!("threw: {e}")]),
        (None, Ok(v)) => {
            let given = {
                let mut s = HashSet::new();
                raw_args.iter().for_each(|a| collect_strings(a, &mut s));
                s
            };
            let canon = canonicalize(&[v], &given);
            let expected = case.get("result").unwrap_or(&Value::Null);
            let mut diffs = Vec::new();
            compare(expected, &canon[0], "$", &mut diffs);
            if diffs.is_empty() { Ok(()) } else { Err(diffs) }
        }
    }
}

#[derive(Default)]
struct ExportStats {
    cases: usize,
    passed: usize,
}

#[test]
fn golden_fixtures() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden");
    let table = dispatch();
    // "<module>#<export>" → stats, across every fixture file (a module's cases can come from other
    // packages' test files).
    let mut stats: BTreeMap<String, ExportStats> = BTreeMap::new();
    let mut failures: Vec<String> = Vec::new();

    let mut dirs: Vec<_> = std::fs::read_dir(&root).expect("read tests/golden").filter_map(Result::ok).map(|e| e.path()).collect();
    dirs.sort();
    for dir in dirs.iter().filter(|d| d.is_dir()) {
        let mut files: Vec<_> = std::fs::read_dir(dir).expect("read package dir").filter_map(Result::ok).map(|e| e.path()).collect();
        files.sort();
        for file in files.iter().filter(|f| f.extension().is_some_and(|x| x == "json")) {
            let text = std::fs::read_to_string(file).expect("read fixture");
            let doc: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            assert_eq!(doc.get("format").and_then(Value::as_str), Some("lamb-golden/1"), "{}", file.display());
            for case in doc.get("cases").and_then(Value::as_array).into_iter().flatten() {
                let module = case.get("module").and_then(Value::as_str).unwrap_or("?");
                let export = case.get("export").and_then(Value::as_str).unwrap_or("?");
                let key = format!("{module}#{export}");
                let entry = stats.entry(key.clone()).or_default();
                entry.cases += 1;
                let Some(handler) = table.get(key.as_str()) else { continue };
                match replay(case, *handler) {
                    Ok(()) => entry.passed += 1,
                    Err(diffs) => {
                        let test = case.get("test").and_then(Value::as_str).unwrap_or("?");
                        let rel = file.strip_prefix(&root).unwrap_or(file).display().to_string();
                        failures.push(format!("FAIL {key} [{rel}] {test}\n    {}", diffs.into_iter().take(8).collect::<Vec<_>>().join("\n    ")));
                    }
                }
            }
        }
    }

    // Report. Written straight to the stderr handle so libtest's capture doesn't hide it.
    let mut report = String::new();
    let mut packages: BTreeMap<&str, (usize, usize, usize, usize)> = BTreeMap::new();
    for (key, s) in &stats {
        let package = key.split('/').next().unwrap_or("?");
        let p = packages.entry(package).or_default();
        p.1 += 1;
        p.3 += s.cases;
        if table.contains_key(key.as_str()) {
            p.0 += 1;
            p.2 += s.passed;
            let mark = if s.passed == s.cases { "ok  " } else { "FAIL" };
            report.push_str(&format!("  {mark} {key}: {}/{} cases pass\n", s.passed, s.cases));
        }
    }
    for (package, (ported, exports, passed, cases)) in &packages {
        report.push_str(&format!("{package}: ported {ported}/{exports} exports, {passed}/{cases} cases pass\n"));
    }
    let unfixtured: Vec<&str> = table.keys().copied().filter(|k| !stats.contains_key(*k)).collect();
    if !unfixtured.is_empty() {
        let mut u = unfixtured;
        u.sort_unstable();
        report.push_str(&format!("ported with no fixture cases: {}\n", u.join(", ")));
    }
    for f in &failures {
        report.push_str(f);
        report.push('\n');
    }
    let _ = std::io::stderr().write_all(format!("\ngolden fixtures:\n{report}").as_bytes());

    assert!(failures.is_empty(), "{} golden case(s) failed for ported exports (see report above)", failures.len());
}

// ---------------------------------------------------------------------------------------------
// Codec self-checks
// ---------------------------------------------------------------------------------------------

#[test]
fn minted_id_shape() {
    assert!(is_minted_id("view-lx3k9a2b-1"));
    assert!(is_minted_id("ref-line-lx3k9a2b-1a"));
    assert!(!is_minted_id("view-short-1"));
    assert!(!is_minted_id("1view-lx3k9a2b-1"));
    assert!(!is_minted_id("lx3k9a2b-1"));
    assert!(!is_minted_id("view-lx3k9a2b-"));
}

#[test]
fn canonicalize_numbers_minted_ids_by_first_appearance() {
    let given: HashSet<&str> = ["keep-abcdefgh-1"].into_iter().collect();
    let v: Value = serde_json::json!({"b": "new-abcdefgh-2", "a": ["keep-abcdefgh-1", "new-zzzzzzzz-3", "new-abcdefgh-2"]});
    let c = canonicalize(&[v], &given);
    assert_eq!(c[0], serde_json::json!({"a": ["keep-abcdefgh-1", "$id:0", "$id:1"], "b": "$id:1"}));
}

#[test]
fn compare_tolerance_and_absent_key() {
    let mut d = Vec::new();
    compare(&serde_json::json!({"x": 1.0, "y": null}), &serde_json::json!({"x": 1.0 + 5e-10}), "$", &mut d);
    assert!(d.is_empty(), "{d:?}");
    compare(&serde_json::json!(1.0), &serde_json::json!(1.000_001), "$", &mut d);
    assert_eq!(d.len(), 1);
}

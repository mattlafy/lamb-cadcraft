//! What one click in a section produces, as JSON (TS `tools/one-click-views.ts`): the plan a
//! "Plan Here" click at `--point` creates, and the section square to the clicked view through it.
//!
//! ```text
//! cargo run -p lamb-cadcraft --example lamb_oneclick -- [FILE] --from <view name> --point <u>,<z> [--length <mm>] [--cut-height <mm>] < records
//! ```
//!
//! - FILE and the stdin records are exactly `lamb_cli`'s (`crate::cli`): `.dxf` or `.lamb.json`,
//!   none → a fresh LAMB project; one `{"command", "params"}` / `"!undo"` / `"!redo"` per line.
//!   A failed record prints `FAILED <command>: <error>` on stderr and the report still prints.
//! - `--from NAME`: a placed section or elevation, by exact view name. `--point u,z`: the click in
//!   that view's own local 2D. `--length` (default 10000): the perpendicular cut line's length.
//!   `--cut-height`: the new plan's cut height above its floor (default: the project's).
//! - stdout: one JSON object — `sourceBasis`, `planPlacementThrough`,
//!   `perpendicularCutLineThrough` (computed before anything is created), then `newPlan`,
//!   `newLevel`, `newSection` (placements and level as created by NEWPLANLINE and
//!   NEWSECTIONLINE), `newSectionBasis` (the new section's basis against its host) and `results`
//!   (`"ok"` or the error, one per command). A failed command leaves its keys `null`.
//!
//! Exit 0 ok, 1 any failure (one line on stderr).

use std::io::Write;

use lamb_core::derivation::projection::{perpendicular_cut_line_through, plan_placement_through, vertical_basis};
use lamb_core::derivation::read::{ProjectRead, ViewKind};
use lamb_core::geometry::Point2;
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::cli::{Record, apply, baseline, parse_records};
use crate::commands::id_value;
use crate::project::DrawingProject;
use crate::schema::read_document;

const USAGE: &str = "usage: lamb_oneclick [FILE] --from <view name> --point <u>,<z> [--length <mm>] [--cut-height <mm>] < records";

struct Opts {
    file: Option<String>,
    from: String,
    point: Point2,
    length: f64,
    cut_height: Option<f64>,
}

fn number(flag: &str, v: &str) -> Result<f64, String> {
    v.trim().parse::<f64>().ok().filter(|n| n.is_finite()).ok_or_else(|| format!("{flag} needs a number, got {v}\n{USAGE}"))
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let (mut file, mut from, mut point, mut length, mut cut_height) = (None, None, None, 10000.0, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value\n{USAGE}"));
        match a.as_str() {
            "--from" => from = Some(value("--from")?),
            "--point" => {
                let v = value("--point")?;
                let (u, z) = v.split_once(',').ok_or_else(|| format!("--point needs <u>,<z>, got {v}\n{USAGE}"))?;
                point = Some(Point2 { x: number("--point", u)?, y: number("--point", z)? });
            }
            "--length" => length = number("--length", &value("--length")?)?,
            "--cut-height" => cut_height = Some(number("--cut-height", &value("--cut-height")?)?),
            f if f.starts_with("--") => return Err(format!("unknown flag {f}\n{USAGE}")),
            p if file.is_none() => file = Some(p.to_owned()),
            p => return Err(format!("unexpected argument {p}\n{USAGE}")),
        }
    }
    Ok(Opts {
        file,
        from: from.ok_or_else(|| format!("--from is required\n{USAGE}"))?,
        point: point.ok_or_else(|| format!("--point is required\n{USAGE}"))?,
        length,
        cut_height,
    })
}

fn to_json<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// Runs the command line `args` (without the program name) over the records in `stdin`; returns
/// the exit code.
pub fn run(args: &[String], stdin: &str, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    match run_inner(args, stdin, out, err) {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(err, "{e}");
            1
        }
    }
}

fn run_inner(args: &[String], stdin: &str, out: &mut dyn Write, err: &mut dyn Write) -> Result<i32, String> {
    let o = parse_args(args)?;
    let records = parse_records(stdin)?;
    let mut s = baseline(o.file.as_deref())?;
    let mut code = 0;
    for (rec, res) in records.iter().zip(apply(&mut s, &records)) {
        if res["ok"] != Value::Bool(true) {
            let name = match rec {
                Record::Command { id, .. } => id.as_str(),
                Record::Undo => "!undo",
                Record::Redo => "!redo",
            };
            let why = res["error"].as_str().unwrap_or("failed");
            writeln!(err, "FAILED {name}: {why}").map_err(|e| e.to_string())?;
            code = 1;
        }
    }

    // What the click means, read before anything is created.
    let (view_id, host_id, source_basis, through, cut) = {
        let proj = DrawingProject::new(s.doc().map_err(|e| e.to_string())?).map_err(|e| format!("project: {e:?}"))?;
        let not_vertical = || format!("--from {} is not a placed vertical view", o.from);
        let view = proj
            .views()
            .find(|v| v.name == o.from.as_str())
            .filter(|v| v.kind == ViewKind::Vertical && v.placement.is_some())
            .ok_or_else(not_vertical)?;
        let host = view.host_view_id().and_then(|h| proj.view(h)).ok_or_else(|| format!("view {} has no host view", o.from))?;
        let basis = vertical_basis(&view, &host)?;
        let through = plan_placement_through(&view, &host, o.point)?;
        let cut = perpendicular_cut_line_through(&view, &host, o.point, o.length)?;
        (view.id, host.id, basis, through, cut)
    };

    let mut plan_params = json!({
        "fromVerticalViewId": view_id.0,
        "localPoint": { "x": o.point.x, "y": o.point.y },
        "name": "From click",
    });
    if let Some(h) = o.cut_height {
        plan_params["cutHeightMm"] = json!(h);
    }
    let plan_res = s.execute("newplanline", &plan_params).map_err(|e| e.to_string());
    let section_params = json!({
        "name": "Perpendicular",
        "hostViewId": host_id.0,
        "cutFrom": { "x": cut.cut_from.x, "y": cut.cut_from.y },
        "cutTo": { "x": cut.cut_to.x, "y": cut.cut_to.y },
    });
    let section_res = s.execute("newsectionline", &section_params).map_err(|e| e.to_string());

    let created = |r: &Result<Value, String>, key: &str| -> Option<Id> { r.as_ref().ok().and_then(|v| v.get(key)).and_then(id_value) };
    let (plan_id, level_id, section_id) = (created(&plan_res, "viewId"), created(&plan_res, "levelId"), created(&section_res, "viewId"));

    let d = s.doc().map_err(|e| e.to_string())?;
    let doc = read_document(d).map_err(|e| format!("document: {e:?}"))?;
    let placement_of =
        |id: Option<Id>| id.and_then(|id| doc.views.iter().find(|v| v.id() == id)).and_then(|v| v.placement()).map_or(Value::Null, to_json);
    let new_level = level_id.and_then(|id| doc.levels.iter().find(|l| l.id == id)).map_or(Value::Null, to_json);
    let new_section_basis = match section_id {
        None => Value::Null,
        Some(id) => {
            let proj = DrawingProject::new(d).map_err(|e| format!("project: {e:?}"))?;
            let view = proj.view(id).ok_or("new section not found")?;
            let host = view.host_view_id().and_then(|h| proj.view(h)).ok_or("new section has no host view")?;
            to_json(&vertical_basis(&view, &host)?)
        }
    };

    let mut results = Vec::new();
    for r in [&plan_res, &section_res] {
        match r {
            Ok(_) => results.push(json!("ok")),
            Err(e) => {
                results.push(json!(e));
                code = 1;
            }
        }
    }

    let report = json!({
        "sourceBasis": to_json(&source_basis),
        "planPlacementThrough": to_json(&through),
        "perpendicularCutLineThrough": to_json(&cut),
        "newPlan": placement_of(plan_id),
        "newLevel": new_level,
        "newSection": placement_of(section_id),
        "newSectionBasis": new_section_basis,
        "results": results,
    });
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    writeln!(out, "{text}").map_err(|e| e.to_string())?;
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> String {
        format!("{}/../lamb-core/tests/lamb/u-wall-two-sections.lamb.json", env!("CARGO_MANIFEST_DIR"))
    }

    fn go(args: &[&str], stdin: &str) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&args, stdin, &mut out, &mut err);
        (code, String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned())
    }

    fn f(v: &Value) -> f64 {
        v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"))
    }

    #[test]
    fn a_click_in_legs_lands_the_plan_and_a_square_section() {
        let (code, out, err) = go(&[&fixture(), "--from", "Legs", "--point", "4000,2700"], "");
        assert_eq!(code, 0, "{err}\n{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["results"], json!(["ok", "ok"]), "{out}");
        assert!((f(&v["planPlacementThrough"]["floorElevationMm"]) - 2700.0).abs() < 1e-6, "{out}");
        let cut = &v["perpendicularCutLineThrough"];
        assert!((f(&cut["cutFrom"]["x"]) - 4000.0).abs() < 1e-6, "{out}");
        assert!((f(&cut["cutTo"]["x"]) - 4000.0).abs() < 1e-6, "{out}");
        assert!(((f(&cut["cutTo"]["y"]) - f(&cut["cutFrom"]["y"])).abs() - 10000.0).abs() < 1e-6, "{out}");

        let doc = read_document(baseline(Some(&fixture())).unwrap().doc().unwrap()).unwrap();
        let default_cut = doc.defaults.default_cut_height_mm;
        let plan = &v["newPlan"];
        assert!(!plan.is_null(), "{out}");
        assert!((f(&plan["cutElevation"]) - (2700.0 + default_cut)).abs() < 1e-6, "{out}");
        assert!(!v["newLevel"].is_null() && !v["newSection"].is_null(), "{out}");

        let (a, b) = (&v["sourceBasis"]["cHat"], &v["newSectionBasis"]["cHat"]);
        let dot = f(&a["x"]) * f(&b["x"]) + f(&a["y"]) * f(&b["y"]);
        assert!(dot.abs() < 1e-9, "dot {dot}\n{out}");
    }

    #[test]
    fn from_must_name_a_placed_vertical_view() {
        for name in ["P", "Nope"] {
            let (code, out, err) = go(&[&fixture(), "--from", name, "--point", "4000,2700"], "");
            assert_eq!(code, 1, "{name}");
            assert!(out.is_empty(), "{out}");
            assert!(err.contains(&format!("--from {name} is not a placed vertical view")), "{err}");
        }
    }

    #[test]
    fn a_failed_record_reports_and_still_prints() {
        let (code, out, err) = go(&[&fixture(), "--from", "Legs", "--point", "4000,2700"], "{\"command\": \"nosuchcommand\"}\n");
        assert_eq!(code, 1);
        assert!(err.starts_with("FAILED nosuchcommand: "), "{err}");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["results"], json!(["ok", "ok"]), "{out}");
    }

    #[test]
    fn bad_args_exit_1() {
        for args in [
            &["--frobnicate"][..],
            &["--from", "Legs"][..],
            &["--point", "1,2"][..],
            &["--from", "Legs", "--point", "1"][..],
            &["--from", "Legs", "--point", "1,2", "--length", "x"][..],
        ] {
            let (code, out, err) = go(args, "");
            assert_eq!(code, 1, "{args:?}");
            assert!(out.is_empty() && !err.is_empty(), "{args:?}");
        }
    }
}

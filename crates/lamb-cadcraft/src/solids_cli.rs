//! What a drawing's closed cut shapes become in 3D, as a few lines of text (TS
//! `tools/solid-report.ts`): one line per solid with its bounds, one per surface, and optionally
//! the cuts one view's plane derives from those solids.
//!
//! ```text
//! cargo run -p lamb-cadcraft --example lamb_solids -- [FILE] [--faces] [--cuts <view name>] < records
//! ```
//!
//! - FILE and the stdin records are exactly `lamb_cli`'s (`crate::cli`): `.dxf` or `.lamb.json`,
//!   none → a fresh LAMB project; one `{"command", "params"}` / `"!undo"` / `"!redo"` per line.
//! - A failed record prints `FAILED <command>: <error>` on stderr and the report still prints.
//! - `--faces` adds every face's vertices under its solid.
//! - `--cuts NAME` adds the cuts the view named NAME (exact match) derives from the solids, in
//!   that view's own local 2D; no such view → `no view named NAME`.
//!
//! Numbers are rounded to 3 dp. Exit 0 ok, 1 any failure (one line on stderr).

use std::io::Write;

use lamb_core::derivation::markers::compute_solid_cuts_for_view;
use lamb_core::derivation::read::ProjectRead;
use lamb_core::derivation::synthesis::synthesize;
use lamb_core::geometry::point::Point3;
use serde::Serialize;

use crate::cli::{Record, apply, baseline, parse_records};
use crate::project::DrawingProject;

const USAGE: &str = "usage: lamb_solids [FILE] [--faces] [--cuts <view name>] < records";

#[derive(Default)]
struct Opts {
    file: Option<String>,
    faces: bool,
    cuts: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--faces" => o.faces = true,
            "--cuts" => o.cuts = Some(it.next().cloned().ok_or_else(|| format!("--cuts needs a view name\n{USAGE}"))?),
            f if f.starts_with("--") => return Err(format!("unknown flag {f}\n{USAGE}")),
            p if o.file.is_none() => o.file = Some(p.to_owned()),
            p => return Err(format!("unexpected argument {p}\n{USAGE}")),
        }
    }
    Ok(o)
}

fn r(n: f64) -> String {
    let v = (n * 1000.0).round() / 1000.0;
    // JS prints -0 as 0.
    format!("{}", if v == 0.0 { 0.0 } else { v })
}

/// An enum's serde name (`shell`, `plane`, `not_3d`, …).
fn word(v: &impl Serialize) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(e) => format!("<{e}>"),
    }
}

fn join<T: std::fmt::Display>(items: &[T]) -> String {
    items.iter().map(ToString::to_string).collect::<Vec<_>>().join(",")
}

fn range(vals: impl Iterator<Item = f64> + Clone) -> String {
    let lo = vals.clone().fold(f64::INFINITY, f64::min);
    let hi = vals.fold(f64::NEG_INFINITY, f64::max);
    format!("{}..{}", r(lo), r(hi))
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
        if res["ok"] != serde_json::Value::Bool(true) {
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

    let d = s.doc().map_err(|e| e.to_string())?;
    let read = DrawingProject::new(d).map_err(|e| format!("project: {e:?}"))?;
    let syn = synthesize(&read)?;
    let mut text = format!("{} solid(s), {} surface(s), {} path(s)\n", syn.solids.len(), syn.surfaces.len(), syn.paths.len());

    for (i, solid) in syn.solids.iter().enumerate() {
        let all: Vec<&Point3> = solid.faces.iter().flatten().collect();
        text += &format!(
            "\n[{i}] entityIds=[{}] form={} provenance={} layerId={} faces={}\n",
            join(&solid.entity_ids),
            word(&solid.form),
            word(&solid.provenance),
            solid.layer_id,
            solid.faces.len()
        );
        text += &format!("    x {}   y {}   z {}\n", range(all.iter().map(|p| p.x)), range(all.iter().map(|p| p.y)), range(all.iter().map(|p| p.z)));
        if o.faces {
            for (k, face) in solid.faces.iter().enumerate() {
                let pts: Vec<String> = face.iter().map(|p| format!("({},{},{})", r(p.x), r(p.y), r(p.z))).collect();
                text += &format!("    face {k}: {}\n", pts.join(" "));
            }
        }
    }

    if !syn.surfaces.is_empty() {
        text += "\nsurfaces (always form=plane):\n";
        for (i, sf) in syn.surfaces.iter().enumerate() {
            text += &format!(
                "  [{i}] entityId={} seg={} span={} form={} provenance={} layerId={} tiltDeg={} z {}\n",
                sf.entity_id,
                sf.segment_index,
                sf.span_index,
                word(&sf.form),
                word(&sf.provenance),
                sf.layer_id,
                r(sf.tilt_deg),
                range(sf.triangles.iter().map(|p| p.z))
            );
        }
    }

    if let Some(name) = &o.cuts {
        let Some(view_id) = read.views().find(|v| v.name == name.as_str()).map(|v| v.id) else {
            out.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
            return Err(format!("no view named {name}"));
        };
        let cuts = compute_solid_cuts_for_view(&read, view_id, Some(&syn.solids))?;
        text += &format!("\n{} derived cut(s) in view {name}, in its own local 2D:\n", cuts.len());
        for cut in &cuts {
            let pts: Vec<String> = cut.loop_.iter().map(|p| format!("({},{})", r(p.x), r(p.y))).collect();
            text += &format!("  {} from [{}] provenance={}\n    {}\n", cut.id, join(&cut.source_entity_ids), word(&cut.provenance), pts.join(" "));
        }
    }

    out.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
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

    /// A closed rectangle on WALL-EXT, a cut-default layer, in a fresh project's plan.
    const BOX: &str = concat!(
        r#"{"command": "layer.new", "params": {"name": "WALL-EXT", "current": true}}"#,
        "
",
        r#"{"command": "pline", "params": {"vertices": [[0, 0], [5000, 0], [5000, 4000], [0, 4000]], "closed": true}}"#,
        "
"
    );

    #[test]
    fn a_closed_rectangle_on_a_cut_layer_is_one_shell() {
        let (code, out, err) = go(&[], BOX);
        assert_eq!(code, 0, "{err}");
        assert!(out.starts_with("1 solid(s), "), "{out}");
        assert!(out.contains(" form=shell "), "{out}");
        assert!(!out.contains("face 0:"), "{out}");
        let (code, out, err) = go(&["--faces"], BOX);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("    face 0: ("), "{out}");
    }

    #[test]
    fn cuts_by_view_name_on_the_fixture() {
        let (code, out, err) = go(&[&fixture(), "--cuts", "Legs"], "");
        assert_eq!(code, 0, "{err}");
        assert!(out.contains(" derived cut(s) in view Legs, in its own local 2D:\n"), "{out}");
        let (code, _, err) = go(&[&fixture(), "--cuts", "no such view"], "");
        assert_eq!(code, 1);
        assert!(err.contains("no view named no such view"), "{err}");
    }

    #[test]
    fn a_failed_record_reports_and_still_prints() {
        let (code, out, err) = go(&[], &format!("{{\"command\": \"nosuchcommand\"}}\n{BOX}"));
        assert_eq!(code, 1);
        assert!(err.starts_with("FAILED nosuchcommand: "), "{err}");
        assert!(out.starts_with("1 solid(s), "), "{out}");
    }

    #[test]
    fn bad_args_exit_1() {
        for args in [&["--frobnicate"][..], &["--cuts"][..], &["a.dxf", "b.dxf"][..]] {
            let (code, out, err) = go(args, "");
            assert_eq!(code, 1, "{args:?}");
            assert!(out.is_empty() && !err.is_empty());
        }
    }
}

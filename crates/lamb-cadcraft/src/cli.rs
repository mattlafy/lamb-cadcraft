//! Command records from stdin applied headlessly, the result printed as JSON (TS `tools/cli/main.ts`):
//!
//! ```text
//! cargo run -p lamb-cadcraft --example lamb_cli -- [FILE] [--out OUT] [--query p1,p2] [--synth] < records
//! ```
//!
//! - FILE: `.dxf` opened with OPEN, `.lamb.json` imported as a LAMB project; none → a fresh LAMB
//!   project.
//! - stdin: one record per non-blank line — `{"command": id, "params": {...}}` (the assistant's
//!   script step shape; `params` defaults to `{}`), or the string `"!undo"` / `"!redo"`.
//! - stdout: one JSON object, `{project, results, issues, synthesis?}`. `project` is the drawing
//!   exported as a lamb-core `Project`; `results` has one `{ok, value | error}` per record
//!   (`{ok, directive, label}` for undo/redo); `issues` is lamb-core's issue list; `synthesis` the
//!   derived 3D model, only with `--synth`.
//! - `--query a.b[0].c,x` prints `{path: value}` instead (a missing path is `null`).
//! - `--out X.dxf` writes the drawing as DXF; `--out X.lamb.json` writes the exported project as
//!   one JSON object path → file content (the fixture format).
//!
//! A failed record does not stop the run. Exit 0 every record ok, 2 any record failed, 1 bad
//! arguments, a bad record line, an unloadable file or any other error (one line on stderr).

use std::collections::BTreeMap;
use std::io::Write;

use cadcraft_engine::Session;
use serde_json::{Map, Value, json};

use crate::assistant_cli::load;
use crate::export::drawing_to_project;
use crate::project::DrawingProject;

const USAGE: &str = "usage: lamb_cli [FILE] [--out OUT.dxf|OUT.lamb.json] [--query p1,p2] [--synth] < records";

#[derive(Default)]
struct Opts {
    file: Option<String>,
    out: Option<String>,
    query: Option<Vec<String>>,
    synth: bool,
}

/// One stdin line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Record {
    Command { id: String, params: Value },
    Undo,
    Redo,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value\n{USAGE}"));
        match a.as_str() {
            "--out" => o.out = Some(value("--out")?),
            "--query" => o.query = Some(value("--query")?.split(',').map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()).collect()),
            "--synth" => o.synth = true,
            f if f.starts_with("--") => return Err(format!("unknown flag {f}\n{USAGE}")),
            p if o.file.is_none() => o.file = Some(p.to_owned()),
            p => return Err(format!("unexpected argument {p}\n{USAGE}")),
        }
    }
    Ok(o)
}

pub(crate) fn parse_records(stdin: &str) -> Result<Vec<Record>, String> {
    let mut records = Vec::new();
    for (i, line) in stdin.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let bad = |why: &str| format!("stdin line {}: {why}: {}", i + 1, line.trim());
        let v: Value = serde_json::from_str(line.trim()).map_err(|e| bad(&e.to_string()))?;
        records.push(match &v {
            Value::String(s) if s == "!undo" => Record::Undo,
            Value::String(s) if s == "!redo" => Record::Redo,
            Value::Object(o) => match o.get("command") {
                Some(Value::String(id)) => Record::Command { id: id.clone(), params: o.get("params").cloned().unwrap_or_else(|| json!({})) },
                _ => return Err(bad("expected a string \"command\"")),
            },
            _ => return Err(bad("expected {\"command\", \"params\"}, \"!undo\" or \"!redo\"")),
        });
    }
    Ok(records)
}

/// The loaded session with no undo history, so `"!undo"` never reaches past the loaded drawing
/// (a fresh project's own LAMBPROJECT included).
pub(crate) fn baseline(file: Option<&str>) -> Result<Session, String> {
    let mut s = load(file)?;
    let st = s.state_mut().map_err(|e| e.to_string())?;
    st.undo.clear();
    st.redo.clear();
    Ok(s)
}

/// Applies each record in order, one result per record; a failure does not stop the rest.
pub(crate) fn apply(s: &mut Session, records: &[Record]) -> Vec<Value> {
    records
        .iter()
        .map(|r| match r {
            Record::Command { id, params } => match s.execute(id, params) {
                Ok(value) => json!({ "ok": true, "value": value }),
                Err(e) => json!({ "ok": false, "error": e.to_string() }),
            },
            Record::Undo | Record::Redo => {
                let (name, step) = if *r == Record::Undo { ("undo", s.undo()) } else { ("redo", s.redo()) };
                match step {
                    Ok(Some(label)) => json!({ "ok": true, "directive": name, "label": label }),
                    Ok(None) => json!({ "ok": false, "directive": name, "label": null, "error": format!("nothing to {name}") }),
                    Err(e) => json!({ "ok": false, "directive": name, "label": null, "error": e.to_string() }),
                }
            }
        })
        .collect()
}

/// The path grammar of TS `getPath`: `key[0][1].key`; anything else is `None`.
pub(crate) fn get_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = root;
    for segment in path.split('.') {
        let (key, mut rest) = segment.split_at(segment.find('[').unwrap_or(segment.len()));
        if key.is_empty() {
            return None;
        }
        cur = cur.get(key)?;
        while !rest.is_empty() {
            let close = rest.find(']')?;
            let digits = rest.get(1..close)?;
            if !rest.starts_with('[') || digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            cur = cur.get(digits.parse::<usize>().ok()?)?;
            rest = rest.get(close + 1..)?;
        }
    }
    Some(cur)
}

fn report(s: &Session, results: Vec<Value>, synth: bool) -> Result<Value, String> {
    let d = s.doc().map_err(|e| e.to_string())?;
    let (project, _) = drawing_to_project(d).map_err(|e| format!("export: {e:?}"))?;
    let read = DrawingProject::new(d).map_err(|e| format!("project: {e:?}"))?;
    let issues = lamb_core::derivation::issues::find_issues(&read)?;
    let mut r = Map::new();
    r.insert("project".into(), serde_json::to_value(&project).map_err(|e| e.to_string())?);
    r.insert("results".into(), Value::Array(results));
    r.insert("issues".into(), serde_json::to_value(&issues).map_err(|e| e.to_string())?);
    if synth {
        let synthesis = lamb_core::derivation::synthesis::synthesize(&read)?;
        r.insert("synthesis".into(), serde_json::to_value(&synthesis).map_err(|e| e.to_string())?);
    }
    Ok(Value::Object(r))
}

pub(crate) fn write_out(s: &Session, path: &str) -> Result<(), String> {
    let d = s.doc().map_err(|e| e.to_string())?;
    let text = if path.to_ascii_lowercase().ends_with(".dxf") {
        cadcraft_io::write_dxf(d)
    } else if path.ends_with(".lamb.json") {
        let (project, _) = drawing_to_project(d).map_err(|e| format!("export: {e:?}"))?;
        let files: BTreeMap<String, String> = lamb_core::persistence::project_to_files(&project).into_iter().collect();
        serde_json::to_string_pretty(&files).map_err(|e| e.to_string())?
    } else {
        return Err(format!("{path}: --out expects a .dxf or .lamb.json file"));
    };
    std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))
}

/// Runs the command line `args` (without the program name) over the records in `stdin`; returns
/// the exit code.
pub fn run(args: &[String], stdin: &str, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    match run_inner(args, stdin, out) {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(err, "{e}");
            1
        }
    }
}

fn run_inner(args: &[String], stdin: &str, out: &mut dyn Write) -> Result<i32, String> {
    let o = parse_args(args)?;
    let records = parse_records(stdin)?;
    let mut s = baseline(o.file.as_deref())?;
    let results = apply(&mut s, &records);
    let all_ok = results.iter().all(|r| r["ok"] == json!(true));
    let full = report(&s, results, o.synth)?;
    if let Some(path) = &o.out {
        write_out(&s, path)?;
    }
    let printed = match &o.query {
        Some(paths) => Value::Object(paths.iter().map(|p| (p.clone(), get_path(&full, p).cloned().unwrap_or(Value::Null))).collect()),
        None => full,
    };
    let text = serde_json::to_string(&printed).map_err(|e| e.to_string())?;
    writeln!(out, "{text}").map_err(|e| e.to_string())?;
    Ok(if all_ok { 0 } else { 2 })
}

#[cfg(test)]
mod tests {
    use super::*;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;

    fn fixture() -> String {
        format!("{}/../lamb-core/tests/lamb/u-wall-two-sections.lamb.json", env!("CARGO_MANIFEST_DIR"))
    }

    fn go(args: &[&str], stdin: &str) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&args, stdin, &mut out, &mut err);
        (code, String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned())
    }

    fn json_of(out: &str) -> std::result::Result<Value, serde_json::Error> {
        serde_json::from_str(out.trim())
    }

    const LINE: &str = r#"{"command": "line", "params": {"points": [[0, 0], [1000, 0]]}}"#;

    #[test]
    fn query_views_on_the_fixture_matches_its_view_count() -> R {
        let (code, out, err) = go(&[&fixture(), "--query", "project.views,nope.x[0]"], "");
        assert_eq!(code, 0, "{err}");
        let text = std::fs::read_to_string(fixture())?;
        let files: BTreeMap<String, String> = serde_json::from_str(&text)?;
        let p = lamb_core::persistence::files_to_project(&files).map_err(|e| format!("{e:?}"))?;
        let v = json_of(&out)?;
        assert_eq!(v["project.views"].as_array().map(Vec::len), Some(p.views.len()), "{out}");
        assert!(!p.views.is_empty());
        assert_eq!(v["nope.x[0]"], Value::Null);
        Ok(())
    }

    #[test]
    fn a_line_record_adds_one_entity_and_undo_takes_it_back() -> R {
        let (code, out, err) = go(&["--query", "results[0].ok,results[1].directive"], &format!("{LINE}\n\n\"!undo\"\n"));
        assert_eq!(code, 0, "{err}");
        let v = json_of(&out)?;
        assert_eq!(v["results[0].ok"], json!(true), "{out}");
        assert_eq!(v["results[1].directive"], json!("undo"), "{out}");

        let count = |s: &Session| -> std::result::Result<usize, Box<dyn std::error::Error>> {
            let (p, _) = drawing_to_project(s.doc()?).map_err(|e| format!("{e:?}"))?;
            Ok(p.views.iter().map(|v| v.entities().len()).sum())
        };
        let mut s = baseline(None)?;
        let before = count(&s)?;
        let r = apply(&mut s, &parse_records(LINE)?);
        assert_eq!(r[0]["ok"], json!(true), "{r:?}");
        assert_eq!(count(&s)?, before + 1);
        let r = apply(&mut s, &parse_records("\"!undo\"")?);
        assert_eq!(r[0]["ok"], json!(true), "{r:?}");
        assert_eq!(count(&s)?, before);
        Ok(())
    }

    #[test]
    fn undo_on_a_fresh_project_exits_2() -> R {
        let (code, out, err) = go(&["--query", "results[0].ok"], "\"!undo\"\n");
        assert_eq!(code, 2, "{out} {err}");
        assert_eq!(json_of(&out)?["results[0].ok"], json!(false));
        Ok(())
    }

    #[test]
    fn a_bad_command_exits_2_and_the_next_record_still_applies() -> R {
        let (code, out, err) = go(&["--query", "results"], &format!("{{\"command\": \"nosuchcommand\"}}\n{LINE}\n"));
        assert_eq!(code, 2, "{err}");
        let v = json_of(&out)?;
        assert_eq!(v["results"][0]["ok"], json!(false), "{out}");
        assert!(v["results"][0]["error"].is_string(), "{out}");
        assert_eq!(v["results"][1]["ok"], json!(true), "{out}");
        Ok(())
    }

    #[test]
    fn synth_gives_solids_and_issues_are_always_present() -> R {
        let (code, out, err) = go(&[&fixture(), "--synth", "--query", "synthesis.solids"], "");
        assert_eq!(code, 0, "{err}");
        assert!(json_of(&out)?["synthesis.solids"].is_array(), "{out}");
        let (code, out, err) = go(&[&fixture()], "");
        assert_eq!(code, 0, "{err}");
        let v = json_of(&out)?;
        assert!(v["issues"].is_array(), "issues present");
        assert!(v.get("synthesis").is_none(), "no synthesis without --synth");
        assert!(v["results"].as_array().is_some_and(Vec::is_empty));
        Ok(())
    }

    #[test]
    fn out_lamb_json_and_dxf_round_trip() -> R {
        let dir = std::env::temp_dir();
        let json_path = dir.join(format!("lamb_cli_out_{}.lamb.json", std::process::id()));
        let dxf_path = dir.join(format!("lamb_cli_out_{}.dxf", std::process::id()));
        for path in [&json_path, &dxf_path] {
            let p = path.to_string_lossy().into_owned();
            let (code, _, err) = go(&[&fixture(), "--out", &p, "--query", "results"], LINE);
            assert_eq!(code, 0, "{err}");
        }
        let text = std::fs::read_to_string(&json_path)?;
        let dxf = std::fs::read(&dxf_path)?;
        let _ = std::fs::remove_file(&json_path);
        let _ = std::fs::remove_file(&dxf_path);
        let files: BTreeMap<String, String> = serde_json::from_str(&text)?;
        let back = lamb_core::persistence::files_to_project(&files).map_err(|e| format!("{e:?}"))?;
        assert!(!back.views.is_empty());
        assert!(cadcraft_io::read_dxf(&dxf).is_ok());
        let (code, _, err) = go(&["--out", "x.txt"], "");
        assert_eq!(code, 1, "{err}");
        Ok(())
    }

    #[test]
    fn bad_args_and_bad_lines_exit_1_with_one_line() {
        for (args, stdin) in
            [(&["--frobnicate"][..], ""), (&["a.dxf", "b.dxf"][..], ""), (&[][..], "not json\n"), (&[][..], "[\"line\", {}]\n"), (&["x.txt"][..], "")]
        {
            let (code, out, err) = go(args, stdin);
            assert_eq!(code, 1, "{args:?} {stdin:?} {err}");
            assert!(out.is_empty(), "{out}");
            assert!(!err.is_empty());
        }
    }

    #[test]
    fn get_path_follows_the_ts_grammar() {
        let v = json!({ "a": { "b": [[1, 2], [3, { "c": 4 }]] } });
        assert_eq!(get_path(&v, "a.b[1][1].c"), Some(&json!(4)));
        assert_eq!(get_path(&v, "a.b[0]"), Some(&json!([1, 2])));
        assert_eq!(get_path(&v, "a.b[9]"), None);
        assert_eq!(get_path(&v, "a.b[x]"), None);
        assert_eq!(get_path(&v, "a..b"), None);
    }
}

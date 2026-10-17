//! One assistant script from stdin, validated whole and then applied as one undo step that
//! changes nothing if any step fails (TS `tools/cli/script.ts`):
//!
//! ```text
//! cargo run -p lamb-cadcraft --example lamb_script -- [FILE] [--out OUT] [--query p1,p2] [--check] [--catalogue] [--prompt [MESSAGE]] < script.json
//! ```
//!
//! - FILE: as `lamb_cli` (`.dxf` or `.lamb.json`; none → a fresh LAMB project).
//! - stdin: one JSON document, a bare array of `{command, params}` steps or `{steps, proposalId?}`.
//!   Anything else is refused with one stderr line ("not a JSON script: …"), exit 1.
//! - `check` is always computed first: `{ok, steps, lint: [{code, step, message}], dryRun?, error?}`.
//!   `lint` notes are retry hints and never make `ok` false; a failing dry run does, with `error`
//!   naming `step <i> (<id>): …`.
//! - `--check`: prints `{check}` and applies nothing; exit 0 when `check.ok`, 2 otherwise.
//! - default: when `check.ok`, applies through `assistant.apply` (one undo step, AI provenance
//!   stamped as the assistant's turn does) and prints `{check, result, project}` — `result` is the
//!   apply value `{proposalId, stamped, created, modified}` or `{ok: false, error}`. Exit 0 applied,
//!   2 rejected or failed (drawing untouched). `--out X.dxf|X.lamb.json` writes the drawing.
//! - `--catalogue`: prints `assistant::catalogue()`; `--prompt [MESSAGE]`: prints the prompt the
//!   assistant would send (role + catalogue, header, user) joined by blank lines. Both ignore stdin.
//! - `--query a.b,c` prints `{path: value}` over the output object (a missing path is `null`).
//!
//! Exit 1: bad arguments, stdin that is not a JSON script, an unloadable file, any other error.

use std::io::Write;

use cadcraft_engine::Session;
use serde_json::{Map, Value, json};

use crate::assistant;
use crate::cli::{baseline, get_path, write_out};
use crate::export::drawing_to_project;

const USAGE: &str =
    "usage: lamb_script [FILE] [--out OUT.dxf|OUT.lamb.json] [--query p1,p2] [--check] [--catalogue] [--prompt [MESSAGE]] < script.json";

#[derive(Default)]
struct Opts {
    file: Option<String>,
    out: Option<String>,
    query: Option<Vec<String>>,
    check: bool,
    catalogue: bool,
    prompt: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" | "--query" => {
                let v = it.next().cloned().ok_or_else(|| format!("{a} needs a value\n{USAGE}"))?;
                if a == "--out" {
                    o.out = Some(v);
                } else {
                    o.query = Some(v.split(',').map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()).collect());
                }
            }
            "--check" => o.check = true,
            "--catalogue" => o.catalogue = true,
            "--prompt" => {
                let msg = match it.peek() {
                    Some(m) if !m.starts_with("--") => it.next().cloned(),
                    _ => None,
                };
                o.prompt = Some(msg.unwrap_or_default());
            }
            f if f.starts_with("--") => return Err(format!("unknown flag {f}\n{USAGE}")),
            p if o.file.is_none() => o.file = Some(p.to_owned()),
            p => return Err(format!("unexpected argument {p}\n{USAGE}")),
        }
    }
    Ok(o)
}

/// stdin as a script: one JSON document holding a steps array.
fn parse_script(stdin: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(stdin.trim()).map_err(|e| format!("not a JSON script: {e}"))?;
    if assistant::script_steps(&v).is_none() {
        return Err("not a JSON script: expected an array of {command, params} or {steps: [...]}".to_owned());
    }
    Ok(v)
}

/// The `check` object: lint plus a dry run, nothing left changed.
fn check(s: &mut Session, script: &Value) -> Result<Value, String> {
    let steps = assistant::script_steps(script).cloned().unwrap_or_default();
    let lint: Vec<Value> = assistant_lint_json(s, &steps)?;
    let mut c = Map::new();
    c.insert("steps".into(), json!(steps.len()));
    c.insert("lint".into(), Value::Array(lint));
    match assistant::dry_run(s, &steps) {
        Ok(d) => {
            c.insert("ok".into(), json!(true));
            c.insert("dryRun".into(), d.to_json(&steps));
        }
        Err(e) => {
            c.insert("ok".into(), json!(false));
            c.insert("error".into(), json!(e.to_string()));
        }
    }
    Ok(Value::Object(c))
}

fn assistant_lint_json(s: &Session, steps: &[Value]) -> Result<Vec<Value>, String> {
    let d = s.doc().map_err(|e| e.to_string())?;
    Ok(crate::assistant_lint::lint(d, steps).into_iter().map(|n| json!({ "code": n.code, "step": n.step, "message": n.message })).collect())
}

fn project_json(s: &Session) -> Result<Value, String> {
    let (project, _) = drawing_to_project(s.doc().map_err(|e| e.to_string())?).map_err(|e| format!("export: {e:?}"))?;
    serde_json::to_value(&project).map_err(|e| e.to_string())
}

/// Checks `script` on `s`, then (unless `check_only`) applies it when the check passed. Returns
/// the exit code and the output object.
fn run_script(s: &mut Session, script: &Value, check_only: bool) -> Result<(i32, Value), String> {
    let c = check(s, script)?;
    let ok = c["ok"] == json!(true);
    if check_only {
        return Ok((if ok { 0 } else { 2 }, json!({ "check": c })));
    }
    let (code, result) = if ok {
        match s.execute(assistant::ID, script) {
            Ok(v) => (0, v),
            Err(e) => (2, json!({ "ok": false, "error": e.to_string() })),
        }
    } else {
        (2, json!({ "ok": false, "error": c["error"].clone() }))
    };
    Ok((code, json!({ "check": c, "result": result, "project": project_json(s)? })))
}

/// Runs the command line `args` (without the program name) over the script in `stdin`; returns
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
    if o.catalogue {
        writeln!(out, "{}", assistant::catalogue()).map_err(|e| e.to_string())?;
        return Ok(0);
    }
    if let Some(user) = &o.prompt {
        let s = baseline(o.file.as_deref())?;
        let msgs = assistant::prompt(&s, user).map_err(|e| e.to_string())?;
        let text = msgs.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join("\n\n");
        writeln!(out, "{text}").map_err(|e| e.to_string())?;
        return Ok(0);
    }
    let script = parse_script(stdin)?;
    let mut s = baseline(o.file.as_deref())?;
    let (code, full) = run_script(&mut s, &script, o.check)?;
    if let (Some(path), false) = (&o.out, o.check) {
        write_out(&s, path)?;
    }
    let printed = match &o.query {
        Some(paths) => Value::Object(paths.iter().map(|p| (p.clone(), get_path(&full, p).cloned().unwrap_or(Value::Null))).collect()),
        None => full,
    };
    let text = serde_json::to_string(&printed).map_err(|e| e.to_string())?;
    writeln!(out, "{text}").map_err(|e| e.to_string())?;
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;

    fn go(args: &[&str], stdin: &str) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&args, stdin, &mut out, &mut err);
        (code, String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned())
    }

    fn line(y: i64) -> Value {
        json!({ "command": "line", "params": { "points": [[0, y], [1000, y]] } })
    }

    fn good() -> Value {
        json!({ "steps": [line(0), line(500)] })
    }

    fn entity_count(p: &Value) -> usize {
        p["views"].as_array().map(|vs| vs.iter().map(|v| v["entities"].as_array().map_or(0, Vec::len)).sum()).unwrap_or(0)
    }

    #[test]
    fn a_two_step_script_applies_as_one_undo_step() -> R {
        let mut s = baseline(None)?;
        let before = entity_count(&project_json(&s)?);
        let (code, v) = run_script(&mut s, &good(), false)?;
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["result"]["created"], json!(2), "{v}");
        assert_eq!(v["result"]["stamped"].as_array().map(Vec::len), Some(2), "{v}");
        assert_eq!(entity_count(&v["project"]), before + 2, "{v}");
        assert_eq!(s.state()?.undo.len(), 1);
        s.undo()?;
        assert_eq!(entity_count(&project_json(&s)?), before, "one undo removes both");

        let (code, out, err) = go(&["--query", "result.created"], &good().to_string());
        assert_eq!(code, 0, "{err}");
        let q: Value = serde_json::from_str(out.trim())?;
        assert_eq!(q, json!({ "result.created": 2 }), "only that key");
        Ok(())
    }

    #[test]
    fn a_failing_step_rejects_the_whole_script() -> R {
        let bad = json!([line(0), { "command": "nosuchcommand", "params": {} }]);
        let mut s = baseline(None)?;
        let before = entity_count(&project_json(&s)?);
        let (code, v) = run_script(&mut s, &bad, false)?;
        assert_eq!(code, 2, "{v}");
        assert_eq!(v["check"]["ok"], json!(false), "{v}");
        assert!(v["check"]["error"].as_str().is_some_and(|e| e.starts_with("step 2 (")), "{v}");
        assert_eq!(v["result"]["ok"], json!(false), "{v}");
        assert_eq!(entity_count(&v["project"]), before, "no new line");
        assert!(s.state()?.undo.is_empty(), "undo stack empty");
        Ok(())
    }

    #[test]
    fn check_applies_nothing_and_lint_flags_invented_handles() -> R {
        let mut s = baseline(None)?;
        let before = entity_count(&project_json(&s)?);
        let (code, v) = run_script(&mut s, &good(), true)?;
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["check"]["dryRun"]["created"].as_array().map(Vec::len), Some(2), "{v}");
        assert!(v.get("result").is_none() && v.get("project").is_none(), "{v}");
        assert_eq!(entity_count(&project_json(&s)?), before, "nothing applied");
        assert!(s.state()?.undo.is_empty());

        let (code, out, err) = go(&["--check"], &json!([{ "command": "move", "params": { "handles": ["ffff"], "delta": [1, 0] } }]).to_string());
        let v: Value = serde_json::from_str(out.trim())?;
        assert!(v["check"]["lint"].as_array().is_some_and(|l| l.iter().any(|n| n["code"] == json!("invented-handle"))), "{out} {err}");
        assert_eq!(code, if v["check"]["ok"] == json!(true) { 0 } else { 2 }, "{out}");
        Ok(())
    }

    #[test]
    fn not_json_and_bad_args_exit_1() -> R {
        for (args, stdin) in
            [(&[][..], "not json"), (&[][..], "{\"no\": 1}"), (&[][..], "line 0,0"), (&["--frobnicate"][..], "[]"), (&["--out"][..], "[]")]
        {
            let (code, out, err) = go(args, stdin);
            assert_eq!(code, 1, "{args:?} {stdin:?}");
            assert!(out.is_empty() && !err.is_empty(), "{out} {err}");
        }
        let (_, _, err) = go(&[], "not json");
        assert!(err.starts_with("not a JSON script"), "{err}");
        Ok(())
    }

    #[test]
    fn catalogue_and_prompt() {
        let (code, out, _) = go(&["--catalogue"], "ignored");
        assert_eq!(code, 0);
        assert_eq!(out, format!("{}\n", assistant::catalogue()));
        let (code, out, err) = go(&["--prompt", "draw"], "ignored");
        assert_eq!(code, 0, "{err}");
        assert!(out.contains(&assistant::catalogue()), "{out}");
        assert!(out.trim_end().ends_with("draw"), "{out}");
    }
}

//! A whole interactive command session driven headlessly, with what the user would see read back
//! after every record (TS `tools/cli/session.ts`):
//!
//! ```text
//! cargo run -p lamb-cadcraft --example lamb_session -- [FILE] [--out OUT] [--query p1,p2] [--scale px-per-mm] [--final] [--no-snap] < records
//! ```
//!
//! - FILE: as `lamb_cli` (`.dxf` or `.lamb.json`; none → a fresh LAMB project). Undo never reaches
//!   past the loaded drawing.
//! - stdin: one JSON array per non-blank line, `[verb, ...args]`:
//!   `["command","LINE"]`, `["entry","500"]` (`"type"` is an alias) — a line typed + Enter;
//!   `["enter"]` / `["space"]`; `["esc"]`; `["click",x,y]` / `["move",x,y]`, optionally with
//!   `{"shift":true,"raw":true}` last; `["select","1A",...]` (hex handles); `["clearSelection"]`;
//!   `["ortho",true]`; `["undo"]` / `["redo"]`. An unknown verb or bad args fails the whole run.
//! - `move` / `click` snap through the engine's cursor pipeline (object snaps, tracking, ORTHO) as
//!   the canvas does, at a prompt that takes a point, unless `--no-snap` or `"raw":true`.
//!   `--scale` (screen px per mm, default 1) sizes the aperture and tracking tolerance.
//! - stdout: one JSON object `{snapshots, applied, project}`. Each snapshot is `{prompt, active,
//!   log, cursor, snap, preview, selection, error?}`: `active` is the running command (lowercased)
//!   or null, `log` the lines this record added, `snap` the snap mode's name, `preview` the
//!   rubber-band entities' kinds. `applied` is `[{ok, error?}]`; `project` the exported lamb-core
//!   `Project`. `--final` keeps only the last snapshot; `--query a.b[0]` prints `{path: value}`;
//!   `--out` as `lamb_cli`.
//!
//! Exit 0 every record ok, 2 any record errored, 1 bad arguments, a bad stdin line or an
//! unloadable file (one line on stderr).

use std::io::Write;

use cadcraft_doc::Handle;
use cadcraft_engine::geom::Vec2;
use cadcraft_engine::snap::tracking::CursorQuery;
use cadcraft_engine::{Input, Session};
use serde_json::{Map, Value, json};

use crate::cli::{baseline, get_path, write_out};
use crate::export::drawing_to_project;

const USAGE: &str = "usage: lamb_session [FILE] [--out OUT.dxf|OUT.lamb.json] [--query p1,p2] [--scale px-per-mm] [--final] [--no-snap] < records";

struct Opts {
    file: Option<String>,
    out: Option<String>,
    query: Option<Vec<String>>,
    scale: f64,
    last_only: bool,
    no_snap: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Rec {
    Line(String),
    Enter,
    Esc,
    Click { p: Vec2, shift: bool, raw: bool },
    Move { p: Vec2, shift: bool, raw: bool },
    Select(Vec<Handle>),
    ClearSelection,
    Ortho(bool),
    Undo,
    Redo,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts { file: None, out: None, query: None, scale: 1.0, last_only: false, no_snap: false };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value\n{USAGE}"));
        match a.as_str() {
            "--out" => o.out = Some(value("--out")?),
            "--query" => o.query = Some(value("--query")?.split(',').map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()).collect()),
            "--scale" => {
                let v = value("--scale")?;
                o.scale = v.parse::<f64>().ok().filter(|s| s.is_finite() && *s > 0.0).ok_or_else(|| format!("--scale: bad number {v}\n{USAGE}"))?;
            }
            "--final" => o.last_only = true,
            "--no-snap" => o.no_snap = true,
            f if f.starts_with("--") => return Err(format!("unknown flag {f}\n{USAGE}")),
            p if o.file.is_none() => o.file = Some(p.to_owned()),
            p => return Err(format!("unexpected argument {p}\n{USAGE}")),
        }
    }
    Ok(o)
}

fn parse_point(args: &[Value]) -> Result<(Vec2, bool, bool), String> {
    let (x, y) = match args {
        [x, y] | [x, y, Value::Object(_)] => (x.as_f64().ok_or("x is not a number")?, y.as_f64().ok_or("y is not a number")?),
        _ => return Err("expected x, y and an optional {\"shift\", \"raw\"}".into()),
    };
    let flag = |k: &str| args.get(2).and_then(|o| o.get(k)).and_then(Value::as_bool).unwrap_or(false);
    Ok((Vec2::new(x, y), flag("shift"), flag("raw")))
}

fn parse_record(v: &Value) -> Result<Rec, String> {
    let Some((Value::String(verb), args)) = v.as_array().and_then(|a| a.split_first()) else {
        return Err("expected [verb, ...args]".into());
    };
    let none = |r: Rec| if args.is_empty() { Ok(r) } else { Err(format!("{verb} takes no arguments")) };
    Ok(match verb.as_str() {
        "command" | "entry" | "type" => match args {
            [Value::String(s)] => Rec::Line(s.clone()),
            _ => return Err(format!("{verb} takes one string")),
        },
        "enter" | "space" => none(Rec::Enter)?,
        "esc" => none(Rec::Esc)?,
        "click" => {
            let (p, shift, raw) = parse_point(args)?;
            Rec::Click { p, shift, raw }
        }
        "move" => {
            let (p, shift, raw) = parse_point(args)?;
            Rec::Move { p, shift, raw }
        }
        "select" => Rec::Select(
            args.iter().map(|a| a.as_str().and_then(Handle::parse_hex).ok_or_else(|| format!("select: bad handle {a}"))).collect::<Result<_, _>>()?,
        ),
        "clearSelection" => none(Rec::ClearSelection)?,
        "ortho" => match args {
            [Value::Bool(b)] => Rec::Ortho(*b),
            _ => return Err("ortho takes true or false".into()),
        },
        "undo" => none(Rec::Undo)?,
        "redo" => none(Rec::Redo)?,
        other => return Err(format!("unknown verb {other}")),
    })
}

fn parse_records(stdin: &str) -> Result<Vec<Rec>, String> {
    stdin
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, line)| {
            let v = serde_json::from_str::<Value>(line.trim()).map_err(|e| e.to_string());
            v.and_then(|v| parse_record(&v)).map_err(|why| format!("stdin line {}: {why}: {}", i + 1, line.trim()))
        })
        .collect()
}

/// The point a cursor at `raw` resolves to, and the snap it took, as the canvas resolves it.
fn resolve(s: &mut Session, raw: Vec2, shift: bool, snap: bool, scale: f64) -> (Vec2, Option<cadcraft_engine::snap::SnapHit>) {
    let prompt = s.current_prompt();
    let Some(p) = prompt.filter(|p| snap && cadcraft_engine::pointmod::accepts(p)) else {
        return (raw, None);
    };
    let q = CursorQuery {
        base: p.base,
        deferred: p.deferred,
        corner: p.corner,
        aperture: s.settings.aperture / scale,
        tol: 6.0 / scale,
        now: 0.0,
        shift,
    };
    let r = s.snap_cursor(raw, &q);
    (r.point, r.snap)
}

fn step(s: &mut Session, rec: &Rec, o: &Opts, snap: &mut Option<cadcraft_engine::snap::SnapHit>) -> Result<(), String> {
    let e = |r: cadcraft_engine::Result<()>| r.map_err(|e| e.to_string());
    match rec {
        Rec::Line(t) => e(s.cmdline(t)),
        Rec::Enter => e(s.input(Input::Enter)),
        Rec::Esc => {
            s.cancel();
            Ok(())
        }
        Rec::Move { p, shift, raw } | Rec::Click { p, shift, raw } => {
            let (point, hit) = resolve(s, *p, *shift, !o.no_snap && !raw, o.scale);
            s.cursor = point;
            *snap = hit;
            if !matches!(rec, Rec::Click { .. }) {
                return Ok(());
            }
            match (s.current_prompt(), hit) {
                (None, _) => e(s.idle_click(point, *shift)),
                (Some(_), Some(hit)) => e(s.input_snapped(hit)),
                (Some(_), None) => e(s.input(Input::Point(point))),
            }
        }
        Rec::Select(hs) => {
            s.set_selection(hs.clone());
            Ok(())
        }
        Rec::ClearSelection => {
            s.set_selection(Vec::new());
            Ok(())
        }
        Rec::Ortho(b) => {
            s.settings.orthomode = *b;
            Ok(())
        }
        Rec::Undo | Rec::Redo => {
            let (name, r) = if *rec == Rec::Undo { ("undo", s.undo()) } else { ("redo", s.redo()) };
            match r.map_err(|e| e.to_string())? {
                Some(_) => Ok(()),
                None => Err(format!("nothing to {name}")),
            }
        }
    }
}

fn snapshot(s: &Session, log_from: usize, snap: Option<&cadcraft_engine::snap::SnapHit>, error: Option<&str>) -> Value {
    let prompt = s.prompt_text();
    let active = if prompt == "Command:" { Value::Null } else { prompt.split_whitespace().next().map_or(Value::Null, |w| json!(w.to_lowercase())) };
    let mut preview: Vec<Value> = s.preview(s.cursor).iter().map(|e| serde_json::to_value(&e.kind).unwrap_or(Value::Null)).collect();
    if let Some((_, extra)) = s.drag(s.cursor) {
        preview.extend(extra.iter().map(|e| serde_json::to_value(&e.kind).unwrap_or(Value::Null)));
    }
    let mut m = Map::new();
    m.insert("prompt".into(), json!(prompt));
    m.insert("active".into(), active);
    m.insert("log".into(), json!(s.log.get(log_from..).unwrap_or_default()));
    m.insert("cursor".into(), json!([s.cursor.x, s.cursor.y]));
    m.insert("snap".into(), snap.map_or(Value::Null, |h| json!(h.name)));
    m.insert("preview".into(), Value::Array(preview));
    m.insert("selection".into(), json!(s.selection().into_iter().map(Handle::hex).collect::<Vec<_>>()));
    if let Some(e) = error {
        m.insert("error".into(), json!(e));
    }
    Value::Object(m)
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
    let (mut snapshots, mut applied) = (Vec::new(), Vec::new());
    for rec in &records {
        let log_from = s.log.len();
        let mut snap = None;
        let r = step(&mut s, rec, &o, &mut snap);
        let error = r.as_ref().err().map(String::as_str);
        snapshots.push(snapshot(&s, log_from, snap.as_ref(), error));
        applied.push(match error {
            None => json!({ "ok": true }),
            Some(e) => json!({ "ok": false, "error": e }),
        });
    }
    let all_ok = applied.iter().all(|r| r["ok"] == json!(true));
    if o.last_only {
        let last = snapshots.pop();
        snapshots = last.into_iter().collect();
    }
    let (project, _) = drawing_to_project(s.doc().map_err(|e| e.to_string())?).map_err(|e| format!("export: {e:?}"))?;
    let full = json!({
        "snapshots": snapshots,
        "applied": applied,
        "project": serde_json::to_value(&project).map_err(|e| e.to_string())?,
    });
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

    fn go(args: &[&str], stdin: &str) -> (i32, Value, String) {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&args, stdin, &mut out, &mut err);
        let v = serde_json::from_slice(&out).unwrap_or(Value::Null);
        (code, v, String::from_utf8_lossy(&err).into_owned())
    }

    /// Every one-segment polyline (a line) in the exported project, as `[x1, y1, x2, y2]`.
    fn lines(v: &Value) -> Vec<[f64; 4]> {
        let mut found = Vec::new();
        fn walk(v: &Value, found: &mut Vec<[f64; 4]>) {
            match v {
                Value::Object(m) => {
                    if let (Some(a), Some(Value::Array(segs))) = (m.get("start"), m.get("segments"))
                        && let [seg] = segs.as_slice()
                    {
                        let n = |p: &Value, k: &str| p.get(k).and_then(Value::as_f64).unwrap_or(f64::NAN);
                        found.push([n(a, "x"), n(a, "y"), n(&seg["to"], "x"), n(&seg["to"], "y")]);
                    }
                    m.values().for_each(|c| walk(c, found));
                }
                Value::Array(a) => a.iter().for_each(|c| walk(c, found)),
                _ => {}
            }
        }
        walk(&v["project"], &mut found);
        found
    }

    fn close(a: [f64; 4], b: [f64; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
    }

    const DRAW: &str = "[\"command\",\"LINE\"]\n[\"click\",0,0]\n[\"move\",1000,500]\n[\"click\",1000,0]\n[\"enter\"]\n";

    #[test]
    fn line_previews_then_commits() -> R {
        let (code, v, err) = go(&[], DRAW);
        assert_eq!(code, 0, "{err}");
        let snaps = v["snapshots"].as_array().ok_or("snapshots")?;
        assert_eq!(snaps.len(), 5);
        assert_eq!(snaps[2]["active"], json!("line"), "{}", snaps[2]);
        let preview = snaps[2]["preview"].as_array().ok_or("preview")?;
        assert_eq!(preview.len(), 1, "{}", snaps[2]);
        assert_eq!(preview[0]["type"], json!("line"));
        assert_eq!((preview[0]["a"]["x"].as_f64(), preview[0]["a"]["y"].as_f64()), (Some(0.0), Some(0.0)), "{}", preview[0]);
        assert_eq!((preview[0]["b"]["x"].as_f64(), preview[0]["b"]["y"].as_f64()), (Some(1000.0), Some(500.0)), "{}", preview[0]);
        assert_eq!(snaps[4]["active"], Value::Null, "{}", snaps[4]);
        assert!(lines(&v).iter().any(|l| close(*l, [0.0, 0.0, 1000.0, 0.0])), "{:?}", lines(&v));
        Ok(())
    }

    #[test]
    fn clicks_snap_to_an_endpoint_unless_no_snap() -> R {
        let more = format!("{DRAW}[\"command\",\"LINE\"]\n[\"click\",1003,2]\n[\"click\",2000,2000]\n[\"enter\"]\n");
        let (code, v, err) = go(&[], &more);
        assert_eq!(code, 0, "{err}");
        assert!(!v["snapshots"][6]["snap"].is_null(), "{}", v["snapshots"][6]);
        assert!(lines(&v).iter().any(|l| close(*l, [1000.0, 0.0, 2000.0, 2000.0])), "{:?}", lines(&v));
        let (code, v, err) = go(&["--no-snap"], &more);
        assert_eq!(code, 0, "{err}");
        assert!(v["snapshots"][6]["snap"].is_null());
        assert!(lines(&v).iter().any(|l| close(*l, [1003.0, 2.0, 2000.0, 2000.0])), "{:?}", lines(&v));
        Ok(())
    }

    #[test]
    fn ortho_squares_the_second_point() -> R {
        let (code, v, err) = go(&[], "[\"ortho\",true]\n[\"command\",\"LINE\"]\n[\"click\",0,0]\n[\"click\",700,40]\n[\"enter\"]\n");
        assert_eq!(code, 0, "{err}");
        assert!(lines(&v).iter().any(|l| close(*l, [0.0, 0.0, 700.0, 0.0])), "{:?}", lines(&v));
        Ok(())
    }

    #[test]
    fn esc_ends_the_command() {
        let (code, v, err) = go(&[], "[\"command\",\"LINE\"]\n[\"click\",0,0]\n[\"esc\"]\n");
        assert_eq!(code, 0, "{err}");
        assert_eq!(v["snapshots"][1]["active"], json!("line"));
        assert_eq!(v["snapshots"][2]["active"], Value::Null);
    }

    #[test]
    fn final_and_query() {
        let (code, v, _) = go(&["--final"], DRAW);
        assert_eq!(code, 0);
        assert_eq!(v["snapshots"].as_array().map(Vec::len), Some(1));
        let (code, v, _) = go(&["--query", "applied"], DRAW);
        assert_eq!(code, 0);
        let keys: Vec<&String> = v.as_object().map(|m| m.keys().collect()).unwrap_or_default();
        assert_eq!(keys, ["applied"]);
        assert_eq!(v["applied"].as_array().map(Vec::len), Some(5));
    }

    #[test]
    fn a_failed_record_exits_2_with_its_error() {
        let (code, v, _) = go(&[], "[\"undo\"]\n");
        assert_eq!(code, 2);
        assert_eq!(v["applied"][0]["ok"], json!(false));
        assert!(v["snapshots"][0]["error"].is_string());
    }

    #[test]
    fn bad_verbs_args_and_flags_exit_1() {
        for (args, stdin) in [
            (&[][..], "[\"frobnicate\"]\n"),
            (&[][..], "[\"click\",1]\n"),
            (&[][..], "[\"select\",\"zz\"]\n"),
            (&[][..], "{\"command\":\"line\"}\n"),
            (&["--scale", "0"][..], ""),
            (&["--nope"][..], ""),
        ] {
            let (code, v, err) = go(args, stdin);
            assert_eq!(code, 1, "{args:?} {stdin:?}");
            assert!(v.is_null() && !err.is_empty(), "{err}");
        }
    }
}

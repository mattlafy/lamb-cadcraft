//! One assistant turn run from a shell, no GUI (TS `tools/cli/llm.ts`):
//!
//! ```text
//! cargo run -p lamb-cadcraft --example lamb_assistant -- [--base-url U] [--model M]
//!     (--list | --ping "text" | [FILE] "request" [--dry-run] [--save OUT.dxf])
//! ```
//!
//! - `--list` prints the server's model ids, one per line.
//! - `--ping "text"` sends one user message and prints the reply.
//! - Otherwise one turn on FILE (`.dxf` opened with OPEN, `.lamb.json` imported as a LAMB project)
//!   or, with no FILE, a fresh LAMB project. Each model call is logged to stderr as
//!   `[ask n] k messages, last: …` and `[reply n] <ms>ms <bytes> bytes`; the outcome is printed to
//!   stdout as one line of JSON. `--dry-run` previews the script without applying it; `--save`
//!   writes the drawing as DXF after an applied turn.
//!
//! `--base-url` defaults to [`DEFAULT_BASE_URL`]; `--model` to the first model the server lists.
//! Exit 0 applied or prose, 2 failed, 1 any error (one line on stderr).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use cadcraft_engine::Session;
use serde_json::json;

use crate::assistant::{AskFn, Message, Step, Turn, TurnOutcome};
use crate::commands::install_app;
use crate::provider::{DEFAULT_BASE_URL, ProviderConfig, ask_fn, list_models};

const USAGE: &str = "usage: lamb_assistant [--base-url U] [--model M] (--list | --ping \"text\" | [FILE] \"request\" [--dry-run] [--save OUT.dxf])";

#[derive(Default)]
struct Opts {
    base_url: Option<String>,
    model: Option<String>,
    list: bool,
    ping: Option<String>,
    dry_run: bool,
    save: Option<String>,
    positional: Vec<String>,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value\n{USAGE}"));
        match a.as_str() {
            "--base-url" => o.base_url = Some(value("--base-url")?),
            "--model" => o.model = Some(value("--model")?),
            "--ping" => o.ping = Some(value("--ping")?),
            "--save" => o.save = Some(value("--save")?),
            "--list" => o.list = true,
            "--dry-run" => o.dry_run = true,
            f if f.starts_with("--") => return Err(format!("unknown flag {f}\n{USAGE}")),
            p => o.positional.push(p.to_owned()),
        }
    }
    Ok(o)
}

/// Runs the command line `args` (without the program name); returns the exit code. `ask: None`
/// talks to the server at `--base-url`; tests pass a stub.
pub fn run(args: &[String], ask: Option<&mut AskFn>, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    match run_inner(args, ask, out, err) {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(err, "{e}");
            1
        }
    }
}

fn run_inner(args: &[String], ask: Option<&mut AskFn>, out: &mut dyn Write, err: &mut dyn Write) -> Result<i32, String> {
    let o = parse(args)?;
    let base_url = o.base_url.clone().unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
    if o.list {
        for id in list_models(&base_url)? {
            writeln!(out, "{id}").map_err(|e| e.to_string())?;
        }
        return Ok(0);
    }
    let (file, request) = match (o.ping.is_some(), o.positional.as_slice()) {
        (true, []) => (None, String::new()),
        (false, [r]) => (None, r.clone()),
        (false, [f, r]) => (Some(f.clone()), r.clone()),
        _ => return Err(USAGE.to_owned()),
    };
    let mut owned: Box<AskFn>;
    let ask: &mut AskFn = match ask {
        Some(a) => a,
        None => {
            let model = match o.model.clone() {
                Some(m) => m,
                None => list_models(&base_url)?.into_iter().next().ok_or_else(|| format!("{base_url} lists no models"))?,
            };
            owned = Box::new(ask_fn(ProviderConfig { base_url, model, read_timeout: None }));
            &mut *owned
        }
    };
    if let Some(text) = &o.ping {
        let reply = ask(&[Message::new("user", text.as_str())])?;
        writeln!(out, "{reply}").map_err(|e| e.to_string())?;
        return Ok(0);
    }
    let mut s = load(file.as_deref())?;
    let outcome = turn(&mut s, &request, o.dry_run, ask, err)?;
    writeln!(out, "{}", serde_json::to_string(&outcome).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(match outcome {
        TurnOutcome::Applied { .. } => {
            if let Some(path) = &o.save {
                let doc = s.doc().map_err(|e| e.to_string())?;
                std::fs::write(path, cadcraft_io::write_dxf(doc)).map_err(|e| format!("{path}: {e}"))?;
            }
            0
        }
        TurnOutcome::NoScript { .. } => 0,
        TurnOutcome::Failed { .. } => 2,
    })
}

/// The session a turn runs in: FILE opened, or a fresh LAMB project.
pub(crate) fn load(file: Option<&str>) -> Result<Session, String> {
    install_app()?;
    let mut s = Session::new();
    match file {
        None => {
            s.new_drawing(true);
            s.execute("lambproject", &json!({})).map_err(|e| e.to_string())?;
        }
        Some(f) if f.ends_with(".lamb.json") => {
            let text = std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?;
            let files: BTreeMap<String, String> = serde_json::from_str(&text).map_err(|e| format!("{f}: {e}"))?;
            let project = lamb_core::persistence::files_to_project(&files).map_err(|e| format!("{f}: {e:?}"))?;
            let (d, _, _) = crate::import::project_to_drawing(&project).map_err(|e| format!("{f}: {e:?}"))?;
            let title = Path::new(f).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| f.to_owned());
            s.open_drawing(d, &title, None);
        }
        Some(f) if f.to_ascii_lowercase().ends_with(".dxf") => {
            s.execute("open", &json!({ "path": f })).map_err(|e| e.to_string())?;
        }
        Some(f) => return Err(format!("{f}: expected a .dxf or .lamb.json file")),
    }
    Ok(s)
}

/// One turn, each model call logged to `err` as it happens.
fn turn(s: &mut Session, request: &str, dry_run: bool, ask: &mut AskFn, err: &mut dyn Write) -> Result<TurnOutcome, String> {
    let (mut turn, mut messages) = Turn::start(s, request).map_err(|e| e.to_string())?;
    turn.set_dry_run(dry_run);
    for n in 1.. {
        let last: String = messages.last().map(|m| m.content.chars().take(120).collect::<String>().replace('\n', " ")).unwrap_or_default();
        let _ = writeln!(err, "[ask {n}] {} messages, last: {last}", messages.len());
        let t = Instant::now();
        let reply = ask(&messages)?;
        let _ = writeln!(err, "[reply {n}] {}ms {} bytes", t.elapsed().as_millis(), reply.len());
        match turn.resume(s, reply).map_err(|e| e.to_string())? {
            Step::Ask(m) => messages = m,
            Step::Done(o) => return Ok(o),
        }
    }
    Err("turn did not end".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::Arc;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;

    const GOOD: &str = "Here:\n```json\n{\"steps\":[{\"command\":\"line\",\"params\":{\"points\":[[0,0],[1000,0]]}}]}\n```";
    const BAD: &str = "Here:\n```json\n{\"steps\":[{\"command\":\"circle\",\"params\":{}}]}\n```";

    /// Runs `args` with replies served in order; returns (exit, stdout, stderr).
    fn go(args: &[&str], replies: &[&str]) -> (i32, String, String) {
        let mut q = replies.iter().map(|r| (*r).to_owned());
        let mut ask = |_: &[Message]| q.next().ok_or_else(|| "no more replies".to_owned());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let code = run(&args, Some(&mut ask), &mut out, &mut err);
        (code, String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned())
    }

    fn outcome(out: &str) -> std::result::Result<Value, serde_json::Error> {
        serde_json::from_str(out.trim())
    }

    #[test]
    fn a_script_reply_applies() -> R {
        let (code, out, err) = go(&["draw a line"], &[GOOD]);
        assert_eq!(code, 0, "{err}");
        let v = outcome(&out)?;
        assert_eq!((v["kind"].clone(), v["attempts"].clone()), (json!("applied"), json!(1)), "{out}");
        assert!(err.contains("[ask 1]") && err.contains("[reply 1]"), "{err}");
        Ok(())
    }

    #[test]
    fn a_bad_then_good_pair_retries() -> R {
        let (code, out, err) = go(&["draw a line"], &[BAD, GOOD]);
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("[ask 2]"), "{err}");
        assert_eq!(outcome(&out)?["attempts"], json!(2), "{out}");
        Ok(())
    }

    #[test]
    fn prose_is_no_script() -> R {
        let (code, out, _) = go(&["what is this?"], &["A plan."]);
        assert_eq!(code, 0);
        assert_eq!(outcome(&out)?["kind"], json!("noScript"), "{out}");
        Ok(())
    }

    #[test]
    fn three_bad_scripts_fail_with_exit_2() -> R {
        let (code, out, _) = go(&["draw"], &[BAD, BAD, BAD]);
        assert_eq!(code, 2);
        assert_eq!(outcome(&out)?["kind"], json!("failed"), "{out}");
        Ok(())
    }

    #[test]
    fn a_dry_run_applies_nothing() -> R {
        let (code, out, err) = go(&["--dry-run", "draw a line"], &[GOOD]);
        assert_eq!(code, 0, "{err}");
        let v = outcome(&out)?;
        assert_eq!((v["kind"].clone(), v["result"]["dryRun"].clone()), (json!("applied"), json!(true)), "{out}");
        let mut s = load(None)?;
        let snapshot = Arc::clone(&s.state()?.doc);
        let mut q = std::iter::once(GOOD.to_owned());
        let mut ask = |_: &[Message]| q.next().ok_or_else(|| "no more replies".to_owned());
        let o = turn(&mut s, "draw a line", true, &mut ask, &mut Vec::new())?;
        assert!(matches!(o, TurnOutcome::Applied { .. }), "{o:?}");
        assert!(Arc::ptr_eq(&s.state()?.doc, &snapshot), "drawing unchanged");
        Ok(())
    }

    #[test]
    fn save_writes_a_readable_dxf() -> R {
        let path = std::env::temp_dir().join(format!("lamb_assistant_save_{}.dxf", std::process::id()));
        let p = path.to_string_lossy().into_owned();
        let (code, _, err) = go(&["--save", &p, "draw a line"], &[GOOD]);
        assert_eq!(code, 0, "{err}");
        let back = cadcraft_io::read_dxf(&std::fs::read(&path)?);
        let _ = std::fs::remove_file(&path);
        assert!(back.is_ok(), "{:?}", back.err());
        Ok(())
    }

    #[test]
    fn list_against_a_closed_port_is_one_error_line() {
        let (code, out, err) = go(&["--base-url", "http://127.0.0.1:9/v1", "--list"], &[]);
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert_eq!(err.trim_end().lines().count(), 1, "{err}");
    }

    #[test]
    fn bad_args_print_usage_and_exit_1() {
        for args in [&["--frobnicate", "x"][..], &[][..], &["a", "b", "c"][..]] {
            let (code, _, err) = go(args, &[]);
            assert_eq!(code, 1, "{args:?}");
            assert!(err.contains("usage:"), "{err}");
        }
    }

    #[test]
    fn ping_prints_the_reply() {
        let (code, out, _) = go(&["--ping", "hello"], &["hi there"]);
        assert_eq!((code, out.trim()), (0, "hi there"));
    }
}

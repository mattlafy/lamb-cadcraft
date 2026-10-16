//! Structural notes on an assistant script (TS `packages/command-engine/src/lint.ts`, the codes
//! that carry over to handle-based `{command, params}` steps). Notes never decide pass/fail —
//! `assistant.apply` does — they ride along on the retry message so the model can fix what is
//! actually wrong. Pure: reads the drawing, runs nothing.
//!
//! - `unknown-command`: `command` missing, or not a registered command.
//! - `empty-target`: a `handles` param that is an empty array, so the step is a no-op.
//! - `invented-handle`: a string or number under a `handle` / `handles` key, at any depth, that is
//!   not an entity in the drawing (or a string that is not hex at all).
//! - `unknown-param`: a top-level `params` key the command's `params` signature does not mention.
//!   A signature that yields no keys gives no notes.

use cadcraft_doc::{Drawing, Handle};
use cadcraft_engine::find_command;
use serde_json::Value;

/// One note: its code, the 1-based step it is about, and a sentence for the model.
#[derive(Debug, Clone, PartialEq)]
pub struct LintNote {
    pub code: &'static str,
    /// 1-based.
    pub step: usize,
    pub message: String,
}

/// How deep [`lint`] looks for `handle` / `handles` keys.
const MAX_DEPTH: usize = 12;

/// Notes on `steps` against `d` as it is now.
pub fn lint(d: &Drawing, steps: &[Value]) -> Vec<LintNote> {
    let mut out = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let n = i + 1;
        let mut note = |code, message: String| out.push(LintNote { code, step: n, message });
        let params = step.get("params");
        let spec = match step.get("command").and_then(Value::as_str) {
            None => {
                note("unknown-command", "the step has no `command` string".to_owned());
                None
            }
            Some(id) => {
                let spec = find_command(id);
                if spec.is_none() {
                    note("unknown-command", format!("`{id}` is not a command; use only the ids in the command list"));
                }
                spec
            }
        };
        if params.and_then(|p| p.get("handles")).and_then(Value::as_array).is_some_and(Vec::is_empty) {
            note("empty-target", "`handles` is an empty array, so this step is always a no-op".to_owned());
        }
        if let Some(p) = params {
            let mut refs = Vec::new();
            walk_handles(p, 0, &mut refs);
            for r in refs {
                if let Some(m) = invented(d, r) {
                    note("invented-handle", m);
                }
            }
        }
        if let (Some(spec), Some(Value::Object(p))) = (spec, params) {
            let keys = signature_keys(spec.params);
            if !keys.is_empty() {
                for k in p.keys().filter(|k| !keys.iter().any(|s| s == *k)) {
                    note("unknown-param", format!("`{k}` is not a parameter of {} (takes: {})", spec.id, keys.join(", ")));
                }
            }
        }
    }
    out
}

/// Every string or number held by a `handle` / `handles` key under `v` (directly or in an array).
fn walk_handles<'a>(v: &'a Value, depth: usize, out: &mut Vec<&'a Value>) {
    if depth > MAX_DEPTH {
        return;
    }
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if k == "handle" || k == "handles" {
                    match x {
                        Value::Array(a) => out.extend(a.iter().filter(|e| e.is_string() || e.is_number())),
                        Value::String(_) | Value::Number(_) => out.push(x),
                        _ => {}
                    }
                }
                walk_handles(x, depth + 1, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| walk_handles(x, depth + 1, out)),
        _ => {}
    }
}

/// The note for one handle reference, or `None` when it names an entity in `d`.
fn invented(d: &Drawing, r: &Value) -> Option<String> {
    let (shown, h) = match r {
        Value::String(t) => {
            let hex = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
            match u64::from_str_radix(hex, 16) {
                Ok(h) => (t.clone(), h),
                Err(_) => return Some(format!("`{t}` is not a hex handle; name entities only by handles `query` returned")),
            }
        }
        Value::Number(n) => (n.to_string(), n.as_u64()?),
        _ => return None,
    };
    if d.entity(Handle(h)).is_some() {
        None
    } else {
        Some(format!("handle `{shown}` is not an entity in the drawing; name entities only by handles `query` returned"))
    }
}

/// The parameter names a `CommandSpec.params` signature mentions: identifiers at brace depth 1,
/// outside `[...]` and `(...)`, that start the text after `{`, `,`, `|` or `+` (a trailing `?`
/// stripped). `"{handles?, from?: [x,y], to?: [x,y] | delta: [dx,dy]}"` → handles, from, to, delta.
pub fn signature_keys(sig: &str) -> Vec<String> {
    let (mut brace, mut nest) = (0usize, 0usize);
    let mut at_start = false;
    let mut keys: Vec<String> = Vec::new();
    let mut chars = sig.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                brace += 1;
                at_start = brace == 1 && nest == 0;
            }
            '}' => {
                brace = brace.saturating_sub(1);
                at_start = false;
            }
            '[' | '(' => {
                nest += 1;
                at_start = false;
            }
            ']' | ')' => {
                nest = nest.saturating_sub(1);
                at_start = false;
            }
            ',' | '|' | '+' if brace == 1 && nest == 0 => at_start = true,
            c if c.is_whitespace() => {}
            c if at_start && (c.is_ascii_alphabetic() || c == '_') => {
                let mut id = String::from(c);
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_alphanumeric() || n == '_' {
                        id.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if !keys.contains(&id) {
                    keys.push(id);
                }
                at_start = false;
            }
            _ => at_start = false,
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::install_app;
    use cadcraft_engine::Session;
    use serde_json::json;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;

    /// A drawing with one line, and that line's handle.
    fn drawing() -> std::result::Result<(Session, Handle), Box<dyn std::error::Error>> {
        install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        s.execute("line", &json!({ "points": [[0, 0], [1000, 0]] }))?;
        let h = s.doc()?.model.iter().map(|e| e.handle).last().ok_or("no line")?;
        Ok((s, h))
    }

    fn codes(n: &[LintNote]) -> Vec<(&'static str, usize)> {
        n.iter().map(|n| (n.code, n.step)).collect()
    }

    #[test]
    fn signature_keys_parse() {
        assert_eq!(signature_keys("{handles?, from?: [x,y], to?: [x,y] | delta: [dx,dy]}"), ["handles", "from", "to", "delta"]);
        assert_eq!(signature_keys("{handles?: [hex]} (default: selection)"), ["handles"]);
        assert_eq!(
            signature_keys("{center, radius} | {center, diameter} | {p1, p2} | {p1, p2, p3}"),
            ["center", "radius", "diameter", "p1", "p2", "p3"]
        );
        assert_eq!(signature_keys("{vertices: [[x,y] | {p:[x,y], bulge}], closed?}"), ["vertices", "closed"]);
        assert!(signature_keys("").is_empty());
        assert!(signature_keys("{}").is_empty());
    }

    #[test]
    fn a_clean_script_on_a_real_handle_has_no_notes() -> R {
        let (s, h) = drawing()?;
        let steps = [
            json!({ "command": "move", "params": { "handles": [h.hex()], "delta": [0, 50] } }),
            json!({ "command": "move", "params": { "handles": [h.0], "from": [0, 0], "to": [1, 1] } }),
            json!({ "command": "circle", "params": { "center": [0, 0], "radius": 5 } }),
        ];
        assert_eq!(lint(s.doc()?, &steps), vec![]);
        Ok(())
    }

    #[test]
    fn each_code_fires() -> R {
        let (s, h) = drawing()?;
        let d = s.doc()?;
        let n = lint(d, &[json!({ "command": "move", "params": { "handles": [], "delta": [1, 0] } })]);
        assert_eq!(codes(&n), [("empty-target", 1)], "{n:?}");

        let n = lint(
            d,
            &[
                json!({ "command": "line", "params": { "points": [[0, 0], [1, 0]] } }),
                json!({ "command": "move", "params": { "handles": ["ffff", h.hex()], "delta": [1, 0] } }),
            ],
        );
        assert_eq!(codes(&n), [("invented-handle", 2)], "{n:?}");
        assert!(n[0].message.contains("`ffff` is not an entity"), "{n:?}");

        let n = lint(d, &[json!({ "command": "move", "params": { "handles": ["zz"], "delta": [1, 0] } })]);
        assert_eq!(codes(&n), [("invented-handle", 1)]);
        assert!(n[0].message.contains("`zz` is not a hex handle"), "{n:?}");

        let n = lint(d, &[json!({ "command": "x", "params": { "nested": [{ "handle": 99999 }] } })]);
        assert_eq!(codes(&n), [("unknown-command", 1), ("invented-handle", 1)], "deep handles too: {n:?}");

        let n = lint(d, &[json!({ "command": "circle", "params": { "center": [0, 0], "radius": 5, "colour": 1 } })]);
        assert_eq!(codes(&n), [("unknown-param", 1)]);
        assert!(n[0].message.contains("`colour` is not a parameter of circle (takes: center, radius"), "{n:?}");

        let n = lint(d, &[json!({ "command": "nosuchcommand", "params": {} }), json!({ "params": {} })]);
        assert_eq!(codes(&n), [("unknown-command", 1), ("unknown-command", 2)]);
        assert!(n[0].message.contains("nosuchcommand"));
        Ok(())
    }
}

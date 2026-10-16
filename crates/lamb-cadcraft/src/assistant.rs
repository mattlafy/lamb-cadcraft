//! The assistant's script, applied (TS `packages/command-engine/src/assistantTurn.ts`
//! `runAssistantTurn`, without the model call or the retry loop).
//!
//! - [`extract_last_script`]: the last ```` ```json ```` fenced block of a model reply (or a bare
//!   ```` ``` ```` fence whose body parses as JSON), parsed.
//! - `assistant.apply {steps: [{command, params}], proposalId?}` (no menu): runs every step's
//!   command `run` directly, in order, as **one** undo step. Any failing step puts the drawing back
//!   as it was and errors with `step <i> (<id>): <message>` (1-based), so nothing is recorded.
//!   On success every entity in a LAMB view block that the script created or changed is stamped
//!   `ai_origin { producer: assistant, accepted: false, proposal_id }`, which ACCEPT takes.
//!   Entities outside view blocks are not stamped: ACCEPT and the rest of LAMB only read view
//!   lines. `proposalId` defaults to `a<n>`, one past the highest `a<n>` already in the drawing.
//!   Returns `{proposalId, stamped: [handle], created, modified}`.
//!
//! Steps that are not undoable (UNDO, REDO, EDITVIEW...) and a nested `assistant.apply` are refused.
//! The after-command observers (link sweep, assembly regeneration) run once, after the whole
//! script, so their side effects are not stamped.

use std::collections::BTreeMap;
use std::sync::Arc;

use cadcraft_doc::{Drawing, Entity, Handle};
use cadcraft_engine::{EngineError, Result, Session, find_command};
use lamb_core::model::{AiOrigin, AiProducer};
use serde_json::{Value, json};

use crate::commands::{bad, other};
use crate::schema::{read_entity_data, set_entity_data};

pub const ID: &str = "assistant.apply";

/// The last fenced JSON script in `reply`, or `None` when there is none or it does not parse.
pub fn extract_last_script(reply: &str) -> Option<Value> {
    let mut blocks: Vec<(String, String)> = Vec::new();
    let mut open: Option<(String, Vec<&str>)> = None;
    for line in reply.lines() {
        let t = line.trim_start();
        match open.take() {
            None => {
                if let Some(info) = t.strip_prefix("```") {
                    open = Some((info.trim().to_ascii_lowercase(), Vec::new()));
                }
            }
            Some((info, mut body)) => {
                if t.trim_end() == "```" {
                    blocks.push((info, body.join("\n")));
                } else {
                    body.push(line);
                    open = Some((info, body));
                }
            }
        }
    }
    let (_, body) =
        blocks.into_iter().rev().find(|(info, body)| info == "json" || (info.is_empty() && serde_json::from_str::<Value>(body).is_ok()))?;
    serde_json::from_str(&body).ok()
}

/// A script's steps: `{steps: [...]}` or a bare array.
pub fn script_steps(script: &Value) -> Option<&Vec<Value>> {
    match script {
        Value::Array(a) => Some(a),
        Value::Object(_) => script.get("steps")?.as_array(),
        _ => None,
    }
}

/// Every entity in a LAMB view block, by handle.
fn view_entities(d: &Drawing) -> BTreeMap<Handle, Arc<Entity>> {
    d.blocks
        .iter()
        .filter(|(name, _)| name.strip_prefix("LAMB_VIEW_").is_some_and(|n| n.parse::<u64>().is_ok()))
        .flat_map(|(_, b)| b.entities.iter().map(|e| (e.handle, e.clone())))
        .collect()
}

/// `a<n>` one past the highest `a<n>` proposal id in the drawing.
fn fresh_proposal_id(d: &Drawing) -> String {
    let n = view_entities(d)
        .values()
        .filter_map(|e| read_entity_data(&e.common).ok()?.ai_origin)
        .filter_map(|o| o.proposal_id.strip_prefix('a')?.parse::<u64>().ok())
        .max()
        .map_or(1, |n| n + 1);
    format!("a{n}")
}

fn run_steps(s: &mut Session, steps: &[Value]) -> Result<()> {
    for (i, step) in steps.iter().enumerate() {
        let n = i + 1;
        let id = step.get("command").and_then(Value::as_str).ok_or_else(|| bad(ID, &format!("step {n}: `command` must be a string")))?;
        let fail = |m: String| other(format!("step {n} ({id}): {m}"));
        let spec = find_command(id).ok_or_else(|| fail("unknown command".into()))?;
        if spec.id == ID {
            return Err(fail("an assistant script cannot apply another".into()));
        }
        if !spec.undoable {
            return Err(fail("not available in an assistant script".into()));
        }
        (spec.enabled)(s).map_err(fail)?;
        let params = step.get("params").cloned().unwrap_or_else(|| json!({}));
        (spec.run)(s, &params).map_err(|e| fail(e.to_string()))?;
    }
    Ok(())
}

pub(crate) fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    let steps = script_steps(p).ok_or_else(|| bad(ID, "`steps` must be an array of {command, params}"))?.clone();
    let proposal_id = match p.get("proposalId") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) if !id.is_empty() => Some(id.clone()),
        Some(_) => return Err(bad(ID, "`proposalId` must be a non-empty string")),
    };
    // The same `Arc` goes back on failure, so the engine sees an untouched drawing: no undo step.
    let snapshot = Arc::clone(&s.state()?.doc);
    let selection = s.selection();
    if let Err(e) = run_steps(s, &steps) {
        s.state_mut()?.doc = snapshot;
        s.set_selection(selection);
        return Err(e);
    }

    let before = view_entities(&snapshot);
    let after = view_entities(s.doc()?);
    let (mut created, mut modified) = (0, 0);
    let mut stamped = Vec::new();
    for (h, e) in &after {
        match before.get(h) {
            None => created += 1,
            Some(old) if !Arc::ptr_eq(old, e) && **old != **e => modified += 1,
            Some(_) => continue,
        }
        stamped.push(*h);
    }
    let proposal_id = proposal_id.unwrap_or_else(|| fresh_proposal_id(&snapshot));
    for h in &stamped {
        let mut res = Ok(());
        s.doc_mut()?.modify_entity(*h, |e| {
            res = read_entity_data(&e.common).and_then(|mut d| {
                d.ai_origin = Some(AiOrigin {
                    proposal_id: proposal_id.clone(),
                    accepted: false,
                    assumption_note: None,
                    source_entity_id: None,
                    swept_geometry: None,
                    producer: Some(AiProducer::Assistant),
                });
                set_entity_data(&mut e.common, &d)
            });
        })?;
        res.map_err(|e| EngineError::Other(e.to_string()))?;
    }
    Ok(json!({
        "proposalId": proposal_id,
        "stamped": stamped.iter().map(|h| h.0).collect::<Vec<_>>(),
        "created": created,
        "modified": modified,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::install_app;
    use std::collections::HashSet;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type E<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    /// A LAMB project with its plan open for editing.
    fn session() -> E<Session> {
        install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        s.execute("lambproject", &json!({}))?;
        Ok(s)
    }

    fn handles(s: &Session) -> E<HashSet<Handle>> {
        Ok(view_entities(s.doc()?).into_keys().collect())
    }

    fn origin(s: &Session, h: Handle) -> E<Option<AiOrigin>> {
        Ok(read_entity_data(&s.doc()?.entity(h).ok_or("no entity")?.common)?.ai_origin)
    }

    fn undo_len(s: &Session) -> E<usize> {
        Ok(s.state()?.undo.len())
    }

    fn stamped(r: &Value) -> Vec<Handle> {
        r["stamped"].as_array().map(|a| a.iter().filter_map(Value::as_u64).map(Handle).collect()).unwrap_or_default()
    }

    #[test]
    fn a_two_step_script_is_one_undo_step_and_stamps_both() -> R {
        let mut s = session()?;
        let (n, before) = (undo_len(&s)?, handles(&s)?);
        let r = s.execute(
            ID,
            &json!({ "steps": [
                { "command": "line", "params": { "points": [[0, 0], [1000, 0]] } },
                { "command": "circle", "params": { "center": [500, 500], "radius": 100 } },
            ] }),
        )?;
        assert_eq!((r["created"].clone(), r["modified"].clone(), r["proposalId"].clone()), (json!(2), json!(0), json!("a1")), "{r}");
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        let new: Vec<Handle> = handles(&s)?.difference(&before).copied().collect();
        assert_eq!(new.len(), 2);
        let mut got = stamped(&r);
        got.sort();
        let mut want = new.clone();
        want.sort();
        assert_eq!(got, want);
        for h in &new {
            let o = origin(&s, *h)?.ok_or("unstamped")?;
            assert_eq!((o.proposal_id.as_str(), o.accepted, o.producer), ("a1", false, Some(AiProducer::Assistant)));
        }
        let r2 = s.execute(ID, &json!([{ "command": "line", "params": { "points": [[0, 900], [10, 900]] } }]))?;
        assert_eq!(r2["proposalId"], json!("a2"), "a bare array, and a fresh id");
        s.undo()?;
        s.undo()?;
        assert_eq!(handles(&s)?, before, "undo removes both");
        Ok(())
    }

    #[test]
    fn a_failing_step_leaves_the_drawing_and_the_undo_stack_alone() -> R {
        let mut s = session()?;
        let (n, before) = (undo_len(&s)?, s.doc()?.clone());
        let e = s
            .execute(
                ID,
                &json!({ "steps": [
                    { "command": "line", "params": { "points": [[0, 0], [1000, 0]] } },
                    { "command": "circle", "params": {} },
                ] }),
            )
            .err()
            .ok_or("should fail")?
            .to_string();
        assert!(e.contains("step 2 (circle)"), "{e}");
        assert!(*s.doc()? == before, "drawing as before");
        assert_eq!(undo_len(&s)?, n);
        Ok(())
    }

    #[test]
    fn an_edited_line_is_stamped_modified() -> R {
        let mut s = session()?;
        let before = handles(&s)?;
        s.execute("line", &json!({ "points": [[0, 0], [1000, 0]] }))?;
        let h = *handles(&s)?.difference(&before).next().ok_or("no line")?;
        assert!(origin(&s, h)?.is_none());
        let r =
            s.execute(ID, &json!({ "steps": [{ "command": "move", "params": { "handles": [h.hex()], "delta": [0, 50] } }], "proposalId": "p7" }))?;
        assert_eq!((r["created"].clone(), r["modified"].clone()), (json!(0), json!(1)), "{r}");
        assert_eq!(stamped(&r), vec![h]);
        assert_eq!(origin(&s, h)?.ok_or("unstamped")?.proposal_id, "p7");
        Ok(())
    }

    #[test]
    fn unknown_nested_and_non_undoable_steps_are_refused() -> R {
        let mut s = session()?;
        let n = undo_len(&s)?;
        for (cmd, why) in [("nosuchcommand", "unknown command"), (ID, "cannot apply another"), ("undo", "not available")] {
            let e = s.execute(ID, &json!({ "steps": [{ "command": cmd, "params": {} }] })).err().ok_or(cmd)?.to_string();
            assert!(e.contains(&format!("step 1 ({cmd})")) && e.contains(why), "{e}");
        }
        assert_eq!(undo_len(&s)?, n);
        Ok(())
    }

    #[test]
    fn accept_takes_a_stamped_line() -> R {
        let mut s = session()?;
        let r = s.execute(ID, &json!({ "steps": [{ "command": "line", "params": { "points": [[0, 0], [1000, 0]] } }] }))?;
        let h = *stamped(&r).first().ok_or("nothing stamped")?;
        let a = s.execute("accept", &json!({ "handles": [h.hex()] }))?;
        assert_eq!(a["accepted"], json!(1), "{a}");
        assert!(origin(&s, h)?.ok_or("unstamped")?.accepted);
        Ok(())
    }

    #[test]
    fn extractor_takes_the_last_json_fence() {
        let two = "first\n```json\n{\"steps\":[1]}\n```\nthen\n```json\n{\"steps\":[2]}\n```\n";
        assert_eq!(extract_last_script(two), Some(json!({ "steps": [2] })));
        assert_eq!(extract_last_script("no script here, just prose"), None);
        assert_eq!(extract_last_script("```json\n{\"steps\": [\n```"), None, "broken JSON");
        let bare = "here:\n```\n[{\"command\":\"line\",\"params\":{}}]\n```";
        let v = extract_last_script(bare);
        assert_eq!(v, Some(json!([{ "command": "line", "params": {} }])));
        assert_eq!(v.as_ref().and_then(script_steps).map(Vec::len), Some(1));
        assert_eq!(extract_last_script("```\nnot json\n```"), None, "a bare fence of prose is no script");
    }
}

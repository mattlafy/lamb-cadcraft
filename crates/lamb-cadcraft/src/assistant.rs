//! The assistant's script, applied (TS `packages/command-engine/src/assistantTurn.ts`
//! `runAssistantTurn`, without the check call; the model call is the caller's `ask`).
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
//!
//! The prompt (TS `packages/command-engine/src/assistantPrompt.ts` `assembleAssistantPrompt`):
//! [`prompt`] returns three [`Message`]s — [`role`] + [`catalogue`] (system, byte-identical across
//! drawings: the cache prefix), [`header`] (system, per turn: views, layers, counts, active view,
//! current layer, selection), then the user's text. `assistant.prompt {message}` (no menu, not an
//! undo step) returns them as `{messages: [{role, content}]}`.
//!
//! The turn loop: [`run_turn`] asks through a caller-given [`AskFn`] (no provider here), then
//! loops on the reply. No script → [`TurnOutcome::NoScript`]. A script of only `query`/`showlinks`
//! is a read round (≤ [`MAX_READ_ROUNDS`], not counted as attempts): each step runs through
//! `execute`, the values go back as `Query result(s)…` and the model is re-asked. Any other script
//! goes through `execute("assistant.apply")` — one undo step. A script with no steps array
//! (Parse) or one that fails to apply (Apply) is retried, at most [`MAX_ATTEMPTS`] scripts in all,
//! the re-ask carrying only that failing reply and its error. A retry with no script, or the
//! attempts or read rounds spent, ends as [`TurnOutcome::Failed`].

use std::collections::BTreeMap;
use std::sync::Arc;

use cadcraft_doc::{Drawing, Entity, Handle, entity_bounds};
use cadcraft_engine::cmd::command_specs;
use cadcraft_engine::{EngineError, Result, Session, find_command};
use lamb_core::model::{AiOrigin, AiProducer};
use serde::Serialize;
use serde_json::{Value, json};

use crate::commands::{bad, edited_view, other};
use crate::project::DrawingProject;
use crate::schema::{EXT_KEY, read_entity_data, set_entity_data};

pub const ID: &str = "assistant.apply";
pub const PROMPT_ID: &str = "assistant.prompt";

/// How many read-only rounds one turn may take before it must answer or edit.
pub const MAX_READ_ROUNDS: usize = 2;

/// The commands a script may run that change nothing; a script of only these is a read round.
pub const READ_ONLY: [&str; 2] = ["query", "showlinks"];

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

/// One chat message; `role` is `"system"`, `"user"` or `"assistant"`.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Message { role: role.to_owned(), content: content.into() }
    }
}

/// System message #1, first part: who the assistant is and the script contract. Stable text.
pub fn role() -> String {
    let lines = [
        "You are the drafting assistant built into LAMB, an architectural CAD application running on CADCraft.".to_owned(),
        String::new(),
        "A reply in plain prose is a complete reply: answer questions in prose and change nothing.".to_owned(),
        "All lengths are millimetres.".to_owned(),
        String::new(),
        "To change the drawing, end your reply with ONE ```json fenced block holding a script:".to_owned(),
        r#"{"steps":[{"command": "<command id>", "params": {...}}]}"#.to_owned(),
        "The script is applied as one undo step. If any step fails, the whole script fails and nothing changes.".to_owned(),
        "Use only the commands listed below, with the params shown for each.".to_owned(),
        String::new(),
        "Rules:".to_owned(),
        "- Never name an existing entity by an id that did not come back from `query`. Write ids exactly as query prints them.".to_owned(),
        "- Never edit an entity on a LOCKED layer.".to_owned(),
        "- Answer questions about what is in the drawing from `query`, not from the header's entity counts.".to_owned(),
        format!(
            "- A script whose steps are all read-only ({}) is a read round: its results come back to you in the next message, \
             and you then answer or edit. At most {MAX_READ_ROUNDS} read rounds per turn.",
            READ_ONLY.join(", ")
        ),
        "- Read-only steps cannot be mixed with edits in one script.".to_owned(),
        String::new(),
        "Commands (id params):".to_owned(),
    ];
    lines.join("\n")
}

/// System message #1, second part: one `id params` line per scriptable command, sorted by id.
pub fn catalogue() -> String {
    let mut rows: Vec<(&str, String)> = command_specs()
        .iter()
        .filter(|c| c.id != ID && c.id != PROMPT_ID)
        .filter_map(|c| {
            if READ_ONLY.contains(&c.id) {
                Some((c.id, format!("{} {} (read-only)", c.id, c.params)))
            } else if c.undoable && !c.params.is_empty() {
                Some((c.id, format!("{} {}", c.id, c.params)))
            } else {
                None
            }
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    rows.dedup_by(|a, b| a.0 == b.0);
    rows.into_iter().map(|(_, l)| l).collect::<Vec<_>>().join("\n")
}

/// ` "name"` when a view or layer's name differs from its id, else nothing.
fn name_suffix(id: &str, name: &str) -> String {
    if id == name { String::new() } else { format!(" \"{name}\"") }
}

fn text(v: &Value) -> String {
    match v {
        Value::String(t) => t.clone(),
        Value::Null => String::new(),
        v => v.to_string(),
    }
}

/// System message #2: the drawing as it is this turn. A plain CADCraft drawing (no LAMB document)
/// gets only the active view, current layer and selection lines.
pub fn header(s: &Session) -> Result<String> {
    let d = s.doc()?;
    let mut lines: Vec<String> = Vec::new();
    if d.ext.contains_key(EXT_KEY) {
        let dp = DrawingProject::new(d).map_err(other)?;
        let ix = crate::querycmds::index(&dp);
        let mut section = |title: &str, key: &str, line: &dyn Fn(&Value) -> String| {
            lines.push(title.to_owned());
            let rows = ix[key].as_array().cloned().unwrap_or_default();
            if rows.is_empty() {
                lines.push("  (none)".to_owned());
            }
            lines.extend(rows.iter().map(|r| format!("  {}", line(r))));
        };
        section("Views:", "views", &|v| {
            let id = text(&v["id"]);
            format!("{id}{} ({})", name_suffix(&id, &text(&v["name"])), text(&v["kind"]))
        });
        section("Layers:", "layers", &|l| {
            let id = text(&l["id"]);
            let locked = if l["locked"].as_bool() == Some(true) { " LOCKED" } else { "" };
            format!("{id}{} role={}{locked}", name_suffix(&id, &text(&l["name"])), text(&l["defaultRole"]))
        });
        section("Entity counts (view/layer):", "counts", &|c| format!("{}/{}: {}", text(&c["viewId"]), text(&c["layerId"]), text(&c["n"])));
    }
    lines.push(format!("Active view: {}", edited_view(s).map_or_else(|| "(none)".to_owned(), |id| id.0.to_string())));
    lines.push(format!("Current layer: {}", d.header.str("CLAYER", "0")));
    let sel = s.selection();
    lines.push(format!("Selection: {} entities", sel.len()));
    for h in sel {
        let Some(e) = d.entity(h) else {
            lines.push(format!("  {} (not found)", h.hex()));
            continue;
        };
        let b = entity_bounds(d, e, 0);
        let bbox = if b.is_empty() { String::new() } else { format!(", bbox [{},{}]-[{},{}]", b.min.x, b.min.y, b.max.x, b.max.y) };
        lines.push(format!("  {} ({}{bbox})", h.hex(), e.kind.type_name()));
    }
    Ok(lines.join("\n"))
}

/// The three messages of one turn: role + catalogue, header, the user's text.
pub fn prompt(s: &Session, user: &str) -> Result<Vec<Message>> {
    Ok(vec![Message::new("system", format!("{}\n\n{}", role(), catalogue())), Message::new("system", header(s)?), Message::new("user", user)])
}

pub(crate) fn prompt_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let message = p.get("message").and_then(Value::as_str).ok_or_else(|| bad(PROMPT_ID, "`message` must be a string"))?;
    Ok(json!({ "messages": prompt(s, message)? }))
}

/// How many scripts one turn may try (the first and two retries).
pub const MAX_ATTEMPTS: usize = 3;

/// Where a failed script failed: its shape, or applying it.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Stage {
    Parse,
    Apply,
}

/// How one assistant turn ended.
#[derive(Debug, PartialEq)]
pub enum TurnOutcome {
    /// The reply held no script: a prose answer, nothing changed.
    NoScript { text: String },
    /// No script applied; `errors` holds the last error, `last_script` the last script tried.
    Failed { stage: Stage, attempts: usize, errors: Vec<String>, last_script: String },
    /// A script applied as one undo step; `result` is `assistant.apply`'s return value.
    Applied { attempts: usize, result: Value },
}

/// The model call: the messages so far in, the reply text out.
pub type AskFn<'a> = dyn FnMut(&[Message]) -> std::result::Result<String, String> + 'a;

fn ask_with(ask: &mut AskFn, base: &[Message], reply: &str, user: String) -> Result<String> {
    let mut m = base.to_vec();
    m.push(Message::new("assistant", reply));
    m.push(Message::new("user", user));
    ask(&m).map_err(|e| EngineError::Other(format!("assistant: {e}")))
}

/// One assistant turn: ask, then read rounds (≤ [`MAX_READ_ROUNDS`], not attempts) and retries
/// (≤ [`MAX_ATTEMPTS`] scripts) until a script applies, the reply is prose, or the budget is spent.
/// A retry's re-ask carries only the failing reply and its error. Errs only when `ask` errs or
/// there is no drawing.
pub fn run_turn(s: &mut Session, user: &str, ask: &mut AskFn) -> Result<TurnOutcome> {
    let base = prompt(s, user)?;
    let mut reply = ask(&base).map_err(|e| EngineError::Other(format!("assistant: {e}")))?;
    let (mut attempts, mut rounds) = (1, 0);
    let mut last: Option<(Stage, String, String)> = None;
    loop {
        let Some(script) = extract_last_script(&reply) else {
            return Ok(match last {
                None => TurnOutcome::NoScript { text: reply },
                Some((stage, error, last_script)) => TurnOutcome::Failed { stage, attempts: attempts - 1, errors: vec![error], last_script },
            });
        };
        let last_script = serde_json::to_string(&script).unwrap_or_default();
        let failure = match script_steps(&script) {
            None => (Stage::Parse, "a script must be {\"steps\": [{command, params}]} or an array of steps".to_owned()),
            Some(steps) => {
                let read_only =
                    !steps.is_empty() && steps.iter().all(|st| st.get("command").and_then(Value::as_str).is_some_and(|c| READ_ONLY.contains(&c)));
                if read_only {
                    if rounds >= MAX_READ_ROUNDS {
                        return Ok(TurnOutcome::Failed {
                            stage: Stage::Apply,
                            attempts,
                            errors: vec!["read-round budget spent with no edit".to_owned()],
                            last_script,
                        });
                    }
                    rounds += 1;
                    let mut lines = Vec::new();
                    for (i, st) in steps.iter().enumerate() {
                        let id = st.get("command").and_then(Value::as_str).unwrap_or_default();
                        let params = st.get("params").cloned().unwrap_or_else(|| json!({}));
                        let value = match s.execute(id, &params) {
                            Ok(v) => v.to_string(),
                            Err(e) => format!("error: {e}"),
                        };
                        lines.push(format!("- read {} {id}({params}): {value}", i + 1));
                    }
                    let msg = format!(
                        "Query result(s), in call order:\n{}\nContinue: reply with prose if this answers the request, \
                         or another script per the contract above.",
                        lines.join("\n")
                    );
                    reply = ask_with(ask, &base, &reply, msg)?;
                    continue;
                }
                match s.execute(ID, &json!({ "steps": steps })) {
                    Ok(result) => return Ok(TurnOutcome::Applied { attempts, result }),
                    Err(e) => (Stage::Apply, e.to_string()),
                }
            }
        };
        let (stage, error) = failure;
        if attempts >= MAX_ATTEMPTS {
            return Ok(TurnOutcome::Failed { stage, attempts, errors: vec![error], last_script });
        }
        let msg = format!("That script failed: {error}\nReturn one corrected script per the contract above, and nothing else.");
        reply = ask_with(ask, &base, &reply, msg)?;
        attempts += 1;
        last = Some((stage, error, last_script));
    }
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
    fn prompt_is_three_messages_with_a_stable_first() -> R {
        let a = session()?;
        install_app()?;
        let mut b = Session::new();
        b.new_drawing(true);
        let pa = prompt(&a, "draw a wall")?;
        let pb = prompt(&b, "something else")?;
        assert_eq!(pa.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(), ["system", "system", "user"]);
        assert_eq!(pa[2].content, "draw a wall");
        assert_eq!(pa[0], pb[0], "message 1 is byte-identical across drawings");
        assert_ne!(pa[1], pb[1]);
        assert!(pa[0].content.contains(&format!("At most {MAX_READ_ROUNDS} read rounds")));
        Ok(())
    }

    #[test]
    fn catalogue_lists_scriptable_commands_sorted() {
        let c = catalogue();
        let ids: Vec<&str> = c.lines().map(|l| l.split(' ').next().unwrap_or("")).collect();
        for want in ["line", "circle", "move", "setrole"] {
            assert!(ids.contains(&want), "{want} missing");
        }
        assert!(c.lines().any(|l| l.starts_with("query ") && l.ends_with(" (read-only)")));
        assert!(c.lines().any(|l| l.starts_with("showlinks ") && l.ends_with(" (read-only)")));
        for gone in ["undo", ID, PROMPT_ID] {
            assert!(!ids.contains(&gone), "{gone} listed");
        }
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
        println!("catalogue: {} lines, {} bytes", ids.len(), c.len());
    }

    #[test]
    fn header_shows_views_locked_layer_counts_active_view_and_selection() -> R {
        let mut s = session()?;
        let before = handles(&s)?;
        s.execute("line", &json!({ "points": [[0, 0], [1000, 250]] }))?;
        let h = *handles(&s)?.difference(&before).next().ok_or("no line")?;
        let layer = s.doc()?.entity(h).ok_or("no entity")?.common.layer.clone();
        s.doc_mut()?.layer_mut(&layer).ok_or("no layer")?.locked = true;
        s.set_selection(vec![h]);
        let view = edited_view(&s).ok_or("not editing a view")?;
        let t = header(&s)?;
        assert!(t.contains(&format!("Views:\n  {}", view.0)), "{t}");
        assert!(t.contains(&format!("  {layer}")) && t.contains(" LOCKED"), "{t}");
        assert!(t.contains(&format!("  {}/{layer}: 1", view.0)), "{t}");
        assert!(t.contains(&format!("Active view: {}", view.0)), "{t}");
        assert!(t.contains("Selection: 1 entities"), "{t}");
        assert!(t.contains(&format!("  {} (", h.hex())) && t.contains("bbox [0,0]-[1000,250]"), "{t}");
        Ok(())
    }

    #[test]
    fn a_plain_drawing_has_a_header() -> R {
        install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let t = header(&s)?;
        assert!(!t.contains("Views:"), "{t}");
        assert!(t.contains("Active view: (none)") && t.contains("Current layer: 0") && t.contains("Selection: 0 entities"), "{t}");
        Ok(())
    }

    #[test]
    fn the_prompt_command_records_no_undo_step() -> R {
        let mut s = session()?;
        let n = undo_len(&s)?;
        let r = s.execute(PROMPT_ID, &json!({ "message": "hi" }))?;
        assert_eq!(undo_len(&s)?, n);
        assert_eq!(r["messages"][2], json!({ "role": "user", "content": "hi" }));
        Ok(())
    }

    /// Runs a turn against canned replies, returning the outcome and every message list asked.
    fn turn(s: &mut Session, replies: &[&str]) -> E<(TurnOutcome, Vec<Vec<Message>>)> {
        let mut left: std::collections::VecDeque<String> = replies.iter().map(|r| (*r).to_owned()).collect();
        let mut asked: Vec<Vec<Message>> = Vec::new();
        let mut ask = |m: &[Message]| {
            asked.push(m.to_vec());
            left.pop_front().ok_or_else(|| "out of canned replies".to_owned())
        };
        let out = run_turn(s, "draw it", &mut ask)?;
        Ok((out, asked))
    }

    const GOOD: &str = "Here:\n```json\n{\"steps\":[{\"command\":\"line\",\"params\":{\"points\":[[0,0],[1000,0]]}}]}\n```";
    const BAD: &str = "Here:\n```json\n{\"steps\":[{\"command\":\"circle\",\"params\":{}}]}\n```";
    const READ: &str = "Let me look.\n```json\n{\"steps\":[{\"command\":\"query\",\"params\":{}}]}\n```";

    fn last(asked: &[Vec<Message>], i: usize) -> E<&Message> {
        Ok(asked.get(i).and_then(|m| m.last()).ok_or("no such ask")?)
    }

    #[test]
    fn a_prose_reply_changes_nothing() -> R {
        let mut s = session()?;
        let before = s.doc()?.clone();
        let (out, asked) = turn(&mut s, &["Walls are 200 thick."])?;
        assert_eq!(out, TurnOutcome::NoScript { text: "Walls are 200 thick.".into() });
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].len(), 3, "the bare prompt");
        assert!(*s.doc()? == before);
        Ok(())
    }

    #[test]
    fn a_good_script_applies_as_one_stamped_undo_step() -> R {
        let mut s = session()?;
        let n = undo_len(&s)?;
        let (out, asked) = turn(&mut s, &[GOOD])?;
        let TurnOutcome::Applied { attempts, result } = out else { return Err(format!("{out:?}").into()) };
        assert_eq!((attempts, asked.len(), undo_len(&s)?), (1, 1, n + 1));
        let h = *stamped(&result).first().ok_or("nothing stamped")?;
        assert_eq!(origin(&s, h)?.ok_or("unstamped")?.producer, Some(AiProducer::Assistant));
        Ok(())
    }

    #[test]
    fn a_bad_script_is_retried_with_only_its_error() -> R {
        let mut s = session()?;
        let (out, asked) = turn(&mut s, &[BAD, GOOD])?;
        assert!(matches!(out, TurnOutcome::Applied { attempts: 2, .. }), "{out:?}");
        assert_eq!(asked.len(), 2);
        let m = &asked[1];
        assert_eq!(m.len(), 5, "prompt + the failed reply + the error");
        assert_eq!((m[3].role.as_str(), m[3].content.as_str()), ("assistant", BAD));
        assert!(m[4].content.starts_with("That script failed: ") && m[4].content.contains("step 1 (circle)"), "{}", m[4].content);
        Ok(())
    }

    #[test]
    fn three_bad_scripts_fail_and_change_nothing() -> R {
        let mut s = session()?;
        let (n, before) = (undo_len(&s)?, s.doc()?.clone());
        let (out, asked) = turn(&mut s, &[BAD, BAD, BAD, GOOD])?;
        let TurnOutcome::Failed { stage, attempts, errors, last_script } = out else { return Err(format!("{out:?}").into()) };
        assert_eq!((stage, attempts, asked.len()), (Stage::Apply, 3, 3));
        assert!(errors.len() == 1 && errors[0].contains("step 1 (circle)"), "{errors:?}");
        assert!(last_script.contains("circle"));
        assert_eq!(last(&asked, 2)?.content.matches("That script failed").count(), 1, "only the latest error");
        assert_eq!(asked[2].len(), 5, "earlier attempts are not carried");
        assert!(*s.doc()? == before);
        assert_eq!(undo_len(&s)?, n);
        Ok(())
    }

    #[test]
    fn a_query_round_feeds_back_then_the_edit_applies() -> R {
        let mut s = session()?;
        let n = undo_len(&s)?;
        let (out, asked) = turn(&mut s, &[READ, GOOD])?;
        assert!(matches!(out, TurnOutcome::Applied { attempts: 1, .. }), "{out:?}");
        assert_eq!(undo_len(&s)?, n + 1, "the read added no undo step");
        let q = &last(&asked, 1)?.content;
        assert!(q.starts_with("Query result(s)") && q.contains("- read 1 query({}): {"), "{q}");
        assert!(q.contains("\"views\""), "the query's JSON: {q}");
        Ok(())
    }

    #[test]
    fn read_rounds_run_out() -> R {
        let mut s = session()?;
        let (out, asked) = turn(&mut s, &[READ, READ, READ, GOOD])?;
        let TurnOutcome::Failed { errors, .. } = out else { return Err(format!("{out:?}").into()) };
        assert_eq!(errors, vec!["read-round budget spent with no edit".to_owned()]);
        assert_eq!(asked.len(), 3);
        Ok(())
    }

    #[test]
    fn a_retry_without_a_script_fails_with_the_previous_error() -> R {
        let mut s = session()?;
        let (out, _) = turn(&mut s, &[BAD, "sorry, I cannot"])?;
        assert!(matches!(&out, TurnOutcome::Failed { stage: Stage::Apply, attempts: 1, errors, .. } if errors[0].contains("circle")), "{out:?}");
        let (out, _) = turn(&mut s, &["```json\n{\"nope\": 1}\n```", GOOD])?;
        assert!(matches!(out, TurnOutcome::Applied { attempts: 2, .. }), "a script of the wrong shape is a Parse attempt: {out:?}");
        Ok(())
    }

    #[test]
    fn an_ask_error_is_a_turn_error() -> R {
        let mut s = session()?;
        let mut ask = |_: &[Message]| Err::<String, String>("offline".into());
        let e = run_turn(&mut s, "hi", &mut ask).err().ok_or("should err")?.to_string();
        assert!(e.contains("assistant: offline"), "{e}");
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

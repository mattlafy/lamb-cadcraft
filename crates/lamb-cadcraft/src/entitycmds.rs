//! ACCEPT, SETROLE and LAYERROLE (TS `commands/link.ts` `ACCEPT_COMMAND`, `entityProps.ts`
//! `SETROLE_COMMAND`, `layer.ts` `setDefaultRole`): take ownership of AI-proposed lines, override an
//! entity's role, set a layer's default role. Each is one undo step (the drawing snapshot is the
//! inverse); a call that would change nothing reads the drawing only, so it records no step.
//!
//! - `accept {handles?}` (`acc`): every requested unaccepted AI line, plus every other unaccepted
//!   AI line with the same `source_entity_id` in any view, gets `accepted = true`. Returns
//!   `{accepted, message}`; none → `"Nothing to accept"`. Interactive: "Select objects:".
//! - `setrole {handles?, role}`: `role` one of `cut seen datum view_clip annotation reference
//!   bylayer`, written on each handle that lives in a LAMB view block; the rest are counted in
//!   `skipped`. Interactive: select, then `Enter role [...] <Bylayer>:`.
//! - `layerrole {layer, role}`: a layer's default role (not `bylayer`). Interactive: layer name
//!   `<CLAYER>`, then the role with the layer's current default as the default.
//!
//! `handles` are hex strings or numbers; absent, the current selection (engine `cmd::targets`'
//! rule, parsed here because that fn is crate-private upstream). The Properties Role row and the
//! Layers palette role column are not built yet; they can call these.

use std::collections::HashSet;

use cadcraft_doc::Handle;
use cadcraft_engine::cmd::machines::{SelOutcome, SelectPhase, SelectRun};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::model::{Id, LineRole};
use serde_json::{Value, json};

use crate::commands::{bad, other};
use crate::project::DrawingProject;
use crate::schema::{read_entity_data, read_layer_data, set_entity_data, set_layer_data};

/// `handles`, or the current selection.
fn handles(cmd: &str, s: &Session, p: &Value) -> Result<Vec<Handle>> {
    match p.get("handles") {
        None | Some(Value::Null) => Ok(s.selection()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_str().and_then(Handle::parse_hex).or_else(|| v.as_u64().map(Handle)))
            .map(|h| h.ok_or_else(|| bad(cmd, "`handles` must be hex handle strings or numbers")))
            .collect(),
        Some(_) => Err(bad(cmd, "`handles` must be an array of object handles")),
    }
}

/// The keyword word of each role, as the role prompt offers it.
const ROLE_WORDS: [(&str, LineRole); 7] = [
    ("Cut", LineRole::Cut),
    ("Seen", LineRole::Seen),
    ("Datum", LineRole::Datum),
    ("Viewclip", LineRole::ViewClip),
    ("Annotation", LineRole::Annotation),
    ("Reference", LineRole::Reference),
    ("Bylayer", LineRole::Bylayer),
];

fn role_word(r: LineRole) -> &'static str {
    ROLE_WORDS.iter().find(|(_, x)| *x == r).map_or("Bylayer", |(w, _)| w)
}

/// A typed role: a keyword (any case, `view_clip` too) or its unique first letter.
fn parse_role(t: &str) -> Option<LineRole> {
    let t = t.trim().to_ascii_lowercase().replace('_', "");
    if t.is_empty() {
        return None;
    }
    ROLE_WORDS.iter().find(|(w, _)| w.eq_ignore_ascii_case(&t) || (t.len() == 1 && w.to_ascii_lowercase().starts_with(&t))).map(|(_, r)| *r)
}

/// `role`, the serde snake_case name; `bylayer` only where `allow_bylayer`.
fn role_param(cmd: &str, p: &Value, allow_bylayer: bool) -> Result<LineRole> {
    let v = p.get("role").ok_or_else(|| bad(cmd, "`role` is required"))?;
    let r: LineRole = serde_json::from_value(v.clone())
        .map_err(|_| bad(cmd, "`role` must be one of cut, seen, datum, view_clip, annotation, reference, bylayer"))?;
    if r == LineRole::Bylayer && !allow_bylayer {
        return Err(other("A layer's default role can't be bylayer"));
    }
    Ok(r)
}

/// ACCEPT: TS `applyAccept`.
pub(crate) fn accept(s: &mut Session, p: &Value) -> Result<Value> {
    let requested: HashSet<Id> = handles("accept", s, p)?.into_iter().map(|h| Id(h.0)).collect();
    // Read before `doc_mut`: a call that accepts nothing must not record an undo step.
    let ids: Vec<Id> = {
        let dp = DrawingProject::new(s.doc()?).map_err(other)?;
        let sources: HashSet<Id> = requested
            .iter()
            .filter_map(|id| dp.entity(*id))
            .filter_map(|e| e.ai_origin.filter(|o| !o.accepted).and_then(|o| o.source_entity_id))
            .collect();
        let mut out = Vec::new();
        for v in dp.views() {
            for e in dp.entities(v.id) {
                let Some(o) = e.ai_origin.filter(|o| !o.accepted) else { continue };
                if requested.contains(&e.id) || o.source_entity_id.is_some_and(|src| sources.contains(&src)) {
                    out.push(e.id);
                }
            }
        }
        out
    };
    if ids.is_empty() {
        return Ok(json!({ "accepted": 0, "message": "Nothing to accept" }));
    }
    let d = s.doc_mut()?;
    for id in &ids {
        let mut res = Ok(());
        d.modify_entity(Handle(id.0), |e| {
            res = read_entity_data(&e.common).and_then(|mut data| {
                if let Some(o) = data.ai_origin.as_mut() {
                    o.accepted = true;
                }
                set_entity_data(&mut e.common, &data)
            });
        })
        .map_err(other)?;
        res.map_err(other)?;
    }
    let n = ids.len();
    Ok(json!({ "accepted": n, "message": format!("Accepted {n} line{}", if n == 1 { "" } else { "s" }) }))
}

/// SETROLE: the role on every handle in a LAMB view block; the rest are skipped.
pub(crate) fn setrole(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = handles("setrole", s, p)?;
    let role = role_param("setrole", p, true)?;
    let (mut changed, mut skipped, mut writes) = (0usize, 0usize, Vec::new());
    {
        let dp = DrawingProject::new(s.doc()?).map_err(other)?;
        for h in hs {
            match dp.entity(Id(h.0)) {
                None => skipped += 1,
                Some(e) => {
                    changed += 1;
                    // An unchanged role isn't written, so a no-op call records no undo step.
                    if e.role != role {
                        writes.push(h);
                    }
                }
            }
        }
    }
    if !writes.is_empty() {
        let d = s.doc_mut()?;
        for h in writes {
            let mut res = Ok(());
            d.modify_entity(h, |e| {
                res = read_entity_data(&e.common).and_then(|mut data| {
                    data.role = role;
                    set_entity_data(&mut e.common, &data)
                });
            })
            .map_err(other)?;
            res.map_err(other)?;
        }
    }
    let mut message = format!("Role {} set on {changed} object{}", role_word(role), if changed == 1 { "" } else { "s" });
    if skipped > 0 {
        message += &format!(", {skipped} not in a LAMB view skipped");
    }
    Ok(json!({ "changed": changed, "skipped": skipped, "message": message }))
}

/// LAYERROLE: a layer's default role.
pub(crate) fn layerrole(s: &mut Session, p: &Value) -> Result<Value> {
    let layer = p.get("layer").and_then(Value::as_str).ok_or_else(|| bad("layerrole", "`layer` must be a string"))?.to_owned();
    let role = role_param("layerrole", p, false)?;
    let current = {
        let l = s.doc()?.layer(&layer).ok_or_else(|| other(format!("No such layer: {layer}")))?;
        read_layer_data(l).map_err(other)?
    };
    let message = format!("Layer {layer} default role {}", role_word(role));
    if current.default_role != role {
        let l = s.doc_mut()?.layer_mut(&layer).ok_or_else(|| other(format!("No such layer: {layer}")))?;
        let mut data = current;
        data.default_role = role;
        set_layer_data(l, &data).map_err(other)?;
    }
    Ok(json!({ "layer": layer, "role": role, "message": message }))
}

/// Run a body, echoing its `message` or its error.
fn call(s: &mut Session, f: fn(&mut Session, &Value) -> Result<Value>, p: &Value) {
    match f(s, p) {
        Ok(r) => {
            if let Some(m) = r.get("message").and_then(Value::as_str) {
                s.echo(m.to_owned());
            }
        }
        Err(e) => s.echo(e.to_string()),
    }
}

/// The text of a prompt answer; `None` for anything else.
fn answer(i: Input) -> Option<String> {
    match i {
        Input::Enter => Some(String::new()),
        Input::Text(t) | Input::Keyword(t) => Some(t.trim().to_owned()),
        _ => None,
    }
}

fn role_prompt(default: LineRole, bylayer: bool) -> Prompt {
    let words: Vec<&str> = ROLE_WORDS.iter().map(|(w, _)| *w).filter(|w| bylayer || *w != "Bylayer").collect();
    Prompt::new("Enter role", Accept::TEXT).kw(&words).default(role_word(default))
}

pub(crate) fn accept_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(SelectRun::new("accept", "ACCEPT")))
}

/// SETROLE: select objects, then a role.
struct SetRole {
    sel: SelectPhase,
    picked: Option<Vec<Handle>>,
}

impl SetRole {
    fn apply(&self, s: &mut Session, role: LineRole) {
        let hs: Vec<String> = self.picked.iter().flatten().map(|h| h.hex()).collect();
        call(s, setrole, &json!({ "handles": hs, "role": role }));
        s.set_selection(Vec::new());
    }
}

impl Interactive for SetRole {
    fn name(&self) -> &'static str {
        "SETROLE"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.picked = Some(self.sel.picked.clone());
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.picked {
            None => self.sel.prompt(),
            Some(_) => role_prompt(LineRole::Bylayer, true),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        if self.picked.is_none() {
            return Ok(match self.sel.feed(s, &i)? {
                SelOutcome::More => Step::Continue,
                SelOutcome::Empty => Step::Done,
                SelOutcome::Done(hs) => {
                    self.picked = Some(hs);
                    Step::Continue
                }
            });
        }
        let Some(text) = answer(i) else { return Ok(Step::Continue) };
        let role = if text.is_empty() { Some(LineRole::Bylayer) } else { parse_role(&text) };
        let Some(role) = role else {
            s.echo("Invalid option keyword.");
            return Ok(Step::Continue);
        };
        self.apply(s, role);
        Ok(Step::Done)
    }
}

pub(crate) fn setrole_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(SetRole { sel: SelectPhase::default(), picked: None }))
}

/// LAYERROLE: a layer name, then its default role.
struct LayerRole {
    layer: Option<String>,
}

fn clayer(s: &Session) -> String {
    s.doc().map(|d| d.header.str("CLAYER", "0")).unwrap_or_else(|_| "0".into())
}

fn default_role_of(s: &Session, layer: &str) -> LineRole {
    s.doc().ok().and_then(|d| d.layer(layer)).and_then(|l| read_layer_data(l).ok()).map_or(LineRole::Cut, |d| d.default_role)
}

impl Interactive for LayerRole {
    fn name(&self) -> &'static str {
        "LAYERROLE"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match &self.layer {
            None => Prompt::new("Enter layer name", Accept::TEXT).default(clayer(s)),
            Some(l) => role_prompt(default_role_of(s, l), false),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(text) = answer(i) else { return Ok(Step::Continue) };
        let Some(layer) = self.layer.clone() else {
            let name = if text.is_empty() { clayer(s) } else { text };
            match s.doc()?.layer(&name) {
                Some(l) => self.layer = Some(l.name.clone()),
                None => s.echo(format!("No such layer: {name}")),
            }
            return Ok(Step::Continue);
        };
        let role = if text.is_empty() { Some(default_role_of(s, &layer)) } else { parse_role(&text).filter(|r| *r != LineRole::Bylayer) };
        let Some(role) = role else {
            s.echo("Invalid option keyword.");
            return Ok(Step::Continue);
        };
        call(s, layerrole, &json!({ "layer": layer, "role": role }));
        Ok(Step::Done)
    }
}

pub(crate) fn layerrole_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(LayerRole { layer: None }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{enter_space, install_app};
    use crate::schema::{EXT_KEY, view_block_name};
    use cadcraft_doc::Space;
    use lamb_core::model::AiOrigin;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type E<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    fn session() -> E<(Session, Id)> {
        install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no plan")?);
        Ok((s, plan))
    }

    /// A line drawn in the current space; its handle.
    fn line(s: &mut Session, y: f64) -> E<Handle> {
        let before: HashSet<Handle> = all_handles(s)?;
        s.execute("line", &json!({ "points": [[0, y], [1000, y]] }))?;
        Ok(all_handles(s)?.into_iter().find(|h| !before.contains(h)).ok_or("line not drawn")?)
    }

    fn all_handles(s: &Session) -> E<HashSet<Handle>> {
        let d = s.doc()?;
        Ok(d.model.iter().map(|e| e.handle).chain(d.blocks.values().flat_map(|b| b.entities.iter().map(|e| e.handle))).collect())
    }

    fn set_ai(s: &mut Session, h: Handle, source: u64) -> R {
        let origin = AiOrigin {
            proposal_id: format!("p{}", h.0),
            accepted: false,
            assumption_note: None,
            source_entity_id: Some(Id(source)),
            swept_geometry: None,
            producer: None,
        };
        let mut res = Ok(());
        s.doc_mut()?.modify_entity(h, |e| {
            res = read_entity_data(&e.common).and_then(|mut d| {
                d.ai_origin = Some(origin);
                set_entity_data(&mut e.common, &d)
            });
        })?;
        Ok(res?)
    }

    fn accepted(s: &Session, h: Handle) -> E<bool> {
        let e = s.doc()?.entity(h).ok_or("no entity")?;
        Ok(read_entity_data(&e.common)?.ai_origin.ok_or("no ai origin")?.accepted)
    }

    fn undo_len(s: &Session) -> E<usize> {
        Ok(s.state()?.undo.len())
    }

    #[test]
    fn accept_takes_the_requested_line_and_its_siblings_and_undoes() -> R {
        let (mut s, _) = session()?;
        let (a, b, c, plain) = (line(&mut s, 0.0)?, line(&mut s, 100.0)?, line(&mut s, 200.0)?, line(&mut s, 300.0)?);
        set_ai(&mut s, a, 9001)?;
        set_ai(&mut s, b, 9001)?;
        set_ai(&mut s, c, 9002)?;

        let n = undo_len(&s)?;
        let r = s.execute("accept", &json!({ "handles": [plain.hex()] }))?;
        assert_eq!(r, json!({ "accepted": 0, "message": "Nothing to accept" }));
        assert_eq!(undo_len(&s)?, n, "a refusal records no undo step");

        let r = s.execute("accept", &json!({ "handles": [a.hex()] }))?;
        assert_eq!(r["accepted"], json!(2));
        assert!(accepted(&s, a)? && accepted(&s, b)?, "the line and its sibling");
        assert!(!accepted(&s, c)?, "another source's line is left");
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        s.undo()?;
        assert!(!accepted(&s, a)? && !accepted(&s, b)? && !accepted(&s, c)?, "undo restores all");
        Ok(())
    }

    #[test]
    fn accept_interactive_selects_then_runs() -> R {
        let (mut s, _) = session()?;
        let a = line(&mut s, 0.0)?;
        set_ai(&mut s, a, 9001)?;
        s.start("acc")?;
        s.input(Input::Pick(vec![a]))?;
        s.input(Input::Enter)?;
        assert!(s.running.is_none());
        assert!(accepted(&s, a)?);
        assert!(s.log.iter().any(|l| l == "Accepted 1 line"), "{:?}", s.log);
        Ok(())
    }

    #[test]
    fn setrole_writes_view_lines_skips_model_and_undoes() -> R {
        let (mut s, _) = session()?;
        let in_view = line(&mut s, 0.0)?;
        enter_space(&mut s, Space::Model)?;
        let in_model = line(&mut s, 0.0)?;
        let r = s.execute("setrole", &json!({ "handles": [in_view.hex(), in_model.hex()], "role": "datum" }))?;
        assert_eq!((r["changed"].clone(), r["skipped"].clone()), (json!(1), json!(1)));
        let role = |s: &Session| -> E<LineRole> { Ok(DrawingProject::new(s.doc()?)?.entity(Id(in_view.0)).ok_or("gone")?.role) };
        assert_eq!(role(&s)?, LineRole::Datum);
        assert!(!s.doc()?.entity(in_model).ok_or("gone")?.common.ext.contains_key(EXT_KEY), "Model line untouched");
        assert!(s.execute("setrole", &json!({ "handles": [in_view.hex()], "role": "wall" })).is_err());
        s.undo()?;
        assert_eq!(role(&s)?, LineRole::Bylayer);
        Ok(())
    }

    #[test]
    fn setrole_interactive_keyword_viewclip_writes_view_clip() -> R {
        let (mut s, _) = session()?;
        let h = line(&mut s, 0.0)?;
        s.start("setrole")?;
        s.input(Input::Pick(vec![h]))?;
        s.input(Input::Enter)?;
        assert_eq!(
            s.current_prompt().map(|p| p.display()).as_deref(),
            Some("Enter role or [Cut/Seen/Datum/Viewclip/Annotation/Reference/Bylayer] <Bylayer>:")
        );
        s.input(Input::Keyword("Viewclip".into()))?;
        assert!(s.running.is_none());
        let e = s.doc()?.entity(h).ok_or("gone")?;
        assert_eq!(e.common.ext.get(EXT_KEY).and_then(|v| v.get("role")), Some(&json!("view_clip")));
        Ok(())
    }

    #[test]
    fn layerrole_sets_the_default_and_refuses_bylayer_and_unknown() -> R {
        let (mut s, plan) = session()?;
        s.doc_mut()?.ensure_layer("A-WALL");
        let n = undo_len(&s)?;
        s.execute("layerrole", &json!({ "layer": "A-WALL", "role": "seen" }))?;
        let role = |s: &Session| -> E<LineRole> {
            let dp = DrawingProject::new(s.doc()?)?;
            Ok(dp.layers().iter().find(|l| l.id == "A-WALL").ok_or("no layer")?.default_role)
        };
        assert_eq!(role(&s)?, LineRole::Seen);
        assert_eq!(undo_len(&s)?, n + 1);
        assert!(s.execute("layerrole", &json!({ "layer": "A-WALL", "role": "bylayer" })).is_err());
        assert!(s.execute("layerrole", &json!({ "layer": "NOPE", "role": "cut" })).is_err_and(|e| e.to_string().contains("No such layer: NOPE")));
        assert_eq!(undo_len(&s)?, n + 1, "refusals record nothing");

        // Interactive: the layer, then Enter keeps the current default; a keyword changes it.
        enter_space(&mut s, Space::Block(view_block_name(plan)))?;
        s.start("layerrole")?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("0"), "CLAYER");
        s.input(Input::Text("A-WALL".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter role or [Cut/Seen/Datum/Viewclip/Annotation/Reference] <Seen>:"));
        s.input(Input::Keyword("Datum".into()))?;
        assert!(s.running.is_none());
        assert_eq!(role(&s)?, LineRole::Datum);
        s.undo()?;
        assert_eq!(role(&s)?, LineRole::Seen);
        Ok(())
    }

    #[test]
    fn parse_role_takes_words_and_letters() {
        assert_eq!(parse_role("view_clip"), Some(LineRole::ViewClip));
        assert_eq!(parse_role("V"), Some(LineRole::ViewClip));
        assert_eq!(parse_role("annotation"), Some(LineRole::Annotation));
        assert_eq!(parse_role("x"), None);
    }
}

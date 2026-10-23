//! VIEWPROPS (LAMB T347): a view's own properties — rename, a placed plan's cut elevation, a
//! vertical view's intent (section / elevation) and flip, and (LAMB T499, fork-only) `cutOffset`: a
//! placed vertical view's cut line moved parallel to itself — CUTLINE in its host plan. The backend is the TS command engine's
//! `applyViewProps` (`packages/command-engine/src/commands/viewEdits.ts`) case for case, refusals
//! word for word; undo is the engine's snapshot of the LAMB slot, so no inverse is written here.
//!
//! The interactive form is the command-line shape of the TS Properties panel's view form: on the
//! edited view, `Enter an option or [Name/Elevation/Intent/Flip]:` with only the options that view
//! has, then one prompt for the new value (Flip applies at once). A rename leaves the view's block
//! (`LAMB_VIEW_<id>`) as it is; the Views palette re-reads names every frame.

use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step, units::parse_distance};
use lamb_core::model::{AnyView, Id, Placement, VerticalIntent};
use serde_json::{Value, json};

use crate::commands::{bad, edited_view, id_value, name_input, other};
use crate::schema::{read_document, write_document};

const CMD: &str = "viewprops";

fn intent_word(i: VerticalIntent) -> &'static str {
    if i == VerticalIntent::Elevation { "Elevation" } else { "Section" }
}

/// VIEWPROPS `{mode, viewId, name | cutElevation | intent | flipped | offsetMm}`. Every refusal is decided on
/// a read of the document before anything is written, so a refusal records no undo step.
pub(crate) fn viewprops(s: &mut Session, p: &Value) -> Result<Value> {
    let mode = p.get("mode").and_then(Value::as_str).ok_or_else(|| bad(CMD, "`mode` must be rename|cutElevation|intent|flip|cutOffset"))?;
    let raw_id = p.get("viewId").ok_or_else(|| bad(CMD, "`viewId` is required"))?;
    let shown_id = raw_id.as_str().map_or_else(|| raw_id.to_string(), str::to_owned);
    let mut doc = read_document(s.doc()?).map_err(other)?;
    let view =
        id_value(raw_id).and_then(|id| doc.views.iter_mut().find(|v| v.id() == id)).ok_or_else(|| other(format!("No such view: {shown_id}")))?;
    match mode {
        "rename" => {
            let name = p.get("name").and_then(Value::as_str).ok_or_else(|| bad(CMD, "`name` must be a string"))?.to_owned();
            match view {
                AnyView::Plan(v) => v.name = name,
                AnyView::Vertical(v) => v.name = name,
                AnyView::Detail(v) => v.name = name,
            }
        }
        "cutElevation" => {
            let value = p
                .get("cutElevation")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite())
                .ok_or_else(|| bad(CMD, "`cutElevation` must be a finite number"))?;
            let AnyView::Plan(v) = view else { return Err(other(format!("View {shown_id} is not a plan"))) };
            let Some(Placement::Plan { cut_elevation, .. }) = &mut v.placement else {
                return Err(other(format!("View {shown_id} is floating; it has no cut elevation")));
            };
            *cut_elevation = value;
        }
        "intent" => {
            let intent = match p.get("intent").and_then(Value::as_str) {
                Some("section") => VerticalIntent::Section,
                Some("elevation") => VerticalIntent::Elevation,
                _ => return Err(bad(CMD, "`intent` must be section|elevation")),
            };
            let AnyView::Vertical(v) = view else { return Err(other(format!("View {shown_id} is not a section or elevation"))) };
            v.intent = intent;
        }
        "flip" => {
            let value = p.get("flipped").and_then(Value::as_bool).ok_or_else(|| bad(CMD, "`flipped` must be a boolean"))?;
            let AnyView::Vertical(v) = view else { return Err(other(format!("View {shown_id} is not a section or elevation"))) };
            let Some(Placement::Vertical { flipped, .. }) = &mut v.placement else {
                return Err(other(format!("View {shown_id} is floating; it has no viewing direction")));
            };
            *flipped = Some(value);
        }
        "cutOffset" => {
            let offset = p
                .get("offsetMm")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && *v != 0.0)
                .ok_or_else(|| bad(CMD, "`offsetMm` must be a finite, non-zero number"))?;
            let AnyView::Vertical(v) = view else { return Err(other(format!("View {shown_id} is not a section or elevation"))) };
            let Some(Placement::Vertical { cut_line, .. }) = &mut v.placement else {
                return Err(other(format!("View {shown_id} is floating; it has no cut line")));
            };
            *cut_line = crate::offsetviews::shifted_cut_line(cut_line, offset)?;
        }
        _ => return Err(bad(CMD, "`mode` must be rename|cutElevation|intent|flip|cutOffset")),
    }
    write_document(s.doc_mut()?, &doc).map_err(other)?;
    Ok(Value::Null)
}

/// What the first prompt picked.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Field {
    Name,
    Elevation,
    Intent,
}

/// VIEWPROPS at the command line, on the edited view.
struct ViewProps {
    view: Option<Id>,
    field: Option<Field>,
}

impl ViewProps {
    fn current(s: &Session, id: Id) -> Option<AnyView> {
        read_document(s.doc().ok()?).ok()?.views.into_iter().find(|v| v.id() == id)
    }

    /// The options this view has: a placed plan Name/Elevation, a placed vertical
    /// Name/Intent/Flip, a floating vertical Name/Intent, anything else Name.
    fn options(v: &AnyView) -> &'static [&'static str] {
        match v {
            AnyView::Plan(p) if p.placement.is_some() => &["Name", "Elevation"],
            AnyView::Vertical(p) if p.placement.is_some() => &["Name", "Intent", "Flip"],
            AnyView::Vertical(_) => &["Name", "Intent"],
            _ => &["Name"],
        }
    }

    fn run(s: &mut Session, p: &Value) -> Result<bool> {
        match viewprops(s, p) {
            Ok(_) => Ok(true),
            Err(e) => {
                s.echo(e.to_string());
                Ok(false)
            }
        }
    }
}

fn cut_elevation(v: &AnyView) -> Option<f64> {
    match v {
        AnyView::Plan(p) => match &p.placement {
            Some(Placement::Plan { cut_elevation, .. }) => Some(*cut_elevation),
            _ => None,
        },
        _ => None,
    }
}

impl Interactive for ViewProps {
    fn name(&self) -> &'static str {
        CMD
    }
    fn prompt(&self, s: &Session) -> Prompt {
        let Some(v) = self.view.and_then(|id| Self::current(s, id)) else { return Prompt::new("Enter an option", Accept::TEXT) };
        match self.field {
            None => Prompt::new("Enter an option", Accept::TEXT).kw(Self::options(&v)),
            Some(Field::Name) => Prompt::new("Enter view name", Accept::TEXT).default(v.name().to_owned()),
            Some(Field::Elevation) => {
                let p = Prompt::new("Specify cut elevation", Accept::NUMBER);
                match cut_elevation(&v) {
                    Some(c) => p.default(c.to_string()),
                    None => p,
                }
            }
            Some(Field::Intent) => {
                let p = Prompt::new("Enter intent", Accept::TEXT).kw(&["Section", "Elevation"]);
                match &v {
                    AnyView::Vertical(vv) => p.default(intent_word(vv.intent)),
                    _ => p,
                }
            }
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.view = edited_view(s);
        if self.view.is_none() {
            s.echo("Edit a view first (EDITVIEW).");
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(id) = self.view else { return Ok(Step::Done) };
        let Some(v) = Self::current(s, id) else { return Ok(Step::Done) };
        let Some(field) = self.field else {
            let picked = match i {
                Input::Enter => return Ok(Step::Cancel),
                Input::Text(t) | Input::Keyword(t) => self.prompt(s).match_keyword(&t),
                _ => return Ok(Step::Continue),
            };
            match picked.as_deref() {
                Some("Name") => self.field = Some(Field::Name),
                Some("Elevation") => self.field = Some(Field::Elevation),
                Some("Intent") => self.field = Some(Field::Intent),
                Some("Flip") => {
                    let AnyView::Vertical(vv) = &v else { return Ok(Step::Done) };
                    let was = matches!(vv.placement, Some(Placement::Vertical { flipped: Some(true), .. }));
                    if Self::run(s, &json!({ "mode": "flip", "viewId": id.0, "flipped": !was }))? {
                        s.echo(format!("{} {}.", v.name(), if was { "unflipped" } else { "flipped" }));
                    }
                    return Ok(Step::Done);
                }
                _ => return Err(other("Invalid option keyword.")),
            }
            return Ok(Step::Continue);
        };
        match field {
            Field::Name => {
                let Some(name) = name_input(i, v.name()) else { return Ok(Step::Continue) };
                if name != v.name() {
                    Self::run(s, &json!({ "mode": "rename", "viewId": id.0, "name": name }))?;
                }
            }
            Field::Elevation => {
                let value = match i {
                    Input::Enter => return Ok(Step::Done),
                    Input::Text(t) | Input::Keyword(t) => parse_distance(&t).filter(|v| v.is_finite()).ok_or_else(|| other("Requires a number."))?,
                    _ => return Ok(Step::Continue),
                };
                if cut_elevation(&v) != Some(value) {
                    Self::run(s, &json!({ "mode": "cutElevation", "viewId": id.0, "cutElevation": value }))?;
                }
            }
            Field::Intent => {
                let picked = match i {
                    Input::Enter => return Ok(Step::Done),
                    Input::Text(t) | Input::Keyword(t) => self.prompt(s).match_keyword(&t),
                    _ => return Ok(Step::Continue),
                };
                let intent = match picked.as_deref() {
                    Some("Section") => VerticalIntent::Section,
                    Some("Elevation") => VerticalIntent::Elevation,
                    _ => return Err(other("Invalid option keyword.")),
                };
                if matches!(&v, AnyView::Vertical(vv) if vv.intent != intent) {
                    let word = if intent == VerticalIntent::Elevation { "elevation" } else { "section" };
                    Self::run(s, &json!({ "mode": "intent", "viewId": id.0, "intent": word }))?;
                }
            }
        }
        Ok(Step::Done)
    }
}

pub(crate) fn viewprops_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(ViewProps { view: None, field: None }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::install_app;
    use cadcraft_doc::Space;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type E = Box<dyn std::error::Error>;

    fn view(s: &Session, id: Id) -> std::result::Result<AnyView, E> {
        Ok(read_document(s.doc()?)?.views.into_iter().find(|v| v.id() == id).ok_or("no view")?)
    }

    /// `lambproject` (the placed `Ground Floor`, edited), a placed section `A-A` on it, a floating
    /// plan `Loose` and a floating elevation `Free`.
    fn project() -> std::result::Result<(Session, Id, Id, Id, Id), E> {
        install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let id = |r: Value| r["viewId"].as_u64().map(Id).ok_or("no viewId");
        let plan = id(s.execute("lambproject", &json!({}))?)?;
        let sec = id(s.execute(
            "newsectionline",
            &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 1000}, "cutTo": {"x": 5000, "y": 1000} }),
        )?)?;
        let loose = id(s.execute("newplan", &json!({ "name": "Loose" }))?)?;
        let free = {
            let d = s.doc_mut()?;
            let v = AnyView::Vertical(lamb_core::model::VerticalView {
                id: Id(0),
                name: "Free".into(),
                intent: VerticalIntent::Elevation,
                entities: Vec::new(),
                placement: None,
            });
            crate::views::create_view(d, v)?
        };
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        Ok((s, plan, sec, loose, free))
    }

    fn err(s: &mut Session, p: Value) -> std::result::Result<String, E> {
        Ok(s.execute(CMD, &p).err().ok_or("accepted")?.to_string())
    }

    #[test]
    fn rename_changes_the_name_only_and_undoes() -> R {
        let (mut s, plan, ..) = project()?;
        s.execute(CMD, &json!({ "mode": "rename", "viewId": plan.0, "name": "Level 1 plan" }))?;
        assert_eq!(view(&s, plan)?.name(), "Level 1 plan");
        assert!(s.doc()?.block(&crate::schema::view_block_name(plan)).is_some(), "the block keeps its name");
        s.undo()?;
        assert_eq!(view(&s, plan)?.name(), "Ground Floor");
        Ok(())
    }

    #[test]
    fn cut_elevation_on_a_placed_plan_and_undo() -> R {
        let (mut s, plan, ..) = project()?;
        let before = cut_elevation(&view(&s, plan)?).ok_or("not placed")?;
        s.execute(CMD, &json!({ "mode": "cutElevation", "viewId": plan.0, "cutElevation": 1500.0 }))?;
        assert_eq!(cut_elevation(&view(&s, plan)?), Some(1500.0));
        s.undo()?;
        assert_eq!(cut_elevation(&view(&s, plan)?), Some(before));
        Ok(())
    }

    #[test]
    fn every_refusal_uses_the_ts_wording_and_changes_nothing() -> R {
        let (mut s, _plan, sec, loose, free) = project()?;
        let before = s.doc()?.clone();
        assert!(err(&mut s, json!({ "mode": "rename", "viewId": 999_999, "name": "x" }))?.contains("No such view: 999999"));
        assert!(
            err(&mut s, json!({ "mode": "cutElevation", "viewId": sec.0, "cutElevation": 1.0 }))?.contains(&format!("View {} is not a plan", sec.0))
        );
        assert!(
            err(&mut s, json!({ "mode": "cutElevation", "viewId": loose.0, "cutElevation": 1.0 }))?
                .contains(&format!("View {} is floating; it has no cut elevation", loose.0))
        );
        assert!(
            err(&mut s, json!({ "mode": "intent", "viewId": loose.0, "intent": "elevation" }))?
                .contains(&format!("View {} is not a section or elevation", loose.0))
        );
        assert!(
            err(&mut s, json!({ "mode": "flip", "viewId": loose.0, "flipped": true }))?
                .contains(&format!("View {} is not a section or elevation", loose.0))
        );
        assert!(
            err(&mut s, json!({ "mode": "flip", "viewId": free.0, "flipped": true }))?
                .contains(&format!("View {} is floating; it has no viewing direction", free.0))
        );
        assert_eq!(read_document(s.doc()?)?, read_document(&before)?, "refusals change nothing");
        Ok(())
    }

    #[test]
    fn intent_and_flip_absent_true_false_each_undo() -> R {
        let (mut s, _plan, sec, ..) = project()?;
        let intent = |s: &Session| -> std::result::Result<VerticalIntent, E> {
            match view(s, sec)? {
                AnyView::Vertical(v) => Ok(v.intent),
                _ => Err("not vertical".into()),
            }
        };
        let flipped = |s: &Session| -> std::result::Result<Option<bool>, E> {
            match view(s, sec)? {
                AnyView::Vertical(lamb_core::model::VerticalView { placement: Some(Placement::Vertical { flipped, .. }), .. }) => Ok(flipped),
                _ => Err("not a placed vertical".into()),
            }
        };
        s.execute(CMD, &json!({ "mode": "intent", "viewId": sec.0, "intent": "elevation" }))?;
        assert_eq!(intent(&s)?, VerticalIntent::Elevation);
        s.undo()?;
        assert_eq!(intent(&s)?, VerticalIntent::Section);

        assert_eq!(flipped(&s)?, None);
        s.execute(CMD, &json!({ "mode": "flip", "viewId": sec.0, "flipped": true }))?;
        assert_eq!(flipped(&s)?, Some(true));
        s.execute(CMD, &json!({ "mode": "flip", "viewId": sec.0, "flipped": false }))?;
        assert_eq!(flipped(&s)?, Some(false));
        s.undo()?;
        assert_eq!(flipped(&s)?, Some(true));
        s.undo()?;
        assert_eq!(flipped(&s)?, None, "undo restores never-flipped, not false");
        Ok(())
    }

    fn cut(s: &Session, id: Id) -> std::result::Result<(lamb_core::geometry::polyline::Polyline2, Option<Placement>), E> {
        match view(s, id)? {
            AnyView::Vertical(lamb_core::model::VerticalView { placement: Some(pl @ Placement::Vertical { .. }), .. }) => {
                let Placement::Vertical { cut_line, .. } = &pl else { return Err("unreachable".into()) };
                Ok((cut_line.clone(), Some(pl.clone())))
            }
            _ => Err("not a placed vertical".into()),
        }
    }

    #[test]
    fn cut_offset_shifts_the_cut_line_only_and_undoes() -> R {
        let (mut s, plan, sec, loose, free) = project()?;
        let (before, Some(Placement::Vertical { host_view_id, transform, flipped, .. })) = cut(&s, sec)? else { return Err("no placement".into()) };
        let entities = match view(&s, sec)? {
            AnyView::Vertical(v) => v.entities,
            _ => return Err("not vertical".into()),
        };
        let undo = s.state()?.undo.len();
        s.execute(CMD, &json!({ "mode": "cutOffset", "viewId": sec.0, "offsetMm": 500 }))?;
        let (after, Some(Placement::Vertical { host_view_id: h2, transform: t2, flipped: f2, .. })) = cut(&s, sec)? else {
            return Err("no placement".into());
        };
        // A cut along +X moves to its left: y 1000 → 1500, x untouched.
        let pts = |c: &lamb_core::geometry::polyline::Polyline2| {
            std::iter::once(c.start).chain(c.segments.iter().map(|g| g.to)).map(|q| (q.x, q.y)).collect::<Vec<_>>()
        };
        assert_eq!(pts(&before), vec![(0.0, 1000.0), (5000.0, 1000.0)]);
        assert_eq!(pts(&after), vec![(0.0, 1500.0), (5000.0, 1500.0)]);
        assert_eq!((h2, t2, f2), (host_view_id, transform, flipped), "host, transform and flip untouched");
        assert_eq!(host_view_id, plan);
        assert!(matches!(view(&s, sec)?, AnyView::Vertical(v) if v.entities == entities), "entities untouched");
        assert_eq!(s.state()?.undo.len(), undo + 1, "one undo step");
        s.undo()?;
        assert_eq!(cut(&s, sec)?.0, before);

        let doc = s.doc()?.clone();
        let undo = s.state()?.undo.len();
        assert!(
            err(&mut s, json!({ "mode": "cutOffset", "viewId": loose.0, "offsetMm": 10 }))?
                .contains(&format!("View {} is not a section or elevation", loose.0))
        );
        assert!(
            err(&mut s, json!({ "mode": "cutOffset", "viewId": free.0, "offsetMm": 10 }))?
                .contains(&format!("View {} is floating; it has no cut line", free.0))
        );
        for bad in [json!(0), json!("x"), Value::Null] {
            assert!(
                err(&mut s, json!({ "mode": "cutOffset", "viewId": sec.0, "offsetMm": bad }))?
                    .contains("`offsetMm` must be a finite, non-zero number")
            );
        }
        assert_eq!(read_document(s.doc()?)?, read_document(&doc)?, "refusals change nothing");
        assert_eq!(s.state()?.undo.len(), undo, "refusals record no undo step");
        Ok(())
    }

    #[test]
    fn machine_offers_the_views_options_renames_and_keeps_on_enter() -> R {
        let (mut s, plan, sec, loose, free) = project()?;
        s.start(CMD)?;
        assert_eq!(s.current_prompt().map(|p| p.keywords), Some(vec!["Name".to_owned(), "Elevation".to_owned()]));
        s.input(Input::Keyword("Name".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter view name <Ground Floor>:"));
        s.input(Input::Text("Ground".into()))?;
        assert!(s.running.is_none());
        assert_eq!(view(&s, plan)?.name(), "Ground");

        s.start(CMD)?;
        s.input(Input::Text("n".into()))?;
        s.input(Input::Enter)?;
        assert!(s.running.is_none());
        assert_eq!(view(&s, plan)?.name(), "Ground", "Enter keeps the name");

        s.start(CMD)?;
        s.input(Input::Text("e".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Specify cut elevation"));
        s.input(Input::Text("1200".into()))?;
        assert_eq!(cut_elevation(&view(&s, plan)?), Some(1200.0));

        let kws = |s: &mut Session, id: Id| -> std::result::Result<Vec<String>, E> {
            s.execute("editview", &json!({ "viewId": id.0 }))?;
            s.start(CMD)?;
            let k = s.current_prompt().map(|p| p.keywords).ok_or("no prompt")?;
            s.input(Input::Cancel)?;
            Ok(k)
        };
        assert_eq!(kws(&mut s, loose)?, ["Name"]);
        assert_eq!(kws(&mut s, free)?, ["Name", "Intent"]);
        assert_eq!(kws(&mut s, sec)?, ["Name", "Intent", "Flip"]);
        Ok(())
    }

    #[test]
    fn machine_flips_at_once_sets_intent_and_refuses_outside_a_view() -> R {
        let (mut s, _plan, sec, ..) = project()?;
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.start(CMD)?;
        s.input(Input::Keyword("Flip".into()))?;
        assert!(s.running.is_none(), "applies at once");
        assert!(s.log.iter().any(|l| l == "A-A flipped."), "{:?}", s.log);
        s.start(CMD)?;
        s.input(Input::Text("f".into()))?;
        assert!(s.log.iter().any(|l| l == "A-A unflipped."), "{:?}", s.log);

        s.start(CMD)?;
        s.input(Input::Keyword("Intent".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter intent or [Section/Elevation] <Section>:"));
        s.input(Input::Text("e".into()))?;
        assert!(matches!(view(&s, sec)?, AnyView::Vertical(v) if v.intent == VerticalIntent::Elevation));

        s.execute("editview", &json!({}))?;
        assert_eq!(s.state()?.space, Space::Model);
        s.start(CMD)?;
        assert!(s.running.is_none());
        assert!(s.log.iter().any(|l| l == "Edit a view first (EDITVIEW)."), "{:?}", s.log);
        Ok(())
    }
}

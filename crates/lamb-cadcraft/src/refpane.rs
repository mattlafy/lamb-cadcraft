//! The reference pane (LAMB T345): the TS app's second canvas showing the edited view's reference
//! view — chosen for you (lamb-core [`find_reference_view_id`]), overridable per edited view
//! (REFPANE `{viewId}`, the Views palette's reference rows, TS `referenceViewByView`).
//!
//! Built on VPORTS tiles: turning it on splits the model area in two (`-vports 2`, horizontal —
//! reference below — or vertical when both views are verticals), the edited view stays in the
//! current tile and [`refresh`] puts the reference into the other one. EDITVIEW (any
//! `commands::enter_space`) refreshes, so the reference follows the edited view; clicking into the
//! other tile does not — that tile is then the one being edited.
//!
//! Session UI state, never saved: whether it is on ([`SHOWN`]) and each edited view's chosen
//! reference ([`CHOSEN`], by block name). A user running VPORTS Single turns it off ([`shown`]).

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use cadcraft_doc::{Drawing, Space};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::derivation::reference::{SplitOrientation, find_eligible_reference_views, find_reference_view_id, split_orientation_for};
use lamb_core::model::{AnyView, Id};
use serde_json::{Value, json};

use crate::commands::{edited_view, id_value, kind_word, other};
use crate::overlays::view_id;
use crate::project::DrawingProject;
use crate::schema::{read_document, view_block_name};

/// REFPANE turned the pane on (and nothing has turned it off).
static SHOWN: AtomicBool = AtomicBool::new(false);
/// Edited view's block name -> its chosen reference view's block name.
static CHOSEN: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

fn chosen() -> std::sync::MutexGuard<'static, BTreeMap<String, String>> {
    CHOSEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The pane is on: REFPANE turned it on and the model area still has exactly two tiles.
pub(crate) fn shown(s: &Session) -> bool {
    SHOWN.load(Ordering::Relaxed) && s.model_tiles().len() == 2
}

/// [`shown`] from the drawing alone (the Views palette has no session): the active VPORTS
/// configuration has two tiles.
pub(crate) fn shown_in(d: &Drawing) -> bool {
    SHOWN.load(Ordering::Relaxed) && d.vports.iter().find(|c| c.is_active()).is_some_and(|c| c.tiles.len() == 2)
}

/// The views eligible as `edit`'s reference, and the one the pane shows: the remembered choice
/// while it is still eligible, else the default. `None` when nothing is eligible.
pub(crate) fn pick(d: &Drawing, edit: Id) -> Option<(Vec<Id>, Id)> {
    let dp = DrawingProject::new(d).ok()?;
    let eligible = find_eligible_reference_views(&dp, edit);
    let remembered = chosen().get(&view_block_name(edit)).and_then(|n| view_id(&Space::Block(n.clone())));
    let r = remembered.filter(|r| eligible.contains(r)).or_else(|| find_reference_view_id(&dp, edit))?;
    Some((eligible, r))
}

fn view_name(d: &Drawing, id: Id) -> String {
    DrawingProject::new(d).ok().and_then(|dp| dp.view(id).map(|v| v.name.to_owned())).unwrap_or_else(|| id.0.to_string())
}

/// How the pane splits for `edit` beside `r`: side by side when both are verticals (they share Z),
/// else stacked (lamb-core [`split_orientation_for`]).
fn orientation(d: &Drawing, edit: Id, r: Id) -> SplitOrientation {
    DrawingProject::new(d).map_or(SplitOrientation::Horizontal, |dp| split_orientation_for(&dp, edit, r))
}

/// The current two-tile split read from the tiles' rects: one x range (stacked) is horizontal,
/// one y range (side by side) is vertical. `None` unless there are exactly two tiles.
fn tiling(s: &Session) -> Option<SplitOrientation> {
    let tiles = s.model_tiles();
    let [a, b] = tiles.as_slice() else { return None };
    let same = |p: f64, q: f64| (p - q).abs() <= 1e-9;
    if same(a.min.x, b.min.x) && same(a.max.x, b.max.x) {
        Some(SplitOrientation::Horizontal)
    } else if same(a.min.y, b.min.y) && same(a.max.y, b.max.y) {
        Some(SplitOrientation::Vertical)
    } else {
        None
    }
}

/// Split the model area into two tiles, `orientation`-wise (VPORTS 2 on the whole display); the
/// current tile keeps its view.
fn split(s: &mut Session, orientation: SplitOrientation) -> Result<()> {
    let arrangement = if orientation == SplitOrientation::Vertical { "vertical" } else { "horizontal" };
    s.execute("-vports", &json!({ "option": "2", "arrangement": arrangement, "applyTo": "display" }))?;
    Ok(())
}

/// While [`shown`], put the edited view's reference (see [`pick`]) into the non-current tile; the
/// current tile keeps its view. When the pair wants the other split ([`orientation`]) the model
/// area is re-split first. No eligible reference: echo
/// `No reference view for <name>.` and leave the tiles alone.
pub(crate) fn refresh(s: &mut Session) -> Result<()> {
    if !shown(s) {
        return Ok(());
    }
    let Some(edit) = edited_view(s) else { return Ok(()) };
    let Some((_, r)) = pick(s.doc()?, edit) else {
        let name = view_name(s.doc()?, edit);
        s.echo(format!("No reference view for {name}."));
        return Ok(());
    };
    let wanted = orientation(s.doc()?, edit, r);
    if tiling(s) != Some(wanted) {
        let edit_space = Space::Block(view_block_name(edit));
        split(s, wanted)?;
        if s.state()?.space != edit_space {
            let st = s.state_mut()?;
            st.space = edit_space;
            st.mspace = None;
        }
    }
    let Some(cur) = s.model_tiles().iter().position(|t| t.active) else { return Ok(()) };
    let other_tile = usize::from(cur == 0);
    let target = Space::Block(view_block_name(r));
    if s.tile_spaces().get(other_tile) == Some(&target) {
        return Ok(());
    }
    s.activate_tile(other_tile)?;
    let st = s.state_mut()?;
    st.space = target;
    st.mspace = None;
    s.activate_tile(cur)
}

/// REFPANE `{on?, viewId?}`: `viewId` (eligible for the edited view) is remembered as its
/// reference and implies on; no `on` toggles. On splits the model area in two when it is not
/// already, then [`refresh`]es; off returns to one tile.
pub(crate) fn refpane(s: &mut Session, p: &Value) -> Result<Value> {
    let view = p.get("viewId").filter(|v| !v.is_null());
    let on = match (view, p.get("on").and_then(Value::as_bool)) {
        (Some(_), _) => true,
        (None, Some(on)) => on,
        (None, None) => !shown(s),
    };
    if let Some(v) = view {
        let edit = edited_view(s).ok_or_else(|| other("Edit a plan or section first (EDITVIEW)."))?;
        let id = id_value(v).ok_or_else(|| other("No such view"))?;
        let d = s.doc()?;
        if !pick(d, edit).is_some_and(|(eligible, _)| eligible.contains(&id)) {
            return Err(other(format!("Not a reference view for {}", view_name(d, edit))));
        }
        chosen().insert(view_block_name(edit), view_block_name(id));
    }
    if !on {
        SHOWN.store(false, Ordering::Relaxed);
        if s.model_tiles().len() > 1 {
            s.execute("-vports", &json!({ "option": "single" }))?;
        }
        return Ok(json!({ "on": false, "message": "<Reference pane off>" }));
    }
    let edit = edited_view(s);
    let reference = edit.and_then(|e| pick(s.doc().ok()?, e).map(|(_, r)| (e, r)));
    if s.model_tiles().len() != 2 {
        let wanted = match reference {
            Some((e, r)) => orientation(s.doc()?, e, r),
            None => SplitOrientation::Horizontal,
        };
        split(s, wanted)?;
    }
    SHOWN.store(true, Ordering::Relaxed);
    refresh(s)?;
    let message = match reference {
        Some((_, r)) => format!("<Reference pane on: {}>", view_name(s.doc()?, r)),
        None => "<Reference pane on>".to_owned(),
    };
    Ok(json!({ "on": true, "viewId": reference.map(|(_, r)| r.0), "message": message }))
}

/// REFPANE: `Enter reference view name or [Off/?] <default name>:`.
struct RefPane {
    default: Option<(Id, String)>,
}

impl RefPane {
    fn run(s: &mut Session, p: &Value) -> Result<Step> {
        match refpane(s, p) {
            Ok(r) => {
                if let Some(m) = r.get("message").and_then(Value::as_str) {
                    s.echo(m.to_owned());
                }
            }
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

impl Interactive for RefPane {
    fn name(&self) -> &'static str {
        "refpane"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        let p = Prompt::new("Enter reference view name", Accept::TEXT).kw(&["Off", "?"]);
        match &self.default {
            Some((_, name)) => p.default(name.clone()),
            None => p,
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let text = match i {
            Input::Cancel => return Ok(Step::Cancel),
            Input::Enter => String::new(),
            Input::Text(t) | Input::Keyword(t) => t.trim().to_owned(),
            _ => return Ok(Step::Continue),
        };
        if text.is_empty() {
            return match self.default {
                Some((id, _)) => Self::run(s, &json!({ "viewId": id.0 })),
                None => Self::run(s, &json!({ "on": true })),
            };
        }
        if text.eq_ignore_ascii_case("off") {
            return Self::run(s, &json!({ "on": false }));
        }
        let edit = edited_view(s).ok_or_else(|| other("Edit a plan or section first (EDITVIEW)."))?;
        let eligible = pick(s.doc()?, edit).map(|(e, _)| e).unwrap_or_default();
        let doc = read_document(s.doc()?).map_err(other)?;
        let views: Vec<&AnyView> = eligible.iter().filter_map(|id| doc.views.iter().find(|v| v.id() == *id)).collect();
        if text == "?" {
            for v in &views {
                s.echo(format!("{}  {}  {}", v.id().0, kind_word(v), v.name()));
            }
            return Ok(Step::Continue);
        }
        let by_name = views.iter().find(|v| v.name().eq_ignore_ascii_case(&text));
        let by_id = || text.parse::<u64>().ok().and_then(|n| views.iter().find(|v| v.id() == Id(n)));
        match by_name.or_else(by_id) {
            Some(v) => Self::run(s, &json!({ "viewId": v.id().0 })),
            None => Err(other(format!("Not a reference view for {}", view_name(s.doc()?, edit)))),
        }
    }
}

pub(crate) fn refpane_machine(s: &Session) -> Result<Box<dyn Interactive>> {
    let default = edited_view(s).and_then(|e| {
        let d = s.doc().ok()?;
        let (_, r) = pick(d, e)?;
        Some((r, view_name(d, r)))
    });
    Ok(Box::new(RefPane { default }))
}

/// Tests that touch the process-wide pane state hold this, and start from [`reset`].
#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn reset() {
    SHOWN.store(false, Ordering::Relaxed);
    chosen().clear();
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::commands::install_app;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type E = Box<dyn std::error::Error>;

    pub(crate) fn lock() -> std::sync::MutexGuard<'static, ()> {
        let g = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        g
    }

    /// A LAMB project: the placed `Ground Floor` plan (edited) and sections `A-A`, `B-B` on it.
    pub(crate) fn project() -> std::result::Result<(Session, Id, Id, Id), E> {
        install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no plan")?);
        let mut sec = |name: &str, y: f64| -> std::result::Result<Id, E> {
            let r = s.execute(
                "newsectionline",
                &json!({ "name": name, "hostViewId": plan.0, "cutFrom": {"x": 0, "y": y}, "cutTo": {"x": 5000, "y": y} }),
            )?;
            Ok(Id(r["viewId"].as_u64().ok_or("no section")?))
        };
        let a = sec("A-A", 1000.0)?;
        let b = sec("B-B", 2000.0)?;
        Ok((s, plan, a, b))
    }

    fn block(id: Id) -> Space {
        Space::Block(view_block_name(id))
    }

    fn active(s: &Session) -> Option<usize> {
        s.model_tiles().iter().position(|t| t.active)
    }

    #[test]
    fn refpane_shows_follows_remembers_and_turns_off() -> R {
        let _g = lock();
        let (mut s, plan, a, b) = project()?;
        let r = s.execute("refpane", &json!({ "on": true }))?;
        assert_eq!(r["message"], json!("<Reference pane on: A-A>"));
        assert_eq!(s.model_tiles().len(), 2);
        assert_eq!(s.tile_spaces(), [block(plan), block(a)], "the plan stays current, its first section below");
        assert_eq!(active(&s), Some(0));
        let tiles = s.model_tiles();
        assert!(tiles[1].max.y <= tiles[0].min.y + 1e-9, "horizontal: the reference is below");

        s.execute("refpane", &json!({ "viewId": b.0 }))?;
        assert_eq!(s.tile_spaces(), [block(plan), block(b)]);

        s.execute("editview", &json!({ "viewId": a.0 }))?;
        assert_eq!(s.tile_spaces(), [block(a), block(plan)], "a section's reference is its host");

        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        assert_eq!(s.tile_spaces(), [block(plan), block(b)], "the plan's choice is remembered");

        let e = s.execute("refpane", &json!({ "viewId": plan.0 })).err().ok_or("accepted an ineligible view")?;
        assert!(e.to_string().contains("Not a reference view for Ground Floor"), "{e}");

        let r = s.execute("refpane", &json!({ "on": false }))?;
        assert_eq!(r["on"], json!(false));
        assert!(s.model_tiles().len() < 2);
        assert!(!shown(&s));

        s.execute("refpane", &json!({}))?;
        assert!(shown(&s), "no `on` toggles");
        assert_eq!(s.tile_spaces(), [block(plan), block(b)]);
        Ok(())
    }

    #[test]
    fn two_sections_go_side_by_side_and_single_turns_it_off() -> R {
        let _g = lock();
        let (mut s, plan, a, b) = project()?;
        s.execute("editview", &json!({ "viewId": a.0 }))?;
        s.execute("refpane", &json!({ "viewId": b.0 }))?;
        let tiles = s.model_tiles();
        assert_eq!(tiles.len(), 2);
        assert!(tiles[0].max.x <= tiles[1].min.x + 1e-9 || tiles[1].max.x <= tiles[0].min.x + 1e-9, "side by side: {tiles:?}");
        assert_eq!(s.tile_spaces(), [block(a), block(b)]);

        s.execute("-vports", &json!({ "option": "single" }))?;
        assert!(!shown(&s));
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        assert!(s.model_tiles().len() < 2, "an off pane splits nothing");
        Ok(())
    }

    fn stacked(s: &Session) -> bool {
        tiling(s) == Some(SplitOrientation::Horizontal)
    }

    #[test]
    fn the_split_follows_the_edited_pair() -> R {
        let _g = lock();
        let (mut s, plan, a, b) = project()?;
        s.execute("refpane", &json!({ "on": true }))?;
        assert!(stacked(&s), "plan over section: {:?}", s.model_tiles());

        s.execute("editview", &json!({ "viewId": a.0 }))?;
        assert!(stacked(&s), "section over its host plan");
        s.execute("refpane", &json!({ "viewId": b.0 }))?;
        assert_eq!(tiling(&s), Some(SplitOrientation::Vertical), "section beside section: {:?}", s.model_tiles());
        let cur = active(&s).ok_or("no current tile")?;
        assert_eq!(s.tile_spaces()[cur], block(a), "the edited section stays current");
        assert_eq!(s.tile_spaces()[usize::from(cur == 0)], block(b));
        assert!(shown(&s));

        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        assert!(stacked(&s), "back to stacked for the plan: {:?}", s.model_tiles());
        let cur = active(&s).ok_or("no current tile")?;
        assert_eq!(s.tile_spaces()[cur], block(plan));
        assert_eq!(s.tile_spaces()[usize::from(cur == 0)], block(a));
        Ok(())
    }

    #[test]
    fn the_prompt_defaults_lists_picks_and_turns_off() -> R {
        let _g = lock();
        let (mut s, plan, a, b) = project()?;
        s.start("refpane")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter reference view name or [Off/?] <A-A>:"));
        s.input(Input::Keyword("?".into()))?;
        assert!(s.log.iter().any(|l| l == &format!("{}  Section  B-B", b.0)), "{:?}", s.log);
        let n = s.log.len();
        s.input(Input::Text("Ground Floor".into()))?;
        assert!(s.log[n..].iter().any(|l| l.contains("Not a reference view for Ground Floor")), "{:?}", &s.log[n..]);
        assert!(s.running.is_some(), "re-prompts");
        s.input(Input::Text("b-b".into()))?;
        assert!(s.running.is_none());
        assert_eq!(s.tile_spaces(), [block(plan), block(b)]);
        assert!(s.log.iter().any(|l| l == "<Reference pane on: B-B>"), "{:?}", s.log);

        s.start("refpane")?;
        assert_eq!(s.current_prompt().and_then(|p| p.default).as_deref(), Some("B-B"), "the remembered choice is the default");
        s.input(Input::Keyword("Off".into()))?;
        assert!(!shown(&s));

        s.execute("editview", &json!({ "viewId": a.0 }))?;
        s.start("refpane")?;
        s.input(Input::Enter)?;
        assert_eq!(s.tile_spaces(), [block(a), block(plan)], "Enter takes the default");
        Ok(())
    }
}

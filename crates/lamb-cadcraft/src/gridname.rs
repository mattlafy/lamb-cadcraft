//! GRIDNAME (LAMB T704): an explicit name for one building grid.
//!
//! A name is a [`GridNameRecord`] on one datum segment of the grid. lamb-core's
//! [`named_grids`] gives it to every coincident member, so every bubble on every plan, section,
//! elevation and sheet that shows the grid reads it ([`crate::annotations`], [`crate::legend`] pass
//! [`overrides`]). Explicit names are unique, compared ignoring case; a grid has at most one
//! record. The automatic sequence skips every explicit name, so naming one grid can renumber the
//! automatic ones after it: names are read live and nothing stores them. A record whose entity or
//! segment is gone matches no grid and is ignored.
//!
//! - `gridname {viewId?, entityId, segmentIndex, name}` or `{grid, name}` (`gn`): `grid` is a grid's
//!   current name, ignoring case; the other form names the grid that datum segment `segmentIndex`
//!   of `entityId` in `viewId` (default: the edited view) draws. `name: ""` returns the grid to
//!   automatic naming. Refuses a segment that draws no grid, an unknown grid and a name another
//!   grid has (`Grid <name> already exists`). One undo step. Echoes and returns
//!   `{grid, name, message}`: `grid` the name before, `name` the name now; `Grid 3 is now B.` /
//!   `Grid B is automatic again (3).`
//! - Interactive: `Select grid line or [Name]:` — a click picks a line of the edited view; `N`, or a
//!   typed grid name, picks by name (`Grid name:`). Then `New grid name <current>:`; Enter keeps it.
//!   Refusals echo and re-prompt.

use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use lamb_core::derivation::datums::{GridMember, NamedGrid, named_grids};
use serde_json::{Value, json};

use crate::commands::{bad, edited_view, id_value, other};
use crate::linkcmds::pick_line;
use crate::project::DrawingProject;
use crate::schema::{GridNameRecord, LambDocument, read_document, write_document};

const CMD: &str = "gridname";

/// The document's explicit grid names as [`named_grids`] takes them.
pub(crate) fn overrides(doc: &LambDocument) -> Vec<(GridMember, String)> {
    doc.grid_names
        .iter()
        .map(|r| (GridMember { view_id: r.view_id, entity_id: r.entity_id, segment_index: r.segment_index }, r.name.clone()))
        .collect()
}

/// Every grid of the drawing, named as it reads now.
fn grids(s: &Session) -> Result<Vec<NamedGrid>> {
    let dp = DrawingProject::new(s.doc()?).map_err(other)?;
    named_grids(&dp, &overrides(dp.document())).map_err(other)
}

/// The grid named `name`, ignoring case.
fn by_name<'g>(grids: &'g [NamedGrid], name: &str) -> Option<&'g NamedGrid> {
    grids.iter().find(|g| g.name.eq_ignore_ascii_case(name.trim()))
}

/// The grid datum segment `m` draws.
fn by_member(grids: &[NamedGrid], m: GridMember) -> Option<&NamedGrid> {
    grids.iter().find(|g| g.members.contains(&m))
}

const NOT_A_GRID: &str = "That line is not a grid line: give it the datum role (SETROLE) first";

/// GRIDNAME's JSON form. See the module doc.
pub(crate) fn gridname(s: &mut Session, p: &Value) -> Result<Value> {
    let name = p.get("name").and_then(Value::as_str).ok_or_else(|| bad(CMD, "`name` is required"))?.trim().to_owned();
    let all = grids(s)?;
    let (grid, member) = match p.get("grid") {
        Some(Value::String(g)) => {
            let grid = by_name(&all, g).ok_or_else(|| other(format!("No grid named {}", g.trim())))?;
            (grid, None)
        }
        Some(_) => return Err(bad(CMD, "`grid` must be a grid name")),
        None => {
            let view_id = match p.get("viewId") {
                None | Some(Value::Null) => edited_view(s).ok_or_else(|| other("Edit a view first (EDITVIEW), or give `viewId`"))?,
                Some(v) => id_value(v).ok_or_else(|| bad(CMD, "`viewId` must be a view id"))?,
            };
            let entity_id = p.get("entityId").and_then(id_value).ok_or_else(|| bad(CMD, "`entityId` or `grid` is required"))?;
            let segment_index = match p.get("segmentIndex") {
                None | Some(Value::Null) => 0,
                Some(v) => v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad(CMD, "`segmentIndex` must be a whole number"))?,
            };
            let m = GridMember { view_id, entity_id, segment_index };
            (by_member(&all, m).ok_or_else(|| other(NOT_A_GRID))?, Some(m))
        }
    };
    let was = grid.name.clone();
    let members = grid.members.clone();
    let mut doc = read_document(s.doc()?).map_err(|e| other(e.to_string()))?;
    let mine = |r: &GridNameRecord| members.contains(&GridMember { view_id: r.view_id, entity_id: r.entity_id, segment_index: r.segment_index });
    let message = if name.is_empty() {
        if !doc.grid_names.iter().any(mine) {
            format!("Grid {was} is already automatic.")
        } else {
            doc.grid_names.retain(|r| !mine(r));
            write_document(s.doc_mut()?, &doc).map_err(|e| other(e.to_string()))?;
            let now = grids(s)?;
            let auto = members.first().and_then(|m| by_member(&now, *m)).map(|g| g.name.clone()).unwrap_or_default();
            format!("Grid {was} is automatic again ({auto}).")
        }
    } else {
        if all.iter().any(|g| g.members != members && g.name.eq_ignore_ascii_case(&name)) {
            return Err(other(format!("Grid {name} already exists")));
        }
        let at = member.or_else(|| members.first().copied()).ok_or_else(|| other(NOT_A_GRID))?;
        doc.grid_names.retain(|r| !mine(r));
        doc.grid_names.push(GridNameRecord { view_id: at.view_id, entity_id: at.entity_id, segment_index: at.segment_index, name: name.clone() });
        write_document(s.doc_mut()?, &doc).map_err(|e| other(e.to_string()))?;
        format!("Grid {was} is now {name}.")
    };
    let now = members.first().and_then(|m| grids(s).ok().and_then(|g| by_member(&g, *m).map(|g| g.name.clone()))).unwrap_or_default();
    s.echo(message.clone());
    Ok(json!({ "grid": was, "name": now, "message": message }))
}

/// Which grid GRIDNAME's prompts have got to.
enum Stage {
    Pick,
    ByName,
    /// The grid by its current name, and that name.
    Rename(Value, String),
}

struct GridNameM {
    stage: Stage,
}

const NAME_KW: &str = "Name";

impl Interactive for GridNameM {
    fn name(&self) -> &'static str {
        CMD
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match &self.stage {
            Stage::Pick => {
                Prompt::new("Select grid line", Accept { point: true, number: false, text: true, select: false, enter: true }).kw(&[NAME_KW])
            }
            Stage::ByName => Prompt::new("Grid name", Accept::TEXT),
            Stage::Rename(_, current) => Prompt::new("New grid name", Accept::TEXT).default(current.clone()),
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        s.echo("GRIDNAME: names a building grid; every bubble on every plan, section, elevation and sheet reads it.");
        if edited_view(s).is_none() {
            s.echo("GRIDNAME picks grid lines in a view: open one from the Views palette or with EDITVIEW, or type a grid's name.");
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        match std::mem::replace(&mut self.stage, Stage::Pick) {
            Stage::Pick => match i {
                Input::Enter => return Ok(Step::Done),
                Input::Keyword(k) if k.eq_ignore_ascii_case(NAME_KW) => self.stage = Stage::ByName,
                Input::Text(t) if t.trim().eq_ignore_ascii_case("n") || t.trim().eq_ignore_ascii_case(NAME_KW) => self.stage = Stage::ByName,
                Input::Text(t) | Input::Keyword(t) => self.stage = self.named(s, &t),
                Input::Point(p) => {
                    let all = grids(s)?;
                    let hit = edited_view(s).zip(pick_line(s, p)).and_then(|(view, h)| {
                        let m = GridMember { view_id: view, entity_id: h.entity, segment_index: h.segment_index };
                        by_member(&all, m).map(|g| (m, g.name.clone()))
                    });
                    match hit {
                        Some((m, current)) => {
                            let target = json!({ "viewId": m.view_id.0, "entityId": m.entity_id.0, "segmentIndex": m.segment_index });
                            self.stage = Stage::Rename(target, current);
                        }
                        None => s.echo(if pick_line(s, p).is_some() { NOT_A_GRID } else { "No grid line there" }),
                    }
                }
                _ => {}
            },
            Stage::ByName => match i {
                Input::Enter => return Ok(Step::Done),
                Input::Text(t) | Input::Keyword(t) => {
                    self.stage = self.named(s, &t);
                    if matches!(self.stage, Stage::Pick) {
                        self.stage = Stage::ByName;
                    }
                }
                _ => self.stage = Stage::ByName,
            },
            Stage::Rename(target, current) => {
                let typed = match i {
                    Input::Enter => return Ok(Step::Done),
                    Input::Text(t) | Input::Keyword(t) => t.trim().to_owned(),
                    _ => {
                        self.stage = Stage::Rename(target, current);
                        return Ok(Step::Continue);
                    }
                };
                if typed.is_empty() || typed == current {
                    return Ok(Step::Done);
                }
                let mut params = target.clone();
                params["name"] = json!(typed);
                match gridname(s, &params) {
                    Ok(_) => return Ok(Step::Done),
                    Err(e) => {
                        s.echo(e.to_string());
                        self.stage = Stage::Rename(target, current);
                    }
                }
            }
        }
        Ok(Step::Continue)
    }
}

impl GridNameM {
    /// The rename stage for the grid called `t`, or (echoing why) back to picking.
    fn named(&self, s: &mut Session, t: &str) -> Stage {
        match grids(s).ok().as_deref().and_then(|g| by_name(g, t)) {
            Some(g) => Stage::Rename(json!({ "grid": g.name }), g.name.clone()),
            None => {
                s.echo(format!("No grid named {}", t.trim()));
                Stage::Pick
            }
        }
    }
}

pub(crate) fn gridname_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(GridNameM { stage: Stage::Pick }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotations::tests::{bubbles_of, datum_line, gridded};
    use cadcraft_geom::Vec2;
    use lamb_core::derivation::read::ProjectRead;
    use lamb_core::model::Id;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    /// [`gridded`] with N–S datums at x = 1000 and 3000 too: grids `1 2 3` (the middle one the
    /// fixture's) and `A`. Returns (session, plan, upper, section).
    fn three() -> T<(Session, Id, Id, Id)> {
        let (mut s, plan, upper, sec, _) = gridded()?;
        datum_line(&mut s, plan, [1000.0, -2000.0], [1000.0, 3000.0])?;
        datum_line(&mut s, plan, [3000.0, -2000.0], [3000.0, 3000.0])?;
        Ok((s, plan, upper, sec))
    }

    /// The names of `view`'s bubbles on the N–S grids at x = 1000, 2000 and 3000 (plan) or, in a
    /// section, its bubbles west to east.
    fn names(s: &Session, view: Id, section: bool) -> T<Vec<String>> {
        let mut b = bubbles_of(s, view)?;
        if section {
            b.sort_by(|a, b| a.0.x.total_cmp(&b.0.x));
            return Ok(b.into_iter().map(|b| b.2).collect());
        }
        let at = |x: f64| -> T<String> {
            let mut n: Vec<_> = b.iter().filter(|b| (b.0.x - x).abs() < 1e-6).map(|b| b.2.clone()).collect();
            n.dedup();
            if n.len() != 1 {
                return Err(format!("bubbles at x={x}: {n:?}").into());
            }
            Ok(n.remove(0))
        };
        Ok(vec![at(1000.0)?, at(2000.0)?, at(3000.0)?])
    }

    fn all(s: &Session, plan: Id, upper: Id, sec: Id) -> T<[Vec<String>; 3]> {
        Ok([names(s, plan, false)?, names(s, upper, false)?, names(s, sec, true)?])
    }

    fn want(n: [&str; 3]) -> [Vec<String>; 3] {
        let v: Vec<String> = n.iter().map(|s| s.to_string()).collect();
        [v.clone(), v.clone(), v]
    }

    #[test]
    fn a_named_grid_reads_its_name_in_every_view_and_undo_restores_it() -> R {
        let (mut s, plan, upper, sec) = three()?;
        assert_eq!(all(&s, plan, upper, sec)?, want(["1", "2", "3"]));
        let n = s.state()?.undo.len();
        let r = s.execute(CMD, &json!({ "grid": "2", "name": "B" }))?;
        assert_eq!(r["message"], "Grid 2 is now B.");
        assert_eq!(s.state()?.undo.len(), n + 1, "one undo step");
        assert_eq!(all(&s, plan, upper, sec)?, want(["1", "B", "2"]));
        assert!(crate::annotations::settled(s.doc()?)?);
        assert_eq!(read_document(s.doc()?)?.grid_names.len(), 1);

        s.undo()?;
        assert_eq!(all(&s, plan, upper, sec)?, want(["1", "2", "3"]));
        assert!(read_document(s.doc()?)?.grid_names.is_empty());
        s.redo()?;
        assert_eq!(all(&s, plan, upper, sec)?, want(["1", "B", "2"]));

        // Explicit names are unique ignoring case.
        let e = s.execute(CMD, &json!({ "grid": "1", "name": "b" })).err().ok_or("no refusal")?;
        assert!(e.to_string().contains("Grid b already exists"), "{e}");
        assert_eq!(all(&s, plan, upper, sec)?, want(["1", "B", "2"]));

        // Renaming replaces the grid's one record; "" makes it automatic again.
        s.execute(CMD, &json!({ "grid": "b", "name": "2.5" }))?;
        assert_eq!(read_document(s.doc()?)?.grid_names.len(), 1);
        let r = s.execute(CMD, &json!({ "grid": "2.5", "name": "" }))?;
        assert_eq!(r["message"], "Grid 2.5 is automatic again (2).");
        assert_eq!(all(&s, plan, upper, sec)?, want(["1", "2", "3"]));
        assert!(read_document(s.doc()?)?.grid_names.is_empty());
        Ok(())
    }

    #[test]
    fn a_line_that_is_not_a_datum_is_refused() -> R {
        let (mut s, plan, _upper, _sec) = three()?;
        let ids = |s: &Session| -> T<Vec<Id>> { Ok(DrawingProject::new(s.doc()?)?.entities(plan).map(|e| e.id).collect()) };
        let before = ids(&s)?;
        s.execute("line", &json!({ "points": [[0, 6000], [500, 6000]] }))?;
        let id = ids(&s)?.into_iter().find(|i| !before.contains(i)).ok_or("no line")?;
        let e = s.execute(CMD, &json!({ "viewId": plan.0, "entityId": id.0, "segmentIndex": 0, "name": "Z" })).err().ok_or("no refusal")?;
        assert!(e.to_string().contains("not a grid line"), "{e}");
        assert!(read_document(s.doc()?)?.grid_names.is_empty());
        Ok(())
    }

    #[test]
    fn the_name_survives_a_dxf_round_trip() -> R {
        let (mut s, _plan, _upper, _sec) = three()?;
        s.execute(CMD, &json!({ "grid": "3", "name": "C" }))?;
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert_eq!(read_document(&back)?.grid_names, read_document(s.doc()?)?.grid_names);
        let dp = DrawingProject::new(&back)?;
        let names: Vec<String> = named_grids(&dp, &overrides(dp.document()))?.into_iter().map(|g| g.name).collect();
        assert_eq!(names, ["1", "2", "C", "A"]);
        assert!(crate::annotations::settled(&back)?, "the bubbles read C after the round trip");
        Ok(())
    }

    fn prompt(s: &Session) -> Option<String> {
        s.current_prompt().map(|p| p.display())
    }

    #[test]
    fn the_machine_names_a_clicked_grid_or_one_typed_by_name() -> R {
        let (mut s, plan, upper, sec) = three()?;
        s.start(CMD)?;
        assert_eq!(prompt(&s).as_deref(), Some("Select grid line or [Name]:"));
        s.input(Input::Point(Vec2::new(-3000.0, -3000.0)))?;
        assert_eq!(prompt(&s).as_deref(), Some("Select grid line or [Name]:"), "a miss re-prompts");
        s.input(Input::Point(Vec2::new(2000.0, 500.0)))?;
        assert_eq!(prompt(&s).as_deref(), Some("New grid name <2>:"));
        s.input(Input::Text("1".into()))?;
        assert!(s.log.iter().any(|l| l.contains("Grid 1 already exists")), "{:?}", s.log);
        assert_eq!(prompt(&s).as_deref(), Some("New grid name <2>:"), "a clash re-prompts");
        s.input(Input::Text("B".into()))?;
        assert!(prompt(&s).is_none());
        assert_eq!(all(&s, plan, upper, sec)?, want(["1", "B", "2"]));
        let rec = read_document(s.doc()?)?.grid_names;
        assert_eq!((rec.len(), rec[0].view_id), (1, plan));

        // By typed name, and Enter keeps the name.
        s.start(CMD)?;
        s.input(Input::Text("b".into()))?;
        assert_eq!(prompt(&s).as_deref(), Some("New grid name <B>:"));
        s.input(Input::Enter)?;
        assert!(prompt(&s).is_none());
        s.start(CMD)?;
        s.input(Input::Keyword(NAME_KW.into()))?;
        assert_eq!(prompt(&s).as_deref(), Some("Grid name:"));
        s.input(Input::Text("3".into()))?;
        assert!(s.log.iter().any(|l| l == "No grid named 3"), "{:?}", s.log);
        s.input(Input::Text("2".into()))?;
        s.input(Input::Text("4".into()))?;
        assert_eq!(all(&s, plan, upper, sec)?, want(["1", "B", "4"]));
        Ok(())
    }
}

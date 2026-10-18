//! Outside pick points (LAMB's `docs/07-architecture/cadcraft-integration.md` §8 "Inside and
//! outside", override 1): a CADCraft `Point` in a plan or section view's block carrying
//! [`EntityData::outside`] marks the region around it outside the building. Every such point feeds
//! [`SideParams::outside_points`](lamb_core::derivation::wall_spaces::SideParams) through
//! [`crate::detailtags::graph_and_sides`], so detail-tag resolution, ASSEMBLYPLAN, WALLCONFLICTS and
//! WALLFIT all see it. DXF/DWG see a plain point.
//!
//! - `outsidepoint {point:{x,y}}` (View ▸ LAMB ▸ Outside Point): one tagged point, on the current
//!   layer, in the edited plan or section view; one undo step. Returns `{"handle": <hex>}`. Interactive:
//!   `Specify a point outside the building:`.

use cadcraft_doc::{Drawing, EntityKind, Point};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::Vec2;
use lamb_core::geometry::Point2;
use lamb_core::model::Id;
use serde_json::{Value, json};

use crate::commands::{other, point_param};
use crate::detailtags::edited_wall_view;
use crate::schema::{EntityData, read_entity_data, set_entity_data, view_block_name};

/// Every outside pick point in `view`'s block; a point with malformed LAMB data is skipped.
pub(crate) fn outside_points(d: &Drawing, view: Id) -> Vec<Point2> {
    let Some(block) = d.block(&view_block_name(view)) else { return Vec::new() };
    block
        .entities
        .iter()
        .filter_map(|e| {
            let EntityKind::Point(pt) = &e.kind else { return None };
            read_entity_data(&e.common).ok().filter(|data| data.outside).map(|_| Point2 { x: pt.p.x, y: pt.p.y })
        })
        .collect()
}

/// OUTSIDEPOINT's JSON form: a tagged point at `point` in the edited plan or section view.
pub(crate) fn outsidepoint(s: &mut Session, p: &Value) -> Result<Value> {
    let at = point_param("outsidepoint", p, "point")?;
    // Every refusal before `doc_mut`, so a refusal records nothing.
    edited_wall_view(s).map_err(other)?;
    let mut common = s.current_common();
    set_entity_data(&mut common, &EntityData { outside: true, ..EntityData::default() }).map_err(other)?;
    let space = s.space();
    let h = s.doc_mut()?.add(&space, common, EntityKind::Point(Point { p: Vec2::new(at.x, at.y).to3(0.0), angle: 0.0 }))?;
    Ok(json!({ "handle": format!("{:X}", h.0) }))
}

/// OUTSIDEPOINT: one point prompt.
struct OutsidePointM;

impl Interactive for OutsidePointM {
    fn name(&self) -> &'static str {
        "outsidepoint"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Specify a point outside the building", Accept::POINT)
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if let Err(e) = edited_wall_view(s) {
            s.echo(e);
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Point(p) => {
                if let Err(e) = outsidepoint(s, &json!({ "point": {"x": p.x, "y": p.y} })) {
                    s.echo(e.to_string());
                }
                Ok(Step::Done)
            }
            Input::Cancel | Input::Enter => Ok(Step::Cancel),
            _ => Ok(Step::Continue),
        }
    }
}

pub(crate) fn outsidepoint_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(OutsidePointM))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detailtags::graph_and_sides;
    use crate::project::DrawingProject;
    use lamb_core::derivation::wall_spaces::{SideKind, WallSides};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    /// A plan with a closed 5000 × 4000 room of 200 mm walls on `WALL-EXT` (outer face 0..5000 ×
    /// 0..4000) and a detail view `D1`, editing the plan on layer `MARKS`. Returns (session, plan, D1).
    fn room() -> T<(Session, Id, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        let detail = Id(s.execute("newdetail", &json!({ "name": "D1" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "WALL-EXT", "current": true }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [5000, 0], [5000, 4000], [0, 4000]], "closed": true }))?;
        s.execute("pline", &json!({ "vertices": [[200, 200], [4800, 200], [4800, 3800], [200, 3800]], "closed": true }))?;
        s.execute("layer.new", &json!({ "name": "MARKS", "current": true }))?;
        Ok((s, plan, detail))
    }

    fn sides_of(d: &Drawing, plan: Id) -> T<WallSides> {
        Ok(graph_and_sides(&DrawingProject::new(d)?, plan).1)
    }

    fn any_space(w: &WallSides) -> bool {
        w.runs.iter().any(|r| matches!(r.left, SideKind::Space(_)) || matches!(r.right, SideKind::Space(_)))
    }

    fn mark(s: &mut Session, x: f64, y: f64) -> Result<Value> {
        s.execute("outsidepoint", &json!({ "point": {"x": x, "y": y} }))
    }

    fn points(s: &Session, plan: Id) -> T<Vec<cadcraft_doc::Entity>> {
        let b = s.doc()?.block(&view_block_name(plan)).ok_or("no block")?.clone();
        Ok(b.entities.iter().filter(|e| matches!(e.kind, EntityKind::Point(_))).map(|e| (**e).clone()).collect())
    }

    #[test]
    fn a_point_inside_the_room_makes_the_courtyard_outside_in_one_undo_step() -> R {
        let (mut s, plan, _) = room()?;
        let before = sides_of(s.doc()?, plan)?;
        assert!(any_space(&before));
        let undo = s.state()?.undo.len();
        let h = mark(&mut s, 2500.0, 2000.0)?["handle"].as_str().ok_or("no handle")?.to_owned();
        let after = sides_of(s.doc()?, plan)?;
        assert!(!any_space(&after), "{after:?}");
        assert_eq!(s.state()?.undo.len(), undo + 1);
        let p = points(&s, plan)?;
        assert_eq!(p.len(), 1);
        assert_eq!(format!("{:X}", p[0].handle.0), h);
        assert!(read_entity_data(&p[0].common)?.outside);
        assert_eq!(outside_points(s.doc()?, plan), [Point2 { x: 2500.0, y: 2000.0 }]);
        Ok(())
    }

    #[test]
    fn a_point_already_outside_changes_nothing() -> R {
        let (mut s, plan, _) = room()?;
        let before = sides_of(s.doc()?, plan)?;
        mark(&mut s, -1000.0, -1000.0)?;
        assert_eq!(sides_of(s.doc()?, plan)?, before);
        Ok(())
    }

    #[test]
    fn erase_and_undo_restore_the_sides_and_the_tag_survives_dxf() -> R {
        let (mut s, plan, _) = room()?;
        let before = sides_of(s.doc()?, plan)?;
        let h = mark(&mut s, 2500.0, 2000.0)?["handle"].as_str().ok_or("no handle")?.to_owned();
        let marked = sides_of(s.doc()?, plan)?;

        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert_eq!(outside_points(&back, plan), outside_points(s.doc()?, plan));
        assert_eq!(sides_of(&back, plan)?, marked);

        s.undo()?;
        assert_eq!(sides_of(s.doc()?, plan)?, before);
        s.redo()?;
        assert_eq!(sides_of(s.doc()?, plan)?, marked);
        s.execute("erase", &json!({ "handles": [h] }))?;
        assert!(points(&s, plan)?.is_empty());
        assert_eq!(sides_of(s.doc()?, plan)?, before);
        Ok(())
    }

    #[test]
    fn outside_a_plan_or_section_view_it_refuses_and_adds_nothing() -> R {
        let (mut s, _plan, detail) = room()?;
        s.execute("editview", &json!({}))?;
        let n = s.doc()?.model.len();
        assert!(mark(&mut s, 2500.0, 2000.0).is_err_and(|e| e.to_string().contains("Edit a plan or section view first")));
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        assert!(mark(&mut s, 2500.0, 2000.0).is_err_and(|e| e.to_string().contains("Detail tags live in plan and section views")));
        assert_eq!(s.doc()?.model.len(), n);
        assert!(s.doc()?.block(&view_block_name(detail)).ok_or("no block")?.entities.is_empty());

        s.execute("editview", &json!({}))?;
        s.start("outsidepoint")?;
        assert!(s.running.is_none(), "refused at once");
        assert!(s.log.iter().any(|l| l == "Edit a plan or section view first (EDITVIEW)"), "{:?}", s.log);
        Ok(())
    }

    #[test]
    fn the_interactive_form_places_the_same_point_as_the_parameter_form() -> R {
        let (mut s, plan, _) = room()?;
        s.start("outsidepoint")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Specify a point outside the building:"));
        s.input(Input::Point(Vec2::new(2500.0, 2000.0)))?;
        assert!(s.running.is_none());
        mark(&mut s, 2500.0, 2000.0)?;
        let p = points(&s, plan)?;
        assert_eq!(p.len(), 2);
        assert_eq!((&p[0].kind, &p[0].common.layer, &p[0].common.ext), (&p[1].kind, &p[1].common.layer, &p[1].common.ext));
        Ok(())
    }
}

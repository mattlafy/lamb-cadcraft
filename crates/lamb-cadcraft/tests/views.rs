//! Views as blocks: `newplan` / `newsectionline` create block + layer + canonical insert +
//! document entry in one undo step, `editview` lets CADCraft's own commands draw inside a view,
//! and a DXF save/reload keeps views and their entities (the §13 phase-2 gate).
//!
//! The command registry is process-global, so this binary has one test fn that registers.

use cadcraft_doc::{Drawing, EntityKind, Handle, Space};
use cadcraft_engine::{Session, register_command_provider};
use cadcraft_io::{read_dxf, write_dxf};
use lamb_cadcraft::{DrawingProject, commands, view_block_name};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::geometry::Polyline2;
use lamb_core::model::{AnyView, Id, Placement, VerticalIntent};
use serde_json::{Value, json};

type R = Result<(), Box<dyn std::error::Error>>;

fn view_id(v: &Value) -> Result<Id, String> {
    v.get("viewId").and_then(Value::as_u64).map(Id).ok_or_else(|| format!("no viewId in {v}"))
}

fn views(d: &Drawing) -> Result<Vec<AnyView>, String> {
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    Ok(p.document().views.clone())
}

/// (entity id, polyline) per view, in view order.
fn entities(d: &Drawing) -> Result<Vec<(Id, Vec<(Id, Option<Polyline2>)>)>, String> {
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    let ids: Vec<Id> = p.views().map(|v| v.id).collect();
    Ok(ids.into_iter().map(|v| (v, p.entities(v).map(|e| (e.id, e.polyline.map(|c| c.into_owned()))).collect())).collect())
}

fn view_entity_ids(d: &Drawing, view: Id) -> Result<Vec<Id>, String> {
    let p = DrawingProject::new(d).map_err(|e| e.to_string())?;
    Ok(p.entities(view).map(|e| e.id).collect())
}

fn block_members(d: &Drawing, view: Id) -> Vec<Handle> {
    d.block(&view_block_name(view)).map(|b| b.entities.iter().map(|e| e.handle).collect()).unwrap_or_default()
}

fn check_view_created(d: &Drawing, id: Id) -> Result<(), String> {
    let name = view_block_name(id);
    if d.block(&name).is_none() {
        return Err(format!("no block {name}"));
    }
    if d.layer(&name).is_none() {
        return Err(format!("no layer {name}"));
    }
    let ins = d.model.get(Handle(id.0)).ok_or("no model entity with the view id's handle")?;
    match &ins.kind {
        EntityKind::Insert(i) if i.block == name && ins.common.layer == name => Ok(()),
        k => Err(format!("canonical insert wrong: {} on {}", k.type_name(), ins.common.layer)),
    }
}

#[test]
fn view_commands_and_dxf_round_trip() -> R {
    register_command_provider(commands)?;
    let mut s = Session::new();
    s.new_drawing(true);

    // newplan: block, layer and model INSERT, one undo step.
    let model_before = s.doc()?.model.len();
    let plan = view_id(&s.execute("newplan", &json!({"name": "P"}))?)?;
    check_view_created(s.doc()?, plan)?;
    assert_eq!(s.doc()?.model.len(), model_before + 1);
    let vs = views(s.doc()?)?;
    assert!(matches!(vs.as_slice(), [AnyView::Plan(p)] if p.id == plan && p.name == "P" && p.placement.is_none()));
    s.undo()?;
    let d = s.doc()?;
    assert!(d.block(&view_block_name(plan)).is_none(), "undo removes the block");
    assert!(d.layer(&view_block_name(plan)).is_none(), "undo removes the layer");
    assert!(d.model.get(Handle(plan.0)).is_none(), "undo removes the insert");
    assert!(views(d)?.is_empty(), "undo removes the document entry");

    let plan = view_id(&s.execute("newplan", &json!({"name": "P"}))?)?;
    check_view_created(s.doc()?, plan)?;

    // newsectionline on the plan.
    let cut = json!({"name": "S", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 500}, "cutTo": {"x": 4000, "y": 500}});
    let section = view_id(&s.execute("newsectionline", &cut)?)?;
    check_view_created(s.doc()?, section)?;
    let vs = views(s.doc()?)?;
    match vs.as_slice() {
        [AnyView::Plan(_), AnyView::Vertical(v)] => {
            assert_eq!(v.id, section);
            assert_eq!(v.intent, VerticalIntent::Section);
            assert!(matches!(&v.placement, Some(Placement::Vertical { host_view_id, .. }) if *host_view_id == plan));
        }
        other => return Err(format!("unexpected views {other:?}").into()),
    }

    // Refusals.
    let mut bad = cut.clone();
    bad["hostViewId"] = json!(999_999);
    let e = s.execute("newsectionline", &bad).err().ok_or("bad host accepted")?;
    assert!(e.to_string().contains("No such host view"), "{e}");
    let elev = view_id(&s.execute("newelevationline", &cut)?)?;
    // An unplaced vertical host: strip the elevation's placement directly in the document.
    {
        let d = s.doc_mut()?;
        let mut doc = lamb_cadcraft::read_document(d)?;
        for v in &mut doc.views {
            if let AnyView::Vertical(v) = v
                && v.id == elev
            {
                assert_eq!(v.intent, VerticalIntent::Elevation);
                v.placement = None;
            }
        }
        lamb_cadcraft::write_document(d, &doc)?;
    }
    let mut on_unplaced = cut.clone();
    on_unplaced["hostViewId"] = json!(elev.0);
    let e = s.execute("newsectionline", &on_unplaced).err().ok_or("unplaced vertical host accepted")?;
    assert!(e.to_string().contains("Host vertical view must be placed"), "{e}");

    // editview + CADCraft `line` draws inside the view's block.
    s.execute("editview", &json!({"viewId": plan.0}))?;
    assert_eq!(s.space(), Space::Block(view_block_name(plan)));
    let model_count = s.doc()?.model.len();
    s.execute("line", &json!({"points": [[0, 0], [4000, 0]]}))?;
    assert_eq!(s.doc()?.model.len(), model_count, "nothing added to model space");
    let members = block_members(s.doc()?, plan);
    assert_eq!(members.len(), 1);
    assert_eq!(view_entity_ids(s.doc()?, plan)?, vec![Id(members[0].0)]);

    s.execute("editview", &json!({"viewId": section.0}))?;
    s.execute("line", &json!({"points": [[0, 0], [0, 3000]]}))?;
    let members = block_members(s.doc()?, section);
    assert_eq!(members.len(), 1);
    assert_eq!(view_entity_ids(s.doc()?, section)?, vec![Id(members[0].0)]);

    let e = s.execute("editview", &json!({"viewId": 999_999})).err().ok_or("unknown view accepted")?;
    assert!(e.to_string().contains("No such view"), "{e}");
    s.execute("editview", &json!({}))?;
    assert_eq!(s.space(), Space::Model);

    // Gate: DXF save / reload keeps views, entity ids and polylines.
    let before = s.doc()?;
    let dxf = write_dxf(before);
    let after = read_dxf(dxf.as_bytes())?;
    assert_eq!(views(&after)?, views(before)?, "views survive DXF");
    let (eb, ea) = (entities(before)?, entities(&after)?);
    assert!(eb.iter().filter(|(_, es)| !es.is_empty()).count() == 2, "both views hold a line: {eb:?}");
    assert_eq!(ea, eb, "entity ids and polylines survive DXF");
    check_view_created(&after, plan)?;
    check_view_created(&after, section)?;
    Ok(())
}

//! LAMB views carry no Z (LAMB T588): after every command, any entity in a LAMB view block (plan,
//! section, elevation or detail) holding a non-zero z, elevation or thickness is flattened to
//! Z = 0 with the engine's own FLATTEN rule ([`cadcraft_engine::cmd::flatten_kind`]) and its
//! thickness zeroed, inside the same command's undo step. However the Z got there — creation, a
//! Properties edit, a grip, MOVE, a paste or DXFIMPORT — this one observer catches it. Generated
//! blocks (assembly, annotation, hole, layer-hatch) are not view blocks and are left alone, as is
//! model space outside every view block.

use std::sync::Arc;

use cadcraft_doc::{Drawing, Entity, EntityKind, Handle};
use cadcraft_engine::Session;
use cadcraft_engine::cmd::flatten_kind;

use crate::schema::{read_document, view_block_name};

/// `k` as FLATTEN leaves it, or `None` when FLATTEN would change nothing that carries Z. A
/// dimension's cached block and a ray's direction length are not Z: FLATTEN drops the one and
/// re-normalises the other, so they are normalised on both sides before comparing.
fn flattened(k: &EntityKind) -> Option<EntityKind> {
    let mut flat = k.clone();
    flatten_kind(&mut flat);
    let mut same = k.clone();
    match &mut same {
        EntityKind::Dimension(d) => d.block = None,
        EntityKind::Ray(r) | EntityKind::XLine(r) => r.dir = r.dir.normalized(),
        _ => {}
    }
    (flat != same).then_some(flat)
}

/// `e` flattened (kind per FLATTEN, thickness 0), or `None` when it is already flat.
fn flat_entity(e: &Entity) -> Option<Entity> {
    let kind = flattened(&e.kind);
    if kind.is_none() && e.common.thickness == 0.0 {
        return None;
    }
    let mut out = e.clone();
    if let Some(k) = kind {
        out.kind = k;
    }
    out.common.thickness = 0.0;
    Some(out)
}

/// Every view block's entities that are not flat: (block name, view name, flattened entities).
fn plan(d: &Drawing) -> Vec<(String, String, Vec<Entity>)> {
    let Ok(doc) = read_document(d) else { return Vec::new() };
    doc.views
        .iter()
        .filter_map(|v| {
            let name = view_block_name(v.id());
            let b = d.blocks.get(&name)?;
            let fixed: Vec<Entity> = b.entities.iter().filter_map(|e| flat_entity(e)).collect();
            (!fixed.is_empty()).then(|| (name, v.name().to_owned(), fixed))
        })
        .collect()
}

/// The after-command observer: every LAMB view block entity flattened to Z = 0. The drawing is
/// written back only when something changed.
pub fn flatz_observer(s: &mut Session, _before: &Arc<Drawing>) {
    let todo = match s.doc() {
        Ok(d) => plan(d),
        Err(_) => return,
    };
    if todo.is_empty() {
        return;
    }
    let Ok(current) = s.doc() else { return };
    let mut after = current.clone();
    let mut said = Vec::new();
    for (name, view, fixed) in todo {
        let Some(b) = after.blocks.get_mut(&name) else { continue };
        let b = Arc::make_mut(b);
        let n = fixed.len();
        for e in fixed {
            let h: Handle = e.handle;
            b.entities.modify(h, |x| *x = e);
        }
        said.push(format!("LAMB: flattened {n} object(s) to Z = 0 in {view}"));
    }
    if let Ok(d) = s.doc_mut() {
        *d = after;
    }
    for m in said {
        s.echo(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assembly::tests::{line, undo_len};
    use cadcraft_doc::Block;
    use lamb_core::model::Id;
    use serde_json::{Value, json};
    use std::collections::HashSet;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    /// A drawing with a plan `Ground` being edited and the observers installed.
    fn plan_session() -> T<(Session, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        Ok((s, plan))
    }

    fn all_handles(s: &Session) -> T<HashSet<Handle>> {
        let d = s.doc()?;
        Ok(d.model.iter().map(|e| e.handle).chain(d.blocks.values().flat_map(|b| b.entities.iter().map(|e| e.handle))).collect())
    }

    fn block(s: &Session, v: Id) -> T<Arc<Block>> {
        Ok(s.doc()?.blocks.get(&view_block_name(v)).cloned().ok_or("no view block")?)
    }

    fn entity(s: &Session, v: Id, h: Handle) -> T<Arc<Entity>> {
        Ok(block(s, v)?.entities.get(h).cloned().ok_or("not in the view block")?)
    }

    fn max_z(k: &EntityKind) -> f64 {
        match k {
            EntityKind::Line(l) => l.a.z.abs().max(l.b.z.abs()),
            EntityKind::LwPolyline(p) => p.elevation.abs(),
            EntityKind::Polyline3d(p) => p.points.iter().map(|q| q.z.abs()).fold(0.0, f64::max),
            _ => 0.0,
        }
    }

    #[test]
    fn a_3d_polyline_drawn_in_a_plan_lands_at_z_0_in_one_undo_step() -> R {
        let (mut s, plan) = plan_session()?;
        let before = all_handles(&s)?;
        let n = undo_len(&s)?;
        s.execute("3dpoly", &json!({ "points": [[0.0, 0.0, 500.0], [1000.0, 0.0, 500.0]] }))?;
        let h = all_handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("not drawn")?;
        let e = entity(&s, plan, h)?;
        assert!(matches!(e.kind, EntityKind::LwPolyline(_)), "{:?}", e.kind);
        assert_eq!(max_z(&e.kind), 0.0);
        assert_eq!(undo_len(&s)?, n + 1, "one undo step");
        s.execute("undo", &json!({}))?;
        assert!(block(&s, plan)?.entities.get(h).is_none(), "one undo removes it");
        Ok(())
    }

    #[test]
    fn an_elevation_or_thickness_edit_on_a_view_line_snaps_back() -> R {
        let (mut s, plan) = plan_session()?;
        let h = line(&mut s, [0.0, 0.0], [1000.0, 0.0])?;
        let r: Value = s.execute("change", &json!({ "handles": [h.hex()], "elevation": 500.0 }))?;
        let e = entity(&s, plan, h)?;
        assert_eq!(max_z(&e.kind), 0.0, "{r}");
        s.execute("properties.set", &json!({ "handles": [h.hex()], "thickness": 100.0 }))?;
        assert_eq!(entity(&s, plan, h)?.common.thickness, 0.0);
        Ok(())
    }

    #[test]
    fn model_space_outside_every_view_keeps_its_z() -> R {
        let (mut s, _plan) = plan_session()?;
        s.execute("editview", &json!({}))?;
        let before = all_handles(&s)?;
        s.execute("3dpoly", &json!({ "points": [[0.0, 0.0, 500.0], [1000.0, 0.0, 500.0]] }))?;
        let h = all_handles(&s)?.into_iter().find(|h| !before.contains(h)).ok_or("not drawn")?;
        let e = s.doc()?.entity(h).cloned().ok_or("gone")?;
        assert_eq!(max_z(&e.kind), 500.0);
        Ok(())
    }

    #[test]
    fn a_flat_drawing_is_not_written() -> R {
        let (mut s, _plan) = plan_session()?;
        line(&mut s, [0.0, 0.0], [1000.0, 0.0])?;
        let d0 = s.state()?.doc.clone();
        flatz_observer(&mut s, &d0);
        assert!(Arc::ptr_eq(&d0, &s.state()?.doc), "a flat drawing is not written");
        Ok(())
    }
}

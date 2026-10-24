//! Sheets: `newsheet` lays a plan and a section on one paper layout, each viewport showing only
//! its own view (every other view's layer frozen there), undoable in one step and kept by DXF.
//!
//! The command registry is process-global, so this binary has one test fn that registers.

use cadcraft_doc::{Drawing, EntityKind, Space, Viewport};
use cadcraft_engine::{Session, register_command_provider};
use cadcraft_geom::{Bounds2, Vec2};
use cadcraft_io::{read_dxf, write_dxf};
use cadcraft_render::{Kind, NO_LAYER, Options, build_plot};
use lamb_cadcraft::{commands, create_sheet, read_entity_data, view_block_name};
use lamb_core::model::Id;
use serde_json::{Value, json};

type R = Result<(), Box<dyn std::error::Error>>;

fn view_id(v: &Value) -> Result<Id, String> {
    v.get("viewId").and_then(Value::as_u64).map(Id).ok_or_else(|| format!("no viewId in {v}"))
}

/// (viewport, its EntityData.view_id) for every viewport in layout `name`.
fn viewports(d: &Drawing, name: &str) -> Result<Vec<(Viewport, Option<Id>)>, String> {
    let l = d.layout(name).ok_or_else(|| format!("no layout {name}"))?;
    let mut out = Vec::new();
    for e in l.entities.iter() {
        if let EntityKind::Viewport(vp) = &e.kind {
            let data = read_entity_data(&e.common).map_err(|e| e.to_string())?;
            out.push((vp.clone(), data.view_id));
        }
    }
    Ok(out)
}

fn check_sheet(d: &Drawing, name: &str, plan: Id, section: Id) -> Result<(), String> {
    let vps = viewports(d, name)?;
    if vps.len() != 2 {
        return Err(format!("expected two viewports, got {}", vps.len()));
    }
    let (a, b) = (&vps[0], &vps[1]);
    if !(a.0.id >= 2 && b.0.id >= 2 && a.0.id != b.0.id) {
        return Err(format!("viewport ids {} {}", a.0.id, b.0.id));
    }
    if a.1 != Some(plan) || b.1 != Some(section) {
        return Err(format!("view ids {:?} {:?}", a.1, b.1));
    }
    if a.0.frozen_layers != vec![view_block_name(section)] || b.0.frozen_layers != vec![view_block_name(plan)] {
        return Err(format!("frozen {:?} / {:?}", a.0.frozen_layers, b.0.frozen_layers));
    }
    Ok(())
}

fn rect(vp: &Viewport) -> Bounds2 {
    let c = vp.center.xy();
    let half = Vec2::new(vp.width / 2.0, vp.height / 2.0);
    Bounds2::new(c - half, c + half)
}

#[test]
fn newsheet_one_viewport_per_view() -> R {
    register_command_provider(commands)?;
    let mut s = Session::new();
    s.new_drawing(true);

    // Refusal: no views at all.
    let before = s.doc()?.clone();
    let e = s.execute("newsheet", &json!({"name": "S0"})).err().ok_or("sheet with no views accepted")?;
    assert!(e.to_string().contains("No views"), "{e}");
    assert_eq!(s.doc()?, &before, "drawing unchanged");

    let plan = view_id(&s.execute("newplan", &json!({"name": "P"}))?)?;
    s.execute("editview", &json!({"viewId": plan.0}))?;
    s.execute("line", &json!({"points": [[0, 0], [4000, 0]]}))?;
    let cut = json!({"name": "S", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 500}, "cutTo": {"x": 4000, "y": 500}});
    let section = view_id(&s.execute("newsectionline", &cut)?)?;
    s.execute("editview", &json!({"viewId": section.0}))?;
    s.execute("line", &json!({"points": [[0, 0], [0, 3000]]}))?;
    s.execute("editview", &json!({}))?;

    let layouts_before = s.doc()?.layouts.len();
    let out = s.execute("newsheet", &json!({"name": "S1"}))?;
    assert_eq!(out["layout"], json!("S1"));
    assert_eq!(out["viewports"].as_array().map(Vec::len), Some(2), "{out}");
    check_sheet(s.doc()?, "S1", plan, section)?;
    // Model space still shows both views: no layer frozen or turned off globally.
    for v in [plan, section] {
        let l = s.doc()?.layer(&view_block_name(v)).ok_or("view layer missing")?;
        assert!(!l.frozen && l.on, "view layer {} changed globally", l.name);
    }

    // Plot: each primitive inside a viewport is on that viewport's view layer, layer 0, or none.
    {
        let d = s.doc()?;
        let list = build_plot(d, &Space::Paper("S1".into()), &Options::default());
        let layer_index = |n: &str| d.layers.iter().position(|l| l.name == n).map(|i| i as u32);
        let zero = layer_index("0");
        let mut seen = Vec::new();
        for (vp, vid) in viewports(d, "S1")? {
            let own = layer_index(&view_block_name(vid.ok_or("viewport without view")?)).ok_or("no own layer")?;
            let r = rect(&vp);
            for (i, p) in list.prims.iter().enumerate() {
                let pts = match p.kind {
                    Kind::Polyline | Kind::Point => &list.verts[p.start as usize..(p.start + p.len) as usize],
                    _ => &list.tris[p.start as usize..(p.start + p.len) as usize],
                };
                let inside = !pts.is_empty() && pts.iter().all(|q| r.contains(*q));
                if !inside {
                    continue;
                }
                let l = list.prim_layers[i];
                assert!(l == own || Some(l) == zero || l == NO_LAYER, "prim {i} in viewport {} on layer {l}, own {own}", vp.id);
                if l == own {
                    seen.push(own);
                }
            }
        }
        for v in [plan, section] {
            let idx = layer_index(&view_block_name(v)).ok_or("no layer")?;
            assert!(seen.contains(&idx), "view {v:?} drawn in its viewport");
        }
    }

    // One undo removes the layout.
    s.undo()?;
    assert!(s.doc()?.layout("S1").is_none(), "undo removes the layout");
    assert_eq!(s.doc()?.layouts.len(), layouts_before);

    // Explicit view list (string ids too), then refusals leave the drawing unchanged.
    let out = s.execute("newsheet", &json!({"name": "S1", "viewIds": [plan.0, section.0.to_string()]}))?;
    assert_eq!(out["viewports"].as_array().map(Vec::len), Some(2));
    check_sheet(s.doc()?, "S1", plan, section)?;
    let before = s.doc()?.clone();
    let e = s.execute("newsheet", &json!({"name": "s1"})).err().ok_or("duplicate layout accepted")?;
    assert!(e.to_string().contains("already exists"), "{e}");
    assert_eq!(s.doc()?, &before);
    let e = s.execute("newsheet", &json!({"name": "S2", "viewIds": [999_999]})).err().ok_or("unknown view accepted")?;
    assert!(e.to_string().contains("No such view"), "{e}");
    assert_eq!(s.doc()?, &before);
    let mut d = before.clone();
    assert!(create_sheet(&mut d, "S3", &[Id(424_242)], &[]).is_err());
    assert_eq!(d, before);

    // DXF keeps the layout, both viewports, their frozen layers and view ids.
    let dxf = write_dxf(s.doc()?);
    let after = read_dxf(dxf.as_bytes())?;
    check_sheet(&after, "S1", plan, section)?;

    // VPVIEW: an ordinary layout's MVIEW viewport picks its view.
    s.execute("layout.new", &json!({"name": "L1", "viewport": false}))?;
    let out = s.execute("mview", &json!({"layout": "L1"}))?;
    let hex = out["viewports"][0].as_str().ok_or("no viewport")?.to_owned();
    let only = |d: &Drawing| -> Result<(Viewport, Option<Id>), String> {
        let v = viewports(d, "L1")?;
        v.into_iter().next().ok_or_else(|| "no viewport".to_owned())
    };
    let (vp0, v0) = only(s.doc()?)?;
    assert_eq!((v0, vp0.frozen_layers.is_empty()), (None, true));
    let bound_to = |d: &Drawing, view: Id, other: Id| -> R {
        let (vp, v) = only(d)?;
        assert_eq!(v, Some(view));
        assert_eq!(vp.frozen_layers, vec![view_block_name(other)]);
        let c = d.extents(&Space::Block(view_block_name(view))).center();
        assert!(vp.view_center.near(c, 1e-6), "zoomed to the view: {:?} vs {c:?}", vp.view_center);
        Ok(())
    };
    s.execute("vpview", &json!({"handle": hex, "viewId": plan.0}))?;
    bound_to(s.doc()?, plan, section)?;
    s.execute("vpview", &json!({"handle": hex, "viewId": section.0}))?;
    bound_to(s.doc()?, section, plan)?;
    s.execute("vpview", &json!({"handle": hex, "viewId": null}))?;
    let (vp, v) = only(s.doc()?)?;
    assert_eq!((v, vp.frozen_layers.is_empty()), (None, true), "unbound and thawed");
    s.undo()?;
    bound_to(s.doc()?, section, plan)?;
    s.undo()?;
    bound_to(s.doc()?, plan, section)?;
    // Refusals change nothing.
    let before = s.doc()?.clone();
    let e = s.execute("vpview", &json!({"handle": hex, "viewId": 999_999})).err().ok_or("unknown view accepted")?;
    assert!(e.to_string().contains("No such view"), "{e}");
    let line = s.doc()?.block(&view_block_name(plan)).and_then(|b| b.entities.iter().next().map(|e| e.handle.hex())).ok_or("no line")?;
    let e = s.execute("vpview", &json!({"handle": line, "viewId": plan.0})).err().ok_or("a line accepted")?;
    assert!(e.to_string().contains("Not a layout viewport"), "{e}");
    assert_eq!(s.doc()?, &before);
    Ok(())
}

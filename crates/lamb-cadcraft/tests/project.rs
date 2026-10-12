//! LAMB's data in `ext["LAMB"]` slots and `ProjectRead` over a CADCraft `Drawing`: views come from
//! the document slot, entities from each view's block, roles from entity and layer slots, and a
//! lamb-core derivation gives the same answer on the drawing as on the equivalent `Project`.

use std::sync::Arc;

use cadcraft_doc::{Block, Circle, Drawing, Entity, EntityKind, HAlign, Handle, Layer, Line, Text, VAlign};
use cadcraft_geom::Vec3;
use lamb_cadcraft::schema::{EntityData, LayerData, SchemaError, read_entity_data, set_entity_data, set_layer_data};
use lamb_cadcraft::{DrawingProject, EXT_KEY, LambDocument, read_document, to_polyline, view_block_name, write_document};
use lamb_core::derivation::markers::compute_markers_for_view;
use lamb_core::derivation::read::{ProjectRead, ViewKind};
use lamb_core::geometry::transform2d::IDENTITY_TRANSFORM;
use lamb_core::geometry::{Point2, PolySegment, Polyline2};
use lamb_core::model::{
    Actor, AnyView, DrawingEntity, EntityGeometry, Id, Level, LineRole, LinkAnchor, LinkOrigin, LinkRecord, Placement, PlanView, VerticalIntent,
    VerticalView, defaulted_field, empty_project,
};
use serde_json::json;

type R = Result<(), String>;

const PLAN: Id = Id(1);
const SECTION: Id = Id(2);
const LEVEL: Id = Id(10);
const WALL: Handle = Handle(0x100);
const GRID_LINE: Handle = Handle(0x101);
const COLUMN: Handle = Handle(0x102);
const LABEL: Handle = Handle(0x103);
const SECTION_LINE: Handle = Handle(0x200);

fn v3(x: f64, y: f64) -> Vec3 {
    Vec3::new(x, y, 0.0)
}

fn on(layer: &str, mut e: Entity) -> Entity {
    e.common.layer = layer.to_owned();
    e
}

fn line(h: Handle, a: (f64, f64), b: (f64, f64)) -> Entity {
    Entity::new(h, EntityKind::Line(Line { a: v3(a.0, a.1), b: v3(b.0, b.1) }))
}

fn level() -> Level {
    Level { id: LEVEL, name: "Ground".into(), order: 0.0, floor_elevation: defaulted_field(0.0, "test") }
}

fn views() -> Vec<AnyView> {
    vec![
        AnyView::Plan(PlanView {
            id: PLAN,
            name: "Ground plan".into(),
            entities: Vec::new(),
            placement: Some(Placement::Plan { level_id: LEVEL, cut_elevation: 1000.0, transform: IDENTITY_TRANSFORM }),
        }),
        AnyView::Vertical(VerticalView {
            id: SECTION,
            name: "Section A".into(),
            intent: VerticalIntent::Section,
            entities: Vec::new(),
            placement: Some(Placement::Vertical {
                cut_line: Polyline2 {
                    start: Point2 { x: -5000.0, y: 500.0 },
                    segments: vec![PolySegment { to: Point2 { x: 5000.0, y: 500.0 }, bulge: 0.0, corner_at_to: None }],
                    closed: false,
                },
                host_view_id: PLAN,
                transform: IDENTITY_TRANSFORM,
                flipped: None,
            }),
        }),
    ]
}

fn link() -> LinkRecord {
    LinkRecord {
        id: Id(50),
        a: LinkAnchor { entity_id: Id(WALL.0), segment_index: 0, segment_param: 0.5 },
        b: LinkAnchor { entity_id: Id(SECTION_LINE.0), segment_index: 0, segment_param: 0.25 },
        origin: LinkOrigin::Explicit,
        created_at: "2026-10-12T00:00:00Z".into(),
        created_by: Actor::User { user_id: "u".into() },
        stale: None,
    }
}

fn document() -> LambDocument {
    LambDocument { name: "T297".into(), levels: vec![level()], views: views(), links: vec![link()], ..LambDocument::empty() }
}

/// Plan 1: a cut wall line on layer 0, a Bylayer line on GRID (default role datum), a circle and
/// a text. Section 2 (placed on plan 1): one vertical line on layer 0.
fn drawing() -> Result<Drawing, String> {
    let mut d = Drawing::new_metric();
    let mut grid = Layer::new("GRID");
    set_layer_data(&mut grid, &LayerData { default_role: LineRole::Datum }).map_err(|e| e.to_string())?;
    d.layers.push(grid);

    let mut wall = on("0", line(WALL, (0.0, 0.0), (0.0, 1000.0)));
    set_entity_data(&mut wall.common, &EntityData { role: LineRole::Cut, ai_origin: None }).map_err(|e| e.to_string())?;
    let mut plan = Block::new(&view_block_name(PLAN));
    plan.entities.push(wall);
    plan.entities.push(on("GRID", line(GRID_LINE, (-3000.0, -1000.0), (3000.0, 2000.0))));
    plan.entities.push(on("0", Entity::new(COLUMN, EntityKind::Circle(Circle { center: v3(2000.0, 800.0), radius: 400.0 }))));
    plan.entities.push(on(
        "0",
        Entity::new(
            LABEL,
            EntityKind::Text(Text {
                insert: v3(0.0, 0.0),
                align_pt: None,
                height: 250.0,
                value: "Living".into(),
                rotation: 0.0,
                width_factor: 1.0,
                oblique: 0.0,
                style: "Standard".into(),
                halign: HAlign::default(),
                valign: VAlign::default(),
            }),
        ),
    ));
    d.blocks.insert(plan.name.clone(), Arc::new(plan));

    let mut section = Block::new(&view_block_name(SECTION));
    section.entities.push(on("0", line(SECTION_LINE, (0.0, 0.0), (0.0, 3000.0))));
    d.blocks.insert(section.name.clone(), Arc::new(section));

    write_document(&mut d, &document()).map_err(|e| e.to_string())?;
    Ok(d)
}

#[test]
fn views_entities_roles_and_links_read_through_the_drawing() -> R {
    let d = drawing()?;
    let p = DrawingProject::new(&d).map_err(|e| e.to_string())?;

    let vs: Vec<_> = p.views().map(|v| (v.id, v.kind, v.placement.cloned())).collect();
    let want = views();
    if vs.len() != 2 || vs[0].0 != PLAN || vs[0].1 != ViewKind::Plan || vs[1].0 != SECTION || vs[1].1 != ViewKind::Vertical {
        return Err(format!("views: {vs:?}"));
    }
    if vs[0].2.as_ref() != want[0].placement() || vs[1].2.as_ref() != want[1].placement() {
        return Err("placements differ from the stored ones".into());
    }
    if p.view(SECTION).and_then(|v| v.host_view_id()) != Some(PLAN) {
        return Err("section is not hosted on the plan".into());
    }

    let es: Vec<_> = p.entities(PLAN).collect();
    let ids: Vec<Id> = es.iter().map(|e| e.id).collect();
    if ids != [Id(WALL.0), Id(GRID_LINE.0), Id(COLUMN.0), Id(LABEL.0)] {
        return Err(format!("entity ids {ids:?}"));
    }
    if es.iter().any(|e| e.view_id != PLAN) {
        return Err("view_id not the plan".into());
    }
    let has: Vec<bool> = es.iter().map(|e| e.polyline.is_some()).collect();
    if has != [true, true, true, false] {
        return Err(format!("polyline presence {has:?} (text must be None)"));
    }
    if es[0].role != LineRole::Cut || es[1].role != LineRole::Bylayer {
        return Err(format!("stored roles {:?} {:?}", es[0].role, es[1].role));
    }
    if p.role_of(&es[1]) != LineRole::Datum {
        return Err(format!("Bylayer line on GRID resolved to {:?}", p.role_of(&es[1])));
    }
    if p.role_of(&es[2]) != LineRole::Cut {
        return Err("Bylayer circle on layer 0 should resolve to cut".into());
    }

    let found = p.links_for_segment(Id(WALL.0), 0);
    if found.len() != 1 || found[0].link.id != Id(50) || found[0].other.entity_id != Id(SECTION_LINE.0) {
        return Err("links_for_segment did not find the wall link".into());
    }
    if p.entity_view_id(Id(SECTION_LINE.0)) != Some(SECTION) {
        return Err("section line not found in the section".into());
    }
    if p.level(LEVEL).map(|l| l.name.as_str()) != Some("Ground") {
        return Err("level missing".into());
    }
    if p.entities(Id(99)).next().is_some() {
        return Err("unknown view should have no entities".into());
    }
    Ok(())
}

#[test]
fn a_view_without_its_block_has_no_entities() -> R {
    let mut d = drawing()?;
    d.blocks.remove(&view_block_name(SECTION));
    let p = DrawingProject::new(&d).map_err(|e| e.to_string())?;
    if p.entities(SECTION).next().is_some() || p.view(SECTION).is_none() {
        return Err("missing block should mean an empty view, not a missing one".into());
    }
    Ok(())
}

/// Builds the lamb-core `Project` the drawing stands for.
fn equivalent_project(p: &DrawingProject<'_>, d: &Drawing) -> Result<lamb_core::model::Project, String> {
    let mut proj = empty_project(Some("T297"));
    proj.layers = p.layers().to_vec();
    proj.levels = vec![level()];
    proj.links = vec![link()];
    let mut vs = views();
    for v in &mut vs {
        let id = v.id();
        let block = d.block(&view_block_name(id)).ok_or("block missing")?;
        let mut entities = Vec::new();
        for e in block.entities.iter() {
            let Ok(c) = to_polyline(e) else { continue };
            let data = read_entity_data(&e.common).map_err(|e| e.to_string())?;
            entities.push(DrawingEntity {
                id: Id(e.handle.0),
                view_id: id,
                role: data.role,
                layer_id: e.common.layer.clone(),
                geometry: EntityGeometry::Polyline { geometry: c.polyline },
                color_override: None,
                lineweight_override: None,
                ai_origin: data.ai_origin,
            });
        }
        match v {
            AnyView::Plan(x) => x.entities = entities,
            AnyView::Vertical(x) => x.entities = entities,
            AnyView::Detail(x) => x.entities = entities,
        }
    }
    proj.views = vs;
    Ok(proj)
}

#[test]
fn markers_match_the_equivalent_project() -> R {
    let d = drawing()?;
    let p = DrawingProject::new(&d).map_err(|e| e.to_string())?;
    let proj = equivalent_project(&p, &d)?;
    for view in [PLAN, SECTION] {
        let ours = compute_markers_for_view(&p, view)?;
        let theirs = compute_markers_for_view(&proj, view)?;
        if ours != theirs {
            return Err(format!("view {view}: markers differ\n  {ours:?}\n  {theirs:?}"));
        }
        if ours.is_empty() {
            return Err(format!("view {view}: expected markers"));
        }
    }
    // Wall (cut) once, circle twice; the datum grid line and the text never.
    let section = compute_markers_for_view(&p, SECTION)?;
    if section.len() != 3 || section.iter().any(|m| m.source_entity_id == Id(GRID_LINE.0)) {
        return Err(format!("section markers {section:?}"));
    }
    if !section.iter().any(|m| m.source_entity_id == Id(WALL.0) && m.filled) {
        return Err("the linked wall's marker should be filled".into());
    }
    Ok(())
}

#[test]
fn document_round_trips_with_entities_emptied() -> R {
    let mut input = document();
    if let Some(AnyView::Plan(v)) = input.views.first_mut() {
        v.entities.push(DrawingEntity {
            id: Id(7),
            view_id: PLAN,
            role: LineRole::Cut,
            layer_id: "0".into(),
            geometry: EntityGeometry::Polyline {
                geometry: Polyline2 {
                    start: Point2 { x: 0.0, y: 0.0 },
                    segments: vec![PolySegment { to: Point2 { x: 1.0, y: 0.0 }, bulge: 0.0, corner_at_to: None }],
                    closed: false,
                },
            },
            color_override: None,
            lineweight_override: None,
            ai_origin: None,
        });
    }
    input.suppressed_proposals = vec!["p1".into()];
    let mut d = Drawing::new_metric();
    write_document(&mut d, &input).map_err(|e| e.to_string())?;
    let stored = d.ext.get(EXT_KEY).ok_or("no slot written")?;
    if stored.get("schemaVersion") != Some(&json!(1)) || stored["views"][0]["entities"] != json!([]) {
        return Err(format!("stored slot {stored}"));
    }
    let back = read_document(&d).map_err(|e| e.to_string())?;
    let mut want = input.clone();
    if let Some(AnyView::Plan(v)) = want.views.first_mut() {
        v.entities.clear();
    }
    if back != want {
        return Err(format!("round trip differs:\n  {back:?}\n  {want:?}"));
    }

    // The reader drops entities found in storage too.
    d.ext.insert(EXT_KEY.into(), serde_json::to_value(&input).map_err(|e| e.to_string())?);
    if read_document(&d).map_err(|e| e.to_string())? != want {
        return Err("reader kept stored entities".into());
    }
    Ok(())
}

#[test]
fn no_slot_reads_as_the_empty_document() -> R {
    let d = Drawing::new_metric();
    let doc = read_document(&d).map_err(|e| e.to_string())?;
    if doc != LambDocument::empty() || !doc.views.is_empty() {
        return Err(format!("{doc:?}"));
    }
    let p = DrawingProject::new(&d).map_err(|e| e.to_string())?;
    if p.views().next().is_some() || p.layers().first().map(|l| l.default_role) != Some(LineRole::Cut) {
        return Err("empty drawing should have no views and a cut layer 0".into());
    }
    Ok(())
}

#[test]
fn malformed_and_future_documents_are_errors() -> R {
    let mut d = Drawing::new_metric();
    d.ext.insert(EXT_KEY.into(), json!({ "schemaVersion": 1, "name": 5 }));
    if !matches!(read_document(&d), Err(SchemaError::Malformed { .. })) || DrawingProject::new(&d).is_ok() {
        return Err("malformed JSON should be Err".into());
    }
    d.ext.insert(EXT_KEY.into(), json!("not an object"));
    if !matches!(read_document(&d), Err(SchemaError::Malformed { .. })) {
        return Err("non-object slot should be Err".into());
    }

    let mut d = drawing()?;
    if let Some(v) = d.ext.get_mut(EXT_KEY) {
        v["schemaVersion"] = json!(2);
    }
    match read_document(&d) {
        Err(SchemaError::FutureVersion { found: 2, supported: 1 }) => {}
        other => return Err(format!("future version read as {other:?}")),
    }
    if DrawingProject::new(&d).is_ok() {
        return Err("DrawingProject::new should refuse a future version".into());
    }

    let mut d = drawing()?;
    let block = d.blocks.get_mut(&view_block_name(PLAN)).ok_or("no plan block")?;
    let mut bad = on("0", line(Handle(0x300), (0.0, 0.0), (1.0, 0.0)));
    bad.common.ext.insert(EXT_KEY.into(), json!({ "role": "sideways" }));
    Arc::make_mut(block).entities.push(bad);
    if DrawingProject::new(&d).is_ok() {
        return Err("a malformed entity slot should be Err".into());
    }
    Ok(())
}

#[test]
fn all_default_slots_are_not_written() -> R {
    let mut e = line(Handle(1), (0.0, 0.0), (1.0, 0.0));
    set_entity_data(&mut e.common, &EntityData::default()).map_err(|e| e.to_string())?;
    if !e.common.ext.is_empty() {
        return Err("default EntityData wrote a slot".into());
    }
    set_entity_data(&mut e.common, &EntityData { role: LineRole::Seen, ai_origin: None }).map_err(|e| e.to_string())?;
    if e.common.ext.get(EXT_KEY) != Some(&json!({ "role": "seen" })) {
        return Err(format!("slot {:?}", e.common.ext));
    }
    set_entity_data(&mut e.common, &EntityData::default()).map_err(|e| e.to_string())?;
    if !e.common.ext.is_empty() {
        return Err("resetting to default should remove the slot".into());
    }
    let mut l = Layer::new("X");
    set_layer_data(&mut l, &LayerData::default()).map_err(|e| e.to_string())?;
    if !l.ext.is_empty() {
        return Err("default LayerData wrote a slot".into());
    }
    Ok(())
}

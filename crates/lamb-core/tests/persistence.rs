//! `lamb_core::persistence::files_to_project` against LAMB's saved `.lamb` fixtures (`tests/lamb/`),
//! plus hand-built maps for the migrations and every `LoadError`. Expected counts are what
//! `JSON.parse` of the same fixture shows.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use lamb_core::model::{CURRENT_SCHEMA_VERSION, EntityGeometry, Project};
use lamb_core::persistence::{LoadError, files_to_project};
use serde_json::{Value, json};

fn fixture_map(name: &str) -> BTreeMap<String, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/lamb").join(format!("{name}.lamb.json"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn load(name: &str) -> Project {
    files_to_project(&fixture_map(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn entity_counts(p: &Project) -> Vec<usize> {
    p.views.iter().map(|v| v.entities().len()).collect()
}

fn assert_links_resolve(p: &Project) {
    let ids: HashSet<_> = p.views.iter().flat_map(|v| v.entities().iter().map(|e| e.id)).collect();
    for link in &p.links {
        assert!(ids.contains(&link.a.entity_id) && ids.contains(&link.b.entity_id), "link {:?} names an unknown entity", link.id);
    }
}

#[test]
fn u_wall_two_sections_schema_1() {
    let p = load("u-wall-two-sections");
    assert_eq!(p.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(entity_counts(&p), [1, 1, 2]);
    assert_eq!((p.links.len(), p.layers.len()), (3, 7));
    assert_eq!(p.defaults.plot_scale, 50.0, "plotScale absent from the file comes from the factory defaults");
    assert_eq!(p.defaults.default_cut_height_mm, 1000.0);
    assert_links_resolve(&p);
}

#[test]
fn dimensioned_u_wall_circle_becomes_polyline() {
    let p = load("dimensioned-u-wall");
    assert_eq!(entity_counts(&p), [6, 1, 2]);
    assert_eq!((p.links.len(), p.layers.len()), (3, 7));
    let all = || p.views.iter().flat_map(|v| v.entities());
    assert!(all().all(|e| !matches!(e.geometry, EntityGeometry::Circle { .. })));
    // The plan's first entity was the circle.
    let EntityGeometry::Polyline { geometry } = &p.views[0].entities()[0].geometry else { panic!("former circle is not a polyline") };
    assert!(geometry.closed);
    assert_eq!(geometry.segments.len(), 2);
    assert!(geometry.segments.iter().all(|s| s.bulge == 1.0));
    assert_links_resolve(&p);
}

#[test]
fn filleted_corners_keeps_corner_tags() {
    let p = load("filleted-corners");
    assert_eq!(entity_counts(&p), [3]);
    assert_eq!((p.links.len(), p.layers.len()), (0, 7));
    let tags = p.views[0]
        .entities()
        .iter()
        .filter_map(|e| match &e.geometry {
            EntityGeometry::Polyline { geometry } => Some(geometry.segments.iter().filter(|s| s.corner_at_to.is_some()).count()),
            _ => None,
        })
        .sum::<usize>();
    assert_eq!(tags, 5);
}

#[test]
fn t76_rotated_section_schema_3() {
    let p = load("t76-rotated-section");
    assert_eq!(entity_counts(&p), [1, 0]);
    assert_eq!((p.links.len(), p.layers.len()), (0, 7));
    assert_links_resolve(&p);
}

/// A minimal valid map: one plan view holding one closed 4-segment polyline.
fn hand_built(schema: Value, geometry_extra: Value) -> BTreeMap<String, String> {
    let mut geometry = json!({
        "kind": "polyline",
        "geometry": { "start": {"x": 0, "y": 0}, "closed": true, "segments": [
            {"to": {"x": 10, "y": 0}, "bulge": 0}, {"to": {"x": 10, "y": 10}, "bulge": 0},
            {"to": {"x": 0, "y": 10}, "bulge": 0}, {"to": {"x": 0, "y": 0}, "bulge": 0}
        ]}
    });
    if let (Some(g), Value::Object(extra)) = (geometry.as_object_mut(), geometry_extra) {
        g.extend(extra);
    }
    let mut files = fixture_map("t76-rotated-section");
    let plan_path = "views/p--plan-1.json".to_owned();
    let mut plan: Value = serde_json::from_str(&files[&plan_path]).unwrap();
    let mut entity = plan["entities"][0].clone();
    entity["geometry"] = geometry;
    plan["entities"] = json!([entity]);
    files.insert(plan_path, plan.to_string());
    let mut manifest: Value = serde_json::from_str(&files["manifest.json"]).unwrap();
    manifest["schemaVersion"] = schema;
    files.insert("manifest.json".into(), manifest.to_string());
    files
}

#[test]
fn virtual_corners_migrate_onto_segments() {
    let corners = json!({ "virtualCorners": { "0": {"kind": "fillet", "radius": 2}, "2": {"kind": "fillet", "radius": 3} } });
    let p = files_to_project(&hand_built(json!(1), corners)).unwrap();
    let EntityGeometry::Polyline { geometry } = &p.views[0].entities()[0].geometry else { panic!() };
    let tagged: Vec<usize> = geometry.segments.iter().enumerate().filter(|(_, s)| s.corner_at_to.is_some()).map(|(i, _)| i).collect();
    assert_eq!(tagged, [1, 3]);
    let tag = |i: usize| serde_json::to_value(geometry.segments[i].corner_at_to).unwrap();
    assert_eq!(tag(3)["radius"], json!(2.0));
    assert_eq!(tag(1)["radius"], json!(3.0));
}

#[test]
fn virtual_corners_ignored_from_schema_2() {
    // From schema 2 on the map is not migrated; serde ignores the stray field.
    let corners = json!({ "virtualCorners": { "2": {"kind": "fillet", "radius": 3} } });
    let p = files_to_project(&hand_built(json!(2), corners)).unwrap();
    let EntityGeometry::Polyline { geometry } = &p.views[0].entities()[0].geometry else { panic!() };
    assert!(geometry.segments.iter().all(|s| s.corner_at_to.is_none()));
}

#[test]
fn load_errors() {
    let mut files = fixture_map("t76-rotated-section");
    files.remove("project.json");
    assert_eq!(files_to_project(&files), Err(LoadError::Missing("project.json".into())));

    let mut files = fixture_map("t76-rotated-section");
    files.remove("views/diag--sec-diag.json");
    assert_eq!(files_to_project(&files), Err(LoadError::Missing("views/diag--sec-diag.json".into())));

    let mut files = fixture_map("t76-rotated-section");
    files.insert("links/links.json".into(), "[{".into());
    assert!(matches!(files_to_project(&files), Err(LoadError::Json { path, .. }) if path == "links/links.json"));

    assert_eq!(files_to_project(&hand_built(json!(99), json!({}))), Err(LoadError::FutureVersion(99)));
}

#[test]
fn optional_files_default_empty() {
    let mut files = fixture_map("t76-rotated-section");
    files.remove("links/links.json");
    files.remove("command-log.json");
    let p = files_to_project(&files).unwrap();
    assert!(p.links.is_empty() && p.command_log.is_empty());
}

/// T355: AI lines' slot keys are rewritten from the TS string ids onto the loaded ids, so the
/// first sweep of a freshly loaded, settled project finds every slot filled.
#[test]
fn ai_slot_keys_name_the_loaded_ids_and_the_first_sweep_is_a_no_op() {
    use lamb_core::derivation::proposals::{ai_proposal_slot_key, reconcile_ai_lines};
    use lamb_core::model::Id;
    let mut p = load("u-wall-two-sections");
    let views: HashSet<Id> = p.views.iter().map(|v| v.id()).collect();
    let entities: HashSet<Id> = p.views.iter().flat_map(|v| v.entities().iter().map(|e| e.id)).collect();
    let origins: Vec<_> = p.views.iter().flat_map(|v| v.entities().iter().filter_map(|e| e.ai_origin.clone())).collect();
    assert!(!origins.is_empty(), "the fixture carries AI lines");
    for o in &origins {
        let parts: Vec<&str> = o.proposal_id.split(':').collect();
        let [source, index, view] = parts.as_slice() else { panic!("not an AI slot key: {}", o.proposal_id) };
        let (source, index, view) = (Id(source.parse().unwrap()), index.parse().unwrap(), Id(view.parse().unwrap()));
        assert!(entities.contains(&source) && views.contains(&view), "{} names ids in the project", o.proposal_id);
        assert_eq!(o.proposal_id, ai_proposal_slot_key(source, index, view));
    }
    let mut n = 1u64 << 50;
    let r = reconcile_ai_lines(&mut p, "2026-01-01T00:00:00.000Z", &mut || {
        n += 1;
        Id(n)
    })
    .unwrap();
    assert_eq!((r.created_entities.len(), r.removed_entities.len()), (0, 0), "the first sweep re-proposes nothing");
}

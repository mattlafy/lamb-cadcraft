//! `lamb_core::persistence::files_to_project` against LAMB's saved `.lamb` fixtures (`tests/lamb/`),
//! plus hand-built maps for the migrations and every `LoadError`. Expected counts are what
//! `JSON.parse` of the same fixture shows.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use lamb_core::model::{CURRENT_SCHEMA_VERSION, EntityGeometry, Project, with_string_ids_seeded};
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

#[test]
fn loaded_id_strings_outlive_the_read() {
    let p = load("t76-rotated-section");
    let ids =
        p.views.iter().map(|v| v.id()).chain(p.views.iter().flat_map(|v| v.entities().iter().map(|e| e.id))).chain(p.links.iter().map(|l| l.id));
    for id in ids {
        assert!(p.id_strings.0.contains_key(&id.0), "id {id:?} has no original string");
    }
    let files = fixture_map("t76-rotated-section");
    let mut fixture_ids: Vec<String> = files
        .iter()
        .filter(|(path, _)| path.starts_with("views/"))
        .map(|(_, text)| serde_json::from_str::<Value>(text).unwrap()["id"].as_str().unwrap().to_owned())
        .collect();
    let (views, _) = with_string_ids_seeded(&p.id_strings, || serde_json::to_value(&p.views).unwrap());
    let mut loaded_ids: Vec<String> = views.as_array().unwrap().iter().map(|v| v["id"].as_str().unwrap().to_owned()).collect();
    fixture_ids.sort();
    loaded_ids.sort();
    assert_eq!(loaded_ids, fixture_ids);
    assert_eq!(loaded_ids.len(), p.views.len());
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

// ---- the writer (LAMB T412) ----

use lamb_core::persistence::{project_to_files, read_lamb_dir, stable_stringify, write_lamb_dir};

const FIXTURES: [&str; 4] = ["u-wall-two-sections", "dimensioned-u-wall", "filleted-corners", "t76-rotated-section"];

fn write(p: &Project) -> BTreeMap<String, String> {
    project_to_files(p).into_iter().collect()
}

/// The first line that differs, for a readable failure.
fn first_diff(a: &str, b: &str) -> String {
    a.lines()
        .zip(b.lines())
        .enumerate()
        .find(|(_, (x, y))| x != y)
        .map_or_else(|| "lengths differ".to_owned(), |(i, (x, y))| format!("line {}: {x:?} vs {y:?}", i + 1))
}

#[test]
fn t76_writes_byte_identical_to_the_ts_writer() {
    let fixture = fixture_map("t76-rotated-section");
    let written = write(&load("t76-rotated-section"));
    assert_eq!(written.keys().collect::<Vec<_>>(), fixture.keys().collect::<Vec<_>>());
    for (path, text) in &fixture {
        assert!(written[path] == *text, "{path}: {}", first_diff(&written[path], text));
    }
}

#[test]
fn unknown_layer_keys_round_trip() {
    let raw = json!({ "id": "layer-0", "name": "0", "color": "#d0d0d0", "lineweight": 0.25, "visible": true,
        "locked": false, "defaultRole": "cut", "printable": false });
    let layer: lamb_core::model::DrawingLayer = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(layer.extra.get("printable"), Some(&json!(false)));
    assert_eq!(serde_json::to_value(&layer).unwrap(), raw);
}

#[test]
fn every_fixture_rewrites_stably() {
    for name in FIXTURES {
        let once = write(&load(name));
        let twice = write(&files_to_project(&once).unwrap());
        for (path, text) in &once {
            assert!(twice.get(path) == Some(text), "{name} {path}: {}", first_diff(twice.get(path).map_or("", String::as_str), text));
        }
        assert_eq!(once.len(), twice.len(), "{name}");
        let manifest: Value = serde_json::from_str(&once["manifest.json"]).unwrap();
        assert_eq!(manifest["schemaVersion"], json!(CURRENT_SCHEMA_VERSION), "{name}");
    }
    let p = load("t76-rotated-section");
    assert_eq!(files_to_project(&write(&p)).unwrap(), p);
}

#[test]
fn stable_stringify_matches_json_stringify() {
    let s = |v: Value| stable_stringify(&v);
    assert_eq!(s(json!(1500.0)), "1500\n");
    assert_eq!(s(json!(0.1)), "0.1\n");
    assert_eq!(s(json!(-0.0)), "0\n");
    assert_eq!(s(json!(1e-7)), "1e-7\n");
    assert_eq!(s(json!(1e-6)), "0.000001\n");
    assert_eq!(s(json!(1.5e21)), "1.5e+21\n");
    assert_eq!(s(json!(1e20)), "100000000000000000000\n");
    assert_eq!(s(json!(123_456_789_012.5)), "123456789012.5\n");
    assert_eq!(s(json!(-3)), "-3\n");
    assert_eq!(s(json!({"b": 1, "10": 2, "2": 3, "a": 4, "02": 5})), "{\n  \"2\": 3,\n  \"10\": 2,\n  \"02\": 5,\n  \"a\": 4,\n  \"b\": 1\n}\n");
    assert_eq!(
        s(json!({"a": [], "b": {}, "c": [{}, [1, null, true]]})),
        "{\n  \"a\": [],\n  \"b\": {},\n  \"c\": [\n    {},\n    [\n      1,\n      null,\n      true\n    ]\n  ]\n}\n"
    );
    assert_eq!(s(json!("q\"b\\\né\u{1}")), concat!(r#""q\"b\\\né\u0001""#, "\n"));
}

#[test]
fn minted_ids_write_as_strings_and_slot_keys_map_back() {
    use lamb_core::derivation::proposals::ai_proposal_slot_key;
    use lamb_core::model::{AnyView, Id};
    let mut p = load("u-wall-two-sections");
    let template = p.views.iter().flat_map(|v| v.entities()).find(|e| e.ai_origin.is_some()).unwrap().clone();
    let (source, view) = (p.views[0].entities()[0].id, p.views[0].id());
    let mut minted = template;
    minted.id = Id(7);
    assert!(!p.id_strings.0.contains_key(&7));
    let key = ai_proposal_slot_key(source, 0, view);
    minted.ai_origin.as_mut().unwrap().proposal_id = key.clone();
    match &mut p.views[0] {
        AnyView::Plan(v) => v.entities.push(minted),
        AnyView::Vertical(v) => v.entities.push(minted),
        AnyView::Detail(v) => v.entities.push(minted),
    }
    let files = write(&p);
    let view_file = files.iter().find(|(_, t)| t.contains("lamb-00000007-0")).map(|(_, t)| t).expect("the minted entity is written by its string id");
    let v: Value = serde_json::from_str(view_file).unwrap();
    let original = |id: Id| p.id_strings.0[&id.0].clone();
    let written_key = format!("{}:0:{}", original(source), original(view));
    assert!(v["entities"].as_array().unwrap().iter().any(|e| e["id"] == "lamb-00000007-0" && e["aiOrigin"]["proposalId"] == written_key.as_str()));
    // No bare decimal anywhere an id is written.
    let ids: Vec<String> = files
        .values()
        .flat_map(|t| {
            let v: Value = serde_json::from_str(t).unwrap();
            let mut out = Vec::new();
            collect_ids(&v, &mut out);
            out
        })
        .collect();
    assert!(ids.iter().all(|s| s.parse::<u64>().is_err()), "a bare decimal id was written: {ids:?}");
    let back = files_to_project(&files).unwrap();
    assert_eq!(entity_counts(&back), entity_counts(&p));
    let by_string = |s: &str| Id(*back.id_strings.0.iter().find(|(_, v)| v.as_str() == s).unwrap().0);
    let readback = back.views.iter().flat_map(|v| v.entities()).find(|e| e.id == by_string("lamb-00000007-0")).unwrap();
    let expected = ai_proposal_slot_key(by_string(&original(source)), 0, by_string(&original(view)));
    assert_eq!(readback.ai_origin.as_ref().unwrap().proposal_id, expected);
}

fn collect_ids(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if (k == "id" || k.ends_with("Id")) && x.is_string() {
                    out.push(x.as_str().unwrap().to_owned());
                }
                collect_ids(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_ids(x, out)),
        _ => {}
    }
}

#[test]
fn write_lamb_dir_round_trips_and_prunes_stale_views() {
    let dir = std::env::temp_dir().join(format!("lamb-core-write-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("views")).unwrap();
    std::fs::write(dir.join("views/old--x.json"), "{}").unwrap();
    std::fs::write(dir.join("notes.txt"), "keep me").unwrap();
    let p = load("t76-rotated-section");
    write_lamb_dir(&p, &dir).unwrap();
    assert!(!dir.join("views/old--x.json").exists());
    assert_eq!(std::fs::read_to_string(dir.join("notes.txt")).unwrap(), "keep me");
    std::fs::remove_file(dir.join("notes.txt")).unwrap();
    assert_eq!(read_lamb_dir(&dir).unwrap(), p);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[ignore = "debug: LAMB_DUMP=<dir> writes each fixture and its rewrite side by side, for diff -r"]
fn dump_written_vs_fixtures() {
    let out = std::path::PathBuf::from(std::env::var("LAMB_DUMP").unwrap());
    for name in FIXTURES {
        for (tag, files) in [("fixture", fixture_map(name)), ("written", write(&load(name)))] {
            for (path, text) in files {
                let p = out.join(name).join(tag).join(path);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, text).unwrap();
            }
        }
    }
}

//! A lamb-core `Project` imported as a `Drawing` derives the same markers and issues on both sides
//! (`cadcraft-integration.md` §3.1 item 2), on every T299 `.lamb` fixture.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use lamb_cadcraft::{DrawingProject, Flattened, IdMap, ImportReport, project_to_drawing};
use lamb_core::derivation::issues::find_issues;
use lamb_core::derivation::markers::{Marker, compute_markers_for_view};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::geometry::Point2;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AnyView, DrawingEntity, EntityGeometry, Id, LineRole, Placement, Project};
use lamb_core::persistence::files_to_project;

type R = Result<(), Box<dyn std::error::Error>>;

const FIXTURES: [&str; 4] = ["u-wall-two-sections", "dimensioned-u-wall", "filleted-corners", "t76-rotated-section"];

fn load(name: &str) -> Result<Project, Box<dyn std::error::Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb").join(format!("{name}.lamb.json"));
    let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(files_to_project(&files).map_err(|e| format!("{e:?}"))?)
}

fn kind(v: &AnyView) -> String {
    match v {
        AnyView::Plan(_) => "plan".into(),
        AnyView::Vertical(x) => format!("vertical/{:?}", x.intent),
        AnyView::Detail(_) => "detail".into(),
    }
}

fn mapped(map: &IdMap, id: Id) -> Result<Id, String> {
    map.get(id).ok_or_else(|| format!("{id} not in the id map"))
}

fn sort_key(m: &Marker) -> (u64, u64, usize, i64) {
    (m.view_id.0, m.source_entity_id.0, m.source_segment_index, (m.source_segment_param * 1e6).round() as i64)
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= TOLERANCE.coincidence_mm
}

fn near(a: Point2, b: Point2) -> bool {
    (a.x - b.x).hypot(a.y - b.y) <= TOLERANCE.coincidence_mm
}

/// Every check in the brief's "Done when", for one project.
fn check_agrees(name: &str, p: &Project) -> Result<(ImportReport, usize), Box<dyn std::error::Error>> {
    let (d, map, report) = project_to_drawing(p)?;
    let dp = DrawingProject::new(&d)?;
    let dviews = &dp.document().views;

    // Same views, names, kinds, intents, in order; hosts mapped.
    assert_eq!(dviews.len(), p.views.len(), "{name}: view count");
    for (pv, dv) in p.views.iter().zip(dviews) {
        assert_eq!(dv.id(), mapped(&map, pv.id())?, "{name}: view id mapped");
        assert_eq!((pv.name(), kind(pv)), (dv.name(), kind(dv)), "{name}: view name/kind");
        if let Some(Placement::Vertical { host_view_id, .. }) = pv.placement() {
            match dv.placement() {
                Some(Placement::Vertical { host_view_id: h, .. }) => assert_eq!(*h, mapped(&map, *host_view_id)?, "{name}: host"),
                other => panic!("{name}: placement lost: {other:?}"),
            }
        }
    }

    // Markers per view, ids mapped, compared field by field (all but `id`).
    let mut markers = 0;
    for pv in &p.views {
        let mut want: Vec<Marker> = compute_markers_for_view(p, pv.id())?
            .into_iter()
            .map(|mut m| -> Result<Marker, String> {
                m.view_id = mapped(&map, m.view_id)?;
                m.source_view_id = mapped(&map, m.source_view_id)?;
                m.source_entity_id = mapped(&map, m.source_entity_id)?;
                Ok(m)
            })
            .collect::<Result<_, _>>()?;
        let mut got = compute_markers_for_view(&dp, mapped(&map, pv.id())?)?;
        want.sort_by_key(sort_key);
        got.sort_by_key(sort_key);
        assert_eq!(got.len(), want.len(), "{name}: marker count in {}", pv.name());
        markers += got.len();
        for (g, w) in got.iter().zip(&want) {
            assert_eq!(
                (g.view_id, g.source_view_id, g.source_entity_id, g.source_segment_index, g.shape, g.filled, g.assumed_z),
                (w.view_id, w.source_view_id, w.source_entity_id, w.source_segment_index, w.shape, w.filled, w.assumed_z),
                "{name}: marker in {}",
                pv.name()
            );
            assert!(close(g.source_segment_param, w.source_segment_param), "{name}: segment param");
            assert!(near(g.position, w.position), "{name}: position {:?} vs {:?}", g.position, w.position);
            assert!(close(g.theta_deg, w.theta_deg), "{name}: theta");
        }
    }

    // Issues: same count and kinds.
    let kinds = |v: Vec<lamb_core::derivation::issues::Issue>| {
        let mut k: Vec<String> = v.iter().map(|i| format!("{:?}", i.kind)).collect();
        k.sort();
        k
    };
    assert_eq!(kinds(find_issues(&dp)?), kinds(find_issues(p)?), "{name}: issues");

    // Every surviving link resolves on the drawing.
    assert_eq!(dp.document().links.len() + report.dropped_links.len(), p.links.len(), "{name}: links accounted for");
    for link in dp.links() {
        for end in [&link.a, &link.b] {
            assert!(
                dp.links_for_segment(end.entity_id, end.segment_index).iter().any(|s| s.link.id == link.id),
                "{name}: link {} not found from {}",
                link.id,
                end.entity_id
            );
            let found = dp.views().any(|v| dp.entities(v.id).any(|e| e.id == end.entity_id && e.polyline.is_some()));
            assert!(found, "{name}: link end {} is not a polyline entity on the drawing", end.entity_id);
        }
    }
    Ok((report, markers))
}

#[test]
fn every_fixture_derives_the_same_on_the_drawing() -> R {
    for name in FIXTURES {
        let p = load(name)?;
        let (report, markers) = check_agrees(name, &p)?;
        assert!(report.unknown_layers.is_empty(), "{name}: {:?}", report.unknown_layers);
        eprintln!("{name}: {markers} markers, {} links, skipped {:?}", p.links.len(), report.skipped);
        if !p.links.is_empty() {
            assert!(markers > 0, "{name}: a linked project derives no markers, so the comparison proves nothing");
        }
    }
    Ok(())
}

#[test]
fn filleted_corners_reports_virtual_corners_per_polyline() -> R {
    let p = load("filleted-corners")?;
    let carriers: BTreeSet<Id> = p.views[0]
        .entities()
        .iter()
        .filter(|e| matches!(&e.geometry, EntityGeometry::Polyline { geometry } if geometry.segments.iter().any(|s| s.corner_at_to.is_some())))
        .map(|e| e.id)
        .collect();
    assert_eq!(carriers.len(), 3);
    let (_, _, report) = project_to_drawing(&p)?;
    let flagged: BTreeSet<Id> = report.flattened.iter().filter(|(_, f)| *f == Flattened::VirtualCorner).map(|(id, _)| *id).collect();
    assert_eq!(flagged, carriers);
    Ok(())
}

#[test]
fn text_entity_is_skipped_and_its_link_dropped() -> R {
    let mut p = load("u-wall-two-sections")?;
    let template = p.links.first().ok_or("fixture has no link")?.clone();
    let text_id = Id(9_000_001);
    let AnyView::Plan(plan) = p.views.iter_mut().find(|v| matches!(v, AnyView::Plan(_))).ok_or("no plan")? else {
        return Err("no plan".into());
    };
    plan.entities.push(DrawingEntity {
        id: text_id,
        view_id: plan.id,
        role: LineRole::Annotation,
        layer_id: "layer-annotation".into(),
        geometry: EntityGeometry::Text { position: Point2 { x: 1.0, y: 2.0 }, content: "A".into(), height: 100.0, rotation: 0.0 },
        color_override: None,
        lineweight_override: None,
        ai_origin: None,
    });
    let mut link = template;
    link.id = Id(9_000_002);
    link.a.entity_id = text_id;
    link.a.segment_index = 0;
    p.links.push(link);

    let (report, _) = check_agrees("u-wall-two-sections + text", &p)?;
    assert_eq!(report.skipped, vec![(text_id, "text")]);
    assert_eq!(report.dropped_links, vec![Id(9_000_002)]);
    Ok(())
}

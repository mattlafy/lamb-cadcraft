//! A lamb-core `Project` imported as a `Drawing` derives the same markers and issues on both sides
//! (`cadcraft-integration.md` §3.1 item 2), on every T299 `.lamb` fixture.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Once;

use cadcraft_doc::{DimKind, Dimension, Drawing, EntityKind, Handle, Space};
use cadcraft_engine::{Session, register_command_provider};
use lamb_cadcraft::convert::apply_corners;
use lamb_cadcraft::{DrawingProject, Flattened, IdMap, ImportReport, commands, project_to_drawing, read_entity_data, view_block_name};
use lamb_core::derivation::issues::find_issues;
use lamb_core::derivation::markers::{Marker, compute_markers_for_view};
use lamb_core::derivation::read::ProjectRead;
use lamb_core::geometry::dimension::{Axis, Dimension2, resolve_dimension};
use lamb_core::geometry::polyline::{CornerEntry, VirtualCornerEntry, virtual_corner_entries};
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::geometry::{Point2, PolySegment, Polyline2};
use lamb_core::model::{AnyView, DrawingEntity, EntityGeometry, HatchGeometry, HatchLoop, Id, LineRole, Placement, Project};
use lamb_core::persistence::files_to_project;
use serde_json::json;

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
    let markers = check_drawing(name, p, &d, &map, &report)?;
    Ok((report, markers))
}

/// `check_agrees` for a drawing already imported from `p` (or read back from its DXF).
fn check_drawing(name: &str, p: &Project, d: &Drawing, map: &IdMap, report: &ImportReport) -> Result<usize, Box<dyn std::error::Error>> {
    let dp = DrawingProject::new(d)?;
    let dviews = &dp.document().views;

    // Same views, names, kinds, intents, in order; hosts mapped.
    assert_eq!(dviews.len(), p.views.len(), "{name}: view count");
    for (pv, dv) in p.views.iter().zip(dviews) {
        assert_eq!(dv.id(), mapped(map, pv.id())?, "{name}: view id mapped");
        assert_eq!((pv.name(), kind(pv)), (dv.name(), kind(dv)), "{name}: view name/kind");
        if let Some(Placement::Vertical { host_view_id, .. }) = pv.placement() {
            match dv.placement() {
                Some(Placement::Vertical { host_view_id: h, .. }) => assert_eq!(*h, mapped(map, *host_view_id)?, "{name}: host"),
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
                m.view_id = mapped(map, m.view_id)?;
                m.source_view_id = mapped(map, m.source_view_id)?;
                m.source_entity_id = mapped(map, m.source_entity_id)?;
                Ok(m)
            })
            .collect::<Result<_, _>>()?;
        let mut got = compute_markers_for_view(&dp, mapped(map, pv.id())?)?;
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
    Ok(markers)
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

/// Every source polyline's `virtual_corner_entries`, keyed by its mapped (drawing) id.
fn source_corners(p: &Project, map: &IdMap) -> Result<BTreeMap<Id, Vec<CornerEntry>>, String> {
    let mut out = BTreeMap::new();
    for v in &p.views {
        for e in v.entities() {
            if let EntityGeometry::Polyline { geometry } = &e.geometry {
                out.insert(mapped(map, e.id)?, virtual_corner_entries(geometry));
            }
        }
    }
    Ok(out)
}

/// Every polyline entity's `virtual_corner_entries` as `DrawingProject` hands it to derivation.
fn drawing_corners(dp: &DrawingProject) -> BTreeMap<Id, Vec<CornerEntry>> {
    let mut out = BTreeMap::new();
    for v in dp.views() {
        for e in dp.entities(v.id) {
            if let Some(poly) = &e.polyline {
                out.insert(e.id, virtual_corner_entries(poly));
            }
        }
    }
    out
}

#[test]
fn filleted_corners_survive_import_and_are_not_flagged() -> R {
    let p = load("filleted-corners")?;
    let carriers: BTreeSet<Id> = p.views[0]
        .entities()
        .iter()
        .filter(|e| matches!(&e.geometry, EntityGeometry::Polyline { geometry } if geometry.segments.iter().any(|s| s.corner_at_to.is_some())))
        .map(|e| e.id)
        .collect();
    assert_eq!(carriers.len(), 3);
    let (d, map, report) = project_to_drawing(&p)?;
    let flagged: BTreeSet<Id> = report.flattened.iter().filter(|(_, f)| *f == Flattened::VirtualCorner).map(|(id, _)| *id).collect();
    assert!(flagged.is_empty(), "virtual corners still reported flattened: {flagged:?}");
    let want = source_corners(&p, &map)?;
    assert_eq!(want.values().filter(|c| !c.is_empty()).count(), 3);
    assert_eq!(drawing_corners(&DrawingProject::new(&d)?), want);
    Ok(())
}

#[test]
fn filleted_corners_survive_a_dxf_round_trip() -> R {
    let p = load("filleted-corners")?;
    let (d, map, report) = project_to_drawing(&p)?;
    let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(&d).as_bytes())?;
    assert_eq!(drawing_corners(&DrawingProject::new(&back)?), source_corners(&p, &map)?);

    // Derivation over the read-back drawing agrees with derivation over the source project.
    check_drawing("filleted-corners via DXF", &p, &back, &map, &report)?;
    Ok(())
}

#[test]
fn apply_corners_skips_an_index_that_is_not_a_corner() {
    let pt = |x: f64, y: f64| Point2 { x, y };
    let seg = |x: f64, y: f64| PolySegment { to: pt(x, y), bulge: 0.0, corner_at_to: None };
    let mut p = Polyline2 { start: pt(0.0, 0.0), segments: vec![seg(10.0, 0.0), seg(10.0, 5.0)], closed: false };
    let fillet = VirtualCornerEntry::Fillet { radius: 2.0 };
    let n = apply_corners(&mut p, &[CornerEntry { corner_index: 1, entry: fillet }, CornerEntry { corner_index: 7, entry: fillet }]);
    assert_eq!(n, 1);
    assert_eq!(virtual_corner_entries(&p), vec![CornerEntry { corner_index: 1, entry: fillet }]);
}

/// `p` with `entities` added to its first plan view; returns that plan's id.
fn add_to_plan(p: &mut Project, entities: Vec<(Id, LineRole, EntityGeometry)>) -> Result<Id, String> {
    let AnyView::Plan(plan) = p.views.iter_mut().find(|v| matches!(v, AnyView::Plan(_))).ok_or("no plan")? else {
        return Err("no plan".into());
    };
    for (id, role, geometry) in entities {
        plan.entities.push(DrawingEntity {
            id,
            view_id: plan.id,
            role,
            layer_id: "layer-annotation".into(),
            geometry,
            color_override: None,
            lineweight_override: None,
            ai_origin: None,
        });
    }
    Ok(plan.id)
}

/// The imported entity for old id `old`, checked to sit in its view's block.
fn imported<'a>(d: &'a Drawing, map: &IdMap, view: Id, old: Id) -> Result<&'a cadcraft_doc::Entity, String> {
    let h = Handle(mapped(map, old)?.0);
    let block = d.block(&view_block_name(mapped(map, view)?)).ok_or("no view block")?;
    assert!(block.entities.contains(h), "{old} not in its view's block");
    d.entity(h).map(|e| e.as_ref()).ok_or_else(|| format!("{old} not on the drawing"))
}

fn dimension_of(e: &cadcraft_doc::Entity) -> Result<&Dimension, String> {
    match &e.kind {
        EntityKind::Dimension(d) => Ok(d),
        other => Err(format!("not a dimension: {other:?}")),
    }
}

fn pt(x: f64, y: f64) -> Point2 {
    Point2 { x, y }
}

fn xy(v: cadcraft_doc::geom::Vec3) -> Point2 {
    Point2 { x: v.x, y: v.y }
}

/// `defpt` lies at the `to` end of the dimension line `resolve_dimension` draws — same side.
fn on_drawn_dimension_line(dim: &Dimension2, defpt: Point2, to: Point2) -> bool {
    resolve_dimension(dim, 100.0).lines.iter().any(|l| l.points.len() == 2 && near(l.points[1], defpt) && !near(l.points[0], to))
}

#[test]
fn text_imports_as_text_on_its_layer_with_its_role() -> R {
    let mut p = load("u-wall-two-sections")?;
    let text_id = Id(9_000_001);
    let text = EntityGeometry::Text { position: pt(1.0, 2.0), content: "A".into(), height: 250.0, rotation: 0.5 };
    let plan = add_to_plan(&mut p, vec![(text_id, LineRole::Annotation, text)])?;
    let (d, map, report) = project_to_drawing(&p)?;
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let e = imported(&d, &map, plan, text_id)?;
    let EntityKind::Text(t) = &e.kind else { return Err(format!("not a text: {:?}", e.kind).into()) };
    assert_eq!((t.insert.x, t.insert.y, t.height, t.rotation, t.value.as_str()), (1.0, 2.0, 250.0, 0.5, "A"));
    let layer = p.layers.iter().find(|l| l.id == "layer-annotation").ok_or("no annotation layer")?;
    assert_eq!(e.common.layer, layer.name);
    assert_eq!(read_entity_data(&e.common)?.role, LineRole::Annotation);
    Ok(())
}

#[test]
fn dimensions_import_with_their_points_on_the_drawn_side() -> R {
    let mut p = load("u-wall-two-sections")?;
    let lin_x = Dimension2::Linear { from: pt(0.0, 0.0), to: pt(3000.0, 0.0), axis: Axis::X, offset: 500.0, override_text: None };
    let lin_y = Dimension2::Linear { from: pt(100.0, 0.0), to: pt(400.0, 2000.0), axis: Axis::Y, offset: -700.0, override_text: None };
    let aligned = Dimension2::Aligned { from: pt(0.0, 0.0), to: pt(3000.0, 4000.0), offset: -600.0, override_text: Some("EQ".into()) };
    let radius = Dimension2::Radius { from: pt(10.0, 20.0), to: pt(510.0, 20.0), override_text: None };
    let angular = Dimension2::Angular { from: pt(1000.0, 0.0), to: pt(0.0, 1000.0), vertex: pt(0.0, 0.0), radius: 800.0, override_text: None };
    let dims = [lin_x, lin_y, aligned, radius, angular];
    let ids: Vec<Id> = (0..dims.len() as u64).map(|i| Id(9_100_000 + i)).collect();
    let entities = ids.iter().zip(&dims).map(|(id, dim)| (*id, LineRole::Annotation, EntityGeometry::Dimension { dimension: dim.clone() })).collect();
    let plan = add_to_plan(&mut p, entities)?;
    let (d, map, report) = project_to_drawing(&p)?;
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let got: Vec<&Dimension> = ids.iter().map(|id| imported(&d, &map, plan, *id).and_then(dimension_of)).collect::<Result<_, _>>()?;

    // Linear X: ends at p13/p14, defpt on the dimension line 500 up.
    assert_eq!(got[0].kind, DimKind::Linear { rotation: 0.0 });
    assert_eq!((xy(got[0].p13), xy(got[0].p14)), (pt(0.0, 0.0), pt(3000.0, 0.0)));
    assert!(close(got[0].defpt.y, 500.0) && close(got[0].defpt.x, 3000.0), "{:?}", got[0].defpt);
    assert!(on_drawn_dimension_line(&dims[0], xy(got[0].defpt), pt(3000.0, 0.0)));
    assert!(got[0].text.is_empty() && got[0].assoc.is_empty());

    // Linear Y and aligned with negative offsets: same side as lamb-core draws.
    assert_eq!(got[1].kind, DimKind::Linear { rotation: std::f64::consts::FRAC_PI_2 });
    assert!(near(xy(got[1].defpt), pt(800.0, 2000.0)), "{:?}", got[1].defpt);
    assert!(on_drawn_dimension_line(&dims[1], xy(got[1].defpt), pt(400.0, 2000.0)));
    assert_eq!(got[2].kind, DimKind::Aligned);
    assert!(on_drawn_dimension_line(&dims[2], xy(got[2].defpt), pt(3000.0, 4000.0)));
    assert_eq!(got[2].text, "EQ");

    // Radius: defpt = centre, p15 = on the curve.
    assert_eq!(got[3].kind, DimKind::Radius);
    assert_eq!((xy(got[3].defpt), xy(got[3].p15)), (pt(10.0, 20.0), pt(510.0, 20.0)));

    // Angular: rays at p13/p14, vertex at p15, defpt on the arc at mid-sweep (45 degrees here).
    assert_eq!(got[4].kind, DimKind::Angular3P);
    assert_eq!((xy(got[4].p13), xy(got[4].p14), xy(got[4].p15)), (pt(1000.0, 0.0), pt(0.0, 1000.0), pt(0.0, 0.0)));
    let h = 800.0 * std::f64::consts::FRAC_1_SQRT_2;
    assert!(near(xy(got[4].defpt), pt(h, h)), "{:?}", got[4].defpt);
    Ok(())
}

#[test]
fn a_link_to_a_dimension_is_kept_and_nothing_is_skipped() -> R {
    let mut p = load("u-wall-two-sections")?;
    let template = p.links.first().ok_or("fixture has no link")?.clone();
    let dim_id = Id(9_000_001);
    let dim = Dimension2::Aligned { from: pt(0.0, 0.0), to: pt(1000.0, 0.0), offset: 300.0, override_text: None };
    let text = EntityGeometry::Text { position: pt(1.0, 2.0), content: "A".into(), height: 100.0, rotation: 0.0 };
    let circle = EntityGeometry::Circle { center: pt(5000.0, 5000.0), radius: 250.0 };
    add_to_plan(
        &mut p,
        vec![
            (dim_id, LineRole::Annotation, EntityGeometry::Dimension { dimension: dim }),
            (Id(9_000_003), LineRole::Annotation, text),
            (Id(9_000_004), LineRole::Annotation, circle),
        ],
    )?;
    let mut link = template;
    link.id = Id(9_000_002);
    link.a.entity_id = dim_id;
    link.a.segment_index = 0;
    p.links.push(link);

    let (d, map, report) = project_to_drawing(&p)?;
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert!(report.dropped_links.is_empty(), "{:?}", report.dropped_links);
    let dp = DrawingProject::new(&d)?;
    let kept = dp.document().links.iter().find(|l| l.id == Id(9_000_002)).ok_or("dimension link dropped")?;
    assert_eq!(kept.a.entity_id, mapped(&map, dim_id)?);
    // The legacy circle became a CADCraft circle.
    let c = d.entity(Handle(mapped(&map, Id(9_000_004))?.0)).ok_or("circle missing")?;
    assert!(matches!(c.kind, EntityKind::Circle(_)), "{:?}", c.kind);
    Ok(())
}

static REGISTER: Once = Once::new();

fn register() {
    REGISTER.call_once(|| register_command_provider(commands).expect("register LAMB commands"));
}

fn seg(x: f64, y: f64, bulge: f64) -> PolySegment {
    PolySegment { to: pt(x, y), bulge, corner_at_to: None }
}

/// A closed square, side `s`, lower-left at (`x`, `y`).
fn square(x: f64, y: f64, s: f64) -> Polyline2 {
    Polyline2 { start: pt(x, y), segments: vec![seg(x + s, y, 0.0), seg(x + s, y + s, 0.0), seg(x, y + s, 0.0), seg(x, y, 0.0)], closed: true }
}

fn hatch(loops: Vec<(Polyline2, Vec<Id>)>, pattern: &str, angle: f64, scale: f64) -> EntityGeometry {
    let loops = loops.into_iter().map(|(polyline, entity_ids)| HatchLoop { polyline, entity_ids }).collect();
    EntityGeometry::Hatch(HatchGeometry { boundary: None, boundary_entity_id: None, loops: Some(loops), pattern: pattern.into(), scale, angle })
}

fn hatch_of(e: &cadcraft_doc::Entity) -> Result<&cadcraft_doc::Hatch, String> {
    match &e.kind {
        EntityKind::Hatch(h) => Ok(h),
        other => Err(format!("not a hatch: {other:?}")),
    }
}

const RECT: Id = Id(9_200_001);
const HATCH: Id = Id(9_200_002);

/// The u-wall project with a 1000 mm square at (50000, 50000) and an ANSI31 hatch naming it; the
/// hatch first in the plan's list when `hatch_first`. Returns the project and the plan's id.
fn hatched_project(hatch_first: bool) -> Result<(Project, Id), Box<dyn std::error::Error>> {
    let mut p = load("u-wall-two-sections")?;
    let rect = (RECT, LineRole::Annotation, EntityGeometry::Polyline { geometry: square(50_000.0, 50_000.0, 1000.0) });
    let h = (HATCH, LineRole::Annotation, hatch(vec![(square(50_000.0, 50_000.0, 1000.0), vec![RECT])], "ansi31", 45.0, 2.0));
    let plan = add_to_plan(&mut p, if hatch_first { vec![h, rect] } else { vec![rect, h] })?;
    Ok((p, plan))
}

#[test]
fn a_hatch_imports_associative_to_its_boundary_in_either_order() -> R {
    for hatch_first in [false, true] {
        let (p, plan) = hatched_project(hatch_first)?;
        let (d, map, report) = project_to_drawing(&p)?;
        assert!(report.skipped.is_empty() && report.unassociated_hatches.is_empty() && report.unknown_patterns.is_empty(), "{report:?}");
        let e = imported(&d, &map, plan, HATCH)?;
        let h = hatch_of(e)?;
        assert!(h.associative && !h.solid, "hatch_first {hatch_first}");
        assert_eq!((h.pattern.as_str(), h.scale, h.style), ("ANSI31", 2.0, 0));
        assert!((h.angle - std::f64::consts::FRAC_PI_4).abs() < 1e-12, "{}", h.angle);
        assert_eq!(h.loops.len(), 1);
        assert!(h.loops[0].outer);
        assert_eq!(h.loops[0].sources, vec![Handle(mapped(&map, RECT)?.0)]);
        assert_eq!(h.loops[0].vertices.len(), 4);
        let layer = p.layers.iter().find(|l| l.id == "layer-annotation").ok_or("no annotation layer")?;
        assert_eq!(e.common.layer, layer.name);
        assert_eq!(read_entity_data(&e.common)?.role, LineRole::Annotation);
    }
    Ok(())
}

#[test]
fn a_holed_hatch_keeps_its_bulges_and_a_lost_boundary_is_reported() -> R {
    let mut p = load("u-wall-two-sections")?;
    let (outer, circle, holed, lost) = (Id(9_300_001), Id(9_300_003), Id(9_300_004), Id(9_300_005));
    // Outer: a square; hole: a two-arc circle (a CADCraft Circle as an entity, vertices in the loop).
    let hole = Polyline2 { start: pt(400.0, 500.0), segments: vec![seg(600.0, 500.0, 1.0), seg(400.0, 500.0, 1.0)], closed: true };
    let holed_hatch = hatch(vec![(square(0.0, 0.0, 1000.0), vec![outer]), (hole.clone(), vec![circle])], "ansi31", 0.0, 1.0);
    let lost_hatch = hatch(vec![(square(0.0, 0.0, 10.0), vec![Id(9_399_999)])], "my-own", 0.0, 1.0);
    let plan = add_to_plan(
        &mut p,
        vec![
            (outer, LineRole::Annotation, EntityGeometry::Polyline { geometry: square(0.0, 0.0, 1000.0) }),
            (circle, LineRole::Annotation, EntityGeometry::Polyline { geometry: hole }),
            (holed, LineRole::Annotation, holed_hatch),
            (lost, LineRole::Annotation, lost_hatch),
        ],
    )?;
    let (d, map, report) = project_to_drawing(&p)?;
    assert!(matches!(d.entity(Handle(mapped(&map, circle)?.0)).ok_or("no circle")?.kind, EntityKind::Circle(_)));

    let h = hatch_of(imported(&d, &map, plan, holed)?)?;
    assert!(h.associative);
    assert_eq!(h.loops.iter().map(|l| l.outer).collect::<Vec<_>>(), vec![true, false]);
    let bulges: Vec<f64> = h.loops[1].vertices.iter().map(|v| v.bulge).collect();
    assert_eq!(bulges, vec![1.0, 1.0], "{:?}", h.loops[1].vertices);
    assert_eq!(h.loops[1].sources, vec![Handle(mapped(&map, circle)?.0)]);

    let l = hatch_of(imported(&d, &map, plan, lost)?)?;
    assert!(!l.associative && l.loops.iter().all(|x| x.sources.is_empty()));
    assert_eq!(l.pattern, "MY-OWN");
    assert_eq!(report.unassociated_hatches, vec![lost]);
    assert_eq!(report.unknown_patterns, vec!["MY-OWN".to_owned()]);
    Ok(())
}

#[test]
fn an_imported_hatch_refills_when_its_boundary_is_stretched_in_the_view() -> R {
    register();
    let (p, plan) = hatched_project(false)?;
    let (d, map, _) = project_to_drawing(&p)?;
    let mut s = Session::new();
    s.open_drawing(d, "hatched", None);
    s.execute("editview", &json!({"viewId": mapped(&map, plan)?.0}))?;
    assert_eq!(s.space(), Space::Block(view_block_name(mapped(&map, plan)?)));
    s.script("STRETCH\nC\n51200,49900\n50800,51100\n\n0,0\n500,0\n")?;
    assert!(s.running.is_none(), "{}", s.prompt_text());
    let h = hatch_of(s.doc()?.entity(Handle(mapped(&map, HATCH)?.0)).ok_or("hatch gone")?)?;
    assert!(h.associative);
    let max_x = h.loops[0].vertices.iter().map(|v| v.p.x).fold(f64::MIN, f64::max);
    assert!((max_x - 51_500.0).abs() < 1e-6, "hatch did not refill: {:?}", h.loops[0].vertices);
    Ok(())
}

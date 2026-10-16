//! A CADCraft `Drawing` exported as a lamb-core `Project` — the inverse of [`crate::import`], for
//! writing a `.lamb` project with `lamb_core::persistence::write_lamb_dir` (LAMBSAVE,
//! [`crate::lambopen`]).
//!
//! Built on [`crate::project::parse`]: the document's name, defaults, levels, links and views come
//! from `ext["LAMB"]`, the layers from the drawing (id = name), and each view's entities from its
//! view block, in block order. Ids are handles (`Id(handle)`), so a re-save of an imported project
//! renames its ids once. Entities outside view blocks (model/paper space, sheets, assembly blocks)
//! are not exported; `mitre_plans` has no TS field and is dropped (counted in the report).
//!
//! Kinds: Line / Arc / Circle / LwPolyline → polyline (virtual-corner tags re-applied; a circle
//! stays the two-arc polyline), TEXT → text, linear (0 or 90 degrees), aligned, radius and
//! three-point angular DIMENSIONs → `Dimension2`, HATCH → loops with the boundary entities as their
//! `entity_ids` and every builtin pattern a hatch names added to `patterns`. Anything else is
//! skipped and listed. Fork-only `EntityData` fields (`detail_tag`, `detail_ref`, `repeat`,
//! `outside`, `view_id`) have no TS slot: the geometry still exports, the tag is listed as dropped.

use std::collections::HashMap;

use cadcraft_doc::{DimKind, Dimension, Entity, EntityKind, Handle, Hatch, LwPolyline};
use lamb_core::geometry::Point2;
use lamb_core::geometry::dimension::{Axis, Dimension2};
use lamb_core::geometry::hatch_pattern::find_builtin_pattern;
use lamb_core::geometry::tolerance::{TOLERANCE, deg_to_rad};
use lamb_core::model::{
    AnyView, CURRENT_SCHEMA_VERSION, DrawingEntity, EntityGeometry, HatchGeometry, HatchLoop, Id, LineRole, Project, empty_project,
};

use crate::convert::{apply_corners, lw_to_polyline, to_polyline};
use crate::project::parse;
use crate::schema::{EntityData, SchemaError, view_block_name};

/// What the export left out; nothing in it is an error.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportReport {
    /// Entities in view blocks not exported (handle id, `EntityKind::type_name()`).
    pub skipped: Vec<(Id, &'static str)>,
    /// Fork-only `EntityData` fields dropped (handle id, field name); the geometry exported.
    pub dropped_tags: Vec<(Id, &'static str)>,
    /// Hatch pattern names with no builtin definition (SOLID included), each listed once.
    pub unknown_patterns: Vec<String>,
    /// Plan views in `mitre_plans`, which has no TS field.
    pub dropped_mitre_plans: usize,
}

fn p2(v: cadcraft_doc::geom::Vec3) -> Point2 {
    Point2 { x: v.x, y: v.y }
}

fn sub(a: Point2, b: Point2) -> Point2 {
    Point2 { x: a.x - b.x, y: a.y - b.y }
}

fn dot(a: Point2, b: Point2) -> f64 {
    a.x * b.x + a.y * b.y
}

fn unit(p: Point2) -> Point2 {
    let l = p.x.hypot(p.y);
    if l > 0.0 { Point2 { x: p.x / l, y: p.y / l } } else { Point2 { x: 0.0, y: 0.0 } }
}

/// `(defpt − from)·n`, n the left normal of `dir`: the inverse of import's dimension-line foot.
fn offset_of(defpt: Point2, from: Point2, dir: Point2) -> f64 {
    dot(sub(defpt, from), Point2 { x: -dir.y, y: dir.x })
}

/// The axis a linear dimension's rotation names, when it is 0 or 90 degrees (mod 180).
fn linear_axis(rotation: f64) -> Option<Axis> {
    let r = rotation.rem_euclid(std::f64::consts::PI);
    let tol = deg_to_rad(TOLERANCE.angle_deg);
    if r <= tol || std::f64::consts::PI - r <= tol {
        Some(Axis::X)
    } else if (r - std::f64::consts::FRAC_PI_2).abs() <= tol {
        Some(Axis::Y)
    } else {
        None
    }
}

/// A DIMENSION as `Dimension2` — the inverse of import's `dimension_kind` — or `None` for a kind
/// lamb-core has no form for.
fn dimension_of(d: &Dimension) -> Option<Dimension2> {
    let override_text = Some(d.text.clone()).filter(|t| !t.is_empty());
    let (defpt, from, to) = (p2(d.defpt), p2(d.p13), p2(d.p14));
    Some(match d.kind {
        DimKind::Linear { rotation } => {
            let axis = linear_axis(rotation)?;
            let dir = match axis {
                Axis::X => Point2 { x: 1.0, y: 0.0 },
                Axis::Y => Point2 { x: 0.0, y: 1.0 },
            };
            Dimension2::Linear { from, to, axis, offset: offset_of(defpt, from, dir), override_text }
        }
        DimKind::Aligned => Dimension2::Aligned { from, to, offset: offset_of(defpt, from, unit(sub(to, from))), override_text },
        DimKind::Radius => Dimension2::Radius { from: defpt, to: p2(d.p15), override_text },
        DimKind::Angular3P => {
            let vertex = p2(d.p15);
            let r = sub(defpt, vertex);
            Dimension2::Angular { from, to, vertex, radius: r.x.hypot(r.y), override_text }
        }
        _ => return None,
    })
}

/// A HATCH as lamb-core's loop form; each loop's polyline from its vertices as a closed LwPolyline.
fn hatch_of(h: &Hatch) -> HatchGeometry {
    let loops = h
        .loops
        .iter()
        .filter_map(|l| {
            let lw = LwPolyline { vertices: l.vertices.clone(), closed: true, const_width: 0.0, elevation: 0.0, plinegen: false };
            let polyline = lw_to_polyline(&lw, false, &mut Vec::new())?;
            Some(HatchLoop { polyline, entity_ids: l.sources.iter().map(|s| Id(s.0)).collect() })
        })
        .collect();
    HatchGeometry {
        boundary: None,
        boundary_entity_id: None,
        loops: Some(loops),
        pattern: h.pattern.clone(),
        scale: h.scale,
        angle: h.angle.to_degrees(),
    }
}

/// What `e` exports as, or `None` when it is skipped.
fn geometry_of(e: &Entity, data: Option<&EntityData>) -> Option<EntityGeometry> {
    match &e.kind {
        EntityKind::Text(t) => {
            Some(EntityGeometry::Text { position: p2(t.insert), content: t.value.clone(), height: t.height, rotation: t.rotation })
        }
        EntityKind::Dimension(d) => dimension_of(d).map(|dimension| EntityGeometry::Dimension { dimension }),
        EntityKind::Hatch(h) => Some(EntityGeometry::Hatch(hatch_of(h))),
        _ => {
            let mut geometry = to_polyline(e).ok()?.polyline;
            if let Some(d) = data {
                apply_corners(&mut geometry, &d.corners);
            }
            Some(EntityGeometry::Polyline { geometry })
        }
    }
}

/// The fork-only fields set on `d`, by name.
fn fork_tags(d: &EntityData) -> Vec<&'static str> {
    let mut tags = Vec::new();
    if d.view_id.is_some() {
        tags.push("view_id");
    }
    if d.detail_tag.is_some() {
        tags.push("detail_tag");
    }
    if d.detail_ref.is_some() {
        tags.push("detail_ref");
    }
    if d.repeat.is_some() {
        tags.push("repeat");
    }
    if d.outside {
        tags.push("outside");
    }
    tags
}

fn entities_mut(v: &mut AnyView) -> &mut Vec<DrawingEntity> {
    match v {
        AnyView::Plan(p) => &mut p.entities,
        AnyView::Vertical(p) => &mut p.entities,
        AnyView::Detail(p) => &mut p.entities,
    }
}

/// Exports `d` as a lamb-core project. See the module docs for what is mapped how.
pub fn drawing_to_project(d: &cadcraft_doc::Drawing) -> Result<(Project, ExportReport), SchemaError> {
    let parsed = parse(d)?;
    let doc = parsed.doc;
    let entity_data: &HashMap<Handle, EntityData> = &parsed.entity_data;
    let mut report = ExportReport { dropped_mitre_plans: doc.mitre_plans.len(), ..ExportReport::default() };

    let mut project = empty_project(Some(&doc.name));
    project.schema_version = CURRENT_SCHEMA_VERSION;
    project.defaults = doc.defaults.clone();
    project.levels = doc.levels.clone();
    project.links = doc.links.clone();
    project.layers = parsed.layers;
    project.suppressed_proposals = Some(doc.suppressed_proposals.clone()).filter(|s| !s.is_empty());

    for v in &doc.views {
        let mut view = v.clone();
        let entities = entities_mut(&mut view);
        entities.clear();
        let Some(block) = d.block(&view_block_name(v.id())) else {
            project.views.push(view);
            continue;
        };
        for e in block.entities.iter() {
            let id = Id(e.handle.0);
            let data = entity_data.get(&e.handle);
            let Some(geometry) = geometry_of(e, data) else {
                report.skipped.push((id, e.kind.type_name()));
                continue;
            };
            if let EntityGeometry::Hatch(h) = &geometry
                && !project.patterns.contains_key(&h.pattern)
                && !report.unknown_patterns.contains(&h.pattern)
            {
                match find_builtin_pattern(&h.pattern) {
                    Some(def) => {
                        project.patterns.insert(h.pattern.clone(), def);
                    }
                    None => report.unknown_patterns.push(h.pattern.clone()),
                }
            }
            if let Some(data) = data {
                report.dropped_tags.extend(fork_tags(data).into_iter().map(|t| (id, t)));
            }
            entities.push(DrawingEntity {
                id,
                view_id: v.id(),
                role: data.map_or(LineRole::Bylayer, |x| x.role),
                layer_id: e.common.layer.clone(),
                geometry,
                color_override: None,
                lineweight_override: None,
                ai_origin: data.and_then(|x| x.ai_origin.clone()),
            });
        }
        project.views.push(view);
    }
    Ok((project, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{IdMap, project_to_drawing};
    use lamb_core::geometry::polyline::VirtualCornerEntry;
    use lamb_core::geometry::{PolySegment, Polyline2};
    use lamb_core::model::{AiOrigin, PlanView};
    use serde_json::json;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;

    fn pt(x: f64, y: f64) -> Point2 {
        Point2 { x, y }
    }

    fn near(a: Point2, b: Point2) -> bool {
        (a.x - b.x).hypot(a.y - b.y) <= TOLERANCE.coincidence_mm
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= TOLERANCE.coincidence_mm
    }

    fn poly_eq(a: &Polyline2, b: &Polyline2) -> bool {
        a.closed == b.closed
            && near(a.start, b.start)
            && a.segments.len() == b.segments.len()
            && a.segments.iter().zip(&b.segments).all(|(s, t)| near(s.to, t.to) && close(s.bulge, t.bulge) && s.corner_at_to == t.corner_at_to)
    }

    fn opt_eq(a: &Option<String>, b: &Option<String>) -> bool {
        a == b
    }

    fn dim_eq(a: &Dimension2, b: &Dimension2) -> bool {
        use Dimension2 as D;
        match (a, b) {
            (D::Linear { from, to, axis, offset, override_text: o }, D::Linear { from: f2, to: t2, axis: a2, offset: off2, override_text: o2 }) => {
                near(*from, *f2) && near(*to, *t2) && axis == a2 && close(*offset, *off2) && opt_eq(o, o2)
            }
            (D::Aligned { from, to, offset, override_text: o }, D::Aligned { from: f2, to: t2, offset: off2, override_text: o2 }) => {
                near(*from, *f2) && near(*to, *t2) && close(*offset, *off2) && opt_eq(o, o2)
            }
            (D::Radius { from, to, override_text: o }, D::Radius { from: f2, to: t2, override_text: o2 }) => {
                near(*from, *f2) && near(*to, *t2) && opt_eq(o, o2)
            }
            (
                D::Angular { from, to, vertex, radius, override_text: o },
                D::Angular { from: f2, to: t2, vertex: v2, radius: r2, override_text: o2 },
            ) => near(*from, *f2) && near(*to, *t2) && near(*vertex, *v2) && close(*radius, *r2) && opt_eq(o, o2),
            _ => false,
        }
    }

    /// `a` (original) equals `b` (round-tripped), hatch ids compared through `map`.
    fn geometry_eq(a: &EntityGeometry, b: &EntityGeometry, map: &IdMap) -> bool {
        match (a, b) {
            (EntityGeometry::Polyline { geometry: g }, EntityGeometry::Polyline { geometry: h }) => poly_eq(g, h),
            (
                EntityGeometry::Text { position, content, height, rotation },
                EntityGeometry::Text { position: p2, content: c2, height: h2, rotation: r2 },
            ) => near(*position, *p2) && content == c2 && close(*height, *h2) && close(*rotation, *r2),
            (EntityGeometry::Dimension { dimension: x }, EntityGeometry::Dimension { dimension: y }) => dim_eq(x, y),
            (EntityGeometry::Hatch(x), EntityGeometry::Hatch(y)) => {
                let (Some(lx), Some(ly)) = (&x.loops, &y.loops) else { return false };
                x.pattern.eq_ignore_ascii_case(&y.pattern)
                    && close(x.scale, y.scale)
                    && close(x.angle, y.angle)
                    && lx.len() == ly.len()
                    && lx.iter().zip(ly).all(|(l, m)| {
                        poly_eq(&l.polyline, &m.polyline)
                            && l.entity_ids.iter().map(|i| map.get(*i)).collect::<Vec<_>>()
                                == m.entity_ids.iter().map(|i| Some(*i)).collect::<Vec<_>>()
                    })
            }
            _ => false,
        }
    }

    fn seg(x: f64, y: f64) -> PolySegment {
        PolySegment { to: pt(x, y), bulge: 0.0, corner_at_to: None }
    }

    /// One plan view: a closed square with a filleted corner, text, every dimension kind, an
    /// associative hatch over the square, and an AI origin on the text.
    fn hand_built() -> std::result::Result<(Project, Vec<Id>), Box<dyn std::error::Error>> {
        let mut p = empty_project(Some("Round trip"));
        let layer = p.layers.first().ok_or("no default layer")?.id.clone();
        let mut square =
            Polyline2 { start: pt(0.0, 0.0), segments: vec![seg(4000.0, 0.0), seg(4000.0, 3000.0), seg(0.0, 3000.0), seg(0.0, 0.0)], closed: true };
        square.segments[1].corner_at_to = Some(VirtualCornerEntry::Fillet { radius: 300.0 });
        let rect = Id(101);
        let mut origin: AiOrigin = serde_json::from_value(json!({ "proposalId": "manual", "accepted": false }))?;
        origin.source_entity_id = Some(rect);
        let geoms = vec![
            EntityGeometry::Polyline { geometry: square.clone() },
            EntityGeometry::Text { position: pt(100.0, 200.0), content: "ROOM".into(), height: 250.0, rotation: 0.3 },
            EntityGeometry::Dimension {
                dimension: Dimension2::Linear { from: pt(0.0, 0.0), to: pt(3000.0, 0.0), axis: Axis::X, offset: 500.0, override_text: None },
            },
            EntityGeometry::Dimension {
                dimension: Dimension2::Linear { from: pt(100.0, 0.0), to: pt(400.0, 2000.0), axis: Axis::Y, offset: -700.0, override_text: None },
            },
            EntityGeometry::Dimension {
                dimension: Dimension2::Aligned { from: pt(0.0, 0.0), to: pt(3000.0, 4000.0), offset: -600.0, override_text: Some("EQ".into()) },
            },
            EntityGeometry::Dimension { dimension: Dimension2::Radius { from: pt(10.0, 20.0), to: pt(510.0, 20.0), override_text: None } },
            EntityGeometry::Dimension {
                dimension: Dimension2::Angular {
                    from: pt(1000.0, 0.0),
                    to: pt(0.0, 1000.0),
                    vertex: pt(0.0, 0.0),
                    radius: 800.0,
                    override_text: None,
                },
            },
            EntityGeometry::Hatch(HatchGeometry {
                boundary: None,
                boundary_entity_id: None,
                loops: Some(vec![HatchLoop {
                    polyline: Polyline2 { segments: square.segments.iter().map(|s| seg(s.to.x, s.to.y)).collect(), ..square.clone() },
                    entity_ids: vec![rect],
                }]),
                pattern: "ANSI31".into(),
                scale: 2.0,
                angle: 45.0,
            }),
        ];
        let view = Id(1);
        let ids: Vec<Id> = (0..geoms.len() as u64).map(|i| Id(rect.0 + i)).collect();
        let entities = ids
            .iter()
            .zip(geoms)
            .map(|(id, geometry)| DrawingEntity {
                id: *id,
                view_id: view,
                role: LineRole::Annotation,
                layer_id: layer.clone(),
                ai_origin: (*id == Id(102)).then(|| origin.clone()),
                geometry,
                color_override: None,
                lineweight_override: None,
            })
            .collect();
        p.views.push(AnyView::Plan(PlanView { id: view, name: "Ground".into(), entities, placement: None }));
        Ok((p, ids))
    }

    #[test]
    fn a_hand_built_project_round_trips_through_a_drawing() -> R {
        let (p, ids) = hand_built()?;
        let (d, map, ireport) = project_to_drawing(&p)?;
        assert!(ireport.skipped.is_empty(), "{ireport:?}");
        let (q, report) = drawing_to_project(&d)?;
        assert!(report.skipped.is_empty() && report.dropped_tags.is_empty() && report.unknown_patterns.is_empty(), "{report:?}");
        assert_eq!(q.views.len(), 1);
        let (src, got) = (p.views[0].entities(), q.views[0].entities());
        assert_eq!(src.len(), got.len());
        assert_eq!(q.views[0].name(), "Ground");
        for (a, id) in src.iter().zip(&ids) {
            let new = map.get(*id).ok_or("unmapped")?;
            let b = got.iter().find(|e| e.id == new).ok_or("entity missing")?;
            assert!(geometry_eq(&a.geometry, &b.geometry, &map), "{id}: {:?} vs {:?}", a.geometry, b.geometry);
            assert_eq!(b.role, LineRole::Annotation);
            assert_eq!(b.view_id, q.views[0].id());
        }
        let hatch = got.iter().find(|e| matches!(e.geometry, EntityGeometry::Hatch(_))).ok_or("no hatch")?;
        let EntityGeometry::Hatch(h) = &hatch.geometry else { return Err("no hatch".into()) };
        assert_eq!(h.loops.as_ref().ok_or("no loops")?[0].entity_ids, vec![map.get(ids[0]).ok_or("unmapped")?]);
        assert!(q.patterns.contains_key("ANSI31"));
        let text = got.iter().find(|e| e.id == map.get(ids[1]).unwrap_or(Id(0))).ok_or("no text")?;
        let o = text.ai_origin.as_ref().ok_or("ai origin lost")?;
        assert_eq!((o.proposal_id.as_str(), o.accepted, o.source_entity_id), ("manual", false, map.get(ids[0])));
        Ok(())
    }

    #[test]
    fn a_rotated_linear_dimension_and_solid_hatch_are_reported() -> R {
        let (p, _) = hand_built()?;
        let (mut d, _, _) = project_to_drawing(&p)?;
        let view = crate::schema::read_document(&d)?.views[0].id();
        let block = d.block(&view_block_name(view)).ok_or("no block")?;
        let handles: Vec<Handle> = block.entities.iter().map(|e| e.handle).collect();
        for h in handles {
            d.modify_entity(h, |e| match &mut e.kind {
                EntityKind::Dimension(x) if matches!(x.kind, DimKind::Linear { .. }) => x.kind = DimKind::Linear { rotation: 0.3 },
                EntityKind::Hatch(x) => x.pattern = "SOLID".into(),
                _ => {}
            })?;
        }
        let (q, report) = drawing_to_project(&d)?;
        assert_eq!(report.skipped.iter().map(|s| s.1).collect::<Vec<_>>(), vec!["Dimension", "Dimension"]);
        assert_eq!(report.unknown_patterns, vec!["SOLID".to_owned()]);
        assert!(q.patterns.is_empty());
        Ok(())
    }
}

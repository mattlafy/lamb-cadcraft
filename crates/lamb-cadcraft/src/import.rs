//! A lamb-core `Project` (an existing `.lamb` file, read by `lamb_core::persistence`) imported as a
//! CADCraft `Drawing` (`cadcraft-integration.md` §3.1 item 2, §4): the one-time string-id -> handle
//! migration.
//!
//! Built only through [`create_view`] and the `ext["LAMB"]` schema. Every view and every imported
//! entity gets a fresh handle, recorded old -> new in the returned [`IdMap`]; placements' hosts,
//! detail hosts, links and AI-origin source ids are rewritten through that map, never by value.
//! Level and link ids keep their values (they live only in `ext["LAMB"]`).
//!
//! Layers are named by `DrawingLayer.name` and entities' `layer_id` (the TS layer id) becomes that
//! name. Colour: the `#rrggbb` hex as a true colour; lineweight: the nearest of CADCraft's
//! `Lineweight::STANDARD` values — the reverse of [`crate::project`]'s mapping.
//!
//! Virtual (fillet/chamfer) corner tags go into the entity's `EntityData.corners`, so they are not
//! reported as flattened — unless the polyline became a Circle, which has no vertices to tag.
//!
//! Polylines (and legacy circles, through `circle_as_polyline`) import as polylines/circles, text
//! as TEXT (left-baseline, rotation in radians as both sides store it) and dimensions as
//! non-associative DIMENSIONs in the drawing's current dimension style, their points mapped onto
//! the DXF definition points. A link to a skipped entity is dropped and listed.
//!
//! Hatches import last, once every other entity has its handle, as HATCHes in their own view's
//! block, associative to their imported boundary entities (each loop's `sources`). A hatch with a
//! loop naming no entity, or one that was not imported, goes in non-associative with no sources and
//! is listed in [`ImportReport::unassociated_hatches`]. Fill is even-odd (style 0), the pattern name
//! upper-cased (an unknown one listed in [`ImportReport::unknown_patterns`]), degrees to radians.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use cadcraft_doc::color::{Color, Rgb};
use cadcraft_doc::geom::Vec3;
use cadcraft_doc::geom::{PolyVertex, Vec2};
use cadcraft_doc::{Common, DimKind, Dimension, Drawing, EntityKind, HAlign, Handle, Hatch, HatchLoop, Lineweight, Space, Text, VAlign};
use lamb_core::derivation::proposals::remap_slot_key;
use lamb_core::geometry::Point2;
use lamb_core::geometry::Polyline2;
use lamb_core::geometry::circle_fit::{CircleFit, circle_as_polyline};
use lamb_core::geometry::dimension::{Axis, Dimension2};
use lamb_core::geometry::polyline::CornerEntry;
use lamb_core::geometry::polyline::virtual_corner_entries;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AnyView, DrawingEntity, DrawingLayer, EntityGeometry, HatchGeometry, Id, Placement, Project, ProjectDefaults, hatch_loops};

use crate::convert::{Flattened, from_polyline};
use crate::schema::{EntityData, LayerData, SchemaError, read_document, set_entity_data, set_layer_data, view_block_name, write_document};
use crate::views::{ViewError, create_view};

/// Old (lamb-core) id -> new (handle) id, for views and imported entities.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdMap(pub BTreeMap<Id, Id>);

impl IdMap {
    pub fn get(&self, old: Id) -> Option<Id> {
        self.0.get(&old).copied()
    }
}

/// What the import left out or changed; nothing in it is an error.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportReport {
    /// Entities not imported (old id, geometry kind).
    pub skipped: Vec<(Id, &'static str)>,
    /// Per imported entity (old id), each thing the import dropped (virtual corners only when the
    /// entity kind has no vertices to carry them).
    pub flattened: Vec<(Id, Flattened)>,
    /// Links (by id) dropped because an end's entity was not imported.
    pub dropped_links: Vec<Id>,
    /// Entity `layer_id`s naming no project layer; those entities went on layer `0`.
    pub unknown_layers: Vec<String>,
    /// Hatches (old id) imported non-associative: a loop names no entity, or one not imported.
    pub unassociated_hatches: Vec<Id>,
    /// Hatch pattern names (upper-cased) the pattern library does not know, each listed once.
    pub unknown_patterns: Vec<String>,
}

/// Why an import failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportError {
    View(ViewError),
    Schema(SchemaError),
    /// A CADCraft document operation failed.
    Doc(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::View(e) => e.fmt(f),
            ImportError::Schema(e) => e.fmt(f),
            ImportError::Doc(m) => write!(f, "drawing error: {m}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<ViewError> for ImportError {
    fn from(e: ViewError) -> Self {
        ImportError::View(e)
    }
}

impl From<SchemaError> for ImportError {
    fn from(e: SchemaError) -> Self {
        ImportError::Schema(e)
    }
}

impl From<cadcraft_doc::DocError> for ImportError {
    fn from(e: cadcraft_doc::DocError) -> Self {
        ImportError::Doc(e.to_string())
    }
}

/// The nearest `Lineweight::STANDARD` value to `mm`.
fn nearest_lineweight(mm: f64) -> Lineweight {
    let target = mm * 100.0;
    let best = Lineweight::STANDARD.iter().copied().min_by(|a, b| (f64::from(*a) - target).abs().total_cmp(&(f64::from(*b) - target).abs()));
    best.map_or(Lineweight::Default, Lineweight::Mm100)
}

fn import_layer(d: &mut Drawing, l: &DrawingLayer) -> Result<(), ImportError> {
    d.ensure_layer(&l.name);
    let layer = d.layer_mut(&l.name).ok_or_else(|| ImportError::Doc(format!("layer {} not created", l.name)))?;
    if let Some(rgb) = Rgb::parse_hex(&l.color) {
        layer.color = Color::True(rgb);
    }
    layer.lineweight = nearest_lineweight(l.lineweight);
    layer.on = l.visible;
    layer.locked = l.locked;
    set_layer_data(layer, &LayerData { default_role: l.default_role })?;
    Ok(())
}

fn geometry_kind(g: &EntityGeometry) -> &'static str {
    match g {
        EntityGeometry::Polyline { .. } => "polyline",
        EntityGeometry::Circle { .. } => "circle",
        EntityGeometry::Text { .. } => "text",
        EntityGeometry::Dimension { .. } => "dimension",
        EntityGeometry::Hatch(_) => "hatch",
    }
}

fn without_entities(v: &AnyView) -> AnyView {
    let mut v = v.clone();
    match &mut v {
        AnyView::Plan(p) => p.entities.clear(),
        AnyView::Vertical(p) => p.entities.clear(),
        AnyView::Detail(p) => p.entities.clear(),
    }
    v
}

fn remap_placement(p: &mut Option<Placement>, map: &IdMap) {
    if let Some(Placement::Vertical { host_view_id, .. }) = p
        && let Some(h) = map.get(*host_view_id)
    {
        *host_view_id = h;
    }
}

fn v3(p: Point2) -> Vec3 {
    Vec3::new(p.x, p.y, 0.0)
}

/// A polyline's kind, corner tags (only an LwPolyline carries them) and flattening warnings.
fn polyline_kind(geometry: &Polyline2) -> (EntityKind, Vec<CornerEntry>, Vec<Flattened>) {
    let (kind, mut warnings) = from_polyline(geometry);
    let corners = if matches!(kind, EntityKind::LwPolyline(_)) {
        warnings.retain(|w| *w != Flattened::VirtualCorner);
        virtual_corner_entries(geometry)
    } else {
        Vec::new()
    };
    (kind, corners, warnings)
}

/// A lamb-core dimension as a non-associative CADCraft DIMENSION in `style`. Linear and aligned put
/// `defpt` at the foot of `to` on the dimension line, by lamb-core's own formula
/// (`geometry/dimension.rs` `resolve_offset_dimension`, copied: it is private there).
fn dimension_kind(dim: &Dimension2, style: &str) -> EntityKind {
    let unit = |p: Point2| {
        let l = p.x.hypot(p.y);
        if l > 0.0 { Point2 { x: p.x / l, y: p.y / l } } else { Point2 { x: 0.0, y: 0.0 } }
    };
    let foot = |from: Point2, to: Point2, dir: Point2, offset: f64| {
        let n = Point2 { x: -dir.y, y: dir.x };
        let t = (to.x - from.x) * dir.x + (to.y - from.y) * dir.y;
        Point2 { x: from.x + dir.x * t + n.x * offset, y: from.y + dir.y * t + n.y * offset }
    };
    let (kind, defpt, p13, p14, p15, text) = match dim {
        Dimension2::Linear { from, to, axis, offset, override_text } => {
            let (dir, rotation) = match axis {
                Axis::X => (Point2 { x: 1.0, y: 0.0 }, 0.0),
                Axis::Y => (Point2 { x: 0.0, y: 1.0 }, std::f64::consts::FRAC_PI_2),
            };
            (DimKind::Linear { rotation }, foot(*from, *to, dir, *offset), *from, *to, Point2::default(), override_text)
        }
        Dimension2::Aligned { from, to, offset, override_text } => {
            let dir = unit(Point2 { x: to.x - from.x, y: to.y - from.y });
            (DimKind::Aligned, foot(*from, *to, dir, *offset), *from, *to, Point2::default(), override_text)
        }
        Dimension2::Radius { from, to, override_text } => (DimKind::Radius, *from, Point2::default(), Point2::default(), *to, override_text),
        Dimension2::Angular { from, to, vertex, radius, override_text } => {
            let tau = std::f64::consts::TAU;
            let a1 = (from.y - vertex.y).atan2(from.x - vertex.x);
            let a2 = (to.y - vertex.y).atan2(to.x - vertex.x);
            let sweep = ((a2 - a1) % tau + tau) % tau;
            let mid = a1 + sweep / 2.0;
            let at = Point2 { x: vertex.x + radius * mid.cos(), y: vertex.y + radius * mid.sin() };
            (DimKind::Angular3P, at, *from, *to, *vertex, override_text)
        }
    };
    EntityKind::Dimension(Dimension {
        kind,
        defpt: v3(defpt),
        text_mid: Vec3::ZERO,
        p13: v3(p13),
        p14: v3(p14),
        p15: v3(p15),
        p16: Vec3::ZERO,
        text: text.clone().unwrap_or_default(),
        style: style.to_owned(),
        measurement: 0.0,
        text_rotation: 0.0,
        user_text_pos: false,
        block: None,
        overrides: Default::default(),
        assoc: Vec::new(),
        marks: Default::default(),
    })
}

/// A hatch loop's vertices: the LwPolyline `from_polyline` makes (bulges kept), or — for a loop it
/// turns into another kind, e.g. a two-arc circle — the polyline's own segments as vertices.
fn loop_vertices(p: &Polyline2) -> Vec<PolyVertex> {
    if let (EntityKind::LwPolyline(lw), _) = from_polyline(p) {
        return lw.vertices;
    }
    let mut vertices = vec![PolyVertex::new(Vec2::new(p.start.x, p.start.y))];
    for s in &p.segments {
        if let Some(prev) = vertices.last_mut() {
            prev.bulge = s.bulge;
        }
        vertices.push(PolyVertex::new(Vec2::new(s.to.x, s.to.y)));
    }
    let closes = p.segments.last().is_some_and(|s| (s.to.x - p.start.x).hypot(s.to.y - p.start.y) <= TOLERANCE.coincidence_mm);
    if vertices.len() > 2 && closes {
        vertices.pop();
    }
    vertices
}

/// `h` as a CADCraft HATCH, its loops' sources mapped through `map`, and whether it is associative:
/// only when every loop names at least one entity and all of them were imported. Otherwise no loop
/// keeps sources (a half-associative hatch would refill wrongly).
fn hatch_kind(h: &HatchGeometry, map: &IdMap) -> (EntityKind, bool) {
    let src = hatch_loops(h);
    let mut loops: Vec<HatchLoop> = src
        .iter()
        .enumerate()
        .map(|(i, l)| HatchLoop {
            vertices: loop_vertices(&l.polyline),
            outer: i == 0,
            sources: l.entity_ids.iter().filter_map(|id| map.get(*id)).map(|id| Handle(id.0)).collect(),
        })
        .collect();
    let associative = !src.is_empty() && src.iter().zip(&loops).all(|(s, l)| !s.entity_ids.is_empty() && l.sources.len() == s.entity_ids.len());
    if !associative {
        loops.iter_mut().for_each(|l| l.sources.clear());
    }
    let pattern = h.pattern.to_uppercase();
    let hatch = Hatch {
        solid: pattern == "SOLID",
        pattern,
        loops,
        scale: h.scale,
        angle: h.angle.to_radians(),
        associative,
        style: 0,
        elevation: 0.0,
        gradient: None,
        origin: Vec2::default(),
        background: None,
        pattern_lines: Vec::new(),
    };
    (EntityKind::Hatch(hatch), associative)
}

/// The drawing layer for `e`: its project layer's name, else `0` with the layer id reported.
fn entity_layer(e: &DrawingEntity, layer_names: &HashMap<&str, &str>, report: &mut ImportReport) -> String {
    match layer_names.get(e.layer_id.as_str()) {
        Some(n) => (*n).to_owned(),
        None => {
            if !report.unknown_layers.contains(&e.layer_id) {
                report.unknown_layers.push(e.layer_id.clone());
            }
            "0".to_owned()
        }
    }
}

/// What `g` imports as — kind, corner tags, flattening warnings — or `None` if it is skipped (a
/// hatch is added in a later pass, by [`hatch_kind`]).
fn entity_kind(g: &EntityGeometry, dim_style: &str) -> Option<(EntityKind, Vec<CornerEntry>, Vec<Flattened>)> {
    match g {
        EntityGeometry::Polyline { geometry } => Some(polyline_kind(geometry)),
        EntityGeometry::Circle { center, radius } => Some(polyline_kind(&circle_as_polyline(&CircleFit { center: *center, radius: *radius }))),
        EntityGeometry::Text { position, content, height, rotation } => Some((
            EntityKind::Text(Text {
                insert: v3(*position),
                align_pt: None,
                height: *height,
                value: content.clone(),
                rotation: *rotation,
                width_factor: 1.0,
                oblique: 0.0,
                style: "Standard".into(),
                halign: HAlign::Left,
                valign: VAlign::Baseline,
            }),
            Vec::new(),
            Vec::new(),
        )),
        EntityGeometry::Dimension { dimension } => Some((dimension_kind(dimension, dim_style), Vec::new(), Vec::new())),
        EntityGeometry::Hatch(_) => None,
    }
}

/// One slot-key token — an old id's decimal — as its new handle's decimal, if `map` has it.
fn remap_token(s: &str, map: &IdMap) -> Option<String> {
    s.parse::<u64>().ok().and_then(|n| map.get(Id(n))).map(|id| id.0.to_string())
}

/// Sizes annotation as LAMB does: dimension text is `dim_text_height_paper_mm` on paper, scaled to
/// the model by `plot_scale` (DIMSCALE, on the header and the current dimension style), and TEXT's
/// default height is that model height. Leaves no dimension-variable overrides.
pub(crate) fn apply_annotation_scale(d: &mut Drawing, defaults: &ProjectDefaults) {
    let name = d.header.str("DIMSTYLE", "Standard");
    if let Some(st) = d.dim_styles.iter_mut().find(|st| st.name.eq_ignore_ascii_case(&name)) {
        st.text_height = defaults.dim_text_height_paper_mm;
        // The style's DIMSCALE wins over the header's, and syncing copies it there.
        st.scale = defaults.plot_scale;
    }
    d.header.set_f64("DIMSCALE", defaults.plot_scale);
    d.sync_dim_vars();
    d.header.set_f64("TEXTSIZE", defaults.plot_scale * defaults.dim_text_height_paper_mm);
}

/// Imports `project` into a new metric drawing. See the module docs for what is mapped how.
pub fn project_to_drawing(project: &Project) -> Result<(Drawing, IdMap, ImportReport), ImportError> {
    let mut d = Drawing::new_metric();
    let mut map = IdMap::default();
    let mut report = ImportReport::default();

    for l in &project.layers {
        import_layer(&mut d, l)?;
    }
    let layer_names: HashMap<&str, &str> = project.layers.iter().map(|l| (l.id.as_str(), l.name.as_str())).collect();

    let mut doc = read_document(&d)?;
    doc.name = project.name.clone();
    doc.defaults = project.defaults.clone();
    doc.levels = project.levels.clone();
    doc.suppressed_proposals = project.suppressed_proposals.clone().unwrap_or_default();
    write_document(&mut d, &doc)?;
    apply_annotation_scale(&mut d, &doc.defaults);

    for v in &project.views {
        let id = create_view(&mut d, without_entities(v))?;
        map.0.insert(v.id(), id);
    }

    // Entities with an AI origin, set once every entity has its handle: the origin's source entity
    // and the ids inside its slot key are rewritten onto handles (T355).
    let mut pending_origin = Vec::new();
    let dim_style = d.header.str("DIMSTYLE", "Standard");
    let mut add = |d: &mut Drawing,
                   map: &mut IdMap,
                   report: &mut ImportReport,
                   space: &Space,
                   e: &DrawingEntity,
                   kind: EntityKind,
                   corners: Vec<CornerEntry>|
     -> Result<(), ImportError> {
        let mut common = Common { layer: entity_layer(e, &layer_names, report), ..Common::default() };
        let data = EntityData {
            role: e.role,
            ai_origin: e.ai_origin.clone(),
            view_id: None,
            corners,
            detail_tag: None,
            detail_ref: None,
            repeat: None,
            outside: false,
            stud_grid: None,
            stud_grid_wall: None,
            stud_move: false,
            hole: None,
        };
        let deferred = data.ai_origin.is_some();
        if !deferred {
            set_entity_data(&mut common, &data)?;
        }
        let h = d.add(space, common, kind)?;
        map.0.insert(e.id, Id(h.0));
        if deferred {
            pending_origin.push((h, data));
        }
        Ok(())
    };
    // Hatches go in after every other entity has its handle: a boundary may come later in its view.
    let mut hatches = Vec::new();
    for v in &project.views {
        let Some(view_id) = map.get(v.id()) else { continue };
        let space = Space::Block(view_block_name(view_id));
        for e in v.entities() {
            if let EntityGeometry::Hatch(h) = &e.geometry {
                hatches.push((space.clone(), e, h));
                continue;
            }
            let Some((kind, corners, warnings)) = entity_kind(&e.geometry, &dim_style) else {
                report.skipped.push((e.id, geometry_kind(&e.geometry)));
                continue;
            };
            report.flattened.extend(warnings.into_iter().map(|w| (e.id, w)));
            add(&mut d, &mut map, &mut report, &space, e, kind, corners)?;
        }
    }
    for (space, e, h) in hatches {
        let (kind, associative) = hatch_kind(h, &map);
        if !associative {
            report.unassociated_hatches.push(e.id);
        }
        if let EntityKind::Hatch(x) = &kind
            && !x.solid
            && cadcraft_doc::library::pattern(&x.pattern).is_none()
            && !report.unknown_patterns.contains(&x.pattern)
        {
            report.unknown_patterns.push(x.pattern.clone());
        }
        add(&mut d, &mut map, &mut report, &space, e, kind, Vec::new())?;
    }
    drop(add);
    for (h, mut data) in pending_origin {
        if let Some(o) = data.ai_origin.as_mut() {
            if let Some(src) = o.source_entity_id
                && let Some(new) = map.get(src)
            {
                o.source_entity_id = Some(new);
            }
            o.proposal_id = remap_slot_key(&o.proposal_id, |s| remap_token(s, &map));
        }
        let mut err = None;
        d.modify_entity(h, |e| err = set_entity_data(&mut e.common, &data).err())?;
        if let Some(e) = err {
            return Err(e.into());
        }
    }

    let mut doc = read_document(&d)?;
    doc.suppressed_proposals = doc.suppressed_proposals.iter().map(|k| remap_slot_key(k, |s| remap_token(s, &map))).collect();
    for v in &mut doc.views {
        match v {
            AnyView::Plan(p) => remap_placement(&mut p.placement, &map),
            AnyView::Vertical(p) => remap_placement(&mut p.placement, &map),
            AnyView::Detail(p) => {
                remap_placement(&mut p.placement, &map);
                p.host_element_ids = p.host_element_ids.iter().filter_map(|h| map.get(*h)).collect();
            }
        }
    }
    for link in &project.links {
        match (map.get(link.a.entity_id), map.get(link.b.entity_id)) {
            (Some(a), Some(b)) => {
                let mut link = link.clone();
                link.a.entity_id = a;
                link.b.entity_id = b;
                doc.links.push(link);
            }
            _ => report.dropped_links.push(link.id),
        }
    }
    write_document(&mut d, &doc)?;
    Ok((d, map, report))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::*;

    type R = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn a_lamb_drawing_sizes_dimensions_and_text_by_plot_scale() -> R {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb/dimensioned-u-wall.lamb.json");
        let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        let project = lamb_core::persistence::files_to_project(&files).map_err(|e| format!("{e:?}"))?;
        assert_eq!((project.defaults.plot_scale, project.defaults.dim_text_height_paper_mm), (50.0, 3.0));
        let (d, _, _) = project_to_drawing(&project)?;
        assert_eq!(d.header.f64("DIMSCALE", 0.0), 50.0);
        assert_eq!(d.header.f64("TEXTSIZE", 0.0), 150.0);
        assert!(d.dim_overrides().is_empty(), "no overrides: {:?}", d.dim_overrides());
        let mut kinds = Vec::new();
        for b in d.blocks.values() {
            for e in b.entities.iter() {
                if let EntityKind::Dimension(dm) = &e.kind {
                    let h = cadcraft_render::dimension_in(&d, dm).text_height;
                    assert!((h - 150.0).abs() < 1e-9, "{:?} text height {h}", dm.kind);
                    kinds.push(format!("{:?}", dm.kind));
                }
            }
        }
        assert!(!kinds.is_empty(), "the fixture has dimensions");
        Ok(())
    }
}

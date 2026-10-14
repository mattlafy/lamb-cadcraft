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
//! Only polyline geometry is imported; text, hatch, dimension and anything else is skipped and
//! listed in the [`ImportReport`], and a link to a skipped entity is dropped and listed.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use cadcraft_doc::color::{Color, Rgb};
use cadcraft_doc::{Common, Drawing, EntityKind, Lineweight, Space};
use lamb_core::derivation::proposals::remap_slot_key;
use lamb_core::geometry::polyline::virtual_corner_entries;
use lamb_core::model::{AnyView, DrawingLayer, EntityGeometry, Id, Placement, Project};

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

/// One slot-key token — an old id's decimal — as its new handle's decimal, if `map` has it.
fn remap_token(s: &str, map: &IdMap) -> Option<String> {
    s.parse::<u64>().ok().and_then(|n| map.get(Id(n))).map(|id| id.0.to_string())
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

    for v in &project.views {
        let id = create_view(&mut d, without_entities(v))?;
        map.0.insert(v.id(), id);
    }

    // Entities with an AI origin, set once every entity has its handle: the origin's source entity
    // and the ids inside its slot key are rewritten onto handles (T355).
    let mut pending_origin = Vec::new();
    for v in &project.views {
        let Some(view_id) = map.get(v.id()) else { continue };
        let space = Space::Block(view_block_name(view_id));
        for e in v.entities() {
            let EntityGeometry::Polyline { geometry } = &e.geometry else {
                report.skipped.push((e.id, geometry_kind(&e.geometry)));
                continue;
            };
            let (kind, mut warnings) = from_polyline(geometry);
            let corners = if matches!(kind, EntityKind::LwPolyline(_)) {
                warnings.retain(|w| *w != Flattened::VirtualCorner);
                virtual_corner_entries(geometry)
            } else {
                Vec::new()
            };
            report.flattened.extend(warnings.into_iter().map(|w| (e.id, w)));
            let layer = match layer_names.get(e.layer_id.as_str()) {
                Some(n) => (*n).to_owned(),
                None => {
                    if !report.unknown_layers.contains(&e.layer_id) {
                        report.unknown_layers.push(e.layer_id.clone());
                    }
                    "0".to_owned()
                }
            };
            let mut common = Common { layer, ..Common::default() };
            let data = EntityData {
                role: e.role,
                ai_origin: e.ai_origin.clone(),
                view_id: None,
                corners,
                detail_tag: None,
                detail_ref: None,
                repeat: None,
                outside: false,
            };
            let deferred = data.ai_origin.is_some();
            if !deferred {
                set_entity_data(&mut common, &data)?;
            }
            let h = d.add(&space, common, kind)?;
            map.0.insert(e.id, Id(h.0));
            if deferred {
                pending_origin.push((h, data));
            }
        }
    }
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

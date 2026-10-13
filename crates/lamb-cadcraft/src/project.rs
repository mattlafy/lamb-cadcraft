//! lamb-core's `ProjectRead` over a CADCraft `Drawing` (`cadcraft-integration.md` §3.1 item 1:
//! "read through traits"), so derivation runs on a drawing with no `Project` rebuilt from it.
//!
//! [`DrawingProject::new`] parses the LAMB slots once ([`crate::schema`]): the document, every
//! layer's default role and every view-block entity's role / AI origin. Polylines are converted
//! on the fly in [`ProjectRead::entities`]; an entity with no `Polyline2` form yields
//! `polyline: None`, which derivation already treats as non-participating; a converted one gets its
//! `EntityData.corners` virtual-corner tags back ([`apply_corners`]). Flatten warnings are dropped
//! here.
//!
//! Layer `color` / `lineweight` are never read by derivation; they are filled as: colour = the
//! layer colour resolved to RGB by CADCraft's own `Color::resolve` (ACI 7 for ByLayer/ByBlock),
//! as `#rrggbb`; lineweight = `Mm100(v)` as `v / 100` mm, anything logical as 0.25 mm (DXF's
//! `LWDEFAULT` default).

use std::borrow::Cow;
use std::collections::HashMap;

use cadcraft_doc::color::Color;
use cadcraft_doc::{Drawing, Handle, Lineweight};
use lamb_core::derivation::read::{EntityRef, ProjectRead, ViewRef, view_ref};
use lamb_core::model::{DrawingLayer, Id, Level, LineRole, LinkRecord, ProjectDefaults};

use crate::convert::{apply_corners, to_polyline};
use crate::schema::{EntityData, LambDocument, SchemaError, read_document, read_entity_data, read_layer_data, view_block_name};

/// Logical lineweights (ByLayer, ByBlock, Default) read as this, in mm.
const DEFAULT_LINEWEIGHT_MM: f64 = 0.25;

/// A CADCraft drawing seen as a LAMB document.
pub struct DrawingProject<'a> {
    drawing: &'a Drawing,
    doc: LambDocument,
    layers: Vec<DrawingLayer>,
    entity_data: HashMap<Handle, EntityData>,
}

impl<'a> DrawingProject<'a> {
    /// Parses every LAMB slot once; any malformed one, or a future `schemaVersion`, is an error.
    pub fn new(drawing: &'a Drawing) -> Result<Self, SchemaError> {
        let doc = read_document(drawing)?;
        let mut layers = Vec::with_capacity(drawing.layers.len());
        for l in &drawing.layers {
            let data = read_layer_data(l)?;
            layers.push(DrawingLayer {
                id: l.name.clone(),
                name: l.name.clone(),
                color: l.color.resolve(Color::WHITE, Color::WHITE).hex(),
                lineweight: match l.lineweight {
                    Lineweight::Mm100(v) => f64::from(v) / 100.0,
                    _ => DEFAULT_LINEWEIGHT_MM,
                },
                visible: l.visible(),
                locked: l.locked,
                default_role: data.default_role,
            });
        }
        let mut entity_data = HashMap::new();
        for v in &doc.views {
            let Some(block) = drawing.block(&view_block_name(v.id())) else { continue };
            for e in block.entities.iter() {
                let data = read_entity_data(&e.common)?;
                if data != EntityData::default() {
                    entity_data.insert(e.handle, data);
                }
            }
        }
        Ok(DrawingProject { drawing, doc, layers, entity_data })
    }

    /// The parsed `Drawing.ext["LAMB"]`.
    pub fn document(&self) -> &LambDocument {
        &self.doc
    }

    pub fn drawing(&self) -> &'a Drawing {
        self.drawing
    }
}

impl ProjectRead for DrawingProject<'_> {
    fn views(&self) -> impl Iterator<Item = ViewRef<'_>> {
        self.doc.views.iter().map(view_ref)
    }

    fn entities(&self, view: Id) -> impl Iterator<Item = EntityRef<'_>> {
        let known = self.doc.views.iter().any(|v| v.id() == view);
        let block = if known { self.drawing.block(&view_block_name(view)) } else { None };
        block.into_iter().flat_map(|b| b.entities.iter()).map(move |e| {
            let data = self.entity_data.get(&e.handle);
            EntityRef {
                id: Id(e.handle.0),
                view_id: view,
                role: data.map_or(LineRole::Bylayer, |d| d.role),
                layer_id: e.common.layer.as_str(),
                polyline: to_polyline(e).ok().map(|c| {
                    let mut p = c.polyline;
                    if let Some(d) = data {
                        apply_corners(&mut p, &d.corners);
                    }
                    Cow::Owned(p)
                }),
                ai_origin: data.and_then(|d| d.ai_origin.as_ref()),
            }
        })
    }

    fn links(&self) -> impl Iterator<Item = &LinkRecord> {
        self.doc.links.iter()
    }

    fn layers(&self) -> &[DrawingLayer] {
        &self.layers
    }

    fn level(&self, id: Id) -> Option<&Level> {
        self.doc.levels.iter().find(|l| l.id == id)
    }

    fn defaults(&self) -> &ProjectDefaults {
        &self.doc.defaults
    }

    fn suppressed_proposals(&self) -> &[String] {
        &self.doc.suppressed_proposals
    }
}

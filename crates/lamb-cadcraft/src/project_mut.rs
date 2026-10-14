//! lamb-core's `ProjectWrite` over a CADCraft `Drawing`, for the mutating sweeps
//! (`reconcile_ai_lines`, `reconcile_solid_cut_lines`) that read and write interleaved.
//!
//! [`DrawingProjectMut`] parses the LAMB slots once, exactly as [`crate::project::DrawingProject`]
//! does (the same free functions), then keeps them current as it writes: links and suppressed
//! proposals edit the in-memory [`LambDocument`], written back once by [`DrawingProjectMut::finish`];
//! entities go straight into the view blocks, their role / AI origin / corner tags into
//! `Common.ext["LAMB"]` and the parsed entity-data map together, so later reads see them.
//!
//! An entity's id is its handle: [`ProjectWrite::push_entity`] stores the entity under
//! `Handle(entity.id.0)`, so ids must be minted from the drawing's handle seed (see
//! [`crate::sweeps`]). Only polyline geometry can be written. `insert_entity` appends (only the
//! `invert_*` functions call it, and the fork's undo is the drawing snapshot).

use std::collections::{HashMap, HashSet};

use cadcraft_doc::{Common, Drawing, Entity, EntityKind, Handle, Space};
use lamb_core::derivation::read::{EntityRef, ProjectRead, ViewRef, view_ref};
use lamb_core::derivation::write::{ProjectWrite, RemovedEntity};
use lamb_core::geometry::polyline::virtual_corner_entries;
use lamb_core::geometry::{Point2, Polyline2};
use lamb_core::model::{AiOrigin, DrawingEntity, DrawingLayer, EntityGeometry, Id, Level, LineRole, LinkRecord, ProjectDefaults};

use crate::convert::{apply_corners, from_polyline, to_polyline};
use crate::project::{Parsed, entities, parse};
use crate::schema::{EntityData, LambDocument, SchemaError, set_entity_data, view_block_name, write_document};

/// A CADCraft drawing seen as a writable LAMB document.
pub struct DrawingProjectMut<'a> {
    drawing: &'a mut Drawing,
    doc: LambDocument,
    layers: Vec<DrawingLayer>,
    entity_data: HashMap<Handle, EntityData>,
    /// The first entity write that failed (a slot that would not serialise, a missing block);
    /// reported by [`DrawingProjectMut::finish`], since the trait's setters return nothing.
    error: Option<String>,
}

impl<'a> DrawingProjectMut<'a> {
    /// Parses every LAMB slot once; any malformed one, or a future `schemaVersion`, is an error.
    pub fn new(drawing: &'a mut Drawing) -> Result<Self, SchemaError> {
        let Parsed { doc, layers, entity_data } = parse(drawing)?;
        Ok(DrawingProjectMut { drawing, doc, layers, entity_data, error: None })
    }

    /// The in-memory LAMB document (with every link / suppression written so far).
    pub fn document(&self) -> &LambDocument {
        &self.doc
    }

    pub fn document_mut(&mut self) -> &mut LambDocument {
        &mut self.doc
    }

    /// Writes the document back to the drawing; reports the first failed entity write, if any.
    pub fn finish(self) -> Result<(), SchemaError> {
        if let Some(message) = self.error {
            return Err(SchemaError::Malformed { what: "a LAMB entity write".into(), message });
        }
        write_document(self.drawing, &self.doc)
    }

    fn fail(&mut self, message: String) {
        if self.error.is_none() {
            self.error = Some(message);
        }
    }

    /// The view whose block holds `h`.
    fn view_of(&self, h: Handle) -> Option<Id> {
        self.doc.views.iter().map(|v| v.id()).find(|v| self.drawing.block(&view_block_name(*v)).is_some_and(|b| b.entities.contains(h)))
    }

    /// Stores `data` on entity `h` and in the parsed map.
    fn write_data(&mut self, h: Handle, data: EntityData) {
        let mut err = None;
        if let Err(e) = self.drawing.modify_entity(h, |e| err = set_entity_data(&mut e.common, &data).err()) {
            self.fail(e.to_string());
            return;
        }
        if let Some(e) = err {
            self.fail(e.to_string());
            return;
        }
        if data == EntityData::default() {
            self.entity_data.remove(&h);
        } else {
            self.entity_data.insert(h, data);
        }
    }

    /// A block entity read back as a lamb-core `DrawingEntity` (an unconvertible one as an empty
    /// polyline at the origin; only AI lines the sweeps wrote are ever removed).
    fn drawing_entity(&self, view: Id, e: &Entity) -> DrawingEntity {
        let data = self.entity_data.get(&e.handle);
        let geometry = to_polyline(e).map_or_else(
            |_| Polyline2 { start: Point2 { x: 0.0, y: 0.0 }, segments: Vec::new(), closed: false },
            |c| {
                let mut p = c.polyline;
                if let Some(d) = data {
                    apply_corners(&mut p, &d.corners);
                }
                p
            },
        );
        DrawingEntity {
            id: Id(e.handle.0),
            view_id: view,
            role: data.map_or(LineRole::Bylayer, |d| d.role),
            layer_id: e.common.layer.clone(),
            geometry: EntityGeometry::Polyline { geometry },
            color_override: None,
            lineweight_override: None,
            ai_origin: data.and_then(|d| d.ai_origin.clone()),
        }
    }
}

/// The corner tags that go with `kind` written from `p` (only an `LwPolyline` has vertices).
fn corners_for(kind: &EntityKind, p: &Polyline2) -> Vec<lamb_core::geometry::polyline::CornerEntry> {
    if matches!(kind, EntityKind::LwPolyline(_)) { virtual_corner_entries(p) } else { Vec::new() }
}

impl ProjectRead for DrawingProjectMut<'_> {
    fn views(&self) -> impl Iterator<Item = ViewRef<'_>> {
        self.doc.views.iter().map(view_ref)
    }

    fn entities(&self, view: Id) -> impl Iterator<Item = EntityRef<'_>> {
        entities(self.drawing, &self.doc, &self.entity_data, view)
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

impl ProjectWrite for DrawingProjectMut<'_> {
    fn push_link(&mut self, link: LinkRecord) {
        self.doc.links.push(link);
    }

    fn take_links(&mut self, pred: &mut dyn FnMut(&LinkRecord) -> bool) -> Vec<LinkRecord> {
        let (taken, kept) = std::mem::take(&mut self.doc.links).into_iter().partition(|l| pred(l));
        self.doc.links = kept;
        taken
    }

    fn push_entity(&mut self, entity: DrawingEntity) -> Result<(), String> {
        let view = entity.view_id;
        if !self.doc.views.iter().any(|v| v.id() == view) {
            return Err(format!("push_entity: no view {view}"));
        }
        let EntityGeometry::Polyline { geometry } = &entity.geometry else {
            return Err(format!("push_entity: entity {} is not a polyline", entity.id));
        };
        let (kind, _) = from_polyline(geometry);
        let data =
            EntityData { role: entity.role, ai_origin: entity.ai_origin.clone(), corners: corners_for(&kind, geometry), ..EntityData::default() };
        let mut common = Common { layer: entity.layer_id.clone(), ..Common::default() };
        set_entity_data(&mut common, &data).map_err(|e| e.to_string())?;
        let h = Handle(entity.id.0);
        self.drawing.ensure_layer(&entity.layer_id);
        if let Some(l) = self.drawing.layer(&entity.layer_id)
            && !self.layers.iter().any(|x| x.id == l.name)
        {
            let layer = crate::project::layer_of(l).map_err(|e| e.to_string())?;
            self.layers.push(layer);
        }
        let store = self.drawing.space_mut(&Space::Block(view_block_name(view))).ok_or_else(|| format!("push_entity: view {view} has no block"))?;
        store.push(Entity { handle: h, common, kind });
        if data == EntityData::default() {
            self.entity_data.remove(&h);
        } else {
            self.entity_data.insert(h, data);
        }
        Ok(())
    }

    fn insert_entity(&mut self, _view: Id, _index: usize, entity: DrawingEntity) {
        if let Err(e) = self.push_entity(entity) {
            self.fail(e);
        }
    }

    fn remove_entities(&mut self, ids: &HashSet<Id>) -> Vec<RemovedEntity> {
        let mut removed = Vec::new();
        let views: Vec<Id> = self.doc.views.iter().map(|v| v.id()).collect();
        for view_id in views {
            let space = Space::Block(view_block_name(view_id));
            let Some(block) = self.drawing.block(&view_block_name(view_id)) else { continue };
            let hits: Vec<(usize, DrawingEntity)> = block
                .entities
                .iter()
                .enumerate()
                .filter(|(_, e)| ids.contains(&Id(e.handle.0)))
                .map(|(i, e)| (i, self.drawing_entity(view_id, e)))
                .collect();
            for (index, entity) in hits.into_iter().rev() {
                let h = Handle(entity.id.0);
                if let Some(store) = self.drawing.space_mut(&space) {
                    store.remove(h);
                }
                self.entity_data.remove(&h);
                removed.push(RemovedEntity { view_id, index, entity });
            }
        }
        removed
    }

    fn set_polyline(&mut self, id: Id, geometry: Polyline2) {
        let h = Handle(id.0);
        if self.view_of(h).is_none() {
            return;
        }
        let (kind, _) = from_polyline(&geometry);
        let corners = corners_for(&kind, &geometry);
        if let Err(e) = self.drawing.modify_entity(h, |e| e.kind = kind) {
            self.fail(e.to_string());
            return;
        }
        let mut data = self.entity_data.get(&h).cloned().unwrap_or_default();
        if data.corners != corners {
            data.corners = corners;
            self.write_data(h, data);
        }
    }

    fn set_ai_origin(&mut self, id: Id, origin: Option<AiOrigin>) {
        let h = Handle(id.0);
        if self.view_of(h).is_none() {
            return;
        }
        let mut data = self.entity_data.get(&h).cloned().unwrap_or_default();
        data.ai_origin = origin;
        self.write_data(h, data);
    }

    fn set_suppressed_proposals(&mut self, keys: Vec<String>) {
        self.doc.suppressed_proposals = keys;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::DrawingProject;
    use crate::views::create_view;
    use lamb_core::geometry::polyline::line2;
    use lamb_core::model::{AnyView, PlanView};

    fn point2(x: f64, y: f64) -> Point2 {
        Point2 { x, y }
    }

    fn line(id: u64, view: Id, x: f64) -> DrawingEntity {
        DrawingEntity {
            id: Id(id),
            view_id: view,
            role: LineRole::Cut,
            layer_id: "AI".into(),
            geometry: EntityGeometry::Polyline { geometry: line2(point2(x, 0.0), point2(x, 10.0)) },
            color_override: None,
            lineweight_override: None,
            ai_origin: Some(AiOrigin {
                proposal_id: format!("p{id}"),
                accepted: false,
                assumption_note: None,
                source_entity_id: None,
                swept_geometry: None,
                producer: None,
            }),
        }
    }

    fn ids(d: &Drawing, view: Id) -> Result<Vec<u64>, SchemaError> {
        let p = DrawingProject::new(d)?;
        Ok(p.entities(view).map(|e| e.id.0).collect())
    }

    #[test]
    fn writes_land_in_the_drawing_and_read_back() -> Result<(), Box<dyn std::error::Error>> {
        let mut d = Drawing::new_metric();
        let view = create_view(&mut d, AnyView::Plan(PlanView { id: Id(0), name: "P".into(), entities: Vec::new(), placement: None }))?;
        let base = d.handseed;
        {
            let mut m = DrawingProjectMut::new(&mut d)?;
            m.push_entity(line(base, view, 0.0))?;
            m.push_entity(line(base + 1, view, 5.0))?;
            m.push_entity(line(base + 2, view, 9.0))?;
            assert!(m.push_entity(line(base + 3, Id(999_999), 0.0)).is_err());
            let read = m.entity(Id(base + 1)).and_then(|e| e.ai_origin.map(|o| o.proposal_id.clone()));
            assert_eq!(read, Some(format!("p{}", base + 1)), "a pushed entity reads back before finish");
            m.set_polyline(Id(base + 1), line2(point2(6.0, 0.0), point2(6.0, 10.0)));
            m.set_ai_origin(Id(base), None);
            m.set_suppressed_proposals(vec!["k".into()]);
            let removed = m.remove_entities(&[Id(base), Id(base + 2)].into_iter().collect());
            let got: Vec<(usize, u64)> = removed.iter().map(|r| (r.index, r.entity.id.0)).collect();
            assert_eq!(got, vec![(2, base + 2), (0, base)], "a view scanned from the back, with block indices");
            m.finish()?;
        }
        d.bump_handseed(Handle(base + 2));
        assert_eq!(ids(&d, view)?, vec![base + 1]);
        let p = DrawingProject::new(&d)?;
        let e = p.entity(Id(base + 1)).ok_or("kept entity")?;
        assert_eq!(e.role, LineRole::Cut);
        assert_eq!(e.layer_id, "AI");
        assert_eq!(e.polyline.as_deref(), Some(&line2(point2(6.0, 0.0), point2(6.0, 10.0))));
        assert!(e.ai_origin.is_some_and(|o| !o.accepted));
        assert_eq!(p.suppressed_proposals(), ["k".to_owned()]);
        assert!(p.layers().iter().any(|l| l.id == "AI"));
        Ok(())
    }
}

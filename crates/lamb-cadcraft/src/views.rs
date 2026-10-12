//! Views as blocks (`cadcraft-integration.md` §4, insertion regime): a LAMB view is a block named
//! [`view_block_name`]`(id)` holding its entities, a layer of the same name, and one canonical
//! INSERT of that block in model space. The canonical insert is the one whose handle equals the
//! view id — identity marks it, no extra field.

use std::fmt;
use std::sync::Arc;

use cadcraft_doc::{Block, Common, Drawing, Entity, EntityKind, Handle, Insert};
use cadcraft_geom::Vec3;
use lamb_core::model::{AnyView, Id};

use crate::schema::{SchemaError, read_document, view_block_name, write_document};

/// Why a view could not be created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewError {
    /// The drawing's `ext["LAMB"]` document could not be read or written.
    Schema(SchemaError),
    /// A block of the view's name already exists (a handle reused across drawings).
    BlockExists(String),
}

impl fmt::Display for ViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ViewError::Schema(e) => e.fmt(f),
            ViewError::BlockExists(n) => write!(f, "block {n} already exists"),
        }
    }
}

impl std::error::Error for ViewError {}

impl From<SchemaError> for ViewError {
    fn from(e: SchemaError) -> Self {
        ViewError::Schema(e)
    }
}

fn set_id(view: &mut AnyView, id: Id) {
    match view {
        AnyView::Plan(v) => v.id = id,
        AnyView::Vertical(v) => v.id = id,
        AnyView::Detail(v) => v.id = id,
    }
}

/// Creates `view` in `d`: allocates a handle `h`, gives the view id `Id(h.0)` (its own id and
/// entities are ignored), adds the layer and empty block `LAMB_VIEW_<id>`, a model-space INSERT of
/// that block at the origin (scale 1, rotation 0) on that layer **with handle `h`**, and appends
/// the view to the LAMB document. Returns the id. Nothing is changed on error.
pub fn create_view(d: &mut Drawing, mut view: AnyView) -> Result<Id, ViewError> {
    let mut doc = read_document(d)?;
    let h: Handle = d.new_handle();
    let id = Id(h.0);
    let name = view_block_name(id);
    if d.block(&name).is_some() {
        return Err(ViewError::BlockExists(name));
    }
    set_id(&mut view, id);
    doc.views.push(view);
    write_document(d, &doc)?;
    d.ensure_layer(&name);
    d.blocks.insert(name.clone(), Arc::new(Block::new(&name)));
    let insert = Insert {
        block: name.clone(),
        insert: Vec3::ZERO,
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: Vec::new(),
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    };
    d.model.push(Entity { handle: h, common: Common { layer: name, ..Common::default() }, kind: EntityKind::Insert(insert) });
    Ok(id)
}

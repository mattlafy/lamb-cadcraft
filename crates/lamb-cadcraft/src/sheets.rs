//! Sheets (`cadcraft-integration.md` §13 phase 2): a paper layout with one viewport per LAMB
//! view. Each viewport freezes every **other** view's layer, so CADCraft hides those views'
//! whole canonical inserts there, while model space keeps showing all views overlaid. The
//! viewport records which view it shows in its `EntityData.view_id`.
//!
//! Arrangement: a grid over the layout's printable area (columns = ceil(sqrt(n)), filled row by
//! row from the top left in the given order), each viewport its cell shrunk by 5 % on every side
//! and zoomed to its own view block's extents. No shared plot scale yet.

use std::fmt;

use cadcraft_doc::{Common, Drawing, EntityKind, Handle, Layout, PageSetup, Space, Viewport};
use cadcraft_geom::{Bounds2, Vec2, Vec3};
use lamb_core::model::Id;

use crate::schema::{EntityData, SchemaError, read_document, set_entity_data, view_block_name};

/// Why a sheet could not be created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SheetError {
    /// The drawing's `ext["LAMB"]` document could not be read, or a viewport's slot written.
    Schema(SchemaError),
    /// A layout of that name already exists.
    LayoutExists(String),
    /// A requested view is not in the LAMB document.
    NoSuchView(Id),
    /// There are no views to lay out.
    NoViews,
}

impl fmt::Display for SheetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SheetError::Schema(e) => e.fmt(f),
            SheetError::LayoutExists(n) => write!(f, "Layout {n} already exists"),
            SheetError::NoSuchView(id) => write!(f, "No such view {}", id.0),
            SheetError::NoViews => f.write_str("No views to place on a sheet"),
        }
    }
}

impl std::error::Error for SheetError {}

impl From<SchemaError> for SheetError {
    fn from(e: SchemaError) -> Self {
        SheetError::Schema(e)
    }
}

/// Model height shown by a viewport onto an empty view.
const EMPTY_VIEW_HEIGHT: f64 = 10_000.0;

fn zoom(d: &Drawing, view: Id, vp_width: f64, vp_height: f64) -> (Vec2, f64) {
    let ext = d.extents(&Space::Block(view_block_name(view)));
    if ext.is_empty() {
        return (Vec2::new(0.0, 0.0), EMPTY_VIEW_HEIGHT);
    }
    let h = ext.height().max(ext.width() * vp_height / vp_width) * 1.1;
    (ext.center(), if h.is_finite() && h > 0.0 { h } else { EMPTY_VIEW_HEIGHT })
}

/// Creates layout `name` with one viewport per view in `views` (every view in document order
/// when empty), each freezing every other LAMB view's layer. Returns the viewports' handles in
/// the given order. Nothing is changed on error.
pub fn create_sheet(d: &mut Drawing, name: &str, views: &[Id]) -> Result<Vec<Handle>, SheetError> {
    let doc = read_document(d)?;
    if d.layout(name).is_some() {
        return Err(SheetError::LayoutExists(name.to_owned()));
    }
    let all: Vec<Id> = doc.views.iter().map(|v| v.id()).collect();
    let chosen: Vec<Id> = if views.is_empty() { all.clone() } else { views.to_vec() };
    if chosen.is_empty() {
        return Err(SheetError::NoViews);
    }
    if let Some(bad) = chosen.iter().find(|v| !all.contains(v)) {
        return Err(SheetError::NoSuchView(*bad));
    }
    // Every viewport's EntityData, built before anything changes.
    let data: Vec<EntityData> = chosen.iter().map(|v| EntityData { view_id: Some(*v), ..EntityData::default() }).collect();
    let mut commons = Vec::with_capacity(chosen.len());
    for dt in &data {
        let mut c = Common::default();
        set_entity_data(&mut c, dt)?;
        commons.push(c);
    }

    let next_id = d
        .layouts
        .iter()
        .flat_map(|l| l.entities.iter())
        .filter_map(|e| match &e.kind {
            EntityKind::Viewport(vp) => Some(vp.id),
            _ => None,
        })
        .fold(1u32, u32::max)
        + 1;
    let tab_order = d.layouts.iter().map(|l| l.tab_order).max().map_or(0, |t| t + 1);
    d.layouts.push(Layout { page: PageSetup::default(), ..Layout::new(name, tab_order) });
    let area = match cadcraft_render::sheet(d, name) {
        Some(s) if !s.printable.is_empty() => s.printable,
        _ => {
            let p = PageSetup::default();
            Bounds2::new(Vec2::new(0.0, 0.0), Vec2::new(p.width_mm, p.height_mm))
        }
    };

    let n = chosen.len();
    let cols = (n as f64).sqrt().ceil().max(1.0) as usize;
    let rows = n.div_ceil(cols);
    let cell = Vec2::new(area.width() / cols as f64, area.height() / rows as f64);
    let (w, h) = (cell.x * 0.9, cell.y * 0.9);
    let space = Space::Paper(name.to_owned());
    let mut handles = Vec::with_capacity(n);
    for (i, (view, common)) in chosen.iter().zip(commons).enumerate() {
        let (r, c) = (i / cols, i % cols);
        let center = Vec3::new(area.min.x + cell.x * (c as f64 + 0.5), area.max.y - cell.y * (r as f64 + 0.5), 0.0);
        let (view_center, view_height) = zoom(d, *view, w, h);
        let mut frozen_layers: Vec<String> = all.iter().filter(|v| *v != view).map(|v| view_block_name(*v)).collect();
        frozen_layers.sort();
        let vp = Viewport { center, width: w, height: h, view_center, view_height, id: next_id + i as u32, frozen_layers, ..Viewport::default() };
        match d.add(&space, common, EntityKind::Viewport(vp)) {
            Ok(hd) => handles.push(hd),
            Err(e) => {
                d.layouts.retain(|l| l.name != name);
                return Err(SheetError::Schema(SchemaError::Malformed { what: format!("layout {name}"), message: e.to_string() }));
            }
        }
    }
    Ok(handles)
}

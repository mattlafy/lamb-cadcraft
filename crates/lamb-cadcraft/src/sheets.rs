//! Sheets (`cadcraft-integration.md` §13 phase 2): a paper layout with one viewport per LAMB
//! view. What a viewport shows is its data: `EntityData.view_id` names the view. Its frozen layers
//! follow from that: each viewport freezes every **other** view's layer, so CADCraft hides those
//! views' whole canonical inserts there, while model space keeps showing all views overlaid.
//!
//! A plan's assembly plan (§10, [`crate::assembly`]) is a canonical model-space insert on its own
//! layer [`assembly_block_name`]`(plan)`. Every viewport freezes every assembly layer except its
//! own; an assembly viewport (requested by plan id) shows only that plan's assembly block, is laid
//! after the views, and names its plan in `EntityData.assembly_of` instead of a `view_id`.
//!
//! [`sheet_freeze_observer`] keeps this true as views and assembly blocks are created: a viewport
//! naming what it shows freezes each new one in the creating command's own undo step. A viewport
//! naming neither is model space or a foreign one, is left alone and takes no number.
//!
//! Arrangement: a grid over the layout's [`usable_area`] (columns = ceil(sqrt(n)), filled row by
//! row from the top left in the given order), each viewport its cell shrunk by 5 % on every side
//! and centred on its own view block's extents at a standard drawing scale ([`STANDARD_SCALES`]):
//! plans, sections and elevations at the project's `plot_scale` when it fits, else the next larger
//! standard denominator that does; details at the smallest standard denominator ≥ 5 that fits; an
//! assembly viewport at its plan's scale. "Fits" means the extents grown 5 % each way fit the
//! viewport at that scale. VPVIEW rescales the same way; existing viewports are not rescaled.
//!
//! Generated annotation is the same size on paper at any scale (LAMB T644): [`view_scale`] sizes a
//! view's annotation block and detail-tag bubbles. One block serves every viewport of a view, so a
//! view on two sheets at two scales sizes for the first in [`numbered_viewports`] order. A plan,
//! section or elevation's viewport also fits its annotation: the block's overhang past the view
//! block counts in paper mm and the view is centred so the overhang is inside. An assembly viewport
//! and a detail take none, since the annotation's layer is frozen there or the view has none.
//!
//! Detail sheets (LAMB T637) are packed instead ([`pack_details`], [`create_detail_sheet`]): each
//! detail takes the Detail rule's scale against the whole [`usable_area`] less [`TITLE_ROOM_MM`],
//! and a viewport of its grown extents at that scale, at least [`MIN_VP_WIDTH_MM`] wide and
//! [`MIN_VP_HEIGHT_MM`] tall. Viewports fill rows from the top left in the given order, tops
//! aligned, [`GAP_X_MM`] apart; the next row starts [`GAP_Y_MM`] below the row's tallest, and a row
//! leaving less than [`TITLE_ROOM_MM`] above the bottom of the area starts a new sheet.
//!
//! A sheet plots as a drawing (LAMB T645): its viewports sit on the no-plot [`VPORT_LAYER`], so a
//! border shows on screen and not on paper while the contents still plot, and its page setup
//! starts on the monochrome plot style table, which a user can change. Screen colours stay.

use std::fmt;
use std::sync::Arc;

use cadcraft_doc::{Common, Drawing, EntityKind, Handle, Layout, MONOCHROME_CTB, PageSetup, Space, Viewport};
use cadcraft_engine::Session;
use cadcraft_geom::{Bounds2, Vec2, Vec3};
use cadcraft_render::{PaperSize, Sheet, paper_size};
use lamb_core::model::Id;

use lamb_core::model::AnyView;

use crate::schema::{
    EntityData, LambDocument, SchemaError, annotation_block_name, assembly_block_name, read_document, read_entity_data, set_entity_data,
    view_block_name,
};

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
    /// A requested plan has no assembly block yet.
    NoAssembly(Id),
    /// The handle is not a paper-space viewport other than a layout's overall one.
    NotAViewport,
}

impl fmt::Display for SheetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SheetError::Schema(e) => e.fmt(f),
            SheetError::LayoutExists(n) => write!(f, "Layout {n} already exists"),
            SheetError::NoSuchView(id) => write!(f, "No such view {}", id.0),
            SheetError::NoViews => f.write_str("No views to place on a sheet"),
            SheetError::NoAssembly(id) => write!(f, "Plan {} has no assembly plan — run ASSEMBLYPLAN", id.0),
            SheetError::NotAViewport => f.write_str("Not a layout viewport"),
        }
    }
}

impl std::error::Error for SheetError {}

impl From<SchemaError> for SheetError {
    fn from(e: SchemaError) -> Self {
        SheetError::Schema(e)
    }
}

/// Every assembly block's (and its layer's) name starts with this ([`assembly_block_name`]).
const ASSEMBLY_PREFIX: &str = "LAMB_ASSEMBLY_";

/// The layer a sheet's viewports sit on: it does not plot, so no rectangle prints around a drawing.
pub(crate) const VPORT_LAYER: &str = "LAMB-VPORTS";

/// Makes [`VPORT_LAYER`] when `d` lacks it, not plotting. Returns whether it was made; a layer
/// already there keeps its settings.
pub(crate) fn ensure_vport_layer(d: &mut Drawing) -> bool {
    let made = d.ensure_layer(VPORT_LAYER);
    if made && let Some(l) = d.layer_mut(VPORT_LAYER) {
        l.plot = false;
    }
    made
}

/// The standard drawing-scale denominators (1:n) a LAMB viewport plots at, ascending.
pub const STANDARD_SCALES: [f64; 13] = [1.0, 2.0, 5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 250.0, 500.0, 1000.0, 2000.0];

/// Details plot no larger than 1:this.
const DETAIL_MIN_SCALE: f64 = 5.0;

/// How a viewport's drawing scale is chosen ([`scaled_zoom`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScaleRule {
    /// Plan and vertical views (and assembly plans): `plot_scale` when it fits, else the next
    /// larger standard denominator that fits.
    Plan,
    /// Detail views: the smallest standard denominator ≥ [`DETAIL_MIN_SCALE`] that fits.
    Detail,
}

fn rule_of(v: &AnyView) -> ScaleRule {
    match v {
        AnyView::Detail(_) => ScaleRule::Detail,
        _ => ScaleRule::Plan,
    }
}

/// Whether bounds `ext` grown 5 % each way fit a `w` × `h` paper viewport at 1:`n`.
fn fits(ext: &Bounds2, n: f64, w: f64, h: f64) -> bool {
    let slack = 1.0 + 1e-9;
    ext.width() * 1.1 <= w * n * slack && ext.height() * 1.1 <= h * n * slack
}

/// The 1:n denominator `rule` picks for bounds `ext` in a `w` × `h` viewport. Empty bounds take
/// `plot_scale`; nothing fitting even at the series' largest takes the largest.
fn scale_for(ext: &Bounds2, rule: ScaleRule, plot_scale: f64, w: f64, h: f64) -> f64 {
    let largest = STANDARD_SCALES[STANDARD_SCALES.len() - 1];
    let plot_scale = if plot_scale.is_finite() && plot_scale > 0.0 { plot_scale } else { 50.0 };
    if ext.is_empty() || !(ext.width().is_finite() && ext.height().is_finite()) {
        return plot_scale;
    }
    let first_fitting = |min: f64| STANDARD_SCALES.iter().copied().filter(|n| *n >= min).find(|n| fits(ext, *n, w, h)).unwrap_or(largest);
    match rule {
        ScaleRule::Plan if fits(ext, plot_scale, w, h) => plot_scale,
        ScaleRule::Plan => first_fitting(plot_scale),
        ScaleRule::Detail => first_fitting(DETAIL_MIN_SCALE),
    }
}

/// What a viewport's content reaches past the shown block's extents on each side, in paper mm.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Overhang {
    left: f64,
    right: f64,
    bottom: f64,
    top: f64,
}

/// How far `view`'s annotation block reaches past its view block under `rule`, on paper at the
/// annotation's current size, which is what makes it independent of the scale being chosen. A
/// detail, an empty view block or an empty annotation block has none.
fn annotation_overhang(d: &Drawing, doc: &LambDocument, view: Id, rule: ScaleRule) -> Overhang {
    let ext = d.extents(&Space::Block(view_block_name(view)));
    let ann = d.extents(&Space::Block(annotation_block_name(view)));
    if rule != ScaleRule::Plan || ext.is_empty() || ann.is_empty() {
        return Overhang::default();
    }
    let n = view_scale(d, doc, view);
    Overhang {
        left: (ext.min.x - ann.min.x).max(0.0) / n,
        right: (ann.max.x - ext.max.x).max(0.0) / n,
        bottom: (ext.min.y - ann.min.y).max(0.0) / n,
        top: (ann.max.y - ext.max.y).max(0.0) / n,
    }
}

/// `(view_center, view_height)` of a `w` × `h` viewport showing `block` at the scale `rule` picks
/// for the room `over` leaves: centred on the block's extents plus `over` (the origin when empty),
/// `view_height = h * n`.
fn scaled_zoom(d: &Drawing, block: &str, rule: ScaleRule, plot_scale: f64, w: f64, h: f64, over: Overhang) -> (Vec2, f64) {
    let ext = d.extents(&Space::Block(block.to_owned()));
    let n = scale_for(&ext, rule, plot_scale, w - over.left - over.right, h - over.bottom - over.top);
    if ext.is_empty() {
        return (Vec2::new(0.0, 0.0), h * n);
    }
    let shift = Vec2::new((over.right - over.left) / 2.0 * n, (over.top - over.bottom) / 2.0 * n);
    (ext.center() + shift, h * n)
}

/// A viewport's drawing scale as the 1:n denominator, model mm per paper mm
/// (`view_height / height`): the nearest [`STANDARD_SCALES`] entry when within 1e-6 relative,
/// else the raw ratio.
pub fn viewport_scale(vp: &Viewport) -> f64 {
    let r = vp.view_height / vp.height;
    STANDARD_SCALES.iter().copied().find(|n| ((r - n) / n).abs() <= 1e-6).unwrap_or(r)
}

/// The 1:n denominator `view`'s generated annotation is sized for: the scale of its first viewport
/// in [`numbered_viewports`] order, else the project's `plot_scale`, else 50.
pub(crate) fn view_scale(d: &Drawing, doc: &LambDocument, view: Id) -> f64 {
    let usable = |n: &f64| n.is_finite() && *n > 0.0;
    numbered_viewports(d, &[(view, view_block_name(view))])
        .first()
        .and_then(|(_, _, _, h)| match &d.entity(*h)?.kind {
            EntityKind::Viewport(vp) => Some(viewport_scale(vp)),
            _ => None,
        })
        .filter(usable)
        .or(Some(doc.defaults.plot_scale).filter(usable))
        .unwrap_or(50.0)
}

/// The frozen-layer list of a viewport showing block `shown`: every LAMB view layer and every
/// assembly layer except `shown`, sorted.
fn frozen_except(view_layers: &[String], assembly_layers: &[String], shown: &str) -> Vec<String> {
    let mut out: Vec<String> = view_layers.iter().chain(assembly_layers).filter(|l| *l != shown).cloned().collect();
    out.sort();
    out
}

/// Every assembly layer in `d`.
fn assembly_layers(d: &Drawing) -> Vec<String> {
    d.layers.iter().map(|l| l.name.clone()).filter(|n| n.starts_with(ASSEMBLY_PREFIX)).collect()
}

/// Binds viewport `vp` to `view` (VPVIEW): writes its `EntityData.view_id`, freezes every other LAMB
/// view layer and every assembly layer in it (the rule [`create_sheet`] applies; non-LAMB frozen
/// layers are kept) and centres it on the view block at the view kind's standard scale. `None` clears `view_id` and thaws
/// every LAMB view layer. Refuses a handle that is not a paper-space viewport, a layout's overall
/// viewport (id 1), or an unknown view; nothing is changed on error. Either way the viewport stops
/// being an assembly viewport (`assembly_of` is cleared).
pub fn set_viewport_view(d: &mut Drawing, vp: Handle, view: Option<Id>) -> Result<(), SheetError> {
    let doc = read_document(d)?;
    let e = d.entity(vp).ok_or(SheetError::NotAViewport)?;
    let EntityKind::Viewport(port) = &e.kind else { return Err(SheetError::NotAViewport) };
    if port.id <= 1 || !matches!(d.space_of(vp), Some(Space::Paper(_))) {
        return Err(SheetError::NotAViewport);
    }
    if let Some(v) = view
        && !doc.views.iter().any(|x| x.id() == v)
    {
        return Err(SheetError::NoSuchView(v));
    }
    let view_layers: Vec<String> = doc.views.iter().map(|v| view_block_name(v.id())).collect();
    let assemblies = assembly_layers(d);
    let is_view_layer = |l: &String| view_layers.iter().any(|x| x.eq_ignore_ascii_case(l));
    let mut port = port.clone();
    let mut common = e.common.clone();
    let mut data = read_entity_data(&common)?;
    data.view_id = view;
    data.assembly_of = None;
    set_entity_data(&mut common, &data)?;
    match view {
        Some(v) => {
            let block = view_block_name(v);
            let mut frozen: Vec<String> =
                port.frozen_layers.iter().filter(|l| !is_view_layer(l) && !assemblies.iter().any(|a| a.eq_ignore_ascii_case(l))).cloned().collect();
            frozen.extend(frozen_except(&view_layers, &assemblies, &block));
            frozen.sort();
            port.frozen_layers = frozen;
            let rule = doc.views.iter().find(|x| x.id() == v).map_or(ScaleRule::Plan, rule_of);
            let over = annotation_overhang(d, &doc, v, rule);
            (port.view_center, port.view_height) = scaled_zoom(d, &block, rule, doc.defaults.plot_scale, port.width, port.height, over);
        }
        None => port.frozen_layers.retain(|l| !is_view_layer(l)),
    }
    d.modify_entity(vp, |e| {
        e.common = common;
        e.kind = EntityKind::Viewport(port);
    })
    .map_err(|e| SheetError::Schema(SchemaError::Malformed { what: "viewport".into(), message: e.to_string() }))
}

/// The paper a project's sheets default to.
pub(crate) const DEFAULT_SHEET_PAPER: &str = "ARCH D";

/// The paper size `doc` makes new sheets on: its `sheet_paper`, else ARCH D.
pub(crate) fn sheet_paper_size(doc: &LambDocument) -> Option<&'static PaperSize> {
    doc.sheet_paper.as_deref().and_then(paper_size).or_else(|| paper_size(DEFAULT_SHEET_PAPER))
}

/// The page setup of a new sheet in `doc`: its paper in landscape, 10 mm margins, the monochrome
/// plot style table, and paper units of millimetres whatever the drawing's MEASUREMENT says, as
/// every paper constant here assumes.
pub(crate) fn sheet_page(doc: &LambDocument) -> PageSetup {
    let mut page = PageSetup {
        landscape: true,
        paper_mm: true,
        margins_mm: [SHEET_MARGIN_MM; 4],
        plot_style_table: MONOCHROME_CTB.to_owned(),
        ..PageSetup::default()
    };
    if let Some(p) = sheet_paper_size(doc) {
        page.paper = p.name.to_owned();
        page.width_mm = p.width_mm;
        page.height_mm = p.height_mm;
    }
    page
}

/// The margin [`sheet_page`] gives a new sheet, mm.
pub(crate) const SHEET_MARGIN_MM: f64 = 10.0;

/// The width of the title strip down the right edge of `sheet`'s printable area
/// ([`crate::titleblock`]), in paper units: 70 mm, or a quarter of the printable width when less.
pub(crate) fn frame_strip_width(sheet: &Sheet) -> f64 {
    let p = if sheet.printable.is_empty() { sheet.bounds() } else { sheet.printable };
    (70.0 / sheet.unit_mm).min(0.25 * p.width())
}

/// Where a sheet's drawings go: the printable area less the title strip, 5 mm inside the border.
pub(crate) fn usable_area(sheet: &Sheet) -> Bounds2 {
    let p = if sheet.printable.is_empty() { sheet.bounds() } else { sheet.printable };
    let inset = 5.0 / sheet.unit_mm;
    Bounds2::new(Vec2::new(p.min.x + inset, p.min.y + inset), Vec2::new(p.max.x - frame_strip_width(sheet) - inset, p.max.y - inset))
}

/// A packed viewport is at least this wide, so a drawing title stays within it.
const MIN_VP_WIDTH_MM: f64 = 95.0;
/// A packed viewport is at least this tall.
const MIN_VP_HEIGHT_MM: f64 = 40.0;
/// Between packed viewports in a row.
const GAP_X_MM: f64 = 15.0;
/// Between a row's tallest viewport and the next row; clears the title under it.
const GAP_Y_MM: f64 = 25.0;
/// What a viewport's drawing title needs below it: [`crate::vptitles::TITLE_DROP_MM`] and the
/// title's lower half.
const TITLE_ROOM_MM: f64 = 15.0;
/// An empty detail is laid out as a square of this side, mm.
const EMPTY_DETAIL_MM: f64 = 500.0;

/// One viewport's place on paper and what it shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Packed {
    pub center: Vec2,
    pub width: f64,
    pub height: f64,
    pub view_center: Vec2,
    pub view_height: f64,
}

impl Packed {
    fn viewport(&self, id: u32, frozen_layers: Vec<String>) -> Viewport {
        Viewport {
            center: Vec3::new(self.center.x, self.center.y, 0.0),
            width: self.width,
            height: self.height,
            view_center: self.view_center,
            view_height: self.view_height,
            id,
            frozen_layers,
            ..Viewport::default()
        }
    }
}

/// `(1:n, width, height)` of the viewport a detail with extents `ext` takes in `area`.
fn detail_viewport(ext: &Bounds2, area: &Bounds2) -> (f64, f64, f64) {
    let n = scale_for(ext, ScaleRule::Detail, DETAIL_MIN_SCALE, area.width(), area.height() - TITLE_ROOM_MM);
    (n, (ext.width() * 1.1 / n).max(MIN_VP_WIDTH_MM), (ext.height() * 1.1 / n).max(MIN_VP_HEIGHT_MM))
}

/// Packs `sizes` (width, height) into rows over `area`, in order: each one's centre, sheet by
/// sheet. A sheet holds at most `per` when given. A viewport too big for an empty sheet still
/// takes one, alone.
fn pack_rows(sizes: &[(f64, f64)], area: &Bounds2, per: Option<usize>) -> Vec<Vec<Vec2>> {
    const SLACK: f64 = 1e-6;
    let room_for = |top: f64, h: f64| top - h - TITLE_ROOM_MM >= area.min.y - SLACK;
    let mut sheets = Vec::new();
    let mut sheet: Vec<Vec2> = Vec::new();
    let (mut x, mut top, mut tallest) = (area.min.x, area.max.y, 0.0_f64);
    for &(w, h) in sizes {
        let open = !sheet.is_empty() && per.is_none_or(|p| sheet.len() < p.max(1));
        let in_row = open && x + w <= area.max.x + SLACK && room_for(top, tallest.max(h));
        if !in_row {
            if open && room_for(top - tallest - GAP_Y_MM, h) {
                top -= tallest + GAP_Y_MM;
            } else {
                if !sheet.is_empty() {
                    sheets.push(std::mem::take(&mut sheet));
                }
                top = area.max.y;
            }
            (x, tallest) = (area.min.x, 0.0);
        }
        sheet.push(Vec2::new(x + w / 2.0, top - h / 2.0));
        x += w + GAP_X_MM;
        tallest = tallest.max(h);
    }
    if !sheet.is_empty() {
        sheets.push(sheet);
    }
    sheets
}

/// Packs detail views `views`, in order, onto as many new sheets of the project's paper as they
/// need, at most `per` to a sheet when given.
pub(crate) fn pack_details(d: &Drawing, views: &[Id], per: Option<usize>) -> Result<Vec<Vec<(Id, Packed)>>, SheetError> {
    let doc = read_document(d)?;
    let area = usable_area(&Sheet::from_page(&sheet_page(&doc), 1.0));
    let half = EMPTY_DETAIL_MM / 2.0;
    let sized: Vec<(Id, Bounds2, f64, f64, f64)> = views
        .iter()
        .map(|v| {
            let ext = d.extents(&Space::Block(view_block_name(*v)));
            let drawn = !ext.is_empty() && ext.width().is_finite() && ext.height().is_finite();
            let ext = if drawn { ext } else { Bounds2::new(Vec2::new(-half, -half), Vec2::new(half, half)) };
            let (n, w, h) = detail_viewport(&ext, &area);
            (*v, ext, n, w, h)
        })
        .collect();
    let sizes: Vec<(f64, f64)> = sized.iter().map(|(_, _, _, w, h)| (*w, *h)).collect();
    let mut rest = sized.iter();
    Ok(pack_rows(&sizes, &area, per)
        .into_iter()
        .map(|centers| {
            centers
                .into_iter()
                .zip(rest.by_ref())
                .map(|(center, (v, ext, n, w, h))| (*v, Packed { center, width: *w, height: *h, view_center: ext.center(), view_height: h * n }))
                .collect()
        })
        .collect())
}

/// The id after every viewport's in `d`, overall ones included.
fn next_viewport_id(d: &Drawing) -> u32 {
    d.layouts
        .iter()
        .flat_map(|l| l.entities.iter())
        .filter_map(|e| match &e.kind {
            EntityKind::Viewport(vp) => Some(vp.id),
            _ => None,
        })
        .fold(1u32, u32::max)
        + 1
}

/// Adds layout `name` on the project's paper, after every other tab.
fn push_sheet_layout(d: &mut Drawing, doc: &LambDocument, name: &str) {
    let tab_order = d.layouts.iter().map(|l| l.tab_order).max().map_or(0, |t| t + 1);
    d.layouts.push(Layout { page: sheet_page(doc), ..Layout::new(name, tab_order) });
}

/// Adds `viewports` to the just-pushed layout `name` on [`VPORT_LAYER`], taking the layout (and
/// the layer, if made here) back out on failure.
fn add_viewports(d: &mut Drawing, name: &str, viewports: Vec<(Common, Viewport)>) -> Result<Vec<Handle>, SheetError> {
    let space = Space::Paper(name.to_owned());
    let made_layer = ensure_vport_layer(d);
    let mut handles = Vec::with_capacity(viewports.len());
    for (mut common, vp) in viewports {
        common.layer = VPORT_LAYER.to_owned();
        match d.add(&space, common, EntityKind::Viewport(vp)) {
            Ok(hd) => handles.push(hd),
            Err(e) => {
                d.layouts.retain(|l| l.name != name);
                if made_layer {
                    d.layers.retain(|l| l.name != VPORT_LAYER);
                }
                return Err(SheetError::Schema(SchemaError::Malformed { what: format!("layout {name}"), message: e.to_string() }));
            }
        }
    }
    Ok(handles)
}

/// Creates layout `name` with one viewport per placed view, in that order, each freezing every
/// other LAMB view's layer and every assembly layer. Returns the viewports' handles. Nothing is
/// changed on error.
pub(crate) fn create_detail_sheet(d: &mut Drawing, name: &str, placed: &[(Id, Packed)]) -> Result<Vec<Handle>, SheetError> {
    let doc = read_document(d)?;
    if d.layout(name).is_some() {
        return Err(SheetError::LayoutExists(name.to_owned()));
    }
    if placed.is_empty() {
        return Err(SheetError::NoViews);
    }
    let view_layers: Vec<String> = doc.views.iter().map(|v| view_block_name(v.id())).collect();
    let assemblies = assembly_layers(d);
    let next_id = next_viewport_id(d);
    let mut viewports = Vec::with_capacity(placed.len());
    for (i, (view, packed)) in placed.iter().enumerate() {
        if !doc.views.iter().any(|v| v.id() == *view) {
            return Err(SheetError::NoSuchView(*view));
        }
        let mut common = Common::default();
        set_entity_data(&mut common, &EntityData { view_id: Some(*view), ..EntityData::default() })?;
        viewports.push((common, packed.viewport(next_id + i as u32, frozen_except(&view_layers, &assemblies, &view_block_name(*view)))));
    }
    push_sheet_layout(d, &doc, name);
    add_viewports(d, name, viewports)
}

/// Creates layout `name` with one viewport per view in `views`, then one per plan or placed
/// vertical in `assemblies` showing that view's assembly block (every view in document order when both are empty). Each
/// viewport freezes every other LAMB view's layer and every assembly layer but its own. Returns
/// the viewports' handles in that order. Nothing is changed on error.
pub fn create_sheet(d: &mut Drawing, name: &str, views: &[Id], assemblies: &[Id]) -> Result<Vec<Handle>, SheetError> {
    let doc = read_document(d)?;
    if d.layout(name).is_some() {
        return Err(SheetError::LayoutExists(name.to_owned()));
    }
    let all: Vec<Id> = doc.views.iter().map(|v| v.id()).collect();
    let chosen: Vec<Id> = if views.is_empty() && assemblies.is_empty() { all.clone() } else { views.to_vec() };
    if chosen.is_empty() && assemblies.is_empty() {
        return Err(SheetError::NoViews);
    }
    if let Some(bad) = chosen.iter().find(|v| !all.contains(v)) {
        return Err(SheetError::NoSuchView(*bad));
    }
    for plan in assemblies {
        let host = |v: &AnyView| match v {
            AnyView::Plan(_) => true,
            AnyView::Vertical(x) => x.placement.is_some(),
            _ => false,
        };
        if !doc.views.iter().any(|v| v.id() == *plan && host(v)) {
            return Err(SheetError::NoSuchView(*plan));
        }
        if d.block(&assembly_block_name(*plan)).is_none() {
            return Err(SheetError::NoAssembly(*plan));
        }
    }
    let view_layers: Vec<String> = all.iter().map(|v| view_block_name(*v)).collect();
    let assembly_layers = assembly_layers(d);
    // (block shown, EntityData) per viewport, built before anything changes.
    // An assembly slot also names its view, whose scale it takes.
    let slots: Vec<(String, EntityData, Option<Id>)> = chosen
        .iter()
        .map(|v| (view_block_name(*v), EntityData { view_id: Some(*v), ..EntityData::default() }, None))
        .chain(assemblies.iter().map(|p| (assembly_block_name(*p), EntityData { assembly_of: Some(*p), ..EntityData::default() }, Some(*p))))
        .collect();
    let mut commons = Vec::with_capacity(slots.len());
    for (_, dt, _) in &slots {
        let mut c = Common::default();
        set_entity_data(&mut c, dt)?;
        commons.push(c);
    }

    let next_id = next_viewport_id(d);
    push_sheet_layout(d, &doc, name);
    let area = usable_area(&cadcraft_render::sheet(d, name).unwrap_or_else(|| Sheet::from_page(&sheet_page(&doc), 1.0)));

    let n = slots.len();
    let cols = (n as f64).sqrt().ceil().max(1.0) as usize;
    let rows = n.div_ceil(cols);
    let cell = Vec2::new(area.width() / cols as f64, area.height() / rows as f64);
    let (w, h) = (cell.x * 0.9, cell.y * 0.9);
    let plot_scale = doc.defaults.plot_scale;
    let mut viewports = Vec::with_capacity(n);
    for (i, ((block, dt, plan), common)) in slots.iter().zip(commons).enumerate() {
        let (r, c) = (i / cols, i % cols);
        let center = Vec2::new(area.min.x + cell.x * (c as f64 + 0.5), area.max.y - cell.y * (r as f64 + 0.5));
        let rule = dt.view_id.and_then(|v| doc.views.iter().find(|x| x.id() == v)).map_or(ScaleRule::Plan, rule_of);
        let over = dt.view_id.map_or_else(Overhang::default, |v| annotation_overhang(d, &doc, v, rule));
        let (view_center, mut view_height) = scaled_zoom(d, block, rule, plot_scale, w, h, over);
        if let Some(p) = plan {
            // The view's own scale, unless the assembly block needs a smaller one to fit.
            view_height = view_height.max(scaled_zoom(d, &view_block_name(*p), ScaleRule::Plan, plot_scale, w, h, Overhang::default()).1);
        }
        let frozen_layers = frozen_except(&view_layers, &assembly_layers, block);
        let packed = Packed { center, width: w, height: h, view_center, view_height };
        viewports.push((common, packed.viewport(next_id + i as u32, frozen_layers)));
    }
    add_viewports(d, name, viewports)
}

/// The after-command observer: every paper viewport (id > 1) naming what it shows in `view_id` or
/// `assembly_of` freezes every LAMB view layer and assembly layer except the one it shows. Layers
/// are only ever added, non-LAMB layers untouched, and the drawing is written back only when one
/// was missing, so a view or assembly block made by a command is frozen in that command's undo step.
pub fn sheet_freeze_observer(s: &mut Session, _before: &Arc<Drawing>) {
    let Ok(d) = s.doc() else { return };
    let named: Vec<(Handle, String, &Viewport)> = d
        .layouts
        .iter()
        .flat_map(|l| l.entities.iter())
        .filter_map(|e| {
            let EntityKind::Viewport(vp) = &e.kind else { return None };
            if vp.id <= 1 {
                return None;
            }
            let data = read_entity_data(&e.common).ok()?;
            let shown = data.view_id.map(view_block_name).or_else(|| data.assembly_of.map(assembly_block_name))?;
            Some((e.handle, shown, vp))
        })
        .collect();
    if named.is_empty() {
        return;
    }
    let doc = match read_document(d) {
        Ok(doc) => doc,
        Err(e) => {
            s.echo(format!("LAMB sheets: {e}"));
            return;
        }
    };
    let view_layers: Vec<String> = doc.views.iter().map(|v| view_block_name(v.id())).collect();
    let assemblies = assembly_layers(d);
    let todo: Vec<(Handle, Vec<String>)> = named
        .into_iter()
        .filter_map(|(h, shown, vp)| {
            let missing: Vec<String> = frozen_except(&view_layers, &assemblies, &shown)
                .into_iter()
                .filter(|l| !vp.frozen_layers.iter().any(|f| f.eq_ignore_ascii_case(l)))
                .collect();
            (!missing.is_empty()).then_some((h, missing))
        })
        .collect();
    if todo.is_empty() {
        return;
    }
    let mut after = d.clone();
    for (h, missing) in todo {
        let frozen = after.modify_entity(h, |e| {
            if let EntityKind::Viewport(vp) = &mut e.kind {
                vp.frozen_layers.extend(missing);
                vp.frozen_layers.sort();
            }
        });
        if let Err(e) = frozen {
            s.echo(format!("LAMB sheets: {e}"));
            return;
        }
    }
    if let Ok(d) = s.doc_mut() {
        *d = after;
    }
}

/// Every view's sheet reference at once: `(view, n, layout)` for each view some single-view
/// viewport shows, `n` that viewport's 1-based position (by viewport `id`) among its layout's
/// single-view viewports, the first layout by name winning. `view_blocks` is every LAMB view with
/// its block name. A single-view viewport is one whose `EntityData.view_id` names one of those
/// views, whatever it freezes; assembly viewports name no view and take no number.
pub fn sheet_refs(d: &Drawing, view_blocks: &[(Id, String)]) -> Vec<(Id, usize, String)> {
    let mut out: Vec<(Id, usize, String)> = Vec::new();
    for (view, n, layout, _) in numbered_viewports(d, view_blocks) {
        if !out.iter().any(|(v, _, _)| *v == view) {
            out.push((view, n, layout));
        }
    }
    out
}

/// Every single-view viewport on every layout, layouts by name and viewports by `id`:
/// `(view, n, layout, viewport handle)`, numbered as [`sheet_refs`] numbers them. A view shown on
/// several layouts appears once per viewport.
pub(crate) fn numbered_viewports(d: &Drawing, view_blocks: &[(Id, String)]) -> Vec<(Id, usize, String, Handle)> {
    let mut layouts: Vec<&Layout> = d.layouts.iter().collect();
    layouts.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = Vec::new();
    for l in layouts {
        let mut vps: Vec<(u32, Handle, Id)> = l
            .entities
            .iter()
            .filter_map(|e| {
                let EntityKind::Viewport(vp) = &e.kind else { return None };
                let view = read_entity_data(&e.common).ok()?.view_id?;
                view_blocks.iter().any(|(v, _)| *v == view).then_some((vp.id, e.handle, view))
            })
            .collect();
        vps.sort_by_key(|(id, _, _)| *id);
        for (i, (_, h, view)) in vps.into_iter().enumerate() {
            out.push((view, i + 1, l.name.clone(), h));
        }
    }
    out
}

/// The sheet showing `view`: `(n, layout)` as [`sheet_refs`] numbers it, `None` when no sheet
/// (or an unreadable LAMB document) shows it.
pub fn view_sheet_ref(d: &Drawing, view: Id) -> Option<(usize, String)> {
    let doc = read_document(d).ok()?;
    let blocks: Vec<(Id, String)> = doc.views.iter().map(|v| (v.id(), view_block_name(v.id()))).collect();
    sheet_refs(d, &blocks).into_iter().find(|(v, _, _)| *v == view).map(|(_, n, l)| (n, l))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assembly::tests::{room_with, undo_len};
    use cadcraft_engine::Session;
    use serde_json::json;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    /// (viewport, its `view_id`) for every viewport in layout `name`.
    fn viewports(d: &Drawing, name: &str) -> T<Vec<(Viewport, Option<Id>)>> {
        let l = d.layout(name).ok_or_else(|| format!("no layout {name}"))?;
        let mut out = Vec::new();
        for e in l.entities.iter() {
            if let EntityKind::Viewport(vp) = &e.kind {
                out.push((vp.clone(), read_entity_data(&e.common)?.view_id));
            }
        }
        Ok(out)
    }

    fn views(s: &Session) -> T<Vec<(Id, bool)>> {
        Ok(read_document(s.doc()?)?.views.iter().map(|v| (v.id(), matches!(v, AnyView::Plan(_)))).collect())
    }

    #[test]
    fn sheets_freeze_assembly_layers_and_lay_out_a_plans_assembly() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        let vs = views(&s)?;
        let detail = vs.iter().find(|v| !v.1).ok_or("no detail")?.0;
        let name = assembly_block_name(plan);

        // Refusals before ASSEMBLYPLAN, and for a detail id: nothing changes.
        let (layouts, n) = (s.doc()?.layouts.len(), undo_len(&s)?);
        let e = s.execute("newsheet", &json!({ "name": "S0", "assemblyIds": [plan.0] })).err().ok_or("no assembly accepted")?;
        assert!(e.to_string().contains("has no assembly plan"), "{e}");
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let (layouts, n) = (layouts, n + 1);
        let e = s.execute("newsheet", &json!({ "name": "S0", "assemblyIds": [detail.0] })).err().ok_or("detail accepted")?;
        assert!(e.to_string().contains("No such view"), "{e}");
        assert_eq!((s.doc()?.layouts.len(), undo_len(&s)?), (layouts, n));

        s.execute("newsheet", &json!({ "name": "S1" }))?;
        let vps = viewports(s.doc()?, "S1")?;
        assert_eq!(vps.len(), vs.len());
        assert!(vps.iter().all(|(vp, v)| v.is_some() && vp.frozen_layers.contains(&name)), "{vps:?}");

        let out = s.execute("newsheet", &json!({ "name": "S2", "assemblyIds": [plan.0] }))?;
        assert_eq!(out["viewports"].as_array().map(Vec::len), Some(1), "{out}");
        let check = |d: &Drawing| -> R {
            let vps = viewports(d, "S2")?;
            assert_eq!(vps.len(), 1);
            let (vp, v) = &vps[0];
            assert_eq!(*v, None, "not a view");
            let mut want: Vec<String> = vs.iter().map(|v| view_block_name(v.0)).collect();
            want.sort();
            assert_eq!(vp.frozen_layers, want);
            let c = d.extents(&Space::Block(name.clone())).center();
            assert!((vp.view_center.x - c.x).abs() < 1e-6 && (vp.view_center.y - c.y).abs() < 1e-6, "{:?} vs {c:?}", vp.view_center);
            Ok(())
        };
        check(s.doc()?)?;
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes())?;
        check(&back)?;
        Ok(())
    }

    #[test]
    fn view_sheet_ref_numbers_single_view_viewports_and_skips_assembly_ones() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        let detail = views(&s)?.iter().find(|v| !v.1).ok_or("no detail")?.0;
        assert_eq!(view_sheet_ref(s.doc()?, detail), None, "on no sheet");
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        s.execute("newsheet", &json!({ "name": "S2", "assemblyIds": [plan.0] }))?;
        assert_eq!((view_sheet_ref(s.doc()?, detail), view_sheet_ref(s.doc()?, plan)), (None, None), "assembly viewports show no view");

        s.execute("newsheet", &json!({ "name": "S3", "viewIds": [plan.0, detail.0], "assemblyIds": [plan.0] }))?;
        assert_eq!(view_sheet_ref(s.doc()?, plan), Some((1, "S3".to_owned())));
        assert_eq!(view_sheet_ref(s.doc()?, detail), Some((2, "S3".to_owned())));
        // An earlier name wins; numbering is per layout.
        s.execute("newsheet", &json!({ "name": "S1", "viewIds": [detail.0] }))?;
        assert_eq!(view_sheet_ref(s.doc()?, detail), Some((1, "S1".to_owned())));
        assert_eq!(view_sheet_ref(s.doc()?, plan), Some((1, "S3".to_owned())));
        Ok(())
    }

    /// The frozen layers of layout `name`'s single viewport.
    fn frozen_in(s: &Session, name: &str) -> T<Vec<String>> {
        Ok(only_viewport(s, name)?.frozen_layers)
    }

    /// The `EntityData` of layout `name`'s single viewport, with its handle.
    fn only_viewport_data(s: &Session, name: &str) -> T<(Handle, EntityData)> {
        let l = s.doc()?.layout(name).ok_or("no layout")?;
        for e in l.entities.iter() {
            if matches!(e.kind, EntityKind::Viewport(_)) {
                return Ok((e.handle, read_entity_data(&e.common)?));
            }
        }
        Err("no viewport".into())
    }

    #[test]
    fn numbering_reads_view_id_not_frozen_layers() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        let detail = views(&s)?.iter().find(|v| !v.1).ok_or("no detail")?.0;
        s.execute("newsheet", &json!({ "name": "S1", "viewIds": [plan.0, detail.0] }))?;
        let d = s.doc_mut()?;
        let hs: Vec<Handle> =
            d.layout("S1").ok_or("no S1")?.entities.iter().filter(|e| matches!(e.kind, EntityKind::Viewport(_))).map(|e| e.handle).collect();
        for h in hs {
            d.modify_entity(h, |e| {
                if let EntityKind::Viewport(vp) = &mut e.kind {
                    vp.frozen_layers.clear();
                }
            })?;
        }
        assert_eq!(view_sheet_ref(s.doc()?, plan), Some((1, "S1".to_owned())));
        assert_eq!(view_sheet_ref(s.doc()?, detail), Some((2, "S1".to_owned())));
        // A viewport naming a view outside the list takes no number.
        let refs = sheet_refs(s.doc()?, &[(detail, view_block_name(detail))]);
        assert_eq!(refs, [(detail, 1, "S1".to_owned())]);
        Ok(())
    }

    #[test]
    fn an_assembly_block_made_later_is_frozen_in_existing_viewports() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        crate::maintain::install(&mut s);
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        s.execute("newsheet", &json!({ "name": "A1", "assemblyIds": [plan.0] }))?;
        let (asm, data) = only_viewport_data(&s, "A1")?;
        assert_eq!((data.view_id, data.assembly_of), (None, Some(plan)));
        assert_eq!(view_sheet_ref(s.doc()?, plan), None, "assembly viewports take no number");

        let second = Id(s.execute("newplan", &json!({ "name": "Second" }))?["viewId"].as_u64().ok_or("no id")?);
        let name = assembly_block_name(second);
        assert!(frozen_in(&s, "A1")?.contains(&view_block_name(second)), "a later view too");
        assert!(!frozen_in(&s, "A1")?.contains(&name));
        s.execute("assemblyplan", &json!({ "viewId": second.0 }))?;
        assert!(s.doc()?.block(&name).is_some(), "the block exists");
        let frozen = frozen_in(&s, "A1")?;
        assert!(frozen.contains(&name), "{frozen:?}");
        assert!(!frozen.contains(&assembly_block_name(plan)), "its own stays thawed");

        // VPVIEW turns it into a view viewport, whichever way it rebinds.
        s.execute("vpview", &json!({ "handle": format!("{:X}", asm.0), "viewId": plan.0 }))?;
        assert_eq!(only_viewport_data(&s, "A1")?.1.assembly_of, None);
        s.undo()?;
        assert_eq!(only_viewport_data(&s, "A1")?.1.assembly_of, Some(plan));
        s.execute("vpview", &json!({ "handle": format!("{:X}", asm.0), "viewId": null }))?;
        assert_eq!(only_viewport_data(&s, "A1")?.1, EntityData::default());
        Ok(())
    }

    #[test]
    fn a_viewport_naming_nothing_is_left_alone() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        crate::maintain::install(&mut s);
        s.execute("newsheet", &json!({ "name": "S1", "viewIds": [plan.0] }))?;
        let (h, _) = only_viewport_data(&s, "S1")?;
        s.execute("vpview", &json!({ "handle": format!("{:X}", h.0), "viewId": null }))?;
        let before = frozen_in(&s, "S1")?;
        s.execute("newplan", &json!({ "name": "Second" }))?;
        assert_eq!(frozen_in(&s, "S1")?, before, "model space or foreign: untouched");
        assert_eq!(view_sheet_ref(s.doc()?, plan), None);
        Ok(())
    }

    /// The single viewport of layout `name`.
    fn only_viewport(s: &Session, name: &str) -> T<Viewport> {
        let vps = viewports(s.doc()?, name)?;
        assert_eq!(vps.len(), 1, "{vps:?}");
        Ok(vps[0].0.clone())
    }

    #[test]
    fn viewports_plot_at_standard_scales_by_view_kind() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 200.0)?;
        let detail = views(&s)?.iter().find(|v| !v.1).ok_or("no detail")?.0;
        assert_eq!(read_document(s.doc()?)?.defaults.plot_scale, 50.0);

        // The room fits its cell at the project's 1:50.
        s.execute("newsheet", &json!({ "name": "P1", "viewIds": [plan.0] }))?;
        let vp = only_viewport(&s, "P1")?;
        assert!((vp.view_height - vp.height * 50.0).abs() < 1e-9, "{vp:?}");
        assert_eq!(viewport_scale(&vp), 50.0);
        let c = s.doc()?.extents(&Space::Block(view_block_name(plan))).center();
        assert!((vp.view_center.x - c.x).abs() < 1e-6 && (vp.view_center.y - c.y).abs() < 1e-6);

        // The 1 m detail plots at 1:5, the largest a detail plots at.
        let out = s.execute("newsheet", &json!({ "name": "D1", "viewIds": [detail.0] }))?;
        assert_eq!(viewport_scale(&only_viewport(&s, "D1")?), 5.0);

        // VPVIEW onto the plan applies the plan rule.
        let hex = out["viewports"][0].as_str().ok_or("no handle")?.to_owned();
        s.execute("vpview", &json!({ "handle": hex, "viewId": plan.0 }))?;
        assert_eq!(viewport_scale(&only_viewport(&s, "D1")?), 50.0);

        // An assembly viewport takes its plan's scale.
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        s.execute("newsheet", &json!({ "name": "A1", "assemblyIds": [plan.0] }))?;
        assert_eq!(viewport_scale(&only_viewport(&s, "A1")?), 50.0);

        // A plan too big for 1:50 takes the next standard scale that fits.
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("line", &json!({ "points": [[0, 0], [40000, 0]] }))?;
        s.execute("newsheet", &json!({ "name": "P2", "viewIds": [plan.0] }))?;
        let vp = only_viewport(&s, "P2")?;
        assert_eq!(viewport_scale(&vp), 100.0, "{vp:?}");
        // Existing viewports are not rescaled.
        assert_eq!(viewport_scale(&only_viewport(&s, "P1")?), 50.0);
        Ok(())
    }

    #[test]
    fn scale_rules_pick_from_the_series() {
        let b = |w: f64, h: f64| Bounds2::new(Vec2::new(0.0, 0.0), Vec2::new(w, h));
        // 100 × 100 paper: 1:50 holds 5000 / 1.1.
        assert_eq!(scale_for(&b(4000.0, 4000.0), ScaleRule::Plan, 50.0, 100.0, 100.0), 50.0);
        assert_eq!(scale_for(&b(6000.0, 100.0), ScaleRule::Plan, 50.0, 100.0, 100.0), 100.0);
        assert_eq!(scale_for(&b(100.0, 100.0), ScaleRule::Plan, 50.0, 100.0, 100.0), 50.0, "never larger than plot_scale");
        assert_eq!(scale_for(&b(100.0, 100.0), ScaleRule::Plan, 75.0, 100.0, 100.0), 75.0, "off-series plot_scale used as-is");
        assert_eq!(scale_for(&b(8000.0, 100.0), ScaleRule::Plan, 75.0, 100.0, 100.0), 100.0);
        assert_eq!(scale_for(&b(10.0, 10.0), ScaleRule::Detail, 50.0, 100.0, 100.0), 5.0, "never 1:2 or 1:1");
        assert_eq!(scale_for(&b(900.0, 10.0), ScaleRule::Detail, 50.0, 100.0, 100.0), 10.0);
        assert_eq!(scale_for(&b(1e9, 10.0), ScaleRule::Detail, 50.0, 100.0, 100.0), 2000.0, "nothing fits: 1:2000");
        assert_eq!(scale_for(&Bounds2::EMPTY, ScaleRule::Detail, 50.0, 100.0, 100.0), 50.0, "empty: plot_scale");
        let vp = Viewport { height: 3.0, view_height: 3.0 * 25.0 * (1.0 + 1e-9), ..Viewport::default() };
        assert_eq!(viewport_scale(&vp), 25.0);
        let vp = Viewport { height: 3.0, view_height: 3.0 * 30.0, ..Viewport::default() };
        assert!((viewport_scale(&vp) - 30.0).abs() < 1e-9);
    }

    /// The rectangle a viewport of `size` at `c` and its title need, as (min x, min y, max x, max y).
    fn claim(c: Vec2, size: (f64, f64)) -> (f64, f64, f64, f64) {
        (c.x - size.0 / 2.0, c.y - size.1 / 2.0 - TITLE_ROOM_MM, c.x + size.0 / 2.0, c.y + size.1 / 2.0)
    }

    #[test]
    fn mixed_sizes_pack_in_order_inside_the_area_without_touching() {
        let area = Bounds2::new(Vec2::new(15.0, 15.0), Vec2::new(829.0, 594.6));
        let sizes: Vec<(f64, f64)> =
            [(95.0, 40.0), (300.0, 120.0), (95.0, 66.0), (500.0, 200.0), (400.0, 90.0), (95.0, 300.0), (700.0, 250.0), (95.0, 40.0)]
                .iter()
                .cycle()
                .take(40)
                .copied()
                .collect();
        let sheets = pack_rows(&sizes, &area, None);
        assert!(sheets.len() > 1, "they overflow");
        assert_eq!(sheets.iter().map(Vec::len).sum::<usize>(), sizes.len(), "every one placed, once");
        let mut left = sizes.iter();
        for centers in &sheets {
            let rects: Vec<_> = centers.iter().zip(left.by_ref()).map(|(c, s)| claim(*c, *s)).collect();
            for (i, a) in rects.iter().enumerate() {
                assert!(a.0 >= area.min.x - 1e-6 && a.1 >= area.min.y - 1e-6 && a.2 <= area.max.x + 1e-6 && a.3 <= area.max.y + 1e-6, "{a:?}");
                for b in &rects[i + 1..] {
                    assert!(a.2 <= b.0 || b.2 <= a.0 || a.3 <= b.1 || b.3 <= a.1, "{a:?} and {b:?}");
                    // Reads left to right, then down: tops never rise, and a level top moves right.
                    assert!(b.3 < a.3 || (b.3 == a.3 && b.0 > a.0), "{a:?} before {b:?}");
                }
            }
        }
        // The order within a row is the given one, and a row's gaps are the settled ones.
        let first = &sheets[0];
        assert_eq!(first[0], Vec2::new(15.0 + 47.5, 594.6 - 20.0));
        assert!((first[1].x - (15.0 + 95.0 + GAP_X_MM + 150.0)).abs() < 1e-9, "{first:?}");
    }

    #[test]
    fn the_next_row_clears_the_tallest_and_a_row_too_low_for_its_title_turns_the_sheet() {
        let area = Bounds2::new(Vec2::new(0.0, 0.0), Vec2::new(300.0, 200.0));
        // 100 tall and 50 tall in a row; the next row starts below the 100.
        let s = pack_rows(&[(120.0, 100.0), (120.0, 50.0), (120.0, 40.0)], &area, None);
        assert_eq!(s, [vec![Vec2::new(60.0, 150.0), Vec2::new(195.0, 175.0), Vec2::new(60.0, 55.0)]]);
        // 75 still leaves room for the title (200 - 125 - 75 = 0 is short of 15): a new sheet.
        let s = pack_rows(&[(120.0, 100.0), (290.0, 75.0)], &area, None);
        assert_eq!(s.len(), 2, "{s:?}");
        // 60 leaves exactly 15.
        let s = pack_rows(&[(120.0, 100.0), (290.0, 60.0)], &area, None);
        assert_eq!(s.len(), 1, "{s:?}");
        // A taller neighbour that would push the row's title off the bottom turns the sheet too.
        let s = pack_rows(&[(120.0, 100.0), (120.0, 190.0)], &area, None);
        assert_eq!(s.len(), 2, "{s:?}");
        // Too big for any sheet: alone, and packing still ends.
        let s = pack_rows(&[(50.0, 50.0), (900.0, 900.0), (50.0, 50.0)], &area, None);
        assert_eq!(s.iter().map(Vec::len).collect::<Vec<_>>(), [1, 1, 1], "{s:?}");
        // `per` caps a sheet.
        let s = pack_rows(&[(50.0, 50.0); 5], &area, Some(2));
        assert_eq!(s.iter().map(Vec::len).collect::<Vec<_>>(), [2, 2, 1]);
    }

    #[test]
    fn a_detail_viewport_takes_the_scale_that_fits_and_its_minimum_size() {
        let area = Bounds2::new(Vec2::new(15.0, 15.0), Vec2::new(829.0, 594.6));
        let b = |w: f64, h: f64| Bounds2::new(Vec2::new(0.0, 0.0), Vec2::new(w, h));
        assert_eq!(detail_viewport(&b(300.0, 300.0), &area), (5.0, 95.0, 300.0 * 1.1 / 5.0), "never larger than 1:5; a title's width");
        assert_eq!(detail_viewport(&b(10.0, 10.0), &area), (5.0, 95.0, 40.0));
        assert_eq!(detail_viewport(&b(6000.0, 6000.0), &area).0, 20.0, "6600 / 1:10 is too tall");
        // Tall enough for the area but not for the area and a title: the next scale.
        let (n, _, h) = detail_viewport(&b(100.0, 2600.0), &area);
        assert_eq!(n, 10.0);
        assert!(h + TITLE_ROOM_MM <= area.height(), "{h}");
    }

    /// Plots layout `sheet` and returns, per optional content group (by layer name) whose marked
    /// content holds a path operator, the number of path operators. Read back with craft-pdf's
    /// reader, as `crates/io/tests/support/pdf_read.rs` does.
    fn plotted_groups(d: &Drawing, sheet: &str) -> T<Vec<(String, usize)>> {
        use craft_pdf::syntax::object::{Dict, String as PdfString};
        use std::ops::Deref;
        let bytes = cadcraft_io::plot(d, &Space::Paper(sheet.into()), &json!({ "compress": false }))?;
        let pdf = craft_pdf::open_slice(&bytes, None).map_err(|e| format!("the PDF opens: {e:?}"))?;
        let pages = pdf.pages();
        let page = pages.iter().next().ok_or("no page")?;
        let raw = page.raw();
        let resources = raw.get::<Dict<'_>>(b"Resources");
        // Marked-content property name -> the name of the optional content group it refers to.
        let props: Vec<(String, String)> = match resources.as_ref().and_then(|r| r.get::<Dict<'_>>(b"Properties")) {
            Some(p) => p
                .keys()
                .filter_map(|k| {
                    let ocg = p.get::<Dict<'_>>(k.deref())?;
                    let name = ocg.get::<PdfString<'_>>(b"Name").map(|n| String::from_utf8_lossy(n.as_bytes()).into_owned())?;
                    Some((k.as_str().to_string(), name))
                })
                .collect(),
            None => Vec::new(),
        };
        let content = String::from_utf8_lossy(page.page_stream().ok_or("no content")?).into_owned();
        // Operators and the names before them; text strings are skipped whole.
        let mut out: Vec<(String, usize)> = Vec::new();
        let mut group: Option<String> = None;
        let mut names: Vec<String> = Vec::new();
        let b: Vec<char> = content.chars().collect();
        let mut i = 0;
        let delim = |c: char| c.is_whitespace() || "()<>[]{}/%".contains(c);
        while i < b.len() {
            let c = b[i];
            if c == '(' {
                let mut depth = 0;
                while i < b.len() {
                    match b[i] {
                        '\\' => i += 1,
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                i += 1;
            } else if delim(c) && c != '/' {
                i += 1;
            } else {
                let start = i;
                i += 1;
                while i < b.len() && !delim(b[i]) {
                    i += 1;
                }
                let tok: String = b[start..i].iter().collect();
                if let Some(n) = tok.strip_prefix('/') {
                    names.push(n.to_string());
                } else if tok.parse::<f64>().is_err() {
                    match tok.as_str() {
                        "BDC" => {
                            let tag = names.last().ok_or("no property")?;
                            group = Some(props.iter().find(|(k, _)| k == tag).map(|(_, n)| n.clone()).ok_or("not an OCG")?);
                        }
                        "EMC" => group = None,
                        "m" | "l" | "c" => {
                            if let Some(g) = &group {
                                match out.iter_mut().find(|(n, _)| n == g) {
                                    Some((_, k)) => *k += 1,
                                    None => out.push((g.clone(), 1)),
                                }
                            }
                        }
                        _ => {}
                    }
                    names.clear();
                }
            }
        }
        Ok(out)
    }

    #[test]
    fn phase_9_gate_assembly_sheet_plots_and_wallfit_clears_the_conflict() -> R {
        let (mut s, plan) = room_with("exterior_face", true, 210.0)?;
        crate::maintain::install(&mut s);
        let conflicts = |s: &mut Session| -> T<usize> {
            Ok(s.execute("wallconflicts", &json!({ "viewId": plan.0 }))?["conflicts"].as_array().ok_or("no list")?.len())
        };
        assert!(conflicts(&mut s)? >= 1, "a 210 mm wall against a 200 mm detail");
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        s.execute("newsheet", &json!({ "name": "A501", "assemblyIds": [plan.0] }))?;
        // The assembly block's lines keep their own layers, so they plot in the bands' groups;
        // the sheet's viewport freezes every view block, so only the assembly can supply them.
        // Four runs: one CLAD and two GYP lines each (a move and a line apiece).
        let bands = |d: &Drawing| -> T<(usize, usize)> {
            let g = plotted_groups(d, "A501")?;
            let n = |name: &str| g.iter().find(|(n, _)| n == name).map_or(0, |(_, k)| *k);
            Ok((n("CLAD"), n("GYP")))
        };
        assert_eq!(bands(s.doc()?)?, (8, 16));
        // The block's lines, and what the plan derives now: the observer keeps them equal.
        let block = |s: &Session| -> T<Vec<[f64; 4]>> {
            let b = s.doc()?.block(&assembly_block_name(plan)).ok_or("no assembly block")?;
            let mut v: Vec<[f64; 4]> = b
                .entities
                .iter()
                .filter_map(|e| match &e.kind {
                    EntityKind::Line(l) => Some([l.a.x, l.a.y, l.b.x, l.b.y]),
                    _ => None,
                })
                .collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            Ok(v)
        };
        let derived = |s: &Session| -> T<Vec<[f64; 4]>> {
            let runs = crate::assembly::assembly_lines(s.doc()?, plan)?;
            let mut v: Vec<[f64; 4]> = runs.iter().flat_map(|r| r.lines.iter().map(|l| [l.from.x, l.from.y, l.to.x, l.to.y])).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            Ok(v)
        };
        assert_eq!(block(&s)?, derived(&s)?);
        let before = block(&s)?;

        let n = undo_len(&s)?;
        s.execute("wallfit", &json!({ "viewId": plan.0 }))?;
        assert_eq!(undo_len(&s)?, n + 1, "WALLFIT is one undo step");
        assert_eq!(conflicts(&mut s)?, 0);
        assert_eq!(block(&s)?, derived(&s)?, "the assembly block follows the fitted plan");
        assert_ne!(block(&s)?, before, "the bands now span the fitted runs");
        assert_eq!(bands(s.doc()?)?, (8, 16));

        s.execute("undo", &json!({}))?;
        assert!(conflicts(&mut s)? >= 1, "undo brings the conflict back");
        assert_eq!(block(&s)?, before);
        Ok(())
    }
}

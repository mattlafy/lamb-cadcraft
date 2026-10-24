//! Sheets (`cadcraft-integration.md` §13 phase 2): a paper layout with one viewport per LAMB
//! view. Each viewport freezes every **other** view's layer, so CADCraft hides those views'
//! whole canonical inserts there, while model space keeps showing all views overlaid. The
//! viewport records which view it shows in its `EntityData.view_id`.
//!
//! A plan's assembly plan (§10, [`crate::assembly`]) is a canonical model-space insert on its own
//! layer [`assembly_block_name`]`(plan)`. Every viewport freezes every assembly layer except its
//! own; an assembly viewport (requested by plan id) shows only that plan's assembly block, is laid
//! after the views, and has no `view_id`.
//!
//! Arrangement: a grid over the layout's printable area (columns = ceil(sqrt(n)), filled row by
//! row from the top left in the given order), each viewport its cell shrunk by 5 % on every side
//! and zoomed to its own view block's extents. No shared plot scale yet.

use std::fmt;

use cadcraft_doc::{Common, Drawing, EntityKind, Handle, Layout, PageSetup, Space, Viewport};
use cadcraft_geom::{Bounds2, Vec2, Vec3};
use lamb_core::model::Id;

use lamb_core::model::AnyView;

use crate::schema::{EntityData, SchemaError, assembly_block_name, read_document, read_entity_data, set_entity_data, view_block_name};

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

/// Model height shown by a viewport onto an empty view.
const EMPTY_VIEW_HEIGHT: f64 = 10_000.0;

fn zoom(d: &Drawing, block: &str, vp_width: f64, vp_height: f64) -> (Vec2, f64) {
    let ext = d.extents(&Space::Block(block.to_owned()));
    if ext.is_empty() {
        return (Vec2::new(0.0, 0.0), EMPTY_VIEW_HEIGHT);
    }
    let h = ext.height().max(ext.width() * vp_height / vp_width) * 1.1;
    (ext.center(), if h.is_finite() && h > 0.0 { h } else { EMPTY_VIEW_HEIGHT })
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
/// layers are kept) and zooms it to the view block's extents. `None` clears `view_id` and thaws
/// every LAMB view layer. Refuses a handle that is not a paper-space viewport, a layout's overall
/// viewport (id 1), or an unknown view; nothing is changed on error.
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
    set_entity_data(&mut common, &data)?;
    match view {
        Some(v) => {
            let block = view_block_name(v);
            let mut frozen: Vec<String> =
                port.frozen_layers.iter().filter(|l| !is_view_layer(l) && !assemblies.iter().any(|a| a.eq_ignore_ascii_case(l))).cloned().collect();
            frozen.extend(frozen_except(&view_layers, &assemblies, &block));
            frozen.sort();
            port.frozen_layers = frozen;
            (port.view_center, port.view_height) = zoom(d, &block, port.width, port.height);
        }
        None => port.frozen_layers.retain(|l| !is_view_layer(l)),
    }
    d.modify_entity(vp, |e| {
        e.common = common;
        e.kind = EntityKind::Viewport(port);
    })
    .map_err(|e| SheetError::Schema(SchemaError::Malformed { what: "viewport".into(), message: e.to_string() }))
}

/// Creates layout `name` with one viewport per view in `views`, then one per plan in `assemblies`
/// showing that plan's assembly block (every view in document order when both are empty). Each
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
        if !doc.views.iter().any(|v| v.id() == *plan && matches!(v, AnyView::Plan(_))) {
            return Err(SheetError::NoSuchView(*plan));
        }
        if d.block(&assembly_block_name(*plan)).is_none() {
            return Err(SheetError::NoAssembly(*plan));
        }
    }
    let view_layers: Vec<String> = all.iter().map(|v| view_block_name(*v)).collect();
    let assembly_layers = assembly_layers(d);
    // (block shown, EntityData) per viewport, built before anything changes.
    let slots: Vec<(String, EntityData)> = chosen
        .iter()
        .map(|v| (view_block_name(*v), EntityData { view_id: Some(*v), ..EntityData::default() }))
        .chain(assemblies.iter().map(|p| (assembly_block_name(*p), EntityData::default())))
        .collect();
    let mut commons = Vec::with_capacity(slots.len());
    for (_, dt) in &slots {
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

    let n = slots.len();
    let cols = (n as f64).sqrt().ceil().max(1.0) as usize;
    let rows = n.div_ceil(cols);
    let cell = Vec2::new(area.width() / cols as f64, area.height() / rows as f64);
    let (w, h) = (cell.x * 0.9, cell.y * 0.9);
    let space = Space::Paper(name.to_owned());
    let mut handles = Vec::with_capacity(n);
    for (i, ((block, _), common)) in slots.iter().zip(commons).enumerate() {
        let (r, c) = (i / cols, i % cols);
        let center = Vec3::new(area.min.x + cell.x * (c as f64 + 0.5), area.max.y - cell.y * (r as f64 + 0.5), 0.0);
        let (view_center, view_height) = zoom(d, block, w, h);
        let frozen_layers = frozen_except(&view_layers, &assembly_layers, block);
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

/// Every view's sheet reference at once: `(view, n, layout)` for each view some single-view
/// viewport shows, `n` that viewport's 1-based position (by viewport `id`) among its layout's
/// single-view viewports, the first layout by name winning. `view_blocks` is every LAMB view with
/// its block name. A viewport shows exactly one view when exactly one view block is absent from its
/// `frozen_layers` (how [`create_sheet`] builds them); assembly viewports freeze every view and
/// take no number.
pub fn sheet_refs(d: &Drawing, view_blocks: &[(Id, String)]) -> Vec<(Id, usize, String)> {
    let mut layouts: Vec<&Layout> = d.layouts.iter().collect();
    layouts.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out: Vec<(Id, usize, String)> = Vec::new();
    for l in layouts {
        let mut vps: Vec<&Viewport> = l
            .entities
            .iter()
            .filter_map(|e| match &e.kind {
                EntityKind::Viewport(vp) => Some(vp),
                _ => None,
            })
            .collect();
        vps.sort_by_key(|vp| vp.id);
        let mut n = 0;
        for vp in vps {
            let mut shown = view_blocks.iter().filter(|(_, b)| !vp.frozen_layers.iter().any(|f| f.eq_ignore_ascii_case(b)));
            let (Some((view, _)), None) = (shown.next(), shown.next()) else { continue };
            n += 1;
            if !out.iter().any(|(v, _, _)| v == view) {
                out.push((*view, n, l.name.clone()));
            }
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

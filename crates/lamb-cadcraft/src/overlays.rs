//! LAMB's overlay and snap providers (`cadcraft-integration.md` §7): while a space is a LAMB view's
//! block (`Space::Block("LAMB_VIEW_<id>")`), [`lamb_overlay`] draws that view's intersection markers
//! and cut lines over the drawing, and [`lamb_snaps`] offers every marker centre as a snap point.
//! Block coordinates are the view's local coordinates, so lamb-core's output is drawn as-is. In a
//! view's assembly block (`LAMB_ASSEMBLY_<id>`) it draws the assembly-space ghost instead
//! ([`crate::asmview::draw`]: what the assembly plan does not lay, in grey). Any other space, and
//! any error reading the LAMB slots or deriving, gives nothing.

use std::sync::OnceLock;

use cadcraft_color::Rgb;
use cadcraft_doc::{Drawing, Handle, Space};
use cadcraft_engine::snap::{SnapHit, register_snap_provider};
use cadcraft_geom::Vec2;
use cadcraft_render::{DPrim, DisplayList, Kind, Options, register_overlay_provider};
use lamb_core::derivation::linking::find_link_for_marker;
use lamb_core::derivation::markers::{Marker, MarkerShape, compute_cut_lines_for_view, compute_markers_for_view, compute_plan_cut_lines_for_view};
use lamb_core::model::Id;

use crate::project::DrawingProject;
use crate::schema::view_block_name;

pub(crate) mod halo;
pub(crate) mod underlay;

/// Marker radius as a fraction of the visible height: `view_height / MARKER_VIEW_FRACTION`.
const MARKER_VIEW_FRACTION: f64 = 150.0;
/// Marker radius (mm) when the build has no visible height.
const MARKER_FALLBACK_RADIUS_MM: f64 = 100.0;
/// Vertices in a marker outline.
const MARKER_SEGMENTS: usize = 32;
const MARKER_COLOR: Rgb = Rgb(0, 190, 255);
const MARKER_ASSUMED_Z_COLOR: Rgb = Rgb(120, 140, 160);
const CUT_LINE_COLOR: Rgb = Rgb(255, 170, 0);
/// A picked view's cut lines (TS `SELECTED_OVERLAY_COLOR`, `#4da3ff`).
const SELECTED_OVERLAY_COLOR: Rgb = Rgb(0x4d, 0xa3, 0xff);
const SNAP_NAME: &str = "LAMB marker";

/// The view id a space's block name stands for (`view_block_name(id)`), without checking the
/// document holds that view.
pub(crate) fn view_id(space: &Space) -> Option<Id> {
    let Space::Block(name) = space else { return None };
    let id = Id(name.strip_prefix("LAMB_VIEW_")?.parse().ok()?);
    (view_block_name(id) == *name).then_some(id)
}

/// The LAMB view a space edits: `Space::Block(view_block_name(id))` for a view in the document.
pub(crate) fn view_of<'a>(d: &'a Drawing, space: &Space) -> Option<(DrawingProject<'a>, Id)> {
    let id = view_id(space)?;
    let dp = DrawingProject::new(d).ok()?;
    dp.document().views.iter().any(|v| v.id() == id).then_some((dp, id))
}

fn marker_radius(opts: &Options) -> f64 {
    marker_radius_at(opts.view_height)
}

/// Marker radius at a visible height of `view_height` (0: none known).
pub(crate) fn marker_radius_at(view_height: f64) -> f64 {
    if view_height > 0.0 { view_height / MARKER_VIEW_FRACTION } else { MARKER_FALLBACK_RADIUS_MM }
}

/// Semi-axes (along local X, along local Y) of a marker of radius `r`.
pub(crate) fn semi_axes(shape: MarkerShape, r: f64) -> (f64, f64) {
    match shape {
        MarkerShape::Circle => (r, r),
        MarkerShape::Ellipse2 => (2.0 * r, r),
        MarkerShape::Ellipse5 => (5.0 * r, r),
    }
}

fn outline(m: &Marker, r: f64) -> Vec<Vec2> {
    let (a, b) = semi_axes(m.shape, r);
    (0..MARKER_SEGMENTS)
        .map(|i| {
            let t = std::f64::consts::TAU * i as f64 / MARKER_SEGMENTS as f64;
            Vec2::new(m.position.x + a * t.cos(), m.position.y + b * t.sin())
        })
        .collect()
}

pub(crate) fn push(list: &mut DisplayList, handle: Handle, color: Rgb, kind: Kind, pts: &[Vec2]) {
    let into_tris = matches!(kind, Kind::Tris | Kind::Mask);
    let buf = if into_tris { &mut list.tris } else { &mut list.verts };
    let start = buf.len() as u32;
    buf.extend_from_slice(pts);
    list.prims.push(DPrim { handle, color, aci7: false, lw: 0.0, transparency: 0, kind, start, len: pts.len() as u32 });
}

/// The overlay provider: the edited view's underlay ([`underlay`], drawn first, pale), its halos
/// ([`halo`]: RESOLVE's attention, unaccepted AI lines' glow), a detail's reference line
/// ([`crate::detailref`], magenta), a detail's repeat ghosts ([`crate::repeats::draw`], one pale copy
/// of each repeat's members at its pitch), an assembly block's ghost ([`crate::asmview::draw`]),
/// then its markers and cut lines, in
/// its block's coordinates.
pub fn lamb_overlay(d: &Drawing, space: &Space, opts: &Options) -> DisplayList {
    let mut list = underlay::draw(d, space, opts);
    append_list(&mut list, halo::draw(d, space, opts));
    append_list(&mut list, crate::detailref::draw(d, space, opts));
    append_list(&mut list, crate::repeats::draw(d, space, opts));
    append_list(&mut list, crate::asmview::draw(d, space, opts));
    let Some((dp, id)) = view_of(d, space) else { return list };
    let Ok(markers) = compute_markers_for_view(&dp, id) else { return list };
    let Ok(cuts) = compute_cut_lines_for_view(&dp, id) else { return list };
    let plan_cuts = compute_plan_cut_lines_for_view(&dp, id);

    let picked = crate::viewpick::highlighted();
    let cut_color = |v: Id| if picked == Some(v) { SELECTED_OVERLAY_COLOR } else { CUT_LINE_COLOR };
    let r = marker_radius(opts);
    // The travel ring sits 4 px outside the marker (TS `drawTravelRing`); plots have no pixels.
    let r_ring = if opts.min_dash > 0.0 { r + 2.0 * opts.min_dash } else { r * 1.3 };
    let travelling = crate::viewpick::travelling();
    let travels = |m: &Marker| !travelling.is_empty() && find_link_for_marker(&dp, m, id).is_some_and(|l| travelling.contains(&l.id));
    for m in &markers {
        let handle = Handle(m.source_entity_id.0);
        let color = if m.assumed_z { MARKER_ASSUMED_Z_COLOR } else { MARKER_COLOR };
        let mut ring = outline(m, r);
        for p in &ring {
            list.bounds.add(*p);
        }
        if m.filled {
            let c = Vec2::new(m.position.x, m.position.y);
            let fan: Vec<Vec2> = (0..ring.len()).flat_map(|i| [c, ring[i], ring[(i + 1) % ring.len()]]).collect();
            push(&mut list, handle, color, Kind::Tris, &fan);
        }
        if let Some(first) = ring.first().copied() {
            ring.push(first);
        }
        push(&mut list, handle, color, Kind::Polyline, &ring);
        if travels(m) {
            let mut tr = outline(m, r_ring);
            if let Some(first) = tr.first().copied() {
                tr.push(first);
            }
            for p in &tr {
                list.bounds.add(*p);
            }
            push(&mut list, handle, color, Kind::Polyline, &tr);
        }
    }
    for c in &cuts {
        let pts = [Vec2::new(c.from.x, c.from.y), Vec2::new(c.to.x, c.to.y)];
        list.bounds.add(pts[0]);
        list.bounds.add(pts[1]);
        push(&mut list, Handle(c.source_view_id.0), cut_color(c.source_view_id), Kind::Polyline, &pts);
    }
    for c in &plan_cuts {
        let base = Vec2::new(0.0, c.local_y);
        list.bounds.add(base);
        push(&mut list, Handle(c.source_view_id.0), cut_color(c.source_view_id), Kind::Infinite { ray: false }, &[base, Vec2::new(1.0, 0.0)]);
    }
    list
}

/// Append `more` to `list`, rebasing its prims onto `list`'s buffers.
fn append_list(list: &mut DisplayList, more: DisplayList) {
    let (v0, t0) = (list.verts.len() as u32, list.tris.len() as u32);
    for p in &more.prims {
        let base = if matches!(p.kind, Kind::Tris | Kind::Mask) { t0 } else { v0 };
        list.prims.push(DPrim { start: p.start + base, ..*p });
    }
    list.verts.extend_from_slice(&more.verts);
    list.tris.extend_from_slice(&more.tris);
    list.bounds = list.bounds.union(&more.bounds);
}

/// The snap provider: every marker centre of the edited view within `aperture` of the cursor,
/// plus the underlay's snap ([`underlay`]). Nothing inside the underlay's own snap call.
pub fn lamb_snaps(d: &Drawing, space: &Space, cursor: Vec2, aperture: f64) -> Vec<SnapHit> {
    if underlay::nested() {
        return Vec::new();
    }
    let mut hits = underlay::snaps(d, space, cursor, aperture);
    let Some((dp, id)) = view_of(d, space) else { return hits };
    let Ok(markers) = compute_markers_for_view(&dp, id) else { return hits };
    hits.extend(markers.iter().map(|m| Vec2::new(m.position.x, m.position.y)).filter(|p| p.dist(cursor) <= aperture).map(|point| SnapHit {
        point,
        mode: 0,
        name: SNAP_NAME,
        viewport: None,
        deferred: None,
    }));
    hits
}

/// Register [`lamb_overlay`], [`lamb_snaps`], the pane relations ([`crate::panes`]) and the
/// virtual corner hook ([`crate::fillet`]) with CADCraft's process-global registries. Only the first call registers; later calls return its
/// result.
pub fn register_providers() -> Result<(), &'static str> {
    static DONE: OnceLock<Result<(), &'static str>> = OnceLock::new();
    *DONE.get_or_init(|| {
        register_overlay_provider(lamb_overlay)?;
        register_snap_provider(lamb_snaps)?;
        crate::panes::register()?;
        crate::viewpick::register()?;
        crate::fillet::register()?;
        crate::rounding::register()
    })
}

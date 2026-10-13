//! Pane relations (LAMB T323): what one VPORTS tile's LAMB view means in another's — the rotation
//! a plan pane takes so a section's cut line reads horizontal, where a point in one pane lands in
//! another (the crosshair), and where a locked pane re-centres when another pans. Registered into
//! the engine's slot ([`cadcraft_engine::panes::register_pane_relations`]) by
//! [`crate::register_providers`]; the canvas reads them through `Session::tile_rotations` /
//! `Session::tile_crosshairs`. Ports of the TS app's `computeMainRotation` and
//! `projectReferencePointToMain` (`referenceAlignment.ts`).
//!
//! A space maps to a view as the overlays do ([`crate::overlays::view_of`]): Model space, or a
//! block that is not a LAMB view, relates to nothing.

use cadcraft_doc::{Drawing, Space};
use cadcraft_engine::panes::{PaneMap, PaneRelations, register_pane_relations};
use cadcraft_geom::Vec2;
use lamb_core::derivation::alignment::{compute_alignment_basis, plan_point_to_uv};
use lamb_core::derivation::projection::{Uz, plan_global_to_local_xy, vertical_local_to_uz, vertical_uz_to_local};
use lamb_core::derivation::read::{ProjectRead, ViewKind, ViewRef};
use lamb_core::derivation::underlay::map_plan_vertical_center;
use lamb_core::geometry::Point2;
use lamb_core::geometry::intersect::infinite_line_intersect;
use lamb_core::geometry::point::{add2, normalize2, scale2, sub2};
use lamb_core::geometry::transform2d::world_to_local_dir2;

use crate::overlays::{view_id, view_of};
use crate::project::DrawingProject;

fn pt(v: Vec2) -> Point2 {
    Point2 { x: v.x, y: v.y }
}

fn vec(p: Point2) -> Vec2 {
    Vec2::new(p.x, p.y)
}

/// Both spaces' views, read from one `DrawingProject`.
fn with_views<T>(d: &Drawing, a: &Space, b: &Space, f: impl FnOnce(&DrawingProject<'_>, &ViewRef<'_>, &ViewRef<'_>) -> Option<T>) -> Option<T> {
    let (dp, ida) = view_of(d, a)?;
    let idb = view_id(b)?;
    let (va, vb) = (dp.view(ida)?, dp.view(idb)?);
    f(&dp, &va, &vb)
}

/// `vertical` is hosted on `plan`.
fn hosted_on(vertical: &ViewRef<'_>, plan: &ViewRef<'_>) -> bool {
    vertical.kind == ViewKind::Vertical && plan.kind == ViewKind::Plan && vertical.host_view_id() == Some(plan.id)
}

/// The rotation (radians) of a pane showing placed plan `pane` so the cut line of `other`, a
/// vertical view hosted on it, reads horizontal: the cut direction's angle in plan-local.
pub fn rotation(d: &Drawing, pane: &Space, other: &Space) -> Option<f64> {
    with_views(d, pane, other, |_, plan, sec| {
        if !hosted_on(sec, plan) {
            return None;
        }
        let t = plan.transform()?;
        let basis = compute_alignment_basis(sec, plan).ok()?;
        let local = world_to_local_dir2(t, basis.c_hat);
        Some(local.y.atan2(local.x))
    })
}

/// The unit direction in `view`'s local space of a step `(du, dz)` in (u, z) from `at`.
fn uz_dir(view: &ViewRef<'_>, at: Uz, du: f64, dz: f64) -> Vec2 {
    let a = vertical_uz_to_local(view, at);
    let b = vertical_uz_to_local(view, Uz { u: at.u + du, z: at.z + dz });
    vec(normalize2(sub2(b, a)))
}

/// `p` in `from`'s space, in `to`'s: vertical -> its host plan is a point on the cut line; plan ->
/// a vertical hosted on it is the vertical line at that `u` (a plan point has no z); vertical <->
/// vertical on the same host is the point where the cut lines cross, at `p`'s z (a horizontal line
/// at that z when they are parallel). Anything else `None`.
pub fn map(d: &Drawing, from: &Space, to: &Space, p: Vec2) -> Option<PaneMap> {
    with_views(d, from, to, |dp, f, t| {
        let local = pt(p);
        if hosted_on(f, t) {
            let basis = compute_alignment_basis(f, t).ok()?;
            let u = vertical_local_to_uz(f, local).u;
            let g = add2(basis.origin_xy, scale2(basis.c_hat, u));
            return Some(PaneMap::Point(vec(plan_global_to_local_xy(t, g).ok()?)));
        }
        if hosted_on(t, f) {
            let basis = compute_alignment_basis(t, f).ok()?;
            let u = plan_point_to_uv(f, &basis, local).ok()?.x;
            let at = Uz { u, z: 0.0 };
            return Some(PaneMap::Line { through: vec(vertical_uz_to_local(t, at)), dir: uz_dir(t, at, 0.0, 1.0) });
        }
        if f.kind == ViewKind::Vertical && t.kind == ViewKind::Vertical && f.id != t.id {
            let host_id = f.host_view_id()?;
            if t.host_view_id() != Some(host_id) {
                return None;
            }
            let host = dp.view(host_id)?;
            let bt = compute_alignment_basis(t, &host).ok()?;
            let bf = compute_alignment_basis(f, &host).ok()?;
            let z = vertical_local_to_uz(f, local).z;
            let hit = infinite_line_intersect(bt.origin_xy, add2(bt.origin_xy, bt.c_hat), bf.origin_xy, add2(bf.origin_xy, bf.c_hat));
            return Some(match hit {
                Some(hit) => PaneMap::Point(vec(vertical_uz_to_local(t, Uz { u: hit.t_a, z }))),
                None => {
                    let at = Uz { u: 0.0, z };
                    PaneMap::Line { through: vec(vertical_uz_to_local(t, at)), dir: uz_dir(t, at, 1.0, 0.0) }
                }
            });
        }
        None
    })
}

/// Camera lock: the centre `to`'s pane takes when `from`'s centres on `p`, given `to`'s previous
/// centre `to_prev` (lamb-core `map_plan_vertical_center`; plan <-> plan by global XY too).
pub fn centre(d: &Drawing, from: &Space, p: Vec2, to: &Space, to_prev: Vec2) -> Option<Vec2> {
    with_views(d, from, to, |dp, f, t| map_plan_vertical_center(dp, f, pt(p), t, pt(to_prev)).ok().flatten().map(vec))
}

/// Register [`rotation`], [`map`] and [`centre`] in the engine's pane-relations slot.
pub fn register() -> Result<(), &'static str> {
    register_pane_relations(PaneRelations { rotation, map, centre })
}

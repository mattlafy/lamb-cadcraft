//! Clicking a section's or elevation's cut line in a placed plan picks that view (TS
//! `Canvas2DView` idle click, `findNearestOverlay`): the engine's idle-pick hook
//! ([`cadcraft_engine::idle_pick`]) asks [`pick_cut_line`] only when the click hit no entity; the
//! nearest cut line within the pick aperture wins, the entity selection is cleared, and the
//! command line says `Selected <name>`. [`picked_view`] reads the pick back (the view Properties
//! form, T365).
//!
//! The pick is drawn too (TS T91): the engine's picked-object observer mirrors it into
//! [`highlighted`], which the overlay provider reads to stroke that view's cut lines in the
//! selected colour. Accepted limitation: the mirror is process-global, so with two drawings open
//! the other drawing's cut line with the same view id would also highlight.
//!
//! With entities selected, an empty click on a filled marker toggles that marker's link in or out
//! of the selection's travel set instead (TS T110 `toggleTravelLink`); the selection stays. The
//! engine's travel observer mirrors the set into [`travelling`], which the overlay provider reads
//! to ring each travelling link's markers — process-global in the same way as [`highlighted`].

use std::sync::{Mutex, PoisonError};

use cadcraft_doc::Space;
use cadcraft_engine::Session;
use cadcraft_engine::idle_pick::IdlePicked;
use cadcraft_geom::Vec2;
use lamb_core::derivation::linking::find_link_for_marker;
use lamb_core::derivation::markers::{Marker, compute_cut_lines_for_view, compute_markers_for_view};
use lamb_core::geometry::point::Point2;
use lamb_core::model::Id;

use crate::overlays::{marker_radius_at, semi_axes, view_id, view_of};
use crate::schema::view_block_name;

/// Distance from `p` to segment `a`–`b` (TS `distToSegment`).
pub(crate) fn dist_to_segment(p: Vec2, a: Point2, b: Point2) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    let (qx, qy) = (a.x + t * dx, a.y + t * dy);
    ((p.x - qx).powi(2) + (p.y - qy).powi(2)).sqrt()
}

/// The idle-pick hook: in a placed plan, the cut line nearest `p` within `aperture` picks its
/// view — `Some(view_block_name(view))`, after echoing `Selected <name>`.
pub fn pick_cut_line(s: &mut Session, p: Vec2, aperture: f64) -> Option<String> {
    let space = s.space();
    let (id, name) = {
        let d = s.doc().ok()?;
        let (dp, plan) = view_of(d, &space)?;
        let cuts = compute_cut_lines_for_view(&dp, plan).ok()?;
        let best =
            cuts.iter().map(|c| (dist_to_segment(p, c.from, c.to), c)).filter(|(dist, _)| *dist <= aperture).min_by(|a, b| a.0.total_cmp(&b.0))?;
        (best.1.source_view_id, best.1.name.clone())
    };
    s.echo(format!("Selected {name}"));
    Some(view_block_name(id))
}

/// How far inside marker `m` drawn at radius `r` the point `p` is: 0 at its centre, 1 on its
/// outline (TS `findMarkerAt` hits within the drawn outline). Within `aperture` of the centre
/// always counts as 0..1.
fn marker_hit(m: &Marker, r: f64, p: Vec2, aperture: f64) -> Option<f64> {
    let (a, b) = semi_axes(m.shape, r);
    let (dx, dy) = (p.x - m.position.x, p.y - m.position.y);
    let k = ((dx / a).powi(2) + (dy / b).powi(2)).sqrt();
    let near = (dx * dx + dy * dy).sqrt() <= aperture;
    (k <= 1.0 || near).then_some(k.min(1.0))
}

/// With a selection, in any LAMB view: the link of the nearest filled marker under `p` (within its
/// drawn outline — a marker sits on its entity, so a hit within the pick aperture alone would
/// always be taken by the entity first), toggled in the travel set (echo `Link travels` /
/// `Link stays`). Hollow markers never toggle.
fn toggle_travel_link(s: &mut Session, p: Vec2, aperture: f64) -> bool {
    if s.selection().is_empty() {
        return false;
    }
    let space = s.space();
    let r = marker_radius_at(s.state().map(|st| st.view().height).unwrap_or(0.0));
    let link = {
        let Ok(d) = s.doc() else { return false };
        let Some((dp, view)) = view_of(d, &space) else { return false };
        let Ok(markers) = compute_markers_for_view(&dp, view) else { return false };
        markers
            .iter()
            .filter(|m| m.filled)
            .filter_map(|m| marker_hit(m, r, p, aperture).map(|k| (k, m)))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .and_then(|(_, m)| find_link_for_marker(&dp, m, view))
            .map(|l| l.id)
    };
    let Some(link) = link else { return false };
    let mut travel = s.travel();
    let msg = if let Some(i) = travel.iter().position(|t| *t == link.0) {
        travel.remove(i);
        "Link stays"
    } else {
        travel.push(link.0);
        "Link travels"
    };
    s.set_travel(travel);
    s.echo(msg.to_string());
    true
}

/// The engine's idle-pick hook: a marker click toggling a travelling link
/// ([`toggle_travel_link`]) first, then in an assembly space the line's source
/// ([`crate::asmview::pick_assembly`]), else [`pick_cut_line`].
pub fn idle_pick(s: &mut Session, p: Vec2, aperture: f64) -> Option<IdlePicked> {
    if toggle_travel_link(s, p, aperture) {
        return Some(IdlePicked::Consumed);
    }
    if crate::asmview::pick_assembly(s, p, aperture) {
        return Some(IdlePicked::Consumed);
    }
    pick_cut_line(s, p, aperture).map(IdlePicked::Pick)
}

/// The view an idle click picked, while it is still in the document.
pub fn picked_view(s: &Session) -> Option<Id> {
    let id = view_id(&Space::Block(s.picked()?))?;
    let d = s.doc().ok()?;
    view_of(d, &Space::Block(view_block_name(id))).map(|_| id)
}

/// The picked view, mirrored out of the engine for the overlay provider.
static PICKED: Mutex<Option<Id>> = Mutex::new(None);

/// The engine's picked-object observer: mirror the pick and redraw the overlays (the drawing's
/// revision does not move).
fn mirror_pick(v: Option<&(Space, String)>) {
    let id = v.and_then(|(_, key)| view_id(&Space::Block(key.clone())));
    *PICKED.lock().unwrap_or_else(PoisonError::into_inner) = id;
    cadcraft_render::bump_overlays();
}

/// The view whose cut lines draw highlighted: the one an idle click last picked.
pub(crate) fn highlighted() -> Option<Id> {
    *PICKED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The selection's travel set, mirrored out of the engine for the overlay provider.
static TRAVEL: Mutex<Vec<Id>> = Mutex::new(Vec::new());

/// The engine's travel observer: mirror the set and redraw the overlays.
fn mirror_travel(ids: &[u64]) {
    *TRAVEL.lock().unwrap_or_else(PoisonError::into_inner) = ids.iter().copied().map(Id).collect();
    cadcraft_render::bump_overlays();
}

/// The links whose markers draw a travel ring.
pub(crate) fn travelling() -> Vec<Id> {
    TRAVEL.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Register [`idle_pick`] as the engine's idle-pick hook, [`mirror_pick`] as its picked-object
/// observer and [`mirror_travel`] as its travel observer.
pub(crate) fn register() -> Result<(), &'static str> {
    cadcraft_engine::idle_pick::register_idle_pick(idle_pick)?;
    cadcraft_engine::idle_pick::register_picked_observer(mirror_pick)?;
    cadcraft_engine::travel::register_travel_observer(mirror_travel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_to_a_segment_clamps_to_its_ends() {
        let (a, b) = (Point2 { x: 0.0, y: 0.0 }, Point2 { x: 10.0, y: 0.0 });
        assert!((dist_to_segment(Vec2::new(5.0, 3.0), a, b) - 3.0).abs() < 1e-12);
        assert!((dist_to_segment(Vec2::new(13.0, 4.0), a, b) - 5.0).abs() < 1e-12);
        assert!((dist_to_segment(Vec2::new(-3.0, -4.0), a, a) - 5.0).abs() < 1e-12);
    }
}

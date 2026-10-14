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

use std::sync::{Mutex, PoisonError};

use cadcraft_doc::Space;
use cadcraft_engine::Session;
use cadcraft_geom::Vec2;
use lamb_core::derivation::markers::compute_cut_lines_for_view;
use lamb_core::geometry::point::Point2;
use lamb_core::model::Id;

use crate::overlays::{view_id, view_of};
use crate::schema::view_block_name;

/// Distance from `p` to segment `a`–`b` (TS `distToSegment`).
fn dist_to_segment(p: Vec2, a: Point2, b: Point2) -> f64 {
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

/// Register [`pick_cut_line`] as the engine's idle-pick hook and [`mirror_pick`] as its
/// picked-object observer.
pub(crate) fn register() -> Result<(), &'static str> {
    cadcraft_engine::idle_pick::register_idle_pick(pick_cut_line)?;
    cadcraft_engine::idle_pick::register_picked_observer(mirror_pick)
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

//! Panes (LAMB fork patch 9, not to be submitted): each tiled model viewport (VPORTS,
//! [`crate::ModelTiles`]) carries its own space — Model or a block (a LAMB view) — and making a
//! tile current makes its space the edit space.
//!
//! The current tile's space is always `DocState::space`; [`Panes::spaces`] only matters for the
//! other tiles. It is session state, never saved with the drawing. When the tiling changes
//! (VPORTS, or undo of it), every tile but the current one goes back to Model.
//!
//! Relations between two tiles' spaces — the rotation a tile takes to line up with another, where
//! a point in one lands in another (the crosshair), where a locked tile re-centres — are geometry
//! the engine doesn't know: an adapter registers them once ([`register_pane_relations`]) and the
//! canvas reads them through [`Session::tile_rotations`] / [`Session::tile_crosshairs_from`]
//! (from whichever tile the pointer is over; [`Session::tile_crosshairs`] from the current one).
//!
//! Camera lock ([`Panes::locked`], off by default, toggled by LAMB's `panelock`): every view change
//! of the current tile ([`after_view`], from `DocState::set_view`/`set_view_quiet`) re-centres each
//! related tile through [`PaneRelations::centre`] at the same screen scale; unrelated tiles stay.

use std::sync::OnceLock;

use cadcraft_doc::{Drawing, Space};
use cadcraft_geom::Vec2;

use crate::{DocState, Session};

/// The space shown in each tile, for the tiling in `rects`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Panes {
    pub rects: Vec<(Vec2, Vec2)>,
    pub spaces: Vec<Space>,
    /// Camera lock: the other tiles follow the current one's centre and scale. Kept across
    /// re-tiling.
    pub locked: bool,
    /// ORTHO and direct distance entry keep to world X/Y instead of the current tile's rotation
    /// ([`Session::ortho_rotation`]; LAMB's `ucsalign`, off = follow the pane, by default). Kept
    /// across re-tiling, never saved.
    pub world_ortho: bool,
}

/// Where a point in one tile's space lands in another's: a point, or (when the other space has an
/// axis the first can't fix, e.g. a plan point in a section has no height) a line through `through`
/// along unit `dir`, both in the other space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PaneMap {
    Point(Vec2),
    Line { through: Vec2, dir: Vec2 },
}

/// The relations between two spaces, filled by an adapter (LAMB's `lamb-cadcraft`).
#[derive(Clone, Copy, Debug)]
pub struct PaneRelations {
    /// The rotation (radians) a tile showing the first space takes so the second reads aligned
    /// with it; `None` when the two are unrelated.
    pub rotation: fn(&Drawing, &Space, &Space) -> Option<f64>,
    /// A point in the first space mapped into the second; `None` when unrelated.
    pub map: fn(&Drawing, &Space, &Space, Vec2) -> Option<PaneMap>,
    /// Camera lock: `(from, p, to, to_prev)` — the centre the `to` tile takes when the `from` tile
    /// centres on `p`, given `to`'s previous centre; `None` when unrelated.
    pub centre: fn(&Drawing, &Space, Vec2, &Space, Vec2) -> Option<Vec2>,
}

static RELATIONS: OnceLock<PaneRelations> = OnceLock::new();

/// Register the pane relations. Once per process; a second call is an error.
pub fn register_pane_relations(r: PaneRelations) -> Result<(), &'static str> {
    RELATIONS.set(r).map_err(|_| "pane relations already registered")
}

/// The registered pane relations, if any.
pub fn pane_relations() -> Option<&'static PaneRelations> {
    RELATIONS.get()
}

/// The spaces a tiled model area can edit: Model, or a block (a view) shown in it.
pub fn tiles_space(sp: &Space) -> bool {
    matches!(sp, Space::Model | Space::Block(_))
}

/// `sp` if it still exists in the drawing, else Model.
fn existing(st: &DocState, sp: Option<&Space>) -> Space {
    match sp {
        Some(Space::Block(name)) if st.doc.block(name).is_some() => Space::Block(name.clone()),
        _ => Space::Model,
    }
}

/// Hook in `Session::activate_tile`, after `st.tiles` holds the new current tile and before its
/// view is set: tile `old` keeps the edit space, tile `new`'s space becomes it.
pub fn on_activate(st: &mut DocState, old: usize, new: usize) {
    if st.panes.rects != st.tiles.rects {
        st.panes = Panes {
            rects: st.tiles.rects.clone(),
            spaces: vec![Space::Model; st.tiles.rects.len()],
            locked: st.panes.locked,
            world_ortho: st.panes.world_ortho,
        };
    }
    let here = st.space.clone();
    if let Some(slot) = st.panes.spaces.get_mut(old) {
        *slot = here;
    }
    st.space = existing(st, st.panes.spaces.get(new));
    st.mspace = None;
}

/// Hook in `DocState::set_view` / `set_view_quiet`, after the view is stored: with the camera
/// lock on, each other tile related to the current one's space takes the centre
/// [`PaneRelations::centre`] gives it and the current view's px per mm (heights scaled by the
/// tiles' fraction heights). Unrelated tiles, and everything with no relations registered, stay.
pub fn after_view(st: &mut DocState) {
    if !st.panes.locked || st.mspace.is_some() || !tiles_space(&st.space) {
        return;
    }
    let Some(rel) = pane_relations() else { return };
    let (tiles, active) = crate::cmd::view::vports::live(st, (1.0, 1.0));
    if tiles.len() < 2 {
        return;
    }
    let rects: Vec<(Vec2, Vec2)> = tiles.iter().map(|t| (t.min, t.max)).collect();
    if st.tiles.rects != rects || st.tiles.views.len() != tiles.len() {
        st.tiles = crate::ModelTiles {
            rects: rects.clone(),
            views: tiles
                .iter()
                .map(|t| crate::View { center: t.center, height: t.height, twist: if t.twist.is_finite() { t.twist } else { 0.0 } })
                .collect(),
            active,
        };
    }
    let panes_synced = st.panes.rects == rects;
    let now = st.view();
    let here = st.space.clone();
    let fh = |i: usize| (rects[i].1.y - rects[i].0.y).max(1e-12);
    for i in (0..tiles.len()).filter(|&i| i != active) {
        let sp = if panes_synced { existing(st, st.panes.spaces.get(i)) } else { Space::Model };
        let prev = st.tiles.views[i];
        if let Some(c) = (rel.centre)(&st.doc, &here, now.center, &sp, prev.center) {
            let v = crate::View { center: c, height: now.height * fh(i) / fh(active), twist: prev.twist };
            if v.center.is_finite() && v.height.is_finite() && v.height > 1e-12 {
                st.tiles.views[i] = v;
            }
        }
    }
}

impl Session {
    /// Turn the camera lock on or off ([`Panes::locked`]); nothing without a drawing.
    pub fn set_panes_locked(&mut self, on: bool) {
        if let Ok(st) = self.state_mut() {
            st.panes.locked = on;
            if on {
                let v = st.view();
                st.set_view_quiet(v);
            }
        }
        self.touch();
    }

    /// Whether the camera lock is on.
    pub fn panes_locked(&self) -> bool {
        self.state().is_ok_and(|st| st.panes.locked)
    }

    /// ORTHO keeps to world X/Y ([`Panes::world_ortho`]) instead of the current tile's rotation;
    /// nothing without a drawing.
    pub fn set_world_ortho(&mut self, on: bool) {
        if let Ok(st) = self.state_mut() {
            st.panes.world_ortho = on;
        }
        self.touch();
    }

    /// Whether ORTHO keeps to world X/Y.
    pub fn world_ortho(&self) -> bool {
        self.state().is_ok_and(|st| st.panes.world_ortho)
    }

    /// The frame ORTHO and direct distance entry constrain to (radians): the current tile's
    /// [`Session::tile_rotations`] entry, else 0 (no tiles, no relations, a layout, or
    /// [`Panes::world_ortho`]).
    pub fn ortho_rotation(&self) -> f64 {
        if self.world_ortho() {
            return 0.0;
        }
        let Some(a) = self.model_tiles().iter().position(|t| t.active) else { return 0.0 };
        self.tile_rotations().get(a).copied().filter(|r| r.is_finite()).unwrap_or(0.0)
    }

    /// Each tile's space, one per [`Session::model_tiles`] entry and in its order: the current
    /// tile's is the edit space, the others' what they showed when they were last current (Model
    /// after a re-tiling, or when their block is gone).
    pub fn tile_spaces(&self) -> Vec<Space> {
        let tiles = self.model_tiles();
        let Ok(st) = self.state() else { return Vec::new() };
        let synced = st.panes.rects.len() == tiles.len() && st.panes.rects.iter().zip(&tiles).all(|(r, t)| *r == (t.min, t.max));
        tiles
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if t.active {
                    st.space.clone()
                } else if synced {
                    existing(st, st.panes.spaces.get(i))
                } else {
                    Space::Model
                }
            })
            .collect()
    }

    /// Each tile's rotation (radians), one per [`Session::model_tiles`] entry: aligned with the
    /// current tile's space when they are related, else with the first related tile in tile
    /// order, else 0. All 0 with no relations registered.
    pub fn tile_rotations(&self) -> Vec<f64> {
        let spaces = self.tile_spaces();
        let (Some(rel), Ok(d)) = (pane_relations(), self.doc()) else { return vec![0.0; spaces.len()] };
        let active = self.model_tiles().iter().position(|t| t.active);
        (0..spaces.len())
            .map(|i| {
                let first = active.filter(|&a| a != i).and_then(|a| (rel.rotation)(d, &spaces[i], &spaces[a]));
                first.or_else(|| (0..spaces.len()).filter(|&j| j != i).find_map(|j| (rel.rotation)(d, &spaces[i], &spaces[j]))).unwrap_or(0.0)
            })
            .collect()
    }

    /// Where `p` (in the current tile's space) lands in each tile, one per
    /// [`Session::model_tiles`] entry: `None` for the current tile itself and for any tile whose
    /// space is unrelated. All `None` with no relations registered.
    pub fn tile_crosshairs(&self, p: Vec2) -> Vec<Option<PaneMap>> {
        let Some(a) = self.model_tiles().iter().position(|t| t.active) else { return vec![None; self.tile_spaces().len()] };
        self.tile_crosshairs_from(a, p)
    }

    /// Where `p` (in tile `from`'s space) lands in each tile, one per [`Session::model_tiles`]
    /// entry: `None` for tile `from` itself and for any tile whose space is unrelated — the
    /// hover crosshair of whichever tile the pointer is over. All `None` with no relations
    /// registered or `from` out of range.
    pub fn tile_crosshairs_from(&self, from: usize, p: Vec2) -> Vec<Option<PaneMap>> {
        let spaces = self.tile_spaces();
        match self.doc() {
            Ok(d) => crosshairs(pane_relations(), d, &spaces, from, p),
            Err(_) => vec![None; spaces.len()],
        }
    }
}

/// [`Session::tile_crosshairs_from`] for the relations `rel`.
fn crosshairs(rel: Option<&PaneRelations>, d: &Drawing, spaces: &[Space], from: usize, p: Vec2) -> Vec<Option<PaneMap>> {
    let (Some(rel), Some(sf)) = (rel, spaces.get(from)) else { return vec![None; spaces.len()] };
    spaces.iter().enumerate().map(|(i, s)| if i == from { None } else { (rel.map)(d, sf, s, p) }).collect()
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::{Drawing, Space};
    use serde_json::json;

    use crate::*;

    fn b() -> Space {
        Space::Block("B".into())
    }

    /// Two tiles, then block `B` made from a line (its own undo step).
    fn two_tiles_with_block() -> Session {
        let mut s = Session::new();
        s.execute("-vports", &json!({"option": "2"})).unwrap();
        s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
        let l = s.doc().unwrap().model.iter().next().unwrap().handle;
        s.execute("block", &json!({ "name": "B", "base": [0, 0], "handles": [l.hex()], "keep": "convert" })).unwrap();
        assert_eq!(s.model_tiles().len(), 2);
        s
    }

    fn enter(s: &mut Session, sp: Space) {
        let st = s.state_mut().unwrap();
        st.space = sp;
        st.mspace = None;
    }

    fn active(s: &Session) -> usize {
        s.model_tiles().iter().position(|t| t.active).unwrap()
    }

    #[test]
    fn activating_a_tile_switches_the_edit_space() {
        let mut s = two_tiles_with_block();
        assert_eq!(active(&s), 0);
        enter(&mut s, b());
        let v0 = View { center: Vec2::new(5.0, 7.0), height: 3.0, twist: 0.0 };
        s.state_mut().unwrap().set_view_quiet(v0);
        assert_eq!(s.model_tiles().len(), 2, "tiles stay while editing a block");
        assert_eq!(s.tile_spaces(), vec![b(), Space::Model]);

        s.activate_tile(1).unwrap();
        assert_eq!(s.space(), Space::Model);
        assert_eq!(s.tile_spaces(), vec![b(), Space::Model]);
        assert_eq!(s.model_tiles()[0].view, v0, "tile 0 keeps its block view");

        s.activate_tile(0).unwrap();
        assert_eq!(s.space(), b());
        assert_eq!(s.state().unwrap().view(), v0, "back in B with tile 0's view");
        assert_eq!(s.tile_spaces(), vec![b(), Space::Model]);
    }

    #[test]
    fn retiling_resets_the_other_tiles_to_model() {
        let mut s = two_tiles_with_block();
        enter(&mut s, b());
        s.activate_tile(1).unwrap();
        enter(&mut s, b());
        assert_eq!(s.tile_spaces(), vec![b(), b()]);
        s.execute("-vports", &json!({"option": "3", "applyTo": "display"})).unwrap();
        assert_eq!(s.space(), b(), "the current tile keeps its space");
        assert_eq!(s.tile_spaces(), vec![b(), Space::Model, Space::Model]);
        // And activating another tile after the re-tiling finds Model there.
        s.activate_tile(2).unwrap();
        assert_eq!(s.space(), Space::Model);
        assert_eq!(s.tile_spaces(), vec![b(), Space::Model, Space::Model]);
    }

    #[test]
    fn vports_works_while_editing_a_block() {
        let mut s = two_tiles_with_block();
        enter(&mut s, b());
        s.execute("-vports", &json!({"option": "single"})).unwrap();
        assert_eq!(s.space(), b());
        assert!(s.model_tiles().is_empty());
        assert!(s.tile_spaces().is_empty());
    }

    #[test]
    fn unregistered_relations_are_neutral() {
        // The engine's own tests never register relations (only an adapter does).
        let mut s = two_tiles_with_block();
        enter(&mut s, b());
        assert!(super::pane_relations().is_none());
        assert_eq!(s.tile_rotations(), vec![0.0, 0.0]);
        assert_eq!(s.tile_crosshairs(Vec2::new(1.0, 2.0)), vec![None, None]);
    }

    #[test]
    fn crosshairs_map_from_any_tile() {
        // A local relation (the process-wide registry stays empty in the engine's tests): Model
        // → B is a point shifted by (1, 0); B → Model a vertical line; nothing else relates.
        fn map(_: &Drawing, a: &Space, b: &Space, p: Vec2) -> Option<super::PaneMap> {
            match (a, b) {
                (Space::Model, Space::Block(_)) => Some(super::PaneMap::Point(p + Vec2::new(1.0, 0.0))),
                (Space::Block(_), Space::Model) => Some(super::PaneMap::Line { through: p, dir: Vec2::new(0.0, 1.0) }),
                _ => None,
            }
        }
        let rel = super::PaneRelations { rotation: |_, _, _| None, map, centre: |_, _, _, _, _| None };
        let s = two_tiles_with_block();
        let d = s.doc().unwrap();
        let spaces = [b(), Space::Model, Space::Model];
        let p = Vec2::new(2.0, 3.0);
        assert_eq!(
            super::crosshairs(Some(&rel), d, &spaces, 0, p),
            vec![
                None,
                Some(super::PaneMap::Line { through: p, dir: Vec2::new(0.0, 1.0) }),
                Some(super::PaneMap::Line { through: p, dir: Vec2::new(0.0, 1.0) })
            ]
        );
        assert_eq!(super::crosshairs(Some(&rel), d, &spaces, 1, p), vec![Some(super::PaneMap::Point(Vec2::new(3.0, 3.0))), None, None]);
        assert_eq!(super::crosshairs(Some(&rel), d, &spaces, 7, p), vec![None, None, None]);
        assert_eq!(super::crosshairs(None, d, &spaces, 0, p), vec![None, None, None]);
        // The session's own: `tile_crosshairs` is `tile_crosshairs_from` the current tile.
        assert_eq!(s.tile_crosshairs(p), s.tile_crosshairs_from(active(&s), p));
        assert_eq!(s.tile_crosshairs_from(1, p), vec![None, None]);
    }

    #[test]
    fn lock_without_relations_leaves_other_tiles() {
        let mut s = two_tiles_with_block();
        let before = s.model_tiles()[1].view;
        assert!(!s.panes_locked(), "off by default");
        s.set_panes_locked(true);
        assert!(s.panes_locked());
        assert!(s.state_mut().unwrap().set_view(View { center: Vec2::new(123.0, -45.0), height: 7.0, twist: 0.0 }));
        s.state_mut().unwrap().set_view_quiet(View { center: Vec2::new(-9.0, 4.0), height: 2.0, twist: 0.0 });
        assert_eq!(s.model_tiles()[1].view, before);
        assert_eq!(s.model_tiles()[0].view.center, Vec2::new(-9.0, 4.0));
    }

    #[test]
    fn world_ortho_survives_retiling_and_rotation_is_zero_without_relations() {
        let mut s = two_tiles_with_block();
        assert!(!s.world_ortho(), "follows the pane by default");
        assert_eq!(s.ortho_rotation(), 0.0);
        assert_eq!(s.drafting_settings().ortho_rotation, 0.0);
        s.set_world_ortho(true);
        s.execute("-vports", &json!({"option": "3"})).unwrap();
        s.activate_tile(1).unwrap();
        assert!(s.world_ortho(), "kept across re-tiling");
        assert_eq!(s.ortho_rotation(), 0.0);
    }

    #[test]
    fn a_deleted_block_shows_as_model() {
        let mut s = two_tiles_with_block();
        enter(&mut s, b());
        s.activate_tile(1).unwrap();
        assert_eq!(s.tile_spaces(), vec![b(), Space::Model]);
        s.undo().unwrap(); // the BLOCK that made B
        assert!(s.doc().unwrap().block("B").is_none());
        assert_eq!(s.tile_spaces(), vec![Space::Model, Space::Model]);
        s.activate_tile(0).unwrap();
        assert_eq!(s.space(), Space::Model);
    }
}

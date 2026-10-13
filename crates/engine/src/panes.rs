//! Panes (LAMB fork patch 9, not to be submitted): each tiled model viewport (VPORTS,
//! [`crate::ModelTiles`]) carries its own space — Model or a block (a LAMB view) — and making a
//! tile current makes its space the edit space.
//!
//! The current tile's space is always `DocState::space`; [`Panes::spaces`] only matters for the
//! other tiles. It is session state, never saved with the drawing. When the tiling changes
//! (VPORTS, or undo of it), every tile but the current one goes back to Model.

use cadcraft_doc::Space;
use cadcraft_geom::Vec2;

use crate::{DocState, Session};

/// The space shown in each tile, for the tiling in `rects`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Panes {
    pub rects: Vec<(Vec2, Vec2)>,
    pub spaces: Vec<Space>,
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
        st.panes = Panes { rects: st.tiles.rects.clone(), spaces: vec![Space::Model; st.tiles.rects.len()] };
    }
    let here = st.space.clone();
    if let Some(slot) = st.panes.spaces.get_mut(old) {
        *slot = here;
    }
    st.space = existing(st, st.panes.spaces.get(new));
    st.mspace = None;
}

impl Session {
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
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::Space;
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
        let v0 = View { center: Vec2::new(5.0, 7.0), height: 3.0 };
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

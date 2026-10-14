//! LAMB (fork-only): each model tile other than the current one draws its own space
//! (`Session::tile_spaces`, LAMB T318) — a plan beside a section — with a display list of its
//! own, built at that tile's zoom for that tile's view, and that space's LAMB overlays (markers
//! and cut lines) sized to that tile's view. A tile showing the edit space draws the canvas's
//! own list, as upstream does. Input is unchanged: only the current tile takes the pointer, and
//! a click in another makes it current (and its space the edit space).

use cadcraft_color::Rgb;
use cadcraft_doc::Space;
use cadcraft_engine::ModelTile;
use cadcraft_geom::Bounds2;
use cadcraft_render::DisplayList;
use egui::Rect;

use super::{Xf, build_window, draw_list, lamb_overlays, tile_xf};
use crate::CadApp;

/// What a tile's list was built from: drawing uid, revision, space, tessellation band and
/// lineweight display.
type Key = (u64, u64, Space, i32, bool);

/// One tile's lists.
#[derive(Default)]
struct Tile {
    key: Option<Key>,
    /// The area `list` was built for (hatch patterns exist only inside it).
    window: Option<Bounds2>,
    /// The tile's own list; `None` for a tile showing the edit space (or the current tile).
    list: Option<DisplayList>,
    overlays: lamb_overlays::Cache,
}

/// The per-tile lists, one entry per model tile in `Session::model_tiles` order.
#[derive(Default)]
pub struct Lists {
    tiles: Vec<Tile>,
    /// Display lists built so far (for tests: a cache hit builds nothing).
    builds: u64,
}

/// Bring each tile's lists up to date for this frame, before `draw_tiles`.
pub(super) fn prepare(app: &mut CadApp, tiles: &[(Rect, ModelTile)]) {
    let lists = &mut app.canvas.panes;
    let Ok(st) = app.session.state() else {
        lists.tiles.clear();
        return;
    };
    let spaces = app.session.tile_spaces();
    let (lwdisplay, transparency) = (app.session.settings.lwdisplay, app.session.settings.transparency_display);
    // Tiles that no longer exist drop their lists.
    lists.tiles.truncate(tiles.len());
    lists.tiles.resize_with(tiles.len(), Tile::default);
    for (i, (r, tile)) in tiles.iter().enumerate() {
        let slot = &mut lists.tiles[i];
        if tile.active || r.width() < 1.0 || r.height() < 1.0 {
            *slot = Tile::default();
            continue;
        }
        let space = spaces.get(i).cloned().unwrap_or(Space::Model);
        let xf = tile_xf(*r, tile);
        slot.overlays.ensure(st, &space, tile.view.height, xf.scale);
        if space == st.space {
            (slot.key, slot.window, slot.list) = (None, None, None);
            continue;
        }
        let px = 1.0 / xf.scale;
        let band = px.max(1e-300).log2().floor() as i32;
        let key: Key = (st.uid, st.revision, space.clone(), band, lwdisplay);
        let bounds = xf.world_bounds();
        let inside = slot.window.is_none_or(|w| bounds.is_empty() || w.contains_box(&bounds));
        if slot.list.is_some() && slot.key.as_ref() == Some(&key) && inside {
            continue;
        }
        // `ensure_list`'s recipe, for this tile alone.
        let opts = cadcraft_render::Options {
            tolerance: 2f64.powi(band) * 0.5,
            min_dash: 2f64.powi(band) * 2.0,
            text: true,
            fill: true,
            lineweights: lwdisplay,
            view_height: tile.view.height,
            plot_style_table: None,
            view: build_window(&[(bounds, px)]),
            transparency,
            skip_viewport: None,
            cancel: None,
        };
        slot.list = Some(cadcraft_render::build(&st.doc, &space, &opts));
        slot.window = opts.view;
        slot.key = Some(key);
        lists.builds += 1;
    }
}

/// Tile `i`'s own list, when it shows a space other than the edit space.
pub(super) fn list_for(app: &CadApp, i: usize) -> Option<&DisplayList> {
    app.canvas.panes.tiles.get(i)?.list.as_ref()
}

/// Draw tile `i`'s LAMB overlays (built by [`prepare`]) in its view `xf`.
pub(super) fn draw_overlays(app: &CadApp, p: &egui::Painter, xf: &Xf, bg: Rgb, i: usize) {
    let Some(tile) = app.canvas.panes.tiles.get(i) else { return };
    for list in tile.overlays.lists() {
        draw_list(p, xf, list, bg, false);
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_engine::{Session, View};
    use cadcraft_geom::Vec2;
    use egui::{Rect, pos2};
    use serde_json::json;

    use super::*;
    use crate::Services;

    fn b() -> Space {
        Space::Block("B".into())
    }

    /// Two tiles: tile 0 shows block `B` (one line, 0,0 → 10,0), tile 1 is current on model
    /// space, which also holds a line far away (100,100 → 200,100).
    fn app() -> (CadApp, Vec<(Rect, ModelTile)>) {
        let mut s = Session::new();
        s.execute("-vports", &json!({"option": "2"})).unwrap();
        s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
        let l = s.doc().unwrap().model.iter().next().unwrap().handle;
        s.execute("block", &json!({ "name": "B", "base": [0, 0], "handles": [l.hex()], "keep": "convert" })).unwrap();
        s.execute("line", &json!({"points": [[100, 100], [200, 100]]})).unwrap();
        {
            let st = s.state_mut().unwrap();
            st.space = b();
            st.mspace = None;
            st.set_view_quiet(View { center: Vec2::new(5.0, 0.0), height: 20.0 });
        }
        s.activate_tile(1).unwrap();
        assert_eq!(s.tile_spaces(), vec![b(), Space::Model]);
        let app = CadApp::new(s, Services::default());
        let tiles = super::super::tile_rects(&app, Rect::from_min_max(pos2(0.0, 0.0), pos2(1600.0, 1000.0)));
        assert_eq!(tiles.len(), 2);
        (app, tiles)
    }

    #[test]
    fn an_inactive_tile_on_another_space_gets_its_own_list() {
        let (mut app, tiles) = app();
        prepare(&mut app, &tiles);
        let list = list_for(&app, 0).expect("tile 0 shows B: a list of its own");
        assert!(!list.prims.is_empty());
        let bb = list.bounds;
        assert!(bb.min.x >= -1e-6 && bb.max.x <= 10.0 + 1e-6 && bb.max.y.abs() < 1e-6, "B's line only, not model space's: {bb:?}");
        assert!(list_for(&app, 1).is_none(), "the current tile draws the canvas list");
        assert_eq!(app.canvas.panes.builds, 1);

        // Same revision, same view: nothing rebuilt.
        prepare(&mut app, &tiles);
        assert_eq!(app.canvas.panes.builds, 1);

        // A new revision rebuilds.
        app.session.execute("line", &json!({"points": [[0, 50], [10, 50]]})).unwrap();
        prepare(&mut app, &tiles);
        assert_eq!(app.canvas.panes.builds, 2);

        // Tiles that are gone drop their lists.
        prepare(&mut app, &tiles[1..]);
        assert!(list_for(&app, 1).is_none());
        assert_eq!(app.canvas.panes.tiles.len(), 1);
    }

    #[test]
    fn a_tile_on_the_edit_space_gets_no_list_of_its_own() {
        let (mut app, tiles) = app();
        // Both tiles on model space: tile 0 draws the canvas list.
        app.session.activate_tile(0).unwrap();
        app.session.state_mut().unwrap().space = Space::Model;
        app.session.activate_tile(1).unwrap();
        assert_eq!(app.session.tile_spaces(), vec![Space::Model, Space::Model]);
        let tiles: Vec<_> = tiles.iter().enumerate().map(|(i, (r, t))| (*r, ModelTile { active: i == 1, ..*t })).collect();
        prepare(&mut app, &tiles);
        assert!(list_for(&app, 0).is_none());
        assert!(list_for(&app, 1).is_none());
        assert_eq!(app.canvas.panes.builds, 0);
    }
}

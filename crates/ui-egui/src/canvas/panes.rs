//! LAMB (fork-only): each model tile other than the current one draws its own space
//! (`Session::tile_spaces`, LAMB T318) — a plan beside a section — with a display list of its
//! own, built at that tile's zoom for that tile's view, and that space's LAMB overlays (markers
//! and cut lines) sized to that tile's view. A tile showing the edit space draws the canvas's
//! own list, as upstream does. Input is unchanged: only the current tile takes the pointer, and
//! a click in another makes it current (and its space the edit space).
//!
//! Each tile is also drawn turned by its display-only rotation (`Session::tile_rotations`, LAMB
//! T320) — a plan beside a section turns so the section's cut line reads horizontal. The
//! rotation is the angle of the direction that should read horizontal (the cut line's, in the
//! tile's space), so the tile turns the other way: minus it is added to the tile's `Xf` twist
//! only; it is never written to the drawing, the tile's view or
//! `View.twist`, and is never an undo step. Pointer input follows through `Xf::to_world`.
//!
//! Hover crosses the seams (LAMB T321, [`draw_hover`]): the hovered point — the current tile's
//! snapped cursor, or a point snapped in an inactive tile's own space — is mapped through
//! `Session::tile_crosshairs_from` into every related tile, which draws a full-tile crosshair
//! for a point or one line for a line. Hover only: a click in an inactive tile still just makes
//! it current.

use cadcraft_color::Rgb;
use cadcraft_doc::Space;
use cadcraft_engine::ModelTile;
use cadcraft_engine::panes::PaneMap;
use cadcraft_engine::snap::{self, SnapHit};
use cadcraft_geom::{Bounds2, Vec2};
use cadcraft_render::DisplayList;
use egui::{Pos2, Rect, Stroke, pos2};

use super::{Xf, build_window, clip_infinite, draw_crosshair, draw_list, draw_snap_marker, lamb_overlays, tile_xf};
use crate::CadApp;
use crate::theme::Tokens;

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

/// Each tile's display-only rotation (radians), one per `Session::model_tiles` entry; empty
/// when untiled. Read once per frame and passed to [`rotation`].
pub(super) fn rotations(app: &CadApp) -> Vec<f64> {
    app.session.tile_rotations()
}

/// The twist tile `i` adds for its rotation from [`rotations`]: minus the rotation, which turns
/// that direction onto the screen's horizontal (the TS reference pane's camera rotation draws
/// `rotate(world, -rotation)` too). 0 when untiled, out of range or not finite.
pub(super) fn rotation(rots: &[f64], i: usize) -> f64 {
    rots.get(i).copied().filter(|a| a.is_finite()).map_or(0.0, |a| -a)
}

/// Bring each tile's lists up to date for this frame, before `draw_tiles`; `rots` is
/// [`rotations`].
pub(super) fn prepare(app: &mut CadApp, tiles: &[(Rect, ModelTile)], rots: &[f64]) {
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
        let xf = tile_xf(*r, tile, rotation(rots, i));
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
            on_screen: true,
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

/// Where the pointer hovers, as the source of the cross-tile crosshair.
#[derive(Clone, Copy, Debug)]
pub(super) struct Hover {
    /// The hovered tile.
    pub tile: usize,
    /// The (snapped) point, in that tile's space.
    pub point: Vec2,
    /// The snap found for the hover: in an inactive tile, or in the current tile at idle (no
    /// point prompt) when tiled; `None` while a point prompt snaps the cursor itself.
    pub snap: Option<SnapHit>,
    /// The pointer, on screen.
    pub at: Pos2,
}

/// The hover source for pointer position `hover`: in the current tile, the canvas's cursor —
/// already snapped by `effective_point` while a point prompt is active, and otherwise (idle,
/// two or more tiles) object-snapped here in the edit space, as the TS app snaps every pointer
/// move; in an inactive tile, the pointer in that tile's view snapped against that tile's own
/// space. The idle snap feeds only the hover: it never changes the canvas cursor or a click.
/// `None` off the tiles, or untiled.
pub(super) fn hover_source(app: &CadApp, tiles: &[(Rect, ModelTile)], rots: &[f64], hover: Option<Pos2>) -> Option<Hover> {
    let hp = hover?;
    let (h, (r, tile)) = tiles.iter().enumerate().find(|(_, (r, _))| r.width() >= 1.0 && r.height() >= 1.0 && r.contains(hp))?;
    if tile.active {
        let cursor = app.canvas.cursor?;
        let prompting = app.session.current_prompt().as_ref().is_some_and(cadcraft_engine::pointmod::accepts);
        let hit = match (prompting, tiles.len() >= 2, app.canvas.xf, app.session.state()) {
            (false, true, Some(xf), Ok(st)) => {
                let s = &app.session.settings;
                snap::osnap(&st.doc, &st.space, cursor, s.aperture / xf.scale, s.osmode, None, false)
            }
            _ => None,
        };
        return Some(Hover { tile: h, point: hit.map_or(cursor, |hit| hit.point), snap: hit, at: hp });
    }
    let xf = tile_xf(*r, tile, rotation(rots, h));
    let w = xf.to_world(hp);
    let space = app.session.tile_spaces().get(h).cloned()?;
    let st = app.session.state().ok()?;
    let s = &app.session.settings;
    let hit = snap::osnap(&st.doc, &space, w, s.aperture / xf.scale, s.osmode, None, false);
    Some(Hover { tile: h, point: hit.map_or(w, |hit| hit.point), snap: hit, at: hp })
}

/// The screen segments `src` draws in the other tiles, each with its tile's index: a
/// screen-aligned horizontal and vertical line across the tile through a mapped point (each
/// only while it lies within the tile), one line for a mapped line, nothing in an unrelated
/// tile. The current tile uses the canvas's own transform.
pub(super) fn hover_lines(app: &CadApp, tiles: &[(Rect, ModelTile)], rots: &[f64], src: &Hover) -> Vec<(usize, [Pos2; 2])> {
    let maps = app.session.tile_crosshairs_from(src.tile, src.point);
    let mut out = Vec::new();
    for (i, m) in maps.into_iter().enumerate() {
        let (Some(m), Some((r, tile))) = (m, tiles.get(i)) else { continue };
        let xf = if tile.active { app.canvas.xf } else { Some(tile_xf(*r, tile, rotation(rots, i))) };
        let Some(xf) = xf else { continue };
        match m {
            PaneMap::Point(q) => {
                let s = xf.to_screen(q);
                if r.y_range().contains(s.y) {
                    out.push((i, [pos2(r.left(), s.y), pos2(r.right(), s.y)]));
                }
                if r.x_range().contains(s.x) {
                    out.push((i, [pos2(s.x, r.top()), pos2(s.x, r.bottom())]));
                }
            }
            PaneMap::Line { through, dir } => {
                if let Some(seg) = clip_infinite(&xf, through, dir, false) {
                    out.push((i, seg));
                }
            }
        }
    }
    out
}

/// Draw the cross-tile hover (after `draw_tiles`): the mapped lines in every related tile; over
/// the current tile, the idle snap's marker (the canvas draws its own crosshair); over an
/// inactive tile, its own crosshair and snap marker, with the OS cursor hidden as over the
/// current one. `hover` is the pointer, `None` when the canvas isn't hovered.
pub(super) fn draw_hover(app: &CadApp, ui: &egui::Ui, tiles: &[(Rect, ModelTile)], rots: &[f64], hover: Option<Pos2>) {
    let Some(src) = hover_source(app, tiles, rots, hover) else { return };
    let stroke = Stroke::new(1.0, Tokens::get().crosshair);
    for (i, seg) in hover_lines(app, tiles, rots, &src) {
        ui.painter_at(tiles[i].0).line_segment(seg, stroke);
    }
    let (r, tile) = &tiles[src.tile];
    if tile.active {
        if let (Some(hit), Some(xf)) = (src.snap, app.canvas.xf) {
            draw_snap_marker(&ui.painter_at(*r), xf.to_screen(hit.point), &hit);
        }
        return;
    }
    ui.ctx().set_cursor_icon(if app.ui.system_cursor { egui::CursorIcon::Crosshair } else { egui::CursorIcon::None });
    let p = ui.painter_at(*r);
    if let Some(hit) = src.snap {
        draw_snap_marker(&p, tile_xf(*r, tile, rotation(rots, src.tile)).to_screen(hit.point), &hit);
    }
    draw_crosshair(app, &p, *r, src.at, false, Tokens::get().crosshair);
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
            st.set_view_quiet(View { center: Vec2::new(5.0, 0.0), height: 20.0, twist: 0.0 });
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
        prepare(&mut app, &tiles, &[]);
        let list = list_for(&app, 0).expect("tile 0 shows B: a list of its own");
        assert!(!list.prims.is_empty());
        let bb = list.bounds;
        assert!(bb.min.x >= -1e-6 && bb.max.x <= 10.0 + 1e-6 && bb.max.y.abs() < 1e-6, "B's line only, not model space's: {bb:?}");
        assert!(list_for(&app, 1).is_none(), "the current tile draws the canvas list");
        assert_eq!(app.canvas.panes.builds, 1);

        // Same revision, same view: nothing rebuilt.
        prepare(&mut app, &tiles, &[]);
        assert_eq!(app.canvas.panes.builds, 1);

        // A new revision rebuilds.
        app.session.execute("line", &json!({"points": [[0, 50], [10, 50]]})).unwrap();
        prepare(&mut app, &tiles, &[]);
        assert_eq!(app.canvas.panes.builds, 2);

        // Tiles that are gone drop their lists.
        prepare(&mut app, &tiles[1..], &[]);
        assert!(list_for(&app, 1).is_none());
        assert_eq!(app.canvas.panes.tiles.len(), 1);
    }

    /// A test relation: a tile on block `B` turns 0.5 rad to align with model space; nothing
    /// else is related. Registered once per test process (a second registration is refused).
    fn register() {
        fn rotation(_: &cadcraft_doc::Drawing, a: &Space, b: &Space) -> Option<f64> {
            (*a == Space::Block("B".into()) && *b == Space::Model).then_some(0.5)
        }
        let _ =
            cadcraft_engine::panes::register_pane_relations(cadcraft_engine::panes::PaneRelations { rotation, map, centre: |_, _, _, _, _| None });
    }

    /// The test relation's map: B → Model is a vertical line through the point; Model → B the
    /// point shifted by (-100, -100).
    fn map(_: &cadcraft_doc::Drawing, a: &Space, b: &Space, p: Vec2) -> Option<PaneMap> {
        match (a, b) {
            (Space::Block(_), Space::Model) => Some(PaneMap::Line { through: p, dir: Vec2::new(0.0, 1.0) }),
            (Space::Model, Space::Block(_)) => Some(PaneMap::Point(p - Vec2::new(100.0, 100.0))),
            _ => None,
        }
    }

    #[test]
    fn hover_crosses_the_seams() {
        register();
        let (mut app, tiles) = app();
        let rots = rotations(&app);
        let (r0, r1) = (tiles[0].0, tiles[1].0);
        let xf0 = tile_xf(r0, &tiles[0].1, rotation(&rots, 0));
        let xf1 = tile_xf(r1, &tiles[1].1, rotation(&rots, 1));
        app.canvas.xf = Some(xf1);

        // Hover in the current tile (model space): a point relation — two segments across tile
        // 0, crossing where the mapped point lands.
        app.canvas.cursor = Some(Vec2::new(105.0, 100.0));
        let src = hover_source(&app, &tiles, &rots, Some(r1.center())).unwrap();
        assert_eq!((src.tile, src.point), (1, Vec2::new(105.0, 100.0)));
        let lines = hover_lines(&app, &tiles, &rots, &src);
        let s = xf0.to_screen(Vec2::new(5.0, 0.0));
        assert!(r0.contains(s));
        assert_eq!(lines, vec![(0, [pos2(r0.left(), s.y), pos2(r0.right(), s.y)]), (0, [pos2(s.x, r0.top()), pos2(s.x, r0.bottom())])]);

        // Hover in the inactive tile (block B): it snaps to B's endpoint (10,0) in B's own
        // space, not to model space's nearer endpoint under the same world point.
        app.session.execute("line", &json!({"points": [[9.95, 0.1], [9.95, 30]]})).unwrap();
        app.session.settings.osmode = snap::mode::END;
        app.session.settings.aperture = 10.0;
        let hp = xf0.to_screen(Vec2::new(10.0, 0.1));
        let src = hover_source(&app, &tiles, &rots, Some(hp)).unwrap();
        assert_eq!(src.tile, 0);
        assert!((src.point - Vec2::new(10.0, 0.0)).len() < 1e-9, "{:?}", src.point);
        assert!(src.snap.is_some());
        // The line relation: one segment in the current tile, vertical through x = 10.
        let lines = hover_lines(&app, &tiles, &rots, &src);
        assert_eq!(lines.len(), 1);
        let (i, [a, b]) = lines[0];
        let x = xf1.to_screen(Vec2::new(10.0, 0.0)).x;
        assert_eq!(i, 1);
        assert!((a.x - x).abs() < 1e-3 && (b.x - x).abs() < 1e-3 && a.y != b.y, "{a:?} {b:?}");

        // Not hovering: nothing.
        assert!(hover_source(&app, &tiles, &rots, None).is_none());
    }

    #[test]
    fn the_current_tile_snaps_its_hover_at_idle() {
        register();
        let (mut app, tiles) = app();
        let rots = rotations(&app);
        let xf0 = tile_xf(tiles[0].0, &tiles[0].1, rotation(&rots, 0));
        app.canvas.xf = Some(tile_xf(tiles[1].0, &tiles[1].1, rotation(&rots, 1)));
        app.session.settings.osmode = snap::mode::END;
        app.session.settings.aperture = 10.0;
        // Near model space's line end (100,100), well inside the aperture.
        let raw = Vec2::new(100.0, 100.0) + Vec2::new(2.0, 1.0) / app.canvas.xf.unwrap().scale;
        app.canvas.cursor = Some(raw);

        // No command: the hover snaps to the end, and the other tile's lines go through the
        // mapped end (0,0), not the mapped raw cursor.
        let src = hover_source(&app, &tiles, &rots, Some(tiles[1].0.center())).unwrap();
        assert_eq!(src.tile, 1);
        assert!(src.snap.is_some());
        assert!((src.point - Vec2::new(100.0, 100.0)).len() < 1e-9, "{:?}", src.point);
        let s = xf0.to_screen(Vec2::new(0.0, 0.0));
        let r0 = tiles[0].0;
        assert_eq!(
            hover_lines(&app, &tiles, &rots, &src),
            vec![(0, [pos2(r0.left(), s.y), pos2(r0.right(), s.y)]), (0, [pos2(s.x, r0.top()), pos2(s.x, r0.bottom())])]
        );
        assert_eq!(app.canvas.cursor, Some(raw), "the idle snap never moves the canvas cursor");

        // A point prompt snaps the cursor itself: the hover takes the cursor as it is.
        app.session.start("line").unwrap();
        let src = hover_source(&app, &tiles, &rots, Some(tiles[1].0.center())).unwrap();
        assert!(src.snap.is_none());
        assert_eq!(src.point, raw);
    }

    #[test]
    fn an_unrelated_tile_draws_no_hover() {
        register();
        let (mut app, tiles) = app();
        app.session.activate_tile(0).unwrap();
        app.session.state_mut().unwrap().space = Space::Model;
        app.session.activate_tile(1).unwrap();
        let tiles: Vec<_> = tiles.iter().enumerate().map(|(i, (r, t))| (*r, ModelTile { active: i == 1, ..*t })).collect();
        let rots = rotations(&app);
        app.canvas.xf = Some(tile_xf(tiles[1].0, &tiles[1].1, 0.0));
        app.canvas.cursor = Some(Vec2::new(105.0, 100.0));
        let src = hover_source(&app, &tiles, &rots, Some(tiles[1].0.center())).unwrap();
        assert!(hover_lines(&app, &tiles, &rots, &src).is_empty());
        let src = hover_source(&app, &tiles, &rots, Some(tiles[0].0.center())).unwrap();
        assert!(hover_lines(&app, &tiles, &rots, &src).is_empty());
    }

    #[test]
    fn a_tile_draws_turned_by_its_rotation_and_nothing_is_written_back() {
        register();
        let (mut app, tiles) = app();
        let rots = rotations(&app);
        assert_eq!(rots, vec![0.5, 0.0]);
        assert_eq!((rotation(&rots, 0), rotation(&rots, 1), rotation(&rots, 7)), (-0.5, 0.0, 0.0));
        assert_eq!(rotation(&[f64::NAN], 0), 0.0);

        // Tile 0's transform is its own view's twist plus the rotation, and round-trips (to
        // within the f32 screen position: a thousandth of a point).
        let (r, tile) = &tiles[0];
        let xf = tile_xf(*r, tile, rotation(&rots, 0));
        assert_eq!(xf.twist, tile.view.twist - 0.5);
        // The direction at 0.5 rad (the cut line's) reads horizontal, pointing right.
        let (a, b) = (xf.to_screen(tile.view.center), xf.to_screen(tile.view.center + Vec2::new(0.5f64.cos(), 0.5f64.sin()) * 5.0));
        assert!((b.y - a.y).abs() < 1e-3 && b.x > a.x, "{a:?} -> {b:?}");
        for p in [Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0), Vec2::new(-3.25, 7.5)] {
            let q = xf.to_world(xf.to_screen(p));
            assert!((q - p).len() < 1e-3 / xf.scale, "{p:?} -> {q:?}");
        }

        // One whole frame: the current tile (rotation 0) keeps its plain view; nothing about
        // the rotation lands in the drawing, the tiles' views or the current view.
        let before = (app.session.state().unwrap().view(), app.session.model_tiles());
        let ctx = egui::Context::default();
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_max(pos2(0.0, 0.0), pos2(1600.0, 1000.0))), ..Default::default() };
        ctx.run_ui(raw, |ui| super::super::show(&mut app, ui)).textures_delta.clear();
        assert_eq!(app.canvas.xf.map(|x| x.twist), Some(before.0.twist));
        assert_eq!((app.session.state().unwrap().view(), app.session.model_tiles()), before);
        assert_eq!(app.session.state().unwrap().view().twist, 0.0);
        assert!(app.session.model_tiles().iter().all(|t| t.view.twist == 0.0));
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
        prepare(&mut app, &tiles, &[]);
        assert!(list_for(&app, 0).is_none());
        assert!(list_for(&app, 1).is_none());
        assert_eq!(app.canvas.panes.builds, 0);
    }
}

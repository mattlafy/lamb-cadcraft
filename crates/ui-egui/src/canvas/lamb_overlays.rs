//! LAMB (fork-only): the registered overlay providers' display lists — intersection markers and
//! cut lines from `lamb_cadcraft::lamb_overlay` — drawn over the drawing for the space being
//! edited (and, through `panes`, each other model tile's space). Not drawn in paper space with an
//! active MSPACE viewport, and never plotted (overlays are not part of `build_plot`).

use cadcraft_color::Rgb;
use cadcraft_doc::Space;
use cadcraft_render::DisplayList;

use super::{Xf, draw_list};
use crate::CadApp;

/// What the lists were built from: drawing uid, revision, space, view height (the provider
/// sizes markers from the view height, so a zoom rebuilds and a pan alone does not) and
/// `cadcraft_render::overlays_generation()` (a provider choice changed, e.g. LAMB's underlay).
type Key = (u64, u64, Space, u64, u64);

/// The overlay lists for the last key.
#[derive(Default)]
pub struct Cache {
    key: Option<Key>,
    lists: Vec<DisplayList>,
}

impl Cache {
    /// The lists for `key`, calling `build` only when it differs from the cached one.
    fn get(&mut self, key: Key, build: impl FnOnce() -> Vec<DisplayList>) -> &[DisplayList] {
        if self.key.as_ref() != Some(&key) {
            self.lists = build();
            self.key = Some(key);
        }
        &self.lists
    }
}

impl Cache {
    /// The overlay lists for `space` seen with a view `view_height` tall at `scale` pixels per
    /// unit, rebuilt only when the drawing, the space or the view height changes. The edit space
    /// ([`draw`]) and each other model tile ([`super::panes`]) keep a cache of their own.
    pub(super) fn ensure(&mut self, st: &cadcraft_engine::DocState, space: &Space, view_height: f64, scale: f64) -> &[DisplayList] {
        // The tessellation tolerance `ensure_list` uses for this zoom band.
        let band = (1.0 / scale).max(1e-300).log2().floor() as i32;
        let opts = cadcraft_render::Options { view_height, tolerance: 2f64.powi(band) * 0.5, ..Default::default() };
        let key = (st.uid, st.revision, space.clone(), view_height.to_bits(), cadcraft_render::overlays_generation());
        let doc = &st.doc;
        self.get(key, || cadcraft_render::overlays(doc, space, &opts))
    }

    /// The lists last built (none before the first [`Cache::ensure`]).
    pub(super) fn lists(&self) -> &[DisplayList] {
        &self.lists
    }
}

/// Draw the overlay lists for the space being edited, in screen view `xf`.
pub(super) fn draw(app: &mut CadApp, painter: &egui::Painter, xf: &Xf, bg: Rgb) {
    let Ok(st) = app.session.state() else { return };
    if st.mspace.is_some() {
        return;
    }
    for list in app.canvas.lamb_overlays.ensure(st, &st.space, st.view().height, xf.scale) {
        draw_list(painter, xf, list, bg, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_are_rebuilt_only_when_the_key_changes() {
        let mut c = Cache::default();
        let builds = std::cell::Cell::new(0);
        let mut get = |k: Key| {
            c.get(k, || {
                builds.set(builds.get() + 1);
                vec![DisplayList::default()]
            });
            builds.get()
        };
        let key = |rev: u64, h: f64| (7, rev, Space::Model, h.to_bits(), 0);
        assert_eq!(get(key(1, 100.0)), 1);
        assert_eq!(get(key(1, 100.0)), 1, "same uid/revision/space/height reuses the lists");
        assert_eq!(get(key(1, 200.0)), 2, "a new view height rebuilds");
        assert_eq!(get(key(2, 200.0)), 3, "a new revision rebuilds");
        assert_eq!(get((7, 2, Space::Block("LAMB_VIEW_1".into()), 200f64.to_bits(), 0)), 4, "a new space rebuilds");
        assert_eq!(get((8, 2, Space::Block("LAMB_VIEW_1".into()), 200f64.to_bits(), 0)), 5, "another drawing rebuilds");
        assert_eq!(get((8, 2, Space::Block("LAMB_VIEW_1".into()), 200f64.to_bits(), 1)), 6, "a new overlay generation rebuilds");
    }
}

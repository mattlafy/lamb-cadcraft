//! Registered extents sources: geometry outside the drawing's entities that ZOOM Extents fits
//! along with them (an overlay that draws real linework, for example). Same shape as
//! [`crate::snap::register_snap_provider`].

use cadcraft_doc::{Drawing, Space};
use cadcraft_geom::Bounds2;

/// Bounds of extra geometry shown in `space`; [`Bounds2::EMPTY`] when there is none.
pub type ExtentsSource = fn(&Drawing, &Space) -> Bounds2;

static EXTENTS_SOURCES: std::sync::RwLock<Vec<ExtentsSource>> = std::sync::RwLock::new(Vec::new());

/// Register a process-global [`ExtentsSource`]; allowed at any time. Fails only when the
/// registry's lock is poisoned.
pub fn register_extents_source(p: ExtentsSource) -> Result<(), &'static str> {
    let mut v = EXTENTS_SOURCES.write().map_err(|_| "extents source registry is poisoned")?;
    v.push(p);
    Ok(())
}

/// `ext` widened by every registered source's non-empty bounds for `space`.
pub(crate) fn with_sources(d: &Drawing, space: &Space, ext: Bounds2) -> Bounds2 {
    let sources: Vec<ExtentsSource> = match EXTENTS_SOURCES.read() {
        Ok(v) => v.clone(),
        Err(_) => return ext,
    };
    sources.iter().map(|f| f(d, space)).filter(|b| !b.is_empty()).fold(ext, |acc, b| acc.union(&b))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use cadcraft_geom::Vec2;
    use serde_json::json;

    use super::*;
    use crate::Session;

    thread_local! {
        /// Only this thread's test sees the source's bounds; other tests share the registry.
        static ON: Cell<bool> = const { Cell::new(false) };
    }

    fn far_box(_d: &Drawing, _s: &Space) -> Bounds2 {
        if ON.with(Cell::get) { Bounds2::new(Vec2::new(1000.0, 0.0), Vec2::new(1100.0, 10.0)) } else { Bounds2::EMPTY }
    }

    fn fit(s: &mut Session) -> crate::View {
        s.execute("zoom.extents", &json!({})).unwrap();
        s.state().unwrap().view()
    }

    #[test]
    fn a_registered_source_widens_zoom_extents_and_an_empty_one_changes_nothing() {
        register_extents_source(far_box).unwrap();
        let mut s = Session::new();
        s.viewport_px = (1000.0, 500.0);
        s.execute("line", &json!({"points": [[0, 0], [100, 10]]})).unwrap();
        let alone = fit(&mut s);
        assert!((alone.center.x - 50.0).abs() < 1e-9, "empty source: unchanged ({:?})", alone.center);
        ON.with(|o| o.set(true));
        let wide = fit(&mut s);
        ON.with(|o| o.set(false));
        assert!((wide.center.x - 550.0).abs() < 1e-9, "fits 0..1100 ({:?})", wide.center);
        assert!(wide.height * 2.0 >= 1100.0, "{}", wide.height);
    }
}

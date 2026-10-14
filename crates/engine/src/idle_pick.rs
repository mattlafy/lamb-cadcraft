//! Idle-click pick hook (LAMB fork patch, not to be submitted): when a click with no command
//! running hits no entity, an adapter may claim it for an object of its own that isn't an entity
//! (LAMB: a section's cut line drawn by an overlay picks that section's view).
//!
//! The adapter registers one hook ([`register_idle_pick`]). [`Session::idle_click`] asks it only
//! when `select::pick` found nothing and Shift is up; a `Some(key)` clears the entity selection,
//! stores `(edit space, key)` as the picked object (`DocState::picked`, session state, never
//! saved) and opens no selection window. Any `set_selection` and every idle click clear it again;
//! [`Session::picked`] reads it back.
//!
//! An adapter that draws the pick registers one observer ([`register_picked_observer`]); it is
//! told the new value every time `DocState::picked` changes (and only then).

use std::sync::OnceLock;

use cadcraft_doc::Space;
use cadcraft_geom::Vec2;

use crate::Session;

/// `(session, point in the edit space, pick aperture in drawing units)` → the picked object's key.
pub type IdlePick = fn(&mut Session, Vec2, f64) -> Option<String>;

static HOOK: OnceLock<IdlePick> = OnceLock::new();

/// Register the idle-click pick hook. Once per process; a second call is an error.
pub fn register_idle_pick(f: IdlePick) -> Result<(), &'static str> {
    HOOK.set(f).map_err(|_| "idle pick hook already registered")
}

/// The registered hook, if any.
pub fn idle_pick() -> Option<IdlePick> {
    HOOK.get().copied()
}

/// Told the new picked object (`(space, key)`, or `None` when cleared) whenever it changes.
pub type PickedObserver = fn(Option<&(Space, String)>);

static OBSERVER: OnceLock<PickedObserver> = OnceLock::new();

/// Register the picked-object observer. Once per process; a second call is an error.
pub fn register_picked_observer(f: PickedObserver) -> Result<(), &'static str> {
    OBSERVER.set(f).map_err(|_| "picked observer already registered")
}

/// The registered observer, if any.
pub fn picked_observer() -> Option<PickedObserver> {
    OBSERVER.get().copied()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::cell::Cell;

    use serde_json::json;

    use super::*;

    /// The test hook only answers in block `IDLEPICK`, so other engine tests' idle clicks are
    /// untouched by it.
    fn space() -> Space {
        Space::Block("IDLEPICK".into())
    }
    thread_local! {
        /// Times the hook ran on this test's thread (the hook runs on the clicking thread).
        static ASKED: Cell<usize> = const { Cell::new(0) };
    }
    fn hook(s: &mut Session, _p: Vec2, _ap: f64) -> Option<String> {
        if s.space() != space() {
            return None;
        }
        ASKED.with(|a| a.set(a.get() + 1));
        Some("thing".into())
    }

    /// A session editing block `IDLEPICK` (a line from 0,0 to 10,0), with the test hook registered.
    fn session() -> Session {
        let _ = register_idle_pick(hook);
        let mut s = Session::new();
        s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
        let l = s.doc().unwrap().model.iter().next().unwrap().handle;
        s.execute("block", &json!({ "name": "IDLEPICK", "base": [0, 0], "handles": [l.hex()], "keep": "delete" })).unwrap();
        let st = s.state_mut().unwrap();
        st.space = space();
        st.mspace = None;
        s
    }

    thread_local! {
        /// Picked-object changes the observer saw on this test's thread.
        static TOLD: Cell<usize> = const { Cell::new(0) };
    }
    fn observer(v: Option<&(Space, String)>) {
        if v.is_none_or(|(sp, _)| *sp == space()) {
            TOLD.with(|t| t.set(t.get() + 1));
        }
    }

    #[test]
    fn the_observer_is_told_each_change_once() {
        let _ = register_picked_observer(observer);
        assert!(register_picked_observer(observer).is_err());
        let mut s = session();
        let t0 = TOLD.with(Cell::get);
        s.idle_click(Vec2::new(5.0, 500.0), false).unwrap();
        assert_eq!(TOLD.with(Cell::get), t0 + 1, "picked: told once, with Some");
        s.set_selection(vec![]);
        s.set_selection(vec![]);
        assert_eq!(TOLD.with(Cell::get), t0 + 2, "cleared once; clearing again is no change");
    }

    #[test]
    fn a_second_registration_is_refused() {
        let _ = register_idle_pick(hook);
        assert!(register_idle_pick(hook).is_err());
    }

    #[test]
    fn an_empty_click_the_hook_claims_picks_without_a_window() {
        let mut s = session();
        s.idle_click(Vec2::new(5.0, 500.0), false).unwrap();
        assert_eq!(s.picked().as_deref(), Some("thing"));
        assert!(s.pending_window.is_none());
        assert!(s.selection().is_empty());
        // The next idle click clears it.
        s.idle_click(Vec2::new(5.0, 900.0), true).unwrap();
        assert_eq!(s.picked(), None);
    }

    #[test]
    fn shift_opens_the_window_instead() {
        let mut s = session();
        s.idle_click(Vec2::new(5.0, 500.0), true).unwrap();
        assert_eq!(s.picked(), None);
        assert!(s.pending_window.is_some());
    }

    #[test]
    fn an_entity_under_the_click_wins_and_the_hook_is_not_asked() {
        let mut s = session();
        let before = ASKED.with(Cell::get);
        s.idle_click(Vec2::new(5.0, 0.0), false).unwrap();
        assert_eq!(s.selection().len(), 1);
        assert_eq!(s.picked(), None);
        assert_eq!(ASKED.with(Cell::get), before, "hook not asked");
        assert!(s.pending_window.is_none());
    }

    #[test]
    fn selecting_entities_clears_the_pick() {
        let mut s = session();
        s.idle_click(Vec2::new(5.0, 500.0), false).unwrap();
        assert!(s.picked().is_some());
        let h = s.doc().unwrap().block("IDLEPICK").unwrap().entities.iter().next().unwrap().handle;
        s.set_selection(vec![h]);
        assert_eq!(s.picked(), None);
        s.set_selection(vec![]);
        assert_eq!(s.picked(), None, "cleared, not merely hidden by the selection");
    }

    #[test]
    fn a_pick_belongs_to_its_space() {
        let mut s = session();
        s.idle_click(Vec2::new(5.0, 500.0), false).unwrap();
        s.state_mut().unwrap().space = Space::Model;
        assert_eq!(s.picked(), None);
    }
}

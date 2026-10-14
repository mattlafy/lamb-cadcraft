//! Link-travel set hook (LAMB fork patch 19, not to be submitted): the ids an adapter attached to
//! the current selection (LAMB: the links that travel with the next MOVE/STRETCH), seeded when an
//! idle selection box closes.
//!
//! `DocState::travel` is session state — never saved, not in undo snapshots. [`Session::travel`]
//! reads it, [`Session::set_travel`] replaces it. The adapter registers one [`BoxObserver`]
//! ([`register_box_observer`]); [`Session::idle_click`] calls it when an idle selection box closes,
//! after the selection is set, with the handles the box selected (after group expansion) and
//! whether it was a crossing box — never for the Shift removal box (removing is not a new gather),
//! and never for a box drawn at a command's `Select objects:` prompt.
//!
//! The set dies with the selection: at the quiet points — the end of an idle click, Esc with no
//! command, and after the after-command observers of a finished command — it is cleared when the
//! selection is empty. Not in `set_selection`: MOVE empties the selection mid-run, before the
//! observers that read the set have run.

use std::sync::OnceLock;

use cadcraft_doc::Handle;

use crate::Session;

/// `(session, handles the box selected, crossing)`, called after the selection is set.
pub type BoxObserver = fn(&mut Session, &[Handle], bool);

static OBSERVER: OnceLock<BoxObserver> = OnceLock::new();

/// Register the idle selection-box observer. Once per process; a second call is an error.
pub fn register_box_observer(f: BoxObserver) -> Result<(), &'static str> {
    OBSERVER.set(f).map_err(|_| "box observer already registered")
}

/// The registered observer, if any.
pub fn box_observer() -> Option<BoxObserver> {
    OBSERVER.get().copied()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use cadcraft_doc::Space;
    use cadcraft_geom::Vec2;
    use serde_json::json;

    use super::*;

    /// The test observer only acts in block `TRAVELTEST`, so other engine tests' boxes are
    /// untouched by it: crossing seeds the selected handles' values, window seeds nothing.
    fn observer(s: &mut Session, handles: &[Handle], crossing: bool) {
        if s.space() != Space::Block("TRAVELTEST".into()) {
            return;
        }
        s.set_travel(if crossing { handles.iter().map(|h| h.0).collect() } else { Vec::new() });
    }

    /// A session editing block `TRAVELTEST` (a line from 0,0 to 10,0), with the test observer.
    fn session() -> (Session, Handle) {
        let _ = register_box_observer(observer);
        let mut s = Session::new();
        s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
        let l = s.doc().unwrap().model.iter().next().unwrap().handle;
        s.execute("block", &json!({ "name": "TRAVELTEST", "base": [0, 0], "handles": [l.hex()], "keep": "delete" })).unwrap();
        let st = s.state_mut().unwrap();
        st.space = Space::Block("TRAVELTEST".into());
        st.mspace = None;
        let h = s.doc().unwrap().block("TRAVELTEST").unwrap().entities.iter().next().unwrap().handle;
        (s, h)
    }

    /// A box from `a` to `b`, opened over empty space.
    fn boxed(s: &mut Session, a: (f64, f64), b: (f64, f64), shift: bool) {
        assert!(s.begin_window(Vec2::new(a.0, a.1)));
        s.idle_click(Vec2::new(b.0, b.1), shift).unwrap();
    }

    #[test]
    fn a_second_registration_is_refused() {
        let _ = register_box_observer(observer);
        assert!(register_box_observer(observer).is_err());
    }

    #[test]
    fn crossing_seeds_window_empties() {
        let (mut s, h) = session();
        // Right to left: crossing, catching the line by touching it.
        boxed(&mut s, (5.0, 5.0), (-5.0, -5.0), false);
        assert_eq!(s.selection(), vec![h]);
        assert_eq!(s.travel(), vec![h.0]);
        // Left to right over the whole line: a window — selects it, seeds nothing.
        boxed(&mut s, (-5.0, -5.0), (15.0, 5.0), false);
        assert_eq!(s.selection(), vec![h]);
        assert!(s.travel().is_empty());
    }

    #[test]
    fn the_shift_removal_box_is_not_a_gather() {
        let (mut s, h) = session();
        boxed(&mut s, (5.0, 5.0), (-5.0, -5.0), false);
        s.set_travel(vec![42]);
        // Shift + crossing over nothing: a removal box, observer not called; selection kept.
        boxed(&mut s, (500.0, 505.0), (495.0, 495.0), true);
        assert_eq!(s.selection(), vec![h]);
        assert_eq!(s.travel(), vec![42]);
    }

    #[test]
    fn esc_and_an_empty_selection_clear() {
        let (mut s, _) = session();
        boxed(&mut s, (5.0, 5.0), (-5.0, -5.0), false);
        assert!(!s.travel().is_empty());
        s.cancel();
        assert!(s.selection().is_empty() && s.travel().is_empty(), "Esc clears");

        // An idle click after the selection was emptied by the host clears at that quiet point.
        boxed(&mut s, (5.0, 5.0), (-5.0, -5.0), false);
        s.set_selection(vec![]);
        assert!(!s.travel().is_empty(), "set_selection itself never clears");
        s.idle_click(Vec2::new(500.0, 500.0), false).unwrap();
        assert!(s.travel().is_empty(), "the next idle click does");

        // PICKADD off: a window over nothing empties the selection, and with it the set.
        s.cancel();
        boxed(&mut s, (5.0, 5.0), (-5.0, -5.0), false);
        s.settings.pickadd = false;
        boxed(&mut s, (500.0, 500.0), (600.0, 600.0), false);
        assert!(s.selection().is_empty() && s.travel().is_empty());
    }

    #[test]
    fn a_move_keeps_the_set_until_its_observers_ran() {
        thread_local! {
            static SEEN: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        fn seen(s: &mut Session, _before: &std::sync::Arc<cadcraft_doc::Drawing>) {
            SEEN.with(|v| *v.borrow_mut() = s.travel());
        }
        let (mut s, h) = session();
        s.observers.push(seen);
        boxed(&mut s, (5.0, 5.0), (-5.0, -5.0), false);
        s.execute("move", &json!({"handles": [h.hex()], "delta": [3, 4]})).unwrap();
        assert_eq!(SEEN.with(|v| v.borrow().clone()), vec![h.0], "the observers read the set");
        assert_eq!(s.travel().is_empty(), s.selection().is_empty(), "afterwards it lives exactly as long as the selection");
        // Interactively: MOVE empties the selection mid-run; the observers still see the set.
        s.set_selection(vec![h]);
        s.set_travel(vec![h.0]);
        SEEN.with(|v| v.borrow_mut().clear());
        s.start("move").unwrap();
        for p in [(0.0, 0.0), (1.0, 1.0)] {
            s.input(crate::Input::Point(Vec2::new(p.0, p.1))).unwrap();
        }
        assert!(s.running.is_none(), "MOVE finished");
        assert_eq!(SEEN.with(|v| v.borrow().clone()), vec![h.0]);
        assert!(s.selection().is_empty() && s.travel().is_empty());
    }
}

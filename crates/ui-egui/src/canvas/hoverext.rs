//! A canvas hover observer (fork-only, LAMB T550): one registered function told the rollover
//! entity (`CanvasState::hover`) whenever it changes, with the open drawing. Unregistered, or no
//! drawing open, nothing happens. LAMB haloes the hovered entity's linked lines in its other views.
//!
//! A selection observer beside it (LAMB T616): one registered function told the session's
//! selection whenever it differs from the last one seen, checked in the same per-frame call. LAMB
//! highlights a selected detail tag's walls and placed detail.

use std::sync::{Mutex, OnceLock};

use cadcraft_doc::{Drawing, Handle};

use crate::CadApp;

/// Told the open drawing and the new rollover entity (`None`: nothing under the pointer).
pub type HoverObserver = fn(&Drawing, Option<Handle>);

static OBSERVER: OnceLock<HoverObserver> = OnceLock::new();

/// Register the observer. Once per process; a second call is an error.
pub fn register_hover_observer(f: HoverObserver) -> Result<(), &'static str> {
    OBSERVER.set(f).map_err(|_| "hover observer already registered")
}

/// Told the open drawing and the new selection (empty: nothing selected).
pub type SelectionObserver = fn(&Drawing, &[Handle]);

static SELECTION_OBSERVER: OnceLock<SelectionObserver> = OnceLock::new();

/// The selection the selection observer was last checked against.
static LAST_SELECTION: Mutex<Vec<Handle>> = Mutex::new(Vec::new());

/// Register the selection observer. Once per process; a second call is an error.
pub fn register_selection_observer(f: SelectionObserver) -> Result<(), &'static str> {
    SELECTION_OBSERVER.set(f).map_err(|_| "selection observer already registered")
}

/// Call the hover observer with `app.canvas.hover` when it differs from `before`, and the
/// selection observer with the session's selection when it differs from the last one seen.
pub(super) fn notify(app: &CadApp, before: Option<Handle>) {
    notify_with(OBSERVER.get().copied(), app, before);
    notify_selection_with(SELECTION_OBSERVER.get().copied(), app, &LAST_SELECTION);
}

fn notify_selection_with(f: Option<SelectionObserver>, app: &CadApp, last: &Mutex<Vec<Handle>>) {
    let Some(f) = f else { return };
    let now = app.session.selection();
    let mut seen = last.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if *seen == now {
        return;
    }
    seen.clone_from(&now);
    drop(seen);
    if let Ok(d) = app.session.doc() {
        f(d, &now);
    }
}

fn notify_with(f: Option<HoverObserver>, app: &CadApp, before: Option<Handle>) {
    let now = app.canvas.hover;
    if now == before {
        return;
    }
    if let (Some(f), Ok(d)) = (f, app.session.doc()) {
        f(d, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static SEEN: Mutex<Vec<Option<Handle>>> = Mutex::new(Vec::new());

    fn record(_: &Drawing, h: Option<Handle>) {
        SEEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(h);
    }

    fn seen() -> Vec<Option<Handle>> {
        SEEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    #[test]
    fn notifies_once_per_change_and_not_when_unchanged() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.session.new_drawing(true);
        let f: Option<HoverObserver> = Some(record);
        notify_with(f, &app, None);
        assert!(seen().is_empty(), "unchanged: no call");
        app.canvas.hover = Some(Handle(0x2A));
        notify_with(f, &app, None);
        assert_eq!(seen(), vec![Some(Handle(0x2A))]);
        notify_with(f, &app, Some(Handle(0x2A)));
        assert_eq!(seen().len(), 1, "same entity: no call");
        app.canvas.hover = None;
        notify_with(f, &app, Some(Handle(0x2A)));
        assert_eq!(seen(), vec![Some(Handle(0x2A)), None]);
        notify_with(None, &app, Some(Handle(1)));
        assert_eq!(seen().len(), 2, "unregistered: nothing");
    }

    static SELECTIONS: Mutex<Vec<Vec<Handle>>> = Mutex::new(Vec::new());

    fn record_selection(_: &Drawing, sel: &[Handle]) {
        SELECTIONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(sel.to_vec());
    }

    fn selections() -> Vec<Vec<Handle>> {
        SELECTIONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    #[test]
    fn selection_observer_fires_once_per_change() -> Result<(), Box<dyn std::error::Error>> {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.session.new_drawing(true);
        app.session.execute("line", &serde_json::json!({ "points": [[0, 0], [1000, 0]] }))?;
        let h = app.session.doc()?.model.iter().map(|e| e.handle).next().ok_or("no line")?;
        let last = Mutex::new(Vec::new());
        let f: Option<SelectionObserver> = Some(record_selection);
        notify_selection_with(f, &app, &last);
        assert!(selections().is_empty(), "unchanged: no call");
        app.session.set_selection(vec![h]);
        notify_selection_with(f, &app, &last);
        notify_selection_with(f, &app, &last);
        assert_eq!(selections(), vec![vec![h]], "once per change");
        app.session.set_selection(Vec::new());
        notify_selection_with(None, &app, &last);
        assert_eq!(selections().len(), 1, "unregistered: nothing");
        let last = Mutex::new(vec![h]);
        notify_selection_with(f, &app, &last);
        assert_eq!(selections(), vec![vec![h], Vec::new()]);
        Ok(())
    }
}

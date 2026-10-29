//! A canvas hover observer (fork-only, LAMB T550): one registered function told the rollover
//! entity (`CanvasState::hover`) whenever it changes, with the open drawing. Unregistered, or no
//! drawing open, nothing happens. LAMB haloes the hovered entity's linked lines in its other views.

use std::sync::OnceLock;

use cadcraft_doc::{Drawing, Handle};

use crate::CadApp;

/// Told the open drawing and the new rollover entity (`None`: nothing under the pointer).
pub type HoverObserver = fn(&Drawing, Option<Handle>);

static OBSERVER: OnceLock<HoverObserver> = OnceLock::new();

/// Register the observer. Once per process; a second call is an error.
pub fn register_hover_observer(f: HoverObserver) -> Result<(), &'static str> {
    OBSERVER.set(f).map_err(|_| "hover observer already registered")
}

/// Call the observer with `app.canvas.hover` when it differs from `before`.
pub(super) fn notify(app: &CadApp, before: Option<Handle>) {
    notify_with(OBSERVER.get().copied(), app, before);
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
}

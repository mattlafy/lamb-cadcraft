//! Extra status-bar toggles (fork-only, LAMB T360): one registered [`ToggleSource`] supplies small
//! text pills drawn left of the icon toggles, each lit when `on` and running its command when
//! clicked. It knows nothing of what the toggles are; the LAMB desktop app registers its PANELOCK
//! and UCSALIGN into it. Unregistered, nothing is drawn.

use std::sync::OnceLock;

use egui::{Rect, Sense, pos2, vec2};
use serde_json::Value;

use cadcraft_engine::Session;

use crate::CadApp;
use crate::theme::Tokens;

/// One status-bar pill.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusToggle {
    pub label: String,
    /// Drawn lit.
    pub on: bool,
    pub tip: String,
    /// The command (id, params) a click runs, transparently.
    pub command: (String, Value),
}

/// The toggles for a session.
pub type ToggleSource = fn(&Session) -> Vec<StatusToggle>;

static SOURCE: OnceLock<ToggleSource> = OnceLock::new();

/// Register the toggle source. Once per process; a second call is an error.
pub fn register_status_toggles(f: ToggleSource) -> Result<(), &'static str> {
    SOURCE.set(f).map_err(|_| "status toggle source already registered")
}

/// The toggles for the app's session; empty when nothing is registered.
pub fn toggles(app: &CadApp) -> Vec<StatusToggle> {
    SOURCE.get().map(|f| f(&app.session)).unwrap_or_default()
}

/// The name of a model-like block space (fork-only, LAMB T466): `Some(label)` means the session's
/// block space is a view called `label`, shown on the first layout tab and keeping the annotation
/// scale list.
pub type SpaceLabelSource = fn(&Session) -> Option<String>;

static SPACE_LABEL: OnceLock<SpaceLabelSource> = OnceLock::new();

/// Register the space-label source. Once per process; a second call is an error.
pub fn register_space_label(f: SpaceLabelSource) -> Result<(), &'static str> {
    SPACE_LABEL.set(f).map_err(|_| "space label source already registered")
}

/// The label of the app's current block space; None when nothing is registered.
pub fn space_label(app: &CadApp) -> Option<String> {
    SPACE_LABEL.get().and_then(|f| f(&app.session))
}

/// Run a toggle's command as a click does: transparently (the active command keeps running), its
/// `message` echoed.
pub fn activate(app: &mut CadApp, t: &StatusToggle) -> Result<Value, String> {
    let (id, params) = &t.command;
    let v = app.session.execute(id, params).map_err(|e| e.to_string())?;
    if let Some(m) = v.get("message").and_then(Value::as_str) {
        app.session.echo(m.to_string());
    }
    Ok(v)
}

/// Draw the registered toggles right to left ending at `right`; returns the new left edge, or
/// `right` when nothing is drawn.
pub fn status(app: &mut CadApp, ui: &mut egui::Ui, right: f32, cy: f32) -> f32 {
    let list = toggles(app);
    if list.is_empty() {
        return right;
    }
    let t = Tokens::get();
    let p = ui.painter().clone();
    let mut x = right;
    let mut clicked = None;
    for (i, tg) in list.iter().enumerate().rev() {
        let g = p.layout_no_wrap(tg.label.clone(), crate::theme::small(), t.text);
        let w = g.size().x + 12.0;
        let r = Rect::from_min_size(pos2(x - w, cy - 9.0), vec2(w, 18.0));
        let resp = ui.interact(r, ui.scope_id().with(("status_ext", i)), Sense::click());
        let fill = if resp.hovered() {
            t.control_hover
        } else if tg.on {
            t.toggle_on.gamma_multiply(0.6)
        } else {
            t.control.gamma_multiply(0.5)
        };
        p.rect_filled(r, 3.0, fill);
        p.galley(pos2(r.center().x - g.size().x / 2.0, r.center().y - g.size().y / 2.0), g, t.text);
        crate::icons::describe_control(ui, &resp, &tg.label, Some(tg.on), false);
        if resp.on_hover_text(&tg.tip).clicked() {
            clicked = Some(i);
        }
        x = r.left() - 4.0;
    }
    if let Some(i) = clicked
        && let Err(e) = activate(app, &list[i])
    {
        app.status = Some((e, crate::now_ms()));
    }
    x - 2.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source(s: &Session) -> Vec<StatusToggle> {
        vec![StatusToggle { label: "ORTH".into(), on: s.settings.orthomode, tip: "Ortho".into(), command: ("ortho".into(), json!({})) }]
    }

    #[test]
    fn a_registered_toggle_is_listed_and_activating_it_runs_its_command() {
        let _ = register_status_toggles(source);
        assert!(register_status_toggles(source).is_err(), "one source per process");
        let mut app = CadApp::new(Session::default(), crate::Services::default());
        let before = app.session.settings.orthomode;
        let rows = toggles(&app);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].on, before);
        assert!(activate(&mut app, &rows[0]).is_ok());
        assert_ne!(app.session.settings.orthomode, before, "ortho flipped");
        assert_eq!(toggles(&app)[0].on, !before, "the pill follows the setting");
    }

    #[test]
    fn clicking_a_rendered_pill_runs_its_command() {
        let _ = register_status_toggles(source);
        let app = CadApp::new(Session::default(), crate::Services::default());
        let before = app.session.settings.orthomode;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(300.0, 40.0)).build_ui_state(
            |ui, app: &mut CadApp| {
                let left = status(app, ui, 290.0, 20.0);
                assert!(left < 290.0, "something was drawn");
            },
            app,
        );
        h.run();
        use egui_kittest::kittest::Queryable;
        h.get_by_label("ORTH").click();
        h.run();
        assert_ne!(h.state().session.settings.orthomode, before);
    }
}

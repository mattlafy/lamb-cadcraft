//! The status bar's annotation scale list (model space): it shows the current annotation scale
//! (CANNOSCALE) and sets it from the drawing's scale list through `setvar`; its last entry starts
//! SCALELISTEDIT.

use egui::{Rect, Sense, pos2, vec2};
use serde_json::json;

use crate::CadApp;
use crate::theme::Tokens;

/// Whether the list is drawn: in model space, or in a block space a registered space label names
/// as a model-like view (fork-only, LAMB T466).
fn shown(space: &cadcraft_doc::Space, view_label: Option<&str>) -> bool {
    match space {
        cadcraft_doc::Space::Model => true,
        cadcraft_doc::Space::Block(_) => view_label.is_some(),
        cadcraft_doc::Space::Paper(_) => false,
    }
}

/// Draw the annotation scale list ending at `right` (model space, or a labelled view); returns
/// its left edge, or `right` when nothing is drawn.
pub fn status(app: &mut CadApp, ui: &mut egui::Ui, right: f32, cy: f32) -> f32 {
    let space = app.session.layout_space();
    if !matches!(space, cadcraft_doc::Space::Model) && !shown(&space, super::statusext::space_label(app).as_deref()) {
        return right;
    }
    let Ok(d) = app.session.doc() else { return right };
    let current = d.current_anno_scale().name;
    let names: Vec<String> = d.scales.iter().map(|s| s.name.clone()).collect();
    let t = Tokens::get();
    let p = ui.painter().clone();
    let g = p.layout_no_wrap(current.clone(), crate::theme::small(), t.text);
    let w = g.size().x.min(160.0) + 22.0;
    let r = Rect::from_min_size(pos2(right - w, cy - 9.0), vec2(w, 18.0));
    let resp = ui.interact(r, ui.id().with("anno_scale"), Sense::click());
    p.rect_filled(r, 3.0, if resp.hovered() { t.control_hover } else { t.control.gamma_multiply(0.5) });
    p.with_clip_rect(r.shrink2(vec2(4.0, 0.0))).galley(pos2(r.left() + 6.0, r.center().y - g.size().y / 2.0), g, t.text);
    let (ax, ay) = (r.right() - 9.0, r.center().y);
    p.line_segment([pos2(ax - 3.0, ay - 1.5), pos2(ax, ay + 1.5)], egui::Stroke::new(1.2, t.text_dim));
    p.line_segment([pos2(ax, ay + 1.5), pos2(ax + 3.0, ay - 1.5)], egui::Stroke::new(1.2, t.text_dim));
    crate::icons::describe_control(ui, &resp, "Annotation scale", None, false);
    let resp = resp.on_hover_text(crate::tl!("Annotation scale"));
    let mut pick: Option<String> = None;
    let mut edit = false;
    egui::Popup::from_toggle_button_response(&resp).width(170.0).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            for n in &names {
                if ui.selectable_label(n.eq_ignore_ascii_case(&current), n).clicked() {
                    pick = Some(n.clone());
                    ui.close();
                }
            }
            ui.separator();
            if ui.selectable_label(false, crate::i18n::t("Scale List...")).clicked() {
                edit = true;
                ui.close();
            }
        });
    });
    if let Some(n) = pick {
        let res = app.run("setvar", json!({ "name": "CANNOSCALE", "value": n }));
        app.canvas.list = None;
        if let Err(e) = res {
            app.status = Some((e, crate::now_ms()));
        }
    }
    if edit {
        app.start("scalelistedit");
    }
    r.left() - 6.0
}

#[cfg(test)]
mod tests {
    use super::shown;
    use cadcraft_doc::Space;

    #[test]
    fn the_list_shows_in_model_space_and_on_a_labelled_view_only() {
        assert!(shown(&Space::Model, None));
        assert!(!shown(&Space::Paper("Layout1".into()), Some("Plan 1")));
        assert!(!shown(&Space::Block("DOOR".into()), None), "BEDIT keeps it hidden");
        assert!(shown(&Space::Block("LAMB_VIEW_1".into()), Some("Plan 1")));
    }
}

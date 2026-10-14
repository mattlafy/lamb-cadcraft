//! A scale bar HUD in the bottom-left corner of each model/block-space tile (fork-only, LAMB T363):
//! a bar spanning a round real length, ticked at both ends and the midpoint, labelled in mm or m.
//! Ported 1:1 from the TypeScript app's `screenMarks.ts` (`chooseScaleBarLengthMm`,
//! `scaleBarLabel`, `drawScaleBar`). Drawn only when a registered predicate says the drawing wants
//! it (LAMB: the drawing has views), so plain CADCraft drawings are unchanged. Drawing units are mm.

use std::sync::OnceLock;

use cadcraft_doc::Drawing;
use egui::{Color32, Rect, Stroke, pos2};

use crate::CadApp;

/// Round lengths the bar may span, in drawing units (mm).
const STEPS: [f64; 10] = [100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0, 20_000.0, 50_000.0, 100_000.0];
const MAX_PX: f64 = 180.0;
const MIN_PX: f64 = 40.0;
const INSET_PX: f32 = 16.0;
const TICK_PX: f32 = 8.0;

/// Whether a drawing shows the scale bar.
pub type ScaleBarPredicate = fn(&Drawing) -> bool;

static WANTED: OnceLock<ScaleBarPredicate> = OnceLock::new();

/// Register the predicate. Once per process; a second call is an error.
pub fn register_scale_bar(f: ScaleBarPredicate) -> Result<(), &'static str> {
    WANTED.set(f).map_err(|_| "scale bar predicate already registered")
}

/// True when a predicate is registered, a drawing is open and the predicate holds.
pub(super) fn wanted(app: &CadApp) -> bool {
    WANTED.get().is_some_and(|f| app.session.doc().is_ok_and(*f))
}

/// The largest step whose on-screen width is at most `MAX_PX` at `scale` px per mm; none fits →
/// the smallest; the chosen width under `MIN_PX` → the largest; a degenerate scale → the smallest.
pub fn length_mm(scale: f64) -> f64 {
    let (smallest, largest) = (STEPS[0], STEPS[STEPS.len() - 1]);
    if !scale.is_finite() || scale <= 0.0 {
        return smallest;
    }
    let Some(chosen) = STEPS.iter().copied().rfind(|mm| mm * scale <= MAX_PX) else { return smallest };
    if chosen * scale < MIN_PX { largest } else { chosen }
}

/// Millimetres below a metre, metres (one decimal, a trailing `.0` trimmed) at or above.
pub fn label(mm: f64) -> String {
    if mm < 1000.0 {
        return format!("{mm}mm");
    }
    let m = format!("{:.1}", mm / 1000.0);
    format!("{}m", m.strip_suffix(".0").unwrap_or(&m))
}

/// The bar in `rect`'s bottom-left corner at `scale` px per mm.
pub(super) fn draw(p: &egui::Painter, rect: Rect, scale: f64, color: Color32) {
    let mm = length_mm(scale);
    let x0 = rect.left() + INSET_PX;
    let y = rect.bottom() - INSET_PX;
    let x1 = (x0 + (mm * scale) as f32).min(rect.right() - INSET_PX);
    let st = Stroke::new(1.0, color);
    p.line_segment([pos2(x0, y), pos2(x1, y)], st);
    for (x, tick) in [(x0, TICK_PX), ((x0 + x1) / 2.0, TICK_PX / 2.0), (x1, TICK_PX)] {
        p.line_segment([pos2(x, y), pos2(x, y - tick)], st);
    }
    p.text(pos2(x1 + 6.0, y), egui::Align2::LEFT_CENTER, label(mm), crate::theme::body(), color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_largest_step_that_fits() {
        assert_eq!(length_mm(0.3), 500.0); // 150 px fits, 300 px does not
        assert_eq!(length_mm(0.08), 2000.0);
        assert_eq!(length_mm(1.0), 100.0);
        assert_eq!(length_mm(0.1), 1000.0);
    }

    #[test]
    fn stays_inside_the_band_across_a_wide_zoom_range() {
        let mut scale = 0.002;
        while scale < 1.5 {
            let px = length_mm(scale) * scale;
            assert!((MIN_PX..=MAX_PX).contains(&px), "{px} px at scale {scale}");
            scale *= 1.15;
        }
    }

    #[test]
    fn clamps_at_both_ends_and_survives_a_degenerate_scale() {
        assert_eq!(length_mm(5.0), 100.0);
        assert_eq!(length_mm(0.0001), 100_000.0);
        assert_eq!(length_mm(0.0), 100.0);
        assert_eq!(length_mm(f64::NAN), 100.0);
        assert_eq!(length_mm(f64::INFINITY), 100.0);
    }

    #[test]
    fn labels_in_mm_below_a_metre_and_m_above() {
        assert_eq!(label(100.0), "100mm");
        assert_eq!(label(500.0), "500mm");
        assert_eq!(label(1000.0), "1m");
        assert_eq!(label(2000.0), "2m");
        assert_eq!(label(2500.0), "2.5m");
        assert_eq!(label(100_000.0), "100m");
    }
}

//! A scale bar HUD in the bottom-left corner of each model/block-space tile (fork-only, LAMB T363):
//! a bar spanning a round real length, ticked at both ends and the midpoint, labelled in mm, m or km.
//! Ported 1:1 from the TypeScript app's `screenMarks.ts` (`chooseScaleBarLengthMm`,
//! `scaleBarLabel`, `drawScaleBar`). Drawn only when a registered predicate says the drawing wants
//! it (LAMB: the drawing has views), so plain CADCraft drawings are unchanged. Drawing units are mm.

use std::sync::OnceLock;

use cadcraft_doc::Drawing;
use egui::{Color32, Rect, Stroke, pos2};

use crate::CadApp;

const MAX_PX: f64 = 180.0;
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

/// The largest value of the unbounded 1-2-5 series (… 0.1, 0.2, 0.5, 1, 2, 5, 10 … mm) whose
/// on-screen width is at most `MAX_PX` at `scale` px per mm. Consecutive steps differ by at most
/// 2.5x and `MAX_PX` / 40 px is 4.5, so the width never drops under 40 px. A degenerate scale
/// gives 100 mm.
pub fn length_mm(scale: f64) -> f64 {
    if !scale.is_finite() || scale <= 0.0 {
        return 100.0;
    }
    let limit = MAX_PX / scale;
    if !limit.is_finite() || limit <= 0.0 {
        return 100.0;
    }
    let mut exp = limit.log10().floor() as i32;
    // `log10` can land a hair off at exact powers of ten; nudge so 10^exp <= limit < 10^(exp+1).
    if decade(exp + 1, 1.0) <= limit {
        exp += 1;
    } else if decade(exp, 1.0) > limit {
        exp -= 1;
    }
    [5.0, 2.0, 1.0].into_iter().map(|m| decade(exp, m)).find(|mm| *mm <= limit).unwrap_or(decade(exp, 1.0))
}

/// `m * 10^exp`, computed so negative exponents stay correctly rounded (`5 / 100`, not `5 * 0.01`).
fn decade(exp: i32, m: f64) -> f64 {
    if exp >= 0 { m * 10f64.powi(exp) } else { m / 10f64.powi(-exp) }
}

/// Millimetres below a metre, metres below a kilometre, kilometres at or above; no trailing `.0`.
pub fn label(mm: f64) -> String {
    if mm < 1000.0 {
        format!("{mm}mm")
    } else if mm < 1_000_000.0 {
        format!("{}m", mm / 1000.0)
    } else {
        format!("{}km", mm / 1_000_000.0)
    }
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

    /// The narrowest the bar ever gets: guaranteed by the series, not clamped.
    const MIN_PX: f64 = 40.0;

    #[test]
    fn picks_the_largest_step_that_fits() {
        assert_eq!(length_mm(0.3), 500.0); // 150 px fits, 300 px does not
        assert_eq!(length_mm(0.08), 2000.0);
        assert_eq!(length_mm(1.0), 100.0);
        assert_eq!(length_mm(0.1), 1000.0);
    }

    #[test]
    fn stays_inside_the_band_across_a_wide_zoom_range() {
        let mut scale = 1e-7;
        while scale < 1e4 {
            let px = length_mm(scale) * scale;
            assert!((MIN_PX..=MAX_PX).contains(&px), "{px} px at scale {scale}");
            scale *= 1.15;
        }
        for scale in [10.0, 1.0, 0.1, 0.001, 1e-6] {
            let px = length_mm(scale) * scale;
            assert!((MIN_PX..=MAX_PX).contains(&px), "{px} px at scale {scale}");
        }
    }

    #[test]
    fn has_no_minimum_or_maximum_and_survives_a_degenerate_scale() {
        assert_eq!(length_mm(10.0), 10.0);
        assert_eq!(length_mm(5.0), 20.0);
        assert_eq!(length_mm(1e-5), 10_000_000.0);
        assert_eq!(length_mm(1.8), 100.0); // exactly MAX_PX
        assert_eq!(length_mm(500.0), 0.2);
        assert_eq!(length_mm(0.0), 100.0);
        assert_eq!(length_mm(-1.0), 100.0);
        assert_eq!(length_mm(f64::NAN), 100.0);
        assert_eq!(length_mm(f64::INFINITY), 100.0);
    }

    #[test]
    fn labels_in_mm_m_and_km() {
        assert_eq!(label(0.5), "0.5mm");
        assert_eq!(label(100.0), "100mm");
        assert_eq!(label(500.0), "500mm");
        assert_eq!(label(1000.0), "1m");
        assert_eq!(label(2000.0), "2m");
        assert_eq!(label(500_000.0), "500m");
        assert_eq!(label(1_000_000.0), "1km");
        assert_eq!(label(5_000_000.0), "5km");
        assert_eq!(label(20_000_000.0), "20km");
    }
}

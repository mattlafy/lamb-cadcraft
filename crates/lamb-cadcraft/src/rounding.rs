//! Virtual corner tags drawn rounded: CADCraft's render display hook (`register_display_kind`,
//! fork patch 11). An `LwPolyline` whose `Common.ext["LAMB"].corners` holds tags draws — on
//! screen, through sheet viewports and in plots — as its resolved shape (fillet arcs, chamfer
//! cuts) while its stored vertices, and so snaps, stay sharp. A polyline with width, an
//! unreadable slot or a tag the geometry can't admit draws as stored.

use cadcraft_doc::{Drawing, Entity, EntityKind};
use cadcraft_render::register_display_kind;
use lamb_core::geometry::fillet::resolve_virtual_polyline;

use crate::convert::{apply_corners, from_polyline, to_polyline};
use crate::schema::{EXT_KEY, read_entity_data};

/// The hook: `Some(kind)` to draw `e` as its resolved virtual corners, `None` to draw it as stored.
fn display_kind(_d: &Drawing, e: &Entity) -> Option<EntityKind> {
    let EntityKind::LwPolyline(pl) = &e.kind else { return None };
    if !e.common.ext.contains_key(EXT_KEY) {
        return None;
    }
    if pl.const_width != 0.0 || pl.vertices.iter().any(|v| v.start_width != 0.0 || v.end_width != 0.0) {
        return None;
    }
    let data = read_entity_data(&e.common).ok()?;
    if data.corners.is_empty() {
        return None;
    }
    let mut poly = to_polyline(e).ok()?.polyline;
    if apply_corners(&mut poly, &data.corners) == 0 {
        return None;
    }
    let (mut kind, _) = from_polyline(&resolve_virtual_polyline(&poly));
    if let EntityKind::LwPolyline(out) = &mut kind {
        out.elevation = pl.elevation;
        out.plinegen = pl.plinegen;
    }
    Some(kind)
}

/// Register the hook with CADCraft's renderer. Once per process; a second call is an error.
pub fn register() -> Result<(), &'static str> {
    register_display_kind(display_kind)
}

#[cfg(test)]
mod tests {
    //! The hook is process-global: every test calls [`crate::commands::install_app`] first,
    //! which registers once. Evidence is the display list's points — what the canvas paints.
    use cadcraft_doc::{Common, Handle, LwPolyline, Space};
    use cadcraft_geom::{PolyVertex, Vec2};
    use cadcraft_render::{DisplayList, Options, build, build_plot};
    use lamb_core::geometry::VirtualCornerEntry;
    use lamb_core::geometry::polyline::CornerEntry;

    use super::*;
    use crate::schema::{EntityData, set_entity_data};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type Res<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    const L: [(f64, f64); 3] = [(0.0, 0.0), (100.0, 0.0), (100.0, 80.0)];

    fn drawing(tag: Option<VirtualCornerEntry>, const_width: f64) -> Res<(Drawing, Handle)> {
        crate::commands::install_app()?;
        let mut d = Drawing::new_metric();
        let mut common = Common::default();
        if let Some(entry) = tag {
            set_entity_data(&mut common, &EntityData { corners: vec![CornerEntry { corner_index: 1, entry }], ..EntityData::default() })?;
        }
        let vertices = L.iter().map(|&(x, y)| PolyVertex::new(Vec2::new(x, y))).collect();
        let kind = EntityKind::LwPolyline(LwPolyline { vertices, closed: false, const_width, elevation: 0.0, plinegen: false });
        let h = d.add(&Space::Model, common, kind)?;
        Ok((d, h))
    }

    fn points(list: &DisplayList, h: Handle) -> Vec<Vec2> {
        list.prims.iter().filter(|p| p.handle == h).flat_map(|p| list.points(p).to_vec()).collect()
    }

    fn near(pts: &[Vec2], x: f64, y: f64) -> bool {
        pts.iter().any(|p| (p.x - x).abs() < 1e-6 && (p.y - y).abs() < 1e-6)
    }

    fn sharp(pts: &[Vec2]) -> bool {
        L.iter().all(|&(x, y)| near(pts, x, y)) && pts.iter().all(|p| L.iter().any(|&(x, y)| (p.x - x).abs() < 1e-6 && (p.y - y).abs() < 1e-6))
    }

    fn stored(d: &Drawing, h: Handle) -> Res<Vec<(f64, f64)>> {
        match &d.entity(h).ok_or("no entity")?.kind {
            EntityKind::LwPolyline(p) => Ok(p.vertices.iter().map(|v| (v.p.x, v.p.y)).collect()),
            _ => Err("not a polyline".into()),
        }
    }

    fn assert_filleted(pts: &[Vec2], tol: f64) {
        assert!(!near(pts, 100.0, 0.0), "the sharp corner is gone");
        assert!(near(pts, 90.0, 0.0) && near(pts, 100.0, 10.0), "tangent points drawn: {pts:?}");
        let mut on_arc = 0;
        for p in pts.iter().filter(|p| p.x > 90.0 + 1e-6 && p.y < 10.0 - 1e-6) {
            let r = ((p.x - 90.0).powi(2) + (p.y - 10.0).powi(2)).sqrt();
            assert!((r - 10.0).abs() <= tol, "{p:?} is {r} from the centre");
            on_arc += 1;
        }
        assert!(on_arc > 0, "the arc is tessellated");
    }

    #[test]
    fn fillet_tag_draws_an_arc_in_build_and_build_plot() -> R {
        let (d, h) = drawing(Some(VirtualCornerEntry::Fillet { radius: 10.0 }), 0.0)?;
        let opts = Options::default();
        assert_filleted(&points(&build(&d, &Space::Model, &opts), h), opts.tolerance + 1e-6);
        assert_filleted(&points(&build_plot(&d, &Space::Model, &opts), h), opts.tolerance + 1e-6);
        assert_eq!(stored(&d, h)?, L.to_vec(), "stored vertices stay sharp");
        Ok(())
    }

    #[test]
    fn chamfer_tag_draws_the_straight_cut() -> R {
        let (d, h) = drawing(Some(VirtualCornerEntry::Chamfer { distance1: 10.0, distance2: 20.0 }), 0.0)?;
        for list in [build(&d, &Space::Model, &Options::default()), build_plot(&d, &Space::Model, &Options::default())] {
            let pts = points(&list, h);
            assert!(!near(&pts, 100.0, 0.0));
            assert!(near(&pts, 90.0, 0.0) && near(&pts, 100.0, 20.0), "{pts:?}");
            let cut = pts.iter().filter(|p| p.x > 90.0 + 1e-6 && p.y < 20.0 - 1e-6);
            assert!(cut.clone().all(|p| (2.0 * (p.x - 90.0) - p.y).abs() < 1e-6), "every corner point on the cut: {pts:?}");
        }
        assert_eq!(stored(&d, h)?, L.to_vec());
        Ok(())
    }

    #[test]
    fn untagged_width_and_infeasible_draw_sharp() -> R {
        for (tag, w) in
            [(None, 0.0), (Some(VirtualCornerEntry::Fillet { radius: 10.0 }), 5.0), (Some(VirtualCornerEntry::Fillet { radius: 200.0 }), 0.0)]
        {
            let (d, h) = drawing(tag, w)?;
            let pts = points(&build(&d, &Space::Model, &Options::default()), h);
            if w == 0.0 {
                assert!(sharp(&pts), "{tag:?}: {pts:?}");
            } else {
                // A wide polyline draws as filled outlines: the same as the untagged one.
                let (plain, ph) = drawing(None, w)?;
                assert_eq!(pts, points(&build(&plain, &Space::Model, &Options::default()), ph));
                assert_eq!(display_kind(&d, d.entity(h).ok_or("no entity")?), None);
            }
            assert_eq!(stored(&d, h)?, L.to_vec());
        }
        Ok(())
    }
}

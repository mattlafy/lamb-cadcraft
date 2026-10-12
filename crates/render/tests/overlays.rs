//! Overlay providers (fork patch 6): display lists from a crate outside the renderer, separate
//! from `build`. Its own test binary because the provider registry is process-global.

use cadcraft_color::Rgb;
use cadcraft_doc::{Common, Drawing, EntityKind, Handle, Line, Space};
use cadcraft_geom::{Bounds2, Vec2, Vec3};
use cadcraft_render::{DPrim, DisplayList, Kind, Options, build, overlays, register_overlay_provider};

fn marker(at: Vec2, handle: u64) -> DisplayList {
    let prim = DPrim { handle: Handle(handle), color: Rgb(255, 0, 0), aci7: false, lw: 0.0, kind: Kind::Point, start: 0, len: 1 };
    DisplayList { prims: vec![prim], verts: vec![at], bounds: Bounds2::from_points([at]), ..Default::default() }
}

fn first(_: &Drawing, _: &Space, _: &Options) -> DisplayList {
    marker(Vec2::new(1.0, 2.0), 1)
}

fn empty(_: &Drawing, _: &Space, _: &Options) -> DisplayList {
    DisplayList::default()
}

fn second(_: &Drawing, _: &Space, _: &Options) -> DisplayList {
    marker(Vec2::new(3.0, 4.0), 2)
}

#[test]
fn overlays_come_back_per_provider_and_leave_build_alone() {
    let mut d = Drawing::new_metric();
    d.add(&Space::Model, Common::default(), EntityKind::Line(Line { a: Vec3::new(0.0, 0.0, 0.0), b: Vec3::new(10.0, 5.0, 0.0) })).unwrap();
    let opts = Options::default();
    let before = format!("{:?}", build(&d, &Space::Model, &opts));

    assert!(overlays(&d, &Space::Model, &opts).is_empty());

    register_overlay_provider(first).unwrap();
    register_overlay_provider(empty).unwrap();
    register_overlay_provider(second).unwrap();
    let lists = overlays(&d, &Space::Model, &opts);
    let handles: Vec<u64> = lists.iter().map(|l| l.prims.first().map_or(0, |p| p.handle.0)).collect();
    assert_eq!(handles, vec![1, 2], "registration order, empty list dropped");
    assert_eq!(lists.first().map(|l| l.verts.clone()), Some(vec![Vec2::new(1.0, 2.0)]));

    assert_eq!(format!("{:?}", build(&d, &Space::Model, &opts)), before, "build is unchanged by overlay providers");
}

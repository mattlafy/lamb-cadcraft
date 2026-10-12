//! Snap providers (fork patch 6): hits from a crate outside the engine compete in `osnap`.
//! Its own test binary because the provider registry is process-global.

use std::sync::Mutex;

use cadcraft_engine::Session;
use cadcraft_engine::doc::{Drawing, Space};
use cadcraft_engine::geom::Vec2;
use cadcraft_engine::pointmod::snap_candidates;
use cadcraft_engine::snap::{self, SnapHit, mode, register_snap_provider};
use serde_json::json;

/// The points the registered provider returns, set by the test step by step.
static POINTS: Mutex<Vec<Vec2>> = Mutex::new(Vec::new());

fn provider(_: &Drawing, _: &Space, _: Vec2, _: f64) -> Vec<SnapHit> {
    let pts = POINTS.lock().map(|v| v.clone()).unwrap_or_default();
    // The provider claims a built-in mode; the engine must overwrite it.
    pts.into_iter().map(|point| SnapHit { point, mode: mode::END, name: "Marker", viewport: None, deferred: None }).collect()
}

fn set(points: &[Vec2]) {
    *POINTS.lock().unwrap() = points.to_vec();
}

#[test]
fn provider_hits_compete_with_builtin_snaps() {
    let mut s = Session::new();
    s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap();
    let d = s.doc().unwrap();
    let space = Space::Model;
    let cursor = Vec2::new(1.0, 0.5);
    let aperture = 2.0;
    let snap = |osmode: u32| snap::osnap(d, &space, cursor, aperture, osmode, None, false);

    // No provider yet: the line's endpoint.
    let hit = snap(mode::END).unwrap();
    assert_eq!((hit.point, hit.mode), (Vec2::new(0.0, 0.0), mode::END));

    register_snap_provider(provider).unwrap();

    // A provider hit nearer the cursor than the endpoint wins, as PROVIDER, with its own name.
    set(&[Vec2::new(1.0, 0.2)]);
    let hit = snap(mode::END).unwrap();
    assert_eq!((hit.point, hit.mode, hit.name), (Vec2::new(1.0, 0.2), mode::PROVIDER, "Marker"));

    // Farther than the endpoint (but inside the aperture): the endpoint wins.
    set(&[Vec2::new(2.5, 0.5)]);
    assert_eq!(snap(mode::END).unwrap().mode, mode::END);

    // Outside the aperture, or not finite: ignored, even when it is the only candidate.
    set(&[Vec2::new(5.0, 5.0), Vec2::new(f64::NAN, 0.5), Vec2::new(1.0, f64::INFINITY)]);
    assert_eq!(snap(mode::END).unwrap().mode, mode::END);
    let far = snap::osnap(d, &space, Vec2::new(5.0, 4.0), 0.5, mode::END, None, false);
    assert!(far.is_none(), "{far:?}");

    // Running snaps off: no provider either.
    set(&[Vec2::new(1.0, 0.2)]);
    assert!(snap(mode::OFF).is_none());
    assert!(snap(mode::END | mode::OFF).is_none());

    // snap_candidates lists the provider hit once (it comes back from every per-mode osnap).
    let cands = snap_candidates(d, &space, cursor, aperture, mode::END | mode::MID, None, false);
    assert_eq!(cands.iter().filter(|h| h.mode == mode::PROVIDER).count(), 1, "{cands:?}");
    assert_eq!(cands.first().map(|h| h.mode), Some(mode::PROVIDER));
}

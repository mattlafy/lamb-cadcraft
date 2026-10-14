//! A dockable 3D pane (fork-only, LAMB T340): draws whatever [`Scene3`] one registered
//! [`SceneSource`] builds from the drawing, with an orbit camera (LAMB's `Viewport3D.tsx`: 50°
//! perspective, up +Z), and a click picks a mesh back to its source entities. Drawn with egui's
//! painter only: CPU projection, far-to-near sorted triangles, flat shading. It knows nothing of
//! what the scene is; the LAMB desktop app registers its derived model into it.

use std::sync::{Arc, OnceLock};

use egui::{Color32, Pos2, Rect, Sense, Stroke, pos2};

use cadcraft_doc::{Drawing, Handle};

use crate::CadApp;
use crate::theme::Tokens;

type V3 = [f64; 3];

/// A scene: meshes and paths whose positions are offsets from `origin` (global mm).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene3 {
    pub origin: [f64; 3],
    pub meshes: Vec<Mesh3>,
    pub paths: Vec<Path3>,
}

/// An indexed triangle mesh, one colour; `sources` are the entities a click on it selects.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh3 {
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    pub rgba: [u8; 4],
    /// Draw every triangle's edges in this colour on top.
    pub edges_rgba: Option<[u8; 4]>,
    pub sources: Vec<Handle>,
}

/// A polyline.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path3 {
    pub points: Vec<[f32; 3]>,
    pub rgba: [u8; 4],
}

/// The registered scene: `build` the scene for a drawing; `pick` the entities a ray (global
/// origin, direction) hits, nearest first.
#[derive(Clone, Copy)]
pub struct SceneSource {
    pub build: fn(&Drawing) -> Option<Scene3>,
    pub pick: fn(&Drawing, [f64; 3], [f64; 3]) -> Vec<Handle>,
}

static SOURCE: OnceLock<SceneSource> = OnceLock::new();

/// Register the scene source. Once per process; a second call is an error.
pub fn register_scene_source(s: SceneSource) -> Result<(), &'static str> {
    SOURCE.set(s).map_err(|_| "3D scene source already registered")
}

// ---- vector helpers ----

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn mul(a: V3, k: f64) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn len(a: V3) -> f64 {
    dot(a, a).sqrt()
}
fn norm(a: V3) -> V3 {
    let l = len(a);
    if l > 0.0 { mul(a, 1.0 / l) } else { a }
}

// ---- camera ----

/// Vertical field of view, near and far planes (the TS app's `PerspectiveCamera(50, …, 10, 200000)`).
pub const FOV_DEG: f64 = 50.0;
pub const NEAR: f64 = 10.0;
pub const FAR: f64 = 200_000.0;
const UP: V3 = [0.0, 0.0, 1.0];
/// The TS directional light's position (light comes from there).
const LIGHT: V3 = [5000.0, -8000.0, 10000.0];
/// A press that moved less than this is a click (px).
const CLICK_SLOP: f32 = 3.0;

/// An orbit camera in global mm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub eye: V3,
    pub target: V3,
}

impl Camera {
    /// Forward, right and up unit vectors.
    fn basis(&self) -> (V3, V3, V3) {
        let f = norm(sub(self.target, self.eye));
        let mut r = cross(f, UP);
        if len(r) < 1e-9 {
            r = [1.0, 0.0, 0.0];
        }
        let r = norm(r);
        (f, r, cross(r, f))
    }
    fn distance(&self) -> f64 {
        len(sub(self.eye, self.target))
    }

    /// Fit a box (global min, max) into `rect`: target at its centre, eye along (1, −1.5, 1).
    pub fn fit(min: V3, max: V3, rect: Rect) -> Camera {
        let centre = mul(add(min, max), 0.5);
        let radius = (len(sub(max, min)) * 0.5).max(1.0);
        let t = (FOV_DEG.to_radians() * 0.5).tan();
        let aspect = aspect(rect);
        // The narrower of the two half-angles must see the bounding sphere.
        let half = t.min(t * aspect).atan();
        let d = (radius / half.sin()).max(radius + NEAR * 2.0);
        Camera { target: centre, eye: add(centre, mul(norm([1.0, -1.5, 1.0]), d)) }
    }

    /// Left drag: orbit about the target by a screen delta; the distance is kept.
    pub fn orbit(&mut self, delta: egui::Vec2) {
        let o = sub(self.eye, self.target);
        let r = len(o);
        let theta = o[1].atan2(o[0]) - f64::from(delta.x) * 0.01;
        let phi = ((o[2] / r).clamp(-1.0, 1.0).acos() - f64::from(delta.y) * 0.01).clamp(0.01, std::f64::consts::PI - 0.01);
        self.eye = add(self.target, [r * phi.sin() * theta.cos(), r * phi.sin() * theta.sin(), r * phi.cos()]);
    }

    /// Move the eye towards (factor < 1) or away from the target.
    pub fn dolly(&mut self, factor: f64) {
        let o = sub(self.eye, self.target);
        let d = (len(o) * factor).clamp(NEAR * 5.0, FAR * 0.5);
        self.eye = add(self.target, mul(norm(o), d));
    }

    /// Right drag: slide eye and target so the point under the cursor follows it.
    pub fn pan(&mut self, delta: egui::Vec2, rect: Rect) {
        let (_, r, u) = self.basis();
        let per_px = 2.0 * self.distance() * (FOV_DEG.to_radians() * 0.5).tan() / f64::from(rect.height().max(1.0));
        let m = add(mul(r, -f64::from(delta.x) * per_px), mul(u, f64::from(delta.y) * per_px));
        self.eye = add(self.eye, m);
        self.target = add(self.target, m);
    }
}

fn aspect(rect: Rect) -> f64 {
    f64::from(rect.width().max(1.0)) / f64::from(rect.height().max(1.0))
}

/// The column-major view-projection matrix (OpenGL clip space) for `cam` at `aspect`.
pub fn view_proj(cam: &Camera, aspect: f64) -> [[f64; 4]; 4] {
    let (f, r, u) = cam.basis();
    let e = cam.eye;
    // View: rows r, u, −f with translation.
    let view = [[r[0], u[0], -f[0], 0.0], [r[1], u[1], -f[1], 0.0], [r[2], u[2], -f[2], 0.0], [-dot(r, e), -dot(u, e), dot(f, e), 1.0]];
    let t = 1.0 / (FOV_DEG.to_radians() * 0.5).tan();
    let proj = [
        [t / aspect, 0.0, 0.0, 0.0],
        [0.0, t, 0.0, 0.0],
        [0.0, 0.0, (FAR + NEAR) / (NEAR - FAR), -1.0],
        [0.0, 0.0, 2.0 * FAR * NEAR / (NEAR - FAR), 0.0],
    ];
    let mut m = [[0.0; 4]; 4];
    for (c, col) in m.iter_mut().enumerate() {
        for (row, out) in col.iter_mut().enumerate() {
            *out = (0..4).map(|k| proj[k][row] * view[c][k]).sum();
        }
    }
    m
}

/// A point's screen position and its depth (distance in front of the eye along the view), or
/// `None` in front of the near plane or past the far plane.
pub fn project(cam: &Camera, rect: Rect, p: V3) -> Option<(Pos2, f64)> {
    let m = view_proj(cam, aspect(rect));
    let c: [f64; 4] = std::array::from_fn(|row| m[0][row] * p[0] + m[1][row] * p[1] + m[2][row] * p[2] + m[3][row]);
    let w = c[3];
    if !(NEAR..=FAR).contains(&w) {
        return None;
    }
    Some((ndc_to_screen(rect, c[0] / w, c[1] / w), w))
}

fn ndc_to_screen(rect: Rect, x: f64, y: f64) -> Pos2 {
    let c = rect.center();
    pos2(c.x + (x * f64::from(rect.width()) * 0.5) as f32, c.y - (y * f64::from(rect.height()) * 0.5) as f32)
}

/// The ray (eye, unit direction) through a screen position.
pub fn ray(cam: &Camera, rect: Rect, screen: Pos2) -> (V3, V3) {
    let (f, r, u) = cam.basis();
    let c = rect.center();
    let x = f64::from(screen.x - c.x) / (f64::from(rect.width().max(1.0)) * 0.5);
    let y = -f64::from(screen.y - c.y) / (f64::from(rect.height().max(1.0)) * 0.5);
    let t = (FOV_DEG.to_radians() * 0.5).tan();
    (cam.eye, norm(add(f, add(mul(r, x * t * aspect(rect)), mul(u, y * t)))))
}

/// A segment clipped to the near plane, on screen.
fn segment(cam: &Camera, rect: Rect, a: V3, b: V3) -> Option<(Pos2, Pos2)> {
    let (f, ..) = cam.basis();
    let (za, zb) = (dot(sub(a, cam.eye), f), dot(sub(b, cam.eye), f));
    let near = NEAR * 1.001;
    if za < near && zb < near {
        return None;
    }
    let clip = |p: V3, q: V3, zp: f64, zq: f64| if zp < near { add(p, mul(sub(q, p), (near - zp) / (zq - zp))) } else { p };
    let (a2, b2) = (clip(a, b, za, zb), clip(b, a, zb, za));
    Some((project(cam, rect, a2)?.0, project(cam, rect, b2)?.0))
}

// ---- drawing ----

/// One projected, shaded triangle.
#[derive(Clone, Debug, PartialEq)]
pub struct Tri {
    pub pts: [Pos2; 3],
    /// Centroid depth.
    pub depth: f64,
    pub fill: Color32,
    pub edges: Option<Color32>,
}

fn rgba(c: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

/// Every triangle of the scene in front of the near plane, shaded, sorted far to near.
pub fn triangles(scene: &Scene3, cam: &Camera, rect: Rect) -> Vec<Tri> {
    let l = norm(LIGHT);
    let mut out = Vec::new();
    for m in &scene.meshes {
        let world = |i: u32| m.positions.get(i as usize).map(|p| add(scene.origin, [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])]));
        for t in m.indices.as_chunks::<3>().0 {
            let (Some(a), Some(b), Some(c)) = (world(t[0]), world(t[1]), world(t[2])) else { continue };
            let (Some(pa), Some(pb), Some(pc)) = (project(cam, rect, a), project(cam, rect, b), project(cam, rect, c)) else { continue };
            let n = norm(cross(sub(b, a), sub(c, a)));
            let k = 0.6 + 0.8 * dot(n, l).abs();
            let ch = |v: u8| (f64::from(v) * k).clamp(0.0, 255.0) as u8;
            out.push(Tri {
                pts: [pa.0, pb.0, pc.0],
                depth: (pa.1 + pb.1 + pc.1) / 3.0,
                fill: Color32::from_rgba_unmultiplied(ch(m.rgba[0]), ch(m.rgba[1]), ch(m.rgba[2]), m.rgba[3]),
                edges: m.edges_rgba.map(rgba),
            });
        }
    }
    out.sort_by(|a, b| b.depth.total_cmp(&a.depth));
    out
}

/// The scene's global bounds, `None` when it has no geometry.
fn bounds(scene: &Scene3) -> Option<(V3, V3)> {
    let pts = scene.meshes.iter().flat_map(|m| m.positions.iter()).chain(scene.paths.iter().flat_map(|p| p.points.iter()));
    let mut b: Option<(V3, V3)> = None;
    for p in pts {
        let g = add(scene.origin, [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])]);
        b = Some(match b {
            None => (g, g),
            Some((lo, hi)) => (std::array::from_fn(|i| lo[i].min(g[i])), std::array::from_fn(|i| hi[i].max(g[i]))),
        });
    }
    b
}

/// The camera a first show uses: the scene's bounds, or the 40 m grid without one.
pub fn fit_scene(scene: Option<&Scene3>, rect: Rect) -> Camera {
    let (lo, hi) = scene.and_then(bounds).unwrap_or(([-10_000.0, -10_000.0, 0.0], [10_000.0, 10_000.0, 0.0]));
    Camera::fit(lo, hi, rect)
}

fn draw(painter: &egui::Painter, scene: Option<&Scene3>, cam: &Camera, rect: Rect) {
    // Ground grid: 40 m, 40 divisions, XY at z = 0; the centre lines brighter. Then 2 m axes.
    let (centre, grid) = (Color32::from_gray(0x44), Color32::from_gray(0x2a));
    for i in -20..=20 {
        let v = f64::from(i) * 1000.0;
        let col = if i == 0 { centre } else { grid };
        for (a, b) in [([v, -20_000.0, 0.0], [v, 20_000.0, 0.0]), ([-20_000.0, v, 0.0], [20_000.0, v, 0.0])] {
            if let Some((p, q)) = segment(cam, rect, a, b) {
                painter.line_segment([p, q], Stroke::new(1.0, col));
            }
        }
    }
    for (axis, col) in [([2000.0, 0.0, 0.0], Color32::RED), ([0.0, 2000.0, 0.0], Color32::GREEN), ([0.0, 0.0, 2000.0], Color32::BLUE)] {
        if let Some((p, q)) = segment(cam, rect, [0.0; 3], axis) {
            painter.line_segment([p, q], Stroke::new(1.5, col));
        }
    }
    let Some(scene) = scene else { return };
    let tris = triangles(scene, cam, rect);
    let mut mesh = egui::Mesh::default();
    for t in &tris {
        let base = mesh.vertices.len() as u32;
        for p in t.pts {
            mesh.colored_vertex(p, t.fill);
        }
        mesh.add_triangle(base, base + 1, base + 2);
    }
    painter.add(egui::Shape::mesh(mesh));
    for t in &tris {
        if let Some(c) = t.edges {
            let s = Stroke::new(1.0, c);
            painter.line_segment([t.pts[0], t.pts[1]], s);
            painter.line_segment([t.pts[1], t.pts[2]], s);
            painter.line_segment([t.pts[2], t.pts[0]], s);
        }
    }
    for path in &scene.paths {
        let s = Stroke::new(1.5, rgba(path.rgba));
        let g = |p: &[f32; 3]| add(scene.origin, [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])]);
        for w in path.points.windows(2) {
            if let Some((p, q)) = segment(cam, rect, g(&w[0]), g(&w[1])) {
                painter.line_segment([p, q], s);
            }
        }
    }
}

// ---- state, pick, panel ----

/// The pane's session state: the scene built for (drawing uid, revision), and the camera.
#[derive(Default)]
pub struct Scene3dState {
    built: Option<((u64, u64), Option<Arc<Scene3>>)>,
    pub camera: Option<Camera>,
    press: Option<Pos2>,
}

/// The scene for the current drawing, rebuilt when it changed; `None` unregistered or empty.
pub fn scene(app: &mut CadApp) -> Option<Arc<Scene3>> {
    let src = SOURCE.get()?;
    let st = app.session.state().ok()?;
    let key = (st.uid, st.revision);
    if app.scene3d.built.as_ref().is_none_or(|(k, _)| *k != key) {
        let s = (src.build)(&st.doc).map(Arc::new);
        app.scene3d.built = Some((key, s));
    }
    app.scene3d.built.as_ref().and_then(|(_, s)| s.clone())
}

/// A click at `pos`: select the source entities the ray hits (those in the edit space) and echo
/// how many. Nothing hit leaves the selection alone. Returns how many were selected.
pub fn click(app: &mut CadApp, cam: &Camera, rect: Rect, pos: Pos2) -> usize {
    let Some(src) = SOURCE.get() else { return 0 };
    let (o, d) = ray(cam, rect, pos);
    let space = app.session.space();
    let Ok(doc) = app.session.doc() else { return 0 };
    let hits: Vec<Handle> = (src.pick)(doc, o, d).into_iter().filter(|h| doc.space_of(*h).as_ref() == Some(&space)).collect();
    if hits.is_empty() {
        return 0;
    }
    let n = hits.len();
    app.session.set_selection(hits);
    app.session.echo(format!("{n} source line(s)"));
    n
}

pub fn show(app: &mut CadApp, ui: &mut egui::Ui) {
    let t = Tokens::get();
    let rect = ui.max_rect();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, t.canvas);
    if SOURCE.get().is_none() {
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, crate::i18n::t("No 3D scene source"), crate::theme::body(), t.text_dim);
        return;
    }
    let scene = scene(app);
    let mut cam = app.scene3d.camera.unwrap_or_else(|| fit_scene(scene.as_deref(), rect));
    // Left drag orbits, middle drag dollies, right drag pans, the wheel dollies (LAMB T47).
    let delta = resp.drag_delta();
    if resp.dragged_by(egui::PointerButton::Primary) {
        cam.orbit(delta);
    } else if resp.dragged_by(egui::PointerButton::Middle) {
        cam.dolly((f64::from(delta.y) * 0.01).exp());
    } else if resp.dragged_by(egui::PointerButton::Secondary) {
        cam.pan(delta, rect);
    }
    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            cam.dolly((-f64::from(scroll) * 0.002).exp());
        }
    }
    let (pressed, released, pos) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_released(), i.pointer.interact_pos()));
    if pressed && resp.hovered() {
        app.scene3d.press = pos;
    }
    if released
        && let (Some(a), Some(b)) = (app.scene3d.press.take(), pos)
        && a.distance(b) < CLICK_SLOP
    {
        click(app, &cam, rect, b);
    }
    app.scene3d.camera = Some(cam);
    draw(&painter, scene.as_deref(), &cam, rect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_engine::Session;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn rect() -> Rect {
        Rect::from_min_size(pos2(10.0, 20.0), egui::vec2(800.0, 600.0))
    }
    fn cam() -> Camera {
        Camera { eye: [3000.0, -4000.0, 2500.0], target: [500.0, 200.0, 100.0] }
    }

    #[test]
    fn the_target_projects_to_the_centre_and_the_centre_ray_points_at_it() {
        let (c, r) = (cam(), rect());
        let (p, depth) = project(&c, r, c.target).unwrap();
        assert!(p.distance(r.center()) < 1e-3, "{p:?}");
        assert!((depth - c.distance()).abs() < 1e-6);
        let (o, d) = ray(&c, r, r.center());
        assert_eq!(o, c.eye);
        assert!((dot(d, norm(sub(c.target, c.eye))) - 1.0).abs() < 1e-9);
        // An off-centre ray lands back on the pixel it came from.
        let px = pos2(150.0, 500.0);
        let (o, d) = ray(&c, r, px);
        let (q, _) = project(&c, r, add(o, mul(d, 3000.0))).unwrap();
        assert!(q.distance(px) < 1e-3, "{q:?}");
        // Behind the eye: nothing.
        assert!(project(&c, r, add(c.eye, mul(d, -100.0))).is_none());
    }

    #[test]
    fn orbit_keeps_the_distance_and_pan_keeps_the_offset() {
        let mut c = cam();
        let d = c.distance();
        c.orbit(egui::vec2(37.0, -12.0));
        assert!((c.distance() - d).abs() < 1e-6);
        assert_eq!(c.target, cam().target);
        let o = sub(c.eye, c.target);
        c.pan(egui::vec2(20.0, 5.0), rect());
        assert!(len(sub(sub(c.eye, c.target), o)) < 1e-6);
        assert_ne!(c.target, cam().target);
        c.dolly(0.5);
        assert!((c.distance() - d * 0.5).abs() < 1e-6);
    }

    #[test]
    fn the_fit_puts_every_corner_of_a_10_m_box_in_the_rect() {
        let r = rect();
        let (lo, hi) = ([-2000.0, 1000.0, 0.0], [8000.0, 11_000.0, 10_000.0]);
        let c = Camera::fit(lo, hi, r);
        for i in 0..8 {
            let p = [if i & 1 == 0 { lo[0] } else { hi[0] }, if i & 2 == 0 { lo[1] } else { hi[1] }, if i & 4 == 0 { lo[2] } else { hi[2] }];
            let (s, _) = project(&c, r, p).unwrap();
            assert!(r.contains(s), "corner {p:?} at {s:?}");
        }
        assert!(project(&c, r, c.target).unwrap().0.distance(r.center()) < 1e-3);
    }

    fn quad(z: f32, rgba: [u8; 4]) -> Mesh3 {
        Mesh3 {
            positions: vec![[0.0, 0.0, z], [1000.0, 0.0, z], [1000.0, 1000.0, z], [0.0, 1000.0, z]],
            indices: vec![0, 1, 2, 0, 2, 3],
            rgba,
            edges_rgba: None,
            sources: Vec::new(),
        }
    }

    #[test]
    fn triangles_sort_far_to_near() {
        let scene = Scene3 { origin: [0.0; 3], meshes: vec![quad(1000.0, [255, 0, 0, 255]), quad(0.0, [0, 0, 255, 255])], paths: Vec::new() };
        let c = Camera { eye: [500.0, -2000.0, 6000.0], target: [500.0, 500.0, 0.0] };
        let tris = triangles(&scene, &c, rect());
        assert_eq!(tris.len(), 4);
        assert!(tris.windows(2).all(|w| w[0].depth >= w[1].depth));
        // The lower (blue) quad is further from an eye above: drawn first.
        assert!(tris[0].fill.b() > 0 && tris[1].fill.b() > 0 && tris[3].fill.r() > 0, "{tris:?}");
    }

    static H: AtomicU64 = AtomicU64::new(0);

    fn build(_: &Drawing) -> Option<Scene3> {
        Some(Scene3 { origin: [0.0; 3], meshes: vec![quad(0.0, [200, 200, 200, 255])], paths: Vec::new() })
    }
    /// The quad (0..1000 square at z = 0) returns H when the ray crosses it.
    fn pick(_: &Drawing, o: [f64; 3], d: [f64; 3]) -> Vec<Handle> {
        if d[2].abs() < 1e-12 {
            return Vec::new();
        }
        let t = -o[2] / d[2];
        let p = add(o, mul(d, t));
        if t > 0.0 && (0.0..=1000.0).contains(&p[0]) && (0.0..=1000.0).contains(&p[1]) { vec![Handle(H.load(Ordering::SeqCst))] } else { Vec::new() }
    }

    #[test]
    fn a_click_on_the_quad_selects_its_source_and_a_miss_leaves_the_selection() {
        let _ = register_scene_source(SceneSource { build, pick });
        assert!(register_scene_source(SceneSource { build, pick }).is_err(), "one source per process");
        let mut s = Session::new();
        s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
        let h = s.doc().unwrap().model.iter().next().unwrap().handle;
        H.store(h.0, Ordering::SeqCst);
        let mut app = CadApp::new(s, crate::Services::default());
        let r = rect();
        let sc = scene(&mut app).unwrap();
        let c = fit_scene(Some(&sc), r);
        let (centre, _) = project(&c, r, [500.0, 500.0, 0.0]).unwrap();
        assert_eq!(click(&mut app, &c, r, centre), 1);
        assert_eq!(app.session.selection(), vec![h]);
        let (off, _) = project(&c, r, [3000.0, 3000.0, 0.0]).unwrap();
        assert_eq!(click(&mut app, &c, r, off), 0);
        assert_eq!(app.session.selection(), vec![h], "a miss leaves the selection alone");
        // The cached scene is reused until the drawing changes.
        assert!(Arc::ptr_eq(&sc, &scene(&mut app).unwrap()));
    }
}

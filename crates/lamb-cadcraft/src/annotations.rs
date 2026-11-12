//! Generated annotation (LAMB's `docs/07-architecture/cadcraft-integration.md` §5, LAMB T474):
//! every placed plan gets a plotted section cut symbol for each placed vertical view whose cut line
//! projects into it — the cut LINE, a gaze arrow at each end, and the section's name at each end
//! (plus `<n>/<layout>` when a sheet shows the section, [`crate::sheets::sheet_refs`]) — and every
//! placed section or elevation gets a plotted level line for each LEVEL (LAMB T473): a LINE at the
//! level's projected floor elevation spanning the view block's extents ±[`LEVEL_OVERRUN_MM`], and
//! `"<name> <±elev>"` left-aligned past its right end, plus a vertical floor-to-floor dimension per
//! pair of consecutive levels by elevation (LAMB T500). Plan cut elevations stay overlay-only. A
//! plan in `auto_dim_plans` also gets its exterior dimension strings ([`crate::autodim`], LAMB T641), one
//! in `auto_dim_interior_plans` its rooms' clear dimensions (LAMB T686), one in `auto_dim_locate_plans`
//! the setting-out strings locating its interior walls (LAMB T694),
//! and every placed plan a text of each opening's schedule mark outside it ([`crate::schedule`], LAMB T639).
//! A plan in `wall_tag_plans` gets each wall type tag, its mark in a hexagon inside the run
//! ([`crate::walltypes::wall_tags`], LAMB T690).
//! An edge detail in `detail_note_views` gets a leader and a `<thickness> <material>` note per layer
//! and one per repeat ([`crate::detailnotes`], LAMB T689).
//! A placed plan, section or elevation with an enlarged sheet gets a box around each enlarged area and
//! `<n>/<layout>` above its top-left corner ([`crate::sheets::enlarged_windows`], LAMB T655).
//! Every building grid ([`named_grids`], LAMB T703) a placed plan, section or elevation shows gets
//! its name in a bubble ([`grid_bubble`]) at both ends in a plan and at the top in a vertical view;
//! a grid drawn in another view also gets its line, a section's clipped to the view block's
//! extents ±[`LEVEL_OVERRUN_MM`].
//!
//! The annotation lives in its own block per view, [`annotation_block_name`]`(view)`, never in the
//! view block, so derivation, the wall graph, links and LAMBSAVE never see them. The block's one
//! model-space INSERT sits at the origin on the view's own view layer, so a sheet viewport showing
//! the view shows its annotation and every other viewport freezes it. Editing a view's block draws
//! no model-space INSERT, so [`draw`] shows the view's annotation there as an overlay: visible,
//! non-pickable and never plotted. [`annotation_observer`] regenerates the blocks after every
//! command, writing the drawing back only when one changed, so the change lands in the command's own
//! undo step.
//!
//! The annotation is the same size on paper at any scale (LAMB T644): every length here is its value
//! at 1:50 and is multiplied by [`k`]`(`[`view_scale`]`)`, and a floor-to-floor dimension carries
//! that scale as its DIMSCALE override. A viewport scale change resizes the block in the command
//! that made it.

use std::sync::Arc;

use cadcraft_doc::{Block, Circle, Common, DimKind, DimStyle, Dimension, Drawing, Entity, EntityKind, HAlign, Insert, Line, Space, Text, VAlign};
use cadcraft_engine::Session;
use cadcraft_geom::{Bounds2, Vec3};
use cadcraft_render::{DisplayList, Options};
use lamb_core::derivation::datums::{GridMember, compute_datum_lines_for_view, named_grids};
use lamb_core::derivation::markers::compute_cut_lines_for_view;
use lamb_core::derivation::projection::{Uz, vertical_uz_to_local};
use lamb_core::derivation::read::{ProjectRead, ViewKind};
use lamb_core::geometry::polyline::resolved_segments;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AnyView, Id};

use crate::project::DrawingProject;
use crate::rooms::{RoomTag, area_text, room_tags};
use crate::schedule::{PlanMark, plan_marks};
use crate::schema::{annotation_block_name, read_document, view_block_name};
use crate::sheets::{enlarged_windows, sheet_refs, view_scale};
use crate::walltypes::{WallTag, wall_tags};

/// The scale every length constant below is the value at.
const BASE_SCALE: f64 = 50.0;
/// Length of a gaze arrow's shaft, from the cut line's end.
pub const CUT_ARROW_MM: f64 = 600.0;
/// Length of each of an arrow head's two strokes, ±30° back from the tip.
pub const CUT_HEAD_MM: f64 = 200.0;
/// The section name's text height.
pub const CUT_TEXT_MM: f64 = 250.0;
/// The name sits this far beyond the arrow's tip.
const CUT_TEXT_GAP_MM: f64 = 300.0;
/// A level line runs this far past the view block's extents at each end.
pub const LEVEL_OVERRUN_MM: f64 = 1000.0;
/// A level label's text height.
pub const LEVEL_TEXT_MM: f64 = 250.0;
/// The level label starts this far past the line's right end.
const LEVEL_TEXT_GAP_MM: f64 = 200.0;
/// A floor-to-floor dimension's line sits this far right of the level lines' right end, clear of
/// the labels.
pub const LEVEL_DIM_OFFSET_MM: f64 = 1200.0;
/// An opening's mark sits this far from the opening's midpoint, on the outside.
pub const MARK_OFFSET_MM: f64 = 400.0;
/// A grid bubble's radius; its name is 0.8 × this high.
pub const GRID_BUBBLE_MM: f64 = 250.0;
/// From a grid line's end to its bubble's edge.
pub const GRID_GAP_MM: f64 = 500.0;
/// Every generated entity's layer inside the block.
const LAYER: &str = "0";

/// One generated entity, in view-local coordinates.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Item {
    Line(Vec3, Vec3),
    /// A cut symbol's name and its height: centred.
    Text(Vec3, String, f64),
    /// A level label and its height: left-aligned.
    Label(Vec3, String, f64),
    /// A rotated-linear dimension: its two measured points, its rotation in radians, a point on its
    /// dimension line, and the view scale that is its DIMSCALE.
    Dim(Vec3, Vec3, f64, Vec3, f64),
    /// A circle: its centre and radius.
    Circle(Vec3, f64),
}

/// What a length constant is multiplied by at 1:`scale`.
pub(crate) fn k(scale: f64) -> f64 {
    scale / BASE_SCALE
}

fn v(x: f64, y: f64) -> Vec3 {
    Vec3::new(x, y, 0.0)
}

fn near(a: Vec3, b: Vec3) -> bool {
    (a.x - b.x).hypot(a.y - b.y) <= TOLERANCE.coincidence_mm
}

/// The box around each enlarged area of view `id` and the sheet it is on, at 1:`scale`.
fn enlarged_boxes(d: &Drawing, id: Id, scale: f64) -> Vec<Item> {
    let height = CUT_TEXT_MM * k(scale);
    let mut out = Vec::new();
    for (w, sheet) in enlarged_windows(d, id) {
        let (a, b, c, e) = (v(w.min.x, w.min.y), v(w.max.x, w.min.y), v(w.max.x, w.max.y), v(w.min.x, w.max.y));
        out.extend([Item::Line(a, b), Item::Line(b, c), Item::Line(c, e), Item::Line(e, a)]);
        out.push(Item::Label(v(w.min.x, w.max.y + 0.5 * height), sheet, height));
    }
    out
}

/// A section cut symbol from `from` to `to` looking along unit `gaze`, with every length its 1:50
/// value × `f`: the cut line, then at each end a gaze arrow (shaft and two head strokes), then at
/// each end `name` beyond the arrow's tip and `sheet` (when given) below it.
pub(crate) fn cut_symbol(from: Vec3, to: Vec3, gaze: (f64, f64), name: &str, sheet: Option<&str>, f: f64) -> Vec<Item> {
    let (arrow, head, height) = (CUT_ARROW_MM * f, CUT_HEAD_MM * f, CUT_TEXT_MM * f);
    let g = gaze;
    let mut out = vec![Item::Line(from, to)];
    for end in [from, to] {
        let tip = v(end.x + g.0 * arrow, end.y + g.1 * arrow);
        out.push(Item::Line(v(end.x, end.y), tip));
        for a in [30f64, -30f64] {
            let (s, co) = a.to_radians().sin_cos();
            // Back from the tip: -gaze rotated by ±30°.
            let (bx, by) = (-(g.0 * co - g.1 * s), -(g.0 * s + g.1 * co));
            out.push(Item::Line(tip, v(tip.x + bx * head, tip.y + by * head)));
        }
    }
    for end in [from, to] {
        let beyond = arrow + CUT_TEXT_GAP_MM * f;
        let at = v(end.x + g.0 * beyond, end.y + g.1 * beyond);
        out.push(Item::Text(at, name.to_owned(), height));
        if let Some(sheet) = sheet {
            out.push(Item::Text(v(at.x, at.y - 1.5 * height), sheet.to_owned(), height));
        }
    }
    out
}

/// A grid bubble past a grid line's `end`, along unit `out`, with every length its 1:50 value ×
/// `f`: the line on from `end` to the bubble's edge, the circle, and `name` centred in it.
pub(crate) fn grid_bubble(end: Vec3, out: (f64, f64), name: &str, f: f64) -> Vec<Item> {
    let (r, gap) = (GRID_BUBBLE_MM * f, GRID_GAP_MM * f);
    let edge = v(end.x + out.0 * gap, end.y + out.1 * gap);
    let c = v(end.x + out.0 * (gap + r), end.y + out.1 * (gap + r));
    vec![Item::Line(v(end.x, end.y), edge), Item::Circle(c, r), Item::Text(c, name.to_owned(), 0.8 * r)]
}

/// Placed view `id`'s grid items at length factor `f`: per grid line another view draws that `id`
/// shows, the line (an infinite one clipped to the view block's extents ±[`LEVEL_OVERRUN_MM`]) and
/// its bubbles; then per datum segment of `id`'s own that is a grid, its bubbles. Bubbles go at both
/// ends in a plan and at the upper end in a vertical view.
fn grid_items(d: &Drawing, dp: &DrawingProject, id: Id, vertical: bool, f: f64) -> Result<Vec<Item>, String> {
    let grids = named_grids(dp, &[])?;
    if grids.is_empty() {
        return Ok(Vec::new());
    }
    let name_of = |m: GridMember| grids.iter().find(|g| g.members.contains(&m)).map(|g| g.name.clone());
    let bubbles = |a: Vec3, b: Vec3, name: &str, out: &mut Vec<Item>| {
        let len = (b.x - a.x).hypot(b.y - a.y);
        if len <= TOLERANCE.coincidence_mm {
            return;
        }
        let u = ((b.x - a.x) / len, (b.y - a.y) / len);
        if vertical {
            let (top, dir) = if b.y >= a.y { (b, u) } else { (a, (-u.0, -u.1)) };
            out.extend(grid_bubble(top, dir, name, f));
        } else {
            out.extend(grid_bubble(a, (-u.0, -u.1), name, f));
            out.extend(grid_bubble(b, u, name, f));
        }
    };
    let mut out = Vec::new();
    let ext = d.extents(&Space::Block(view_block_name(id)));
    for o in compute_datum_lines_for_view(dp, id)? {
        let member = GridMember { view_id: o.source_view_id, entity_id: o.source_entity_id, segment_index: o.source_segment_index };
        let Some(name) = name_of(member) else { continue };
        let (a, b) = if o.infinite {
            let (dx, dy) = (o.to.x - o.from.x, o.to.y - o.from.y);
            let len = dx.hypot(dy);
            if ext.is_empty() || len <= 0.0 {
                continue;
            }
            let dir = (dx / len, dy / len);
            let ts = [(ext.min.x, ext.min.y), (ext.max.x, ext.min.y), (ext.max.x, ext.max.y), (ext.min.x, ext.max.y)]
                .map(|(x, y)| (x - o.from.x) * dir.0 + (y - o.from.y) * dir.1);
            let over = LEVEL_OVERRUN_MM * f;
            let t0 = ts.iter().copied().fold(f64::INFINITY, f64::min) - over;
            let t1 = ts.iter().copied().fold(f64::NEG_INFINITY, f64::max) + over;
            (v(o.from.x + dir.0 * t0, o.from.y + dir.1 * t0), v(o.from.x + dir.0 * t1, o.from.y + dir.1 * t1))
        } else {
            (v(o.from.x, o.from.y), v(o.to.x, o.to.y))
        };
        out.push(Item::Line(a, b));
        bubbles(a, b, &name, &mut out);
    }
    for g in &grids {
        for m in g.members.iter().filter(|m| m.view_id == id) {
            let Some(e) = dp.entity(m.entity_id) else { continue };
            let Some(poly) = e.polyline.as_deref() else { continue };
            let Some(seg) = resolved_segments(poly).into_iter().find(|s| s.index == m.segment_index) else { continue };
            bubbles(v(seg.from.x, seg.from.y), v(seg.to.x, seg.to.y), &g.name, &mut out);
        }
    }
    Ok(out)
}

/// A wall type tag: a flat-topped hexagon of circumradius 0.9 × `height` round `centre`, and `mark`
/// in it at 0.6 × `height`.
pub(crate) fn wall_tag_symbol(centre: Vec3, mark: &str, height: f64) -> Vec<Item> {
    let (c, r) = (centre, 0.9 * height);
    let corner = |i: usize| {
        let (s, co) = (i as f64 * std::f64::consts::FRAC_PI_3).sin_cos();
        v(c.x + r * co, c.y + r * s)
    };
    let mut out: Vec<Item> = (0..6).map(|i| Item::Line(corner(i), corner((i + 1) % 6))).collect();
    out.push(Item::Text(c, mark.to_owned(), 0.6 * height));
    out
}

/// A room tag: `name`, `number` and `area` centred on `at`, 0.35 × `height` apart.
pub(crate) fn room_tag_items(at: Vec3, name: &str, number: &str, area: &str, height: f64) -> Vec<Item> {
    let step = 1.35 * height;
    vec![
        Item::Text(v(at.x, at.y + step), name.to_owned(), height),
        Item::Text(v(at.x, at.y), number.to_owned(), height),
        Item::Text(v(at.x, at.y - step), area.to_owned(), height),
    ]
}

/// What view `id` wants, in write order: a plan its cut symbols, opening marks and room tags, a vertical view
/// its level lines, and either the box of each enlarged area; a detail in `detail_note_views` its layer
/// notes ([`crate::detailnotes`]); nothing for an unplaced plan or vertical view or any other detail.
fn wanted(
    d: &Drawing,
    id: Id,
    sheet_of: &dyn Fn(Id) -> Option<String>,
    marks: &[PlanMark],
    rooms: &[RoomTag],
    tags: &[WallTag],
) -> Result<Vec<Item>, String> {
    let dp = DrawingProject::new(d).map_err(|e| e.to_string())?;
    if dp.view(id).is_some_and(|v| v.kind == ViewKind::Detail) {
        if !dp.document().detail_note_views.contains(&id) {
            return Ok(Vec::new());
        }
        let at = |p: lamb_core::geometry::Point2| v(p.x, p.y);
        let mut out = Vec::new();
        for n in crate::detailnotes::notes(d, id, view_scale(d, dp.document(), id)) {
            out.extend(n.leader.windows(2).map(|w| Item::Line(at(w[0]), at(w[1]))));
            out.push(Item::Label(at(n.at), n.text, n.height));
        }
        return Ok(out);
    }
    let Some(view) = dp.view(id).filter(|v| v.placement.is_some()) else { return Ok(Vec::new()) };
    let scale = view_scale(d, dp.document(), id);
    if view.kind == ViewKind::Vertical {
        let mut out = level_lines(d, &dp, id, scale);
        out.extend(grid_items(d, &dp, id, true, k(scale))?);
        out.extend(enlarged_boxes(d, id, scale));
        return Ok(out);
    }
    let plan = id;
    let f = k(scale);
    let height = CUT_TEXT_MM * f;
    let mut out = Vec::new();
    for c in compute_cut_lines_for_view(&dp, plan)? {
        let sheet = sheet_of(c.source_view_id);
        out.extend(cut_symbol(v(c.from.x, c.from.y), v(c.to.x, c.to.y), (c.gaze_dir.x, c.gaze_dir.y), &c.name, sheet.as_deref(), f));
    }
    for m in marks.iter().filter(|m| m.plan == plan) {
        let off = MARK_OFFSET_MM * f;
        out.push(Item::Text(v(m.at.x + m.out.x * off, m.at.y + m.out.y * off), m.mark.clone(), height));
    }
    for t in tags.iter().filter(|t| t.plan == plan) {
        let off = t.half_width + MARK_OFFSET_MM * f;
        out.extend(wall_tag_symbol(v(t.at.x + t.inward.x * off, t.at.y + t.inward.y * off), &t.mark, height));
    }
    for r in rooms.iter().filter(|r| r.plan == plan) {
        out.extend(room_tag_items(v(r.at.x, r.at.y), &r.name, &r.number, &area_text(r.area_m2), height));
    }
    if dp.document().auto_dim_plans.contains(&plan) {
        let at = |p: lamb_core::geometry::Point2| v(p.x, p.y);
        out.extend(crate::autodim::strings(&dp, plan, f).into_iter().map(|d| Item::Dim(at(d.a), at(d.b), d.rotation, at(d.line_at), scale)));
    }
    if dp.document().auto_dim_interior_plans.contains(&plan) {
        let at = |p: lamb_core::geometry::Point2| v(p.x, p.y);
        out.extend(crate::autodim::interior(&dp, plan).into_iter().map(|d| Item::Dim(at(d.a), at(d.b), d.rotation, at(d.line_at), scale)));
    }
    if dp.document().auto_dim_locate_plans.contains(&plan) {
        let at = |p: lamb_core::geometry::Point2| v(p.x, p.y);
        out.extend(crate::autodim::locate(&dp, plan).into_iter().map(|d| Item::Dim(at(d.a), at(d.b), d.rotation, at(d.line_at), scale)));
    }
    out.extend(grid_items(d, &dp, plan, false, f)?);
    out.extend(enlarged_boxes(d, id, scale));
    Ok(out)
}

/// `"+0"`, `"+3000"`, `"-2700"`: signed whole millimetres, never `-0`.
fn signed_mm(z: f64) -> String {
    let r = z.round();
    format!("{:+.0}", if r == 0.0 { 0.0 } else { r })
}

/// Placed vertical view `id`'s level lines at 1:`scale`: per LEVEL (by `order`, then id) a LINE at
/// its projected floor elevation across the view block's x-extents ±[`LEVEL_OVERRUN_MM`], then its
/// label. An empty view block wants none.
fn level_lines(d: &Drawing, dp: &DrawingProject, id: Id, scale: f64) -> Vec<Item> {
    let Some(view) = dp.view(id) else { return Vec::new() };
    let ext = d.extents(&Space::Block(view_block_name(id)));
    if ext.is_empty() {
        return Vec::new();
    }
    let f = k(scale);
    let (x0, x1) = (ext.min.x - LEVEL_OVERRUN_MM * f, ext.max.x + LEVEL_OVERRUN_MM * f);
    let mut levels: Vec<_> = dp.document().levels.iter().collect();
    levels.sort_by(|a, b| a.order.total_cmp(&b.order).then(a.id.0.cmp(&b.id.0)));
    let y_of = |z: f64| vertical_uz_to_local(&view, Uz { u: 0.0, z }).y;
    let mut out = Vec::new();
    for l in &levels {
        let z = l.floor_elevation.value;
        let y = y_of(z);
        out.push(Item::Line(v(x0, y), v(x1, y)));
        out.push(Item::Label(v(x1 + LEVEL_TEXT_GAP_MM * f, y), format!("{} {}", l.name, signed_mm(z)), LEVEL_TEXT_MM * f));
    }
    let mut zs: Vec<f64> = levels.iter().map(|l| l.floor_elevation.value).collect();
    zs.sort_by(f64::total_cmp);
    for w in zs.windows(2) {
        if w[1] - w[0] > TOLERANCE.coincidence_mm {
            let (a, b) = (v(x1, y_of(w[0])), v(x1, y_of(w[1])));
            out.push(Item::Dim(a, b, std::f64::consts::FRAC_PI_2, dim_line_pt(b, scale), scale));
        }
    }
    out
}

fn text(at: Vec3, value: &str, height: f64, halign: HAlign) -> Text {
    Text {
        insert: at,
        align_pt: Some(at),
        height,
        value: value.into(),
        rotation: 0.0,
        width_factor: 1.0,
        oblique: 0.0,
        style: "Standard".into(),
        halign,
        valign: VAlign::Middle,
    }
}

pub(crate) fn entity_of(i: &Item) -> EntityKind {
    match i {
        Item::Line(a, b) => EntityKind::Line(Line { a: *a, b: *b }),
        Item::Text(at, s, h) => EntityKind::Text(text(*at, s, *h, HAlign::Center)),
        Item::Label(at, s, h) => EntityKind::Text(text(*at, s, *h, HAlign::Left)),
        Item::Dim(a, b, rotation, line_at, scale) => EntityKind::Dimension(rotated_dim(*a, *b, *rotation, *line_at, *scale)),
        Item::Circle(center, radius) => EntityKind::Circle(Circle { center: *center, radius: *radius }),
    }
}

/// Where a floor-to-floor dimension's line, vertical from the lower to the upper level line's right
/// end, passes at 1:`scale`: level with `b`,
/// [`LEVEL_DIM_OFFSET_MM`] right.
fn dim_line_pt(b: Vec3, scale: f64) -> Vec3 {
    v(b.x + LEVEL_DIM_OFFSET_MM * k(scale), b.y)
}

/// A linear dimension from `a` to `b` at `rotation` and 1:`scale`, as DIMLINEAR draws one: `defpt`
/// the point `line_at` on the dimension line, measured text, style `Standard` with DIMSCALE overridden
/// to `scale`, never associative.
fn rotated_dim(a: Vec3, b: Vec3, rotation: f64, line_at: Vec3, scale: f64) -> Dimension {
    let mut overrides = serde_json::Map::new();
    overrides.insert("scale".to_owned(), serde_json::json!(scale));
    Dimension {
        kind: DimKind::Linear { rotation },
        defpt: line_at,
        text_mid: Vec3::ZERO,
        p13: a,
        p14: b,
        p15: Vec3::ZERO,
        p16: Vec3::ZERO,
        text: String::new(),
        style: "Standard".into(),
        measurement: 0.0,
        text_rotation: 0.0,
        user_text_pos: false,
        block: None,
        insertion_point: Vec3::ZERO,
        overrides,
        assoc: Vec::new(),
        marks: Default::default(),
    }
}

/// `true` when `dm` is a linear dimension at `rotation` (or its reverse) whose dimension line passes
/// through `line_at`.
fn same_dim_line(dm: &Dimension, rotation: f64, line_at: Vec3) -> bool {
    let DimKind::Linear { rotation: got } = dm.kind else { return false };
    let (sin, cos) = rotation.sin_cos();
    (got - rotation).sin().abs() <= TOLERANCE.angle_deg.to_radians().sin()
        && ((dm.defpt.x - line_at.x) * sin - (dm.defpt.y - line_at.y) * cos).abs() <= TOLERANCE.coincidence_mm
}

/// `true` when `a` and `b` are the same size or scale, whatever a file round trip did to the last
/// few bits.
fn same_size(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * b.abs().max(1.0)
}

/// `true` when `block` holds exactly `want`, in order: each on layer `0`, LINE ends, TEXT
/// alignment points and DIMENSION points within [`TOLERANCE`]`.coincidence_mm`, the same text
/// strings and heights, and every dimension at the wanted rotation and line with measured text and
/// the wanted DIMSCALE.
fn block_matches(block: &Block, want: &[Item]) -> bool {
    block.entities.len() == want.len()
        && block.entities.iter().zip(want).all(|(e, w)| {
            e.common.layer == LAYER
                && match (&e.kind, w) {
                    (EntityKind::Line(l), Item::Line(a, b)) => near(l.a, *a) && near(l.b, *b),
                    (EntityKind::Text(t), Item::Text(at, s, h)) => {
                        t.halign == HAlign::Center && near(t.align_pt.unwrap_or(t.insert), *at) && t.value == *s && same_size(t.height, *h)
                    }
                    (EntityKind::Text(t), Item::Label(at, s, h)) => {
                        t.halign == HAlign::Left && near(t.align_pt.unwrap_or(t.insert), *at) && t.value == *s && same_size(t.height, *h)
                    }
                    (EntityKind::Dimension(dm), Item::Dim(a, b, rotation, line_at, scale)) => {
                        same_dim_line(dm, *rotation, *line_at)
                            && near(dm.p13, *a)
                            && near(dm.p14, *b)
                            && dm.text.is_empty()
                            && same_size(DimStyle::default().with_overrides(&dm.overrides).scale, *scale)
                    }
                    (EntityKind::Circle(c), Item::Circle(at, r)) => near(c.center, *at) && same_size(c.radius, *r),
                    _ => false,
                }
        })
}

/// The block's canonical model-space INSERT: at the origin, scale 1, on the view's own layer.
/// Added only when model space has no INSERT of the block.
fn ensure_insert(d: &mut Drawing, plan: Id) {
    let name = annotation_block_name(plan);
    if d.model.iter().any(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == name)) {
        return;
    }
    let layer = view_block_name(plan);
    d.ensure_layer(&layer);
    let insert = Insert {
        clip: None,
        block: name,
        insert: Vec3::ZERO,
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: Vec::new(),
        cols: 1,
        rows: 1,
        col_spacing: 0.0,
        row_spacing: 0.0,
    };
    let handle = d.new_handle();
    d.model.push(Entity { handle, common: Common { layer, ..Common::default() }, kind: EntityKind::Insert(insert) });
}

fn replace_block(d: &mut Drawing, plan: Id, items: &[Item]) -> Result<(), String> {
    let name = annotation_block_name(plan);
    d.blocks.insert(name.clone(), Arc::new(Block::new(&name)));
    let space = Space::Block(name);
    for i in items {
        d.add(&space, Common { layer: LAYER.into(), ..Common::default() }, entity_of(i)).map_err(|e| e.to_string())?;
    }
    ensure_insert(d, plan);
    Ok(())
}

/// Per view whose block must change: the view and its wanted items.
fn plan_changes(d: &Drawing) -> Result<Vec<(Id, Vec<Item>)>, String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    let blocks: Vec<(Id, String)> = doc.views.iter().map(|v| (v.id(), view_block_name(v.id()))).collect();
    let refs = sheet_refs(d, &blocks);
    let sheet_of = |id: Id| refs.iter().find(|(v, _, _)| *v == id).map(|(_, n, l)| format!("{n}/{l}"));
    let marks = plan_marks(d);
    let rooms = room_tags(d);
    let tags = wall_tags(d);
    let mut out = Vec::new();
    for p in doc.views.iter().map(AnyView::id) {
        let want = wanted(d, p, &sheet_of, &marks, &rooms, &tags)?;
        let changed = match d.block(&annotation_block_name(p)) {
            Some(b) => !block_matches(b, &want),
            None => !want.is_empty(),
        };
        if changed {
            out.push((p, want));
        }
    }
    Ok(out)
}

/// `true` when [`annotation_observer`] would change nothing.
#[cfg(test)]
pub(crate) fn settled(d: &Drawing) -> Result<bool, String> {
    plan_changes(d).map(|t| t.is_empty())
}

/// The after-command observer: every view's [`annotation_block_name`] block made to equal its
/// wanted annotation — untouched when it already does, rewritten (and given its INSERT) when not,
/// emptied but kept when the view now wants nothing. The drawing is written back only when some
/// block changed; an error is echoed and changes nothing.
pub fn annotation_observer(s: &mut Session, _before: &Arc<Drawing>) {
    let todo = match s.doc().map_err(|e| e.to_string()).and_then(plan_changes) {
        Ok(t) if t.is_empty() => return,
        Ok(t) => t,
        Err(e) => {
            s.echo(format!("LAMB annotations: {e}"));
            return;
        }
    };
    let Ok(current) = s.doc() else { return };
    let mut after = current.clone();
    for (p, items) in &todo {
        if let Err(e) = replace_block(&mut after, *p, items) {
            s.echo(format!("LAMB annotations: {e}"));
            return;
        }
    }
    if let Ok(d) = s.doc_mut() {
        *d = after;
    }
}

/// The annotation of the view whose block `space` edits, as an overlay in the view's local
/// coordinates. Empty for any other space and for a view without an annotation block.
pub(crate) fn draw(d: &Drawing, space: &Space, opts: &Options) -> DisplayList {
    let block = crate::overlays::view_id(space).and_then(|id| d.block(&annotation_block_name(id)));
    let Some(block) = block else { return DisplayList { bounds: Bounds2::EMPTY, ..DisplayList::default() } };
    cadcraft_render::build_entities(d, block.entities.iter().map(|e| &**e), &Options { view: None, ..opts.clone() })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::overlays::lamb_overlay;
    use cadcraft_geom::Vec2;
    use cadcraft_render::{Kind, build_plot};
    use serde_json::json;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    fn is_vertical(k: &DimKind) -> bool {
        matches!(k, DimKind::Linear { rotation } if (rotation - std::f64::consts::FRAC_PI_2).abs() < 1e-9)
    }

    /// LAMBPROJECT's placed ground-floor plan, edited, with section `A-A` cut from (0,0) to
    /// (5000,0); observers installed. Returns (session, plan, section).
    fn fixture() -> T<(Session, Id, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let r =
            s.execute("newsectionline", &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 0}, "cutTo": {"x": 5000, "y": 0} }))?;
        let sec = Id(r["viewId"].as_u64().ok_or("no id")?);
        Ok((s, plan, sec))
    }

    fn block_of(s: &Session, plan: Id) -> Option<Arc<Block>> {
        s.doc().ok()?.blocks.get(&annotation_block_name(plan)).cloned()
    }

    /// The block's content as items (layer checked to be `0`).
    fn items(s: &Session, plan: Id) -> T<Vec<Item>> {
        let b = block_of(s, plan).ok_or("no annotation block")?;
        b.entities
            .iter()
            .map(|e| {
                if e.common.layer != LAYER {
                    return Err("not on layer 0".into());
                }
                match &e.kind {
                    EntityKind::Line(l) => Ok(Item::Line(l.a, l.b)),
                    EntityKind::Circle(c) => Ok(Item::Circle(c.center, c.radius)),
                    EntityKind::Text(t) if t.halign == HAlign::Left => Ok(Item::Label(t.align_pt.unwrap_or(t.insert), t.value.clone(), t.height)),
                    EntityKind::Text(t) => Ok(Item::Text(t.align_pt.unwrap_or(t.insert), t.value.clone(), t.height)),
                    EntityKind::Dimension(dm) if is_vertical(&dm.kind) => {
                        let scale = DimStyle::default().with_overrides(&dm.overrides).scale;
                        Ok(Item::Dim(dm.p13, dm.p14, std::f64::consts::FRAC_PI_2, dm.defpt, scale))
                    }
                    _ => Err("unexpected kind".into()),
                }
            })
            .collect()
    }

    /// The opening marks in the plan's annotation block: (mark, x, y), in block order.
    pub(crate) fn mark_texts(s: &Session, plan: Id) -> T<Vec<(String, f64, f64)>> {
        let is_mark = |t: &str| t.len() == 3 && (t.starts_with('D') || t.starts_with('W')) && t[1..].bytes().all(|b| b.is_ascii_digit());
        Ok(items(s, plan)?
            .into_iter()
            .filter_map(|i| if let Item::Text(at, t, _) = i { is_mark(&t).then_some((t, at.x, at.y)) } else { None })
            .collect())
    }

    fn texts(s: &Session, plan: Id) -> T<Vec<String>> {
        Ok(items(s, plan)?.into_iter().filter_map(|i| if let Item::Text(_, t, _) | Item::Label(_, t, _) = i { Some(t) } else { None }).collect())
    }

    fn undo_len(s: &Session) -> T<usize> {
        Ok(s.state()?.undo.len())
    }

    fn inserts(s: &Session, plan: Id) -> T<Vec<String>> {
        let name = annotation_block_name(plan);
        Ok(s.doc()?.model.iter().filter(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == name)).map(|e| e.common.layer.clone()).collect())
    }

    fn close(a: Vec3, x: f64, y: f64) -> bool {
        (a.x - x).abs() < 1e-6 && (a.y - y).abs() < 1e-6
    }

    #[test]
    fn a_section_line_gets_its_cut_symbol_in_the_same_undo_step() -> R {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let n = undo_len(&s)?;
        let r =
            s.execute("newsectionline", &json!({ "name": "A-A", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 0}, "cutTo": {"x": 5000, "y": 0} }))?;
        let _sec = Id(r["viewId"].as_u64().ok_or("no id")?);
        assert_eq!(undo_len(&s)?, n + 1);
        let it = items(&s, plan)?;
        assert_eq!(it.len(), 1 + 2 * 3 + 2, "{it:?}");
        let Item::Line(a, b) = &it[0] else { return Err("first is not the cut line".into()) };
        assert!(close(*a, 0.0, 0.0) && close(*b, 5000.0, 0.0), "{a:?} {b:?}");
        // The arrows point along the gaze, perpendicular to the cut.
        let Item::Line(e, tip) = &it[1] else { return Err("no arrow".into()) };
        assert!(close(*e, 0.0, 0.0));
        assert!((tip.x).abs() < 1e-6 && (tip.y.abs() - CUT_ARROW_MM).abs() < 1e-6, "{tip:?}");
        let gy = tip.y.signum();
        let Item::Line(t2, h) = &it[2] else { return Err("no head".into()) };
        assert!(near(*t2, *tip));
        assert!(((h.x - tip.x).hypot(h.y - tip.y) - CUT_HEAD_MM).abs() < 1e-6 && (h.y - tip.y) * gy < 0.0, "head points back: {h:?}");
        let Item::Line(e2, _) = &it[4] else { return Err("no second arrow".into()) };
        assert!(close(*e2, 5000.0, 0.0));
        let k = gy * (CUT_ARROW_MM + CUT_TEXT_GAP_MM);
        assert_eq!(it[7..].to_vec(), vec![Item::Text(v(0.0, k), "A-A".into(), CUT_TEXT_MM), Item::Text(v(5000.0, k), "A-A".into(), CUT_TEXT_MM)]);
        assert_eq!(inserts(&s, plan)?, [view_block_name(plan)]);
        let ins = s.doc()?.model.iter().find(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == annotation_block_name(plan))).cloned();
        let Some(EntityKind::Insert(i)) = ins.as_deref().map(|e| &e.kind) else { return Err("no insert".into()) };
        assert_eq!((i.insert, i.scale, i.rotation), (Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0), 0.0));

        s.undo()?;
        assert!(block_of(&s, plan).is_none() && inserts(&s, plan)?.is_empty());
        s.redo()?;
        assert_eq!(items(&s, plan)?, it);
        Ok(())
    }

    #[test]
    fn rename_and_flip_regenerate_unrelated_commands_rewrite_nothing_unplace_empties() -> R {
        let (mut s, plan, sec) = fixture()?;
        let first = items(&s, plan)?;
        let b0 = block_of(&s, plan).ok_or("no block")?;
        s.execute("line", &json!({ "points": [[9000, 9000], [9500, 9000]] }))?;
        assert!(Arc::ptr_eq(&b0, &block_of(&s, plan).ok_or("no block")?), "an unrelated command leaves the block alone");

        s.execute("viewprops", &json!({ "mode": "rename", "viewId": sec.0, "name": "B-B" }))?;
        assert_eq!(texts(&s, plan)?, ["B-B", "B-B"]);

        s.execute("viewprops", &json!({ "mode": "flip", "viewId": sec.0, "flipped": true }))?;
        let flipped = items(&s, plan)?;
        let (Item::Line(_, t0), Item::Line(_, t1)) = (&first[1], &flipped[1]) else { return Err("no arrows".into()) };
        assert!(near(*t1, v(t0.x, -t0.y)), "the arrow turns round: {t0:?} -> {t1:?}");

        s.execute("unplaceview", &json!({ "viewId": sec.0 }))?;
        assert!(items(&s, plan)?.is_empty(), "emptied");
        assert_eq!(inserts(&s, plan)?.len(), 1, "the insert stays");
        Ok(())
    }

    #[test]
    fn a_sheet_showing_the_section_adds_its_reference() -> R {
        let (mut s, plan, sec) = fixture()?;
        let n = undo_len(&s)?;
        s.execute("newsheet", &json!({ "name": "A301", "viewIds": [plan.0, sec.0] }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        assert_eq!(texts(&s, plan)?, ["A-A", "2/A301", "A-A", "2/A301"]);
        let it = items(&s, plan)?;
        let (Item::Text(a, ..), Item::Text(b, ..)) = (&it[7], &it[8]) else { return Err("no texts".into()) };
        assert!(close(*b, a.x, a.y - 1.5 * CUT_TEXT_MM));
        s.undo()?;
        assert_eq!(texts(&s, plan)?, ["A-A", "A-A"]);
        Ok(())
    }

    #[test]
    fn derivation_ignores_the_block_and_a_dxf_round_trip_keeps_it() -> R {
        let (s, plan, _sec) = fixture()?;
        let d = s.doc()?;
        let mut bare = d.clone();
        bare.blocks.remove(&annotation_block_name(plan));
        let h =
            bare.model.iter().find(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == annotation_block_name(plan))).ok_or("no insert")?.handle;
        bare.model.remove(h);
        let (with, without) = (DrawingProject::new(d)?, DrawingProject::new(&bare)?);
        let ents = |p: &DrawingProject| -> String {
            use lamb_core::derivation::read::ProjectRead;
            let ids: Vec<Id> = p.views().map(|v| v.id).collect();
            ids.iter().map(|id| format!("{:?}", p.entities(*id).collect::<Vec<_>>())).collect()
        };
        assert!(!ents(&with).is_empty());
        assert_eq!(ents(&with), ents(&without));
        let markers = |p: &DrawingProject| format!("{:?}", lamb_core::derivation::markers::compute_markers_for_view(p, plan));
        assert_eq!(markers(&with), markers(&without));

        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(d).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert!(back.model.iter().any(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == annotation_block_name(plan))));
        assert!(plan_changes(&back)?.is_empty(), "the observer would change nothing after a round trip");
        Ok(())
    }
    /// [`fixture`] plus `Level 2` at 3000 and a 4000-wide rectangle drawn in the section from
    /// (0,0); returns (session, plan, section, level 1, level 2).
    fn levelled() -> T<(Session, Id, Id, Id, Id)> {
        let (mut s, plan, sec) = fixture()?;
        let l1 = read_document(s.doc()?)?.levels.first().ok_or("no level")?.id;
        let l2 =
            Id(s.execute("level", &json!({ "mode": "create", "name": "Level 2", "floorElevationMm": 3000 }))?["levelId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [4000, 0], [4000, 3000], [0, 3000]], "closed": true }))?;
        Ok((s, plan, sec, l1, l2))
    }

    /// The level lines' (y, x0, x1, label) per level, in block order.
    fn levels_of(s: &Session, view: Id) -> T<Vec<(f64, f64, f64, String)>> {
        let it: Vec<Item> = items(s, view)?.into_iter().filter(|i| !matches!(i, Item::Dim(..))).collect();
        it.chunks(2)
            .map(|c| match c {
                [Item::Line(a, b), Item::Label(at, t, _)] => {
                    assert!((a.y - b.y).abs() < 1e-9 && (at.y - a.y).abs() < 1e-9 && (at.x - b.x - LEVEL_TEXT_GAP_MM).abs() < 1e-9);
                    Ok((a.y, a.x, b.x, t.clone()))
                }
                _ => Err(format!("not a level line: {c:?}").into()),
            })
            .collect()
    }

    fn local_y(s: &Session, view: Id, z: f64) -> T<f64> {
        let d = s.doc()?;
        let dp = DrawingProject::new(d)?;
        let vr = dp.view(view).ok_or("no view")?;
        Ok(vertical_uz_to_local(&vr, Uz { u: 0.0, z }).y)
    }

    #[test]
    fn a_placed_section_gets_a_level_line_per_level_and_its_block_is_unchanged() -> R {
        let (s, plan, sec, _l1, _l2) = levelled()?;
        let (y0, y1) = (local_y(&s, sec, 0.0)?, local_y(&s, sec, 3000.0)?);
        assert!((y1 - y0 - 3000.0).abs() < 1e-6, "{y0} {y1}");
        let ext = s.doc()?.extents(&Space::Block(view_block_name(sec)));
        let (x0, x1) = (ext.min.x - LEVEL_OVERRUN_MM, ext.max.x + LEVEL_OVERRUN_MM);
        let got = levels_of(&s, sec)?;
        assert_eq!(got.len(), 2, "{got:?}");
        for ((y, a, b, t), (wy, wt)) in got.iter().zip([(y0, "Level 1 +0"), (y1, "Level 2 +3000")]) {
            assert!((y - wy).abs() < 1e-6 && (a - x0).abs() < 1e-6 && (b - x1).abs() < 1e-6, "{y} {a} {b}");
            assert_eq!(t, wt);
        }
        // The section's own block holds only what was drawn; the plan keeps its cut symbols only.
        assert_eq!(s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.len(), 1);
        assert_eq!(texts(&s, plan)?, ["A-A", "A-A"]);
        assert_eq!(inserts(&s, sec)?, [view_block_name(sec)]);
        let t = s.doc()?.block(&annotation_block_name(sec)).ok_or("no block")?;
        let Some(EntityKind::Text(tx)) = t.entities.iter().nth(1).map(|e| &e.kind) else { return Err("no label".into()) };
        assert_eq!((tx.height, tx.halign), (LEVEL_TEXT_MM, HAlign::Left));
        Ok(())
    }

    #[test]
    fn level_edits_move_and_relabel_the_line_in_one_undo_step_and_longer_geometry_widens_it() -> R {
        let (mut s, _plan, sec, l1, l2) = levelled()?;
        let before = levels_of(&s, sec)?;
        let n = undo_len(&s)?;
        s.execute("level", &json!({ "mode": "elevation", "levelId": l2.0, "floorElevationMm": 3300 }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        let moved = levels_of(&s, sec)?;
        assert!((moved[1].0 - local_y(&s, sec, 3300.0)?).abs() < 1e-6 && moved[1].3 == "Level 2 +3300", "{moved:?}");
        s.undo()?;
        assert_eq!(levels_of(&s, sec)?, before);

        s.execute("level", &json!({ "mode": "rename", "levelId": l1.0, "name": "Basement" }))?;
        s.execute("level", &json!({ "mode": "elevation", "levelId": l1.0, "floorElevationMm": -2700 }))?;
        assert_eq!(levels_of(&s, sec)?[0].3, "Basement -2700");

        s.execute("line", &json!({ "points": [[-2000, 500], [9000, 500]] }))?;
        let wide = levels_of(&s, sec)?;
        assert!(
            wide.iter().all(|l| (l.1 - (-2000.0 - LEVEL_OVERRUN_MM)).abs() < 1e-6 && (l.2 - (9000.0 + LEVEL_OVERRUN_MM)).abs() < 1e-6),
            "{wide:?}"
        );
        Ok(())
    }

    #[test]
    fn elevations_get_them_unplaced_verticals_and_plans_do_not_unrelated_commands_rewrite_nothing() -> R {
        let (mut s, plan, sec, _l1, _l2) = levelled()?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let r = s.execute(
            "newelevationline",
            &json!({ "name": "North", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 8000}, "cutTo": {"x": 5000, "y": 8000} }),
        )?;
        let el = Id(r["viewId"].as_u64().ok_or("no id")?);
        assert!(block_of(&s, el).is_none_or(|b| b.entities.is_empty()), "an empty elevation wants nothing");
        s.execute("editview", &json!({ "viewId": el.0 }))?;
        s.execute("line", &json!({ "points": [[0, 0], [5000, 0]] }))?;
        assert_eq!(levels_of(&s, el)?.len(), 2);
        assert!(items(&s, plan)?.iter().all(|i| !matches!(i, Item::Label(..))), "a plan gets no level lines");

        let b0 = block_of(&s, sec).ok_or("no block")?;
        s.execute("line", &json!({ "points": [[100, 100], [200, 100]] }))?;
        assert!(Arc::ptr_eq(&b0, &block_of(&s, sec).ok_or("no block")?), "an unrelated command leaves the block alone");

        s.execute("unplaceview", &json!({ "viewId": sec.0 }))?;
        assert!(items(&s, sec)?.is_empty(), "an unplaced vertical wants nothing");
        Ok(())
    }

    #[test]
    fn level_lines_change_no_marker_link_or_wall_graph_run() -> R {
        let (s, _plan, sec, _l1, _l2) = levelled()?;
        let d = s.doc()?;
        assert!(!items(&s, sec)?.is_empty());
        let mut bare = d.clone();
        bare.blocks.remove(&annotation_block_name(sec));
        let h =
            bare.model.iter().find(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == annotation_block_name(sec))).ok_or("no insert")?.handle;
        bare.model.remove(h);
        let (with, without) = (DrawingProject::new(d)?, DrawingProject::new(&bare)?);
        let all = |p: &DrawingProject| -> String {
            let ids: Vec<Id> = p.views().map(|v| v.id).collect();
            let ents: String = ids.iter().map(|id| format!("{:?}", p.entities(*id).collect::<Vec<_>>())).collect();
            let marks: String = ids.iter().map(|id| format!("{:?}", lamb_core::derivation::markers::compute_markers_for_view(p, *id))).collect();
            let params = lamb_core::derivation::walls::WallRunParams::for_vertical(p.layers());
            let walls = format!("{:?}", lamb_core::derivation::wall_junctions::wall_graph(p, sec, &params));
            format!("{ents}|{marks}|{walls}|{:?}", p.document().links)
        };
        assert_eq!(all(&with), all(&without));
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(d).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert!(plan_changes(&back)?.is_empty(), "level labels survive a DXF round trip unchanged");
        Ok(())
    }

    /// The view's floor-to-floor dimensions, in block order.
    fn dims_of(s: &Session, view: Id) -> T<Vec<Dimension>> {
        let b = block_of(s, view).ok_or("no annotation block")?;
        Ok(b.entities.iter().filter_map(|e| if let EntityKind::Dimension(d) = &e.kind { Some(d.clone()) } else { None }).collect())
    }

    /// The value CADCraft displays for `dm`, under the drawing's own `Standard` style.
    fn measured(s: &Session, dm: &Dimension) -> T<f64> {
        let st = s.doc()?.dim_style(&dm.style).cloned().unwrap_or_default();
        Ok(cadcraft_render::dimension_geometry(dm, &st, 1.0).measurement)
    }

    /// The right end of the section's level lines.
    fn x1_of(s: &Session, view: Id) -> T<f64> {
        Ok(s.doc()?.extents(&Space::Block(view_block_name(view))).max.x + LEVEL_OVERRUN_MM)
    }

    #[test]
    fn consecutive_levels_get_a_vertical_floor_to_floor_dimension_measuring_their_height() -> R {
        let (s, _plan, sec, _l1, _l2) = levelled()?;
        let dims = dims_of(&s, sec)?;
        assert_eq!(dims.len(), 1, "{dims:?}");
        let (dm, x1) = (&dims[0], x1_of(&s, sec)?);
        assert!(close(dm.p13, x1, local_y(&s, sec, 0.0)?) && close(dm.p14, x1, local_y(&s, sec, 3000.0)?), "{dm:?}");
        assert!((dm.defpt.x - (x1 + LEVEL_DIM_OFFSET_MM)).abs() < 1e-6);
        assert!(is_vertical(&dm.kind) && dm.text.is_empty() && dm.style == "Standard" && dm.assoc.is_empty() && dm.block.is_none());
        assert!((measured(&s, dm)? - 3000.0).abs() < 1e-6, "{}", measured(&s, dm)?);
        // The section's own block still holds only what was drawn.
        assert_eq!(s.doc()?.block(&view_block_name(sec)).ok_or("no block")?.entities.len(), 1);
        Ok(())
    }

    #[test]
    fn levels_out_of_order_are_dimensioned_by_elevation() -> R {
        let (mut s, _plan, sec) = fixture()?;
        // Level 1 (order 0) at 0; then order 1 at 6000 and order 2 at 3000.
        s.execute("level", &json!({ "mode": "create", "name": "Roof", "floorElevationMm": 6000, "order": 1 }))?;
        s.execute("level", &json!({ "mode": "create", "name": "Level 2", "floorElevationMm": 3000, "order": 2 }))?;
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.execute("line", &json!({ "points": [[0, 0], [4000, 0]] }))?;
        let dims = dims_of(&s, sec)?;
        let spans: Vec<(f64, f64, f64)> = dims.iter().map(|d| (d.p13.y, d.p14.y, measured(&s, d).unwrap_or(f64::NAN))).collect();
        let want = [(0.0, 3000.0), (3000.0, 6000.0)].map(|(a, b)| (local_y(&s, sec, a), local_y(&s, sec, b)));
        assert_eq!(spans.len(), 2, "{spans:?}");
        for ((a, b, m), (wa, wb)) in spans.iter().zip(want) {
            assert!((a - wa?).abs() < 1e-6 && (b - wb?).abs() < 1e-6 && (m - 3000.0).abs() < 1e-6, "{spans:?}");
        }
        Ok(())
    }

    #[test]
    fn a_level_edit_moves_the_dimension_in_one_undo_step_and_unrelated_commands_leave_it() -> R {
        let (mut s, _plan, sec, _l1, l2) = levelled()?;
        let before = dims_of(&s, sec)?;
        let b0 = block_of(&s, sec).ok_or("no block")?;
        s.execute("line", &json!({ "points": [[100, 100], [200, 100]] }))?;
        assert!(Arc::ptr_eq(&b0, &block_of(&s, sec).ok_or("no block")?), "an unrelated command leaves the block alone");

        let n = undo_len(&s)?;
        s.execute("level", &json!({ "mode": "elevation", "levelId": l2.0, "floorElevationMm": 3200 }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        let moved = dims_of(&s, sec)?;
        assert_eq!(moved.len(), 1);
        assert!((moved[0].p14.y - local_y(&s, sec, 3200.0)?).abs() < 1e-6 && (measured(&s, &moved[0])? - 3200.0).abs() < 1e-6);
        s.undo()?;
        assert_eq!(dims_of(&s, sec)?, before);

        s.execute("level", &json!({ "mode": "elevation", "levelId": l2.0, "floorElevationMm": 0 }))?;
        assert!(dims_of(&s, sec)?.is_empty(), "two levels at one elevation get no dimension");
        assert_eq!(levels_of(&s, sec)?.len(), 2, "but keep both level lines");
        Ok(())
    }

    #[test]
    fn one_level_gets_no_dimension() -> R {
        let (mut s, _plan, sec) = fixture()?;
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.execute("line", &json!({ "points": [[0, 0], [4000, 0]] }))?;
        assert_eq!(levels_of(&s, sec)?.len(), 1);
        assert!(dims_of(&s, sec)?.is_empty());
        Ok(())
    }

    #[test]
    fn a_dimensioned_section_survives_a_dxf_round_trip_and_derivation_ignores_it() -> R {
        let (s, _plan, sec, _l1, _l2) = levelled()?;
        let d = s.doc()?;
        assert_eq!(dims_of(&s, sec)?.len(), 1);
        let mut bare = d.clone();
        bare.blocks.remove(&annotation_block_name(sec));
        let (with, without) = (DrawingProject::new(d)?, DrawingProject::new(&bare)?);
        let all = |p: &DrawingProject| -> String {
            let ids: Vec<Id> = p.views().map(|v| v.id).collect();
            let ents: String = ids.iter().map(|id| format!("{:?}", p.entities(*id).collect::<Vec<_>>())).collect();
            let marks: String = ids.iter().map(|id| format!("{:?}", lamb_core::derivation::markers::compute_markers_for_view(p, *id))).collect();
            format!("{ents}|{marks}|{:?}", p.document().links)
        };
        assert_eq!(all(&with), all(&without));
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(d).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert!(plan_changes(&back)?.is_empty(), "the observer would change nothing after a round trip");
        Ok(())
    }

    /// A viewport made and rebound on a sheet: `view` alone on layout `name`, then a line far enough
    /// across `view` that VPVIEW picks 1:100. Returns the viewport's handle (hex).
    fn pushed_to_1_100(s: &mut Session, view: Id, name: &str) -> T<String> {
        let out = s.execute("newsheet", &json!({ "name": name, "viewIds": [view.0] }))?;
        let hex = out["viewports"][0].as_str().ok_or("no viewport")?.to_owned();
        assert_eq!(scale_of(s, &hex)?, 50.0);
        s.execute("editview", &json!({ "viewId": view.0 }))?;
        s.execute("line", &json!({ "points": [[0, 500], [40000, 500]] }))?;
        s.execute("vpview", &json!({ "handle": hex, "viewId": view.0 }))?;
        assert_eq!(scale_of(s, &hex)?, 100.0);
        Ok(hex)
    }

    fn scale_of(s: &Session, hex: &str) -> T<f64> {
        let h = cadcraft_doc::Handle::parse_hex(hex).ok_or("bad handle")?;
        let EntityKind::Viewport(vp) = &s.doc()?.entity(h).ok_or("gone")?.kind else { return Err("not a viewport".into()) };
        Ok(crate::sheets::viewport_scale(vp))
    }

    /// Every text height in the view's annotation block.
    fn heights(s: &Session, view: Id) -> T<Vec<f64>> {
        Ok(items(s, view)?.into_iter().filter_map(|i| if let Item::Text(_, _, h) | Item::Label(_, _, h) = i { Some(h) } else { None }).collect())
    }

    #[test]
    fn a_section_name_is_the_same_size_on_paper_at_1_50_and_1_100() -> R {
        let (mut s, plan, _sec) = fixture()?;
        assert!(heights(&s, plan)?.iter().all(|h| *h == 250.0));
        let hex = pushed_to_1_100(&mut s, plan, "P1")?;
        let hs = heights(&s, plan)?;
        assert!(!hs.is_empty() && hs.iter().all(|h| *h == 500.0), "5 mm on paper at 1:100: {hs:?}");
        let Item::Line(_, tip) = items(&s, plan)?[1].clone() else { return Err("no arrow".into()) };
        assert_eq!(tip.y.abs(), 2.0 * CUT_ARROW_MM);

        let b0 = block_of(&s, plan).ok_or("no block")?;
        s.execute("line", &json!({ "points": [[100, 9000], [200, 9000]] }))?;
        assert!(Arc::ptr_eq(&b0, &block_of(&s, plan).ok_or("no block")?), "an unrelated command rewrites nothing");
        s.undo()?;

        // One undo restores the viewport and the sizes.
        let n = undo_len(&s)?;
        s.undo()?;
        assert_eq!(undo_len(&s)?, n - 1);
        assert_eq!(scale_of(&s, &hex)?, 50.0);
        assert!(heights(&s, plan)?.iter().all(|h| *h == 250.0));
        Ok(())
    }

    #[test]
    fn a_level_label_and_dimension_follow_the_scale_and_survive_a_round_trip() -> R {
        let (mut s, _plan, sec, _l1, _l2) = levelled()?;
        pushed_to_1_100(&mut s, sec, "S1")?;
        let hs = heights(&s, sec)?;
        assert_eq!(hs, [500.0, 500.0]);
        let x1 = s.doc()?.extents(&Space::Block(view_block_name(sec))).max.x + 2.0 * LEVEL_OVERRUN_MM;
        let dims = dims_of(&s, sec)?;
        assert_eq!(dims.len(), 1);
        assert!((dims[0].p14.x - x1).abs() < 1e-6 && (dims[0].defpt.x - (x1 + 2.0 * LEVEL_DIM_OFFSET_MM)).abs() < 1e-6, "{:?}", dims[0]);
        let st = s.doc()?.dim_style(&dims[0].style).cloned().unwrap_or_default().with_overrides(&dims[0].overrides);
        assert_eq!((dims[0].style.as_str(), st.scale), ("Standard", 100.0));
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert!(plan_changes(&back)?.is_empty(), "the observer would change nothing after a round trip");
        Ok(())
    }

    #[test]
    fn a_view_on_two_sheets_sizes_for_the_first_by_layout_name() -> R {
        let (mut s, plan, _sec) = fixture()?;
        s.execute("newsheet", &json!({ "name": "B1", "viewIds": [plan.0] }))?;
        pushed_to_1_100(&mut s, plan, "A1")?;
        assert!(heights(&s, plan)?.iter().all(|h| *h == 500.0), "A1 comes before B1");
        Ok(())
    }

    #[test]
    fn a_section_that_only_just_fits_without_its_levels_takes_the_next_scale_with_them_inside() -> R {
        let (mut s, _plan, sec, _l1, _l2) = levelled()?;
        // A sheet's single viewport, measured on a throwaway: its paper size at 1:50.
        let out = s.execute("newsheet", &json!({ "name": "S0", "viewIds": [sec.0] }))?;
        let h0 = cadcraft_doc::Handle::parse_hex(out["viewports"][0].as_str().ok_or("no viewport")?).ok_or("bad handle")?;
        let EntityKind::Viewport(vp0) = &s.doc()?.entity(h0).ok_or("gone")?.kind else { return Err("not a viewport".into()) };
        let w = vp0.width;
        s.execute("erase", &json!({ "handles": [h0.hex()] }))?;

        // A block whose width grown 5 % each way just fits that width at 1:50.
        let ext = s.doc()?.extents(&Space::Block(view_block_name(sec)));
        let wide = ext.min.x + w * 50.0 / 1.1 - 1.0;
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.execute("line", &json!({ "points": [[ext.min.x, 500], [wide, 500]] }))?;

        s.execute("newsheet", &json!({ "name": "S1", "viewIds": [sec.0] }))?;
        let vps: Vec<cadcraft_doc::Viewport> = s
            .doc()?
            .layout("S1")
            .ok_or("no layout")?
            .entities
            .iter()
            .filter_map(|e| if let EntityKind::Viewport(vp) = &e.kind { Some(vp.clone()) } else { None })
            .collect();
        let vp = vps.first().ok_or("no viewport")?;
        assert_eq!(crate::sheets::viewport_scale(vp), 100.0, "{vp:?}");

        let d = s.doc()?;
        let (block, ann) = (d.extents(&Space::Block(view_block_name(sec))), d.extents(&Space::Block(annotation_block_name(sec))));
        let shown = block.union(&ann);
        let half = Vec2::new(vp.width * 100.0 / 2.0, vp.view_height / 2.0);
        let window = Bounds2::new(vp.view_center - half, vp.view_center + half);
        assert!(
            window.min.x <= shown.min.x && window.min.y <= shown.min.y && window.max.x >= shown.max.x && window.max.y >= shown.max.y,
            "{window:?} holds {shown:?}"
        );
        Ok(())
    }

    /// The four box lines and label an enlarged area from `min` to `max` wants at 1:50.
    fn box_of(min: (f64, f64), max: (f64, f64), label: &str) -> Vec<Item> {
        let (a, b, c, e) = (v(min.0, min.1), v(max.0, min.1), v(max.0, max.1), v(min.0, max.1));
        let at = v(min.0, max.1 + 0.5 * CUT_TEXT_MM);
        vec![Item::Line(a, b), Item::Line(b, c), Item::Line(c, e), Item::Line(e, a), Item::Label(at, label.to_owned(), CUT_TEXT_MM)]
    }

    fn enlarge(s: &mut Session, view: Id, from: (f64, f64), to: (f64, f64)) -> T<(String, cadcraft_doc::Handle)> {
        let r = s.execute(
            "enlargedsheet",
            &json!({ "viewId": view.0, "from": {"x": from.0, "y": from.1}, "to": {"x": to.0, "y": to.1}, "scale": 10, "show": "view" }),
        )?;
        let h = cadcraft_doc::Handle::parse_hex(r["viewport"].as_str().ok_or("no viewport")?).ok_or("bad handle")?;
        Ok((r["layout"].as_str().ok_or("no layout")?.to_owned(), h))
    }

    #[test]
    fn an_enlarged_area_of_a_plan_gets_a_box_naming_its_sheet_in_the_same_undo_step() -> R {
        let (mut s, plan, _sec) = fixture()?;
        let bare = items(&s, plan)?;
        let n = undo_len(&s)?;
        let (layout, _) = enlarge(&mut s, plan, (1000.0, 500.0), (3000.0, 2000.0))?;
        assert_eq!(undo_len(&s)?, n + 1);
        let got = items(&s, plan)?;
        let want = box_of((1000.0, 500.0), (3000.0, 2000.0), &format!("1/{layout}"));
        assert_eq!(got[..bare.len()], bare[..], "what was there stays first");
        assert_eq!(got[bare.len()..], want[..]);
        assert!(plan_changes(s.doc()?)?.is_empty(), "settled");
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert!(plan_changes(&back)?.is_empty(), "the box survives a DXF round trip unchanged");
        s.undo()?;
        assert_eq!(items(&s, plan)?, bare, "undo removes it");
        Ok(())
    }

    #[test]
    fn an_enlarged_area_of_a_section_gets_the_same_box_after_its_level_lines() -> R {
        let (mut s, plan, sec, _l1, _l2) = levelled()?;
        let (bare, marks) = (items(&s, sec)?, items(&s, plan)?);
        let (layout, _) = enlarge(&mut s, sec, (500.0, 200.0), (2500.0, 1700.0))?;
        let got = items(&s, sec)?;
        assert_eq!(got[..bare.len()], bare[..]);
        assert_eq!(got[bare.len()..], box_of((500.0, 200.0), (2500.0, 1700.0), &format!("1/{layout}"))[..]);
        assert_eq!(items(&s, plan)?, marks, "the plan shows no box for a section's area");
        Ok(())
    }

    #[test]
    fn the_box_follows_its_viewport_and_goes_when_it_is_erased() -> R {
        let (mut s, plan, _sec) = fixture()?;
        let bare = items(&s, plan)?;
        let (layout, h) = enlarge(&mut s, plan, (1000.0, 500.0), (3000.0, 2000.0))?;
        let label = format!("1/{layout}");
        s.doc_mut()?.modify_entity(h, |e| {
            if let EntityKind::Viewport(vp) = &mut e.kind {
                vp.view_center += Vec2::new(500.0, -300.0);
            }
        })?;
        s.execute("line", &json!({ "points": [[100, 100], [200, 100]] }))?;
        let got = items(&s, plan)?;
        assert_eq!(got[bare.len()..], box_of((1500.0, 200.0), (3500.0, 1700.0), &label)[..]);

        let n = undo_len(&s)?;
        s.execute("erase", &json!({ "handles": [h.hex()] }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        assert_eq!(items(&s, plan)?, bare, "erased with its viewport");
        s.undo()?;
        assert_eq!(items(&s, plan)?, got, "and back");
        Ok(())
    }

    #[test]
    fn the_box_is_sized_for_the_views_own_sheet_and_each_enlargement_has_one() -> R {
        let (mut s, plan, _sec) = fixture()?;
        pushed_to_1_100(&mut s, plan, "P1")?;
        let bare = items(&s, plan)?.len();
        let (first, _) = enlarge(&mut s, plan, (1000.0, 500.0), (3000.0, 2000.0))?;
        let (second, _) = enlarge(&mut s, plan, (4000.0, 500.0), (5000.0, 1500.0))?;
        let got = items(&s, plan)?;
        assert_eq!(got.len(), bare + 10);
        let labels: Vec<_> =
            got[bare..].iter().filter_map(|i| if let Item::Label(at, t, h) = i { Some((at.y, t.clone(), *h)) } else { None }).collect();
        assert_eq!(labels, [(2000.0 + 250.0, format!("1/{first}"), 500.0), (1500.0 + 250.0, format!("1/{second}"), 500.0)]);
        Ok(())
    }

    #[test]
    fn the_views_sheet_plots_the_box() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        let (mut s, plan, _sec) = fixture()?;
        s.execute("newsheet", &json!({ "name": "P1", "viewIds": [plan.0] }))?;
        let paper = Space::Paper("P1".into());
        let opts = Options::default();
        let before = build_plot(s.doc()?, &paper, &opts).prims.len();
        enlarge(&mut s, plan, (1000.0, 500.0), (3000.0, 2000.0))?;
        let after = build_plot(s.doc()?, &paper, &opts).prims.len();
        assert!(after >= before + 4, "{before} -> {after}");
        Ok(())
    }

    /// Whether `list` holds a polyline with a point at (x, y).
    fn has_point(list: &DisplayList, x: f64, y: f64) -> bool {
        let at = Vec2::new(x, y);
        list.prims.iter().filter(|p| p.kind == Kind::Polyline).any(|p| list.points(p).iter().any(|q| q.dist(at) < 1e-6))
    }

    #[test]
    fn editing_a_plan_shows_the_arrows_on_the_side_the_section_faces() -> R {
        let (mut s, plan, sec) = fixture()?;
        let space = Space::Block(view_block_name(plan));
        let Item::Line(_, tip) = items(&s, plan)?[1].clone() else { return Err("no arrow".into()) };
        let g = tip.y;
        assert_eq!(g.abs(), CUT_ARROW_MM);
        let list = lamb_overlay(s.doc()?, &space, &Options::default());
        assert!(has_point(&list, 0.0, g) && has_point(&list, 5000.0, g), "both tips");
        assert!(!has_point(&list, 0.0, -g) && !has_point(&list, 5000.0, -g), "not the far side");
        s.execute("viewprops", &json!({ "mode": "flip", "viewId": sec.0, "flipped": true }))?;
        let list = lamb_overlay(s.doc()?, &space, &Options::default());
        assert!(has_point(&list, 0.0, -g) && has_point(&list, 5000.0, -g), "flipped tips");
        assert!(!has_point(&list, 0.0, g) && !has_point(&list, 5000.0, g), "the old side is empty");
        Ok(())
    }

    #[test]
    fn editing_a_section_shows_its_level_lines() -> R {
        let (s, _plan, sec, _l1, _l2) = levelled()?;
        let list = lamb_overlay(s.doc()?, &Space::Block(view_block_name(sec)), &Options::default());
        let got = levels_of(&s, sec)?;
        assert_eq!(got.len(), 2, "{got:?}");
        for (y, a, b, _) in got {
            assert!(has_point(&list, a, y) && has_point(&list, b, y), "level line at {y}");
        }
        Ok(())
    }

    #[test]
    fn model_space_and_sheets_gain_nothing_and_a_plot_shows_the_annotation_once() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        let (mut s, plan, sec) = fixture()?;
        s.execute("newsheet", &json!({ "name": "A301", "viewIds": [plan.0, sec.0] }))?;
        let d = s.doc()?;
        let mut bare = d.clone();
        let handles: Vec<_> = bare
            .model
            .iter()
            .filter(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == annotation_block_name(plan) || i.block == annotation_block_name(sec)))
            .map(|e| e.handle)
            .collect();
        assert!(!handles.is_empty());
        for h in handles {
            bare.remove_entity(h);
        }
        let opts = Options::default();
        let paper = Space::Paper("A301".into());
        for space in [Space::Model, paper.clone()] {
            assert!(draw(d, &space, &opts).prims.is_empty());
            assert_eq!(lamb_overlay(d, &space, &opts).prims.len(), lamb_overlay(&bare, &space, &opts).prims.len());
        }
        // Through a viewport the INSERT draws the block; the overlay adds no second copy.
        let edited = draw(d, &Space::Block(view_block_name(plan)), &opts).prims.len();
        assert!(edited > 0);
        let (with, without) = (build_plot(d, &paper, &opts).prims.len(), build_plot(&bare, &paper, &opts).prims.len());
        assert!(with > without && with - without <= edited, "{with} {without} {edited}");
        assert!(cadcraft_render::overlays(d, &paper, &opts).iter().all(|l| l.prims.is_empty()));
        Ok(())
    }

    /// T703: a LINE drawn in the edited `view` and given the `datum` role; its entity id.
    pub(crate) fn datum_line(s: &mut Session, view: Id, a: [f64; 2], b: [f64; 2]) -> T<Id> {
        let ids = |s: &Session| -> T<Vec<Id>> { Ok(DrawingProject::new(s.doc()?)?.entities(view).map(|e| e.id).collect()) };
        let before = ids(s)?;
        s.execute("line", &json!({ "points": [a, b] }))?;
        let id = ids(s)?.into_iter().find(|i| !before.contains(i)).ok_or("line not drawn in the view")?;
        s.execute("setrole", &json!({ "handles": [format!("{:X}", id.0)], "role": "datum" }))?;
        Ok(id)
    }

    /// [`fixture`] plus `Level 2` with a second placed plan `Upper`, a 4000 × 3000 rectangle in the
    /// section, and in the ground plan a N–S datum at x = 2000 (crossing A-A) and an E–W one at
    /// y = 4000. Returns (session, plan, upper, section, the N–S datum).
    fn gridded() -> T<(Session, Id, Id, Id, Id)> {
        let (mut s, plan, sec) = fixture()?;
        let l2 =
            Id(s.execute("level", &json!({ "mode": "create", "name": "Level 2", "floorElevationMm": 3000 }))?["levelId"].as_u64().ok_or("no id")?);
        let upper = Id(s.execute("newplan", &json!({ "name": "Upper" }))?["viewId"].as_u64().ok_or("no id")?);
        let t = json!({ "originX": 0.0, "originY": 0.0, "rotation": 0.0 });
        s.execute("placeview", &json!({ "viewId": upper.0, "kind": "plan", "levelId": l2.0, "cutElevation": 4200, "transform": t }))?;
        s.execute("editview", &json!({ "viewId": sec.0 }))?;
        s.execute("pline", &json!({ "vertices": [[0, 0], [4000, 0], [4000, 3000], [0, 3000]], "closed": true }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        let ns = datum_line(&mut s, plan, [2000.0, -2000.0], [2000.0, 3000.0])?;
        datum_line(&mut s, plan, [-1000.0, 4000.0], [6000.0, 4000.0])?;
        Ok((s, plan, upper, sec, ns))
    }

    /// The view's bubbles: (centre, radius, name), each circle with the text centred on it.
    fn bubbles_of(s: &Session, view: Id) -> T<Vec<(Vec3, f64, String)>> {
        let it = items(s, view)?;
        let mut out = Vec::new();
        for w in it.windows(2) {
            if let [Item::Circle(c, r), Item::Text(at, name, h)] = w {
                assert!(near(*c, *at) && (h - 0.8 * r).abs() < 1e-9, "{w:?}");
                out.push((*c, *r, name.clone()));
            }
        }
        Ok(out)
    }

    #[test]
    fn grid_lines_are_named_and_bubbled_on_every_plan_and_section_they_cross() -> R {
        let (s, plan, upper, sec, _ns) = gridded()?;
        let b = bubbles_of(&s, plan)?;
        assert_eq!(b.iter().map(|b| b.2.as_str()).collect::<Vec<_>>(), ["1", "1", "A", "A"], "{b:?}");
        let f = b[0].1 / GRID_BUBBLE_MM;
        let off = (GRID_GAP_MM + GRID_BUBBLE_MM) * f;
        assert!(close(b[0].0, 2000.0, -2000.0 - off) && close(b[1].0, 2000.0, 3000.0 + off), "{b:?}");
        assert!(close(b[2].0, -1000.0 - off, 4000.0) && close(b[3].0, 6000.0 + off, 4000.0), "{b:?}");

        // The other plan draws both lines too, with the same bubbles.
        let it = items(&s, upper)?;
        assert!(it.contains(&Item::Line(v(2000.0, -2000.0), v(2000.0, 3000.0))) && it.contains(&Item::Line(v(-1000.0, 4000.0), v(6000.0, 4000.0))));
        let bu = bubbles_of(&s, upper)?;
        assert_eq!(bu.len(), 4);
        assert!(bu.iter().zip(&b).all(|(x, y)| near(x.0, y.0) && x.2 == y.2), "{bu:?}");

        // The section: one vertical line across its block ± the overrun, one bubble at its top.
        let (x, ext) = section_grid(&s, sec)?;
        let fs = bubbles_of(&s, sec)?;
        assert_eq!(fs.len(), 1, "{fs:?}");
        let over = LEVEL_OVERRUN_MM * fs[0].1 / GRID_BUBBLE_MM;
        let lines: Vec<_> =
            items(&s, sec)?.into_iter().filter(|i| matches!(i, Item::Line(a, b) if (a.x - x).abs() < 1e-6 && (b.x - x).abs() < 1e-6)).collect();
        assert_eq!(lines.len(), 2, "the grid line and the bubble's stub: {lines:?}");
        let Item::Line(a, top) = &lines[0] else { return Err("no line".into()) };
        assert!((a.y - (ext.0 - over)).abs() < 1e-6 && (top.y - (ext.1 + over)).abs() < 1e-6, "{a:?} {top:?} {ext:?}");
        let fsf = fs[0].1 / GRID_BUBBLE_MM;
        assert!(close(fs[0].0, x, top.y + (GRID_GAP_MM + GRID_BUBBLE_MM) * fsf) && fs[0].2 == "1", "{fs:?}");
        Ok(())
    }

    /// The local x of the section's derived grid line and its view block's y-extents.
    fn section_grid(s: &Session, sec: Id) -> T<(f64, (f64, f64))> {
        let d = s.doc()?;
        let dp = DrawingProject::new(d)?;
        let o = compute_datum_lines_for_view(&dp, sec)?;
        let g = o.iter().find(|o| (o.to.x - o.from.x).abs() < 1e-9).ok_or("no vertical grid in the section")?;
        let ext = d.extents(&Space::Block(view_block_name(sec)));
        Ok((g.from.x, (ext.min.y, ext.max.y)))
    }

    #[test]
    fn moving_the_datum_moves_the_sections_grid_in_one_undo_step() -> R {
        let (mut s, _plan, _upper, sec, ns) = gridded()?;
        let (x0, _) = section_grid(&s, sec)?;
        let b0 = bubbles_of(&s, sec)?;
        let n = undo_len(&s)?;
        s.execute("move", &json!({ "handles": [format!("{:X}", ns.0)], "delta": [500, 0] }))?;
        assert_eq!(undo_len(&s)?, n + 1);
        let (x1, _) = section_grid(&s, sec)?;
        assert!(((x1 - x0).abs() - 500.0).abs() < 1e-6, "{x0} {x1}");
        let b1 = bubbles_of(&s, sec)?;
        assert!(b1.len() == 1 && (b1[0].0.x - x1).abs() < 1e-6 && (b1[0].0.y - b0[0].0.y).abs() < 1e-6, "{b1:?}");
        assert!(settled(s.doc()?)?);
        s.undo()?;
        assert_eq!(bubbles_of(&s, sec)?, b0);
        assert!((section_grid(&s, sec)?.0 - x0).abs() < 1e-9);
        Ok(())
    }
}

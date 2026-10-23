//! NEWOFFSETSECTION (`nos`) and NEWOFFSETELEVATION (`noe`), View ▸ LAMB: a new section or
//! elevation parallel to the edited one, a typed distance away — a series of parallel cuts through
//! a repeating condition (`docs/01-core-concept/view-creation-and-placement.md` §2, §6).
//!
//! - `newoffsetsection {fromViewId, offsetMm, name?}` / `newoffsetelevation {…}`: `fromViewId` is a
//!   placed vertical view of the command's intent. The new view copies its intent, host, transform
//!   and `flipped`; its cut line is the from-view's, shifted `offsetMm` along the CCW perpendicular
//!   of the first segment's from→to direction in the host's local frame (positive = left of the
//!   cut's own direction; `flipped` never changes the sign). Named `<from name> offset <±mm>` unless
//!   given a name. One undo step, returns `{viewId}`, then edits the new view.

use std::sync::{Mutex, PoisonError};

use cadcraft_doc::Space;
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step, units::parse_distance};
use lamb_core::geometry::Point2;
use lamb_core::model::{AnyView, Id, Placement, VerticalIntent, VerticalView};
use serde_json::{Value, json};

use crate::commands::{bad, create, echo_created, edited_view, enter_space, id_value, other};
use crate::schema::{read_document, view_block_name};

fn word(intent: VerticalIntent) -> &'static str {
    if intent == VerticalIntent::Elevation { "Elevation" } else { "Section" }
}

fn cmd_name(intent: VerticalIntent) -> &'static str {
    if intent == VerticalIntent::Elevation { "newoffsetelevation" } else { "newoffsetsection" }
}

/// `+1200` / `-1200`, whole millimetres (as `annotations.rs`'s private `signed_mm`).
fn signed_mm(z: f64) -> String {
    let r = z.round();
    format!("{:+.0}", if r == 0.0 { 0.0 } else { r })
}

/// The view, when it is a placed vertical of `intent`.
fn placed_vertical(s: &Session, id: Id, intent: VerticalIntent) -> Option<VerticalView> {
    let doc = read_document(s.doc().ok()?).ok()?;
    doc.views.into_iter().find_map(|v| match v {
        AnyView::Vertical(v) if v.id == id && v.intent == intent && matches!(v.placement, Some(Placement::Vertical { .. })) => Some(v),
        _ => None,
    })
}

fn offset_view(s: &mut Session, p: &Value, intent: VerticalIntent) -> Result<Value> {
    let cmd = cmd_name(intent);
    let from = p.get("fromViewId").and_then(id_value).ok_or_else(|| bad(cmd, "`fromViewId` is required"))?;
    let offset = p
        .get("offsetMm")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v != 0.0)
        .ok_or_else(|| bad(cmd, "`offsetMm` must be a finite, non-zero number"))?;
    let src = placed_vertical(s, from, intent).ok_or_else(|| other(format!("fromViewId must be a placed {}", word(intent).to_lowercase())))?;
    let Some(Placement::Vertical { cut_line, host_view_id, transform, flipped }) = src.placement else {
        return Err(other(format!("fromViewId must be a placed {}", word(intent).to_lowercase())));
    };
    let first_to = cut_line.segments.first().map(|g| g.to).ok_or_else(|| other("The from-view's cut line has no segment"))?;
    let (dx, dy) = (first_to.x - cut_line.start.x, first_to.y - cut_line.start.y);
    let len = dx.hypot(dy);
    if len == 0.0 {
        return Err(other("The from-view's cut line has a zero-length first segment"));
    }
    let (ox, oy) = (-dy / len * offset, dx / len * offset);
    let shift = |q: Point2| Point2 { x: q.x + ox, y: q.y + oy };
    let mut moved = cut_line.clone();
    moved.start = shift(moved.start);
    for g in &mut moved.segments {
        g.to = shift(g.to);
    }
    let name = p.get("name").and_then(Value::as_str).map_or_else(|| format!("{} offset {}", src.name, signed_mm(offset)), str::to_owned);
    let placement = Placement::Vertical { cut_line: moved, host_view_id, transform, flipped };
    let r = create(s, AnyView::Vertical(VerticalView { id: Id(0), name: name.clone(), intent, entities: Vec::new(), placement: Some(placement) }))?;
    if let Some(id) = r.get("viewId").and_then(Value::as_u64) {
        enter_space(s, Space::Block(view_block_name(Id(id))))?;
    }
    Ok(json!({ "viewId": r["viewId"], "name": name }))
}

/// NEWOFFSETSECTION.
pub(crate) fn newoffsetsection(s: &mut Session, p: &Value) -> Result<Value> {
    offset_view(s, p, VerticalIntent::Section)
}

/// NEWOFFSETELEVATION.
pub(crate) fn newoffsetelevation(s: &mut Session, p: &Value) -> Result<Value> {
    offset_view(s, p, VerticalIntent::Elevation)
}

// ---------------- interactive ----------------

/// The last distance typed at each command's prompt, for this process.
static LAST_SECTION: Mutex<f64> = Mutex::new(1000.0);
static LAST_ELEVATION: Mutex<f64> = Mutex::new(1000.0);

fn remembered(m: &Mutex<f64>) -> f64 {
    *m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn remember(m: &Mutex<f64>, v: f64) {
    *m.lock().unwrap_or_else(PoisonError::into_inner) = v;
}

/// `Specify offset distance <last>:` in the edited section/elevation.
struct NewOffsetVertical {
    intent: VerticalIntent,
    from: Option<Id>,
}

impl NewOffsetVertical {
    fn store(&self) -> &'static Mutex<f64> {
        if self.intent == VerticalIntent::Elevation { &LAST_ELEVATION } else { &LAST_SECTION }
    }
}

impl Interactive for NewOffsetVertical {
    fn name(&self) -> &'static str {
        cmd_name(self.intent)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Specify offset distance", Accept::NUMBER).default(remembered(self.store()).to_string())
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let intent = self.intent;
        self.from = edited_view(s).filter(|&id| placed_vertical(s, id, intent).is_some());
        if self.from.is_none() {
            s.echo(format!("Switch to a placed {} first", word(intent).to_lowercase()));
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let value = match i {
            Input::Cancel => return Ok(Step::Cancel),
            Input::Enter => remembered(self.store()),
            Input::Text(t) | Input::Keyword(t) => parse_distance(&t).filter(|v| v.is_finite()).ok_or_else(|| other("Requires a number."))?,
            _ => return Ok(Step::Continue),
        };
        let Some(from) = self.from else { return Ok(Step::Done) };
        let params = json!({ "fromViewId": from.0, "offsetMm": value });
        match offset_view(s, &params, self.intent) {
            Ok(r) => {
                remember(self.store(), value);
                let name = r["name"].as_str().unwrap_or_default().to_owned();
                echo_created(s, word(self.intent), &name, &r);
            }
            Err(e) => s.echo(e.to_string()),
        }
        Ok(Step::Done)
    }
}

pub(crate) fn newoffsetsection_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(NewOffsetVertical { intent: VerticalIntent::Section, from: None }))
}

pub(crate) fn newoffsetelevation_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(NewOffsetVertical { intent: VerticalIntent::Elevation, from: None }))
}

#[cfg(test)]
mod tests {
    //! Headless, as `newviews.rs`'s own: the registry is process-global, so every test installs.
    use super::*;
    use crate::commands::install_app;
    use lamb_core::geometry::polyline::line2;
    use lamb_core::geometry::{IDENTITY_TRANSFORM, Transform2D};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type Res<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    const P0: Point2 = Point2 { x: 0.0, y: 0.0 };
    const P5: Point2 = Point2 { x: 5000.0, y: 0.0 };

    /// LAMBPROJECT: `Ground Floor` placed (identity transform) — returns the plan.
    fn project() -> Res<(Session, Id)> {
        install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let r = s.execute("lambproject", &json!({}))?;
        Ok((s, Id(r["viewId"].as_u64().ok_or("no id")?)))
    }

    /// A vertical view on `plan` cut `from`→`to`, edited.
    fn vertical(s: &mut Session, plan: Id, cmd: &str, name: &str, from: Point2, to: Point2) -> Res<Id> {
        let r =
            s.execute(cmd, &json!({ "name": name, "hostViewId": plan.0, "cutFrom": {"x": from.x, "y": from.y}, "cutTo": {"x": to.x, "y": to.y} }))?;
        let id = Id(r["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": id.0 }))?;
        Ok(id)
    }

    fn verticals(s: &Session) -> Res<Vec<VerticalView>> {
        Ok(read_document(s.doc()?)?.views.into_iter().filter_map(|v| if let AnyView::Vertical(v) = v { Some(v) } else { None }).collect())
    }

    fn view(s: &Session, id: Id) -> Res<VerticalView> {
        Ok(verticals(s)?.into_iter().find(|v| v.id == id).ok_or("no view")?)
    }

    fn id_of(r: &Value) -> Res<Id> {
        Ok(Id(r["viewId"].as_u64().ok_or("no viewId")?))
    }

    fn placement(v: &VerticalView) -> Res<(lamb_core::geometry::polyline::Polyline2, Id, Transform2D, Option<bool>)> {
        let Some(Placement::Vertical { cut_line, host_view_id, transform, flipped }) = &v.placement else { return Err("not placed".into()) };
        Ok((cut_line.clone(), *host_view_id, *transform, *flipped))
    }

    fn at(y: f64) -> lamb_core::geometry::polyline::Polyline2 {
        line2(Point2 { x: 0.0, y }, Point2 { x: 5000.0, y })
    }

    #[test]
    fn newoffsetsection_shifts_the_cut_left_names_edits_and_undoes() -> R {
        let (mut s, plan) = project()?;
        let a = vertical(&mut s, plan, "newsectionline", "A-A", P0, P5)?;
        let r = s.execute("newoffsetsection", &json!({ "fromViewId": a.0, "offsetMm": 1200 }))?;
        let id = id_of(&r)?;
        let v = view(&s, id)?;
        assert_eq!((v.name.as_str(), v.intent), ("A-A offset +1200", VerticalIntent::Section));
        assert_eq!(placement(&v)?, (at(1200.0), plan, IDENTITY_TRANSFORM, None));
        assert_eq!(s.state()?.space, Space::Block(view_block_name(id)), "edits the new view");
        let n = verticals(&s)?.len();
        s.undo()?;
        assert_eq!(verticals(&s)?.len(), n - 1, "one undo removes it");

        let r = s.execute("newoffsetsection", &json!({ "fromViewId": a.0, "offsetMm": -1200, "name": "C-C" }))?;
        let v = view(&s, id_of(&r)?)?;
        assert_eq!((v.name.as_str(), placement(&v)?.0), ("C-C", at(-1200.0)));
        Ok(())
    }

    #[test]
    fn a_reversed_cut_offsets_to_its_own_left_and_flipped_is_copied() -> R {
        let (mut s, plan) = project()?;
        let a = vertical(&mut s, plan, "newsectionline", "A-A", P5, P0)?;
        s.execute("viewprops", &json!({ "mode": "flip", "viewId": a.0, "flipped": true }))?;
        let r = s.execute("newoffsetsection", &json!({ "fromViewId": a.0, "offsetMm": 1200 }))?;
        let (cut, _, _, flipped) = placement(&view(&s, id_of(&r)?)?)?;
        assert_eq!(cut, line2(Point2 { x: 5000.0, y: -1200.0 }, Point2 { x: 0.0, y: -1200.0 }));
        assert_eq!(flipped, Some(true), "flipped is copied and never changes the side");
        Ok(())
    }

    #[test]
    fn wrong_kinds_and_a_zero_offset_refuse_and_change_nothing() -> R {
        let (mut s, plan) = project()?;
        let sec = vertical(&mut s, plan, "newsectionline", "A-A", P0, P5)?;
        let ele = vertical(&mut s, plan, "newelevationline", "North", P0, P5)?;
        let loose =
            s.execute("newelevationline", &json!({ "name": "Loose", "hostViewId": plan.0, "cutFrom": {"x": 0, "y": 0}, "cutTo": {"x": 1, "y": 0} }))?;
        let loose = id_of(&loose)?;
        // Unplace it.
        let d = s.doc_mut()?;
        let mut doc = read_document(d)?;
        for v in &mut doc.views {
            if let AnyView::Vertical(v) = v
                && v.id == loose
            {
                v.placement = None;
            }
        }
        crate::schema::write_document(d, &doc)?;
        let n = read_document(s.doc()?)?.views.len();
        for (cmd, from, msg) in [
            ("newoffsetelevation", sec, "fromViewId must be a placed elevation"),
            ("newoffsetsection", ele, "fromViewId must be a placed section"),
            ("newoffsetsection", plan, "fromViewId must be a placed section"),
            ("newoffsetelevation", loose, "fromViewId must be a placed elevation"),
            ("newoffsetsection", sec, "offsetMm"),
        ] {
            let off = if msg == "offsetMm" { 0 } else { 1000 };
            let e = s.execute(cmd, &json!({ "fromViewId": from.0, "offsetMm": off }));
            assert!(e.is_err_and(|e| e.to_string().contains(msg)), "{cmd} {from:?}");
        }
        assert_eq!(read_document(s.doc()?)?.views.len(), n, "a refusal changes nothing");
        // And the elevation form works on an elevation.
        let r = s.execute("newoffsetelevation", &json!({ "fromViewId": ele.0, "offsetMm": 600 }))?;
        let v = view(&s, id_of(&r)?)?;
        assert_eq!((v.name.as_str(), v.intent, placement(&v)?.0), ("North offset +600", VerticalIntent::Elevation, at(600.0)));
        Ok(())
    }

    #[test]
    fn interactive_matches_the_backend_and_enter_reuses_the_distance() -> R {
        let (mut s, plan) = project()?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.start("nos")?;
        assert!(s.running.is_none() && s.log.iter().any(|l| l == "Switch to a placed section first"), "{:?}", s.log);

        vertical(&mut s, plan, "newsectionline", "A-A", P0, P5)?;
        s.start("nos")?;
        assert_eq!(s.current_prompt().map(|p| p.message).as_deref(), Some("Specify offset distance"));
        s.input(Input::Text("1200".into()))?;
        assert!(s.running.is_none(), "done");
        let first = verticals(&s)?.into_iter().find(|v| v.name == "A-A offset +1200").ok_or("no view")?;
        assert_eq!(placement(&first)?, (at(1200.0), plan, IDENTITY_TRANSFORM, None));
        assert_eq!(s.state()?.space, Space::Block(view_block_name(first.id)));
        assert!(s.log.iter().any(|l| l == &format!("Section \"A-A offset +1200\" created (view {}).", first.id.0)), "{:?}", s.log);

        // Enter takes the remembered 1200, from the now-edited offset view.
        s.start("newoffsetsection")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Specify offset distance <1200>:"));
        s.input(Input::Enter)?;
        let second = verticals(&s)?.into_iter().find(|v| v.name == "A-A offset +1200 offset +1200").ok_or("no second view")?;
        assert_eq!(placement(&second)?.0, at(2400.0));

        s.start("noe")?;
        assert!(s.running.is_none() && s.log.iter().any(|l| l == "Switch to a placed elevation first"), "{:?}", s.log);
        Ok(())
    }
}

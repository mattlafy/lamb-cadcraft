//! A layer's one hatch (LAMB T699): a layer may carry a hatch spec, and a LAMB view with layer
//! hatches on fills every closed shape drawn on that layer with it.
//!
//! The spec is `Layer.ext["LAMB"]["hatch"]` ([`LayerHatch`]), written by LAYERHATCH; the per-view
//! switch is [`LambDocument::layer_hatch_views`](crate::schema::LambDocument::layer_hatch_views),
//! written by LAYERHATCHES. [`layer_fills`] computes the fills a view gets, by the closed-area
//! rule: the layer's curves in the view block and the view's assembly block (view-local) make an
//! arrangement ([`cadcraft_geom::region::faces`]), and a face is filled when a point inside it has
//! an odd number of +X ray crossings against the raw segments, a duplicate counted once per copy.
//! So a stud's outline is filled, a doubled stud's two outlines fill both halves, and a wall band
//! drawn as an outer and an inner loop is filled while the room inside stays empty. LAYERFILLS
//! reports them. Nothing here writes a fill into the drawing.

use cadcraft_doc::{Drawing, Hatch, HatchLoop, Layer, library};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::region::faces;
use cadcraft_geom::{PolyVertex, Vec2};
use lamb_core::geometry::polyline::{point_at_param, resolved_segments};
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::Id;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::choices::{echo_numbered, pick_numbered};
use crate::commands::{bad, edited_view, id_value, other};
use crate::convert::to_polyline;
use crate::schema::{EXT_KEY, assembly_block_name, read_document, view_block_name, write_document};

/// The key of the hatch spec inside a layer's `ext["LAMB"]`.
pub(crate) const HATCH_KEY: &str = "hatch";

/// A layer's hatch: a library pattern (upper case; `SOLID` is a solid fill), its scale, angle in
/// degrees and transparency (0..=90 %). Colour is always ByLayer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerHatch {
    pub pattern: String,
    #[serde(default = "one")]
    pub scale: f64,
    #[serde(default)]
    pub angle: f64,
    #[serde(default)]
    pub transparency: u8,
}

fn one() -> f64 {
    1.0
}

/// The layer's hatch spec; none when absent or unreadable.
pub fn layer_hatch(l: &Layer) -> Option<LayerHatch> {
    serde_json::from_value(l.ext.get(EXT_KEY)?.get(HATCH_KEY)?.clone()).ok()
}

/// Stores `spec` as the layer's hatch, or with `None` removes it (and the `LAMB` slot when
/// nothing else is left in it).
pub fn set_layer_hatch(l: &mut Layer, spec: Option<&LayerHatch>) -> std::result::Result<(), String> {
    match spec {
        Some(h) => {
            let v = serde_json::to_value(h).map_err(|e| e.to_string())?;
            let slot = l.ext.entry(EXT_KEY.to_owned()).or_insert_with(|| Value::Object(serde_json::Map::new()));
            let Some(o) = slot.as_object_mut() else { return Err(format!("layer {:?}'s ext[\"LAMB\"] is not an object", l.name)) };
            o.insert(HATCH_KEY.to_owned(), v);
        }
        None => {
            let empty = match l.ext.get_mut(EXT_KEY).and_then(Value::as_object_mut) {
                Some(o) => {
                    o.remove(HATCH_KEY);
                    o.is_empty()
                }
                None => false,
            };
            if empty {
                l.ext.remove(EXT_KEY);
            }
        }
    }
    Ok(())
}

/// The points of a converted entity, arcs flattened to chords of at most 10°.
fn flattened(poly: &lamb_core::geometry::Polyline2) -> Vec<Vec2> {
    let mut out = vec![Vec2::new(poly.start.x, poly.start.y)];
    let mut segs = resolved_segments(poly);
    if poly.closed
        && let Some(last) = segs.last()
        && (last.to.x != poly.start.x || last.to.y != poly.start.y)
    {
        segs.push(lamb_core::geometry::polyline::ResolvedSegment { index: segs.len(), from: last.to, to: poly.start, bulge: 0.0 });
    }
    for seg in &segs {
        let sweep = 4.0 * seg.bulge.atan();
        let n = if seg.bulge == 0.0 || !sweep.is_finite() { 1 } else { ((sweep.abs() / 10f64.to_radians()).ceil() as usize).clamp(1, 360) };
        for i in 1..=n {
            let p = if i == n { seg.to } else { point_at_param(seg, i as f64 / n as f64) };
            out.push(Vec2::new(p.x, p.y));
        }
    }
    out
}

/// Odd when `p` has an odd number of +X ray crossings against `polys`' segments (half-open in y).
fn odd_crossings(polys: &[Vec<Vec2>], p: Vec2) -> bool {
    let mut n = 0usize;
    for pl in polys {
        for w in pl.windows(2) {
            let [a, b] = w else { continue };
            if (a.y > p.y) != (b.y > p.y) {
                let x = a.x + (b.x - a.x) * (p.y - a.y) / (b.y - a.y);
                if x > p.x {
                    n += 1;
                }
            }
        }
    }
    n % 2 == 1
}

fn hatch_loop(lp: &[Vec2], outer: bool) -> HatchLoop {
    HatchLoop { vertices: lp.iter().map(|p| PolyVertex::new(*p)).collect(), outer, sources: Vec::new() }
}

/// The fills view `view` gets from its layers' hatches, each `(layer, hatch)` in view-local
/// coordinates, by the closed-area rule (module doc): one HATCH per filled face, its outer loop
/// first and the outlines of separate shapes inside it as further loops. Empty when the view has
/// layer hatches off. Err when there is no such view.
pub(crate) fn layer_fills(d: &Drawing, view: Id) -> std::result::Result<Vec<(String, Hatch)>, String> {
    let doc = read_document(d).map_err(|e| e.to_string())?;
    if !doc.views.iter().any(|v| v.id() == view) {
        return Err("No such view".into());
    }
    if !doc.layer_hatch_views.contains(&view) {
        return Ok(Vec::new());
    }
    let blocks: Vec<_> = [view_block_name(view), assembly_block_name(view)].iter().filter_map(|n| d.block(n).cloned()).collect();
    let mut out = Vec::new();
    for layer in &d.layers {
        let Some(spec) = layer_hatch(layer) else { continue };
        let polys: Vec<Vec<Vec2>> = blocks
            .iter()
            .flat_map(|b| b.entities.iter())
            .filter(|e| e.common.layer.eq_ignore_ascii_case(&layer.name))
            .filter_map(|e| to_polyline(e).ok())
            .map(|c| flattened(&c.polyline))
            .filter(|p| p.len() >= 2)
            .collect();
        if polys.is_empty() {
            continue;
        }
        for f in faces(&polys, TOLERANCE.coincidence_mm) {
            let Some(p) = f.interior_point() else { continue };
            if !odd_crossings(&polys, p) {
                continue;
            }
            let mut loops = vec![hatch_loop(&f.outer, true)];
            loops.extend(f.holes.iter().map(|h| hatch_loop(h, false)));
            let hatch = Hatch {
                solid: spec.pattern.eq_ignore_ascii_case("SOLID"),
                pattern: spec.pattern.clone(),
                loops,
                scale: spec.scale,
                angle: spec.angle.to_radians(),
                associative: false,
                style: 0,
                elevation: 0.0,
                gradient: None,
                origin: Vec2::default(),
                background: None,
                pattern_lines: Vec::new(),
            };
            out.push((layer.name.clone(), hatch));
        }
    }
    Ok(out)
}

fn num(cmd: &str, p: &Value, key: &str) -> Result<Option<f64>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().filter(|x| x.is_finite()).map(Some).ok_or_else(|| bad(cmd, &format!("`{key}` must be a number"))),
    }
}

/// LAYERHATCH `{layer, pattern?, scale?, angle?, transparency?, off?}`: gives `layer` its one
/// hatch, or with `off: true` removes it. Unset values keep the layer's current ones, else
/// `SOLID`, scale 1, angle 0° and transparency 0. Refuses an unknown layer or pattern, a scale
/// that isn't positive and a transparency outside 0..=90, changing nothing. Returns `{layer,
/// hatch, message}`, `hatch` null when off.
pub fn layerhatch(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layerhatch";
    let name = p.get("layer").and_then(Value::as_str).ok_or_else(|| bad(CMD, "`layer` must be a string"))?;
    let off = match p.get("off") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(bad(CMD, "`off` must be true or false")),
    };
    let layer = s.doc()?.layer(name).ok_or_else(|| other(format!("No layer named \"{name}\"")))?;
    let layer_name = layer.name.clone();
    let current = layer_hatch(layer);
    let spec = if off {
        None
    } else {
        let pattern = match p.get("pattern") {
            None | Some(Value::Null) => current.as_ref().map_or_else(|| "SOLID".to_owned(), |c| c.pattern.clone()),
            Some(Value::String(t)) => t.clone(),
            Some(_) => return Err(bad(CMD, "`pattern` must be a string")),
        };
        let found = library::pattern(pattern.trim()).ok_or_else(|| other(format!("No hatch pattern named \"{}\"", pattern.trim())))?;
        let scale = num(CMD, p, "scale")?.or(current.as_ref().map(|c| c.scale)).unwrap_or(1.0);
        if scale <= 0.0 {
            return Err(bad(CMD, "`scale` must be positive"));
        }
        let angle = num(CMD, p, "angle")?.or(current.as_ref().map(|c| c.angle)).unwrap_or(0.0);
        let transparency = num(CMD, p, "transparency")?.or(current.as_ref().map(|c| f64::from(c.transparency))).unwrap_or(0.0);
        if !(0.0..=90.0).contains(&transparency) || transparency.fract() != 0.0 {
            return Err(bad(CMD, "`transparency` must be a whole number 0 to 90"));
        }
        Some(LayerHatch { pattern: found.name.to_uppercase(), scale, angle, transparency: transparency as u8 })
    };
    if spec != current {
        let l = s.doc_mut()?.layer_mut(&layer_name).ok_or_else(|| other("layer vanished"))?;
        set_layer_hatch(l, spec.as_ref()).map_err(other)?;
    }
    let message = match &spec {
        None => format!("Layer {layer_name}: no hatch"),
        Some(h) => format!("Layer {layer_name}: hatch {} scale {} angle {} transparency {}", h.pattern, h.scale, h.angle, h.transparency),
    };
    s.echo(message.clone());
    Ok(json!({ "layer": layer_name, "hatch": spec, "message": message }))
}

/// The `view` parameter, defaulting to the edited view; Err naming `cmd` when neither.
fn view_param(s: &Session, cmd: &str, p: &Value) -> Result<Id> {
    match p.get("view") {
        None | Some(Value::Null) => edited_view(s).ok_or_else(|| other("Edit a view first (EDITVIEW), or give `view`")),
        Some(v) => id_value(v).ok_or_else(|| bad(cmd, "`view` must be a view id")),
    }
}

/// LAYERHATCHES `{view?, on}`: turns layer hatches on or off in `view` (default: the edited view).
/// Returns `{view, on, fills, message}`, `fills` counting what the view now gets. A view already in
/// that state is left alone.
pub fn layerhatches(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layerhatches";
    let view = view_param(s, CMD, p)?;
    let on = p.get("on").and_then(Value::as_bool).ok_or_else(|| bad(CMD, "`on` must be true or false"))?;
    let mut doc = read_document(s.doc()?).map_err(|e| other(e.to_string()))?;
    if !doc.views.iter().any(|v| v.id() == view) {
        return Err(other("No such view"));
    }
    if doc.layer_hatch_views.contains(&view) != on {
        doc.layer_hatch_views.retain(|v| *v != view);
        if on {
            doc.layer_hatch_views.push(view);
        }
        write_document(s.doc_mut()?, &doc).map_err(|e| other(e.to_string()))?;
    }
    let fills = layer_fills(s.doc()?, view).map_err(other)?.len();
    let message = if on { format!("Layer hatches on: {fills} fills") } else { "Layer hatches off".to_owned() };
    s.echo(message.clone());
    Ok(json!({ "view": view.0, "on": on, "fills": fills, "message": message }))
}

/// LAYERFILLS `{view?}` (query): the fills [`layer_fills`] gives `view` (default: the edited view),
/// `{view, on, fills: [{layer, pattern, loop: [[x, y]…], holes: [[[x, y]…]…]}]}`.
pub fn layerfills(s: &mut Session, p: &Value) -> Result<Value> {
    let view = view_param(s, "layerfills", p)?;
    let d = s.doc()?;
    let on = read_document(d).map_err(|e| other(e.to_string()))?.layer_hatch_views.contains(&view);
    let pts = |l: &HatchLoop| l.vertices.iter().map(|v| json!([v.p.x, v.p.y])).collect::<Vec<_>>();
    let fills: Vec<Value> = layer_fills(d, view)
        .map_err(other)?
        .iter()
        .map(|(layer, h)| {
            json!({
                "layer": layer,
                "pattern": h.pattern,
                "loop": h.loops.first().map(pts).unwrap_or_default(),
                "holes": h.loops.iter().skip(1).map(pts).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(json!({ "view": view.0, "on": on, "fills": fills }))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Layer,
    Pattern,
    Scale,
    Angle,
    Transparency,
}

/// LAYERHATCH's interactive form: layer (with a numbered `?` list), pattern or Off, scale, angle,
/// transparency.
struct LayerHatchM {
    phase: Phase,
    layer: String,
    current: Option<LayerHatch>,
    params: serde_json::Map<String, Value>,
}

impl LayerHatchM {
    fn run(&self, s: &mut Session, mut p: serde_json::Map<String, Value>) {
        p.insert("layer".into(), json!(self.layer));
        if let Err(e) = layerhatch(s, &Value::Object(p)) {
            s.echo(e.to_string());
        }
    }
    fn fmt(x: f64) -> String {
        format!("{x}")
    }
}

impl Interactive for LayerHatchM {
    fn name(&self) -> &'static str {
        "layerhatch"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        let c = self.current.as_ref();
        match self.phase {
            Phase::Layer => Prompt::new("Enter layer name", Accept::TEXT).kw(&["?"]),
            Phase::Pattern => {
                Prompt::new("Enter pattern name", Accept::TEXT).kw(&["Off"]).default(c.map_or_else(|| "SOLID".to_owned(), |h| h.pattern.clone()))
            }
            Phase::Scale => Prompt::new("Specify scale", Accept::NUMBER).default(Self::fmt(c.map_or(1.0, |h| h.scale))),
            Phase::Angle => Prompt::new("Specify angle", Accept::NUMBER).default(Self::fmt(c.map_or(0.0, |h| h.angle))),
            Phase::Transparency => {
                Prompt::new("Specify transparency (0-90)", Accept::NUMBER).default(Self::fmt(c.map_or(0.0, |h| f64::from(h.transparency))))
            }
        }
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        s.echo("LAYERHATCH: gives a layer one hatch, which fills every closed shape on it in views with layer hatches on (LAYERHATCHES).");
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let text = match i {
            Input::Cancel => return Ok(Step::Cancel),
            Input::Enter => String::new(),
            Input::Text(t) | Input::Keyword(t) => t.trim().to_owned(),
            _ => return Ok(Step::Continue),
        };
        match self.phase {
            Phase::Layer => {
                if text.is_empty() {
                    return Ok(Step::Cancel);
                }
                let layers: Vec<String> = s.doc().map(|d| d.layers.iter().map(|l| l.name.clone()).collect()).unwrap_or_default();
                if text == "?" {
                    echo_numbered(s, &layers);
                    return Ok(Step::Continue);
                }
                let names: Vec<&str> = layers.iter().map(String::as_str).collect();
                let Some(name) = pick_numbered(&names, &text).and_then(|n| layers.get(n)) else {
                    s.echo(format!("No layer named \"{text}\""));
                    return Ok(Step::Continue);
                };
                self.layer = name.clone();
                self.current = s.doc().ok().and_then(|d| d.layer(name)).and_then(layer_hatch);
                self.phase = Phase::Pattern;
            }
            Phase::Pattern => {
                if text.eq_ignore_ascii_case("off") || text.eq_ignore_ascii_case("o") {
                    let mut p = serde_json::Map::new();
                    p.insert("off".into(), json!(true));
                    self.run(s, p);
                    return Ok(Step::Done);
                }
                let pattern = if text.is_empty() { self.current.as_ref().map_or_else(|| "SOLID".to_owned(), |h| h.pattern.clone()) } else { text };
                if library::pattern(&pattern).is_none() {
                    s.echo(format!("No hatch pattern named \"{pattern}\""));
                    return Ok(Step::Continue);
                }
                self.params.insert("pattern".into(), json!(pattern));
                self.phase = Phase::Scale;
            }
            Phase::Scale | Phase::Angle | Phase::Transparency => {
                let key = match self.phase {
                    Phase::Scale => "scale",
                    Phase::Angle => "angle",
                    _ => "transparency",
                };
                if !text.is_empty() {
                    let Some(x) = text.parse::<f64>().ok().filter(|x| x.is_finite()) else {
                        s.echo("Requires a number");
                        return Ok(Step::Continue);
                    };
                    let ok = match self.phase {
                        Phase::Scale => x > 0.0,
                        Phase::Transparency => (0.0..=90.0).contains(&x) && x.fract() == 0.0,
                        _ => true,
                    };
                    if !ok {
                        s.echo(if self.phase == Phase::Scale { "Requires a positive number" } else { "Requires a whole number 0 to 90" });
                        return Ok(Step::Continue);
                    }
                    self.params.insert(key.into(), json!(x));
                }
                self.phase = match self.phase {
                    Phase::Scale => Phase::Angle,
                    Phase::Angle => Phase::Transparency,
                    _ => {
                        let p = self.params.clone();
                        self.run(s, p);
                        return Ok(Step::Done);
                    }
                };
            }
        }
        Ok(Step::Continue)
    }
}

pub(crate) fn layerhatch_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(LayerHatchM { phase: Phase::Layer, layer: String::new(), current: None, params: serde_json::Map::new() }))
}

/// LAYERHATCHES' interactive form, on the edited view.
struct LayerHatchesM {
    view: Id,
    on: bool,
}

impl Interactive for LayerHatchesM {
    fn name(&self) -> &'static str {
        "layerhatches"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Layer hatches in this view", Accept::TEXT)
            .kw(&["On", "Off"])
            .options_only()
            .default(if self.on { "On" } else { "Off" }.to_owned())
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let Some(v) = edited_view(s) else {
            s.echo("LAYERHATCHES needs a view: open one from the Views palette or with EDITVIEW.");
            return Ok(Step::Done);
        };
        self.view = v;
        self.on = s.doc().ok().and_then(|d| read_document(d).ok()).is_some_and(|d| d.layer_hatch_views.contains(&v));
        Ok(Step::Continue)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let on = match i {
            Input::Cancel => return Ok(Step::Cancel),
            Input::Enter => self.on,
            Input::Text(t) | Input::Keyword(t) => match t.trim().to_ascii_lowercase().as_str() {
                "on" => true,
                "off" => false,
                _ => {
                    s.echo("On or Off");
                    return Ok(Step::Continue);
                }
            },
            _ => return Ok(Step::Continue),
        };
        if let Err(e) = layerhatches(s, &json!({ "view": self.view.0, "on": on })) {
            s.echo(e.to_string());
        }
        Ok(Step::Done)
    }
}

pub(crate) fn layerhatches_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(LayerHatchesM { view: Id(0), on: false }))
}

#[cfg(test)]
mod tests {
    use super::*;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    /// A new plan, edited, on layer `SPF` (current), observers installed.
    fn plan() -> T<(Session, Id)> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        crate::maintain::install(&mut s);
        let plan = Id(s.execute("newplan", &json!({ "name": "Ground" }))?["viewId"].as_u64().ok_or("no id")?);
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("layer.new", &json!({ "name": "SPF", "current": true }))?;
        Ok((s, plan))
    }

    fn rect(s: &mut Session, x0: f64, y0: f64, x1: f64, y1: f64) -> R {
        s.execute("pline", &json!({ "vertices": [[x0, y0], [x1, y0], [x1, y1], [x0, y1]], "closed": true }))?;
        Ok(())
    }

    fn fills(s: &Session, view: Id) -> T<Vec<(String, Hatch)>> {
        Ok(layer_fills(s.doc()?, view)?)
    }

    fn area(h: &Hatch) -> f64 {
        h.loops.iter().map(|l| cadcraft_geom::shoelace(&l.vertices.iter().map(|v| v.p).collect::<Vec<_>>())).sum()
    }

    #[test]
    fn closed_shapes_on_a_hatched_layer_are_filled_by_the_closed_area_rule() -> R {
        let (mut s, plan) = plan()?;
        rect(&mut s, 0.0, 0.0, 38.0, 89.0)?;
        // No spec, switch off: nothing.
        assert!(fills(&s, plan)?.is_empty());
        s.execute("layerhatch", &json!({ "layer": "spf", "pattern": "x" }))?;
        assert!(fills(&s, plan)?.is_empty(), "switch off");
        let r = s.execute("layerhatches", &json!({ "on": true }))?;
        assert_eq!(r["fills"], 1);
        let f = fills(&s, plan)?;
        assert_eq!(f.len(), 1);
        let (layer, h) = &f[0];
        assert_eq!((layer.as_str(), h.pattern.as_str(), h.solid, h.associative, h.style, h.loops.len()), ("SPF", "X", false, false, 0, 1));
        assert!((area(h) - 38.0 * 89.0).abs() < 1e-6, "{}", area(h));
        // A second stud sharing an edge: two fills.
        rect(&mut s, 38.0, 0.0, 76.0, 89.0)?;
        assert_eq!(fills(&s, plan)?.len(), 2);
        // A layer without a spec gives none.
        s.execute("layer.new", &json!({ "name": "OTHER", "current": true }))?;
        rect(&mut s, 500.0, 0.0, 600.0, 100.0)?;
        assert_eq!(fills(&s, plan)?.len(), 2);
        // Turned off: none.
        s.execute("layerhatch", &json!({ "layer": "SPF", "off": true }))?;
        assert!(fills(&s, plan)?.is_empty());
        assert!(s.doc()?.layer("SPF").is_some_and(|l| l.ext.is_empty()));
        Ok(())
    }

    #[test]
    fn a_ring_of_two_nested_rectangles_fills_the_ring_only() -> R {
        let (mut s, plan) = plan()?;
        rect(&mut s, 0.0, 0.0, 4000.0, 3000.0)?;
        rect(&mut s, 200.0, 200.0, 3800.0, 2800.0)?;
        s.execute("layerhatch", &json!({ "layer": "SPF", "pattern": "SOLID", "transparency": 70 }))?;
        s.execute("layerhatches", &json!({ "view": plan.0, "on": true }))?;
        let f = fills(&s, plan)?;
        assert_eq!(f.len(), 1);
        assert!(f[0].1.solid && f[0].1.loops.len() == 2);
        assert!((area(&f[0].1) - (4000.0 * 3000.0 - 3600.0 * 2600.0)).abs() < 1e-6, "{}", area(&f[0].1));
        let q = s.execute("layerfills", &json!({}))?;
        assert_eq!(q["fills"][0]["layer"], "SPF");
        assert_eq!(q["fills"][0]["loop"].as_array().map(Vec::len), Some(4));
        assert_eq!(q["fills"][0]["holes"].as_array().map(Vec::len), Some(1));
        Ok(())
    }

    #[test]
    fn an_assembly_member_on_a_hatched_layer_is_filled() -> R {
        let (mut s, plan) = crate::assembly::tests::room("exterior_face", true)?;
        let detail = read_document(s.doc()?)?.views.iter().find(|v| v.name() == "D1").map(lamb_core::model::AnyView::id).ok_or("no D1")?;
        s.execute("editview", &json!({ "viewId": detail.0 }))?;
        s.execute("layer.new", &json!({ "name": "SPF", "current": true }))?;
        let before: Vec<_> = s.doc()?.block(&view_block_name(detail)).map(|b| b.entities.iter().map(|e| e.handle).collect()).unwrap_or_default();
        rect(&mut s, 100.0, 50.0, 138.0, 200.0)?;
        let stud = s
            .doc()?
            .block(&view_block_name(detail))
            .and_then(|b| b.entities.iter().map(|e| e.handle).find(|h| !before.contains(h)))
            .ok_or("no stud")?;
        let before: Vec<_> = s.doc()?.block(&view_block_name(detail)).map(|b| b.entities.iter().map(|e| e.handle).collect()).unwrap_or_default();
        s.execute("dimaligned", &json!({ "p1": [100, 0], "p2": [500, 0], "at": [300, -300] }))?;
        let pitch = s
            .doc()?
            .block(&view_block_name(detail))
            .and_then(|b| b.entities.iter().map(|e| e.handle).find(|h| !before.contains(h)))
            .ok_or("no dimension")?;
        s.execute("repeat", &json!({ "dimension": pitch.hex(), "handles": [stud.hex()] }))?;
        s.execute("editview", &json!({ "viewId": plan.0 }))?;
        s.execute("assemblyplan", &json!({ "viewId": plan.0 }))?;
        let in_assembly = s.doc()?.block(&assembly_block_name(plan)).map_or(0, |b| b.entities.iter().filter(|e| e.common.layer == "SPF").count());
        assert!(in_assembly >= 4, "{in_assembly}");
        assert!(s.doc()?.block(&view_block_name(plan)).is_some_and(|b| b.entities.iter().all(|e| e.common.layer != "SPF")));
        s.execute("layerhatch", &json!({ "layer": "SPF" }))?;
        s.execute("layerhatches", &json!({ "on": true }))?;
        let f = fills(&s, plan)?;
        assert!(!f.is_empty() && f.iter().all(|(l, h)| l == "SPF" && h.pattern == "SOLID"), "{f:?}");
        // Each laid stud copy is 38 × 150.
        assert!(f.iter().all(|(_, h)| (area(h) - 38.0 * 150.0).abs() < 1e-3), "{:?}", f.iter().map(|(_, h)| area(h)).collect::<Vec<_>>());
        Ok(())
    }

    #[test]
    fn the_spec_and_switch_round_trip_and_undo_and_bad_input_changes_nothing() -> R {
        let (mut s, plan) = plan()?;
        s.execute("layerhatch", &json!({ "layer": "SPF", "pattern": "ansi31", "scale": 2, "angle": 45, "transparency": 50 }))?;
        let spec = LayerHatch { pattern: "ANSI31".into(), scale: 2.0, angle: 45.0, transparency: 50 };
        s.execute("layerhatches", &json!({ "on": true }))?;
        let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc()?).as_bytes()).map_err(|e| format!("{e:?}"))?;
        assert_eq!(back.layer("SPF").and_then(layer_hatch), Some(spec.clone()));
        assert_eq!(read_document(&back)?.layer_hatch_views, [plan]);
        // A later LAYERROLE keeps the hatch.
        let l = s.doc_mut()?.layer_mut("SPF").ok_or("no SPF")?;
        crate::schema::set_layer_data(l, &crate::schema::LayerData::default())?;
        assert_eq!(s.doc()?.layer("SPF").and_then(layer_hatch), Some(spec.clone()));
        // Refusals change nothing.
        assert!(s.execute("layerhatch", &json!({ "layer": "SPF", "pattern": "NOPE" })).is_err());
        assert!(s.execute("layerhatch", &json!({ "layer": "MISSING", "pattern": "SOLID" })).is_err());
        assert!(s.execute("layerhatch", &json!({ "layer": "SPF", "transparency": 95 })).is_err());
        assert_eq!(s.doc()?.layer("SPF").and_then(layer_hatch), Some(spec.clone()));
        // Undo restores the previous spec and the switch.
        s.execute("layerhatch", &json!({ "layer": "SPF", "pattern": "SOLID" }))?;
        s.undo()?;
        assert_eq!(s.doc()?.layer("SPF").and_then(layer_hatch), Some(spec));
        s.undo()?;
        assert!(read_document(s.doc()?)?.layer_hatch_views.is_empty());
        s.undo()?;
        assert_eq!(s.doc()?.layer("SPF").and_then(layer_hatch), None);
        Ok(())
    }

    #[test]
    fn interactive_layer_prompt_lists_numbered_and_a_number_picks() -> R {
        let (mut s, plan) = plan()?;
        s.start("layerhatch")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter layer name or [?]:"));
        let layers: Vec<String> = s.doc()?.layers.iter().enumerate().map(|(n, l)| format!("{}  {}", n + 1, l.name)).collect();
        let at = layers.iter().position(|r| r.ends_with("  SPF")).ok_or("no SPF row")?;
        let before = s.log.len();
        s.input(Input::Keyword("?".into()))?;
        assert_eq!(s.log[before..], layers[..], "{:?}", s.log);
        s.input(Input::Text(format!("{}", at + 1)))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter pattern name or [Off] <SOLID>:"));
        s.input(Input::Text("nope".into()))?;
        assert!(s.log.iter().any(|l| l == "No hatch pattern named \"nope\""));
        s.input(Input::Text("x".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Specify scale <1>:"));
        s.input(Input::Enter)?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Specify angle <0>:"));
        s.input(Input::Text("30".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Specify transparency (0-90) <0>:"));
        s.input(Input::Enter)?;
        assert!(s.running.is_none());
        assert_eq!(s.doc()?.layer("SPF").and_then(layer_hatch), Some(LayerHatch { pattern: "X".into(), scale: 1.0, angle: 30.0, transparency: 0 }));
        // LAYERHATCHES on the edited view.
        s.start("layerhatches")?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Layer hatches in this view [On/Off] <Off>:"));
        s.input(Input::Keyword("On".into()))?;
        assert_eq!(read_document(s.doc()?)?.layer_hatch_views, [plan]);
        // And Off at the pattern prompt clears the spec.
        s.start("layerhatch")?;
        s.input(Input::Text("spf".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter pattern name or [Off] <X>:"));
        s.input(Input::Keyword("Off".into()))?;
        assert_eq!(s.doc()?.layer("SPF").and_then(layer_hatch), None);
        Ok(())
    }
}

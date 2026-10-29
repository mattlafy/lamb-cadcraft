//! The LAMB group of the Properties palette (TS `PropertiesPanel.tsx`'s Role select, `CutSummary`,
//! `SeenSummary`): a Role row whose choices run SETROLE, and for one plan polyline in a placed plan
//! view whose resolved role is cut or seen, where its height comes from at the stepped segment —
//! each such source row offering `Go to <view>` choices (EDITVIEW) for the placed vertical views
//! whose linked lines supply it.
//! One selected layout viewport gets a View row instead, whose choices run VPVIEW.
//! The desktop app maps [`LambGroup`] onto the UI's generic property-group slot.

use cadcraft_doc::Space;
use cadcraft_doc::{Drawing, EntityKind, Handle};
use lamb_core::derivation::read::{EntityRef, ProjectRead, ViewKind};
use lamb_core::derivation::resolve::{ProvenanceKind, ResolvedSpan, resolve_cut_segment, resolve_seen_z};
use lamb_core::geometry::polyline::resolved_segments;
use lamb_core::geometry::tolerance::TOLERANCE;
use lamb_core::model::{AnyView, Id, LineRole, Placement, VerticalIntent};
use serde_json::{Value, json};

use crate::commands::kind_word;
use crate::entitycmds::ROLE_WORDS;
use crate::overlays::view_id;
use crate::project::DrawingProject;
use crate::schema::{read_document, read_entity_data};

/// One row: a label, its shown value, hover text, and (label, command, params) choices — none for
/// a read-only row.
#[derive(Clone, Debug, PartialEq)]
pub struct LambRow {
    pub label: String,
    pub value: String,
    pub tip: Option<String>,
    pub choices: Vec<(String, String, Value)>,
    /// A committed text (or number) field running a command; `None` for choice and read-only rows.
    pub edit: Option<LambEdit>,
}

/// An editable row: the typed text goes into `params[key]` — a string, or a number when `number`
/// — and `command` runs with those params.
#[derive(Clone, Debug, PartialEq)]
pub struct LambEdit {
    pub command: String,
    pub params: Value,
    pub key: String,
    pub number: bool,
}

/// The group: title `LAMB`, the segment count the stepper walks (0 when there's no height summary),
/// and its rows.
#[derive(Clone, Debug, PartialEq)]
pub struct LambGroup {
    pub title: String,
    pub segments: usize,
    pub rows: Vec<LambRow>,
}

/// TS `PROVENANCE_TOOLTIP`.
fn provenance_tip(p: ProvenanceKind) -> Option<String> {
    match p {
        ProvenanceKind::Authored => Some("Drawn and unambiguous.".into()),
        ProvenanceKind::Defaulted => Some("Nothing drawn; project defaults used.".into()),
        ProvenanceKind::Ambiguous => Some("Known at points, unknown between them — never interpolated.".into()),
        ProvenanceKind::Conflicted => Some("Two views contradict at the same point — never averaged.".into()),
        ProvenanceKind::Proposed | ProvenanceKind::NotThreeD => None,
    }
}

/// The serde word of a provenance (`authored`, `not_3d`, ...), as TS shows it on its badge.
fn provenance_word(p: ProvenanceKind) -> String {
    serde_json::to_value(p).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

fn role_word(r: LineRole) -> &'static str {
    ROLE_WORDS.iter().find(|(_, x)| *x == r).map_or("Bylayer", |(w, _)| w)
}

/// The Role row: the shared stored role (`*VARIES*` when they differ; a stored Bylayer shows what it
/// resolves to), with SETROLE choices for every role.
fn role_row(dp: &DrawingProject<'_>, sel: &[Handle], ents: &[EntityRef<'_>]) -> LambRow {
    let stored: Vec<LineRole> = ents.iter().map(|e| e.role).collect();
    let resolved: Vec<LineRole> = ents.iter().map(|e| dp.role_of(e)).collect();
    let all_same = |v: &[LineRole]| v.iter().all(|r| *r == v[0]);
    let value = if !all_same(&stored) {
        "*VARIES*".to_owned()
    } else if stored[0] == LineRole::Bylayer {
        if all_same(&resolved) { format!("Bylayer → {}", role_word(resolved[0])) } else { "Bylayer → *VARIES*".to_owned() }
    } else {
        role_word(stored[0]).to_owned()
    };
    let hex: Vec<String> = sel.iter().map(|h| h.hex()).collect();
    let choices = ROLE_WORDS.iter().map(|(w, r)| ((*w).to_owned(), "setrole".to_owned(), json!({ "handles": hex, "role": r }))).collect();
    LambRow { label: "Role".into(), value, tip: None, choices, edit: None }
}

/// The View row of one selected layout viewport (not a layout's overall one): its bound view's name
/// or `(none)`, with VPVIEW choices `(none)` then every view in document order. `None` for anything
/// else.
fn viewport_group(d: &Drawing, sel: &[Handle]) -> Option<LambGroup> {
    let [h] = sel else { return None };
    let e = d.entity(*h)?;
    let EntityKind::Viewport(vp) = &e.kind else { return None };
    if vp.id <= 1 || !matches!(d.space_of(*h), Some(Space::Paper(_))) {
        return None;
    }
    let doc = read_document(d).ok()?;
    let bound = read_entity_data(&e.common).ok().and_then(|x| x.view_id);
    let value = bound.and_then(|id| doc.views.iter().find(|v| v.id() == id)).map_or("(none)".to_owned(), |v| v.name().to_owned());
    let hex = h.hex();
    let choices = std::iter::once(("(none)".to_owned(), "vpview".to_owned(), json!({ "handle": hex, "viewId": null })))
        .chain(doc.views.iter().map(|v| (v.name().to_owned(), "vpview".to_owned(), json!({ "handle": hex, "viewId": v.id().0 }))))
        .collect();
    Some(LambGroup { title: "LAMB".into(), segments: 0, rows: vec![LambRow { label: "View".into(), value, tip: None, choices, edit: None }] })
}

/// The placed vertical views linked to (entity, segment), with the plan-side crossing param of each
/// link; deduplication and ordering are left to [`go_to_choices`].
fn linked_vertical_views(dp: &DrawingProject<'_>, entity: Id, segment: usize) -> Vec<(Id, f64)> {
    dp.links_for_segment(entity, segment)
        .iter()
        .filter_map(|l| {
            let v = dp.view(dp.entity_view_id(l.other.entity_id)?)?;
            (v.kind == ViewKind::Vertical && v.placement.is_some()).then_some((v.id, l.mine.segment_param))
        })
        .collect()
}

/// `Go to <name>` / `editview {viewId}` for every view in `ids`, deduplicated, in document view order.
fn go_to_choices(dp: &DrawingProject<'_>, ids: &[Id]) -> Vec<(String, String, Value)> {
    dp.views().filter(|v| ids.contains(&v.id)).map(|v| (format!("Go to {}", v.name), "editview".to_owned(), json!({ "viewId": v.id.0 }))).collect()
}

/// The views a Height source span comes from: a conflicted span's candidates, else the links
/// crossing inside `[from_t, to_t]` (± the grouping tolerance); none when ambiguous or defaulted.
fn span_views(span: &ResolvedSpan, links: &[(Id, f64)], tol_t: f64) -> Vec<Id> {
    match span.provenance {
        ProvenanceKind::Ambiguous | ProvenanceKind::Defaulted => vec![],
        ProvenanceKind::Conflicted => span.conflict_candidates.iter().flatten().map(|c| c.view_id).collect(),
        _ => links.iter().filter(|(_, t)| *t >= span.from_t - tol_t && *t <= span.to_t + tol_t).map(|(id, _)| *id).collect(),
    }
}

/// The LAMB group for `sel` at `segment` (clamped to the polyline's last segment): the View row for
/// one layout viewport, else `None` unless every handle is a LAMB entity.
pub fn property_group(d: &Drawing, sel: &[Handle], segment: usize) -> Option<LambGroup> {
    if sel.is_empty() {
        return None;
    }
    if let Some(g) = viewport_group(d, sel) {
        return Some(g);
    }
    let dp = DrawingProject::new(d).ok()?;
    let ents: Vec<EntityRef<'_>> = sel.iter().map(|h| dp.entity(Id(h.0))).collect::<Option<_>>()?;
    let mut rows = vec![role_row(&dp, sel, &ents)];
    if let [e] = ents.as_slice()
        && let Some(o) = e.ai_origin
    {
        let value = if o.accepted { "Accepted" } else { "Proposed — ACCEPT to keep" };
        rows.push(LambRow { label: "AI line".into(), value: value.into(), tip: None, choices: vec![], edit: None });
    }
    let mut segments = 0;
    if let [e] = ents.as_slice()
        && let Some(poly) = &e.polyline
        && let Some(view) = dp.view(e.view_id)
        && view.kind == ViewKind::Plan
        && view.placement.is_some()
        && !poly.segments.is_empty()
    {
        let n = poly.segments.len();
        let seg = segment.min(n - 1);
        match dp.role_of(e) {
            LineRole::Cut => {
                segments = n;
                let links = linked_vertical_views(&dp, e.id, seg);
                let tol_t = resolved_segments(poly).get(seg).map_or(0.0, |r| {
                    let len = (r.to.x - r.from.x).hypot(r.to.y - r.from.y);
                    TOLERANCE.grouping_mm / if len > 0.0 { len } else { 1.0 }
                });
                for s in resolve_cut_segment(&dp, e, seg).ok()? {
                    let choices = go_to_choices(&dp, &span_views(&s, &links, tol_t));
                    let mut value = provenance_word(s.provenance);
                    if let Some(x) = s.extent {
                        value += &format!(" {:.0}…{:.0} mm", x.bottom, x.top);
                    }
                    if let Some(t) = s.tilt_deg {
                        value += &format!(", tilt {t:.1}°");
                    }
                    rows.push(LambRow { label: "Height source".into(), value, tip: provenance_tip(s.provenance), choices, edit: None });
                }
            }
            LineRole::Seen => {
                segments = n;
                let z = resolve_seen_z(&dp, e, seg).ok()?;
                let choices = if z.provenance == ProvenanceKind::Defaulted {
                    vec![]
                } else {
                    let ids: Vec<Id> = linked_vertical_views(&dp, e.id, seg).into_iter().map(|(id, _)| id).collect();
                    go_to_choices(&dp, &ids)
                };
                rows.push(LambRow {
                    label: "Z source".into(),
                    value: format!("{} z = {:.0} mm", provenance_word(z.provenance), z.z),
                    tip: provenance_tip(z.provenance),
                    choices,
                    edit: None,
                });
            }
            _ => {}
        }
    }
    Some(LambGroup { title: "LAMB".into(), segments, rows })
}

/// The picked view's form (TS `ViewPropertiesForm`) for the idle-pick key `LAMB_VIEW_<id>`: title
/// the view's kind, then Name, and Cut elevation (placed plan), Intent (vertical), Flip (placed
/// vertical), each running VIEWPROPS. `None` for a key that is not a view in the document.
pub fn picked_view_group(d: &Drawing, key: &str) -> Option<LambGroup> {
    let id = view_id(&Space::Block(key.to_owned()))?;
    let doc = read_document(d).ok()?;
    let view = doc.views.iter().find(|v| v.id() == id)?;
    let params = |mode: &str| json!({ "mode": mode, "viewId": id.0 });
    let edit = |mode: &str, key: &str, number: bool| Some(LambEdit { command: "viewprops".into(), params: params(mode), key: key.into(), number });
    let mut rows =
        vec![LambRow { label: "Name".into(), value: view.name().to_owned(), tip: None, choices: vec![], edit: edit("rename", "name", false) }];
    match view {
        AnyView::Plan(_) => {
            if let Some(Placement::Plan { cut_elevation, .. }) = view.placement() {
                rows.push(LambRow {
                    label: "Cut elevation".into(),
                    value: format!("{cut_elevation:.0}"),
                    tip: None,
                    choices: vec![],
                    edit: edit("cutElevation", "cutElevation", true),
                });
            }
        }
        AnyView::Vertical(v) => {
            let word = |i: VerticalIntent| if i == VerticalIntent::Elevation { "Elevation" } else { "Section" };
            let choices = [("Section", "section"), ("Elevation", "elevation")]
                .iter()
                .map(|(w, i)| ((*w).to_owned(), "viewprops".to_owned(), json!({ "mode": "intent", "viewId": id.0, "intent": i })))
                .collect();
            rows.push(LambRow { label: "Intent".into(), value: word(v.intent).into(), tip: None, choices, edit: None });
            if let Some(Placement::Vertical { flipped, .. }) = view.placement() {
                let cur = flipped.unwrap_or(false);
                rows.push(LambRow {
                    label: "Flip".into(),
                    value: if cur { "Flipped" } else { "Not flipped" }.into(),
                    tip: None,
                    choices: vec![("Flip".into(), "viewprops".into(), json!({ "mode": "flip", "viewId": id.0, "flipped": !cur }))],
                    edit: None,
                });
            }
        }
        AnyView::Detail(_) => {}
    }
    Some(LambGroup { title: kind_word(view).into(), segments: 0, rows })
}

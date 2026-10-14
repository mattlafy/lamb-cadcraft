//! The LAMB group of the Properties palette (TS `PropertiesPanel.tsx`'s Role select, `CutSummary`,
//! `SeenSummary`): a Role row whose choices run SETROLE, and for one plan polyline in a placed plan
//! view whose resolved role is cut or seen, where its height comes from at the stepped segment.
//! The desktop app maps [`LambGroup`] onto the UI's generic property-group slot.

use cadcraft_doc::Space;
use cadcraft_doc::{Drawing, Handle};
use lamb_core::derivation::read::{EntityRef, ProjectRead, ViewKind};
use lamb_core::derivation::resolve::{ProvenanceKind, resolve_cut_segment, resolve_seen_z};
use lamb_core::model::{AnyView, Id, LineRole, Placement, VerticalIntent};
use serde_json::{Value, json};

use crate::commands::kind_word;
use crate::entitycmds::ROLE_WORDS;
use crate::overlays::view_id;
use crate::project::DrawingProject;
use crate::schema::read_document;

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

/// The LAMB group for `sel` at `segment` (clamped to the polyline's last segment); `None` unless
/// every handle is a LAMB entity.
pub fn property_group(d: &Drawing, sel: &[Handle], segment: usize) -> Option<LambGroup> {
    if sel.is_empty() {
        return None;
    }
    let dp = DrawingProject::new(d).ok()?;
    let ents: Vec<EntityRef<'_>> = sel.iter().map(|h| dp.entity(Id(h.0))).collect::<Option<_>>()?;
    let mut rows = vec![role_row(&dp, sel, &ents)];
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
                for s in resolve_cut_segment(&dp, e, seg).ok()? {
                    let mut value = provenance_word(s.provenance);
                    if let Some(x) = s.extent {
                        value += &format!(" {:.0}…{:.0} mm", x.bottom, x.top);
                    }
                    if let Some(t) = s.tilt_deg {
                        value += &format!(", tilt {t:.1}°");
                    }
                    rows.push(LambRow { label: "Height source".into(), value, tip: provenance_tip(s.provenance), choices: vec![], edit: None });
                }
            }
            LineRole::Seen => {
                segments = n;
                let z = resolve_seen_z(&dp, e, seg).ok()?;
                rows.push(LambRow {
                    label: "Z source".into(),
                    value: format!("{} z = {:.0} mm", provenance_word(z.provenance), z.z),
                    tip: provenance_tip(z.provenance),
                    choices: vec![],
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

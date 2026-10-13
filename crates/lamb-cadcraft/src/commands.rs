//! LAMB's view commands as engine `CommandSpec`s, registered once per process with
//! `cadcraft_engine::register_command_provider(lamb_cadcraft::commands)` (FORK.md patch 4).
//!
//! - `newplan {name}` (`npl`): a floating plan view.
//! - `newsectionline` / `newelevationline {name, hostViewId, cutFrom:{x,y}, cutTo:{x,y}}` (`nsl`,
//!   `nel`): a vertical view placed on a straight cut line in the host view.
//! - `editview {viewId?}`: edit inside the view's block; no `viewId` returns to Model.
//! - `newsheet {name, viewIds?}` (`nsh`): a paper layout with one viewport per view (default every
//!   view), each freezing every other view's layer; returns `{layout, viewports: [hex handles]}`.
//!
//! Creating commands return `{"viewId": <id>}`; each is one undo step (block, layer, insert and
//! document entry together). Refusals use the TS command engine's wording.

use cadcraft_doc::Space;
use cadcraft_engine::{CommandSpec, EngineError, Result, Session};
use lamb_core::geometry::polyline::line2;
use lamb_core::geometry::{IDENTITY_TRANSFORM, Point2};
use lamb_core::model::{AnyView, Id, Placement, PlanView, VerticalIntent, VerticalView};
use serde_json::{Value, json};

use crate::schema::{read_document, view_block_name};
use crate::sheets::create_sheet;
use crate::views::create_view;

fn other(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

fn bad(cmd: &str, msg: &str) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn str_param<'a>(cmd: &str, p: &'a Value, key: &str) -> Result<&'a str> {
    p.get(key).and_then(Value::as_str).ok_or_else(|| bad(cmd, &format!("`{key}` must be a string")))
}

/// A view id: a JSON number, or a decimal string.
fn id_value(v: &Value) -> Option<Id> {
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).map(Id)
}

fn point_param(cmd: &str, p: &Value, key: &str) -> Result<Point2> {
    let q = p.get(key).ok_or_else(|| bad(cmd, &format!("`{key}` is required")))?;
    let c = |k: &str| q.get(k).and_then(Value::as_f64).filter(|v| v.is_finite());
    match (c("x"), c("y")) {
        (Some(x), Some(y)) => Ok(Point2 { x, y }),
        _ => Err(bad(cmd, &format!("`{key}` must be {{x, y}} with finite numbers"))),
    }
}

fn create(s: &mut Session, view: AnyView) -> Result<Value> {
    let id = create_view(s.doc_mut()?, view).map_err(other)?;
    Ok(json!({ "viewId": id.0 }))
}

fn newplan(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_param("newplan", p, "name")?.to_owned();
    create(s, AnyView::Plan(PlanView { id: Id(0), name, entities: Vec::new(), placement: None }))
}

fn new_vertical(s: &mut Session, p: &Value, cmd: &str, intent: VerticalIntent) -> Result<Value> {
    let name = str_param(cmd, p, "name")?.to_owned();
    let host = p.get("hostViewId").and_then(id_value).ok_or_else(|| bad(cmd, "`hostViewId` is required"))?;
    let from = point_param(cmd, p, "cutFrom")?;
    let to = point_param(cmd, p, "cutTo")?;
    let doc = read_document(s.doc()?).map_err(other)?;
    let host_view = doc.views.iter().find(|v| v.id() == host).ok_or_else(|| other("No such host view"))?;
    if let AnyView::Vertical(v) = host_view
        && v.placement.is_none()
    {
        return Err(other("Host vertical view must be placed"));
    }
    let placement = Placement::Vertical { cut_line: line2(from, to), host_view_id: host, transform: IDENTITY_TRANSFORM, flipped: None };
    create(s, AnyView::Vertical(VerticalView { id: Id(0), name, intent, entities: Vec::new(), placement: Some(placement) }))
}

fn newsectionline(s: &mut Session, p: &Value) -> Result<Value> {
    new_vertical(s, p, "newsectionline", VerticalIntent::Section)
}

fn newelevationline(s: &mut Session, p: &Value) -> Result<Value> {
    new_vertical(s, p, "newelevationline", VerticalIntent::Elevation)
}

fn editview(s: &mut Session, p: &Value) -> Result<Value> {
    let space = match p.get("viewId").filter(|v| !v.is_null()) {
        None => Space::Model,
        Some(v) => {
            let id = id_value(v).ok_or_else(|| other("No such view"))?;
            let doc = read_document(s.doc()?).map_err(other)?;
            if !doc.views.iter().any(|v| v.id() == id) {
                return Err(other("No such view"));
            }
            Space::Block(view_block_name(id))
        }
    };
    let st = s.state_mut()?;
    st.space = space;
    st.mspace = None;
    s.touch();
    Ok(Value::Null)
}

fn newsheet(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_param("newsheet", p, "name")?.to_owned();
    let ids = match p.get("viewIds").filter(|v| !v.is_null()) {
        None => Vec::new(),
        Some(v) => {
            let arr = v.as_array().ok_or_else(|| bad("newsheet", "`viewIds` must be an array"))?;
            arr.iter().map(|x| id_value(x).ok_or_else(|| other("No such view"))).collect::<Result<Vec<Id>>>()?
        }
    };
    let handles = create_sheet(s.doc_mut()?, &name, &ids).map_err(other)?;
    let hex: Vec<String> = handles.iter().map(|h| format!("{:X}", h.0)).collect();
    Ok(json!({ "layout": name, "viewports": hex }))
}

/// The LAMB commands, for `register_command_provider`.
pub fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("newplan", "New Plan", newplan).alias(&["npl"]).params("{name}"),
        CommandSpec::new("newsectionline", "New Section Line", newsectionline)
            .alias(&["nsl"])
            .params("{name, hostViewId, cutFrom:{x,y}, cutTo:{x,y}}"),
        CommandSpec::new("newelevationline", "New Elevation Line", newelevationline)
            .alias(&["nel"])
            .params("{name, hostViewId, cutFrom:{x,y}, cutTo:{x,y}}"),
        CommandSpec::new("newsheet", "New Sheet", newsheet).alias(&["nsh"]).params("{name, viewIds?}"),
        CommandSpec::new("editview", "Edit View", editview).params("{viewId?}").noundo(),
    ]
}

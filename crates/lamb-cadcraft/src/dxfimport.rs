//! DXFIMPORT (the TS app's `referenceImport.ts`): a DXF or DWG file becomes its own **unplaced plan
//! view** holding the file's line work, rather than geometry landed in the edited view (CADCraft's
//! IMPORT does that).
//!
//! - `dxfimport {path | data: base64, fileName?, name?, unitScale?}` (View ▸ LAMB ▸ Import DXF as
//!   View): every model-space entity of the file that [`to_polyline`] accepts is copied into the new
//!   view's block, scaled by `unitScale` about the origin, on the current layer with role `Seen`;
//!   everything else is counted as skipped by its upper-case type name. The view and its contents are one undo step.
//!   Returns `{viewId, imported, skipped: {TYPE: n}}`. `name` defaults to the file stem (`Import` for
//!   `data` without `fileName`). Nothing to copy refuses before anything is created.
//! - Interactive: `Enter DXF file path`, `Specify unit scale <1>`, `Enter view name <stem>`, then
//!   edit inside the new view. The file is a typed path; a file picker is a later GUI slice.

use std::collections::BTreeMap;
use std::path::Path;

use cadcraft_doc::Space;
use cadcraft_engine::cmd::file::{base64_decode, io};
use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use cadcraft_geom::Mat3;
use lamb_core::model::{AnyView, Id, LineRole, PlanView};
use serde_json::{Value, json};

use crate::commands::{bad, create, enter_space, name_input, other};
use crate::convert::to_polyline;
use crate::schema::{EntityData, set_entity_data, view_block_name};

const CMD: &str = "dxfimport";

/// The view name a file gives by default: its stem, or `Import`.
fn stem_of(name: &str) -> String {
    Path::new(name).file_stem().map(|s| s.to_string_lossy().trim().to_owned()).filter(|s| !s.is_empty()).unwrap_or_else(|| "Import".to_owned())
}

/// DXFIMPORT's JSON form. Every refusal comes before `doc_mut`, so a refusal records nothing.
pub(crate) fn dxfimport(s: &mut Session, p: &Value) -> Result<Value> {
    let hooks = io().ok_or_else(|| bad(CMD, "file formats are not available in this build"))?;
    let k = match p.get("unitScale").filter(|v| !v.is_null()) {
        None => 1.0,
        Some(v) => v.as_f64().filter(|k| k.is_finite() && *k > 0.0).ok_or_else(|| bad(CMD, "unitScale must be positive"))?,
    };
    let str_of = |key: &str| p.get(key).and_then(Value::as_str);
    let (bytes, file, default_name) = if let Some(path) = str_of("path") {
        let bytes = std::fs::read(path).map_err(|e| other(format!("Cannot read {path}: {e}")))?;
        (bytes, path.to_owned(), stem_of(path))
    } else {
        let data = str_of("data").ok_or_else(|| bad(CMD, "`path` or `data` (base64) is required"))?;
        let bytes = base64_decode(data).ok_or_else(|| bad(CMD, "invalid base64"))?;
        let file_name = str_of("fileName");
        (bytes, file_name.unwrap_or("import.dxf").to_owned(), file_name.map_or_else(|| "Import".to_owned(), stem_of))
    };
    let name = str_of("name").map(str::trim).filter(|n| !n.is_empty()).map_or(default_name, str::to_owned);
    let src = (hooks.read)(&bytes, &file).map_err(|e| bad(CMD, &e))?;

    let mut kinds = Vec::new();
    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
    for e in src.model.iter() {
        if to_polyline(e).is_ok() {
            let mut kind = e.kind.clone();
            if k != 1.0 {
                kind.transform(&Mat3::scale(k, k));
            }
            kinds.push(kind);
        } else {
            *skipped.entry(e.kind.type_name().to_uppercase()).or_default() += 1;
        }
    }
    if kinds.is_empty() {
        return Err(other("No supported entities found in DXF"));
    }
    let mut common = s.current_common();
    set_entity_data(&mut common, &EntityData { role: LineRole::Seen, ..EntityData::default() }).map_err(other)?;

    let r = create(s, AnyView::Plan(PlanView { id: Id(0), name: name.clone(), entities: Vec::new(), placement: None }))?;
    let id = r.get("viewId").and_then(Value::as_u64).map(Id).ok_or_else(|| other("no view id"))?;
    let space = Space::Block(view_block_name(id));
    let d = s.doc_mut()?;
    let imported = kinds.len();
    for kind in kinds {
        d.add(&space, common.clone(), kind)?;
    }
    let n_skipped: usize = skipped.values().sum();
    s.echo(format!("Imported {imported} entities into \"{name}\" ({n_skipped} skipped)"));
    Ok(json!({ "viewId": id.0, "imported": imported, "skipped": skipped }))
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Path,
    Scale,
    Name,
}

/// DXFIMPORT interactively: path, unit scale, view name; then edit inside the new view.
struct DxfImportM {
    stage: Stage,
    path: String,
    scale: f64,
}

impl Interactive for DxfImportM {
    fn name(&self) -> &'static str {
        CMD
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.stage {
            Stage::Path => Prompt::new("Enter DXF file path", Accept::TEXT),
            Stage::Scale => Prompt::new("Specify unit scale", Accept::TEXT).default("1".to_owned()),
            Stage::Name => Prompt::new("Enter view name", Accept::TEXT).default(stem_of(&self.path)),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        match self.stage {
            Stage::Path => {
                let (Input::Text(t) | Input::Keyword(t)) = i else { return Ok(Step::Continue) };
                let t = t.trim().trim_matches('"').trim();
                if t.is_empty() {
                    return Ok(Step::Continue);
                }
                self.path = t.to_owned();
                self.stage = Stage::Scale;
                Ok(Step::Continue)
            }
            Stage::Scale => {
                let scale = match i {
                    Input::Enter => Some(1.0),
                    Input::Text(t) | Input::Keyword(t) => {
                        let t = t.trim();
                        if t.is_empty() { Some(1.0) } else { t.parse::<f64>().ok().filter(|k| k.is_finite() && *k > 0.0) }
                    }
                    _ => return Ok(Step::Continue),
                };
                match scale {
                    Some(k) => {
                        self.scale = k;
                        self.stage = Stage::Name;
                    }
                    None => s.echo("Invalid unit scale"),
                }
                Ok(Step::Continue)
            }
            Stage::Name => {
                let Some(name) = name_input(i, &stem_of(&self.path)) else { return Ok(Step::Continue) };
                match dxfimport(s, &json!({ "path": self.path, "unitScale": self.scale, "name": name })) {
                    Ok(r) => {
                        if let Some(id) = r.get("viewId").and_then(Value::as_u64) {
                            enter_space(s, Space::Block(view_block_name(Id(id))))?;
                        }
                    }
                    Err(e) => s.echo(e.to_string()),
                }
                Ok(Step::Done)
            }
        }
    }
}

pub(crate) fn dxfimport_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(DxfImportM { stage: Stage::Path, path: String::new(), scale: 1.0 }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{read_document, read_entity_data};
    use cadcraft_doc::EntityKind;
    use cadcraft_engine::cmd::file::{IoHooks, set_io};
    use std::sync::atomic::{AtomicUsize, Ordering};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    fn hooks() {
        set_io(IoHooks {
            read: |b, n| cadcraft_io::read(b, n).map_err(|e| e.to_string()),
            write: |d, n| cadcraft_io::write(d, n).map_err(|e| e.to_string()),
            plot: None,
        });
    }

    /// A DXF file built by `draw` in a fresh session, written to a unique temp path.
    fn dxf_file(stem: &str, draw: impl Fn(&mut Session) -> Result<()>) -> T<std::path::PathBuf> {
        static N: AtomicUsize = AtomicUsize::new(0);
        let mut src = Session::new();
        src.new_drawing(true);
        draw(&mut src)?;
        let dir = std::env::temp_dir().join(format!("lamb-dxfimport-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{stem}.dxf"));
        std::fs::write(&path, cadcraft_io::write_dxf(src.doc()?))?;
        Ok(path)
    }

    fn walls(s: &mut Session) -> Result<()> {
        s.execute("line", &json!({ "points": [[0, 0], [1000, 0]] }))?;
        s.execute("circle", &json!({ "center": [0, 0], "radius": 500 }))?;
        s.execute("text", &json!({ "at": [0, 0], "text": "abc" }))?;
        Ok(())
    }

    /// A LAMB session editing nothing, with `NOTES` as the current layer.
    fn session() -> T<Session> {
        crate::commands::install_app()?;
        hooks();
        let mut s = Session::new();
        s.new_drawing(true);
        s.execute("layer.new", &json!({ "name": "NOTES", "current": true }))?;
        Ok(s)
    }

    fn check_view(s: &Session, id: u64, name: &str) -> R {
        let doc = read_document(s.doc()?)?;
        let v = doc.views.iter().find(|v| v.id() == Id(id)).ok_or("no view")?;
        let AnyView::Plan(plan) = v else { return Err("not a plan".into()) };
        assert_eq!(plan.name, name);
        assert!(plan.placement.is_none());
        let block = s.doc()?.block(&view_block_name(Id(id))).ok_or("no block")?;
        assert_eq!(block.entities.len(), 2);
        for e in block.entities.iter() {
            assert_eq!(e.common.layer, "NOTES");
            assert_eq!(read_entity_data(&e.common)?.role, LineRole::Seen);
            match &e.kind {
                EntityKind::Line(l) => {
                    assert!((l.a.x - 0.0).abs() < 1e-9 && (l.b.x - 10000.0).abs() < 1e-9 && l.b.y.abs() < 1e-9, "{l:?}");
                }
                EntityKind::Circle(c) => assert!((c.radius - 5000.0).abs() < 1e-9, "{c:?}"),
                k => return Err(format!("unexpected {}", k.type_name()).into()),
            }
        }
        Ok(())
    }

    #[test]
    fn line_work_lands_in_a_new_unplaced_plan_scaled_in_one_undo_step() -> R {
        let mut s = session()?;
        let path = dxf_file("walls", walls)?;
        let space = s.space();
        let undo = s.state()?.undo.len();
        let r = s.execute(CMD, &json!({ "path": path.to_string_lossy(), "unitScale": 10 }))?;
        assert_eq!(r["imported"], 2);
        assert_eq!(r["skipped"], json!({ "TEXT": 1 }));
        let id = r["viewId"].as_u64().ok_or("no id")?;
        check_view(&s, id, "walls")?;
        assert_eq!(s.space(), space);
        assert_eq!(s.state()?.undo.len(), undo + 1);
        s.execute("undo", &json!({}))?;
        assert!(read_document(s.doc()?)?.views.is_empty());
        assert!(s.doc()?.block(&view_block_name(Id(id))).is_none_or(|b| b.entities.is_empty()));
        Ok(())
    }

    #[test]
    fn nothing_supported_a_zero_scale_or_a_missing_file_refuses_and_records_nothing() -> R {
        let mut s = session()?;
        let text_only = dxf_file("notes", |s| s.execute("text", &json!({ "at": [0, 0], "text": "abc" })).map(|_| ()))?;
        let walls = dxf_file("walls", walls)?;
        let undo = s.state()?.undo.len();
        let e = s.execute(CMD, &json!({ "path": text_only.to_string_lossy() })).err().ok_or("no refusal")?;
        assert!(e.to_string().contains("No supported entities found in DXF"), "{e}");
        let e = s.execute(CMD, &json!({ "path": walls.to_string_lossy(), "unitScale": 0 })).err().ok_or("no refusal")?;
        assert!(e.to_string().contains("unitScale must be positive"), "{e}");
        assert!(s.execute(CMD, &json!({ "path": walls.with_file_name("missing.dxf").to_string_lossy() })).is_err());
        assert!(read_document(s.doc()?)?.views.is_empty());
        assert_eq!(s.state()?.undo.len(), undo);
        Ok(())
    }

    #[test]
    fn base64_data_takes_its_name_from_file_name() -> R {
        let mut s = session()?;
        let path = dxf_file("walls", walls)?;
        let data = cadcraft_engine::cmd::file::base64_encode(&std::fs::read(&path)?);
        let r = s.execute(CMD, &json!({ "data": data, "fileName": "site.dxf", "unitScale": 10 }))?;
        check_view(&s, r["viewId"].as_u64().ok_or("no id")?, "site")?;
        let r = s.execute(CMD, &json!({ "data": data, "unitScale": 10 }))?;
        check_view(&s, r["viewId"].as_u64().ok_or("no id")?, "Import")?;
        Ok(())
    }

    #[test]
    fn the_interactive_form_takes_defaults_and_edits_the_new_view() -> R {
        let mut s = session()?;
        let path = dxf_file("walls", walls)?;
        s.start(CMD)?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter DXF file path:"));
        s.input(Input::Text(path.to_string_lossy().into_owned()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Specify unit scale <1>:"));
        s.input(Input::Text("nope".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Specify unit scale <1>:"));
        s.input(Input::Text("10".into()))?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter view name <walls>:"));
        s.input(Input::Enter)?;
        assert!(s.current_prompt().is_none());
        let doc = read_document(s.doc()?)?;
        assert_eq!(doc.views.len(), 1);
        let id = doc.views[0].id();
        check_view(&s, id.0, "walls")?;
        assert_eq!(s.space(), Space::Block(view_block_name(id)));
        Ok(())
    }

    #[test]
    fn interactive_enter_at_every_prompt_imports_unscaled() -> R {
        let mut s = session()?;
        let path = dxf_file("walls", walls)?;
        s.start(CMD)?;
        s.input(Input::Text(path.to_string_lossy().into_owned()))?;
        s.input(Input::Enter)?;
        s.input(Input::Enter)?;
        let doc = read_document(s.doc()?)?;
        assert_eq!(doc.views.len(), 1);
        let block = s.doc()?.block(&view_block_name(doc.views[0].id())).ok_or("no block")?;
        assert!(block.entities.iter().any(|e| matches!(&e.kind, EntityKind::Circle(c) if (c.radius - 500.0).abs() < 1e-9)));
        Ok(())
    }
}

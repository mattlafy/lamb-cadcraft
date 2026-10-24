//! LAMBOPEN / LAMBSAVE are interchange with the old TypeScript app's `.lamb` folder, not LAMB's save
//! format: LAMB's data is stored inside the DWG/DXF and ordinary Save and Open carry it. Both are
//! typed commands, off the menu.
//!
//! LAMBOPEN: a TS-app project (a `.lamb` directory, or its single-file `{path: content}` JSON form)
//! opens as a **new document tab**, as OPEN does for a DWG.
//!
//! - `lambopen {path}` (typed only, Import TS-app Project): reads the project, imports it with
//!   [`crate::import::project_to_drawing`] and opens it titled with the project's name (else the
//!   path's stem). The new document has no path, so QSAVE asks where to save it rather than
//!   writing over the `.lamb`. Not an undo step. Echoes `Opened "<name>": <v> views, <e> entities`
//!   and, when non-empty, what the import skipped, the links it dropped and unknown layers.
//!   Returns `{views, entities, skipped: {kind: n}, droppedLinks, unknownLayers, document}`.
//! - Interactive: `Enter .lamb project path`. The path is typed; a picker is a later GUI slice.
//!
//! LAMBSAVE: the open drawing written as a TS-app `.lamb` **directory**.
//!
//! - `lambsave {path}` (typed only, Export TS-app Project): exports the drawing with
//!   [`crate::export::drawing_to_project`] and writes it with `lamb_core::persistence::write_lamb_dir`.
//!   Refuses a path that is an existing file, or an existing non-empty directory with no
//!   `manifest.json`. Leaves the document's path, title and undo alone (QSAVE still writes DWG).
//!   Echoes `Saved "<name>" to <path>: <v> views, <e> entities` and, when non-empty, what the export
//!   skipped, the fork-only tags it dropped and unknown patterns. Returns
//!   `{views, entities, skipped: {kind: n}, droppedTags, unknownPatterns, path}`.
//! - Interactive: `Enter .lamb project path`, typed, quotes trimmed.

use std::collections::BTreeMap;
use std::path::Path;

use cadcraft_engine::{Accept, Input, Interactive, Prompt, Result, Session, Step};
use lamb_core::model::Project;
use serde_json::{Value, json};

use crate::commands::{bad, other};

const CMD: &str = "lambopen";
const SAVE: &str = "lambsave";

/// The project at `path`: a `.lamb` directory, or a file holding the `{relative path: content}` JSON.
fn read_project(path: &str) -> std::result::Result<Project, String> {
    let p = Path::new(path);
    if p.is_dir() {
        return lamb_core::persistence::read_lamb_dir(p).map_err(|e| format!("{e:?}"));
    }
    let text = std::fs::read_to_string(p).map_err(|e| e.to_string())?;
    let files: BTreeMap<String, String> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    lamb_core::persistence::files_to_project(&files).map_err(|e| format!("{e:?}"))
}

/// LAMBOPEN's JSON form. Every refusal comes before a document is added.
pub(crate) fn lambopen(s: &mut Session, p: &Value) -> Result<Value> {
    let path = p.get("path").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad(CMD, "`path` is required"))?;
    let refuse = |e: String| other(format!("Cannot open {path}: {e}"));
    let project = read_project(path).map_err(refuse)?;
    let (d, _, report) = crate::import::project_to_drawing(&project).map_err(|e| refuse(format!("{e:?}")))?;

    let title = Some(project.name.trim()).filter(|n| !n.is_empty()).map(str::to_owned).unwrap_or_else(|| {
        Path::new(path).file_stem().map(|s| s.to_string_lossy().trim_end_matches(".lamb").to_owned()).unwrap_or_else(|| path.to_owned())
    });
    let views = project.views.len();
    let total: usize = project.views.iter().map(|v| v.entities().len()).sum();
    let entities = total.saturating_sub(report.skipped.len());
    let mut skipped: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, kind) in &report.skipped {
        *skipped.entry(kind).or_default() += 1;
    }

    let document = s.open_drawing(d, &title, None);
    s.echo(format!("Opened \"{title}\": {views} views, {entities} entities"));
    if !skipped.is_empty() {
        let kinds: Vec<String> = skipped.iter().map(|(k, n)| format!("{k} ×{n}")).collect();
        s.echo(format!("Skipped {} ({})", report.skipped.len(), kinds.join(", ")));
    }
    if !report.dropped_links.is_empty() {
        s.echo(format!("Dropped {} links", report.dropped_links.len()));
    }
    if !report.unknown_layers.is_empty() {
        s.echo(format!("Unknown layers: {}", report.unknown_layers.join(", ")));
    }
    if !report.unassociated_hatches.is_empty() {
        s.echo(format!("Hatches not associative: {}", report.unassociated_hatches.len()));
    }
    if !report.unknown_patterns.is_empty() {
        s.echo(format!("Unknown patterns: {}", report.unknown_patterns.join(", ")));
    }
    Ok(json!({
        "views": views,
        "entities": entities,
        "skipped": skipped,
        "droppedLinks": report.dropped_links.len(),
        "unknownLayers": report.unknown_layers,
        "document": document,
    }))
}

/// LAMBOPEN interactively: one typed path, then open (or echo the refusal).
struct LambOpenM;

impl Interactive for LambOpenM {
    fn name(&self) -> &'static str {
        CMD
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter .lamb project path", Accept::TEXT)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let (Input::Text(t) | Input::Keyword(t)) = i else { return Ok(Step::Continue) };
        let t = t.trim().trim_matches('"').trim();
        if t.is_empty() {
            return Ok(Step::Continue);
        }
        if let Err(e) = lambopen(s, &json!({ "path": t })) {
            s.echo(e.to_string());
        }
        Ok(Step::Done)
    }
}

pub(crate) fn lambopen_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(LambOpenM))
}

/// Why `dir` cannot take a `.lamb` project, if it cannot.
fn refuse_target(dir: &Path) -> Option<&'static str> {
    if dir.is_file() {
        return Some("it is a file, not a directory");
    }
    if dir.is_dir() && !dir.join("manifest.json").is_file() && std::fs::read_dir(dir).ok().is_some_and(|mut r| r.next().is_some()) {
        return Some("the directory is not empty and has no manifest.json");
    }
    None
}

/// LAMBSAVE's JSON form. Every refusal comes before anything is written.
pub(crate) fn lambsave(s: &mut Session, p: &Value) -> Result<Value> {
    let path = p.get("path").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad(SAVE, "`path` is required"))?;
    let refuse = |e: String| other(format!("Cannot save {path}: {e}"));
    let dir = Path::new(path);
    if let Some(why) = refuse_target(dir) {
        return Err(refuse(why.to_owned()));
    }
    let (project, report) = crate::export::drawing_to_project(s.doc()?).map_err(|e| refuse(e.to_string()))?;
    lamb_core::persistence::write_lamb_dir(&project, dir).map_err(|e| refuse(e.to_string()))?;

    let views = project.views.len();
    let entities: usize = project.views.iter().map(|v| v.entities().len()).sum();
    let mut skipped: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, kind) in &report.skipped {
        *skipped.entry(kind).or_default() += 1;
    }
    s.echo(format!("Saved \"{}\" to {path}: {views} views, {entities} entities", project.name));
    if !skipped.is_empty() {
        let kinds: Vec<String> = skipped.iter().map(|(k, n)| format!("{k} ×{n}")).collect();
        s.echo(format!("Skipped {} ({})", report.skipped.len(), kinds.join(", ")));
    }
    if !report.dropped_tags.is_empty() {
        s.echo(format!("Dropped tags: {}", report.dropped_tags.len()));
    }
    if !report.unknown_patterns.is_empty() {
        s.echo(format!("Unknown patterns: {}", report.unknown_patterns.join(", ")));
    }
    Ok(json!({
        "views": views,
        "entities": entities,
        "skipped": skipped,
        "droppedTags": report.dropped_tags.len(),
        "unknownPatterns": report.unknown_patterns,
        "path": path,
    }))
}

/// LAMBSAVE interactively: one typed path, then save (or echo the refusal).
struct LambSaveM;

impl Interactive for LambSaveM {
    fn name(&self) -> &'static str {
        SAVE
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter .lamb project path", Accept::TEXT)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let (Input::Text(t) | Input::Keyword(t)) = i else { return Ok(Step::Continue) };
        let t = t.trim().trim_matches('"').trim();
        if t.is_empty() {
            return Ok(Step::Continue);
        }
        if let Err(e) = lambsave(s, &json!({ "path": t })) {
            s.echo(e.to_string());
        }
        Ok(Step::Done)
    }
}

pub(crate) fn lambsave_machine(_s: &Session) -> Result<Box<dyn Interactive>> {
    Ok(Box::new(LambSaveM))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::read_document;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type R = std::result::Result<(), Box<dyn std::error::Error>>;
    type T<X> = std::result::Result<X, Box<dyn std::error::Error>>;

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb/u-wall-two-sections.lamb.json")
    }

    fn temp_dir(tag: &str) -> T<PathBuf> {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("lamb-lambopen-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// The fixture written out as a `.lamb` directory.
    fn fixture_dir() -> T<PathBuf> {
        let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(fixture())?)?;
        let dir = temp_dir("dir")?.join("u-wall.lamb");
        for (rel, content) in &files {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().ok_or("no parent")?)?;
            std::fs::write(p, content)?;
        }
        Ok(dir)
    }

    /// A LAMB session with one drawn line in its own document (so its undo stack is non-empty).
    fn session() -> T<Session> {
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        s.execute("line", &json!({ "points": [[0, 0], [1000, 0]] }))?;
        Ok(s)
    }

    fn project() -> T<Project> {
        Ok(read_project(&fixture().to_string_lossy())?)
    }

    #[test]
    fn a_fixture_file_opens_as_a_new_current_untitled_path_document() -> R {
        let mut s = session()?;
        let before = s.docs.len();
        let prev = s.active;
        let undo = s.state()?.undo.len();
        assert!(undo > 0);
        let r = s.execute(CMD, &json!({ "path": fixture().to_string_lossy() }))?;
        assert_eq!(s.docs.len(), before + 1);
        assert_eq!(s.active, before);
        assert_eq!(r["document"], before);
        let p = project()?;
        let st = &s.docs[before];
        assert_eq!(st.path, None);
        assert_eq!(st.title, p.name);
        let doc = read_document(s.doc()?)?;
        let mut got: Vec<_> = doc.views.iter().map(|v| v.name().to_owned()).collect();
        let mut want: Vec<_> = p.views.iter().map(|v| v.name().to_owned()).collect();
        got.sort();
        want.sort();
        assert_eq!(got, want);
        assert_eq!(r["views"], p.views.len());
        assert_eq!(s.docs[prev].undo.len(), undo, "the previous document's undo stack is unchanged");
        Ok(())
    }

    #[test]
    fn the_directory_form_opens_identically() -> R {
        let mut s = session()?;
        let a = s.execute(CMD, &json!({ "path": fixture().to_string_lossy() }))?;
        let b = s.execute(CMD, &json!({ "path": fixture_dir()?.to_string_lossy() }))?;
        assert_eq!(a["views"], b["views"]);
        assert_eq!(a["entities"], b["entities"]);
        assert!(a["entities"].as_u64().ok_or("no count")? > 0);
        Ok(())
    }

    #[test]
    fn a_missing_path_or_a_directory_without_a_manifest_refuses_and_adds_nothing() -> R {
        let mut s = session()?;
        let before = s.docs.len();
        let missing = temp_dir("missing")?.join("nope.lamb");
        let e = s.execute(CMD, &json!({ "path": missing.to_string_lossy() })).err().ok_or("no refusal")?;
        assert!(e.to_string().contains("Cannot open"), "{e}");
        let empty = temp_dir("empty")?;
        std::fs::write(empty.join("project.json"), "{}")?;
        let e = s.execute(CMD, &json!({ "path": empty.to_string_lossy() })).err().ok_or("no refusal")?;
        assert!(e.to_string().contains("Cannot open") && e.to_string().contains("manifest"), "{e}");
        assert_eq!(s.docs.len(), before);
        Ok(())
    }

    #[test]
    fn the_interactive_form_opens_it() -> R {
        let mut s = session()?;
        let before = s.docs.len();
        s.start(CMD)?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter .lamb project path:"));
        s.input(Input::Text(format!("\"{}\"", fixture().to_string_lossy())))?;
        assert!(s.current_prompt().is_none());
        assert_eq!(s.docs.len(), before + 1);
        assert_eq!(s.active, before);
        assert!(!read_document(s.doc()?)?.views.is_empty());
        Ok(())
    }

    fn named(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../lamb-core/tests/lamb/{name}.lamb.json"))
    }

    fn corner_tags(p: &Project) -> usize {
        p.views
            .iter()
            .flat_map(|v| v.entities())
            .filter_map(|e| match &e.geometry {
                lamb_core::model::EntityGeometry::Polyline { geometry } => {
                    Some(geometry.segments.iter().filter(|s| s.corner_at_to.is_some()).count())
                }
                _ => None,
            })
            .sum()
    }

    #[test]
    fn every_fixture_saves_back_with_its_views_entities_links_and_corners() -> R {
        for name in ["filleted-corners", "dimensioned-u-wall", "t76-rotated-section"] {
            let src = read_project(&named(name).to_string_lossy())?;
            let (_, _, report) = crate::import::project_to_drawing(&src)?;
            let lost_corners: usize = report.flattened.iter().filter(|(_, w)| *w == crate::convert::Flattened::VirtualCorner).count();
            let mut s = session()?;
            s.execute(CMD, &json!({ "path": named(name).to_string_lossy() }))?;
            let out = temp_dir("save")?.join(format!("{name}.lamb"));
            let r = s.execute(SAVE, &json!({ "path": out.to_string_lossy() }))?;
            let back = lamb_core::persistence::read_lamb_dir(&out).map_err(|e| format!("{e:?}"))?;
            assert_eq!(r["views"], back.views.len(), "{name}");
            let mut want: Vec<(String, usize)> = src
                .views
                .iter()
                .map(|v| (v.name().to_owned(), v.entities().iter().filter(|e| !report.skipped.iter().any(|(id, _)| *id == e.id)).count()))
                .collect();
            let mut got: Vec<(String, usize)> = back.views.iter().map(|v| (v.name().to_owned(), v.entities().len())).collect();
            want.sort();
            got.sort();
            assert_eq!(got, want, "{name}");
            assert_eq!(back.links.len(), src.links.len() - report.dropped_links.len(), "{name}");
            assert_eq!(corner_tags(&back), corner_tags(&src) - lost_corners, "{name}");
            assert!(corner_tags(&back) > 0 || name != "filleted-corners");
        }
        Ok(())
    }

    #[test]
    fn lambsave_leaves_the_document_path_and_undo_alone() -> R {
        let mut s = session()?;
        s.execute(CMD, &json!({ "path": fixture().to_string_lossy() }))?;
        let (path, title, undo) = (s.state()?.path.clone(), s.state()?.title.clone(), s.state()?.undo.len());
        let out = temp_dir("keep")?.join("u.lamb");
        s.execute(SAVE, &json!({ "path": out.to_string_lossy() }))?;
        assert!(out.join("manifest.json").is_file());
        assert_eq!((s.state()?.path.clone(), s.state()?.title.clone(), s.state()?.undo.len()), (path, title, undo));
        // Saving again over its own output is allowed.
        s.execute(SAVE, &json!({ "path": out.to_string_lossy() }))?;
        Ok(())
    }

    #[test]
    fn lambsave_refuses_a_file_and_a_foreign_non_empty_directory() -> R {
        let mut s = session()?;
        s.execute(CMD, &json!({ "path": fixture().to_string_lossy() }))?;
        let e = s.execute(SAVE, &json!({ "path": fixture().to_string_lossy() })).err().ok_or("no refusal")?;
        assert!(e.to_string().contains("Cannot save") && e.to_string().contains("file"), "{e}");
        let foreign = temp_dir("foreign")?;
        std::fs::write(foreign.join("notes.txt"), "keep me")?;
        let e = s.execute(SAVE, &json!({ "path": foreign.to_string_lossy() })).err().ok_or("no refusal")?;
        assert!(e.to_string().contains("manifest.json"), "{e}");
        assert!(!foreign.join("views").exists());
        Ok(())
    }

    #[test]
    fn the_interactive_form_saves_it() -> R {
        let mut s = session()?;
        s.execute(CMD, &json!({ "path": fixture().to_string_lossy() }))?;
        let out = temp_dir("interactive")?.join("i.lamb");
        s.start(SAVE)?;
        assert_eq!(s.current_prompt().map(|p| p.display()).as_deref(), Some("Enter .lamb project path:"));
        s.input(Input::Text(format!("\"{}\"", out.to_string_lossy())))?;
        assert!(s.current_prompt().is_none());
        let back = lamb_core::persistence::read_lamb_dir(&out).map_err(|e| format!("{e:?}"))?;
        assert_eq!(back.views.len(), project()?.views.len());
        Ok(())
    }
}

//! Reading a `.lamb` project directory: a port of LAMB's `packages/persistence/src/layout.ts`
//! `filesToProject`, with its two schema migrations and its defaults merge. Read-only; there is no
//! Rust writer yet.
//!
//! The input is the directory's files as a `/`-separated relative path -> UTF-8 content map (the
//! shape of LAMB's `tools/fixtures/*.lamb.json`): `manifest.json`, `project.json`,
//! `views/<slug>--<id>.json` (listed in `project.json`'s `views[]`), and the optional
//! `links/links.json` and `command-log.json`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::geometry::Point2;
use crate::geometry::circle_fit::{CircleFit, circle_as_polyline};
use crate::model::{AnyView, CURRENT_SCHEMA_VERSION, LinkRecord, Project, default_project_defaults, with_string_ids};

pub const MANIFEST_PATH: &str = "manifest.json";
pub const PROJECT_PATH: &str = "project.json";
pub const LINKS_PATH: &str = "links/links.json";
pub const COMMAND_LOG_PATH: &str = "command-log.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// A required file (manifest, project, or a view listed in `project.json`) is absent.
    Missing(String),
    /// A file is not valid JSON, or does not decode into the shape LAMB writes.
    Json { path: String, message: String },
    /// Saved by a newer LAMB than this reader knows how to migrate.
    FutureVersion(u32),
    /// [`read_lamb_dir`] only: a file could not be read as UTF-8.
    Io { path: String, message: String },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Missing(path) => write!(f, "missing {path}"),
            LoadError::Json { path, message } => write!(f, "{path}: {message}"),
            LoadError::FutureVersion(v) => write!(f, "schema version {v} is newer than {CURRENT_SCHEMA_VERSION}"),
            LoadError::Io { path, message } => write!(f, "{path}: {message}"),
        }
    }
}

impl std::error::Error for LoadError {}

fn json_err(path: &str, e: impl fmt::Display) -> LoadError {
    LoadError::Json { path: path.to_owned(), message: e.to_string() }
}

fn parse(files: &BTreeMap<String, String>, path: &str) -> Result<Option<Value>, LoadError> {
    files.get(path).map(|raw| serde_json::from_str(raw).map_err(|e| json_err(path, e))).transpose()
}

fn decode<T: DeserializeOwned>(path: &str, v: Value) -> Result<T, LoadError> {
    serde_json::from_value(v).map_err(|e| json_err(path, e))
}

/// Schema 1 -> 2 (T58): `geometry.virtualCorners` keyed by VERTEX index moves onto
/// `segments[i].cornerAtTo` of the segment that ends there (vertex 0 -> the last segment of a
/// closed polyline with n >= 2, else dropped; vertex k -> segment k-1). The map is deleted.
fn migrate_virtual_corners(views: &mut [Value]) {
    for entity in views.iter_mut().filter_map(|v| v.get_mut("entities")).filter_map(Value::as_array_mut).flatten() {
        let Some(geometry) = entity.get_mut("geometry").and_then(Value::as_object_mut) else { continue };
        if geometry.get("kind").and_then(Value::as_str) != Some("polyline") {
            continue;
        }
        let Some(Value::Object(corners)) = geometry.remove("virtualCorners") else { continue };
        let Some(poly) = geometry.get_mut("geometry") else { continue };
        let closed = poly.get("closed").and_then(Value::as_bool).unwrap_or(false);
        let Some(segments) = poly.get_mut("segments").and_then(Value::as_array_mut) else { continue };
        let n = segments.len();
        for (key, entry) in corners {
            let Ok(vertex) = key.parse::<usize>() else { continue };
            let seg_index = match vertex {
                0 if closed && n >= 2 => n - 1,
                0 => continue,
                k => k - 1,
            };
            if let Some(Value::Object(seg)) = segments.get_mut(seg_index) {
                seg.insert("cornerAtTo".to_owned(), entry);
            }
        }
    }
}

/// Schema 2 -> 3 (T94): `{"kind":"circle",center,radius}` becomes the closed two-arc polyline
/// [`circle_as_polyline`] gives.
fn migrate_circles(views: &mut [Value]) {
    for entity in views.iter_mut().filter_map(|v| v.get_mut("entities")).filter_map(Value::as_array_mut).flatten() {
        let Some(geometry) = entity.get_mut("geometry") else { continue };
        if geometry.get("kind").and_then(Value::as_str) != Some("circle") {
            continue;
        }
        let center = geometry.get("center").cloned().and_then(|c| serde_json::from_value::<Point2>(c).ok());
        let (Some(center), Some(radius)) = (center, geometry.get("radius").and_then(Value::as_f64)) else { continue };
        let Ok(poly) = serde_json::to_value(circle_as_polyline(&CircleFit { center, radius })) else { continue };
        *geometry = Value::Object(Map::from_iter([("kind".to_owned(), Value::from("polyline")), ("geometry".to_owned(), poly)]));
    }
}

/// Reconstruct a [`Project`] from a `.lamb` directory's file contents, migrated up to
/// [`CURRENT_SCHEMA_VERSION`]. String ids are interned once across every file, so link anchors
/// resolve to the same [`crate::model::Id`]s as the views and entities they name.
pub fn files_to_project(files: &BTreeMap<String, String>) -> Result<Project, LoadError> {
    let manifest = parse(files, MANIFEST_PATH)?.ok_or_else(|| LoadError::Missing(MANIFEST_PATH.to_owned()))?;
    let mut project_json = parse(files, PROJECT_PATH)?.ok_or_else(|| LoadError::Missing(PROJECT_PATH.to_owned()))?;

    // A file with no recorded version predates versioning, so it is schema 1.
    let saved_version = match manifest.get("schemaVersion") {
        None | Some(Value::Null) => 1,
        Some(v) => v.as_u64().and_then(|v| u32::try_from(v).ok()).ok_or_else(|| json_err(MANIFEST_PATH, "schemaVersion is not a version number"))?,
    };
    if saved_version > CURRENT_SCHEMA_VERSION {
        return Err(LoadError::FutureVersion(saved_version));
    }
    let name = manifest.get("name").and_then(Value::as_str).ok_or_else(|| json_err(MANIFEST_PATH, "missing name"))?.to_owned();

    let view_paths: Vec<String> = project_json
        .get("views")
        .and_then(Value::as_array)
        .ok_or_else(|| json_err(PROJECT_PATH, "missing views"))?
        .iter()
        .map(|v| v.get("path").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| json_err(PROJECT_PATH, "a views[] entry has no path")))
        .collect::<Result<_, _>>()?;
    let mut views =
        view_paths.iter().map(|path| parse(files, path)?.ok_or_else(|| LoadError::Missing(path.clone()))).collect::<Result<Vec<Value>, _>>()?;

    if saved_version < 2 {
        migrate_virtual_corners(&mut views);
    }
    if saved_version < 3 {
        migrate_circles(&mut views);
    }

    // Merged over the factory defaults, never taken whole: files saved before a defaults field
    // existed would otherwise lack it (T88).
    let mut defaults = serde_json::to_value(default_project_defaults()).map_err(|e| json_err(PROJECT_PATH, e))?;
    if let (Some(base), Some(Value::Object(saved))) = (defaults.as_object_mut(), project_json.get_mut("defaults").map(Value::take)) {
        base.extend(saved);
    }

    let links = parse(files, LINKS_PATH)?.unwrap_or_else(|| Value::Array(Vec::new()));
    let command_log = parse(files, COMMAND_LOG_PATH)?.unwrap_or_else(|| Value::Array(Vec::new()));
    let field = |key: &str| project_json.get(key).cloned().unwrap_or(Value::Null);
    let (levels, layers) = (field("levels"), field("layers"));
    // Pre-T34 files predate the pattern table.
    let patterns = match field("patterns") {
        Value::Null => Value::Object(Map::new()),
        p => p,
    };

    with_string_ids(|| {
        Ok(Project {
            schema_version: CURRENT_SCHEMA_VERSION,
            name,
            defaults: decode(PROJECT_PATH, defaults)?,
            levels: decode(PROJECT_PATH, levels)?,
            layers: decode(PROJECT_PATH, layers)?,
            views: view_paths.iter().zip(views).map(|(path, v)| decode::<AnyView>(path, v)).collect::<Result<_, _>>()?,
            links: decode::<Vec<LinkRecord>>(LINKS_PATH, links)?,
            command_log: decode(COMMAND_LOG_PATH, command_log)?,
            patterns: decode(PROJECT_PATH, patterns)?,
            suppressed_proposals: None,
        })
    })
}

/// Reads a `.lamb` directory from disk into the map [`files_to_project`] takes, and decodes it.
pub fn read_lamb_dir(path: &Path) -> Result<Project, LoadError> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) -> Result<(), LoadError> {
        let io = |p: &Path, e: std::io::Error| LoadError::Io { path: p.display().to_string(), message: e.to_string() };
        for entry in std::fs::read_dir(dir).map_err(|e| io(dir, e))? {
            let p = entry.map_err(|e| io(dir, e))?.path();
            if p.is_dir() {
                walk(root, &p, out)?;
            } else if let Ok(rel) = p.strip_prefix(root) {
                let key = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
                out.insert(key, std::fs::read_to_string(&p).map_err(|e| io(&p, e))?);
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    walk(path, path, &mut files)?;
    files_to_project(&files)
}

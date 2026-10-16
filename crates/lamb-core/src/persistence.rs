//! Reading a `.lamb` project directory: a port of LAMB's `packages/persistence/src/layout.ts`
//! `filesToProject`, with its two schema migrations and its defaults merge; and writing one: a port
//! of `projectToFiles` + `serialize.ts` `stableStringify`, byte-identical to what the TS app writes.
//!
//! The input is the directory's files as a `/`-separated relative path -> UTF-8 content map (the
//! shape of LAMB's `tools/fixtures/*.lamb.json`): `manifest.json`, `project.json`,
//! `views/<slug>--<id>.json` (listed in `project.json`'s `views[]`), and the optional
//! `links/links.json` and `command-log.json`.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::derivation::proposals::remap_slot_key;
use crate::geometry::Point2;
use crate::geometry::circle_fit::{CircleFit, circle_as_polyline};
use crate::model::{AnyView, CURRENT_SCHEMA_VERSION, Id, IdStrings, LinkRecord, Project, default_project_defaults, with_string_ids_seeded};

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

/// Inside the [`with_string_ids`](crate::model::with_string_ids) scope: rewrites every `ai_origin.proposal_id` from the TS string
/// ids it was saved with onto the interned ids' decimals — what `Id`'s `Display` gives outside the
/// scope, so lamb-core's sweeps find the saved slots (T355).
fn rewrite_proposal_ids(views: &mut [AnyView]) {
    let mut table: HashMap<String, Id> = HashMap::new();
    for v in views.iter() {
        table.insert(v.id().to_string(), v.id());
        table.extend(v.entities().iter().map(|e| (e.id.to_string(), e.id)));
    }
    for v in views.iter_mut() {
        let entities = match v {
            AnyView::Plan(v) => &mut v.entities,
            AnyView::Vertical(v) => &mut v.entities,
            AnyView::Detail(v) => &mut v.entities,
        };
        for o in entities.iter_mut().filter_map(|e| e.ai_origin.as_mut()) {
            o.proposal_id = remap_slot_key(&o.proposal_id, |s| table.get(s).map(|id| id.0.to_string()));
        }
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

    let (project, id_strings) = with_string_ids_seeded(&IdStrings::default(), || {
        let mut views = view_paths.iter().zip(views).map(|(path, v)| decode::<AnyView>(path, v)).collect::<Result<Vec<_>, _>>()?;
        rewrite_proposal_ids(&mut views);
        Ok(Project {
            schema_version: CURRENT_SCHEMA_VERSION,
            name,
            defaults: decode(PROJECT_PATH, defaults)?,
            levels: decode(PROJECT_PATH, levels)?,
            layers: decode(PROJECT_PATH, layers)?,
            views,
            links: decode::<Vec<LinkRecord>>(LINKS_PATH, links)?,
            command_log: decode(COMMAND_LOG_PATH, command_log)?,
            patterns: decode(PROJECT_PATH, patterns)?,
            suppressed_proposals: None,
            id_strings: IdStrings::default(),
        })
    });
    project.map(|p| Project { id_strings, ..p })
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

/// True for a key JS orders as an array index: canonical decimal, below 2^32 - 1.
fn is_array_index(k: &str) -> bool {
    !k.is_empty()
        && k.bytes().all(|b| b.is_ascii_digit())
        && (k == "0" || !k.starts_with('0'))
        && k.parse::<u64>().is_ok_and(|n| n < u64::from(u32::MAX))
}

/// The order of a JS object's own keys once `Object.keys(..).sort()` re-inserts them: array
/// indices first, numerically, then the rest by UTF-16 code unit.
fn js_key_order(a: &str, b: &str) -> Ordering {
    match (is_array_index(a), is_array_index(b)) {
        (true, true) => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => a.encode_utf16().cmp(b.encode_utf16()),
    }
}

/// A number as JS `Number.prototype.toString` prints it.
fn js_number(n: &serde_json::Number) -> String {
    if n.is_u64() || n.is_i64() {
        return n.to_string();
    }
    let x = n.as_f64().unwrap_or(0.0);
    if x == 0.0 {
        return "0".to_owned();
    }
    if (1e-6..1e21).contains(&x.abs()) {
        // Display prints the shortest round-trip digits as a plain decimal, with no `.0`.
        return format!("{x}");
    }
    let e = format!("{x:e}");
    match e.split_once('e') {
        Some((mantissa, exp)) if !exp.starts_with('-') => format!("{mantissa}e+{exp}"),
        _ => e,
    }
}

fn json_string(s: &str) -> String {
    Value::String(s.to_owned()).to_string()
}

fn stringify_into(v: &Value, indent: usize, out: &mut String) {
    fn open(out: &mut String, i: usize, indent: usize) {
        out.push_str(if i == 0 { "\n" } else { ",\n" });
        out.push_str(&"  ".repeat(indent + 1));
    }
    fn close(out: &mut String, indent: usize, bracket: char) {
        out.push('\n');
        out.push_str(&"  ".repeat(indent));
        out.push(bracket);
    }
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&js_number(n)),
        Value::String(s) => out.push_str(&json_string(s)),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                open(out, i, indent);
                stringify_into(item, indent + 1, out);
            }
            close(out, indent, ']');
        }
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| js_key_order(a.0, b.0));
            out.push('{');
            for (i, (key, value)) in entries.into_iter().enumerate() {
                open(out, i, indent);
                out.push_str(&json_string(key));
                out.push_str(": ");
                stringify_into(value, indent + 1, out);
            }
            close(out, indent, '}');
        }
    }
}

/// TS `stableStringify`: keys sorted, 2-space indent, a trailing newline, numbers as JS prints them.
pub fn stable_stringify(v: &Value) -> String {
    let mut out = String::new();
    stringify_into(v, 0, &mut out);
    out.push('\n');
    out
}

/// TS `slugify`, for a view's file name.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for c in name.to_lowercase().trim().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    match slug.trim_matches('-') {
        "" => "view".to_owned(),
        s => s.to_owned(),
    }
}

fn id_of(v: &Value) -> &str {
    v.get("id").and_then(Value::as_str).unwrap_or("")
}

fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn sorted_by_id(v: Value) -> Value {
    match v {
        Value::Array(mut items) => {
            items.sort_by(|a, b| id_of(a).cmp(id_of(b)));
            Value::Array(items)
        }
        other => other,
    }
}

/// Serialize a [`Project`] into the `.lamb` directory's files, in TS `projectToFiles` order. Ids are
/// written as the strings they were loaded with (`lamb-{n:08x}-0` for an id minted since), and AI
/// slot keys are mapped back from decimals onto those strings. Like the TS writer, it does not
/// write `suppressed_proposals`.
pub fn project_to_files(p: &Project) -> Vec<(String, String)> {
    let (files, _) = with_string_ids_seeded(&p.id_strings, || {
        let mut views = p.views.clone();
        for v in views.iter_mut() {
            let entities = match v {
                AnyView::Plan(v) => &mut v.entities,
                AnyView::Vertical(v) => &mut v.entities,
                AnyView::Detail(v) => &mut v.entities,
            };
            for o in entities.iter_mut().filter_map(|e| e.ai_origin.as_mut()) {
                o.proposal_id = remap_slot_key(&o.proposal_id, |s| s.parse::<u64>().ok().map(|n| Id(n).to_string()));
            }
        }
        let mut views: Vec<(String, Value)> = views
            .iter()
            .map(|v| {
                let mut value = to_value(v);
                if let Some(entities) = value.get_mut("entities") {
                    *entities = sorted_by_id(entities.take());
                }
                (format!("views/{}--{}.json", slugify(v.name()), v.id()), value)
            })
            .collect();
        views.sort_by(|a, b| id_of(&a.1).cmp(id_of(&b.1)));

        let manifest = json!({ "schemaVersion": p.schema_version, "name": p.name });
        let project = json!({
            "defaults": to_value(&p.defaults),
            "levels": sorted_by_id(to_value(&p.levels)),
            "layers": sorted_by_id(to_value(&p.layers)),
            "views": views.iter().map(|(path, v)| json!({ "id": id_of(v), "path": path })).collect::<Vec<_>>(),
            "patterns": to_value(&p.patterns),
        });
        let mut files = vec![(MANIFEST_PATH.to_owned(), stable_stringify(&manifest)), (PROJECT_PATH.to_owned(), stable_stringify(&project))];
        files.extend(views.into_iter().map(|(path, v)| (path, stable_stringify(&v))));
        files.push((LINKS_PATH.to_owned(), stable_stringify(&sorted_by_id(to_value(&p.links)))));
        files.push((COMMAND_LOG_PATH.to_owned(), stable_stringify(&to_value(&p.command_log))));
        files
    });
    files
}

/// Writes [`project_to_files`] into `dir`, creating `views/` and `links/`, and removes any
/// `views/*.json` the project no longer lists. Other files in `dir` are left alone.
pub fn write_lamb_dir(p: &Project, dir: &Path) -> std::io::Result<()> {
    let files = project_to_files(p);
    std::fs::create_dir_all(dir.join("views"))?;
    std::fs::create_dir_all(dir.join("links"))?;
    for (path, content) in &files {
        std::fs::write(dir.join(path), content)?;
    }
    let listed: HashSet<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
    for entry in std::fs::read_dir(dir.join("views"))? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if path.is_file() && name.ends_with(".json") && !listed.contains(format!("views/{name}").as_str()) {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

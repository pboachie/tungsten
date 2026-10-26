// SPDX-License-Identifier: AGPL-3.0-only
//! Bounded local defaults, captured once and composed without executable work.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tungsten_core::{Diagnostic, Diagnostics, Digest};

use crate::target_options::{PythonOptions, RustOptions, TypeScriptOptions};
use crate::{ManifestSource, TungstenConfig, capabilities, parse_yaml, pointer};

pub const MAX_FILE_BYTES: usize = 1024 * 1024;
pub const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_IDENTITIES: usize = 64;
pub const MAX_EDGES: usize = 256;
pub const MAX_DEPTH: usize = 8;

/// Built-in targets whose typed options profiles may default, in application order.
pub const PROFILE_TARGETS: &[&str] = &["typescript", "python", "rust"];

/// Fields composed from profiles and projects, including project-only fields
/// so explicit declarations are recorded and win.
fn composed_fields(target: &str) -> &'static [&'static str] {
    match target {
        "typescript" => &[
            "package",
            "version",
            "runtime",
            "runtime_path",
            "dependencies",
        ],
        "python" => &[
            "package",
            "version",
            "runtime",
            "runtime_path",
            "models",
            "python_version",
        ],
        "rust" => &["crate", "version", "runtime", "runtime_path"],
        _ => &[],
    }
}

/// Strict profile validation for one target's defaults.
fn profile_diagnostics(target: &str, options: &Value) -> Diagnostics {
    match target {
        "typescript" => TypeScriptOptions::parse_profile(options).1,
        "python" => PythonOptions::parse_profile(options).1,
        "rust" => RustOptions::parse_profile(options).1,
        _ => Diagnostics::new(),
    }
}

/// Both paths are relative to the selected manifest/base directory. A root is
/// deliberately supplied for each selection; included files cannot widen it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProfileSelection {
    pub path: String,
    pub root: String,
}

/// Reads are injected by the driver. Implementations must enforce `max_bytes`
/// before unbounded allocation and resolve physical/virtual local identities.
pub trait ProfileSource {
    /// Existing physical directory selected independently of included content.
    fn canonical_root(&self, path: &Path) -> Option<PathBuf>;
    fn canonical_path(&self, path: &Path) -> Option<PathBuf>;
    fn read(&self, path: &Path, max_bytes: usize) -> Result<(String, Digest), Diagnostic>;
    fn is_cancelled(&self) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Profile,
    Project,
    Invocation,
}

/// Authored declaration, not a replayable approval or effective export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub kind: SourceKind,
    pub identity: PathBuf,
    pub file: String,
    pub pointer: String,
    pub digest: Option<Digest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldProvenance {
    pub winner: Declaration,
    /// Earlier declarations, in application order.
    pub shadowed: Vec<Declaration>,
}

/// In-memory supported declarations only. IR-dependent defaults stay with the
/// emitter. This result is not accepted as a manifest or policy document.
#[derive(Debug, Clone)]
pub struct EffectiveConfiguration {
    pub schema_version: u32,
    pub typescript: TypeScriptOptions,
    pub python: PythonOptions,
    pub rust: RustOptions,
    pub provenance: BTreeMap<String, FieldProvenance>,
    /// Portable source names paired with invocation-local physical identities.
    pub profiles: Vec<ProfileIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileIdentity {
    pub name: String,
    pub identity: PathBuf,
    pub digest: Digest,
}

#[derive(Debug, Clone)]
pub struct CapturedProfile {
    pub identity: PathBuf,
    pub digest: Digest,
    pub source: ManifestSource,
    includes: Vec<(usize, String)>,
    /// Target id, authored alias and defaults, in authored order.
    options: Vec<(&'static str, String, Value)>,
}

#[derive(Debug, Default)]
pub struct Composition {
    pub effective: Option<EffectiveConfiguration>,
    pub sources: Vec<CapturedProfile>,
    pub diagnostics: Diagnostics,
}

/// Profile v1 schema uses the shared target supported-value definitions.
/// A target and its alias must not both appear in one profile.
pub fn json_schema() -> Value {
    let typescript = crate::target_options::typescript_profile_json_schema();
    let python = crate::target_options::python_profile_json_schema();
    let rust = crate::target_options::rust_profile_json_schema();
    serde_json::json!({
        "$schema":"https://json-schema.org/draft/2020-12/schema",
        "title":"Local profile v1", "type":"object",
        "additionalProperties":false, "required":["tungsten_profile"],
        "properties":{
            "tungsten_profile":{"const":1},
            "includes":{"type":"array","maxItems":256,"items":{"type":"string","minLength":1}},
            "targets":{"type":"object","additionalProperties":false,
                "maxProperties":PROFILE_TARGETS.len(), "properties":{
                    "typescript": typescript, "ts": typescript,
                    "python": python, "py": python,
                    "rust": rust, "rs": rust,
                }}
        }
    })
}

/// Apply captured profile defaults, then explicit project fields, to targets the
/// project declares; profiles never add an output. Invalid
/// profiles leave the configuration untouched. Legacy project values remain
/// raw so their emitter warning/fallback/error behavior is preserved.
pub fn compose(
    config: &mut TungstenConfig,
    base: &Path,
    project: Declaration,
    source: &impl ProfileSource,
) -> Composition {
    let mut loader = Loader {
        source,
        captures: vec![],
        identities: BTreeMap::new(),
        active: BTreeSet::new(),
        applied: BTreeSet::new(),
        bytes: 0,
        read_refused: false,
        edges: 0,
        defaults: BTreeMap::new(),
        provenance: BTreeMap::new(),
        diagnostics: Diagnostics::new(),
    };
    if config.profiles.len() > MAX_IDENTITIES {
        loader.error(
            &project,
            "/profiles",
            "too many selected profiles (maximum 64)",
        );
    } else {
        for (index, selected) in config.profiles.iter().enumerate() {
            if source.is_cancelled() {
                break;
            }
            let at = format!("/profiles/{index}");
            if !relative_local_path(&selected.path) || !relative_local_path(&selected.root) {
                loader.error(
                    &project,
                    &at,
                    "profile selection needs non-empty relative local path and root",
                );
                continue;
            }
            let Some(root) = source.canonical_root(&base.join(&selected.root)) else {
                loader.error(
                    &project,
                    &format!("{at}/root"),
                    "profile include root cannot be resolved",
                );
                continue;
            };
            let mut origin = project.clone();
            origin.pointer = format!("{at}/path");
            loader.visit(&base.join(&selected.path), &root, index, 0, &origin);
        }
    }
    let mut effective = None;
    if !loader.diagnostics.has_errors() && !source.is_cancelled() {
        for &target in PROFILE_TARGETS {
            let base = format!("/targets/{target}");
            let mut origin = project.clone();
            origin.pointer = base.clone();
            let nested = format!("{base}/");
            // Profiles default declared targets only; they never add an output.
            let Some(value) = config.targets.get(target) else {
                loader.defaults.remove(target);
                loader
                    .provenance
                    .retain(|path, _| path != &base && !path.starts_with(&nested));
                continue;
            };
            // A non-mapping project target keeps its old raw emitter behavior.
            let Some(declared) = value.as_object() else {
                loader.defaults.remove(target);
                loader
                    .provenance
                    .retain(|path, _| path != &base && !path.starts_with(&nested));
                loader.record(base.clone(), origin);
                continue;
            };
            let mut merged = declared.clone();
            loader.apply(target, value, &origin);
            if let Some(defaults) = loader.defaults.get(target) {
                merged.extend(defaults.clone());
                config.targets.insert(target.into(), Value::Object(merged));
            }
        }
        let options = |target: &str| config.targets.get(target).unwrap_or(&Value::Null);
        let (typescript, _) = TypeScriptOptions::parse(options("typescript"));
        let (python, _) = PythonOptions::parse(options("python"));
        let (rust, _) = RustOptions::parse(options("rust"));
        effective = Some(EffectiveConfiguration {
            schema_version: 1,
            typescript,
            python,
            rust,
            provenance: loader.provenance,
            profiles: loader
                .captures
                .iter()
                .map(|capture| ProfileIdentity {
                    name: capture.source.name().into(),
                    identity: capture.identity.clone(),
                    digest: capture.digest.clone(),
                })
                .collect(),
        });
    }
    Composition {
        effective,
        sources: loader.captures,
        diagnostics: loader.diagnostics,
    }
}

/// A non-empty relative local path; URLs, `git+` sources and rooted forms are refused.
pub fn is_relative_local_path(path: &str) -> bool {
    relative_local_path(path)
}

fn relative_local_path(path: &str) -> bool {
    // Reject portable Windows drive/root forms even on Unix. On Windows,
    // drive-relative and rooted-without-drive joins can replace part of base.
    !path.trim().is_empty()
        && !path.contains("://")
        && !path.starts_with("git+")
        && !path.starts_with('\\')
        && !(path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic())
        && !Path::new(path).components().any(|part| {
            matches!(
                part,
                std::path::Component::Prefix(_) | std::path::Component::RootDir
            )
        })
}

struct Loader<'s, S> {
    source: &'s S,
    captures: Vec<CapturedProfile>,
    identities: BTreeMap<PathBuf, usize>,
    active: BTreeSet<PathBuf>,
    applied: BTreeSet<PathBuf>,
    bytes: usize,
    read_refused: bool,
    edges: usize,
    /// Composed declarations per target id.
    defaults: BTreeMap<&'static str, Map<String, Value>>,
    provenance: BTreeMap<String, FieldProvenance>,
    diagnostics: Diagnostics,
}

impl<S: ProfileSource> Loader<'_, S> {
    fn error(&mut self, origin: &Declaration, at: &str, message: &str) {
        self.diagnostics
            .push(Diagnostic::error("TG0602", message).at(&origin.file, at, None));
    }

    fn visit(
        &mut self,
        path: &Path,
        root: &Path,
        selected: usize,
        depth: usize,
        origin: &Declaration,
    ) {
        if self.source.is_cancelled() || self.edges > MAX_EDGES {
            return;
        }
        if depth > MAX_DEPTH {
            self.error(
                origin,
                &origin.pointer,
                "profile include depth exceeds eight",
            );
            return;
        }
        let Some(identity) = self.source.canonical_path(path) else {
            self.error(origin, &origin.pointer, "local profile cannot be resolved");
            return;
        };
        if !identity.starts_with(root) {
            self.error(
                origin,
                &origin.pointer,
                "profile is outside its selected include root",
            );
            return;
        }
        if self.active.contains(&identity) {
            self.error(origin, &origin.pointer, "profile include cycle");
            return;
        }
        let index = if let Some(&index) = self.identities.get(&identity) {
            index
        } else {
            if self.read_refused {
                return;
            }
            if self.captures.len() >= MAX_IDENTITIES {
                self.error(origin, &origin.pointer, "profile identity limit exceeds 64");
                return;
            }
            let remaining = MAX_TOTAL_BYTES.saturating_sub(self.bytes);
            if remaining == 0 {
                self.read_refused = true;
                self.error(
                    origin,
                    &origin.pointer,
                    "profile captured-byte limit exhausted",
                );
                return;
            }
            let max_bytes = MAX_FILE_BYTES.min(remaining);
            let (text, digest) = match self.source.read(&identity, max_bytes) {
                Ok(captured) => captured,
                Err(_) => {
                    self.read_refused = true;
                    self.error(
                        origin,
                        &origin.pointer,
                        if remaining < MAX_FILE_BYTES {
                            "profile captured-byte allowance cannot read this profile"
                        } else {
                            "cannot read local profile within its 1 MiB UTF-8 limit"
                        },
                    );
                    return;
                }
            };
            if self.source.is_cancelled() {
                return;
            }
            if text.len() > MAX_FILE_BYTES
                || text.len() > MAX_TOTAL_BYTES.saturating_sub(self.bytes)
            {
                self.error(
                    origin,
                    &origin.pointer,
                    "profile captured-byte limit exceeded",
                );
                return;
            }
            self.bytes += text.len();
            // Physical identities are invocation-local. Portable source names
            // and digest keys must not encode the caller's absolute checkout.
            let relative = identity.strip_prefix(root).expect("confined identity");
            let name = format!(
                "profile[{selected}]/{}",
                relative.to_string_lossy().replace('\\', "/")
            );
            let (value, parsed_source) = parse_yaml(&name, &text);
            let index = self.captures.len();
            self.captures.push(CapturedProfile {
                identity: identity.clone(),
                digest,
                source: parsed_source,
                includes: vec![],
                options: vec![],
            });
            self.identities.insert(identity.clone(), index);
            match value {
                Ok(value) => self.parse(index, value),
                Err(mut diagnostic) => {
                    // Parser internals may quote input fragments; keep only its pointer.
                    diagnostic.message = "invalid profile YAML or JSON".into();
                    diagnostic.help = None;
                    self.diagnostics.push(diagnostic);
                }
            }
            index
        };
        self.active.insert(identity.clone());
        // Reused DAG nodes still validate every incoming root/depth and all
        // descendant paths. Only reads and application are deduplicated.
        let includes = self.captures[index].includes.clone();
        for (position, include) in &includes {
            if self.source.is_cancelled() {
                break;
            }
            self.edges += 1;
            let declaration = self.declaration(index, &format!("/includes/{position}"));
            if self.edges > MAX_EDGES {
                self.error(
                    &declaration,
                    &declaration.pointer,
                    "profile include edge limit exceeds 256",
                );
                break;
            }
            if !relative_local_path(include) {
                self.error(
                    &declaration,
                    &declaration.pointer,
                    "profile include must be a relative local path",
                );
                continue;
            }
            self.visit(
                &identity.parent().unwrap_or(Path::new(".")).join(include),
                root,
                selected,
                depth + 1,
                &declaration,
            );
        }
        self.active.remove(&identity);
        if self.applied.insert(identity) && !self.source.is_cancelled() {
            for (target, alias, options) in self.captures[index].options.clone() {
                let declaration = self.declaration(index, &pointer::child("/targets", &alias));
                self.apply(target, &options, &declaration);
            }
        }
    }

    fn declaration(&self, index: usize, at: &str) -> Declaration {
        let capture = &self.captures[index];
        Declaration {
            kind: SourceKind::Profile,
            identity: capture.identity.clone(),
            file: capture.source.name().into(),
            pointer: at.into(),
            digest: Some(capture.digest.clone()),
        }
    }

    fn parse(&mut self, index: usize, value: Value) {
        let origin = self.declaration(index, "");
        let Some(fields) = value.as_object() else {
            self.error(&origin, "", "profile must be a mapping");
            return;
        };
        for key in fields.keys() {
            if !["tungsten_profile", "includes", "targets"].contains(&key.as_str()) {
                self.error(
                    &origin,
                    &pointer::child("", key),
                    "unsupported profile field",
                );
            }
        }
        if fields.get("tungsten_profile").and_then(Value::as_u64) != Some(1) {
            self.error(&origin, "/tungsten_profile", "profile version must be 1");
        }
        if let Some(includes) = fields.get("includes") {
            if let Some(includes) = includes.as_array() {
                // Bound this list before cloning entries or recursing.
                if includes.len() > MAX_EDGES {
                    self.error(
                        &origin,
                        "/includes",
                        "profile include list exceeds 256 edges",
                    );
                } else {
                    for (position, include) in includes.iter().enumerate() {
                        if let Some(include) = include.as_str() {
                            self.captures[index]
                                .includes
                                .push((position, include.into()));
                        } else {
                            self.error(
                                &origin,
                                &format!("/includes/{position}"),
                                "profile include must be a relative local path",
                            );
                        }
                    }
                }
            } else {
                self.error(&origin, "/includes", "profile includes must be a list");
            }
        }
        if let Some(targets) = fields.get("targets") {
            let Some(targets) = targets.as_object() else {
                self.error(&origin, "/targets", "profile targets must be a mapping");
                return;
            };
            let mut found = BTreeSet::new();
            for (name, options) in targets {
                let at = pointer::child("/targets", name);
                let Some(target) = capabilities::target(name)
                    .map(|target| target.id)
                    .filter(|id| PROFILE_TARGETS.contains(id))
                else {
                    self.error(
                        &origin,
                        &at,
                        "profile v1 supports only TypeScript, Python and Rust defaults",
                    );
                    continue;
                };
                if !found.insert(target) {
                    self.error(
                        &origin,
                        &at,
                        "conflicting aliases for the same target's profile defaults",
                    );
                }
                for mut diagnostic in profile_diagnostics(target, options).0 {
                    for label in &mut diagnostic.labels {
                        label.file = origin.file.clone();
                        label.pointer = format!("{at}{}", label.pointer);
                    }
                    self.diagnostics.push(diagnostic);
                }
                self.captures[index]
                    .options
                    .push((target, name.clone(), options.clone()));
            }
        }
    }

    fn record(&mut self, at: String, origin: Declaration) {
        if let Some(previous) = self.provenance.get_mut(&at) {
            previous
                .shadowed
                .push(std::mem::replace(&mut previous.winner, origin));
        } else {
            self.provenance.insert(
                at,
                FieldProvenance {
                    winner: origin,
                    shadowed: vec![],
                },
            );
        }
    }

    fn apply(&mut self, target: &'static str, value: &Value, origin: &Declaration) {
        let Some(fields) = value.as_object() else {
            return;
        };
        let base = format!("/targets/{target}");
        let entries_base = format!("{base}/dependencies/");
        for (key, value) in fields {
            if self.source.is_cancelled() {
                break;
            }
            if !composed_fields(target).contains(&key.as_str()) {
                continue;
            }
            let mut declaration = origin.clone();
            declaration.pointer = pointer::child(&origin.pointer, key);
            self.record(pointer::child(&base, key), declaration.clone());
            let defaults = self.defaults.entry(target).or_default();
            if key == "dependencies"
                && let Some(entries) = value.as_object()
            {
                let existing = defaults
                    .entry(key.clone())
                    .or_insert_with(|| Value::Object(Map::new()));
                if !existing.is_object() {
                    *existing = Value::Object(Map::new());
                }
                existing
                    .as_object_mut()
                    .expect("dependency mapping")
                    .extend(entries.clone());
                for name in entries.keys() {
                    let mut entry = declaration.clone();
                    entry.pointer = pointer::child(&declaration.pointer, name);
                    self.record(pointer::child(&format!("{base}/dependencies"), name), entry);
                }
            } else {
                defaults.insert(key.clone(), value.clone());
                if key == "dependencies" {
                    self.provenance
                        .retain(|path, _| !path.starts_with(&entries_base));
                }
            }
        }
    }
}
